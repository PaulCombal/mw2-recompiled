// The command trace, MW2_DETERMINISTIC and the forced fallbacks (trace.h).
// Empty in a release build (-DMW2_DIAGNOSTICS=0), not even a header.
#if !defined(MW2_DIAGNOSTICS) || MW2_DIAGNOSTICS
#define MW2_TRACE_IMPLEMENTATION
#include "trace.h"
#endif

#if MW2_TRACE

#include "../../env.h"
#include "../../log.h"

#include <atomic>
#include <bit>
#include <chrono>
#include <cstdarg>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <deque>
#include <mutex>
#include <string>
#include <unordered_map>

#if defined(__x86_64__) || defined(_M_X64)
#include <smmintrin.h>
#endif

namespace vk::trace::detail
{
    bool on = false;
    bool deterministic = false;
}

namespace
{
    using namespace vk::trace;

    constexpr const char* kKindNames[kKinds] = {
        "Image", "View", "Buffer", "Memory", "Sampler", "DescriptorPool", "Set", "Pipeline", "Module",
        "CommandPool", "Cmd", "Fence", "QueryPool", "SetLayout", "PipelineLayout", "Semaphore",
        "Device", "Physical", "Queue", "Cache",
    };

    struct Pending
    {
        uint64_t position;
        std::string text;
        int64_t frame;   // a frame marker's frame, or -1
    };

    struct State
    {
        std::mutex lock;
        std::FILE* file = nullptr;
        std::string path;
        uint64_t first = 0, frames = UINT64_MAX;   // the window, in renderer frames
        uint64_t frame = 0;                         // as the lines written so far have it
        uint64_t generated = 0;                     // the last frame marker made
        uint64_t seq = 0, written = 0, unregistered = 0;
        std::unordered_map<uint64_t, std::string> names[kKinds];
        uint64_t ordinals[kKinds]{};
        std::string next[kKinds];
        uint64_t (*position)() = nullptr;
        bool recorderRunning = false;
        uint64_t recorderAt = 0;
        std::deque<Pending> pending;
        bool finished = false;
        // The extension commands the wrappers call.
        PFN_vkCmdBeginRenderingKHR beginRendering = nullptr;
        PFN_vkCmdEndRenderingKHR endRendering = nullptr;
        PFN_vkCmdSetCullModeEXT setCullMode = nullptr;
        PFN_vkCmdSetFrontFaceEXT setFrontFace = nullptr;
        PFN_vkCmdSetDepthTestEnableEXT setDepthTestEnable = nullptr;
        PFN_vkCmdSetDepthWriteEnableEXT setDepthWriteEnable = nullptr;
        PFN_vkCmdSetDepthCompareOpEXT setDepthCompareOp = nullptr;
        PFN_vkCmdSetStencilTestEnableEXT setStencilTestEnable = nullptr;
        PFN_vkCmdSetStencilOpEXT setStencilOp = nullptr;
        PFN_vkCmdDrawMultiEXT drawMulti = nullptr;
        PFN_vkCmdDrawMultiIndexedEXT drawMultiIndexed = nullptr;
    } s;
    thread_local bool t_recorder = false;
    // The line being made falls outside the window: it is counted, not written,
    // and not formatted either (under s.lock).
    bool g_skip = false;

    // A line made now is written at a frame between the writer's and the last marker
    // made; whether that is outside the window for certain.
    bool OutsideLocked()
    {
        return (s.frame >= s.first && s.frame - s.first >= s.frames) || s.generated < s.first;
    }

    Forced g_forced;
    ForcedCounts g_counts;
    std::chrono::steady_clock::time_point g_started;

    // Under s.lock.
    void WriteLocked(const std::string& text, int64_t frame)
    {
        s.seq++;
        if (frame >= 0) s.frame = uint64_t(frame);
        if (!s.file || text.empty() || s.frame < s.first || s.frame - s.first >= s.frames) return;
        std::fprintf(s.file, "%llu %s\n", (unsigned long long)s.seq, text.c_str());
        s.written++;
        // The run leaves by _exit: what is buffered at a frame's end is kept.
        if (frame >= 0) std::fflush(s.file);
    }

    // A line made on the recorder goes straight out: the recorder makes calls in the
    // order they were queued. One made anywhere else (the consumer) goes after the
    // calls queued before it, so it waits while the recorder has not made them.
    void EmitLocked(std::string&& text, int64_t frame = -1)
    {
        if (t_recorder || !s.recorderRunning || !s.position) { WriteLocked(text, frame); return; }
        const uint64_t at = s.position();
        if (s.pending.empty() && s.recorderAt >= at) { WriteLocked(text, frame); return; }
        s.pending.push_back({ at, std::move(text), frame });
    }

    void Append(std::string& out, const char* format, ...)
    {
        if (g_skip) return;
        char buffer[256];
        va_list args;
        va_start(args, format);
        const int n = std::vsnprintf(buffer, sizeof buffer, format, args);
        va_end(args);
        if (n < 0) return;
        if (size_t(n) < sizeof buffer) { out.append(buffer, size_t(n)); return; }
        std::string big(size_t(n) + 1, '\0');
        va_start(args, format);
        std::vsnprintf(big.data(), big.size(), format, args);
        va_end(args);
        out.append(big.data(), size_t(n));
    }

    uint32_t Bits(float value) { return std::bit_cast<uint32_t>(value); }

    // One line, built under the lock and emitted when it goes out of scope.
    struct Line
    {
        std::unique_lock<std::mutex> held;
        std::string text;
        explicit Line(const char* command) : held(s.lock)
        {
            g_skip = OutsideLocked();
            if (!g_skip) text = command;
        }
        ~Line()
        {
            if (g_skip) text.clear();
            g_skip = false;
            EmitLocked(std::move(text));
        }

        Line& u(const char* key, uint64_t value) { Append(text, " %s=%llu", key, (unsigned long long)value); return *this; }
        Line& i(const char* key, int64_t value) { Append(text, " %s=%lld", key, (long long)value); return *this; }
        Line& x(const char* key, uint64_t value) { Append(text, " %s=0x%llx", key, (unsigned long long)value); return *this; }
        Line& f(const char* key, float value) { Append(text, " %s=%08x", key, Bits(value)); return *this; }
        Line& raw(const char* chunk) { if (!g_skip) text += chunk; return *this; }
        Line& key(const char* key) { Append(text, " %s=", key); return *this; }

