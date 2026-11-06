// The window and the Vulkan device: runtime/gpu/vulkan/presenter.cpp in Rust. It runs
// on its own thread -- the guest owns the main thread and never gives it back -- and
// degrades to headless if there is no device. The window is made hidden, and shown by
// show_window(): a player's start that finds the game's shaders not compiled for this
// device hands over to the launcher before anything shows.
//
// The renderer hands over each frame it finishes (show_image: a VkImage in
// TRANSFER_SRC layout and its serial); the window shows every one, once, in order, a
// blank each, and the renderer draws into the image again only after
// wait_until_taken(serial) has returned.
//
// Unsafe by nature: the instance, surface, device and swapchain are made and destroyed
// on the window thread, the device by C++'s vk::pipeline::CreateDevice and destroyed by
// stop() after the renderer has torn down what it built on it (stop_presenting() first
// joins the window thread). A swapchain is replaced only with the presenter's two
// fences waited for and the present-wait thread kept out by `swapchain_life`; a frame's
// image is the renderer's until `taken` passes its serial, which happens only after
// the blit that reads it is submitted. The queue is shared with the renderer and used
// only under vk::pipeline::QueueMutex.
#![deny(unsafe_op_in_unsafe_fn)]

use super::backdrop;
#[cfg(not(mw2_gpu_rig))]
use super::backdrop::Backdrop;
use super::{QueueLock, entry, gpu_time, util};
use crate::env;
use crate::log::{log_info, log_warn};
use crate::runtime::screen::Image;
use crate::runtime::{bridge, clock, crash, frame_rate, friends, guide, platform, render_size, signin};
use ash::vk;
use ash::vk::Handle;
#[cfg(mw2_gpu_rig)]
use rig::Backdrop;
use sdl3_sys::everything as sdl;
use sdl3_sys::vulkan as sdl_vulkan;
use std::collections::VecDeque;
use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Condvar, LazyLock, Mutex, MutexGuard, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

const HEIGHT: u32 = 720;
/// How long a screen takes to fade in or out, in seconds.
const FADE: f32 = 0.15;
const FRAMES_IN_FLIGHT: u32 = 2;
/// How much longer the guest's blank may be than the display's while latency drains:
/// 0.5%, a frame in about three seconds, and nobody sees it.
const NUDGE: f64 = 0.005;

// ---- what the window decides, as functions of values -------------------------------

/// Where a frame lands in a window of another shape: as large as fits at the frame's
/// own proportions, centred, the rest black. Nothing is squashed, and the window can be
/// any shape at all. With `stretch` (MW2_STRETCH=1) it fills the window whatever the
/// shapes: a CRT TV fed a widescreen signal shows a 4:3 picture made as wide as that at
/// its own shape. The frame's sides are not 0.
pub fn letterbox(stretch: bool, extent: vk::Extent2D, frame_width: u32, frame_height: u32) -> [vk::Offset3D; 2] {
    if stretch {
        return [
            vk::Offset3D { x: 0, y: 0, z: 0 },
            vk::Offset3D { x: extent.width as i32, y: extent.height as i32, z: 1 },
        ];
    }
    let by_width = u64::from(extent.width) * u64::from(frame_height);
    let by_height = u64::from(extent.height) * u64::from(frame_width);
    let (mut width, mut height) = (extent.width, extent.height);
    if by_width > by_height {
        width = (by_height / u64::from(frame_height)) as u32; // wider than the frame
    } else {
        height = (by_width / u64::from(frame_width)) as u32; // taller than the frame
    }
    width = width.max(1);
    height = height.max(1);
    let from = vk::Offset3D {
        x: (extent.width.wrapping_sub(width) / 2) as i32,
        y: (extent.height.wrapping_sub(height) / 2) as i32,
        z: 0,
    };
    [from, vk::Offset3D { x: from.x.wrapping_add(width as i32), y: from.y.wrapping_add(height as i32), z: 1 }]
}

// std::clamp as libstdc++ has it, min(max(value, low), high), which differs from
// u32::clamp (it panics) where `low` is above `high`.
fn clamp(value: u32, low: u32, high: u32) -> u32 {
    value.max(low).min(high)
}

/// The swapchain's size. The surface says what size it is, except where the window
/// system leaves it to the application (the width at the maximum), and then the
/// window's own size in pixels is the answer, held within what the surface allows.
pub fn swapchain_extent(caps: &vk::SurfaceCapabilitiesKHR, window: impl FnOnce() -> (i32, i32)) -> vk::Extent2D {
    if caps.current_extent.width != u32::MAX {
        return caps.current_extent;
    }
    let (width, height) = window();
    vk::Extent2D {
        width: clamp(width.max(0) as u32, caps.min_image_extent.width, caps.max_image_extent.width),
        height: clamp(height.max(0) as u32, caps.min_image_extent.height, caps.max_image_extent.height),
    }
}

/// B8G8R8A8_UNORM where the surface has it, else its first. `formats` is not empty.
pub fn choose_format(formats: &[vk::SurfaceFormatKHR]) -> vk::SurfaceFormatKHR {
    formats.iter().copied().find(|format| format.format == vk::Format::B8G8R8A8_UNORM).unwrap_or(formats[0])
}

/// The swapchain's size, and what is asked for when there is one to make: none for a
/// window of no size (minimised) or a surface with no formats, which are asked for only
/// when the size is not 0.
pub fn plan_swapchain(
    caps: &vk::SurfaceCapabilitiesKHR,
    window: impl FnOnce() -> (i32, i32),
    formats: impl FnOnce() -> Vec<vk::SurfaceFormatKHR>,
    surface: vk::SurfaceKHR,
    old: vk::SwapchainKHR,
) -> (vk::Extent2D, Option<vk::SwapchainCreateInfoKHR<'static>>) {
    let extent = swapchain_extent(caps, window);
    if extent.width == 0 || extent.height == 0 {
        return (extent, None);
    }
    let formats = formats();
    if formats.is_empty() {
        return (extent, None);
    }
    (extent, Some(swapchain_info(caps, choose_format(&formats), extent, surface, old)))
}

/// Where a picture of this size sits, centred in the window.
pub fn centred(extent: vk::Extent2D, width: u32, height: u32) -> vk::Offset2D {
    vk::Offset2D {
        x: (extent.width.wrapping_sub(width) as i32) / 2,
        y: (extent.height.wrapping_sub(height) as i32) / 2,
    }
}

/// The swapchain asked for: FIFO, at least two images, blitted into.
pub fn swapchain_info(
    caps: &vk::SurfaceCapabilitiesKHR,
    chosen: vk::SurfaceFormatKHR,
    extent: vk::Extent2D,
    surface: vk::SurfaceKHR,
    old: vk::SwapchainKHR,
) -> vk::SwapchainCreateInfoKHR<'static> {
    vk::SwapchainCreateInfoKHR {
        surface,
        min_image_count: caps.min_image_count.max(2),
        image_format: chosen.format,
        image_color_space: chosen.color_space,
        image_extent: extent,
        image_array_layers: 1,
        // The renderer's frame is blitted in, so a transfer destination.
        image_usage: vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::COLOR_ATTACHMENT,
        image_sharing_mode: vk::SharingMode::EXCLUSIVE,
        pre_transform: caps.current_transform,
        composite_alpha: vk::CompositeAlphaFlagsKHR::OPAQUE,
        present_mode: vk::PresentModeKHR::FIFO, // always supported
        clipped: vk::TRUE,
        // The one being replaced, if any, so the driver can carry over what it can; it
        // is destroyed once the new one exists.
        old_swapchain: old,
        ..Default::default()
    }
}

/// The display mode SDL gives: its exact rate as a fraction, when it has one, and its
/// rounded rate.
#[derive(Clone, Copy)]
pub struct Mode {
    pub numerator: i32,
    pub denominator: i32,
    pub refresh: f32,
}

/// What the display's mode says: whether its rate is exact, the rate, the period, and
/// the period the guest's blank follows (0: not followed). The blank follows the
/// display when it runs at the console's 60 Hz and the device can say when a present
/// reaches it; a display at another rate cannot show 60 frames a second evenly anyway.
pub fn follow(mode: Option<Mode>, present_wait: bool) -> (bool, f64, f64, f64) {
    let (mut exact, mut hz) = (false, 0.0);
    if let Some(mode) = mode {
        // The mode's exact rate, from its pixel clock and totals, when the window system
        // gives it; otherwise measured (Display::saw_displayed).
        exact = mode.denominator != 0;
        hz = if exact { f64::from(mode.numerator) / f64::from(mode.denominator) } else { f64::from(mode.refresh) };
    }
    let period = 1e9 / if hz > 0.0 { hz } else { 60.0 };
    let nominal = if present_wait && (59.0..=61.0).contains(&hz) { period } else { 0.0 };
    (exact, hz, period, nominal)
}

/// The display's clock, as the presents reaching it measure it.
pub struct Display {
    pub nominal_ns: f64, // from the display mode; 0 = not followed
    pub period_ns: f64,
    pub last_shown_ns: i64,
    pub last_shown_id: u64,
    pub exact_rate: bool, // the display mode gave its rate exactly
    pub buffered: f64,    // frames queued behind the one on screen
    pub shown_on_time: u64,
    pub stretched: u64,
    pub behind_count: [u64; 4], // 0, 1, 2, 3+ frames behind at each
}

impl Display {
    pub const fn new() -> Display {
        Display {
            nominal_ns: 0.0,
            period_ns: 0.0,
            last_shown_ns: -1,
            last_shown_id: 0,
            exact_rate: false,
            buffered: 1.0,
            shown_on_time: 0,
            stretched: 0,
            behind_count: [0; 4],
        }
    }

    /// Frame `id` reached the screen at `at`, with `queued` the last frame queued. A
    /// frame held on screen longer than a blank is a stutter (`displayed`: the frame,
    /// the blanks it was late and the gap in ms); the guest's blank is set to the
    /// display's, a touch longer while more than one frame waits behind the one on
    /// screen, so latency a stall built up drains away unseen. It is never set shorter:
    /// a frame late once leaves one frame in hand from then on, which is double
    /// buffering, and when the renderer cannot keep up, hurrying the title only gives it
    /// more to fall behind on. Returns the guest's blank period and its time, when the
    /// display is followed.
    ///
    /// The display's period comes from its mode where the mode says it exactly.
    /// Measured from these completions it read 17.1 ms against the panel's 16.67 in a
    /// run where frames were often late -- late completions pull an average up -- and
    /// the guest's blank followed it down to 58.5 Hz. On the console the blank the title
    /// counts is the display's own; a timer of ours beside it drifts, and every so often
    /// a frame is shown twice or waits a frame longer.
    pub fn saw_displayed(
        &mut self,
        id: u64,
        at: i64,
        queued: u64,
        console: bool,
        displayed: &mut dyn FnMut(u64, u32, f64),
    ) -> Option<(u64, i64)> {
        // Off the blank nothing waits its turn behind the frame on screen: the frames
        // after it are let go for the newest (draw_one_frame).
        let behind = if console && queued > id { (queued - id) as f64 } else { 0.0 };
        if self.last_shown_ns >= 0 && id > self.last_shown_id {
            let gap = at.wrapping_sub(self.last_shown_ns) as f64;
            let frames = id - self.last_shown_id;
            let blanks = (gap / self.period_ns).round() as i64;
            // Only a gap no longer than the period can be trusted to measure it.
            if !self.exact_rate && frames == 1 && gap <= self.period_ns * 1.02 && gap > self.period_ns * 0.9 {
                self.period_ns += (gap - self.period_ns) / 256.0;
            }
            if blanks > frames as i64 {
                displayed(id, (blanks - frames as i64) as u32, gap / 1e6);
            }
        }
        self.last_shown_ns = at;
        self.last_shown_id = id;
        self.buffered += (behind - self.buffered) / 16.0;
        self.behind_count[(behind as u64).min(3) as usize] += 1;
        if self.nominal_ns == 0.0 {
            return None;
        }
        let stretch = if self.buffered > 1.5 { NUDGE } else { 0.0 };
        if stretch > 0.0 {
            self.stretched += 1;
        } else {
            self.shown_on_time += 1;
        }
        Some(((self.period_ns * (1.0 + stretch)) as u64, at))
    }
}

