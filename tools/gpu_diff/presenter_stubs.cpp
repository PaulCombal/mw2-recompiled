// The rig_ stand-ins presenter_rig.h renames the C++ originals' Vulkan and SDL calls to:
// each records what it was given in rig::s and answers what the case put there. The
// Rust reaches the same ones: through ash, whose tables rig_vkGetInstanceProcAddr fills
// with them, and the window's size through presenter.rs's rig module. Each command, and each
// object made or destroyed, is also written to rig::s.trace with every argument that is
// not a pointer, so that the two sides' calls can be held against each other whole.
#include "presenter_rig.h"

#include <algorithm>
#include <cstring>
#include <initializer_list>

namespace rig
{
    Stubs s;
    int64_t now = 0;
    uint32_t notifies = 0;

    template <class T> T Handle() { return reinterpret_cast<T>(uintptr_t(s.nextHandle++)); }
}

using rig::s;

namespace
{
    template <class T> uint64_t H(T handle) { return uint64_t(uintptr_t(handle)); }
    uint64_t F(float f) { uint32_t b; std::memcpy(&b, &f, 4); return b; }
    void T(std::initializer_list<uint64_t> words)
    {
        if (s.tracing) s.trace.insert(s.trace.end(), words);
    }
    void Layers(const VkImageSubresourceLayers& l) { T({ l.aspectMask, l.mipLevel, l.baseArrayLayer, l.layerCount }); }
    void Range(const VkImageSubresourceRange& r)
    {
        T({ r.aspectMask, r.baseMipLevel, r.levelCount, r.baseArrayLayer, r.layerCount });
    }
    void Offset(const VkOffset3D& o) { T({ uint32_t(o.x), uint32_t(o.y), uint32_t(o.z) }); }
    enum Call : uint64_t
    {
        kCaps = 0xCA11000, kFormats, kCreateSwapchain, kDestroySwapchain, kSwapchainImages, kWindowSize, kWait, kAcquire,
        kResetFences, kResetCommands, kBegin, kEnd, kBarrier, kClear, kBlit, kPresent, kCreateImage, kAllocate, kBindImage,
        kCreateView, kDestroyView, kDestroyImage, kFree, kUpdateSets, kBindPipeline, kViewport, kScissor, kBindSets, kPush,
        kDraw, kSubmit, kBeginRendering, kEndRendering,
    };
}