        // A handle by its name; "?" for one nobody named, counted.
        void name(Kind kind, uint64_t handle)
        {
            if (!handle) { if (!g_skip) text += '0'; return; }
            auto found = s.names[kind].find(handle);
            if (found != s.names[kind].end()) { if (!g_skip) text += found->second; return; }
            if (!g_skip) text += '?';
            s.unregistered++;
        }
        template <class H> Line& h(const char* key, Kind kind, H handle)
        {
            Append(text, " %s=", key);
            name(kind, Raw(handle));
            return *this;
        }
        template <class H> Line& hs(const char* key, Kind kind, const H* handles, uint32_t count)
        {
            Append(text, " %s=[", key);
            for (uint32_t n = 0; n < count; n++)
            {
                if (n) text += ',';
                name(kind, handles ? Raw(handles[n]) : 0);
            }
            text += ']';
            return *this;
        }
        // A handle just made: named NameNext's way or by its ordinal.
        template <class H> Line& made(const char* key, Kind kind, H handle)
        {
            const uint64_t raw = Raw(handle);
            if (raw)
            {
                std::string& next = s.next[kind];
                if (!next.empty()) { s.names[kind][raw] = std::move(next); next.clear(); }
                else s.names[kind][raw] = std::string(kKindNames[kind]) + std::to_string(s.ordinals[kind]++);
            }
            return h(key, kind, handle);
        }
        template <class H> Line& gone(const char* key, Kind kind, H handle)
        {
            h(key, kind, handle);
            if (Raw(handle)) s.names[kind].erase(Raw(handle));
            return *this;
        }
        template <class T> Line& us(const char* key, const T* values, uint32_t count)
        {
            Append(text, " %s=[", key);
            for (uint32_t n = 0; n < count; n++) Append(text, n ? ",%llu" : "%llu", (unsigned long long)values[n]);
            text += ']';
            return *this;
        }
        // The sTypes of a pNext chain, and the structures the renderer chains decoded.
        Line& next(const void* chain)
        {
            text += " next=[";
            bool firstOne = true;
            for (auto* at = static_cast<const VkBaseInStructure*>(chain); at; at = at->pNext)
            {
                if (!firstOne) text += ',';
                firstOne = false;
                Append(text, "%u", unsigned(at->sType));
                switch (at->sType)
                {
                case VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO:
                {
                    auto* r = reinterpret_cast<const VkPipelineRenderingCreateInfo*>(at);
                    Append(text, "{view=%u colour=", r->viewMask);
                    for (uint32_t n = 0; n < r->colorAttachmentCount; n++)
                        Append(text, n ? ",%u" : "%u", unsigned(r->pColorAttachmentFormats[n]));
                    Append(text, " depth=%u stencil=%u}", unsigned(r->depthAttachmentFormat),
                           unsigned(r->stencilAttachmentFormat));
                    break;
                }
                case VK_STRUCTURE_TYPE_PIPELINE_LIBRARY_CREATE_INFO_KHR:
                {
                    auto* l = reinterpret_cast<const VkPipelineLibraryCreateInfoKHR*>(at);
                    text += '{';
                    for (uint32_t n = 0; n < l->libraryCount; n++)
                    {
                        if (n) text += ',';
                        name(kPipeline, Raw(l->pLibraries[n]));
                    }
                    text += '}';
                    break;
                }
                case VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_LIBRARY_CREATE_INFO_EXT:
                    Append(text, "{0x%x}", unsigned(reinterpret_cast<const VkGraphicsPipelineLibraryCreateInfoEXT*>(at)->flags));
                    break;
                case VK_STRUCTURE_TYPE_IMAGE_FORMAT_LIST_CREATE_INFO:
                {
                    auto* l = reinterpret_cast<const VkImageFormatListCreateInfo*>(at);
                    text += '{';
                    for (uint32_t n = 0; n < l->viewFormatCount; n++)
                        Append(text, n ? ",%u" : "%u", unsigned(l->pViewFormats[n]));
                    text += '}';
                    break;
                }
                case VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_BINDING_FLAGS_CREATE_INFO:
                {
                    auto* l = reinterpret_cast<const VkDescriptorSetLayoutBindingFlagsCreateInfo*>(at);
                    text += '{';
                    for (uint32_t n = 0; n < l->bindingCount; n++)
                        Append(text, n ? ",0x%x" : "0x%x", unsigned(l->pBindingFlags[n]));
                    text += '}';
                    break;
                }
                case VK_STRUCTURE_TYPE_DESCRIPTOR_SET_VARIABLE_DESCRIPTOR_COUNT_ALLOCATE_INFO:
                {
                    auto* l = reinterpret_cast<const VkDescriptorSetVariableDescriptorCountAllocateInfo*>(at);
                    text += '{';
                    for (uint32_t n = 0; n < l->descriptorSetCount; n++)
                        Append(text, n ? ",%u" : "%u", l->pDescriptorCounts[n]);
                    text += '}';
                    break;
                }
                case VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO:
                {
                    auto* d = reinterpret_cast<const VkMemoryDedicatedAllocateInfo*>(at);
                    text += '{';
                    name(kImage, Raw(d->image));
                    text += ',';
                    name(kBuffer, Raw(d->buffer));
                    text += '}';
                    break;
                }
                case VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_FLAGS_INFO:
                    Append(text, "{0x%x}", unsigned(reinterpret_cast<const VkMemoryAllocateFlagsInfo*>(at)->flags));
                    break;
                case VK_STRUCTURE_TYPE_IMAGE_VIEW_USAGE_CREATE_INFO:
                    Append(text, "{0x%x}", unsigned(reinterpret_cast<const VkImageViewUsageCreateInfo*>(at)->usage));
                    break;
                default:
                    break;
                }
            }
            text += ']';
            return *this;
        }
        Line& range(const VkImageSubresourceRange& r)
        {
            Append(text, "%x/%u/%u/%u/%u", unsigned(r.aspectMask), r.baseMipLevel, r.levelCount, r.baseArrayLayer,
                   r.layerCount);
            return *this;
        }
        Line& layers(const VkImageSubresourceLayers& r)
        {
            Append(text, "%x/%u/%u/%u", unsigned(r.aspectMask), r.mipLevel, r.baseArrayLayer, r.layerCount);
            return *this;
        }
        Line& offset(const VkOffset3D& o) { Append(text, "%d,%d,%d", o.x, o.y, o.z); return *this; }
        Line& extent(const VkExtent3D& e) { Append(text, "%ux%ux%u", e.width, e.height, e.depth); return *this; }
        Line& rect(const VkRect2D& r)
        {
            Append(text, "%d,%d,%ux%u", r.offset.x, r.offset.y, r.extent.width, r.extent.height);
            return *this;
        }
        Line& ranges(const char* key, const VkImageSubresourceRange* r, uint32_t count)
        {
            Append(text, " %s=[", key);
            for (uint32_t n = 0; n < count; n++) { if (n) text += ','; range(r[n]); }
            text += ']';
            return *this;
        }
        Line& copies(const char* key, const VkBufferImageCopy* r, uint32_t count)
        {
            Append(text, " %s=[", key);
            for (uint32_t n = 0; n < count; n++)
            {
                if (n) text += ',';
                Append(text, "(%llu,%u,%u,", (unsigned long long)r[n].bufferOffset, r[n].bufferRowLength,
                       r[n].bufferImageHeight);
                layers(r[n].imageSubresource).raw(",").offset(r[n].imageOffset).raw(",").extent(r[n].imageExtent).raw(")");
            }
            text += ']';
            return *this;
        }
        Line& stencil(const VkStencilOpState& o)
        {
            Append(text, "(%u,%u,%u,%u,%x,%x,%x)", unsigned(o.failOp), unsigned(o.passOp), unsigned(o.depthFailOp),
                   unsigned(o.compareOp), o.compareMask, o.writeMask, o.reference);
            return *this;
        }
        Line& attachment(const char* key, const VkRenderingAttachmentInfo* a)
        {
            if (key) Append(text, " %s=", key);
            if (!a) { text += '-'; return *this; }
            text += '(';
            name(kView, Raw(a->imageView));
            Append(text, ",%u,%u,", unsigned(a->imageLayout), unsigned(a->resolveMode));
            name(kView, Raw(a->resolveImageView));
            Append(text, ",%u,%u,%u,%08x,%08x,%08x,%08x)", unsigned(a->resolveImageLayout), unsigned(a->loadOp),
                   unsigned(a->storeOp), a->clearValue.color.uint32[0], a->clearValue.color.uint32[1],
                   a->clearValue.color.uint32[2], a->clearValue.color.uint32[3]);
            return *this;
        }
    };

    void Barriers(Line& l, uint32_t nm, const VkMemoryBarrier* m, uint32_t nb, const VkBufferMemoryBarrier* b,
                  uint32_t ni, const VkImageMemoryBarrier* im)
    {
        l.raw(" memory=[");
        for (uint32_t n = 0; n < nm; n++)
            Append(l.text, n ? ",(%x,%x)" : "(%x,%x)", unsigned(m[n].srcAccessMask), unsigned(m[n].dstAccessMask));
        l.raw("] buffers=[");
        for (uint32_t n = 0; n < nb; n++)
        {
            if (n) l.raw(",");
            Append(l.text, "(%x,%x,%d,%d,", unsigned(b[n].srcAccessMask), unsigned(b[n].dstAccessMask),
                   int(b[n].srcQueueFamilyIndex), int(b[n].dstQueueFamilyIndex));
            l.name(kBuffer, Raw(b[n].buffer));
            Append(l.text, ",%llu,%llx)", (unsigned long long)b[n].offset, (unsigned long long)b[n].size);
        }
        l.raw("] images=[");
        for (uint32_t n = 0; n < ni; n++)
        {
            if (n) l.raw(",");
            Append(l.text, "(%x,%x,%u,%u,%d,%d,", unsigned(im[n].srcAccessMask), unsigned(im[n].dstAccessMask),
                   unsigned(im[n].oldLayout), unsigned(im[n].newLayout), int(im[n].srcQueueFamilyIndex),
                   int(im[n].dstQueueFamilyIndex));
            l.name(kImage, Raw(im[n].image));
            l.raw(",").range(im[n].subresourceRange).raw(")");
        }
        l.raw("]");
    }

    void Stages(Line& l, const VkPipelineShaderStageCreateInfo* stages, uint32_t count)
    {
        l.raw(" stages=[");
        for (uint32_t n = 0; n < count; n++)
        {
            if (n) l.raw(",");
            Append(l.text, "(%x,%x,", unsigned(stages[n].stage), unsigned(stages[n].flags));
            l.name(kModule, Raw(stages[n].module));
            Append(l.text, ",%s", stages[n].pName ? stages[n].pName : "-");
            if (const VkSpecializationInfo* spec = stages[n].pSpecializationInfo)
            {
                for (uint32_t e = 0; e < spec->mapEntryCount; e++)
                    Append(l.text, ",spec%u@%u:%zu", spec->pMapEntries[e].constantID, spec->pMapEntries[e].offset,
                           spec->pMapEntries[e].size);
                Append(l.text, ",data%016llx", (unsigned long long)Fnv64(spec->pData, spec->dataSize));
            }
            l.next(stages[n].pNext);
            l.raw(")");
        }
        l.raw("]");
    }

