// tools/gpu_diff.py --rig recorder, recorder_inline, recorder_sleeps, recorder_bench and
// device_trace: the C++ recorder (runtime/gpu/vulkan/recorder.cpp, included whole for its
// queue) and the Rust one (src/runtime/gpu/vulkan/recorder.rs) on the same script, and the
// C++ command trace (trace.cpp, through trace_cpp.cpp) against the Rust Vk's lines
// (device.rs, trace.rs) on the same calls, both on the stand-in Vulkan of device_stubs.cpp.
//
//   recorder <seed> <ops> recorder   a script of <ops> (generated from the seed): calls of
//       16 sizes from 32 B to 64 KB, so the ring wraps behind pad headers, as command,
//       joining and host; a new command buffer now and then; done, wait_for and drain on
//       calls a little way back; spins that let the recorder sleep; host calls polled with
//       done and no wake, two seconds at most; bursts of 64 KB calls that fill the ring, so
//       the producer waits for room; a flush from a third thread and stray tokens
//       for the recorder and the waiter (C++: futex wakes that change nothing; Rust: a
//       foreign unpark) at random times; a counting settle. Each side checks, by itself,
//       that every call is made in queue order, into the command buffer named when it was
//       queued, behind a settle, bytes intact; that each is dropped once; that done never
//       says a call was made before it was, and wait_for and drain never return before it
//       is; that no wake is lost; a call queued, then flush and a poll, two seconds at
//       most; a tail of small calls the recorder lets gather, then, on seeds 2 and 3 of
//       every four, stop with them still queued (as the renderer's shutdown does): stop
//       must make them all, each dropped once. The two must agree on all that, on their report line's
//       format, on their call counts (but for pad headers: a C++ closure of no bytes takes
//       one), and their sleeps, waits, room waits and wakes must be within twice each
//       other's.
//   ... inline       the same with MW2_RECORD_THREAD=0: every call made where it is queued.
//   ... sleeps       a million calls that each wake a sleeping recorder: 64 KB commands, each
//       due by its size, alone or two with a pause between them about as long as the
//       recorder takes to make the first and go to sleep, each polled with done and no
//       wake (no host calls: see Script); and calls the recorder takes longer over than the producer spins,
//       waited for. No flush and no stray token: a wake lost between the producer's store of
//       `written` and the recorder's of its sleep is a call never made, and the poll times
//       out; a waiter the recorder does not wake is a hang the watchdog ends.
//   ... bench        the benchmark: rounds of 60,000 calls of 224 B (a draw) and 64 B (a
//       barrier) closures queued behind a gate the recorder waits in, C++ and Rust in
//       turns; the producer's ns and instructions per call, then the recorder's from the
//       gate's opening to the round's last call. Both recorders run on one core and the
//       producer on another, so neither side gains by where the scheduler put its thread,
//       32 MB are written before every round, so none finds the last one's ring in the
//       cache (a 224 B round queues 15 MB, near the whole L3), and both recorders are made
//       again every ten rounds, their rings on new pages;
//       C++ and Rust strictly in turns inside blocks of ten pairs (a round after one of
//       its own side's runs warm), C++ beginning every other block and Rust the others (the
//       side that goes first gains a few per cent), the two orders weighed the same; the
//       verdict takes the worse of the ratio of the medians and the paired ratio (the
//       median of each pair's Rust/C++ ratio), and over 1.03
//       on either the producer or the recorder is a failure: exit 1.
//   ... trace        <ops> cases, each a trace started as the renderer's initialise starts
//       it (a window of frames or none, the forced switches, foreign handles), then a few
//       hundred of the calls trace.h wraps, on handles made, foreign, unknown or null,
//       structures with the pNext chains the trace decodes, results that fail, names
//       given, notes, frames, presents, a submission, a state library, the recorder's
//       start, positions and stop with lines made on the recorder's thread and on the
//       consumer's, and the untraced calls; the trace files, the printed lines, the stand-
//       ins' logs and every result must be the same.
#include "gpu/vulkan/recorder.cpp"
#include "rig_log.h"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <fstream>
#include <memory>
#include <random>
#include <sstream>
#include <string>
#include <thread>
#include <vector>

#include <sched.h>
#include <unistd.h>

