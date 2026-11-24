// What each image was last used for, and the barrier from that use to the next (the
// plan's S13b). Xenia keeps the same three things on each render target
// (VulkanRenderTargetCache::VulkanRenderTarget's current_stage_mask, current_access_mask
// and current_layout) and asks for a barrier from them to the next use
// (TransitionRenderTarget through VulkanCommandProcessor::PushImageMemoryBarrier), which
// it skips where nothing would change.
//
// The rule, for an image whose last use is `last` entering `next`:
//
// - The contents are kept unless the next use overwrites them all: then the barrier comes
//   out of UNDEFINED, which lets the driver discard them.
// - A use that only reads, in the layout the image is in, after uses that only read:
//   no barrier. The reads are added to the last use, so whatever writes next waits for
//   all of them.
// - Anything else -- a change of layout, a write after anything, anything after a
//   write -- is a barrier whose source is the last use's stages and accesses, as Xenia
//   passes them (an image never used is TOP_OF_PIPE, as Xenia's SubmitBarriers makes an
//   empty source mask), and whose destination is the next use's stages and accesses.
//   Two uses that both write in one layout (the same attachment drawn in two rendering
//   instances) still get one: a rendering instance has no dependency of its own on what
//   came before it, which is what a render pass's external dependency was.
//
// - For an attachment (an image a rendering instance has used: a target, its twin, a depth
//   resolve copy, the image the backdrop draws into), while only reads
//   follow a write, every barrier also names that write in its source: a transfer's read of
//   a resolved target, then the next frame's rendering instance over it, is a barrier whose
//   source is the transfer and the drawing before it. Vulkan would take the chain of
//   barriers (the one before the read waited for the drawing), but the validation layer's
//   synchronisation checks do not follow that chain across submissions for an attachment:
//   with the transfer alone named, they report the drawing as a write after write (sync.sh,
//   MW2_NO_MULTI_DRAW, where the read and the next instance fall in different submissions).
//   This is for the validation layer, and it is not free on the GPU: a barrier's stages are
//   the queue's, not the image's, so the extra stage waits for every earlier command at it,
//   on any image. An image no rendering instance has used -- a texture, a colour resolve
//   copy, the renderer's present images, the display table's images -- names its last use
//   alone, which is clean under sync.sh: naming the write there too would be a wait the
//   validation layer does not need.
//
// One state per image: every barrier the renderer records names all the aspects an image
// has together (colour, or depth and stencil, which change layout together), and all its
// levels and layers, except the blur's chain in backdrop.rs, which keeps one per level.

use ash::vk;

/// Every access that writes.
pub const WRITES: vk::AccessFlags = vk::AccessFlags::from_raw(
    vk::AccessFlags::SHADER_WRITE.as_raw()
        | vk::AccessFlags::COLOR_ATTACHMENT_WRITE.as_raw()
        | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE.as_raw()
        | vk::AccessFlags::TRANSFER_WRITE.as_raw()
        | vk::AccessFlags::HOST_WRITE.as_raw()
        | vk::AccessFlags::MEMORY_WRITE.as_raw(),
);

/// The accesses of a rendering instance's attachments.
const ATTACHMENT_ACCESS: vk::AccessFlags = vk::AccessFlags::from_raw(
    vk::AccessFlags::COLOR_ATTACHMENT_READ.as_raw()
        | vk::AccessFlags::COLOR_ATTACHMENT_WRITE.as_raw()
        | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_READ.as_raw()
        | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE.as_raw(),
);

/// An image's last use: the layout it is in, and the stages and accesses that have used
/// it there since its last barrier; whether a rendering instance has ever used it
/// (`names_write`); and, for such an attachment, while only reads have followed it, the
/// last write before that barrier, which later barriers name too (the header says why).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TrackedImage {
    pub layout: vk::ImageLayout,
    pub stage: vk::PipelineStageFlags,
    pub access: vk::AccessFlags,
    pub written_stage: vk::PipelineStageFlags,
    pub written_access: vk::AccessFlags,
    pub names_write: bool,
}

impl Default for TrackedImage {
    fn default() -> Self {
        Self::NEW
    }
}

