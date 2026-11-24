// MW2_SUBGROUP_CHECK: whether a subgroup of pixel invocations ever holds two draws, on
// the device the renderer draws on, looked at once at initialise (diagnostics builds).
//
// The translated pixel shaders read their draw's record through a flat input made
// uniform again with OpGroupNonUniformBroadcastFirst (docs/shaders.md, "Finding the
// record"): right only if no subgroup holds the invocations of two draws. Vulkan does not
// promise it. The check draws 10^6 tiny adjacent triangles, each its own draw, made as
// the renderer makes a run of draws (renderer_draw.rs's make_run: vkCmdDrawMultiEXT and
// vkCmdDrawMultiIndexedEXT runs of fewer than maxMultiDrawCount, at most 2047, the record
// in BaseVertex; one vkCmdDraw or vkCmdDrawIndexed a draw without VK_EXT_multi_draw or
// under MW2_NO_MULTI_DRAW), with a pixel shader that writes
// broadcastFirst(record) != record into an R32_UINT attachment, which is read back. The
// record is written above that bit, and every pixel is held to the triangle that covers
// it, so that a check whose draws did not reach the pixels as laid out gives no verdict;
// so does a device without the basic and ballot subgroup operations in the fragment stage,
// where the pixel shader cannot run ("could not run").
// The control follows a clean check: the same triangles in one draw, each handing on
// its own first vertex as the record, which shows whether the device packs several
// triangles into one subgroup at all, and so whether the check could have seen a failure;
// the check passes only if it could ("no verdict" where the control saw no subgroup hold
// two triangles). MW2_SUBGROUP_CHECK=2 runs the control alone.
//
// Unsafe by nature: every call is Vulkan's, on the renderer's device, before anything
// else records on it (the recorder starts after); the queue is held under
// the shared pipeline's queue lock for the submission, every object is made and destroyed here,
// and destroyed only after the fence says the GPU is done with it (after a failed wait,
// a lost device, they are left as they are).
#![deny(unsafe_op_in_unsafe_fn)]

use super::QueueLock;
use super::device::Vk;
use super::spirv_builtin::{subgroup_check_pixel_shader, subgroup_check_vertex_shader};
use super::util::find_memory;
use crate::log::{log_info, log_warn};
use ash::vk;
use std::ffi::CStr;

const TRIANGLES: u32 = 1_000_000;
// Two triangles a cell of 2x2 pixels, the top-left and the bottom-right half, which
// share the cell's diagonal (each one to three pixels), 1024 cells a row.
const COLUMNS: u32 = 1024;
const CELL: u32 = 2;
const WIDTH: u32 = COLUMNS * CELL;
const HEIGHT: u32 = 1024;
const CELLS: u32 = TRIANGLES / 2;
const PIXELS: u64 = CELLS as u64 * (CELL * CELL) as u64;
const _: () = assert!(TRIANGLES.is_multiple_of(2) && CELLS.div_ceil(COLUMNS) * CELL <= HEIGHT);
// renderer_draw.rs's LONGEST_RUN: a run makes fewer draws than this and maxMultiDrawCount.
const LONGEST_RUN: u32 = 2048;
const UNDRAWN: u32 = u32::MAX;

/// MW2_SUBGROUP_CHECK: 0 off, 1 the check (and its control after a clean one), 2 the
/// control alone.
fn mode() -> u64 {
    crate::env::number(c"MW2_SUBGROUP_CHECK", 0)
}

#[derive(Default)]
struct Objects {
    pool: vk::CommandPool,
    command: vk::CommandBuffer,
    fence: vk::Fence,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    image: vk::Image,
    image_memory: vk::DeviceMemory,
    view: vk::ImageView,
    readback: vk::Buffer,
    readback_memory: vk::DeviceMemory,
    indices: vk::Buffer,
    indices_memory: vk::DeviceMemory,
    // Submitted and not seen done: the wait failed (a lost device), so nothing is destroyed.
    in_flight: bool,
}

