// The display's colour lookup table.
//
// The console's scanout hardware runs the front buffer through a 256-entry table of ten
// bits per channel before it reaches the screen, and the title fills that table -- MW2
// writes all 256 entries three times in a run, and the curve is not the identity. Vulkan
// has no fixed-function equivalent and a blit cannot do a lookup, so this is a compute
// pass over the presented image, in the frame's command stream (the recorder): the table
// is copied into a 256x1 image from a buffer the command buffer itself fills
// (vkCmdUpdateBuffer), so a frame still in flight never has its copy's source rewritten
// under it, and the shader (spirv_builtin.rs) reads it with one texel fetch a channel.
//
// Unsafe by nature: every object is made on the renderer's device and used by the frames
// it records; a presented image's set is made once and kept, and nothing is destroyed but
// at shutdown, after the renderer's last frame.
#![deny(unsafe_op_in_unsafe_fn)]

use super::device::Vk;
use super::recorder;
use super::spirv_builtin::{DISPLAY_GROUP, display_table_shader};
use super::tracked::{self, TrackedImage};
use super::util;
use crate::log::{log_info, log_warn};
use ash::vk;

use crate::runtime::gpu::command_processor::display_colour_table;

/// The most presented images the table is applied to, each with a set of its own made on
/// first use and kept. The renderer presents from four images made once (renderer.rs);
/// a frame of a ninth goes out without the table, counted (`refused`, in the report), and
/// one whose set could not be made without it too.
const MOST_IMAGES: usize = 8;

/// Ten bits a channel down to eight, rounded: (ten * 255 + 511) / 1023. The alpha byte is
/// unused, 255. The bytes the table image is filled with, entry by entry, RGBA.
pub fn table_bytes(wanted: &[u32; 256]) -> [u8; 256 * 4] {
    #[inline]
    fn eight(ten: u32) -> u8 {
        ((ten * 255 + 511) / 1023) as u8
    }
    let mut bytes = [0u8; 256 * 4];
    for (entry, out) in wanted.iter().zip(bytes.chunks_exact_mut(4)) {
        out[0] = eight((entry >> 20) & 0x3FF);
        out[1] = eight((entry >> 10) & 0x3FF);
        out[2] = eight(entry & 0x3FF);
        out[3] = 255;
    }
    bytes
}

pub struct DisplayTable {
    // initialise has run with a device: shutdown has work.
    started: bool,
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    pool: vk::DescriptorPool,
    // The table as a 256x1 image, so the shader reads it with one texel fetch and no
    // sampler, and the buffer the command buffer fills it from.
    table: vk::Image,
    table_memory: vk::DeviceMemory,
    table_view: vk::ImageView,
    staging: vk::Buffer,
    staging_memory: vk::DeviceMemory,
    // One set per presented image, so a set is not rewritten while the frame that uses
    // it is still in flight.
    sets: Vec<vk::DescriptorSet>,
    views: Vec<vk::ImageView>,
    for_image: Vec<vk::Image>,
    held: [u32; 256], // what the table image currently holds
    have_table: bool,
    /// What the table's image was last used for (tracked.rs).
    table_track: TrackedImage,
    applied: u64,
    uploads: u64,
    refused: u64,
    failed: bool,
}

impl Default for DisplayTable {
    fn default() -> Self {
        DisplayTable::new()
    }
}

impl DisplayTable {
    pub const fn new() -> DisplayTable {
        DisplayTable {
            started: false,
            set_layout: vk::DescriptorSetLayout::null(),
            layout: vk::PipelineLayout::null(),
            pipeline: vk::Pipeline::null(),
            pool: vk::DescriptorPool::null(),
            table: vk::Image::null(),
            table_memory: vk::DeviceMemory::null(),
            table_view: vk::ImageView::null(),
            staging: vk::Buffer::null(),
            staging_memory: vk::DeviceMemory::null(),
            sets: Vec::new(),
            views: Vec::new(),
            for_image: Vec::new(),
            held: [0; 256],
            have_table: false,
            table_track: TrackedImage::NEW,
            applied: 0,
            uploads: 0,
            refused: 0,
            failed: false,
        }
    }

