#include "frame_rate.h"
#include "env.h"

#include <algorithm>

extern "C" uint32_t mw2_frame_rate_limit()
{
    return uint32_t(std::min<uint64_t>(env::Number("MW2_FPS_LIMIT", frame_rate::kConsole), 1000));
}
