// The GPU as the title sees it: the thread that consumes the ring buffer, the vertical
// blank, and the interrupts both raise into the title's handler (runtime/gpu/gpu.cpp,
// whose public API gpu.h is; the C++ command processor reaches what it needs of this
// through runtime/rust_renderer.cpp's forwarders, and this calls the C++ that is left
// through bridge::gpu).
//
// MW2's D3D9 is statically linked, so it is already recompiled and running: it builds
// real PM4 command streams and publishes a ring write pointer. The consumer here reads
// that stream as the console's command processor does, and the draws in it are
// rendered with Vulkan.
//
// What the C++ held as plain fields that other threads wrote -- the ring's place and
// size, its read index, the read pointer's write-back, the interrupt callback and its
// context -- are atomics here, the callback and its context one word, so that a reader
// never sees one without the other. Every guest access goes through guest::Memory.

use crate::guest::{self, Memory};
use crate::log::{log_info, log_warn};
use crate::runtime::bridge::gpu as calls;
use crate::runtime::gpu::vulkan::presenter;
use crate::runtime::{clock, crash, guest_memory, kernel, platform, report};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant};

// Registers are plain memory in this address space; the CPU writes some straight
// through the aperture rather than through the command stream (gpu/internal.h).
const REGISTER_APERTURE: u32 = 0x7FC8_0000;
const CP_RB_WPTR: u32 = REGISTER_APERTURE + 0x714;

// D3D9's interrupt handler only retires queued flips -- and with them the frame counter
// its render thread waits on -- when bit 0 is set.
const DISPLAY_STATUS: u32 = REGISTER_APERTURE + 0x6544;
const DISPLAY_STATUS_VBLANK: u32 = 0x1;

// The interrupt sources the title's handler tells apart.
const VBLANK_SOURCE: u32 = 0;
const COMMAND_PROCESSOR_SOURCE: u32 = 1;

// KPCR.CurrentProcessorNumber.
const PROCESSOR_OFFSET: u32 = 268;

// The guest stack each raising thread runs the handler on.
const INTERRUPT_STACK: u32 = 64 * 1024;

// 60 Hz, or the display's own blank when the window can follow it.
const PERIOD_NS: u64 = 16_666_667;

/// The ring: guest VA of dword 0, its size in dwords (a power of two), the mask, and
/// how far the consumer has read. Written by VdInitializeRingBuffer's thread and the
/// consumer, read by the consumer once a lap and by the reports.
struct Ring {
    base: AtomicU32,
    dwords: AtomicU32,
    mask: AtomicU32,
    read_index: AtomicU32,
}

static RING: Ring =
    Ring { base: AtomicU32::new(0), dwords: AtomicU32::new(0), mask: AtomicU32::new(0), read_index: AtomicU32::new(0) };
// Guest VA, or 0.
static RPTR_WRITE_BACK: AtomicU32 = AtomicU32::new(0);
// The callback in the high word, its context in the low one.
static INTERRUPT: AtomicU64 = AtomicU64::new(0);

static RUNNING: AtomicBool = AtomicBool::new(false);
// VdSwap calls.
static FRAMES: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "diagnostics")]
static VBLANKS: AtomicU64 = AtomicU64::new(0);
// The consumer's time in the batches it executed; the rest of its life it had nothing to
// read, which is the title not keeping it fed. Times on clock::nanoseconds, 0 unset.
#[cfg(feature = "diagnostics")]
static CONSUMER_BUSY_NS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "diagnostics")]
static CONSUMER_STARTED: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "diagnostics")]
static CONSUMER_STOPPED: AtomicU64 = AtomicU64::new(0);
static START: AtomicU64 = AtomicU64::new(0);

struct Handles {
    worker: JoinHandle<()>,
    vblank: JoinHandle<()>,
}

enum Threads {
    None,
    Running(Handles),
    // A shutdown is joining them and taking the renderer down.
    Stopping,
}

