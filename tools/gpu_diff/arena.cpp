// tools/gpu_diff/arena.cpp: D3D9's arena progress, C++ (runtime/gpu/arena.cpp as it is)
// against the Rust (src/runtime/gpu/arena.rs) through the C functions the C++ command
// processor and capture call (exports.rs), on generated sequences of the reports the
// command processor meets: a block learnt and learnt again, positions and sequences
// aimed at it and beside it, a first sequence of 0, wraps, and advances of 0, 1, 1024,
// 1025 and back. After every step: the answer, the command processor's writes, the
// capture's sightings of the block, the state the capture stores, the lines each side
// printed (the block learnt, only when it changes) and its report line, which counts the
// reports applied and the stale ones dropped.
//
//   arena <seed> <cases> [arena]...
//
// Exits non-zero at the first difference, printing the case.
#include "gpu/gpu.h"
#include "gpu/internal.h"
#include "gpu/stream_capture.h"
#include "rig_log.h"

#include <cinttypes>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>

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

    // What each side did beside answering: [kind, a, b] triples.
    std::vector<uint32_t> g_did[2];
    void Did(int side, uint32_t kind, uint32_t a, uint32_t b = 0)
    {
        g_did[side].insert(g_did[side].end(), { kind, a, b });
    }
    enum : uint32_t { kWrite = 1, kSeen = 2 };
}

// What arena.cpp calls of the command processor and the capture, and what arena.rs
// calls of them through runtime/rust_calls.cpp: each side's own record.
void gpu::detail::WritePhysical(uint32_t physicalAddress, uint32_t value) { Did(0, kWrite, physicalAddress, value); }
namespace gpu::stream
{
    namespace detail
    {
        bool capturing = false, replaying = false, counting = false;
    }
    void ArenaBlockSeen(uint32_t block) { Did(0, kSeen, block); }
}
extern "C" void mw2_cp_write_physical(uint32_t physical, uint32_t value) { Did(1, kWrite, physical, value); }
extern "C" void mw2_stream_arena_block_seen(uint32_t block)
{
    if (gpu::stream::Capturing()) Did(1, kSeen, block);
}

extern "C"
{
    bool mw2_arena_progress(uint32_t address, uint32_t value);
    void mw2_arena_save(gpu::detail::ArenaState* out);
    void mw2_arena_restore(const gpu::detail::ArenaState* state);
    void mw2_arena_set_block(uint32_t physical);
    void mw2_rig_set_arena_progress_block(uint32_t address);
    void mw2_rig_arena_report();
}

namespace
{
    std::string g_case;

    [[noreturn]] void Fail(const char* what)
    {
        std::printf("DIFFERENCE: %s\n%s", what, g_case.c_str());
        std::exit(1);
    }

    void Note(const char* format, uint32_t a, uint32_t b = 0)
    {
        char line[96];
        std::snprintf(line, sizeof line, format, a, b);
        g_case += line;
    }

    void Compare(bool cpp, bool rust)
    {
        // The report line, after what the step printed.
        rig_log::Take(0);
        gpu::detail::ReportArena();
        rig_log::Take(1);
        mw2_rig_arena_report();
        if (!rig_log::Same())
        {
            const std::string text = "the lines printed:\n  C++:  " + rig_log::g_log[0] + "  Rust: " + rig_log::g_log[1];
            Fail(text.c_str());
        }
        rig_log::Clear();
        if (cpp != rust) Fail("the answer");
        const gpu::detail::ArenaState a = gpu::detail::SaveArena();
        gpu::detail::ArenaState b;
        mw2_arena_save(&b);
        if (std::memcmp(&a, &b, sizeof a))
        {
            char text[160];
            std::snprintf(text, sizeof text, "the state: C++ %08X %08X %08X %u, Rust %08X %08X %08X %u", a.block, a.sequence,
                          a.pending, a.havePosition, b.block, b.sequence, b.pending, b.havePosition);
            Fail(text);
        }
        if (g_did[0] != g_did[1]) Fail("the writes or the sightings");
        g_compared += 7 + g_did[0].size();
        g_did[0].clear();
        g_did[1].clear();
    }

