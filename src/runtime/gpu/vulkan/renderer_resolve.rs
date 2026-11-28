// Resolves: EDRAM copied out to a texture the title reads back, or the finished frame
// copied out for the window.
//
// A resolve that is not the frame is the title handing itself a texture: the pixels never
// go through guest memory, the destination address names one of a ring of images, a copy
// per frame slot chosen once a frame (renderer_state.rs's ResolveRing), and a fetch of
// that address binds it. The frame is whatever the title swaps to: the front buffer the
// swap packet names, which a resolve filled like any other destination, blitted into one
// of the present images and run through the display's colour table.
//
// Unsafe by nature: the images are the renderer's device's and named by commands queued
// on the recorder; an image is replaced only after retire_before_destroy has waited for
// every submission and the recorder, and after the texture cache has forgotten every view
// of it (forget_views).
#![deny(unsafe_op_in_unsafe_fn)]

use super::device::Vk;
use super::display_table::DisplayTable;
use super::format_support;
use super::recorder;
use super::renderer_draw::scissor_of;
use super::renderer_state::{FRAME_SLOTS, ResolveRecords, Resolved, TargetKey};
use super::renderer_targets::{
    DEPTH_ASPECTS, DepthInto, Frame, Targets, colour_format_for, record_use, resolve_colour_into, scaled3,
};
use super::texture_formats::srgb_format_for;
use super::tracked::{self, TrackedImage};
use super::util;
use crate::log::log_info;
use crate::runtime::gpu::registers::{
    ColorInfo, CopyControl, CopyDestInfo, CopyDestPitch, DepthInfo, RB_COLOR_INFO, RB_COPY_CONTROL, RB_COPY_DEST_BASE,
    RB_COPY_DEST_INFO, RB_COPY_DEST_PITCH, RB_DEPTH_INFO, RB_SURFACE_INFO, RegisterFile, SurfaceInfo,
};
use crate::runtime::gpu::texture::texture_format_of;
use ash::vk;
use std::collections::BTreeMap;

use super::presenter::wait_until_taken;
use super::texture_cache::forget_views;

/// The surface each resolve destination is the start of, as RB_COPY_DEST_PITCH describes
/// it: a resolve whose address falls inside one is writing part of that surface, not a
/// surface of its own.
#[derive(Clone, Copy, Default)]
pub struct ResolveSurface {
    pub bytes: u32,
    pub row_bytes: u32,
    pub frame: u64,
}

/// The resolves' state: the records the texture lookup shares, the surfaces, the counts
/// the report prints, and the display's colour table the swap applies (display_table.rs).
pub struct Resolves {
    pub records: ResolveRecords,
    pub surfaces: BTreeMap<u32, ResolveSurface>,
    pub display: DisplayTable,
    /// The device the display table asks for its memory types.
    physical: vk::PhysicalDevice,
    // ---- counted for the report only ----
    pub resolves: u64,
    pub depth_resolves: u64,
    pub resolve_images_made: u64,
    pub resolve_nanoseconds: u64,
    /// Depth resolves whose rectangle starts past its surface: empty, and refused.
    pub depth_resolve_wraps: u64,
    /// Those of them starting short of its right edge, past its height: a refusal of the
    /// renderer's own, as Xenia clamps only x.
    pub depth_resolve_wraps_below: u64,
}

impl Resolves {
    pub fn new(physical: vk::PhysicalDevice) -> Resolves {
        Resolves {
            records: ResolveRecords::default(),
            surfaces: BTreeMap::new(),
            display: DisplayTable::new(),
            physical,
            resolves: 0,
            depth_resolves: 0,
            resolve_images_made: 0,
            resolve_nanoseconds: 0,
            depth_resolve_wraps: 0,
            depth_resolve_wraps_below: 0,
        }
    }

    /// The diagnostics counter's line, when it was ever hit.
    #[allow(dead_code, reason = "the renderer's report prints this line from RendererReport, after the shutdown")]
    pub fn report(&self) {
        if cfg!(feature = "diagnostics") && self.depth_resolve_wraps != 0 {
            log_info!(
                "renderer: %llu depth resolves started past their surface's edge, refused as empty (%llu past its height only, not Xenia's rule)",
                self.depth_resolve_wraps,
                self.depth_resolve_wraps_below
            );
        }
    }

