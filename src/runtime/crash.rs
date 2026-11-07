// What ends a run badly, or shows where a stuck one is: the fault handlers and the
// threads they report on. See runtime/crash.h. Recompiled functions are ordinary global
// symbols named sub_XXXXXXXX, so a host backtrace doubles as a guest call stack.
//
// A handler that returns -- a write to a watched page, a sample of the threads -- has
// interrupted a thread that may be holding anything, so it takes no lock and allocates
// nothing: the table of threads below is what lets it. A run that is ending may still
// write its reports on the way out, as a bug report needs them, knowing that a fault
// inside the allocator would hang there.

#[cfg(any(feature = "diagnostics", windows))]
use crate::log::log_info;
use crate::runtime::bridge;
#[cfg(feature = "diagnostics")]
use crate::runtime::kernel;
use crate::runtime::platform;
use crate::runtime::report;
use std::cell::Cell;
use std::ffi::{CStr, CString, c_char};
use std::ptr;

// A write to a page the GPU's memory watch protects: gpu/memory_watch.rs's handler when
// Rust owns memory_watch.cpp (build.rs, RUST_OWNS), the C++'s through bridge.rs if not.
// Both are safe in a signal handler: they read and write only atomics and protect pages.
#[cfg(not(all(feature = "rust-renderer", mw2_owns_memory_watch)))]
use crate::runtime::bridge::watch_handle_fault;
#[cfg(all(feature = "rust-renderer", mw2_owns_memory_watch))]
use crate::runtime::gpu::memory_watch::handle_fault as watch_handle_fault;

thread_local! {
    static NAME: Cell<*const c_char> = const { Cell::new(ptr::null()) };
}

// Never freed: the wait and predicate records quote a thread's name after the thread
// has gone.
fn keep_name(name: &CStr) -> *const c_char {
    let name: &'static CStr = Box::leak(CString::from(name).into_boxed_c_str());
    NAME.set(name.as_ptr());
    name.as_ptr()
}

/// The label this thread registered.
pub fn current_thread_name() -> Option<&'static CStr> {
    let name = NAME.get();
    (!name.is_null()).then(|| unsafe { CStr::from_ptr(name) })
}

// The system's name as well, so a profiler or /proc tells the threads apart. Not the
// main thread's: its name is the process's, and `pgrep mw2` has to go on finding it.
fn name_for_the_system(name: &CStr) {
    if !platform::is_main_thread() {
        platform::set_thread_name(name);
    }
}

#[cfg(feature = "diagnostics")]
fn watchdog_seconds() -> u32 {
    crate::env::number(c"MW2_WATCHDOG", 0) as u32
}

#[cfg(all(not(feature = "diagnostics"), windows))]
fn watchdog_seconds() -> u32 {
    0
}

pub use system::*;

#[cfg(not(windows))]
mod system {
    use super::*;
    use crate::runtime::fault_text::{Pointer, say};
    use crate::runtime::guest_memory;
    #[cfg(feature = "diagnostics")]
    use crate::runtime::watchpoint;
    use ppc_rt::{PPC_CODE_BASE, PPC_CODE_SIZE, PPC_IMAGE_BASE, PPC_IMAGE_SIZE};
    use std::ffi::{c_int, c_uint, c_void};
    use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

    const SIGILL: c_int = 4;
    const SIGTRAP: c_int = 5;
    const SIGBUS: c_int = 7;
    const SIGFPE: c_int = 8;
    const SIGUSR1: c_int = 10;
    const SIGSEGV: c_int = 11;
    #[cfg(feature = "diagnostics")]
    const SIGUSR2: c_int = 12;
    #[cfg(feature = "diagnostics")]
    const SIGALRM: c_int = 14;
    const SA_SIGINFO: c_int = 4;
    const SA_RESTART: c_int = 0x10000000;
    const SEGV_ACCERR: c_int = 2;

    // glibc's, for x86-64.
    #[repr(C)]
    struct SigAction {
        handler: usize,
        mask: [u64; 16],
        flags: c_int,
        restorer: usize,
    }

