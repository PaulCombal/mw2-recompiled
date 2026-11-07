// The command trace, MW2_DETERMINISTIC and the forced fallbacks
// (runtime/gpu/vulkan/trace.{h,cpp}), diagnostics builds only.
//
//   MW2_TRACE_COMMANDS=<file>[:<first frame>:<frames>]  every Vulkan call the renderer
//       makes that creates, destroys, records or submits, one normalised line each
//   MW2_DETERMINISTIC=1   none of the renderer's decisions taken by the clock
//   MW2_FORCE_* and the rest  paths a device or a run seldom takes, forced (forcing)
//
// The state, the line builder and the instruments are here; the line of each Vulkan
// call is device.rs's, which writes it when its Vk was made with the trace on. A handle
// the traced calls did not make is named by its role (register); one they made, by its
// kind and creation order, or as name_next says. docs/rendering.md (Checking) gives the
// line format, which is the C++'s byte for byte: the two renderers' traces of one capture
// are compared line by line.
#![deny(unsafe_op_in_unsafe_fn)]

use crate::env;
use crate::log::{log_info, log_warn};
use crate::runtime::clock;
use ash::vk;
use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::ffi::{CString, c_char, c_int};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufWriter, Write as _};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard};

static ON: AtomicBool = AtomicBool::new(false); // MW2_TRACE_COMMANDS names a file
static DETERMINISTIC: AtomicBool = AtomicBool::new(false); // MW2_DETERMINISTIC=1

#[inline]
pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

#[inline]
pub fn deterministic() -> bool {
    DETERMINISTIC.load(Ordering::Relaxed)
}

/// The forced fallbacks, read once at the renderer's initialise (start).
#[derive(Clone, Copy, Default, Debug)]
pub struct Forced {
    pub instance_depth_resolve: bool, // MW2_FORCE_INSTANCE_DEPTH_RESOLVE
    pub resolve_twin: bool,           // MW2_FORCE_RESOLVE_TWIN
    pub drawn_resolve_sets: u32,      // MW2_DRAWN_RESOLVE_SETS, 0: as built
    pub resolve_remake: u32,          // MW2_FORCE_RESOLVE_REMAKE, frames; 0: never
    pub no_multi_draw: bool,          // MW2_NO_MULTI_DRAW
    pub lone_uploads: bool,           // MW2_FORCE_LONE_UPLOADS
    pub staging_least_kb: u32,        // MW2_STAGING_LEAST_KB, 0: as built
    pub fake_device_lost: f64,        // MW2_FAKE_DEVICE_LOST, seconds; 0: never
}

static FORCED: Mutex<Forced> = Mutex::new(Forced {
    instance_depth_resolve: false,
    resolve_twin: false,
    drawn_resolve_sets: 0,
    resolve_remake: 0,
    no_multi_draw: false,
    lone_uploads: false,
    staging_least_kb: 0,
    fake_device_lost: 0.0,
});
static STARTED: AtomicU64 = AtomicU64::new(0);

pub fn forcing() -> Forced {
    *FORCED.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Counted where a forced path is taken, for the report.
pub struct ForcedCounts {
    pub twin_resolves: AtomicU64,
    pub remade_resolves: AtomicU64,
    pub lone_uploads: AtomicU64,
    pub lost_submissions: AtomicU64,
}

static COUNTS: ForcedCounts = ForcedCounts {
    twin_resolves: AtomicU64::new(0),
    remade_resolves: AtomicU64::new(0),
    lone_uploads: AtomicU64::new(0),
    lost_submissions: AtomicU64::new(0),
};

pub fn counts() -> &'static ForcedCounts {
    &COUNTS
}

