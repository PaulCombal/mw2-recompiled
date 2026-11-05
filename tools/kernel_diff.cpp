// tools/kernel_diff.py's program: the kernel's C++ (runtime/kernel/) and its Rust
// (src/runtime/kernel/) given the same calls in one process, each on a guest space of
// its own: mw2_guest_base is switched to a side's before anything of it runs. Each side
// works in a folder of its own too, holding the same game files: files, saved games and
// the profile are compared as they are left on the disk.
//
// What one thread asks of the objects decides what it gets, so a seed is a script of
// cases -- creates, sets, resets, releases, closes, waits that end at once or soon,
// timers, critical sections, spin locks, TLS, memory, threads that are made, run and
// waited for, APCs, files, XAM's sessions, listeners and enumerators, and sockets -- run
// on one side and then the other with the same random draws. Every status, every word a call wrote, the guest memory, what
// the guest threads and APC routines saw, and the log must be the same. Nothing a script
// does may block for ever or race: a thread is made suspended and waited for once it
// runs, a timer is waited for only when it is due, and every other wait has a timeout.
//
// What many threads do together is not reproducible, so `stress` checks what must hold
// on each side -- no wake is lost, nothing is taken twice -- and `time` measures what a
// call and a wake cost on each.
//
// Both sides' XAM and network talk to one online service, the rig's (or to none, as a
// seed has it), which writes down what it is told and answers from the script: what
// arrives for a port, a search's results, an invitation. Without a service the title's
// sockets are the host's, on the loopback: a side's are closed at the end of each case,
// so that the other side can bind the same ports.
#include <ppc_context.h>
#include "title.h"
#include "kernel/kernel.h"
#include "kernel/objects.h"
#include "log.h"
#include "online/service.h"

#include <sys/mman.h>
#include <unistd.h>

#include <filesystem>
#include <fstream>
#include <map>

#include <algorithm>
#include <atomic>
#include <cstdarg>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <deque>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

using Import = void (*)(PPCContext&, uint8_t*);

#define KERNEL_IMPORTS(X)                                                                                          \
    X(NtCreateEvent) X(NtSetEvent) X(NtClearEvent) X(NtCreateMutant) X(NtReleaseMutant) X(NtCreateTimer)              \
    X(NtSetTimerEx) X(NtCancelTimer) X(NtWaitForSingleObjectEx) X(NtWaitForMultipleObjectsEx) X(KeSetEvent)           \
    X(KeResetEvent) X(KeWaitForSingleObject) X(KeWaitForMultipleObjects) X(KeInitializeSemaphore)                    \
    X(KeReleaseSemaphore) X(NtClose) X(NtDuplicateObject) X(ObReferenceObjectByHandle) X(ObDereferenceObject)        \
    X(ObCreateSymbolicLink) X(ObDeleteSymbolicLink) X(RtlInitializeCriticalSection)                                  \
    X(RtlInitializeCriticalSectionAndSpinCount) X(RtlEnterCriticalSection) X(RtlLeaveCriticalSection)                \
    X(RtlTryEnterCriticalSection) X(KeTlsAlloc) X(KeTlsFree) X(KeTlsGetValue) X(KeTlsSetValue)                        \
    X(KeEnterCriticalRegion) X(KeLeaveCriticalRegion) X(KeRaiseIrqlToDpcLevel) X(KfLowerIrql) X(KfAcquireSpinLock)  \
    X(KfReleaseSpinLock) X(KeAcquireSpinLockAtRaisedIrql) X(KeReleaseSpinLockFromRaisedIrql)                         \
    X(KeTryToAcquireSpinLockAtRaisedIrql) X(ExCreateThread) X(NtResumeThread) X(KeResumeThread) X(NtSuspendThread)  \
    X(ExTerminateThread) X(KeSetAffinityThread) X(KeSetBasePriorityThread) X(ExRegisterTitleTerminateNotification)    \
    X(NtAllocateVirtualMemory) X(NtFreeVirtualMemory) X(NtQueryVirtualMemory) X(MmAllocatePhysicalMemoryEx)          \
    X(MmFreePhysicalMemory) X(MmQueryAddressProtect) X(MmSetAddressProtect) X(MmQueryStatistics)                    \
    X(NtCreateFile) X(NtOpenFile) X(NtReadFile) X(NtWriteFile) X(NtQueryInformationFile)                              \
    X(NtQueryFullAttributesFile) X(NtSetInformationFile) X(NtFlushBuffersFile) X(NtQueryVolumeInformationFile)        \
    X(NtReadFileScatter) X(XamContentGetDeviceState) X(XamContentGetDeviceData) X(XamContentCreateEx)                 \
    X(XamContentClose) X(XamContentDelete) X(XamUserReadProfileSettings) X(XamUserWriteProfileSettings)              \
    X(KeGetCurrentProcessType) X(XexCheckExecutablePrivilege) X(KeQueryPerformanceFrequency) X(KeQuerySystemTime)     \
    X(KeDelayExecutionThread) X(XamLoaderGetLaunchData) X(XamLoaderSetLaunchData) X(XamGetSystemVersion)              \
    X(XexGetModuleHandle) X(XexGetProcedureAddress) X(XexLoadImage) X(XexUnloadImage) X(XGetAVPack) X(XGetLanguage)  \
    X(KeLockL2) X(KeUnlockL2) X(KiApcNormalRoutineNop) X(DbgPrint) X(RtlNtStatusToDosError) X(RtlInitAnsiString)     \
    X(RtlMultiByteToUnicodeN) X(RtlUnicodeToMultiByteN) X(RtlFillMemoryUlong) X(RtlCompareMemoryUlong)                \
    X(VdInitializeRingBuffer) X(VdEnableRingBufferRPtrWriteBack) X(VdSetGraphicsInterruptCallback)                    \
    X(VdInitializeEngines) X(VdShutdownEngines) X(VdQueryVideoMode) X(XGetVideoMode) X(VdQueryVideoFlags)             \
    X(VdGetCurrentDisplayInformation) X(VdGetCurrentDisplayGamma) X(VdSetDisplayMode) X(VdSwap)                       \
    X(VdGetSystemCommandBuffer) X(VdSetSystemCommandBufferGpuIdentifierAddress) X(VdCallGraphicsNotificationRoutines) \
    X(VdIsHSIOTrainingSucceeded) X(VdPersistDisplay) X(VdEnableDisableClockGating) X(VdRetrainEDRAM)                  \
    X(VdRetrainEDRAMWorker) X(VdInitializeScalerCommandBuffer) X(MmGetPhysicalAddress) X(XamInputGetState)            \
    X(XamInputGetCapabilities) X(XamInputSetState) X(NetDll_XNetGetTitleXnAddr) X(XNetLogonGetTitleID)                \
    X(NetDll_XNetStartup) X(NetDll_WSACleanup) X(NetDll_WSAGetLastError) X(NetDll_WSAStartup)                         \
    X(NetDll_XNetGetEthernetLinkStatus) X(NetDll_XNetRandom) X(NetDll_XNetXnAddrToInAddr) X(NetDll_XNetInAddrToXnAddr) \
    X(NetDll_XNetConnect) X(NetDll_XNetGetConnectStatus) X(NetDll_XNetUnregisterInAddr) X(NetDll_XNetServerToInAddr)  \
    X(NetDll_XNetXnAddrToMachineId) X(NetDll_XNetQosServiceLookup) X(NetDll_XNetQosLookup) X(NetDll_XNetQosRelease)   \
    X(NetDll_XNetQosListen) X(NetDll_socket) X(NetDll_closesocket) X(NetDll_bind) X(NetDll_connect)                   \
    X(NetDll_ioctlsocket) X(NetDll_setsockopt) X(NetDll_sendto) X(NetDll_recvfrom) X(NetDll_inet_addr)                \
    X(XamNotifyCreateListener) X(XNotifyGetNext) X(XNotifyPositionUI) X(XamUserGetSigninState) X(XamUserGetXUID)      \
    X(XamUserGetName) X(XamUserGetSigninInfo) X(XamUserCheckPrivilege) X(XamUserAreUsersFriends)                      \
    X(XamUserCreateStatsEnumerator) X(XamShowDeviceSelectorUI) X(XamShowMessageBoxUIEx)                               \
    X(XamShowGamerCardUIForXUID) X(XamShowDirtyDiscErrorUI) X(XamSessionCreateHandle) X(XamSessionRefObjByHandle)     \
    X(XMsgInProcessCall) X(XMsgStartIORequest) X(XMsgCancelIORequest) X(XamCreateEnumeratorHandle)                    \
    X(XamGetPrivateEnumStructureFromHandle) X(XamContentCreateEnumerator) X(XamEnumerate) X(XamAlloc) X(XamFree)      \
    X(ExGetXConfigSetting)

enum : int
{
#define X(name) k##name,
    KERNEL_IMPORTS(X)
#undef X
    kImportCount
};

const char* const kImportNames[] = {
#define X(name) #name,
    KERNEL_IMPORTS(X)
#undef X
};

extern "C"
{
#define X(name) void __imp__##name(PPCContext&, uint8_t*); void cpp___imp__##name(PPCContext&, uint8_t*);
    KERNEL_IMPORTS(X)
#undef X

    extern uint8_t* mw2_guest_base;
    extern uint32_t mw2_kernel_physical_top;
    extern uint32_t cpp_mw2_kernel_physical_top;

#define KERNEL_CALLS(X)                                                                                             \
    X(void, initialise, (const char*))                                                                             \
    X(uint32_t, allocate_guest, (uint32_t, uint32_t))                                                              \
    X(uint32_t, allocate_physical, (uint32_t, uint32_t))                                                           \
    X(bool, free_guest, (uint32_t))                                                                                \
    X(bool, free_physical, (uint32_t))                                                                             \
    X(void, report_memory, ())                                                                                     \
    X(uint32_t, create_thread_block, (uint32_t))                                                                   \
    X(uint32_t, new_thread_id, ())                                                                                 \
    X(void, update_time_stamp_bundle, ())                                                                          \
    X(PPCFunc*, guest_function, (uint32_t))                                                                        \
    X(void, queue_user_apc, (uint32_t, uint32_t, uint32_t, void (*)(void*), void (*)(void*), void*))               \
    X(bool, deliver_user_apcs, (const PPCContext*))                                                                \
    X(void, unimplemented, (const char*))                                                                          \
    X(void, report_unimplemented, ())                                                                              \
    X(void, report_waits, ())                                                                                      \
    X(void, report_missing_files, ())
#define X(type, name, arguments) type mw2_kernel_##name arguments; type cpp_mw2_kernel_##name arguments;
    KERNEL_CALLS(X)
#undef X

    // What the Rust runtime asks of the C++ one, which is not here.
    uint64_t mw2_kernel_system_time();
    bool mw2_watch_handle_fault(const void*) { return false; }
    uint64_t mw2_gpu_frame_count() { return 0; }
    void mw2_crash_on_fault() {}
    struct Costs { uint64_t faults, protects, nanoseconds; };
    void mw2_watch_costs(Costs* costs) { *costs = {}; }

    // The title's hook of Memcard_InitializeSystem, on each side, and the original it calls.
    void MW2_CAT(sub_, T_Memcard_InitializeSystem)(PPCContext&, uint8_t*);
    void MW2_CAT(cpp_sub_, T_Memcard_InitializeSystem)(PPCContext&, uint8_t*);

    // The launch data both sides keep in the Rust install module.
    void mw2_install_launch_data(void* data, void (*sink)(void*, const char*, size_t));
    void mw2_install_set_launch_data(const uint8_t* data, size_t size);
    bool mw2_signin_input(uint32_t user, uint16_t buttons);
    void mw2_render_frame(uint32_t* size);
    // What the rest of the Rust runtime asks of its XAM (tools/kernel_diff.rs).
    void rig_online_choose(bool any);
    void rig_online_frame();
    void rig_xam_post_notification(uint32_t id, uint32_t param);
    void rig_xam_accept_invite(uint64_t inviter, const uint8_t* session, bool fromInvite);
    void rig_xam_move_player_data(uint64_t from, uint64_t to);
    void mw2_signin_at(uint32_t user, bool* signedIn, uint64_t* id, void* name, void (*sink)(void*, const char*, size_t));
    void mw2_signin_first(uint64_t* id, void* name, void (*sink)(void*, const char*, size_t));
    void mw2_presence_set_context(uint32_t user, uint32_t id, uint32_t value);
}

// A fixed system time, so that absolute timeouts fall due the same on both sides.
constexpr uint64_t kSystemTime = 0x01DD000000000000ull;
extern "C" uint64_t mw2_kernel_system_time() { return kSystemTime; }
namespace kernel
{
    uint64_t SystemTime100ns() { return kSystemTime; }
}

namespace
{
    // What the GPU is told, written down for the side that runs now.
    void Said(const char* format, ...) __attribute__((format(printf, 1, 2)));
}

// The GPU's side of the Vd* calls, the C++'s and the Rust's.
namespace gpu
{
    void SetRingBuffer(uint32_t base, uint32_t size) { Said("ring buffer %08X %u", base, size); }
    void SetReadPointerWriteBack(uint32_t address, uint32_t size) { Said("write back %08X %u", address, size); }
    void SetInterruptCallback(uint32_t callback, uint32_t context) { Said("interrupt %08X %08X", callback, context); }
    void Swap() { Said("swap"); }
}
namespace vk::renderer
{
    void FlashSeen(const char* how) { Said("flash %s", how); }
}
extern "C"
{
    void mw2_gpu_set_ring_buffer(uint32_t base, uint32_t size) { gpu::SetRingBuffer(base, size); }
    void mw2_gpu_set_read_pointer_write_back(uint32_t address, uint32_t size) { gpu::SetReadPointerWriteBack(address, size); }
    void mw2_gpu_set_interrupt_callback(uint32_t callback, uint32_t context) { gpu::SetInterruptCallback(callback, context); }
    void mw2_gpu_swap() { gpu::Swap(); }
    void mw2_renderer_flash_seen(const char* how) { vk::renderer::FlashSeen(how); }
    void mw2_crash_request_exit(const char* why) { Said("exit %s", why); }
}

namespace
{
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
        uint32_t Below(uint32_t n) { return uint32_t(Next() % n); }
        bool Chance(uint32_t percent) { return Below(100) < percent; }
    };

    // The guest space each side gets. The arena is the script's: headers, critical
    // sections, spin locks, what a call writes back, and what guest code records.
    constexpr uint64_t kSpace = 1ull << 32;
    constexpr uint32_t kArena = 0x40000000, kArenaSize = 0x40000;
    constexpr uint32_t kHeaders = kArena + 0x1000, kHeaderCount = 64;          // 32 bytes each
    constexpr uint32_t kSections = kArena + 0x2000, kSectionCount = 32;        // 32 bytes each
    constexpr uint32_t kSpinLocks = kArena + 0x2800, kSpinLockCount = 16;
    constexpr uint32_t kOut = kArena + 0x3000;                                 // what calls write back
    constexpr uint32_t kTimeouts = kArena + 0x3800;
    constexpr uint32_t kLists = kArena + 0x4000;                               // handle and header arrays
    constexpr uint32_t kApcLog = kArena + 0x8000, kApcLogSize = 0x4000;
    constexpr uint32_t kRecords = kArena + 0x10000, kRecordSize = 64, kRecordCount = 0x400;
    constexpr uint32_t kStack = kArena + 0x3F000;

    // Guest code: host functions in the lookup table at these addresses.
    constexpr uint32_t kCodeBase = uint32_t(PPC_CODE_BASE);
    constexpr uint32_t kThreadRoutine = kCodeBase + 0x100;   // r3: its record
    constexpr uint32_t kShim = kCodeBase + 0x200;            // XAPI's startup: r3 the routine, r4 its record
    constexpr uint32_t kWaiter = kCodeBase + 0x300;          // r3: a record whose first word is a handle
    constexpr uint32_t kApcRoutine = kCodeBase + 0x400;
    constexpr uint32_t kApcRequeue = kCodeBase + 0x500;      // queues another APC while one is delivered
    constexpr uint32_t kNoFunction = kCodeBase + 0x600;      // in the code, but nothing starts there
    constexpr uint32_t kTaker = kCodeBase + 0x700;           // r3: a record naming what to take for 1 ms

    constexpr uint32_t kSuccess = 0, kTimeout = 0x102, kUserApc = 0xC0;

    struct Side;

    // An object as the script knows it, so that what it asks can be sure not to block.
    struct Known
    {
        enum Kind { kEvent, kMutant, kTimer, kThread, kListener, kSession, kEnumerator, kDuplicate, kFile } kind;
        uint32_t handle;
        bool settled = true;   // a thread that is not running: never resumed, or done
        uint32_t record = 0;   // a thread's
    };

    struct Side
    {
        const char* name;
        uint8_t* base;
        Import imports[kImportCount];
#define X(type, name, arguments) type (*name) arguments;
        KERNEL_CALLS(X)
#undef X
        uint32_t* physicalTop;
        uint32_t mainBlock = 0;      // r13 of the script's thread
        uint32_t otherBlocks[3] = {};
        std::vector<std::string> transcript;
        std::vector<Known> known;
        std::vector<uint32_t> allocations, physical, referenced;
        uint32_t records = 0;
        uint64_t discarded = 0;
        std::filesystem::path folder;   // where its files are: the game's, and saves/

        // The online service's side of it: what the script has arrive, and when.
        uint64_t randomBytes = 0;    // what RandomBytes draws from
        std::map<uint16_t, std::deque<online::Datagram>> incoming;
        std::map<uint32_t, std::vector<online::SessionFound>> answered;
        uint32_t searches = 0;
        std::deque<online::Invite> invites;
        // The title's sockets, and what the script knows of them.
        struct Socket
        {
            uint32_t handle;
            bool stream, nonBlocking;
            uint16_t port;   // bound to, or 0
        };
        std::vector<Socket> sockets;
        std::vector<uint32_t> standIns, sessionObjects, qos, searchBlocks, xamMemory;
    };

    Side g_sides[2];
    uint64_t g_seed;   // the script's
    Side* g_running;   // the side whose script is running

    void Note(Side& side, const char* format, ...) __attribute__((format(printf, 2, 3)));
    void Note(Side& side, const char* format, ...)
    {
        char line[512];
        va_list arguments;
        va_start(arguments, format);
        std::vsnprintf(line, sizeof line, format, arguments);
        va_end(arguments);
        side.transcript.push_back(line);
    }

    uint32_t Load32(Side& side, uint32_t address)
    {
        uint32_t value;
        std::memcpy(&value, side.base + address, 4);
        return __builtin_bswap32(value);
    }
    void Store32(Side& side, uint32_t address, uint32_t value)
    {
        value = __builtin_bswap32(value);
        std::memcpy(side.base + address, &value, 4);
    }
    void Store64(Side& side, uint32_t address, uint64_t value)
    {
        value = __builtin_bswap64(value);
        std::memcpy(side.base + address, &value, 8);
    }

    Side& SideOf(uint8_t* base) { return base == g_sides[0].base ? g_sides[0] : g_sides[1]; }
    Side& Here() { return SideOf(mw2_guest_base); }

    // 0: no service; 1: one like lan, which carries datagrams only; 2: one that carries
    // Live's lobbies and matchmaking too.
    int g_service = 0;
    constexpr uint64_t kLocalId = 0x0000777700000001ull, kAccount = 0x42;

    std::string Hex(const uint8_t* bytes, size_t size)
    {
        std::string text;
        char pair[3];
        for (size_t i = 0; i < size; i++)
        {
            std::snprintf(pair, sizeof pair, "%02x", bytes[i]);
            text += pair;
        }
        return text;
    }
}

