#pragma once
#include <cstdint>
#include "../diagnostics.h"

// Which pages of guest physical memory the title has written, found out the way
// Xenia finds out: a page the GPU reads from is made read-only, and the first
// write to it faults once, which marks it written and makes it writable again
// (src/runtime/gpu/memory_watch.rs). What the GPU capture asks of it.
namespace gpu::watch
{
#if MW2_DIAGNOSTICS
    // How many times page `page` of the bank has gone from watched to written.
    uint32_t Loosened(uint32_t page);
#endif
}
