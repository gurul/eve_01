//! eve lite's view of the home network: this Mac's LAN addresses, who may knock on the LAN door, the Mac's Bonjour
//! name, and the macOS Local Network privilege (docs/plans/eve-lite.md M4, M5, M6). Port of `lan_net.py`.
//!
//! * **Addresses.** [`private_addresses`] lists this Mac's RFC 1918 IPv4 addresses on the interfaces a phone on the
//!   same Wi-Fi can reach, best first. They go into the LAN certificate (lite_tls), the LAN door's Host allowlist
//!   and the Android QR code. VPN tunnels, VM bridges, AirDrop's links and loopback are left out: a phone cannot use
//!   them, and every new address reissues the certificate, which brings the browser warning back.
//! * **The peer filter.** [`peer_allowed`] runs on every TCP connection to the LAN door, before TLS. It admits
//!   loopback, RFC 1918, link-local and ULA addresses, and an address inside the prefix of one of the Mac's LAN
//!   interfaces: home IPv6 uses global addresses, and a phone on the same Wi-Fi shares the Mac's /64. Everything
//!   else is dropped. The ranges are spelled out because a generic "is private" test also counts documentation,
//!   benchmarking and reserved space.
//! * **The Bonjour name.** [`local_hostname`] is the LocalHostName that `<name>.local` answers to, read with
//!   `/usr/sbin/scutil` (one of the three programs the lite process firewall lets start, profile).
//! * **Local Network.** A LaunchAgent needs the Local Network privilege to send UDP on the LAN, though never to
//!   accept TCP (TN3179). [`trigger_local_network_alert`] raises the macOS alert on purpose, at "Add a phone", with
//!   a UDP connect to the mDNS group that sends nothing: a home gateway is usually the LAN's DNS server, which
//!   TN3179 exempts, so it would raise nothing. [`probe`] then sends one empty datagram to the phone's discard
//!   port and reads the answer from the error: EHOSTUNREACH or EPERM is "blocked".
//!
//! Interfaces are read with getifaddrs (the same walk Python's `ifaddr` does), so nothing here starts a process
//! except scutil. Logs carry types and errno names, never an address or the Mac's name.
//!
//! Seams: every function that touches the system has a `*_with` / `*_in` / `*_from` twin taking the system part
//! as a parameter (the interface list, the scutil runner, the host name, the UDP socket factory), which is what the
//! Python tests monkeypatched.

use std::ffi::CStr;
use std::io::{self, Read};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use regex::Regex;

/// An IPv4 or IPv6 address (Python's `Address` union).
pub type Address = IpAddr;

/// An IPv4 or IPv6 network: an address with its host bits cleared and a prefix length (Python's `Network`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Network {
    addr: IpAddr,
    prefix: u8,
}

impl Network {
    /// A network from an address and prefix, host bits cleared (`ip_network(..., strict=False)`). The prefix is
    /// clamped to the family's width.
    pub fn new(addr: IpAddr, prefix: u8) -> Network {
        match addr {
            IpAddr::V4(a) => {
                let prefix = prefix.min(32);
                Network { addr: IpAddr::V4(Ipv4Addr::from(u32::from(a) & mask_v4(prefix))), prefix }
            }
            IpAddr::V6(a) => {
                let prefix = prefix.min(128);
                Network { addr: IpAddr::V6(Ipv6Addr::from(u128::from(a) & mask_v6(prefix))), prefix }
            }
        }
    }

    const fn v4(a: u8, b: u8, c: u8, d: u8, prefix: u8) -> Network {
        Network { addr: IpAddr::V4(Ipv4Addr::new(a, b, c, d)), prefix }
    }

    const fn v6(first: u16, prefix: u8) -> Network {
        Network { addr: IpAddr::V6(Ipv6Addr::new(first, 0, 0, 0, 0, 0, 0, 0)), prefix }
    }

    /// The network address.
    pub fn network_address(&self) -> IpAddr {
        self.addr
    }

    /// The prefix length.
    pub fn prefixlen(&self) -> u8 {
        self.prefix
    }

    /// 4 or 6.
    pub fn version(&self) -> u8 {
        version(&self.addr)
    }

    /// `ip in net`: false across families, as in Python.
    pub fn contains(&self, ip: &IpAddr) -> bool {
        match (self.addr, ip) {
            (IpAddr::V4(n), IpAddr::V4(a)) => u32::from(*a) & mask_v4(self.prefix) == u32::from(n),
            (IpAddr::V6(n), IpAddr::V6(a)) => u128::from(*a) & mask_v6(self.prefix) == u128::from(n),
            _ => false,
        }
    }
}

impl std::fmt::Display for Network {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

fn mask_v4(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - u32::from(prefix.min(32)))
    }
}

fn mask_v6(prefix: u8) -> u128 {
    if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - u32::from(prefix.min(128)))
    }
}

fn version(ip: &IpAddr) -> u8 {
    match ip {
        IpAddr::V4(_) => 4,
        IpAddr::V6(_) => 6,
    }
}

fn max_prefixlen(ip: &IpAddr) -> u32 {
    match ip {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    }
}

/// RFC 1918: 10/8, 172.16/12, 192.168/16.
pub static PRIVATE_V4: [Network; 3] =
    [Network::v4(10, 0, 0, 0, 8), Network::v4(172, 16, 0, 0, 12), Network::v4(192, 168, 0, 0, 16)];