extern "C"
{
    struct RigOp
    {
        uint32_t kind, a;
        uint64_t b;
    };
    void mw2_rig_rec_run(const RigOp* script, uint64_t count, bool settle, bool drainFirst, uint8_t* dropped,
                         uint8_t* zst, const std::atomic<uint64_t>* progress, uint64_t* out);
    void mw2_rig_rec_counters_reset();
    void mw2_rig_rec_report();
    void mw2_rig_rec_stray();
    void mw2_rig_rec_flush();
    void mw2_rig_rec_bench(uint32_t bytes, uint64_t calls, double* out);
    void mw2_rig_rec_bench_start();
    void mw2_rig_rec_bench_stop();
    struct RigCall
    {
        uint32_t op, onRecorder;
        uint64_t a[12];
    };
    void mw2_rig_vk_begin(uint64_t device, uint64_t physical, uint64_t queue, uint64_t cache, uint64_t layout,
                          const uint64_t* setLayouts, uint32_t count, uint32_t multiDraw);
    void mw2_rig_vk_forced(uint64_t* out);
    uint64_t mw2_rig_vk_fnv(const uint8_t* bytes, size_t size, bool writeCombined);
    uint64_t mw2_rig_vk_call(const RigCall* call, uint64_t* out);
    uint64_t rig_position();
    uint64_t rig_rec_now();
    int rig_counter_open();
    uint64_t rig_counter_read(int fd);
    void rig_counter_close(int fd);
}
void rig_cpp_trace_reset();
namespace rig_vk
{
    extern std::vector<uint64_t> g_calls;
    extern bool g_logging;
    extern uint64_t g_next;
    extern VkResult g_result;
    extern uint64_t g_position;
    PFN_vkVoidFunction Command(const char* name);
    void Withhold(bool multi, bool dynamic);
}

namespace
{
    std::atomic<uint64_t> g_progress{ 0 };
    std::atomic<bool> g_helpers{ false };
    std::atomic<int> g_side{ -1 };

    // ---- the script --------------------------------------------------------------------

    enum : uint32_t { kCommand, kJoining, kHost, kInto, kDone, kWaitFor, kDrain, kSpin, kHostPoll, kBigPoll, kFlush, kStall, kRace, kStallWait, kFlushPoll, kRestart };
    // Words of payload for each size index (index 0: a closure of no bytes).
    constexpr int kWords[16] = { -1, 0, 4, 8, 12, 20, 24, 28, 60, 124, 252, 508, 1020, 2044, 4092, 8184 };
    enum : uint64_t
    {
        kEOrder = 1, kEIntoWrong = 2, kESettle = 4, kEBytes = 8, kEDoneEarly = 16, kEWaitEarly = 32, kEDrainEarly = 64,
        kELostWake = 128, kEDrops = 256, kEStopEarly = 512,
    };

    uint32_t SizeIndex(std::mt19937_64& rng)
    {
        const uint32_t r = rng() % 100;
        if (r < 5) return 0;
        if (r < 60) return 1 + rng() % 7;
        if (r < 92) return 8 + rng() % 4;
        if (r < 99) return 12 + rng() % 2;
        return 14 + rng() % 2;
    }

    std::vector<RigOp> Script(uint64_t seed, uint64_t count, bool sleeps)
    {
        std::mt19937_64 rng(seed * 0x9E3779B97F4A7C15ull + 1);
        std::vector<RigOp> ops;
        ops.reserve(count);
        if (sleeps)
        {
            for (uint64_t i = 0; i < count; i++)
            {
                const uint32_t r = rng() % 100;
                // No host call: a host call made and taken before the recorder's next sleep
                // at the same place spends that sleep's one wake for gathered calls (C++ and
                // Rust alike), and 64 KB queued after it then waits for a flush, a wait or the
                // next host call -- latency the poll would count as a lost wake.
                if (r < 50) ops.push_back({ kBigPoll, 15, 0 });
                else if (r < 95) ops.push_back({ kRace, uint32_t(rng() % 3000), 0 });
                else ops.push_back({ kStallWait, 8000, 0 });
            }
            return ops;
        }
        for (uint64_t i = 0; i < count; i++)
        {
            const uint32_t r = rng() % 1000;
            if (r < 600) ops.push_back({ kCommand, SizeIndex(rng), 0 });
            else if (r < 700) ops.push_back({ kJoining, SizeIndex(rng), 0 });
            else if (r < 730) ops.push_back({ kHost, SizeIndex(rng), 0 });
            else if (r < 750) ops.push_back({ kInto, 0, 0x100000 + (rng() % 64) * 0x40 });
            else if (r < 800) ops.push_back({ kDone, rng() % 2 ? 0 : uint32_t(rng() % 64), 0 });
            else if (r < 810) ops.push_back({ kWaitFor, rng() % 2 ? 0 : uint32_t(rng() % 64), 0 });
            else if (r < 812) ops.push_back({ kDrain, 0, 0 });
            else if (r < 830) ops.push_back({ kSpin, uint32_t(rng() % 20000), 0 });
            else if (r < 835) ops.push_back({ kHostPoll, SizeIndex(rng), 0 });
            else if (r < 838) ops.push_back({ kFlush, 0, 0 });
            // A small call the recorder would let gather, then flush, polled: only the flush
            // wakes it.
            else if (r < 840) ops.push_back({ kFlushPoll, 1 + uint32_t(rng() % 7), 0 });
            else if (r < 846 && r >= 841)
            {
                // A call the recorder takes a while over, waited for at once: done and wait_for
                // must not return before it is made.
                ops.push_back({ kStall, 20000, 0 });
                ops.push_back({ rng() % 2 ? kWaitFor : kDone, 0, 0 });
                i++;
            }
            else if (r < 841)
            {
                // A call the recorder takes a few milliseconds over, then a burst of 64 KB
                // calls with no wait between them: more than the ring holds, so the producer
                // waits for room.
                ops.push_back({ kStall, 200000 + uint32_t(rng() % 400000), 0 });
                for (uint32_t k = 0; k < 300 && i < count; k++, i++) ops.push_back({ kCommand, 15, 0 });
            }
            // A start while threaded: settle off (a = 0) or the run's again (a = 1), after a
            // drain (a call queued before it but not yet made may run with either settle).
            else if (r < 850) ops.push_back({ kRestart, uint32_t(rng() % 2), 0 });
            else ops.push_back({ kCommand, SizeIndex(rng), 0 });
        }
        // The tail: under 64 KB of small calls, which the recorder lets gather, for the
        // stop that comes with them still queued.
        for (int k = 0; k < 100; k++) ops.push_back({ k % 10 ? kCommand : kJoining, 1 + uint32_t(rng() % 7), 0 });
        return ops;
    }

