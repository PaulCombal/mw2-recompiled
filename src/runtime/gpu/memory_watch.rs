// Which pages of guest physical memory the title has written (runtime/gpu/memory_watch.cpp),
// found out the way Xenia finds out: a page the GPU reads from is made read-only, and
// the first write to it faults once, which marks it written and makes it writable again.
//
// What it is for is the shadow: a copy of physical memory the GPU can read, kept page by
// page. A draw whose vertex data lies in pages nobody has written since they were copied
// reads them there, where it used to copy its window of the buffer into the frame arena
// on every draw -- 28 MB a frame, most of it world geometry that never changes.
//
// A page's copy is only replaced once no submission still on the GPU can read it: those
// were recorded against the bytes it holds, which is what the console would have drawn
// from.
//
// Unsafe by nature, and its contract is this. The page states are atomics any thread
// moves (STATE); the shadow's pointer and the switch are atomics published once by
// set_shadow, the pointer before the switch. Everything else -- the last submission to
// read a page, when it was armed, its strikes, its stamp -- is the ring consumer's alone
// (CONSUMER): serve, track and unwritten run only on it, and set_shadow only while it does
// not run (before it starts, after it is joined). handle_fault runs in a signal handler or
// the vectored exception handler, and will_write on any thread: they and loosen and
// protect read and write only atomics, take no lock, allocate nothing and log nothing.
// Guest memory and the shadow are read and written through raw pointers only, never
// through a slice: guest threads write the one, the GPU reads the other.
//
// One change from the C++, the plan's D15: a fault on a page that reads Loose makes the
// page writable again. A writer that loosens a page between the consumer's store of
// Arming and its protect leaves the page Loose and read-only; the C++ handler then
// answered every fault on it without making it writable, and the writing thread faulted
// for ever unless the consumer happened to arm that page again. Here the page is made
// writable, and the state read again, as loosen does: a page armed again in between is
// loosened properly. The faults this answers are counted (LOOSE_FAULTS) and reported;
// the count also takes in two threads faulting on one page together, where the second
// finds it Loose and protects it again, so it is not a count of stuck pages.
// The same page can still be read-only when set_shadow(null) turns the handler off, and
// loosen leaves a Loose page alone: there every run of Loose pages is made writable before
// the switch goes off, uncounted and untimed (the report's protections stay the C++'s).
// The other: whether protections are timed (MW2_REPORT) is read once, at set_shadow, into
// an atomic, where the C++ asked report::On() -- a function-local static -- inside the
// signal handler.
#![deny(unsafe_op_in_unsafe_fn)]

use crate::runtime::bridge::WatchCosts;
use crate::runtime::guest_memory::{self, PHYSICAL_BASE, PHYSICAL_END};
use crate::runtime::{platform, report};
use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU64, Ordering::*};
use std::time::Instant;

#[cfg(feature = "diagnostics")]
use crate::runtime::bridge::stream;
#[cfg(feature = "diagnostics")]
use std::sync::atomic::AtomicU32;

#[cfg(feature = "diagnostics")]
use crate::log::log_info;

pub const PAGE_BYTES: u32 = 4096;
const PHYSICAL_BYTES: u32 = PHYSICAL_END - PHYSICAL_BASE;
const PAGES: usize = (PHYSICAL_BYTES / PAGE_BYTES) as usize;
const APERTURE_BASE: u32 = 0xC000_0000;

// A page's life. The consumer moves Never or Loose to Arming and Arming to Watched;
// anyone who writes moves Arming or Watched to Loose, and makes the page writable
// *before* doing so -- so a page is never writable while it reads Watched for longer than
// it takes the writer's handler to finish.
const NEVER: u8 = 0; // never copied; writable; a fault here is not ours
const LOOSE: u8 = 1; // copied once, written since; writable (or being armed again)
const ARMING: u8 = 2; // read-only, being copied
const WATCHED: u8 = 3; // read-only, the copy is current

