// The renderer: the ring consumer owns the Renderer and the command processor calls it
// directly. What it shares with the launcher -- the translator, the device and pipeline
// layout, a shader's build and the compile pass's marker -- is the shaders crate's
// (shaders/); the GPU capture (stream_capture.cpp, C++) is called through
// runtime/rust_calls.cpp (docs/building.md, "The renderer").

// D3D9's command-buffer arena and its progress reports: safe code.
#[deny(unsafe_code)]
pub mod arena;
// What the stream holds, counted, and the texture dumps (diagnostics): the counting is
// safe, the dumps read guest memory through the untiler.
#[cfg(feature = "diagnostics")]
pub mod census;
// The command processor: raw guest reads and writes, so not under deny(unsafe_code).
pub mod command_processor;
// The C interface the GPU capture (C++, diagnostics) calls.
mod exports;
// The ring consumer, the vertical blank and the interrupts they raise.
#[allow(clippy::module_inception)]
pub mod gpu;
// The renderer's hashes: safe code.
#[deny(unsafe_code)]
pub mod hash;
// The image pool's copies at their draw: the queue is safe code, the copy a memmove of
// guest memory.
pub mod image_move;
// Which pages of guest physical memory the title has written, and the shadow it keeps of
// them: unsafe by nature (page protections, a fault handler's half, raw copies).
pub mod memory_watch;
// The register file: safe code.
#[deny(unsafe_code)]
pub mod registers;
// Texture layouts and the untiler: raw guest reads, so not under deny(unsafe_code).
pub mod texture;
// The shader translator, the shaders crate's (shaders/src/shader/).
pub use mw2_shaders::shader;
pub mod vulkan;
