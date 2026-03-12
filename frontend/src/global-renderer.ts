import type { GlobalMinimap, GlobalOverviewNode, GlobalOverviewScene } from 'gcrates'

export type MacroCameraState = {
  focusX: number
  focusY: number
  focusZ: number
  zoom: number
  rotX: number
  rotY: number
  rotZ: number
}

export type ProjectedSceneNode = {
  index: number
  node: GlobalOverviewNode
  x: number
  y: number
  radius: number
  depth: number
  rotatedZ: number
  isProxy?: boolean
}

type ProjectedScenePlacement = {
  x: number
  y: number
  depth: number
  rotatedZ: number
  visible: boolean
  inFront: boolean
}

export type OverlayPaintOptions = {
  hoverAnchor?: string | null
  selectedAnchor?: string | null
}

type GPUBuffer = any
type GPURenderPipeline = any
type GPUBindGroup = any
type GPUCanvasContext = any
type GPUDevice = any
type GPUTextureFormat = any
type GPUBindGroupLayout = any

const GPUBufferUsageRef = (globalThis as any).GPUBufferUsage
const GPUShaderStageRef = (globalThis as any).GPUShaderStage
const GPUColorWriteRef = (globalThis as any).GPUColorWrite

const NODE_QUAD = new Float32Array([
  -1, -1,
  -1, 1,
  1, -1,
  1, -1,
  -1, 1,
  1, 1,
])

const DEFAULT_CAMERA: MacroCameraState = {
  focusX: 0,
  focusY: 0,
  focusZ: 0,
  zoom: 1,
  rotX: 0.54,
  rotY: -0.52,
  rotZ: 0,
}

const CAMERA_FOV_DEGREES = 58

export class GlobalGraphRenderer {
  static async create(canvas: HTMLCanvasElement) {
    const gpu = (navigator as { gpu?: any }).gpu
    if (!gpu) {
      throw new Error('WebGPU is not available in this browser')
    }

    const adapter = await gpu.requestAdapter({ powerPreference: 'high-performance' })
    if (!adapter) {
      throw new Error('Unable to acquire a WebGPU adapter')
    }

    const device = await adapter.requestDevice()
    const context = canvas.getContext('webgpu')
    if (!context) {
      throw new Error('Unable to acquire a WebGPU context')
    }

    const format = gpu.getPreferredCanvasFormat()
    const cameraBuffer = device.createBuffer({
      label: 'macro-camera-buffer',
      size: 96,
      usage: GPUBufferUsageRef.UNIFORM | GPUBufferUsageRef.COPY_DST,
    })
    const cameraLayout = device.createBindGroupLayout({
      label: 'macro-camera-layout',
      entries: [
        {
          binding: 0,
          visibility: GPUShaderStageRef.VERTEX,
          buffer: { type: 'uniform' },
        },
      ],
    })
    const cameraBindGroup = device.createBindGroup({
      label: 'macro-camera-bind-group',
      layout: cameraLayout,
      entries: [{ binding: 0, resource: { buffer: cameraBuffer } }],
    })

    const nodeQuadBuffer = device.createBuffer({
      label: 'macro-node-quad',
      size: NODE_QUAD.byteLength,
      usage: GPUBufferUsageRef.VERTEX | GPUBufferUsageRef.COPY_DST,
      mappedAtCreation: true,
    })
    new Float32Array(nodeQuadBuffer.getMappedRange()).set(NODE_QUAD)
    nodeQuadBuffer.unmap()

    const nodePipeline = createNodePipeline(device, format, cameraLayout)
    const edgePipeline = createEdgePipeline(device, format, cameraLayout)

    const renderer = new GlobalGraphRenderer(
      canvas,
      context,
      device,
      format,
      cameraBuffer,
      cameraBindGroup,
      nodeQuadBuffer,
      nodePipeline,
      edgePipeline,
    )
    renderer.resize()
    renderer.setCamera(DEFAULT_CAMERA)
    return renderer
  }

  private sourceScene: GlobalOverviewScene | null = null
  private transformedScene: GlobalOverviewScene | null = null
  private camera: MacroCameraState = DEFAULT_CAMERA
  private readonly nodeQuadBuffer: GPUBuffer
  private readonly nodePipeline: GPURenderPipeline
  private readonly edgePipeline: GPURenderPipeline
  private readonly cameraBuffer: GPUBuffer
  private readonly cameraBindGroup: GPUBindGroup
  private readonly context: GPUCanvasContext
  private readonly device: GPUDevice
  private readonly canvas: HTMLCanvasElement
  private readonly format: GPUTextureFormat
  private configuredWidth = 0
  private configuredHeight = 0
  private nodeInstanceBuffer: GPUBuffer | null = null
  private nodeInstanceCount = 0
  private edgeBuffer: GPUBuffer | null = null
  private edgeVertexCount = 0
  private screenNodes: ProjectedSceneNode[] = []
  private viewProjection = identityMatrix()

  private constructor(
    canvas: HTMLCanvasElement,
    context: GPUCanvasContext,
    device: GPUDevice,
    format: GPUTextureFormat,
    cameraBuffer: GPUBuffer,
    cameraBindGroup: GPUBindGroup,
    nodeQuadBuffer: GPUBuffer,
    nodePipeline: GPURenderPipeline,
    edgePipeline: GPURenderPipeline,
  ) {
    this.canvas = canvas
    this.context = context
    this.device = device
    this.format = format
    this.cameraBuffer = cameraBuffer
    this.cameraBindGroup = cameraBindGroup
    this.nodeQuadBuffer = nodeQuadBuffer
    this.nodePipeline = nodePipeline
    this.edgePipeline = edgePipeline
  }

  resize() {
    const dpr = window.devicePixelRatio || 1
    const width = Math.max(Math.round(this.canvas.clientWidth * dpr), 1)
    const height = Math.max(Math.round(this.canvas.clientHeight * dpr), 1)
    let needsConfigure = false
    if (this.canvas.width !== width) {
      this.canvas.width = width
      needsConfigure = true
    }
    if (this.canvas.height !== height) {
      this.canvas.height = height
      needsConfigure = true
    }
    if (!needsConfigure && this.configuredWidth === width && this.configuredHeight === height) {
      return
    }

    this.context.configure({
      device: this.device,
      format: this.format,
      alphaMode: 'opaque',
    })
    this.configuredWidth = width
    this.configuredHeight = height
  }

