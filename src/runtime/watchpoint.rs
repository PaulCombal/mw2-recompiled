// A write watchpoint on guest memory: see runtime/watchpoint.h. Most of this runs in the
// fault handler, where it may only take what no interrupted thread can be holding: so
// the lock is a flag of its own, and the text goes out through fault_text.

use crate::env;
use crate::log::log_info;
use crate::runtime::bridge;
use crate::runtime::crash;
use crate::runtime::fault_text::say;
use crate::runtime::guest_memory;
use std::cell::Cell;
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};

const PROT_READ: c_int = 1;
const PROT_WRITE: c_int = 2;
const SC_PAGESIZE: c_int = 30;
const TRAP_FLAG: u64 = 0x100;
const FAULT_BUDGET: u64 = 2000000;

unsafe extern "C" {
    fn mprotect(address: *mut c_void, size: usize, protection: c_int) -> c_int;
    fn sysconf(name: c_int) -> i64;
    fn sched_yield() -> c_int;
    fn backtrace(frames: *mut *mut c_void, size: c_int) -> c_int;
    fn backtrace_symbols_fd(frames: *const *mut c_void, size: c_int, file: c_int);
}

static LO: AtomicU32 = AtomicU32::new(0); // the guest range being reported on
static HI: AtomicU32 = AtomicU32::new(0);
static PAGE: AtomicPtr<u8> = AtomicPtr::new(std::ptr::null_mut()); // the host pages that carry it
static LENGTH: AtomicUsize = AtomicUsize::new(0);
static ARMED: AtomicBool = AtomicBool::new(false);
static REPORTS: AtomicI32 = AtomicI32::new(0);
static BUDGET: AtomicI32 = AtomicI32::new(20);
static FAULTS: AtomicU64 = AtomicU64::new(0);

// Held from the write fault to the single-step trap that follows, so two threads cannot
// have the page unprotected at once. A thread that holds it runs one instruction, or the
// runtime's own copy, and cannot fault on the page meanwhile.
static HELD: AtomicBool = AtomicBool::new(false);

thread_local! {
    static STEPPING: Cell<bool> = const { Cell::new(false) };
}

fn lock() {
    while HELD.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        unsafe { sched_yield() };
    }
}

fn unlock() {
    HELD.store(false, Ordering::Release);
}

fn protect(flags: c_int) {
    unsafe { mprotect(PAGE.load(Ordering::Relaxed).cast(), LENGTH.load(Ordering::Relaxed), flags) };
}

fn arm(address: u32, bytes: u32) {
    let base = guest_memory::base();
    if base.is_null() || address == 0 || bytes == 0 {
        return;
    }
    let (lo, hi) = (address, address.wrapping_add(bytes));
    let page_size = unsafe { sysconf(SC_PAGESIZE) } as u32;
    let mask = !page_size.wrapping_sub(1);
    let first = base.wrapping_add((lo & mask) as usize);
    let last = base.wrapping_add((hi.wrapping_add(page_size - 1) & mask) as usize);
    LO.store(lo, Ordering::Relaxed);
    HI.store(hi, Ordering::Relaxed);
    PAGE.store(first, Ordering::Relaxed);
    LENGTH.store((last as usize).wrapping_sub(first as usize), Ordering::Relaxed);
    protect(PROT_READ);
    ARMED.store(true, Ordering::Release);
    log_info!(
        "watchpoint: guest %08X..%08X armed (%llu bytes of host pages, %d reports)",
        lo,
        hi,
        LENGTH.load(Ordering::Relaxed) as u64,
        BUDGET.load(Ordering::Relaxed)
    );
}

pub fn install() {
    BUDGET.store(env::number(c"MW2_WATCH_REPORTS", 20) as i32, Ordering::Relaxed);
    let Some(text) = env::text(c"MW2_WATCH") else { return };
    let (lo, rest) = env::parse_number(text, 16);
    let bytes = match rest.to_bytes().first() {
        Some(b':') => env::parse_number(&rest[1..], 0).0 as u32,
        _ => 4,
    };
    arm(lo as u32, if bytes != 0 { bytes } else { 4 });
}

pub fn suspend() {
    if !ARMED.load(Ordering::Acquire) {
        return;
    }
    lock();
    protect(PROT_READ | PROT_WRITE);
}

pub fn resume() {
    if !ARMED.load(Ordering::Acquire) {
        return;
    }
    protect(PROT_READ);
    unlock();
}

// The flags in the machine state a signal handler can change: gregs[REG_EFL] of the
// ucontext_t, whose gregs follow 40 bytes in.
fn flags(context: *mut c_void) -> *mut u64 {
    context.cast::<u8>().wrapping_add(40 + 17 * 8).cast()
}

/// From the fault handler, before it decides the process is dying: true when the fault
/// was this one's and has been dealt with.
#[inline(never)]
pub fn handle_write(address: *mut u8, context: *mut c_void) -> bool {
    if !ARMED.load(Ordering::Acquire) {
        return false;
    }
    let page = PAGE.load(Ordering::Relaxed);
    if address < page || address >= page.wrapping_add(LENGTH.load(Ordering::Relaxed)) {
        return false;
    }

    lock();
    STEPPING.set(true);
    protect(PROT_READ | PROT_WRITE);

    let guest = (address as usize).wrapping_sub(guest_memory::base() as usize) as u32;
    if guest >= LO.load(Ordering::Relaxed)
        && guest < HI.load(Ordering::Relaxed)
        && REPORTS.fetch_add(1, Ordering::SeqCst) < BUDGET.load(Ordering::Relaxed)
    {
        // Deep enough to reach the thread's own routine, and the thread named: a write
        // into another thread's block is only explained by who made it.
        let mut frames = [std::ptr::null_mut(); 40];
        let n = unsafe { backtrace(frames.as_mut_ptr(), 40) };
        let mut line = crate::runtime::fault_text::Line::new();
        let _ = std::fmt::Write::write_fmt(
            &mut line,
            format_args!("\n[K] watchpoint: guest write to {guest:08X} at frame {}, on ", bridge::gpu_frame_count()),
        );
        line.put(crash::current_thread_name().map_or(b"an unnamed thread", |name| name.to_bytes()));
        line.put(b"\n");
        line.send();
        // This function, the handler and the signal frame, then the writer.
        let skipped = if n > 3 { 3 } else { 0 };
        unsafe { backtrace_symbols_fd(frames.as_ptr().add(skipped as usize), n - skipped, 2) };
    }

    // Let the store retire, then trap on the next instruction and re-arm.
    unsafe { *flags(context) |= TRAP_FLAG };
    true
}

pub fn handle_step(context: *mut c_void) -> bool {
    if !STEPPING.get() {
        return false;
    }
    STEPPING.set(false);
    unsafe { *flags(context) &= !TRAP_FLAG };
    // Every write to the page faults, not just the ones in range, so a watchpoint on a
    // busy page has to give up or the run never finishes. Giving up silently is worse
    // than not watching at all: the run then shows no writes and reads as proof that
    // nothing writes the address.
    let n = FAULTS.fetch_add(1, Ordering::SeqCst) + 1;
    let reports = REPORTS.load(Ordering::SeqCst);
    if reports < BUDGET.load(Ordering::Relaxed) && n < FAULT_BUDGET {
        protect(PROT_READ);
    } else {
        ARMED.store(false, Ordering::Release);
        say!(
            "[K] watchpoint: disarmed after {reports} reports and {n} faults on the page -- later writes are NOT reported\n"
        );
    }
    unlock();
    true
}