impl Objects {
    /// # Safety
    /// `vk` made them; the GPU is done with them.
    unsafe fn destroy(&self, vk: &Vk) {
        // SAFETY: the caller's; each is null or made here.
        unsafe {
            if self.pipeline != vk::Pipeline::null() {
                vk.destroy_pipeline(self.pipeline);
            }
            if self.layout != vk::PipelineLayout::null() {
                vk.destroy_pipeline_layout(self.layout);
            }
            if self.view != vk::ImageView::null() {
                vk.destroy_image_view(self.view);
            }
            if self.image != vk::Image::null() {
                vk.destroy_image(self.image);
            }
            for buffer in [self.readback, self.indices] {
                if buffer != vk::Buffer::null() {
                    vk.destroy_buffer(buffer);
                }
            }
            for memory in [self.image_memory, self.readback_memory, self.indices_memory] {
                if memory != vk::DeviceMemory::null() {
                    vk.free_memory(memory);
                }
            }
            if self.fence != vk::Fence::null() {
                vk.destroy_fence(self.fence);
            }
            if self.pool != vk::CommandPool::null() {
                vk.destroy_command_pool(self.pool);
            }
        }
    }
}

/// Runs the check when MW2_SUBGROUP_CHECK asks for it, and says what it found.
///
/// # Safety
/// `vk` is the renderer's live device, of `physical`, with `queue` of `queue_family`;
/// nothing else records on the device yet.
pub unsafe fn run(
    vk: &Vk,
    physical: vk::PhysicalDevice,
    queue: vk::Queue,
    queue_family: u32,
    memory: &vk::PhysicalDeviceMemoryProperties,
) {
    let mode = mode();
    if mode == 0 {
        return;
    }
    if mode == 2 {
        // SAFETY: the caller's.
        unsafe { attempt(vk, physical, queue, queue_family, memory, true) };
        return;
    }
    // SAFETY (both attempts): the caller's; the first has destroyed its objects, or left
    // them to a lost device.
    match unsafe { attempt(vk, physical, queue, queue_family, memory, false) } {
        Some(Outcome::Shared(pixels)) => log_warn!(
            "renderer: subgroup check FAILED: subgroups held two draws (%llu pixels), so a pixel shader can read another draw's record",
            pixels
        ),
        Some(Outcome::Clean) => match unsafe { attempt(vk, physical, queue, queue_family, memory, true) } {
            Some(Outcome::Shared(_)) => log_info!(
                "renderer: subgroup check passed: no subgroup held two draws, where the control's held several triangles of one draw"
            ),
            Some(Outcome::Clean) => log_info!(
                "renderer: subgroup check: no subgroup held two draws, but the control's never held two triangles of one draw either, so the check could not fail on this device; no verdict"
            ),
            Some(Outcome::NoVerdict) | None => {
                log_warn!(
                    "renderer: subgroup check: no subgroup held two draws, but the control gave nothing; no verdict"
                )
            }
        },
        Some(Outcome::NoVerdict) | None => {}
    }
}

/// What a check or a control saw, where its triangles drew as laid out.
enum Outcome {
    /// No subgroup held two draws (the control: two triangles).
    Clean,
    /// Pixels given another draw's (the control: triangle's) record.
    Shared(u64),
    /// The triangles did not draw as laid out.
    NoVerdict,
}

/// One check, or the control, on objects of its own; None where it could not run.
///
/// # Safety
/// run's.
unsafe fn attempt(
    vk: &Vk,
    physical: vk::PhysicalDevice,
    queue: vk::Queue,
    queue_family: u32,
    memory: &vk::PhysicalDeviceMemoryProperties,
    control: bool,
) -> Option<Outcome> {
    let mut objects = Objects::default();
    // SAFETY: the caller's.
    let outcome = match unsafe { check(vk, physical, queue, queue_family, memory, control, &mut objects) } {
        Ok(outcome) => Some(outcome),
        Err(what) => {
            log_warn!(
                "renderer: subgroup check%s: could not run (%s)",
                if control { c" (control)" } else { c"" },
                what
            );
            None
        }
    };
    if !objects.in_flight {
        // SAFETY: check waited for its fence before it returned, or submitted nothing.
        unsafe { objects.destroy(vk) };
    }
    outcome
}