/// Whether MW2_FAKE_DEVICE_LOST's time has come: every submission from then on fails
/// with VK_ERROR_DEVICE_LOST. Any thread.
pub fn device_lost_faked() -> bool {
    let after = forcing().fake_device_lost;
    if after <= 0.0 {
        return false;
    }
    clock::nanoseconds().wrapping_sub(STARTED.load(Ordering::Relaxed)) as f64 / 1e9 >= after
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Kind {
    Image,
    View,
    Buffer,
    Memory,
    Sampler,
    DescriptorPool,
    Set,
    Pipeline,
    Module,
    CommandPool,
    Command,
    Fence,
    QueryPool,
    SetLayout,
    PipelineLayout,
    Semaphore,
    Device,
    Physical,
    Queue,
    Cache,
}
const KINDS: usize = 20;
const KIND_NAMES: [&str; KINDS] = [
    "Image",
    "View",
    "Buffer",
    "Memory",
    "Sampler",
    "DescriptorPool",
    "Set",
    "Pipeline",
    "Module",
    "CommandPool",
    "Cmd",
    "Fence",
    "QueryPool",
    "SetLayout",
    "PipelineLayout",
    "Semaphore",
    "Device",
    "Physical",
    "Queue",
    "Cache",
];

struct Pending {
    position: u64,
    text: String,
    frame: i64, // a frame marker's frame, or -1
}

pub(super) struct State {
    file: Option<BufWriter<File>>,
    path: String,
    first: u64,
    frames: u64,    // the window, in renderer frames
    frame: u64,     // as the lines written so far have it
    generated: u64, // the last frame marker made
    seq: u64,
    written: u64,
    unregistered: u64,
    names: [HashMap<u64, String>; KINDS],
    ordinals: [u64; KINDS],
    next: [String; KINDS],
    position: Option<fn() -> u64>,
    recorder_running: bool,
    recorder_at: u64,
    pending: VecDeque<Pending>,
    finished: bool,
}

impl State {
    fn new() -> State {
        State {
            file: None,
            path: String::new(),
            first: 0,
            frames: u64::MAX,
            frame: 0,
            generated: 0,
            seq: 0,
            written: 0,
            unregistered: 0,
            names: std::array::from_fn(|_| HashMap::new()),
            ordinals: [0; KINDS],
            next: std::array::from_fn(|_| String::new()),
            position: None,
            recorder_running: false,
            recorder_at: 0,
            pending: VecDeque::new(),
            finished: false,
        }
    }

    // A line made now is written at a frame between the writer's and the last marker
    // made; whether that is outside the window for certain.
    fn outside(&self) -> bool {
        (self.frame >= self.first && self.frame - self.first >= self.frames) || self.generated < self.first
    }

    fn write(&mut self, text: &str, frame: i64) {
        self.seq += 1;
        if frame >= 0 {
            self.frame = frame as u64;
        }
        let Some(file) = self.file.as_mut() else { return };
        if text.is_empty() || self.frame < self.first || self.frame - self.first >= self.frames {
            return;
        }
        let _ = writeln!(file, "{} {}", self.seq, text);
        self.written += 1;
        // The run leaves by _exit: what is buffered at a frame's end is kept.
        if frame >= 0 {
            let _ = file.flush();
        }
    }

    // A line made on the recorder goes straight out: the recorder makes calls in the
    // order they were queued. One made anywhere else (the consumer) goes after the calls
    // queued before it, so it waits while the recorder has not made them.
    fn emit(&mut self, text: String, frame: i64) {
        let Some(position) = self.position.filter(|_| !RECORDER_THREAD.get() && self.recorder_running) else {
            self.write(&text, frame);
            return;
        };
        let at = position();
        if self.pending.is_empty() && self.recorder_at >= at {
            self.write(&text, frame);
            return;
        }
        self.pending.push_back(Pending { position: at, text, frame });
    }

    fn name_of(&self, kind: Kind, handle: u64) -> Option<&str> {
        self.names[kind as usize].get(&handle).map(String::as_str)
    }
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::new()));

thread_local! {
    static RECORDER_THREAD: Cell<bool> = const { Cell::new(false) };
}

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One line, built under the lock and emitted when it is dropped. A line outside the
/// window is counted, not written, and not formatted either; the names it makes and
/// drops, and the unnamed handles it counts, are kept all the same.
pub(super) struct Line {
    state: MutexGuard<'static, State>,
    pub(super) text: String,
    pub(super) skip: bool,
}

impl Drop for Line {
    fn drop(&mut self) {
        let text = if self.skip { String::new() } else { std::mem::take(&mut self.text) };
        self.state.emit(text, -1);
    }
}

/// printf's %08x of a float's bits.
#[inline]
pub(super) fn bits(value: f32) -> u32 {
    value.to_bits()
}

/// Appended to a line unless the line is skipped: write!'s arguments.
macro_rules! put {
    ($line:expr, $($format:tt)*) => {
        if !$line.skip {
            let _ = write!($line.text, $($format)*);
        }
    };
}
pub(super) use put;

impl Line {
    pub(super) fn new(command: &str) -> Line {
        let state = state();
        let skip = state.outside();
        Line { text: if skip { String::new() } else { command.to_owned() }, state, skip }
    }

    pub(super) fn u(&mut self, key: &str, value: u64) -> &mut Self {
        put!(self, " {key}={value}");
        self
    }
    pub(super) fn i(&mut self, key: &str, value: i64) -> &mut Self {
        put!(self, " {key}={value}");
        self
    }
    pub(super) fn x(&mut self, key: &str, value: u64) -> &mut Self {
        put!(self, " {key}=0x{value:x}");
        self
    }
    pub(super) fn f(&mut self, key: &str, value: f32) -> &mut Self {
        put!(self, " {key}={:08x}", bits(value));
        self
    }
    pub(super) fn raw(&mut self, chunk: &str) -> &mut Self {
        if !self.skip {
            self.text.push_str(chunk);
        }
        self
    }
    pub(super) fn key(&mut self, key: &str) -> &mut Self {
        put!(self, " {key}=");
        self
    }

