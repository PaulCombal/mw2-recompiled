#pragma once
// Pictures: the PC game's .iwi files, and the 360's tiled textures.
//
// A picture is a chain of mip levels, largest first, each one linear (rows of
// pixels or of 4x4 blocks, little-endian, as a DDS file stores them). The 360
// keeps them tiled (Xenos' 2D tiling, the small levels packed into the last
// slice) and byte-swapped.
#include <cstdint>
#include <string>
#include <vector>

namespace mapport
{
    using Mip = std::vector<uint8_t>;

    // The 360's texture formats (the fetch constant's low six bits).
    enum class Gpu : uint8_t
    {
        L8 = 0x02,
        ARGB8 = 0x06,
        DXT1 = 0x12,
        DXT3 = 0x13,
        DXT5 = 0x14,
        DXN = 0x31,
        DXT3A = 0x3A,
        DXT5A = 0x3B,
    };
    struct Format
    {
        Gpu gpu;
        const char* name;
        int blockWidth;     // pixels a block is wide (1: not block compressed)
        int blockBytes;
    };
    const Format& FormatOf(Gpu g);
    bool FormatNamed(const std::string& name, Gpu& out);

    // A picture read from an .iwi: its format, size and mips (for a cube map,
    // the six faces' top levels instead).
    struct Picture
    {
        Gpu format = Gpu::DXT1;
        int width = 0, height = 0;
        std::vector<Mip> mips;
        bool cube = false;
    };
    // Throws Error for a picture it can't read.
    Picture ReadIwi(const std::vector<uint8_t>& data);

    // ---- the 360's layout ---------------------------------------------------

    // Linear mips -> one tiled level chunk, as the 360 keeps it. `single`: the
    // top mip alone, in a slice of its own.
    std::vector<uint8_t> Tile(const std::vector<Mip>& mips, int width, int height, Gpu f, bool single = false);
    // (width blocks, height blocks, stored width blocks, slice bytes) of a mip.
    struct MipLayout
    {
        int wb, hb, sw;
        size_t slice;
    };
    MipLayout Layout(int width, int height, int mip, Gpu f);
    // Each mip's place in a tiled chain: (mip, slice offset, stored width,
    // block offsets x and y), and the chain's size.
    struct MipPlace
    {
        int mip;
        size_t base;
        int sw, ox, oy;
    };
    std::pair<std::vector<MipPlace>, size_t> Plan(int width, int height, Gpu f);
    // The console's byte order: 16-bit swaps for block formats, 32-bit for ARGB8.
    std::vector<uint8_t> Swap(const std::vector<uint8_t>& data, Gpu f);
    inline size_t AlignUp(size_t v, size_t a) { return (v + a - 1) / a * a; }

    // ---- conversions --------------------------------------------------------

    // A PC DXT5 normal map (X in alpha, Y in green) as 360 DXN: the alpha block
    // as it is, then green as a second block.
    std::vector<Mip> NormalMapsToDxn(const std::vector<Mip>& mips);
    // 8-bit single channel -> DXT3A blocks (4 bits a pixel), linear.
    Mip EncodeDxt3a(const std::vector<uint8_t>& lum, int width, int height);
    // A reflection probe mip (B, G, R, A) as plain colour: the PC keeps a
    // brightness multiplier in alpha.
    Mip DecodeProbe(const Mip& bgra);
    constexpr double kProbeScale = 2.5;

    // The size a picture the 360 can't take is resized to: each side down to
    // a power of two, at most `limit`.
    std::pair<int, int> PictureSize(int width, int height, int limit = 2048);
    // Pictures the 360 can't take as they are, resized: decoded,
    // Lanczos-resampled with premultiplied alpha (as Pillow resamples),
    // compressed again, top level only.
    struct Fitted
    {
        int width, height;
        std::vector<Mip> mips;
    };
    Fitted FitPicture(Gpu f, int width, int height, const std::vector<Mip>& mips, int limit);

    // RGBA pixels -> a level in a format, and a level decoded to RGBA as
    // Pillow decodes it.
    Mip EncodePixels(const std::vector<uint8_t>& rgba, int width, int height, Gpu f);
    std::vector<uint8_t> DecodePixels(const Mip& level, int width, int height, Gpu f);
    // Pillow's Image.resize(size, LANCZOS) of an RGBA picture.
    std::vector<uint8_t> ResizeLanczos(const std::vector<uint8_t>& rgba, int width, int height, int toWidth, int toHeight);
}
