// EDRAM surfaces as Vulkan images: making them, reading them back, the rendering instances
// they are drawn in, and clearing them; and the decisions the renderer's start takes about
// multisampling (renderer.rs).
//
// The targets are a slab of boxed Targets with a BTreeMap from key to slot, the key
// order kept for the walks that depend on it (the EDRAM clear idiom, the world
// frame's count), and the last answer for colour and for depth by slot, forgotten
// whenever an entry goes (the plan's §3.8). What the rest of the renderer does for them
// -- the frame, the pass, the submission, the report's skips -- it is asked through
// `Frame`, which renderer.rs implements over its own disjoint fields.
//
// Unsafe by nature: every image, view, set and pipeline is made on the renderer's device
// and named by commands queued on the recorder, which run later; an image is destroyed
// only after RetireBeforeDestroy has waited for every submission and the recorder.
#![deny(unsafe_op_in_unsafe_fn)]

use super::device::Vk;
use super::recorder;
use super::renderer_state::TargetKey;
use super::spirv_builtin::{cover_vertex_shader, fetch_depth_shader};
use super::tracked::{self, TrackedImage, Transition};
use super::util;
use crate::log::{log_info, log_warn};
use crate::runtime::gpu::registers::{
    ColorInfo, CopyControl, DepthInfo, RB_DEPTH_INFO, RB_SURFACE_INFO, RegisterFile, SurfaceInfo,
    color_target_format_name, depth_target_format_name,
};
use crate::runtime::gpu::shader::bindings::TEXTURE_SET;
use ash::vk;
use std::collections::BTreeMap;
use std::ffi::CStr;
use std::fmt;

use super::texture_cache::TextureCache;
use crate::runtime::shadow_resolution::{is_shadow_map_depth, surfaces_known};

// Registers the clears read.
pub const RB_DEPTH_CLEAR: u32 = 0x231D;
pub const RB_COLOR_CLEAR: u32 = 0x231E;
// Each colour target's RB_COLOR_INFO (Xenia's RB_COLOR_INFO::rt_register_indices).
pub const RB_COLOR_INFO_OF_TARGET: [u32; 4] = [0x2001, 0x2003, 0x2004, 0x2005];

/// A depth format here carries stencil too, and the two change layout together.
pub const DEPTH_ASPECTS: vk::ImageAspectFlags =
    vk::ImageAspectFlags::from_raw(vk::ImageAspectFlags::DEPTH.as_raw() | vk::ImageAspectFlags::STENCIL.as_raw());

// The title's pixels as an image's drawn at `scale`: int and unsigned multiplies, which
// wrap.
#[inline]
pub fn scaled_offset(at: vk::Offset2D, scale: u32) -> vk::Offset2D {
    vk::Offset2D { x: at.x.wrapping_mul(scale as i32), y: at.y.wrapping_mul(scale as i32) }
}
#[inline]
pub fn scaled_extent(size: vk::Extent2D, scale: u32) -> vk::Extent2D {
    vk::Extent2D { width: size.width.wrapping_mul(scale), height: size.height.wrapping_mul(scale) }
}
#[inline]
pub fn scaled3(x: i32, y: i32, z: i32, scale: u32) -> vk::Offset3D {
    vk::Offset3D { x: x.wrapping_mul(scale as i32), y: y.wrapping_mul(scale as i32), z }
}

#[inline]
fn range(aspect: vk::ImageAspectFlags) -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: aspect,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}

#[inline]
fn layers(aspect: vk::ImageAspectFlags) -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers { aspect_mask: aspect, mip_level: 0, base_array_layer: 0, layer_count: 1 }
}

/// The image enters `next` (tracked.rs): the barrier that needs from its last use, if
/// any, queued for the recorder. The tracked state is the consumer's, and the recorder
/// runs the commands in the order they are queued, so the state is what the image's last
/// queued command left it in.
#[inline]
pub fn record_use(
    image: vk::Image,
    aspect: vk::ImageAspectFlags,
    track: &mut TrackedImage,
    next: TrackedImage,
    keep: bool,
) {
    if let Some(transition) = track.enter(next, keep) {
        record_transitions([Some((image, aspect, transition)), None]);
    }
}

/// Up to two images' transitions in one barrier, queued for the recorder: the stages are
/// both's.
pub fn record_transitions(list: [Option<(vk::Image, vk::ImageAspectFlags, Transition)>; 2]) {
    if list[0].is_none() && list[1].is_none() {
        return;
    }
    recorder::command(move |c| {
        // SAFETY: the recorder's command buffer on the renderer's device; the images live
        // until a RetireBeforeDestroy that waits for this call.
        unsafe { transitions_now(c, list) }
    });
}

/// Up to two images' transitions, recorded now into the recorder's command buffer.
///
/// # Safety
/// Inside a recorder command; the images are the renderer's device's and live until the
/// commands have run.
#[inline]
pub unsafe fn transitions_now(c: &recorder::Context, list: [Option<(vk::Image, vk::ImageAspectFlags, Transition)>; 2]) {
    let mut barriers = [vk::ImageMemoryBarrier::default(); 2];
    let mut count = 0;
    let mut source = vk::PipelineStageFlags::empty();
    let mut destination = vk::PipelineStageFlags::empty();
    for (image, aspect, transition) in list.into_iter().flatten() {
        barriers[count] = transition.image_barrier(image, aspect);
        source |= transition.source_stage;
        destination |= transition.destination_stage;
        count += 1;
    }
    if count != 0 {
        // SAFETY: the caller's.
        unsafe {
            c.vk.util_barrier(c.into, source, destination, vk::DependencyFlags::empty(), &[], &[], &barriers[..count])
        }
    }
}

/// An EDRAM surface as an image.
#[derive(Clone, Copy)]
pub struct Target {
    pub image: vk::Image,
    pub memory: vk::DeviceMemory,
    pub view: vk::ImageView,
    pub format: vk::Format,
    pub guest_format: u32, // for the report
    pub width: u32,
    pub height: u32,
    pub depth: bool,
    /// How many times the title's size the image is, each way.
    pub scale: u32,
    /// The console's 2x and 4x surfaces are drawn with that many samples here; every read
    /// of one is a resolve, straight into the copy asked for or into the single-sampled
    /// twin, made on first use.
    pub samples: vk::SampleCountFlags,
    pub single_image: vk::Image,
    pub single_memory: vk::DeviceMemory,
    pub single_view: vk::ImageView,
    /// A multisampled depth target's depth, for draw_depth_resolve to fetch, and the set
    /// that names it.
    pub sampled_view: vk::ImageView,
    pub sampled_set: vk::DescriptorSet,
    pub draws: u64,
    pub presented: u64,
    /// Colour draws this frame; a multisampled target with any is the world pass.
    pub colour_draws_this_frame: u64,
    /// How many presented frames drew colour into this target at all.
    pub frames_with_colour: u64,
    /// The draw count at which an EDRAM clear last emptied this, so the second rectangle
    /// of a clear pair does not clear it again.
    pub cleared_at_draw: u64,
    /// The frame that clear happened in, for the pass trace.
    pub depth_cleared_in: u64,
    /// What the image and the twin were last used for (tracked.rs).
    pub track: TrackedImage,
    pub single_track: TrackedImage,
}

impl Default for Target {
    fn default() -> Self {
        Target {
            image: vk::Image::null(),
            memory: vk::DeviceMemory::null(),
            view: vk::ImageView::null(),
            format: vk::Format::UNDEFINED,
            guest_format: 0,
            width: 0,
            height: 0,
            depth: false,
            scale: 1,
            samples: vk::SampleCountFlags::TYPE_1,
            single_image: vk::Image::null(),
            single_memory: vk::DeviceMemory::null(),
            single_view: vk::ImageView::null(),
            sampled_view: vk::ImageView::null(),
            sampled_set: vk::DescriptorSet::null(),
            draws: 0,
            presented: 0,
            colour_draws_this_frame: 0,
            frames_with_colour: 0,
            cleared_at_draw: u64::MAX,
            depth_cleared_in: u64::MAX,
            track: TrackedImage::NEW,
            single_track: TrackedImage::NEW,
        }
    }
}

/// The set-up command buffer, its fence and the queue (State::setup, setupFence, queue):
/// work that cannot go inside a frame, submitted and waited for at once.
#[derive(Clone, Copy)]
pub struct Setup {
    pub command: vk::CommandBuffer,
    pub fence: vk::Fence,
    pub queue: vk::Queue,
}

/// The present image a swap copies the frame into, the frame it last held, which the
/// window must have taken before it is drawn over, and the present images' size
/// (State::present[presentIndex], presentSerial[presentIndex], presentWidth/Height).
#[derive(Clone, Copy)]
pub struct Present {
    pub image: vk::Image,
    pub serial: u64,
    pub width: u32,
    pub height: u32,
}

/// Where a depth resolve writes: the target's own twin, or a resolve's copy through its
/// attachment view of both aspects, with what the copy was last used for.
pub enum DepthInto<'a> {
    Twin,
    Copy { image: vk::Image, view: vk::ImageView, track: &'a mut TrackedImage },
}

