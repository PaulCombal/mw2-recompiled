// KeInitializeDpc / KeInsertQueueDpc.
//
//   void    KeInitializeDpc(KDPC* dpc, PKDEFERRED_ROUTINE routine, PVOID context);
//   BOOLEAN KeInsertQueueDpc(KDPC* dpc, PVOID arg1, PVOID arg2);
//
// The routine is called as routine(dpc, context, arg1, arg2) on one dedicated
// worker, after the queuing call has returned, one DPC at a time -- the console
// runs them on the processor that took the interrupt, never re-entrantly. The
// worker owns a guest stack and thread block, like the audio worker, so the
// routine reads a valid r13. The KDPC's own memory is left untouched: the title
// only passes the pointer around (docs/MW3-DPC-INVESTIGATION.md), and the
// queued/not-queued state lives in DpcQueue.
//
// Not modelled: IRQL, processor affinity, importance, KeRemoveQueueDpc.
#include <ppc_recomp_shared.h>

#include "dpc_queue.h"
#include "kernel.h"
#include "../crash.h"
#include "../guest.h"
#include "../log.h"

#include <mutex>
#include <thread>

namespace
{
    constexpr uint32_t kStackSize = 256u * 1024u;

    kernel::DpcQueue g_queue;
    std::once_flag g_started;

    void Worker()
    {
        crash::RegisterThread("dpc worker");
        const uint32_t stack = kernel::AllocateGuest(kStackSize, 0x1000);
        const uint32_t pcr = kernel::CreateThreadBlock(kernel::NewThreadId());
        if (!stack || !pcr)
        {
            LOGE("dpc: could not set up the worker's guest state");
            return;
        }
        const uint32_t top = (stack + kStackSize - 0x100) & ~0xFu;

        kernel::DpcWork w;
        while (g_queue.Wait(w))
        {
            PPCFunc* fn = kernel::GuestFunction(w.routine);
            if (!fn)
            {
                LOGW("dpc: no recompiled function at routine %08X", w.routine);
                continue;
            }
            PPCContext ctx{};
            ctx.r1.u32 = top;
            ctx.r13.u32 = pcr;
            ctx.r3.u32 = w.dpc;
            ctx.r4.u32 = w.context;
            ctx.r5.u32 = w.arg1;
            ctx.r6.u32 = w.arg2;
            ctx.fpscr.loadFromHost();
            fn(ctx, guest::Base());
        }
    }
}

PPC_FUNC(__imp__KeInitializeDpc)
{
    if (!g_queue.Initialize(ctx.r3.u32, ctx.r4.u32, ctx.r5.u32))
        LOGW("KeInitializeDpc: rejected dpc=%08X routine=%08X", ctx.r3.u32, ctx.r4.u32);
    std::call_once(g_started, [] { std::thread(Worker).detach(); });
}

PPC_FUNC(__imp__KeInsertQueueDpc)
{
    ctx.r3.u64 = g_queue.Insert(ctx.r3.u32, ctx.r4.u32, ctx.r5.u32) ? 1 : 0;
}
