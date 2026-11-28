// The frame's Vulkan commands, recorded on a thread of their own
// (docs/rendering.md, "The recorder thread").
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
// own condition, which its waker sets before it unparks: the
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
/// it makes (the renderer's make_gathered_draws). Unsafe: it may own state that only the
/// thread making the calls touches, which only the recorder knows it is on.
pub type Settle = unsafe fn(&Context);

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
    // What woken_from would be if a call due now set it too: the wakes for gathered calls
    // that such a call would have spent are counted (WAKES_KEPT).
    #[cfg(feature = "diagnostics")]
    woken_from_before: u64,
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
    #[cfg(feature = "diagnostics")]
    woken_from_before: !0,
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
// The producer's, diagnostics: wakes for gathered calls that would not have been made had
// a call due now spent the sleep's wake.
#[cfg(feature = "diagnostics")]
static WAKES_KEPT: AtomicU64 = AtomicU64::new(0);
// Whether the thread runs, for the calls any thread may make (flush, position, done: the
// trace reads position from the pipeline workers); the producer's own copy is for push.
static THREADED: AtomicBool = AtomicBool::new(false);
// The recorder, for ring(); the one thread waiting on `made` (the consumer, or the exit
// thread once the consumer is joined), for the recorder; the recorder's join handle.
static RECORDER: Mutex<Option<Thread>> = Mutex::new(None);
static WAITER: Mutex<Option<Thread>> = Mutex::new(None);
static JOIN: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
// The settle start was last given, which the recorder reads before every call not queued
// as joining: a start while threaded changes it for every call queued after it (their
// `written` publishes the store) and maybe for some queued before it and not yet made.
// Null for none.
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
    // this moved `wakes` first, so the sleep below finds it moved.
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
                    // SAFETY: the recorder thread, which makes the calls, with their context.
                    unsafe { settle(&context) };
                }
                // Read again after settle: held across that call, the
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
    if now {
        // Due now, whatever was woken before. It leaves the sleep's wake for gathered calls
        // alone: the recorder may already have made this call and gone to sleep again at
        // the same place before the flag was read here, and that sleep's gathered calls
        // would then wait for a flush, a wait or the next call due now.
        #[cfg(feature = "diagnostics")]
        {
            p.woken_from_before = slept;
        }
        ring();
    } else if p.reserved - slept >= WAKE_BYTES && p.woken_from != slept {
        // Until it clears its flag the recorder still looks asleep: one wake a sleep is
        // enough.
        p.woken_from = slept;
        #[cfg(feature = "diagnostics")]
        {
            if p.woken_from_before == slept {
                WAKES_KEPT.store(WAKES_KEPT.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
            }
            p.woken_from_before = slept;
        }
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
        // SAFETY: inline, this thread makes the calls, with their context.
        unsafe { settle(&context) };
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

/// At the renderer's start: stop runs what is queued first. `settle`, when given, runs on
/// the recorder before every call that was not queued as joining; a start while threaded
/// replaces it for the calls queued after it and for any queued before it that the
/// recorder has not made yet (drain first to say which). `vk` is the recorder's from
/// the start that made the thread until stop: a start while threaded changes it only for
/// the calls made inline after stop.
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
    #[cfg(feature = "diagnostics")]
    {
        p.woken_from_before = !0;
    }
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
            // A thread that cannot start ends the program.
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

#[cfg_attr(not(feature = "diagnostics"), allow(dead_code, reason = "the report and the diagnostic keys' alone"))]
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
    #[cfg(feature = "diagnostics")]
    {
        let kept = WAKES_KEPT.load(Ordering::SeqCst);
        if kept != 0 {
            log_info!(
                "recorder: %llu wakes for gathered calls made where a call due now had spent them before (S14d)",
                kept
            );
        }
    }
}
