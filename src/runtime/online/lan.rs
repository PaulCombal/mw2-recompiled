// Players on the same network, over plain UDP: the smallest real transport, and the
// one that tests the kernel side without an account anywhere. Two copies on one
// machine find each other too, which the title's own sockets cannot do -- both would
// want port 1000.
//
// Each copy sends from a port of its own, and a peer is that port's address: the IPv4
// address in the top bits, the port in the low 16. Broadcasts go to a port every copy
// shares (MW2_LAN_PORT, 3074 by default), which the kernel delivers to all of them;
// each copy's own port is the first free one after it, and how far after is the
// copy's slot.

use super::players::{self, Transport};
use super::{Service, udp};
use crate::env;
use crate::log::{c_string, log_error, log_info};
use std::ffi::CStr;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};

fn id_of(address: SocketAddrV4) -> u64 {
    (u64::from(u32::from(*address.ip())) << 16) | u64::from(address.port())
}

fn address_of(id: u64) -> SocketAddrV4 {
    SocketAddrV4::new(Ipv4Addr::from((id >> 16) as u32), id as u16)
}

/// The address other copies reach this one at: the first connection that is up and
/// not the loopback, or the loopback when there is none. The log names them all: with
/// more than one, the first may not be the one the other players are on.
fn own_address() -> Ipv4Addr {
    let adapters = udp::adapters();
    let list = adapters
        .iter()
        .map(|(name, address)| {
            let mut entry = name.clone();
            entry.extend_from_slice(format!(" {address}").as_bytes());
            entry
        })
        .collect::<Vec<_>>()
        .join(&b", "[..]);
    match adapters.first() {
        None => {
            log_info!("online: no network connection is up; lan stays on this machine");
            Ipv4Addr::LOCALHOST
        }
        Some(&(_, first)) => {
            log_info!("online: network connections: %s; announcing the first", c_string(list).as_c_str());
            first
        }
    }
}

struct Lan {
    own: Option<UdpSocket>,
    shared: Option<UdpSocket>,
    shared_port: u16,
    id: u64,
    slot: u32,
}

pub fn make() -> Box<dyn Service> {
    players::make(Box::new(Lan { own: None, shared: None, shared_port: 0, id: 0, slot: 0 }))
}

impl Transport for Lan {
    fn open(&mut self) -> bool {
        if !udp::startup() {
            log_error!("online: no Winsock");
            return false;
        }
        self.shared_port = env::number(c"MW2_LAN_PORT", 3074) as u16;
        let own = udp::open_own_socket(self.shared_port);
        let shared = udp::open_socket(self.shared_port, true);
        let (Some((own, slot)), Some(shared)) = (own, shared) else {
            log_error!("online: lan could not open its sockets (shared port %u)", u32::from(self.shared_port));
            return false;
        };
        let port = match own.local_addr() {
            Ok(std::net::SocketAddr::V4(bound)) => bound.port(),
            _ => 0,
        };
        let bound = SocketAddrV4::new(own_address(), port);
        self.id = id_of(bound);
        log_info!(
            "online: lan at %s:%u, broadcasts on %u",
            c_string(bound.ip().to_string()).as_c_str(),
            u32::from(port),
            u32::from(self.shared_port)
        );
        self.own = Some(own);
        self.shared = Some(shared);
        self.slot = slot;
        true
    }

    fn name(&self) -> &'static CStr {
        c"lan"
    }

    fn local_id(&self) -> u64 {
        self.id
    }

    fn slot(&self) -> u32 {
        self.slot
    }

    fn send_to(&self, peer: u64, bytes: &[u8]) -> bool {
        self.own.as_ref().is_some_and(|own| own.send_to(bytes, address_of(peer)).is_ok())
    }

    fn send_all(&self, bytes: &[u8]) -> bool {
        let everyone = SocketAddrV4::new(Ipv4Addr::BROADCAST, self.shared_port);
        self.own.as_ref().is_some_and(|own| own.send_to(bytes, everyone).is_ok())
    }

    fn next(&self, bytes: &mut [u8]) -> Option<(usize, u64)> {
        [&self.own, &self.shared].into_iter().flatten().find_map(|socket| match socket.recv_from(bytes) {
            Ok((got, std::net::SocketAddr::V4(from))) => Some((got, id_of(from))),
            _ => None,
        })
    }
}
