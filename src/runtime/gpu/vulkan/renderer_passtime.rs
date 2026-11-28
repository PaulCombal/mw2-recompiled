// MW2_TIME_PASSES=<seconds>: where the GPU's time goes inside a frame.
//
// A timestamp is written at the bottom of the pipe after each rendering instance ends and
// before the next begins: the GPU writes it once everything before it is done, so the time
// between two is the work between them. A pass's barrier out waits for the pass, so passes
// do not overlap much and the split is close to what each costs. Uploads and copies between
// passes go to "between passes". From that far into the run, by the target each pass draws
// into; written at the exit. Off, each call is one test of a flag.
//
// Unsafe by nature: the query pool is the renderer's device's, reset and written by
// commands queued on the recorder, read once the slot's submission is done, destroyed with
// the device idle.
#![deny(unsafe_op_in_unsafe_fn)]

use super::device::Vk;
use super::recorder;
use super::renderer_state::FRAME_SLOTS;
use super::renderer_targets::Target;
use crate::log::{log_info, log_warn};
use crate::runtime::gpu::registers::color_target_format_name;
use ash::vk;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const MARKS: u32 = 4096; // a slot's, a submission at most
const BETWEEN: u16 = 0;
// A name's most bytes: what snprintf keeps of a longer one in 96.
const NAME_BYTES: usize = 95;

struct Label {
    name: Vec<u8>,
    ticks: u64,
    instances: u64,
    draws: u64,
}

impl Label {
    fn new(name: Vec<u8>) -> Label {
        Label { name, ticks: 0, instances: 0, draws: 0 }
    }
}

/// The pass timer's state.
pub struct PassTimes {
    on: bool,
    from: Option<Instant>,
    pool: vk::QueryPool,
    nanoseconds_per_tick: f64,
    mask: u64,
    // What each mark of a slot's submission closes, and the draws made since the mark before.
    labels: [Vec<u16>; FRAME_SLOTS as usize],
    draws: [Vec<u32>; FRAME_SLOTS as usize],
    slot: u32,
    draws_at_mark: u64,
    table: Vec<Label>,
    by_name: BTreeMap<Vec<u8>, u16>,
    open: u16, // the label of the pass open, BETWEEN when none
    counting: bool,
    frames_at_start: u64,
    submissions: u64,
    results: Vec<u64>,
}

impl Default for PassTimes {
    fn default() -> Self {
        PassTimes::new()
    }
}

impl PassTimes {
    pub fn new() -> PassTimes {
        PassTimes {
            on: false,
            from: None,
            pool: vk::QueryPool::null(),
            nanoseconds_per_tick: 1.0,
            mask: !0,
            labels: Default::default(),
            draws: Default::default(),
            slot: 0,
            draws_at_mark: 0,
            table: Vec::new(),
            by_name: BTreeMap::new(),
            open: BETWEEN,
            counting: false,
            frames_at_start: 0,
            submissions: 0,
            results: Vec::new(),
        }
    }

    fn label_for(&mut self, name: &[u8]) -> u16 {
        let name = &name[..name.len().min(NAME_BYTES)];
        if let Some(&at) = self.by_name.get(name) {
            return at;
        }
        let at = self.table.len() as u16;
        self.by_name.insert(name.to_vec(), at);
        self.table.push(Label::new(name.to_vec()));
        at
    }

    fn mark(&mut self, label: u16, draws: u64) {
        let slot = self.slot as usize;
        if self.labels[slot].len() >= MARKS as usize {
            return;
        }
        let pool = self.pool;
        let query = self.slot * MARKS + self.labels[slot].len() as u32;
        self.labels[slot].push(label);
        self.draws[slot].push(draws.wrapping_sub(self.draws_at_mark) as u32);
        self.draws_at_mark = draws;
        recorder::command(move |c| {
            // SAFETY: the recorder's command buffer; the pool lives until stop, after the
            // device is idle.
            unsafe { c.vk.cmd_write_timestamp(c.into, vk::PipelineStageFlags::BOTTOM_OF_PIPE, pool, query) }
        });
    }

