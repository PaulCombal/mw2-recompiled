// The frame's Vulkan commands, recorded on a thread of their own
// (runtime/gpu/vulkan/recorder.{h,cpp}).
//
// The ring consumer parses the title's command stream and turns each draw into Vulkan
// calls; what has to stay where the stream is stays, and the calls themselves are
// queued, each a closure owning a copy of its arguments, for a second thread that makes
// them in the order they were queued. The submission is queued the same way, and so is
// everything that has to follow it (host). MW2_RECORD_THREAD=0 makes every call where
// it is queued, on the consumer.
//
// What a queued call names has to outlive it: everything that destroys a Vulkan object
// the frame may use, or waits for a submission's fence, first waits for the recorder to
// get that far (drain, or wait_for a position).
//
// One ring of bytes, one producer (the ring consumer; the exit thread at stop, once the
// consumer is joined) and one reader (the recorder). A call is a 32-byte header and the
// closure, placed at the end of the ring and made visible by moving `written` past it;
// the recorder makes it and moves `made` past it. A call that would run off the end of
// the ring starts again at the beginning, behind a header that says so.
//
// The recorder works in bursts. It sleeps while fewer than WAKE_BYTES are queued and
// none of it is due, and the producer wakes it once that many are waiting, or at once
// for a call that has to reach the GPU (host), when a thread waits for the recorder
// (wait_made), and when the consumer has had nothing to read for a while (flush). A
// wake is a system call, so it is not made per call either.
//
// Sleeping and waking are Dekker's: each side stores its own flag SeqCst and then loads
// the other's SeqCst, so one of the two sees the other. Every park sits in a loop on its
// own condition, which its waker sets before it unparks (the plan's park rule): the
// recorder's sleep waits for `wakes` to move, which ring() moves before it unparks, and
// a waiter for `made` to reach its position, which the recorder stores before it looks
// for waiters. A stray unpark (another primitive's token on the same thread) costs one
// more look.
//
// Unsafe by nature: the ring holds closures as raw bytes, moved in by ptr::write and
// out, once, by ptr::read on the recorder (the trampoline), and the producer's fields
// are the producer's alone (Producer).
#![deny(unsafe_op_in_unsafe_fn)]

use super::device::Vk;
#[cfg(feature = "diagnostics")]
use super::trace;
use crate::log::log_info;
use crate::runtime::{clock, crash, platform, report};
use ash::vk;
use std::alloc::{self, Layout};
use std::cell::UnsafeCell;
use std::mem::{align_of, size_of};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::thread::{self, JoinHandle, Thread};

// A frame queues about 2 MB; this is several frames.
const BYTES: u64 = 16 << 20;
const MASK: u64 = BYTES - 1;
// Pauses before the consumer sleeps waiting for the recorder: tens of microseconds. The
// recorder does not spin.
const SPINS: u32 = 4000;
// Queued bytes that wake a sleeping recorder: a hundred or so draws.
const WAKE_BYTES: u64 = 64 << 10;
const HEADER: usize = size_of::<Header>();

/// What a queued call is given when it is made: the command buffer `into` named when it
/// was queued, and the device's tables.
pub struct Context<'a> {
    pub into: vk::CommandBuffer,
    pub vk: &'a Vk,
}

/// Run before every call that was not queued as joining: what joining calls held back,
/// it makes (the renderer's MakeGatheredDraws).
pub type Settle = fn(&Context);

#[repr(C, align(32))]
struct Header {
    run: Option<unsafe fn(*mut u8, &Context)>, // None: skip to the start
    into: vk::CommandBuffer,
    bytes: u32, // the header's and the call's, to the next header
    joins: u32, // queued as joining: settle is not run before it
}
const _: () = assert!(size_of::<Header>() == 32);

/// The closure at `at`, moved out and called: it is dropped once, here.
///
/// # Safety
/// `at` holds an `F` that ptr::write put there and nothing has read since.
unsafe fn run<F: FnOnce(&Context)>(at: *mut u8, context: &Context) {
    // SAFETY: the caller's.
    let call = unsafe { ptr::read(at.cast::<F>()) };
    call(context)
}

// The producer's: the ring consumer's while it runs, the exit thread's at stop once the
// consumer is joined, never two threads' at once.
struct Producer {
    ring: *mut u8,
    reserved: u64,   // queued, not yet published
    made_seen: u64,  // `made` as last read, which only grows
    woken_from: u64, // the sleep (its slept_at) already woken for gathered calls
    threaded: bool,
    into: vk::CommandBuffer,
    vk: *const Vk,
    settle: Option<Settle>,
}

struct ProducerCell(UnsafeCell<Producer>);
// SAFETY: the single-owner rule above.
unsafe impl Sync for ProducerCell {}

/// # Safety
/// The calling thread is the producer (Producer's rule), and holds no other reference.
#[inline(always)]
unsafe fn producer() -> &'static mut Producer {
    // SAFETY: the caller's contract.
    unsafe { &mut *PRODUCER.0.get() }
}

static PRODUCER: ProducerCell = ProducerCell(UnsafeCell::new(Producer {
    ring: ptr::null_mut(),
    reserved: 0,
    made_seen: 0,
    woken_from: !0,
    threaded: false,
    into: vk::CommandBuffer::null(),
    vk: ptr::null(),
    settle: None,
}));