    /// Every resolve image destroyed and the records emptied, in the map's order: the
    /// device is idle and the images unrelated, so only a command trace's shutdown lines
    /// depend on it.
    ///
    /// # Safety
    /// `vk` is the renderer's device, idle.
    pub unsafe fn destroy_all(&mut self, vk: &Vk) {
        for ring in self.records.resolved_to.values_mut() {
            for copy in &mut ring.copies {
                // SAFETY: the caller's.
                unsafe { destroy_resolved(vk, copy) };
            }
        }
        self.records.resolved_to.clear();
        self.records.fetched_width.clear();
        self.records.resolve_epoch = self.records.resolve_epoch.wrapping_add(1);
    }

    /// The resolve the registers describe: RB_COPY_CONTROL's source, colour or depth, out
    /// of the surface the registers name into the destination's copy for this frame, at
    /// the place Xenia's copy_dest_coordinate_info puts it, then the clears the copy
    /// control asks for.
    ///
    /// # Safety
    /// `vk` is the renderer's live device and the recorder's; `targets` and `f` are the
    /// renderer's.
    pub unsafe fn resolve<F: Frame>(&mut self, vk: &Vk, targets: &mut Targets, f: &mut F, r: &RegisterFile) {
        let began = f.timing().then(std::time::Instant::now);
        // SAFETY: the caller's.
        unsafe { self.resolve_untimed(vk, targets, f, r) };
        if let Some(began) = began {
            // Nanoseconds: a phase of one draw is a few hundred of them.
            self.resolve_nanoseconds = self.resolve_nanoseconds.wrapping_add(began.elapsed().as_nanos() as u64);
        }
    }

