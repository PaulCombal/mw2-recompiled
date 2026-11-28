//! The copy thread: the vertex windows a draw copies into the frame arena, made beside the
//! ring consumer rather than on it.
//!
//! Most of a draw's copying reads geometry the title wrote up to a frame before, out of
//! memory rather than any cache, and the consumer waited on every load of it. The consumer
//! now hands such a copy over -- where it comes from, where it goes in the arena, how long
//! -- and goes on with the draw; this thread makes it.
//!
//! When it may be made late. The console's GPU reads a draw's vertices when it gets to the
//! draw, so the title rewrites none of them until the command processor has let it see the
//! draw done: a write to its memory (a fence, the arena's progress, a scratch register),
//! an interrupt, the read pointer's write-back. Until the consumer does any of those, the
//! bytes a parsed draw reads stay as they were when it was parsed, and its copy can be made
//! any time before. So the consumer settles -- every copy handed over made -- before each
//! of them (settle), before a submission hands the arena to the GPU, before it writes guest
//! memory itself (an image-pool move), and before the arena goes.
//!
//! The copies are independent: each reads guest memory nothing writes until the next
//! settle and writes its own part of the arena. So either thread makes any of them, in any
//! order: settle takes those not yet taken itself and waits only for the ones this thread
//! is in the middle of, and a full queue makes the copy on the spot.
//!
//! The consumer alone hands over and settles; this thread alone sleeps. Its sleep follows
//! the recorder's rule (recorder.rs): it announces itself asleep, looks once more, and
//! parks until `wakes` moves, which hand_over moves before it unparks.

use crate::runtime::{crash, platform};
use std::cell::UnsafeCell;
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::*};
use std::thread::{self, JoinHandle, Thread};

/// Below this a copy is made where it is met: handing it over costs about as much.
pub const SMALLEST: u32 = 1024;

const JOBS: usize = 1024;

/// How long the thread looks for more before it sleeps, in spin-loop hints.
const SPIN: u32 = 200;

/// A sleeping thread is woken once this many copies wait: a wake costs the consumer a
/// system call, and what is still waiting at a settle the consumer makes itself.
const WAKE_AT: u64 = 4;

#[derive(Clone, Copy)]
struct Job {
    from: *const u8,
    to: *mut u8,
    bytes: usize,
}

struct Queue {
    jobs: [UnsafeCell<Job>; JOBS],
    handed: AtomicU64,  // jobs written, the consumer's
    taken: AtomicU64,   // jobs claimed by either thread
    made: AtomicU64,    // jobs finished by either thread
    wakes: AtomicU64,   // moved before each unpark
    asleep: AtomicBool, // the thread is about to park, or parked
    stopping: AtomicBool,
    running: AtomicBool,
}

// SAFETY: a job is written by the consumer before `handed` is published past it and read
// only by the thread that claims it through `taken`, after; the pointers it holds are
// guest memory and the mapped arena, both valid from any thread until stop.
unsafe impl Sync for Queue {}

static QUEUE: Queue = Queue {
    jobs: [const { UnsafeCell::new(Job { from: ptr::null(), to: ptr::null_mut(), bytes: 0 }) }; JOBS],
    handed: AtomicU64::new(0),
    taken: AtomicU64::new(0),
    made: AtomicU64::new(0),
    wakes: AtomicU64::new(0),
    asleep: AtomicBool::new(false),
    stopping: AtomicBool::new(false),
    running: AtomicBool::new(false),
};

static THREAD: Mutex<Option<(Thread, JoinHandle<()>)>> = Mutex::new(None);

// For the report: copies handed over, made by the consumer at a settle or because the
// queue was full, and the settles that waited on this thread.
pub static HANDED: AtomicU64 = AtomicU64::new(0);
pub static MADE_BY_CONSUMER: AtomicU64 = AtomicU64::new(0);
pub static SETTLES_WAITED: AtomicU64 = AtomicU64::new(0);

fn bump(counter: &AtomicU64) {
    counter.store(counter.load(Relaxed).wrapping_add(1), Relaxed);
}

/// # Safety
/// The job's own contract (hand_over's).
#[inline]
unsafe fn make(job: Job) {
    // SAFETY: the caller's.
    unsafe { ptr::copy_nonoverlapping(job.from, job.to, job.bytes) };
}

/// The next job no thread has taken, claimed; None when there is none.
#[inline]
fn take() -> Option<Job> {
    let q = &QUEUE;
    loop {
        let taken = q.taken.load(Acquire);
        if taken >= q.handed.load(Acquire) {
            return None;
        }
        if q.taken.compare_exchange_weak(taken, taken + 1, AcqRel, Relaxed).is_ok() {
            // SAFETY: below `handed`, so written and published; claimed by this thread
            // alone, and not overwritten until `made` passes it (hand_over waits for room).
            return Some(unsafe { *q.jobs[taken as usize % JOBS].get() });
        }
    }
}

/// A made job: its bytes are in the arena for whoever settles after this. The arena is
/// write-combined, and a release store does not drain write-combining buffers.
#[inline]
fn finished() {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: a store fence has no preconditions.
    unsafe {
        std::arch::x86_64::_mm_sfence()
    };
    QUEUE.made.fetch_add(1, Release);
}

