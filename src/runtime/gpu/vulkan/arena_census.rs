//! What the frame arena's copies were, for the report of a diagnostics build: the
//! constants by what they are, why a draw's fetch block was assembled anew, why a vertex
//! window was copied rather than read from the shadow, and how much of what was copied
//! an earlier draw of the frame had copied already. Ring consumer only; counted with a
//! relaxed load and store, as the command processor's statistics are.

use crate::log::{c_string, log_info};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[derive(Clone, Copy)]
#[repr(usize)]
pub enum Constants {
    VertexBlock,
    Table,
    PixelWindow,
    FetchBlock,
    Booleans,
    Loops,
}
const CONSTANTS: usize = 6;
const CONSTANT_NAMES: [&str; CONSTANTS] =
    ["vertex blocks", "vertex table", "pixel window", "fetch block", "booleans", "loops"];

/// Why the fetch block was not the previous draw's, the first reason that holds.
#[derive(Clone, Copy, PartialEq)]
#[repr(usize)]
pub enum FetchMiss {
    Reused,
    Rectangles,
    NewFrame,
    Stamp,
    Range,
    Subs,
    Pairs,
}
const FETCH_CAUSES: usize = 7;
const FETCH_NAMES: [&str; FETCH_CAUSES] = [
    "reused",
    "a rectangle list",
    "first of its frame slot",
    "fetch constants written",
    "another vertex range",
    "other fetch subs",
    "other pairs read",
];

/// Why a vertex window was copied and not served from the shadow.
#[derive(Clone, Copy)]
#[repr(usize)]
pub enum VertexCopy {
    NoShadow,
    NotPhysical,
    Outside,
    InFlight,
    BackingOff,
    ArmFailed,
}
const VERTEX_CAUSES: usize = 6;
const VERTEX_NAMES: [&str; VERTEX_CAUSES] = [
    "no shadow",
    "address outside the physical window",
    "past the physical bank",
    "a page in flight",
    "a page backing off",
    "a page's copy failed",
];

pub struct Counter {
    count: AtomicU64,
    bytes: AtomicU64,
}

impl Counter {
    const fn new() -> Self {
        Counter { count: AtomicU64::new(0), bytes: AtomicU64::new(0) }
    }
    #[inline]
    fn add(&self, bytes: u64) {
        self.count.store(self.count.load(Relaxed).wrapping_add(1), Relaxed);
        self.bytes.store(self.bytes.load(Relaxed).wrapping_add(bytes), Relaxed);
    }
}

static CONSTANT_COPIES: [Counter; CONSTANTS] = [const { Counter::new() }; CONSTANTS];
static FETCH: [Counter; FETCH_CAUSES] = [const { Counter::new() }; FETCH_CAUSES];
// Of the fetch blocks assembled anew, those whose every vertex fetch the shadow served,
// and, of those missed for a written stamp, those whose pairs held the values they had.
static FETCH_ALL_SERVED: [AtomicU64; FETCH_CAUSES] = [const { AtomicU64::new(0) }; FETCH_CAUSES];
static FETCH_SAME_VALUES: AtomicU64 = AtomicU64::new(0);
static VERTEX: [Counter; VERTEX_CAUSES] = [const { Counter::new() }; VERTEX_CAUSES];
static VERTEX_AGAIN: AtomicU64 = AtomicU64::new(0); // copied bytes an earlier draw of the frame copied
static VERTEX_SERVED: Counter = Counter::new();
static INDICES: Counter = Counter::new();

#[inline]
fn bump(counter: &AtomicU64, by: u64) {
    counter.store(counter.load(Relaxed).wrapping_add(by), Relaxed);
}

pub fn constants(kind: Constants, bytes: u32) {
    CONSTANT_COPIES[kind as usize].add(u64::from(bytes));
}

pub fn fetch(cause: FetchMiss, bytes: u32) {
    FETCH[cause as usize].add(u64::from(bytes));
}

pub fn fetch_all_served(cause: FetchMiss) {
    bump(&FETCH_ALL_SERVED[cause as usize], 1);
}

pub fn fetch_same_values() {
    bump(&FETCH_SAME_VALUES, 1);
}

pub fn vertex_served(bytes: u32) {
    VERTEX_SERVED.add(u64::from(bytes));
}

pub fn indices(bytes: u32) {
    INDICES.add(u64::from(bytes));
}

/// The vertex windows copied this frame, by guest physical address: what a later draw's
/// copy of the same bytes counts as copied again.
#[derive(Default)]
pub struct Copied {
    windows: HashMap<u32, Vec<(u32, u32, u64)>>,
}

