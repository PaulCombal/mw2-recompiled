#include "kernel.h"
#include "title.h"
#include "../guest.h"
#include "../diagnostics.h"
#include "../log.h"
#include <map>
#include <mutex>
#include <vector>
#include <chrono>
#include <atomic>
#include <algorithm>
#include <cstring>
#include <random>
#include <utility>

// The physical heap's top, which physical.h reads inline.
extern "C" uint32_t mw2_kernel_physical_top = guest::kPhysicalBase;

namespace
{
    // A range of guest address space. Freed blocks are kept, merged with
    // their neighbours and handed out again first-fit, and zeroed when they
    // are, as the console's pages come; the rest is carved off the top, which
    // only rises, so what lies above it has never been used and is zero.
    struct Heap
    {
        const char* name;
        const uint32_t end;
        std::atomic_ref<uint32_t> top;
        std::mutex lock;
        std::map<uint32_t, uint32_t> live, free;   // address -> size
        uint64_t allocations = 0, reused = 0, liveBytes = 0;

        Heap(const char* heapName, uint32_t& topWord, uint32_t limit)
            : name(heapName), end(limit), top(topWord) {}

        uint32_t Alloc(uint32_t size, uint32_t align)
        {
            if (!size) return 0;
            std::lock_guard held(lock);
            allocations++;
            for (auto it = free.begin(); it != free.end(); ++it)
            {
                const uint32_t start = it->first, length = it->second;
                const uint32_t at = (start + align - 1) & ~(align - 1);
                if (uint64_t(at) + size > uint64_t(start) + length) continue;
                free.erase(it);
                if (at > start) free[start] = at - start;
                if (at + size < start + length) free[at + size] = start + length - (at + size);
                std::memset(guest::Base() + at, 0, size);
                reused++;
                return Took(at, size);
            }
            const uint32_t from = top.load(std::memory_order_relaxed);
            const uint32_t at = (from + align - 1) & ~(align - 1);
            if (uint64_t(at) + size > end) return 0;
            if (at > from) free[from] = at - from;
            top.store(at + size, std::memory_order_release);
            return Took(at, size);
        }

        uint32_t Took(uint32_t at, uint32_t size)
        {
            live[at] = size;
            liveBytes += size;
            return at;
        }

        bool Free(uint32_t address)
        {
            std::lock_guard held(lock);
            auto it = live.find(address);
            if (it == live.end()) return false;
            uint32_t start = address, length = it->second;
            liveBytes -= length;
            live.erase(it);
            if (auto after = free.find(start + length); after != free.end())
            {
                length += after->second;
                free.erase(after);
            }
            if (auto before = free.lower_bound(start); before != free.begin())
                if (--before; before->first + before->second == start)
                {
                    start = before->first;
                    length += before->second;
                    free.erase(before);
                }
            free[start] = length;
            return true;
        }

        uint32_t HighWater() const { return top.load(std::memory_order_acquire); }

        void Report()
        {
            std::lock_guard held(lock);
            uint64_t freeBytes = 0;
            for (const auto& [at, size] : free) freeBytes += size;
            LOGI("memory: %s heap %llu MB in use, %llu MB free below the high water of %08X;"
                 " %llu allocations, %llu of them into freed space",
                 name, (unsigned long long)(liveBytes >> 20), (unsigned long long)(freeBytes >> 20),
                 HighWater(), (unsigned long long)allocations, (unsigned long long)reused);
        }
    };

    uint32_t g_heapTop = guest::kHeapBase;
    Heap g_heap{ "virtual", g_heapTop, guest::kHeapEnd };
    Heap g_phys{ "physical", mw2_kernel_physical_top, guest::kPhysicalEnd };

    struct TlsTemplate { uint32_t address = 0, size = 0; };
    TlsTemplate g_tlsTemplate;

    std::mutex g_unimplLock;
    std::map<std::string, uint64_t> g_unimpl;
}