// Taken by the first shutdown. A second one -- the exit thread's while the run thread
// shuts down too -- waits for the first to finish and returns, so that what it does next
// (the reports, the exit) never runs beside the renderer's teardown.
static THREADS: Mutex<Threads> = Mutex::new(Threads::None);
static STOPPED: Condvar = Condvar::new();

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

thread_local! {
    // The guest PCR the current host thread raises interrupts with: the ring consumer
    // and the vblank each have one, because a KPCR is per hardware thread on the
    // console too.
    static GUEST_PCR: Cell<u32> = const { Cell::new(0) };
    // The thread's guest stack for the handler, allocated at its first interrupt.
    static STACK: Cell<Option<u32>> = const { Cell::new(None) };
}

// ---- the turn the two raising threads take ----------------------------------------------

/// The console delivers a graphics interrupt on one hardware thread, so the two threads
/// that can raise one here take turns. Held across a call into the guest, so it is no
/// guard object: lock and unlock are called around the call (src/hooks/mod.rs's rule).
/// A thread that does not get it spins a little, then parks until the holder lets go;
/// the holder lets go before it unparks anyone, and a parked thread tries again when it
/// wakes, so a stray wake costs one more try.
struct GuestCall {
    held: AtomicBool,
    // Those parked, or about to park, for the turn.
    waiting: Mutex<Vec<Thread>>,
}

const SPINS: u32 = 200;

impl GuestCall {
    const fn new() -> Self {
        Self { held: AtomicBool::new(false), waiting: Mutex::new(Vec::new()) }
    }

    fn try_take(&self) -> bool {
        self.held.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok()
    }

    fn lock(&self) {
        let mut spins = 0;
        loop {
            if self.try_take() {
                return;
            }
            if spins < SPINS {
                spins += 1;
                std::hint::spin_loop();
                continue;
            }
            {
                let mut waiting = lock(&self.waiting);
                // An unlock lets go first and takes this lock after: either it finds this
                // thread in the list, or this finds the turn free.
                if self.try_take() {
                    return;
                }
                let me = thread::current();
                if !waiting.iter().any(|thread| thread.id() == me.id()) {
                    waiting.push(me);
                }
            }
            thread::park();
        }
    }

    fn unlock(&self) {
        self.held.store(false, Ordering::Release);
        let waiting = std::mem::take(&mut *lock(&self.waiting));
        for thread in waiting {
            thread.unpark();
        }
    }
}

static GUEST_CALL: GuestCall = GuestCall::new();

// ---- interrupts ---------------------------------------------------------------------------

fn memory() -> Memory {
    guest_memory::memory()
}

fn raise_interrupt(source: u32) {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    let interrupt = INTERRUPT.load(Ordering::Acquire);
    let (callback, context) = ((interrupt >> 32) as u32, interrupt as u32);
    let memory = memory();
    let function = if callback != 0 { memory.function_at(callback) } else { None };
    let pcr = GUEST_PCR.get();
    let Some(function) = function.filter(|_| pcr != 0) else {
        if !REPORTED.swap(true, Ordering::Relaxed) {
            log_warn!("gpu: no graphics interrupt callback to call yet (%08X)", callback);
        }
        return;
    };
    let stack = STACK.get().unwrap_or_else(|| {
        let stack = kernel::allocate_guest(INTERRUPT_STACK, 0x1000);
        STACK.set(Some(stack));
        stack
    });
    if stack == 0 {
        return;
    }
    let mut ctx = guest::blank_context();
    ctx.r1.u64 = u64::from(stack.wrapping_add(INTERRUPT_STACK).wrapping_sub(0x100) & !0xF);
    ctx.r13.u64 = u64::from(pcr);
    ctx.r3.u64 = u64::from(source);
    ctx.r4.u64 = u64::from(context);
    // The guest's float mode is loaded and the host's is not put back, as in the C++.
    guest::load_float_mode(&mut ctx);
    function.call(&mut ctx, memory);
}

