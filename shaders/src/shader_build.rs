// One shader made ready for the GPU: translated, made a module and, where the device has
// pipeline libraries, compiled on its own as its stage's library.
//
// The game does this as the title loads its shaders (renderer_libraries.rs), and the
// launcher does it for every shader of the game before the game first runs, into the
// pipeline cache the game then starts with. A driver finds a library in that cache only
// when it is asked for exactly the same thing, so both build it here, and everything the
// result depends on -- the depth format, the scale, the pipeline layout -- is decided
// here as well.
#![allow(clippy::missing_safety_doc)]

use crate::pipeline;
use crate::shader::{Translation, translate};
use ash::vk;
use std::ffi::c_void;

/// Where a player's copy keeps the pipeline cache the launcher fills, beside game/ and
/// saves/.
pub const CACHE_FILE: &str = "shaders/pipelines.bin";

/// What this build's shaders come out as: a SHA-256 build.rs takes of the sources that
/// decide it -- the translator, this file, the pipeline layout, the launcher's reader of
/// the game's shaders. Another build's cache holds other shaders.
pub const BUILD_ID: &str = env!("MW2_SHADER_BUILD");

fn has(
    instance: &ash::Instance,
    physical: vk::PhysicalDevice,
    candidate: vk::Format,
    wanted: vk::FormatFeatureFlags,
) -> bool {
    let mut format = vk::FormatProperties::default();
    // SAFETY: the caller's physical device of `instance`.
    unsafe { (instance.fp_v1_0().get_physical_device_format_properties)(physical, candidate, &mut format) };
    format.optimal_tiling_features & wanted == wanted
}

/// The depth and stencil format of the title's D24S8 surfaces on this device, as Xenia's
/// render target cache: D24S8 where it can be drawn into and sampled (a depth resolve is
/// sampled; AMD has none), else D32S8. UNDEFINED if neither. The shader libraries are
/// built with it.
///
/// # Safety
/// `physical` is a physical device of `instance`.
pub unsafe fn depth_format(instance: &ash::Instance, physical: vk::PhysicalDevice) -> vk::Format {
    use vk::FormatFeatureFlags as F;
    if has(instance, physical, vk::Format::D24_UNORM_S8_UINT, F::DEPTH_STENCIL_ATTACHMENT | F::SAMPLED_IMAGE) {
        return vk::Format::D24_UNORM_S8_UINT;
    }
    if has(instance, physical, vk::Format::D32_SFLOAT_S8_UINT, F::DEPTH_STENCIL_ATTACHMENT) {
        return vk::Format::D32_SFLOAT_S8_UINT;
    }
    if has(instance, physical, vk::Format::D24_UNORM_S8_UINT, F::DEPTH_STENCIL_ATTACHMENT) {
        return vk::Format::D24_UNORM_S8_UINT;
    }
    vk::Format::UNDEFINED
}

/// The format of its D24FS8 surfaces, the shadow maps: D32S8, as Xenia has it, since a
/// float depth keeps its precision and its depth bias only in a float format.
/// depth_format's if the device cannot draw into D32S8.
///
/// # Safety
/// As depth_format.
pub unsafe fn float_depth_format(instance: &ash::Instance, physical: vk::PhysicalDevice) -> vk::Format {
    if has(instance, physical, vk::Format::D32_SFLOAT_S8_UINT, vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT) {
        return vk::Format::D32_SFLOAT_S8_UINT;
    }
    // SAFETY: the caller's.
    unsafe { depth_format(instance, physical) }
}

fn max_image(instance: &ash::Instance, physical: vk::PhysicalDevice) -> u32 {
    let mut properties = vk::PhysicalDeviceProperties::default();
    // SAFETY: the caller's physical device of `instance`.
    unsafe { (instance.fp_v1_0().get_physical_device_properties)(physical, &mut properties) };
    properties.limits.max_image_dimension2_d
}

/// MW2_SCALE as the device can draw it: a surface is at most 4096 of the title's pixels
/// either way, and must fit one of the device's images.
///
/// # Safety
/// As depth_format.
pub unsafe fn scale(instance: &ash::Instance, physical: vk::PhysicalDevice, asked: u32) -> u32 {
    let most = max_image(instance, physical);
    let mut scale = asked.clamp(1, 3);
    while scale > 1 && 4096u32.wrapping_mul(scale) > most {
        scale -= 1;
    }
    scale
}

/// MW2_SHADOW_SCALE as the device can draw it: the shadow maps' surfaces at 1 to 4 times
/// the title's, and never less than the frame's `scale`.
pub const MAX_SHADOW_SCALE: u32 = 4;

