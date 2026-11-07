// tools/gpu_diff/recorder.cpp's stand-in Vulkan: every command the C++ trace's wrappers
// (trace.cpp, which calls the global vk* names) and the Rust Vk (device.rs, whose tables
// the rig's loader below fills) reach is a stub here that logs what it was given --
// structures and arrays by a hash of their bytes, which hold pointers into the rig's own
// memory, the same for both sides -- makes handles in order, and returns the result the
// case asks for. Two sides that make the same calls leave the same log.
#include <vulkan/vulkan.h>

#include <linux/perf_event.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <unistd.h>

#include <chrono>
#include <cstdint>
#include <cstring>
#include <string>
#include <vector>

namespace rig_vk
{
    std::vector<uint64_t> g_calls;
    bool g_logging = false;
    uint64_t g_next = 0x1000;
    VkResult g_result = VK_SUCCESS;

    uint64_t Hash(const void* bytes, size_t size)
    {
        uint64_t h = 1469598103934665603ull;
        const auto* at = static_cast<const uint8_t*>(bytes);
        for (size_t n = 0; at && n < size; n++) h = (h ^ at[n]) * 1099511628211ull;
        return h;
    }
    // An array of none is any pointer, to Vulkan as to the hash: a Rust slice of none has no null.
    template <class T> uint64_t H(const T* p, size_t count = 1) { return p && count ? Hash(p, sizeof(T) * count) : 0; }
    template <class T> uint64_t R(T handle) { return uint64_t(reinterpret_cast<uintptr_t>(handle)); }
    // Image barriers field by field: util's barrier is built on each side's own stack, and
    // its padding is not part of it.
    uint64_t Images(const VkImageMemoryBarrier* b, uint32_t n)
    {
        uint64_t h = 0;
        for (uint32_t k = 0; b && k < n; k++)
        {
            const VkImageMemoryBarrier& i = b[k];
            const uint64_t words[] = { uint64_t(i.sType), uint64_t(reinterpret_cast<uintptr_t>(i.pNext)), i.srcAccessMask,
                                       i.dstAccessMask, uint64_t(i.oldLayout), uint64_t(i.newLayout),
                                       i.srcQueueFamilyIndex, i.dstQueueFamilyIndex,
                                       uint64_t(reinterpret_cast<uintptr_t>(i.image)), i.subresourceRange.aspectMask,
                                       i.subresourceRange.baseMipLevel, i.subresourceRange.levelCount,
                                       i.subresourceRange.baseArrayLayer, i.subresourceRange.layerCount };
            h = h * 31 + Hash(words, sizeof words);
        }
        return h;
    }
    void Log(std::initializer_list<uint64_t> words)
    {
        if (!g_logging) return;
        g_calls.push_back(words.size());
        g_calls.insert(g_calls.end(), words);
    }
    template <class T> void Make(T* out, uint32_t count = 1)
    {
        for (uint32_t n = 0; n < count; n++)
        {
            out[n] = reinterpret_cast<T>(uintptr_t(g_next));
            g_next += 0x10;
        }
    }
}

using namespace rig_vk;

#define CREATE(name, Info, Handle, id)                                                              \
    extern "C" VkResult name(VkDevice d, const Info* i, const VkAllocationCallbacks* a, Handle* o)   \
    {                                                                                                \
        Log({ id, R(d), H(i), R(a) });                                                               \
        if (g_result != VK_SUCCESS) return g_result;                                                 \
        Make(o);                                                                                     \
        return VK_SUCCESS;                                                                           \
    }
#define DESTROY(name, Handle, id)                                                                    \
    extern "C" void name(VkDevice d, Handle h, const VkAllocationCallbacks* a) { Log({ id, R(d), R(h), R(a) }); }

