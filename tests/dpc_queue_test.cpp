#include "runtime/kernel/dpc_queue.h"

#ifdef NDEBUG
#undef NDEBUG
#endif
#include <cassert>
#include <thread>

using kernel::DpcQueue;
using kernel::DpcWork;

int main()
{
    DpcQueue q;
    DpcWork w{};

    // Unknown DPC, null arguments.
    assert(!q.Insert(0x1000, 1, 2));
    assert(!q.Initialize(0, 0x820A9568, 5));
    assert(!q.Initialize(0x1000, 0, 5));

    // Queue, argument capture, duplicate suppression.
    assert(q.Initialize(0x1000, 0x820A9568, 0xC0DE));
    assert(q.Insert(0x1000, 7, 8));
    assert(!q.Insert(0x1000, 9, 9));
    assert(q.TryPop(w));
    assert(w.dpc == 0x1000 && w.routine == 0x820A9568 && w.context == 0xC0DE);
    assert(w.arg1 == 7 && w.arg2 == 8);
    assert(!q.TryPop(w));

    // Requeue after dispatch (and from inside the routine) is allowed.
    assert(q.Insert(0x1000, 1, 1));
    assert(q.TryPop(w));
    assert(q.Insert(0x1000, 2, 2));
    assert(q.TryPop(w) && w.arg1 == 2);

    // FIFO across DPCs.
    assert(q.Initialize(0x2000, 0x82000010, 1));
    assert(q.Insert(0x2000, 0, 0));
    assert(q.Insert(0x1000, 0, 0));
    assert(q.TryPop(w) && w.dpc == 0x2000);
    assert(q.TryPop(w) && w.dpc == 0x1000);

    // Re-initialising a queued DPC drops its pending work.
    assert(q.Insert(0x1000, 3, 3));
    assert(q.Initialize(0x1000, 0x82000020, 4));
    assert(!q.TryPop(w));
    assert(q.Insert(0x1000, 3, 3));
    assert(q.TryPop(w) && w.routine == 0x82000020 && w.context == 4);

    // A blocked waiter wakes for work, and for Stop.
    DpcWork got{};
    std::thread t([&] { assert(q.Wait(got)); });
    q.Insert(0x2000, 11, 12);
    t.join();
    assert(got.arg1 == 11);
    std::thread s([&] { DpcWork x; assert(!q.Wait(x)); });
    q.Stop();
    s.join();
    return 0;
}
