// The launcher's entry. The window, the install and the tools are C++ (launcher/*.cpp,
// the library launcher_core); the compile pass is Rust (precompile.rs), on the crate the
// game builds its shaders with, so both build every shader the same way.
//
// `main` is the C runtime's, as the C++ launcher's was: the arguments it hands over
// (Windows' in the code page, which main.cpp reads the wide command line instead of),
// and nothing of Rust's start-up -- no SIGPIPE ignored, which the game the launcher
// starts would inherit, no file descriptors opened in place of closed ones.
#![no_main]
// A player's build opens no console window; a terminal it was started from gets what
// the terminal mode prints (main.cpp). The diagnostics build keeps its own console.
#![cfg_attr(not(feature = "diagnostics"), windows_subsystem = "windows")]

mod precompile;

use std::ffi::{c_char, c_int};

unsafe extern "C" {
    // main.cpp's main.
    fn launcher_run(argc: c_int, argv: *mut *mut c_char) -> c_int;
}

#[unsafe(no_mangle)]
extern "C" fn main(argc: c_int, argv: *mut *mut c_char) -> c_int {
    mw2_shaders::set_log(precompile::log);
    // SAFETY: the C runtime's arguments, as it hands them.
    unsafe { launcher_run(argc, argv) }
}
