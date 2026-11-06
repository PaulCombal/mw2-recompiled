// runtime/gpu/vulkan/backdrop.cpp as it is, compiled into the rig: its two shaders, and
// Record run on stand-in Vulkan calls (presenter_stubs.cpp) for the calls it makes, the
// push constants it gives and the blur it keeps; the Rust Backdrop::record on the same
// stand-ins (backdrop.rs, mod rig), read the same way.
#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <functional>
#include <iterator>
#include <utility>
#include <vector>

#include "presenter_rig.h"

#include "diagnostics.h"

#define vk vkb
#include "../../runtime/gpu/vulkan/backdrop.cpp"
#undef vk

namespace
{
    template <class T> T Fake(uint64_t value) { return reinterpret_cast<T>(uintptr_t(value)); }
}

extern "C"
{
    size_t rig_cpp_backdrop_shader(bool fragment, uint32_t* out, size_t capacity)
    {
        const std::vector<uint32_t> words = fragment ? FragmentShader() : VertexShader();
        std::copy_n(words.begin(), std::min(capacity, words.size()), out);
        return words.size();
    }

    // As after Create: a pipeline and its sets, no blur.
    void rig_cpp_backdrop_reset()
    {
        g = State{};
        g.device = Fake<VkDevice>(0x9000);
        g.physical = Fake<VkPhysicalDevice>(0x9001);
        g.format = VK_FORMAT_B8G8R8A8_UNORM;
        g.pipeline = Fake<VkPipeline>(0x9002);
        g.layout = Fake<VkPipelineLayout>(0x9003);
        g.sampler = Fake<VkSampler>(0x9004);
        for (uint32_t i = 0; i < kSlots; i++) g.sets[i] = Fake<VkDescriptorSet>(0x9010 + i);
        g.beginRendering = rig_vkCmdBeginRenderingKHR;
        g.endRendering = rig_vkCmdEndRenderingKHR;
    }

    struct RigDraw
    {
        uint32_t frameWidth, frameHeight;
        int32_t frame[4], picture[4];
        uint32_t extent[2];
        float outline, shown;
    };
    struct Recorded
    {
        float push[16];
        uint32_t pushes, settles, levels;
        uint32_t level[8];
        uint32_t madeWidth, madeHeight, hasImage;
    };

    void rig_cpp_backdrop_record(const RigDraw* d, bool hasFrame, uint32_t slot, Recorded* out)
    {
        uint32_t settles = 0;
        vkb::backdrop::Draw draw{};
        draw.commands = Fake<VkCommandBuffer>(0x9100);
        draw.slot = slot;
        draw.target = Fake<VkImage>(0x9101);
        draw.extent = { d->extent[0], d->extent[1] };
        draw.frame = hasFrame ? Fake<VkImage>(0x9102) : VK_NULL_HANDLE;
        draw.frameWidth = d->frameWidth;
        draw.frameHeight = d->frameHeight;
        draw.frameFrom = { d->frame[0], d->frame[1] };
        draw.frameTo = { d->frame[2], d->frame[3] };
        draw.picture = Fake<VkImageView>(0x9103);
        draw.pictureFrom = { d->picture[0], d->picture[1] };
        draw.pictureTo = { d->picture[2], d->picture[3] };
        draw.outline = d->outline;
        draw.shown = d->shown;
        draw.settle = [&] { settles++; };
        const uint32_t pushes = rig::s.pushes;
        std::fill(std::begin(rig::s.push), std::end(rig::s.push), 0.0f);
        vkb::backdrop::Record(draw);
        *out = {};
        std::memcpy(out->push, rig::s.push, sizeof out->push);
        out->pushes = rig::s.pushes - pushes;
        out->settles = settles;
        out->levels = g.blur.levels;
        for (uint32_t i = 0; i < kLevels; i++)
        {
            out->level[2 * i] = g.blur.level[i].width;
            out->level[2 * i + 1] = g.blur.level[i].height;
        }
        out->madeWidth = g.blur.frameWidth;
        out->madeHeight = g.blur.frameHeight;
        out->hasImage = g.blur.image != VK_NULL_HANDLE;
    }

    void* mw2_rig_backdrop_new();
    void mw2_rig_backdrop_free(void* backdrop);
    struct RustRecorded
    {
        uint32_t settles, levels;
        uint32_t level[8];
        uint32_t madeWidth, madeHeight, hasImage;
    };
    void mw2_rig_backdrop_record(void* backdrop, const RigDraw* d, bool hasFrame, uint32_t slot, RustRecorded* out);

    // As rig_cpp_backdrop_reset, a new Rust backdrop.
    void* rig_rust_backdrop_new() { return mw2_rig_backdrop_new(); }
    void rig_rust_backdrop_free(void* backdrop) { mw2_rig_backdrop_free(backdrop); }

    void rig_rust_backdrop_record(void* backdrop, const RigDraw* d, bool hasFrame, uint32_t slot, Recorded* out)
    {
        const uint32_t pushes = rig::s.pushes;
        std::fill(std::begin(rig::s.push), std::end(rig::s.push), 0.0f);
        RustRecorded r{};
        mw2_rig_backdrop_record(backdrop, d, hasFrame, slot, &r);
        *out = {};
        std::memcpy(out->push, rig::s.push, sizeof out->push);
        out->pushes = rig::s.pushes - pushes;
        out->settles = r.settles;
        out->levels = r.levels;
        std::memcpy(out->level, r.level, sizeof out->level);
        out->madeWidth = r.madeWidth;
        out->madeHeight = r.madeHeight;
        out->hasImage = r.hasImage;
    }
}
