# gcrates

> **⚠️ This project is 100% built with vibe coding. Just a toy project.**

A visualization tool for exploring the crates.io dependency graph in 3D space.

## Tech Stack

### Backend
- **Rust** - Core language for graph processing and WASM compilation
- **petgraph** - Graph data structure and algorithms
- **csv** - CSV parsing for crates.io database dump
- **reqwest** - HTTP client for downloading database dumps
- **flate2 & tar** - Compression and archive handling
- **wgpu** - GPU rendering (WebGPU)
- **wasm-bindgen** - Rust-to-JavaScript bindings
- **web-sys** - Web APIs bindings

### Frontend
- **Svelte 5** - UI framework
- **TypeScript** - Type-safe JavaScript
- **Vite** - Build tool and dev server
- **wasm-pack** - WASM packaging and bundling

## Project Structure

```
gcrates/
├── src/                          # Rust backend
│   ├── main.rs                   # CLI entry point
│   ├── lib.rs                    # Library root
│   ├── download.rs               # Database dump downloading
│   ├── graph.rs                  # Graph building from CSV data
│   ├── format.rs                 # Binary graph serialization (GCR1 format)
│   └── wasm.rs                   # WASM bindings and 3D rendering
├── frontend/                     # Svelte frontend
│   ├── src/
│   │   ├── main.ts               # Entry point
│   │   ├── GlobalGraphApp.svelte # Main visualization component
│   │   ├── global-renderer.ts    # WebGPU rendering logic
│   │   └── global.css            # Styles
│   ├── package.json              # Frontend dependencies
│   ├── vite.config.ts            # Vite configuration
│   └── tsconfig.json             # TypeScript configuration
├── Cargo.toml                    # Rust dependencies
└── README.md                     # This file
```

## Usage

### Prerequisites
- Rust toolchain with `wasm32-unknown-unknown` target
- Node.js and npm
- wasm-pack

### CLI Commands

#### Download crates.io database dump
```bash
cargo run -- download [--url <URL>] [--output <PATH>]
```
- `--url`: Database dump URL (default: `https://static.crates.io/db-dump.tar.gz`)
- `--output`: Output directory (default: `db-dump`)

#### Build dependency graph
```bash
cargo run -- build-graph [--input <PATH>] [--output <PATH>] [OPTIONS]
```
- `--input`: Input database dump directory (default: `db-dump`)
- `--output`: Output graph file (default: `artifacts/graph.gcr`)
- `--exclude-normal`: Exclude normal dependencies
- `--exclude-build`: Exclude build dependencies
- `--exclude-dev`: Exclude dev dependencies
- `--exclude-target-specific`: Exclude target-specific dependencies

#### Inspect graph
```bash
cargo run -- inspect [--graph <PATH>] [--crate <NAME>]
```
- `--graph`: Graph file path (default: `artifacts/graph.gcr`)
- `--crate`: Specific crate to inspect (optional)

### Frontend Development

```bash
cd frontend

npm install

npm run dev
```

## How It Works

1. **Download**: Fetches the crates.io database dump (CSV files)
2. **Build Graph**: Parses CSV data and constructs a dependency graph using petgraph
3. **Serialize**: Converts the graph to a compact binary format (GCR1)
4. **Visualize**: WASM module loads the binary graph and renders it in 3D using WebGPU
5. **Interact**: Frontend provides search, navigation, and inspection capabilities

## Features

- 3D visualization of the entire crates.io dependency ecosystem
- Search for crates by name with prefix matching
- Interactive navigation (drag to rotate, WASD to move, scroll to zoom)
- Minimap for spatial awareness
- Crate inspection with dependency information
- Configurable dependency filtering (normal, build, dev, target-specific)
- Compact binary graph format for efficient storage and loading

## Graph Format

The project uses a custom binary format (GCR1) for storing graphs:
- Magic bytes: `GCR1`
- Compact string pool for deduplication
- Efficient package and dependency storage
- Supports metadata like download counts and version information
