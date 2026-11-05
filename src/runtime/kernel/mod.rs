// The kernel: its objects and the waits on them, threads, critical sections and memory,
// files, saved games, the profile, the clocks, the GPU's calls, the controllers, the
// network, XAM, the audio render driver and the XMA decoder's contexts, and what the rest
// of the runtime asks of them (runtime/kernel/kernel.h). The XMA frames themselves are
// FFmpeg's (runtime/apu/xma_codec.cpp).

mod audio;
pub mod content;
pub mod files;
mod input;
pub mod memory;
pub mod misc;
mod net;
pub mod objects;
mod profile;
mod sync;
pub mod sync_objects;
pub mod threads;
mod video;
pub mod xam;
pub mod xma;

use crate::guest::{self, Memory, PPCContext, Register};
use crate::log::{log_error, log_info, log_warn};
use crate::runtime::{guest_memory, platform};
use crate::title;
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::ffi::{CStr, CString};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Instant;

#[cfg_attr(not(T_UI_LoadArenas), allow(unused_imports))]
pub use files::game_root;
pub use memory::allocate_guest;
pub use threads::new_thread_id;

// The Xbox NTSTATUS values these need.
pub const STATUS_SUCCESS: u32 = 0x0000_0000;
pub const STATUS_INVALID_HANDLE: u32 = 0xC000_0008;
pub const STATUS_INVALID_PARAMETER: u32 = 0xC000_000D;
pub const STATUS_NO_MEMORY: u32 = 0xC000_0017;
pub const STATUS_TIMEOUT: u32 = 0x0000_0102;
pub const STATUS_USER_APC: u32 = 0x0000_00C0;

pub use audio::report as report_audio;
pub use misc::system_time;

pub fn initialise(game_root: &CStr) {
    misc::start_timebase();
    files::set_game_root(platform::path(game_root));
    read_tls_directory();
    install_time_stamp_bundle();
    install_debug_monitor_data();
    let exists = platform::path(game_root).exists();
    log_info!("game root: %s%s", game_root, if exists { c"" } else { c"  (does not exist)" });
}

// The image's TLS template, which each thread's block starts as.
static TLS_TEMPLATE: AtomicU32 = AtomicU32::new(0);
static TLS_TEMPLATE_SIZE: AtomicU32 = AtomicU32::new(0);

// IMAGE_TLS_DIRECTORY32, big-endian, located through the PE data directory, whose own
// fields are little-endian.
fn read_tls_directory() {
    let memory = guest_memory::memory();
    let image = ppc_rt::PPC_IMAGE_BASE as u32;
    let little = |at: u32| memory.u32_at(image.wrapping_add(at)).swap_bytes();
    let pe_offset = little(0x3C); // e_lfanew
    if pe_offset.wrapping_add(0x100) > ppc_rt::PPC_IMAGE_SIZE as u32 {
        return;
    }
    let optional_magic = memory.u16_at(image.wrapping_add(pe_offset).wrapping_add(24)).swap_bytes();
    let directories = pe_offset.wrapping_add(24).wrapping_add(if optional_magic == 0x10B { 96 } else { 112 });
    let tls_rva = little(directories.wrapping_add(9 * 8));
    let tls_size = little(directories.wrapping_add(9 * 8 + 4));
    if tls_rva == 0 || tls_size < 24 {
        return;
    }
    // Guest-endian: raw data start and end, then the index and the zero fill.
    let entry = image.wrapping_add(tls_rva);
    let raw_start = memory.u32_at(entry);
    let raw_end = memory.u32_at(entry.wrapping_add(4));
    let zero_fill = memory.u32_at(entry.wrapping_add(16));
    if raw_end <= raw_start {
        return;
    }
    TLS_TEMPLATE.store(raw_start, Ordering::Relaxed);
    TLS_TEMPLATE_SIZE.store((raw_end - raw_start).wrapping_add(zero_fill), Ordering::Relaxed);
}

// X_TIME_STAMP_BUNDLE: interrupt time, system time, millisecond tick count. GetTickCount
// loads the pointer and reads the third field, so an unresolved pointer freezes the
// title's clock and every timeout built on it.
const TIME_STAMP_BUNDLE_SIZE: u32 = 24;
static TIME_STAMP_BUNDLE: AtomicU32 = AtomicU32::new(0);
static BOOT_TIME: OnceLock<Instant> = OnceLock::new();

