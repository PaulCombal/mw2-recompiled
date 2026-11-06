// How long the graphics chip is at work, for the report (MW2_REPORT=1): a timestamp
// before and after every submission to the queue, read back once the GPU has written
// them -- nothing ever waits for one. It says whether a slow run is the GPU's: the
// renderer's thread waiting on a fence is the GPU being full, or the two taking turns,
// and only the GPU's own clock tells them apart. Off, a submission is the plain
// vkQueueSubmit. runtime/gpu/vulkan/gpu_time.cpp in Rust.
//
// Unsafe by nature: the query pool and the command buffers are made on the device the
// renderer submits to, and destroyed by stop() with that device idle; submit() runs
// under vk::pipeline::QueueMutex like any submission, and the caller's command buffers
// and semaphores are valid for the call.
#![deny(unsafe_op_in_unsafe_fn)]

use crate::log::log_info;
use crate::runtime::{bridge, report};
use ash::vk;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, RwLock};

use super::QueueLock;

/// Pairs of queries, one a submission, taken in turn. A frame is a few submissions and
/// they are collected every frame, so the ring is never close to full; a submission
/// that finds it full goes untimed.
pub const PAIRS: u32 = 64;

/// The bits of a timestamp that count, for a queue family's timestampValidBits.
pub fn mask_of(bits: u32) -> u64 {
    if bits >= 64 { !0 } else { (1u64 << bits) - 1 }
}

/// The timestamps' mask and nanoseconds per tick, or None when the family keeps no time.
pub fn timing(family_bits: &[u32], family: u32, period: f32) -> Option<(u64, f64)> {
    let bits = family_bits.get(family as usize).copied().unwrap_or(0);
    if bits == 0 || period <= 0.0 {
        return None;
    }
    Some((mask_of(bits), f64::from(period)))
}

/// Whether timestamp `a` is before `b`, both read through the mask: they are never half
/// the counter's range apart.
#[inline]
pub fn before(a: u64, b: u64, mask: u64) -> bool {
    (b.wrapping_sub(a) & mask).wrapping_sub(1) < (mask >> 1)
}

/// The pairs handed to submissions.
#[derive(Default)]
pub struct Ring {
    pub taken: u64,
}

impl Ring {
    /// The pair for the next submission, or None while every pair waits to be collected.
    pub fn take(&mut self, collected: u64) -> Option<u32> {
        if self.taken.wrapping_sub(collected) >= u64::from(PAIRS) {
            return None;
        }
        let pair = (self.taken % u64::from(PAIRS)) as u32;
        self.taken += 1;
        Some(pair)
    }
}

/// What Collect keeps between calls.
pub struct Collector {
    /// A pair's results from its last round stay readable until the GPU reaches its
    /// reset; seeing them again is "not written yet".
    pub seen: [[u64; 2]; PAIRS as usize],
    pub last_end: u64,
    pub any_end: bool,
}

impl Collector {
    pub const fn new() -> Collector {
        Collector { seen: [[0; 2]; PAIRS as usize], last_end: 0, any_end: false }
    }

    /// The pairs from `collected` up to `taken`, in order, while each is written: how
    /// far it got, and the ticks the GPU was busy over them. `read` gives a pair's
    /// vkGetQueryPoolResults: its result, then each query's value and availability.
    pub fn collect(
        &mut self,
        mut collected: u64,
        taken: u64,
        mask: u64,
        live: impl Fn(u32) -> bool,
        mut read: impl FnMut(u32) -> (vk::Result, [[u64; 2]; 2]),
    ) -> (u64, u64) {
        let mut busy = 0u64;
        while collected < taken {
            let pair = (collected % u64::from(PAIRS)) as u32;
            if live(pair) {
                let (result, results) = read(pair);
                if (result != vk::Result::SUCCESS && result != vk::Result::NOT_READY)
                    || results[0][1] == 0
                    || results[1][1] == 0
                {
                    break;
                }
                let (start, end) = (results[0][0] & mask, results[1][0] & mask);
                let seen = &mut self.seen[pair as usize];
                if start == seen[0] && end == seen[1] {
                    break;
                }
                *seen = [start, end];
                // Submissions overlap -- the start is stamped as the command processor
                // reaches it, which can be while the last one's draws are still on -- so
                // only the part past the last end is new.
                let from = if self.any_end && before(start, self.last_end, mask) { self.last_end } else { start };
                if before(from, end, mask) {
                    busy = busy.wrapping_add(end.wrapping_sub(from) & mask);
                }
                if !self.any_end || before(self.last_end, end, mask) {
                    self.last_end = end;
                }
                self.any_end = true;
            }
            collected += 1;
        }
        (collected, busy)
    }
}