/// { cpu mask }: a command-processor interrupt, once per processor in the mask, as Xenia
/// raises it. The title's handler clears that processor's pending bit and reads which
/// processor it is on from the KPCR, so the PCR the call runs on reports each one in turn.
pub fn raise_command_processor_interrupt(cpu_mask: u32) {
    GUEST_CALL.lock();
    let memory = memory();
    let processor = GUEST_PCR.get().wrapping_add(PROCESSOR_OFFSET);
    let saved = memory.u8_at(processor);
    for n in 0..6 {
        if cpu_mask & (1 << n) != 0 {
            memory.store_u8(processor, n as u8);
            raise_interrupt(COMMAND_PROCESSOR_SOURCE);
        }
    }
    memory.store_u8(processor, saved);
    GUEST_CALL.unlock();
}

// A blank: the display status the handler reads, then the interrupt.
fn blank() {
    #[cfg(feature = "diagnostics")]
    VBLANKS.fetch_add(1, Ordering::Relaxed);
    memory().store_u32(DISPLAY_STATUS, DISPLAY_STATUS_VBLANK);
    GUEST_CALL.lock();
    raise_interrupt(VBLANK_SOURCE);
    GUEST_CALL.unlock();
}

// ---- the threads ------------------------------------------------------------------------

#[cfg(windows)]
unsafe extern "system" {
    fn Sleep(milliseconds: u32);
}

// tools/gpu_diff.py --rig interrupts runs the consumer's and the vblank's loops on a
// clock of its own, which these four are in the rig, as gpu_cpp.cpp has them for the C++.
#[cfg(mw2_gpu_rig)]
unsafe extern "C" {
    fn rig_loop_now() -> u64;
    fn rig_loop_sleep(nanoseconds: u64);
    fn rig_loop_bundle();
    fn rig_loop_blank_period() -> u64;
}

// The loops' clock: std::chrono::steady_clock's in the C++.
fn now() -> Instant {
    #[cfg(mw2_gpu_rig)]
    {
        static ORIGIN: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);
        // SAFETY: the rig's function.
        *ORIGIN + Duration::from_nanos(unsafe { rig_loop_now() })
    }
    #[cfg(not(mw2_gpu_rig))]
    Instant::now()
}

fn update_time_stamp_bundle() {
    #[cfg(mw2_gpu_rig)]
    // SAFETY: the rig's function.
    unsafe {
        rig_loop_bundle()
    };
    #[cfg(not(mw2_gpu_rig))]
    kernel::update_time_stamp_bundle();
}

fn guest_blank_period() -> u64 {
    #[cfg(mw2_gpu_rig)]
    // SAFETY: the rig's function.
    return unsafe { rig_loop_blank_period() };
    #[cfg(not(mw2_gpu_rig))]
    presenter::guest_blank_period()
}

// std::this_thread::sleep_for as the C++ build's library has it: nanosleep on Linux, and
// on Windows libc++'s Sleep of the time rounded up to a whole millisecond.
fn sleep_for(time: Duration) {
    #[cfg(mw2_gpu_rig)]
    // SAFETY: the rig's function.
    unsafe {
        rig_loop_sleep(time.as_nanos() as u64)
    };
    #[cfg(all(not(windows), not(mw2_gpu_rig)))]
    thread::sleep(time);
    #[cfg(all(windows, not(mw2_gpu_rig)))]
    {
        let nanoseconds = time.as_nanos() as u64;
        let milliseconds = nanoseconds / 1_000_000 + u64::from(nanoseconds % 1_000_000 != 0);
        // SAFETY: Sleep takes any count of milliseconds.
        unsafe { Sleep(milliseconds as u32) };
    }
}

fn spawn(name: &str, body: impl FnOnce() + Send + 'static) -> JoinHandle<()> {
    // The stack a C++ std::thread gets: both run the title's interrupt handler on it.
    thread::Builder::new().name(name.into()).stack_size(platform::default_thread_stack()).spawn(body).unwrap_or_else(
        |_| {
            // A std::thread that cannot start ends the program.
            crate::log::log_error!("gpu: a thread of the GPU would not start");
            std::process::abort()
        },
    )
}

