#pragma once
// The frames the title can be drawn at, for the launcher's resolutions
// (settings.cpp asserts each row is one). The frame is the picture the title
// shows -- the menus and the HUD are drawn into it -- and MW2_RENDER=<w>x<h>
// names it. The game's own rule is src/runtime/render_size.rs's, the same.
#include <cstdint>

namespace render
{
    struct Size
    {
        uint32_t width = 0, height = 0;
        bool operator==(const Size&) const = default;
    };

    // The title draws into EDRAM: 2048 tiles of 80x16 samples. A surface is a
    // whole number of tiles wide, and as many rows of samples tall as its
    // pixels times its samples (a 2x surface has two rows a pixel).
    constexpr uint32_t Tiles(Size size, uint32_t samples)
    {
        return (size.width + 79) / 80 * ((size.height * samples + 15) / 16);
    }

    // The frame is drawn at 1x, its colour and its depth in EDRAM together.
    constexpr bool FrameFits(Size frame) { return 2 * Tiles(frame, 1) <= 2048; }

    // A frame the title can be given: as large as the console's at most in
    // either direction, and no smaller than its standard-definition 640 wide,
    // a whole number of 16 pixels wide and 4 tall, and fitting EDRAM.
    constexpr bool FrameValid(Size frame)
    {
        return frame.width >= 640 && frame.height >= 360 && frame.width <= 1280 && frame.height <= 800 &&
               frame.width % 16 == 0 && frame.height % 4 == 0 && FrameFits(frame);
    }
}
