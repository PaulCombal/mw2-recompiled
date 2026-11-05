// iw4x: the "relay" connectionless packet is an unauthenticated forwarder --
// disable it.
//
// CL_HandleRelayPacket (sub_82184B88) is the handler for the "relay" out-of-band
// packet. It reads a target client slot from argv[1] and forwards the packet's
// payload to that client through the party mesh's "send an out-of-band packet to
// client N" router (sub_82191DF0). The only check is "don't relay to myself" --
// nothing checks who sent the request. So any host that can deliver a packet to
// this client (on lan, anyone on the network; on steam, any lobby member) can
// make it forward attacker-chosen bytes to another peer: the forwarded packet
// arrives looking as if it came from this client (spoofing), and the client
// serves as a reflector aimed at a third party. iw4x disables the handler
// outright; this does the same.
//
// The sender's address is not available here to check against the party -- the
// dispatcher passes this handler only the message and a flag, not the "from"
// address -- so a sender check would have to live in the dispatcher, not here.
// Disabling the handler is iw4x's remedy and the one chosen here.
//
// "relay" is the party mesh's fallback for reaching a peer that has no direct
// route, by bouncing the packet through an intermediary member. Disabling the
// receive side alone would lose that: a copy running this build can still choose
// to relay through another member, and that member now drops the packet -- and
// the sending router (CL_ForwardRelayPacket) sends through the intermediary once
// and returns, with no retry and no direct fallback, so the packet is simply
// lost. So the send side is closed too (below): the next-hop lookup is made to
// never pick an intermediary, which routes every send directly.
//
// This works because both transports here deliver between any two members
// directly (the lan transport is a single network; the steam transport relays
// through Steam), so the direct send always reaches the target and the mesh's
// own relay fallback is never needed. The out-of-band dispatcher marks the
// packet handled regardless of what the receive hook returns, so not forwarding
// is a clean drop. MP-only: the campaign has no such handler.

use crate::guest::{Register, guest_hook, preserving};
use crate::log::log_warn;
use crate::title::*;
use std::sync::Once;

#[cfg(T_CL_HandleRelayPacket)]
static LOGGED: Once = Once::new();

#[cfg(T_CL_HandleRelayPacket)]
guest_hook!(T_CL_HandleRelayPacket, |_ctx, _memory| {
    // Drop the request without forwarding. Log once -- in normal play relay is
    // rarely reached, so a packet here is usually an outside reflection attempt.
    LOGGED.call_once(|| log_warn!("online: not forwarding a relay packet (relay forwarding is disabled)"));
});

// CL_RouteToClient(session = r3, target = r4) -> next hop: the local client when
// the target is directly reachable, an intermediary member when it is not, or -1
// for no route. With relay disabled, a send that picks an intermediary is lost,
// so turn that case into -1 -- which the forwarder handles by firing the packet
// off directly itself. The directly-reachable and no-route answers pass through
// unchanged, so nothing but the intermediary case is diverted.
#[cfg(all(T_CL_RouteToClient, T_Session_LocalClientNum))]
guest_hook!(T_CL_RouteToClient, |ctx, memory| {
    let session = ctx.r3.as_u32();

    // The local client number, derived the way the function itself does
    // (Session_LocalClientNum of session+1440). Computed first, with the args
    // saved and restored so the original runs untouched.
    let local = preserving(ctx, |ctx| {
        ctx.r3.u64 = u64::from(memory.u32_at(session.wrapping_add(1440)));
        ctx.r4.u64 = u64::from(session); // the lookup takes (field, session); session is the member table it walks
        T_Session_LocalClientNum.call(ctx, memory);
        ctx.r3.as_i32()
    });

    T_CL_RouteToClient.original(ctx, memory);
    let route = ctx.r3.as_i32();
    if route >= 0 && route != local {
        ctx.r3.s64 = -1; // an intermediary hop -> force the forwarder's direct path
    }
});
