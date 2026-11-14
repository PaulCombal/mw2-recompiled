#pragma once
// A fake guest for asset-free tests: a flat 16 MiB memory behind guest::Base()
// and the two kernel services the dispatcher and file sources call. Include it
// from exactly one translation unit per test executable.
#include "runtime/guest.h"
#include "runtime/kernel/kernel.h"

#include <chrono>
#include <cstdint>
#include <vector>

namespace guest
{
    uint8_t* Base()
    {
        static std::vector<uint8_t> memory(1u << 24);
        return memory.data();
    }
}

namespace kernel
{
    uint32_t AllocateGuest(uint32_t size, uint32_t alignment)
    {
        static uint32_t next = 0x00100000;
        next = (next + alignment - 1) & ~(alignment - 1);
        const uint32_t address = next;
        next += size;
        return address;
    }

    uint64_t SystemTime100ns()
    {
        using namespace std::chrono;
        return 1'000'000'000ull +
               uint64_t(duration_cast<nanoseconds>(steady_clock::now().time_since_epoch()).count() / 100);
    }
}
