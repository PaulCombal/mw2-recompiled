// iw4x/Quake 3: a client takes packets about its connection only from the
// server it is connected to -- drop the ones from anyone else.
//
// CL_PacketEvent (sub_82189A88) is the client's entry for every packet that
// reaches a local client. A connectionless packet (first word -1) goes on to
// the out-of-band dispatcher; any other is a sequenced packet of the game
// connection, and once the connection state is at least 5 it goes straight to
// Netchan_Process (sub_8228E010) and from there to CL_ParseServerMessage. No
// step compares the sender with the netchan's remote address: CL_PacketEvent
// passes Netchan_Process only the channel and the message, and Netchan_Process
// tests only that the packet's own sequence number is newer than the last
// (cmpw r26,r29; bgt). Its callers (the queue drain sub_8227FFA8 and the
// local-client loop sub_82280028) do not compare either. Quake 3 and iw4x
// drop the packet unless NET_CompareAdr(from, netchan.remoteAddress) holds.
// Without the check, any peer the transport lets through (on lan, anyone on
// the network; on steam, any lobby member) can hand a connected client a whole
// server message -- snapshots, and reliable server commands such as the
// set-dvar command, which writes any dvar past its flags -- and one packet
// with sequence 0x7FFFFFFF makes the client drop every later packet from the
// real server as out of order, so it times out.
//
// The out-of-band connection handshake has the same gap. In the dispatcher
// (sub_82188DD0) challengeResponse, connectResponse and statResponse test only
// the connection state: challengeResponse (state 3) then stores the sender as
// the server address, so the connect line, with the player's userinfo, goes to
// whoever answered; connectResponse (state 4) sets the netchan up to the sender,
// which the check above would then accept; statResponse (state 6) moves the
// stat sync on. Quake 3 and iw4x refuse challengeResponse and connectResponse
// from an address other than clc.serverAddress. The dispatcher's other
// connection-changing commands already check: error, loadingnewmap and
// fastrestart compare the IP, and disconnect the netchan's whole address.
//
// So do the in-game host migration messages, which the migration dispatcher
// (sub_8218BDF8, which CL_ConnectionlessPacket calls among its commands) takes
// from anyone. The server streams its game state to the client it picked as
// the next host: mstart starts it, mhead brings the save header, mdata the
// state in 1200-byte chunks, each with a checksum of its own bytes only, and
// mstate the server's progress. The client loads what it got when it becomes
// the host (G_InitGame sub_82233A68 -> sub_8223F360). The server sends all four
// from its own socket (NET_OutOfBandData sub_8228EF80 with NS_SERVER, e.g.
// sub_822C54A0), and the client acknowledges mstart, mhead and mdata to its
// server address (sub_8218B750), never to the sender. Taking them only from that
// address is the rule the acknowledgements already assume. ping and pinga stay
// open: members ping each other, and the ping answer goes only to a member.
//
// The rule is the console's. Its secure network layer gives each peer one
// address, and the server sends everything from the socket the client reached
// it on, so the checks never refuse the real server. It holds here too:
// XNetXnAddrToInAddr and recvfrom (runtime/kernel/net.cpp) give a peer the same
// stand-in address, both keyed by the service's peer id, and the loopback
// address of a copy's own server compares equal by type. The one setup where
// a real server's packets would carry another address is a lan host whose
// replies leave by a different interface than the first adapter it announces
// (lan.cpp OwnAddress); the party code's XNADDR lookups already fail there.
//
// The compare is the title's own NET_CompareAdr (sub_8228E5B0, the one the
// disconnect handler uses): same type, then for an IP address the four bytes
// and the port; two addresses of any other type (the loopback) compare equal
// on the type alone, which the transport sets, not the sender. A refused packet gets the answer the original gives a packet
// it does not take (0, "not for this local client"), which sends the
// local-client loop on to the next client as it does for a packet that arrives
// before the connection is up. The one difference: a runt packet (under four
// bytes) from someone else now gets that 0 too, where the original prints
// "Runt packet" and answers 1, which only lets the loop offer it to the other
// local clients as well. MP-only: the campaign's packet handler
// (sub_8233C508) looks up the co-op client by the sender's address and answers
// anyone else with coopDis.

use crate::guest::{Memory, PPCContext, Register, guest_hook, preserving};
use crate::log::{Argument, Doubling, log_warn};
use crate::title::{
    T_CL_ConnectionlessPacket, T_CL_PacketEvent, T_CONN_STATE_STRIDE, T_CONNECTION_NETCHAN_ADR,
    T_CONNECTION_SERVER_ADR, T_CONNECTION_STRIDE, T_DATA_ClientConnState, T_DATA_ClientConnections, T_DATA_CmdArgs,
    T_NET_CompareAdr,
};
use std::ffi::CStr;

// Per local client: the connection state, and the client connection, which
// holds the server address and the game netchan.
fn connection_state(memory: Memory, local_client: u32) -> u32 {
    memory.u32_at(T_DATA_ClientConnState.wrapping_add(local_client.wrapping_mul(T_CONN_STATE_STRIDE)))
}

