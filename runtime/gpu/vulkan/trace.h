#pragma once
// The renderer's reference instruments (diagnostics builds only):
//
//   MW2_TRACE_COMMANDS=<file>[:<first frame>:<frames>]  every Vulkan call the renderer
//       makes that creates, destroys, records or submits, one normalised line each
//   MW2_DETERMINISTIC=1   none of the renderer's decisions taken by the clock
//   MW2_FORCE_* and the rest  paths a device or a run seldom takes, forced (Forcing)
//
// Every renderer file that records, creates or submits includes this header last.
// It turns each core vk* call the renderer makes into the vk::trace wrapper of the
// same name: with the trace off a wrapper is the call and one test of a flag. The
// presenter, the backdrop, gpu_time and the capture are left out (they are Rust in
// the Rust renderer), and so is the C++ the launcher shares (pipeline.cpp,
// shader_build.cpp); a handle they made is named by its role (Register).
//
// docs/rendering.md (Checking) gives the line format.
#include "../../diagnostics.h"

#if MW2_DIAGNOSTICS && defined(MW2_HAVE_VULKAN)
#define MW2_TRACE 1

#include <cstddef>
#include <cstdint>
#include <vector>

#include <vulkan/vulkan.h>

namespace vk::trace
{
    namespace detail
    {
        extern bool on;              // MW2_TRACE_COMMANDS names a file
        extern bool deterministic;   // MW2_DETERMINISTIC=1
    }
    inline bool On() { return detail::on; }
    inline bool Deterministic() { return detail::deterministic; }

    // The forced fallbacks, read once at the renderer's Initialise (Start).
    struct Forced
    {
        bool instanceDepthResolve = false;   // MW2_FORCE_INSTANCE_DEPTH_RESOLVE
        bool resolveTwin = false;            // MW2_FORCE_RESOLVE_TWIN
        uint32_t drawnResolveSets = 0;       // MW2_DRAWN_RESOLVE_SETS, 0: as built
        uint32_t resolveRemake = 0;          // MW2_FORCE_RESOLVE_REMAKE, frames; 0: never
        bool noMultiDraw = false;            // MW2_NO_MULTI_DRAW
        bool loneUploads = false;            // MW2_FORCE_LONE_UPLOADS
        uint32_t stagingLeastKb = 0;         // MW2_STAGING_LEAST_KB, 0: as built
        double fakeDeviceLost = 0;           // MW2_FAKE_DEVICE_LOST, seconds; 0: never
    };
    const Forced& Forcing();
    // Counted where a forced path is taken, for the report.
    struct ForcedCounts
    {
        uint64_t twinResolves = 0, remadeResolves = 0, loneUploads = 0, lostSubmissions = 0;
    };
    ForcedCounts& Counts();
    // Whether MW2_FAKE_DEVICE_LOST's time has come: every submission from then on
    // fails with VK_ERROR_DEVICE_LOST. Any thread.
    bool DeviceLostFaked();

    enum Kind : uint8_t
    {
        kImage, kView, kBuffer, kMemory, kSampler, kDescriptorPool, kSet, kPipeline, kModule,
        kCommandPool, kCommand, kFence, kQueryPool, kSetLayout, kPipelineLayout, kSemaphore,
        kDevice, kPhysical, kQueue, kCache, kKinds
    };
    template <class Handle> inline uint64_t Raw(Handle handle)
    {
        return uint64_t(reinterpret_cast<uintptr_t>(handle));
    }

    // At the renderer's Initialise, before it makes anything: reads the switches,
    // opens the trace and names the handles it did not make. `position` is the
    // recorder's queue position (vk::record::Position).
    void Start(VkDevice device, VkPhysicalDevice physical, VkQueue queue, VkPipelineCache cache,
               VkPipelineLayout layout, const VkDescriptorSetLayout* setLayouts, uint32_t setLayoutCount,
               uint64_t (*position)());
    // Names a handle traced code did not make, by its role. Any thread.
    void Register(Kind kind, uint64_t handle, const char* format, ...);
    // The next handle of `kind` traced code makes is called `name`, not by its ordinal.
    void NameNext(Kind kind, const char* name);
    // A line of its own, "<seq> <text>": the hashes, markers.
    void Note(const char* format, ...);
    // Whether a line made now can fall in the window MW2_TRACE_COMMANDS names: the
    // hashes are slow, and made only then.
    bool Wanted();
    // The renderer finished frame `frame`: the window MW2_TRACE_COMMANDS names is in frames.
    void Frame(uint64_t frame);
    // The renderer's submission, at its call site (gpu_time submits it, untraced).
    void Submission(VkQueue queue, const VkSubmitInfo& submit, VkFence fence);
    // The image handed to the window for frame `frame`: one of Present0..3.
    void Present(VkImage image, uint64_t frame);
    // The renderer's Shutdown begins: the destroys after this line come in hash-map order.
    void ShutdownBegins();
    // After the last traced call: the summary line, and the file closed.
    void Finish();

