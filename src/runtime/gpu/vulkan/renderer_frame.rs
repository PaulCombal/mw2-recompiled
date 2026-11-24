// Command buffers and their submission: the frame slots, the end of a frame, and the
// title's occlusion queries, which live and die with the render passes.
//
// `Core` is the renderer's state that is neither its targets, its resolves, its pipelines
// nor its texture slots, so that the four can be borrowed
// beside it: `FrameView` (a Core and the device) is what the targets and the resolves are
// handed as their `Frame` (renderer_targets.rs).
//
// The invariants this keeps: a slot's fence counts only once the recorder
// has passed `queued_at`, the fence before it having said what it said about the slot's
// last submission; the submission's closure resets the fence under the queue's lock, and
// if the command buffer will not end or the submission fails it still signals the fence
// with an empty submission, or whoever waits for the slot waits for ever; the command
// pool's reset is itself a recorded call; the recorder is drained before a slot's buffers
// grow; a submission is the ended segments (each one's uploads, then its draws), then the
// last segment's uploads, then its command buffer; the trace records the submission at
// this call site, before gpu_time submits it; `bound` is forgotten at every command
// buffer's start, where sets 1 and 2 are bound once; a waited Submit leaves no slot in
// flight; the completed serial is sampled once per BeginFrame, and the fast links an
// optimised one replaced are freed there, after it, once their submission is done and
// the recorder past their calls (Retiring, the plan's S12c).
//
// Unsafe by nature: the command pools, buffers, fences and query pool are the renderer's
// device's, named by calls queued on the recorder and waited for through their fences.
#![deny(unsafe_op_in_unsafe_fn)]

use super::device::Vk;
use super::recorder;
use super::renderer_arena::Arena;
use super::renderer_diagnostics::Diagnostics;
use super::renderer_passtime::PassTimes;
use super::renderer_pipelines::Retiring;
use super::renderer_state::{FRAME_SLOTS, ResolveRecords};
use super::renderer_targets::{Frame, Present, Setup, Target, Targets};
use super::tracked::TrackedImage;
use crate::log::{log_error, log_warn};
use crate::runtime::gpu::hash::MulBuild;
use crate::runtime::gpu::shader::bindings::{RECORD_WORDS, TEXTURE_SET};
use crate::runtime::{clock, report};
use ash::vk;
use std::collections::HashMap;
use std::ffi::CStr;
use std::fmt;

use super::recorder::{done, drain, flush, position, wait_for};
use super::texture_cache::TextureCache;
#[cfg(feature = "diagnostics")]
use super::trace;
use super::{capture, gpu_time, presenter};

/// Four rather than two: the window shows every frame, a blank each, and each image is
/// kept intact until the window has copied it out; four let the renderer run up to three
/// frames ahead of the screen before it waits.
pub const PRESENT_IMAGES: u32 = 4;
/// Command buffers made for a slot at the start, and again each time a submission needs
/// more segments than it has: a frame has about twenty.
pub const SEGMENT_BUFFERS: u32 = 32;
const _: () = assert!(FRAME_SLOTS <= super::texture_cache::UPLOAD_SLOTS, "a slot's uploads need a slot too");
/// The title's own occlusion queries: one of the title's brackets takes as many of these as
/// it covers render passes, and their counts are summed. The pool is a ring, so brackets
/// whose counts are not in yet keep theirs.
pub const MAX_GUEST_QUERIES: u32 = 64;
pub const GUEST_QUERY_RING: u32 = 4 * MAX_GUEST_QUERIES;
/// Play starts with the first run of this many world frames in a row.
pub const PLAY_STREAK: u64 = 30;

// stutters.rs's costs and report.rs's times these time.
const STUTTER_GPU_WAITS: usize = 4;
const STUTTER_QUERY_READS: usize = 5;
const REPORT_GPU_WAIT: usize = 2;

/// A slot: the command buffers it records into, the fence that says when the GPU has
/// finished with it, which submission it holds, and the recorder's queue just past that
/// submission (State::Slot; its share of the arena is the arena's).
#[derive(Default)]
pub struct Slot {
    pub commands: vk::CommandPool,
    /// One a segment of the submission, made as submissions need more; the first is the
    /// frame's start.
    pub buffers: Vec<vk::CommandBuffer>,
    pub fence: vk::Fence,
    pub in_flight: bool,
    pub serial: u64,
    /// Until the recorder gets here the fence has not been handed to the queue, and still
    /// says what it said about the submission before.
    pub queued_at: u64,
}

/// What the command buffer was last told, so a draw that asks for the same thing does not
/// say it again; reset per command buffer (State::Bound).
#[derive(Clone, Copy)]
pub struct Bound {
    pub pipeline: vk::Pipeline,
    /// The last draw's record, and where it is: the next draw names the same one when
    /// nothing in it differs.
    pub record: [u32; RECORD_WORDS as usize],
    pub record_words: u32,
    pub record_at: u32,
    pub record_valid: bool,
    pub viewport: vk::Viewport,
    pub scissor: vk::Rect2D,
    pub viewport_valid: bool,
    pub scissor_valid: bool,
    pub depth_bias: [f32; 2], // constant, slope
    pub depth_bias_valid: bool,
    pub stencil: [u32; 2], // RB_STENCILREFMASK for front and back faces
    pub stencil_valid: bool,
    pub mode_cntl: u32,
    pub faces_valid: bool,
    pub depth_control: u32,
    pub depth_tests_valid: bool,
    pub index_buffer: vk::Buffer,
    pub index_offset: u32,
    pub index_type: i32,
}