impl Default for Display {
    fn default() -> Self {
        Display::new()
    }
}

/// The guest's blank period in nanoseconds, or 0 when nothing reached the screen for
/// half a second (minimised, or no window yet): there is no display clock to follow.
pub fn blank_period(period: u64, at: i64, now: i64) -> u64 {
    if period == 0 || now.wrapping_sub(at) > 500_000_000 { 0 } else { period }
}

/// How far a screen has faded in, from 0 (shut) to 1, after `elapsed_ns` more: with the
/// backdrop it fades at 1/FADE a second, at most a 30th of a second's worth at a time,
/// and without it it is shown or not.
pub fn fade_step(fade: f32, elapsed_ns: i64, layered: bool, picture: bool) -> f32 {
    let seconds = elapsed_ns as f32 / 1_000_000_000.0f32;
    let longest = 1.0f32 / 30.0;
    let step = if longest < seconds { longest } else { seconds } / FADE;
    if !layered {
        return if picture { 1.0 } else { 0.0 };
    }
    // std::clamp's: NaN stays NaN, as with f32::clamp.
    (fade + if picture { step } else { -step }).clamp(0.0, 1.0)
}

/// The fade eased, so it settles rather than stops.
pub fn eased(fade: f32) -> f32 {
    1.0 - (1.0 - fade) * (1.0 - fade)
}

/// A frame the renderer has finished and the window has not shown yet.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Queued {
    pub image: vk::Image,
    pub width: u32,
    pub height: u32,
    pub serial: u64,
}

/// The frames the renderer has finished and the window has not shown yet, oldest
/// first. Each is shown once, in order: the window used to show whichever frame was
/// newest when the title asked for a present, and a frame finishing about then was
/// shown twice or never.
#[derive(Default)]
pub struct FrameQueue {
    pub frames: VecDeque<Queued>,
    pub taken: u64, // the last frame copied out or let go
    wake_sleepers: u32,
    taken_sleepers: u32,
}

impl FrameQueue {
    pub const fn new() -> FrameQueue {
        FrameQueue { frames: VecDeque::new(), taken: 0, wake_sleepers: 0, taken_sleepers: 0 }
    }

    /// The frame to show next: off the blank (MW2_FPS_LIMIT) the title finishes frames
    /// whenever it likes, and the display still takes one a blank, so the newest is
    /// shown and the ones before it are let go. True with it when any was let go.
    pub fn take(&mut self, console: bool) -> (Option<Queued>, bool) {
        let mut let_go = false;
        while !console && self.frames.len() > 1 {
            let front = self.frames.pop_front().unwrap_or_default();
            self.taken = self.taken.max(front.serial);
            let_go = true;
        }
        (self.frames.pop_front(), let_go)
    }

    /// The frames queued up to now are let go without being shown. False when there
    /// were none.
    pub fn let_go(&mut self) -> bool {
        let Some(newest) = self.frames.back() else { return false };
        self.taken = self.taken.max(newest.serial);
        self.frames.clear();
        true
    }

    /// The window took no frame in time: the frames up to `serial` go unshown.
    pub fn give_up(&mut self, serial: u64) {
        while self.frames.front().is_some_and(|front| front.serial <= serial) {
            self.frames.pop_front();
        }
        self.taken = self.taken.max(serial);
    }
}

/// The present ids not yet seen on screen, and the display's clock they measure.
#[derive(Default)]
pub struct WaitState {
    pub waiting: VecDeque<u64>,
    pub display: Display,
    sleepers: u32,
}

/// What the window thread shares with the renderer's threads and the present-wait
/// thread. Lock order: `swapchain_life`, then `wait`. A condition variable is notified
/// only when somebody waits on it, as glibc's skip a signal nobody waits for.
pub struct Shared {
    pub running: AtomicBool,
    pub ready: AtomicBool,
    pub show_asked: AtomicBool,
    pub presented: AtomicU64,
    pub last_queued: AtomicU64,
    pub guest_period_ns: AtomicU64,
    pub guest_period_at: AtomicI64,
    // How many times frames were let go with a line in the log: at most 4 lines.
    pub told: AtomicU32,
    pub queue: Mutex<FrameQueue>,
    wake: Condvar,
    taken_wake: Condvar,
    pub wait: Mutex<WaitState>,
    wait_wake: Condvar,
    // Held shared while waiting on the swapchain, exclusive to replace it.
    swapchain_life: RwLock<vk::SwapchainKHR>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Shared {
    pub const fn new() -> Shared {
        Shared {
            running: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            show_asked: AtomicBool::new(false),
            presented: AtomicU64::new(0),
            last_queued: AtomicU64::new(0),
            guest_period_ns: AtomicU64::new(0),
            guest_period_at: AtomicI64::new(0),
            told: AtomicU32::new(0),
            queue: Mutex::new(FrameQueue::new()),
            wake: Condvar::new(),
            taken_wake: Condvar::new(),
            wait: Mutex::new(WaitState { waiting: VecDeque::new(), display: Display::new(), sleepers: 0 }),
            wait_wake: Condvar::new(),
            swapchain_life: RwLock::new(vk::SwapchainKHR::null()),
        }
    }

    fn queue(&self) -> MutexGuard<'_, FrameQueue> {
        lock(&self.queue)
    }

    fn wait_state(&self) -> MutexGuard<'_, WaitState> {
        lock(&self.wait)
    }

    fn taken_changed(&self, queue: &FrameQueue) {
        if queue.taken_sleepers != 0 {
            self.taken_wake.notify_all();
        }
    }

    /// A frame the renderer has finished (show_image).
    pub fn show_image(&self, image: vk::Image, width: u32, height: u32, serial: u64) {
        if !self.running.load(Ordering::Relaxed) {
            return;
        }
        let mut queue = self.queue();
        queue.frames.push_back(Queued { image, width, height, serial });
        self.last_queued.store(serial, Ordering::Relaxed);
        if queue.wake_sleepers != 0 {
            self.wake.notify_one();
        }
    }

    /// wait_until_taken with `wait`, which waits on `taken_wake` for at most 250 ms
    /// while the condition it is given holds, and says whether it timed out with the
    /// condition still holding. True when the frames were let go with a line in the log.
    pub fn wait_until_taken_with<'a>(
        &'a self,
        serial: u64,
        wait: impl FnOnce(
            MutexGuard<'a, FrameQueue>,
            &dyn Fn(&mut FrameQueue) -> bool,
        ) -> (MutexGuard<'a, FrameQueue>, bool),
    ) -> bool {
        if serial == 0 || !self.running.load(Ordering::Relaxed) {
            return false;
        }
        let queue = self.queue();
        let waiting = |queue: &mut FrameQueue| !(queue.taken >= serial || !self.running.load(Ordering::SeqCst));
        let (mut queue, timed_out) = wait(queue, &waiting);
        if !timed_out {
            return false;
        }
        // A quarter of a second without the window taking a frame: it is hidden or its
        // display has stopped. The game does not wait on a window.
        queue.give_up(serial);
        self.told.fetch_add(1, Ordering::SeqCst) < 4
    }

    fn wait_until_taken(&self, serial: u64) {
        let logged = self.wait_until_taken_with(serial, |mut queue, waiting| {
            queue.taken_sleepers += 1;
            let (mut queue, result) = self
                .taken_wake
                .wait_timeout_while(queue, Duration::from_millis(250), |queue| waiting(queue))
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            queue.taken_sleepers -= 1;
            (queue, result.timed_out())
        });
        if logged {
            log_warn!("vulkan: the window took no frame in 250 ms; frames up to %llu let go unshown", serial);
        }
    }

    /// The frames queued up to now are let go without being shown: there is no
    /// swapchain to show them in (a minimised window), and the renderer must not wait
    /// on a window that cannot take them.
    pub fn let_go(&self) {
        let mut queue = self.queue();
        if queue.let_go() {
            self.taken_changed(&queue);
        }
    }
}

impl Default for Shared {
    fn default() -> Self {
        Shared::new()
    }
}

/// The window's steps of a frame that touch Vulkan, so that draw_one_frame's order and
/// decisions can be run on their own (tools/gpu_diff, the presenter rig).
pub trait Steps {
    /// RemakeSwapchain: the presenter's fences waited for, and whether a swapchain was
    /// made.
    fn remake_swapchain(&mut self) -> bool;
    fn has_swapchain(&self) -> bool;
    /// The frame's fence waited for; true when that failed.
    fn wait_frame(&mut self, frame: u32) -> bool;
    fn acquire(&mut self, frame: u32) -> (vk::Result, u32);
    /// Everything from resetting the frame's fence to ending its command buffer.
    fn record(&mut self, frame: u32, image_index: u32, shown: &Queued);
    fn submit(&mut self, frame: u32);
    fn present(&mut self, frame: u32, image_index: u32, id: Option<u64>) -> vk::Result;
    /// vk::pipeline::Failed on a present's result that is not out of date.
    fn present_failed(&mut self, result: vk::Result);
    /// stutters::Shown.
    fn shown(&mut self, serial: u64);
}

/// The window thread's state of the frame loop.
#[derive(Default)]
pub struct Flow {
    // The window changed size, or the swapchain said it no longer fits: remade before
    // the next frame. A window of no size -- minimised -- has nothing to remake it for,
    // and frames are skipped until it has one.
    pub remake: bool,
    pub frame_index: u32,
    pub present_wait: bool,
}

/// Shows the oldest queued frame, or with `idle` the window's own colour before the
/// renderer has finished any.
pub fn draw_one_frame(flow: &mut Flow, steps: &mut impl Steps, shared: &Shared, idle: bool, console: bool) {
    if flow.remake && steps.remake_swapchain() {
        flow.remake = false;
    }
    if flow.remake || !steps.has_swapchain() {
        shared.let_go();
        return;
    }
    let frame = flow.frame_index;
    if steps.wait_frame(frame) {
        return;
    }
    // FIFO: this is where the window waits for the display, with the frame it will show
    // still in the queue, so the queue's length is how far the renderer is ahead of the
    // screen.
    let (acquired, image_index) = steps.acquire(frame);
    // Out of date: nothing was acquired, and the semaphore is untouched; remade next
    // frame. Suboptimal: an image was acquired and is shown, since the semaphore it
    // signals has to be waited on, and then remade.
    if acquired == vk::Result::ERROR_OUT_OF_DATE_KHR {
        flow.remake = true;
        return;
    }
    if acquired == vk::Result::SUBOPTIMAL_KHR {
        flow.remake = true;
    } else if acquired != vk::Result::SUCCESS {
        return;
    }

    let mut shown = Queued::default();
    if !idle {
        let mut queue = shared.queue();
        let (front, let_go) = queue.take(console);
        if let_go {
            shared.taken_changed(&queue);
        }
        shown = front.unwrap_or_default();
    }
    steps.record(frame, image_index, &shown);
    steps.submit(frame);
    // The copy is in the queue ahead of anything the renderer submits from now on, so
    // the renderer may draw over the image.
    if shown.serial != 0 {
        let mut queue = shared.queue();
        queue.taken = queue.taken.max(shown.serial);
        shared.taken_changed(&queue);
    }
    if shown.serial != 0 {
        steps.shown(shown.serial);
    }
    // The frame's serial is its present id: they only ever go up.
    let id = (flow.present_wait && shown.serial != 0).then_some(shown.serial);
    let presented = steps.present(frame, image_index, id);
    // Out of date and suboptimal here mean the window changed under the present: the
    // swapchain is remade before the next frame. Only a lost device is a failure.
    if presented == vk::Result::ERROR_OUT_OF_DATE_KHR || presented == vk::Result::SUBOPTIMAL_KHR {
        flow.remake = true;
    } else {
        steps.present_failed(presented);
    }
    if flow.present_wait
        && shown.serial != 0
        && (presented == vk::Result::SUCCESS || presented == vk::Result::SUBOPTIMAL_KHR)
    {
        let mut wait = shared.wait_state();
        wait.waiting.push_back(shown.serial);
        if wait.sleepers != 0 {
            shared.wait_wake.notify_one();
        }
    }
    flow.frame_index = (flow.frame_index + 1) % FRAMES_IN_FLIGHT;
    shared.presented.fetch_add(1, Ordering::Relaxed);
}