CREATE(vkCreateImage, VkImageCreateInfo, VkImage, 1)
DESTROY(vkDestroyImage, VkImage, 2)
CREATE(vkCreateImageView, VkImageViewCreateInfo, VkImageView, 3)
DESTROY(vkDestroyImageView, VkImageView, 4)
CREATE(vkCreateBuffer, VkBufferCreateInfo, VkBuffer, 5)
DESTROY(vkDestroyBuffer, VkBuffer, 6)
CREATE(vkAllocateMemory, VkMemoryAllocateInfo, VkDeviceMemory, 7)
DESTROY(vkFreeMemory, VkDeviceMemory, 8)
CREATE(vkCreateSampler, VkSamplerCreateInfo, VkSampler, 9)
DESTROY(vkDestroySampler, VkSampler, 10)
CREATE(vkCreateDescriptorPool, VkDescriptorPoolCreateInfo, VkDescriptorPool, 11)
DESTROY(vkDestroyDescriptorPool, VkDescriptorPool, 12)
CREATE(vkCreateDescriptorSetLayout, VkDescriptorSetLayoutCreateInfo, VkDescriptorSetLayout, 13)
DESTROY(vkDestroyDescriptorSetLayout, VkDescriptorSetLayout, 14)
CREATE(vkCreatePipelineLayout, VkPipelineLayoutCreateInfo, VkPipelineLayout, 15)
DESTROY(vkDestroyPipelineLayout, VkPipelineLayout, 16)
DESTROY(vkDestroyPipeline, VkPipeline, 17)
CREATE(vkCreateShaderModule, VkShaderModuleCreateInfo, VkShaderModule, 18)
DESTROY(vkDestroyShaderModule, VkShaderModule, 19)
CREATE(vkCreateCommandPool, VkCommandPoolCreateInfo, VkCommandPool, 20)
DESTROY(vkDestroyCommandPool, VkCommandPool, 21)
CREATE(vkCreateFence, VkFenceCreateInfo, VkFence, 22)
DESTROY(vkDestroyFence, VkFence, 23)
CREATE(vkCreateQueryPool, VkQueryPoolCreateInfo, VkQueryPool, 24)
DESTROY(vkDestroyQueryPool, VkQueryPool, 25)

