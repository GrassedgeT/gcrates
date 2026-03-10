# GCrates: Crates.io Dependency Visualization Technical Document

## 1. Project Overview
GCrates is a high-performance, web-based visualization tool for the entire crates.io ecosystem. It aims to visualize the complex web of crate dependencies using a modern tech stack centered around Rust, WebAssembly (WASM), and WebGPU.

## 2. Technical Stack
- **Frontend UI:** Svelte 5 (Modern, fast, and reactive)
- **Rendering Engine:** Rust + `wgpu` (WebGPU abstraction)
- **Communication:** `wasm-bindgen` + `web-sys`
- **Data Processing:** Rust (Processing crates.io database dumps)
- **Build System:** Vite + `vite-plugin-wasm-pack`

## 3. Architecture Design

### 3.1 Data Pipeline (Offline/Server-side)
1. **Ingestion:** Download and extract the latest `db-dump.tar.gz` from crates.io.
2. **Parsing:** Use the `csv` crate to parse `crates.csv`, `versions.csv`, and `dependencies.csv`.
3. **Graph Construction:** Use `petgraph` to build a directed acyclic graph (DAG) of all crates and their dependencies.
4. **Optimization:**
   - Filter out older versions (keep only the latest or stable versions).
   - Compress the graph into a custom binary format (e.g., MessagePack or a specialized flat buffer) for efficient loading in the browser.

### 3.2 Frontend Architecture
- **Svelte Layer:** Manages the search interface, crate details sidebar, filtering options (e.g., dev-dependencies, target-specific), and general UI state.
- **WASM Layer:** A dedicated Rust module that:
  - Loads the compressed graph data.
  - Manages the WebGPU lifecycle (Device, Queue, Surface).
  - Handles the physics simulation and rendering loop.

### 3.3 Graph Rendering & Simulation (WebGPU)
- **GPU-Accelerated Layout:**
  - Implement a **Force-Directed Layout** (e.g., Fruchterman-Reingold or Barnes-Hut) using **WebGPU Compute Shaders**.
  - This allows real-time simulation of hundreds of thousands of nodes and edges by offloading the O(N²) or O(N log N) calculations to the GPU.
- **Rendering Strategy:**
  - **Nodes:** Use instanced rendering to draw circles/points. Each instance carries node-specific data (position, color, size).
  - **Edges:** Use a large vertex buffer of lines or a specialized shader to draw connections between nodes.
  - **Interactivity:** Use a quadtree (on GPU or CPU) for fast hover/click detection.

## 4. Implementation Plan

### Phase 1: Data Infrastructure (Current Status)
- [x] Basic downloader for crates.io db-dump.
- [ ] Implement a parser to extract relevant dependency data.
- [ ] Design and implement a compact binary format for the graph.

### Phase 2: WebGPU Renderer (WASM)
- [ ] Set up a `wgpu` boilerplate project targeting WASM.
- [ ] Implement a basic compute shader for force-directed layout.
- [ ] Implement instanced rendering for nodes and line rendering for edges.
- [ ] Add camera controls (pan, zoom, orbit).

### Phase 3: Svelte Integration
- [ ] Initialize a Svelte 5 project with Vite.
- [ ] Integrate the WASM module using `wasm-pack`.
- [ ] Build the UI for searching and selecting crates.
- [ ] Implement the "focal point" logic (highlighting a crate and its direct neighbors).

### Phase 4: Optimization & Polish
- [ ] Implement level-of-detail (LOD) rendering for large-scale views.
- [ ] Add labels and text rendering (using `wgpu_glyph` or SDF fonts).
- [ ] Support different visualization modes (e.g., chronological growth, category-based clustering).

## 5. Challenges & Solutions
- **Memory Management:** The crates.io graph is large. We must use bit-packed data structures and avoid excessive cloning in WASM.
- **Performance:** For 100k+ nodes, even a GPU layout needs optimization. Use hierarchical clustering or spatial partitioning (Barnes-Hut).
- **WebGPU Support:** WebGPU is still rolling out. Provide a WebGL2 fallback using `wgpu`'s multi-backend support.