    // The recorder thread (recorder.cpp): lines made on it come in queue order, and a
    // line the consumer makes goes after the calls queued before it and before those
    // queued after, whichever thread gets there first.
    void RecorderStarted();
    void RecorderThread();
    void RecorderMade(uint64_t position);
    void RecorderStopped();

    uint64_t Fnv64(const void* bytes, size_t size);
    // The same, for mapped device memory: the arena and the shadow.
    uint64_t FnvWriteCombined(const void* bytes, size_t size);
    // MW2_DUMP_SHADERS=<dir>: a built-in shader as <dir>/builtin_<name>.spv.
    void DumpBuiltin(const char* name, const std::vector<uint32_t>& words);

    // A state library made by the launcher's shared MakeLibrary, which is not traced:
    // the create info and chain it was given, written field by field once it is named.
    void StateLibrary(VkPipeline made, VkGraphicsPipelineLibraryFlagsEXT part,
                      const VkGraphicsPipelineCreateInfo& info, const void* next);

    // util.h's barrier: traced when it goes into a command buffer traced code made.
    void UtilBarrier(VkCommandBuffer command, VkPipelineStageFlags srcStageMask,
                     VkPipelineStageFlags dstStageMask, VkDependencyFlags dependencyFlags,
                     uint32_t memoryBarrierCount, const VkMemoryBarrier* pMemoryBarriers,
                     uint32_t bufferMemoryBarrierCount, const VkBufferMemoryBarrier* pBufferMemoryBarriers,
                     uint32_t imageMemoryBarrierCount, const VkImageMemoryBarrier* pImageMemoryBarriers);

    // The renderer's extension commands (renderer_state.h's Dispatch), wrapped when the
    // trace is on: each pointer is kept and replaced by the wrapper.
    void WrapDispatch(PFN_vkCmdBeginRenderingKHR& beginRendering, PFN_vkCmdEndRenderingKHR& endRendering,
                      PFN_vkCmdSetCullModeEXT& setCullMode, PFN_vkCmdSetFrontFaceEXT& setFrontFace,
                      PFN_vkCmdSetDepthTestEnableEXT& setDepthTestEnable,
                      PFN_vkCmdSetDepthWriteEnableEXT& setDepthWriteEnable,
                      PFN_vkCmdSetDepthCompareOpEXT& setDepthCompareOp,
                      PFN_vkCmdSetStencilTestEnableEXT& setStencilTestEnable,
                      PFN_vkCmdSetStencilOpEXT& setStencilOp, PFN_vkCmdDrawMultiEXT& drawMulti,
                      PFN_vkCmdDrawMultiIndexedEXT& drawMultiIndexed);

// The wrapper: the call itself while the trace is off, the traced copy in trace.cpp otherwise.
#define MW2_TRACE_WRAP(result, name, parameters, arguments)        \
    result name##Traced parameters;                                \
    inline result name parameters                                  \
    {                                                              \
        if (!detail::on) return ::vk##name arguments;              \
        return name##Traced arguments;                             \
    }