/// IPv4 link-local, 169.254/16.
pub static LINK_LOCAL_V4: Network = Network::v4(169, 254, 0, 0, 16);
/// Unique local addresses, fc00::/7.
pub static ULA: Network = Network::v6(0xfc00, 7);
/// IPv6 link-local, fe80::/10.
pub static LINK_LOCAL_V6: Network = Network::v6(0xfe80, 10);

static LAN_PEERS_V4: [Network; 4] = [PRIVATE_V4[0], PRIVATE_V4[1], PRIVATE_V4[2], LINK_LOCAL_V4];
static LAN_PEERS_V6: [Network; 2] = [ULA, LINK_LOCAL_V6];

/// `LAN_PEERS[version]`: the fixed ranges a LAN door peer may come from, by IP version (4 or 6). Empty for any
/// other version.
pub fn lan_peers(version: u8) -> &'static [Network] {
    match version {
        4 => &LAN_PEERS_V4,
        6 => &LAN_PEERS_V6,
        _ => &[],
    }
}

// An interface prefix wider than this is no single home link (a mistyped netmask, a /8), so it admits no one. TCP
// keeps the rest honest: a peer that only claims an on-link address never sees the handshake's reply.
/// `MIN_ON_LINK_PREFIX[4]`.
pub const MIN_ON_LINK_PREFIX_V4: u32 = 16;
/// `MIN_ON_LINK_PREFIX[6]`.
pub const MIN_ON_LINK_PREFIX_V6: u32 = 48;

/// `MIN_ON_LINK_PREFIX[version]`.
pub fn min_on_link_prefix(version: u8) -> u32 {
    if version == 4 {
        MIN_ON_LINK_PREFIX_V4
    } else {
        MIN_ON_LINK_PREFIX_V6
    }
}

/// Interfaces a phone on the LAN never reaches the Mac through: loopback, tunnels (VPNs, iCloud Private Relay),
/// bridges (Internet Sharing, VMs, Thunderbolt Bridge), AirDrop and Sidecar links (awdl, llw), the access point
/// and the Apple silicon debug interface (anpi). Physical Wi-Fi and Ethernet, and USB adapters, are en<N>.
/// Anchored: Python uses it with `fullmatch`.
pub static NOT_LAN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:lo|gif|stf|utun|tun|tap|ipsec|ppp|bridge|vmenet|vnic|feth|awdl|llw|anpi|ap)\d*$")
        .expect("NOT_LAN is a valid pattern")
});

/// scutil, by full path: the lite process firewall's allowlist names it so.
pub const SCUTIL: &str = "/usr/sbin/scutil";
/// How long scutil may take.
pub const SCUTIL_SECS: f64 = 5.0;
/// One DNS label (RFC 1123). Anchored: Python uses it with `fullmatch`.
pub static LABEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?$").expect("LABEL is a valid pattern")
});

/// The mDNS group, 224.0.0.251:5353.
pub const MDNS_GROUP: (Ipv4Addr, u16) = (Ipv4Addr::new(224, 0, 0, 251), 5353);
/// RFC 863: whatever arrives is thrown away, and nothing answers.
pub const DISCARD_PORT: u16 = 9;
/// The errnos that mean macOS refused the datagram: the Local Network privilege is off.
pub const BLOCKED: [i32; 2] = [libc::EHOSTUNREACH, libc::EPERM];

// ---- what a peer may be ------------------------------------------------------------------------------------------

/// What the Python functions accept as `addr: Any`: a string (`fe80::1%en0` and `[::1]` too), a socket peername
/// (a `SocketAddr`, or the asyncio tuple's host and scope id), an address object, or anything else (`None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Peer {
    /// Not an address of any kind: `None`, an int, bytes, an empty tuple.
    None,
    /// A string.
    Text(String),
    /// An address object (no zone).
    Ip(IpAddr),
    /// A socket peername; a V6 one carries its scope id.
    Socket(SocketAddr),
    /// A peername tuple as asyncio gives it: `(host, port)` or `(host, port, flowinfo, scope_id)`. `scope_id` is 0
    /// for a 2-tuple.
    Tuple { host: String, scope_id: u32 },
}

impl From<&str> for Peer {
    fn from(s: &str) -> Peer {
        Peer::Text(s.to_string())
    }
}

impl From<String> for Peer {
    fn from(s: String) -> Peer {
        Peer::Text(s)
    }
}

impl From<&String> for Peer {
    fn from(s: &String) -> Peer {
        Peer::Text(s.clone())
    }
}

impl From<IpAddr> for Peer {
    fn from(ip: IpAddr) -> Peer {
        Peer::Ip(ip)
    }
}

impl From<Ipv4Addr> for Peer {
    fn from(ip: Ipv4Addr) -> Peer {
        Peer::Ip(IpAddr::V4(ip))
    }
}

impl From<Ipv6Addr> for Peer {
    fn from(ip: Ipv6Addr) -> Peer {
        Peer::Ip(IpAddr::V6(ip))
    }
}

impl From<SocketAddr> for Peer {
    fn from(sa: SocketAddr) -> Peer {
        Peer::Socket(sa)
    }
}

impl From<&Peer> for Peer {
    fn from(p: &Peer) -> Peer {
        p.clone()
    }
}

impl From<(&str, u16)> for Peer {
    fn from((host, _port): (&str, u16)) -> Peer {
        Peer::Tuple { host: host.to_string(), scope_id: 0 }
    }
}

impl From<(&str, u16, u32, u32)> for Peer {
    fn from((host, _port, _flowinfo, scope_id): (&str, u16, u32, u32)) -> Peer {
        Peer::Tuple { host: host.to_string(), scope_id }
    }
}

