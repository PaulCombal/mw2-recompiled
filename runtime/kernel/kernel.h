#pragma once
#include <ppc_recomp_shared.h>
#include "physical.h"
#include "../guest.h"
#include <cstdint>
#include <string>
#include <filesystem>
#include <functional>

// The kernel is src/runtime/kernel/ in the Rust build, all but audio, and these are what it
// gives the rest of the C++ (kernel.cpp, threads.cpp, sync_objects.cpp, files.cpp and
// misc.cpp in the C++ build). What follows the C declarations and is not inline is the C++
// build's alone: in the Rust build only kernel/audio.cpp is the C++'s.
extern "C"
{
    void mw2_kernel_report_waits();
    void mw2_kernel_initialise(const char* gameRoot);
    uint32_t mw2_kernel_allocate_guest(uint32_t size, uint32_t alignment);
    uint32_t mw2_kernel_allocate_physical(uint32_t size, uint32_t alignment);
    bool mw2_kernel_free_guest(uint32_t address);
    bool mw2_kernel_free_physical(uint32_t address);
    void mw2_kernel_report_memory();
    uint32_t mw2_kernel_create_thread_block(uint32_t threadId);
    uint32_t mw2_kernel_new_thread_id();
    void mw2_kernel_update_time_stamp_bundle();
    PPCFunc* mw2_kernel_guest_function(uint32_t address);
    void mw2_kernel_queue_user_apc(uint32_t routine, uint32_t context, uint32_t ioStatusBlock,
                                   void (*run)(void* work), void (*discard)(void* work), void* work);
    bool mw2_kernel_deliver_user_apcs(const PPCContext* caller);
    void mw2_kernel_unimplemented(const char* name);
    void mw2_kernel_report_unimplemented();
    void mw2_kernel_report_missing_files();
}

namespace kernel
{
    // The threads blocked in a kernel wait, and on what: for the watchdog.
    inline void ReportWaits() { mw2_kernel_report_waits(); }

    // The game root is kept here as well, as files.cpp reads it as a path.
    inline std::filesystem::path& GameRootKept()
    {
        static std::filesystem::path root;
        return root;
    }
    inline void Initialise(const std::filesystem::path& gameRoot)
    {
        GameRootKept() = gameRoot;
        mw2_kernel_initialise(reinterpret_cast<const char*>(gameRoot.u8string().c_str()));
    }
    inline const std::filesystem::path& GameRoot() { return GameRootKept(); }

    inline uint32_t AllocateGuest(uint32_t size, uint32_t alignment = 16)
    {
        return mw2_kernel_allocate_guest(size, alignment);
    }
    inline uint32_t AllocatePhysical(uint32_t size, uint32_t alignment = 4096)
    {
        return mw2_kernel_allocate_physical(size, alignment);
    }
    // False when `address` is not the start of a live allocation.
    inline bool FreeGuest(uint32_t address) { return mw2_kernel_free_guest(address); }
    inline bool FreePhysical(uint32_t address) { return mw2_kernel_free_physical(address); }
    inline void ReportMemory() { mw2_kernel_report_memory(); }

    // Returns the value r13 must hold. The recompiled code reads the thread id
    // straight out of these structures (KPCR.CurrentThread -> KTHREAD.ThreadId),
    // so they have to be populated or every thread reports id 0.
    inline uint32_t CreateThreadBlock(uint32_t threadId) { return mw2_kernel_create_thread_block(threadId); }
    // A fresh id from the same sequence guest threads draw on, for a host
    // thread that runs guest code (the audio worker).
    inline uint32_t NewThreadId() { return mw2_kernel_new_thread_id(); }

    // 100ns ticks since 1601-01-01, the epoch the console reports: misc.cpp, the C++
    // build's alone.
    uint64_t SystemTime100ns();

    // The kernel publishes a small block of clock values the title reads directly
    // rather than through a call. It arrives as a data import, which nothing
    // resolves, so it is created at Initialise and kept ticking by this.
    inline void UpdateTimeStampBundle() { mw2_kernel_update_time_stamp_bundle(); }

    // The recompiled function at a guest address, or null when the address is
    // outside the image's code or has no function starting there.
    inline PPCFunc* GuestFunction(uint32_t address) { return mw2_kernel_guest_function(address); }

    // The nth argument (from 0) of a guest call, for the ones past the eight that
    // arrive in r3-r10: the caller's parameter save area starts at r1+0x14 with
    // eight-byte slots, so the ninth is at r1+0x54.
    inline uint32_t StackArgument(const PPCContext& ctx, uint32_t n)
    {
        return *reinterpret_cast<const be32*>(guest::Base() + ctx.r1.u32 + 0x14 + n * 8);
    }