    // ---- the C++ side's calls, as the Rust's (recorder.rs, mod rig) --------------------

    std::atomic<uint64_t> g_ran{ 0 }, g_errors{ 0 }, g_firstError{ ~0ull };
    uint64_t g_settles = 0;   // the recorder's (or, inline, the producer's)
    uint8_t* g_dropped = nullptr;
    uint8_t* g_zst = nullptr;
    bool g_settleOn = false;

    void Fail(uint64_t error, uint64_t at)
    {
        g_errors.fetch_or(error, std::memory_order_relaxed);
        uint64_t none = ~0ull;
        g_firstError.compare_exchange_strong(none, at, std::memory_order_relaxed);
    }

    void Settle() { g_settles++; }

    struct Head
    {
        uint64_t id, into, nonjoin, check;
    };
    template <int N> struct Payload : Head
    {
        uint64_t words[N];
    };
    template <> struct Payload<0> : Head
    {
    };
    constexpr uint64_t kMoved = 1ull << 63;

    template <int N> struct Call
    {
        Payload<N> p;
        bool joins;   // not part of the closure's bytes: see Size
        Call(uint64_t id, uint64_t into, uint64_t nonjoin)
        {
            p.id = id;
            p.into = into;
            p.nonjoin = nonjoin;
            p.check = (id * 0x9E3779B97F4A7C15ull) ^ into;
            if constexpr (N > 0)
            {
                std::memset(p.words, 0, sizeof p.words);
                p.words[0] = id ^ 0x5555;
                p.words[N - 1] = id ^ 0xAAAA;
            }
        }
    };

    // The closure, the payload's bytes and nothing else, as Rust's is.
    template <int N, bool Joins> struct Made
    {
        Payload<N> p;
        explicit Made(const Payload<N>& from) : p(from) {}
        Made(Made&& from) noexcept : p(from.p) { from.p.id |= kMoved; }
        Made(const Made& from) : p(from.p) {}
        ~Made()
        {
            if (!(p.id & kMoved)) g_dropped[p.id]++;
        }
        void Check(bool hasInto, uint64_t into) const
        {
            const uint64_t ran = g_ran.load(std::memory_order_relaxed);
            if (ran != p.id) Fail(kEOrder, p.id);
            if (hasInto && into != p.into) Fail(kEIntoWrong, p.id);
            if (g_settleOn && !Joins && g_settles != p.nonjoin) Fail(kESettle, p.id);
            bool bytes = p.check == ((p.id * 0x9E3779B97F4A7C15ull) ^ p.into);
            if constexpr (N > 0) bytes = bytes && p.words[0] == (p.id ^ 0x5555) && p.words[N - 1] == (p.id ^ 0xAAAA);
            if (!bytes) Fail(kEBytes, p.id);
            g_ran.store(ran + 1, std::memory_order_release);
        }
        void operator()(VkCommandBuffer command) const { Check(true, vk::trace::Raw(command)); }
        void operator()() const { Check(false, 0); }
    };
    static_assert(sizeof(Made<0, false>) == 32 && sizeof(Made<24, false>) == 224 && sizeof(Made<4, false>) == 64);

    void ZstMade()
    {
        const uint64_t ran = g_ran.load(std::memory_order_relaxed);
        if (!g_zst[ran]) Fail(kEOrder, ran);
        g_ran.store(ran + 1, std::memory_order_release);
    }

    template <int N> void QueueN(uint32_t kind, uint64_t id, uint64_t into, uint64_t nonjoin)
    {
        Call<N> c(id, into, nonjoin);
        if (kind == kCommand) vk::record::Command(Made<N, false>(c.p));
        else if (kind == kJoining) vk::record::Joining(Made<N, true>(c.p));
        else vk::record::Host(Made<N, false>(c.p));
    }

