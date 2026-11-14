// Asset-free test of the GPU command processor (runtime/gpu/command_processor.cpp):
// it is fed PM4 streams built here, from nothing but the packet formats, and
// what it does is read back through the renderer's draw calls and the guest's
// memory. The Vulkan renderer and the rest of the GPU thread are replaced by
// recording fakes; no game data, no recompiled code and no GPU are involved.
#include "tests/fake_guest.h"
#include "runtime/gpu/gpu.h"
#include "runtime/gpu/internal.h"
#include "runtime/gpu/memory_watch.h"
#include "runtime/gpu/registers.h"
#include "runtime/gpu/vulkan/renderer.h"
#include "runtime/image_move.h"
#include "runtime/pacing_trace.h"

#ifdef NDEBUG
#undef NDEBUG
#endif
#include <atomic>
#include <cassert>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <memory>
#include <mutex>
#include <thread>
#include <vector>

// ---- what the command processor calls, as recording fakes ---------------------
namespace
{
    struct Drawn
    {
        vk::renderer::DrawCall call;
        std::unique_ptr<gpu::RegisterFile> registers;
    };
    std::vector<Drawn> g_draws;
    std::vector<uint32_t> g_swaps, g_interrupts;
    std::atomic<int> g_resolves{ 0 }, g_completions{ 0 }, g_idles{ 0 }, g_runningChecks{ 0 };
    std::atomic<bool> g_running{ true };
    constexpr uint32_t kArenaAddress = 0x7000;
    std::vector<std::pair<uint32_t, uint32_t>> g_arena;
}

namespace vk::renderer
{
    void Draw(const gpu::RegisterFile& registers, const DrawCall& call)
    {
        g_draws.push_back({ call, std::make_unique<gpu::RegisterFile>(registers) });
    }
    void Resolve(const gpu::RegisterFile&) { g_resolves++; }
    void Swap(uint32_t frontBuffer) { g_swaps.push_back(frontBuffer); }
    void BeforeCompletion() { g_completions++; }
    void Idle() { g_idles++; }
    void BeginOcclusionQuery() {}
    bool EndOcclusionQuery(uint32_t, uint64_t&) { return false; }
    void CollectOcclusionQueries(void (*)(uint32_t, uint64_t)) {}
    uint64_t CompletedSubmission() { return 0; }
}

namespace gpu
{
    uint64_t FrameCount() { return 0; }
    namespace watch
    {
        bool Enabled() { return false; }
        uint64_t Track(uint32_t, uint32_t, uint64_t (*)(), uint64_t) { return 0; }
        bool Unwritten(uint32_t, uint32_t, uint64_t) { return false; }
    }
    namespace detail
    {
        bool Running() { g_runningChecks++; return g_running; }
        void RaiseCommandProcessorInterrupt(uint32_t mask) { g_interrupts.push_back(mask); }
        bool ArenaProgress(uint32_t address, uint32_t value)
        {
            if (address != kArenaAddress) return false;
            g_arena.push_back({ address, value });
            return true;
        }
#if MW2_DIAGNOSTICS
        void CensusDraw(const RegisterFile&, uint32_t, uint32_t, bool, EdramMode) {}
        void CensusSwap() {}
        bool DumpingTextures() { return false; }
        void DumpBoundTextures(const RegisterFile&) {}
#endif
    }
}

namespace pacing { void Note(Event, unsigned) {} }
namespace imagepool
{
    bool CopiesWaiting() { return false; }
    void AtDraw(uint32_t, uint64_t) {}
    void AtFrameEnd(uint64_t) {}
}

// Guest physical addresses sit at a fixed offset from the fake physical heap.
namespace kernel
{
    uint32_t FromPhysical(uint32_t physical)
    {
        return physical < 0x00600000 ? fake::kPhysicalBase + physical : 0;
    }
}

