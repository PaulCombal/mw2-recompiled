// The recorder thread and the queue that feeds it. See recorder.h.
//
// One ring of bytes, one producer (the ring consumer) and one reader (the
// recorder). A call is a header and the call's copy of its arguments, placed
// at the end of the ring and made visible by moving `written` past it; the
// recorder makes it and moves `made` past it. A call that would run off the
// end of the ring starts again at the beginning, behind a header that says so.
//
// The recorder works in bursts. It sleeps while fewer than kWakeBytes are
// queued and none of it is due, and the consumer wakes it once that many are
// waiting, or at once for a call that has to reach the GPU (a submission, the
// frame's hand-off: Host), when a thread waits for the recorder (WaitMade), and
// when the consumer has had nothing to read for a while (Flush). A call is queued
// every two microseconds or so, and the recorder makes one faster than that,
// so a recorder that spun until the next one arrived never slept: it held a
// whole core spinning, which on a four-core machine is a core the consumer
// and the title's threads are short of. A wake is a system call, so it is not
// made per call either.
#include "recorder.h"
#include "../../diagnostics.h"
#include "../../crash.h"
#include "../../env.h"
#include "../../log.h"
#include "../../report.h"

#include <atomic>
#include <thread>

#include <immintrin.h>

namespace vk::record::detail
{
    bool threaded = false;
    VkCommandBuffer into = VK_NULL_HANDLE;
    void (*settle)() = nullptr;
}

namespace
{
    using vk::record::detail::Header;

    // A frame queues about 2 MB; this is several frames.
    constexpr uint64_t kBytes = 16u << 20;
    // Pauses before the consumer sleeps waiting for the recorder: tens of
    // microseconds. The recorder does not spin.
    constexpr uint32_t kSpins = 4000;
    // Queued bytes that wake a sleeping recorder: a hundred or so draws.
    constexpr uint64_t kWakeBytes = 64u << 10;

    // Each side's own fields on lines of their own, so neither side's writes
    // take the other's cache line away from it on every call.
    struct Queue
    {
        // The producer's.
        uint8_t* ring = nullptr;
        uint64_t reserved = 0;      // queued, not yet published
        uint64_t madeSeen = 0;      // `made` as last read, which only grows
        uint64_t wokenFrom = ~0ull; // the sleep (its sleptAt) already woken for gathered calls
        uint64_t roomWaits = 0, waits = 0;
        alignas(64) std::atomic<uint64_t> written{ 0 };
        // Calls up to here are due now: a submission among them, or a thread
        // waiting for them. Only moves forward.
        std::atomic<uint64_t> urgent{ 0 };
        std::atomic<uint32_t> wakes{ 0 };           // what a sleeping recorder waits on
        std::atomic<bool> recorderAsleep{ false };
        std::atomic<uint64_t> sleptAt{ 0 };         // `made` when it went to sleep
        alignas(64) std::atomic<uint64_t> made{ 0 };
        std::atomic<uint32_t> waiters{ 0 };         // threads waiting on `made`
        // The recorder's.
        alignas(64) bool stopping = false;
        std::atomic<uint64_t> calls{ 0 }, sleeps{ 0 };
        std::thread thread;
    };
    Queue q;

    void Run()
    {
        crash::RegisterThread("recorder");
        uint8_t* const ring = q.ring;
        uint64_t at = q.made.load(std::memory_order_relaxed);
        uint64_t calls = 0;
        // Whether what is queued past `at` has to be made now.
        const auto due = [&](uint64_t end) {
            return end - at >= kWakeBytes || q.urgent.load(std::memory_order_seq_cst) > at;
        };
        while (!q.stopping)
        {
            const uint64_t end = q.written.load(std::memory_order_acquire);
            if (!due(end))
            {
                q.calls.store(calls, std::memory_order_relaxed);
                // Asleep before looking once more; the producer publishes before
                // it looks at this, so one of the two sees the other. It waits
                // on `wakes`, not on `written`: a wait on `written` spins first,
                // and returned at the next call queued.
                const uint32_t seen = q.wakes.load(std::memory_order_seq_cst);
                q.sleptAt.store(at, std::memory_order_relaxed);
                q.recorderAsleep.store(true, std::memory_order_seq_cst);
                if (!due(q.written.load(std::memory_order_seq_cst)))
                {
                    q.sleeps.fetch_add(1, std::memory_order_relaxed);
                    q.wakes.wait(seen, std::memory_order_seq_cst);
                }
                q.recorderAsleep.store(false, std::memory_order_relaxed);
                continue;
            }
            while (at != end)
            {
                Header* header = reinterpret_cast<Header*>(ring + (at & (kBytes - 1)));
                if (header->run)
                {
                    if (!header->joins && vk::record::detail::settle) vk::record::detail::settle();
                    header->run(header + 1, header->command);
                }
                at += header->bytes;
                calls++;
                q.made.store(at, std::memory_order_seq_cst);
                if (q.waiters.load(std::memory_order_seq_cst)) q.made.notify_all();
            }
        }
        q.calls.store(calls, std::memory_order_relaxed);
    }