fn connection(memory: Memory, local_client: u32) -> u32 {
    memory.u32_at(T_DATA_ClientConnections).wrapping_add(local_client.wrapping_mul(T_CONNECTION_STRIDE))
}

// A netadr_t (type, four IP bytes, port) arrives in two registers: bytes 0..7,
// then bytes 8..11 in the high word of the second, which is also how
// NET_CompareAdr takes it.
#[derive(Copy, Clone)]
struct Sender(u64, u64);

fn same_address(ctx: &mut PPCContext, memory: Memory, sender: Sender, address: u32) -> bool {
    preserving(ctx, |ctx| {
        ctx.r3.u64 = sender.0;
        ctx.r4.u64 = sender.1;
        ctx.r5.u64 = memory.u64_at(address);
        ctx.r6.u64 = u64::from(memory.u32_at(address.wrapping_add(8))) << 32;
        T_NET_CompareAdr.call(ctx, memory);
        ctx.r3.as_u32() != 0
    })
}

// The sender as the log shows it: type, then IP:port for an IP address.
fn refused(what: impl Argument, sender: Sender, dropped: &Doubling) {
    if let Some(count) = dropped.count() {
        let ip = sender.0 as u32;
        log_warn!(
            "online: dropped %s from %u.%u.%u.%u:%u (type %u), not the server (%u so far)",
            what,
            ip >> 24,
            (ip >> 16) & 0xFF,
            (ip >> 8) & 0xFF,
            ip & 0xFF,
            (sender.1 >> 48) as u32,
            (sender.0 >> 32) as u32,
            count
        );
    }
}

static SEQUENCED: Doubling = Doubling::new();
static CHALLENGE: Doubling = Doubling::new();
static CONNECT: Doubling = Doubling::new();
static STAT: Doubling = Doubling::new();
static MIGRATION: Doubling = Doubling::new();

// CL_PacketEvent(localClient = r3, from = r4:r5, msg = r6, time = r7) -> handled
guest_hook!(T_CL_PacketEvent, |ctx, memory| {
    let local_client = ctx.r3.as_u32();
    let msg = ctx.r6.as_u32();
    // msg->cursize at +20, msg->data at +8, as the original reads them.
    let connectionless =
        memory.u32_at(msg.wrapping_add(20)) >= 4 && memory.u32_at(memory.u32_at(msg.wrapping_add(8))) == 0xFFFFFFFF;
    // At a state below 5 the original refuses a sequenced packet itself.
    if !connectionless && connection_state(memory, local_client) >= 5 {
        let sender = Sender(ctx.r4.as_u64(), ctx.r5.as_u64());
        let netchan = connection(memory, local_client).wrapping_add(T_CONNECTION_NETCHAN_ADR);
        if !same_address(ctx, memory, sender, netchan) {
            refused(c"a game packet", sender, &SEQUENCED);
            ctx.r3.u64 = 0;
            return;
        }
    }
    T_CL_PacketEvent.original(ctx, memory);
});

// CL_ConnectionlessPacket(localClient = r3, from = r4:r5, msg = r6, time = r7)
// -> handled. Its wrapper (sub_82189898) has tokenized the line already.
guest_hook!(T_CL_ConnectionlessPacket, |ctx, memory| {
    let local_client = ctx.r3.as_u32();
    let args = T_DATA_CmdArgs;
    let nest = memory.u32_at(args).wrapping_mul(4);
    // The original's argc test (ble -> "").
    let name = if memory.u32_at(args.wrapping_add(68).wrapping_add(nest)) as i32 > 0 {
        memory.pointer_at(memory.u32_at(args.wrapping_add(100).wrapping_add(nest)))
    } else {
        None
    };

    // The dispatcher's compare, I_stricmp (sub_82315b70, strnicmp with no length
    // limit): ASCII case-folded.
    let is = |text: &CStr| name.is_some_and(|name| memory.holds_any_case(name, text));

    // Each command only at the one state where the original acts on it; at any
    // other it refuses the command itself.
    let state = connection_state(memory, local_client);
    let dropped = if state == 3 && is(c"challengeResponse") {
        Some(&CHALLENGE)
    } else if state == 4 && is(c"connectResponse") {
        Some(&CONNECT)
    } else if state == 6 && is(c"statResponse") {
        Some(&STAT)
    // The migration messages at any state: each handler tests its own stage,
    // and only the server has a reason to send one.
    } else if is(c"mstart") || is(c"mhead") || is(c"mdata") || is(c"mstate") {
        Some(&MIGRATION)
    } else {
        None
    };
    if let (Some(dropped), Some(name)) = (dropped, name) {
        let sender = Sender(ctx.r4.as_u64(), ctx.r5.as_u64());
        let server = connection(memory, local_client).wrapping_add(T_CONNECTION_SERVER_ADR);
        if !same_address(ctx, memory, sender, server) {
            refused(memory.text(name), sender, dropped);
            ctx.r3.u64 = 0;
            return;
        }
    }
    T_CL_ConnectionlessPacket.original(ctx, memory);
});