// ---- building and running PM4 streams ------------------------------------------
namespace
{
    using Words = std::vector<uint32_t>;
    constexpr uint32_t kRing = 0x00F00000;
    constexpr uint32_t PM4_NOP = 0x10, PM4_IB = 0x3F, PM4_WAIT_IDLE = 0x26, PM4_WAIT_REG_MEM = 0x3C,
                       PM4_REG_TO_MEM = 0x3E, PM4_MEM_WRITE = 0x3D, PM4_COND_WRITE = 0x45,
                       PM4_EVENT_SHD = 0x58, PM4_EVENT_CFL = 0x59, PM4_DRAW_INDX = 0x22,
                       PM4_DRAW_INDX_2 = 0x36, PM4_SET_CONSTANT = 0x2D, PM4_SET_CONSTANT2 = 0x55,
                       PM4_IM_LOAD = 0x27, PM4_IM_LOAD_IMMEDIATE = 0x2B, PM4_SET_BIN_MASK = 0x50,
                       PM4_SET_BIN_SELECT = 0x51, PM4_SET_BIN_MASK_LO = 0x60, PM4_SET_BIN_MASK_HI = 0x61,
                       PM4_SET_BIN_SELECT_LO = 0x62, PM4_INTERRUPT = 0x54, PM4_XE_SWAP = 0x64;

    uint32_t Type0(uint32_t base, uint32_t count, bool oneReg = false)
    {
        return ((count - 1) << 16) | base | (oneReg ? 0x8000u : 0u);
    }
    uint32_t Type1(uint32_t first, uint32_t second) { return (1u << 30) | (second << 11) | first; }
    uint32_t Type2() { return 2u << 30; }
    uint32_t Type3(uint32_t opcode, uint32_t payload, bool predicated = false)
    {
        return (3u << 30) | ((payload - 1) << 16) | (opcode << 8) | (predicated ? 1u : 0u);
    }
    void Append(Words& to, std::initializer_list<uint32_t> w) { to.insert(to.end(), w); }

    // A draw with auto-generated indices (the shape MW2's draws take).
    Words AutoDraw(uint32_t primitive, uint32_t indices)
    {
        return { Type3(PM4_DRAW_INDX_2, 1), primitive | (2u << 6) | (indices << 16) };
    }

    void Put(uint32_t va, const Words& w)
    {
        for (size_t i = 0; i < w.size(); i++) *GuestPtr<be32>(va + uint32_t(i) * 4) = w[i];
    }
    uint32_t VaOf(uint32_t physical) { return kernel::FromPhysical(physical); }
    uint32_t Mem(uint32_t physical) { return *GuestPtr<be32>(VaOf(physical)); }
    void SetMem(uint32_t physical, uint32_t v) { *GuestPtr<be32>(VaOf(physical)) = v; }
    void PutPhysical(uint32_t physical, const Words& w) { Put(VaOf(physical), w); }

    void Run(const Words& stream)
    {
        Put(kRing, stream);
        gpu::detail::ExecuteRing(kRing, 0, 0, uint32_t(stream.size()));
    }

    gpu::RegisterFile& Registers() { return *g_draws.back().registers; }

    // Run a stream and a draw after it, and read the registers as that draw saw them.
    gpu::RegisterFile& RunThenSee(Words stream)
    {
        const size_t before = g_draws.size();
        for (uint32_t w : AutoDraw(4, 3)) stream.push_back(w);
        Run(stream);
        assert(g_draws.size() == before + 1);
        return Registers();
    }