extern "C" VkResult vkBindImageMemory(VkDevice d, VkImage h, VkDeviceMemory m, VkDeviceSize o)
{
    Log({ 30, R(d), R(h), R(m), o });
    return g_result;
}
extern "C" VkResult vkBindBufferMemory(VkDevice d, VkBuffer h, VkDeviceMemory m, VkDeviceSize o)
{
    Log({ 31, R(d), R(h), R(m), o });
    return g_result;
}
extern "C" VkResult vkAllocateDescriptorSets(VkDevice d, const VkDescriptorSetAllocateInfo* i, VkDescriptorSet* o)
{
    Log({ 32, R(d), H(i) });
    if (g_result != VK_SUCCESS) return g_result;
    Make(o, i->descriptorSetCount);
    return VK_SUCCESS;
}
extern "C" VkResult vkFreeDescriptorSets(VkDevice d, VkDescriptorPool p, uint32_t n, const VkDescriptorSet* s)
{
    Log({ 33, R(d), R(p), n, H(s, n) });
    return g_result;
}
extern "C" void vkUpdateDescriptorSets(VkDevice d, uint32_t nw, const VkWriteDescriptorSet* w, uint32_t nc,
                                       const VkCopyDescriptorSet* c)
{
    Log({ 34, R(d), nw, H(w, nw), nc, H(c, nc) });
}
extern "C" VkResult vkCreateGraphicsPipelines(VkDevice d, VkPipelineCache c, uint32_t n,
                                             const VkGraphicsPipelineCreateInfo* i, const VkAllocationCallbacks* a,
                                             VkPipeline* o)
{
    Log({ 35, R(d), R(c), n, H(i, n), R(a) });
    if (g_result != VK_SUCCESS) return g_result;
    Make(o, n);
    return VK_SUCCESS;
}
extern "C" VkResult vkCreateComputePipelines(VkDevice d, VkPipelineCache c, uint32_t n,
                                            const VkComputePipelineCreateInfo* i, const VkAllocationCallbacks* a,
                                            VkPipeline* o)
{
    Log({ 36, R(d), R(c), n, H(i, n), R(a) });
    if (g_result != VK_SUCCESS) return g_result;
    Make(o, n);
    return VK_SUCCESS;
}
extern "C" VkResult vkResetCommandPool(VkDevice d, VkCommandPool p, VkCommandPoolResetFlags f)
{
    Log({ 37, R(d), R(p), f });
    return g_result;
}
extern "C" VkResult vkAllocateCommandBuffers(VkDevice d, const VkCommandBufferAllocateInfo* i, VkCommandBuffer* o)
{
    Log({ 38, R(d), H(i) });
    if (g_result != VK_SUCCESS) return g_result;
    Make(o, i->commandBufferCount);
    return VK_SUCCESS;
}
extern "C" void vkFreeCommandBuffers(VkDevice d, VkCommandPool p, uint32_t n, const VkCommandBuffer* c)
{
    Log({ 39, R(d), R(p), n, H(c, n) });
}
extern "C" VkResult vkBeginCommandBuffer(VkCommandBuffer c, const VkCommandBufferBeginInfo* i)
{
    Log({ 40, R(c), H(i) });
    return g_result;
}
extern "C" VkResult vkEndCommandBuffer(VkCommandBuffer c)
{
    Log({ 41, R(c) });
    return g_result;
}
extern "C" VkResult vkResetFences(VkDevice d, uint32_t n, const VkFence* f)
{
    Log({ 42, R(d), n, H(f, n) });
    return g_result;
}
extern "C" VkResult vkQueueSubmit(VkQueue q, uint32_t n, const VkSubmitInfo* s, VkFence f)
{
    Log({ 43, R(q), n, H(s, n), R(f) });
    return g_result;
}
extern "C" void vkCmdBindDescriptorSets(VkCommandBuffer c, VkPipelineBindPoint b, VkPipelineLayout l, uint32_t first,
                                        uint32_t n, const VkDescriptorSet* s, uint32_t nd, const uint32_t* dyn)
{
    Log({ 44, R(c), uint64_t(b), R(l), first, n, H(s, n), nd, H(dyn, nd) });
}
extern "C" void vkCmdPipelineBarrier(VkCommandBuffer c, VkPipelineStageFlags src, VkPipelineStageFlags dst,
                                     VkDependencyFlags dep, uint32_t nm, const VkMemoryBarrier* m, uint32_t nb,
                                     const VkBufferMemoryBarrier* b, uint32_t ni, const VkImageMemoryBarrier* i)
{
    Log({ 45, R(c), src, dst, dep, nm, H(m, nm), nb, H(b, nb), ni, Images(i, ni) });
}
extern "C" void vkCmdBeginQuery(VkCommandBuffer c, VkQueryPool p, uint32_t q, VkQueryControlFlags f)
{
    Log({ 46, R(c), R(p), q, f });
}
extern "C" void vkCmdEndQuery(VkCommandBuffer c, VkQueryPool p, uint32_t q) { Log({ 47, R(c), R(p), q }); }
extern "C" void vkCmdResetQueryPool(VkCommandBuffer c, VkQueryPool p, uint32_t first, uint32_t n)
{
    Log({ 48, R(c), R(p), first, n });
}
extern "C" void vkCmdWriteTimestamp(VkCommandBuffer c, VkPipelineStageFlagBits s, VkQueryPool p, uint32_t q)
{
    Log({ 49, R(c), uint64_t(s), R(p), q });
}
extern "C" void vkCmdBindIndexBuffer(VkCommandBuffer c, VkBuffer b, VkDeviceSize o, VkIndexType t)
{
    Log({ 50, R(c), R(b), o, uint64_t(t) });
}
extern "C" void vkCmdBindPipeline(VkCommandBuffer c, VkPipelineBindPoint b, VkPipeline p)
{
    Log({ 51, R(c), uint64_t(b), R(p) });
}
extern "C" void vkCmdDraw(VkCommandBuffer c, uint32_t v, uint32_t i, uint32_t fv, uint32_t fi)
{
    Log({ 52, R(c), v, i, fv, fi });
}
extern "C" void vkCmdDrawIndexed(VkCommandBuffer c, uint32_t n, uint32_t i, uint32_t fi, int32_t vo, uint32_t fin)
{
    Log({ 53, R(c), n, i, fi, uint64_t(int64_t(vo)), fin });
}
extern "C" void vkCmdSetDepthBias(VkCommandBuffer c, float k, float clamp, float slope)
{
    Log({ 54, R(c), H(&k), H(&clamp), H(&slope) });
}
extern "C" void vkCmdSetScissor(VkCommandBuffer c, uint32_t first, uint32_t n, const VkRect2D* r)
{
    Log({ 55, R(c), first, n, H(r, n) });
}
extern "C" void vkCmdSetViewport(VkCommandBuffer c, uint32_t first, uint32_t n, const VkViewport* v)
{
    Log({ 56, R(c), first, n, H(v, n) });
}
extern "C" void vkCmdSetStencilCompareMask(VkCommandBuffer c, VkStencilFaceFlags f, uint32_t v) { Log({ 57, R(c), f, v }); }
extern "C" void vkCmdSetStencilWriteMask(VkCommandBuffer c, VkStencilFaceFlags f, uint32_t v) { Log({ 58, R(c), f, v }); }
extern "C" void vkCmdSetStencilReference(VkCommandBuffer c, VkStencilFaceFlags f, uint32_t v) { Log({ 59, R(c), f, v }); }
extern "C" void vkCmdBlitImage(VkCommandBuffer c, VkImage s, VkImageLayout sl, VkImage d, VkImageLayout dl, uint32_t n,
                               const VkImageBlit* r, VkFilter f)
{
    Log({ 60, R(c), R(s), uint64_t(sl), R(d), uint64_t(dl), n, H(r, n), uint64_t(f) });
}
extern "C" void vkCmdCopyImage(VkCommandBuffer c, VkImage s, VkImageLayout sl, VkImage d, VkImageLayout dl, uint32_t n,
                               const VkImageCopy* r)
{
    Log({ 61, R(c), R(s), uint64_t(sl), R(d), uint64_t(dl), n, H(r, n) });
}
extern "C" void vkCmdResolveImage(VkCommandBuffer c, VkImage s, VkImageLayout sl, VkImage d, VkImageLayout dl,
                                  uint32_t n, const VkImageResolve* r)
{
    Log({ 62, R(c), R(s), uint64_t(sl), R(d), uint64_t(dl), n, H(r, n) });
}
extern "C" void vkCmdClearColorImage(VkCommandBuffer c, VkImage i, VkImageLayout l, const VkClearColorValue* v,
                                     uint32_t n, const VkImageSubresourceRange* r)
{
    Log({ 63, R(c), R(i), uint64_t(l), H(v), n, H(r, n) });
}
extern "C" void vkCmdClearDepthStencilImage(VkCommandBuffer c, VkImage i, VkImageLayout l,
                                            const VkClearDepthStencilValue* v, uint32_t n,
                                            const VkImageSubresourceRange* r)
{
    Log({ 64, R(c), R(i), uint64_t(l), H(v), n, H(r, n) });
}
extern "C" void vkCmdCopyBufferToImage(VkCommandBuffer c, VkBuffer b, VkImage i, VkImageLayout l, uint32_t n,
                                       const VkBufferImageCopy* r)
{
    Log({ 65, R(c), R(b), R(i), uint64_t(l), n, H(r, n) });
}
extern "C" void vkCmdCopyImageToBuffer(VkCommandBuffer c, VkImage i, VkImageLayout l, VkBuffer b, uint32_t n,
                                       const VkBufferImageCopy* r)
{
    Log({ 66, R(c), R(i), uint64_t(l), R(b), n, H(r, n) });
}
extern "C" void vkCmdDispatch(VkCommandBuffer c, uint32_t x, uint32_t y, uint32_t z) { Log({ 67, R(c), x, y, z }); }
extern "C" void vkCmdUpdateBuffer(VkCommandBuffer c, VkBuffer b, VkDeviceSize o, VkDeviceSize n, const void* data)
{
    Log({ 68, R(c), R(b), o, n, Hash(data, size_t(n)) });
}