// Each side's own fields on lines of their own, so neither side's writes take the other's
// cache line away from it on every call.
#[repr(C, align(64))]
struct Signals {
    written: AtomicU64,
    // Calls up to here are due now: a submission among them, or a thread waiting for
    // them. Only moves forward.
    urgent: AtomicU64,
    wakes: AtomicU32, // what a sleeping recorder waits on
    asleep: AtomicBool,
    slept_at: AtomicU64, // `made` when it went to sleep
}

#[repr(C, align(64))]
struct Made {
    made: AtomicU64,
    waiters: AtomicU32, // threads waiting on `made`
}

// The recorder's.
#[repr(C, align(64))]
struct Own {
    stopping: AtomicBool,
    calls: AtomicU64,
    sleeps: AtomicU64,
}

// Single-writer counters the report reads: the producer's.
#[repr(C, align(64))]
struct Waits {
    room_waits: AtomicU64,
    waits: AtomicU64,
}

static SIGNALS: Signals = Signals {
    written: AtomicU64::new(0),
    urgent: AtomicU64::new(0),
    wakes: AtomicU32::new(0),
    asleep: AtomicBool::new(false),
    slept_at: AtomicU64::new(0),
};
static MADE: Made = Made { made: AtomicU64::new(0), waiters: AtomicU32::new(0) };
static OWN: Own = Own { stopping: AtomicBool::new(false), calls: AtomicU64::new(0), sleeps: AtomicU64::new(0) };
static WAITS: Waits = Waits { room_waits: AtomicU64::new(0), waits: AtomicU64::new(0) };
// Whether the thread runs, for the calls any thread may make (flush, position, done: the
// trace reads position from the pipeline workers); the producer's own copy is for push.
static THREADED: AtomicBool = AtomicBool::new(false);
// The recorder, for ring(); the one thread waiting on `made` (the consumer, or the exit
// thread once the consumer is joined), for the recorder; the recorder's join handle.
static RECORDER: Mutex<Option<Thread>> = Mutex::new(None);
static WAITER: Mutex<Option<Thread>> = Mutex::new(None);
static JOIN: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
// The settle start was last given, which the recorder reads before every call not queued
// as joining, as the C++ recorder reads detail::settle: a start while threaded changes it
// for every call queued after it (their `written` publishes the store) and maybe for some
// queued before it and not yet made, as in the C++. Null for none.
static RECORDER_SETTLE: AtomicPtr<()> = AtomicPtr::new(ptr::null_mut());

