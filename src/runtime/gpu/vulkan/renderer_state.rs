// The renderer's state shared by its parts: the keys its maps are made of, the arena's
// kinds and per-frame caches, and what a draw's constants come back as. Plain data, safe
// code.

use super::tracked::TrackedImage;
use crate::runtime::gpu::hash::{Words, WordsBuild, words_hash};
use ash::vk;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// Per frame, reset at the resolve. A frame that runs out goes black from the point it ran
/// out, which reads as a shading bug.
pub const ARENA_WANTED: u32 = 512 << 20;
pub const ARENA_LEAST: u32 = 32 << 20;

/// Split, because the total only says a frame ran out of room, not on what.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(usize)]
pub enum ArenaKind {
    Constants = 0,
    Vertices = 1,
    Indices = 2,
}
pub const ARENA_KINDS: usize = 3;
#[cfg_attr(not(feature = "diagnostics"), allow(dead_code, reason = "the report and the diagnostic keys' alone"))]
pub const ARENA_NAMES: [&str; ARENA_KINDS] = ["constants", "vertex data", "indices"];

/// How many submissions the GPU may be holding while the CPU records the next; each needs
/// its own share of the arena. A frame is about six, and a heavy multiplayer submission
/// copies 87 MB into its share: at six slots the shares overflowed and draws were
/// dropped, at three they never do.
pub const FRAME_SLOTS: u32 = 3;

/// A tile of EDRAM is one surface however the title declares it, so the key holds the host
/// format, and the scale the image is drawn at (pass_scale). The fields' order gives the
/// derived order, on which the clear-rectangle choice depends.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct TargetKey {
    pub base_tile: u32,
    pub pitch: u32,
    pub format: u32,
    pub samples: u32,
    pub depth: bool,
    pub scale: u32,
}

/// Everything about a pipeline: the two shaders, the formats of the targets they draw into
/// (dynamic rendering: there is no render pass object), and the fixed-function register
/// bits they are drawn with. 40 bytes, none of them padding.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct PipelineKey {
    pub vertex_shader: u64,
    pub pixel_shader: u64,
    pub colour_format: u32,
    pub depth_format: u32,
    pub topology: u32,
    pub blend_control: u32,
    pub colour_mask: u32,
    pub samples: u32, // the render pass's, so the rasteriser matches it
}

const _: () = assert!(std::mem::size_of::<PipelineKey>() == 40);

impl Default for PipelineKey {
    fn default() -> Self {
        PipelineKey {
            vertex_shader: 0,
            pixel_shader: 0,
            colour_format: 0,
            depth_format: 0,
            topology: 0,
            blend_control: 0,
            colour_mask: 0,
            samples: 1,
        }
    }
}

impl Words for PipelineKey {
    const SIZE: usize = 40;
    #[inline]
    fn words(&self) -> impl Iterator<Item = u64> {
        let pair = |low: u32, high: u32| u64::from(low) | (u64::from(high) << 32);
        [
            self.vertex_shader,
            self.pixel_shader,
            pair(self.colour_format, self.depth_format),
            pair(self.topology, self.blend_control),
            pair(self.colour_mask, self.samples),
        ]
        .into_iter()
    }
}

impl Hash for PipelineKey {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(words_hash(Self::SIZE, self.words()));
    }
}

/// The six dwords of a texture fetch constant, as the texture cache's maps key them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct FetchWords(pub [u32; 6]);

impl Words for FetchWords {
    const SIZE: usize = 24;
    #[inline]
    fn words(&self) -> impl Iterator<Item = u64> {
        let w = &self.0;
        [0, 2, 4].into_iter().map(move |i| u64::from(w[i]) | (u64::from(w[i + 1]) << 32))
    }
}

impl Hash for FetchWords {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(words_hash(Self::SIZE, self.words()));
    }
}

/// The pipelines by key, hashed by words_hash.
pub type PipelineMap = HashMap<PipelineKey, vk::Pipeline, WordsBuild>;

/// What a pipeline's output stage is built from: the blend, the write mask, and the
/// targets' formats and sample count.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct OutputKey {
    pub blend_control: u32,
    pub colour_mask: u32,
    pub colour_format: u32,
    pub depth_format: u32,
    pub samples: u32,
}

/// What is already in the arena this frame, so a draw that reads the same thing as the
/// last one does not copy it again. Dropped at the reset.
#[derive(Clone, Copy, Default, Debug)]
pub struct Cached {
    pub stamp: u64,
    pub base: u64,
    pub at: u32,
    pub valid: bool,
    /// For the pixel window: the blocks of 16 the copy holds, a bit each, so a draw whose
    /// program names one it lacks is not handed it.
    pub count: u32,
}

/// Where in this slot's arena a block of 16 vertex constants was copied, at which register
/// stamp.
#[derive(Clone, Copy, Default, Debug)]
pub struct ConstantBlock {
    pub stamp: u64,
    pub valid: bool,
}

/// Every one of the five uniform blocks is a straight copy of a window of the register
/// file: the register file *is* the constant buffer. Arena offsets, in bytes.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct ConstantOffsets {
    pub vertex_floats: u32,
    pub fetch: u32,
    pub booleans: u32,
    pub loops: u32,
    pub pixel_floats: u32,
    pub ok: bool,
}

/// The depth and stencil tests RB_DEPTHCONTROL asks for, as dynamic state.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DepthTests {
    pub depth_test: vk::Bool32,
    pub depth_write: vk::Bool32,
    pub stencil_test: vk::Bool32,
    pub depth_compare: vk::CompareOp,
    pub front: vk::StencilOpState,
    pub back: vk::StencilOpState,
}

