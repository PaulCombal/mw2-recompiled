// The Rust-renderer build's C++ forwarders (runtime/rust_renderer.cpp), compiled again
// under namespaces of their own: what the renderer calls of the presenter, gpu_time and
// capture, run in the rig where that needs no window and no device -- the presenter not
// started, gpu_time off, so a submission goes straight to the vkQueueSubmit the forwarder
// passes, which presenter_rig.h renames to the stand-in that records it.
#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <vector>

#include "presenter_rig.h"

#define MW2_RUST_RENDERER 1
#define gpu gpu_fwd
#define vk vk_fwd
#include "../../runtime/rust_renderer.cpp"
#undef vk
#undef gpu

namespace
{
    template <class T> T Fake(uint64_t value) { return reinterpret_cast<T>(uintptr_t(value)); }
}

extern "C" bool fwd_abi(char* why, size_t size)
{
    auto fail = [&](const char* what) {
        std::snprintf(why, size, "%s", what);
        return false;
    };
    const VkCommandBuffer buffers[3] = { Fake<VkCommandBuffer>(0xC001), Fake<VkCommandBuffer>(0xC002),
                                         Fake<VkCommandBuffer>(0xC003) };
    const VkSemaphore waits[1] = { Fake<VkSemaphore>(0xC010) };
    const VkPipelineStageFlags stages[1] = { VK_PIPELINE_STAGE_TRANSFER_BIT };
    const VkSemaphore signals[2] = { Fake<VkSemaphore>(0xC020), Fake<VkSemaphore>(0xC021) };
    VkSubmitInfo submit{ VK_STRUCTURE_TYPE_SUBMIT_INFO };
    submit.waitSemaphoreCount = 1;
    submit.pWaitSemaphores = waits;
    submit.pWaitDstStageMask = stages;
    submit.commandBufferCount = 3;
    submit.pCommandBuffers = buffers;
    submit.signalSemaphoreCount = 2;
    submit.pSignalSemaphores = signals;
    rig::s.batch.clear();
    rig::s.submits = 0;
    rig::s.queueSubmit = VK_ERROR_DEVICE_LOST;
    const VkResult result = vk_fwd::gpu_time::Submit(Fake<VkQueue>(0xC030), submit, Fake<VkFence>(0xC040));
    if (result != VK_ERROR_DEVICE_LOST) return fail("gpu_time::Submit's result did not come back");
    if (rig::s.submits != 1) return fail("gpu_time::Submit did not submit once");
    if (rig::s.batch != std::vector<VkCommandBuffer>(buffers, buffers + 3)) return fail("the command buffers changed");
    vk_fwd::gpu_time::Collect();
    if (vk_fwd::Running()) return fail("Running() without a start");
    if (vk_fwd::PresentedFrames() != 0) return fail("PresentedFrames() without a start");
    if (vk_fwd::GuestBlankPeriod() != 0) return fail("GuestBlankPeriod() without a display");
    vk_fwd::ShowImage(Fake<void*>(0xC050), 1280, 720, 1);
    vk_fwd::WaitUntilTaken(1);
    vk_fwd::capture::FrameBoundary(false, 3);
    vk_fwd::capture::Report();
    return true;
}
