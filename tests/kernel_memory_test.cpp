// Asset-free test of the memory imports (runtime/kernel/memory.cpp):
// NtAllocateVirtualMemory, NtFreeVirtualMemory, MmAllocatePhysicalMemoryEx and
// the rest, called as recompiled code would call them, over the fake guest's
// small heaps.
#include "tests/fake_guest.h"

#ifdef NDEBUG
#undef NDEBUG
#endif
#include <algorithm>
#include <cassert>
#include <cstdint>
#include <cstdio>

PPC_FUNC(__imp__NtAllocateVirtualMemory);
PPC_FUNC(__imp__NtFreeVirtualMemory);
PPC_FUNC(__imp__NtQueryVirtualMemory);
PPC_FUNC(__imp__MmAllocatePhysicalMemoryEx);
PPC_FUNC(__imp__MmFreePhysicalMemory);
PPC_FUNC(__imp__MmQueryAddressProtect);
PPC_FUNC(__imp__MmSetAddressProtect);

namespace
{
    constexpr uint32_t MEM_COMMIT = 0x1000, MEM_RESERVE = 0x2000, MEM_RELEASE = 0x8000;
    constexpr uint32_t kBasePtr = 0x3000, kSizePtr = 0x3010, kInfo = 0x3100;

    struct Result { uint32_t status, base, size; };

    Result Allocate(uint32_t base, uint32_t size, uint32_t type)
    {
        *GuestPtr<be32>(kBasePtr) = base;
        *GuestPtr<be32>(kSizePtr) = size;
        PPCContext ctx;
        ctx.r3.u64 = kBasePtr;
        ctx.r4.u64 = kSizePtr;
        ctx.r5.u64 = type;
        __imp__NtAllocateVirtualMemory(ctx, guest::Base());
        return { uint32_t(ctx.r3.u64), *GuestPtr<be32>(kBasePtr), *GuestPtr<be32>(kSizePtr) };
    }

    uint32_t Free(uint32_t base, uint32_t type)
    {
        *GuestPtr<be32>(kBasePtr) = base;
        *GuestPtr<be32>(kSizePtr) = 0;
        PPCContext ctx;
        ctx.r3.u64 = kBasePtr;
        ctx.r4.u64 = kSizePtr;
        ctx.r5.u64 = type;
        __imp__NtFreeVirtualMemory(ctx, guest::Base());
        return uint32_t(ctx.r3.u64);
    }

    uint32_t Physical(uint32_t size, uint32_t alignment)
    {
        PPCContext ctx;
        ctx.r4.u64 = size;
        ctx.r6.u64 = 0;
        ctx.r7.u64 = 0xFFFFFFFF;
        ctx.r8.u64 = alignment;
        __imp__MmAllocatePhysicalMemoryEx(ctx, guest::Base());
        return uint32_t(ctx.r3.u64);
    }

    void FreePhysical(uint32_t address)
    {
        PPCContext ctx;
        ctx.r4.u64 = address;
        __imp__MmFreePhysicalMemory(ctx, guest::Base());
    }

