// What the system's screens are drawn with (screen.h).
#include "screen.h"
#include "diagnostics.h"
#include "kernel/kernel.h"

#include <algorithm>
#include <cmath>
#include <cstdio>
#include <mutex>

#ifdef MW2_SCREENS
#define STB_TRUETYPE_IMPLEMENTATION
#define STBTT_STATIC
#include <imstb_truetype.h>
// The launcher's fonts, in the executable (CMake writes them out).
extern const unsigned char kFontMenu[];
extern const unsigned char kFontHeading[];
extern const unsigned char kFontBody[];

namespace
{
    constexpr int kAtlas = 512;
    struct Baked
    {
        std::vector<uint8_t> atlas;
        stbtt_packedchar glyphs[95]{};
        float size = 0, ascent = 0;
    };
    // Each the size of the last sheet drawn; the presenter draws one at a time.
    Baked g_fonts[3];

    Baked& Bake(screen::Font which, float unit)
    {
        static const unsigned char* const kData[] = { kFontHeading, kFontMenu, kFontBody };
        static const float kSizes[] = { 38, 28, 18 };
        Baked& font = g_fonts[int(which)];
        const unsigned char* data = kData[int(which)];
        const float size = std::round(kSizes[int(which)] * unit);
        if (font.size == size) return font;
        font.size = size;
        font.atlas.assign(size_t(kAtlas) * kAtlas, 0);
        stbtt_pack_context pack;
        stbtt_PackBegin(&pack, font.atlas.data(), kAtlas, kAtlas, 0, 1, nullptr);
        stbtt_PackFontRange(&pack, data, 0, size, 32, 95, font.glyphs);
        stbtt_PackEnd(&pack);
        stbtt_fontinfo info;
        stbtt_InitFont(&info, data, 0);
        int ascent = 0, descent = 0, gap = 0;
        stbtt_GetFontVMetrics(&info, &ascent, &descent, &gap);
        font.ascent = float(ascent) * stbtt_ScaleForPixelHeight(&info, size);
        return font;
    }
}
#endif

bool screen::Drawn()
{
#ifdef MW2_SCREENS
    return true;
#else
    return false;
#endif
}

namespace
{
    constexpr uint32_t XN_SYS_UI = 0x00000009;
    std::mutex g_openLock;
    screen::Which g_open = screen::Which::None;
}

bool screen::Claim(Which which)
{
    std::lock_guard lock(g_openLock);
    if (g_open == which) return true;
    if (g_open != Which::None) return false;
    g_open = which;
    kernel::PostNotification(XN_SYS_UI, 1);
    return true;
}

void screen::Release(Which which)
{
    std::lock_guard lock(g_openLock);
    if (g_open != which) return;
    g_open = Which::None;
    kernel::PostNotification(XN_SYS_UI, 0);
}

void screen::Pass(Which from, Which to)
{
    std::lock_guard lock(g_openLock);
    if (g_open == from) g_open = to;
}

screen::Sheet::Sheet(Image& image, uint32_t height, float sheetWidth, float sheetHeight, uint64_t version)
    : image(image), unit(float(height) / sheetHeight)
{
    image.height = height;
    image.width = uint32_t(std::lround(sheetWidth * unit)) & ~1u;
    image.version = version;
    image.outline = 2 * unit;
    image.pixels.assign(size_t(image.width) * image.height * 4, 0);
}

int screen::Sheet::At(float units) const { return int(std::lround(units * unit)); }

void screen::Sheet::Fill(float fx0, float fy0, float fx1, float fy1, Colour colour)
{
    const int x0 = std::max(At(fx0), 0), y0 = std::max(At(fy0), 0);
    const int x1 = std::min(At(fx1), int(image.width)), y1 = std::min(At(fy1), int(image.height));
    for (int y = y0; y < y1; y++)
        for (int x = x0; x < x1; x++)
        {
            uint8_t* p = &image.pixels[(size_t(y) * image.width + x) * 4];
            p[0] = colour.b; p[1] = colour.g; p[2] = colour.r; p[3] = 255;
        }
}

