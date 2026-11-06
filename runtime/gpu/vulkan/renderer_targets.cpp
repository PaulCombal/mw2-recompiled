// EDRAM surfaces as Vulkan images: making them, reading them back, the
// rendering instances they are drawn in, and clearing them.
#include "renderer_state.h"
#include "pipeline.h"
#include "spirv.h"
#include "../../log.h"

#include <algorithm>
#include <bit>
#include <map>
#include <vector>
#if MW2_DIAGNOSTICS
#include "trace.h"   // last: the command trace's wrappers
#endif

#ifdef MW2_HAVE_VULKAN

namespace vk::renderer::detail
{
    bool MakeImage(Target& target, VkImageUsageFlags usage, VkImageAspectFlags aspect)
    {
        VkImageCreateInfo info{ VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO };
        info.imageType = VK_IMAGE_TYPE_2D;
        info.format = target.format;
        info.extent = { target.width * target.scale, target.height * target.scale, 1 };
        info.mipLevels = 1;
        info.arrayLayers = 1;
        info.samples = target.samples;
        info.tiling = VK_IMAGE_TILING_OPTIMAL;
        info.usage = usage;
        info.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
        info.initialLayout = VK_IMAGE_LAYOUT_UNDEFINED;
        if (vkCreateImage(g.device, &info, nullptr, &target.image) != VK_SUCCESS) return false;

        VkMemoryRequirements needs{};
        vkGetImageMemoryRequirements(g.device, target.image, &needs);
        const uint32_t type = FindMemory(needs.memoryTypeBits, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
        if (type == UINT32_MAX) return false;
        VkMemoryAllocateInfo allocate{ VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO };
        allocate.allocationSize = needs.size;
        allocate.memoryTypeIndex = type;
        if (vkAllocateMemory(g.device, &allocate, nullptr, &target.memory) != VK_SUCCESS) return false;
        vkBindImageMemory(g.device, target.image, target.memory, 0);

        if (aspect)
        {
            VkImageViewCreateInfo view{ VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO };
            view.image = target.image;
            view.viewType = VK_IMAGE_VIEW_TYPE_2D;
            view.format = target.format;
            view.subresourceRange = { aspect, 0, 1, 0, 1 };
            if (vkCreateImageView(g.device, &view, nullptr, &target.view) != VK_SUCCESS) return false;
        }
        return true;
    }

    VkFormat ColourFormatFor(uint32_t format)
    {
        switch (format)
        {
        case 0: case 1: case 10: return VK_FORMAT_R8G8B8A8_UNORM;   // 8_8_8_8 and gamma
        case 2: case 3: case 12: return VK_FORMAT_A2B10G10R10_UNORM_PACK32;
        case 4:  return VK_FORMAT_R16G16_SNORM;
        case 5:  return VK_FORMAT_R16G16B16A16_SNORM;
        case 6:  return VK_FORMAT_R16G16_SFLOAT;
        case 7:  return VK_FORMAT_R16G16B16A16_SFLOAT;
        case 14: return VK_FORMAT_R32_SFLOAT;
        case 15: return VK_FORMAT_R32G32_SFLOAT;
        default: return VK_FORMAT_UNDEFINED;
        }
    }

    // EDRAM has one depth buffer, not one per way of describing it. A pass that
    // writes depth at 1040 wide without multisampling and a pass that tests it at
    // 1040 wide with two samples are the same tiles on the hardware; keeping an
    // image per (pitch, samples) gives the second pass a buffer the first never
    // wrote, and MW2 draws its whole world in the second pass. So the pitch is
    // not part of a depth target's key.
    //
    // With the surfaces drawn multisampled the sample count stays in the key:
    // a depth image has one sample count, and the colour target it is bound
    // beside has to share it. MW2 tests and writes its world depth at 2x in
    // both passes, so nothing is split by that.
    TargetKey DepthKey(uint32_t baseTile, uint32_t format, uint32_t samples, uint32_t scale)
    {
        return { baseTile, 0, format, g.msaa ? samples : 0, true, scale };
    }

    // The format is in the key, and a D24S8 and a D24FS8 surface at one tile are
    // two images wherever the two have different host formats. MW2 draws all
    // its depth in D24FS8, reversed (cleared to zero, tested GEQUAL): an
    // integer format there spends its steps near the camera and leaves the
    // distance a few, so it is float on every device, as on AMD all along.
    // D24S8 is a pass with no depth surface (RB_DEPTH_INFO 0, tile 0): a few
    // hundred draws in the training level, none in a multiplayer match, none
    // with depth or stencil on. Where its image is another one, on a device
    // with D24S8, nothing it holds is ever read.
    VkFormat DepthFormatFor(uint32_t guestFormat)
    {
        return guestFormat ? g.floatDepthFormat : g.depthFormat;
    }

    // As Xenia's UNorm24To32 and Float20e4To32: the unorm divided by 2^24 with
    // its top bit added back, which is exact, and the float24 with 4 exponent
    // bits biased by 15 and 20 of mantissa, denormals normalised. A float24
    // reaches 2, past what a clear may write; the host's depth is 0 to 1.
    float DepthClearValue(uint32_t guestFormat, uint32_t packed)
    {
        const uint32_t value = packed >> 8;
        if (!guestFormat) return float(value + (value >> 23)) * (1.0f / float(1u << 24));
        if (!value) return 0.0f;
        uint32_t mantissa = value & 0xFFFFF;
        uint32_t exponent = value >> 20;
        if (!exponent)
        {
            const uint32_t shift = uint32_t(std::countl_zero(mantissa)) - (32 - 21);
            exponent = uint32_t(1 - int32_t(shift));
            mantissa = (mantissa << shift) & 0xFFFFF;
        }
        return std::min(std::bit_cast<float>(((exponent + 112) << 23) | (mantissa << 3)), 1.0f);
    }

    // The console's 1x, 2x and 4x, as the device can draw them. A count the
    // device lacks is rounded up to the next one it has; 1x is the last resort.
    //
    // MW2_MSAA=<n> draws every surface the title multisamples at n instead --
    // 8 on most devices -- for seeing that it works: at the console's own 2x,
    // upscaled from 1024x600 to the window with a linear blit, the difference is
    // slight. A count past what the device offers is rounded down to what it has.
    VkSampleCountFlagBits HostSamples(uint32_t guestSamples)
    {
        if (!g.msaa) return VK_SAMPLE_COUNT_1_BIT;
        static const uint32_t forced = uint32_t(env::Number("MW2_MSAA"));
        if (forced > 1 && guestSamples)
        {
            for (uint32_t bit = 64; bit > 1; bit >>= 1)
                if (bit <= forced && (g.sampleCounts & bit)) return VkSampleCountFlagBits(bit);
        }
        for (uint32_t bit = 1u << (guestSamples & 3); bit <= 64; bit <<= 1)
            if (g.sampleCounts & bit) return VkSampleCountFlagBits(bit);
        return VK_SAMPLE_COUNT_1_BIT;
    }

    // Besides the clears a resolve asks for (ClearAfterResolve), MW2 clears
    // EDRAM by drawing:
    // it points the surface at a base with a pitch and height that span the rest
    // of the memory and draws rectangles over all of it with the depth test
    // ALWAYS and depth writes on. The rectangles are in EDRAM's own coordinates
    // -- the viewport transform is switched off for them -- and they come in
    // pairs, because a tile is 80 samples wide and a surface is not a whole
    // number of tiles: one rectangle covers the columns that divide evenly, a
    // second, describing the same memory at a different pitch and sample count,
    // covers the column left over. Together they name one span of tiles from the
    // surface's base upwards, which is where every depth buffer the frame is
    // about to draw against lives.
    //
    // Here each (base, pitch, samples) is an image of its own, so a rectangle
    // covers whatever part of one image its own coordinates happen to fall in
    // and nothing else. So the idiom is recognised and answered with clears of
    // the images it names, and the rectangles themselves are not drawn. It is not
    // a model of EDRAM, but it is what this particular idiom means.

    void ClearAliasedDepth(uint32_t fromTile)
    {
        bool ended = false;
        for (auto& [key, target] : g.targets)
        {
            if (!target.depth || key.baseTile < fromTile) continue;
            // Nothing has been drawn since the last time this was cleared, so
            // there is nothing to clear. The idiom arrives as a pair of
            // rectangles covering one tile span between them, and each pass
            // clears before it draws, so without this every target is cleared
            // several times a frame for nothing.
            if (target.clearedAtDraw == g.drawsRecorded) continue;
            if (!ended) { EndPass(); ended = true; }
            const VkImageAspectFlags aspect =
                VK_IMAGE_ASPECT_DEPTH_BIT | VK_IMAGE_ASPECT_STENCIL_BIT;
            RecordBarrier(target.image, aspect,
                          VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
                          VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                          VK_ACCESS_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT, VK_ACCESS_TRANSFER_WRITE_BIT,
                          kDepthStages, VK_PIPELINE_STAGE_TRANSFER_BIT);
            // Zero: the level tests GEQUAL and RB_DEPTH_CLEAR is zero.
            const VkClearDepthStencilValue value{ 0.0f, 0 };
            const VkImageSubresourceRange range{ aspect, 0, 1, 0, 1 };
            const VkImage image = target.image;
            Record([=](VkCommandBuffer command) {
                vkCmdClearDepthStencilImage(command, image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                                            &value, 1, &range);
            });
            RecordBarrier(target.image, aspect,
                          VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                          VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
                          VK_ACCESS_TRANSFER_WRITE_BIT, kDepthAccess,
                          VK_PIPELINE_STAGE_TRANSFER_BIT, kDepthStages);
            target.clearedAtDraw = g.drawsRecorded;
            target.depthClearedIn = g.frames;
        }
    }

    Target* FindTarget(const TargetKey& key)
    {
        auto found = g.targets.find(key);
        return found == g.targets.end() ? nullptr : &found->second;
    }

    void DestroySingle(Target& target)
    {
        if (target.singleView) vkDestroyImageView(g.device, target.singleView, nullptr);
        if (target.singleImage) vkDestroyImage(g.device, target.singleImage, nullptr);
        if (target.singleMemory) vkFreeMemory(g.device, target.singleMemory, nullptr);
        target.singleView = VK_NULL_HANDLE;
        target.singleImage = VK_NULL_HANDLE;
        target.singleMemory = VK_NULL_HANDLE;
    }

    void DestroyTarget(Target& target)
    {
        ForgetSampledDepth(target);
        if (target.view) vkDestroyImageView(g.device, target.view, nullptr);
        if (target.image) vkDestroyImage(g.device, target.image, nullptr);
        if (target.memory) vkFreeMemory(g.device, target.memory, nullptr);
        DestroySingle(target);
    }

    // The single-sampled twin of a multisampled target, made on first use. A
    // colour twin is a transfer destination the resolve fills; a depth twin is
    // an attachment, because a depth resolve only happens inside a render pass.
    bool EnsureSingle(Target& target)
    {
        if (target.singleImage) return true;
        Target one = target;
        one.samples = VK_SAMPLE_COUNT_1_BIT;
        one.image = VK_NULL_HANDLE;
        one.memory = VK_NULL_HANDLE;
        one.view = VK_NULL_HANDLE;
        const VkImageUsageFlags usage = target.depth
            ? (VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT)
            : (VK_IMAGE_USAGE_TRANSFER_DST_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT);
        // A view only for the depth twin, which is bound as an attachment; a
        // transfer-only image may not have one.
        const VkImageAspectFlags aspect = target.depth
            ? (VK_IMAGE_ASPECT_DEPTH_BIT | VK_IMAGE_ASPECT_STENCIL_BIT)
            : 0;
        if (!MakeImage(one, usage, aspect))
        {
            if (one.view) vkDestroyImageView(g.device, one.view, nullptr);
            if (one.image) vkDestroyImage(g.device, one.image, nullptr);
            if (one.memory) vkFreeMemory(g.device, one.memory, nullptr);
            return false;
        }
        target.singleImage = one.image;
        target.singleMemory = one.memory;
        target.singleView = one.view;
        return true;
    }

    // The rectangle of a multisampled colour target resolved into `into`, an
    // image of its format in TRANSFER_DST, at the same place. Outside any
    // render pass; the target is an attachment again afterwards.
    void ResolveColourInto(Target& target, VkOffset2D at, uint32_t width, uint32_t height, VkImage into)
    {
        RecordBarrier(target.image, VK_IMAGE_ASPECT_COLOR_BIT,
                      VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                      VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT, VK_ACCESS_TRANSFER_READ_BIT,
                      kColourStages, VK_PIPELINE_STAGE_TRANSFER_BIT);
        VkImageResolve region{};
        region.srcSubresource = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1 };
        region.srcOffset = Scaled(at.x, at.y, 0, target.scale);
        region.dstSubresource = region.srcSubresource;
        region.dstOffset = region.srcOffset;
        region.extent = { width * target.scale, height * target.scale, 1 };
        const VkImage from = target.image;
        Record([=](VkCommandBuffer command) {
            vkCmdResolveImage(command, from, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                              into, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &region);
        });
        RecordBarrier(target.image, VK_IMAGE_ASPECT_COLOR_BIT,
                      VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                      VK_ACCESS_TRANSFER_READ_BIT, kColourAccess,
                      VK_PIPELINE_STAGE_TRANSFER_BIT, kColourStages);
    }