    #[repr(C)]
    struct SigInfo {
        number: c_int,
        error: c_int,
        code: c_int,
        padding: c_int,
        address: *mut u8,
    }

    unsafe extern "C" {
        fn sigaction(signal: c_int, action: *const SigAction, old: *mut SigAction) -> c_int;
        fn sigemptyset(mask: *mut [u64; 16]) -> c_int;
        #[cfg(feature = "diagnostics")]
        fn signal(signal: c_int, handler: usize) -> usize;
        #[cfg(feature = "diagnostics")]
        fn alarm(seconds: c_uint) -> c_uint;
        #[cfg(feature = "diagnostics")]
        fn raise(signal: c_int) -> c_int;
        fn pthread_self() -> usize;
        #[cfg(feature = "diagnostics")]
        fn pthread_kill(thread: usize, signal: c_int) -> c_int;
        #[cfg(feature = "diagnostics")]
        fn usleep(microseconds: c_uint) -> c_int;
        fn backtrace(frames: *mut *mut c_void, size: c_int) -> c_int;
        fn backtrace_symbols_fd(frames: *const *mut c_void, size: c_int, file: c_int);
        fn _exit(code: c_int) -> !;
        fn exit(code: c_int) -> !;
    }

    // Every guest thread registers itself so the watchdog can show where all of them
    // are. A place is taken by putting a name in it and left by taking it out, so a
    // handler reads this as it is, with no lock.
    struct Place {
        name: AtomicPtr<c_char>,
        thread: AtomicUsize,
    }
    const PLACES: usize = 256;
    static THREADS: [Place; PLACES] =
        [const { Place { name: AtomicPtr::new(ptr::null_mut()), thread: AtomicUsize::new(0) } }; PLACES];
    static DUMPING: AtomicBool = AtomicBool::new(false);

    fn dump_backtrace(why: &str) {
        let mut frames = [ptr::null_mut(); 64];
        let n = unsafe { backtrace(frames.as_mut_ptr(), 64) };
        say!("\n[E] ---- {why} ({n} frames) ----\n");
        unsafe { backtrace_symbols_fd(frames.as_ptr(), n, 2) };
        say!("[E] --------------------------------\n");
    }

    fn explain_address(base: *mut u8, address: *mut u8) {
        let top = base.wrapping_add(0x1_0000_0000);
        if !base.is_null() && address >= base && address < top {
            let guest = (address as u64).wrapping_sub(base as u64);
            let table = PPC_IMAGE_BASE + PPC_IMAGE_SIZE;
            say!("[E] guest address {guest:08X}\n");
            if guest < 0x10000 {
                say!("[E] -> null dereference (a stub returned 0 where a pointer was wanted)\n");
            } else if (table..table + PPC_CODE_SIZE * 2).contains(&guest) {
                let code = PPC_CODE_BASE + (guest - table) / 2;
                say!("[E] -> function table slot for guest code {code:08X}\n");
            } else if (PPC_IMAGE_BASE..PPC_IMAGE_BASE + PPC_IMAGE_SIZE).contains(&guest) {
                say!("[E] -> inside the loaded image\n");
            }
        } else {
            // Guest addresses are 32 bits, so a fault a few bytes past the top of the
            // window is a load that started just below 0x100000000 and ran off the end.
            let delta = if base.is_null() { 0 } else { (address as i64).wrapping_sub(base as i64) };
            if !base.is_null() && (0x1_0000_0000..0x1_0000_0010).contains(&delta) {
                say!(
                    "[E] a guest access ran off the top of the address space ({:+} bytes past)\n[E] -> the pointer was around 0xFFFFFFFF: something returned -1 where an address was wanted\n",
                    delta - 0x1_0000_0000
                );
            } else {
                say!("[E] outside the guest space (offset {delta:+}) -- fault in host runtime code\n");
            }
        }
    }

