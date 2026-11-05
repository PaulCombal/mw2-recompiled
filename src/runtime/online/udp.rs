// The host's UDP, for the transports that use it (lan.rs, server.rs): what Rust's
// own sockets cannot do -- share a port with other copies, which must be said before
// the socket is bound -- the network connections that are up, and a host's address
// as the system's resolver gives it. Rust's sockets do the rest.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};

const AF_INET: c_int = 2;
const SOCK_DGRAM: c_int = 2;

// sockaddr_in, alike on both systems.
#[repr(C)]
#[derive(Clone, Copy)]
struct SockaddrIn {
    family: u16,
    port: [u8; 2],
    address: [u8; 4],
    zero: [u8; 8],
}

impl SockaddrIn {
    fn of(address: SocketAddrV4) -> Self {
        SockaddrIn {
            family: AF_INET as u16,
            port: address.port().to_be_bytes(),
            address: address.ip().octets(),
            zero: [0; 8],
        }
    }

    fn address(&self) -> SocketAddrV4 {
        SocketAddrV4::new(Ipv4Addr::from(self.address), u16::from_be_bytes(self.port))
    }
}

#[cfg(not(windows))]
mod system {
    use super::*;
    use std::os::fd::FromRawFd;

    pub const SOL_SOCKET: c_int = 1;
    pub const SO_REUSEADDR: c_int = 2;
    pub const SO_BROADCAST: c_int = 6;
    const IFF_UP: u32 = 0x1;
    const IFF_LOOPBACK: u32 = 0x8;

    pub type Raw = c_int;
    pub const NO_SOCKET: Raw = -1;

    #[repr(C)]
    struct Ifaddrs {
        next: *mut Ifaddrs,
        name: *const c_char,
        flags: u32,
        address: *const SockaddrIn,
        netmask: *const c_void,
        broadcast: *const c_void,
        data: *mut c_void,
    }

    #[repr(C)]
    pub struct Addrinfo {
        pub flags: c_int,
        pub family: c_int,
        pub socktype: c_int,
        pub protocol: c_int,
        pub address_size: u32,
        pub address: *const SockaddrIn,
        pub canonical: *const c_char,
        pub next: *mut Addrinfo,
    }

    unsafe extern "C" {
        pub fn socket(family: c_int, kind: c_int, protocol: c_int) -> Raw;
        pub fn setsockopt(socket: Raw, level: c_int, name: c_int, value: *const c_void, size: u32) -> c_int;
        pub fn bind(socket: Raw, address: *const SockaddrIn, size: u32) -> c_int;
        pub fn close(socket: Raw) -> c_int;
        fn getifaddrs(list: *mut *mut Ifaddrs) -> c_int;
        fn freeifaddrs(list: *mut Ifaddrs);
        pub fn getaddrinfo(
            host: *const c_char,
            service: *const c_char,
            hints: *const Addrinfo,
            found: *mut *mut Addrinfo,
        ) -> c_int;
        pub fn freeaddrinfo(found: *mut Addrinfo);
    }

    pub fn startup() -> bool {
        true
    }

    pub unsafe fn adopt(raw: Raw) -> UdpSocket {
        unsafe { UdpSocket::from_raw_fd(raw) }
    }

    pub fn hints() -> Addrinfo {
        Addrinfo {
            flags: 0,
            family: AF_INET,
            socktype: SOCK_DGRAM,
            protocol: 0,
            address_size: 0,
            address: std::ptr::null(),
            canonical: std::ptr::null(),
            next: std::ptr::null_mut(),
        }
    }

    /// The network connections that are up, the loopback aside, in the order the
    /// system lists them.
    pub fn adapters() -> Vec<(Vec<u8>, Ipv4Addr)> {
        let mut found = Vec::new();
        let mut list = std::ptr::null_mut();
        if unsafe { getifaddrs(&mut list) } != 0 {
            return found;
        }
        let mut at = list;
        while !at.is_null() {
            let entry = unsafe { &*at };
            at = entry.next;
            if entry.address.is_null() || unsafe { (*entry.address).family } != AF_INET as u16 {
                continue;
            }
            if entry.flags & IFF_UP == 0 || entry.flags & IFF_LOOPBACK != 0 {
                continue;
            }
            let name = unsafe { CStr::from_ptr(entry.name) }.to_bytes().to_vec();
            found.push((name, *unsafe { &*entry.address }.address().ip()));
        }
        unsafe { freeifaddrs(list) };
        found
    }
}