impl Default for Collector {
    fn default() -> Self {
        Collector::new()
    }
}

/// The report's nanoseconds for `busy` ticks.
pub fn busy_nanoseconds(busy: u64, nanoseconds_per_tick: f64) -> u64 {
    (busy as f64 * nanoseconds_per_tick) as u64
}

/// What start() made, read by submit() and collect() while `ON` is set.
struct Fixed {
    device: ash::Device,
    pool: vk::QueryPool,
    commands: vk::CommandPool,
    // Recorded once: the first resets its pair and stamps the start, the second stamps
    // the end.
    begins: [vk::CommandBuffer; PAIRS as usize],
    ends: [vk::CommandBuffer; PAIRS as usize],
    nanoseconds_per_tick: f64,
    mask: u64,
}

static ON: AtomicBool = AtomicBool::new(false);
static FIXED: RwLock<Option<Fixed>> = RwLock::new(None);
static RING: Mutex<Ring> = Mutex::new(Ring { taken: 0 });
static COLLECTED: AtomicU64 = AtomicU64::new(0);
// False for a submission the queue refused.
static LIVE: [AtomicBool; PAIRS as usize] = [const { AtomicBool::new(false) }; PAIRS as usize];
static COLLECTOR: Mutex<Collector> = Mutex::new(Collector::new());
// Under the queue's lock: the timed batch, built again for each submission.
static BATCH: Mutex<Vec<vk::CommandBuffer>> = Mutex::new(Vec::new());

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn fixed() -> std::sync::RwLockReadGuard<'static, Option<Fixed>> {
    FIXED.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// On the device and queue family every submission goes to, when the report is on and