    unsafe extern "C" fn fault_handler(number: c_int, info: *const SigInfo, context: *mut c_void) {
        let (address, code) = unsafe { ((*info).address, (*info).code) };
        // A write watchpoint takes the page's faults and steps over them; it is not the
        // process dying, so it must be asked first.
        #[cfg(feature = "diagnostics")]
        if number == SIGSEGV && watchpoint::handle_write(address, context) {
            return;
        }
        // A write to a page the GPU's shadow of guest memory watches.
        if number == SIGSEGV && code == SEGV_ACCERR && watch_handle_fault(address.cast()) {
            return;
        }
        #[cfg(feature = "diagnostics")]
        if number == SIGTRAP && watchpoint::handle_step(context) {
            return;
        }
        #[cfg(not(feature = "diagnostics"))]
        let _ = context;

        let name = match number {
            SIGSEGV => "SIGSEGV",
            SIGBUS => "SIGBUS",
            SIGFPE => "SIGFPE",
            SIGTRAP => "SIGTRAP",
            _ => "SIGILL",
        };
        say!("\n[E] ---- guest fault: {name} at host {} ----\n", Pointer(address as usize));
        // The recompiler turns the PowerPC trap instructions into a host trap, and IW4
        // builds its asserts out of them -- so this is the title saying one of its own
        // invariants broke, not the runtime crashing.
        if number == SIGTRAP {
            say!("[E] -> a guest trap instruction: an assert inside the title fired\n");
        }
        explain_address(guest_memory::base(), address);
        dump_backtrace("fault backtrace");
        bridge::on_fault();
        unsafe { _exit(3) }
    }

    // MW2_DUMP=<hexaddr>[:words][,...] prints blocks of guest memory when the watchdog
    // fires. Several blocks, because the state that explains a stall is rarely all in
    // one place.
    #[cfg(feature = "diagnostics")]
    fn dump_one_block(spec: &CStr) {
        let (address, _) = crate::env::parse_number(spec, 16);
        let bytes = spec.to_bytes();
        let comma = bytes.iter().position(|&byte| byte == b',');
        let colon = bytes.iter().position(|&byte| byte == b':');
        let words = match colon {
            Some(colon) if comma.is_none_or(|comma| colon < comma) => {
                crate::env::parse_number(&spec[colon + 1..], 10).0 as u32
            }
            _ => 16,
        };
        say!("\n[E] guest memory at {address:08X}:\n");
        let memory = guest_memory::memory();
        for i in 0..words {
            let at = address.wrapping_add(u64::from(i) * 4);
            let value = memory.u32_at(at as u32);
            say!("[E]   +{:<5} {at:08X} = {value:08X} ({})\n", i * 4, value as i32);
        }
    }

    #[cfg(feature = "diagnostics")]
    fn dump_guest_memory() {
        let Some(mut spec) = crate::env::text(c"MW2_DUMP") else { return };
        if guest_memory::base().is_null() {
            return;
        }
        while !spec.is_empty() {
            dump_one_block(spec);
            let Some(comma) = spec.to_bytes().iter().position(|&byte| byte == b',') else { break };
            spec = &spec[comma + 1..];
        }
    }

    // Runs on each guest thread when the watchdog pokes it.
    unsafe extern "C" fn thread_dump_handler(_: c_int) {
        dump_backtrace("thread stack");
        DUMPING.store(false, Ordering::Release);
    }

    #[cfg(feature = "diagnostics")]
    fn dump_other_threads() {
        use crate::runtime::fault_text::Line;
        let me = unsafe { pthread_self() };
        for place in &THREADS {
            let name = place.name.load(Ordering::Acquire);
            let thread = place.thread.load(Ordering::Acquire);
            if name.is_null() || thread == 0 || thread == me {
                continue;
            }
            let mut line = Line::new();
            line.put(b"\n[E] ==== ");
            line.put(unsafe { CStr::from_ptr(name) }.to_bytes());
            line.put(b" ====\n");
            line.send();
            DUMPING.store(true, Ordering::Release);
            if unsafe { pthread_kill(thread, SIGUSR1) } != 0 {
                DUMPING.store(false, Ordering::SeqCst);
                continue;
            }
            // A last-gasp diagnostic, so a thread that never responds is skipped.
            for _ in 0..200 {
                if !DUMPING.load(Ordering::Acquire) {
                    break;
                }
                unsafe { usleep(1000) };
            }
        }
    }

