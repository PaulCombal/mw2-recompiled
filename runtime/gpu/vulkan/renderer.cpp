// The renderer: the title's draws and resolves, recorded into Vulkan. This
// file holds its state and brings it up and down; renderer_state.h lists the
// parts.
#include "renderer.h"
#include "../../diagnostics.h"

#ifndef MW2_HAVE_VULKAN
bool vk::renderer::Initialise() { return false; }
void vk::renderer::CheckCompiledShaders(const std::string&) {}
bool vk::renderer::CompiledShaders(std::string&, std::string&) { return true; }
void vk::renderer::Shutdown() {}
bool vk::renderer::Ready() { return false; }
void vk::renderer::Draw(const gpu::RegisterFile&, const DrawCall&) {}
void vk::renderer::Resolve(const gpu::RegisterFile&) {}
void vk::renderer::Swap(uint32_t) {}
void vk::renderer::BeginOcclusionQuery() {}
bool vk::renderer::EndOcclusionQuery(uint32_t, uint64_t& samples) { samples = 0; return true; }
void vk::renderer::CollectOcclusionQueries(void (*)(uint32_t, uint64_t)) {}
void vk::renderer::Report() {}
void vk::renderer::FlashSeen(const char*) {}
uint64_t vk::renderer::CompletedSubmission() { return 0; }
void vk::renderer::BeforeStreamWrite(uint32_t, uint32_t) {}
void vk::renderer::BeforeCompletion() {}
void vk::renderer::Idle() {}
#else

#include "renderer_state.h"
#include "shader_build.h"
#include "shader_marker.h"
#include "display_table.h"
#include "texture_cache.h"
#include "../memory_watch.h"
#include "../../guest_memory.h"
#include "../../crash.h"
#include "../../env.h"
#include "../../log.h"
#include "../../report.h"
#include "../../render_size.h"

#include <algorithm>

namespace vk::renderer::detail
{
    State g;
}

namespace
{
    struct Compiled
    {
        bool checked = false, current = true;
        std::string gameFolder, why, device;
    } g_compiled;
}

void vk::renderer::CheckCompiledShaders(const std::string& gameFolder)
{
    g_compiled.checked = true;
    g_compiled.gameFolder = gameFolder;
}

bool vk::renderer::CompiledShaders(std::string& why, std::string& device)
{
    why = g_compiled.why;
    device = g_compiled.device;
    return g_compiled.current;
}

using namespace vk::renderer::detail;