    unsafe fn resolve_untimed<F: Frame>(&mut self, vk: &Vk, targets: &mut Targets, f: &mut F, r: &RegisterFile) {
        self.records.resolve_epoch = self.records.resolve_epoch.wrapping_add(1);
        let control = CopyControl(r.get(RB_COPY_CONTROL));
        let rect = scissor_of(r);
        let destination = r.get(RB_COPY_DEST_BASE) & !0xFFF;
        let surface = SurfaceInfo(r.get(RB_SURFACE_INFO));
        let colour = ColorInfo(r.get(RB_COLOR_INFO));
        // The title reads the scene's depth back as an ordinary texture: a soft particle
        // fades where it meets geometry, and one that cannot find the depth does not fade
        // at all.
        if control.copies_depth() {
            // SAFETY: the caller's.
            unsafe { self.resolve_depth(vk, targets, f, r, &control, rect, destination, &surface) };
            return;
        }

        // The registers name the surface, and the last pass recorded is not always it: the
        // world's is resolved after a later pass has bound its own.
        let source_key = TargetKey {
            base_tile: colour.base_tile(),
            pitch: surface.pitch(),
            format: colour_format_for(colour.format()).as_raw() as u32,
            samples: surface.msaa_samples(),
            depth: false,
            scale: targets.pass_scale(r.get(RB_DEPTH_INFO), surface.msaa_samples()),
        };
        let Some(source) = targets.find(&source_key) else {
            f.skip(c"resolve of a surface never drawn into");
            clear_without_copy(targets, f, r, &control);
            submit_if_filling(f);
            return;
        };

        // A resolve can arrive with nothing recording -- the one before it may have
        // submitted a full arena share. Its surface is still there.
        if !f.recording() && !f.begin_frame() {
            f.skip(c"no command buffer");
            return;
        }
        self.resolves += 1;

        f.end_pass();
        f.pass_times_between();

        // The image is the destination surface RB_COPY_DEST_PITCH describes, not the
        // rectangle, and the rectangle lands at its own offset in it -- as the depth
        // resolve does. The depth of field resolves a 256x152 rectangle, rounded up to 8
        // rows, into a 256x150 surface and fetches 256x150: an image the size of the
        // rectangle put two rows of whatever the target held below the picture, and their
        // alpha, the blur weight, spread up into a dark bar along the bottom of the screen.
        let dest_pitch = CopyDestPitch(r.get(RB_COPY_DEST_PITCH));
        let dest_x = rect.offset.x as u32;
        let dest_y = rect.offset.y as u32;
        let mut surface_width =
            if dest_pitch.pitch() != 0 { dest_pitch.pitch().min(4096) } else { dest_x.wrapping_add(rect.extent.width) };
        if let Some(&fetched) = self.records.fetched_width.get(&destination)
            && fetched > dest_x
            && fetched < surface_width
        {
            surface_width = fetched;
        }
        let surface_height = if dest_pitch.height() != 0 {
            dest_pitch.height().min(4096)
        } else {
            dest_y.wrapping_add(rect.extent.height)
        };
        let room = |size: u32, at: u32| size.saturating_sub(at);
        let (source_width, source_height, source_scale, source_format, source_samples) = {
            let t = targets.get(source);
            (t.width, t.height, t.scale, t.format, t.samples)
        };
        let width = rect.extent.width.min(room(surface_width, dest_x)).min(room(source_width, dest_x));
        let height = rect.extent.height.min(room(surface_height, dest_y)).min(room(source_height, dest_y));
        let frames = f.frames();
        let resolve_images_made = &mut self.resolve_images_made;
        let into = resolve_copy_for(&mut self.records, destination, frames);
        let first_this_frame = into.written_frame != frames;
        if destination == 0
            || width == 0
            || height == 0
            // SAFETY: the caller's.
            || !unsafe {
                make_resolve_image(
                    vk,
                    f,
                    &targets.settings.memory,
                    targets.settings.physical,
                    resolve_images_made,
                    into,
                    surface_width,
                    surface_height,
                    source_scale,
                    source_format,
                    false,
                    first_this_frame,
                    false,
                )
            }
        {
            f.skip(c"resolve destination image");
            clear_without_copy(targets, f, r, &control);
            submit_if_filling(f);
            return;
        }
        into.written_frame = frames;
        into.from = source_key;
        let into_image = into.image;
        // After the draws that sampled this copy's last contents, which are not kept.
        record_use(into_image, vk::ImageAspectFlags::COLOR, &mut into.track, tracked::COPIED_TO, false);
        // The copy is of the surface's format and the rectangle lands where it sits, so a
        // multisampled surface is resolved straight into it, and a single-sampled one
        // copied.
        let at = vk::Offset2D { x: dest_x as i32, y: dest_y as i32 };
        if source_samples != vk::SampleCountFlags::TYPE_1 {
            resolve_colour_into(targets.get_mut(source), at, width, height, into_image);
        } else {
            // SAFETY: the caller's.
            let source_image = unsafe { targets.begin_reading_colour(vk, source, at, width, height) };
            #[cfg(feature = "diagnostics")]
            if source_image == vk::Image::null() {
                f.skip(c"colour resolve through the twin");
                // Draws still sample the copy (its contents undefined): it goes back to its
                // sampled layout.
                record_use(into_image, vk::ImageAspectFlags::COLOR, &mut into.track, tracked::SAMPLED, true);
                clear_without_copy(targets, f, r, &control);
                submit_if_filling(f);
                return;
            }
            let offset = scaled3(dest_x as i32, dest_y as i32, 0, source_scale);
            let copy = vk::ImageCopy {
                src_subresource: colour_layers(),
                src_offset: offset,
                dst_subresource: colour_layers(),
                dst_offset: offset,
                extent: vk::Extent3D {
                    width: width.wrapping_mul(source_scale),
                    height: height.wrapping_mul(source_scale),
                    depth: 1,
                },
            };
            recorder::command(move |c| {
                // SAFETY: the recorder's command buffer; both images live until a retire.
                unsafe {
                    c.vk.cmd_copy_image(
                        c.into,
                        source_image,
                        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                        into_image,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        &[copy],
                    )
                }
            });
        }
        // Sampled from here on: draws cannot bring it on themselves.
        record_use(into_image, vk::ImageAspectFlags::COLOR, &mut into.track, tracked::SAMPLED, true);
        f.pass_times_mark(format_args!(
            "colour resolve {}x{} {}x",
            width.wrapping_mul(source_scale),
            height.wrapping_mul(source_scale),
            source_samples.as_raw()
        ));
        targets.clear_after_resolve(r, &control);
        f.pass_times_mark(format_args!("clears after a resolve"));
        if !submit_if_filling(f) {
            f.skip(c"frame submission failed");
        }
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn resolve_depth<F: Frame>(
        &mut self,
        vk: &Vk,
        targets: &mut Targets,
        f: &mut F,
        r: &RegisterFile,
        control: &CopyControl,
        rect: vk::Rect2D,
        destination: u32,
        surface: &SurfaceInfo,
    ) {
        let depth_info = DepthInfo(r.get(RB_DEPTH_INFO));
        let format = targets.depth_format_for(depth_info.format()).as_raw() as u32;
        let scale = targets.pass_scale(r.get(RB_DEPTH_INFO), surface.msaa_samples());
        let key = targets.depth_key(depth_info.base_tile(), format, surface.msaa_samples(), scale);
        let Some(from) = targets.find(&key) else {
            f.skip(c"resolve of a depth surface never drawn into");
            clear_without_copy(targets, f, r, control);
            submit_if_filling(f);
            return;
        };
        if !f.recording() && !f.begin_frame() {
            f.skip(c"no command buffer");
            return;
        }
        self.resolves += 1;
        f.end_pass();
        f.pass_times_between();

        let (from_width, from_height, from_scale, from_format, from_samples) = {
            let t = targets.get(from);
            (t.width, t.height, t.scale, t.format, t.samples)
        };
        // The rectangle clamped to the surface, so the width less the offset does not
        // wrap when it starts past the edge. Across, Xenia's GetResolveInfo
        // (draw_util.cc) clamps x0 and x1 to the surface pitch and takes the region left
        // empty as a no-op: so does this. Down, Xenia clamps nothing and reads EDRAM on
        // below the surface, which the target image does not hold: a rectangle starting
        // past its height is refused here, the renderer's own rule, and counted apart.
        // Either is refused below as one starting at the edge is.
        let (x, y) = (rect.offset.x as u32, rect.offset.y as u32);
        if x > from_width || y > from_height {
            self.depth_resolve_wraps += 1;
            // At the right edge or past it Xenia's x clamp has already left it empty.
            if x < from_width {
                self.depth_resolve_wraps_below += 1;
            }
        }
        let mut width = rect.extent.width.min(from_width.saturating_sub(x));
        let mut height = rect.extent.height.min(from_height.saturating_sub(y));

        // The sun shadow map is one 1024x2048 surface holding two cascades, filled by two
        // resolves, the second describing itself as a surface of its own four megabytes
        // on; the world's shaders bind the whole thing at the first address. So a resolve
        // that lands inside a surface an earlier resolve this frame started is copied into
        // that surface's image, at the row it lands on.
        let dest_pitch = CopyDestPitch(r.get(RB_COPY_DEST_PITCH));
        let dest_info = CopyDestInfo(r.get(RB_COPY_DEST_INFO));
        let texel_bytes = (texture_format_of(dest_info.format()).bits_per_block / 8).max(1);
        let row_bytes = dest_pitch.pitch().wrapping_mul(texel_bytes);
        let frames = f.frames();
        let (mut surface_base, mut row) = (destination, 0u32);
        for (&base, known) in &self.surfaces {
            if known.frame == frames
                && base < destination
                && destination - base < known.bytes
                && known.row_bytes == row_bytes
                && row_bytes != 0
                && (destination - base).is_multiple_of(row_bytes)
            {
                surface_base = base;
                row = (destination - base) / row_bytes;
                break;
            }
        }
        if surface_base == destination && row_bytes != 0 {
            self.surfaces.insert(
                destination,
                ResolveSurface {
                    bytes: row_bytes.wrapping_mul(dest_pitch.height().min(8192)),
                    row_bytes,
                    frame: frames,
                },
            );
        }

        // A resolve lands where its rectangle sits, as Xenia's copy_dest_coordinate_info
        // has it: the shadow cascades are resolved from the draw scissor rounded down to 8,
        // which moves with the camera.
        let dest_x = x;
        let dest_y = row.wrapping_add(y);

        let resolve_images_made = &mut self.resolve_images_made;
        let into = resolve_copy_for(&mut self.records, surface_base, frames);
        // Sized for the whole surface, so the lower half has somewhere to go.
        let image_width = dest_x.wrapping_add(width).max(dest_pitch.pitch().min(4096));
        let image_height = dest_y.wrapping_add(height).max(dest_pitch.height().min(4096));
        let first_this_frame = into.written_frame != frames;
        // A multisampled surface is resolved straight into the copy where the rectangle
        // sits at the surface's first row, the copy an attachment for it, and one that is
        // not is made again only at the frame's first resolve into it; a copy landing
        // further down, or into such an image, goes through the surface's twin.
        let direct = from_samples != vk::SampleCountFlags::TYPE_1
            && row == 0
            && (first_this_frame || into.attachment_view != vk::ImageView::null());
        if destination == 0
            || width == 0
            || height == 0
            // SAFETY: the caller's.
            || !unsafe {
                make_resolve_image(
                    vk,
                    f,
                    &targets.settings.memory,
                    targets.settings.physical,
                    resolve_images_made,
                    into,
                    image_width,
                    image_height,
                    from_scale,
                    from_format,
                    true,
                    first_this_frame,
                    direct,
                )
            }
        {
            f.skip(c"depth resolve destination image");
            clear_without_copy(targets, f, r, control);
            submit_if_filling(f);
            return;
        }
        width = width.min(into.width.saturating_sub(dest_x));
        height = height.min(into.height.saturating_sub(dest_y));
        let keep = into.written_frame == frames;
        into.written_frame = frames;
        // The second cascade keeps the first: it comes out of the layout the first left it
        // in rather than out of UNDEFINED, which may discard.
        let (into_image, into_attachment) = (into.image, into.attachment_view);

        if direct {
            // SAFETY: the caller's; the copy's attachment view is of both aspects.
            unsafe {
                targets.resolve_depth_into(
                    vk,
                    f,
                    from,
                    rect.offset,
                    width,
                    height,
                    DepthInto::Copy { image: into_image, view: into_attachment, track: &mut into.track },
                    keep,
                    tracked::SAMPLED,
                )
            };
        } else {
            // SAFETY: the caller's.
            let from_image = unsafe { targets.begin_reading_depth(vk, f, from, rect.offset, width, height) };
            if from_image == vk::Image::null() {
                f.skip(c"depth resolve of a multisampled surface");
                clear_without_copy(targets, f, r, control);
                submit_if_filling(f);
                return;
            }
            // Layouts change for both aspects together.
            record_use(into_image, DEPTH_ASPECTS, &mut into.track, tracked::COPIED_TO, keep);

            // A copy rather than a blit: depth cannot be filtered on the way through. Of
            // the depth alone: the copy is sampled through a depth-only view.
            let depth_layers = vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::DEPTH,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            };
            let copy = vk::ImageCopy {
                src_subresource: depth_layers,
                src_offset: scaled3(rect.offset.x, rect.offset.y, 0, from_scale),
                dst_subresource: depth_layers,
                dst_offset: scaled3(dest_x as i32, dest_y as i32, 0, from_scale),
                extent: vk::Extent3D {
                    width: width.wrapping_mul(from_scale),
                    height: height.wrapping_mul(from_scale),
                    depth: 1,
                },
            };
            recorder::command(move |c| {
                // SAFETY: the recorder's command buffer; both images live until a retire.
                unsafe {
                    c.vk.cmd_copy_image(
                        c.into,
                        from_image,
                        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                        into_image,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        &[copy],
                    )
                }
            });

            record_use(into_image, DEPTH_ASPECTS, &mut into.track, tracked::SAMPLED, true);
        }
        f.pass_times_mark(format_args!(
            "depth resolve {}x{} {}x",
            width.wrapping_mul(from_scale),
            height.wrapping_mul(from_scale),
            from_samples.as_raw()
        ));
        self.depth_resolves += 1;
        targets.clear_after_resolve(r, control);
        f.pass_times_mark(format_args!("clears after a resolve"));
        if !submit_if_filling(f) {
            f.skip(c"frame submission failed");
        }
    }

