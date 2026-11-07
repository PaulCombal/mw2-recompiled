// The renderer's Vulkan device: ash's tables, and a thin method for each Vulkan call the
// C++ renderer makes (runtime/gpu/vulkan/renderer*.cpp, texture_cache.cpp,
// image_memory.cpp, display_table.cpp), with its extension commands (renderer_state.h's
// Dispatch). With diagnostics and the trace on (trace.rs) the calls trace.h wraps write
// their line, the same line trace.cpp's wrapper writes: after the call for one that makes
// something, before it for the rest. The calls trace.h does not wrap (queries, maps,
// waits) are never traced, as in the C++.
//
// Unsafe by nature: every method is a Vulkan call. Its contract, which each method's
// `# Safety` refers to, is Vulkan's: the handles are live and this device's (or, for a
// physical-device query, its instance's), each structure is valid with its pNext chain,
// each array holds the count beside it, and what Vulkan says is externally synchronised
// (a command buffer while it records, the queue) is held by the caller.
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(feature = "diagnostics")]
use super::trace;
use ash::vk;
use std::ffi::{CStr, c_void};

pub struct Vk {
    pub device: ash::Device,
    instance: ash::Instance,
    dynamic_rendering: ash::khr::dynamic_rendering::Device,
    extended_dynamic_state: ash::ext::extended_dynamic_state::Device,
    multi_draw: Option<ash::ext::multi_draw::Device>,
    // The most one multi-draw may make; 0 without VK_EXT_multi_draw (Dispatch::multiDraw).
    multi_draw_count: u32,
    // Begin and end rendering, the cull mode and the stencil op are there: without them
    // the renderer does not start (renderer.cpp's Initialise).
    dynamic_commands: bool,
    // The trace was on when the tables were made (trace::start comes first).
    #[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
    trace: bool,
}

/// Whether the device gives the command `name`.
fn given(instance: &ash::Instance, device: vk::Device, name: &CStr) -> bool {
    // SAFETY: a live device of the instance (Vk::new's contract), and a name.
    unsafe { instance.get_device_proc_addr(device, name.as_ptr()) }.is_some()
}

impl Vk {
    /// The tables of `device`, made by `instance`'s physical device, with its extension
    /// commands; `multi_draw` is the most one multi-draw may make, 0 without
    /// VK_EXT_multi_draw (vk::pipeline::MultiDraw). Made after trace::start.
    ///
    /// # Safety
    /// `device` is live and outlives the tables.
    pub unsafe fn new(instance: &ash::Instance, device: vk::Device, multi_draw: u32) -> Vk {
        // SAFETY: the caller's.
        let table = unsafe { ash::Device::load(instance.fp_v1_0(), device) };
        let dynamic_rendering = ash::khr::dynamic_rendering::Device::new(instance, &table);
        let extended_dynamic_state = ash::ext::extended_dynamic_state::Device::new(instance, &table);
        let dynamic_commands = given(instance, device, c"vkCmdBeginRenderingKHR")
            && given(instance, device, c"vkCmdEndRenderingKHR")
            && given(instance, device, c"vkCmdSetCullModeEXT")
            && given(instance, device, c"vkCmdSetStencilOpEXT");
        let mut multi_draw_count = multi_draw;
        let mut multi = None;
        if multi_draw_count != 0 {
            if given(instance, device, c"vkCmdDrawMultiEXT") && given(instance, device, c"vkCmdDrawMultiIndexedEXT") {
                multi = Some(ash::ext::multi_draw::Device::new(instance, &table));
            } else {
                multi_draw_count = 0;
            }
        }
        #[cfg(feature = "diagnostics")]
        if trace::forcing().no_multi_draw {
            multi_draw_count = 0;
        }
        Vk {
            device: table,
            instance: instance.clone(),
            dynamic_rendering,
            extended_dynamic_state,
            multi_draw: multi,
            multi_draw_count,
            dynamic_commands,
            #[cfg(feature = "diagnostics")]
            trace: trace::on(),
            #[cfg(not(feature = "diagnostics"))]
            trace: false,
        }
    }

    pub fn handle(&self) -> vk::Device {
        self.device.handle()
    }

    pub fn multi_draw(&self) -> u32 {
        self.multi_draw_count
    }

    pub fn dynamic_commands(&self) -> bool {
        self.dynamic_commands
    }

    #[inline(always)]
    fn traced(&self) -> bool {
        cfg!(feature = "diagnostics") && self.trace
    }

    #[inline(always)]
    fn d(&self) -> &ash::DeviceFnV1_0 {
        self.device.fp_v1_0()
    }
}

// Each traced call: the line before or after, as trace.cpp's wrapper has it, then the call.
macro_rules! traced {
    ($self:ident, before: $line:expr, $call:expr) => {{
        #[cfg(feature = "diagnostics")]
        if $self.traced() {
            // SAFETY: the method's contract covers what the line reads.
            #[allow(unused_unsafe)]
            unsafe {
                $line
            };
        }
        // SAFETY: the method's contract.
        #[allow(unused_unsafe)]
        unsafe {
            $call
        }
    }};
    ($self:ident, after: $r:ident = $call:expr, $line:expr) => {{
        // SAFETY: the method's contract.
        #[allow(unused_unsafe)]
        let $r = unsafe { $call };
        #[cfg(feature = "diagnostics")]
        if $self.traced() {
            // SAFETY: the method's contract covers what the line reads.
            #[allow(unused_unsafe)]
            unsafe {
                $line
            };
        }
        $r
    }};
}

fn null<T>() -> *const T {
    std::ptr::null()
}

fn len(count: usize) -> u32 {
    count as u32
}

