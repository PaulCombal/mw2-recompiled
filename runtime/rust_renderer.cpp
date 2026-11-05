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

#endif  // MW2_RUST_RENDERER