    /// The frame the title swaps to: the front buffer the swap packet names, which a
    /// resolve filled like any other destination, blitted (LINEAR) into the present image
    /// once the window has taken what it last held, run through the display's colour
    /// table, left in TRANSFER_SRC for the window, and the frame finished.
    ///
    /// # Safety
    /// `vk` is the renderer's live device and the recorder's, of the physical device new
    /// was given; `targets` and `f` are the renderer's.
    pub unsafe fn swap<F: Frame>(&mut self, vk: &Vk, targets: &mut Targets, f: &mut F, front_buffer: u32) {
        let front = front_buffer & !0xFFF;
        let Some(ring) = self.records.resolved_to.get(&front) else { return };
        let newest = ring.newest as usize;
        let frame = ring.copies[newest];
        if frame.image == vk::Image::null() || frame.depth {
            return;
        }
        // Filtered as its format allows; a format that cannot be blitted out of is not
        // presented (format_support.rs).
        let Some(filter) = frame.blit else {
            f.skip(c"front buffer format cannot be blitted");
            return;
        };
        if !f.recording() && !f.begin_frame() {
            return;
        }
        f.end_pass();
        if let Some(source) = targets.find(&frame.from) {
            targets.get_mut(source).presented += 1;
        }

        // Copying the frame into an image of its own, rather than handing the resolve's
        // to the window thread, keeps the next frame from overwriting it.
        let present = f.present();
        wait_until_taken(present.serial);

        record_use(
            frame.image,
            vk::ImageAspectFlags::COLOR,
            copy_track(&mut self.records, front, newest),
            tracked::COPIED_FROM,
            true,
        );
        // After its last use, the last time round: the window's copy out of it and the
        // frame read back, both transfer reads in TRANSFER_SRC, which is where the swap
        // left it (its tracked use stands for theirs). Its contents are not kept.
        record_use(present.image, vk::ImageAspectFlags::COLOR, f.present_track(), tracked::COPIED_TO, false);

        let blit = vk::ImageBlit {
            src_subresource: colour_layers(),
            src_offsets: [
                vk::Offset3D { x: 0, y: 0, z: 0 },
                scaled3(frame.width as i32, frame.height as i32, 1, frame.scale),
            ],
            dst_subresource: colour_layers(),
            dst_offsets: [
                vk::Offset3D { x: 0, y: 0, z: 0 },
                vk::Offset3D { x: present.width as i32, y: present.height as i32, z: 1 },
            ],
        };
        let (frame_image, present_image) = (frame.image, present.image);
        recorder::command(move |c| {
            // SAFETY: the recorder's command buffer; the resolve's copy lives until a
            // retire, the present image until shutdown.
            unsafe {
                c.vk.cmd_blit_image(
                    c.into,
                    frame_image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    present_image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[blit],
                    filter,
                )
            }
        });
        // Draws may sample the front buffer again, and cannot bring it on themselves.
        record_use(
            frame.image,
            vk::ImageAspectFlags::COLOR,
            copy_track(&mut self.records, front, newest),
            tracked::SAMPLED,
            true,
        );

        // What a television is handed has been through the display's colour table, so
        // what the window and the frame dumps show has to be too.
        // SAFETY: the caller's; the present images are made with STORAGE usage.
        unsafe {
            self.display.apply(vk, self.physical, present.image, present.width, present.height, f.present_track())
        };

        // For the window's copy and the frame read back.
        record_use(present.image, vk::ImageAspectFlags::COLOR, f.present_track(), tracked::COPIED_FROM, true);
        f.finish_frame(present.image, targets, &mut self.records);
    }
}