// ---- the window --------------------------------------------------------------------

static SHARED: Shared = Shared::new();
static WORKER: Mutex<Option<JoinHandle<Presenter>>> = Mutex::new(None);
static WAITER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
// The window thread's state once it has stopped, for stop() to tear down.
static STOPPED: Mutex<Option<Presenter>> = Mutex::new(None);
// Under VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation, what the layer reports goes
// into this log. Left to itself it prints to stdout, which is block-buffered into a
// file and never flushed at this runtime's exit: a run with a hundred errors read as a
// clean one.
static VALIDATION_MESSAGES: AtomicU64 = AtomicU64::new(0);

#[cfg(not(mw2_gpu_rig))]
fn nanoseconds() -> i64 {
    clock::nanoseconds() as i64
}

#[cfg(mw2_gpu_rig)]
fn nanoseconds() -> i64 {
    rig::now()
}

// frame_rate::Console(): frames retired at the blank, one a blank, as on the console.
fn console_rate() -> bool {
    static CONSOLE: LazyLock<bool> = LazyLock::new(|| frame_rate::limit() == 60);
    *CONSOLE
}

fn sdl_error() -> &'static CStr {
    let text = sdl::SDL_GetError();
    // SAFETY: SDL's own message for this thread, which it keeps.
    if text.is_null() { c"" } else { unsafe { CStr::from_ptr(text) } }
}

fn check(result: vk::Result, what: &CStr) -> bool {
    if result == vk::Result::SUCCESS {
        return true;
    }
    log_warn!("vulkan: %s failed (%d)", what, result.as_raw());
    false
}

fn result_of<T>(result: Result<T, vk::Result>) -> vk::Result {
    result.err().unwrap_or(vk::Result::SUCCESS)
}

unsafe extern "system" fn validation(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    _types: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    _user: *mut c_void,
) -> vk::Bool32 {
    let n = VALIDATION_MESSAGES.fetch_add(1, Ordering::Relaxed);
    if n < 500 {
        // SAFETY: the layer's callback data and message, valid for the call.
        let message = unsafe {
            if data.is_null() || (*data).p_message.is_null() { c"?" } else { CStr::from_ptr((*data).p_message) }
        };
        let kind = if severity.as_raw() >= vk::DebugUtilsMessageSeverityFlagsEXT::ERROR.as_raw() {
            c"error"
        } else {
            c"warning"
        };
        log_warn!("validation %s: %s", kind, message);
    }
    vk::FALSE
}

fn validating() -> bool {
    env::text(c"VK_INSTANCE_LAYERS").is_some_and(|layers| layers.to_bytes().windows(10).any(|w| w == b"validation"))
}

// The window's size in pixels.
#[cfg(not(mw2_gpu_rig))]
fn window_size(window: *mut sdl::SDL_Window) -> (i32, i32) {
    let (mut width, mut height) = (0, 0);
    // SAFETY: the window thread's window.
    unsafe { sdl::SDL_GetWindowSizeInPixels(window, &mut width, &mut height) };
    (width, height)
}

#[cfg(mw2_gpu_rig)]
fn window_size(window: *mut sdl::SDL_Window) -> (i32, i32) {
    rig::window_size(window)
}

// The system's screen open now, if any, its picture made for a window this tall into
// its slot: the sign-in screen, the friends list or the guide.
#[cfg(not(mw2_gpu_rig))]
fn open_picture(window_height: u32, pictures: &mut [Image; 3]) -> Option<usize> {
    let [signin_picture, friends_picture, guide_picture] = pictures;
    if signin::picture(window_height, signin_picture) {
        Some(0)
    } else if friends::picture(window_height, friends_picture) {
        Some(1)
    } else if guide::picture(window_height, guide_picture) {
        Some(2)
    } else {
        None
    }
}

#[cfg(mw2_gpu_rig)]
fn open_picture(_window_height: u32, pictures: &mut [Image; 3]) -> Option<usize> {
    rig::picture(&mut pictures[0]).then_some(0)
}

#[derive(Clone, Copy, Default)]
struct Frame {
    commands: vk::CommandBuffer,
    acquired: vk::Semaphore,
    rendered: vk::Semaphore,
    in_flight: vk::Fence,
}

// The system's screens (screen.rs), laid over the frame while one is open: their
// pictures, the image the one shown is copied to and the buffer it goes by.
struct Overlay {
    pictures: [Image; 3], // signin, friends, guide
    shown: Option<usize>, // the picture the image holds
    outline: f32,         // and its outline's width
    image: vk::Image,
    image_memory: vk::DeviceMemory,
    view: vk::ImageView,
    staging: vk::Buffer,
    staging_memory: vk::DeviceMemory,
    mapped: *mut u8,
    width: u32,
    height: u32,
    uploaded: u64, // the picture's version the image holds
    ever_uploaded: bool,
    fade: f32,     // 0 shut to 1 shown
    layered: bool, // the image is laid out for the backdrop's shader
    frame_from: vk::Offset3D,
    frame_to: vk::Offset3D, // where the last game frame was shown
    drawn: i64,
}

impl Default for Overlay {
    fn default() -> Self {
        Overlay {
            pictures: Default::default(),
            shown: None,
            outline: 0.0,
            image: vk::Image::null(),
            image_memory: vk::DeviceMemory::null(),
            view: vk::ImageView::null(),
            staging: vk::Buffer::null(),
            staging_memory: vk::DeviceMemory::null(),
            mapped: std::ptr::null_mut(),
            width: 0,
            height: 0,
            uploaded: 0,
            ever_uploaded: false,
            fade: 0.0,
            layered: false,
            frame_from: vk::Offset3D::default(),
            frame_to: vk::Offset3D::default(),
            drawn: 0,
        }
    }
}

/// The window thread's own: the window, the instance and the device it presents with.
pub struct Presenter {
    window: *mut sdl::SDL_Window,
    instance: Option<ash::Instance>,
    surface_fn: Option<ash::khr::surface::Instance>,
    debug_utils: Option<ash::ext::debug_utils::Instance>,
    messenger: vk::DebugUtilsMessengerEXT,
    surface: vk::SurfaceKHR,
    physical: vk::PhysicalDevice,
    device: Option<ash::Device>,
    swapchain_fn: Option<ash::khr::swapchain::Device>,
    present_wait_fn: Option<ash::khr::present_wait::Device>,
    queue_family: u32,
    queue: vk::Queue,
    swapchain: vk::SwapchainKHR,
    swapchain_format: vk::Format,
    extent: vk::Extent2D,
    images: Vec<vk::Image>,
    fullscreen: bool,
    stretch: bool, // MW2_STRETCH
    swapchains_made: u64,
    present_wait: bool,
    pool: vk::CommandPool,
    frames: [Frame; FRAMES_IN_FLIGHT as usize],
    overlay: Overlay,
    backdrop: Backdrop,
    shown: bool, // the window is shown
}

// SAFETY: the window and the mapped staging memory are used by one thread at a time:
// the window thread, and once it has been joined, the thread that stops the presenter.
unsafe impl Send for Presenter {}

impl Presenter {
    fn new() -> Presenter {
        Presenter {
            window: std::ptr::null_mut(),
            instance: None,
            surface_fn: None,
            debug_utils: None,
            messenger: vk::DebugUtilsMessengerEXT::null(),
            surface: vk::SurfaceKHR::null(),
            physical: vk::PhysicalDevice::null(),
            device: None,
            swapchain_fn: None,
            present_wait_fn: None,
            queue_family: 0,
            queue: vk::Queue::null(),
            swapchain: vk::SwapchainKHR::null(),
            swapchain_format: vk::Format::UNDEFINED,
            extent: vk::Extent2D { width: 1280, height: HEIGHT },
            images: Vec::new(),
            fullscreen: false,
            stretch: false,
            swapchains_made: 0,
            present_wait: false,
            pool: vk::CommandPool::null(),
            frames: [Frame::default(); FRAMES_IN_FLIGHT as usize],
            overlay: Overlay::default(),
            backdrop: Backdrop::default(),
            shown: false,
        }
    }

    fn instance(&self) -> &ash::Instance {
        self.instance.as_ref().expect("the presenter's instance")
    }

    fn device(&self) -> &ash::Device {
        self.device.as_ref().expect("the presenter's device")
    }

    fn surface_fn(&self) -> &ash::khr::surface::Instance {
        self.surface_fn.as_ref().expect("the surface's commands")
    }

    fn swapchain_fn(&self) -> &ash::khr::swapchain::Device {
        self.swapchain_fn.as_ref().expect("the swapchain's commands")
    }

    fn create_instance(&mut self) -> bool {
        let mut count = 0u32;
        // SAFETY: SDL's list, which it keeps, of `count` names.
        let names = unsafe { sdl_vulkan::SDL_Vulkan_GetInstanceExtensions(&mut count) };
        if names.is_null() {
            log_warn!("vulkan: SDL has no instance extensions (%s)", sdl_error());
            return false;
        }
        // SAFETY: as above.
        let mut extensions: Vec<*const c_char> = unsafe { std::slice::from_raw_parts(names, count as usize) }.to_vec();

        let app = vk::ApplicationInfo {
            p_application_name: c"mw2recomp".as_ptr(),
            api_version: vk::API_VERSION_1_2,
            ..Default::default()
        };
        let messenger = vk::DebugUtilsMessengerCreateInfoEXT {
            message_severity: vk::DebugUtilsMessageSeverityFlagsEXT::WARNING
                | vk::DebugUtilsMessageSeverityFlagsEXT::ERROR,
            message_type: vk::DebugUtilsMessageTypeFlagsEXT::GENERAL | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION,
            pfn_user_callback: Some(validation),
            ..Default::default()
        };
        let validating = validating();
        if validating {
            extensions.push(ash::ext::debug_utils::NAME.as_ptr());
        }
        let info = vk::InstanceCreateInfo {
            // instance creation's own messages
            p_next: if validating { (&raw const messenger).cast() } else { std::ptr::null() },
            p_application_info: &app,
            enabled_extension_count: extensions.len() as u32,
            pp_enabled_extension_names: extensions.as_ptr(),
            ..Default::default()
        };
        // SAFETY: the infos and names above live through the call.
        let instance = match unsafe { entry().create_instance(&info, None) } {
            Ok(instance) => instance,
            Err(error) => return check(error, c"vkCreateInstance"),
        };
        self.surface_fn = Some(ash::khr::surface::Instance::new(entry(), &instance));
        if validating {
            // SAFETY: a name asked of the live instance.
            let create = unsafe {
                entry().get_instance_proc_addr(instance.handle(), c"vkCreateDebugUtilsMessengerEXT".as_ptr())
            };
            let debug_utils = ash::ext::debug_utils::Instance::new(entry(), &instance);
            // SAFETY: the messenger's info, on the instance that has the extension.
            let made = create.is_some()
                && match unsafe { debug_utils.create_debug_utils_messenger(&messenger, None) } {
                    Ok(made) => {
                        self.messenger = made;
                        true
                    }
                    Err(_) => false,
                };
            if made {
                log_info!("vulkan: validation messages go to this log");
                self.debug_utils = Some(debug_utils);
            } else {
                log_warn!("vulkan: validation asked for, but its messages cannot be caught");
            }
        }
        self.instance = Some(instance);
        true
    }