/// # Safety
/// As depth_format.
pub unsafe fn shadow_scale(instance: &ash::Instance, physical: vk::PhysicalDevice, asked: u32, scale: u32) -> u32 {
    let most = max_image(instance, physical);
    let mut shadow = asked.clamp(1, MAX_SHADOW_SCALE);
    while shadow > 1 && 4096u32.wrapping_mul(shadow) > most {
        shadow -= 1;
    }
    shadow.max(scale)
}

/// The dynamic state every pipeline here has.
pub static DYNAMIC_STATES: [vk::DynamicState; 13] = [
    vk::DynamicState::VIEWPORT,
    vk::DynamicState::SCISSOR,
    vk::DynamicState::DEPTH_BIAS,
    vk::DynamicState::STENCIL_COMPARE_MASK,
    vk::DynamicState::STENCIL_WRITE_MASK,
    vk::DynamicState::STENCIL_REFERENCE,
    vk::DynamicState::CULL_MODE,
    vk::DynamicState::FRONT_FACE,
    vk::DynamicState::DEPTH_TEST_ENABLE,
    vk::DynamicState::DEPTH_WRITE_ENABLE,
    vk::DynamicState::DEPTH_COMPARE_OP,
    vk::DynamicState::STENCIL_TEST_ENABLE,
    vk::DynamicState::STENCIL_OP,
];

/// The rasterisation state every pipeline starts from.
pub fn rasterisation() -> vk::PipelineRasterizationStateCreateInfo<'static> {
    vk::PipelineRasterizationStateCreateInfo {
        polygon_mode: vk::PolygonMode::FILL,
        // Culling and winding are set per draw (faces_for).
        line_width: 1.0,
        // Always on, and set per draw: a shadow map drawn without the title's bias
        // shadows the very surfaces it was rendered from.
        depth_bias_enable: vk::TRUE,
        ..Default::default()
    }
}

/// A library of one part of a pipeline, on pipeline.rs's device, layout and cache.
/// `stage` is None for the parts that hold only state. `next` goes on the library's
/// create info chain. Null when the driver refuses it.
///
/// # Safety
/// `info`'s pointers and `next`'s chain are valid for the call.
pub unsafe fn make_library<'a>(
    part: vk::GraphicsPipelineLibraryFlagsEXT,
    stage: Option<&'a vk::PipelineShaderStageCreateInfo<'a>>,
    mut info: vk::GraphicsPipelineCreateInfo<'a>,
    next: *const c_void,
) -> vk::Pipeline {
    let Some(device) = pipeline::device_table() else { return vk::Pipeline::null() };
    let library = vk::GraphicsPipelineLibraryCreateInfoEXT { p_next: next, flags: part, ..Default::default() };
    info.s_type = vk::StructureType::GRAPHICS_PIPELINE_CREATE_INFO;
    info.p_next = (&raw const library).cast();
    info.flags = vk::PipelineCreateFlags::LIBRARY_KHR | vk::PipelineCreateFlags::RETAIN_LINK_TIME_OPTIMIZATION_INFO_EXT;
    info.stage_count = u32::from(stage.is_some());
    info.p_stages = stage.map_or(std::ptr::null(), |stage| stage as *const _);
    info.layout = pipeline::layout();
    let mut made = vk::Pipeline::null();
    // SAFETY: the caller's info, with the library info on this stack, on the live device.
    let r = unsafe {
        (device.fp_v1_0().create_graphics_pipelines)(
            device.handle(),
            pipeline::cache(),
            1,
            &info,
            std::ptr::null(),
            &mut made,
        )
    };
    if r != vk::Result::SUCCESS {
        return vk::Pipeline::null();
    }
    made
}

/// A shader built: its translation, its module (null when it did not translate, or the
/// driver refused it) and its library (null on a device without pipeline libraries).
pub struct Built {
    pub translation: Translation,
    pub module: vk::ShaderModule,
    pub library: vk::Pipeline,
    /// Whether the driver found the library in the pipeline cache: 1 it did, 0 it
    /// compiled it, -1 it does not say (or there is no library).
    pub from_cache: i32,
}

