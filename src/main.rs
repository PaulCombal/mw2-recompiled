// The executable. The run is runtime/run.rs; the C++ that is left is the renderer
// (runtime/gpu/), which it calls through runtime/bridge.rs.
//
// no_main because Rust's own start would put signal handlers in front of the
// runtime's (runtime/crash.rs), and ignore SIGPIPE, before the first line of it.
#![no_main]

use std::ffi::{CStr, c_char, c_int};

// A panic must not unwind through guest frames, which have no landing pads.
#[cfg(not(panic = "abort"))]
compile_error!("the guest code needs panic = \"abort\", in every profile");

// What uses all of these is the hooks only the update's multiplayer has.
#[cfg_attr(any(not(mw2_title_mp), mw2_version_tu0, not(feature = "diagnostics")), allow(dead_code))]
mod env;
#[cfg_attr(any(not(mw2_title_mp), mw2_version_tu0), allow(dead_code))]
mod guest;
#[deny(unsafe_code)]
mod hooks;
#[cfg_attr(any(not(mw2_title_mp), mw2_version_tu0, not(feature = "diagnostics")), allow(dead_code))]
mod log;
mod runtime;
mod title;

// Linked for the guest code it brings: nothing here names it.
extern crate ppc_recomp;

// The C library's start calls this with the command line, as it would a C main.
#[unsafe(no_mangle)]
unsafe extern "C" fn main(argc: c_int, argv: *mut *mut c_char) -> c_int {
    runtime::clock::start();
    // The command line lives as long as the process; the run may be on another thread.
    let argv = argv as usize;
    runtime::platform::run_main(move || {
        let argv = argv as *mut *mut c_char;
        let arguments: Vec<&CStr> =
            (0..argc.max(0) as usize).map(|i| unsafe { CStr::from_ptr(*argv.add(i)) }).collect();
        runtime::run::run(&arguments)
    })
}
