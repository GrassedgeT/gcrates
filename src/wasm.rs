use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::f32::consts::{PI, TAU};
use std::mem::size_of;

use bytemuck::{Pod, Zeroable};
use js_sys::{Array, Object, Reflect};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{Document, HtmlCanvasElement, Window};
use wgpu::util::DeviceExt;

use crate::format::{StoredGraph, StoredPackage};

#[wasm_bindgen(start)]
pub fn install_panic_hook() {
    console_error_panic_hook::set_once();
}

#[wasm_bindgen]
pub struct GraphHandle {
    graph: StoredGraph,
    global_layout: GlobalLayout,
}

#[wasm_bindgen]
impl GraphHandle {
    #[wasm_bindgen(js_name = fromBytes)]
    pub fn from_bytes(bytes: &[u8]) -> Result<GraphHandle, JsValue> {
        let mut cursor = bytes;
        let graph = StoredGraph::read_from(&mut cursor).map_err(js_error)?;
        let global_layout = GlobalLayout::build(&graph);
        Ok(Self {
            graph,
            global_layout,
        })
    }

    #[wasm_bindgen(getter, js_name = packageCount)]
    pub fn package_count(&self) -> usize {
        self.graph.packages.len()
    }

    #[wasm_bindgen(getter, js_name = dependencyCount)]
    pub fn dependency_count(&self) -> usize {
        self.graph.dependencies.len()
    }

    #[wasm_bindgen(js_name = searchPrefix)]
    pub fn search_prefix(&self, query: &str, limit: usize) -> js_sys::Array {
        let query = query.trim().to_ascii_lowercase();
        let matches = js_sys::Array::new();
        if query.is_empty() || limit == 0 {
            return matches;
        }

        let mut best_matches: HashMap<String, (u8, u64, String)> = HashMap::new();
        for package in &self.graph.packages {
            let Some(name) = self.graph.resolve(package.name) else {
                continue;
            };
            let lowercase_name = name.to_ascii_lowercase();
            let rank = if lowercase_name == query {
                0
            } else if lowercase_name.starts_with(&query) {
                1
            } else if lowercase_name.contains(&query) {
                2
            } else {
                continue;
            };

            match best_matches.get_mut(&lowercase_name) {
                Some(entry) => {
                    if rank < entry.0 || (rank == entry.0 && package.downloads > entry.1) {
                        *entry = (rank, package.downloads, name.to_owned());
                    }
                }
                None => {
                    best_matches.insert(
                        lowercase_name,
                        (rank, package.downloads, name.to_owned()),
                    );
                }
            }
        }

        let mut sorted_matches: Vec<_> = best_matches.into_values().collect();
        sorted_matches.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then(right.1.cmp(&left.1))
                .then(left.2.cmp(&right.2))
        });
        for (_rank, _downloads, name) in sorted_matches.into_iter().take(limit) {
            matches.push(&JsValue::from_str(&name));
        }
        matches
    }

    #[wasm_bindgen(js_name = crateSummary)]
    pub fn crate_summary(&self, name: &str) -> Option<String> {
        let (package_index, package) = self.graph.package_by_name(name)?;
        let version = self.graph.resolve(package.version).unwrap_or("<missing>");
        let dependent_count = self
            .global_layout
            .dependent_counts
            .get(package_index)
            .copied()
            .unwrap_or_default();

        let mut lines = vec![
            format!("{name} v{version}"),
            format!("crate id: {}", package.crate_id),
            format!("downloads: {}", package.downloads),
            format!("direct dependencies: {}", package.dependency_count),
            format!("dependents in snapshot: {}", dependent_count),
        ];

        for dependency in self.graph.dependency_slice(package).iter().take(20) {
            let Some(target_package) = self.graph.packages.get(dependency.package_index as usize)
            else {
                continue;
            };
            let dependency_name = self.graph.resolve(target_package.name).unwrap_or("<missing>");
            let requirement = self.graph.resolve(dependency.req).unwrap_or("*");
            let target = match dependency.target {
                u32::MAX => String::new(),
                index => format!(" target={}", self.graph.resolve(index).unwrap_or("<missing>")),
            };
            let optional = if dependency.optional() { " optional" } else { "" };
            let default_features = if dependency.uses_default_features() {
                " default-features"
            } else {
                ""
            };
            lines.push(format!(
                "- {dependency_name} {requirement} [{}]{}{}{}",
                dependency.kind().as_str(),
                optional,
                default_features,
                target
            ));
        }

        let omitted = package.dependency_count.saturating_sub(20);
        if omitted > 0 {
            lines.push(format!("... {omitted} more"));
        }

        Some(lines.join("\n"))
    }

    #[wasm_bindgen(js_name = focalScene)]
    pub fn focal_scene(&self, name: &str, viewport_aspect: f32) -> Result<Object, JsValue> {
        let scene = build_focus_scene(&self.graph, name, viewport_aspect)?;
        scene_to_js(&scene)
    }

    #[wasm_bindgen(js_name = globalOverview)]
    pub fn global_overview(
        &self,
        center_x: f32,
        center_y: f32,
        center_z: f32,
        zoom: f32,
        viewport_aspect: f32,
        max_nodes: usize,
        max_edges: usize,
    ) -> Result<Object, JsValue> {
        let scene = self.global_layout.query(
            &self.graph,
            center_x,
            center_y,
            center_z,
            zoom,
            viewport_aspect,
            max_nodes,
            max_edges,
        );
        global_scene_to_js(&scene)
    }

    #[wasm_bindgen(js_name = globalMinimap)]
    pub fn global_minimap(&self) -> Result<Object, JsValue> {
        global_minimap_to_js(&self.global_layout.minimap)
    }

    #[wasm_bindgen(js_name = dependencyFocus)]
    pub fn dependency_focus(&self, name: &str) -> Result<Object, JsValue> {
        let scene = build_dependency_focus_scene(&self.graph, &self.global_layout.dependent_counts, name)?;
        global_scene_to_js(&scene)
    }

    #[wasm_bindgen(js_name = globalCratePosition)]
    pub fn global_crate_position(&self, name: &str) -> Result<Object, JsValue> {
        let (package_index, package) = self
            .graph
            .package_by_name(name)
            .ok_or_else(|| JsValue::from_str("crate was not found in the global layout"))?;
        let position = self
            .global_layout
            .nodes
            .get(package_index)
            .ok_or_else(|| JsValue::from_str("crate position is not available"))?;
        let result = Object::new();
        set_f64(&result, "x", position.position[0] as f64)?;
        set_f64(&result, "y", position.position[1] as f64)?;
        set_f64(&result, "z", position.position[2] as f64)?;
        set_f64(&result, "rank", position.rank as f64)?;
        set_f64(&result, "downloads", package.downloads as f64)?;
        set_f64(&result, "dependencyCount", package.dependency_count as f64)?;
        set_f64(
            &result,
            "dependentCount",
            self.global_layout
                .dependent_counts
                .get(package_index)
                .copied()
                .unwrap_or_default() as f64,
        )?;
        Reflect::set(&result, &JsValue::from_str("name"), &JsValue::from_str(name))?;
        Ok(result)
    }
}

#[wasm_bindgen]
pub struct WasmRenderer {
    canvas: HtmlCanvasElement,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    camera: CameraState,
    hovered_node: Option<usize>,
    scene: Option<FocusScene>,
    node_quad_buffer: wgpu::Buffer,
    edge_quad_buffer: wgpu::Buffer,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    edge_pipeline: wgpu::RenderPipeline,
    node_pipeline: wgpu::RenderPipeline,
    edge_instance_buffer: Option<wgpu::Buffer>,
    edge_instance_count: u32,
    node_instance_buffer: Option<wgpu::Buffer>,
    node_instance_count: u32,
}

