// The runtime, moving here from runtime/ a module at a time. Each module is the
// C++ file of its name, safe code, and that file stays for the C++ build until the
// last one is here; build.rs compiles only the C++ the Rust build still has.
//
// C++ still calls some of them: runtime/gpu/ above all. It keeps the same header,
// whose inline functions call what exports.rs gives it, so that a caller does not
// change. exports.rs is what is shared with C++, and it alone turns C's pointers into
// safe types; what is the system's or a C library's is unsafe by nature, and is kept
// to itself (guest_memory, platform, windows, install/spawn, cpu_level, crash,
// fault_text, watchpoint, scan, online/udp, online/steam/client, and of the renderer
// gpu/exports, gpu/texture and every gpu/vulkan file that calls Vulkan or SDL). Text
// goes back through a sink that copies it (runtime/text_sink.h): C++ never frees what
// Rust made, nor the other way round.
//
// Rust is outside the C++'s ThinLTO and profile: what gpu/ calls for each draw
// stays inline in its header, or has been measured.

pub mod bridge;
pub mod clock;
mod cpu_level;
pub mod crash;
mod exports;
#[cfg(not(windows))]
mod fault_text;
pub mod guest_memory;
// The renderer, taking over runtime/gpu/ a file at a time (build.rs, RUST_OWNS).
#[cfg(feature = "rust-renderer")]
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
// The run itself, which the rigs, with no renderer, stand in for.
#[cfg(not(any(mw2_rig, mw2_screens_rig, mw2_hook_rig)))]
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
// gunzip, for the title's XLAST, which zlib did before.
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
