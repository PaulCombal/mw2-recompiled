// XNet / Winsock over host UDP sockets, enough for system link on one machine, or over
// the online service (src/runtime/online/) when one was chosen. Every call answers
// definitively rather than leave the title polling -- single-player spins on
// XNetGetTitleXnAddr until the address stops being "pending".
//
// With a service, a player is the service's peer id. The console's XNADDR is opaque to
// the title, so the id travels inside it, and the title's sockets reach a peer through a
// stand-in address in 10.0.0.0/8 that XNetXnAddrToInAddr hands out, as the console's
// secure network layer hands out addresses of its own. The title's sockets are then the
// service's: nothing is bound on the host, so any number of copies can run on one
// machine. Datagrams to a stand-in address or to the broadcast address go to the
// service, and ones to this machine's own address come straight back.
//
// Without one, a title socket is a host socket made when it is first bound or used, as
// a host binds an unbound socket that sends: the standard library makes a socket bound.

use super::{allocate_guest, free_guest, random_bytes, set_event_handle, stack_argument};
use crate::env;
use crate::guest::{Memory, Register, kernel_import};
use crate::log::{c_string, log_info, log_warn};
use crate::runtime::online::{self, Datagram};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream, UdpSocket};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

// The title tests these as a bitfield.
const XNET_GET_XNADDR_STATIC: u32 = 0x0000_0004;
const XNET_GET_XNADDR_ETHERNET: u32 = 0x0000_0002;
const XNET_GET_XNADDR_GATEWAY: u32 = 0x0000_0020;
const XNET_GET_XNADDR_DNS: u32 = 0x0000_0040;
const XNET_GET_XNADDR_ONLINE: u32 = 0x0000_0080;

const WSAEINTR: u32 = 10004;
const WSAEACCES: u32 = 10013;
const WSAEINVAL: u32 = 10022;
const WSAEWOULDBLOCK: u32 = 10035;
const WSAENOTSOCK: u32 = 10038;
const WSAENOTCONN: u32 = 10057;
const WSAEADDRINUSE: u32 = 10048;
const WSAEADDRNOTAVAIL: u32 = 10049;
const WSAENETDOWN: u32 = 10050;
const WSAECONNREFUSED: u32 = 10061;

const TITLE_ID: u32 = 0x4156_0817;

// The title's default net_port is 1000, and ports below 1024 need privileges this process
// lacks. Everything it sends stays on the loopback, so shifting the whole privileged
// range keeps it self-consistent.
const PRIVILEGED_PORT_BASE: u16 = 1024;
const PORT_SHIFT: u16 = 30000;

fn shift_port(port: u16) -> u16 {
    if port == 0 || port >= PRIVILEGED_PORT_BASE { port } else { port + PORT_SHIFT }
}

fn unshift_port(port: u16) -> u16 {
    if (PORT_SHIFT..PORT_SHIFT + PRIVILEGED_PORT_BASE).contains(&port) { port - PORT_SHIFT } else { port }
}

// A title socket. The title only ever hands back what it was given, so a small table
// keyed by a synthetic descriptor is enough.
enum Host {
    Datagram(UdpSocket),
    Stream(TcpStream),
}

impl Host {
    fn set_nonblocking(&self, non_blocking: bool) {
        let _ = match self {
            Host::Datagram(socket) => socket.set_nonblocking(non_blocking),
            Host::Stream(stream) => stream.set_nonblocking(non_blocking),
        };
    }
}

struct Socket {
    stream: bool,
    host: Option<Arc<Host>>,
    non_blocking: bool,
    /// the title's, as it bound it; 0 until then
    port: u16,
}

struct Sockets {
    table: BTreeMap<u32, Socket>,
    next: u32,
}

static SOCKETS: Mutex<Sockets> = Mutex::new(Sockets { table: BTreeMap::new(), next: 1 });

fn sockets() -> std::sync::MutexGuard<'static, Sockets> {
    SOCKETS.lock().unwrap_or_else(PoisonError::into_inner)
}