    fn pick_device(&mut self) -> bool {
        let instance = self.instance().clone();
        // SAFETY (the function): queries of the live instance's physical devices, about
        // the live surface.
        let devices = unsafe { instance.enumerate_physical_devices() }.unwrap_or_default();
        if devices.is_empty() {
            log_warn!("vulkan: no physical devices");
            return false;
        }
        let name =
            |properties: &vk::PhysicalDeviceProperties| properties.device_name_as_c_str().unwrap_or(c"?").to_owned();
        // Prefer a discrete device, but take whatever can present.
        let mut fallback: Option<(vk::PhysicalDevice, u32)> = None;
        for &candidate in &devices {
            let families = unsafe { instance.get_physical_device_queue_family_properties(candidate) };
            for (i, family) in families.iter().enumerate() {
                let i = i as u32;
                if !family.queue_flags.contains(vk::QueueFlags::GRAPHICS) {
                    continue;
                }
                let can_present =
                    unsafe { self.surface_fn().get_physical_device_surface_support(candidate, i, self.surface) }
                        .unwrap_or(false);
                if !can_present {
                    continue;
                }
                let info = unsafe { instance.get_physical_device_properties(candidate) };
                if info.device_type == vk::PhysicalDeviceType::DISCRETE_GPU {
                    (self.physical, self.queue_family) = (candidate, i);
                    log_info!("vulkan: %s", name(&info).as_c_str());
                    return true;
                }
                if fallback.is_none() {
                    fallback = Some((candidate, i));
                }
                break;
            }
        }
        let Some((physical, family)) = fallback else {
            log_warn!("vulkan: no device can present to the window");
            return false;
        };
        (self.physical, self.queue_family) = (physical, family);
        let info = unsafe { instance.get_physical_device_properties(physical) };
        log_info!("vulkan: %s", name(&info).as_c_str());
        true
    }

    // The renderer draws on this device when there is a window, so it is made the way
    // pipeline.cpp makes its own, with the swapchain beside.
    fn create_device(&mut self) -> bool {
        let instance = self.instance().clone();
        let physical = self.physical.as_raw() as *mut c_void;
        let mut extensions: Vec<*const c_char> = vec![ash::khr::swapchain::NAME.as_ptr()];
        // Present ids and waiting on them say when a frame reached the screen, which is
        // the display's clock.
        let mut present_id = vk::PhysicalDevicePresentIdFeaturesKHR::default();
        let mut present_wait = vk::PhysicalDevicePresentWaitFeaturesKHR::default();
        let mut features: *mut c_void = std::ptr::null_mut();
        if bridge::vulkan::device_has_extension(physical, ash::khr::present_id::NAME)
            && bridge::vulkan::device_has_extension(physical, ash::khr::present_wait::NAME)
        {
            present_id.p_next = (&raw mut present_wait).cast();
            let mut query = vk::PhysicalDeviceFeatures2 { p_next: (&raw mut present_id).cast(), ..Default::default() };
            // SAFETY: a chain of structs on this stack, which do not move.
            unsafe { instance.get_physical_device_features2(self.physical, &mut query) };
            if present_id.present_id != 0 && present_wait.present_wait != 0 {
                extensions.push(ash::khr::present_id::NAME.as_ptr());
                extensions.push(ash::khr::present_wait::NAME.as_ptr());
                features = (&raw mut present_id).cast();
                self.present_wait = true;
            }
        }
        // SAFETY: the instance's physical device, C strings and a chain on this stack.
        let Some((device, queue)) =
            (unsafe { bridge::vulkan::create_device(physical, self.queue_family, &extensions, features) })
        else {
            return false;
        };
        // SAFETY: the device pipeline.cpp just made on this instance.
        let device = unsafe { ash::Device::load(instance.fp_v1_0(), vk::Device::from_raw(device as u64)) };
        self.queue = vk::Queue::from_raw(queue as u64);
        self.swapchain_fn = Some(ash::khr::swapchain::Device::new(&instance, &device));
        if self.present_wait {
            // SAFETY: a name asked of the live device.
            let wait = unsafe { instance.get_device_proc_addr(device.handle(), c"vkWaitForPresentKHR".as_ptr()) };
            self.present_wait = wait.is_some();
            if self.present_wait {
                self.present_wait_fn = Some(ash::khr::present_wait::Device::new(&instance, &device));
            }
        }
        self.device = Some(device);
        if !self.present_wait {
            log_warn!(
                "vulkan: the device cannot say when a present reaches the screen (VK_KHR_present_wait); the guest's blank keeps its own 60 Hz clock"
            );
        }
        true
    }

    fn create_swapchain(&mut self) -> bool {
        // SAFETY (the function): Vulkan calls on the window thread's live surface and
        // device; the old swapchain is destroyed with the present-wait thread kept out.
        let caps = unsafe { self.surface_fn().get_physical_device_surface_capabilities(self.physical, self.surface) }
            .unwrap_or_default();
        let window = self.window;
        let old = self.swapchain;
        let (extent, info) = plan_swapchain(
            &caps,
            || window_size(window),
            || {
                // Asked for again only when there are any, as many as the first answer.
                let formats_of = self.surface_fn().fp().get_physical_device_surface_formats_khr;
                let mut count = 0u32;
                let _ = unsafe { formats_of(self.physical, self.surface, &mut count, std::ptr::null_mut()) };
                let mut formats = vec![vk::SurfaceFormatKHR::default(); count as usize];
                if count != 0 {
                    let _ = unsafe { formats_of(self.physical, self.surface, &mut count, formats.as_mut_ptr()) };
                }
                formats
            },
            self.surface,
            old,
        );
        self.extent = extent;
        // Minimised, or a surface with no formats: nothing to make.
        let Some(info) = info else { return false };
        self.swapchain_format = info.image_format;
        let device = self.device().handle();
        let mut made = vk::SwapchainKHR::null();
        let result =
            unsafe { (self.swapchain_fn().fp().create_swapchain_khr)(device, &info, std::ptr::null(), &mut made) };
        let ok = check(result, c"vkCreateSwapchainKHR");
        {
            // Nothing waits on a present of the old one once it is gone.
            let mut life = SHARED.swapchain_life.write().unwrap_or_else(|poisoned| poisoned.into_inner());
            if old != vk::SwapchainKHR::null() {
                unsafe { self.swapchain_fn().destroy_swapchain(old, None) };
            }
            self.swapchain = if ok { made } else { vk::SwapchainKHR::null() };
            *life = self.swapchain;
            let mut wait = SHARED.wait_state();
            wait.waiting.clear();
            wait.display.last_shown_ns = -1;
        }
        if !ok {
            return false;
        }
        self.swapchains_made += 1;
        let images_of = self.swapchain_fn().fp().get_swapchain_images_khr;
        let mut count = 0u32;
        let _ = unsafe { images_of(device, self.swapchain, &mut count, std::ptr::null_mut()) };
        self.images = vec![vk::Image::null(); count as usize];
        let _ = unsafe { images_of(device, self.swapchain, &mut count, self.images.as_mut_ptr()) };
        log_info!("vulkan: swapchain %ux%u, %u images", self.extent.width, self.extent.height, count);
        true
    }

    fn create_frames(&mut self) -> bool {
        let device = self.device().clone();
        let pool_info = vk::CommandPoolCreateInfo {
            flags: vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER,
            queue_family_index: self.queue_family,
            ..Default::default()
        };
        // SAFETY (the function): objects made on the window thread's device.
        match unsafe { device.create_command_pool(&pool_info, None) } {
            Ok(pool) => self.pool = pool,
            Err(error) => return check(error, c"vkCreateCommandPool"),
        }
        for frame in &mut self.frames {
            let allocate = vk::CommandBufferAllocateInfo {
                command_pool: self.pool,
                level: vk::CommandBufferLevel::PRIMARY,
                command_buffer_count: 1,
                ..Default::default()
            };
            match unsafe { device.allocate_command_buffers(&allocate) } {
                Ok(buffers) => frame.commands = buffers[0],
                Err(error) => return check(error, c"vkAllocateCommandBuffers"),
            }
            let semaphore = vk::SemaphoreCreateInfo::default();
            let fence = vk::FenceCreateInfo { flags: vk::FenceCreateFlags::SIGNALED, ..Default::default() };
            match unsafe { device.create_semaphore(&semaphore, None) } {
                Ok(made) => frame.acquired = made,
                Err(error) => return check(error, c"vkCreateSemaphore"),
            }
            match unsafe { device.create_semaphore(&semaphore, None) } {
                Ok(made) => frame.rendered = made,
                Err(error) => return check(error, c"vkCreateSemaphore"),
            }
            match unsafe { device.create_fence(&fence, None) } {
                Ok(made) => frame.in_flight = made,
                Err(error) => return check(error, c"vkCreateFence"),
            }
        }
        true
    }

    // The fences of the presenter's frames other than `frame`, waited for.
    fn settle_others(device: &ash::Device, frames: &[Frame], frame: u32) {
        for (i, other) in frames.iter().enumerate() {
            if i as u32 != frame {
                // SAFETY: the presenter's own fence.
                let _ = unsafe { device.wait_for_fences(&[other.in_flight], true, u64::MAX) };
            }
        }
    }

    fn follow_the_display(&mut self) {
        // SAFETY: SDL's mode of the display the live window is on, which it keeps.
        let mode = unsafe {
            let mode = sdl::SDL_GetCurrentDisplayMode(sdl::SDL_GetDisplayForWindow(self.window));
            (!mode.is_null()).then(|| Mode {
                numerator: (*mode).refresh_rate_numerator,
                denominator: (*mode).refresh_rate_denominator,
                refresh: (*mode).refresh_rate,
            })
        };
        let (exact, hz, period, nominal) = follow(mode, self.present_wait);
        {
            let mut wait = SHARED.wait_state();
            wait.display.exact_rate = exact;
            wait.display.period_ns = period;
            wait.display.nominal_ns = nominal;
        }
        if !self.present_wait {
            return;
        }
        if nominal != 0.0 {
            log_info!("vulkan: the display runs at %.4f Hz; the guest's blank follows it", hz);
        } else {
            log_info!("vulkan: the display runs at %.3f Hz, not 60; the guest's blank keeps its own clock", hz);
        }
        let wait = self.present_wait_fn.clone().expect("present wait's commands");
        let spawned = std::thread::Builder::new()
            .name("vulkan present wait".into())
            .stack_size(platform::default_thread_stack())
            .spawn(move || waiter(wait));
        match spawned {
            Ok(handle) => *lock(&WAITER) = Some(handle),
            Err(_) => {
                // Without the thread nothing takes the presented serials: present as
                // the device without present wait does, and keep the guest's own clock.
                log_warn!("vulkan: the present wait thread would not start");
                self.present_wait = false;
                SHARED.wait_state().display.nominal_ns = 0.0;
            }
        }
    }