#[cfg(feature = "diagnostics")]
fn note(event: u8, value: u32) {
    use crate::runtime::pacing_trace;
    if pacing_trace::on() {
        pacing_trace::note(clock::nanoseconds(), event, value, platform::thread_id());
    }
}

#[cfg(not(feature = "diagnostics"))]
fn note(_event: u8, _value: u32) {}

// Executes `available` dwords from `read`: the stutter and report timers around the
// command processor's execution and nothing else.
fn execute_batch(base: u32, read: u32, mask: u32, available: u32) {
    #[cfg(feature = "diagnostics")]
    let stutter_from = crate::runtime::stutters::on().then(Instant::now);
    let report_from = report::on().then(|| report::timed_begin(clock::nanoseconds(), report::EXECUTING));
    #[cfg(feature = "diagnostics")]
    calls::stream_batch_begin(base, read, mask, available);
    calls::execute_ring(base, read, mask, available);
    #[cfg(feature = "diagnostics")]
    calls::stream_batch_end();
    if let Some(began) = report_from {
        report::timed_end(clock::nanoseconds(), report::EXECUTING, began);
    }
    #[cfg(feature = "diagnostics")]
    if let Some(from) = stutter_from {
        let stutters = crate::runtime::stutters::RING_BATCHES;
        crate::runtime::stutters::add(stutters, from.elapsed().as_nanos() as u64);
    }
}

// The ring consumer.
fn worker(pcr: u32) {
    crash::register_thread(c"gpu ring consumer");
    GUEST_PCR.set(pcr);
    log_info!("gpu: ring consumer started");
    #[cfg(feature = "diagnostics")]
    CONSUMER_STARTED.store(clock::nanoseconds(), Ordering::Relaxed);
    // Laps in a row with nothing to read. The title hands the ring over in small pieces,
    // so the consumer runs dry hundreds of times a frame; only a lull of a millisecond or
    // so is worth waking the recorder for.
    let mut idle_laps: u32 = 0;
    let memory = memory();

    while RUNNING.load(Ordering::Relaxed) {
        calls::deliver_occlusion_queries();
        if RING.dwords.load(Ordering::Acquire) == 0 {
            sleep_for(Duration::from_millis(1));
            continue;
        }
        let (base, mask) = (RING.base.load(Ordering::Relaxed), RING.mask.load(Ordering::Relaxed));
        let read = RING.read_index.load(Ordering::Relaxed);

        let wptr = memory.u32_at(CP_RB_WPTR) & mask;
        if wptr != read {
            let available = wptr.wrapping_sub(read) & mask;
            #[cfg(feature = "diagnostics")]
            let began = now();
            note(b'B', available);
            execute_batch(base, read, mask, available);
            note(b'E', 0);
            #[cfg(feature = "diagnostics")]
            {
                // A batch slow enough to notice is the consumer wedged, not busy.
                let took = now() - began;
                CONSUMER_BUSY_NS.fetch_add(took.as_nanos() as u64, Ordering::Relaxed);
                if took > Duration::from_millis(200) {
                    static SHOWN: AtomicU32 = AtomicU32::new(0);
                    if SHOWN.fetch_add(1, Ordering::Relaxed) < 8 {
                        log_warn!("gpu: a ring batch of %u dwords took %lld ms", available, took.as_millis() as i64);
                    }
                }
            }
            RING.read_index.store(wptr, Ordering::Relaxed);
            let write_back = RPTR_WRITE_BACK.load(Ordering::Relaxed);
            if write_back != 0 {
                memory.store_u32(write_back, wptr);
            }
            idle_laps = 0;
            continue;
        }
        idle_laps = idle_laps.wrapping_add(1);
        if idle_laps == 10 {
            calls::renderer_idle();
        }
        sleep_for(Duration::from_micros(100));
    }
    #[cfg(feature = "diagnostics")]
    CONSUMER_STOPPED.store(clock::nanoseconds(), Ordering::Relaxed);
    log_info!("gpu: ring consumer stopped");
}

