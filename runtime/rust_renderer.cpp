// The C++ renderer's calls into what Rust has taken over of it (build.rs, RUST_OWNS):
// each function is the C++ declaration of its name, defined here to call the Rust
// (src/runtime/gpu/exports.rs), so that no header and no caller changes. Compiled only
// with the rust-renderer feature; CMake's build never sees it.
#if MW2_RUST_RENDERER

// ---- gpu/texture.cpp, gpu/vulkan/texture_formats.cpp (gpu/texture.rs, gpu/vulkan/texture_formats.rs)
#include "gpu/texture.h"
#include "gpu/vulkan/texture_formats.h"

#include <cstddef>

namespace
{
    // TextureData without its bytes, as the Rust fills it (gpu/texture.rs, TextureHeader).
    struct TextureHeader
    {
        bool ok;
        const char* error;
        uint32_t width, height, blocksWide, blocksHigh, bytesPerBlock, layers;
        bool volume, expanded;
        uint32_t minLevel, levelCount;
        gpu::TextureData::Level levels[16];
    };
    static_assert(sizeof(gpu::TextureData::Level) == 40 && offsetof(gpu::TextureData::Level, offset) == 24 &&
                  offsetof(gpu::TextureData::Level, size) == 32);
    static_assert(sizeof(gpu::TextureExtents) == 24);
    static_assert(sizeof(gpu::TextureFormatInfo) == 16 && offsetof(gpu::TextureFormatInfo, known) == 12);
    static_assert(offsetof(TextureHeader, levels) == 56 && sizeof(TextureHeader) == 56 + 16 * 40);

    // A vector's bytes, zeroed, for the Rust to write into. noexcept: an allocation
    // failure ends the program here rather than unwinding through Rust frames.
    uint8_t* Reserve(void* context, size_t size) noexcept
    {
        auto& bytes = *static_cast<std::vector<uint8_t>*>(context);
        bytes.assign(size, 0);
        return bytes.data();
    }
}

extern "C"
{
    bool mw2_texture_read(const uint32_t* fetch, const uint8_t* base, const uint8_t* mips, TextureHeader* out,
                          uint8_t* (*reserve)(void*, size_t), void* context);
    void mw2_texture_extents(const uint32_t* fetch, gpu::TextureExtents* out);
    void mw2_texture_format_of(uint32_t format, gpu::TextureFormatInfo* out);
    const char* mw2_texture_format_name(uint32_t format);
    int32_t mw2_texture_tiled_offset_2d(int32_t x, int32_t y, uint32_t pitch, uint32_t log2);
    int64_t mw2_texture_tiled_offset_3d(int32_t x, int32_t y, int32_t z, uint32_t pitch, uint32_t height, uint32_t log2);
    bool mw2_texture_decode_rgba(bool ok, bool expanded, uint32_t bytesPerBlock, const gpu::TextureData::Level* levels,
                                 size_t levelCount, const uint8_t* bytes, size_t size, uint32_t format, uint32_t level,
                                 uint8_t* (*rgba)(void*, size_t), void* context);
    int mw2_formats_vulkan_format_for(uint32_t format);
    int mw2_formats_srgb_format_for(int format);
    const char* mw2_formats_vulkan_format_name(int format);
    uint32_t mw2_formats_host_swizzle_for(uint32_t format);
    uint32_t mw2_formats_compose_swizzle(uint32_t guest, uint32_t host);
}

const char* gpu::TextureFormatName(uint32_t format) { return mw2_texture_format_name(format); }

gpu::TextureFormatInfo gpu::TextureFormatOf(uint32_t format)
{
    TextureFormatInfo info;
    mw2_texture_format_of(format, &info);
    return info;
}

int32_t gpu::TiledOffset2D(int32_t x, int32_t y, uint32_t pitchAlignedBlocks, uint32_t bytesPerBlockLog2)
{
    return mw2_texture_tiled_offset_2d(x, y, pitchAlignedBlocks, bytesPerBlockLog2);
}

int64_t gpu::TiledOffset3D(int32_t x, int32_t y, int32_t z, uint32_t pitchAlignedBlocks,
                           uint32_t heightAlignedBlocks, uint32_t bytesPerBlockLog2)
{
    return mw2_texture_tiled_offset_3d(x, y, z, pitchAlignedBlocks, heightAlignedBlocks, bytesPerBlockLog2);
}

