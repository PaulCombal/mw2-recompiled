#include "pipeline.h"
#include "bindings.h"
#include "../../env.h"
#include "../../log.h"
#include "../../report.h"

#ifndef MW2_HAVE_VULKAN
// No loader, so nothing here can be built. The callers all tolerate a device
// that never comes up, because that is what a machine with no GPU gives them.
bool vk::pipeline::Initialise(void*, void*, void*, uint32_t, void*) { return false; }
void vk::pipeline::Shutdown() {}
bool vk::pipeline::Ready() { return false; }
const char* vk::pipeline::DeviceName() { return "none"; }
void*    vk::pipeline::Instance() { return nullptr; }
void*    vk::pipeline::Device() { return nullptr; }
void*    vk::pipeline::PhysicalDevice() { return nullptr; }
void*    vk::pipeline::Queue() { return nullptr; }
uint32_t vk::pipeline::QueueFamily() { return 0; }
bool     vk::pipeline::PreciseOcclusionQueries() { return false; }
float    vk::pipeline::MaxAnisotropy() { return 1.0f; }
bool     vk::pipeline::NonSeamlessCubes() { return false; }
uint32_t vk::pipeline::MultiDraw() { return 0; }
uint32_t vk::pipeline::Samplers() { return 0; }
bool     vk::pipeline::PipelineLibraries() { return false; }
std::mutex& vk::pipeline::QueueMutex() { static std::mutex m; return m; }
void*    vk::pipeline::SetLayout(uint32_t) { return nullptr; }
void*    vk::pipeline::Layout() { return nullptr; }
void*    vk::pipeline::Cache() { return nullptr; }
bool vk::pipeline::CreateModule(const uint32_t*, size_t, const char** error)
{
    if (error) *error = "built without Vulkan";
    return false;
}
bool vk::pipeline::DeviceHasExtension(void*, const char*) { return false; }
uint32_t vk::pipeline::GraphicsFamily(void*) { return UINT32_MAX; }
void vk::pipeline::SetCachePath(const char*) {}
bool vk::pipeline::CacheLoaded() { return false; }
void vk::pipeline::SetDevice(const std::string&) {}
std::string vk::pipeline::DeviceId(void*) { return {}; }
void* vk::pipeline::ChooseDevice(void*, const std::string&, uint32_t*) { return nullptr; }
bool vk::pipeline::CreationFeedback() { return false; }
bool vk::pipeline::CreateDevice(void*, uint32_t, const char* const*, uint32_t, void**, void**, void*)
{
    return false;
}
bool vk::pipeline::Failed(int vkResult, const char*) { return vkResult != 0; }
void vk::pipeline::OnDeviceLost(void (*)(const char*)) {}
#else

#include <atomic>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <string>
#include <system_error>
#include <vector>

#include <vulkan/vulkan.h>

namespace
{
    namespace bindings = vk::bindings;

    struct State
    {
        // Owned only when this file brought the device up itself; when the presenter
        // supplies one, teardown leaves it alone.
        bool ownsDevice = false;
        VkInstance instance = VK_NULL_HANDLE;
        VkPhysicalDevice physical = VK_NULL_HANDLE;
        VkDevice device = VK_NULL_HANDLE;
        VkQueue queue = VK_NULL_HANDLE;
        uint32_t family = 0;
        bool preciseQueries = false;
        float maxAnisotropy = 1.0f;
        bool nonSeamlessCubes = false;
        uint32_t multiDraw = 0;   // the most draws one vkCmdDrawMulti*EXT may make
        uint32_t samplers = 0;    // the most samplers the texture cache may make
        bool pipelineLibraries = false, fastLinking = false;
        bool creationFeedback = false;
        bool cacheLoaded = false;
        std::string name = "none";

        VkDescriptorSetLayout sets[3]{};
        VkPipelineLayout layout = VK_NULL_HANDLE;
        VkPipelineCache cache = VK_NULL_HANDLE;
        std::string lastError;
    };
    State g;

    const char* Explain(VkResult r)
    {
        switch (r)
        {
        case VK_SUCCESS: return "ok";
        case VK_ERROR_OUT_OF_HOST_MEMORY: return "out of host memory";
        case VK_ERROR_OUT_OF_DEVICE_MEMORY: return "out of device memory";
        case VK_ERROR_INVALID_SHADER_NV: return "driver rejected the shader";
        case VK_ERROR_INITIALIZATION_FAILED: return "initialisation failed";
        case VK_ERROR_EXTENSION_NOT_PRESENT: return "extension not present";
        case VK_ERROR_FEATURE_NOT_PRESENT: return "feature not present";
        case VK_ERROR_INCOMPATIBLE_DRIVER: return "incompatible driver";
        default: return "failed";
        }
    }

}

