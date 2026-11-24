// The Vulkan device the renderer draws on, the pipeline layout every translated shader
// is built against, and the pipeline cache. The translator emits a fixed interface
// (shader/bindings.rs), so one layout serves every shader and no reflection is needed at
// bind time. Pipeline *state* is absent: it is decided by the register file and the
// render target, in the renderer.
//
// One device a process: state set by initialise and create_device and read from any
// thread after.
//
// Unsafe by nature: every call is Vulkan's on handles the caller says are live.
#![allow(clippy::missing_safety_doc)]

use crate::host::{files, flag, log_line, log_parts, stdio, text};
use crate::shader::bindings;
use ash::vk;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// The Vulkan loader, linked by name (`vulkan`, `vulkan-1`).
pub fn entry() -> &'static ash::Entry {
    static ENTRY: LazyLock<ash::Entry> = LazyLock::new(ash::Entry::linked);
    &ENTRY
}

/// The instance commands of a live instance.
///
/// # Safety
/// `instance` is a live VkInstance, which outlives every use of the table.
pub unsafe fn instance_table(instance: vk::Instance) -> ash::Instance {
    // SAFETY: the caller's.
    unsafe { ash::Instance::load(entry().static_fn(), instance) }
}

struct State {
    // Owned only when this file brought the device up itself; when the presenter
    // supplies one, teardown leaves it alone.
    owns_device: bool,
    instance: vk::Instance,
    instance_fn: Option<ash::Instance>,
    physical: vk::PhysicalDevice,
    device: vk::Device,
    device_fn: Option<Arc<ash::Device>>,
    queue: vk::Queue,
    family: u32,
    precise_queries: bool,
    max_anisotropy: f32,
    non_seamless_cubes: bool,
    multi_draw: u32, // the most draws one vkCmdDrawMulti*EXT may make
    samplers: u32,   // the most samplers the texture cache may make
    pipeline_libraries: bool,
    fast_linking: bool,
    creation_feedback: bool,
    cache_loaded: bool,
    name: Option<CString>, // None: "none", before a device is up

    sets: [vk::DescriptorSetLayout; 3],
    layout: vk::PipelineLayout,
    cache: vk::PipelineCache,
}

static G: RwLock<State> = RwLock::new(State {
    owns_device: false,
    instance: vk::Instance::null(),
    instance_fn: None,
    physical: vk::PhysicalDevice::null(),
    device: vk::Device::null(),
    device_fn: None,
    queue: vk::Queue::null(),
    family: 0,
    precise_queries: false,
    max_anisotropy: 1.0,
    non_seamless_cubes: false,
    multi_draw: 0,
    samplers: 0,
    pipeline_libraries: false,
    fast_linking: false,
    creation_feedback: false,
    cache_loaded: false,
    name: None,
    sets: [vk::DescriptorSetLayout::null(); 3],
    layout: vk::PipelineLayout::null(),
    cache: vk::PipelineCache::null(),
});

fn read() -> RwLockReadGuard<'static, State> {
    G.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write() -> RwLockWriteGuard<'static, State> {
    G.write().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// What the device the next headless initialise brings up is, by device_id; empty for
// choose_device's own choice. And the cache file set_cache_path names.
static WANTED_DEVICE: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static CACHE_PATH: Mutex<Vec<u8>> = Mutex::new(Vec::new());

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn explain(r: vk::Result) -> &'static str {
    match r {
        vk::Result::SUCCESS => "ok",
        vk::Result::ERROR_OUT_OF_HOST_MEMORY => "out of host memory",
        vk::Result::ERROR_OUT_OF_DEVICE_MEMORY => "out of device memory",
        vk::Result::ERROR_INVALID_SHADER_NV => "driver rejected the shader",
        vk::Result::ERROR_INITIALIZATION_FAILED => "initialisation failed",
        vk::Result::ERROR_EXTENSION_NOT_PRESENT => "extension not present",
        vk::Result::ERROR_FEATURE_NOT_PRESENT => "feature not present",
        vk::Result::ERROR_INCOMPATIBLE_DRIVER => "incompatible driver",
        _ => "failed",
    }
}

static DEVICE_LOST: RwLock<Option<fn(&CStr)>> = RwLock::new(None);

/// What ending the run means is the program's business, not this file's: a lost device
/// calls `handler`. Without one it is only reported.
pub fn on_device_lost(handler: fn(&CStr)) {
    *DEVICE_LOST.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(handler);
}

/// A VkResult that is not VK_SUCCESS. VK_ERROR_DEVICE_LOST also ends the run, once,
/// saying so: after the driver resets the GPU nothing submitted again will finish, and a
/// window left waiting on it stops answering its close button. `at` names what was
/// being waited for or submitted.
pub fn failed(result: vk::Result, at: &CStr) -> bool {
    if result == vk::Result::SUCCESS {
        return false;
    }
    if result == vk::Result::ERROR_DEVICE_LOST {
        static REPORTED: AtomicBool = AtomicBool::new(false);
        if !REPORTED.swap(true, Ordering::SeqCst) {
            log_parts(&[
                b"[E] vulkan: the device was lost at ",
                at.to_bytes(),
                b" -- the driver reset the GPU under this run (after a GPU page fault or a hang; \
                  `journalctl -k | grep amdgpu` has the fault), and nothing submitted from here on will finish. \
                  Ending the run.",
            ]);
            let handler = *DEVICE_LOST.read().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(handler) = handler {
                handler(c"the Vulkan device was lost");
            }
        }
    }
    true
}