// The display's vertical blank is a property of the display. Raising it from the ring
// consumer tied it to how long a frame took to draw -- and since D3D9's swap bookkeeping
// and the title's own clock hang off it, the whole simulation slowed with the frame rate.
fn vblank_worker(pcr: u32) {
    crash::register_thread(c"gpu vblank");
    GUEST_PCR.set(pcr);
    // 60 Hz, or the display's own blank when the window can follow it: on the console
    // the title's blank is the display's, and a timer beside the display drifts from it.
    let period = || {
        let display = guest_blank_period();
        Duration::from_nanos(if display != 0 { display } else { PERIOD_NS })
    };
    let mut next = now() + Duration::from_nanos(PERIOD_NS);
    while RUNNING.load(Ordering::Relaxed) {
        // The console's clock interrupt moves the time stamp bundle on every millisecond
        // (Xenia's timer does the same), and the title's millisecond clock is its tick
        // count. Moved on only at a blank, that clock went in 16.7 ms steps, and every
        // wait the title times with it -- Com_Frame's limiter, its pacing after a swap --
        // ended at the next blank rather than when it was due.
        update_time_stamp_bundle();
        let at = now();
        if at < next {
            sleep_for((next - at).min(Duration::from_millis(1)));
            continue;
        }
        // After a stall, resume from now rather than firing once for every blank that
        // went by: the title reads a heartbeat, not a tick count.
        let blank_period = period();
        next = if at - next > blank_period * 4 { at + blank_period } else { next + blank_period };
        blank();
    }
}

// ---- the API ----------------------------------------------------------------------------

pub fn running() -> bool {
    RUNNING.load(Ordering::Relaxed)
}

/// Seconds since the GPU started, for the per-second tables and traces.
pub fn seconds() -> f64 {
    clock::nanoseconds().wrapping_sub(START.load(Ordering::Relaxed)) as f64 / 1e9
}

/// The ring's read and write indices, and what the read-pointer write-back holds.
pub fn ring_position() -> (u32, u32, u32) {
    let memory = memory();
    let read = RING.read_index.load(Ordering::Relaxed);
    let mask = RING.mask.load(Ordering::Relaxed);
    let written = if mask != 0 { memory.u32_at(CP_RB_WPTR) & mask } else { 0 };
    let write_back = RPTR_WRITE_BACK.load(Ordering::Relaxed);
    (read, written, if write_back != 0 { memory.u32_at(write_back) } else { 0 })
}

/// Where the ring is, its size, and the read pointer's write-back (MW2_CAPTURE_GPU).
#[cfg(feature = "diagnostics")]
pub fn ring_layout() -> (u32, u32, u32) {
    (RING.base.load(Ordering::Relaxed), RING.dwords.load(Ordering::Relaxed), RPTR_WRITE_BACK.load(Ordering::Relaxed))
}

pub fn initialise() {
    // Decided under the lock: a shutdown asked for meanwhile waits for the threads to be
    // there to stop, and a start during another call's teardown waits for it to end.
    let mut threads = lock(&THREADS);
    loop {
        match *threads {
            Threads::Running(_) => return,
            Threads::Stopping => threads = STOPPED.wait(threads).unwrap_or_else(|poisoned| poisoned.into_inner()),
            Threads::None => break,
        }
    }
    RUNNING.store(true, Ordering::SeqCst);
    START.store(clock::nanoseconds(), Ordering::Relaxed);
    presenter::start();
    // Eagerly, not on the first draw: a renderer that comes up on the first bind reports
    // the whole opening burst as failures.
    if calls::rendering() && !calls::renderer_initialise() {
        log_warn!("gpu: no device came up to render on");
    }
    let gpu_pcr = kernel::create_thread_block(0xF000);
    let vblank_pcr = kernel::create_thread_block(0xF001);
    let worker = spawn("gpu ring consumer", move || worker(gpu_pcr));
    let vblank = spawn("gpu vblank", move || vblank_worker(vblank_pcr));
    *threads = Threads::Running(Handles { worker, vblank });
}