    // What a copy out of a colour target reads, in TRANSFER_SRC. For a
    // single-sampled target that is the target itself; for a multisampled one
    // the rectangle is resolved into the twin first. Outside any render pass.
    VkImage BeginReadingColour(Target& target, VkOffset2D at, uint32_t width, uint32_t height)
    {
        if (target.samples == VK_SAMPLE_COUNT_1_BIT)
        {
            RecordBarrier(target.image, VK_IMAGE_ASPECT_COLOR_BIT,
                          VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                          VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT, VK_ACCESS_TRANSFER_READ_BIT,
                          kColourStages, VK_PIPELINE_STAGE_TRANSFER_BIT);
            return target.image;
        }
        if (!EnsureSingle(target)) return VK_NULL_HANDLE;
        // From UNDEFINED: only the rectangle is read afterwards. After the
        // copies out of the twin's last contents.
        RecordBarrier(target.singleImage, VK_IMAGE_ASPECT_COLOR_BIT,
                      VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                      0, VK_ACCESS_TRANSFER_WRITE_BIT,
                      VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT);
        ResolveColourInto(target, at, width, height, target.singleImage);
        RecordBarrier(target.singleImage, VK_IMAGE_ASPECT_COLOR_BIT,
                      VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                      VK_ACCESS_TRANSFER_WRITE_BIT, VK_ACCESS_TRANSFER_READ_BIT,
                      VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT);
        return target.singleImage;
    }