// The image word that holds the pointer is an unresolved import record rather than an
// address, which is how it is recognised: the low half is an ordinal and the high half a
// small type tag, so a real pointer cannot be mistaken for one.
fn looks_like_unresolved_import(value: u32) -> bool {
    (value >> 24) == 0 && (value & 0xFFFF) != 0 && value < 0x0100_0000
}

fn install_time_stamp_bundle() {
    let memory = guest_memory::memory();
    let pointer = title::T_DATA_TimeStampBundlePtr;
    let existing = memory.u32_at(pointer);
    if !looks_like_unresolved_import(existing) {
        log_warn!(
            "time stamp bundle: %08X already holds %08X, not an unresolved import -- leaving it alone",
            pointer,
            existing
        );
        return;
    }
    let bundle = allocate_guest(TIME_STAMP_BUNDLE_SIZE, 16);
    if bundle == 0 {
        log_error!("could not allocate the time stamp bundle");
        return;
    }
    memory.zero(bundle, TIME_STAMP_BUNDLE_SIZE);
    memory.store_u32(pointer, bundle);
    BOOT_TIME.get_or_init(Instant::now);
    TIME_STAMP_BUNDLE.store(bundle, Ordering::Release);
    update_time_stamp_bundle();
}

// KeDebugMonitorData: a pointer to the debug monitor's dispatch table, null on a retail
// console. XAudio2's mixer reads it on every pass and, when the word it points at is not
// zero, calls a method through it -- so left as an import record it points into the low
// guest space, and whatever the title happens to have written there (handles handed back
// as object pointers live there) becomes a call target.
fn install_debug_monitor_data() {
    let memory = guest_memory::memory();
    let pointer = title::T_DATA_DebugMonitorPtr;
    const ORDINAL: u32 = 0x59;
    let existing = memory.u32_at(pointer);
    if !looks_like_unresolved_import(existing) || (existing & 0xFFFF) != ORDINAL {
        log_warn!("debug monitor data: %08X holds %08X, not the import record -- leaving it alone", pointer, existing);
        return;
    }
    let word = allocate_guest(16, 16);
    if word == 0 {
        log_error!("could not allocate the debug monitor word");
        return;
    }
    memory.store_u32(word, 0);
    memory.store_u32(pointer, word);
}

/// Brings the clock values the title reads directly up to now.
pub fn update_time_stamp_bundle() {
    let bundle = TIME_STAMP_BUNDLE.load(Ordering::Acquire);
    let Some(boot) = BOOT_TIME.get().filter(|_| bundle != 0) else { return };
    let memory = guest_memory::memory();
    let elapsed = boot.elapsed();
    memory.store_u64(bundle, (elapsed.as_nanos() / 100) as u64);
    memory.store_u64(bundle + 8, system_time());
    memory.store_u32(bundle + 16, elapsed.as_millis() as u32);
}

/// Returns the value r13 must hold. The recompiled code reads the thread id straight out
/// of these structures (KPCR.CurrentThread -> KTHREAD.ThreadId), so they have to be
/// populated or every thread reports id 0.
pub fn create_thread_block(thread_id: u32) -> u32 {
    // [block .. block+0x8000) is the KPCR, with r13 pointing 0x4000 in so the negative
    // r13-relative offsets the game uses stay inside it. The KTHREAD follows, then this
    // thread's copy of the image's TLS block.
    const PCR_SIZE: u32 = 0x8000;
    const PCR_BIAS: u32 = 0x4000;
    const THREAD_SIZE: u32 = 0x1000;
    const TLS_SIZE: u32 = 0x200;
    const TLS_POINTER_OFFSET: u32 = 0; // KPCR.TlsData
    const CURRENT_THREAD_OFFSET: u32 = 256; // KPCR.CurrentThread
    const THREAD_ID_OFFSET: u32 = 332; // KTHREAD.ThreadId

    let total = PCR_SIZE + THREAD_SIZE + TLS_SIZE;
    let block = allocate_guest(total, 0x1000);
    if block == 0 {
        return 0;
    }
    let memory = guest_memory::memory();
    memory.zero(block, total);
    let pcr = block + PCR_BIAS;
    let kthread = block + PCR_SIZE;
    let tls = block + PCR_SIZE + THREAD_SIZE;

    // The CRT hangs its per-thread scratch allocator off a slot in the TLS template,
    // reached as [[r13] + slot], so a null TlsData sends every thread to the same
    // imaginary pool at guest address 0 -- where they corrupt each other.
    let template = TLS_TEMPLATE.load(Ordering::Relaxed);
    let template_size = TLS_TEMPLATE_SIZE.load(Ordering::Relaxed);
    if template != 0 && template_size != 0 && template_size <= TLS_SIZE {
        memory.copy(tls, template, template_size);
    }
    memory.store_u32(pcr + TLS_POINTER_OFFSET, tls);
    memory.store_u32(pcr + CURRENT_THREAD_OFFSET, kthread);
    memory.store_u32(kthread + THREAD_ID_OFFSET, thread_id);
    pcr
}

