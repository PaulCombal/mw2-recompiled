#pragma once
// What the GPU capture (stream_capture.cpp) asks of the command processor, which is
// src/runtime/gpu/: rust_calls.cpp forwards each to the Rust's export.
#include <cstdint>
#include <vector>
#include "../diagnostics.h"

namespace gpu
{
    // The register file's size (src/runtime/gpu/registers.rs's REGISTER_COUNT).
    inline constexpr uint32_t kRegisterCount = 0x5003;
}

namespace gpu::detail
{
    // Registers are plain memory in this address space; the CPU writes some
    // straight through the aperture rather than through the command stream.
    constexpr uint32_t kRegisterAperture = 0x7FC80000u;

    // The ring's read and write indices, and what the read-pointer write-back holds.
    void RingPosition(uint32_t& read, uint32_t& written, uint32_t& writeBack);

#if MW2_DIAGNOSTICS
    // ---- for MW2_CAPTURE_GPU and MW2_REPLAY_GPU
    // The swap packets met so far, and the command processor's state with the
    // arena's progress, as the capture's state.bin holds it
    // (mw2_cp_restore_for_replay takes it back).
    uint64_t SwapsParsed();
    void SaveCommandProcessor(std::vector<uint8_t>& out);
    // D3D9's command-buffer arena (gpu/arena.rs).
    struct ArenaState { uint32_t block, sequence, pending, havePosition; };
    ArenaState SaveArena();
    void SetArenaBlock(uint32_t physical);
    // Where the ring is, its size, and the read pointer's write-back.
    void RingLayout(uint32_t& base, uint32_t& dwords, uint32_t& writeBack);
#endif
}
