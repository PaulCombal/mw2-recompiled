#pragma once
#include <cstdint>
#include <cstddef>

// The recompiled code addresses memory as a 32-bit offset from a single host
// base pointer, so the whole guest address space is one reservation.
//
// Layout (guest addresses):
//   0x82000000 .. 0x83F00000   XEX image            (PPC_IMAGE_BASE + PPC_IMAGE_SIZE)
//   0x83F00000 .. +CODE_SIZE*2 function lookup table (see PPC_LOOKUP_FUNC)
//   0x84600000 .. 0xA0000000   general heap         (NtAllocateVirtualMemory)
//   0xA0000000 .. 0xC0000000   "physical" heap      (MmAllocatePhysicalMemoryEx)
//   0xC0000000 .. 0xE0000000   the same memory again, through the second aperture
//
// The physical range is exactly 512 MB -- the console's whole RAM -- because
// the title turns a virtual address into a physical one by masking off the top
// three bits, which only round-trips if the arena does not run past 0xC0000000.
//
// The console reaches that one bank through several address windows, differing
// only in page size and caching: 0xA0000000 and 0xC0000000 are the same bytes
// at the same offset, and the title uses both -- D3D9 hands the command
// processor addresses formed as (address & 0x1FFFFFFF) - 0x40000000 and then
// reads its own structures back through them. So the second window has to be
// the same memory here, aliased rather than translated on access: the
// recompiled code dereferences a guest pointer directly, with nothing to hook.
// The space is src/runtime/guest_memory.rs's.
extern "C"
{
    extern uint8_t* mw2_guest_base;
}

namespace guest
{
    inline constexpr uint32_t kHeapBase      = 0x84600000u;
    inline constexpr uint32_t kHeapEnd       = 0xA0000000u;
    inline constexpr uint32_t kPhysicalBase  = 0xA0000000u;
    inline constexpr uint32_t kPhysicalEnd   = 0xC0000000u;
    inline constexpr uint32_t kApertureBase  = 0xC0000000u;   // the same memory again

    // A variable, and read here: every guest pointer the C++ follows comes through.
    inline uint8_t* Base() { return mw2_guest_base; }

    inline void* Translate(uint32_t address) { return Base() + address; }
}