impl<T: Into<Peer>> From<Option<T>> for Peer {
    fn from(v: Option<T>) -> Peer {
        v.map_or(Peer::None, Into::into)
    }
}

/// Python's `str.isspace()` for one character: Unicode White_Space plus the four separators \x1c-\x1f, which
/// Python's `str.strip()` also removes.
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// Python's `text.strip()`.
fn py_strip(text: &str) -> &str {
    text.trim_matches(py_space)
}

/// Parse Python's `ipaddress.ip_address(text)` way: IPv4 dotted quad, or IPv6 with an optional zone
/// (`%<zone>`, which must be non-empty and hold no `%`). The zone is dropped.
fn parse_ip(text: &str) -> Option<IpAddr> {
    if let Ok(v4) = text.parse::<Ipv4Addr>() {
        return Some(IpAddr::V4(v4));
    }
    let bare = match text.split_once('%') {
        None => text,
        Some((addr, zone)) => {
            if zone.is_empty() || zone.contains('%') {
                return None;
            }
            addr
        }
    };
    bare.parse::<Ipv6Addr>().ok().map(IpAddr::V6)
}

/// `_address`: `addr` as an address: a string (`fe80::1%en0` and `[::1]` too), a socket peername, or an address
/// object. An IPv4-mapped IPv6 address (a dual-stack listener's IPv4 peer) becomes its IPv4 address; a zone is
/// dropped. None when it is none of these.
pub fn address(addr: &Peer) -> Option<Address> {
    let ip = match addr {
        Peer::None => return None,
        Peer::Ip(ip) => *ip,
        Peer::Socket(sa) => sa.ip(),
        Peer::Text(text) | Peer::Tuple { host: text, .. } => {
            let mut text = py_strip(text);
            if text.starts_with('[') && text.ends_with(']') && text.len() >= 2 {
                text = &text[1..text.len() - 1];
            }
            parse_ip(text)?
        }
    };
    Some(match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
    })
}

/// `_scope`: the zone of a link-local IPv6 peer (`en0` from `fe80::1%en0`, or a peername's scope id), or "".
pub fn scope(addr: &Peer) -> String {
    let text = match addr {
        Peer::Socket(SocketAddr::V6(sa)) if sa.scope_id() != 0 => return sa.scope_id().to_string(),
        Peer::Tuple { scope_id, .. } if *scope_id != 0 => return scope_id.to_string(),
        Peer::Tuple { host, .. } => host.as_str(),
        Peer::Text(text) => text.as_str(),
        _ => return String::new(),
    };
    if !text.contains('%') {
        return String::new();
    }
    let stripped = py_strip(text).trim_matches(|c| c == '[' || c == ']');
    stripped.split_once('%').map(|(_, zone)| zone.to_string()).unwrap_or_default()
}

// ---- the interfaces ----------------------------------------------------------------------------------------------

/// One address on one interface (Python's `(interface, address, prefix length)` tuple).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceIp {
    pub name: String,
    pub ip: Address,
    pub prefix: u32,
}

impl InterfaceIp {
    pub fn new(name: &str, ip: Address, prefix: u32) -> InterfaceIp {
        InterfaceIp { name: name.to_string(), ip, prefix }
    }
}

/// One interface as `ifaddr` reports it: its name and its addresses, in the order the system gave them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adapter {
    pub name: String,
    pub ips: Vec<AdapterIp>,
}

/// One address of an [`Adapter`]: the address text (no zone) and the netmask's prefix length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterIp {
    pub ip: String,
    pub network_prefix: u32,
}

/// Why the interfaces could not be read. `kind` is the Python exception type name the log carries
/// (`OSError` for a failed getifaddrs, `AssertionError` for an address without a netmask, `NetmaskValueError` for a
/// netmask that is no mask, `UnicodeDecodeError` for a name that is not UTF-8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterError {
    pub kind: String,
    pub message: String,
}