    void RaiseUrgent(uint64_t to)
    {
        uint64_t was = q.urgent.load(std::memory_order_relaxed);
        while (was < to && !q.urgent.compare_exchange_weak(was, to, std::memory_order_seq_cst)) {}
    }

    void Ring()
    {
        q.wakes.fetch_add(1, std::memory_order_seq_cst);
        q.wakes.notify_one();
    }

    // Everything queued so far is due now, and a sleeping recorder is woken
    // for it. Any thread.
    void Wake()
    {
        RaiseUrgent(q.written.load(std::memory_order_seq_cst));
        if (q.recorderAsleep.load(std::memory_order_seq_cst)) Ring();
    }

    void WaitMade(uint64_t position)
    {
        if (q.made.load(std::memory_order_acquire) >= position) return;
        Wake();
        for (uint32_t i = 0; i < kSpins; i++)
        {
            _mm_pause();
            if (q.made.load(std::memory_order_acquire) >= position) return;
        }
        q.waiters.fetch_add(1, std::memory_order_seq_cst);
        for (;;)
        {
            const uint64_t now = q.made.load(std::memory_order_seq_cst);
            if (now >= position) break;
            q.made.wait(now, std::memory_order_seq_cst);
        }
        q.waiters.fetch_sub(1, std::memory_order_relaxed);
    }
}

void* vk::record::detail::Reserve(uint32_t bytes)
{
    uint64_t at = q.reserved;
    uint64_t offset = at & (kBytes - 1);
    const uint64_t pad = offset + bytes > kBytes ? kBytes - offset : 0;
    const uint64_t end = at + pad + bytes;
    if (end - q.madeSeen > kBytes)
    {
        q.madeSeen = q.made.load(std::memory_order_acquire);
        if (end - q.madeSeen > kBytes)
        {
            q.roomWaits++;
            report::Timed spent(report::kRecorderWait);
            WaitMade(end - kBytes);
            q.madeSeen = q.made.load(std::memory_order_acquire);
        }
    }
    if (pad)
    {
        new (q.ring + offset) Header{ nullptr, VK_NULL_HANDLE, uint32_t(pad), 0 };
        at += pad;
        offset = 0;
    }
    q.reserved = at + bytes;
    return q.ring + offset;
}

void vk::record::detail::Publish(bool now)
{
    q.written.store(q.reserved, std::memory_order_seq_cst);
    if (now) RaiseUrgent(q.reserved);
    if (!q.recorderAsleep.load(std::memory_order_seq_cst)) return;
    const uint64_t slept = q.sleptAt.load(std::memory_order_relaxed);
    if (now || (q.reserved - slept >= kWakeBytes && q.wokenFrom != slept))
    {
        // Until it clears its flag the recorder still looks asleep: one wake
        // a sleep is enough.
        q.wokenFrom = slept;
        Ring();
    }
}

void vk::record::Flush()
{
    if (detail::threaded) Wake();
}

void vk::record::Start(void (*settle)())
{
    detail::settle = settle;
    if (detail::threaded) return;
    static const bool off =
        diag::kOn && diag::Text("MW2_RECORD_THREAD") && !diag::Flag("MW2_RECORD_THREAD");
    if (off)
    {
        LOGI("recorder: MW2_RECORD_THREAD=0, the consumer records its own commands");
        return;
    }
    q.ring = static_cast<uint8_t*>(::operator new(kBytes, std::align_val_t(64)));
    q.reserved = q.madeSeen = 0;
    q.written.store(0);
    q.made.store(0);
    q.urgent.store(0);
    q.sleptAt.store(0);
    q.wokenFrom = ~0ull;
    q.stopping = false;
    detail::threaded = true;
    q.thread = std::thread(Run);
}

void vk::record::Stop()
{
    if (!detail::threaded) return;
    Host([] { q.stopping = true; });
    q.thread.join();
    detail::threaded = false;
    ::operator delete(q.ring, std::align_val_t(64));
    q.ring = nullptr;
}

bool vk::record::Threaded() { return detail::threaded; }

void vk::record::Into(VkCommandBuffer command) { detail::into = command; }

uint64_t vk::record::Position()
{
    return detail::threaded ? q.written.load(std::memory_order_acquire) : 0;
}

bool vk::record::Done(uint64_t position)
{
    return !detail::threaded || q.made.load(std::memory_order_acquire) >= position;
}

void vk::record::WaitFor(uint64_t position)
{
    if (Done(position)) return;
    q.waits++;
    report::Timed spent(report::kRecorderWait);
    WaitMade(position);
}

void vk::record::Drain() { WaitFor(Position()); }

void vk::record::Report()
{
    const uint64_t calls = q.calls.load();
    if (!calls) return;
    LOGI("recorder: %llu calls made on its own thread; it slept %llu times; the consumer"
         " waited for it %llu times, %llu of them for room in the queue",
         (unsigned long long)calls, (unsigned long long)q.sleeps.load(),
         (unsigned long long)(q.waits + q.roomWaits), (unsigned long long)q.roomWaits);
}