#[wasm_bindgen]
impl WasmRenderer {
    #[wasm_bindgen(js_name = create)]
    pub async fn create(canvas_id: String) -> Result<WasmRenderer, JsValue> {
        let canvas = canvas_by_id(&canvas_id)?;
        sync_canvas_size(&canvas);

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
            .map_err(js_error)?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .map_err(js_error)?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("gcrates-device"),
                ..Default::default()
            })
            .await
            .map_err(js_error)?;

        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .or_else(|| capabilities.formats.first().copied())
            .ok_or_else(|| JsValue::from_str("surface does not expose any texture format"))?;
        let present_mode = capabilities
            .present_modes
            .iter()
            .copied()
            .find(|mode| *mode == wgpu::PresentMode::Fifo)
            .or_else(|| capabilities.present_modes.first().copied())
            .ok_or_else(|| JsValue::from_str("surface does not expose any present mode"))?;
        let alpha_mode = capabilities
            .alpha_modes
            .first()
            .copied()
            .ok_or_else(|| JsValue::from_str("surface does not expose any alpha mode"))?;

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: canvas.width().max(1),
            height: canvas.height().max(1),
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let node_quad_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("gcrates-node-quad"),
            contents: bytemuck::cast_slice(&NODE_QUAD),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let edge_quad_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("gcrates-edge-quad"),
            contents: bytemuck::cast_slice(&EDGE_QUAD),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let camera = CameraState::default();
        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("gcrates-camera-buffer"),
            contents: bytemuck::bytes_of(&camera.as_uniform()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let camera_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("gcrates-camera-layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("gcrates-camera-bind-group"),
            layout: &camera_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let edge_pipeline = create_edge_pipeline(&device, config.format, &camera_bind_group_layout);
        let node_pipeline = create_node_pipeline(&device, config.format, &camera_bind_group_layout);

        Ok(Self {
            canvas,
            surface,
            device,
            queue,
            config,
            camera,
            hovered_node: None,
            scene: None,
            node_quad_buffer,
            edge_quad_buffer,
            camera_buffer,
            camera_bind_group,
            edge_pipeline,
            node_pipeline,
            edge_instance_buffer: None,
            edge_instance_count: 0,
            node_instance_buffer: None,
            node_instance_count: 0,
        })
    }

    pub fn resize(&mut self) {
        sync_canvas_size(&self.canvas);
        self.config.width = self.canvas.width().max(1);
        self.config.height = self.canvas.height().max(1);
        self.surface.configure(&self.device, &self.config);
    }

    #[wasm_bindgen(js_name = setFocus)]
    pub fn set_focus(&mut self, graph: &GraphHandle, crate_name: &str) -> Result<(), JsValue> {
        self.resize();
        self.camera = CameraState::default();
        self.hovered_node = None;
        self.scene = Some(build_focus_scene(
            &graph.graph,
            crate_name,
            self.viewport_aspect(),
        )?);
        self.write_camera_uniform();
        self.rebuild_gpu_scene();
        Ok(())
    }

    #[wasm_bindgen(js_name = panBy)]
    pub fn pan_by(&mut self, delta_x: f32, delta_y: f32) -> Result<(), JsValue> {
        let width = self.canvas.client_width().max(1) as f32;
        let height = self.canvas.client_height().max(1) as f32;
        self.camera.offset[0] += 2.0 * delta_x / width / self.camera.zoom;
        self.camera.offset[1] -= 2.0 * delta_y / height / self.camera.zoom;
        self.write_camera_uniform();
        Ok(())
    }

    #[wasm_bindgen(js_name = zoomAt)]
    pub fn zoom_at(&mut self, factor: f32, x: f32, y: f32) -> Result<(), JsValue> {
        if !factor.is_finite() || factor <= 0.0 {
            return Ok(());
        }

        let clip = self.canvas_to_clip(x, y);
        let previous_zoom = self.camera.zoom;
        let next_zoom = (self.camera.zoom * factor).clamp(0.45, 6.5);
        self.camera.offset[0] += clip[0] * (1.0 / next_zoom - 1.0 / previous_zoom);
        self.camera.offset[1] += clip[1] * (1.0 / next_zoom - 1.0 / previous_zoom);
        self.camera.zoom = next_zoom;
        self.write_camera_uniform();
        Ok(())
    }

    #[wasm_bindgen(js_name = resetView)]
    pub fn reset_view(&mut self) {
        self.camera = CameraState::default();
        self.write_camera_uniform();
    }

    #[wasm_bindgen(js_name = hoverAt)]
    pub fn hover_at(&mut self, x: f32, y: f32) -> Result<JsValue, JsValue> {
        let hovered = self.pick_node_at(x, y);
        if hovered != self.hovered_node {
            self.hovered_node = hovered;
            self.rebuild_gpu_scene();
        }

        if let Some(index) = hovered {
            if let Some(scene) = &self.scene {
                return scene_node_to_js(index, &scene.nodes[index]);
            }
        }

        Ok(JsValue::NULL)
    }

    #[wasm_bindgen(js_name = clearHover)]
    pub fn clear_hover(&mut self) {
        if self.hovered_node.take().is_some() {
            self.rebuild_gpu_scene();
        }
    }

    pub fn render(&mut self) -> Result<(), JsValue> {
        self.resize();

        let frame = self.surface.get_current_texture().map_err(js_error)?;
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("gcrates-render-encoder"),
            });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("gcrates-render-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.051,
                            g: 0.090,
                            b: 0.118,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });

            if let Some(edge_buffer) = &self.edge_instance_buffer {
                if self.edge_instance_count > 0 {
                    render_pass.set_pipeline(&self.edge_pipeline);
                    render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
                    render_pass.set_vertex_buffer(0, self.edge_quad_buffer.slice(..));
                    render_pass.set_vertex_buffer(1, edge_buffer.slice(..));
                    render_pass.draw(0..EDGE_QUAD.len() as u32, 0..self.edge_instance_count);
                }
            }

            if let Some(node_buffer) = &self.node_instance_buffer {
                if self.node_instance_count > 0 {
                    render_pass.set_pipeline(&self.node_pipeline);
                    render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
                    render_pass.set_vertex_buffer(0, self.node_quad_buffer.slice(..));
                    render_pass.set_vertex_buffer(1, node_buffer.slice(..));
                    render_pass.draw(0..NODE_QUAD.len() as u32, 0..self.node_instance_count);
                }
            }
        }

        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }

    fn viewport_aspect(&self) -> f32 {
        self.config.height as f32 / self.config.width.max(1) as f32
    }

    fn write_camera_uniform(&self) {
        self.queue
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&self.camera.as_uniform()));
    }

    fn canvas_to_clip(&self, x: f32, y: f32) -> [f32; 2] {
        let width = self.canvas.client_width().max(1) as f32;
        let height = self.canvas.client_height().max(1) as f32;
        [x / width * 2.0 - 1.0, 1.0 - y / height * 2.0]
    }

    fn pick_node_at(&self, x: f32, y: f32) -> Option<usize> {
        let scene = self.scene.as_ref()?;
        let clip = self.canvas_to_clip(x, y);
        let world_x = clip[0] / self.camera.zoom - self.camera.offset[0];
        let world_y = clip[1] / self.camera.zoom - self.camera.offset[1];

        let mut best = None;
        for (index, node) in scene.nodes.iter().enumerate() {
            let radius_x = (node.radius * scene.viewport_aspect).max(0.0001);
            let radius_y = node.radius.max(0.0001);
            let dx = (world_x - node.position[0]) / radius_x;
            let dy = (world_y - node.position[1]) / radius_y;
            let score = dx * dx + dy * dy;
            if score <= 1.25 {
                match best {
                    Some((_, best_score)) if score >= best_score => {}
                    _ => best = Some((index, score)),
                }
            }
        }

        best.map(|(index, _)| index)
    }

    fn rebuild_gpu_scene(&mut self) {
        let Some(scene) = &self.scene else {
            self.edge_instance_buffer = None;
            self.edge_instance_count = 0;
            self.node_instance_buffer = None;
            self.node_instance_count = 0;
            return;
        };

        let mut edge_instances = Vec::with_capacity(scene.edges.len());
        for edge in &scene.edges {
            let mut color = edge.color;
            let mut width = edge.width;
            let mut emphasis = 0.0;

            if let Some(hovered) = self.hovered_node {
                let connected = edge.from == Some(hovered) || edge.to == Some(hovered);
                if connected {
                    width *= 1.85;
                    color[3] = (color[3] + 0.22).min(0.95);
                    emphasis = 1.0;
                } else {
                    color[3] *= 0.26;
                    width *= 0.78;
                }
            }

            edge_instances.push(EdgeInstance::new(edge.start, edge.end, color, width, emphasis));
        }

        let mut node_instances = Vec::with_capacity(scene.nodes.len());
        for (index, node) in scene.nodes.iter().enumerate() {
            let connected_to_hover = self
                .hovered_node
                .map(|hovered| {
                    hovered == index
                        || scene.edges.iter().any(|edge| {
                            (edge.from == Some(hovered) && edge.to == Some(index))
                                || (edge.from == Some(index) && edge.to == Some(hovered))
                        })
                })
                .unwrap_or(false);

            let is_hovered = self.hovered_node == Some(index);
            let highlight = if is_hovered {
                1.0
            } else if connected_to_hover {
                0.52
            } else if self.hovered_node.is_some() {
                0.0
            } else {
                0.16
            };
            let dim = if self.hovered_node.is_some() && !connected_to_hover && !is_hovered {
                0.58
            } else {
                1.0
            };

            let fill = [
                node.fill[0] * dim,
                node.fill[1] * dim,
                node.fill[2] * dim,
                node.fill[3] * if dim < 1.0 { 0.74 } else { 1.0 },
            ];
            let accent = [
                node.accent[0],
                node.accent[1],
                node.accent[2],
                (node.accent[3] + highlight * 0.22).min(1.0),
            ];
            let radius_y = node.radius * (1.0 + highlight * 0.1);
            let radius_x = radius_y * scene.viewport_aspect;
            let glow = node_glow(node.role) + highlight * 0.28;
            let ring = node_ring_width(node.role) + highlight * 0.06;

            node_instances.push(NodeInstance::new(
                node.position,
                [radius_x, radius_y],
                fill,
                accent,
                [glow, ring, highlight, 0.0],
            ));
        }

        self.edge_instance_count = edge_instances.len() as u32;
        self.node_instance_count = node_instances.len() as u32;
        self.edge_instance_buffer = create_vertex_buffer(
            &self.device,
            "gcrates-edge-instance-buffer",
            &edge_instances,
        );
        self.node_instance_buffer = create_vertex_buffer(
            &self.device,
            "gcrates-node-instance-buffer",
            &node_instances,
        );
    }
}

#[derive(Clone, Copy)]
struct CameraState {
    offset: [f32; 2],
    zoom: f32,
}

impl CameraState {
    fn as_uniform(self) -> CameraUniform {
        CameraUniform {
            offset: self.offset,
            zoom: self.zoom,
            _padding: 0.0,
        }
    }
}