    void Queue(uint32_t size, uint32_t kind, uint64_t id, uint64_t into, uint64_t nonjoin)
    {
        switch (size)
        {
        case 1: return QueueN<0>(kind, id, into, nonjoin);
        case 2: return QueueN<4>(kind, id, into, nonjoin);
        case 3: return QueueN<8>(kind, id, into, nonjoin);
        case 4: return QueueN<12>(kind, id, into, nonjoin);
        case 5: return QueueN<20>(kind, id, into, nonjoin);
        case 6: return QueueN<24>(kind, id, into, nonjoin);
        case 7: return QueueN<28>(kind, id, into, nonjoin);
        case 8: return QueueN<60>(kind, id, into, nonjoin);
        case 9: return QueueN<124>(kind, id, into, nonjoin);
        case 10: return QueueN<252>(kind, id, into, nonjoin);
        case 11: return QueueN<508>(kind, id, into, nonjoin);
        case 12: return QueueN<1020>(kind, id, into, nonjoin);
        case 13: return QueueN<2044>(kind, id, into, nonjoin);
        case 14: return QueueN<4092>(kind, id, into, nonjoin);
        case 15: return QueueN<8184>(kind, id, into, nonjoin);
        default:
            if (kind == kCommand) vk::record::Command([](VkCommandBuffer) { ZstMade(); });
            else if (kind == kJoining) vk::record::Joining([](VkCommandBuffer) { ZstMade(); });
            else vk::record::Host([] { ZstMade(); });
        }
    }

    void Poll(uint64_t position, uint64_t at)
    {
        const auto began = std::chrono::steady_clock::now();
        uint32_t n = 0;
        while (!vk::record::Done(position))
        {
            _mm_pause();
            if (++n % 1024 == 0 &&
                std::chrono::duration<double>(std::chrono::steady_clock::now() - began).count() > 2.0)
            {
                // Every poll after it would wait its two seconds too: it ends the run.
                std::printf("DIFFERENCE: the C++ recorder lost a wake: call %llu not made in 2 s (written %llu made %llu"
                            " urgent %llu asleep %d slept at %llu woken from %llu wakes %u)\n",
                            (unsigned long long)at, (unsigned long long)q.written.load(),
                            (unsigned long long)q.made.load(), (unsigned long long)q.urgent.load(),
                            int(q.recorderAsleep.load()), (unsigned long long)q.sleptAt.load(),
                            (unsigned long long)q.wokenFrom, q.wakes.load());
                std::fflush(stdout);
                _exit(4);
            }
        }
    }

    void CppRun(const std::vector<RigOp>& script, bool settle, bool drainFirst, uint8_t* dropped, uint8_t* zst,
                uint64_t* out)
    {
        g_ran = 0;
        g_settles = 0;
        g_errors = 0;
        g_firstError = ~0ull;
        g_dropped = dropped;
        g_zst = zst;
        g_settleOn = settle;
        vk::record::Start(settle ? Settle : nullptr);
        // Non-joining calls queued while a settle is on: settle runs before each of them only.
        uint64_t queued = 0, nonjoin = 0, intoNow = 0, settleNow = settle;
        vk::record::Into(VK_NULL_HANDLE);
        std::vector<uint64_t> positions(1024);
        for (const RigOp& op : script)
        {
            g_progress.fetch_add(1, std::memory_order_relaxed);
            switch (op.kind)
            {
            case kCommand:
            case kJoining:
            case kHost:
                if (op.kind != kJoining) nonjoin += settleNow;
                Queue(op.a, op.kind, queued, intoNow, nonjoin);
                positions[queued & 1023] = vk::record::Position();
                queued++;
                break;
            case kInto:
                intoNow = op.b;
                vk::record::Into(reinterpret_cast<VkCommandBuffer>(uintptr_t(op.b)));
                break;
            case kDone:
            case kWaitFor:
                if (op.a < queued && op.a < 1024)
                {
                    const uint64_t call = queued - 1 - op.a;
                    const uint64_t at = positions[call & 1023];
                    if (op.kind == kDone)
                    {
                        if (vk::record::Done(at) && g_ran.load(std::memory_order_acquire) <= call) Fail(kEDoneEarly, call);
                    }
                    else
                    {
                        vk::record::WaitFor(at);
                        if (g_ran.load(std::memory_order_acquire) <= call) Fail(kEWaitEarly, call);
                    }
                }
                break;
            case kDrain:
                vk::record::Drain();
                if (g_ran.load(std::memory_order_acquire) != queued) Fail(kEDrainEarly, queued);
                break;
            case kSpin:
                for (uint32_t i = 0; i < op.a; i++) _mm_pause();
                break;
            case kHostPoll:
            case kBigPoll:
            {
                nonjoin += settleNow;
                Queue(op.a, op.kind == kHostPoll ? kHost : kCommand, queued, intoNow, nonjoin);
                const uint64_t at = vk::record::Position();
                positions[queued & 1023] = at;
                queued++;
                Poll(at, queued - 1);
                break;
            }
            case kFlush: vk::record::Flush(); break;
            case kRestart:
                settleNow = settle && op.a;
                vk::record::Drain();
                vk::record::Start(settleNow ? Settle : nullptr);
                break;
            case kFlushPoll:
            {
                nonjoin += settleNow;
                Queue(op.a, kCommand, queued, intoNow, nonjoin);
                const uint64_t at = vk::record::Position();
                positions[queued & 1023] = at;
                queued++;
                vk::record::Flush();
                Poll(at, queued - 1);
                break;
            }
            case kRace:
            {
                // A 64 KB call, a pause about as long as the recorder takes to wake for it,
                // make it and go to sleep, then another, polled: its publication meets the
                // recorder's going to sleep now and then.
                nonjoin += settleNow;
                Queue(15, kCommand, queued, intoNow, nonjoin);
                positions[queued & 1023] = vk::record::Position();
                queued++;
                for (uint32_t k = 0; k < op.a; k++) _mm_pause();
                nonjoin += settleNow;
                Queue(15, kCommand, queued, intoNow, nonjoin);
                const uint64_t at = vk::record::Position();
                positions[queued & 1023] = at;
                queued++;
                Poll(at, queued - 1);
                break;
            }
            case kStallWait:
            case kStall:
            {
                // A command of 32 bytes and a count of pauses: 40, as the Rust's.
                nonjoin += settleNow;
                Call<0> c(queued, intoNow, nonjoin);
                vk::record::Command([made = Made<0, false>(c.p), spins = op.a](VkCommandBuffer command) {
                    for (uint32_t k = 0; k < spins; k++) _mm_pause();
                    made(command);
                });
                positions[queued & 1023] = vk::record::Position();
                queued++;
                if (op.kind == kStallWait)
                {
                    // Longer than the producer spins: it parks, and the recorder must wake it.
                    vk::record::WaitFor(vk::record::Position());
                    if (g_ran.load(std::memory_order_acquire) != queued) Fail(kEWaitEarly, queued - 1);
                }
                break;
            }
            }
        }
        if (drainFirst)
        {
            vk::record::Drain();
            if (g_ran.load(std::memory_order_acquire) != queued) Fail(kEDrainEarly, queued);
        }
        vk::record::Stop();
        if (g_ran.load(std::memory_order_acquire) != queued) Fail(kEStopEarly, queued);
        for (uint64_t id = 0; id < queued; id++)
            if (!zst[id] && dropped[id] != 1) { Fail(kEDrops, id); break; }
        out[0] = g_errors;
        out[1] = g_firstError;
        out[2] = q.calls.load();
        out[3] = q.sleeps.load();
        out[4] = q.waits;
        out[5] = q.roomWaits;
        out[6] = q.wakes.load();
    }