static STATE: [AtomicU8; PAGES] = [const { AtomicU8::new(NEVER) }; PAGES];

// The consumer's bookkeeping.
struct Consumer {
    last_served: [u64; PAGES], // the last submission to read the page's copy
    armed_frame: [u32; PAGES],
    strikes: [u8; PAGES], // how often it was written soon after being copied
    // Which copy the page holds: a stamp that rises with every copy made, so "watched"
    // can be asked as "watched since" -- a page written and copied again in between
    // reads Watched with a later stamp.
    armed_at: [u64; PAGES],
    stamp: u64,
}

struct ConsumerCell(UnsafeCell<Consumer>);
// SAFETY: the single-owner rule above: only the ring consumer touches it, and
// set_shadow, which does not, never touches it either.
unsafe impl Sync for ConsumerCell {}

static CONSUMER: ConsumerCell = ConsumerCell(UnsafeCell::new(Consumer {
    last_served: [0; PAGES],
    armed_frame: [0; PAGES],
    strikes: [0; PAGES],
    armed_at: [0; PAGES],
    stamp: 0,
}));

/// The consumer's bookkeeping.
///
/// # Safety
/// The ring consumer's thread only, and one reference at a time.
#[inline(always)]
#[allow(clippy::mut_from_ref)]
unsafe fn consumer() -> &'static mut Consumer {
    // SAFETY: the caller's contract.
    unsafe { &mut *CONSUMER.0.get() }
}

static SHADOW: AtomicPtr<u8> = AtomicPtr::new(ptr::null_mut());
static ENABLED: AtomicBool = AtomicBool::new(false);
// MW2_REPORT, read at set_shadow: the protections are counted and timed while it is on.
static TIMED: AtomicBool = AtomicBool::new(false);

static FAULTS: AtomicU64 = AtomicU64::new(0);
static BULK_WRITES: AtomicU64 = AtomicU64::new(0);
static PROTECTS: AtomicU64 = AtomicU64::new(0);
static PROTECT_NANOSECONDS: AtomicU64 = AtomicU64::new(0);
// Faults on a page that read Loose, made writable again (D15).
static LOOSE_FAULTS: AtomicU64 = AtomicU64::new(0);
// How often each page went from watched to written: MW2_CAPTURE_GPU stores a page the
// title wrote with the bytes it held, so that the replay's write is seen too.
#[cfg(feature = "diagnostics")]
static LOOSENED: [AtomicU32; PAGES] = [const { AtomicU32::new(0) }; PAGES];

// The consumer's counters, single-writer atomics (a load and a store), which the report
// reads after the consumer has stopped.
static ARMS: AtomicU64 = AtomicU64::new(0);
static PAGES_ARMED: AtomicU64 = AtomicU64::new(0);
static ARMED_WRITTEN_DURING: AtomicU64 = AtomicU64::new(0);
static ARM_FAILURES: AtomicU64 = AtomicU64::new(0);
static SERVED: AtomicU64 = AtomicU64::new(0);
static SERVED_BYTES: AtomicU64 = AtomicU64::new(0);
static TRACKED: AtomicU64 = AtomicU64::new(0);
static REFUSED_IN_FLIGHT: AtomicU64 = AtomicU64::new(0);
static REFUSED_BACKING_OFF: AtomicU64 = AtomicU64::new(0);
static REFUSED_OUTSIDE: AtomicU64 = AtomicU64::new(0);

#[inline(always)]
fn bump(counter: &AtomicU64, by: u64) {
    counter.store(counter.load(Relaxed).wrapping_add(by), Relaxed);
}

// Both windows onto the physical bank: a write through either lands on the page, so both
// have to be read-only for a write to be seen. Safe in a signal handler.
fn protect(first_page: u32, pages: u32, writable: bool) -> bool {
    let timed = TIMED.load(Relaxed);
    let began = if timed { Some(Instant::now()) } else { None };
    let ok = protect_windows(first_page, pages, writable);
    if let Some(began) = began {
        PROTECTS.fetch_add(2, Relaxed);
        PROTECT_NANOSECONDS.fetch_add(began.elapsed().as_nanos() as u64, Relaxed);
    }
    ok
}

