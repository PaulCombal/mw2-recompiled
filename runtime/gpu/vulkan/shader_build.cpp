#include "shader_build.h"
#include "pipeline.h"

#ifndef MW2_SHADER_BUILD
#error "CMake defines MW2_SHADER_BUILD for this file"
#endif
const char vk::shaders::kBuildId[] = MW2_SHADER_BUILD;

#ifdef MW2_HAVE_VULKAN

#include <algorithm>
#include <iterator>

namespace
{
    constexpr VkPipelineCreateFlags kLibrary =
        VK_PIPELINE_CREATE_LIBRARY_BIT_KHR | VK_PIPELINE_CREATE_RETAIN_LINK_TIME_OPTIMIZATION_INFO_BIT_EXT;

    VkDevice Device() { return static_cast<VkDevice>(vk::pipeline::Device()); }
}

VkFormat vk::shaders::DepthFormat(VkPhysicalDevice physical)
{
    for (VkFormat candidate : { VK_FORMAT_D24_UNORM_S8_UINT, VK_FORMAT_D32_SFLOAT_S8_UINT })
    {
        VkFormatProperties format{};
        vkGetPhysicalDeviceFormatProperties(physical, candidate, &format);
        if (format.optimalTilingFeatures & VK_FORMAT_FEATURE_DEPTH_STENCIL_ATTACHMENT_BIT) return candidate;
    }
    return VK_FORMAT_UNDEFINED;
}

uint32_t vk::shaders::Scale(VkPhysicalDevice physical, uint32_t asked)
{
    VkPhysicalDeviceProperties properties{};
    vkGetPhysicalDeviceProperties(physical, &properties);
    uint32_t scale = std::clamp(asked, 1u, 3u);
    while (scale > 1 && 4096 * scale > properties.limits.maxImageDimension2D) scale--;
    return scale;
}

const VkDynamicState vk::shaders::kDynamicStates[] = {
    VK_DYNAMIC_STATE_VIEWPORT, VK_DYNAMIC_STATE_SCISSOR, VK_DYNAMIC_STATE_DEPTH_BIAS,
    VK_DYNAMIC_STATE_STENCIL_COMPARE_MASK, VK_DYNAMIC_STATE_STENCIL_WRITE_MASK,
    VK_DYNAMIC_STATE_STENCIL_REFERENCE, VK_DYNAMIC_STATE_CULL_MODE_EXT,
    VK_DYNAMIC_STATE_FRONT_FACE_EXT, VK_DYNAMIC_STATE_DEPTH_TEST_ENABLE_EXT,
    VK_DYNAMIC_STATE_DEPTH_WRITE_ENABLE_EXT, VK_DYNAMIC_STATE_DEPTH_COMPARE_OP_EXT,
    VK_DYNAMIC_STATE_STENCIL_TEST_ENABLE_EXT, VK_DYNAMIC_STATE_STENCIL_OP_EXT };
const uint32_t vk::shaders::kDynamicStateCount = uint32_t(std::size(vk::shaders::kDynamicStates));

VkPipelineRasterizationStateCreateInfo vk::shaders::Rasterisation()
{
    VkPipelineRasterizationStateCreateInfo raster{ VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO };
    raster.polygonMode = VK_POLYGON_MODE_FILL;
    // Culling and winding are set per draw (FacesFor).
    raster.lineWidth = 1.0f;
    // Always on, and set per draw: a shadow map drawn without the title's
    // bias shadows the very surfaces it was rendered from.
    raster.depthBiasEnable = VK_TRUE;
    return raster;
}

VkPipeline vk::shaders::MakeLibrary(VkGraphicsPipelineLibraryFlagsEXT part, const VkPipelineShaderStageCreateInfo* stage,
                                    VkGraphicsPipelineCreateInfo info, void* next)
{
    VkGraphicsPipelineLibraryCreateInfoEXT library{ VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_LIBRARY_CREATE_INFO_EXT };
    library.pNext = next;
    library.flags = part;
    info.sType = VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO;
    info.pNext = &library;
    info.flags = kLibrary;
    info.stageCount = stage ? 1 : 0;
    info.pStages = stage;
    info.layout = static_cast<VkPipelineLayout>(vk::pipeline::Layout());
    VkPipeline made = VK_NULL_HANDLE;
    if (vkCreateGraphicsPipelines(Device(), static_cast<VkPipelineCache>(vk::pipeline::Cache()), 1, &info, nullptr,
                                  &made) != VK_SUCCESS)
        return VK_NULL_HANDLE;
    return made;
}

