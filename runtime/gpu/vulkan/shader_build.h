#pragma once
// One shader made ready for the GPU: translated, made a module and, where the
// device has pipeline libraries, compiled on its own as its stage's library.
//
// The game does this as the title loads its shaders (renderer_libraries.cpp),
// and the launcher does it for every shader of the game before the game first
// runs (launcher/precompile.cpp), into the pipeline cache the game then
// starts with. A driver finds a library in that cache only when it is asked
// for exactly the same thing, so both build it here, and everything the
// result depends on -- the depth format, the scale, the pipeline layout --
// is decided here as well.
#include "shader_translator.h"

namespace vk::shaders
{
    // Where a player's copy keeps the pipeline cache the launcher fills,
    // beside game/ and saves/.
    inline constexpr const char* kCacheFile = "shaders/pipelines.bin";

    // What this build's shaders come out as: a SHA-256 CMake takes of the
    // sources that decide it -- the translator, this file, the pipeline
    // layout, the launcher's reader of the game's shaders. Another build's
    // cache holds other shaders.
    extern const char kBuildId[];
}

#ifdef MW2_HAVE_VULKAN
#include <vulkan/vulkan.h>

namespace vk::shaders
{
    // The depth and stencil format the renderer draws with on this device:
    // D24S8 where there is one (AMD has none), else D32S8. Undefined if neither.
    VkFormat DepthFormat(VkPhysicalDevice physical);
    // MW2_SCALE as the device can draw it: a surface is at most 4096 of the
    // title's pixels either way, and must fit one of the device's images.
    uint32_t Scale(VkPhysicalDevice physical, uint32_t asked);

    // The dynamic state every pipeline here has, and the rasterisation state
    // every one starts from.
    extern const VkDynamicState kDynamicStates[];
    extern const uint32_t kDynamicStateCount;
    VkPipelineRasterizationStateCreateInfo Rasterisation();

    // A library of one part of a pipeline. `stage` is null for the parts that
    // hold only state. `next` goes on the library's create info chain.
    VkPipeline MakeLibrary(VkGraphicsPipelineLibraryFlagsEXT part, const VkPipelineShaderStageCreateInfo* stage,
                           VkGraphicsPipelineCreateInfo info, void* next = nullptr);

    struct Built
    {
        shader::Translation translation;
        VkShaderModule module = VK_NULL_HANDLE;   // null when it did not translate, or the driver refused it
        VkPipeline library = VK_NULL_HANDLE;      // null on a device without pipeline libraries
        // Whether the driver found the library in the pipeline cache: 1 it
        // did, 0 it compiled it, -1 it does not say (or there is no library).
        int fromCache = -1;
    };
    // Any thread: the device, layout and cache are vk::pipeline's.
    Built Build(shader::Type type, const uint32_t* code, size_t words, uint32_t scale, VkFormat depthFormat);
    void Destroy(Built& built);
}
#endif
