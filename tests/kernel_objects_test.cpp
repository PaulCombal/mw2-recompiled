// Asset-free test of the kernel's dispatcher objects: events, semaphores,
// mutants, timers, the waits on them, the handle table and the guest-memory
// dispatcher headers. It compiles runtime/kernel/objects.cpp unchanged and runs
// it against a small fake guest memory, so it needs no game files and no
// recompiled code.
#include "tests/fake_guest.h"
#include "runtime/kernel/objects.h"

#ifdef NDEBUG
#undef NDEBUG
#endif
#include <atomic>
#include <cassert>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <memory>
#include <thread>
#include <vector>

using namespace kernel;
using Clock = std::chrono::steady_clock;

namespace
{
    // NT timeouts: negative is relative, in 100 ns units.
    int64_t Ms(int64_t ms) { return -ms * 10'000; }
    constexpr int64_t kPoll = 0;   // already due: a test of the state, not a wait

    uint32_t WaitOne(Dispatcher& d, int64_t timeout)
    {
        Dispatcher* list[] = { &d };
        return Wait(list, 1, false, &timeout);
    }

    void Header(uint32_t address, uint8_t type, uint32_t state, uint32_t limit = 0)
    {
        uint8_t* p = guest::Base() + address;
        for (int i = 0; i < 32; i++) p[i] = 0;
        p[0] = type;
        *GuestPtr<be32>(address + 4) = state;
        *GuestPtr<be32>(address + 16) = limit;
    }

    void Events()
    {
        auto manual = std::make_shared<Event>();
        manual->manualReset = true;
        assert(WaitOne(*manual, kPoll) == X_STATUS_TIMEOUT);
        assert(!manual->Set());
        assert(manual->Set());                       // returns the previous state
        assert(WaitOne(*manual, kPoll) == 0);        // a notification event stays set
        assert(WaitOne(*manual, kPoll) == 0);
        assert(manual->Reset());
        assert(!manual->Reset());
        assert(WaitOne(*manual, kPoll) == X_STATUS_TIMEOUT);

        auto autoEvent = std::make_shared<Event>();
        autoEvent->manualReset = false;
        autoEvent->Set();
        assert(WaitOne(*autoEvent, kPoll) == 0);                 // consumed by the wait
        assert(WaitOne(*autoEvent, kPoll) == X_STATUS_TIMEOUT);

        const auto start = Clock::now();
        assert(WaitOne(*manual, Ms(60)) == X_STATUS_TIMEOUT);
        assert(Clock::now() - start >= std::chrono::milliseconds(45));
    }

    void Semaphores()
    {
        auto s = std::make_shared<Semaphore>();
        s->count = 2;
        s->limit = 3;
        assert(WaitOne(*s, kPoll) == 0);
        assert(WaitOne(*s, kPoll) == 0);
        assert(WaitOne(*s, kPoll) == X_STATUS_TIMEOUT);
        assert(s->count == 0);
        assert(s->Release(2) == 0);
        assert(s->count == 2);

        bool exceeded = false;
        assert(s->Release(5, &exceeded) == 2);       // fails atomically with a status
        assert(exceeded && s->count == 2);
        assert(s->Release(5) == 2);                  // the older path saturates
        assert(s->count == 3);
    }

    void Mutants()
    {
        auto m = std::make_shared<Mutant>();
        assert(WaitOne(*m, kPoll) == 0);
        assert(WaitOne(*m, kPoll) == 0);             // recursive
        assert(m->depth == 2);

        std::atomic<uint32_t> other{ 99 };
        std::thread([&] { other = WaitOne(*m, kPoll); }).join();
        assert(other == X_STATUS_TIMEOUT);           // owned by this thread

        assert(m->Release() == 2);
        assert(m->Release() == 1);
        assert(m->depth == 0 && m->owner == 0);
        std::thread([&] { other = WaitOne(*m, kPoll); }).join();
        assert(other == 0);                          // now free to take
    }

    void WaitAnyAndAll()
    {
        auto a = std::make_shared<Event>();
        auto b = std::make_shared<Event>();
        a->manualReset = b->manualReset = false;
        Dispatcher* both[] = { a.get(), b.get() };

        int64_t poll = kPoll;
        assert(Wait(both, 2, false, &poll) == X_STATUS_TIMEOUT);
        b->Set();
        assert(Wait(both, 2, false, &poll) == 1);    // STATUS_WAIT_0 + index
        assert(!b->signalled);

        // A wait for all is atomic: with one missing, nothing is consumed.
        a->Set();
        assert(Wait(both, 2, true, &poll) == X_STATUS_TIMEOUT);
        assert(a->signalled);
        b->Set();
        assert(Wait(both, 2, true, &poll) == X_STATUS_SUCCESS);
        assert(!a->signalled && !b->signalled);
    }

    void BlockedWaitIsWoken()
    {
        auto e = std::make_shared<Event>();
        std::atomic<bool> started{ false };
        std::atomic<uint32_t> result{ 99 };
        std::thread waiter([&] {
            started = true;
            Dispatcher* list[] = { e.get() };
            result = Wait(list, 1, false, nullptr);   // for ever
        });
        while (!started) std::this_thread::yield();
        std::this_thread::sleep_for(std::chrono::milliseconds(30));
        assert(result == 99);                         // still asleep, not polling to a result
        e->Set();
        waiter.join();
        assert(result == 0);

        // An absolute timeout (positive) falls due at that system time.
        auto never = std::make_shared<Event>();
        int64_t absolute = int64_t(SystemTime100ns()) + 40 * 10'000;
        Dispatcher* list[] = { never.get() };
        const auto start = Clock::now();
        assert(Wait(list, 1, false, &absolute) == X_STATUS_TIMEOUT);
        assert(Clock::now() - start >= std::chrono::milliseconds(30));
    }

    void Handles()
    {
        auto e = std::make_shared<Event>();
        const uint32_t h1 = InsertHandle(e);
        const uint32_t h2 = InsertHandle(std::make_shared<Semaphore>());
        assert(h1 && h2 && h1 != h2);
        assert(e->handle == h1);
        assert(LookupHandle(h1) == e);
        assert(LookupHandleAs<Event>(h1) == e);
        assert(!LookupHandleAs<Event>(h2));           // right handle, wrong type
        assert(!LookupHandle(0xDEADBEEF));
        assert(CloseHandle(h1));
        assert(!CloseHandle(h1));
        assert(!LookupHandle(h1));
    }

    void GuestHeaders()
    {
        assert(!ObjectAt(0));

        // A header the title initialised itself is taken up with its state.
        Header(0x2000, 1, 1);                         // a synchronization event, signalled
        auto event = ObjectAtAs<Event>(0x2000);
        assert(event && !event->manualReset && event->signalled);
        assert(ObjectAt(0x2000) == event);            // the same object the second time

        Header(0x2100, 5, 2, 4);                      // a semaphore, count 2, limit 4
        auto sem = ObjectAtAs<Semaphore>(0x2100);
        assert(sem && sem->count == 2 && sem->limit == 4);

        Header(0x2200, 2, 1);
        assert(ObjectAtAs<Mutant>(0x2200));
        Header(0x2300, 8, 0);
        auto timer = ObjectAtAs<Timer>(0x2300);
        assert(timer && timer->manualReset && !timer->signalled);
        Header(0x2400, 99, 0);
        assert(!ObjectAt(0x2400));                    // not a dispatcher type we know

        // An object made here gets a header on demand, of the right type.
        auto manual = std::make_shared<Event>();
        auto autoEvent = std::make_shared<Event>();
        autoEvent->manualReset = false;
        auto sem2 = std::make_shared<Semaphore>();
        auto mutant = std::make_shared<Mutant>();
        auto sync = std::make_shared<Timer>();
        sync->manualReset = false;
        const uint32_t hm = HeaderOf(manual), ha = HeaderOf(autoEvent), hs = HeaderOf(sem2),
                       hu = HeaderOf(mutant), ht = HeaderOf(sync);
        assert(*GuestPtr<uint8_t>(hm) == 0 && *GuestPtr<uint8_t>(ha) == 1);
        assert(*GuestPtr<uint8_t>(hs) == 5 && *GuestPtr<uint8_t>(hu) == 2);
        assert(*GuestPtr<uint8_t>(ht) == 9);
        assert(HeaderOf(manual) == hm);               // stable
        assert(ObjectAt(hm) == manual && ObjectAt(hs) == sem2);
        assert(!HeaderOf(nullptr));
    }

    void Timers()
    {
        auto oneShot = std::make_shared<Timer>();     // notification timer
        assert(!SetTimer(oneShot, Ms(20), 0));
        assert(WaitOne(*oneShot, Ms(2000)) == 0);
        assert(!oneShot->running);

        auto periodic = std::make_shared<Timer>();
        periodic->manualReset = false;
        SetTimer(periodic, Ms(10), 10);
        for (int i = 0; i < 4; i++) assert(WaitOne(*periodic, Ms(2000)) == 0);
        assert(CancelTimer(periodic));
        assert(!CancelTimer(periodic));

        auto cancelled = std::make_shared<Timer>();
        SetTimer(cancelled, Ms(100), 0);
        assert(CancelTimer(cancelled));
        assert(WaitOne(*cancelled, Ms(250)) == X_STATUS_TIMEOUT);

        // Setting a running timer again replaces it and reports it was running.
        auto again = std::make_shared<Timer>();
        SetTimer(again, Ms(5000), 0);
        assert(SetTimer(again, Ms(20), 0));
        assert(WaitOne(*again, Ms(2000)) == 0);
    }
}

int main()
{
    Events();
    Semaphores();
    Mutants();
    WaitAnyAndAll();
    BlockedWaitIsWoken();
    Handles();
    GuestHeaders();
    Timers();
    std::puts("kernel-objects-test ok");
    std::fflush(stdout);
    return 0;
}