extern "C"
{
    VkResult rig_vkGetPhysicalDeviceSurfaceCapabilitiesKHR(VkPhysicalDevice, VkSurfaceKHR, VkSurfaceCapabilitiesKHR* caps)
    {
        T({ kCaps });
        *caps = s.caps;
        return VK_SUCCESS;
    }
    VkResult rig_vkGetPhysicalDeviceSurfaceFormatsKHR(VkPhysicalDevice, VkSurfaceKHR, uint32_t* count, VkSurfaceFormatKHR* formats)
    {
        s.formatsAsked = true;
        T({ kFormats, formats ? *count : ~0ull });
        if (!formats) { *count = uint32_t(s.formats.size()); return VK_SUCCESS; }
        *count = std::min(*count, uint32_t(s.formats.size()));
        std::copy_n(s.formats.begin(), *count, formats);
        return VK_SUCCESS;
    }
    VkResult rig_vkCreateSwapchainKHR(VkDevice, const VkSwapchainCreateInfoKHR* info, const VkAllocationCallbacks*,
                                      VkSwapchainKHR* made)
    {
        s.created = true;
        s.createInfo = *info;
        T({ kCreateSwapchain, H(info->pNext), info->flags, H(info->surface), info->minImageCount, uint64_t(info->imageFormat),
            uint64_t(info->imageColorSpace), info->imageExtent.width, info->imageExtent.height, info->imageArrayLayers,
            info->imageUsage, uint64_t(info->imageSharingMode), info->queueFamilyIndexCount, uint64_t(info->preTransform),
            uint64_t(info->compositeAlpha), uint64_t(info->presentMode), info->clipped, H(info->oldSwapchain) });
        if (s.createSwapchain == VK_SUCCESS) *made = rig::Handle<VkSwapchainKHR>();
        return s.createSwapchain;
    }
    void rig_vkDestroySwapchainKHR(VkDevice, VkSwapchainKHR swapchain, const VkAllocationCallbacks*)
    {
        s.destroyedSwapchains++;
        T({ kDestroySwapchain, H(swapchain) });
    }
    VkResult rig_vkGetSwapchainImagesKHR(VkDevice, VkSwapchainKHR swapchain, uint32_t* count, VkImage* images)
    {
        T({ kSwapchainImages, H(swapchain), images ? *count : ~0ull });
        if (!images) { *count = 3; return VK_SUCCESS; }
        for (uint32_t i = 0; i < *count; i++) images[i] = rig::Handle<VkImage>();
        return VK_SUCCESS;
    }
    bool rig_SDL_GetWindowSizeInPixels(SDL_Window*, int* width, int* height)
    {
        s.windowAsked = true;
        T({ kWindowSize });
        *width = s.windowWidth;
        *height = s.windowHeight;
        return true;
    }
    SDL_DisplayID rig_SDL_GetDisplayForWindow(SDL_Window*) { return 1; }
    const SDL_DisplayMode* rig_SDL_GetCurrentDisplayMode(SDL_DisplayID) { return s.hasMode ? &s.mode : nullptr; }

    VkResult rig_vkWaitForFences(VkDevice, uint32_t count, const VkFence* fences, VkBool32 all, uint64_t timeout)
    {
        T({ kWait, count, all, timeout });
        for (uint32_t i = 0; i < count; i++) T({ H(fences[i]) });
        if (count == 2) s.remakes++;
        return s.waitFences;
    }
    VkResult rig_vkAcquireNextImageKHR(VkDevice, VkSwapchainKHR swapchain, uint64_t timeout, VkSemaphore semaphore,
                                       VkFence fence, uint32_t* index)
    {
        T({ kAcquire, H(swapchain), timeout, H(semaphore), H(fence) });
        *index = s.imageIndex;
        return s.acquire;
    }
    VkResult rig_vkResetFences(VkDevice, uint32_t count, const VkFence* fences)
    {
        T({ kResetFences, count });
        for (uint32_t i = 0; i < count; i++) T({ H(fences[i]) });
        return VK_SUCCESS;
    }
    VkResult rig_vkResetCommandBuffer(VkCommandBuffer commands, VkCommandBufferResetFlags flags)
    {
        T({ kResetCommands, H(commands), flags });
        return VK_SUCCESS;
    }
    VkResult rig_vkBeginCommandBuffer(VkCommandBuffer commands, const VkCommandBufferBeginInfo* info)
    {
        s.begun++;
        T({ kBegin, H(commands), H(info->pNext), info->flags, H(info->pInheritanceInfo) });
        return VK_SUCCESS;
    }
    VkResult rig_vkEndCommandBuffer(VkCommandBuffer commands)
    {
        T({ kEnd, H(commands) });
        return VK_SUCCESS;
    }
    void rig_vkCmdPipelineBarrier(VkCommandBuffer commands, VkPipelineStageFlags from, VkPipelineStageFlags to,
                                  VkDependencyFlags dependency, uint32_t memoryCount, const VkMemoryBarrier*,
                                  uint32_t bufferCount, const VkBufferMemoryBarrier*, uint32_t imageCount,
                                  const VkImageMemoryBarrier* images)
    {
        T({ kBarrier, H(commands), from, to, dependency, memoryCount, bufferCount, imageCount });
        for (uint32_t i = 0; i < imageCount; i++)
        {
            const VkImageMemoryBarrier& b = images[i];
            T({ H(b.pNext), b.srcAccessMask, b.dstAccessMask, uint64_t(b.oldLayout), uint64_t(b.newLayout),
                b.srcQueueFamilyIndex, b.dstQueueFamilyIndex, H(b.image) });
            Range(b.subresourceRange);
        }
    }
    void rig_vkCmdClearColorImage(VkCommandBuffer commands, VkImage image, VkImageLayout layout,
                                  const VkClearColorValue* colour, uint32_t count, const VkImageSubresourceRange* ranges)
    {
        s.clears++;
        T({ kClear, H(commands), H(image), uint64_t(layout), colour->uint32[0], colour->uint32[1], colour->uint32[2],
            colour->uint32[3], count });
        for (uint32_t i = 0; i < count; i++) Range(ranges[i]);
    }
    void rig_vkCmdBlitImage(VkCommandBuffer commands, VkImage source, VkImageLayout sourceLayout, VkImage target,
                            VkImageLayout targetLayout, uint32_t count, const VkImageBlit* regions, VkFilter filter)
    {
        s.blits++;
        s.blitSource = uint64_t(uintptr_t(source));
        T({ kBlit, H(commands), H(source), uint64_t(sourceLayout), H(target), uint64_t(targetLayout), count,
            uint64_t(filter) });
        for (uint32_t i = 0; i < count; i++)
        {
            Layers(regions[i].srcSubresource);
            Offset(regions[i].srcOffsets[0]);
            Offset(regions[i].srcOffsets[1]);
            Layers(regions[i].dstSubresource);
            Offset(regions[i].dstOffsets[0]);
            Offset(regions[i].dstOffsets[1]);
        }
    }
    VkResult rig_vkQueuePresentKHR(VkQueue queue, const VkPresentInfoKHR* present)
    {
        s.presents++;
        const auto* id = static_cast<const VkPresentIdKHR*>(present->pNext);
        s.hasId = id && id->sType == VK_STRUCTURE_TYPE_PRESENT_ID_KHR;
        s.presentId = s.hasId ? id->pPresentIds[0] : 0;
        T({ kPresent, H(queue), present->waitSemaphoreCount, present->swapchainCount, H(present->pResults), s.hasId,
            s.hasId ? id->swapchainCount : 0, s.presentId, s.hasId ? H(id->pNext) : 0 });
        for (uint32_t i = 0; i < present->waitSemaphoreCount; i++) T({ H(present->pWaitSemaphores[i]) });
        for (uint32_t i = 0; i < present->swapchainCount; i++)
            T({ H(present->pSwapchains[i]), present->pImageIndices[i] });
        return s.present;
    }

    VkResult rig_vkCreateImage(VkDevice, const VkImageCreateInfo* info, const VkAllocationCallbacks*, VkImage* image)
    {
        s.imagesMade++;
        s.imageExtent = info->extent;
        s.mipLevels = info->mipLevels;
        *image = rig::Handle<VkImage>();
        T({ kCreateImage, H(info->pNext), info->flags, uint64_t(info->imageType), uint64_t(info->format), info->extent.width,
            info->extent.height, info->extent.depth, info->mipLevels, info->arrayLayers, uint64_t(info->samples),
            uint64_t(info->tiling), info->usage, uint64_t(info->sharingMode), info->queueFamilyIndexCount,
            uint64_t(info->initialLayout), H(*image) });
        return VK_SUCCESS;
    }
    void rig_vkGetImageMemoryRequirements(VkDevice, VkImage, VkMemoryRequirements* needs)
    {
        *needs = { 1 << 20, 256, 1 };
    }
    void rig_vkGetPhysicalDeviceMemoryProperties(VkPhysicalDevice, VkPhysicalDeviceMemoryProperties* memory)
    {
        *memory = {};
        memory->memoryTypeCount = 1;
        memory->memoryTypes[0].propertyFlags = VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT | VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT |
                                               VK_MEMORY_PROPERTY_HOST_COHERENT_BIT;
        memory->memoryHeapCount = 1;
    }
    VkResult rig_vkAllocateMemory(VkDevice, const VkMemoryAllocateInfo* info, const VkAllocationCallbacks*,
                                  VkDeviceMemory* memory)
    {
        *memory = rig::Handle<VkDeviceMemory>();
        T({ kAllocate, H(info->pNext), info->allocationSize, info->memoryTypeIndex, H(*memory) });
        return VK_SUCCESS;
    }
    VkResult rig_vkBindImageMemory(VkDevice, VkImage image, VkDeviceMemory memory, VkDeviceSize offset)
    {
        T({ kBindImage, H(image), H(memory), offset });
        return VK_SUCCESS;
    }
    VkResult rig_vkCreateImageView(VkDevice, const VkImageViewCreateInfo* info, const VkAllocationCallbacks*, VkImageView* view)
    {
        *view = rig::Handle<VkImageView>();
        const VkComponentMapping& c = info->components;
        T({ kCreateView, H(info->pNext), info->flags, H(info->image), uint64_t(info->viewType), uint64_t(info->format),
            uint64_t(c.r), uint64_t(c.g), uint64_t(c.b), uint64_t(c.a) });
        Range(info->subresourceRange);
        T({ H(*view) });
        return VK_SUCCESS;
    }
    void rig_vkDestroyImageView(VkDevice, VkImageView view, const VkAllocationCallbacks*) { T({ kDestroyView, H(view) }); }
    void rig_vkDestroyImage(VkDevice, VkImage image, const VkAllocationCallbacks*) { T({ kDestroyImage, H(image) }); }
    void rig_vkFreeMemory(VkDevice, VkDeviceMemory memory, const VkAllocationCallbacks*) { T({ kFree, H(memory) }); }
    void rig_vkUpdateDescriptorSets(VkDevice, uint32_t count, const VkWriteDescriptorSet* writes, uint32_t copies,
                                    const VkCopyDescriptorSet*)
    {
        T({ kUpdateSets, count, copies });
        for (uint32_t i = 0; i < count; i++)
        {
            const VkWriteDescriptorSet& w = writes[i];
            T({ H(w.pNext), H(w.dstSet), w.dstBinding, w.dstArrayElement, w.descriptorCount, uint64_t(w.descriptorType),
                H(w.pBufferInfo), H(w.pTexelBufferView) });
            for (uint32_t k = 0; k < w.descriptorCount && w.pImageInfo; k++)
                T({ H(w.pImageInfo[k].sampler), H(w.pImageInfo[k].imageView), uint64_t(w.pImageInfo[k].imageLayout) });
        }
    }
    void rig_vkCmdBindPipeline(VkCommandBuffer commands, VkPipelineBindPoint point, VkPipeline pipeline)
    {
        T({ kBindPipeline, H(commands), uint64_t(point), H(pipeline) });
    }
    void rig_vkCmdSetViewport(VkCommandBuffer commands, uint32_t first, uint32_t count, const VkViewport* viewports)
    {
        T({ kViewport, H(commands), first, count });
        for (uint32_t i = 0; i < count; i++)
        {
            const VkViewport& v = viewports[i];
            T({ F(v.x), F(v.y), F(v.width), F(v.height), F(v.minDepth), F(v.maxDepth) });
        }
    }
    void rig_vkCmdSetScissor(VkCommandBuffer commands, uint32_t first, uint32_t count, const VkRect2D* scissors)
    {
        T({ kScissor, H(commands), first, count });
        for (uint32_t i = 0; i < count; i++)
        {
            const VkRect2D& r = scissors[i];
            T({ uint32_t(r.offset.x), uint32_t(r.offset.y), r.extent.width, r.extent.height });
        }
    }
    void rig_vkCmdBindDescriptorSets(VkCommandBuffer commands, VkPipelineBindPoint point, VkPipelineLayout layout,
                                     uint32_t first, uint32_t count, const VkDescriptorSet* sets, uint32_t dynamicCount,
                                     const uint32_t*)
    {
        T({ kBindSets, H(commands), uint64_t(point), H(layout), first, count, dynamicCount });
        for (uint32_t i = 0; i < count; i++) T({ H(sets[i]) });
    }
    void rig_vkCmdPushConstants(VkCommandBuffer commands, VkPipelineLayout layout, VkShaderStageFlags stages, uint32_t offset,
                                uint32_t size, const void* values)
    {
        s.pushes++;
        std::memcpy(reinterpret_cast<char*>(s.push) + offset, values, std::min<size_t>(size, sizeof s.push - offset));
        T({ kPush, H(commands), H(layout), stages, offset, size });
        for (uint32_t i = 0; i + 4 <= size; i += 4)
        {
            uint32_t word;
            std::memcpy(&word, static_cast<const char*>(values) + i, 4);
            T({ word });
        }
    }
    void rig_vkCmdDraw(VkCommandBuffer commands, uint32_t vertices, uint32_t instances, uint32_t firstVertex,
                       uint32_t firstInstance)
    {
        T({ kDraw, H(commands), vertices, instances, firstVertex, firstInstance });
    }
    void rig_vkCmdBeginRenderingKHR(VkCommandBuffer commands, const VkRenderingInfo* info)
    {
        T({ kBeginRendering, H(commands), H(info->pNext), info->flags, uint32_t(info->renderArea.offset.x),
            uint32_t(info->renderArea.offset.y), info->renderArea.extent.width, info->renderArea.extent.height,
            info->layerCount, info->viewMask, info->colorAttachmentCount, H(info->pDepthAttachment),
            H(info->pStencilAttachment) });
        for (uint32_t i = 0; i < info->colorAttachmentCount; i++)
        {
            const VkRenderingAttachmentInfo& a = info->pColorAttachments[i];
            T({ H(a.pNext), H(a.imageView), uint64_t(a.imageLayout), uint64_t(a.resolveMode), H(a.resolveImageView),
                uint64_t(a.resolveImageLayout), uint64_t(a.loadOp), uint64_t(a.storeOp), a.clearValue.color.uint32[0],
                a.clearValue.color.uint32[1], a.clearValue.color.uint32[2], a.clearValue.color.uint32[3] });
        }
    }
    void rig_vkCmdEndRenderingKHR(VkCommandBuffer commands) { T({ kEndRendering, H(commands) }); }

    void rig_vkGetPhysicalDeviceQueueFamilyProperties(VkPhysicalDevice, uint32_t* count, VkQueueFamilyProperties* families)
    {
        if (!families) { *count = uint32_t(s.familyBits.size()); return; }
        for (uint32_t i = 0; i < *count && i < s.familyBits.size(); i++)
        {
            families[i] = {};
            families[i].queueFlags = VK_QUEUE_GRAPHICS_BIT;
            families[i].queueCount = 1;
            families[i].timestampValidBits = s.familyBits[i];
        }
    }
    void rig_vkGetPhysicalDeviceProperties(VkPhysicalDevice, VkPhysicalDeviceProperties* properties)
    {
        *properties = {};
        properties->limits.timestampPeriod = s.timestampPeriod;
    }
    VkResult rig_vkCreateQueryPool(VkDevice, const VkQueryPoolCreateInfo*, const VkAllocationCallbacks*, VkQueryPool* pool)
    {
        *pool = rig::Handle<VkQueryPool>();
        return VK_SUCCESS;
    }
    VkResult rig_vkCreateCommandPool(VkDevice, const VkCommandPoolCreateInfo*, const VkAllocationCallbacks*, VkCommandPool* pool)
    {
        *pool = rig::Handle<VkCommandPool>();
        return VK_SUCCESS;
    }
    VkResult rig_vkAllocateCommandBuffers(VkDevice, const VkCommandBufferAllocateInfo* info, VkCommandBuffer* buffers)
    {
        for (uint32_t i = 0; i < info->commandBufferCount; i++) buffers[i] = rig::Handle<VkCommandBuffer>();
        return VK_SUCCESS;
    }
    void rig_vkCmdResetQueryPool(VkCommandBuffer, VkQueryPool, uint32_t, uint32_t) {}
    void rig_vkCmdWriteTimestamp(VkCommandBuffer, VkPipelineStageFlagBits, VkQueryPool, uint32_t) {}
    // A submission as the trace has it, for the C++ presenter's gpu_time::Submit stand-in too.
    void rig_trace_submit(VkQueue queue, uint32_t count, const VkSubmitInfo* submits, VkFence fence)
    {
        T({ kSubmit, H(queue), count, H(fence) });
        for (uint32_t i = 0; i < count; i++)
        {
            const VkSubmitInfo& b = submits[i];
            T({ H(b.pNext), b.waitSemaphoreCount, b.commandBufferCount, b.signalSemaphoreCount });
            for (uint32_t k = 0; k < b.waitSemaphoreCount; k++) T({ H(b.pWaitSemaphores[k]), b.pWaitDstStageMask[k] });
            for (uint32_t k = 0; k < b.commandBufferCount; k++) T({ H(b.pCommandBuffers[k]) });
            for (uint32_t k = 0; k < b.signalSemaphoreCount; k++) T({ H(b.pSignalSemaphores[k]) });
        }
    }
    VkResult rig_vkQueueSubmit(VkQueue queue, uint32_t count, const VkSubmitInfo* submit, VkFence fence)
    {
        rig_trace_submit(queue, count, submit, fence);
        s.submits++;
        s.batch.assign(submit->pCommandBuffers, submit->pCommandBuffers + submit->commandBufferCount);
        return s.queueSubmit;
    }
    VkResult rig_vkQueueWaitIdle(VkQueue) { return VK_SUCCESS; }
    void rig_vkFreeCommandBuffers(VkDevice, VkCommandPool, uint32_t, const VkCommandBuffer*) {}
    void rig_vkDestroyCommandPool(VkDevice, VkCommandPool, const VkAllocationCallbacks*) {}
    void rig_vkDestroyQueryPool(VkDevice, VkQueryPool, const VkAllocationCallbacks*) {}
    VkResult rig_vkGetQueryPoolResults(VkDevice, VkQueryPool, uint32_t first, uint32_t count, size_t size, void* data,
                                       VkDeviceSize stride, VkQueryResultFlags)
    {
        auto* out = static_cast<uint64_t*>(data);
        for (uint32_t i = 0; i < count && (i + 1) * stride <= size; i++)
        {
            out[i * stride / 8] = s.query[first + i][0];
            out[i * stride / 8 + 1] = s.query[first + i][1];
        }
        return s.queryResult[first / 2];
    }
}