impl DepthTests {
    pub const fn new() -> Self {
        DepthTests {
            depth_test: vk::FALSE,
            depth_write: vk::FALSE,
            stencil_test: vk::FALSE,
            depth_compare: vk::CompareOp::ALWAYS,
            front: STENCIL_NONE,
            back: STENCIL_NONE,
        }
    }
}

impl Default for DepthTests {
    fn default() -> Self {
        DepthTests::new()
    }
}

const STENCIL_NONE: vk::StencilOpState = vk::StencilOpState {
    fail_op: vk::StencilOp::KEEP,
    pass_op: vk::StencilOp::KEEP,
    depth_fail_op: vk::StencilOp::KEEP,
    compare_op: vk::CompareOp::NEVER,
    compare_mask: 0,
    write_mask: 0,
    reference: 0,
};

// ---- the resolve records, shared by the texture lookup and the resolves ----

/// A resolve that is not the frame is the title handing itself a texture: the pixels never
/// go through guest memory here, the destination address names one of these images and a
/// fetch of that address binds it.
#[derive(Clone, Copy)]
pub struct Resolved {
    pub image: vk::Image,
    pub memory: vk::DeviceMemory,
    pub view: vk::ImageView,
    /// The same image seen as sRGB, for a fetch constant that asks the sampler to
    /// linearise. MW2 draws the world into 8_8_8_8_GAMMA and reads it back through a
    /// fetch whose component signs say `gamma`, the hardware undoing the encode on the
    /// way in; without this view the post chain encodes once a hop, a white band along
    /// the bottom of the screen where the near depth of field blends twice-encoded
    /// quarter-resolution buffers in.
    pub gamma_view: vk::ImageView,
    pub format: vk::Format,
    pub width: u32,
    pub height: u32,
    /// The surface's it was copied out of.
    pub scale: u32,
    /// A depth resolve is sampled through a depth-only view.
    pub depth: bool,
    /// For a depth copy out of a multisampled surface, resolved into it by a rendering
    /// instance: the image is an attachment too, through this view of both aspects.
    pub attachment_view: vk::ImageView,
    /// The surface the last colour resolve copied out of.
    pub from: TargetKey,
    /// The frame the last resolve landed in this image: the first resolve of a frame may
    /// discard what was there, a later one (the second shadow cascade) must keep it.
    pub written_frame: u64,
    /// Whether `view`'s format and `gamma_view`'s can be filtered linearly, and the frame
    /// blitted out of the image at a swap, and how (format_support.rs).
    pub filterable: bool,
    pub gamma_filterable: bool,
    pub blit: Option<vk::Filter>,
    /// What the image was last used for (tracked.rs).
    pub track: TrackedImage,
}

impl Default for Resolved {
    fn default() -> Self {
        Resolved {
            image: vk::Image::null(),
            memory: vk::DeviceMemory::null(),
            view: vk::ImageView::null(),
            gamma_view: vk::ImageView::null(),
            format: vk::Format::UNDEFINED,
            width: 0,
            height: 0,
            scale: 1,
            depth: false,
            attachment_view: vk::ImageView::null(),
            from: TargetKey::default(),
            written_frame: u64::MAX,
            filterable: true,
            gamma_filterable: true,
            blit: Some(vk::Filter::LINEAR),
            track: TrackedImage::NEW,
        }
    }
}

/// A copy per frame slot, chosen once a frame -- not once a resolve, because the title
/// resolves each shadow cascade separately into the same destination and the second must
/// not land in a different image from the first.
#[derive(Clone, Copy)]
pub struct ResolveRing {
    pub copies: [Resolved; FRAME_SLOTS as usize],
    /// The copy the last resolve wrote.
    pub newest: u32,
    /// And the frame it wrote it in.
    pub frame: u64,
}

impl Default for ResolveRing {
    fn default() -> Self {
        ResolveRing { copies: [Resolved::default(); FRAME_SLOTS as usize], newest: 0, frame: u64::MAX }
    }
}

/// What the texture lookup and the resolves share: each destination's ring,
/// the width a fetch gives the texture at a destination where it is narrower than the
/// surface's pitch, and the epoch that moves on at every resolve (resolved_to may have
/// changed).
#[derive(Default)]
pub struct ResolveRecords {
    pub resolved_to: HashMap<u32, ResolveRing, crate::runtime::gpu::hash::MulBuild>,
    pub fetched_width: HashMap<u32, u32, crate::runtime::gpu::hash::MulBuild>,
    pub resolve_epoch: u64,
}

// ---- the registers the draw path reads by name that registers.rs does not name ----

pub const RB_COLOR_MASK: u32 = 0x2104;
pub const RB_STENCILREFMASK_BF: u32 = 0x210C; // the back faces', when they have their own
pub const RB_STENCILREFMASK: u32 = 0x210D;
pub const RB_DEPTHCONTROL: u32 = 0x2200;
pub const RB_BLENDCONTROL0: u32 = 0x2201;
pub const PA_SU_SC_MODE_CNTL: u32 = 0x2205;

/// RB_STENCILREFMASK: the reference, the compare mask and the write mask.
#[derive(Clone, Copy)]
pub struct StencilRefMask(pub u32);

impl StencilRefMask {
    #[inline]
    pub fn reference(self) -> u32 {
        self.0 & 0xFF
    }
    #[inline]
    pub fn mask(self) -> u32 {
        (self.0 >> 8) & 0xFF
    }
    #[inline]
    pub fn write_mask(self) -> u32 {
        (self.0 >> 16) & 0xFF
    }
}