/// What targets and resolves ask of the rest of the renderer: renderer_frame.rs's,
/// renderer_passtime.rs's and renderer_diagnostics.rs's functions and the fields they
/// read or set, which renderer.rs implements over its own fields -- none of them the
/// targets or the resolves, so both can be borrowed beside it.
pub trait Frame {
    /// g.frames: presented frames so far.
    fn frames(&self) -> u64;
    /// g.drawsRecorded.
    fn draws_recorded(&self) -> u64;
    /// g.recording: a frame's command buffer is open.
    fn recording(&self) -> bool;
    fn begin_frame(&mut self) -> bool;
    fn end_pass(&mut self);
    /// Submit(), not waiting.
    fn submit(&mut self) -> bool;
    /// Before destroying an image the GPU may still use: submit and wait, drain the
    /// recorder, vkDeviceWaitIdle.
    fn retire_before_destroy(&mut self);
    /// The frame copied into `present` handed on; its world-frame count walks the targets,
    /// and MW2_DUMP_TARGETS reads them and the resolves' copies.
    fn finish_frame(
        &mut self,
        present: vk::Image,
        targets: &mut Targets,
        records: &mut super::renderer_state::ResolveRecords,
    );
    /// Skip(why): counted by reason for the report, by the literal's address.
    fn skip(&mut self, why: &'static CStr);
    /// g.currentScale, which open_guest_query reads.
    fn set_current_scale(&mut self, scale: u32);
    fn open_guest_query(&mut self);
    /// The rendering instance just opened: g.currentColour, currentDepth, currentSamples,
    /// currentWidth and currentHeight.
    fn set_current(&mut self, colour: vk::ImageView, depth: vk::ImageView, samples: u32, width: u32, height: u32);
    /// g.bound = {}: the pipeline, viewport and scissor are another draw's.
    fn reset_bound(&mut self);
    /// g.arenaUsed and g.arenaSlotBytes.
    fn arena_used(&self) -> u32;
    fn arena_slot_bytes(&self) -> u32;
    fn setup(&self) -> Setup;
    /// The draws' sets 1 and 2 and their layout: vk::textures::Set(), g.memorySet,
    /// The shared pipeline layout (shaders/src/pipeline.rs).
    fn texture_set(&self) -> vk::DescriptorSet;
    fn memory_set(&self) -> vk::DescriptorSet;
    fn pipeline_layout(&self) -> vk::PipelineLayout;
    /// The present image g.presentIndex names.
    fn present(&self) -> Present;
    /// What the present image present() names was last used for.
    fn present_track(&mut self) -> &mut TrackedImage;
    /// MW2_TIME_RENDER (Timing()).
    fn timing(&self) -> bool;
    fn pass_times_open(&mut self, colour: &Target, samples: u32);
    fn pass_times_between(&mut self);
    /// PassTimesMark(format, ...): a mark by a name of its own.
    fn pass_times_mark(&mut self, name: fmt::Arguments<'_>);
    /// The texture cache, whose texture_cache::forget_views a resolve image's replacement
    /// calls (vk::textures::ForgetViews).
    fn textures(&mut self) -> &mut TextureCache;
}

// ---- the multisampling decisions (the renderer's start) ---------------------------------

/// What the renderer's start reads to decide on multisampling: the switches and the
/// device's properties as its queries answer them.
#[derive(Clone, Copy, Default)]
pub struct MultisampleQuery {
    /// MW2_NO_MSAA.
    pub off: bool,
    /// MW2_MSAA, as uint32_t(env::Number).
    pub forced: u32,
    /// MW2_FORCE_INSTANCE_DEPTH_RESOLVE (diagnostics).
    pub instance_depth_resolve: bool,
    pub api_version: u32,
    pub colour_counts: vk::SampleCountFlags,
    pub depth_counts: vk::SampleCountFlags,
    pub stencil_counts: vk::SampleCountFlags,
    pub sampled_depth_counts: vk::SampleCountFlags,
    /// VkPhysicalDeviceDepthStencilResolveProperties, zero before Vulkan 1.2 (not asked).
    pub depth_resolve_modes: vk::ResolveModeFlags,
    pub stencil_resolve_modes: vk::ResolveModeFlags,
    /// Optimal-tiling features of the two depth formats.
    pub depth_features: vk::FormatFeatureFlags,
    pub float_depth_features: vk::FormatFeatureFlags,
}

/// What was decided: whether the console's multisampled surfaces are drawn multisampled,
/// whether a multisampled depth surface's first sample is copied by a draw, and the
/// counts the device offers for colour, depth and stencil alike.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Multisampling {
    pub msaa: bool,
    pub drawn_depth_resolve: bool,
    pub sample_counts: vk::SampleCountFlags,
    /// Why multisampling is off, for the line: 0 on, 1 MW2_NO_MSAA, 2 older than 1.2,
    /// 3 no first-sample depth resolve, 4 1x only.
    pub why_off: u32,
}

/// The console's 1x, 2x and 4x, as the device can draw them: a count the
/// device lacks is rounded up to the next one it has, 1x the
/// last resort. `forced` (MW2_MSAA) draws every surface the title multisamples at that
/// many instead, rounded down to what the device has.
pub fn host_samples(guest_samples: u32, msaa: bool, forced: u32, counts: vk::SampleCountFlags) -> vk::SampleCountFlags {
    if !msaa {
        return vk::SampleCountFlags::TYPE_1;
    }
    let counts = counts.as_raw();
    if forced > 1 && guest_samples != 0 {
        let mut bit = 64u32;
        while bit > 1 {
            if bit <= forced && counts & bit != 0 {
                return vk::SampleCountFlags::from_raw(bit);
            }
            bit >>= 1;
        }
    }
    let mut bit = 1u32 << (guest_samples & 3);
    while bit <= 64 {
        if counts & bit != 0 {
            return vk::SampleCountFlags::from_raw(bit);
        }
        bit <<= 1;
    }
    vk::SampleCountFlags::TYPE_1
}

/// The decision, on what the queries answered.
pub fn decide_multisampling(q: &MultisampleQuery) -> Multisampling {
    let sample_counts = q.colour_counts & q.depth_counts & q.stencil_counts;
    let modern = q.api_version >= vk::API_VERSION_1_2;
    let depth_resolve = modern
        && q.depth_resolve_modes.contains(vk::ResolveModeFlags::SAMPLE_ZERO)
        && q.stencil_resolve_modes.contains(vk::ResolveModeFlags::SAMPLE_ZERO);
    let msaa = !q.off && depth_resolve && !(sample_counts & !vk::SampleCountFlags::TYPE_1).is_empty();
    // The depth resolve is a draw that samples the surface where the device can sample a
    // multisampled depth image of both formats at the counts the title's 2x and 4x are
    // drawn at.
    let used = if msaa {
        host_samples(1, msaa, q.forced, sample_counts) | host_samples(2, msaa, q.forced, sample_counts)
    } else {
        vk::SampleCountFlags::empty()
    };
    let mut drawn_depth_resolve = msaa
        && (q.sampled_depth_counts & used) == used
        && q.depth_features.contains(vk::FormatFeatureFlags::SAMPLED_IMAGE)
        && q.float_depth_features.contains(vk::FormatFeatureFlags::SAMPLED_IMAGE);
    if q.instance_depth_resolve {
        drawn_depth_resolve = false;
    }
    let why_off = if msaa {
        0
    } else if q.off {
        1
    } else if !modern {
        2
    } else if !depth_resolve {
        3
    } else {
        4
    };
    Multisampling { msaa, drawn_depth_resolve, sample_counts, why_off }
}

/// The renderer's start's block: the switches read, the device asked (its 1.2
/// properties only on a 1.2 device, then each depth format's features), the decision
/// taken and said. `properties` is what vkGetPhysicalDeviceProperties answered.
///
/// # Safety
/// `vk` is the renderer's device, of the live `physical`.
pub unsafe fn multisampling(
    vk: &Vk,
    physical: vk::PhysicalDevice,
    properties: &vk::PhysicalDeviceProperties,
    depth_format: vk::Format,
    float_depth_format: vk::Format,
) -> Multisampling {
    let mut q = MultisampleQuery {
        off: crate::env::flag(c"MW2_NO_MSAA"),
        forced: crate::env::number(c"MW2_MSAA", 0) as u32,
        api_version: properties.api_version,
        colour_counts: properties.limits.framebuffer_color_sample_counts,
        depth_counts: properties.limits.framebuffer_depth_sample_counts,
        stencil_counts: properties.limits.framebuffer_stencil_sample_counts,
        sampled_depth_counts: properties.limits.sampled_image_depth_sample_counts,
        ..Default::default()
    };
    // SAFETY: the caller's.
    unsafe {
        if q.api_version >= vk::API_VERSION_1_2 {
            let mut resolve = vk::PhysicalDeviceDepthStencilResolveProperties::default();
            let mut properties2 = vk::PhysicalDeviceProperties2 {
                p_next: (&mut resolve as *mut vk::PhysicalDeviceDepthStencilResolveProperties).cast(),
                ..Default::default()
            };
            vk.get_physical_device_properties2(physical, &mut properties2);
            q.depth_resolve_modes = resolve.supported_depth_resolve_modes;
            q.stencil_resolve_modes = resolve.supported_stencil_resolve_modes;
        }
        q.depth_features = vk.get_physical_device_format_properties(physical, depth_format).optimal_tiling_features;
        q.float_depth_features =
            vk.get_physical_device_format_properties(physical, float_depth_format).optimal_tiling_features;
    }
    #[cfg(feature = "diagnostics")]
    {
        q.instance_depth_resolve = super::trace::forcing().instance_depth_resolve;
    }
    let m = decide_multisampling(&q);
    say_multisampling(&m, q.forced);
    m
}

/// The line the renderer's start says about it.
pub fn say_multisampling(m: &Multisampling, forced: u32) {
    if m.msaa {
        let counts = m.sample_counts;
        let has = |bit: vk::SampleCountFlags, text: &'static CStr| if counts.contains(bit) { text } else { c"" };
        log_info!(
            "renderer: multisampling on: the device draws at%s%s%s%s, the title's 2x and 4x surfaces are drawn at %ux and %ux%s, depth resolved by %s",
            has(vk::SampleCountFlags::TYPE_2, c" 2x"),
            has(vk::SampleCountFlags::TYPE_4, c" 4x"),
            has(vk::SampleCountFlags::TYPE_8, c" 8x"),
            has(vk::SampleCountFlags::TYPE_16, c" 16x"),
            host_samples(1, true, forced, counts).as_raw(),
            host_samples(2, true, forced, counts).as_raw(),
            if crate::env::text(c"MW2_MSAA").is_some() { c" (MW2_MSAA)" } else { c"" },
            if m.drawn_depth_resolve { c"a draw" } else { c"the rendering instance" }
        );
    } else {
        let why = match m.why_off {
            1 => c"MW2_NO_MSAA",
            2 => c"the device is older than Vulkan 1.2",
            3 => c"no first-sample depth resolve",
            _ => c"the device draws at 1x only",
        };
        log_info!("renderer: multisampling off (%s): the title's 2x and 4x surfaces are drawn at 1x", why);
    }
}

// ---- pure mappings ------------------------------------------------------------------------

/// The host format of a colour surface of RB_COLOR_INFO's format (ColourFormatFor).
pub fn colour_format_for(format: u32) -> vk::Format {
    match format {
        0 | 1 | 10 => vk::Format::R8G8B8A8_UNORM, // 8_8_8_8 and gamma
        2 | 3 | 12 => vk::Format::A2B10G10R10_UNORM_PACK32,
        4 => vk::Format::R16G16_SNORM,
        5 => vk::Format::R16G16B16A16_SNORM,
        6 => vk::Format::R16G16_SFLOAT,
        7 => vk::Format::R16G16B16A16_SFLOAT,
        14 => vk::Format::R32_SFLOAT,
        15 => vk::Format::R32G32_SFLOAT,
        _ => vk::Format::UNDEFINED,
    }
}

/// As Xenia's UNorm24To32 and Float20e4To32 (DepthClearValue): the unorm divided by 2^24
/// with its top bit added back, which is exact, and the float24 with 4 exponent bits
/// biased by 15 and 20 of mantissa, denormals normalised. A float24 reaches 2, past what
/// a clear may write; the host's depth is 0 to 1. `packed` is RB_DEPTH_CLEAR, the depth
/// in its upper 24 bits.
pub fn depth_clear_value(guest_format: u32, packed: u32) -> f32 {
    let value = packed >> 8;
    if guest_format == 0 {
        return (value + (value >> 23)) as f32 * (1.0 / (1u32 << 24) as f32);
    }
    if value == 0 {
        return 0.0;
    }
    let mut mantissa = value & 0xFFFFF;
    let mut exponent = value >> 20;
    if exponent == 0 {
        let shift = mantissa.leading_zeros().wrapping_sub(32 - 21);
        exponent = 1i32.wrapping_sub(shift as i32) as u32;
        mantissa = mantissa.wrapping_shl(shift) & 0xFFFFF;
    }
    // std::min(x, 1.0f): x unless 1 < x.
    let x = f32::from_bits((exponent.wrapping_add(112) << 23) | (mantissa << 3));
    if 1.0 < x { 1.0 } else { x }
}

/// The settings the renderer's start decided that the targets read.
#[derive(Clone, Copy)]
pub struct TargetSettings {
    pub multisampling: Multisampling,
    /// MW2_MSAA, read once.
    pub forced_samples: u32,
    /// The host formats of the title's D24S8 and D24FS8 depth surfaces.
    pub depth_format: vk::Format,
    pub float_depth_format: vk::Format,
    /// MW2_SCALE and MW2_SHADOW_SCALE as the device allows them.
    pub scale: u32,
    pub shadow_scale: u32,
    pub memory: vk::PhysicalDeviceMemoryProperties,
    /// The widest render target the device takes, min(maxFramebufferWidth,
    /// maxImageDimension2D) as Xenia's GetMaxRenderTargetWidth: a resolve's clear of a
    /// surface whose pitch at its scale is wider is refused (clear_after_resolve).
    pub max_target_width: u32,
    /// The device whose formats' features format_support.rs's decisions read.
    pub physical: vk::PhysicalDevice,
}

// The last target ensure answered for colour and for depth: a draw asks for both, and the
// same two as the draw before it, nearly always.
#[derive(Clone, Copy)]
struct LastTarget {
    key: TargetKey,
    id: u32, // NONE for none
}
const NONE: u32 = u32::MAX;
const NO_LAST: LastTarget =
    LastTarget { key: TargetKey { base_tile: 0, pitch: 0, format: 0, samples: 0, depth: false, scale: 0 }, id: NONE };

// PassScale's statics: whether the shadow maps' surfaces are known yet, and the last
// question and its answer.
#[derive(Clone, Copy)]
struct PassScaleCache {
    known: bool,
    last_info: u32,
    last_samples: u32,
    last_scale: u32,
}

// The depth resolve as a draw: its set and pipeline layouts, its pool of sets, a pipeline
// per depth format (std::map's order, by the format's value), and a failure remembered.
struct DrawnResolve {
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pool: vk::DescriptorPool,
    pipelines: BTreeMap<i32, vk::Pipeline>,
    failed: bool,
}

impl DrawnResolve {
    const fn new() -> DrawnResolve {
        DrawnResolve {
            set_layout: vk::DescriptorSetLayout::null(),
            layout: vk::PipelineLayout::null(),
            pool: vk::DescriptorPool::null(),
            pipelines: BTreeMap::new(),
            failed: false,
        }
    }
}

/// The EDRAM surfaces as images (State::targets and lastTargets).
pub struct Targets {
    slab: Vec<Option<Box<Target>>>,
    free: Vec<u32>,
    map: BTreeMap<TargetKey, u32>,
    last: [LastTarget; 2],
    drawn: DrawnResolve,
    pass: PassScaleCache,
    pub settings: TargetSettings,
    /// The clears after a resolve, by what Xenia's rectangle came to (S14d).
    pub clears: ResolveClears,
}

impl Targets {
    pub fn new(settings: TargetSettings) -> Targets {
        Targets {
            slab: Vec::new(),
            free: Vec::new(),
            map: BTreeMap::new(),
            last: [NO_LAST; 2],
            drawn: DrawnResolve::new(),
            pass: PassScaleCache { known: false, last_info: u32::MAX, last_samples: u32::MAX, last_scale: 1 },
            settings,
            clears: ResolveClears::default(),
        }
    }