/// Its first queue family that can draw; u32::MAX if none.
///
/// # Safety
/// `physical` is a physical device of `instance`.
pub unsafe fn graphics_family(instance: &ash::Instance, physical: vk::PhysicalDevice) -> u32 {
    let get = instance.fp_v1_0().get_physical_device_queue_family_properties;
    let mut families = 0u32;
    // SAFETY: the caller's; a count, then that many.
    unsafe { get(physical, &mut families, std::ptr::null_mut()) };
    let mut props = vec![vk::QueueFamilyProperties::default(); families as usize];
    unsafe { get(physical, &mut families, props.as_mut_ptr()) };
    for i in 0..families {
        if props[i as usize].queue_flags.contains(vk::QueueFlags::GRAPHICS) {
            return i;
        }
    }
    u32::MAX
}

/// Whether the physical device offers a device extension.
///
/// # Safety
/// `physical` is a physical device of `instance`.
pub unsafe fn device_has_extension(instance: &ash::Instance, physical: vk::PhysicalDevice, name: &CStr) -> bool {
    let get = instance.fp_v1_0().enumerate_device_extension_properties;
    let mut count = 0u32;
    // SAFETY: the caller's; a count, then that many.
    let _ = unsafe { get(physical, std::ptr::null(), &mut count, std::ptr::null_mut()) };
    let mut offered = vec![vk::ExtensionProperties::default(); count as usize];
    let _ = unsafe { get(physical, std::ptr::null(), &mut count, offered.as_mut_ptr()) };
    offered.iter().any(|e| unsafe { CStr::from_ptr(e.extension_name.as_ptr()) } == name)
}

/// Creates the device the runtime draws on, with the features and extensions the
/// renderer depends on, plus the caller's own (the swapchain, for the window). Both the
/// presenter's device and the headless one come from here. `features` is a pNext chain
/// of feature structures the caller's extensions need, or null.
///
/// # Safety
/// `physical` is a physical device of `instance`, every extension name a C string that
/// lives through the call, and `features` null or a chain that does.
pub unsafe fn create_device(
    instance: &ash::Instance,
    physical: vk::PhysicalDevice,
    family: u32,
    extra_extensions: &[*const c_char],
    features: *mut c_void,
) -> Option<(vk::Device, vk::Queue)> {
    let mut g = write();
    // SAFETY: the caller's.
    unsafe { create_device_in(&mut g, instance, physical, family, extra_extensions, features) }
}

