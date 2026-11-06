// tools/gpu_diff/interrupts.cpp: the graphics interrupts, C++ (runtime/gpu/gpu.cpp as it
// is, gpu_cpp.cpp) against the Rust (src/runtime/gpu/gpu.rs), each on a thread made as
// its consumer is. The title's handler is a stub guest function here, at a guest address
// of the function table, which records what it was called with -- r3, r4, r13, the
// processor number at KPCR + 268, its stack pointer's place in the 64 KB guest stack and
// the float mode -- and then uses 4 MB of the host stack, which must not fault on that
// thread. After each raise the processor byte's restore is recorded too. Cases cover CPU
// masks of every bit, the vblank, no callback, a callback with no function, no PCR, and
// both raising threads at once, which must take turns. The lines each side printed are
// compared too: the callback set, and the warning of no callback, once a process.
//
// The vblank and worker parts run VBlankWorker and Worker themselves, each side's on a
// thread made as its consumer is, on a clock of the rig's: a sleep moves it on by what
// was asked and a lateness drawn for it (a little, a millisecond or two, exactly four
// blank periods and one nanosecond more, a stall of tens of milliseconds), a batch by
// its cost (either side of the 200 ms a slow one is told at). Everything the loops call
// is recorded with the clock's time -- the sleeps, the time stamp bundle, the blank
// period asked for (0, the display's 60 Hz, others, a nanosecond), the handler's call
// with the display status, the occlusion delivery with what the read pointer's
// write-back holds, the capture's batch brackets, the command processor's batches and
// the renderer's idle -- and at the occlusion delivery the case sets a ring, a
// write-back or a write pointer, as the title's threads do; after a drawn number of
// events the loop is stopped. Then the events, the lines printed, the blanks counted
// and the consumer's busy time are compared.
//
//   interrupts <seed> <cases> [interrupts|vblank|worker]...
//
// Exits non-zero at the first difference, printing the case.
#include <ppc_recomp_shared.h>
#include "rig_log.h"

#include <atomic>
#include <functional>
#include <map>
#include <set>
#include <cinttypes>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <sys/mman.h>
#include <thread>
#include <vector>

extern "C"
{
    extern uint8_t* mw2_guest_base;
    uint32_t mw2_kernel_create_thread_block(uint32_t threadId);

    using Raise = void (*)(uint32_t pcr, uint32_t mask, bool blank);
    using Body = void (*)(void* context, Raise raise);
    void cpp_rig_set_interrupt_callback(uint32_t callback, uint32_t context);
    void cpp_rig_on_consumer_thread(Body body, void* context);
    void mw2_rig_set_interrupt_callback(uint32_t callback, uint32_t context);
    void mw2_rig_on_consumer_thread(Body body, void* context);

    void cpp_rig_worker(uint32_t pcr);
    void cpp_rig_vblank_worker(uint32_t pcr);
    void cpp_rig_stop_loop();
    void cpp_rig_forget_ring();
    void cpp_rig_set_ring_buffer(uint32_t basePhysical, uint32_t sizeLog2);
    void cpp_rig_set_read_pointer_write_back(uint32_t addressPhysical, uint32_t blockSizeLog2);
    uint32_t cpp_rig_write_back();
    void cpp_rig_loop_counts(uint64_t* vblanks, uint64_t* busyNanoseconds);
    void mw2_rig_worker(uint32_t pcr);
    void mw2_rig_vblank_worker(uint32_t pcr);
    void mw2_rig_stop_loop();
    void mw2_rig_forget_ring();
    void mw2_rig_set_ring_buffer(uint32_t basePhysical, uint32_t sizeLog2);
    void mw2_rig_set_read_pointer_write_back(uint32_t addressPhysical, uint32_t blockSizeLog2);
    void mw2_rig_loop_counts(uint64_t* vblanks, uint64_t* busyNanoseconds);
}

namespace
{
    uint64_t g_state;
    uint64_t g_compared;

    uint64_t Next()
    {
        uint64_t z = (g_state += 0x9E3779B97F4A7C15ull);
        z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ull;
        z = (z ^ (z >> 27)) * 0x94D049BB133111EBull;
        return z ^ (z >> 31);
    }
    uint32_t Below(uint32_t n) { return uint32_t(Next() % n); }