    /// EDRAM has one depth buffer, not one per way of describing it, so the pitch is not
    /// part of a depth target's key; with the surfaces drawn multisampled the sample count
    /// stays in it, because a depth image has one count and the colour target beside it
    /// shares it.
    #[inline]
    pub fn depth_key(&self, base_tile: u32, format: u32, samples: u32, scale: u32) -> TargetKey {
        TargetKey {
            base_tile,
            pitch: 0,
            format,
            samples: if self.settings.multisampling.msaa { samples } else { 0 },
            depth: true,
            scale,
        }
    }

    /// The host format of a depth surface of RB_DEPTH_INFO's format: float for D24FS8.
    /// MW2 draws all its depth in D24FS8, reversed (cleared to zero, tested GEQUAL): an
    /// integer format spends its steps near the camera and leaves the distance a few.
    /// D24S8 is a pass with no depth surface (RB_DEPTH_INFO 0, tile 0), none with depth or
    /// stencil on, so nothing its image holds is ever read.
    #[inline]
    pub fn depth_format_for(&self, guest_format: u32) -> vk::Format {
        if guest_format != 0 { self.settings.float_depth_format } else { self.settings.depth_format }
    }

    #[inline]
    pub fn host_samples(&self, guest_samples: u32) -> vk::SampleCountFlags {
        let m = &self.settings.multisampling;
        host_samples(guest_samples, m.msaa, self.settings.forced_samples, m.sample_counts)
    }

    /// The scale a pass is drawn at, and its surfaces and their copies made at: the shadow
    /// maps' for a pass whose depth registers name the surface the title made for them,
    /// the frame's for any other (PassScale).
    #[inline]
    pub fn pass_scale(&mut self, depth_info: u32, samples: u32) -> u32 {
        let s = &self.settings;
        if s.shadow_scale == s.scale {
            return s.scale;
        }
        if !self.pass.known {
            self.pass.known = surfaces_known();
            if !self.pass.known {
                return s.scale;
            }
        }
        if depth_info != self.pass.last_info || samples != self.pass.last_samples {
            self.pass.last_info = depth_info;
            self.pass.last_samples = samples;
            self.pass.last_scale = if is_shadow_map_depth(depth_info, samples) { s.shadow_scale } else { s.scale };
        }
        self.pass.last_scale
    }

    #[inline]
    pub fn find(&self, key: &TargetKey) -> Option<u32> {
        self.map.get(key).copied()
    }

    /// The target in slot `id`, which find or ensure answered and nothing has erased since.
    #[inline]
    pub fn get(&self, id: u32) -> &Target {
        self.slab[id as usize].as_deref().expect("a live target's slot")
    }

    #[inline]
    pub fn get_mut(&mut self, id: u32) -> &mut Target {
        self.slab[id as usize].as_deref_mut().expect("a live target's slot")
    }

    /// Every target in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&TargetKey, &Target)> {
        self.map.iter().map(|(key, &id)| (key, self.slab[id as usize].as_deref().expect("a live target's slot")))
    }

    /// Every target in key order, to change.
    pub fn for_each_mut(&mut self, mut f: impl FnMut(&TargetKey, &mut Target)) {
        for (key, &id) in &self.map {
            f(key, self.slab[id as usize].as_deref_mut().expect("a live target's slot"));
        }
    }

    fn insert(&mut self, key: TargetKey, target: Target) -> u32 {
        let id = match self.free.pop() {
            Some(id) => {
                self.slab[id as usize] = Some(Box::new(target));
                id
            }
            None => {
                self.slab.push(Some(Box::new(target)));
                (self.slab.len() - 1) as u32
            }
        };
        self.map.insert(key, id);
        id
    }

    fn erase(&mut self, key: &TargetKey) {
        if let Some(id) = self.map.remove(key) {
            self.slab[id as usize] = None;
            self.free.push(id);
        }
    }

    /// The target `key` names, at least `width` by `height` of the title's pixels: the
    /// last answer, the map's, or a new image -- one that grew is made again at the
    /// larger of the two sizes, after RetireBeforeDestroy. None, with the reason
    /// skipped, when the format has no host one or the image cannot be made.
    ///
    /// # Safety
    /// `vk` is the renderer's live device.
    #[inline]
    pub unsafe fn ensure<F: Frame>(
        &mut self,
        vk: &Vk,
        f: &mut F,
        key: &TargetKey,
        width: u32,
        height: u32,
        guest_format: u32,
    ) -> Option<u32> {
        let last = self.last[key.depth as usize];
        // A last answer's slot is live (the answers are forgotten whenever an entry goes);
        // the lookup's misses fall to the slow path, which finds the same, so the draw's
        // path has no panic.
        if last.key == *key
            && let Some(Some(target)) = self.slab.get(last.id as usize)
            && target.width >= width
            && target.height >= height
        {
            return Some(last.id);
        }
        // SAFETY: the caller's.
        unsafe { self.ensure_slow(vk, f, key, width, height, guest_format) }
    }

    #[cold]
    #[inline(never)]
    unsafe fn ensure_slow<F: Frame>(
        &mut self,
        vk: &Vk,
        f: &mut F,
        key: &TargetKey,
        mut width: u32,
        mut height: u32,
        guest_format: u32,
    ) -> Option<u32> {
        let found = self.find(key);
        if let Some(id) = found {
            let target = self.get(id);
            if target.width >= width && target.height >= height {
                self.last[key.depth as usize] = LastTarget { key: *key, id };
                return Some(id);
            }
            // The surface grew. Rare enough to rebuild rather than plan for.
            f.retire_before_destroy();
            let mut old = *self.get(id);
            // SAFETY: the caller's; the GPU and the recorder are done with it.
            unsafe { destroy_target(vk, &mut self.drawn, &mut old) };
            width = width.max(old.width);
            height = height.max(old.height);
            self.erase(key);
            self.last = [NO_LAST; 2];
        }

        let mut target = Target {
            depth: key.depth,
            scale: key.scale,
            width,
            height,
            format: vk::Format::from_raw(key.format as i32),
            guest_format,
            samples: self.host_samples(key.samples),
            ..Default::default()
        };
        if target.format == vk::Format::UNDEFINED {
            f.skip(c"render target format");
            return None;
        }

        // TRANSFER_DST because targets are cleared with transfer commands -- on a new
        // target, after a resolve, and for the EDRAM clear idiom.
        let usage = if key.depth {
            vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT
                | vk::ImageUsageFlags::TRANSFER_SRC
                | vk::ImageUsageFlags::TRANSFER_DST
        } else {
            vk::ImageUsageFlags::COLOR_ATTACHMENT
                | vk::ImageUsageFlags::TRANSFER_SRC
                | vk::ImageUsageFlags::TRANSFER_DST
                | vk::ImageUsageFlags::SAMPLED
        };
        let aspect = if key.depth { DEPTH_ASPECTS } else { vk::ImageAspectFlags::COLOR };
        // A format that cannot be drawn into is refused like one of no format; a colour
        // target is sampled by nothing (a resolve copies out of it), so it is made sampled
        // only where its format can be (format_support.rs).
        // SAFETY: the caller's.
        let features = unsafe { vk.format_features(self.settings.physical, target.format) };
        let drawn = if key.depth {
            vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT
        } else {
            vk::FormatFeatureFlags::COLOR_ATTACHMENT
        };
        if !features.contains(drawn) {
            f.skip(c"render target format");
            return None;
        }
        let usage = if key.depth || features.contains(vk::FormatFeatureFlags::SAMPLED_IMAGE) {
            usage
        } else {
            usage & !vk::ImageUsageFlags::SAMPLED
        };
        let sampled = key.depth
            && self.settings.multisampling.drawn_depth_resolve
            && target.samples != vk::SampleCountFlags::TYPE_1;
        let usage = if sampled { usage | vk::ImageUsageFlags::SAMPLED } else { usage };
        // SAFETY: the caller's.
        if !unsafe { make_image(vk, &self.settings.memory, &mut target, usage, aspect) } {
            f.skip(c"render target allocation");
            return None;
        }
        if sampled {
            // SAFETY: the caller's; the image was just made.
            unsafe { make_sampled_depth(vk, &mut self.drawn, &mut target) };
        }

        log_info!(
            "renderer: %s target %ux%u at EDRAM tile %u (%s), %ux for the title's %ux",
            if key.depth { c"depth" } else { c"colour" },
            target.width.wrapping_mul(target.scale),
            target.height.wrapping_mul(target.scale),
            key.base_tile,
            if key.depth {
                depth_target_format_name(guest_format)
            } else {
                color_target_format_name(guest_format).unwrap_or(c"unknown")
            },
            target.samples.as_raw(),
            1u32.wrapping_shl(key.samples)
        );
        let id = self.insert(*key, target);
        // SAFETY: the caller's; the image was just made.
        unsafe { clear_new_target(vk, f, self.get_mut(id)) };
        Some(id)
    }