#[inline(always)]
fn recorder_settle() -> Option<Settle> {
    let settle = RECORDER_SETTLE.load(Ordering::Relaxed);
    // SAFETY: null or a Settle that start stored; Option<fn> is a pointer with null as None.
    unsafe { std::mem::transmute::<*mut (), Option<Settle>>(settle) }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn ring_layout() -> Layout {
    // 16 MB aligned to 64: a valid layout.
    Layout::from_size_align(BYTES as usize, 64).unwrap_or_else(|_| unreachable!())
}

// What the recorder thread is handed at its start.
struct Handed {
    ring: *mut u8,
    vk: *const Vk,
}
// SAFETY: the ring is the recorder's to read behind `written`, which the producer
// publishes with SeqCst (a release) and the recorder loads with Acquire; the tables are
// immutable after start (Vk holds function pointers and handles only).
unsafe impl Send for Handed {}

fn record_loop(handed: Handed) {
    let Handed { ring, vk } = handed;
    // Named for ring() before the first look at `wakes`: a ring() that took the lock before
    // this moved `wakes` first, so the sleep below finds it moved (the C++ waits on the
    // futex itself and has no such window).
    *lock(&RECORDER) = Some(thread::current());
    crash::register_thread(c"recorder");
    #[cfg(feature = "diagnostics")]
    trace::recorder_thread();
    // SAFETY: start's contract: the tables outlive stop, which joins this thread.
    let vk = unsafe { &*vk };
    let s = &SIGNALS;
    let mut context = Context { into: vk::CommandBuffer::null(), vk };
    let mut at = MADE.made.load(Ordering::Relaxed);
    let mut calls: u64 = 0;
    // Whether what is queued past `at` has to be made now.
    let due = |end: u64, at: u64| end.wrapping_sub(at) >= WAKE_BYTES || s.urgent.load(Ordering::SeqCst) > at;
    while !OWN.stopping.load(Ordering::Relaxed) {
        let end = s.written.load(Ordering::Acquire);
        if !due(end, at) {
            OWN.calls.store(calls, Ordering::Relaxed);
            // Asleep before looking once more; the producer publishes before it looks at
            // this, so one of the two sees the other. It waits on `wakes`, which ring()
            // moves before it unparks: a stray token finds it unmoved and parks again.
            let seen = s.wakes.load(Ordering::SeqCst);
            s.slept_at.store(at, Ordering::Relaxed);
            s.asleep.store(true, Ordering::SeqCst);
            if !due(s.written.load(Ordering::SeqCst), at) {
                OWN.sleeps.store(OWN.sleeps.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
                while s.wakes.load(Ordering::SeqCst) == seen {
                    thread::park();
                }
            }
            s.asleep.store(false, Ordering::Relaxed);
            continue;
        }
        while at != end {
            // SAFETY: `at` is behind `written`, so a header the producer wrote and
            // published; the mask keeps it in the ring, at a multiple of 32. Read through
            // the pointer, after the call too: nothing is kept across it.
            let header = unsafe { ring.add((at & MASK) as usize) }.cast::<Header>();
            // SAFETY: as above.
            if unsafe { (*header).run }.is_some() {
                // SAFETY: as above.
                let (into, joins) = unsafe { ((*header).into, (*header).joins) };
                context.into = into;
                if joins == 0
                    && let Some(settle) = recorder_settle()
                {
                    settle(&context);
                }
                // Read again after settle, as the C++ does: held across that call, the
                // trampoline would take a register the loop needs.
                // SAFETY: as above; the closure behind the header, written by push and read
                // once.
                if let Some(call) = unsafe { (*header).run } {
                    unsafe { call(header.cast::<u8>().add(HEADER), &context) };
                }
            }
            // SAFETY: as above.
            at += u64::from(unsafe { (*header).bytes });
            calls += 1;
            // The consumer's trace lines that come after this call (trace.rs).
            #[cfg(feature = "diagnostics")]
            if trace::on() {
                trace::recorder_made(at);
            }
            MADE.made.store(at, Ordering::SeqCst);
            if MADE.waiters.load(Ordering::SeqCst) != 0 {
                wake_waiter();
            }
        }
    }
    OWN.calls.store(calls, Ordering::Relaxed);
}

#[cold]
#[inline(never)]
fn wake_waiter() {
    if let Some(waiter) = lock(&WAITER).as_ref() {
        waiter.unpark();
    }
}

fn raise_urgent(to: u64) {
    let mut was = SIGNALS.urgent.load(Ordering::Relaxed);
    while was < to {
        match SIGNALS.urgent.compare_exchange_weak(was, to, Ordering::SeqCst, Ordering::Relaxed) {
            Ok(_) => break,
            Err(now) => was = now,
        }
    }
}

#[cold]
#[inline(never)]
fn ring() {
    SIGNALS.wakes.fetch_add(1, Ordering::SeqCst);
    if let Some(recorder) = lock(&RECORDER).as_ref() {
        recorder.unpark();
    }
}

// Everything queued so far is due now, and a sleeping recorder is woken for it. Any
// thread.
fn wake() {
    raise_urgent(SIGNALS.written.load(Ordering::SeqCst));
    if SIGNALS.asleep.load(Ordering::SeqCst) {
        ring();
    }
}

#[cold]
#[inline(never)]
fn wait_made(position: u64) {
    let made = &MADE.made;
    if made.load(Ordering::Acquire) >= position {
        return;
    }
    wake();
    for _ in 0..SPINS {
        std::hint::spin_loop();
        if made.load(Ordering::Acquire) >= position {
            return;
        }
    }
    *lock(&WAITER) = Some(thread::current());
    MADE.waiters.fetch_add(1, Ordering::SeqCst);
    while made.load(Ordering::SeqCst) < position {
        thread::park();
    }
    MADE.waiters.fetch_sub(1, Ordering::Relaxed);
}

/// The room-wait half of reserve: the queue is full up to `end`.
#[cold]
#[inline(never)]
fn wait_for_room(p: &mut Producer, end: u64) {
    WAITS.room_waits.store(WAITS.room_waits.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
    let began = report::on().then(|| report::timed_begin(clock::nanoseconds(), report::RECORDER_WAIT));
    wait_made(end - BYTES);
    if let Some(began) = began {
        report::timed_end(clock::nanoseconds(), report::RECORDER_WAIT, began);
    }
    p.made_seen = MADE.made.load(Ordering::Acquire);
}

// Room for `bytes` at the end of the queue.
#[inline(always)]
fn reserve(p: &mut Producer, bytes: u32) -> *mut u8 {
    let mut at = p.reserved;
    let mut offset = at & MASK;
    let pad = if offset + u64::from(bytes) > BYTES { BYTES - offset } else { 0 };
    let end = at + pad + u64::from(bytes);
    if end - p.made_seen > BYTES {
        p.made_seen = MADE.made.load(Ordering::Acquire);
        if end - p.made_seen > BYTES {
            wait_for_room(p, end);
        }
    }
    if pad != 0 {
        // SAFETY: offset is a multiple of 32 inside the ring, with room for a header (a
        // record is at least one); the recorder has made what was there.
        unsafe {
            p.ring.add(offset as usize).cast::<Header>().write(Header {
                run: None,
                into: vk::CommandBuffer::null(),
                bytes: pad as u32,
                joins: 0,
            })
        };
        at += pad;
        offset = 0;
    }
    p.reserved = at + u64::from(bytes);
    // SAFETY: offset is inside the ring.
    unsafe { p.ring.add(offset as usize) }
}

// Making what was reserved visible: `now` wakes the recorder for it whatever else is
// queued.
#[inline(always)]
fn publish(p: &mut Producer, now: bool) {
    let s = &SIGNALS;
    s.written.store(p.reserved, Ordering::SeqCst);
    if now {
        raise_urgent(p.reserved);
    }
    if !s.asleep.load(Ordering::SeqCst) {
        return;
    }
    let slept = s.slept_at.load(Ordering::Relaxed);
    if now || (p.reserved - slept >= WAKE_BYTES && p.woken_from != slept) {
        // Until it clears its flag the recorder still looks asleep: one wake a sleep is
        // enough.
        p.woken_from = slept;
        ring();
    }
}

#[inline(always)]
fn push<F: FnOnce(&Context) + Send + 'static>(call: F, now: bool, joins: bool) {
    // SAFETY: only the producer queues calls.
    let p = unsafe { producer() };
    if !p.threaded {
        // The call moved to a slot of its own, made through its trampoline: handing the
        // call itself to a function would keep it in memory on the threaded path too, built
        // there and copied into the ring, where it can be built in place.
        let mut slot = std::mem::MaybeUninit::<F>::uninit();
        slot.write(call);
        inline(p, slot.as_mut_ptr().cast(), run::<F>, joins);
        return;
    }
    const { assert!(align_of::<F>() <= align_of::<Header>()) };
    let bytes = const { ((HEADER + size_of::<F>() + HEADER - 1) & !(HEADER - 1)) as u32 };
    let at = reserve(p, bytes);
    // SAFETY: reserve gave `bytes` at a multiple of 32 in the ring, which the recorder
    // reads only once `written` passes them; F's alignment is at most 32.
    unsafe {
        at.cast::<Header>().write(Header { run: Some(run::<F>), into: p.into, bytes, joins: u32::from(joins) });
        at.add(HEADER).cast::<F>().write(call);
    }
    publish(p, now);
}

#[cold]
#[inline(never)]
fn inline(p: &mut Producer, at: *mut u8, call: unsafe fn(*mut u8, &Context), joins: bool) {
    debug_assert!(!p.vk.is_null(), "a call queued before the recorder's start");
    // SAFETY: start's contract: the tables outlive stop.
    let context = Context { into: p.into, vk: unsafe { &*p.vk } };
    if !joins && let Some(settle) = p.settle {
        settle(&context);
    }
    // SAFETY: push's slot, holding the call, read once here.
    unsafe { call(at, &context) };
}

/// `call` with the command buffer `into` last named, on the recorder thread, after every
/// call queued before it. It owns what it uses: it runs later.
#[inline]
pub fn command<F: FnOnce(&Context) + Send + 'static>(call: F) {
    push(call, false, false);
}

/// A call that may hold its work back to make it together with the joining calls after
/// it: the renderer's draws, made as one multi-draw while nothing changes between them.
/// Whatever it holds back, settle makes before the next call that is not joining, so a
/// held draw is made only once a call after it is: done and drain do not wait for it.
#[inline]
pub fn joining<F: FnOnce(&Context) + Send + 'static>(call: F) {
    push(call, false, true);
}

