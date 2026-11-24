// What the Rust asks of the C++ the game compiles (src/runtime/bridge.rs): the GPU
// capture, diagnostics only (build.rs compiles this file with it alone).
#include "crash.h"
#include "gpu/memory_watch.h"
#include "gpu/internal.h"
#include "image_move.h"
#if MW2_DIAGNOSTICS
#include "gpu/stream_capture.h"
#endif

#include <cstddef>
#include <vector>

#if MW2_DIAGNOSTICS
// MW2_CAPTURE_GPU and MW2_REPLAY_GPU, which are C++ (gpu/stream_capture.cpp): its
// brackets around each ring batch and its report (src/runtime/bridge.rs, gpu), its hooks
// in the command processor and the image pool (bridge.rs, stream), and what it asks of
// the Rust GPU, command processor, arena, image pool and memory watch (the end of this
// block, each the C++ declaration of its name forwarded to src/runtime/gpu/exports.rs).
extern "C" void mw2_stream_batch_begin(uint32_t base, uint32_t readIndex, uint32_t mask, uint32_t dwords)
{
    gpu::stream::BatchBegin(base, readIndex, mask, dwords);
}

extern "C" void mw2_stream_batch_end() { gpu::stream::BatchEnd(); }

extern "C" void mw2_stream_shutdown() { gpu::stream::Shutdown(); }

extern "C" void mw2_stream_report() { gpu::stream::Report(); }

extern "C" void mw2_stream_arena_block_seen(uint32_t block)
{
    if (gpu::stream::Capturing()) gpu::stream::ArenaBlockSeen(block);
}

// The capture's three flags, by address: the Rust reads them where the C++ reads them inline.
extern "C" const bool* const mw2_stream_capturing = &gpu::stream::detail::capturing;
extern "C" const bool* const mw2_stream_replaying = &gpu::stream::detail::replaying;
extern "C" const bool* const mw2_stream_counting = &gpu::stream::detail::counting;

extern "C" void mw2_stream_packet() { gpu::stream::PacketSlow(); }

extern "C" void mw2_stream_read(uint32_t address, uint64_t bytes) { gpu::stream::ReadSlow(address, bytes); }

extern "C" void mw2_stream_polled(uint32_t address) { gpu::stream::PolledSlow(address); }

extern "C" void mw2_stream_write(uint32_t address, uint32_t bytes, bool done) { gpu::stream::WriteSlow(address, bytes, done); }

extern "C" void mw2_stream_aperture_read(uint32_t index, uint32_t value) { gpu::stream::ApertureRead(index, value); }

extern "C" void mw2_stream_wait_done(bool memory, uint32_t address, uint32_t value, uint32_t polls, bool timedOut)
{
    gpu::stream::WaitDone(memory, address, value, polls, timedOut);
}

static_assert(sizeof(gpu::stream::Wait) == 16 && offsetof(gpu::stream::Wait, value) == 4 &&
              offsetof(gpu::stream::Wait, polls) == 8 && offsetof(gpu::stream::Wait, timedOut) == 12);

extern "C" bool mw2_stream_replay_wait(gpu::stream::Wait* wait) { return gpu::stream::ReplayWait(*wait); }

extern "C" void mw2_stream_pool_seen(uint32_t destination, uint32_t source, uint32_t size, uint32_t streamConstant,
                                     uint64_t frame)
{
    gpu::stream::PoolSeen(destination, source, size, streamConstant, frame);
}

extern "C" void mw2_stream_diverged(const char* what) { gpu::stream::Diverged(what); }

namespace
{
    // A vector's bytes, zeroed, for the Rust to write into. noexcept: an allocation
    // failure ends the program here rather than unwinding through Rust frames.
    uint8_t* Reserve(void* context, size_t size) noexcept
    {
        auto& bytes = *static_cast<std::vector<uint8_t>*>(context);
        bytes.assign(size, 0);
        return bytes.data();
    }
}

extern "C"
{
    void mw2_gpu_ring_position(uint32_t* read, uint32_t* written, uint32_t* writeBack);
    void mw2_gpu_ring_layout(uint32_t* base, uint32_t* dwords, uint32_t* writeBack);
    void mw2_arena_save(gpu::detail::ArenaState* out);
    void mw2_arena_set_block(uint32_t physical);
    uint64_t mw2_cp_swaps_parsed();
    void mw2_cp_save(uint8_t* (*reserve)(void*, size_t), void* context);
    void mw2_imagepool_queue_for_replay(uint32_t destination, uint32_t source, uint32_t size, uint32_t streamConstant,
                                        uint64_t frame);
    uint32_t mw2_memory_watch_loosened(uint32_t page);
}

// The four words gpu/arena.rs's ArenaState has.
static_assert(sizeof(gpu::detail::ArenaState) == 16 && offsetof(gpu::detail::ArenaState, havePosition) == 12);

void gpu::detail::RingPosition(uint32_t& read, uint32_t& written, uint32_t& writeBack)
{
    mw2_gpu_ring_position(&read, &written, &writeBack);
}
void gpu::detail::RingLayout(uint32_t& base, uint32_t& dwords, uint32_t& writeBack)
{
    mw2_gpu_ring_layout(&base, &dwords, &writeBack);
}
gpu::detail::ArenaState gpu::detail::SaveArena()
{
    ArenaState state;
    mw2_arena_save(&state);
    return state;
}
void gpu::detail::SetArenaBlock(uint32_t physical) { mw2_arena_set_block(physical); }
uint64_t gpu::detail::SwapsParsed() { return mw2_cp_swaps_parsed(); }
void gpu::detail::SaveCommandProcessor(std::vector<uint8_t>& out) { mw2_cp_save(Reserve, &out); }
void imagepool::QueueForReplay(uint32_t destination, uint32_t source, uint32_t size, uint32_t streamConstant,
                               uint64_t frame)
{
    mw2_imagepool_queue_for_replay(destination, source, size, streamConstant, frame);
}
uint32_t gpu::watch::Loosened(uint32_t page) { return mw2_memory_watch_loosened(page); }
#endif
