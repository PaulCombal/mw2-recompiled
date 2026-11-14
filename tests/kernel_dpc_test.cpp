// Asset-free test of DPC delivery end to end (runtime/kernel/dpc.cpp): the
// KeInitializeDpc and KeInsertQueueDpc imports, the worker they start, and what
// the routine sees when it runs. The queue's own bookkeeping is covered by
// dpc_queue_test; this is the dispatch around it: that a routine never runs
// inline in the queuing call, one at a time, with the four arguments and a valid
// guest stack and KPCR, and that requeueing, re-initialising and a missing
// routine all behave. The last case plays the shape the investigation recovered
// for MW3's graphics-completion path (six records, a four-slot ring, an event)
// with synthetic code and addresses; no game code or data is involved.
#include "tests/fake_guest.h"
#include "runtime/crash.h"
#include "runtime/kernel/objects.h"

#ifdef NDEBUG
#undef NDEBUG
#endif
#include <atomic>
#include <cassert>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <cstdio>
#include <functional>
#include <map>
#include <mutex>
#include <thread>
#include <vector>

PPC_FUNC(__imp__KeInitializeDpc);
PPC_FUNC(__imp__KeInsertQueueDpc);

using namespace kernel;

namespace
{
    // ---- the routines a DPC can name: plain C++ standing in for guest code -----
    constexpr uint32_t kRecord = 0x82010000, kBlocking = 0x82010010, kRequeue = 0x82010020,
                       kCompletion = 0x82010030, kExclusive = 0x82010040, kNoCode = 0x82019990;
    constexpr uint32_t kKpcrCurrentThread = 256;

    struct Call
    {
        uint32_t routine, dpc, context, arg1, arg2, r1, r13;
        std::thread::id thread;
    };

    std::mutex g_lock;
    std::vector<Call> g_calls;

    // A gate a blocking routine waits at until the test opens it.
    std::mutex g_gateLock;
    std::condition_variable g_gateChanged;
    bool g_gateOpen = true;
    std::atomic<int> g_blocked{ 0 };

    void SetGate(bool open)
    {
        { std::lock_guard g(g_gateLock); g_gateOpen = open; }
        g_gateChanged.notify_all();
    }

    void Log(uint32_t routine, PPCContext& ctx)
    {
        std::lock_guard g(g_lock);
        g_calls.push_back({ routine, ctx.r3.u32, ctx.r4.u32, ctx.r5.u32, ctx.r6.u32,
                            ctx.r1.u32, ctx.r13.u32, std::this_thread::get_id() });
    }

    void Record(PPCContext& ctx, uint8_t*) { Log(kRecord, ctx); }

    void Blocking(PPCContext& ctx, uint8_t*)
    {
        g_blocked++;
        {
            std::unique_lock g(g_gateLock);
            g_gateChanged.wait(g, [] { return g_gateOpen; });
        }
        g_blocked--;
        Log(kBlocking, ctx);
    }

    // Queues its own DPC again from inside, until arg1 reaches 3.
    std::vector<uint32_t> g_requeueResults;
    void Requeue(PPCContext& ctx, uint8_t* base)
    {
        Log(kRequeue, ctx);
        if (ctx.r5.u32 >= 3) return;
        PPCContext call;
        call.r3.u64 = ctx.r3.u32;
        call.r4.u64 = ctx.r5.u32 + 1;
        call.r5.u64 = ctx.r6.u32;
        __imp__KeInsertQueueDpc(call, base);
        std::lock_guard g(g_lock);
        g_requeueResults.push_back(uint32_t(call.r3.u64));
    }

    std::atomic<int> g_active{ 0 }, g_maxActive{ 0 };
    void Exclusive(PPCContext& ctx, uint8_t*)
    {
        const int now = ++g_active;
        int seen = g_maxActive;
        while (now > seen && !g_maxActive.compare_exchange_weak(seen, now)) {}
        std::this_thread::sleep_for(std::chrono::microseconds(300));
        Log(kExclusive, ctx);
        --g_active;
    }

    // The completion callback as the notes describe it: the context holds a
    // producer counter at +0x54, a four-slot ring at +0x5C, and an event at
    // +0x3C. It stores the completion value, bumps the counter, signals.
    void Completion(PPCContext& ctx, uint8_t*)
    {
        const uint32_t context = ctx.r4.u32, value = ctx.r5.u32;
        const uint32_t produced = *GuestPtr<be32>(context + 0x54);
        *GuestPtr<be32>(context + 0x5C + (produced & 3) * 4) = value;
        *GuestPtr<be32>(context + 0x54) = produced + 1;
        if (auto event = ObjectAtAs<Event>(context + 0x3C)) event->Set();
    }
}

