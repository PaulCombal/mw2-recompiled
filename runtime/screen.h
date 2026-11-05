#pragma once
// The pictures of the screens the console's system draws over a title --
// signing in (signin.h), picking friends (friends.h), the guide (guide.h) --
// which the presenter lays over the middle of the frame.
#include "text_sink.h"

#include <cstdint>
#include <vector>

extern "C"
{
    // A picture as it crosses: the pixels go to `pixels` through `sink` only
    // when they are drawn again.
    struct mw2_image
    {
        uint64_t version;
        uint32_t width, height;
        float outline;
        void* pixels;
        mw2_text_sink sink;
    };
}

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

    // A screen's picture for a window of this height, through its C function.
    inline bool Picture(bool (*picture)(uint32_t, mw2_image*), uint32_t windowHeight, Image& image)
    {
        mw2_image c{ image.version, image.width, image.height, image.outline, &image.pixels, mw2_assign_bytes };
        if (!picture(windowHeight, &c)) return false;
        image.version = c.version;
        image.width = c.width;
        image.height = c.height;
        image.outline = c.outline;
        return true;
    }

    // The other way, for the C++ build's screens: draws into `image`, as the
    // presenter's picture had it, and hands the pixels over if it drew.
    inline bool Answer(mw2_image* c, bool drawn, const Image& image)
    {
        if (!drawn) return false;
        if (image.version != c->version || image.height != c->height)
            c->sink(c->pixels, reinterpret_cast<const char*>(image.pixels.data()), image.pixels.size());
        c->version = image.version;
        c->width = image.width;
        c->height = image.height;
        c->outline = image.outline;
        return true;
    }
}