fn run() {
    crash::register_thread(c"gpu copier");
    let q = &QUEUE;
    while !q.stopping.load(Relaxed) {
        if let Some(job) = take() {
            // SAFETY: hand_over's contract, valid until the settle that waits for this.
            unsafe { make(job) };
            finished();
            continue;
        }
        // A short spin first, a few microseconds: the copies of a stretch of draws come in
        // runs. Any longer and the thread spends a processor's time waiting.
        let mut found = false;
        for _ in 0..SPIN {
            std::hint::spin_loop();
            if q.taken.load(Relaxed) < q.handed.load(Acquire) {
                found = true;
                break;
            }
        }
        if found {
            continue;
        }
        let seen = q.wakes.load(SeqCst);
        q.asleep.store(true, SeqCst);
        if q.taken.load(SeqCst) >= q.handed.load(SeqCst) && !q.stopping.load(SeqCst) {
            while q.wakes.load(SeqCst) == seen {
                thread::park();
            }
        }
        q.asleep.store(false, Relaxed);
    }
}

/// The renderer's start, with MW2_COPY_THREAD=1 alone: without it, or on a machine with
/// one processor thread, nothing is handed over and the consumer makes every copy where
/// it meets it.
///
/// Off unless asked for: on a processor whose cores share a power budget -- a laptop's,
/// the Steam Deck's -- the busy core it adds lowers the consumer's clock about as much as
/// it takes work off it (the consumer's cycles a frame down 6%, the frame rate unchanged).
pub fn start() {
    let q = &QUEUE;
    if q.running.load(Relaxed)
        || !crate::env::flag(c"MW2_COPY_THREAD")
        || std::thread::available_parallelism().map_or(1, |n| n.get()) < 2
    {
        return;
    }
    q.stopping.store(false, Relaxed);
    let Ok(handle) =
        thread::Builder::new().name("gpu copier".into()).stack_size(platform::default_thread_stack()).spawn(run)
    else {
        crate::log::log_warn!("renderer: the copy thread would not start; the consumer makes its copies");
        return;
    };
    *THREAD.lock().unwrap_or_else(|p| p.into_inner()) = Some((handle.thread().clone(), handle));
    q.running.store(true, Release);
}

/// The renderer's shutdown, before the arena is unmapped: every copy made, the thread
/// joined. Nothing is handed over after.
pub fn stop() {
    settle();
    let q = &QUEUE;
    q.running.store(false, Release);
    q.stopping.store(true, SeqCst);
    let joined = THREAD.lock().unwrap_or_else(|p| p.into_inner()).take();
    if let Some((thread, handle)) = joined {
        q.wakes.fetch_add(1, SeqCst);
        thread.unpark();
        let _ = handle.join();
    }
}

/// `bytes` from `from` to `to`, made now or by the copy thread before the next settle.
///
/// # Safety
/// The consumer's. `bytes` readable at `from` -- guest memory, which nothing writes before
/// the next settle (the module's rule) -- and writable at `to`, in the mapped arena, which
/// nothing else writes or reads before then.
#[inline]
pub unsafe fn hand_over(from: *const u8, to: *mut u8, bytes: u32) {
    let q = &QUEUE;
    let handed = q.handed.load(Relaxed);
    // Room: the job `JOBS` back has to be finished before its place is written again.
    if bytes < SMALLEST || !q.running.load(Relaxed) || handed - q.made.load(Acquire) >= JOBS as u64 {
        if bytes >= SMALLEST {
            bump(&MADE_BY_CONSUMER);
        }
        // SAFETY: the caller's.
        unsafe { ptr::copy_nonoverlapping(from, to, bytes as usize) };
        return;
    }
    // SAFETY: the place is free (above), and only this thread writes jobs.
    unsafe { *q.jobs[handed as usize % JOBS].get() = Job { from, to, bytes: bytes as usize } };
    q.handed.store(handed + 1, SeqCst);
    bump(&HANDED);
    if q.asleep.load(SeqCst) && handed + 1 - q.taken.load(Relaxed) >= WAKE_AT {
        q.wakes.fetch_add(1, SeqCst);
        if let Some((thread, _)) = THREAD.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            thread.unpark();
        }
    }
}

/// Every copy handed over made, its bytes in the arena: what the consumer does before it
/// lets the title or the GPU see anything (the module's rule). The consumer's.
#[inline]
pub fn settle() {
    let q = &QUEUE;
    let handed = q.handed.load(Relaxed);
    if q.made.load(Acquire) == handed {
        return;
    }
    settle_slow(handed);
}

#[cold]
#[inline(never)]
fn settle_slow(handed: u64) {
    let q = &QUEUE;
    while let Some(job) = take() {
        bump(&MADE_BY_CONSUMER);
        // SAFETY: hand_over's contract.
        unsafe { make(job) };
        finished();
    }
    if q.made.load(Acquire) != handed {
        bump(&SETTLES_WAITED);
        while q.made.load(Acquire) != handed {
            std::hint::spin_loop();
        }
    }
}