// Untraced, as in the C++ renderer: what they give back is made from their arguments.
extern "C" VkResult vkDeviceWaitIdle(VkDevice d)
{
    Log({ 80, R(d) });
    return g_result;
}
extern "C" void vkGetBufferMemoryRequirements(VkDevice d, VkBuffer b, VkMemoryRequirements* r)
{
    Log({ 81, R(d), R(b) });
    *r = { R(b) * 3, 256, uint32_t(R(b) >> 4) };
}
extern "C" void vkGetImageMemoryRequirements(VkDevice d, VkImage i, VkMemoryRequirements* r)
{
    Log({ 82, R(d), R(i) });
    *r = { R(i) * 5, 4096, uint32_t(R(i) >> 3) };
}
extern "C" VkResult vkGetFenceStatus(VkDevice d, VkFence f)
{
    Log({ 83, R(d), R(f) });
    return g_result;
}
extern "C" VkResult vkWaitForFences(VkDevice d, uint32_t n, const VkFence* f, VkBool32 all, uint64_t timeout)
{
    Log({ 84, R(d), n, H(f, n), all, timeout });
    return g_result;
}
extern "C" VkResult vkGetQueryPoolResults(VkDevice d, VkQueryPool p, uint32_t first, uint32_t n, size_t size, void* data,
                                         VkDeviceSize stride, VkQueryResultFlags flags)
{
    Log({ 85, R(d), R(p), first, n, size, stride, flags });
    auto* bytes = static_cast<uint8_t*>(data);
    for (size_t k = 0; k < size; k++) bytes[k] = uint8_t(k * 7 + first);
    return g_result;
}
extern "C" VkResult vkMapMemory(VkDevice d, VkDeviceMemory m, VkDeviceSize o, VkDeviceSize s, VkMemoryMapFlags f,
                                void** out)
{
    Log({ 86, R(d), R(m), o, s, f });
    *out = reinterpret_cast<void*>(uintptr_t(R(m) + o));
    return g_result;
}
extern "C" void vkUnmapMemory(VkDevice d, VkDeviceMemory m) { Log({ 87, R(d), R(m) }); }
extern "C" void vkGetPhysicalDeviceFormatProperties(VkPhysicalDevice p, VkFormat f, VkFormatProperties* o)
{
    Log({ 88, R(p), uint64_t(f) });
    *o = { VkFormatFeatureFlags(f * 3), VkFormatFeatureFlags(f * 5), VkFormatFeatureFlags(f * 7) };
}
extern "C" void vkGetPhysicalDeviceMemoryProperties(VkPhysicalDevice p, VkPhysicalDeviceMemoryProperties* o)
{
    Log({ 89, R(p) });
    std::memset(o, 0, sizeof *o);
    o->memoryTypeCount = 3;
    for (uint32_t n = 0; n < 3; n++) o->memoryTypes[n] = { VkMemoryPropertyFlags(1u << n), n % 2 };
    o->memoryHeapCount = 2;
    o->memoryHeaps[1] = { 1ull << 33, 1 };
}
extern "C" void vkGetPhysicalDeviceProperties(VkPhysicalDevice p, VkPhysicalDeviceProperties* o)
{
    Log({ 90, R(p) });
    std::memset(o, 0, sizeof *o);
    o->apiVersion = VK_API_VERSION_1_3;
    o->vendorID = 0x1002;
    std::strcpy(o->deviceName, "rig stand-in");
    o->limits.minUniformBufferOffsetAlignment = 256;
    o->limits.timestampPeriod = 10.0f;
}
extern "C" void vkGetPhysicalDeviceProperties2(VkPhysicalDevice p, VkPhysicalDeviceProperties2* o)
{
    Log({ 91, R(p), uint64_t(o->sType), R(o->pNext) });
    o->properties.deviceID = uint32_t(R(p));
}
extern "C" void vkGetPhysicalDeviceQueueFamilyProperties(VkPhysicalDevice p, uint32_t* n, VkQueueFamilyProperties* o)
{
    Log({ 92, R(p), *n, uint64_t(o != nullptr) });
    if (!o) { *n = 3; return; }
    for (uint32_t k = 0; k < *n && k < 3; k++) o[k] = { VkQueueFlags(k + 1), k + 1, 64, { 1, 1, 1 } };
    if (*n > 3) *n = 3;
}