// The bytes both sides' sessions and XNetRandom draw: the side's own sequence.
void kernel::RandomBytes(void* out, size_t size)
{
    Side& side = Here();
    for (size_t i = 0; i < size; i++) static_cast<uint8_t*>(out)[i] = uint8_t(Random{ side.randomBytes++ }.Next());
}

// The rig's online service, the same for both sides: the Rust's reaches it through
// tools/kernel_diff.rs, the C++'s through RigBackend.
extern "C"
{
    using Sink = void (*)(void* to, const char* bytes, size_t size);
    struct RigWant
    {
        uint32_t id, value;
        bool atLeast;
    };
    struct RigFound
    {
        uint64_t host;
        uint8_t session[60];
        uint32_t openPublic, openPrivate, filledPublic, filledPrivate, lag;
    };

    void mw2_kernel_random_bytes(uint8_t* out, size_t size) { kernel::RandomBytes(out, size); }

    uint64_t rig_service_local_id() { return kLocalId; }
    uint64_t rig_service_account() { return kAccount; }
    void rig_service_local_name(void* to, Sink sink)
    {
        // A name longer than a gamertag in some seeds, none in others: the profile's.
        const char* name = g_seed % 4 == 0 ? "" : g_seed % 4 == 1 ? "RigPlayerWithALongName" : "Rig";
        sink(to, name, std::strlen(name));
    }
    bool rig_service_send(uint64_t peer, uint16_t fromPort, uint16_t toPort, const uint8_t* bytes, size_t size)
    {
        Note(Here(), "service: send %llx %u->%u %s", (unsigned long long)peer, fromPort, toPort, Hex(bytes, size).c_str());
        return true;
    }
    bool rig_service_broadcast(uint16_t fromPort, uint16_t toPort, const uint8_t* bytes, size_t size)
    {
        Note(Here(), "service: broadcast %u->%u %s", fromPort, toPort, Hex(bytes, size).c_str());
        return true;
    }
    bool rig_service_receive(uint16_t port, uint64_t* from, uint16_t* fromPort, void* to, Sink sink)
    {
        auto& waiting = Here().incoming[port];
        if (waiting.empty()) return false;
        const online::Datagram datagram = waiting.front();
        waiting.pop_front();
        *from = datagram.from;
        *fromPort = datagram.fromPort;
        sink(to, reinterpret_cast<const char*>(datagram.bytes.data()), datagram.bytes.size());
        return true;
    }
    void rig_service_advertise(const uint8_t* line, size_t size)
    {
        Note(Here(), "service: advertise \"%s\"", std::string(reinterpret_cast<const char*>(line), size).c_str());
    }
    bool rig_service_lobbies() { return g_service == 2; }
    void rig_service_set_joinable(const uint8_t* session)
    {
        Note(Here(), "service: joinable %s", session ? Hex(session, 60).c_str() : "none");
    }
    void rig_service_host_moved(uint64_t peer) { Note(Here(), "service: host moved to %llx", (unsigned long long)peer); }
    bool rig_service_next_invite(uint64_t* inviter, uint8_t* session, bool* fromInvite)
    {
        Side& side = Here();
        if (side.invites.empty()) return false;
        *inviter = side.invites.front().inviter;
        std::memcpy(session, side.invites.front().session, 60);
        *fromInvite = side.invites.front().fromInvite;
        side.invites.pop_front();
        return true;
    }
    bool rig_service_matchmaking() { return g_service == 2; }
    void rig_service_list(const uint8_t* session, const uint32_t* counts, const uint32_t* numbers, size_t count)
    {
        if (!session) return Note(Here(), "service: list nothing");
        std::string text;
        for (size_t i = 0; i < count; i++) text += " " + std::to_string(numbers[2 * i]) + "=" + std::to_string(numbers[2 * i + 1]);
        Note(Here(), "service: list %s open %u %u filled %u %u numbers%s", Hex(session, 60).c_str(), counts[0], counts[1],
             counts[2], counts[3], text.c_str());
    }
    void rig_service_in_game(bool playing) { Note(Here(), "service: in game %d", playing); }
    uint32_t rig_service_search(uint32_t room, uint32_t most, const RigWant* wants, size_t count)
    {
        std::string text;
        for (size_t i = 0; i < count; i++)
            text += " " + std::to_string(wants[i].id) + (wants[i].atLeast ? ">=" : "=") + std::to_string(wants[i].value);
        Side& side = Here();
        Note(side, "service: search %u room %u most %u wants%s", side.searches + 1, room, most, text.c_str());
        return ++side.searches;
    }
    ptrdiff_t rig_service_search_results(uint32_t search, RigFound* out, size_t room)
    {
        auto& answered = Here().answered;
        const auto it = answered.find(search);
        if (it == answered.end()) return -1;
        size_t count = 0;
        for (const online::SessionFound& one : it->second)
        {
            if (count == room) break;
            RigFound& found = out[count++];
            found.host = one.host;
            std::memcpy(found.session, one.session, 60);
            found.openPublic = one.openPublic;
            found.openPrivate = one.openPrivate;
            found.filledPublic = one.filledPublic;
            found.filledPrivate = one.filledPrivate;
            found.lag = one.lag;
        }
        answered.erase(it);
        return ptrdiff_t(count);
    }
    bool rig_service_title_file(const char* name, void* to, Sink sink)
    {
        Note(Here(), "service: title file %s", name);
        if (std::strcmp(name, "playlists.patch2") != 0) return false;
        sink(to, "the rig's playlists", 19);
        return true;
    }
}

namespace
{
    class RigBackend final : public online::Backend
    {
    public:
        bool Start() override { return true; }
        uint64_t LocalId() override { return rig_service_local_id(); }
        uint64_t Account() override { return rig_service_account(); }
        std::string LocalName() override
        {
            std::string name;
            rig_service_local_name(&name, [](void* to, const char* text, size_t size) { static_cast<std::string*>(to)->assign(text, size); });
            return name;
        }
        bool Send(uint64_t peer, uint16_t fromPort, uint16_t toPort, const void* bytes, size_t size) override
        {
            return rig_service_send(peer, fromPort, toPort, static_cast<const uint8_t*>(bytes), size);
        }
        bool Broadcast(uint16_t fromPort, uint16_t toPort, const void* bytes, size_t size) override
        {
            return rig_service_broadcast(fromPort, toPort, static_cast<const uint8_t*>(bytes), size);
        }
        bool Receive(uint16_t port, online::Datagram& out) override
        {
            return rig_service_receive(port, &out.from, &out.fromPort, &out.bytes, [](void* to, const char* bytes, size_t size) {
                static_cast<std::vector<uint8_t>*>(to)->assign(bytes, bytes + size);
            });
        }
        void Advertise(const std::string& connect) override
        {
            rig_service_advertise(reinterpret_cast<const uint8_t*>(connect.data()), connect.size());
        }
        bool Lobbies() override { return rig_service_lobbies(); }
        void SetJoinable(const online::Joinable* joinable) override { rig_service_set_joinable(joinable ? joinable->session : nullptr); }
        void HostMoved(uint64_t peer) override { rig_service_host_moved(peer); }
        bool NextInvite(online::Invite& out) override { return rig_service_next_invite(&out.inviter, out.session, &out.fromInvite); }
        bool Matchmaking() override { return rig_service_matchmaking(); }
        void List(const online::SessionListing* listing) override
        {
            if (!listing) return rig_service_list(nullptr, nullptr, nullptr, 0);
            const uint32_t counts[] = { listing->openPublic, listing->openPrivate, listing->filledPublic, listing->filledPrivate };
            std::vector<uint32_t> numbers;
            for (const online::SessionNumber& number : listing->numbers)
            {
                numbers.push_back(number.id);
                numbers.push_back(number.value);
            }
            rig_service_list(listing->session, counts, numbers.data(), listing->numbers.size());
        }
        void InGame(bool playing) override { rig_service_in_game(playing); }
        uint32_t Search(const online::SessionSearch& search) override
        {
            std::vector<RigWant> wants;
            for (const online::SessionWant& want : search.wants) wants.push_back({ want.id, want.value, want.atLeast });
            return rig_service_search(search.room, search.most, wants.data(), wants.size());
        }
        bool SearchResults(uint32_t search, std::vector<online::SessionFound>& out) override
        {
            RigFound found[16];
            const ptrdiff_t count = rig_service_search_results(search, found, 16);
            if (count < 0) return false;
            out.clear();
            for (ptrdiff_t i = 0; i < count; i++)
            {
                online::SessionFound one;
                one.host = found[i].host;
                std::memcpy(one.session, found[i].session, 60);
                one.openPublic = found[i].openPublic;
                one.openPrivate = found[i].openPrivate;
                one.filledPublic = found[i].filledPublic;
                one.filledPrivate = found[i].filledPrivate;
                one.lag = found[i].lag;
                out.push_back(one);
            }
            return true;
        }
        bool TitleFile(const std::string& name, std::vector<uint8_t>& out) override
        {
            return rig_service_title_file(name.c_str(), &out, [](void* to, const char* bytes, size_t size) {
                static_cast<std::vector<uint8_t>*>(to)->assign(bytes, bytes + size);
            });
        }
    };
}

// The C++ build's service.cpp, as the C++ XAM and network see it here.
online::Backend* online::GetBackend()
{
    static RigBackend rig;
    return g_service ? &rig : nullptr;
}
bool online::Live() { return g_service == 2; }
const char* online::Name() { return g_service ? "rig" : nullptr; }
void online::Frame()
{
    auto* service = GetBackend();
    if (!service) return;
    kernel::FinishSearches();
    online::Invite invite;
    if (service->NextInvite(invite))
    {
        LOGI("online: joining a friend's Xbox LIVE %s", invite.fromInvite ? "invitation" : "session");
        kernel::AcceptInvite(invite.inviter, invite.session, invite.fromInvite);
    }
}

namespace
{

    void Said(const char* format, ...)
    {
        if (!g_running) return;
        char line[512];
        va_list arguments;
        va_start(arguments, format);
        std::vsnprintf(line, sizeof line, format, arguments);
        va_end(arguments);
        g_running->transcript.push_back(line);
    }

    // A call as the title makes it, from the script's thread or another block's.
    uint64_t Call(Side& side, int import, std::initializer_list<uint64_t> arguments, uint32_t r13 = 0)
    {
        PPCContext ctx;
        std::memset(&ctx, 0, sizeof ctx);
        ctx.r1.u64 = kStack;
        ctx.r13.u64 = r13 ? r13 : side.mainBlock;
        ctx.lr = 0x82123450;
        PPCRegister* registers[] = { &ctx.r3, &ctx.r4, &ctx.r5, &ctx.r6, &ctx.r7, &ctx.r8, &ctx.r9, &ctx.r10 };
        size_t i = 0;
        for (uint64_t argument : arguments) registers[i++]->u64 = argument;
        PPCContext before;
        std::memcpy(&before, &ctx, sizeof ctx);
        mw2_guest_base = side.base;
        side.imports[import](ctx, side.base);
        // An import changes r3 and nothing else.
        before.r3 = ctx.r3;
        before.fpscr = ctx.fpscr;
        if (std::memcmp(&ctx, &before, sizeof ctx) != 0) Note(side, "%s changed a register besides r3", kImportNames[import]);
        Note(side, "%s -> %llx", kImportNames[import], (unsigned long long)ctx.r3.u64);
        return ctx.r3.u64;
    }

    // ---- guest code ------------------------------------------------------------

    void RecordThread(PPCContext& ctx, uint8_t* base, uint32_t record)
    {
        Side& side = SideOf(base);
        const uint32_t kthread = Load32(side, ctx.r13.u32 + 256);
        const uint32_t tls = Load32(side, ctx.r13.u32);
        Store32(side, record + 0, ctx.r3.u32);
        Store32(side, record + 4, ctx.r4.u32);
        Store32(side, record + 8, ctx.r13.u32);
        Store32(side, record + 12, ctx.r1.u32);
        Store32(side, record + 16, kthread ? Load32(side, kthread + 332) : 0);
        side.base[record + 20] = side.base[ctx.r13.u32 + 268];
        Store32(side, record + 24, tls);
        std::memcpy(side.base + record + 28, side.base + tls, 16);
        Store32(side, record + 44, 0x600DF00D);
        ctx.r3.u64 = 0;
    }

    void ThreadRoutine(PPCContext& ctx, uint8_t* base) { RecordThread(ctx, base, ctx.r3.u32); }
    void Shim(PPCContext& ctx, uint8_t* base) { RecordThread(ctx, base, ctx.r4.u32); }

    void Waiter(PPCContext& ctx, uint8_t* base)
    {
        Side& side = SideOf(base);
        const uint32_t record = ctx.r3.u32;
        PPCContext call;
        std::memset(&call, 0, sizeof call);
        call.r1 = ctx.r1;
        call.r13 = ctx.r13;
        call.r3.u64 = Load32(side, record);
        side.imports[kNtWaitForSingleObjectEx](call, base);
        Store32(side, record + 4, call.r3.u32);
        Store32(side, record + 8, 0x600DF00D);
        ctx.r3.u64 = 0;
    }

    void LogApc(Side& side, uint32_t what, const PPCContext& ctx)
    {
        const uint32_t count = Load32(side, kApcLog);
        const uint32_t at = kApcLog + 4 + (count * 24) % (kApcLogSize - 32);
        Store32(side, at, what);
        Store32(side, at + 4, ctx.r3.u32);
        Store32(side, at + 8, ctx.r4.u32);
        Store32(side, at + 12, ctx.r5.u32);
        Store32(side, at + 16, ctx.r1.u32);
        Store32(side, at + 20, ctx.r13.u32);
        Store32(side, kApcLog, count + 1);
    }

    void ApcRoutine(PPCContext& ctx, uint8_t* base) { LogApc(SideOf(base), 0xA9C, ctx); }

    // Takes a mutant (or whatever the record names) and ends without giving it back: by
    // handle when the word at +12 is 0, by header otherwise, waiting 1 ms at most.
    void Taker(PPCContext& ctx, uint8_t* base)
    {
        Side& side = SideOf(base);
        const uint32_t record = ctx.r3.u32;
        Store64(side, record + 16, uint64_t(-10000));
        PPCContext call;
        std::memset(&call, 0, sizeof call);
        call.r1 = ctx.r1;
        call.r13 = ctx.r13;
        call.r3.u64 = Load32(side, record);
        if (Load32(side, record + 12) == 0)
        {
            call.r4.u64 = 1;
            call.r6.u64 = record + 16;
            side.imports[kNtWaitForSingleObjectEx](call, base);
        }
        else
        {
            call.r5.u64 = 1;
            call.r7.u64 = record + 16;
            side.imports[kKeWaitForSingleObject](call, base);
        }
        Store32(side, record + 4, call.r3.u32);
        Store32(side, record + 8, 0x600DF00D);
        ctx.r3.u64 = 0;
    }

    struct Work
    {
        Side* side;
        uint32_t id;
    };
    void RunWork(void* work)
    {
        auto* one = static_cast<Work*>(work);
        PPCContext ctx;
        std::memset(&ctx, 0, sizeof ctx);
        ctx.r3.u64 = one->id;
        LogApc(*one->side, 0x30, ctx);
        delete one;
    }
    void DiscardWork(void* work)
    {
        auto* one = static_cast<Work*>(work);
        one->side->discarded++;
        delete one;
    }

    void ApcRequeue(PPCContext& ctx, uint8_t* base)
    {
        Side& side = SideOf(base);
        LogApc(side, 0x2E9, ctx);
        side.queue_user_apc(kApcRoutine, ctx.r3.u32 + 1, ctx.r4.u32, RunWork, DiscardWork, new Work{ &side, ctx.r3.u32 });
    }

    void PlaceFunction(Side& side, uint32_t address, PPCFunc* function)
    {
        PPCFunc** slot = reinterpret_cast<PPCFunc**>(side.base + PPC_IMAGE_BASE + PPC_IMAGE_SIZE +
                                                     uint64_t(address - kCodeBase) * 2);
        *slot = function;
    }

    // ---- the script ------------------------------------------------------------

    uint32_t Out(uint32_t n) { return kOut + n * 8; }

    // A timeout pointer to one that ends at once or soon, or to none.
    uint32_t ShortTimeout(Side& side, Random& random, uint32_t slot)
    {
        const uint32_t at = kTimeouts + slot * 8;
        int64_t value;
        switch (random.Below(6))
        {
        case 0: value = 0; break;                                            // an absolute time long gone
        case 1: value = -int64_t(random.Below(3000)); break;                 // up to 300 us
        case 2: value = int64_t(kSystemTime) - int64_t(random.Below(100000)); break;   // in the past
        case 3: value = int64_t(kSystemTime) + int64_t(random.Below(2000)); break;     // in 200 us
        case 4: value = -1; break;
        default: value = -int64_t(random.Below(20000)); break;               // up to 2 ms
        }
        Store64(side, at, uint64_t(value));
        return at;
    }

    uint32_t Forever(Side& side, Random& random, uint32_t slot)
    {
        if (random.Chance(50)) return 0;
        const uint32_t at = kTimeouts + slot * 8;
        Store64(side, at, 0x8000000000000000ull);   // the most negative: for ever
        return at;
    }

    // A header in the arena, or one ObReferenceObjectByHandle gave: a C++ object's too.
    uint32_t PickHeader(Side& side, Random& random)
    {
        if (!side.referenced.empty() && random.Chance(25)) return side.referenced[random.Below(uint32_t(side.referenced.size()))];
        return kHeaders + random.Below(kHeaderCount) * 32;
    }

    uint32_t PickHandle(Side& side, Random& random, bool settledOnly)
    {
        const uint32_t choice = random.Below(16);
        if (choice == 0) return uint32_t(random.Next());
        if (choice == 1) return 0xFFFFFFFF;
        if (side.known.empty()) return 0x10004 + random.Below(64) * 4;
        const Known& one = side.known[random.Below(uint32_t(side.known.size()))];
        if (settledOnly && !one.settled) return 0;
        return one.handle;
    }

    // What a create wrote back, when it wrote anything: Out(0) is cleared before each.
    void Remember(Side& side, Known::Kind kind, uint32_t status)
    {
        if (status == kSuccess && Load32(side, Out(0))) side.known.push_back({ kind, Load32(side, Out(0)) });
    }

    // Writes a dispatcher header as the title's own code would.
    void TitleHeader(Side& side, Random& random, uint32_t header)
    {
        static const uint8_t kTypes[] = { 0, 1, 2, 5, 6, 8, 9, 3, 0xFF };
        side.base[header] = kTypes[random.Below(sizeof kTypes)];
        static const uint32_t kStates[] = { 0, 1, 2, 0xFFFFFFFF, 0x7FFFFFFF, 0x80000000 };
        Store32(side, header + 4, kStates[random.Below(6)]);
        Store32(side, header + 8, header + 8);
        Store32(side, header + 12, header + 8);
        Store32(side, header + 16, random.Chance(50) ? random.Below(5) : uint32_t(random.Next()));
        if (random.Chance(10)) Store32(side, header + 12, 0x4D573200);   // a forged signature
    }

    // Waits on what is there: the one wait a script may make on a thread that runs.
    void WaitForThread(Side& side, Known& thread)
    {
        Call(side, kNtWaitForSingleObjectEx, { thread.handle, 1, 0, 0 });
        thread.settled = true;
        Note(side, "thread record: %08X %08X %08X %08X id %u processor %u tls %08X done %08X", Load32(side, thread.record),
             Load32(side, thread.record + 4), Load32(side, thread.record + 8), Load32(side, thread.record + 12),
             Load32(side, thread.record + 16), side.base[thread.record + 20], Load32(side, thread.record + 24),
             Load32(side, thread.record + 44));
    }

    // ---- files, saved games, the profile, the clocks, video and input -------------