pub fn shutdown() {
    let handles = {
        let mut threads = lock(&THREADS);
        loop {
            match std::mem::replace(&mut *threads, Threads::Stopping) {
                Threads::Running(handles) => break handles,
                Threads::Stopping => threads = STOPPED.wait(threads).unwrap_or_else(|poisoned| poisoned.into_inner()),
                Threads::None => {
                    *threads = Threads::None;
                    return;
                }
            }
        }
    };
    RUNNING.store(false, Ordering::SeqCst);
    let _ = handles.vblank.join();
    let _ = handles.worker.join();
    #[cfg(feature = "diagnostics")]
    calls::stream_shutdown();
    // The window thread first, because it shows an image the renderer owns.
    presenter::stop_presenting();
    calls::renderer_shutdown();
    calls::textures_shutdown();
    calls::pipeline_shutdown();
    presenter::stop();
    *lock(&THREADS) = Threads::None;
    STOPPED.notify_all();
}

/// True while a command processor is consuming the ring, so work that must happen at a
/// point in the stream can be left to it.
pub fn consuming() -> bool {
    #[cfg(mw2_hook_rig)]
    if let Some(consuming) = rig::consuming() {
        return consuming;
    }
    RUNNING.load(Ordering::Relaxed) && RING.mask.load(Ordering::Relaxed) != 0
}

pub fn set_ring_buffer(base_physical: u32, size_log2: u32) {
    // The title passes log2(size in bytes) - 3.
    let size_bytes = 8u32.wrapping_shl(size_log2);
    let va = kernel::from_physical(base_physical);
    let dwords = size_bytes / 4;
    RING.base.store(va, Ordering::Relaxed);
    RING.mask.store(dwords.wrapping_sub(1), Ordering::Relaxed);
    RING.read_index.store(0, Ordering::Relaxed);
    RING.dwords.store(dwords, Ordering::Release);
    log_info!(
        "gpu: ring buffer at %08X (physical %08X), %u KB, %u dwords",
        va,
        base_physical,
        size_bytes / 1024,
        dwords
    );
}

pub fn set_read_pointer_write_back(address_physical: u32, block_size_log2: u32) {
    let write_back = kernel::from_physical(address_physical);
    RPTR_WRITE_BACK.store(write_back, Ordering::Relaxed);
    log_info!(
        "gpu: read pointer write-back at %08X (physical %08X, block 2^%u)",
        write_back,
        address_physical,
        block_size_log2
    );
}

pub fn set_interrupt_callback(callback: u32, context: u32) {
    INTERRUPT.store(u64::from(callback) << 32 | u64::from(context), Ordering::Release);
    log_info!("gpu: graphics interrupt callback %08X (context %08X)", callback, context);
}

/// After each of the title's presents.
pub fn presented() {
    #[cfg(feature = "diagnostics")]
    calls::census_present();
}

/// VdSwap: one frame submitted.
pub fn swap() {
    FRAMES.fetch_add(1, Ordering::SeqCst);
}

/// Frames the title has submitted (VdSwap calls).
pub fn frame_count() -> u64 {
    #[cfg(mw2_hook_rig)]
    if let Some(frames) = rig::frames() {
        return frames;
    }
    FRAMES.load(Ordering::Relaxed)
}