bool vk::renderer::Initialise()
{
    if (g.device) return true;
    if (!vk::pipeline::Ready() && !vk::pipeline::Initialise()) return false;

    g.device = static_cast<VkDevice>(vk::pipeline::Device());
    g.physical = static_cast<VkPhysicalDevice>(vk::pipeline::PhysicalDevice());
    g.queue = static_cast<VkQueue>(vk::pipeline::Queue());
    if (!g.device || !g.queue) { g.device = VK_NULL_HANDLE; return false; }
    if (!vk::textures::Initialise()) { g.device = VK_NULL_HANDLE; return false; }

    vkGetPhysicalDeviceMemoryProperties(g.physical, &g.memory);
    VkPhysicalDeviceProperties properties{};
    vkGetPhysicalDeviceProperties(g.physical, &properties);
    g.uniformAlignment = uint32_t(std::max<VkDeviceSize>(
        std::max(properties.limits.minUniformBufferOffsetAlignment,
                 properties.limits.minStorageBufferOffsetAlignment), 256));

    // AMD has no D24_UNORM_S8_UINT, so the depth format is whichever 24-bit-ish
    // combined format the device actually has.
    g.depthFormat = vk::shaders::DepthFormat(g.physical);
    if (g.depthFormat == VK_FORMAT_UNDEFINED) { LOGW("renderer: no depth format"); return false; }

    // The console's 2x and 4x surfaces are drawn with that many samples unless
    // MW2_NO_MSAA=1. A count is usable when colour, depth and stencil can all
    // be drawn at it; and a multisampled depth target can only be read back
    // through a render pass resolve, which is Vulkan 1.2 and has to take the
    // first sample -- the only mode every 1.2 device is required to offer.
    {
        const bool off = env::Flag("MW2_NO_MSAA");
        g.sampleCounts = properties.limits.framebufferColorSampleCounts &
                         properties.limits.framebufferDepthSampleCounts &
                         properties.limits.framebufferStencilSampleCounts;
        VkPhysicalDeviceDepthStencilResolveProperties resolve{
            VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DEPTH_STENCIL_RESOLVE_PROPERTIES };
        VkPhysicalDeviceProperties2 properties2{ VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2 };
        properties2.pNext = &resolve;
        const bool modern = properties.apiVersion >= VK_API_VERSION_1_2;
        if (modern) vkGetPhysicalDeviceProperties2(g.physical, &properties2);
        const bool depthResolve = modern &&
            (resolve.supportedDepthResolveModes & VK_RESOLVE_MODE_SAMPLE_ZERO_BIT);
        g.msaa = !off && depthResolve && (g.sampleCounts & ~VkSampleCountFlags(VK_SAMPLE_COUNT_1_BIT));
        if (g.msaa)
            LOGI("renderer: multisampling on: the device draws at%s%s%s%s, the title's 2x and 4x"
                 " surfaces are drawn at %ux and %ux%s",
                 (g.sampleCounts & VK_SAMPLE_COUNT_2_BIT) ? " 2x" : "",
                 (g.sampleCounts & VK_SAMPLE_COUNT_4_BIT) ? " 4x" : "",
                 (g.sampleCounts & VK_SAMPLE_COUNT_8_BIT) ? " 8x" : "",
                 (g.sampleCounts & VK_SAMPLE_COUNT_16_BIT) ? " 16x" : "",
                 unsigned(HostSamples(1)), unsigned(HostSamples(2)),
                 env::Text("MW2_MSAA") ? " (MW2_MSAA)" : "");
        else
            LOGI("renderer: multisampling off (%s): the title's 2x and 4x surfaces are drawn at 1x",
                 off ? "MW2_NO_MSAA" : !modern ? "the device is older than Vulkan 1.2"
                     : !depthResolve ? "no first-sample depth resolve"
                     : "the device draws at 1x only");
    }

    // MW2_SCALE=2 or 3 draws everything that many times wider and taller. A
    // surface is at most 4096 of the title's pixels either way, and the scale
    // is lowered until that fits an image of the device's. The frame is the
    // title's (render_size.h).
    {
        VkPhysicalDeviceProperties properties{};
        vkGetPhysicalDeviceProperties(g.physical, &properties);
        const uint32_t asked = uint32_t(std::clamp<uint64_t>(env::Number("MW2_SCALE", 1), 1, 3));
        g.scale = vk::shaders::Scale(g.physical, asked);
        const render::Size frame = render::Frame();
        g.presentWidth = frame.width * g.scale;
        g.presentHeight = frame.height * g.scale;
        if (g.scale != asked)
            LOGW("renderer: MW2_SCALE=%u is more than the device's %u-pixel images hold",
                 asked, properties.limits.maxImageDimension2D);
        if (g.scale > 1)
            LOGI("renderer: drawing at %ux the title's sizes, a %ux%u frame", g.scale,
                 g.presentWidth, g.presentHeight);
    }

    // The launcher compiled every shader for a device, a scale and the
    // game's files; this run draws on the device and at the scale just
    // settled.
    if (g_compiled.checked)
    {
        g_compiled.device = vk::pipeline::DeviceId(g.physical);
        const std::string fingerprint = vk::shaders::Fingerprint(g.physical, g.scale, g_compiled.gameFolder,
                                                                   env::Flag("MW2_CUSTOM_MAPS"));
        g_compiled.current = vk::shaders::Compiled(fingerprint, &g_compiled.why);
        const vk::shaders::Marker marker = vk::shaders::ReadMarker();
        if (g_compiled.current && marker.shaders && !vk::pipeline::CacheLoaded())
        {
            g_compiled.current = false;
            g_compiled.why = "The graphics driver did not take the compiled shaders.";
        }
        if (g_compiled.current)
        {
            LOGI("renderer: every shader of the game is compiled for this device (%u)", marker.shaders);
            report::ShadersCompiled(marker.shaders ? "yes, " + std::to_string(marker.shaders) + " for this device, driver and scale"
                                                   : std::string("none: the device has no pipeline libraries, so each is compiled at its first draw"));
        }
        else
        {
            LOGW("renderer: the game's shaders are not compiled for this run: %s", g_compiled.why.c_str());
            report::ShadersCompiled("no: " + g_compiled.why);
            // What this run would write back is not what the marker describes.
            vk::pipeline::SetCachePath(nullptr);
        }
    }

    VkCommandPoolCreateInfo pool{ VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO };
    pool.queueFamilyIndex = vk::pipeline::QueueFamily();
    VkCommandBufferAllocateInfo allocate{ VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO };
    allocate.level = VK_COMMAND_BUFFER_LEVEL_PRIMARY;
    allocate.commandBufferCount = 1;
    // A pool each, because a slot is reset on its own while the others are busy.
    for (State::Slot& slot : g.slots)
    {
        if (vkCreateCommandPool(g.device, &pool, nullptr, &slot.commands) != VK_SUCCESS) return false;
        allocate.commandPool = slot.commands;
        allocate.commandBufferCount = kSegmentBuffers;
        slot.buffers.resize(kSegmentBuffers);
        if (vkAllocateCommandBuffers(g.device, &allocate, slot.buffers.data()) != VK_SUCCESS) return false;
        allocate.commandBufferCount = 1;
    }

    VkCommandPoolCreateInfo setupPool{ VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO };
    setupPool.flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT;
    setupPool.queueFamilyIndex = vk::pipeline::QueueFamily();
    if (vkCreateCommandPool(g.device, &setupPool, nullptr, &g.setupCommands) != VK_SUCCESS) return false;
    allocate.commandPool = g.setupCommands;
    if (vkAllocateCommandBuffers(g.device, &allocate, &g.setup) != VK_SUCCESS) return false;

    VkQueryPoolCreateInfo queries{ VK_STRUCTURE_TYPE_QUERY_POOL_CREATE_INFO };
    queries.queryType = VK_QUERY_TYPE_OCCLUSION;
    queries.queryCount = kGuestQueryRing;
    if (vkCreateQueryPool(g.device, &queries, nullptr, &g.guestQueries) != VK_SUCCESS)
        g.guestQueries = VK_NULL_HANDLE;

    VkFenceCreateInfo fence{ VK_STRUCTURE_TYPE_FENCE_CREATE_INFO };
    for (State::Slot& slot : g.slots)
        if (vkCreateFence(g.device, &fence, nullptr, &slot.fence) != VK_SUCCESS) return false;
    if (vkCreateFence(g.device, &fence, nullptr, &g.setupFence) != VK_SUCCESS) return false;

    // Constants, vertex data and indices all live in the arena, which is why
    // it carries every usage at once.
    // Ask for the whole thing and settle for what the device will give: on a
    // shared-memory part this is system RAM, and refusing to start because the
    // largest size did not fit would be the wrong answer.
    //
    // Behind the arena, when there is room, the shadow of guest physical
    // memory: vertex data a draw reads from pages the title has not written
    // since they were copied is read there instead of being copied again (see
    // gpu/memory_watch.h). One buffer, so set 2 reaches both and a fetch
    // constant says which by its address alone.
    static const bool noShadow = diag::Flag("MW2_NO_SHADOW");
    const uint32_t shadowBytes = guest::kPhysicalEnd - guest::kPhysicalBase;
    uint32_t wanted = uint32_t(env::Number("MW2_ARENA_MB", kArenaWanted >> 20)) << 20;
    bool withShadow = !noShadow;
    for (; wanted >= kArenaLeast; withShadow ? (void)(withShadow = false) : (void)(wanted /= 2))
    {
        VkBufferCreateInfo buffer{ VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO };
        buffer.size = VkDeviceSize(wanted) + (withShadow ? shadowBytes : 0);
        buffer.usage = VK_BUFFER_USAGE_UNIFORM_BUFFER_BIT | VK_BUFFER_USAGE_STORAGE_BUFFER_BIT |
                       VK_BUFFER_USAGE_INDEX_BUFFER_BIT;
        if (vkCreateBuffer(g.device, &buffer, nullptr, &g.arena) != VK_SUCCESS) continue;
        VkMemoryRequirements needs{};
        vkGetBufferMemoryRequirements(g.device, g.arena, &needs);
        const uint32_t type = FindMemory(needs.memoryTypeBits,
                                         VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT |
                                         VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
        if (type == UINT32_MAX) { LOGW("renderer: no host-visible memory"); return false; }
        VkMemoryAllocateInfo allocateArena{ VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO };
        allocateArena.allocationSize = needs.size;
        allocateArena.memoryTypeIndex = type;
        if (vkAllocateMemory(g.device, &allocateArena, nullptr, &g.arenaMemory) == VK_SUCCESS &&
            vkBindBufferMemory(g.device, g.arena, g.arenaMemory, 0) == VK_SUCCESS &&
            vkMapMemory(g.device, g.arenaMemory, 0, buffer.size, 0,
                        reinterpret_cast<void**>(&g.arenaMapped)) == VK_SUCCESS)
        {
            g.arenaBytes = wanted;
            if (withShadow)
            {
                g.shadowBase = wanted;
                gpu::watch::SetShadow(g.arenaMapped + wanted, shadowBytes);
            }
            break;
        }
        if (g.arenaMemory) vkFreeMemory(g.device, g.arenaMemory, nullptr);
        vkDestroyBuffer(g.device, g.arena, nullptr);
        g.arenaMemory = VK_NULL_HANDLE;
        g.arena = VK_NULL_HANDLE;
    }
    if (!g.arenaBytes) { LOGW("renderer: no frame arena"); return false; }

    // A slot's share has to hold everything one submission copies -- a frame's
    // is about 50 MB here against the 512 MB asked for, so three shares fit with
    // room to spare. An overflow is reported rather than silently wrapping.
    const uint32_t share = (g.arenaBytes / kFrameSlots) & ~(g.uniformAlignment - 1);
    g.arenaSlotBytes = share;
    for (uint32_t i = 0; i < kFrameSlots; i++) g.slots[i].arenaBase = i * share;
    g.command = g.slots[g.slot].buffers[0];
    g.arenaBase = g.slots[g.slot].arenaBase;
    LOGI("renderer: %u frame slots of %u KB of arena each", kFrameSlots, g.arenaSlotBytes / 1024);

    // Before any command is recorded: the recorder thread only reads these.
    const auto command = [](const char* name) { return vkGetDeviceProcAddr(g.device, name); };
    dispatch.beginRendering = PFN_vkCmdBeginRenderingKHR(command("vkCmdBeginRenderingKHR"));
    dispatch.endRendering = PFN_vkCmdEndRenderingKHR(command("vkCmdEndRenderingKHR"));
    dispatch.setCullMode = PFN_vkCmdSetCullModeEXT(command("vkCmdSetCullModeEXT"));
    dispatch.setFrontFace = PFN_vkCmdSetFrontFaceEXT(command("vkCmdSetFrontFaceEXT"));
    dispatch.setDepthTestEnable = PFN_vkCmdSetDepthTestEnableEXT(command("vkCmdSetDepthTestEnableEXT"));
    dispatch.setDepthWriteEnable = PFN_vkCmdSetDepthWriteEnableEXT(command("vkCmdSetDepthWriteEnableEXT"));
    dispatch.setDepthCompareOp = PFN_vkCmdSetDepthCompareOpEXT(command("vkCmdSetDepthCompareOpEXT"));
    dispatch.setStencilTestEnable = PFN_vkCmdSetStencilTestEnableEXT(command("vkCmdSetStencilTestEnableEXT"));
    dispatch.setStencilOp = PFN_vkCmdSetStencilOpEXT(command("vkCmdSetStencilOpEXT"));
    if (!dispatch.beginRendering || !dispatch.endRendering || !dispatch.setCullMode ||
        !dispatch.setStencilOp)
    {
        LOGW("renderer: the device did not give dynamic rendering or dynamic state commands");
        return false;
    }

    VkDescriptorPoolSize size{ VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 2 };
    VkDescriptorPoolCreateInfo descriptors{ VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO };
    descriptors.maxSets = 1;
    descriptors.poolSizeCount = 1;
    descriptors.pPoolSizes = &size;
    if (vkCreateDescriptorPool(g.device, &descriptors, nullptr, &g.descriptors) != VK_SUCCESS)
        return false;

    VkDescriptorSetLayout memoryLayout =
        static_cast<VkDescriptorSetLayout>(vk::pipeline::SetLayout(bindings::kArenaSet));
    VkDescriptorSetAllocateInfo allocateSet{ VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO };
    allocateSet.descriptorPool = g.descriptors;
    allocateSet.descriptorSetCount = 1;
    allocateSet.pSetLayouts = &memoryLayout;
    if (vkAllocateDescriptorSets(g.device, &allocateSet, &g.memorySet) != VK_SUCCESS) return false;

    // The whole arena, as dwords and as vec4.
    VkDescriptorBufferInfo buffer{ g.arena, 0, VK_WHOLE_SIZE };
    VkWriteDescriptorSet writes[2]{};
    for (uint32_t i = 0; i < 2; i++)
    {
        writes[i].sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET;
        writes[i].dstSet = g.memorySet;
        writes[i].dstBinding = i ? bindings::kArenaFloats : bindings::kArenaBinding;
        writes[i].descriptorCount = 1;
        writes[i].descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER;
        writes[i].pBufferInfo = &buffer;
    }
    vkUpdateDescriptorSets(g.device, 2, writes, 0, nullptr);

    for (uint32_t i = 0; i < kPresentImages; i++)
    {
        Target image;
        image.format = VK_FORMAT_R8G8B8A8_UNORM;
        image.width = g.presentWidth / g.scale;     // MakeImage scales
        image.height = g.presentHeight / g.scale;
        // STORAGE so the display's colour table can be applied to it in place.
        if (!MakeImage(image, VK_IMAGE_USAGE_TRANSFER_DST_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT |
                              VK_IMAGE_USAGE_STORAGE_BIT, 0))
            return false;
        g.present[i] = image.image;
        g.presentMemory[i] = image.memory;
    }

    LOGI("renderer: ready on %s, depth %s, %u MB frame arena%s", vk::pipeline::DeviceName(),
         g.depthFormat == VK_FORMAT_D24_UNORM_S8_UINT ? "D24S8" : "D32S8", g.arenaBytes >> 20,
         g.shadowBase ? " and a shadow of guest memory behind it" : ", no shadow of guest memory");
    StartPreparing();
    PrewarmFromCache();
    // A lost device ends the run, the way closing the window does.
    vk::pipeline::OnDeviceLost(crash::RequestExit);
    vk::record::Start();
    return true;
}

void vk::renderer::Shutdown()
{
    if (!g.device) return;
    // Before anything is destroyed, the display table's pipeline included: the
    // last frames are still on the GPU, and they use it -- and the last
    // commands may still be with the recorder thread.
    vk::record::Stop();
    vkDeviceWaitIdle(g.device);
    StopPreparing();
    vk::display::Shutdown();
    WriteShaderCache();
    SummariseTargets();
    g.pipelineCount = g.pipelines.size();
    g.shaderCount = g.shaders.size();
    for (auto& [key, pipeline] : g.pipelines) if (pipeline) vkDestroyPipeline(g.device, pipeline, nullptr);
    for (VkPipeline pipeline : g.replacedPipelines) vkDestroyPipeline(g.device, pipeline, nullptr);
    for (auto& [key, library] : g.inputLibraries) if (library) vkDestroyPipeline(g.device, library, nullptr);
    for (auto& [key, library] : g.outputLibraries) if (library) vkDestroyPipeline(g.device, library, nullptr);
    g.replacedPipelines.clear();
    g.inputLibraries.clear();
    g.outputLibraries.clear();
    for (auto& [hash, shader] : g.shaders)
    {
        if (shader.library) vkDestroyPipeline(g.device, shader.library, nullptr);
        if (shader.module) vkDestroyShaderModule(g.device, shader.module, nullptr);
    }
    for (auto& [key, target] : g.targets) DestroyTarget(target);
    g.pipelines.clear();
    g.shaders.clear();
    for (auto& last : g.lastShader) last = {};
    g.targets.clear();
    for (uint32_t i = 0; i < kPresentImages; i++)
    {
        if (g.present[i]) vkDestroyImage(g.device, g.present[i], nullptr);
        if (g.presentMemory[i]) vkFreeMemory(g.device, g.presentMemory[i], nullptr);
        g.present[i] = VK_NULL_HANDLE;
        g.presentMemory[i] = VK_NULL_HANDLE;
    }
    for (auto& [address, ring] : g.resolvedTo)
        for (Resolved& resolved : ring.copies)
        {
            if (resolved.view) vkDestroyImageView(g.device, resolved.view, nullptr);
            if (resolved.gammaView) vkDestroyImageView(g.device, resolved.gammaView, nullptr);
            if (resolved.image) vkDestroyImage(g.device, resolved.image, nullptr);
            if (resolved.memory) vkFreeMemory(g.device, resolved.memory, nullptr);
        }
    g.resolvedTo.clear();
    g.fetchedWidth.clear();
    g.resolveEpoch++;
    if (g.readBack) vkDestroyBuffer(g.device, g.readBack, nullptr);
    if (g.readBackMemory) vkFreeMemory(g.device, g.readBackMemory, nullptr);
    g.readBack = VK_NULL_HANDLE;
    g.readBackMemory = VK_NULL_HANDLE;
    g.readBackMapped = nullptr;
    if (g.descriptors) vkDestroyDescriptorPool(g.device, g.descriptors, nullptr);
    gpu::watch::SetShadow(nullptr, 0);
    g.shadowBase = 0;
    if (g.arenaMapped) vkUnmapMemory(g.device, g.arenaMemory);
    if (g.arena) vkDestroyBuffer(g.device, g.arena, nullptr);
    if (g.arenaMemory) vkFreeMemory(g.device, g.arenaMemory, nullptr);
    if (g.guestQueries) vkDestroyQueryPool(g.device, g.guestQueries, nullptr);
    for (State::Slot& slot : g.slots)
    {
        if (slot.fence) vkDestroyFence(g.device, slot.fence, nullptr);
        if (slot.commands) vkDestroyCommandPool(g.device, slot.commands, nullptr);
    }
    if (g.setupFence) vkDestroyFence(g.device, g.setupFence, nullptr);

    if (g.setupCommands) vkDestroyCommandPool(g.device, g.setupCommands, nullptr);

    // The handles are gone; the counters stay, because the report is written
    // after the shutdown.
    g.arena = VK_NULL_HANDLE;
    g.arenaMemory = VK_NULL_HANDLE;
    g.arenaMapped = nullptr;
    g.guestQueries = VK_NULL_HANDLE;
    g.descriptors = VK_NULL_HANDLE;
    g.setupFence = VK_NULL_HANDLE;
    g.setupCommands = VK_NULL_HANDLE;
    for (State::Slot& slot : g.slots) slot = State::Slot{};
    g.recording = false;
    g.device = VK_NULL_HANDLE;
}

bool vk::renderer::Ready() { return g.device != VK_NULL_HANDLE; }

void vk::renderer::Idle() { vk::record::Flush(); }

#endif  // MW2_HAVE_VULKAN