impl Default for Bound {
    fn default() -> Self {
        Bound {
            pipeline: vk::Pipeline::null(),
            record: [0; RECORD_WORDS as usize],
            record_words: 0,
            record_at: 0,
            record_valid: false,
            viewport: vk::Viewport::default(),
            scissor: vk::Rect2D::default(),
            viewport_valid: false,
            scissor_valid: false,
            depth_bias: [0.0; 2],
            depth_bias_valid: false,
            stencil: [0; 2],
            stencil_valid: false,
            mode_cntl: 0,
            faces_valid: false,
            depth_control: 0,
            depth_tests_valid: false,
            index_buffer: vk::Buffer::null(),
            index_offset: 0,
            index_type: -1,
        }
    }
}

/// A bracket submitted whose counts the GPU has not produced yet (State::PendingQuery).
#[derive(Clone, Copy, Default, Debug)]
pub struct PendingQuery {
    pub first: u32,
    pub count: u32,
    pub tag: u32,
    pub slot: u32,
    pub serial: u64,
    /// Counted already, waiting to be handed over.
    pub read: bool,
    pub samples: u64,
}

/// The title's own occlusion queries (State's guest query fields).
pub struct Occlusion {
    pub pool: vk::QueryPool,
    pub first: u32, // the bracket in progress's first query
    pub count: u32, // and how many of them it has used
    pub next: u32,  // where the next bracket starts in the ring
    /// The scale of the pass each query in the ring counted in.
    pub scale: [u8; GUEST_QUERY_RING as usize],
    pub pending: Vec<PendingQuery>,
    pub counting: bool, // between the title's two events
    pub open: bool,     // one is open in the current render pass
}

impl Default for Occlusion {
    fn default() -> Self {
        Occlusion {
            pool: vk::QueryPool::null(),
            first: 0,
            count: 0,
            next: 0,
            scale: [0; GUEST_QUERY_RING as usize],
            pending: Vec::new(),
            counting: false,
            open: false,
        }
    }
}

/// Where a frame's wall clock goes, in nanoseconds (MW2_TIME_RENDER=1).
#[derive(Clone, Copy, Default, Debug)]
pub struct Times {
    pub draw: u64,
    pub submit: u64,
    pub fence: u64,
    pub texture: u64,
    pub constants: u64,
    pub pipeline: u64,
    pub record: u64,
    pub setup: u64,
    pub index: u64,
    pub state: u64,
}

/// The renderer's state but its targets, resolves, pipelines and texture slots.
pub struct Core {
    pub device: vk::Device,
    /// The device of a renderer that did not start (Renderer::initialise), kept for its
    /// shutdown to destroy what was made on it; `device` is null then.
    pub refused: vk::Device,
    pub physical: vk::PhysicalDevice,
    pub queue: vk::Queue,
    pub queue_family: u32,
    /// pipeline::precise_occlusion_queries (shaders/): a query may be asked for an exact count.
    pub precise_queries: bool,
    pub memory: vk::PhysicalDeviceMemoryProperties,
    pub uniform_alignment: u32,
    /// MW2_SCALE and MW2_SHADOW_SCALE as the device draws them.
    pub scale: u32,
    pub shadow_scale: u32,

    pub slots: [Slot; FRAME_SLOTS as usize],
    pub slot: u32, // BeginFrame moves on before it records
    /// The current segment's command buffer, which commands go into through the recorder.
    pub command: vk::CommandBuffer,
    /// The submission so far, in the order the queue runs it: each ended segment's uploads
    /// (when it had any) and then its draws.
    pub ended: Vec<vk::CommandBuffer>,
    pub segment: u32,
    pub segment_draws: bool,
    pub segments: u64,
    pub completions: u64,
    pub setup_commands: vk::CommandPool,
    pub setup: vk::CommandBuffer,
    pub setup_fence: vk::Fence,

    pub arena_buffer: vk::Buffer,
    pub arena_memory: vk::DeviceMemory,
    pub arena_mapped: *mut u8,
    pub arena_bytes: u32,
    pub shadow_base: u32,
    pub arena: Arena,
    pub arena_peak: u32,

    pub completed_serial: u64,
    pub stream_write_segments: u64,
    pub submission_serial: u64,
    pub occlusion: Occlusion,
    /// The fast links optimised ones replaced, until their frames are done.
    pub retiring: Retiring,

    pub descriptors: vk::DescriptorPool,
    pub memory_set: vk::DescriptorSet,
    pub pipeline_layout: vk::PipelineLayout,

    pub bound: Bound,
    pub recording: bool,
    pub current_colour: vk::ImageView,
    pub current_depth: vk::ImageView,
    pub current_width: u32,
    pub current_height: u32,
    pub current_samples: u32,
    pub current_scale: u32,

    pub present: [vk::Image; PRESENT_IMAGES as usize],
    pub present_memory: [vk::DeviceMemory; PRESENT_IMAGES as usize],
    pub present_index: u32,
    pub present_serial: [u64; PRESENT_IMAGES as usize],
    /// What each present image was last used for (tracked.rs).
    pub present_track: [TrackedImage; PRESENT_IMAGES as usize],
    pub present_width: u32,
    pub present_height: u32,

    pub frames: u64,
    pub draws: u64,
    pub draws_recorded: u64,
    pub world_frames: u64,
    pub world_streak: u64,
    /// The steady clock's nanoseconds (std::chrono::steady_clock's).
    pub first_world_at: Option<u64>,
    pub last_world_at: Option<u64>,
    pub streak_from: Option<u64>,

