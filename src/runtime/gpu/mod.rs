// The renderer in Rust: runtime/gpu/ and runtime/gpu/vulkan/ taken over a file at a
// time, each module named after the C++ file it replaces. build.rs's RUST_OWNS lists
// the C++ files Rust has taken over; each gives a cfg mw2_owns_<stem>, on which the
// Rust's hooks, exports and bridge routes for it depend, so that every function exists
// once whichever side has it. Until the switch-over the C++ renderer calls in through
// runtime/rust_renderer.cpp, which forwards its own declarations to exports.rs, and the
// Rust calls what is still C++ through runtime/rust_calls.cpp. The plan and the proofs
// of each step: docs/building.md, "The renderer moves to Rust".

// D3D9's command-buffer arena and its progress reports: safe code.
#[deny(unsafe_code)]
pub mod arena;
// The C interface the C++ renderer calls while the two share the game.
mod exports;
// The ring consumer, the vertical blank and the interrupts they raise: gpu.cpp's, named
// after it as every module here is.
#[allow(clippy::module_inception)]
pub mod gpu;
// Texture layouts and the untiler: raw guest reads, so not under deny(unsafe_code).
pub mod texture;
// The shader translator: safe code, but for its C interface (shader/ffi.rs).
pub mod shader;
pub mod vulkan;