unsafe fn create_device_in(
    g: &mut State,
    instance: &ash::Instance,
    physical: vk::PhysicalDevice,
    family: u32,
    extra_extensions: &[*const c_char],
    features: *mut c_void,
) -> Option<(vk::Device, vk::Queue)> {
    let v10 = instance.fp_v1_0();
    let v11 = instance.fp_v1_1();
    let has = |name: &CStr| unsafe { device_has_extension(instance, physical, name) };
    let features2 = |chain: *mut c_void| {
        let mut query = vk::PhysicalDeviceFeatures2 { p_next: chain, ..Default::default() };
        // SAFETY: a chain of structures on the caller's stack.
        unsafe { (v11.get_physical_device_features2)(physical, &mut query) };
    };
    let properties2 = |chain: *mut c_void| {
        let mut query = vk::PhysicalDeviceProperties2 { p_next: chain, ..Default::default() };
        // SAFETY: as above.
        unsafe { (v11.get_physical_device_properties2)(physical, &mut query) };
    };

    let priority = 1.0f32;
    let queue = vk::DeviceQueueCreateInfo {
        queue_family_index: family,
        queue_count: 1,
        p_queue_priorities: &priority,
        ..Default::default()
    };

    let mut supported = vk::PhysicalDeviceFeatures::default();
    // SAFETY: the caller's physical device.
    unsafe { (v10.get_physical_device_features)(physical, &mut supported) };
    let mut enabled = vk::PhysicalDeviceFeatures {
        // Robust buffer access, because the vertex fetch path reads the frame arena at
        // indices the guest's own data computes. One index past the end is, without it, a
        // GPU page fault: the driver retries it until the queue stalls, and enough of them
        // hung the whole machine. With it the read returns zero or something in bounds,
        // and the draw is merely wrong. Xenia asks for the same thing for the same reason.
        robust_buffer_access: supported.robust_buffer_access,
        // The title's occlusion queries are a sample count it compares against a
        // threshold, and without this a query is only allowed to answer "some".
        occlusion_query_precise: supported.occlusion_query_precise,
        // The title asks for anisotropic filtering on nearly every world texture (2:1,
        // once 4:1), and a sampler cannot give it unless the device enables it.
        sampler_anisotropy: supported.sampler_anisotropy,
        ..Default::default()
    };
    if supported.robust_buffer_access == 0 {
        log_line!(
            b"[w] ",
            "vulkan: the device offers no robust buffer access; an out-of-range vertex fetch will fault"
        );
    }
    g.precise_queries = supported.occlusion_query_precise != 0;
    {
        let mut properties = vk::PhysicalDeviceProperties::default();
        // SAFETY: the caller's physical device.
        unsafe { (v10.get_physical_device_properties)(physical, &mut properties) };
        if supported.sampler_anisotropy != 0 {
            g.max_anisotropy = properties.limits.max_sampler_anisotropy;
        }
        // NVIDIA allows 4000 samplers in all, fewer than the set's slots; the backdrop
        // makes one of its own.
        g.samplers = bindings::SAMPLER_COUNT.min(properties.limits.max_sampler_allocation_count.wrapping_sub(16));
    }

    // The image format list lets a resolve destination carry an sRGB view beside its own
    // without the driver giving up compression; naming it in an image's pNext without
    // enabling it is invalid.
    let mut extensions: Vec<*const c_char> = extra_extensions.to_vec();
    if has(ash::khr::image_format_list::NAME) {
        extensions.push(ash::khr::image_format_list::NAME.as_ptr());
    }
    // The driver's word on whether a shader came out of the pipeline cache: the bug
    // report counts the ones the launcher's compile pass had not put there.
    g.creation_feedback = has(ash::ext::pipeline_creation_feedback::NAME);
    if g.creation_feedback {
        extensions.push(ash::ext::pipeline_creation_feedback::NAME.as_ptr());
    }

    // Direct3D 9 filters each cube face on its own, clamped at its edges; a Vulkan cube
    // blends across them unless told not to. Xenia asks for the same thing.
    let mut non_seamless = vk::PhysicalDeviceNonSeamlessCubeMapFeaturesEXT::default();
    if has(ash::ext::non_seamless_cube_map::NAME) {
        features2((&raw mut non_seamless).cast());
        if non_seamless.non_seamless_cube_map != 0 {
            extensions.push(ash::ext::non_seamless_cube_map::NAME.as_ptr());
        }
    }
    g.non_seamless_cubes = non_seamless.non_seamless_cube_map != 0;

    // Render passes give way to dynamic rendering, and cull mode, winding and the depth
    // and stencil tests become state set per draw: then a shader compiled on its own, as
    // a pipeline library, fits every target and every draw it is used in, and can be
    // compiled when the title loads it rather than at the first draw -- which is what
    // the console does, where a shader is ready the moment it is loaded.
    let mut dynamic_rendering = vk::PhysicalDeviceDynamicRenderingFeatures::default();
    let mut dynamic_state = vk::PhysicalDeviceExtendedDynamicStateFeaturesEXT::default();
    let mut libraries = vk::PhysicalDeviceGraphicsPipelineLibraryFeaturesEXT::default();
    {
        dynamic_rendering.p_next = (&raw mut dynamic_state).cast();
        dynamic_state.p_next = (&raw mut libraries).cast();
        features2((&raw mut dynamic_rendering).cast());
    }
    let has_dynamic_rendering = has(ash::khr::dynamic_rendering::NAME) && dynamic_rendering.dynamic_rendering != 0;
    let has_dynamic_state = has(ash::ext::extended_dynamic_state::NAME) && dynamic_state.extended_dynamic_state != 0;
    if !has_dynamic_rendering || !has_dynamic_state {
        log_line!(
            b"[w] ",
            "vulkan: the device lacks dynamic rendering or extended dynamic state, which the renderer needs"
        );
        return None;
    }
    extensions.push(ash::khr::dynamic_rendering::NAME.as_ptr());
    extensions.push(ash::ext::extended_dynamic_state::NAME.as_ptr());

    // Every texture in one set that stays bound, written as textures are made, and a
    // draw's record found through its base vertex and handed to the pixel stage
    // (shader/bindings.rs). Vulkan 1.2 has all of it; every device with the dynamic
    // rendering above does.
    let mut indexing = vk::PhysicalDeviceDescriptorIndexingFeatures::default();
    let mut draw_parameters = vk::PhysicalDeviceShaderDrawParametersFeatures::default();
    {
        let mut properties = vk::PhysicalDeviceProperties::default();
        // SAFETY: the caller's physical device.
        unsafe { (v10.get_physical_device_properties)(physical, &mut properties) };
        let core = properties.api_version >= vk::API_VERSION_1_2;
        if !core && has(ash::ext::descriptor_indexing::NAME) {
            extensions.push(ash::ext::descriptor_indexing::NAME.as_ptr());
        }
        indexing.p_next = (&raw mut draw_parameters).cast();
        features2((&raw mut indexing).cast());

        let mut limits = vk::PhysicalDeviceDescriptorIndexingProperties::default();
        let mut subgroup = vk::PhysicalDeviceSubgroupProperties::default();
        limits.p_next = (&raw mut subgroup).cast();
        properties2((&raw mut limits).cast());

        // A layout with an update-after-bind set is held to these limits for every
        // descriptor in it, set 2's included. A stage's resources are what it can reach
        // other than samplers, which that limit leaves out: set 1's images, set 2's two
        // views of the arena, and for the pixel stage the one colour target the renderer
        // draws into. Set 1's pool, samplers and all, counts against what all such pools
        // may hold.
        const TEXTURES: u32 = bindings::IMAGE_COUNT + bindings::SAMPLER_COUNT;
        const ARENA_VIEWS: u32 = 2;
        const STAGE_RESOURCES: u32 = bindings::IMAGE_COUNT + ARENA_VIEWS + 1;
        let missing = if properties.api_version < vk::API_VERSION_1_1 {
            Some("Vulkan 1.1")
        } else if indexing.descriptor_binding_sampled_image_update_after_bind == 0
            || indexing.descriptor_binding_partially_bound == 0
            || indexing.descriptor_binding_update_unused_while_pending == 0
        {
            Some("descriptor indexing")
        } else if supported.shader_sampled_image_array_dynamic_indexing == 0 {
            Some("dynamically indexed textures")
        } else if draw_parameters.shader_draw_parameters == 0 {
            Some("shader draw parameters")
        } else if !subgroup.supported_stages.contains(vk::ShaderStageFlags::FRAGMENT)
            || !subgroup.supported_operations.contains(vk::SubgroupFeatureFlags::BALLOT)
        {
            Some("subgroup ballot in pixel shaders")
        } else if limits.max_per_stage_descriptor_update_after_bind_sampled_images < bindings::IMAGE_COUNT
            || limits.max_descriptor_set_update_after_bind_sampled_images < bindings::IMAGE_COUNT
            || limits.max_per_stage_descriptor_update_after_bind_samplers < bindings::SAMPLER_COUNT
            || limits.max_descriptor_set_update_after_bind_samplers < bindings::SAMPLER_COUNT
            || limits.max_per_stage_descriptor_update_after_bind_storage_buffers < ARENA_VIEWS
            || limits.max_descriptor_set_update_after_bind_storage_buffers < ARENA_VIEWS
            || limits.max_per_stage_update_after_bind_resources < STAGE_RESOURCES
            || limits.max_update_after_bind_descriptors_in_all_pools < TEXTURES
        {
            Some("room for every texture in one set")
        } else if properties.limits.max_vertex_output_components < 4 * (bindings::RECORD_LOCATION + 1)
            || properties.limits.max_fragment_input_components < 4 * (bindings::RECORD_LOCATION + 1)
        {
            Some("seventeen interpolators")
        } else {
            None
        };
        if let Some(missing) = missing {
            log_line!(b"[w] ", "vulkan: the device lacks {missing}, which the renderer needs");
            return None;
        }
        enabled.shader_sampled_image_array_dynamic_indexing = vk::TRUE;
        indexing = vk::PhysicalDeviceDescriptorIndexingFeatures::default();
        indexing.descriptor_binding_sampled_image_update_after_bind = vk::TRUE;
        indexing.descriptor_binding_partially_bound = vk::TRUE;
        indexing.descriptor_binding_update_unused_while_pending = vk::TRUE;
        draw_parameters.shader_draw_parameters = vk::TRUE;
    }
    g.pipeline_libraries = has(ash::khr::pipeline_library::NAME)
        && has(ash::ext::graphics_pipeline_library::NAME)
        && libraries.graphics_pipeline_library != 0;
    if g.pipeline_libraries {
        extensions.push(ash::khr::pipeline_library::NAME.as_ptr());
        extensions.push(ash::ext::graphics_pipeline_library::NAME.as_ptr());
        let mut linking = vk::PhysicalDeviceGraphicsPipelineLibraryPropertiesEXT::default();
        properties2((&raw mut linking).cast());
        g.fast_linking = linking.graphics_pipeline_library_fast_linking != 0;
    } else {
        libraries.graphics_pipeline_library = vk::FALSE;
    }
    // Draws in a row that change nothing between them are made with one call
    // (renderer_draw.rs); without it each is made on its own.
    let mut multi_draw = vk::PhysicalDeviceMultiDrawFeaturesEXT::default();
    g.multi_draw = 0;
    if has(ash::ext::multi_draw::NAME) {
        features2((&raw mut multi_draw).cast());
        let mut most = vk::PhysicalDeviceMultiDrawPropertiesEXT::default();
        properties2((&raw mut most).cast());
        if multi_draw.multi_draw != 0 && most.max_multi_draw_count > 1 {
            extensions.push(ash::ext::multi_draw::NAME.as_ptr());
            g.multi_draw = most.max_multi_draw_count;
        }
    }
    multi_draw = vk::PhysicalDeviceMultiDrawFeaturesEXT::default();
    multi_draw.multi_draw = vk::TRUE;
    // The chain, linked from its end, each structure's address taken after its last
    // write: [non_seamless ->] indexing -> draw_parameters -> [multi_draw ->]
    // dynamic_rendering -> dynamic_state -> [libraries ->] the caller's features.
    libraries.p_next = features;
    dynamic_state.p_next = if g.pipeline_libraries { (&raw mut libraries).cast() } else { features };
    dynamic_rendering.p_next = (&raw mut dynamic_state).cast();
    multi_draw.p_next = (&raw mut dynamic_rendering).cast();
    draw_parameters.p_next =
        if g.multi_draw != 0 { (&raw mut multi_draw).cast() } else { (&raw mut dynamic_rendering).cast() };
    indexing.p_next = (&raw mut draw_parameters).cast();
    let chain: *mut c_void = (&raw mut indexing).cast();

    let mut info = vk::DeviceCreateInfo { p_next: chain, ..Default::default() };
    if g.non_seamless_cubes {
        non_seamless.p_next = chain;
        info.p_next = (&raw const non_seamless).cast();
    }
    info.queue_create_info_count = 1;
    info.p_queue_create_infos = &queue;
    info.enabled_extension_count = extensions.len() as u32;
    info.pp_enabled_extension_names = extensions.as_ptr();
    info.p_enabled_features = &enabled;
    let mut device = vk::Device::null();
    // SAFETY: the info and everything it points at live on this stack across the call.
    let r = unsafe { (v10.create_device)(physical, &info, std::ptr::null(), &mut device) };
    if r != vk::Result::SUCCESS {
        log_line!(b"[w] ", "vulkan: no device ({})", explain(r));
        return None;
    }
    let mut made = vk::Queue::null();
    // SAFETY: the device just made, through its own table.
    unsafe {
        let get_queue: vk::PFN_vkGetDeviceQueue =
            std::mem::transmute((v10.get_device_proc_addr)(device, c"vkGetDeviceQueue".as_ptr()));
        get_queue(device, family, 0, &mut made);
    }
    Some((device, made))
}

