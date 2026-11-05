// MW2_STUTTERS=1: see runtime/stutters.h. As with report.rs, the time is the caller's,
// so that tools/runtime_diff.py can give it the same as to the C++.

use crate::env;
use crate::log::{c_string, format_text, log_info, log_warn};
use crate::runtime::frame_rate;
use std::ffi::CStr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};

const BLANK_MS: f64 = 1000.0 / 60.0;
// Late by half a blank or more: the blank it was due at has gone by.
const LATE_US: i64 = 25000;
// Longer than this is not play: a loading screen, a menu, the end of a round.
const PAUSE_US: i64 = 1000000;
const PLAY_STREAK: u32 = 30; // as the in-play frame rate counts it
const LINES_SHOWN: u32 = 200;

const COSTS: usize = 7;
const RING_BATCHES: usize = 6;
const COST_NAMES: [&CStr; COSTS] = [
    c"translating shaders",
    c"building pipelines",
    c"reading new textures",
    c"re-checking watched textures",
    c"waiting on the GPU",
    c"reading occlusion queries",
    c"executing ring batches",
];

struct Spent {
    nanoseconds: AtomicU64,
    times: AtomicU64,
}

static COST: [Spent; COSTS] = [const { Spent { nanoseconds: AtomicU64::new(0), times: AtomicU64::new(0) } }; COSTS];

static IN_PLAY: AtomicBool = AtomicBool::new(false);
static LAST_STUTTER_US: AtomicI64 = AtomicI64::new(-1);
static MARKS: AtomicU32 = AtomicU32::new(0);

// The title's presents, from its render thread.
static LAST_PRESENT_US: AtomicI64 = AtomicI64::new(-1);
static LONGEST_PRESENT_GAP_US: AtomicI64 = AtomicI64::new(0);

// The renderer's side, touched only by the ring consumer.
struct Renderer {
    streak: u32,
    last_world_us: i64,
    costs_at: [u64; COSTS],
    times_at: [u64; COSTS],
    play_us: i64,
    stutters: u64,
    frames_lost: u64,
    pauses: u64,
    title_late: u64,
    renderer_late: u64,
    by_length: [u64; 6], // 1, 2, 3, 4-7, 8-15, 16 and more frames
    late_cost_ns: [u64; COSTS],
}

static RENDERER: Mutex<Renderer> = Mutex::new(Renderer {
    streak: 0,
    last_world_us: -1,
    costs_at: [0; COSTS],
    times_at: [0; COSTS],
    play_us: 0,
    stutters: 0,
    frames_lost: 0,
    pauses: 0,
    title_late: 0,
    renderer_late: 0,
    by_length: [0; 6],
    late_cost_ns: [0; COSTS],
});

// The window's side: frames it let go (the window thread), and frames the display held
// (the present wait thread).
struct Window {
    last_serial: u64,
    presents: u64,
    skips: u64,
    frames_skipped: u64,
    held: u64,
    blanks_held: u64,
}

static WINDOW: Mutex<Window> =
    Mutex::new(Window { last_serial: 0, presents: 0, skips: 0, frames_skipped: 0, held: 0, blanks_held: 0 });
static WINDOW_LINES: AtomicU32 = AtomicU32::new(0);

// For the thread that watches for stalls.
static LAST_WORLD_US: AtomicI64 = AtomicI64::new(-1);
static ENDING: AtomicBool = AtomicBool::new(false);

fn seconds(us: i64) -> f64 {
    us as f64 / 1e6
}

fn plural(count: u64) -> &'static CStr {
    if count == 1 { c"" } else { c"s" }
}

fn length_bucket(lost: u64) -> usize {
    match lost {
        0..=3 => lost.wrapping_sub(1) as usize,
        4..=7 => 3,
        8..=15 => 4,
        _ => 5,
    }
}

/// A frame at every blank is the console's pacing; off it (MW2_FPS_LIMIT) there is no
/// blank a frame was due at.
pub fn wanted() -> bool {
    if !env::flag(c"MW2_STUTTERS") {
        return false;
    }
    if !frame_rate::console() {
        log_warn!("stutters: MW2_STUTTERS is for the console's 60, not MW2_FPS_LIMIT; off");
    }
    frame_rate::console()
}

/// wanted(), asked once: the renderer and the pads share the answer, and the warning.
pub fn on() -> bool {
    static ON: std::sync::LazyLock<bool> = std::sync::LazyLock::new(wanted);
    *ON
}

pub fn add(cost: usize, nanoseconds: u64) {
    COST[cost].nanoseconds.fetch_add(nanoseconds, Ordering::Relaxed);
    COST[cost].times.fetch_add(1, Ordering::Relaxed);
}