// protect without the counting and timing.
#[inline(always)]
fn protect_windows(first_page: u32, pages: u32, writable: bool) -> bool {
    let base = guest_memory::base();
    let offset = first_page as usize * PAGE_BYTES as usize;
    let length = pages as usize * PAGE_BYTES as usize;
    let mut ok = true;
    for window in [PHYSICAL_BASE, APERTURE_BASE] {
        // SAFETY: pages of the bank, which guest_memory maps at both windows; a write the
        // protection stops faults into handle_fault.
        ok &= unsafe { platform::protect(base.wrapping_add(window as usize + offset), length, writable) };
    }
    ok
}

// The writer's half of the protocol. Safe in a signal handler.
fn loosen(page: usize) {
    let state = &STATE[page];
    loop {
        let was = state.load(Acquire);
        if was == NEVER || was == LOOSE {
            return;
        }
        protect(page as u32, 1, true);
        if state.compare_exchange_weak(was, LOOSE, AcqRel, Acquire).is_ok() {
            #[cfg(feature = "diagnostics")]
            LOOSENED[page].fetch_add(1, Relaxed);
            return;
        }
    }
}

// Offset into the bank of a guest address in either window.
#[inline(always)]
fn physical_offset(guest_address: u64) -> Option<u32> {
    for window in [PHYSICAL_BASE as u64, APERTURE_BASE as u64] {
        if guest_address >= window && guest_address < window + PHYSICAL_BYTES as u64 {
            return Some((guest_address - window) as u32);
        }
    }
    None
}

// Copies a run of pages into the shadow and watches them. True when every one came out
// Watched: a write during the copy leaves its page Loose.
#[inline(never)]
fn arm(c: &mut Consumer, first: u32, count: u32, frame: u64) -> bool {
    #[cfg(feature = "diagnostics")]
    stream::note_read(PHYSICAL_BASE + first * PAGE_BYTES, u64::from(count) * u64::from(PAGE_BYTES));
    let run = first as usize..(first + count) as usize;
    for p in run.clone() {
        let was = STATE[p].load(Relaxed);
        // Paced by how soon the page was written after it was last copied. Anything inside
        // a thousand frames counts: the title's dynamic geometry goes round a ring that
        // comes back to a page every few frames, and copying it back in after each lap cost
        // a fault and two mprotects a page for data the next lap overwrites unread.
        if was == LOOSE {
            c.strikes[p] =
                if frame.wrapping_sub(u64::from(c.armed_frame[p])) < 1024 { (c.strikes[p] + 1).min(10) } else { 0 };
        }
        c.armed_frame[p] = frame as u32;
        STATE[p].store(ARMING, Release);
    }
    bump(&ARMS, 1);
    c.stamp = c.stamp.wrapping_add(1);
    c.armed_at[run.clone()].fill(c.stamp);
    if !protect(first, count, false) {
        // Out of mappings, most likely. Nothing is watched, nothing is served.
        protect(first, count, true);
        for p in run {
            STATE[p].store(LOOSE, Release);
        }
        bump(&ARM_FAILURES, 1);
        return false;
    }
    let shadow = SHADOW.load(Relaxed);
    let offset = first as usize * PAGE_BYTES as usize;
    // SAFETY: the shadow holds the whole bank (set_shadow refuses a smaller one) and the
    // run lies in it (pages); guest memory is the bank as mapped at its first window.
    // Guest threads may write the pages while they are copied: such a page comes out Loose.
    unsafe {
        ptr::copy_nonoverlapping(
            guest_memory::base().add(PHYSICAL_BASE as usize + offset),
            shadow.add(offset),
            count as usize * PAGE_BYTES as usize,
        );
    }
    #[cfg(feature = "diagnostics")]
    trace_arm(first, count, shadow);
    let mut all = true;
    for p in run {
        if STATE[p].compare_exchange(ARMING, WATCHED, AcqRel, Acquire).is_err() {
            bump(&ARMED_WRITTEN_DURING, 1);
            all = false;
        }
    }
    bump(&PAGES_ARMED, u64::from(count));
    all
}