    /// A handle by its name; "?" for one nobody named, counted.
    pub(super) fn name(&mut self, kind: Kind, handle: u64) {
        if handle == 0 {
            if !self.skip {
                self.text.push('0');
            }
            return;
        }
        if let Some(name) = self.state.names[kind as usize].get(&handle) {
            if !self.skip {
                self.text.push_str(name);
            }
            return;
        }
        if !self.skip {
            self.text.push('?');
        }
        self.state.unregistered += 1;
    }

    pub(super) fn h(&mut self, key: &str, kind: Kind, handle: u64) -> &mut Self {
        put!(self, " {key}=");
        self.name(kind, handle);
        self
    }

    /// # Safety
    /// `handles` is null or points at `count` handles.
    pub(super) unsafe fn hs<H: vk::Handle + Copy>(
        &mut self,
        key: &str,
        kind: Kind,
        handles: *const H,
        count: u32,
    ) -> &mut Self {
        put!(self, " {key}=[");
        for n in 0..count as usize {
            if n != 0 && !self.skip {
                self.text.push(',');
            }
            // SAFETY: the caller's.
            let raw = if handles.is_null() { 0 } else { unsafe { *handles.add(n) }.as_raw() };
            self.name(kind, raw);
        }
        self.raw("]")
    }

    /// A handle just made: named name_next's way or by its ordinal.
    pub(super) fn made(&mut self, key: &str, kind: Kind, handle: u64) -> &mut Self {
        if handle != 0 {
            self.register_made(kind, handle);
        }
        self.h(key, kind, handle)
    }

    pub(super) fn register_made(&mut self, kind: Kind, handle: u64) -> String {
        let k = kind as usize;
        let s = &mut *self.state;
        let name = if !s.next[k].is_empty() {
            std::mem::take(&mut s.next[k])
        } else {
            let name = format!("{}{}", KIND_NAMES[k], s.ordinals[k]);
            s.ordinals[k] += 1;
            name
        };
        s.names[k].insert(handle, name.clone());
        name
    }

    /// A set or command buffer of an allocation: always by its ordinal.
    pub(super) fn allocated(&mut self, kind: Kind, handle: u64) -> String {
        let k = kind as usize;
        let s = &mut *self.state;
        let name = format!("{}{}", KIND_NAMES[k], s.ordinals[k]);
        s.ordinals[k] += 1;
        s.names[k].insert(handle, name.clone());
        name
    }

    pub(super) fn gone(&mut self, key: &str, kind: Kind, handle: u64) -> &mut Self {
        self.h(key, kind, handle);
        if handle != 0 {
            self.state.names[kind as usize].remove(&handle);
        }
        self
    }

    pub(super) fn forget(&mut self, kind: Kind, handle: u64) {
        self.state.names[kind as usize].remove(&handle);
    }

    pub(super) fn is_named(&self, kind: Kind, handle: u64) -> bool {
        self.state.name_of(kind, handle).is_some()
    }

    pub(super) fn named_with(&self, kind: Kind, handle: u64, prefix: &str) -> bool {
        self.state.name_of(kind, handle).is_some_and(|name| name.starts_with(prefix))
    }

    pub(super) fn count_unregistered(&mut self) {
        self.state.unregistered += 1;
    }

    /// # Safety
    /// `values` points at `count` values.
    pub(super) unsafe fn us<T: Copy + Into<u64>>(&mut self, key: &str, values: *const T, count: u32) -> &mut Self {
        put!(self, " {key}=[");
        for n in 0..count as usize {
            // SAFETY: the caller's.
            let value: u64 = unsafe { *values.add(n) }.into();
            if n != 0 {
                put!(self, ",{value}");
            } else {
                put!(self, "{value}");
            }
        }
        self.raw("]")
    }