    pub submits: u64,
    pub times: Times,
    pub timing: bool,
    pub verts_traced: u64,
    pub verts_matched: u64,
    /// By reason, the literal's address its key (State::skipped).
    pub skipped: HashMap<usize, (&'static CStr, u64), MulBuild>,

    pub textures: TextureCache,
    pub pass: PassTimes,
    pub diag: Diagnostics,
}

// SAFETY: the renderer is the ring consumer's, moved into its thread whole (§3.6); the
// arena mapping it points at lives until shutdown unmaps it.
unsafe impl Send for Core {}

impl Core {
    pub fn new() -> Core {
        Core {
            device: vk::Device::null(),
            refused: vk::Device::null(),
            physical: vk::PhysicalDevice::null(),
            queue: vk::Queue::null(),
            queue_family: 0,
            precise_queries: false,
            memory: vk::PhysicalDeviceMemoryProperties::default(),
            uniform_alignment: 256,
            scale: 1,
            shadow_scale: 1,
            slots: Default::default(),
            slot: FRAME_SLOTS - 1,
            command: vk::CommandBuffer::null(),
            ended: Vec::new(),
            segment: 0,
            segment_draws: false,
            segments: 0,
            completions: 0,
            setup_commands: vk::CommandPool::null(),
            setup: vk::CommandBuffer::null(),
            setup_fence: vk::Fence::null(),
            arena_buffer: vk::Buffer::null(),
            arena_memory: vk::DeviceMemory::null(),
            arena_mapped: std::ptr::null_mut(),
            arena_bytes: 0,
            shadow_base: 0,
            arena: Arena::empty(),
            arena_peak: 0,
            completed_serial: 0,
            stream_write_segments: 0,
            submission_serial: 0,
            occlusion: Occlusion::default(),
            retiring: Retiring::default(),
            descriptors: vk::DescriptorPool::null(),
            memory_set: vk::DescriptorSet::null(),
            pipeline_layout: vk::PipelineLayout::null(),
            bound: Bound::default(),
            recording: false,
            current_colour: vk::ImageView::null(),
            current_depth: vk::ImageView::null(),
            current_width: 0,
            current_height: 0,
            current_samples: 1,
            current_scale: 1,
            present: [vk::Image::null(); PRESENT_IMAGES as usize],
            present_memory: [vk::DeviceMemory::null(); PRESENT_IMAGES as usize],
            present_index: 0,
            present_serial: [0; PRESENT_IMAGES as usize],
            present_track: [TrackedImage::NEW; PRESENT_IMAGES as usize],
            present_width: 0,
            present_height: 0,
            frames: 0,
            draws: 0,
            draws_recorded: 0,
            world_frames: 0,
            world_streak: 0,
            first_world_at: None,
            last_world_at: None,
            streak_from: None,
            submits: 0,
            times: Times::default(),
            timing: false,
            verts_traced: 0,
            verts_matched: 0,
            skipped: HashMap::default(),
            textures: TextureCache::new(),
            pass: PassTimes::new(),
            diag: Diagnostics::new(),
        }
    }

    /// Skip(why): counted by reason for the report, and the flash hunt's draw record told.
    #[inline]
    pub fn skip(&mut self, why: &'static CStr) {
        self.skipped.entry(why.as_ptr() as usize).or_insert((why, 0)).1 += 1;
        self.diag.skipped(why);
    }

    /// The time from `began` added to `into`, when MW2_TIME_RENDER.
    #[inline]
    pub fn began(&self) -> u64 {
        if self.timing { clock::nanoseconds() } else { 0 }
    }

    // ---- the frame ------------------------------------------------------------------------

    /// Destroying an image the GPU may still use: the open command buffer finished and
    /// waited for, the recorder drained, the device idle, and a fresh buffer begun if one
    /// was being recorded. Waiting for the device alone covers only work submitted: the
    /// buffer being recorded -- a render pass open on the very view about to go, or a copy
    /// out of it -- would point at freed memory, and the GPU write through addresses nothing
    /// backs, page faults that can hang the driver and the machine.
    pub fn retire_before_destroy(&mut self, vk: &Vk) {
        let was_recording = self.recording;
        if was_recording {
            self.submit(vk, true);
        }
        drain();
        // The queue is in order, so nothing is outstanding afterwards. vkDeviceWaitIdle
        // waits on every queue of the device, which must be externally synchronised as a
        // submission is: under the queue's lock, beside the window thread's presents.
        {
            let _lock = super::QueueLock::hold();
            // SAFETY: the renderer's device, its queue held.
            let _ = unsafe { vk.device_wait_idle() };
        }
        for slot in &mut self.slots {
            slot.in_flight = false;
        }
        if was_recording {
            self.begin_frame(vk);
        }
    }

    pub fn begin_frame(&mut self, vk: &Vk) -> bool {
        if self.recording {
            return true;
        }
        // Round to the next slot and wait only if the GPU has not finished with it.
        self.slot = (self.slot + 1) % FRAME_SLOTS;
        let slot = self.slot as usize;
        if !self.wait_for_slot(vk, slot, c"a frame slot's fence") {
            return false;
        }
        self.note_completed(vk);
        // SAFETY: the completed serial just sampled.
        unsafe { self.retiring.free(vk, self.completed_serial) };
        gpu_time::collect();
        self.textures.begin_uploads(vk, self.slot, self.submission_serial + 1, self.completed_serial);
        self.ended.clear();
        self.segment = 0;
        self.command = self.slots[slot].buffers[0];
        recorder::into(self.command);
        let pool = self.slots[slot].commands;
        recorder::command(move |c| {
            // SAFETY: the slot's pool, whose last submission the wait above saw done.
            let _ = unsafe { c.vk.reset_command_pool(pool, vk::CommandPoolResetFlags::empty()) };
        });
        self.begin_segment_buffer();
        self.pass.begin(vk, self.slot, self.frames, self.draws);
        self.arena.begin_frame(self.slot);
        self.recording = true;
        true
    }