/// Guest memory from the physical heap; zero when there is none.
pub fn allocate_physical(size: u32, alignment: u32) -> u32 {
    memory::PHYSICAL.allocate(guest_memory::memory(), size, alignment)
}

/// False when `address` is not the start of a live allocation.
pub fn free_guest(address: u32) -> bool {
    memory::VIRTUAL.free(address)
}

pub fn free_physical(address: u32) -> bool {
    memory::PHYSICAL.free(address)
}

pub fn report_memory() {
    if cfg!(feature = "diagnostics") {
        memory::VIRTUAL.report();
        memory::PHYSICAL.report();
    }
}

// The imports the generated stubs stand in for, and how often each was reached.
static UNIMPLEMENTED: Mutex<BTreeMap<CString, u64>> = Mutex::new(BTreeMap::new());

/// Called by the generated stubs: logs the first call and counts the rest.
pub fn unimplemented(name: &CStr) {
    let mut reached = UNIMPLEMENTED.lock().unwrap_or_else(PoisonError::into_inner);
    match reached.get_mut(name) {
        Some(count) => *count += 1,
        None => {
            reached.insert(name.to_owned(), 1);
            log_warn!("unimplemented: %s", name);
        }
    }
}

/// The end-of-run report of them.
pub fn report_unimplemented() {
    if !cfg!(feature = "diagnostics") {
        return;
    }
    let reached = UNIMPLEMENTED.lock().unwrap_or_else(PoisonError::into_inner);
    if reached.is_empty() {
        log_info!("no unimplemented imports were reached");
        return;
    }
    let mut counted: Vec<(&CString, u64)> = reached.iter().map(|(name, &count)| (name, count)).collect();
    counted.sort_by_key(|one| std::cmp::Reverse(one.1));
    log_info!("unimplemented imports actually reached (%u distinct):", counted.len() as u32);
    for (name, count) in counted {
        log_info!("  %8llu  %s", count, name.as_c_str());
    }
}

/// Bytes from the system's random source, for XNetRandom and the sessions this machine
/// makes; a rig's own under one.
#[cfg(not(mw2_rig))]
pub fn random_bytes(out: &mut [u8]) {
    for chunk in out.chunks_mut(8) {
        chunk.copy_from_slice(&platform::random().to_le_bytes()[..chunk.len()]);
    }
}

#[cfg(mw2_rig)]
#[allow(unsafe_code)]
pub fn random_bytes(out: &mut [u8]) {
    unsafe extern "C" {
        fn mw2_kernel_random_bytes(out: *mut u8, size: usize);
    }
    unsafe { mw2_kernel_random_bytes(out.as_mut_ptr(), out.len()) }
}

/// Signals the event a handle names, as NtSetEvent does; false when it names none.
pub fn set_event_handle(handle: u32) -> bool {
    let held = objects::lock();
    let Some(event) = held.lookup(handle).filter(|object| object.as_event().is_some()) else { return false };
    objects::set_event_held(&held, event);
    true
}

/// XOVERLAPPED. Asynchronous XAM calls take one and the caller polls it, so a call that
/// completes inline still has to mark it finished and signal the event the caller may be
/// waiting on.
pub fn complete_overlapped(memory: Memory, address: u32, result: u32) {
    complete_overlapped_ex(memory, address, result, result, 0);
}