thread_local! {
    // WSAGetLastError is per thread.
    static LAST_ERROR: Cell<u32> = const { Cell::new(0) };
    // Through a buffer of our own, as file reads are: the kernel fails a receive into a
    // page the GPU's shadow has made read-only.
    static BOUNCE: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

// What Winsock would have said: the host's own number on Windows, where the sockets are
// Winsock's.
fn host_error(error: &io::Error) -> u32 {
    #[cfg(windows)]
    if let Some(code) = error.raw_os_error() {
        return code as u32;
    }
    match error.kind() {
        io::ErrorKind::WouldBlock => WSAEWOULDBLOCK,
        io::ErrorKind::PermissionDenied => WSAEACCES,
        io::ErrorKind::AddrInUse => WSAEADDRINUSE,
        io::ErrorKind::AddrNotAvailable => WSAEADDRNOTAVAIL,
        io::ErrorKind::ConnectionRefused => WSAECONNREFUSED,
        io::ErrorKind::InvalidInput => WSAEINVAL,
        // Winsock's for a stream that is not connected; Linux's ENOTCONN is the C++'s default.
        io::ErrorKind::NotConnected if cfg!(windows) => WSAENOTCONN,
        _ => WSAEINTR,
    }
}

fn fail(ctx: &mut crate::guest::PPCContext, error: u32) {
    LAST_ERROR.set(error);
    ctx.r3.u64 = u64::from(u32::MAX);
}

// sockaddr_in. The guest is big-endian, so its sin_port and sin_addr are in network
// order already, which is the order a word is read in: the port, shifted, and the
// address.
fn read_sock_addr(memory: Memory, address: u32) -> Option<(u16, u32)> {
    (address != 0).then(|| (shift_port(memory.u16_at(address.wrapping_add(2))), memory.u32_at(address.wrapping_add(4))))
}

fn write_sock_addr(memory: Memory, address: u32, port: u16, ina: u32) {
    if address == 0 {
        return;
    }
    memory.store_u16(address, 2); // AF_INET
    memory.store_u16(address.wrapping_add(2), unshift_port(port));
    memory.store_u32(address.wrapping_add(4), ina);
    memory.zero(address.wrapping_add(8), 8);
}

// The host socket a title socket stands on, made now if it has none: bound to any port,
// as the host would bind it.
fn host_of(handle: u32) -> Option<io::Result<Arc<Host>>> {
    let mut held = sockets();
    let socket = held.table.get_mut(&handle)?;
    if let Some(host) = &socket.host {
        return Some(Ok(host.clone()));
    }
    if socket.stream {
        return Some(Err(io::Error::from(io::ErrorKind::NotConnected)));
    }
    Some(UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).map(|made| {
        let host = Arc::new(Host::Datagram(made));
        host.set_nonblocking(socket.non_blocking);
        socket.host = Some(host.clone());
        host
    }))
}

// ---- addresses --------------------------------------------------------------------

// XNADDR { inaOnline, ina, wPortOnline, abEnet[6], abOnline[20] }, 36 bytes.
pub const XNADDR_SIZE: usize = 36;
pub type XnAddr = [u8; XNADDR_SIZE];
const INA: usize = 4;
const ENET: usize = 10;
const ONLINE: usize = 16;

const LOOPBACK: u32 = 0x7F00_0001;
const BROADCAST: u32 = 0xFFFF_FFFF;

// A peer's id, in the part of the XNADDR the console fills in from Xbox Live, behind a
// tag so an address that did not come from here is refused.
const PEER_TAG: [u8; 4] = *b"MW2O";

/// Names `id` in an XNADDR.
pub fn set_peer(address: &mut XnAddr, id: u64) {
    address[ONLINE..ONLINE + 4].copy_from_slice(&PEER_TAG);
    address[ONLINE + 4..ONLINE + 12].copy_from_slice(&id.to_be_bytes());
    // The title tells machines apart by their Ethernet address too.
    address[ENET..ENET + 6].copy_from_slice(&id.to_be_bytes()[2..]);
    address[ENET] = (address[ENET] & 0xFE) | 0x02; // locally administered, unicast
}

/// The peer an XNADDR names, if it came from here.
pub fn peer_of(address: &XnAddr) -> Option<u64> {
    (address[ONLINE..ONLINE + 4] == PEER_TAG)
        .then(|| u64::from_be_bytes(address[ONLINE + 4..ONLINE + 12].try_into().expect("eight bytes")))
}

