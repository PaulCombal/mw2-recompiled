// tools/font_diff.py's C++ half: stb_truetype as runtime/screen.cpp compiles and calls
// it (imstb_truetype.h with STB_TRUETYPE_IMPLEMENTATION and STBTT_STATIC, no stb_rect_pack),
// to hold src/runtime/truetype.rs to. font_diff.rs links font_diff_c_bake in; built with
// FONT_DIFF_CHILD this is also a program that bakes one damaged font, for the fuzz, where
// the C may read outside the font: it runs the bake twice, the font once against a guard
// page after it and once against one before it, and fails when either faults or the two
// differ, so that what it prints is only what the C gives without reading outside.
//
//   usage (child): font_diff_child <font> <size> <side>
//
// It prints a little-endian i32 whether stbtt_InitFont took the font (when it did not,
// nothing more: stb would go on with fields it never set), then the i32 result, the
// ascent's bits, the 95 stbtt_packedchar of 28 bytes and the atlas.
#include <cstdint>
#include <cstring>

#define STB_TRUETYPE_IMPLEMENTATION
#define STBTT_STATIC
#include <imstb_truetype.h>

static_assert(sizeof(stbtt_packedchar) == 28, "font_diff.rs reads 28 bytes a glyph");

// mw2_fonts_bake's body, with the font passed in.
extern "C" int font_diff_c_bake(const unsigned char* data, float size, uint8_t* atlas, int side, void* glyphs, float* ascent)
{
    stbtt_pack_context pack;
    stbtt_PackBegin(&pack, atlas, side, side, 0, 1, nullptr);
    const bool fitted = stbtt_PackFontRange(&pack, data, 0, size, 32, 95, static_cast<stbtt_packedchar*>(glyphs)) != 0;
    stbtt_PackEnd(&pack);
    stbtt_fontinfo info;
    stbtt_InitFont(&info, data, 0);
    int fontAscent = 0, descent = 0, gap = 0;
    stbtt_GetFontVMetrics(&info, &fontAscent, &descent, &gap);
    *ascent = float(fontAscent) * stbtt_ScaleForPixelHeight(&info, size);
    return fitted;
}

// Whether the InitFont inside stbtt_PackFontRanges takes the font.
extern "C" int font_diff_c_init(const unsigned char* data)
{
    stbtt_fontinfo info;
    return stbtt_InitFont(&info, data, stbtt_GetFontOffsetForIndex(data, 0));
}

#include <cstdio>
#include <cstdlib>
#include <vector>
#include <sys/mman.h>
#include <unistd.h>

// A copy of the font against an inaccessible page, for font_diff.rs's sweep: a C that read
// past the font's end would fault there rather than read what follows. Freed with
// font_diff_free_guarded.
extern "C" const unsigned char* font_diff_guarded(const unsigned char* font, size_t size)
{
    const size_t page = size_t(sysconf(_SC_PAGESIZE));
    const size_t pages = (size + page - 1) / page * page;
    auto* base = static_cast<unsigned char*>(mmap(nullptr, pages + page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0));
    if (base == MAP_FAILED || mprotect(base + pages, page, PROT_NONE) != 0)
        abort();
    memcpy(base + pages - size, font, size);
    return base + pages - size;
}

extern "C" void font_diff_free_guarded(const unsigned char* font, size_t size)
{
    const size_t page = size_t(sysconf(_SC_PAGESIZE));
    const size_t pages = (size + page - 1) / page * page;
    munmap(const_cast<unsigned char*>(font) + size - pages, pages + page);
}

#ifdef FONT_DIFF_CHILD

// A copy of the font with 8 GB of inaccessible addresses on each side, the font against
// the end of its pages or against their start.
static const unsigned char* Guarded(const std::vector<unsigned char>& font, bool atEnd)
{
    const size_t page = size_t(sysconf(_SC_PAGESIZE));
    const size_t guard = size_t(8) << 30;
    const size_t pages = (font.size() + page - 1) / page * page + page;
    auto* base = static_cast<unsigned char*>(
        mmap(nullptr, guard * 2 + pages, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0));
    if (base == MAP_FAILED || mprotect(base + guard, pages, PROT_READ | PROT_WRITE) != 0) {
        perror("mmap");
        exit(2);
    }
    unsigned char* at = base + guard + (atEnd ? pages - font.size() : 0);
    memset(base + guard, atEnd ? 0x5A : 0xA5, pages);
    memcpy(at, font.data(), font.size());
    return at;
}

int main(int argc, char** argv)
{
    if (argc != 4) {
        fprintf(stderr, "usage: font_diff_child <font> <size> <side>\n");
        return 2;
    }
    FILE* file = fopen(argv[1], "rb");
    if (!file)
        return 2;
    std::vector<unsigned char> font;
    unsigned char chunk[65536];
    for (size_t got; (got = fread(chunk, 1, sizeof(chunk), file)) > 0;)
        font.insert(font.end(), chunk, chunk + got);
    fclose(file);
    const float size = strtof(argv[2], nullptr);
    const int side = atoi(argv[3]);

    std::vector<unsigned char> out[2];
    for (int run = 0; run < 2; ++run) {
        const unsigned char* data = Guarded(font, run == 0);
        int32_t init = font_diff_c_init(data);
        std::vector<unsigned char>& o = out[run];
        o.insert(o.end(), reinterpret_cast<unsigned char*>(&init), reinterpret_cast<unsigned char*>(&init) + 4);
        if (!init)
            continue;
        std::vector<unsigned char> atlas(size_t(side) * side, 0);
        stbtt_packedchar glyphs[95];
        memset(glyphs, 0, sizeof(glyphs));
        float ascent = 0;
        int32_t fitted = font_diff_c_bake(data, size, atlas.data(), side, glyphs, &ascent);
        o.insert(o.end(), reinterpret_cast<unsigned char*>(&fitted), reinterpret_cast<unsigned char*>(&fitted) + 4);
        o.insert(o.end(), reinterpret_cast<unsigned char*>(&ascent), reinterpret_cast<unsigned char*>(&ascent) + 4);
        o.insert(o.end(), reinterpret_cast<unsigned char*>(glyphs), reinterpret_cast<unsigned char*>(glyphs) + sizeof(glyphs));
        o.insert(o.end(), atlas.begin(), atlas.end());
    }
    if (out[0] != out[1]) {
        fprintf(stderr, "the two placements differ: the C read outside the font\n");
        return 3;
    }
    fwrite(out[0].data(), 1, out[0].size(), stdout);
    return 0;
}
#endif