    constexpr uint32_t kHandler = uint32_t(PPC_CODE_BASE) + 0x100;   // has the stub
    constexpr uint32_t kNothing = uint32_t(PPC_CODE_BASE) + 0x200;   // has no function
    constexpr uint32_t kProcessor = 268;
    constexpr uint32_t kDisplayStatus = 0x7FC80000u + 0x6544u;
    constexpr uint32_t kWritePointer = 0x7FC80000u + 0x714u;   // CP_RB_WPTR

    uint8_t* Guest() { return mw2_guest_base; }
    uint32_t Load32(uint32_t address)
    {
        uint32_t value;
        std::memcpy(&value, Guest() + address, 4);
        return __builtin_bswap32(value);
    }
    void Store32(uint32_t address, uint32_t value)
    {
        value = __builtin_bswap32(value);
        std::memcpy(Guest() + address, &value, 4);
    }

    // ---- the loops' clock and record ----------------------------------------------------

    struct Loop
    {
        uint64_t state = 0;          // the case's randomness, drawn the same for both sides
        uint64_t now = 0;            // nanoseconds
        uint64_t period = 16'666'667;   // the blank period last given, as the loop takes it
        uint32_t limit = 0;          // events before the loop is stopped
        uint32_t wptr = 0;
        uint32_t writeBack = 0;      // where the read pointer is written back (the C++'s)
        int side = 0;
        bool on = false, stopped = false;
        std::vector<uint64_t> events;
    };
    Loop g_loop;
    // Each write-back's place, as the C++ found it, and every place used.
    std::map<uint32_t, uint32_t> g_writeBacks;
    std::set<uint32_t> g_writeBackPlaces;

    uint64_t LoopNext()
    {
        uint64_t z = (g_loop.state += 0x9E3779B97F4A7C15ull);
        z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ull;
        z = (z ^ (z >> 27)) * 0x94D049BB133111EBull;
        return z ^ (z >> 31);
    }
    uint32_t LoopBelow(uint32_t n) { return uint32_t(LoopNext() % n); }

    void Event(uint64_t kind, uint64_t a = 0, uint64_t b = 0)
    {
        g_loop.events.insert(g_loop.events.end(), { kind, a, b, g_loop.now });
        if (!g_loop.stopped && g_loop.events.size() / 4 >= g_loop.limit)
        {
            g_loop.stopped = true;
            if (g_loop.side) mw2_rig_stop_loop();
            else cpp_rig_stop_loop();
        }
    }