namespace { std::atomic<void (*)(const char*)> g_deviceLost{ nullptr }; }

void vk::pipeline::OnDeviceLost(void (*handler)(const char*)) { g_deviceLost = handler; }

bool vk::pipeline::Failed(int vkResult, const char* where)
{
    if (vkResult == VK_SUCCESS) return false;
    if (vkResult == VK_ERROR_DEVICE_LOST)
    {
        static std::atomic<bool> reported{ false };
        if (!reported.exchange(true))
        {
            LOGE("vulkan: the device was lost at %s -- the driver reset the GPU under this"
                 " run (after a GPU page fault or a hang; `journalctl -k | grep amdgpu` has"
                 " the fault), and nothing submitted from here on will finish. Ending the run.",
                 where);
            if (auto handler = g_deviceLost.load()) handler("the Vulkan device was lost");
        }
    }
    return true;
}

uint32_t vk::pipeline::GraphicsFamily(void* physical)
{
    VkPhysicalDevice device = static_cast<VkPhysicalDevice>(physical);
    uint32_t families = 0;
    vkGetPhysicalDeviceQueueFamilyProperties(device, &families, nullptr);
    std::vector<VkQueueFamilyProperties> props(families);
    vkGetPhysicalDeviceQueueFamilyProperties(device, &families, props.data());
    for (uint32_t i = 0; i < families; i++)
        if (props[i].queueFlags & VK_QUEUE_GRAPHICS_BIT) return i;
    return UINT32_MAX;
}

bool vk::pipeline::DeviceHasExtension(void* physical, const char* name)
{
    uint32_t count = 0;
    VkPhysicalDevice device = static_cast<VkPhysicalDevice>(physical);
    vkEnumerateDeviceExtensionProperties(device, nullptr, &count, nullptr);
    std::vector<VkExtensionProperties> offered(count);
    vkEnumerateDeviceExtensionProperties(device, nullptr, &count, offered.data());
    for (const auto& e : offered)
        if (std::strcmp(e.extensionName, name) == 0) return true;
    return false;
}