    MW2_TRACE_WRAP(VkResult, CreateImage, (VkDevice d, const VkImageCreateInfo* i, const VkAllocationCallbacks* a, VkImage* o), (d, i, a, o))
    MW2_TRACE_WRAP(void, DestroyImage, (VkDevice d, VkImage h, const VkAllocationCallbacks* a), (d, h, a))
    MW2_TRACE_WRAP(VkResult, CreateImageView, (VkDevice d, const VkImageViewCreateInfo* i, const VkAllocationCallbacks* a, VkImageView* o), (d, i, a, o))
    MW2_TRACE_WRAP(void, DestroyImageView, (VkDevice d, VkImageView h, const VkAllocationCallbacks* a), (d, h, a))
    MW2_TRACE_WRAP(VkResult, CreateBuffer, (VkDevice d, const VkBufferCreateInfo* i, const VkAllocationCallbacks* a, VkBuffer* o), (d, i, a, o))
    MW2_TRACE_WRAP(void, DestroyBuffer, (VkDevice d, VkBuffer h, const VkAllocationCallbacks* a), (d, h, a))
    MW2_TRACE_WRAP(VkResult, AllocateMemory, (VkDevice d, const VkMemoryAllocateInfo* i, const VkAllocationCallbacks* a, VkDeviceMemory* o), (d, i, a, o))
    MW2_TRACE_WRAP(void, FreeMemory, (VkDevice d, VkDeviceMemory h, const VkAllocationCallbacks* a), (d, h, a))
    MW2_TRACE_WRAP(VkResult, BindImageMemory, (VkDevice d, VkImage h, VkDeviceMemory m, VkDeviceSize o), (d, h, m, o))
    MW2_TRACE_WRAP(VkResult, BindBufferMemory, (VkDevice d, VkBuffer h, VkDeviceMemory m, VkDeviceSize o), (d, h, m, o))
    MW2_TRACE_WRAP(VkResult, CreateSampler, (VkDevice d, const VkSamplerCreateInfo* i, const VkAllocationCallbacks* a, VkSampler* o), (d, i, a, o))
    MW2_TRACE_WRAP(void, DestroySampler, (VkDevice d, VkSampler h, const VkAllocationCallbacks* a), (d, h, a))
    MW2_TRACE_WRAP(VkResult, CreateDescriptorPool, (VkDevice d, const VkDescriptorPoolCreateInfo* i, const VkAllocationCallbacks* a, VkDescriptorPool* o), (d, i, a, o))
    MW2_TRACE_WRAP(void, DestroyDescriptorPool, (VkDevice d, VkDescriptorPool h, const VkAllocationCallbacks* a), (d, h, a))
    MW2_TRACE_WRAP(VkResult, AllocateDescriptorSets, (VkDevice d, const VkDescriptorSetAllocateInfo* i, VkDescriptorSet* o), (d, i, o))
    MW2_TRACE_WRAP(VkResult, FreeDescriptorSets, (VkDevice d, VkDescriptorPool p, uint32_t n, const VkDescriptorSet* s), (d, p, n, s))
    MW2_TRACE_WRAP(void, UpdateDescriptorSets, (VkDevice d, uint32_t nw, const VkWriteDescriptorSet* w, uint32_t nc, const VkCopyDescriptorSet* c), (d, nw, w, nc, c))
    MW2_TRACE_WRAP(VkResult, CreateDescriptorSetLayout, (VkDevice d, const VkDescriptorSetLayoutCreateInfo* i, const VkAllocationCallbacks* a, VkDescriptorSetLayout* o), (d, i, a, o))
    MW2_TRACE_WRAP(void, DestroyDescriptorSetLayout, (VkDevice d, VkDescriptorSetLayout h, const VkAllocationCallbacks* a), (d, h, a))
    MW2_TRACE_WRAP(VkResult, CreatePipelineLayout, (VkDevice d, const VkPipelineLayoutCreateInfo* i, const VkAllocationCallbacks* a, VkPipelineLayout* o), (d, i, a, o))
    MW2_TRACE_WRAP(void, DestroyPipelineLayout, (VkDevice d, VkPipelineLayout h, const VkAllocationCallbacks* a), (d, h, a))
    MW2_TRACE_WRAP(VkResult, CreateGraphicsPipelines, (VkDevice d, VkPipelineCache c, uint32_t n, const VkGraphicsPipelineCreateInfo* i, const VkAllocationCallbacks* a, VkPipeline* o), (d, c, n, i, a, o))
    MW2_TRACE_WRAP(VkResult, CreateComputePipelines, (VkDevice d, VkPipelineCache c, uint32_t n, const VkComputePipelineCreateInfo* i, const VkAllocationCallbacks* a, VkPipeline* o), (d, c, n, i, a, o))
    MW2_TRACE_WRAP(void, DestroyPipeline, (VkDevice d, VkPipeline h, const VkAllocationCallbacks* a), (d, h, a))
    MW2_TRACE_WRAP(VkResult, CreateShaderModule, (VkDevice d, const VkShaderModuleCreateInfo* i, const VkAllocationCallbacks* a, VkShaderModule* o), (d, i, a, o))
    MW2_TRACE_WRAP(void, DestroyShaderModule, (VkDevice d, VkShaderModule h, const VkAllocationCallbacks* a), (d, h, a))
    MW2_TRACE_WRAP(VkResult, CreateCommandPool, (VkDevice d, const VkCommandPoolCreateInfo* i, const VkAllocationCallbacks* a, VkCommandPool* o), (d, i, a, o))
    MW2_TRACE_WRAP(void, DestroyCommandPool, (VkDevice d, VkCommandPool h, const VkAllocationCallbacks* a), (d, h, a))
    MW2_TRACE_WRAP(VkResult, ResetCommandPool, (VkDevice d, VkCommandPool h, VkCommandPoolResetFlags f), (d, h, f))
    MW2_TRACE_WRAP(VkResult, AllocateCommandBuffers, (VkDevice d, const VkCommandBufferAllocateInfo* i, VkCommandBuffer* o), (d, i, o))
    MW2_TRACE_WRAP(void, FreeCommandBuffers, (VkDevice d, VkCommandPool p, uint32_t n, const VkCommandBuffer* c), (d, p, n, c))
    MW2_TRACE_WRAP(VkResult, BeginCommandBuffer, (VkCommandBuffer c, const VkCommandBufferBeginInfo* i), (c, i))
    MW2_TRACE_WRAP(VkResult, EndCommandBuffer, (VkCommandBuffer c), (c))
    MW2_TRACE_WRAP(VkResult, CreateFence, (VkDevice d, const VkFenceCreateInfo* i, const VkAllocationCallbacks* a, VkFence* o), (d, i, a, o))
    MW2_TRACE_WRAP(void, DestroyFence, (VkDevice d, VkFence h, const VkAllocationCallbacks* a), (d, h, a))
    MW2_TRACE_WRAP(VkResult, ResetFences, (VkDevice d, uint32_t n, const VkFence* f), (d, n, f))
    MW2_TRACE_WRAP(VkResult, CreateQueryPool, (VkDevice d, const VkQueryPoolCreateInfo* i, const VkAllocationCallbacks* a, VkQueryPool* o), (d, i, a, o))
    MW2_TRACE_WRAP(void, DestroyQueryPool, (VkDevice d, VkQueryPool h, const VkAllocationCallbacks* a), (d, h, a))
    MW2_TRACE_WRAP(VkResult, QueueSubmit, (VkQueue q, uint32_t n, const VkSubmitInfo* s, VkFence f), (q, n, s, f))
    MW2_TRACE_WRAP(void, CmdBindDescriptorSets, (VkCommandBuffer c, VkPipelineBindPoint b, VkPipelineLayout l, uint32_t first, uint32_t n, const VkDescriptorSet* s, uint32_t nd, const uint32_t* dyn), (c, b, l, first, n, s, nd, dyn))
    MW2_TRACE_WRAP(void, CmdPipelineBarrier, (VkCommandBuffer c, VkPipelineStageFlags src, VkPipelineStageFlags dst, VkDependencyFlags dep, uint32_t nm, const VkMemoryBarrier* m, uint32_t nb, const VkBufferMemoryBarrier* b, uint32_t ni, const VkImageMemoryBarrier* i), (c, src, dst, dep, nm, m, nb, b, ni, i))
    MW2_TRACE_WRAP(void, CmdBeginQuery, (VkCommandBuffer c, VkQueryPool p, uint32_t q, VkQueryControlFlags f), (c, p, q, f))
    MW2_TRACE_WRAP(void, CmdEndQuery, (VkCommandBuffer c, VkQueryPool p, uint32_t q), (c, p, q))
    MW2_TRACE_WRAP(void, CmdResetQueryPool, (VkCommandBuffer c, VkQueryPool p, uint32_t first, uint32_t n), (c, p, first, n))
    MW2_TRACE_WRAP(void, CmdWriteTimestamp, (VkCommandBuffer c, VkPipelineStageFlagBits s, VkQueryPool p, uint32_t q), (c, s, p, q))
    MW2_TRACE_WRAP(void, CmdBindIndexBuffer, (VkCommandBuffer c, VkBuffer b, VkDeviceSize o, VkIndexType t), (c, b, o, t))
    MW2_TRACE_WRAP(void, CmdBindPipeline, (VkCommandBuffer c, VkPipelineBindPoint b, VkPipeline p), (c, b, p))
    MW2_TRACE_WRAP(void, CmdDraw, (VkCommandBuffer c, uint32_t v, uint32_t i, uint32_t fv, uint32_t fi), (c, v, i, fv, fi))
    MW2_TRACE_WRAP(void, CmdDrawIndexed, (VkCommandBuffer c, uint32_t n, uint32_t i, uint32_t fi, int32_t vo, uint32_t fin), (c, n, i, fi, vo, fin))
    MW2_TRACE_WRAP(void, CmdSetDepthBias, (VkCommandBuffer c, float k, float clamp, float slope), (c, k, clamp, slope))
    MW2_TRACE_WRAP(void, CmdSetScissor, (VkCommandBuffer c, uint32_t first, uint32_t n, const VkRect2D* r), (c, first, n, r))
    MW2_TRACE_WRAP(void, CmdSetViewport, (VkCommandBuffer c, uint32_t first, uint32_t n, const VkViewport* v), (c, first, n, v))
    MW2_TRACE_WRAP(void, CmdSetStencilCompareMask, (VkCommandBuffer c, VkStencilFaceFlags f, uint32_t v), (c, f, v))
    MW2_TRACE_WRAP(void, CmdSetStencilWriteMask, (VkCommandBuffer c, VkStencilFaceFlags f, uint32_t v), (c, f, v))
    MW2_TRACE_WRAP(void, CmdSetStencilReference, (VkCommandBuffer c, VkStencilFaceFlags f, uint32_t v), (c, f, v))
    MW2_TRACE_WRAP(void, CmdBlitImage, (VkCommandBuffer c, VkImage s, VkImageLayout sl, VkImage d, VkImageLayout dl, uint32_t n, const VkImageBlit* r, VkFilter f), (c, s, sl, d, dl, n, r, f))
    MW2_TRACE_WRAP(void, CmdCopyImage, (VkCommandBuffer c, VkImage s, VkImageLayout sl, VkImage d, VkImageLayout dl, uint32_t n, const VkImageCopy* r), (c, s, sl, d, dl, n, r))
    MW2_TRACE_WRAP(void, CmdResolveImage, (VkCommandBuffer c, VkImage s, VkImageLayout sl, VkImage d, VkImageLayout dl, uint32_t n, const VkImageResolve* r), (c, s, sl, d, dl, n, r))
    MW2_TRACE_WRAP(void, CmdClearColorImage, (VkCommandBuffer c, VkImage i, VkImageLayout l, const VkClearColorValue* v, uint32_t n, const VkImageSubresourceRange* r), (c, i, l, v, n, r))
    MW2_TRACE_WRAP(void, CmdClearDepthStencilImage, (VkCommandBuffer c, VkImage i, VkImageLayout l, const VkClearDepthStencilValue* v, uint32_t n, const VkImageSubresourceRange* r), (c, i, l, v, n, r))
    MW2_TRACE_WRAP(void, CmdCopyBufferToImage, (VkCommandBuffer c, VkBuffer b, VkImage i, VkImageLayout l, uint32_t n, const VkBufferImageCopy* r), (c, b, i, l, n, r))
    MW2_TRACE_WRAP(void, CmdCopyImageToBuffer, (VkCommandBuffer c, VkImage i, VkImageLayout l, VkBuffer b, uint32_t n, const VkBufferImageCopy* r), (c, i, l, b, n, r))
    MW2_TRACE_WRAP(void, CmdDispatch, (VkCommandBuffer c, uint32_t x, uint32_t y, uint32_t z), (c, x, y, z))
    MW2_TRACE_WRAP(void, CmdUpdateBuffer, (VkCommandBuffer c, VkBuffer b, VkDeviceSize o, VkDeviceSize n, const void* data), (c, b, o, n, data))
#undef MW2_TRACE_WRAP
}