/// The same, for a call that failed: the caller reads `result` from the block, and why
/// from its extended error. XOVERLAPPED: result, length, context, event, completion
/// routine and its context, extended error.
pub fn complete_overlapped_ex(memory: Memory, address: u32, result: u32, extended_error: u32, length: u32) {
    if address == 0 {
        return;
    }
    memory.store_u32(address, result);
    memory.store_u32(address + 4, length);
    memory.store_u32(address + 24, extended_error);
    let event = memory.u32_at(address + 12);
    if event != 0 {
        set_event_handle(event);
    }
}

/// The nth argument (from 0) of a guest call, for the ones past the eight that arrive in
/// r3-r10: the caller's parameter save area starts at r1+0x14 with eight-byte slots, so
/// the ninth is at r1+0x54.
pub fn stack_argument(ctx: &PPCContext, memory: Memory, n: u32) -> u32 {
    memory.u32_at(ctx.r1.as_u32().wrapping_add(0x14).wrapping_add(n * 8))
}

/// Runs the guest function at `address` on the caller's thread, borrowing its stack
/// below its frame.
pub fn call_guest(caller: &PPCContext, memory: Memory, address: u32, a0: u32, a1: u32, a2: u32) {
    let Some(function) = memory.function_at(address) else {
        log_warn!("CallGuest: no recompiled function at %08X", address);
        return;
    };
    let mut ctx = guest::blank_context();
    // Leave the caller's frame and its 64-byte red zone untouched.
    ctx.r1.u64 = u64::from(caller.r1.as_u32().wrapping_sub(0x400) & !0xF);
    ctx.r13.u64 = u64::from(caller.r13.as_u32());
    ctx.r3.u64 = u64::from(a0);
    ctx.r4.u64 = u64::from(a1);
    ctx.r5.u64 = u64::from(a2);
    // The caller's code keeps its own idea of the float mode (whether denormals flush:
    // on for vector code, off for scalar) and only switches when it thinks the mode
    // differs. The console keeps the two in separate registers, FPSCR and VSCR; x86 has
    // one, MXCSR, and the recompiled code tracks which it last set. A routine run here in
    // a context of its own can leave MXCSR switched -- vector code turns flushing on --
    // and the caller, believing it off, would flush every tiny value its scalar code makes
    // from then on. So the caller gets the register back as it left it.
    let before = guest::host_float_mode();
    guest::load_float_mode(&mut ctx);
    function.call(&mut ctx, memory);
    if guest::host_float_mode() != before {
        guest::set_host_float_mode(before);
    }
}

// User APCs. The title issues asynchronous reads and does its accounting in a completion
// routine, which NT delivers only when the issuing thread enters an alertable wait --
// never inside the call that queued it.
//
// `work` runs just before the routine, at delivery time. Asynchronous reads put the
// transfer itself there: the title double-buffers on the assumption that a read it has
// issued is still in flight while it decompresses the other buffer, so filling the buffer
// at issue time corrupts what it is reading.
pub type Work = Box<dyn FnOnce()>;

struct PendingApc {
    routine: u32,
    context: u32,
    io_status_block: u32,
    work: Option<Work>,
}

thread_local! {
    static APCS: RefCell<VecDeque<PendingApc>> = const { RefCell::new(VecDeque::new()) };
}

pub fn queue_user_apc(routine: u32, context: u32, io_status_block: u32, work: Option<Work>) {
    if routine == 0 {
        if let Some(work) = work {
            work();
        }
        return;
    }
    APCS.with_borrow_mut(|apcs| apcs.push_back(PendingApc { routine, context, io_status_block, work }));
}

/// Delivers as many as were queued when this was called, oldest first, and says whether
/// there were any. One at a time, so that nothing is held across the guest call: an APC
/// routine that waits alertably itself delivers what is still queued, as NT does, where
/// the C++ build's took the whole queue first and left those to the outer wait.
pub fn deliver_user_apcs(caller: &PPCContext, memory: Memory) -> bool {
    let queued = APCS.with_borrow(VecDeque::len);
    // One at a time, so that nothing is held across the guest call.
    for _ in 0..queued {
        let Some(apc) = APCS.with_borrow_mut(VecDeque::pop_front) else { break };
        if let Some(work) = apc.work {
            work();
        }
        call_guest(caller, memory, apc.routine, apc.context, apc.io_status_block, 0);
    }
    queued != 0
}