bool vk::pipeline::CreateDevice(void* physicalDevice, uint32_t family,
                                const char* const* extraExtensions, uint32_t extraCount,
                                void** deviceOut, void** queueOut, void* features)
{
    VkPhysicalDevice physical = static_cast<VkPhysicalDevice>(physicalDevice);
    const float priority = 1.0f;
    VkDeviceQueueCreateInfo queue{ VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO };
    queue.queueFamilyIndex = family;
    queue.queueCount = 1;
    queue.pQueuePriorities = &priority;

    VkPhysicalDeviceFeatures supported{};
    vkGetPhysicalDeviceFeatures(physical, &supported);
    VkPhysicalDeviceFeatures enabled{};
    // Robust buffer access, because the vertex fetch path reads the frame arena
    // at indices the guest's own data computes. One index past the end is,
    // without it, a GPU page fault: the driver retries it until the queue
    // stalls, and enough of them hung the whole machine. With it the read
    // returns zero or something in bounds, and the draw is merely wrong. Xenia
    // asks for the same thing for the same reason.
    enabled.robustBufferAccess = supported.robustBufferAccess;
    if (!supported.robustBufferAccess)
        LOGW("vulkan: the device offers no robust buffer access; an out-of-range vertex"
             " fetch will fault");
    // The title's occlusion queries are a sample count it compares against a
    // threshold, and without this a query is only allowed to answer "some".
    enabled.occlusionQueryPrecise = supported.occlusionQueryPrecise;
    g.preciseQueries = supported.occlusionQueryPrecise;
    // The title asks for anisotropic filtering on nearly every world texture
    // (2:1, once 4:1), and a sampler cannot give it unless the device enables it.
    enabled.samplerAnisotropy = supported.samplerAnisotropy;
    {
        VkPhysicalDeviceProperties properties{};
        vkGetPhysicalDeviceProperties(physical, &properties);
        if (supported.samplerAnisotropy) g.maxAnisotropy = properties.limits.maxSamplerAnisotropy;
        // NVIDIA allows 4000 samplers in all, fewer than the set's slots; the
        // backdrop makes one of its own.
        g.samplers = std::min<uint32_t>(bindings::kSamplerCount,
                                        properties.limits.maxSamplerAllocationCount - 16);
    }

    // The image format list lets a resolve destination carry an sRGB view beside
    // its own without the driver giving up compression; naming it in an image's
    // pNext without enabling it is invalid.
    std::vector<const char*> extensions(extraExtensions, extraExtensions + extraCount);
    if (DeviceHasExtension(physical, VK_KHR_IMAGE_FORMAT_LIST_EXTENSION_NAME))
        extensions.push_back(VK_KHR_IMAGE_FORMAT_LIST_EXTENSION_NAME);
    // The driver's word on whether a shader came out of the pipeline cache:
    // the bug report counts the ones the launcher's compile pass had not
    // put there.
    g.creationFeedback = DeviceHasExtension(physical, VK_EXT_PIPELINE_CREATION_FEEDBACK_EXTENSION_NAME);
    if (g.creationFeedback) extensions.push_back(VK_EXT_PIPELINE_CREATION_FEEDBACK_EXTENSION_NAME);

    // Direct3D 9 filters each cube face on its own, clamped at its edges; a
    // Vulkan cube blends across them unless told not to. Xenia asks for the
    // same thing.
    VkPhysicalDeviceNonSeamlessCubeMapFeaturesEXT nonSeamless{
        VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_NON_SEAMLESS_CUBE_MAP_FEATURES_EXT };
    if (DeviceHasExtension(physical, VK_EXT_NON_SEAMLESS_CUBE_MAP_EXTENSION_NAME))
    {
        VkPhysicalDeviceFeatures2 query{ VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2 };
        query.pNext = &nonSeamless;
        vkGetPhysicalDeviceFeatures2(physical, &query);
        query.pNext = nullptr;
        if (nonSeamless.nonSeamlessCubeMap)
            extensions.push_back(VK_EXT_NON_SEAMLESS_CUBE_MAP_EXTENSION_NAME);
    }
    g.nonSeamlessCubes = nonSeamless.nonSeamlessCubeMap;

    // Render passes give way to dynamic rendering, and cull mode, winding and
    // the depth and stencil tests become state set per draw: then a shader
    // compiled on its own, as a pipeline library, fits every target and every
    // draw it is used in, and can be compiled when the title loads it rather
    // than at the first draw -- which is what the console does, where a
    // shader is ready the moment it is loaded.
    VkPhysicalDeviceDynamicRenderingFeaturesKHR dynamicRendering{
        VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DYNAMIC_RENDERING_FEATURES_KHR };
    VkPhysicalDeviceExtendedDynamicStateFeaturesEXT dynamicState{
        VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTENDED_DYNAMIC_STATE_FEATURES_EXT };
    VkPhysicalDeviceGraphicsPipelineLibraryFeaturesEXT libraries{
        VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_GRAPHICS_PIPELINE_LIBRARY_FEATURES_EXT };
    {
        dynamicRendering.pNext = &dynamicState;
        dynamicState.pNext = &libraries;
        VkPhysicalDeviceFeatures2 query{ VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2 };
        query.pNext = &dynamicRendering;
        vkGetPhysicalDeviceFeatures2(physical, &query);
    }
    const bool hasDynamicRendering =
        DeviceHasExtension(physical, VK_KHR_DYNAMIC_RENDERING_EXTENSION_NAME) &&
        dynamicRendering.dynamicRendering;
    const bool hasDynamicState =
        DeviceHasExtension(physical, VK_EXT_EXTENDED_DYNAMIC_STATE_EXTENSION_NAME) &&
        dynamicState.extendedDynamicState;
    if (!hasDynamicRendering || !hasDynamicState)
    {
        LOGW("vulkan: the device lacks dynamic rendering or extended dynamic state, which the"
             " renderer needs");
        return false;
    }
    extensions.push_back(VK_KHR_DYNAMIC_RENDERING_EXTENSION_NAME);
    extensions.push_back(VK_EXT_EXTENDED_DYNAMIC_STATE_EXTENSION_NAME);

    // Every texture in one set that stays bound, written as textures are
    // made, and a draw's record found through its base vertex and handed to
    // the pixel stage (bindings.h). Vulkan 1.2 has all of it; every device
    // with the dynamic rendering above does.
    VkPhysicalDeviceDescriptorIndexingFeatures indexing{
        VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DESCRIPTOR_INDEXING_FEATURES };
    VkPhysicalDeviceShaderDrawParametersFeatures drawParameters{
        VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SHADER_DRAW_PARAMETERS_FEATURES };
    {
        VkPhysicalDeviceProperties properties{};
        vkGetPhysicalDeviceProperties(physical, &properties);
        const bool core = properties.apiVersion >= VK_API_VERSION_1_2;
        if (!core && DeviceHasExtension(physical, VK_EXT_DESCRIPTOR_INDEXING_EXTENSION_NAME))
            extensions.push_back(VK_EXT_DESCRIPTOR_INDEXING_EXTENSION_NAME);
        indexing.pNext = &drawParameters;
        VkPhysicalDeviceFeatures2 query{ VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2 };
        query.pNext = &indexing;
        vkGetPhysicalDeviceFeatures2(physical, &query);

        VkPhysicalDeviceDescriptorIndexingProperties limits{
            VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DESCRIPTOR_INDEXING_PROPERTIES };
        VkPhysicalDeviceSubgroupProperties subgroup{
            VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SUBGROUP_PROPERTIES };
        limits.pNext = &subgroup;
        VkPhysicalDeviceProperties2 properties2{ VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2 };
        properties2.pNext = &limits;
        vkGetPhysicalDeviceProperties2(physical, &properties2);

        constexpr uint32_t kImages = bindings::kImageCount * bindings::kImageKinds;
        const char* missing =
            properties.apiVersion < VK_API_VERSION_1_1 ? "Vulkan 1.1"
            : !indexing.descriptorBindingSampledImageUpdateAfterBind ||
              !indexing.descriptorBindingPartiallyBound ||
              !indexing.descriptorBindingUpdateUnusedWhilePending ? "descriptor indexing"
            : !supported.shaderSampledImageArrayDynamicIndexing ? "dynamically indexed textures"
            : !drawParameters.shaderDrawParameters ? "shader draw parameters"
            : !(subgroup.supportedStages & VK_SHADER_STAGE_FRAGMENT_BIT) ||
              !(subgroup.supportedOperations & VK_SUBGROUP_FEATURE_BALLOT_BIT) ? "subgroup ballot in pixel shaders"
            : limits.maxPerStageDescriptorUpdateAfterBindSampledImages < kImages ||
              limits.maxDescriptorSetUpdateAfterBindSampledImages < kImages ||
              limits.maxPerStageDescriptorUpdateAfterBindSamplers < bindings::kSamplerCount ||
              limits.maxDescriptorSetUpdateAfterBindSamplers < bindings::kSamplerCount ||
              limits.maxPerStageUpdateAfterBindResources < kImages + bindings::kSamplerCount
                ? "room for every texture in one set"
            : properties.limits.maxVertexOutputComponents < 4 * (bindings::kRecordLocation + 1) ||
              properties.limits.maxFragmentInputComponents < 4 * (bindings::kRecordLocation + 1)
                ? "seventeen interpolators"
            : nullptr;
        if (missing)
        {
            LOGW("vulkan: the device lacks %s, which the renderer needs", missing);
            return false;
        }
        enabled.shaderSampledImageArrayDynamicIndexing = VK_TRUE;
        indexing = VkPhysicalDeviceDescriptorIndexingFeatures{
            VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DESCRIPTOR_INDEXING_FEATURES };
        indexing.descriptorBindingSampledImageUpdateAfterBind = VK_TRUE;
        indexing.descriptorBindingPartiallyBound = VK_TRUE;
        indexing.descriptorBindingUpdateUnusedWhilePending = VK_TRUE;
        indexing.pNext = &drawParameters;
        drawParameters.shaderDrawParameters = VK_TRUE;
    }
    g.pipelineLibraries =
        DeviceHasExtension(physical, VK_KHR_PIPELINE_LIBRARY_EXTENSION_NAME) &&
        DeviceHasExtension(physical, VK_EXT_GRAPHICS_PIPELINE_LIBRARY_EXTENSION_NAME) &&
        libraries.graphicsPipelineLibrary;
    if (g.pipelineLibraries)
    {
        extensions.push_back(VK_KHR_PIPELINE_LIBRARY_EXTENSION_NAME);
        extensions.push_back(VK_EXT_GRAPHICS_PIPELINE_LIBRARY_EXTENSION_NAME);
        VkPhysicalDeviceGraphicsPipelineLibraryPropertiesEXT linking{
            VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_GRAPHICS_PIPELINE_LIBRARY_PROPERTIES_EXT };
        VkPhysicalDeviceProperties2 properties{ VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2 };
        properties.pNext = &linking;
        vkGetPhysicalDeviceProperties2(physical, &properties);
        g.fastLinking = linking.graphicsPipelineLibraryFastLinking;
    }
    else libraries.graphicsPipelineLibrary = VK_FALSE;
    dynamicRendering.pNext = &dynamicState;
    dynamicState.pNext = g.pipelineLibraries ? static_cast<void*>(&libraries) : features;
    libraries.pNext = features;
    // Draws in a row that change nothing between them are made with one call
    // (renderer_draw.cpp); without it each is made on its own.
    VkPhysicalDeviceMultiDrawFeaturesEXT multiDraw{
        VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MULTI_DRAW_FEATURES_EXT };
    g.multiDraw = 0;
    if (DeviceHasExtension(physical, VK_EXT_MULTI_DRAW_EXTENSION_NAME))
    {
        VkPhysicalDeviceFeatures2 query{ VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2 };
        query.pNext = &multiDraw;
        vkGetPhysicalDeviceFeatures2(physical, &query);
        VkPhysicalDeviceMultiDrawPropertiesEXT most{
            VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MULTI_DRAW_PROPERTIES_EXT };
        VkPhysicalDeviceProperties2 properties{ VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2 };
        properties.pNext = &most;
        vkGetPhysicalDeviceProperties2(physical, &properties);
        if (multiDraw.multiDraw && most.maxMultiDrawCount > 1)
        {
            extensions.push_back(VK_EXT_MULTI_DRAW_EXTENSION_NAME);
            g.multiDraw = most.maxMultiDrawCount;
        }
    }
    multiDraw = VkPhysicalDeviceMultiDrawFeaturesEXT{
        VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MULTI_DRAW_FEATURES_EXT };
    multiDraw.multiDraw = VK_TRUE;
    multiDraw.pNext = &dynamicRendering;
    drawParameters.pNext = g.multiDraw ? static_cast<void*>(&multiDraw) : &dynamicRendering;
    void* chain = &indexing;

    VkDeviceCreateInfo info{ VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO };
    info.pNext = chain;
    if (g.nonSeamlessCubes) { nonSeamless.pNext = chain; info.pNext = &nonSeamless; }
    info.queueCreateInfoCount = 1;
    info.pQueueCreateInfos = &queue;
    info.enabledExtensionCount = uint32_t(extensions.size());
    info.ppEnabledExtensionNames = extensions.data();
    info.pEnabledFeatures = &enabled;
    VkDevice device = VK_NULL_HANDLE;
    const VkResult r = vkCreateDevice(physical, &info, nullptr, &device);
    if (r != VK_SUCCESS) { LOGW("vulkan: no device (%s)", Explain(r)); return false; }
    VkQueue made = VK_NULL_HANDLE;
    vkGetDeviceQueue(device, family, 0, &made);
    *deviceOut = device;
    *queueOut = made;
    return true;
}