// The command trace's line for each page armed (vk::trace, runtime/rust_calls.cpp).
#[cfg(feature = "diagnostics")]
fn trace_arm(first: u32, count: u32, shadow: *const u8) {
    unsafe extern "C" {
        fn mw2_trace_shadow_arm(first: u32, count: u32, shadow: *const u8);
    }
    // SAFETY: the shadow as set_shadow has it; the C++ reads the run's pages of it.
    unsafe { mw2_trace_shadow_arm(first, count, shadow) }
}

/// The copy, as mapped for the CPU: physical address N at `shadow + N`. Null leaves every
/// draw on the per-draw copy (MW2_NO_SHADOW=1 does that). The ring consumer is not
/// running.
pub fn set_shadow(shadow: *mut u8, bytes: u32) {
    TIMED.store(report::on(), Relaxed);
    if shadow.is_null() || bytes < PHYSICAL_BYTES {
        // Every page writable again before the handler stops answering for them, or the
        // title's next write to one is taken for a crash.
        SHADOW.store(ptr::null_mut(), Relaxed);
        // A Loose page may be read-only (D15: loosened between an arm's store of Arming
        // and its protect, and not written since); loosen leaves a Loose page as it is.
        // Every run of them writable in one call, which is idempotent for the many that
        // are writable already.
        let mut page = 0;
        while page < PAGES {
            if STATE[page].load(Acquire) != LOOSE {
                loosen(page);
                page += 1;
                continue;
            }
            let first = page;
            while page < PAGES && STATE[page].load(Acquire) == LOOSE {
                page += 1;
            }
            protect_windows(first as u32, (page - first) as u32, true);
        }
        ENABLED.store(false, Release);
        return;
    }
    SHADOW.store(shadow, Release);
    ENABLED.store(true, Release);
}

/// For a bug report (report.rs): totals so far. The protection changes are counted and
/// timed only while MW2_REPORT is on.
pub fn costs() -> WatchCosts {
    WatchCosts {
        faults: FAULTS.load(Relaxed),
        protects: PROTECTS.load(Relaxed),
        protect_nanoseconds: PROTECT_NANOSECONDS.load(Relaxed),
    }
}

#[inline]
pub fn enabled() -> bool {
    ENABLED.load(Relaxed)
}

/// From the fault handler, before anything else sees the fault, for a write to a page
/// that is there but read-only. True when the page is a watched one, which is writable
/// again and marked. Safe in a signal handler.
pub fn handle_fault(address: *const c_void) -> bool {
    if !ENABLED.load(Relaxed) {
        return false;
    }
    let base = guest_memory::base() as usize;
    let at = address as usize;
    if base == 0 || at < base || at as u64 >= base as u64 + 0x1_0000_0000 {
        return false;
    }
    let Some(offset) = physical_offset((at - base) as u64) else { return false };
    let page = (offset / PAGE_BYTES) as usize;
    // Never watched: the fault is somebody else's.
    let state = STATE[page].load(Acquire);
    if state == NEVER {
        return false;
    }
    // Loose already: another thread's handler got there first and the page is writable,
    // or a writer loosened it while the consumer was arming it and the consumer's protect
    // came after -- then nothing would make it writable, and this write would fault for
    // ever. Writable again either way; loosen then reads the state again, and loosens a
    // page armed again in between.
    if state == LOOSE {
        protect(page as u32, 1, true);
        LOOSE_FAULTS.fetch_add(1, Relaxed);
    }
    loosen(page);
    FAULTS.fetch_add(1, Relaxed);
    true
}