    // ---- the tests ---------------------------------------------------------------
    void RegisterWrites()
    {
        // Type 0: a run of registers from a base, in the ordinary register space.
        Words s = { Type0(0x2100, 3), 0x11, 0x22, 0x33 };
        auto& r = RunThenSee(s);
        assert(r[0x2100] == 0x11 && r[0x2101] == 0x22 && r[0x2102] == 0x33);

        // ONE_REG_WRITE feeds a register the same index over and over: the last wins.
        r = RunThenSee({ Type0(0x2110, 3, true), 1, 2, 3 });
        assert(r[0x2110] == 3 && r[0x2111] == 0);

        // Type 1 carries two indices in its header.
        auto& r1 = RunThenSee({ Type1(0x120, 0x121), 0xAA, 0xBB });
        assert(r1[0x120] == 0xAA && r1[0x121] == 0xBB);

        // Type 2 and a zero dword are filler: nothing after them is swallowed.
        auto& r2 = RunThenSee({ Type2(), 0, 0, Type0(0x2130, 1), 0x77 });
        assert(r2[0x2130] == 0x77);

        // A write past the register file is dropped without disturbing the stream.
        auto& r3 = RunThenSee({ Type0(0x7000, 2), 5, 6, Type0(0x2131, 1), 0x78 });
        assert(r3[0x2131] == 0x78);
    }

    void Constants()
    {
        const uint32_t alu = gpu::kConstantBaseAlu, fetch = gpu::kConstantBaseFetch,
                       boolean = gpu::kConstantBaseBool, loop = gpu::kConstantBaseLoop,
                       ordinary = gpu::kConstantBaseRegisters;
        // SET_CONSTANT names a window by type and an index within it.
        auto& r = RunThenSee({ Type3(PM4_SET_CONSTANT, 5), (0u << 16) | 4, 1, 2, 3, 4,
                               Type3(PM4_SET_CONSTANT, 3), (1u << 16) | 6, 0x50, 0x51,
                               Type3(PM4_SET_CONSTANT, 2), (2u << 16) | 1, 0x60,
                               Type3(PM4_SET_CONSTANT, 2), (3u << 16) | 2, 0x70,
                               Type3(PM4_SET_CONSTANT, 2), (4u << 16) | 0x20, 0x80 });
        for (uint32_t i = 0; i < 4; i++) assert(r[alu + 4 + i] == i + 1);
        assert(r[fetch + 6] == 0x50 && r[fetch + 7] == 0x51);
        assert(r[boolean + 1] == 0x60 && r[loop + 2] == 0x70 && r[ordinary + 0x20] == 0x80);

        // A window type that does not exist is ignored, and what follows still runs.
        auto& r2 = RunThenSee({ Type3(PM4_SET_CONSTANT, 2), (9u << 16) | 0, 0xDEAD,
                                Type0(0x2140, 1), 0x99 });
        assert(r2[0x2140] == 0x99);

        // SET_CONSTANT2 takes an absolute register.
        auto& r3 = RunThenSee({ Type3(PM4_SET_CONSTANT2, 3), 0x2150, 0xC1, 0xC2 });
        assert(r3[0x2150] == 0xC1 && r3[0x2151] == 0xC2);

        // A long run crosses the block boundary the register file copies in.
        Words big = { Type3(PM4_SET_CONSTANT, 301), (0u << 16) | 0 };
        for (uint32_t i = 0; i < 300; i++) big.push_back(0x1000 + i);
        auto& r4 = RunThenSee(big);
        for (uint32_t i = 0; i < 300; i += 37) assert(r4[alu + i] == 0x1000 + i);
        assert(r4[alu + 299] == 0x1000 + 299);

        // LOAD_ALU_CONSTANT reads the values from memory, big-endian in the guest.
        PutPhysical(0x20000, { 0xAAAA0001, 0xAAAA0002, 0xAAAA0003 });
        auto& r5 = RunThenSee({ Type3(PM4_SET_CONSTANT, 1), 0,   // a no-op to keep the shape
                                Type3(PM4_NOP, 1), 0,
                                Type3(0x2F, 3), 0x20000, (0u << 16) | 10, 3 });
        assert(r5[alu + 10] == 0xAAAA0001 && r5[alu + 12] == 0xAAAA0003);
    }

