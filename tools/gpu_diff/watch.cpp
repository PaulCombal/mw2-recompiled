// tools/gpu_diff/watch.cpp: the memory watch, C++ (runtime/gpu/memory_watch.cpp as it is,
// included here for its file statics) against the Rust (src/runtime/gpu/memory_watch.rs,
// through exports.rs), on real page protections: the rig's guest space has a memfd mapped
// twice, at 0xA0000000 and 0xC0000000, as the game's physical bank is, and a fault
// handler of the rig's own that asks the side under test, as crash.rs asks the watch.
// mprotect is wrapped (-Wl,--wrap=mprotect): every protection change either side makes
// is logged with the state its first page holds at that moment, and a change can be made
// to fail, or, for the parts below that ask for it, a writer's WillWrite can be run
// between the consumer's store of Arming and its protect.
//
//   watch <seed> <cases> watch      scripts, one side and then the other on the same
//       memory: SetShadow (a shadow, null, one too small), Serve, Track, Unwritten,
//       WillWrite, real writes through either window that fault, HandleFault called on
//       any address, completed serials and frames, a failed protect, Loosened, the
//       report, the costs, the capture's reads and the command trace's lines. After each
//       step: the answer, the faults the step's write took, every protection change with
//       the state at its time, each page's state, strikes, armed frame, stamp and last
//       submission, the counters, the shadow's bytes of the pages copied and the
//       loosened counts; at the end of a case the memory and the lines each side printed.
//       The Rust's one named change (D15: a fault on a page that reads Loose makes it
//       writable again) is told apart: the Rust's protections of one page to writable
//       while it reads Loose are taken out and must number the Rust's loose faults.
//   watch <seed> <iterations> stress    two writer threads (real writes, WillWrite)
//       against the consumer's Serve and Track on sixteen pages, with now and then a
//       WillWrite forced between Arming and the protect; each side in turn. Every 65536
//       iterations the writers stop and every Watched page's copy must equal memory. A
//       write that faults 100,000 times over is a livelock: the rig makes the page
//       writable to go on, and counts it. The Rust must have none; the C++'s are shown.
//   watch <seed> <cases> livelock   the interleaving itself, once a side: a page armed,
//       written, armed again with a WillWrite between the store of Arming and the
//       protect, then written. The C++ must livelock (else the test proves nothing), the
//       Rust must not.
//   watch <seed> <cases> teardown   that page, Loose and read-only, when the watch is
//       turned off, then written: the C++ leaves it read-only (the write is taken for a
//       crash, which the rig lets through; the test needs that), the Rust's write must not
//       fault. In the scripts, the Rust's set_shadow(null) protections of Loose pages are
//       taken out and must number the pages that read Loose, window by window.
//
// Exits non-zero at the first difference, printing the case.
#include "gpu/memory_watch.cpp"
#include "rig_log.h"

#include <atomic>
#include <cerrno>
#include <cinttypes>
#include <csignal>
#include <cstdarg>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <thread>
#include <vector>

#include <fcntl.h>
#include <linux/falloc.h>
#include <sys/mman.h>
#include <unistd.h>

// What memory_watch.cpp calls of the capture and the command trace, and what the Rust
// calls of them through runtime/rust_calls.cpp: the same lines from either side.
namespace gpu::stream
{
    namespace detail
    {
        bool capturing = false, replaying = false, counting = false;
    }
    void ReadSlow(uint32_t address, uint64_t bytes)
    {
        std::fprintf(stderr, "[rig] capture read %08X %" PRIu64 "\n", address, bytes);
    }
}
extern "C" const bool* const mw2_stream_capturing = &gpu::stream::detail::capturing;
extern "C" const bool* const mw2_stream_replaying = &gpu::stream::detail::replaying;
extern "C" const bool* const mw2_stream_counting = &gpu::stream::detail::counting;
extern "C" void mw2_stream_read(uint32_t address, uint64_t bytes) { gpu::stream::ReadSlow(address, bytes); }

namespace vk::trace
{
    bool g_wanted = false;
    bool Wanted() { return g_wanted; }
    void Note(const char* format, ...)
    {
        std::va_list arguments;
        va_start(arguments, format);
        std::fputs("[rig] trace ", stderr);
        std::vfprintf(stderr, format, arguments);
        std::fputc('\n', stderr);
        va_end(arguments);
    }
    uint64_t FnvWriteCombined(const void* bytes, size_t size)
    {
        uint64_t hash = 1469598103934665603ull;
        for (size_t i = 0; i < size; i++) hash = (hash ^ static_cast<const uint8_t*>(bytes)[i]) * 1099511628211ull;
        return hash;
    }
}
// runtime/rust_calls.cpp's, as it is there.
extern "C" void mw2_trace_shadow_arm(uint32_t first, uint32_t count, const uint8_t* shadow)
{
#if MW2_TRACE
    if (vk::trace::Wanted())
        for (uint32_t p = first; p < first + count; p++)
            vk::trace::Note("ShadowArm page=%u fnv=%016llx", p,
                            (unsigned long long)vk::trace::FnvWriteCombined(shadow + size_t(p) * gpu::watch::kPageBytes,
                                                                            gpu::watch::kPageBytes));
#endif
}

extern "C"
{
    bool mw2_memory_watch_enabled();
    void mw2_memory_watch_set_shadow(uint8_t* shadow, uint32_t bytes);
    bool mw2_memory_watch_serve(uint32_t physical, uint32_t bytes, uint64_t recording, uint64_t completed, uint64_t frame);
    uint64_t mw2_memory_watch_track(uint32_t physical, uint32_t bytes, uint64_t completed, uint64_t frame);
    bool mw2_memory_watch_unwritten(uint32_t physical, uint32_t bytes, uint64_t since);
    uint32_t mw2_memory_watch_loosened(uint32_t page);
    bool mw2_rig_watch_handle_fault(const void* address);
    void mw2_rig_watch_will_write(uint32_t address, uint32_t bytes);
    void mw2_rig_watch_costs(uint64_t* out);
    void mw2_rig_watch_report();
    void mw2_rig_watch_page(uint32_t page, uint64_t* out);
    void mw2_rig_watch_counters(uint64_t* out);
    uint8_t mw2_rig_watch_state(uint32_t page);
    void mw2_rig_watch_reset();
    int __real_mprotect(void* address, size_t length, int protection);
}