    // The slot's last submission is done: its marks are all written.
    fn collect(&mut self, vk: &Vk, slot: u32) {
        let s = slot as usize;
        let count = self.labels[s].len();
        if count > 1 && self.counting {
            self.results.resize(count, 0);
            let mut bytes = vec![0u8; count * 8];
            // SAFETY: the pool's queries of this slot, written by a submission that is done.
            let result = unsafe {
                vk.get_query_pool_results(
                    self.pool,
                    slot * MARKS,
                    count as u32,
                    &mut bytes,
                    8,
                    vk::QueryResultFlags::TYPE_64,
                )
            };
            if result == vk::Result::SUCCESS {
                for (out, word) in self.results.iter_mut().zip(bytes.chunks_exact(8)) {
                    *out = u64::from_ne_bytes([word[0], word[1], word[2], word[3], word[4], word[5], word[6], word[7]]);
                }
                for i in 1..count {
                    let label = &mut self.table[self.labels[s][i] as usize];
                    label.ticks =
                        label.ticks.wrapping_add(self.results[i].wrapping_sub(self.results[i - 1]) & self.mask);
                    label.draws = label.draws.wrapping_add(u64::from(self.draws[s][i]));
                    if self.labels[s][i] != BETWEEN {
                        label.instances += 1;
                    }
                }
                self.submissions += 1;
            }
        }
        self.labels[s].clear();
        self.draws[s].clear();
    }

    /// At the renderer's initialise, unless MW2_DETERMINISTIC.
    ///
    /// # Safety
    /// `vk` is the renderer's device, of `physical`.
    pub unsafe fn start(&mut self, vk: &Vk, physical: vk::PhysicalDevice, family: u32) {
        if !cfg!(feature = "diagnostics") {
            return;
        }
        let seconds = crate::env::number(c"MW2_TIME_PASSES", u64::MAX);
        if seconds == u64::MAX {
            return;
        }
        // SAFETY: the caller's.
        let (bits, period) = unsafe {
            let families = vk.get_physical_device_queue_family_properties(physical, None);
            let mut properties = vec![vk::QueueFamilyProperties::default(); families as usize];
            vk.get_physical_device_queue_family_properties(physical, Some(&mut properties));
            let device = vk.get_physical_device_properties(physical);
            let bits = if family < families { properties[family as usize].timestamp_valid_bits } else { 0 };
            (bits, device.limits.timestamp_period)
        };
        if bits == 0 {
            log_warn!("renderer: MW2_TIME_PASSES: the queue keeps no time");
            return;
        }
        self.mask = if bits >= 64 { !0 } else { (1u64 << bits) - 1 };
        self.nanoseconds_per_tick = f64::from(period);
        let queries = vk::QueryPoolCreateInfo {
            query_type: vk::QueryType::TIMESTAMP,
            query_count: FRAME_SLOTS * MARKS,
            ..Default::default()
        };
        let mut pool = vk::QueryPool::null();
        // SAFETY: the caller's device.
        if unsafe { vk.create_query_pool(&queries, &mut pool) } != vk::Result::SUCCESS {
            return;
        }
        self.pool = pool;
        self.table.push(Label::new(b"between passes: copies, resolves, clears, uploads".to_vec()));
        // None when too far for the clock to hold: a time no run reaches.
        self.from = Instant::now().checked_add(Duration::from_secs(seconds));
        self.on = true;
        log_info!("renderer: timing the GPU's passes from %llu s into the run (MW2_TIME_PASSES)", seconds);
    }

    /// At a slot's reuse, once its last submission is done.
    pub fn begin(&mut self, vk: &Vk, slot: u32, frames: u64, draws: u64) {
        if !self.on {
            return;
        }
        self.collect(vk, slot);
        if !self.counting && self.from.is_some_and(|from| Instant::now() >= from) {
            self.counting = true;
            self.frames_at_start = frames;
        }
        self.slot = slot;
        self.open = BETWEEN;
        let pool = self.pool;
        recorder::command(move |c| {
            // SAFETY: the recorder's command buffer; the pool lives until stop.
            unsafe { c.vk.cmd_reset_query_pool(c.into, pool, slot * MARKS, MARKS) }
        });
        self.mark(BETWEEN, draws);
    }

