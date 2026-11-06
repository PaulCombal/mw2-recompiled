// tools/gpu_diff/pm4.cpp: the command processor, C++ (runtime/gpu/command_processor.cpp,
// census.cpp, arena.cpp and runtime/image_move.cpp as they are) against the Rust
// (src/runtime/gpu/command_processor.rs, census.rs, arena.rs, image_move.rs), one side a
// process: `pm4 <seed> <cases> cpp|rust <part>...` prints a line per case, and
// tools/gpu_diff.py runs both sides and compares the two outputs; `--show N` prints case
// N's whole log instead, for the first case that differs.
//
// What each side calls of the renderer, the memory watch, the GPU capture, the GPU's
// threads and the guest's clock is the same logger here, whichever side calls it: the
// C++ through its own declarations (renderer.h, memory_watch.h, stream_capture.h,
// internal.h), the Rust through runtime/rust_calls.cpp's names (bridge.rs) and the
// rig_* seams of command_processor.rs. Its answers -- an occlusion count now or later,
// the watch's stamps and verdicts, a wait's journal entry, Running() for a number of
// polls -- come from a generator seeded per case, so both sides get the same ones as
// long as they ask the same questions in the same order.
//
// The log of a case: per draw, the DrawCall (the code's words hashed, not its address)
// and a hash of the register file's values, stamps and writes; each resolve, swap,
// BeforeCompletion, occlusion begin, end and delivery, Idle, interrupt, watch call,
// capture note and image-pool call; every line either side printed (stderr points at
// the log); after each batch, every page of guest memory written in it (the 4 GB are
// read-only and a write faults once a page) with its bytes hashed; after each case the
// capture's state.bin bytes, the display table, the swap count and the moves around a
// range. At the end of a seed: the reports (cp, census, image pool, arena), with the
// timings in them masked.
//
// Parts:
//   pm4         generated streams: every packet type and opcode, malformed packets
//               (past the end, zero padding, 0xFFFFFFFF, unknown opcodes, each handler's
//               short count), predication, indirect buffers to depth 5, misaligned,
//               oversized and self-referencing ones (to the batch limit), ring runs that
//               wrap, constant runs past 0x5003, scratch write-back, waits satisfied at
//               once, later or never, occlusion answered at once and later, image-pool
//               copies at their draw and at the frame's end, the capture's and the
//               replay's hooks
//   image_pool  an event script on the pool alone: moves queued, draws with stream
//               constants and shaders, frame ends, the moves described
//   capture:<file>  a capture of the game's GPU input (tools/gpu_diff.py --capture
//               converts one): its snapshot, state and batches, each batch's pages and
//               events applied at their packet as the replay applies them
#include <ppc_recomp_shared.h>
#include "gpu/gpu.h"
#include "gpu/internal.h"
#include "gpu/memory_watch.h"
#include "gpu/registers.h"
#include "gpu/stream_capture.h"
#include "gpu/vulkan/renderer.h"
#include "gpu/vulkan/texture_cache.h"
#include "image_move.h"
#include "rig_log.h"

#include <algorithm>
#include <cinttypes>
#include <csignal>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <deque>
#include <set>
#include <string>
#include <fcntl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>
#include <cstdarg>
#include <vector>

extern "C"
{
    extern uint8_t* mw2_guest_base;
    extern uint32_t mw2_kernel_physical_top;
    // The C++'s own, renamed so the Rust's can stand beside it (gpu_diff.py).
    bool cpp_mw2_cp_restore_for_replay(const uint8_t* bytes, uint64_t size);
    // The Rust's (exports.rs).
    bool mw2_cp_restore_for_replay(const uint8_t* bytes, uint64_t size);
    bool mw2_cp_display_colour_table(uint32_t* out);
    uint64_t mw2_cp_swaps_parsed();
    void mw2_cp_save(uint8_t* (*reserve)(void*, size_t), void* context);
    void mw2_imagepool_describe_moves_around(uint32_t physical, uint32_t size, uint64_t frames, char* out, size_t bytes);
    void mw2_imagepool_queue_for_replay(uint32_t destination, uint32_t source, uint32_t size, uint32_t streamConstant,
                                        uint64_t frame);
    void mw2_arena_set_block(uint32_t physical);
    void mw2_rig_arena_report();
    void mw2_rig_set_gpu_values(uint64_t frames, bool consuming, double seconds);
    void mw2_rig_cp_execute_ring(uint32_t base, uint32_t readIndex, uint32_t mask, uint32_t dwords);
    void mw2_rig_cp_deliver_occlusion_queries();
    void mw2_rig_cp_write_physical(uint32_t physical, uint32_t value);
    void mw2_rig_cp_report();
    void mw2_rig_census_present();
    void mw2_rig_census_report();
    void mw2_rig_pool_report();
    bool mw2_rig_pool_copies_waiting();
    void mw2_rig_pool_at_draw(uint32_t streamConstant, uint64_t vertexShader);
    void mw2_rig_pool_at_frame_end(uint64_t parsedFrames);
}

namespace
{
    constexpr uint32_t kPhysicalBase = 0xA0000000u;
    constexpr uint32_t kPhysicalBytes = 64u << 20;
    constexpr uint64_t kMapped = (1ull << 32) + 0x10000;
    constexpr uint32_t kPage = 4096;

    // The rig's layout, physical.
    constexpr uint32_t kRing = 0x00010000u;        // up to 16k dwords
    constexpr uint32_t kBuffers = 0x00100000u;     // indirect buffers, bump-allocated per batch
    constexpr uint32_t kBuffersEnd = 0x00300000u;
    constexpr uint32_t kShaders = 0x00300000u;     // shaders for IM_LOAD, at fixed places
    constexpr uint32_t kConstants = 0x00340000u;   // LOAD_ALU_CONSTANT's blocks
    constexpr uint32_t kData = 0x00380000u;        // writes, waits, occlusion records, the arena block
    constexpr uint32_t kPool = 0x00400000u;        // the image pool's blocks
    constexpr uint32_t kPoolEnd = 0x00800000u;
    constexpr uint32_t kAperture = 0x7FC80000u;

    bool g_rust = false;
    uint64_t g_compared = 0;

    struct Random
    {
        uint64_t state;
        uint64_t Next()
        {
            uint64_t z = (state += 0x9E3779B97F4A7C15ull);
            z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ull;
            z = (z ^ (z >> 27)) * 0x94D049BB133111EBull;
            return z ^ (z >> 31);
        }
        uint32_t Below(uint32_t n) { return n ? uint32_t(Next() % n) : 0; }
        bool One(uint32_t n) { return Below(n) == 0; }
    };
    Random g_random;   // the streams
    Random g_answers;  // the stubs' answers, reseeded per case

    uint64_t Mix(uint64_t h, uint64_t v)
    {
        h ^= v + 0x9E3779B97F4A7C15ull + (h << 6) + (h >> 2);
        return h * 0xFF51AFD7ED558CCDull;
    }
    uint64_t HashWords(const void* p, size_t bytes)
    {
        uint64_t h = 0xCBF29CE484222325ull;
        const uint8_t* b = static_cast<const uint8_t*>(p);
        size_t i = 0;
        for (; i + 8 <= bytes; i += 8)
        {
            uint64_t v;
            std::memcpy(&v, b + i, 8);
            h = Mix(h, v);
        }
        for (; i < bytes; i++) h = Mix(h, b[i]);
        return h;
    }

    // ---- the log ----------------------------------------------------------------------
    void Log(const char* format, ...) __attribute__((format(printf, 1, 2)));
    void Log(const char* format, ...)
    {
        va_list list;
        va_start(list, format);
        std::vfprintf(stderr, format, list);
        va_end(list);
        std::fputc('\n', stderr);
    }

    // ---- guest memory: read-only, a write faults once a page ----------------------------
    uint8_t* g_base;
    std::vector<uint32_t> g_dirty;   // guest pages written since the last look