    /// The sTypes of a pNext chain, and the structures the renderer chains decoded.
    ///
    /// # Safety
    /// `chain` is null or a valid Vulkan pNext chain.
    pub(super) unsafe fn next(&mut self, chain: *const std::ffi::c_void) -> &mut Self {
        self.raw(" next=[");
        let mut first = true;
        let mut at = chain.cast::<vk::BaseInStructure>();
        while !at.is_null() {
            // SAFETY: a chain's links are valid structures (the caller's).
            let base = unsafe { &*at };
            if !first {
                self.raw(",");
            }
            first = false;
            put!(self, "{}", base.s_type.as_raw() as u32);
            // SAFETY: each structure is the type its sType names, and its arrays hold
            // the counts it gives.
            unsafe {
                match base.s_type {
                    vk::StructureType::PIPELINE_RENDERING_CREATE_INFO => {
                        let r = &*at.cast::<vk::PipelineRenderingCreateInfo>();
                        put!(self, "{{view={} colour=", r.view_mask);
                        for n in 0..r.color_attachment_count as usize {
                            let format = (*r.p_color_attachment_formats.add(n)).as_raw() as u32;
                            if n != 0 {
                                put!(self, ",{format}");
                            } else {
                                put!(self, "{format}");
                            }
                        }
                        put!(
                            self,
                            " depth={} stencil={}}}",
                            r.depth_attachment_format.as_raw() as u32,
                            r.stencil_attachment_format.as_raw() as u32
                        );
                    }
                    vk::StructureType::PIPELINE_LIBRARY_CREATE_INFO_KHR => {
                        let l = &*at.cast::<vk::PipelineLibraryCreateInfoKHR>();
                        self.raw("{");
                        for n in 0..l.library_count as usize {
                            if n != 0 {
                                self.raw(",");
                            }
                            self.name(Kind::Pipeline, vk::Handle::as_raw(*l.p_libraries.add(n)));
                        }
                        self.raw("}");
                    }
                    vk::StructureType::GRAPHICS_PIPELINE_LIBRARY_CREATE_INFO_EXT => {
                        let l = &*at.cast::<vk::GraphicsPipelineLibraryCreateInfoEXT>();
                        put!(self, "{{0x{:x}}}", l.flags.as_raw());
                    }
                    vk::StructureType::IMAGE_FORMAT_LIST_CREATE_INFO => {
                        let l = &*at.cast::<vk::ImageFormatListCreateInfo>();
                        self.raw("{");
                        for n in 0..l.view_format_count as usize {
                            let format = (*l.p_view_formats.add(n)).as_raw() as u32;
                            if n != 0 {
                                put!(self, ",{format}");
                            } else {
                                put!(self, "{format}");
                            }
                        }
                        self.raw("}");
                    }
                    vk::StructureType::DESCRIPTOR_SET_LAYOUT_BINDING_FLAGS_CREATE_INFO => {
                        let l = &*at.cast::<vk::DescriptorSetLayoutBindingFlagsCreateInfo>();
                        self.raw("{");
                        for n in 0..l.binding_count as usize {
                            let flags = (*l.p_binding_flags.add(n)).as_raw();
                            if n != 0 {
                                put!(self, ",0x{flags:x}");
                            } else {
                                put!(self, "0x{flags:x}");
                            }
                        }
                        self.raw("}");
                    }
                    vk::StructureType::DESCRIPTOR_SET_VARIABLE_DESCRIPTOR_COUNT_ALLOCATE_INFO => {
                        let l = &*at.cast::<vk::DescriptorSetVariableDescriptorCountAllocateInfo>();
                        self.raw("{");
                        for n in 0..l.descriptor_set_count as usize {
                            let count = *l.p_descriptor_counts.add(n);
                            if n != 0 {
                                put!(self, ",{count}");
                            } else {
                                put!(self, "{count}");
                            }
                        }
                        self.raw("}");
                    }
                    vk::StructureType::MEMORY_DEDICATED_ALLOCATE_INFO => {
                        let d = &*at.cast::<vk::MemoryDedicatedAllocateInfo>();
                        self.raw("{");
                        self.name(Kind::Image, vk::Handle::as_raw(d.image));
                        self.raw(",");
                        self.name(Kind::Buffer, vk::Handle::as_raw(d.buffer));
                        self.raw("}");
                    }
                    vk::StructureType::MEMORY_ALLOCATE_FLAGS_INFO => {
                        let f = &*at.cast::<vk::MemoryAllocateFlagsInfo>();
                        put!(self, "{{0x{:x}}}", f.flags.as_raw());
                    }
                    vk::StructureType::IMAGE_VIEW_USAGE_CREATE_INFO => {
                        let u = &*at.cast::<vk::ImageViewUsageCreateInfo>();
                        put!(self, "{{0x{:x}}}", u.usage.as_raw());
                    }
                    _ => {}
                }
            }
            at = base.p_next;
        }
        self.raw("]")
    }