namespace
{
    constexpr uint64_t kBytes = kPhysicalBytes;   // memory_watch.cpp's
    constexpr uint32_t kLivelock = 100000;

    uint8_t* r_base = nullptr;
    int r_fd = -1;
    uint8_t* r_shadow[2];
    int r_side = 0;   // whose watch the fault handler and the wrapper ask

    uint64_t r_rng;
    uint64_t Next()
    {
        uint64_t z = (r_rng += 0x9E3779B97F4A7C15ull);
        z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ull;
        z = (z ^ (z >> 27)) * 0x94D049BB133111EBull;
        return z ^ (z >> 31);
    }
    uint32_t Below(uint32_t n) { return uint32_t(Next() % n); }

    // ---- the two sides -----------------------------------------------------------------
    uint64_t r_completed = 0;
    uint64_t Completed() { return r_completed; }

    uint8_t StateOf(uint32_t page)
    {
        return r_side == 0 ? g_state[page].load(std::memory_order_relaxed) : mw2_rig_watch_state(page);
    }
    bool HandleFault(const void* at) { return r_side == 0 ? gpu::watch::HandleFault(at) : mw2_rig_watch_handle_fault(at); }
    void WillWrite(uint32_t address, uint32_t bytes)
    {
        if (r_side == 0) gpu::watch::WillWrite(address, bytes);
        else mw2_rig_watch_will_write(address, bytes);
    }
    // The Rust's set_shadow(null) also makes each run of Loose pages writable (D15's page
    // that reads Loose and is read-only): how many pages read Loose as it began, which its
    // writable protections of Loose pages must number, window by window.
    bool r_teardown = false;
    uint64_t r_teardownLoose = 0;
    void SetShadow(uint8_t* shadow, uint32_t bytes)
    {
        if (r_side == 0) { gpu::watch::SetShadow(shadow, bytes); return; }
        r_teardown = !shadow || bytes < kPhysicalBytes;
        r_teardownLoose = 0;
        if (r_teardown)
            for (uint32_t page = 0; page < kPages; page++) r_teardownLoose += mw2_rig_watch_state(page) == kLoose;
        mw2_memory_watch_set_shadow(shadow, bytes);
    }
    bool Serve(uint32_t physical, uint32_t bytes, uint64_t recording, uint64_t frame)
    {
        return r_side == 0 ? gpu::watch::Serve(physical, bytes, recording, Completed, frame)
                           : mw2_memory_watch_serve(physical, bytes, recording, r_completed, frame);
    }
    uint64_t Track(uint32_t physical, uint32_t bytes, uint64_t frame)
    {
        return r_side == 0 ? gpu::watch::Track(physical, bytes, Completed, frame)
                           : mw2_memory_watch_track(physical, bytes, r_completed, frame);
    }
    bool Unwritten(uint32_t physical, uint32_t bytes, uint64_t since)
    {
        return r_side == 0 ? gpu::watch::Unwritten(physical, bytes, since) : mw2_memory_watch_unwritten(physical, bytes, since);
    }
    uint32_t Loosened(uint32_t page) { return r_side == 0 ? gpu::watch::Loosened(page) : mw2_memory_watch_loosened(page); }
    bool Enabled() { return r_side == 0 ? gpu::watch::Enabled() : mw2_memory_watch_enabled(); }
    void Report()
    {
        if (r_side == 0) gpu::watch::Report();
        else mw2_rig_watch_report();
    }
    void Costs(uint64_t out[3])
    {
        if (r_side == 0)
        {
            const gpu::watch::Costs c = gpu::watch::CostsSoFar();
            out[0] = c.faults, out[1] = c.protects, out[2] = c.protectNanoseconds;
        }
        else mw2_rig_watch_costs(out);
    }
    void Page(uint32_t page, uint64_t out[5])
    {
        if (r_side == 0)
        {
            out[0] = g_state[page].load(), out[1] = g_strikes[page], out[2] = g_armedFrame[page];
            out[3] = g_armedAt[page], out[4] = g_lastServed[page];
        }
        else mw2_rig_watch_page(page, out);
    }
    // The stamp, the counters in the report's order, then (the Rust's alone) the loose faults.
    void Counters(uint64_t out[16])
    {
        if (r_side == 0)
        {
            const uint64_t c[16] = { g_stamp, g_served, g_servedBytes, g_arms, g_pagesArmed, g_armedWrittenDuring,
                                     g_armFailures, g_refusedInFlight, g_refusedBackingOff, g_refusedOutside,
                                     g_faults.load(), g_bulkWrites.load(), g_tracked, g_protects.load(),
                                     uint64_t(g_enabled.load()), 0 };
            std::memcpy(out, c, sizeof c);
        }
        else mw2_rig_watch_counters(out);
    }
    void ResetWatch()
    {
        if (r_side == 1) { mw2_rig_watch_reset(); return; }
        for (auto& state : g_state) state.store(0, std::memory_order_relaxed);
        std::memset(g_lastServed, 0, sizeof g_lastServed);
        std::memset(g_armedFrame, 0, sizeof g_armedFrame);
        std::memset(g_strikes, 0, sizeof g_strikes);
        std::memset(g_armedAt, 0, sizeof g_armedAt);
        g_stamp = 0;
        g_shadow = nullptr;
        g_enabled.store(false);
        g_faults = 0, g_bulkWrites = 0, g_protects = 0, g_protectNanoseconds = 0;
        for (auto& count : g_loosened) count.store(0, std::memory_order_relaxed);
        g_arms = g_pagesArmed = g_armedWrittenDuring = g_armFailures = 0;
        g_served = g_servedBytes = g_tracked = 0;
        g_refusedInFlight = g_refusedBackingOff = g_refusedOutside = 0;
    }