/// What the copy `newest` of the ring at `front` was last used for.
fn copy_track(records: &mut ResolveRecords, front: u32, newest: usize) -> &mut TrackedImage {
    &mut records.resolved_to.get_mut(&front).expect("a ring found before").copies[newest].track
}

/// A resolve is a copy in the command stream and nothing more, so it does not end the
/// submission; only a slot whose arena share is more than half used goes early, so the
/// rest of the frame has room (submit_if_filling).
/// A resolve whose copy is refused here still clears what it asks to: Xenia's Resolve
/// (vulkan_render_target_cache.cc) clears after the copy whether or not the copy was made,
/// on every path past GetResolveInfo, its empty region and IssueCopy's BeginSubmission --
/// so with nothing recording a command buffer is begun for it, as BeginSubmission does,
/// and only where a clear is asked, so a resolve asking none records nothing more.
fn clear_without_copy<F: Frame>(targets: &mut Targets, f: &mut F, r: &RegisterFile, control: &CopyControl) {
    if !control.clears_color() && !control.clears_depth() {
        return;
    }
    if !f.recording() && !f.begin_frame() {
        f.skip(c"no command buffer");
        return;
    }
    f.end_pass();
    targets.clear_after_resolve(r, control);
}

fn submit_if_filling<F: Frame>(f: &mut F) -> bool {
    f.arena_used() <= f.arena_slot_bytes() / 2 || f.submit()
}