    /// Begins the current command buffer. Sets 1 and 2 -- every texture, and the arena --
    /// are the same for every draw, so they are bound once a command buffer.
    pub fn begin_segment_buffer(&mut self) {
        let layout = self.pipeline_layout;
        let sets = [self.textures.set(), self.memory_set];
        recorder::command(move |c| {
            let begin = vk::CommandBufferBeginInfo {
                flags: vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT,
                ..Default::default()
            };
            // SAFETY: the recorder's command buffer, reset with its pool.
            unsafe {
                if c.vk.begin_command_buffer(c.into, &begin) != vk::Result::SUCCESS {
                    log_warn!("renderer: a frame's command buffer would not begin");
                }
                c.vk.cmd_bind_descriptor_sets(c.into, vk::PipelineBindPoint::GRAPHICS, layout, TEXTURE_SET, &sets, &[]);
            }
        });
        // A fresh command buffer remembers nothing.
        self.bound = Bound::default();
        self.current_colour = vk::ImageView::null();
        self.current_depth = vk::ImageView::null();
        self.segment_draws = false;
    }

    /// The command processor is about to report the draws so far done: the segment's
    /// textures are read for the last time, and its draws closed off in a command buffer of
    /// their own. The submission goes on in the next command buffer, and is still one.
    pub fn close_segment(&mut self, vk: &Vk) {
        self.end_pass();
        self.pass.between(self.draws);
        let uploads = self.textures.end_segment(vk);
        if uploads != vk::CommandBuffer::null() {
            self.ended.push(uploads);
        }
        self.ended.push(self.command);
        recorder::command(|c| {
            // SAFETY: the recorder's command buffer, begun at the segment's start.
            if unsafe { c.vk.end_command_buffer(c.into) } != vk::Result::SUCCESS {
                log_warn!("renderer: a segment's command buffer would not end");
            }
        });
        let slot = self.slot as usize;
        self.segment += 1;
        if self.segment as usize >= self.slots[slot].buffers.len() {
            // The recorder may be making calls into the pool's other buffers: it has to be
            // done with them before the pool is touched here.
            drain();
            let allocate = vk::CommandBufferAllocateInfo {
                command_pool: self.slots[slot].commands,
                level: vk::CommandBufferLevel::PRIMARY,
                command_buffer_count: SEGMENT_BUFFERS,
                ..Default::default()
            };
            let had = self.slots[slot].buffers.len();
            self.slots[slot].buffers.resize(had + SEGMENT_BUFFERS as usize, vk::CommandBuffer::null());
            // SAFETY: the slot's pool, which nothing is recording into (drained).
            if unsafe { vk.allocate_command_buffers(&allocate, &mut self.slots[slot].buffers[had..]) }
                != vk::Result::SUCCESS
            {
                log_error!("renderer: no command buffers for another segment");
            }
        }
        self.command = self.slots[slot].buffers[self.segment as usize];
        recorder::into(self.command);
        self.begin_segment_buffer();
        self.pass.mark_named(format_args!("texture uploads at a segment's end"), self.draws);
        self.segments += 1;
    }

    /// A Vulkan occlusion query has to begin and end inside one render pass instance, so a
    /// bracket of the title's becomes one query per pass it covers.
    pub fn open_guest_query(&mut self) {
        let precise = self.precise_queries;
        let o = &mut self.occlusion;
        if !o.counting || o.open {
            return;
        }
        if o.pool == vk::QueryPool::null() || o.count >= MAX_GUEST_QUERIES {
            return;
        }
        let pool = o.pool;
        let query = o.first + o.count;
        let flags = if precise { vk::QueryControlFlags::PRECISE } else { vk::QueryControlFlags::empty() };
        o.scale[(query % GUEST_QUERY_RING) as usize] = self.current_scale as u8;
        recorder::command(move |c| {
            // SAFETY: the renderer's query pool, reset at the bracket's begin.
            unsafe { c.vk.cmd_begin_query(c.into, pool, query, flags) }
        });
        o.open = true;
    }

    pub fn close_guest_query(&mut self) {
        let o = &mut self.occlusion;
        if !o.open {
            return;
        }
        let pool = o.pool;
        let query = o.first + o.count;
        o.count += 1;
        recorder::command(move |c| {
            // SAFETY: the query this pass began.
            unsafe { c.vk.cmd_end_query(c.into, pool, query) }
        });
        o.open = false;
    }

    pub fn end_pass(&mut self) {
        self.close_guest_query();
        if self.current_colour != vk::ImageView::null() {
            // No barrier out: whatever uses the targets next brings them on from the
            // attachments' use (tracked.rs), as a render pass's dependency out was. The
            // attachments' drain then falls in the next pass window, not this one (its marks
            // are BOTTOM_OF_PIPE timestamps): at a segment's end the uploads' window.
            recorder::command(|c| {
                // SAFETY: the recorder's command buffer, inside the rendering instance
                // begin_rendering opened.
                unsafe { c.vk.cmd_end_rendering(c.into) };
            });
        }
        self.pass.close(self.draws);
        self.current_colour = vk::ImageView::null();
        self.current_depth = vk::ImageView::null();
    }