impl AdapterError {
    pub fn new(kind: &str, message: impl Into<String>) -> AdapterError {
        AdapterError { kind: kind.to_string(), message: message.into() }
    }
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for AdapterError {}

/// The prefix length of an IPv4 netmask, as `IPv4Network("0.0.0.0/<mask>").prefixlen` reads it: a netmask
/// (ones then zeros), or failing that a hostmask (zeros then ones).
fn prefix_from_v4_mask(mask: u32) -> Result<u32, AdapterError> {
    fn prefix_of(v: u32) -> Option<u32> {
        let trailing = if v == 0 { 32 } else { v.trailing_zeros() };
        let prefixlen = 32 - trailing;
        let leading = if trailing == 32 { 0 } else { v >> trailing };
        let all_ones = if prefixlen == 0 { 0 } else { u32::MAX >> (32 - prefixlen) };
        (leading == all_ones).then_some(prefixlen)
    }
    prefix_of(mask)
        .or_else(|| prefix_of(mask ^ u32::MAX))
        .ok_or_else(|| AdapterError::new("NetmaskValueError", format!("{} is not a valid netmask", Ipv4Addr::from(mask))))
}

/// A sockaddr's family, and its address bytes. On BSD the read stops at `sa_len` (the rest is zero, as a routing
/// sockaddr's truncated tail means), so no byte past the system's buffer is read.
///
/// # Safety
/// `sa` must be null or point at a sockaddr from getifaddrs.
unsafe fn sockaddr_family(sa: *const libc::sockaddr) -> Option<i32> {
    if sa.is_null() {
        return None;
    }
    Some(i32::from(unsafe { (*sa).sa_family }))
}

/// # Safety
/// `sa` must point at a sockaddr from getifaddrs.
unsafe fn sockaddr_bytes<const N: usize>(sa: *const libc::sockaddr, offset: usize) -> [u8; N] {
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
    let len = usize::from(unsafe { (*sa).sa_len });
    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd")))]
    let len = offset + N;
    let mut out = [0u8; N];
    let base = sa as *const u8;
    for (i, slot) in out.iter_mut().enumerate() {
        if offset + i < len {
            *slot = unsafe { *base.add(offset + i) };
        }
    }
    out
}

const SIN_ADDR_OFFSET: usize = std::mem::offset_of!(libc::sockaddr_in, sin_addr);
const SIN6_ADDR_OFFSET: usize = std::mem::offset_of!(libc::sockaddr_in6, sin6_addr);

/// `ifaddr`'s `sockaddr_to_ip`: the address of an AF_INET or AF_INET6 sockaddr, read as `family` (the
/// sockaddr's own family unless overridden). None for any other family or a null pointer.
///
/// # Safety
/// `sa` must be null or point at a sockaddr from getifaddrs.
unsafe fn sockaddr_ip(sa: *const libc::sockaddr, family: Option<i32>) -> Option<IpAddr> {
    let fam = family.or(unsafe { sockaddr_family(sa) })?;
    if sa.is_null() {
        return None;
    }
    if fam == libc::AF_INET {
        Some(IpAddr::V4(Ipv4Addr::from(unsafe { sockaddr_bytes::<4>(sa, SIN_ADDR_OFFSET) })))
    } else if fam == libc::AF_INET6 {
        Some(IpAddr::V6(Ipv6Addr::from(unsafe { sockaddr_bytes::<16>(sa, SIN6_ADDR_OFFSET) })))
    } else {
        None
    }
}

/// Every interface and its addresses, through getifaddrs, as Python's `ifaddr.get_adapters()` reports them:
/// grouped by interface name in the order the names first appear, each interface's addresses in system order,
/// only AF_INET and AF_INET6 addresses, each with its netmask's prefix length. A netmask without a family takes
/// the address's family, as ifaddr does.
pub fn get_adapters() -> Result<Vec<Adapter>, AdapterError> {
    struct Owned(*mut libc::ifaddrs);
    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { libc::freeifaddrs(self.0) };
            }
        }
    }

    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        let e = io::Error::last_os_error();
        return Err(AdapterError::new("OSError", e.to_string()));
    }
    let owned = Owned(head);
    let mut adapters: Vec<Adapter> = Vec::new();
    let mut cur = owned.0;
    while !cur.is_null() {
        // SAFETY: cur walks the list getifaddrs returned, which stays alive until `owned` drops.
        let ifa = unsafe { &*cur };
        cur = ifa.ifa_next;
        let name = if ifa.ifa_name.is_null() {
            String::new()
        } else {
            match unsafe { CStr::from_ptr(ifa.ifa_name) }.to_str() {
                Ok(s) => s.to_string(),
                Err(e) => return Err(AdapterError::new("UnicodeDecodeError", e.to_string())),
            }
        };
        let addr = ifa.ifa_addr as *const libc::sockaddr;
        let Some(ip) = (unsafe { sockaddr_ip(addr, None) }) else { continue };
        let addr_family = unsafe { sockaddr_family(addr) };
        let mask = ifa.ifa_netmask as *const libc::sockaddr;
        let mask_family = match unsafe { sockaddr_family(mask) } {
            Some(0) => addr_family,
            other => other,
        };
        let prefix = match unsafe { sockaddr_ip(mask, mask_family) } {
            Some(IpAddr::V4(m)) => prefix_from_v4_mask(u32::from(m))?,
            Some(IpAddr::V6(m)) => u128::from(m).count_ones(),
            None => return Err(AdapterError::new("AssertionError", "sockaddr_to_ip(netmask) returned None")),
        };
        let entry = AdapterIp { ip: ip.to_string(), network_prefix: prefix };
        match adapters.iter_mut().find(|a| a.name == name) {
            Some(a) => a.ips.push(entry),
            None => adapters.push(Adapter { name, ips: vec![entry] }),
        }
    }
    Ok(adapters)
}

/// `_interface_ips` over a given adapter listing: every address on every interface, as (interface, address,
/// prefix length). [] when the interfaces could not be read, with a warning naming only the error's type.
pub fn interface_ips_from(adapters: Result<Vec<Adapter>, AdapterError>) -> Vec<InterfaceIp> {
    let adapters = match adapters {
        Ok(a) => a,
        // a broken getifaddrs is "no LAN", never a crash at the door
        Err(e) => {
            tracing::warn!("lan: could not list the network interfaces ({})", e.kind);
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for adapter in adapters {
        for entry in &adapter.ips {
            if let Some(ip) = address(&Peer::Text(entry.ip.clone())) {
                out.push(InterfaceIp { name: adapter.name.clone(), ip, prefix: entry.network_prefix });
            }
        }
    }
    out
}

/// `_interface_ips`: every address on every interface, read with getifaddrs, so no process starts; [] when the
/// interfaces cannot be read.
pub fn interface_ips() -> Vec<InterfaceIp> {
    interface_ips_from(get_adapters())
}

/// `_lan_interface`: may a phone on the LAN reach the Mac through this interface?
pub fn lan_interface(name: &str) -> bool {
    !NOT_LAN.is_match(name)
}

static ORDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Za-z]+)(\d*)$").expect("ORDER is a valid pattern"));
static DIGIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d$").expect("DIGIT is a valid pattern"));