#[cfg(windows)]
mod system {
    use super::*;
    use crate::runtime::windows::{CP_UTF8, WideCharToMultiByte};
    use std::os::windows::io::FromRawSocket;

    pub const SOL_SOCKET: c_int = 0xFFFF;
    pub const SO_REUSEADDR: c_int = 0x0004;
    pub const SO_BROADCAST: c_int = 0x0020;

    pub type Raw = usize;
    pub const NO_SOCKET: Raw = usize::MAX;

    #[repr(C)]
    pub struct Addrinfo {
        pub flags: c_int,
        pub family: c_int,
        pub socktype: c_int,
        pub protocol: c_int,
        pub address_size: usize,
        pub canonical: *const c_char,
        pub address: *const SockaddrIn,
        pub next: *mut Addrinfo,
    }

    // IP_ADAPTER_UNICAST_ADDRESS's start.
    #[repr(C)]
    struct Unicast {
        header: u64,
        next: *const Unicast,
        address: *const SockaddrIn,
        address_size: c_int,
    }

    // IP_ADAPTER_ADDRESSES's start.
    #[repr(C)]
    struct Adapter {
        header: u64,
        next: *const Adapter,
        name: *const c_char,
        unicast: *const Unicast,
        anycast: *const c_void,
        multicast: *const c_void,
        dns: *const c_void,
        suffix: *const u16,
        description: *const u16,
        friendly: *const u16,
        physical: [u8; 8],
        physical_size: u32,
        flags: u32,
        mtu: u32,
        kind: u32,
        status: u32,
    }

    const GAA_FLAG_SKIP_ANYCAST: u32 = 0x2;
    const GAA_FLAG_SKIP_MULTICAST: u32 = 0x4;
    const GAA_FLAG_SKIP_DNS_SERVER: u32 = 0x8;
    const ERROR_BUFFER_OVERFLOW: u32 = 111;
    const IF_OPER_STATUS_UP: u32 = 1;
    const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;

    unsafe extern "system" {
        pub fn socket(family: c_int, kind: c_int, protocol: c_int) -> Raw;
        pub fn setsockopt(socket: Raw, level: c_int, name: c_int, value: *const c_void, size: c_int) -> c_int;
        pub fn bind(socket: Raw, address: *const SockaddrIn, size: c_int) -> c_int;
        #[link_name = "closesocket"]
        pub fn close(socket: Raw) -> c_int;
        fn WSAStartup(version: u16, data: *mut [u8; 512]) -> c_int;
        pub fn getaddrinfo(
            host: *const c_char,
            service: *const c_char,
            hints: *const Addrinfo,
            found: *mut *mut Addrinfo,
        ) -> c_int;
        pub fn freeaddrinfo(found: *mut Addrinfo);
        fn GetAdaptersAddresses(
            family: u32,
            flags: u32,
            reserved: *mut c_void,
            adapters: *mut u8,
            size: *mut u32,
        ) -> u32;
    }

    pub fn startup() -> bool {
        let mut data = [0u8; 512];
        unsafe { WSAStartup(0x0202, &mut data) == 0 }
    }

    pub unsafe fn adopt(raw: Raw) -> UdpSocket {
        unsafe { UdpSocket::from_raw_socket(raw as u64) }
    }

    pub fn hints() -> Addrinfo {
        Addrinfo {
            flags: 0,
            family: AF_INET,
            socktype: SOCK_DGRAM,
            protocol: 0,
            address_size: 0,
            canonical: std::ptr::null(),
            address: std::ptr::null(),
            next: std::ptr::null_mut(),
        }
    }