    /// `wait` when the CPU is about to read what the submission produced, or free what it
    /// uses; otherwise queued, with up to FRAME_SLOTS submissions on the GPU at once. The
    /// submission itself is the recorder thread's, after the commands before it; the
    /// serial is the consumer's, counted here.
    pub fn submit(&mut self, vk: &Vk, wait: bool) -> bool {
        let began = self.began();
        let done = self.submit_timed(vk, wait);
        if self.timing {
            self.times.submit = self.times.submit.wrapping_add(clock::nanoseconds().wrapping_sub(began));
        }
        done
    }

    fn submit_timed(&mut self, vk: &Vk, wait: bool) -> bool {
        if !self.recording {
            return true;
        }
        self.end_pass();
        self.pass.between(self.draws);
        if self.arena.used() > self.arena_peak {
            self.arena_peak = self.arena.used();
        }
        self.recording = false;

        let slot = self.slot as usize;
        // The last segment's textures are read for the last time, and its uploads go just
        // ahead of its draws, after the segments before.
        let mut buffers = std::mem::take(&mut self.ended);
        let uploads = self.textures.end_submission(vk);
        if uploads != vk::CommandBuffer::null() {
            buffers.push(uploads);
        }
        buffers.push(self.command);
        let queue = self.queue;
        let fence = self.slots[slot].fence;
        // What the submission's draws read out of the arena (slow: write-combined memory).
        #[cfg(feature = "diagnostics")]
        if trace::wanted() {
            // SAFETY: the slot's share, mapped, `used` bytes from its base.
            let fnv = unsafe { trace::fnv_write_combined(self.arena.share_start(), self.arena.used() as usize) };
            trace::note(&format!("ArenaHash slot={} used={} fnv={:016x}", self.slot, self.arena.used(), fnv));
        }
        recorder::command(move |c| {
            // SAFETY: the recorder's command buffer, begun at its segment's start; the
            // submission's buffers, fence and queue are the renderer's, the queue's lock
            // held across the reset and the submissions.
            unsafe {
                let ended = c.vk.end_command_buffer(c.into) == vk::Result::SUCCESS;
                if !ended {
                    log_warn!("renderer: a frame's command buffer would not end");
                }
                let submit = vk::SubmitInfo {
                    command_buffer_count: buffers.len() as u32,
                    p_command_buffers: buffers.as_ptr(),
                    ..Default::default()
                };
                let _lock = super::QueueLock::hold();
                let _ = c.vk.reset_fences(&[fence]);
                // Whatever happens the fence is signalled, or whoever waits for this slot
                // waits for ever.
                let queue_submit = c.vk.device.fp_v1_0().queue_submit;
                #[cfg(feature = "diagnostics")]
                {
                    // gpu_time submits it, untraced; MW2_FAKE_DEVICE_LOST fails it instead.
                    let mut submitted = vk::Result::ERROR_DEVICE_LOST;
                    if ended && trace::device_lost_faked() {
                        let lost = &trace::counts().lost_submissions;
                        lost.store(
                            lost.load(std::sync::atomic::Ordering::Relaxed) + 1,
                            std::sync::atomic::Ordering::Relaxed,
                        );
                    } else if ended {
                        trace::submission(queue, &submit, fence);
                        submitted = gpu_time::submit(queue_submit, queue, &submit, fence);
                    }
                    if !ended || super::failed(submitted, c"a frame's submission") {
                        let _ = c.vk.queue_submit(queue, &[], fence);
                    }
                }
                #[cfg(not(feature = "diagnostics"))]
                if !ended
                    || super::failed(gpu_time::submit(queue_submit, queue, &submit, fence), c"a frame's submission")
                {
                    let _ = c.vk.queue_submit(queue, &[], fence);
                }
            }
        });
        // The GPU gets it now, not when more calls have gathered behind it.
        flush();
        self.submission_serial += 1;
        // The draws recorded from here on go in the next one (Retiring::serial).
        self.retiring.serial = self.submission_serial + 1;
        let s = &mut self.slots[slot];
        s.in_flight = true;
        s.serial = self.submission_serial;
        s.queued_at = position();
        self.submits += 1;
        if !wait {
            return true;
        }
        if !self.wait_for_slot(vk, slot, c"a frame slot's fence") {
            return false;
        }
        // One queue, submitted in order: this one being done says all of them are.
        for other in &mut self.slots {
            other.in_flight = false;
        }
        true
    }

    /// Until the GPU has finished the submission slot `slot` holds, if it holds one.
    pub fn wait_for_slot(&mut self, vk: &Vk, slot: usize, what: &CStr) -> bool {
        if !self.slots[slot].in_flight {
            return true;
        }
        let began = self.began();
        let stutter = stutters_on().then(clock::nanoseconds);
        wait_for(self.slots[slot].queued_at);
        let spent = report::on().then(|| report::timed_begin(clock::nanoseconds(), REPORT_GPU_WAIT));
        let fence = self.slots[slot].fence;
        // SAFETY: the slot's fence, handed to the queue (the recorder is past queued_at).
        let result = unsafe { vk.wait_for_fences(&[fence], true, u64::MAX) };
        let failed = super::failed(result, what);
        if let Some(spent) = spent {
            report::timed_end(clock::nanoseconds(), REPORT_GPU_WAIT, spent);
        }
        stutter_add(STUTTER_GPU_WAITS, stutter);
        if self.timing {
            self.times.fence = self.times.fence.wrapping_add(clock::nanoseconds().wrapping_sub(began));
        }
        if failed {
            return false;
        }
        self.slots[slot].in_flight = false;
        true
    }