/// the family keeps time.
///
/// # Safety
/// The handles are the renderer's live physical device, device and queue, on the
/// instance vk::pipeline::Instance() names.
pub unsafe fn start(physical: vk::PhysicalDevice, raw_device: vk::Device, queue: vk::Queue, family: u32) {
    if !report::on() {
        return;
    }
    // SAFETY: the renderer's instance, live for the run.
    let instance = unsafe { super::instance_table(bridge::vulkan::instance()) };
    // SAFETY: the instance's physical device.
    let (families, limits) = unsafe {
        (
            instance.get_physical_device_queue_family_properties(physical),
            instance.get_physical_device_properties(physical),
        )
    };
    let bits: Vec<u32> = families.iter().map(|family| family.timestamp_valid_bits).collect();
    let Some((mask, nanoseconds_per_tick)) = timing(&bits, family, limits.limits.timestamp_period) else {
        log_info!("report: the graphics chip does not keep time on its queue; its busy time is not known");
        return;
    };
    // SAFETY: the renderer's device, made on that instance.
    let device = unsafe { ash::Device::load(instance.fp_v1_0(), raw_device) };
    let mut made = Fixed {
        device,
        pool: vk::QueryPool::null(),
        commands: vk::CommandPool::null(),
        begins: [vk::CommandBuffer::null(); PAIRS as usize],
        ends: [vk::CommandBuffer::null(); PAIRS as usize],
        nanoseconds_per_tick,
        mask,
    };
    // SAFETY: Vulkan calls on the renderer's device and queue; what fails is destroyed by
    // give_up, and nothing else holds what it destroys.
    unsafe {
        let device = &made.device;
        let queries = vk::QueryPoolCreateInfo {
            query_type: vk::QueryType::TIMESTAMP,
            query_count: 2 * PAIRS,
            ..Default::default()
        };
        let pool = vk::CommandPoolCreateInfo { queue_family_index: family, ..Default::default() };
        match device.create_query_pool(&queries, None) {
            Ok(created) => made.pool = created,
            Err(_) => return give_up(made),
        }
        match device.create_command_pool(&pool, None) {
            Ok(created) => made.commands = created,
            Err(_) => return give_up(made),
        }
        let mut allocate = vk::CommandBufferAllocateInfo {
            command_pool: made.commands,
            level: vk::CommandBufferLevel::PRIMARY,
            command_buffer_count: PAIRS,
            ..Default::default()
        };
        let mut reset = [vk::CommandBuffer::null()];
        let allocate_fn = device.fp_v1_0().allocate_command_buffers;
        if allocate_fn(device.handle(), &allocate, made.begins.as_mut_ptr()) != vk::Result::SUCCESS
            || allocate_fn(device.handle(), &allocate, made.ends.as_mut_ptr()) != vk::Result::SUCCESS
            || {
                allocate.command_buffer_count = 1;
                allocate_fn(device.handle(), &allocate, reset.as_mut_ptr()) != vk::Result::SUCCESS
            }
        {
            return give_up(made);
        }
        // Resubmitted every PAIRS submissions, by when the last round is done.
        let begin =
            vk::CommandBufferBeginInfo { flags: vk::CommandBufferUsageFlags::SIMULTANEOUS_USE, ..Default::default() };
        for pair in 0..PAIRS {
            let (first, last) = (made.begins[pair as usize], made.ends[pair as usize]);
            let _ = device.begin_command_buffer(first, &begin);
            device.cmd_reset_query_pool(first, made.pool, 2 * pair, 2);
            device.cmd_write_timestamp(first, vk::PipelineStageFlags::TOP_OF_PIPE, made.pool, 2 * pair);
            let _ = device.end_command_buffer(first);
            let _ = device.begin_command_buffer(last, &begin);
            device.cmd_write_timestamp(last, vk::PipelineStageFlags::BOTTOM_OF_PIPE, made.pool, 2 * pair + 1);
            let _ = device.end_command_buffer(last);
        }
        // Every query starts reset, so a pair not yet written reads as unavailable.
        let once =
            vk::CommandBufferBeginInfo { flags: vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT, ..Default::default() };
        let _ = device.begin_command_buffer(reset[0], &once);
        device.cmd_reset_query_pool(reset[0], made.pool, 0, 2 * PAIRS);
        let _ = device.end_command_buffer(reset[0]);
        let submit =
            vk::SubmitInfo { command_buffer_count: 1, p_command_buffers: reset.as_ptr(), ..Default::default() };
        let done = {
            let _queue = QueueLock::hold();
            (device.fp_v1_0().queue_submit)(queue, 1, &submit, vk::Fence::null()) == vk::Result::SUCCESS
                && (device.fp_v1_0().queue_wait_idle)(queue) == vk::Result::SUCCESS
        };
        if !done {
            return give_up(made);
        }
        device.free_command_buffers(made.commands, &reset);
    }
    *FIXED.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(made);
    ON.store(true, Ordering::Release);
}

/// What start() made before something failed, destroyed: Stop() without a Collect.
///
/// # Safety
/// Nothing uses the pools.
unsafe fn give_up(made: Fixed) {
    // SAFETY: the pools start() made and nothing submitted.
    unsafe {
        if made.commands != vk::CommandPool::null() {
            made.device.destroy_command_pool(made.commands, None);
        }
        if made.pool != vk::QueryPool::null() {
            made.device.destroy_query_pool(made.pool, None);
        }
    }
}