    // SIGUSR2: every thread's stack, and the run goes on -- sent a few times over a
    // hang, it tells a thread that is stuck from one that is looping.
    #[cfg(feature = "diagnostics")]
    unsafe extern "C" fn sample_handler(_: c_int) {
        dump_backtrace("sampled, this thread is here");
        dump_other_threads();
    }

    #[cfg(feature = "diagnostics")]
    unsafe extern "C" fn alarm_handler(_: c_int) {
        dump_guest_memory();
        kernel::sync_objects::report_waits();
        dump_backtrace("watchdog fired, this thread is here");
        dump_other_threads();
        bridge::on_fault();
        unsafe { _exit(5) }
    }

    /// What a stall report asks of the other threads: `kill -USR2`.
    #[cfg(feature = "diagnostics")]
    pub fn sample_every_thread() {
        unsafe { raise(SIGUSR2) };
    }

    pub fn register_thread(name: &CStr) {
        report::thread_started(&report::System, name);
        let kept = keep_name(name).cast_mut();
        let me = unsafe { pthread_self() };
        for place in &THREADS {
            let free = place.name.compare_exchange(ptr::null_mut(), kept, Ordering::AcqRel, Ordering::Relaxed);
            if free.is_ok() {
                place.thread.store(me, Ordering::Release);
                break;
            }
        }
        name_for_the_system(name);
    }

    pub fn unregister_thread() {
        report::thread_ended(&report::System);
        let me = unsafe { pthread_self() };
        for place in &THREADS {
            if place.thread.load(Ordering::Acquire) == me {
                place.thread.store(0, Ordering::Release);
                place.name.store(ptr::null_mut(), Ordering::Release);
                return;
            }
        }
    }

    pub fn install() {
        // Its first call loads a library, which no handler may be the one to do.
        let mut frame = [ptr::null_mut(); 1];
        unsafe { backtrace(frame.as_mut_ptr(), 1) };

        let mut fault =
            SigAction { handler: fault_handler as *const () as usize, mask: [0; 16], flags: SA_SIGINFO, restorer: 0 };
        unsafe { sigemptyset(&mut fault.mask) };
        for number in [SIGSEGV, SIGBUS, SIGILL, SIGFPE, SIGTRAP] {
            unsafe { sigaction(number, &fault, ptr::null_mut()) };
        }
        let mut dump = SigAction {
            handler: thread_dump_handler as *const () as usize,
            mask: [0; 16],
            flags: SA_RESTART,
            restorer: 0,
        };
        unsafe {
            sigemptyset(&mut dump.mask);
            sigaction(SIGUSR1, &dump, ptr::null_mut());
        }
        #[cfg(feature = "diagnostics")]
        unsafe {
            signal(SIGUSR2, sample_handler as *const () as usize);
        }
        #[cfg(feature = "diagnostics")]
        {
            let seconds = watchdog_seconds();
            if seconds != 0 {
                unsafe {
                    signal(SIGALRM, alarm_handler as *const () as usize);
                    alarm(seconds);
                }
                log_info!("watchdog armed for %u seconds", seconds);
            }
        }
    }

    #[unsafe(no_mangle)]
    extern "C" fn MW2ReportBadIndirectCall(address: c_uint) -> ! {
        say!("\n[E] ---- indirect call to {address:08X} has no recompiled function ----\n");
        let target = u64::from(address);
        if target == 0 {
            say!("[E] target is null: the guest read a function pointer that was never written\n");
        } else if !(PPC_CODE_BASE..PPC_CODE_BASE + PPC_CODE_SIZE).contains(&target) {
            say!("[E] target is outside the recompiled code range\n");
        } else {
            say!("[E] target is inside the code range but was not identified as a function start\n");
        }
        dump_backtrace("indirect call backtrace");
        bridge::on_fault();
        unsafe { exit(4) }
    }
}