/// A device's VkPhysicalDeviceIDProperties::deviceUUID, in hex: which of the machine's
/// devices it is.
///
/// # Safety
/// `physical` is a physical device of `instance`.
pub unsafe fn device_id(instance: &ash::Instance, physical: vk::PhysicalDevice) -> String {
    let mut id = vk::PhysicalDeviceIDProperties::default();
    let mut properties = vk::PhysicalDeviceProperties2 { p_next: (&raw mut id).cast(), ..Default::default() };
    // SAFETY: the caller's; a chain on this stack.
    unsafe { (instance.fp_v1_1().get_physical_device_properties2)(physical, &mut properties) };
    id.device_uuid.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The device a headless initialise brings up from `instance`: the one `id` names, or
/// with none, the one the game's window would draw on. Its graphics queue family in
/// `family`; None if none.
///
/// # Safety
/// `instance` is live.
pub unsafe fn choose_device(
    instance: &ash::Instance,
    id: &[u8],
    family: Option<&mut u32>,
) -> Option<vk::PhysicalDevice> {
    let enumerate = instance.fp_v1_0().enumerate_physical_devices;
    let mut count = 0u32;
    // SAFETY: the caller's; a count, then that many.
    let _ = unsafe { enumerate(instance.handle(), &mut count, std::ptr::null_mut()) };
    let mut devices = vec![vk::PhysicalDevice::null(); count as usize];
    let _ = unsafe { enumerate(instance.handle(), &mut count, devices.as_mut_ptr()) };
    devices.truncate(count as usize);
    // The device the game's window would draw on (presenter.rs): the first discrete
    // one, else the first of any kind, with a graphics queue -- or the one asked for,
    // which the game names when it draws on another.
    for pass in 0..2 {
        for &candidate in &devices {
            let mut p = vk::PhysicalDeviceProperties::default();
            // SAFETY: a device the instance listed.
            unsafe { (instance.fp_v1_0().get_physical_device_properties)(candidate, &mut p) };
            let skip = if !id.is_empty() {
                unsafe { device_id(instance, candidate) }.as_bytes() != id
            } else {
                pass == 0 && p.device_type != vk::PhysicalDeviceType::DISCRETE_GPU
            };
            if skip {
                continue;
            }
            let graphics = unsafe { graphics_family(instance, candidate) };
            if graphics == u32::MAX {
                continue;
            }
            if let Some(family) = family {
                *family = graphics;
            }
            return Some(candidate);
        }
    }
    None
}

/// The device the next headless initialise brings up, by device_id; empty for
/// choose_device's own choice.
pub fn set_device(id: &[u8]) {
    *lock(&WANTED_DEVICE) = id.to_vec();
}

// Enough of a device to compile shaders on. No surface, no swapchain: the point is to
// reach the driver's compiler, which needs neither.
fn bring_up_headless(g: &mut State) -> bool {
    let app = vk::ApplicationInfo {
        p_application_name: c"mw2recomp".as_ptr(),
        // 1.1 for negative viewport heights, which is how the renderer expresses the
        // console's +Y-up clip space; 1.2 for the render pass depth resolve the
        // multisampled surfaces need.
        api_version: vk::API_VERSION_1_2,
        ..Default::default()
    };
    let info = vk::InstanceCreateInfo { p_application_info: &app, ..Default::default() };
    let mut instance = vk::Instance::null();
    // SAFETY: the info on this stack.
    let r = unsafe { (entry().fp_v1_0().create_instance)(&info, std::ptr::null(), &mut instance) };
    g.instance = instance;
    if r != vk::Result::SUCCESS {
        log_line!(b"[w] ", "pipeline: no Vulkan instance ({})", explain(r));
        return false;
    }
    // SAFETY: the instance just made.
    let table = unsafe { instance_table(instance) };
    g.instance_fn = Some(table.clone());

    let mut count = 0u32;
    // SAFETY: the live instance.
    let _ = unsafe { (table.fp_v1_0().enumerate_physical_devices)(instance, &mut count, std::ptr::null_mut()) };
    if count == 0 {
        log_line!(b"[w] ", "pipeline: no Vulkan device");
        return false;
    }

    let mut family = u32::MAX;
    let wanted = lock(&WANTED_DEVICE).clone();
    // SAFETY: the live instance.
    let Some(physical) = (unsafe { choose_device(&table, &wanted, Some(&mut family)) }) else {
        g.physical = vk::PhysicalDevice::null();
        if wanted.is_empty() {
            log_line!(b"[w] ", "pipeline: no graphics queue");
        } else {
            log_parts(&[b"[w] pipeline: no device ", &wanted]);
        }
        return false;
    };
    g.physical = physical;

    // SAFETY: a physical device of the instance; no extra extensions or features.
    let Some((device, queue)) = (unsafe { create_device_in(g, &table, physical, family, &[], std::ptr::null_mut()) })
    else {
        return false;
    };
    g.device = device;
    g.queue = queue;
    g.family = family;
    g.owns_device = true;
    true
}

fn create_set_layouts(g: &mut State, device: &ash::Device) -> bool {
    let both = vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT;

    // Every texture and sampler the cache holds (shader/bindings.rs). Written while
    // command buffers that bind the set are being recorded and run, each entry before
    // the first draw that names it and never while one that names it is pending; most
    // entries are empty.
    let mut textures = [vk::DescriptorSetLayoutBinding::default(); 2];
    let mut texture_flags = [vk::DescriptorBindingFlags::empty(); 2];
    for i in 0..2u32 {
        let t = &mut textures[i as usize];
        t.binding = i;
        t.descriptor_type =
            if i == bindings::SAMPLERS { vk::DescriptorType::SAMPLER } else { vk::DescriptorType::SAMPLED_IMAGE };
        t.descriptor_count = if i == bindings::SAMPLERS { bindings::SAMPLER_COUNT } else { bindings::IMAGE_COUNT };
        t.stage_flags = both;
        texture_flags[i as usize] = vk::DescriptorBindingFlags::UPDATE_AFTER_BIND
            | vk::DescriptorBindingFlags::UPDATE_UNUSED_WHILE_PENDING
            | vk::DescriptorBindingFlags::PARTIALLY_BOUND;
    }
    let texture_binding_flags = vk::DescriptorSetLayoutBindingFlagsCreateInfo {
        binding_count: 2,
        p_binding_flags: texture_flags.as_ptr(),
        ..Default::default()
    };

    let mut memory = [vk::DescriptorSetLayoutBinding::default(); 2];
    for (i, binding) in memory.iter_mut().enumerate() {
        binding.binding = if i != 0 { bindings::ARENA_FLOATS } else { bindings::ARENA_BINDING };
        binding.descriptor_type = vk::DescriptorType::STORAGE_BUFFER;
        binding.descriptor_count = 1;
        binding.stage_flags = both;
    }

    let mut sets: [(*const vk::DescriptorSetLayoutBinding, u32); 3] = [(std::ptr::null(), 0); 3];
    sets[bindings::CONSTANT_SET as usize] = (std::ptr::null(), 0); // the constants are in the arena
    sets[bindings::TEXTURE_SET as usize] = (textures.as_ptr(), 2);
    sets[bindings::ARENA_SET as usize] = (memory.as_ptr(), 2);
    for i in 0..3u32 {
        let mut info = vk::DescriptorSetLayoutCreateInfo {
            binding_count: sets[i as usize].1,
            p_bindings: sets[i as usize].0,
            ..Default::default()
        };
        if i == bindings::TEXTURE_SET {
            info.flags = vk::DescriptorSetLayoutCreateFlags::UPDATE_AFTER_BIND_POOL;
            info.p_next = (&raw const texture_binding_flags).cast();
        }
        let mut made = vk::DescriptorSetLayout::null();
        // SAFETY: the info and its arrays on this stack.
        let r = unsafe {
            (device.fp_v1_0().create_descriptor_set_layout)(device.handle(), &info, std::ptr::null(), &mut made)
        };
        g.sets[i as usize] = made;
        if r != vk::Result::SUCCESS {
            log_line!(b"[w] ", "pipeline: descriptor set layout {i} failed ({})", explain(r));
            return false;
        }
    }

    // No push constants: the register state a draw needs is in its record
    // (shader/bindings.rs).
    let info =
        vk::PipelineLayoutCreateInfo { set_layout_count: 3, p_set_layouts: g.sets.as_ptr(), ..Default::default() };
    let mut layout = vk::PipelineLayout::null();
    // SAFETY: as above.
    let r = unsafe { (device.fp_v1_0().create_pipeline_layout)(device.handle(), &info, std::ptr::null(), &mut layout) };
    g.layout = layout;
    if r != vk::Result::SUCCESS {
        log_line!(b"[w] ", "pipeline: layout failed ({})", explain(r));
        return false;
    }
    true
}

// MW2_PIPELINE_CACHE=<file> in place of the one the game always keeps. Without a file
// the cache still works, just only within one run.
fn cache_path() -> Option<CString> {
    if let Some(asked) = text(c"MW2_PIPELINE_CACHE") {
        return Some(asked.to_owned());
    }
    let path = lock(&CACHE_PATH);
    if path.is_empty() { None } else { CString::new(path.clone()).ok() }
}

fn create_cache(g: &mut State, device: &ash::Device) {
    let initial = match cache_path() {
        Some(path) => stdio::read(&path),
        None => Vec::new(),
    };
    let mut info = vk::PipelineCacheCreateInfo {
        initial_data_size: initial.len(),
        p_initial_data: if initial.is_empty() { std::ptr::null() } else { initial.as_ptr().cast() },
        ..Default::default()
    };
    let create = device.fp_v1_0().create_pipeline_cache;
    let mut cache = vk::PipelineCache::null();
    // A cache the driver rejects is not an error: it starts empty instead.
    // SAFETY: the info and the data it points at live across the call.
    if unsafe { create(device.handle(), &info, std::ptr::null(), &mut cache) } != vk::Result::SUCCESS {
        info.initial_data_size = 0;
        info.p_initial_data = std::ptr::null();
        cache = vk::PipelineCache::null();
        let _ = unsafe { create(device.handle(), &info, std::ptr::null(), &mut cache) };
    } else if !initial.is_empty() {
        g.cache_loaded = true;
        log_line!(b"[i] ", "pipeline: loaded {} KB of pipeline cache", initial.len() / 1024);
    }
    g.cache = cache;
}

fn save_cache(g: &State, device: &ash::Device) {
    let Some(path) = cache_path() else { return };
    if g.cache == vk::PipelineCache::null() {
        return;
    }
    let get = device.fp_v1_0().get_pipeline_cache_data;
    let mut size = 0usize;
    // SAFETY: the live cache; a size, then that many bytes.
    if unsafe { get(device.handle(), g.cache, &mut size, std::ptr::null_mut()) } != vk::Result::SUCCESS || size == 0 {
        return;
    }
    let mut data = vec![0u8; size];
    if unsafe { get(device.handle(), g.cache, &mut size, data.as_mut_ptr().cast()) } != vk::Result::SUCCESS {
        return;
    }
    // Beside it first, then in its place: a run that ends halfway through the write
    // leaves the cache it had, not a broken one.
    let mut partial = path.as_bytes().to_vec();
    partial.extend_from_slice(b".partial");
    let partial = CString::new(partial).unwrap();
    if !stdio::write(&partial, &data[..size]) {
        return;
    }
    match files::rename_file(&partial, &path) {
        Err(message) => log_parts(&[
            b"[w] pipeline: the pipeline cache could not be put in place at ",
            path.as_bytes(),
            b" (",
            &message,
            b")",
        ]),
        Ok(()) => log_parts(&[
            format!("[i] pipeline: wrote {} KB of pipeline cache to ", size / 1024).as_bytes(),
            path.as_bytes(),
        ]),
    }
}

/// The file the pipeline cache is read from at initialise and written back to at
/// shutdown (MW2_PIPELINE_CACHE overrides it). The launcher fills it with every shader of
/// the game. None for no file.
pub fn set_cache_path(path: Option<&[u8]>) {
    *lock(&CACHE_PATH) = path.unwrap_or_default().to_vec();
}

/// Whether initialise found a cache there the driver took.
pub fn cache_loaded() -> bool {
    read().cache_loaded
}

/// Whether a pipeline's creation can say if it came out of the cache
/// (VK_EXT_pipeline_creation_feedback).
pub fn creation_feedback() -> bool {
    read().creation_feedback
}

// MW2_REPORT=1: a bug report is read against the driver's known faults.
fn report_on() -> bool {
    static ON: LazyLock<bool> = LazyLock::new(|| flag(c"MW2_REPORT"));
    *ON
}

/// The device the renderer draws on, the layout and the cache. Brings up a headless
/// device of its own when `borrowed` is None, which is what the offline tools use to put
/// a translated shader in front of a real driver. A borrowed device comes with its
/// instance, which RenderDoc needs to name the device a capture is of; it stays the
/// lender's to destroy.
pub struct Borrowed {
    pub device: vk::Device,
    pub physical: vk::PhysicalDevice,
    pub queue: vk::Queue,
    pub queue_family: u32,
    pub instance: vk::Instance,
}

/// # Safety
/// A borrowed device, its physical device, queue and instance are live until shutdown.
pub unsafe fn initialise(borrowed: Option<Borrowed>) -> bool {
    let mut g = write();
    if g.device != vk::Device::null() {
        return true;
    }

    match borrowed {
        Some(b) if b.device != vk::Device::null() && b.physical != vk::PhysicalDevice::null() => {
            // The device's and the instance's commands are loaded from the instance, so a
            // lent device comes with it (no caller lends one without).
            if b.instance == vk::Instance::null() {
                return false;
            }
            g.instance = b.instance;
            // SAFETY: the lender's live instance.
            g.instance_fn = Some(unsafe { instance_table(b.instance) });
            g.device = b.device;
            g.physical = b.physical;
            g.queue = b.queue;
            g.family = b.queue_family;
            g.owns_device = false;
        }
        _ => {
            if !bring_up_headless(&mut g) {
                return false;
            }
        }
    }
    let instance = g.instance_fn.clone().expect("an instance is loaded with the device");
    // SAFETY: the device made or lent, of this instance.
    let device = Arc::new(unsafe { ash::Device::load(instance.fp_v1_0(), g.device) });
    g.device_fn = Some(device.clone());

    let mut props = vk::PhysicalDeviceProperties::default();
    // SAFETY: the live physical device.
    unsafe { (instance.fp_v1_0().get_physical_device_properties)(g.physical, &mut props) };
    let name = unsafe { CStr::from_ptr(props.device_name.as_ptr()) };
    g.name = Some(name.to_owned());
    if report_on() {
        let mut driver = vk::PhysicalDeviceDriverProperties::default();
        if props.api_version >= vk::API_VERSION_1_2 {
            let mut more = vk::PhysicalDeviceProperties2 { p_next: (&raw mut driver).cast(), ..Default::default() };
            // SAFETY: a chain on this stack.
            unsafe { (instance.fp_v1_1().get_physical_device_properties2)(g.physical, &mut more) };
        }
        let driver_name = unsafe { CStr::from_ptr(driver.driver_name.as_ptr()) };
        let driver_info = unsafe { CStr::from_ptr(driver.driver_info.as_ptr()) };
        log_parts(&[
            b"[i] report: graphics ",
            name.to_bytes(),
            format!(" ({:04X}:{:04X}), driver ", props.vendor_id, props.device_id).as_bytes(),
            driver_name.to_bytes(),
            b" ",
            driver_info.to_bytes(),
            format!(
                " ({:08X}), Vulkan {}.{}.{}",
                props.driver_version,
                vk::api_version_major(props.api_version),
                vk::api_version_minor(props.api_version),
                vk::api_version_patch(props.api_version)
            )
            .as_bytes(),
        ]);
    }

    if !create_set_layouts(&mut g, &device) {
        shutdown_in(&mut g);
        return false;
    }
    create_cache(&mut g, &device);
    log_parts(&[b"[i] pipeline: layout ready on ", name.to_bytes()]);
    true
}

fn shutdown_in(g: &mut State) {
    if g.device == vk::Device::null() {
        return;
    }
    let device = g.device_fn.take().expect("a device's table is loaded with it");
    save_cache(g, &device);
    let v10 = device.fp_v1_0();
    // SAFETY: what this file made on the live device, nothing of it in use any more (the
    // caller's).
    unsafe {
        if g.cache != vk::PipelineCache::null() {
            (v10.destroy_pipeline_cache)(device.handle(), g.cache, std::ptr::null());
        }
        if g.layout != vk::PipelineLayout::null() {
            (v10.destroy_pipeline_layout)(device.handle(), g.layout, std::ptr::null());
        }
        for set in &mut g.sets {
            if *set != vk::DescriptorSetLayout::null() {
                (v10.destroy_descriptor_set_layout)(device.handle(), *set, std::ptr::null());
                *set = vk::DescriptorSetLayout::null();
            }
        }
    }
    g.cache = vk::PipelineCache::null();
    g.layout = vk::PipelineLayout::null();
    if g.owns_device {
        // SAFETY: made by bring_up_headless, nothing of it in use.
        unsafe {
            (v10.destroy_device)(device.handle(), std::ptr::null());
            if g.instance != vk::Instance::null()
                && let Some(instance) = &g.instance_fn
            {
                (instance.fp_v1_0().destroy_instance)(g.instance, std::ptr::null());
            }
        }
        g.instance = vk::Instance::null();
        g.instance_fn = None;
    }
    g.device = vk::Device::null();
    g.physical = vk::PhysicalDevice::null();
    g.queue = vk::Queue::null();
    g.cache_loaded = false;
}

/// Writes the cache back and destroys what initialise made, the device too when it
/// brought it up.
///
/// # Safety
/// Nothing made with the layout, the cache or an owned device is in use.
pub unsafe fn shutdown() {
    shutdown_in(&mut write());
}

pub fn ready() -> bool {
    read().device != vk::Device::null()
}

/// The device's name; "none" before one is up.
pub fn device_name() -> CString {
    read().name.clone().unwrap_or_else(|| c"none".to_owned())
}

pub fn instance() -> vk::Instance {
    read().instance
}

pub fn device() -> vk::Device {
    read().device
}

/// The device's commands, None before initialise.
pub fn device_table() -> Option<Arc<ash::Device>> {
    read().device_fn.clone()
}

pub fn physical_device() -> vk::PhysicalDevice {
    read().physical
}

pub fn queue() -> vk::Queue {
    read().queue
}

pub fn queue_family() -> u32 {
    read().family
}

/// Whether an occlusion query may be asked for an exact sample count rather than only
/// "some passed".
pub fn precise_occlusion_queries() -> bool {
    read().precise_queries
}

/// The most a sampler may filter anisotropically; 1 when the device cannot.
pub fn max_anisotropy() -> f32 {
    read().max_anisotropy
}

/// Whether a sampler may filter a cube's faces apart, as Direct3D 9 does.
pub fn non_seamless_cubes() -> bool {
    read().non_seamless_cubes
}

/// The most draws one vkCmdDrawMulti*EXT call may make; 0 without VK_EXT_multi_draw.
pub fn multi_draw() -> u32 {
    read().multi_draw
}

/// The most samplers the texture cache may make: the set's slots or the device's
/// allocation limit, whichever is lower.
pub fn samplers() -> u32 {
    read().samplers
}

/// Whether shaders can be compiled on their own as pipeline libraries and linked quickly
/// (VK_EXT_graphics_pipeline_library with fast linking). MW2_NO_PIPELINE_LIBRARIES=1
/// builds whole pipelines at the first draw instead.
pub fn pipeline_libraries() -> bool {
    static REFUSED: LazyLock<bool> = LazyLock::new(|| flag(c"MW2_NO_PIPELINE_LIBRARIES"));
    let g = read();
    g.pipeline_libraries && g.fast_linking && !*REFUSED
}

/// The device has one queue, and the window thread and the GPU thread both submit to
/// it; Vulkan requires a queue to be externally synchronised.
pub fn queue_mutex() -> &'static Mutex<()> {
    static QUEUE: Mutex<()> = Mutex::new(());
    &QUEUE
}

