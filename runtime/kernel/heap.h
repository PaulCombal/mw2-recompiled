#pragma once
// The guest address-space allocator the kernel's virtual and physical heaps use.
// Kept in a header of its own so the tests can run it over a small fake range.
#include "../guest.h"
#include "../log.h"
#include <atomic>
#include <cstdint>
#include <cstring>
#include <map>
#include <mutex>

namespace kernel
{
    // A range of guest address space. Freed blocks are kept, merged with
    // their neighbours and handed out again first-fit, and zeroed when they
    // are, as the console's pages come; the rest is carved off the top, which
    // only rises, so what lies above it has never been used and is zero.
    struct Heap
    {
        const char* name;
        const uint32_t end;
        std::atomic<uint32_t> top;
        std::mutex lock;
        std::map<uint32_t, uint32_t> live, free;   // address -> size
        uint64_t allocations = 0, reused = 0, liveBytes = 0;

        Heap(const char* heapName, uint32_t base, uint32_t limit)
            : name(heapName), end(limit), top(base) {}

        uint32_t Alloc(uint32_t size, uint32_t align)
        {
            if (!size) return 0;
            std::lock_guard held(lock);
            allocations++;
            for (auto it = free.begin(); it != free.end(); ++it)
            {
                const uint32_t start = it->first, length = it->second;
                const uint32_t at = (start + align - 1) & ~(align - 1);
                if (uint64_t(at) + size > uint64_t(start) + length) continue;
                free.erase(it);
                if (at > start) free[start] = at - start;
                if (at + size < start + length) free[at + size] = start + length - (at + size);
                std::memset(guest::Base() + at, 0, size);
                reused++;
                return Took(at, size);
            }
            const uint32_t from = top.load(std::memory_order_relaxed);
            const uint32_t at = (from + align - 1) & ~(align - 1);
            if (uint64_t(at) + size > end) return 0;
            if (at > from) free[from] = at - from;
            top.store(at + size, std::memory_order_release);
            return Took(at, size);
        }

        uint32_t Took(uint32_t at, uint32_t size)
        {
            live[at] = size;
            liveBytes += size;
            return at;
        }

        bool Free(uint32_t address)
        {
            std::lock_guard held(lock);
            auto it = live.find(address);
            if (it == live.end()) return false;
            uint32_t start = address, length = it->second;
            liveBytes -= length;
            live.erase(it);
            if (auto after = free.find(start + length); after != free.end())
            {
                length += after->second;
                free.erase(after);
            }
            if (auto before = free.lower_bound(start); before != free.begin())
                if (--before; before->first + before->second == start)
                {
                    start = before->first;
                    length += before->second;
                    free.erase(before);
                }
            free[start] = length;
            return true;
        }

        uint32_t HighWater() const { return top.load(std::memory_order_acquire); }

        void Report()
        {
            std::lock_guard held(lock);
            uint64_t freeBytes = 0;
            for (const auto& [at, size] : free) freeBytes += size;
            LOGI("memory: %s heap %llu MB in use, %llu MB free below the high water of %08X;"
                 " %llu allocations, %llu of them into freed space",
                 name, (unsigned long long)(liveBytes >> 20), (unsigned long long)(freeBytes >> 20),
                 HighWater(), (unsigned long long)allocations, (unsigned long long)reused);
        }
    };
}