    pub(super) fn range(&mut self, r: &vk::ImageSubresourceRange) -> &mut Self {
        put!(
            self,
            "{:x}/{}/{}/{}/{}",
            r.aspect_mask.as_raw(),
            r.base_mip_level,
            r.level_count,
            r.base_array_layer,
            r.layer_count
        );
        self
    }
    pub(super) fn layers(&mut self, r: &vk::ImageSubresourceLayers) -> &mut Self {
        put!(self, "{:x}/{}/{}/{}", r.aspect_mask.as_raw(), r.mip_level, r.base_array_layer, r.layer_count);
        self
    }
    pub(super) fn offset(&mut self, o: &vk::Offset3D) -> &mut Self {
        put!(self, "{},{},{}", o.x, o.y, o.z);
        self
    }
    pub(super) fn extent(&mut self, e: &vk::Extent3D) -> &mut Self {
        put!(self, "{}x{}x{}", e.width, e.height, e.depth);
        self
    }
    pub(super) fn rect(&mut self, r: &vk::Rect2D) -> &mut Self {
        put!(self, "{},{},{}x{}", r.offset.x, r.offset.y, r.extent.width, r.extent.height);
        self
    }

    /// # Safety
    /// `r` points at `count` ranges.
    pub(super) unsafe fn ranges(&mut self, key: &str, r: *const vk::ImageSubresourceRange, count: u32) -> &mut Self {
        put!(self, " {key}=[");
        for n in 0..count as usize {
            if n != 0 {
                self.raw(",");
            }
            // SAFETY: the caller's.
            self.range(unsafe { &*r.add(n) });
        }
        self.raw("]")
    }

    /// # Safety
    /// `r` points at `count` regions.
    pub(super) unsafe fn copies(&mut self, key: &str, r: *const vk::BufferImageCopy, count: u32) -> &mut Self {
        put!(self, " {key}=[");
        for n in 0..count as usize {
            if n != 0 {
                self.raw(",");
            }
            // SAFETY: the caller's.
            let c = unsafe { &*r.add(n) };
            put!(self, "({},{},{},", c.buffer_offset, c.buffer_row_length, c.buffer_image_height);
            self.layers(&c.image_subresource)
                .raw(",")
                .offset(&c.image_offset)
                .raw(",")
                .extent(&c.image_extent)
                .raw(")");
        }
        self.raw("]")
    }

    pub(super) fn stencil(&mut self, o: &vk::StencilOpState) -> &mut Self {
        put!(
            self,
            "({},{},{},{},{:x},{:x},{:x})",
            o.fail_op.as_raw() as u32,
            o.pass_op.as_raw() as u32,
            o.depth_fail_op.as_raw() as u32,
            o.compare_op.as_raw() as u32,
            o.compare_mask,
            o.write_mask,
            o.reference
        );
        self
    }

    pub(super) fn attachment(&mut self, key: Option<&str>, a: Option<&vk::RenderingAttachmentInfo>) -> &mut Self {
        if let Some(key) = key {
            put!(self, " {key}=");
        }
        let Some(a) = a else { return self.raw("-") };
        self.raw("(");
        self.name(Kind::View, vk::Handle::as_raw(a.image_view));
        put!(self, ",{},{},", a.image_layout.as_raw() as u32, a.resolve_mode.as_raw());
        self.name(Kind::View, vk::Handle::as_raw(a.resolve_image_view));
        // SAFETY: a clear value's four words, whichever member was written.
        let colour = unsafe { a.clear_value.color.uint32 };
        put!(
            self,
            ",{},{},{},{:08x},{:08x},{:08x},{:08x})",
            a.resolve_image_layout.as_raw() as u32,
            a.load_op.as_raw() as u32,
            a.store_op.as_raw() as u32,
            colour[0],
            colour[1],
            colour[2],
            colour[3]
        );
        self
    }
}

// ---- the instruments -----------------------------------------------------------------

unsafe extern "C" {
    fn strtoull(text: *const c_char, end: *mut *mut c_char, base: c_int) -> u64;
}

// strtoull's base 10, as the C++ parses: whole text, or not a number.
fn parse_unsigned(text: &str) -> Option<u64> {
    let text = CString::new(text).ok()?;
    let mut end: *mut c_char = std::ptr::null_mut();
    // SAFETY: a C string, and a place for the end.
    let value = unsafe { strtoull(text.as_ptr(), &mut end, 10) };
    // SAFETY: end is inside the string, at its zero at most.
    let ok = !std::ptr::eq(end, text.as_ptr()) && unsafe { *end } == 0;
    ok.then_some(value)
}