namespace
{
    std::string g_wantedDevice;

    // Enough of a device to compile shaders on. No surface, no swapchain: the
    // point is to reach the driver's compiler, which needs neither.
    bool BringUpHeadless()
    {
        VkApplicationInfo app{ VK_STRUCTURE_TYPE_APPLICATION_INFO };
        app.pApplicationName = "mw2recomp";
        // 1.1 for negative viewport heights, which is how the renderer expresses the
        // console's +Y-up clip space; 1.2 for the render pass depth resolve the
        // multisampled surfaces need.
        app.apiVersion = VK_API_VERSION_1_2;

        VkInstanceCreateInfo info{ VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO };
        info.pApplicationInfo = &app;
        VkResult r = vkCreateInstance(&info, nullptr, &g.instance);
        if (r != VK_SUCCESS) { LOGW("pipeline: no Vulkan instance (%s)", Explain(r)); return false; }

        uint32_t count = 0;
        vkEnumeratePhysicalDevices(g.instance, &count, nullptr);
        if (!count) { LOGW("pipeline: no Vulkan device"); return false; }
        std::vector<VkPhysicalDevice> devices(count);
        vkEnumeratePhysicalDevices(g.instance, &count, devices.data());

        uint32_t family = UINT32_MAX;
        g.physical = static_cast<VkPhysicalDevice>(vk::pipeline::ChooseDevice(g.instance, g_wantedDevice, &family));
        if (!g.physical)
        {
            if (g_wantedDevice.empty()) LOGW("pipeline: no graphics queue");
            else LOGW("pipeline: no device %s", g_wantedDevice.c_str());
            return false;
        }

        void* device = nullptr;
        void* queue = nullptr;
        if (!vk::pipeline::CreateDevice(g.physical, family, nullptr, 0, &device, &queue))
            return false;
        g.device = static_cast<VkDevice>(device);
        g.queue = static_cast<VkQueue>(queue);
        g.family = family;
        g.ownsDevice = true;
        return true;
    }

