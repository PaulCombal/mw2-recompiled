#include "backdrop.h"

#ifdef MW2_HAVE_VULKAN
#include "spirv.h"
#include "util.h"
#include "../../log.h"

#include <algorithm>
#include <cstdlib>
#include <iterator>
#include <vector>

namespace
{
    using spirv::Op;
    using spirv::Id;
    using spirv::StorageClass;
    using spirv::Decoration;
    using spirv::Glsl;

    constexpr uint32_t kExecutionModelVertex = 0, kExecutionModelFragment = 4;
    constexpr uint32_t kExecutionModeOriginUpperLeft = 7;
    constexpr uint32_t kImageOperandsLod = 0x2;
    constexpr uint32_t kDim2D = 1;
    // At least the presenter's frames in flight: each has its own descriptor
    // set and view of the image it draws into.
    constexpr uint32_t kSlots = 4;
    // The blur: the frame at half size, then halved again three times, and
    // read at the smallest.
    constexpr uint32_t kLevels = 4;

    // The fragment shader's push constants, four vectors.
    struct Push
    {
        float frame[4];     // where the game's frame is in the window: from x, y, to x, y
        float picture[4];   // where the picture is, as drawn now
        float params[4];    // shown, corner radius, outline, 0
        float view[4];      // the window's size, the blur's texel in the frame's coordinates
    };

    // A function body under construction: each instruction with a result is
    // (type, id, operands).
    struct Code
    {
        spirv::Module m;

        Id Op_(Op op, Id type, std::vector<uint32_t> operands)
        {
            const Id result = m.Allocate();
            operands.insert(operands.begin(), { type, result });
            m.Code().push_back(spirv::Head(op, uint32_t(operands.size() + 1)));
            m.Code().insert(m.Code().end(), operands.begin(), operands.end());
            return result;
        }
        Id Ext(Glsl function, Id type, std::vector<uint32_t> operands)
        {
            operands.insert(operands.begin(), { m.GlslSet(), uint32_t(function) });
            return Op_(Op::ExtInst, type, std::move(operands));
        }
        Id Variable(StorageClass storage, Id type)
        {
            const Id pointer = m.Pointer(storage, type);
            const Id variable = m.Allocate();
            spirv::Module::Emit(m.Declarations(), Op::Variable, { pointer, variable, uint32_t(storage) });
            return variable;
        }
        void Decorate(Id target, Decoration decoration, uint32_t value)
        {
            spirv::Module::Emit(m.Decorations(), Op::Decorate, { target, uint32_t(decoration), value });
        }
        // OpFunction for `main`, after its entry point is declared.
        Id Main(uint32_t model, const std::vector<Id>& interface)
        {
            const Id function = m.Allocate();
            spirv::Module::Emit(m.Declarations(), Op::TypeFunction, { function, m.Void() });
            const Id main = m.Allocate();
            m.EntryPoint(model, main, "main", interface);
            spirv::Module::Emit(m.Code(), Op::Function, { m.Void(), main, 0, function });
            spirv::Module::Emit(m.Code(), Op::Label, { m.Allocate() });
            return main;
        }
        std::vector<uint32_t> End()
        {
            spirv::Module::Emit(m.Code(), Op::Return, {});
            spirv::Module::Emit(m.Code(), Op::FunctionEnd, {});
            return m.Finish();
        }
    };

    // A triangle that covers the window: vertices (-1,-1), (3,-1), (-1,3).
    std::vector<uint32_t> VertexShader()
    {
        Code c;
        spirv::Module& m = c.m;
        const Id i32 = m.Int(), f32 = m.Float(), f4 = m.Float4();
        const Id index = c.Variable(StorageClass::Input, i32);
        c.Decorate(index, Decoration::BuiltIn, uint32_t(spirv::BuiltIn::VertexIndex));
        const Id position = c.Variable(StorageClass::Output, f4);
        c.Decorate(position, Decoration::BuiltIn, uint32_t(spirv::BuiltIn::Position));
        c.Main(kExecutionModelVertex, { index, position });

        const Id i = c.Op_(Op::Load, i32, { index });
        const Id doubled = c.Op_(Op::ShiftLeftLogical, i32, { i, m.ConstantS(1) });
        const Id x = c.Op_(Op::ConvertSToF, f32, { c.Op_(Op::BitwiseAnd, i32, { doubled, m.ConstantS(2) }) });
        const Id y = c.Op_(Op::ConvertSToF, f32, { c.Op_(Op::BitwiseAnd, i32, { i, m.ConstantS(2) }) });
        auto clip = [&](Id v) {
            return c.Op_(Op::FSub, f32, { c.Op_(Op::FMul, f32, { v, m.ConstantF(2) }), m.ConstantF(1) });
        };
        const Id at = c.Op_(Op::CompositeConstruct, f4, { clip(x), clip(y), m.ConstantF(0), m.ConstantF(1) });
        spirv::Module::Emit(m.Code(), Op::Store, { position, at });
        return c.End();
    }