    void Graphics(Line& l, const VkGraphicsPipelineCreateInfo& p)
    {
        l.x("flags", p.flags);
        Stages(l, p.pStages, p.stageCount);
        if (auto* in = p.pVertexInputState)
            l.u("bindings", in->vertexBindingDescriptionCount).u("attributes", in->vertexAttributeDescriptionCount);
        if (auto* a = p.pInputAssemblyState) l.u("topology", a->topology).u("restart", a->primitiveRestartEnable);
        if (auto* v = p.pViewportState) l.u("viewports", v->viewportCount).u("scissors", v->scissorCount);
        if (auto* r = p.pRasterizationState)
        {
            Append(l.text, " raster=(%u,%u,%u,%x,%u,%u,%08x,%08x,%08x,%08x)", r->depthClampEnable,
                   r->rasterizerDiscardEnable, unsigned(r->polygonMode), unsigned(r->cullMode), unsigned(r->frontFace),
                   r->depthBiasEnable, Bits(r->depthBiasConstantFactor), Bits(r->depthBiasClamp),
                   Bits(r->depthBiasSlopeFactor), Bits(r->lineWidth));
            l.next(r->pNext);
        }
        if (auto* m = p.pMultisampleState)
            Append(l.text, " samples=%u shading=(%u,%08x) mask=%x a2c=%u", unsigned(m->rasterizationSamples),
                   m->sampleShadingEnable, Bits(m->minSampleShading), m->pSampleMask ? *m->pSampleMask : 0xFFFFFFFFu,
                   m->alphaToCoverageEnable);
        if (auto* d = p.pDepthStencilState)
        {
            Append(l.text, " depth=(%u,%u,%u,%u,%u) front=", d->depthTestEnable, d->depthWriteEnable,
                   unsigned(d->depthCompareOp), d->depthBoundsTestEnable, d->stencilTestEnable);
            l.stencil(d->front).raw(" back=").stencil(d->back);
        }
        if (auto* b = p.pColorBlendState)
        {
            Append(l.text, " blend=(%u,%u,%08x,%08x,%08x,%08x)[", b->logicOpEnable, unsigned(b->logicOp),
                   Bits(b->blendConstants[0]), Bits(b->blendConstants[1]), Bits(b->blendConstants[2]),
                   Bits(b->blendConstants[3]));
            for (uint32_t n = 0; n < b->attachmentCount; n++)
            {
                const VkPipelineColorBlendAttachmentState& a = b->pAttachments[n];
                Append(l.text, "%s(%u,%u,%u,%u,%u,%u,%u,mask=%x)", n ? "," : "", a.blendEnable,
                       unsigned(a.srcColorBlendFactor), unsigned(a.dstColorBlendFactor), unsigned(a.colorBlendOp),
                       unsigned(a.srcAlphaBlendFactor), unsigned(a.dstAlphaBlendFactor), unsigned(a.alphaBlendOp),
                       unsigned(a.colorWriteMask));
            }
            l.raw("]");
        }
        if (auto* d = p.pDynamicState)
        {
            l.raw(" dynamic=[");
            for (uint32_t n = 0; n < d->dynamicStateCount; n++)
                Append(l.text, n ? ",%u" : "%u", unsigned(d->pDynamicStates[n]));
            l.raw("]");
        }
        l.h("layout", kPipelineLayout, p.layout).h("base", kPipeline, p.basePipelineHandle).i("baseIndex", p.basePipelineIndex);
        l.next(p.pNext);
    }

    bool KnownCommand(VkCommandBuffer command)
    {
        std::lock_guard lock(s.lock);
        return s.names[kCommand].count(Raw(command)) != 0;
    }

    void SubmitLine(const char* command, VkQueue queue, uint32_t count, const VkSubmitInfo* submits, VkFence fence)
    {
        Line l(command);
        l.h("queue", kQueue, queue).raw(" submits=[");
        for (uint32_t n = 0; n < count; n++)
        {
            const VkSubmitInfo& one = submits[n];
            if (n) l.raw(",");
            l.raw("{").hs("cmds", kCommand, one.pCommandBuffers, one.commandBufferCount)
                .hs("wait", kSemaphore, one.pWaitSemaphores, one.waitSemaphoreCount)
                .us("stages", one.pWaitDstStageMask, one.pWaitDstStageMask ? one.waitSemaphoreCount : 0)
                .hs("signal", kSemaphore, one.pSignalSemaphores, one.signalSemaphoreCount).next(one.pNext).raw("}");
        }
        l.raw("]").h("fence", kFence, fence);
    }

    uint64_t ParseUnsigned(const char* text, bool& ok)
    {
        char* end = nullptr;
        const unsigned long long value = std::strtoull(text, &end, 10);
        ok = end != text && *end == 0;
        return uint64_t(value);
    }
}

// ---- the instruments -----------------------------------------------------------

const Forced& vk::trace::Forcing() { return g_forced; }
ForcedCounts& vk::trace::Counts() { return g_counts; }

bool vk::trace::DeviceLostFaked()
{
    if (g_forced.fakeDeviceLost <= 0) return false;
    return std::chrono::duration<double>(std::chrono::steady_clock::now() - g_started).count() >=
           g_forced.fakeDeviceLost;
}

void vk::trace::Start(VkDevice device, VkPhysicalDevice physical, VkQueue queue, VkPipelineCache cache,
                      VkPipelineLayout layout, const VkDescriptorSetLayout* setLayouts, uint32_t setLayoutCount,
                      uint64_t (*position)())
{
    g_started = std::chrono::steady_clock::now();
    detail::deterministic = diag::Flag("MW2_DETERMINISTIC");
    g_forced.instanceDepthResolve = diag::Flag("MW2_FORCE_INSTANCE_DEPTH_RESOLVE");
    g_forced.resolveTwin = diag::Flag("MW2_FORCE_RESOLVE_TWIN");
    g_forced.drawnResolveSets = uint32_t(diag::Number("MW2_DRAWN_RESOLVE_SETS"));
    g_forced.resolveRemake = uint32_t(diag::Number("MW2_FORCE_RESOLVE_REMAKE"));
    g_forced.noMultiDraw = diag::Flag("MW2_NO_MULTI_DRAW");
    g_forced.loneUploads = diag::Flag("MW2_FORCE_LONE_UPLOADS");
    g_forced.stagingLeastKb = uint32_t(diag::Number("MW2_STAGING_LEAST_KB"));
    g_forced.fakeDeviceLost = diag::Real("MW2_FAKE_DEVICE_LOST", 0.0);
    if (detail::deterministic)
        LOGI("renderer: MW2_DETERMINISTIC: completed = submitted, no optimised links, no shader workers,"
             " every doomed texture destroyed at once, no GPU timing");
    if (g_forced.instanceDepthResolve) LOGI("renderer: MW2_FORCE_INSTANCE_DEPTH_RESOLVE: no drawn depth resolve");
    if (g_forced.resolveTwin) LOGI("renderer: MW2_FORCE_RESOLVE_TWIN: every multisampled resolve goes through the twin");
    if (g_forced.drawnResolveSets)
        LOGI("renderer: MW2_DRAWN_RESOLVE_SETS: %u sets for the drawn depth resolve", g_forced.drawnResolveSets);
    if (g_forced.resolveRemake)
        LOGI("renderer: MW2_FORCE_RESOLVE_REMAKE: every resolve image made again every %u frames",
             g_forced.resolveRemake);
    if (g_forced.noMultiDraw) LOGI("renderer: MW2_NO_MULTI_DRAW: one call a draw");
    if (g_forced.loneUploads) LOGI("renderer: MW2_FORCE_LONE_UPLOADS: every texture upload submitted alone");
    if (g_forced.stagingLeastKb)
        LOGI("renderer: MW2_STAGING_LEAST_KB: an upload slot's staging starts at %u KB", g_forced.stagingLeastKb);
    if (g_forced.fakeDeviceLost > 0)
        LOGI("renderer: MW2_FAKE_DEVICE_LOST: the device is lost %.1f s from now", g_forced.fakeDeviceLost);

    const char* asked = diag::Text("MW2_TRACE_COMMANDS");
    if (!asked || !*asked) return;
    std::lock_guard lock(s.lock);
    s.path = asked;
    // <file>[:<first frame>:<frames>]
    if (size_t last = s.path.rfind(':'); last != std::string::npos && last > 0)
        if (size_t middle = s.path.rfind(':', last - 1); middle != std::string::npos)
        {
            bool okFirst = false, okCount = false;
            const uint64_t first = ParseUnsigned(s.path.substr(middle + 1, last - middle - 1).c_str(), okFirst);
            const uint64_t frames = ParseUnsigned(s.path.substr(last + 1).c_str(), okCount);
            if (okFirst && okCount)
            {
                s.first = first;
                s.frames = frames;
                s.path.resize(middle);
            }
        }
    s.file = std::fopen(s.path.c_str(), "wb");
    if (!s.file) { LOGW("renderer: MW2_TRACE_COMMANDS: cannot write %s", s.path.c_str()); return; }
    std::setvbuf(s.file, nullptr, _IOFBF, 1 << 20);
    s.position = position;
    s.names[kDevice][Raw(device)] = "Device";
    s.names[kPhysical][Raw(physical)] = "Physical";
    s.names[kQueue][Raw(queue)] = "Queue";
    s.names[kCache][Raw(cache)] = "Cache";
    s.names[kPipelineLayout][Raw(layout)] = "Layout";
    for (uint32_t n = 0; n < setLayoutCount; n++)
        s.names[kSetLayout][Raw(setLayouts[n])] = "SetLayout" + std::to_string(n);
    detail::on = true;
    LOGI("renderer: MW2_TRACE_COMMANDS: tracing to %s, frames %llu to %llu", s.path.c_str(),
         (unsigned long long)s.first,
         (unsigned long long)(s.frames == UINT64_MAX ? UINT64_MAX : s.first + s.frames));
    EmitLocked("trace version=1");
}

void vk::trace::Register(Kind kind, uint64_t handle, const char* format, ...)
{
    if (!detail::on || !handle) return;
    char name[64];
    va_list args;
    va_start(args, format);
    std::vsnprintf(name, sizeof name, format, args);
    va_end(args);
    std::lock_guard lock(s.lock);
    s.names[kind][handle] = name;
}

void vk::trace::NameNext(Kind kind, const char* name)
{
    if (!detail::on) return;
    std::lock_guard lock(s.lock);
    s.next[kind] = name;
}