namespace kernel
{
    PPCFunc* GuestFunction(uint32_t address)
    {
        switch (address)
        {
        case kRecord:     return &Record;
        case kBlocking:   return &Blocking;
        case kRequeue:    return &Requeue;
        case kCompletion: return &Completion;
        case kExclusive:  return &Exclusive;
        default:          return nullptr;
        }
    }

    uint32_t NewThreadId()
    {
        static std::atomic<uint32_t> next{ 100 };
        return next++;
    }

    uint32_t CreateThreadBlock(uint32_t)
    {
        const uint32_t pcr = AllocateGuest(0x1000, 0x1000);
        *GuestPtr<be32>(pcr + kKpcrCurrentThread) = AllocateGuest(0x100, 0x10);
        return pcr;
    }
}

namespace crash
{
    std::vector<std::string> g_threads;
    void RegisterThread(const char* name) { std::lock_guard g(g_lock); g_threads.push_back(name); }
    void UnregisterThread() {}
    const char* CurrentThreadName() { return "test"; }
}

namespace
{
    void Init(uint32_t dpc, uint32_t routine, uint32_t context)
    {
        PPCContext ctx;
        ctx.r3.u64 = dpc; ctx.r4.u64 = routine; ctx.r5.u64 = context;
        __imp__KeInitializeDpc(ctx, guest::Base());
    }

    bool Insert(uint32_t dpc, uint32_t arg1 = 0, uint32_t arg2 = 0)
    {
        PPCContext ctx;
        ctx.r3.u64 = dpc; ctx.r4.u64 = arg1; ctx.r5.u64 = arg2;
        __imp__KeInsertQueueDpc(ctx, guest::Base());
        assert(ctx.r3.u64 == 0 || ctx.r3.u64 == 1);
        return ctx.r3.u64 == 1;
    }

    bool WaitFor(const std::function<bool()>& done, int ms = 5000)
    {
        const auto limit = std::chrono::steady_clock::now() + std::chrono::milliseconds(ms);
        while (!done())
        {
            if (std::chrono::steady_clock::now() > limit) return false;
            std::this_thread::sleep_for(std::chrono::milliseconds(1));
        }
        return true;
    }

    size_t Calls() { std::lock_guard g(g_lock); return g_calls.size(); }
    Call CallAt(size_t i) { std::lock_guard g(g_lock); return g_calls[i]; }
    bool CallsReach(size_t n) { return WaitFor([&] { return Calls() >= n; }); }

    void BeforeAnythingIsInitialised()
    {
        assert(!Insert(0x1000, 1, 2));                 // an unknown DPC is not queued
    }

    void RejectedInitialisation()
    {
        Init(0, kRecord, 1);                           // no DPC
        Init(0x1000, 0, 1);                            // no routine
        assert(!Insert(0x1000));
        assert(!Insert(0));
    }

    void RunsOnTheWorkerWithItsArguments()
    {
        const size_t before = Calls();
        Init(0x2000, kRecord, 0xC0DE);
        assert(Insert(0x2000, 7, 8));
        assert(CallsReach(before + 1));
        const Call c = CallAt(before);
        assert(c.routine == kRecord && c.dpc == 0x2000 && c.context == 0xC0DE);
        assert(c.arg1 == 7 && c.arg2 == 8);
        assert(c.thread != std::this_thread::get_id());   // never inline in the queuing call
        assert(c.r1 % 16 == 0 && c.r1 > fake::kHeapBase && c.r1 < fake::kHeapEnd);   // a guest stack
        assert(c.r13 != 0 && *GuestPtr<be32>(c.r13 + kKpcrCurrentThread) != 0);        // a thread block

        // Every DPC runs on the same worker, which is named for the crash reports.
        Init(0x2100, kRecord, 1);
        assert(Insert(0x2100, 1, 1));
        assert(CallsReach(before + 2));
        assert(CallAt(before + 1).thread == c.thread && CallAt(before + 1).r13 == c.r13);
        std::lock_guard g(g_lock);
        assert(crash::g_threads.size() == 1 && crash::g_threads[0] == "dpc worker");
    }

