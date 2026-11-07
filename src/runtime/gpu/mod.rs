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
// What the stream holds, counted, and the texture dumps (diagnostics): the counting is
// safe, the dumps read guest memory through the untiler.
#[cfg(feature = "diagnostics")]
#[cfg_attr(not(mw2_owns_census), allow(dead_code))]
pub mod census;
// The command processor: raw guest reads and writes, so not under deny(unsafe_code).
// It, the census, the image pool, the register file and the hashes are compiled whoever
// owns their C++ files, their callers only when Rust does: out of RUST_OWNS, they stay
// compiled but unwired.
#[cfg_attr(not(mw2_owns_command_processor), allow(dead_code))]
pub mod command_processor;
// The three fold together (the plan's D16): the Rust command processor calls only the
// Rust census and image pool, so the Rust of one without the others would build and
// leave, for instance, the C++ pool's queued moves never made.
#[cfg(not(any(
    all(mw2_owns_command_processor, mw2_owns_census, mw2_owns_image_move),
    not(any(mw2_owns_command_processor, mw2_owns_census, mw2_owns_image_move))
)))]
compile_error!(
    "RUST_OWNS takes runtime/gpu/command_processor.cpp, runtime/gpu/census.cpp and runtime/image_move.cpp together or not at all"
);
// The C interface the C++ renderer calls while the two share the game.
mod exports;
// The ring consumer, the vertical blank and the interrupts they raise: gpu.cpp's, named
// after it as every module here is.
#[allow(clippy::module_inception)]
pub mod gpu;
// The renderer's hashes: safe code.
#[deny(unsafe_code)]
#[cfg_attr(not(mw2_owns_command_processor), allow(dead_code))]
pub mod hash;
// The image pool's copies at their draw: the queue is safe code, the copy a memmove of
// guest memory.
#[cfg_attr(not(mw2_owns_image_move), allow(dead_code))]
pub mod image_move;
// Which pages of guest physical memory the title has written, and the shadow it keeps of
// them: unsafe by nature (page protections, a fault handler's half, raw copies). Out of
// RUST_OWNS it stays compiled but unwired, and the C++ watch answers.
#[cfg_attr(not(mw2_owns_memory_watch), allow(dead_code))]
pub mod memory_watch;
// The register file: safe code.
#[deny(unsafe_code)]
#[cfg_attr(not(mw2_owns_command_processor), allow(dead_code))]
pub mod registers;
// Texture layouts and the untiler: raw guest reads, so not under deny(unsafe_code).
pub mod texture;
// The shader translator: safe code, but for its C interface (shader/ffi.rs).
pub mod shader;
pub mod vulkan;
