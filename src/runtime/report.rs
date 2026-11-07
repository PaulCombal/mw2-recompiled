// MW2_REPORT=1: see runtime/report.h. What a run did is worked out here from the times
// and readings the caller gives, so that tools/runtime_diff.py can give it the same
// ones it gives the C++; exports.rs gives it the clock and the System below.

use crate::env;
use crate::log::{c_string, format_text, log_info};
use crate::runtime::bridge::{self, WatchCosts};
use crate::runtime::platform::{self, Load, MemoryState};
use std::cell::Cell;
use std::ffi::CStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard};

// A frame every blank is 16.7 ms; the bounds are a blank and a half, then two, three
// and four blanks.
const BOUNDS: [f64; 4] = [17.5, 25.0, 34.0, 50.0];
const BUCKETS: usize = BOUNDS.len() + 1;

pub const SHADER: usize = 0;
pub const PIPELINE: usize = 1;
const COSTS: usize = 2;

pub const EXECUTING: usize = 0;
pub const RECORDER_WAIT: usize = 1;
const GPU_WAIT: usize = 2;
const TEXTURES: usize = 3;
const COMPILING: usize = 4;
pub const GPU_BUSY: usize = 5;
const TIMES: usize = 6;

/// The machine, as a report reads it.
pub trait Machine {
    fn load(&self, load: &mut Load) -> bool;
    fn memory(&self) -> MemoryState;
    fn watch(&self) -> WatchCosts;
    fn thread_clock(&self) -> usize;
    fn thread_time(&self, clock: usize) -> Option<u64>;
    fn processor_threads(&self) -> u32;
}

pub struct System;

impl Machine for System {
    fn load(&self, load: &mut Load) -> bool {
        platform::processor_load(load)
    }

    fn memory(&self) -> MemoryState {
        let mut memory = MemoryState::default();
        platform::memory_state(&mut memory);
        memory
    }

    fn watch(&self) -> WatchCosts {
        bridge::watch_costs()
    }

    fn thread_clock(&self) -> usize {
        platform::thread_clock()
    }

    fn thread_time(&self, clock: usize) -> Option<u64> {
        platform::thread_time(clock)
    }

    fn processor_threads(&self) -> u32 {
        platform::processor_threads()
    }
}

struct State {
    first: u64,
    last: u64,
    last_was_world: bool,
    frames: u64,
    world_frames: u64,
    // Between two world frames in a row: a loading screen in between is not a slow
    // frame.
    intervals: u64,
    buckets: [u64; BUCKETS],
    interval_ms: f64,
    longest_ms: f64,
    // The consumer's times as they stood at the last frame, and what the intervals
    // counted above took of them.
    spent_at_last: [u64; TIMES],
    spent_in_play: [u64; TIMES],
    cost_count: [u64; COSTS],
    cost_microseconds: [u64; COSTS],
    cost_longest: [u64; COSTS],
    written: bool,
    compiled: Option<Vec<u8>>,

    // The machine's load in play, read once a second: whether the processor was ours
    // or another program's, which of our threads took it, and how close the memory
    // came to running out. A few system calls a second.
    sampled: u64,
    started: bool,
    all_play: bool, // every frame since `sampled` was of play
    loaded: bool,
    last_load: Load,
    in_play: Load,
    play_nanoseconds: u64,
    least_free: u64,
    most_committed: u64,
    watch: WatchCosts,
    watch_in_play: WatchCosts,
}

static STATE: Mutex<State> = Mutex::new(State {
    first: 0,
    last: 0,
    last_was_world: false,
    frames: 0,
    world_frames: 0,
    intervals: 0,
    buckets: [0; BUCKETS],
    interval_ms: 0.0,
    longest_ms: 0.0,
    spent_at_last: [0; TIMES],
    spent_in_play: [0; TIMES],
    cost_count: [0; COSTS],
    cost_microseconds: [0; COSTS],
    cost_longest: [0; COSTS],
    written: false,
    compiled: None,
    sampled: 0,
    started: false,
    all_play: false,
    loaded: false,
    last_load: Load { process: 0, busy: 0, total: 0 },
    in_play: Load { process: 0, busy: 0, total: 0 },
    play_nanoseconds: 0,
    least_free: u64::MAX,
    most_committed: 0,
    watch: WatchCosts { faults: 0, protects: 0, protect_nanoseconds: 0 },
    watch_in_play: WatchCosts { faults: 0, protects: 0, protect_nanoseconds: 0 },
});

// Written by the ring consumer between frames, outside the lock.
static SPENT: [AtomicU64; TIMES] = [const { AtomicU64::new(0) }; TIMES];
// When the ring batch being executed began, 0 when none is. A frame ends in the middle
// of one, so its part up to there is counted with the frame.
static EXECUTING_SINCE: AtomicU64 = AtomicU64::new(0);
// By ShaderBuilt's from_cache, plus one.
static SHADERS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

