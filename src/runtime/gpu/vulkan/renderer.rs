// The renderer: the title's draws and resolves, recorded into Vulkan: its state, its
// start and its end.
//
// It builds an image per EDRAM surface rather than an emulation of EDRAM
// (docs/rendering.md): a resolve copies a surface out to an image a later fetch binds,
// clears it if the resolve asks, and the title's own clear idiom is answered with clears of
// the images it names. The parts: renderer_frame.rs (command buffers, submission, frame
// slots, the title's occlusion queries), renderer_draw.rs (a draw), renderer_targets.rs,
// renderer_resolve.rs, renderer_arena.rs, renderer_pipelines.rs, renderer_libraries.rs,
// renderer_diagnostics.rs (the switches that look at a frame, and the report) and
// renderer_passtime.rs.
//
// One owner, the ring consumer (§3.6): no global renderer. Initialise makes every object
// in a fixed order, so a command trace names each by the same ordinal; Shutdown destroys
// them in a fixed order and moves what the report prints into
// renderer_diagnostics::REPORT, which the report reads after it.
//
// Unsafe by nature: every handle is the device's that the shared pipeline made
// (shaders/src/pipeline.rs), kept valid by the retire protocol (RetireBeforeDestroy,
// ForgetViews, 600-frame id reuse) and destroyed at shutdown with the device idle and the
// recorder stopped. The device tables (`vk`) are boxed and stay where they are from the
// start to the end: the recorder and the pipeline optimiser hold a pointer to them, and
// the ring consumer holds the renderer boxed (gpu.rs). tools/replay_diff.py holds two
// builds of the whole renderer to each other on captures of the game.
#![deny(unsafe_op_in_unsafe_fn)]

use super::device::Vk;
use super::recorder;
use super::renderer_arena::{Arena, alignment};
use super::renderer_diagnostics::{REPORT, RendererReport};
use super::renderer_draw::{TextureSlots, make_gathered_draws};
use super::renderer_frame::{Core, FrameView, GUEST_QUERY_RING, PRESENT_IMAGES, SEGMENT_BUFFERS};
use super::renderer_pipelines::Pipelines;
use super::renderer_resolve::Resolves;
use super::renderer_state::{ARENA_LEAST, ARENA_WANTED, FRAME_SLOTS};
use super::renderer_targets::{Multisampling, Target, TargetSettings, Targets, make_image, multisampling};
use super::tracked::TrackedImage;
use crate::log::{log_info, log_warn};
use crate::runtime::bridge;
use crate::runtime::gpu::registers::RegisterFile;
use crate::runtime::gpu::shader::bindings::{ARENA_BINDING, ARENA_FLOATS, ARENA_SET};
use crate::runtime::guest_memory::{PHYSICAL_BASE, PHYSICAL_END};
use ash::vk;
use ash::vk::Handle;
use std::ffi::{CStr, CString, c_void};
use std::path::{Path, PathBuf};

#[cfg(feature = "diagnostics")]
use super::renderer_frame::recorder_position;
use std::sync::Mutex;

use super::gpu_time;
#[cfg(feature = "diagnostics")]
use super::trace;
use crate::runtime::gpu::memory_watch::set_shadow;
use crate::runtime::render_size;

/// What the shared pipeline says of the device the renderer draws on (shaders/src/
/// pipeline.rs), the device started first (headless) if it is not running; None when it
/// will not start.
struct DeviceInfo {
    device: vk::Device,
    physical: vk::PhysicalDevice,
    queue: vk::Queue,
    #[cfg(feature = "diagnostics")]
    cache: vk::PipelineCache,
    layout: vk::PipelineLayout,
    set_layouts: [vk::DescriptorSetLayout; 3],
    queue_family: u32,
    multi_draw: u32,
    precise_occlusion: bool,
    name: CString,
}

fn device_info() -> Option<DeviceInfo> {
    use mw2_shaders::pipeline as p;
    // SAFETY: a headless start: the pipeline brings up and owns its own device.
    if !p::ready() && !unsafe { p::initialise(None) } {
        return None;
    }
    Some(DeviceInfo {
        device: p::device(),
        physical: p::physical_device(),
        queue: p::queue(),
        #[cfg(feature = "diagnostics")]
        cache: p::cache(),
        layout: p::layout(),
        set_layouts: [p::set_layout(0), p::set_layout(1), p::set_layout(2)],
        queue_family: p::queue_family(),
        multi_draw: p::multi_draw(),
        precise_occlusion: p::precise_occlusion_queries(),
        name: p::device_name(),
    })
}

// CheckCompiledShaders' note, and what Initialise found (g_compiled).
struct Compiled {
    checked: bool,
    current: bool,
    game_folder: PathBuf,
    why: Vec<u8>,
    device: Vec<u8>,
}