/// A barrier from one use to the next.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub from: vk::ImageLayout,
    pub to: vk::ImageLayout,
    pub source_access: vk::AccessFlags,
    pub destination_access: vk::AccessFlags,
    pub source_stage: vk::PipelineStageFlags,
    pub destination_stage: vk::PipelineStageFlags,
}

impl TrackedImage {
    /// An image just made: no use yet, its contents undefined.
    pub const NEW: Self =
        Self::new(vk::ImageLayout::UNDEFINED, vk::PipelineStageFlags::empty(), vk::AccessFlags::empty());

    pub const fn new(layout: vk::ImageLayout, stage: vk::PipelineStageFlags, access: vk::AccessFlags) -> Self {
        Self {
            layout,
            stage,
            access,
            written_stage: vk::PipelineStageFlags::empty(),
            written_access: vk::AccessFlags::empty(),
            names_write: false,
        }
    }

    /// The image enters `next`, keeping its contents or not; the barrier that needs, if
    /// any. Afterwards `next` (or, for reads after reads, both) is the last use.
    #[inline]
    #[must_use]
    pub fn enter(&mut self, next: TrackedImage, keep: bool) -> Option<Transition> {
        if keep
            && self.layout == next.layout
            && !self.access.intersects(WRITES)
            && !next.access.intersects(WRITES)
            && self.layout != vk::ImageLayout::UNDEFINED
        {
            self.stage |= next.stage;
            self.access |= next.access;
            return None;
        }
        let source_stage = self.stage | self.written_stage;
        let transition = Transition {
            from: if keep { self.layout } else { vk::ImageLayout::UNDEFINED },
            to: next.layout,
            source_access: self.access | self.written_access,
            destination_access: next.access,
            source_stage: if source_stage.is_empty() { vk::PipelineStageFlags::TOP_OF_PIPE } else { source_stage },
            destination_stage: next.stage,
        };
        // The write this barrier waits for, carried while only reads follow it, for an
        // attachment alone.
        let (written_stage, written_access) = if !self.names_write || next.access.intersects(WRITES) {
            (vk::PipelineStageFlags::empty(), vk::AccessFlags::empty())
        } else if self.access.intersects(WRITES) {
            (self.stage, self.access & WRITES)
        } else {
            (self.written_stage, self.written_access)
        };
        let names_write = self.names_write || next.access.intersects(ATTACHMENT_ACCESS);
        *self = TrackedImage { written_stage, written_access, names_write, ..next };
        Some(transition)
    }

    /// The image was handed back by work this state does not see, which the next use is
    /// held back for at `stage` by a semaphore: a swapchain image just acquired, whose
    /// wait is at `stage`. Its layout stays what it was presented in.
    #[inline]
    pub fn acquired(&mut self, stage: vk::PipelineStageFlags) {
        self.stage = stage;
        self.access = vk::AccessFlags::empty();
        self.written_stage = vk::PipelineStageFlags::empty();
        self.written_access = vk::AccessFlags::empty();
    }
}

impl Transition {
    /// The barrier as Vulkan takes it, on `image`'s `aspect`, every level and layer.
    #[inline]
    pub fn image_barrier(&self, image: vk::Image, aspect: vk::ImageAspectFlags) -> vk::ImageMemoryBarrier<'static> {
        let mut barrier = self.range_barrier(image, aspect, 0, vk::REMAINING_MIP_LEVELS);
        barrier.subresource_range.layer_count = vk::REMAINING_ARRAY_LAYERS;
        barrier
    }

    /// The same for `count` levels from `first` of the first layer.
    #[inline]
    pub fn range_barrier(
        &self,
        image: vk::Image,
        aspect: vk::ImageAspectFlags,
        first: u32,
        count: u32,
    ) -> vk::ImageMemoryBarrier<'static> {
        vk::ImageMemoryBarrier {
            src_access_mask: self.source_access,
            dst_access_mask: self.destination_access,
            old_layout: self.from,
            new_layout: self.to,
            src_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
            dst_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
            image,
            subresource_range: vk::ImageSubresourceRange {
                aspect_mask: aspect,
                base_mip_level: first,
                level_count: count,
                base_array_layer: 0,
                layer_count: 1,
            },
            ..Default::default()
        }
    }
}