pub fn title_presented(now_us: i64) {
    let last = LAST_PRESENT_US.swap(now_us, Ordering::Relaxed);
    if last >= 0 && now_us - last > LONGEST_PRESENT_GAP_US.load(Ordering::Relaxed) {
        LONGEST_PRESENT_GAP_US.store(now_us - last, Ordering::Relaxed);
    }
}

/// `started` runs once, when play first starts.
pub fn finished(now: i64, world: bool, started: impl FnOnce()) {
    if !world {
        return;
    }
    let mut guard = RENDERER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let r = &mut *guard;
    let gap = if r.last_world_us < 0 { 0 } else { now - r.last_world_us };
    let title_gap = LONGEST_PRESENT_GAP_US.swap(0, Ordering::Relaxed);
    let costs: [u64; COSTS] = std::array::from_fn(|c| COST[c].nanoseconds.load(Ordering::Relaxed));
    let times: [u64; COSTS] = std::array::from_fn(|c| COST[c].times.load(Ordering::Relaxed));
    let remember = |r: &mut Renderer| {
        r.last_world_us = now;
        LAST_WORLD_US.store(now, Ordering::Relaxed);
        r.costs_at = costs;
        r.times_at = times;
    };

    if gap >= PAUSE_US {
        if r.streak >= PLAY_STREAK {
            r.pauses += 1;
            log_warn!("STALL: over at %.3f s, %.1f s after the last world frame", seconds(now), seconds(gap));
        }
        r.streak = 0;
        IN_PLAY.store(false, Ordering::Relaxed);
    }
    if r.streak < PLAY_STREAK {
        r.streak += 1;
        if r.streak == PLAY_STREAK {
            IN_PLAY.store(true, Ordering::Relaxed);
            log_info!("stutters: in play at %.3f s, watching", seconds(now));
            started();
        }
        remember(r);
        return;
    }
    r.play_us += gap;
    if gap < LATE_US {
        remember(r);
        return;
    }

    let lost = (gap as f64 / 1000.0 / BLANK_MS).round() as i64 - 1;
    let lost = lost.max(1) as u64;
    r.stutters += 1;
    r.frames_lost += lost;
    r.by_length[length_bucket(lost)] += 1;
    LAST_STUTTER_US.store(now, Ordering::Relaxed);
    let title_late = title_gap >= LATE_US;
    if title_late {
        r.title_late += 1;
    } else {
        r.renderer_late += 1;
    }

    // What the renderer's occasional work cost between the two world frames.
    let mut spent = Vec::new();
    for c in 0..COSTS {
        let ns = costs[c].wrapping_sub(r.costs_at[c]);
        if !title_late {
            r.late_cost_ns[c] = r.late_cost_ns[c].wrapping_add(ns);
        }
        if c == RING_BATCHES || ns < 500000 {
            continue;
        }
        let comma = if spent.is_empty() { c"" } else { c", " };
        let one = format_text!(
            "%s%.1f ms %s (%llu)",
            comma,
            ns as f64 / 1e6,
            COST_NAMES[c],
            times[c].wrapping_sub(r.times_at[c])
        );
        spent.extend_from_slice(one.as_bytes());
    }
    if r.stutters <= u64::from(LINES_SHOWN) {
        let of_that = if spent.is_empty() { c"" } else { c"; of that, " };
        log_warn!(
            "STUTTER %llu: %llu frame%s lost at %.3f s -- %.1f ms between world frames; %s (%.1f ms between its presents); the ring consumer was busy %.1f ms%s%s",
            r.stutters,
            lost,
            plural(lost),
            seconds(now),
            gap as f64 / 1000.0,
            if title_late { c"the title presented late" } else { c"the title presented on time" },
            title_gap as f64 / 1000.0,
            costs[RING_BATCHES].wrapping_sub(r.costs_at[RING_BATCHES]) as f64 / 1e6,
            of_that,
            c_string(spent).as_c_str()
        );
    }
    remember(r);
}

pub fn shown(now_us: i64, serial: u64) {
    let mut w = WINDOW.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let last = w.last_serial;
    w.last_serial = serial;
    if !IN_PLAY.load(Ordering::Relaxed) || last == 0 {
        return;
    }
    w.presents += 1;
    if serial <= last.wrapping_add(1) {
        return;
    }
    let skipped = serial.wrapping_sub(last).wrapping_sub(1);
    w.skips += 1;
    w.frames_skipped = w.frames_skipped.wrapping_add(skipped);
    if WINDOW_LINES.fetch_add(1, Ordering::Relaxed) < LINES_SHOWN {
        log_warn!(
            "DISPLAY at %.3f s: the window let %llu frame%s go unshown (before frame %llu)",
            seconds(now_us),
            skipped,
            plural(skipped),
            serial
        );
    }
}