    bool CreateSetLayouts()
    {
        const VkShaderStageFlags both = VK_SHADER_STAGE_VERTEX_BIT | VK_SHADER_STAGE_FRAGMENT_BIT;

        // Every texture and sampler the cache holds (bindings.h). Written while
        // command buffers that bind the set are being recorded and run, each
        // entry before the first draw that names it and never while one that
        // names it is pending; most entries are empty.
        VkDescriptorSetLayoutBinding textures[4]{};
        VkDescriptorBindingFlags textureFlags[4]{};
        for (uint32_t i = 0; i < 4; i++)
        {
            textures[i].binding = i;
            textures[i].descriptorType = i == bindings::kSamplers ? VK_DESCRIPTOR_TYPE_SAMPLER
                                                                 : VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE;
            textures[i].descriptorCount = i == bindings::kSamplers ? bindings::kSamplerCount
                                                                  : bindings::kImageCount;
            textures[i].stageFlags = both;
            textureFlags[i] = VK_DESCRIPTOR_BINDING_UPDATE_AFTER_BIND_BIT |
                              VK_DESCRIPTOR_BINDING_UPDATE_UNUSED_WHILE_PENDING_BIT |
                              VK_DESCRIPTOR_BINDING_PARTIALLY_BOUND_BIT;
        }
        VkDescriptorSetLayoutBindingFlagsCreateInfo textureBindingFlags{
            VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_BINDING_FLAGS_CREATE_INFO };
        textureBindingFlags.bindingCount = 4;
        textureBindingFlags.pBindingFlags = textureFlags;

        VkDescriptorSetLayoutBinding memory[2]{};
        for (uint32_t i = 0; i < 2; i++)
        {
            memory[i].binding = i ? bindings::kArenaFloats : bindings::kArenaBinding;
            memory[i].descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER;
            memory[i].descriptorCount = 1;
            memory[i].stageFlags = both;
        }

        struct Set { const VkDescriptorSetLayoutBinding* bindings; uint32_t count; };
        Set sets[3];
        sets[bindings::kConstantSet] = { nullptr, 0 };    // the constants are in the arena
        sets[bindings::kTextureSet] = { textures, 4 };
        sets[bindings::kArenaSet] = { memory, 2 };
        for (uint32_t i = 0; i < 3; i++)
        {
            VkDescriptorSetLayoutCreateInfo info{ VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO };
            info.bindingCount = sets[i].count;
            info.pBindings = sets[i].bindings;
            if (i == bindings::kTextureSet)
            {
                info.flags = VK_DESCRIPTOR_SET_LAYOUT_CREATE_UPDATE_AFTER_BIND_POOL_BIT;
                info.pNext = &textureBindingFlags;
            }
            VkResult r = vkCreateDescriptorSetLayout(g.device, &info, nullptr, &g.sets[i]);
            if (r != VK_SUCCESS)
            {
                LOGW("pipeline: descriptor set layout %u failed (%s)", i, Explain(r));
                return false;
            }
        }

        // No push constants: the register state a draw needs is in its record
        // (bindings.h).
        VkPipelineLayoutCreateInfo info{ VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO };
        info.setLayoutCount = 3;
        info.pSetLayouts = g.sets;
        VkResult r = vkCreatePipelineLayout(g.device, &info, nullptr, &g.layout);
        if (r != VK_SUCCESS) { LOGW("pipeline: layout failed (%s)", Explain(r)); return false; }
        return true;
    }