    /// Opens a rendering instance over the two targets (closing the one open, if it is
    /// another), each brought from its last use into the attachment's (tracked.rs): what a
    /// render pass's dependency in from the outside was, for these two alone. Both are
    /// drawn at the pass's scale: their keys carry it.
    pub fn begin_rendering<F: Frame>(&mut self, f: &mut F, colour: u32, depth: u32, width: u32, height: u32) {
        f.end_pass();
        let into_colour = {
            let target = self.get_mut(colour);
            target.track.enter(tracked::COLOUR_DRAWN, true).map(|t| (target.image, vk::ImageAspectFlags::COLOR, t))
        };
        let into_depth = {
            let target = self.get_mut(depth);
            target.track.enter(tracked::DEPTH_DRAWN, true).map(|t| (target.image, DEPTH_ASPECTS, t))
        };
        let colour = self.get(colour);
        let depth = self.get(depth);
        f.pass_times_open(colour, colour.samples.as_raw());
        let colour_view = colour.view;
        let depth_view = depth.view;
        let area = scaled_extent(vk::Extent2D { width, height }, colour.scale);
        recorder::command(move |c| {
            // SAFETY: the recorder's command buffer on the renderer's device; the images and
            // views live until a RetireBeforeDestroy that waits for this call.
            unsafe {
                transitions_now(c, [into_colour, into_depth]);
                // LOAD, not CLEAR: a frame is many rendering instances -- one per change
                // of target -- and only the title's own draws clear anything.
                let colour_attachment = vk::RenderingAttachmentInfo {
                    image_view: colour_view,
                    image_layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    load_op: vk::AttachmentLoadOp::LOAD,
                    store_op: vk::AttachmentStoreOp::STORE,
                    ..Default::default()
                };
                let depth_attachment = vk::RenderingAttachmentInfo {
                    image_view: depth_view,
                    image_layout: vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
                    ..colour_attachment
                };
                let info = vk::RenderingInfo {
                    render_area: vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent: area },
                    layer_count: 1,
                    color_attachment_count: 1,
                    p_color_attachments: &colour_attachment,
                    p_depth_attachment: &depth_attachment,
                    p_stencil_attachment: &depth_attachment,
                    ..Default::default()
                };
                c.vk.cmd_begin_rendering(c.into, &info);
            }
        });
        f.set_current_scale(colour.scale);
        f.open_guest_query();
        f.set_current(colour.view, depth.view, colour.samples.as_raw(), width, height);
    }

    /// The EDRAM clear idiom: every depth target from `from_tile` up cleared to zero, once
    /// per draw count, ending the pass first. MW2 clears EDRAM by drawing: it points the
    /// surface at a base whose pitch and height span the rest of the memory and draws
    /// rectangles over all of it, depth test ALWAYS and writes on, in EDRAM's own
    /// coordinates, in pairs at two pitches and sample counts because a surface is not a
    /// whole number of 80-sample tiles. Here each (base, pitch, samples) is an image of its
    /// own, so the idiom is answered with clears of the images it names, and the rectangles
    /// are not drawn.
    pub fn clear_aliased_depth<F: Frame>(&mut self, f: &mut F, from_tile: u32) {
        let mut ended = false;
        let draws = f.draws_recorded();
        for (key, &id) in &self.map {
            let target = self.slab[id as usize].as_deref_mut().expect("a live target's slot");
            if !target.depth || key.base_tile < from_tile {
                continue;
            }
            // Nothing has been drawn since the last time this was cleared, so there is
            // nothing to clear: the idiom arrives as a pair of rectangles.
            if target.cleared_at_draw == draws {
                continue;
            }
            if !ended {
                f.end_pass();
                ended = true;
            }
            let aspect = DEPTH_ASPECTS;
            record_use(target.image, aspect, &mut target.track, tracked::COPIED_TO, true);
            // Zero: the level tests GEQUAL and RB_DEPTH_CLEAR is zero.
            let value = vk::ClearDepthStencilValue { depth: 0.0, stencil: 0 };
            let image = target.image;
            recorder::command(move |c| {
                // SAFETY: the recorder's command buffer; the image lives until a retire.
                unsafe {
                    c.vk.cmd_clear_depth_stencil_image(
                        c.into,
                        image,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        &value,
                        &[range(aspect)],
                    )
                }
            });
            target.cleared_at_draw = draws;
            target.depth_cleared_in = f.frames();
        }
    }

    /// The Xenos clears the EDRAM surface as part of the resolve that reads it (bits 8
    /// and 9 of RB_COPY_CONTROL): the resolve's rectangle, as Xenia's
    /// PrepareHostRenderTargetsResolveClear works it out (resolve_clear), in the depth
    /// target and the colour target the copy's source select names (RB_COLOR_INFO of
    /// rt_register_indices[copy_src_select]). Colour only where the resolve copies colour,
    /// as Xenia's IsClearingColor; either only where Xenia's tile range for it is not empty
    /// (past EDRAM's last tile, or the other target's start, it is). A rectangle covering the whole image is cleared as one
    /// (a transfer clear, the same contents); a smaller one in a rendering instance of
    /// that area alone whose load clears it, which leaves the rest of the image as it was
    /// (Xenia: vkCmdClearAttachments with the rectangle, the same contents).
    pub fn clear_after_resolve(&mut self, r: &RegisterFile, control: &CopyControl) {
        if !control.clears_color() && !control.clears_depth() {
            return;
        }
        let clears_colour = control.clears_color() && !copies_depth(control);
        if control.clears_color() && !clears_colour {
            self.clears.colour_of_depth_resolve += 1;
        }
        if !clears_colour && !control.clears_depth() {
            return;
        }
        // The source select is below 4 where colour is cleared.
        let colour_info = clears_colour.then(|| r.get(RB_COLOR_INFO_OF_TARGET[control.source_select() as usize & 3]));
        let Some(clear) = resolve_clear(r, control.clears_depth(), colour_info) else {
            self.clears.nothing += 1;
            return;
        };
        // PrepareHostRenderTargetsResolveClear's third refusal: the pitch at the scale the
        // targets are drawn at (Xenia's pitch_pixels_scaled; the renderer's scale is per
        // pass, not Xenia's IsScaleNativeForPitch) wider than the device's widest target.
        let scale = self.pass_scale(r.get(RB_DEPTH_INFO), SurfaceInfo(r.get(RB_SURFACE_INFO)).msaa_samples());
        if clear.pitch_pixels.saturating_mul(scale) > self.settings.max_target_width {
            self.clears.nothing += 1;
            return;
        }
        let rectangle = clear.rectangle;
        self.clears.no_tiles +=
            u64::from(clears_colour && !clear.colour) + u64::from(control.clears_depth() && !clear.depth);

        if let (true, Some(colour_info)) = (clear.colour, colour_info) {
            let surface = SurfaceInfo(r.get(RB_SURFACE_INFO));
            let colour = ColorInfo(colour_info);
            let key = TargetKey {
                base_tile: colour.base_tile(),
                pitch: surface.pitch(),
                format: colour_format_for(colour.format()).as_raw() as u32,
                samples: surface.msaa_samples(),
                depth: false,
                scale: self.pass_scale(r.get(RB_DEPTH_INFO), surface.msaa_samples()),
            };
            if let Some(id) = self.find(&key) {
                // RB_COLOR_CLEAR is the packed colour in the surface's own format; for the
                // 8_8_8_8 family that is ARGB, most significant byte first.
                let packed = r.get(RB_COLOR_CLEAR);
                let value = vk::ClearColorValue {
                    float32: [
                        ((packed >> 16) & 0xFF) as f32 / 255.0,
                        ((packed >> 8) & 0xFF) as f32 / 255.0,
                        (packed & 0xFF) as f32 / 255.0,
                        ((packed >> 24) & 0xFF) as f32 / 255.0,
                    ],
                };
                let clears = &mut self.clears;
                let target = self.slab[id as usize].as_deref_mut().expect("a live target's slot");
                match rectangle.within(target) {
                    Within::Nothing => clears.outside += 1,
                    Within::Whole => {
                        clears.whole += 1;
                        record_use(
                            target.image,
                            vk::ImageAspectFlags::COLOR,
                            &mut target.track,
                            tracked::COPIED_TO,
                            true,
                        );
                        let image = target.image;
                        recorder::command(move |c| {
                            // SAFETY: the recorder's command buffer; the image lives until a retire.
                            unsafe {
                                c.vk.cmd_clear_color_image(
                                    c.into,
                                    image,
                                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                                    &value,
                                    &[range(vk::ImageAspectFlags::COLOR)],
                                )
                            }
                        });
                    }
                    Within::Part(area) => {
                        clears.part += 1;
                        clear_area(target, area, vk::ClearValue { color: value });
                    }
                }
            }
        }

        if clear.depth {
            let surface = SurfaceInfo(r.get(RB_SURFACE_INFO));
            let depth_info = DepthInfo(r.get(RB_DEPTH_INFO));
            let format = self.depth_format_for(depth_info.format()).as_raw() as u32;
            let scale = self.pass_scale(r.get(RB_DEPTH_INFO), surface.msaa_samples());
            let key = self.depth_key(depth_info.base_tile(), format, surface.msaa_samples(), scale);
            if let Some(id) = self.find(&key) {
                // The stored value is 24-bit depth with stencil in the low byte.
                let packed = r.get(RB_DEPTH_CLEAR);
                let value = vk::ClearDepthStencilValue {
                    depth: depth_clear_value(depth_info.format(), packed),
                    stencil: packed & 0xFF,
                };
                let clears = &mut self.clears;
                let target = self.slab[id as usize].as_deref_mut().expect("a live target's slot");
                match rectangle.within(target) {
                    Within::Nothing => clears.outside += 1,
                    Within::Whole => {
                        clears.whole += 1;
                        record_use(target.image, DEPTH_ASPECTS, &mut target.track, tracked::COPIED_TO, true);
                        let image = target.image;
                        recorder::command(move |c| {
                            // SAFETY: the recorder's command buffer; the image lives until a retire.
                            unsafe {
                                c.vk.cmd_clear_depth_stencil_image(
                                    c.into,
                                    image,
                                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                                    &value,
                                    &[range(DEPTH_ASPECTS)],
                                )
                            }
                        });
                    }
                    Within::Part(area) => {
                        clears.part += 1;
                        // All 16 bytes said: the trace reads the union whole.
                        let mut clear = vk::ClearValue { color: vk::ClearColorValue { uint32: [0; 4] } };
                        clear.depth_stencil = value;
                        clear_area(target, area, clear);
                    }
                }
            }
        }
    }

    /// What a copy out of a colour target reads, in TRANSFER_SRC: the target itself, or
    /// for a multisampled one its twin, the rectangle resolved into it. Null when the twin
    /// cannot be made. Outside any render pass. The target is left there: its next use
    /// brings it on (tracked.rs).
    ///
    /// # Safety
    /// `vk` is the renderer's live device.
    pub unsafe fn begin_reading_colour(
        &mut self,
        vk: &Vk,
        id: u32,
        at: vk::Offset2D,
        width: u32,
        height: u32,
    ) -> vk::Image {
        let memory = self.settings.memory;
        let target = self.get_mut(id);
        if target.samples == vk::SampleCountFlags::TYPE_1 {
            record_use(target.image, vk::ImageAspectFlags::COLOR, &mut target.track, tracked::COPIED_FROM, true);
            return target.image;
        }
        // SAFETY: the caller's.
        if !unsafe { ensure_single(vk, &memory, target) } {
            return vk::Image::null();
        }
        // Its contents not kept: only the rectangle is read afterwards. After the copies
        // out of the twin's last contents.
        let single = target.single_image;
        record_use(single, vk::ImageAspectFlags::COLOR, &mut target.single_track, tracked::COPIED_TO, false);
        resolve_colour_into(target, at, width, height, single);
        record_use(single, vk::ImageAspectFlags::COLOR, &mut target.single_track, tracked::COPIED_FROM, true);
        single
    }

    /// The same for a depth target, whose rectangle is resolved into the twin by
    /// resolve_depth_into, which leaves it in TRANSFER_SRC.
    ///
    /// # Safety
    /// `vk` is the renderer's live device.
    pub unsafe fn begin_reading_depth<F: Frame>(
        &mut self,
        vk: &Vk,
        f: &mut F,
        id: u32,
        at: vk::Offset2D,
        width: u32,
        height: u32,
    ) -> vk::Image {
        let memory = self.settings.memory;
        let target = self.get_mut(id);
        if target.samples == vk::SampleCountFlags::TYPE_1 {
            record_use(target.image, DEPTH_ASPECTS, &mut target.track, tracked::COPIED_FROM, true);
            return target.image;
        }
        // SAFETY: the caller's.
        if !unsafe { ensure_single(vk, &memory, target) } {
            return vk::Image::null();
        }
        let single_image = target.single_image;
        // SAFETY: the caller's.
        unsafe { self.resolve_depth_into(vk, f, id, at, width, height, DepthInto::Twin, false, tracked::COPIED_FROM) };
        single_image
    }

    /// A multisampled target's rectangle resolved into `into` at the same place
    /// (resolve_colour_into's twin for depth): by a draw that fetches sample 0 wherever
    /// the target has its set (draw_depth_resolve), else by a rendering instance with
    /// nothing drawn in it whose resolve takes sample 0 of the depth and of the stencil
    /// attached with it: Intel's Windows driver, asked for the depth alone, resolved no
    /// depth either (issue #37), and both at the first sample is what every 1.2 device
    /// must offer. `into` keeps its contents
    /// outside the rectangle or not, and is left in `after`; the target is left where the
    /// resolve used it, for its next use to bring on (tracked.rs).
    ///
    /// # Safety
    /// `vk` is the renderer's live device; a copy's image and view are its, the view one
    /// of both aspects of the image.
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn resolve_depth_into<F: Frame>(
        &mut self,
        vk: &Vk,
        f: &mut F,
        id: u32,
        at: vk::Offset2D,
        width: u32,
        height: u32,
        mut into: DepthInto<'_>,
        keep: bool,
        after: TrackedImage,
    ) {
        if self.get(id).sampled_set != vk::DescriptorSet::null()
            // SAFETY: the caller's.
            && unsafe { self.draw_depth_resolve(vk, f, id, at, width, height, &mut into, keep, after) }
        {
            return;
        }
        let (into_image, into_view) = self.handles_of(id, &into);
        let target = self.get_mut(id);
        let view = target.view;
        let area = vk::Rect2D {
            offset: scaled_offset(at, target.scale),
            extent: scaled_extent(vk::Extent2D { width, height }, target.scale),
        };
        let surface = target.image;
        let from = target.track.enter(tracked::DEPTH_RESOLVED_FROM, true).map(|t| (surface, DEPTH_ASPECTS, t));
        let entering = self
            .track_of(id, &mut into)
            .enter(tracked::DEPTH_RESOLVED_TO, keep)
            .map(|t| (into_image, DEPTH_ASPECTS, t));
        let leaving = self.track_of(id, &mut into).enter(after, true).map(|t| (into_image, DEPTH_ASPECTS, t));
        recorder::command(move |c| {
            // SAFETY: the recorder's command buffer on the renderer's device; the images,
            // the views and `into` live until a RetireBeforeDestroy that waits for this call.
            unsafe {
                transitions_now(c, [from, entering]);
                let depth = vk::RenderingAttachmentInfo {
                    image_view: view,
                    image_layout: vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
                    resolve_mode: vk::ResolveModeFlags::SAMPLE_ZERO,
                    resolve_image_view: into_view,
                    resolve_image_layout: vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
                    load_op: vk::AttachmentLoadOp::LOAD,
                    store_op: vk::AttachmentStoreOp::STORE,
                    ..Default::default()
                };
                let info = vk::RenderingInfo {
                    render_area: area,
                    layer_count: 1,
                    p_depth_attachment: &depth,
                    p_stencil_attachment: &depth,
                    ..Default::default()
                };
                c.vk.cmd_begin_rendering(c.into, &info);
                c.vk.cmd_end_rendering(c.into);
                transitions_now(c, [leaving, None]);
            }
        });
    }

    /// The image and the attachment view a depth resolve writes.
    fn handles_of(&self, id: u32, into: &DepthInto<'_>) -> (vk::Image, vk::ImageView) {
        match into {
            DepthInto::Twin => {
                let target = self.get(id);
                (target.single_image, target.single_view)
            }
            DepthInto::Copy { image, view, .. } => (*image, *view),
        }
    }

    /// What it was last used for.
    fn track_of<'a>(&'a mut self, id: u32, into: &'a mut DepthInto<'_>) -> &'a mut TrackedImage {
        match into {
            DepthInto::Twin => &mut self.get_mut(id).single_track,
            DepthInto::Copy { track, .. } => track,
        }
    }

    /// resolve_depth_into's work done by a draw: a triangle over the rectangle whose
    /// fragment shader fetches sample 0 at its own pixel and writes it as its depth, the
    /// test ALWAYS with writes, the stencil not kept. False where the pipeline is
    /// missing, and nothing recorded. Afterwards the draws' sets 1 and 2 are bound again
    /// and `bound` is reset: the pipeline, viewport and scissor are this draw's now.
    ///
    /// # Safety
    /// As resolve_depth_into's.
    #[allow(clippy::too_many_arguments)]
    unsafe fn draw_depth_resolve<F: Frame>(
        &mut self,
        vk: &Vk,
        f: &mut F,
        id: u32,
        at: vk::Offset2D,
        width: u32,
        height: u32,
        into: &mut DepthInto<'_>,
        keep: bool,
        after: TrackedImage,
    ) -> bool {
        let format = self.get(id).format;
        // SAFETY: the caller's.
        let pipeline = unsafe { drawn_pipeline(vk, &mut self.drawn, format) };
        if pipeline == vk::Pipeline::null() {
            return false;
        }
        let (into_image, into_view) = self.handles_of(id, into);
        let layout = self.drawn.layout;
        let target = self.get_mut(id);
        let area = vk::Rect2D {
            offset: scaled_offset(at, target.scale),
            extent: scaled_extent(vk::Extent2D { width, height }, target.scale),
        };
        let surface = target.image;
        let set = target.sampled_set;
        // The surface, written by the draws before, read by this one's fragment shader;
        // `into` written as depth.
        let fetched = target.track.enter(tracked::DEPTH_FETCHED, true).map(|t| (surface, DEPTH_ASPECTS, t));
        let entering = self.track_of(id, into).enter(tracked::DEPTH_DRAWN, keep);
        // Out of UNDEFINED, what was there is not loaded.
        let discarded = entering.is_none_or(|t| t.from == vk::ImageLayout::UNDEFINED);
        let entering = entering.map(|t| (into_image, DEPTH_ASPECTS, t));
        let leaving = self.track_of(id, into).enter(after, true).map(|t| (into_image, DEPTH_ASPECTS, t));
        let main_layout = f.pipeline_layout();
        let main_sets = [f.texture_set(), f.memory_set()];
        recorder::command(move |c| {
            // SAFETY: the recorder's command buffer on the renderer's device; the surface,
            // `into`, the set and both layouts live until a RetireBeforeDestroy that waits
            // for this call (the drawn resolve's objects until shutdown).
            unsafe {
                transitions_now(c, [fetched, entering]);
                let depth = vk::RenderingAttachmentInfo {
                    image_view: into_view,
                    image_layout: vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
                    load_op: if discarded { vk::AttachmentLoadOp::DONT_CARE } else { vk::AttachmentLoadOp::LOAD },
                    store_op: vk::AttachmentStoreOp::STORE,
                    ..Default::default()
                };
                // Nothing reads a resolved stencil.
                let stencil = vk::RenderingAttachmentInfo {
                    load_op: vk::AttachmentLoadOp::DONT_CARE,
                    store_op: vk::AttachmentStoreOp::DONT_CARE,
                    ..depth
                };
                let info = vk::RenderingInfo {
                    render_area: area,
                    layer_count: 1,
                    p_depth_attachment: &depth,
                    p_stencil_attachment: &stencil,
                    ..Default::default()
                };
                c.vk.cmd_begin_rendering(c.into, &info);
                c.vk.cmd_bind_pipeline(c.into, vk::PipelineBindPoint::GRAPHICS, pipeline);
                c.vk.cmd_bind_descriptor_sets(c.into, vk::PipelineBindPoint::GRAPHICS, layout, 0, &[set], &[]);
                let viewport = vk::Viewport {
                    x: area.offset.x as f32,
                    y: area.offset.y as f32,
                    width: area.extent.width as f32,
                    height: area.extent.height as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                };
                c.vk.cmd_set_viewport(c.into, 0, &[viewport]);
                c.vk.cmd_set_scissor(c.into, 0, &[area]);
                c.vk.cmd_draw(c.into, 3, 1, 0, 0);
                c.vk.cmd_end_rendering(c.into);
                transitions_now(c, [leaving, None]);
                // The draws' own sets, which binding another layout's set 0 disturbed.
                c.vk.cmd_bind_descriptor_sets(
                    c.into,
                    vk::PipelineBindPoint::GRAPHICS,
                    main_layout,
                    TEXTURE_SET,
                    &main_sets,
                    &[],
                );
            }
        });
        // The pipeline, viewport and scissor are this draw's now.
        f.reset_bound();
        true
    }

    /// Every target destroyed and the map emptied, then
    /// the drawn resolve's objects: after every target's sets are freed.
    ///
    /// # Safety
    /// `vk` is the renderer's device, idle.
    pub unsafe fn destroy_all(&mut self, vk: &Vk) {
        let ids: Vec<u32> = self.map.values().copied().collect();
        for id in ids {
            let mut target = *self.get(id);
            // SAFETY: the caller's.
            unsafe { destroy_target(vk, &mut self.drawn, &mut target) };
        }
        self.map.clear();
        self.slab.clear();
        self.free.clear();
        self.last = [NO_LAST; 2];
        // SAFETY: the caller's.
        unsafe { self.destroy_drawn_resolve(vk) };
    }

    /// The drawn resolve's pipelines, pool and layouts (DestroyDrawnResolve): after every
    /// target's forget_sampled_depth.
    ///
    /// # Safety
    /// `vk` is the renderer's device, idle.
    pub unsafe fn destroy_drawn_resolve(&mut self, vk: &Vk) {
        // SAFETY: the caller's; each handle was made here and nothing uses it.
        unsafe {
            for &pipeline in self.drawn.pipelines.values() {
                if pipeline != vk::Pipeline::null() {
                    vk.destroy_pipeline(pipeline);
                }
            }
            if self.drawn.pool != vk::DescriptorPool::null() {
                vk.destroy_descriptor_pool(self.drawn.pool);
            }
            if self.drawn.layout != vk::PipelineLayout::null() {
                vk.destroy_pipeline_layout(self.drawn.layout);
            }
            if self.drawn.set_layout != vk::DescriptorSetLayout::null() {
                vk.destroy_descriptor_set_layout(self.drawn.set_layout);
            }
        }
        self.drawn = DrawnResolve::new();
    }
}

