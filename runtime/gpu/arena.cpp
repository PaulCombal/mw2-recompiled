// D3D9's command-buffer arena, from the command processor's side.
//
// D3D9's arena is a ring of its own, and the producer will not overwrite a
// byte until the command processor reports it read past -- a pair of
// EVENT_WRITE_SHDs closing each segment: position, then sequence. Missing one
// report stops the title for good, with an empty ring.
#include <ppc_recomp_shared.h>
#include "internal.h"
#include "gpu.h"
#include "../log.h"
#include "../kernel/kernel.h"
#if MW2_DIAGNOSTICS
#include "stream_capture.h"
#endif

#include <atomic>

namespace
{
    using namespace gpu::detail;

    std::atomic<uint32_t> g_block{ 0 };        // physical
    uint32_t g_pendingPosition = 0;            // GPU thread only, as is the rest
    bool     g_havePosition = false;
    uint32_t g_sequence = 0;
#if MW2_DIAGNOSTICS
    uint64_t g_applied = 0, g_staleSequences = 0;   // for the report
#endif
}

// A segment is allocated at a fixed size but filled only as far as the frame
// needs, so a stale trailer from its previous use can sit before the real one.
// Applying it would move the position backwards and deadlock the title with an
// empty ring. The sequence only moves forward, so it settles which is which.
bool gpu::detail::ArenaProgress(uint32_t address, uint32_t value)
{
    const uint32_t block = g_block.load(std::memory_order_relaxed);
#if MW2_DIAGNOSTICS
    if (gpu::stream::Capturing()) gpu::stream::ArenaBlockSeen(block);
#endif
    if (!block) return false;
    const uint32_t target = address & ~3u;
    if (target == block + 4) { g_pendingPosition = value; g_havePosition = true; return true; }
    if (target != block) return false;

    // The sequence advances by two per segment, and signed arithmetic keeps the
    // comparison right across a wrap. A report claiming thousands ahead would
    // poison the counter for good -- every genuine one after it would read as
    // stale -- so it is dropped too.
    constexpr int32_t kMaxAdvance = 1024;
    const int32_t advance = int32_t(value - g_sequence);
    if (g_sequence && (advance <= 0 || advance > kMaxAdvance))
    {
#if MW2_DIAGNOSTICS
        g_staleSequences++;
#endif
        g_havePosition = false;
        return true;
    }
    g_sequence = value;
#if MW2_DIAGNOSTICS
    g_applied++;
#endif
    if (g_havePosition)
    {
        WritePhysical(block + 4, g_pendingPosition);
        g_havePosition = false;
    }
    WritePhysical(block, value);
    return true;
}

void gpu::SetArenaProgressBlock(uint32_t address)
{
    const uint32_t physical = kernel::ToPhysical(address);
    if (g_block.exchange(physical, std::memory_order_relaxed) != physical)
        LOGI("gpu: arena progress block at %08X (physical %08X)", address, physical);
}

#if MW2_DIAGNOSTICS
gpu::detail::ArenaState gpu::detail::SaveArena()
{
    return { g_block.load(std::memory_order_relaxed), g_sequence, g_pendingPosition,
             uint32_t(g_havePosition) };
}

void gpu::detail::RestoreArena(const ArenaState& state)
{
    g_block.store(state.block, std::memory_order_relaxed);
    g_sequence = state.sequence;
    g_pendingPosition = state.pending;
    g_havePosition = state.havePosition != 0;
}

void gpu::detail::SetArenaBlock(uint32_t physical) { g_block.store(physical, std::memory_order_relaxed); }

void gpu::detail::ReportArena()
{
    LOGI("gpu: arena progress: %llu reports applied, %llu stale ones dropped",
         (unsigned long long)g_applied, (unsigned long long)g_staleSequences);
}
#endif