    void OnFault(int, siginfo_t* info, void*)
    {
        const uintptr_t at = reinterpret_cast<uintptr_t>(info->si_addr);
        const uintptr_t base = reinterpret_cast<uintptr_t>(g_base);
        if (at < base || at >= base + kMapped) std::abort();
        const uint32_t page = uint32_t((at - base) / kPage);
        mprotect(g_base + uint64_t(page) * kPage, kPage, PROT_READ | PROT_WRITE);
        g_dirty.push_back(page);
    }

    // The pages written since the last call, sorted, each with its bytes' hash; then
    // read-only again.
    void LogDirty(const char* when)
    {
        std::sort(g_dirty.begin(), g_dirty.end());
        g_dirty.erase(std::unique(g_dirty.begin(), g_dirty.end()), g_dirty.end());
        for (uint32_t page : g_dirty)
        {
            Log("%s page %05X %016" PRIx64, when, page, HashWords(g_base + uint64_t(page) * kPage, kPage));
            mprotect(g_base + uint64_t(page) * kPage, kPage, PROT_READ);
        }
        g_compared += g_dirty.size();
        g_dirty.clear();
    }

    // The rig's own writes: made writable, written, and left out of the log.
    void Quiet()
    {
        for (uint32_t page : g_dirty) mprotect(g_base + uint64_t(page) * kPage, kPage, PROT_READ);
        g_dirty.clear();
    }
    void Put32(uint32_t va, uint32_t value)   // big-endian, as the guest holds it
    {
        const uint32_t be = __builtin_bswap32(value);
        std::memcpy(g_base + va, &be, 4);
    }
    void PutRaw32(uint32_t va, uint32_t value) { std::memcpy(g_base + va, &value, 4); }
    uint32_t Va(uint32_t physical) { return kPhysicalBase + physical; }

    // ---- the GPU's values and the stubs' scripts ------------------------------------------
    uint64_t g_frames = 0;
    bool g_consuming = false;
    double g_seconds = 0;
    uint32_t g_runningPolls = 0;      // Running() answers true this many more times
    uint32_t g_satisfyAtPoll = 0;     // the Idle call that writes the waited-for value (0: none)
    uint32_t g_satisfyAddress = 0, g_satisfyValue = 0;
    uint32_t g_idles = 0;
    uint64_t g_timebase = 0;
    bool g_watchOn = false;
    uint64_t g_stamp = 0;
    std::deque<std::pair<uint32_t, uint64_t>> g_pendingQueries;   // tags answered later
    std::deque<gpu::stream::Wait> g_journal;                     // the replay's waits
    // capture: the batch's pages and events, applied at their packet.
    struct Applied { uint32_t packet, page; const uint8_t* bytes; };
    struct Event { uint32_t kind, packet, a, b, c, d; uint64_t e; };
    std::vector<Applied> g_pages;
    std::vector<Event> g_events;
    size_t g_nextPage = 0, g_nextEvent = 0;
    uint32_t g_packet = 0;

    void SetValues()
    {
        mw2_rig_set_gpu_values(g_frames, g_consuming, g_seconds);
    }

    void QueueForReplay(uint32_t d, uint32_t s, uint32_t n, uint32_t c, uint64_t f);
    void SetArenaBlock(uint32_t physical);

    // What both sides call, logged the same.
    void LogDraw(const gpu::RegisterFile& r, const vk::renderer::DrawCall& c)
    {
        const uint64_t vertex = c.vertexCode ? HashWords(c.vertexCode, c.vertexWords * 4) : 0;
        const uint64_t pixel = c.pixelCode ? HashWords(c.pixelCode, c.pixelWords * 4) : 0;
        Log("draw %u %u %d %d %08X v %d %zu %016" PRIx64 " %016" PRIx64 " p %d %zu %016" PRIx64 " %016" PRIx64
            " r %016" PRIx64 " %016" PRIx64 " %" PRIu64,
            c.primitive, c.indexCount, c.indexed, c.index32, c.indexAddress, c.vertexCode != nullptr, c.vertexWords,
            c.vertexHash, vertex, c.pixelCode != nullptr, c.pixelWords, c.pixelHash, pixel,
            HashWords(r.values, sizeof r.values), HashWords(r.stamps, sizeof r.stamps), r.writes);
        g_compared += 16;
    }
    void LogResolve(const gpu::RegisterFile& r)
    {
        Log("resolve %016" PRIx64 " %016" PRIx64 " %" PRIu64, HashWords(r.values, sizeof r.values),
            HashWords(r.stamps, sizeof r.stamps), r.writes);
    }
    bool EndQuery(uint32_t tag, uint64_t& samples)
    {
        const uint32_t how = g_answers.Below(4);
        const uint64_t count = how == 0 ? 0 : how == 1 ? g_answers.Next() : g_answers.Below(1000);
        const bool now = g_answers.Below(2);
        Log("occlusion end %08X %s %" PRIu64, tag, now ? "now" : "later", count);
        if (now) samples = count;
        else g_pendingQueries.push_back({ tag, count });
        return now;
    }
    void Collect(void (*deliver)(uint32_t, uint64_t))
    {
        Log("occlusion collect %zu", g_pendingQueries.size());
        uint32_t n = g_answers.Below(3);
        while (n-- && !g_pendingQueries.empty())
        {
            const auto [tag, count] = g_pendingQueries.front();
            g_pendingQueries.pop_front();
            Log("occlusion delivered %08X %" PRIu64, tag, count);
            deliver(tag, count);
        }
    }
    void Idle()
    {
        g_idles++;
        Log("idle %u", g_idles);
        if (g_satisfyAtPoll && g_idles == g_satisfyAtPoll)
        {
            PutRaw32(g_satisfyAddress, g_satisfyValue);
            Quiet();
        }
    }
    uint64_t Track(uint32_t physical, uint32_t bytes, uint64_t frame)
    {
        const uint64_t stamp = g_answers.Below(4) ? ++g_stamp : 0;
        Log("watch track %08X %u %" PRIu64 " -> %" PRIu64, physical, bytes, frame, stamp);
        return stamp;
    }
    bool Unwritten(uint32_t physical, uint32_t bytes, uint64_t since)
    {
        const bool unwritten = g_answers.Below(3) != 0;
        Log("watch unwritten %08X %u %" PRIu64 " -> %d", physical, bytes, since, unwritten);
        return unwritten;
    }

    // capture: what the replay applies at a packet (stream_capture.cpp's Apply).
    void Apply(uint32_t packet)
    {
        while (g_nextPage < g_pages.size() && g_pages[g_nextPage].packet <= packet)
        {
            const Applied& a = g_pages[g_nextPage++];
            if (a.packet == 0) continue;
            std::memcpy(g_base + uint64_t(a.page) * kPage, a.bytes, kPage);
            Quiet();
        }
        while (g_nextEvent < g_events.size() && g_events[g_nextEvent].packet <= packet)
        {
            const Event& e = g_events[g_nextEvent++];
            switch (e.kind)
            {
            case kMw2ReplayPool: QueueForReplay(e.a, e.b, e.c, e.d, e.e); break;
            case kMw2ReplayWait: g_journal.push_back({ e.a != 0, e.c, e.d, e.e != 0 }); break;
            case kMw2ReplayAperture: Put32(kAperture + e.a * 4, e.b); Quiet(); break;
            case kMw2ReplayArena: SetArenaBlock(e.a); break;
            }
        }
    }
    void Packet()
    {
        g_packet++;
        Log("packet %u", g_packet);
        if (gpu::stream::detail::replaying) Apply(g_packet);
    }
    bool RigReplayWait(gpu::stream::Wait& wait)
    {
        if (g_journal.empty())
        {
            Log("replay wait none");
            return false;
        }
        wait = g_journal.front();
        g_journal.pop_front();
        Log("replay wait %d %08X %u %d", wait.memory, wait.value, wait.polls, wait.timedOut);
        return true;
    }
}

