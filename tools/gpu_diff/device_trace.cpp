// tools/gpu_diff.py --rig device_trace (recorder.cpp's "trace" part): generated cases of
// the calls trace.h wraps, made through the C++ wrappers (this file includes trace.h last,
// as every renderer file does, so its vk* calls are the wrappers) and through the Rust
// Vk's methods and trace functions (device.rs, mod rig), on the same structures and the
// stand-ins of device_stubs.cpp. Each case's trace file, printed lines, stand-in log and
// results are compared.
#include "gpu/vulkan/util.h"

#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <memory>
#include <random>
#include <sstream>
#include <string>
#include <thread>
#include <vector>

#include <unistd.h>

#include "rig_log.h"
#include "gpu/vulkan/trace.h"   // last: the command trace's wrappers

extern "C"
{
    struct RigCall
    {
        uint32_t op, onRecorder;
        uint64_t a[12];
    };
    void mw2_rig_vk_begin(uint64_t device, uint64_t physical, uint64_t queue, uint64_t cache, uint64_t layout,
                          const uint64_t* setLayouts, uint32_t count, uint32_t multiDraw);
    void mw2_rig_vk_forced(uint64_t* out);
    uint64_t mw2_rig_vk_fnv(const uint8_t* bytes, size_t size, bool writeCombined);
    uint64_t mw2_rig_vk_call(const RigCall* call, uint64_t* out);
    uint64_t rig_position();
}
void rig_cpp_trace_reset();
namespace rig_vk
{
    extern std::vector<uint64_t> g_calls;
    extern bool g_logging;
    extern uint64_t g_next;
    extern VkResult g_result;
    extern uint64_t g_position;
    PFN_vkVoidFunction Command(const char* name);
    void Withhold(bool multi, int dynamic);
}

namespace
{
    using namespace vk::trace;

    constexpr uint32_t kOps = 103;
    constexpr uint32_t kSetPosition = 200;   // the rig's own: the recorder's queue position

    template <class H> H As(uint64_t v) { return reinterpret_cast<H>(uintptr_t(v)); }
    template <> VkImage As<VkImage>(uint64_t v) { return reinterpret_cast<VkImage>(uintptr_t(v)); }
    template <class T> const T* P(uint64_t v) { return reinterpret_cast<const T*>(uintptr_t(v)); }
    template <class T> uint64_t U(T* p) { return uint64_t(reinterpret_cast<uintptr_t>(p)); }

    struct Gen
    {
        std::mt19937_64 rng;
        std::vector<std::unique_ptr<uint8_t[]>> keep;
        uint64_t next = 0x1000;
        std::vector<uint64_t> live[kKinds], foreign[kKinds];
        bool recorder = false;
        uint64_t frame = 0;
        uint64_t position = 0;

        explicit Gen(uint64_t seed) : rng(seed) {}
        uint32_t u(uint32_t n) { return n ? uint32_t(rng() % n) : 0; }
        bool p(uint32_t percent) { return u(100) < percent; }
        template <class T> T* make(size_t n = 1)
        {
            auto block = std::make_unique<uint8_t[]>(sizeof(T) * (n ? n : 1) + 64);
            std::memset(block.get(), 0, sizeof(T) * (n ? n : 1) + 64);
            T* at = reinterpret_cast<T*>(block.get());
            keep.push_back(std::move(block));
            return at;
        }
        uint64_t pick(Kind kind)
        {
            const uint32_t r = u(100);
            if (r < 65 && !live[kind].empty()) return live[kind][u(uint32_t(live[kind].size()))];
            if (r < 78 && !foreign[kind].empty()) return foreign[kind][u(uint32_t(foreign[kind].size()))];
            if (r < 90) return 0x7000000 + u(4096) * 8;
            return 0;
        }
        template <class H> H h(Kind kind) { return As<H>(pick(kind)); }
        template <class H> H* hs(Kind kind, uint32_t n)
        {
            H* at = make<H>(n);
            for (uint32_t k = 0; k < n; k++) at[k] = h<H>(kind);
            return at;
        }
        void made(Kind kind, uint32_t n, VkResult r)
        {
            if (r != VK_SUCCESS) return;
            for (uint32_t k = 0; k < n; k++)
            {
                live[kind].push_back(next);
                next += 0x10;
            }
        }
        void gone(Kind kind, uint64_t handle)
        {
            auto& l = live[kind];
            for (size_t k = 0; k < l.size(); k++)
                if (l[k] == handle) { l.erase(l.begin() + long(k)); return; }
        }
        VkResult result() { return p(88) ? VK_SUCCESS : VkResult(-1 - int(u(3))); }
        const char* text(size_t most)
        {
            const size_t n = u(uint32_t(most + 1));
            char* at = make<char>(n + 1);
            for (size_t k = 0; k < n; k++) at[k] = char(' ' + 1 + u(94));
            return at;
        }
        float f()
        {
            if (p(70)) return float(int(u(2001)) - 1000) / float(1 + u(16));
            uint32_t bits = uint32_t(rng());
            float value;
            std::memcpy(&value, &bits, 4);
            return value;
        }
        uint32_t flags() { return p(50) ? uint32_t(rng()) & 0xFFF : uint32_t(rng()); }

        // A pNext chain of the structures the trace decodes, and one it does not.
        const void* chain()
        {
            const void* head = nullptr;
            const uint32_t n = p(55) ? 0 : 1 + u(3);
            for (uint32_t k = 0; k < n; k++)
            {
                VkBaseOutStructure* made = nullptr;
                switch (u(10))
                {
                case 0:
                {
                    auto* r = make<VkPipelineRenderingCreateInfo>();
                    r->sType = VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO;
                    r->viewMask = u(4);
                    r->colorAttachmentCount = u(4);
                    auto* formats = make<VkFormat>(r->colorAttachmentCount);
                    for (uint32_t c = 0; c < r->colorAttachmentCount; c++) formats[c] = VkFormat(u(130));
                    r->pColorAttachmentFormats = formats;
                    r->depthAttachmentFormat = VkFormat(u(130));
                    r->stencilAttachmentFormat = VkFormat(u(130));
                    made = reinterpret_cast<VkBaseOutStructure*>(r);
                    break;
                }
                case 1:
                {
                    auto* l = make<VkPipelineLibraryCreateInfoKHR>();
                    l->sType = VK_STRUCTURE_TYPE_PIPELINE_LIBRARY_CREATE_INFO_KHR;
                    l->libraryCount = u(4);
                    l->pLibraries = hs<VkPipeline>(kPipeline, l->libraryCount);
                    made = reinterpret_cast<VkBaseOutStructure*>(l);
                    break;
                }
                case 2:
                {
                    auto* l = make<VkGraphicsPipelineLibraryCreateInfoEXT>();
                    l->sType = VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_LIBRARY_CREATE_INFO_EXT;
                    l->flags = u(16);
                    made = reinterpret_cast<VkBaseOutStructure*>(l);
                    break;
                }
                case 3:
                {
                    auto* l = make<VkImageFormatListCreateInfo>();
                    l->sType = VK_STRUCTURE_TYPE_IMAGE_FORMAT_LIST_CREATE_INFO;
                    l->viewFormatCount = u(4);
                    auto* formats = make<VkFormat>(l->viewFormatCount);
                    for (uint32_t c = 0; c < l->viewFormatCount; c++) formats[c] = VkFormat(u(130));
                    l->pViewFormats = formats;
                    made = reinterpret_cast<VkBaseOutStructure*>(l);
                    break;
                }
                case 4:
                {
                    auto* l = make<VkDescriptorSetLayoutBindingFlagsCreateInfo>();
                    l->sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_BINDING_FLAGS_CREATE_INFO;
                    l->bindingCount = u(4);
                    auto* f = make<VkDescriptorBindingFlags>(l->bindingCount);
                    for (uint32_t c = 0; c < l->bindingCount; c++) f[c] = flags();
                    l->pBindingFlags = f;
                    made = reinterpret_cast<VkBaseOutStructure*>(l);
                    break;
                }
                case 5:
                {
                    auto* l = make<VkDescriptorSetVariableDescriptorCountAllocateInfo>();
                    l->sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_VARIABLE_DESCRIPTOR_COUNT_ALLOCATE_INFO;
                    l->descriptorSetCount = u(4);
                    auto* c = make<uint32_t>(l->descriptorSetCount);
                    for (uint32_t k2 = 0; k2 < l->descriptorSetCount; k2++) c[k2] = uint32_t(rng());
                    l->pDescriptorCounts = c;
                    made = reinterpret_cast<VkBaseOutStructure*>(l);
                    break;
                }
                case 6:
                {
                    auto* d = make<VkMemoryDedicatedAllocateInfo>();
                    d->sType = VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO;
                    d->image = h<VkImage>(kImage);
                    d->buffer = h<VkBuffer>(kBuffer);
                    made = reinterpret_cast<VkBaseOutStructure*>(d);
                    break;
                }
                case 7:
                {
                    auto* d = make<VkMemoryAllocateFlagsInfo>();
                    d->sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_FLAGS_INFO;
                    d->flags = flags();
                    made = reinterpret_cast<VkBaseOutStructure*>(d);
                    break;
                }
                case 8:
                {
                    auto* d = make<VkImageViewUsageCreateInfo>();
                    d->sType = VK_STRUCTURE_TYPE_IMAGE_VIEW_USAGE_CREATE_INFO;
                    d->usage = flags();
                    made = reinterpret_cast<VkBaseOutStructure*>(d);
                    break;
                }
                default:
                {
                    auto* d = make<VkPhysicalDeviceFeatures2>();
                    d->sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2;
                    made = reinterpret_cast<VkBaseOutStructure*>(d);
                    break;
                }
                }
                made->pNext = static_cast<VkBaseOutStructure*>(const_cast<void*>(head));
                head = made;
            }
            return head;
        }

