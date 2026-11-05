// Copies of different releases do not play together, by the title's own version
// check, and a player turned away is told the host's release.
//
// The multiplayer has one protocol number, from sub_8230A3D8: 136 (128 on the
// disc), plus the version of the online update the console's Live service patched
// it with, in the high half. Every check reads it:
//   - CL_ServerInfoPacket leaves a System Link game of another protocol out of
//     the list;
//   - SV_DirectConnect refuses a client of another protocol (one the client's
//     connect sends in its userinfo), answering
//     "error\nEXE_SERVER_IS_DIFFERENT_VER\x15<version>\n", which the client
//     shows as "Server is a different version:\n<version>";
//   - PartyHost_HandleJoinPartyRequest refuses a party member of another
//     protocol ("Incompatible party net code"), and the member sees "Unable to
//     join game session".
// The online update never runs here, so every copy said 136. The high half now
// carries the release (version::NETWORK) instead, and the low half is the title's
// own, so the disc version and the update still differ. The campaign's co-op has
// no such check.
//
// <version> is the string "1.4", the build number every copy has. The answer
// carries the host's release instead: NET_OutOfBandPrint, which sends it, is given
// a copy with the release in its place. The format is SV_DirectConnect's alone.

use crate::guest::{Memory, PPCContext, Register, guest_hook};
use crate::runtime::version;
use crate::title::*;
use std::ffi::CStr;

const DIFFERENT: &CStr = c"error\nEXE_SERVER_IS_DIFFERENT_VER\x15";

guest_hook!(T_ProtocolVersion, |ctx, memory| {
    T_ProtocolVersion.original(ctx, memory);
    ctx.r3.u64 = u64::from((ctx.r3.as_u32() & 0xFFFF) | (u32::from(version::NETWORK) << 16));
});

// (socket, address, text): the address is passed by value in r4 and r5.
fn print_with_release(ctx: &mut PPCContext, memory: Memory) {
    let text = ctx.r6.as_u32();
    let sp = ctx.r1.as_u32();
    if text == 0 || sp < 0x2000 || !memory.starts_with(text, DIFFERENT) {
        T_NET_OutOfBandPrint.original(ctx, memory);
        return;
    }
    let release = if version::RELEASE.is_empty() { c"dev" } else { version::RELEASE };
    let prefix = DIFFERENT.to_bytes();
    let tag = release.to_bytes();
    let length = (prefix.len() + tag.len() + 1) as u32;
    // Just below the guest stack pointer, with the call made at a stack pointer
    // pushed past it, as map_name_command.rs does.
    let scratch = sp.wrapping_sub(0x100).wrapping_sub(length).wrapping_sub(1) & !0xF;
    memory.store_bytes(scratch, prefix);
    memory.store_bytes(scratch.wrapping_add(prefix.len() as u32), tag);
    memory.store_bytes(scratch.wrapping_add(length - 1), b"\n\0");
    let saved_sp = ctx.r1.as_u64();
    ctx.r1.u64 = u64::from(scratch.wrapping_sub(0x100) & !0xF);
    ctx.r6.u64 = u64::from(scratch);
    T_NET_OutOfBandPrint.original(ctx, memory);
    ctx.r1.u64 = saved_sp;
}

guest_hook!(T_NET_OutOfBandPrint, |ctx, memory| {
    print_with_release(ctx, memory);
});