// Every page of the range watched, copying in those that can be. A copy is never replaced
// while a submission on the GPU can read it. For the shadow, pages the title keeps
// writing wait longer each time before they are copied again (`back_off`).
#[inline]
fn ready(c: &mut Consumer, first: u32, last: u32, completed: u64, frame: u64, back_off: bool) -> bool {
    let (first, last) = (first as usize, last as usize);
    let states = &STATE[first..=last];
    let Some(unwatched) = states.iter().position(|state| state.load(Acquire) != WATCHED) else { return true };
    copy_in(c, first + unwatched, last, completed, frame, back_off)
}

// The rest of ready, from the first page that is not Watched: out of line, so that the
// common answer -- every page watched -- costs the caller no more than its loop.
#[inline(never)]
#[expect(clippy::needless_range_loop, reason = "a page indexes the states and three of the consumer's arrays")]
fn copy_in(c: &mut Consumer, p: usize, last: usize, completed: u64, frame: u64, back_off: bool) -> bool {
    for q in p..=last {
        let state = STATE[q].load(Acquire);
        if state == WATCHED {
            continue;
        }
        let served = c.last_served[q];
        if served != 0 && served > completed {
            bump(&REFUSED_IN_FLIGHT, 1);
            return false;
        }
        if back_off && state == LOOSE && frame < u64::from(c.armed_frame[q]) + (1u64 << c.strikes[q]) {
            bump(&REFUSED_BACKING_OFF, 1);
            return false;
        }
    }
    let mut all = true;
    let mut q = p;
    while q <= last {
        if STATE[q].load(Acquire) == WATCHED {
            q += 1;
            continue;
        }
        let mut run = 1;
        while q + run <= last && STATE[q + run].load(Acquire) != WATCHED {
            run += 1;
        }
        all &= arm(c, q as u32, run as u32, frame);
        q += run;
    }
    all
}

#[inline(always)]
fn pages(physical: u32, bytes: u32) -> Option<(u32, u32)> {
    if SHADOW.load(Relaxed).is_null() || bytes == 0 {
        return None;
    }
    if u64::from(physical) + u64::from(bytes) > u64::from(PHYSICAL_BYTES) {
        bump(&REFUSED_OUTSIDE, 1);
        return None;
    }
    let first = physical / PAGE_BYTES;
    let last = ((u64::from(physical) + u64::from(bytes) - 1) / u64::from(PAGE_BYTES)) as u32;
    Some((first, last))
}

/// Before this runtime writes a large range of guest memory itself -- the image pool's
/// moves: the range is made writable in one call rather than taking a fault a page. Any
/// thread. `guest_address` is virtual.
pub fn will_write(guest_address: u32, bytes: u32) {
    if !ENABLED.load(Relaxed) || bytes == 0 {
        return;
    }
    let Some(offset) = physical_offset(u64::from(guest_address)) else { return };
    let first = offset / PAGE_BYTES;
    let last =
        ((u64::from(offset) + u64::from(bytes) - 1).min(u64::from(PHYSICAL_BYTES) - 1) / u64::from(PAGE_BYTES)) as u32;
    if !STATE[first as usize..=last as usize].iter().any(|state| state.load(Acquire) >= ARMING) {
        return;
    }
    // Writable first, then marked: the same order a fault takes, so a page is never Loose
    // while still read-only by this writer's doing. One re-armed in between faults as usual.
    protect(first, last - first + 1, true);
    for p in first as usize..=last as usize {
        let mut state = STATE[p].load(Acquire);
        while state >= ARMING {
            match STATE[p].compare_exchange_weak(state, LOOSE, AcqRel, Acquire) {
                Ok(_) => break,
                Err(now) => state = now,
            }
        }
        #[cfg(feature = "diagnostics")]
        if state >= ARMING {
            LOOSENED[p].fetch_add(1, Relaxed);
        }
    }
    BULK_WRITES.fetch_add(1, Relaxed);
}