        VkImageSubresourceRange range()
        {
            return { flags() & 7, u(4), p(20) ? VK_REMAINING_MIP_LEVELS : u(8), u(4),
                     p(20) ? VK_REMAINING_ARRAY_LAYERS : u(8) };
        }
        VkImageSubresourceLayers layers() { return { flags() & 7, u(4), u(4), u(8) }; }
        VkOffset3D offset() { return { int32_t(u(2000)) - 100, int32_t(u(2000)) - 100, int32_t(u(4)) }; }
        VkExtent3D extent() { return { u(4096), u(4096), u(4) }; }
        VkStencilOpState stencil()
        {
            return { VkStencilOp(u(8)), VkStencilOp(u(8)), VkStencilOp(u(8)), VkCompareOp(u(8)), u(256), u(256), u(256) };
        }

        VkPipelineShaderStageCreateInfo* stages(uint32_t n)
        {
            auto* s = make<VkPipelineShaderStageCreateInfo>(n);
            for (uint32_t k = 0; k < n; k++)
            {
                s[k].sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO;
                s[k].pNext = p(20) ? chain() : nullptr;
                s[k].flags = u(4);
                s[k].stage = VkShaderStageFlagBits(1u << u(6));
                s[k].module = h<VkShaderModule>(kModule);
                s[k].pName = p(85) ? (p(70) ? "main" : text(12)) : nullptr;
                if (p(40))
                {
                    auto* spec = make<VkSpecializationInfo>();
                    spec->mapEntryCount = u(4);
                    auto* entries = make<VkSpecializationMapEntry>(spec->mapEntryCount);
                    for (uint32_t e = 0; e < spec->mapEntryCount; e++) entries[e] = { u(64), u(64), size_t(1 + u(8)) };
                    spec->pMapEntries = entries;
                    spec->dataSize = u(40);
                    auto* data = make<uint8_t>(spec->dataSize);
                    for (size_t d = 0; d < spec->dataSize; d++) data[d] = uint8_t(rng());
                    spec->pData = data;
                    s[k].pSpecializationInfo = spec;
                }
            }
            return s;
        }

        VkGraphicsPipelineCreateInfo graphics()
        {
            VkGraphicsPipelineCreateInfo g{ VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO };
            g.pNext = chain();
            g.flags = flags();
            g.stageCount = u(3);
            g.pStages = stages(g.stageCount);
            if (p(60))
            {
                auto* v = make<VkPipelineVertexInputStateCreateInfo>();
                v->vertexBindingDescriptionCount = u(4);
                v->vertexAttributeDescriptionCount = u(8);
                g.pVertexInputState = v;
            }
            if (p(60))
            {
                auto* a = make<VkPipelineInputAssemblyStateCreateInfo>();
                a->topology = VkPrimitiveTopology(u(11));
                a->primitiveRestartEnable = u(2);
                g.pInputAssemblyState = a;
            }
            if (p(60))
            {
                auto* v = make<VkPipelineViewportStateCreateInfo>();
                v->viewportCount = u(2);
                v->scissorCount = u(2);
                g.pViewportState = v;
            }
            if (p(60))
            {
                auto* r = make<VkPipelineRasterizationStateCreateInfo>();
                r->pNext = p(30) ? chain() : nullptr;
                r->depthClampEnable = u(2);
                r->rasterizerDiscardEnable = u(2);
                r->polygonMode = VkPolygonMode(u(3));
                r->cullMode = u(4);
                r->frontFace = VkFrontFace(u(2));
                r->depthBiasEnable = u(2);
                r->depthBiasConstantFactor = f();
                r->depthBiasClamp = f();
                r->depthBiasSlopeFactor = f();
                r->lineWidth = f();
                g.pRasterizationState = r;
            }
            if (p(60))
            {
                auto* m = make<VkPipelineMultisampleStateCreateInfo>();
                m->rasterizationSamples = VkSampleCountFlagBits(1u << u(4));
                m->sampleShadingEnable = u(2);
                m->minSampleShading = f();
                if (p(50))
                {
                    auto* mask = make<VkSampleMask>();
                    *mask = uint32_t(rng());
                    m->pSampleMask = mask;
                }
                m->alphaToCoverageEnable = u(2);
                g.pMultisampleState = m;
            }
            if (p(60))
            {
                auto* d = make<VkPipelineDepthStencilStateCreateInfo>();
                d->depthTestEnable = u(2);
                d->depthWriteEnable = u(2);
                d->depthCompareOp = VkCompareOp(u(8));
                d->depthBoundsTestEnable = u(2);
                d->stencilTestEnable = u(2);
                d->front = stencil();
                d->back = stencil();
                g.pDepthStencilState = d;
            }
            if (p(60))
            {
                auto* b = make<VkPipelineColorBlendStateCreateInfo>();
                b->logicOpEnable = u(2);
                b->logicOp = VkLogicOp(u(16));
                for (float& c : b->blendConstants) c = f();
                b->attachmentCount = u(4);
                auto* a = make<VkPipelineColorBlendAttachmentState>(b->attachmentCount);
                for (uint32_t k = 0; k < b->attachmentCount; k++)
                    a[k] = { u(2), VkBlendFactor(u(19)), VkBlendFactor(u(19)), VkBlendOp(u(5)), VkBlendFactor(u(19)),
                             VkBlendFactor(u(19)), VkBlendOp(u(5)), u(16) };
                b->pAttachments = a;
                g.pColorBlendState = b;
            }
            if (p(60))
            {
                auto* d = make<VkPipelineDynamicStateCreateInfo>();
                d->dynamicStateCount = u(6);
                auto* s = make<VkDynamicState>(d->dynamicStateCount);
                for (uint32_t k = 0; k < d->dynamicStateCount; k++) s[k] = VkDynamicState(u(9));
                d->pDynamicStates = s;
                g.pDynamicState = d;
            }
            g.layout = h<VkPipelineLayout>(kPipelineLayout);
            g.basePipelineHandle = h<VkPipeline>(kPipeline);
            g.basePipelineIndex = int32_t(u(5)) - 1;
            return g;
        }

        VkSubmitInfo submit()
        {
            VkSubmitInfo s{ VK_STRUCTURE_TYPE_SUBMIT_INFO };
            s.pNext = p(20) ? chain() : nullptr;
            s.commandBufferCount = u(4);
            s.pCommandBuffers = hs<VkCommandBuffer>(kCommand, s.commandBufferCount);
            s.waitSemaphoreCount = u(3);
            s.pWaitSemaphores = p(90) ? hs<VkSemaphore>(kSemaphore, s.waitSemaphoreCount) : nullptr;
            if (p(70))
            {
                auto* stages = make<VkPipelineStageFlags>(s.waitSemaphoreCount);
                for (uint32_t k = 0; k < s.waitSemaphoreCount; k++) stages[k] = flags();
                s.pWaitDstStageMask = stages;
            }
            s.signalSemaphoreCount = u(3);
            s.pSignalSemaphores = hs<VkSemaphore>(kSemaphore, s.signalSemaphoreCount);
            return s;
        }