/// # Safety
/// run's.
unsafe fn check(
    vk: &Vk,
    physical: vk::PhysicalDevice,
    queue: vk::Queue,
    queue_family: u32,
    memory: &vk::PhysicalDeviceMemoryProperties,
    control: bool,
    o: &mut Objects,
) -> Result<Outcome, &'static CStr> {
    let ok = |result: vk::Result, what: &'static CStr| if result == vk::Result::SUCCESS { Ok(()) } else { Err(what) };
    let mut subgroup = vk::PhysicalDeviceSubgroupProperties::default();
    let mut properties = vk::PhysicalDeviceProperties2 {
        p_next: (&mut subgroup as *mut vk::PhysicalDeviceSubgroupProperties).cast(),
        ..Default::default()
    };
    // SAFETY: the caller's physical device; the chain lives across the call.
    unsafe { vk.get_physical_device_properties2(physical, &mut properties) };
    // The pixel shader's GroupNonUniform and GroupNonUniformBallot (OpGroupNonUniformBroadcastFirst)
    // need the basic and ballot operations in the fragment stage, which Vulkan 1.1 does not
    // require (only the basic ones, in compute): without them the pipeline is invalid usage
    // and anything it draws means nothing, so there is no verdict.
    log_info!(
        "renderer: subgroup check: subgroups of %u, operations 0x%x in stages 0x%x",
        subgroup.subgroup_size,
        subgroup.supported_operations.as_raw(),
        subgroup.supported_stages.as_raw()
    );
    if !supports_pixel_ballot(&subgroup) {
        return Err(c"no ballot subgroup operations in the pixel stage");
    }

    // SAFETY (every call below): the caller's device; each structure lives across its call.
    unsafe {
        let pool = vk::CommandPoolCreateInfo { queue_family_index: queue_family, ..Default::default() };
        ok(vk.create_command_pool(&pool, &mut o.pool), c"command pool")?;
        let allocate = vk::CommandBufferAllocateInfo {
            command_pool: o.pool,
            level: vk::CommandBufferLevel::PRIMARY,
            command_buffer_count: 1,
            ..Default::default()
        };
        ok(vk.allocate_command_buffers(&allocate, std::slice::from_mut(&mut o.command)), c"command buffer")?;
        ok(vk.create_fence(&vk::FenceCreateInfo::default(), &mut o.fence), c"fence")?;

        // The attachment, R32_UINT (every device draws into it), and where it is read back.
        let image = vk::ImageCreateInfo {
            image_type: vk::ImageType::TYPE_2D,
            format: vk::Format::R32_UINT,
            extent: vk::Extent3D { width: WIDTH, height: HEIGHT, depth: 1 },
            mip_levels: 1,
            array_layers: 1,
            samples: vk::SampleCountFlags::TYPE_1,
            tiling: vk::ImageTiling::OPTIMAL,
            usage: vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC,
            sharing_mode: vk::SharingMode::EXCLUSIVE,
            initial_layout: vk::ImageLayout::UNDEFINED,
            ..Default::default()
        };
        ok(vk.create_image(&image, &mut o.image), c"image")?;
        let needs = vk.get_image_memory_requirements(o.image);
        o.image_memory = allocate_memory(vk, memory, needs, vk::MemoryPropertyFlags::DEVICE_LOCAL)?;
        ok(vk.bind_image_memory(o.image, o.image_memory, 0), c"image memory")?;
        let colour = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            level_count: 1,
            layer_count: 1,
            ..Default::default()
        };
        let view = vk::ImageViewCreateInfo {
            image: o.image,
            view_type: vk::ImageViewType::TYPE_2D,
            format: vk::Format::R32_UINT,
            subresource_range: colour,
            ..Default::default()
        };
        ok(vk.create_image_view(&view, &mut o.view), c"image view")?;
        let bytes = u64::from(WIDTH) * u64::from(HEIGHT) * 4;
        o.readback = make_buffer(vk, bytes, vk::BufferUsageFlags::TRANSFER_DST)?;
        let needs = vk.get_buffer_memory_requirements(o.readback);
        // Coherent, so the host reads what the copy wrote once the fence is signalled.
        o.readback_memory = allocate_memory(
            vk,
            memory,
            needs,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        )?;
        ok(vk.bind_buffer_memory(o.readback, o.readback_memory, 0), c"readback memory")?;
        // A triangle's three indices, for the indexed half.
        o.indices = make_buffer(vk, 8, vk::BufferUsageFlags::INDEX_BUFFER | vk::BufferUsageFlags::TRANSFER_DST)?;
        let needs = vk.get_buffer_memory_requirements(o.indices);
        o.indices_memory = allocate_memory(vk, memory, needs, vk::MemoryPropertyFlags::DEVICE_LOCAL)?;
        ok(vk.bind_buffer_memory(o.indices, o.indices_memory, 0), c"index memory")?;

        o.pipeline = make_pipeline(vk, control, &mut o.layout)?;

        let begin =
            vk::CommandBufferBeginInfo { flags: vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT, ..Default::default() };
        let c = o.command;
        ok(vk.begin_command_buffer(c, &begin), c"command buffer begin")?;
        let indices: [u16; 4] = [0, 1, 2, 0];
        let index_bytes: Vec<u8> = indices.iter().flat_map(|index| index.to_le_bytes()).collect();
        vk.cmd_update_buffer(c, o.indices, 0, &index_bytes);
        let index_ready = vk::BufferMemoryBarrier {
            src_access_mask: vk::AccessFlags::TRANSFER_WRITE,
            dst_access_mask: vk::AccessFlags::INDEX_READ,
            src_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
            dst_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
            buffer: o.indices,
            offset: 0,
            size: vk::WHOLE_SIZE,
            ..Default::default()
        };
        let to_attachment = vk::ImageMemoryBarrier {
            src_access_mask: vk::AccessFlags::empty(),
            dst_access_mask: vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            old_layout: vk::ImageLayout::UNDEFINED,
            new_layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            src_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
            dst_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
            image: o.image,
            subresource_range: colour,
            ..Default::default()
        };
        vk.cmd_pipeline_barrier(
            c,
            vk::PipelineStageFlags::TRANSFER | vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::PipelineStageFlags::VERTEX_INPUT | vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            vk::DependencyFlags::empty(),
            &[],
            &[index_ready],
            &[to_attachment],
        );

        let attachment = vk::RenderingAttachmentInfo {
            image_view: o.view,
            image_layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            load_op: vk::AttachmentLoadOp::CLEAR,
            store_op: vk::AttachmentStoreOp::STORE,
            clear_value: vk::ClearValue { color: vk::ClearColorValue { uint32: [UNDRAWN; 4] } },
            ..Default::default()
        };
        let area =
            vk::Rect2D { offset: vk::Offset2D::default(), extent: vk::Extent2D { width: WIDTH, height: HEIGHT } };
        let rendering = vk::RenderingInfo {
            render_area: area,
            layer_count: 1,
            color_attachment_count: 1,
            p_color_attachments: &attachment,
            ..Default::default()
        };
        vk.cmd_begin_rendering(c, &rendering);
        vk.cmd_bind_pipeline(c, vk::PipelineBindPoint::GRAPHICS, o.pipeline);
        let viewport =
            vk::Viewport { x: 0.0, y: 0.0, width: WIDTH as f32, height: HEIGHT as f32, min_depth: 0.0, max_depth: 1.0 };
        vk.cmd_set_viewport(c, 0, &[viewport]);
        vk.cmd_set_scissor(c, 0, &[area]);
        vk.cmd_bind_index_buffer(c, o.indices, 0, vk::IndexType::UINT16);
        let made = record_draws(vk, c, control);
        vk.cmd_end_rendering(c);

        let to_copy = vk::ImageMemoryBarrier {
            src_access_mask: vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            dst_access_mask: vk::AccessFlags::TRANSFER_READ,
            old_layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            new_layout: vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            ..to_attachment
        };
        vk.cmd_pipeline_barrier(
            c,
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[to_copy],
        );
        let region = vk::BufferImageCopy {
            image_subresource: vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            },
            image_extent: vk::Extent3D { width: WIDTH, height: HEIGHT, depth: 1 },
            ..Default::default()
        };
        vk.cmd_copy_image_to_buffer(c, o.image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, o.readback, &[region]);
        let to_host = vk::BufferMemoryBarrier {
            src_access_mask: vk::AccessFlags::TRANSFER_WRITE,
            dst_access_mask: vk::AccessFlags::HOST_READ,
            buffer: o.readback,
            ..index_ready
        };
        vk.cmd_pipeline_barrier(
            c,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::HOST,
            vk::DependencyFlags::empty(),
            &[],
            &[to_host],
            &[],
        );
        ok(vk.end_command_buffer(c), c"command buffer end")?;

        let submit = vk::SubmitInfo { command_buffer_count: 1, p_command_buffers: &c, ..Default::default() };
        let started = std::time::Instant::now();
        {
            let _queue = QueueLock::hold();
            ok(vk.queue_submit(queue, &[submit], o.fence), c"submission")?;
        }
        o.in_flight = true;
        ok(vk.wait_for_fences(&[o.fence], true, u64::MAX), c"the wait for the GPU")?;
        o.in_flight = false;
        let milliseconds = started.elapsed().as_secs_f64() * 1000.0;

        let mut mapped = std::ptr::null_mut();
        ok(vk.map_memory(o.readback_memory, 0, bytes, &mut mapped), c"readback map")?;
        // SAFETY: the mapping holds `bytes` bytes, which the copy wrote and the fence made
        // visible (coherent memory); nothing writes them while they are read.
        let texels = std::slice::from_raw_parts(mapped.cast::<u32>(), (bytes / 4) as usize);
        let counts = count(texels);
        vk.unmap_memory(o.readback_memory);
        Ok(say(&subgroup, control, &made, &counts, milliseconds))
    }
}