    // Per pixel p:
    //   backdrop = blur(frame at p) * 0.42 * vignette, black outside the frame
    //   shadow   = the picture's rounded rectangle, lowered and softened
    //   colour   = mix(backdrop * (1 - shadow), picture (outline at its edge), inside)
    // and `shown` as alpha, blended over the frame already in the window.
    std::vector<uint32_t> FragmentShader()
    {
        Code c;
        spirv::Module& m = c.m;
        const Id f32 = m.Float(), f2 = m.Float2(), f3 = m.Float3(), f4 = m.Float4();

        const Id fragCoord = c.Variable(StorageClass::Input, f4);
        c.Decorate(fragCoord, Decoration::BuiltIn, uint32_t(spirv::BuiltIn::FragCoord));
        const Id colour = c.Variable(StorageClass::Output, f4);
        c.Decorate(colour, Decoration::Location, 0);

        const Id sampled = m.SampledImage(m.Image(f32, kDim2D));
        const Id blur = c.Variable(StorageClass::UniformConstant, sampled);
        const Id picture = c.Variable(StorageClass::UniformConstant, sampled);
        for (auto [variable, binding] : { std::pair{ blur, 0u }, std::pair{ picture, 1u } })
        {
            c.Decorate(variable, Decoration::DescriptorSet, 0);
            c.Decorate(variable, Decoration::Binding, binding);
        }

        const Id block = m.Allocate();
        spirv::Module::Emit(m.Declarations(), Op::TypeStruct, { block, f4, f4, f4, f4 });
        spirv::Module::Emit(m.Decorations(), Op::Decorate, { block, uint32_t(Decoration::Block) });
        for (uint32_t member = 0; member < 4; member++)
            spirv::Module::Emit(m.Decorations(), Op::MemberDecorate,
                                { block, member, uint32_t(Decoration::Offset), member * 16 });
        const Id push = c.Variable(StorageClass::PushConstant, block);

        const Id main = c.Main(kExecutionModelFragment, { fragCoord, colour });
        spirv::Module::Emit(m.ExecutionModes(), Op::ExecutionMode, { main, kExecutionModeOriginUpperLeft });

        auto f = [&](float value) { return m.ConstantF(value); };
        auto member = [&](int32_t index) {
            const Id at = c.Op_(Op::AccessChain, m.Pointer(StorageClass::PushConstant, f4), { push, m.ConstantS(index) });
            return c.Op_(Op::Load, f4, { at });
        };
        auto pair = [&](Id v, uint32_t a, uint32_t b) { return c.Op_(Op::VectorShuffle, f2, { v, v, a, b }); };
        auto part = [&](Id v, uint32_t i) { return c.Op_(Op::CompositeExtract, f32, { v, i }); };
        auto two = [&](Id a, Id b) { return c.Op_(Op::CompositeConstruct, f2, { a, b }); };
        auto three = [&](Id a) { return c.Op_(Op::CompositeConstruct, f3, { a, a, a }); };
        auto add = [&](Id type, Id a, Id b) { return c.Op_(Op::FAdd, type, { a, b }); };
        auto sub = [&](Id type, Id a, Id b) { return c.Op_(Op::FSub, type, { a, b }); };
        auto mul = [&](Id type, Id a, Id b) { return c.Op_(Op::FMul, type, { a, b }); };
        auto scale = [&](Id type, Id v, Id s) { return c.Op_(Op::VectorTimesScalar, type, { v, s }); };
        auto sample = [&](Id variable, Id uv, Id lod) {
            const Id image = c.Op_(Op::Load, sampled, { variable });
            return c.Op_(Op::ImageSampleExplicitLod, f4, { image, uv, kImageOperandsLod, lod });
        };
        auto rgb = [&](Id v) { return c.Op_(Op::VectorShuffle, f3, { v, v, 0, 1, 2 }); };

        const Id p = pair(c.Op_(Op::Load, f4, { fragCoord }), 0, 1);
        const Id frame = member(0), place = member(1), params = member(2), view = member(3);

        // The backdrop.
        const Id frameFrom = pair(frame, 0, 1), frameTo = pair(frame, 2, 3);
        const Id uv = c.Op_(Op::FDiv, f2, { sub(f2, p, frameFrom), sub(f2, frameTo, frameFrom) });
        const Id texel = pair(view, 2, 3);
        const Id lod = f(float(kLevels - 1));
        Id sum = 0;
        for (int dy = -1; dy <= 1; dy++)
            for (int dx = -1; dx <= 1; dx++)
            {
                const float weight = float((2 - std::abs(dx)) * (2 - std::abs(dy))) / 16.0f;
                const Id at = add(f2, uv, mul(f2, two(f(float(dx)), f(float(dy))), texel));
                const Id tap = scale(f4, sample(blur, at, lod), f(weight));
                sum = sum ? add(f4, sum, tap) : tap;
            }
        const Id u = part(uv, 0), v = part(uv, 1);
        auto step = [&](Id edge, Id x) { return c.Ext(Glsl::Step, f32, { edge, x }); };
        const Id inFrame = mul(f32, mul(f32, step(f(0), u), step(u, f(1))), mul(f32, step(f(0), v), step(v, f(1))));
        const Id centred = sub(f2, c.Op_(Op::FDiv, f2, { p, pair(view, 0, 1) }), two(f(0.5f), f(0.5f)));
        const Id vignette = sub(f32, f(1), mul(f32, c.Op_(Op::Dot, f32, { centred, centred }), f(0.6f)));
        const Id backdrop = scale(f3, rgb(sum), mul(f32, mul(f32, inFrame, vignette), f(0.42f)));

        // The picture's rounded rectangle, as a signed distance: negative inside.
        const Id from = pair(place, 0, 1), to = pair(place, 2, 3);
        const Id centre = scale(f2, add(f2, from, to), f(0.5f));
        const Id half = scale(f2, sub(f2, to, from), f(0.5f));
        const Id radius = part(params, 1), outline = part(params, 2), shown = part(params, 0);
        auto distance = [&](Id offset) {
            const Id local = c.Ext(Glsl::FAbs, f2, { sub(f2, sub(f2, p, centre), offset) });
            const Id q = add(f2, sub(f2, local, half), two(radius, radius));
            const Id outside = c.Ext(Glsl::Length, f32, { c.Ext(Glsl::FMax, f2, { q, two(f(0), f(0)) }) });
            const Id within = c.Ext(Glsl::FMin, f32, { c.Ext(Glsl::FMax, f32, { part(q, 0), part(q, 1) }), f(0) });
            return sub(f32, add(f32, outside, within), radius);
        };
        auto smooth = [&](Id from, Id to, Id x) { return c.Ext(Glsl::SmoothStep, f32, { from, to, x }); };

        const Id below = distance(two(f(0), mul(f32, radius, f(0.8f))));
        const Id shadow = mul(f32, f(0.6f), sub(f32, f(1), smooth(c.Op_(Op::FNegate, f32, { radius }),
                                                                   mul(f32, radius, f(3)), below)));
        const Id behind = scale(f3, backdrop, sub(f32, f(1), shadow));

        const Id edge = distance(two(f(0), f(0)));
        const Id inside = sub(f32, f(1), smooth(f(-0.75f), f(0.75f), edge));
        const Id at = c.Op_(Op::FDiv, f2, { sub(f2, p, from), sub(f2, to, from) });
        const Id rim = smooth(c.Op_(Op::FNegate, f32, { add(f32, outline, f(0.75f)) }),
                              c.Op_(Op::FNegate, f32, { sub(f32, outline, f(0.25f)) }), edge);
        const Id outlineColour = c.Op_(Op::CompositeConstruct, f3, { f(70 / 255.0f), f(73 / 255.0f), f(78 / 255.0f) });
        const Id face = c.Ext(Glsl::FMix, f3, { rgb(sample(picture, at, f(0))), outlineColour, three(rim) });
        const Id mixed = c.Ext(Glsl::FMix, f3, { behind, face, three(inside) });
        spirv::Module::Emit(m.Code(), Op::Store,
                            { colour, c.Op_(Op::CompositeConstruct, f4, { mixed, shown }) });
        return c.End();
    }