    constexpr uint32_t kNames = kArena + 0x20000;                      // OBJECT_ATTRIBUTES, ANSI_STRING, the text
    constexpr uint32_t kBuffer = kArena + 0x21000, kBufferSize = 0x8000;
    constexpr uint32_t kProfile = kArena + 0x29000;                    // a read's buffer, then what a write gives
    constexpr uint32_t kContent = kArena + 0x31000;                    // XCONTENT_DATA
    constexpr uint32_t kSmall = kArena + 0x32000;                      // strings, an XOVERLAPPED, a vibration

    // The game's files, as each side's folder has them: names in any case, folders in
    // folders, and one name that only differs from a folder's by case.
    const char* const kGameFiles[] = { "zone/english/common_mp.ff", "zone/english/Patch_MP.ff", "zone/dlc/map1.FF",
                                       "maps/mp/afghan.d3dbsp", "Video/Intro.BIK", "readme.TXT", "Data/config.cfg",
                                       "Data/sub/deep/file.bin", "empty.bin" };

    void MakeGame(const std::filesystem::path& folder, uint64_t seed)
    {
        Random random{ seed * 31 };
        std::filesystem::remove_all(folder);
        for (const char* name : kGameFiles)
        {
            const auto path = folder / "game" / name;
            std::filesystem::create_directories(path.parent_path());
            std::string bytes(std::string(name) == "empty.bin" ? 0 : random.Below(6000), '\0');
            for (char& c : bytes) c = char(random.Next());
            std::ofstream(path, std::ios::binary) << bytes;
        }
        std::filesystem::create_directories(folder / "game" / "emptydir");
        std::filesystem::create_directories(folder / "saves");
        // A file where a package would go, and in a package where a folder would.
        std::ofstream(folder / "saves" / "x.y-z", std::ios::binary) << "not a package";
        std::filesystem::create_directories(folder / "saves" / "stats");
        std::ofstream(folder / "saves" / "stats" / "folder", std::ios::binary) << "not a folder";
        // Packages with something in them, for the title to open, overwrite and write to.
        std::filesystem::create_directories(folder / "saves" / "savegame");
        std::ofstream(folder / "saves" / "savegame" / "savegame.svg", std::ios::binary) << std::string(3000 + random.Below(3000), 'S');
        std::ofstream(folder / "saves" / "stats" / "stats.dat", std::ios::binary) << std::string(100 + random.Below(500), 'D');
        // The profile a run before left: whole, cut short, someone else's, or a size no
        // setting has.
        std::string profile = "MW2PROF1";
        auto word = [&](uint32_t value) { profile.append(reinterpret_cast<const char*>(&value), 4); };
        const uint32_t kind = uint32_t(seed % 5);
        if (kind != 4)
        {
            word(3);
            word(0x10040001); word(2); word(8); profile += std::string("\x00\x00\x00\x01\x00\x00\x00\x00", 8);
            word(0x63E83FFF); word(1); word(5); profile += "blob!";
            word(0x60010003); word(2); word(kind == 3 ? 70000 : 0);
            if (kind == 3) profile += std::string(70000, 'L');
            if (kind == 1) profile.resize(profile.size() - 7);
            if (kind == 2) profile[3] = 'X';
            std::ofstream(folder / "saves" / "profile.bin", std::ios::binary) << profile;
        }
    }

    // Every file and folder under a side's, with what it holds.
    std::string Tree(const std::filesystem::path& folder)
    {
        std::map<std::string, std::string> found;
        std::error_code ec;
        for (auto it = std::filesystem::recursive_directory_iterator(folder, ec); it != std::filesystem::recursive_directory_iterator(); it.increment(ec))
        {
            const auto relative = std::filesystem::relative(it->path(), folder).string();
            if (it->is_directory(ec)) { found[relative] = "folder"; continue; }
            // A write far past the end leaves a file of a terabyte, sparse: its size, and
            // what its first megabyte holds.
            std::ifstream in(it->path(), std::ios::binary);
            std::string bytes(1 << 20, '\0');
            in.read(bytes.data(), std::streamsize(bytes.size()));
            bytes.resize(size_t(in.gcount()));
            uint64_t hash = 0xcbf29ce484222325ull;
            for (char c : bytes) hash = (hash ^ uint8_t(c)) * 0x100000001b3ull;
            char line[80];
            std::snprintf(line, sizeof line, "%llu bytes %016llx", (unsigned long long)std::filesystem::file_size(it->path(), ec),
                          (unsigned long long)hash);
            found[relative] = line;
        }
        std::string out;
        for (const auto& [name, what] : found) out += name + ": " + what + "\n";
        return out;
    }

    // A path as the title may name one: a device, then folders in any case and either
    // separator, with what a peer could send among them.
    std::string GuestPath(Random& random)
    {
        static const char* const kDevices[] = { "game:\\", "GAME:\\", "game:", "\\Device\\Cdrom0\\", "\\device\\Harddisk0\\Partition1\\",
                                                "d:\\", "save0:\\", "Save0:", "save1:\\", "cache:\\", "", ":", "\\Device\\Cdrom0" };
        static const char* const kPaths[] = { "zone/english/common_mp.ff", "zone/english/patch_mp.ff", "zone/dlc/map1.ff", "maps/mp/afghan.d3dbsp",
                                              "video/intro.bik", "readme.txt", "data/config.cfg", "data/sub/deep/file.bin", "empty.bin",
                                              "emptydir", "zone", "zone/english", "data", "savegame.svg", "stats.dat", "folder/inner.dat",
                                              "missing.ff", "zone/missing/x.ff", "../../etc/passwd", "zone/../readme.txt", "./zone/./dlc/map1.ff",
                                              "C:/x.ff", "a:b", "", "zone//english\\\\common_mp.ff", "data/config.cfg/x", "folder/sub" };
        std::string path = kDevices[random.Below(sizeof kDevices / sizeof *kDevices)];
        std::string rest = kPaths[random.Below(sizeof kPaths / sizeof *kPaths)];
        // What a package holds, under a drive a package may be mounted on.
        static const char* const kSaved[] = { "savegame.svg", "stats.dat", "SaveGame.SVG", "folder" };
        if (random.Chance(30))
        {
            path = random.Chance(70) ? "save0:\\" : "save1:\\";
            rest = kSaved[random.Below(4)];
        }
        if (random.Chance(20)) rest = std::string(random.Chance(50) ? "../" : "..\\") + rest;
        for (char& c : rest)
        {
            if (c == '/' && random.Chance(60)) c = '\\';
            if (random.Chance(30)) c = char(std::toupper(uint8_t(c)));
        }
        return path + rest;
    }

    // OBJECT_ATTRIBUTES naming `path`, as the title builds one, now and then without a
    // name, or with a length that cuts it short.
    uint32_t ObjectAttributes(Side& side, Random& random, const std::string& path)
    {
        const uint32_t attributes = kNames, string = kNames + 16, text = kNames + 32;
        std::memset(side.base + kNames, 0, 0x400);
        std::memcpy(side.base + text, path.data(), std::min<size_t>(path.size(), 0x3C0));
        Store32(side, attributes, 0);
        Store32(side, attributes + 4, random.Chance(97) ? string : 0);
        const uint16_t length = random.Chance(90) ? uint16_t(path.size()) : uint16_t(random.Below(uint32_t(path.size()) + 1));
        side.base[string] = uint8_t(length >> 8);
        side.base[string + 1] = uint8_t(length);
        Store32(side, string + 4, random.Chance(97) ? text : 0);
        return random.Chance(98) ? attributes : 0;
    }

    uint32_t PickFile(Side& side, Random& random)
    {
        std::vector<uint32_t> files;
        for (const Known& one : side.known)
            if (one.kind == Known::kFile) files.push_back(one.handle);
        if (files.empty() || random.Chance(5)) return PickHandle(side, random, true);
        return files[random.Below(uint32_t(files.size()))];
    }

    // An offset for a read or a write, or none: the file's position.
    uint32_t Offset(Side& side, Random& random)
    {
        if (random.Chance(40)) return 0;
        const uint32_t at = kSmall + 0x200;
        static const uint64_t kOdd[] = { uint64_t(-1), uint64_t(-2), 0xFFFFFFFE00000000ull, 1ull << 40 };
        Store64(side, at, random.Chance(90) ? random.Below(7000) : kOdd[random.Below(4)]);
        return at;
    }

    void Fill(Side& side, Random& random, uint32_t at, uint32_t size)
    {
        for (uint32_t i = 0; i < size; i++) side.base[at + i] = uint8_t(random.Next());
    }

    // A string in the guest at `at`.
    uint32_t Text(Side& side, uint32_t at, const char* text)
    {
        std::memcpy(side.base + at, text, std::strlen(text) + 1);
        return at;
    }

    // An XOVERLAPPED, whose event is one the script made, or nothing.
    uint32_t Overlapped(Side& side, Random& random)
    {
        if (random.Chance(40)) return 0;
        const uint32_t at = kSmall + 0x100;
        std::memset(side.base + at, 0xCC, 28);
        uint32_t event = 0;
        for (const Known& one : side.known)
            if (one.kind == Known::kEvent && random.Chance(50)) event = one.handle;
        Store32(side, at + 12, random.Chance(90) ? event : uint32_t(random.Next()));
        return at;
    }

    // XCONTENT_DATA naming a package.
    uint32_t ContentData(Side& side, Random& random)
    {
        static const char* const kNames[] = { "savegame", "SaveGame", "save_1", "stats", "...hidden", "a/b", "bad name!", "",
                                              "0123456789012345678901234567890123456789ABCDEFGH", "..", "x.y-z" };
        std::memset(side.base + kContent, 0, 0x200);
        Store32(side, kContent, random.Chance(80) ? 1 : random.Below(4));
        Store32(side, kContent + 4, random.Chance(80) ? 1 : uint32_t(random.Next()));
        Text(side, kContent + 264, kNames[random.Below(sizeof kNames / sizeof *kNames)]);
        return random.Chance(97) ? kContent : 0;
    }

    void Delivered(Side& side)
    {
        PPCContext caller;
        std::memset(&caller, 0, sizeof caller);
        caller.r1.u64 = kStack;
        caller.r13.u64 = side.mainBlock;
        mw2_guest_base = side.base;
        Note(side, "delivered %d, apc log %u", side.deliver_user_apcs(&caller), Load32(side, kApcLog));
    }

    void FileStep(Side& side, Random& random)
    {
        switch (random.Below(24))
        {
        case 0: case 1: case 2:
        {
            static const uint32_t kAccess[] = { 0x80000000, 0x40000000, 0x10000000, 0xC0000000, 0x00120089, 0x2, 0x4, 0x1, 0x00100080 };
            // Now and then a folder below what is a file in the "stats" package, if that is
            // mounted: one that cannot be made.
            const bool blocked = random.Chance(8);
            const std::string path = blocked ? "save0:\\folder\\sub" : GuestPath(random);
            const uint32_t attributes = ObjectAttributes(side, random, path);
            const uint32_t access = kAccess[random.Below(sizeof kAccess / sizeof *kAccess)];
            const uint32_t options = blocked ? 1 : random.Chance(70) ? 0x40 : random.Chance(50) ? 1 : uint32_t(random.Next());
            Store32(side, Out(0), 0);
            Store32(side, Out(1), 0xCCCCCCCC);
            Store32(side, Out(1) + 4, 0xCCCCCCCC);
            uint64_t status;
            if (random.Chance(70))
            {
                Store32(side, kStack + 0x54, options);
                status = Call(side, kNtCreateFile, { random.Chance(97) ? Out(0) : 0, access, attributes, random.Chance(90) ? Out(1) : 0, 0, 0x80, 7,
                                                     random.Chance(90) ? random.Below(6) : random.Below(9) });
            }
            else status = Call(side, kNtOpenFile, { random.Chance(97) ? Out(0) : 0, access, attributes, random.Chance(90) ? Out(1) : 0, 7, options });
            Note(side, "open %s: %08X status block %08X %08X", path.c_str(), Load32(side, Out(0)), Load32(side, Out(1)), Load32(side, Out(1) + 4));
            Remember(side, Known::kFile, uint32_t(status));
            break;
        }
        case 3: case 4: case 5:
        {
            const uint32_t length = random.Chance(90) ? random.Below(0x2000) : random.Below(kBufferSize);
            const uint32_t buffer = random.Chance(97) ? kBuffer : 0;
            std::memset(side.base + kBuffer, 0xEE, kBufferSize);
            const bool apc = random.Chance(30);
            Call(side, kNtReadFile, { PickFile(side, random), 0, apc ? (kApcRoutine | random.Below(4)) : 0, random.Below(100),
                                      random.Chance(95) ? Out(1) : 0, buffer, length, Offset(side, random) });
            Note(side, "status block %08X %08X", Load32(side, Out(1)), Load32(side, Out(1) + 4));
            if (apc)
            {
                if (random.Chance(50)) Delivered(side);
                else
                {
                    Store64(side, kTimeouts + 24, uint64_t(-int64_t(random.Below(100))));
                    Call(side, kKeDelayExecutionThread, { 1, 1, kTimeouts + 24 });
                }
                Note(side, "after: %08X %08X", Load32(side, Out(1)), Load32(side, Out(1) + 4));
            }
            break;
        }
        case 6: case 7:
        {
            const uint32_t length = random.Chance(90) ? random.Below(0x1800) : random.Below(kBufferSize);
            Fill(side, random, kBuffer, length);
            Call(side, kNtWriteFile, { PickFile(side, random), 0, 0, 0, random.Chance(95) ? Out(1) : 0, random.Chance(97) ? kBuffer : 0, length,
                                       Offset(side, random) });
            Note(side, "status block %08X %08X", Load32(side, Out(1)), Load32(side, Out(1) + 4));
            break;
        }
        case 8:
        {
            static const uint32_t kClasses[] = { 5, 14, 34, 4, 0 };
            std::memset(side.base + Out(4), 0xCC, 64);
            Call(side, kNtQueryInformationFile, { PickFile(side, random), random.Chance(90) ? Out(1) : 0, random.Chance(95) ? Out(4) : 0, 64,
                                                  kClasses[random.Below(5)] });
            break;
        }
        case 9:
        {
            const std::string path = GuestPath(random);
            std::memset(side.base + Out(4), 0xCC, 64);
            Call(side, kNtQueryFullAttributesFile, { ObjectAttributes(side, random, path), random.Chance(95) ? Out(4) : 0 });
            Note(side, "attributes of %s", path.c_str());
            break;
        }
        case 10: case 11:
        {
            static const uint32_t kClasses[] = { 13, 14, 20, 4 };
            const uint32_t which = kClasses[random.Below(4)];
            const uint32_t at = kSmall + 0x300;
            if (which == 13) side.base[at] = uint8_t(random.Below(3));
            else Store64(side, at, random.Chance(90) ? random.Below(7000) : uint64_t(-1));
            Call(side, kNtSetInformationFile, { PickFile(side, random), random.Chance(90) ? Out(1) : 0, random.Chance(97) ? at : 0, 8, which });
            break;
        }
        case 12:
        {
            const uint32_t handle = PickFile(side, random);
            Call(side, kNtFlushBuffersFile, { handle });
            Call(side, kNtQueryVolumeInformationFile, { handle, 0, 0, 0, 0 });
            Call(side, kNtReadFileScatter, { handle });
            if (random.Chance(60) && Call(side, kNtClose, { handle }) == kSuccess)
                side.known.erase(std::remove_if(side.known.begin(), side.known.end(), [&](const Known& one) { return one.handle == handle; }), side.known.end());
            break;
        }
        case 13: case 14:
        {
            static const char* const kDrives[] = { "save0", "SAVE0", "save1", "", "game", "\\x" };
            const uint32_t root = random.Chance(95) ? Text(side, kSmall, kDrives[random.Below(6)]) : 0;
            const uint32_t data = ContentData(side, random);
            Store32(side, Out(2), 0xCCCCCCCC);
            Store32(side, Out(3), 0xCCCCCCCC);
            const uint32_t overlapped = Overlapped(side, random);
            Store32(side, kStack + 0x54, overlapped);
            Call(side, kXamContentCreateEx, { 0, root, data, (random.Chance(90) ? random.Below(7) : uint32_t(random.Next())) | (random.Chance(50) ? 0x100u : 0u),
                                              random.Chance(90) ? Out(2) : 0, random.Chance(90) ? Out(3) : 0, 0, 0 });
            Note(side, "disposition %08X licence %08X", Load32(side, Out(2)), Load32(side, Out(3)));
            break;
        }
        case 15:
        {
            static const char* const kDrives[] = { "save0", "Save0", "save1", "" };
            const uint32_t root = random.Chance(95) ? Text(side, kSmall, kDrives[random.Below(4)]) : 0;
            if (random.Chance(60)) Call(side, kXamContentClose, { root, Overlapped(side, random) });
            else Call(side, kXamContentDelete, { 0, ContentData(side, random), Overlapped(side, random) });
            break;
        }
        case 16:
        {
            std::memset(side.base + Out(4), 0xCC, 96);
            Call(side, kXamContentGetDeviceState, { random.Below(4), Overlapped(side, random) });
            Call(side, kXamContentGetDeviceData, { random.Chance(80) ? 1 : random.Below(4), random.Chance(95) ? Out(4) : 0 });
            break;
        }
        case 17: case 18:
        {
            // A profile's settings: values, blobs and ids nobody wrote, read the way the
            // title reads them, with a call for the size first.
            static const uint32_t kIds[] = { 0x10040001, 0x10040002, 0x63E83FFF, 0x63E83FFE, 0x60010003, 0x20040004, 0x50080005, 0, 0x6000FFFF, 0xE0010006 };
            const uint32_t user = random.Chance(70) ? 0 : random.Below(5);
            if (random.Chance(50))
            {
                const uint32_t count = random.Chance(95) ? random.Below(6) : random.Below(40);
                for (uint32_t i = 0; i < count; i++)
                {
                    const uint32_t entry = kProfile + i * 40;
                    const uint32_t id = kIds[random.Below(sizeof kIds / sizeof *kIds)];
                    Fill(side, random, entry, 40);
                    Store32(side, entry, random.Below(3));
                    Store32(side, entry + 16, id);
                    if (((id >> 28) & 0xF) == 6)
                    {
                        const uint32_t length = random.Chance(90) ? random.Below(1200) : (random.Chance(50) ? 70000 : 0);
                        const uint32_t data = kBuffer + random.Below(0x100);
                        Fill(side, random, data, std::min<uint32_t>(length, 0x2000));
                        Store32(side, entry + 32, length);
                        Store32(side, entry + 36, random.Chance(95) ? data : 0);
                    }
                }
                Call(side, kXamUserWriteProfileSettings, { 0x41560817, user, count, random.Chance(97) ? kProfile : 0, Overlapped(side, random) });
            }
            else
            {
                const uint32_t count = random.Chance(95) ? random.Below(7) : random.Below(40);
                for (uint32_t i = 0; i < count; i++) Store32(side, kSmall + 0x400 + i * 4, kIds[random.Below(sizeof kIds / sizeof *kIds)]);
                const uint32_t ids = random.Chance(97) ? kSmall + 0x400 : 0;
                Store32(side, Out(5), random.Chance(20) ? random.Below(400) : 0);
                std::memset(side.base + kProfile, 0xCC, 0x8000);
                const uint32_t overlapped = Overlapped(side, random);
                Store32(side, kStack + 0x54, overlapped);
                Call(side, kXamUserReadProfileSettings, { 0x41560817, user, 0, 0, count, ids, random.Chance(97) ? Out(5) : 0, random.Chance(30) ? kProfile : 0 });
                Note(side, "size %08X", Load32(side, Out(5)));
                if (Load32(side, Out(5)) <= 0x8000 && random.Chance(80))
                    Call(side, kXamUserReadProfileSettings, { 0x41560817, user, 0, 0, count, ids, Out(5), kProfile });
            }
            break;
        }
        case 19:
        {
            // The clocks, the delays and the small queries.
            // The C++ inlines its clock, which the rig's cannot stand in for: it has to be
            // the host's now, and the Rust's the rig's.
            Store64(side, Out(4), 0);
            Call(side, kKeQuerySystemTime, { random.Chance(90) ? Out(4) : 0 });
            const uint64_t time = (uint64_t(Load32(side, Out(4))) << 32) | Load32(side, Out(4) + 4);
            const uint64_t now = uint64_t(std::chrono::duration_cast<std::chrono::nanoseconds>(std::chrono::system_clock::now().time_since_epoch()).count() / 100) + 116444736000000000ull;
            const bool right = !time || (side.name[0] == 'C' ? time > now - 100000000 && time <= now : time == kSystemTime);
            Note(side, "system time %s", right ? "right" : "wrong");
            Store64(side, Out(4), 0);
            Call(side, kKeGetCurrentProcessType, {});
            Call(side, kXexCheckExecutablePrivilege, { random.Below(20) });
            Call(side, kKeQueryPerformanceFrequency, {});
            Call(side, kXamGetSystemVersion, {});
            Store32(side, Out(6), 0xCCCCCCCC);
            Call(side, kXexGetModuleHandle, { 0, random.Chance(90) ? Out(6) : 0 });
            Call(side, kXexGetProcedureAddress, { 0, random.Below(100), random.Chance(90) ? Out(6) : 0 });
            Call(side, kXexLoadImage, { 0, 0, 0, random.Chance(90) ? Out(6) : 0 });
            Call(side, kXexUnloadImage, { 0 });
            Call(side, kXGetAVPack, {});
            Call(side, kXGetLanguage, {});
            Call(side, kKeLockL2, { 0 });
            Call(side, kKeUnlockL2, { 0 });
            Call(side, kKiApcNormalRoutineNop, {});
            static const uint32_t kStatuses[] = { 0, 0xC0000034, 0xC000000F, 0xC0000011, 0xC000000D, 0xC0000017, 0x102, 0xC0000001, 0x103 };
            Call(side, kRtlNtStatusToDosError, { random.Chance(90) ? kStatuses[random.Below(9)] : uint32_t(random.Next()) });
            break;
        }
        case 20:
        {
            static const int64_t kIntervals[] = { 0, 1, 1000, -1, -100, -5000, -20000, INT64_MIN };
            const int64_t interval = kIntervals[random.Below(8)];
            Store64(side, kTimeouts + 24, uint64_t(interval));
            const uint32_t count = random.Below(3);
            for (uint32_t i = 0; i < count; i++)
                side.queue_user_apc(kApcRoutine, uint32_t(random.Next()), uint32_t(random.Next()), RunWork, DiscardWork, nullptr);
            Call(side, kKeDelayExecutionThread, { 1, random.Below(2), random.Chance(95) ? kTimeouts + 24 : 0 });
            Note(side, "apc log %u", Load32(side, kApcLog));
            break;
        }
        case 21:
        {
            // Text: what the kernel prints, and the strings it converts.
            static const char* const kTexts[] = { "hello\n", "no newline", "", "\n", "two\n\n", "caf\xE9 au lait\n", "%s %d %x\n" };
            const char* text = kTexts[random.Below(7)];
            Text(side, kSmall, text);
            Call(side, kDbgPrint, { random.Chance(95) ? kSmall : 0 });
            std::memset(side.base + Out(6), 0xCC, 8);
            Call(side, kRtlInitAnsiString, { random.Chance(95) ? Out(6) : 0, random.Chance(90) ? kSmall : 0 });
            const uint32_t bytes = random.Below(40), maximum = random.Below(100);
            Fill(side, random, kSmall + 0x40, 0x100);
            std::memset(side.base + kSmall + 0x200, 0xCC, 0x100);
            Store32(side, Out(7), 0xCCCCCCCC);
            if (random.Chance(50))
                Call(side, kRtlMultiByteToUnicodeN, { random.Chance(95) ? kSmall + 0x200 : 0, maximum, random.Chance(90) ? Out(7) : 0,
                                                      random.Chance(95) ? kSmall + 0x40 : 0, bytes });
            else
                Call(side, kRtlUnicodeToMultiByteN, { random.Chance(95) ? kSmall + 0x200 : 0, maximum, random.Chance(90) ? Out(7) : 0,
                                                      random.Chance(95) ? kSmall + 0x40 : 0, bytes });
            const uint32_t pattern = random.Chance(50) ? 0xDEADBEEF : uint32_t(random.Next());
            const uint32_t length = random.Chance(90) ? random.Below(0x80) : uint32_t(random.Next()) & 0xFFF;
            if (random.Chance(50)) Call(side, kRtlFillMemoryUlong, { random.Chance(95) ? kBuffer : 0, length, pattern });
            else
            {
                for (uint32_t i = 0; i < 0x40; i++) Store32(side, kBuffer + i * 4, i < random.Below(0x40) ? pattern : ~pattern);
                Call(side, kRtlCompareMemoryUlong, { random.Chance(95) ? kBuffer : 0, std::min<uint32_t>(length, 0x100), pattern });
            }
            break;
        }
        case 22:
        {
            // What one executable leaves the next, and the video calls.
            if (random.Chance(50))
            {
                const uint32_t size = random.Below(64);
                Fill(side, random, kSmall + 0x500, size);
                Call(side, kXamLoaderSetLaunchData, { random.Chance(90) ? kSmall + 0x500 : 0, size });
            }
            std::memset(side.base + kSmall + 0x600, 0xCC, 0x80);
            Call(side, kXamLoaderGetLaunchData, { kSmall + 0x600, random.Below(0x80) });
            std::memset(side.base + kBuffer, 0xCC, 0x200);
            const uint32_t out = random.Chance(90) ? kBuffer : 0;
            switch (random.Below(4))
            {
            case 0: Call(side, kVdQueryVideoMode, { out }); Call(side, kXGetVideoMode, { out }); break;
            case 1: Call(side, kVdGetCurrentDisplayInformation, { out }); Call(side, kVdQueryVideoFlags, {}); break;
            case 2: Call(side, kVdGetCurrentDisplayGamma, { out, random.Chance(90) ? Out(6) : 0 }); Call(side, kVdGetSystemCommandBuffer, { random.Chance(90) ? Out(6) : 0, random.Chance(90) ? Out(7) : 0 }); break;
            default:
                Call(side, kVdInitializeRingBuffer, { uint32_t(random.Next()), random.Below(20) });
                Call(side, kVdEnableRingBufferRPtrWriteBack, { uint32_t(random.Next()), random.Below(8) });
                Call(side, kVdSetGraphicsInterruptCallback, { 0x82100000, uint32_t(random.Next()) });
                Call(side, kVdInitializeEngines, {});
                Call(side, kVdShutdownEngines, {});
                Call(side, kVdSetDisplayMode, { 0 });
                Call(side, kVdSetSystemCommandBufferGpuIdentifierAddress, { 0 });
                Call(side, kVdCallGraphicsNotificationRoutines, { 0 });
                Call(side, kVdIsHSIOTrainingSucceeded, {});
                Call(side, kVdPersistDisplay, { 0 });
                Call(side, kVdEnableDisableClockGating, { 0 });
                Call(side, kVdRetrainEDRAM, { 0 });
                Call(side, kVdRetrainEDRAMWorker, { 0 });
                Call(side, kVdInitializeScalerCommandBuffer, { 0 });
                Call(side, kMmGetPhysicalAddress, { random.Chance(50) ? 0x8A000000u + random.Below(0x100000) : uint32_t(random.Next()) });
                break;
            }
            // A swap: its 64 words, the front buffer's fetch constant and where its address is.
            Store32(side, kSmall + 0x700, uint32_t(random.Next()));
            Store32(side, kSmall + 0x704, random.Chance(80) ? 0xBC000000u + random.Below(0x1000000) : uint32_t(random.Next()));
            Store32(side, kSmall + 0x708, uint32_t(random.Next()));
            Call(side, kVdSwap, { random.Chance(90) ? kBuffer : 0, random.Chance(90) ? kSmall + 0x700 : 0, 0, 0, 0, random.Chance(90) ? kSmall + 0x704 : 0 });
            break;
        }
        default:
        {
            // The controllers: none is plugged in, and the script stands in for some.
            const uint32_t user = random.Chance(80) ? random.Below(4) : random.Below(8);
            std::memset(side.base + Out(4), 0xCC, 32);
            switch (random.Below(3))
            {
            case 0: Call(side, kXamInputGetState, { user, 0, random.Chance(95) ? Out(4) : 0 }); break;
            case 1: Call(side, kXamInputGetCapabilities, { user, 1, random.Chance(95) ? Out(4) : 0 }); break;
            default:
                Store32(side, kSmall + 0x800, uint32_t(random.Next()));
                Call(side, kXamInputSetState, { user, 0, random.Chance(90) ? kSmall + 0x800 : 0 });
                break;
            }
            break;
        }
        }
    }