/// A resolved local address with no online component. Returning "pending" instead
/// makes the title poll for one for ever.
pub fn title_xn_addr() -> XnAddr {
    let mut address = [0; XNADDR_SIZE];
    address[INA..INA + 4].copy_from_slice(&LOOPBACK.to_be_bytes());
    address[ENET..ENET + 6].copy_from_slice(&[0x00, 0x22, 0x48, 0x01, 0x02, 0x03]);
    if let Some(service) = online::get() {
        set_peer(&mut address, service.local_id());
    }
    address
}

fn xn_addr_at(memory: Memory, address: u32) -> XnAddr {
    let mut out = [0; XNADDR_SIZE];
    memory.load_bytes(address, &mut out);
    out
}

// Every peer the title has been given an address for, both ways, the session key it
// named with it, and the session this machine is in.
struct Peers {
    address: BTreeMap<u64, u32>,
    peer: BTreeMap<u32, u64>,
    session_id: BTreeMap<u32, [u8; 8]>,
    next: u32,
    own_session: [u8; 8],
}

static PEERS: Mutex<Peers> = Mutex::new(Peers {
    address: BTreeMap::new(),
    peer: BTreeMap::new(),
    session_id: BTreeMap::new(),
    next: 1,
    own_session: [0; 8],
});

fn peers() -> std::sync::MutexGuard<'static, Peers> {
    PEERS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The session this machine is in, named by its XNKID.
pub fn note_session_id(id: [u8; 8]) {
    peers().own_session = id;
}

fn stand_in(id: u64) -> u32 {
    let mut held = peers();
    if let Some(&address) = held.address.get(&id) {
        return address;
    }
    let address = 0x0A00_0000 | (held.next & 0x00FF_FFFF);
    held.next = held.next.wrapping_add(1);
    held.address.insert(id, address);
    held.peer.insert(address, id);
    address
}

fn peer_at(address: u32) -> Option<u64> {
    peers().peer.get(&address).copied()
}

// MW2_TRACE_ONLINE: every datagram through the service, with the start of its text --
// the title's out-of-band packets begin FF FF FF FF and a word.
fn trace(what: &std::ffi::CStr, peer: u64, from: u16, to: u16, bytes: &[u8]) {
    static TRACED: LazyLock<bool> = LazyLock::new(|| cfg!(feature = "diagnostics") && env::flag(c"MW2_TRACE_ONLINE"));
    if !*TRACED {
        return;
    }
    let text: Vec<u8> = bytes.iter().take(400).map(|&c| if (32..127).contains(&c) { c } else { b'.' }).collect();
    log_info!(
        "online: %s %012llx %u->%u %llu bytes '%s'",
        what,
        peer,
        u32::from(from),
        u32::from(to),
        bytes.len() as u64,
        c_string(text.as_slice()).as_c_str()
    );
}

// What the title sends to itself while a service carries its sockets.
static TO_SELF: Mutex<BTreeMap<u16, VecDeque<Datagram>>> = Mutex::new(BTreeMap::new());
const SELF_QUEUED: usize = 512;

// ---- XNet -------------------------------------------------------------------------

// Signed in to Live, the address says so: the title's party refuses to add a player --
// itself included -- whose address has no online part. NetDll entry points take the
// interface as r3.
kernel_import!(NetDll_XNetGetTitleXnAddr, |ctx, memory| {
    let out = ctx.r4.as_u32();
    if out != 0 {
        memory.store_bytes(out, &title_xn_addr());
    }
    let live = if online::live() { XNET_GET_XNADDR_GATEWAY | XNET_GET_XNADDR_DNS | XNET_GET_XNADDR_ONLINE } else { 0 };
    ctx.r3.u64 = u64::from(XNET_GET_XNADDR_STATIC | XNET_GET_XNADDR_ETHERNET | live);
});

kernel_import!(XNetLogonGetTitleID, |ctx, _memory| {
    ctx.r3.u64 = u64::from(TITLE_ID);
});

kernel_import!(NetDll_XNetStartup, |ctx, _memory| {
    ctx.r3.u64 = 0;
});

kernel_import!(NetDll_WSACleanup, |ctx, _memory| {
    ctx.r3.u64 = 0;
});

kernel_import!(NetDll_WSAGetLastError, |ctx, _memory| {
    ctx.r3.u64 = u64::from(LAST_ERROR.get());
});