    void EndReadingColour(Target& target)
    {
        if (target.samples != VK_SAMPLE_COUNT_1_BIT) return;   // never left the attachment layout
        RecordBarrier(target.image, VK_IMAGE_ASPECT_COLOR_BIT,
                      VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                      VK_ACCESS_TRANSFER_READ_BIT, kColourAccess,
                      VK_PIPELINE_STAGE_TRANSFER_BIT, kColourStages);
    }

    // ---- the depth resolve as a draw ----
    //
    // The first sample of a multisampled depth surface copied by a draw rather
    // than by the rendering instance's resolve: a triangle over the rectangle
    // whose fragment shader fetches sample 0 at its own pixel and writes it as
    // its depth, the test ALWAYS. Xenia reads multisampled surfaces the same
    // way, with a shader. It is the resolve wherever the device can sample the
    // surface (State::drawnDepthResolve): it costs what a rendering instance
    // resolving the depth alone costs, and leaves the stencil out without
    // asking a driver to (ResolveDepthInto). Float depth comes through exact;
    // a unorm one may round to the neighbouring step, on D24S8 surfaces, which
    // MW2 never draws depth into (DepthFormatFor).
    namespace
    {
        constexpr uint32_t kModelVertex = 0, kModelFragment = 4;
        constexpr uint32_t kModeOriginUpperLeft = 7, kModeDepthReplacing = 12;
        constexpr uint32_t kOpImageFetch = 95, kImageOperandsSample = 0x40;

        struct DrawnResolve
        {
            VkDescriptorSetLayout setLayout = VK_NULL_HANDLE;
            VkPipelineLayout layout = VK_NULL_HANDLE;
            VkDescriptorPool pool = VK_NULL_HANDLE;
            std::map<VkFormat, VkPipeline> pipelines;
            bool failed = false;
        };
        DrawnResolve drawn;

        spirv::Id Op_(spirv::Module& m, spirv::Op op, spirv::Id type, std::vector<uint32_t> operands)
        {
            const spirv::Id result = m.Allocate();
            operands.insert(operands.begin(), { type, result });
            m.Code().push_back(spirv::Head(op, uint32_t(operands.size() + 1)));
            m.Code().insert(m.Code().end(), operands.begin(), operands.end());
            return result;
        }
        spirv::Id Variable(spirv::Module& m, spirv::StorageClass storage, spirv::Id type)
        {
            const spirv::Id variable = m.Allocate();
            spirv::Module::Emit(m.Declarations(), spirv::Op::Variable,
                                { m.Pointer(storage, type), variable, uint32_t(storage) });
            return variable;
        }
        spirv::Id Main(spirv::Module& m, uint32_t model, const std::vector<spirv::Id>& interface)
        {
            const spirv::Id function = m.Allocate();
            spirv::Module::Emit(m.Declarations(), spirv::Op::TypeFunction, { function, m.Void() });
            const spirv::Id main = m.Allocate();
            m.EntryPoint(model, main, "main", interface);
            spirv::Module::Emit(m.Code(), spirv::Op::Function, { m.Void(), main, 0, function });
            spirv::Module::Emit(m.Code(), spirv::Op::Label, { m.Allocate() });
            return main;
        }
        std::vector<uint32_t> End(spirv::Module& m)
        {
            spirv::Module::Emit(m.Code(), spirv::Op::Return, {});
            spirv::Module::Emit(m.Code(), spirv::Op::FunctionEnd, {});
            return m.Finish();
        }

        // A triangle over the viewport: (-1,-1), (3,-1), (-1,3).
        std::vector<uint32_t> CoverVertexShader()
        {
            using namespace spirv;
            Module m;
            const Id i32 = m.Int(), f32 = m.Float(), f4 = m.Float4();
            const Id index = Variable(m, StorageClass::Input, i32);
            Module::Emit(m.Decorations(), Op::Decorate,
                         { index, uint32_t(Decoration::BuiltIn), uint32_t(BuiltIn::VertexIndex) });
            const Id position = Variable(m, StorageClass::Output, f4);
            Module::Emit(m.Decorations(), Op::Decorate,
                         { position, uint32_t(Decoration::BuiltIn), uint32_t(BuiltIn::Position) });
            Main(m, kModelVertex, { index, position });
            const Id i = Op_(m, Op::Load, i32, { index });
            const Id doubled = Op_(m, Op::ShiftLeftLogical, i32, { i, m.ConstantS(1) });
            const Id x = Op_(m, Op::ConvertSToF, f32, { Op_(m, Op::BitwiseAnd, i32, { doubled, m.ConstantS(2) }) });
            const Id y = Op_(m, Op::ConvertSToF, f32, { Op_(m, Op::BitwiseAnd, i32, { i, m.ConstantS(2) }) });
            auto clip = [&](Id v) {
                return Op_(m, Op::FSub, f32, { Op_(m, Op::FMul, f32, { v, m.ConstantF(2) }), m.ConstantF(1) });
            };
            const Id at = Op_(m, Op::CompositeConstruct, f4, { clip(x), clip(y), m.ConstantF(0), m.ConstantF(1) });
            Module::Emit(m.Code(), Op::Store, { position, at });
            return End(m);
        }

