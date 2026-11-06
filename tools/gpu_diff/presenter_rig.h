// tools/gpu_diff/presenter.cpp's stand-ins for what the presenter's, the backdrop's and
// gpu_time's C++ call: each Vulkan and SDL function they reach in the rig is renamed to a
// rig_ one (presenter_stubs.cpp) that records its arguments and returns what the case
// says, so the C++ originals run with no device and no window. Included after the system
// headers and before the original .cpp, whose calls the #defines rename.
#pragma once
#include <SDL3/SDL.h>
#include <SDL3/SDL_vulkan.h>
#include <vulkan/vulkan.h>

#include <chrono>
#include <cstdint>
#include <vector>

namespace rig
{
    struct Stubs
    {
        // vkGetPhysicalDeviceSurface*, SDL_GetWindowSizeInPixels, vkCreateSwapchainKHR.
        VkSurfaceCapabilitiesKHR caps{};
        int windowWidth = 0, windowHeight = 0;
        bool windowAsked = false, formatsAsked = false;
        std::vector<VkSurfaceFormatKHR> formats;
        VkResult createSwapchain = VK_SUCCESS;
        bool created = false;
        VkSwapchainCreateInfoKHR createInfo{};
        uint32_t destroyedSwapchains = 0;
        uint64_t nextHandle = 0x10000;
        // The display mode.
        bool hasMode = false;
        SDL_DisplayMode mode{};
        // A frame of the window.
        VkResult waitFences = VK_SUCCESS, acquire = VK_SUCCESS, present = VK_SUCCESS;
        uint32_t imageIndex = 0;
        uint32_t remakes = 0;       // waits on both of the presenter's fences: RemakeSwapchain
        uint32_t begun = 0, blits = 0, clears = 0;
        uint64_t blitSource = 0;
        uint32_t presents = 0;
        bool hasId = false;
        uint64_t presentId = 0;
        // The backdrop.
        float push[16]{};
        uint32_t pushes = 0;
        VkExtent3D imageExtent{};
        uint32_t mipLevels = 0, imagesMade = 0;
        // gpu_time.
        std::vector<uint32_t> familyBits;
        float timestampPeriod = 1.0f;
        VkResult queueSubmit = VK_SUCCESS;
        uint32_t submits = 0;
        std::vector<VkCommandBuffer> batch;
        uint64_t query[128][2]{};   // value, available
        VkResult queryResult[64]{};
        // Every call's arguments, while tracing (presenter_stubs.cpp).
        bool tracing = false;
        std::vector<uint64_t> trace;
    };
    extern Stubs s;
    // The clock the presenter's steady_clock reads, in nanoseconds.
    extern int64_t now;
    extern uint32_t notifies;
}

// A steady clock that moves only when the rig moves it.
namespace std::chrono
{
    struct rig_steady_clock
    {
        using duration = std::chrono::nanoseconds;
        using rep = duration::rep;
        using period = duration::period;
        using time_point = std::chrono::time_point<rig_steady_clock>;
        static constexpr bool is_steady = true;
        static time_point now() noexcept { return time_point(duration(rig::now)); }
    };
}

#define DECLARE(ret, name, ...) extern "C" ret rig_##name(__VA_ARGS__);
DECLARE(VkResult, vkGetPhysicalDeviceSurfaceCapabilitiesKHR, VkPhysicalDevice, VkSurfaceKHR, VkSurfaceCapabilitiesKHR*)
DECLARE(VkResult, vkGetPhysicalDeviceSurfaceFormatsKHR, VkPhysicalDevice, VkSurfaceKHR, uint32_t*, VkSurfaceFormatKHR*)
DECLARE(VkResult, vkCreateSwapchainKHR, VkDevice, const VkSwapchainCreateInfoKHR*, const VkAllocationCallbacks*, VkSwapchainKHR*)
DECLARE(void, vkDestroySwapchainKHR, VkDevice, VkSwapchainKHR, const VkAllocationCallbacks*)
DECLARE(VkResult, vkGetSwapchainImagesKHR, VkDevice, VkSwapchainKHR, uint32_t*, VkImage*)
DECLARE(bool, SDL_GetWindowSizeInPixels, SDL_Window*, int*, int*)
DECLARE(SDL_DisplayID, SDL_GetDisplayForWindow, SDL_Window*)
DECLARE(const SDL_DisplayMode*, SDL_GetCurrentDisplayMode, SDL_DisplayID)
DECLARE(VkResult, vkWaitForFences, VkDevice, uint32_t, const VkFence*, VkBool32, uint64_t)
DECLARE(VkResult, vkAcquireNextImageKHR, VkDevice, VkSwapchainKHR, uint64_t, VkSemaphore, VkFence, uint32_t*)
DECLARE(VkResult, vkResetFences, VkDevice, uint32_t, const VkFence*)
DECLARE(VkResult, vkResetCommandBuffer, VkCommandBuffer, VkCommandBufferResetFlags)
DECLARE(VkResult, vkBeginCommandBuffer, VkCommandBuffer, const VkCommandBufferBeginInfo*)
DECLARE(VkResult, vkEndCommandBuffer, VkCommandBuffer)
DECLARE(void, vkCmdPipelineBarrier, VkCommandBuffer, VkPipelineStageFlags, VkPipelineStageFlags, VkDependencyFlags,
        uint32_t, const VkMemoryBarrier*, uint32_t, const VkBufferMemoryBarrier*, uint32_t, const VkImageMemoryBarrier*)