// The title reads the version words straight back out, so leaving the block untouched
// makes it report the nonsense it found there and restart itself. The host's Winsock,
// on Windows, the standard library starts with its first socket.
kernel_import!(NetDll_WSAStartup, |ctx, memory| {
    let requested = ctx.r4.as_u32() as u16;
    let data = ctx.r5.as_u32();
    if data != 0 {
        memory.zero(data, 400);
        memory.store_u16(data, if requested != 0 { requested } else { 0x0202 }); // wVersion
        memory.store_u16(data.wrapping_add(2), 0x0202); // wHighVersion
        memory.store_bytes(data.wrapping_add(4), b"mw2recomp sockets");
        memory.store_bytes(data.wrapping_add(261), b"Running");
        memory.store_u16(data.wrapping_add(390), 64); // iMaxSockets
        memory.store_u16(data.wrapping_add(392), 1264); // iMaxUdpDg
    }
    ctx.r3.u64 = 0;
});

// 0 means "no link", which the campaign handles: it never needs one. The multiplayer
// refuses to start any match without one -- "You must have an active network connection
// to play online or systemlink matches" -- and a system-link match is the only way it has
// of reaching a map, so that build reports a 100 Mb/s full-duplex link. MW2_NET_LINK=0|1
// overrides either.
fn ethernet_link_up() -> bool {
    static UP: LazyLock<bool> = LazyLock::new(|| {
        if env::text(c"MW2_NET_LINK").is_some() { env::flag(c"MW2_NET_LINK") } else { cfg!(mw2_title_mp) }
    });
    *UP
}

const XNET_ETHERNET_LINK_ACTIVE: u32 = 1;
const XNET_ETHERNET_LINK_100MBPS: u32 = 2;
const XNET_ETHERNET_LINK_FULL_DUPLEX: u32 = 8;

kernel_import!(NetDll_XNetGetEthernetLinkStatus, |ctx, _memory| {
    let up = XNET_ETHERNET_LINK_ACTIVE | XNET_ETHERNET_LINK_100MBPS | XNET_ETHERNET_LINK_FULL_DUPLEX;
    ctx.r3.u64 = u64::from(if ethernet_link_up() { up } else { 0 });
});

kernel_import!(NetDll_XNetRandom, |ctx, memory| {
    let (buffer, length) = (ctx.r4.as_u32(), ctx.r5.as_u32());
    if buffer != 0 {
        let mut bytes = [0; 256];
        for at in (0..length).step_by(bytes.len()) {
            let chunk = &mut bytes[..(length - at).min(256) as usize];
            random_bytes(chunk);
            memory.store_bytes(buffer.wrapping_add(at), chunk);
        }
    }
    ctx.r3.u64 = 0;
});

// (xnet, XNADDR*, XNKID*, IN_ADDR* out). How the title reaches a host it found: the
// address names the peer, and this machine's own is the loopback, which the host's
// sockets carry.
kernel_import!(NetDll_XNetXnAddrToInAddr, |ctx, memory| {
    let (address, kid, out) = (ctx.r4.as_u32(), ctx.r5.as_u32(), ctx.r6.as_u32());
    let Some(service) = online::get() else {
        ctx.r3.u64 = u64::from(WSAENETDOWN);
        return;
    };
    let id = if address != 0 && out != 0 { peer_of(&xn_addr_at(memory, address)) } else { None };
    let Some(id) = id else {
        ctx.r3.u64 = u64::from(WSAENETDOWN);
        return;
    };
    let ina = if id == service.local_id() { LOOPBACK } else { stand_in(id) };
    if kid != 0 {
        let mut key = [0; 8];
        memory.load_bytes(kid, &mut key);
        peers().session_id.insert(ina, key);
    }
    memory.store_u32(out, ina);
    ctx.r3.u64 = 0;
});

// (xnet, IN_ADDR, XNADDR* out, XNKID* out). Who a datagram came from.
kernel_import!(NetDll_XNetInAddrToXnAddr, |ctx, memory| {
    let (ina, out, kid) = (ctx.r4.as_u32(), ctx.r5.as_u32(), ctx.r6.as_u32());
    let own = ina == LOOPBACK;
    let peer = if own { None } else { peer_at(ina) };
    if online::get().is_none() || out == 0 || !(own || peer.is_some()) {
        ctx.r3.u64 = u64::from(WSAENETDOWN);
        return;
    }
    let mut address = title_xn_addr();
    if let Some(id) = peer {
        set_peer(&mut address, id);
        address[INA..INA + 4].copy_from_slice(&ina.to_be_bytes());
    }
    memory.store_bytes(out, &address);
    if kid != 0 {
        let held = peers();
        let key = held.session_id.get(&ina).copied().unwrap_or(held.own_session);
        drop(held);
        memory.store_bytes(kid, &key);
    }
    ctx.r3.u64 = 0;
});

