// Small Vulkan helpers the renderer, the presenter and the backdrop share: memory types,
// and a tracked image's barrier (tracked.rs).
//
// Unsafe by nature: every function records into a command buffer the caller holds in
// the recording state, on the device whose table it passes, and names images that
// device made and keeps alive until the commands have run.
#![deny(unsafe_op_in_unsafe_fn)]

use ash::vk;

/// The first memory type `allowed` permits that has every property in `want`, or
/// u32::MAX.
pub fn find_memory(memory: &vk::PhysicalDeviceMemoryProperties, allowed: u32, want: vk::MemoryPropertyFlags) -> u32 {
    for i in 0..memory.memory_type_count.min(vk::MAX_MEMORY_TYPES as u32) {
        if allowed & (1u32 << i) != 0 && memory.memory_types[i as usize].property_flags & want == want {
            return i;
        }
    }
    u32::MAX
}

/// A tracked image's transition (tracked.rs) on every level and layer of `image`.
///
/// # Safety
/// `commands` is recording on `device`, and `image` is that device's.
pub unsafe fn transition(
    device: &ash::Device,
    commands: vk::CommandBuffer,
    image: vk::Image,
    aspect: vk::ImageAspectFlags,
    t: &super::tracked::Transition,
) {
    let barriers = [t.image_barrier(image, aspect)];
    unsafe {
        device.cmd_pipeline_barrier(
            commands,
            t.source_stage,
            t.destination_stage,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &barriers,
        )
    };
}

/// The same through the renderer's device, so the command trace has it when the command
/// buffer is one the renderer made.
///
/// # Safety
/// As `transition`, on `device`.
pub unsafe fn renderer_transition(
    device: &super::device::Vk,
    commands: vk::CommandBuffer,
    image: vk::Image,
    aspect: vk::ImageAspectFlags,
    t: &super::tracked::Transition,
) {
    let barriers = [t.image_barrier(image, aspect)];
    unsafe {
        device.util_barrier(
            commands,
            t.source_stage,
            t.destination_stage,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &barriers,
        )
    };
}