static COMPILED: Mutex<Compiled> = Mutex::new(Compiled {
    checked: false,
    current: true,
    game_folder: PathBuf::new(),
    why: Vec::new(),
    device: Vec::new(),
});

fn compiled() -> std::sync::MutexGuard<'static, Compiled> {
    COMPILED.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A player's start draws only with every shader of the game compiled before it, by the
/// launcher (shaders/src/marker.rs): asked before initialise, which checks the launcher's
/// marker against the device and scale it settles on.
pub fn check_compiled_shaders(game_folder: &Path) {
    let mut c = compiled();
    c.checked = true;
    c.game_folder = game_folder.to_path_buf();
}

/// Whether they match, and if not why, and which device the run draws on.
pub fn compiled_shaders() -> (bool, CString, CString) {
    let c = compiled();
    (c.current, crate::log::c_string(c.why.clone()), crate::log::c_string(c.device.clone()))
}

/// The title made a shader object over this microcode while loading a level: it is
/// translated and compiled on a worker thread now. Any thread; returns at once.
pub fn prepare_shader(pixel: bool, code: &[u32]) {
    super::renderer_libraries::prepare_shader(pixel, code);
}

/// The renderer: all its state, in the parts that are borrowed apart (§3.6).
pub struct Renderer {
    pub vk: Option<Box<Vk>>,
    pub core: Core,
    pub targets: Targets,
    pub resolves: Resolves,
    pub pipelines: Pipelines,
    pub slots: TextureSlots,
}

fn no_settings() -> TargetSettings {
    TargetSettings {
        multisampling: Multisampling::default(),
        forced_samples: 0,
        depth_format: vk::Format::UNDEFINED,
        float_depth_format: vk::Format::UNDEFINED,
        scale: 1,
        shadow_scale: 1,
        memory: vk::PhysicalDeviceMemoryProperties::default(),
        max_target_width: u32::MAX,
        physical: vk::PhysicalDevice::null(),
    }
}

/// MW2_TIME_RENDER: where a frame's wall clock goes (Timing()).
fn timing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| cfg!(feature = "diagnostics") && crate::env::flag(c"MW2_TIME_RENDER"))
}

impl Default for Renderer {
    fn default() -> Self {
        Renderer::new()
    }
}

impl Renderer {
    pub fn new() -> Renderer {
        Renderer {
            vk: None,
            core: Core::new(),
            targets: Targets::new(no_settings()),
            resolves: Resolves::new(vk::PhysicalDevice::null()),
            pipelines: Pipelines::new(false, vk::Format::UNDEFINED),
            slots: TextureSlots::default(),
        }
    }

    /// vk::renderer::Initialise: the device the shared pipeline brought up, and everything the
    /// renderer draws with made on it, in a fixed order. False when it cannot draw; what
    /// was made by then is kept for shutdown to destroy.
    ///
    /// # Safety
    /// The calling thread is the one that will record (the recorder's producer), and the
    /// renderer is not moved between this and shutdown (the recorder and the optimiser hold
    /// pointers into it: `vk`, boxed).
    pub unsafe fn initialise(&mut self) -> bool {
        if self.core.device != vk::Device::null() {
            return true;
        }
        // SAFETY: the caller's.
        let started = unsafe { self.start() };
        // A renderer that did not start draws nothing: every entry asks for the device
        // first, so a refusal such as "no depth format" leaves no first frame indexing
        // slots never made. Shutdown still destroys what was made by then.
        if !started && self.core.device != vk::Device::null() {
            log_warn!("renderer: did not start; nothing is drawn");
            self.core.refused = self.core.device;
            self.core.device = vk::Device::null();
        }
        started
    }