// The service is already a connection to everyone it can name.
kernel_import!(NetDll_XNetConnect, |ctx, _memory| {
    ctx.r3.u64 = if online::get().is_some() { 0 } else { u64::from(WSAENETDOWN) };
});

kernel_import!(NetDll_XNetGetConnectStatus, |ctx, _memory| {
    ctx.r3.u64 = 2; // XNET_CONNECT_STATUS_CONNECTED
});

kernel_import!(NetDll_XNetUnregisterInAddr, |ctx, _memory| {
    ctx.r3.u64 = 0;
});

// Xbox Live's own servers, which nothing here stands in for.
kernel_import!(NetDll_XNetServerToInAddr, |ctx, _memory| {
    ctx.r3.u64 = u64::from(WSAENETDOWN);
});

kernel_import!(NetDll_XNetXnAddrToMachineId, |ctx, _memory| {
    ctx.r3.u64 = u64::from(WSAENETDOWN);
});

// XNQOS { cxnqos, cxnqosPending, XNQOSINFO[] }; XNQOSINFO { bFlags, bReserved,
// cProbesXmit, cProbesRecv, cbData, pbData, wRttMin, wRttMed, dwUpBps, dwDnBps }, 24 bytes.
const QOS_INFO_SIZE: u32 = 24;
const QOS_COMPLETE: u8 = 0x01;
const QOS_TARGET_CONTACTED: u8 = 0x02;
const QOS_TARGET_DISABLED: u8 = 0x04;
const QOS_PARTIAL_COMPLETE: u8 = 0x10;
const WIDE_LINE: u32 = 100_000_000; // bits a second

fn write_qos_probe(memory: Memory, info: u32, flags: u8, milliseconds: u16) {
    memory.store_u8(info, flags);
    memory.store_u16(info.wrapping_add(2), 1); // probes sent
    memory.store_u16(info.wrapping_add(4), 1); // and answered
    memory.store_u16(info.wrapping_add(12), milliseconds);
    memory.store_u16(info.wrapping_add(14), milliseconds);
    memory.store_u32(info.wrapping_add(16), WIDE_LINE); // up
    memory.store_u32(info.wrapping_add(20), WIDE_LINE); // and down
}

// (xnet, flags, event, XNQOS** out). Signed in to Live, the title measures its line to
// Live's own service once it is told the connection is up; there is no such service, and
// nothing here stands between the players, so the answer is one probe, complete, with a
// short round trip and a wide line.
kernel_import!(NetDll_XNetQosServiceLookup, |ctx, memory| {
    let (event, out) = (ctx.r5.as_u32(), ctx.r6.as_u32());
    if !online::live() || out == 0 {
        ctx.r3.u64 = u64::from(WSAENETDOWN);
        return;
    }
    let size = 8 + QOS_INFO_SIZE;
    let qos = allocate_guest(size, 16);
    if qos == 0 {
        ctx.r3.u64 = u64::from(WSAENETDOWN);
        return;
    }
    memory.zero(qos, size);
    memory.store_u32(qos, 1); // cxnqos
    write_qos_probe(memory, qos.wrapping_add(8), QOS_COMPLETE | QOS_TARGET_CONTACTED | QOS_PARTIAL_COMPLETE, 10);
    memory.store_u32(out, qos);
    if event != 0 {
        set_event_handle(event);
    }
    ctx.r3.u64 = 0;
});

// The round trip each session a search found was listed with, by its XNKID.
static SESSION_LAGS: Mutex<BTreeMap<[u8; 8], u32>> = Mutex::new(BTreeMap::new());

fn session_lags() -> std::sync::MutexGuard<'static, BTreeMap<[u8; 8], u32>> {
    SESSION_LAGS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A new search forgets what the last one found.
pub fn forget_session_lags() {
    session_lags().clear();
}

pub fn note_session_lag(id: [u8; 8], milliseconds: u32) {
    session_lags().insert(id, milliseconds);
}

