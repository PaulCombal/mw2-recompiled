// The runtime; the renderer is gpu/, and what it shares with the launcher -- the
// translator, the device and pipeline layout, a shader's build and the compile pass's
// marker -- is the shaders crate (shaders/). build.rs compiles only this C++ into the
// game: the GPU capture (stream_capture.cpp) and rust_calls.cpp, diagnostics only; a
// player's build compiles none.
//
// The Rust calls that C++ through bridge.rs and runtime/rust_calls.cpp. The C++ calls
// the Rust through its own headers, whose inline functions call what exports.rs (and
// gpu/exports.rs) gives. exports.rs is what is
// shared with C++, and it alone turns C's pointers into safe types; what is the
// system's or a C library's is unsafe by nature, and is kept to itself (guest_memory,
// platform, windows, install/spawn, cpu_level, crash, fault_text, watchpoint, scan,
// online/udp, online/steam/client, and of the renderer gpu/exports, gpu/texture,
// gpu/memory_watch, gpu/vulkan/renderer_arena and every gpu/vulkan file that calls
// Vulkan or SDL). C++ never frees what Rust made, nor the other way round: text
// crosses as a C string the callee reads during the call, and bytes go back into a
// buffer the caller reserves (gpu/exports.rs's mw2_cp_save).
//
// The renderer is in this crate, so the command processor, the renderer and the
// texture cache inline into one another.

pub mod bridge;
pub mod clock;
mod cpu_level;
pub mod crash;
mod exports;
#[cfg(not(windows))]
mod fault_text;
pub mod guest_memory;
// The GPU: the command processor, the renderer and the texture cache.
pub mod gpu;
pub mod platform;
#[cfg(feature = "diagnostics")]
mod scan;
#[cfg(all(feature = "diagnostics", not(windows)))]
pub mod watchpoint;
#[cfg(windows)]
pub mod windows;

#[deny(unsafe_code)]
pub mod console;
#[deny(unsafe_code)]
pub mod frame_rate;
#[deny(unsafe_code)]
pub mod friends;
#[deny(unsafe_code)]
pub mod guide;
#[deny(unsafe_code)]
pub mod install;
#[deny(unsafe_code)]
pub mod kernel;
pub mod online;
#[deny(unsafe_code)]
pub mod pacing_trace;
#[cfg(feature = "diagnostics")]
#[deny(unsafe_code)]
pub mod player;
#[deny(unsafe_code)]
pub mod presence;
// The disc's executables have no size to change.
#[cfg_attr(not(T_R_VideoSizes), allow(dead_code, unused_imports))]
#[deny(unsafe_code)]
pub mod render_size;
#[deny(unsafe_code)]
pub mod report;
// MW2_REPLAY_GPU: the GPU's input fed back with no title running (run.rs hands over).
#[cfg(feature = "diagnostics")]
pub mod gpu_replay;
// The run itself.
#[deny(unsafe_code)]
pub mod run;
#[deny(unsafe_code)]
pub mod screen;
#[deny(unsafe_code)]
pub mod settings;
#[cfg(T_DATA_RenderTargets)]
#[deny(unsafe_code)]
pub mod shadow_resolution;
#[deny(unsafe_code)]
pub mod signin;
#[cfg(feature = "diagnostics")]
#[deny(unsafe_code)]
pub mod stutters;
#[deny(unsafe_code)]
pub mod truetype;
// gunzip, for the title's XLAST (tools/inflate_diff holds it to zlib).
#[deny(unsafe_code)]
mod inflate;
#[deny(unsafe_code)]
pub mod version;
// The XMA frame decoder, a port of FFmpeg's (LGPL 2.1 or later, wmapro/NOTICE.md).
#[deny(unsafe_code)]
pub mod wmapro;

/// The lock for an end-of-run report, which may be written by a thread that a fault or the
/// watchdog interrupted while it held this very lock: a short wait for the thread that
/// holds it, then nothing rather than a hang.
#[cfg(feature = "diagnostics")]
pub fn lock_for_report<T>(lock: &std::sync::Mutex<T>) -> Option<std::sync::MutexGuard<'_, T>> {
    for _ in 0..100 {
        match lock.try_lock() {
            Ok(guard) => return Some(guard),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => return Some(poisoned.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => std::thread::sleep(std::time::Duration::from_millis(1)),
        }
    }
    None
}
