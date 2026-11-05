#pragma once
#include <cstdint>
#include "../guest_memory.h"

// Guest virtual and physical addresses. The title computes physical addresses
// inline as (va & 0x1FFFFFFF), which is what MmGetPhysicalAddress returns, so
// the two have to agree. FromPhysical is the inverse, which the GPU side uses to
// get from an address the command stream names back to one it can read.
//
// Inline, as the GPU's command processor calls them for every packet: the
// physical heap's top is a word the kernel keeps (src/runtime/kernel/memory.rs,
// kernel.cpp in the C++ build), and only rises.
extern "C" uint32_t mw2_kernel_physical_top;

namespace kernel
{
    inline uint32_t PhysicalHighWater() { return __atomic_load_n(&mw2_kernel_physical_top, __ATOMIC_ACQUIRE); }

    inline uint32_t ToPhysical(uint32_t va)
    {
        // Exactly the sequence the title inlines around its Vd* calls:
        //   phys = (va & 0x1FFFFFFF) + (((va >> 20) + 0x200) & 0x1000)
        return (va & 0x1FFFFFFFu) + ((((va >> 20) + 0x200u) & 0x1000u));
    }

    inline uint32_t FromPhysical(uint32_t phys)
    {
        // Ambiguous in principle -- on hardware 0x8xxxxxxx and 0xAxxxxxxx are two
        // views of the same RAM, while here they are separate arenas. Everything the
        // GPU is handed comes from the physical arena, so try that first.
        uint32_t arena = guest::kPhysicalBase + phys;
        if (phys < (PhysicalHighWater() - guest::kPhysicalBase)) return arena;
        uint32_t cached = 0x80000000u + phys;
        if (cached >= 0x82000000u && cached < guest::kHeapEnd) return cached;
        return 0;
    }
}