// The extension commands: no global names (the loader exports these too).
namespace
{
    VKAPI_ATTR void VKAPI_CALL BeginRendering(VkCommandBuffer c, const VkRenderingInfo* i) { Log({ 100, R(c), H(i) }); }
    VKAPI_ATTR void VKAPI_CALL EndRendering(VkCommandBuffer c) { Log({ 101, R(c) }); }
    VKAPI_ATTR void VKAPI_CALL SetCullMode(VkCommandBuffer c, VkCullModeFlags m) { Log({ 102, R(c), m }); }
    VKAPI_ATTR void VKAPI_CALL SetFrontFace(VkCommandBuffer c, VkFrontFace f) { Log({ 103, R(c), uint64_t(f) }); }
    VKAPI_ATTR void VKAPI_CALL SetDepthTestEnable(VkCommandBuffer c, VkBool32 v) { Log({ 104, R(c), v }); }
    VKAPI_ATTR void VKAPI_CALL SetDepthWriteEnable(VkCommandBuffer c, VkBool32 v) { Log({ 105, R(c), v }); }
    VKAPI_ATTR void VKAPI_CALL SetDepthCompareOp(VkCommandBuffer c, VkCompareOp v) { Log({ 106, R(c), uint64_t(v) }); }
    VKAPI_ATTR void VKAPI_CALL SetStencilTestEnable(VkCommandBuffer c, VkBool32 v) { Log({ 107, R(c), v }); }
    VKAPI_ATTR void VKAPI_CALL SetStencilOp(VkCommandBuffer c, VkStencilFaceFlags f, VkStencilOp a, VkStencilOp b,
                                            VkStencilOp d, VkCompareOp o)
    {
        Log({ 108, R(c), f, uint64_t(a), uint64_t(b), uint64_t(d), uint64_t(o) });
    }
    VKAPI_ATTR void VKAPI_CALL DrawMulti(VkCommandBuffer c, uint32_t n, const VkMultiDrawInfoEXT* d, uint32_t i,
                                         uint32_t fi, uint32_t stride)
    {
        Log({ 109, R(c), n, Hash(d, size_t(n) * stride), i, fi, stride });
    }
    VKAPI_ATTR void VKAPI_CALL DrawMultiIndexed(VkCommandBuffer c, uint32_t n, const VkMultiDrawIndexedInfoEXT* d,
                                                uint32_t i, uint32_t fi, uint32_t stride, const int32_t* vo)
    {
        Log({ 110, R(c), n, Hash(d, size_t(n) * stride), i, fi, stride, vo ? uint64_t(int64_t(*vo)) : 0xDEAD, R(vo) });
    }