  setScene(scene: GlobalOverviewScene) {
    this.sourceScene = scene
    this.rebuildTransformedScene()
  }

  setCamera(camera: MacroCameraState) {
    this.camera = camera
    this.resize()
    this.rebuildTransformedScene()
  }

  render() {
    this.resize()
    this.updateCameraUniform()
    const texture = this.context.getCurrentTexture()
    const view = texture.createView()
    const encoder = this.device.createCommandEncoder({ label: 'macro-render-encoder' })
    const pass = encoder.beginRenderPass({
      label: 'macro-render-pass',
      colorAttachments: [
        {
          view,
          clearValue: { r: 0.015, g: 0.028, b: 0.041, a: 1 },
          loadOp: 'clear',
          storeOp: 'store',
        },
      ],
    })

    if (this.edgeBuffer && this.edgeVertexCount > 0) {
      pass.setPipeline(this.edgePipeline)
      pass.setBindGroup(0, this.cameraBindGroup)
      pass.setVertexBuffer(0, this.edgeBuffer)
      pass.draw(this.edgeVertexCount)
    }

    if (this.nodeInstanceBuffer && this.nodeInstanceCount > 0) {
      pass.setPipeline(this.nodePipeline)
      pass.setBindGroup(0, this.cameraBindGroup)
      pass.setVertexBuffer(0, this.nodeQuadBuffer)
      pass.setVertexBuffer(1, this.nodeInstanceBuffer)
      pass.draw(6, this.nodeInstanceCount)
    }

    pass.end()
    this.device.queue.submit([encoder.finish()])
  }

  pickNode(x: number, y: number) {
    return pickProjectedNode(this.screenNodes, x, y)?.node ?? null
  }

  paintOverlay(canvas: HTMLCanvasElement, options: OverlayPaintOptions = {}) {
    return paintProjectedScene(canvas, this.sourceScene, this.camera, options)
  }

  private rebuildTransformedScene() {
    this.transformedScene = this.sourceScene ? transformScene(this.sourceScene, this.camera) : null
    const width = this.canvas.clientWidth || 1
    const height = this.canvas.clientHeight || 1
    const placements = this.sourceScene
      ? projectScenePlacements(
          this.sourceScene,
          this.camera,
          width,
          height,
        )
      : []
    this.screenNodes = this.sourceScene
      ? buildInteractiveProjectedNodes(this.sourceScene, placements, width, height)
      : []
    this.uploadScene(placements)
    this.updateCameraUniform()
  }

  private uploadScene(placements: ProjectedScenePlacement[] = []) {
    if (!this.transformedScene) {
      this.nodeInstanceBuffer = null
      this.nodeInstanceCount = 0
      this.edgeBuffer = null
      this.edgeVertexCount = 0
      return
    }

    const nodeData = new Float32Array(this.transformedScene.nodes.length * 16)
    this.transformedScene.nodes.forEach((node, index) => {
      const base = index * 16
      const gpuSize =
        node.kind === 'cluster'
          ? Math.max(node.size * 1.02, 0.02)
          : Math.max(node.size * 0.94, 0.016)
      nodeData.set([node.x, node.y, node.z, gpuSize], base)
      nodeData.set(node.color, base + 4)
      nodeData.set(node.accent, base + 8)
      nodeData.set([
        node.kind === 'cluster' ? 1 : 0,
        Math.max(node.count, 1),
        Math.max(node.downloads, 1),
        Math.max(node.dependencyCount, 0),
      ], base + 12)
    })

    this.nodeInstanceBuffer = this.device.createBuffer({
      label: 'macro-node-instance-buffer',
      size: nodeData.byteLength,
      usage: GPUBufferUsageRef.VERTEX | GPUBufferUsageRef.COPY_DST,
    })
    this.device.queue.writeBuffer(this.nodeInstanceBuffer, 0, nodeData)
    this.nodeInstanceCount = this.transformedScene.nodes.length

    const visibleEdges = this.transformedScene.edges.filter((edge) => {
      const fromPlacement = placements[edge.from]
      const toPlacement = placements[edge.to]
      if (!fromPlacement || !toPlacement) {
        return true
      }
      if (!fromPlacement.inFront && !toPlacement.inFront) {
        return false
      }
      return fromPlacement.visible || toPlacement.visible
    })

    const edgeData = new Float32Array(visibleEdges.length * 14)
    visibleEdges.forEach((edge, index) => {
      const base = index * 14
      edgeData.set([edge.x1, edge.y1, edge.z1], base)
      edgeData.set(edge.color, base + 3)
      edgeData.set([edge.x2, edge.y2, edge.z2], base + 7)
      edgeData.set(edge.color, base + 10)
    })
    this.edgeBuffer = this.device.createBuffer({
      label: 'macro-edge-buffer',
      size: edgeData.byteLength,
      usage: GPUBufferUsageRef.VERTEX | GPUBufferUsageRef.COPY_DST,
    })
    this.device.queue.writeBuffer(this.edgeBuffer, 0, edgeData)
    this.edgeVertexCount = visibleEdges.length * 2
  }

  private updateCameraUniform() {
    const aspect = this.canvas.width / Math.max(this.canvas.height, 1)
    const distance = cameraDistanceForZoom(this.camera.zoom)
    const view = lookAt([0, 0, distance], [0, 0, 0], [0, 1, 0])
    const projection = perspective((Math.PI / 180) * CAMERA_FOV_DEGREES, aspect, 0.1, 36)
    this.viewProjection = multiplyMatrix(projection, view)

    const uniform = new Float32Array(24)
    uniform.set(this.viewProjection, 0)
    uniform.set([1, 0, 0, 0], 16)
    uniform.set([0, 1, 0, 0], 20)
    this.device.queue.writeBuffer(this.cameraBuffer, 0, uniform)
  }
}

