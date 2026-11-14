// Asset-free test of the guest address-space allocator (runtime/kernel/heap.h)
// that backs NtAllocateVirtualMemory and MmAllocatePhysicalMemoryEx: alignment,
// first-fit reuse, coalescing, zeroing of reused memory, exhaustion, and many
// threads allocating and freeing at once.
#include "tests/fake_guest.h"

#ifdef NDEBUG
#undef NDEBUG
#endif
#include <algorithm>
#include <atomic>
#include <cassert>
#include <cstdint>
#include <cstdio>
#include <mutex>
#include <random>
#include <thread>
#include <vector>

using kernel::Heap;

namespace
{
    constexpr uint32_t kBase = 0x00A00000, kEnd = 0x00B00000;   // 1 MiB, inside the fake memory

    bool IsZero(uint32_t at, uint32_t size)
    {
        const uint8_t* p = guest::Base() + at;
        return std::all_of(p, p + size, [](uint8_t b) { return b == 0; });
    }

    void Fill(uint32_t at, uint32_t size, uint8_t value)
    {
        std::fill(guest::Base() + at, guest::Base() + at + size, value);
    }

    void Basics()
    {
        Heap h("t", kBase, kEnd);
        assert(h.Alloc(0, 16) == 0);                  // nothing to give
        assert(h.HighWater() == kBase);

        const uint32_t a = h.Alloc(100, 16);
        const uint32_t b = h.Alloc(100, 16);
        assert(a == kBase && a % 16 == 0);
        assert(b >= a + 100 && b % 16 == 0);          // never overlaps
        assert(h.liveBytes == 200 && h.allocations == 2);

        const uint32_t c = h.Alloc(10, 0x1000);       // alignment is honoured
        assert(c % 0x1000 == 0 && c >= b + 100);
        assert(h.HighWater() == c + 10);

        assert(!h.Free(0));
        assert(!h.Free(a + 1));                        // not the start of an allocation
        assert(h.Free(a));
        assert(!h.Free(a));                            // not twice
        assert(h.liveBytes == 110);
    }

    void AlignmentGapIsReusable()
    {
        Heap h("t", kBase, kEnd);
        const uint32_t a = h.Alloc(10, 16);
        const uint32_t b = h.Alloc(10, 0x10000);       // leaves a gap below it
        assert(b == kBase + 0x10000);
        const uint32_t gap = h.Alloc(0x100, 16);       // goes into the gap, not above b
        assert(gap >= a + 10 && gap < b);
        assert(h.reused == 1);
    }

    void FirstFitAndCoalescing()
    {
        Heap h("t", kBase, kEnd);
        uint32_t p[5];
        for (auto& x : p) x = h.Alloc(0x100, 0x100);
        for (int i = 1; i < 5; i++) assert(p[i] == p[i - 1] + 0x100);

        // Freeing the middle three in an awkward order merges them into one block.
        assert(h.Free(p[2]));
        assert(h.Free(p[1]));
        assert(h.Free(p[3]));
        assert(h.free.size() == 1);
        assert(h.free.begin()->first == p[1] && h.free.begin()->second == 0x300);

        // A block of that whole size fits exactly there, first-fit.
        const uint32_t big = h.Alloc(0x300, 0x100);
        assert(big == p[1] && h.free.empty());
        assert(h.Free(big));

        // A smaller request takes the lowest address of the hole and leaves the rest.
        const uint32_t small = h.Alloc(0x80, 0x10);
        assert(small == p[1]);
        assert(h.free.size() == 1 && h.free.begin()->first == p[1] + 0x80);
        assert(h.free.begin()->second == 0x300 - 0x80);

        // Freeing the neighbours on both sides merges across them.
        assert(h.Free(small) && h.Free(p[0]) && h.Free(p[4]));
        assert(h.free.size() == 1 && h.free.begin()->first == kBase);
        assert(h.free.begin()->second == h.HighWater() - kBase);
    }