namespace
{
    // IMAGE_TLS_DIRECTORY32, big-endian, located through the PE data directory.
    void ReadTlsDirectory()
    {
        const uint8_t* image = guest::Base() + PPC_IMAGE_BASE;
        uint32_t peOffset;
        std::memcpy(&peOffset, image + 0x3C, 4);          // e_lfanew, little-endian
        if (peOffset + 0x100 > PPC_IMAGE_SIZE) return;

        uint16_t optionalMagic;
        std::memcpy(&optionalMagic, image + peOffset + 24, 2);
        const uint32_t directories = peOffset + 24 + (optionalMagic == 0x10B ? 96 : 112);

        uint32_t tlsRva, tlsSize;
        std::memcpy(&tlsRva, image + directories + 9 * 8, 4);
        std::memcpy(&tlsSize, image + directories + 9 * 8 + 4, 4);
        if (!tlsRva || tlsSize < 24) return;

        // Guest-endian: raw data start/end, then index.
        const be32* entry = reinterpret_cast<const be32*>(image + tlsRva);
        uint32_t rawStart = entry[0], rawEnd = entry[1], zeroFill = entry[4];
        if (rawEnd <= rawStart) return;

        g_tlsTemplate.address = rawStart;
        g_tlsTemplate.size = (rawEnd - rawStart) + zeroFill;
    }
}

const TlsTemplate& ImageTlsTemplate() { return g_tlsTemplate; }

namespace
{
    void InstallTimeStampBundle();
    void InstallDebugMonitorData();
}

extern "C" void mw2_kernel_initialise(const char* gameRoot)
{
    ReadTlsDirectory();
    InstallTimeStampBundle();
    InstallDebugMonitorData();
    const std::filesystem::path root(std::u8string(reinterpret_cast<const char8_t*>(gameRoot)));
    LOGI("game root: %s%s", gameRoot, std::filesystem::exists(root) ? "" : "  (does not exist)");
}

extern "C" uint32_t mw2_kernel_allocate_guest(uint32_t size, uint32_t alignment) { return g_heap.Alloc(size, alignment); }
extern "C" uint32_t mw2_kernel_allocate_physical(uint32_t size, uint32_t alignment) { return g_phys.Alloc(size, alignment); }
extern "C" bool     mw2_kernel_free_guest(uint32_t address) { return g_heap.Free(address); }
extern "C" bool     mw2_kernel_free_physical(uint32_t address) { return g_phys.Free(address); }

extern "C" void mw2_kernel_report_memory()
{
    if constexpr (!diag::kOn) return;
    g_heap.Report();
    g_phys.Report();
}

namespace
{
    // X_TIME_STAMP_BUNDLE: interrupt time, system time, millisecond tick count.
    // GetTickCount loads the pointer and reads the third field, so an unresolved
    // pointer freezes the title's clock and every timeout built on it.
    struct TimeStampBundle
    {
        be64 interruptTime;
        be64 systemTime;
        be32 tickCount;
        be32 padding;
    };

    uint32_t g_timeStampBundle = 0;
    std::chrono::steady_clock::time_point g_bootTime;

    // The image word that holds the pointer is an unresolved import record rather
    // than an address, which is how it is recognised: the low half is an ordinal
    // and the high half a small type tag, so a real pointer cannot be mistaken
    // for one.
    constexpr uint32_t kTimeStampBundlePointer = T_DATA_TimeStampBundlePtr;

    bool LooksLikeUnresolvedImport(uint32_t value)
    {
        return (value >> 24) == 0 && (value & 0xFFFF) != 0 && value < 0x01000000;
    }

    void InstallTimeStampBundle()
    {
        uint32_t existing = *GuestPtr<be32>(kTimeStampBundlePointer);
        if (!LooksLikeUnresolvedImport(existing))
        {
            LOGW("time stamp bundle: %08X already holds %08X, not an unresolved import -- leaving it alone",
                 kTimeStampBundlePointer, existing);
            return;
        }

        g_timeStampBundle = kernel::AllocateGuest(sizeof(TimeStampBundle), 16);
        if (!g_timeStampBundle) { LOGE("could not allocate the time stamp bundle"); return; }
        std::memset(guest::Base() + g_timeStampBundle, 0, sizeof(TimeStampBundle));
        *GuestPtr<be32>(kTimeStampBundlePointer) = g_timeStampBundle;
        g_bootTime = std::chrono::steady_clock::now();
        kernel::UpdateTimeStampBundle();
    }