    void Draws()
    {
        const size_t before = g_draws.size();
        Run(AutoDraw(5, 12));
        assert(g_draws.size() == before + 1);
        const auto& call = g_draws.back().call;
        assert(call.primitive == 5 && call.indexCount == 12 && !call.indexed && call.indexAddress == 0);
        assert(Registers()[gpu::VGT_DRAW_INITIATOR] == (5u | (2u << 6) | (12u << 16)));

        // An indexed draw carries a DMA base and size; the address handed on is the guest's.
        Run({ Type3(PM4_DRAW_INDX_2, 3), 4u | (0u << 6) | (1u << 11) | (6u << 16), 0x30000, 6 });
        const auto& idx = g_draws.back().call;
        assert(idx.indexed && idx.index32 && idx.indexCount == 6);
        assert(idx.indexAddress == fake::kPhysicalBase + 0x30000);

        // DRAW_INDX has a visibility-query token first.
        Run({ Type3(PM4_DRAW_INDX, 2), 0xFEED, 1u | (2u << 6) | (3u << 16) });
        assert(g_draws.back().call.primitive == 1 && g_draws.back().call.indexCount == 3);

        // In copy mode a draw is a resolve, not a draw.
        const size_t draws = g_draws.size();
        const int resolves = g_resolves;
        Run({ Type0(gpu::RB_MODECONTROL, 1), uint32_t(gpu::EdramMode::Copy) });
        Run(AutoDraw(6, 3));
        assert(g_resolves == resolves + 1 && g_draws.size() == draws);
        Run({ Type0(gpu::RB_MODECONTROL, 1), uint32_t(gpu::EdramMode::ColorDepth) });
    }

    void Shaders()
    {
        // IM_LOAD_IMMEDIATE: the microcode is in the packet; the draw gets it and its hash.
        const Words vs = { 0x10, 0x20, 0x30, 0x40 }, ps = { 0xA0, 0xB0 };
        Words s = { Type3(PM4_IM_LOAD_IMMEDIATE, 6), 0, (0u << 16) | 4 };
        s.insert(s.end(), vs.begin(), vs.end());
        Append(s, { Type3(PM4_IM_LOAD_IMMEDIATE, 4), 1, (0u << 16) | 2 });
        s.insert(s.end(), ps.begin(), ps.end());
        RunThenSee(s);
        const auto& call = g_draws.back().call;
        assert(call.vertexWords == 4 && call.pixelWords == 2);
        assert(call.vertexHash == vk::renderer::ShaderHash(vs.data(), vs.size()));
        assert(call.pixelHash == vk::renderer::ShaderHash(ps.data(), ps.size()));
        assert(call.vertexCode[3] == 0x40 && call.pixelCode[1] == 0xB0);

        // A packet whose own length disagrees with itself is refused; the old shader stays.
        Run({ Type3(PM4_IM_LOAD_IMMEDIATE, 5), 0, (0u << 16) | 9, 1, 2, 3 });
        Run(AutoDraw(4, 3));
        assert(g_draws.back().call.vertexWords == 4);

        // IM_LOAD reads the microcode from memory; the low bit of the address picks the stage.
        PutPhysical(0x21000, { 0xD1, 0xD2, 0xD3 });
        PutPhysical(0x22000, { 0xE1, 0xE2 });
        Run({ Type3(PM4_IM_LOAD, 2), 0x21000 | 0, (0u << 16) | 3,
              Type3(PM4_IM_LOAD, 2), 0x22000 | 1, (0u << 16) | 2 });
        Run(AutoDraw(4, 3));
        const auto& loaded = g_draws.back().call;
        assert(loaded.vertexWords == 3 && loaded.vertexCode[0] == 0xD1 && loaded.vertexCode[2] == 0xD3);
        assert(loaded.pixelWords == 2 && loaded.pixelCode[1] == 0xE2);
        assert(loaded.vertexHash != 0 && loaded.vertexHash != loaded.pixelHash);

        // An empty or oversized load changes nothing.
        Run({ Type3(PM4_IM_LOAD, 2), 0x21000, 0, Type3(PM4_IM_LOAD, 2), 0x21000, (0u << 16) | 0x20000 });
        Run(AutoDraw(4, 3));
        assert(g_draws.back().call.vertexWords == 3);
    }