/// Whether the device runs the check's pixel shader: subgroup operations in the fragment
/// stage, the basic ones and ballot's.
fn supports_pixel_ballot(subgroup: &vk::PhysicalDeviceSubgroupProperties) -> bool {
    subgroup.supported_stages.contains(vk::ShaderStageFlags::FRAGMENT)
        && subgroup.supported_operations.contains(vk::SubgroupFeatureFlags::BASIC | vk::SubgroupFeatureFlags::BALLOT)
}

/// How the draws were made, for the report line.
struct Made {
    alone: u32,
    runs: u32,
    in_runs: u32,
    longest: u32,
}

/// The draws: the first half without indices, the second with, each run made as
/// renderer_draw.rs's make_run makes one; the control one draw of every triangle.
///
/// # Safety
/// `c` is recording on `vk`'s device, inside the rendering instance, the pipeline and the
/// index buffer bound.
unsafe fn record_draws(vk: &Vk, c: vk::CommandBuffer, control: bool) -> Made {
    // A run makes fewer draws than maxMultiDrawCount (renderer_draw.rs's draw).
    let longest = vk.multi_draw().min(LONGEST_RUN).saturating_sub(1);
    let mut made = Made { alone: 0, runs: 0, in_runs: 0, longest };
    // SAFETY (every call): the caller's.
    unsafe {
        if control {
            vk.cmd_draw(c, TRIANGLES * 3, 1, 0, 0);
            made.alone = 1;
            return made;
        }
        let half = TRIANGLES / 2;
        let mut draws: Vec<vk::MultiDrawInfoEXT> = Vec::with_capacity(longest as usize);
        let mut indexed: Vec<vk::MultiDrawIndexedInfoEXT> = Vec::with_capacity(longest as usize);
        let mut k = 0;
        while k < TRIANGLES {
            let end = if k < half { half } else { TRIANGLES };
            let count = if longest >= 2 { longest.min(end - k) } else { 1 };
            if count == 1 {
                made.alone += 1;
                if k < half {
                    vk.cmd_draw(c, 3, 1, k * 3, 0);
                } else {
                    vk.cmd_draw_indexed(c, 3, 1, 0, (k * 3) as i32, 0);
                }
            } else {
                made.runs += 1;
                made.in_runs += count;
                if k < half {
                    draws.clear();
                    draws.extend((k..k + count).map(|t| vk::MultiDrawInfoEXT { first_vertex: t * 3, vertex_count: 3 }));
                    vk.cmd_draw_multi(c, &draws, 1, 0);
                } else {
                    indexed.clear();
                    indexed.extend((k..k + count).map(|t| vk::MultiDrawIndexedInfoEXT {
                        first_index: 0,
                        index_count: 3,
                        vertex_offset: (t * 3) as i32,
                    }));
                    vk.cmd_draw_multi_indexed(c, &indexed, 1, 0, None);
                }
            }
            k += count;
        }
    }
    made
}

