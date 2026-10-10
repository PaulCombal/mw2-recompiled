#pragma once
// MW2_FPS_LIMIT: how many frames a second the title draws at most.
//
// Unset or 60 is the console's pacing: D3D retires a frame at the display's
// blank and the title's own limiter (com_maxfps) is at its 60. Any other
// number takes the title off the blank the way the console's D3D offers it
// (d3d_hooks.cpp), with that number as com_maxfps; 0 is no limit. The title's
// limiter counts whole milliseconds a frame, so the rate it keeps is 1000
// over a whole number: 120 gives 125, 144 gives 166.
#include <algorithm>
#include <cstdint>
#include "env.h"

namespace frame_rate
{
    constexpr uint32_t kConsole = 60;

    // com_maxfps: 0 is no limit, and 1000 is the most the title takes.
    inline uint32_t Limit()
    {
        static const uint32_t limit = uint32_t(std::min<uint64_t>(env::Number("MW2_FPS_LIMIT", kConsole), 1000));
        return limit;
    }

    // Frames are retired at the blank, one a blank, as on the console.
    inline bool Console() { return Limit() == kConsole; }
}