function createNodePipeline(device: GPUDevice, format: GPUTextureFormat, cameraLayout: GPUBindGroupLayout) {
  const shader = device.createShaderModule({
    label: 'macro-node-shader',
    code: `
struct Camera {
  view_proj: mat4x4<f32>,
  right: vec4<f32>,
  up: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

struct VertexInput {
  @location(0) local: vec2<f32>,
  @location(1) center_size: vec4<f32>,
  @location(2) color: vec4<f32>,
  @location(3) accent: vec4<f32>,
  @location(4) meta: vec4<f32>,
};

struct VertexOutput {
  @builtin(position) position: vec4<f32>,
  @location(0) local: vec2<f32>,
  @location(1) color: vec4<f32>,
  @location(2) accent: vec4<f32>,
  @location(3) kind: f32,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
  var output: VertexOutput;
  let world = input.center_size.xyz + camera.right.xyz * input.local.x * input.center_size.w + camera.up.xyz * input.local.y * input.center_size.w;
  output.position = camera.view_proj * vec4<f32>(world, 1.0);
  output.local = input.local;
  output.color = input.color;
  output.accent = input.accent;
  output.kind = input.meta.x;
  return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
  let dist2 = dot(input.local, input.local);
  if (dist2 > 1.0) {
    discard;
  }
  let z = sqrt(max(1.0 - dist2, 0.0));
  let normal = normalize(vec3<f32>(input.local.x, -input.local.y, z));
  let light_dir = normalize(vec3<f32>(-0.46, 0.62, 0.74));
  let diffuse = max(dot(normal, light_dir), 0.0);
  let ambient = 0.24 + input.kind * 0.08;
  let rim = pow(1.0 - max(normal.z, 0.0), 2.6);
  let spec = pow(max(dot(reflect(-light_dir, normal), vec3<f32>(0.0, 0.0, 1.0)), 0.0), 18.0);
  let sphere = input.color.rgb * (ambient + diffuse * 0.84)
    + input.accent.rgb * (rim * 0.32 + spec * 0.58)
    + vec3<f32>(1.0, 1.0, 1.0) * spec * 0.16;
  let edge = smoothstep(1.0, 0.84, sqrt(dist2));
  let alpha = input.color.a * edge + rim * 0.08;
  return vec4<f32>(sphere, alpha);
}
`,
  })

  const layout = device.createPipelineLayout({
    label: 'macro-node-layout',
    bindGroupLayouts: [cameraLayout],
  })

  return device.createRenderPipeline({
    label: 'macro-node-pipeline',
    layout,
    vertex: {
      module: shader,
      entryPoint: 'vs_main',
      buffers: [
        {
          arrayStride: 8,
          stepMode: 'vertex',
          attributes: [{ shaderLocation: 0, offset: 0, format: 'float32x2' }],
        },
        {
          arrayStride: 64,
          stepMode: 'instance',
          attributes: [
            { shaderLocation: 1, offset: 0, format: 'float32x4' },
            { shaderLocation: 2, offset: 16, format: 'float32x4' },
            { shaderLocation: 3, offset: 32, format: 'float32x4' },
            { shaderLocation: 4, offset: 48, format: 'float32x4' },
          ],
        },
      ],
    },
    fragment: {
      module: shader,
      entryPoint: 'fs_main',
      targets: [
        {
          format,
          blend: {
            color: { srcFactor: 'src-alpha', dstFactor: 'one-minus-src-alpha', operation: 'add' },
            alpha: { srcFactor: 'one', dstFactor: 'one-minus-src-alpha', operation: 'add' },
          },
          writeMask: GPUColorWriteRef.ALL,
        },
      ],
    },
    primitive: { topology: 'triangle-list' },
    multisample: { count: 1 },
  })
}

function createEdgePipeline(device: GPUDevice, format: GPUTextureFormat, cameraLayout: GPUBindGroupLayout) {
  const shader = device.createShaderModule({
    label: 'macro-edge-shader',
    code: `
struct Camera {
  view_proj: mat4x4<f32>,
  right: vec4<f32>,
  up: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

struct VertexInput {
  @location(0) position: vec3<f32>,
  @location(1) color: vec4<f32>,
};

struct VertexOutput {
  @builtin(position) position: vec4<f32>,
  @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
  var output: VertexOutput;
  output.position = camera.view_proj * vec4<f32>(input.position, 1.0);
  output.color = input.color;
  return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
  return input.color;
}
`,
  })

  const layout = device.createPipelineLayout({
    label: 'macro-edge-layout',
    bindGroupLayouts: [cameraLayout],
  })

  return device.createRenderPipeline({
    label: 'macro-edge-pipeline',
    layout,
    vertex: {
      module: shader,
      entryPoint: 'vs_main',
      buffers: [
        {
          arrayStride: 28,
          stepMode: 'vertex',
          attributes: [
            { shaderLocation: 0, offset: 0, format: 'float32x3' },
            { shaderLocation: 1, offset: 12, format: 'float32x4' },
          ],
        },
      ],
    },
    fragment: {
      module: shader,
      entryPoint: 'fs_main',
      targets: [
        {
          format,
          blend: {
            color: { srcFactor: 'src-alpha', dstFactor: 'one', operation: 'add' },
            alpha: { srcFactor: 'one', dstFactor: 'one-minus-src-alpha', operation: 'add' },
          },
          writeMask: GPUColorWriteRef.ALL,
        },
      ],
    },
    primitive: { topology: 'line-list' },
    multisample: { count: 1 },
  })
}

function identityMatrix() {
  return new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1])
}

function perspective(fovY: number, aspect: number, near: number, far: number) {
  const f = 1 / Math.tan(fovY / 2)
  const nf = 1 / (near - far)
  return new Float32Array([
    f / aspect, 0, 0, 0,
    0, f, 0, 0,
    0, 0, (far + near) * nf, -1,
    0, 0, (2 * far * near) * nf, 0,
  ])
}

function lookAt(eye: [number, number, number], target: [number, number, number], up: [number, number, number]) {
  const zAxis = normalize3(subtract3(eye, target))
  const xAxis = normalize3(cross3(up, zAxis))
  const yAxis = cross3(zAxis, xAxis)

  return new Float32Array([
    xAxis[0], yAxis[0], zAxis[0], 0,
    xAxis[1], yAxis[1], zAxis[1], 0,
    xAxis[2], yAxis[2], zAxis[2], 0,
    -dot3(xAxis, eye), -dot3(yAxis, eye), -dot3(zAxis, eye), 1,
  ])
}