// ---- the C++ side's stand-ins ------------------------------------------------------------
namespace vk::renderer
{
    void Draw(const gpu::RegisterFile& registers, const DrawCall& call) { LogDraw(registers, call); }
    void Resolve(const gpu::RegisterFile& registers) { LogResolve(registers); }
    void Swap(uint32_t frontBuffer) { Log("swap %08X", frontBuffer); }
    void BeforeCompletion() { Log("before completion"); }
    void BeginOcclusionQuery() { Log("occlusion begin"); }
    bool EndOcclusionQuery(uint32_t tag, uint64_t& samples) { return EndQuery(tag, samples); }
    void CollectOcclusionQueries(void (*deliver)(uint32_t, uint64_t)) { Collect(deliver); }
    void Idle() { ::Idle(); }
    uint64_t CompletedSubmission() { return 0; }
    void BeforeStreamWrite(uint32_t physical, uint32_t size) { Log("before stream write %08X %u", physical, size); }
}
namespace vk::textures
{
    void MemoryWritten(uint32_t physical, uint32_t size) { Log("memory written %08X %u", physical, size); }
}
namespace gpu
{
    uint64_t FrameCount() { return g_frames; }
    bool Consuming() { return g_consuming; }
}
namespace gpu::watch
{
    bool Enabled() { return g_watchOn; }
    uint64_t Track(uint32_t physical, uint32_t bytes, uint64_t (*)(), uint64_t frame) { return ::Track(physical, bytes, frame); }
    bool Unwritten(uint32_t physical, uint32_t bytes, uint64_t since) { return ::Unwritten(physical, bytes, since); }
    void WillWrite(uint32_t address, uint32_t bytes) { Log("watch will write %08X %u", address, bytes); }
}
namespace gpu::detail
{
    bool Running()
    {
        const bool running = g_runningPolls != 0;
        if (running) g_runningPolls--;
        return running;
    }
    void RaiseCommandProcessorInterrupt(uint32_t cpuMask) { Log("interrupt %08X", cpuMask); }
    double Seconds() { return g_seconds; }
}
namespace gpu::stream
{
    namespace detail
    {
        bool capturing = false, replaying = false, counting = false;
    }
    void PacketSlow() { ::Packet(); }
    void ReadSlow(uint32_t address, uint64_t bytes) { Log("capture read %08X %" PRIu64, address, bytes); }
    void ReadPhysicalSlow(uint32_t physical, uint64_t bytes) { Log("capture read physical %08X %" PRIu64, physical, bytes); }
    void PolledSlow(uint32_t address) { Log("capture polled %08X", address); }
    void WriteSlow(uint32_t address, uint32_t bytes, bool done) { Log("capture write %08X %u %d", address, bytes, done); }
    void ApertureRead(uint32_t index, uint32_t value) { Log("capture aperture %04X %08X", index, value); }
    void ArenaBlockSeen(uint32_t block) { Log("capture arena block %08X", block); }
    void WaitDone(bool memory, uint32_t address, uint32_t value, uint32_t polls, bool timedOut)
    {
        Log("capture wait %d %08X %08X %u %d", memory, address, value, polls, timedOut);
    }
    bool ReplayWait(Wait& wait) { return RigReplayWait(wait); }
    void PoolSeen(uint32_t destination, uint32_t source, uint32_t size, uint32_t streamConstant, uint64_t frame)
    {
        Log("capture pool %08X %08X %u %08X %" PRIu64, destination, source, size, streamConstant, frame);
    }
    void Diverged(const char* what) { Log("replay diverged: %s", what); }
}
extern "C" uint64_t rig_timebase() { return g_timebase += 977; }
// Both sides' sleeps between WAIT_REG_MEM polls: the C++'s std::this_thread::sleep_for
// (pm4_sleep.h) and the Rust's gpu::sleep_for, logged and not slept.
extern "C" void rig_loop_sleep(uint64_t nanoseconds) { Log("sleep %" PRIu64, nanoseconds); }

// ---- the Rust side's: runtime/rust_calls.cpp's names --------------------------------------
extern "C"
{
    void mw2_renderer_draw(const gpu::RegisterFile* r, const vk::renderer::DrawCall* c) { LogDraw(*r, *c); }
    void mw2_renderer_resolve(const gpu::RegisterFile* r) { LogResolve(*r); }
    void mw2_renderer_swap(uint32_t frontBuffer) { vk::renderer::Swap(frontBuffer); }
    void mw2_renderer_before_completion() { vk::renderer::BeforeCompletion(); }
    void mw2_renderer_begin_occlusion() { vk::renderer::BeginOcclusionQuery(); }
    bool mw2_renderer_end_occlusion(uint32_t tag, uint64_t* samples) { return EndQuery(tag, *samples); }
    void mw2_renderer_collect_occlusion(void (*deliver)(uint32_t, uint64_t)) { Collect(deliver); }
    void mw2_renderer_idle() { Idle(); }
    void mw2_renderer_before_stream_write(uint32_t physical, uint32_t size) { vk::renderer::BeforeStreamWrite(physical, size); }
    void mw2_textures_memory_written(uint32_t physical, uint32_t size) { vk::textures::MemoryWritten(physical, size); }
    bool mw2_watch_enabled() { return g_watchOn; }
    uint64_t mw2_watch_track(uint32_t physical, uint32_t bytes, uint64_t frame) { return Track(physical, bytes, frame); }
    bool mw2_watch_unwritten(uint32_t physical, uint32_t bytes, uint64_t since) { return Unwritten(physical, bytes, since); }
    void mw2_watch_will_write(uint32_t address, uint32_t bytes) { gpu::watch::WillWrite(address, bytes); }
    extern const bool* const mw2_stream_capturing = &gpu::stream::detail::capturing;
    extern const bool* const mw2_stream_replaying = &gpu::stream::detail::replaying;
    extern const bool* const mw2_stream_counting = &gpu::stream::detail::counting;
    void mw2_stream_packet() { gpu::stream::PacketSlow(); }
    void mw2_stream_read(uint32_t address, uint64_t bytes) { gpu::stream::ReadSlow(address, bytes); }
    void mw2_stream_polled(uint32_t address) { gpu::stream::PolledSlow(address); }
    void mw2_stream_write(uint32_t address, uint32_t bytes, bool done) { gpu::stream::WriteSlow(address, bytes, done); }
    void mw2_stream_aperture_read(uint32_t index, uint32_t value) { gpu::stream::ApertureRead(index, value); }
    void mw2_stream_wait_done(bool memory, uint32_t address, uint32_t value, uint32_t polls, bool timedOut)
    {
        gpu::stream::WaitDone(memory, address, value, polls, timedOut);
    }
    bool mw2_stream_replay_wait(gpu::stream::Wait* wait) { return RigReplayWait(*wait); }
    void mw2_stream_pool_seen(uint32_t destination, uint32_t source, uint32_t size, uint32_t streamConstant, uint64_t frame)
    {
        gpu::stream::PoolSeen(destination, source, size, streamConstant, frame);
    }
    void mw2_stream_diverged(const char* what) { gpu::stream::Diverged(what); }
    void mw2_stream_arena_block_seen(uint32_t block)
    {
        if (gpu::stream::Capturing()) gpu::stream::ArenaBlockSeen(block);
    }
    bool rig_running() { return gpu::detail::Running(); }
    void rig_cp_interrupt(uint32_t cpuMask) { gpu::detail::RaiseCommandProcessorInterrupt(cpuMask); }
}

// ---- one side or the other -------------------------------------------------------------
namespace
{
    uint8_t* Reserve(void* context, size_t size) noexcept
    {
        auto& bytes = *static_cast<std::vector<uint8_t>*>(context);
        bytes.assign(size, 0);
        return bytes.data();
    }

