pub mod format;
pub mod graph;

#[cfg(not(target_arch = "wasm32"))]
pub mod download;

#[cfg(target_arch = "wasm32")]
pub mod wasm;