// The window's bytes, to tell a repeat that copies what an earlier draw copied from one
// that copies bytes the title has rewritten since.
fn digest(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut chunks = bytes.chunks_exact(8);
    for c in &mut chunks {
        h = (h ^ u64::from_le_bytes(c.try_into().unwrap_or([0; 8]))).wrapping_mul(0x100_0000_01b3).rotate_left(29);
    }
    for &b in chunks.remainder() {
        h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
    }
    h
}

static VERTEX_REPEAT: AtomicU64 = AtomicU64::new(0); // bytes of windows copied before, whole
static VERTEX_REPEAT_SAME: AtomicU64 = AtomicU64::new(0); // and of those, holding the same bytes

impl Copied {
    pub fn new_frame(&mut self) {
        self.windows.clear();
    }

    /// Dwords [first, last) of the buffer at `physical` were copied, for `why`; `bytes`
    /// are the guest's.
    pub fn copied(&mut self, physical: u32, first: u32, last: u32, why: VertexCopy, bytes: &[u8]) {
        VERTEX[why as usize].add(u64::from(last - first) * 4);
        let earlier = self.windows.entry(physical).or_default();
        // The part of [first, last) the earlier windows cover, each dword once.
        let mut covered = 0u64;
        let mut cursor = first;
        let mut spans: Vec<(u32, u32)> = earlier
            .iter()
            .filter(|&&(a, b, _)| a < last && b > first)
            .map(|&(a, b, _)| (a.max(first), b.min(last)))
            .collect();
        spans.sort_unstable();
        for (a, b) in spans {
            let a = a.max(cursor);
            if b > a {
                covered += u64::from(b - a);
                cursor = b;
            }
        }
        bump(&VERTEX_AGAIN, covered * 4);
        let hash = digest(bytes);
        if let Some(&(_, _, before)) = earlier.iter().rev().find(|&&(a, b, _)| a == first && b == last) {
            bump(&VERTEX_REPEAT, u64::from(last - first) * 4);
            if before == hash {
                bump(&VERTEX_REPEAT_SAME, u64::from(last - first) * 4);
            }
        }
        if earlier.len() < 64 {
            earlier.push((first, last, hash));
        }
    }
}

/// The census, per frame, after the arena's own lines.
pub fn report(frames: u64) {
    if frames == 0 {
        return;
    }
    let per = |counter: &Counter| (counter.count.load(Relaxed) / frames, (counter.bytes.load(Relaxed) / frames) >> 10);
    for (kind, name) in CONSTANT_NAMES.iter().enumerate() {
        let (count, kb) = per(&CONSTANT_COPIES[kind]);
        let name = c_string(*name);
        log_info!("renderer: constants copied, %-14s %6llu a frame, %6llu KB", name.as_c_str(), count, kb);
    }
    for (cause, name) in FETCH_NAMES.iter().enumerate() {
        let (count, kb) = per(&FETCH[cause]);
        let name = c_string(*name);
        log_info!(
            "renderer: fetch block %-26s %6llu a frame, %6llu KB written, %6llu a frame with every vertex fetch served",
            name.as_c_str(),
            count,
            kb,
            FETCH_ALL_SERVED[cause].load(Relaxed) / frames
        );
    }
    log_info!(
        "renderer: fetch block missed for written fetch constants whose pairs held the same values: %llu a frame",
        FETCH_SAME_VALUES.load(Relaxed) / frames
    );
    let (count, kb) = per(&VERTEX_SERVED);
    log_info!("renderer: vertex windows served by the shadow %6llu a frame, %6llu KB", count, kb);
    for (cause, name) in VERTEX_NAMES.iter().enumerate() {
        let (count, kb) = per(&VERTEX[cause]);
        if count == 0 {
            continue;
        }
        let name = c_string(*name);
        log_info!("renderer: vertex windows copied, %-36s %6llu a frame, %6llu KB", name.as_c_str(), count, kb);
    }
    log_info!(
        "renderer: vertex bytes copied that an earlier draw of the frame had copied: %llu KB a frame",
        (VERTEX_AGAIN.load(Relaxed) / frames) >> 10
    );
    log_info!(
        "renderer: of them, windows an earlier draw copied whole: %llu KB a frame, %llu KB of it holding the same bytes",
        (VERTEX_REPEAT.load(Relaxed) / frames) >> 10,
        (VERTEX_REPEAT_SAME.load(Relaxed) / frames) >> 10
    );
    {
        use super::copy_worker::{HANDED, MADE_BY_CONSUMER, SETTLES_WAITED};
        log_info!(
            "renderer: vertex copies handed to the copy thread %llu a frame, %llu of them made by the consumer, which waited on the copy thread at %llu settles a frame",
            HANDED.load(Relaxed) / frames,
            MADE_BY_CONSUMER.load(Relaxed) / frames,
            SETTLES_WAITED.load(Relaxed) / frames
        );
    }
    let (count, kb) = per(&INDICES);
    log_info!("renderer: index swaps %llu a frame, %llu KB", count, kb);
}