const NOT_USED: &[u8] = b"not used: a development run";

struct Thread {
    name: Vec<u8>,
    clock: usize,
    last: u64,
    in_play: u64,
    unsampled: u64, // between the last reading and the thread's end
    read: bool,
    ended: bool,
}

static THREADS: Mutex<Vec<Thread>> = Mutex::new(Vec::new());

thread_local! {
    // This thread's place in THREADS, plus one.
    static THREAD: Cell<usize> = const { Cell::new(0) };
}

static ON: LazyLock<bool> = LazyLock::new(|| env::flag(c"MW2_REPORT"));

pub fn on() -> bool {
    *ON
}

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn threads() -> MutexGuard<'static, Vec<Thread>> {
    THREADS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// Under the state's lock. `play` when the second since the last reading was all play.
fn sample(s: &mut State, machine: &impl Machine, play: bool, since: u64) {
    let mut load = Load::default();
    let loaded = machine.load(&mut load);
    let memory = machine.memory();
    for thread in threads().iter_mut() {
        if thread.ended {
            if play {
                thread.in_play = thread.in_play.wrapping_add(thread.unsampled);
            }
            thread.unsampled = 0;
            continue;
        }
        let Some(now) = machine.thread_time(thread.clock) else {
            thread.ended = true;
            continue;
        };
        if play && thread.read {
            thread.in_play = thread.in_play.wrapping_add(now.wrapping_sub(thread.last));
        }
        thread.last = now;
        thread.read = true;
    }
    if play && loaded && s.loaded {
        s.in_play.process = s.in_play.process.wrapping_add(load.process.wrapping_sub(s.last_load.process));
        s.in_play.busy = s.in_play.busy.wrapping_add(load.busy.wrapping_sub(s.last_load.busy));
        s.in_play.total = s.in_play.total.wrapping_add(load.total.wrapping_sub(s.last_load.total));
        s.play_nanoseconds = s.play_nanoseconds.wrapping_add(since);
    }
    if loaded {
        s.last_load = load;
        s.loaded = true;
    }
    let watch = machine.watch();
    if play {
        let (in_play, was) = (&mut s.watch_in_play, &s.watch);
        in_play.faults = in_play.faults.wrapping_add(watch.faults.wrapping_sub(was.faults));
        in_play.protects = in_play.protects.wrapping_add(watch.protects.wrapping_sub(was.protects));
        in_play.protect_nanoseconds =
            in_play.protect_nanoseconds.wrapping_add(watch.protect_nanoseconds.wrapping_sub(was.protect_nanoseconds));
    }
    s.watch = watch;
    if play && memory.usable != 0 {
        s.least_free = s.least_free.min(memory.free);
    }
    s.most_committed = s.most_committed.max(memory.committed);
}

fn gigabytes(bytes: u64) -> Vec<u8> {
    format_text!("%.1f GB", bytes as f64 / (1u64 << 30) as f64).into_bytes()
}

pub fn thread_started(machine: &impl Machine, name: &CStr) {
    if !on() {
        return;
    }
    // "guest thread 12 (routine 8228B2C8)" is guest thread 12 here.
    let name = name.to_bytes();
    let kept = name.windows(2).position(|pair| pair == b" (").unwrap_or(name.len());
    let thread = Thread {
        name: name[..kept].to_vec(),
        clock: machine.thread_clock(),
        last: 0,
        in_play: 0,
        unsampled: 0,
        read: false,
        ended: false,
    };
    let mut all = threads();
    all.push(thread);
    let place = all.len();
    THREAD.set(place);
}

pub fn thread_ended(machine: &impl Machine) {
    let place = THREAD.get();
    if place == 0 {
        return;
    }
    // Read on the way out: the clock goes with the thread.
    let clock = threads()[place - 1].clock;
    let now = machine.thread_time(clock);
    let mut all = threads();
    let thread = &mut all[place - 1];
    if let Some(now) = now
        && thread.read
    {
        thread.unsampled = now.wrapping_sub(thread.last);
    }
    thread.ended = true;
    THREAD.set(0);
}

pub fn shader_built(from_cache: i32) {
    SHADERS[(from_cache.clamp(-1, 1) + 1) as usize].fetch_add(1, Ordering::Relaxed);
}

pub fn shaders_compiled(text: &[u8]) {
    state().compiled = Some(text.to_vec());
}

pub fn timed_begin(now: u64, time: usize) -> u64 {
    if time == EXECUTING {
        EXECUTING_SINCE.store(now, Ordering::Relaxed);
    }
    now
}