    // The flush from a third thread and the stray tokens, while a side runs.
    void Helpers(uint64_t seed)
    {
        std::mt19937_64 rng(seed ^ 0xC0FFEE);
        auto next = std::chrono::steady_clock::now();
        auto nextFlush = next;
        while (g_helpers.load())
        {
            const auto now = std::chrono::steady_clock::now();
            const int side = g_side.load();
            if (now >= next)
            {
                if (side == 0) { q.wakes.notify_all(); q.made.notify_all(); }
                else if (side == 1) mw2_rig_rec_stray();
                next = now + std::chrono::microseconds(5 + rng() % 80);
            }
            if (now >= nextFlush)
            {
                if (side == 0) vk::record::Flush();
                else if (side == 1) mw2_rig_rec_flush();
                nextFlush = now + std::chrono::microseconds(20 + rng() % 200);
            }
            std::this_thread::yield();
        }
    }

    void Watchdog()
    {
        uint64_t last = ~0ull;
        int still = 0;
        for (;;)
        {
            std::this_thread::sleep_for(std::chrono::milliseconds(500));
            const uint64_t now = g_progress.load();
            if (now != last) { last = now; still = 0; continue; }
            if (g_side.load() >= 0 && ++still >= 60)
            {
                std::printf("DIFFERENCE: STUCK: the %s side made no progress for 30 s at op %llu (C++ written %llu"
                            " made %llu asleep %d)\n",
                            g_side.load() ? "Rust" : "C++", (unsigned long long)now,
                            (unsigned long long)q.written.load(), (unsigned long long)q.made.load(),
                            int(q.recorderAsleep.load()));
                std::fflush(stdout);
                _exit(3);
            }
        }
    }

    const char* kErrorNames[] = { "order", "into", "settle", "bytes", "done early", "wait early", "drain early",
                                  "lost wake", "drops", "stop early" };

    std::string Errors(uint64_t errors)
    {
        std::string text;
        for (int b = 0; b < 10; b++)
            if (errors & (1ull << b)) text += std::string(text.empty() ? "" : ",") + kErrorNames[b];
        return text.empty() ? "none" : text;
    }

    bool Within(uint64_t a, uint64_t b) { return a <= 2 * b + 16 && b <= 2 * a + 16; }

