// Asset-free test of the XAM imports a title makes on its way to a menu
// (runtime/kernel/xam.cpp): who is signed in at each controller and how it is
// reported, the notification listeners, overlapped completion, the device
// selector, enumerators, XamAlloc and the config settings. The sign-in screen,
// the online service and the network layer are replaced by small fakes, so the
// test sees only what the kernel side does with them. No game files are involved.
#include "tests/fake_guest.h"
#include "runtime/friends.h"
#include "runtime/online/service.h"
#include "runtime/presence.h"
#include "runtime/signin.h"
#include "runtime/kernel/objects.h"

#ifdef NDEBUG
#undef NDEBUG
#endif
#include <cassert>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <filesystem>
#include <string>

PPC_FUNC(__imp__XamNotifyCreateListener);
PPC_FUNC(__imp__XNotifyGetNext);
PPC_FUNC(__imp__XamUserGetSigninState);
PPC_FUNC(__imp__XamUserGetXUID);
PPC_FUNC(__imp__XamUserGetName);
PPC_FUNC(__imp__XamUserGetSigninInfo);
PPC_FUNC(__imp__XamUserCheckPrivilege);
PPC_FUNC(__imp__XamUserAreUsersFriends);
PPC_FUNC(__imp__XamUserCreateStatsEnumerator);
PPC_FUNC(__imp__XamShowSigninUI);
PPC_FUNC(__imp__XamShowFriendsUI);
PPC_FUNC(__imp__XamShowDeviceSelectorUI);
PPC_FUNC(__imp__XamShowMessageBoxUIEx);
PPC_FUNC(__imp__XamShowDirtyDiscErrorUI);
PPC_FUNC(__imp__XamCreateEnumeratorHandle);
PPC_FUNC(__imp__XamGetPrivateEnumStructureFromHandle);
PPC_FUNC(__imp__XamContentCreateEnumerator);
PPC_FUNC(__imp__XamEnumerate);
PPC_FUNC(__imp__XamAlloc);
PPC_FUNC(__imp__XamFree);
PPC_FUNC(__imp__ExGetXConfigSetting);

// ---- the fakes xam.cpp is linked against --------------------------------------
namespace
{
    struct FakeService : online::Service
    {
        uint64_t account = 0;
        std::string name;
        bool Start() override { return true; }
        uint64_t LocalId() override { return 1; }
        uint64_t Account() override { return account; }
        std::string LocalName() override { return name; }
        bool Send(uint64_t, uint16_t, uint16_t, const void*, size_t) override { return false; }
        bool Broadcast(uint16_t, uint16_t, const void*, size_t) override { return false; }
        bool Receive(uint16_t, online::Datagram&) override { return false; }
    };

    FakeService g_service;
    bool g_haveService = false, g_live = false;
    signin::Player g_first, g_others[3];
    int g_signinAsked = 0, g_friendsAsked = 0;
}

namespace online
{
    Service* Get() { return g_haveService ? &g_service : nullptr; }
    bool Live() { return g_live; }
}
namespace signin
{
    Player At(uint32_t user) { return user >= 1 && user <= 3 ? g_others[user - 1] : Player{}; }
    Player First() { return g_first; }
    void Ask() { g_signinAsked++; }
}
namespace friends { void Ask() { g_friendsAsked++; } }
namespace presence { void SetContext(uint32_t, uint32_t, uint32_t) {} }

namespace kernel
{
    const std::filesystem::path& SaveRoot()
    {
        static const std::filesystem::path root = std::filesystem::temp_directory_path() / "mw3-xam-test";
        return root;
    }
    void TitleXnAddr(void* out) { std::memset(out, 0, 36); }
    bool XnAddrPeer(const void*, uint64_t&) { return false; }
    void NoteSessionId(const void*) {}
    void ForgetSessionLags() {}
    void NoteSessionLag(const void*, uint32_t) {}
    void SetXnAddrPeer(void*, uint64_t) {}
}

using namespace kernel;