    struct Blur
    {
        VkImage image = VK_NULL_HANDLE;
        VkDeviceMemory memory = VK_NULL_HANDLE;
        VkImageView view = VK_NULL_HANDLE;
        uint32_t frameWidth = 0, frameHeight = 0;   // what it was made for
        uint32_t levels = 0;
        VkExtent2D level[kLevels]{};
        bool filled = false;
    };

    struct State
    {
        VkDevice device = VK_NULL_HANDLE;
        VkPhysicalDevice physical = VK_NULL_HANDLE;
        VkFormat format = VK_FORMAT_UNDEFINED;
        VkDescriptorSetLayout setLayout = VK_NULL_HANDLE;
        VkPipelineLayout layout = VK_NULL_HANDLE;
        VkPipeline pipeline = VK_NULL_HANDLE;
        VkSampler sampler = VK_NULL_HANDLE;
        VkDescriptorPool pool = VK_NULL_HANDLE;
        VkDescriptorSet sets[kSlots]{};
        VkImageView targets[kSlots]{};   // a view of the image each slot drew into last
        PFN_vkCmdBeginRenderingKHR beginRendering = nullptr;
        PFN_vkCmdEndRenderingKHR endRendering = nullptr;
        Blur blur;
    };
    State g;
    // A format the pipeline could not be made for, not tried again.
    VkFormat g_failed = VK_FORMAT_UNDEFINED;

