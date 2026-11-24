#pragma once
// MW2_REPORT=1: what a bug report needs to say about a run, in every build.
//
// The launcher's REPORT A BUG starts the game with it. The log then names the
// graphics driver, and ends -- at exit or at a fault -- with how the run
// went: the frame rate, how the frame times spread, where the renderer's
// thread spent play, and what the renderer compiled in the middle of it
// (src/runtime/report.rs). Off, the C++'s question is one load of a flag.
#include "env.h"

namespace report
{
    inline bool On()
    {
        static const bool on = env::Flag("MW2_REPORT");
        return on;
    }
}