    // MW2_PIPELINE_CACHE=<file> in place of the one the game always keeps.
    // Without a file the cache still works, just only within one run.
    std::string g_cachePath;
    const char* CachePath()
    {
        if (const char* asked = env::Text("MW2_PIPELINE_CACHE")) return asked;
        return g_cachePath.empty() ? nullptr : g_cachePath.c_str();
    }

    void CreateCache()
    {
        std::vector<uint8_t> initial;
        if (const char* path = CachePath())
        {
            if (std::FILE* f = std::fopen(path, "rb"))
            {
                std::fseek(f, 0, SEEK_END);
                long size = std::ftell(f);
                std::fseek(f, 0, SEEK_SET);
                if (size > 0)
                {
                    initial.resize(size_t(size));
                    if (std::fread(initial.data(), 1, initial.size(), f) != initial.size())
                        initial.clear();
                }
                std::fclose(f);
            }
        }
        VkPipelineCacheCreateInfo info{ VK_STRUCTURE_TYPE_PIPELINE_CACHE_CREATE_INFO };
        info.initialDataSize = initial.size();
        info.pInitialData = initial.empty() ? nullptr : initial.data();
        // A cache the driver rejects is not an error: it starts empty instead.
        if (vkCreatePipelineCache(g.device, &info, nullptr, &g.cache) != VK_SUCCESS)
        {
            info.initialDataSize = 0;
            info.pInitialData = nullptr;
            vkCreatePipelineCache(g.device, &info, nullptr, &g.cache);
        }
        else if (!initial.empty())
        {
            g.cacheLoaded = true;
            LOGI("pipeline: loaded %zu KB of pipeline cache", initial.size() / 1024);
        }
    }