    // ---- the protections ---------------------------------------------------------------
    struct Protection { uint8_t window, prot, state; int8_t result; uint32_t page, pages; };
    constexpr size_t kMostProtections = 1 << 16;
    Protection r_protections[kMostProtections];
    size_t r_protectionCount = 0;
    bool r_logging = false;
    std::atomic<bool> r_failNext{ false };
    std::atomic<bool> r_inject{ false };    // WillWrite of r_injectAddress before the next read-only protect
    std::atomic<uint32_t> r_injectProbability{ 0 };   // stress: 1 in N read-only protects
    uint32_t r_injectAddress = 0, r_injectBytes = 4096;
    std::atomic<uint64_t> r_injected{ 0 };

    void WritableAgain(uint32_t page)
    {
        for (uint32_t window : { guest::kPhysicalBase, guest::kApertureBase })
            __real_mprotect(r_base + window + size_t(page) * 4096, 4096, PROT_READ | PROT_WRITE);
    }
}

extern "C" int __wrap_mprotect(void* address, size_t length, int protection)
{
    const uintptr_t at = uintptr_t(address) - uintptr_t(r_base);
    if (!r_base || uintptr_t(address) < uintptr_t(r_base) || at < guest::kPhysicalBase || at >= 0xE0000000u)
        return __real_mprotect(address, length, protection);
    const uint8_t window = at >= guest::kApertureBase;
    const uint32_t page = uint32_t((at - (window ? guest::kApertureBase : guest::kPhysicalBase)) / 4096);
    const uint8_t state = page < kPages ? StateOf(page) : 0xFF;
    if (protection == PROT_READ && window == 0)
    {
        // A writer's WillWrite between the consumer's store of Arming and its protect.
        bool inject = r_inject.exchange(false);
        const uint32_t odds = r_injectProbability.load(std::memory_order_relaxed);
        uint32_t injectAddress = r_injectAddress, injectBytes = r_injectBytes;
        if (!inject && odds && Below(odds) == 0)
        {
            inject = true;
            injectAddress = guest::kPhysicalBase + page * 4096, injectBytes = uint32_t(length);
        }
        if (inject)
        {
            r_injected.fetch_add(1);
            WillWrite(injectAddress, injectBytes);
        }
    }
    int result;
    if (protection == PROT_READ && r_failNext.exchange(false))
    {
        errno = ENOMEM;
        result = -1;
    }
    else
        result = __real_mprotect(address, length, protection);
    if (r_logging && r_protectionCount < kMostProtections)
        r_protections[r_protectionCount++] = { window, uint8_t(protection), state, int8_t(result), page,
                                               uint32_t(length / 4096) };
    return result;
}

namespace
{
    // ---- the fault handler -------------------------------------------------------------
    thread_local uintptr_t t_lastFault = 0;
    thread_local uint32_t t_faults = 0;      // in a row at t_lastFault
    thread_local volatile uint64_t t_writeFaults = 0;   // since the write began (the handler counts)
    std::atomic<uint64_t> r_livelocks{ 0 }, r_refused{ 0 };

    void OnFault(int, siginfo_t* info, void*)
    {
        auto* at = static_cast<uint8_t*>(info->si_addr);
        const uintptr_t offset = uintptr_t(at) - uintptr_t(r_base);
        if (info->si_code != SEGV_ACCERR || uintptr_t(at) < uintptr_t(r_base) || offset < guest::kPhysicalBase ||
            offset >= 0xE0000000u)
        {
            const char text[] = "rig: a fault outside the physical windows\n";
            write(2, text, sizeof text - 1);
            _exit(3);
        }
        const uint32_t page = uint32_t((offset - (offset >= guest::kApertureBase ? guest::kApertureBase : guest::kPhysicalBase)) / 4096);
        t_writeFaults = t_writeFaults + 1;
        if (uintptr_t(at) == t_lastFault)
        {
            if (++t_faults >= kLivelock)
            {
                r_livelocks.fetch_add(1);
                WritableAgain(page);
                t_faults = 0;
                return;
            }
        }
        else
            t_lastFault = uintptr_t(at), t_faults = 1;
        if (!HandleFault(at))
        {
            // Not the watch's: the rig lets the write through, and says so.
            r_refused.fetch_add(1);
            WritableAgain(page);
        }
    }

    // A real write through window `window` (0: 0xA0000000, 1: 0xC0000000). The faults it took.
    uint64_t Write(int window, uint32_t physical, uint32_t value)
    {
        t_lastFault = 0, t_faults = 0, t_writeFaults = 0;
        auto* word = reinterpret_cast<volatile uint32_t*>(r_base + (window ? guest::kApertureBase : guest::kPhysicalBase) +
                                                          physical);
        *word = value;
        std::atomic_signal_fence(std::memory_order_seq_cst);
        return t_writeFaults;
    }