    void ExecuteRing(uint32_t base, uint32_t read, uint32_t mask, uint32_t dwords)
    {
        if (g_rust) mw2_rig_cp_execute_ring(base, read, mask, dwords);
        else gpu::detail::ExecuteRing(base, read, mask, dwords);
    }
    void Deliver()
    {
        if (g_rust) mw2_rig_cp_deliver_occlusion_queries();
        else gpu::detail::DeliverOcclusionQueries();
    }
    std::vector<uint8_t> Save()
    {
        std::vector<uint8_t> out;
        if (g_rust) mw2_cp_save(Reserve, &out);
        else gpu::detail::SaveCommandProcessor(out);
        return out;
    }
    bool Restore(const std::vector<uint8_t>& state)
    {
        return g_rust ? mw2_cp_restore_for_replay(state.data(), state.size())
                      : cpp_mw2_cp_restore_for_replay(state.data(), state.size());
    }
    bool Lut(uint32_t out[256]) { return g_rust ? mw2_cp_display_colour_table(out) : gpu::DisplayColourTable(out); }
    uint64_t Swaps() { return g_rust ? mw2_cp_swaps_parsed() : gpu::detail::SwapsParsed(); }
    void QueueForReplay(uint32_t d, uint32_t s, uint32_t n, uint32_t c, uint64_t f)
    {
        Log("pool queued %08X %08X %u %08X %" PRIu64, d, s, n, c, f);
        if (g_rust) mw2_imagepool_queue_for_replay(d, s, n, c, f);
        else imagepool::QueueForReplay(d, s, n, c, f);
    }
    void SetArenaBlock(uint32_t physical)
    {
        if (g_rust) mw2_arena_set_block(physical);
        else gpu::detail::SetArenaBlock(physical);
    }
    std::string Describe(uint32_t physical, uint32_t size, uint64_t frames)
    {
        char out[1024];
        std::memset(out, 0x55, sizeof out);
        const size_t bytes = 64 + g_answers.Below(960);
        if (g_rust) mw2_imagepool_describe_moves_around(physical, size, frames, out, bytes);
        else imagepool::DescribeMovesAround(physical, size, frames, out, bytes);
        return std::string(out, strnlen(out, bytes));
    }
    bool CopiesWaiting() { return g_rust ? mw2_rig_pool_copies_waiting() : imagepool::CopiesWaiting(); }
    void AtDraw(uint32_t c, uint64_t v)
    {
        if (g_rust) mw2_rig_pool_at_draw(c, v);
        else imagepool::AtDraw(c, v);
    }
    void AtFrameEnd(uint64_t f)
    {
        if (g_rust) mw2_rig_pool_at_frame_end(f);
        else imagepool::AtFrameEnd(f);
    }
    void Present()
    {
        if (g_rust) mw2_rig_census_present();
        else gpu::detail::CensusPresent();
    }
    void Reports()
    {
        if (g_rust)
        {
            mw2_rig_pool_report();
            mw2_rig_census_report();
            mw2_rig_cp_report();
            mw2_rig_arena_report();
        }
        else
        {
            imagepool::Report();
            gpu::detail::ReportCensus();
            gpu::detail::ReportCommandProcessor();
            gpu::detail::ReportArena();
        }
    }

    // After a case: what the command processor holds, and the pool's view of a range.
    void LogState()
    {
        const std::vector<uint8_t> state = Save();
        uint32_t lut[256];
        std::memset(lut, 0, sizeof lut);
        const bool ready = Lut(lut);
        Log("state %zu %016" PRIx64 " lut %d %016" PRIx64 " swaps %" PRIu64 " waiting %d", state.size(),
            HashWords(state.data(), state.size()), ready, HashWords(lut, sizeof lut), Swaps(), CopiesWaiting());
        if (g_answers.One(8))
        {
            const uint32_t physical = kPool + g_answers.Below(kPoolEnd - kPool);
            const uint32_t size = g_answers.One(8) ? 0 : 1 + g_answers.Below(0x40000);
            Log("moves around %08X %u: %s", physical, size, Describe(physical, size, g_answers.Below(20)).c_str());
        }
        g_compared += 6;
    }

    // ---- the streams ---------------------------------------------------------------------
    enum : uint32_t
    {
        NOP = 0x10, INDIRECT_BUFFER = 0x3F, INDIRECT_BUFFER_PFD = 0x37, WAIT_FOR_IDLE = 0x26, WAIT_REG_MEM = 0x3C,
        REG_RMW = 0x21, REG_TO_MEM = 0x3E, MEM_WRITE = 0x3D, COND_WRITE = 0x45, EVENT_WRITE = 0x46,
        EVENT_WRITE_SHD = 0x58, EVENT_WRITE_CFL = 0x59, EVENT_WRITE_ZPD = 0x5B, DRAW_INDX = 0x22, DRAW_INDX_2 = 0x36,
        SET_CONSTANT = 0x2D, SET_CONSTANT2 = 0x55, IM_LOAD = 0x27, IM_LOAD_IMMEDIATE = 0x2B, INVALIDATE_STATE = 0x3B,
        SET_BIN_MASK = 0x50, SET_BIN_SELECT = 0x51, CONTEXT_UPDATE = 0x5E, INTERRUPT = 0x54, ME_INIT = 0x48,
        VIZ_QUERY = 0x23, LOAD_ALU_CONSTANT = 0x2F, EVENT_WRITE_EXT = 0x5A, SET_BIN_MASK_LO = 0x60,
        SET_BIN_MASK_HI = 0x61, SET_BIN_SELECT_LO = 0x62, SET_BIN_SELECT_HI = 0x63, XE_SWAP = 0x64,
    };
    constexpr uint32_t kOpcodes[] = {
        NOP, INDIRECT_BUFFER, INDIRECT_BUFFER_PFD, WAIT_FOR_IDLE, WAIT_REG_MEM, REG_RMW, REG_TO_MEM, MEM_WRITE,
        COND_WRITE, EVENT_WRITE, EVENT_WRITE_SHD, EVENT_WRITE_CFL, EVENT_WRITE_ZPD, DRAW_INDX, DRAW_INDX_2,
        SET_CONSTANT, SET_CONSTANT2, IM_LOAD, IM_LOAD_IMMEDIATE, INVALIDATE_STATE, SET_BIN_MASK, SET_BIN_SELECT,
        CONTEXT_UPDATE, INTERRUPT, ME_INIT, VIZ_QUERY, LOAD_ALU_CONSTANT, EVENT_WRITE_EXT, SET_BIN_MASK_LO,
        SET_BIN_MASK_HI, SET_BIN_SELECT_LO, SET_BIN_SELECT_HI, XE_SWAP,
    };
    // The handlers' least counts, for the short ones.
    uint32_t Least(uint32_t op)
    {
        switch (op)
        {
        case INDIRECT_BUFFER: case INDIRECT_BUFFER_PFD: case SET_BIN_MASK: case SET_BIN_SELECT: case IM_LOAD:
        case IM_LOAD_IMMEDIATE: case DRAW_INDX: case REG_TO_MEM: case XE_SWAP: return 2;
        case EVENT_WRITE_SHD: case EVENT_WRITE_CFL: case LOAD_ALU_CONSTANT: return 3;
        case WAIT_REG_MEM: return 5;
        case COND_WRITE: return 6;
        default: return 1;
        }
    }

    uint32_t g_bufferNext = kBuffers;
    uint32_t g_streamConstant = 0;   // a queued move's, for a draw to name
    uint32_t g_arenaBlock = 0;       // physical, the progress block the stream reports to

    struct Stream
    {
        std::vector<uint32_t> w;
        void Header3(uint32_t op, uint32_t count, bool predicated)
        {
            w.push_back(0xC0000000u | (((count - 1) & 0x3FFF) << 16) | (op << 8) | (predicated ? 1u : 0u));
        }
    };

    // A dword of a value worth trying.
    uint32_t Value()
    {
        constexpr uint32_t kEdges[] = { 0, 1, 2, 3, 4, 0x1F, 0x20, 0x7FFFFFFF, 0x80000000, 0xFFFFFFFF, 0xFFFFFEED, 0x3FF,
                                        0xFFC00, 0x3FF00000, 0x40000000 };
        switch (g_random.Below(4))
        {
        case 0: return kEdges[g_random.Below(std::size(kEdges))];
        case 1: return g_random.Below(256);
        default: return uint32_t(g_random.Next());
        }
    }

