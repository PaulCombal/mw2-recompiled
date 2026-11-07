// runtime/gpu/vulkan/ in Rust, a file at a time.
//
// The presenter, the backdrop, gpu_time and capture call Vulkan through ash, on the
// device the C++ pipeline.cpp makes (vk::pipeline::CreateDevice) and the instance the
// presenter makes, or pipeline.cpp's when there is no window: their tables are loaded
// from those handles here, and the extensions' only when the device enabled them.
// Each such file is unsafe by nature, and says what keeps its handles valid.
#![deny(unsafe_op_in_unsafe_fn)]

pub mod backdrop;
pub mod capture;
// The renderer's device tables and its Vulkan calls (device.rs), the recorder thread and
// queue (recorder.rs) and the command trace (trace.rs): the renderer's from S10, built and
// proven (tools/gpu_diff.py --rig recorder,device_trace) before anything calls them.
#[cfg_attr(not(mw2_owns_renderer), allow(dead_code))]
pub mod device;
pub mod gpu_time;
pub mod presenter;
#[cfg_attr(not(mw2_owns_recorder), allow(dead_code))]
pub mod recorder;
#[deny(unsafe_code)]
pub mod texture_formats;
#[cfg(feature = "diagnostics")]
#[cfg_attr(not(mw2_owns_trace), allow(dead_code))]
pub mod trace;
pub mod util;

use crate::runtime::bridge;
use ash::vk;
use ash::vk::Handle;
use std::ffi::CStr;
use std::sync::LazyLock;

/// The Vulkan loader, linked by name as the C++ links it (`vulkan`, `vulkan-1`).
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

/// vk::pipeline::Failed: true for anything but VK_SUCCESS, and a lost device ends the
/// run there, as the C++ renderer's handler says.
pub fn failed(result: vk::Result, at: &CStr) -> bool {
    bridge::vulkan::failed(result.as_raw(), at)
}

/// vk::pipeline::QueueMutex held: the queue the renderer submits to is externally
/// synchronised, for a present as for a submission.
pub struct QueueLock(());

impl QueueLock {
    pub fn hold() -> QueueLock {
        bridge::vulkan::queue_lock();
        QueueLock(())
    }
}

impl Drop for QueueLock {
    fn drop(&mut self) {
        bridge::vulkan::queue_unlock();
    }
}