/// At the renderer's initialise, before it makes anything: reads the switches, opens the
/// trace and names the handles it did not make. `position` is the recorder's queue
/// position (recorder::position).
#[allow(clippy::too_many_arguments)]
pub fn start(
    device: vk::Device,
    physical: vk::PhysicalDevice,
    queue: vk::Queue,
    cache: vk::PipelineCache,
    layout: vk::PipelineLayout,
    set_layouts: &[vk::DescriptorSetLayout],
    position: fn() -> u64,
) {
    use vk::Handle;
    STARTED.store(clock::nanoseconds(), Ordering::Relaxed);
    DETERMINISTIC.store(env::flag(c"MW2_DETERMINISTIC"), Ordering::Relaxed);
    let forced = Forced {
        instance_depth_resolve: env::flag(c"MW2_FORCE_INSTANCE_DEPTH_RESOLVE"),
        resolve_twin: env::flag(c"MW2_FORCE_RESOLVE_TWIN"),
        drawn_resolve_sets: env::number(c"MW2_DRAWN_RESOLVE_SETS", 0) as u32,
        resolve_remake: env::number(c"MW2_FORCE_RESOLVE_REMAKE", 0) as u32,
        no_multi_draw: env::flag(c"MW2_NO_MULTI_DRAW"),
        lone_uploads: env::flag(c"MW2_FORCE_LONE_UPLOADS"),
        staging_least_kb: env::number(c"MW2_STAGING_LEAST_KB", 0) as u32,
        fake_device_lost: env::real(c"MW2_FAKE_DEVICE_LOST", 0.0),
    };
    *FORCED.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = forced;
    if deterministic() {
        log_info!(
            "renderer: MW2_DETERMINISTIC: completed = submitted, no optimised links, no shader workers, every doomed texture destroyed at once, no GPU timing"
        );
    }
    if forced.instance_depth_resolve {
        log_info!("renderer: MW2_FORCE_INSTANCE_DEPTH_RESOLVE: no drawn depth resolve");
    }
    if forced.resolve_twin {
        log_info!("renderer: MW2_FORCE_RESOLVE_TWIN: every multisampled resolve goes through the twin");
    }
    if forced.drawn_resolve_sets != 0 {
        log_info!("renderer: MW2_DRAWN_RESOLVE_SETS: %u sets for the drawn depth resolve", forced.drawn_resolve_sets);
    }
    if forced.resolve_remake != 0 {
        log_info!(
            "renderer: MW2_FORCE_RESOLVE_REMAKE: every resolve image made again every %u frames",
            forced.resolve_remake
        );
    }
    if forced.no_multi_draw {
        log_info!("renderer: MW2_NO_MULTI_DRAW: one call a draw");
    }
    if forced.lone_uploads {
        log_info!("renderer: MW2_FORCE_LONE_UPLOADS: every texture upload submitted alone");
    }
    if forced.staging_least_kb != 0 {
        log_info!("renderer: MW2_STAGING_LEAST_KB: an upload slot's staging starts at %u KB", forced.staging_least_kb);
    }
    if forced.fake_device_lost > 0.0 {
        log_info!("renderer: MW2_FAKE_DEVICE_LOST: the device is lost %.1f s from now", forced.fake_device_lost);
    }

    let Some(asked) = env::text(c"MW2_TRACE_COMMANDS") else { return };
    let mut s = state();
    let asked = asked.to_bytes();
    let mut path = asked.to_vec();
    // <file>[:<first frame>:<frames>]
    if let Some(last) = asked.iter().rposition(|&c| c == b':').filter(|&last| last > 0)
        && let Some(middle) = asked[..last].iter().rposition(|&c| c == b':')
    {
        let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
        if let (Some(first), Some(frames)) =
            (parse_unsigned(&text(&asked[middle + 1..last])), parse_unsigned(&text(&asked[last + 1..])))
        {
            s.first = first;
            s.frames = frames;
            path.truncate(middle);
        }
    }
    let path_c = crate::log::c_string(path.clone());
    s.path = String::from_utf8_lossy(&path).into_owned();
    let file = path_os(&path).and_then(|p| File::create(p).ok());
    let Some(file) = file else {
        log_warn!("renderer: MW2_TRACE_COMMANDS: cannot write %s", path_c.as_c_str());
        return;
    };
    s.file = Some(BufWriter::with_capacity(1 << 20, file));
    s.position = Some(position);
    s.names[Kind::Device as usize].insert(device.as_raw(), "Device".into());
    s.names[Kind::Physical as usize].insert(physical.as_raw(), "Physical".into());
    s.names[Kind::Queue as usize].insert(queue.as_raw(), "Queue".into());
    s.names[Kind::Cache as usize].insert(cache.as_raw(), "Cache".into());
    s.names[Kind::PipelineLayout as usize].insert(layout.as_raw(), "Layout".into());
    for (n, set_layout) in set_layouts.iter().enumerate() {
        s.names[Kind::SetLayout as usize].insert(set_layout.as_raw(), format!("SetLayout{n}"));
    }
    ON.store(true, Ordering::Relaxed);
    let last = if s.frames == u64::MAX { u64::MAX } else { s.first.wrapping_add(s.frames) };
    log_info!("renderer: MW2_TRACE_COMMANDS: tracing to %s, frames %llu to %llu", path_c.as_c_str(), s.first, last);
    s.emit("trace version=1".into(), -1);
}