pub fn timed_end(now: u64, time: usize, began: u64) {
    if time == EXECUTING {
        EXECUTING_SINCE.store(0, Ordering::Relaxed);
    }
    spent(time, now.wrapping_sub(began));
}

pub fn spent(time: usize, nanoseconds: u64) {
    SPENT[time].fetch_add(nanoseconds, Ordering::Relaxed);
}

pub fn frame(now: u64, machine: &impl Machine, world: bool) {
    let mut guard = state();
    let s = &mut *guard;
    if s.frames == 0 {
        s.first = now;
    }
    s.frames += 1;
    let mut times: [u64; TIMES] = std::array::from_fn(|time| SPENT[time].load(Ordering::Relaxed));
    let since = EXECUTING_SINCE.load(Ordering::Relaxed);
    if since != 0 {
        times[EXECUTING] = times[EXECUTING].wrapping_add(now.wrapping_sub(since));
    }
    if world {
        s.world_frames += 1;
        if s.last_was_world {
            for (time, &now_spent) in times.iter().enumerate() {
                s.spent_in_play[time] =
                    s.spent_in_play[time].wrapping_add(now_spent.wrapping_sub(s.spent_at_last[time]));
            }
            let ms = now.wrapping_sub(s.last) as f64 / 1e6;
            let bucket = BOUNDS.iter().take_while(|&&bound| ms > bound).count();
            s.buckets[bucket] += 1;
            s.intervals += 1;
            s.interval_ms += ms;
            if ms > s.longest_ms {
                s.longest_ms = ms;
            }
        }
    }
    s.spent_at_last = times;
    if !(world && s.last_was_world) {
        s.all_play = false;
    }
    s.last_was_world = world;
    s.last = now;
    if !s.started || now.wrapping_sub(s.sampled) >= 1_000_000_000 {
        let (play, since) = (s.started && s.all_play, now.wrapping_sub(s.sampled));
        sample(s, machine, play, since);
        s.started = true;
        s.sampled = now;
        s.all_play = true;
    }
}

pub fn add(cost: usize, microseconds: u64) {
    if !on() {
        return;
    }
    let mut s = state();
    s.cost_count[cost] += 1;
    s.cost_microseconds[cost] = s.cost_microseconds[cost].wrapping_add(microseconds);
    s.cost_longest[cost] = s.cost_longest[cost].max(microseconds);
    spent(COMPILING, microseconds.wrapping_mul(1000));
}