    // ---- XAM and the network -----------------------------------------------------

    constexpr uint32_t kXam = kArena + 0x34000;
    constexpr uint32_t kXamOut = kXam;                  // what calls write back
    constexpr uint32_t kXamMessage = kXam + 0x200;      // a message's buffer
    constexpr uint32_t kXamInfo = kXam + 0x400;         // XSESSION_INFO, a nonce, an XNADDR, XNKIDs
    constexpr uint32_t kXamResults = kXam + 0x800;      // a search's results
    constexpr uint32_t kXamLists = kXam + 0x1000;       // XUIDs, flags, contexts, properties, XNKID pointers
    constexpr uint32_t kXamTask = kXam + 0x1800;        // a Live storage message, its task and request
    constexpr uint32_t kXamData = kXam + 0x2000;        // what storage reads into and writes from
    constexpr uint32_t kXamText = kXam + 0x4000;        // strings
    constexpr uint32_t kNetBuffer = kXam + 0x5000;      // what a socket sends and receives
    constexpr uint32_t kNetAddress = kXam + 0x5800;     // sockaddr_in, to and from
    constexpr uint32_t kSearchBlocks = kXam + 0x5900;   // XOVERLAPPED of the searches, 32 bytes each

    void StoreText(Side& side, uint32_t at, const std::string& text)
    {
        std::memcpy(side.base + at, text.c_str(), text.size() + 1);
    }

    void StoreWide(Side& side, uint32_t at, const std::string& text)
    {
        for (size_t i = 0; i <= text.size(); i++)
        {
            const uint16_t c = i < text.size() ? uint16_t(uint8_t(text[i])) : 0;
            side.base[at + 2 * i] = uint8_t(c >> 8);
            side.base[at + 2 * i + 1] = uint8_t(c);
        }
    }

    void StoreBytes(Side& side, Random& random, uint32_t at, uint32_t size)
    {
        for (uint32_t i = 0; i < size; i++) side.base[at + i] = uint8_t(random.Next());
    }

    // A peer's id, as the service names peers: this machine, one of a few others, or none.
    uint64_t PickPeer(Random& random)
    {
        static const uint64_t kPeers[] = { kLocalId, 0x1111, 0x2222, 0x0000123456789ABCull, 0 };
        return kPeers[random.Below(5)];
    }

    // An XNADDR as the title passes one: its own, one naming a peer, or one from nowhere.
    void StoreXnAddr(Side& side, Random& random, uint32_t at)
    {
        StoreBytes(side, random, at, 36);
        if (random.Chance(70))
        {
            const uint64_t peer = PickPeer(random);
            std::memcpy(side.base + at + 16, random.Chance(90) ? "MW2O" : "MW2X", 4);
            Store64(side, at + 20, peer);
        }
    }

    // XSESSION_INFO: an id, a host's XNADDR, a key.
    void StoreSessionInfo(Side& side, Random& random, uint32_t at)
    {
        StoreBytes(side, random, at, 8);
        if (random.Chance(30)) side.base[at + 7] |= 1;   // a Find Game session's
        StoreXnAddr(side, random, at + 8);
        StoreBytes(side, random, at + 44, 16);
    }

    uint32_t PickIn(Side& side, Random& random)
    {
        if (!side.standIns.empty() && random.Chance(50)) return side.standIns[random.Below(uint32_t(side.standIns.size()))];
        static const uint32_t kAddresses[] = { 0x7F000001, 0xFFFFFFFF, 0x0A000001, 0x0A000002, 0xC0A80129, 0 };
        return kAddresses[random.Below(6)];
    }

    uint32_t PickUser(Random& random)
    {
        static const uint32_t kUsers[] = { 0, 0, 1, 2, 3, 4, 0xFF, 0x12345678 };
        return kUsers[random.Below(8)];
    }

    uint32_t PickKnown(Side& side, Random& random, Known::Kind kind)
    {
        std::vector<uint32_t> handles;
        for (const Known& one : side.known)
            if (one.kind == kind) handles.push_back(one.handle);
        if (handles.empty() || random.Chance(10)) return PickHandle(side, random, true);
        return handles[random.Below(uint32_t(handles.size()))];
    }

    uint32_t PickSessionObject(Side& side, Random& random)
    {
        if (side.sessionObjects.empty() || random.Chance(10)) return random.Chance(50) ? kXamOut + 0x100 : uint32_t(random.Next());
        return side.sessionObjects[random.Below(uint32_t(side.sessionObjects.size()))];
    }

    // A message to XAM, the way the title sends it: in process, or started with an
    // XOVERLAPPED it then polls.
    void Message(Side& side, Random& random, uint32_t app, uint32_t message, uint32_t param1, uint32_t param2)
    {
        if (random.Chance(50)) Call(side, kXMsgInProcessCall, { app, message, param1, param2 });
        else Call(side, kXMsgStartIORequest, { app, message, Overlapped(side, random), param1, param2 });
    }

    // XSESSION_*_FLAGS as the title uses them, and some it does not.
    uint32_t SessionFlags(Random& random)
    {
        static const uint32_t kFlags[] = { 0, 0x1, 0x29, 0x2D, 0x0D, 0x0C, 0x28, 0x109, 0x40D, 0x40C, 0x3, 0x10 };
        return random.Chance(95) ? kFlags[random.Below(12)] : uint32_t(random.Next());
    }

    void SessionStep(Side& side, Random& random)
    {
        const uint32_t message = kXamMessage;
        std::memset(side.base + message, 0, 0x200);
        switch (random.Below(12))
        {
        case 0: case 1:
        {
            // XSessionCreate: joining a session found carries its info and nonce in.
            const uint32_t flags = SessionFlags(random);
            const uint32_t info = random.Chance(90) ? kXamInfo : 0, nonce = random.Chance(90) ? kXamInfo + 64 : 0;
            StoreSessionInfo(side, random, kXamInfo);
            StoreBytes(side, random, kXamInfo + 64, 8);
            const uint32_t words[] = { PickSessionObject(side, random), flags, random.Below(18), random.Below(18), 0, info, nonce };
            for (uint32_t i = 0; i < 7; i++) Store32(side, message + 4 * i, words[i]);
            Message(side, random, 0xFB, 0x000B0010, random.Chance(95) ? message : 0, random.Chance(95) ? 28 : random.Below(28));
            break;
        }
        case 2:
        {
            static const uint32_t kMessages[] = { 0x000B0011, 0x000B0014, 0x000B0015, 0x000B001F };
            Store32(side, message, PickSessionObject(side, random));
            Message(side, random, 0xFB, kMessages[random.Below(4)], random.Chance(95) ? message : 0, 8);
            break;
        }
        case 3:
        {
            // Players joining or leaving, in public or private slots.
            const uint32_t count = random.Below(4);
            for (uint32_t i = 0; i < count; i++)
            {
                Store64(side, kXamLists + 8 * i, 0x0009000000000000ull | random.Below(6));
                Store32(side, kXamLists + 0x40 + 4 * i, random.Below(2));
            }
            const bool join = random.Chance(60);
            const uint32_t words[] = { PickSessionObject(side, random), count, random.Chance(95) ? kXamLists : 0, 0,
                                       random.Chance(80) ? kXamLists + 0x40 : 0 };
            for (uint32_t i = 0; i < 5; i++) Store32(side, message + 4 * i, words[i]);
            Message(side, random, 0xFB, join ? 0x000B0012 : 0x000B0013, message, random.Chance(95) ? 20 : random.Below(20));
            break;
        }
        case 4:
        {
            const uint32_t words[] = { PickSessionObject(side, random), SessionFlags(random), random.Below(18), random.Below(18) };
            for (uint32_t i = 0; i < 4; i++) Store32(side, message + 4 * i, words[i]);
            Message(side, random, 0xFB, 0x000B0018, message, random.Chance(95) ? 16 : random.Below(16));
            break;
        }
        case 5:
        {
            // A migration: this machine hosts now, or another does.
            StoreSessionInfo(side, random, kXamInfo);
            const uint32_t words[] = { PickSessionObject(side, random), random.Chance(95) ? kXamInfo : 0, random.Chance(50) ? 0u : 0xFFu };
            for (uint32_t i = 0; i < 3; i++) Store32(side, message + 4 * i, words[i]);
            Message(side, random, 0xFB, 0x000B001E, message, random.Chance(95) ? 12 : random.Below(12));
            break;
        }
        case 6:
        {
            // XSessionSearch, which the title starts with an XOVERLAPPED it polls.
            static const uint32_t kIds[] = { 0x800B, 0x800A, 0x0006, 0x10000014, 0x10000015, 0x10000017, 0x1000001A, 0x5 };
            const uint32_t contexts = random.Below(4), properties = random.Below(5);
            for (uint32_t i = 0; i < contexts; i++)
            {
                Store32(side, kXamLists + 8 * i, kIds[random.Below(8)]);
                Store32(side, kXamLists + 8 * i + 4, random.Below(5));
            }
            for (uint32_t i = 0; i < properties; i++)
            {
                const uint32_t at = kXamLists + 0x100 + 24 * i;
                std::memset(side.base + at, 0, 24);
                Store32(side, at, kIds[random.Below(8)]);
                side.base[at + 8] = random.Chance(80) ? 1 : 6;
                Store32(side, at + 16, random.Below(20));
            }
            const uint32_t block = kSearchBlocks + 32 * random.Below(4);
            std::memset(side.base + block, 0, 32);
            Store32(side, block + 12, PickKnown(side, random, Known::kEvent));
            const uint32_t capacity = random.Chance(80) ? 8 + 92 * random.Below(6) : random.Below(12);
            const uint32_t words[] = { random.Below(3), 0, random.Below(8), properties << 16 | contexts,
                                       random.Chance(90) ? kXamLists + 0x100 : 0, random.Chance(90) ? kXamLists : 0, capacity,
                                       random.Chance(95) ? kXamResults : 0, 0 };
            for (uint32_t i = 0; i < 9; i++) Store32(side, message + 4 * i, words[i]);
            Call(side, kXMsgStartIORequest, { 0xFB, 0x000B001C, random.Chance(95) ? block : 0, message, random.Chance(95) ? 36 : 20 });
            side.searchBlocks.push_back(block);
            break;
        }
        case 7:
        {
            // The service answers the last search, or one already answered or cancelled.
            if (!side.searches) break;
            const uint32_t search = random.Chance(80) ? side.searches : 1 + random.Below(side.searches);
            std::vector<online::SessionFound>& found = side.answered[search];
            found.clear();
            const uint32_t count = random.Below(5);
            for (uint32_t i = 0; i < count; i++)
            {
                online::SessionFound one;
                one.host = PickPeer(random);
                StoreSessionInfo(side, random, kXamOut + 0x180);
                std::memcpy(one.session, side.base + kXamOut + 0x180, 60);
                if (i && random.Chance(20)) std::memcpy(one.session, found[0].session, 8);   // the same session twice
                one.openPublic = random.Below(18);
                one.openPrivate = random.Below(18);
                one.filledPublic = random.Below(18);
                one.filledPrivate = random.Below(18);
                one.lag = random.Chance(70) ? random.Below(300) : uint32_t(random.Next());
                found.push_back(one);
            }
            Note(side, "the service answers search %u with %u", search, count);
            break;
        }
        case 8:
        {
            uint32_t block = side.searchBlocks.empty() || random.Chance(20) ? kSearchBlocks + 32 * random.Below(4)
                                                                             : side.searchBlocks[random.Below(uint32_t(side.searchBlocks.size()))];
            Call(side, kXMsgCancelIORequest, { random.Chance(95) ? block : 0, 0 });
            break;
        }
        case 9:
        {
            // XUserSetContext and XUserSetProperty, whose numbers a search compares.
            static const uint32_t kIds[] = { 0x800B, 0x800A, 0x0006, 0x10000014, 0x10000015, 0x10000017, 0x1000001A, 0x5, 0x10000099 };
            static const uint64_t kXuids[] = { 0, 0, 0xE000000000000042ull, 0x0009000000000042ull, 0xE000000000000099ull };
            const uint64_t xuid = kXuids[random.Below(5)];
            if (random.Chance(50))
            {
                const uint32_t words[] = { PickUser(random), 0, uint32_t(xuid >> 32), uint32_t(xuid), kIds[random.Below(9)], random.Below(6) };
                for (uint32_t i = 0; i < 6; i++) Store32(side, message + 4 * i, words[i]);
                Message(side, random, 0xFB, 0x000B0006, random.Chance(95) ? message : 0, 24);
            }
            else
            {
                Store32(side, kXamLists, random.Below(30));
                const uint32_t words[] = { PickUser(random), 0, uint32_t(xuid >> 32), uint32_t(xuid), kIds[random.Below(9)],
                                           random.Chance(80) ? 4u : 8u, random.Chance(90) ? kXamLists : 0 };
                for (uint32_t i = 0; i < 7; i++) Store32(side, message + 4 * i, words[i]);
                Message(side, random, 0xFB, 0x000B0007, random.Chance(95) ? message : 0, 28);
            }
            break;
        }
        case 10:
        {
            // A message nothing here answers, said once.
            static const uint32_t kApps[] = { 0xFB, 0xFC, 0xF0, 0xFE };
            Message(side, random, kApps[random.Below(4)], random.Chance(70) ? 0x000B0000 | random.Below(8) : random.Below(0x60000), message, 0);
            break;
        }
        default:
        {
            // A frame: the searches the service answered, and an invitation accepted.
            if (side.name[0] == 'C') online::Frame();
            else rig_online_frame();
            break;
        }
        }
    }

