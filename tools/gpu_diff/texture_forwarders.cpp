// The Rust-renderer build's C++ forwarders (runtime/rust_renderer.cpp), compiled again
// under namespaces of their own so that they can sit beside the C++ originals the rig
// compares against: what the texture cache calls in the game, run in the rig.
#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <vector>

#define MW2_RUST_RENDERER 1
#define gpu gpu_fwd
#define vk vk_fwd
#include "../../runtime/rust_renderer.cpp"
#undef vk
#undef gpu

// TextureData and its fields are the same declarations in either namespace, so the
// rig reads the forwarders' results as its own types.
extern "C"
{
    void* fwd_read(const uint32_t* fetch, const uint8_t* base, const uint8_t* mips)
    {
        gpu_fwd::TextureFetch f;
        std::memcpy(f.d, fetch, sizeof f.d);
        return new gpu_fwd::TextureData(gpu_fwd::ReadTexture(f, base, mips));
    }
    void fwd_free(void* data) { delete static_cast<gpu_fwd::TextureData*>(data); }
    bool fwd_decode(const void* data, uint32_t format, std::vector<uint8_t>* rgba, uint32_t level)
    {
        return gpu_fwd::DecodeToRgba(*static_cast<const gpu_fwd::TextureData*>(data), format, *rgba, level);
    }
    void fwd_extents(const uint32_t* fetch, void* out)
    {
        gpu_fwd::TextureFetch f;
        std::memcpy(f.d, fetch, sizeof f.d);
        *static_cast<gpu_fwd::TextureExtents*>(out) = gpu_fwd::ExtentsOf(f);
    }
    void fwd_format_of(uint32_t format, void* out) { *static_cast<gpu_fwd::TextureFormatInfo*>(out) = gpu_fwd::TextureFormatOf(format); }
    const char* fwd_format_name(uint32_t format) { return gpu_fwd::TextureFormatName(format); }
    int32_t fwd_tiled_2d(int32_t x, int32_t y, uint32_t p, uint32_t l) { return gpu_fwd::TiledOffset2D(x, y, p, l); }
    int64_t fwd_tiled_3d(int32_t x, int32_t y, int32_t z, uint32_t p, uint32_t h, uint32_t l)
    {
        return gpu_fwd::TiledOffset3D(x, y, z, p, h, l);
    }
    int fwd_vulkan_format_for(uint32_t f) { return vk_fwd::formats::VulkanFormatFor(f); }
    int fwd_srgb_format_for(int f) { return vk_fwd::formats::SrgbFormatFor(f); }
    const char* fwd_vulkan_format_name(int f) { return vk_fwd::formats::VulkanFormatName(f); }
    uint32_t fwd_host_swizzle_for(uint32_t f) { return vk_fwd::formats::HostSwizzleFor(f); }
    uint32_t fwd_compose_swizzle(uint32_t g, uint32_t h) { return vk_fwd::formats::ComposeSwizzle(g, h); }
}