gpu::TextureExtents gpu::ExtentsOf(const TextureFetch& fetch)
{
    TextureExtents out;
    mw2_texture_extents(fetch.d, &out);
    return out;
}

gpu::TextureData gpu::ReadTexture(const TextureFetch& fetch, const uint8_t* base, const uint8_t* mips)
{
    TextureData out;
    TextureHeader h;
    mw2_texture_read(fetch.d, base, mips, &h, Reserve, &out.bytes);
    out.ok = h.ok;
    out.error = h.error;
    out.width = h.width;
    out.height = h.height;
    out.blocksWide = h.blocksWide;
    out.blocksHigh = h.blocksHigh;
    out.bytesPerBlock = h.bytesPerBlock;
    out.layers = h.layers;
    out.volume = h.volume;
    out.expanded = h.expanded;
    out.minLevel = h.minLevel;
    out.levels.assign(h.levels, h.levels + h.levelCount);
    return out;
}

bool gpu::DecodeToRgba(const TextureData& data, uint32_t format, std::vector<uint8_t>& rgba, uint32_t level)
{
    return mw2_texture_decode_rgba(data.ok, data.expanded, data.bytesPerBlock, data.levels.data(), data.levels.size(),
                                   data.bytes.data(), data.bytes.size(), format, level, Reserve, &rgba);
}

int vk::formats::VulkanFormatFor(uint32_t format) { return mw2_formats_vulkan_format_for(format); }
int vk::formats::SrgbFormatFor(int format) { return mw2_formats_srgb_format_for(format); }
const char* vk::formats::VulkanFormatName(int format) { return mw2_formats_vulkan_format_name(format); }
uint32_t vk::formats::HostSwizzleFor(uint32_t format) { return mw2_formats_host_swizzle_for(format); }
uint32_t vk::formats::ComposeSwizzle(uint32_t guest, uint32_t host) { return mw2_formats_compose_swizzle(guest, host); }

// ---- gpu/vulkan/presenter.cpp, backdrop.cpp, gpu_time.cpp, capture.cpp (gpu/vulkan/presenter.rs,
// backdrop.rs, gpu_time.rs, capture.rs). The backdrop is the presenter's alone and has no forwarder.
#include "gpu/vulkan/capture.h"
#include "gpu/vulkan/gpu_time.h"
#include "gpu/vulkan/presenter.h"

// Handles cross as their Vulkan types (ash's are the same, transparently); VkSubmitInfo
// by pointer, its layout asserted here and in exports.rs.
static_assert(sizeof(VkSubmitInfo) == 72 && offsetof(VkSubmitInfo, commandBufferCount) == 40 &&
              offsetof(VkSubmitInfo, pCommandBuffers) == 48 && offsetof(VkSubmitInfo, pSignalSemaphores) == 64);
static_assert(sizeof(VkImage) == 8 && sizeof(VkFence) == 8 && sizeof(VkQueue) == 8);

extern "C"
{
    bool mw2_presenter_start();
    void mw2_presenter_show_window();
    void mw2_presenter_stop_presenting();
    void mw2_presenter_stop();
    void mw2_presenter_show_image(VkImage image, uint32_t width, uint32_t height, uint64_t serial);
    void mw2_presenter_wait_until_taken(uint64_t serial);
    uint64_t mw2_presenter_guest_blank_period();
    bool mw2_presenter_running();
    uint64_t mw2_presenter_presented_frames();
    void mw2_gpu_time_start(VkPhysicalDevice physical, VkDevice device, VkQueue queue, uint32_t family);
    void mw2_gpu_time_stop();
    VkResult mw2_gpu_time_submit(PFN_vkQueueSubmit queueSubmit, VkQueue queue, const VkSubmitInfo* submit, VkFence fence);
    void mw2_gpu_time_collect();
    void mw2_capture_frame_boundary(bool worldDrawn, uint64_t frame);
    void mw2_capture_request_now(uint32_t frames);
    void mw2_capture_toggle_stream();
    void mw2_capture_report();
}