#ifdef MW2_SCREENS
float screen::Sheet::Text(Font which, float ux, float uy, Colour colour, const std::string& text)
{
    const Baked& font = Bake(which, unit);
    float x = float(At(ux));
    float penY = float(At(uy)) + font.ascent;
    for (unsigned char c : text)
    {
        if (c < 32 || c > 126) c = '?';
        stbtt_aligned_quad quad;
        stbtt_GetPackedQuad(font.glyphs, kAtlas, kAtlas, c - 32, &x, &penY, &quad, 1);
        const int sx = int(quad.s0 * kAtlas + 0.5f), sy = int(quad.t0 * kAtlas + 0.5f);
        const int w = int(quad.x1 - quad.x0), h = int(quad.y1 - quad.y0);
        for (int row = 0; row < h; row++)
            for (int column = 0; column < w; column++)
            {
                const int px = int(quad.x0) + column, py = int(quad.y0) + row;
                if (px < 0 || py < 0 || px >= int(image.width) || py >= int(image.height)) continue;
                const uint32_t alpha = font.atlas[size_t(sy + row) * kAtlas + sx + column];
                uint8_t* p = &image.pixels[(size_t(py) * image.width + px) * 4];
                p[0] = uint8_t((colour.b * alpha + p[0] * (255 - alpha)) / 255);
                p[1] = uint8_t((colour.g * alpha + p[1] * (255 - alpha)) / 255);
                p[2] = uint8_t((colour.r * alpha + p[2] * (255 - alpha)) / 255);
            }
    }
    return x / unit;
}

float screen::Sheet::Width(Font which, const std::string& text)
{
    const Baked& font = Bake(which, unit);
    float x = 0, y = 0;
    for (unsigned char c : text)
    {
        if (c < 32 || c > 126) c = '?';
        stbtt_aligned_quad quad;
        stbtt_GetPackedQuad(font.glyphs, kAtlas, kAtlas, c - 32, &x, &y, &quad, 1);
    }
    return x / unit;
}
#else
float screen::Sheet::Text(Font, float x, float, Colour, const std::string&) { return x; }
float screen::Sheet::Width(Font, const std::string&) { return 0; }
#endif

void screen::Sheet::Picture(float ux, float uy, float size, const std::vector<uint8_t>& rgba, uint32_t side)
{
    if (!side || rgba.size() < size_t(side) * side * 4) return;
    const int x0 = At(ux), y0 = At(uy), pixels = std::max(At(size), 1);
    for (int y = 0; y < pixels; y++)
        for (int x = 0; x < pixels; x++)
        {
            const int px = x0 + x, py = y0 + y;
            if (px < 0 || py < 0 || px >= int(image.width) || py >= int(image.height)) continue;
            const uint8_t* s = &rgba[(size_t(y * int(side) / pixels) * side + size_t(x * int(side) / pixels)) * 4];
            uint8_t* p = &image.pixels[(size_t(py) * image.width + px) * 4];
            p[0] = s[2]; p[1] = s[1]; p[2] = s[0]; p[3] = 255;
        }
}

void screen::Sheet::Frame(const std::string& heading)
{
    const float width = float(image.width) / unit, height = float(image.height) / unit;
    Fill(0, 0, width, height, kEdge);
    Fill(2, 2, width - 2, height - 2, kBack);
    Text(Font::Heading, 28, 20, kWhite, heading);
}

void screen::Sheet::Buttons(float y, const std::vector<std::pair<std::string, std::string>>& buttons)
{
    float x = 28;
    for (const auto& [button, does] : buttons)
    {
        x = Text(Font::Small, x, y, kAmber, button);
        x = Text(Font::Small, x, y, kDim, "  " + does + "      ");
    }
}

void screen::Dump(const char* name, const Image& image)
{
    if constexpr (diag::kOn)
        if (const char* folder = diag::Text("MW2_DUMP_FRAMES"))
        {
            const std::string path = std::string(folder) + "/" + name + "_" + std::to_string(image.version) + ".ppm";
            if (std::FILE* out = std::fopen(path.c_str(), "wb"))
            {
                std::fprintf(out, "P6\n%u %u\n255\n", image.width, image.height);
                for (size_t i = 0; i < image.pixels.size(); i += 4)
                {
                    const uint8_t rgb[3] = { image.pixels[i + 2], image.pixels[i + 1], image.pixels[i] };
                    std::fwrite(rgb, 1, 3, out);
                }
                std::fclose(out);
            }
        }
}