    // Live itself: storage, the NAT, invitations.
    void LiveStep(Side& side, Random& random)
    {
        static const char* const kPaths[] = {
            "//title.41560817/t:41560817/motd-english.txt", "//title.41560817/t:41560817/playlists.patch2",
            "//title.41560817/t:41560817/..", "//title.41560817/t:41560817/a:b",
            "//tuser.41560817/u:0009000000000042/41560817/mpdata", "//tuser.41560817/u:0009000000001234/41560817/stats",
            "//tuser.41560817/u:0009000000000042/41560818/mpdata", "//tuser.41560817/u:0009ZZ0000000042/41560817/mpdata",
            "//tuser.41560817/u:0009000000000042/41560817/", "//tuser.41560817/u:00090000000042/41560817/x",
            "//tuser.41560817/u:0009000000000042/41560817/sub/x", "//elsewhere/x", "",
        };
        switch (random.Below(8))
        {
        case 0: case 1: case 2:
        {
            // XStorageDownloadToMemory and XStorageUploadFromMemory.
            const std::string path = kPaths[random.Below(sizeof kPaths / sizeof *kPaths)];
            const uint32_t task = kXamTask, request = kXamTask + 0x100;
            std::memset(side.base + task, 0, 0x700);
            Store32(side, kXamMessage, random.Chance(95) ? task : 0);
            Store32(side, task + 0x18, random.Chance(95) ? request : 0);
            const uint32_t chars = random.Chance(90) ? uint32_t(path.size()) : random.Below(70);
            Store32(side, request + 4, chars);
            StoreWide(side, request + 8, path);
            const uint32_t room = random.Chance(70) ? 0x1000 : random.Below(40);
            const bool upload = random.Chance(40);
            Store32(side, request + 8 + chars * 2, upload ? random.Below(300) : room);
            Store32(side, request + 8 + chars * 2 + 4, random.Chance(95) ? kXamData : 0);
            Store32(side, task + 0x1C, random.Chance(90) ? 8 + chars * 2 + 8 : random.Below(40));
            Store32(side, task + 0x2C, random.Chance(90) ? kXamTask + 0x600 : 0);
            Store32(side, task + 0x30, random.Chance(90) ? 20 : random.Below(20));
            if (upload) StoreBytes(side, random, kXamData, 300);
            Message(side, random, 0xFC, upload ? 0x0005000B : 0x00050009, kXamMessage, 0);
            break;
        }
        case 3:
        {
            // XStorageBuildServerPath.
            static const char* const kNames[] = { "mpdata", "stats", "", "a/b", "x:y" };
            const uint32_t at = kXamMessage;
            std::memset(side.base + at, 0, 40);
            Store32(side, at, random.Chance(70) ? 0 : 1);
            Store64(side, at + 8, 0x0009000000001234ull);
            Store32(side, at + 16, 2 + random.Below(3));
            std::string name = random.Chance(10) ? std::string(200, 'n') + "\xC3" : kNames[random.Below(5)];
            StoreWide(side, kXamText, name);
            if (random.Chance(10)) { side.base[kXamText + 2] = 0x20; side.base[kXamText + 3] = 0xAC; }   // a character past ASCII
            Store32(side, at + 28, random.Chance(95) ? kXamText : 0);
            Store32(side, at + 32, random.Chance(90) ? kXamOut : 0);
            Store32(side, kXamOut + 0x1F0, random.Chance(70) ? 0x100 : random.Below(40));
            Store32(side, at + 36, random.Chance(95) ? kXamOut + 0x1F0 : 0);
            std::memset(side.base + kXamOut, 0xCC, 0x1F0);
            Message(side, random, 0xFC, 0x00058035, random.Chance(97) ? at : 0, 0);
            break;
        }
        case 4:
        {
            static const uint32_t kMessages[] = { 0x00058004, 0x00058006, 0x0005800E, 0x00050008 };
            Store32(side, kXamOut, 0xCCCCCCCC);
            Store32(side, kXamOut + 4, 0xCCCCCCCC);
            Message(side, random, 0xFC, kMessages[random.Below(4)], random.Chance(90) ? kXamOut : 0, random.Chance(90) ? kXamOut + 4 : 0);
            break;
        }
        case 5:
        {
            // XInviteGetAcceptedInfo: { size, value } entries, the user and where to write.
            const uint32_t list = kXamMessage;
            std::memset(side.base + list, 0, 32);
            Store32(side, kXamLists, random.Chance(80) ? 0 : 1);
            Store64(side, list + 8, random.Chance(95) ? kXamLists : 0);
            Store64(side, list + 24, random.Chance(95) ? kXamOut : 0);
            std::memset(side.base + kXamOut, 0xCC, 84);
            Message(side, random, 0xFC, 0x00058023, 0, random.Chance(97) ? list : 0);
            break;
        }
        case 6:
        {
            // An invitation accepted: told by the service at a frame, or straight to XAM.
            online::Invite invite;
            invite.inviter = random.Chance(80) ? 0x1234 : uint64_t(random.Next());
            StoreSessionInfo(side, random, kXamOut + 0x180);
            std::memcpy(invite.session, side.base + kXamOut + 0x180, 60);
            invite.fromInvite = random.Chance(50);
            if (random.Chance(50)) side.invites.push_back(invite);
            else if (side.name[0] == 'C') kernel::AcceptInvite(invite.inviter, invite.session, invite.fromInvite);
            else rig_xam_accept_invite(invite.inviter, invite.session, invite.fromInvite);
            Note(side, "invitation from %llx", (unsigned long long)invite.inviter);
            break;
        }
        default:
        {
            // A profile renumbered: what is kept under the old number moves, where the new
            // has none.
            const uint64_t from = random.Below(4), to = random.Below(4);
            if (random.Chance(60))
            {
                char name[40];
                std::snprintf(name, sizeof name, "mpdata_e000%012llx", (unsigned long long)from);
                std::filesystem::create_directories(std::filesystem::path("saves") / name);
                std::ofstream(std::filesystem::path("saves") / name / name) << "rank";
                std::snprintf(name, sizeof name, "0009%012llx", (unsigned long long)from);
                std::filesystem::create_directories(std::filesystem::path("saves") / "online" / "user" / name);
                std::snprintf(name, sizeof name, "profile_%012llx.bin", (unsigned long long)from);
                std::ofstream(std::filesystem::path("saves") / name) << "settings";
            }
            if (side.name[0] == 'C') kernel::MovePlayerData(from, to);
            else rig_xam_move_player_data(from, to);
            Note(side, "moved %llu to %llu", (unsigned long long)from, (unsigned long long)to);
            break;
        }
        }
    }

    // Who is signed in, the system's screens, notifications, enumerators and memory.
    void XamStep(Side& side, Random& random)
    {
        const uint32_t user = PickUser(random);
        const uint32_t out = random.Chance(95) ? kXamOut : 0;
        std::memset(side.base + kXamOut, 0xCC, 0x80);
        switch (random.Below(16))
        {
        case 0: Call(side, kXamUserGetSigninState, { user }); break;
        case 1: Call(side, kXamUserGetXUID, { user, random.Below(8), out }); break;
        case 2:
        {
            static const uint32_t kSizes[] = { 0, 1, 4, 16, 32 };
            Call(side, kXamUserGetName, { user, out, kSizes[random.Below(5)] });
            break;
        }
        case 3: Call(side, kXamUserGetSigninInfo, { user, random.Below(4), out }); break;
        case 4: Call(side, kXamUserCheckPrivilege, { user, random.Below(300), out }); break;
        case 5:
            Call(side, kXamUserAreUsersFriends, { user, kXamLists, 1, out, 0 });
            Call(side, kXamUserCreateStatsEnumerator, { 0x41560817, 0, 0, 1, 1, 0, out, kXamOut + 8 });
            break;
        case 6:
            Call(side, kXamShowDeviceSelectorUI, { user, 1, 0, 0x10000, out, Overlapped(side, random) });
            Call(side, kXamShowMessageBoxUIEx, { user, 0, 0, 0, 0, 0, 0, 0 });
            Call(side, kXamShowGamerCardUIForXUID, { user, 0x0009000000001234ull });
            Call(side, kXNotifyPositionUI, { random.Below(16) });
            if (random.Chance(10)) Call(side, kXamShowDirtyDiscErrorUI, {});
            break;
        case 7:
        {
            static const uint64_t kAreas[] = { ~0ull, 1, 2, 3, 0x80, 0 };
            Store32(side, Out(0), 0);
            const uint64_t handle = Call(side, kXamNotifyCreateListener, { kAreas[random.Below(6)], 1 });
            if (handle) side.known.push_back({ Known::kListener, uint32_t(handle) });
            break;
        }
        case 8: case 9:
        {
            static const uint32_t kIds[] = { 0, 0, 0x0000000A, 0x00000009, 0x02000002, 0x02000001 };
            Call(side, kXNotifyGetNext, { PickKnown(side, random, Known::kListener), kIds[random.Below(6)], out,
                                          random.Chance(90) ? kXamOut + 8 : 0 });
            break;
        }
        case 10:
        {
            static const uint32_t kIds[] = { 0x0000000A, 0x00000009, 0x02000002, 0x02000001, 0x0A000005, 0x04000001 };
            const uint32_t id = kIds[random.Below(6)], param = random.Below(16);
            if (side.name[0] == 'C') kernel::PostNotification(id, param);
            else rig_xam_post_notification(id, param);
            Note(side, "posted %08X %u", id, param);
            break;
        }
        case 11:
        {
            // A session object, which the title names its sessions by.
            const uint64_t status = Call(side, kXamSessionCreateHandle, { out });
            if (status == 0 && out) side.known.push_back({ Known::kSession, Load32(side, out) });
            Call(side, kXamSessionRefObjByHandle, { PickKnown(side, random, Known::kSession), kXamOut + 8 });
            if (Load32(side, kXamOut + 8) != 0xCCCCCCCC && Load32(side, kXamOut + 8))
                side.sessionObjects.push_back(Load32(side, kXamOut + 8));
            break;
        }
        case 12:
        {
            static const uint32_t kExtra[] = { 0, 16, 100, 0x7FFFFFFF };
            uint64_t status;
            if (random.Chance(50))
                status = Call(side, kXamCreateEnumeratorHandle, { user, 0xFE, 0, 0, kExtra[random.Below(4)], 1, 0, out });
            else
                status = Call(side, kXamContentCreateEnumerator, { user, 1, 1, 0, random.Below(5), random.Chance(90) ? kXamOut + 8 : 0, out });
            if (status == 0 && out) side.known.push_back({ Known::kEnumerator, Load32(side, out) });
            break;
        }
        case 13:
        {
            const uint32_t handle = PickKnown(side, random, Known::kEnumerator);
            if (random.Chance(50)) Call(side, kXamGetPrivateEnumStructureFromHandle, { handle, out });
            else Call(side, kXamEnumerate, { handle, 0, kXamData, 0x100, random.Chance(90) ? kXamOut + 8 : 0, Overlapped(side, random) });
            break;
        }
        case 14:
        {
            if (side.xamMemory.empty() || random.Chance(60))
            {
                Call(side, kXamAlloc, { 0, random.Chance(90) ? 1 + random.Below(0x2000) : 0x7FFFFFFF, out });
                if (out && Load32(side, out) && Load32(side, out) != 0xCCCCCCCC) side.xamMemory.push_back(Load32(side, out));
            }
            else
            {
                const uint32_t at = random.Below(uint32_t(side.xamMemory.size()));
                Call(side, kXamFree, { side.xamMemory[at] });
                side.xamMemory.erase(side.xamMemory.begin() + at);
            }
            break;
        }
        default:
        {
            static const uint32_t kSizes[] = { 0, 1, 4, 8 };
            Call(side, kExGetXConfigSetting, { random.Chance(80) ? 3u : 2u, random.Below(5), random.Chance(90) ? kXamOut : 0,
                                               kSizes[random.Below(4)], random.Chance(90) ? kXamOut + 8 : 0 });
            break;
        }
        }
    }

    // A port the title binds: ones the shift leaves, one it moves, and with a service
    // the title's own. With none they are the host's: 1000 would be a copy of the game's.
    uint16_t PickPort(Random& random)
    {
        static const uint16_t kPorts[] = { 1100, 1101, 999, 41100, 1000, 3074 };
        return kPorts[random.Below(g_service ? 6 : 4)];
    }

    Side::Socket* PickSocket(Side& side, Random& random)
    {
        if (side.sockets.empty() || random.Chance(5)) return nullptr;
        return &side.sockets[random.Below(uint32_t(side.sockets.size()))];
    }

    void StoreAddress(Side& side, uint32_t at, uint16_t port, uint32_t address)
    {
        std::memset(side.base + at, 0, 16);
        side.base[at + 1] = 2;   // AF_INET
        side.base[at + 2] = uint8_t(port >> 8);
        side.base[at + 3] = uint8_t(port);
        Store32(side, at + 4, address);
    }

    // Two sockets, one sending to the other, a third asking for a port already taken
    // or for an address that is not this machine's, and one connected where nothing
    // listens: on the host's loopback with no service. All closed before the step ends.
    void Exchange(Side& side, Random& random)
    {
        uint32_t made[4];
        for (uint32_t& one : made)
        {
            one = uint32_t(Call(side, kNetDll_socket, { 0, 2, 2, 17 }));
            Store32(side, kNetAddress + 0x40, 1);
            Call(side, kNetDll_ioctlsocket, { 0, one, 0x8004667E, kNetAddress + 0x40 });
        }
        // Ports of the exchange's own, which no other socket of the case holds: one the
        // shift leaves and one it moves.
        const uint16_t from = random.Chance(50) ? 1102 : 998, to = random.Chance(50) ? 1103 : 997;
        StoreAddress(side, kNetAddress, from, 0);
        Call(side, kNetDll_bind, { 0, made[0], kNetAddress, 16 });
        StoreAddress(side, kNetAddress, to, random.Chance(80) ? 0 : 0x7F000001);
        Call(side, kNetDll_bind, { 0, made[1], kNetAddress, 16 });
        Call(side, kNetDll_WSAGetLastError, {});
        const uint32_t size = random.Below(64);
        StoreBytes(side, random, kNetBuffer, size);
        StoreAddress(side, kNetAddress, to, 0x7F000001);
        Call(side, kNetDll_sendto, { 0, made[0], kNetBuffer, size, 0, kNetAddress, 16 });
        std::memset(side.base + kNetAddress + 0x20, 0xCC, 16);
        Call(side, kNetDll_recvfrom, { 0, made[1], kNetBuffer + 0x100, random.Chance(80) ? 0x80u : random.Below(8), 0,
                                       random.Chance(90) ? kNetAddress + 0x20 : 0, kNetAddress + 0x30 });
        Call(side, kNetDll_WSAGetLastError, {});
        StoreAddress(side, kNetAddress, from, random.Chance(70) ? 0 : 0xC0000201);
        Call(side, kNetDll_bind, { 0, made[2], kNetAddress, 16 });
        Call(side, kNetDll_WSAGetLastError, {});
        StoreAddress(side, kNetAddress, 1199, 0x7F000001);
        Call(side, kNetDll_connect, { 0, made[3], kNetAddress, 16 });
        Call(side, kNetDll_sendto, { 0, made[3], kNetBuffer, size, 0, kNetAddress, 16 });
        Call(side, kNetDll_recvfrom, { 0, made[3], kNetBuffer + 0x100, 0x80, 0, 0, 0 });
        Call(side, kNetDll_WSAGetLastError, {});
        for (uint32_t one : made) Call(side, kNetDll_closesocket, { 0, one });
    }

