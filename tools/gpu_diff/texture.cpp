// tools/gpu_diff/texture.cpp: the texture layout, untiler and format tables, C++
// (runtime/gpu/texture.cpp, vulkan/texture_formats.cpp as they are) against the Rust
// through its C interface (src/runtime/gpu/exports.rs), on generated cases.
//
//   texture <seed> <cases> [tiling|texture|formats|bench]...
//
// Exits non-zero at the first difference, printing the case.
#include "gpu/texture.h"
#include "gpu/vulkan/texture_formats.h"

#include <chrono>
#include <cinttypes>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <sys/mman.h>
#include <vector>

namespace
{
    struct Header
    {
        bool ok;
        const char* error;
        uint32_t width, height, blocksWide, blocksHigh, bytesPerBlock, layers;
        bool volume, expanded;
        uint32_t minLevel, levelCount;
        gpu::TextureData::Level levels[16];
    };
    static_assert(sizeof(gpu::TextureData::Level) == 40);
    static_assert(offsetof(gpu::TextureData::Level, offset) == 24);
    static_assert(sizeof(gpu::TextureExtents) == 24);

    uint8_t* Reserve(void* context, size_t size)
    {
        auto& bytes = *static_cast<std::vector<uint8_t>*>(context);
        bytes.assign(size, 0);
        return bytes.data();
    }
}

// The forwarders of the Rust-renderer build (texture_forwarders.cpp).
extern "C"
{
    void* fwd_read(const uint32_t* fetch, const uint8_t* base, const uint8_t* mips);
    void fwd_free(void* data);
    bool fwd_decode(const void* data, uint32_t format, std::vector<uint8_t>* rgba, uint32_t level);
    void fwd_extents(const uint32_t* fetch, void* out);
    void fwd_format_of(uint32_t format, void* out);
    const char* fwd_format_name(uint32_t format);
    int32_t fwd_tiled_2d(int32_t x, int32_t y, uint32_t p, uint32_t l);
    int64_t fwd_tiled_3d(int32_t x, int32_t y, int32_t z, uint32_t p, uint32_t h, uint32_t l);
    int fwd_vulkan_format_for(uint32_t f);
    int fwd_srgb_format_for(int f);
    const char* fwd_vulkan_format_name(int f);
    uint32_t fwd_host_swizzle_for(uint32_t f);
    uint32_t fwd_compose_swizzle(uint32_t g, uint32_t h);
}