    int RecorderPart(uint64_t seed, uint64_t count, bool sleeps)
    {
        const std::vector<RigOp> script = Script(seed, sleeps ? 1000000 : count, sleeps);
        uint64_t calls = 0, bytes = 0;
        std::vector<uint8_t> zst;
        zst.reserve(script.size());
        for (const RigOp& op : script)
            if (op.kind == kRace)
                for (int k = 0; k < 2; k++)
                {
                    zst.push_back(0);
                    calls++;
                    bytes += 32 + 32 + 8 * kWords[15];
                }
            else if (op.kind == kCommand || op.kind == kJoining || op.kind == kHost || op.kind == kHostPoll ||
                     op.kind == kBigPoll || op.kind == kStall || op.kind == kStallWait || op.kind == kFlushPoll)
            {
                const bool stall = op.kind == kStall || op.kind == kStallWait;
                const uint32_t size = stall ? 1 : op.a;
                zst.push_back(size == 0);
                calls++;
                bytes += 32 + (kWords[size] < 0 ? 0 : 32 + 8 * kWords[size]) + (stall ? 32 : 0);
            }
        // Pad headers: one a lap of the ring at most; C++ makes a closure of no bytes 64.
        uint64_t zsts = 0;
        for (uint8_t z : zst) zsts += z;
        const uint64_t laps = (bytes + 32 * zsts) / ((16u << 20) - (64u << 10)) + 2;
        const bool settle = seed % 2 == 1;
        // Stop with the tail still queued on seeds 2 and 3 of every four (the sleeps part has
        // no tail: it drains).
        const bool drainFirst = sleeps || seed % 4 < 2;
        uint64_t out[2][7]{};
        std::string logs[2];
        for (int side = 0; side < 2; side++)
        {
            std::vector<uint8_t> dropped(calls + 1, 0);
            g_progress = 0;
            g_side = side;
            // Not in the sleeps part: a flush or a stray token would wake a recorder whose wake
            // was lost, or a waiter the recorder forgot.
            g_helpers = true;
            std::thread helpers;
            if (!sleeps) helpers = std::thread(Helpers, seed + side);
            rig_log::Take(side);
            if (side == 0)
            {
                q.calls = 0;
                q.sleeps = 0;
                q.waits = q.roomWaits = 0;
                q.wakes = 0;
                CppRun(script, settle, drainFirst, dropped.data(), zst.data(), out[0]);
                vk::record::Report();
            }
            else
            {
                mw2_rig_rec_counters_reset();
                mw2_rig_rec_run(script.data(), script.size(), settle, drainFirst, dropped.data(), zst.data(), &g_progress,
                                out[1]);
                mw2_rig_rec_report();
            }
            rig_log::Stop();
            g_helpers = false;
            if (helpers.joinable()) helpers.join();
            g_side = -1;
        }
        logs[0] = rig_log::g_log[0];
        logs[1] = rig_log::g_log[1];
        rig_log::Clear();
        bool bad = false;
        for (int side = 0; side < 2; side++)
            if (out[side][0])
            {
                std::printf("DIFFERENCE: the %s recorder: %s at call %llu\n", side ? "Rust" : "C++",
                            Errors(out[side][0]).c_str(), (unsigned long long)out[side][1]);
                bad = true;
            }
        const uint64_t* c = out[0];
        const uint64_t* r = out[1];
        const uint64_t madeCalls = calls + 1;   // and the stop's host call
        const bool threaded = !std::getenv("MW2_RECORD_THREAD");
        for (int side = 0; side < 2; side++)
            if (threaded ? out[side][2] < madeCalls || out[side][2] > madeCalls + laps : out[side][2] != 0)
            {
                std::printf("DIFFERENCE: the %s recorder made %llu calls of %llu queued\n", side ? "Rust" : "C++",
                            (unsigned long long)out[side][2], (unsigned long long)madeCalls);
                bad = true;
            }
        const char* names[] = { "sleeps", "waits", "room waits", "wakes" };
        for (int k = 0; k < 4; k++)
            if (!Within(c[3 + k], r[3 + k]))
            {
                std::printf("DIFFERENCE: %s: C++ %llu, Rust %llu, not within twice\n", names[k],
                            (unsigned long long)c[3 + k], (unsigned long long)r[3 + k]);
                bad = true;
            }
        // The report line: the Rust's numbers through the C++'s format.
        char expected[512];
        if (!r[2]) expected[0] = 0;
        else
            std::snprintf(expected, sizeof expected,
                          "[i] recorder: %llu calls made on its own thread; it slept %llu times; the consumer"
                          " waited for it %llu times, %llu of them for room in the queue\n",
                          (unsigned long long)r[2], (unsigned long long)r[3], (unsigned long long)(r[4] + r[5]),
                          (unsigned long long)r[5]);
        char cppExpected[512];
        if (!c[2]) cppExpected[0] = 0;
        else
            std::snprintf(cppExpected, sizeof cppExpected,
                          "[i] recorder: %llu calls made on its own thread; it slept %llu times; the consumer"
                          " waited for it %llu times, %llu of them for room in the queue\n",
                          (unsigned long long)c[2], (unsigned long long)c[3], (unsigned long long)(c[4] + c[5]),
                          (unsigned long long)c[5]);
        const std::string inlineLine = "[i] recorder: MW2_RECORD_THREAD=0, the consumer records its own commands\n";
        auto strip = [&](const std::string& log, size_t& lines) {
            // The inline switch's line comes from every start, the script's restarts too.
            std::string text = log;
            lines = 0;
            for (size_t at; (at = text.find(inlineLine)) != std::string::npos; lines++) text.erase(at, inlineLine.size());
            return text;
        };
        size_t inlineLines[2];
        const std::string stripped[2] = { strip(logs[0], inlineLines[0]), strip(logs[1], inlineLines[1]) };
        if (inlineLines[0] != inlineLines[1] || (inlineLines[0] != 0) == threaded || stripped[0] != cppExpected ||
            stripped[1] != expected)
        {
            std::printf("DIFFERENCE: the report lines: C++ %s / Rust %s\n", logs[0].c_str(), logs[1].c_str());
            bad = true;
        }
        std::printf("%llu ops, %llu calls, %llu MB queued, settle %s, %s: errors C++ %s Rust %s; calls %llu/%llu, sleeps"
                    " %llu/%llu, waits %llu/%llu, room waits %llu/%llu, wakes %llu/%llu (C++/Rust)\n",
                    (unsigned long long)script.size(), (unsigned long long)calls, (unsigned long long)(bytes >> 20),
                    settle ? "on" : "off", drainFirst ? "drained before stop" : "stopped with calls queued", Errors(c[0]).c_str(), Errors(r[0]).c_str(), (unsigned long long)c[2],
                    (unsigned long long)r[2], (unsigned long long)c[3], (unsigned long long)r[3],
                    (unsigned long long)c[4], (unsigned long long)r[4], (unsigned long long)c[5],
                    (unsigned long long)r[5], (unsigned long long)c[6], (unsigned long long)r[6]);
        return bad ? 1 : 0;
    }