        // depth = texelFetch(surface, ivec2(gl_FragCoord.xy), 0).r
        std::vector<uint32_t> FetchDepthShader()
        {
            using namespace spirv;
            Module m;
            const Id f32 = m.Float(), f2 = m.Float2(), f4 = m.Float4(), i2 = m.IntVector(2);
            // Sampled type, 2D, not depth, not arrayed, multisampled, sampled, no format.
            const Id image = m.Allocate();
            Module::Emit(m.Declarations(), Op::TypeImage, { image, f32, 1, 0, 0, 1, 1, 0 });
            const Id surface = Variable(m, StorageClass::UniformConstant, image);
            Module::Emit(m.Decorations(), Op::Decorate, { surface, uint32_t(Decoration::DescriptorSet), 0 });
            Module::Emit(m.Decorations(), Op::Decorate, { surface, uint32_t(Decoration::Binding), 0 });
            const Id fragCoord = Variable(m, StorageClass::Input, f4);
            Module::Emit(m.Decorations(), Op::Decorate,
                         { fragCoord, uint32_t(Decoration::BuiltIn), uint32_t(BuiltIn::FragCoord) });
            const Id fragDepth = Variable(m, StorageClass::Output, f32);
            Module::Emit(m.Decorations(), Op::Decorate,
                         { fragDepth, uint32_t(Decoration::BuiltIn), uint32_t(BuiltIn::FragDepth) });
            const Id main = Main(m, kModelFragment, { fragCoord, fragDepth });
            Module::Emit(m.ExecutionModes(), Op::ExecutionMode, { main, kModeOriginUpperLeft });
            Module::Emit(m.ExecutionModes(), Op::ExecutionMode, { main, kModeDepthReplacing });
            const Id coord = Op_(m, Op::Load, f4, { fragCoord });
            const Id xy = Op_(m, Op::VectorShuffle, f2, { coord, coord, 0, 1 });
            const Id texel = Op_(m, Op::ConvertFToS, i2, { xy });
            const Id loaded = Op_(m, Op::Load, image, { surface });
            const Id fetched = Op_(m, Op(kOpImageFetch), f4, { loaded, texel, kImageOperandsSample, m.ConstantS(0) });
            Module::Emit(m.Code(), Op::Store, { fragDepth, Op_(m, Op::CompositeExtract, f32, { fetched, 0 }) });
            return End(m);
        }

        bool MakeDrawnLayout()
        {
            if (drawn.layout) return true;
            if (drawn.failed) return false;
            drawn.failed = true;
            VkDescriptorSetLayoutBinding binding{};
            binding.binding = 0;
            binding.descriptorType = VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE;
            binding.descriptorCount = 1;
            binding.stageFlags = VK_SHADER_STAGE_FRAGMENT_BIT;
            VkDescriptorSetLayoutCreateInfo setInfo{ VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO };
            setInfo.bindingCount = 1;
            setInfo.pBindings = &binding;
            if (vkCreateDescriptorSetLayout(g.device, &setInfo, nullptr, &drawn.setLayout) != VK_SUCCESS)
                return false;
            VkPipelineLayoutCreateInfo layoutInfo{ VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO };
            layoutInfo.setLayoutCount = 1;
            layoutInfo.pSetLayouts = &drawn.setLayout;
            if (vkCreatePipelineLayout(g.device, &layoutInfo, nullptr, &drawn.layout) != VK_SUCCESS) return false;
            // A set per multisampled depth target: a handful.
#if MW2_TRACE
            // MW2_DRAWN_RESOLVE_SETS=<n>: fewer, to run the pool dry.
            const uint32_t kSets = vk::trace::Forcing().drawnResolveSets ? vk::trace::Forcing().drawnResolveSets : 64;
            // MW2_DUMP_SHADERS: the draw's two shaders.
            vk::trace::DumpBuiltin("drawn_resolve_cover_vertex", CoverVertexShader());
            vk::trace::DumpBuiltin("drawn_resolve_fetch_depth", FetchDepthShader());
#else
            constexpr uint32_t kSets = 64;
#endif
            VkDescriptorPoolSize size{ VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE, kSets };
            VkDescriptorPoolCreateInfo poolInfo{ VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO };
            poolInfo.flags = VK_DESCRIPTOR_POOL_CREATE_FREE_DESCRIPTOR_SET_BIT;
            poolInfo.maxSets = kSets;
            poolInfo.poolSizeCount = 1;
            poolInfo.pPoolSizes = &size;
            if (vkCreateDescriptorPool(g.device, &poolInfo, nullptr, &drawn.pool) != VK_SUCCESS) return false;
            drawn.failed = false;
            return true;
        }

        VkShaderModule ShaderModule(const std::vector<uint32_t>& words)
        {
            VkShaderModuleCreateInfo info{ VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO };
            info.codeSize = words.size() * sizeof(uint32_t);
            info.pCode = words.data();
            VkShaderModule module = VK_NULL_HANDLE;
            vkCreateShaderModule(g.device, &info, nullptr, &module);
            return module;
        }

        VkPipeline DrawnPipeline(VkFormat format)
        {
            if (auto found = drawn.pipelines.find(format); found != drawn.pipelines.end()) return found->second;
            VkPipeline& pipeline = drawn.pipelines[format];
            const VkShaderModule vertex = ShaderModule(CoverVertexShader());
            const VkShaderModule fragment = ShaderModule(FetchDepthShader());
            VkPipelineShaderStageCreateInfo stages[2]{};
            for (uint32_t i = 0; i < 2; i++)
            {
                stages[i] = { VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO };
                stages[i].stage = i ? VK_SHADER_STAGE_FRAGMENT_BIT : VK_SHADER_STAGE_VERTEX_BIT;
                stages[i].module = i ? fragment : vertex;
                stages[i].pName = "main";
            }
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
            VkPipelineDepthStencilStateCreateInfo depth{ VK_STRUCTURE_TYPE_PIPELINE_DEPTH_STENCIL_STATE_CREATE_INFO };
            depth.depthTestEnable = VK_TRUE;
            depth.depthWriteEnable = VK_TRUE;
            depth.depthCompareOp = VK_COMPARE_OP_ALWAYS;
            VkPipelineColorBlendStateCreateInfo blend{ VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO };
            const VkDynamicState dynamics[] = { VK_DYNAMIC_STATE_VIEWPORT, VK_DYNAMIC_STATE_SCISSOR };
            VkPipelineDynamicStateCreateInfo dynamic{ VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO };
            dynamic.dynamicStateCount = 2;
            dynamic.pDynamicStates = dynamics;
            VkPipelineRenderingCreateInfoKHR rendering{ VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO_KHR };
            rendering.depthAttachmentFormat = format;
            rendering.stencilAttachmentFormat = format;
            VkGraphicsPipelineCreateInfo info{ VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO };
            info.pNext = &rendering;
            info.stageCount = 2;
            info.pStages = stages;
            info.pVertexInputState = &input;
            info.pInputAssemblyState = &assembly;
            info.pViewportState = &viewport;
            info.pRasterizationState = &raster;
            info.pMultisampleState = &multisample;
            info.pDepthStencilState = &depth;
            info.pColorBlendState = &blend;
            info.pDynamicState = &dynamic;
            info.layout = drawn.layout;
            if (!vertex || !fragment ||
                vkCreateGraphicsPipelines(g.device, VK_NULL_HANDLE, 1, &info, nullptr, &pipeline) != VK_SUCCESS)
            {
                pipeline = VK_NULL_HANDLE;
                LOGW("renderer: the depth resolve's draw pipeline could not be made; resolving as before");
            }
            if (vertex) vkDestroyShaderModule(g.device, vertex, nullptr);
            if (fragment) vkDestroyShaderModule(g.device, fragment, nullptr);
            return pipeline;
        }
    }