/// The target's image at its size and scale, its memory and, when `aspect` asks for one,
/// its view (MakeImage). False at the first failure, with what was made left in the
/// target.
///
/// # Safety
/// `vk` is the renderer's live device; `memory` its properties.
pub unsafe fn make_image(
    vk: &Vk,
    memory: &vk::PhysicalDeviceMemoryProperties,
    target: &mut Target,
    usage: vk::ImageUsageFlags,
    aspect: vk::ImageAspectFlags,
) -> bool {
    let info = vk::ImageCreateInfo {
        image_type: vk::ImageType::TYPE_2D,
        format: target.format,
        extent: vk::Extent3D {
            width: target.width.wrapping_mul(target.scale),
            height: target.height.wrapping_mul(target.scale),
            depth: 1,
        },
        mip_levels: 1,
        array_layers: 1,
        samples: target.samples,
        tiling: vk::ImageTiling::OPTIMAL,
        usage,
        sharing_mode: vk::SharingMode::EXCLUSIVE,
        initial_layout: vk::ImageLayout::UNDEFINED,
        ..Default::default()
    };
    // SAFETY: the caller's; every handle is made here.
    unsafe {
        if vk.create_image(&info, &mut target.image) != vk::Result::SUCCESS {
            return false;
        }
        let needs = vk.get_image_memory_requirements(target.image);
        let kind = util::find_memory(memory, needs.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL);
        if kind == u32::MAX {
            return false;
        }
        let allocate =
            vk::MemoryAllocateInfo { allocation_size: needs.size, memory_type_index: kind, ..Default::default() };
        if vk.allocate_memory(&allocate, &mut target.memory) != vk::Result::SUCCESS {
            return false;
        }
        let _ = vk.bind_image_memory(target.image, target.memory, 0);

        if !aspect.is_empty() {
            let view = vk::ImageViewCreateInfo {
                image: target.image,
                view_type: vk::ImageViewType::TYPE_2D,
                format: target.format,
                subresource_range: range(aspect),
                ..Default::default()
            };
            if vk.create_image_view(&view, &mut target.view) != vk::Result::SUCCESS {
                return false;
            }
        }
    }
    true
}