    void NotInlineAndRequeueWhileRunning()
    {
        const size_t before = Calls();
        Init(0x3000, kBlocking, 0xA);
        Init(0x3100, kRecord, 0xB);
        SetGate(false);
        assert(Insert(0x3000, 1, 0));                  // returns although the routine will block
        assert(WaitFor([] { return g_blocked == 1; })); // it is running, and stuck

        // The queued flag was cleared before the routine started, so it can queue again;
        // a second request while that is still waiting is refused.
        assert(Insert(0x3000, 2, 0));
        assert(!Insert(0x3000, 3, 0));
        assert(Insert(0x3100, 4, 0));
        assert(Calls() == before);                     // nothing has finished: one at a time

        SetGate(true);
        assert(CallsReach(before + 3));
        // First the one that was running, then in the order they were queued.
        assert(CallAt(before).arg1 == 1 && CallAt(before + 1).arg1 == 2 && CallAt(before + 2).arg1 == 4);
        assert(CallAt(before + 1).routine == kBlocking && CallAt(before + 2).routine == kRecord);
    }

    void RequeueFromInsideTheRoutine()
    {
        const size_t before = Calls();
        Init(0x4000, kRequeue, 0);
        assert(Insert(0x4000, 0, 0x55));
        assert(CallsReach(before + 4));                // arg1 runs 0, 1, 2, 3
        for (uint32_t i = 0; i < 4; i++)
        {
            const Call c = CallAt(before + i);
            assert(c.arg1 == i && c.arg2 == 0x55 && c.dpc == 0x4000);
        }
        std::this_thread::sleep_for(std::chrono::milliseconds(20));
        assert(Calls() == before + 4);                 // and then stops
        std::lock_guard g(g_lock);
        assert(g_requeueResults.size() == 3);
        for (uint32_t r : g_requeueResults) assert(r == 1);   // the routine could queue itself
    }

    void ReinitialisingDropsPendingWork()
    {
        const size_t before = Calls();
        Init(0x5000, kBlocking, 1);
        Init(0x5100, kRecord, 0x11);
        SetGate(false);
        assert(Insert(0x5000, 1, 0));
        assert(WaitFor([] { return g_blocked == 1; }));
        assert(Insert(0x5100, 99, 99));                // waiting behind the blocked one
        Init(0x5100, kRecord, 0x22);                   // dropped, and the new context takes over
        assert(Insert(0x5100, 5, 6));                  // so it can be queued afresh
        SetGate(true);
        assert(CallsReach(before + 2));
        std::this_thread::sleep_for(std::chrono::milliseconds(30));
        assert(Calls() == before + 2);                 // the dropped one never ran
        const Call c = CallAt(before + 1);
        assert(c.dpc == 0x5100 && c.context == 0x22 && c.arg1 == 5 && c.arg2 == 6);
    }

    void MissingRoutineIsSkipped()
    {
        const size_t before = Calls();
        Init(0x6000, kNoCode, 1);
        Init(0x6100, kRecord, 2);
        assert(Insert(0x6000, 1, 1));                  // accepted
        assert(Insert(0x6100, 2, 2));
        assert(CallsReach(before + 1));                // the next one still runs
        assert(CallAt(before).dpc == 0x6100);
    }

    void OneAtATimeUnderLoad()
    {
        const size_t before = Calls();
        constexpr uint32_t kDpcs = 6, kPerThread = 150;
        for (uint32_t d = 0; d < kDpcs; d++) Init(0x7000 + d * 0x20, kExclusive, d);

        std::vector<std::thread> producers;
        for (uint32_t t = 0; t < 3; t++)
            producers.emplace_back([t] {
                for (uint32_t i = 0; i < kPerThread; i++)
                {
                    const uint32_t dpc = 0x7000 + ((i + t) % kDpcs) * 0x20;
                    const uint32_t value = (t << 16) | i;
                    // Refused only while that DPC is still waiting; it is taken once it has run.
                    while (!Insert(dpc, value, 0)) std::this_thread::yield();
                }
            });
        for (auto& p : producers) p.join();
        assert(CallsReach(before + 3 * kPerThread));
        assert(g_maxActive == 1);                      // never two routines at once

        // Every value arrived exactly once, and each producer's came in the order it queued them.
        std::map<uint32_t, int> seen;
        std::map<std::pair<uint32_t, uint32_t>, int64_t> lastIndex;   // (dpc, producer) -> index
        for (size_t i = before; i < Calls(); i++)
        {
            const Call c = CallAt(i);
            if (c.routine != kExclusive) continue;
            seen[c.arg1]++;
            const uint32_t producer = c.arg1 >> 16, index = c.arg1 & 0xFFFF;
            int64_t& last = lastIndex.try_emplace({ c.dpc, producer }, -1).first->second;
            assert(int64_t(index) > last);
            last = index;
        }
        assert(seen.size() == 3 * kPerThread);
        for (auto& [value, n] : seen) assert(n == 1);
    }