    void MakeSampledDepth(Target& target)
    {
        // The pipeline too, while the level loads rather than at its first frame.
        if (!MakeDrawnLayout() || !DrawnPipeline(target.format)) return;
        VkImageViewCreateInfo view{ VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO };
        view.image = target.image;
        view.viewType = VK_IMAGE_VIEW_TYPE_2D;
        view.format = target.format;
        view.subresourceRange = { VK_IMAGE_ASPECT_DEPTH_BIT, 0, 1, 0, 1 };
        if (vkCreateImageView(g.device, &view, nullptr, &target.sampledView) != VK_SUCCESS)
        {
            target.sampledView = VK_NULL_HANDLE;
            return;
        }
        VkDescriptorSetAllocateInfo allocate{ VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO };
        allocate.descriptorPool = drawn.pool;
        allocate.descriptorSetCount = 1;
        allocate.pSetLayouts = &drawn.setLayout;
        if (vkAllocateDescriptorSets(g.device, &allocate, &target.sampledSet) != VK_SUCCESS)
        {
            LOGW("renderer: no descriptor set for a multisampled depth target; it is resolved by a rendering"
                 " instance");
            target.sampledSet = VK_NULL_HANDLE;
            return;
        }
        VkDescriptorImageInfo image{ VK_NULL_HANDLE, target.sampledView,
                                     VK_IMAGE_LAYOUT_DEPTH_STENCIL_READ_ONLY_OPTIMAL };
        VkWriteDescriptorSet write{ VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET };
        write.dstSet = target.sampledSet;
        write.descriptorCount = 1;
        write.descriptorType = VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE;
        write.pImageInfo = &image;
        vkUpdateDescriptorSets(g.device, 1, &write, 0, nullptr);
    }

    void ForgetSampledDepth(Target& target)
    {
        if (target.sampledSet) vkFreeDescriptorSets(g.device, drawn.pool, 1, &target.sampledSet);
        if (target.sampledView) vkDestroyImageView(g.device, target.sampledView, nullptr);
        target.sampledSet = VK_NULL_HANDLE;
        target.sampledView = VK_NULL_HANDLE;
    }

    // After every target's ForgetSampledDepth.
    void DestroyDrawnResolve()
    {
        for (auto& [format, pipeline] : drawn.pipelines)
            if (pipeline) vkDestroyPipeline(g.device, pipeline, nullptr);
        if (drawn.pool) vkDestroyDescriptorPool(g.device, drawn.pool, nullptr);
        if (drawn.layout) vkDestroyPipelineLayout(g.device, drawn.layout, nullptr);
        if (drawn.setLayout) vkDestroyDescriptorSetLayout(g.device, drawn.setLayout, nullptr);
        drawn = DrawnResolve{};
    }

    // ResolveDepthInto's work done by a draw; false where the pipeline is
    // missing, and nothing recorded.
    bool DrawDepthResolve(Target& target, VkOffset2D at, uint32_t width, uint32_t height,
                          VkImage into, VkImageView intoView, VkImageLayout before, VkPipelineStageFlags usedBy,
                          VkImageLayout after, VkAccessFlags readAs, VkPipelineStageFlags readBy)
    {
        const VkPipeline pipeline = DrawnPipeline(target.format);
        if (!pipeline) return false;
        const VkRect2D area{ Scaled(at, target.scale), Scaled(VkExtent2D{ width, height }, target.scale) };
        const VkImage surface = target.image;
        const VkDescriptorSet set = target.sampledSet;
        const VkPipelineLayout layout = drawn.layout;
        const VkPipelineLayout mainLayout = static_cast<VkPipelineLayout>(vk::pipeline::Layout());
        const VkDescriptorSet mainSets[2] = { static_cast<VkDescriptorSet>(vk::textures::Set()), g.memorySet };
        Record([=](VkCommandBuffer command) {
            // The surface, written by the draws before, read by this one's
            // fragment shader; `into`, last read by `usedBy`, written as depth.
            VkImageMemoryBarrier barriers[2]{};
            for (VkImageMemoryBarrier& b : barriers)
            {
                b.sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER;
                b.srcQueueFamilyIndex = b.dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
                b.subresourceRange = { kDepthAspects, 0, 1, 0, 1 };
            }
            barriers[0].image = surface;
            barriers[0].srcAccessMask = VK_ACCESS_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT;
            barriers[0].dstAccessMask = VK_ACCESS_SHADER_READ_BIT;
            barriers[0].oldLayout = VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL;
            barriers[0].newLayout = VK_IMAGE_LAYOUT_DEPTH_STENCIL_READ_ONLY_OPTIMAL;
            barriers[1].image = into;
            barriers[1].srcAccessMask = 0;
            barriers[1].dstAccessMask = VK_ACCESS_DEPTH_STENCIL_ATTACHMENT_READ_BIT |
                                        VK_ACCESS_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT;
            barriers[1].oldLayout = before;
            barriers[1].newLayout = VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL;
            vkCmdPipelineBarrier(command, kDepthStages | usedBy,
                                 kDepthStages | VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT,
                                 0, 0, nullptr, 0, nullptr, 2, barriers);

            VkRenderingAttachmentInfoKHR depth{ VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO_KHR };
            depth.imageView = intoView;
            depth.imageLayout = VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL;
            depth.loadOp = before == VK_IMAGE_LAYOUT_UNDEFINED ? VK_ATTACHMENT_LOAD_OP_DONT_CARE
                                                                : VK_ATTACHMENT_LOAD_OP_LOAD;
            depth.storeOp = VK_ATTACHMENT_STORE_OP_STORE;
            // Nothing reads a resolved stencil.
            VkRenderingAttachmentInfoKHR stencil = depth;
            stencil.loadOp = VK_ATTACHMENT_LOAD_OP_DONT_CARE;
            stencil.storeOp = VK_ATTACHMENT_STORE_OP_DONT_CARE;
            VkRenderingInfoKHR info{ VK_STRUCTURE_TYPE_RENDERING_INFO_KHR };
            info.renderArea = area;
            info.layerCount = 1;
            info.pDepthAttachment = &depth;
            info.pStencilAttachment = &stencil;
            dispatch.beginRendering(command, &info);
            vkCmdBindPipeline(command, VK_PIPELINE_BIND_POINT_GRAPHICS, pipeline);
            vkCmdBindDescriptorSets(command, VK_PIPELINE_BIND_POINT_GRAPHICS, layout, 0, 1, &set, 0, nullptr);
            const VkViewport viewport{ float(area.offset.x), float(area.offset.y),
                                       float(area.extent.width), float(area.extent.height), 0.0f, 1.0f };
            vkCmdSetViewport(command, 0, 1, &viewport);
            vkCmdSetScissor(command, 0, 1, &area);
            vkCmdDraw(command, 3, 1, 0, 0);
            dispatch.endRendering(command);

            barriers[0].srcAccessMask = VK_ACCESS_SHADER_READ_BIT;
            barriers[0].dstAccessMask = kDepthAccess | VK_ACCESS_TRANSFER_READ_BIT | VK_ACCESS_TRANSFER_WRITE_BIT;
            barriers[0].oldLayout = VK_IMAGE_LAYOUT_DEPTH_STENCIL_READ_ONLY_OPTIMAL;
            barriers[0].newLayout = VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL;
            barriers[1].srcAccessMask = VK_ACCESS_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT;
            barriers[1].dstAccessMask = readAs;
            barriers[1].oldLayout = VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL;
            barriers[1].newLayout = after;
            vkCmdPipelineBarrier(command, kDepthStages | VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT,
                                 readBy | kDepthStages | VK_PIPELINE_STAGE_TRANSFER_BIT,
                                 0, 0, nullptr, 0, nullptr, 2, barriers);
            // The draws' own sets, which binding another layout's set 0 disturbed.
            vkCmdBindDescriptorSets(command, VK_PIPELINE_BIND_POINT_GRAPHICS, mainLayout,
                                    bindings::kTextureSet, 2, mainSets, 0, nullptr);
        });
        // The pipeline, viewport and scissor are this draw's now.
        g.bound = State::Bound{};
        return true;
    }