    struct Named
    {
        const char* name;
        PFN_vkVoidFunction function;
    };
#define N(name) { #name, reinterpret_cast<PFN_vkVoidFunction>(&name) }
#define X(name, function) { name, reinterpret_cast<PFN_vkVoidFunction>(&function) }
    const Named kCommands[] = {
        N(vkCreateImage), N(vkDestroyImage), N(vkCreateImageView), N(vkDestroyImageView), N(vkCreateBuffer),
        N(vkDestroyBuffer), N(vkAllocateMemory), N(vkFreeMemory), N(vkBindImageMemory), N(vkBindBufferMemory),
        N(vkCreateSampler), N(vkDestroySampler), N(vkCreateDescriptorPool), N(vkDestroyDescriptorPool),
        N(vkAllocateDescriptorSets), N(vkFreeDescriptorSets), N(vkUpdateDescriptorSets),
        N(vkCreateDescriptorSetLayout), N(vkDestroyDescriptorSetLayout), N(vkCreatePipelineLayout),
        N(vkDestroyPipelineLayout), N(vkCreateGraphicsPipelines), N(vkCreateComputePipelines), N(vkDestroyPipeline),
        N(vkCreateShaderModule), N(vkDestroyShaderModule), N(vkCreateCommandPool), N(vkDestroyCommandPool),
        N(vkResetCommandPool), N(vkAllocateCommandBuffers), N(vkFreeCommandBuffers), N(vkBeginCommandBuffer),
        N(vkEndCommandBuffer), N(vkCreateFence), N(vkDestroyFence), N(vkResetFences), N(vkCreateQueryPool),
        N(vkDestroyQueryPool), N(vkQueueSubmit), N(vkCmdBindDescriptorSets), N(vkCmdPipelineBarrier),
        N(vkCmdBeginQuery), N(vkCmdEndQuery), N(vkCmdResetQueryPool), N(vkCmdWriteTimestamp),
        N(vkCmdBindIndexBuffer), N(vkCmdBindPipeline), N(vkCmdDraw), N(vkCmdDrawIndexed), N(vkCmdSetDepthBias),
        N(vkCmdSetScissor), N(vkCmdSetViewport), N(vkCmdSetStencilCompareMask), N(vkCmdSetStencilWriteMask),
        N(vkCmdSetStencilReference), N(vkCmdBlitImage), N(vkCmdCopyImage), N(vkCmdResolveImage),
        N(vkCmdClearColorImage), N(vkCmdClearDepthStencilImage), N(vkCmdCopyBufferToImage),
        N(vkCmdCopyImageToBuffer), N(vkCmdDispatch), N(vkCmdUpdateBuffer), N(vkDeviceWaitIdle),
        N(vkGetBufferMemoryRequirements), N(vkGetImageMemoryRequirements), N(vkGetFenceStatus), N(vkWaitForFences),
        N(vkGetQueryPoolResults), N(vkMapMemory), N(vkUnmapMemory), N(vkGetPhysicalDeviceFormatProperties),
        N(vkGetPhysicalDeviceMemoryProperties), N(vkGetPhysicalDeviceProperties), N(vkGetPhysicalDeviceProperties2),
        N(vkGetPhysicalDeviceQueueFamilyProperties),
        X("vkCmdBeginRenderingKHR", BeginRendering), X("vkCmdEndRenderingKHR", EndRendering),
        X("vkCmdSetCullModeEXT", SetCullMode), X("vkCmdSetFrontFaceEXT", SetFrontFace),
        X("vkCmdSetDepthTestEnableEXT", SetDepthTestEnable), X("vkCmdSetDepthWriteEnableEXT", SetDepthWriteEnable),
        X("vkCmdSetDepthCompareOpEXT", SetDepthCompareOp), X("vkCmdSetStencilTestEnableEXT", SetStencilTestEnable),
        X("vkCmdSetStencilOpEXT", SetStencilOp), X("vkCmdDrawMultiEXT", DrawMulti),
        X("vkCmdDrawMultiIndexedEXT", DrawMultiIndexed),
    };
#undef N
#undef X
    // Commands the case says the device does not give.
    bool g_noMulti = false;
    int g_noDynamic = 0;   // 1: rendering and the stencil op, 2: the stencil op