        RigCall call()
        {
            RigCall c{};
            uint32_t op = u(kOps + 6);
            if (op >= kOps) op = op == kOps ? kSetPosition : (op == kOps + 1 ? 80 : 47 + u(30));   // frames, commands
            if (op == 89) op = 88;   // the finish comes last
            c.op = op;
            const VkResult r = result();
            c.a[11] = uint64_t(int64_t(r));
            auto info = [&](auto* made) {
                c.a[0] = U(made);
                return made;
            };
            switch (op)
            {
            case 0:
            {
                auto* i = info(make<VkImageCreateInfo>());
                *i = { VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, chain(), flags(), VkImageType(u(3)), VkFormat(u(200)), extent(),
                       u(14), u(8), VkSampleCountFlagBits(1u << u(4)), VkImageTiling(u(2)), flags(), VkSharingMode(u(2)),
                       0, nullptr, VkImageLayout(u(9)) };
                made(kImage, 1, r);
                break;
            }
            case 1: c.a[0] = pick(kImage); gone(kImage, c.a[0]); break;
            case 2:
            {
                auto* i = info(make<VkImageViewCreateInfo>());
                *i = { VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, chain(), flags(), h<VkImage>(kImage), VkImageViewType(u(7)),
                       VkFormat(u(200)), { VkComponentSwizzle(u(7)), VkComponentSwizzle(u(7)), VkComponentSwizzle(u(7)),
                                           VkComponentSwizzle(u(7)) }, range() };
                made(kView, 1, r);
                break;
            }
            case 3: c.a[0] = pick(kView); gone(kView, c.a[0]); break;
            case 4:
            {
                auto* i = info(make<VkBufferCreateInfo>());
                *i = { VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, chain(), flags(), rng() >> u(64), flags(), VkSharingMode(u(2)) };
                made(kBuffer, 1, r);
                break;
            }
            case 5: c.a[0] = pick(kBuffer); gone(kBuffer, c.a[0]); break;
            case 6:
            {
                auto* i = info(make<VkMemoryAllocateInfo>());
                *i = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, chain(), rng() >> u(64), u(32) };
                made(kMemory, 1, r);
                break;
            }
            case 7: c.a[0] = pick(kMemory); gone(kMemory, c.a[0]); break;
            case 8: c.a[0] = pick(kImage); c.a[1] = pick(kMemory); c.a[2] = rng() >> u(64); break;
            case 9: c.a[0] = pick(kBuffer); c.a[1] = pick(kMemory); c.a[2] = rng() >> u(64); break;
            case 10:
            {
                auto* i = info(make<VkSamplerCreateInfo>());
                *i = { VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO, chain(), flags(), VkFilter(u(2)), VkFilter(u(2)),
                       VkSamplerMipmapMode(u(2)), VkSamplerAddressMode(u(5)), VkSamplerAddressMode(u(5)),
                       VkSamplerAddressMode(u(5)), f(), u(2), f(), u(2), VkCompareOp(u(8)), f(), f(),
                       VkBorderColor(u(6)), u(2) };
                made(kSampler, 1, r);
                break;
            }
            case 11: c.a[0] = pick(kSampler); gone(kSampler, c.a[0]); break;
            case 12:
            {
                auto* i = info(make<VkDescriptorPoolCreateInfo>());
                const uint32_t n = u(4);
                auto* sizes = make<VkDescriptorPoolSize>(n);
                for (uint32_t k = 0; k < n; k++) sizes[k] = { VkDescriptorType(u(11)), uint32_t(rng()) };
                *i = { VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO, chain(), flags(), uint32_t(rng()), n, sizes };
                made(kDescriptorPool, 1, r);
                break;
            }
            case 13: c.a[0] = pick(kDescriptorPool); gone(kDescriptorPool, c.a[0]); break;
            case 14:
            {
                auto* i = info(make<VkDescriptorSetAllocateInfo>());
                const uint32_t n = 1 + u(4);
                *i = { VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO, chain(), h<VkDescriptorPool>(kDescriptorPool), n,
                       hs<VkDescriptorSetLayout>(kSetLayout, n) };
                made(kSet, n, r);
                break;
            }
            case 15:
            {
                const uint32_t n = u(4);
                auto* sets = hs<VkDescriptorSet>(kSet, n);
                for (uint32_t k = 0; k < n; k++) gone(kSet, U(sets[k]));
                c.a[0] = pick(kDescriptorPool);
                c.a[1] = n;
                c.a[2] = U(sets);
                break;
            }
            case 16:
            {
                const uint32_t nw = u(4), nc = u(3);
                auto* w = make<VkWriteDescriptorSet>(nw);
                for (uint32_t k = 0; k < nw; k++)
                {
                    const uint32_t n = 1 + u(3);
                    w[k] = { VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, p(20) ? chain() : nullptr, h<VkDescriptorSet>(kSet),
                             u(8), u(4), n, VkDescriptorType(u(11)) };
                    if (p(50))
                    {
                        auto* images = make<VkDescriptorImageInfo>(n);
                        for (uint32_t e = 0; e < n; e++)
                            images[e] = { h<VkSampler>(kSampler), h<VkImageView>(kView), VkImageLayout(u(9)) };
                        w[k].pImageInfo = images;
                    }
                    if (p(50))
                    {
                        auto* buffers = make<VkDescriptorBufferInfo>(n);
                        for (uint32_t e = 0; e < n; e++)
                            buffers[e] = { h<VkBuffer>(kBuffer), rng() >> u(64), p(30) ? VK_WHOLE_SIZE : rng() >> u(64) };
                        w[k].pBufferInfo = buffers;
                    }
                }
                auto* copies = make<VkCopyDescriptorSet>(nc);
                for (uint32_t k = 0; k < nc; k++) copies[k].sType = VK_STRUCTURE_TYPE_COPY_DESCRIPTOR_SET;
                c.a[0] = nw;
                c.a[1] = U(w);
                c.a[2] = nc;
                c.a[3] = U(copies);
                break;
            }
            case 17:
            {
                auto* i = info(make<VkDescriptorSetLayoutCreateInfo>());
                const uint32_t n = u(5);
                auto* b = make<VkDescriptorSetLayoutBinding>(n);
                for (uint32_t k = 0; k < n; k++)
                {
                    const uint32_t count = 1 + u(3);
                    b[k] = { u(16), VkDescriptorType(u(11)), count, flags(),
                             p(30) ? hs<VkSampler>(kSampler, count) : nullptr };
                }
                *i = { VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, chain(), flags(), n, b };
                made(kSetLayout, 1, r);
                break;
            }
            case 18: c.a[0] = pick(kSetLayout); gone(kSetLayout, c.a[0]); break;
            case 19:
            {
                auto* i = info(make<VkPipelineLayoutCreateInfo>());
                const uint32_t n = u(4), np = u(3);
                auto* push = make<VkPushConstantRange>(np);
                for (uint32_t k = 0; k < np; k++) push[k] = { flags(), u(256), u(256) };
                *i = { VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, chain(), flags(), n,
                       hs<VkDescriptorSetLayout>(kSetLayout, n), np, push };
                made(kPipelineLayout, 1, r);
                break;
            }
            case 20: c.a[0] = pick(kPipelineLayout); gone(kPipelineLayout, c.a[0]); break;
            case 21:
            {
                const uint32_t n = 1 + u(2);
                auto* infos = make<VkGraphicsPipelineCreateInfo>(n);
                for (uint32_t k = 0; k < n; k++) infos[k] = graphics();
                c.a[0] = pick(kCache);
                c.a[1] = n;
                c.a[2] = U(infos);
                made(kPipeline, n, r);
                break;
            }
            case 22:
            {
                const uint32_t n = 1 + u(2);
                auto* infos = make<VkComputePipelineCreateInfo>(n);
                for (uint32_t k = 0; k < n; k++)
                    infos[k] = { VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO, chain(), flags(), *stages(1),
                                 h<VkPipelineLayout>(kPipelineLayout), h<VkPipeline>(kPipeline), int32_t(u(3)) - 1 };
                c.a[0] = pick(kCache);
                c.a[1] = n;
                c.a[2] = U(infos);
                made(kPipeline, n, r);
                break;
            }
            case 23: c.a[0] = pick(kPipeline); gone(kPipeline, c.a[0]); break;
            case 24:
            {
                auto* i = info(make<VkShaderModuleCreateInfo>());
                const uint32_t words = u(64);
                auto* code = make<uint32_t>(words);
                for (uint32_t k = 0; k < words; k++) code[k] = uint32_t(rng());
                *i = { VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, chain(), 0, size_t(words) * 4, code };
                made(kModule, 1, r);
                break;
            }
            case 25: c.a[0] = pick(kModule); gone(kModule, c.a[0]); break;
            case 26:
            {
                auto* i = info(make<VkCommandPoolCreateInfo>());
                *i = { VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, chain(), flags(), u(4) };
                made(kCommandPool, 1, r);
                break;
            }
            case 27: c.a[0] = pick(kCommandPool); gone(kCommandPool, c.a[0]); break;
            case 28: c.a[0] = pick(kCommandPool); c.a[1] = flags(); break;
            case 29:
            {
                auto* i = info(make<VkCommandBufferAllocateInfo>());
                const uint32_t n = 1 + u(3);
                *i = { VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, nullptr, h<VkCommandPool>(kCommandPool),
                       VkCommandBufferLevel(u(2)), n };
                made(kCommand, n, r);
                break;
            }
            case 30:
            {
                const uint32_t n = u(4);
                auto* cmds = hs<VkCommandBuffer>(kCommand, n);
                for (uint32_t k = 0; k < n; k++) gone(kCommand, U(cmds[k]));
                c.a[0] = pick(kCommandPool);
                c.a[1] = n;
                c.a[2] = U(cmds);
                break;
            }
            case 31:
            {
                auto* i = make<VkCommandBufferBeginInfo>();
                *i = { VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, chain(), flags(), nullptr };
                c.a[0] = pick(kCommand);
                c.a[1] = U(i);
                break;
            }
            case 32: c.a[0] = pick(kCommand); break;
            case 33:
            {
                auto* i = info(make<VkFenceCreateInfo>());
                *i = { VK_STRUCTURE_TYPE_FENCE_CREATE_INFO, chain(), flags() };
                made(kFence, 1, r);
                break;
            }
            case 34: c.a[0] = pick(kFence); gone(kFence, c.a[0]); break;
            case 35: c.a[0] = u(4); c.a[1] = U(hs<VkFence>(kFence, uint32_t(c.a[0]))); break;
            case 36:
            {
                auto* i = info(make<VkQueryPoolCreateInfo>());
                *i = { VK_STRUCTURE_TYPE_QUERY_POOL_CREATE_INFO, chain(), flags(), VkQueryType(u(3)), uint32_t(rng()),
                       flags() };
                made(kQueryPool, 1, r);
                break;
            }
            case 37: c.a[0] = pick(kQueryPool); gone(kQueryPool, c.a[0]); break;
            case 38:
            case 82:
            {
                const uint32_t n = op == 82 ? 1 : u(3);
                auto* s = make<VkSubmitInfo>(n);
                for (uint32_t k = 0; k < n; k++) s[k] = submit();
                c.a[0] = pick(kQueue);
                if (op == 38)
                {
                    c.a[1] = n;
                    c.a[2] = U(s);
                    c.a[3] = pick(kFence);
                }
                else
                {
                    c.a[1] = U(s);
                    c.a[2] = pick(kFence);
                }
                break;
            }
            case 39:
            {
                const uint32_t n = u(4), nd = u(3);
                auto* dyn = make<uint32_t>(nd);
                for (uint32_t k = 0; k < nd; k++) dyn[k] = uint32_t(rng());
                c.a[0] = pick(kCommand);
                c.a[1] = u(2);
                c.a[2] = pick(kPipelineLayout);
                c.a[3] = u(4);
                c.a[4] = n;
                c.a[5] = U(hs<VkDescriptorSet>(kSet, n));
                c.a[6] = nd;
                c.a[7] = U(dyn);
                break;
            }
            case 40:
            case 75:
            {
                const uint32_t nm = u(3), nb = u(3), ni = u(4);
                auto* m = make<VkMemoryBarrier>(nm);
                for (uint32_t k = 0; k < nm; k++) m[k] = { VK_STRUCTURE_TYPE_MEMORY_BARRIER, nullptr, flags(), flags() };
                auto* b = make<VkBufferMemoryBarrier>(nb);
                for (uint32_t k = 0; k < nb; k++)
                    b[k] = { VK_STRUCTURE_TYPE_BUFFER_MEMORY_BARRIER, nullptr, flags(), flags(),
                             p(50) ? VK_QUEUE_FAMILY_IGNORED : u(4), p(50) ? VK_QUEUE_FAMILY_IGNORED : u(4),
                             h<VkBuffer>(kBuffer), rng() >> u(64), p(30) ? VK_WHOLE_SIZE : rng() >> u(64) };
                auto* im = make<VkImageMemoryBarrier>(ni);
                for (uint32_t k = 0; k < ni; k++)
                    im[k] = { VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, nullptr, flags(), flags(), VkImageLayout(u(9)),
                              VkImageLayout(u(9)), p(50) ? VK_QUEUE_FAMILY_IGNORED : u(4),
                              p(50) ? VK_QUEUE_FAMILY_IGNORED : u(4), h<VkImage>(kImage), range() };
                c.a[0] = pick(kCommand);
                c.a[1] = flags();
                c.a[2] = flags();
                c.a[3] = u(8);
                c.a[4] = nm;
                c.a[5] = U(m);
                c.a[6] = nb;
                c.a[7] = U(b);
                c.a[8] = ni;
                c.a[9] = U(im);
                break;
            }
            case 41: c.a[0] = pick(kCommand); c.a[1] = pick(kQueryPool); c.a[2] = u(64); c.a[3] = u(2); break;
            case 42: c.a[0] = pick(kCommand); c.a[1] = pick(kQueryPool); c.a[2] = u(64); break;
            case 43: c.a[0] = pick(kCommand); c.a[1] = pick(kQueryPool); c.a[2] = u(64); c.a[3] = u(64); break;
            case 44: c.a[0] = pick(kCommand); c.a[1] = 1u << u(17); c.a[2] = pick(kQueryPool); c.a[3] = u(64); break;
            case 45: c.a[0] = pick(kCommand); c.a[1] = pick(kBuffer); c.a[2] = rng() >> u(64); c.a[3] = u(2); break;
            case 46: c.a[0] = pick(kCommand); c.a[1] = u(2); c.a[2] = pick(kPipeline); break;
            case 47:
                c.a[0] = pick(kCommand);
                for (int k = 1; k <= 4; k++) c.a[k] = uint32_t(rng());
                break;
            case 48:
                c.a[0] = pick(kCommand);
                for (int k = 1; k <= 5; k++) c.a[k] = uint32_t(rng());
                c.a[4] = uint64_t(int64_t(int32_t(rng())));
                break;
            case 49:
                c.a[0] = pick(kCommand);
                for (int k = 1; k <= 3; k++)
                {
                    const float v = f();
                    uint32_t bits;
                    std::memcpy(&bits, &v, 4);
                    c.a[k] = bits;
                }
                break;
            case 50:
            {
                const uint32_t n = u(3);
                auto* rects = make<VkRect2D>(n);
                for (uint32_t k = 0; k < n; k++) rects[k] = { { int32_t(u(400)) - 50, int32_t(u(400)) - 50 }, { u(4096), u(4096) } };
                c.a[0] = pick(kCommand);
                c.a[1] = u(2);
                c.a[2] = n;
                c.a[3] = U(rects);
                break;
            }
            case 51:
            {
                const uint32_t n = u(3);
                auto* v = make<VkViewport>(n);
                for (uint32_t k = 0; k < n; k++) v[k] = { f(), f(), f(), f(), f(), f() };
                c.a[0] = pick(kCommand);
                c.a[1] = u(2);
                c.a[2] = n;
                c.a[3] = U(v);
                break;
            }
            case 52:
            case 53:
            case 54: c.a[0] = pick(kCommand); c.a[1] = u(4); c.a[2] = uint32_t(rng()); break;
            case 55:
            case 56:
            case 57:
            {
                const uint32_t n = u(3);
                c.a[0] = pick(kCommand);
                c.a[1] = pick(kImage);
                c.a[2] = u(9);
                c.a[3] = pick(kImage);
                c.a[4] = u(9);
                c.a[5] = n;
                if (op == 55)
                {
                    auto* r2 = make<VkImageBlit>(n);
                    for (uint32_t k = 0; k < n; k++) r2[k] = { layers(), { offset(), offset() }, layers(), { offset(), offset() } };
                    c.a[6] = U(r2);
                    c.a[7] = u(2);
                }
                else if (op == 56)
                {
                    auto* r2 = make<VkImageCopy>(n);
                    for (uint32_t k = 0; k < n; k++) r2[k] = { layers(), offset(), layers(), offset(), extent() };
                    c.a[6] = U(r2);
                }
                else
                {
                    auto* r2 = make<VkImageResolve>(n);
                    for (uint32_t k = 0; k < n; k++) r2[k] = { layers(), offset(), layers(), offset(), extent() };
                    c.a[6] = U(r2);
                }
                break;
            }
            case 58:
            case 59:
            {
                const uint32_t n = u(3);
                auto* ranges = make<VkImageSubresourceRange>(n);
                for (uint32_t k = 0; k < n; k++) ranges[k] = range();
                c.a[0] = pick(kCommand);
                c.a[1] = pick(kImage);
                c.a[2] = u(9);
                if (op == 58)
                {
                    auto* v = make<VkClearColorValue>();
                    for (auto& w : v->uint32) w = uint32_t(rng());
                    c.a[3] = U(v);
                }
                else
                {
                    auto* v = make<VkClearDepthStencilValue>();
                    *v = { f(), uint32_t(rng()) };
                    c.a[3] = U(v);
                }
                c.a[4] = n;
                c.a[5] = U(ranges);
                break;
            }
            case 60:
            case 61:
            {
                const uint32_t n = u(3);
                auto* regions = make<VkBufferImageCopy>(n);
                for (uint32_t k = 0; k < n; k++) regions[k] = { rng() >> u(64), u(4096), u(4096), layers(), offset(), extent() };
                c.a[0] = pick(kCommand);
                if (op == 60) { c.a[1] = pick(kBuffer); c.a[2] = pick(kImage); }
                else { c.a[1] = pick(kImage); c.a[2] = u(9); c.a[3] = pick(kBuffer); }
                if (op == 60) c.a[3] = u(9);
                c.a[4] = n;
                c.a[5] = U(regions);
                break;
            }
            case 62: c.a[0] = pick(kCommand); c.a[1] = u(1024); c.a[2] = u(1024); c.a[3] = u(64); break;
            case 63:
            {
                const uint32_t words = u(16);
                auto* data = make<uint32_t>(words);
                for (uint32_t k = 0; k < words; k++) data[k] = uint32_t(rng());
                c.a[0] = pick(kCommand);
                c.a[1] = pick(kBuffer);
                c.a[2] = 4 * u(1024);
                c.a[3] = words * 4;
                c.a[4] = U(data);
                break;
            }
            case 64:
            {
                auto* i = make<VkRenderingInfo>();
                const uint32_t n = u(4);
                auto* colour = make<VkRenderingAttachmentInfo>(n);
                auto attachment = [&] {
                    VkRenderingAttachmentInfo a{ VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO };
                    a.imageView = h<VkImageView>(kView);
                    a.imageLayout = VkImageLayout(u(9));
                    a.resolveMode = VkResolveModeFlagBits(u(9));
                    a.resolveImageView = h<VkImageView>(kView);
                    a.resolveImageLayout = VkImageLayout(u(9));
                    a.loadOp = VkAttachmentLoadOp(u(3));
                    a.storeOp = VkAttachmentStoreOp(u(2));
                    for (auto& w : a.clearValue.color.uint32) w = uint32_t(rng());
                    return a;
                };
                for (uint32_t k = 0; k < n; k++) colour[k] = attachment();
                *i = { VK_STRUCTURE_TYPE_RENDERING_INFO, chain(), flags(),
                       { { int32_t(u(100)), int32_t(u(100)) }, { u(4096), u(4096) } }, u(3), u(4), n, colour };
                if (p(60)) { auto* d = make<VkRenderingAttachmentInfo>(); *d = attachment(); i->pDepthAttachment = d; }
                if (p(40)) { auto* d = make<VkRenderingAttachmentInfo>(); *d = attachment(); i->pStencilAttachment = d; }
                c.a[0] = pick(kCommand);
                c.a[1] = U(i);
                break;
            }
            case 65: c.a[0] = pick(kCommand); break;
            case 66: c.a[0] = pick(kCommand); c.a[1] = u(4); break;
            case 67: c.a[0] = pick(kCommand); c.a[1] = u(2); break;
            case 68:
            case 69:
            case 71: c.a[0] = pick(kCommand); c.a[1] = u(2); break;
            case 70: c.a[0] = pick(kCommand); c.a[1] = u(8); break;
            case 72:
                c.a[0] = pick(kCommand);
                c.a[1] = u(4);
                c.a[2] = u(8);
                c.a[3] = u(8);
                c.a[4] = u(8);
                c.a[5] = u(8);
                break;
            case 73:
            case 74:
            {
                const uint32_t n = 1 + u(5);
                c.a[0] = pick(kCommand);
                c.a[1] = n;
                if (op == 73)
                {
                    auto* d = make<VkMultiDrawInfoEXT>(n);
                    for (uint32_t k = 0; k < n; k++) d[k] = { uint32_t(rng()), uint32_t(rng()) };
                    c.a[2] = U(d);
                }
                else
                {
                    auto* d = make<VkMultiDrawIndexedInfoEXT>(n);
                    for (uint32_t k = 0; k < n; k++) d[k] = { uint32_t(rng()), uint32_t(rng()), int32_t(rng()) };
                    c.a[2] = U(d);
                    if (p(50))
                    {
                        auto* vo = make<int32_t>();
                        *vo = int32_t(rng());
                        c.a[5] = U(vo);
                    }
                }
                c.a[3] = u(3);
                c.a[4] = u(3);
                break;
            }
            case 76:
                c.a[0] = pick(kCommand);
                c.a[1] = pick(kImage);
                c.a[2] = flags() & 7;
                c.a[3] = u(9);
                c.a[4] = u(9);
                c.a[5] = flags();
                c.a[6] = flags();
                c.a[7] = flags();
                c.a[8] = flags();
                break;
            case 77:
            {
                const Kind kind = Kind(u(kKinds));
                uint64_t handle = p(50) && !live[kind].empty() ? live[kind][u(uint32_t(live[kind].size()))]
                                                                : 0x900000 + u(4096) * 8;
                if (p(5)) handle = 0;
                const char* name = p(30) && kind == kImage ? (p(50) ? "Present1" : p(50) ? "PresentX" : "Presen9") : text(90);
                c.a[0] = kind;
                c.a[1] = handle;
                c.a[2] = U(name);
                if (handle) foreign[kind].push_back(handle);
                break;
            }
            case 78:
                c.a[0] = u(kKinds);
                c.a[1] = U(p(10) ? "" : text(20));
                break;
            case 79: c.a[0] = U(text(p(10) ? 700 : 60)); break;
            case 80: frame += u(3); c.a[0] = frame; break;
            case 81: c.a[0] = pick(kImage); c.a[1] = frame; break;
            case 83:
            {
                auto* g = make<VkGraphicsPipelineCreateInfo>();
                *g = graphics();
                c.a[0] = pick(kPipeline);
                c.a[1] = u(16);
                c.a[2] = U(g);
                c.a[3] = U(chain());
                break;
            }
            case 84: break;
            case 85: recorder = true; break;
            case 86: c.a[0] = position ? rng() % (position + 1) : 0; break;
            case 87: recorder = false; break;
            case 88: break;
            case 90: break;
            case 91: c.a[0] = pick(kBuffer); break;
            case 92: c.a[0] = pick(kImage); break;
            case 93: c.a[0] = pick(kFence); break;
            case 94: c.a[0] = u(4); c.a[1] = U(hs<VkFence>(kFence, uint32_t(c.a[0]))); c.a[2] = u(2); c.a[3] = rng(); break;
            case 95: c.a[0] = pick(kQueryPool); c.a[1] = u(8); c.a[2] = u(8); c.a[3] = u(512); c.a[4] = 8 * u(4); c.a[5] = u(16); break;
            case 96: c.a[0] = pick(kMemory); c.a[1] = rng() >> u(64); c.a[2] = rng() >> u(64); break;
            case 97: c.a[0] = pick(kMemory); break;
            case 98: c.a[0] = pick(kPhysical); c.a[1] = u(200); break;
            case 99:
            case 100:
            case 102: c.a[0] = pick(kPhysical); break;
            case 101:
            {
                auto* props = make<VkPhysicalDeviceProperties2>();
                props->sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2;
                c.a[0] = pick(kPhysical);
                c.a[1] = U(props);
                break;
            }
            case kSetPosition: position += u(5); c.a[0] = position; break;
            default: break;
            }
            // A line made on the recorder's own thread, while it runs.
            if (recorder && op < 89 && op != 85 && op != 87 && p(15)) c.onRecorder = 1;
            return c;
        }
    };

    struct Dispatch
    {
        PFN_vkCmdBeginRenderingKHR beginRendering;
        PFN_vkCmdEndRenderingKHR endRendering;
        PFN_vkCmdSetCullModeEXT setCullMode;
        PFN_vkCmdSetFrontFaceEXT setFrontFace;
        PFN_vkCmdSetDepthTestEnableEXT setDepthTestEnable;
        PFN_vkCmdSetDepthWriteEnableEXT setDepthWriteEnable;
        PFN_vkCmdSetDepthCompareOpEXT setDepthCompareOp;
        PFN_vkCmdSetStencilTestEnableEXT setStencilTestEnable;
        PFN_vkCmdSetStencilOpEXT setStencilOp;
        PFN_vkCmdDrawMultiEXT drawMulti;
        PFN_vkCmdDrawMultiIndexedEXT drawMultiIndexed;
        uint32_t multiDraw;
    };

    VkDevice g_device;

    // One call through the C++: the wrappers trace.h makes of the renderer's vk* calls.
    uint64_t CppMade(const RigCall& c, uint64_t* out, const Dispatch& d)
    {
        const uint64_t* a = c.a;
        const VkCommandBuffer cmd = As<VkCommandBuffer>(a[0]);
        const VkDevice dev = g_device;
        auto made = [&](VkResult r, uint64_t handle) {
            out[0] = handle;
            return uint64_t(int64_t(r));
        };
        auto f = [&](int k) {
            float v;
            const uint32_t bits = uint32_t(a[k]);
            std::memcpy(&v, &bits, 4);
            return v;
        };
#define CREATE(fn, Info, H)                                        \
    {                                                              \
        H h = VK_NULL_HANDLE;                                      \
        const VkResult r = fn(dev, P<Info>(a[0]), nullptr, &h);    \
        return made(r, U(h));                                      \
    }
        switch (c.op)
        {
        case 0: CREATE(vkCreateImage, VkImageCreateInfo, VkImage)
        case 1: vkDestroyImage(dev, As<VkImage>(a[0]), nullptr); return 0;
        case 2: CREATE(vkCreateImageView, VkImageViewCreateInfo, VkImageView)
        case 3: vkDestroyImageView(dev, As<VkImageView>(a[0]), nullptr); return 0;
        case 4: CREATE(vkCreateBuffer, VkBufferCreateInfo, VkBuffer)
        case 5: vkDestroyBuffer(dev, As<VkBuffer>(a[0]), nullptr); return 0;
        case 6: CREATE(vkAllocateMemory, VkMemoryAllocateInfo, VkDeviceMemory)
        case 7: vkFreeMemory(dev, As<VkDeviceMemory>(a[0]), nullptr); return 0;
        case 8: return uint64_t(int64_t(vkBindImageMemory(dev, As<VkImage>(a[0]), As<VkDeviceMemory>(a[1]), a[2])));
        case 9: return uint64_t(int64_t(vkBindBufferMemory(dev, As<VkBuffer>(a[0]), As<VkDeviceMemory>(a[1]), a[2])));
        case 10: CREATE(vkCreateSampler, VkSamplerCreateInfo, VkSampler)
        case 11: vkDestroySampler(dev, As<VkSampler>(a[0]), nullptr); return 0;
        case 12: CREATE(vkCreateDescriptorPool, VkDescriptorPoolCreateInfo, VkDescriptorPool)
        case 13: vkDestroyDescriptorPool(dev, As<VkDescriptorPool>(a[0]), nullptr); return 0;
        case 14:
            return uint64_t(int64_t(vkAllocateDescriptorSets(dev, P<VkDescriptorSetAllocateInfo>(a[0]),
                                                             reinterpret_cast<VkDescriptorSet*>(out))));
        case 15:
            return uint64_t(int64_t(vkFreeDescriptorSets(dev, As<VkDescriptorPool>(a[0]), uint32_t(a[1]),
                                                         P<VkDescriptorSet>(a[2]))));
        case 16:
            vkUpdateDescriptorSets(dev, uint32_t(a[0]), P<VkWriteDescriptorSet>(a[1]), uint32_t(a[2]),
                                   P<VkCopyDescriptorSet>(a[3]));
            return 0;
        case 17: CREATE(vkCreateDescriptorSetLayout, VkDescriptorSetLayoutCreateInfo, VkDescriptorSetLayout)
        case 18: vkDestroyDescriptorSetLayout(dev, As<VkDescriptorSetLayout>(a[0]), nullptr); return 0;
        case 19: CREATE(vkCreatePipelineLayout, VkPipelineLayoutCreateInfo, VkPipelineLayout)
        case 20: vkDestroyPipelineLayout(dev, As<VkPipelineLayout>(a[0]), nullptr); return 0;
        case 21:
            return uint64_t(int64_t(vkCreateGraphicsPipelines(dev, As<VkPipelineCache>(a[0]), uint32_t(a[1]),
                                                              P<VkGraphicsPipelineCreateInfo>(a[2]), nullptr,
                                                              reinterpret_cast<VkPipeline*>(out))));
        case 22:
            return uint64_t(int64_t(vkCreateComputePipelines(dev, As<VkPipelineCache>(a[0]), uint32_t(a[1]),
                                                             P<VkComputePipelineCreateInfo>(a[2]), nullptr,
                                                             reinterpret_cast<VkPipeline*>(out))));
        case 23: vkDestroyPipeline(dev, As<VkPipeline>(a[0]), nullptr); return 0;
        case 24: CREATE(vkCreateShaderModule, VkShaderModuleCreateInfo, VkShaderModule)
        case 25: vkDestroyShaderModule(dev, As<VkShaderModule>(a[0]), nullptr); return 0;
        case 26: CREATE(vkCreateCommandPool, VkCommandPoolCreateInfo, VkCommandPool)
        case 27: vkDestroyCommandPool(dev, As<VkCommandPool>(a[0]), nullptr); return 0;
        case 28: return uint64_t(int64_t(vkResetCommandPool(dev, As<VkCommandPool>(a[0]), uint32_t(a[1]))));
        case 29:
            return uint64_t(int64_t(vkAllocateCommandBuffers(dev, P<VkCommandBufferAllocateInfo>(a[0]),
                                                             reinterpret_cast<VkCommandBuffer*>(out))));
        case 30: vkFreeCommandBuffers(dev, As<VkCommandPool>(a[0]), uint32_t(a[1]), P<VkCommandBuffer>(a[2])); return 0;
        case 31: return uint64_t(int64_t(vkBeginCommandBuffer(cmd, P<VkCommandBufferBeginInfo>(a[1]))));
        case 32: return uint64_t(int64_t(vkEndCommandBuffer(cmd)));
        case 33: CREATE(vkCreateFence, VkFenceCreateInfo, VkFence)
        case 34: vkDestroyFence(dev, As<VkFence>(a[0]), nullptr); return 0;
        case 35: return uint64_t(int64_t(vkResetFences(dev, uint32_t(a[0]), P<VkFence>(a[1]))));
        case 36: CREATE(vkCreateQueryPool, VkQueryPoolCreateInfo, VkQueryPool)
        case 37: vkDestroyQueryPool(dev, As<VkQueryPool>(a[0]), nullptr); return 0;
        case 38:
            return uint64_t(int64_t(vkQueueSubmit(As<VkQueue>(a[0]), uint32_t(a[1]), P<VkSubmitInfo>(a[2]),
                                                  As<VkFence>(a[3]))));
        case 39:
            vkCmdBindDescriptorSets(cmd, VkPipelineBindPoint(a[1]), As<VkPipelineLayout>(a[2]), uint32_t(a[3]),
                                    uint32_t(a[4]), P<VkDescriptorSet>(a[5]), uint32_t(a[6]), P<uint32_t>(a[7]));
            return 0;
        case 40:
            vkCmdPipelineBarrier(cmd, uint32_t(a[1]), uint32_t(a[2]), uint32_t(a[3]), uint32_t(a[4]),
                                 P<VkMemoryBarrier>(a[5]), uint32_t(a[6]), P<VkBufferMemoryBarrier>(a[7]),
                                 uint32_t(a[8]), P<VkImageMemoryBarrier>(a[9]));
            return 0;
        case 75:
            vk::trace::UtilBarrier(cmd, uint32_t(a[1]), uint32_t(a[2]), uint32_t(a[3]), uint32_t(a[4]),
                                   P<VkMemoryBarrier>(a[5]), uint32_t(a[6]), P<VkBufferMemoryBarrier>(a[7]),
                                   uint32_t(a[8]), P<VkImageMemoryBarrier>(a[9]));
            return 0;
        case 41: vkCmdBeginQuery(cmd, As<VkQueryPool>(a[1]), uint32_t(a[2]), uint32_t(a[3])); return 0;
        case 42: vkCmdEndQuery(cmd, As<VkQueryPool>(a[1]), uint32_t(a[2])); return 0;
        case 43: vkCmdResetQueryPool(cmd, As<VkQueryPool>(a[1]), uint32_t(a[2]), uint32_t(a[3])); return 0;
        case 44:
            vkCmdWriteTimestamp(cmd, VkPipelineStageFlagBits(a[1]), As<VkQueryPool>(a[2]), uint32_t(a[3]));
            return 0;
        case 45: vkCmdBindIndexBuffer(cmd, As<VkBuffer>(a[1]), a[2], VkIndexType(a[3])); return 0;
        case 46: vkCmdBindPipeline(cmd, VkPipelineBindPoint(a[1]), As<VkPipeline>(a[2])); return 0;
        case 47: vkCmdDraw(cmd, uint32_t(a[1]), uint32_t(a[2]), uint32_t(a[3]), uint32_t(a[4])); return 0;
        case 48:
            vkCmdDrawIndexed(cmd, uint32_t(a[1]), uint32_t(a[2]), uint32_t(a[3]), int32_t(a[4]), uint32_t(a[5]));
            return 0;
        case 49: vkCmdSetDepthBias(cmd, f(1), f(2), f(3)); return 0;
        case 50: vkCmdSetScissor(cmd, uint32_t(a[1]), uint32_t(a[2]), P<VkRect2D>(a[3])); return 0;
        case 51: vkCmdSetViewport(cmd, uint32_t(a[1]), uint32_t(a[2]), P<VkViewport>(a[3])); return 0;
        case 52: vkCmdSetStencilCompareMask(cmd, uint32_t(a[1]), uint32_t(a[2])); return 0;
        case 53: vkCmdSetStencilWriteMask(cmd, uint32_t(a[1]), uint32_t(a[2])); return 0;
        case 54: vkCmdSetStencilReference(cmd, uint32_t(a[1]), uint32_t(a[2])); return 0;
        case 55:
            vkCmdBlitImage(cmd, As<VkImage>(a[1]), VkImageLayout(a[2]), As<VkImage>(a[3]), VkImageLayout(a[4]),
                           uint32_t(a[5]), P<VkImageBlit>(a[6]), VkFilter(a[7]));
            return 0;
        case 56:
            vkCmdCopyImage(cmd, As<VkImage>(a[1]), VkImageLayout(a[2]), As<VkImage>(a[3]), VkImageLayout(a[4]),
                           uint32_t(a[5]), P<VkImageCopy>(a[6]));
            return 0;
        case 57:
            vkCmdResolveImage(cmd, As<VkImage>(a[1]), VkImageLayout(a[2]), As<VkImage>(a[3]), VkImageLayout(a[4]),
                              uint32_t(a[5]), P<VkImageResolve>(a[6]));
            return 0;
        case 58:
            vkCmdClearColorImage(cmd, As<VkImage>(a[1]), VkImageLayout(a[2]), P<VkClearColorValue>(a[3]), uint32_t(a[4]),
                                 P<VkImageSubresourceRange>(a[5]));
            return 0;
        case 59:
            vkCmdClearDepthStencilImage(cmd, As<VkImage>(a[1]), VkImageLayout(a[2]), P<VkClearDepthStencilValue>(a[3]),
                                        uint32_t(a[4]), P<VkImageSubresourceRange>(a[5]));
            return 0;
        case 60:
            vkCmdCopyBufferToImage(cmd, As<VkBuffer>(a[1]), As<VkImage>(a[2]), VkImageLayout(a[3]), uint32_t(a[4]),
                                   P<VkBufferImageCopy>(a[5]));
            return 0;
        case 61:
            vkCmdCopyImageToBuffer(cmd, As<VkImage>(a[1]), VkImageLayout(a[2]), As<VkBuffer>(a[3]), uint32_t(a[4]),
                                   P<VkBufferImageCopy>(a[5]));
            return 0;
        case 62: vkCmdDispatch(cmd, uint32_t(a[1]), uint32_t(a[2]), uint32_t(a[3])); return 0;
        case 63: vkCmdUpdateBuffer(cmd, As<VkBuffer>(a[1]), a[2], a[3], P<void>(a[4])); return 0;
        case 64: d.beginRendering(cmd, P<VkRenderingInfo>(a[1])); return 0;
        case 65: d.endRendering(cmd); return 0;
        case 66: d.setCullMode(cmd, uint32_t(a[1])); return 0;
        case 67: d.setFrontFace(cmd, VkFrontFace(a[1])); return 0;
        case 68: d.setDepthTestEnable(cmd, uint32_t(a[1])); return 0;
        case 69: d.setDepthWriteEnable(cmd, uint32_t(a[1])); return 0;
        case 70: d.setDepthCompareOp(cmd, VkCompareOp(a[1])); return 0;
        case 71: d.setStencilTestEnable(cmd, uint32_t(a[1])); return 0;
        case 72:
            d.setStencilOp(cmd, uint32_t(a[1]), VkStencilOp(a[2]), VkStencilOp(a[3]), VkStencilOp(a[4]),
                           VkCompareOp(a[5]));
            return 0;
        case 73:
            d.drawMulti(cmd, uint32_t(a[1]), P<VkMultiDrawInfoEXT>(a[2]), uint32_t(a[3]), uint32_t(a[4]),
                        sizeof(VkMultiDrawInfoEXT));
            return 0;
        case 74:
            d.drawMultiIndexed(cmd, uint32_t(a[1]), P<VkMultiDrawIndexedInfoEXT>(a[2]), uint32_t(a[3]), uint32_t(a[4]),
                               sizeof(VkMultiDrawIndexedInfoEXT), P<int32_t>(a[5]));
            return 0;
        case 76:
            vk::util::Barrier(cmd, As<VkImage>(a[1]), uint32_t(a[2]), VkImageLayout(a[3]), VkImageLayout(a[4]),
                              uint32_t(a[5]), uint32_t(a[6]), uint32_t(a[7]), uint32_t(a[8]));
            return 0;
        case 77: vk::trace::Register(Kind(a[0]), a[1], "%s", P<char>(a[2])); return 0;
        case 78: vk::trace::NameNext(Kind(a[0]), P<char>(a[1])); return 0;
        case 79: vk::trace::Note("%s", P<char>(a[0])); return 0;
        case 80: vk::trace::Frame(a[0]); return 0;
        case 81: vk::trace::Present(As<VkImage>(a[0]), a[1]); return 0;
        case 82: vk::trace::Submission(As<VkQueue>(a[0]), *P<VkSubmitInfo>(a[1]), As<VkFence>(a[2])); return 0;
        case 83:
            vk::trace::StateLibrary(As<VkPipeline>(a[0]), uint32_t(a[1]), *P<VkGraphicsPipelineCreateInfo>(a[2]),
                                    P<void>(a[3]));
            return 0;
        case 84: vk::trace::ShutdownBegins(); return 0;
        case 85: vk::trace::RecorderStarted(); return 0;
        case 86: vk::trace::RecorderMade(a[0]); return 0;
        case 87: vk::trace::RecorderStopped(); return 0;
        case 88: return vk::trace::Wanted();
        case 89: vk::trace::Finish(); return 0;
        case 90: return uint64_t(int64_t(vkDeviceWaitIdle(dev)));
        case 91:
        {
            VkMemoryRequirements r;
            vkGetBufferMemoryRequirements(dev, As<VkBuffer>(a[0]), &r);
            out[0] = r.size;
            out[1] = r.alignment;
            return r.memoryTypeBits;
        }
        case 92:
        {
            VkMemoryRequirements r;
            vkGetImageMemoryRequirements(dev, As<VkImage>(a[0]), &r);
            out[0] = r.size;
            out[1] = r.alignment;
            return r.memoryTypeBits;
        }
        case 93: return uint64_t(int64_t(vkGetFenceStatus(dev, As<VkFence>(a[0]))));
        case 94: return uint64_t(int64_t(vkWaitForFences(dev, uint32_t(a[0]), P<VkFence>(a[1]), uint32_t(a[2]), a[3])));
        case 95:
            return uint64_t(int64_t(vkGetQueryPoolResults(dev, As<VkQueryPool>(a[0]), uint32_t(a[1]), uint32_t(a[2]),
                                                          size_t(a[3]), out, a[4], uint32_t(a[5]))));
        case 96:
        {
            void* mapped = nullptr;
            const VkResult r = vkMapMemory(dev, As<VkDeviceMemory>(a[0]), a[1], a[2], 0, &mapped);
            out[0] = U(mapped);
            return uint64_t(int64_t(r));
        }
        case 97: vkUnmapMemory(dev, As<VkDeviceMemory>(a[0])); return 0;
        case 98:
        {
            VkFormatProperties p;
            vkGetPhysicalDeviceFormatProperties(As<VkPhysicalDevice>(a[0]), VkFormat(a[1]), &p);
            out[0] = p.linearTilingFeatures;
            out[1] = p.optimalTilingFeatures;
            return p.bufferFeatures;
        }
        case 99:
            vkGetPhysicalDeviceMemoryProperties(As<VkPhysicalDevice>(a[0]),
                                                reinterpret_cast<VkPhysicalDeviceMemoryProperties*>(out));
            return 0;
        case 100:
            vkGetPhysicalDeviceProperties(As<VkPhysicalDevice>(a[0]), reinterpret_cast<VkPhysicalDeviceProperties*>(out));
            return 0;
        case 101:
            vkGetPhysicalDeviceProperties2(As<VkPhysicalDevice>(a[0]),
                                           reinterpret_cast<VkPhysicalDeviceProperties2*>(uintptr_t(a[1])));
            return 0;
        case 102:
        {
            uint32_t n = 0;
            vkGetPhysicalDeviceQueueFamilyProperties(As<VkPhysicalDevice>(a[0]), &n, nullptr);
            uint32_t m = n < 4 ? n : 4;
            vkGetPhysicalDeviceQueueFamilyProperties(As<VkPhysicalDevice>(a[0]), &m,
                                                     reinterpret_cast<VkQueueFamilyProperties*>(out));
            return uint64_t(n) << 32 | m;
        }
        }
#undef CREATE
        return ~0ull;
    }

    uint64_t CppCall(const RigCall& c, uint64_t* out, const Dispatch& d)
    {
        if (!c.onRecorder) return CppMade(c, out, d);
        uint64_t result = 0;
        std::thread([&] {
            vk::trace::RecorderThread();
            result = CppMade(c, out, d);
        }).join();
        return result;
    }

    std::string Read(const std::string& path)
    {
        std::ifstream in(path, std::ios::binary);
        std::stringstream text;
        text << in.rdbuf();
        return text.str();
    }

    // The first line where two texts differ, with the lines before it.
    void ShowDifference(const char* what, uint64_t number, const std::string& a, const std::string& b)
    {
        std::vector<std::string> la, lb;
        std::stringstream sa(a), sb(b);
        for (std::string l; std::getline(sa, l);) la.push_back(l);
        for (std::string l; std::getline(sb, l);) lb.push_back(l);
        size_t at = 0;
        while (at < la.size() && at < lb.size() && la[at] == lb[at]) at++;
        std::printf("DIFFERENCE: case %llu: the %s differ at line %zu (%zu lines from C++, %zu from Rust)\n",
                    (unsigned long long)number, what, at + 1, la.size(), lb.size());
        for (size_t k = at > 3 ? at - 3 : 0; k < at; k++) std::printf("   %s\n", la[k].c_str());
        std::printf("  C++:  %s\n  Rust: %s\n", at < la.size() ? la[at].c_str() : "(nothing)",
                    at < lb.size() ? lb[at].c_str() : "(nothing)");
    }

    const char* kSwitches[] = { "MW2_DETERMINISTIC", "MW2_FORCE_INSTANCE_DEPTH_RESOLVE", "MW2_FORCE_RESOLVE_TWIN",
                                "MW2_DRAWN_RESOLVE_SETS", "MW2_FORCE_RESOLVE_REMAKE", "MW2_NO_MULTI_DRAW",
                                "MW2_FORCE_LONE_UPLOADS", "MW2_STAGING_LEAST_KB", "MW2_FAKE_DEVICE_LOST" };
    const char* kValues[] = { "1", "0", "", "3", " 7", "x", "12abc", "1e1", "0.5", "-1", "2.25", "99999999999" };
}

