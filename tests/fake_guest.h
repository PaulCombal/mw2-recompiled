#pragma once
// A fake guest for asset-free tests: a flat 16 MiB memory behind guest::Base(),
// small virtual and physical heaps built from the kernel's own Heap, and the
// clock. Include it from exactly one translation unit per test executable.
#include "runtime/guest.h"
#include "runtime/kernel/heap.h"
#include "runtime/kernel/kernel.h"

#include <chrono>
#include <cstdint>
#include <vector>

namespace fake
{
    // Guest addresses of the two heaps, inside the 16 MiB below.
    inline constexpr uint32_t kHeapBase = 0x00100000, kHeapEnd = 0x00900000;
    inline constexpr uint32_t kPhysicalBase = 0x00900000, kPhysicalEnd = 0x00F00000;
}

namespace guest
{
    uint8_t* Base()
    {
        static std::vector<uint8_t> memory(1u << 24);
        return memory.data();
    }
}

namespace
{
    kernel::Heap g_fakeHeap{ "virtual", fake::kHeapBase, fake::kHeapEnd };
    kernel::Heap g_fakePhysical{ "physical", fake::kPhysicalBase, fake::kPhysicalEnd };
}

namespace kernel
{
    uint32_t AllocateGuest(uint32_t size, uint32_t alignment) { return g_fakeHeap.Alloc(size, alignment); }
    uint32_t AllocatePhysical(uint32_t size, uint32_t alignment) { return g_fakePhysical.Alloc(size, alignment); }
    bool FreeGuest(uint32_t address) { return g_fakeHeap.Free(address); }
    bool FreePhysical(uint32_t address) { return g_fakePhysical.Free(address); }
    uint32_t PhysicalHighWater() { return g_fakePhysical.HighWater(); }

    uint64_t SystemTime100ns()
    {
        using namespace std::chrono;
        return 1'000'000'000ull +
               uint64_t(duration_cast<nanoseconds>(steady_clock::now().time_since_epoch()).count() / 100);
    }
}