    fn destroy_overlay(&mut self) {
        let device = self.device().clone();
        let o = &mut self.overlay;
        // SAFETY: the overlay's objects; the callers waited for the frames that read them.
        unsafe {
            if o.view != vk::ImageView::null() {
                device.destroy_image_view(o.view, None);
            }
            if o.image != vk::Image::null() {
                device.destroy_image(o.image, None);
            }
            if o.image_memory != vk::DeviceMemory::null() {
                device.free_memory(o.image_memory, None);
            }
            if o.staging != vk::Buffer::null() {
                device.destroy_buffer(o.staging, None);
            }
            if o.staging_memory != vk::DeviceMemory::null() {
                device.free_memory(o.staging_memory, None);
            }
        }
        o.image = vk::Image::null();
        o.image_memory = vk::DeviceMemory::null();
        o.view = vk::ImageView::null();
        o.staging = vk::Buffer::null();
        o.staging_memory = vk::DeviceMemory::null();
        o.mapped = std::ptr::null_mut();
        o.width = 0;
        o.height = 0;
        o.ever_uploaded = false;
    }

    fn create_overlay(&mut self, width: u32, height: u32) -> bool {
        let device = self.device().clone();
        // SAFETY (the function): objects made on the window thread's device.
        let memory = unsafe { self.instance().get_physical_device_memory_properties(self.physical) };
        let o = &mut self.overlay;
        let image = vk::ImageCreateInfo {
            image_type: vk::ImageType::TYPE_2D,
            format: vk::Format::B8G8R8A8_UNORM,
            extent: vk::Extent3D { width, height, depth: 1 },
            mip_levels: 1,
            array_layers: 1,
            samples: vk::SampleCountFlags::TYPE_1,
            tiling: vk::ImageTiling::OPTIMAL,
            usage: vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED,
            initial_layout: vk::ImageLayout::UNDEFINED,
            ..Default::default()
        };
        match unsafe { device.create_image(&image, None) } {
            Ok(made) => o.image = made,
            Err(error) => return check(error, c"the screens' image"),
        }
        let needs = unsafe { device.get_image_memory_requirements(o.image) };
        let mut allocate = vk::MemoryAllocateInfo {
            allocation_size: needs.size,
            memory_type_index: util::find_memory(
                &memory,
                needs.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ),
            ..Default::default()
        };
        if allocate.memory_type_index == u32::MAX {
            return false;
        }
        match unsafe { device.allocate_memory(&allocate, None) } {
            Ok(made) => o.image_memory = made,
            Err(error) => return check(error, c"the sign-in screen's memory"),
        }
        if !check(
            result_of(unsafe { device.bind_image_memory(o.image, o.image_memory, 0) }),
            c"the sign-in screen's memory",
        ) {
            return false;
        }
        let view = vk::ImageViewCreateInfo {
            image: o.image,
            view_type: vk::ImageViewType::TYPE_2D,
            format: image.format,
            subresource_range: vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            },
            ..Default::default()
        };
        match unsafe { device.create_image_view(&view, None) } {
            Ok(made) => o.view = made,
            Err(error) => return check(error, c"the screens' view"),
        }