impl Default for CameraState {
    fn default() -> Self {
        Self {
            offset: [0.0, 0.0],
            zoom: 1.0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraUniform {
    offset: [f32; 2],
    zoom: f32,
    _padding: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct UnitVertex {
    position: [f32; 2],
}

impl UnitVertex {
    fn layout<'a>() -> wgpu::VertexBufferLayout<'a> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![0 => Float32x2];
        wgpu::VertexBufferLayout {
            array_stride: size_of::<UnitVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &ATTRIBUTES,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct EdgeInstance {
    start: [f32; 2],
    end: [f32; 2],
    color: [f32; 4],
    params: [f32; 4],
}

impl EdgeInstance {
    fn new(start: [f32; 2], end: [f32; 2], color: [f32; 4], width: f32, emphasis: f32) -> Self {
        Self {
            start,
            end,
            color,
            params: [width, emphasis, 0.0, 0.0],
        }
    }

    fn layout<'a>() -> wgpu::VertexBufferLayout<'a> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![
            1 => Float32x2,
            2 => Float32x2,
            3 => Float32x4,
            4 => Float32x4
        ];
        wgpu::VertexBufferLayout {
            array_stride: size_of::<EdgeInstance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRIBUTES,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct NodeInstance {
    center: [f32; 2],
    radii: [f32; 2],
    fill: [f32; 4],
    accent: [f32; 4],
    params: [f32; 4],
}

impl NodeInstance {
    fn new(
        center: [f32; 2],
        radii: [f32; 2],
        fill: [f32; 4],
        accent: [f32; 4],
        params: [f32; 4],
    ) -> Self {
        Self {
            center,
            radii,
            fill,
            accent,
            params,
        }
    }

    fn layout<'a>() -> wgpu::VertexBufferLayout<'a> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
            1 => Float32x2,
            2 => Float32x2,
            3 => Float32x4,
            4 => Float32x4,
            5 => Float32x4
        ];
        wgpu::VertexBufferLayout {
            array_stride: size_of::<NodeInstance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRIBUTES,
        }
    }
}

const NODE_QUAD: [UnitVertex; 6] = [
    UnitVertex {
        position: [-1.0, -1.0],
    },
    UnitVertex {
        position: [-1.0, 1.0],
    },
    UnitVertex {
        position: [1.0, -1.0],
    },
    UnitVertex {
        position: [1.0, -1.0],
    },
    UnitVertex {
        position: [-1.0, 1.0],
    },
    UnitVertex { position: [1.0, 1.0] },
];

const EDGE_QUAD: [UnitVertex; 6] = [
    UnitVertex { position: [0.0, -1.0] },
    UnitVertex { position: [0.0, 1.0] },
    UnitVertex { position: [1.0, -1.0] },
    UnitVertex { position: [1.0, -1.0] },
    UnitVertex { position: [0.0, 1.0] },
    UnitVertex { position: [1.0, 1.0] },
];

#[derive(Clone, Copy)]
enum NodeRole {
    Focal,
    DirectDependency,
    SecondaryDependency,
    Dependent,
}

impl NodeRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::Focal => "focal crate",
            Self::DirectDependency => "direct dependency",
            Self::SecondaryDependency => "transitive dependency",
            Self::Dependent => "dependent crate",
        }
    }
}

#[derive(Clone)]
struct SceneNode {
    name: String,
    role: NodeRole,
    position: [f32; 2],
    fill: [f32; 4],
    accent: [f32; 4],
    radius: f32,
    downloads: u64,
    dependency_count: u32,
}

#[derive(Clone, Copy)]
struct SceneEdge {
    start: [f32; 2],
    end: [f32; 2],
    color: [f32; 4],
    width: f32,
    from: Option<usize>,
    to: Option<usize>,
}

struct FocusScene {
    viewport_aspect: f32,
    nodes: Vec<SceneNode>,
    edges: Vec<SceneEdge>,
}

struct DirectSlot {
    scene_index: usize,
    package_index: usize,
    angle: f32,
    accent: [f32; 4],
}

struct SecondaryCandidate {
    package_index: usize,
    count: usize,
    downloads: u64,
    dependency_count: u32,
    parents: Vec<usize>,
}

struct DependencySceneSlot {
    package_index: usize,
    node_index: usize,
    direction: [f32; 3],
    fill: [f32; 4],
    accent: [f32; 4],
}

struct DependencySceneCandidate {
    package_index: usize,
    downloads: u64,
    parents: Vec<usize>,
}

const GLOBAL_LAYOUT_LEVELS: usize = 6;
const GLOBAL_BASE_CELL: f32 = 0.92;
const GLOBAL_CUBE_SPAN: f32 = 6.4;
const GLOBAL_PARTITION_COUNT: f32 = 6.0;
const GLOBAL_MINIMAP_GRID: i32 = 18;

struct GlobalLayout {
    nodes: Vec<GlobalLayoutNode>,
    dependent_counts: Vec<u32>,
    clusters_by_level: Vec<Vec<GlobalCluster>>,
    minimap: GlobalMinimap,
}

#[derive(Clone, Copy)]
struct GlobalLayoutNode {
    position: [f32; 3],
    rank: u32,
}

struct GlobalCluster {
    center: [f32; 3],
    count: u32,
    total_downloads: u64,
    max_downloads: u64,
    dependency_count: u32,
    dependent_count: u32,
    top_package_index: usize,
    sample_packages: Vec<usize>,
}

struct GlobalClusterAccumulator {
    weighted_sum: [f64; 3],
    total_weight: f64,
    total_downloads: u64,
    max_downloads: u64,
    dependency_sum: u64,
    dependent_sum: u64,
    count: u32,
    top_package_index: usize,
    sample_packages: Vec<usize>,
}

#[derive(Clone, Copy)]
enum OverviewNodeKind {
    Cluster,
    Crate,
}

struct GlobalSceneNode {
    kind: OverviewNodeKind,
    anchor_name: String,
    title: String,
    subtitle: String,
    position: [f32; 3],
    size: f32,
    color: [f32; 4],
    accent: [f32; 4],
    count: u32,
    downloads: u64,
    dependency_count: u32,
    dependent_count: u32,
    package_index: Option<usize>,
    members: Vec<usize>,
}

#[derive(Clone, Copy)]
struct GlobalSceneEdge {
    from: usize,
    to: usize,
    weight: f32,
    color: [f32; 4],
}

struct GlobalScene {
    level: usize,
    leaf_mode: bool,
    nodes: Vec<GlobalSceneNode>,
    edges: Vec<GlobalSceneEdge>,
}

struct GlobalMinimap {
    span: f32,
    voxels: Vec<GlobalMinimapVoxel>,
}

struct GlobalMinimapVoxel {
    position: [f32; 3],
    count: u32,
    downloads: u64,
    dependency_count: u32,
    dependent_count: u32,
    color: [f32; 4],
}

#[derive(Default)]
struct GlobalMinimapVoxelAccumulator {
    count: u32,
    total_downloads: u64,
    dependency_sum: u64,
    dependent_sum: u64,
}

impl GlobalClusterAccumulator {
    fn update(
        &mut self,
        package_index: usize,
        package: &StoredPackage,
        position: [f32; 3],
        dependent_count: u32,
    ) {
        let weight = ((package.downloads + 1) as f64).ln().max(1.0);
        self.weighted_sum[0] += position[0] as f64 * weight;
        self.weighted_sum[1] += position[1] as f64 * weight;
        self.weighted_sum[2] += position[2] as f64 * weight;
        self.total_weight += weight;
        self.total_downloads = self.total_downloads.saturating_add(package.downloads);
        self.dependency_sum = self
            .dependency_sum
            .saturating_add(package.dependency_count as u64);
        self.dependent_sum = self.dependent_sum.saturating_add(dependent_count as u64);
        self.count = self.count.saturating_add(1);

        if package.downloads >= self.max_downloads {
            self.max_downloads = package.downloads;
            self.top_package_index = package_index;
        }

        self.sample_packages.push(package_index);
    }

    fn finalize(mut self, graph: &StoredGraph) -> GlobalCluster {
        self.sample_packages.sort_by(|left, right| {
            graph.packages[*right]
                .downloads
                .cmp(&graph.packages[*left].downloads)
                .then(graph.packages[*left].crate_id.cmp(&graph.packages[*right].crate_id))
        });
        self.sample_packages.truncate(5);

        let total_weight = self.total_weight.max(1.0);
        GlobalCluster {
            center: [
                (self.weighted_sum[0] / total_weight) as f32,
                (self.weighted_sum[1] / total_weight) as f32,
                (self.weighted_sum[2] / total_weight) as f32,
            ],
            count: self.count,
            total_downloads: self.total_downloads,
            max_downloads: self.max_downloads,
            dependency_count: (self.dependency_sum / self.count.max(1) as u64) as u32,
            dependent_count: (self.dependent_sum / self.count.max(1) as u64) as u32,
            top_package_index: self.top_package_index,
            sample_packages: self.sample_packages,
        }
    }
}

impl GlobalLayout {
    fn build(graph: &StoredGraph) -> Self {
        let mut dependent_counts = vec![0_u32; graph.packages.len()];
        for package in &graph.packages {
            for dependency in graph.dependency_slice(package) {
                if let Some(slot) = dependent_counts.get_mut(dependency.package_index as usize) {
                    *slot = slot.saturating_add(1);
                }
            }
        }

        let mut order: Vec<_> = (0..graph.packages.len()).collect();
        order.sort_by(|left, right| {
            graph.packages[*right]
                .downloads
                .cmp(&graph.packages[*left].downloads)
                .then(graph.packages[*left].crate_id.cmp(&graph.packages[*right].crate_id))
        });

        let mut nodes = vec![
            GlobalLayoutNode {
                position: [0.0, 0.0, 0.0],
                rank: 0,
            };
            graph.packages.len()
        ];
        let mut cluster_maps: Vec<HashMap<(i32, i32, i32), GlobalClusterAccumulator>> =
            (0..GLOBAL_LAYOUT_LEVELS).map(|_| HashMap::new()).collect();
        let total = order.len().max(1) as f32;

        for (rank, &package_index) in order.iter().enumerate() {
            let package = &graph.packages[package_index];
            let name = graph.resolve(package.name).unwrap_or("<missing>");
            let hash = stable_hash(name.as_bytes());
            let normalized_rank = (rank as f32 + 0.5) / total;
            let download_signal = (((package.downloads + 1) as f32).log10().clamp(0.0, 8.0) / 8.0)
                .clamp(0.0, 1.0);
            let dependency_signal =
                (((package.dependency_count + 1) as f32).ln().clamp(0.0, 5.6) / 5.6).clamp(0.0, 1.0);
            let dependent_signal = (((dependent_counts[package_index] + 1) as f32)
                .ln()
                .clamp(0.0, 7.2)
                / 7.2)
                .clamp(0.0, 1.0);
            let partition_span = GLOBAL_CUBE_SPAN / GLOBAL_PARTITION_COUNT;
            let sector_x =
                ((hash_unit(hash ^ 0x31F4_8A1D) * GLOBAL_PARTITION_COUNT).floor() as i32)
                    .clamp(0, GLOBAL_PARTITION_COUNT as i32 - 1);
            let sector_y =
                ((download_signal * GLOBAL_PARTITION_COUNT).floor() as i32)
                    .clamp(0, GLOBAL_PARTITION_COUNT as i32 - 1);
            let sector_z =
                ((dependent_signal * GLOBAL_PARTITION_COUNT).floor() as i32)
                    .clamp(0, GLOBAL_PARTITION_COUNT as i32 - 1);
            let sector_center = |sector: i32| {
                (((sector as f32 + 0.5) / GLOBAL_PARTITION_COUNT) - 0.5) * GLOBAL_CUBE_SPAN
            };
            let turbulence = [
                ((rank as f32 * 0.13 + hash_unit(hash ^ 0x5163_3E2D) * TAU).sin()) * partition_span * 0.09,
                ((rank as f32 * 0.17 + hash_unit(hash ^ 0x8EC7_AA4B) * TAU).cos()) * partition_span * 0.08,
                ((rank as f32 * 0.19 + hash_unit(hash ^ 0xC2B2_AE35) * TAU).sin()) * partition_span * 0.1,
            ];
            let jitter = [
                (hash_unit(hash ^ 0x9E37_79B9) - 0.5) * partition_span * 0.84,
                (hash_unit(hash ^ 0x7F4A_7C15) - 0.5) * partition_span * 0.84,
                (hash_unit(hash ^ 0x94D0_49BB) - 0.5) * partition_span * 0.84,
            ];
            let metric_bias = [
                (dependency_signal - 0.5) * partition_span * 0.22
                    + (normalized_rank - 0.5) * partition_span * 0.14,
                (download_signal - 0.5) * partition_span * 0.3,
                (dependent_signal - 0.5) * partition_span * 0.28,
            ];
            let position = [
                sector_center(sector_x) + jitter[0] + turbulence[0] + metric_bias[0],
                sector_center(sector_y) + jitter[1] + turbulence[1] + metric_bias[1],
                sector_center(sector_z) + jitter[2] + turbulence[2] + metric_bias[2],
            ];

            nodes[package_index] = GlobalLayoutNode {
                position,
                rank: rank as u32,
            };

            for (level, map) in cluster_maps.iter_mut().enumerate() {
                let cell_size = GLOBAL_BASE_CELL / 2_f32.powi(level as i32);
                let cell_x = (position[0] / cell_size).floor() as i32;
                let cell_y = (position[1] / cell_size).floor() as i32;
                let cell_z = (position[2] / cell_size).floor() as i32;
                map.entry((cell_x, cell_y, cell_z))
                    .or_insert_with(|| GlobalClusterAccumulator {
                        weighted_sum: [0.0; 3],
                        total_weight: 0.0,
                        total_downloads: 0,
                        max_downloads: 0,
                        dependency_sum: 0,
                        dependent_sum: 0,
                        count: 0,
                        top_package_index: package_index,
                        sample_packages: Vec::new(),
                    })
                    .update(
                        package_index,
                        package,
                        position,
                        dependent_counts[package_index],
                    );
            }
        }

        let clusters_by_level = cluster_maps
            .into_iter()
            .enumerate()
            .map(|(_level, map)| map.into_values().map(|cluster| cluster.finalize(graph)).collect::<Vec<_>>())
            .collect();
        let minimap = build_global_minimap(graph, &nodes, &dependent_counts);

        Self {
            nodes,
            dependent_counts,
            clusters_by_level,
            minimap,
        }
    }

    fn query(
        &self,
        graph: &StoredGraph,
        center_x: f32,
        center_y: f32,
        center_z: f32,
        zoom: f32,
        _viewport_aspect: f32,
        max_nodes: usize,
        max_edges: usize,
    ) -> GlobalScene {
        let clamped_zoom = zoom.clamp(0.35, 28.0);
        let center = [center_x, center_y, center_z];
        let cube_radius = (GLOBAL_CUBE_SPAN * 0.56 / clamped_zoom.powf(0.72))
            .clamp(0.44, GLOBAL_CUBE_SPAN * 0.9);
        let leaf_mode = clamped_zoom >= 4.8;
        let level = overview_level_for_zoom(clamped_zoom);

        let mut nodes = if leaf_mode {
            self.query_leaf_nodes(
                graph,
                center,
                cube_radius,
                clamped_zoom,
                max_nodes,
            )
        } else {
            self.query_cluster_nodes(
                graph,
                level,
                center,
                cube_radius,
                clamped_zoom,
                max_nodes,
            )
        };

        let edges = build_global_scene_edges(graph, &mut nodes, max_edges);
        GlobalScene {
            level,
            leaf_mode,
            nodes,
            edges,
        }
    }

    fn query_cluster_nodes(
        &self,
        graph: &StoredGraph,
        level: usize,
        center: [f32; 3],
        cube_radius: f32,
        zoom: f32,
        max_nodes: usize,
    ) -> Vec<GlobalSceneNode> {
        let Some(clusters) = self.clusters_by_level.get(level) else {
            return Vec::new();
        };

        let mut visible: Vec<_> = clusters
            .iter()
            .filter_map(|cluster| {
                let chebyshev = cube_distance(cluster.center, center);
                let score = (((cluster.total_downloads + 1) as f32).log10() * 1.08)
                    + (((cluster.dependent_count + 1) as f32).ln() * 0.34)
                    - chebyshev * (0.82 + zoom * 0.08);
                if chebyshev <= cube_radius * 1.15 || score > -0.4 {
                    Some((cluster, score, chebyshev))
                } else {
                    None
                }
            })
            .collect();
        visible.sort_by(|left, right| {
            right.1.total_cmp(&left.1).then(right.0.count.cmp(&left.0.count))
        });

        let target_limit = max_nodes.max(140);
        let mut scene_nodes = Vec::with_capacity(target_limit.min(visible.len()));
        for (cluster, _score, chebyshev) in visible.into_iter().take(target_limit * 4) {
            let Some(top_package) = graph.packages.get(cluster.top_package_index) else {
                continue;
            };
            let top_name = graph.resolve(top_package.name).unwrap_or("<missing>");
            let color = cluster_color(cluster, chebyshev, cube_radius);
            let accent = brighten_color(color, 0.28);
            let title = if cluster.count <= 1 {
                top_name.to_owned()
            } else {
                format!("{top_name} +{}", cluster.count - 1)
            };
            let sample_names = cluster
                .sample_packages
                .iter()
                .take(3)
                .filter_map(|package_index| graph.packages.get(*package_index))
                .filter_map(|package| graph.resolve(package.name))
                .collect::<Vec<_>>();
            let subtitle = if cluster.count <= 1 {
                format!(
                    "{} downloads · {} dependents · {} direct dependencies",
                    top_package.downloads,
                    self.dependent_counts
                        .get(cluster.top_package_index)
                        .copied()
                        .unwrap_or_default(),
                    top_package.dependency_count
                )
            } else {
                format!(
                    "{} crates · top {} downloads · avg {} dependents",
                    cluster.count,
                    cluster.max_downloads,
                    cluster.dependent_count.max(1),
                )
            };
            scene_nodes.push(GlobalSceneNode {
                kind: if cluster.count <= 1 {
                    OverviewNodeKind::Crate
                } else {
                    OverviewNodeKind::Cluster
                },
                anchor_name: top_name.to_owned(),
                title,
                subtitle: if cluster.count <= 1 {
                    subtitle
                } else {
                    format!("{subtitle} · {}", sample_names.join(", "))
                },
                position: cluster.center,
                size: scaled_global_radius(
                    cluster.max_downloads,
                    cluster.dependency_count,
                    cluster.dependent_count,
                    0.026,
                    0.116,
                ),
                color,
                accent,
                count: cluster.count,
                downloads: cluster.total_downloads,
                dependency_count: cluster.dependency_count,
                dependent_count: cluster.dependent_count,
                package_index: Some(cluster.top_package_index),
                members: cluster.sample_packages.clone(),
            });
        }

        let mut nodes = thin_global_nodes(
            scene_nodes,
            (cube_radius * 0.21 / zoom.sqrt()).clamp(0.22, 0.48),
            max_nodes,
        );
        relax_global_nodes(&mut nodes, 6, 0.12);
        nodes
    }

    fn query_leaf_nodes(
        &self,
        graph: &StoredGraph,
        center: [f32; 3],
        cube_radius: f32,
        zoom: f32,
        max_nodes: usize,
    ) -> Vec<GlobalSceneNode> {
        let mut candidates = Vec::new();
        for (package_index, layout_node) in self.nodes.iter().enumerate() {
            let Some(package) = graph.packages.get(package_index) else {
                continue;
            };
            let chebyshev = cube_distance(layout_node.position, center);
            let score = (((package.downloads + 1) as f32).log10() * 1.06)
                + (((self.dependent_counts[package_index] + 1) as f32).ln() * 0.4)
                - chebyshev * (0.96 + zoom * 0.12);
            if chebyshev > cube_radius * 1.08 && score < -0.2 {
                continue;
            }
            let title = graph.resolve(package.name).unwrap_or("<missing>").to_owned();
            let color = crate_color(
                &title,
                package.downloads,
                package.dependency_count,
                self.dependent_counts[package_index],
                chebyshev,
                cube_radius,
            );
            let accent = brighten_color(color, 0.18);
            candidates.push(GlobalSceneNode {
                kind: OverviewNodeKind::Crate,
                anchor_name: title.clone(),
                title,
                subtitle: format!(
                    "{} downloads, {} dependents, {} direct dependencies",
                    package.downloads,
                    self.dependent_counts[package_index],
                    package.dependency_count
                ),
                position: layout_node.position,
                size: scaled_global_radius(
                    package.downloads,
                    package.dependency_count,
                    self.dependent_counts[package_index],
                    0.013,
                    0.054,
                ),
                color,
                accent,
                count: 1,
                downloads: package.downloads,
                dependency_count: package.dependency_count,
                dependent_count: self.dependent_counts[package_index],
                package_index: Some(package_index),
                members: vec![package_index],
            });
        }

        candidates.sort_by(|left, right| {
            cube_distance(left.position, center)
                .total_cmp(&cube_distance(right.position, center))
                .then(right.downloads.cmp(&left.downloads))
                .then(left.title.cmp(&right.title))
        });
        let mut nodes = thin_global_nodes(
            candidates,
            (cube_radius * 0.12 / zoom.sqrt()).clamp(0.08, 0.2),
            max_nodes,
        );
        relax_global_nodes(&mut nodes, 5, 0.08);
        nodes
    }
}

fn overview_level_for_zoom(zoom: f32) -> usize {
    if zoom < 0.85 {
        0
    } else if zoom < 1.35 {
        1
    } else if zoom < 2.1 {
        2
    } else if zoom < 3.2 {
        3
    } else if zoom < 4.8 {
        4
    } else {
        5
    }
}

fn build_global_scene_edges(
    graph: &StoredGraph,
    nodes: &mut [GlobalSceneNode],
    max_edges: usize,
) -> Vec<GlobalSceneEdge> {
    if nodes.len() < 2 || max_edges == 0 {
        return Vec::new();
    }

    let mut package_to_node = HashMap::new();
    for (scene_index, node) in nodes.iter().enumerate() {
        for &package_index in node.members.iter().take(8) {
            package_to_node.insert(package_index, scene_index);
        }
    }

    let mut edge_weights: HashMap<(usize, usize), f32> = HashMap::new();
    for (scene_index, node) in nodes.iter().enumerate() {
        for &package_index in node.members.iter().take(8) {
            let Some(package) = graph.packages.get(package_index) else {
                continue;
            };
            let base_weight = ((package.downloads + 1) as f32).log10().max(1.0);
            for dependency in graph.dependency_slice(package).iter().take(36) {
                let target_index = dependency.package_index as usize;
                let Some(&target_scene_index) = package_to_node.get(&target_index) else {
                    continue;
                };
                if scene_index == target_scene_index {
                    continue;
                }
                let key = if scene_index < target_scene_index {
                    (scene_index, target_scene_index)
                } else {
                    (target_scene_index, scene_index)
                };
                *edge_weights.entry(key).or_insert(0.0) += base_weight;
            }
        }
    }

    let mut edges: Vec<_> = edge_weights
        .into_iter()
        .map(|((from, to), weight)| {
            let mixed = mix_color(nodes[from].accent, nodes[to].accent, 0.5);
            let glow = mix_color(mixed, [1.0, 0.94, 0.82, 1.0], 0.22);
            GlobalSceneEdge {
                from,
                to,
                weight,
                color: [
                    glow[0],
                    glow[1],
                    glow[2],
                    (0.34 + weight.log10().max(0.0) * 0.12).clamp(0.34, 0.86),
                ],
            }
        })
        .collect();
    edges.sort_by(|left, right| right.weight.total_cmp(&left.weight));

    let guaranteed_per_node = if nodes.len() <= 42 { 4 } else { 3 };
    let mut guaranteed_indexes = HashSet::new();
    let mut adjacency = vec![Vec::<(usize, f32)>::new(); nodes.len()];
    for (index, edge) in edges.iter().enumerate() {
        adjacency[edge.from].push((index, edge.weight));
        adjacency[edge.to].push((index, edge.weight));
    }
    for neighbors in &mut adjacency {
        neighbors.sort_by(|left, right| right.1.total_cmp(&left.1));
        for (index, _weight) in neighbors.iter().take(guaranteed_per_node) {
            guaranteed_indexes.insert(*index);
        }
    }

    let mut selected = Vec::with_capacity(max_edges.max(1));
    for (index, edge) in edges.iter().enumerate() {
        if guaranteed_indexes.contains(&index) {
            selected.push(*edge);
            if selected.len() >= max_edges {
                return selected;
            }
        }
    }
    for (index, edge) in edges.iter().enumerate() {
        if guaranteed_indexes.contains(&index) {
            continue;
        }
        let _ = index;
        selected.push(*edge);
        if selected.len() >= max_edges {
            break;
        }
    }
    selected
}

fn thin_global_nodes(
    candidates: Vec<GlobalSceneNode>,
    min_distance: f32,
    max_nodes: usize,
) -> Vec<GlobalSceneNode> {
    if candidates.len() <= max_nodes {
        return candidates;
    }

    let mut accepted: Vec<GlobalSceneNode> = Vec::with_capacity(max_nodes);
    let mut grid: HashMap<(i32, i32, i32), Vec<usize>> = HashMap::new();
    let safe_distance = min_distance.max(0.0001);

    for candidate in candidates {
        let cell_x = (candidate.position[0] / safe_distance).floor() as i32;
        let cell_y = (candidate.position[1] / safe_distance).floor() as i32;
        let cell_z = (candidate.position[2] / safe_distance).floor() as i32;
        let mut blocked = false;

        for neighbor_x in (cell_x - 1)..=(cell_x + 1) {
            for neighbor_y in (cell_y - 1)..=(cell_y + 1) {
                for neighbor_z in (cell_z - 1)..=(cell_z + 1) {
                    let Some(indexes) = grid.get(&(neighbor_x, neighbor_y, neighbor_z)) else {
                        continue;
                    };
                    if indexes.iter().any(|&accepted_index| {
                        let accepted_node = &accepted[accepted_index];
                        let dx = accepted_node.position[0] - candidate.position[0];
                        let dy = accepted_node.position[1] - candidate.position[1];
                        let dz = accepted_node.position[2] - candidate.position[2];
                        let min_spacing = safe_distance + accepted_node.size.max(candidate.size) * 0.2;
                        dx * dx + dy * dy + dz * dz < min_spacing * min_spacing
                    }) {
                        blocked = true;
                        break;
                    }
                }
                if blocked {
                    break;
                }
            }
            if blocked {
                break;
            }
        }

        if blocked {
            continue;
        }

        grid.entry((cell_x, cell_y, cell_z))
            .or_default()
            .push(accepted.len());
        accepted.push(candidate);
        if accepted.len() >= max_nodes {
            break;
        }
    }

    accepted
}

fn relax_global_nodes(nodes: &mut [GlobalSceneNode], iterations: usize, anchor_pull: f32) {
    if nodes.len() < 2 || iterations == 0 {
        return;
    }

    let anchors = nodes.iter().map(|node| node.position).collect::<Vec<_>>();
    let cube_limit = GLOBAL_CUBE_SPAN * 0.52;
    let mut offsets = vec![[0.0_f32; 3]; nodes.len()];

    for _ in 0..iterations {
        offsets.iter_mut().for_each(|offset| *offset = [0.0; 3]);

        for left in 0..nodes.len() {
            for right in (left + 1)..nodes.len() {
                let dx = nodes[right].position[0] - nodes[left].position[0];
                let dy = nodes[right].position[1] - nodes[left].position[1];
                let dz = nodes[right].position[2] - nodes[left].position[2];
                let distance_sq = dx * dx + dy * dy + dz * dz;
                let distance = distance_sq.sqrt();
                let desired = 0.1
                    + nodes[left].size.max(nodes[right].size) * 4.8
                    + if matches!(
                        (nodes[left].kind, nodes[right].kind),
                        (OverviewNodeKind::Cluster, _) | (_, OverviewNodeKind::Cluster)
                    ) {
                        0.065
                    } else {
                        0.032
                    };

                if distance >= desired {
                    continue;
                }

                let direction = if distance > 0.0001 {
                    [dx / distance, dy / distance, dz / distance]
                } else {
                    let seed = stable_hash(
                        format!("{}:{}", nodes[left].anchor_name, nodes[right].anchor_name).as_bytes(),
                    );
                    [
                        hash_unit(seed ^ 0x9E37_79B9) - 0.5,
                        hash_unit(seed ^ 0x85EB_CA6B) - 0.5,
                        hash_unit(seed ^ 0xC2B2_AE35) - 0.5,
                    ]
                };
                let push = (desired - distance).max(0.0) * 0.34;
                for axis in 0..3 {
                    offsets[left][axis] -= direction[axis] * push;
                    offsets[right][axis] += direction[axis] * push;
                }
            }
        }

        for (index, node) in nodes.iter_mut().enumerate() {
            for axis in 0..3 {
                let anchor_delta = anchors[index][axis] - node.position[axis];
                node.position[axis] += offsets[index][axis] + anchor_delta * anchor_pull;
                node.position[axis] = node.position[axis].clamp(-cube_limit, cube_limit);
            }
        }
    }
}

fn build_global_minimap(
    graph: &StoredGraph,
    nodes: &[GlobalLayoutNode],
    dependent_counts: &[u32],
) -> GlobalMinimap {
    let cell_span = GLOBAL_CUBE_SPAN / GLOBAL_MINIMAP_GRID as f32;
    let mut voxel_accumulators =
        HashMap::<(i32, i32, i32), GlobalMinimapVoxelAccumulator>::new();

    for (package_index, layout_node) in nodes.iter().enumerate() {
        let Some(package) = graph.packages.get(package_index) else {
            continue;
        };
        let cell = minimap_cell(layout_node.position, cell_span);
        let accumulator = voxel_accumulators.entry(cell).or_default();
        accumulator.count = accumulator.count.saturating_add(1);
        accumulator.total_downloads = accumulator.total_downloads.saturating_add(package.downloads);
        accumulator.dependency_sum = accumulator
            .dependency_sum
            .saturating_add(package.dependency_count as u64);
        accumulator.dependent_sum = accumulator
            .dependent_sum
            .saturating_add(dependent_counts.get(package_index).copied().unwrap_or_default() as u64);
    }

    let mut voxels = Vec::with_capacity(voxel_accumulators.len());
    for ((x, y, z), accumulator) in voxel_accumulators {
        let count = accumulator.count.max(1);
        let avg_downloads = accumulator.total_downloads / count as u64;
        let avg_dependency_count = (accumulator.dependency_sum / count as u64) as u32;
        let avg_dependent_count = (accumulator.dependent_sum / count as u64) as u32;
        let density = (count as f32).log10().clamp(0.0, 3.2) / 3.2;
        let position = minimap_cell_center(x, y, z);
        let seed = voxel_hash(x, y, z);
        let color = color_from_metrics(
            seed,
            ((avg_downloads + 1) as f32).log10().clamp(0.0, 8.0) / 8.0,
            ((avg_dependency_count + 1) as f32).ln().clamp(0.0, 5.6) / 5.6,
            ((avg_dependent_count + 1) as f32).ln().clamp(0.0, 7.2) / 7.2,
            density,
            1.0,
            0.92,
        );
        voxels.push(GlobalMinimapVoxel {
            position,
            count,
            downloads: accumulator.total_downloads,
            dependency_count: avg_dependency_count,
            dependent_count: avg_dependent_count,
            color,
        });
    }

    voxels.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then(right.downloads.cmp(&left.downloads))
    });

    GlobalMinimap {
        span: GLOBAL_CUBE_SPAN,
        voxels,
    }
}

fn minimap_cell(position: [f32; 3], cell_span: f32) -> (i32, i32, i32) {
    let to_axis = |value: f32| {
        (((value / GLOBAL_CUBE_SPAN) + 0.5) * GLOBAL_MINIMAP_GRID as f32)
            .floor() as i32
    };
    let cell_x = to_axis(position[0]).clamp(0, GLOBAL_MINIMAP_GRID - 1);
    let cell_y = to_axis(position[1]).clamp(0, GLOBAL_MINIMAP_GRID - 1);
    let cell_z = to_axis(position[2]).clamp(0, GLOBAL_MINIMAP_GRID - 1);
    let _ = cell_span;
    (cell_x, cell_y, cell_z)
}

fn minimap_cell_center(x: i32, y: i32, z: i32) -> [f32; 3] {
    let center = |cell: i32| (((cell as f32 + 0.5) / GLOBAL_MINIMAP_GRID as f32) - 0.5) * GLOBAL_CUBE_SPAN;
    [center(x), center(y), center(z)]
}

fn voxel_hash(x: i32, y: i32, z: i32) -> u32 {
    let ux = x as u32;
    let uy = y as u32;
    let uz = z as u32;
    ux.wrapping_mul(0x9E37_79B9) ^ uy.wrapping_mul(0x85EB_CA6B) ^ uz.wrapping_mul(0xC2B2_AE35)
}

fn cube_distance(left: [f32; 3], right: [f32; 3]) -> f32 {
    (left[0] - right[0])
        .abs()
        .max((left[1] - right[1]).abs())
        .max((left[2] - right[2]).abs())
}

fn scaled_global_radius(
    downloads: u64,
    dependency_count: u32,
    dependent_count: u32,
    min: f32,
    max: f32,
) -> f32 {
    let popularity = ((downloads + 1) as f32).log10().clamp(0.0, 8.0) / 8.0;
    let complexity = ((dependency_count + 1) as f32).ln().clamp(0.0, 5.6) / 5.6;
    let influence = ((dependent_count + 1) as f32).ln().clamp(0.0, 7.2) / 7.2;
    (min + popularity * 0.04 + influence * 0.03 + complexity * 0.014).clamp(min, max)
}

fn cluster_color(cluster: &GlobalCluster, chebyshev: f32, cube_radius: f32) -> [f32; 4] {
    let count_factor = (cluster.count.max(1) as f32).log10().clamp(0.0, 2.6) / 2.6;
    let popularity = ((cluster.max_downloads + 1) as f32).log10().clamp(0.0, 8.0) / 8.0;
    let complexity = ((cluster.dependency_count + 1) as f32).ln().clamp(0.0, 5.6) / 5.6;
    let influence = ((cluster.dependent_count + 1) as f32).ln().clamp(0.0, 7.2) / 7.2;
    let distance_factor = 1.0 - (chebyshev / cube_radius.max(0.0001)).clamp(0.0, 1.4) * 0.24;
    let seed = stable_hash(&cluster.top_package_index.to_le_bytes());
    color_from_metrics(
        seed,
        popularity,
        complexity,
        influence,
        (count_factor * 0.78 + popularity * 0.22).clamp(0.0, 1.0),
        distance_factor,
        0.9,
    )
}

fn crate_color(
    name: &str,
    downloads: u64,
    dependency_count: u32,
    dependent_count: u32,
    chebyshev: f32,
    cube_radius: f32,
) -> [f32; 4] {
    let popularity = ((downloads + 1) as f32).log10().clamp(0.0, 8.0) / 8.0;
    let complexity = ((dependency_count + 1) as f32).ln().clamp(0.0, 5.6) / 5.6;
    let influence = ((dependent_count + 1) as f32).ln().clamp(0.0, 7.2) / 7.2;
    let distance_factor = 1.0 - (chebyshev / cube_radius.max(0.0001)).clamp(0.0, 1.4) * 0.18;
    let seed = stable_hash(name.as_bytes());
    color_from_metrics(
        seed,
        popularity,
        complexity,
        influence,
        (0.24 + popularity * 0.32 + influence * 0.24).clamp(0.0, 1.0),
        distance_factor,
        0.94,
    )
}

fn color_from_metrics(
    seed: u32,
    popularity: f32,
    complexity: f32,
    influence: f32,
    density: f32,
    distance_factor: f32,
    alpha: f32,
) -> [f32; 4] {
    let hue = metric_palette_hue(seed, popularity, complexity, influence, density);
    let saturation = (0.54
        + popularity * 0.14
        + influence * 0.11
        + density * 0.08
        - complexity * 0.06
        + hash_unit(seed ^ 0x68E3_1DA4) * 0.08)
        .clamp(0.42, 0.92);
    let lightness = (0.3
        + popularity * 0.16
        + density * 0.08
        + distance_factor * 0.1
        + (1.0 - complexity) * 0.05
        + hash_unit(seed ^ 0xB529_7A4D) * 0.07)
        .clamp(0.26, 0.78);
    hsl_color(hue, saturation, lightness, alpha)
}

fn metric_palette_hue(
    seed: u32,
    popularity: f32,
    complexity: f32,
    influence: f32,
    density: f32,
) -> f32 {
    const PALETTE: [f32; 10] = [18.0, 42.0, 74.0, 108.0, 146.0, 188.0, 222.0, 258.0, 294.0, 332.0];
    let dominant_shift = if influence > popularity.max(complexity) + 0.08 {
        8
    } else if complexity > popularity.max(influence) + 0.06 {
        3
    } else if popularity > influence.max(complexity) + 0.06 {
        1
    } else {
        5
    };
    let metric_slot = ((popularity * 1.7 + complexity * 2.3 + influence * 2.9 + density * 1.1)
        * PALETTE.len() as f32)
        .floor() as usize;
    let slot = (metric_slot + dominant_shift) % PALETTE.len();
    PALETTE[slot] + (hash_unit(seed ^ 0x1B87_3593) - 0.5) * 20.0
}

fn brighten_color(color: [f32; 4], amount: f32) -> [f32; 4] {
    [
        color[0] + (1.0 - color[0]) * amount,
        color[1] + (1.0 - color[1]) * amount,
        color[2] + (1.0 - color[2]) * amount,
        (color[3] + amount * 0.1).min(1.0),
    ]
}

fn stable_hash(bytes: &[u8]) -> u32 {
    let mut hash = 0x811C_9DC5_u32;
    for &byte in bytes {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

fn hash_unit(hash: u32) -> f32 {
    hash as f32 / u32::MAX as f32
}

fn hsl_color(hue_degrees: f32, saturation: f32, lightness: f32, alpha: f32) -> [f32; 4] {
    let hue = hue_degrees.rem_euclid(360.0) / 360.0;
    let saturation = saturation.clamp(0.0, 1.0);
    let lightness = lightness.clamp(0.0, 1.0);

    let q = if lightness < 0.5 {
        lightness * (1.0 + saturation)
    } else {
        lightness + saturation - lightness * saturation
    };
    let p = 2.0 * lightness - q;
    [
        hue_to_rgb(p, q, hue + 1.0 / 3.0),
        hue_to_rgb(p, q, hue),
        hue_to_rgb(p, q, hue - 1.0 / 3.0),
        alpha,
    ]
}

fn hue_to_rgb(p: f32, q: f32, mut t: f32) -> f32 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        return p + (q - p) * 6.0 * t;
    }
    if t < 1.0 / 2.0 {
        return q;
    }
    if t < 2.0 / 3.0 {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    p
}

fn build_focus_scene(
    graph: &StoredGraph,
    crate_name: &str,
    viewport_aspect: f32,
) -> Result<FocusScene, JsValue> {
    let (focal_index, focal_package) = graph
        .package_by_name(crate_name)
        .ok_or_else(|| JsValue::from_str("crate was not found in the graph"))?;
    let focal_name = graph.resolve(focal_package.name).unwrap_or(crate_name).to_owned();

    let mut scene = FocusScene {
        viewport_aspect,
        nodes: Vec::new(),
        edges: Vec::new(),
    };

    append_ring_segments(
        &mut scene,
        [0.0, 0.0],
        0.54,
        viewport_aspect,
        [0.29, 0.43, 0.52, 0.15],
        0.0032,
    );
    append_ring_segments(
        &mut scene,
        [0.0, 0.0],
        1.06,
        viewport_aspect,
        [0.87, 0.76, 0.62, 0.08],
        0.0022,
    );

    let focal_index_in_scene = push_node(
        &mut scene,
        SceneNode {
            name: focal_name,
            role: NodeRole::Focal,
            position: [0.0, 0.0],
            fill: [0.98, 0.46, 0.20, 0.98],
            accent: [1.0, 0.86, 0.58, 0.96],
            radius: 0.074,
            downloads: focal_package.downloads,
            dependency_count: focal_package.dependency_count,
        },
    );

    let mut direct_neighbors: Vec<_> = graph
        .dependency_slice(focal_package)
        .iter()
        .filter_map(|dependency| {
            let package_index = dependency.package_index as usize;
            graph.packages.get(package_index).map(|target| {
                let (fill, accent) = kind_palette(dependency.kind().as_str());
                (package_index, target, fill, accent)
            })
        })
        .collect();
    direct_neighbors.sort_by(|left, right| {
        right
            .1
            .downloads
            .cmp(&left.1.downloads)
            .then(left.1.crate_id.cmp(&right.1.crate_id))
    });
    direct_neighbors.truncate(18);

    let mut direct_slots = Vec::with_capacity(direct_neighbors.len());
    let mut direct_package_indices = HashSet::new();
    for (slot, (package_index, package, fill, accent)) in direct_neighbors.iter().enumerate() {
        direct_package_indices.insert(*package_index);
        let count = direct_neighbors.len().max(1) as f32;
        let angle = -PI * 0.5 + TAU * slot as f32 / count;
        let position = polar(angle, 0.58, viewport_aspect);
        let radius = scaled_radius(package.downloads, 0.027, 0.052, 0.0048);
        let scene_index = push_node(
            &mut scene,
            SceneNode {
                name: graph.resolve(package.name).unwrap_or("<missing>").to_owned(),
                role: NodeRole::DirectDependency,
                position,
                fill: *fill,
                accent: *accent,
                radius,
                downloads: package.downloads,
                dependency_count: package.dependency_count,
            },
        );
        push_edge_between(
            &mut scene,
            focal_index_in_scene,
            scene_index,
            [accent[0], accent[1], accent[2], 0.28],
            0.006,
        );
        direct_slots.push(DirectSlot {
            scene_index,
            package_index: *package_index,
            angle,
            accent: *accent,
        });
    }

    let mut secondaries = HashMap::<usize, SecondaryCandidate>::new();
    for (slot_index, slot) in direct_slots.iter().enumerate().take(14) {
        let Some(package) = graph.packages.get(slot.package_index) else {
            continue;
        };

        let mut local_unique = HashSet::new();
        let mut candidates: Vec<_> = graph
            .dependency_slice(package)
            .iter()
            .filter_map(|dependency| {
                let package_index = dependency.package_index as usize;
                if package_index == focal_index
                    || direct_package_indices.contains(&package_index)
                    || !local_unique.insert(package_index)
                {
                    return None;
                }

                graph.packages.get(package_index).map(|target| (package_index, target))
            })
            .collect();
        candidates.sort_by(|left, right| {
            right
                .1
                .downloads
                .cmp(&left.1.downloads)
                .then(left.1.crate_id.cmp(&right.1.crate_id))
        });
        candidates.truncate(5);

        for (package_index, target) in candidates {
            let entry = secondaries
                .entry(package_index)
                .or_insert_with(|| SecondaryCandidate {
                    package_index,
                    count: 0,
                    downloads: target.downloads,
                    dependency_count: target.dependency_count,
                    parents: Vec::new(),
                });
            entry.count += 1;
            if !entry.parents.contains(&slot_index) {
                entry.parents.push(slot_index);
            }
            entry.downloads = entry.downloads.max(target.downloads);
        }
    }

    let mut secondary_list: Vec<_> = secondaries.into_values().collect();
    secondary_list.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then(right.downloads.cmp(&left.downloads))
    });
    secondary_list.truncate(26);