    // The title's sockets. With no service they are the host's, so nothing here may
    // wait on one, or hear what another socket sent from a port the host chose.
    void SocketStep(Side& side, Random& random)
    {
        if (random.Chance(10)) return Exchange(side, random);
        Side::Socket* socket = PickSocket(side, random);
        const uint32_t handle = socket ? socket->handle : random.Chance(50) ? 0 : uint32_t(random.Next());
        switch (random.Below(10))
        {
        case 0:
        {
            const bool stream = random.Chance(10);
            const uint64_t made = Call(side, kNetDll_socket, { 0, 2, stream ? 1u : 2u, stream ? 6u : 17u });
            if (made == 0xFFFFFFFF) break;
            side.sockets.push_back({ uint32_t(made), stream, false, 0 });
            // As the title makes one: not waiting, and bound. A case is short, and with no
            // service its sockets last no longer.
            Side::Socket& socket = side.sockets.back();
            if (random.Chance(80))
            {
                Store32(side, kNetAddress + 0x40, 1);
                Call(side, kNetDll_ioctlsocket, { 0, socket.handle, 0x8004667E, kNetAddress + 0x40 });
                socket.nonBlocking = true;
            }
            if (!stream && random.Chance(70))
            {
                socket.port = PickPort(random);
                StoreAddress(side, kNetAddress, socket.port, random.Chance(90) ? 0 : 0x7F000001);
                Call(side, kNetDll_bind, { 0, socket.handle, kNetAddress, 16 });
            }
            break;
        }
        case 1:
        {
            // A stream is bound on the host by the C++ alone: not without a service.
            if (!g_service && socket && socket->stream) break;
            const uint16_t port = PickPort(random);
            StoreAddress(side, kNetAddress, port, random.Chance(80) ? 0 : 0x7F000001);
            Call(side, kNetDll_bind, { 0, handle, random.Chance(97) ? kNetAddress : 0, 16 });
            if (socket && !socket->port) socket->port = port;
            break;
        }
        case 2:
        {
            const uint32_t value = random.Below(2);
            Store32(side, kNetAddress + 0x40, value);
            const uint32_t command = random.Chance(90) ? 0x8004667E : 0x4004667F;
            const uint32_t argument = random.Chance(95) ? kNetAddress + 0x40 : 0;
            Call(side, kNetDll_ioctlsocket, { 0, handle, command, argument });
            if (socket && command == 0x8004667E) socket->nonBlocking = argument && value;
            Call(side, kNetDll_setsockopt, { 0, handle, 0xFFFF, 0x20, kNetAddress + 0x40, 4 });
            break;
        }
        case 3: case 4:
        {
            // To another machine, everyone, this one, or nobody.
            uint32_t to;
            uint16_t port;
            if (g_service)
            {
                to = PickIn(side, random);
                port = PickPort(random);
            }
            else
            {
                // A stream that is not connected would have the host raise SIGPIPE.
                if (socket && socket->stream) break;
                // A socket the host bound for it sends; a datagram from a port the host
                // chose goes where nothing listens.
                const bool bound = socket && socket->port && !socket->stream;
                to = random.Chance(20) ? 0xFFFFFFFF : 0x7F000001;
                port = bound ? PickPort(random) : 1199;
            }
            StoreAddress(side, kNetAddress, port, to);
            const uint32_t size = random.Below(64);
            StoreBytes(side, random, kNetBuffer, size);
            Call(side, kNetDll_sendto, { 0, handle, kNetBuffer, size, 0, random.Chance(97) ? kNetAddress : 0, 16 });
            break;
        }
        case 5: case 6:
        {
            // With no service, only a socket that does not wait.
            if (!g_service && !(socket && socket->nonBlocking)) break;
            std::memset(side.base + kNetBuffer, 0xCC, 0x80);
            std::memset(side.base + kNetAddress + 0x20, 0xCC, 16);
            Call(side, kNetDll_recvfrom, { 0, handle, kNetBuffer, random.Chance(80) ? 0x80u : random.Below(8), 0,
                                           random.Chance(80) ? kNetAddress + 0x20 : 0, kNetAddress + 0x30 });
            break;
        }
        case 7:
        {
            // What arrives for a port from the service.
            online::Datagram datagram;
            datagram.from = PickPeer(random);
            datagram.fromPort = PickPort(random);
            datagram.bytes.resize(random.Below(100));
            for (uint8_t& byte : datagram.bytes) byte = uint8_t(random.Next());
            side.incoming[PickPort(random)].push_back(datagram);
            break;
        }
        case 8:
        {
            if (!g_service && (!socket || socket->stream)) break;
            StoreAddress(side, kNetAddress, PickPort(random), random.Chance(80) ? 0x7F000001 : 0xC0A80101);
            Call(side, kNetDll_connect, { 0, handle, random.Chance(97) ? kNetAddress : 0, 16 });
            break;
        }
        default:
        {
            Call(side, kNetDll_closesocket, { 0, handle });
            if (socket) side.sockets.erase(side.sockets.begin() + (socket - side.sockets.data()));
            break;
        }
        }
        Call(side, kNetDll_WSAGetLastError, {});
    }

    void XnetStep(Side& side, Random& random)
    {
        std::memset(side.base + kXamOut, 0xCC, 0x80);
        const uint32_t out = random.Chance(95) ? kXamOut : 0;
        switch (random.Below(9))
        {
        case 0:
            Call(side, kNetDll_XNetGetTitleXnAddr, { 0, out });
            Call(side, kXNetLogonGetTitleID, {});
            Call(side, kNetDll_XNetGetEthernetLinkStatus, { 0 });
            break;
        case 1:
        {
            StoreXnAddr(side, random, kXamInfo);
            if (random.Chance(20)) Call(side, kNetDll_XNetGetTitleXnAddr, { 0, kXamInfo });
            StoreBytes(side, random, kXamInfo + 64, 8);
            Call(side, kNetDll_XNetXnAddrToInAddr, { 0, random.Chance(95) ? kXamInfo : 0, random.Chance(50) ? kXamInfo + 64 : 0, out });
            if (out && Load32(side, out) != 0xCCCCCCCC) side.standIns.push_back(Load32(side, out));
            break;
        }
        case 2:
            Call(side, kNetDll_XNetInAddrToXnAddr, { 0, PickIn(side, random), out, random.Chance(80) ? kXamOut + 0x40 : 0 });
            break;
        case 3:
            Call(side, kNetDll_XNetConnect, { 0, PickIn(side, random) });
            Call(side, kNetDll_XNetGetConnectStatus, { 0, PickIn(side, random) });
            Call(side, kNetDll_XNetUnregisterInAddr, { 0, PickIn(side, random) });
            Call(side, kNetDll_XNetServerToInAddr, { 0, PickIn(side, random), 0, out });
            Call(side, kNetDll_XNetXnAddrToMachineId, { 0, kXamInfo, out });
            Call(side, kNetDll_XNetQosListen, { 0, kXamInfo, 0, 0, 0 });
            break;
        case 4:
        {
            Call(side, kNetDll_XNetQosServiceLookup, { 0, 0, PickKnown(side, random, Known::kEvent), out });
            if (out && Load32(side, out) != 0xCCCCCCCC) side.qos.push_back(Load32(side, out));
            break;
        }
        case 5:
        {
            // Probes of what searches found, by their XNKIDs.
            const uint32_t count = random.Chance(90) ? random.Below(5) : 65;
            for (uint32_t i = 0; i < count && i < 8; i++)
            {
                const uint32_t kid = kXamLists + 0x200 + 8 * i;
                if (random.Chance(60)) std::memcpy(side.base + kid, side.base + kXamResults + 8 + 92 * random.Below(4), 8);
                else StoreBytes(side, random, kid, 8);
                Store32(side, kXamLists + 0x300 + 4 * i, random.Chance(90) ? kid : 0);
            }
            Store32(side, kStack + 0x14 + 8 * 11, PickKnown(side, random, Known::kEvent));
            Store32(side, kStack + 0x14 + 8 * 12, out);
            Call(side, kNetDll_XNetQosLookup, { 0, count, kXamInfo, random.Chance(95) ? kXamLists + 0x300 : 0, 0, 0, 0, 0 });
            if (out && Load32(side, out) != 0xCCCCCCCC) side.qos.push_back(Load32(side, out));
            break;
        }
        case 6:
        {
            if (side.qos.empty()) break;
            const uint32_t at = random.Below(uint32_t(side.qos.size()));
            Call(side, kNetDll_XNetQosRelease, { 0, side.qos[at] });
            side.qos.erase(side.qos.begin() + at);
            break;
        }
        case 7:
            Call(side, kNetDll_XNetRandom, { 0, random.Chance(95) ? kXamOut : 0, random.Below(0x80) });
            Call(side, kNetDll_XNetStartup, { 0, 0 });
            Call(side, kNetDll_WSAStartup, { 0, random.Chance(50) ? 0x0202u : 0u, random.Chance(90) ? kXamData : 0 });
            Call(side, kNetDll_WSACleanup, { 0 });
            break;
        default:
        {
            static const char* const kTexts[] = { "127.0.0.1", "192.168.1.41", "255.255.255.255", "1.2.3", "01.2.3.4",
                                                  "256.1.1.1", "", "abc", " 1.2.3.4", "1.2.3.4 ", "0.0.0.0", "1.2.3.4.5" };
            StoreText(side, kXamText, kTexts[random.Below(12)]);
            Call(side, kNetDll_inet_addr, { random.Chance(95) ? kXamText : 0 });
            break;
        }
        }
    }

    void NetXamStep(Side& side, Random& random)
    {
        switch (random.Below(6))
        {
        case 0: case 1: return SessionStep(side, random);
        case 2: return LiveStep(side, random);
        case 3: return XamStep(side, random);
        case 4: return SocketStep(side, random);
        default: return XnetStep(side, random);
        }
    }

    void Step(Side& side, Random& random)
    {
        const uint32_t me = side.mainBlock;
        const uint32_t pick = random.Below(75);
        if (pick >= 60) return NetXamStep(side, random);
        if (pick >= 40) return FileStep(side, random);
        switch (pick)
        {
        case 0: case 1:
            Store32(side, Out(0), 0);
            Remember(side, Known::kEvent, Call(side, kNtCreateEvent, { random.Chance(95) ? Out(0) : 0, 0, random.Below(3), random.Below(3) }));
            break;
        case 2:
            Call(side, kNtSetEvent, { PickHandle(side, random, true), random.Chance(70) ? Out(1) : 0 });
            Note(side, "previous %08X", Load32(side, Out(1)));
            break;
        case 3:
            Call(side, kNtClearEvent, { PickHandle(side, random, true) });
            break;
        case 4:
        {
            Store32(side, Out(0), 0);
            Remember(side, Known::kMutant, Call(side, kNtCreateMutant, { Out(0), 0, random.Below(2) }));
            if (side.records >= kRecordCount || random.Chance(50)) break;
            // Another thread takes it, or a mutant header the title wrote, and ends
            // holding it: the script then releases what it does not own, and waits.
            const uint32_t record = kRecords + side.records++ * kRecordSize;
            std::memset(side.base + record, 0, kRecordSize);
            const bool byHeader = random.Chance(50);
            const uint32_t header = kHeaders + random.Below(kHeaderCount) * 32;
            if (byHeader)
            {
                side.base[header] = 2;
                Store32(side, header + 4, 1);
                Store32(side, header + 8, header + 8);
                Store32(side, header + 12, header + 8);
                Call(side, kKeResetEvent, { header });   // taken up by the script, not taken
            }
            const uint32_t mutant = Load32(side, Out(0));
            Store32(side, record, byHeader ? header : mutant);
            Store32(side, record + 12, byHeader);
            if (Call(side, kExCreateThread, { Out(1), 0, 0, 0, kTaker, record, 1 }) != kSuccess) break;
            const uint32_t thread = Load32(side, Out(1));
            Call(side, kNtResumeThread, { thread, 0 });
            Call(side, kNtWaitForSingleObjectEx, { thread, 1, 0, 0 });
            Note(side, "taker got %08X done %08X", Load32(side, record + 4), Load32(side, record + 8));
            Store64(side, kTimeouts + 16, 0);
            if (byHeader) Call(side, kKeWaitForSingleObject, { header, 0, 1, 0, kTimeouts + 16 });
            else
            {
                Call(side, kNtReleaseMutant, { mutant, Out(2) });
                Note(side, "previous %08X", Load32(side, Out(2)));
                Call(side, kNtWaitForSingleObjectEx, { mutant, 1, 0, kTimeouts + 16 });
            }
            Call(side, kNtClose, { thread });
            break;
        }
        case 5:
            Call(side, kNtReleaseMutant, { PickHandle(side, random, true), random.Chance(70) ? Out(1) : 0 });
            Note(side, "previous %08X", Load32(side, Out(1)));
            break;
        case 6:
            Store32(side, Out(0), 0);
            Remember(side, Known::kTimer, Call(side, kNtCreateTimer, { Out(0), 0, random.Below(3) }));
            break;
        case 7:
        {
            // A timer is set to fall due soon and waited for, or far off and cancelled.
            uint32_t timer = 0;
            for (const Known& one : side.known)
                if (one.kind == Known::kTimer && random.Chance(40)) timer = one.handle;
            if (!timer) break;
            const bool soon = random.Chance(70);
            const uint32_t period = soon && random.Chance(30) ? 1 + random.Below(3) : 0;
            const int64_t due = soon ? (random.Chance(50) ? -int64_t(random.Below(2000)) : int64_t(kSystemTime - random.Below(1000)))
                                     : -int64_t(600000000);
            Store64(side, kTimeouts + 8, uint64_t(due));
            const uint64_t set = Call(side, kNtSetTimerEx, { timer, random.Chance(97) ? kTimeouts + 8 : 0, random.Chance(20) ? 0x82000100u : 0u, 0, 0, 0, period });
            if (!soon)
            {
                // Not signalled once set, whatever it was before.
                Store64(side, kTimeouts + 16, 0);
                Call(side, kNtWaitForSingleObjectEx, { timer, 1, 0, kTimeouts + 16 });
            }
            if (soon && set == kSuccess) Call(side, kNtWaitForSingleObjectEx, { timer, 1, 0, Forever(side, random, 2) });
            if (!soon || period)
            {
                Call(side, kNtCancelTimer, { timer, Out(1) });
                Note(side, "was running %08X", Load32(side, Out(1)));
                // A period may have signalled it again before the cancel: settle that.
                Call(side, kNtClearEvent, { timer });
            }
            break;
        }
        case 8:
            Call(side, kNtCancelTimer, { PickHandle(side, random, true), random.Chance(80) ? Out(1) : 0 });
            break;
        case 9: case 10: case 11:
        {
            const uint32_t handle = PickHandle(side, random, true);
            if (!handle) break;
            // A timeout of 2 ms at most ends long before this, however busy the machine.
            const auto from = std::chrono::steady_clock::now();
            if (Call(side, kNtWaitForSingleObjectEx, { handle, 1, random.Below(2), ShortTimeout(side, random, 0) }) == kTimeout
                && std::chrono::steady_clock::now() - from > std::chrono::milliseconds(150))
                Note(side, "a short timeout ran long");
            break;
        }
        case 12: case 13:
        {
            const uint32_t count = random.Chance(90) ? 1 + random.Below(6) : random.Below(70);
            bool usable = true;
            for (uint32_t i = 0; i < std::min<uint32_t>(count, 70); i++)
            {
                const uint32_t handle = PickHandle(side, random, true);
                if (!handle) usable = false;
                Store32(side, kLists + i * 4, handle);
            }
            if (!usable) break;
            Call(side, kNtWaitForMultipleObjectsEx,
                 { count, random.Chance(97) ? kLists : 0, random.Below(2), 1, random.Below(2), ShortTimeout(side, random, 0) });
            break;
        }
        case 14:
            Call(side, kKeSetEvent, { PickHeader(side, random), 1, 0 });
            break;
        case 15:
            Call(side, kKeResetEvent, { PickHeader(side, random) });
            break;
        case 16:
            Call(side, kKeWaitForSingleObject, { PickHeader(side, random), 0, 1, random.Below(2), ShortTimeout(side, random, 0) });
            break;
        case 17:
        {
            const uint32_t count = random.Chance(90) ? 1 + random.Below(5) : random.Below(70);
            for (uint32_t i = 0; i < std::min<uint32_t>(count, 70); i++)
                Store32(side, kLists + i * 4, random.Chance(95) ? PickHeader(side, random) : uint32_t(random.Below(3)));
            Call(side, kKeWaitForMultipleObjects, { count, kLists, random.Below(2), 0, 1, random.Below(2), ShortTimeout(side, random, 0), 0 });
            break;
        }
        case 18:
            Call(side, kKeInitializeSemaphore, { kHeaders + random.Below(kHeaderCount) * 32, random.Below(4), random.Chance(80) ? 1 + random.Below(4) : uint32_t(random.Next()) });
            break;
        case 19:
            Call(side, kKeReleaseSemaphore, { PickHeader(side, random), 1, random.Chance(80) ? random.Below(3) : uint32_t(random.Next()), 0 });
            break;
        case 20:
            TitleHeader(side, random, kHeaders + random.Below(kHeaderCount) * 32);
            break;
        case 21:
        {
            const uint32_t handle = PickHandle(side, random, true);
            if (!handle) break;
            if (Call(side, kNtClose, { handle }) == kSuccess)
                side.known.erase(std::remove_if(side.known.begin(), side.known.end(), [&](const Known& one) { return one.handle == handle; }), side.known.end());
            break;
        }
        case 22:
            Store32(side, Out(0), 0);
            Remember(side, Known::kDuplicate, Call(side, kNtDuplicateObject, { PickHandle(side, random, true), random.Chance(90) ? Out(0) : 0, 0 }));
            break;
        case 23:
            Store32(side, Out(1), 0);
            if (Call(side, kObReferenceObjectByHandle, { PickHandle(side, random, true), 0, random.Chance(90) ? Out(1) : 0 }) == kSuccess
                && Load32(side, Out(1)) >= 0x84600000 && Load32(side, Out(1)) < 0xA0000000)
                side.referenced.push_back(Load32(side, Out(1)));
            Note(side, "header %08X", Load32(side, Out(1)));
            break;
        case 24:
        {
            // A critical section is entered only when that cannot block.
            const uint32_t cs = kSections + random.Below(kSectionCount) * 32;
            const uint32_t who = random.Chance(70) ? me : side.otherBlocks[random.Below(3)];
            const uint32_t kthread = Load32(side, who + 256);
            switch (random.Below(6))
            {
            case 0: Call(side, kRtlInitializeCriticalSection, { cs }); break;
            case 1: Call(side, kRtlInitializeCriticalSectionAndSpinCount, { cs, random.Chance(50) ? random.Below(2000) : uint32_t(random.Next()) }); break;
            case 2:
                if (Load32(side, cs + 16) == 0xFFFFFFFF || Load32(side, cs + 24) == kthread) Call(side, kRtlEnterCriticalSection, { cs }, who);
                break;
            case 3:
                if (Load32(side, cs + 24) == kthread) Call(side, kRtlLeaveCriticalSection, { cs }, who);
                break;
            default: Call(side, kRtlTryEnterCriticalSection, { cs }, who); break;
            }
            Note(side, "cs %08X %08X %08X %08X", Load32(side, cs), Load32(side, cs + 16), Load32(side, cs + 20), Load32(side, cs + 24));
            break;
        }
        case 25:
        {
            const uint32_t slot = random.Below(12);
            switch (random.Below(4))
            {
            case 0: Call(side, kKeTlsAlloc, {}); break;
            case 1: Call(side, kKeTlsFree, { random.Chance(90) ? slot : uint32_t(random.Next()) }); break;
            case 2: Call(side, kKeTlsGetValue, { random.Chance(90) ? slot : uint32_t(random.Next()) }); break;
            default: Call(side, kKeTlsSetValue, { random.Chance(95) ? slot : random.Below(300), uint32_t(random.Next()) }); break;
            }
            break;
        }
        case 26:
        {
            const uint32_t lock = kSpinLocks + random.Below(kSpinLockCount) * 4;
            const uint32_t who = random.Chance(70) ? me : side.otherBlocks[random.Below(3)];
            switch (random.Below(7))
            {
            case 0: if (!Load32(side, lock)) Call(side, kKfAcquireSpinLock, { lock }, who); break;
            case 1: if (!Load32(side, lock)) Call(side, kKeAcquireSpinLockAtRaisedIrql, { lock }, who); break;
            case 2: Call(side, kKfReleaseSpinLock, { lock, 0 }, who); break;
            case 3: Call(side, kKeReleaseSpinLockFromRaisedIrql, { lock }, who); break;
            case 4: Call(side, kKeTryToAcquireSpinLockAtRaisedIrql, { lock }, who); break;
            case 5: Call(side, kKeRaiseIrqlToDpcLevel, {}); Call(side, kKfLowerIrql, { 0 }); break;
            default: Call(side, kKeEnterCriticalRegion, {}); Call(side, kKeLeaveCriticalRegion, {}); break;
            }
            Note(side, "lock %08X", Load32(side, lock));
            break;
        }
        case 27: case 28:
        {
            // A thread, made suspended so that it logs nothing before ExCreateThread does.
            if (side.records >= kRecordCount) break;
            const uint32_t record = kRecords + side.records++ * kRecordSize;
            std::memset(side.base + record, 0, kRecordSize);
            const uint32_t viaShim = random.Chance(50);
            uint32_t start = random.Chance(90) ? kThreadRoutine : (random.Chance(50) ? kNoFunction : uint32_t(random.Next()));
            const uint32_t flags = 1 | (random.Chance(50) ? (1u << (24 + random.Below(6))) : 0) | (random.Chance(10) ? uint32_t(random.Next()) & 0x00FFFFFE : 0);
            const uint32_t stack = random.Chance(70) ? 0 : (random.Chance(50) ? 0x8000 : 0x100000);
            Store32(side, Out(0), 0);
            const uint32_t status = uint32_t(Call(side, kExCreateThread,
                { random.Chance(95) ? Out(0) : 0, stack, random.Chance(80) ? Out(1) : 0, viaShim ? kShim : 0, start, record, flags }));
            if (status != kSuccess || !Load32(side, Out(0))) break;
            Note(side, "thread id %u", Load32(side, Out(1)));
            side.known.push_back({ Known::kThread, Load32(side, Out(0)), true, record });
            if (random.Chance(60))
            {
                Known& thread = side.known.back();
                if (random.Chance(50))
                {
                    Call(side, kNtResumeThread, { thread.handle, random.Chance(80) ? Out(1) : 0 });
                }
                else
                {
                    Call(side, kObReferenceObjectByHandle, { thread.handle, 0, Out(2) });
                    if (random.Chance(50)) Call(side, kKeSetAffinityThread, { Load32(side, Out(2)), random.Chance(80) ? 1u << random.Below(6) : uint32_t(random.Next()) });
                    Call(side, kKeResumeThread, { Load32(side, Out(2)) });
                }
                WaitForThread(side, thread);
            }
            break;
        }
        case 29:
        {
            // A thread made suspended earlier, started now.
            for (Known& one : side.known)
                if (one.kind == Known::kThread && one.record && Load32(side, one.record + 44) == 0 && random.Chance(50))
                {
                    if (random.Chance(30)) Call(side, kNtSuspendThread, { one.handle, Out(1) });
                    Call(side, kNtResumeThread, { one.handle, 0 });
                    WaitForThread(side, one);
                    break;
                }
            break;
        }
        case 30:
        {
            // A thread made running, waiting on an event the script sets after.
            if (side.records >= kRecordCount) break;
            const uint32_t record = kRecords + side.records++ * kRecordSize;
            std::memset(side.base + record, 0, kRecordSize);
            if (Call(side, kNtCreateEvent, { Out(0), 0, random.Below(2), 0 }) != kSuccess) break;
            const uint32_t event = Load32(side, Out(0));
            Store32(side, record, event);
            if (Call(side, kExCreateThread, { Out(1), 0, 0, 0, kWaiter, record, random.Chance(50) ? 0x02000000u : 0u }) != kSuccess) break;
            const uint32_t thread = Load32(side, Out(1));
            if (random.Chance(50)) std::this_thread::sleep_for(std::chrono::microseconds(random.Below(300)));
            if (random.Chance(50)) Call(side, kNtSetEvent, { event, 0 });
            else
            {
                Call(side, kObReferenceObjectByHandle, { event, 0, Out(2) });
                Call(side, kKeSetEvent, { Load32(side, Out(2)), 1, 0 });
            }
            Call(side, kNtWaitForSingleObjectEx, { thread, 1, 0, 0 });
            Note(side, "waiter got %08X done %08X", Load32(side, record + 4), Load32(side, record + 8));
            Call(side, kNtClose, { thread });
            Call(side, kNtClose, { event });
            break;
        }
        case 31:
        {
            // APCs, and the alertable wait that delivers them.
            const uint32_t count = random.Below(5);
            for (uint32_t i = 0; i < count; i++)
            {
                const uint32_t routine = random.Chance(70) ? kApcRoutine : (random.Chance(50) ? kApcRequeue : (random.Chance(50) ? 0 : kNoFunction));
                const bool work = random.Chance(60);
                side.queue_user_apc(routine, uint32_t(random.Next()), uint32_t(random.Next()), RunWork, DiscardWork,
                                    work ? new Work{ &side, random.Below(1000) } : nullptr);
            }
            if (random.Chance(70))
            {
                const uint32_t handle = PickHandle(side, random, true);
                if (handle) Call(side, kNtWaitForSingleObjectEx, { handle, 1, random.Below(2), ShortTimeout(side, random, 0) });
            }
            else
            {
                PPCContext caller;
                std::memset(&caller, 0, sizeof caller);
                caller.r1.u64 = kStack;
                caller.r13.u64 = me;
                mw2_guest_base = side.base;
                Note(side, "delivered %d", side.deliver_user_apcs(&caller));
            }
            Note(side, "apc log %u", Load32(side, kApcLog));
            break;
        }
        case 32: case 33:
            NetXamStep(side, random);
            break;
        case 34: case 35:
        {
            // Memory, as the title asks for it, written to as the title would.
            switch (random.Below(6))
            {
            case 0:
            {
                Store32(side, Out(3), random.Chance(80) ? 0 : 0x90000000);
                Store32(side, Out(4), random.Chance(90) ? random.Below(0x40000) : uint32_t(random.Next()));
                const uint32_t type = random.Chance(80) ? 0x3000 : uint32_t(random.Below(4)) << 12;
                if (Call(side, kNtAllocateVirtualMemory, { random.Chance(97) ? Out(3) : 0, random.Chance(97) ? Out(4) : 0, type, 4 }) == kSuccess)
                    side.allocations.push_back(Load32(side, Out(3)));
                Note(side, "base %08X size %08X", Load32(side, Out(3)), Load32(side, Out(4)));
                break;
            }
            case 1:
            {
                const uint32_t address = !side.allocations.empty() && random.Chance(80) ? side.allocations[random.Below(uint32_t(side.allocations.size()))]
                                                                                         : uint32_t(random.Next());
                Store32(side, Out(3), address + (random.Chance(90) ? 0 : 0x1000));
                Call(side, kNtFreeVirtualMemory, { random.Chance(97) ? Out(3) : 0, Out(4), random.Chance(80) ? 0x8000u : 0x4000u, 0 });
                break;
            }
            case 2:
            {
                static const uint32_t kAlignments[] = { 0, 0x1000, 0x10000, 0x20000, 0x100000, 0x3000 };
                const uint32_t address = uint32_t(Call(side, kMmAllocatePhysicalMemoryEx,
                    { 0, random.Chance(95) ? random.Below(0x30000) : uint32_t(random.Next()), 4, 0, 0xFFFFFFFF, kAlignments[random.Below(6)] }));
                if (address) side.physical.push_back(address);
                break;
            }
            case 3:
            {
                const uint32_t address = !side.physical.empty() && random.Chance(80) ? side.physical[random.Below(uint32_t(side.physical.size()))]
                                                                                       : uint32_t(random.Next());
                Call(side, kMmFreePhysicalMemory, { 0, address });
                break;
            }
            case 4:
            {
                const uint32_t size = random.Chance(95) ? random.Below(0x20000) : uint32_t(random.Next());
                mw2_guest_base = side.base;
                const uint32_t address = side.allocate_guest(size, 1u << random.Below(17));
                Note(side, "allocate guest %08X: %08X", size, address);
                if (address) side.allocations.push_back(address);
                break;
            }
            default:
            {
                const uint32_t address = !side.allocations.empty() && random.Chance(80) ? side.allocations[random.Below(uint32_t(side.allocations.size()))]
                                                                                         : uint32_t(random.Next());
                mw2_guest_base = side.base;
                Note(side, "free guest %08X: %d", address, side.free_guest(address));
                Note(side, "free physical %08X: %d", address, side.free_physical(address));
                break;
            }
            }
            // What the title leaves in memory it was given, for a reuse to zero.
            for (uint32_t i = 0; i < 4 && !side.allocations.empty(); i++)
            {
                const uint32_t at = side.allocations[random.Below(uint32_t(side.allocations.size()))];
                if (at >= 0x84600000 && at < 0xA0000000)
                    for (uint32_t j = 0; j < 64; j++) side.base[at + random.Below(0x1000)] = uint8_t(random.Next());
            }
            break;
        }
        case 36:
            Call(side, kNtQueryVirtualMemory, { 0x84600000, random.Chance(90) ? Out(5) : 0, 0, 28 });
            Call(side, kMmQueryStatistics, { random.Chance(90) ? Out(5) : 0 });
            Call(side, kMmQueryAddressProtect, { 0x84600000 });
            Call(side, kMmSetAddressProtect, { 0x84600000, 0x1000, 4 });
            break;
        case 37:
            Call(side, kKeSetAffinityThread, { kHeaders + random.Below(kHeaderCount) * 32, random.Below(3) });
            Call(side, kKeResumeThread, { kHeaders + random.Below(kHeaderCount) * 32 });
            Call(side, kObDereferenceObject, { PickHandle(side, random, true) });
            Call(side, kObCreateSymbolicLink, { 0, 0 });
            Call(side, kObDeleteSymbolicLink, { 0 });
            Call(side, kKeSetBasePriorityThread, { 0, random.Below(10) });
            Call(side, kExRegisterTitleTerminateNotification, { 0, 1 });
            Call(side, kExTerminateThread, { random.Below(5) });
            break;
        case 38:
        {
            static const char* const kNames[] = { "KeBugCheck", "RtlUnwind", "NtQueryDirectoryFile", "RtlRaiseException", "XamTwo", "Xam3" };
            const uint32_t name = random.Below(6);
            // Distinct counts, so that the report's order does not rest on a tie.
            for (uint32_t i = 0; i <= name * 7; i++) side.unimplemented(kNames[name]);
            break;
        }
        default:
        {
            // A wait that ends at once on an object the script knows is signalled.
            for (const Known& one : side.known)
                if (one.kind == Known::kListener || one.kind == Known::kSession || one.kind == Known::kEnumerator ||
                    (one.kind == Known::kThread && one.record && Load32(side, one.record + 44)))
                {
                    Call(side, kNtWaitForSingleObjectEx, { one.handle, 1, 0, Forever(side, random, 3) });
                    break;
                }
            break;
        }
        }
    }