/// What the pixels say: [the record broadcast agreed, another record broadcast, a pixel of
/// a cell left undrawn, anything else drawn: a cell's pixel by a triangle not covering it,
/// or a pixel outside the cells]. A cell's top-left pixel is its first triangle's and its
/// bottom-right its second's; the two on the shared edge either's, as Vulkan's tie-break
/// leaves to the device, so long as it is one of the cell's two. The margin outside the
/// cells, left undrawn as it should be, is counted nowhere.
fn count(texels: &[u32]) -> [u64; 4] {
    let mut counts = [0u64; 4];
    for (at, &texel) in texels.iter().enumerate() {
        let (x, y) = (at as u32 % WIDTH, at as u32 / WIDTH);
        let cell = (y / CELL) * COLUMNS + x / CELL;
        let kind = if cell >= CELLS {
            if texel == UNDRAWN {
                continue;
            }
            3
        } else if texel == UNDRAWN {
            2
        } else {
            let first = cell * 2 * 3;
            let second = first + 3;
            let (inside_x, inside_y) = (x % CELL, y % CELL);
            let record = texel >> 1;
            let laid_out = match (inside_x, inside_y) {
                (0, 0) => record == first,
                (1, 1) => record == second,
                _ => record == first || record == second,
            };
            if !laid_out { 3 } else { (texel & 1) as usize }
        };
        counts[kind] += 1;
    }
    counts
}