        let buffer = vk::BufferCreateInfo {
            size: u64::from(width) * u64::from(height) * 4,
            usage: vk::BufferUsageFlags::TRANSFER_SRC,
            ..Default::default()
        };
        match unsafe { device.create_buffer(&buffer, None) } {
            Ok(made) => o.staging = made,
            Err(error) => return check(error, c"the sign-in screen's buffer"),
        }
        let needs = unsafe { device.get_buffer_memory_requirements(o.staging) };
        allocate.allocation_size = needs.size;
        allocate.memory_type_index = util::find_memory(
            &memory,
            needs.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        );
        if allocate.memory_type_index == u32::MAX {
            return false;
        }
        match unsafe { device.allocate_memory(&allocate, None) } {
            Ok(made) => o.staging_memory = made,
            Err(error) => return check(error, c"the sign-in screen's buffer"),
        }
        if !check(
            result_of(unsafe { device.bind_buffer_memory(o.staging, o.staging_memory, 0) }),
            c"the sign-in screen's buffer",
        ) {
            return false;
        }
        match unsafe { device.map_memory(o.staging_memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty()) } {
            Ok(mapped) => o.mapped = mapped.cast(),
            Err(error) => return check(error, c"the sign-in screen's buffer"),
        }
        o.width = width;
        o.height = height;
        true
    }

    // The screen open over the middle of the swapchain image, which is in TRANSFER_DST
    // layout: over a blur of the game, faded in and out (backdrop.rs), or else copied
    // over as it is.
    fn draw_overlay(
        &mut self,
        frame: u32,
        target: vk::Image,
        shown: &Queued,
        frame_from: vk::Offset3D,
        frame_to: vk::Offset3D,
    ) {
        let commands = self.frames[frame as usize].commands;
        let height = self.extent.height;
        let picture = open_picture(height, &mut self.overlay.pictures);
        let instance = self.instance.clone().expect("the presenter's instance");
        let device = self.device().clone();
        // SAFETY: the window thread's device, made from this physical device.
        let layered = unsafe { self.backdrop.create(&instance, &device, self.physical, self.swapchain_format) };
        // The fade goes on after the screen shuts, with the picture it last had.
        let o = &mut self.overlay;
        let now = nanoseconds();
        let elapsed = now.wrapping_sub(o.drawn);
        o.drawn = now;
        o.fade = fade_step(o.fade, elapsed, layered, picture.is_some());
        if picture.is_none() && (o.fade <= 0.0 || !o.ever_uploaded) {
            return;
        }

        if let Some(which) = picture {
            let (width, height, version, outline) = {
                let image = &o.pictures[which];
                (image.width, image.height, image.version, image.outline)
            };
            if width > self.extent.width || height > self.extent.height {
                return;
            }
            let resized = width != o.width || height != o.height;
            if resized || o.shown != Some(which) || o.uploaded != version || !o.ever_uploaded || o.layered != layered {
                // The other frames' commands may still be reading the image and the
                // buffer; this happens at a press of a button, not per frame.
                Self::settle_others(&device, &self.frames, frame);
                if resized {
                    self.destroy_overlay();
                    if !self.create_overlay(width, height) {
                        self.destroy_overlay();
                        return;
                    }
                }
                let o = &mut self.overlay;
                let pixels = &o.pictures[which].pixels;
                debug_assert!(pixels.len() as u64 <= u64::from(width) * u64::from(height) * 4);
                // SAFETY: the staging buffer is mapped, width * height * 4 bytes, and a
                // screen's picture is that many (screen.rs, Sheet::new); no frame reads
                // the buffer now (settle_others).
                unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), o.mapped, pixels.len()) };
                // SAFETY: recording into the frame's command buffer, on the overlay's image.
                unsafe {
                    util::barrier(
                        &device,
                        commands,
                        o.image,
                        vk::ImageAspectFlags::COLOR,
                        vk::ImageLayout::UNDEFINED,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        vk::AccessFlags::empty(),
                        vk::AccessFlags::TRANSFER_WRITE,
                        vk::PipelineStageFlags::TRANSFER,
                        vk::PipelineStageFlags::TRANSFER,
                    );
                    let copy = [vk::BufferImageCopy {
                        image_subresource: vk::ImageSubresourceLayers {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            mip_level: 0,
                            base_array_layer: 0,
                            layer_count: 1,
                        },
                        image_extent: vk::Extent3D { width, height, depth: 1 },
                        ..Default::default()
                    }];
                    device.cmd_copy_buffer_to_image(
                        commands,
                        o.staging,
                        o.image,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        &copy,
                    );
                    util::barrier(
                        &device,
                        commands,
                        o.image,
                        vk::ImageAspectFlags::COLOR,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        if layered {
                            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
                        } else {
                            vk::ImageLayout::TRANSFER_SRC_OPTIMAL
                        },
                        vk::AccessFlags::TRANSFER_WRITE,
                        if layered { vk::AccessFlags::SHADER_READ } else { vk::AccessFlags::TRANSFER_READ },
                        vk::PipelineStageFlags::TRANSFER,
                        if layered {
                            vk::PipelineStageFlags::FRAGMENT_SHADER
                        } else {
                            vk::PipelineStageFlags::TRANSFER
                        },
                    );
                }
                o.uploaded = version;
                o.shown = Some(which);
                o.outline = outline;
                o.layered = layered;
                o.ever_uploaded = true;
            }
        }
        let o = &mut self.overlay;
        let vk::Offset2D { x: left, y: top } = centred(self.extent, o.width, o.height);

        // A fade out with no picture any more can only go on as it was laid out.
        if o.layered != layered {
            return;
        }
        if shown.image != vk::Image::null() {
            o.frame_from = frame_from;
            o.frame_to = frame_to;
        }

        if layered {
            let draw = backdrop::Draw {
                commands,
                slot: frame,
                target,
                extent: self.extent,
                frame: shown.image,
                frame_width: shown.width,
                frame_height: shown.height,
                frame_from: vk::Offset2D { x: o.frame_from.x, y: o.frame_from.y },
                frame_to: vk::Offset2D { x: o.frame_to.x, y: o.frame_to.y },
                picture: o.view,
                picture_from: vk::Offset2D { x: left, y: top },
                picture_to: vk::Offset2D { x: left.wrapping_add(o.width as i32), y: top.wrapping_add(o.height as i32) },
                outline: o.outline,
                // Eased, so it settles rather than stops.
                shown: eased(o.fade),
            };
            let frames = self.frames;
            let mut settle = || Self::settle_others(&device, &frames, frame);
            // SAFETY: the frame's command buffer is recording on the backdrop's device;
            // the target is TRANSFER_DST, the frame TRANSFER_SRC, the picture
            // SHADER_READ_ONLY (uploaded above with the backdrop on).
            unsafe { self.backdrop.record(&draw, &mut settle) };
            return;
        }

        let mut blit = vk::ImageBlit {
            src_subresource: vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            },
            ..Default::default()
        };
        blit.src_offsets[1] = vk::Offset3D { x: o.width as i32, y: o.height as i32, z: 1 };
        blit.dst_subresource = blit.src_subresource;
        blit.dst_offsets[0] = vk::Offset3D { x: left, y: top, z: 0 };
        blit.dst_offsets[1] =
            vk::Offset3D { x: left.wrapping_add(o.width as i32), y: top.wrapping_add(o.height as i32), z: 1 };
        // SAFETY: recording into the frame's command buffer: the swapchain image and the
        // overlay's image in TRANSFER_SRC.
        unsafe {
            // The frame's blit wrote where this one writes.
            util::barrier(
                &device,
                commands,
                target,
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::AccessFlags::TRANSFER_WRITE,
                vk::AccessFlags::TRANSFER_WRITE,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::TRANSFER,
            );
            device.cmd_blit_image(
                commands,
                o.image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                target,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[blit],
                vk::Filter::NEAREST,
            );
        }
    }

    fn teardown(mut self) {
        if let Some(device) = self.device.clone() {
            // SAFETY: the window thread has stopped and the renderer has torn down what it
            // built on the device: everything here is idle once the device is.
            unsafe {
                let _ = device.device_wait_idle();
                self.backdrop.destroy();
                self.destroy_overlay();
                for frame in &self.frames {
                    if frame.acquired != vk::Semaphore::null() {
                        device.destroy_semaphore(frame.acquired, None);
                    }
                    if frame.rendered != vk::Semaphore::null() {
                        device.destroy_semaphore(frame.rendered, None);
                    }
                    if frame.in_flight != vk::Fence::null() {
                        device.destroy_fence(frame.in_flight, None);
                    }
                }
                if self.pool != vk::CommandPool::null() {
                    device.destroy_command_pool(self.pool, None);
                }
                if self.swapchain != vk::SwapchainKHR::null() {
                    self.swapchain_fn().destroy_swapchain(self.swapchain, None);
                }
                device.destroy_device(None);
            }
            self.pool = vk::CommandPool::null();
            self.swapchain = vk::SwapchainKHR::null();
            self.device = None;
            self.frames = [Frame::default(); FRAMES_IN_FLIGHT as usize];
        }
        // SAFETY: the instance's children are gone; the window is the window thread's,
        // which has stopped.
        unsafe {
            if self.surface != vk::SurfaceKHR::null() {
                self.surface_fn().destroy_surface(self.surface, None);
            }
            if self.messenger != vk::DebugUtilsMessengerEXT::null() {
                log_info!("vulkan: %llu validation messages", VALIDATION_MESSAGES.load(Ordering::SeqCst));
                if let Some(debug_utils) = &self.debug_utils {
                    debug_utils.destroy_debug_utils_messenger(self.messenger, None);
                }
                self.messenger = vk::DebugUtilsMessengerEXT::null();
            }
            if let Some(instance) = self.instance.take() {
                instance.destroy_instance(None);
            }
            if !self.window.is_null() {
                sdl::SDL_DestroyWindow(self.window);
            }
        }
        self.surface = vk::SurfaceKHR::null();
        self.window = std::ptr::null_mut();
    }

    fn bring(&mut self) -> bool {
        // SAFETY (the function): SDL's video, on the window thread, which pumps its events.
        if !unsafe { sdl::SDL_InitSubSystem(sdl::SDL_INIT_VIDEO) } {
            log_warn!("vulkan: SDL video would not start (%s)", sdl_error());
            return false;
        }
        // Resizable, and fullscreen from the start under MW2_FULLSCREEN=1 -- borderless at
        // the desktop's resolution, which SDL gives a window asked for fullscreen without
        // a mode of its own. F8 toggles it.
        self.fullscreen = env::flag(c"MW2_FULLSCREEN");
        self.stretch = env::flag(c"MW2_STRETCH");
        // Hidden until show_window. As tall as the console's frame, and as wide as the
        // title's frame is for that (render_size.rs).
        let frame = render_size::frame();
        let mut flags = sdl::SDL_WINDOW_VULKAN | sdl::SDL_WINDOW_RESIZABLE | sdl::SDL_WINDOW_HIDDEN;
        if self.fullscreen {
            flags |= sdl::SDL_WINDOW_FULLSCREEN;
        }
        self.window = unsafe {
            sdl::SDL_CreateWindow(
                c"Modern Warfare 2".as_ptr(),
                (HEIGHT.wrapping_mul(frame.width) / frame.height) as i32,
                HEIGHT as i32,
                flags,
            )
        };
        if self.window.is_null() {
            log_warn!("vulkan: no window (%s)", sdl_error());
            return false;
        }
        if !self.create_instance() {
            return false;
        }
        let mut surface: sdl_vulkan::VkSurfaceKHR = std::ptr::null_mut();
        let instance = self.instance().handle().as_raw() as usize as sdl_vulkan::VkInstance;
        if !unsafe { sdl_vulkan::SDL_Vulkan_CreateSurface(self.window, instance, std::ptr::null(), &mut surface) } {
            log_warn!("vulkan: no surface (%s)", sdl_error());
            return false;
        }
        self.surface = vk::SurfaceKHR::from_raw(surface as usize as u64);
        if !self.pick_device() || !self.create_device() || !self.create_swapchain() || !self.create_frames() {
            return false;
        }
        // Everything else that touches the GPU -- the texture cache, the renderer --
        // allocates against this device rather than one of its own, because an image
        // cannot be shared between two.
        bridge::vulkan::pipeline_initialise(
            self.device().handle().as_raw() as *mut c_void,
            self.physical.as_raw() as *mut c_void,
            self.queue.as_raw() as *mut c_void,
            self.queue_family,
            self.instance().handle().as_raw() as *mut c_void,
        );
        true
    }

    fn pump_events(&mut self, flow: &mut Flow) {
        // SAFETY: SDL's event queue, pumped on the thread that made the window; an
        // event's union is read as the member its type names.
        let mut event: sdl::SDL_Event = unsafe { std::mem::zeroed() };
        while unsafe { sdl::SDL_PollEvent(&mut event) } {
            let kind = unsafe { event.r#type };
            let key = (kind == sdl::SDL_EVENT_KEY_DOWN.0 && !unsafe { event.key.repeat })
                .then_some(unsafe { event.key.scancode });
            // Not `running = false`: that stops the pump, and with nothing pumping, the
            // window stops repainting and SDL stops seeing the keyboard and the pad,
            // which looks like a freeze while the guest runs on headless forever. Ending
            // the process is what closing the window means. The shutdown joins this
            // thread, so it runs on one of its own and this loop keeps the window alive
            // until it lands.
            if kind == sdl::SDL_EVENT_QUIT.0 || kind == sdl::SDL_EVENT_WINDOW_CLOSE_REQUESTED.0 {
                bridge::request_exit(c"the window was closed");
            } else if key.is_some_and(|key| self.diagnostic_key(key)) {
                // F10, F11, F9, F5 and F7 are diagnostics (diagnostic_key).
            }
            // F6 opens the friends screen, as the lobby's "Invite friends" does.
            else if key == Some(sdl::SDL_SCANCODE_F6) {
                friends::ask();
            }
            // F8 toggles fullscreen. The size change that follows arrives as an event
            // like any other resize.
            else if key == Some(sdl::SDL_SCANCODE_F8) {
                self.fullscreen = !self.fullscreen;
                if !unsafe { sdl::SDL_SetWindowFullscreen(self.window, self.fullscreen) } {
                    log_warn!("vulkan: fullscreen would not toggle (%s)", sdl_error());
                    self.fullscreen = !self.fullscreen;
                }
            }
            // The window's size in pixels changed -- dragged, maximised, fullscreen --
            // and the swapchain has to be its size.
            else if kind == sdl::SDL_EVENT_WINDOW_PIXEL_SIZE_CHANGED.0 || kind == sdl::SDL_EVENT_WINDOW_RESIZED.0 {
                flow.remake = true;
            }
        }
    }

    // A diagnostics key, handled: true when `key` is one.
    #[cfg(feature = "diagnostics")]
    fn diagnostic_key(&mut self, key: sdl::SDL_Scancode) -> bool {
        use super::capture;
        use crate::runtime::console;
        // F10 captures the next frames that draw the world. A defect that only appears
        // in motion has to be caught by somebody watching for it; no time chosen in
        // advance will do.
        if key == sdl::SDL_SCANCODE_F10 {
            capture::request_now(4);
        }
        // F11 captures every world frame until pressed again, for flicker: which frame
        // goes wrong is not known in advance. Not F12, which is RenderDoc's own key.
        else if key == sdl::SDL_SCANCODE_F11 {
            capture::toggle_stream();
        }
        // F9 asks the title where the player stands, so a spot seen in play can be given
        // back to setviewpos. The answer is printed under MW2_ENGINE_LOG=1.
        else if key == sdl::SDL_SCANCODE_F9 {
            console::run_now(c"viewpos");
            log_info!("console: F9 -- viewpos asked for");
        }
        // F5 runs the console command MW2_F5 names: `noclip`, say, once a level's
        // opening is behind.
        else if key == sdl::SDL_SCANCODE_F5
            && let Some(command) = env::text(c"MW2_F5")
        {
            console::run_now(command);
            log_info!("console: F5 -- %s", command);
        }
        // F7: "I saw a flash just now" -- a mark in the log, and the frames before it
        // written out when MW2_FLASH_FRAMES keeps them. Under MW2_STUTTERS it marks a
        // hitch instead.
        else if key == sdl::SDL_SCANCODE_F7 {
            use crate::runtime::{pacing_trace, stutters};
            if stutters::on() {
                stutters::mark(pacing_trace::microseconds(clock::nanoseconds()), c"F7");
            } else {
                bridge::flash_seen(c"F7");
            }
        } else {
            return false;
        }
        true
    }

    #[cfg(not(feature = "diagnostics"))]
    fn diagnostic_key(&mut self, _key: sdl::SDL_Scancode) -> bool {
        false
    }

    fn run(&mut self) {
        let mut flow = Flow { present_wait: self.present_wait, ..Flow::default() };
        while SHARED.running.load(Ordering::Relaxed) {
            // In front, with the keyboard's focus: a window shown a moment after its
            // program started may otherwise open behind another.
            if !self.shown && SHARED.show_asked.load(Ordering::Acquire) {
                // SAFETY: the window thread's window.
                unsafe {
                    sdl::SDL_ShowWindow(self.window);
                    sdl::SDL_RaiseWindow(self.window);
                }
                self.shown = true;
            }
            self.pump_events(&mut flow);

            // A frame to show, or 16 ms to go round the events again. Until the renderer
            // has finished one, the window shows its own colour.
            let queued = {
                let mut queue = SHARED.queue();
                queue.wake_sleepers += 1;
                let (mut queue, _) = SHARED
                    .wake
                    .wait_timeout_while(queue, Duration::from_millis(16), |queue| {
                        queue.frames.is_empty() && SHARED.running.load(Ordering::SeqCst)
                    })
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                queue.wake_sleepers -= 1;
                !queue.frames.is_empty()
            };
            if !SHARED.running.load(Ordering::SeqCst) {
                break;
            }
            // Nothing is presented to a hidden window, where a present can wait for a
            // frame the compositor never asks for.
            if !self.shown {
                continue;
            }
            if queued {
                draw_one_frame(&mut flow, self, &SHARED, false, console_rate());
            } else if SHARED.presented.load(Ordering::Relaxed) == 0 {
                draw_one_frame(&mut flow, self, &SHARED, true, console_rate());
            }
        }
        log_info!(
            "vulkan: presenter stopped after %llu frames, %llu swapchains",
            SHARED.presented.load(Ordering::SeqCst),
            self.swapchains_made
        );
    }
}

impl Steps for Presenter {
    // Only the presenter's own command buffers touch the swapchain images, so waiting
    // for its two fences is enough to replace it; the renderer's queue work carries on.
    // A window without a size keeps the old swapchain, which stays unusable, and every
    // frame until it has one is skipped.
    fn remake_swapchain(&mut self) -> bool {
        let fences = self.frames.map(|frame| frame.in_flight);
        // SAFETY: the presenter's own fences.
        let waited = unsafe { self.device().wait_for_fences(&fences, true, u64::MAX) };
        super::failed(result_of(waited), c"the presenter's fences before a resize");
        self.create_swapchain()
    }

    fn has_swapchain(&self) -> bool {
        self.swapchain != vk::SwapchainKHR::null()
    }

    fn wait_frame(&mut self, frame: u32) -> bool {
        let fence = [self.frames[frame as usize].in_flight];
        // SAFETY: the frame's own fence.
        let waited = unsafe { self.device().wait_for_fences(&fence, true, u64::MAX) };
        super::failed(result_of(waited), c"the presenter's fence")
    }

    fn acquire(&mut self, frame: u32) -> (vk::Result, u32) {
        let mut image_index = 0;
        // SAFETY: the live swapchain, and the frame's semaphore, which nothing waits on.
        let result = unsafe {
            (self.swapchain_fn().fp().acquire_next_image_khr)(
                self.device().handle(),
                self.swapchain,
                u64::MAX,
                self.frames[frame as usize].acquired,
                vk::Fence::null(),
                &mut image_index,
            )
        };
        (result, image_index)
    }

