#pragma once
// The screens the console's system draws over a title -- signing in, picking
// friends, the guide -- drawn here into a picture the presenter lays over the
// middle of the frame. They share the launcher's fonts and look.
//
// One is open at a time, as on the console: a screen asked for while another
// is open stays shut. The title hears XN_SYS_UI when the first opens and when
// the last shuts, not at each.
#include <cstdint>
#include <string>
#include <vector>

namespace screen
{
    // BGRA, opaque. The pixels are only rewritten when `version` is not theirs.
    struct Image
    {
        std::vector<uint8_t> pixels;
        uint32_t width = 0, height = 0;
        uint64_t version = 0;
        float outline = 0;   // the frame's width in pixels, which the window rounds the corners of
    };

    // Whether this build draws them (MW2_SCREENS): without, a screen still
    // opens and takes the controllers, unseen.
    bool Drawn();

    enum class Which { None, Signin, Friends, Guide };
    // Opens `which`: false while another is open. True, with nothing posted,
    // when it is the one open.
    bool Claim(Which which);
    // Shuts it, if it is the one open.
    void Release(Which which);
    // The screen open becomes `to` with no notification in between: the guide
    // giving way to the friends screen it opens.
    void Pass(Which from, Which to);

    struct Colour { uint8_t r, g, b; };
    constexpr Colour kWhite{ 236, 236, 232 }, kDim{ 150, 151, 150 }, kAmber{ 232, 178, 52 },
                     kBack{ 22, 24, 28 }, kBar{ 0, 0, 0 }, kEdge{ 70, 73, 78 };

    enum class Font { Heading, Entry, Small };

    // A sheet of `sheetWidth` by `sheetHeight` units, drawn `height` pixels
    // tall; everything below is placed in units.
    struct Sheet
    {
        Image& image;
        float unit;

        Sheet(Image& image, uint32_t height, float sheetWidth, float sheetHeight, uint64_t version);
        int At(float units) const;

        void Fill(float x0, float y0, float x1, float y1, Colour colour);
        // Left-aligned at x, the line's top at y; returns where the text ends.
        float Text(Font font, float x, float y, Colour colour, const std::string& text);
        float Width(Font font, const std::string& text);
        // An RGBA square, scaled to `size` units.
        void Picture(float x, float y, float size, const std::vector<uint8_t>& rgba, uint32_t side);
        // The outline, the dark inside and the heading every screen has.
        void Frame(const std::string& heading);
        // "A  Select      B  Back" along the bottom.
        void Buttons(float y, const std::vector<std::pair<std::string, std::string>>& buttons);
    };

    // MW2_DUMP_FRAMES keeps the screens too, which the frames do not show.
    void Dump(const char* name, const Image& image);
}