    /// The network connections that are up, the loopback aside, in the order the
    /// system lists them.
    pub fn adapters() -> Vec<(Vec<u8>, Ipv4Addr)> {
        let mut found = Vec::new();
        let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let mut size = 16 * 1024u32;
        // u64s, for the structures' alignment.
        let mut buffer = vec![0u64; size as usize / 8];
        let mut listed = unsafe {
            GetAdaptersAddresses(AF_INET as u32, flags, std::ptr::null_mut(), buffer.as_mut_ptr().cast(), &mut size)
        };
        if listed == ERROR_BUFFER_OVERFLOW {
            buffer = vec![0u64; (size as usize).div_ceil(8)];
            listed = unsafe {
                GetAdaptersAddresses(AF_INET as u32, flags, std::ptr::null_mut(), buffer.as_mut_ptr().cast(), &mut size)
            };
        }
        if listed != 0 {
            return found;
        }
        let mut at = buffer.as_ptr().cast::<Adapter>();
        while !at.is_null() {
            let adapter = unsafe { &*at };
            at = adapter.next;
            if adapter.status != IF_OPER_STATUS_UP || adapter.kind == IF_TYPE_SOFTWARE_LOOPBACK {
                continue;
            }
            let mut name = [0u8; 128];
            name[0] = b'?';
            if !adapter.friendly.is_null() {
                unsafe {
                    WideCharToMultiByte(
                        CP_UTF8,
                        0,
                        adapter.friendly,
                        -1,
                        name.as_mut_ptr().cast(),
                        name.len() as c_int,
                        std::ptr::null(),
                        std::ptr::null_mut(),
                    )
                };
            }
            name[127] = 0;
            let name = name.split(|&byte| byte == 0).next().unwrap_or_default().to_vec();
            let mut unicast = adapter.unicast;
            while !unicast.is_null() {
                let address = unsafe { &*unicast };
                unicast = address.next;
                if !address.address.is_null() && unsafe { (*address.address).family } == AF_INET as u16 {
                    found.push((name.clone(), *unsafe { &*address.address }.address().ip()));
                }
            }
        }
        found
    }
}

use system::*;
pub use system::{adapters, startup};

/// A non-blocking socket on `port` of every address; broadcasts allowed, and the port
/// shared with other copies when `shared`.
pub fn open_socket(port: u16, shared: bool) -> Option<UdpSocket> {
    let raw = unsafe { socket(AF_INET, SOCK_DGRAM, 0) };
    if raw == NO_SOCKET {
        return None;
    }
    let on: c_int = 1;
    let size = size_of::<c_int>() as _;
    unsafe { setsockopt(raw, SOL_SOCKET, SO_BROADCAST, (&raw const on).cast(), size) };
    if shared {
        unsafe { setsockopt(raw, SOL_SOCKET, SO_REUSEADDR, (&raw const on).cast(), size) };
    }
    let address = SockaddrIn::of(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port));
    if unsafe { bind(raw, &address, size_of::<SockaddrIn>() as _) } != 0 {
        unsafe { close(raw) };
        return None;
    }
    let socket = unsafe { adopt(raw) };
    socket.set_nonblocking(true).ok()?;
    Some(socket)
}

/// A port of a copy's own, the first free one above `base`, and how far above it is:
/// 0 for the first copy on this machine to start.
pub fn open_own_socket(base: u16) -> Option<(UdpSocket, u32)> {
    (0..64u32).find_map(|slot| open_socket(base.wrapping_add(1).wrapping_add(slot as u16), false).map(|s| (s, slot)))
}

/// The first IPv4 address of a host, a port its number or the name of a service, as
/// the system's resolver reads them.
pub fn resolve(host: &CStr, port: &CStr) -> Option<SocketAddrV4> {
    let hints = hints();
    let mut found = std::ptr::null_mut();
    if unsafe { getaddrinfo(host.as_ptr(), port.as_ptr(), &hints, &mut found) } != 0 || found.is_null() {
        return None;
    }
    let first = unsafe { &*found };
    let address = (!first.address.is_null()).then(|| unsafe { &*first.address }.address());
    unsafe { freeaddrinfo(found) };
    address
}