bool vk::Start() { return mw2_presenter_start(); }
void vk::ShowWindow() { mw2_presenter_show_window(); }
void vk::StopPresenting() { mw2_presenter_stop_presenting(); }
void vk::Stop() { mw2_presenter_stop(); }
void vk::ShowImage(void* image, uint32_t width, uint32_t height, uint64_t serial)
{
    mw2_presenter_show_image(static_cast<VkImage>(image), width, height, serial);
}
void vk::WaitUntilTaken(uint64_t serial) { mw2_presenter_wait_until_taken(serial); }
uint64_t vk::GuestBlankPeriod() { return mw2_presenter_guest_blank_period(); }
bool vk::Running() { return mw2_presenter_running(); }
uint64_t vk::PresentedFrames() { return mw2_presenter_presented_frames(); }

void vk::gpu_time::Start(VkPhysicalDevice physical, VkDevice device, VkQueue queue, uint32_t family)
{
    mw2_gpu_time_start(physical, device, queue, family);
}
void vk::gpu_time::Stop() { mw2_gpu_time_stop(); }
// The renderer's submissions go through the loader's vkQueueSubmit, as they did.
VkResult vk::gpu_time::Submit(VkQueue queue, const VkSubmitInfo& submit, VkFence fence)
{
    return mw2_gpu_time_submit(vkQueueSubmit, queue, &submit, fence);
}
void vk::gpu_time::Collect() { mw2_gpu_time_collect(); }

void vk::capture::FrameBoundary(bool worldDrawn, uint64_t frame) { mw2_capture_frame_boundary(worldDrawn, frame); }
void vk::capture::RequestNow(unsigned frames) { mw2_capture_request_now(frames); }
void vk::capture::ToggleStream() { mw2_capture_toggle_stream(); }
void vk::capture::Report() { mw2_capture_report(); }

// ---- gpu/gpu.cpp, gpu/arena.cpp (gpu/gpu.rs, gpu/arena.rs). What the command processor,
// the census, the image pool and the GPU capture call; the rest of gpu.h, and
// d3d_hooks.cpp (src/hooks/d3d_hooks.rs), only Rust calls.
#include "gpu/gpu.h"
#include "gpu/internal.h"

extern "C"
{
    uint64_t mw2_gpu_frames();
    bool mw2_gpu_consuming();
    bool mw2_gpu_running();
    double mw2_gpu_seconds();
    void mw2_gpu_raise_cp_interrupt(uint32_t cpuMask);
    void mw2_gpu_ring_position(uint32_t* read, uint32_t* written, uint32_t* writeBack);
    bool mw2_arena_progress(uint32_t address, uint32_t value);
#if MW2_DIAGNOSTICS
    void mw2_gpu_ring_layout(uint32_t* base, uint32_t* dwords, uint32_t* writeBack);
    void mw2_arena_save(gpu::detail::ArenaState* out);
    void mw2_arena_restore(const gpu::detail::ArenaState* state);
    void mw2_arena_set_block(uint32_t physical);
#endif
}

uint64_t gpu::FrameCount() { return mw2_gpu_frames(); }
bool gpu::Consuming() { return mw2_gpu_consuming(); }
bool gpu::detail::Running() { return mw2_gpu_running(); }
double gpu::detail::Seconds() { return mw2_gpu_seconds(); }
void gpu::detail::RaiseCommandProcessorInterrupt(uint32_t cpuMask) { mw2_gpu_raise_cp_interrupt(cpuMask); }
void gpu::detail::RingPosition(uint32_t& read, uint32_t& written, uint32_t& writeBack)
{
    mw2_gpu_ring_position(&read, &written, &writeBack);
}
bool gpu::detail::ArenaProgress(uint32_t address, uint32_t value) { return mw2_arena_progress(address, value); }

#if MW2_DIAGNOSTICS
// The four words gpu/arena.rs's ArenaState has.
static_assert(sizeof(gpu::detail::ArenaState) == 16 && offsetof(gpu::detail::ArenaState, havePosition) == 12);

void gpu::detail::RingLayout(uint32_t& base, uint32_t& dwords, uint32_t& writeBack)
{
    mw2_gpu_ring_layout(&base, &dwords, &writeBack);
}
gpu::detail::ArenaState gpu::detail::SaveArena()
{
    ArenaState state;
    mw2_arena_save(&state);
    return state;
}
void gpu::detail::RestoreArena(const ArenaState& state) { mw2_arena_restore(&state); }
void gpu::detail::SetArenaBlock(uint32_t physical) { mw2_arena_set_block(physical); }
#endif

#endif  // MW2_RUST_RENDERER