    void Map()
    {
        void* space = mmap(nullptr, (1ull << 32) + 0x10000, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
        if (space == MAP_FAILED) { std::perror("mmap"); std::exit(2); }
        r_base = reinterpret_cast<uint8_t*>((uintptr_t(space) + 0xFFFF) & ~uintptr_t(0xFFFF));
        r_fd = memfd_create("rig-physical", 0);
        if (r_fd < 0 || ftruncate(r_fd, off_t(kBytes))) { std::perror("memfd"); std::exit(2); }
        for (uint32_t window : { guest::kPhysicalBase, guest::kApertureBase })
            if (mmap(r_base + window, kBytes, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_FIXED | MAP_NORESERVE, r_fd, 0) ==
                MAP_FAILED)
            { std::perror("mmap window"); std::exit(2); }
        for (auto& shadow : r_shadow)
        {
            shadow = static_cast<uint8_t*>(mmap(nullptr, kBytes, PROT_READ | PROT_WRITE,
                                                MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0));
            if (shadow == MAP_FAILED) { std::perror("mmap shadow"); std::exit(2); }
        }
        mw2_guest_base = r_base;
        struct sigaction action{};
        action.sa_sigaction = OnFault;
        action.sa_flags = SA_SIGINFO | SA_NODEFER;
        sigaction(SIGSEGV, &action, nullptr);
    }

    // Both windows writable, memory and both shadows zero, the side's watch at its start.
    void Clean(int side)
    {
        r_side = side;
        for (uint32_t window : { guest::kPhysicalBase, guest::kApertureBase })
            __real_mprotect(r_base + window, kBytes, PROT_READ | PROT_WRITE);
        fallocate(r_fd, FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE, 0, off_t(kBytes));
        madvise(r_shadow[side], kBytes, MADV_DONTNEED);
        ResetWatch();
        gpu::stream::detail::counting = false;
        vk::trace::g_wanted = false;
        r_failNext = false;
        r_inject = false;
        r_injectProbability = 0;
        r_protectionCount = 0;
    }

    uint64_t Fnv(const uint8_t* bytes, size_t size, uint64_t hash = 1469598103934665603ull)
    {
        for (size_t i = 0; i < size; i++) hash = (hash ^ bytes[i]) * 1099511628211ull;
        return hash;
    }

    // ---- watch: the scripts -------------------------------------------------------------
    uint64_t g_compared = 0;

    struct Script
    {
        uint32_t base;    // the case's first page of interest
        uint32_t first, last;   // the pages observed
    };

    void Add(std::string& to, const char* format, ...)
    {
        char text[512];
        std::va_list arguments;
        va_start(arguments, format);
        std::vsnprintf(text, sizeof text, format, arguments);
        va_end(arguments);
        to += text;
    }

    // A physical address the case is about, now and then one at the bank's ends or past it.
    uint32_t Address(const Script& s)
    {
        switch (Below(24))
        {
        case 0: return uint32_t(kBytes) - 1 - Below(9000);
        case 1: return uint32_t(kBytes) + Below(9000);
        case 2: return 0xFFFFFFF0u + Below(16);
        case 3: return Below(9000);
        default:
        {
            const int64_t at = int64_t(s.base) * 4096 - 8192 + Below(28 * 4096);
            return at < 0 ? uint32_t(Below(4096)) : uint32_t(at) & (Below(4) ? ~3u : ~0u);
        }
        }
    }
    uint32_t Size(uint32_t physical)
    {
        switch (Below(20))
        {
        case 0: return 0;
        case 1: return 1 + Below(4);
        case 2:
            // To the bank's end, or one past it; from far below the end, only one past (a
            // refusal), or the copy would be hundreds of megabytes.
            if (physical >= kBytes) return 1 + Below(4096);
            return uint32_t(kBytes) - physical + (physical > kBytes - 64 * 4096 ? Below(2) : 1);
        case 3: return 0xFFFFFFFFu;
        case 4: return 64 * 4096 + Below(4096);
        default: return 4 * (1 + Below(Below(4) ? 2048 : 6 * 1024));
        }
    }
    uint32_t Page(const Script& s) { return s.base + Below(24); }

    // The pages a call over [physical, physical + bytes) may arm, within the bank.
    void Range(uint32_t physical, uint32_t bytes, uint32_t& first, uint32_t& last)
    {
        first = last = 1;
        if (!bytes || uint64_t(physical) + bytes > kBytes) { first = 1, last = 0; return; }
        first = physical / 4096, last = uint32_t((uint64_t(physical) + bytes - 1) / 4096);
    }

    // What one side did in a step, as text the other side's must equal.
    std::string Observe(const Script& s, const char* step, uint64_t answer, uint32_t armedFirst, uint32_t armedLast,
                        uint64_t looseFaultsBefore)
    {
        std::string seen = step;
        Add(seen, " -> %" PRIu64 "\n", answer);
        // The protections, the Rust's D15 ones taken out and counted.
        uint64_t looseProtects = 0, teardownPages[2] = { 0, 0 };
        const bool teardown = r_side == 1 && r_teardown;
        r_teardown = false;
        for (size_t i = 0; i < r_protectionCount; i++)
        {
            const Protection& p = r_protections[i];
            if (teardown && p.prot == (PROT_READ | PROT_WRITE) && p.state == kLoose)
            {
                teardownPages[p.window] += p.pages;
                continue;
            }
            if (r_side == 1 && p.prot == (PROT_READ | PROT_WRITE) && p.pages == 1 && p.state == kLoose && p.window == 0)
            {
                looseProtects++;
                continue;
            }
            if (r_side == 1 && p.prot == (PROT_READ | PROT_WRITE) && p.pages == 1 && p.state == kLoose && p.window == 1)
                continue;
            Add(seen, "  protect %c %u+%u %s state %u -> %d\n", p.window ? 'C' : 'A', p.page, p.pages,
                p.prot == PROT_READ ? "ro" : "rw", p.state, p.result);
        }
        r_protectionCount = 0;
        if (teardown && (teardownPages[0] != r_teardownLoose || teardownPages[1] != r_teardownLoose))
            Add(seen, "  the Rust's set_shadow(null) made %" PRIu64 "+%" PRIu64 " Loose pages writable of %" PRIu64 "\n",
                teardownPages[0], teardownPages[1], r_teardownLoose);
        uint64_t counters[16];
        Counters(counters);
        const uint64_t looseFaults = counters[15] - looseFaultsBefore;
        if (looseProtects != looseFaults)
            Add(seen, "  the Rust's loose faults %" PRIu64 " and its protections of a Loose page %" PRIu64 " differ\n",
                looseFaults, looseProtects);
        // The C++ counts protections (two a change, one per window) only under MW2_REPORT;
        // the Rust's D15 ones are not the C++'s.
        uint64_t costs[3];
        Costs(costs);
        if (r_side == 1 && report::On())
        {
            counters[13] -= 2 * counters[15];
            costs[1] -= 2 * counters[15];
        }
        counters[15] = 0;
        Add(seen, "  counters");
        for (uint64_t c : counters) Add(seen, " %" PRIu64, c);
        seen += "\n";
        for (uint32_t page = s.first; page <= s.last; page++)
        {
            uint64_t p[5];
            Page(page, p);
            const uint32_t loosened = Loosened(page);
            if (p[0] || p[1] || p[2] || p[3] || p[4] || loosened)
                Add(seen, "  page %u: %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " loosened %u\n", page, p[0],
                    p[1], p[2], p[3], p[4], loosened);
        }
        // Pages outside the window of interest a step armed.
        for (uint32_t page = armedFirst; page <= armedLast && page < kPages; page++)
            if (page < s.first || page > s.last)
            {
                uint64_t p[5];
                Page(page, p);
                Add(seen, "  page %u: %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64 " %" PRIu64 "\n", page, p[0], p[1],
                    p[2], p[3], p[4]);
            }
        // The shadow's copy of what the step may have armed.
        if (armedFirst <= armedLast)
        {
            uint64_t hash = 1469598103934665603ull;
            for (uint32_t page = armedFirst; page <= armedLast && page < kPages; page++)
                hash = Fnv(r_shadow[r_side] + size_t(page) * 4096, 4096, hash);
            Add(seen, "  shadow %u..%u %016" PRIx64 "\n", armedFirst, armedLast, hash);
        }
        Add(seen, "  faults %" PRIu64 " protects %" PRIu64 " enabled %d loosened outside %u\n", costs[0], costs[1],
            Enabled(), Loosened(kPages));
        g_compared += 40;
        return seen;
    }

    // The Rust's report has a line the C++'s has not, D15's count, when it is not 0: what
    // each report should have said, and the lines taken out of the Rust's text and held
    // to it.
    std::vector<uint64_t> r_looseExpected;
    void NoteReport(int side)
    {
        if (side != 1) return;
        uint64_t counters[16];
        Counters(counters);
        if (counters[3] && counters[15]) r_looseExpected.push_back(counters[15]);
    }
    void TakeLooseLines(std::string& log)
    {
        const std::string mark = " loose faults: writes to a page already written since its copy, made writable again\n";
        std::vector<uint64_t> found;
        std::string kept;
        size_t at = 0;
        while (at < log.size())
        {
            size_t end = log.find('\n', at);
            end = end == std::string::npos ? log.size() : end + 1;
            const std::string line = log.substr(at, end - at);
            const size_t m = line.find(mark);
            if (line.rfind("[i] gpu: ", 0) == 0 && m != std::string::npos && m + mark.size() == line.size())
                found.push_back(std::strtoull(line.c_str() + 9, nullptr, 10));
            else
                kept += line;
            at = end;
        }
        if (found != r_looseExpected) kept += "(the Rust's loose-fault lines are not its count)\n";
        r_looseExpected.clear();
        log = kept;
    }

    std::vector<std::string> RunScript(const Script& s, uint64_t seed, int side, std::string& log)
    {
        Clean(side);
        r_teardown = false;
        r_rng = seed;
        std::vector<std::string> seen;
        uint64_t recording = 1 + Below(100), frame = Below(4) ? 2000 + Below(100) : Below(10);
        uint64_t lastStamp = 0;
        r_completed = recording - 1;
        rig_log::Take(side);
        r_logging = true;
        char step[256];
        uint64_t looseBefore = 0;
        const auto observe = [&](uint64_t answer, uint32_t first, uint32_t last) {
            uint64_t counters[16];
            seen.push_back(Observe(s, step, answer, first, last, looseBefore));
            Counters(counters);
            looseBefore = counters[15];
        };
        if (Below(8))
        {
            SetShadow(r_shadow[side], uint32_t(kBytes));
            std::snprintf(step, sizeof step, "set shadow");
            observe(0, 1, 0);
        }
        const uint32_t steps = 1 + Below(48);
        for (uint32_t i = 0; i < steps; i++)
        {
            const uint32_t kind = Below(100);
            // The GPU's progress and the frames, as the consumer sees them.
            if (Below(4) == 0) frame += Below(16) ? Below(3) : (Below(2) ? 1024 + Below(64) : uint64_t(1) << Below(12));
            if (Below(3) == 0) recording += 1 + Below(2);
            switch (Below(6))
            {
            case 0: r_completed = recording - Below(4); break;
            case 1: r_completed = 0; break;
            case 2: r_completed = recording + Below(3); break;
            default: break;
            }
            if (kind < 25)
            {
                const uint32_t physical = Address(s), bytes = Size(physical);
                uint32_t first, last;
                Range(physical, bytes, first, last);
                if (last > first + 70) last = first + 70;
                std::snprintf(step, sizeof step, "serve %08X %u rec %" PRIu64 " done %" PRIu64 " frame %" PRIu64, physical,
                              bytes, recording, r_completed, frame);
                observe(Serve(physical, bytes, recording, frame), first, last);
            }
            else if (kind < 40)
            {
                const uint32_t physical = Address(s), bytes = Size(physical);
                uint32_t first, last;
                Range(physical, bytes, first, last);
                if (last > first + 70) last = first + 70;
                std::snprintf(step, sizeof step, "track %08X %u done %" PRIu64 " frame %" PRIu64, physical, bytes,
                              r_completed, frame);
                const uint64_t stamp = Track(physical, bytes, frame);
                if (stamp) lastStamp = stamp;
                observe(stamp, first, last);
            }
            else if (kind < 50)
            {
                const uint32_t physical = Address(s), bytes = Size(physical);
                uint64_t since;
                switch (Below(5))
                {
                case 0: since = 0; break;
                case 1: since = lastStamp ? lastStamp - 1 : 0; break;
                case 2: since = Next() % 64; break;
                default: since = lastStamp; break;
                }
                std::snprintf(step, sizeof step, "unwritten %08X %u since %" PRIu64, physical, bytes, since);
                observe(Unwritten(physical, bytes, since), 1, 0);
            }
            else if (kind < 65)
            {
                const int window = Below(2);
                const uint32_t physical = Page(s) * 4096 + Below(1024) * 4;
                const uint32_t value = uint32_t(Next());
                std::snprintf(step, sizeof step, "write %c %08X = %08X", window ? 'C' : 'A', physical, value);
                const uint64_t refusedBefore = r_refused.load(), livelocksBefore = r_livelocks.load();
                const uint64_t faults = Write(window, physical, value);
                const uint32_t read = *reinterpret_cast<volatile uint32_t*>(r_base + guest::kPhysicalBase + physical);
                char more[96];
                std::snprintf(more, sizeof more, " faults %" PRIu64 " refused %" PRIu64 " livelocks %" PRIu64 " reads %08X",
                              faults, r_refused.load() - refusedBefore, r_livelocks.load() - livelocksBefore, read);
                std::strncat(step, more, sizeof step - std::strlen(step) - 1);
                observe(0, 1, 0);
            }
            else if (kind < 73)
            {
                uint32_t address;
                switch (Below(8))
                {
                case 0: address = 0x80000000u + Below(0x1000); break;
                case 1: address = 0xE0000000u + Below(0x1000); break;
                case 2: address = 0xC0000000u - 4 - Below(8); break;
                default: address = (Below(2) ? guest::kApertureBase : guest::kPhysicalBase) + Address(s); break;
                }
                const uint32_t bytes = Size(address & 0x1FFFFFFF);
                std::snprintf(step, sizeof step, "will write %08X %u", address, bytes);
                WillWrite(address, bytes);
                observe(0, 1, 0);
            }
            else if (kind < 78)
            {
                // The handler asked about any address: the windows, the space around them,
                // and outside the guest altogether.
                const uint8_t* at;
                const char* what;
                switch (Below(8))
                {
                case 0: at = r_base - 1 - Below(64); what = "below the space"; break;
                case 1: at = r_base + (1ull << 32) + Below(64); what = "above the space"; break;
                case 2: at = r_base + 0x82000000u + Below(4096); what = "outside the windows"; break;
                default:
                    at = r_base + (Below(2) ? guest::kApertureBase : guest::kPhysicalBase) + Page(s) * 4096 + Below(4096);
                    what = "in a window";
                    break;
                }
                std::snprintf(step, sizeof step, "handle fault %s at %" PRIx64, what, uint64_t(at - r_base));
                observe(HandleFault(at), 1, 0);
            }
            else if (kind < 80)
            {
                const uint32_t which = Below(3);
                std::snprintf(step, sizeof step, "set shadow %s", which == 0 ? "null" : which == 1 ? "too small" : "whole");
                SetShadow(which == 0 ? nullptr : r_shadow[side], which == 1 ? uint32_t(kBytes) - 4096 : uint32_t(kBytes));
                observe(0, 1, 0);
            }
            else if (kind < 83)
            {
                std::snprintf(step, sizeof step, "report");
                Report();
                NoteReport(side);
                observe(0, 1, 0);
            }
            else if (kind < 85)
            {
                const uint32_t page = Below(4) ? Page(s) : kPages + Below(3);
                std::snprintf(step, sizeof step, "loosened %u", page);
                observe(Loosened(page), 1, 0);
            }
            else if (kind < 86)
            {
                gpu::stream::detail::counting = !gpu::stream::detail::counting;
                std::snprintf(step, sizeof step, "capture counting %d", gpu::stream::detail::counting);
                observe(0, 1, 0);
            }
            else if (kind < 87)
            {
                vk::trace::g_wanted = !vk::trace::g_wanted;
                std::snprintf(step, sizeof step, "trace %d", vk::trace::g_wanted);
                observe(0, 1, 0);
            }
            else if (kind < 92)
            {
                // A page of the title's dynamic-geometry ring: written soon after each copy,
                // copied again as soon as the back-off allows, lap after lap, so that the
                // strikes climb to their cap.
                const uint32_t page = Page(s);
                const uint32_t laps = 4 + Below(12);
                std::snprintf(step, sizeof step, "laps %u of page %u from frame %" PRIu64, laps, page, frame);
                uint64_t answers = 0;
                r_completed = recording;
                for (uint32_t lap = 0; lap < laps; lap++)
                {
                    Write(Below(2), page * 4096 + Below(1024) * 4, uint32_t(Next()));
                    frame += (uint64_t(1) << std::min<uint32_t>(lap, 10)) + Below(2);
                    answers = answers * 2 + Serve(page * 4096, 4096, recording, frame);
                }
                observe(answers, page, page);
            }
            else if (kind < 94)
            {
                r_failNext = true;
                std::snprintf(step, sizeof step, "fail the next read-only protect");
                observe(0, 1, 0);
            }
        }
        r_failNext = false;
        // The end: the report, and the memory the case is about.
        Report();
        NoteReport(side);
        uint64_t hash = 1469598103934665603ull;
        for (uint32_t page = s.first; page <= s.last; page++)
            hash = Fnv(r_base + guest::kPhysicalBase + size_t(page) * 4096, 4096, hash);
        std::snprintf(step, sizeof step, "end memory %016" PRIx64, hash);
        observe(0, 1, 0);
        r_logging = false;
        rig_log::Stop();
        log = rig_log::g_log[side];
        rig_log::g_log[side].clear();
        if (side == 1) TakeLooseLines(log);
        return seen;
    }

    void Scripts(uint64_t seed, uint32_t cases)
    {
        r_rng = seed;
        constexpr uint32_t kBases[] = { 0, 2, 4000, 65530, kPages - 24, kPages - 30 };
        for (uint32_t i = 0; i < cases; i++)
        {
            Script s;
            s.base = Below(4) ? kBases[Below(std::size(kBases))] : Below(kPages - 24);
            s.first = s.base >= 2 ? s.base - 2 : 0;
            s.last = std::min<uint32_t>(s.base + 26, kPages - 1);
            const uint64_t caseSeed = Next();
            std::string log[2];
            const std::vector<std::string> cpp = RunScript(s, caseSeed, 0, log[0]);
            const std::vector<std::string> rust = RunScript(s, caseSeed, 1, log[1]);
            r_rng = caseSeed ^ 0x5DEECE66Dull;
            size_t at = 0;
            while (at < cpp.size() && at < rust.size() && cpp[at] == rust[at]) at++;
            if (at < cpp.size() || at < rust.size() || log[0] != log[1])
            {
                std::printf("DIFFERENCE: watch case %u (base page %u)", i, s.base);
                if (at < cpp.size() || at < rust.size())
                {
                    std::printf(", step %zu\n", at);
                    for (size_t k = at >= 3 ? at - 3 : 0; k < at; k++) std::printf("  %s", cpp[k].c_str());
                    std::printf("C++:\n  %s\nRust:\n  %s\n", at < cpp.size() ? cpp[at].c_str() : "(nothing)\n",
                                at < rust.size() ? rust[at].c_str() : "(nothing)\n");
                }
                else
                    std::printf(", the lines printed:\nC++:\n%sRust:\n%s", log[0].c_str(), log[1].c_str());
                std::exit(1);
            }
            g_compared += cpp.size() + log[0].size() / 32;
        }
        std::printf("watch: %u cases the same\n", cases);
    }

    // ---- stress ----------------------------------------------------------------------------
    struct StressResult { uint64_t livelocks, faults, injected, checks, refused, arms; };

    StressResult Stress(int side, uint64_t seed, uint64_t iterations)
    {
        Clean(side);
        r_rng = seed;
        SetShadow(r_shadow[side], uint32_t(kBytes));
        constexpr uint32_t kFirst = 4096, kCount = 16;
        r_injectProbability = 1024;
        const uint64_t livelocksBefore = r_livelocks.load(), refusedBefore = r_refused.load(), injectedBefore = r_injected.load();
        std::atomic<bool> stop{ false }, pause{ false };
        std::atomic<int> paused{ 0 };
        std::vector<std::thread> writers;
        for (int w = 0; w < 2; w++)
            writers.emplace_back([&, w] {
                uint64_t state = seed * 31 + w;
                const auto next = [&] { state = state * 6364136223846793005ull + 1442695040888963407ull; return uint32_t(state >> 33); };
                while (!stop.load(std::memory_order_relaxed))
                {
                    if (pause.load(std::memory_order_acquire))
                    {
                        paused.fetch_add(1);
                        while (pause.load(std::memory_order_acquire)) {}
                        paused.fetch_sub(1);
                        continue;
                    }
                    const uint32_t page = kFirst + next() % kCount;
                    if (next() % 64 == 0)
                        WillWrite((next() & 1 ? guest::kApertureBase : guest::kPhysicalBase) + page * 4096, 4096 * (1 + next() % 3));
                    else
                        Write(next() & 1, page * 4096 + (next() % 1024) * 4, next());
                }
            });
        uint64_t recording = 1, frame = 0, checks = 0;
        for (uint64_t i = 0; i < iterations; i++)
        {
            if (i % 8 == 0) frame++;
            if (i % 4 == 0) recording++;
            r_completed = recording - 1;
            const uint32_t first = kFirst + Below(kCount), pages = 1 + Below(kFirst + kCount - first);
            if (Below(2)) Serve(first * 4096, pages * 4096 - Below(8), recording, frame);
            else Track(first * 4096 + Below(64), pages * 4096 - 64, frame);
            if (i % 65536 == 65535 || i + 1 == iterations)
            {
                pause.store(true, std::memory_order_release);
                while (paused.load() != 2) {}
                for (uint32_t page = kFirst; page < kFirst + kCount; page++)
                    if (StateOf(page) == kWatched &&
                        std::memcmp(r_shadow[side] + size_t(page) * 4096, r_base + guest::kPhysicalBase + size_t(page) * 4096, 4096))
                    {
                        std::printf("DIFFERENCE: stress, the %s side: page %u reads Watched and its copy is not memory "
                                    "(a write was missed), iteration %" PRIu64 "\n", side ? "Rust" : "C++", page, i);
                        std::exit(1);
                    }
                checks++;
                pause.store(false, std::memory_order_release);
            }
        }
        stop = true;
        for (auto& writer : writers) writer.join();
        r_injectProbability = 0;
        uint64_t costs[3], counters[16];
        Costs(costs);
        Counters(counters);
        return { r_livelocks.load() - livelocksBefore, costs[0], r_injected.load() - injectedBefore, checks,
                 r_refused.load() - refusedBefore, counters[3] };
    }

    // ---- the livelock itself ---------------------------------------------------------------
    // A page armed, written (Loose), armed again with a WillWrite between the store of
    // Arming and the protect, then written from another thread. The faults that write took
    // and whether the rig had to step in.
    std::pair<uint64_t, uint64_t> Livelock(int side)
    {
        Clean(side);
        SetShadow(r_shadow[side], uint32_t(kBytes));
        constexpr uint32_t kPage = 777;
        r_completed = 0;
        if (!Serve(kPage * 4096, 4096, 1, 1)) { std::printf("DIFFERENCE: livelock: the first serve failed\n"); std::exit(1); }
        Write(0, kPage * 4096, 1);
        r_injectAddress = guest::kPhysicalBase + kPage * 4096, r_injectBytes = 4096;
        r_inject = true;
        r_completed = 1;   // the first serve's submission is done: the copy may be replaced
        Serve(kPage * 4096, 4096, 2, 5000);   // armed again, past any back-off; comes out Loose
        const uint64_t livelocksBefore = r_livelocks.load();
        uint64_t faults = 0;
        std::thread writer([&] { faults = Write(1, kPage * 4096 + 8, 2); });
        writer.join();
        return { faults, r_livelocks.load() - livelocksBefore };
    }

    // ---- the same page at the end ----------------------------------------------------------
    // The livelock's page, Loose and read-only, when the watch is turned off (the renderer's
    // shutdown), then written: the handler no longer answers. The faults the write took and
    // those the rig had to let through.
    std::pair<uint64_t, uint64_t> Teardown(int side)
    {
        Clean(side);
        SetShadow(r_shadow[side], uint32_t(kBytes));
        constexpr uint32_t kPage = 777;
        r_completed = 0;
        if (!Serve(kPage * 4096, 4096, 1, 1)) { std::printf("DIFFERENCE: teardown: the first serve failed\n"); std::exit(1); }
        Write(0, kPage * 4096, 1);
        r_injectAddress = guest::kPhysicalBase + kPage * 4096, r_injectBytes = 4096;
        r_inject = true;
        r_completed = 1;
        Serve(kPage * 4096, 4096, 2, 5000);   // comes out Loose and read-only
        if (StateOf(kPage) != kLoose) { std::printf("DIFFERENCE: teardown: the page is not Loose\n"); std::exit(1); }
        SetShadow(nullptr, 0);
        const uint64_t refusedBefore = r_refused.load();
        uint64_t faults = 0;
        std::thread writer([&] { faults = Write(1, kPage * 4096 + 8, 2); });
        writer.join();
        return { faults, r_refused.load() - refusedBefore };
    }
}

int main(int argc, char** argv)
{
    if (argc < 4)
    {
        std::fprintf(stderr, "usage: watch <seed> <cases> watch|stress|livelock...\n");
        return 2;
    }
    const uint64_t seed = std::strtoull(argv[1], nullptr, 10);
    const uint64_t cases = std::strtoull(argv[2], nullptr, 10);
    // The protections are counted under MW2_REPORT, read once by each side: on for odd seeds.
    if (seed % 2) setenv("MW2_REPORT", "1", 1);
    Map();
    for (int i = 3; i < argc; i++)
    {
        const std::string what = argv[i];
        if (what == "watch") Scripts(seed * 0x2545F4914F6CDD1Dull + 1, uint32_t(cases));
        else if (what == "stress")
        {
            for (int side = 0; side < 2; side++)
            {
                const StressResult r = Stress(side, seed, cases);
                std::printf("stress %s: %" PRIu64 " iterations, %" PRIu64 " arms, %" PRIu64 " faults, %" PRIu64
                            " WillWrites forced between Arming and the protect, %" PRIu64 " checks, %" PRIu64
                            " livelocks, %" PRIu64 " faults refused\n",
                            side ? "Rust" : "C++", cases, r.arms, r.faults, r.injected, r.checks, r.livelocks, r.refused);
                if (side == 1 && (r.livelocks || r.refused))
                {
                    std::printf("DIFFERENCE: stress: the Rust livelocked or refused a fault\n");
                    return 1;
                }
            }
        }
        else if (what == "livelock")
        {
            const auto [cppFaults, cppLivelocks] = Livelock(0);
            const auto [rustFaults, rustLivelocks] = Livelock(1);
            uint64_t counters[16];
            Counters(counters);
            std::printf("livelock: the C++ write took %" PRIu64 " faults and %s; the Rust's %" PRIu64
                        " faults, %" PRIu64 " loose faults, %s\n",
                        cppFaults, cppLivelocks ? "livelocked (the rig made the page writable)" : "went through",
                        rustFaults, counters[15], rustLivelocks ? "livelocked" : "went through");
            if (!cppLivelocks)
            {
                std::printf("DIFFERENCE: livelock: the forced interleaving did not livelock the C++: the test proves nothing\n");
                return 1;
            }
            if (rustLivelocks || rustFaults > 2 || counters[15] != 1)
            {
                std::printf("DIFFERENCE: livelock: the Rust livelocked, or did not count its loose fault\n");
                return 1;
            }
        }
        else if (what == "teardown")
        {
            const auto [cppFaults, cppRefused] = Teardown(0);
            const auto [rustFaults, rustRefused] = Teardown(1);
            std::printf("teardown: after the watch was turned off the C++ write took %" PRIu64 " faults, %" PRIu64
                        " let through by the rig; the Rust's %" PRIu64 " faults, %" PRIu64 " let through\n",
                        cppFaults, cppRefused, rustFaults, rustRefused);
            if (!cppRefused)
            {
                std::printf("DIFFERENCE: teardown: the C++ page was writable: the test proves nothing\n");
                return 1;
            }
            if (rustFaults || rustRefused)
            {
                std::printf("DIFFERENCE: teardown: the Rust left a Loose page read-only when the watch was turned off\n");
                return 1;
            }
        }
        else { std::fprintf(stderr, "unknown part %s\n", argv[i]); return 2; }
    }
    std::printf("%" PRIu64 " fields compared\n", g_compared);
    return 0;
}
