// The renderer, the presenter and the texture cache.
//
// Every file here calls Vulkan through ash, on the device the shaders crate makes
// (shaders/src/pipeline.rs's create_device) and the instance the presenter makes, or the
// crate's own when there is no window: their tables are loaded from those handles here,
// and the extensions' only when the device enabled them.
// Each such file is unsafe by nature, and says what keeps its handles valid.
#![deny(unsafe_op_in_unsafe_fn)]

pub mod backdrop;
pub mod capture;
// The renderer's device tables and its Vulkan calls (device.rs), the recorder thread and
// queue (recorder.rs) and the command trace (trace.rs).
pub mod device;
// What each format can do on the device and the fallbacks where it cannot
// (format_support.rs), and the texel conversions they need (texture_convert.rs).
pub mod format_support;
pub mod gpu_time;
pub mod presenter;
pub mod recorder;
#[deny(unsafe_code)]
pub mod texture_convert;
// The renderer's core: the arena and the index swap (renderer_arena.rs), the draw's
// arithmetic (renderer_draw.rs), the state mappings and PipelineState
// (renderer_pipelines.rs) and the keys and caches they share (renderer_state.rs).
pub mod renderer_arena;
#[deny(unsafe_code)]
pub mod renderer_draw;
pub mod renderer_pipelines;
#[deny(unsafe_code)]
pub mod renderer_state;
#[deny(unsafe_code)]
pub mod texture_formats;
#[cfg(feature = "diagnostics")]
pub mod trace;
#[deny(unsafe_code)]
pub mod tracked;
pub mod util;
// The shader workers, the optimiser and the pipeline libraries (renderer_libraries.rs).
pub mod renderer_libraries;
// The texture cache and its image memory: texture_cache.rs, image_memory.rs.
pub mod image_memory;
pub mod texture_cache;
// The EDRAM surfaces as images (renderer_targets.rs), the resolves and the swap
// (renderer_resolve.rs), the display's colour table (display_table.rs) and the renderer's
// own shaders (spirv_builtin.rs, outside the shader-build hash).
pub mod display_table;
pub mod renderer_resolve;
pub mod renderer_targets;
pub mod spirv_builtin;
// MW2_SUBGROUP_CHECK: whether a subgroup of pixel invocations ever holds two draws
// (subgroup_check.rs; diagnostics builds, at the renderer's start).
#[cfg(feature = "diagnostics")]
pub mod subgroup_check;
// The renderer whole (renderer.rs), its frames, submissions and occlusion queries
// (renderer_frame.rs), the switches that look at a frame and the report
// (renderer_diagnostics.rs), and MW2_TIME_PASSES (renderer_passtime.rs); the draw itself
// is renderer_draw.rs's. tools/replay_diff.py holds two builds of it to each other on one
// GPU capture.
pub mod renderer;
pub mod renderer_diagnostics;
pub mod renderer_frame;
pub mod renderer_passtime;

use ash::vk;
use ash::vk::Handle;
use std::ffi::CStr;
use std::sync::LazyLock;

/// The Vulkan loader, linked by name (`vulkan`, `vulkan-1`).
pub fn entry() -> &'static ash::Entry {
    static ENTRY: LazyLock<ash::Entry> = LazyLock::new(ash::Entry::linked);
    &ENTRY
}

/// The instance commands of an instance made elsewhere.
///
/// # Safety
/// `raw` is a live VkInstance, which outlives every use of the table.
pub unsafe fn instance_table(raw: *mut std::ffi::c_void) -> ash::Instance {
    // SAFETY: the caller's live instance, through the linked loader.
    unsafe { ash::Instance::load(entry().static_fn(), vk::Instance::from_raw(raw as u64)) }
}

/// The shared pipeline's verdict: true for anything but VK_SUCCESS, and a lost device ends
/// the run there, as the handler the renderer gives it says (bridge::request_exit).
pub fn failed(result: vk::Result, at: &CStr) -> bool {
    mw2_shaders::pipeline::failed(result, at)
}

/// The shared pipeline's queue lock held: the queue the renderer submits to is externally
/// synchronised, for a present as for a submission.
pub struct QueueLock {
    _held: std::sync::MutexGuard<'static, ()>,
}

impl QueueLock {
    pub fn hold() -> QueueLock {
        let queue = mw2_shaders::pipeline::queue_mutex();
        QueueLock { _held: queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner) }
    }
}