// (xnet, count, XNADDR*[], XNKID*[], XNKEY*[], services, ids, service ids, probes, bits
// a second, flags, event, XNQOS** out), the last five on the stack. The title probes
// what a search found, to rank the hosts by their round trip and drop those over its
// limit. It reads the flags, the round trips and the speed up, none of what a host
// offers with XNetQosListen. A search's answer already holds the round trip, so every
// probe here is complete at once; a host no search found reads as one that does not
// answer probes. The line is not what limits a match on a relay, so it reads as wide, as
// in XNetQosServiceLookup.
kernel_import!(NetDll_XNetQosLookup, |ctx, memory| {
    let (count, ids) = (ctx.r4.as_u32(), ctx.r6.as_u32());
    let event = stack_argument(ctx, memory, 11);
    let out = stack_argument(ctx, memory, 12);
    if ids == 0 || out == 0 || count == 0 || count > 64 {
        ctx.r3.u64 = u64::from(WSAENETDOWN);
        return;
    }
    let size = 8 + QOS_INFO_SIZE * count;
    let qos = allocate_guest(size, 16);
    if qos == 0 {
        ctx.r3.u64 = u64::from(WSAENETDOWN);
        return;
    }
    memory.zero(qos, size);
    memory.store_u32(qos, count); // cxnqos
    {
        let lags = session_lags();
        for i in 0..count {
            let info = qos.wrapping_add(8 + QOS_INFO_SIZE * i);
            let kid = memory.u32_at(ids.wrapping_add(4 * i));
            let mut id = [0; 8];
            if kid != 0 {
                memory.load_bytes(kid, &mut id);
            }
            match lags.get(&id) {
                None => memory.store_u8(info, QOS_COMPLETE | QOS_TARGET_DISABLED),
                Some(&lag) => {
                    write_qos_probe(memory, info, QOS_COMPLETE | QOS_TARGET_CONTACTED, lag.min(0xFFFF) as u16)
                }
            }
        }
    }
    memory.store_u32(out, qos);
    if event != 0 {
        set_event_handle(event);
    }
    ctx.r3.u64 = 0;
});

// (xnet, XNQOS*): what a lookup handed out.
kernel_import!(NetDll_XNetQosRelease, |ctx, _memory| {
    let qos = ctx.r4.as_u32();
    if qos != 0 {
        free_guest(qos);
    }
    ctx.r3.u64 = 0;
});

// A host offering its session's details to machines that probe it before joining.
// Nothing probes: invitations and the party's own join do not.
kernel_import!(NetDll_XNetQosListen, |ctx, _memory| {
    ctx.r3.u64 = 0;
});

// ---- Winsock ----------------------------------------------------------------------

// Even offline single-player makes a local UDP port and talks to itself over it, and
// refuses to start if that fails. (af, type, protocol) after the interface: type 1 is
// SOCK_STREAM.
kernel_import!(NetDll_socket, |ctx, _memory| {
    let stream = ctx.r5.as_u32() == 1;
    let mut held = sockets();
    let handle = held.next;
    held.next = held.next.wrapping_add(1);
    held.table.insert(handle, Socket { stream, host: None, non_blocking: false, port: 0 });
    ctx.r3.u64 = u64::from(handle);
});

kernel_import!(NetDll_closesocket, |ctx, _memory| {
    let closed = sockets().table.remove(&ctx.r4.as_u32());
    drop(closed);
    ctx.r3.u64 = 0;
});

kernel_import!(NetDll_bind, |ctx, memory| {
    let handle = ctx.r4.as_u32();
    let Some((port, ina)) = read_sock_addr(memory, ctx.r5.as_u32()) else { return fail(ctx, WSAEINVAL) };
    let mut held = sockets();
    let Some(socket) = held.table.get_mut(&handle) else { return fail(ctx, WSAEINVAL) };
    if socket.host.is_some() {
        return fail(ctx, WSAEINVAL);
    }
    // A stream is the socket for Xbox Live's servers, which only ever connects.
    if online::get().is_none() && !socket.stream {
        match UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::from(ina), port)) {
            Ok(made) => {
                let host = Host::Datagram(made);
                host.set_nonblocking(socket.non_blocking);
                socket.host = Some(Arc::new(host));
            }
            Err(error) => {
                let error = host_error(&error);
                LAST_ERROR.set(error);
                // Another copy on this machine has the port, which no console ever finds:
                // the title takes a failed bind for a fatal error and starts itself again,
                // into the same failure. The socket stays unbound instead, and this copy
                // plays without a network: it hears nothing sent to the port. Two copies
                // on one machine need an online service (lan), which gives each its own.
                if error != WSAEADDRINUSE {
                    return fail(ctx, error);
                }
                log_warn!(
                    "net: port %u is taken by another copy of the game on this machine; this copy has no network",
                    u32::from(unshift_port(port))
                );
            }
        }
    }
    socket.port = unshift_port(port);
    ctx.r3.u64 = 0;
});