    /// The table's image, its view and the buffer it is filled from.
    ///
    /// # Safety
    /// `vk` is the renderer's live device, of `physical`.
    unsafe fn make_table(&mut self, vk: &Vk, physical: vk::PhysicalDevice) -> bool {
        // SAFETY: the caller's; every handle below is this device's, made here.
        unsafe {
            let memory = vk.get_physical_device_memory_properties(physical);
            let info = vk::ImageCreateInfo {
                image_type: vk::ImageType::TYPE_2D,
                format: vk::Format::R8G8B8A8_UNORM,
                extent: vk::Extent3D { width: 256, height: 1, depth: 1 },
                mip_levels: 1,
                array_layers: 1,
                samples: vk::SampleCountFlags::TYPE_1,
                tiling: vk::ImageTiling::OPTIMAL,
                usage: vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_DST,
                initial_layout: vk::ImageLayout::UNDEFINED,
                ..Default::default()
            };
            if vk.create_image(&info, &mut self.table) != vk::Result::SUCCESS {
                return false;
            }

            let mut needs = vk.get_image_memory_requirements(self.table);
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
            if vk.allocate_memory(&allocate, &mut self.table_memory) != vk::Result::SUCCESS {
                return false;
            }
            let _ = vk.bind_image_memory(self.table, self.table_memory, 0);

            let view = vk::ImageViewCreateInfo {
                image: self.table,
                view_type: vk::ImageViewType::TYPE_2D,
                format: vk::Format::R8G8B8A8_UNORM,
                subresource_range: vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                },
                ..Default::default()
            };
            if vk.create_image_view(&view, &mut self.table_view) != vk::Result::SUCCESS {
                return false;
            }

            let buffer = vk::BufferCreateInfo {
                size: 256 * 4,
                usage: vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST,
                ..Default::default()
            };
            if vk.create_buffer(&buffer, &mut self.staging) != vk::Result::SUCCESS {
                return false;
            }
            needs = vk.get_buffer_memory_requirements(self.staging);
            allocate.allocation_size = needs.size;
            allocate.memory_type_index =
                util::find_memory(&memory, needs.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL);
            if allocate.memory_type_index == u32::MAX {
                return false;
            }
            if vk.allocate_memory(&allocate, &mut self.staging_memory) != vk::Result::SUCCESS {
                return false;
            }
            vk.bind_buffer_memory(self.staging, self.staging_memory, 0) == vk::Result::SUCCESS
        }
    }

    /// The pipeline, its layouts, the pool and the table, once; false for good after a
    /// failure. The presented image has to be made with STORAGE usage for this.
    ///
    /// # Safety
    /// `vk` is the renderer's live device, of `physical`.
    pub unsafe fn initialise(&mut self, vk: &Vk, physical: vk::PhysicalDevice) -> bool {
        if self.pipeline != vk::Pipeline::null() {
            return true;
        }
        if self.failed {
            return false;
        }
        self.started = true;

        // SAFETY: the caller's; every handle below is this device's, made here.
        unsafe {
            let bindings: [vk::DescriptorSetLayoutBinding; 2] =
                std::array::from_fn(|i| vk::DescriptorSetLayoutBinding {
                    binding: i as u32,
                    descriptor_type: vk::DescriptorType::STORAGE_IMAGE,
                    descriptor_count: 1,
                    stage_flags: vk::ShaderStageFlags::COMPUTE,
                    ..Default::default()
                });
            let set_info = vk::DescriptorSetLayoutCreateInfo {
                binding_count: 2,
                p_bindings: bindings.as_ptr(),
                ..Default::default()
            };
            if vk.create_descriptor_set_layout(&set_info, &mut self.set_layout) != vk::Result::SUCCESS {
                self.failed = true;
                return false;
            }

            let layout_info = vk::PipelineLayoutCreateInfo {
                set_layout_count: 1,
                p_set_layouts: &self.set_layout,
                ..Default::default()
            };
            if vk.create_pipeline_layout(&layout_info, &mut self.layout) != vk::Result::SUCCESS {
                self.failed = true;
                return false;
            }

            let words = display_table_shader();
            #[cfg(feature = "diagnostics")]
            super::trace::dump_builtin("display_table", &words); // MW2_DUMP_SHADERS
            let module_info =
                vk::ShaderModuleCreateInfo { code_size: words.len() * 4, p_code: words.as_ptr(), ..Default::default() };
            let mut module = vk::ShaderModule::null();
            if vk.create_shader_module(&module_info, &mut module) != vk::Result::SUCCESS {
                log_warn!("display: the driver refused the colour table shader");
                self.failed = true;
                return false;
            }

            let pipeline_info = vk::ComputePipelineCreateInfo {
                stage: vk::PipelineShaderStageCreateInfo {
                    stage: vk::ShaderStageFlags::COMPUTE,
                    module,
                    p_name: c"main".as_ptr(),
                    ..Default::default()
                },
                layout: self.layout,
                ..Default::default()
            };
            let mut made = [vk::Pipeline::null()];
            let r = vk.create_compute_pipelines(vk::PipelineCache::null(), &[pipeline_info], &mut made);
            self.pipeline = made[0];
            vk.destroy_shader_module(module);
            if r != vk::Result::SUCCESS {
                log_warn!("display: no colour table pipeline");
                self.failed = true;
                return false;
            }

            let size = vk::DescriptorPoolSize { ty: vk::DescriptorType::STORAGE_IMAGE, descriptor_count: 16 };
            let pool_info = vk::DescriptorPoolCreateInfo {
                max_sets: MOST_IMAGES as u32,
                pool_size_count: 1,
                p_pool_sizes: &size,
                ..Default::default()
            };
            if vk.create_descriptor_pool(&pool_info, &mut self.pool) != vk::Result::SUCCESS {
                self.failed = true;
                return false;
            }

            if !self.make_table(vk, physical) {
                log_warn!("display: no colour table image");
                self.failed = true;
                return false;
            }
        }
        log_info!("display: the title's colour table will be applied to the presented frame");
        true
    }

    /// True once the pipeline is made and nothing has failed.
    pub fn ready(&self) -> bool {
        self.pipeline != vk::Pipeline::null() && !self.failed
    }

    /// Applies the table to `image` in place, in the frame's command stream. `track` is
    /// what the image was last used for; it is left in the compute program's use, for the
    /// caller's next use to bring on (tracked.rs). Before the title has filled
    /// the table there is nothing to apply and the frame goes out as it is.
    ///
    /// # Safety
    /// `vk` is the renderer's live device, of `physical`, the recorder's too; `image` is
    /// that device's, `width` by `height`, made with STORAGE usage, and outlives the
    /// frames it is recorded into.
    pub unsafe fn apply(
        &mut self,
        vk: &Vk,
        physical: vk::PhysicalDevice,
        image: vk::Image,
        width: u32,
        height: u32,
        track: &mut TrackedImage,
    ) {
        // SAFETY: the caller's.
        if !self.ready() && !unsafe { self.initialise(vk, physical) } {
            return;
        }

        let Some(wanted) = display_colour_table() else { return };

        if !self.have_table || wanted != self.held {
            let bytes = table_bytes(&wanted);
            // After the last frame's copy out of the buffer and its shader's reads of the
            // table, both of which may still be running.
            let staging = self.staging;
            let table = self.table;
            // Overwritten whole, so its contents are not kept; then read by the program.
            let writing = self.table_track.enter(tracked::COPIED_TO, false);
            let reading = self.table_track.enter(tracked::COMPUTE_READ, true);
            recorder::command(move |c| {
                // SAFETY: the recorder's command buffer, recording on the renderer's device;
                // the buffer and the table live until shutdown.
                unsafe {
                    let before =
                        vk::MemoryBarrier { dst_access_mask: vk::AccessFlags::TRANSFER_WRITE, ..Default::default() };
                    c.vk.cmd_pipeline_barrier(
                        c.into,
                        vk::PipelineStageFlags::TRANSFER | vk::PipelineStageFlags::COMPUTE_SHADER,
                        vk::PipelineStageFlags::TRANSFER,
                        vk::DependencyFlags::empty(),
                        &[before],
                        &[],
                        &[],
                    );
                    c.vk.cmd_update_buffer(c.into, staging, 0, &bytes);
                    let written = vk::MemoryBarrier {
                        src_access_mask: vk::AccessFlags::TRANSFER_WRITE,
                        dst_access_mask: vk::AccessFlags::TRANSFER_READ,
                        ..Default::default()
                    };
                    c.vk.cmd_pipeline_barrier(
                        c.into,
                        vk::PipelineStageFlags::TRANSFER,
                        vk::PipelineStageFlags::TRANSFER,
                        vk::DependencyFlags::empty(),
                        &[written],
                        &[],
                        &[],
                    );
                    if let Some(t) = writing {
                        util::renderer_transition(c.vk, c.into, table, vk::ImageAspectFlags::COLOR, &t);
                    }
                    let copy = vk::BufferImageCopy {
                        image_subresource: vk::ImageSubresourceLayers {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            mip_level: 0,
                            base_array_layer: 0,
                            layer_count: 1,
                        },
                        image_extent: vk::Extent3D { width: 256, height: 1, depth: 1 },
                        ..Default::default()
                    };
                    c.vk.cmd_copy_buffer_to_image(
                        c.into,
                        staging,
                        table,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        &[copy],
                    );
                    if let Some(t) = reading {
                        util::renderer_transition(c.vk, c.into, table, vk::ImageAspectFlags::COLOR, &t);
                    }
                }
            });
            self.held = wanted;
            self.have_table = true;
            self.uploads += 1;
        }

        // One descriptor set per presented image, made once and kept: the set names the
        // image, and the images rotate.
        let slot = match self.for_image.iter().position(|&known| known == image) {
            Some(slot) => slot,
            None => {
                if self.for_image.len() >= MOST_IMAGES {
                    self.refused += 1;
                    return;
                }
                // SAFETY: the caller's; the view and the set are this device's.
                let Some(slot) = (unsafe { self.make_set(vk, image) }) else { return };
                slot
            }
        };

        let pipeline = self.pipeline;
        let pipeline_layout = self.layout;
        let set = self.sets[slot];
        let entering = track.enter(tracked::COMPUTED, true);
        recorder::command(move |c| {
            // SAFETY: the recorder's command buffer on the renderer's device; the pipeline,
            // its layout and the set live until shutdown, the image is the caller's.
            unsafe {
                if let Some(t) = entering {
                    util::renderer_transition(c.vk, c.into, image, vk::ImageAspectFlags::COLOR, &t);
                }
                c.vk.cmd_bind_pipeline(c.into, vk::PipelineBindPoint::COMPUTE, pipeline);
                c.vk.cmd_bind_descriptor_sets(c.into, vk::PipelineBindPoint::COMPUTE, pipeline_layout, 0, &[set], &[]);
                c.vk.cmd_dispatch(
                    c.into,
                    width.wrapping_add(DISPLAY_GROUP - 1) / DISPLAY_GROUP,
                    height.wrapping_add(DISPLAY_GROUP - 1) / DISPLAY_GROUP,
                    1,
                );
            }
        });
        self.applied += 1;
    }

    /// A view of `image` and a set naming it and the table, kept in the next slot.
    ///
    /// # Safety
    /// As apply's.
    #[cold]
    unsafe fn make_set(&mut self, vk: &Vk, image: vk::Image) -> Option<usize> {
        // SAFETY: the caller's.
        unsafe {
            let view = vk::ImageViewCreateInfo {
                image,
                view_type: vk::ImageViewType::TYPE_2D,
                format: vk::Format::R8G8B8A8_UNORM,
                subresource_range: vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                },
                ..Default::default()
            };
            let mut made = vk::ImageView::null();
            if vk.create_image_view(&view, &mut made) != vk::Result::SUCCESS {
                return None;
            }

            let allocate = vk::DescriptorSetAllocateInfo {
                descriptor_pool: self.pool,
                descriptor_set_count: 1,
                p_set_layouts: &self.set_layout,
                ..Default::default()
            };
            let mut set = [vk::DescriptorSet::null()];
            if vk.allocate_descriptor_sets(&allocate, &mut set) != vk::Result::SUCCESS {
                vk.destroy_image_view(made);
                return None;
            }

            let images = [
                vk::DescriptorImageInfo {
                    image_view: made,
                    image_layout: vk::ImageLayout::GENERAL,
                    ..Default::default()
                },
                vk::DescriptorImageInfo {
                    image_view: self.table_view,
                    image_layout: vk::ImageLayout::GENERAL,
                    ..Default::default()
                },
            ];
            let writes: [vk::WriteDescriptorSet; 2] = std::array::from_fn(|i| vk::WriteDescriptorSet {
                dst_set: set[0],
                dst_binding: i as u32,
                descriptor_count: 1,
                descriptor_type: vk::DescriptorType::STORAGE_IMAGE,
                p_image_info: &images[i],
                ..Default::default()
            });
            vk.update_descriptor_sets(&writes, &[]);
            let slot = self.for_image.len();
            self.for_image.push(image);
            self.views.push(made);
            self.sets.push(set[0]);
            Some(slot)
        }
    }

    /// Everything made, destroyed; the counts kept for report.
    ///
    /// # Safety
    /// `vk` is the device initialise was given, idle: no frame that names these runs.
    pub unsafe fn shutdown(&mut self, vk: &Vk) {
        if !self.started {
            return;
        }
        // SAFETY: the caller's; each handle is this device's, made here, and unused.
        unsafe {
            for &view in &self.views {
                vk.destroy_image_view(view);
            }
            if self.pool != vk::DescriptorPool::null() {
                vk.destroy_descriptor_pool(self.pool);
            }
            if self.table_view != vk::ImageView::null() {
                vk.destroy_image_view(self.table_view);
            }
            if self.table != vk::Image::null() {
                vk.destroy_image(self.table);
            }
            if self.table_memory != vk::DeviceMemory::null() {
                vk.free_memory(self.table_memory);
            }
            if self.staging != vk::Buffer::null() {
                vk.destroy_buffer(self.staging);
            }
            if self.staging_memory != vk::DeviceMemory::null() {
                vk.free_memory(self.staging_memory);
            }
            if self.pipeline != vk::Pipeline::null() {
                vk.destroy_pipeline(self.pipeline);
            }
            if self.layout != vk::PipelineLayout::null() {
                vk.destroy_pipeline_layout(self.layout);
            }
            if self.set_layout != vk::DescriptorSetLayout::null() {
                vk.destroy_descriptor_set_layout(self.set_layout);
            }
        }
        let (applied, uploads, refused) = (self.applied, self.uploads, self.refused);
        *self = DisplayTable::new();
        self.applied = applied;
        self.uploads = uploads;
        self.refused = refused;
    }

    #[allow(dead_code, reason = "the renderer's report prints this line from RendererReport, after the shutdown")]
    pub fn report(&self) {
        if self.applied == 0 && self.uploads == 0 {
            return;
        }
        log_info!(
            "display: the title's colour table applied to %llu frames (%llu table changes)",
            self.applied,
            self.uploads
        );
        if self.refused != 0 {
            log_info!(
                "display: %llu frames went out without the table: their image was past the 8 that have a set",
                self.refused
            );
        }
    }
}

// What the renderer's report keeps of it after the shutdown.
impl DisplayTable {
    /// The frames the table was applied to and the table's changes.
    pub fn counts(&self) -> (u64, u64, u64) {
        (self.applied, self.uploads, self.refused)
    }
}
