// Small Vulkan helpers the presenter and the backdrop share (runtime/gpu/vulkan/util.h,
// whose renderer users move with the renderer).
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

/// A layout transition of every mip level and array layer of `image`.
///
/// # Safety
/// `commands` is recording on `device`, and `image` is that device's.
#[allow(clippy::too_many_arguments)]
pub unsafe fn barrier(
    device: &ash::Device,
    commands: vk::CommandBuffer,
    image: vk::Image,
    aspect: vk::ImageAspectFlags,
    from: vk::ImageLayout,
    to: vk::ImageLayout,
    source_access: vk::AccessFlags,
    destination_access: vk::AccessFlags,
    source_stage: vk::PipelineStageFlags,
    destination_stage: vk::PipelineStageFlags,
) {
    let barriers = [vk::ImageMemoryBarrier {
        src_access_mask: source_access,
        dst_access_mask: destination_access,
        old_layout: from,
        new_layout: to,
        src_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
        dst_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
        image,
        subresource_range: vk::ImageSubresourceRange {
            aspect_mask: aspect,
            base_mip_level: 0,
            level_count: vk::REMAINING_MIP_LEVELS,
            base_array_layer: 0,
            layer_count: vk::REMAINING_ARRAY_LAYERS,
        },
        ..Default::default()
    }];
    unsafe {
        device.cmd_pipeline_barrier(
            commands,
            source_stage,
            destination_stage,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &barriers,
        )
    };
}