    let mut grouped_secondaries: HashMap<usize, Vec<SecondaryCandidate>> = HashMap::new();
    for candidate in secondary_list {
        if let Some(primary_parent) = candidate.parents.first().copied() {
            grouped_secondaries
                .entry(primary_parent)
                .or_default()
                .push(candidate);
        }
    }

    let mut group_keys: Vec<_> = grouped_secondaries.keys().copied().collect();
    group_keys.sort_unstable();
    for key in group_keys {
        let Some(slot) = direct_slots.get(key) else {
            continue;
        };
        let Some(mut group) = grouped_secondaries.remove(&key) else {
            continue;
        };
        group.sort_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then(right.downloads.cmp(&left.downloads))
        });

        let total = group.len();
        for (index, candidate) in group.into_iter().enumerate() {
            let spread = (0.44 + total as f32 * 0.05).min(1.05);
            let offset = if total <= 1 {
                0.0
            } else {
                -spread * 0.5 + spread * index as f32 / (total - 1) as f32
            };
            let orbit = 0.92 + 0.08 * (index % 2) as f32;
            let position = polar(slot.angle + offset, orbit, viewport_aspect);
            let Some(package) = graph.packages.get(candidate.package_index) else {
                continue;
            };
            let fill = mix_color([0.15, 0.77, 0.78, 0.88], slot.accent, 0.34);
            let accent = mix_color([0.96, 0.94, 0.84, 0.94], slot.accent, 0.46);
            let scene_index = push_node(
                &mut scene,
                SceneNode {
                    name: graph.resolve(package.name).unwrap_or("<missing>").to_owned(),
                    role: NodeRole::SecondaryDependency,
                    position,
                    fill,
                    accent,
                    radius: scaled_radius(package.downloads, 0.017, 0.031, 0.0032),
                    downloads: package.downloads,
                    dependency_count: candidate.dependency_count,
                },
            );
            push_edge_between(
                &mut scene,
                slot.scene_index,
                scene_index,
                [slot.accent[0], slot.accent[1], slot.accent[2], 0.16],
                0.0038,
            );
            for parent_slot in candidate.parents.iter().skip(1).take(1) {
                if let Some(extra_parent) = direct_slots.get(*parent_slot) {
                    push_edge_between(
                        &mut scene,
                        extra_parent.scene_index,
                        scene_index,
                        [0.56, 0.74, 0.82, 0.09],
                        0.0022,
                    );
                }
            }
        }
    }

    let dependents = collect_dependents_sorted(graph, focal_index, 12);
    for (slot, (package_index, package)) in dependents.into_iter().enumerate() {
        let count = 12.0_f32.max(1.0);
        let angle = PI * (1.14 + 0.72 * slot as f32 / count);
        let position = polar(angle, 1.18, viewport_aspect);
        let fill = [0.93, 0.88, 0.80, 0.56];
        let accent = [0.98, 0.73, 0.42, 0.72];
        let scene_index = push_node(
            &mut scene,
            SceneNode {
                name: graph.resolve(package.name).unwrap_or("<missing>").to_owned(),
                role: NodeRole::Dependent,
                position,
                fill,
                accent,
                radius: scaled_radius(package.downloads, 0.014, 0.024, 0.0026),
                downloads: package.downloads,
                dependency_count: package.dependency_count,
            },
        );
        let _ = package_index;
        push_edge_between(
            &mut scene,
            scene_index,
            focal_index_in_scene,
            [0.94, 0.86, 0.72, 0.09],
            0.003,
        );
    }

    if direct_slots.is_empty() {
        let orbit = polar(-PI * 0.5, 0.62, viewport_aspect);
        let empty_index = push_node(
            &mut scene,
            SceneNode {
                name: "no direct dependencies in snapshot".to_owned(),
                role: NodeRole::SecondaryDependency,
                position: orbit,
                fill: [0.32, 0.78, 0.82, 0.7],
                accent: [0.88, 0.95, 0.98, 0.92],
                radius: 0.024,
                downloads: 0,
                dependency_count: 0,
            },
        );
        push_edge_between(
            &mut scene,
            focal_index_in_scene,
            empty_index,
            [0.35, 0.76, 0.81, 0.18],
            0.004,
        );
    }

    Ok(scene)
}