    // The same for a depth target, whose rectangle is resolved into the twin
    // by ResolveDepthInto.
    VkImage BeginReadingDepth(Target& target, VkOffset2D at, uint32_t width, uint32_t height)
    {
        if (target.samples == VK_SAMPLE_COUNT_1_BIT)
        {
            RecordBarrier(target.image, kDepthAspects,
                          VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
                          VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                          VK_ACCESS_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT, VK_ACCESS_TRANSFER_READ_BIT,
                          kDepthStages, VK_PIPELINE_STAGE_TRANSFER_BIT);
            return target.image;
        }
        if (!EnsureSingle(target)) return VK_NULL_HANDLE;
        // The copies before it read the twin, which comes out in
        // TRANSFER_SRC, ready to be copied from.
        ResolveDepthInto(target, at, width, height, target.singleImage, target.singleView,
                         VK_IMAGE_LAYOUT_UNDEFINED, VK_PIPELINE_STAGE_TRANSFER_BIT,
                         VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, VK_ACCESS_TRANSFER_READ_BIT,
                         VK_PIPELINE_STAGE_TRANSFER_BIT);
        return target.singleImage;
    }

    // A rendering instance with nothing drawn in it, which loads the
    // multisampled depth and resolves the rectangle into `into` at the same
    // place: a transfer cannot. The first sample, as the console's own depth
    // resolve takes one sample rather than an average -- an averaged depth is
    // a surface nothing was drawn at. The stencil is attached too, loaded and
    // stored, so the target's stays what it was: a view of both aspects
    // attached as depth alone would leave it to how a driver keeps the two
    // together. Nothing reads a resolved stencil, but it is resolved with the
    // depth, the same way: Intel's Windows driver, asked for the depth alone
    // (the stencil's mode NONE, which independentResolveNone allows), resolved
    // no depth either (issue #37). Both at the first sample is what every 1.2
    // device must offer. Where the device can sample the surface, a draw does
    // the work instead (DrawDepthResolve), and the stencil costs nothing. The
    // target stays an attachment throughout; `into` goes from `before` (last
    // used by `usedBy`) to `after`, for `readAs` at `readBy`.
    void ResolveDepthInto(Target& target, VkOffset2D at, uint32_t width, uint32_t height,
                          VkImage into, VkImageView intoView, VkImageLayout before, VkPipelineStageFlags usedBy,
                          VkImageLayout after, VkAccessFlags readAs, VkPipelineStageFlags readBy)
    {
        if (target.sampledSet && DrawDepthResolve(target, at, width, height, into, intoView, before, usedBy,
                                                   after, readAs, readBy))
            return;
        const VkImageView view = target.view;
        const VkRect2D area{ Scaled(at, target.scale), Scaled(VkExtent2D{ width, height }, target.scale) };
        Record([=](VkCommandBuffer command) {
            // The draws before it wrote the depth. The resolve reads it at the
            // colour output stage as a colour attachment read, depth or not
            // (the render pass chapter's resolve rules), and the load as a
            // depth one.
            VkMemoryBarrier written{ VK_STRUCTURE_TYPE_MEMORY_BARRIER };
            written.srcAccessMask = VK_ACCESS_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT;
            written.dstAccessMask = VK_ACCESS_DEPTH_STENCIL_ATTACHMENT_READ_BIT |
                                    VK_ACCESS_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT |
                                    VK_ACCESS_COLOR_ATTACHMENT_READ_BIT;
            VkImageMemoryBarrier entering{ VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER };
            entering.srcAccessMask = 0;
            entering.dstAccessMask = VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT |
                                     VK_ACCESS_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT;
            entering.oldLayout = before;
            entering.newLayout = VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL;
            entering.srcQueueFamilyIndex = entering.dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
            entering.image = into;
            entering.subresourceRange = { kDepthAspects, 0, 1, 0, 1 };
            vkCmdPipelineBarrier(command, kDepthStages | usedBy,
                                 kDepthStages | VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT,
                                 0, 1, &written, 0, nullptr, 1, &entering);

            VkRenderingAttachmentInfoKHR depth{ VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO_KHR };
            depth.imageView = view;
            depth.imageLayout = VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL;
            depth.resolveMode = VK_RESOLVE_MODE_SAMPLE_ZERO_BIT;
            depth.resolveImageView = intoView;
            depth.resolveImageLayout = VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL;
            depth.loadOp = VK_ATTACHMENT_LOAD_OP_LOAD;
            depth.storeOp = VK_ATTACHMENT_STORE_OP_STORE;
            VkRenderingInfoKHR info{ VK_STRUCTURE_TYPE_RENDERING_INFO_KHR };
            info.renderArea = area;
            info.layerCount = 1;
            info.pDepthAttachment = &depth;
            info.pStencilAttachment = &depth;
            dispatch.beginRendering(command, &info);
            dispatch.endRendering(command);

            // A resolve writes at the colour output stage whatever it resolves.
            VkImageMemoryBarrier leaving = entering;
            leaving.srcAccessMask = VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT |
                                    VK_ACCESS_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT;
            leaving.dstAccessMask = readAs;
            leaving.oldLayout = VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL;
            leaving.newLayout = after;
            // And the target, which stays an attachment, is next written by a
            // draw, a clear or a transition (ClearAfterResolve), none of which
            // waits for the colour output stage: each must come after the
            // resolve's read of it and the store that ends the instance.
            VkMemoryBarrier read{ VK_STRUCTURE_TYPE_MEMORY_BARRIER };
            read.srcAccessMask = VK_ACCESS_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT;
            read.dstAccessMask = kDepthAccess | VK_ACCESS_TRANSFER_READ_BIT | VK_ACCESS_TRANSFER_WRITE_BIT;
            vkCmdPipelineBarrier(command, VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT | kDepthStages,
                                 readBy | kDepthStages | VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 1, &read, 0, nullptr,
                                 1, &leaving);
        });
    }