fn say(
    subgroup: &vk::PhysicalDeviceSubgroupProperties,
    control: bool,
    made: &Made,
    counts: &[u64; 4],
    milliseconds: f64,
) -> Outcome {
    // Every pixel laid out drawn by its own triangle, nothing else drawn.
    let drawn = counts[0] + counts[1];
    log_info!(
        "renderer: subgroup check%s: %u triangles, %u draws alone and %u in %u multi-draw runs of up to %u, subgroups of %u, the GPU %.0f ms: %llu of %llu pixels drawn, %llu given another %s's record",
        if control { c" (control: one draw, a record a triangle)" } else { c"" },
        TRIANGLES,
        made.alone,
        made.in_runs,
        made.runs,
        made.longest,
        subgroup.subgroup_size,
        milliseconds,
        drawn,
        PIXELS,
        counts[1],
        if control { c"triangle" } else { c"draw" }
    );
    if drawn != PIXELS || counts[2] != 0 || counts[3] != 0 {
        log_warn!(
            "renderer: subgroup check: the triangles did not draw as laid out (%llu pixels drawn by their own triangle, %llu left undrawn, %llu drawn wrong); no verdict",
            drawn,
            counts[2],
            counts[3]
        );
        return Outcome::NoVerdict;
    }
    if control {
        if counts[1] != 0 {
            log_info!("renderer: subgroup check (control): a subgroup holds several triangles of one draw");
        } else {
            log_info!("renderer: subgroup check (control): no subgroup held two triangles of one draw");
        }
    }
    if counts[1] == 0 { Outcome::Clean } else { Outcome::Shared(counts[1]) }
}

/// # Safety
/// `vk` is a live device.
unsafe fn make_buffer(vk: &Vk, size: u64, usage: vk::BufferUsageFlags) -> Result<vk::Buffer, &'static CStr> {
    let info = vk::BufferCreateInfo { size, usage, sharing_mode: vk::SharingMode::EXCLUSIVE, ..Default::default() };
    let mut buffer = vk::Buffer::null();
    // SAFETY: the caller's.
    if unsafe { vk.create_buffer(&info, &mut buffer) } != vk::Result::SUCCESS {
        return Err(c"buffer");
    }
    Ok(buffer)
}