namespace
{
    constexpr uint32_t X_ERROR_NO_MORE_FILES = 0x12;
    constexpr uint32_t kNotLoggedOn = 1245, kFunctionFailed = 1627;
    constexpr uint32_t kOut = 0x3000, kOut2 = 0x3040, kBuffer = 0x3100, kOverlapped = 0x3200;

    void Reset()
    {
        g_haveService = false;
        g_live = false;
        g_service.account = 0;
        g_service.name.clear();
        g_first = { true, 0x00AABBCCDDEEull, "Profile" };
        for (auto& p : g_others) p = {};
    }

    uint32_t Call(PPCFunc* f, uint64_t r3 = 0, uint64_t r4 = 0, uint64_t r5 = 0, uint64_t r6 = 0,
                  uint64_t r7 = 0, uint64_t r8 = 0, uint64_t r9 = 0, uint64_t r10 = 0)
    {
        PPCContext ctx;
        ctx.r3.u64 = r3; ctx.r4.u64 = r4; ctx.r5.u64 = r5; ctx.r6.u64 = r6;
        ctx.r7.u64 = r7; ctx.r8.u64 = r8; ctx.r9.u64 = r9; ctx.r10.u64 = r10;
        f(ctx, guest::Base());
        return uint32_t(ctx.r3.u64);
    }

    void Clear(uint32_t at, uint32_t bytes) { std::memset(guest::Base() + at, 0, bytes); }

    // ---- who is signed in -----------------------------------------------------------
    void SigninState()
    {
        Reset();
        // The first controller is always a player: local without a service, Live with one that carries lobbies.
        assert(Call(__imp__XamUserGetSigninState, 0) == 1);
        g_live = true;
        assert(Call(__imp__XamUserGetSigninState, 0) == 2);
        g_live = false;

        // The others are whoever the sign-in screen put there: local, never Live.
        assert(Call(__imp__XamUserGetSigninState, 1) == 0);
        g_others[0] = { true, 0x111, "Second" };
        assert(Call(__imp__XamUserGetSigninState, 1) == 1);
        g_live = true;
        assert(Call(__imp__XamUserGetSigninState, 1) == 1);
        g_live = false;
        assert(Call(__imp__XamUserGetSigninState, 2) == 0 && Call(__imp__XamUserGetSigninState, 7) == 0);
    }