fn build_dependency_focus_scene(
    graph: &StoredGraph,
    dependent_counts: &[u32],
    crate_name: &str,
) -> Result<GlobalScene, JsValue> {
    const MAX_DIRECT: usize = 32;
    const MAX_SECONDARY: usize = 180;
    const MAX_TERTIARY_LEAVES: usize = 120;

    let (focal_index, focal_package) = graph
        .package_by_name(crate_name)
        .ok_or_else(|| JsValue::from_str("crate was not found in the graph"))?;
    let focal_name = graph.resolve(focal_package.name).unwrap_or(crate_name).to_owned();
    let focal_color = mix_color(
        crate_color(
            &focal_name,
            focal_package.downloads,
            focal_package.dependency_count,
            dependent_counts.get(focal_index).copied().unwrap_or_default(),
            0.0,
            1.0,
        ),
        [0.98, 0.46, 0.2, 0.98],
        0.46,
    );
    let focal_accent = brighten_color(focal_color, 0.24);

    let mut scene = GlobalScene {
        level: usize::MAX,
        leaf_mode: true,
        nodes: vec![GlobalSceneNode {
            kind: OverviewNodeKind::Crate,
            anchor_name: focal_name.clone(),
            title: focal_name.clone(),
            subtitle: dependency_summary_line(graph, dependent_counts, focal_index),
            position: [0.0, 0.0, 0.0],
            size: scaled_global_radius(
                focal_package.downloads,
                focal_package.dependency_count,
                dependent_counts.get(focal_index).copied().unwrap_or_default(),
                0.055,
                0.11,
            ),
            color: focal_color,
            accent: focal_accent,
            count: 1,
            downloads: focal_package.downloads,
            dependency_count: focal_package.dependency_count,
            dependent_count: dependent_counts.get(focal_index).copied().unwrap_or_default(),
            package_index: Some(focal_index),
            members: vec![focal_index],
        }],
        edges: Vec::new(),
    };

    let mut direct_entries = collect_unique_dependencies(graph, focal_package);
    direct_entries.sort_by(|left, right| {
        graph.packages[right.0]
            .downloads
            .cmp(&graph.packages[left.0].downloads)
            .then(graph.packages[left.0].crate_id.cmp(&graph.packages[right.0].crate_id))
    });
    if direct_entries.len() > MAX_DIRECT {
        direct_entries.truncate(MAX_DIRECT);
    }

    let direct_set = direct_entries
        .iter()
        .map(|(package_index, _kind)| *package_index)
        .collect::<HashSet<_>>();
    let direct_total = direct_entries.len().max(1);
    let mut direct_slots = Vec::with_capacity(direct_entries.len());

    for (slot_index, (package_index, kind)) in direct_entries.iter().enumerate() {
        let Some(package) = graph.packages.get(*package_index) else {
            continue;
        };
        let name = graph.resolve(package.name).unwrap_or("<missing>").to_owned();
        let direction = fibonacci_direction(slot_index, direct_total, 0.42);
        let position = scale3(direction, 1.18);
        let (kind_fill, kind_accent) = kind_palette(kind);
        let base_color = crate_color(
            &name,
            package.downloads,
            package.dependency_count,
            dependent_counts.get(*package_index).copied().unwrap_or_default(),
            0.0,
            1.0,
        );
        let fill = mix_color(base_color, kind_fill, 0.34);
        let accent = mix_color(brighten_color(base_color, 0.18), kind_accent, 0.46);
        let node_index = scene.nodes.len();
        scene.nodes.push(GlobalSceneNode {
            kind: OverviewNodeKind::Crate,
            anchor_name: name.clone(),
            title: name,
            subtitle: format!(
                "direct dependency · {} · {}",
                kind,
                dependency_summary_line(graph, dependent_counts, *package_index)
            ),
            position,
            size: scaled_global_radius(
                package.downloads,
                package.dependency_count,
                dependent_counts.get(*package_index).copied().unwrap_or_default(),
                0.026,
                0.072,
            ),
            color: fill,
            accent,
            count: 1,
            downloads: package.downloads,
            dependency_count: package.dependency_count,
            dependent_count: dependent_counts.get(*package_index).copied().unwrap_or_default(),
            package_index: Some(*package_index),
            members: vec![*package_index],
        });
        scene.edges.push(GlobalSceneEdge {
            from: 0,
            to: node_index,
            weight: 1.8,
            color: [accent[0], accent[1], accent[2], 0.54],
        });
        direct_slots.push(DependencySceneSlot {
            package_index: *package_index,
            node_index,
            direction,
            fill,
            accent,
        });
    }

    let mut secondaries = HashMap::<usize, DependencySceneCandidate>::new();
    for (slot_index, direct_slot) in direct_slots.iter().enumerate() {
        let Some(package) = graph.packages.get(direct_slot.package_index) else {
            continue;
        };
        let mut local_seen = HashSet::new();
        for dependency in graph.dependency_slice(package) {
            let target_index = dependency.package_index as usize;
            if target_index == focal_index
                || direct_set.contains(&target_index)
                || !local_seen.insert(target_index)
            {
                continue;
            }
            let Some(target_package) = graph.packages.get(target_index) else {
                continue;
            };
            let entry = secondaries
                .entry(target_index)
                .or_insert_with(|| DependencySceneCandidate {
                    package_index: target_index,
                    downloads: target_package.downloads,
                    parents: Vec::new(),
                });
            if !entry.parents.contains(&slot_index) {
                entry.parents.push(slot_index);
            }
        }
    }

    let mut secondary_list = secondaries.into_values().collect::<Vec<_>>();
    secondary_list.sort_by(|left, right| {
        right
            .parents
            .len()
            .cmp(&left.parents.len())
            .then(right.downloads.cmp(&left.downloads))
            .then(left.package_index.cmp(&right.package_index))
    });
    if secondary_list.len() > MAX_SECONDARY {
        secondary_list.truncate(MAX_SECONDARY);
    }

    let mut grouped_secondaries = HashMap::<usize, Vec<DependencySceneCandidate>>::new();
    for candidate in secondary_list {
        if let Some(primary_parent) = candidate.parents.first().copied() {
            grouped_secondaries
                .entry(primary_parent)
                .or_default()
                .push(candidate);
        }
    }

    let mut secondary_slots = Vec::new();
    let mut secondary_index_by_package = HashMap::new();
    for (parent_slot_index, mut group) in grouped_secondaries {
        let Some(parent_slot) = direct_slots.get(parent_slot_index) else {
            continue;
        };
        group.sort_by(|left, right| {
            right
                .parents
                .len()
                .cmp(&left.parents.len())
                .then(right.downloads.cmp(&left.downloads))
        });
        let (basis_a, basis_b) = orthonormal_basis(parent_slot.direction);
        let total = group.len().max(1) as f32;

        for (index, candidate) in group.into_iter().enumerate() {
            let Some(package) = graph.packages.get(candidate.package_index) else {
                continue;
            };
            let name = graph.resolve(package.name).unwrap_or("<missing>").to_owned();
            let angle = TAU * index as f32 / total + parent_slot_index as f32 * 0.27;
            let orbit = 0.28 + 0.04 * (index / 5) as f32;
            let branch = 2.02 + 0.12 * (index % 3) as f32;
            let base = scale3(parent_slot.direction, branch);
            let position = add3(
                base,
                add3(
                    scale3(basis_a, orbit * angle.cos()),
                    scale3(basis_b, orbit * angle.sin()),
                ),
            );
            let direction = normalize3d(position);
            let base_color = crate_color(
                &name,
                package.downloads,
                package.dependency_count,
                dependent_counts.get(candidate.package_index).copied().unwrap_or_default(),
                0.0,
                1.0,
            );
            let fill = mix_color(base_color, parent_slot.fill, 0.18);
            let accent = mix_color(brighten_color(base_color, 0.18), parent_slot.accent, 0.34);
            let node_index = scene.nodes.len();
            scene.nodes.push(GlobalSceneNode {
                kind: OverviewNodeKind::Crate,
                anchor_name: name.clone(),
                title: name,
                subtitle: format!(
                    "secondary dependency · {}",
                    dependency_summary_line(graph, dependent_counts, candidate.package_index)
                ),
                position,
                size: scaled_global_radius(
                    package.downloads,
                    package.dependency_count,
                    dependent_counts.get(candidate.package_index).copied().unwrap_or_default(),
                    0.018,
                    0.056,
                ),
                color: fill,
                accent,
                count: 1,
                downloads: package.downloads,
                dependency_count: package.dependency_count,
                dependent_count: dependent_counts.get(candidate.package_index).copied().unwrap_or_default(),
                package_index: Some(candidate.package_index),
                members: vec![candidate.package_index],
            });
            scene.edges.push(GlobalSceneEdge {
                from: parent_slot.node_index,
                to: node_index,
                weight: 1.28,
                color: [accent[0], accent[1], accent[2], 0.42],
            });
            for extra_parent_index in candidate.parents.iter().skip(1).take(2) {
                if let Some(extra_parent) = direct_slots.get(*extra_parent_index) {
                    scene.edges.push(GlobalSceneEdge {
                        from: extra_parent.node_index,
                        to: node_index,
                        weight: 0.74,
                        color: [extra_parent.accent[0], extra_parent.accent[1], extra_parent.accent[2], 0.22],
                    });
                }
            }
            secondary_index_by_package.insert(candidate.package_index, secondary_slots.len());
            secondary_slots.push(DependencySceneSlot {
                package_index: candidate.package_index,
                node_index,
                direction,
                fill,
                accent,
            });
        }
    }

    let secondary_set = secondary_slots
        .iter()
        .map(|slot| slot.package_index)
        .collect::<HashSet<_>>();
    let mut tertiary_candidates = HashMap::<usize, DependencySceneCandidate>::new();
    for secondary_slot in &secondary_slots {
        let Some(package) = graph.packages.get(secondary_slot.package_index) else {
            continue;
        };
        let mut local_seen = HashSet::new();
        for dependency in graph.dependency_slice(package) {
            let target_index = dependency.package_index as usize;
            if target_index == focal_index
                || direct_set.contains(&target_index)
                || secondary_set.contains(&target_index)
                || !local_seen.insert(target_index)
            {
                continue;
            }
            let Some(target_package) = graph.packages.get(target_index) else {
                continue;
            };
            if target_package.dependency_count > 0 {
                continue;
            }
            let entry = tertiary_candidates
                .entry(target_index)
                .or_insert_with(|| DependencySceneCandidate {
                    package_index: target_index,
                    downloads: target_package.downloads,
                    parents: Vec::new(),
                });
            if let Some(parent_index) = secondary_index_by_package.get(&secondary_slot.package_index).copied() {
                if !entry.parents.contains(&parent_index) {
                    entry.parents.push(parent_index);
                }
            }
        }
    }

    let mut tertiary_list = tertiary_candidates.into_values().collect::<Vec<_>>();
    tertiary_list.sort_by(|left, right| {
        right
            .parents
            .len()
            .cmp(&left.parents.len())
            .then(right.downloads.cmp(&left.downloads))
            .then(left.package_index.cmp(&right.package_index))
    });
    if tertiary_list.len() > MAX_TERTIARY_LEAVES {
        tertiary_list.truncate(MAX_TERTIARY_LEAVES);
    }

    let mut grouped_tertiaries = HashMap::<usize, Vec<DependencySceneCandidate>>::new();
    for candidate in tertiary_list {
        if let Some(primary_parent) = candidate.parents.first().copied() {
            grouped_tertiaries
                .entry(primary_parent)
                .or_default()
                .push(candidate);
        }
    }

    for (parent_slot_index, mut group) in grouped_tertiaries {
        let Some(parent_slot) = secondary_slots.get(parent_slot_index) else {
            continue;
        };
        group.sort_by(|left, right| {
            right
                .parents
                .len()
                .cmp(&left.parents.len())
                .then(right.downloads.cmp(&left.downloads))
        });
        let (basis_a, basis_b) = orthonormal_basis(parent_slot.direction);
        let total = group.len().max(1) as f32;

        for (index, candidate) in group.into_iter().enumerate() {
            let Some(package) = graph.packages.get(candidate.package_index) else {
                continue;
            };
            let name = graph.resolve(package.name).unwrap_or("<missing>").to_owned();
            let angle = TAU * index as f32 / total + parent_slot_index as f32 * 0.39;
            let orbit = 0.18 + 0.03 * (index / 4) as f32;
            let branch = 2.92 + 0.08 * (index % 2) as f32;
            let base = scale3(parent_slot.direction, branch);
            let position = add3(
                base,
                add3(
                    scale3(basis_a, orbit * angle.cos()),
                    scale3(basis_b, orbit * angle.sin()),
                ),
            );
            let base_color = crate_color(
                &name,
                package.downloads,
                package.dependency_count,
                dependent_counts.get(candidate.package_index).copied().unwrap_or_default(),
                0.0,
                1.0,
            );
            let fill = mix_color(base_color, parent_slot.fill, 0.12);
            let accent = mix_color(brighten_color(base_color, 0.18), parent_slot.accent, 0.24);
            let node_index = scene.nodes.len();
            scene.nodes.push(GlobalSceneNode {
                kind: OverviewNodeKind::Crate,
                anchor_name: name.clone(),
                title: name,
                subtitle: format!(
                    "leaf dependency · {}",
                    dependency_summary_line(graph, dependent_counts, candidate.package_index)
                ),
                position,
                size: scaled_global_radius(
                    package.downloads,
                    package.dependency_count,
                    dependent_counts.get(candidate.package_index).copied().unwrap_or_default(),
                    0.013,
                    0.032,
                ),
                color: fill,
                accent,
                count: 1,
                downloads: package.downloads,
                dependency_count: package.dependency_count,
                dependent_count: dependent_counts.get(candidate.package_index).copied().unwrap_or_default(),
                package_index: Some(candidate.package_index),
                members: vec![candidate.package_index],
            });
            scene.edges.push(GlobalSceneEdge {
                from: parent_slot.node_index,
                to: node_index,
                weight: 0.92,
                color: [accent[0], accent[1], accent[2], 0.34],
            });
            for extra_parent_index in candidate.parents.iter().skip(1).take(1) {
                if let Some(extra_parent) = secondary_slots.get(*extra_parent_index) {
                    scene.edges.push(GlobalSceneEdge {
                        from: extra_parent.node_index,
                        to: node_index,
                        weight: 0.56,
                        color: [extra_parent.accent[0], extra_parent.accent[1], extra_parent.accent[2], 0.18],
                    });
                }
            }
        }
    }

    relax_global_nodes(&mut scene.nodes, 5, 0.24);
    Ok(scene)
}