    // ---- comparing ---------------------------------------------------------------

    template <typename Body> std::string Logged(Body body)
    {
        std::fflush(stderr);
        const int kept = dup(2);
        FILE* file = std::tmpfile();
        dup2(fileno(file), 2);
        body();
        std::fflush(stderr);
        dup2(kept, 2);
        close(kept);
        std::string text(size_t(lseek(fileno(file), 0, SEEK_END)), '\0');
        lseek(fileno(file), 0, SEEK_SET);
        if (read(fileno(file), text.data(), text.size()) < 0) text.clear();
        std::fclose(file);
        return text;
    }

    [[noreturn]] void Different(const std::string& what)
    {
        std::printf("DIFFERENT: %s\n", what.c_str());
        std::fflush(stdout);
        std::_Exit(1);
    }

    // What a log holds, with its unprintable bytes spelt out.
    std::string Shown(const std::string& text)
    {
        std::string out;
        for (char c : text)
        {
            if (c == '\n' || (uint8_t(c) >= 0x20 && uint8_t(c) < 0x7F)) { out += c; continue; }
            char escaped[8];
            std::snprintf(escaped, sizeof escaped, "\\x%02X", uint8_t(c));
            out += escaped;
        }
        return out;
    }

    void SameText(const char* what, const std::string& a, const std::string& b)
    {
        if (a == b) return;
        size_t i = 0;
        while (i < a.size() && i < b.size() && a[i] == b[i]) i++;
        const size_t from = i > 300 ? i - 300 : 0;
        Different(std::string(what) + " (" + std::to_string(a.size()) + " and " + std::to_string(b.size()) + " bytes, apart at " +
                  std::to_string(i) + ")\n--- C++:\n" + Shown(a.substr(from, 600)) + "\n--- Rust:\n" + Shown(b.substr(from, 600)));
    }

    std::string g_lastCase;   // what the case just compared did, for a difference found after

    void SameTranscripts(uint64_t at)
    {
        auto& a = g_sides[0].transcript;
        auto& b = g_sides[1].transcript;
        g_lastCase.clear();
        for (size_t i = a.size() > 30 ? a.size() - 30 : 0; i < a.size(); i++) g_lastCase += "  " + a[i] + "\n";
        for (size_t i = 0; i < std::max(a.size(), b.size()); i++)
        {
            const std::string& x = i < a.size() ? a[i] : std::string("(nothing)");
            const std::string& y = i < b.size() ? b[i] : std::string("(nothing)");
            if (x == y) continue;
            std::string context;
            for (size_t j = i > 12 ? i - 12 : 0; j < i; j++) context += "  " + a[j] + "\n";
            Different("case " + std::to_string(at) + ", step " + std::to_string(i) + "\n" + context + "C++:  " + x + "\nRust: " + y);
        }
        a.clear();
        b.clear();
    }

    void SameMemory(const char* what, uint32_t from, uint32_t to)
    {
        if (to <= from) return;
        const uint8_t* a = g_sides[0].base + from;
        const uint8_t* b = g_sides[1].base + from;
        if (std::memcmp(a, b, to - from) == 0) return;
        uint32_t i = 0;
        while (a[i] == b[i]) i++;
        char line[200];
        std::snprintf(line, sizeof line, "%s memory at %08X: C++ %02X, Rust %02X", what, from + i, a[i], b[i]);
        Different(std::string(line) + "\nafter:\n" + g_lastCase);
    }

    // The image a side starts from: a PE header whose TLS directory names a template,
    // and the two import records the kernel resolves.
    void MakeImage(Side& side, Random random, uint64_t seed)
    {
        uint8_t* image = side.base + PPC_IMAGE_BASE;
        auto little = [&](uint32_t at, uint32_t value) { std::memcpy(image + at, &value, 4); };
        const uint32_t pe = 0x80 + random.Below(4) * 8;
        little(0x3C, pe);
        const uint16_t magic = random.Chance(80) ? 0x10B : 0x20B;
        std::memcpy(image + pe + 24, &magic, 2);
        const uint32_t directories = pe + 24 + (magic == 0x10B ? 96 : 112);
        const uint32_t tls = 0x1000;
        little(directories + 72, seed % 4 == 1 || random.Chance(90) ? tls : 0);
        little(directories + 76, seed % 4 == 1 || random.Chance(90) ? 24 : random.Below(30));
        const uint32_t template_ = PPC_IMAGE_BASE + 0x2000;
        const uint32_t size = seed % 4 == 1 ? 0x300 : random.Chance(80) ? 8 + random.Below(0x100) : random.Below(0x400);
        Store32(side, PPC_IMAGE_BASE + tls, template_);
        Store32(side, PPC_IMAGE_BASE + tls + 4, template_ + size);
        Store32(side, PPC_IMAGE_BASE + tls + 16, random.Below(0x80));
        for (uint32_t i = 0; i < 0x400; i++) image[0x2000 + i] = uint8_t(random.Next());
        Store32(side, T_DATA_TimeStampBundlePtr, seed % 4 == 3 ? 0x82001000 : 0x00010011);
        Store32(side, T_DATA_DebugMonitorPtr, seed % 4 == 2 ? 0x00030058 : 0x00030059);
    }

    void Prepare(Side& side, uint64_t seed)
    {
        side.base = static_cast<uint8_t*>(mmap(nullptr, kSpace, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0));
        if (side.base == MAP_FAILED) { std::perror("mmap"); std::exit(2); }
        PlaceFunction(side, kThreadRoutine, ThreadRoutine);
        PlaceFunction(side, kShim, Shim);
        PlaceFunction(side, kWaiter, Waiter);
        PlaceFunction(side, kApcRoutine, ApcRoutine);
        PlaceFunction(side, kApcRequeue, ApcRequeue);
        PlaceFunction(side, kTaker, Taker);
        MakeImage(side, Random{ seed * 0x777 }, seed);
        side.randomBytes = seed << 32;
    }

    void Start(Side& side)
    {
        mw2_guest_base = side.base;
        if (!side.folder.empty()) std::filesystem::current_path(side.folder);
        kernel::GameRootKept() = "game";
        side.initialise("game");
        side.mainBlock = side.create_thread_block(1);
        for (uint32_t& other : side.otherBlocks) other = side.create_thread_block(side.new_thread_id());
        Note(side, "main block %08X others %08X %08X %08X", side.mainBlock, side.otherBlocks[0], side.otherBlocks[1], side.otherBlocks[2]);
        Note(side, "guest function %d %d %d", side.guest_function(kThreadRoutine) == ThreadRoutine,
             side.guest_function(kNoFunction) == nullptr, side.guest_function(0x80000000) == nullptr);
        // The save device each controller chose: some had, some had not.
        if (T_DATA_DeviceTable)
        {
            Random random{ g_seed };
            for (uint32_t i = 0; i < 4; i++) Store32(side, T_DATA_DeviceTable + i * 4, random.Chance(50) ? 0 : random.Below(3));
        }
        PPCContext ctx;
        std::memset(&ctx, 0, sizeof ctx);
        ctx.r1.u64 = kStack;
        (side.name[0] == 'C' ? MW2_CAT(cpp_sub_, T_Memcard_InitializeSystem) : MW2_CAT(sub_, T_Memcard_InitializeSystem))(ctx, side.base);
        if (T_DATA_DeviceTable)
            Note(side, "devices %08X %08X %08X %08X", Load32(side, T_DATA_DeviceTable), Load32(side, T_DATA_DeviceTable + 4),
                 Load32(side, T_DATA_DeviceTable + 8), Load32(side, T_DATA_DeviceTable + 12));
    }

    std::string LaunchData()
    {
        std::string data;
        mw2_install_launch_data(&data, [](void* to, const char* text, size_t size) { static_cast<std::string*>(to)->assign(text, size); });
        return data;
    }

    // Someone signed in at the second controller, for the profile of a player there: both
    // sides ask the one sign-in screen, the Rust's.
    void SignInSecond(const std::filesystem::path& folder)
    {
        std::filesystem::create_directories(folder / "saves");
        std::filesystem::current_path(folder);
        for (const uint16_t buttons : { 0x0400, 0, 0x1000, 0 }) mw2_signin_input(1, buttons);
        bool signedIn = false;
        uint64_t id = 0;
        mw2_signin_at(1, &signedIn, &id, nullptr, [](void*, const char*, size_t) {});
        std::printf("the second controller: %s\n", signedIn ? "signed in" : "nobody");
    }

    void BindSides()
    {
        Side& cpp = g_sides[0];
        Side& rust = g_sides[1];
        cpp.name = "C++";
        rust.name = "Rust";
#define X(name) cpp.imports[k##name] = cpp___imp__##name; rust.imports[k##name] = __imp__##name;
        KERNEL_IMPORTS(X)
#undef X
#define X(type, name, arguments) cpp.name = cpp_mw2_kernel_##name; rust.name = mw2_kernel_##name;
        KERNEL_CALLS(X)
#undef X
        cpp.physicalTop = &cpp_mw2_kernel_physical_top;
        rust.physicalTop = &mw2_kernel_physical_top;
    }

    // The bundle's clocks are the host's, and are not compared.
    void MaskBundle()
    {
        for (Side& side : g_sides)
        {
            const uint32_t bundle = Load32(side, T_DATA_TimeStampBundlePtr);
            if (bundle >= 0x84600000 && bundle < 0xA0000000)
            {
                Store64(side, bundle, 0);
                Store32(side, bundle + 16, 0);
            }
        }
    }