    void Xuids()
    {
        Reset();
        // The first player's XUID is the offline prefix over the profile's 48 bits.
        Clear(kOut, 8);
        assert(Call(__imp__XamUserGetXUID, 0, 1, kOut) == X_ERROR_SUCCESS);
        assert(uint64_t(*GuestPtr<be64>(kOut)) == 0xE000'00AA'BBCC'DDEEull);

        // With a service, its account number is the identity, cut to 48 bits.
        g_haveService = true;
        g_service.account = 0x1234'5678'9ABC'DEF0ull;
        assert(Call(__imp__XamUserGetXUID, 0, 1, kOut) == X_ERROR_SUCCESS);
        assert(uint64_t(*GuestPtr<be64>(kOut)) == 0xE000'5678'9ABC'DEF0ull);

        // On Live, the masks 2 and 4 ask for the online XUID; 1 still gets the offline one.
        g_live = true;
        assert(Call(__imp__XamUserGetXUID, 0, 1, kOut) == X_ERROR_SUCCESS);
        assert(uint64_t(*GuestPtr<be64>(kOut)) == 0xE000'5678'9ABC'DEF0ull);
        for (uint32_t mask : { 2u, 4u, 6u })
        {
            assert(Call(__imp__XamUserGetXUID, 0, mask, kOut) == X_ERROR_SUCCESS);
            assert(uint64_t(*GuestPtr<be64>(kOut)) == 0x0009'5678'9ABC'DEF0ull);
        }

        // Another controller's player is a local profile with its own offline XUID.
        g_others[1] = { true, 0xABCDEFull, "Third" };
        assert(Call(__imp__XamUserGetXUID, 2, 6, kOut) == X_ERROR_SUCCESS);
        assert(uint64_t(*GuestPtr<be64>(kOut)) == 0xE000'0000'00AB'CDEFull);

        // Nobody there: the error, and nothing written.
        Clear(kOut, 8);
        assert(Call(__imp__XamUserGetXUID, 3, 1, kOut) == X_ERROR_NO_SUCH_USER);
        assert(uint64_t(*GuestPtr<be64>(kOut)) == 0);
        assert(Call(__imp__XamUserGetXUID, 0, 1, 0) == X_ERROR_SUCCESS);   // no out pointer is fine
    }

    void Names()
    {
        Reset();
        Clear(kBuffer, 64);
        assert(Call(__imp__XamUserGetName, 0, kBuffer, 16) == X_ERROR_SUCCESS);
        assert(std::strcmp(GuestPtr<char>(kBuffer), "Profile") == 0);

        // The service's name wins, and a gamertag is at most 15 characters.
        g_haveService = true;
        g_service.name = "AVeryLongSteamName";
        assert(Call(__imp__XamUserGetName, 0, kBuffer, 32) == X_ERROR_SUCCESS);
        assert(std::strcmp(GuestPtr<char>(kBuffer), "AVeryLongSteamN") == 0);
        g_service.name.clear();                      // empty: back to the profile's
        assert(Call(__imp__XamUserGetName, 0, kBuffer, 32) == X_ERROR_SUCCESS);
        assert(std::strcmp(GuestPtr<char>(kBuffer), "Profile") == 0);

        // The buffer's size is respected, always with a terminator.
        std::memset(guest::Base() + kBuffer, 0xEE, 16);
        assert(Call(__imp__XamUserGetName, 0, kBuffer, 4) == X_ERROR_SUCCESS);
        assert(std::memcmp(GuestPtr<char>(kBuffer), "Pro\0", 4) == 0 && *GuestPtr<uint8_t>(kBuffer + 4) == 0xEE);
        std::memset(guest::Base() + kBuffer, 0xEE, 16);
        assert(Call(__imp__XamUserGetName, 0, kBuffer, 0) == X_ERROR_SUCCESS);
        assert(*GuestPtr<uint8_t>(kBuffer) == 0 && *GuestPtr<uint8_t>(kBuffer + 1) == 0xEE);

        assert(Call(__imp__XamUserGetName, 1, kBuffer, 16) == X_ERROR_NO_SUCH_USER);
        g_others[0] = { true, 5, "Second" };
        assert(Call(__imp__XamUserGetName, 1, kBuffer, 16) == X_ERROR_SUCCESS);
        assert(std::strcmp(GuestPtr<char>(kBuffer), "Second") == 0);
    }

    void SigninInfo()
    {
        Reset();
        // XUSER_SIGNIN_INFO { xuid, flags, state, guest, sponsor, name[16] }
        Clear(kBuffer, 64);
        assert(Call(__imp__XamUserGetSigninInfo, 0, 0, kBuffer) == X_ERROR_SUCCESS);
        assert(uint64_t(*GuestPtr<be64>(kBuffer)) == 0xE000'00AA'BBCC'DDEEull);
        assert(uint32_t(*GuestPtr<be32>(kBuffer + 8)) == 0);       // not on Live
        assert(uint32_t(*GuestPtr<be32>(kBuffer + 12)) == 1);      // signed in locally
        assert(std::strcmp(GuestPtr<char>(kBuffer + 24), "Profile") == 0);

        g_haveService = true;
        g_service.account = 0x42;
        g_live = true;
        assert(Call(__imp__XamUserGetSigninInfo, 0, 0, kBuffer) == X_ERROR_SUCCESS);
        assert(uint64_t(*GuestPtr<be64>(kBuffer)) == 0x0009'0000'0000'0042ull);   // Live names by the online XUID
        assert(uint32_t(*GuestPtr<be32>(kBuffer + 8)) == 1 && uint32_t(*GuestPtr<be32>(kBuffer + 12)) == 2);
        assert(Call(__imp__XamUserGetSigninInfo, 0, 2, kBuffer) == X_ERROR_SUCCESS);   // 2: offline only
        assert(uint64_t(*GuestPtr<be64>(kBuffer)) == 0xE000'0000'0000'0042ull);

        assert(Call(__imp__XamUserGetSigninInfo, 2, 0, kBuffer) == X_ERROR_NO_SUCH_USER);
    }

    void Privileges()
    {
        Reset();
        // A profile that is not on Live is not logged on, which the title learns from what it was refused.
        Clear(kOut, 4);
        assert(Call(__imp__XamUserCheckPrivilege, 0, 0x0B, kOut) == kNotLoggedOn);
        assert(uint32_t(*GuestPtr<be32>(kOut)) == 0);

        // On Live the player has them all.
        g_live = true;
        assert(Call(__imp__XamUserCheckPrivilege, 0, 0x0B, kOut) == X_ERROR_SUCCESS);
        assert(uint32_t(*GuestPtr<be32>(kOut)) == 1);

        // Any local profile at any controller makes "anybody" not logged on.
        g_others[0] = { true, 9, "Second" };
        assert(Call(__imp__XamUserCheckPrivilege, 0xFF, 0x0B, kOut) == kNotLoggedOn);
        assert(Call(__imp__XamUserCheckPrivilege, 1, 0x0B, kOut) == kNotLoggedOn);
        assert(uint32_t(*GuestPtr<be32>(kOut)) == 0);

        // Friendship and statistics are not carried.
        *GuestPtr<be32>(kOut) = 5;
        assert(Call(__imp__XamUserAreUsersFriends, 0, 1, kOut) == X_ERROR_SUCCESS);
        assert(uint32_t(*GuestPtr<be32>(kOut)) == 0);
        assert(Call(__imp__XamUserCreateStatsEnumerator) == X_ERROR_NOT_FOUND);
    }

    void Screens()
    {
        const int signin = g_signinAsked, friends = g_friendsAsked;
        assert(Call(__imp__XamShowSigninUI, 1, 0) == X_ERROR_SUCCESS && g_signinAsked == signin + 1);
        assert(Call(__imp__XamShowFriendsUI, 0) == X_ERROR_SUCCESS && g_friendsAsked == friends + 1);
        assert(Call(__imp__XamShowMessageBoxUIEx) == X_ERROR_NOT_FOUND);
        assert(Call(__imp__XamShowDirtyDiscErrorUI) == X_ERROR_SUCCESS);   // it is reported, not acted on
    }

    // ---- notifications ------------------------------------------------------------------
    void Notifications()
    {
        Reset();
        constexpr uint32_t kSystem = 0x00000001, kLive = 0x02000001, kFriends = 0x04000001;
        const uint64_t areaSystem = 1ull << 0, areaLive = 1ull << 1, areaFriends = 1ull << 2;

        const uint32_t live = Call(__imp__XamNotifyCreateListener, areaLive, 0);
        const uint32_t friendsOnly = Call(__imp__XamNotifyCreateListener, areaFriends, 0);
        const uint32_t both = Call(__imp__XamNotifyCreateListener, areaLive | areaSystem, 0);
        assert(live && friendsOnly && both && live != friendsOnly);

        auto next = [&](uint32_t handle, uint32_t only = 0, uint32_t* id = nullptr, uint32_t* param = nullptr) {
            *GuestPtr<be32>(kOut) = 0xDEAD; *GuestPtr<be32>(kOut2) = 0xBEEF;
            const uint32_t got = Call(__imp__XNotifyGetNext, handle, only, kOut, kOut2);
            if (id) *id = *GuestPtr<be32>(kOut);
            if (param) *param = *GuestPtr<be32>(kOut2);
            return got;
        };
        uint32_t id = 1, param = 1;

        // Nothing is waiting: no, and the outputs are cleared.
        assert(next(live, 0, &id, &param) == 0 && id == 0 && param == 0);
        assert(next(0xDEADBEEF, 0, &id, &param) == 0 && id == 0);   // not a listener at all

        // A notification goes to every listener of its area, in order, and only those.
        kernel::PostNotification(kLive, 10);
        kernel::PostNotification(kLive, 11);
        kernel::PostNotification(kSystem, 12);
        kernel::PostNotification(kFriends, 13);
        assert(next(live, 0, &id, &param) == 1 && id == kLive && param == 10);
        assert(next(live, 0, &id, &param) == 1 && id == kLive && param == 11);
        assert(next(live) == 0);                                 // the system one is not for it
        assert(next(friendsOnly, 0, &id, &param) == 1 && id == kFriends && param == 13);
        assert(next(friendsOnly) == 0);

        // A listener can ask for one id, leaving the rest queued in order.
        assert(next(both, kSystem, &id, &param) == 1 && id == kSystem && param == 12);
        assert(next(both, kSystem) == 0);
        assert(next(both, 0, &id, &param) == 1 && id == kLive && param == 10);
        assert(next(both, 0, &id, &param) == 1 && id == kLive && param == 11);

        // A listener that has been closed no longer hears anything, and nothing breaks.
        assert(CloseHandle(friendsOnly));
        kernel::PostNotification(kFriends, 14);
        assert(next(friendsOnly) == 0);
        kernel::PostNotification(kLive, 15);
        assert(next(live, 0, &id, &param) == 1 && param == 15);
    }

    void LiveListenersAreToldTheyAreConnected()
    {
        Reset();
        // Signed in to Live, the connection came up before the title was listening, so a
        // new listener of the Live area is told at once. The title then asks for its NAT type.
        g_live = true;
        const uint32_t late = Call(__imp__XamNotifyCreateListener, 1ull << 1, 0);
        *GuestPtr<be32>(kOut) = 0; *GuestPtr<be32>(kOut2) = 0;
        assert(Call(__imp__XNotifyGetNext, late, 0, kOut, kOut2) == 1);
        assert(uint32_t(*GuestPtr<be32>(kOut)) == 0x02000001);                  // XN_LIVE_CONNECTIONCHANGED
        assert(uint32_t(*GuestPtr<be32>(kOut2)) == 0x001510F0);                 // ...connection established
        assert(Call(__imp__XNotifyGetNext, late, 0, kOut, kOut2) == 0);

        // Not for a listener of another area, and not when offline.
        const uint32_t system = Call(__imp__XamNotifyCreateListener, 1ull << 0, 0);
        assert(Call(__imp__XNotifyGetNext, system, 0, kOut, kOut2) == 0);
        g_live = false;
        const uint32_t offline = Call(__imp__XamNotifyCreateListener, 1ull << 1, 0);
        assert(Call(__imp__XNotifyGetNext, offline, 0, kOut, kOut2) == 0);
    }

    // ---- overlapped completion and the device selector -----------------------------------
    struct Overlapped { uint32_t result, length, context, event, routine, routineContext, extended; };
    Overlapped ReadOverlapped()
    {
        auto* o = GuestPtr<be32>(kOverlapped);
        return { o[0], o[1], o[2], o[3], o[4], o[5], o[6] };
    }

    void OverlappedCompletion()
    {
        Clear(kOverlapped, 32);
        auto event = std::make_shared<Event>();
        event->manualReset = true;
        const uint32_t handle = InsertHandle(event);
        *GuestPtr<be32>(kOverlapped + 12) = handle;
        *GuestPtr<be32>(kOverlapped) = X_ERROR_IO_PENDING;

        kernel::CompleteOverlappedEx(kOverlapped, 7, 0x80070012, 99);
        const Overlapped o = ReadOverlapped();
        assert(o.result == 7 && o.length == 99 && o.extended == 0x80070012 && o.event == handle);
        assert(event->signalled);                    // the title waits on the event at +12

        // With no event, or an event that is not one, completion still stores its result.
        event->Reset();
        *GuestPtr<be32>(kOverlapped + 12) = 0;
        kernel::CompleteOverlapped(kOverlapped, X_ERROR_SUCCESS);
        assert(ReadOverlapped().result == 0 && ReadOverlapped().extended == 0 && !event->signalled);
        *GuestPtr<be32>(kOverlapped + 12) = 0xDEADBEEF;
        kernel::CompleteOverlapped(kOverlapped, 5);
        assert(ReadOverlapped().result == 5);
        kernel::CompleteOverlapped(0, 5);            // a null block is ignored
        CloseHandle(handle);
    }

    void DeviceSelector()
    {
        // The title accepts one answer only: "pending", with the choice reported through the
        // overlapped block, the hard disk as the content layer names it.
        Clear(kOverlapped, 32);
        Clear(kOut, 4);
        auto event = std::make_shared<Event>();
        const uint32_t handle = InsertHandle(event);
        *GuestPtr<be32>(kOverlapped + 12) = handle;
        assert(Call(__imp__XamShowDeviceSelectorUI, 0, 1, 0, 0, kOut, kOverlapped) == X_ERROR_IO_PENDING);
        assert(uint32_t(*GuestPtr<be32>(kOut)) == kSaveDeviceId);
        assert(ReadOverlapped().result == X_ERROR_SUCCESS && event->signalled);

        // Without an overlapped block there is nothing to wait on: plain success.
        Clear(kOut, 4);
        assert(Call(__imp__XamShowDeviceSelectorUI, 0, 1, 0, 0, kOut, 0) == X_ERROR_SUCCESS);
        assert(uint32_t(*GuestPtr<be32>(kOut)) == kSaveDeviceId);
        assert(Call(__imp__XamShowDeviceSelectorUI, 0, 1, 0, 0, 0, 0) == X_ERROR_SUCCESS);
        CloseHandle(handle);
    }

    // ---- enumerators --------------------------------------------------------------------------
    void Enumerators()
    {
        // XamCreateEnumeratorHandle gives the title a private block, zeroed, that it reads back by handle.
        std::memset(guest::Base() + 0x900000, 0xAA, 0x400);
        assert(Call(__imp__XamCreateEnumeratorHandle, 0, 0, 0, 0, 0x40, 8, 0, kOut) == X_ERROR_SUCCESS);
        const uint32_t handle = *GuestPtr<be32>(kOut);
        assert(handle);
        assert(Call(__imp__XamGetPrivateEnumStructureFromHandle, handle, kOut2) == X_ERROR_SUCCESS);
        const uint32_t extra = *GuestPtr<be32>(kOut2);
        assert(extra && extra % 16 == 0);
        for (uint32_t i = 0; i < 0x40; i++) assert(*GuestPtr<uint8_t>(extra + i) == 0);

        // Closing it gives the block back.
        const uint32_t again = kernel::AllocateGuest(0x40, 16);
        assert(again != extra);
        kernel::FreeGuest(again);
        assert(CloseHandle(handle));
        assert(kernel::AllocateGuest(0x40, 16) == extra);   // reused, so it was freed
        assert(Call(__imp__XamGetPrivateEnumStructureFromHandle, handle, kOut2) == X_ERROR_INVALID_HANDLE);
        assert(uint32_t(*GuestPtr<be32>(kOut2)) == 0);

        // No private block asked for: a handle with none.
        assert(Call(__imp__XamCreateEnumeratorHandle, 0, 0, 0, 0, 0, 8, 0, kOut) == X_ERROR_SUCCESS);
        const uint32_t plain = *GuestPtr<be32>(kOut);
        assert(Call(__imp__XamGetPrivateEnumStructureFromHandle, plain, kOut2) == X_ERROR_SUCCESS);
        assert(uint32_t(*GuestPtr<be32>(kOut2)) == 0);

        // A content enumerator says how big its buffer must be: 0x134 bytes an item.
        assert(Call(__imp__XamContentCreateEnumerator, 0, 1, 1, 0, 10, kOut, kOut2) == X_ERROR_SUCCESS);
        assert(uint32_t(*GuestPtr<be32>(kOut)) == 10 * 0x134);
        const uint32_t content = *GuestPtr<be32>(kOut2);
        assert(content);

        // There is nothing to list: ERROR_NO_MORE_FILES, immediately or through the overlapped block.
        *GuestPtr<be32>(kBuffer) = 99;
        assert(Call(__imp__XamEnumerate, content, 0, 0, 0, kBuffer, 0) == X_ERROR_NO_MORE_FILES);
        assert(uint32_t(*GuestPtr<be32>(kBuffer)) == 0);
        Clear(kOverlapped, 32);
        assert(Call(__imp__XamEnumerate, content, 0, 0, 0, kBuffer, kOverlapped) == X_ERROR_IO_PENDING);
        assert(ReadOverlapped().result == kFunctionFailed && ReadOverlapped().extended == (0x80070000 | X_ERROR_NO_MORE_FILES));
        assert(Call(__imp__XamEnumerate, 0xDEADBEEF, 0, 0, 0, kBuffer, 0) == X_ERROR_INVALID_HANDLE);
        CloseHandle(plain);
        CloseHandle(content);
    }

    void Allocation()
    {
        assert(Call(__imp__XamAlloc, 0, 0x100, kOut) == X_ERROR_SUCCESS);
        const uint32_t block = *GuestPtr<be32>(kOut);
        assert(block && block % 16 == 0);
        assert(Call(__imp__XamFree, block) == X_ERROR_SUCCESS);
        assert(Call(__imp__XamAlloc, 0, 0x100, kOut) == X_ERROR_SUCCESS);   // first-fit may use an earlier hole
        assert(*GuestPtr<be32>(kOut) && *GuestPtr<be32>(kOut) <= block);
        assert(Call(__imp__XamAlloc, 0, 0, kOut) == X_STATUS_NO_MEMORY && uint32_t(*GuestPtr<be32>(kOut)) == 0);
        assert(Call(__imp__XamFree, 0x12345) == X_ERROR_SUCCESS);   // not a block: harmless
    }

    void ConfigSettings()
    {
        // XCONFIG_USER_CATEGORY: language is English, the others zero, each with its own width.
        Clear(kBuffer, 16);
        Clear(kOut, 4);
        assert(Call(__imp__ExGetXConfigSetting, 3, 2, kBuffer, 4, kOut) == X_ERROR_SUCCESS);
        assert(uint32_t(*GuestPtr<be32>(kBuffer)) == 1 && uint16_t(*GuestPtr<be16>(kOut)) == 4);
        assert(Call(__imp__ExGetXConfigSetting, 3, 1, kBuffer, 4, kOut) == X_ERROR_SUCCESS);
        assert(uint16_t(*GuestPtr<be16>(kOut)) == 1);

        // A buffer too small is left alone while the width is still reported.
        *GuestPtr<be32>(kBuffer) = 0x55555555;
        assert(Call(__imp__ExGetXConfigSetting, 3, 2, kBuffer, 2, kOut) == X_ERROR_SUCCESS);
        assert(uint32_t(*GuestPtr<be32>(kBuffer)) == 0x55555555 && uint16_t(*GuestPtr<be16>(kOut)) == 4);

        // Anything else is a zero of the default width.
        *GuestPtr<be32>(kBuffer) = 0x55555555;
        assert(Call(__imp__ExGetXConfigSetting, 9, 9, kBuffer, 4, kOut) == X_ERROR_SUCCESS);
        assert(uint32_t(*GuestPtr<be32>(kBuffer)) == 0 && uint16_t(*GuestPtr<be16>(kOut)) == 4);
        assert(Call(__imp__ExGetXConfigSetting, 3, 2, 0, 0, 0) == X_ERROR_SUCCESS);
    }
}

int main()
{
    SigninState();
    Xuids();
    Names();
    SigninInfo();
    Privileges();
    Screens();
    Notifications();
    LiveListenersAreToldTheyAreConnected();
    OverlappedCompletion();
    DeviceSelector();
    Enumerators();
    Allocation();
    ConfigSettings();
    std::puts("kernel-xam-test ok");
    std::fflush(stdout);
    return 0;
}