pub fn displayed(now_us: i64, serial: u64, extra_blanks: u32, gap_ms: f64) {
    if !IN_PLAY.load(Ordering::Relaxed) {
        return;
    }
    let mut w = WINDOW.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    w.held += 1;
    w.blanks_held = w.blanks_held.wrapping_add(u64::from(extra_blanks));
    if WINDOW_LINES.fetch_add(1, Ordering::Relaxed) < LINES_SHOWN {
        log_warn!(
            "DISPLAY at %.3f s: the frame before %llu stayed on screen %u blank%s longer (%.1f ms between the two)",
            seconds(now_us),
            serial,
            extra_blanks,
            plural(u64::from(extra_blanks)),
            gap_ms
        );
    }
}

pub fn mark(now_us: i64, how: &CStr) {
    let last = LAST_STUTTER_US.load(Ordering::Relaxed);
    let n = MARKS.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    if last < 0 {
        log_warn!("STUTTER MARK %u (%s) at %.3f s: no stutter logged yet", n, how, seconds(now_us));
    } else {
        log_warn!(
            "STUTTER MARK %u (%s) at %.3f s: the last stutter was %.0f ms before",
            n,
            how,
            seconds(now_us),
            (now_us - last) as f64 / 1000.0
        );
    }
}

pub fn ending() {
    ENDING.store(true, Ordering::Relaxed);
}

pub fn report() {
    let Some(r) = crate::runtime::lock_for_report(&RENDERER) else { return };
    let Some(w) = crate::runtime::lock_for_report(&WINDOW) else { return };
    let minutes = r.play_us as f64 / 60e6;
    log_info!(
        "stutters: %llu in %.1f s of play (%.1f a minute), %llu frames lost; by length: 1:%llu 2:%llu 3:%llu 4-7:%llu 8-15:%llu 16+:%llu; %llu pauses over a second",
        r.stutters,
        seconds(r.play_us),
        if minutes > 0.0 { r.stutters as f64 / minutes } else { 0.0 },
        r.frames_lost,
        r.by_length[0],
        r.by_length[1],
        r.by_length[2],
        r.by_length[3],
        r.by_length[4],
        r.by_length[5],
        r.pauses
    );
    let mut spent = Vec::new();
    for (c, name) in COST_NAMES.iter().enumerate() {
        let comma = if c != 0 { c", " } else { c"" };
        spent.extend_from_slice(format_text!("%s%.1f ms %s", comma, r.late_cost_ns[c] as f64 / 1e6, *name).as_bytes());
    }
    log_info!(
        "stutters: %llu with the title presenting late, %llu with it on time and the renderer late; across the renderer's: %s",
        r.title_late,
        r.renderer_late,
        c_string(spent).as_c_str()
    );
    let mut spent = Vec::new();
    for c in 0..COSTS {
        let comma = if c != 0 { c", " } else { c"" };
        let nanoseconds = COST[c].nanoseconds.load(Ordering::SeqCst);
        let times = COST[c].times.load(Ordering::SeqCst);
        spent.extend_from_slice(
            format_text!("%s%.0f ms %s (%llu)", comma, nanoseconds as f64 / 1e6, COST_NAMES[c], times).as_bytes(),
        );
    }
    log_info!("stutters: over the whole run: %s", c_string(spent).as_c_str());
    log_info!(
        "stutters: the window showed %llu frames in play and let %llu go unshown; the display held a frame %llu times, for %llu blanks in all; %u marks",
        w.presents,
        w.frames_skipped,
        w.held,
        w.blanks_held,
        MARKS.load(Ordering::SeqCst)
    );
}

/// A stall: play was going and no world frame has come for a second. What the time went
/// on is not the renderer's to say, so every guest thread's wait and every thread's stack
/// go to the log, once a stall, as `kill -USR2` gives them.
#[cfg(not(windows))]
pub fn watch_for_stalls() {
    use crate::runtime::pacing_trace::microseconds;
    use crate::runtime::{clock, crash, kernel};
    use std::time::Duration;
    let mut reported = -1;
    while !ENDING.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(100));
        let last = LAST_WORLD_US.load(Ordering::Relaxed);
        if ENDING.load(Ordering::Relaxed)
            || !IN_PLAY.load(Ordering::Relaxed)
            || last == reported
            || microseconds(clock::nanoseconds()) - last < PAUSE_US
        {
            continue;
        }
        reported = last;
        log_warn!("STALL: no world frame since %.3f s; what every thread is doing follows", seconds(last));
        kernel::sync_objects::report_waits();
        crash::sample_every_thread();
    }
}

#[cfg(windows)]
pub fn watch_for_stalls() {}