/// The value of one decimal digit as Python's `int()` reads it: `\d` (Unicode Nd) matches any script's digits.
/// Unicode encodes every decimal digit set as a contiguous run of ten, zero first, and runs may abut, so the value
/// is the distance from the start of the stretch of digits, modulo ten.
fn digit_value(c: char) -> u128 {
    if let Some(d) = c.to_digit(10) {
        return u128::from(d);
    }
    let is_digit = |cp: u32| char::from_u32(cp).is_some_and(|ch| DIGIT.is_match(ch.encode_utf8(&mut [0u8; 4])));
    let mut start = u32::from(c);
    while start > 0 && is_digit(start - 1) {
        start -= 1;
    }
    u128::from((u32::from(c) - start) % 10)
}

/// `_interface_order`: en0 before en1 before en10 before anything else: on a Mac, en0 is the built-in Wi-Fi or
/// Ethernet.
pub fn interface_order(name: &str) -> (u8, String, u128) {
    let (stem, num) = match ORDER.captures(name) {
        Some(m) => {
            let digits = m.get(2).map_or("", |d| d.as_str());
            let num = digits.chars().fold(0u128, |n, d| n.saturating_mul(10).saturating_add(digit_value(d)));
            (m[1].to_string(), num)
        }
        None => (name.to_string(), 0),
    };
    (if stem == "en" { 0 } else { 1 }, stem, num)
}

/// `private_addresses` over a given interface list.
pub fn private_addresses_in(interfaces: &[InterfaceIp]) -> Vec<String> {
    let mut found: Vec<((u8, String, u128), usize, String)> = Vec::new();
    for (i, entry) in interfaces.iter().enumerate() {
        if matches!(entry.ip, IpAddr::V4(_))
            && lan_interface(&entry.name)
            && PRIVATE_V4.iter().any(|net| net.contains(&entry.ip))
        {
            found.push((interface_order(&entry.name), i, entry.ip.to_string()));
        }
    }
    found.sort();
    let mut out: Vec<String> = Vec::new();
    for (_order, _i, ip_text) in found {
        if !out.contains(&ip_text) {
            out.push(ip_text);
        }
    }
    out
}

/// This Mac's RFC 1918 IPv4 addresses on its LAN interfaces, best first (en0 first), each once. [] when the Mac
/// has none, or its interfaces cannot be read.
pub fn private_addresses() -> Vec<String> {
    private_addresses_in(&interface_ips())
}

/// `_on_link_networks`: the prefixes of the Mac's LAN interfaces that are narrow enough to be one home link.
pub fn on_link_networks(interfaces: &[InterfaceIp]) -> Vec<Network> {
    let mut nets = Vec::new();
    for entry in interfaces {
        if entry.ip.is_loopback() || !lan_interface(&entry.name) {
            continue;
        }
        if min_on_link_prefix(version(&entry.ip)) <= entry.prefix && entry.prefix <= max_prefixlen(&entry.ip) {
            nets.push(Network::new(entry.ip, entry.prefix as u8));
        }
    }
    nets
}

fn allowed_ip(ip: &IpAddr, interfaces: impl FnOnce() -> Vec<InterfaceIp>) -> bool {
    if ip.is_unspecified() || ip.is_multicast() {
        return false;
    }
    if ip.is_loopback() || lan_peers(version(ip)).iter().any(|net| net.contains(ip)) {
        return true;
    }
    on_link_networks(&interfaces()).iter().any(|net| net.version() == version(ip) && net.contains(ip))
}

/// `peer_allowed` with the interface list as a parameter; it is read only when the fixed ranges did not decide.
pub fn peer_allowed_with(addr: &Peer, interfaces: impl FnOnce() -> Vec<InterfaceIp>) -> bool {
    match address(addr) {
        Some(ip) => allowed_ip(&ip, interfaces),
        None => false,
    }
}

/// May this peer reach the LAN door? Loopback, RFC 1918, link-local (169.254/16, fe80::/10), ULA (fc00::/7), or
/// inside the prefix of one of the Mac's LAN interfaces. Anything else, and anything that is not an address, is
/// refused. `addr` may be the peername as the listener gives it.
pub fn peer_allowed(addr: impl Into<Peer>) -> bool {
    peer_allowed_with(&addr.into(), interface_ips)
}

// ---- the Bonjour name --------------------------------------------------------------------------------------------

/// What a finished scutil run gave: its exit code (None when a signal ended it) and its stdout bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutput {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
}

/// Run `argv` with stdin from /dev/null, stdout and stderr captured, killed at `timeout` (`subprocess.run(...,
/// capture_output=True, stdin=DEVNULL, timeout=...)`). A timeout is an `io::ErrorKind::TimedOut` error.
pub fn run_captured(argv: &[&str], timeout: Duration) -> io::Result<RunOutput> {
    let (program, args) = argv.split_first().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty argv"))?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(s) = stdout.as_mut() {
            let _ = s.read_to_end(&mut buf);
        }
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(s) = stderr.as_mut() {
            let _ = s.read_to_end(&mut buf);
        }
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = out_reader.join();
            let _ = err_reader.join();
            return Err(io::Error::new(io::ErrorKind::TimedOut, format!("{program} timed out")));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let stdout = out_reader.join().unwrap_or_default();
    let _ = err_reader.join();
    Ok(RunOutput { code: status.code(), stdout })
}