    // A physical address a write lands at: the rig's data, the arena block, or anywhere.
    uint32_t WriteAddress()
    {
        switch (g_random.Below(8))
        {
        case 0: return uint32_t(g_random.Next());
        case 1: return g_arenaBlock + 4 * g_random.Below(3);
        case 2: return kData + g_random.Below(0x1000) * 4 + g_random.Below(4);
        default: return kData + g_random.Below(0x4000) * 4;
        }
    }

    // A register run's first register: the constant windows, the ordinary registers, the
    // ones with side effects, or past the end.
    uint32_t Register()
    {
        constexpr uint32_t kNamed[] = { 0x578, 0x579, 0x57F, 0x580, 0x1DC, 0x1DD, 0x1921, 0x1922, 0x1925, 0x1927,
                                        0x2208, 0x2325, 0x21FC, 0x4000, 0x4800, 0x48BF, 0x48C0, 0x4900, 0x4908,
                                        0x5000, 0x5002, 0x5003, 0x7FFF, 0x0A31 };
        switch (g_random.Below(6))
        {
        case 0: return kNamed[g_random.Below(std::size(kNamed))];
        case 1: return g_random.Below(0x8000);
        case 2: return 0x2000 + g_random.Below(0x400);
        default: return 0x4000 + g_random.Below(0x1010);
        }
    }

    void Packets(Stream& s, uint32_t count, int depth);

    // An indirect buffer's place and size: generated, misaligned, oversized, itself.
    std::pair<uint32_t, uint32_t> Buffer(int depth)
    {
        if (depth >= 6 || g_bufferNext + 0x20000 > kBuffersEnd) return { kBuffers, 4 };
        Stream inner;
        Packets(inner, 1 + g_random.Below(depth < 2 ? 12 : 4), depth + 1);
        const uint32_t at = g_bufferNext;
        uint32_t size = uint32_t(inner.w.size());
        if (g_random.One(512))
        {
            // One that calls itself, many times over: the batch's work limit ends it.
            inner.w.clear();
            const uint32_t calls = 4000 + g_random.Below(1400);
            for (uint32_t i = 0; i < calls; i++)
            {
                inner.Header3(INDIRECT_BUFFER, 2, false);
                inner.w.push_back(at);
                inner.w.push_back(calls * 3);
            }
            size = calls * 3;
        }
        for (uint32_t i = 0; i < inner.w.size(); i++) Put32(Va(at) + i * 4, inner.w[i]);
        g_bufferNext = (at + uint32_t(inner.w.size()) * 4 + 0x3F) & ~0x1Fu;
        switch (g_random.Below(24))
        {
        case 0: return { at | (4 << g_random.Below(3)), size };          // misaligned
        case 1: return { at, (16u << 10) + 1 + g_random.Below(4) };       // oversized
        case 2: return { at, size + 1 + g_random.Below(3) };              // runs into whatever follows
        case 3: return { at, size ? size - 1 : 0 };                      // cut short
        case 4: return { uint32_t(g_random.Next()) & ~0x1Fu, 1 + g_random.Below(64) };   // anywhere
        case 5: return { at | 0xFFF00000u, size | (g_random.Below(16) << 20) };          // the size's top bits
        default: return { at, size };
        }
    }

    void Draw(Stream& s, bool predicated, bool two)
    {
        // Now and then a resolve's mode, and a queued move's stream constant in c0.x.
        if (g_random.One(6))
        {
            s.w.push_back(0x00000000u | 0x2208);   // type 0, one register
            s.w.push_back(g_random.One(3) ? 6 : g_random.Below(8));
        }
        if (g_streamConstant && g_random.One(3))
        {
            // The copy draw's shader first, mostly: once the pool has learnt it, a
            // constant under another shader is not a copy.
            if (!g_random.One(16))
            {
                s.Header3(IM_LOAD, 2, false);
                s.w.push_back(kShaders);
                s.w.push_back(8);
            }
            s.w.push_back(0x4000);
            s.w.push_back(g_random.One(6) ? g_streamConstant ^ 0x4 : g_streamConstant);
        }
        const uint32_t initiator = (g_random.Below(0x40)) | (g_random.Below(4) << 6) | (g_random.Below(4) << 8) |
                                   (g_random.Below(2) << 11) | (g_random.Below(2) << 12) | (g_random.Below(0x10000) << 16);
        std::vector<uint32_t> payload;
        if (!two) payload.push_back(Value());
        payload.push_back(g_random.Below(4) ? initiator & ~0xC0u : initiator);
        const uint32_t extra = g_random.Below(4) ? 2 : g_random.Below(3);
        for (uint32_t i = 0; i < extra; i++)
            payload.push_back(i == 0 ? (g_random.Below(4) ? kData + g_random.Below(0x1000) * 2 : Value()) : Value());
        s.Header3(two ? DRAW_INDX_2 : DRAW_INDX, uint32_t(payload.size()), predicated);
        s.w.insert(s.w.end(), payload.begin(), payload.end());
    }