int TracePart(uint64_t seed, uint64_t cases)
{
    char folder[] = "/tmp/claude-1000/rig_traceXXXXXX";
    std::system("mkdir -p /tmp/claude-1000");
    if (!mkdtemp(folder)) { std::printf("no scratch folder\n"); return 2; }
    const std::string base = std::string(folder) + "/trace.txt";
    uint64_t lines = 0, calls = 0, kinds[kOps + 1]{};
    std::mt19937_64 top(seed * 7919 + 13);
    // FNV-1a over words, straight and through streaming loads, at every alignment.
    {
        std::vector<uint8_t> bytes(20000);
        for (auto& b : bytes) b = uint8_t(top());
        for (int k = 0; k < 2000; k++)
        {
            const size_t from = top() % 64, size = top() % 12000;
            const uint64_t c1 = vk::trace::Fnv64(bytes.data() + from, size);
            const uint64_t c2 = vk::trace::FnvWriteCombined(bytes.data() + from, size);
            if (c1 != mw2_rig_vk_fnv(bytes.data() + from, size, false) || c2 != mw2_rig_vk_fnv(bytes.data() + from, size, true))
            {
                std::printf("DIFFERENCE: fnv of %zu bytes at +%zu\n", size, from);
                return 1;
            }
        }
    }
    for (uint64_t number = 0; number < cases; number++)
    {
        Gen g(top());
        // The switches, the same for both sides.
        std::string path = base;
        const uint32_t how = g.u(100);
        std::string asked;
        if (how < 50) asked = base;
        else if (how < 75) asked = base + ":" + std::to_string(g.u(5)) + ":" + std::to_string(g.u(7));
        else if (how < 80) { asked = base + ":x:" + std::to_string(g.u(3)); path = asked; }
        else if (how < 85) { asked = base + ":" + std::to_string(g.u(3)); path = asked; }
        else if (how < 90) asked = "";
        else if (how < 95) asked = std::string(folder) + "/no/such/folder/trace.txt";
        if (how < 85 || (how >= 90 && how < 95)) setenv("MW2_TRACE_COMMANDS", asked.c_str(), 1);
        else if (how < 90) setenv("MW2_TRACE_COMMANDS", "", 1);
        else unsetenv("MW2_TRACE_COMMANDS");
        for (const char* name : kSwitches)
            if (g.p(15)) setenv(name, kValues[g.u(12)], 1);
            else unsetenv(name);
        const bool noMulti = g.p(10);
        const int noDynamic = g.p(5) ? 1 : g.p(5) ? 2 : 0;
        rig_vk::Withhold(noMulti, noDynamic);
        const uint32_t multiDraw = g.p(80) ? 1 + g.u(2048) : 0;
        // The handles the renderer did not make.
        const uint64_t device = 0x20, physical = 0x30, queue = 0x40, cache = 0x50, layout = 0x60;
        const uint32_t nLayouts = g.u(5);
        uint64_t setLayouts[4];
        for (uint32_t k = 0; k < nLayouts; k++) setLayouts[k] = 0x70 + k * 8;
        g.foreign[kDevice].push_back(device);
        g.foreign[kPhysical].push_back(physical);
        g.foreign[kQueue].push_back(queue);
        g.foreign[kCache].push_back(cache);
        g.foreign[kPipelineLayout].push_back(layout);
        for (uint32_t k = 0; k < nLayouts; k++) g.foreign[kSetLayout].push_back(setLayouts[k]);
        std::vector<RigCall> script;
        const uint32_t n = 50 + g.u(400);
        for (uint32_t k = 0; k < n; k++)
        {
            RigCall c = g.call();
            if ((noMulti || !multiDraw) && (c.op == 73 || c.op == 74)) continue;
            if ((noDynamic == 1 && c.op >= 64 && c.op <= 72) || (noDynamic == 2 && c.op == 72)) continue;
            script.push_back(c);
        }
        RigCall finish{};
        finish.op = 89;
        script.push_back(finish);

        std::string trace[2], results[2];
        std::vector<uint64_t> logged[2];
        uint64_t forced[2][10]{};
        for (int side = 0; side < 2; side++)
        {
            std::remove(path.c_str());
            rig_vk::g_next = 0x1000;
            rig_vk::g_position = 0;
            rig_vk::g_calls.clear();
            rig_vk::g_logging = true;
            rig_log::Take(side);
            Dispatch d{};
            if (side == 0)
            {
                rig_cpp_trace_reset();
                g_device = As<VkDevice>(device);
                vk::trace::Start(As<VkDevice>(device), As<VkPhysicalDevice>(physical), As<VkQueue>(queue),
                                 As<VkPipelineCache>(cache), As<VkPipelineLayout>(layout),
                                 reinterpret_cast<const VkDescriptorSetLayout*>(setLayouts), nLayouts, rig_position);
                // The renderer's Dispatch (renderer.cpp's Initialise).
                d.beginRendering = PFN_vkCmdBeginRenderingKHR(rig_vk::Command("vkCmdBeginRenderingKHR"));
                d.endRendering = PFN_vkCmdEndRenderingKHR(rig_vk::Command("vkCmdEndRenderingKHR"));
                d.setCullMode = PFN_vkCmdSetCullModeEXT(rig_vk::Command("vkCmdSetCullModeEXT"));
                d.setFrontFace = PFN_vkCmdSetFrontFaceEXT(rig_vk::Command("vkCmdSetFrontFaceEXT"));
                d.setDepthTestEnable = PFN_vkCmdSetDepthTestEnableEXT(rig_vk::Command("vkCmdSetDepthTestEnableEXT"));
                d.setDepthWriteEnable = PFN_vkCmdSetDepthWriteEnableEXT(rig_vk::Command("vkCmdSetDepthWriteEnableEXT"));
                d.setDepthCompareOp = PFN_vkCmdSetDepthCompareOpEXT(rig_vk::Command("vkCmdSetDepthCompareOpEXT"));
                d.setStencilTestEnable = PFN_vkCmdSetStencilTestEnableEXT(rig_vk::Command("vkCmdSetStencilTestEnableEXT"));
                d.setStencilOp = PFN_vkCmdSetStencilOpEXT(rig_vk::Command("vkCmdSetStencilOpEXT"));
                if ((d.multiDraw = multiDraw))
                {
                    d.drawMulti = PFN_vkCmdDrawMultiEXT(rig_vk::Command("vkCmdDrawMultiEXT"));
                    d.drawMultiIndexed = PFN_vkCmdDrawMultiIndexedEXT(rig_vk::Command("vkCmdDrawMultiIndexedEXT"));
                    if (!d.drawMulti || !d.drawMultiIndexed) d.multiDraw = 0;
                }
                if (vk::trace::Forcing().noMultiDraw) d.multiDraw = 0;
                vk::trace::WrapDispatch(d.beginRendering, d.endRendering, d.setCullMode, d.setFrontFace,
                                        d.setDepthTestEnable, d.setDepthWriteEnable, d.setDepthCompareOp,
                                        d.setStencilTestEnable, d.setStencilOp, d.drawMulti, d.drawMultiIndexed);
                const Forced& f = vk::trace::Forcing();
                double lost = f.fakeDeviceLost;
                uint64_t lostBits;
                std::memcpy(&lostBits, &lost, 8);
                const bool dynamic = d.beginRendering && d.endRendering && d.setCullMode && d.setStencilOp;
                const uint64_t values[10] = { f.instanceDepthResolve, f.resolveTwin, f.drawnResolveSets, f.resolveRemake,
                                              f.noMultiDraw, f.loneUploads, f.stagingLeastKb, lostBits,
                                              vk::trace::Deterministic(), uint64_t(d.multiDraw) | uint64_t(dynamic) << 32 };
                std::memcpy(forced[0], values, sizeof values);
            }
            else
            {
                mw2_rig_vk_begin(device, physical, queue, cache, layout, setLayouts, nLayouts, multiDraw);
                mw2_rig_vk_forced(forced[1]);
            }
            std::string& out = results[side];
            for (const RigCall& c : script)
            {
                if (c.op == kSetPosition) { rig_vk::g_position = c.a[0]; continue; }
                rig_vk::g_result = VkResult(int64_t(c.a[11]));
                uint64_t buffer[1100]{};
                const uint64_t r = side == 0 ? CppCall(c, buffer, d) : mw2_rig_vk_call(&c, buffer);
                char line[64];
                std::snprintf(line, sizeof line, "%u %llx", c.op, (unsigned long long)r);
                out += line;
                // What a call wrote back: the handles it made, or what a query filled in.
                const int words = c.op == 99 || c.op == 100 ? 120 : 12;
                for (int k = 0; k < words; k++)
                {
                    std::snprintf(line, sizeof line, " %llx", (unsigned long long)buffer[k]);
                    out += line;
                }
                out += '\n';
                if (side == 0) kinds[c.op < kOps ? c.op : kOps]++;
            }
            rig_log::Stop();
            rig_vk::g_logging = false;
            logged[side] = rig_vk::g_calls;
            trace[side] = Read(path);
        }
        unsetenv("MW2_TRACE_COMMANDS");
        const std::string logs[2] = { rig_log::g_log[0], rig_log::g_log[1] };
        rig_log::Clear();
        if (trace[0] != trace[1]) { ShowDifference("trace files", number, trace[0], trace[1]); return 1; }
        if (logs[0] != logs[1]) { ShowDifference("printed lines", number, logs[0], logs[1]); return 1; }
        if (results[0] != results[1]) { ShowDifference("results", number, results[0], results[1]); return 1; }
        if (logged[0] != logged[1])
        {
            size_t at = 0;
            while (at < logged[0].size() && at < logged[1].size() && logged[0][at] == logged[1][at]) at++;
            std::printf("DIFFERENCE: case %llu: the stand-ins were called differently at word %zu of %zu/%zu (word %llx"
                        " against %llx)\n",
                        (unsigned long long)number, at, logged[0].size(), logged[1].size(),
                        (unsigned long long)(at < logged[0].size() ? logged[0][at] : 0),
                        (unsigned long long)(at < logged[1].size() ? logged[1][at] : 0));
            // The stand-in call it falls in: each logged as its word count, then its words.
            size_t from = 0;
            while (from + logged[0][from] + 1 <= at) from += logged[0][from] + 1;
            for (int side = 0; side < 2; side++)
            {
                std::printf("  %s:", side ? "Rust" : "C++ ");
                for (size_t k = from; k < logged[side].size() && k <= from + logged[side][from]; k++)
                    std::printf(" %llx", (unsigned long long)logged[side][k]);
                std::printf("\n");
            }
            return 1;
        }
        if (std::memcmp(forced[0], forced[1], sizeof forced[0]))
        {
            std::printf("DIFFERENCE: case %llu: the forced switches or the dispatch differ\n", (unsigned long long)number);
            for (int k = 0; k < 10; k++)
                std::printf("  %d: C++ %llx Rust %llx\n", k, (unsigned long long)forced[0][k], (unsigned long long)forced[1][k]);
            return 1;
        }
        // RIG_TRACE_KEEP=<file>: every case's trace, one after another, to look at.
        if (const char* keep = std::getenv("RIG_TRACE_KEEP"))
            if (std::FILE* f = std::fopen(keep, number ? "ab" : "wb"))
            {
                std::fprintf(f, "== case %llu\n%s%s", (unsigned long long)number, trace[0].c_str(), logs[0].c_str());
                std::fclose(f);
            }
        lines += uint64_t(std::count(trace[0].begin(), trace[0].end(), '\n'));
        calls += script.size();
    }
    rig_cpp_trace_reset();
    std::system((std::string("rm -rf ") + folder).c_str());
    uint64_t missing = 0;
    for (uint32_t k = 0; k < kOps; k++) missing += kinds[k] == 0;
    std::printf("%llu cases, %llu calls, %llu trace lines the same, every printed line, stand-in call and result too;"
                " %llu of %u kinds of call never made\n",
                (unsigned long long)cases, (unsigned long long)calls, (unsigned long long)lines,
                (unsigned long long)missing, kOps);
    return missing ? 1 : 0;
}