impl Vk {
    // ---- objects ------------------------------------------------------------------------

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn create_image(&self, info: &vk::ImageCreateInfo, out: &mut vk::Image) -> vk::Result {
        traced!(self, after: r = (self.d().create_image)(self.handle(), info, null(), out), lines::create_image(info, *out, r))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn destroy_image(&self, image: vk::Image) {
        traced!(self, before: lines::gone("DestroyImage", "image", trace::Kind::Image, image), (self.d().destroy_image)(self.handle(), image, null()))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn create_image_view(&self, info: &vk::ImageViewCreateInfo, out: &mut vk::ImageView) -> vk::Result {
        traced!(self, after: r = (self.d().create_image_view)(self.handle(), info, null(), out), lines::create_image_view(info, *out, r))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn destroy_image_view(&self, view: vk::ImageView) {
        traced!(self, before: lines::gone("DestroyImageView", "view", trace::Kind::View, view), (self.d().destroy_image_view)(self.handle(), view, null()))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn create_buffer(&self, info: &vk::BufferCreateInfo, out: &mut vk::Buffer) -> vk::Result {
        traced!(self, after: r = (self.d().create_buffer)(self.handle(), info, null(), out), lines::create_buffer(info, *out, r))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn destroy_buffer(&self, buffer: vk::Buffer) {
        traced!(self, before: lines::gone("DestroyBuffer", "buffer", trace::Kind::Buffer, buffer), (self.d().destroy_buffer)(self.handle(), buffer, null()))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn allocate_memory(&self, info: &vk::MemoryAllocateInfo, out: &mut vk::DeviceMemory) -> vk::Result {
        traced!(self, after: r = (self.d().allocate_memory)(self.handle(), info, null(), out), lines::allocate_memory(info, *out, r))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn free_memory(&self, memory: vk::DeviceMemory) {
        traced!(self, before: lines::gone("FreeMemory", "memory", trace::Kind::Memory, memory), (self.d().free_memory)(self.handle(), memory, null()))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn bind_image_memory(&self, image: vk::Image, memory: vk::DeviceMemory, offset: u64) -> vk::Result {
        traced!(self, after: r = (self.d().bind_image_memory)(self.handle(), image, memory, offset), lines::bind_memory("BindImageMemory", "image", trace::Kind::Image, vk::Handle::as_raw(image), memory, offset, r))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn bind_buffer_memory(&self, buffer: vk::Buffer, memory: vk::DeviceMemory, offset: u64) -> vk::Result {
        traced!(self, after: r = (self.d().bind_buffer_memory)(self.handle(), buffer, memory, offset), lines::bind_memory("BindBufferMemory", "buffer", trace::Kind::Buffer, vk::Handle::as_raw(buffer), memory, offset, r))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn create_sampler(&self, info: &vk::SamplerCreateInfo, out: &mut vk::Sampler) -> vk::Result {
        traced!(self, after: r = (self.d().create_sampler)(self.handle(), info, null(), out), lines::create_sampler(info, *out, r))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn destroy_sampler(&self, sampler: vk::Sampler) {
        traced!(self, before: lines::gone("DestroySampler", "sampler", trace::Kind::Sampler, sampler), (self.d().destroy_sampler)(self.handle(), sampler, null()))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn create_descriptor_pool(
        &self,
        info: &vk::DescriptorPoolCreateInfo,
        out: &mut vk::DescriptorPool,
    ) -> vk::Result {
        traced!(self, after: r = (self.d().create_descriptor_pool)(self.handle(), info, null(), out), lines::create_descriptor_pool(info, *out, r))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn destroy_descriptor_pool(&self, pool: vk::DescriptorPool) {
        traced!(self, before: lines::gone("DestroyDescriptorPool", "pool", trace::Kind::DescriptorPool, pool), (self.d().destroy_descriptor_pool)(self.handle(), pool, null()))
    }

    /// `out` holds info's descriptor_set_count sets.
    ///
    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn allocate_descriptor_sets(
        &self,
        info: &vk::DescriptorSetAllocateInfo,
        out: &mut [vk::DescriptorSet],
    ) -> vk::Result {
        debug_assert!(out.len() >= info.descriptor_set_count as usize);
        traced!(self, after: r = (self.d().allocate_descriptor_sets)(self.handle(), info, out.as_mut_ptr()), lines::allocate_descriptor_sets(info, out.as_ptr(), r))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn free_descriptor_sets(&self, pool: vk::DescriptorPool, sets: &[vk::DescriptorSet]) -> vk::Result {
        traced!(self, before: lines::free_descriptor_sets(pool, sets), (self.d().free_descriptor_sets)(self.handle(), pool, len(sets.len()), sets.as_ptr()))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn update_descriptor_sets(&self, writes: &[vk::WriteDescriptorSet], copies: &[vk::CopyDescriptorSet]) {
        traced!(self, before: lines::update_descriptor_sets(writes, len(copies.len())), (self.d().update_descriptor_sets)(self.handle(), len(writes.len()), writes.as_ptr(), len(copies.len()), copies.as_ptr()))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn create_descriptor_set_layout(
        &self,
        info: &vk::DescriptorSetLayoutCreateInfo,
        out: &mut vk::DescriptorSetLayout,
    ) -> vk::Result {
        traced!(self, after: r = (self.d().create_descriptor_set_layout)(self.handle(), info, null(), out), lines::create_descriptor_set_layout(info, *out, r))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn destroy_descriptor_set_layout(&self, layout: vk::DescriptorSetLayout) {
        traced!(self, before: lines::gone("DestroyDescriptorSetLayout", "layout", trace::Kind::SetLayout, layout), (self.d().destroy_descriptor_set_layout)(self.handle(), layout, null()))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn create_pipeline_layout(
        &self,
        info: &vk::PipelineLayoutCreateInfo,
        out: &mut vk::PipelineLayout,
    ) -> vk::Result {
        traced!(self, after: r = (self.d().create_pipeline_layout)(self.handle(), info, null(), out), lines::create_pipeline_layout(info, *out, r))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn destroy_pipeline_layout(&self, layout: vk::PipelineLayout) {
        traced!(self, before: lines::gone("DestroyPipelineLayout", "layout", trace::Kind::PipelineLayout, layout), (self.d().destroy_pipeline_layout)(self.handle(), layout, null()))
    }

    /// `out` holds a pipeline for each of `infos`.
    ///
    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn create_graphics_pipelines(
        &self,
        cache: vk::PipelineCache,
        infos: &[vk::GraphicsPipelineCreateInfo],
        out: &mut [vk::Pipeline],
    ) -> vk::Result {
        debug_assert!(out.len() >= infos.len());
        traced!(self, after: r = (self.d().create_graphics_pipelines)(self.handle(), cache, len(infos.len()), infos.as_ptr(), null(), out.as_mut_ptr()), lines::create_graphics_pipelines(cache, infos, out.as_ptr(), r))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn create_compute_pipelines(
        &self,
        cache: vk::PipelineCache,
        infos: &[vk::ComputePipelineCreateInfo],
        out: &mut [vk::Pipeline],
    ) -> vk::Result {
        debug_assert!(out.len() >= infos.len());
        traced!(self, after: r = (self.d().create_compute_pipelines)(self.handle(), cache, len(infos.len()), infos.as_ptr(), null(), out.as_mut_ptr()), lines::create_compute_pipelines(cache, infos, out.as_ptr(), r))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn destroy_pipeline(&self, pipeline: vk::Pipeline) {
        traced!(self, before: lines::gone("DestroyPipeline", "pipeline", trace::Kind::Pipeline, pipeline), (self.d().destroy_pipeline)(self.handle(), pipeline, null()))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn create_shader_module(
        &self,
        info: &vk::ShaderModuleCreateInfo,
        out: &mut vk::ShaderModule,
    ) -> vk::Result {
        traced!(self, after: r = (self.d().create_shader_module)(self.handle(), info, null(), out), lines::create_shader_module(info, *out, r))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn destroy_shader_module(&self, module: vk::ShaderModule) {
        traced!(self, before: lines::gone("DestroyShaderModule", "module", trace::Kind::Module, module), (self.d().destroy_shader_module)(self.handle(), module, null()))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn create_command_pool(
        &self,
        info: &vk::CommandPoolCreateInfo,
        out: &mut vk::CommandPool,
    ) -> vk::Result {
        traced!(self, after: r = (self.d().create_command_pool)(self.handle(), info, null(), out), lines::create_command_pool(info, *out, r))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn destroy_command_pool(&self, pool: vk::CommandPool) {
        traced!(self, before: lines::gone("DestroyCommandPool", "pool", trace::Kind::CommandPool, pool), (self.d().destroy_command_pool)(self.handle(), pool, null()))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn reset_command_pool(&self, pool: vk::CommandPool, flags: vk::CommandPoolResetFlags) -> vk::Result {
        traced!(self, before: lines::reset_command_pool(pool, flags), (self.d().reset_command_pool)(self.handle(), pool, flags))
    }

    /// `out` holds info's command_buffer_count buffers.
    ///
    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn allocate_command_buffers(
        &self,
        info: &vk::CommandBufferAllocateInfo,
        out: &mut [vk::CommandBuffer],
    ) -> vk::Result {
        debug_assert!(out.len() >= info.command_buffer_count as usize);
        traced!(self, after: r = (self.d().allocate_command_buffers)(self.handle(), info, out.as_mut_ptr()), lines::allocate_command_buffers(info, out.as_ptr(), r))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn free_command_buffers(&self, pool: vk::CommandPool, commands: &[vk::CommandBuffer]) {
        traced!(self, before: lines::free_command_buffers(pool, commands), (self.d().free_command_buffers)(self.handle(), pool, len(commands.len()), commands.as_ptr()))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn begin_command_buffer(
        &self,
        command: vk::CommandBuffer,
        info: &vk::CommandBufferBeginInfo,
    ) -> vk::Result {
        traced!(self, before: lines::begin_command_buffer(command, info), (self.d().begin_command_buffer)(command, info))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn end_command_buffer(&self, command: vk::CommandBuffer) -> vk::Result {
        traced!(self, before: lines::command_only("EndCommandBuffer", command), (self.d().end_command_buffer)(command))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn create_fence(&self, info: &vk::FenceCreateInfo, out: &mut vk::Fence) -> vk::Result {
        traced!(self, after: r = (self.d().create_fence)(self.handle(), info, null(), out), lines::create_fence(info, *out, r))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn destroy_fence(&self, fence: vk::Fence) {
        traced!(self, before: lines::gone("DestroyFence", "fence", trace::Kind::Fence, fence), (self.d().destroy_fence)(self.handle(), fence, null()))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn reset_fences(&self, fences: &[vk::Fence]) -> vk::Result {
        traced!(self, before: lines::reset_fences(fences), (self.d().reset_fences)(self.handle(), len(fences.len()), fences.as_ptr()))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn create_query_pool(&self, info: &vk::QueryPoolCreateInfo, out: &mut vk::QueryPool) -> vk::Result {
        traced!(self, after: r = (self.d().create_query_pool)(self.handle(), info, null(), out), lines::create_query_pool(info, *out, r))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn destroy_query_pool(&self, pool: vk::QueryPool) {
        traced!(self, before: lines::gone("DestroyQueryPool", "pool", trace::Kind::QueryPool, pool), (self.d().destroy_query_pool)(self.handle(), pool, null()))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn queue_submit(&self, queue: vk::Queue, submits: &[vk::SubmitInfo], fence: vk::Fence) -> vk::Result {
        traced!(self, before: trace::submit_line("QueueSubmit", queue, submits, fence), (self.d().queue_submit)(queue, len(submits.len()), submits.as_ptr(), fence))
    }

    // ---- untraced, as in the C++ ------------------------------------------------------------

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn device_wait_idle(&self) -> vk::Result {
        // SAFETY: the module's contract.
        unsafe { (self.d().device_wait_idle)(self.handle()) }
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn get_buffer_memory_requirements(&self, buffer: vk::Buffer) -> vk::MemoryRequirements {
        let mut out = vk::MemoryRequirements::default();
        // SAFETY: the module's contract.
        unsafe { (self.d().get_buffer_memory_requirements)(self.handle(), buffer, &mut out) };
        out
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn get_image_memory_requirements(&self, image: vk::Image) -> vk::MemoryRequirements {
        let mut out = vk::MemoryRequirements::default();
        // SAFETY: the module's contract.
        unsafe { (self.d().get_image_memory_requirements)(self.handle(), image, &mut out) };
        out
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn get_fence_status(&self, fence: vk::Fence) -> vk::Result {
        // SAFETY: the module's contract.
        unsafe { (self.d().get_fence_status)(self.handle(), fence) }
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn wait_for_fences(&self, fences: &[vk::Fence], wait_all: bool, timeout: u64) -> vk::Result {
        // SAFETY: the module's contract.
        unsafe {
            (self.d().wait_for_fences)(self.handle(), len(fences.len()), fences.as_ptr(), u32::from(wait_all), timeout)
        }
    }

    /// # Safety
    /// The module's contract; `data` holds `count` results at `stride`.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn get_query_pool_results(
        &self,
        pool: vk::QueryPool,
        first: u32,
        count: u32,
        data: &mut [u8],
        stride: u64,
        flags: vk::QueryResultFlags,
    ) -> vk::Result {
        // SAFETY: the module's contract.
        unsafe {
            (self.d().get_query_pool_results)(
                self.handle(),
                pool,
                first,
                count,
                data.len(),
                data.as_mut_ptr().cast(),
                stride,
                flags,
            )
        }
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn map_memory(
        &self,
        memory: vk::DeviceMemory,
        offset: u64,
        size: u64,
        out: &mut *mut c_void,
    ) -> vk::Result {
        // SAFETY: the module's contract.
        unsafe { (self.d().map_memory)(self.handle(), memory, offset, size, vk::MemoryMapFlags::empty(), out) }
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn unmap_memory(&self, memory: vk::DeviceMemory) {
        // SAFETY: the module's contract.
        unsafe { (self.d().unmap_memory)(self.handle(), memory) }
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn get_physical_device_format_properties(
        &self,
        physical: vk::PhysicalDevice,
        format: vk::Format,
    ) -> vk::FormatProperties {
        let mut out = vk::FormatProperties::default();
        // SAFETY: the module's contract.
        unsafe { (self.instance.fp_v1_0().get_physical_device_format_properties)(physical, format, &mut out) };
        out
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn get_physical_device_memory_properties(
        &self,
        physical: vk::PhysicalDevice,
    ) -> vk::PhysicalDeviceMemoryProperties {
        let mut out = vk::PhysicalDeviceMemoryProperties::default();
        // SAFETY: the module's contract.
        unsafe { (self.instance.fp_v1_0().get_physical_device_memory_properties)(physical, &mut out) };
        out
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn get_physical_device_properties(&self, physical: vk::PhysicalDevice) -> vk::PhysicalDeviceProperties {
        let mut out = vk::PhysicalDeviceProperties::default();
        // SAFETY: the module's contract.
        unsafe { (self.instance.fp_v1_0().get_physical_device_properties)(physical, &mut out) };
        out
    }

    /// `out`'s pNext chain is filled in too.
    ///
    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn get_physical_device_properties2(
        &self,
        physical: vk::PhysicalDevice,
        out: &mut vk::PhysicalDeviceProperties2,
    ) {
        // SAFETY: the module's contract.
        unsafe { (self.instance.fp_v1_1().get_physical_device_properties2)(physical, out) };
    }

    /// The families' properties into `out` (all when `out` is empty, None: the count).
    ///
    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn get_physical_device_queue_family_properties(
        &self,
        physical: vk::PhysicalDevice,
        out: Option<&mut [vk::QueueFamilyProperties]>,
    ) -> u32 {
        let fp = self.instance.fp_v1_0().get_physical_device_queue_family_properties;
        match out {
            None => {
                let mut count = 0;
                // SAFETY: the module's contract.
                unsafe { fp(physical, &mut count, std::ptr::null_mut()) };
                count
            }
            Some(out) => {
                let mut count = len(out.len());
                // SAFETY: the module's contract; `out` holds `count`.
                unsafe { fp(physical, &mut count, out.as_mut_ptr()) };
                count
            }
        }
    }

    // ---- commands -----------------------------------------------------------------------------

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn cmd_bind_descriptor_sets(
        &self,
        command: vk::CommandBuffer,
        bind: vk::PipelineBindPoint,
        layout: vk::PipelineLayout,
        first: u32,
        sets: &[vk::DescriptorSet],
        dynamic: &[u32],
    ) {
        traced!(self, before: lines::cmd_bind_descriptor_sets(command, bind, layout, first, sets, dynamic), (self.d().cmd_bind_descriptor_sets)(command, bind, layout, first, len(sets.len()), sets.as_ptr(), len(dynamic.len()), dynamic.as_ptr()))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn cmd_pipeline_barrier(
        &self,
        command: vk::CommandBuffer,
        source: vk::PipelineStageFlags,
        destination: vk::PipelineStageFlags,
        dependency: vk::DependencyFlags,
        memory: &[vk::MemoryBarrier],
        buffers: &[vk::BufferMemoryBarrier],
        images: &[vk::ImageMemoryBarrier],
    ) {
        traced!(self, before: lines::cmd_pipeline_barrier(command, source, destination, dependency, memory, buffers, images), (self.d().cmd_pipeline_barrier)(command, source, destination, dependency, len(memory.len()), memory.as_ptr(), len(buffers.len()), buffers.as_ptr(), len(images.len()), images.as_ptr()))
    }

    /// util.h's barrier (util.rs's renderer_barrier): traced only when it goes into a
    /// command buffer traced code made.
    ///
    /// # Safety
    /// The module's contract.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn util_barrier(
        &self,
        command: vk::CommandBuffer,
        source: vk::PipelineStageFlags,
        destination: vk::PipelineStageFlags,
        dependency: vk::DependencyFlags,
        memory: &[vk::MemoryBarrier],
        buffers: &[vk::BufferMemoryBarrier],
        images: &[vk::ImageMemoryBarrier],
    ) {
        traced!(self, before: if trace::known_command(command) { lines::cmd_pipeline_barrier(command, source, destination, dependency, memory, buffers, images) }, (self.d().cmd_pipeline_barrier)(command, source, destination, dependency, len(memory.len()), memory.as_ptr(), len(buffers.len()), buffers.as_ptr(), len(images.len()), images.as_ptr()))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn cmd_begin_query(
        &self,
        command: vk::CommandBuffer,
        pool: vk::QueryPool,
        query: u32,
        flags: vk::QueryControlFlags,
    ) {
        traced!(self, before: lines::cmd_begin_query(command, pool, query, flags), (self.d().cmd_begin_query)(command, pool, query, flags))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn cmd_end_query(&self, command: vk::CommandBuffer, pool: vk::QueryPool, query: u32) {
        traced!(self, before: lines::cmd_end_query(command, pool, query), (self.d().cmd_end_query)(command, pool, query))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn cmd_reset_query_pool(&self, command: vk::CommandBuffer, pool: vk::QueryPool, first: u32, count: u32) {
        traced!(self, before: lines::cmd_reset_query_pool(command, pool, first, count), (self.d().cmd_reset_query_pool)(command, pool, first, count))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn cmd_write_timestamp(
        &self,
        command: vk::CommandBuffer,
        stage: vk::PipelineStageFlags,
        pool: vk::QueryPool,
        query: u32,
    ) {
        traced!(self, before: lines::cmd_write_timestamp(command, stage, pool, query), (self.d().cmd_write_timestamp)(command, stage, pool, query))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn cmd_bind_index_buffer(
        &self,
        command: vk::CommandBuffer,
        buffer: vk::Buffer,
        offset: u64,
        kind: vk::IndexType,
    ) {
        traced!(self, before: lines::cmd_bind_index_buffer(command, buffer, offset, kind), (self.d().cmd_bind_index_buffer)(command, buffer, offset, kind))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn cmd_bind_pipeline(
        &self,
        command: vk::CommandBuffer,
        bind: vk::PipelineBindPoint,
        pipeline: vk::Pipeline,
    ) {
        traced!(self, before: lines::cmd_bind_pipeline(command, bind, pipeline), (self.d().cmd_bind_pipeline)(command, bind, pipeline))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn cmd_draw(
        &self,
        command: vk::CommandBuffer,
        vertices: u32,
        instances: u32,
        first_vertex: u32,
        first_instance: u32,
    ) {
        traced!(self, before: lines::cmd_draw(command, vertices, instances, first_vertex, first_instance), (self.d().cmd_draw)(command, vertices, instances, first_vertex, first_instance))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn cmd_draw_indexed(
        &self,
        command: vk::CommandBuffer,
        indices: u32,
        instances: u32,
        first_index: u32,
        vertex_offset: i32,
        first_instance: u32,
    ) {
        traced!(self, before: lines::cmd_draw_indexed(command, indices, instances, first_index, vertex_offset, first_instance), (self.d().cmd_draw_indexed)(command, indices, instances, first_index, vertex_offset, first_instance))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn cmd_set_depth_bias(&self, command: vk::CommandBuffer, constant: f32, clamp: f32, slope: f32) {
        traced!(self, before: lines::cmd_set_depth_bias(command, constant, clamp, slope), (self.d().cmd_set_depth_bias)(command, constant, clamp, slope))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn cmd_set_scissor(&self, command: vk::CommandBuffer, first: u32, rects: &[vk::Rect2D]) {
        traced!(self, before: lines::cmd_set_scissor(command, first, rects), (self.d().cmd_set_scissor)(command, first, len(rects.len()), rects.as_ptr()))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn cmd_set_viewport(&self, command: vk::CommandBuffer, first: u32, viewports: &[vk::Viewport]) {
        traced!(self, before: lines::cmd_set_viewport(command, first, viewports), (self.d().cmd_set_viewport)(command, first, len(viewports.len()), viewports.as_ptr()))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn cmd_set_stencil_compare_mask(
        &self,
        command: vk::CommandBuffer,
        faces: vk::StencilFaceFlags,
        mask: u32,
    ) {
        traced!(self, before: lines::cmd_stencil("CmdSetStencilCompareMask", "mask", command, faces, mask), (self.d().cmd_set_stencil_compare_mask)(command, faces, mask))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn cmd_set_stencil_write_mask(
        &self,
        command: vk::CommandBuffer,
        faces: vk::StencilFaceFlags,
        mask: u32,
    ) {
        traced!(self, before: lines::cmd_stencil("CmdSetStencilWriteMask", "mask", command, faces, mask), (self.d().cmd_set_stencil_write_mask)(command, faces, mask))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn cmd_set_stencil_reference(
        &self,
        command: vk::CommandBuffer,
        faces: vk::StencilFaceFlags,
        reference: u32,
    ) {
        traced!(self, before: lines::cmd_stencil("CmdSetStencilReference", "reference", command, faces, reference), (self.d().cmd_set_stencil_reference)(command, faces, reference))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn cmd_blit_image(
        &self,
        command: vk::CommandBuffer,
        source: vk::Image,
        source_layout: vk::ImageLayout,
        destination: vk::Image,
        destination_layout: vk::ImageLayout,
        regions: &[vk::ImageBlit],
        filter: vk::Filter,
    ) {
        traced!(self, before: lines::cmd_blit_image(command, source, source_layout, destination, destination_layout, regions, filter), (self.d().cmd_blit_image)(command, source, source_layout, destination, destination_layout, len(regions.len()), regions.as_ptr(), filter))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn cmd_copy_image(
        &self,
        command: vk::CommandBuffer,
        source: vk::Image,
        source_layout: vk::ImageLayout,
        destination: vk::Image,
        destination_layout: vk::ImageLayout,
        regions: &[vk::ImageCopy],
    ) {
        traced!(self, before: lines::cmd_copy_image("CmdCopyImage", command, source, source_layout, destination, destination_layout, regions.iter().map(|r| (r.src_subresource, r.src_offset, r.dst_subresource, r.dst_offset, r.extent))), (self.d().cmd_copy_image)(command, source, source_layout, destination, destination_layout, len(regions.len()), regions.as_ptr()))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn cmd_resolve_image(
        &self,
        command: vk::CommandBuffer,
        source: vk::Image,
        source_layout: vk::ImageLayout,
        destination: vk::Image,
        destination_layout: vk::ImageLayout,
        regions: &[vk::ImageResolve],
    ) {
        traced!(self, before: lines::cmd_copy_image("CmdResolveImage", command, source, source_layout, destination, destination_layout, regions.iter().map(|r| (r.src_subresource, r.src_offset, r.dst_subresource, r.dst_offset, r.extent))), (self.d().cmd_resolve_image)(command, source, source_layout, destination, destination_layout, len(regions.len()), regions.as_ptr()))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn cmd_clear_color_image(
        &self,
        command: vk::CommandBuffer,
        image: vk::Image,
        layout: vk::ImageLayout,
        colour: &vk::ClearColorValue,
        ranges: &[vk::ImageSubresourceRange],
    ) {
        traced!(self, before: lines::cmd_clear_color_image(command, image, layout, colour, ranges), (self.d().cmd_clear_color_image)(command, image, layout, colour, len(ranges.len()), ranges.as_ptr()))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn cmd_clear_depth_stencil_image(
        &self,
        command: vk::CommandBuffer,
        image: vk::Image,
        layout: vk::ImageLayout,
        value: &vk::ClearDepthStencilValue,
        ranges: &[vk::ImageSubresourceRange],
    ) {
        traced!(self, before: lines::cmd_clear_depth_stencil_image(command, image, layout, value, ranges), (self.d().cmd_clear_depth_stencil_image)(command, image, layout, value, len(ranges.len()), ranges.as_ptr()))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn cmd_copy_buffer_to_image(
        &self,
        command: vk::CommandBuffer,
        buffer: vk::Buffer,
        image: vk::Image,
        layout: vk::ImageLayout,
        regions: &[vk::BufferImageCopy],
    ) {
        traced!(self, before: lines::cmd_copy_buffer_image(true, command, buffer, image, layout, regions), (self.d().cmd_copy_buffer_to_image)(command, buffer, image, layout, len(regions.len()), regions.as_ptr()))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn cmd_copy_image_to_buffer(
        &self,
        command: vk::CommandBuffer,
        image: vk::Image,
        layout: vk::ImageLayout,
        buffer: vk::Buffer,
        regions: &[vk::BufferImageCopy],
    ) {
        traced!(self, before: lines::cmd_copy_buffer_image(false, command, buffer, image, layout, regions), (self.d().cmd_copy_image_to_buffer)(command, image, layout, buffer, len(regions.len()), regions.as_ptr()))
    }

    /// # Safety
    /// The module's contract.
    #[inline]
    pub unsafe fn cmd_dispatch(&self, command: vk::CommandBuffer, x: u32, y: u32, z: u32) {
        traced!(self, before: lines::cmd_dispatch(command, x, y, z), (self.d().cmd_dispatch)(command, x, y, z))
    }

    /// # Safety
    /// The module's contract; `data` is a multiple of 4 bytes.
    #[inline]
    pub unsafe fn cmd_update_buffer(&self, command: vk::CommandBuffer, buffer: vk::Buffer, offset: u64, data: &[u8]) {
        traced!(self, before: lines::cmd_update_buffer(command, buffer, offset, data), (self.d().cmd_update_buffer)(command, buffer, offset, data.len() as u64, data.as_ptr().cast()))
    }

    // ---- the extension commands (Dispatch) ----------------------------------------------

    /// # Safety
    /// The module's contract; the device gives dynamic rendering (dynamic_commands).
    #[inline]
    pub unsafe fn cmd_begin_rendering(&self, command: vk::CommandBuffer, info: &vk::RenderingInfo) {
        traced!(self, before: lines::cmd_begin_rendering(command, info), (self.dynamic_rendering.fp().cmd_begin_rendering_khr)(command, info))
    }

    /// # Safety
    /// As cmd_begin_rendering.
    #[inline]
    pub unsafe fn cmd_end_rendering(&self, command: vk::CommandBuffer) {
        traced!(self, before: lines::command_only("CmdEndRendering", command), (self.dynamic_rendering.fp().cmd_end_rendering_khr)(command))
    }

    /// # Safety
    /// The module's contract; the device gives extended dynamic state.
    #[inline]
    pub unsafe fn cmd_set_cull_mode(&self, command: vk::CommandBuffer, mode: vk::CullModeFlags) {
        traced!(self, before: lines::cmd_value("CmdSetCullMode", "mode", true, command, u64::from(mode.as_raw())), (self.extended_dynamic_state.fp().cmd_set_cull_mode_ext)(command, mode))
    }

    /// # Safety
    /// As cmd_set_cull_mode.
    #[inline]
    pub unsafe fn cmd_set_front_face(&self, command: vk::CommandBuffer, face: vk::FrontFace) {
        traced!(self, before: lines::cmd_value("CmdSetFrontFace", "face", false, command, face.as_raw() as u64), (self.extended_dynamic_state.fp().cmd_set_front_face_ext)(command, face))
    }

    /// # Safety
    /// As cmd_set_cull_mode.
    #[inline]
    pub unsafe fn cmd_set_depth_test_enable(&self, command: vk::CommandBuffer, on: vk::Bool32) {
        traced!(self, before: lines::cmd_value("CmdSetDepthTestEnable", "on", false, command, u64::from(on)), (self.extended_dynamic_state.fp().cmd_set_depth_test_enable_ext)(command, on))
    }

    /// # Safety
    /// As cmd_set_cull_mode.
    #[inline]
    pub unsafe fn cmd_set_depth_write_enable(&self, command: vk::CommandBuffer, on: vk::Bool32) {
        traced!(self, before: lines::cmd_value("CmdSetDepthWriteEnable", "on", false, command, u64::from(on)), (self.extended_dynamic_state.fp().cmd_set_depth_write_enable_ext)(command, on))
    }

    /// # Safety
    /// As cmd_set_cull_mode.
    #[inline]
    pub unsafe fn cmd_set_depth_compare_op(&self, command: vk::CommandBuffer, op: vk::CompareOp) {
        traced!(self, before: lines::cmd_value("CmdSetDepthCompareOp", "op", false, command, op.as_raw() as u64), (self.extended_dynamic_state.fp().cmd_set_depth_compare_op_ext)(command, op))
    }

    /// # Safety
    /// As cmd_set_cull_mode.
    #[inline]
    pub unsafe fn cmd_set_stencil_test_enable(&self, command: vk::CommandBuffer, on: vk::Bool32) {
        traced!(self, before: lines::cmd_value("CmdSetStencilTestEnable", "on", false, command, u64::from(on)), (self.extended_dynamic_state.fp().cmd_set_stencil_test_enable_ext)(command, on))
    }

    /// # Safety
    /// As cmd_set_cull_mode.
    #[inline]
    pub unsafe fn cmd_set_stencil_op(
        &self,
        command: vk::CommandBuffer,
        faces: vk::StencilFaceFlags,
        fail: vk::StencilOp,
        pass: vk::StencilOp,
        depth_fail: vk::StencilOp,
        compare: vk::CompareOp,
    ) {
        traced!(self, before: lines::cmd_set_stencil_op(command, faces, fail, pass, depth_fail, compare), (self.extended_dynamic_state.fp().cmd_set_stencil_op_ext)(command, faces, fail, pass, depth_fail, compare))
    }

    /// # Safety
    /// The module's contract; multi_draw() is not 0.
    #[inline]
    pub unsafe fn cmd_draw_multi(
        &self,
        command: vk::CommandBuffer,
        draws: &[vk::MultiDrawInfoEXT],
        instances: u32,
        first_instance: u32,
    ) {
        debug_assert!(self.multi_draw.is_some());
        let stride = size_of::<vk::MultiDrawInfoEXT>() as u32;
        let Some(multi) = self.multi_draw.as_ref() else { return };
        traced!(self, before: lines::cmd_draw_multi(command, draws, instances, first_instance), (multi.fp().cmd_draw_multi_ext)(command, len(draws.len()), draws.as_ptr(), instances, first_instance, stride))
    }

    /// # Safety
    /// As cmd_draw_multi.
    #[inline]
    pub unsafe fn cmd_draw_multi_indexed(
        &self,
        command: vk::CommandBuffer,
        draws: &[vk::MultiDrawIndexedInfoEXT],
        instances: u32,
        first_instance: u32,
        vertex_offset: Option<&i32>,
    ) {
        debug_assert!(self.multi_draw.is_some());
        let stride = size_of::<vk::MultiDrawIndexedInfoEXT>() as u32;
        let offset = vertex_offset.map_or(std::ptr::null(), |offset| offset as *const i32);
        let Some(multi) = self.multi_draw.as_ref() else { return };
        traced!(self, before: lines::cmd_draw_multi_indexed(command, draws, instances, first_instance, vertex_offset.copied()), (multi.fp().cmd_draw_multi_indexed_ext)(command, len(draws.len()), draws.as_ptr(), instances, first_instance, stride, offset))
    }
}

/// A state library made by the launcher's shared MakeLibrary, which is not traced: the
/// create info and chain it was given, written field by field once it is named.
///
/// # Safety
/// `info` is valid with `next` as its pNext chain.
#[cfg(feature = "diagnostics")]
pub unsafe fn state_library(
    made: vk::Pipeline,
    part: vk::GraphicsPipelineLibraryFlagsEXT,
    info: &vk::GraphicsPipelineCreateInfo,
    next: *const c_void,
) {
    if !trace::on() {
        return;
    }
    let mut given = *info;
    given.p_next = next;
    // SAFETY: the caller's.
    unsafe { lines::state_library(made, part, &given) };
}

// The line of each traced call, as trace.cpp's wrapper writes it.
#[cfg(feature = "diagnostics")]
mod lines {
    use super::trace::{Kind, Line, bits, put};
    use ash::vk::{self, Handle};
    use std::fmt::Write as _;

    // An enum as C++ passes it to a uint64_t: sign-extended.
    fn e(value: i32) -> u64 {
        value as u64
    }

    fn result(l: &mut Line, r: vk::Result) {
        l.i("r", i64::from(r.as_raw()));
    }

    // A handle made, or 0 when the call failed.
    fn out<H: Handle>(made: H, r: vk::Result) -> u64 {
        if r == vk::Result::SUCCESS { made.as_raw() } else { 0 }
    }

    #[cold]
    #[inline(never)]
    pub fn gone<H: Handle>(command: &str, key: &str, kind: Kind, handle: H) {
        Line::new(command).gone(key, kind, handle.as_raw());
    }

    #[cold]
    #[inline(never)]
    pub fn command_only(command: &str, c: vk::CommandBuffer) {
        Line::new(command).h("cmd", Kind::Command, c.as_raw());
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn create_image(i: &vk::ImageCreateInfo, o: vk::Image, r: vk::Result) {
        let mut l = Line::new("CreateImage");
        l.made("out", Kind::Image, out(o, r))
            .x("flags", u64::from(i.flags.as_raw()))
            .u("type", e(i.image_type.as_raw()))
            .u("format", e(i.format.as_raw()))
            .key("extent")
            .extent(&i.extent);
        l.u("mips", u64::from(i.mip_levels))
            .u("layers", u64::from(i.array_layers))
            .u("samples", u64::from(i.samples.as_raw()))
            .u("tiling", e(i.tiling.as_raw()))
            .x("usage", u64::from(i.usage.as_raw()))
            .u("sharing", e(i.sharing_mode.as_raw()))
            .u("initial", e(i.initial_layout.as_raw()));
        // SAFETY: the chain of a valid structure.
        unsafe { l.next(i.p_next) };
        result(&mut l, r);
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn create_image_view(i: &vk::ImageViewCreateInfo, o: vk::ImageView, r: vk::Result) {
        let mut l = Line::new("CreateImageView");
        l.made("out", Kind::View, out(o, r))
            .h("image", Kind::Image, i.image.as_raw())
            .x("flags", u64::from(i.flags.as_raw()))
            .u("type", e(i.view_type.as_raw()))
            .u("format", e(i.format.as_raw()));
        put!(
            l,
            " swizzle={},{},{},{} range=",
            i.components.r.as_raw() as u32,
            i.components.g.as_raw() as u32,
            i.components.b.as_raw() as u32,
            i.components.a.as_raw() as u32
        );
        l.range(&i.subresource_range);
        // SAFETY: the chain of a valid structure.
        unsafe { l.next(i.p_next) };
        result(&mut l, r);
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn create_buffer(i: &vk::BufferCreateInfo, o: vk::Buffer, r: vk::Result) {
        let mut l = Line::new("CreateBuffer");
        l.made("out", Kind::Buffer, out(o, r))
            .x("flags", u64::from(i.flags.as_raw()))
            .u("size", i.size)
            .x("usage", u64::from(i.usage.as_raw()))
            .u("sharing", e(i.sharing_mode.as_raw()));
        // SAFETY: the chain of a valid structure.
        unsafe { l.next(i.p_next) };
        result(&mut l, r);
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn allocate_memory(i: &vk::MemoryAllocateInfo, o: vk::DeviceMemory, r: vk::Result) {
        let mut l = Line::new("AllocateMemory");
        l.made("out", Kind::Memory, out(o, r)).u("size", i.allocation_size).u("type", u64::from(i.memory_type_index));
        // SAFETY: the chain of a valid structure.
        unsafe { l.next(i.p_next) };
        result(&mut l, r);
    }

    #[cold]
    #[inline(never)]
    pub fn bind_memory(
        command: &str,
        key: &str,
        kind: Kind,
        object: u64,
        memory: vk::DeviceMemory,
        offset: u64,
        r: vk::Result,
    ) {
        let mut l = Line::new(command);
        l.h(key, kind, object).h("memory", Kind::Memory, memory.as_raw()).u("offset", offset);
        result(&mut l, r);
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn create_sampler(i: &vk::SamplerCreateInfo, o: vk::Sampler, r: vk::Result) {
        let mut l = Line::new("CreateSampler");
        l.made("out", Kind::Sampler, out(o, r))
            .x("flags", u64::from(i.flags.as_raw()))
            .u("mag", e(i.mag_filter.as_raw()))
            .u("min", e(i.min_filter.as_raw()))
            .u("mip", e(i.mipmap_mode.as_raw()));
        put!(
            l,
            " address={},{},{}",
            i.address_mode_u.as_raw() as u32,
            i.address_mode_v.as_raw() as u32,
            i.address_mode_w.as_raw() as u32
        );
        l.f("lodBias", i.mip_lod_bias)
            .u("aniso", u64::from(i.anisotropy_enable))
            .f("maxAniso", i.max_anisotropy)
            .u("compare", u64::from(i.compare_enable))
            .u("compareOp", e(i.compare_op.as_raw()))
            .f("minLod", i.min_lod)
            .f("maxLod", i.max_lod)
            .u("border", e(i.border_color.as_raw()))
            .u("unnormalised", u64::from(i.unnormalized_coordinates));
        // SAFETY: the chain of a valid structure.
        unsafe { l.next(i.p_next) };
        result(&mut l, r);
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn create_descriptor_pool(i: &vk::DescriptorPoolCreateInfo, o: vk::DescriptorPool, r: vk::Result) {
        let mut l = Line::new("CreateDescriptorPool");
        l.made("out", Kind::DescriptorPool, out(o, r))
            .x("flags", u64::from(i.flags.as_raw()))
            .u("maxSets", u64::from(i.max_sets));
        l.raw(" sizes=[");
        for n in 0..i.pool_size_count as usize {
            // SAFETY: the array holds its count.
            let size = unsafe { &*i.p_pool_sizes.add(n) };
            let (kind, count) = (size.ty.as_raw() as u32, size.descriptor_count);
            if n != 0 {
                put!(l, ",({kind},{count})");
            } else {
                put!(l, "({kind},{count})");
            }
        }
        l.raw("]");
        // SAFETY: the chain of a valid structure.
        unsafe { l.next(i.p_next) };
        result(&mut l, r);
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn allocate_descriptor_sets(
        i: &vk::DescriptorSetAllocateInfo,
        o: *const vk::DescriptorSet,
        r: vk::Result,
    ) {
        let mut l = Line::new("AllocateDescriptorSets");
        l.h("pool", Kind::DescriptorPool, i.descriptor_pool.as_raw());
        // SAFETY: the array holds its count.
        unsafe { l.hs("layouts", Kind::SetLayout, i.p_set_layouts, i.descriptor_set_count) };
        l.raw(" out=[");
        for n in 0..i.descriptor_set_count as usize {
            if n != 0 {
                l.raw(",");
            }
            if r == vk::Result::SUCCESS {
                // SAFETY: the call wrote the count.
                let name = l.allocated(Kind::Set, unsafe { *o.add(n) }.as_raw());
                l.raw(&name);
            } else {
                l.raw("0");
            }
        }
        l.raw("]");
        // SAFETY: the chain of a valid structure.
        unsafe { l.next(i.p_next) };
        result(&mut l, r);
    }

    #[cold]
    #[inline(never)]
    pub fn free_descriptor_sets(p: vk::DescriptorPool, sets: &[vk::DescriptorSet]) {
        let mut l = Line::new("FreeDescriptorSets");
        l.h("pool", Kind::DescriptorPool, p.as_raw());
        // SAFETY: a slice.
        unsafe { l.hs("sets", Kind::Set, sets.as_ptr(), sets.len() as u32) };
        for set in sets {
            l.forget(Kind::Set, set.as_raw());
        }
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn update_descriptor_sets(writes: &[vk::WriteDescriptorSet], copies: u32) {
        let mut l = Line::new("UpdateDescriptorSets");
        l.raw(" writes=[");
        for (n, one) in writes.iter().enumerate() {
            if n != 0 {
                l.raw(",");
            }
            l.raw("{")
                .h("set", Kind::Set, one.dst_set.as_raw())
                .u("binding", u64::from(one.dst_binding))
                .u("element", u64::from(one.dst_array_element))
                .u("type", e(one.descriptor_type.as_raw()))
                .u("n", u64::from(one.descriptor_count));
            if !one.p_image_info.is_null() {
                l.raw(" images=[");
                for k in 0..one.descriptor_count as usize {
                    if k != 0 {
                        l.raw(",");
                    }
                    // SAFETY: the array holds descriptor_count.
                    let image = unsafe { &*one.p_image_info.add(k) };
                    l.raw("(");
                    l.name(Kind::Sampler, image.sampler.as_raw());
                    l.raw(",");
                    l.name(Kind::View, image.image_view.as_raw());
                    put!(l, ",{})", image.image_layout.as_raw() as u32);
                }
                l.raw("]");
            }
            if !one.p_buffer_info.is_null() {
                l.raw(" buffers=[");
                for k in 0..one.descriptor_count as usize {
                    if k != 0 {
                        l.raw(",");
                    }
                    // SAFETY: the array holds descriptor_count.
                    let buffer = unsafe { &*one.p_buffer_info.add(k) };
                    l.raw("(");
                    l.name(Kind::Buffer, buffer.buffer.as_raw());
                    put!(l, ",{},{:x})", buffer.offset, buffer.range);
                }
                l.raw("]");
            }
            // SAFETY: the chain of a valid structure.
            unsafe { l.next(one.p_next) };
            l.raw("}");
        }
        put!(l, "] copies={copies}");
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn create_descriptor_set_layout(
        i: &vk::DescriptorSetLayoutCreateInfo,
        o: vk::DescriptorSetLayout,
        r: vk::Result,
    ) {
        let mut l = Line::new("CreateDescriptorSetLayout");
        l.made("out", Kind::SetLayout, out(o, r)).x("flags", u64::from(i.flags.as_raw())).raw(" bindings=[");
        for n in 0..i.binding_count as usize {
            // SAFETY: the array holds its count.
            let b = unsafe { &*i.p_bindings.add(n) };
            put!(
                l,
                "{}({},{},{},{:x},",
                if n != 0 { "," } else { "" },
                b.binding,
                b.descriptor_type.as_raw() as u32,
                b.descriptor_count,
                b.stage_flags.as_raw()
            );
            if !b.p_immutable_samplers.is_null() {
                for k in 0..b.descriptor_count as usize {
                    if k != 0 {
                        l.raw("/");
                    }
                    // SAFETY: the array holds descriptor_count.
                    l.name(Kind::Sampler, unsafe { *b.p_immutable_samplers.add(k) }.as_raw());
                }
            } else {
                l.raw("-");
            }
            l.raw(")");
        }
        l.raw("]");
        // SAFETY: the chain of a valid structure.
        unsafe { l.next(i.p_next) };
        result(&mut l, r);
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn create_pipeline_layout(i: &vk::PipelineLayoutCreateInfo, o: vk::PipelineLayout, r: vk::Result) {
        let mut l = Line::new("CreatePipelineLayout");
        l.made("out", Kind::PipelineLayout, out(o, r)).x("flags", u64::from(i.flags.as_raw()));
        // SAFETY: the array holds its count.
        unsafe { l.hs("sets", Kind::SetLayout, i.p_set_layouts, i.set_layout_count) };
        l.raw(" push=[");
        for n in 0..i.push_constant_range_count as usize {
            // SAFETY: the array holds its count.
            let p = unsafe { &*i.p_push_constant_ranges.add(n) };
            let (stages, offset, size) = (p.stage_flags.as_raw(), p.offset, p.size);
            if n != 0 {
                put!(l, ",({stages:x},{offset},{size})");
            } else {
                put!(l, "({stages:x},{offset},{size})");
            }
        }
        l.raw("]");
        // SAFETY: the chain of a valid structure.
        unsafe { l.next(i.p_next) };
        result(&mut l, r);
    }

    unsafe fn stages(l: &mut Line, stages: *const vk::PipelineShaderStageCreateInfo, count: u32) {
        l.raw(" stages=[");
        for n in 0..count as usize {
            if n != 0 {
                l.raw(",");
            }
            // SAFETY: the array holds its count.
            let s = unsafe { &*stages.add(n) };
            put!(l, "({:x},{:x},", s.stage.as_raw(), s.flags.as_raw());
            l.name(Kind::Module, s.module.as_raw());
            if s.p_name.is_null() {
                l.raw(",-");
            } else {
                // SAFETY: a C string.
                let name = unsafe { std::ffi::CStr::from_ptr(s.p_name) };
                put!(l, ",{}", name.to_string_lossy());
            }
            if !s.p_specialization_info.is_null() {
                // SAFETY: a valid structure, its arrays holding their counts.
                let spec = unsafe { &*s.p_specialization_info };
                for k in 0..spec.map_entry_count as usize {
                    // SAFETY: as above.
                    let entry = unsafe { &*spec.p_map_entries.add(k) };
                    put!(l, ",spec{}@{}:{}", entry.constant_id, entry.offset, entry.size);
                }
                let data = if spec.data_size == 0 {
                    &[][..]
                } else {
                    // SAFETY: data_size readable bytes.
                    unsafe { std::slice::from_raw_parts(spec.p_data.cast::<u8>(), spec.data_size) }
                };
                put!(l, ",data{:016x}", super::trace::fnv64(data));
            }
            // SAFETY: the chain of a valid structure.
            unsafe { l.next(s.p_next) };
            l.raw(")");
        }
        l.raw("]");
    }

    unsafe fn graphics(l: &mut Line, p: &vk::GraphicsPipelineCreateInfo) {
        l.x("flags", u64::from(p.flags.as_raw()));
        // SAFETY: the structure's arrays and pointers are valid (the caller's).
        unsafe {
            stages(l, p.p_stages, p.stage_count);
            if let Some(input) = p.p_vertex_input_state.as_ref() {
                l.u("bindings", u64::from(input.vertex_binding_description_count))
                    .u("attributes", u64::from(input.vertex_attribute_description_count));
            }
            if let Some(a) = p.p_input_assembly_state.as_ref() {
                l.u("topology", e(a.topology.as_raw())).u("restart", u64::from(a.primitive_restart_enable));
            }
            if let Some(v) = p.p_viewport_state.as_ref() {
                l.u("viewports", u64::from(v.viewport_count)).u("scissors", u64::from(v.scissor_count));
            }
            if let Some(r) = p.p_rasterization_state.as_ref() {
                put!(
                    l,
                    " raster=({},{},{},{:x},{},{},{:08x},{:08x},{:08x},{:08x})",
                    r.depth_clamp_enable,
                    r.rasterizer_discard_enable,
                    r.polygon_mode.as_raw() as u32,
                    r.cull_mode.as_raw(),
                    r.front_face.as_raw() as u32,
                    r.depth_bias_enable,
                    bits(r.depth_bias_constant_factor),
                    bits(r.depth_bias_clamp),
                    bits(r.depth_bias_slope_factor),
                    bits(r.line_width)
                );
                l.next(r.p_next);
            }
            if let Some(m) = p.p_multisample_state.as_ref() {
                let mask = if m.p_sample_mask.is_null() { 0xFFFF_FFFF } else { *m.p_sample_mask };
                put!(
                    l,
                    " samples={} shading=({},{:08x}) mask={:x} a2c={}",
                    m.rasterization_samples.as_raw(),
                    m.sample_shading_enable,
                    bits(m.min_sample_shading),
                    mask,
                    m.alpha_to_coverage_enable
                );
            }
            if let Some(d) = p.p_depth_stencil_state.as_ref() {
                put!(
                    l,
                    " depth=({},{},{},{},{}) front=",
                    d.depth_test_enable,
                    d.depth_write_enable,
                    d.depth_compare_op.as_raw() as u32,
                    d.depth_bounds_test_enable,
                    d.stencil_test_enable
                );
                l.stencil(&d.front).raw(" back=").stencil(&d.back);
            }
            if let Some(b) = p.p_color_blend_state.as_ref() {
                put!(
                    l,
                    " blend=({},{},{:08x},{:08x},{:08x},{:08x})[",
                    b.logic_op_enable,
                    b.logic_op.as_raw() as u32,
                    bits(b.blend_constants[0]),
                    bits(b.blend_constants[1]),
                    bits(b.blend_constants[2]),
                    bits(b.blend_constants[3])
                );
                for n in 0..b.attachment_count as usize {
                    let a = &*b.p_attachments.add(n);
                    put!(
                        l,
                        "{}({},{},{},{},{},{},{},mask={:x})",
                        if n != 0 { "," } else { "" },
                        a.blend_enable,
                        a.src_color_blend_factor.as_raw() as u32,
                        a.dst_color_blend_factor.as_raw() as u32,
                        a.color_blend_op.as_raw() as u32,
                        a.src_alpha_blend_factor.as_raw() as u32,
                        a.dst_alpha_blend_factor.as_raw() as u32,
                        a.alpha_blend_op.as_raw() as u32,
                        a.color_write_mask.as_raw()
                    );
                }
                l.raw("]");
            }
            if let Some(d) = p.p_dynamic_state.as_ref() {
                l.raw(" dynamic=[");
                for n in 0..d.dynamic_state_count as usize {
                    let state = (*d.p_dynamic_states.add(n)).as_raw() as u32;
                    if n != 0 {
                        put!(l, ",{state}");
                    } else {
                        put!(l, "{state}");
                    }
                }
                l.raw("]");
            }
            l.h("layout", Kind::PipelineLayout, p.layout.as_raw())
                .h("base", Kind::Pipeline, p.base_pipeline_handle.as_raw())
                .i("baseIndex", i64::from(p.base_pipeline_index));
            l.next(p.p_next);
        }
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn create_graphics_pipelines(
        cache: vk::PipelineCache,
        infos: &[vk::GraphicsPipelineCreateInfo],
        o: *const vk::Pipeline,
        r: vk::Result,
    ) {
        for (k, info) in infos.iter().enumerate() {
            let mut l = Line::new("CreateGraphicsPipeline");
            // SAFETY: the call wrote a pipeline for each info.
            let made = if r == vk::Result::SUCCESS { unsafe { *o.add(k) }.as_raw() } else { 0 };
            l.made("out", Kind::Pipeline, made).h("cache", Kind::Cache, cache.as_raw());
            // SAFETY: a valid structure.
            unsafe { graphics(&mut l, info) };
            result(&mut l, r);
        }
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn state_library(
        made: vk::Pipeline,
        part: vk::GraphicsPipelineLibraryFlagsEXT,
        info: &vk::GraphicsPipelineCreateInfo,
    ) {
        let mut l = Line::new("CreateStateLibrary");
        l.h("out", Kind::Pipeline, made.as_raw()).x("part", u64::from(part.as_raw()));
        // SAFETY: the caller's.
        unsafe { graphics(&mut l, info) };
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn create_compute_pipelines(
        cache: vk::PipelineCache,
        infos: &[vk::ComputePipelineCreateInfo],
        o: *const vk::Pipeline,
        r: vk::Result,
    ) {
        for (k, info) in infos.iter().enumerate() {
            let mut l = Line::new("CreateComputePipeline");
            // SAFETY: the call wrote a pipeline for each info.
            let made = if r == vk::Result::SUCCESS { unsafe { *o.add(k) }.as_raw() } else { 0 };
            l.made("out", Kind::Pipeline, made)
                .h("cache", Kind::Cache, cache.as_raw())
                .x("flags", u64::from(info.flags.as_raw()));
            // SAFETY: a valid structure.
            unsafe {
                stages(&mut l, &info.stage, 1);
                l.h("layout", Kind::PipelineLayout, info.layout.as_raw()).next(info.p_next);
            }
            result(&mut l, r);
        }
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn create_shader_module(i: &vk::ShaderModuleCreateInfo, o: vk::ShaderModule, r: vk::Result) {
        let mut l = Line::new("CreateShaderModule");
        l.made("out", Kind::Module, out(o, r)).u("words", (i.code_size / 4) as u64);
        let code = if i.code_size == 0 {
            &[][..]
        } else {
            // SAFETY: code_size readable bytes.
            unsafe { std::slice::from_raw_parts(i.p_code.cast::<u8>(), i.code_size) }
        };
        put!(l, " fnv={:016x}", super::trace::fnv64(code));
        // SAFETY: the chain of a valid structure.
        unsafe { l.next(i.p_next) };
        result(&mut l, r);
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn create_command_pool(i: &vk::CommandPoolCreateInfo, o: vk::CommandPool, r: vk::Result) {
        let mut l = Line::new("CreateCommandPool");
        l.made("out", Kind::CommandPool, out(o, r))
            .x("flags", u64::from(i.flags.as_raw()))
            .u("family", u64::from(i.queue_family_index));
        // SAFETY: the chain of a valid structure.
        unsafe { l.next(i.p_next) };
        result(&mut l, r);
    }

    #[cold]
    #[inline(never)]
    pub fn reset_command_pool(p: vk::CommandPool, f: vk::CommandPoolResetFlags) {
        Line::new("ResetCommandPool").h("pool", Kind::CommandPool, p.as_raw()).x("flags", u64::from(f.as_raw()));
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn allocate_command_buffers(
        i: &vk::CommandBufferAllocateInfo,
        o: *const vk::CommandBuffer,
        r: vk::Result,
    ) {
        let mut l = Line::new("AllocateCommandBuffers");
        l.h("pool", Kind::CommandPool, i.command_pool.as_raw()).u("level", e(i.level.as_raw())).raw(" out=[");
        for n in 0..i.command_buffer_count as usize {
            if n != 0 {
                l.raw(",");
            }
            if r == vk::Result::SUCCESS {
                // SAFETY: the call wrote the count.
                let name = l.allocated(Kind::Command, unsafe { *o.add(n) }.as_raw());
                l.raw(&name);
            } else {
                l.raw("0");
            }
        }
        l.raw("]");
        result(&mut l, r);
    }

    #[cold]
    #[inline(never)]
    pub fn free_command_buffers(p: vk::CommandPool, commands: &[vk::CommandBuffer]) {
        let mut l = Line::new("FreeCommandBuffers");
        l.h("pool", Kind::CommandPool, p.as_raw());
        // SAFETY: a slice.
        unsafe { l.hs("cmds", Kind::Command, commands.as_ptr(), commands.len() as u32) };
        for command in commands {
            l.forget(Kind::Command, command.as_raw());
        }
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn begin_command_buffer(c: vk::CommandBuffer, i: &vk::CommandBufferBeginInfo) {
        let mut l = Line::new("BeginCommandBuffer");
        l.h("cmd", Kind::Command, c.as_raw()).x("flags", u64::from(i.flags.as_raw()));
        // SAFETY: the chain of a valid structure.
        unsafe { l.next(i.p_next) };
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn create_fence(i: &vk::FenceCreateInfo, o: vk::Fence, r: vk::Result) {
        let mut l = Line::new("CreateFence");
        l.made("out", Kind::Fence, out(o, r)).x("flags", u64::from(i.flags.as_raw()));
        // SAFETY: the chain of a valid structure.
        unsafe { l.next(i.p_next) };
        result(&mut l, r);
    }

    #[cold]
    #[inline(never)]
    pub fn reset_fences(fences: &[vk::Fence]) {
        // SAFETY: a slice.
        unsafe { Line::new("ResetFences").hs("fences", Kind::Fence, fences.as_ptr(), fences.len() as u32) };
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn create_query_pool(i: &vk::QueryPoolCreateInfo, o: vk::QueryPool, r: vk::Result) {
        let mut l = Line::new("CreateQueryPool");
        l.made("out", Kind::QueryPool, out(o, r))
            .x("flags", u64::from(i.flags.as_raw()))
            .u("type", e(i.query_type.as_raw()))
            .u("count", u64::from(i.query_count))
            .x("statistics", u64::from(i.pipeline_statistics.as_raw()));
        // SAFETY: the chain of a valid structure.
        unsafe { l.next(i.p_next) };
        result(&mut l, r);
    }

    #[cold]
    #[inline(never)]
    pub fn cmd_bind_descriptor_sets(
        c: vk::CommandBuffer,
        b: vk::PipelineBindPoint,
        layout: vk::PipelineLayout,
        first: u32,
        sets: &[vk::DescriptorSet],
        dynamic: &[u32],
    ) {
        let mut l = Line::new("CmdBindDescriptorSets");
        l.h("cmd", Kind::Command, c.as_raw())
            .u("bind", e(b.as_raw()))
            .h("layout", Kind::PipelineLayout, layout.as_raw())
            .u("first", u64::from(first));
        // SAFETY: slices.
        unsafe {
            l.hs("sets", Kind::Set, sets.as_ptr(), sets.len() as u32).us(
                "dynamic",
                dynamic.as_ptr(),
                dynamic.len() as u32,
            )
        };
    }

    #[cold]
    #[inline(never)]
    pub fn cmd_pipeline_barrier(
        c: vk::CommandBuffer,
        src: vk::PipelineStageFlags,
        dst: vk::PipelineStageFlags,
        dep: vk::DependencyFlags,
        memory: &[vk::MemoryBarrier],
        buffers: &[vk::BufferMemoryBarrier],
        images: &[vk::ImageMemoryBarrier],
    ) {
        let mut l = Line::new("CmdPipelineBarrier");
        l.h("cmd", Kind::Command, c.as_raw())
            .x("src", u64::from(src.as_raw()))
            .x("dst", u64::from(dst.as_raw()))
            .x("dependency", u64::from(dep.as_raw()));
        l.raw(" memory=[");
        for (n, m) in memory.iter().enumerate() {
            let (s, d) = (m.src_access_mask.as_raw(), m.dst_access_mask.as_raw());
            if n != 0 {
                put!(l, ",({s:x},{d:x})");
            } else {
                put!(l, "({s:x},{d:x})");
            }
        }
        l.raw("] buffers=[");
        for (n, b) in buffers.iter().enumerate() {
            if n != 0 {
                l.raw(",");
            }
            put!(
                l,
                "({:x},{:x},{},{},",
                b.src_access_mask.as_raw(),
                b.dst_access_mask.as_raw(),
                b.src_queue_family_index as i32,
                b.dst_queue_family_index as i32
            );
            l.name(Kind::Buffer, b.buffer.as_raw());
            put!(l, ",{},{:x})", b.offset, b.size);
        }
        l.raw("] images=[");
        for (n, i) in images.iter().enumerate() {
            if n != 0 {
                l.raw(",");
            }
            put!(
                l,
                "({:x},{:x},{},{},{},{},",
                i.src_access_mask.as_raw(),
                i.dst_access_mask.as_raw(),
                i.old_layout.as_raw() as u32,
                i.new_layout.as_raw() as u32,
                i.src_queue_family_index as i32,
                i.dst_queue_family_index as i32
            );
            l.name(Kind::Image, i.image.as_raw());
            l.raw(",").range(&i.subresource_range).raw(")");
        }
        l.raw("]");
    }

    #[cold]
    #[inline(never)]
    pub fn cmd_begin_query(c: vk::CommandBuffer, p: vk::QueryPool, q: u32, f: vk::QueryControlFlags) {
        Line::new("CmdBeginQuery")
            .h("cmd", Kind::Command, c.as_raw())
            .h("pool", Kind::QueryPool, p.as_raw())
            .u("query", u64::from(q))
            .x("flags", u64::from(f.as_raw()));
    }

    #[cold]
    #[inline(never)]
    pub fn cmd_end_query(c: vk::CommandBuffer, p: vk::QueryPool, q: u32) {
        Line::new("CmdEndQuery")
            .h("cmd", Kind::Command, c.as_raw())
            .h("pool", Kind::QueryPool, p.as_raw())
            .u("query", u64::from(q));
    }

    #[cold]
    #[inline(never)]
    pub fn cmd_reset_query_pool(c: vk::CommandBuffer, p: vk::QueryPool, first: u32, n: u32) {
        Line::new("CmdResetQueryPool")
            .h("cmd", Kind::Command, c.as_raw())
            .h("pool", Kind::QueryPool, p.as_raw())
            .u("first", u64::from(first))
            .u("count", u64::from(n));
    }

    #[cold]
    #[inline(never)]
    pub fn cmd_write_timestamp(c: vk::CommandBuffer, st: vk::PipelineStageFlags, p: vk::QueryPool, q: u32) {
        Line::new("CmdWriteTimestamp")
            .h("cmd", Kind::Command, c.as_raw())
            .x("stage", u64::from(st.as_raw()))
            .h("pool", Kind::QueryPool, p.as_raw())
            .u("query", u64::from(q));
    }

    #[cold]
    #[inline(never)]
    pub fn cmd_bind_index_buffer(c: vk::CommandBuffer, b: vk::Buffer, o: u64, t: vk::IndexType) {
        Line::new("CmdBindIndexBuffer")
            .h("cmd", Kind::Command, c.as_raw())
            .h("buffer", Kind::Buffer, b.as_raw())
            .u("offset", o)
            .u("type", e(t.as_raw()));
    }

    #[cold]
    #[inline(never)]
    pub fn cmd_bind_pipeline(c: vk::CommandBuffer, b: vk::PipelineBindPoint, p: vk::Pipeline) {
        Line::new("CmdBindPipeline").h("cmd", Kind::Command, c.as_raw()).u("bind", e(b.as_raw())).h(
            "pipeline",
            Kind::Pipeline,
            p.as_raw(),
        );
    }

    #[cold]
    #[inline(never)]
    pub fn cmd_draw(c: vk::CommandBuffer, v: u32, i: u32, fv: u32, fi: u32) {
        Line::new("CmdDraw")
            .h("cmd", Kind::Command, c.as_raw())
            .u("vertices", u64::from(v))
            .u("instances", u64::from(i))
            .u("firstVertex", u64::from(fv))
            .u("firstInstance", u64::from(fi));
    }

    #[cold]
    #[inline(never)]
    pub fn cmd_draw_indexed(c: vk::CommandBuffer, n: u32, i: u32, fi: u32, vo: i32, fin: u32) {
        Line::new("CmdDrawIndexed")
            .h("cmd", Kind::Command, c.as_raw())
            .u("indices", u64::from(n))
            .u("instances", u64::from(i))
            .u("firstIndex", u64::from(fi))
            .i("vertexOffset", i64::from(vo))
            .u("firstInstance", u64::from(fin));
    }

    #[cold]
    #[inline(never)]
    pub fn cmd_set_depth_bias(c: vk::CommandBuffer, k: f32, clamp: f32, slope: f32) {
        Line::new("CmdSetDepthBias")
            .h("cmd", Kind::Command, c.as_raw())
            .f("constant", k)
            .f("clamp", clamp)
            .f("slope", slope);
    }

    #[cold]
    #[inline(never)]
    pub fn cmd_set_scissor(c: vk::CommandBuffer, first: u32, rects: &[vk::Rect2D]) {
        let mut l = Line::new("CmdSetScissor");
        l.h("cmd", Kind::Command, c.as_raw()).u("first", u64::from(first)).raw(" rects=[");
        for (k, r) in rects.iter().enumerate() {
            if k != 0 {
                l.raw(",");
            }
            l.rect(r);
        }
        l.raw("]");
    }

    #[cold]
    #[inline(never)]
    pub fn cmd_set_viewport(c: vk::CommandBuffer, first: u32, viewports: &[vk::Viewport]) {
        let mut l = Line::new("CmdSetViewport");
        l.h("cmd", Kind::Command, c.as_raw()).u("first", u64::from(first)).raw(" viewports=[");
        for (k, v) in viewports.iter().enumerate() {
            put!(
                l,
                "{}({:08x},{:08x},{:08x},{:08x},{:08x},{:08x})",
                if k != 0 { "," } else { "" },
                bits(v.x),
                bits(v.y),
                bits(v.width),
                bits(v.height),
                bits(v.min_depth),
                bits(v.max_depth)
            );
        }
        l.raw("]");
    }

    #[cold]
    #[inline(never)]
    pub fn cmd_stencil(command: &str, key: &str, c: vk::CommandBuffer, f: vk::StencilFaceFlags, v: u32) {
        Line::new(command).h("cmd", Kind::Command, c.as_raw()).x("faces", u64::from(f.as_raw())).x(key, u64::from(v));
    }

    #[cold]
    #[inline(never)]
    pub fn cmd_blit_image(
        c: vk::CommandBuffer,
        src: vk::Image,
        sl: vk::ImageLayout,
        dst: vk::Image,
        dl: vk::ImageLayout,
        regions: &[vk::ImageBlit],
        f: vk::Filter,
    ) {
        let mut l = Line::new("CmdBlitImage");
        l.h("cmd", Kind::Command, c.as_raw())
            .h("src", Kind::Image, src.as_raw())
            .u("srcLayout", e(sl.as_raw()))
            .h("dst", Kind::Image, dst.as_raw())
            .u("dstLayout", e(dl.as_raw()))
            .raw(" regions=[");
        for (k, r) in regions.iter().enumerate() {
            if k != 0 {
                l.raw(",");
            }
            l.raw("(")
                .layers(&r.src_subresource)
                .raw(",")
                .offset(&r.src_offsets[0])
                .raw(",")
                .offset(&r.src_offsets[1])
                .raw(",")
                .layers(&r.dst_subresource)
                .raw(",")
                .offset(&r.dst_offsets[0])
                .raw(",")
                .offset(&r.dst_offsets[1])
                .raw(")");
        }
        l.raw("]").u("filter", e(f.as_raw()));
    }

    type Region = (vk::ImageSubresourceLayers, vk::Offset3D, vk::ImageSubresourceLayers, vk::Offset3D, vk::Extent3D);

    #[cold]
    #[inline(never)]
    pub fn cmd_copy_image(
        command: &str,
        c: vk::CommandBuffer,
        src: vk::Image,
        sl: vk::ImageLayout,
        dst: vk::Image,
        dl: vk::ImageLayout,
        regions: impl Iterator<Item = Region>,
    ) {
        let mut l = Line::new(command);
        l.h("cmd", Kind::Command, c.as_raw())
            .h("src", Kind::Image, src.as_raw())
            .u("srcLayout", e(sl.as_raw()))
            .h("dst", Kind::Image, dst.as_raw())
            .u("dstLayout", e(dl.as_raw()))
            .raw(" regions=[");
        for (k, (s, so, d, dof, extent)) in regions.enumerate() {
            if k != 0 {
                l.raw(",");
            }
            l.raw("(")
                .layers(&s)
                .raw(",")
                .offset(&so)
                .raw(",")
                .layers(&d)
                .raw(",")
                .offset(&dof)
                .raw(",")
                .extent(&extent)
                .raw(")");
        }
        l.raw("]");
    }

    #[cold]
    #[inline(never)]
    pub fn cmd_clear_color_image(
        c: vk::CommandBuffer,
        i: vk::Image,
        layout: vk::ImageLayout,
        v: &vk::ClearColorValue,
        ranges: &[vk::ImageSubresourceRange],
    ) {
        let mut l = Line::new("CmdClearColorImage");
        l.h("cmd", Kind::Command, c.as_raw()).h("image", Kind::Image, i.as_raw()).u("layout", e(layout.as_raw()));
        // SAFETY: the four words of a clear colour, whichever member was written.
        let words = unsafe { v.uint32 };
        put!(l, " colour={:08x},{:08x},{:08x},{:08x}", words[0], words[1], words[2], words[3]);
        // SAFETY: a slice.
        unsafe { l.ranges("ranges", ranges.as_ptr(), ranges.len() as u32) };
    }

    #[cold]
    #[inline(never)]
    pub fn cmd_clear_depth_stencil_image(
        c: vk::CommandBuffer,
        i: vk::Image,
        layout: vk::ImageLayout,
        v: &vk::ClearDepthStencilValue,
        ranges: &[vk::ImageSubresourceRange],
    ) {
        let mut l = Line::new("CmdClearDepthStencilImage");
        l.h("cmd", Kind::Command, c.as_raw())
            .h("image", Kind::Image, i.as_raw())
            .u("layout", e(layout.as_raw()))
            .f("depth", v.depth)
            .u("stencil", u64::from(v.stencil));
        // SAFETY: a slice.
        unsafe { l.ranges("ranges", ranges.as_ptr(), ranges.len() as u32) };
    }

    #[cold]
    #[inline(never)]
    pub fn cmd_copy_buffer_image(
        to_image: bool,
        c: vk::CommandBuffer,
        b: vk::Buffer,
        i: vk::Image,
        layout: vk::ImageLayout,
        regions: &[vk::BufferImageCopy],
    ) {
        let mut l;
        if to_image {
            l = Line::new("CmdCopyBufferToImage");
            l.h("cmd", Kind::Command, c.as_raw())
                .h("buffer", Kind::Buffer, b.as_raw())
                .h("image", Kind::Image, i.as_raw())
                .u("layout", e(layout.as_raw()));
        } else {
            l = Line::new("CmdCopyImageToBuffer");
            l.h("cmd", Kind::Command, c.as_raw())
                .h("image", Kind::Image, i.as_raw())
                .u("layout", e(layout.as_raw()))
                .h("buffer", Kind::Buffer, b.as_raw());
        }
        // SAFETY: a slice.
        unsafe { l.copies("regions", regions.as_ptr(), regions.len() as u32) };
    }

    #[cold]
    #[inline(never)]
    pub fn cmd_dispatch(c: vk::CommandBuffer, x: u32, y: u32, z: u32) {
        Line::new("CmdDispatch")
            .h("cmd", Kind::Command, c.as_raw())
            .u("x", u64::from(x))
            .u("y", u64::from(y))
            .u("z", u64::from(z));
    }

    #[cold]
    #[inline(never)]
    pub fn cmd_update_buffer(c: vk::CommandBuffer, b: vk::Buffer, o: u64, data: &[u8]) {
        let mut l = Line::new("CmdUpdateBuffer");
        l.h("cmd", Kind::Command, c.as_raw())
            .h("buffer", Kind::Buffer, b.as_raw())
            .u("offset", o)
            .u("size", data.len() as u64)
            .raw(" data=");
        for word in data.chunks_exact(4) {
            put!(l, "{:08x}", u32::from_ne_bytes([word[0], word[1], word[2], word[3]]));
        }
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn cmd_begin_rendering(c: vk::CommandBuffer, i: &vk::RenderingInfo) {
        let mut l = Line::new("CmdBeginRendering");
        l.h("cmd", Kind::Command, c.as_raw()).x("flags", u64::from(i.flags.as_raw())).key("area").rect(&i.render_area);
        l.u("layers", u64::from(i.layer_count)).u("viewMask", u64::from(i.view_mask)).raw(" colour=[");
        // SAFETY: the structure's arrays and pointers are valid (the caller's).
        unsafe {
            for n in 0..i.color_attachment_count as usize {
                if n != 0 {
                    l.raw(",");
                }
                l.attachment(None, Some(&*i.p_color_attachments.add(n)));
            }
            l.raw("]")
                .attachment(Some("depth"), i.p_depth_attachment.as_ref())
                .attachment(Some("stencil"), i.p_stencil_attachment.as_ref())
                .next(i.p_next);
        }
    }

    #[cold]
    #[inline(never)]
    pub fn cmd_value(command: &str, key: &str, hex: bool, c: vk::CommandBuffer, value: u64) {
        let mut l = Line::new(command);
        l.h("cmd", Kind::Command, c.as_raw());
        if hex {
            l.x(key, value);
        } else {
            l.u(key, value);
        }
    }

    #[cold]
    #[inline(never)]
    pub fn cmd_set_stencil_op(
        c: vk::CommandBuffer,
        f: vk::StencilFaceFlags,
        fail: vk::StencilOp,
        pass: vk::StencilOp,
        depth_fail: vk::StencilOp,
        compare: vk::CompareOp,
    ) {
        Line::new("CmdSetStencilOp")
            .h("cmd", Kind::Command, c.as_raw())
            .x("faces", u64::from(f.as_raw()))
            .u("fail", e(fail.as_raw()))
            .u("pass", e(pass.as_raw()))
            .u("depthFail", e(depth_fail.as_raw()))
            .u("compare", e(compare.as_raw()));
    }

    #[cold]
    #[inline(never)]
    pub fn cmd_draw_multi(c: vk::CommandBuffer, draws: &[vk::MultiDrawInfoEXT], instances: u32, first_instance: u32) {
        let mut l = Line::new("CmdDrawMulti");
        l.h("cmd", Kind::Command, c.as_raw()).raw(" draws=[");
        for (k, d) in draws.iter().enumerate() {
            if k != 0 {
                put!(l, ",({},{})", d.first_vertex, d.vertex_count);
            } else {
                put!(l, "({},{})", d.first_vertex, d.vertex_count);
            }
        }
        l.raw("]").u("instances", u64::from(instances)).u("firstInstance", u64::from(first_instance));
    }

    #[cold]
    #[inline(never)]
    pub fn cmd_draw_multi_indexed(
        c: vk::CommandBuffer,
        draws: &[vk::MultiDrawIndexedInfoEXT],
        instances: u32,
        first_instance: u32,
        vertex_offset: Option<i32>,
    ) {
        let mut l = Line::new("CmdDrawMultiIndexed");
        l.h("cmd", Kind::Command, c.as_raw()).raw(" draws=[");
        for (k, d) in draws.iter().enumerate() {
            if k != 0 {
                put!(l, ",({},{},{})", d.first_index, d.index_count, d.vertex_offset);
            } else {
                put!(l, "({},{},{})", d.first_index, d.index_count, d.vertex_offset);
            }
        }
        l.raw("]").u("instances", u64::from(instances)).u("firstInstance", u64::from(first_instance));
        match vertex_offset {
            Some(offset) => {
                l.i("vertexOffset", i64::from(offset));
            }
            None => {
                l.raw(" vertexOffset=-");
            }
        }
    }
}

// tools/gpu_diff.py --rig device_trace (and the recorder rig's Context): a Vk on the
// rig's stand-in Vulkan (tools/gpu_diff/device_stubs.cpp, every command a stub that logs
// its arguments), and each of the rig's calls (device_trace.cpp's Call, whose op numbers
// and argument places these follow) made through the Vk method or trace instrument the
// renderer of S10 will use, on structures the rig built.
#[cfg(mw2_gpu_rig)]
pub mod rig {
    use super::*;
    use std::ffi::c_char;
    use std::sync::OnceLock;
    use vk::Handle;

    unsafe extern "system" {
        fn rig_dev_vkGetInstanceProcAddr(instance: vk::Instance, name: *const c_char) -> vk::PFN_vkVoidFunction;
    }
    unsafe extern "C" {
        fn rig_position() -> u64;
    }

    fn instance() -> ash::Instance {
        // SAFETY: the rig's loader, which answers by name and with null for the rest.
        unsafe {
            let entry =
                ash::Entry::from_static_fn(ash::StaticFn { get_instance_proc_addr: rig_dev_vkGetInstanceProcAddr });
            ash::Instance::load(entry.static_fn(), vk::Instance::from_raw(0x10))
        }
    }

    /// A Vk on the stand-ins, made once, the trace off.
    pub fn tables() -> *const Vk {
        static TABLES: OnceLock<Vk> = OnceLock::new();
        // SAFETY: the stand-in device lives as long as the process.
        TABLES.get_or_init(|| unsafe { Vk::new(&instance(), vk::Device::from_raw(0x20), 256) })
    }

    struct Case {
        vk: Vk,
    }
    static CASE: std::sync::Mutex<Option<Case>> = std::sync::Mutex::new(None);

    fn position() -> u64 {
        // SAFETY: the rig's position.
        unsafe { rig_position() }
    }

    /// A case's start: the trace back to a process's start, then started as the
    /// renderer's initialise starts it, and the case's Vk made after it.
    ///
    /// # Safety
    /// `set_layouts` holds `count` handles.
    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_vk_begin(
        device: u64,
        physical: u64,
        queue: u64,
        cache: u64,
        layout: u64,
        set_layouts: *const u64,
        count: u32,
        multi_draw: u32,
    ) {
        super::trace::rig::reset();
        // SAFETY: the caller's.
        let layouts: Vec<vk::DescriptorSetLayout> =
            (0..count as usize).map(|n| vk::DescriptorSetLayout::from_raw(unsafe { *set_layouts.add(n) })).collect();
        super::trace::start(
            vk::Device::from_raw(device),
            vk::PhysicalDevice::from_raw(physical),
            vk::Queue::from_raw(queue),
            vk::PipelineCache::from_raw(cache),
            vk::PipelineLayout::from_raw(layout),
            &layouts,
            position,
        );
        // SAFETY: the stand-in device.
        let vk = unsafe { Vk::new(&instance(), vk::Device::from_raw(device), multi_draw) };
        *CASE.lock().unwrap() = Some(Case { vk });
    }

    /// The forced switches as start read them, and whether the device is lost yet.
    ///
    /// # Safety
    /// `out` has ten places.
    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_vk_forced(out: *mut u64) {
        let f = super::trace::forcing();
        let case = CASE.lock().unwrap();
        let vk = &case.as_ref().unwrap().vk;
        let values = [
            u64::from(f.instance_depth_resolve),
            u64::from(f.resolve_twin),
            u64::from(f.drawn_resolve_sets),
            u64::from(f.resolve_remake),
            u64::from(f.no_multi_draw),
            u64::from(f.lone_uploads),
            u64::from(f.staging_least_kb),
            f.fake_device_lost.to_bits(),
            u64::from(super::trace::deterministic()),
            u64::from(vk.multi_draw()) | u64::from(vk.dynamic_commands()) << 32,
        ];
        // SAFETY: the caller's ten places.
        unsafe { std::ptr::copy_nonoverlapping(values.as_ptr(), out, values.len()) };
    }

    #[unsafe(no_mangle)]
    extern "C" fn mw2_rig_vk_fnv(bytes: *const u8, size: usize, write_combined: bool) -> u64 {
        if write_combined {
            // SAFETY: the rig's buffer.
            unsafe { super::trace::fnv_write_combined(bytes, size) }
        } else {
            // SAFETY: the rig's buffer.
            super::trace::fnv64(unsafe { std::slice::from_raw_parts(bytes, size) })
        }
    }

    #[repr(C)]
    pub struct Call {
        op: u32,
        on_recorder: u32,
        a: [u64; 12],
    }

    unsafe fn slice<'a, T>(at: u64, count: u64) -> &'a [T] {
        if count == 0 {
            &[]
        } else {
            // SAFETY: the rig's array of `count`.
            unsafe { std::slice::from_raw_parts(at as *const T, count as usize) }
        }
    }

    unsafe fn slice_mut<'a, T>(at: u64, count: u64) -> &'a mut [T] {
        if count == 0 {
            &mut []
        } else {
            // SAFETY: the rig's array of `count`.
            unsafe { std::slice::from_raw_parts_mut(at as *mut T, count as usize) }
        }
    }

    unsafe fn text(at: u64) -> String {
        // SAFETY: the rig's C string.
        unsafe { std::ffi::CStr::from_ptr(at as *const c_char) }.to_string_lossy().into_owned()
    }

    fn kind(n: u64) -> super::trace::Kind {
        use super::trace::Kind::*;
        [
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
        ][n as usize]
    }

    /// One of the rig's calls, on this thread or, as the rig asks, on a thread of its own
    /// that says it is the recorder: what it returns (a result, a handle, a flag), and
    /// what it wrote into `out`.
    ///
    /// # Safety
    /// `call` is the rig's, its pointers to structures and arrays it keeps for the call.
    #[unsafe(no_mangle)]
    unsafe extern "C" fn mw2_rig_vk_call(call: *const Call, out: *mut u64) -> u64 {
        // SAFETY: the caller's.
        let call = unsafe { &*call };
        if call.on_recorder != 0 {
            let (c, o) = (call as *const Call as usize, out as usize);
            return std::thread::spawn(move || {
                super::trace::recorder_thread();
                // SAFETY: the caller waits for this thread.
                unsafe { made(&*(c as *const Call), o as *mut u64) }
            })
            .join()
            .unwrap();
        }
        // SAFETY: the caller's.
        unsafe { made(call, out) }
    }

    #[allow(clippy::too_many_lines)]
    unsafe fn made(call: &Call, out: *mut u64) -> u64 {
        if call.op == 89 {
            super::trace::finish();
            return 0;
        }
        let case = CASE.lock().unwrap();
        let vk = &case.as_ref().unwrap().vk;
        let a = &call.a;
        let cmd = vk::CommandBuffer::from_raw(a[0]);
        macro_rules! h {
            ($t:ty, $i:expr) => {
                <$t>::from_raw(a[$i])
            };
        }
        macro_rules! info {
            ($t:ty, $i:expr) => {
                &*(a[$i] as *const $t)
            };
        }
        macro_rules! create {
            ($method:ident, $info:ty, $t:ty) => {{
                let mut made = <$t>::null();
                let r = vk.$method(info!($info, 0), &mut made);
                *out = made.as_raw();
                r.as_raw() as u64
            }};
        }
        let f = |i: usize| f32::from_bits(a[i] as u32);
        // SAFETY: the rig's structures, arrays and handles for each op (device_trace.cpp).
        unsafe {
            match call.op {
                0 => create!(create_image, vk::ImageCreateInfo, vk::Image),
                1 => {
                    vk.destroy_image(h!(vk::Image, 0));
                    0
                }
                2 => create!(create_image_view, vk::ImageViewCreateInfo, vk::ImageView),
                3 => {
                    vk.destroy_image_view(h!(vk::ImageView, 0));
                    0
                }
                4 => create!(create_buffer, vk::BufferCreateInfo, vk::Buffer),
                5 => {
                    vk.destroy_buffer(h!(vk::Buffer, 0));
                    0
                }
                6 => create!(allocate_memory, vk::MemoryAllocateInfo, vk::DeviceMemory),
                7 => {
                    vk.free_memory(h!(vk::DeviceMemory, 0));
                    0
                }
                8 => vk.bind_image_memory(h!(vk::Image, 0), h!(vk::DeviceMemory, 1), a[2]).as_raw() as u64,
                9 => vk.bind_buffer_memory(h!(vk::Buffer, 0), h!(vk::DeviceMemory, 1), a[2]).as_raw() as u64,
                10 => create!(create_sampler, vk::SamplerCreateInfo, vk::Sampler),
                11 => {
                    vk.destroy_sampler(h!(vk::Sampler, 0));
                    0
                }
                12 => create!(create_descriptor_pool, vk::DescriptorPoolCreateInfo, vk::DescriptorPool),
                13 => {
                    vk.destroy_descriptor_pool(h!(vk::DescriptorPool, 0));
                    0
                }
                14 => {
                    let i = info!(vk::DescriptorSetAllocateInfo, 0);
                    let sets = slice_mut::<vk::DescriptorSet>(out as u64, u64::from(i.descriptor_set_count));
                    vk.allocate_descriptor_sets(i, sets).as_raw() as u64
                }
                15 => vk.free_descriptor_sets(h!(vk::DescriptorPool, 0), slice(a[2], a[1])).as_raw() as u64,
                16 => {
                    vk.update_descriptor_sets(slice(a[1], a[0]), slice(a[3], a[2]));
                    0
                }
                17 => create!(create_descriptor_set_layout, vk::DescriptorSetLayoutCreateInfo, vk::DescriptorSetLayout),
                18 => {
                    vk.destroy_descriptor_set_layout(h!(vk::DescriptorSetLayout, 0));
                    0
                }
                19 => create!(create_pipeline_layout, vk::PipelineLayoutCreateInfo, vk::PipelineLayout),
                20 => {
                    vk.destroy_pipeline_layout(h!(vk::PipelineLayout, 0));
                    0
                }
                21 => vk
                    .create_graphics_pipelines(h!(vk::PipelineCache, 0), slice(a[2], a[1]), slice_mut(out as u64, a[1]))
                    .as_raw() as u64,
                22 => vk
                    .create_compute_pipelines(h!(vk::PipelineCache, 0), slice(a[2], a[1]), slice_mut(out as u64, a[1]))
                    .as_raw() as u64,
                23 => {
                    vk.destroy_pipeline(h!(vk::Pipeline, 0));
                    0
                }
                24 => create!(create_shader_module, vk::ShaderModuleCreateInfo, vk::ShaderModule),
                25 => {
                    vk.destroy_shader_module(h!(vk::ShaderModule, 0));
                    0
                }
                26 => create!(create_command_pool, vk::CommandPoolCreateInfo, vk::CommandPool),
                27 => {
                    vk.destroy_command_pool(h!(vk::CommandPool, 0));
                    0
                }
                28 => vk
                    .reset_command_pool(h!(vk::CommandPool, 0), vk::CommandPoolResetFlags::from_raw(a[1] as u32))
                    .as_raw() as u64,
                29 => {
                    let i = info!(vk::CommandBufferAllocateInfo, 0);
                    let commands = slice_mut::<vk::CommandBuffer>(out as u64, u64::from(i.command_buffer_count));
                    vk.allocate_command_buffers(i, commands).as_raw() as u64
                }
                30 => {
                    vk.free_command_buffers(h!(vk::CommandPool, 0), slice(a[2], a[1]));
                    0
                }
                31 => vk.begin_command_buffer(cmd, info!(vk::CommandBufferBeginInfo, 1)).as_raw() as u64,
                32 => vk.end_command_buffer(cmd).as_raw() as u64,
                33 => create!(create_fence, vk::FenceCreateInfo, vk::Fence),
                34 => {
                    vk.destroy_fence(h!(vk::Fence, 0));
                    0
                }
                35 => vk.reset_fences(slice(a[1], a[0])).as_raw() as u64,
                36 => create!(create_query_pool, vk::QueryPoolCreateInfo, vk::QueryPool),
                37 => {
                    vk.destroy_query_pool(h!(vk::QueryPool, 0));
                    0
                }
                38 => vk.queue_submit(h!(vk::Queue, 0), slice(a[2], a[1]), h!(vk::Fence, 3)).as_raw() as u64,
                39 => {
                    vk.cmd_bind_descriptor_sets(
                        cmd,
                        vk::PipelineBindPoint::from_raw(a[1] as i32),
                        h!(vk::PipelineLayout, 2),
                        a[3] as u32,
                        slice(a[5], a[4]),
                        slice(a[7], a[6]),
                    );
                    0
                }
                40 | 75 => {
                    let (src, dst) =
                        (vk::PipelineStageFlags::from_raw(a[1] as u32), vk::PipelineStageFlags::from_raw(a[2] as u32));
                    let dep = vk::DependencyFlags::from_raw(a[3] as u32);
                    if call.op == 40 {
                        vk.cmd_pipeline_barrier(
                            cmd,
                            src,
                            dst,
                            dep,
                            slice(a[5], a[4]),
                            slice(a[7], a[6]),
                            slice(a[9], a[8]),
                        );
                    } else {
                        vk.util_barrier(cmd, src, dst, dep, slice(a[5], a[4]), slice(a[7], a[6]), slice(a[9], a[8]));
                    }
                    0
                }
                41 => {
                    vk.cmd_begin_query(
                        cmd,
                        h!(vk::QueryPool, 1),
                        a[2] as u32,
                        vk::QueryControlFlags::from_raw(a[3] as u32),
                    );
                    0
                }
                42 => {
                    vk.cmd_end_query(cmd, h!(vk::QueryPool, 1), a[2] as u32);
                    0
                }
                43 => {
                    vk.cmd_reset_query_pool(cmd, h!(vk::QueryPool, 1), a[2] as u32, a[3] as u32);
                    0
                }
                44 => {
                    vk.cmd_write_timestamp(
                        cmd,
                        vk::PipelineStageFlags::from_raw(a[1] as u32),
                        h!(vk::QueryPool, 2),
                        a[3] as u32,
                    );
                    0
                }
                45 => {
                    vk.cmd_bind_index_buffer(cmd, h!(vk::Buffer, 1), a[2], vk::IndexType::from_raw(a[3] as i32));
                    0
                }
                46 => {
                    vk.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::from_raw(a[1] as i32), h!(vk::Pipeline, 2));
                    0
                }
                47 => {
                    vk.cmd_draw(cmd, a[1] as u32, a[2] as u32, a[3] as u32, a[4] as u32);
                    0
                }
                48 => {
                    vk.cmd_draw_indexed(cmd, a[1] as u32, a[2] as u32, a[3] as u32, a[4] as i32, a[5] as u32);
                    0
                }
                49 => {
                    vk.cmd_set_depth_bias(cmd, f(1), f(2), f(3));
                    0
                }
                50 => {
                    vk.cmd_set_scissor(cmd, a[1] as u32, slice(a[3], a[2]));
                    0
                }
                51 => {
                    vk.cmd_set_viewport(cmd, a[1] as u32, slice(a[3], a[2]));
                    0
                }
                52 => {
                    vk.cmd_set_stencil_compare_mask(cmd, vk::StencilFaceFlags::from_raw(a[1] as u32), a[2] as u32);
                    0
                }
                53 => {
                    vk.cmd_set_stencil_write_mask(cmd, vk::StencilFaceFlags::from_raw(a[1] as u32), a[2] as u32);
                    0
                }
                54 => {
                    vk.cmd_set_stencil_reference(cmd, vk::StencilFaceFlags::from_raw(a[1] as u32), a[2] as u32);
                    0
                }
                55 => {
                    vk.cmd_blit_image(
                        cmd,
                        h!(vk::Image, 1),
                        vk::ImageLayout::from_raw(a[2] as i32),
                        h!(vk::Image, 3),
                        vk::ImageLayout::from_raw(a[4] as i32),
                        slice(a[6], a[5]),
                        vk::Filter::from_raw(a[7] as i32),
                    );
                    0
                }
                56 => {
                    vk.cmd_copy_image(
                        cmd,
                        h!(vk::Image, 1),
                        vk::ImageLayout::from_raw(a[2] as i32),
                        h!(vk::Image, 3),
                        vk::ImageLayout::from_raw(a[4] as i32),
                        slice(a[6], a[5]),
                    );
                    0
                }
                57 => {
                    vk.cmd_resolve_image(
                        cmd,
                        h!(vk::Image, 1),
                        vk::ImageLayout::from_raw(a[2] as i32),
                        h!(vk::Image, 3),
                        vk::ImageLayout::from_raw(a[4] as i32),
                        slice(a[6], a[5]),
                    );
                    0
                }
                58 => {
                    vk.cmd_clear_color_image(
                        cmd,
                        h!(vk::Image, 1),
                        vk::ImageLayout::from_raw(a[2] as i32),
                        info!(vk::ClearColorValue, 3),
                        slice(a[5], a[4]),
                    );
                    0
                }
                59 => {
                    vk.cmd_clear_depth_stencil_image(
                        cmd,
                        h!(vk::Image, 1),
                        vk::ImageLayout::from_raw(a[2] as i32),
                        info!(vk::ClearDepthStencilValue, 3),
                        slice(a[5], a[4]),
                    );
                    0
                }
                60 => {
                    vk.cmd_copy_buffer_to_image(
                        cmd,
                        h!(vk::Buffer, 1),
                        h!(vk::Image, 2),
                        vk::ImageLayout::from_raw(a[3] as i32),
                        slice(a[5], a[4]),
                    );
                    0
                }
                61 => {
                    vk.cmd_copy_image_to_buffer(
                        cmd,
                        h!(vk::Image, 1),
                        vk::ImageLayout::from_raw(a[2] as i32),
                        h!(vk::Buffer, 3),
                        slice(a[5], a[4]),
                    );
                    0
                }
                62 => {
                    vk.cmd_dispatch(cmd, a[1] as u32, a[2] as u32, a[3] as u32);
                    0
                }
                63 => {
                    vk.cmd_update_buffer(cmd, h!(vk::Buffer, 1), a[2], slice(a[4], a[3]));
                    0
                }
                64 => {
                    vk.cmd_begin_rendering(cmd, info!(vk::RenderingInfo, 1));
                    0
                }
                65 => {
                    vk.cmd_end_rendering(cmd);
                    0
                }
                66 => {
                    vk.cmd_set_cull_mode(cmd, vk::CullModeFlags::from_raw(a[1] as u32));
                    0
                }
                67 => {
                    vk.cmd_set_front_face(cmd, vk::FrontFace::from_raw(a[1] as i32));
                    0
                }
                68 => {
                    vk.cmd_set_depth_test_enable(cmd, a[1] as u32);
                    0
                }
                69 => {
                    vk.cmd_set_depth_write_enable(cmd, a[1] as u32);
                    0
                }
                70 => {
                    vk.cmd_set_depth_compare_op(cmd, vk::CompareOp::from_raw(a[1] as i32));
                    0
                }
                71 => {
                    vk.cmd_set_stencil_test_enable(cmd, a[1] as u32);
                    0
                }
                72 => {
                    vk.cmd_set_stencil_op(
                        cmd,
                        vk::StencilFaceFlags::from_raw(a[1] as u32),
                        vk::StencilOp::from_raw(a[2] as i32),
                        vk::StencilOp::from_raw(a[3] as i32),
                        vk::StencilOp::from_raw(a[4] as i32),
                        vk::CompareOp::from_raw(a[5] as i32),
                    );
                    0
                }
                73 => {
                    vk.cmd_draw_multi(cmd, slice(a[2], a[1]), a[3] as u32, a[4] as u32);
                    0
                }
                74 => {
                    let offset = (a[5] as *const i32).as_ref();
                    vk.cmd_draw_multi_indexed(cmd, slice(a[2], a[1]), a[3] as u32, a[4] as u32, offset);
                    0
                }
                76 => {
                    super::super::util::renderer_barrier(
                        vk,
                        cmd,
                        h!(vk::Image, 1),
                        vk::ImageAspectFlags::from_raw(a[2] as u32),
                        vk::ImageLayout::from_raw(a[3] as i32),
                        vk::ImageLayout::from_raw(a[4] as i32),
                        vk::AccessFlags::from_raw(a[5] as u32),
                        vk::AccessFlags::from_raw(a[6] as u32),
                        vk::PipelineStageFlags::from_raw(a[7] as u32),
                        vk::PipelineStageFlags::from_raw(a[8] as u32),
                    );
                    0
                }
                77 => {
                    super::trace::register(kind(a[0]), a[1], &text(a[2]));
                    0
                }
                78 => {
                    super::trace::name_next(kind(a[0]), &text(a[1]));
                    0
                }
                79 => {
                    super::trace::note(&text(a[0]));
                    0
                }
                80 => {
                    super::trace::frame(a[0]);
                    0
                }
                81 => {
                    super::trace::present(h!(vk::Image, 0), a[1]);
                    0
                }
                82 => {
                    super::trace::submission(h!(vk::Queue, 0), info!(vk::SubmitInfo, 1), h!(vk::Fence, 2));
                    0
                }
                83 => {
                    super::state_library(
                        h!(vk::Pipeline, 0),
                        vk::GraphicsPipelineLibraryFlagsEXT::from_raw(a[1] as u32),
                        info!(vk::GraphicsPipelineCreateInfo, 2),
                        a[3] as *const c_void,
                    );
                    0
                }
                84 => {
                    super::trace::shutdown_begins();
                    0
                }
                85 => {
                    super::trace::recorder_started();
                    0
                }
                86 => {
                    super::trace::recorder_made(a[0]);
                    0
                }
                87 => {
                    super::trace::recorder_stopped();
                    0
                }
                88 => u64::from(super::trace::wanted()),
                // Untraced: what they hand back is compared.
                90 => vk.device_wait_idle().as_raw() as u64,
                91 => {
                    let r = vk.get_buffer_memory_requirements(h!(vk::Buffer, 0));
                    *out = r.size;
                    *out.add(1) = r.alignment;
                    u64::from(r.memory_type_bits)
                }
                92 => {
                    let r = vk.get_image_memory_requirements(h!(vk::Image, 0));
                    *out = r.size;
                    *out.add(1) = r.alignment;
                    u64::from(r.memory_type_bits)
                }
                93 => vk.get_fence_status(h!(vk::Fence, 0)).as_raw() as u64,
                94 => vk.wait_for_fences(slice(a[1], a[0]), a[2] != 0, a[3]).as_raw() as u64,
                95 => vk
                    .get_query_pool_results(
                        h!(vk::QueryPool, 0),
                        a[1] as u32,
                        a[2] as u32,
                        slice_mut(out as u64, a[3]),
                        a[4],
                        vk::QueryResultFlags::from_raw(a[5] as u32),
                    )
                    .as_raw() as u64,
                96 => {
                    let mut mapped = std::ptr::null_mut();
                    let r = vk.map_memory(h!(vk::DeviceMemory, 0), a[1], a[2], &mut mapped);
                    *out = mapped as u64;
                    r.as_raw() as u64
                }
                97 => {
                    vk.unmap_memory(h!(vk::DeviceMemory, 0));
                    0
                }
                98 => {
                    let p = vk.get_physical_device_format_properties(
                        h!(vk::PhysicalDevice, 0),
                        vk::Format::from_raw(a[1] as i32),
                    );
                    *out = u64::from(p.linear_tiling_features.as_raw());
                    *out.add(1) = u64::from(p.optimal_tiling_features.as_raw());
                    u64::from(p.buffer_features.as_raw())
                }
                99 => {
                    let p = vk.get_physical_device_memory_properties(h!(vk::PhysicalDevice, 0));
                    std::ptr::copy_nonoverlapping(
                        (&p as *const vk::PhysicalDeviceMemoryProperties).cast::<u8>(),
                        out.cast::<u8>(),
                        size_of::<vk::PhysicalDeviceMemoryProperties>(),
                    );
                    0
                }
                100 => {
                    let p = vk.get_physical_device_properties(h!(vk::PhysicalDevice, 0));
                    std::ptr::copy_nonoverlapping(
                        (&p as *const vk::PhysicalDeviceProperties).cast::<u8>(),
                        out.cast::<u8>(),
                        size_of::<vk::PhysicalDeviceProperties>(),
                    );
                    0
                }
                101 => {
                    vk.get_physical_device_properties2(
                        h!(vk::PhysicalDevice, 0),
                        &mut *(a[1] as *mut vk::PhysicalDeviceProperties2),
                    );
                    0
                }
                102 => {
                    let n = vk.get_physical_device_queue_family_properties(h!(vk::PhysicalDevice, 0), None);
                    let m = vk.get_physical_device_queue_family_properties(
                        h!(vk::PhysicalDevice, 0),
                        Some(slice_mut(out as u64, u64::from(n.min(4)))),
                    );
                    u64::from(n) << 32 | u64::from(m)
                }
                _ => u64::MAX,
            }
        }
    }
}