bool vk::trace::Wanted()
{
    if (!detail::on) return false;
    std::lock_guard lock(s.lock);
    return !OutsideLocked();
}

void vk::trace::Note(const char* format, ...)
{
    if (!detail::on) return;
    std::string text;
    char buffer[512];
    va_list args;
    va_start(args, format);
    std::vsnprintf(buffer, sizeof buffer, format, args);
    va_end(args);
    text = buffer;
    std::lock_guard lock(s.lock);
    EmitLocked(std::move(text));
}

void vk::trace::Frame(uint64_t frame)
{
    if (!detail::on) return;
    // Not Append: its g_skip belongs to the line another thread may be making.
    char buffer[40];
    std::snprintf(buffer, sizeof buffer, "frame n=%llu", (unsigned long long)frame);
    std::string text = buffer;
    std::lock_guard lock(s.lock);
    s.generated = frame;
    EmitLocked(std::move(text), int64_t(frame));
}

void vk::trace::Submission(VkQueue queue, const VkSubmitInfo& submit, VkFence fence)
{
    if (!detail::on) return;
    SubmitLine("QueueSubmit", queue, 1, &submit, fence);
}

void vk::trace::Present(VkImage image, uint64_t frame)
{
    if (!detail::on) return;
    Line l("Present");
    l.h("image", kImage, image).u("frame", frame);
    const auto found = s.names[kImage].find(Raw(image));
    if (found == s.names[kImage].end() || found->second.rfind("Present", 0) != 0)
    {
        // The window was handed an image that is not one of the four.
        l.raw(" unregistered");
        s.unregistered++;
    }
}

void vk::trace::ShutdownBegins()
{
    if (!detail::on) return;
    std::lock_guard lock(s.lock);
    EmitLocked("shutdown");
}

void vk::trace::Finish()
{
    if (!detail::on) return;
    std::lock_guard lock(s.lock);
    if (s.finished) return;
    for (Pending& line : s.pending) WriteLocked(line.text, line.frame);
    s.pending.clear();
    s.finished = true;
    if (s.file)
    {
        std::fflush(s.file);
        std::fclose(s.file);
        s.file = nullptr;
    }
    LOGI("renderer: command trace %s: %llu lines of %llu, %llu unregistered handles", s.path.c_str(),
         (unsigned long long)s.written, (unsigned long long)s.seq, (unsigned long long)s.unregistered);
}

void vk::trace::RecorderStarted()
{
    if (!detail::on) return;
    std::lock_guard lock(s.lock);
    s.recorderAt = 0;
    s.recorderRunning = true;
}

void vk::trace::RecorderThread() { t_recorder = true; }

void vk::trace::RecorderMade(uint64_t position)
{
    std::lock_guard lock(s.lock);
    s.recorderAt = position;
    while (!s.pending.empty() && s.pending.front().position <= position)
    {
        WriteLocked(s.pending.front().text, s.pending.front().frame);
        s.pending.pop_front();
    }
}

void vk::trace::RecorderStopped()
{
    if (!detail::on) return;
    std::lock_guard lock(s.lock);
    for (Pending& line : s.pending) WriteLocked(line.text, line.frame);
    s.pending.clear();
    s.recorderRunning = false;
}

namespace
{
    // FNV-1a over little-endian 64-bit words, then the bytes left over: eight times
    // as fast as a byte at a time over a submission's arena share.
    constexpr uint64_t kFnvBasis = 1469598103934665603ull, kFnvPrime = 1099511628211ull;
    uint64_t Mix(uint64_t hash, const uint8_t* at, size_t size)
    {
        size_t n = 0;
        for (; n + 8 <= size; n += 8)
        {
            uint64_t word;
            std::memcpy(&word, at + n, 8);
            hash = (hash ^ word) * kFnvPrime;
        }
        for (; n < size; n++) hash = (hash ^ at[n]) * kFnvPrime;
        return hash;
    }
}

uint64_t vk::trace::Fnv64(const void* bytes, size_t size)
{
    return Mix(kFnvBasis, static_cast<const uint8_t*>(bytes), size);
}

uint64_t vk::trace::FnvWriteCombined(const void* bytes, size_t size)
{
    // A plain read of write-combined memory is uncached, a few tens of MB a second
    // here; a streaming load takes a whole line at a time.
    alignas(64) uint8_t chunk[4096];
    uint64_t hash = kFnvBasis;
    const auto* at = static_cast<const uint8_t*>(bytes);
    while (size)
    {
        const size_t take = std::min(size, sizeof chunk);
        size_t n = 0;
#if defined(__x86_64__) || defined(_M_X64)
        if ((reinterpret_cast<uintptr_t>(at) & 15) == 0)
            for (; n + 16 <= take; n += 16)
                _mm_store_si128(reinterpret_cast<__m128i*>(chunk + n),
                                _mm_stream_load_si128(reinterpret_cast<const __m128i*>(at + n)));
#endif
        std::memcpy(chunk + n, at + n, take - n);
        hash = Mix(hash, chunk, take);   // whole words but at the very end: the same as Fnv64
        at += take;
        size -= take;
    }
    return hash;
}

void vk::trace::DumpBuiltin(const char* name, const std::vector<uint32_t>& words)
{
    const char* dir = diag::Text("MW2_DUMP_SHADERS");
    if (!dir) return;
    char path[512];
    std::snprintf(path, sizeof path, "%s/builtin_%s.spv", dir, name);
    if (std::FILE* f = std::fopen(path, "wb"))
    {
        std::fwrite(words.data(), sizeof(uint32_t), words.size(), f);
        std::fclose(f);
    }
}

// ---- the wrappers --------------------------------------------------------------

using namespace vk::trace;

VkResult vk::trace::CreateImageTraced(VkDevice d, const VkImageCreateInfo* i, const VkAllocationCallbacks* a, VkImage* o)
{
    const VkResult r = vkCreateImage(d, i, a, o);
    Line l("CreateImage");
    l.made("out", kImage, r == VK_SUCCESS ? *o : VK_NULL_HANDLE).x("flags", i->flags).u("type", i->imageType)
        .u("format", i->format).key("extent").extent(i->extent);
    l.u("mips", i->mipLevels).u("layers", i->arrayLayers).u("samples", i->samples).u("tiling", i->tiling)
        .x("usage", i->usage).u("sharing", i->sharingMode).u("initial", i->initialLayout).next(i->pNext).i("r", r);
    return r;
}

void vk::trace::DestroyImageTraced(VkDevice d, VkImage h, const VkAllocationCallbacks* a)
{
    { Line l("DestroyImage"); l.gone("image", kImage, h); }
    vkDestroyImage(d, h, a);
}

VkResult vk::trace::CreateImageViewTraced(VkDevice d, const VkImageViewCreateInfo* i, const VkAllocationCallbacks* a,
                                          VkImageView* o)
{
    const VkResult r = vkCreateImageView(d, i, a, o);
    Line l("CreateImageView");
    l.made("out", kView, r == VK_SUCCESS ? *o : VK_NULL_HANDLE).h("image", kImage, i->image).x("flags", i->flags)
        .u("type", i->viewType).u("format", i->format);
    Append(l.text, " swizzle=%u,%u,%u,%u range=", unsigned(i->components.r), unsigned(i->components.g),
           unsigned(i->components.b), unsigned(i->components.a));
    l.range(i->subresourceRange).next(i->pNext).i("r", r);
    return r;
}

void vk::trace::DestroyImageViewTraced(VkDevice d, VkImageView h, const VkAllocationCallbacks* a)
{
    { Line l("DestroyImageView"); l.gone("view", kView, h); }
    vkDestroyImageView(d, h, a);
}

VkResult vk::trace::CreateBufferTraced(VkDevice d, const VkBufferCreateInfo* i, const VkAllocationCallbacks* a, VkBuffer* o)
{
    const VkResult r = vkCreateBuffer(d, i, a, o);
    Line l("CreateBuffer");
    l.made("out", kBuffer, r == VK_SUCCESS ? *o : VK_NULL_HANDLE).x("flags", i->flags).u("size", i->size)
        .x("usage", i->usage).u("sharing", i->sharingMode).next(i->pNext).i("r", r);
    return r;
}

void vk::trace::DestroyBufferTraced(VkDevice d, VkBuffer h, const VkAllocationCallbacks* a)
{
    { Line l("DestroyBuffer"); l.gone("buffer", kBuffer, h); }
    vkDestroyBuffer(d, h, a);
}

VkResult vk::trace::AllocateMemoryTraced(VkDevice d, const VkMemoryAllocateInfo* i, const VkAllocationCallbacks* a,
                                         VkDeviceMemory* o)
{
    const VkResult r = vkAllocateMemory(d, i, a, o);
    Line l("AllocateMemory");
    l.made("out", kMemory, r == VK_SUCCESS ? *o : VK_NULL_HANDLE).u("size", i->allocationSize)
        .u("type", i->memoryTypeIndex).next(i->pNext).i("r", r);
    return r;
}

void vk::trace::FreeMemoryTraced(VkDevice d, VkDeviceMemory h, const VkAllocationCallbacks* a)
{
    { Line l("FreeMemory"); l.gone("memory", kMemory, h); }
    vkFreeMemory(d, h, a);
}

VkResult vk::trace::BindImageMemoryTraced(VkDevice d, VkImage h, VkDeviceMemory m, VkDeviceSize o)
{
    const VkResult r = vkBindImageMemory(d, h, m, o);
    Line l("BindImageMemory");
    l.h("image", kImage, h).h("memory", kMemory, m).u("offset", o).i("r", r);
    return r;
}