/// Work that is not a command but has to come after the ones before it: a submission,
/// the frame handed to the window. It wakes the recorder.
#[inline]
pub fn host<F: FnOnce() + Send + 'static>(call: F) {
    push(move |_: &Context| call(), true, false);
}

/// At the renderer's start: Stop runs what is queued first. `settle`, when given, runs on
/// the recorder before every call that was not queued as joining; a start while threaded
/// replaces it, as the C++'s does, for the calls queued after it and for any queued before
/// it that the recorder has not made yet (drain first to say which). `vk` is the recorder's from
/// the start that made the thread until stop (the C++ has one global table): a start while
/// threaded changes it only for the calls made inline after stop.
///
/// # Safety
/// The calling thread is the producer, and `vk` stays valid until stop has returned.
pub unsafe fn start(vk: *const Vk, settle: Option<Settle>) {
    // SAFETY: the caller's.
    let p = unsafe { producer() };
    p.settle = settle;
    RECORDER_SETTLE.store(settle.map_or(ptr::null_mut(), |settle| settle as *mut ()), Ordering::Relaxed);
    p.vk = vk;
    if p.threaded {
        return;
    }
    static OFF: LazyLock<bool> = LazyLock::new(|| {
        cfg!(feature = "diagnostics")
            && crate::env::text(c"MW2_RECORD_THREAD").is_some()
            && !crate::env::flag(c"MW2_RECORD_THREAD")
    });
    if *OFF {
        log_info!("recorder: MW2_RECORD_THREAD=0, the consumer records its own commands");
        return;
    }
    // SAFETY: a layout of non-zero size.
    let ring = unsafe { alloc::alloc(ring_layout()) };
    if ring.is_null() {
        alloc::handle_alloc_error(ring_layout());
    }
    p.ring = ring;
    p.reserved = 0;
    p.made_seen = 0;
    SIGNALS.written.store(0, Ordering::SeqCst);
    MADE.made.store(0, Ordering::SeqCst);
    SIGNALS.urgent.store(0, Ordering::SeqCst);
    SIGNALS.slept_at.store(0, Ordering::SeqCst);
    p.woken_from = !0;
    OWN.stopping.store(false, Ordering::Relaxed);
    p.threaded = true;
    THREADED.store(true, Ordering::Relaxed);
    #[cfg(feature = "diagnostics")]
    trace::recorder_started();
    let handed = Handed { ring, vk };
    let handle = thread::Builder::new()
        .name("recorder".into())
        .stack_size(platform::default_thread_stack())
        .spawn(move || record_loop(handed))
        .unwrap_or_else(|_| {
            // A std::thread that cannot start ends the program.
            crate::log::log_error!("recorder: the recorder thread would not start");
            std::process::abort()
        });
    *lock(&JOIN) = Some(handle);
}

/// At the renderer's end: what is queued is made first.
///
/// # Safety
/// The calling thread is the producer (the consumer, or the exit thread once the consumer
/// is joined).
pub unsafe fn stop() {
    // SAFETY: the caller's.
    if !unsafe { producer() }.threaded {
        return;
    }
    host(|| OWN.stopping.store(true, Ordering::Relaxed));
    if let Some(handle) = lock(&JOIN).take() {
        let _ = handle.join();
    }
    #[cfg(feature = "diagnostics")]
    trace::recorder_stopped();
    *lock(&RECORDER) = None;
    // SAFETY: the caller's; the recorder is gone.
    let p = unsafe { producer() };
    p.threaded = false;
    THREADED.store(false, Ordering::Relaxed);
    // SAFETY: start's allocation, with its layout; nothing reads it any more.
    unsafe { alloc::dealloc(p.ring, ring_layout()) };
    p.ring = ptr::null_mut();
}

