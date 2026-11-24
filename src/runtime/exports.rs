// The runtime's functions under C names: those the C++ the game links calls, by what the
// header of its name declares (runtime/crash.h, log.h), those
// the guest code calls (PPC_QueryTimebase, mw2_xma_register_store) and those other Rust calls
// through an extern declaration (bridge.rs, renderer_frame.rs). Each converts between C and a
// safe module's types and nothing else. The pointers are what those headers promise: valid,
// and a string has its zero.

use crate::runtime::crash;
use std::ffi::{CStr, c_char};

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_crash_register_thread(name: *const c_char) {
    crash::register_thread(unsafe { CStr::from_ptr(name) });
}

// The run's end (run.rs): crash.rs's fault (through bridge.rs) and crash.h's RequestExit
// ask for it by these names.
#[unsafe(no_mangle)]
extern "C" fn mw2_crash_on_fault() {
    crate::runtime::run::on_fault();
}

#[unsafe(no_mangle)]
unsafe extern "C" fn mw2_crash_request_exit(why: *const c_char) {
    crate::runtime::run::request_exit(unsafe { CStr::from_ptr(why) });
}

/// runtime/log.h's stamp: src/log.rs's clock.
#[unsafe(no_mangle)]
extern "C" fn mw2_log_stamp() -> *const c_char {
    crate::log::stamp()
}

#[cfg(feature = "diagnostics")]
mod diagnostics {
    use crate::runtime::{clock, pacing_trace, stutters};
    use std::sync::Once;

    fn now_us() -> i64 {
        pacing_trace::microseconds(clock::nanoseconds())
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_stutters_finished(_serial: u64, world: bool) {
        static WATCHING: Once = Once::new();
        stutters::finished(now_us(), world, || {
            WATCHING.call_once(|| {
                std::thread::spawn(stutters::watch_for_stalls);
            });
        });
    }
}

// The kernel, as the recompiled code declares it.
mod kernel_exports {
    use crate::runtime::kernel;

    /// The guest's timebase, which every mftb reads (ppc_rt declares it).
    #[unsafe(no_mangle)]
    extern "C" fn PPC_QueryTimebase() -> u64 {
        kernel::misc::timebase()
    }

    /// What the recompiled code's stw would write; the hardware reads its registers
    /// little-endian, so the title wrote them with stwbrx. The guest's stores into the XMA
    /// decoder's register window (kernel/xma.rs) come here through config/mw2_hooks.rs.
    #[unsafe(no_mangle)]
    extern "C" fn mw2_xma_register_store(address: u32, value: u32) {
        kernel::xma::register_store(address, value.swap_bytes());
    }
}