    fn record(&mut self, frame: u32, image_index: u32, shown: &Queued) {
        let device = self.device().clone();
        let current = self.frames[frame as usize];
        let commands = current.commands;
        let image = self.images[image_index as usize];
        let mut frame_from = vk::Offset3D::default();
        let mut frame_to = vk::Offset3D::default();
        // SAFETY: the frame's fence was waited for, so its command buffer is free; the
        // swapchain image is acquired, and the renderer's frame is in TRANSFER_SRC
        // layout and stays its until this copy is submitted (wait_until_taken).
        unsafe {
            let _ = device.reset_fences(&[current.in_flight]);
            let _ = device.reset_command_buffer(commands, vk::CommandBufferResetFlags::empty());
            let begin = vk::CommandBufferBeginInfo {
                flags: vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT,
                ..Default::default()
            };
            let _ = device.begin_command_buffer(commands, &begin);

            // At the transfer stage, which the acquire's semaphore holds back, so the
            // transition does not run before the image is ours.
            util::barrier(
                &device,
                commands,
                image,
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::AccessFlags::empty(),
                vk::AccessFlags::TRANSFER_WRITE,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::TRANSFER,
            );
            let range = [vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            }];

            if shown.image != vk::Image::null() && shown.width != 0 && shown.height != 0 {
                // The renderer's frame, scaled to the window at its own proportions. The
                // bars are cleared first; the clear covers the whole image and the blit
                // paints over the middle, which is cheaper than working out the bars.
                let mut blit = vk::ImageBlit {
                    src_subresource: vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    },
                    ..Default::default()
                };
                blit.src_offsets[1] = vk::Offset3D { x: shown.width as i32, y: shown.height as i32, z: 1 };
                blit.dst_subresource = blit.src_subresource;
                blit.dst_offsets = letterbox(self.stretch, self.extent, shown.width, shown.height);
                frame_from = blit.dst_offsets[0];
                frame_to = blit.dst_offsets[1];
                let bars = blit.dst_offsets[0].x != 0 || blit.dst_offsets[0].y != 0;
                if bars {
                    let black = vk::ClearColorValue::default();
                    device.cmd_clear_color_image(
                        commands,
                        image,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        &black,
                        &range,
                    );
                    util::barrier(
                        &device,
                        commands,
                        image,
                        vk::ImageAspectFlags::COLOR,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        vk::AccessFlags::TRANSFER_WRITE,
                        vk::AccessFlags::TRANSFER_WRITE,
                        vk::PipelineStageFlags::TRANSFER,
                        vk::PipelineStageFlags::TRANSFER,
                    );
                }
                device.cmd_blit_image(
                    commands,
                    shown.image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[blit],
                    vk::Filter::LINEAR,
                );
            } else {
                // A recognisably "nothing rendered yet" colour rather than black, so a
                // blank window is distinguishable from a dead one.
                let colour = vk::ClearColorValue { float32: [0.06, 0.07, 0.10, 1.0] };
                device.cmd_clear_color_image(commands, image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &colour, &range);
            }
        }
        self.draw_overlay(frame, image, shown, frame_from, frame_to);
        // SAFETY: as above.
        unsafe {
            util::barrier(
                &device,
                commands,
                image,
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::PRESENT_SRC_KHR,
                vk::AccessFlags::TRANSFER_WRITE,
                vk::AccessFlags::empty(),
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
            );
            let _ = device.end_command_buffer(commands);
        }
    }

    fn submit(&mut self, frame: u32) {
        let current = &self.frames[frame as usize];
        let wait = [vk::PipelineStageFlags::TRANSFER];
        let submit = vk::SubmitInfo {
            wait_semaphore_count: 1,
            p_wait_semaphores: &current.acquired,
            p_wait_dst_stage_mask: wait.as_ptr(),
            command_buffer_count: 1,
            p_command_buffers: &current.commands,
            signal_semaphore_count: 1,
            p_signal_semaphores: &current.rendered,
            ..Default::default()
        };
        // The GPU thread submits to this queue too, and Vulkan requires a queue to be
        // externally synchronised for a present as for a submission. Two holds rather
        // than one, so a submission of the renderer's can go between them.
        let _queue = QueueLock::hold();
        // SAFETY: the frame's recorded commands and semaphores, under the queue's lock.
        let result =
            unsafe { gpu_time::submit(self.device().fp_v1_0().queue_submit, self.queue, &submit, current.in_flight) };
        super::failed(result, c"the presenter's submission");
    }

    fn present(&mut self, frame: u32, image_index: u32, id: Option<u64>) -> vk::Result {
        let ids = [id.unwrap_or(0)];
        let present_id = vk::PresentIdKHR { swapchain_count: 1, p_present_ids: ids.as_ptr(), ..Default::default() };
        let present = vk::PresentInfoKHR {
            p_next: if id.is_some() { (&raw const present_id).cast() } else { std::ptr::null() },
            wait_semaphore_count: 1,
            p_wait_semaphores: &self.frames[frame as usize].rendered,
            swapchain_count: 1,
            p_swapchains: &self.swapchain,
            p_image_indices: &image_index,
            ..Default::default()
        };
        let _queue = QueueLock::hold();
        // SAFETY: the acquired image, after the semaphore its commands signal, under the
        // queue's lock.
        unsafe { (self.swapchain_fn().fp().queue_present_khr)(self.queue, &present) }
    }

    fn present_failed(&mut self, result: vk::Result) {
        super::failed(result, c"present");
    }

    #[cfg(all(feature = "diagnostics", not(mw2_gpu_rig)))]
    fn shown(&mut self, serial: u64) {
        use crate::runtime::{pacing_trace, stutters};
        if stutters::on() {
            stutters::shown(pacing_trace::microseconds(clock::nanoseconds()), serial);
        }
    }

    #[cfg(mw2_gpu_rig)]
    fn shown(&mut self, serial: u64) {
        rig::shown(serial);
    }

    #[cfg(all(not(feature = "diagnostics"), not(mw2_gpu_rig)))]
    fn shown(&mut self, _serial: u64) {}
}

// Waits on each present in turn until it is on the screen. Its own thread, as DXVK does
// it: the window thread cannot wait on the display without falling behind the frames it
// has to show.
fn waiter(wait: ash::khr::present_wait::Device) {
    crash::register_thread(c"vulkan present wait");
    while SHARED.running.load(Ordering::Relaxed) {
        let id = {
            let mut state = SHARED.wait_state();
            state.sleepers += 1;
            let (mut state, _) = SHARED
                .wait_wake
                .wait_timeout_while(state, Duration::from_millis(50), |state| {
                    state.waiting.is_empty() && SHARED.running.load(Ordering::SeqCst)
                })
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.sleepers -= 1;
            match state.waiting.front() {
                Some(&id) => id,
                None => continue,
            }
        };
        let result = {
            let life = SHARED.swapchain_life.read().unwrap_or_else(|poisoned| poisoned.into_inner());
            if *life != vk::SwapchainKHR::null() {
                // SAFETY: the swapchain, kept alive by `swapchain_life` held shared.
                unsafe { (wait.fp().wait_for_present_khr)(wait.device(), *life, id, 20_000_000) } // 20 ms
            } else {
                vk::Result::ERROR_OUT_OF_DATE_KHR
            }
        };
        if result == vk::Result::TIMEOUT {
            continue; // still waiting; lets a resize in
        }
        let at = nanoseconds();
        let mut state = SHARED.wait_state();
        // A resize cleared the list, and this id went with the old swapchain.
        if state.waiting.front() != Some(&id) {
            continue;
        }
        state.waiting.pop_front();
        if result == vk::Result::SUCCESS {
            let queued = SHARED.last_queued.load(Ordering::Relaxed);
            let published = state.display.saw_displayed(id, at, queued, console_rate(), &mut stutter);
            if let Some((period, at)) = published {
                SHARED.guest_period_ns.store(period, Ordering::Relaxed);
                SHARED.guest_period_at.store(at, Ordering::Relaxed);
            }
        } else {
            state.display.last_shown_ns = -1;
        }
    }
}

// stutters::Displayed: a frame `extra` blanks late, `gap_ms` after the one before.
#[cfg(feature = "diagnostics")]
fn stutter(serial: u64, extra: u32, gap_ms: f64) {
    use crate::runtime::{pacing_trace, stutters};
    if stutters::on() {
        stutters::displayed(pacing_trace::microseconds(clock::nanoseconds()), serial, extra, gap_ms);
    }
}

#[cfg(not(feature = "diagnostics"))]
fn stutter(_serial: u64, _extra: u32, _gap_ms: f64) {}

// ---- what the rest of the runtime calls -----------------------------------------------

/// The window thread started, and the device up: false headless (MW2_WINDOW=0, or no
/// device or window system).
pub fn start() -> bool {
    // Every build opens it; MW2_WINDOW=0 keeps it shut, for a headless measurement run.
    let window = env::text(c"MW2_WINDOW").is_none() || env::flag(c"MW2_WINDOW");
    if !window {
        return false;
    }
    SHARED.running.store(true, Ordering::SeqCst);
    let spawned = std::thread::Builder::new()
        .name("vulkan presenter".into())
        .stack_size(platform::default_thread_stack())
        .spawn(|| {
            crash::register_thread(c"vulkan presenter");
            let mut presenter = Presenter::new();
            if !presenter.bring() {
                log_warn!("vulkan: presenting disabled, continuing headless");
                SHARED.running.store(false, Ordering::SeqCst);
                SHARED.ready.store(true, Ordering::SeqCst);
                return presenter;
            }
            presenter.follow_the_display();
            SHARED.ready.store(true, Ordering::SeqCst);
            log_info!("vulkan: presenter running");
            presenter.run();
            presenter
        });
    match spawned {
        Ok(handle) => *lock(&WORKER) = Some(handle),
        Err(_) => {
            log_warn!("vulkan: the presenter thread would not start");
            SHARED.running.store(false, Ordering::SeqCst);
            return false;
        }
    }
    while !SHARED.ready.load(Ordering::Acquire) {
        std::thread::yield_now();
    }
    SHARED.running.load(Ordering::SeqCst)
}

/// The window is shown from here: it is made hidden.
pub fn show_window() {
    SHARED.show_asked.store(true, Ordering::Release);
    let queue = SHARED.queue();
    if queue.wake_sleepers != 0 {
        SHARED.wake.notify_one();
    }
}

/// A frame the renderer has finished: a VkImage in TRANSFER_SRC layout, `serial` its
/// number.
pub fn show_image(image: vk::Image, width: u32, height: u32, serial: u64) {
    SHARED.show_image(image, width, height, serial);
}

/// Returns once the window has copied frame `serial` out (or let it go), at once
/// without a window.
pub fn wait_until_taken(serial: u64) {
    SHARED.wait_until_taken(serial);
}

/// The guest's blank period in nanoseconds, following the display the window is on; 0
/// when there is none to follow.
pub fn guest_blank_period() -> u64 {
    blank_period(
        SHARED.guest_period_ns.load(Ordering::Relaxed),
        SHARED.guest_period_at.load(Ordering::Relaxed),
        nanoseconds(),
    )
}

/// The window thread has to stop before anything that owns an image it might be
/// showing is torn down, but the device has to outlive that teardown -- so stopping and
/// destroying are two steps rather than one.
pub fn stop_presenting() {
    let Some(worker) = lock(&WORKER).take() else { return };
    SHARED.running.store(false, Ordering::SeqCst);
    {
        let _queue = SHARED.queue();
        SHARED.wake.notify_all();
        SHARED.taken_wake.notify_all();
    }
    {
        let _wait = SHARED.wait_state();
        SHARED.wait_wake.notify_all();
    }
    match worker.join() {
        Ok(presenter) => *lock(&STOPPED) = Some(presenter),
        Err(_) => log_warn!("vulkan: the presenter thread ended abnormally"),
    }
    let waiter = lock(&WAITER).take();
    if let Some(waiter) = waiter {
        let _ = waiter.join();
        let wait = SHARED.wait_state();
        let d = &wait.display;
        log_info!(
            "vulkan: the display's blank measured %.1f us; frames behind the one on screen when it was shown: 0 x%llu, 1 x%llu, 2 x%llu, 3+ x%llu; the guest's blank ran at the display's for %llu, longer for %llu",
            d.period_ns / 1000.0,
            d.behind_count[0],
            d.behind_count[1],
            d.behind_count[2],
            d.behind_count[3],
            d.shown_on_time,
            d.stretched
        );
    }
}

pub fn stop() {
    stop_presenting();
    let presenter = lock(&STOPPED).take();
    if let Some(presenter) = presenter {
        presenter.teardown();
    }
}