VkResult vk::trace::BindBufferMemoryTraced(VkDevice d, VkBuffer h, VkDeviceMemory m, VkDeviceSize o)
{
    const VkResult r = vkBindBufferMemory(d, h, m, o);
    Line l("BindBufferMemory");
    l.h("buffer", kBuffer, h).h("memory", kMemory, m).u("offset", o).i("r", r);
    return r;
}

VkResult vk::trace::CreateSamplerTraced(VkDevice d, const VkSamplerCreateInfo* i, const VkAllocationCallbacks* a,
                                        VkSampler* o)
{
    const VkResult r = vkCreateSampler(d, i, a, o);
    Line l("CreateSampler");
    l.made("out", kSampler, r == VK_SUCCESS ? *o : VK_NULL_HANDLE).x("flags", i->flags).u("mag", i->magFilter)
        .u("min", i->minFilter).u("mip", i->mipmapMode);
    Append(l.text, " address=%u,%u,%u", unsigned(i->addressModeU), unsigned(i->addressModeV), unsigned(i->addressModeW));
    l.f("lodBias", i->mipLodBias).u("aniso", i->anisotropyEnable).f("maxAniso", i->maxAnisotropy)
        .u("compare", i->compareEnable).u("compareOp", i->compareOp).f("minLod", i->minLod).f("maxLod", i->maxLod)
        .u("border", i->borderColor).u("unnormalised", i->unnormalizedCoordinates).next(i->pNext).i("r", r);
    return r;
}

void vk::trace::DestroySamplerTraced(VkDevice d, VkSampler h, const VkAllocationCallbacks* a)
{
    { Line l("DestroySampler"); l.gone("sampler", kSampler, h); }
    vkDestroySampler(d, h, a);
}

VkResult vk::trace::CreateDescriptorPoolTraced(VkDevice d, const VkDescriptorPoolCreateInfo* i,
                                               const VkAllocationCallbacks* a, VkDescriptorPool* o)
{
    const VkResult r = vkCreateDescriptorPool(d, i, a, o);
    Line l("CreateDescriptorPool");
    l.made("out", kDescriptorPool, r == VK_SUCCESS ? *o : VK_NULL_HANDLE).x("flags", i->flags).u("maxSets", i->maxSets);
    l.raw(" sizes=[");
    for (uint32_t n = 0; n < i->poolSizeCount; n++)
        Append(l.text, n ? ",(%u,%u)" : "(%u,%u)", unsigned(i->pPoolSizes[n].type), i->pPoolSizes[n].descriptorCount);
    l.raw("]").next(i->pNext).i("r", r);
    return r;
}

void vk::trace::DestroyDescriptorPoolTraced(VkDevice d, VkDescriptorPool h, const VkAllocationCallbacks* a)
{
    { Line l("DestroyDescriptorPool"); l.gone("pool", kDescriptorPool, h); }
    vkDestroyDescriptorPool(d, h, a);
}

VkResult vk::trace::AllocateDescriptorSetsTraced(VkDevice d, const VkDescriptorSetAllocateInfo* i, VkDescriptorSet* o)
{
    const VkResult r = vkAllocateDescriptorSets(d, i, o);
    Line l("AllocateDescriptorSets");
    l.h("pool", kDescriptorPool, i->descriptorPool).hs("layouts", kSetLayout, i->pSetLayouts, i->descriptorSetCount);
    l.raw(" out=[");
    for (uint32_t n = 0; n < i->descriptorSetCount; n++)
    {
        if (n) l.raw(",");
        if (r == VK_SUCCESS)
        {
            const std::string name = std::string(kKindNames[kSet]) + std::to_string(s.ordinals[kSet]++);
            s.names[kSet][Raw(o[n])] = name;
            l.raw(name.c_str());
        }
        else l.raw("0");
    }
    l.raw("]").next(i->pNext).i("r", r);
    return r;
}

VkResult vk::trace::FreeDescriptorSetsTraced(VkDevice d, VkDescriptorPool p, uint32_t n, const VkDescriptorSet* sets)
{
    {
        Line l("FreeDescriptorSets");
        l.h("pool", kDescriptorPool, p).hs("sets", kSet, sets, n);
        for (uint32_t k = 0; k < n; k++) s.names[kSet].erase(Raw(sets[k]));
    }
    return vkFreeDescriptorSets(d, p, n, sets);
}

void vk::trace::UpdateDescriptorSetsTraced(VkDevice d, uint32_t nw, const VkWriteDescriptorSet* w, uint32_t nc,
                                           const VkCopyDescriptorSet* c)
{
    {
        Line l("UpdateDescriptorSets");
        l.raw(" writes=[");
        for (uint32_t n = 0; n < nw; n++)
        {
            const VkWriteDescriptorSet& one = w[n];
            if (n) l.raw(",");
            l.raw("{").h("set", kSet, one.dstSet).u("binding", one.dstBinding).u("element", one.dstArrayElement)
                .u("type", one.descriptorType).u("n", one.descriptorCount);
            if (one.pImageInfo)
            {
                l.raw(" images=[");
                for (uint32_t k = 0; k < one.descriptorCount; k++)
                {
                    if (k) l.raw(",");
                    l.raw("(");
                    l.name(kSampler, Raw(one.pImageInfo[k].sampler));
                    l.raw(",");
                    l.name(kView, Raw(one.pImageInfo[k].imageView));
                    Append(l.text, ",%u)", unsigned(one.pImageInfo[k].imageLayout));
                }
                l.raw("]");
            }
            if (one.pBufferInfo)
            {
                l.raw(" buffers=[");
                for (uint32_t k = 0; k < one.descriptorCount; k++)
                {
                    if (k) l.raw(",");
                    l.raw("(");
                    l.name(kBuffer, Raw(one.pBufferInfo[k].buffer));
                    Append(l.text, ",%llu,%llx)", (unsigned long long)one.pBufferInfo[k].offset,
                           (unsigned long long)one.pBufferInfo[k].range);
                }
                l.raw("]");
            }
            l.next(one.pNext).raw("}");
        }
        Append(l.text, "] copies=%u", nc);
    }
    vkUpdateDescriptorSets(d, nw, w, nc, c);
}

VkResult vk::trace::CreateDescriptorSetLayoutTraced(VkDevice d, const VkDescriptorSetLayoutCreateInfo* i,
                                                    const VkAllocationCallbacks* a, VkDescriptorSetLayout* o)
{
    const VkResult r = vkCreateDescriptorSetLayout(d, i, a, o);
    Line l("CreateDescriptorSetLayout");
    l.made("out", kSetLayout, r == VK_SUCCESS ? *o : VK_NULL_HANDLE).x("flags", i->flags).raw(" bindings=[");
    for (uint32_t n = 0; n < i->bindingCount; n++)
    {
        const VkDescriptorSetLayoutBinding& b = i->pBindings[n];
        Append(l.text, "%s(%u,%u,%u,%x,", n ? "," : "", b.binding, unsigned(b.descriptorType), b.descriptorCount,
               unsigned(b.stageFlags));
        if (b.pImmutableSamplers)
            for (uint32_t k = 0; k < b.descriptorCount; k++)
            {
                if (k) l.raw("/");
                l.name(kSampler, Raw(b.pImmutableSamplers[k]));
            }
        else l.raw("-");
        l.raw(")");
    }
    l.raw("]").next(i->pNext).i("r", r);
    return r;
}

void vk::trace::DestroyDescriptorSetLayoutTraced(VkDevice d, VkDescriptorSetLayout h, const VkAllocationCallbacks* a)
{
    { Line l("DestroyDescriptorSetLayout"); l.gone("layout", kSetLayout, h); }
    vkDestroyDescriptorSetLayout(d, h, a);
}

VkResult vk::trace::CreatePipelineLayoutTraced(VkDevice d, const VkPipelineLayoutCreateInfo* i,
                                               const VkAllocationCallbacks* a, VkPipelineLayout* o)
{
    const VkResult r = vkCreatePipelineLayout(d, i, a, o);
    Line l("CreatePipelineLayout");
    l.made("out", kPipelineLayout, r == VK_SUCCESS ? *o : VK_NULL_HANDLE).x("flags", i->flags)
        .hs("sets", kSetLayout, i->pSetLayouts, i->setLayoutCount).raw(" push=[");
    for (uint32_t n = 0; n < i->pushConstantRangeCount; n++)
        Append(l.text, n ? ",(%x,%u,%u)" : "(%x,%u,%u)", unsigned(i->pPushConstantRanges[n].stageFlags),
               i->pPushConstantRanges[n].offset, i->pPushConstantRanges[n].size);
    l.raw("]").next(i->pNext).i("r", r);
    return r;
}

void vk::trace::DestroyPipelineLayoutTraced(VkDevice d, VkPipelineLayout h, const VkAllocationCallbacks* a)
{
    { Line l("DestroyPipelineLayout"); l.gone("layout", kPipelineLayout, h); }
    vkDestroyPipelineLayout(d, h, a);
}