    void MemoryWrites()
    {
        Run({ Type3(PM4_MEM_WRITE, 4), 0x40000, 0x11, 0x22, 0x33 });
        assert(Mem(0x40000) == 0x11 && Mem(0x40004) == 0x22 && Mem(0x40008) == 0x33);

        const int completions = g_completions;
        Run({ Type3(PM4_EVENT_SHD, 3), 0, 0x41000, 0xABCD });
        assert(Mem(0x41000) == 0xABCD);
        assert(g_completions == completions + 1);   // the title is told the draws before it finished

        Run({ Type3(PM4_EVENT_CFL, 3), 0, 0x41004, 0xCF });
        assert(Mem(0x41004) == 0xCF);

        // An event write aimed at the arena's progress block goes there, not to memory.
        SetMem(kArenaAddress, 0x5555);
        const size_t arena = g_arena.size();
        Run({ Type3(PM4_EVENT_SHD, 3), 0, kArenaAddress, 0x1234 });
        assert(g_arena.size() == arena + 1 && g_arena.back().second == 0x1234);
        assert(Mem(kArenaAddress) == 0x5555);

        // REG_TO_MEM copies a register out.
        Run({ Type0(0x2160, 1), 0xFACE, Type3(PM4_REG_TO_MEM, 2), 0x2160, 0x42000 });
        assert(Mem(0x42000) == 0xFACE);

        // COND_WRITE in memory mode writes; in register mode it does not.
        Run({ Type3(PM4_COND_WRITE, 6), 0x000, 0, 0, 0, 0x43000, 0x01 });
        assert(Mem(0x43000) == 0x01);
        Run({ Type3(PM4_COND_WRITE, 6), 0x100, 0, 0, 0, 0x43004, 0x02 });
        assert(Mem(0x43004) == 0);

        // Writes are word-aligned, and an address the GPU cannot reach is dropped.
        Run({ Type3(PM4_MEM_WRITE, 2), 0x44003, 0x99 });
        assert(Mem(0x44000) == 0x99);
        Run({ Type3(PM4_MEM_WRITE, 2), 0x00700000, 0x99, Type3(PM4_MEM_WRITE, 2), 0x45000, 0x98 });
        assert(Mem(0x45000) == 0x98);
    }

    void ScratchWriteBack()
    {
        // A scratch register whose bit is set in SCRATCH_UMSK lands in memory at
        // SCRATCH_ADDR + 4n: how the title learns the GPU got this far.
        Run({ Type0(gpu::detail::kScratchUmsk, 1), 0x6, Type0(gpu::detail::kScratchAddr, 1), 0x46000 });
        Run({ Type0(gpu::detail::kScratchReg0 + 1, 1), 0x111,
              Type0(gpu::detail::kScratchReg0 + 2, 1), 0x222,
              Type0(gpu::detail::kScratchReg0 + 0, 1), 0x333 });
        assert(Mem(0x46004) == 0x111 && Mem(0x46008) == 0x222);
        assert(Mem(0x46000) == 0);                   // bit 0 is not in the mask
    }

