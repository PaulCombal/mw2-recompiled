// A draw's arithmetic: the scissor and the viewport out of the registers, where a fetch's
// answer goes in the recent-textures table, and the parts of the draw's record and dynamic
// state -- the clip-space transform, the scaled-slot block, the alpha test, the record's
// length and the depth bias, all pure functions; and textures_for, which is
// not pure: it binds through the texture cache, moves the slot memo, the recent table,
// their counts and the fetched widths, and logs. Then the draw itself (path).

use crate::runtime::gpu::registers::{
    ColorControl, ColorInfo, DepthInfo, EdramMode, ModeCntl, PA_CL_VPORT_XSCALE, PA_CL_VTE_CNTL,
    PA_SC_WINDOW_SCISSOR_BR, PA_SC_WINDOW_SCISSOR_TL, PA_SU_POLY_OFFSET_BACK_OFFSET, PA_SU_POLY_OFFSET_BACK_SCALE,
    PA_SU_POLY_OFFSET_FRONT_OFFSET, PA_SU_POLY_OFFSET_FRONT_SCALE, RB_ALPHA_REF, RB_COLOR_INFO, RB_COLORCONTROL,
    RB_DEPTH_INFO, RB_MODECONTROL, RegisterFile, ScissorCorner, ViewportControl,
};
use crate::runtime::gpu::shader::bindings::{RECORD_TEXTURES, RECORD_VERTEX_TEXTURES};
use ash::vk;

// std::min, std::max and std::clamp's rule: a comparison picks one argument, so a NaN or a
// -0 passes through by position -- min and max answer `a` when either is NaN or the two
// compare equal, clamp answers `v` when it is NaN (f32::min and f32::max answer otherwise).
#[inline]
fn std_min(a: f32, b: f32) -> f32 {
    if b < a { b } else { a }
}
#[inline]
fn std_max(a: f32, b: f32) -> f32 {
    if a < b { b } else { a }
}
#[inline]
fn std_clamp(v: f32, lo: f32, hi: f32) -> f32 {
    if v < lo {
        lo
    } else if hi < v {
        hi
    } else {
        v
    }
}

#[inline]
fn as_float(r: &RegisterFile, index: u32) -> f32 {
    f32::from_bits(r.get(index))
}

/// The window scissor, corners as the title gave them; an inverted one is empty.
#[inline]
pub fn scissor_of(r: &RegisterFile) -> vk::Rect2D {
    let top_left = ScissorCorner(r.get(PA_SC_WINDOW_SCISSOR_TL));
    let bottom_right = ScissorCorner(r.get(PA_SC_WINDOW_SCISSOR_BR));
    vk::Rect2D {
        offset: vk::Offset2D { x: top_left.x() as i32, y: top_left.y() as i32 },
        extent: vk::Extent2D {
            width: if bottom_right.x() > top_left.x() { bottom_right.x() - top_left.x() } else { 0 },
            height: if bottom_right.y() > top_left.y() { bottom_right.y() - top_left.y() } else { 0 },
        },
    }
}

/// The viewport is a scale and an offset rather than a rectangle, and PA_CL_VTE_CNTL says
/// which of the six terms apply at all. A term that is switched off is not absent, it is
/// one (a scale) or nothing (an offset), and keeping the sign of the Y scale is what keeps
/// the frame the right way up: Xenos puts +Y up in clip space and says so with a negative
/// Y scale, Vulkan puts +Y down, and the magnitude alone draws the frame upside down with
/// every winding reversed, so culling discards it. Xenia's GetViewportInfo (BSD-3).
#[inline]
pub fn viewport_of(r: &RegisterFile, scissor: &vk::Rect2D) -> vk::Viewport {
    let control = ViewportControl(r.get(PA_CL_VTE_CNTL));
    let x_scale = if control.x_scale() { as_float(r, PA_CL_VPORT_XSCALE) } else { 1.0 };
    let y_scale = if control.y_scale() { as_float(r, PA_CL_VPORT_XSCALE + 2) } else { 1.0 };
    let x_offset = if control.x_offset() { as_float(r, PA_CL_VPORT_XSCALE + 1) } else { 0.0 };
    let y_offset = if control.y_offset() { as_float(r, PA_CL_VPORT_XSCALE + 3) } else { 0.0 };
    let (mut x, mut y) = (x_offset - x_scale, y_offset - y_scale);
    let (mut w, mut h) = (2.0 * x_scale, 2.0 * y_scale);
    // A viewport of no width is not a transform, it is an unset register.
    if w == 0.0 || h == 0.0 {
        x = scissor.offset.x as f32;
        y = scissor.offset.y as f32;
        w = scissor.extent.width as f32;
        h = scissor.extent.height as f32;
    }
    let z_scale = if control.z_scale() { as_float(r, PA_CL_VPORT_XSCALE + 4) } else { 1.0 };
    let z_offset = if control.z_offset() { as_float(r, PA_CL_VPORT_XSCALE + 5) } else { 0.0 };
    vk::Viewport {
        x,
        y,
        width: w,
        height: h,
        min_depth: std_clamp(std_min(z_offset, z_offset + z_scale), 0.0, 1.0),
        max_depth: std_clamp(std_max(z_offset, z_offset + z_scale), 0.0, 1.0),
    }
}

