// Copies of different releases do not play together, by the title's own
// version check, and a player turned away is told the host's release.
//
// The multiplayer has one protocol number, from sub_8230A3D8: 136 (128 on the
// disc), plus the version of the online update the console's Live service
// patched it with, in the high half. Every check reads it:
//   - CL_ServerInfoPacket leaves a System Link game of another protocol out of
//     the list;
//   - SV_DirectConnect refuses a client of another protocol (one the client's
//     connect sends in its userinfo), answering
//     "error\nEXE_SERVER_IS_DIFFERENT_VER\x15<version>\n", which the client
//     shows as "Server is a different version:\n<version>";
//   - PartyHost_HandleJoinPartyRequest refuses a party member of another
//     protocol ("Incompatible party net code"), and the member sees "Unable to
//     join game session".
// The online update never runs here, so every copy said 136. The high half
// now carries the release (version::Network()) instead, and the low half is
// the title's own, so the disc version and the update still differ. The
// campaign's co-op has no such check.
//
// <version> is the string "1.4", the build number every copy has. The answer
// carries the host's release instead: NET_OutOfBandPrint, which sends it, is
// given a copy with the release in its place. The format is SV_DirectConnect's
// alone.
#include <ppc_recomp_shared.h>
#include "title.h"
#include "version.h"

#include <cstdint>
#include <cstring>
#include <string>

#if defined(T_ProtocolVersion) && defined(T_NET_OutOfBandPrint)

GUEST_HOOK(T_ProtocolVersion)
{
    GUEST_ORIG(T_ProtocolVersion)(ctx, base);
    ctx.r3.u64 = (ctx.r3.u32 & 0xFFFFu) | (uint32_t(version::Network()) << 16);
}

// (socket, address, text): the address is passed by value in r4 and r5.
GUEST_HOOK(T_NET_OutOfBandPrint)
{
    static constexpr char kDifferent[] = "error\nEXE_SERVER_IS_DIFFERENT_VER\x15";
    const uint32_t text = ctx.r6.u32;
    const uint32_t sp = ctx.r1.u32;
    if (!text || sp < 0x2000 ||
        std::strncmp(reinterpret_cast<const char*>(base + text), kDifferent, sizeof kDifferent - 1) != 0)
    {
        GUEST_ORIG(T_NET_OutOfBandPrint)(ctx, base);
        return;
    }
    const std::string answer = std::string(kDifferent) + (*version::Release() ? version::Release() : "dev") + "\n";
    // Just below the guest stack pointer, with the call made at a stack
    // pointer pushed past it, as map_name_command.cpp does.
    const uint32_t scratch = (sp - 0x100 - uint32_t(answer.size()) - 1) & ~0xFu;
    std::memcpy(base + scratch, answer.c_str(), answer.size() + 1);
    const uint64_t savedSp = ctx.r1.u64;
    ctx.r1.u64 = (scratch - 0x100) & ~0xFull;
    ctx.r6.u64 = scratch;
    GUEST_ORIG(T_NET_OutOfBandPrint)(ctx, base);
    ctx.r1.u64 = savedSp;
}

#endif