/// The single-sampled twin's objects (DestroySingle).
///
/// # Safety
/// `vk` is the device that made them; nothing uses them any more.
unsafe fn destroy_single(vk: &Vk, target: &mut Target) {
    // SAFETY: the caller's.
    unsafe {
        if target.single_view != vk::ImageView::null() {
            vk.destroy_image_view(target.single_view);
        }
        if target.single_image != vk::Image::null() {
            vk.destroy_image(target.single_image);
        }
        if target.single_memory != vk::DeviceMemory::null() {
            vk.free_memory(target.single_memory);
        }
    }
    target.single_view = vk::ImageView::null();
    target.single_image = vk::Image::null();
    target.single_memory = vk::DeviceMemory::null();
}

/// A target's objects, its sampled view and set first (DestroyTarget).
///
/// # Safety
/// `vk` is the device that made them; nothing uses them any more.
unsafe fn destroy_target(vk: &Vk, drawn: &mut DrawnResolve, target: &mut Target) {
    // SAFETY: the caller's.
    unsafe {
        forget_sampled_depth(vk, drawn, target);
        if target.view != vk::ImageView::null() {
            vk.destroy_image_view(target.view);
        }
        if target.image != vk::Image::null() {
            vk.destroy_image(target.image);
        }
        if target.memory != vk::DeviceMemory::null() {
            vk.free_memory(target.memory);
        }
        destroy_single(vk, target);
    }
}

/// The single-sampled twin of a multisampled target, made on first use (EnsureSingle). A
/// colour twin is a transfer destination the resolve fills; a depth twin is an
/// attachment, because a depth resolve only happens inside a render pass.
///
/// # Safety
/// `vk` is the renderer's live device; `memory` its properties.
unsafe fn ensure_single(vk: &Vk, memory: &vk::PhysicalDeviceMemoryProperties, target: &mut Target) -> bool {
    if target.single_image != vk::Image::null() {
        return true;
    }
    let mut one = *target;
    one.samples = vk::SampleCountFlags::TYPE_1;
    one.image = vk::Image::null();
    one.memory = vk::DeviceMemory::null();
    one.view = vk::ImageView::null();
    let usage = if target.depth {
        vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC
    } else {
        vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::TRANSFER_SRC
    };
    // A view only for the depth twin, which is bound as an attachment; a transfer-only
    // image may not have one.
    let aspect = if target.depth { DEPTH_ASPECTS } else { vk::ImageAspectFlags::empty() };
    // SAFETY: the caller's.
    unsafe {
        if !make_image(vk, memory, &mut one, usage, aspect) {
            if one.view != vk::ImageView::null() {
                vk.destroy_image_view(one.view);
            }
            if one.image != vk::Image::null() {
                vk.destroy_image(one.image);
            }
            if one.memory != vk::DeviceMemory::null() {
                vk.free_memory(one.memory);
            }
            return false;
        }
    }
    target.single_image = one.image;
    target.single_memory = one.memory;
    target.single_view = one.view;
    true
}

/// The rectangle of a multisampled colour target resolved into `into`, an image of its
/// format in TRANSFER_DST, at the same place (ResolveColourInto): by vkCmdResolveImage,
/// the target an attachment again afterwards. Outside any render pass.
pub fn resolve_colour_into(target: &mut Target, at: vk::Offset2D, width: u32, height: u32, into: vk::Image) {
    record_use(target.image, vk::ImageAspectFlags::COLOR, &mut target.track, tracked::COPIED_FROM, true);
    let offset = scaled3(at.x, at.y, 0, target.scale);
    let region = vk::ImageResolve {
        src_subresource: layers(vk::ImageAspectFlags::COLOR),
        src_offset: offset,
        dst_subresource: layers(vk::ImageAspectFlags::COLOR),
        dst_offset: offset,
        extent: vk::Extent3D {
            width: width.wrapping_mul(target.scale),
            height: height.wrapping_mul(target.scale),
            depth: 1,
        },
    };
    let from = target.image;
    recorder::command(move |c| {
        // SAFETY: the recorder's command buffer; both images live until a retire.
        unsafe {
            c.vk.cmd_resolve_image(
                c.into,
                from,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                into,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[region],
            )
        }
    });
}

/// The clears after a resolve, counted for the report (S14d).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct ResolveClears {
    /// A target cleared whole: the rectangle covers the image.
    pub whole: u64,
    /// A target cleared in the rectangle alone, the rest of the image kept.
    pub part: u64,
    /// The rectangle lay outside the target's image.
    pub outside: u64,
    /// No rectangle by Xenia's rule: empty, or a resolve it refuses (a pitch past 8192
    /// pixels, or wider at its scale than the device's widest target).
    pub nothing: u64,
    /// A colour clear asked of a resolve copying depth, which Xenia does not make.
    pub colour_of_depth_resolve: u64,
    /// A clear not made because Xenia's tile range for it came out empty: it starts past
    /// EDRAM's last tile, or the other target's clear starts where it does.
    pub no_tiles: u64,
}

impl ResolveClears {
    /// Whether any resolve asked for a clear.
    pub fn any(&self) -> bool {
        self.whole != 0
            || self.part != 0
            || self.outside != 0
            || self.nothing != 0
            || self.colour_of_depth_resolve != 0
            || self.no_tiles != 0
    }
}

/// Xenia's IsCopyingDepth: a source select past the four colour targets.
#[inline]
fn copies_depth(control: &CopyControl) -> bool {
    control.source_select() >= 4
}

/// What a resolve clears, in the title's pixels of the surface the registers name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ClearRectangle {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Where a clear rectangle falls in one target's image.
#[derive(Clone, Copy)]
pub enum Within {
    Nothing,
    Whole,
    /// The scaled area, a part of the image.
    Part(vk::Rect2D),
}

impl ClearRectangle {
    /// The rectangle in `target`'s image: clipped to it, which the image's own extent
    /// asks (Xenia's render target is the EDRAM's rows; the image holds what was drawn).
    pub fn within(&self, target: &Target) -> Within {
        let x = self.x.min(target.width);
        let y = self.y.min(target.height);
        let width = self.width.min(target.width - x);
        let height = self.height.min(target.height - y);
        if width == 0 || height == 0 {
            return Within::Nothing;
        }
        if x == 0 && y == 0 && width == target.width && height == target.height {
            return Within::Whole;
        }
        Within::Part(vk::Rect2D {
            offset: scaled_offset(vk::Offset2D { x: x as i32, y: y as i32 }, target.scale),
            extent: scaled_extent(vk::Extent2D { width, height }, target.scale),
        })
    }
}

// Xenia's constants (xenos.h): an EDRAM tile is 80x16 samples and there are 2048 of them;
// a resolve's rectangle is aligned to 8 pixels and at most 2^14 - 8 tall; a render target
// is at most 8192 rows.
const EDRAM_TILE_WIDTH_SAMPLES: u32 = 80;
const EDRAM_TILE_HEIGHT_SAMPLES: u32 = 16;
const EDRAM_TILE_COUNT: u32 = 2048;
const RESOLVE_ALIGNMENT: u32 = 8;
const MAX_RESOLVE_SIZE: u32 = (1 << 14) - RESOLVE_ALIGNMENT;
const MAX_TARGET_SIZE: u32 = 8192;

/// What a resolve clears: the rectangle, and whether Xenia's tile range for each target
/// asked is not empty.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ResolveClear {
    pub rectangle: ClearRectangle,
    /// The surface's pitch in pixels, unscaled, for the device-width refusal the caller
    /// makes at its scale.
    pub pitch_pixels: u32,
    pub depth: bool,
    pub colour: bool,
}