    void Type3(Stream& s, int depth)
    {
        uint32_t op = kOpcodes[g_random.Below(std::size(kOpcodes))];
        if (g_random.One(40)) op = g_random.Below(128);   // unknown ones among them
        const bool predicated = g_random.One(10);
        const bool shorted = g_random.One(20);
        std::vector<uint32_t> p;
        switch (op)
        {
        case INDIRECT_BUFFER:
        case INDIRECT_BUFFER_PFD:
        {
            const auto [at, size] = Buffer(depth);
            p = { at, size };
            break;
        }
        case MEM_WRITE:
            p.push_back(WriteAddress());
            for (uint32_t i = g_random.Below(6); i; i--) p.push_back(Value());
            break;
        case EVENT_WRITE_SHD:
            p = { g_random.One(6) ? 0x80000000u | Value() : Value(), WriteAddress(),
                  g_random.Below(2) ? Value() : g_random.Below(4096) };
            break;
        case EVENT_WRITE_CFL: p = { Value(), WriteAddress(), Value() }; break;
        case EVENT_WRITE_ZPD:
        {
            // The record's address first: a begin (bit 5), an end, unaligned, or none.
            const uint32_t record = g_random.One(8) ? Value() : kData + 0x8000 + g_random.Below(64) * 0x40 +
                                                                  (g_random.Below(2) ? 0x20 : 0) + g_random.Below(2) * 4;
            s.w.push_back(0x2325);
            s.w.push_back(record);
            p = { Value() };
            break;
        }
        case XE_SWAP: p = { 0x53574150u, Value(), 1280, 720 }; p.resize(1 + g_random.Below(4)); break;
        case SET_BIN_MASK_LO: case SET_BIN_MASK_HI: case SET_BIN_SELECT_LO: case SET_BIN_SELECT_HI:
            p = { g_random.Below(3) ? (g_random.Below(2) ? 0u : 0xFFFFFFFFu) : Value() };
            break;
        case SET_BIN_MASK: case SET_BIN_SELECT:
            p = { g_random.Below(2) ? 0u : Value(), g_random.Below(3) ? 0u : Value() };
            break;
        case IM_LOAD_IMMEDIATE:
        {
            const uint32_t words = g_random.Below(5) ? 1 + g_random.Below(40) : 0;
            p = { g_random.Below(4), (g_random.Below(16) << 16) | (g_random.One(8) ? words + 1 : words) };
            for (uint32_t i = 0; i < words; i++) p.push_back(g_random.Below(3) ? uint32_t(g_random.Below(8)) : Value());
            break;
        }
        case IM_LOAD:
        {
            // One of a few shaders at fixed places, so the memo sees them again; now and
            // then a shorter or longer program at the same place, a memo entry of its own.
            const uint32_t which = g_random.Below(12);
            const uint32_t at = kShaders + which * 0x1000;
            uint32_t words = g_random.One(16) ? (g_random.Below(2) ? 0 : 0x10001) : 8 + which * 13;
            if (words == 8 + which * 13 && g_random.One(4)) words = 1 + g_random.Below(8 + which * 13 + 32);
            uint32_t address = at | g_random.Below(4);
            if (g_random.One(16)) address = Value();
            p = { address, (g_random.Below(16) << 16) | words };
            break;
        }
        case INTERRUPT: p = { g_random.Below(4) ? 1u << g_random.Below(6) : Value() }; break;
        case SET_CONSTANT:
        {
            const uint32_t kind = g_random.Below(7) < 5 ? g_random.Below(5) : g_random.Below(256);
            const uint32_t count = g_random.Below(4) ? g_random.Below(70) : g_random.Below(600);
            p.push_back((kind << 16) | (g_random.Below(4) ? g_random.Below(0x80) * 4 : g_random.Below(0x800)));
            for (uint32_t i = 0; i < count; i++) p.push_back(Value());
            break;
        }
        case SET_CONSTANT2:
        {
            const uint32_t count = g_random.Below(4) ? g_random.Below(70) : g_random.Below(400);
            p.push_back((g_random.Below(4) ? Register() : Value()) | (g_random.Below(2) << 16));
            for (uint32_t i = 0; i < count; i++) p.push_back(Value());
            break;
        }
        case LOAD_ALU_CONSTANT:
        {
            const uint32_t kind = g_random.Below(7) < 5 ? g_random.Below(5) : g_random.Below(256);
            // Now and then off a dword's alignment: nothing in the packet aligns it.
            const uint32_t at = kConstants + g_random.Below(0x100) * 0x100 + (g_random.One(8) ? 1 + g_random.Below(3) : 0);
            p = { g_random.One(10) ? Value() : at | (g_random.Below(4) << 30),
                  (kind << 16) | (g_random.Below(4) ? g_random.Below(0x80) * 4 : g_random.Below(0x800)),
                  g_random.Below(4) ? g_random.Below(300) : Value() };
            break;
        }
        case DRAW_INDX: case DRAW_INDX_2: Draw(s, predicated, op == DRAW_INDX_2); return;
        case REG_TO_MEM: p = { g_random.Below(4) ? Register() : Value(), WriteAddress() }; break;
        case WAIT_REG_MEM:
        {
            // { info, address, reference, mask, interval }: memory or a register, every
            // function and swap, met at once, at a later poll, or never.
            const bool memory = g_random.Below(3) != 0;
            const uint32_t function = g_random.Below(8);
            const uint32_t word = kData + 0xC000 + g_random.Below(64) * 4;
            const uint32_t address = memory ? (g_random.One(16) ? Value() : word | g_random.Below(4))
                                            : (g_random.Below(3) ? 0x0A31 : Register());
            const uint32_t reference = g_random.Below(2) ? g_random.Below(4) : Value();
            const uint32_t mask = g_random.Below(2) ? 0xFFFFFFFFu : Value();
            p = { function | (memory ? 0x10u : 0u) | (g_random.Below(4) << 8), address, reference, mask, Value() };
            break;
        }
        case COND_WRITE:
            p = { g_random.Below(2) ? 0x100u : Value(), WriteAddress(), Value(), Value(), WriteAddress(), Value() };
            break;
        default:
            for (uint32_t i = g_random.Below(5); i; i--) p.push_back(Value());
            if (p.empty()) p.push_back(Value());
            break;
        }
        if (shorted && p.size() > 1) p.resize(g_random.Below(std::max<uint32_t>(Least(op), 1)) + 1 > p.size()
                                              ? p.size() - 1 : g_random.Below(std::max<uint32_t>(Least(op), 1)) + 1);
        if (g_random.One(30)) p.push_back(Value());   // more than the handler reads
        if (p.empty()) p.push_back(0);
        s.Header3(op, uint32_t(p.size()), predicated);
        s.w.insert(s.w.end(), p.begin(), p.end());
    }

    void Packets(Stream& s, uint32_t count, int depth)
    {
        for (uint32_t n = 0; n < count; n++)
        {
            switch (g_random.Below(20))
            {
            case 0:
            {
                // Type 0: a run, or one register fed again and again.
                const bool one = g_random.One(5);
                const uint32_t base = Register();
                const uint32_t length = g_random.Below(4) ? 1 + g_random.Below(16) : 1 + g_random.Below(300);
                s.w.push_back(((length - 1) << 16) | (one ? 0x8000u : 0u) | (base & 0x7FFF));
                for (uint32_t i = 0; i < length; i++) s.w.push_back(Value());
                break;
            }
            case 1:
            {
                // Type 1: two registers, the LUT's among them.
                const uint32_t a = g_random.Below(3) ? 0x1925 : g_random.Below(0x800);
                const uint32_t b = g_random.Below(3) ? 0x1922 : g_random.Below(0x800);
                s.w.push_back(0x40000000u | ((b & 0x7FF) << 11) | (a & 0x7FF));
                s.w.push_back(Value());
                s.w.push_back(Value());
                break;
            }
            case 2: s.w.push_back(0x80000000u | g_random.Below(0x40000000)); break;   // type 2
            case 3: s.w.push_back(0); break;                                          // padding
            case 4:
                if (g_random.One(20)) s.w.push_back(0xFFFFFFFFu);
                else Draw(s, g_random.One(10), g_random.Below(4) != 0);
                break;
            case 5:
            {
                // The display table swept, entry by entry, with each write mask.
                s.w.push_back(0x1927);
                s.w.push_back(g_random.Below(8));
                s.w.push_back(0x1922);
                s.w.push_back(g_random.Below(4) ? 0 : g_random.Below(256));
                const uint32_t entries = g_random.Below(3) ? 256 : g_random.Below(300);
                s.w.push_back(((entries ? entries - 1 : 0) << 16) | 0x8000u | 0x1925);
                for (uint32_t i = 0; i < std::max(entries, 1u); i++) s.w.push_back(Value());
                break;
            }
            case 6:
            {
                // Scratch write-back: the mask and address, then the registers.
                s.w.push_back(0x01DC);
                s.w.push_back(g_random.Below(4) ? 0xFF : Value());
                s.w.push_back(0x01DD);
                s.w.push_back(g_random.Below(4) ? kData + 0x10000 : (g_random.Below(2) ? 0u : Value()));
                s.w.push_back((7u << 16) | 0x0578);
                for (uint32_t i = 0; i < 8; i++) s.w.push_back(Value());
                break;
            }
            default: Type3(s, depth); break;
            }
        }
    }

    // A case's stubs, flags and memory set the same way on both sides.
    void Setup(uint64_t caseNumber)
    {
        g_answers.state = caseNumber * 0x9E3779B97F4A7C15ull ^ g_random.Next();
        g_frames = g_random.Below(4) ? g_frames + g_random.Below(2) : g_random.Below(1000);
        g_consuming = g_random.Below(4) != 0;
        g_seconds = g_random.Below(8) ? g_seconds + 0.25 * g_random.Below(3) : double(g_random.Below(500)) - 50;
        SetValues();
        g_watchOn = g_random.Below(3) != 0;
        g_runningPolls = g_random.Below(4) ? g_random.Below(3) : g_random.Below(8);
        g_idles = 0;
        g_satisfyAtPoll = g_random.Below(3) ? 0 : 1 + g_random.Below(4);
        g_satisfyAddress = Va(kData + 0xC000 + g_random.Below(64) * 4);
        g_satisfyValue = Value();
        gpu::stream::detail::counting = g_random.One(6);
        gpu::stream::detail::capturing = gpu::stream::detail::counting && g_random.Below(2);
        gpu::stream::detail::replaying = gpu::stream::detail::counting && !gpu::stream::detail::capturing &&
                                         g_random.Below(2);
        g_packet = 0;
        g_journal.clear();
        if (gpu::stream::detail::replaying)
            for (uint32_t i = g_random.Below(6); i; i--)
                g_journal.push_back({ g_random.Below(2) != 0, Value(), g_random.Below(4), g_random.One(4) });

        // Memory the stream reads: the waited-for words, the shaders, the constants, the
        // aperture's scratch registers, now and then rewritten.
        for (uint32_t i = 0; i < 64; i++)
            if (g_random.One(4)) PutRaw32(Va(kData + 0xC000 + i * 4), g_random.Below(2) ? g_random.Below(4) : Value());
        if (g_random.One(8))
        {
            // Not the copy draw's shader (the first), which the pool learns.
            const uint32_t which = 1 + g_random.Below(11);
            for (uint32_t i = 0; i < 8 + which * 13; i++) Put32(Va(kShaders + which * 0x1000 + i * 4), Value());
        }
        if (g_random.One(8))
            for (uint32_t i = 0; i < 256; i++) Put32(Va(kConstants + g_random.Below(0x4000) * 4), Value());
        if (g_random.One(16))
        {
            Put32(kAperture + 0x1DC * 4, g_random.Below(2) ? 0xFF : Value());
            Put32(kAperture + 0x1DD * 4, g_random.Below(2) ? kData + 0x10040 : Value());
        }
        // The arena's progress block, learnt now and then.
        if (g_random.One(32))
        {
            g_arenaBlock = g_random.Below(4) ? kData + 0x14000 + g_random.Below(16) * 8 : 0;
            SetArenaBlock(g_arenaBlock);
        }
        // An image-pool move queued, as the title's hook queues one.
        if (g_random.One(6))
        {
            const uint32_t size = (1 + g_random.Below(0x4000)) * 4;
            const uint32_t destination = Va(kPool + g_random.Below(0x100) * 0x2000);
            const uint32_t source = Va(kPool + 0x200000 + g_random.Below(0x100) * 0x1000);
            const uint32_t physical = destination - kPhysicalBase;
            g_streamConstant = 0x40000000u | (physical >> 2);
            QueueForReplay(destination, source, size, g_streamConstant, g_frames + g_random.Below(3) - 1);
        }
        if (g_random.One(12)) Present();
        Quiet();
    }