VkResult vk::trace::CreateGraphicsPipelinesTraced(VkDevice d, VkPipelineCache c, uint32_t n,
                                                  const VkGraphicsPipelineCreateInfo* i, const VkAllocationCallbacks* a,
                                                  VkPipeline* o)
{
    const VkResult r = vkCreateGraphicsPipelines(d, c, n, i, a, o);
    for (uint32_t k = 0; k < n; k++)
    {
        Line l("CreateGraphicsPipeline");
        l.made("out", kPipeline, r == VK_SUCCESS ? o[k] : VK_NULL_HANDLE).h("cache", kCache, c);
        Graphics(l, i[k]);
        l.i("r", r);
    }
    return r;
}

void vk::trace::StateLibrary(VkPipeline made, VkGraphicsPipelineLibraryFlagsEXT part,
                             const VkGraphicsPipelineCreateInfo& info, const void* next)
{
    if (!detail::on) return;
    Line l("CreateStateLibrary");
    l.h("out", kPipeline, made).x("part", part);
    VkGraphicsPipelineCreateInfo given = info;
    given.pNext = next;
    Graphics(l, given);
}

VkResult vk::trace::CreateComputePipelinesTraced(VkDevice d, VkPipelineCache c, uint32_t n,
                                                 const VkComputePipelineCreateInfo* i, const VkAllocationCallbacks* a,
                                                 VkPipeline* o)
{
    const VkResult r = vkCreateComputePipelines(d, c, n, i, a, o);
    for (uint32_t k = 0; k < n; k++)
    {
        Line l("CreateComputePipeline");
        l.made("out", kPipeline, r == VK_SUCCESS ? o[k] : VK_NULL_HANDLE).h("cache", kCache, c).x("flags", i[k].flags);
        Stages(l, &i[k].stage, 1);
        l.h("layout", kPipelineLayout, i[k].layout).next(i[k].pNext).i("r", r);
    }
    return r;
}

void vk::trace::DestroyPipelineTraced(VkDevice d, VkPipeline h, const VkAllocationCallbacks* a)
{
    { Line l("DestroyPipeline"); l.gone("pipeline", kPipeline, h); }
    vkDestroyPipeline(d, h, a);
}

VkResult vk::trace::CreateShaderModuleTraced(VkDevice d, const VkShaderModuleCreateInfo* i,
                                             const VkAllocationCallbacks* a, VkShaderModule* o)
{
    const VkResult r = vkCreateShaderModule(d, i, a, o);
    Line l("CreateShaderModule");
    l.made("out", kModule, r == VK_SUCCESS ? *o : VK_NULL_HANDLE).u("words", i->codeSize / 4);
    Append(l.text, " fnv=%016llx", (unsigned long long)Fnv64(i->pCode, i->codeSize));
    l.next(i->pNext).i("r", r);
    return r;
}

void vk::trace::DestroyShaderModuleTraced(VkDevice d, VkShaderModule h, const VkAllocationCallbacks* a)
{
    { Line l("DestroyShaderModule"); l.gone("module", kModule, h); }
    vkDestroyShaderModule(d, h, a);
}

VkResult vk::trace::CreateCommandPoolTraced(VkDevice d, const VkCommandPoolCreateInfo* i, const VkAllocationCallbacks* a,
                                            VkCommandPool* o)
{
    const VkResult r = vkCreateCommandPool(d, i, a, o);
    Line l("CreateCommandPool");
    l.made("out", kCommandPool, r == VK_SUCCESS ? *o : VK_NULL_HANDLE).x("flags", i->flags)
        .u("family", i->queueFamilyIndex).next(i->pNext).i("r", r);
    return r;
}

void vk::trace::DestroyCommandPoolTraced(VkDevice d, VkCommandPool h, const VkAllocationCallbacks* a)
{
    { Line l("DestroyCommandPool"); l.gone("pool", kCommandPool, h); }
    vkDestroyCommandPool(d, h, a);
}

VkResult vk::trace::ResetCommandPoolTraced(VkDevice d, VkCommandPool h, VkCommandPoolResetFlags f)
{
    { Line l("ResetCommandPool"); l.h("pool", kCommandPool, h).x("flags", f); }
    return vkResetCommandPool(d, h, f);
}

VkResult vk::trace::AllocateCommandBuffersTraced(VkDevice d, const VkCommandBufferAllocateInfo* i, VkCommandBuffer* o)
{
    const VkResult r = vkAllocateCommandBuffers(d, i, o);
    Line l("AllocateCommandBuffers");
    l.h("pool", kCommandPool, i->commandPool).u("level", i->level).raw(" out=[");
    for (uint32_t n = 0; n < i->commandBufferCount; n++)
    {
        if (n) l.raw(",");
        if (r == VK_SUCCESS)
        {
            const std::string name = std::string(kKindNames[kCommand]) + std::to_string(s.ordinals[kCommand]++);
            s.names[kCommand][Raw(o[n])] = name;
            l.raw(name.c_str());
        }
        else l.raw("0");
    }
    l.raw("]").i("r", r);
    return r;
}

void vk::trace::FreeCommandBuffersTraced(VkDevice d, VkCommandPool p, uint32_t n, const VkCommandBuffer* c)
{
    {
        Line l("FreeCommandBuffers");
        l.h("pool", kCommandPool, p).hs("cmds", kCommand, c, n);
        for (uint32_t k = 0; k < n; k++) s.names[kCommand].erase(Raw(c[k]));
    }
    vkFreeCommandBuffers(d, p, n, c);
}

VkResult vk::trace::BeginCommandBufferTraced(VkCommandBuffer c, const VkCommandBufferBeginInfo* i)
{
    { Line l("BeginCommandBuffer"); l.h("cmd", kCommand, c).x("flags", i->flags).next(i->pNext); }
    return vkBeginCommandBuffer(c, i);
}

VkResult vk::trace::EndCommandBufferTraced(VkCommandBuffer c)
{
    { Line l("EndCommandBuffer"); l.h("cmd", kCommand, c); }
    return vkEndCommandBuffer(c);
}

VkResult vk::trace::CreateFenceTraced(VkDevice d, const VkFenceCreateInfo* i, const VkAllocationCallbacks* a, VkFence* o)
{
    const VkResult r = vkCreateFence(d, i, a, o);
    Line l("CreateFence");
    l.made("out", kFence, r == VK_SUCCESS ? *o : VK_NULL_HANDLE).x("flags", i->flags).next(i->pNext).i("r", r);
    return r;
}

void vk::trace::DestroyFenceTraced(VkDevice d, VkFence h, const VkAllocationCallbacks* a)
{
    { Line l("DestroyFence"); l.gone("fence", kFence, h); }
    vkDestroyFence(d, h, a);
}

VkResult vk::trace::ResetFencesTraced(VkDevice d, uint32_t n, const VkFence* f)
{
    { Line l("ResetFences"); l.hs("fences", kFence, f, n); }
    return vkResetFences(d, n, f);
}

VkResult vk::trace::CreateQueryPoolTraced(VkDevice d, const VkQueryPoolCreateInfo* i, const VkAllocationCallbacks* a,
                                          VkQueryPool* o)
{
    const VkResult r = vkCreateQueryPool(d, i, a, o);
    Line l("CreateQueryPool");
    l.made("out", kQueryPool, r == VK_SUCCESS ? *o : VK_NULL_HANDLE).x("flags", i->flags).u("type", i->queryType)
        .u("count", i->queryCount).x("statistics", i->pipelineStatistics).next(i->pNext).i("r", r);
    return r;
}

void vk::trace::DestroyQueryPoolTraced(VkDevice d, VkQueryPool h, const VkAllocationCallbacks* a)
{
    { Line l("DestroyQueryPool"); l.gone("pool", kQueryPool, h); }
    vkDestroyQueryPool(d, h, a);
}

VkResult vk::trace::QueueSubmitTraced(VkQueue q, uint32_t n, const VkSubmitInfo* submits, VkFence f)
{
    SubmitLine("QueueSubmit", q, n, submits, f);
    return vkQueueSubmit(q, n, submits, f);
}

void vk::trace::CmdBindDescriptorSetsTraced(VkCommandBuffer c, VkPipelineBindPoint b, VkPipelineLayout layout,
                                            uint32_t first, uint32_t n, const VkDescriptorSet* sets, uint32_t nd,
                                            const uint32_t* dyn)
{
    {
        Line l("CmdBindDescriptorSets");
        l.h("cmd", kCommand, c).u("bind", b).h("layout", kPipelineLayout, layout).u("first", first)
            .hs("sets", kSet, sets, n).us("dynamic", dyn, nd);
    }
    vkCmdBindDescriptorSets(c, b, layout, first, n, sets, nd, dyn);
}

void vk::trace::CmdPipelineBarrierTraced(VkCommandBuffer c, VkPipelineStageFlags src, VkPipelineStageFlags dst,
                                         VkDependencyFlags dep, uint32_t nm, const VkMemoryBarrier* m, uint32_t nb,
                                         const VkBufferMemoryBarrier* b, uint32_t ni, const VkImageMemoryBarrier* i)
{
    {
        Line l("CmdPipelineBarrier");
        l.h("cmd", kCommand, c).x("src", src).x("dst", dst).x("dependency", dep);
        Barriers(l, nm, m, nb, b, ni, i);
    }
    vkCmdPipelineBarrier(c, src, dst, dep, nm, m, nb, b, ni, i);
}

