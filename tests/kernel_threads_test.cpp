// Asset-free test of guest thread creation (runtime/kernel/threads.cpp):
// ExCreateThread, suspend and resume, the XAPI start-up shim, the processor
// number the title reads from its KPCR, and a thread's exit being what a wait on
// it waits for. "Guest code" here is a handful of plain C++ functions standing
// in for recompiled ones; no game code or data is involved.
#include "tests/fake_guest.h"
#include "runtime/crash.h"
#include "runtime/kernel/objects.h"

#ifdef NDEBUG
#undef NDEBUG
#endif
#include <atomic>
#include <cassert>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <map>
#include <mutex>
#include <set>
#include <thread>

PPC_FUNC(__imp__ExCreateThread);
PPC_FUNC(__imp__NtResumeThread);
PPC_FUNC(__imp__KeResumeThread);
PPC_FUNC(__imp__KeSetAffinityThread);

// ---- the fake guest code and the services threads.cpp links against ----------
namespace
{
    constexpr uint32_t kRoutine = 0x82001000, kShim = 0x82002000, kNoCode = 0x82004000;
    constexpr uint32_t kKpcrCurrentThread = 256, kKpcrProcessor = 268;

    std::mutex g_lock;
    struct Seen { int runs = 0; uint32_t r1 = 0, r3 = 0, r4 = 0, r13 = 0; };
    std::map<uint32_t, Seen> g_seen;          // by the entry address it ran
    std::map<uint32_t, uint32_t> g_pcrOf;     // thread id -> the KPCR handed out

    void Record(uint32_t entry, PPCContext& ctx)
    {
        std::lock_guard g(g_lock);
        Seen& s = g_seen[entry];
        s.runs++;
        s.r1 = ctx.r1.u32; s.r3 = ctx.r3.u32; s.r4 = ctx.r4.u32; s.r13 = ctx.r13.u32;
    }
    Seen SeenAt(uint32_t entry) { std::lock_guard g(g_lock); return g_seen[entry]; }

    void Routine(PPCContext& ctx, uint8_t*) { Record(kRoutine, ctx); }
    void Shim(PPCContext& ctx, uint8_t*)    { Record(kShim, ctx); }
}

namespace kernel
{
    PPCFunc* GuestFunction(uint32_t address)
    {
        if (address == kRoutine) return &Routine;
        if (address == kShim) return &Shim;
        return nullptr;   // kNoCode and anything else: no recompiled function there
    }

    uint32_t CreateThreadBlock(uint32_t threadId)
    {
        const uint32_t pcr = AllocateGuest(0x1000, 0x1000);
        const uint32_t kthread = AllocateGuest(0x100, 0x10);
        *GuestPtr<be32>(pcr + kKpcrCurrentThread) = kthread;
        std::lock_guard g(g_lock);
        g_pcrOf[threadId] = pcr;
        return pcr;
    }
}

namespace crash
{
    void RegisterThread(const char*) {}
    void UnregisterThread() {}
    const char* CurrentThreadName() { return "test"; }
}

using namespace kernel;

namespace
{
    constexpr uint32_t kHandleOut = 0x3000, kThreadIdOut = 0x3010, kPrevious = 0x3020;
    constexpr uint32_t kSuspended = 1;

    struct Created { uint32_t status = 0, handle = 0, id = 0, pcr = 0, kthread = 0; };

    Created Create(uint32_t start, uint32_t context, uint32_t flags = 0, uint32_t shim = 0,
                   uint32_t stack = 0)
    {
        *GuestPtr<be32>(kHandleOut) = 0;
        *GuestPtr<be32>(kThreadIdOut) = 0;
        PPCContext ctx;
        ctx.r3.u64 = kHandleOut;
        ctx.r4.u64 = stack;
        ctx.r5.u64 = kThreadIdOut;
        ctx.r6.u64 = shim;
        ctx.r7.u64 = start;
        ctx.r8.u64 = context;
        ctx.r9.u64 = flags;
        __imp__ExCreateThread(ctx, guest::Base());
        Created c;
        c.status = uint32_t(ctx.r3.u64);
        c.handle = *GuestPtr<be32>(kHandleOut);
        c.id = *GuestPtr<be32>(kThreadIdOut);
        if (c.status == X_STATUS_SUCCESS)
        {
            std::lock_guard g(g_lock);
            c.pcr = g_pcrOf[c.id];
            c.kthread = *GuestPtr<be32>(c.pcr + kKpcrCurrentThread);
        }
        return c;
    }

    // What waiting on a thread's handle does: it is signalled once it has exited.
    uint32_t WaitExit(uint32_t handle, int64_t timeoutMs)
    {
        auto thread = LookupHandleAs<Dispatcher>(handle);
        assert(thread);
        Dispatcher* list[] = { thread.get() };
        int64_t timeout = -timeoutMs * 10'000;
        return Wait(list, 1, false, &timeout);
    }

    void Resume(uint32_t handle)
    {
        PPCContext ctx;
        ctx.r3.u64 = handle;
        ctx.r4.u64 = kPrevious;
        __imp__NtResumeThread(ctx, guest::Base());
        assert(uint32_t(ctx.r3.u64) == X_STATUS_SUCCESS);
    }

    void BadStartAddress()
    {
        for (uint32_t bad : { 0u, 0x00010000u, 0x81FFFFFCu, PPC_CODE_BASE + PPC_CODE_SIZE })
        {
            Created c = Create(bad, 0);
            assert(c.status == X_STATUS_INVALID_PARAMETER && c.handle == 0);
        }
    }