fn dependency_summary_line(
    graph: &StoredGraph,
    dependent_counts: &[u32],
    package_index: usize,
) -> String {
    let Some(package) = graph.packages.get(package_index) else {
        return "crate data unavailable".to_owned();
    };
    format!(
        "{} downloads · {} dependents · {} direct dependencies",
        package.downloads,
        dependent_counts.get(package_index).copied().unwrap_or_default(),
        package.dependency_count
    )
}

fn collect_unique_dependencies(
    graph: &StoredGraph,
    package: &StoredPackage,
) -> Vec<(usize, &'static str)> {
    let mut best_kind = HashMap::<usize, &'static str>::new();
    for dependency in graph.dependency_slice(package) {
        let target_index = dependency.package_index as usize;
        let kind = dependency.kind().as_str();
        match best_kind.get(&target_index).copied() {
            Some(existing_kind)
                if dependency_kind_priority(existing_kind) <= dependency_kind_priority(kind) => {}
            _ => {
                best_kind.insert(target_index, kind);
            }
        }
    }
    best_kind.into_iter().collect()
}

fn dependency_kind_priority(kind: &str) -> u8 {
    match kind {
        "normal" => 0,
        "build" => 1,
        "dev" => 2,
        _ => 3,
    }
}

fn fibonacci_direction(index: usize, total: usize, vertical_scale: f32) -> [f32; 3] {
    let golden_angle = PI * (3.0 - 5.0_f32.sqrt());
    let t = (index as f32 + 0.5) / total.max(1) as f32;
    let y = (1.0 - t * 2.0) * vertical_scale;
    let radius = (1.0 - y * y).max(0.08).sqrt();
    let theta = golden_angle * index as f32;
    normalize3d([radius * theta.cos(), y, radius * theta.sin()])
}