/// The process's host name (`socket.gethostname()`).
pub fn gethostname() -> io::Result<String> {
    let mut buf = vec![0u8; 1024];
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Ok(String::from_utf8_lossy(&buf[..end]).into_owned())
}

/// The OSError subclass Python raises for an errno (PEP 3151's table, `Objects/exceptions.c`).
fn os_error_subclass(code: i32) -> &'static str {
    match code {
        libc::EAGAIN | libc::EALREADY | libc::EINPROGRESS => "BlockingIOError",
        libc::ECHILD => "ChildProcessError",
        libc::EPIPE | libc::ESHUTDOWN => "BrokenPipeError",
        libc::ECONNABORTED => "ConnectionAbortedError",
        libc::ECONNREFUSED => "ConnectionRefusedError",
        libc::ECONNRESET => "ConnectionResetError",
        libc::EEXIST => "FileExistsError",
        libc::ENOENT => "FileNotFoundError",
        libc::EISDIR => "IsADirectoryError",
        libc::ENOTDIR => "NotADirectoryError",
        libc::EINTR => "InterruptedError",
        libc::EACCES | libc::EPERM => "PermissionError",
        libc::ESRCH => "ProcessLookupError",
        libc::ETIMEDOUT => "TimeoutError",
        _ => "OSError",
    }
}

/// The Python exception type an `io::Error` from running scutil would have been: the errno's OSError subclass
/// for a failed start, `TimeoutExpired` for the timeout, `UnicodeDecodeError` for stdout that is not UTF-8.
fn run_error_kind(e: &io::Error) -> &'static str {
    if let Some(code) = e.raw_os_error() {
        return os_error_subclass(code);
    }
    match e.kind() {
        io::ErrorKind::NotFound => "FileNotFoundError",
        io::ErrorKind::PermissionDenied => "PermissionError",
        io::ErrorKind::TimedOut => "TimeoutExpired",
        io::ErrorKind::InvalidData => "UnicodeDecodeError",
        _ => "OSError",
    }
}

/// `local_hostname` with the scutil runner and the host name as parameters.
pub fn local_hostname_with(
    run: impl FnOnce(&[&str], Duration) -> io::Result<RunOutput>,
    hostname: impl FnOnce() -> io::Result<String>,
) -> String {
    let mut name = String::new();
    let outcome = run(&[SCUTIL, "--get", "LocalHostName"], Duration::from_secs_f64(SCUTIL_SECS)).and_then(|done| {
        // text=True decodes strictly: bytes that are not UTF-8 are an error, as in Python
        let text = String::from_utf8(done.stdout).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok((done.code, text))
    });
    match outcome {
        Ok((Some(0), text)) => name = py_strip(&text).to_string(),
        Ok(_) => {}
        Err(e) => tracing::info!("lan: scutil could not tell the local host name ({})", run_error_kind(&e)),
    }
    if LABEL.is_match(&name) {
        return name;
    }
    let Ok(host) = hostname() else { return String::new() };
    let host = py_strip(&host).trim_end_matches('.');
    if let Some((stem, suffix)) = host.rsplit_once('.') {
        if suffix.to_lowercase() == "local" && LABEL.is_match(stem) {
            return stem.to_string();
        }
    }
    String::new()
}

/// This Mac's Bonjour name, the LocalHostName that `<name>.local` resolves to on the LAN ("Kitchen-Mac"), or ""
/// when it cannot be told. Falls back to the host name only when that is a `.local` name.
pub fn local_hostname() -> String {
    local_hostname_with(run_captured, gethostname)
}

// ---- Local Network: the alert and the probe ----------------------------------------------------------------------

/// A socket family: AF_INET or AF_INET6.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Inet,
    Inet6,
}

/// What this module does with a UDP socket. Dropping it closes it.
pub trait UdpSocket {
    /// `setblocking(flag)`.
    fn set_blocking(&mut self, flag: bool) -> io::Result<()>;
    /// `connect(target)`.
    fn connect(&mut self, target: SocketAddr) -> io::Result<()>;
    /// `send(data)`.
    fn send(&mut self, data: &[u8]) -> io::Result<usize>;
}

struct SystemUdp(socket2::Socket);

impl UdpSocket for SystemUdp {
    fn set_blocking(&mut self, flag: bool) -> io::Result<()> {
        self.0.set_nonblocking(!flag)
    }

    fn connect(&mut self, target: SocketAddr) -> io::Result<()> {
        self.0.connect(&target.into())
    }

    fn send(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0.send(data)
    }
}

/// `_udp`: a new, unbound UDP socket of this family.
pub fn udp(family: Family) -> io::Result<Box<dyn UdpSocket>> {
    let domain = match family {
        Family::Inet => socket2::Domain::IPV4,
        Family::Inet6 => socket2::Domain::IPV6,
    };
    let s = socket2::Socket::new(domain, socket2::Type::DGRAM, None)?;
    Ok(Box::new(SystemUdp(s)))
}

macro_rules! errno_names {
    ($code:expr; $($name:ident),* $(,)?) => {{
        let code = $code;
        $( if code == libc::$name { return Some(stringify!($name)); } )*
        None
    }};
}