pub fn write(machine: &impl Machine) {
    if !on() {
        return;
    }
    let mut guard = state();
    let s = &mut *guard;
    if s.written {
        return;
    }
    s.written = true;
    let seconds = if s.frames != 0 { s.last.wrapping_sub(s.first) as f64 / 1e9 } else { 0.0 };
    log_info!("report: %llu frames in %.0f s, %llu of them of the world", s.frames, seconds, s.world_frames);
    if s.intervals != 0 {
        let share = |count: u64| 100.0 * count as f64 / s.intervals as f64;
        log_info!(
            "report: in play, %.1f frames a second on average; the longest frame took %.0f ms",
            1000.0 * s.intervals as f64 / s.interval_ms,
            s.longest_ms
        );
        log_info!(
            "report: frame times: %.1f%% within %.1f ms, %.1f%% to %.0f ms, %.1f%% to %.0f ms, %.1f%% to %.0f ms, %.1f%% longer",
            share(s.buckets[0]),
            BOUNDS[0],
            share(s.buckets[1]),
            BOUNDS[1],
            share(s.buckets[2]),
            BOUNDS[2],
            share(s.buckets[3]),
            BOUNDS[3],
            share(s.buckets[4])
        );
        let took = |time: usize| s.spent_in_play[time] as f64 / 1e4 / s.interval_ms;
        log_info!(
            "report: in play, the renderer's thread was at work %.0f%% of the time: %.0f%% waiting for the graphics chip, %.0f%% for its recorder thread, %.0f%% reading textures, %.0f%% compiling",
            took(EXECUTING),
            took(GPU_WAIT),
            took(RECORDER_WAIT),
            took(TEXTURES),
            took(COMPILING)
        );
        if s.spent_in_play[GPU_BUSY] != 0 {
            log_info!(
                "report: in play, the graphics chip was at work %.0f%% of the time, %.1f ms a frame",
                took(GPU_BUSY),
                s.spent_in_play[GPU_BUSY] as f64 / 1e6 / s.intervals as f64
            );
        }
    }
    if s.play_nanoseconds != 0 && s.in_play.total != 0 {
        let total = s.in_play.total as f64;
        let others = s.in_play.busy.saturating_sub(s.in_play.process);
        log_info!(
            "report: in play, the processor's %u threads were %.0f%% busy: %.0f%% with this game, %.0f%% with other programs",
            machine.processor_threads().max(1),
            100.0 * s.in_play.busy as f64 / total,
            100.0 * s.in_play.process as f64 / total,
            100.0 * others as f64 / total
        );
        // The busiest of ours; the rest has the driver's and the system's threads in it.
        let mut busiest: Vec<(u64, Vec<u8>)> = threads()
            .iter()
            .filter(|thread| thread.in_play != 0)
            .map(|thread| (thread.in_play, thread.name.clone()))
            .collect();
        busiest.sort_by_key(|&(time, _)| std::cmp::Reverse(time));
        let wall = s.play_nanoseconds as f64 / 100.0; // one thread's time, in %
        let mut list = Vec::new();
        let mut listed = 0u64;
        for (time, name) in busiest.iter().take(8) {
            if *time as f64 / wall < 1.0 {
                break;
            }
            if !list.is_empty() {
                list.extend_from_slice(b", ");
            }
            list.extend_from_slice(name);
            list.extend_from_slice(format_text!(" %.0f", *time as f64 / wall).as_bytes());
            listed = listed.wrapping_add(*time);
        }
        if !list.is_empty() {
            let rest = s.in_play.process.saturating_sub(listed) as f64 / wall;
            log_info!(
                "report: in play, the game's threads in %% of one processor thread: %s, the rest %.0f",
                c_string(list).as_c_str(),
                rest
            );
        }
    }
    if s.play_nanoseconds != 0 && s.watch_in_play.protects != 0 {
        // Write faults and protection changes are cheap on Linux; what they cost
        // elsewhere (Windows, more so with Memory Integrity on) is the question.
        let seconds = s.play_nanoseconds as f64 / 1e9;
        log_info!(
            "report: in play, the memory watch took %.0f write faults and %.0f protection changes a second; the changes took %.1f ms a second, %.1f us each",
            s.watch_in_play.faults as f64 / seconds,
            s.watch_in_play.protects as f64 / seconds,
            s.watch_in_play.protect_nanoseconds as f64 / 1e6 / seconds,
            s.watch_in_play.protect_nanoseconds as f64 / 1e3 / s.watch_in_play.protects as f64
        );
    }
    let memory = machine.memory();
    let mut line = b"report: memory: ".to_vec();
    line.extend(gigabytes(memory.usable));
    if memory.installed > memory.usable {
        line.extend_from_slice(b" usable of ");
        line.extend(gigabytes(memory.installed));
        line.extend_from_slice(b" (");
        line.extend(gigabytes(memory.installed - memory.usable));
        line.extend_from_slice(b" kept by the hardware)");
    } else {
        line.extend_from_slice(b" usable");
    }
    if s.least_free != u64::MAX {
        line.extend_from_slice(b"; in play at least ");
        line.extend(gigabytes(s.least_free));
        line.extend_from_slice(b" free");
    }
    if memory.commit_limit != 0 {
        line.extend_from_slice(b"; commit charge up to ");
        line.extend(gigabytes(s.most_committed.max(memory.committed)));
        line.extend_from_slice(b" of ");
        line.extend(gigabytes(memory.commit_limit));
    }
    if memory.process_peak != 0 {
        line.extend_from_slice(b"; this game at most ");
        line.extend(gigabytes(memory.process_peak));
        line.extend_from_slice(b" in memory");
    }
    if memory.process_committed != 0 {
        line.extend_from_slice(b" and ");
        line.extend(gigabytes(memory.process_committed));
        line.extend_from_slice(b" committed");
    }
    log_info!("%s", c_string(line).as_c_str());
    log_info!(
        "report: compiled at a draw: %llu shaders in %llu ms (the longest %llu ms), %llu pipelines in %llu ms (the longest %llu ms)",
        s.cost_count[SHADER],
        s.cost_microseconds[SHADER] / 1000,
        s.cost_longest[SHADER] / 1000,
        s.cost_count[PIPELINE],
        s.cost_microseconds[PIPELINE] / 1000,
        s.cost_longest[PIPELINE] / 1000
    );
    // Every one should have come out of the launcher's cache.
    let [unknown, compiled, cached] = [0, 1, 2].map(|kind| SHADERS[kind].load(Ordering::SeqCst));
    let state_of_pass = s.compiled.as_deref().unwrap_or(NOT_USED);
    log_info!("report: shaders compiled before play by the launcher: %s", c_string(state_of_pass).as_c_str());
    if unknown != 0 {
        log_info!(
            "report: %llu shaders built in this run, %llu of them not in the launcher's (the driver says so for %llu)",
            unknown.wrapping_add(compiled).wrapping_add(cached),
            compiled,
            compiled.wrapping_add(cached)
        );
    } else {
        log_info!(
            "report: %llu shaders built in this run, %llu of them not in the launcher's",
            compiled.wrapping_add(cached),
            compiled
        );
    }
}