fn normalize3d(vector: [f32; 3]) -> [f32; 3] {
    let length = (vector[0] * vector[0] + vector[1] * vector[1] + vector[2] * vector[2]).sqrt();
    if length <= 0.0001 {
        return [0.0, 0.0, 1.0];
    }
    [vector[0] / length, vector[1] / length, vector[2] / length]
}

fn scale3(vector: [f32; 3], scale: f32) -> [f32; 3] {
    [vector[0] * scale, vector[1] * scale, vector[2] * scale]
}

fn add3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

fn cross3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn orthonormal_basis(direction: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    let helper = if direction[1].abs() < 0.82 {
        [0.0, 1.0, 0.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let basis_a = normalize3d(cross3(direction, helper));
    let basis_b = normalize3d(cross3(direction, basis_a));
    (basis_a, basis_b)
}

fn collect_dependents_sorted(
    graph: &StoredGraph,
    focal_index: usize,
    limit: usize,
) -> Vec<(usize, &StoredPackage)> {
    let mut dependents = Vec::new();
    for (package_index, package) in graph.packages.iter().enumerate() {
        if package_index == focal_index {
            continue;
        }

        if graph
            .dependency_slice(package)
            .iter()
            .any(|dependency| dependency.package_index as usize == focal_index)
        {
            dependents.push((package_index, package));
        }
    }

    dependents.sort_by(|left, right| {
        right
            .1
            .downloads
            .cmp(&left.1.downloads)
            .then(left.1.crate_id.cmp(&right.1.crate_id))
    });
    dependents.truncate(limit);
    dependents
}

fn push_node(scene: &mut FocusScene, node: SceneNode) -> usize {
    scene.nodes.push(node);
    scene.nodes.len() - 1
}

fn push_edge_between(
    scene: &mut FocusScene,
    from: usize,
    to: usize,
    color: [f32; 4],
    width: f32,
) {
    let start = scene.nodes[from].position;
    let end = scene.nodes[to].position;
    scene.edges.push(SceneEdge {
        start,
        end,
        color,
        width,
        from: Some(from),
        to: Some(to),
    });
}

fn push_segment(
    scene: &mut FocusScene,
    start: [f32; 2],
    end: [f32; 2],
    color: [f32; 4],
    width: f32,
) {
    scene.edges.push(SceneEdge {
        start,
        end,
        color,
        width,
        from: None,
        to: None,
    });
}

fn append_ring_segments(
    scene: &mut FocusScene,
    center: [f32; 2],
    radius: f32,
    viewport_aspect: f32,
    color: [f32; 4],
    width: f32,
) {
    let segments = 96;
    let mut previous = None;
    for index in 0..=segments {
        let angle = TAU * index as f32 / segments as f32;
        let point = [
            center[0] + angle.cos() * radius * viewport_aspect,
            center[1] + angle.sin() * radius,
        ];
        if let Some(last) = previous {
            push_segment(scene, last, point, color, width);
        }
        previous = Some(point);
    }
}

fn scene_to_js(scene: &FocusScene) -> Result<Object, JsValue> {
    let result = Object::new();
    let nodes = Array::new();
    let edges = Array::new();

    for (index, node) in scene.nodes.iter().enumerate() {
        nodes.push(&scene_node_to_js(index, node)?);
    }

    for edge in &scene.edges {
        let entry = Object::new();
        set_number(&entry, "from", edge.from.map(|value| value as f64))?;
        set_number(&entry, "to", edge.to.map(|value| value as f64))?;
        set_f64(&entry, "x1", edge.start[0] as f64)?;
        set_f64(&entry, "y1", edge.start[1] as f64)?;
        set_f64(&entry, "x2", edge.end[0] as f64)?;
        set_f64(&entry, "y2", edge.end[1] as f64)?;
        Reflect::set(&entry, &JsValue::from_str("color"), &color_to_js(edge.color))?;
        set_f64(&entry, "width", edge.width as f64)?;
        edges.push(&entry);
    }

    set_f64(&result, "viewportAspect", scene.viewport_aspect as f64)?;
    Reflect::set(&result, &JsValue::from_str("nodes"), &nodes)?;
    Reflect::set(&result, &JsValue::from_str("edges"), &edges)?;
    Ok(result)
}

fn global_scene_to_js(scene: &GlobalScene) -> Result<Object, JsValue> {
    let result = Object::new();
    let nodes = Array::new();
    let edges = Array::new();

    for (index, node) in scene.nodes.iter().enumerate() {
        let entry = Object::new();
        let kind = match node.kind {
            OverviewNodeKind::Cluster => "cluster",
            OverviewNodeKind::Crate => "crate",
        };
        Reflect::set(&entry, &JsValue::from_str("kind"), &JsValue::from_str(kind))?;
        Reflect::set(
            &entry,
            &JsValue::from_str("title"),
            &JsValue::from_str(&node.title),
        )?;
        Reflect::set(
            &entry,
            &JsValue::from_str("anchorName"),
            &JsValue::from_str(&node.anchor_name),
        )?;
        Reflect::set(
            &entry,
            &JsValue::from_str("subtitle"),
            &JsValue::from_str(&node.subtitle),
        )?;
        set_f64(&entry, "index", index as f64)?;
        set_f64(&entry, "x", node.position[0] as f64)?;
        set_f64(&entry, "y", node.position[1] as f64)?;
        set_f64(&entry, "z", node.position[2] as f64)?;
        set_f64(&entry, "size", node.size as f64)?;
        set_f64(&entry, "count", node.count as f64)?;
        set_f64(&entry, "downloads", node.downloads as f64)?;
        set_f64(&entry, "dependencyCount", node.dependency_count as f64)?;
        set_f64(&entry, "dependentCount", node.dependent_count as f64)?;
        set_number(&entry, "packageIndex", node.package_index.map(|value| value as f64))?;
        Reflect::set(&entry, &JsValue::from_str("color"), &color_to_js(node.color))?;
        Reflect::set(&entry, &JsValue::from_str("accent"), &color_to_js(node.accent))?;

        nodes.push(&entry);
    }

    for edge in &scene.edges {
        let entry = Object::new();
        let from = &scene.nodes[edge.from];
        let to = &scene.nodes[edge.to];
        set_f64(&entry, "from", edge.from as f64)?;
        set_f64(&entry, "to", edge.to as f64)?;
        set_f64(&entry, "x1", from.position[0] as f64)?;
        set_f64(&entry, "y1", from.position[1] as f64)?;
        set_f64(&entry, "z1", from.position[2] as f64)?;
        set_f64(&entry, "x2", to.position[0] as f64)?;
        set_f64(&entry, "y2", to.position[1] as f64)?;
        set_f64(&entry, "z2", to.position[2] as f64)?;
        set_f64(&entry, "weight", edge.weight as f64)?;
        Reflect::set(&entry, &JsValue::from_str("color"), &color_to_js(edge.color))?;
        edges.push(&entry);
    }

    set_f64(&result, "level", scene.level as f64)?;
    Reflect::set(
        &result,
        &JsValue::from_str("leafMode"),
        &JsValue::from_bool(scene.leaf_mode),
    )?;
    Reflect::set(&result, &JsValue::from_str("nodes"), &nodes)?;
    Reflect::set(&result, &JsValue::from_str("edges"), &edges)?;
    Ok(result)
}

fn global_minimap_to_js(minimap: &GlobalMinimap) -> Result<Object, JsValue> {
    let result = Object::new();
    let voxels = Array::new();

    for voxel in &minimap.voxels {
        let entry = Object::new();
        set_f64(&entry, "x", voxel.position[0] as f64)?;
        set_f64(&entry, "y", voxel.position[1] as f64)?;
        set_f64(&entry, "z", voxel.position[2] as f64)?;
        set_f64(&entry, "count", voxel.count as f64)?;
        set_f64(&entry, "downloads", voxel.downloads as f64)?;
        set_f64(&entry, "dependencyCount", voxel.dependency_count as f64)?;
        set_f64(&entry, "dependentCount", voxel.dependent_count as f64)?;
        Reflect::set(&entry, &JsValue::from_str("color"), &color_to_js(voxel.color))?;
        voxels.push(&entry);
    }

    set_f64(&result, "span", minimap.span as f64)?;
    set_f64(&result, "grid", GLOBAL_MINIMAP_GRID as f64)?;
    Reflect::set(&result, &JsValue::from_str("voxels"), &voxels)?;
    Ok(result)
}

fn scene_node_to_js(index: usize, node: &SceneNode) -> Result<JsValue, JsValue> {
    let entry = Object::new();
    set_f64(&entry, "index", index as f64)?;
    Reflect::set(
        &entry,
        &JsValue::from_str("name"),
        &JsValue::from_str(&node.name),
    )?;
    Reflect::set(
        &entry,
        &JsValue::from_str("role"),
        &JsValue::from_str(node.role.as_str()),
    )?;
    set_f64(&entry, "x", node.position[0] as f64)?;
    set_f64(&entry, "y", node.position[1] as f64)?;
    set_f64(&entry, "radius", node.radius as f64)?;
    Reflect::set(&entry, &JsValue::from_str("color"), &color_to_js(node.fill))?;
    Reflect::set(&entry, &JsValue::from_str("accent"), &color_to_js(node.accent))?;
    set_f64(&entry, "downloads", node.downloads as f64)?;
    set_f64(&entry, "dependencyCount", node.dependency_count as f64)?;
    Ok(entry.into())
}

fn set_f64(target: &Object, key: &str, value: f64) -> Result<(), JsValue> {
    Reflect::set(target, &JsValue::from_str(key), &JsValue::from_f64(value))?;
    Ok(())
}

fn set_number(target: &Object, key: &str, value: Option<f64>) -> Result<(), JsValue> {
    let js_value = value.map(JsValue::from_f64).unwrap_or(JsValue::NULL);
    Reflect::set(target, &JsValue::from_str(key), &js_value)?;
    Ok(())
}

fn color_to_js(color: [f32; 4]) -> Array {
    let array = Array::new();
    for value in color {
        array.push(&JsValue::from_f64(value as f64));
    }
    array
}

fn scaled_radius(downloads: u64, min: f32, max: f32, factor: f32) -> f32 {
    (min + ((downloads + 1) as f32).log10() * factor).clamp(min, max)
}

fn polar(angle: f32, orbit: f32, viewport_aspect: f32) -> [f32; 2] {
    [angle.cos() * orbit * viewport_aspect, angle.sin() * orbit]
}

fn mix_color(left: [f32; 4], right: [f32; 4], t: f32) -> [f32; 4] {
    let clamped = t.clamp(0.0, 1.0);
    [
        left[0] + (right[0] - left[0]) * clamped,
        left[1] + (right[1] - left[1]) * clamped,
        left[2] + (right[2] - left[2]) * clamped,
        left[3] + (right[3] - left[3]) * clamped,
    ]
}

fn kind_palette(kind: &str) -> ([f32; 4], [f32; 4]) {
    match kind {
        "normal" => ([0.14, 0.78, 0.79, 0.92], [0.62, 0.95, 0.94, 0.98]),
        "build" => ([0.96, 0.71, 0.18, 0.9], [1.0, 0.90, 0.58, 0.98]),
        "dev" => ([0.95, 0.34, 0.58, 0.88], [1.0, 0.76, 0.84, 0.98]),
        _ => ([0.68, 0.82, 0.92, 0.82], [0.88, 0.96, 0.98, 0.96]),
    }
}

fn node_glow(role: NodeRole) -> f32 {
    match role {
        NodeRole::Focal => 0.44,
        NodeRole::DirectDependency => 0.3,
        NodeRole::SecondaryDependency => 0.2,
        NodeRole::Dependent => 0.16,
    }
}

fn node_ring_width(role: NodeRole) -> f32 {
    match role {
        NodeRole::Focal => 0.14,
        NodeRole::DirectDependency => 0.12,
        NodeRole::SecondaryDependency => 0.08,
        NodeRole::Dependent => 0.06,
    }
}

fn create_vertex_buffer<T: Pod>(
    device: &wgpu::Device,
    label: &str,
    vertices: &[T],
) -> Option<wgpu::Buffer> {
    if vertices.is_empty() {
        return None;
    }

    Some(device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: bytemuck::cast_slice(vertices),
        usage: wgpu::BufferUsages::VERTEX,
    }))
}