    /// initialise's work.
    ///
    /// # Safety
    /// initialise's.
    unsafe fn start(&mut self) -> bool {
        let Some(info) = device_info() else {
            return false;
        };
        let (device, physical, queue) = (info.device, info.physical, info.queue);
        if device == vk::Device::null() || queue == vk::Queue::null() {
            return false;
        }
        let core = &mut self.core;
        core.device = device;
        core.physical = physical;
        core.queue = queue;
        core.queue_family = info.queue_family;
        core.precise_queries = info.precise_occlusion;
        core.pipeline_layout = info.layout;
        core.timing = timing();
        // Before anything traced is made, the texture cache's start included.
        #[cfg(feature = "diagnostics")]
        {
            trace::start(
                device,
                physical,
                queue,
                info.cache,
                core.pipeline_layout,
                &info.set_layouts,
                recorder_position,
            );
        }
        // SAFETY: the shared pipeline's instance and its device, live until the run's end.
        let instance_handle = mw2_shaders::pipeline::instance();
        let tables = unsafe {
            let instance = super::instance_table(instance_handle.as_raw() as *mut c_void);
            Vk::new(&instance, device, info.multi_draw)
        };
        let vk: &Vk = self.vk.insert(Box::new(tables));
        let core = &mut self.core;
        if !core.textures.initialise(vk) {
            core.device = vk::Device::null();
            return false;
        }

        // SAFETY (the queries): the renderer's physical device.
        let properties = unsafe {
            core.memory = vk.get_physical_device_memory_properties(physical);
            vk.get_physical_device_properties(physical)
        };
        core.uniform_alignment = alignment(
            properties.limits.min_uniform_buffer_offset_alignment,
            properties.limits.min_storage_buffer_offset_alignment,
        );

        // AMD has no D24_UNORM_S8_UINT, so the D24S8 surfaces' format is whichever combined
        // format the device actually has; the D24FS8 ones are float.
        // SAFETY: the shared pipeline's live instance and the renderer's physical device.
        let instance = unsafe { mw2_shaders::pipeline::instance_table(instance_handle) };
        let (depth_format, float_depth_format) = unsafe {
            (
                mw2_shaders::shader_build::depth_format(&instance, physical),
                mw2_shaders::shader_build::float_depth_format(&instance, physical),
            )
        };
        if depth_format == vk::Format::UNDEFINED {
            log_warn!("renderer: no depth format");
            return false;
        }

        // The console's 2x and 4x surfaces are drawn with that many samples unless
        // MW2_NO_MSAA=1 (renderer_targets.rs's multisampling says how it is decided).
        // SAFETY: the renderer's device, of `physical`.
        let samples = unsafe { multisampling(vk, physical, &properties, depth_format, float_depth_format) };

        // MW2_SCALE=2 or 3 draws everything that many times wider and taller; the scale is
        // lowered until a surface of 4096 of the title's pixels fits the device's images.
        // SAFETY: the renderer's physical device.
        let properties = unsafe { vk.get_physical_device_properties(physical) };
        let asked = crate::env::number(c"MW2_SCALE", 1).clamp(1, 3) as u32;
        // SAFETY: as above.
        let scale = unsafe { mw2_shaders::shader_build::scale(&instance, physical, asked) };
        let frame = render_size::frame();
        core.scale = scale;
        core.present_width = frame.width.wrapping_mul(scale);
        core.present_height = frame.height.wrapping_mul(scale);
        if scale != asked {
            log_warn!(
                "renderer: MW2_SCALE=%u is more than the device's %u-pixel images hold",
                asked,
                properties.limits.max_image_dimension2_d
            );
        }
        if scale > 1 {
            log_info!(
                "renderer: drawing at %ux the title's sizes, a %ux%u frame",
                scale,
                core.present_width,
                core.present_height
            );
        }
        // MW2_SHADOW_SCALE=2 to 4 draws the shadow maps that many times wider and taller,
        // and never smaller than the rest.
        let shadow_asked = crate::env::number(c"MW2_SHADOW_SCALE", 1)
            .clamp(1, u64::from(mw2_shaders::shader_build::MAX_SHADOW_SCALE)) as u32;
        // SAFETY: as above.
        let shadow_scale = unsafe { mw2_shaders::shader_build::shadow_scale(&instance, physical, shadow_asked, scale) };
        core.shadow_scale = shadow_scale;
        if shadow_scale < shadow_asked {
            log_warn!(
                "renderer: MW2_SHADOW_SCALE=%u is more than the device's %u-pixel images hold",
                shadow_asked,
                properties.limits.max_image_dimension2_d
            );
        }
        if shadow_scale != scale {
            log_info!("renderer: drawing the shadow maps at %ux the title's sizes", shadow_scale);
        }
        self.targets.settings = TargetSettings {
            multisampling: samples,
            forced_samples: crate::env::number(c"MW2_MSAA", 0) as u32,
            depth_format,
            float_depth_format,
            scale,
            shadow_scale,
            memory: core.memory,
            max_target_width: properties.limits.max_framebuffer_width.min(properties.limits.max_image_dimension2_d),
            physical,
        };
        self.resolves = Resolves::new(physical);
        self.pipelines.scaled = shadow_scale > 1;
        self.pipelines.depth_format = depth_format;

        // The launcher compiled every shader for a device, a scale and the game's files;
        // this run draws on the device and at the scale just settled.
        check_compiled(&instance, physical, shadow_scale > 1);

        // SAFETY (each call below): the renderer's device and what it made here.
        unsafe {
            // A pool each, because a slot is reset on its own while the others are busy.
            let pool = vk::CommandPoolCreateInfo { queue_family_index: info.queue_family, ..Default::default() };
            for slot in &mut core.slots {
                if vk.create_command_pool(&pool, &mut slot.commands) != vk::Result::SUCCESS {
                    return false;
                }
                let allocate = vk::CommandBufferAllocateInfo {
                    command_pool: slot.commands,
                    level: vk::CommandBufferLevel::PRIMARY,
                    command_buffer_count: SEGMENT_BUFFERS,
                    ..Default::default()
                };
                slot.buffers.resize(SEGMENT_BUFFERS as usize, vk::CommandBuffer::null());
                if vk.allocate_command_buffers(&allocate, &mut slot.buffers) != vk::Result::SUCCESS {
                    return false;
                }
            }

            let setup_pool = vk::CommandPoolCreateInfo {
                flags: vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER,
                queue_family_index: info.queue_family,
                ..Default::default()
            };
            if vk.create_command_pool(&setup_pool, &mut core.setup_commands) != vk::Result::SUCCESS {
                return false;
            }
            let allocate = vk::CommandBufferAllocateInfo {
                command_pool: core.setup_commands,
                level: vk::CommandBufferLevel::PRIMARY,
                command_buffer_count: 1,
                ..Default::default()
            };
            if vk.allocate_command_buffers(&allocate, std::slice::from_mut(&mut core.setup)) != vk::Result::SUCCESS {
                return false;
            }

            let queries = vk::QueryPoolCreateInfo {
                query_type: vk::QueryType::OCCLUSION,
                query_count: GUEST_QUERY_RING,
                ..Default::default()
            };
            if vk.create_query_pool(&queries, &mut core.occlusion.pool) != vk::Result::SUCCESS {
                core.occlusion.pool = vk::QueryPool::null();
            }
            // MW2_DETERMINISTIC: nothing the GPU's clock says.
            #[cfg(feature = "diagnostics")]
            let clocked = !trace::deterministic();
            #[cfg(not(feature = "diagnostics"))]
            let clocked = true;
            if clocked {
                gpu_time::start(physical, device, queue, info.queue_family);
                core.pass.start(vk, physical, info.queue_family);
            }

            let fence = vk::FenceCreateInfo::default();
            for slot in &mut core.slots {
                if vk.create_fence(&fence, &mut slot.fence) != vk::Result::SUCCESS {
                    return false;
                }
            }
            if vk.create_fence(&fence, &mut core.setup_fence) != vk::Result::SUCCESS {
                return false;
            }

            // Constants, vertex data and indices all live in the arena, and behind it, when
            // there is room, the shadow of guest physical memory; asked for whole and
            // settled for what the device gives: on a shared-memory part it is system RAM,
            // and refusing to start because the largest did not fit would be the wrong answer.
            if !make_arena(vk, core) {
                return false;
            }

            // A slot's share has to hold everything one submission copies.
            core.arena = Arena::new(core.arena_mapped, core.arena_bytes, core.uniform_alignment, core.shadow_base);
            core.command = core.slots[core.slot as usize].buffers[0];
            log_info!("renderer: %u frame slots of %u KB of arena each", FRAME_SLOTS, core.arena.slot_bytes() / 1024);

            if !vk.dynamic_commands() {
                log_warn!("renderer: the device did not give dynamic rendering or dynamic state commands");
                return false;
            }

            let size = vk::DescriptorPoolSize { ty: vk::DescriptorType::STORAGE_BUFFER, descriptor_count: 2 };
            let descriptors = vk::DescriptorPoolCreateInfo {
                max_sets: 1,
                pool_size_count: 1,
                p_pool_sizes: &size,
                ..Default::default()
            };
            if vk.create_descriptor_pool(&descriptors, &mut core.descriptors) != vk::Result::SUCCESS {
                return false;
            }
            let memory_layout = info.set_layouts[ARENA_SET as usize];
            let allocate_set = vk::DescriptorSetAllocateInfo {
                descriptor_pool: core.descriptors,
                descriptor_set_count: 1,
                p_set_layouts: &memory_layout,
                ..Default::default()
            };
            if vk.allocate_descriptor_sets(&allocate_set, std::slice::from_mut(&mut core.memory_set))
                != vk::Result::SUCCESS
            {
                return false;
            }
            // The whole arena, as dwords and as vec4.
            let buffer = vk::DescriptorBufferInfo { buffer: core.arena_buffer, offset: 0, range: vk::WHOLE_SIZE };
            let writes: [vk::WriteDescriptorSet; 2] = std::array::from_fn(|i| vk::WriteDescriptorSet {
                dst_set: core.memory_set,
                dst_binding: if i != 0 { ARENA_FLOATS } else { ARENA_BINDING },
                descriptor_count: 1,
                descriptor_type: vk::DescriptorType::STORAGE_BUFFER,
                p_buffer_info: &buffer,
                ..Default::default()
            });
            vk.update_descriptor_sets(&writes, &[]);

            // Swap blits into the present images, the window blits out of them, and the
            // display's colour table is applied to them in place: Vulkan requires all of it
            // of R8G8B8A8_UNORM, which only MW2_FAKE_FORMAT_UNSUPPORTED takes away
            // (format_support.rs).
            let present_needs = vk::FormatFeatureFlags::BLIT_SRC
                | vk::FormatFeatureFlags::BLIT_DST
                | vk::FormatFeatureFlags::STORAGE_IMAGE;
            if !vk.format_features(core.physical, vk::Format::R8G8B8A8_UNORM).contains(present_needs) {
                log_warn!("renderer: the device cannot blit into, out of or store to an R8G8B8A8 image");
                return false;
            }
            for i in 0..PRESENT_IMAGES as usize {
                let mut image = Target {
                    format: vk::Format::R8G8B8A8_UNORM,
                    width: core.present_width / scale, // make_image scales
                    height: core.present_height / scale,
                    scale,
                    ..Target::default()
                };
                // The images handed to the window, by role.
                #[cfg(feature = "diagnostics")]
                trace::name_next(trace::Kind::Image, ["Present0", "Present1", "Present2", "Present3"][i]);
                // STORAGE so the display's colour table can be applied to it in place.
                if !make_image(
                    vk,
                    &core.memory,
                    &mut image,
                    vk::ImageUsageFlags::TRANSFER_DST
                        | vk::ImageUsageFlags::TRANSFER_SRC
                        | vk::ImageUsageFlags::STORAGE,
                    vk::ImageAspectFlags::empty(),
                ) {
                    return false;
                }
                core.present[i] = image.image;
                core.present_track[i] = TrackedImage::NEW;
                core.present_memory[i] = image.memory;
            }
        }

        let depth_name = |format: vk::Format| if format == vk::Format::D24_UNORM_S8_UINT { c"D24S8" } else { c"D32S8" };
        log_info!(
            "renderer: ready on %s, depth %s, float depth %s, %u MB frame arena%s",
            info.name.as_c_str(),
            depth_name(depth_format),
            depth_name(float_depth_format),
            core.arena_bytes >> 20,
            if core.shadow_base != 0 {
                c" and a shadow of guest memory behind it"
            } else {
                c", no shadow of guest memory"
            }
        );
        // MW2_SUBGROUP_CHECK: before anything else records on the device.
        #[cfg(feature = "diagnostics")]
        // SAFETY: the renderer's device and queue; the recorder has not started.
        unsafe {
            super::subgroup_check::run(vk, physical, queue, info.queue_family, &self.core.memory)
        };
        let vk_pointer: *const Vk = vk;
        // SAFETY: the tables are boxed and stay until shutdown, which stops the workers and
        // the recorder first (the caller's).
        unsafe {
            self.pipelines.start_preparing(vk_pointer);
            self.pipelines.prewarm_from_cache(&*vk_pointer, &mut self.core.retiring);
            // A lost device ends the run, the way closing the window does.
            mw2_shaders::pipeline::on_device_lost(bridge::request_exit);
            recorder::start(vk_pointer, Some(make_gathered_draws));
        }
        super::copy_worker::start();
        true
    }