    pub fn open(&mut self, colour: &Target, samples: u32, draws: u64) {
        if !self.on {
            return;
        }
        self.mark(BETWEEN, draws);
        let name = crate::log::format_text!(
            "%ux%u %ux %s",
            colour.width.wrapping_mul(colour.scale),
            colour.height.wrapping_mul(colour.scale),
            samples,
            color_target_format_name(colour.guest_format).unwrap_or(c"?")
        );
        self.open = self.label_for(name.to_bytes());
    }

    pub fn close(&mut self, draws: u64) {
        if !self.on || self.open == BETWEEN {
            return;
        }
        self.mark(self.open, draws);
        self.open = BETWEEN;
    }

    /// Ahead of a pass, and at the end of a submission: what came since the last mark.
    pub fn between(&mut self, draws: u64) {
        if self.on {
            self.mark(BETWEEN, draws);
        }
    }

    /// What came since the last mark, by a name of its own.
    pub fn mark_named(&mut self, name: std::fmt::Arguments<'_>, draws: u64) {
        if !self.on {
            return;
        }
        let name = std::fmt::format(name);
        let label = self.label_for(name.as_bytes());
        self.mark(label, draws);
    }

    /// With the device idle.
    ///
    /// # Safety
    /// `vk` is the renderer's device, idle.
    pub unsafe fn stop(&mut self, vk: &Vk, frames: u64) {
        if !self.on {
            return;
        }
        for slot in 0..FRAME_SLOTS {
            self.collect(vk, slot);
        }
        let frames = frames.wrapping_sub(self.frames_at_start).max(1);
        let all = self.table.iter().fold(0u64, |sum, label| sum.wrapping_add(label.ticks));
        let mut order: Vec<&Label> = self.table.iter().filter(|label| label.ticks != 0).collect();
        // By ticks, most first, as libstdc++'s std::sort leaves labels of equal ticks.
        cxx_sort(&mut order, |a, b| a.ticks > b.ticks);
        let ms = |ticks: u64| ticks as f64 * self.nanoseconds_per_tick / 1e6;
        log_info!(
            "renderer: the GPU's passes over %llu frames (%llu submissions): %.2f ms a frame",
            frames,
            self.submissions,
            ms(all) / frames as f64
        );
        for label in order {
            let name = crate::log::c_string(label.name.clone());
            log_info!(
                "renderer:   %6.2f ms a frame %5.1f%%  %6.1f passes %7.1f draws  %s",
                ms(label.ticks) / frames as f64,
                100.0 * label.ticks as f64 / all.max(1) as f64,
                label.instances as f64 / frames as f64,
                label.draws as f64 / frames as f64,
                name.as_c_str()
            );
        }
        // SAFETY: the caller's: the device is idle.
        unsafe { vk.destroy_query_pool(self.pool) };
        self.pool = vk::QueryPool::null();
        self.on = false;
    }
}

// ---- libstdc++'s std::sort (bits/stl_algo.h), whose order of equal elements the report's
// lines follow: an introsort to runs of 16, depth-limited by twice the log of the length
// (then a heap sort), finished by an insertion sort. ----

/// std::sort(first, last, less) as libstdc++ makes it.
pub fn cxx_sort<T: Copy>(v: &mut [T], less: impl Fn(&T, &T) -> bool + Copy) {
    let n = v.len();
    if n == 0 {
        return;
    }
    introsort_loop(v, 0, n, 2 * (usize::BITS - 1 - n.leading_zeros()) as usize, less);
    final_insertion_sort(v, 0, n, less);
}

const THRESHOLD: usize = 16;

fn introsort_loop<T: Copy>(
    v: &mut [T],
    first: usize,
    mut last: usize,
    mut depth: usize,
    less: impl Fn(&T, &T) -> bool + Copy,
) {
    while last - first > THRESHOLD {
        if depth == 0 {
            heap_sort(v, first, last, less);
            return;
        }
        depth -= 1;
        let mid = first + (last - first) / 2;
        move_median_to_first(v, first, first + 1, mid, last - 1, less);
        let cut = unguarded_partition(v, first + 1, last, first, less);
        introsort_loop(v, cut, last, depth, less);
        last = cut;
    }
}