/// What a resolve clears, by Xenia's rule (xenia-canary e6cd2e4): draw_util.cc
/// GetResolveInfo's region -- the corners aligned out to 8, x clamped to the surface pitch
/// aligned down to 8, the height to kMaxResolveSize, empty a no-op -- and its EDRAM
/// coordinates (edram_offset_x/y_div_8, the base offset in tiles and the 11-bit base
/// fields; the colour's of `colour`, the RB_COLOR_INFO the source select names, None where
/// colour is not cleared), turned back into pixels by render_target_cache.cc
/// PrepareHostRenderTargetsResolveClear: from depth's EDRAM info when depth is cleared,
/// else colour's (a 64bpp surface's tiles halved), clamped to the pitch in pixels and to
/// GetRenderTargetHeight, None where it is empty or the surface is one Xenia refuses
/// (over 4x MSAA, no pitch, a pitch past 8192 pixels; its third refusal, a pitch at the
/// resolution scale wider than the device's widest render target, needs the device and
/// is clear_after_resolve's). Then each target's tile range: the
/// rectangle's tiles from its first, the start clamped to kEdramTileCount (colour's
/// doubled for 64bpp first), and where both are cleared each stopped where the other
/// starts in the 11-bit tile space; a target whose range is empty is not cleared. The
/// region is the renderer's own resolve rectangle, the window scissor its copy reads
/// (renderer_resolve.rs), where Xenia's is resolve vertex buffer 0's corners clamped to
/// that scissor.
pub fn resolve_clear(r: &RegisterFile, clears_depth: bool, colour: Option<u32>) -> Option<ResolveClear> {
    let region = super::renderer_draw::scissor_of(r);
    let surface = SurfaceInfo(r.get(RB_SURFACE_INFO));
    let msaa = surface.msaa_samples();
    // GetResolveInfo.
    let mut x0 = region.offset.x as u32;
    let mut y0 = region.offset.y as u32;
    let mut x1 = x0 + region.extent.width;
    let mut y1 = y0 + region.extent.height;
    x0 &= !(RESOLVE_ALIGNMENT - 1);
    y0 &= !(RESOLVE_ALIGNMENT - 1);
    x1 = x1.next_multiple_of(RESOLVE_ALIGNMENT);
    y1 = y1.next_multiple_of(RESOLVE_ALIGNMENT);
    if msaa > 2 {
        return None;
    }
    let pitch_aligned = surface.pitch() & !(RESOLVE_ALIGNMENT - 1);
    if x1 > pitch_aligned {
        x0 = x0.min(pitch_aligned);
        x1 = x1.min(pitch_aligned);
    }
    if y1 - y0 > MAX_RESOLVE_SIZE {
        y1 = y0 + MAX_RESOLVE_SIZE;
    }
    if x0 >= x1 || y0 >= y1 {
        return None;
    }
    let width_div_8 = (x1 - x0) >> 3;
    let height_div_8 = (y1 - y0) >> 3;
    let x_log2 = u32::from(msaa >= 2);
    let y_log2 = u32::from(msaa >= 1);
    let x0_samples = x0 << x_log2;
    let y0_samples = y0 << y_log2;
    let edram_offset_x_div_8 = (x0_samples % EDRAM_TILE_WIDTH_SAMPLES) >> (x_log2 + 3);
    let edram_offset_y_div_8 = (y0_samples % EDRAM_TILE_HEIGHT_SAMPLES) >> (y_log2 + 3);
    // GetSurfacePitchTiles, 32bpp.
    let surface_pitch_tiles = (surface.pitch() << x_log2).div_ceil(EDRAM_TILE_WIDTH_SAMPLES);
    let edram_base_offset_tiles =
        (y0_samples / EDRAM_TILE_HEIGHT_SAMPLES) * surface_pitch_tiles + x0_samples / EDRAM_TILE_WIDTH_SAMPLES;
    // The EDRAM info's 10-bit pitch and 11-bit base, less the register's base.
    let depth_base = r.get(RB_DEPTH_INFO) & 0x7FF;
    let colour_base = colour.map_or(0, |info| info & 0x7FF);
    // xenos.h IsColorRenderTargetFormat64bpp: 16_16_16_16, 16_16_16_16_FLOAT, 32_32_FLOAT.
    let wide = colour.map_or(0, |info| u32::from(matches!((info >> 16) & 0xF, 5 | 7 | 15)));
    let (pitch_tiles, base_offset_tiles) = if clears_depth {
        (surface_pitch_tiles & 0x3FF, ((depth_base + edram_base_offset_tiles) & 0x7FF).wrapping_sub(depth_base))
    } else if colour.is_some() {
        let pitch = (surface_pitch_tiles << wide) & 0x3FF;
        let base_tiles = (colour_base + (edram_base_offset_tiles << wide)) & 0x7FF;
        (pitch >> wide, base_tiles.wrapping_sub(colour_base) >> wide)
    } else {
        return None;
    };
    // PrepareHostRenderTargetsResolveClear.
    if pitch_tiles == 0 {
        return None;
    }
    if pitch_tiles > (MAX_TARGET_SIZE << x_log2).div_ceil(EDRAM_TILE_WIDTH_SAMPLES) {
        return None;
    }
    let pitch_pixels = pitch_tiles * (EDRAM_TILE_WIDTH_SAMPLES >> x_log2);
    // GetRenderTargetHeight: to the next 11-bit addressing period, at most 8192 rows (the
    // resolution scale cancels; the device's limit is the image's own, clipped in within).
    let tile_rows = EDRAM_TILE_COUNT.div_ceil(pitch_tiles).min((MAX_TARGET_SIZE << y_log2) / EDRAM_TILE_HEIGHT_SAMPLES);
    let target_height = tile_rows * (EDRAM_TILE_HEIGHT_SAMPLES >> y_log2);
    let rows = base_offset_tiles / pitch_tiles;
    let x = ((base_offset_tiles - rows * pitch_tiles) * (EDRAM_TILE_WIDTH_SAMPLES >> x_log2)
        + (edram_offset_x_div_8 << 3))
        .min(pitch_pixels);
    let y = rows
        .wrapping_mul(EDRAM_TILE_HEIGHT_SAMPLES >> y_log2)
        .wrapping_add(edram_offset_y_div_8 << 3)
        .min(target_height);
    let width = (width_div_8 << 3).min(pitch_pixels - x);
    let height = (height_div_8 << 3).min(target_height - y);
    if width == 0 || height == 0 {
        return None;
    }
    // The tiles from the rectangle's first to its last, at 32bpp.
    let start = ((y << y_log2) / EDRAM_TILE_HEIGHT_SAMPLES) * pitch_tiles + (x << x_log2) / EDRAM_TILE_WIDTH_SAMPLES;
    let length = (((y + height - 1) << y_log2) / EDRAM_TILE_HEIGHT_SAMPLES) * pitch_tiles
        + ((x + width - 1) << x_log2) / EDRAM_TILE_WIDTH_SAMPLES
        + 1
        - start;
    let (depth_start, mut depth_length) = if clears_depth {
        let at = start.min(EDRAM_TILE_COUNT);
        (at, (start + length).min(EDRAM_TILE_COUNT) - at)
    } else {
        (0, 0)
    };
    let (colour_start, mut colour_length) = if colour.is_some() {
        let at = (start << wide).min(EDRAM_TILE_COUNT);
        (at, ((start + length) << wide).min(EDRAM_TILE_COUNT) - at)
    } else {
        (0, 0)
    };
    if depth_length != 0 && colour_length != 0 {
        // Prevent overlap: each only until the other's start, in this or the next period.
        let d = (depth_base + depth_start) & (EDRAM_TILE_COUNT - 1);
        let c = (colour_base + colour_start) & (EDRAM_TILE_COUNT - 1);
        depth_length = depth_length.min((if c < d { EDRAM_TILE_COUNT } else { 0 }) + c - d);
        colour_length = colour_length.min((if d < c { EDRAM_TILE_COUNT } else { 0 }) + d - c);
    }
    Some(ResolveClear {
        rectangle: ClearRectangle { x, y, width, height },
        pitch_pixels,
        depth: depth_length != 0,
        colour: colour_length != 0,
    })
}

/// `area` of `target`'s image cleared to `value` and the rest kept: a rendering instance
/// of that area whose load clears it (the attachment brought from its last use, kept).
fn clear_area(target: &mut Target, area: vk::Rect2D, value: vk::ClearValue) {
    let (aspect, used) = if target.depth {
        (DEPTH_ASPECTS, tracked::DEPTH_DRAWN)
    } else {
        (vk::ImageAspectFlags::COLOR, tracked::COLOUR_DRAWN)
    };
    let into = target.track.enter(used, true).map(|t| (target.image, aspect, t));
    let view = target.view;
    let depth = target.depth;
    recorder::command(move |c| {
        // SAFETY: the recorder's command buffer on the renderer's device; the image and its
        // view live until a RetireBeforeDestroy that waits for this call.
        unsafe {
            transitions_now(c, [into, None]);
            let attachment = vk::RenderingAttachmentInfo {
                image_view: view,
                image_layout: if depth {
                    vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL
                } else {
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL
                },
                load_op: vk::AttachmentLoadOp::CLEAR,
                store_op: vk::AttachmentStoreOp::STORE,
                clear_value: value,
                ..Default::default()
            };
            let mut info = vk::RenderingInfo { render_area: area, layer_count: 1, ..Default::default() };
            if depth {
                info.p_depth_attachment = &attachment;
                info.p_stencil_attachment = &attachment;
            } else {
                info.color_attachment_count = 1;
                info.p_color_attachments = &attachment;
            }
            c.vk.cmd_begin_rendering(c.into, &info);
            c.vk.cmd_end_rendering(c.into);
        }
    });
}

/// One-shot, on the set-up command buffer: a new target leaves UNDEFINED and is cleared
/// before any frame's render pass loads it -- depth to 0 and stencil 0 (a level tests
/// GEQUAL, and RB_DEPTH_CLEAR is zero), colour to (0.06, 0.07, 0.10, 1), not black, so a
/// target the title has not drawn into reads apart from an empty resolve. Submitted and
/// waited for under the queue lock (ClearNewTarget). Left in TRANSFER_DST: the first
/// rendering instance over it brings it on (tracked.rs).
///
/// # Safety
/// `vk` is the renderer's live device; the target's image is its.
unsafe fn clear_new_target<F: Frame>(vk: &Vk, f: &mut F, target: &mut Target) {
    let setup = f.setup();
    let begin =
        vk::CommandBufferBeginInfo { flags: vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT, ..Default::default() };
    let command = setup.command;
    // SAFETY: the caller's; the set-up command buffer is the consumer's, and the queue is
    // held under its lock.
    unsafe {
        if vk.begin_command_buffer(command, &begin) != vk::Result::SUCCESS {
            return;
        }

        let aspect = if target.depth { DEPTH_ASPECTS } else { vk::ImageAspectFlags::COLOR };
        if let Some(transition) = target.track.enter(tracked::COPIED_TO, false) {
            util::renderer_transition(vk, command, target.image, aspect, &transition);
        }
        if target.depth {
            let value = vk::ClearDepthStencilValue { depth: 0.0, stencil: 0 };
            vk.cmd_clear_depth_stencil_image(
                command,
                target.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &value,
                &[range(aspect)],
            );
        } else {
            let value = vk::ClearColorValue { float32: [0.06, 0.07, 0.10, 1.0] };
            vk.cmd_clear_color_image(
                command,
                target.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &value,
                &[range(aspect)],
            );
        }
        let _ = vk.end_command_buffer(command);

        let submit = vk::SubmitInfo { command_buffer_count: 1, p_command_buffers: &command, ..Default::default() };
        let _lock = super::QueueLock::hold();
        let _ = vk.reset_fences(&[setup.fence]);
        let _ = vk.queue_submit(setup.queue, &[submit], setup.fence);
        super::failed(vk.wait_for_fences(&[setup.fence], true, u64::MAX), c"the set-up fence");
    }
}

// ---- the depth resolve as a draw: its layouts, pool, pipelines and sets ----------------