// What the harness reads and sets of the stand-ins.
extern "C"
{
    void rig_trace_start(uint64_t nextHandle)
    {
        s.trace.clear();
        s.tracing = true;
        s.nextHandle = nextHandle;
    }
    // The trace since rig_trace_start, which then stops.
    size_t rig_trace_take(uint64_t* out, size_t capacity)
    {
        s.tracing = false;
        std::copy_n(s.trace.begin(), std::min(capacity, s.trace.size()), out);
        return s.trace.size();
    }
    int64_t rig_now() { return rig::now; }
}

// The ash tables the Rust presenter and backdrop are given in the rig: every command the
// stand-ins answer, by name; anything else is null, which ash turns into a panic.
extern "C" PFN_vkVoidFunction rig_vkGetDeviceProcAddr(VkDevice, const char* name);
extern "C" PFN_vkVoidFunction rig_vkGetInstanceProcAddr(VkInstance, const char* name)
{
    struct Entry { const char* name; PFN_vkVoidFunction function; };
#define E(name) { #name, reinterpret_cast<PFN_vkVoidFunction>(rig_##name) }
    static const Entry kTable[] = {
        E(vkGetInstanceProcAddr), E(vkGetDeviceProcAddr), E(vkGetPhysicalDeviceSurfaceCapabilitiesKHR),
        E(vkGetPhysicalDeviceSurfaceFormatsKHR), E(vkCreateSwapchainKHR), E(vkDestroySwapchainKHR),
        E(vkGetSwapchainImagesKHR), E(vkWaitForFences), E(vkAcquireNextImageKHR), E(vkResetFences),
        E(vkResetCommandBuffer), E(vkBeginCommandBuffer), E(vkEndCommandBuffer), E(vkCmdPipelineBarrier),
        E(vkCmdClearColorImage), E(vkCmdBlitImage), E(vkQueuePresentKHR), E(vkCreateImage), E(vkGetImageMemoryRequirements),
        E(vkGetPhysicalDeviceMemoryProperties), E(vkAllocateMemory), E(vkBindImageMemory), E(vkCreateImageView),
        E(vkDestroyImageView), E(vkDestroyImage), E(vkFreeMemory), E(vkUpdateDescriptorSets), E(vkCmdBindPipeline),
        E(vkCmdSetViewport), E(vkCmdSetScissor), E(vkCmdBindDescriptorSets), E(vkCmdPushConstants), E(vkCmdDraw),
        E(vkCmdBeginRenderingKHR), E(vkCmdEndRenderingKHR), E(vkGetPhysicalDeviceQueueFamilyProperties),
        E(vkGetPhysicalDeviceProperties), E(vkQueueSubmit), E(vkQueueWaitIdle),
    };
#undef E
    for (const Entry& e : kTable)
        if (std::strcmp(e.name, name) == 0) return e.function;
    return nullptr;
}
extern "C" PFN_vkVoidFunction rig_vkGetDeviceProcAddr(VkDevice, const char* name)
{
    return rig_vkGetInstanceProcAddr(VK_NULL_HANDLE, name);
}