    // ---- the benchmark -----------------------------------------------------------------

    std::atomic<bool> g_gate{ false };
    std::atomic<uint64_t> g_sum{ 0 };
    uint64_t g_madeFrom = 0, g_madeTo = 0, g_instrFrom = 0, g_instrTo = 0;
    int g_counter = -1;

    template <int W> struct Bench
    {
        uint64_t data[W];
    };

    template <int W> __attribute__((noinline)) void CppBenchRound(uint64_t calls, double* out)
    {
        g_gate = false;
        vk::record::Command([](VkCommandBuffer) {
            g_counter = rig_counter_open();
            while (!g_gate.load(std::memory_order_acquire)) _mm_pause();
            g_instrFrom = rig_counter_read(g_counter);
            g_madeFrom = rig_rec_now();
        });
        Bench<W> templ;
        for (int k = 0; k < W; k++) templ.data[k] = 7;
        const int fd = rig_counter_open();
        const uint64_t instructions = rig_counter_read(fd);
        const uint64_t began = rig_rec_now();
        for (uint64_t i = 0; i < calls; i++)
        {
            Bench<W> b = templ;
            b.data[0] = i;
            b.data[W - 1] = i;
            vk::record::Command([b](VkCommandBuffer) {
                g_sum.store(g_sum.load(std::memory_order_relaxed) + (b.data[0] ^ b.data[W - 1]),
                            std::memory_order_relaxed);
            });
        }
        const uint64_t ended = rig_rec_now();
        const uint64_t spent = rig_counter_read(fd) - instructions;
        rig_counter_close(fd);
        vk::record::Command([](VkCommandBuffer) {
            g_instrTo = rig_counter_read(g_counter);
            g_madeTo = rig_rec_now();
            rig_counter_close(g_counter);
        });
        g_gate.store(true, std::memory_order_release);
        // Not Drain: a thread waiting on `made` makes the recorder wake it after every call.
        const uint64_t end = vk::record::Position();
        while (!vk::record::Done(end)) std::this_thread::sleep_for(std::chrono::microseconds(500));
        out[0] = double(ended - began) / double(calls);
        out[1] = double(g_madeTo - g_madeFrom) / double(calls);
        out[2] = double(spent) / double(calls);
        out[3] = double(g_instrTo - g_instrFrom) / double(calls);
    }

    double Median(std::vector<double> v)
    {
        std::sort(v.begin(), v.end());
        return v.empty() ? 0 : v[v.size() / 2];
    }

    bool Pin(int cpu)
    {
        cpu_set_t set;
        CPU_ZERO(&set);
        CPU_SET(cpu, &set);
        return sched_setaffinity(0, sizeof set, &set) == 0;
    }