function multiplyMatrix(a: Float32Array, b: Float32Array) {
  const out = new Float32Array(16)
  for (let col = 0; col < 4; col += 1) {
    for (let row = 0; row < 4; row += 1) {
      out[col * 4 + row] =
        a[row] * b[col * 4] +
        a[4 + row] * b[col * 4 + 1] +
        a[8 + row] * b[col * 4 + 2] +
        a[12 + row] * b[col * 4 + 3]
    }
  }
  return out
}

function projectPoint(matrix: Float32Array, point: [number, number, number]) {
  const x = point[0]
  const y = point[1]
  const z = point[2]
  const clipX = matrix[0] * x + matrix[4] * y + matrix[8] * z + matrix[12]
  const clipY = matrix[1] * x + matrix[5] * y + matrix[9] * z + matrix[13]
  const clipZ = matrix[2] * x + matrix[6] * y + matrix[10] * z + matrix[14]
  const clipW = matrix[3] * x + matrix[7] * y + matrix[11] * z + matrix[15]
  if (clipW <= 0) {
    return { x: 0, y: 0, depth: 0, visible: false, inFront: false }
  }
  return {
    x: clipX / clipW,
    y: clipY / clipW,
    depth: clipZ / clipW,
    visible: Math.abs(clipX) <= clipW * 1.2 && Math.abs(clipY) <= clipW * 1.2,
    inFront: clipW > 0.02,
  }
}

function projectScenePlacements(
  scene: GlobalOverviewScene,
  camera: MacroCameraState,
  width: number,
  height: number,
) {
  const aspect = width / Math.max(height, 1)
  const distance = cameraDistanceForZoom(camera.zoom)
  const viewProjection = multiplyMatrix(
    perspective((Math.PI / 180) * CAMERA_FOV_DEGREES, aspect, 0.1, 36),
    lookAt([0, 0, distance], [0, 0, 0], [0, 1, 0]),
  )

  const placements: ProjectedScenePlacement[] = new Array(scene.nodes.length)
  for (const [index, node] of scene.nodes.entries()) {
    const rotated = displayWorldPoint(node, camera)
    const center = projectPoint(viewProjection, rotated)
    placements[index] = {
      x: (center.x * 0.5 + 0.5) * width,
      y: (0.5 - center.y * 0.5) * height,
      depth: center.depth,
      rotatedZ: rotated[2],
      visible: center.visible,
      inFront: center.inFront,
    }
  }
  return placements
}

function projectedNodesFromPlacements(
  scene: GlobalOverviewScene,
  placements: ProjectedScenePlacement[],
) {
  const projected: ProjectedSceneNode[] = []
  for (const [index, node] of scene.nodes.entries()) {
    const placement = placements[index]
    if (!placement?.visible) {
      continue
    }
    projected.push({
      index,
      node,
      x: placement.x,
      y: placement.y,
      radius: styleRadiusPx(node, placement.rotatedZ),
      depth: placement.depth,
      rotatedZ: placement.rotatedZ,
      isProxy: false,
    })
  }
  projected.sort((left, right) => left.depth - right.depth)
  return projected
}

function collectOffscreenProxyNodes(
  scene: GlobalOverviewScene,
  placements: ProjectedScenePlacement[],
  width: number,
  height: number,
) {
  const offscreenMarkers = new Map<
    number,
    { x: number; y: number; node: GlobalOverviewNode; count: number; depth: number; rotatedZ: number }
  >()

  for (const edge of scene.edges) {
    const fromPlacement = placements[edge.from]
    const toPlacement = placements[edge.to]
    if (!fromPlacement || !toPlacement) {
      continue
    }
    if (!fromPlacement.inFront && !toPlacement.inFront) {
      continue
    }
    if (!fromPlacement.visible && !toPlacement.visible) {
      continue
    }

    const clipped = clipLineToRect(
      fromPlacement.x,
      fromPlacement.y,
      toPlacement.x,
      toPlacement.y,
      0,
      0,
      width,
      height,
    )
    if (!clipped) {
      continue
    }

    if (!fromPlacement.visible && toPlacement.visible && scene.nodes[edge.from]) {
      const marker = offscreenMarkers.get(edge.from)
      if (marker) {
        marker.x = (marker.x * marker.count + clipped[0]) / (marker.count + 1)
        marker.y = (marker.y * marker.count + clipped[1]) / (marker.count + 1)
        marker.count += 1
      } else {
        offscreenMarkers.set(edge.from, {
          x: clipped[0],
          y: clipped[1],
          node: scene.nodes[edge.from],
          count: 1,
          depth: fromPlacement.depth,
          rotatedZ: fromPlacement.rotatedZ,
        })
      }
    }

    if (!toPlacement.visible && fromPlacement.visible && scene.nodes[edge.to]) {
      const marker = offscreenMarkers.get(edge.to)
      if (marker) {
        marker.x = (marker.x * marker.count + clipped[2]) / (marker.count + 1)
        marker.y = (marker.y * marker.count + clipped[3]) / (marker.count + 1)
        marker.count += 1
      } else {
        offscreenMarkers.set(edge.to, {
          x: clipped[2],
          y: clipped[3],
          node: scene.nodes[edge.to],
          count: 1,
          depth: toPlacement.depth,
          rotatedZ: toPlacement.rotatedZ,
        })
      }
    }
  }

  return Array.from(offscreenMarkers.entries())
    .map(([index, marker]) => ({
      index,
      node: marker.node,
      x: marker.x,
      y: marker.y,
      radius: clamp(4.2 + Math.log2(marker.count + 1) * 1.1, 4.2, 8.5),
      depth: marker.depth,
      rotatedZ: marker.rotatedZ,
      isProxy: true,
    }))
    .sort((left, right) => left.depth - right.depth)
}

function buildInteractiveProjectedNodes(
  scene: GlobalOverviewScene,
  placements: ProjectedScenePlacement[],
  width: number,
  height: number,
) {
  const visibleNodes = projectedNodesFromPlacements(scene, placements)
  const proxyNodes = collectOffscreenProxyNodes(scene, placements, width, height)
  return [...visibleNodes, ...proxyNodes].sort((left, right) => left.depth - right.depth)
}

