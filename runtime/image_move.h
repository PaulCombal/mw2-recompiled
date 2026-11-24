#pragma once
#include <cstdint>
#include "diagnostics.h"

// MW2's streamed-image pool and the copies it has the GPU make
// (src/runtime/gpu/image_move.rs), as the GPU capture's replay asks of it.
namespace imagepool
{
#if MW2_DIAGNOSTICS
    // MW2_REPLAY_GPU: a move as the title's T_Image_FlushMove queued it.
    void QueueForReplay(uint32_t destination, uint32_t source, uint32_t size,
                        uint32_t streamConstant, uint64_t frame);
#endif
}