void vk::trace::UtilBarrier(VkCommandBuffer c, VkPipelineStageFlags src, VkPipelineStageFlags dst,
                            VkDependencyFlags dep, uint32_t nm, const VkMemoryBarrier* m, uint32_t nb,
                            const VkBufferMemoryBarrier* b, uint32_t ni, const VkImageMemoryBarrier* i)
{
    // The presenter's command buffers (in the C++ renderer) are not the renderer's.
    if (detail::on && KnownCommand(c)) CmdPipelineBarrierTraced(c, src, dst, dep, nm, m, nb, b, ni, i);
    else vkCmdPipelineBarrier(c, src, dst, dep, nm, m, nb, b, ni, i);
}

void vk::trace::CmdBeginQueryTraced(VkCommandBuffer c, VkQueryPool p, uint32_t q, VkQueryControlFlags f)
{
    { Line l("CmdBeginQuery"); l.h("cmd", kCommand, c).h("pool", kQueryPool, p).u("query", q).x("flags", f); }
    vkCmdBeginQuery(c, p, q, f);
}

void vk::trace::CmdEndQueryTraced(VkCommandBuffer c, VkQueryPool p, uint32_t q)
{
    { Line l("CmdEndQuery"); l.h("cmd", kCommand, c).h("pool", kQueryPool, p).u("query", q); }
    vkCmdEndQuery(c, p, q);
}

void vk::trace::CmdResetQueryPoolTraced(VkCommandBuffer c, VkQueryPool p, uint32_t first, uint32_t n)
{
    { Line l("CmdResetQueryPool"); l.h("cmd", kCommand, c).h("pool", kQueryPool, p).u("first", first).u("count", n); }
    vkCmdResetQueryPool(c, p, first, n);
}

void vk::trace::CmdWriteTimestampTraced(VkCommandBuffer c, VkPipelineStageFlagBits st, VkQueryPool p, uint32_t q)
{
    { Line l("CmdWriteTimestamp"); l.h("cmd", kCommand, c).x("stage", st).h("pool", kQueryPool, p).u("query", q); }
    vkCmdWriteTimestamp(c, st, p, q);
}

void vk::trace::CmdBindIndexBufferTraced(VkCommandBuffer c, VkBuffer b, VkDeviceSize o, VkIndexType t)
{
    { Line l("CmdBindIndexBuffer"); l.h("cmd", kCommand, c).h("buffer", kBuffer, b).u("offset", o).u("type", t); }
    vkCmdBindIndexBuffer(c, b, o, t);
}

void vk::trace::CmdBindPipelineTraced(VkCommandBuffer c, VkPipelineBindPoint b, VkPipeline p)
{
    { Line l("CmdBindPipeline"); l.h("cmd", kCommand, c).u("bind", b).h("pipeline", kPipeline, p); }
    vkCmdBindPipeline(c, b, p);
}

void vk::trace::CmdDrawTraced(VkCommandBuffer c, uint32_t v, uint32_t i, uint32_t fv, uint32_t fi)
{
    { Line l("CmdDraw"); l.h("cmd", kCommand, c).u("vertices", v).u("instances", i).u("firstVertex", fv).u("firstInstance", fi); }
    vkCmdDraw(c, v, i, fv, fi);
}

void vk::trace::CmdDrawIndexedTraced(VkCommandBuffer c, uint32_t n, uint32_t i, uint32_t fi, int32_t vo, uint32_t fin)
{
    {
        Line l("CmdDrawIndexed");
        l.h("cmd", kCommand, c).u("indices", n).u("instances", i).u("firstIndex", fi).i("vertexOffset", vo)
            .u("firstInstance", fin);
    }
    vkCmdDrawIndexed(c, n, i, fi, vo, fin);
}

void vk::trace::CmdSetDepthBiasTraced(VkCommandBuffer c, float k, float clamp, float slope)
{
    { Line l("CmdSetDepthBias"); l.h("cmd", kCommand, c).f("constant", k).f("clamp", clamp).f("slope", slope); }
    vkCmdSetDepthBias(c, k, clamp, slope);
}

void vk::trace::CmdSetScissorTraced(VkCommandBuffer c, uint32_t first, uint32_t n, const VkRect2D* r)
{
    {
        Line l("CmdSetScissor");
        l.h("cmd", kCommand, c).u("first", first).raw(" rects=[");
        for (uint32_t k = 0; k < n; k++) { if (k) l.raw(","); l.rect(r[k]); }
        l.raw("]");
    }
    vkCmdSetScissor(c, first, n, r);
}

void vk::trace::CmdSetViewportTraced(VkCommandBuffer c, uint32_t first, uint32_t n, const VkViewport* v)
{
    {
        Line l("CmdSetViewport");
        l.h("cmd", kCommand, c).u("first", first).raw(" viewports=[");
        for (uint32_t k = 0; k < n; k++)
            Append(l.text, "%s(%08x,%08x,%08x,%08x,%08x,%08x)", k ? "," : "", Bits(v[k].x), Bits(v[k].y),
                   Bits(v[k].width), Bits(v[k].height), Bits(v[k].minDepth), Bits(v[k].maxDepth));
        l.raw("]");
    }
    vkCmdSetViewport(c, first, n, v);
}

void vk::trace::CmdSetStencilCompareMaskTraced(VkCommandBuffer c, VkStencilFaceFlags f, uint32_t v)
{
    { Line l("CmdSetStencilCompareMask"); l.h("cmd", kCommand, c).x("faces", f).x("mask", v); }
    vkCmdSetStencilCompareMask(c, f, v);
}

void vk::trace::CmdSetStencilWriteMaskTraced(VkCommandBuffer c, VkStencilFaceFlags f, uint32_t v)
{
    { Line l("CmdSetStencilWriteMask"); l.h("cmd", kCommand, c).x("faces", f).x("mask", v); }
    vkCmdSetStencilWriteMask(c, f, v);
}

void vk::trace::CmdSetStencilReferenceTraced(VkCommandBuffer c, VkStencilFaceFlags f, uint32_t v)
{
    { Line l("CmdSetStencilReference"); l.h("cmd", kCommand, c).x("faces", f).x("reference", v); }
    vkCmdSetStencilReference(c, f, v);
}

void vk::trace::CmdBlitImageTraced(VkCommandBuffer c, VkImage src, VkImageLayout sl, VkImage dst, VkImageLayout dl,
                                   uint32_t n, const VkImageBlit* r, VkFilter f)
{
    {
        Line l("CmdBlitImage");
        l.h("cmd", kCommand, c).h("src", kImage, src).u("srcLayout", sl).h("dst", kImage, dst).u("dstLayout", dl)
            .raw(" regions=[");
        for (uint32_t k = 0; k < n; k++)
        {
            if (k) l.raw(",");
            l.raw("(").layers(r[k].srcSubresource).raw(",").offset(r[k].srcOffsets[0]).raw(",").offset(r[k].srcOffsets[1])
                .raw(",").layers(r[k].dstSubresource).raw(",").offset(r[k].dstOffsets[0]).raw(",")
                .offset(r[k].dstOffsets[1]).raw(")");
        }
        l.raw("]").u("filter", f);
    }
    vkCmdBlitImage(c, src, sl, dst, dl, n, r, f);
}

void vk::trace::CmdCopyImageTraced(VkCommandBuffer c, VkImage src, VkImageLayout sl, VkImage dst, VkImageLayout dl,
                                   uint32_t n, const VkImageCopy* r)
{
    {
        Line l("CmdCopyImage");
        l.h("cmd", kCommand, c).h("src", kImage, src).u("srcLayout", sl).h("dst", kImage, dst).u("dstLayout", dl)
            .raw(" regions=[");
        for (uint32_t k = 0; k < n; k++)
        {
            if (k) l.raw(",");
            l.raw("(").layers(r[k].srcSubresource).raw(",").offset(r[k].srcOffset).raw(",").layers(r[k].dstSubresource)
                .raw(",").offset(r[k].dstOffset).raw(",").extent(r[k].extent).raw(")");
        }
        l.raw("]");
    }
    vkCmdCopyImage(c, src, sl, dst, dl, n, r);
}

void vk::trace::CmdResolveImageTraced(VkCommandBuffer c, VkImage src, VkImageLayout sl, VkImage dst, VkImageLayout dl,
                                      uint32_t n, const VkImageResolve* r)
{
    {
        Line l("CmdResolveImage");
        l.h("cmd", kCommand, c).h("src", kImage, src).u("srcLayout", sl).h("dst", kImage, dst).u("dstLayout", dl)
            .raw(" regions=[");
        for (uint32_t k = 0; k < n; k++)
        {
            if (k) l.raw(",");
            l.raw("(").layers(r[k].srcSubresource).raw(",").offset(r[k].srcOffset).raw(",").layers(r[k].dstSubresource)
                .raw(",").offset(r[k].dstOffset).raw(",").extent(r[k].extent).raw(")");
        }
        l.raw("]");
    }
    vkCmdResolveImage(c, src, sl, dst, dl, n, r);
}

void vk::trace::CmdClearColorImageTraced(VkCommandBuffer c, VkImage i, VkImageLayout lay, const VkClearColorValue* v,
                                         uint32_t n, const VkImageSubresourceRange* r)
{
    {
        Line l("CmdClearColorImage");
        l.h("cmd", kCommand, c).h("image", kImage, i).u("layout", lay);
        Append(l.text, " colour=%08x,%08x,%08x,%08x", v->uint32[0], v->uint32[1], v->uint32[2], v->uint32[3]);
        l.ranges("ranges", r, n);
    }
    vkCmdClearColorImage(c, i, lay, v, n, r);
}