function subtract3(a: [number, number, number], b: [number, number, number]): [number, number, number] {
  return [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

function cross3(a: [number, number, number], b: [number, number, number]): [number, number, number] {
  return [
    a[1] * b[2] - a[2] * b[1],
    a[2] * b[0] - a[0] * b[2],
    a[0] * b[1] - a[1] * b[0],
  ]
}

function dot3(a: [number, number, number], b: [number, number, number]) {
  return a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

function normalize3(vector: [number, number, number]): [number, number, number] {
  const length = Math.hypot(vector[0], vector[1], vector[2]) || 1
  return [vector[0] / length, vector[1] / length, vector[2] / length]
}

export function projectSceneNodes(
  scene: GlobalOverviewScene,
  camera: MacroCameraState,
  width: number,
  height: number,
): ProjectedSceneNode[] {
  const placements = projectScenePlacements(scene, camera, width, height)
  return buildInteractiveProjectedNodes(scene, placements, width, height)
}

export function paintProjectedScene(
  canvas: HTMLCanvasElement,
  scene: GlobalOverviewScene | null,
  camera: MacroCameraState,
  options: OverlayPaintOptions = {},
) {
  const dpr = window.devicePixelRatio || 1
  const width = Math.max(Math.round(canvas.clientWidth * dpr), 1)
  const height = Math.max(Math.round(canvas.clientHeight * dpr), 1)
  if (canvas.width !== width) {
    canvas.width = width
  }
  if (canvas.height !== height) {
    canvas.height = height
  }

  const context = canvas.getContext('2d')
  if (!context) {
    return [] as ProjectedSceneNode[]
  }

  context.setTransform(dpr, 0, 0, dpr, 0, 0)
  context.clearRect(0, 0, canvas.clientWidth, canvas.clientHeight)

  if (!scene) {
    return [] as ProjectedSceneNode[]
  }

  const placements = projectScenePlacements(scene, camera, canvas.clientWidth || 1, canvas.clientHeight || 1)
  const projectedNodes = buildInteractiveProjectedNodes(
    scene,
    placements,
    canvas.clientWidth || 1,
    canvas.clientHeight || 1,
  )
  if (projectedNodes.length === 0) {
    return projectedNodes
  }
  const visibleNodes = projectedNodes.filter((node) => !node.isProxy)
  const visibleByIndex = new Map<number, ProjectedSceneNode>()
  for (const screenNode of visibleNodes) {
    visibleByIndex.set(screenNode.index, screenNode)
  }

  context.save()
  context.lineCap = 'round'
  context.lineJoin = 'round'

  for (const edge of scene.edges) {
    const fromPlacement = placements[edge.from]
    const toPlacement = placements[edge.to]
    if (!fromPlacement || !toPlacement) {
      continue
    }
    if (!fromPlacement.inFront && !toPlacement.inFront) {
      continue
    }
    if (!fromPlacement.visible && !toPlacement.visible) {
      continue
    }
    const clipped = clipLineToRect(
      fromPlacement.x,
      fromPlacement.y,
      toPlacement.x,
      toPlacement.y,
      0,
      0,
      canvas.clientWidth || 1,
      canvas.clientHeight || 1,
    )
    if (!clipped) {
      continue
    }
    const from = visibleByIndex.get(edge.from)
    const to = visibleByIndex.get(edge.to)

    const selectedMatch =
      options.selectedAnchor &&
      ((from?.node.anchorName ?? scene.nodes[edge.from]?.anchorName) === options.selectedAnchor ||
        (to?.node.anchorName ?? scene.nodes[edge.to]?.anchorName) === options.selectedAnchor)
    const baseWidth = selectedMatch ? 2.2 : Math.max(0.95, 0.8 + edge.weight * 0.22)
    context.strokeStyle = toCssColor(edge.color, selectedMatch ? 0.16 : edge.color[3] * 0.32)
    context.lineWidth = baseWidth + 2.1
    context.beginPath()
    context.moveTo(clipped[0], clipped[1])
    context.lineTo(clipped[2], clipped[3])
    context.stroke()

    context.strokeStyle = toCssColor(edge.color, selectedMatch ? 0.72 : edge.color[3] * 0.96)
    context.lineWidth = baseWidth
    context.beginPath()
    context.moveTo(clipped[0], clipped[1])
    context.lineTo(clipped[2], clipped[3])
    context.stroke()

  }

  for (const screenNode of visibleNodes) {
    const { node, x, y, radius } = screenNode
    const isSelected = options.selectedAnchor === node.anchorName
    const isHover = options.hoverAnchor === node.anchorName
    const glowRadius = radius * (node.kind === 'cluster' ? 2.3 : 1.95)
    const glow = context.createRadialGradient(x, y, radius * 0.18, x, y, glowRadius)
    glow.addColorStop(0, toCssColor(node.accent, isSelected ? 0.44 : isHover ? 0.32 : 0.18))
    glow.addColorStop(1, toCssColor(node.accent, 0))
    context.fillStyle = glow
    context.beginPath()
    context.arc(x, y, glowRadius, 0, Math.PI * 2)
    context.fill()

    const shadow = context.createRadialGradient(
      x + radius * 0.24,
      y + radius * 0.28,
      radius * 0.16,
      x,
      y,
      radius * 1.18,
    )
    shadow.addColorStop(0, 'rgba(0, 0, 0, 0)')
    shadow.addColorStop(1, 'rgba(0, 0, 0, 0.28)')
    context.fillStyle = shadow
    context.beginPath()
    context.arc(x, y, radius * 1.08, 0, Math.PI * 2)
    context.fill()

    const sphere = context.createRadialGradient(
      x - radius * 0.34,
      y - radius * 0.4,
      radius * 0.12,
      x,
      y,
      radius,
    )
    sphere.addColorStop(0, toCssColor(brightenColor(node.accent, isSelected ? 0.46 : 0.24), 0.96))
    sphere.addColorStop(0.34, toCssColor(node.accent, isSelected ? 0.92 : isHover ? 0.88 : 0.8))
    sphere.addColorStop(0.72, toCssColor(node.color, isSelected ? 0.96 : 0.92))
    sphere.addColorStop(1, toCssColor(dimColor(node.color, 0.38), isSelected ? 0.98 : 0.94))
    context.fillStyle = sphere
    context.beginPath()
    context.arc(x, y, radius, 0, Math.PI * 2)
    context.fill()

    context.strokeStyle = toCssColor(brightenColor(node.accent, 0.34), isSelected ? 0.38 : 0.24)
    context.lineWidth = Math.max(0.9, radius * 0.1)
    context.beginPath()
    context.arc(x - radius * 0.12, y - radius * 0.14, radius * 0.7, Math.PI * 1.08, Math.PI * 1.82)
    context.stroke()

    context.strokeStyle = toCssColor(dimColor(node.color, 0.12), isSelected ? 0.34 : 0.2)
    context.lineWidth = Math.max(0.8, radius * 0.08)
    context.beginPath()
    context.ellipse(x, y + radius * 0.18, radius * 0.72, radius * 0.24, 0, Math.PI * 0.12, Math.PI * 0.88)
    context.stroke()

    context.strokeStyle = toCssColor(node.accent, isSelected ? 1 : isHover ? 0.9 : 0.72)
    context.lineWidth = isSelected ? 2.4 : node.kind === 'cluster' ? 1.5 : 1.15
    context.beginPath()
    context.arc(x, y, radius + 1.5, 0, Math.PI * 2)
    context.stroke()
  }

  for (const marker of projectedNodes.filter((node) => node.isProxy)) {
    const radius = marker.radius
    const halo = context.createRadialGradient(marker.x, marker.y, radius * 0.3, marker.x, marker.y, radius * 1.9)
    halo.addColorStop(0, toCssColor(marker.node.accent, 0.28))
    halo.addColorStop(1, toCssColor(marker.node.accent, 0))
    context.fillStyle = halo
    context.beginPath()
    context.arc(marker.x, marker.y, radius * 1.9, 0, Math.PI * 2)
    context.fill()

    const sphere = context.createRadialGradient(
      marker.x - radius * 0.28,
      marker.y - radius * 0.32,
      radius * 0.1,
      marker.x,
      marker.y,
      radius,
    )
    sphere.addColorStop(0, toCssColor(brightenColor(marker.node.accent, 0.22), 0.92))
    sphere.addColorStop(0.72, toCssColor(marker.node.color, 0.86))
    sphere.addColorStop(1, toCssColor(dimColor(marker.node.color, 0.36), 0.92))
    context.fillStyle = sphere
    context.beginPath()
    context.arc(marker.x, marker.y, radius, 0, Math.PI * 2)
    context.fill()

    context.strokeStyle = toCssColor(marker.node.accent, options.selectedAnchor === marker.node.anchorName ? 0.92 : 0.68)
    context.lineWidth = options.selectedAnchor === marker.node.anchorName ? 2.1 : 1.2
    context.beginPath()
    context.arc(marker.x, marker.y, radius + 1.4, 0, Math.PI * 2)
    context.stroke()
  }

  drawLabels(context, visibleNodes, options)
  context.restore()
  return projectedNodes
}

export function pickProjectedNode(projectedNodes: ProjectedSceneNode[], x: number, y: number) {
  let best: ProjectedSceneNode | null = null
  let bestScore = Number.POSITIVE_INFINITY
  for (const screenNode of projectedNodes) {
    const dx = x - screenNode.x
    const dy = y - screenNode.y
    const score =
      (dx * dx + dy * dy) / Math.max(screenNode.radius * screenNode.radius, 1) +
      (screenNode.isProxy ? 0.035 : 0)
    if (score <= 1.2 && score < bestScore) {
      best = screenNode
      bestScore = score
    }
  }
  return best
}

function drawLabels(
  context: CanvasRenderingContext2D,
  projectedNodes: ProjectedSceneNode[],
  options: OverlayPaintOptions,
) {
  const candidates = [...projectedNodes].sort((left, right) => labelPriority(right, options) - labelPriority(left, options))

  context.textAlign = 'center'
  context.textBaseline = 'middle'
  for (const candidate of candidates.slice(0, 26)) {
    const isSelected = options.selectedAnchor === candidate.node.anchorName
    const isHover = options.hoverAnchor === candidate.node.anchorName
    if (!isSelected && !isHover && candidate.radius < 12) {
      continue
    }

    const label = truncateLabel(candidate.node.anchorName)
    const fontSize = clamp(candidate.radius * 0.42, 9, isSelected ? 15 : 12.5)
    context.font = `600 ${fontSize}px "IBM Plex Sans", "Segoe UI", sans-serif`
    context.lineWidth = fontSize * 0.36
    context.strokeStyle = isSelected ? 'rgba(4, 12, 18, 0.92)' : 'rgba(4, 12, 18, 0.76)'
    context.fillStyle = isSelected ? 'rgba(255, 244, 228, 0.98)' : 'rgba(234, 245, 255, 0.92)'
    context.strokeText(label, candidate.x, candidate.y)
    context.fillText(label, candidate.x, candidate.y)
  }
}

function clipLineToRect(
  x1: number,
  y1: number,
  x2: number,
  y2: number,
  minX: number,
  minY: number,
  maxX: number,
  maxY: number,
) {
  let t0 = 0
  let t1 = 1
  const dx = x2 - x1
  const dy = y2 - y1
  const tests: Array<[number, number]> = [
    [-dx, x1 - minX],
    [dx, maxX - x1],
    [-dy, y1 - minY],
    [dy, maxY - y1],
  ]

  for (const [p, q] of tests) {
    if (p === 0) {
      if (q < 0) {
        return null
      }
      continue
    }
    const ratio = q / p
    if (p < 0) {
      if (ratio > t1) {
        return null
      }
      if (ratio > t0) {
        t0 = ratio
      }
    } else if (ratio < t0) {
      return null
    } else if (ratio < t1) {
      t1 = ratio
    }
  }

  return [x1 + dx * t0, y1 + dy * t0, x1 + dx * t1, y1 + dy * t1] as const
}

export function paintMinimapVolume(
  canvas: HTMLCanvasElement,
  minimap: GlobalMinimap | null,
  camera: MacroCameraState,
) {
  const dpr = window.devicePixelRatio || 1
  const width = Math.max(Math.round(canvas.clientWidth * dpr), 1)
  const height = Math.max(Math.round(canvas.clientHeight * dpr), 1)
  if (canvas.width !== width) {
    canvas.width = width
  }
  if (canvas.height !== height) {
    canvas.height = height
  }

  const context = canvas.getContext('2d')
  if (!context) {
    return
  }

  context.setTransform(dpr, 0, 0, dpr, 0, 0)
  context.clearRect(0, 0, canvas.clientWidth, canvas.clientHeight)
  if (!minimap) {
    return
  }

  const viewProjection = multiplyMatrix(
    perspective((Math.PI / 180) * 34, (canvas.clientWidth || 1) / Math.max(canvas.clientHeight || 1, 1), 0.1, 52),
    lookAt([0, 0, 10.8], [0, 0, 0], [0, 1, 0]),
  )
  const pixels = minimap.voxels
    .map((voxel) => {
      const rotated = rotateRelativePoint([voxel.x, voxel.y, voxel.z], camera)
      const projected = projectPoint(viewProjection, rotated)
      if (!projected.inFront) {
        return null
      }
      return {
        x: (projected.x * 0.5 + 0.5) * (canvas.clientWidth || 1),
        y: (0.5 - projected.y * 0.5) * (canvas.clientHeight || 1),
        depth: projected.depth,
        size: clamp(1.4 + Math.log2(voxel.count + 1) * 0.65, 1.4, 4.6),
        color: voxel.color,
      }
    })
    .filter((entry): entry is NonNullable<typeof entry> => entry !== null)
    .sort((left, right) => left.depth - right.depth)

  const gradient = context.createLinearGradient(0, 0, 0, canvas.clientHeight || 1)
  gradient.addColorStop(0, 'rgba(7, 18, 27, 0.92)')
  gradient.addColorStop(1, 'rgba(3, 8, 13, 0.82)')
  context.fillStyle = gradient
  roundRect(context, 0, 0, canvas.clientWidth || 1, canvas.clientHeight || 1, 18)
  context.fill()

  for (const pixel of pixels) {
    context.fillStyle = toCssColor(pixel.color, clamp((pixel.color[3] ?? 0.9) * 0.92, 0.28, 0.96))
    context.fillRect(pixel.x - pixel.size * 0.5, pixel.y - pixel.size * 0.5, pixel.size, pixel.size)
  }

  const cubeRadius = cubeQueryRadius(camera.zoom, minimap.span)
  const corners = cubeCorners(
    [camera.focusX, camera.focusY, camera.focusZ],
    cubeRadius,
  ).map((corner) => {
    const rotated = rotateRelativePoint(corner, camera)
    const projected = projectPoint(viewProjection, rotated)
    return {
      x: (projected.x * 0.5 + 0.5) * (canvas.clientWidth || 1),
      y: (0.5 - projected.y * 0.5) * (canvas.clientHeight || 1),
      inFront: projected.inFront,
    }
  })

  const cubeEdges = [
    [0, 1], [0, 2], [0, 4],
    [1, 3], [1, 5],
    [2, 3], [2, 6],
    [3, 7],
    [4, 5], [4, 6],
    [5, 7],
    [6, 7],
  ] as const

  context.save()
  context.strokeStyle = 'rgba(255, 209, 156, 0.9)'
  context.lineWidth = 1.1
  for (const [fromIndex, toIndex] of cubeEdges) {
    const from = corners[fromIndex]
    const to = corners[toIndex]
    if (!from?.inFront || !to?.inFront) {
      continue
    }
    const clipped = clipLineToRect(
      from.x,
      from.y,
      to.x,
      to.y,
      6,
      6,
      (canvas.clientWidth || 1) - 6,
      (canvas.clientHeight || 1) - 6,
    )
    if (!clipped) {
      continue
    }
    context.beginPath()
    context.moveTo(clipped[0], clipped[1])
    context.lineTo(clipped[2], clipped[3])
    context.stroke()
  }
  context.restore()
}

function transformScene(scene: GlobalOverviewScene, camera: MacroCameraState): GlobalOverviewScene {
  const nodes = scene.nodes.map((node) => {
    const transformed = displayWorldPoint(node, camera)
    return {
      ...node,
      x: transformed[0],
      y: transformed[1],
      z: transformed[2],
    }
  })
  const edges = scene.edges.map((edge) => ({
    ...edge,
    x1: nodes[edge.from]?.x ?? edge.x1,
    y1: nodes[edge.from]?.y ?? edge.y1,
    z1: nodes[edge.from]?.z ?? edge.z1,
    x2: nodes[edge.to]?.x ?? edge.x2,
    y2: nodes[edge.to]?.y ?? edge.y2,
    z2: nodes[edge.to]?.z ?? edge.z2,
  }))
  return {
    ...scene,
    nodes,
    edges,
  }
}

function displayWorldPoint(node: GlobalOverviewNode, camera: MacroCameraState): [number, number, number] {
  const relative: [number, number, number] = [
    node.x - camera.focusX,
    node.y - camera.focusY,
    node.z - camera.focusZ,
  ]
  return rotateRelativePoint(warpPoint(relative, node), camera)
}

function rotateRelativePoint(point: [number, number, number], camera: MacroCameraState): [number, number, number] {
  let x = point[0]
  let y = point[1]
  let z = point[2]

  ;[x, y, z] = rotateX([x, y, z], camera.rotX)
  ;[x, y, z] = rotateY([x, y, z], camera.rotY)
  ;[x, y, z] = rotateZ([x, y, z], camera.rotZ)
  return [x, y, z]
}

function warpPoint(point: [number, number, number], node: GlobalOverviewNode): [number, number, number] {
  const hash = stableHash(node.anchorName)
  const warpScale = node.kind === 'cluster' ? 0.16 : 0.08
  const anisotropy: [number, number, number] = [1.1, 0.92, 1.2]
  const spread =
    (node.kind === 'cluster' ? 1.22 : 1.08) +
    Math.min(Math.log10(node.downloads + 10) * 0.04 + Math.log2(node.count + 1) * 0.04, 0.24)
  const turbulence: [number, number, number] = [
    Math.sin(point[1] * 2.7 + hashUnit(hash ^ 0x9e37) * Math.PI * 2) * 0.1,
    Math.cos(point[2] * 2.1 + hashUnit(hash ^ 0x7f4a) * Math.PI * 2) * 0.08,
    Math.sin(point[0] * 2.4 + hashUnit(hash ^ 0xa24b) * Math.PI * 2) * 0.12,
  ]
  return [
    (point[0] * anisotropy[0] + (hashUnit(hash ^ 0x5163) - 0.5) * warpScale + turbulence[0]) * spread,
    (point[1] * anisotropy[1] + (hashUnit(hash ^ 0x85eb) - 0.5) * warpScale + turbulence[1]) * spread,
    (point[2] * anisotropy[2] + (hashUnit(hash ^ 0xc2b2) - 0.5) * warpScale + turbulence[2]) * spread,
  ]
}

function rotateX(point: [number, number, number], angle: number): [number, number, number] {
  const c = Math.cos(angle)
  const s = Math.sin(angle)
  return [point[0], point[1] * c - point[2] * s, point[1] * s + point[2] * c]
}

function rotateY(point: [number, number, number], angle: number): [number, number, number] {
  const c = Math.cos(angle)
  const s = Math.sin(angle)
  return [point[0] * c + point[2] * s, point[1], -point[0] * s + point[2] * c]
}

function rotateZ(point: [number, number, number], angle: number): [number, number, number] {
  const c = Math.cos(angle)
  const s = Math.sin(angle)
  return [point[0] * c - point[1] * s, point[0] * s + point[1] * c, point[2]]
}

function styleRadiusPx(node: GlobalOverviewNode, rotatedZ: number) {
  const weight = Math.log10(node.downloads + 10)
  const breadth = Math.log10(node.count + 1)
  const dependency = Math.log2(node.dependencyCount + 2)
  const influence = Math.log2(node.dependentCount + 2)
  const sizeSignal = node.size * 160
  const base =
    node.kind === 'cluster'
      ? 4.6 + sizeSignal * 1.02 + breadth * 1.8 + weight * 0.56 + influence * 0.42
      : 3 + sizeSignal * 0.9 + weight * 0.62 + dependency * 0.38 + influence * 0.52
  const depthScale = clamp(1 + rotatedZ * 0.09, 0.82, 1.22)
  return clamp(
    base * depthScale,
    node.kind === 'cluster' ? 6.4 : 3.6,
    node.kind === 'cluster' ? 28 : 18,
  )
}

function labelPriority(candidate: ProjectedSceneNode, options: OverlayPaintOptions) {
  let score = candidate.radius + Math.log10(candidate.node.downloads + 10) * 2
  if (candidate.node.anchorName === options.selectedAnchor) {
    score += 120
  }
  if (candidate.node.anchorName === options.hoverAnchor) {
    score += 60
  }
  return score
}

function truncateLabel(label: string) {
  return label.length > 22 ? `${label.slice(0, 21)}…` : label
}

function boxesOverlap(a: { x1: number; y1: number; x2: number; y2: number }, b: { x1: number; y1: number; x2: number; y2: number }) {
  return !(a.x2 < b.x1 || b.x2 < a.x1 || a.y2 < b.y1 || b.y2 < a.y1)
}

function roundRect(
  context: CanvasRenderingContext2D,
  x: number,
  y: number,
  width: number,
  height: number,
  radius: number,
) {
  context.beginPath()
  context.moveTo(x + radius, y)
  context.lineTo(x + width - radius, y)
  context.quadraticCurveTo(x + width, y, x + width, y + radius)
  context.lineTo(x + width, y + height - radius)
  context.quadraticCurveTo(x + width, y + height, x + width - radius, y + height)
  context.lineTo(x + radius, y + height)
  context.quadraticCurveTo(x, y + height, x, y + height - radius)
  context.lineTo(x, y + radius)
  context.quadraticCurveTo(x, y, x + radius, y)
  context.closePath()
}

function toCssColor(color: number[], alpha = color[3] ?? 1) {
  const r = Math.round((color[0] ?? 0) * 255)
  const g = Math.round((color[1] ?? 0) * 255)
  const b = Math.round((color[2] ?? 0) * 255)
  return `rgba(${r}, ${g}, ${b}, ${clamp(alpha, 0, 1)})`
}

function brightenColor(color: number[], amount: number) {
  return [
    Math.min((color[0] ?? 0) + (1 - (color[0] ?? 0)) * amount, 1),
    Math.min((color[1] ?? 0) + (1 - (color[1] ?? 0)) * amount, 1),
    Math.min((color[2] ?? 0) + (1 - (color[2] ?? 0)) * amount, 1),
    color[3] ?? 1,
  ]
}

function dimColor(color: number[], amount: number) {
  return [
    Math.max((color[0] ?? 0) * (1 - amount), 0),
    Math.max((color[1] ?? 0) * (1 - amount), 0),
    Math.max((color[2] ?? 0) * (1 - amount), 0),
    color[3] ?? 1,
  ]
}

export function cubeQueryRadius(zoom: number, span = 6.4) {
  return clamp((span * 0.56) / Math.pow(clamp(zoom, 0.35, 28), 0.72), 0.44, span * 0.9)
}

function cubeCorners(center: [number, number, number], radius: number) {
  const [cx, cy, cz] = center
  const offsets = [-radius, radius]
  const corners: Array<[number, number, number]> = []
  for (const dx of offsets) {
    for (const dy of offsets) {
      for (const dz of offsets) {
        corners.push([cx + dx, cy + dy, cz + dz])
      }
    }
  }
  return corners
}

function cameraDistanceForZoom(zoom: number) {
  return 6.4 / Math.pow(clamp(zoom, 0.72, 18), 0.88) + 0.84
}

function stableHash(value: string) {
  let hash = 2166136261
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index)
    hash = Math.imul(hash, 16777619)
  }
  return hash >>> 0
}

function hashUnit(hash: number) {
  return hash / 0xffffffff
}

function clamp(value: number, min: number, max: number) {
  return Math.min(Math.max(value, min), max)
}