    void GraphicsCompletionShape()
    {
        // Six records, 0x6C apart, each a context for the completion callback and
        // a DPC 0x20 bytes into the record, inside one parent object.
        constexpr uint32_t kParent = 0x00B00000, kContexts = kParent + 0x2CC0, kDpcs = kParent + 0x2CE0;
        constexpr uint32_t kStride = 0x6C, kEntries = 6;
        for (uint32_t i = 0; i < kEntries; i++)
        {
            const uint32_t context = kContexts + i * kStride;
            // The title initialises the event in the context; this one is auto-reset.
            uint8_t* header = guest::Base() + context + 0x3C;
            for (int b = 0; b < 16; b++) header[b] = 0;
            header[0] = 1;
            Init(kDpcs + i * kStride, kCompletion, context);
        }

        // The consumer is a guest thread in the title: it waits on the event, then
        // reads every record the producer has added since it last looked.
        struct Consumed { uint32_t entry, value; };
        std::mutex lock;
        std::vector<Consumed> consumed;
        std::atomic<bool> stop{ false };
        std::vector<std::thread> consumers;
        for (uint32_t i = 0; i < kEntries; i++)
            consumers.emplace_back([&, i] {
                const uint32_t context = kContexts + i * kStride;
                auto event = ObjectAtAs<Event>(context + 0x3C);
                assert(event);
                while (!stop)
                {
                    Dispatcher* list[] = { event.get() };
                    int64_t timeout = -20 * 10'000;
                    Wait(list, 1, false, &timeout);
                    for (;;)
                    {
                        const uint32_t produced = *GuestPtr<be32>(context + 0x54);
                        const uint32_t taken = *GuestPtr<be32>(context + 0x58);
                        if (taken == produced) break;
                        const uint32_t value = *GuestPtr<be32>(context + 0x5C + (taken & 3) * 4);
                        *GuestPtr<be32>(context + 0x58) = taken + 1;
                        std::lock_guard g(lock);
                        consumed.push_back({ i, value });
                    }
                }
            });

        // The graphics interrupt queues the entry's DPC with a completion value.
        // A value is only offered once the entry has caught up, as the ring is four deep.
        constexpr uint32_t kRounds = 40;
        for (uint32_t round = 0; round < kRounds; round++)
            for (uint32_t i = 0; i < kEntries; i++)
            {
                const uint32_t context = kContexts + i * kStride;
                const uint32_t value = 0x1000 * (i + 1) + round;
                assert(WaitFor([&] {
                    return uint32_t(*GuestPtr<be32>(context + 0x54)) - uint32_t(*GuestPtr<be32>(context + 0x58)) < 3; }));
                while (!Insert(kDpcs + i * kStride, value, 0)) std::this_thread::yield();
            }
        assert(WaitFor([&] { std::lock_guard g(lock); return consumed.size() == kRounds * kEntries; }));
        stop = true;
        for (auto& c : consumers) c.join();

        // Each entry's values arrive in the order they were completed, none lost or repeated.
        std::vector<uint32_t> next(kEntries, 0);
        for (const Consumed& c : consumed)
        {
            assert(c.value == 0x1000 * (c.entry + 1) + next[c.entry]);
            next[c.entry]++;
        }
        for (uint32_t n : next) assert(n == kRounds);
    }
}

int main()
{
    BeforeAnythingIsInitialised();
    RejectedInitialisation();
    RunsOnTheWorkerWithItsArguments();
    NotInlineAndRequeueWhileRunning();
    RequeueFromInsideTheRoutine();
    ReinitialisingDropsPendingWork();
    MissingRoutineIsSkipped();
    OneAtATimeUnderLoad();
    GraphicsCompletionShape();
    std::puts("kernel-dpc-test ok");
    std::fflush(stdout);
    // The worker is detached and still waiting on the queue, which static
    // destruction would pull out from under it; the game never exits normally.
    std::_Exit(0);
}