    void SaveCache()
    {
        const char* path = CachePath();
        if (!path || !g.cache) return;
        size_t size = 0;
        if (vkGetPipelineCacheData(g.device, g.cache, &size, nullptr) != VK_SUCCESS || !size) return;
        std::vector<uint8_t> data(size);
        if (vkGetPipelineCacheData(g.device, g.cache, &size, data.data()) != VK_SUCCESS) return;
        // Beside it first, then in its place: a run that ends halfway through
        // the write leaves the cache it had, not a broken one.
        const std::string partial = std::string(path) + ".partial";
        std::FILE* f = std::fopen(partial.c_str(), "wb");
        if (!f) return;
        const bool written = std::fwrite(data.data(), 1, size, f) == size;
        if (std::fclose(f) != 0 || !written) return;
        std::error_code ec;
        std::filesystem::rename(partial, path, ec);
        if (ec) LOGW("pipeline: the pipeline cache could not be put in place at %s (%s)", path, ec.message().c_str());
        else LOGI("pipeline: wrote %zu KB of pipeline cache to %s", size / 1024, path);
    }

    VkShaderModule MakeModule(const uint32_t* spirv, size_t words, VkResult& result)
    {
        VkShaderModuleCreateInfo info{ VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO };
        info.codeSize = words * 4;
        info.pCode = spirv;
        VkShaderModule module = VK_NULL_HANDLE;
        result = vkCreateShaderModule(g.device, &info, nullptr, &module);
        return module;
    }
}

void vk::pipeline::SetCachePath(const char* path) { g_cachePath = path ? path : ""; }
bool vk::pipeline::CacheLoaded() { return g.cacheLoaded; }
void vk::pipeline::SetDevice(const std::string& id) { g_wantedDevice = id; }
bool vk::pipeline::CreationFeedback() { return g.creationFeedback; }

std::string vk::pipeline::DeviceId(void* physical)
{
    VkPhysicalDeviceIDProperties id{ VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_ID_PROPERTIES };
    VkPhysicalDeviceProperties2 properties{ VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2 };
    properties.pNext = &id;
    vkGetPhysicalDeviceProperties2(static_cast<VkPhysicalDevice>(physical), &properties);
    std::string text;
    for (uint8_t byte : id.deviceUUID)
    {
        char hex[3];
        std::snprintf(hex, sizeof hex, "%02x", byte);
        text += hex;
    }
    return text;
}

void* vk::pipeline::ChooseDevice(void* instance, const std::string& id, uint32_t* familyOut)
{
    uint32_t count = 0;
    vkEnumeratePhysicalDevices(static_cast<VkInstance>(instance), &count, nullptr);
    std::vector<VkPhysicalDevice> devices(count);
    vkEnumeratePhysicalDevices(static_cast<VkInstance>(instance), &count, devices.data());
    // The device the game's window would draw on (presenter.cpp): the first
    // discrete one, else the first of any kind, with a graphics queue -- or
    // the one asked for, which the game names when it draws on another.
    for (int pass = 0; pass < 2; pass++)
        for (VkPhysicalDevice candidate : devices)
        {
            VkPhysicalDeviceProperties p{};
            vkGetPhysicalDeviceProperties(candidate, &p);
            if (!id.empty() ? DeviceId(candidate) != id : pass == 0 && p.deviceType != VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU)
                continue;
            const uint32_t graphics = GraphicsFamily(candidate);
            if (graphics == UINT32_MAX) continue;
            if (familyOut) *familyOut = graphics;
            return candidate;
        }
    return nullptr;
}