/// Ring consumer only. Whether [physical, physical + bytes) of the shadow holds what guest
/// memory holds now, copying in the pages that can be. A true answer marks those pages
/// read by `recording`, the submission being recorded; `completed` is the last one the GPU
/// has finished, and `frame` paces the pages that keep being written.
#[inline]
pub fn serve(physical: u32, bytes: u32, recording: u64, completed: u64, frame: u64) -> bool {
    let Some((first, last)) = pages(physical, bytes) else { return false };
    let (first, last) = (first as usize, last as usize);
    // Once a vertex fetch: the common answer, every page watched, without a call; the
    // rest out of line, in tail position, so that this path saves no registers.
    match STATE[first..=last].iter().position(|state| state.load(Acquire) != WATCHED) {
        None => {
            served(first, last, recording, bytes);
            true
        }
        Some(unwatched) => serve_copying(first + unwatched, first, last, recording, completed, frame, bytes),
    }
}

#[inline(always)]
fn served(first: usize, last: usize, recording: u64, bytes: u32) {
    // SAFETY: the ring consumer's thread, the module's single-owner rule.
    let c = unsafe { consumer() };
    c.last_served[first..=last].fill(recording);
    bump(&SERVED, 1);
    bump(&SERVED_BYTES, u64::from(bytes));
}

#[inline(never)]
fn serve_copying(p: usize, first: usize, last: usize, recording: u64, completed: u64, frame: u64, bytes: u32) -> bool {
    // SAFETY: the ring consumer's thread.
    if !copy_in(unsafe { consumer() }, p, last, completed, frame, true) {
        return false;
    }
    served(first, last, recording, bytes);
    true
}

/// The same watch for a reader that keeps its own copy -- the texture cache and the
/// command processor's shader memo. Track, *before* reading the range, returns a stamp
/// when every page of it is watched from then on (0 when not); unwritten says whether
/// none of them has been written since that stamp. Ring consumer only.
///
/// Unlike the shadow, track does not back off from pages the title keeps writing: a
/// texture is read again whenever a page of it is written, which costs far more than the
/// fault that says so.
pub fn track(physical: u32, bytes: u32, completed: u64, frame: u64) -> u64 {
    let Some((first, last)) = pages(physical, bytes) else { return 0 };
    // SAFETY: the ring consumer's thread.
    let c = unsafe { consumer() };
    if !ready(c, first, last, completed, frame, false) {
        return 0;
    }
    bump(&TRACKED, 1);
    c.stamp
}

pub fn unwritten(physical: u32, bytes: u32, since: u64) -> bool {
    if since == 0 {
        return false;
    }
    let Some((first, last)) = pages(physical, bytes) else { return false };
    // SAFETY: the ring consumer's thread.
    let c = unsafe { consumer() };
    let (first, last) = (first as usize, last as usize);
    STATE[first..=last]
        .iter()
        .zip(&c.armed_at[first..=last])
        .all(|(state, &armed_at)| state.load(Acquire) == WATCHED && armed_at <= since)
}

/// How many times page `page` of the bank has gone from watched to written.
#[cfg(feature = "diagnostics")]
pub fn loosened(page: u32) -> u32 {
    LOOSENED.get(page as usize).map_or(0, |count| count.load(Relaxed))
}