    // KeDebugMonitorData: a pointer to the debug monitor's dispatch table, null on
    // a retail console. XAudio2's mixer reads it on every pass and, when the word
    // it points at is not zero, calls a method through it -- so left as an import
    // record it points into the low guest space, and whatever the title happens
    // to have written there (handles handed back as object pointers live there)
    // becomes a call target.
    void InstallDebugMonitorData()
    {
        constexpr uint32_t kPointer = T_DATA_DebugMonitorPtr;
        constexpr uint32_t kOrdinal = 0x59;
        uint32_t existing = *GuestPtr<be32>(kPointer);
        if (!LooksLikeUnresolvedImport(existing) || (existing & 0xFFFF) != kOrdinal)
        {
            LOGW("debug monitor data: %08X holds %08X, not the import record -- leaving it alone", kPointer, existing);
            return;
        }
        uint32_t word = kernel::AllocateGuest(16, 16);
        if (!word) { LOGE("could not allocate the debug monitor word"); return; }
        *GuestPtr<be32>(word) = 0;
        *GuestPtr<be32>(kPointer) = word;
    }
}

extern "C" void mw2_kernel_update_time_stamp_bundle()
{
    if (!g_timeStampBundle) return;
    auto* bundle = GuestPtr<TimeStampBundle>(g_timeStampBundle);
    const auto elapsed = std::chrono::steady_clock::now() - g_bootTime;
    const uint64_t hundredNanoseconds =
        uint64_t(std::chrono::duration_cast<std::chrono::nanoseconds>(elapsed).count() / 100);
    bundle->interruptTime = hundredNanoseconds;
    bundle->systemTime = kernel::SystemTime100ns();
    bundle->tickCount = uint32_t(
        std::chrono::duration_cast<std::chrono::milliseconds>(elapsed).count());
}

extern "C" uint32_t mw2_kernel_create_thread_block(uint32_t threadId)
{
    // [block .. block+0x8000) is the KPCR, with r13 pointing 0x4000 in so the
    // negative r13-relative offsets the game uses stay inside it. The KTHREAD
    // follows, then this thread's copy of the image's TLS block.
    constexpr uint32_t kPcrSize     = 0x8000;
    constexpr uint32_t kPcrBias     = 0x4000;
    constexpr uint32_t kThreadSize  = 0x1000;
    constexpr uint32_t kTlsSize     = 0x200;
    constexpr uint32_t kTlsPointerOffset    = 0;     // KPCR.TlsData
    constexpr uint32_t kCurrentThreadOffset = 256;   // KPCR.CurrentThread
    constexpr uint32_t kThreadIdOffset      = 332;   // KTHREAD.ThreadId

    const uint32_t total = kPcrSize + kThreadSize + kTlsSize;
    uint32_t block = kernel::AllocateGuest(total, 0x1000);
    if (!block) return 0;
    std::memset(guest::Base() + block, 0, total);

    uint32_t pcr     = block + kPcrBias;
    uint32_t kthread = block + kPcrSize;
    uint32_t tls     = block + kPcrSize + kThreadSize;

    // The CRT hangs its per-thread scratch allocator off a slot in the TLS
    // template, reached as [[r13] + slot], so a null TlsData sends every thread to
    // the same imaginary pool at guest address 0 -- where they corrupt each other.
    const auto& tlsTemplate = ImageTlsTemplate();
    if (tlsTemplate.address && tlsTemplate.size && tlsTemplate.size <= kTlsSize)
        std::memcpy(guest::Base() + tls, guest::Base() + tlsTemplate.address, tlsTemplate.size);

    *GuestPtr<be32>(pcr + kTlsPointerOffset)    = tls;
    *GuestPtr<be32>(pcr + kCurrentThreadOffset) = kthread;
    *GuestPtr<be32>(kthread + kThreadIdOffset)  = threadId;
    return pcr;
}

extern "C" void mw2_kernel_unimplemented(const char* name)
{
    std::lock_guard g(g_unimplLock);
    auto [it, inserted] = g_unimpl.try_emplace(name, 0);
    it->second++;
    if (inserted) LOGW("unimplemented: %s", name);
}