bool vk::pipeline::Initialise(void* device, void* physical, void* queue, uint32_t queueFamily,
                              void* instance)
{
    if (g.device) return true;

    if (device && physical)
    {
        g.instance = static_cast<VkInstance>(instance);
        g.device = static_cast<VkDevice>(device);
        g.physical = static_cast<VkPhysicalDevice>(physical);
        g.queue = static_cast<VkQueue>(queue);
        g.family = queueFamily;
        g.ownsDevice = false;
    }
    else if (!BringUpHeadless())
    {
        return false;
    }

    VkPhysicalDeviceProperties props{};
    vkGetPhysicalDeviceProperties(g.physical, &props);
    g.name = props.deviceName;
    // A bug report is read against the driver's known faults.
    if (report::On())
    {
        VkPhysicalDeviceDriverProperties driver{ VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES };
        if (props.apiVersion >= VK_API_VERSION_1_2)
        {
            VkPhysicalDeviceProperties2 more{ VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2 };
            more.pNext = &driver;
            vkGetPhysicalDeviceProperties2(g.physical, &more);
        }
        LOGI("report: graphics %s (%04X:%04X), driver %s %s (%08X), Vulkan %u.%u.%u", props.deviceName,
             props.vendorID, props.deviceID, driver.driverName, driver.driverInfo, props.driverVersion,
             VK_API_VERSION_MAJOR(props.apiVersion), VK_API_VERSION_MINOR(props.apiVersion),
             VK_API_VERSION_PATCH(props.apiVersion));
    }

    if (!CreateSetLayouts()) { Shutdown(); return false; }
    CreateCache();
    LOGI("pipeline: layout ready on %s", g.name.c_str());
    return true;
}

void vk::pipeline::Shutdown()
{
    if (!g.device) return;
    SaveCache();
    if (g.cache) vkDestroyPipelineCache(g.device, g.cache, nullptr);
    if (g.layout) vkDestroyPipelineLayout(g.device, g.layout, nullptr);
    for (VkDescriptorSetLayout& set : g.sets)
        if (set) { vkDestroyDescriptorSetLayout(g.device, set, nullptr); set = VK_NULL_HANDLE; }
    g.cache = VK_NULL_HANDLE;
    g.layout = VK_NULL_HANDLE;
    if (g.ownsDevice)
    {
        vkDestroyDevice(g.device, nullptr);
        if (g.instance) vkDestroyInstance(g.instance, nullptr);
        g.instance = VK_NULL_HANDLE;
    }
    g.device = VK_NULL_HANDLE;
    g.physical = VK_NULL_HANDLE;
    g.queue = VK_NULL_HANDLE;
    g.cacheLoaded = false;
}

bool vk::pipeline::Ready() { return g.device != VK_NULL_HANDLE; }
const char* vk::pipeline::DeviceName() { return g.name.c_str(); }
void*    vk::pipeline::Instance() { return g.instance; }
void*    vk::pipeline::Device() { return g.device; }
void*    vk::pipeline::PhysicalDevice() { return g.physical; }
void*    vk::pipeline::Queue() { return g.queue; }
uint32_t vk::pipeline::QueueFamily() { return g.family; }
bool     vk::pipeline::PreciseOcclusionQueries() { return g.preciseQueries; }
float    vk::pipeline::MaxAnisotropy() { return g.maxAnisotropy; }
bool     vk::pipeline::NonSeamlessCubes() { return g.nonSeamlessCubes; }
uint32_t vk::pipeline::MultiDraw() { return g.multiDraw; }
uint32_t vk::pipeline::Samplers() { return g.samplers; }
// MW2_NO_PIPELINE_LIBRARIES=1 builds whole pipelines at the first draw instead.
bool     vk::pipeline::PipelineLibraries()
{
    static const bool refused = env::Flag("MW2_NO_PIPELINE_LIBRARIES");
    return g.pipelineLibraries && g.fastLinking && !refused;
}
std::mutex& vk::pipeline::QueueMutex() { static std::mutex m; return m; }
void*    vk::pipeline::SetLayout(uint32_t set) { return set < 3 ? g.sets[set] : nullptr; }
void*    vk::pipeline::Layout() { return g.layout; }
void*    vk::pipeline::Cache() { return g.cache; }

bool vk::pipeline::CreateModule(const uint32_t* spirv, size_t words, const char** error)
{
    if (!g.device) { if (error) *error = "no device"; return false; }
    VkResult r;
    VkShaderModule module = MakeModule(spirv, words, r);
    if (r != VK_SUCCESS)
    {
        g.lastError = Explain(r);
        if (error) *error = g.lastError.c_str();
        return false;
    }
    vkDestroyShaderModule(g.device, module, nullptr);
    return true;
}

#endif  // MW2_HAVE_VULKAN