DECLARE(void, vkCmdClearColorImage, VkCommandBuffer, VkImage, VkImageLayout, const VkClearColorValue*, uint32_t,
        const VkImageSubresourceRange*)
DECLARE(void, vkCmdBlitImage, VkCommandBuffer, VkImage, VkImageLayout, VkImage, VkImageLayout, uint32_t, const VkImageBlit*,
        VkFilter)
DECLARE(VkResult, vkQueuePresentKHR, VkQueue, const VkPresentInfoKHR*)
DECLARE(VkResult, vkCreateImage, VkDevice, const VkImageCreateInfo*, const VkAllocationCallbacks*, VkImage*)
DECLARE(void, vkGetImageMemoryRequirements, VkDevice, VkImage, VkMemoryRequirements*)
DECLARE(void, vkGetPhysicalDeviceMemoryProperties, VkPhysicalDevice, VkPhysicalDeviceMemoryProperties*)
DECLARE(VkResult, vkAllocateMemory, VkDevice, const VkMemoryAllocateInfo*, const VkAllocationCallbacks*, VkDeviceMemory*)
DECLARE(VkResult, vkBindImageMemory, VkDevice, VkImage, VkDeviceMemory, VkDeviceSize)
DECLARE(VkResult, vkCreateImageView, VkDevice, const VkImageViewCreateInfo*, const VkAllocationCallbacks*, VkImageView*)
DECLARE(void, vkDestroyImageView, VkDevice, VkImageView, const VkAllocationCallbacks*)
DECLARE(void, vkDestroyImage, VkDevice, VkImage, const VkAllocationCallbacks*)
DECLARE(void, vkFreeMemory, VkDevice, VkDeviceMemory, const VkAllocationCallbacks*)
DECLARE(void, vkUpdateDescriptorSets, VkDevice, uint32_t, const VkWriteDescriptorSet*, uint32_t, const VkCopyDescriptorSet*)
DECLARE(void, vkCmdBindPipeline, VkCommandBuffer, VkPipelineBindPoint, VkPipeline)
DECLARE(void, vkCmdSetViewport, VkCommandBuffer, uint32_t, uint32_t, const VkViewport*)
DECLARE(void, vkCmdSetScissor, VkCommandBuffer, uint32_t, uint32_t, const VkRect2D*)
DECLARE(void, vkCmdBindDescriptorSets, VkCommandBuffer, VkPipelineBindPoint, VkPipelineLayout, uint32_t, uint32_t,
        const VkDescriptorSet*, uint32_t, const uint32_t*)
DECLARE(void, vkCmdPushConstants, VkCommandBuffer, VkPipelineLayout, VkShaderStageFlags, uint32_t, uint32_t, const void*)
DECLARE(void, vkCmdDraw, VkCommandBuffer, uint32_t, uint32_t, uint32_t, uint32_t)
DECLARE(void, vkGetPhysicalDeviceQueueFamilyProperties, VkPhysicalDevice, uint32_t*, VkQueueFamilyProperties*)
DECLARE(void, vkGetPhysicalDeviceProperties, VkPhysicalDevice, VkPhysicalDeviceProperties*)
DECLARE(VkResult, vkCreateQueryPool, VkDevice, const VkQueryPoolCreateInfo*, const VkAllocationCallbacks*, VkQueryPool*)
DECLARE(VkResult, vkCreateCommandPool, VkDevice, const VkCommandPoolCreateInfo*, const VkAllocationCallbacks*, VkCommandPool*)
DECLARE(VkResult, vkAllocateCommandBuffers, VkDevice, const VkCommandBufferAllocateInfo*, VkCommandBuffer*)
DECLARE(void, vkCmdResetQueryPool, VkCommandBuffer, VkQueryPool, uint32_t, uint32_t)
DECLARE(void, vkCmdWriteTimestamp, VkCommandBuffer, VkPipelineStageFlagBits, VkQueryPool, uint32_t)
DECLARE(VkResult, vkQueueSubmit, VkQueue, uint32_t, const VkSubmitInfo*, VkFence)
DECLARE(VkResult, vkQueueWaitIdle, VkQueue)
DECLARE(void, vkFreeCommandBuffers, VkDevice, VkCommandPool, uint32_t, const VkCommandBuffer*)
DECLARE(void, vkDestroyCommandPool, VkDevice, VkCommandPool, const VkAllocationCallbacks*)
DECLARE(void, vkDestroyQueryPool, VkDevice, VkQueryPool, const VkAllocationCallbacks*)
DECLARE(void, vkCmdBeginRenderingKHR, VkCommandBuffer, const VkRenderingInfo*)
DECLARE(void, vkCmdEndRenderingKHR, VkCommandBuffer)
DECLARE(VkResult, vkGetQueryPoolResults, VkDevice, VkQueryPool, uint32_t, uint32_t, size_t, void*, VkDeviceSize,
        VkQueryResultFlags)