extern "C" void mw2_kernel_report_unimplemented()
{
    if constexpr (!diag::kOn) return;
    std::lock_guard g(g_unimplLock);
    if (g_unimpl.empty()) { LOGI("no unimplemented imports were reached"); return; }
    std::vector<std::pair<std::string, uint64_t>> v(g_unimpl.begin(), g_unimpl.end());
    std::stable_sort(v.begin(), v.end(), [](auto& a, auto& b){ return a.second > b.second; });
    LOGI("unimplemented imports actually reached (%zu distinct):", v.size());
    for (auto& [name, count] : v) LOGI("  %8llu  %s", (unsigned long long)count, name.c_str());
}

extern "C" PPCFunc* mw2_kernel_guest_function(uint32_t address)
{
    if (address < PPC_CODE_BASE || address >= PPC_CODE_BASE + PPC_CODE_SIZE) return nullptr;
    return PPC_LOOKUP_FUNC(guest::Base(), address);
}

namespace
{
    // Borrows the caller's stack below its frame.
    void CallGuest(const PPCContext& caller, uint32_t address,
                   uint32_t a0 = 0, uint32_t a1 = 0, uint32_t a2 = 0)
    {
        PPCFunc* fn = kernel::GuestFunction(address);
        if (!fn) { LOGW("CallGuest: no recompiled function at %08X", address); return; }

        PPCContext ctx{};
        std::memset(&ctx, 0, sizeof(ctx));
        // Leave the caller's frame and its 64-byte red zone untouched.
        ctx.r1.u32  = (caller.r1.u32 - 0x400) & ~0xFu;
        ctx.r13.u32 = caller.r13.u32;
        ctx.r3.u32  = a0;
        ctx.r4.u32  = a1;
        ctx.r5.u32  = a2;
        // The caller's code keeps its own idea of the float mode (whether
        // denormals flush: on for vector code, off for scalar) and only switches
        // when it thinks the mode differs. The console keeps the two in separate
        // registers, FPSCR and VSCR; x86 has one, MXCSR, and the recompiled code
        // tracks which it last set. A routine run here in a context of its own can
        // leave MXCSR switched -- vector code turns flushing on -- and the caller,
        // believing it off, would flush every tiny value its scalar code makes from
        // then on. So the caller gets the register back as it left it.
        const uint32_t before = ctx.fpscr.getcsr();
        ctx.fpscr.loadFromHost();
        fn(ctx, guest::Base());
        if (ctx.fpscr.getcsr() != before) ctx.fpscr.setcsr(before);
    }

    // The C++'s work for an APC (kernel.h): run once, or else discarded with
    // the thread's queue.
    struct Work
    {
        void (*run)(void*) = nullptr;
        void (*discard)(void*) = nullptr;
        void* work = nullptr;
        Work() = default;
        Work(void (*r)(void*), void (*d)(void*), void* w) : run(r), discard(d), work(w) {}
        Work(Work&& other) noexcept : run(other.run), discard(other.discard), work(other.work) { other.work = nullptr; }
        Work& operator=(Work&& other) noexcept { std::swap(run, other.run); std::swap(discard, other.discard); std::swap(work, other.work); return *this; }
        ~Work() { if (work) discard(work); }
        void Run() { if (void* w = std::exchange(work, nullptr)) run(w); }
    };

    struct PendingApc
    {
        uint32_t routine, context, ioStatusBlock;
        Work work;
    };
    thread_local std::vector<PendingApc> t_apcs;
}

extern "C" void mw2_kernel_queue_user_apc(uint32_t routine, uint32_t context, uint32_t ioStatusBlock,
                                          void (*run)(void*), void (*discard)(void*), void* work)
{
    Work kept(run, discard, work);
    if (!routine) { kept.Run(); return; }
    t_apcs.push_back({ routine, context, ioStatusBlock, std::move(kept) });
}

extern "C" bool mw2_kernel_deliver_user_apcs(const PPCContext* caller)
{
    if (t_apcs.empty()) return false;
    // An APC may queue more work, so drain rather than iterate.
    std::vector<PendingApc> batch;
    batch.swap(t_apcs);
    for (auto& a : batch)
    {
        a.work.Run();
        CallGuest(*caller, a.routine, a.context, a.ioStatusBlock, 0);
    }
    return true;
}

void kernel::RandomBytes(void* out, size_t size)
{
    thread_local std::mt19937 rng{ std::random_device{}() };
    for (size_t i = 0; i < size; i++) static_cast<uint8_t*>(out)[i] = uint8_t(rng());
}