    PFN_vkVoidFunction Find(const char* name)
    {
        if (g_noMulti && std::strstr(name, "DrawMulti")) return nullptr;
        if (g_noDynamic == 1 && (std::strstr(name, "Rendering") || std::strstr(name, "StencilOpEXT"))) return nullptr;
        if (g_noDynamic == 2 && std::strstr(name, "StencilOpEXT")) return nullptr;
        for (const Named& c : kCommands)
            if (std::strcmp(c.name, name) == 0) return c.function;
        return nullptr;
    }

    VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL GetDeviceProcAddr(VkDevice, const char* name) { return Find(name); }
}

extern "C" VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL rig_dev_vkGetInstanceProcAddr(VkInstance, const char* name)
{
    if (std::strcmp(name, "vkGetDeviceProcAddr") == 0) return reinterpret_cast<PFN_vkVoidFunction>(&GetDeviceProcAddr);
    return Find(name);
}

namespace rig_vk
{
    // The C++ renderer's Dispatch, from the same table.
    PFN_vkVoidFunction Command(const char* name) { return Find(name); }
    void Withhold(bool multi, int dynamic)
    {
        g_noMulti = multi;
        g_noDynamic = dynamic;
    }
}

// ---- the trace's position, the benchmark's clock and counters ------------------------------

namespace rig_vk
{
    uint64_t g_position = 0;
}
extern "C" uint64_t rig_position() { return rig_vk::g_position; }

extern "C" uint64_t rig_rec_now()
{
    return uint64_t(std::chrono::duration_cast<std::chrono::nanoseconds>(
                        std::chrono::steady_clock::now().time_since_epoch())
                        .count());
}

// This thread's instructions in user space, counting from now; -1 when perf is refused.
extern "C" int rig_counter_open()
{
    perf_event_attr attr{};
    attr.type = PERF_TYPE_HARDWARE;
    attr.size = sizeof attr;
    attr.config = PERF_COUNT_HW_INSTRUCTIONS;
    attr.exclude_kernel = 1;
    attr.exclude_hv = 1;
    return int(syscall(SYS_perf_event_open, &attr, 0, -1, -1, 0));
}

extern "C" uint64_t rig_counter_read(int fd)
{
    uint64_t value = 0;
    if (fd < 0 || read(fd, &value, sizeof value) != sizeof value) return 0;
    return value;
}

extern "C" void rig_counter_close(int fd)
{
    if (fd >= 0) close(fd);
}