/// The set and pipeline layouts and the pool of sets, once; a failure remembered
/// (MakeDrawnLayout). A set per multisampled depth target: 64, or MW2_DRAWN_RESOLVE_SETS
/// in a diagnostics build, to run the pool dry.
///
/// # Safety
/// `vk` is the renderer's live device.
unsafe fn make_drawn_layout(vk: &Vk, drawn: &mut DrawnResolve) -> bool {
    if drawn.layout != vk::PipelineLayout::null() {
        return true;
    }
    if drawn.failed {
        return false;
    }
    drawn.failed = true;
    let binding = vk::DescriptorSetLayoutBinding {
        binding: 0,
        descriptor_type: vk::DescriptorType::SAMPLED_IMAGE,
        descriptor_count: 1,
        stage_flags: vk::ShaderStageFlags::FRAGMENT,
        ..Default::default()
    };
    let set_info = vk::DescriptorSetLayoutCreateInfo { binding_count: 1, p_bindings: &binding, ..Default::default() };
    // SAFETY: the caller's; every handle is made here.
    unsafe {
        if vk.create_descriptor_set_layout(&set_info, &mut drawn.set_layout) != vk::Result::SUCCESS {
            return false;
        }
        let layout_info = vk::PipelineLayoutCreateInfo {
            set_layout_count: 1,
            p_set_layouts: &drawn.set_layout,
            ..Default::default()
        };
        if vk.create_pipeline_layout(&layout_info, &mut drawn.layout) != vk::Result::SUCCESS {
            return false;
        }
    }
    #[cfg(feature = "diagnostics")]
    let sets = {
        let forced = super::trace::forcing().drawn_resolve_sets;
        // MW2_DUMP_SHADERS: the draw's two shaders.
        super::trace::dump_builtin("drawn_resolve_cover_vertex", &cover_vertex_shader());
        super::trace::dump_builtin("drawn_resolve_fetch_depth", &fetch_depth_shader());
        if forced != 0 { forced } else { 64 }
    };
    #[cfg(not(feature = "diagnostics"))]
    let sets = 64u32;
    let size = vk::DescriptorPoolSize { ty: vk::DescriptorType::SAMPLED_IMAGE, descriptor_count: sets };
    let pool_info = vk::DescriptorPoolCreateInfo {
        flags: vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET,
        max_sets: sets,
        pool_size_count: 1,
        p_pool_sizes: &size,
        ..Default::default()
    };
    // SAFETY: the caller's.
    if unsafe { vk.create_descriptor_pool(&pool_info, &mut drawn.pool) } != vk::Result::SUCCESS {
        return false;
    }
    drawn.failed = false;
    true
}

/// A module of `words`, null when the driver refuses it.
///
/// # Safety
/// `vk` is the renderer's live device.
unsafe fn shader_module(vk: &Vk, words: &[u32]) -> vk::ShaderModule {
    let info = vk::ShaderModuleCreateInfo { code_size: words.len() * 4, p_code: words.as_ptr(), ..Default::default() };
    let mut module = vk::ShaderModule::null();
    // SAFETY: the caller's.
    let _ = unsafe { vk.create_shader_module(&info, &mut module) };
    module
}

/// The drawn resolve's pipeline for depth `format`, made with the first target of it and
/// kept, a failure (null) too (DrawnPipeline).
///
/// # Safety
/// `vk` is the renderer's live device; make_drawn_layout has run.
unsafe fn drawn_pipeline(vk: &Vk, drawn: &mut DrawnResolve, format: vk::Format) -> vk::Pipeline {
    if let Some(&pipeline) = drawn.pipelines.get(&format.as_raw()) {
        return pipeline;
    }
    drawn.pipelines.insert(format.as_raw(), vk::Pipeline::null());
    // SAFETY: the caller's.
    let vertex = unsafe { shader_module(vk, &cover_vertex_shader()) };
    // SAFETY: the caller's.
    let fragment = unsafe { shader_module(vk, &fetch_depth_shader()) };
    let stages: [vk::PipelineShaderStageCreateInfo; 2] = std::array::from_fn(|i| vk::PipelineShaderStageCreateInfo {
        stage: if i != 0 { vk::ShaderStageFlags::FRAGMENT } else { vk::ShaderStageFlags::VERTEX },
        module: if i != 0 { fragment } else { vertex },
        p_name: c"main".as_ptr(),
        ..Default::default()
    });
    let input = vk::PipelineVertexInputStateCreateInfo::default();
    let assembly = vk::PipelineInputAssemblyStateCreateInfo {
        topology: vk::PrimitiveTopology::TRIANGLE_LIST,
        ..Default::default()
    };
    let viewport = vk::PipelineViewportStateCreateInfo { viewport_count: 1, scissor_count: 1, ..Default::default() };
    let raster = vk::PipelineRasterizationStateCreateInfo {
        polygon_mode: vk::PolygonMode::FILL,
        cull_mode: vk::CullModeFlags::NONE,
        line_width: 1.0,
        ..Default::default()
    };
    let multisample = vk::PipelineMultisampleStateCreateInfo {
        rasterization_samples: vk::SampleCountFlags::TYPE_1,
        ..Default::default()
    };
    let depth = vk::PipelineDepthStencilStateCreateInfo {
        depth_test_enable: vk::TRUE,
        depth_write_enable: vk::TRUE,
        depth_compare_op: vk::CompareOp::ALWAYS,
        ..Default::default()
    };
    let blend = vk::PipelineColorBlendStateCreateInfo::default();
    let dynamics = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic = vk::PipelineDynamicStateCreateInfo {
        dynamic_state_count: 2,
        p_dynamic_states: dynamics.as_ptr(),
        ..Default::default()
    };
    let rendering = vk::PipelineRenderingCreateInfo {
        depth_attachment_format: format,
        stencil_attachment_format: format,
        ..Default::default()
    };
    let info = vk::GraphicsPipelineCreateInfo {
        p_next: (&rendering as *const vk::PipelineRenderingCreateInfo).cast(),
        stage_count: 2,
        p_stages: stages.as_ptr(),
        p_vertex_input_state: &input,
        p_input_assembly_state: &assembly,
        p_viewport_state: &viewport,
        p_rasterization_state: &raster,
        p_multisample_state: &multisample,
        p_depth_stencil_state: &depth,
        p_color_blend_state: &blend,
        p_dynamic_state: &dynamic,
        layout: drawn.layout,
        ..Default::default()
    };
    let mut made = [vk::Pipeline::null()];
    let made_ok = vertex != vk::ShaderModule::null()
        && fragment != vk::ShaderModule::null()
        // SAFETY: the caller's; every structure above lives across the call.
        && unsafe { vk.create_graphics_pipelines(vk::PipelineCache::null(), &[info], &mut made) } == vk::Result::SUCCESS;
    let pipeline = if made_ok {
        made[0]
    } else {
        log_warn!("renderer: the depth resolve's draw pipeline could not be made; resolving as before");
        vk::Pipeline::null()
    };
    drawn.pipelines.insert(format.as_raw(), pipeline);
    // SAFETY: the caller's; the pipeline holds what it needs of the modules.
    unsafe {
        if vertex != vk::ShaderModule::null() {
            vk.destroy_shader_module(vertex);
        }
        if fragment != vk::ShaderModule::null() {
            vk.destroy_shader_module(fragment);
        }
    }
    pipeline
}

/// A multisampled depth target's depth-only view and the set naming it, in
/// DEPTH_STENCIL_READ_ONLY; the pipeline made too, while the level loads rather than at
/// its first frame (MakeSampledDepth). Without a set -- the pool of 64 dry -- the target
/// is resolved by a rendering instance.
///
/// # Safety
/// `vk` is the renderer's live device; the target's image is its.
unsafe fn make_sampled_depth(vk: &Vk, drawn: &mut DrawnResolve, target: &mut Target) {
    // SAFETY: the caller's.
    unsafe {
        if !make_drawn_layout(vk, drawn) || drawn_pipeline(vk, drawn, target.format) == vk::Pipeline::null() {
            return;
        }
        let view = vk::ImageViewCreateInfo {
            image: target.image,
            view_type: vk::ImageViewType::TYPE_2D,
            format: target.format,
            subresource_range: range(vk::ImageAspectFlags::DEPTH),
            ..Default::default()
        };
        if vk.create_image_view(&view, &mut target.sampled_view) != vk::Result::SUCCESS {
            target.sampled_view = vk::ImageView::null();
            return;
        }
        let allocate = vk::DescriptorSetAllocateInfo {
            descriptor_pool: drawn.pool,
            descriptor_set_count: 1,
            p_set_layouts: &drawn.set_layout,
            ..Default::default()
        };
        let mut set = [vk::DescriptorSet::null()];
        if vk.allocate_descriptor_sets(&allocate, &mut set) != vk::Result::SUCCESS {
            log_warn!(
                "renderer: no descriptor set for a multisampled depth target; it is resolved by a rendering instance"
            );
            target.sampled_set = vk::DescriptorSet::null();
            return;
        }
        target.sampled_set = set[0];
        let image = vk::DescriptorImageInfo {
            sampler: vk::Sampler::null(),
            image_view: target.sampled_view,
            image_layout: vk::ImageLayout::DEPTH_STENCIL_READ_ONLY_OPTIMAL,
        };
        let write = vk::WriteDescriptorSet {
            dst_set: target.sampled_set,
            descriptor_count: 1,
            descriptor_type: vk::DescriptorType::SAMPLED_IMAGE,
            p_image_info: &image,
            ..Default::default()
        };
        vk.update_descriptor_sets(&[write], &[]);
    }
}

/// The target's set back to the pool and its sampled view gone (ForgetSampledDepth).
///
/// # Safety
/// `vk` is the device that made them; nothing uses them any more.
unsafe fn forget_sampled_depth(vk: &Vk, drawn: &DrawnResolve, target: &mut Target) {
    // SAFETY: the caller's.
    unsafe {
        if target.sampled_set != vk::DescriptorSet::null() {
            let _ = vk.free_descriptor_sets(drawn.pool, &[target.sampled_set]);
        }
        if target.sampled_view != vk::ImageView::null() {
            vk.destroy_image_view(target.sampled_view);
        }
    }
    target.sampled_set = vk::DescriptorSet::null();
    target.sampled_view = vk::ImageView::null();
}

// What the frame's diagnostics walk (S9e: renderer_diagnostics.rs's DumpTargets).
impl Targets {
    /// Every target's key and slot, in key order.
    pub fn ids(&self) -> Vec<(TargetKey, u32)> {
        self.map.iter().map(|(key, &id)| (*key, id)).collect()
    }

    /// The EDRAM clear idiom's pair (renderer_draw.rs's draw, S9e): of the colour targets
    /// at `base_tile` in host `format` over EDRAM row `row` with fewer samples than
    /// `use_samples` and drawn at the pass scale, the last in key order with
    /// fewer samples than the one before; the pass scale asked of a key only when its
    /// other tests pass, since the asking moves its memo. The
    /// walk takes each next key from the map again, so pass_scale can borrow the whole and
    /// nothing is copied out (a clear rectangle is rare and the map small).
    #[cold]
    pub fn clear_pair(
        &mut self,
        base_tile: u32,
        format: u32,
        row: u32,
        depth_info: u32,
        use_pitch: &mut u32,
        use_samples: &mut u32,
    ) {
        use std::ops::Bound::{Excluded, Unbounded};
        let mut next = self.map.keys().next().copied();
        while let Some(key) = next {
            if !key.depth
                && key.base_tile == base_tile
                && key.format == format
                && key.samples < *use_samples
                && key.pitch.wrapping_shl(key.samples) == row
                && key.scale == self.pass_scale(depth_info, key.samples)
            {
                *use_pitch = key.pitch;
                *use_samples = key.samples;
            }
            next = self.map.range((Excluded(key), Unbounded)).next().map(|(key, _)| *key);
        }
    }
}