    int BenchPart(uint64_t rounds)
    {
        // Each order needs pairs of its own (rounds 10-19 are the first Rust-first block):
        // with one order empty its medians are 0 and the verdict would compare NaN.
        if (rounds < 20)
        {
            std::fprintf(stderr, "recorder bench: %llu rounds, at least 20 needed (both orders)\n",
                         (unsigned long long)rounds);
            return 2;
        }
        // The recorder threads inherit the core this thread is on when they start; then the
        // producer moves to its own. Two cores of the eight, neither the other's SMT twin.
        const long cpus = sysconf(_SC_NPROCESSORS_ONLN);
        const int recorderCpu = cpus >= 8 ? 6 : 1, producerCpu = cpus >= 8 ? 4 : 0;
        const bool pinned = cpus >= 2 && Pin(recorderCpu);
        vk::record::Start(nullptr);
        mw2_rig_rec_bench_start();
        if (pinned) Pin(producerCpu);
        // Both recorders made again every block of ten pairs, so each side's ring lands on new
        // physical pages (the L3 is physically indexed: one placement can be a few per cent
        // slower for a whole process), the side that allocates first taking turns.
        uint32_t restarts = 0;
        const auto restart = [&] {
            vk::record::Stop();
            mw2_rig_rec_bench_stop();
            if (pinned) Pin(recorderCpu);
            if (++restarts % 2) { mw2_rig_rec_bench_start(); vk::record::Start(nullptr); }
            else { vk::record::Start(nullptr); mw2_rig_rec_bench_start(); }
            if (pinned) Pin(producerCpu);
        };
        const uint64_t calls = 60000;
        int bad = 0;
        std::vector<uint8_t> evict(32u << 20);
        for (uint32_t bytes : { 224u, 64u })
        {
            // Each side's figures, and each pair's Rust/C++ ratio (producer, recorder), apart
            // by which side began the pair's block.
            std::vector<double> m[2][2][4];
            std::vector<double> ratio[2][2];
            for (uint64_t round = 0; round < rounds; round++)
            {
                // Blocks of ten pairs, the recorders made again between them. Inside a block
                // the sides strictly take turns: a round after one of its own side's is about
                // a fifth faster on the recorder. Even strict turns favour the side that goes
                // first by a few per cent, so C++ begins every other block and Rust the others,
                // and each block's first pair (after the restart) is not counted.
                const uint64_t block = round / 10;
                const int first = int(block % 2);
                if (round % 10 == 0 && round) restart();
                double out[2][4];
                for (int turn = 0; turn < 2; turn++)
                {
                    const int side = turn ^ first;
                    std::memset(evict.data(), int(round * 2 + turn), evict.size());
                    asm volatile("" : : "r"(evict.data()) : "memory");
                    if (side == 0) bytes == 224 ? CppBenchRound<28>(calls, out[0]) : CppBenchRound<8>(calls, out[0]);
                    else mw2_rig_rec_bench(bytes, calls, out[1]);
                }
                if (round % 10 == 0) continue;   // warming up
                for (int side = 0; side < 2; side++)
                    for (int k = 0; k < 4; k++) m[first][side][k].push_back(out[side][k]);
                for (int k = 0; k < 2; k++) ratio[first][k].push_back(out[1][k] / out[0][k]);
            }
            // The two orders' figures weigh the same: geometric means of the two.
            const auto both = [&](const std::vector<double> (&v)[2][2][4], int side, int k) {
                return std::sqrt(Median(v[0][side][k]) * Median(v[1][side][k]));
            };
            const double cp = both(m, 0, 0), rp = both(m, 1, 0), cr = both(m, 0, 1), rr = both(m, 1, 1);
            const double pr = std::sqrt(Median(ratio[0][0]) * Median(ratio[1][0]));
            const double rc = std::sqrt(Median(ratio[0][1]) * Median(ratio[1][1]));
            std::printf("bench %u B closures: producer ns per command C++ %.2f Rust %.2f (%+.1f%%, paired %+.1f%%), instructions"
                        " %.1f / %.1f; recorder ns per call C++ %.2f Rust %.2f (%+.1f%%, paired %+.1f%%), instructions %.1f / %.1f;"
                        " %zu pairs of %llu calls, half of them begun by each side%s\n",
                        bytes, cp, rp, (rp / cp - 1) * 100, (pr - 1) * 100, both(m, 0, 2), both(m, 1, 2), cr, rr,
                        (rr / cr - 1) * 100, (rc - 1) * 100, both(m, 0, 3), both(m, 1, 3),
                        ratio[0][0].size() + ratio[1][0].size(), (unsigned long long)calls, pinned ? ", pinned" : ", not pinned");
            if (std::max(pr, rp / cp) > 1.03 || std::max(rc, rr / cr) > 1.03) bad = 1;
        }
        vk::record::Stop();
        mw2_rig_rec_bench_stop();
        std::printf("%sbench: %s\n", bad ? "DIFFERENCE: " : "", bad ? "Rust slower than C++ + 3%" : "Rust within C++ + 3%");
        return bad;
    }
}

int TracePart(uint64_t seed, uint64_t cases);

int main(int argc, char** argv)
{
    if (argc < 4) { std::fprintf(stderr, "usage: recorder <seed> <ops> <part>\n"); return 2; }
    const uint64_t seed = std::strtoull(argv[1], nullptr, 10), count = std::strtoull(argv[2], nullptr, 10);
    const std::string part = argv[3];
    if (part == "inline") setenv("MW2_RECORD_THREAD", "0", 1);
    else unsetenv("MW2_RECORD_THREAD");
    std::thread(Watchdog).detach();
    if (part == "recorder" || part == "inline") return RecorderPart(seed, count, false);
    if (part == "sleeps") return RecorderPart(seed, count, true);
    if (part == "bench") return BenchPart(count);
    if (part == "trace") return TracePart(seed, count);
    std::fprintf(stderr, "no part %s\n", part.c_str());
    return 2;
}
