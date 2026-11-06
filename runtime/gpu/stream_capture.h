#pragma once
// The GPU's input, captured and replayed (diagnostics builds only):
//
//   MW2_CAPTURE_GPU=<dir>:<after presents>:<presents>  what the command processor
//       reads, from the swap packet <after presents> for <presents> more: a snapshot
//       of guest memory and of its own state, the ring, every page as the title had
//       written it when the GPU side read it, and what else the title did to the
//       consumer (image-pool moves, satisfied waits)
//   MW2_REPLAY_GPU=<dir>   feeds that back to the command processor with no title
//       running (src/runtime/gpu_replay.rs drives it; this file is its hook)
//
// A position is (batch, packet): the ring batch the consumer took, counted from the
// capture's start, and the packet in it, counted from 1 over every packet it
// executes, those in indirect buffers included; 0 is before the batch's first.
// docs/rendering.md (Checking) gives the files.
#include "../diagnostics.h"

#include <cstddef>
#include <cstdint>

#if MW2_DIAGNOSTICS

// Shared with src/runtime/gpu_replay.rs, which builds them from the capture's files.
extern "C"
{
    // A page to write before the packet, or with `own` set, one to find there when
    // the consumer next reads it (the consumer wrote it itself).
    struct Mw2ReplayPage
    {
        uint32_t packet;
        uint32_t page;      // guest address >> 12
        uint32_t own;
        uint32_t reserved;
        const uint8_t* bytes;   // 4096
    };
    enum : uint32_t
    {
        kMw2ReplayPool = 1,       // a: destination, b: source, c: size, d: stream constant, e: frame
        kMw2ReplayWait = 2,       // a: memory, b: address, c: value, d: polls, e: timed out
        kMw2ReplayAperture = 3,   // a: register, b: value
        kMw2ReplayArena = 4,      // a: block (physical)
    };
    struct Mw2ReplayEvent
    {
        uint32_t kind, packet, a, b, c, d;
        uint64_t e;
    };
    struct Mw2ReplayBatch
    {
        const Mw2ReplayPage* pages;
        uint64_t pageCount;
        const Mw2ReplayEvent* events;
        uint64_t eventCount;
    };
}

namespace gpu::stream
{
    namespace detail
    {
        extern bool capturing;   // from the snapshot to the end of the last batch
        extern bool replaying;   // MW2_REPLAY_GPU
        extern bool counting;    // either of them: positions are kept
    }
    inline bool Capturing() { return detail::capturing; }
    inline bool Replaying() { return detail::replaying; }

    // The consumer, before each packet it executes (command_processor.cpp).
    void PacketSlow();
    inline void Packet() { if (detail::counting) PacketSlow(); }

    // A read of guest memory on the GPU side, at a guest address (the second
    // window of the physical bank taken as the first).
    void ReadSlow(uint32_t address, uint64_t bytes);
    inline void NoteRead(uint32_t address, uint64_t bytes)
    {
        if (detail::counting && bytes) ReadSlow(address, bytes);
    }
    // The same at a physical address, as the GPU is handed them.
    void ReadPhysicalSlow(uint32_t physical, uint64_t bytes);
    inline void NoteReadPhysical(uint32_t physical, uint64_t bytes)
    {
        if (detail::counting && bytes) ReadPhysicalSlow(physical, bytes);
    }

    // The dword a memory WAIT_REG_MEM polls, before its first poll. The title may have
    // written it since the batch first read its page, on a page the memory watch does
    // not guard: the capture looks at the page again, whatever it saw before.
    void PolledSlow(uint32_t address);
    inline void NotePolled(uint32_t address)
    {
        if (detail::counting) PolledSlow(address);
    }

    // A write the consumer makes itself, bracketed: NoteWrite before it, NoteWritten
    // after. The replay makes the same write, so what it leaves is checked, not stored:
    // in the replay, NoteWritten says their next read checks them.
    void WriteSlow(uint32_t address, uint32_t bytes, bool done);
    inline void NoteWrite(uint32_t address, uint32_t bytes)
    {
        if (detail::capturing) WriteSlow(address, bytes, false);
    }
    inline void NoteWritten(uint32_t address, uint32_t bytes)
    {
        if (detail::capturing || detail::replaying) WriteSlow(address, bytes, true);
    }

    // The ring consumer around each batch (gpu.cpp). BatchBegin starts the capture at
    // the batch boundary MW2_CAPTURE_GPU names, and BatchEnd ends it.
    void BatchBegin(uint32_t base, uint32_t readIndex, uint32_t mask, uint32_t dwords);
    void BatchEnd();
    // The run ends: a capture still going is closed where it is.
    void Shutdown();

    // A register read from the aperture because the stream never wrote it.
    void ApertureRead(uint32_t index, uint32_t value);
    // The arena's progress block, as ArenaProgress finds it.
    void ArenaBlockSeen(uint32_t block);
    // WAIT_REG_MEM, as it went: `value` is the dword the last poll read, as the
    // guest holds it, or the register.
    void WaitDone(bool memory, uint32_t address, uint32_t value, uint32_t polls, bool timedOut);
    // The replay's journal entry for the WAIT_REG_MEM at this packet.
    struct Wait { bool memory = false; uint32_t value = 0, polls = 0; bool timedOut = false; };
    bool ReplayWait(Wait& wait);
    // The image pool's queue, at the moment the consumer looks at it.
    void PoolSeen(uint32_t destination, uint32_t source, uint32_t size, uint32_t streamConstant,
                  uint64_t frame);
    // The replay found the consumer doing something the capture did not have it do.
    void Diverged(const char* what);

    void Report();
}

#endif