    bool Check(VkResult result, const char* what)
    {
        if (result == VK_SUCCESS) return true;
        LOGW("vulkan: the screens' backdrop: %s failed (%d)", what, int(result));
        return false;
    }

    VkShaderModule Module(const std::vector<uint32_t>& words)
    {
        VkShaderModuleCreateInfo info{ VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO };
        info.codeSize = words.size() * sizeof(uint32_t);
        info.pCode = words.data();
        VkShaderModule module = VK_NULL_HANDLE;
        Check(vkCreateShaderModule(g.device, &info, nullptr, &module), "a shader");
        return module;
    }

    bool MakePipeline()
    {
        VkDescriptorSetLayoutBinding bindings[2]{};
        for (uint32_t i = 0; i < 2; i++)
        {
            bindings[i].binding = i;
            bindings[i].descriptorType = VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER;
            bindings[i].descriptorCount = 1;
            bindings[i].stageFlags = VK_SHADER_STAGE_FRAGMENT_BIT;
        }
        VkDescriptorSetLayoutCreateInfo setInfo{ VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO };
        setInfo.bindingCount = 2;
        setInfo.pBindings = bindings;
        if (!Check(vkCreateDescriptorSetLayout(g.device, &setInfo, nullptr, &g.setLayout), "the set layout"))
            return false;

        VkPushConstantRange range{ VK_SHADER_STAGE_FRAGMENT_BIT, 0, sizeof(Push) };
        VkPipelineLayoutCreateInfo layoutInfo{ VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO };
        layoutInfo.setLayoutCount = 1;
        layoutInfo.pSetLayouts = &g.setLayout;
        layoutInfo.pushConstantRangeCount = 1;
        layoutInfo.pPushConstantRanges = &range;
        if (!Check(vkCreatePipelineLayout(g.device, &layoutInfo, nullptr, &g.layout), "the pipeline layout"))
            return false;

        const VkShaderModule vertex = Module(VertexShader()), fragment = Module(FragmentShader());
        VkPipelineShaderStageCreateInfo stages[2]{};
        stages[0] = { VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO };
        stages[0].stage = VK_SHADER_STAGE_VERTEX_BIT;
        stages[0].module = vertex;
        stages[0].pName = "main";
        stages[1] = { VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO };
        stages[1].stage = VK_SHADER_STAGE_FRAGMENT_BIT;
        stages[1].module = fragment;
        stages[1].pName = "main";

        VkPipelineVertexInputStateCreateInfo input{ VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO };
        VkPipelineInputAssemblyStateCreateInfo assembly{ VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO };
        assembly.topology = VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST;
        VkPipelineViewportStateCreateInfo viewport{ VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO };
        viewport.viewportCount = 1;
        viewport.scissorCount = 1;
        VkPipelineRasterizationStateCreateInfo raster{ VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO };
        raster.polygonMode = VK_POLYGON_MODE_FILL;
        raster.cullMode = VK_CULL_MODE_NONE;
        raster.lineWidth = 1.0f;
        VkPipelineMultisampleStateCreateInfo multisample{ VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO };
        multisample.rasterizationSamples = VK_SAMPLE_COUNT_1_BIT;
        // Over the frame by `shown`; the window's alpha is left alone.
        VkPipelineColorBlendAttachmentState attachment{};
        attachment.blendEnable = VK_TRUE;
        attachment.srcColorBlendFactor = VK_BLEND_FACTOR_SRC_ALPHA;
        attachment.dstColorBlendFactor = VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA;
        attachment.colorBlendOp = VK_BLEND_OP_ADD;
        attachment.srcAlphaBlendFactor = VK_BLEND_FACTOR_ZERO;
        attachment.dstAlphaBlendFactor = VK_BLEND_FACTOR_ONE;
        attachment.alphaBlendOp = VK_BLEND_OP_ADD;
        attachment.colorWriteMask = VK_COLOR_COMPONENT_R_BIT | VK_COLOR_COMPONENT_G_BIT | VK_COLOR_COMPONENT_B_BIT;
        VkPipelineColorBlendStateCreateInfo blend{ VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO };
        blend.attachmentCount = 1;
        blend.pAttachments = &attachment;
        const VkDynamicState dynamics[] = { VK_DYNAMIC_STATE_VIEWPORT, VK_DYNAMIC_STATE_SCISSOR };
        VkPipelineDynamicStateCreateInfo dynamic{ VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO };
        dynamic.dynamicStateCount = 2;
        dynamic.pDynamicStates = dynamics;
        VkPipelineRenderingCreateInfoKHR rendering{ VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO_KHR };
        rendering.colorAttachmentCount = 1;
        rendering.pColorAttachmentFormats = &g.format;

        VkGraphicsPipelineCreateInfo info{ VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO };
        info.pNext = &rendering;
        info.stageCount = 2;
        info.pStages = stages;
        info.pVertexInputState = &input;
        info.pInputAssemblyState = &assembly;
        info.pViewportState = &viewport;
        info.pRasterizationState = &raster;
        info.pMultisampleState = &multisample;
        info.pColorBlendState = &blend;
        info.pDynamicState = &dynamic;
        info.layout = g.layout;
        const bool made = vertex && fragment &&
            Check(vkCreateGraphicsPipelines(g.device, VK_NULL_HANDLE, 1, &info, nullptr, &g.pipeline), "the pipeline");
        if (vertex) vkDestroyShaderModule(g.device, vertex, nullptr);
        if (fragment) vkDestroyShaderModule(g.device, fragment, nullptr);
        return made;
    }