    void Pm4(uint64_t cases, uint64_t show)
    {
        // The pool's source blocks hold something to copy.
        for (uint32_t i = 0; i < 0x200000 / 4; i += 4)
        {
            Put32(Va(kPool + 0x200000 + i * 4), i * 0x01010101u);
            if (g_dirty.size() >= 256) Quiet();
        }
        for (uint32_t i = 0; i < 12; i++)
            for (uint32_t w = 0; w < 8 + i * 13; w++) Put32(Va(kShaders + i * 0x1000 + w * 4), (i << 24) | w);
        Quiet();
        std::vector<uint8_t> saved;
        for (uint64_t n = 1; n <= cases; n++)
        {
            rig_log::Take(0);
            Setup(n);
            // Now and then the command processor put back as an earlier case left it,
            // onto the memo it has now (the replay's restore, on a consumer that has run),
            // or a state.bin it must refuse: cut short, a word too long, another magic or
            // version.
            if (!saved.empty() && g_random.One(24))
            {
                std::vector<uint8_t> state = saved;
                switch (g_random.Below(8))
                {
                case 0: state.resize(g_random.Below(uint32_t(state.size()))); break;
                case 1: state.resize(state.size() + 4); break;
                case 2: state[g_random.Below(4)] ^= 1u << g_random.Below(8); break;
                case 3: state[4 + g_random.Below(4)] ^= 1u << g_random.Below(8); break;
                default: break;
                }
                Log("restore %zu: %d", state.size(), Restore(state));
            }
            if (g_random.One(16)) saved = Save();
            Stream s;
            g_bufferNext = kBuffers;
            Packets(s, 1 + g_random.Below(g_random.One(8) ? 120 : 24), 0);
            // Damage: a dword changed, the batch cut short.
            if (g_random.One(12) && !s.w.empty()) s.w[g_random.Below(uint32_t(s.w.size()))] = Value();
            uint32_t dwords = uint32_t(s.w.size());
            if (g_random.One(10) && dwords > 1) dwords -= 1 + g_random.Below(std::min(dwords - 1, 8u));
            // The ring: a size, and a place to start that wraps now and then.
            uint32_t ringDwords = 1u << (6 + g_random.Below(9));
            while (ringDwords < s.w.size() + 1 && ringDwords < 16384) ringDwords <<= 1;
            if (s.w.size() + 1 > ringDwords)
            {
                s.w.resize(ringDwords - 1);
                dwords = std::min(dwords, ringDwords - 1);
            }
            const uint32_t mask = ringDwords - 1;
            const uint32_t read = g_random.Below(2) ? ringDwords - g_random.Below(std::min<uint32_t>(ringDwords, 40)) - 1
                                                    : g_random.Below(ringDwords);
            for (uint32_t i = 0; i < s.w.size(); i++) Put32(Va(kRing) + ((read + i) & mask) * 4, s.w[i]);
            Quiet();
            Log("case %" PRIu64 ": %u dwords from %u of a ring of %u", n, dwords, read & mask, ringDwords);
            ExecuteRing(Va(kRing), read & mask, mask, dwords);
            LogDirty("batch");
            // The consumer delivers what the GPU has counted between batches.
            if (g_random.One(3)) Deliver();
            LogDirty("delivery");
            LogState();
            rig_log::Stop();
            const std::string& text = rig_log::g_log[0];
            if (show == n) std::fwrite(text.data(), 1, text.size(), stdout);
            else if (!show) std::printf("case %" PRIu64 " %zu %016" PRIx64 "\n", n, text.size(), HashWords(text.data(), text.size()));
            g_compared += 1;
            rig_log::Clear();
        }
    }

    // The pool alone: an event script.
    void Pool(uint64_t cases, uint64_t show)
    {
        for (uint32_t i = 0; i < 0x200000 / 4; i += 4)
        {
            Put32(Va(kPool + 0x200000 + i * 4), i * 0x02030405u);
            if (g_dirty.size() >= 256) Quiet();
        }
        Quiet();
        uint64_t parsed = 0;
        std::vector<uint32_t> constants;
        constexpr uint64_t kShadersSeen[] = { 0x1111, 0x2222, 0x3333 };
        for (uint64_t n = 1; n <= cases; n++)
        {
            rig_log::Take(0);
            g_answers.state = n * 0x9E3779B97F4A7C15ull ^ g_random.Next();
            g_frames = g_random.Below(4) ? g_frames + g_random.Below(2) : g_random.Below(100);
            SetValues();
            gpu::stream::detail::capturing = g_random.One(4);
            Log("pool case %" PRIu64 ": frame %" PRIu64 " capturing %d", n, g_frames, gpu::stream::detail::capturing);
            for (uint32_t step = 1 + g_random.Below(12); step; step--)
            {
                switch (g_random.Below(6))
                {
                case 0:
                case 1:
                {
                    const uint32_t size = (1 + g_random.Below(g_random.Below(4) ? 0x400 : 0x8000)) * 4;
                    const uint32_t destination = Va(kPool + g_random.Below(0x100) * 0x2000);
                    const uint32_t source = Va(kPool + 0x200000 + g_random.Below(0x100) * 0x1000);
                    const uint32_t constant = 0x40000000u | ((destination - kPhysicalBase) >> 2);
                    constants.push_back(constant);
                    QueueForReplay(destination, source, size, constant, g_frames + g_random.Below(3) - 1);
                    break;
                }
                case 2:
                case 3:
                {
                    uint32_t constant = constants.empty() || g_random.One(4) ? Value() : constants[g_random.Below(uint32_t(constants.size()))];
                    if (g_random.One(8)) constant = (constant & 0x3FFFFFFF) | (g_random.Below(4) << 30);
                    const uint64_t shader = g_random.One(8) ? g_random.Next() : kShadersSeen[g_random.Below(3)];
                    Log("pool draw %08X %016" PRIx64 " waiting %d", constant, shader, CopiesWaiting());
                    if (CopiesWaiting()) AtDraw(constant, shader);
                    break;
                }
                case 4:
                    parsed += g_random.Below(3);
                    Log("pool frame end %" PRIu64 " waiting %d", parsed, CopiesWaiting());
                    if (CopiesWaiting()) AtFrameEnd(parsed);
                    break;
                default:
                {
                    const uint32_t physical = kPool + g_random.Below(kPoolEnd - kPool);
                    const uint32_t size = g_random.One(8) ? 0 : 1 + g_random.Below(0x40000);
                    const uint64_t frames = g_random.Below(30);
                    Log("pool moves around %08X %u %" PRIu64 ": %s", physical, size, frames,
                        Describe(g_random.One(16) ? Value() : physical, size, frames).c_str());
                    break;
                }
                }
                LogDirty("pool");
            }
            Log("pool waiting %d", CopiesWaiting());
            rig_log::Stop();
            const std::string& text = rig_log::g_log[0];
            if (show == n) std::fwrite(text.data(), 1, text.size(), stdout);
            else if (!show) std::printf("pool %" PRIu64 " %zu %016" PRIx64 "\n", n, text.size(), HashWords(text.data(), text.size()));
            g_compared += 1;
            rig_log::Clear();
        }
    }