#undef DECLARE
extern "C" void rig_trace_submit(VkQueue, uint32_t, const VkSubmitInfo*, VkFence);

#define vkGetPhysicalDeviceSurfaceCapabilitiesKHR rig_vkGetPhysicalDeviceSurfaceCapabilitiesKHR
#define vkGetPhysicalDeviceSurfaceFormatsKHR rig_vkGetPhysicalDeviceSurfaceFormatsKHR
#define vkCreateSwapchainKHR rig_vkCreateSwapchainKHR
#define vkDestroySwapchainKHR rig_vkDestroySwapchainKHR
#define vkGetSwapchainImagesKHR rig_vkGetSwapchainImagesKHR
#define SDL_GetWindowSizeInPixels rig_SDL_GetWindowSizeInPixels
#define SDL_GetDisplayForWindow rig_SDL_GetDisplayForWindow
#define SDL_GetCurrentDisplayMode rig_SDL_GetCurrentDisplayMode
#define vkWaitForFences rig_vkWaitForFences
#define vkAcquireNextImageKHR rig_vkAcquireNextImageKHR
#define vkResetFences rig_vkResetFences
#define vkResetCommandBuffer rig_vkResetCommandBuffer
#define vkBeginCommandBuffer rig_vkBeginCommandBuffer
#define vkEndCommandBuffer rig_vkEndCommandBuffer
#define vkCmdPipelineBarrier rig_vkCmdPipelineBarrier
#define vkCmdClearColorImage rig_vkCmdClearColorImage
#define vkCmdBlitImage rig_vkCmdBlitImage
#define vkQueuePresentKHR rig_vkQueuePresentKHR
#define vkCreateImage rig_vkCreateImage
#define vkGetImageMemoryRequirements rig_vkGetImageMemoryRequirements
#define vkGetPhysicalDeviceMemoryProperties rig_vkGetPhysicalDeviceMemoryProperties
#define vkAllocateMemory rig_vkAllocateMemory
#define vkBindImageMemory rig_vkBindImageMemory
#define vkCreateImageView rig_vkCreateImageView
#define vkDestroyImageView rig_vkDestroyImageView
#define vkDestroyImage rig_vkDestroyImage
#define vkFreeMemory rig_vkFreeMemory
#define vkUpdateDescriptorSets rig_vkUpdateDescriptorSets
#define vkCmdBindPipeline rig_vkCmdBindPipeline
#define vkCmdSetViewport rig_vkCmdSetViewport
#define vkCmdSetScissor rig_vkCmdSetScissor
#define vkCmdBindDescriptorSets rig_vkCmdBindDescriptorSets
#define vkCmdPushConstants rig_vkCmdPushConstants
#define vkCmdDraw rig_vkCmdDraw
#define vkGetPhysicalDeviceQueueFamilyProperties rig_vkGetPhysicalDeviceQueueFamilyProperties
#define vkGetPhysicalDeviceProperties rig_vkGetPhysicalDeviceProperties
#define vkCreateQueryPool rig_vkCreateQueryPool
#define vkCreateCommandPool rig_vkCreateCommandPool
#define vkAllocateCommandBuffers rig_vkAllocateCommandBuffers
#define vkCmdResetQueryPool rig_vkCmdResetQueryPool
#define vkCmdWriteTimestamp rig_vkCmdWriteTimestamp
#define vkQueueSubmit rig_vkQueueSubmit
#define vkQueueWaitIdle rig_vkQueueWaitIdle
#define vkFreeCommandBuffers rig_vkFreeCommandBuffers
#define vkDestroyCommandPool rig_vkDestroyCommandPool
#define vkDestroyQueryPool rig_vkDestroyQueryPool
#define vkGetQueryPoolResults rig_vkGetQueryPoolResults

// The log, counted rather than printed.
#include "log.h"
#undef LOGI
#undef LOGW
#undef LOGE
namespace rig { inline uint32_t warnings = 0; }
#define LOGI(...) ((void)0)
#define LOGW(...) ((void)rig::warnings++)
#define LOGE(...) ((void)0)