#[cfg(unix)]
fn path_os(path: &[u8]) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    Some(std::ffi::OsStr::from_bytes(path).into())
}

#[cfg(not(unix))]
fn path_os(path: &[u8]) -> Option<std::path::PathBuf> {
    std::str::from_utf8(path).ok().map(Into::into)
}

// vsnprintf into a buffer of `size`: what fits of it.
fn clipped(text: &str, size: usize) -> String {
    let bytes = text.as_bytes();
    String::from_utf8_lossy(&bytes[..bytes.len().min(size - 1)]).into_owned()
}

/// Names a handle traced code did not make, by its role. Any thread.
pub fn register(kind: Kind, handle: u64, name: &str) {
    if !on() || handle == 0 {
        return;
    }
    let name = clipped(name, 64);
    state().names[kind as usize].insert(handle, name);
}

/// The next handle of `kind` traced code makes is called `name`, not by its ordinal.
pub fn name_next(kind: Kind, name: &str) {
    if !on() {
        return;
    }
    state().next[kind as usize] = name.to_owned();
}

/// Whether a line made now can fall in the window MW2_TRACE_COMMANDS names: the hashes
/// are slow, and made only then.
pub fn wanted() -> bool {
    on() && !state().outside()
}

/// A line of its own, "<seq> <text>": the hashes, markers.
pub fn note(text: &str) {
    if !on() {
        return;
    }
    let text = clipped(text, 512);
    state().emit(text, -1);
}

/// The renderer finished frame `frame`: the window MW2_TRACE_COMMANDS names is in frames.
pub fn frame(frame: u64) {
    if !on() {
        return;
    }
    let text = format!("frame n={frame}");
    let mut s = state();
    s.generated = frame;
    s.emit(text, frame as i64);
}

/// The renderer's submission, at its call site (gpu_time submits it, untraced).
///
/// # Safety
/// `submit` is a valid VkSubmitInfo.
pub unsafe fn submission(queue: vk::Queue, submit: &vk::SubmitInfo, fence: vk::Fence) {
    if !on() {
        return;
    }
    // SAFETY: the caller's.
    unsafe { submit_line("QueueSubmit", queue, std::slice::from_ref(submit), fence) };
}

/// # Safety
/// Each submit is a valid VkSubmitInfo.
pub(super) unsafe fn submit_line(command: &str, queue: vk::Queue, submits: &[vk::SubmitInfo], fence: vk::Fence) {
    use vk::Handle;
    let mut l = Line::new(command);
    l.h("queue", Kind::Queue, queue.as_raw()).raw(" submits=[");
    for (n, one) in submits.iter().enumerate() {
        if n != 0 {
            l.raw(",");
        }
        l.raw("{");
        let stages = if one.p_wait_dst_stage_mask.is_null() { 0 } else { one.wait_semaphore_count };
        // SAFETY: the arrays hold their counts (the caller's).
        unsafe {
            l.hs("cmds", Kind::Command, one.p_command_buffers, one.command_buffer_count)
                .hs("wait", Kind::Semaphore, one.p_wait_semaphores, one.wait_semaphore_count)
                .us("stages", one.p_wait_dst_stage_mask.cast::<u32>(), stages)
                .hs("signal", Kind::Semaphore, one.p_signal_semaphores, one.signal_semaphore_count)
                .next(one.p_next)
                .raw("}");
        }
    }
    l.raw("]").h("fence", Kind::Fence, fence.as_raw());
}

/// The image handed to the window for frame `frame`: one of Present0..3.
pub fn present(image: vk::Image, frame: u64) {
    use vk::Handle;
    if !on() {
        return;
    }
    let mut l = Line::new("Present");
    l.h("image", Kind::Image, image.as_raw()).u("frame", frame);
    if !l.named_with(Kind::Image, image.as_raw(), "Present") {
        // The window was handed an image that is not one of the four.
        l.raw(" unregistered");
        l.count_unregistered();
    }
}

/// The renderer's shutdown begins: the destroys after this line come in hash-map order.
pub fn shutdown_begins() {
    if !on() {
        return;
    }
    state().emit("shutdown".into(), -1);
}

/// After the last traced call: the summary line, and the file closed.
pub fn finish() {
    if !on() {
        return;
    }
    let mut s = state();
    if s.finished {
        return;
    }
    let pending = std::mem::take(&mut s.pending);
    for line in pending {
        s.write(&line.text, line.frame);
    }
    s.finished = true;
    if let Some(mut file) = s.file.take() {
        let _ = file.flush();
    }
    let path = crate::log::c_string(s.path.clone());
    log_info!(
        "renderer: command trace %s: %llu lines of %llu, %llu unregistered handles",
        path.as_c_str(),
        s.written,
        s.seq,
        s.unregistered
    );
}