#[inline]
fn colour_layers() -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        mip_level: 0,
        base_array_layer: 0,
        layer_count: 1,
    }
}

/// The copy this frame's resolves write and this frame's draws read: the first resolve
/// of a frame moves on to the next one, the rest of the frame -- the second shadow
/// cascade, the draws that sample it -- get that same one back.
fn resolve_copy_for(records: &mut ResolveRecords, destination: u32, frames: u64) -> &mut Resolved {
    let ring = records.resolved_to.entry(destination).or_default();
    if ring.frame != frames {
        ring.frame = frames;
        ring.newest = (ring.newest + 1) % FRAME_SLOTS;
    }
    &mut ring.copies[ring.newest as usize]
}

/// A resolve image's objects, and the record emptied.
///
/// # Safety
/// `vk` made them; nothing uses them any more.
unsafe fn destroy_resolved(vk: &Vk, into: &mut Resolved) {
    // SAFETY: the caller's.
    unsafe {
        if into.view != vk::ImageView::null() {
            vk.destroy_image_view(into.view);
        }
        if into.gamma_view != vk::ImageView::null() {
            vk.destroy_image_view(into.gamma_view);
        }
        if into.attachment_view != vk::ImageView::null() {
            vk.destroy_image_view(into.attachment_view);
        }
        if into.image != vk::Image::null() {
            vk.destroy_image(into.image);
        }
        if into.memory != vk::DeviceMemory::null() {
            vk.free_memory(into.memory);
        }
    }
    *into = Resolved::default();
}