namespace
{
    VkPipeline StageLibrary(shader::Type type, VkShaderModule module, VkFormat depthFormat, int& fromCache)
    {
        // Without a render pass, the shader stages learn from their own
        // rendering info whether there is a depth and stencil attachment:
        // one format for every target here, so the libraries still fit all.
        // A fragment library must carry it: without it, the library legally
        // ignores depth and stencil state, and every draw comes out empty.
        VkPipelineRenderingCreateInfoKHR rendering{ VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO_KHR };
        rendering.depthAttachmentFormat = depthFormat;
        rendering.stencilAttachmentFormat = depthFormat;
        VkPipelineCreationFeedbackEXT feedback{};
        VkPipelineCreationFeedbackCreateInfoEXT feedbackInfo{ VK_STRUCTURE_TYPE_PIPELINE_CREATION_FEEDBACK_CREATE_INFO_EXT };
        feedbackInfo.pPipelineCreationFeedback = &feedback;
        if (vk::pipeline::CreationFeedback()) rendering.pNext = &feedbackInfo;
        VkPipelineDynamicStateCreateInfo dynamic{ VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO };
        dynamic.dynamicStateCount = vk::shaders::kDynamicStateCount;
        dynamic.pDynamicStates = vk::shaders::kDynamicStates;

        VkPipelineShaderStageCreateInfo stage{ VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO };
        stage.module = module;
        stage.pName = "main";
        VkGraphicsPipelineCreateInfo info{};
        info.pDynamicState = &dynamic;
        VkPipeline made;
        const VkPipelineDepthStencilStateCreateInfo depth{ VK_STRUCTURE_TYPE_PIPELINE_DEPTH_STENCIL_STATE_CREATE_INFO };
        if (type == shader::Type::Vertex)
        {
            VkPipelineViewportStateCreateInfo viewport{ VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO };
            viewport.viewportCount = 1;
            viewport.scissorCount = 1;
            const VkPipelineRasterizationStateCreateInfo raster = vk::shaders::Rasterisation();
            stage.stage = VK_SHADER_STAGE_VERTEX_BIT;
            info.pViewportState = &viewport;
            info.pRasterizationState = &raster;
            made = vk::shaders::MakeLibrary(VK_GRAPHICS_PIPELINE_LIBRARY_PRE_RASTERIZATION_SHADERS_BIT_EXT, &stage, info,
                                            &rendering);
        }
        else
        {
            // The tests themselves are set per draw (DepthTestsFor).
            stage.stage = VK_SHADER_STAGE_FRAGMENT_BIT;
            info.pDepthStencilState = &depth;
            made = vk::shaders::MakeLibrary(VK_GRAPHICS_PIPELINE_LIBRARY_FRAGMENT_SHADER_BIT_EXT, &stage, info, &rendering);
        }
        if (feedback.flags & VK_PIPELINE_CREATION_FEEDBACK_VALID_BIT_EXT)
            fromCache = (feedback.flags & VK_PIPELINE_CREATION_FEEDBACK_APPLICATION_PIPELINE_CACHE_HIT_BIT_EXT) ? 1 : 0;
        return made;
    }
}

vk::shaders::Built vk::shaders::Build(shader::Type type, const uint32_t* code, size_t words, uint32_t scale,
                                      VkFormat depthFormat)
{
    Built built;
    built.translation = shader::Translate(type, code, words, scale);
    if (!built.translation.ok) return built;
    VkShaderModuleCreateInfo info{ VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO };
    info.codeSize = built.translation.spirv.size() * 4;
    info.pCode = built.translation.spirv.data();
    if (vkCreateShaderModule(Device(), &info, nullptr, &built.module) != VK_SUCCESS)
    {
        built.module = VK_NULL_HANDLE;
        return built;
    }
    if (vk::pipeline::PipelineLibraries())
    {
        built.library = StageLibrary(type, built.module, depthFormat, built.fromCache);
        if (!built.library) built.fromCache = -1;
    }
    return built;
}

void vk::shaders::Destroy(Built& built)
{
    if (built.library) vkDestroyPipeline(Device(), built.library, nullptr);
    if (built.module) vkDestroyShaderModule(Device(), built.module, nullptr);
    built.library = VK_NULL_HANDLE;
    built.module = VK_NULL_HANDLE;
}

#endif