    void RunsAndExits()
    {
        Created c = Create(kRoutine, 0xC0DE);
        assert(c.status == X_STATUS_SUCCESS && c.handle && c.id >= 2);
        assert(WaitExit(c.handle, 5000) == 0);          // signalled when it returns

        const Seen s = SeenAt(kRoutine);
        assert(s.runs == 1);
        assert(s.r3 == 0xC0DE);                          // the routine gets its context
        assert(s.r13 == c.pcr);                          // r13 is the KPCR the kernel made
        assert(s.r1 % 16 == 0 && s.r1 > fake::kHeapBase && s.r1 < fake::kHeapEnd);   // inside its stack

        // The KTHREAD in the KPCR is the thread object's dispatcher header.
        assert(ObjectAt(c.kthread) == LookupHandle(c.handle));

        // Ids are distinct and rise.
        Created d = Create(kRoutine, 1);
        assert(d.id > c.id && d.pcr != c.pcr);
        assert(WaitExit(d.handle, 5000) == 0);
    }

    void SuspendedUntilResumed()
    {
        const int before = SeenAt(kRoutine).runs;
        Created c = Create(kRoutine, 5, kSuspended);
        assert(c.status == X_STATUS_SUCCESS);
        assert(WaitExit(c.handle, 80) == X_STATUS_TIMEOUT);   // not running, not exited
        assert(SeenAt(kRoutine).runs == before);

        *GuestPtr<be32>(kPrevious) = 99;
        Resume(c.handle);
        assert(uint32_t(*GuestPtr<be32>(kPrevious)) == 1);
        assert(WaitExit(c.handle, 5000) == 0);
        assert(SeenAt(kRoutine).runs == before + 1);

        // KeResumeThread takes the KTHREAD rather than a handle.
        Created k = Create(kRoutine, 6, kSuspended);
        assert(WaitExit(k.handle, 50) == X_STATUS_TIMEOUT);
        PPCContext ctx;
        ctx.r3.u64 = k.kthread;
        __imp__KeResumeThread(ctx, guest::Base());
        assert(WaitExit(k.handle, 5000) == 0);
        assert(SeenAt(kRoutine).runs == before + 2);
    }

    void EntersThroughTheXapiShim()
    {
        // The kernel enters XAPI's start-up shim, handing it the routine and its
        // context, rather than jumping straight to the routine.
        const int routineRuns = SeenAt(kRoutine).runs;
        Created c = Create(kRoutine, 0xBEEF, 0, kShim);
        assert(WaitExit(c.handle, 5000) == 0);
        const Seen s = SeenAt(kShim);
        assert(s.runs == 1 && s.r3 == kRoutine && s.r4 == 0xBEEF && s.r13 == c.pcr);
        assert(SeenAt(kRoutine).runs == routineRuns);   // the shim here does not call it
    }

    void ProcessorNumber()
    {
        // The top byte of the creation flags is a one-bit mask naming the hardware thread.
        Created c = Create(kRoutine, 0, kSuspended | (1u << (2 + 24)));
        assert(*GuestPtr<uint8_t>(c.pcr + kKpcrProcessor) == 2);

        Created none = Create(kRoutine, 0, kSuspended);
        assert(*GuestPtr<uint8_t>(none.pcr + kKpcrProcessor) == 0);

        // KeSetAffinityThread moves it, and the title reads the number back from the KPCR.
        PPCContext ctx;
        ctx.r3.u64 = c.kthread;
        ctx.r4.u64 = 1u << 4;
        __imp__KeSetAffinityThread(ctx, guest::Base());
        assert(ctx.r3.u64 == (1u << 4));
        assert(*GuestPtr<uint8_t>(c.pcr + kKpcrProcessor) == 4);

        ctx.r3.u64 = none.kthread;
        ctx.r4.u64 = 1u << 1;
        __imp__KeSetAffinityThread(ctx, guest::Base());
        assert(*GuestPtr<uint8_t>(none.pcr + kKpcrProcessor) == 1);
        assert(*GuestPtr<uint8_t>(c.pcr + kKpcrProcessor) == 4);   // threads are independent

        Resume(c.handle);
        Resume(none.handle);
        assert(WaitExit(c.handle, 5000) == 0 && WaitExit(none.handle, 5000) == 0);
    }

    void NoRecompiledFunction()
    {
        // An address in the code range with nothing recompiled behind it ends the
        // thread instead of hanging whoever waits on it.
        Created c = Create(kNoCode, 0);
        assert(c.status == X_STATUS_SUCCESS);
        assert(WaitExit(c.handle, 5000) == 0);
    }

    void ManyAtOnce()
    {
        const int before = SeenAt(kRoutine).runs;
        std::set<uint32_t> ids;
        Created threads[4];
        for (auto& t : threads)
        {
            t = Create(kRoutine, 1, kSuspended);
            assert(t.status == X_STATUS_SUCCESS);
            assert(ids.insert(t.id).second);
        }
        for (auto& t : threads) Resume(t.handle);
        for (auto& t : threads) assert(WaitExit(t.handle, 5000) == 0);
        assert(SeenAt(kRoutine).runs == before + 4);
    }
}

int main()
{
    BadStartAddress();
    RunsAndExits();
    SuspendedUntilResumed();
    EntersThroughTheXapiShim();
    ProcessorNumber();
    NoRecompiledFunction();
    ManyAtOnce();
    std::puts("kernel-threads-test ok");
    std::fflush(stdout);
    return 0;
}