    void Sleep(uint64_t nanoseconds)
    {
        Event('S', nanoseconds);
        uint64_t late;
        const uint32_t kind = LoopBelow(64);
        if (kind < 44) late = LoopBelow(60'000);
        else if (kind < 56) late = 1'000'000 + LoopBelow(2'000'000);
        else if (kind < 59) late = 4 * g_loop.period + LoopBelow(2);   // either side of "more than 4 periods"
        else if (kind < 61) late = (2 + LoopBelow(3)) * g_loop.period + LoopBelow(1000);
        else late = 20'000'000 + LoopBelow(120'000'000);
        g_loop.now += nanoseconds + late;
    }

    void Bundle()
    {
        Event('U');
        if (LoopBelow(16) == 0) g_loop.now += LoopBelow(30'000);
    }

    uint64_t BlankPeriod()
    {
        uint64_t period;
        switch (LoopBelow(8))
        {
        case 5: period = 16'666'667; break;
        case 6: period = 4'000'000 + LoopBelow(36'000'000); break;
        case 7: period = 1 + LoopBelow(2); break;
        default: period = 0; break;
        }
        Event('P', period);
        g_loop.period = period ? period : 16'666'667;
        return period;
    }

    // At every lap's occlusion delivery: what the write-back holds, then what the
    // title's threads might have done meanwhile.
    void Deliver()
    {
        Event('D', g_loop.writeBack ? Load32(g_loop.writeBack) : ~0ull, Load32(kWritePointer));
        const uint32_t kind = LoopBelow(64);
        if (kind == 0)
        {
            const uint32_t base = LoopBelow(0x2000) << 12, size = 4 + LoopBelow(10);
            if (g_loop.side) mw2_rig_set_ring_buffer(base, size);
            else cpp_rig_set_ring_buffer(base, size);
        }
        else if (kind == 1)
        {
            const uint32_t physical = LoopBelow(4) ? LoopBelow(0x400000) << 2 : uint32_t(LoopNext()) & ~3u;
            const uint32_t block = LoopBelow(16);
            if (g_loop.side) mw2_rig_set_read_pointer_write_back(physical, block);
            else
            {
                cpp_rig_set_read_pointer_write_back(physical, block);
                g_writeBacks[physical] = cpp_rig_write_back();
                g_writeBackPlaces.insert(cpp_rig_write_back());
            }
            g_loop.writeBack = g_writeBacks[physical];
        }
        else if (kind < 10)
        {
            g_loop.wptr = LoopBelow(4) ? g_loop.wptr + LoopBelow(80) : uint32_t(LoopNext());
            Store32(kWritePointer, g_loop.wptr);
        }
    }

    void Execute(uint32_t base, uint32_t readIndex, uint32_t mask, uint32_t dwords)
    {
        Event('X', uint64_t(base) << 32 | readIndex, uint64_t(mask) << 32 | dwords);
        switch (LoopBelow(256))
        {
        case 0: g_loop.now += 200'000'000; break;
        case 1: g_loop.now += 200'000'001; break;
        case 2: g_loop.now += 200'000'000 + LoopBelow(400'000'000); break;
        default: g_loop.now += LoopBelow(500'000); break;
        }
    }
}

// The loops' calls, from the C++ (gpu_cpp.cpp) and from the Rust (gpu.rs's rig_loop_*,
// and what it calls of runtime/rust_calls.cpp), on the one record of the side running.
extern "C"
{
    uint64_t cpp_loop_now() { return g_loop.now; }
    uint64_t rig_loop_now() { return g_loop.now; }
    void cpp_loop_sleep(uint64_t nanoseconds) { Sleep(nanoseconds); }
    void rig_loop_sleep(uint64_t nanoseconds) { Sleep(nanoseconds); }

    void cpp_loop_bundle() { Bundle(); }
    void rig_loop_bundle() { Bundle(); }

    uint64_t cpp_loop_blank_period() { return BlankPeriod(); }
    uint64_t rig_loop_blank_period() { return BlankPeriod(); }

    void cpp_loop_deliver() { Deliver(); }
    void mw2_cp_deliver_occlusion_queries() { Deliver(); }

    void cpp_loop_execute(uint32_t base, uint32_t readIndex, uint32_t mask, uint32_t dwords) { Execute(base, readIndex, mask, dwords); }
    void mw2_cp_execute_ring(uint32_t base, uint32_t readIndex, uint32_t mask, uint32_t dwords) { Execute(base, readIndex, mask, dwords); }

    void cpp_loop_batch_begin(uint32_t base, uint32_t readIndex, uint32_t mask, uint32_t dwords)
    {
        Event('B', uint64_t(base) << 32 | readIndex, uint64_t(mask) << 32 | dwords);
    }
    void mw2_stream_batch_begin(uint32_t base, uint32_t readIndex, uint32_t mask, uint32_t dwords)
    {
        Event('B', uint64_t(base) << 32 | readIndex, uint64_t(mask) << 32 | dwords);
    }
    void cpp_loop_batch_end() { Event('E'); }
    void mw2_stream_batch_end() { Event('E'); }
    void cpp_loop_idle() { Event('I'); }
    void mw2_renderer_idle() { Event('I'); }
}

namespace
{

    // What the handler saw, per side.
    std::vector<uint64_t> g_seen;
    std::atomic<int> g_inside{ 0 };
    std::atomic<bool> g_overlapped{ false };
    std::atomic<uint64_t> g_calls{ 0 };
    bool g_record = true;

    void Handler(PPCContext& ctx, uint8_t* base)
    {
        if (g_loop.on)
        {
            // A blank of the vblank loop: what it was called with and the status it saw,
            // which is cleared for the next blank to write again.
            Event('V', ctx.r3.u64 << 32 | ctx.r4.u32, uint64_t(ctx.r13.u32) << 32 | Load32(kDisplayStatus));
            Store32(kDisplayStatus, 0);
            return;
        }
        if (g_inside.fetch_add(1) != 0) g_overlapped = true;
        g_calls++;
        if (g_record)
        {
            const uint32_t pcr = ctx.r13.u32;
            g_seen.insert(g_seen.end(), { ctx.r3.u64, ctx.r4.u64, ctx.r13.u64, base[pcr + kProcessor],
                                          ctx.r1.u64 & 0xFFF, ctx.r1.u64 >> 32, ctx.fpscr.csr, Load32(kDisplayStatus) });
        }
        // 4 MB of the host stack, every page touched.
        volatile uint8_t big[4 << 20];
        for (size_t at = 0; at < sizeof big; at += 4096) big[at] = uint8_t(at);
        g_inside.fetch_sub(1);
    }

    struct Step
    {
        uint32_t pcr, mask;
        bool blank;
        uint8_t processor;   // the byte at KPCR + 268 before
    };

    struct Script
    {
        std::vector<Step> steps;
        uint32_t threads;    // 1, or 2 raising at once
        uint32_t repeats;
        uint32_t callback, context;
        std::string text;
    };

    struct Outcome
    {
        std::vector<uint64_t> seen, after;
        uint64_t calls;
        bool overlapped;
        std::string log;
    };

    // What the side printed since it was taken.
    std::string Taken(int side)
    {
        rig_log::Stop();
        std::string text = std::move(rig_log::g_log[side]);
        rig_log::g_log[side].clear();
        return text;
    }

    void RunSteps(const Script& script, Raise raise, std::vector<uint64_t>* after)
    {
        for (uint32_t r = 0; r < script.repeats; r++)
            for (const Step& step : script.steps)
            {
                if (after)
                {
                    Guest()[step.pcr + kProcessor] = step.processor;
                    std::memset(Guest() + kDisplayStatus, 0, 4);
                }
                raise(step.pcr, step.mask, step.blank);
                if (after) after->push_back(Guest()[step.pcr + kProcessor]);
            }
    }

    // Every case on one thread of the side's, as the consumer raises them all on one:
    // a thread's guest stack is allocated once, at its first interrupt.
    struct Side
    {
        bool rust;
        const std::vector<Script>* scripts;
        std::vector<Outcome> outcomes;
    };

    void RunSide(void* context, Raise raise)
    {
        Side& side = *static_cast<Side*>(context);
        for (const Script& script : *side.scripts)
        {
            g_seen.clear();
            g_calls = 0;
            g_overlapped = false;
            g_record = script.threads == 1;
            rig_log::Take(side.rust);
            if (side.rust) mw2_rig_set_interrupt_callback(script.callback, script.context);
            else cpp_rig_set_interrupt_callback(script.callback, script.context);
            Outcome outcome;
            if (script.threads == 1)
                RunSteps(script, raise, &outcome.after);
            else
            {
                // A second raising thread at once (a std::thread): the turn is the side's own.
                std::thread other(RunSteps, std::cref(script), raise, nullptr);
                RunSteps(script, raise, nullptr);
                other.join();
            }
            outcome.seen = g_seen;
            outcome.calls = g_calls.load();
            outcome.overlapped = g_overlapped.load();
            outcome.log = Taken(side.rust);
            side.outcomes.push_back(std::move(outcome));
        }
    }

    [[noreturn]] void Fail(const char* what, const Script& script)
    {
        std::printf("DIFFERENCE: %s\n%s", what, script.text.c_str());
        std::exit(1);
    }

    void Interrupts(uint32_t cases)
    {
        const uint32_t pcrs[2] = { mw2_kernel_create_thread_block(0xF000), mw2_kernel_create_thread_block(0xF001) };
        std::vector<Script> scripts(cases);
        for (Script& script : scripts)
        {
            script.threads = Below(16) == 0 ? 2 : 1;
            script.repeats = script.threads == 2 ? 200 : 1;
            const uint32_t steps = 1 + Below(6);
            for (uint32_t s = 0; s < steps; s++)
            {
                Step step;
                step.pcr = Below(16) ? pcrs[Below(2)] : 0;
                step.blank = Below(4) == 0;
                step.mask = Below(4) ? Below(64) : uint32_t(Next());
                step.processor = uint8_t(Next());
                script.steps.push_back(step);
                char line[96];
                std::snprintf(line, sizeof line, "step pcr %08X mask %08X blank %u processor %u\n", step.pcr, step.mask,
                              step.blank, step.processor);
                script.text += line;
            }
            const uint32_t pick = Below(16);
            script.callback = pick == 0 ? 0 : pick == 1 ? kNothing : kHandler;
            script.context = uint32_t(Next());
            char line[96];
            std::snprintf(line, sizeof line, "callback %08X context %08X threads %u\n", script.callback, script.context,
                          script.threads);
            script.text += line;
        }
        Side cpp{ false, &scripts, {} }, rust{ true, &scripts, {} };
        cpp_rig_on_consumer_thread(RunSide, &cpp);
        mw2_rig_on_consumer_thread(RunSide, &rust);
        if (cpp.outcomes.size() != cases || rust.outcomes.size() != cases) Fail("a side did not finish", scripts[0]);
        uint64_t calls = 0;
        for (const Outcome& outcome : cpp.outcomes) calls += outcome.calls;
        // A rig whose handler is never reached compares nothing.
        if (cases >= 100 && calls == 0) Fail("the handler was never called", scripts[0]);
        std::printf("%llu calls of the handler on each side\n", (unsigned long long)calls);
        for (uint32_t i = 0; i < cases; i++)
        {
            const Outcome &a = cpp.outcomes[i], &b = rust.outcomes[i];
            if (a.overlapped || b.overlapped)
                Fail(a.overlapped ? "the C++ handler was entered twice at once" : "the Rust handler was entered twice at once",
                     scripts[i]);
            if (a.calls != b.calls) Fail("the number of calls", scripts[i]);
            if (a.seen != b.seen)
            {
                size_t at = 0;
                while (at < a.seen.size() && at < b.seen.size() && a.seen[at] == b.seen[at]) at++;
                char text[160];
                std::snprintf(text, sizeof text, "what the handler saw, word %zu: C++ %llX, Rust %llX", at,
                              at < a.seen.size() ? (unsigned long long)a.seen[at] : 0ull,
                              at < b.seen.size() ? (unsigned long long)b.seen[at] : 0ull);
                Fail(text, scripts[i]);
            }
            if (a.after != b.after) Fail("the processor number after the raise", scripts[i]);
            if (a.log != b.log)
            {
                const std::string text = "the lines printed:\n  C++:  " + a.log + "  Rust: " + b.log;
                Fail(text.c_str(), scripts[i]);
            }
            g_compared += 2 + a.seen.size() + a.after.size();
        }
    }
}

namespace
{
    // ---- the loops --------------------------------------------------------------------

    struct LoopCase
    {
        uint64_t seed;
        uint32_t limit;
        bool ring, writeBack;
        uint32_t ringBase, ringSize, writeBackPhysical, writeBackBlock;
        uint32_t callback, context;
        std::string text;
    };

    struct LoopOutcome
    {
        std::vector<uint64_t> events;
        std::string log;
        uint64_t vblanks, busy;   // in the case
    };

    struct LoopSide
    {
        bool vblank;
        int side;
        uint32_t pcr;
        const std::vector<LoopCase>* cases;
        std::vector<LoopOutcome> outcomes;
    };

    // Every case of a side on one thread, as the loops each run on one: a thread's guest
    // stack is allocated at its first interrupt and kept.
    void RunLoops(void* context, Raise)
    {
        LoopSide& side = *static_cast<LoopSide*>(context);
        const int s = side.side;
        for (const LoopCase& c : *side.cases)
        {
            for (const uint32_t place : g_writeBackPlaces)
                if (place) Store32(place, 0);
            Store32(kWritePointer, 0);
            Store32(kDisplayStatus, 0);
            g_loop.state = c.seed;
            g_loop.now = 1'000'000'000'000ull + c.seed % 1'000'000'000ull;
            g_loop.period = 16'666'667;
            g_loop.limit = c.limit;
            g_loop.wptr = 0;
            g_loop.writeBack = 0;
            g_loop.side = s;
            g_loop.stopped = false;
            g_loop.events.clear();
            g_loop.on = side.vblank;
            // The case's own counts: the interrupts part's blanks, raised from two
            // threads at once, are not all counted by the C++, whose count is the vblank
            // thread's alone in the game.
            uint64_t vblanks, busy;
            if (s) mw2_rig_loop_counts(&vblanks, &busy);
            else cpp_rig_loop_counts(&vblanks, &busy);
            rig_log::Take(s);
            if (s) mw2_rig_forget_ring(), mw2_rig_set_interrupt_callback(c.callback, c.context);
            else cpp_rig_forget_ring(), cpp_rig_set_interrupt_callback(c.callback, c.context);
            if (c.ring)
            {
                if (s) mw2_rig_set_ring_buffer(c.ringBase, c.ringSize);
                else cpp_rig_set_ring_buffer(c.ringBase, c.ringSize);
            }
            if (c.writeBack)
            {
                if (s) mw2_rig_set_read_pointer_write_back(c.writeBackPhysical, c.writeBackBlock);
                else
                {
                    cpp_rig_set_read_pointer_write_back(c.writeBackPhysical, c.writeBackBlock);
                    g_writeBacks[c.writeBackPhysical] = cpp_rig_write_back();
                    g_writeBackPlaces.insert(cpp_rig_write_back());
                }
                g_loop.writeBack = g_writeBacks[c.writeBackPhysical];
            }
            if (side.vblank) s ? mw2_rig_vblank_worker(side.pcr) : cpp_rig_vblank_worker(side.pcr);
            else s ? mw2_rig_worker(side.pcr) : cpp_rig_worker(side.pcr);
            g_loop.on = false;
            LoopOutcome outcome;
            outcome.log = Taken(s);
            outcome.events = std::move(g_loop.events);
            g_loop.events.clear();
            if (s) mw2_rig_loop_counts(&outcome.vblanks, &outcome.busy);
            else cpp_rig_loop_counts(&outcome.vblanks, &outcome.busy);
            outcome.vblanks -= vblanks;
            outcome.busy -= busy;
            side.outcomes.push_back(std::move(outcome));
        }
    }

    void FailLoop(const char* what, const LoopCase& c)
    {
        std::printf("DIFFERENCE: %s\n%s", what, c.text.c_str());
        std::exit(1);
    }

    void Loops(bool vblank, uint32_t cases)
    {
        const uint32_t pcr = mw2_kernel_create_thread_block(vblank ? 0xF001 : 0xF000);
        std::vector<LoopCase> loopCases(cases);
        for (LoopCase& c : loopCases)
        {
            c.seed = Next();
            c.limit = 1 + Below(Below(4) ? 400 : 4000);
            c.ring = Below(4) != 0;
            c.ringBase = Below(0x2000) << 12;
            c.ringSize = 4 + Below(10);
            c.writeBack = Below(2) != 0;
            c.writeBackPhysical = Below(4) ? Below(0x400000) << 2 : uint32_t(Next()) & ~3u;
            c.writeBackBlock = Below(16);
            c.callback = Below(32) ? kHandler : Below(2) ? kNothing : 0;
            c.context = uint32_t(Next());
            char text[200];
            std::snprintf(text, sizeof text, "%s case %016llX: %u events, ring %u %08X 2^%u, write-back %u %08X, callback %08X %08X\n",
                          vblank ? "vblank" : "worker", (unsigned long long)c.seed, c.limit, c.ring, c.ringBase, c.ringSize,
                          c.writeBack, c.writeBackPhysical, c.callback, c.context);
            c.text = text;
        }
        LoopSide cpp{ vblank, 0, pcr, &loopCases, {} }, rust{ vblank, 1, pcr, &loopCases, {} };
        cpp_rig_on_consumer_thread(RunLoops, &cpp);
        mw2_rig_on_consumer_thread(RunLoops, &rust);
        uint64_t events = 0, kinds[128] = {};
        for (uint32_t i = 0; i < cases; i++)
        {
            const LoopOutcome &a = cpp.outcomes[i], &b = rust.outcomes[i];
            if (a.events != b.events)
            {
                size_t at = 0;
                while (at < a.events.size() && at < b.events.size() && a.events[at] == b.events[at]) at++;
                at &= ~size_t(3);
                const auto show = [&](const std::vector<uint64_t>& e) {
                    char text[120];
                    if (at >= e.size()) return std::string("nothing");
                    std::snprintf(text, sizeof text, "%c %llX %llX at %llu", char(e[at]), (unsigned long long)e[at + 1],
                                  (unsigned long long)e[at + 2], (unsigned long long)e[at + 3]);
                    return std::string(text);
                };
                const std::string text = "event " + std::to_string(at / 4) + ": C++ " + show(a.events) + ", Rust " + show(b.events);
                FailLoop(text.c_str(), loopCases[i]);
            }
            if (a.log != b.log)
            {
                const std::string text = "the lines printed:\n  C++:  " + a.log + "  Rust: " + b.log;
                FailLoop(text.c_str(), loopCases[i]);
            }
            if (a.vblanks != b.vblanks || a.busy != b.busy)
            {
                char text[160];
                std::snprintf(text, sizeof text, "the blanks counted or the consumer's busy time: C++ %llu, %llu ns, Rust %llu, %llu ns",
                              (unsigned long long)a.vblanks, (unsigned long long)a.busy, (unsigned long long)b.vblanks,
                              (unsigned long long)b.busy);
                FailLoop(text, loopCases[i]);
            }
            for (size_t at = 0; at < a.events.size(); at += 4) kinds[a.events[at] & 127]++;
            events += a.events.size() / 4;
            g_compared += a.events.size() + 3;
        }
        // A loop that never reaches a branch compares nothing there.
        const char* wanted = vblank ? "SUPV" : "SDXBEI";
        if (cases >= 1000)
            for (const char* kind = wanted; *kind; kind++)
                if (!kinds[uint8_t(*kind)]) FailLoop((std::string("no event ") + *kind).c_str(), loopCases[0]);
        std::printf("%s: %llu events on each side", vblank ? "vblank" : "worker", (unsigned long long)events);
        for (const char* kind = wanted; *kind; kind++) std::printf(", %c %llu", *kind, (unsigned long long)kinds[uint8_t(*kind)]);
        std::printf("\n");
    }
}

int main(int argc, char** argv)
{
    if (argc < 3)
    {
        std::fprintf(stderr, "usage: interrupts <seed> <cases> [interrupts]...\n");
        return 2;
    }
    g_state = std::strtoull(argv[1], nullptr, 10) * 0x2545F4914F6CDD1Dull + 1;
    const uint32_t cases = uint32_t(std::strtoul(argv[2], nullptr, 10));

    // The guest's 4 GB, and the stub where the function table has the handler's slot.
    constexpr uint64_t kMapped = (1ull << 32) + 0x10000;
    auto* mapped = static_cast<uint8_t*>(mmap(nullptr, kMapped + 0x10000, PROT_READ | PROT_WRITE,
                                              MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0));
    if (mapped == MAP_FAILED) return 3;
    mw2_guest_base = reinterpret_cast<uint8_t*>((reinterpret_cast<uintptr_t>(mapped) + 0xFFFF) & ~uintptr_t(0xFFFF));
    uint8_t* base = mw2_guest_base;
    PPC_LOOKUP_FUNC(base, kHandler) = Handler;

    for (int i = 3; i < argc; i++)
    {
        const std::string what = argv[i];
        if (what == "interrupts") Interrupts(cases);
        else if (what == "vblank") Loops(true, cases / 4 ? cases / 4 : 1);
        else if (what == "worker") Loops(false, cases / 4 ? cases / 4 : 1);
        else { std::fprintf(stderr, "unknown part %s\n", argv[i]); return 2; }
    }
    std::printf("%" PRIu64 " fields compared\n", g_compared);
    return 0;
}