fn stage_library(
    pixel: bool,
    module: vk::ShaderModule,
    depth_format: vk::Format,
    from_cache: &mut i32,
) -> vk::Pipeline {
    // Without a render pass, the shader stages learn from their own rendering info
    // whether there is a depth and stencil attachment, and ignore which format it is:
    // built with depth_format's, the libraries also fit a D24FS8 target's. A fragment
    // library must carry it: without it, the library legally ignores depth and stencil
    // state, and every draw comes out empty.
    let mut rendering = vk::PipelineRenderingCreateInfo {
        depth_attachment_format: depth_format,
        stencil_attachment_format: depth_format,
        ..Default::default()
    };
    let mut feedback = vk::PipelineCreationFeedback::default();
    let feedback_info =
        vk::PipelineCreationFeedbackCreateInfo { p_pipeline_creation_feedback: &mut feedback, ..Default::default() };
    if pipeline::creation_feedback() {
        rendering.p_next = (&raw const feedback_info).cast();
    }
    let dynamic = vk::PipelineDynamicStateCreateInfo {
        dynamic_state_count: DYNAMIC_STATES.len() as u32,
        p_dynamic_states: DYNAMIC_STATES.as_ptr(),
        ..Default::default()
    };

    let mut stage = vk::PipelineShaderStageCreateInfo { module, p_name: c"main".as_ptr(), ..Default::default() };
    let mut info = vk::GraphicsPipelineCreateInfo { p_dynamic_state: &dynamic, ..Default::default() };
    let depth = vk::PipelineDepthStencilStateCreateInfo::default();
    let made;
    if !pixel {
        let viewport =
            vk::PipelineViewportStateCreateInfo { viewport_count: 1, scissor_count: 1, ..Default::default() };
        let raster = rasterisation();
        stage.stage = vk::ShaderStageFlags::VERTEX;
        info.p_viewport_state = &viewport;
        info.p_rasterization_state = &raster;
        // SAFETY: the info's states on this stack.
        made = unsafe {
            make_library(
                vk::GraphicsPipelineLibraryFlagsEXT::PRE_RASTERIZATION_SHADERS,
                Some(&stage),
                info,
                (&raw const rendering).cast(),
            )
        };
    } else {
        // The tests themselves are set per draw (depth_tests_for).
        stage.stage = vk::ShaderStageFlags::FRAGMENT;
        info.p_depth_stencil_state = &depth;
        // SAFETY: as above.
        made = unsafe {
            make_library(
                vk::GraphicsPipelineLibraryFlagsEXT::FRAGMENT_SHADER,
                Some(&stage),
                info,
                (&raw const rendering).cast(),
            )
        };
    }
    if feedback.flags.contains(vk::PipelineCreationFeedbackFlags::VALID) {
        *from_cache = if feedback.flags.contains(vk::PipelineCreationFeedbackFlags::APPLICATION_PIPELINE_CACHE_HIT) {
            1
        } else {
            0
        };
    }
    made
}

/// Translates `code` and makes its module and, where the device has them, its stage's
/// library. Any thread: the device, layout and cache are pipeline.rs's. `scaled`: either
/// scale is past 1 (translate).
pub fn build(pixel: bool, code: &[u32], scaled: bool, depth_format: vk::Format) -> Built {
    let mut built = Built {
        translation: translate(pixel, code, scaled),
        module: vk::ShaderModule::null(),
        library: vk::Pipeline::null(),
        from_cache: -1,
    };
    if !built.translation.ok {
        return built;
    }
    let Some(device) = pipeline::device_table() else { return built };
    let spirv = &built.translation.spirv;
    let info = vk::ShaderModuleCreateInfo { code_size: spirv.len() * 4, p_code: spirv.as_ptr(), ..Default::default() };
    let mut module = vk::ShaderModule::null();
    // SAFETY: the words across the call, on the live device.
    if unsafe { (device.fp_v1_0().create_shader_module)(device.handle(), &info, std::ptr::null(), &mut module) }
        != vk::Result::SUCCESS
    {
        return built;
    }
    built.module = module;
    if pipeline::pipeline_libraries() {
        built.library = stage_library(pixel, built.module, depth_format, &mut built.from_cache);
        if built.library == vk::Pipeline::null() {
            built.from_cache = -1;
        }
    }
    built
}

/// Destroys what build made.
///
/// # Safety
/// Nothing made from the module or the library is still being made.
pub unsafe fn destroy(built: &mut Built) {
    if let Some(device) = pipeline::device_table() {
        // SAFETY: the caller's.
        unsafe {
            if built.library != vk::Pipeline::null() {
                (device.fp_v1_0().destroy_pipeline)(device.handle(), built.library, std::ptr::null());
            }
            if built.module != vk::ShaderModule::null() {
                (device.fp_v1_0().destroy_shader_module)(device.handle(), built.module, std::ptr::null());
            }
        }
    }
    built.library = vk::Pipeline::null();
    built.module = vk::ShaderModule::null();
}