    // A virtual address of the block, as D3D9 holds it.
    uint32_t BlockAddress()
    {
        constexpr uint32_t kBlocks[] = { 0xA0001000u, 0xA0001000u, 0xBFFFFFF8u, 0xC0002000u, 0x82000004u, 0x40000000u, 0xDFFFFFFCu };
        return Below(8) ? kBlocks[Below(std::size(kBlocks))] : uint32_t(Next());
    }

    void Arena(uint32_t cases)
    {
        for (uint32_t i = 0; i < cases; i++)
        {
            g_case.clear();
            // From a state of either kind: as the run starts, or mid-way.
            gpu::detail::ArenaState start{};
            if (Below(2))
                start = { uint32_t(Next()) & ~3u, Below(4) ? uint32_t(Next()) : Below(3), uint32_t(Next()), Below(2) };
            if (Below(4) == 0) start.block = 0;
            gpu::detail::RestoreArena(start);
            mw2_arena_restore(&start);
            Note("start %08X %08X\n", start.block, start.sequence);
            gpu::stream::detail::capturing = false;
            const uint32_t steps = 1 + Below(48);
            for (uint32_t step = 0; step < steps; step++)
            {
                const uint32_t kind = Below(32);
                if (kind == 0)
                {
                    const uint32_t address = BlockAddress();
                    Note("block %08X\n", address);
                    rig_log::Take(0);
                    gpu::SetArenaProgressBlock(address);
                    rig_log::Take(1);
                    mw2_rig_set_arena_progress_block(address);
                    Compare(true, true);
                    continue;
                }
                if (kind == 1)
                {
                    const uint32_t physical = Below(4) ? uint32_t(Next()) & ~3u : 0;
                    Note("set block %08X\n", physical);
                    rig_log::Take(0);
                    gpu::detail::SetArenaBlock(physical);
                    rig_log::Take(1);
                    mw2_arena_set_block(physical);
                    Compare(true, true);
                    continue;
                }
                if (kind == 2) gpu::stream::detail::capturing = !gpu::stream::detail::capturing;
                const gpu::detail::ArenaState now = gpu::detail::SaveArena();
                // Where the report is aimed: the sequence, the position, either of them
                // unaligned, a word beside them, or anywhere.
                constexpr int32_t kNear[] = { 0, 0, 0, 4, 4, 4, 1, 2, 3, 5, 6, 7, -4, 8, -1 };
                const uint32_t address = Below(16) ? now.block + uint32_t(kNear[Below(std::size(kNear))]) : uint32_t(Next());
                // The value: a sequence's next, an advance at either side of the bounds,
                // backwards, 0, a wrap, or anything.
                constexpr int32_t kAdvance[] = { 2, 2, 2, 2, 1, 0, 1023, 1024, 1025, -1, -2, -1024, 0x7FFFFFFF, INT32_MIN };
                uint32_t value;
                switch (Below(8))
                {
                case 0: value = 0; break;
                case 1: value = 0xFFFFFFFEu + Below(4); break;
                case 2: value = uint32_t(Next()); break;
                default: value = now.sequence + uint32_t(kAdvance[Below(std::size(kAdvance))]); break;
                }
                Note("report %08X = %08X\n", address, value);
                rig_log::Take(0);
                const bool cpp = gpu::detail::ArenaProgress(address, value);
                rig_log::Take(1);
                const bool rust = mw2_arena_progress(address, value);
                Compare(cpp, rust);
            }
        }
    }
}

int main(int argc, char** argv)
{
    if (argc < 3)
    {
        std::fprintf(stderr, "usage: arena <seed> <cases> [arena]...\n");
        return 2;
    }
    g_state = std::strtoull(argv[1], nullptr, 10) * 0x2545F4914F6CDD1Dull + 1;
    const uint32_t cases = uint32_t(std::strtoul(argv[2], nullptr, 10));
    for (int i = 3; i < argc; i++)
    {
        const std::string what = argv[i];
        if (what == "arena") Arena(cases);
        else { std::fprintf(stderr, "unknown part %s\n", argv[i]); return 2; }
    }
    std::printf("%" PRIu64 " fields compared\n", g_compared);
    return 0;
}