pub fn running() -> bool {
    SHARED.running.load(Ordering::SeqCst)
}

pub fn presented_frames() -> u64 {
    SHARED.presented.load(Ordering::SeqCst)
}

// tools/gpu_diff.py --rig presenter: the window thread's own code -- create_swapchain,
// draw_one_frame with the Presenter's steps, record and draw_overlay -- run on the
// rig's stand-in Vulkan and SDL calls (tools/gpu_diff/presenter_stubs.cpp), which ash's
// tables are filled with here, against the C++ run on the same stand-ins. What the
// window reaches beyond Vulkan is the rig's too: its clock, the sign-in screen's
// picture, the backdrop (held against the C++ on its own, backdrop.rs) and the
// stutters report. Nothing here makes a device or a window.
#[cfg(mw2_gpu_rig)]
pub mod rig {
    use super::*;

    unsafe extern "C" {
        fn rig_now() -> i64;
        fn rig_picture(width: *mut u32, height: *mut u32, version: *mut u64, outline: *mut f32) -> bool;
        fn rig_backdrop_layered() -> bool;
        fn rig_backdrop_record(draw: *const RigBackdropDraw);
        fn rig_shown(serial: u64);
        fn rig_SDL_GetWindowSizeInPixels(window: *mut sdl::SDL_Window, width: *mut i32, height: *mut i32) -> bool;
    }
    unsafe extern "system" {
        fn rig_vkGetInstanceProcAddr(instance: vk::Instance, name: *const c_char) -> vk::PFN_vkVoidFunction;
    }

    pub(super) fn now() -> i64 {
        // SAFETY: the rig's clock.
        unsafe { rig_now() }
    }

    pub(super) fn picture(image: &mut Image) -> bool {
        // SAFETY: four places to write, on this stack.
        unsafe { rig_picture(&mut image.width, &mut image.height, &mut image.version, &mut image.outline) }
    }

    pub(super) fn window_size(window: *mut sdl::SDL_Window) -> (i32, i32) {
        let (mut width, mut height) = (0, 0);
        // SAFETY: two places to write, on this stack.
        unsafe { rig_SDL_GetWindowSizeInPixels(window, &mut width, &mut height) };
        (width, height)
    }

    pub(super) fn shown(serial: u64) {
        // SAFETY: the rig's count.
        unsafe { rig_shown(serial) }
    }

    /// The instance, device and their entry, every command one of the rig's stand-ins.
    pub fn tables() -> (ash::Entry, ash::Instance, ash::Device) {
        // SAFETY: the rig's loader, which answers by name and with null for the rest.
        unsafe {
            let entry = ash::Entry::from_static_fn(ash::StaticFn { get_instance_proc_addr: rig_vkGetInstanceProcAddr });
            let instance = ash::Instance::load(entry.static_fn(), vk::Instance::null());
            let device = ash::Device::load(instance.fp_v1_0(), vk::Device::null());
            (entry, instance, device)
        }
    }

    /// backdrop::Draw as the rig's C++ takes it.
    #[repr(C)]
    pub struct RigBackdropDraw {
        commands: u64,
        slot: u32,
        target: u64,
        extent: [u32; 2],
        frame: u64,
        frame_width: u32,
        frame_height: u32,
        frame_from: [i32; 2],
        frame_to: [i32; 2],
        picture: u64,
        picture_from: [i32; 2],
        picture_to: [i32; 2],
        outline: f32,
        shown: f32,
    }

    /// The backdrop as the window sees it in the rig: made or not as the case says, and
    /// each draw handed to the rig, as the C++ presenter's is.
    #[derive(Default)]
    pub struct Backdrop;

    impl Backdrop {
        pub unsafe fn create(
            &mut self,
            _instance: &ash::Instance,
            _device: &ash::Device,
            _physical: vk::PhysicalDevice,
            _format: vk::Format,
        ) -> bool {
            // SAFETY: the rig's case.
            unsafe { rig_backdrop_layered() }
        }

        pub unsafe fn record(&mut self, draw: &backdrop::Draw, _settle: &mut dyn FnMut()) {
            let d = RigBackdropDraw {
                commands: draw.commands.as_raw(),
                slot: draw.slot,
                target: draw.target.as_raw(),
                extent: [draw.extent.width, draw.extent.height],
                frame: draw.frame.as_raw(),
                frame_width: draw.frame_width,
                frame_height: draw.frame_height,
                frame_from: [draw.frame_from.x, draw.frame_from.y],
                frame_to: [draw.frame_to.x, draw.frame_to.y],
                picture: draw.picture.as_raw(),
                picture_from: [draw.picture_from.x, draw.picture_from.y],
                picture_to: [draw.picture_to.x, draw.picture_to.y],
                outline: draw.outline,
                shown: draw.shown,
            };
            // SAFETY: a draw on this stack.
            unsafe { rig_backdrop_record(&d) };
        }

        pub unsafe fn destroy(&mut self) {}
    }

    /// The window thread's state, on the rig's tables, and its frame loop's.
    pub struct RigPresenter {
        presenter: Presenter,
        flow: Flow,
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_rig_presenter_new() -> *mut RigPresenter {
        let (entry, instance, device) = tables();
        let mut presenter = Presenter::new();
        presenter.surface_fn = Some(ash::khr::surface::Instance::new(&entry, &instance));
        presenter.swapchain_fn = Some(ash::khr::swapchain::Device::new(&instance, &device));
        presenter.instance = Some(instance);
        presenter.device = Some(device);
        Box::into_raw(Box::new(RigPresenter { presenter, flow: Flow::default() }))
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_presenter_free(p: *mut RigPresenter) {
        drop(unsafe { Box::from_raw(p) });
    }

    // ---- create_swapchain

    /// create_swapchain over `old`, from a 1280x720 extent: whether it made one, and the
    /// extent it left.
    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_presenter_swapchain(
        p: *mut RigPresenter,
        old: u64,
        extent: *mut vk::Extent2D,
    ) -> bool {
        let p = &mut unsafe { &mut *p }.presenter;
        p.swapchain = vk::SwapchainKHR::from_raw(old);
        p.extent = vk::Extent2D { width: 1280, height: HEIGHT };
        let made = p.create_swapchain();
        unsafe { extent.write(p.extent) };
        p.swapchain = vk::SwapchainKHR::null();
        made
    }

    /// The present ids waited for and the display's last present, set and read.
    #[unsafe(no_mangle)]
    extern "C" fn mw2_rig_wait_fill(count: u32, last_shown_ns: i64) {
        let mut wait = SHARED.wait_state();
        wait.waiting = (1..=u64::from(count)).collect();
        wait.display.last_shown_ns = last_shown_ns;
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_wait_read(count: *mut u32, last_shown_ns: *mut i64) {
        let wait = SHARED.wait_state();
        unsafe {
            count.write(wait.waiting.len() as u32);
            last_shown_ns.write(wait.display.last_shown_ns);
        }
    }

    // ---- draw_overlay

    /// The overlay as after an upload of the sign-in screen's picture.
    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_overlay_reset(
        p: *mut RigPresenter,
        width: u32,
        height: u32,
        layered: bool,
        picture_width: u32,
        picture_height: u32,
        version: u64,
        outline: f32,
    ) {
        let p = &mut unsafe { &mut *p }.presenter;
        p.extent = vk::Extent2D { width, height };
        let o = &mut p.overlay;
        o.pictures[0].width = picture_width;
        o.pictures[0].height = picture_height;
        o.pictures[0].version = version;
        o.shown = Some(0);
        o.width = picture_width;
        o.height = picture_height;
        o.uploaded = version;
        o.outline = outline;
        o.ever_uploaded = true;
        o.layered = layered;
        o.fade = 0.0;
        o.frame_from = vk::Offset3D::default();
        o.frame_to = vk::Offset3D::default();
        o.drawn = 0;
    }

    /// draw_overlay into frame 0 over image 0x77 with this game frame (0: none) shown
    /// between `from` and `to`; returns the fade it left.
    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_overlay_step(
        p: *mut RigPresenter,
        frame: u64,
        frame_width: u32,
        frame_height: u32,
        from: *const [i32; 2],
        to: *const [i32; 2],
    ) -> f32 {
        let p = &mut unsafe { &mut *p }.presenter;
        let (from, to) = unsafe { (*from, *to) };
        let shown = Queued { image: vk::Image::from_raw(frame), width: frame_width, height: frame_height, serial: 1 };
        p.draw_overlay(
            0,
            vk::Image::from_raw(0x77),
            &shown,
            vk::Offset3D { x: from[0], y: from[1], z: 0 },
            vk::Offset3D { x: to[0], y: to[1], z: 1 },
        );
        p.overlay.fade
    }

    // ---- the frame loop and the frame queue, on the presenter's own shared state

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_flow_reset(p: *mut RigPresenter, present_wait: bool) {
        let p = unsafe { &mut *p };
        // Everything as new but how many times the log was told, which, like the C++'s,
        // counts for the whole run.
        SHARED.running.store(true, Ordering::SeqCst);
        SHARED.presented.store(0, Ordering::SeqCst);
        SHARED.last_queued.store(0, Ordering::SeqCst);
        *SHARED.queue() = FrameQueue::new();
        SHARED.wait_state().waiting.clear();
        p.flow = Flow { present_wait, ..Flow::default() };
        let w = &mut p.presenter;
        w.stretch = false;
        w.extent = vk::Extent2D { width: 1280, height: HEIGHT };
        w.swapchain = vk::SwapchainKHR::from_raw(0x5000);
        w.images = (0x5001..=0x5003).map(vk::Image::from_raw).collect();
        w.overlay.ever_uploaded = false;
        w.overlay.fade = 0.0;
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_rig_flow_running(running: bool) {
        SHARED.running.store(running, Ordering::SeqCst);
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_rig_flow_show_image(image: u64, width: u32, height: u32, serial: u64) {
        show_image(vk::Image::from_raw(image), width, height, serial);
    }

    /// wait_until_taken with a clock that only moves when nothing takes the frame: the
    /// wait times out exactly when its condition still holds. Returns whether it logged.
    #[unsafe(no_mangle)]
    extern "C" fn mw2_rig_flow_wait(serial: u64) -> bool {
        SHARED.wait_until_taken_with(serial, |mut queue, waiting| {
            let timed_out = waiting(&mut queue);
            (queue, timed_out)
        })
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_rig_flow_let_go() {
        SHARED.let_go();
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_flow_draw(p: *mut RigPresenter, idle: bool, console: bool) {
        let p = unsafe { &mut *p };
        draw_one_frame(&mut p.flow, &mut p.presenter, &SHARED, idle, console);
    }

    #[repr(C)]
    pub struct FlowState {
        remake: u32,
        frame_index: u32,
        presented: u64,
        taken: u64,
        last_queued: u64,
        queued: u32,
        serials: [u64; 32],
        waiting: u32,
        ids: [u64; 32],
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_flow_read(p: *const RigPresenter, out: *mut FlowState) {
        let p = unsafe { &*p };
        let queue = SHARED.queue();
        let wait = SHARED.wait_state();
        let mut state = FlowState {
            remake: p.flow.remake.into(),
            frame_index: p.flow.frame_index,
            presented: SHARED.presented.load(Ordering::SeqCst),
            taken: queue.taken,
            last_queued: SHARED.last_queued.load(Ordering::SeqCst),
            queued: queue.frames.len() as u32,
            serials: [0; 32],
            waiting: wait.waiting.len() as u32,
            ids: [0; 32],
        };
        for (slot, frame) in state.serials.iter_mut().zip(&queue.frames) {
            *slot = frame.serial;
        }
        for (slot, id) in state.ids.iter_mut().zip(&wait.waiting) {
            *slot = *id;
        }
        unsafe { out.write(state) };
    }
}