/// `into` made to hold a `width` by `height` copy at `scale` of `format`:
/// kept when it already is that (`exact`, the frame's first resolve into it, sizes it to
/// the surface exactly, as a fetch normalises its coordinates over the image), grown
/// within a frame otherwise, so the second shadow cascade lands inside the surface the
/// first one made. A replaced image goes after retire_before_destroy and forget_views. A
/// colour image gets a mutable format list and an sRGB view where its format has one; a
/// depth image a depth-only view, and with `attachment` a view of both aspects as well.
/// A format the device cannot sample gets no image, and the one there goes, so that the
/// textures at the destination are read from memory (format_support.rs).
///
/// # Safety
/// `vk` is the renderer's live device, `memory` its properties, `physical` its device.
#[allow(clippy::too_many_arguments)]
unsafe fn make_resolve_image<F: Frame>(
    vk: &Vk,
    f: &mut F,
    memory: &vk::PhysicalDeviceMemoryProperties,
    physical: vk::PhysicalDevice,
    made: &mut u64,
    into: &mut Resolved,
    mut width: u32,
    mut height: u32,
    scale: u32,
    format: vk::Format,
    depth: bool,
    exact: bool,
    attachment: bool,
) -> bool {
    let fits =
        if exact { into.width == width && into.height == height } else { into.width >= width && into.height >= height };
    let same = into.image != vk::Image::null()
        && into.format == format
        && into.depth == depth
        && into.scale == scale
        && (into.attachment_view != vk::ImageView::null() || !attachment);
    if same && fits {
        return true;
    }
    if !exact && same {
        width = width.max(into.width);
        height = height.max(into.height);
    }
    // SAFETY: the caller's.
    let features = |format: vk::Format| unsafe { vk.format_features(physical, format) };
    let usable = features(format);
    let sampled = usable.contains(vk::FormatFeatureFlags::SAMPLED_IMAGE)
        && (!attachment || usable.contains(vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT));
    if !sampled && into.image == vk::Image::null() {
        return false;
    }
    // Rare -- a resolve destination that has to grow -- and both a submitted slot and the
    // buffer being recorded may still name the image being replaced.
    if into.image != vk::Image::null() {
        f.retire_before_destroy();
    }
    // The texture cache's set holds these views; an entry on the freed image is a texture
    // read of unbacked memory, a GPU page fault that hangs the GPU. The cache's own check,
    // the view behind an id changed, misses a replacement that gets the old handle back.
    if into.image != vk::Image::null() {
        forget_views(f.textures());
    }
    // SAFETY: the caller's; the GPU, the recorder and the texture cache are done with it.
    unsafe { destroy_resolved(vk, into) };
    if !sampled {
        return false;
    }
    *made += 1;

    let mut info = vk::ImageCreateInfo {
        image_type: vk::ImageType::TYPE_2D,
        format,
        extent: vk::Extent3D { width: width.wrapping_mul(scale), height: height.wrapping_mul(scale), depth: 1 },
        mip_levels: 1,
        array_layers: 1,
        samples: vk::SampleCountFlags::TYPE_1,
        tiling: vk::ImageTiling::OPTIMAL,
        usage: vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED,
        sharing_mode: vk::SharingMode::EXCLUSIVE,
        initial_layout: vk::ImageLayout::UNDEFINED,
        ..Default::default()
    };
    if attachment {
        info.usage |= vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT;
    }
    // Two views of one image, so the format has to be allowed to differ from the one the
    // image was made with. The list is what lets a driver keep the compression it would
    // otherwise give up for a mutable image.
    let srgb = match vk::Format::from_raw(srgb_format_for(format.as_raw())) {
        srgb if !depth && features(srgb).contains(vk::FormatFeatureFlags::SAMPLED_IMAGE) => srgb,
        _ => vk::Format::UNDEFINED,
    };
    let both = [format, srgb];
    let list =
        vk::ImageFormatListCreateInfo { view_format_count: 2, p_view_formats: both.as_ptr(), ..Default::default() };
    if srgb != vk::Format::UNDEFINED {
        info.flags |= vk::ImageCreateFlags::MUTABLE_FORMAT;
        info.p_next = (&list as *const vk::ImageFormatListCreateInfo).cast();
    }
    // What the image's format allows, set before anything can fail: a copy left half made
    // (an image without its views) is still filtered and blitted as its format allows.
    into.filterable = format_support::filterable(usable);
    into.gamma_filterable = srgb == vk::Format::UNDEFINED || format_support::filterable(features(srgb));
    into.blit = format_support::blit_filter(usable);
    // SAFETY: the caller's; every structure lives across its call, the handles are made
    // here.
    unsafe {
        if vk.create_image(&info, &mut into.image) != vk::Result::SUCCESS {
            return false;
        }

        let needs = vk.get_image_memory_requirements(into.image);
        let kind = util::find_memory(memory, needs.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL);
        if kind == u32::MAX {
            return false;
        }
        let allocate =
            vk::MemoryAllocateInfo { allocation_size: needs.size, memory_type_index: kind, ..Default::default() };
        if vk.allocate_memory(&allocate, &mut into.memory) != vk::Result::SUCCESS {
            return false;
        }
        let _ = vk.bind_image_memory(into.image, into.memory, 0);

        let mut view = vk::ImageViewCreateInfo {
            image: into.image,
            view_type: vk::ImageViewType::TYPE_2D,
            format,
            // Depth only: a view carrying stencil as well cannot be sampled.
            subresource_range: vk::ImageSubresourceRange {
                aspect_mask: if depth { vk::ImageAspectFlags::DEPTH } else { vk::ImageAspectFlags::COLOR },
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            },
            ..Default::default()
        };
        if vk.create_image_view(&view, &mut into.view) != vk::Result::SUCCESS {
            return false;
        }
        if attachment {
            view.subresource_range.aspect_mask = DEPTH_ASPECTS;
            if vk.create_image_view(&view, &mut into.attachment_view) != vk::Result::SUCCESS {
                return false;
            }
        }
        if srgb != vk::Format::UNDEFINED {
            view.format = srgb;
            if vk.create_image_view(&view, &mut into.gamma_view) != vk::Result::SUCCESS {
                into.gamma_view = vk::ImageView::null();
            }
        }
    }
    into.format = format;
    into.width = width;
    into.height = height;
    into.scale = scale;
    into.depth = depth;
    log_info!(
        "renderer: %ux%u image for a %s resolve destination",
        width.wrapping_mul(scale),
        height.wrapping_mul(scale),
        if depth { c"depth" } else { c"colour" }
    );
    true
}