    bool MakeDescriptors()
    {
        VkSamplerCreateInfo sampler{ VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO };
        sampler.magFilter = sampler.minFilter = VK_FILTER_LINEAR;
        sampler.mipmapMode = VK_SAMPLER_MIPMAP_MODE_LINEAR;
        sampler.addressModeU = sampler.addressModeV = sampler.addressModeW = VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE;
        sampler.maxLod = VK_LOD_CLAMP_NONE;
        if (!Check(vkCreateSampler(g.device, &sampler, nullptr, &g.sampler), "the sampler")) return false;

        VkDescriptorPoolSize size{ VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, 2 * kSlots };
        VkDescriptorPoolCreateInfo poolInfo{ VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO };
        poolInfo.maxSets = kSlots;
        poolInfo.poolSizeCount = 1;
        poolInfo.pPoolSizes = &size;
        if (!Check(vkCreateDescriptorPool(g.device, &poolInfo, nullptr, &g.pool), "the descriptor pool")) return false;
        VkDescriptorSetLayout layouts[kSlots];
        std::fill(std::begin(layouts), std::end(layouts), g.setLayout);
        VkDescriptorSetAllocateInfo allocate{ VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO };
        allocate.descriptorPool = g.pool;
        allocate.descriptorSetCount = kSlots;
        allocate.pSetLayouts = layouts;
        return Check(vkAllocateDescriptorSets(g.device, &allocate, g.sets), "the descriptor sets");
    }

    void DestroyBlur()
    {
        if (g.blur.view) vkDestroyImageView(g.device, g.blur.view, nullptr);
        if (g.blur.image) vkDestroyImage(g.device, g.blur.image, nullptr);
        if (g.blur.memory) vkFreeMemory(g.device, g.blur.memory, nullptr);
        g.blur = Blur{};
    }

