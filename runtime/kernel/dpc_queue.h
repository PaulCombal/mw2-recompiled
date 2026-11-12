#pragma once
// Deferred procedure call bookkeeping, independent of guest memory so it can be
// tested on its own. The runtime keeps its own record per DPC, keyed by the
// guest address of the caller-owned KDPC, so the KDPC's in-memory layout never
// has to be known (docs/MW3-DPC-INVESTIGATION.md).
//
// Contract (NT semantics): Initialize records routine and context and leaves
// the DPC not queued; Insert returns true when it queues and false when the DPC
// is unknown or already queued; dispatch is FIFO; the queued flag is cleared
// *before* the routine runs, so a routine may queue its own DPC again.

#include <condition_variable>
#include <cstdint>
#include <deque>
#include <mutex>
#include <unordered_map>

namespace kernel
{
    struct DpcWork
    {
        uint32_t dpc, routine, context, arg1, arg2;
    };

    class DpcQueue
    {
    public:
        // False for a null DPC or routine. Re-initialising a queued DPC drops
        // its pending work, as NT leaves it undefined and this is the safe choice.
        bool Initialize(uint32_t dpc, uint32_t routine, uint32_t context)
        {
            if (!dpc || !routine) return false;
            std::lock_guard<std::mutex> lock(m_);
            Entry& e = entries_[dpc];
            e.routine = routine;
            e.context = context;
            if (e.queued) Drop(dpc);
            e.queued = false;
            return true;
        }

        bool Insert(uint32_t dpc, uint32_t arg1, uint32_t arg2)
        {
            {
                std::lock_guard<std::mutex> lock(m_);
                auto it = entries_.find(dpc);
                if (it == entries_.end() || it->second.queued) return false;
                it->second.queued = true;
                pending_.push_back({dpc, it->second.routine, it->second.context, arg1, arg2});
            }
            wake_.notify_one();
            return true;
        }

        // Blocks until work exists or Stop() was called; false after Stop().
        bool Wait(DpcWork& out)
        {
            std::unique_lock<std::mutex> lock(m_);
            wake_.wait(lock, [&] { return stop_ || !pending_.empty(); });
            if (pending_.empty()) return false;
            out = pending_.front();
            pending_.pop_front();
            auto it = entries_.find(out.dpc);
            if (it != entries_.end()) it->second.queued = false;
            return true;
        }

        // Non-blocking form for tests.
        bool TryPop(DpcWork& out)
        {
            std::lock_guard<std::mutex> lock(m_);
            if (pending_.empty()) return false;
            out = pending_.front();
            pending_.pop_front();
            auto it = entries_.find(out.dpc);
            if (it != entries_.end()) it->second.queued = false;
            return true;
        }

        void Stop()
        {
            { std::lock_guard<std::mutex> lock(m_); stop_ = true; }
            wake_.notify_all();
        }

    private:
        struct Entry { uint32_t routine = 0, context = 0; bool queued = false; };

        void Drop(uint32_t dpc)   // caller holds m_
        {
            for (auto it = pending_.begin(); it != pending_.end();)
                it = it->dpc == dpc ? pending_.erase(it) : it + 1;
        }

        std::mutex m_;
        std::condition_variable wake_;
        std::unordered_map<uint32_t, Entry> entries_;
        std::deque<DpcWork> pending_;
        bool stop_ = false;
    };
}