    // User APCs. The title issues asynchronous reads and does its accounting in a
    // completion routine, which NT delivers only when the issuing thread enters an
    // alertable wait -- never inside the call that queued it.
    //
    // `work` runs just before the routine, at delivery time. Asynchronous reads
    // put the transfer itself there: the title double-buffers on the assumption
    // that a read it has issued is still in flight while it decompresses the other
    // buffer, so filling the buffer at issue time corrupts what it is reading.
    inline void QueueUserApc(uint32_t routine, uint32_t context, uint32_t ioStatusBlock,
                             std::function<void()> work = {})
    {
        using Work = std::function<void()>;
        auto* kept = work ? new Work(std::move(work)) : nullptr;
        mw2_kernel_queue_user_apc(
            routine, context, ioStatusBlock,
            [](void* w) { auto* one = static_cast<Work*>(w); (*one)(); delete one; },
            [](void* w) { delete static_cast<Work*>(w); }, kept);
    }
    inline bool DeliverUserApcs(const PPCContext& caller) { return mw2_kernel_deliver_user_apcs(&caller); }

    // XOVERLAPPED. Asynchronous XAM calls take one and the caller polls it, so a
    // call that completes inline still has to mark it finished and signal the
    // event the caller may be waiting on.
    void CompleteOverlapped(uint32_t address, uint32_t result);
    // The same, for a call that failed: the caller reads `result` from the
    // block, and why from its extended error.
    void CompleteOverlappedEx(uint32_t address, uint32_t result, uint32_t extendedError,
                              uint32_t length);

    // XNotify: queues a notification for every listener that asked for its area.
    void PostNotification(uint32_t id, uint32_t param);
    // An accepted Xbox LIVE invitation: XInviteGetAcceptedInfo reports it from
    // here on, and the title is told it arrived.
    void AcceptInvite(uint64_t inviterAccount, const uint8_t* session60, bool fromInvite);

    // Content packages -- saved games. The title mounts one as a lettered drive
    // ("save0:") and then opens "save0:\savegame.svg" through the ordinary file
    // API, so XAM and the file layer have to share one table of device roots.
    // A package here is a directory under SaveRoot(); nothing writes the console's
    // STFS container format.
    void MountDevice(const std::string& name, const std::filesystem::path& root);
    void UnmountDevice(const std::string& name);
    // Beside the runtime, not inside the game root: the game root is the disc. The same
    // as src/runtime/kernel/content.rs's.
    inline std::filesystem::path SaveRoot() { return "saves"; }
    // What is kept under a player's number -- his rank, offline and Live, and
    // the settings of a profile at the second to fourth controllers -- renamed
    // to another number, where that one has none yet (kernel/xam.cpp).
    void MovePlayerData(uint64_t from, uint64_t to);

    // The one storage device this runtime offers. The title takes the id from the
    // device selector, hands it back in every content call and stores it in the
    // package it creates, so the selector and the content layer have to agree.
    constexpr uint32_t kSaveDeviceId = 1;

    // The console's own network address, as XNetGetTitleXnAddr reports it: 36
    // bytes of XNADDR. XSessionCreate has to put the same one in the session it
    // hands out, so the two answers come from one place.
    void TitleXnAddr(void* out36);
    // The peer an address names; false for one that did not come from here.
    bool XnAddrPeer(const void* addr36, uint64_t& peer);
    // The 8-byte id of the session this machine hosts or joined, which
    // XNetInAddrToXnAddr names for a player the title did not look up itself.
    void NoteSessionId(const void* id8);

    // A search's answer, for XNetQosLookup: the round trip to the player
    // hosting a session, by its id, until the next search.
    void ForgetSessionLags();
    void NoteSessionLag(const void* id8, uint32_t milliseconds);
    // Puts a service's id for a peer into a 36-byte XNADDR.
    void SetXnAddrPeer(void* addr36, uint64_t peer);
    // Bytes from a random source, for XNetRandom and the sessions this machine
    // makes (kernel/kernel.cpp).
    void RandomBytes(void* out, size_t size);

    // Once a frame: hands the title the searches that finished.
    void FinishSearches();

    // Called by generated stubs; logs the first hit and counts the rest.
    inline void Unimplemented(const char* name) { mw2_kernel_unimplemented(name); }

    // End-of-run reports.
    inline void ReportUnimplemented() { mw2_kernel_report_unimplemented(); }
    inline void ReportMissingFiles() { mw2_kernel_report_missing_files(); }
}