// The uses the renderer, the presenter and the backdrop put images to.
use vk::{AccessFlags as A, ImageLayout as L, PipelineStageFlags as S};

const fn s(a: S, b: S) -> S {
    S::from_raw(a.as_raw() | b.as_raw())
}
const fn a(x: A, y: A) -> A {
    A::from_raw(x.as_raw() | y.as_raw())
}

/// Sampled by a draw's vertex or pixel program (a resolve is sampled by both).
pub const SAMPLED: TrackedImage =
    TrackedImage::new(L::SHADER_READ_ONLY_OPTIMAL, s(S::VERTEX_SHADER, S::FRAGMENT_SHADER), A::SHADER_READ);
/// Sampled by a pixel program alone (the presenter's and the backdrop's).
pub const SAMPLED_BY_PIXELS: TrackedImage =
    TrackedImage::new(L::SHADER_READ_ONLY_OPTIMAL, S::FRAGMENT_SHADER, A::SHADER_READ);
/// Copied, blitted or resolved out of.
pub const COPIED_FROM: TrackedImage = TrackedImage::new(L::TRANSFER_SRC_OPTIMAL, S::TRANSFER, A::TRANSFER_READ);
/// Copied, blitted or resolved into, or cleared.
pub const COPIED_TO: TrackedImage = TrackedImage::new(L::TRANSFER_DST_OPTIMAL, S::TRANSFER, A::TRANSFER_WRITE);
/// Drawn into as a colour attachment.
pub const COLOUR_DRAWN: TrackedImage = TrackedImage::new(
    L::COLOR_ATTACHMENT_OPTIMAL,
    S::COLOR_ATTACHMENT_OUTPUT,
    a(A::COLOR_ATTACHMENT_READ, A::COLOR_ATTACHMENT_WRITE),
);
/// Drawn into as depth and stencil, at both tests (the late one for a program that can
/// discard).
pub const DEPTH_DRAWN: TrackedImage = TrackedImage::new(
    L::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
    s(S::EARLY_FRAGMENT_TESTS, S::LATE_FRAGMENT_TESTS),
    a(A::DEPTH_STENCIL_ATTACHMENT_READ, A::DEPTH_STENCIL_ATTACHMENT_WRITE),
);
/// A multisampled depth surface fetched by the drawn depth resolve's pixel program.
pub const DEPTH_FETCHED: TrackedImage =
    TrackedImage::new(L::DEPTH_STENCIL_READ_ONLY_OPTIMAL, S::FRAGMENT_SHADER, A::SHADER_READ);
/// A depth surface loaded, stored and read by a rendering instance's resolve, which runs
/// at the colour output stage as a colour attachment read whatever it resolves.
pub const DEPTH_RESOLVED_FROM: TrackedImage = TrackedImage::new(
    L::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
    s(s(S::EARLY_FRAGMENT_TESTS, S::LATE_FRAGMENT_TESTS), S::COLOR_ATTACHMENT_OUTPUT),
    a(a(A::DEPTH_STENCIL_ATTACHMENT_READ, A::DEPTH_STENCIL_ATTACHMENT_WRITE), A::COLOR_ATTACHMENT_READ),
);
/// The image that resolve writes, at the colour output stage as a colour attachment
/// write (and as depth, for the store).
pub const DEPTH_RESOLVED_TO: TrackedImage = TrackedImage::new(
    L::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
    s(s(S::EARLY_FRAGMENT_TESTS, S::LATE_FRAGMENT_TESTS), S::COLOR_ATTACHMENT_OUTPUT),
    a(A::COLOR_ATTACHMENT_WRITE, A::DEPTH_STENCIL_ATTACHMENT_WRITE),
);
/// Read and written by a compute program (the display table's).
pub const COMPUTED: TrackedImage = TrackedImage::new(L::GENERAL, S::COMPUTE_SHADER, a(A::SHADER_READ, A::SHADER_WRITE));
/// Read by a compute program (the display table's table).
pub const COMPUTE_READ: TrackedImage = TrackedImage::new(L::GENERAL, S::COMPUTE_SHADER, A::SHADER_READ);
/// Handed to the presentation engine.
pub const PRESENTED: TrackedImage = TrackedImage::new(L::PRESENT_SRC_KHR, S::BOTTOM_OF_PIPE, A::empty());
