#pragma once
#include <cstdint>

// The console's XMA decoder: 320 hardware contexts, each a 64-byte block in
// physical memory the title fills in directly, driven by a register window at
// 0x7FEA0000. The title's XAudio2 (statically linked, so already recompiled)
// allocates contexts through XMACreateContext, points them at 2 KB XMA2 packets
// and a small PCM ring, and kicks them by writing a bit into the window; the
// decoder writes 16-bit big-endian PCM into the ring and advances the context's
// bit offset, and XAudio2 reads the ring on its next pass.
//
// src/runtime/kernel/xma.rs, or xma.cpp in the C++ build.
extern "C" {
    void     mw2_xma_initialise();
    uint32_t mw2_xma_allocate_context();
    void     mw2_xma_release_context(uint32_t guestAddress);
    // What the recompiled code's stw into the window would write (runtime/mmio_hook.h).
    void     mw2_xma_register_store(uint32_t address, uint32_t value);
    void     mw2_xma_report();
}

namespace apu::xma
{
    inline void Initialise() { mw2_xma_initialise(); }

    // The guest address of a free context (in the physical arena, as the title
    // turns it back into a physical address by masking), or 0 when all are taken.
    inline uint32_t AllocateContext() { return mw2_xma_allocate_context(); }
    inline void     ReleaseContext(uint32_t guestAddress) { mw2_xma_release_context(guestAddress); }

    inline void Report() { mw2_xma_report(); }
}