    void Waits()
    {
        constexpr uint32_t kMemory = 0x10;
        const uint32_t address = 0x47000;
        SetMem(address, 10);
        auto Wait = [&](uint32_t function, uint32_t reference, uint32_t mask = 0xFFFFFFFF,
                        uint32_t low = 2) {
            // Address bit 1 asks for a byte swap of what the host reads, which is the guest's value.
            Run({ Type3(PM4_WAIT_REG_MEM, 5), function | kMemory, address | low, reference, mask, 1,
                  Type3(PM4_MEM_WRITE, 2), 0x47100, 0xD0 });
        };

        // Satisfied at once: the consumer never has to look at whether it is running.
        g_running = true;
        for (uint32_t function : { 3u, 2u, 5u, 7u })
        {
            const int checks = g_runningChecks;
            SetMem(0x47100, 0);
            Wait(function, 10);
            assert(g_runningChecks == checks && Mem(0x47100) == 0xD0);
        }
        // Not satisfied, with the GPU stopped: it does not hang, and the stream carries on.
        g_running = false;
        for (uint32_t function : { 0u, 1u, 4u, 6u })
        {
            const int checks = g_runningChecks;
            SetMem(0x47100, 0);
            Wait(function, 10);
            assert(g_runningChecks > checks && Mem(0x47100) == 0xD0);
        }
        // The mask applies before the compare.
        g_running = false;
        int checks = g_runningChecks;
        Wait(3, 2, 0x0F);                            // 10 & 0xF = 10, not 2
        assert(g_runningChecks > checks);
        checks = g_runningChecks;
        Wait(3, 2, 0x03);                            // 10 & 3 = 2
        assert(g_runningChecks == checks);

        // The other byte-swap selections read the same bytes four ways.
        SetMem(address, 0x01020304);
        checks = g_runningChecks;
        Wait(3, 0x01020304, 0xFFFFFFFF, 2);
        assert(g_runningChecks == checks);
        Wait(3, 0x04030201, 0xFFFFFFFF, 0);          // no swap: the host's view of the bytes
        assert(g_runningChecks == checks);
        Wait(3, 0x03040102, 0xFFFFFFFF, 1);          // the two halves exchanged
        assert(g_runningChecks == checks);
        Wait(3, 0x02010403, 0xFFFFFFFF, 3);          // the bytes swapped within each half
        assert(g_runningChecks == checks);

        // While the GPU runs, the stream waits for the memory to change, then goes on.
        g_running = true;
        SetMem(address, 0);
        SetMem(0x47100, 0);
        std::atomic<bool> done{ false };
        std::thread consumer([&] {
            Run({ Type3(PM4_WAIT_REG_MEM, 5), 3u | kMemory, address | 2, 0x77, 0xFFFFFFFF, 1,
                  Type3(PM4_MEM_WRITE, 2), 0x47100, 0xD1 });
            done = true;
        });
        std::this_thread::sleep_for(std::chrono::milliseconds(120));
        assert(!done && Mem(0x47100) == 0);          // still waiting, nothing after it ran
        SetMem(address, 0x77);
        consumer.join();
        assert(done && Mem(0x47100) == 0xD1);
        assert(g_idles > 0);                         // it kept the renderer moving while it waited

        // A wait on an unreachable address does not wait at all.
        g_running = true;
        Run({ Type3(PM4_WAIT_REG_MEM, 5), 3u | kMemory, 0x00700000, 1, 0xFFFFFFFF, 1,
              Type3(PM4_MEM_WRITE, 2), 0x47100, 0xD2 });
        assert(Mem(0x47100) == 0xD2);
    }

    void Predication()
    {
        // A predicated packet runs only when the bin select and mask share a bit.
        SetMem(0x48000, 0);
        SetMem(0x48004, 0);
        Run({ Type3(PM4_SET_BIN_SELECT, 2), 0, 0x1, Type3(PM4_SET_BIN_MASK, 2), 0, 0x2,
              Type3(PM4_MEM_WRITE, 2, true), 0x48000, 0xAA });
        assert(Mem(0x48000) == 0);                    // 1 & 2 share nothing: skipped
        Run({ Type3(PM4_SET_BIN_MASK_LO, 1), 0x3,
              Type3(PM4_MEM_WRITE, 2, true), 0x48000, 0xAB });
        assert(Mem(0x48000) == 0xAB);                 // 1 & 3: runs
        // The upper halves count too.
        Run({ Type3(PM4_SET_BIN_SELECT, 2), 0x4, 0x0, Type3(PM4_SET_BIN_MASK, 2), 0x4, 0x0,
              Type3(PM4_MEM_WRITE, 2, true), 0x48004, 0xAC });
        assert(Mem(0x48004) == 0xAC);
        // A packet that is not predicated is unaffected, and a predicated swap never runs.
        Run({ Type3(PM4_SET_BIN_SELECT_LO, 1), 0, Type3(PM4_SET_BIN_MASK_HI, 1), 0,
              Type3(PM4_MEM_WRITE, 2), 0x48008, 0xAD });
        assert(Mem(0x48008) == 0xAD);
        const size_t swaps = g_swaps.size();
        Run({ Type3(PM4_SET_BIN_SELECT, 2), 0xFFFFFFFF, 0xFFFFFFFF, Type3(PM4_SET_BIN_MASK, 2),
              0xFFFFFFFF, 0xFFFFFFFF, Type3(PM4_XE_SWAP, 4, true), 0x53574150, 0x12, 1280, 720 });
        assert(g_swaps.size() == swaps);
        Run({ Type3(PM4_XE_SWAP, 4), 0x53574150, 0x12, 1280, 720 });
        assert(g_swaps.size() == swaps + 1 && g_swaps.back() == 0x12);
    }