/// The end-of-run report, after shutdown; on a fault, from atomics alone.
#[cfg(feature = "diagnostics")]
pub fn report() {
    calls::imagepool_report();
    if presenter::running() {
        log_info!("gpu: %llu frames reached the window", presenter::presented_frames());
    }
    calls::census_report();
    calls::renderer_report();
    calls::textures_report();
    calls::watch_report();
    #[cfg(not(mw2_gpu_rig))]
    crate::hooks::report_d3d_hooks();
    let (read, written, _) = ring_position();
    log_info!(
        "gpu: ring write %u, read %u (%u dwords outstanding)",
        written,
        read,
        written.wrapping_sub(read) & RING.mask.load(Ordering::Relaxed)
    );
    log_info!("gpu: %llu frames, %llu vblanks", FRAMES.load(Ordering::SeqCst), VBLANKS.load(Ordering::Relaxed));
    let (started, stopped) = (CONSUMER_STARTED.load(Ordering::Relaxed), CONSUMER_STOPPED.load(Ordering::Relaxed));
    if stopped > started {
        let life = stopped.wrapping_sub(started) as f64 / 1e6;
        let busy = CONSUMER_BUSY_NS.load(Ordering::Relaxed) as f64 / 1e6;
        log_info!(
            "gpu: the ring consumer ran %.0f ms: %.0f ms executing the ring (%.0f%%), %.0f ms with nothing to read",
            life,
            busy,
            100.0 * busy / life,
            life - busy
        );
    }
    calls::cp_report();
    super::arena::report();
    calls::stream_report();
}

// tools/hook_diff.py: what the C++ hooks' stand-ins for gpu::FrameCount and
// gpu::Consuming return, given to the Rust hooks too.
#[cfg(mw2_hook_rig)]
pub mod rig {
    use std::sync::atomic::{AtomicU64, Ordering};

    // 0 unset; else the frame count plus one, and the consuming flag plus one.
    static FRAMES: AtomicU64 = AtomicU64::new(0);
    static CONSUMING: AtomicU64 = AtomicU64::new(0);

    #[allow(dead_code)]
    pub fn set(frames: u64, consuming: bool) {
        FRAMES.store(frames.wrapping_add(1), Ordering::Relaxed);
        CONSUMING.store(u64::from(consuming) + 1, Ordering::Relaxed);
    }

    pub fn frames() -> Option<u64> {
        let frames = FRAMES.load(Ordering::Relaxed);
        (frames != 0).then(|| frames - 1)
    }

    pub fn consuming() -> Option<bool> {
        let consuming = CONSUMING.load(Ordering::Relaxed);
        (consuming != 0).then_some(consuming == 2)
    }
}

// tools/gpu_diff.py --rig interrupts: the raising of interrupts, as the consumer and the
// vblank do it, on a thread made as they are.
#[cfg(mw2_gpu_rig)]
pub mod gpu_rig {
    use super::*;

    /// Runs `body` on a thread spawned as the consumer is, and waits for it.
    pub fn on_consumer_thread(body: impl FnOnce() + Send + 'static) {
        let _ = spawn("gpu ring consumer", body).join();
    }

    pub fn set_thread_pcr(pcr: u32) {
        GUEST_PCR.set(pcr);
    }

    pub fn blank() {
        super::blank();
    }

    /// The consumer's loop and the vblank's, on the rig's clock, until the rig stops them.
    pub fn worker(pcr: u32) {
        RUNNING.store(true, Ordering::SeqCst);
        super::worker(pcr);
    }

    pub fn vblank_worker(pcr: u32) {
        RUNNING.store(true, Ordering::SeqCst);
        super::vblank_worker(pcr);
    }

    pub fn stop() {
        RUNNING.store(false, Ordering::SeqCst);
    }

    /// No ring and no write-back, as before VdInitializeRingBuffer.
    pub fn forget_ring() {
        for field in [&RING.base, &RING.dwords, &RING.mask, &RING.read_index, &RPTR_WRITE_BACK] {
            field.store(0, Ordering::SeqCst);
        }
    }

    /// What the end-of-run report counts of the loops: the blanks, and the consumer's
    /// time in batches.
    pub fn counts() -> (u64, u64) {
        (VBLANKS.load(Ordering::SeqCst), CONSUMER_BUSY_NS.load(Ordering::SeqCst))
    }
}