    // A capture, as gpu_diff.py --capture writes it for the rig: little-endian words.
    //   "MW2R" top ring_physical ring_dwords write_back state_bytes snapshot_pages batches
    //   state bytes; per snapshot page: page, 4096 bytes
    //   per batch: ring_dwords, pages, events; its ring dwords (as the guest holds them);
    //     per page: packet, page, own, 4096 bytes; per event: kind packet a b c d e(64)
    void Capture(const char* path, uint64_t show)
    {
        // Mapped, not read: a capture is hundreds of megabytes.
        const int fd = open(path, O_RDONLY);
        struct stat info{};
        if (fd < 0 || fstat(fd, &info) != 0) { std::fprintf(stderr, "no %s\n", path); std::exit(2); }
        const auto* data = static_cast<const uint8_t*>(mmap(nullptr, size_t(info.st_size), PROT_READ, MAP_PRIVATE, fd, 0));
        if (data == MAP_FAILED) std::exit(2);
        struct View { const uint8_t* p; const uint8_t* data() const { return p; } } file{ data };
        size_t at = 0;
        const auto word = [&] { uint32_t v; std::memcpy(&v, file.data() + at, 4); at += 4; return v; };
        if (word() != 0x5232574Du) { std::fprintf(stderr, "%s is not a rig capture\n", path); std::exit(2); }
        const uint32_t top = word(), ringPhysical = word(), ringDwords = word(), writeBack = word();
        const uint32_t stateBytes = word(), snapshotPages = word(), batches = word();
        (void)writeBack;
        mw2_kernel_physical_top = top;
        std::vector<uint8_t> state(file.data() + at, file.data() + at + stateBytes);
        at += stateBytes;
        for (uint32_t i = 0; i < snapshotPages; i++)
        {
            const uint32_t page = word();
            std::memcpy(g_base + uint64_t(page) * kPage, file.data() + at, kPage);
            at += kPage;
            // A writable page is a mapping of its own: put them back before there are too many.
            if (g_dirty.size() >= 1024) Quiet();
        }
        Quiet();
        rig_log::Take(0);
        Log("restore %d", Restore(state));
        rig_log::Stop();
        if (!show) std::printf("restore %016" PRIx64 "\n", HashWords(rig_log::g_log[0].data(), rig_log::g_log[0].size()));
        rig_log::Clear();
        gpu::stream::detail::replaying = gpu::stream::detail::counting = true;
        const uint32_t ring = kPhysicalBase + ringPhysical, mask = ringDwords - 1;
        uint32_t written = 0;
        g_frames = 0;
        g_consuming = true;
        g_seconds = 1;
        SetValues();
        for (uint32_t b = 1; b <= batches; b++)
        {
            const uint32_t dwords = word(), pages = word(), events = word();
            for (uint32_t i = 0; i < dwords; i++)
            {
                std::memcpy(g_base + ring + ((written + i) & mask) * 4, file.data() + at, 4);
                at += 4;
            }
            Quiet();
            g_pages.clear();
            g_events.clear();
            g_nextPage = g_nextEvent = 0;
            for (uint32_t i = 0; i < pages; i++)
            {
                const uint32_t packet = word(), page = word(), own = word();
                if (!own) g_pages.push_back({ packet, page, file.data() + at });
                at += kPage;
            }
            for (uint32_t i = 0; i < events; i++)
            {
                Event e;
                e.kind = word(); e.packet = word(); e.a = word(); e.b = word(); e.c = word(); e.d = word();
                uint64_t lo = word(), hi = word();
                e.e = lo | hi << 32;
                g_events.push_back(e);
            }
            // The pages the batch found at its start, and the events before its first packet.
            for (const Applied& a : g_pages)
                if (a.packet == 0)
                {
                    std::memcpy(g_base + uint64_t(a.page) * kPage, a.bytes, kPage);
                    if (g_dirty.size() >= 1024) Quiet();
                }
            Quiet();
            rig_log::Take(0);
            g_answers.state = b;
            g_runningPolls = 0;
            g_watchOn = false;
            g_packet = 0;
            Apply(0);
            Log("batch %u: %u dwords from %u", b, dwords, written);
            ExecuteRing(ring, written, mask, dwords);
            written = (written + dwords) & mask;
            LogDirty("batch");
            Deliver();
            LogDirty("delivery");
            LogState();
            rig_log::Stop();
            const std::string& text = rig_log::g_log[0];
            if (show == b) std::fwrite(text.data(), 1, text.size(), stdout);
            else if (!show) std::printf("batch %u %zu %016" PRIx64 "\n", b, text.size(), HashWords(text.data(), text.size()));
            g_compared += 1;
            rig_log::Clear();
        }
    }
}

int main(int argc, char** argv)
{
    if (argc < 5)
    {
        std::fprintf(stderr, "usage: pm4 <seed> <cases> cpp|rust <part>... [--show N]\n");
        return 2;
    }
    g_random.state = std::strtoull(argv[1], nullptr, 10) * 0x2545F4914F6CDD1Dull + 1;
    const uint64_t cases = std::strtoull(argv[2], nullptr, 10);
    g_rust = std::string(argv[3]) == "rust";
    uint64_t show = 0;
    std::vector<std::string> parts;
    for (int i = 4; i < argc; i++)
    {
        if (std::string(argv[i]) == "--show" && i + 1 < argc) show = std::strtoull(argv[++i], nullptr, 10);
        else parts.push_back(argv[i]);
    }

    // The guest's 4 GB, read-only: a write faults once a page and is seen.
    auto* mapped = static_cast<uint8_t*>(mmap(nullptr, kMapped + 0x10000, PROT_READ,
                                              MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0));
    if (mapped == MAP_FAILED) return 3;
    g_base = reinterpret_cast<uint8_t*>((reinterpret_cast<uintptr_t>(mapped) + 0xFFFF) & ~uintptr_t(0xFFFF));
    mw2_guest_base = g_base;
    mw2_kernel_physical_top = kPhysicalBase + kPhysicalBytes;
    struct sigaction action{};
    action.sa_sigaction = OnFault;
    action.sa_flags = SA_SIGINFO | SA_NODEFER;
    sigaction(SIGSEGV, &action, nullptr);

    for (const std::string& part : parts)
    {
        if (part == "pm4") Pm4(cases, show);
        else if (part == "image_pool") Pool(cases, show);
        else if (part.rfind("capture:", 0) == 0) Capture(part.c_str() + 8, show);
        else { std::fprintf(stderr, "unknown part %s\n", part.c_str()); return 2; }
    }

    // The reports, the timings in them masked: they are each side's own clock.
    rig_log::Take(0);
    Reports();
    rig_log::Stop();
    if (!show)
    {
        std::string text = rig_log::g_log[0];
        size_t from = 0;
        while (from < text.size())
        {
            size_t end = text.find('\n', from);
            if (end == std::string::npos) end = text.size();
            std::string line = text.substr(from, end - from);
            if (line.find("waited in all") != std::string::npos)
                line = line.substr(0, line.find('(')) + "(masked)" + line.substr(line.find(')') + 1);
            if (line.find("frames by") != std::string::npos) line = line.substr(0, line.find(':', line.find("frames by"))) + ": masked";
            std::printf("report %s\n", line.c_str());
            from = end + 1;
        }
        std::printf("%" PRIu64 " fields compared\n", g_compared);
    }
    return 0;
}