    void IdleAndInterrupts()
    {
        const size_t interrupts = g_interrupts.size();
        int completions = g_completions;
        Run({ Type3(PM4_INTERRUPT, 1), 0x4 });
        assert(g_interrupts.size() == interrupts + 1 && g_interrupts.back() == 0x4);
        assert(g_completions == completions);         // no idle since a draw: nothing to announce

        // After a wait for idle with no draw behind it, the title is told before the interrupt.
        Run({ Type3(PM4_WAIT_IDLE, 1), 0, Type3(PM4_INTERRUPT, 1), 0x1 });
        assert(g_completions == completions + 1 && g_interrupts.back() == 0x1);

        // A draw ends the idle.
        completions = g_completions;
        Run({ Type3(PM4_WAIT_IDLE, 1), 0, AutoDraw(4, 3)[0], AutoDraw(4, 3)[1],
              Type3(PM4_INTERRUPT, 1), 0x2 });
        assert(g_completions == completions);
    }

    void IndirectBuffers()
    {
        // An indirect buffer is another stream in memory, run where it is named.
        PutPhysical(0x50000, { Type3(PM4_MEM_WRITE, 2), 0x49000, 0x1B });
        Run({ Type3(PM4_IB, 2), 0x50000, 3 });
        assert(Mem(0x49000) == 0x1B);

        // Misaligned, oversized and empty buffers are not run.
        for (uint32_t i = 0; i < 4; i++) SetMem(0x49010 + i * 4, 0);
        PutPhysical(0x50020, { Type3(PM4_MEM_WRITE, 2), 0x49010, 0x1C });
        PutPhysical(0x50040, { Type3(PM4_MEM_WRITE, 2), 0x49014, 0x1D });
        Run({ Type3(PM4_IB, 2), 0x50024, 3,          // not 32-byte aligned
              Type3(PM4_IB, 2), 0x50040, 20000,      // far longer than a real buffer
              Type3(PM4_IB, 2), 0x50040, 0 });       // nothing in it
        assert(Mem(0x49010) == 0 && Mem(0x49014) == 0);

        // Buffers nest, but not without limit: the sixth level down is not run.
        for (uint32_t level = 1; level <= 6; level++)
        {
            Words w = { Type3(PM4_MEM_WRITE, 2), 0x4A000 + level * 4, 0xB0 + level };
            if (level < 6) Append(w, { Type3(PM4_IB, 2), 0x51000 + (level + 1) * 0x100, 6 });
            PutPhysical(0x51000 + level * 0x100, w);
            SetMem(0x4A000 + level * 4, 0);
        }
        Run({ Type3(PM4_IB, 2), 0x51100, 6 });
        for (uint32_t level = 1; level <= 5; level++) assert(Mem(0x4A000 + level * 4) == 0xB0 + level);
        assert(Mem(0x4A000 + 6 * 4) == 0);
    }