    bool MakeBlur(uint32_t frameWidth, uint32_t frameHeight)
    {
        Blur& b = g.blur;
        b.frameWidth = frameWidth;
        b.frameHeight = frameHeight;
        VkExtent2D size{ std::max(frameWidth / 2, 1u), std::max(frameHeight / 2, 1u) };
        for (b.levels = 0; b.levels < kLevels; b.levels++)
        {
            b.level[b.levels] = size;
            if (size.width == 1 && size.height == 1) { b.levels++; break; }
            size = { std::max(size.width / 2, 1u), std::max(size.height / 2, 1u) };
        }

        VkImageCreateInfo image{ VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO };
        image.imageType = VK_IMAGE_TYPE_2D;
        image.format = VK_FORMAT_B8G8R8A8_UNORM;
        image.extent = { b.level[0].width, b.level[0].height, 1 };
        image.mipLevels = b.levels;
        image.arrayLayers = 1;
        image.samples = VK_SAMPLE_COUNT_1_BIT;
        image.tiling = VK_IMAGE_TILING_OPTIMAL;
        image.usage = VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT | VK_IMAGE_USAGE_SAMPLED_BIT;
        image.initialLayout = VK_IMAGE_LAYOUT_UNDEFINED;
        if (!Check(vkCreateImage(g.device, &image, nullptr, &b.image), "the blur's image")) return false;
        VkMemoryRequirements needs{};
        vkGetImageMemoryRequirements(g.device, b.image, &needs);
        VkPhysicalDeviceMemoryProperties memory{};
        vkGetPhysicalDeviceMemoryProperties(g.physical, &memory);
        VkMemoryAllocateInfo allocate{ VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO };
        allocate.allocationSize = needs.size;
        allocate.memoryTypeIndex = vk::util::FindMemory(memory, needs.memoryTypeBits, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
        if (allocate.memoryTypeIndex == UINT32_MAX ||
            !Check(vkAllocateMemory(g.device, &allocate, nullptr, &b.memory), "the blur's memory") ||
            !Check(vkBindImageMemory(g.device, b.image, b.memory, 0), "the blur's memory"))
            return false;
        VkImageViewCreateInfo view{ VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO };
        view.image = b.image;
        view.viewType = VK_IMAGE_VIEW_TYPE_2D;
        view.format = image.format;
        view.subresourceRange = { VK_IMAGE_ASPECT_COLOR_BIT, 0, b.levels, 0, 1 };
        return Check(vkCreateImageView(g.device, &view, nullptr, &b.view), "the blur's view");
    }

    void LevelBarrier(VkCommandBuffer commands, uint32_t first, uint32_t count, VkImageLayout from, VkImageLayout to,
                      VkAccessFlags fromAccess, VkAccessFlags toAccess,
                      VkPipelineStageFlags fromStage, VkPipelineStageFlags toStage)
    {
        VkImageMemoryBarrier barrier{ VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER };
        barrier.srcAccessMask = fromAccess;
        barrier.dstAccessMask = toAccess;
        barrier.oldLayout = from;
        barrier.newLayout = to;
        barrier.srcQueueFamilyIndex = barrier.dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
        barrier.image = g.blur.image;
        barrier.subresourceRange = { VK_IMAGE_ASPECT_COLOR_BIT, first, count, 0, 1 };
        vkCmdPipelineBarrier(commands, fromStage, toStage, 0, 0, nullptr, 0, nullptr, 1, &barrier);
    }

    // The frame copied down the chain: each level a linear blit of the one
    // above, so every texel of the frame counts. Read in the fragment shader.
    void Fill(const vk::backdrop::Draw& draw)
    {
        const Blur& b = g.blur;
        const uint32_t last = b.levels - 1;
        LevelBarrier(draw.commands, 0, b.levels,
                     b.filled ? VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL : VK_IMAGE_LAYOUT_UNDEFINED,
                     VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_ACCESS_SHADER_READ_BIT, VK_ACCESS_TRANSFER_WRITE_BIT,
                     VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT);
        for (uint32_t level = 0; level < b.levels; level++)
        {
            VkImageBlit blit{};
            blit.dstSubresource = { VK_IMAGE_ASPECT_COLOR_BIT, level, 0, 1 };
            blit.dstOffsets[1] = { int32_t(b.level[level].width), int32_t(b.level[level].height), 1 };
            if (level == 0)
            {
                blit.srcSubresource = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1 };
                blit.srcOffsets[1] = { int32_t(draw.frameWidth), int32_t(draw.frameHeight), 1 };
                vkCmdBlitImage(draw.commands, draw.frame, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                               b.image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &blit, VK_FILTER_LINEAR);
            }
            else
            {
                LevelBarrier(draw.commands, level - 1, 1, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                             VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, VK_ACCESS_TRANSFER_WRITE_BIT,
                             VK_ACCESS_TRANSFER_READ_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT);
                blit.srcSubresource = { VK_IMAGE_ASPECT_COLOR_BIT, level - 1, 0, 1 };
                blit.srcOffsets[1] = { int32_t(b.level[level - 1].width), int32_t(b.level[level - 1].height), 1 };
                vkCmdBlitImage(draw.commands, b.image, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                               b.image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &blit, VK_FILTER_LINEAR);
            }
        }
        if (last > 0)
            LevelBarrier(draw.commands, 0, last, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                         VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, VK_ACCESS_TRANSFER_READ_BIT, VK_ACCESS_SHADER_READ_BIT,
                         VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT);
        LevelBarrier(draw.commands, last, 1, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                     VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, VK_ACCESS_TRANSFER_WRITE_BIT, VK_ACCESS_SHADER_READ_BIT,
                     VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT);
        g.blur.filled = true;
    }