/// With the device idle: what is left is counted first.
pub fn stop() {
    if ON.load(Ordering::Acquire) {
        collect();
    }
    ON.store(false, Ordering::Release);
    let made = FIXED.write().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
    if let Some(made) = made {
        // SAFETY: the device is idle, and with ON clear nothing reads the pools again.
        unsafe { give_up(made) };
    }
}

/// vkQueueSubmit of one batch, between the two timestamps, through `queue_submit` (the
/// caller's: the loader's for the C++ renderer, the device's for the presenter). Under
/// vk::pipeline::QueueMutex like any submission: the order the pairs are taken in is
/// the order the queue runs them.
///
/// # Safety
/// `queue_submit` is vkQueueSubmit of the device `queue` belongs to; `submit` and the
/// arrays it points at are valid for the call; the queue's lock is held.
pub unsafe fn submit(
    queue_submit: vk::PFN_vkQueueSubmit,
    queue: vk::Queue,
    submit: &vk::SubmitInfo<'_>,
    fence: vk::Fence,
) -> vk::Result {
    // SAFETY (each call): the caller's queue, batch and fence, under the queue's lock.
    if !ON.load(Ordering::Acquire) {
        return unsafe { queue_submit(queue, 1, submit, fence) };
    }
    let guard = fixed();
    let Some(made) = guard.as_ref() else { return unsafe { queue_submit(queue, 1, submit, fence) } };
    let pair = {
        let mut ring = lock(&RING);
        match ring.take(COLLECTED.load(Ordering::Acquire)) {
            Some(pair) => {
                LIVE[pair as usize].store(true, Ordering::Relaxed);
                pair
            }
            None => return unsafe { queue_submit(queue, 1, submit, fence) },
        }
    };
    let mut batch = lock(&BATCH);
    batch.clear();
    batch.push(made.begins[pair as usize]);
    if submit.command_buffer_count != 0 {
        // SAFETY: the caller's array of command_buffer_count handles.
        batch.extend_from_slice(unsafe {
            std::slice::from_raw_parts(submit.p_command_buffers, submit.command_buffer_count as usize)
        });
    }
    batch.push(made.ends[pair as usize]);
    let mut timed = *submit;
    timed.command_buffer_count = batch.len() as u32;
    timed.p_command_buffers = batch.as_ptr();
    let result = unsafe { queue_submit(queue, 1, &timed, fence) };
    if result != vk::Result::SUCCESS {
        LIVE[pair as usize].store(false, Ordering::Release);
    }
    result
}

/// Adds to the report the time the GPU took over the submissions it has finished since
/// the last call. One thread at a time.
pub fn collect() {
    if !ON.load(Ordering::Acquire) {
        return;
    }
    let guard = fixed();
    let Some(made) = guard.as_ref() else { return };
    let taken = lock(&RING).taken;
    let mut collector = lock(&COLLECTOR);
    let (collected, busy) = collector.collect(
        COLLECTED.load(Ordering::Relaxed),
        taken,
        made.mask,
        |pair| LIVE[pair as usize].load(Ordering::Acquire),
        |pair| {
            // Each query's value and then whether it is there yet.
            let mut results = [[0u64; 2]; 2];
            // SAFETY: two queries of the pool start() made, into 32 bytes, 16 apart.
            let read = unsafe {
                (made.device.fp_v1_0().get_query_pool_results)(
                    made.device.handle(),
                    made.pool,
                    2 * pair,
                    2,
                    std::mem::size_of_val(&results),
                    results.as_mut_ptr().cast(),
                    std::mem::size_of_val(&results[0]) as u64,
                    vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WITH_AVAILABILITY,
                )
            };
            (read, results)
        },
    );
    COLLECTED.store(collected, Ordering::Release);
    if busy != 0 {
        report::spent(report::GPU_BUSY, busy_nanoseconds(busy, made.nanoseconds_per_tick));
    }
}