void vk::trace::CmdClearDepthStencilImageTraced(VkCommandBuffer c, VkImage i, VkImageLayout lay,
                                                const VkClearDepthStencilValue* v, uint32_t n,
                                                const VkImageSubresourceRange* r)
{
    {
        Line l("CmdClearDepthStencilImage");
        l.h("cmd", kCommand, c).h("image", kImage, i).u("layout", lay).f("depth", v->depth).u("stencil", v->stencil)
            .ranges("ranges", r, n);
    }
    vkCmdClearDepthStencilImage(c, i, lay, v, n, r);
}

void vk::trace::CmdCopyBufferToImageTraced(VkCommandBuffer c, VkBuffer b, VkImage i, VkImageLayout lay, uint32_t n,
                                           const VkBufferImageCopy* r)
{
    {
        Line l("CmdCopyBufferToImage");
        l.h("cmd", kCommand, c).h("buffer", kBuffer, b).h("image", kImage, i).u("layout", lay).copies("regions", r, n);
    }
    vkCmdCopyBufferToImage(c, b, i, lay, n, r);
}

void vk::trace::CmdCopyImageToBufferTraced(VkCommandBuffer c, VkImage i, VkImageLayout lay, VkBuffer b, uint32_t n,
                                           const VkBufferImageCopy* r)
{
    {
        Line l("CmdCopyImageToBuffer");
        l.h("cmd", kCommand, c).h("image", kImage, i).u("layout", lay).h("buffer", kBuffer, b).copies("regions", r, n);
    }
    vkCmdCopyImageToBuffer(c, i, lay, b, n, r);
}

void vk::trace::CmdDispatchTraced(VkCommandBuffer c, uint32_t x, uint32_t y, uint32_t z)
{
    { Line l("CmdDispatch"); l.h("cmd", kCommand, c).u("x", x).u("y", y).u("z", z); }
    vkCmdDispatch(c, x, y, z);
}

void vk::trace::CmdUpdateBufferTraced(VkCommandBuffer c, VkBuffer b, VkDeviceSize o, VkDeviceSize n, const void* data)
{
    {
        Line l("CmdUpdateBuffer");
        l.h("cmd", kCommand, c).h("buffer", kBuffer, b).u("offset", o).u("size", n).raw(" data=");
        const auto* words = static_cast<const uint32_t*>(data);
        for (VkDeviceSize k = 0; k < n / 4; k++) Append(l.text, "%08x", words[k]);
    }
    vkCmdUpdateBuffer(c, b, o, n, data);
}

// ---- the extension commands ------------------------------------------------------

namespace
{
    void BeginRendering(VkCommandBuffer c, const VkRenderingInfo* i)
    {
        {
            Line l("CmdBeginRendering");
            l.h("cmd", kCommand, c).x("flags", i->flags).key("area").rect(i->renderArea);
            l.u("layers", i->layerCount).u("viewMask", i->viewMask).raw(" colour=[");
            for (uint32_t n = 0; n < i->colorAttachmentCount; n++)
            {
                if (n) l.raw(",");
                l.attachment(nullptr, &i->pColorAttachments[n]);
            }
            l.raw("]").attachment("depth", i->pDepthAttachment).attachment("stencil", i->pStencilAttachment).next(i->pNext);
        }
        s.beginRendering(c, i);
    }
    void EndRendering(VkCommandBuffer c)
    {
        { Line l("CmdEndRendering"); l.h("cmd", kCommand, c); }
        s.endRendering(c);
    }
    void SetCullMode(VkCommandBuffer c, VkCullModeFlags m)
    {
        { Line l("CmdSetCullMode"); l.h("cmd", kCommand, c).x("mode", m); }
        s.setCullMode(c, m);
    }
    void SetFrontFace(VkCommandBuffer c, VkFrontFace f)
    {
        { Line l("CmdSetFrontFace"); l.h("cmd", kCommand, c).u("face", f); }
        s.setFrontFace(c, f);
    }
    void SetDepthTestEnable(VkCommandBuffer c, VkBool32 v)
    {
        { Line l("CmdSetDepthTestEnable"); l.h("cmd", kCommand, c).u("on", v); }
        s.setDepthTestEnable(c, v);
    }
    void SetDepthWriteEnable(VkCommandBuffer c, VkBool32 v)
    {
        { Line l("CmdSetDepthWriteEnable"); l.h("cmd", kCommand, c).u("on", v); }
        s.setDepthWriteEnable(c, v);
    }
    void SetDepthCompareOp(VkCommandBuffer c, VkCompareOp v)
    {
        { Line l("CmdSetDepthCompareOp"); l.h("cmd", kCommand, c).u("op", v); }
        s.setDepthCompareOp(c, v);
    }
    void SetStencilTestEnable(VkCommandBuffer c, VkBool32 v)
    {
        { Line l("CmdSetStencilTestEnable"); l.h("cmd", kCommand, c).u("on", v); }
        s.setStencilTestEnable(c, v);
    }
    void SetStencilOp(VkCommandBuffer c, VkStencilFaceFlags f, VkStencilOp fail, VkStencilOp pass, VkStencilOp depthFail,
                      VkCompareOp compare)
    {
        {
            Line l("CmdSetStencilOp");
            l.h("cmd", kCommand, c).x("faces", f).u("fail", fail).u("pass", pass).u("depthFail", depthFail)
                .u("compare", compare);
        }
        s.setStencilOp(c, f, fail, pass, depthFail, compare);
    }
    void DrawMulti(VkCommandBuffer c, uint32_t n, const VkMultiDrawInfoEXT* draws, uint32_t instances,
                   uint32_t firstInstance, uint32_t stride)
    {
        {
            Line l("CmdDrawMulti");
            l.h("cmd", kCommand, c).raw(" draws=[");
            for (uint32_t k = 0; k < n; k++)
            {
                auto* d = reinterpret_cast<const VkMultiDrawInfoEXT*>(reinterpret_cast<const uint8_t*>(draws) + k * stride);
                Append(l.text, k ? ",(%u,%u)" : "(%u,%u)", d->firstVertex, d->vertexCount);
            }
            l.raw("]").u("instances", instances).u("firstInstance", firstInstance);
        }
        s.drawMulti(c, n, draws, instances, firstInstance, stride);
    }
    void DrawMultiIndexed(VkCommandBuffer c, uint32_t n, const VkMultiDrawIndexedInfoEXT* draws, uint32_t instances,
                          uint32_t firstInstance, uint32_t stride, const int32_t* vertexOffset)
    {
        {
            Line l("CmdDrawMultiIndexed");
            l.h("cmd", kCommand, c).raw(" draws=[");
            for (uint32_t k = 0; k < n; k++)
            {
                auto* d = reinterpret_cast<const VkMultiDrawIndexedInfoEXT*>(reinterpret_cast<const uint8_t*>(draws) +
                                                                            k * stride);
                Append(l.text, k ? ",(%u,%u,%d)" : "(%u,%u,%d)", d->firstIndex, d->indexCount, d->vertexOffset);
            }
            l.raw("]").u("instances", instances).u("firstInstance", firstInstance);
            if (vertexOffset) l.i("vertexOffset", *vertexOffset);
            else l.raw(" vertexOffset=-");
        }
        s.drawMultiIndexed(c, n, draws, instances, firstInstance, stride, vertexOffset);
    }
}

void vk::trace::WrapDispatch(PFN_vkCmdBeginRenderingKHR& beginRendering, PFN_vkCmdEndRenderingKHR& endRendering,
                             PFN_vkCmdSetCullModeEXT& setCullMode, PFN_vkCmdSetFrontFaceEXT& setFrontFace,
                             PFN_vkCmdSetDepthTestEnableEXT& setDepthTestEnable,
                             PFN_vkCmdSetDepthWriteEnableEXT& setDepthWriteEnable,
                             PFN_vkCmdSetDepthCompareOpEXT& setDepthCompareOp,
                             PFN_vkCmdSetStencilTestEnableEXT& setStencilTestEnable, PFN_vkCmdSetStencilOpEXT& setStencilOp,
                             PFN_vkCmdDrawMultiEXT& drawMulti, PFN_vkCmdDrawMultiIndexedEXT& drawMultiIndexed)
{
    if (!detail::on) return;
    // A null pointer stays null: the renderer tests them.
    const auto wrap = [](auto& pointer, auto& kept, auto wrapper) {
        kept = pointer;
        if (pointer) pointer = wrapper;
    };
    wrap(beginRendering, s.beginRendering, &BeginRendering);
    wrap(endRendering, s.endRendering, &EndRendering);
    wrap(setCullMode, s.setCullMode, &SetCullMode);
    wrap(setFrontFace, s.setFrontFace, &SetFrontFace);
    wrap(setDepthTestEnable, s.setDepthTestEnable, &SetDepthTestEnable);
    wrap(setDepthWriteEnable, s.setDepthWriteEnable, &SetDepthWriteEnable);
    wrap(setDepthCompareOp, s.setDepthCompareOp, &SetDepthCompareOp);
    wrap(setStencilTestEnable, s.setStencilTestEnable, &SetStencilTestEnable);
    wrap(setStencilOp, s.setStencilOp, &SetStencilOp);
    wrap(drawMulti, s.drawMulti, &DrawMulti);
    wrap(drawMultiIndexed, s.drawMultiIndexed, &DrawMultiIndexed);
}

#endif  // MW2_TRACE