kernel_import!(NetDll_connect, |ctx, memory| {
    let handle = ctx.r4.as_u32();
    let (Some(stream), Some((port, ina))) =
        (sockets().table.get(&handle).map(|socket| socket.stream), read_sock_addr(memory, ctx.r5.as_u32()))
    else {
        return fail(ctx, WSAEINVAL);
    };
    // Only the socket for Xbox Live's servers connects, and there are none.
    if online::get().is_some() {
        return fail(ctx, WSAECONNREFUSED);
    }
    let to = SocketAddrV4::new(Ipv4Addr::from(ina), port);
    let non_blocking = sockets().table.get(&handle).is_some_and(|socket| socket.non_blocking);
    let connected = if stream && non_blocking {
        // A socket that does not wait does not wait to connect either: the host answers
        // at once that it is still connecting, which the C++'s Linux sockets report as
        // their default error. The standard library cannot leave a connection pending.
        Err(io::Error::from(if cfg!(windows) { io::ErrorKind::WouldBlock } else { io::ErrorKind::Other }))
    } else if stream {
        TcpStream::connect(to).map(|made| {
            let mut held = sockets();
            if let Some(socket) = held.table.get_mut(&handle) {
                let host = Host::Stream(made);
                host.set_nonblocking(socket.non_blocking);
                socket.host = Some(Arc::new(host));
            }
        })
    } else {
        match host_of(handle) {
            Some(Ok(host)) => match &*host {
                Host::Datagram(socket) => socket.connect(to),
                Host::Stream(_) => Ok(()),
            },
            Some(Err(error)) => Err(error),
            None => return fail(ctx, WSAEINVAL),
        }
    };
    match connected {
        Ok(()) => ctx.r3.u64 = 0,
        Err(error) => fail(ctx, host_error(&error)),
    }
});

kernel_import!(NetDll_ioctlsocket, |ctx, memory| {
    // Only FIONBIO is used.
    const FIONBIO: u32 = 0x8004_667E;
    let (handle, command, argument) = (ctx.r4.as_u32(), ctx.r5.as_u32(), ctx.r6.as_u32());
    let mut held = sockets();
    let Some(socket) = held.table.get_mut(&handle) else { return fail(ctx, WSAENOTSOCK) };
    if command == FIONBIO {
        socket.non_blocking = argument != 0 && memory.u32_at(argument) != 0;
        if let Some(host) = &socket.host {
            host.set_nonblocking(socket.non_blocking);
        }
    }
    ctx.r3.u64 = 0;
});

// The title sets broadcast and buffer sizes; accepting them is enough.
kernel_import!(NetDll_setsockopt, |ctx, _memory| {
    ctx.r3.u64 = 0;
});

// (socket, buffer, length, flags, to, toLength)
kernel_import!(NetDll_sendto, |ctx, memory| {
    let handle = ctx.r4.as_u32();
    let (buffer, length) = (ctx.r5.as_u32(), ctx.r6.as_u32());
    let (Some(from_port), Some((port, to))) =
        (sockets().table.get(&handle).map(|socket| socket.port), read_sock_addr(memory, ctx.r8.as_u32()))
    else {
        return fail(ctx, WSAEINVAL);
    };
    let bytes = memory.bytes(buffer, length as usize);

    if let Some(service) = online::get() {
        let port = unshift_port(port);
        // Like UDP, a datagram that could not go is lost, not an error.
        if to == BROADCAST {
            service.broadcast(from_port, port, &bytes);
            trace(c"broadcast", 0, from_port, port, &bytes);
        } else if let Some(peer) = peer_at(to) {
            service.send(peer, from_port, port, &bytes);
            trace(c"send", peer, from_port, port, &bytes);
        } else if to == LOOPBACK {
            let datagram = Datagram { from: 0, from_port, to_port: port, bytes };
            let mut waiting = TO_SELF.lock().unwrap_or_else(PoisonError::into_inner);
            let queue = waiting.entry(port).or_default();
            if queue.len() < SELF_QUEUED {
                queue.push_back(datagram);
            }
        } else {
            trace(c"dropped, no such peer", 0, from_port, port, &bytes);
        }
        ctx.r3.u64 = u64::from(length);
        return;
    }

    let host = match host_of(handle) {
        Some(Ok(host)) => host,
        Some(Err(error)) => return fail(ctx, host_error(&error)),
        None => return fail(ctx, WSAEINVAL),
    };
    let sent = match &*host {
        Host::Datagram(socket) => socket.send_to(&bytes, SocketAddrV4::new(Ipv4Addr::from(to), port)),
        Host::Stream(stream) => (&*stream).write(&bytes),
    };
    match sent {
        Ok(sent) => ctx.r3.u64 = sent as u64,
        Err(error) => fail(ctx, host_error(&error)),
    }
});