    /// vk::renderer::Shutdown: everything destroyed in a fixed order, with the last frames
    /// done and the recorder stopped first; what the report prints moved into REPORT.
    ///
    /// # Safety
    /// The calling thread is the recorder's producer (the consumer, or the exit thread once
    /// the consumer is joined).
    pub unsafe fn shutdown(&mut self) {
        if self.core.device == vk::Device::null() {
            if self.core.refused == vk::Device::null() {
                return;
            }
            self.core.device = std::mem::replace(&mut self.core.refused, vk::Device::null());
        }
        let Renderer { vk, core, targets, resolves, pipelines, slots } = self;
        let Some(vk) = vk.as_deref() else { return };
        #[cfg(feature = "diagnostics")]
        trace::shutdown_begins();
        // SAFETY (all of it): the renderer's device, idle once the recorder has stopped and
        // the device waited for; every handle was made by it here.
        unsafe {
            // Before anything is destroyed, the display table's pipeline included: the last
            // frames are still on the GPU, and the last commands may still be with the
            // recorder thread.
            // The copies into the arena made and the copy thread gone, before the arena is.
            super::copy_worker::stop();
            recorder::stop();
            let _ = vk.device_wait_idle();
            gpu_time::stop();
            core.pass.stop(vk, core.frames);
            let joined = pipelines.stop_preparing(vk);
            resolves.display.shutdown(vk);
            pipelines.write_shader_cache();
            core.diag.summarise_targets(targets, core.frames);
            let (shader_count, pipeline_count) = pipelines.counts();
            // The fast links still retiring go with the rest, the device idle.
            pipelines.replaced.extend(core.retiring.take_all());
            pipelines.destroy(vk, joined);
            targets.destroy_all(vk);
            for i in 0..PRESENT_IMAGES as usize {
                if core.present[i] != vk::Image::null() {
                    vk.destroy_image(core.present[i]);
                }
                if core.present_memory[i] != vk::DeviceMemory::null() {
                    vk.free_memory(core.present_memory[i]);
                }
                core.present[i] = vk::Image::null();
                core.present_memory[i] = vk::DeviceMemory::null();
            }
            resolves.destroy_all(vk);
            let d = &mut core.diag;
            if d.read_back != vk::Buffer::null() {
                vk.destroy_buffer(d.read_back);
            }
            if d.read_back_memory != vk::DeviceMemory::null() {
                vk.free_memory(d.read_back_memory);
            }
            d.read_back = vk::Buffer::null();
            d.read_back_memory = vk::DeviceMemory::null();
            d.read_back_mapped = std::ptr::null();
            if core.descriptors != vk::DescriptorPool::null() {
                vk.destroy_descriptor_pool(core.descriptors);
            }
            set_shadow(std::ptr::null_mut(), 0);
            core.shadow_base = 0;
            if !core.arena_mapped.is_null() {
                vk.unmap_memory(core.arena_memory);
            }
            if core.arena_buffer != vk::Buffer::null() {
                vk.destroy_buffer(core.arena_buffer);
            }
            if core.arena_memory != vk::DeviceMemory::null() {
                vk.free_memory(core.arena_memory);
            }
            if core.occlusion.pool != vk::QueryPool::null() {
                vk.destroy_query_pool(core.occlusion.pool);
            }
            for slot in &core.slots {
                if slot.fence != vk::Fence::null() {
                    vk.destroy_fence(slot.fence);
                }
                if slot.commands != vk::CommandPool::null() {
                    vk.destroy_command_pool(slot.commands);
                }
            }
            if core.setup_fence != vk::Fence::null() {
                vk.destroy_fence(core.setup_fence);
            }
            if core.setup_commands != vk::CommandPool::null() {
                vk.destroy_command_pool(core.setup_commands);
            }

            // The handles are gone; the counters stay, for the report written after this.
            core.arena_buffer = vk::Buffer::null();
            core.arena_memory = vk::DeviceMemory::null();
            core.arena_mapped = std::ptr::null_mut();
            core.occlusion.pool = vk::QueryPool::null();
            core.descriptors = vk::DescriptorPool::null();
            core.setup_fence = vk::Fence::null();
            core.setup_commands = vk::CommandPool::null();
            for slot in &mut core.slots {
                *slot = Default::default();
            }
            core.recording = false;
            core.device = vk::Device::null();

            let mut skipped: Vec<(u64, &'static CStr)> = core.skipped.values().map(|&(why, n)| (n, why)).collect();
            skipped.sort_by_key(|&(n, why)| std::cmp::Reverse((n, why.as_ptr() as usize)));
            let (applied, uploads, refused) = resolves.display.counts();
            let world_seconds = match (core.first_world_at, core.last_world_at) {
                (Some(first), Some(last)) => last.wrapping_sub(first) as f64 / 1e9,
                _ => 0.0,
            };
            *REPORT.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(RendererReport {
                frames: core.frames,
                draws: core.draws,
                draws_recorded: core.draws_recorded,
                resolves: resolves.resolves,
                depth_resolves: resolves.depth_resolves,
                depth_resolve_wraps: resolves.depth_resolve_wraps,
                depth_resolve_wraps_below: resolves.depth_resolve_wraps_below,
                resolve_clears: targets.clears,
                pipeline_count: pipeline_count as u64,
                shader_count: shader_count as u64,
                slots_kept: slots.kept,
                slots_looked_up: slots.looked_up,
                times: core.times,
                resolve_nanoseconds: resolves.resolve_nanoseconds,
                world_frames: core.world_frames,
                world_seconds,
                submits: core.submits,
                segments: core.segments,
                stream_write_segments: core.stream_write_segments,
                completions: core.completions,
                prewarm_wanted: pipelines.prewarm_wanted,
                prewarmed: pipelines.prewarmed,
                prewarm_microseconds: pipelines.prewarm_microseconds,
                translate_microseconds: pipelines.translate_microseconds,
                compile_microseconds: pipelines.compile_microseconds,
                shaders_built_at_draw: pipelines.shaders_built_at_draw,
                pipelines_linked: pipelines.pipelines_linked,
                pipelines_optimised: pipelines.pipelines_optimised,
                target_summary: std::mem::take(&mut core.diag.target_summary),
                resolve_images_made: resolves.resolve_images_made,
                arena_peak: core.arena_peak,
                arena_bytes: core.arena_bytes,
                arena_written: core.arena.written,
                arena_reused: core.arena.reused,
                verts_matched: core.verts_matched,
                verts_traced: core.verts_traced,
                arena_overflows: core.arena.overflows,
                skipped,
                display_applied: applied,
                display_uploads: uploads,
                display_refused: refused,
            });
            core.arena.unmapped();
        }
    }

    /// The texture cache's end (vk::textures::Shutdown), which the GPU's shutdown makes
    /// after the renderer's; the device tables go with it.
    pub fn shutdown_textures(&mut self) {
        if let Some(vk) = self.vk.as_deref() {
            self.core.textures.shutdown(vk);
        }
        self.vk = None;
        super::texture_cache::keep_for_report(std::mem::take(&mut self.core.textures));
    }

    /// A draw issued in copy mode: the colour or depth target copied where the copy
    /// registers say it goes (vk::renderer::Resolve).
    ///
    /// # Safety
    /// As draw's.
    pub unsafe fn resolve(&mut self, r: &RegisterFile) {
        let Renderer { vk, core, targets, resolves, .. } = self;
        let Some(vk) = vk.as_deref() else { return };
        if core.device == vk::Device::null() {
            return;
        }
        // SAFETY: the renderer's live device.
        unsafe { resolves.resolve(vk, targets, &mut FrameView { vk, core }, r) };
    }

    /// The swap packet: the frame is what was resolved to this front buffer.
    pub fn swap(&mut self, front_buffer: u32) {
        let Renderer { vk, core, targets, resolves, .. } = self;
        let Some(vk) = vk.as_deref() else { return };
        if core.device == vk::Device::null() {
            return;
        }
        // SAFETY: the renderer's live device.
        unsafe { resolves.swap(vk, targets, &mut FrameView { vk, core }, front_buffer) };
    }

    /// The title's own occlusion query, bracketed by two EVENT_WRITE_ZPD events.
    pub fn begin_occlusion_query(&mut self) {
        let Some(vk) = self.vk.as_deref() else { return };
        if self.core.device == vk::Device::null() {
            return;
        }
        self.core.begin_occlusion_query(vk);
    }

    /// True, with `samples` 0, when there is nothing to count; otherwise the count comes
    /// through collect_occlusion_queries once the GPU has finished it.
    pub fn end_occlusion_query(&mut self, tag: u32, samples: &mut u64) -> bool {
        if self.core.device == vk::Device::null() {
            *samples = 0;
            return true;
        }
        self.core.end_occlusion_query(tag, samples)
    }

    pub fn collect_occlusion_queries(&mut self, deliver: impl FnMut(u32, u64)) {
        let Some(vk) = self.vk.as_deref() else { return };
        if self.core.device == vk::Device::null() {
            return;
        }
        self.core.collect_occlusion_queries(vk, deliver);
    }

    pub fn before_stream_write(&mut self, physical: u32, size: u32) {
        // The consumer is about to write guest memory: copies of it come first.
        super::copy_worker::settle();
        if let Some(vk) = self.vk.as_deref() {
            self.core.before_stream_write(vk, physical, size);
        }
    }

    pub fn before_completion(&mut self) {
        match self.vk.as_deref() {
            Some(vk) => self.core.before_completion(vk),
            None => self.core.completions += 1,
        }
    }

    /// The ring consumer has nothing to read: what is queued for the recorder is made now.
    pub fn idle(&self) {
        super::renderer_frame::flush_recorder();
    }
}

/// The arena and the shadow behind it: the largest the device will give, halving from
/// MW2_ARENA_MB (512) to 32 MB, the shadow dropped first.
///
/// # Safety
/// `vk` is the renderer's device.
unsafe fn make_arena(vk: &Vk, core: &mut Core) -> bool {
    static NO_SHADOW: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let no_shadow = *NO_SHADOW.get_or_init(|| cfg!(feature = "diagnostics") && crate::env::flag(c"MW2_NO_SHADOW"));
    let shadow_bytes = PHYSICAL_END - PHYSICAL_BASE;
    let mut wanted = (crate::env::number(c"MW2_ARENA_MB", u64::from(ARENA_WANTED >> 20)) as u32).wrapping_shl(20);
    let mut with_shadow = !no_shadow;
    while wanted >= ARENA_LEAST {
        let size = u64::from(wanted) + if with_shadow { u64::from(shadow_bytes) } else { 0 };
        let buffer = vk::BufferCreateInfo {
            size,
            usage: vk::BufferUsageFlags::UNIFORM_BUFFER
                | vk::BufferUsageFlags::STORAGE_BUFFER
                | vk::BufferUsageFlags::INDEX_BUFFER,
            ..Default::default()
        };
        // SAFETY (each call): the caller's device, and what this made.
        unsafe {
            if vk.create_buffer(&buffer, &mut core.arena_buffer) == vk::Result::SUCCESS {
                let needs = vk.get_buffer_memory_requirements(core.arena_buffer);
                let kind = super::util::find_memory(
                    &core.memory,
                    needs.memory_type_bits,
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                );
                if kind == u32::MAX {
                    log_warn!("renderer: no host-visible memory");
                    return false;
                }
                let allocate = vk::MemoryAllocateInfo {
                    allocation_size: needs.size,
                    memory_type_index: kind,
                    ..Default::default()
                };
                let mut mapped: *mut c_void = std::ptr::null_mut();
                if vk.allocate_memory(&allocate, &mut core.arena_memory) == vk::Result::SUCCESS
                    && vk.bind_buffer_memory(core.arena_buffer, core.arena_memory, 0) == vk::Result::SUCCESS
                    && vk.map_memory(core.arena_memory, 0, size, &mut mapped) == vk::Result::SUCCESS
                {
                    core.arena_mapped = mapped.cast();
                    core.arena_bytes = wanted;
                    if with_shadow {
                        core.shadow_base = wanted;
                        set_shadow(core.arena_mapped.add(wanted as usize), shadow_bytes);
                    }
                    break;
                }
                if core.arena_memory != vk::DeviceMemory::null() {
                    vk.free_memory(core.arena_memory);
                }
                vk.destroy_buffer(core.arena_buffer);
                core.arena_memory = vk::DeviceMemory::null();
                core.arena_buffer = vk::Buffer::null();
            }
        }
        if with_shadow {
            with_shadow = false;
        } else {
            wanted /= 2;
        }
    }
    if core.arena_bytes == 0 {
        log_warn!("renderer: no frame arena");
        return false;
    }
    true
}

/// The renderer's start's check of the launcher's marker (shaders/src/marker.rs): the
/// device's id, this run's fingerprint and the verdict on it, the marker's shader count,
/// and whether the pipeline cache loaded (read only when the rest says yes).
fn check_compiled(instance: &ash::Instance, physical: vk::PhysicalDevice, scaled: bool) {
    use mw2_shaders::{marker, pipeline};
    let mut c = compiled();
    if !c.checked {
        return;
    }
    let custom_maps = crate::env::flag(c"MW2_CUSTOM_MAPS");
    // SAFETY: the renderer's physical device, of the shared pipeline's instance.
    let device = unsafe { pipeline::device_id(instance, physical) }.into_bytes();
    let fingerprint = unsafe { marker::fingerprint(instance, physical, scaled, &c.game_folder, custom_maps) };
    let mut reason: &'static str = "";
    let mut current = marker::compiled(&fingerprint, scaled, Some(&mut reason));
    let mut why = reason.as_bytes().to_vec();
    let shaders = marker::read_marker(scaled).shaders;
    // The scale's own cache, now that the scale is known; nothing is built with the
    // shared pipeline's before the title runs. A cache the marker does not describe is
    // neither read nor written back.
    if current {
        pipeline::set_cache_path(Some(mw2_shaders::shader_build::cache_file(scaled).as_bytes()));
        pipeline::reopen_cache();
    }
    let cache_loaded = pipeline::cache_loaded();
    c.device = device;
    if current && shaders != 0 && !cache_loaded {
        current = false;
        why = b"The graphics driver did not take the compiled shaders.".to_vec();
    }
    c.current = current;
    c.why = why;
    if current {
        log_info!("renderer: every shader of the game is compiled for this device (%u)", shaders);
        let state = if shaders != 0 {
            format!("yes, {shaders} for this device, driver and scale")
        } else {
            "none: the device has no pipeline libraries, so each is compiled at its first draw".to_owned()
        };
        crate::runtime::report::shaders_compiled(state.as_bytes());
    } else {
        let why = crate::log::c_string(c.why.clone());
        log_warn!("renderer: the game's shaders are not compiled for this run: %s", why.as_c_str());
        let mut state = b"no: ".to_vec();
        state.extend_from_slice(&c.why);
        crate::runtime::report::shaders_compiled(&state);
        // What this run would write back is not what the marker describes.
        pipeline::set_cache_path(None);
    }
}