// trace.cpp makes the real calls.
#ifndef MW2_TRACE_IMPLEMENTATION
#define vkCreateImage vk::trace::CreateImage
#define vkDestroyImage vk::trace::DestroyImage
#define vkCreateImageView vk::trace::CreateImageView
#define vkDestroyImageView vk::trace::DestroyImageView
#define vkCreateBuffer vk::trace::CreateBuffer
#define vkDestroyBuffer vk::trace::DestroyBuffer
#define vkAllocateMemory vk::trace::AllocateMemory
#define vkFreeMemory vk::trace::FreeMemory
#define vkBindImageMemory vk::trace::BindImageMemory
#define vkBindBufferMemory vk::trace::BindBufferMemory
#define vkCreateSampler vk::trace::CreateSampler
#define vkDestroySampler vk::trace::DestroySampler
#define vkCreateDescriptorPool vk::trace::CreateDescriptorPool
#define vkDestroyDescriptorPool vk::trace::DestroyDescriptorPool
#define vkAllocateDescriptorSets vk::trace::AllocateDescriptorSets
#define vkFreeDescriptorSets vk::trace::FreeDescriptorSets
#define vkUpdateDescriptorSets vk::trace::UpdateDescriptorSets
#define vkCreateDescriptorSetLayout vk::trace::CreateDescriptorSetLayout
#define vkDestroyDescriptorSetLayout vk::trace::DestroyDescriptorSetLayout
#define vkCreatePipelineLayout vk::trace::CreatePipelineLayout
#define vkDestroyPipelineLayout vk::trace::DestroyPipelineLayout
#define vkCreateGraphicsPipelines vk::trace::CreateGraphicsPipelines
#define vkCreateComputePipelines vk::trace::CreateComputePipelines
#define vkDestroyPipeline vk::trace::DestroyPipeline
#define vkCreateShaderModule vk::trace::CreateShaderModule
#define vkDestroyShaderModule vk::trace::DestroyShaderModule
#define vkCreateCommandPool vk::trace::CreateCommandPool
#define vkDestroyCommandPool vk::trace::DestroyCommandPool
#define vkResetCommandPool vk::trace::ResetCommandPool
#define vkAllocateCommandBuffers vk::trace::AllocateCommandBuffers
#define vkFreeCommandBuffers vk::trace::FreeCommandBuffers
#define vkBeginCommandBuffer vk::trace::BeginCommandBuffer
#define vkEndCommandBuffer vk::trace::EndCommandBuffer
#define vkCreateFence vk::trace::CreateFence
#define vkDestroyFence vk::trace::DestroyFence
#define vkResetFences vk::trace::ResetFences
#define vkCreateQueryPool vk::trace::CreateQueryPool
#define vkDestroyQueryPool vk::trace::DestroyQueryPool
#define vkQueueSubmit vk::trace::QueueSubmit
#define vkCmdBindDescriptorSets vk::trace::CmdBindDescriptorSets
#define vkCmdPipelineBarrier vk::trace::CmdPipelineBarrier
#define vkCmdBeginQuery vk::trace::CmdBeginQuery
#define vkCmdEndQuery vk::trace::CmdEndQuery
#define vkCmdResetQueryPool vk::trace::CmdResetQueryPool
#define vkCmdWriteTimestamp vk::trace::CmdWriteTimestamp
#define vkCmdBindIndexBuffer vk::trace::CmdBindIndexBuffer
#define vkCmdBindPipeline vk::trace::CmdBindPipeline
#define vkCmdDraw vk::trace::CmdDraw
#define vkCmdDrawIndexed vk::trace::CmdDrawIndexed
#define vkCmdSetDepthBias vk::trace::CmdSetDepthBias
#define vkCmdSetScissor vk::trace::CmdSetScissor
#define vkCmdSetViewport vk::trace::CmdSetViewport
#define vkCmdSetStencilCompareMask vk::trace::CmdSetStencilCompareMask
#define vkCmdSetStencilWriteMask vk::trace::CmdSetStencilWriteMask
#define vkCmdSetStencilReference vk::trace::CmdSetStencilReference
#define vkCmdBlitImage vk::trace::CmdBlitImage
#define vkCmdCopyImage vk::trace::CmdCopyImage
#define vkCmdResolveImage vk::trace::CmdResolveImage
#define vkCmdClearColorImage vk::trace::CmdClearColorImage
#define vkCmdClearDepthStencilImage vk::trace::CmdClearDepthStencilImage
#define vkCmdCopyBufferToImage vk::trace::CmdCopyBufferToImage
#define vkCmdCopyImageToBuffer vk::trace::CmdCopyImageToBuffer
#define vkCmdDispatch vk::trace::CmdDispatch
#define vkCmdUpdateBuffer vk::trace::CmdUpdateBuffer
#endif

#else
#define MW2_TRACE 0
#endif