fn move_median_to_first<T: Copy>(
    v: &mut [T],
    result: usize,
    a: usize,
    b: usize,
    c: usize,
    less: impl Fn(&T, &T) -> bool,
) {
    let pick = if less(&v[a], &v[b]) {
        if less(&v[b], &v[c]) {
            b
        } else if less(&v[a], &v[c]) {
            c
        } else {
            a
        }
    } else if less(&v[a], &v[c]) {
        a
    } else if less(&v[b], &v[c]) {
        c
    } else {
        b
    };
    v.swap(result, pick);
}

fn unguarded_partition<T: Copy>(
    v: &mut [T],
    mut first: usize,
    mut last: usize,
    pivot: usize,
    less: impl Fn(&T, &T) -> bool,
) -> usize {
    loop {
        while less(&v[first], &v[pivot]) {
            first += 1;
        }
        last -= 1;
        while less(&v[pivot], &v[last]) {
            last -= 1;
        }
        if first >= last {
            return first;
        }
        v.swap(first, last);
        first += 1;
    }
}

fn final_insertion_sort<T: Copy>(v: &mut [T], first: usize, last: usize, less: impl Fn(&T, &T) -> bool + Copy) {
    if last - first > THRESHOLD {
        insertion_sort(v, first, first + THRESHOLD, less);
        for i in first + THRESHOLD..last {
            unguarded_linear_insert(v, i, less);
        }
    } else {
        insertion_sort(v, first, last, less);
    }
}

fn insertion_sort<T: Copy>(v: &mut [T], first: usize, last: usize, less: impl Fn(&T, &T) -> bool + Copy) {
    if first == last {
        return;
    }
    for i in first + 1..last {
        if less(&v[i], &v[first]) {
            let value = v[i];
            v.copy_within(first..i, first + 1);
            v[first] = value;
        } else {
            unguarded_linear_insert(v, i, less);
        }
    }
}

fn unguarded_linear_insert<T: Copy>(v: &mut [T], mut last: usize, less: impl Fn(&T, &T) -> bool) {
    let value = v[last];
    let mut next = last - 1;
    while less(&value, &v[next]) {
        v[last] = v[next];
        last = next;
        next -= 1;
    }
    v[last] = value;
}

fn heap_sort<T: Copy>(v: &mut [T], first: usize, last: usize, less: impl Fn(&T, &T) -> bool + Copy) {
    // __partial_sort(first, last, last): make the heap, then sort it.
    let len = last - first;
    if len >= 2 {
        let mut parent = (len - 2) / 2;
        loop {
            let value = v[first + parent];
            adjust_heap(v, first, parent, len, value, less);
            if parent == 0 {
                break;
            }
            parent -= 1;
        }
    }
    let mut end = last;
    while end - first > 1 {
        end -= 1;
        let value = v[end];
        v[end] = v[first];
        adjust_heap(v, first, 0, end - first, value, less);
    }
}

fn adjust_heap<T: Copy>(
    v: &mut [T],
    first: usize,
    mut hole: usize,
    len: usize,
    value: T,
    less: impl Fn(&T, &T) -> bool,
) {
    let top = hole;
    let mut child = hole;
    while child < (len - 1) / 2 {
        child = 2 * (child + 1);
        if less(&v[first + child], &v[first + child - 1]) {
            child -= 1;
        }
        v[first + hole] = v[first + child];
        hole = child;
    }
    if len & 1 == 0 && child == (len - 2) / 2 {
        child = 2 * (child + 1);
        v[first + hole] = v[first + child - 1];
        hole = child - 1;
    }
    // __push_heap(first, hole, top, value)
    let mut parent = hole.wrapping_sub(1) / 2;
    while hole > top && less(&v[first + parent], &value) {
        v[first + hole] = v[first + parent];
        hole = parent;
        parent = hole.wrapping_sub(1) / 2;
    }
    v[first + hole] = value;
}