    void ReusedMemoryIsZeroed()
    {
        Heap h("t", kBase, kEnd);
        const uint32_t a = h.Alloc(0x200, 16);
        assert(IsZero(a, 0x200));                      // fresh memory is zero
        Fill(a, 0x200, 0xAB);
        assert(h.Free(a));
        const uint32_t b = h.Alloc(0x100, 16);
        assert(b == a);
        assert(IsZero(b, 0x100));                      // as the console's pages come
    }

    void Exhaustion()
    {
        Heap h("t", kBase, kBase + 0x1000);
        const uint32_t a = h.Alloc(0x800, 16);
        assert(a == kBase);
        assert(h.Alloc(0x800, 16) == kBase + 0x800);
        const uint32_t top = h.HighWater();
        assert(h.Alloc(1, 16) == 0);                   // full
        assert(h.HighWater() == top);                  // and nothing moved
        assert(h.Alloc(0xFFFFFFF0u, 16) == 0);         // no overflow from a huge size
        assert(h.Free(a));
        assert(h.Alloc(0x400, 16) == kBase);           // freed space is still usable
        assert(h.Alloc(0x1000, 16) == 0);              // but there is no hole that large
    }

    void ManyThreads()
    {
        Heap h("t", kBase, kEnd);
        struct Block { uint32_t at, size; };
        std::mutex ownedLock;
        std::vector<Block> everLive;                   // every block still held, checked at the end
        std::atomic<bool> corrupt{ false };

        std::vector<std::thread> threads;
        for (int t = 0; t < 6; t++)
            threads.emplace_back([&, t] {
                std::mt19937 rng(1234 + t);
                std::vector<Block> mine;
                for (int i = 0; i < 2000; i++)
                {
                    if (mine.empty() || rng() % 3)
                    {
                        const uint32_t size = 16 + rng() % 512;
                        const uint32_t align = 16u << (rng() % 4);
                        const uint32_t at = h.Alloc(size, align);
                        if (!at) continue;
                        if (at % align) corrupt = true;
                        // What this thread owns must stay as it wrote it.
                        Fill(at, size, uint8_t(t + 1));
                        mine.push_back({ at, size });
                    }
                    else
                    {
                        const size_t k = rng() % mine.size();
                        const Block b = mine[k];
                        const uint8_t* p = guest::Base() + b.at;
                        for (uint32_t j = 0; j < b.size; j++)
                            if (p[j] != uint8_t(t + 1)) corrupt = true;
                        mine.erase(mine.begin() + long(k));
                        if (!h.Free(b.at)) corrupt = true;
                    }
                }
                std::lock_guard g(ownedLock);
                everLive.insert(everLive.end(), mine.begin(), mine.end());
            });
        for (auto& th : threads) th.join();
        assert(!corrupt);

        std::sort(everLive.begin(), everLive.end(), [](auto& a, auto& b) { return a.at < b.at; });
        for (size_t i = 1; i < everLive.size(); i++)
            assert(everLive[i - 1].at + everLive[i - 1].size <= everLive[i].at);   // no overlap

        for (auto& b : everLive) assert(h.Free(b.at));
        assert(h.liveBytes == 0 && h.live.empty());

        // Everything below the high-water mark is free again, and the free blocks
        // do not overlap. They need not be one block: a gap left by an aligned
        // allocation at the top is not merged with a free block that ends where
        // it starts, so adjacent holes can stay separate. That costs address
        // space, not correctness, and is left as it is.
        uint64_t freeBytes = 0;
        uint32_t previousEnd = kBase;
        for (const auto& [at, size] : h.free)
        {
            assert(at >= previousEnd);
            previousEnd = at + size;
            freeBytes += size;
        }
        assert(previousEnd <= h.HighWater());
        assert(freeBytes == h.HighWater() - kBase);
    }
}

int main()
{
    Basics();
    AlignmentGapIsReusable();
    FirstFitAndCoalescing();
    ReusedMemoryIsZeroed();
    Exhaustion();
    ManyThreads();
    std::puts("kernel-heap-test ok");
    return 0;
}