// Called by gpu.rs's report, which a diagnostics build has.
#[cfg(feature = "diagnostics")]
pub fn report() {
    let arms = ARMS.load(Relaxed);
    if arms == 0 {
        log_info!("gpu: no shadow of guest memory was used; every draw copied its vertex data");
        return;
    }
    let (mut watched, mut loose) = (0u32, 0u32);
    for state in &STATE {
        let state = state.load(Relaxed);
        watched += u32::from(state == WATCHED);
        loose += u32::from(state == LOOSE);
    }
    log_info!(
        "gpu: shadow of guest memory served %llu ranges (%llu MB); %llu copies of %llu pages, %llu written during the copy, %llu failed; refused %llu in flight, %llu backing off, %llu outside",
        SERVED.load(Relaxed),
        SERVED_BYTES.load(Relaxed) >> 20,
        arms,
        PAGES_ARMED.load(Relaxed),
        ARMED_WRITTEN_DURING.load(Relaxed),
        ARM_FAILURES.load(Relaxed),
        REFUSED_IN_FLIGHT.load(Relaxed),
        REFUSED_BACKING_OFF.load(Relaxed),
        REFUSED_OUTSIDE.load(Relaxed)
    );
    log_info!(
        "gpu: %llu write faults, %llu ranges written by the runtime; %llu texture ranges watched; %u pages watched at the end, %u written since their copy",
        FAULTS.load(Relaxed),
        BULK_WRITES.load(Relaxed),
        TRACKED.load(Relaxed),
        watched,
        loose
    );
    let loose_faults = LOOSE_FAULTS.load(Relaxed);
    if loose_faults != 0 {
        log_info!(
            "gpu: %llu loose faults: writes to a page already written since its copy, made writable again",
            loose_faults
        );
    }
}

// tools/gpu_diff.py --rig watch: the state the rig compares with the C++'s, and a reset
// between its scripts.
#[cfg(mw2_gpu_rig)]
pub mod rig {
    use super::*;

    /// Page `page`'s state, strikes, armed frame, stamp and last submission.
    pub fn page(page: u32) -> (u8, u8, u32, u64, u64) {
        let p = page as usize;
        // SAFETY: the rig's thread is the consumer, and it is not in a call.
        let c = unsafe { consumer() };
        (STATE[p].load(Relaxed), c.strikes[p], c.armed_frame[p], c.armed_at[p], c.last_served[p])
    }

    /// Page `page`'s state, which any thread may read while a call is in progress.
    pub fn state(page: u32) -> u8 {
        STATE[page as usize].load(Relaxed)
    }

    /// The stamp, then the counters in the report's order, the loose faults last.
    pub fn counters() -> [u64; 16] {
        // SAFETY: as page.
        let c = unsafe { consumer() };
        [
            c.stamp,
            SERVED.load(Relaxed),
            SERVED_BYTES.load(Relaxed),
            ARMS.load(Relaxed),
            PAGES_ARMED.load(Relaxed),
            ARMED_WRITTEN_DURING.load(Relaxed),
            ARM_FAILURES.load(Relaxed),
            REFUSED_IN_FLIGHT.load(Relaxed),
            REFUSED_BACKING_OFF.load(Relaxed),
            REFUSED_OUTSIDE.load(Relaxed),
            FAULTS.load(Relaxed),
            BULK_WRITES.load(Relaxed),
            TRACKED.load(Relaxed),
            PROTECTS.load(Relaxed),
            u64::from(ENABLED.load(Relaxed)),
            LOOSE_FAULTS.load(Relaxed),
        ]
    }

    /// Everything back to a start, the pages Never: the rig has made them writable.
    pub fn reset() {
        // SAFETY: as page.
        let c = unsafe { consumer() };
        c.last_served.fill(0);
        c.armed_frame.fill(0);
        c.strikes.fill(0);
        c.armed_at.fill(0);
        c.stamp = 0;
        for state in &STATE {
            state.store(NEVER, Relaxed);
        }
        #[cfg(feature = "diagnostics")]
        for count in &LOOSENED {
            count.store(0, Relaxed);
        }
        SHADOW.store(ptr::null_mut(), Relaxed);
        ENABLED.store(false, Relaxed);
        TIMED.store(false, Relaxed);
        for counter in [
            &FAULTS,
            &BULK_WRITES,
            &PROTECTS,
            &PROTECT_NANOSECONDS,
            &LOOSE_FAULTS,
            &ARMS,
            &PAGES_ARMED,
            &ARMED_WRITTEN_DURING,
            &ARM_FAILURES,
            &SERVED,
            &SERVED_BYTES,
            &TRACKED,
            &REFUSED_IN_FLIGHT,
            &REFUSED_BACKING_OFF,
            &REFUSED_OUTSIDE,
        ] {
            counter.store(0, Relaxed);
        }
    }
}