    void EndReadingDepth(Target& target)
    {
        if (target.samples != VK_SAMPLE_COUNT_1_BIT) return;
        RecordBarrier(target.image, VK_IMAGE_ASPECT_DEPTH_BIT | VK_IMAGE_ASPECT_STENCIL_BIT,
                      VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                      VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
                      VK_ACCESS_TRANSFER_READ_BIT, kDepthAccess,
                      VK_PIPELINE_STAGE_TRANSFER_BIT, kDepthStages);
    }

    Target* EnsureTarget(const TargetKey& key, uint32_t width, uint32_t height,
                         uint32_t guestFormat)
    {
        State::LastTarget& last = g.lastTargets[key.depth];
        if (last.target && last.key == key && last.target->width >= width && last.target->height >= height)
            return last.target;
        auto found = g.targets.find(key);
        if (found != g.targets.end() &&
            found->second.width >= width && found->second.height >= height)
        {
            last = { key, &found->second };
            return &found->second;
        }

        if (found != g.targets.end())
        {
            // The surface grew. Rare enough to rebuild rather than plan for.
            RetireBeforeDestroy();
            DestroyTarget(found->second);
            width = std::max(width, found->second.width);
            height = std::max(height, found->second.height);
            g.targets.erase(found);
            g.lastTargets[0] = g.lastTargets[1] = {};
        }

        Target target;
        target.depth = key.depth;
        target.scale = key.scale;
        target.width = width;
        target.height = height;
        target.format = VkFormat(key.format);
        target.guestFormat = guestFormat;
        target.samples = HostSamples(key.samples);
        if (target.format == VK_FORMAT_UNDEFINED) { Skip("render target format"); return nullptr; }

        // TRANSFER_DST because targets are cleared with transfer commands --
        // on a new target, after a resolve, and for the EDRAM clear idiom. An
        // image created without it is laid out on the assumption nothing will
        // ever write it that way.
        const VkImageUsageFlags usage = key.depth
            ? (VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT |
               VK_IMAGE_USAGE_TRANSFER_DST_BIT)
            : (VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT |
               VK_IMAGE_USAGE_TRANSFER_DST_BIT | VK_IMAGE_USAGE_SAMPLED_BIT);
        const VkImageAspectFlags aspect = key.depth
            ? (VK_IMAGE_ASPECT_DEPTH_BIT | VK_IMAGE_ASPECT_STENCIL_BIT)
            : VK_IMAGE_ASPECT_COLOR_BIT;
        const bool sampled = key.depth && g.drawnDepthResolve && target.samples != VK_SAMPLE_COUNT_1_BIT;
        if (!MakeImage(target, usage | (sampled ? VK_IMAGE_USAGE_SAMPLED_BIT : 0), aspect))
        {
            Skip("render target allocation");
            return nullptr;
        }
        if (sampled) MakeSampledDepth(target);

        LOGI("renderer: %s target %ux%u at EDRAM tile %u (%s), %ux for the title's %ux",
             key.depth ? "depth" : "colour", target.width * target.scale, target.height * target.scale,
             key.baseTile,
             key.depth ? gpu::DepthTargetFormatName(guestFormat)
                       : (gpu::ColorTargetFormatName(guestFormat) ?: "unknown"),
             unsigned(target.samples), 1u << key.samples);
        Target& stored = g.targets.emplace(key, target).first->second;
        ClearNewTarget(stored);
        return &stored;
    }

    void BeginRendering(const Target& colour, const Target& depth, uint32_t width,
                        uint32_t height)
    {
        EndPass();
        PassTimesOpen(colour, uint32_t(colour.samples));
        const VkImageView colourView = colour.view, depthView = depth.view;
        // Both are drawn at the pass's scale: their keys carry it.
        const VkExtent2D area = Scaled(VkExtent2D{ width, height }, colour.scale);
        Record([=](VkCommandBuffer command) {
            // What a render pass's dependency in from the outside was: a frame
            // is many passes over the same attachments, with copies and clears
            // between them, and this one waits for their writes.
            VkMemoryBarrier before{ VK_STRUCTURE_TYPE_MEMORY_BARRIER };
            before.srcAccessMask = VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT |
                                   VK_ACCESS_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT |
                                   VK_ACCESS_TRANSFER_WRITE_BIT;
            before.dstAccessMask = kColourAccess | kDepthAccess;
            vkCmdPipelineBarrier(command,
                                 kColourStages | kDepthStages | VK_PIPELINE_STAGE_TRANSFER_BIT,
                                 kColourStages | kDepthStages, 0, 1, &before, 0, nullptr, 0,
                                 nullptr);
            // LOAD, not CLEAR: a frame is many rendering instances -- one per
            // change of target -- and only the title's own draws clear anything.
            VkRenderingAttachmentInfoKHR colourAttachment{
                VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO_KHR };
            colourAttachment.imageView = colourView;
            colourAttachment.imageLayout = VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL;
            colourAttachment.loadOp = VK_ATTACHMENT_LOAD_OP_LOAD;
            colourAttachment.storeOp = VK_ATTACHMENT_STORE_OP_STORE;
            VkRenderingAttachmentInfoKHR depthAttachment = colourAttachment;
            depthAttachment.imageView = depthView;
            depthAttachment.imageLayout = VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL;
            VkRenderingInfoKHR info{ VK_STRUCTURE_TYPE_RENDERING_INFO_KHR };
            info.renderArea.extent = area;
            info.layerCount = 1;
            info.colorAttachmentCount = 1;
            info.pColorAttachments = &colourAttachment;
            info.pDepthAttachment = &depthAttachment;
            info.pStencilAttachment = &depthAttachment;
            dispatch.beginRendering(command, &info);
        });
        g.currentScale = colour.scale;
        OpenGuestQuery();
        g.currentColour = colour.view;
        g.currentDepth = depth.view;
        g.currentSamples = uint32_t(colour.samples);
        g.currentWidth = width;
        g.currentHeight = height;
    }