/// VkDescriptorSetLayout `set` (0 to 2); null past them.
pub fn set_layout(set: u32) -> vk::DescriptorSetLayout {
    if set < 3 { read().sets[set as usize] } else { vk::DescriptorSetLayout::null() }
}

pub fn layout() -> vk::PipelineLayout {
    read().layout
}

pub fn cache() -> vk::PipelineCache {
    read().cache
}

/// Creates and destroys a shader module: a stronger check than spirv-val, because it is
/// the compiler that will actually run the shader. The reason when it fails.
pub fn create_module(spirv: &[u32]) -> Result<(), &'static str> {
    let Some(device) = device_table() else { return Err("no device") };
    let info = vk::ShaderModuleCreateInfo { code_size: spirv.len() * 4, p_code: spirv.as_ptr(), ..Default::default() };
    let mut module = vk::ShaderModule::null();
    // SAFETY: the live device, and the words across the call.
    let r = unsafe { (device.fp_v1_0().create_shader_module)(device.handle(), &info, std::ptr::null(), &mut module) };
    if r != vk::Result::SUCCESS {
        return Err(explain(r));
    }
    // SAFETY: made just now, used by nothing.
    unsafe { (device.fp_v1_0().destroy_shader_module)(device.handle(), module, std::ptr::null()) };
    Ok(())
}