// Windows: one vectored exception handler does what the signal handlers do -- first the
// GPU watch's write faults and the pages the guest touches for the first time, then the
// report of a real fault.
#[cfg(windows)]
mod system {
    use super::*;
    use crate::log::format_text;
    use crate::runtime::guest_memory;
    use crate::runtime::windows::*;
    use std::ffi::{c_uint, c_void};
    use std::time::Duration;

    unsafe extern "C" {
        fn exit(code: i32) -> !;
    }

    fn describe(code: u32) -> Option<&'static CStr> {
        match code {
            EXCEPTION_ACCESS_VIOLATION => Some(c"access violation"),
            EXCEPTION_ILLEGAL_INSTRUCTION => Some(c"illegal instruction"),
            EXCEPTION_INT_DIVIDE_BY_ZERO => Some(c"integer divide by zero"),
            EXCEPTION_BREAKPOINT => Some(c"trap"),
            EXCEPTION_STACK_OVERFLOW => Some(c"stack overflow"),
            EXCEPTION_IN_PAGE_ERROR => Some(c"page error"),
            _ => None,
        }
    }

    unsafe extern "system" fn handler(exception: *mut ExceptionPointers) -> i32 {
        let record = unsafe { &*(*exception).record };
        let access = record.code == EXCEPTION_ACCESS_VIOLATION && record.parameters >= 2;
        if access {
            let write = record.information[0] == 1;
            let at = record.information[1] as *const c_void;
            if write && watch_handle_fault(at) {
                return EXCEPTION_CONTINUE_EXECUTION;
            }
            if guest_memory::commit_on_touch(at) {
                return EXCEPTION_CONTINUE_EXECUTION;
            }
        }
        // Everything else Windows raises -- C++ exceptions, debug output -- is someone
        // else's to handle.
        let Some(name) = describe(record.code) else { return EXCEPTION_CONTINUE_SEARCH };

        platform::print_line(&format_text!("\n[E] ---- guest fault: %s at %p ----", name, record.address.cast_const()));
        if record.code == EXCEPTION_BREAKPOINT {
            platform::print_line(c"[E] -> a guest trap instruction: an assert inside the title fired");
        }
        if access {
            let at = record.information[1];
            let base = guest_memory::base();
            if !base.is_null() && at >= base as usize && at < base as usize + 0x1_0000_0000 {
                platform::print_line(&format_text!("[E] guest address %08X", (at - base as usize) as u32));
            } else {
                platform::print_line(&format_text!(
                    "[E] host address %p, outside the guest space",
                    at as *const c_void
                ));
            }
        }
        platform::print_backtrace(c"fault backtrace");
        bridge::on_fault();
        unsafe { TerminateProcess(GetCurrentProcess(), 3) };
        EXCEPTION_CONTINUE_SEARCH
    }

    pub fn install() {
        unsafe { AddVectoredExceptionHandler(1, handler) };
        let seconds = watchdog_seconds();
        if seconds != 0 {
            log_info!("watchdog armed for %u seconds", seconds);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(u64::from(seconds)));
                #[cfg(feature = "diagnostics")]
                kernel::sync_objects::report_waits();
                bridge::on_fault();
                unsafe { TerminateProcess(GetCurrentProcess(), 5) };
            });
        }
    }

    pub fn register_thread(name: &CStr) {
        report::thread_started(&report::System, name);
        keep_name(name);
        name_for_the_system(name);
    }

    pub fn unregister_thread() {
        report::thread_ended(&report::System);
    }

    #[unsafe(no_mangle)]
    extern "C" fn MW2ReportBadIndirectCall(address: c_uint) -> ! {
        platform::print_line(&format_text!(
            "\n[E] ---- indirect call to %08X has no recompiled function ----",
            address
        ));
        platform::print_backtrace(c"indirect call backtrace");
        bridge::on_fault();
        unsafe { exit(4) }
    }
}