// The recorder thread (recorder.rs): lines made on it come in queue order, and a line the
// consumer makes goes after the calls queued before it and before those queued after,
// whichever thread gets there first.
pub fn recorder_started() {
    if !on() {
        return;
    }
    let mut s = state();
    s.recorder_at = 0;
    s.recorder_running = true;
}

pub fn recorder_thread() {
    RECORDER_THREAD.set(true);
}

pub fn recorder_made(position: u64) {
    let mut s = state();
    s.recorder_at = position;
    while s.pending.front().is_some_and(|line| line.position <= position) {
        if let Some(line) = s.pending.pop_front() {
            s.write(&line.text, line.frame);
        }
    }
}

pub fn recorder_stopped() {
    if !on() {
        return;
    }
    let mut s = state();
    let pending = std::mem::take(&mut s.pending);
    for line in pending {
        s.write(&line.text, line.frame);
    }
    s.recorder_running = false;
}

// FNV-1a over little-endian 64-bit words, then the bytes left over: eight times as fast
// as a byte at a time over a submission's arena share.
const FNV_BASIS: u64 = 1469598103934665603;
const FNV_PRIME: u64 = 1099511628211;

fn mix(mut hash: u64, bytes: &[u8]) -> u64 {
    let mut words = bytes.chunks_exact(8);
    for word in &mut words {
        hash = (hash ^ u64::from_le_bytes([word[0], word[1], word[2], word[3], word[4], word[5], word[6], word[7]]))
            .wrapping_mul(FNV_PRIME);
    }
    for &byte in words.remainder() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
    }
    hash
}

pub fn fnv64(bytes: &[u8]) -> u64 {
    mix(FNV_BASIS, bytes)
}

/// The same, for mapped device memory: the arena and the shadow.
///
/// # Safety
/// `bytes` is readable for `size` bytes.
pub unsafe fn fnv_write_combined(bytes: *const u8, size: usize) -> u64 {
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::{__m128i, _mm_store_si128, _mm_stream_load_si128};
    // A plain read of write-combined memory is uncached, a few tens of MB a second here;
    // a streaming load takes a whole line at a time.
    #[repr(C, align(64))]
    struct Chunk([u8; 4096]);
    let mut chunk = Chunk([0; 4096]);
    let mut hash = FNV_BASIS;
    let mut at = bytes;
    let mut size = size;
    while size != 0 {
        let take = size.min(chunk.0.len());
        let mut n = 0;
        if at as usize & 15 == 0 {
            while n + 16 <= take {
                // SAFETY: 16 readable bytes at a 16-aligned address, into the chunk's own
                // aligned bytes; x86-64-v2 has SSE4.1.
                unsafe {
                    _mm_store_si128(
                        chunk.0.as_mut_ptr().add(n).cast::<__m128i>(),
                        _mm_stream_load_si128(at.add(n).cast::<__m128i>().cast_mut()),
                    )
                };
                n += 16;
            }
        }
        // SAFETY: the rest of `take` readable bytes.
        unsafe { std::ptr::copy_nonoverlapping(at.add(n), chunk.0.as_mut_ptr().add(n), take - n) };
        hash = mix(hash, &chunk.0[..take]); // whole words but at the very end: the same as fnv64
        // SAFETY: within the caller's range.
        at = unsafe { at.add(take) };
        size -= take;
    }
    hash
}

/// MW2_DUMP_SHADERS=<dir>: a built-in shader as <dir>/builtin_<name>.spv.
pub fn dump_builtin(name: &str, words: &[u32]) {
    let Some(dir) = env::text(c"MW2_DUMP_SHADERS") else { return };
    let path = format!("{}/builtin_{name}.spv", dir.to_string_lossy());
    if let Ok(mut file) = File::create(path) {
        let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
        let _ = file.write_all(&bytes);
    }
}

/// Whether traced code made the command buffer: util's barrier is traced only then (the
/// presenter's are not the renderer's).
pub(super) fn known_command(command: vk::CommandBuffer) -> bool {
    state().name_of(Kind::Command, vk::Handle::as_raw(command)).is_some()
}

// tools/gpu_diff.py --rig device_trace: the trace's state back to a process's start,
// between the rig's cases.
#[cfg(mw2_gpu_rig)]
pub mod rig {
    use super::*;

    pub fn reset() {
        if let Some(mut file) = state().file.take() {
            let _ = file.flush();
        }
        *state() = State::new();
        ON.store(false, Ordering::Relaxed);
        DETERMINISTIC.store(false, Ordering::Relaxed);
        *FORCED.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Forced::default();
        RECORDER_THREAD.set(false);
    }

    pub fn set_recorder_thread(on: bool) {
        RECORDER_THREAD.set(on);
    }
}