    void BadPackets()
    {
        // An opcode the processor does not know is skipped by its length.
        SetMem(0x4B000, 0);
        Run({ Type3(0x7E, 3), 1, 2, 3, Type3(PM4_MEM_WRITE, 2), 0x4B000, 0x6E });
        assert(Mem(0x4B000) == 0x6E);

        // A packet that claims more than the buffer holds ends the buffer.
        SetMem(0x4B004, 0);
        PutPhysical(0x52000, { Type3(PM4_MEM_WRITE, 2), 0x4B004, 0x6F,
                               Type3(PM4_MEM_WRITE, 200), 0x4B008, 1 });
        Run({ Type3(PM4_IB, 2), 0x52000, 5, Type3(PM4_MEM_WRITE, 2), 0x4B00C, 0x70 });
        assert(Mem(0x4B004) == 0x6F);                // the good packet before it ran
        assert(Mem(0x4B008) == 0);                   // the bad one did not
        assert(Mem(0x4B00C) == 0x70);                // and the ring carried on

        // A handler that wants more than its packet carries leaves the stream aligned.
        SetMem(0x4B010, 0);
        Run({ Type3(PM4_MEM_WRITE, 1), 0x4B010, Type3(PM4_MEM_WRITE, 2), 0x4B010, 0x71 });
        assert(Mem(0x4B010) == 0x71);
    }

    void WrappingRing()
    {
        // The ring is a power of two dwords and a read can start anywhere in it.
        // This stream starts at dword 12 of 16 and runs round the end.
        const uint32_t ring = kRing + 0x1000, mask = 15;
        const uint32_t alu = gpu::kConstantBaseAlu;
        Words stream = { Type3(PM4_SET_CONSTANT, 5), (0u << 16) | 40, 0xC0, 0xC1, 0xC2, 0xC3,
                         AutoDraw(4, 3)[0], AutoDraw(4, 3)[1] };
        for (size_t i = 0; i < stream.size(); i++)
            *GuestPtr<be32>(ring + uint32_t((12 + i) & mask) * 4) = stream[i];
        const size_t before = g_draws.size();
        gpu::detail::ExecuteRing(ring, 12, mask, uint32_t(stream.size()));
        assert(g_draws.size() == before + 1);
        const auto& r = Registers();
        assert(r[alu + 40] == 0xC0 && r[alu + 41] == 0xC1 && r[alu + 42] == 0xC2 && r[alu + 43] == 0xC3);
    }

    void DisplayColourTable()
    {
        uint32_t table[256];
        assert(!gpu::DisplayColourTable(table));       // nothing until a whole ramp is written

        // The title points the index at entry 0 and streams 256 colours into one register.
        Words s = { Type0(gpu::DC_LUT_WRITE_EN_MASK, 1), 7, Type0(gpu::DC_LUT_RW_INDEX, 1), 0,
                    Type0(gpu::DC_LUT_30_COLOR, 200, true) };
        for (uint32_t i = 0; i < 200; i++) s.push_back((i << 20) | (i << 10) | i);
        Run(s);
        assert(!gpu::DisplayColourTable(table));       // half a sweep is not a ramp

        Words rest = { Type0(gpu::DC_LUT_30_COLOR, 56, true) };
        for (uint32_t i = 200; i < 256; i++) rest.push_back((i << 20) | (i << 10) | i);
        Run(rest);
        assert(gpu::DisplayColourTable(table));
        for (uint32_t i = 0; i < 256; i++) assert(table[i] == ((i << 20) | (i << 10) | i));
    }
}

int main()
{
    RegisterWrites();
    Constants();
    Draws();
    Shaders();
    MemoryWrites();
    ScratchWriteBack();
    Waits();
    Predication();
    IdleAndInterrupts();
    IndirectBuffers();
    BadPackets();
    WrappingRing();
    DisplayColourTable();
    std::puts("gpu-command-processor-test ok");
    std::fflush(stdout);
    return 0;
}