/// Whether the calls are made on the recorder thread.
pub fn threaded() -> bool {
    THREADED.load(Ordering::Relaxed)
}

/// The command buffer the calls queued from here on go into. The producer's.
pub fn into(command: vk::CommandBuffer) {
    // SAFETY: the producer writes its own field.
    unsafe { producer() }.into = command;
}

/// Where the queue has got to: the position just past the last call queued; 0 when
/// there is no thread. Any thread (the trace's lines on the pipeline workers): it takes no
/// reference to the producer's fields.
#[inline]
pub fn position() -> u64 {
    if THREADED.load(Ordering::Relaxed) { SIGNALS.written.load(Ordering::Acquire) } else { 0 }
}

/// Wakes the recorder for whatever is queued, which it otherwise lets gather: for when
/// nothing more is coming for a while. Any thread.
pub fn flush() {
    if THREADED.load(Ordering::Relaxed) {
        wake();
    }
}

/// Whether the recorder has made every call before `position`; true with no thread.
#[inline]
pub fn done(position: u64) -> bool {
    !THREADED.load(Ordering::Relaxed) || MADE.made.load(Ordering::Acquire) >= position
}

/// Waits until the recorder has made every call before `position`.
pub fn wait_for(position: u64) {
    if done(position) {
        return;
    }
    WAITS.waits.store(WAITS.waits.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
    let began = report::on().then(|| report::timed_begin(clock::nanoseconds(), report::RECORDER_WAIT));
    wait_made(position);
    if let Some(began) = began {
        report::timed_end(clock::nanoseconds(), report::RECORDER_WAIT, began);
    }
}

/// Waits until the recorder has made every call queued.
pub fn drain() {
    wait_for(position());
}

pub fn report() {
    let calls = OWN.calls.load(Ordering::SeqCst);
    if calls == 0 {
        return;
    }
    let room_waits = WAITS.room_waits.load(Ordering::SeqCst);
    log_info!(
        "recorder: %llu calls made on its own thread; it slept %llu times; the consumer waited for it %llu times, %llu of them for room in the queue",
        calls,
        OWN.sleeps.load(Ordering::SeqCst),
        WAITS.waits.load(Ordering::SeqCst) + room_waits,
        room_waits
    );
}

// tools/gpu_diff.py --rig recorder: the rig's script (tools/gpu_diff/recorder.cpp says
// what each op does) run on the Rust recorder with Rust closures, each a payload of one
// of the rig's sizes that checks, when it is made, that it comes in queue order, into the
// command buffer named when it was queued, behind settle's run, with its bytes intact,
// and that counts its drop; the counters and stray tokens; and the benchmark's loop.
#[cfg(mw2_gpu_rig)]
pub mod rig {
    use super::*;
    use std::sync::atomic::AtomicPtr;
    use vk::Handle;

    #[repr(C)]
    pub struct Op {
        kind: u32,
        a: u32,
        b: u64,
    }

    const COMMAND: u32 = 0;
    const JOINING: u32 = 1;
    const HOST: u32 = 2;
    const INTO: u32 = 3;
    const DONE: u32 = 4;
    const WAIT_FOR: u32 = 5;
    const DRAIN: u32 = 6;
    const SPIN: u32 = 7;
    const HOST_POLL: u32 = 8;
    const BIG_POLL: u32 = 9;
    const FLUSH: u32 = 10;
    const STALL: u32 = 11;
    const RACE: u32 = 12;
    const STALL_WAIT: u32 = 13;
    const FLUSH_POLL: u32 = 14;
    const RESTART: u32 = 15;

    // Errors, as the C++ half numbers them.
    const ORDER: u64 = 1;
    const INTO_WRONG: u64 = 2;
    const SETTLE: u64 = 4;
    const BYTES_WRONG: u64 = 8;
    const DONE_EARLY: u64 = 16;
    const WAIT_EARLY: u64 = 32;
    const DRAIN_EARLY: u64 = 64;
    const LOST_WAKE: u64 = 128;
    const DROPS: u64 = 256;
    const STOP_EARLY: u64 = 512;

    static RAN: AtomicU64 = AtomicU64::new(0);
    static SETTLES: AtomicU64 = AtomicU64::new(0);
    static ERRORS: AtomicU64 = AtomicU64::new(0);
    static FIRST_ERROR_AT: AtomicU64 = AtomicU64::new(u64::MAX);
    static DROPPED: AtomicPtr<u8> = AtomicPtr::new(ptr::null_mut());
    static ZST: AtomicPtr<u8> = AtomicPtr::new(ptr::null_mut());
    static SETTLE_ON: AtomicBool = AtomicBool::new(false);

    fn fail(error: u64, at: u64) {
        ERRORS.fetch_or(error, Ordering::Relaxed);
        let _ = FIRST_ERROR_AT.compare_exchange(u64::MAX, at, Ordering::Relaxed, Ordering::Relaxed);
    }

    fn settle(_: &Context) {
        SETTLES.store(SETTLES.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
    }

    // A call of 32 + 8 * N bytes: its id (its place in the queue), the command buffer it
    // should be made into, how many non-joining calls were queued up to it, and words
    // that say its id.
    #[repr(C)]
    struct Payload<const N: usize> {
        id: u64,
        into: u64,
        nonjoin: u64,
        check: u64,
        words: [u64; N],
    }

    impl<const N: usize> Payload<N> {
        fn new(id: u64, into: u64, nonjoin: u64) -> Self {
            let mut words = [0u64; N];
            if N > 0 {
                words[0] = id ^ 0x5555;
                words[N - 1] = id ^ 0xAAAA;
            }
            Payload { id, into, nonjoin, check: id.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ into, words }
        }

        fn made(&self, into: Option<u64>, joins: bool) {
            let ran = RAN.load(Ordering::Relaxed);
            if ran != self.id {
                fail(ORDER, self.id);
            }
            if let Some(into) = into
                && into != self.into
            {
                fail(INTO_WRONG, self.id);
            }
            if SETTLE_ON.load(Ordering::Relaxed) && !joins && SETTLES.load(Ordering::Relaxed) != self.nonjoin {
                fail(SETTLE, self.id);
            }
            if self.check != self.id.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ self.into
                || (N > 0 && (self.words[0] != self.id ^ 0x5555 || self.words[N - 1] != self.id ^ 0xAAAA))
            {
                fail(BYTES_WRONG, self.id);
            }
            RAN.store(ran + 1, Ordering::Release);
        }
    }

    impl<const N: usize> Drop for Payload<N> {
        fn drop(&mut self) {
            // SAFETY: the run's array, one byte per call.
            unsafe {
                let at = DROPPED.load(Ordering::Relaxed).add(self.id as usize);
                *at = (*at).wrapping_add(1);
            }
        }
    }

    fn zst_made(id: u64) {
        let ran = RAN.load(Ordering::Relaxed);
        // SAFETY: the run's array, one byte per call.
        if ran != id || unsafe { *ZST.load(Ordering::Relaxed).add(ran as usize) } == 0 {
            fail(ORDER, id);
        }
        RAN.store(ran + 1, Ordering::Release);
    }

    fn queue<const N: usize>(kind: u32, id: u64, into: u64, nonjoin: u64) {
        let payload = Payload::<N>::new(id, into, nonjoin);
        match kind {
            COMMAND => command(move |c: &Context| payload.made(Some(c.into.as_raw()), false)),
            JOINING => joining(move |c: &Context| payload.made(Some(c.into.as_raw()), true)),
            _ => host(move || payload.made(None, false)),
        }
    }

    // The rig's sizes (tools/gpu_diff/recorder.cpp's kWords): index 0 a closure of no
    // bytes, the others a payload of that many words.
    fn queue_sized(size: u32, kind: u32, id: u64, into: u64, nonjoin: u64) {
        macro_rules! sizes {
            ($($index:literal => $words:literal),*) => {
                match size {
                    $($index => queue::<$words>(kind, id, into, nonjoin),)*
                    _ => match kind {
                        COMMAND => command(move |_: &Context| zst_made(id_of())),
                        JOINING => joining(move |_: &Context| zst_made(id_of())),
                        _ => host(move || zst_made(id_of())),
                    },
                }
            };
        }
        sizes!(1 => 0, 2 => 4, 3 => 8, 4 => 12, 5 => 20, 6 => 24, 7 => 28, 8 => 60, 9 => 124, 10 => 252,
               11 => 508, 12 => 1020, 13 => 2044, 14 => 4092, 15 => 8184)
    }

    // A closure of no bytes knows no id: it is the next one.
    fn id_of() -> u64 {
        RAN.load(Ordering::Relaxed)
    }

    fn poll(position: u64, at: u64) {
        let began = std::time::Instant::now();
        let mut n = 0u32;
        while !done(position) {
            std::hint::spin_loop();
            n = n.wrapping_add(1);
            if n % 1024 == 0 && began.elapsed().as_secs_f64() > 2.0 {
                // Every poll after it would wait its two seconds too: it ends the run.
                fail(LOST_WAKE, at);
                // SAFETY: the rig's thread is the producer.
                let p = unsafe { producer() };
                println!(
                    "DIFFERENCE: the Rust recorder lost a wake: call {at} not made in 2 s (written {} made {} urgent {} asleep {} slept at {} woken from {} wakes {} reserved {})",
                    SIGNALS.written.load(Ordering::SeqCst),
                    MADE.made.load(Ordering::SeqCst),
                    SIGNALS.urgent.load(Ordering::SeqCst),
                    SIGNALS.asleep.load(Ordering::SeqCst),
                    SIGNALS.slept_at.load(Ordering::SeqCst),
                    p.woken_from,
                    SIGNALS.wakes.load(Ordering::SeqCst),
                    p.reserved
                );
                std::process::exit(4);
            }
        }
    }

    /// Runs `script` on the Rust recorder, then drains before stop or, without
    /// `drain_first`, stops with the calls still queued: errors, the first failing call,
    /// and the counters (calls, sleeps, waits, room waits, wakes) into `out`.
    ///
    /// # Safety
    /// `script` holds `count` ops; `dropped` and `zst` one byte per call the script
    /// queues, zeroed, `zst` marking the calls of no bytes; `progress` is the rig's.
    #[allow(clippy::too_many_arguments)]
    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_rec_run(
        script: *const Op,
        count: u64,
        with_settle: bool,
        drain_first: bool,
        dropped: *mut u8,
        zst: *mut u8,
        progress: *const AtomicU64,
        out: *mut u64,
    ) {
        // SAFETY: the caller's.
        let (script, progress) = unsafe { (std::slice::from_raw_parts(script, count as usize), &*progress) };
        RAN.store(0, Ordering::SeqCst);
        SETTLES.store(0, Ordering::SeqCst);
        ERRORS.store(0, Ordering::SeqCst);
        FIRST_ERROR_AT.store(u64::MAX, Ordering::SeqCst);
        DROPPED.store(dropped, Ordering::SeqCst);
        ZST.store(zst, Ordering::SeqCst);
        SETTLE_ON.store(with_settle, Ordering::SeqCst);
        // SAFETY: the rig's thread is the producer; the tables are static.
        unsafe { start(super::super::device::rig::tables(), with_settle.then_some(settle as Settle)) };
        let mut queued = 0u64;
        // Non-joining calls queued while a settle is on: settle runs before each of them only.
        let mut nonjoin = 0u64;
        let mut settle_now = with_settle;
        let mut into_now = 0u64;
        into(vk::CommandBuffer::null());
        // The positions just past the last 1024 calls.
        let mut positions = vec![0u64; 1024];
        for op in script {
            progress.fetch_add(1, Ordering::Relaxed);
            match op.kind {
                COMMAND | JOINING | HOST => {
                    if op.kind != JOINING {
                        nonjoin += u64::from(settle_now);
                    }
                    queue_sized(op.a, op.kind, queued, into_now, nonjoin);
                    positions[(queued & 1023) as usize] = position();
                    queued += 1;
                }
                INTO => {
                    into_now = op.b;
                    into(vk::CommandBuffer::from_raw(op.b));
                }
                DONE | WAIT_FOR => {
                    let back = u64::from(op.a);
                    if back < queued && back < 1024 {
                        let call = queued - 1 - back;
                        let at = positions[(call & 1023) as usize];
                        if op.kind == DONE {
                            if done(at) && RAN.load(Ordering::Acquire) <= call {
                                fail(DONE_EARLY, call);
                            }
                        } else {
                            wait_for(at);
                            if RAN.load(Ordering::Acquire) <= call {
                                fail(WAIT_EARLY, call);
                            }
                        }
                    }
                }
                DRAIN => {
                    drain();
                    if RAN.load(Ordering::Acquire) != queued {
                        fail(DRAIN_EARLY, queued);
                    }
                }
                SPIN => {
                    for _ in 0..op.a {
                        std::hint::spin_loop();
                    }
                }
                HOST_POLL | BIG_POLL => {
                    nonjoin += u64::from(settle_now);
                    queue_sized(op.a, if op.kind == HOST_POLL { HOST } else { COMMAND }, queued, into_now, nonjoin);
                    let at = position();
                    positions[(queued & 1023) as usize] = at;
                    queued += 1;
                    poll(at, queued - 1);
                }
                FLUSH => flush(),
                RESTART => {
                    settle_now = with_settle && op.a != 0;
                    drain();
                    // SAFETY: as start above.
                    unsafe { start(super::super::device::rig::tables(), settle_now.then_some(settle as Settle)) };
                }
                FLUSH_POLL => {
                    nonjoin += u64::from(settle_now);
                    queue_sized(op.a, COMMAND, queued, into_now, nonjoin);
                    let at = position();
                    positions[(queued & 1023) as usize] = at;
                    queued += 1;
                    flush();
                    poll(at, queued - 1);
                }
                RACE => {
                    nonjoin += u64::from(settle_now);
                    queue_sized(15, COMMAND, queued, into_now, nonjoin);
                    positions[(queued & 1023) as usize] = position();
                    queued += 1;
                    for _ in 0..op.a {
                        std::hint::spin_loop();
                    }
                    nonjoin += u64::from(settle_now);
                    queue_sized(15, COMMAND, queued, into_now, nonjoin);
                    let at = position();
                    positions[(queued & 1023) as usize] = at;
                    queued += 1;
                    poll(at, queued - 1);
                }
                STALL | STALL_WAIT => {
                    nonjoin += u64::from(settle_now);
                    let payload = Payload::<0>::new(queued, into_now, nonjoin);
                    let spins = op.a;
                    command(move |c: &Context| {
                        for _ in 0..spins {
                            std::hint::spin_loop();
                        }
                        payload.made(Some(c.into.as_raw()), false);
                    });
                    positions[(queued & 1023) as usize] = position();
                    queued += 1;
                    if op.kind == STALL_WAIT {
                        wait_for(position());
                        if RAN.load(Ordering::Acquire) != queued {
                            fail(WAIT_EARLY, queued - 1);
                        }
                    }
                }
                _ => {}
            }
        }
        if drain_first {
            drain();
            if RAN.load(Ordering::Acquire) != queued {
                fail(DRAIN_EARLY, queued);
            }
        }
        // SAFETY: as start.
        unsafe { stop() };
        if RAN.load(Ordering::Acquire) != queued {
            fail(STOP_EARLY, queued);
        }
        for id in 0..queued as usize {
            // SAFETY: one byte per call.
            let (drops, bare) = unsafe { (*dropped.add(id), *zst.add(id)) };
            if bare == 0 && drops != 1 {
                fail(DROPS, id as u64);
                break;
            }
        }
        let counters = counters();
        // SAFETY: the caller's seven places.
        unsafe {
            *out = ERRORS.load(Ordering::SeqCst);
            *out.add(1) = FIRST_ERROR_AT.load(Ordering::SeqCst);
            for (n, value) in counters.iter().enumerate() {
                *out.add(2 + n) = *value;
            }
        }
    }

    /// calls, sleeps, waits, room waits, wakes.
    pub fn counters() -> [u64; 5] {
        [
            OWN.calls.load(Ordering::SeqCst),
            OWN.sleeps.load(Ordering::SeqCst),
            WAITS.waits.load(Ordering::SeqCst),
            WAITS.room_waits.load(Ordering::SeqCst),
            u64::from(SIGNALS.wakes.load(Ordering::SeqCst)),
        ]
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_rig_rec_counters_reset() {
        OWN.calls.store(0, Ordering::SeqCst);
        OWN.sleeps.store(0, Ordering::SeqCst);
        WAITS.waits.store(0, Ordering::SeqCst);
        WAITS.room_waits.store(0, Ordering::SeqCst);
        SIGNALS.wakes.store(0, Ordering::SeqCst);
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_rig_rec_report() {
        report();
    }

    /// A token for the recorder thread and for the thread waiting on `made`, from no
    /// primitive of theirs.
    #[unsafe(no_mangle)]
    extern "C" fn mw2_rig_rec_stray() {
        if let Some(recorder) = lock(&RECORDER).as_ref() {
            recorder.unpark();
        }
        if let Some(waiter) = lock(&WAITER).as_ref() {
            waiter.unpark();
        }
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_rig_rec_flush() {
        flush();
    }

    // ---- the benchmark ------------------------------------------------------------------

    unsafe extern "C" {
        fn rig_counter_open() -> i32;
        fn rig_counter_read(fd: i32) -> u64;
        fn rig_counter_close(fd: i32);
        fn rig_rec_now() -> u64;
    }

    static GATE: AtomicBool = AtomicBool::new(false);
    static SUM: AtomicU64 = AtomicU64::new(0);
    // The recorder's start and end: nanoseconds and instructions.
    static MADE_FROM: AtomicU64 = AtomicU64::new(0);
    static MADE_TO: AtomicU64 = AtomicU64::new(0);
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    static INSTRUCTIONS_FROM: AtomicU64 = AtomicU64::new(0);
    static INSTRUCTIONS_TO: AtomicU64 = AtomicU64::new(0);

    #[derive(Clone, Copy)]
    struct Bench<const W: usize> {
        data: [u64; W],
    }

    #[inline(never)]
    fn bench_round<const W: usize>(calls: u64, out: &mut [f64; 4]) {
        GATE.store(false, Ordering::SeqCst);
        // The gate: the recorder waits in it until the whole round is queued, then is
        // timed from there to the round's last call.
        command(|_: &Context| {
            // SAFETY: the rig's counter of this thread's instructions.
            let fd = unsafe { rig_counter_open() };
            COUNTER.store(fd as u64, Ordering::Relaxed);
            while !GATE.load(Ordering::Acquire) {
                std::hint::spin_loop();
            }
            // SAFETY: as above.
            INSTRUCTIONS_FROM.store(unsafe { rig_counter_read(fd) }, Ordering::Relaxed);
            // SAFETY: the rig's clock.
            MADE_FROM.store(unsafe { rig_rec_now() }, Ordering::Relaxed);
        });
        let template = Bench::<W> { data: [7; W] };
        // SAFETY: the rig's counter of this thread's instructions, and its clock.
        let fd = unsafe { rig_counter_open() };
        let instructions = unsafe { rig_counter_read(fd) };
        let began = unsafe { rig_rec_now() };
        for i in 0..calls {
            let mut b = template;
            b.data[0] = i;
            b.data[W - 1] = i;
            command(move |_: &Context| {
                SUM.store(SUM.load(Ordering::Relaxed).wrapping_add(b.data[0] ^ b.data[W - 1]), Ordering::Relaxed);
            });
        }
        // SAFETY: as above.
        let ended = unsafe { rig_rec_now() };
        let instructions = unsafe { rig_counter_read(fd) } - instructions;
        unsafe { rig_counter_close(fd) };
        command(|_: &Context| {
            // SAFETY: the counter the gate opened, on this thread.
            let fd = COUNTER.load(Ordering::Relaxed) as i32;
            INSTRUCTIONS_TO.store(unsafe { rig_counter_read(fd) }, Ordering::Relaxed);
            MADE_TO.store(unsafe { rig_rec_now() }, Ordering::Relaxed);
            unsafe { rig_counter_close(fd) };
        });
        GATE.store(true, Ordering::Release);
        // Not drain: a thread waiting on `made` makes the recorder wake it after every call.
        let end = position();
        while !done(end) {
            std::thread::sleep(std::time::Duration::from_micros(500));
        }
        out[0] = (ended - began) as f64 / calls as f64;
        out[1] = (MADE_TO.load(Ordering::Relaxed) - MADE_FROM.load(Ordering::Relaxed)) as f64 / calls as f64;
        out[2] = instructions as f64 / calls as f64;
        out[3] =
            (INSTRUCTIONS_TO.load(Ordering::Relaxed) - INSTRUCTIONS_FROM.load(Ordering::Relaxed)) as f64 / calls as f64;
    }

    /// One round of `calls` closures of `bytes` (224 or 64): producer ns per command,
    /// recorder ns per call, and the instructions of each, into `out`.
    ///
    /// # Safety
    /// `out` has four places; the recorder runs (mw2_rig_rec_bench_start).
    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_rec_bench(bytes: u32, calls: u64, out: *mut [f64; 4]) {
        // SAFETY: the caller's.
        let out = unsafe { &mut *out };
        if bytes == 224 {
            bench_round::<28>(calls, out);
        } else {
            bench_round::<8>(calls, out);
        }
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_rig_rec_bench_start() {
        // SAFETY: the rig's thread is the producer; the tables are static.
        unsafe { start(super::super::device::rig::tables(), None) };
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_rig_rec_bench_stop() {
        // SAFETY: as start.
        unsafe { stop() };
    }
}