fn create_edge_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    camera_layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("gcrates-edge-shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(EDGE_SHADER)),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("gcrates-edge-layout"),
        bind_group_layouts: &[camera_layout],
        immediate_size: 0,
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("gcrates-edge-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[UnitVertex::layout(), EdgeInstance::layout()],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn create_node_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    camera_layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("gcrates-node-shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(NODE_SHADER)),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("gcrates-node-layout"),
        bind_group_layouts: &[camera_layout],
        immediate_size: 0,
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("gcrates-node-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[UnitVertex::layout(), NodeInstance::layout()],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

const EDGE_SHADER: &str = r#"
struct Camera {
    offset: vec2<f32>,
    zoom: f32,
    _padding: f32,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

struct VertexInput {
    @location(0) local: vec2<f32>,
    @location(1) start: vec2<f32>,
    @location(2) end: vec2<f32>,
    @location(3) color: vec4<f32>,
    @location(4) params: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) emphasis: f32,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    let delta = input.end - input.start;
    let length_value = max(length(delta), 0.0001);
    let tangent = delta / length_value;
    let normal = vec2<f32>(-tangent.y, tangent.x);
    let point = mix(input.start, input.end, input.local.x) + normal * input.local.y * input.params.x;

    var output: VertexOutput;
    output.position = vec4<f32>((point + camera.offset) * camera.zoom, 0.0, 1.0);
    output.uv = input.local;
    output.color = input.color;
    output.emphasis = input.params.y;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let edge_alpha = smoothstep(1.0, 0.14, abs(input.uv.y));
    let along_fade = smoothstep(0.0, 0.04, input.uv.x) * smoothstep(0.0, 0.04, 1.0 - input.uv.x);
    let alpha = input.color.a * edge_alpha * along_fade;
    let tint = mix(input.color.rgb, vec3<f32>(1.0, 0.96, 0.84), input.emphasis * 0.34);
    if alpha < 0.01 {
        discard;
    }
    return vec4<f32>(tint, alpha + input.emphasis * 0.08);
}
"#;

const NODE_SHADER: &str = r#"
struct Camera {
    offset: vec2<f32>,
    zoom: f32,
    _padding: f32,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

struct VertexInput {
    @location(0) local: vec2<f32>,
    @location(1) center: vec2<f32>,
    @location(2) radii: vec2<f32>,
    @location(3) fill: vec4<f32>,
    @location(4) accent: vec4<f32>,
    @location(5) params: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) fill: vec4<f32>,
    @location(2) accent: vec4<f32>,
    @location(3) params: vec4<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    let point = input.center + input.local * input.radii;
    output.position = vec4<f32>((point + camera.offset) * camera.zoom, 0.0, 1.0);
    output.local = input.local;
    output.fill = input.fill;
    output.accent = input.accent;
    output.params = input.params;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let distance_value = length(input.local);
    let disc = smoothstep(1.0, 0.78, distance_value);
    let ring_outer = smoothstep(1.0, 0.82, distance_value);
    let ring_inner = smoothstep(0.84, 0.58, distance_value);
    let ring = max(ring_outer - ring_inner, 0.0);
    let core = smoothstep(0.54, 0.0, distance_value);
    let glow = smoothstep(1.42, 0.56, distance_value) * input.params.x;
    let highlight = input.params.z;
    let fill_color = mix(input.fill.rgb, input.accent.rgb, ring * 0.52 + core * 0.18 + highlight * 0.12);
    let alpha = input.fill.a * disc + glow * 0.22 + ring * input.params.y * 0.36;
    if alpha < 0.01 {
        discard;
    }
    return vec4<f32>(fill_color + input.accent.rgb * glow * 0.32, alpha);
}
"#;

fn window() -> Result<Window, JsValue> {
    web_sys::window().ok_or_else(|| JsValue::from_str("window is not available"))
}

fn document() -> Result<Document, JsValue> {
    window()?
        .document()
        .ok_or_else(|| JsValue::from_str("document is not available"))
}

fn canvas_by_id(canvas_id: &str) -> Result<HtmlCanvasElement, JsValue> {
    let element = document()?
        .get_element_by_id(canvas_id)
        .ok_or_else(|| JsValue::from_str("canvas element was not found"))?;
    element
        .dyn_into::<HtmlCanvasElement>()
        .map_err(|_| JsValue::from_str("target element is not an HTML canvas"))
}

fn sync_canvas_size(canvas: &HtmlCanvasElement) {
    let scale = window().map(|handle| handle.device_pixel_ratio()).unwrap_or(1.0);
    let width = ((canvas.client_width().max(1) as f64) * scale).round().max(1.0) as u32;
    let height = ((canvas.client_height().max(1) as f64) * scale).round().max(1.0) as u32;
    if canvas.width() != width {
        canvas.set_width(width);
    }
    if canvas.height() != height {
        canvas.set_height(height);
    }
}

fn js_error(error: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&error.to_string())
}