    // Before any frame, a blur of nothing: black.
    void Clear(VkCommandBuffer commands)
    {
        LevelBarrier(commands, 0, g.blur.levels, VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 0,
                     VK_ACCESS_TRANSFER_WRITE_BIT, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT);
        VkClearColorValue black{};
        VkImageSubresourceRange range{ VK_IMAGE_ASPECT_COLOR_BIT, 0, g.blur.levels, 0, 1 };
        vkCmdClearColorImage(commands, g.blur.image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, &black, 1, &range);
        LevelBarrier(commands, 0, g.blur.levels, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                     VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, VK_ACCESS_TRANSFER_WRITE_BIT, VK_ACCESS_SHADER_READ_BIT,
                     VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT);
        g.blur.filled = true;
    }
}

bool vk::backdrop::Create(VkDevice device, VkPhysicalDevice physical, VkFormat format)
{
    if (g.pipeline && g.format == format) return true;
    if (format == g_failed) return false;
    Destroy();
    g.device = device;
    g.physical = physical;
    g.format = format;
    g.beginRendering = PFN_vkCmdBeginRenderingKHR(vkGetDeviceProcAddr(device, "vkCmdBeginRenderingKHR"));
    g.endRendering = PFN_vkCmdEndRenderingKHR(vkGetDeviceProcAddr(device, "vkCmdEndRenderingKHR"));
    if (!g.beginRendering || !g.endRendering || !MakePipeline() || !MakeDescriptors())
    {
        LOGW("vulkan: the screens are drawn without their backdrop");
        Destroy();
        g_failed = format;
        return false;
    }
    return true;
}

void vk::backdrop::Destroy()
{
    if (!g.device) return;
    DestroyBlur();
    for (VkImageView& view : g.targets)
        if (view) { vkDestroyImageView(g.device, view, nullptr); view = VK_NULL_HANDLE; }
    if (g.pool) vkDestroyDescriptorPool(g.device, g.pool, nullptr);
    if (g.sampler) vkDestroySampler(g.device, g.sampler, nullptr);
    if (g.pipeline) vkDestroyPipeline(g.device, g.pipeline, nullptr);
    if (g.layout) vkDestroyPipelineLayout(g.device, g.layout, nullptr);
    if (g.setLayout) vkDestroyDescriptorSetLayout(g.device, g.setLayout, nullptr);
    g = State{};
}

