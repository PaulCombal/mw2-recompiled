#pragma once
// MW2_TRACE_PACING=<file>: a timeline of what paces a frame -- the ring
// consumer's batches and swaps, the title's threads waiting and presenting --
// written to <file> at exit, one "microseconds event value thread" a line.
// Off, a Note is a load of a flag; without diagnostics, nothing.
#include <cstdint>
#include "diagnostics.h"

extern "C"
{
    int64_t mw2_pacing_microseconds();
#if MW2_DIAGNOSTICS
    bool mw2_pacing_wanted();
    void mw2_pacing_note(char event, uint32_t value);
    void mw2_pacing_note_callers(uint32_t r1);
    void mw2_pacing_write();
#endif
}

namespace pacing
{
    enum Event : char
    {
        kBatchBegin = 'B',   // the consumer found `value` dwords to read
        kBatchEnd = 'E',
        kSwap = 'S',         // the consumer parsed a frame's swap
        kWaitBegin = 'W',    // a thread began waiting on predicate `value`
        kWaitEnd = 'R',
        kThreadTime = 'C',   // the thread's CPU time, in microseconds
        kKernelWait = 'K',   // a thread waits on kernel objects, called from `value`
        kSleep = 'D',        // a thread sleeps, called from `value`
        kKernelWaitEnd = 'k',
        kCaller = 'X',       // a return address further up the guest stack
        kPresent = 'P',      // the title's D3D present, in
        kFrame = 'F',        // the renderer finished a frame; `value` 1 when it drew the world
        kPresentEnd = 'p',
    };
    // Microseconds since the start, on the clock the trace is written in.
    inline int64_t Microseconds() { return mw2_pacing_microseconds(); }
#if MW2_DIAGNOSTICS
    inline bool On()
    {
        static const bool on = mw2_pacing_wanted();
        return on;
    }
    inline void Note(Event event, uint32_t value = 0)
    {
        if (On()) mw2_pacing_note(char(event), value);
    }
    // kCaller notes for the guest frames above the one with stack pointer
    // `r1`: whoever called the wrapper that waited.
    inline void NoteCallers(uint32_t r1)
    {
        if (On()) mw2_pacing_note_callers(r1);
    }
    inline void Write() { mw2_pacing_write(); }
#else
    inline bool On() { return false; }
    inline void Note(Event, uint32_t = 0) {}
    inline void NoteCallers(uint32_t) {}
    inline void Write() {}
#endif
}