    /// Asked once a submission rather than whenever it is wanted: a fence's status is a
    /// system call. A value from a little earlier only makes the answer more cautious.
    pub fn note_completed(&mut self, vk: &Vk) {
        // MW2_DETERMINISTIC: what the GPU has finished is what was submitted.
        #[cfg(feature = "diagnostics")]
        if trace::deterministic() {
            for slot in 0..FRAME_SLOTS as usize {
                self.wait_for_slot(vk, slot, c"a frame slot's fence");
            }
            self.completed_serial = self.submission_serial;
            COMPLETED.store(self.completed_serial, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        let mut oldest_running = u64::MAX;
        for slot in &self.slots {
            // SAFETY: the slot's fence, handed to the queue once the recorder is past
            // queued_at, which `done` asks first.
            if slot.in_flight
                && slot.serial < oldest_running
                && (!done(slot.queued_at) || unsafe { vk.get_fence_status(slot.fence) } != vk::Result::SUCCESS)
            {
                oldest_running = slot.serial;
            }
        }
        self.completed_serial = if oldest_running == u64::MAX { self.submission_serial } else { oldest_running - 1 };
        COMPLETED.store(self.completed_serial, std::sync::atomic::Ordering::Relaxed);
    }

    /// The frame's image is complete in `present`: submit it, move the frame counters on,
    /// look at it if asked to, and hand it to the window.
    pub fn finish_frame(&mut self, vk: &Vk, present: vk::Image, targets: &mut Targets, records: &mut ResolveRecords) {
        // The frame is queued, not waited for: the window thread's blit goes into the same
        // queue behind it. Only a frame something reads back has to be finished first.
        let plan = self.diag.plan_read_backs(self.frames);
        if plan.any() {
            self.record_read_back(vk, present);
        }
        if !self.submit(vk, plan.any()) {
            self.skip(c"frame submission failed");
            return;
        }
        self.frames += 1;
        #[cfg(feature = "diagnostics")]
        trace::frame(self.frames);
        self.textures.new_frame(vk);

        // The title does not redraw the world every time it presents: the world pass is
        // the only one drawn multisampled in both executables, so a handful of
        // multisampled draws says it happened; 200 draws into any target is the fallback
        // for a device drawing at 1x. Better than half the frames of the training level
        // are the interface over whatever the last world pass left in EDRAM.
        const WORLD_DRAWS: u64 = 200;
        const MULTISAMPLED_DRAWS: u64 = 8;
        let mut world_drawn = false;
        targets.for_each_mut(|key, target| {
            if !target.depth
                && (target.colour_draws_this_frame >= WORLD_DRAWS
                    || (key.samples > 0 && target.colour_draws_this_frame >= MULTISAMPLED_DRAWS))
            {
                world_drawn = true;
            }
            target.colour_draws_this_frame = 0;
        });
        pacing_note(b'F', u32::from(world_drawn));
        stutters_finished(self.frames, world_drawn);
        if report::on() {
            report::frame(clock::nanoseconds(), &report::System, world_drawn);
        }
        super::renderer_libraries::note_frame(world_drawn);
        if world_drawn {
            let now = steady_now();
            let streak = self.world_streak;
            self.world_streak += 1;
            if streak == 0 {
                self.streak_from = Some(now);
            }
            if self.world_frames != 0 {
                self.last_world_at = Some(now);
                self.world_frames += 1;
            } else if self.world_streak == PLAY_STREAK {
                self.first_world_at = self.streak_from;
                self.last_world_at = Some(now);
                self.world_frames = PLAY_STREAK;
            }
        } else {
            self.world_streak = 0;
        }
        self.diag.end_frame_draws(&mut self.textures, self.frames, world_drawn);
        // A capture ends and begins between submissions as the GPU gets them, and the
        // window must not copy the frame out before it is submitted: both come after the
        // submission, on the recorder thread.
        let frame = self.frames;
        recorder::host(move || capture::frame_boundary(world_drawn, frame));
        self.present_serial[self.present_index as usize] = frame;
        self.present_index = (self.present_index + 1) % PRESENT_IMAGES;
        self.examine_frame(vk, world_drawn, plan, targets, records);
        let (width, height) = (self.present_width, self.present_height);
        recorder::host(move || {
            #[cfg(feature = "diagnostics")]
            trace::present(present, frame);
            presenter::show_image(present, width, height, frame);
        });
    }

    // ---- the title's occlusion queries ----------------------------------------------------

    /// Whether the GPU has finished the submission a bracket went out in: its slot has moved
    /// on to a later one (BeginFrame waited for it first), or the slot's fence says so.
    fn finished(&mut self, vk: &Vk, query: &PendingQuery) -> bool {
        // Still in the command buffer being recorded.
        if query.serial > self.submission_serial {
            return false;
        }
        let slot = &self.slots[query.slot as usize];
        if slot.serial != query.serial || !slot.in_flight {
            return true;
        }
        // MW2_DETERMINISTIC: a submitted bracket's count is waited for, not asked after.
        #[cfg(feature = "diagnostics")]
        if trace::deterministic() {
            self.wait_for_slot(vk, query.slot as usize, c"an occlusion query's submission");
            return true;
        }
        // Queued, and not yet handed to the GPU by the recorder thread.
        let slot = &self.slots[query.slot as usize];
        // SAFETY: the slot's fence, handed to the queue once the recorder is past
        // queued_at, which `done` asks first.
        done(slot.queued_at) && unsafe { vk.get_fence_status(slot.fence) } == vk::Result::SUCCESS
    }

    fn read(&mut self, vk: &Vk, at: usize) {
        let stutter = stutters_on().then(clock::nanoseconds);
        let o = &mut self.occlusion;
        let query = &mut o.pending[at];
        let mut samples = [0u8; MAX_GUEST_QUERIES as usize * 8];
        query.read = true;
        query.samples = 0;
        let count = query.count as usize;
        // SAFETY: the pool's queries of a submitted bracket; count <= MAX_GUEST_QUERIES.
        let result = unsafe {
            vk.get_query_pool_results(
                o.pool,
                query.first,
                query.count,
                &mut samples[..count.min(MAX_GUEST_QUERIES as usize) * 8],
                8,
                vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WAIT,
            )
        };
        if result == vk::Result::SUCCESS {
            // The title compares the count with numbers of its own pixels, and each query
            // counted the pixels of its own pass's scale.
            for i in 0..query.count {
                let at = i as usize * 8;
                let n = u64::from_ne_bytes([
                    samples[at],
                    samples[at + 1],
                    samples[at + 2],
                    samples[at + 3],
                    samples[at + 4],
                    samples[at + 5],
                    samples[at + 6],
                    samples[at + 7],
                ]);
                let scale = u64::from(o.scale[(query.first.wrapping_add(i) % GUEST_QUERY_RING) as usize]);
                query.samples = query.samples.wrapping_add(n / (scale * scale));
            }
        }
        stutter_add(STUTTER_QUERY_READS, stutter);
    }

    /// The title's own occlusion query begins: everything drawn until its end is counted,
    /// however many render passes and command buffers that takes.
    pub fn begin_occlusion_query(&mut self, vk: &Vk) {
        if self.occlusion.pool == vk::QueryPool::null() {
            return;
        }
        // Any query still open belongs to a bracket the title abandoned.
        self.end_pass();
        if !self.begin_frame(vk) {
            return;
        }
        if self.occlusion.next + MAX_GUEST_QUERIES > GUEST_QUERY_RING {
            self.occlusion.next = 0;
        }
        let first = self.occlusion.next;
        let last = first + MAX_GUEST_QUERIES;
        // A bracket still out on this stretch of the ring is counted now, before the reset
        // below reaches the GPU.
        for at in 0..self.occlusion.pending.len() {
            let query = self.occlusion.pending[at];
            if !query.read && query.first < last && first < query.first + query.count {
                // Not submitted yet: this recording holds it, and has to go first.
                if query.serial > self.submission_serial {
                    self.end_pass();
                    if !self.submit(vk, true) || !self.begin_frame(vk) {
                        return;
                    }
                }
                if self.slots[query.slot as usize].serial == query.serial {
                    self.wait_for_slot(vk, query.slot as usize, c"an occlusion query's submission");
                }
                self.read(vk, at);
            }
        }
        let pool = self.occlusion.pool;
        recorder::command(move |c| {
            // SAFETY: the renderer's query pool, outside any rendering instance.
            unsafe { c.vk.cmd_reset_query_pool(c.into, pool, first, MAX_GUEST_QUERIES) }
        });
        let o = &mut self.occlusion;
        o.first = first;
        o.count = 0;
        o.counting = true;
    }

    /// Not submitted here: the count goes out with the rest of the frame and is handed over
    /// once the GPU has finished it, and the title polls the record until then. True, with
    /// `samples` 0, when there is nothing to count.
    pub fn end_occlusion_query(&mut self, tag: u32, samples: &mut u64) -> bool {
        *samples = 0;
        if self.occlusion.pool == vk::QueryPool::null() || !self.occlusion.counting {
            return true;
        }
        self.end_pass();
        let o = &mut self.occlusion;
        o.counting = false;
        let counted = o.count;
        o.count = 0;
        if counted == 0 {
            return true;
        }
        o.next = o.first + counted;
        o.pending.push(PendingQuery {
            first: o.first,
            count: counted,
            tag,
            slot: self.slot,
            serial: self.submission_serial + 1,
            read: false,
            samples: 0,
        });
        false
    }

    /// In the order the title closed them: a later bracket is not handed over before an
    /// earlier one.
    pub fn collect_occlusion_queries(&mut self, vk: &Vk, mut deliver: impl FnMut(u32, u64)) {
        let mut done = 0;
        while done < self.occlusion.pending.len() {
            let query = self.occlusion.pending[done];
            if !query.read && !self.finished(vk, &query) {
                break;
            }
            if !query.read {
                self.read(vk, done);
            }
            let query = self.occlusion.pending[done];
            deliver(query.tag, query.samples);
            done += 1;
        }
        self.occlusion.pending.drain(..done);
    }

    /// The command stream is about to write [physical, physical + size), which a texture a
    /// draw of this segment samples: the segment goes now (vk::renderer::BeforeStreamWrite).
    pub fn before_stream_write(&mut self, vk: &Vk, physical: u32, size: u32) {
        if self.device == vk::Device::null() || !self.recording {
            return;
        }
        if !self.textures.bound_in_this_segment(physical, size) {
            return;
        }
        self.close_segment(vk);
        self.stream_write_segments += 1;
    }

    /// The command processor is about to tell the title that the work before this point has
    /// finished: every read of guest memory owed to the draws so far is made now
    /// (vk::renderer::BeforeCompletion).
    pub fn before_completion(&mut self, vk: &Vk) {
        self.completions += 1;
        if self.device == vk::Device::null() || !self.recording || !self.segment_draws {
            return;
        }
        self.close_segment(vk);
    }
}

impl Default for Core {
    fn default() -> Self {
        Core::new()
    }
}

// Whether the stutters' costs are timed: when MW2_STUTTERS is on.
#[inline]
fn stutters_on() -> bool {
    #[cfg(feature = "diagnostics")]
    {
        crate::runtime::stutters::on()
    }
    #[cfg(not(feature = "diagnostics"))]
    {
        false
    }
}

#[inline]
fn stutter_add(cost: usize, from: Option<u64>) {
    #[cfg(feature = "diagnostics")]
    if let Some(from) = from {
        crate::runtime::stutters::add(cost, clock::nanoseconds().wrapping_sub(from));
    }
    #[cfg(not(feature = "diagnostics"))]
    let _ = (cost, from);
}

// stutters::finished through its export (exports.rs), which starts the stall watcher once.
#[cfg(feature = "diagnostics")]
unsafe extern "C" {
    fn mw2_stutters_finished(serial: u64, world: bool);
}

#[inline]
fn stutters_finished(serial: u64, world: bool) {
    #[cfg(feature = "diagnostics")]
    if stutters_on() {
        // SAFETY: exports.rs's, any thread.
        unsafe { mw2_stutters_finished(serial, world) };
    }
    #[cfg(not(feature = "diagnostics"))]
    let _ = (serial, world);
}

#[inline]
fn pacing_note(event: u8, value: u32) {
    #[cfg(feature = "diagnostics")]
    if crate::runtime::pacing_trace::path().is_some() {
        crate::runtime::pacing_trace::note(clock::nanoseconds(), event, value, crate::runtime::platform::thread_id());
    }
    #[cfg(not(feature = "diagnostics"))]
    let _ = (event, value);
}

#[cfg_attr(not(feature = "diagnostics"), allow(dead_code, reason = "the report and the diagnostic keys' alone"))]
/// The recorder's queue position, drained and flushed through the calls this file makes:
/// for the trace, the dumps and Idle.
pub fn recorder_position() -> u64 {
    position()
}

pub fn drain_recorder() {
    drain();
}

pub fn flush_recorder() {
    flush();
}

/// completed_serial as the memory watch's Track is given it (vk::renderer::CompletedSubmission,
/// a cached number): the texture cache and the command processor's shader memo read it
/// without reaching into the renderer. Written by the ring consumer alone, where
/// note_completed samples it.
static COMPLETED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn completed_submission() -> u64 {
    COMPLETED.load(std::sync::atomic::Ordering::Relaxed)
}

/// steady_clock::now() as nanoseconds: the run's clock.
#[inline]
fn steady_now() -> u64 {
    clock::nanoseconds()
}

/// What the targets and the resolves are handed as the rest of the renderer: a Core and the
/// device, disjoint from them.
pub struct FrameView<'a> {
    pub vk: &'a Vk,
    pub core: &'a mut Core,
}

impl Frame for FrameView<'_> {
    #[inline]
    fn frames(&self) -> u64 {
        self.core.frames
    }
    #[inline]
    fn draws_recorded(&self) -> u64 {
        self.core.draws_recorded
    }
    #[inline]
    fn recording(&self) -> bool {
        self.core.recording
    }
    fn begin_frame(&mut self) -> bool {
        self.core.begin_frame(self.vk)
    }
    fn end_pass(&mut self) {
        self.core.end_pass();
    }
    fn submit(&mut self) -> bool {
        self.core.submit(self.vk, false)
    }
    fn retire_before_destroy(&mut self) {
        self.core.retire_before_destroy(self.vk);
    }
    fn finish_frame(&mut self, present: vk::Image, targets: &mut Targets, records: &mut ResolveRecords) {
        self.core.finish_frame(self.vk, present, targets, records);
    }
    fn skip(&mut self, why: &'static CStr) {
        self.core.skip(why);
    }
    fn set_current_scale(&mut self, scale: u32) {
        self.core.current_scale = scale;
    }
    fn open_guest_query(&mut self) {
        self.core.open_guest_query();
    }
    fn set_current(&mut self, colour: vk::ImageView, depth: vk::ImageView, samples: u32, width: u32, height: u32) {
        let c = &mut *self.core;
        c.current_colour = colour;
        c.current_depth = depth;
        c.current_samples = samples;
        c.current_width = width;
        c.current_height = height;
    }
    fn reset_bound(&mut self) {
        self.core.bound = Bound::default();
    }
    fn arena_used(&self) -> u32 {
        self.core.arena.used()
    }
    fn arena_slot_bytes(&self) -> u32 {
        self.core.arena.slot_bytes()
    }
    fn setup(&self) -> Setup {
        Setup { command: self.core.setup, fence: self.core.setup_fence, queue: self.core.queue }
    }
    fn texture_set(&self) -> vk::DescriptorSet {
        self.core.textures.set()
    }
    fn memory_set(&self) -> vk::DescriptorSet {
        self.core.memory_set
    }
    fn pipeline_layout(&self) -> vk::PipelineLayout {
        self.core.pipeline_layout
    }
    fn present_track(&mut self) -> &mut TrackedImage {
        &mut self.core.present_track[self.core.present_index as usize]
    }
    fn present(&self) -> Present {
        let i = self.core.present_index as usize;
        Present {
            image: self.core.present[i],
            serial: self.core.present_serial[i],
            width: self.core.present_width,
            height: self.core.present_height,
        }
    }
    fn timing(&self) -> bool {
        self.core.timing
    }
    fn pass_times_open(&mut self, colour: &Target, samples: u32) {
        let draws = self.core.draws;
        self.core.pass.open(colour, samples, draws);
    }
    fn pass_times_between(&mut self) {
        let draws = self.core.draws;
        self.core.pass.between(draws);
    }
    fn pass_times_mark(&mut self, name: fmt::Arguments<'_>) {
        let draws = self.core.draws;
        self.core.pass.mark_named(name, draws);
    }
    fn textures(&mut self) -> &mut TextureCache {
        &mut self.core.textures
    }
}