    // The Xenos clears the EDRAM surface as part of the resolve that reads it
    // (bits 8 and 9 of RB_COPY_CONTROL). Without the clear every frame is drawn
    // on top of the last.
    void ClearAfterResolve(const gpu::RegisterFile& r, const gpu::CopyControl& control)
    {
        if (!control.ClearsColor() && !control.ClearsDepth()) return;

        if (control.ClearsColor())
        {
            const gpu::SurfaceInfo surface{ r[gpu::RB_SURFACE_INFO] };
            const gpu::ColorInfo colour{ r[gpu::RB_COLOR_INFO] };
            if (Target* target = FindTarget({ colour.BaseTile(), surface.Pitch(),
                                              uint32_t(ColourFormatFor(colour.Format())),
                                              surface.MsaaSamples(), false,
                                              PassScale(r[gpu::RB_DEPTH_INFO], surface.MsaaSamples()) }))
            {
                // RB_COLOR_CLEAR is the packed colour in the surface's own format;
                // for the 8_8_8_8 family that is ARGB, most significant byte first.
                const uint32_t packed = r[gpu::RB_COLOR_CLEAR];
                VkClearColorValue value{};
                value.float32[0] = ((packed >> 16) & 0xFF) / 255.0f;
                value.float32[1] = ((packed >>  8) & 0xFF) / 255.0f;
                value.float32[2] = ((packed >>  0) & 0xFF) / 255.0f;
                value.float32[3] = ((packed >> 24) & 0xFF) / 255.0f;
                VkImageSubresourceRange range{ VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 };
                RecordBarrier(target->image, VK_IMAGE_ASPECT_COLOR_BIT,
                              VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                              VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                              VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT, VK_ACCESS_TRANSFER_WRITE_BIT,
                              kColourStages, VK_PIPELINE_STAGE_TRANSFER_BIT);
                const VkImage image = target->image;
                Record([=](VkCommandBuffer command) {
                    vkCmdClearColorImage(command, image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                                         &value, 1, &range);
                });
                RecordBarrier(target->image, VK_IMAGE_ASPECT_COLOR_BIT,
                              VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                              VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                              VK_ACCESS_TRANSFER_WRITE_BIT, kColourAccess,
                              VK_PIPELINE_STAGE_TRANSFER_BIT, kColourStages);
            }
        }

        if (control.ClearsDepth())
        {
            const gpu::SurfaceInfo surface{ r[gpu::RB_SURFACE_INFO] };
            const gpu::DepthInfo depthInfo{ r[gpu::RB_DEPTH_INFO] };
            if (Target* target = FindTarget(DepthKey(depthInfo.BaseTile(), uint32_t(DepthFormatFor(depthInfo.Format())),
                                                     surface.MsaaSamples(),
                                                     PassScale(r[gpu::RB_DEPTH_INFO], surface.MsaaSamples()))))
            {
                // The stored value is 24-bit depth with stencil in the low byte.
                const uint32_t packed = r[gpu::RB_DEPTH_CLEAR];
                VkClearDepthStencilValue value{ DepthClearValue(depthInfo.Format(), packed),
                                                uint32_t(packed & 0xFF) };
                constexpr VkImageAspectFlags kBoth =
                    VK_IMAGE_ASPECT_DEPTH_BIT | VK_IMAGE_ASPECT_STENCIL_BIT;
                VkImageSubresourceRange range{ kBoth, 0, 1, 0, 1 };
                RecordBarrier(target->image, kBoth,
                              VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
                              VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                              VK_ACCESS_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT, VK_ACCESS_TRANSFER_WRITE_BIT,
                              kDepthStages, VK_PIPELINE_STAGE_TRANSFER_BIT);
                const VkImage image = target->image;
                Record([=](VkCommandBuffer command) {
                    vkCmdClearDepthStencilImage(command, image,
                                                VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                                                &value, 1, &range);
                });
                RecordBarrier(target->image, kBoth,
                              VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                              VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
                              VK_ACCESS_TRANSFER_WRITE_BIT, kDepthAccess,
                              VK_PIPELINE_STAGE_TRANSFER_BIT, kDepthStages);
            }
        }
    }

    // One-shot, on the set-up command buffer: a new target leaves UNDEFINED
    // and is cleared before any frame's render pass loads it.
    void ClearNewTarget(Target& target)
    {
        VkCommandBufferBeginInfo begin{ VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO };
        begin.flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
        VkCommandBuffer cmd = g.setup;
        if (vkBeginCommandBuffer(cmd, &begin) != VK_SUCCESS) return;

        const VkImageAspectFlags aspect = target.depth
            ? (VK_IMAGE_ASPECT_DEPTH_BIT | VK_IMAGE_ASPECT_STENCIL_BIT)
            : VK_IMAGE_ASPECT_COLOR_BIT;
        Barrier(cmd, target.image, aspect, VK_IMAGE_LAYOUT_UNDEFINED,
                VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 0, VK_ACCESS_TRANSFER_WRITE_BIT,
                VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT);
        VkImageSubresourceRange range{ aspect, 0, 1, 0, 1 };
        if (target.depth)
        {
            // Zero, not one: a level tests GEQUAL, and RB_DEPTH_CLEAR is zero.
            VkClearDepthStencilValue value{ 0.0f, 0 };
            vkCmdClearDepthStencilImage(cmd, target.image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                                        &value, 1, &range);
        }
        else
        {
            // Not black: a target the title has not drawn into, and a frame that reached
            // the screen without a draw, are then distinguishable from an empty resolve.
            VkClearColorValue value{};
            value.float32[0] = 0.06f; value.float32[1] = 0.07f;
            value.float32[2] = 0.10f; value.float32[3] = 1.0f;
            vkCmdClearColorImage(cmd, target.image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                                 &value, 1, &range);
        }
        Barrier(cmd, target.image, aspect, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                target.depth ? VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL
                             : VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                VK_ACCESS_TRANSFER_WRITE_BIT,
                target.depth ? kDepthAccess : kColourAccess,
                VK_PIPELINE_STAGE_TRANSFER_BIT, target.depth ? kDepthStages : kColourStages);
        vkEndCommandBuffer(cmd);

        VkSubmitInfo submit{ VK_STRUCTURE_TYPE_SUBMIT_INFO };
        submit.commandBufferCount = 1;
        submit.pCommandBuffers = &cmd;
        std::lock_guard lock(vk::pipeline::QueueMutex());
        vkResetFences(g.device, 1, &g.setupFence);
        vkQueueSubmit(g.queue, 1, &submit, g.setupFence);
        vk::pipeline::Failed(vkWaitForFences(g.device, 1, &g.setupFence, VK_TRUE, UINT64_MAX),
                             "the set-up fence");
    }
}

#endif  // MW2_HAVE_VULKAN