extern "C"
{
    void gpu_reach_measure(const uint32_t* fetch, bool withBase, bool withMips, uint64_t* baseEnd, uint64_t* mipsEnd);
    bool mw2_texture_read(const uint32_t* fetch, const uint8_t* base, const uint8_t* mips, Header* out,
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

namespace
{
    uint64_t g_state;
    uint32_t Next()
    {
        g_state = g_state * 6364136223846793005ull + 1442695040888963407ull;
        return uint32_t(g_state >> 33);
    }
    uint32_t Below(uint32_t n) { return n ? Next() % n : 0; }

    std::string g_case;
    [[noreturn]] void Fail(const char* what)
    {
        std::printf("DIFFERENCE: %s\n  case: %s\n", what, g_case.c_str());
        std::exit(1);
    }
#define CHECK(cond, what) do { if (!(cond)) Fail(what); } while (0)

    bool SameText(const char* a, const char* b)
    {
        if (!a || !b) return a == b;
        return std::strcmp(a, b) == 0;
    }

    // ---- tiling ------------------------------------------------------------------
    void Tiling()
    {
        uint64_t n = 0;
        for (uint32_t pitch = 32; pitch <= 8192; pitch += 32)
            for (uint32_t log2 = 0; log2 <= 4; log2++)
                for (int32_t y = 0; y < 72; y++)
                    for (int32_t x = 0; x < int32_t(pitch); x++)
                    {
                        const int32_t a = gpu::TiledOffset2D(x, y, pitch, log2);
                        const int32_t b = mw2_texture_tiled_offset_2d(x, y, pitch, log2);
                        if (a != b || (x % 7 == 0 && fwd_tiled_2d(x, y, pitch, log2) != a))
                        {
                            g_case = "2d x=" + std::to_string(x) + " y=" + std::to_string(y) + " pitch=" +
                                     std::to_string(pitch) + " log2=" + std::to_string(log2);
                            Fail("TiledOffset2D");
                        }
                        n++;
                    }
        for (uint32_t pitch = 32; pitch <= 1024; pitch += 32)
            for (uint32_t height = 16; height <= 256; height += 16)
                for (uint32_t log2 = 0; log2 <= 4; log2++)
                    for (int32_t z = 0; z < 12; z++)
                        for (int32_t y = 0; y < int32_t(height); y += (height > 64 ? 3 : 1))
                            for (int32_t x = 0; x < int32_t(pitch); x += (pitch > 128 ? 5 : 1))
                            {
                                const int64_t a = gpu::TiledOffset3D(x, y, z, pitch, height, log2);
                                const int64_t b = mw2_texture_tiled_offset_3d(x, y, z, pitch, height, log2);
                                if (a != b || (x % 7 == 0 && fwd_tiled_3d(x, y, z, pitch, height, log2) != a))
                                {
                                    g_case = "3d x=" + std::to_string(x) + " y=" + std::to_string(y) + " z=" +
                                             std::to_string(z) + " pitch=" + std::to_string(pitch) + " height=" +
                                             std::to_string(height) + " log2=" + std::to_string(log2);
                                    Fail("TiledOffset3D");
                                }
                                n++;
                            }
        // Out of range, negative and huge coordinates: the arithmetic wraps the same.
        for (int i = 0; i < 2000000; i++)
        {
            const int32_t x = int32_t(Next()), y = int32_t(Next()), z = int32_t(Next());
            const uint32_t pitch = Next(), height = Next(), log2 = Below(5);
            if (gpu::TiledOffset2D(x, y, pitch, log2) != mw2_texture_tiled_offset_2d(x, y, pitch, log2) ||
                gpu::TiledOffset3D(x, y, z, pitch, height, log2) != mw2_texture_tiled_offset_3d(x, y, z, pitch, height, log2))
            {
                g_case = "wide x=" + std::to_string(x) + " y=" + std::to_string(y) + " z=" + std::to_string(z) +
                         " pitch=" + std::to_string(pitch) + " height=" + std::to_string(height);
                Fail("tiled offsets out of range");
            }
            n++;
        }
        // The light grid: 512x256x4, four bytes a block, 524288 distinct addresses over 2 MB.
        std::vector<uint8_t> seen(2u << 20, 0);
        uint32_t distinct = 0;
        int64_t top = 0;
        for (int32_t z = 0; z < 4; z++)
            for (int32_t y = 0; y < 256; y++)
                for (int32_t x = 0; x < 512; x++)
                {
                    const int64_t at = mw2_texture_tiled_offset_3d(x, y, z, 512, 256, 2);
                    top = std::max(top, at + 4);
                    CHECK(at >= 0 && at + 4 <= int64_t(seen.size()), "light grid address out of 2 MB");
                    if (!seen[at]) distinct++;
                    seen[at] = 1;
                }
        CHECK(distinct == 524288 && top == (2 << 20), "light grid: not 524288 distinct addresses over 2 MB");
        std::printf("tiling: %" PRIu64 " offsets the same; the light grid 524288 distinct over 2 MB\n", n);
    }

    // ---- formats -----------------------------------------------------------------
    void Formats()
    {
        g_case = "self-check";
        CHECK(vk::formats::VulkanFormatFor(0x12) != 0, "the C++ side is the no-Vulkan stub");
        for (uint32_t f = 0; f < 300; f++)
        {
            g_case = "format " + std::to_string(f);
            CHECK(vk::formats::VulkanFormatFor(f) == mw2_formats_vulkan_format_for(f), "VulkanFormatFor");
            CHECK(vk::formats::HostSwizzleFor(f) == mw2_formats_host_swizzle_for(f), "HostSwizzleFor");
            CHECK(SameText(gpu::TextureFormatName(f), mw2_texture_format_name(f)), "TextureFormatName");
            CHECK(vk::formats::VulkanFormatFor(f) == fwd_vulkan_format_for(f), "forwarded VulkanFormatFor");
            CHECK(vk::formats::HostSwizzleFor(f) == fwd_host_swizzle_for(f), "forwarded HostSwizzleFor");
            CHECK(SameText(gpu::TextureFormatName(f), fwd_format_name(f)), "forwarded TextureFormatName");
            gpu::TextureFormatInfo a = gpu::TextureFormatOf(f), b{}, c{};
            b.bitsPerBlock = 77;
            c.bitsPerBlock = 77;
            mw2_texture_format_of(f, &b);
            fwd_format_of(f, &c);
            CHECK(a.bitsPerBlock == b.bitsPerBlock && a.blockWidth == b.blockWidth &&
                  a.blockHeight == b.blockHeight && a.known == b.known, "TextureFormatOf");
            CHECK(std::memcmp(&b, &c, sizeof b) == 0, "forwarded TextureFormatOf");
        }
        for (int v = -5; v < 2000; v++)
        {
            g_case = "vkformat " + std::to_string(v);
            CHECK(vk::formats::SrgbFormatFor(v) == mw2_formats_srgb_format_for(v), "SrgbFormatFor");
            CHECK(SameText(vk::formats::VulkanFormatName(v), mw2_formats_vulkan_format_name(v)), "VulkanFormatName");
            CHECK(vk::formats::SrgbFormatFor(v) == fwd_srgb_format_for(v), "forwarded SrgbFormatFor");
            CHECK(SameText(vk::formats::VulkanFormatName(v), fwd_vulkan_format_name(v)), "forwarded VulkanFormatName");
        }
        const uint32_t hosts[] = { 0u, 0u | (1u << 3) | (1u << 6) | (1u << 9), 0u | (1u << 3) | (2u << 6) | (2u << 9),
                                   0u | (1u << 3) | (2u << 6) | (3u << 9), 0x688 };
        for (uint32_t guest = 0; guest < 4096; guest++)
            for (uint32_t host : hosts)
            {
                g_case = "swizzle " + std::to_string(guest) + " host " + std::to_string(host);
                CHECK(vk::formats::ComposeSwizzle(guest, host) == mw2_formats_compose_swizzle(guest, host), "ComposeSwizzle");
                CHECK(vk::formats::ComposeSwizzle(guest, host) == fwd_compose_swizzle(guest, host), "forwarded ComposeSwizzle");
            }
        for (int i = 0; i < 1000000; i++)
        {
            const uint32_t guest = Next(), host = Next();
            g_case = "swizzle " + std::to_string(guest) + " host " + std::to_string(host);
            CHECK(vk::formats::ComposeSwizzle(guest, host) == mw2_formats_compose_swizzle(guest, host), "ComposeSwizzle random");
        }
        std::printf("formats: every id, every Vulkan value to 2000, every swizzle the same\n");
    }

    // ---- textures ----------------------------------------------------------------
    constexpr size_t kRegion = 256u << 20;
    uint8_t* g_memory;

    void MakeMemory()
    {
        // The region, then a guard page: a read past what the extents name faults.
        const size_t page = 4096;
        void* m = mmap(nullptr, kRegion + page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (m == MAP_FAILED) { std::perror("mmap"); std::exit(2); }
        g_memory = static_cast<uint8_t*>(m);
        mprotect(g_memory + kRegion, page, PROT_NONE);
        uint64_t s = 0x9E3779B97F4A7C15ull;
        for (size_t i = 0; i < kRegion; i += 8)
        {
            s = s * 6364136223846793005ull + 1442695040888963407ull;
            std::memcpy(g_memory + i, &s, 8);
        }
    }

    uint32_t Field(uint32_t& word, uint32_t value, uint32_t lo, uint32_t count)
    {
        const uint32_t mask = (count >= 32 ? ~0u : ((1u << count) - 1u)) << lo;
        word = (word & ~mask) | ((value << lo) & mask);
        return value;
    }

    // A fetch constant shaped like the ones titles write, with every field the layout
    // reads varied, and a share of plain random words.
    gpu::TextureFetch MakeFetch(uint32_t kind)
    {
        gpu::TextureFetch f;
        for (uint32_t& d : f.d) d = Next();
        if (kind % 16 == 15) return f;   // raw
        Field(f.d[0], 2, 0, 2);
        static const uint32_t common[] = { 18, 19, 20, 6, 2, 10, 49, 58, 59, 4, 15, 26, 32, 36, 38, 7, 24, 25, 31,
                                           57, 11, 0, 1, 60, 61, 35 };
        const uint32_t format = Below(3) ? common[Below(sizeof common / sizeof common[0])] : Below(64);
        Field(f.d[1], format, 0, 6);
        const uint32_t dimension = Below(10) < 6 ? 1 : Below(4);
        Field(f.d[5], dimension, 9, 2);
        uint32_t w, h, d = 1;
        switch (Below(6))
        {
        case 0: w = 1 + Below(8); h = 1 + Below(8); break;
        case 1: w = 1u << Below(11); h = 1u << Below(11); break;
        case 2: w = 1 + Below(600); h = 1 + Below(600); break;
        case 3: w = 17; h = 9; break;
        case 4: w = 128; h = 16; break;
        default: w = 1 + Below(2048); h = 1 + Below(64); break;
        }
        if (dimension == 2) { w = std::min(w, 256u); h = std::min(h, 256u); d = 1 + Below(Below(4) ? 8 : 64); }
        if (dimension == 3) { h = w = std::min(w, 512u); }
        if (dimension == 0) { w = 1 + Below(Below(2) ? 64 : 1u << 14); h = 1; }
        if (dimension == 2 && Below(8) == 0) { w = 512; h = 256; d = 4; }
        if (dimension == 0) Field(f.d[2], w - 1, 0, 24);
        else if (dimension == 2) { Field(f.d[2], w - 1, 0, 11); Field(f.d[2], h - 1, 11, 11); Field(f.d[2], d - 1, 22, 10); }
        else { Field(f.d[2], w - 1, 0, 13); Field(f.d[2], h - 1, 13, 13); }
        // Stacked 2D: up to 64 slices.
        if (dimension == 1)
        {
            const bool stacked = Below(5) == 0;
            Field(f.d[1], stacked, 10, 1);
            if (stacked) Field(f.d[2], Below(Below(3) ? 4 : 64), 26, 6);
        }
        // Pitch: 0, the width rounded, or anything.
        const uint32_t pitchChoice = Below(4);
        const uint32_t pitch = pitchChoice == 0 ? 0 : pitchChoice == 3 ? Below(512) : ((w + 31) >> 5);
        Field(f.d[0], pitch, 22, 9);
        Field(f.d[0], Below(4) != 0, 31, 1);           // tiled
        Field(f.d[1], Below(4), 6, 2);                   // endian
        Field(f.d[5], Below(3) != 0, 11, 1);             // packed mips
        Field(f.d[4], Below(Below(2) ? 3 : 16), 2, 4);   // min level
        Field(f.d[4], Below(16), 6, 4);                  // max level
        const uint32_t base = Below(10) == 0 ? 0 : 1 + Below(0xFFFFF);
        uint32_t mip = Below(4) == 0 ? 0 : Below(4) == 0 ? base : 1 + Below(0xFFFFF);
        Field(f.d[1], base, 12, 20);
        Field(f.d[5], mip, 12, 20);
        return f;
    }

    void CompareRead(const gpu::TextureFetch& fetch, const uint8_t* base, const uint8_t* mips)
    {
        const gpu::TextureData a = gpu::ReadTexture(fetch, base, mips);
        Header h{};
        std::vector<uint8_t> bytes;
        const bool okB = mw2_texture_read(fetch.d, base, mips, &h, Reserve, &bytes);
        CHECK(okB == h.ok, "returned ok differs from the header's");
        CHECK(a.ok == h.ok, "ok");
        CHECK(SameText(a.error, h.error), "error");
        CHECK(a.width == h.width && a.height == h.height && a.blocksWide == h.blocksWide &&
              a.blocksHigh == h.blocksHigh && a.bytesPerBlock == h.bytesPerBlock && a.layers == h.layers &&
              a.volume == h.volume && a.expanded == h.expanded && a.minLevel == h.minLevel, "header fields");
        CHECK(a.levels.size() == h.levelCount, "level count");
        for (size_t i = 0; i < a.levels.size(); i++)
        {
            const auto& x = a.levels[i];
            const auto& y = h.levels[i];
            CHECK(x.width == y.width && x.height == y.height && x.depth == y.depth && x.blocksWide == y.blocksWide &&
                  x.blocksHigh == y.blocksHigh && x.layers == y.layers && x.offset == y.offset && x.size == y.size,
                  "level fields");
        }
        CHECK(a.bytes.size() == bytes.size(), "byte count");
        CHECK(a.bytes.empty() || std::memcmp(a.bytes.data(), bytes.data(), bytes.size()) == 0, "bytes");
        // The forwarder the texture cache calls: every TextureData field.
        auto* f = static_cast<gpu::TextureData*>(fwd_read(fetch.d, base, mips));
        CHECK(f->ok == a.ok && SameText(f->error, a.error) && f->width == a.width && f->height == a.height &&
              f->blocksWide == a.blocksWide && f->blocksHigh == a.blocksHigh && f->bytesPerBlock == a.bytesPerBlock &&
              f->layers == a.layers && f->volume == a.volume && f->expanded == a.expanded && f->minLevel == a.minLevel,
              "forwarded header fields");
        CHECK(f->levels.size() == a.levels.size(), "forwarded level count");
        for (size_t i = 0; i < a.levels.size(); i++)
        {
            const auto& x = a.levels[i];
            const auto& y = f->levels[i];
            CHECK(x.width == y.width && x.height == y.height && x.depth == y.depth && x.blocksWide == y.blocksWide &&
                  x.blocksHigh == y.blocksHigh && x.layers == y.layers && x.offset == y.offset && x.size == y.size,
                  "forwarded level fields");
        }
        CHECK(f->bytes == a.bytes, "forwarded bytes");
        // The decoder, over every level, on the same data.
        for (uint32_t level = 0; level <= a.levels.size(); level++)
        {
            std::vector<uint8_t> ra(3, 7), rb(3, 7);
            const bool da = gpu::DecodeToRgba(a, fetch.Format(), ra, level);
            const bool db = mw2_texture_decode_rgba(a.ok, a.expanded, a.bytesPerBlock, a.levels.data(), a.levels.size(),
                                                    a.bytes.data(), a.bytes.size(), fetch.Format(), level, Reserve, &rb);
            CHECK(da == db, "DecodeToRgba result");
            CHECK(ra == rb, "DecodeToRgba pixels");
            std::vector<uint8_t> rc(3, 7);
            CHECK(fwd_decode(f, fetch.Format(), &rc, level) == da && rc == ra, "forwarded DecodeToRgba");
        }
        fwd_free(f);
    }

    uint64_t g_bytes;
    uint64_t g_pastExtents;

    void Texture(uint32_t cases)
    {
        uint64_t read = 0, failed = 0;
        for (uint32_t c = 0; c < cases; c++)
        {
            const gpu::TextureFetch fetch = MakeFetch(c);
            char text[160];
            std::snprintf(text, sizeof text, "%u: %08X %08X %08X %08X %08X %08X", c, fetch.d[0], fetch.d[1], fetch.d[2],
                          fetch.d[3], fetch.d[4], fetch.d[5]);
            g_case = text;
            const gpu::TextureExtents a = gpu::ExtentsOf(fetch);
            gpu::TextureExtents b{ 1, 2, 3, 4, 5, 6 };
            mw2_texture_extents(fetch.d, &b);
            CHECK(std::memcmp(&a, &b, sizeof a) == 0, "ExtentsOf");
            gpu::TextureExtents e{ 1, 2, 3, 4, 5, 6 };
            fwd_extents(fetch.d, &e);
            CHECK(std::memcmp(&a, &e, sizeof a) == 0, "forwarded ExtentsOf");
            // Where the two ranges are: inside the region, against its end where they
            // fit, so that a read past them would fault.
            if (uint64_t(a.baseBytes) > kRegion || uint64_t(a.mipBytes) > kRegion) { failed++; continue; }
            // Each range against its guard page by how far the C++ reader really reads
            // (tools/gpu_diff/texture_reach.cpp), which can be past what ExtentsOf names: a
            // tiled slice under 4 KB reaches past rows x pitch, a 1D tail's packed levels
            // past its one row. A read past the reach faults.
            const bool withBase = a.baseBytes && Below(12) != 0, withMips = a.mipBytes && Below(12) != 0;
            uint64_t baseReach = 0, mipsReach = 0;
            gpu_reach_measure(fetch.d, withBase, withMips, &baseReach, &mipsReach);
            if (baseReach > a.baseBytes || mipsReach > a.mipBytes) g_pastExtents++;
            // A quarter of the region at most: a DXT texture decodes to eight times its
            // bytes in RGBA, on both sides, inside the run's memory cap.
            if (baseReach + mipsReach > kRegion / 4) { failed++; continue; }
            // A range not read is null, or, where ExtentsOf names none, any pointer.
            auto place = [&](bool with, uint32_t named, uint64_t bytes) -> const uint8_t* {
                if (!with) return !named && Below(2) ? g_memory : nullptr;
                const size_t room = kRegion - bytes;
                return g_memory + (Below(3) == 0 ? room : (size_t(Next()) * 16) % (room + 1) & ~size_t(15));
            };
            const uint8_t* base = place(withBase, a.baseBytes, baseReach);
            const uint8_t* mips = place(withMips, a.mipBytes, mipsReach);
            CompareRead(fetch, base, mips);
            read++;
        }
        std::printf("texture: %" PRIu64 " fetches read the same (%" PRIu64 " too large to place); the reader went past"
                    " ExtentsOf on %" PRIu64 "\n", read, failed, g_pastExtents);
    }

    // ---- bench -------------------------------------------------------------------
    void Bench()
    {
        struct Class { const char* name; uint32_t format, w, h, d, dim; bool tiled; };
        const Class classes[] = {
            { "tiled bpb 1", 2, 1024, 1024, 1, 1, true },   { "tiled bpb 2", 4, 1024, 1024, 1, 1, true },
            { "tiled bpb 4", 6, 1024, 1024, 1, 1, true },   { "tiled bpb 8", 18, 2048, 2048, 1, 1, true },
            { "tiled bpb 16", 20, 2048, 2048, 1, 1, true }, { "linear", 6, 1024, 1024, 1, 1, false },
            { "volume", 6, 128, 128, 32, 2, true },         { "DXT3A", 58, 2048, 2048, 1, 1, true },
        };
        for (const Class& k : classes)
        {
            gpu::TextureFetch f;
            Field(f.d[0], 2, 0, 2);
            Field(f.d[1], k.format, 0, 6);
            Field(f.d[1], 2, 6, 2);
            Field(f.d[5], k.dim, 9, 2);
            if (k.dim == 2) { Field(f.d[2], k.w - 1, 0, 11); Field(f.d[2], k.h - 1, 11, 11); Field(f.d[2], k.d - 1, 22, 10); }
            else { Field(f.d[2], k.w - 1, 0, 13); Field(f.d[2], k.h - 1, 13, 13); }
            Field(f.d[0], k.tiled, 31, 1);
            Field(f.d[1], 1, 12, 20);
            const gpu::TextureExtents e = gpu::ExtentsOf(f);
            double best[2] = { 1e30, 1e30 };
            std::vector<uint8_t> bytes;
            for (int round = 0; round < 12; round++)
                for (int side = 0; side < 2; side++)
                {
                    const auto t0 = std::chrono::steady_clock::now();
                    for (int i = 0; i < 4; i++)
                    {
                        if (side == 0)
                        {
                            gpu::TextureData a = gpu::ReadTexture(f, g_memory, nullptr);
                            g_bytes += a.bytes[i];
                        }
                        else
                        {
                            Header h{};
                            mw2_texture_read(f.d, g_memory, nullptr, &h, Reserve, &bytes);
                            g_bytes += bytes[i];
                        }
                    }
                    const double s = std::chrono::duration<double>(std::chrono::steady_clock::now() - t0).count();
                    best[side] = std::min(best[side], s);
                }
            const double mb = 4.0 * e.baseBytes / 1e6;
            std::printf("bench %-14s C++ %8.0f MB/s  Rust %8.0f MB/s  Rust/C++ %+.1f%%\n", k.name, mb / best[0],
                        mb / best[1], 100.0 * (best[0] / best[1] - 1));
        }
    }
}

int main(int argc, char** argv)
{
    if (argc < 3) { std::fprintf(stderr, "usage: texture <seed> <cases> [tiling|texture|formats|bench]...\n"); return 2; }
    g_state = std::strtoull(argv[1], nullptr, 10) * 0x2545F4914F6CDD1Dull + 1;
    const uint32_t cases = uint32_t(std::strtoul(argv[2], nullptr, 10));
    MakeMemory();
    for (int i = 3; i < argc; i++)
    {
        const std::string what = argv[i];
        if (what == "tiling") Tiling();
        else if (what == "formats") Formats();
        else if (what == "texture") Texture(cases);
        else if (what == "bench") Bench();
        else { std::fprintf(stderr, "unknown part %s\n", argv[i]); return 2; }
    }
    // The bench's sum of what it read, so that the reads are not optimised away.
    if (g_bytes) std::printf("bench sum %" PRIu64 "\n", g_bytes);
    return 0;
}