/// # Safety
/// `vk` is a live device of `memory`'s physical device.
unsafe fn allocate_memory(
    vk: &Vk,
    memory: &vk::PhysicalDeviceMemoryProperties,
    needs: vk::MemoryRequirements,
    want: vk::MemoryPropertyFlags,
) -> Result<vk::DeviceMemory, &'static CStr> {
    let kind = find_memory(memory, needs.memory_type_bits, want);
    if kind == u32::MAX {
        return Err(c"memory type");
    }
    let info = vk::MemoryAllocateInfo { allocation_size: needs.size, memory_type_index: kind, ..Default::default() };
    let mut out = vk::DeviceMemory::null();
    // SAFETY: the caller's.
    if unsafe { vk.allocate_memory(&info, &mut out) } != vk::Result::SUCCESS {
        return Err(c"memory");
    }
    Ok(out)
}

/// The check's pipeline (an R32_UINT attachment, no blending, no culling, the viewport and
/// scissor dynamic) and its empty layout.
///
/// # Safety
/// `vk` is a live device.
unsafe fn make_pipeline(
    vk: &Vk,
    control: bool,
    layout: &mut vk::PipelineLayout,
) -> Result<vk::Pipeline, &'static CStr> {
    let vertex_words = subgroup_check_vertex_shader(control, COLUMNS as i32, CELL as i32, WIDTH, HEIGHT);
    let pixel_words = subgroup_check_pixel_shader();
    // MW2_DUMP_SHADERS: both, for spirv-val.
    super::trace::dump_builtin("subgroup_check_vertex", &vertex_words);
    super::trace::dump_builtin("subgroup_check_pixel", &pixel_words);
    // SAFETY (every call): the caller's; each structure lives across its call.
    unsafe {
        if vk.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default(), layout) != vk::Result::SUCCESS {
            return Err(c"pipeline layout");
        }
        let mut modules = [vk::ShaderModule::null(); 2];
        for (module, words) in modules.iter_mut().zip([&vertex_words, &pixel_words]) {
            let info =
                vk::ShaderModuleCreateInfo { code_size: words.len() * 4, p_code: words.as_ptr(), ..Default::default() };
            let _ = vk.create_shader_module(&info, module);
        }
        let stages: [vk::PipelineShaderStageCreateInfo; 2] =
            std::array::from_fn(|i| vk::PipelineShaderStageCreateInfo {
                stage: if i != 0 { vk::ShaderStageFlags::FRAGMENT } else { vk::ShaderStageFlags::VERTEX },
                module: modules[i],
                p_name: c"main".as_ptr(),
                ..Default::default()
            });
        let input = vk::PipelineVertexInputStateCreateInfo::default();
        let assembly = vk::PipelineInputAssemblyStateCreateInfo {
            topology: vk::PrimitiveTopology::TRIANGLE_LIST,
            ..Default::default()
        };
        let viewport =
            vk::PipelineViewportStateCreateInfo { viewport_count: 1, scissor_count: 1, ..Default::default() };
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
        let depth = vk::PipelineDepthStencilStateCreateInfo::default();
        let blend_attachment = vk::PipelineColorBlendAttachmentState {
            color_write_mask: vk::ColorComponentFlags::R,
            ..Default::default()
        };
        let blend = vk::PipelineColorBlendStateCreateInfo {
            attachment_count: 1,
            p_attachments: &blend_attachment,
            ..Default::default()
        };
        let dynamics = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo {
            dynamic_state_count: 2,
            p_dynamic_states: dynamics.as_ptr(),
            ..Default::default()
        };
        let format = vk::Format::R32_UINT;
        let rendering = vk::PipelineRenderingCreateInfo {
            color_attachment_count: 1,
            p_color_attachment_formats: &format,
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
            layout: *layout,
            ..Default::default()
        };
        let mut made = [vk::Pipeline::null()];
        let made_ok = modules.iter().all(|&module| module != vk::ShaderModule::null())
            && vk.create_graphics_pipelines(vk::PipelineCache::null(), &[info], &mut made) == vk::Result::SUCCESS;
        for module in modules {
            if module != vk::ShaderModule::null() {
                vk.destroy_shader_module(module);
            }
        }
        if !made_ok {
            return Err(c"pipeline");
        }
        Ok(made[0])
    }
}