/// Where a fetch's answer goes in the recent-textures table: one of 64.
#[inline]
pub fn recent_texture(fetch: &[u32; 6]) -> u32 {
    let mut h: u64 = 0;
    for &word in fetch {
        h = (h ^ u64::from(word)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
    (h >> 58) as u32
}

/// Window coordinates to clip space, which the translated vertex epilogue applies from
/// these four floats: the identity for every ordinary draw. When PA_CL_VTE_CNTL switches
/// the X and Y scale terms off, the program has emitted window coordinates itself; x is then
/// normalised by the pitch the *title* named (`surface_pitch`), not by the surface the draw
/// may have been redirected to, so the wide half of an EDRAM clear pair -- half the pitch
/// at twice the sample count -- still lands over the EDRAM it names, and the viewport
/// becomes the open pass's whole size. Vulkan clips against w first: a window x of 960 at
/// w = 1 would be thrown out, and the primitive with it.
#[inline]
pub fn clip_transform(
    r: &RegisterFile,
    surface_pitch: u32,
    width: u32,
    height: u32,
    viewport: &mut vk::Viewport,
) -> [f32; 4] {
    let control = ViewportControl(r.get(PA_CL_VTE_CNTL));
    let mut ndc = [1.0f32, 1.0, 0.0, 0.0];
    if !control.x_scale() && !control.y_scale() && width != 0 && height != 0 {
        ndc[0] = 2.0 / surface_pitch as f32;
        ndc[1] = 2.0 / height as f32;
        ndc[2] = -1.0;
        ndc[3] = -1.0;
        viewport.x = 0.0;
        viewport.y = 0.0;
        viewport.width = width as f32;
        viewport.height = height as f32;
    }
    ndc
}

/// The record's scaled-slot block (bindings::RECORD_SCALED): the slots bound to a
/// resolve's copy at the frame's scale and at the shadow maps', and the two factors that
/// undo them; None, leaving the block as the record was cleared, when nothing is scaled.
#[inline]
pub fn scaled_block(scale: u32, shadow_scale: u32, slots: [u32; 2]) -> Option<[u32; 4]> {
    if scale > 1 || shadow_scale > 1 {
        return Some([slots[0], slots[1], (1.0f32 / scale as f32).to_bits(), (1.0f32 / shadow_scale as f32).to_bits()]);
    }
    None
}

/// The alpha test's block of the record (bindings::RECORD_PIXEL): the function, the
/// reference, the target's exponent bias as a factor and whether the target is gamma.
/// `always` when the title has the test off, and in depth-only mode, where the console runs
/// no pixel shader at all (Xenia's reading too).
#[inline]
pub fn alpha_test(r: &RegisterFile) -> [u32; 4] {
    let edram_mode = EdramMode(r.get(RB_MODECONTROL) & 7);
    let colour = ColorInfo(r.get(RB_COLOR_INFO));
    let pixel_shader_runs = edram_mode != EdramMode::DEPTH_ONLY;
    let control = ColorControl(r.get(RB_COLORCONTROL));
    // Two to the power of the target's exponent bias, built by moving the exponent of 1.0f
    // rather than by calling powf on the draw path.
    let bias = colour.exp_bias().clamp(-31, 31);
    let scale = 0x3F80_0000u32.wrapping_add((bias << 23) as u32);
    let gamma_target = colour.format() == 1;
    [
        if control.alpha_test() && pixel_shader_runs { control.alpha_func() } else { 7 },
        r.get(RB_ALPHA_REF),
        scale,
        u32::from(gamma_target),
    ]
}

/// The record, as far as the highest slot sampled: the vertex shader's texture words are
/// there only when it samples a texture.
#[inline]
pub fn record_words(vertex_textures: u32, pixel_textures: u32) -> u32 {
    if vertex_textures != 0 {
        RECORD_VERTEX_TEXTURES + 32 - vertex_textures.leading_zeros()
    } else {
        RECORD_TEXTURES + if pixel_textures != 0 { 32 - pixel_textures.leading_zeros() } else { 0 }
    }
}

/// The depth bias as vkCmdSetDepthBias takes it, constant then slope. The guest's polygon
/// offset is Direct3D 9's, an absolute depth offset, converted as Xenia does it: the front
/// face first (the face that is drawn, bar shadow volumes), its scale rounded to the
/// nearest power of ten, the back's only when the front gave nothing; the constant in steps
/// of the depth format (2^24, or 2^24 - 1 for D24S8) and the slope from 1/16 pixel units to
/// the images' pixels, `pass_scale` times smaller than the title's.
#[inline]
pub fn depth_bias(r: &RegisterFile, mode_cntl: u32, primitive: u32, pass_scale: u32) -> [f32; 2] {
    let mode = ModeCntl(mode_cntl);
    let depth_info = DepthInfo(r.get(RB_DEPTH_INFO));
    // Triangles, fans, strips, quads and polygons; not points, lines or rectangles.
    let polygonal = primitive < 16 && (0xE0F0u32 >> primitive) & 1 != 0;
    let (mut scale, mut offset) = (0.0f32, 0.0f32);
    if polygonal {
        if mode.poly_offset_front() && !mode.cull_front() {
            scale = as_float(r, PA_SU_POLY_OFFSET_FRONT_SCALE);
            offset = as_float(r, PA_SU_POLY_OFFSET_FRONT_OFFSET);
            if scale != 0.0 {
                // To the nearest order of magnitude, as Xenia does: float log10 and floor,
                // then double pow, division and round.
                let order = 10.0f64.powf(f64::from(scale.abs().log10().floor()));
                scale = ((f64::from(scale) / order).round() * order) as f32;
            }
        }
        if mode.poly_offset_back() && !mode.cull_back() && scale == 0.0 && offset == 0.0 {
            scale = as_float(r, PA_SU_POLY_OFFSET_BACK_SCALE);
            offset = as_float(r, PA_SU_POLY_OFFSET_BACK_OFFSET);
        }
    } else if mode.poly_offset_para() {
        scale = as_float(r, PA_SU_POLY_OFFSET_FRONT_SCALE);
        offset = as_float(r, PA_SU_POLY_OFFSET_FRONT_OFFSET);
    }
    [
        offset * if depth_info.format() != 0 { (1u32 << 24) as f32 } else { ((1u32 << 24) - 1) as f32 },
        scale * (1.0f32 / 16.0) * pass_scale as f32,
    ]
}

// ---- textures_for ----

use super::renderer_state::ResolveRecords;
use crate::log::log_info;
use crate::runtime::gpu::registers::CONSTANT_BASE_FETCH;
use crate::runtime::gpu::shader::TranslationMeta;
use crate::runtime::gpu::shader::bindings::{RECORD_WORDS, TEXTURE_SLOTS};
use crate::runtime::gpu::texture::TextureFetch;

/// What textures_for asks of the texture cache: texture_cache.rs's Bound, which is the
/// cache on its device.
pub trait Textures {
    fn epoch(&self) -> u64;
    fn upload(&mut self, fetch: &TextureFetch) -> u64;
    /// `filterable`: the copy's format can be filtered linearly (format_support.rs).
    fn adopt(&mut self, key: u64, view: vk::ImageView, fetch: &TextureFetch, filterable: bool) -> u64;
    fn handle(&mut self, id: u64, kind: u8) -> u32;
    fn note_drawn_in_world(&mut self, id: u64) -> u64;
}

/// The binding a texture slot got from the last draw that bound it, kept while neither
/// epoch has moved.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct TextureSlot {
    pub fetch: [u32; 6],
    pub id: u64,
    pub epoch: u64,
    pub resolve_epoch: u64,
    pub copy_scale: u32, // the scale of the resolve's copy bound, 0 for none
    pub valid: bool,
}

/// The per-slot memo, the recent-textures table and their counts.
pub struct TextureSlots {
    pub slots: [TextureSlot; TEXTURE_SLOTS as usize],
    /// The last answers for any slot, by a hash of the fetch (recent_texture).
    pub recent: [TextureSlot; 64],
    pub kept: u64,
    pub looked_up: u64,
    /// The draw's answer for each slot, written for a slot as textures_for reaches it and
    /// read only for those: kept here rather than zeroed afresh on every draw.
    pub ids: [u64; TEXTURE_SLOTS as usize],
}

impl Default for TextureSlots {
    fn default() -> Self {
        TextureSlots {
            slots: [TextureSlot::default(); TEXTURE_SLOTS as usize],
            recent: [TextureSlot::default(); 64],
            kept: 0,
            looked_up: 0,
            ids: [0; TEXTURE_SLOTS as usize],
        }
    }
}

/// Where textures_for reads the renderer's pass from: the frame's scale and the open pass's
/// sample count.
#[derive(Clone, Copy)]
pub struct TexturePass {
    pub scale: u32,
    pub current_samples: u32,
}

/// The flash hunt's draw record (renderer_diagnostics.rs): a draw being
/// recorded looks every slot up, and each answer is noted.
pub type NoteTexture<'a> = &'a mut dyn FnMut(u32, &TextureFetch, u64, bool);

/// What the draw's record holds for each slot a shader samples (Textures::handle), in the
/// stage's own words, each with the kind that stage declares the slot as; slots neither
/// names are left as they are.
///
/// `scaled` gets a bit for each slot bound to a resolve's copy, whose image is a whole
/// number of times the size the title gave it: [0] for the frame's scale, [1] for the
/// shadow maps' where it differs. `nanoseconds`, under MW2_TIME_RENDER, gets the time.
#[allow(clippy::too_many_arguments)]
pub fn textures_for<T: Textures>(
    cache: &mut T,
    r: &RegisterFile,
    vertex: &TranslationMeta,
    pixel: &TranslationMeta,
    record: &mut [u32; RECORD_WORDS as usize],
    scaled: &mut [u32; 2],
    slots: &mut TextureSlots,
    resolves: &mut ResolveRecords,
    pass: TexturePass,
    mut draw_record: Option<NoteTexture>,
    nanoseconds: Option<&mut u64>,
) {
    let began = nanoseconds.is_some().then(crate::runtime::clock::nanoseconds);
    let mask = vertex.texture_mask | pixel.texture_mask;
    *scaled = [0, 0];
    let note = |scaled: &mut [u32; 2], slot: usize, copy_scale: u32| {
        if copy_scale != 0 {
            scaled[usize::from(copy_scale != pass.scale)] |= 1u32 << slot;
        }
    };
    let epoch = cache.epoch();
    let resolve_epoch = resolves.resolve_epoch;
    let mut bits = mask;
    while bits != 0 {
        let slot = bits.trailing_zeros() as usize;
        bits &= bits - 1;
        slots.ids[slot] = 0;
        let mut fetch = TextureFetch::default();
        for (i, word) in fetch.d.iter_mut().enumerate() {
            *word = r.get(CONSTANT_BASE_FETCH + slot as u32 * 6 + i as u32);
        }
        if !fetch.is_texture() || (fetch.base_address() == 0 && fetch.mip_address() == 0) {
            continue;
        }

        // The same six dwords as the last draw that bound this slot, with nothing since
        // that could make the lookup answer otherwise or have more to do (the cache's
        // epoch, a resolve): that draw's answer. Then the same for any slot, by the fetch.
        let answers = |entry: &TextureSlot| {
            entry.valid && entry.epoch == epoch && entry.resolve_epoch == resolve_epoch && entry.fetch == fetch.d
        };
        // The hash only when the slot's own memo does not answer, which it does for most.
        let mut known = false;
        let mut recent_at = None;
        if draw_record.is_none() {
            if answers(&slots.slots[slot]) {
                known = true;
            } else {
                let at = recent_texture(&fetch.d) as usize;
                recent_at = Some(at);
                if answers(&slots.recent[at]) {
                    slots.slots[slot] = slots.recent[at];
                    known = true;
                }
            }
        }
        if known {
            slots.kept = slots.kept.wrapping_add(1);
            let kept = slots.slots[slot];
            slots.ids[slot] = kept.id;
            note(scaled, slot, kept.copy_scale);
            if cfg!(feature = "diagnostics") && kept.copy_scale == 0 && slots.ids[slot] != 0 && pass.current_samples > 1
            {
                slots.ids[slot] = cache.note_drawn_in_world(slots.ids[slot]);
            }
            continue;
        }
        slots.looked_up = slots.looked_up.wrapping_add(1);
        let recent_at = recent_at.unwrap_or_else(|| recent_texture(&fetch.d) as usize);
        let keep = |slots: &mut TextureSlots, id: u64, copy_scale: u32| {
            let kept = TextureSlot { fetch: fetch.d, id, copy_scale, epoch, resolve_epoch, valid: true };
            slots.slots[slot] = kept;
            slots.recent[recent_at] = kept;
        };

        let base = fetch.base_address();
        let newest = resolves.resolved_to.get(&base).map(|ring| (ring.newest, ring.copies[ring.newest as usize]));
        if let Some((newest_index, newest)) = newest
            && newest.view != vk::ImageView::null()
        {
            // The texture is as wide as the fetch says; the copy was made as wide as the
            // surface's pitch until a fetch said otherwise (ResolveRecords::fetched_width).
            if !newest.depth && fetch.width() < newest.width {
                let learned = resolves.fetched_width.entry(base).or_insert(0);
                if *learned != fetch.width() {
                    *learned = fetch.width();
                    log_info!(
                        "renderer: the texture at %08X is %u wide in a %u-wide surface; its copies are made %u wide from the next resolve",
                        base,
                        *learned,
                        newest.width,
                        *learned
                    );
                }
            }
            // The key names the copy, not just the guest address, and whether the
            // sampler linearises: the two views of one copy are different entries.
            let linearise = !newest.depth && fetch.linearises() && newest.gamma_view != vk::ImageView::null();
            let key = u64::from(base) | (u64::from(newest_index.wrapping_add(1)) << 32) | (u64::from(linearise) << 34);
            let (view, filterable) =
                if linearise { (newest.gamma_view, newest.gamma_filterable) } else { (newest.view, newest.filterable) };
            slots.ids[slot] = cache.adopt(key, view, &fetch, filterable);
            if slots.ids[slot] != 0 {
                note(scaled, slot, newest.scale);
                let id = slots.ids[slot];
                keep(slots, id, newest.scale);
                if let Some(record) = draw_record.as_mut() {
                    record(slot as u32, &fetch, slots.ids[slot], true);
                }
                continue;
            }
        }
        slots.ids[slot] = cache.upload(&fetch);
        let id = slots.ids[slot];
        keep(slots, id, 0);
        // MW2_MARK_EMPTY: the world pass is the multisampled one; a texture read from an
        // empty block matters when a draw into the world binds it.
        if cfg!(feature = "diagnostics") && slots.ids[slot] != 0 && pass.current_samples > 1 {
            slots.ids[slot] = cache.note_drawn_in_world(slots.ids[slot]);
        }
        if let Some(record) = draw_record.as_mut() {
            record(slot as u32, &fetch, slots.ids[slot], false);
        }
    }
    let mut bits = pixel.texture_mask;
    while bits != 0 {
        let slot = bits.trailing_zeros() as usize;
        bits &= bits - 1;
        record[(RECORD_TEXTURES as usize) + slot] = cache.handle(slots.ids[slot], pixel.texture_kinds[slot]);
    }
    let mut bits = vertex.texture_mask;
    while bits != 0 {
        let slot = bits.trailing_zeros() as usize;
        bits &= bits - 1;
        record[(RECORD_VERTEX_TEXTURES as usize) + slot] = cache.handle(slots.ids[slot], vertex.texture_kinds[slot]);
    }
    if let (Some(began), Some(nanoseconds)) = (began, nanoseconds) {
        *nanoseconds = nanoseconds.wrapping_add(crate::runtime::clock::nanoseconds().wrapping_sub(began));
    }
}

// ---- the draw itself (draw, DrawOp and make_gathered_draws) --------------------------------

pub use path::DrawCall;
pub use path::{gathered, make_gathered_draws};

// Unsafe by nature: the draw writes the frame arena through its mapping and guest memory is
// read for its indices and vertices (the arena's contracts), and its Vulkan calls are
// queued for the recorder, which makes them later with what the closure owns; the
// gathering arrays are the thread's that makes the calls (the recorder, or the consumer
// with MW2_RECORD_THREAD=0), never two threads' at once.
#[allow(unsafe_code, reason = "the draw path: the arena's mapping, the guest's memory and the recorder's calls")]
mod path {
    #![deny(unsafe_op_in_unsafe_fn)]
    use super::super::device::Vk;
    use super::super::recorder::{self, Context};
    use super::super::renderer::Renderer;
    use super::super::renderer_arena::{NoNotes, ServeClock, rectangle_indices_wide};
    use super::super::renderer_diagnostics::{
        diagnose_here, no_depth_test, only_shaders, skip_shaders, trace_passes, traced_shaders,
    };
    use super::super::renderer_frame::FrameView;
    use super::super::renderer_pipelines::{Recorded, TOPOLOGY_NONE, depth_tests_for, faces_for, shader_cache_path};
    use super::super::renderer_state::{
        ArenaKind, DepthTests, PA_SU_SC_MODE_CNTL, PipelineKey, RB_BLENDCONTROL0, RB_COLOR_MASK, RB_DEPTHCONTROL,
        RB_STENCILREFMASK, RB_STENCILREFMASK_BF, StencilRefMask, TargetKey,
    };
    use super::super::renderer_targets::{colour_format_for, scaled_extent, scaled_offset};
    use super::{
        TexturePass, alpha_test, clip_transform, depth_bias, record_words, scaled_block, scissor_of, textures_for,
        viewport_of,
    };
    use crate::log::format_text;
    use crate::runtime::clock;
    use crate::runtime::gpu::registers::{
        ColorInfo, DepthControl, DepthInfo, EdramMode, PA_CL_VTE_CNTL, RB_COLOR_INFO, RB_DEPTH_INFO, RB_MODECONTROL,
        RB_SURFACE_INFO, RegisterFile, SurfaceInfo, ViewportControl,
    };
    use crate::runtime::gpu::shader::bindings::{
        RECORD_NDC, RECORD_OFFSETS, RECORD_PIXEL, RECORD_SCALED, RECORD_WORDS,
    };
    use ash::vk;
    use std::cell::UnsafeCell;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::super::texture_cache::Bound;
    use crate::runtime::shadow_resolution::surface_height;

    /// What the draw packet carries that the register file does not (bridge.rs's DrawCall).
    #[derive(Clone, Copy)]
    pub struct DrawCall<'a> {
        pub primitive: u32,
        pub index_count: u32,
        pub indexed: bool,
        pub index32: bool,
        pub index_address: u32, // guest virtual, already resolved
        // The shaders' microcode, and a hash of each, so a shader is translated once.
        pub vertex_code: &'a [u32],
        pub vertex_hash: u64,
        pub pixel_code: &'a [u32],
        pub pixel_hash: u64,
    }

    /// Written by the thread that makes the calls alone, read by the report
    /// (GatheredCounts): a single writer's load and store.
    pub struct GatheredCounts {
        pub draws: AtomicU64,
        pub alone: AtomicU64,
        pub runs: AtomicU64,
        pub in_runs: AtomicU64,
    }

    #[inline(always)]
    fn add(counter: &AtomicU64, n: u64) {
        counter.store(counter.load(Ordering::Relaxed).wrapping_add(n), Ordering::Relaxed);
    }

    /// The draws the recorder holds back, to be made with one call; a call makes fewer draws
    /// than maxMultiDrawCount, never as many.
    const LONGEST_RUN: u32 = 2048; // RADV's maxMultiDrawCount

    struct Gathering {
        command: vk::CommandBuffer,
        count: u32,
        indexed: bool,
        indexed_draws: [vk::MultiDrawIndexedInfoEXT; LONGEST_RUN as usize],
        draws: [vk::MultiDrawInfoEXT; LONGEST_RUN as usize],
    }

    struct GatheringCell(UnsafeCell<Gathering>);
    // SAFETY: the thread that makes the calls is the only one that touches it (above).
    unsafe impl Sync for GatheringCell {}

    static GATHERING: GatheringCell = GatheringCell(UnsafeCell::new(Gathering {
        command: vk::CommandBuffer::null(),
        count: 0,
        indexed: false,
        indexed_draws: [vk::MultiDrawIndexedInfoEXT { first_index: 0, index_count: 0, vertex_offset: 0 };
            LONGEST_RUN as usize],
        draws: [vk::MultiDrawInfoEXT { first_vertex: 0, vertex_count: 0 }; LONGEST_RUN as usize],
    }));
    static COUNTS: GatheredCounts = GatheredCounts {
        draws: AtomicU64::new(0),
        alone: AtomicU64::new(0),
        runs: AtomicU64::new(0),
        in_runs: AtomicU64::new(0),
    };

    #[cfg_attr(not(feature = "diagnostics"), allow(dead_code, reason = "the report and the diagnostic keys' alone"))]
    pub fn gathered() -> &'static GatheredCounts {
        &COUNTS
    }

    /// # Safety
    /// The calling thread makes the recorder's calls, and holds no other reference.
    #[inline(always)]
    unsafe fn gathering() -> &'static mut Gathering {
        // SAFETY: the caller's.
        unsafe { &mut *GATHERING.0.get() }
    }

    // Which state a DrawOp sets.
    const PIPELINE: u32 = 1;
    const VIEWPORT: u32 = 2;
    const SCISSOR: u32 = 4;
    const DEPTH_BIAS: u32 = 8;
    const STENCIL: u32 = 16;
    const INDEX_BUFFER: u32 = 32;
    const INDEXED: u32 = 64;
    const FACES: u32 = 128;
    const DEPTH_TESTS: u32 = 256;

    /// A draw as the recorder makes it: the state that changed since the last draw in the
    /// command buffer, and the draw (DrawOp). The draw itself is held back while the draws
    /// after it change nothing, and a run of them is made with one
    /// vkCmdDrawMulti(Indexed)EXT; a run ends at a draw that changes any state and at any
    /// other call (make_gathered_draws, the recorder's settle).
    #[derive(Clone, Copy)]
    struct DrawOp {
        set: u32,
        pipeline: vk::Pipeline,
        viewport: vk::Viewport,
        scissor: vk::Rect2D,
        depth_bias: [f32; 2],
        stencil: [u32; 2],
        cull_mode: vk::CullModeFlags,
        front_face: vk::FrontFace,
        depth_tests: DepthTests,
        index_buffer: vk::Buffer,
        index_type: vk::IndexType,
        count: u32,
        first_index: u32,
        record: u32,
    }

    impl DrawOp {
        fn new() -> DrawOp {
            DrawOp {
                set: 0,
                pipeline: vk::Pipeline::null(),
                viewport: vk::Viewport::default(),
                scissor: vk::Rect2D::default(),
                depth_bias: [0.0; 2],
                stencil: [0; 2],
                cull_mode: vk::CullModeFlags::empty(),
                front_face: vk::FrontFace::COUNTER_CLOCKWISE,
                depth_tests: DepthTests::new(),
                index_buffer: vk::Buffer::null(),
                index_type: vk::IndexType::UINT16,
                count: 0,
                first_index: 0,
                record: 0,
            }
        }

        #[inline]
        fn run(&self, c: &Context) {
            // SAFETY: made on the thread that makes the calls.
            let run = unsafe { gathering() };
            let indexed = self.set & INDEXED != 0;
            add(&COUNTS.draws, 1);
            // A draw that changes no state joins the run before it. The draws held back
            // before one that does were made under the state before its own.
            if self.set & !INDEXED == 0
                && run.count != 0
                && run.command == c.into
                && run.indexed == indexed
                && run.count + 1 < c.vk.multi_draw().min(LONGEST_RUN)
            {
                self.join(run);
                return;
            }
            make_run(run, c.vk);
            let command = c.into;
            // SAFETY: the recorder's command buffer, inside the rendering instance the draw
            // was recorded in; the pipeline lives until shutdown, the arena until then too.
            unsafe {
                let v = c.vk;
                if self.set & PIPELINE != 0 {
                    v.cmd_bind_pipeline(command, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
                }
                if self.set & VIEWPORT != 0 {
                    v.cmd_set_viewport(command, 0, std::slice::from_ref(&self.viewport));
                }
                if self.set & SCISSOR != 0 {
                    v.cmd_set_scissor(command, 0, std::slice::from_ref(&self.scissor));
                }
                if self.set & DEPTH_BIAS != 0 {
                    v.cmd_set_depth_bias(command, self.depth_bias[0], 0.0, self.depth_bias[1]);
                }
                if self.set & FACES != 0 {
                    v.cmd_set_cull_mode(command, self.cull_mode);
                    v.cmd_set_front_face(command, self.front_face);
                }
                if self.set & DEPTH_TESTS != 0 {
                    let d = &self.depth_tests;
                    v.cmd_set_depth_test_enable(command, d.depth_test);
                    v.cmd_set_depth_write_enable(command, d.depth_write);
                    v.cmd_set_depth_compare_op(command, d.depth_compare);
                    v.cmd_set_stencil_test_enable(command, d.stencil_test);
                    v.cmd_set_stencil_op(
                        command,
                        vk::StencilFaceFlags::FRONT,
                        d.front.fail_op,
                        d.front.pass_op,
                        d.front.depth_fail_op,
                        d.front.compare_op,
                    );
                    v.cmd_set_stencil_op(
                        command,
                        vk::StencilFaceFlags::BACK,
                        d.back.fail_op,
                        d.back.pass_op,
                        d.back.depth_fail_op,
                        d.back.compare_op,
                    );
                }
                if self.set & STENCIL != 0 {
                    let faces = [vk::StencilFaceFlags::FRONT, vk::StencilFaceFlags::BACK];
                    for (face, &state) in faces.into_iter().zip(&self.stencil) {
                        let state = StencilRefMask(state);
                        v.cmd_set_stencil_compare_mask(command, face, state.mask());
                        v.cmd_set_stencil_write_mask(command, face, state.write_mask());
                        v.cmd_set_stencil_reference(command, face, state.reference());
                    }
                }
                if self.set & INDEX_BUFFER != 0 {
                    v.cmd_bind_index_buffer(command, self.index_buffer, 0, self.index_type);
                }
            }
            run.command = command;
            run.indexed = indexed;
            self.join(run);
        }

        /// The record goes in as the base vertex, which the vertex shader takes away from
        /// the vertex index again.
        #[inline(always)]
        fn join(&self, run: &mut Gathering) {
            // run.count < LONGEST_RUN: a run is made before it reaches the shorter of the
            // device's limit and LONGEST_RUN (run.count + 1 < limit), and a new run starts
            // at 0.
            let at = (run.count & (LONGEST_RUN - 1)) as usize;
            debug_assert!(run.count < LONGEST_RUN);
            if run.indexed {
                run.indexed_draws[at] = vk::MultiDrawIndexedInfoEXT {
                    first_index: self.first_index,
                    index_count: self.count,
                    vertex_offset: self.record as i32,
                };
            } else {
                run.draws[at] = vk::MultiDrawInfoEXT { first_vertex: self.record, vertex_count: self.count };
            }
            run.count += 1;
        }
    }

    /// The recorder's settle: makes the draws DrawOp held back, before any call that is
    /// not joining.
    ///
    /// # Safety
    /// Called only as the recorder calls it: on the thread that makes the recorder's calls
    /// (the recorder, or the consumer with MW2_RECORD_THREAD=0), with `c` the context it
    /// makes them with, while no DrawOp runs.
    pub unsafe fn make_gathered_draws(c: &Context) {
        // SAFETY: the caller's.
        make_run(unsafe { gathering() }, c.vk);
    }

    #[inline]
    fn make_run(run: &mut Gathering, vk: &Vk) {
        if run.count == 0 {
            return;
        }
        let count = (run.count as usize).min(LONGEST_RUN as usize);
        // SAFETY: the run's command buffer, which is still being recorded (a run never
        // crosses a command buffer: a draw into another starts a new one, and every
        // segment's end is a call that settles first).
        unsafe {
            if count == 1 {
                add(&COUNTS.alone, 1);
                if run.indexed {
                    let draw = run.indexed_draws[0];
                    vk.cmd_draw_indexed(run.command, draw.index_count, 1, draw.first_index, draw.vertex_offset, 0);
                } else {
                    vk.cmd_draw(run.command, run.draws[0].vertex_count, 1, run.draws[0].first_vertex, 0);
                }
            } else {
                add(&COUNTS.runs, 1);
                add(&COUNTS.in_runs, count as u64);
                if run.indexed {
                    vk.cmd_draw_multi_indexed(run.command, &run.indexed_draws[..count], 1, 0, None);
                } else {
                    vk.cmd_draw_multi(run.command, &run.draws[..count], 1, 0);
                }
            }
        }
        run.count = 0;
    }

    /// What a draw reads of a target, copied out of it: a Target whole is 150 bytes.
    #[derive(Clone, Copy)]
    struct TargetFacts {
        view: vk::ImageView,
        format: vk::Format,
        samples: vk::SampleCountFlags,
        width: u32,
        height: u32,
    }

    impl TargetFacts {
        #[inline(always)]
        fn of(t: &super::super::renderer_targets::Target) -> TargetFacts {
            TargetFacts { view: t.view, format: t.format, samples: t.samples, width: t.width, height: t.height }
        }
    }

    #[inline(always)]
    fn same_viewport(a: &vk::Viewport, b: &vk::Viewport) -> bool {
        a.x.to_bits() == b.x.to_bits()
            && a.y.to_bits() == b.y.to_bits()
            && a.width.to_bits() == b.width.to_bits()
            && a.height.to_bits() == b.height.to_bits()
            && a.min_depth.to_bits() == b.min_depth.to_bits()
            && a.max_depth.to_bits() == b.max_depth.to_bits()
    }

    #[inline(always)]
    fn same_scissor(a: &vk::Rect2D, b: &vk::Rect2D) -> bool {
        a.offset.x == b.offset.x
            && a.offset.y == b.offset.y
            && a.extent.width == b.extent.width
            && a.extent.height == b.extent.height
    }

    #[inline(always)]
    fn since(timing: bool, from: u64) -> u64 {
        if timing { clock::nanoseconds().wrapping_sub(from) } else { 0 }
    }

    impl Renderer {
        /// The draw path: register file in, Vulkan out. A PM4 draw
        /// packet says only "go": everything it draws with is whatever the register file
        /// holds at that moment.
        ///
        /// # Safety
        /// The draw's index buffer, `call.index_count` indices at `call.index_address`, and
        /// the vertex buffers its fetch constants name lie inside the guest's reservation (the
        /// arena's contract).
        pub unsafe fn draw(&mut self, r: &RegisterFile, call: &DrawCall) {
            let Renderer { vk, core, targets, resolves, pipelines, slots } = self;
            let timing = core.timing;
            let began = if timing { clock::nanoseconds() } else { 0 };
            let Some(vk) = vk.as_deref() else { return };
            if core.device == vk::Device::null() {
                return;
            }
            // A flash hunt's record of this draw, open until it returns however it does.
            if cfg!(feature = "diagnostics") {
                core.diag.begin_draw_record(
                    r,
                    call.vertex_hash,
                    call.pixel_hash,
                    call.primitive,
                    call.index_count,
                    if call.indexed { call.index_address } else { 0 },
                );
            }
            // SAFETY: the caller's.
            unsafe { draw_open(vk, core, targets, resolves, pipelines, slots, r, call) };
            core.diag.record = None;
            if timing {
                core.times.draw = core.times.draw.wrapping_add(since(true, began));
            }
        }
    }

    #[inline]
    #[allow(clippy::too_many_arguments)]
    unsafe fn draw_open(
        vk: &Vk,
        core: &mut super::super::renderer_frame::Core,
        targets: &mut super::super::renderer_targets::Targets,
        resolves: &mut super::super::renderer_resolve::Resolves,
        pipelines: &mut super::super::renderer_pipelines::Pipelines,
        slots: &mut super::TextureSlots,
        r: &RegisterFile,
        call: &DrawCall,
    ) {
        {
            let timing = core.timing;
            // Everything up to the indices: the shaders, the scissor, the render target and
            // the pass that goes with it.
            let setup_from = if timing { clock::nanoseconds() } else { 0 };
            core.draws += 1;

            let topology = super::super::renderer_pipelines::topology_for(call.primitive);
            if topology == TOPOLOGY_NONE {
                core.skip(c"primitive type");
                return;
            }
            let vertex = pipelines.shader_for(vk, false, call.vertex_hash, call.vertex_code);
            let pixel = pipelines.shader_for(vk, true, call.pixel_hash, call.pixel_code);
            let (Some(vertex), Some(pixel)) = (vertex, pixel) else {
                core.skip(c"shader not translated");
                return;
            };

            let mut scissor = scissor_of(r);
            if scissor.extent.width == 0 || scissor.extent.height == 0 {
                core.skip(c"empty scissor");
                return;
            }
            let surface = SurfaceInfo(r.get(RB_SURFACE_INFO));
            let colour = ColorInfo(r.get(RB_COLOR_INFO));
            let depth_info = DepthInfo(r.get(RB_DEPTH_INFO));
            let depth_state = DepthControl(r.get(RB_DEPTHCONTROL));
            let viewport_control = ViewportControl(r.get(PA_CL_VTE_CNTL));
            let edram_mode = EdramMode(r.get(RB_MODECONTROL) & 7);
            if surface.pitch() == 0 {
                core.skip(c"no surface pitch");
                return;
            }

            // MW2 clears EDRAM by drawing rectangles with the depth test ALWAYS, depth
            // writes on and the viewport transform off (see clear_aliased_depth).
            let clear_rectangles = call.primitive == 8
                && depth_state.z_write()
                && depth_state.z_func() == 7
                && !viewport_control.x_scale()
                && !viewport_control.y_scale();

            // The depth-only form, answered before the targets are sized: the rectangle's
            // scissor is left at its reset value, 8192 square.
            if clear_rectangles && edram_mode == EdramMode::DEPTH_ONLY {
                if !core.begin_frame(vk) {
                    core.skip(c"no command buffer");
                    return;
                }
                targets.clear_aliased_depth(&mut FrameView { vk, core: &mut *core }, depth_info.base_tile());
                core.skip(c"EDRAM depth clear");
                return;
            }

            // The colour-and-depth form comes in pairs that describe one row of EDRAM two
            // ways; both are drawn into the surface the world uses, the one with the lowest
            // sample count. A target here is an image per sample count, so the wide half
            // would land in an image nothing else draws into; both halves span the same
            // row of EDRAM in clip coordinates normalised by their own pitch, so the
            // world's surface, with its viewport, takes each over exactly the columns it
            // names. The idiom doubles the count to halve the pitch, so that surface is
            // the one with the lowest.
            let mut use_pitch = surface.pitch();
            let mut use_samples = surface.msaa_samples();
            if clear_rectangles {
                use_clear_target(targets, r, &colour, &surface, &mut use_pitch, &mut use_samples);
            }

            // A surface has a pitch but no height: how tall it is only shows in the region
            // the draws cover, clamped.
            let width = use_pitch.min(4096);
            let mut height = (scissor.offset.y as u32).wrapping_add(scissor.extent.height).clamp(32, 4096);

            // The shadow maps' scale or the frame's, for both targets.
            let pass_scale = targets.pass_scale(r.get(RB_DEPTH_INFO), use_samples);
            // A shadow map's surface is as tall as the title made it.
            if pass_scale != core.scale {
                height = height.min(surface_height());
            }
            let colour_key = TargetKey {
                base_tile: colour.base_tile(),
                pitch: use_pitch,
                format: colour_format_for(colour.format()).as_raw() as u32,
                samples: use_samples,
                depth: false,
                scale: pass_scale,
            };
            let depth_format = targets.depth_format_for(depth_info.format()).as_raw() as u32;
            let depth_key = targets.depth_key(depth_info.base_tile(), depth_format, use_samples, pass_scale);
            // SAFETY: the renderer's live device.
            let (colour_id, depth_id) = unsafe {
                let colour_id = targets.ensure(
                    vk,
                    &mut FrameView { vk, core: &mut *core },
                    &colour_key,
                    width,
                    height,
                    colour.format(),
                );
                let depth_id = targets.ensure(
                    vk,
                    &mut FrameView { vk, core: &mut *core },
                    &depth_key,
                    width,
                    height,
                    depth_info.format(),
                );
                (colour_id, depth_id)
            };
            let (Some(colour_id), Some(depth_id)) = (colour_id, depth_id) else { return };

            if !core.begin_frame(vk) {
                core.skip(c"no command buffer");
                return;
            }

            // Both keys carry the surface's sample count, so the two agree unless a depth
            // image made under another count is reused; a pass cannot mix them. What the
            // draw reads of the two, taken by value (the targets are borrowed again below).
            let (colour_target, depth_target) = {
                let (c, d) = (targets.get(colour_id), targets.get(depth_id));
                (TargetFacts::of(c), TargetFacts::of(d))
            };
            if colour_target.samples != depth_target.samples {
                core.skip(c"sample counts differ");
                return;
            }
            let pass_width = colour_target.width.min(depth_target.width);
            let pass_height = colour_target.height.min(depth_target.height);
            if colour_target.view != core.current_colour || depth_target.view != core.current_depth {
                if cfg!(feature = "diagnostics")
                    && trace_passes() != 0
                    && diagnose_here(core.frames)
                    && core.diag.pass_frames <= trace_passes()
                {
                    let (c, d) = (targets.get(colour_id), targets.get(depth_id));
                    trace_pass(r, core, c, d, &surface, &colour, &depth_info, &edram_mode, &scissor);
                }
                targets.begin_rendering(
                    &mut FrameView { vk, core: &mut *core },
                    colour_id,
                    depth_id,
                    pass_width,
                    pass_height,
                );
            }
            if timing {
                core.times.setup = core.times.setup.wrapping_add(since(true, setup_from));
            }

            // Before the constants, because the highest index in the run is what says how
            // much of the vertex buffer the fetch constants have to bring across.
            let mut index_offset = 0u32;
            let mut lowest_vertex = 0u32;
            let mut highest_vertex = call.index_count.saturating_sub(1);
            if call.indexed {
                if call.index_address == 0 || call.index_count == 0 {
                    core.skip(c"indexed draw with no indices");
                    return;
                }
                let from = if timing { clock::nanoseconds() } else { 0 };
                // SAFETY: the caller's index buffer. The flash hunt's notes only while a draw
                // is recorded: otherwise the instance without them.
                index_offset = unsafe {
                    if core.diag.record.is_some() {
                        core.arena.write_indices(
                            call.index_address,
                            call.index_count,
                            call.index32,
                            &mut lowest_vertex,
                            &mut highest_vertex,
                            Some(&mut core.diag),
                        )
                    } else {
                        core.arena.write_indices(
                            call.index_address,
                            call.index_count,
                            call.index32,
                            &mut lowest_vertex,
                            &mut highest_vertex,
                            None::<&mut NoNotes>,
                        )
                    }
                };
                if timing {
                    core.times.index = core.times.index.wrapping_add(since(true, from));
                }
                if index_offset == u32::MAX {
                    core.skip(c"frame arena full");
                    return;
                }
            }

            // Three vertices a rectangle, and the fourth corner is written into the arena
            // beside them.
            let rectangles = if call.primitive == 8 && !call.indexed { call.index_count / 3 } else { 0 };
            if call.primitive == 8 && rectangles == 0 {
                core.skip(c"rectangle list this pass cannot expand");
                return;
            }

            let constants = {
                let from = if timing { clock::nanoseconds() } else { 0 };
                let clock = ServeClock {
                    recording: core.submission_serial.wrapping_add(1),
                    completed: core.completed_serial,
                    frame: core.frames,
                };
                let (vertex_meta, pixel_meta) = (&pipelines.shader(vertex).meta, &pipelines.shader(pixel).meta);
                let constants = if core.diag.record.is_some() {
                    core.arena.write_constants(
                        r,
                        vertex_meta,
                        pixel_meta,
                        lowest_vertex,
                        highest_vertex,
                        rectangles,
                        clock,
                        Some(&mut core.diag),
                    )
                } else {
                    core.arena.write_constants(
                        r,
                        vertex_meta,
                        pixel_meta,
                        lowest_vertex,
                        highest_vertex,
                        rectangles,
                        clock,
                        None::<&mut NoNotes>,
                    )
                };
                if timing {
                    core.times.constants = core.times.constants.wrapping_add(since(true, from));
                }
                constants
            };
            if !constants.ok {
                core.skip(c"frame arena full");
                return;
            }
            // From here to the first command recorded: the pipeline key, the texture handles
            // and the viewport.
            let state_from = if timing { clock::nanoseconds() } else { 0 };

            if rectangles != 0 {
                index_offset = core.arena.write_rectangle_indices(rectangles);
                if index_offset == u32::MAX {
                    core.skip(c"frame arena full");
                    return;
                }
            }

            // MW2_TRACE_SHADER_VERTS=<hex> dumps what a draw actually hands the GPU.
            let named = cfg!(feature = "diagnostics") && traced_shaders().names(call.vertex_hash, call.pixel_hash);
            if named {
                core.verts_matched += 1;
            }
            if named && diagnose_here(core.frames) && core.verts_traced < 24 {
                trace_verts(
                    r,
                    core,
                    pipelines,
                    vertex,
                    pixel,
                    call,
                    targets.get(colour_id),
                    (&surface, &colour, &depth_info, &edram_mode),
                    rectangles,
                    &scissor,
                    (lowest_vertex, highest_vertex),
                    constants.fetch,
                );
            }

            if cfg!(feature = "diagnostics") {
                if !only_shaders().hashes.is_empty() && !only_shaders().names(call.vertex_hash, call.pixel_hash) {
                    core.skip(c"not the shader asked for");
                    return;
                }
                if skip_shaders().names(call.vertex_hash, call.pixel_hash) {
                    core.skip(c"the shader asked to be skipped");
                    return;
                }
            }

            // The depth and stencil tests and the culling are set per draw (below), not
            // built into the pipeline; MW2_NO_DEPTH_TEST tells a shader writing nothing from
            // a depth test rejecting everything in one run.
            let key = PipelineKey {
                vertex_shader: call.vertex_hash,
                pixel_shader: call.pixel_hash,
                colour_format: colour_target.format.as_raw() as u32,
                depth_format: depth_target.format.as_raw() as u32,
                topology: topology.as_raw() as u32,
                blend_control: r.get(RB_BLENDCONTROL0),
                colour_mask: r.get(RB_COLOR_MASK),
                samples: colour_target.samples.as_raw(),
            };
            let depth_control = if no_depth_test() { 0 } else { r.get(RB_DEPTHCONTROL) };
            let mode_cntl = r.get(PA_SU_SC_MODE_CNTL);
            let mut new_pipeline = false;
            let built = {
                let from = if timing { clock::nanoseconds() } else { 0 };
                let built = pipelines.ensure_pipeline(
                    vk,
                    &key,
                    vertex,
                    pixel,
                    [call.vertex_code, call.pixel_code],
                    &mut core.retiring,
                    Some(&mut new_pipeline),
                );
                if timing {
                    core.times.pipeline = core.times.pipeline.wrapping_add(since(true, from));
                }
                built
            };
            if built == vk::Pipeline::null() {
                core.skip(c"pipeline rejected");
                return;
            }
            if new_pipeline && shader_cache_path().is_some() {
                pipelines.recorded.push(Recorded {
                    vertex_shader: key.vertex_shader,
                    pixel_shader: key.pixel_shader,
                    colour_format: colour_target.format.as_raw() as u32,
                    depth_format: depth_target.format.as_raw() as u32,
                    topology: key.topology,
                    depth_control,
                    blend_control: key.blend_control,
                    colour_mask: key.colour_mask,
                    mode_cntl,
                    samples: key.samples,
                });
            }

            // The draw's record (bindings.rs), filled from here on and written into the
            // arena just before the draw.
            let vertex_meta = &pipelines.shader(vertex).meta;
            let pixel_meta = &pipelines.shader(pixel).meta;
            let (pixel_textures, vertex_textures) = (pixel_meta.texture_mask, vertex_meta.texture_mask);
            let mut values = [0u32; RECORD_WORDS as usize];
            let mut scaled_slots = [0u32; 2];
            {
                let recording = core.diag.record.is_some();
                let diag = &mut core.diag;
                let mut note =
                    |slot: u32, fetch: &crate::runtime::gpu::texture::TextureFetch, id: u64, resolved: bool| {
                        diag.note_draw_texture(slot, fetch, id, resolved)
                    };
                textures_for(
                    &mut Bound { cache: &mut core.textures, vk },
                    r,
                    vertex_meta,
                    pixel_meta,
                    &mut values,
                    &mut scaled_slots,
                    slots,
                    &mut resolves.records,
                    TexturePass { scale: core.scale, current_samples: core.current_samples },
                    if recording { Some(&mut note) } else { None },
                    if timing { Some(&mut core.times.texture) } else { None },
                );
                if recording {
                    core.diag.draw_texture_versions(&core.textures);
                }
            }

            let mut viewport = viewport_of(r, &scissor);
            scissor.extent.width = scissor.extent.width.min(core.current_width.wrapping_sub(scissor.offset.x as u32));
            scissor.extent.height =
                scissor.extent.height.min(core.current_height.wrapping_sub(scissor.offset.y as u32));

            let mut op = DrawOp::new();

            // Window coordinates to clip space, which the translated vertex epilogue applies
            // from these four floats: the identity for every ordinary draw.
            let ndc = clip_transform(r, surface.pitch(), core.current_width, core.current_height, &mut viewport);
            for (word, value) in values[RECORD_NDC as usize..RECORD_NDC as usize + 4].iter_mut().zip(ndc) {
                *word = value.to_bits();
            }
            if let Some(block) = scaled_block(core.scale, core.shadow_scale, scaled_slots) {
                values[RECORD_SCALED as usize..RECORD_SCALED as usize + 4].copy_from_slice(&block);
            }
            // In dwords, in the order bindings.rs numbers the blocks.
            let offsets = [
                constants.vertex_floats / 4,
                constants.fetch / 4,
                constants.booleans / 4,
                constants.loops / 4,
                constants.pixel_floats / 4,
            ];
            values[RECORD_OFFSETS as usize..RECORD_OFFSETS as usize + 5].copy_from_slice(&offsets);
            // The alpha test, which the translated pixel shader makes.
            values[RECORD_PIXEL as usize..RECORD_PIXEL as usize + 4].copy_from_slice(&alpha_test(r));

            // The record, as far as the highest slot sampled. Draws come in runs with the
            // same state, and a run shares one.
            {
                let words = (record_words(vertex_textures, pixel_textures) as usize).min(RECORD_WORDS as usize);
                if !core.bound.record_valid
                    || core.bound.record_words as usize != words
                    || core.bound.record[..words] != values[..words]
                {
                    // SAFETY: the record's own array, `words` of them.
                    let at = unsafe {
                        core.arena.copy_into_arena(values.as_ptr().cast(), words as u32 * 4, ArenaKind::Constants)
                    };
                    if at == u32::MAX {
                        core.skip(c"frame arena full");
                        return;
                    }
                    core.bound.record[..words].copy_from_slice(&values[..words]);
                    core.bound.record_words = words as u32;
                    core.bound.record_at = at / 4;
                    core.bound.record_valid = true;
                }
                op.record = core.bound.record_at;
            }

            if timing {
                core.times.state = core.times.state.wrapping_add(since(true, state_from));
            }

            let record_from = if timing { clock::nanoseconds() } else { 0 };
            if core.bound.pipeline != built {
                op.set |= PIPELINE;
                op.pipeline = built;
                core.bound.pipeline = built;
            }

            // The title's pixels up to here, the images' from here.
            let factor = pass_scale as f32;
            viewport.x *= factor;
            viewport.y *= factor;
            viewport.width *= factor;
            viewport.height *= factor;
            scissor = vk::Rect2D {
                offset: scaled_offset(scissor.offset, pass_scale),
                extent: scaled_extent(scissor.extent, pass_scale),
            };

            if !core.bound.viewport_valid || !same_viewport(&core.bound.viewport, &viewport) {
                op.set |= VIEWPORT;
                op.viewport = viewport;
                core.bound.viewport = viewport;
                core.bound.viewport_valid = true;
            }
            if !core.bound.scissor_valid || !same_scissor(&core.bound.scissor, &scissor) {
                op.set |= SCISSOR;
                op.scissor = scissor;
                core.bound.scissor = scissor;
                core.bound.scissor_valid = true;
            }

            // The guest's polygon offset is Direct3D 9's, converted as Xenia does it.
            {
                let bias = depth_bias(r, mode_cntl, call.primitive, pass_scale);
                if !core.bound.depth_bias_valid
                    || core.bound.depth_bias[0].to_bits() != bias[0].to_bits()
                    || core.bound.depth_bias[1].to_bits() != bias[1].to_bits()
                {
                    op.set |= DEPTH_BIAS;
                    op.depth_bias = bias;
                    core.bound.depth_bias = bias;
                    core.bound.depth_bias_valid = true;
                }
            }

            // Culling and winding, and the depth and stencil tests: dynamic state, so one
            // pipeline serves every combination the title draws a pair of shaders with.
            if !core.bound.faces_valid || core.bound.mode_cntl != mode_cntl {
                op.set |= FACES;
                (op.cull_mode, op.front_face) = faces_for(mode_cntl);
                core.bound.mode_cntl = mode_cntl;
                core.bound.faces_valid = true;
            }
            if !core.bound.depth_tests_valid || core.bound.depth_control != depth_control {
                op.set |= DEPTH_TESTS;
                op.depth_tests = depth_tests_for(depth_control);
                core.bound.depth_control = depth_control;
                core.bound.depth_tests_valid = true;
            }

            // The stencil reference and masks, set whether this draw tests stencil or not.
            {
                let front = r.get(RB_STENCILREFMASK);
                let stencil = [front, if depth_state.backface_enable() { r.get(RB_STENCILREFMASK_BF) } else { front }];
                if !core.bound.stencil_valid || core.bound.stencil != stencil {
                    op.set |= STENCIL;
                    op.stencil = stencil;
                    core.bound.stencil = stencil;
                    core.bound.stencil_valid = true;
                }
            }

            if call.indexed || rectangles != 0 {
                // The whole arena is bound once per index size and each draw names where its
                // indices start.
                let wide = (call.indexed && call.index32) || (rectangles != 0 && rectangle_indices_wide(rectangles));
                let index_type = if wide { vk::IndexType::UINT32 } else { vk::IndexType::UINT16 };
                if core.bound.index_buffer != core.arena_buffer
                    || core.bound.index_offset != 0
                    || core.bound.index_type != index_type.as_raw()
                {
                    op.set |= INDEX_BUFFER;
                    op.index_buffer = core.arena_buffer;
                    op.index_type = index_type;
                    core.bound.index_buffer = core.arena_buffer;
                    core.bound.index_offset = 0;
                    core.bound.index_type = index_type.as_raw();
                }
                op.set |= INDEXED;
                op.count = if rectangles != 0 { rectangles.wrapping_mul(6) } else { call.index_count };
                op.first_index = index_offset / if wide { 4 } else { 2 };
            } else {
                op.count = call.index_count;
            }
            recorder::joining(move |c| op.run(c));
            {
                let t = targets.get_mut(colour_id);
                t.draws += 1;
                if edram_mode == EdramMode::COLOR_DEPTH && key.colour_mask != 0 {
                    let first = t.colour_draws_this_frame == 0;
                    t.colour_draws_this_frame += 1;
                    if first {
                        t.frames_with_colour += 1;
                    }
                }
            }
            // The depth target too, so a dump can tell a depth buffer some pass is testing
            // against from one no pass has ever bound.
            targets.get_mut(depth_id).draws += 1;
            core.draws_recorded += 1;
            core.segment_draws = true;
            if timing {
                core.times.record = core.times.record.wrapping_add(since(true, record_from));
            }
        }
    }

    /// The clear idiom's pair: the surface with the lowest sample count over the same row
    /// of EDRAM.
    #[cold]
    #[inline(never)]
    fn use_clear_target(
        targets: &mut super::super::renderer_targets::Targets,
        r: &RegisterFile,
        colour: &ColorInfo,
        surface: &SurfaceInfo,
        use_pitch: &mut u32,
        use_samples: &mut u32,
    ) {
        let row = surface.pitch().wrapping_shl(surface.msaa_samples());
        let format = colour_format_for(colour.format()).as_raw() as u32;
        targets.clear_pair(colour.base_tile(), format, row, r.get(RB_DEPTH_INFO), use_pitch, use_samples);
    }

    /// MW2_TRACE_PASSES: the pass about to open, its targets and its state.
    #[cold]
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn trace_pass(
        r: &RegisterFile,
        core: &super::super::renderer_frame::Core,
        colour_target: &super::super::renderer_targets::Target,
        depth_target: &super::super::renderer_targets::Target,
        surface: &SurfaceInfo,
        colour: &ColorInfo,
        depth_info: &DepthInfo,
        edram_mode: &EdramMode,
        scissor: &vk::Rect2D,
    ) {
        let first = format_text!(
            "pass: %ux%u colour at tile %u, %ux drawn %ux, mask %08X, depth %08X, mode %u,",
            colour_target.width,
            colour_target.height,
            colour.base_tile(),
            1u32.wrapping_shl(surface.msaa_samples()),
            colour_target.samples.as_raw(),
            r.get(RB_COLOR_MASK),
            r.get(RB_DEPTHCONTROL),
            edram_mode.0
        );
        let second = format_text!(
            " scissor %d,%d %ux%u, after %llu draws; depth tile %u %ux, %llu draws, cleared %s",
            scissor.offset.x,
            scissor.offset.y,
            scissor.extent.width,
            scissor.extent.height,
            core.draws_recorded,
            depth_info.base_tile(),
            depth_target.samples.as_raw(),
            depth_target.draws,
            if depth_target.depth_cleared_in == core.frames { c"this frame" } else { c"not this frame" }
        );
        crate::log::log_kernel!("%s%s", first.as_c_str(), second.as_c_str());
    }

    /// MW2_TRACE_SHADER_VERTS: what a draw actually hands the GPU -- the fetch constants
    /// after rewriting, the vertices they name, and the first float constants.
    #[cold]
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn trace_verts(
        r: &RegisterFile,
        core: &mut super::super::renderer_frame::Core,
        pipelines: &super::super::renderer_pipelines::Pipelines,
        vertex: u32,
        pixel: u32,
        call: &DrawCall,
        colour_target: &super::super::renderer_targets::Target,
        (surface, colour, depth_info, edram_mode): (&SurfaceInfo, &ColorInfo, &DepthInfo, &EdramMode),
        rectangles: u32,
        scissor: &vk::Rect2D,
        (lowest_vertex, highest_vertex): (u32, u32),
        fetch_at: u32,
    ) {
        use crate::log::log_kernel;
        use crate::runtime::gpu::registers::{CONSTANT_BASE_ALU, ConstantWindow, PA_CL_VPORT_XSCALE, SQ_VS_CONST};
        core.verts_traced += 1;
        let term = |i: u32| f64::from(f32::from_bits(r.get(PA_CL_VPORT_XSCALE + i)));
        let first = format_text!(
            "verts: colour tile %u %ux %ux%u, depth tile %u, mask %08X, mode %u, depthcontrol %08X, %u rectangle(s),",
            colour.base_tile(),
            1u32.wrapping_shl(surface.msaa_samples()),
            colour_target.width,
            colour_target.height,
            depth_info.base_tile(),
            r.get(RB_COLOR_MASK),
            edram_mode.0,
            r.get(RB_DEPTHCONTROL),
            rectangles
        );
        let second = format_text!(
            " viewport scale/offset x %.1f/%.1f y %.1f/%.1f z %.4f/%.4f, scissor %d,%d %ux%u",
            term(0),
            term(1),
            term(2),
            term(3),
            term(4),
            term(5),
            scissor.offset.x,
            scissor.offset.y,
            scissor.extent.width,
            scissor.extent.height
        );
        log_kernel!("%s%s", first.as_c_str(), second.as_c_str());
        let v = pipelines.shader(vertex);
        let p = pipelines.shader(pixel);
        if core.verts_traced == 1 {
            let text = crate::log::c_string(crate::runtime::gpu::shader::describe_parts(
                v.ok,
                &v.error,
                v.spirv_words,
                &v.meta,
            ));
            log_kernel!("verts: the shader itself: %s", text.as_c_str());
        }
        log_kernel!(
            "verts: %016llX/%016llX primitive %u, %u %sindices, vertices %u..%u",
            call.vertex_hash,
            call.pixel_hash,
            call.primitive,
            call.index_count,
            if call.indexed { c"" } else { c"auto " },
            lowest_vertex,
            highest_vertex
        );
        let window = ConstantWindow(r.get(SQ_VS_CONST)).base().min(256);
        let constant =
            |row: u32, column: u32| f64::from(f32::from_bits(r.get(CONSTANT_BASE_ALU + window * 4 + row * 4 + column)));
        for row in 0..8 {
            log_kernel!(
                "verts:   c%-2u %9.4f %9.4f %9.4f %9.4f",
                row,
                constant(row, 0),
                constant(row, 1),
                constant(row, 2),
                constant(row, 3)
            );
        }
        // What the arena holds: written by this draw, read back (write-combined: slow, and
        // only here), its vertex copies made first (copy_worker.rs).
        crate::runtime::gpu::vulkan::copy_worker::settle();
        let arena_word = |at: u32| -> u32 {
            let mut word = [0u8; 4];
            // SAFETY: inside the arena: the fetch window write_constants wrote, or a vertex
            // checked against the arena's size below.
            unsafe { std::ptr::copy_nonoverlapping(core.arena.mapped_at(at), word.as_mut_ptr(), 4) };
            u32::from_ne_bytes(word)
        };
        for slot in 0..96u32 {
            // The arena holds only the pairs the programs read (write_constants).
            let read = v.meta.fetch_pairs_read[(slot / 32) as usize] | p.meta.fetch_pairs_read[(slot / 32) as usize];
            if read & (1 << (slot % 32)) == 0 {
                continue;
            }
            let entry = fetch_at.wrapping_add(slot * 16);
            let (word0, word1) = (arena_word(entry), arena_word(entry + 4));
            if word0 & 3 != 3 || word1 == 0 {
                continue;
            }
            let stride = v.meta.vertex_fetch_stride[slot as usize];
            log_kernel!(
                "verts:   fetch slot %u: arena %08X, %u dwords declared, stride %u",
                slot,
                word0 & !3,
                (word1 >> 2) & 0xFFFFFF,
                stride
            );
            if stride == 0 || stride > 32 {
                continue;
            }
            // Past highest_vertex by the rectangle count: the fourth corner of each
            // rectangle is written after the vertices the title gave.
            let last = highest_vertex.wrapping_add(rectangles);
            let mut v_at = lowest_vertex;
            while v_at <= last && v_at < lowest_vertex.wrapping_add(8) {
                let at = (word0 & !3).wrapping_add(v_at.wrapping_mul(stride).wrapping_mul(4));
                if at.wrapping_add(stride * 4) > core.arena.bytes() {
                    break;
                }
                // Hex as well as float: several of a vertex's dwords are packed attributes.
                // The copy holds the guest's big-endian bytes.
                let mut line = format_text!("verts:     v%-3u", v_at).into_bytes();
                for w in 0..stride.min(8) {
                    line.extend_from_slice(format_text!(" %08X", arena_word(at + w * 4)).as_bytes());
                }
                line.extend_from_slice(b"  |");
                for w in 0..stride.min(4) {
                    let value = f64::from(f32::from_bits(arena_word(at + w * 4).swap_bytes()));
                    line.extend_from_slice(format_text!(" %11.4f", value).as_bytes());
                }
                line.truncate(511);
                let line = crate::log::c_string(line);
                log_kernel!("%s", line.as_c_str());
                v_at += 1;
            }
        }
    }
}