void vk::backdrop::Record(const Draw& draw)
{
    if (!g.pipeline || draw.slot >= kSlots) return;
    VkCommandBuffer commands = draw.commands;

    if (draw.frame && (draw.frameWidth != g.blur.frameWidth || draw.frameHeight != g.blur.frameHeight))
    {
        if (g.blur.image) draw.settle();
        DestroyBlur();
        if (!MakeBlur(draw.frameWidth, draw.frameHeight)) { DestroyBlur(); return; }
    }
    if (!g.blur.image)
    {
        if (!MakeBlur(16, 16)) { DestroyBlur(); return; }
        Clear(commands);
    }
    if (draw.frame) Fill(draw);

    // The slot's last view is no longer read: its frame's fence was waited for.
    VkImageView& view = g.targets[draw.slot];
    if (view) vkDestroyImageView(g.device, view, nullptr);
    view = VK_NULL_HANDLE;
    VkImageViewCreateInfo viewInfo{ VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO };
    viewInfo.image = draw.target;
    viewInfo.viewType = VK_IMAGE_VIEW_TYPE_2D;
    viewInfo.format = g.format;
    viewInfo.subresourceRange = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 };
    if (!Check(vkCreateImageView(g.device, &viewInfo, nullptr, &view), "the window's view")) return;

    VkDescriptorImageInfo images[2]{
        { g.sampler, g.blur.view, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL },
        { g.sampler, draw.picture, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL },
    };
    VkWriteDescriptorSet writes[2]{};
    for (uint32_t i = 0; i < 2; i++)
    {
        writes[i] = { VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET };
        writes[i].dstSet = g.sets[draw.slot];
        writes[i].dstBinding = i;
        writes[i].descriptorCount = 1;
        writes[i].descriptorType = VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER;
        writes[i].pImageInfo = &images[i];
    }
    vkUpdateDescriptorSets(g.device, 2, writes, 0, nullptr);

    vk::util::Barrier(commands, draw.target, VK_IMAGE_ASPECT_COLOR_BIT,
                      VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                      VK_ACCESS_TRANSFER_WRITE_BIT,
                      VK_ACCESS_COLOR_ATTACHMENT_READ_BIT | VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT,
                      VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT);

    VkRenderingAttachmentInfoKHR attachment{ VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO_KHR };
    attachment.imageView = view;
    attachment.imageLayout = VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL;
    attachment.loadOp = VK_ATTACHMENT_LOAD_OP_LOAD;
    attachment.storeOp = VK_ATTACHMENT_STORE_OP_STORE;
    VkRenderingInfoKHR rendering{ VK_STRUCTURE_TYPE_RENDERING_INFO_KHR };
    rendering.renderArea = { { 0, 0 }, draw.extent };
    rendering.layerCount = 1;
    rendering.colorAttachmentCount = 1;
    rendering.pColorAttachments = &attachment;
    g.beginRendering(commands, &rendering);

    VkViewport viewport{ 0, 0, float(draw.extent.width), float(draw.extent.height), 0, 1 };
    VkRect2D scissor{ { 0, 0 }, draw.extent };
    vkCmdBindPipeline(commands, VK_PIPELINE_BIND_POINT_GRAPHICS, g.pipeline);
    vkCmdSetViewport(commands, 0, 1, &viewport);
    vkCmdSetScissor(commands, 0, 1, &scissor);
    vkCmdBindDescriptorSets(commands, VK_PIPELINE_BIND_POINT_GRAPHICS, g.layout, 0, 1, &g.sets[draw.slot], 0, nullptr);

    // The picture grows the last few percent as it fades in, about its centre.
    const float grow = 0.96f + 0.04f * draw.shown;
    const float cx = 0.5f * float(draw.pictureFrom.x + draw.pictureTo.x);
    const float cy = 0.5f * float(draw.pictureFrom.y + draw.pictureTo.y);
    const float hw = 0.5f * float(draw.pictureTo.x - draw.pictureFrom.x) * grow;
    const float hh = 0.5f * float(draw.pictureTo.y - draw.pictureFrom.y) * grow;
    const Blur& b = g.blur;
    const VkExtent2D smallest = b.level[b.levels - 1];
    // Before any frame, the whole window: the blur is black there anyway, and
    // the shader divides by the frame's size.
    const bool framed = draw.frameTo.x > draw.frameFrom.x && draw.frameTo.y > draw.frameFrom.y;
    const VkOffset2D from = framed ? draw.frameFrom : VkOffset2D{ 0, 0 };
    const VkOffset2D to = framed ? draw.frameTo : VkOffset2D{ int32_t(draw.extent.width), int32_t(draw.extent.height) };
    Push push{
        { float(from.x), float(from.y), float(to.x), float(to.y) },
        { cx - hw, cy - hh, cx + hw, cy + hh },
        { draw.shown, draw.outline * 5, draw.outline * grow, 0 },
        { float(draw.extent.width), float(draw.extent.height), 1.0f / float(smallest.width),
          1.0f / float(smallest.height) },
    };
    vkCmdPushConstants(commands, g.layout, VK_SHADER_STAGE_FRAGMENT_BIT, 0, sizeof push, &push);
    vkCmdDraw(commands, 3, 1, 0, 0);
    g.endRendering(commands);

    vk::util::Barrier(commands, draw.target, VK_IMAGE_ASPECT_COLOR_BIT,
                      VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                      VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT, VK_ACCESS_TRANSFER_WRITE_BIT,
                      VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT);
}
#endif