// (socket, buffer, length, flags, from, fromLength)
kernel_import!(NetDll_recvfrom, |ctx, memory| {
    let handle = ctx.r4.as_u32();
    let (buffer, length, from) = (ctx.r5.as_u32(), ctx.r6.as_u32(), ctx.r8.as_u32());
    let Some(port) = sockets().table.get(&handle).map(|socket| socket.port) else { return fail(ctx, WSAENOTSOCK) };

    if let Some(service) = online::get() {
        let waiting =
            TO_SELF.lock().unwrap_or_else(PoisonError::into_inner).get_mut(&port).and_then(VecDeque::pop_front);
        let (datagram, own) = match waiting {
            Some(datagram) => (datagram, true),
            None => {
                let mut datagram = Datagram::default();
                if !(port != 0 && service.receive(port, &mut datagram)) {
                    return fail(ctx, WSAEWOULDBLOCK); // the title's sockets never block
                }
                (datagram, false)
            }
        };
        if !own {
            trace(c"receive", datagram.from, datagram.from_port, port, &datagram.bytes);
        }
        let got = datagram.bytes.len().min(length as usize);
        memory.store_bytes(buffer, &datagram.bytes[..got]);
        if from != 0 {
            write_sock_addr(memory, from, datagram.from_port, if own { LOOPBACK } else { stand_in(datagram.from) });
        }
        ctx.r3.u64 = got as u64;
        return;
    }

    // A receive does not bind a socket, as a send does: one not bound yet hears nothing,
    // and can still be bound. Winsock refuses the call.
    if sockets().table.get(&handle).is_some_and(|socket| socket.host.is_none() && !socket.stream) {
        return fail(ctx, if cfg!(windows) { WSAEINVAL } else { WSAEWOULDBLOCK });
    }
    let host = match host_of(handle) {
        Some(Ok(host)) => host,
        Some(Err(error)) => return fail(ctx, host_error(&error)),
        None => return fail(ctx, WSAENOTSOCK),
    };
    let received = BOUNCE.with_borrow_mut(|bounce| {
        if bounce.len() < length as usize {
            bounce.resize(length as usize, 0);
        }
        let into = &mut bounce[..length as usize];
        let got = match &*host {
            Host::Datagram(socket) => socket.recv_from(into),
            Host::Stream(stream) => (&*stream).read(into).and_then(|got| Ok((got, stream.peer_addr()?))),
        };
        got.map(|(got, sender)| {
            memory.store_bytes(buffer, &into[..got]);
            (got, sender)
        })
    });
    match received {
        Ok((got, sender)) => {
            if let SocketAddr::V4(sender) = sender {
                write_sock_addr(memory, from, sender.port(), u32::from(*sender.ip()));
            }
            ctx.r3.u64 = got as u64;
        }
        Err(error) => fail(ctx, host_error(&error)),
    }
});

kernel_import!(NetDll_inet_addr, |ctx, memory| {
    let text = ctx.r3.as_u32();
    let text = if text == 0 { Vec::new() } else { memory.string(text, 4096) };
    let parsed = std::str::from_utf8(&text).ok().and_then(|text| text.parse().ok());
    // The guest wants network byte order as a big-endian word: the address's own.
    ctx.r3.u64 = u64::from(parsed.map_or(u32::MAX, |address: Ipv4Addr| u32::from(address)));
});