/// `errno.errorcode[code]`, for the errnos a socket or a process start can give.
fn errno_code_name(code: i32) -> Option<&'static str> {
    errno_names!(code;
        EPERM, ENOENT, ESRCH, EINTR, EIO, ENXIO, E2BIG, ENOEXEC, EBADF, ECHILD, EAGAIN, ENOMEM, EACCES, EFAULT,
        EBUSY, EEXIST, EXDEV, ENODEV, ENOTDIR, EISDIR, EINVAL, ENFILE, EMFILE, ENOTTY, ETXTBSY, EFBIG, ENOSPC,
        ESPIPE, EROFS, EMLINK, EPIPE, EDOM, ERANGE, EDEADLK, ENAMETOOLONG, ENOLCK, ENOSYS, ENOTEMPTY, ELOOP,
        ENOTSOCK, EDESTADDRREQ, EMSGSIZE, EPROTOTYPE, ENOPROTOOPT, EPROTONOSUPPORT, ESOCKTNOSUPPORT, EOPNOTSUPP,
        EPFNOSUPPORT, EAFNOSUPPORT, EADDRINUSE, EADDRNOTAVAIL, ENETDOWN, ENETUNREACH, ENETRESET, ECONNABORTED,
        ECONNRESET, ENOBUFS, EISCONN, ENOTCONN, ESHUTDOWN, ETOOMANYREFS, ETIMEDOUT, ECONNREFUSED, EHOSTDOWN,
        EHOSTUNREACH, EALREADY, EINPROGRESS, ESTALE, EDQUOT, ECANCELED, EOVERFLOW, EILSEQ, ENOTSUP, EPROTO,
        ENOTBLK, EUSERS, EREMOTE, EIDRM, ENOMSG, EBADMSG, EMULTIHOP, ENODATA, ENOLINK, ENOSR, ENOSTR, ETIME,
        ENOTRECOVERABLE, EOWNERDEAD,
    )
    .or_else(|| apple_errno_name(code))
}

/// The errnos only Darwin has, so `errno.errorcode` there knows every code from 1 to 106.
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn apple_errno_name(code: i32) -> Option<&'static str> {
    errno_names!(code;
        EPROCLIM, EBADRPC, ERPCMISMATCH, EPROGUNAVAIL, EPROGMISMATCH, EPROCUNAVAIL, EFTYPE, EAUTH, ENEEDAUTH,
        EPWROFF, EDEVERR, EBADEXEC, EBADARCH, ESHLIBVERS, EBADMACHO, ENOATTR, ENOPOLICY, EQFULL,
    )
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
fn apple_errno_name(_code: i32) -> Option<&'static str> {
    None
}

/// `_errno_name`: the errno's name ("EHOSTUNREACH"), or the error's type when it has no known errno. A
/// getaddrinfo failure carries its EAI code as its errno, as Python's `socket.gaierror` does, so it is looked up in
/// the same table (EAI_NONAME, 8, reads as "ENOEXEC" on a Mac, exactly as Python logs it).
pub fn errno_name(e: &io::Error) -> String {
    let type_name = if e.get_ref().is_some_and(|inner| inner.is::<GaiError>()) {
        "gaierror"
    } else if e.get_ref().is_some_and(|inner| inner.is::<IdnaError>()) {
        "UnicodeEncodeError"
    } else {
        "OSError"
    };
    py_errno(e).and_then(errno_code_name).unwrap_or(type_name).to_string()
}

/// The error's `errno` as Python sees it: the OS errno, or a gaierror's EAI code.
fn py_errno(e: &io::Error) -> Option<i32> {
    match e.get_ref().and_then(|inner| inner.downcast_ref::<GaiError>()) {
        Some(gai) => Some(gai.0),
        None => e.raw_os_error(),
    }
}

/// `trigger_local_network_alert` with the socket factory as a parameter.
pub fn trigger_local_network_alert_with(make: impl FnOnce(Family) -> io::Result<Box<dyn UdpSocket>>) {
    let outcome = make(Family::Inet).and_then(|mut s| {
        s.connect(SocketAddr::V4(SocketAddrV4::new(MDNS_GROUP.0, MDNS_GROUP.1)))
        // the socket closes here, on drop, whatever connect said
    });
    if let Err(e) = outcome {
        tracing::info!("lan: the local network trigger got {}", errno_name(&e));
    }
}

/// Make macOS ask for the Local Network privilege now: a UDP connect to the mDNS group, sending nothing. Never
/// fails. After the owner's first answer macOS remembers it, and this does nothing visible.
pub fn trigger_local_network_alert() {
    trigger_local_network_alert_with(udp)
}

/// A getaddrinfo failure: Python's `socket.gaierror`, whose errno is the EAI code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GaiError(pub i32);

impl std::fmt::Display for GaiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "getaddrinfo failed with code {}", self.0)
    }
}

impl std::error::Error for GaiError {}

/// A host Python's `idna` codec refuses before getaddrinfo is called (a label empty or longer than 63).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdnaError(pub String);

impl std::fmt::Display for IdnaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "'idna' codec can't encode: {}", self.0)
    }
}

impl std::error::Error for IdnaError {}

/// The bytes Python's `socket.getaddrinfo` hands the system for a str host: the `idna` codec's encoding. An ASCII
/// host passes through when every label but the last is 1 to 63 bytes and the last at most 63; any other ASCII host
/// is a UnicodeEncodeError. A non-ASCII host is punycoded, which no numeric parse accepts (EAI_NONAME).
fn idna_host(host: &str) -> io::Result<std::ffi::CString> {
    if !host.is_ascii() {
        return Err(io::Error::other(GaiError(libc::EAI_NONAME)));
    }
    let labels: Vec<&str> = host.split('.').collect();
    let (last, rest) = labels.split_last().unwrap_or((&"", &[]));
    if rest.iter().any(|label| label.is_empty() || label.len() >= 64) {
        return Err(io::Error::other(IdnaError("label empty or too long".to_string())));
    }
    if last.len() >= 64 {
        return Err(io::Error::other(IdnaError("label too long".to_string())));
    }
    // a C string ends at the first NUL: getaddrinfo reads the host only up to there, as it does from Python
    let up_to_nul = host.split('\0').next().unwrap_or_default();
    std::ffi::CString::new(up_to_nul).map_err(io::Error::other)
}