    void Virtual()
    {
        // Bad parameters.
        {
            PPCContext ctx;
            ctx.r3.u64 = 0; ctx.r4.u64 = kSizePtr; ctx.r5.u64 = MEM_COMMIT | MEM_RESERVE;
            __imp__NtAllocateVirtualMemory(ctx, guest::Base());
            assert(uint32_t(ctx.r3.u64) == X_STATUS_INVALID_PARAMETER);
            ctx.r3.u64 = kBasePtr; ctx.r4.u64 = 0;
            __imp__NtAllocateVirtualMemory(ctx, guest::Base());
            assert(uint32_t(ctx.r3.u64) == X_STATUS_INVALID_PARAMETER);
        }
        assert(Allocate(0, 0, MEM_COMMIT | MEM_RESERVE).status == X_STATUS_INVALID_PARAMETER);

        // A reservation is rounded up to whole pages, page aligned, and zeroed.
        Result a = Allocate(0, 1, MEM_COMMIT | MEM_RESERVE);
        assert(a.status == X_STATUS_SUCCESS && a.size == 0x1000 && a.base % 0x1000 == 0);
        Result b = Allocate(0, 0x2001, MEM_COMMIT | MEM_RESERVE);
        assert(b.status == X_STATUS_SUCCESS && b.size == 0x3000);
        assert(b.base >= a.base + a.size || b.base + b.size <= a.base);   // never overlap

        // A commit onto an existing reservation succeeds where it is.
        Result c = Allocate(b.base, 0x1800, MEM_COMMIT);
        assert(c.status == X_STATUS_SUCCESS && c.base == b.base && c.size == 0x2000);

        // Memory the title scribbled on comes back zero when it is given out again.
        std::fill(guest::Base() + a.base, guest::Base() + a.base + 0x1000, uint8_t(0xCD));
        assert(Free(a.base, MEM_RELEASE) == X_STATUS_SUCCESS);
        Result d = Allocate(0, 0x1000, MEM_COMMIT | MEM_RESERVE);
        assert(d.base == a.base);
        assert(std::all_of(guest::Base() + d.base, guest::Base() + d.base + 0x1000,
                           [](uint8_t v) { return v == 0; }));

        // Release frees once; a decommit keeps the allocation; unknown bases fail.
        assert(Free(b.base, MEM_COMMIT) == X_STATUS_SUCCESS);             // decommit only
        assert(Free(b.base, MEM_RELEASE) == X_STATUS_SUCCESS);
        assert(Free(b.base, MEM_RELEASE) == X_STATUS_INVALID_PARAMETER);  // already gone
        assert(Free(0x12345000, MEM_RELEASE) == X_STATUS_INVALID_PARAMETER);
        {
            PPCContext ctx;
            ctx.r3.u64 = 0; ctx.r4.u64 = kSizePtr; ctx.r5.u64 = MEM_RELEASE;
            __imp__NtFreeVirtualMemory(ctx, guest::Base());
            assert(uint32_t(ctx.r3.u64) == X_STATUS_INVALID_PARAMETER);
        }
        assert(Free(d.base, MEM_RELEASE) == X_STATUS_SUCCESS);

        // Exhausting the heap reports no memory, and nothing is written to the out values.
        Result big = Allocate(0, fake::kHeapEnd - fake::kHeapBase, MEM_COMMIT | MEM_RESERVE);
        assert(big.status == X_STATUS_NO_MEMORY && big.base == 0);
    }

    void Query()
    {
        PPCContext ctx;
        ctx.r4.u64 = kInfo;
        __imp__NtQueryVirtualMemory(ctx, guest::Base());
        assert(uint32_t(ctx.r3.u64) == X_STATUS_SUCCESS);
        auto* info = GuestPtr<be32>(kInfo);
        assert(uint32_t(info[0]) == guest::kHeapBase && uint32_t(info[1]) == guest::kHeapBase);
        assert(uint32_t(info[3]) == guest::kHeapEnd - guest::kHeapBase);
        assert(uint32_t(info[4]) == 0x1000);   // MEM_COMMIT
        assert(uint32_t(info[2]) == 0x04 && uint32_t(info[5]) == 0x04);   // PAGE_READWRITE

        __imp__MmQueryAddressProtect(ctx, guest::Base());
        assert(ctx.r3.u64 == 0x04);
        __imp__MmSetAddressProtect(ctx, guest::Base());
        assert(ctx.r3.u64 == 0);
    }

    void PhysicalMemory()
    {
        // The console hands out 64 KB pages, so a block is never at an odd 4 KB
        // offset whatever alignment the caller asked for. The title relies on it.
        for (uint32_t requested : { 0u, 0x1000u, 0x4000u, 0x10000u })
        {
            const uint32_t at = Physical(0x1000, requested);
            assert(at != 0 && at % 0x10000 == 0);
            assert(at >= fake::kPhysicalBase && at < fake::kPhysicalEnd);
            FreePhysical(at);
        }

        // A larger alignment than that is honoured.
        const uint32_t big = Physical(0x1000, 0x40000);
        assert(big % 0x40000 == 0);
        FreePhysical(big);

        // Sizes round up to pages, blocks do not overlap, and freed ones are reused.
        const uint32_t a = Physical(0x1001, 0x1000);
        const uint32_t b = Physical(0x20000, 0x1000);
        assert(a && b && (b >= a + 0x2000 || b + 0x20000 <= a));
        FreePhysical(a);
        const uint32_t again = Physical(0x1000, 0x1000);   // first-fit: the lowest free block
        assert(again && again <= a && again % 0x10000 == 0);
        assert(again + 0x1000 <= b || again >= b + 0x20000);
        assert(Physical(0xFFFFFFF0u, 0x1000) == 0);   // too large: zero, not a wild address
        assert(kernel::PhysicalHighWater() >= b + 0x20000);

        // Freeing something that is not an allocation is harmless.
        FreePhysical(0x00123456);
    }
}

int main()
{
    Virtual();
    Query();
    PhysicalMemory();
    std::puts("kernel-memory-test ok");
    return 0;
}