    uint32_t HighWater(const std::string& log)
    {
        const size_t at = log.find("high water of ");
        return at == std::string::npos ? 0 : uint32_t(std::strtoul(log.c_str() + at + 14, nullptr, 16));
    }

    void Script(uint64_t seed, uint64_t cases)
    {
        g_seed = seed;
        const auto base = std::filesystem::temp_directory_path() / ("kernel_diff_" + std::to_string(getpid()));
        std::filesystem::remove_all(base);
        // Some seeds have the file trace on, a script standing in for controllers, or the
        // save device left to the player.
        if (seed % 2 == 0) setenv("MW2_TRACE_FILES", "1", 1);
        if (seed % 3 == 0) setenv("MW2_TRACE_ONLINE", "1", 1);
        if (seed % 3 == 1) setenv("MW2_INPUT_SCRIPT", "0:a:100000,0:lt:100000,0:ry-:100000,0:b@3:100000,0:lx+@2:100000,9999:x,junk,1:nothing", 1);
        // A script with nothing held yet at the first controller.
        if (seed % 3 == 2) setenv("MW2_INPUT_SCRIPT", "9999:x,9999:y@2", 1);
        // A 4:3 picture, which is not widescreen.
        if (seed % 4 == 3) setenv("MW2_RENDER", "960x720", 1);
        if (seed % 5 == 4) setenv("MW2_NO_AUTO_SAVE_DEVICE", "1", 1);
        // No pad of this machine's: the controllers are the script's.
        setenv("SDL_GAMECONTROLLER_IGNORE_DEVICES_EXCEPT", "0x0000/0x0000", 1);
        if (seed % 2 == 1) SignInSecond(base / "signin");
        // No service, one like lan, or one with Live's lobbies.
        g_service = int(seed / 2 % 3);
        // What both sides share says what it says once, before either: the first
        // controller's profile among them, made in a folder of the rig's.
        std::filesystem::create_directories(base / "signin" / "saves");
        std::filesystem::current_path(base / "signin");
        Logged([] {
            uint64_t id;
            mw2_signin_first(&id, nullptr, [](void*, const char*, size_t) {});
            rig_online_choose(g_service != 0);
            uint32_t size[2];
            mw2_render_frame(size);
        });
        for (int i = 0; i < 2; i++)
        {
            g_sides[i].folder = base / (i ? "rust" : "cpp");
            MakeGame(g_sides[i].folder, seed);
        }
        for (Side& side : g_sides) Prepare(side, seed);
        std::string logs[2];
        for (int i = 0; i < 2; i++) logs[i] = Logged([&] { Start(g_sides[i]); });
        SameText("starting", logs[0], logs[1]);
        SameTranscripts(0);
        // Presence reads the image's XLAST at the title's first context, once for both.
        Logged([] {
            mw2_guest_base = g_sides[0].base;
            mw2_presence_set_context(3, 0xFFFFFFFF, 0);
        });
        // Mixed, so that one seed's draws are not the last's a step later.
        Random random{ Random{ seed }.Next() };
        for (uint64_t c = 1; c <= cases; c++)
        {
            const uint32_t steps = 1 + random.Below(40);
            const Random from = random;
            const std::string launch = LaunchData();
            for (int i = 0; i < 2; i++)
            {
                Random mine = from;
                g_running = &g_sides[i];
                std::filesystem::current_path(g_sides[i].folder);
                mw2_install_set_launch_data(reinterpret_cast<const uint8_t*>(launch.data()), launch.size());
                logs[i] = Logged([&] {
                    // What the rig's service keeps is the side's whose guest space is current.
                    mw2_guest_base = g_sides[i].base;
                    for (uint32_t s = 0; s < steps; s++) Step(g_sides[i], mine);
                    // The host's ports, for the other side.
                    if (!g_service)
                        for (const Side::Socket& socket : g_sides[i].sockets) Call(g_sides[i], kNetDll_closesocket, { 0, socket.handle });
                    if (!g_service) g_sides[i].sockets.clear();
                });
                if (i == 1) random = mine;
            }
            SameText(("case " + std::to_string(c) + "'s log").c_str(), logs[0], logs[1]);
            SameTranscripts(c);
            g_lastCase += "and logged:\n" + Shown(logs[0].substr(logs[0].size() > 1500 ? logs[0].size() - 1500 : 0));
            SameText(("case " + std::to_string(c) + "'s files").c_str(), Tree(g_sides[0].folder), Tree(g_sides[1].folder));
            MaskBundle();
            SameMemory("the arena's", kArena, kArena + kArenaSize);
        }
        for (int i = 0; i < 2; i++)
            logs[i] = Logged([&] {
                Side& side = g_sides[i];
                // A guest thread asleep on an event nobody has set, for the watchdog's report.
                const uint32_t record = kRecords + (kRecordCount - 1) * kRecordSize;
                Call(side, kNtCreateEvent, { Out(0), 0, 0, 0 });
                const uint32_t event = Load32(side, Out(0));
                Store32(side, record, event);
                Store32(side, record + 8, 0);
                Call(side, kExCreateThread, { Out(1), 0, 0, 0, kWaiter, record, 1 });
                const uint32_t thread = Load32(side, Out(1));
                Call(side, kNtResumeThread, { thread, 0 });
                std::this_thread::sleep_for(std::chrono::milliseconds(100));
                side.report_waits();
                Call(side, kNtSetEvent, { event, 0 });
                Call(side, kNtWaitForSingleObjectEx, { thread, 1, 0, 0 });
                mw2_guest_base = side.base;
                side.report_memory();
                g_sides[i].report_unimplemented();
                g_sides[i].report_waits();
                g_sides[i].report_missing_files();
            });
        SameText("the reports", logs[0], logs[1]);
        MaskBundle();
        SameMemory("the virtual heap's", 0x84600000, std::max(HighWater(logs[0]), HighWater(logs[1])));
        SameMemory("the physical heap's", 0xA0000000, std::max(*g_sides[0].physicalTop, *g_sides[1].physicalTop));
        SameMemory("the image's", uint32_t(PPC_IMAGE_BASE), uint32_t(PPC_IMAGE_BASE) + 0x4000);
        if (g_sides[0].discarded != g_sides[1].discarded) Different("discarded APC work");
        size_t files = 0;
        for (const Known& one : g_sides[0].known) files += one.kind == Known::kFile;
        const std::string tree = Tree(g_sides[0].folder);
        std::printf("same: seed %llu, %llu cases, service %d, %zu objects known (%zu files), %u searches, heap to %08X, physical to %08X, "
                    "%zu things on the disk\n",
                    (unsigned long long)seed, (unsigned long long)cases, g_service, g_sides[0].known.size(), files, g_sides[0].searches,
                    HighWater(logs[0]), *g_sides[0].physicalTop, size_t(std::count(tree.begin(), tree.end(), '\n')));
        std::filesystem::current_path("/");
        std::filesystem::remove_all(base);
    }
}

// ---- many threads ---------------------------------------------------------------

namespace
{
    using Clock = std::chrono::steady_clock;

    // A watchdog for a check: a lost wake leaves a thread asleep for good.
    struct Deadline
    {
        std::atomic<bool> done{ false };
        std::thread watcher;
        const char* name;
        Clock::time_point began = Clock::now();
        explicit Deadline(const char* what) : name(what)
        {
            watcher = std::thread([this, what] {
                const auto until = Clock::now() + std::chrono::seconds(60);
                while (!done.load() && Clock::now() < until) std::this_thread::sleep_for(std::chrono::milliseconds(20));
                if (!done.load()) { std::printf("HUNG: %s\n", what); std::fflush(stdout); std::_Exit(3); }
            });
        }
        ~Deadline()
        {
            done = true;
            watcher.join();
            std::printf("  %-40s %.3f s\n", name, std::chrono::duration<double>(Clock::now() - began).count());
        }
    };

    struct Context
    {
        PPCContext ctx;
        Context(Side& side, uint32_t r13)
        {
            std::memset(&ctx, 0, sizeof ctx);
            ctx.r1.u64 = kStack;
            ctx.r13.u64 = r13;
        }
        uint32_t operator()(Side& side, int import, std::initializer_list<uint64_t> arguments)
        {
            PPCRegister* registers[] = { &ctx.r3, &ctx.r4, &ctx.r5, &ctx.r6, &ctx.r7, &ctx.r8, &ctx.r9, &ctx.r10 };
            size_t i = 0;
            for (uint64_t argument : arguments) registers[i++]->u64 = argument;
            side.imports[import](ctx, side.base);
            return ctx.r3.u32;
        }
    };

    template <typename Body> void Threads(int count, Body body)
    {
        std::vector<std::thread> threads;
        for (int i = 0; i < count; i++) threads.emplace_back(body, i);
        for (auto& thread : threads) thread.join();
    }

    void Fail(Side& side, const char* what, long long got, long long wanted)
    {
        std::printf("WRONG on %s: %s: %lld, not %lld\n", side.name, what, got, wanted);
        std::fflush(stdout);
        std::_Exit(4);
    }

    void Stress(Side& side, int rounds)
    {
        mw2_guest_base = side.base;
        std::vector<uint32_t> blocks;
        for (int i = 0; i < 8; i++) blocks.push_back(side.create_thread_block(side.new_thread_id()));
        const auto began = Clock::now();

        {
            // A semaphore: four producers, four consumers, no count lost or taken twice.
            Deadline deadline("semaphore producers and consumers");
            const uint32_t header = kHeaders;
            Context(side, blocks[0])(side, kKeInitializeSemaphore, { header, 0, 0x7FFFFFFF });
            Threads(8, [&](int i) {
                Context call(side, blocks[i]);
                for (int n = 0; n < rounds; n++)
                    if (i < 4) call(side, kKeReleaseSemaphore, { header, 1, 1, 0 });
                    else if (call(side, kKeWaitForSingleObject, { header, 0, 1, 0, 0 }) != 0) Fail(side, "a semaphore wait", call.ctx.r3.u32, 0);
            });
            Store64(side, kTimeouts, 0);
            const uint32_t left = Context(side, blocks[0])(side, kKeWaitForSingleObject, { header, 0, 1, 0, kTimeouts });
            if (left != kTimeout) Fail(side, "a semaphore after as many takes as gives", left, kTimeout);
        }
        for (const uint32_t spin : { 0u, 1000u })
        {
            // A critical section around a count no atomic guards, held long enough for
            // the others to find it held and sleep, with and without spinning first.
            Deadline deadline("critical section");
            const uint32_t cs = kSections, counter = kOut;
            Context(side, blocks[0])(side, kRtlInitializeCriticalSectionAndSpinCount, { cs, spin });
            Store32(side, counter, 0);
            std::atomic<int> ready{ 0 };
            Threads(6, [&](int i) {
                Context call(side, blocks[i]);
                for (ready++; ready < 6;) {}
                for (int n = 0; n < rounds; n++)
                {
                    if (n % 7 == 0) { if (!call(side, kRtlTryEnterCriticalSection, { cs })) continue; }
                    else call(side, kRtlEnterCriticalSection, { cs });
                    if (n % 5 == 0) call(side, kRtlEnterCriticalSection, { cs });
                    const uint32_t was = Load32(side, counter);
                    for (volatile int k = 0; k < 50; k++) {}
                    Store32(side, counter, was + 1);
                    if (n % 5 == 0) call(side, kRtlLeaveCriticalSection, { cs });
                    call(side, kRtlLeaveCriticalSection, { cs });
                }
            });
            const uint32_t count = Load32(side, counter), tries = uint32_t(rounds + 6) / 7;
            if (count > uint32_t(6 * rounds) || count < 6 * (uint32_t(rounds) - tries)) Fail(side, "a count under a critical section", count, 6 * rounds);
            if (Load32(side, cs + 16) != 0xFFFFFFFF || Load32(side, cs + 24) != 0) Fail(side, "a critical section left", Load32(side, cs + 16), -1);
        }
        {
            // Two threads passing the turn by two synchronization events.
            Deadline deadline("event ping-pong");
            Context make(side, blocks[0]);
            make(side, kNtCreateEvent, { Out(0), 0, 1, 0 });
            make(side, kNtCreateEvent, { Out(1), 0, 1, 0 });
            const uint32_t ping = Load32(side, Out(0)), pong = Load32(side, Out(1));
            std::atomic<int> turns{ 0 };
            Threads(2, [&](int i) {
                Context call(side, blocks[i]);
                for (int n = 0; n < rounds; n++)
                {
                    if (i == 0) call(side, kNtSetEvent, { ping, 0 });
                    call(side, kNtWaitForSingleObjectEx, { i == 0 ? pong : ping, 1, 0, 0 });
                    if (i == 1) call(side, kNtSetEvent, { pong, 0 });
                    turns++;
                }
            });
            if (turns != 2 * rounds) Fail(side, "turns", turns, 2 * rounds);
            make(side, kNtClose, { ping });
            make(side, kNtClose, { pong });
        }
        {
            // A wait for all of two semaphores, given one at a time from two threads and
            // taken by two others, half each: nothing left, and nobody left waiting.
            Deadline deadline("wait for all");
            Context make(side, blocks[0]);
            const uint32_t a = kHeaders + 64, b = kHeaders + 96;
            make(side, kKeInitializeSemaphore, { a, 0, 0x7FFFFFFF });
            make(side, kKeInitializeSemaphore, { b, 0, 0x7FFFFFFF });
            Store32(side, kLists, a);
            Store32(side, kLists + 4, b);
            Threads(4, [&](int i) {
                Context call(side, blocks[i]);
                for (int n = 0; n < rounds; n++)
                    if (i == 0) call(side, kKeReleaseSemaphore, { a, 1, 1, 0 });
                    else if (i == 1) call(side, kKeReleaseSemaphore, { b, 1, 1, 0 });
                    else if (n % 2 == i - 2 && call(side, kKeWaitForMultipleObjects, { 2, kLists, 0, 0, 1, 0, 0, 0 }) != 0)
                        Fail(side, "a wait for all", call.ctx.r3.u32, 0);
            });
            Store64(side, kTimeouts, 0);
            const uint32_t left = make(side, kKeWaitForMultipleObjects, { 2, kLists, 1, 0, 1, 0, kTimeouts, 0 });
            if (left != kTimeout) Fail(side, "two semaphores after as many takes as gives", left, kTimeout);
        }
        {
            // A mutant taken twice by its owner and released as often, by six threads.
            Deadline deadline("mutant");
            Context make(side, blocks[0]);
            make(side, kNtCreateMutant, { Out(2), 0, 0 });
            const uint32_t mutant = Load32(side, Out(2)), counter = kOut + 64;
            Store32(side, counter, 0);
            Threads(6, [&](int i) {
                Context call(side, blocks[i]);
                for (int n = 0; n < rounds; n++)
                {
                    call(side, kNtWaitForSingleObjectEx, { mutant, 1, 0, 0 });
                    call(side, kNtWaitForSingleObjectEx, { mutant, 1, 0, 0 });
                    Store32(side, counter, Load32(side, counter) + 1);
                    call(side, kNtReleaseMutant, { mutant, 0 });
                    call(side, kNtReleaseMutant, { mutant, 0 });
                }
            });
            if (Load32(side, counter) != uint32_t(6 * rounds)) Fail(side, "a count under a mutant", Load32(side, counter), 6 * rounds);
            make(side, kNtClose, { mutant });
        }
        {
            // A spin lock around a count.
            Deadline deadline("spin lock");
            const uint32_t lock = kSpinLocks, counter = kOut + 128;
            Store32(side, lock, 0);
            Store32(side, counter, 0);
            Threads(4, [&](int i) {
                Context call(side, blocks[i]);
                for (int n = 0; n < rounds; n++)
                {
                    call(side, kKfAcquireSpinLock, { lock });
                    Store32(side, counter, Load32(side, counter) + 1);
                    call(side, kKfReleaseSpinLock, { lock, 0 });
                }
            });
            if (Load32(side, counter) != uint32_t(4 * rounds)) Fail(side, "a count under a spin lock", Load32(side, counter), 4 * rounds);
        }
        std::printf("held: %s, %d rounds in %.2f s\n", side.name, rounds,
                    std::chrono::duration<double>(Clock::now() - began).count());
    }

    // What a call costs, and a wake: nanoseconds, the median of a few tries.
    void Time(Side& side, int calls)
    {
        mw2_guest_base = side.base;
        const uint32_t a = side.create_thread_block(side.new_thread_id()), b = side.create_thread_block(side.new_thread_id());
        Context call(side, a);
        call(side, kNtCreateEvent, { Out(0), 0, 1, 0 });
        call(side, kNtCreateEvent, { Out(1), 0, 1, 0 });
        const uint32_t ping = Load32(side, Out(0)), pong = Load32(side, Out(1));
        const uint32_t cs = kSections + 64;
        call(side, kRtlInitializeCriticalSection, { cs });
        auto measure = [&](const char* what, auto body, int n) {
            double best[5];
            for (double& one : best)
            {
                const auto from = Clock::now();
                for (int i = 0; i < n; i++) body();
                one = std::chrono::duration<double, std::nano>(Clock::now() - from).count() / n;
            }
            std::sort(best, best + 5);
            std::printf("time: %-4s %-46s %8.1f ns\n", side.name, what, best[2]);
        };
        measure("NtSetEvent + NtClearEvent", [&] { call(side, kNtSetEvent, { ping, 0 }); call(side, kNtClearEvent, { ping }); }, calls);
        measure("NtSetEvent + NtWaitForSingleObjectEx (no sleep)", [&] { call(side, kNtSetEvent, { ping, 0 }); call(side, kNtWaitForSingleObjectEx, { ping, 1, 0, 0 }); }, calls);
        measure("RtlEnter + RtlLeaveCriticalSection", [&] { call(side, kRtlEnterCriticalSection, { cs }); call(side, kRtlLeaveCriticalSection, { cs }); }, calls);
        measure("KeTlsGetValue", [&] { call(side, kKeTlsGetValue, { 0 }); }, calls);
        // A round trip: set, and sleep until the other thread sets back.
        std::atomic<bool> stop{ false };
        std::thread other([&] {
            Context mine(side, b);
            for (;;)
            {
                mine(side, kNtWaitForSingleObjectEx, { ping, 1, 0, 0 });
                if (stop) break;
                mine(side, kNtSetEvent, { pong, 0 });
            }
        });
        measure("a wake there and back (set, sleep, woken)", [&] {
            call(side, kNtSetEvent, { ping, 0 });
            call(side, kNtWaitForSingleObjectEx, { pong, 1, 0, 0 });
        }, calls / 50);
        stop = true;
        call(side, kNtSetEvent, { ping, 0 });
        other.join();
    }
}

extern "C" int __llvm_profile_write_file() __attribute__((weak));

int main(int argc, char** argv)
{
    BindSides();
    const std::string mode = argc > 1 ? argv[1] : "script";
    if (mode == "stress" || mode == "time")
    {
        const int n = argc > 2 ? std::atoi(argv[2]) : 20000;
        for (Side& side : g_sides)
        {
            Prepare(side, 1);
            Logged([&] { Start(side); });
            side.transcript.clear();
        }
        for (Side& side : g_sides)
            if (mode == "stress") Stress(side, n);
            else Time(side, n);
    }
    else Script(std::strtoull(argv[2], nullptr, 10), std::strtoull(argv[3], nullptr, 10));
    // Both tables still hold objects whose sides are statics too: no destructors, and
    // so the coverage profile is written here.
    if (__llvm_profile_write_file) __llvm_profile_write_file();
    std::fflush(stdout);
    std::_Exit(0);
}