/// `socket.getaddrinfo(host, port, family, SOCK_DGRAM, 0, AI_NUMERICHOST)[0][4]`: the system parses the numeric
/// host, and a zone (`fe80::1%en0`, `fe80::1%4`) goes through the system's own scope rules, so an unknown zone
/// reads as the system reads it (scope 0 on a Mac). Nothing is looked up on the network.
fn numeric_sockaddr(host: &str, port: u16, family: Family) -> io::Result<SocketAddr> {
    struct Owned(*mut libc::addrinfo);
    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { libc::freeaddrinfo(self.0) };
            }
        }
    }

    let c_host = idna_host(host)?;
    let c_port = std::ffi::CString::new(port.to_string()).map_err(io::Error::other)?;
    // SAFETY: addrinfo is a plain C struct; all-zero is its documented "no hints" value.
    let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
    hints.ai_family = match family {
        Family::Inet => libc::AF_INET,
        Family::Inet6 => libc::AF_INET6,
    };
    hints.ai_socktype = libc::SOCK_DGRAM;
    hints.ai_flags = libc::AI_NUMERICHOST;
    let mut res: *mut libc::addrinfo = std::ptr::null_mut();
    let rc = unsafe { libc::getaddrinfo(c_host.as_ptr(), c_port.as_ptr(), &hints, &mut res) };
    let owned = Owned(res);
    if rc != 0 {
        // Python raises the errno's own OSError for EAI_SYSTEM, a gaierror for the rest
        if rc == libc::EAI_SYSTEM {
            return Err(io::Error::last_os_error());
        }
        return Err(io::Error::other(GaiError(rc)));
    }
    if owned.0.is_null() {
        return Err(io::Error::other(GaiError(libc::EAI_NONAME)));
    }
    // SAFETY: getaddrinfo succeeded, so the first entry and its sockaddr are valid until `owned` drops.
    let first = unsafe { &*owned.0 };
    let sa = first.ai_addr as *const libc::sockaddr;
    if sa.is_null() {
        return Err(io::Error::other(GaiError(libc::EAI_NONAME)));
    }
    match first.ai_family {
        libc::AF_INET => {
            let sin = unsafe { &*(sa as *const libc::sockaddr_in) };
            let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
            Ok(SocketAddr::V4(SocketAddrV4::new(ip, u16::from_be(sin.sin_port))))
        }
        libc::AF_INET6 => {
            let sin6 = unsafe { &*(sa as *const libc::sockaddr_in6) };
            let ip = Ipv6Addr::from(sin6.sin6_addr.s6_addr);
            Ok(SocketAddr::V6(SocketAddrV6::new(
                ip,
                u16::from_be(sin6.sin6_port),
                u32::from_be(sin6.sin6_flowinfo),
                sin6.sin6_scope_id,
            )))
        }
        _ => Err(io::Error::other(GaiError(libc::EAI_FAMILY))),
    }
}

/// `probe` with the interface list and the socket factory as parameters.
pub fn probe_with(
    addr: &Peer,
    interfaces: impl FnOnce() -> Vec<InterfaceIp>,
    make: impl FnOnce(Family) -> io::Result<Box<dyn UdpSocket>>,
) -> &'static str {
    let Some(ip) = address(addr) else { return "unknown" };
    if ip.is_loopback() || !allowed_ip(&ip, interfaces) {
        return "unknown";
    }
    let mut zone = String::new();
    if ip.is_ipv6() && LINK_LOCAL_V6.contains(&ip) {
        zone = scope(addr);
        if zone.is_empty() {
            return "unknown";
        }
    }
    let host = if zone.is_empty() { ip.to_string() } else { format!("{ip}%{zone}") };
    let family = if matches!(ip, IpAddr::V4(_)) { Family::Inet } else { Family::Inet6 };
    let outcome = numeric_sockaddr(&host, DISCARD_PORT, family).and_then(|target| {
        let mut s = make(family)?;
        s.set_blocking(false)?;
        s.connect(target)?;
        s.send(b"")?;
        Ok(())
    });
    match outcome {
        Ok(()) => "ok",
        Err(e) => {
            // `e.errno in BLOCKED`, a gaierror's EAI code included, as in Python
            if py_errno(&e).is_some_and(|code| BLOCKED.contains(&code)) {
                tracing::info!("lan: the local network probe was refused ({})", errno_name(&e));
                "blocked"
            } else {
                tracing::info!("lan: the local network probe got {}", errno_name(&e));
                "unknown"
            }
        }
    }
}

/// Can this process send to a device on the LAN? One empty datagram to the device's discard port: "ok" when it
/// left, "blocked" when macOS refused it (EHOSTUNREACH or EPERM: the Local Network privilege is off), "unknown"
/// for any other error, and without sending for an address that is not a LAN peer (loopback included, where the
/// privilege does not apply) or a link-local IPv6 address without its zone. Never fails.
pub fn probe(addr: impl Into<Peer>) -> &'static str {
    probe_with(&addr.into(), interface_ips, udp)
}

#[cfg(test)]
mod tests;
