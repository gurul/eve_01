//! watch.py's network section: the guarded HTTP every watcher, TinyFish and Firecrawl request goes through.
//!
//! The URL is checked before the request (http(s), no credentials, a public host), the connection goes to the very
//! address that one lookup checked (no second lookup to rebind; verification/Eve/WatchSsrf.lean), every redirect
//! is checked the same way and stripped of every header but the plain ones when it changes origin, no proxy from
//! the environment is used, and a deadline covers the whole answer (a server trickling a byte a second is cut).
//! Python does this with urllib and pinned http.client connections; here a blocking reqwest client with a pinning
//! resolver (`dns_resolver`) and redirects followed by hand.
//!
//! The address rules are seams ([`AddressRules`]): the tests' loopback servers pass them as Python's tests pass
//! `watch.check_url` / `watch._ip_public`, and the DNS-rebinding replay lends a lookup that answers differently
//! the second time.

use std::collections::HashMap;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use regex::Regex;

use super::{FetchError, MAX_BYTES, USER_AGENT};

/// One network (an interface's address and prefix length), as `ipaddress.ip_network(..., strict=False)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpNet {
    pub addr: IpAddr,
    pub prefix: u8,
}

fn v4_in(ip: Ipv4Addr, net: Ipv4Addr, prefix: u8) -> bool {
    let prefix = prefix.min(32) as u32;
    let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix) };
    (u32::from(ip) & mask) == (u32::from(net) & mask)
}

fn v6_in(ip: Ipv6Addr, net: Ipv6Addr, prefix: u8) -> bool {
    let prefix = prefix.min(128) as u32;
    let mask = if prefix == 0 { 0 } else { u128::MAX << (128 - prefix) };
    (u128::from(ip) & mask) == (u128::from(net) & mask)
}

impl IpNet {
    /// `ip in net` (same family only).
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip) {
            (IpAddr::V4(net), IpAddr::V4(a)) => v4_in(a, net, self.prefix),
            (IpAddr::V6(net), IpAddr::V6(a)) => v6_in(a, net, self.prefix),
            _ => false,
        }
    }
}

/// `socket.getaddrinfo(host, port, SOCK_STREAM)`: every address, in the resolver's order.
pub type Lookup = Arc<dyn Fn(&str, u16) -> std::io::Result<Vec<SocketAddr>> + Send + Sync>;
/// `_ip_public(ip)`.
pub type IpPublic = Arc<dyn Fn(IpAddr) -> bool + Send + Sync>;
/// `check_url`'s `resolve=` (host -> every address public).
pub type Resolve = Arc<dyn Fn(&str) -> bool + Send + Sync>;
/// `url_guard(url) -> "" or why not`: an additional URL restriction, applied to every redirect too.
pub type UrlGuard = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// The address checks the network layer makes, as one value (Python's module functions `_public`, `_ip_public`,
/// `socket.getaddrinfo`, which its tests monkeypatch). `AddressRules::system()` is the real one.
#[derive(Clone)]
pub struct AddressRules {
    pub lookup: Lookup,
    pub ip_public: IpPublic,
    /// `check_url`'s host test; None is `_public` over `lookup` and `ip_public`.
    pub resolve: Option<Resolve>,
}

/// getaddrinfo with SOCK_STREAM hints (std's lookup), the bare host: brackets and a zone are not part of a name.
fn system_lookup(host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
    Ok((host, port).to_socket_addrs()?.collect())
}

impl AddressRules {
    /// getaddrinfo, `_ip_public` with this Mac's interfaces, `_public`.
    pub fn system() -> Self {
        AddressRules { lookup: Arc::new(system_lookup), ip_public: Arc::new(ip_public_here), resolve: None }
    }
}

/// `http_request`'s keyword arguments. `Default` is a GET with `TIMEOUT_SECS`, no extra headers, no guard, and
/// the system address rules.
#[derive(Clone)]
pub struct HttpOptions {
    /// POST with this body; None is a GET.
    pub data: Option<Vec<u8>>,
    /// merged over the default User-Agent, Accept and Accept-Language
    pub headers: Vec<(String, String)>,
    pub timeout: f64,
    pub url_guard: Option<UrlGuard>,
    /// None is `AddressRules::system()`.
    pub rules: Option<AddressRules>,
}

impl Default for HttpOptions {
    fn default() -> Self {
        HttpOptions { data: None, headers: Vec::new(), timeout: super::TIMEOUT_SECS, url_guard: None, rules: None }
    }
}

impl std::fmt::Debug for HttpOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpOptions")
            .field("data", &self.data.as_ref().map(|d| d.len()))
            .field("headers", &self.headers)
            .field("timeout", &self.timeout)
            .field("url_guard", &self.url_guard.is_some())
            .finish()
    }
}

impl HttpOptions {
    /// `headers.get(name)`, case-insensitive.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// `LOCAL_NETS_SECS`: the interfaces are read at most once a minute.
pub const LOCAL_NETS_SECS: f64 = 60.0;
/// The request headers that may follow a redirect to another origin; every other one is dropped (review A7).
/// (urllib's capitalize()d names.)
pub const REDIRECT_SAFE_HEADERS: [&str; 5] = ["Accept", "Accept-language", "Accept-encoding", "User-agent", "Content-type"];

const PRIVATE_ADDRESS: &str = "that link goes to a private or unknown address";
const TOO_LONG: &str = "the site took too long to answer";
const UNREACHABLE: &str = "the site could not be reached";
const UNREADABLE: &str = "the site sent an unreadable answer";

// ---- urllib.parse.urlsplit ----------------------------------------------------------------------------------

/// `urllib.parse.urlsplit(url)` (Python 3.13): leading C0 controls and spaces stripped, tabs and newlines removed,
/// a lowercased scheme only when every character before the first ":" is a scheme character, the netloc after
/// "//" up to "/", "?" or "#", and Python's bracket checks (ValueError: `Err`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SplitUrl {
    pub scheme: String,
    pub netloc: String,
    pub path: String,
    pub query: String,
    pub fragment: String,
}

impl SplitUrl {
    fn hostinfo(&self) -> (String, Option<String>) {
        let hostinfo = self.netloc.rsplit_once('@').map(|(_, h)| h).unwrap_or(&self.netloc);
        let (hostname, port) = match hostinfo.split_once('[') {
            Some((_, bracketed)) => {
                let (host, rest) = bracketed.split_once(']').unwrap_or((bracketed, ""));
                (host, rest.split_once(':').map(|(_, p)| p).unwrap_or(""))
            }
            None => hostinfo.split_once(':').unwrap_or((hostinfo, "")),
        };
        (hostname.to_string(), (!port.is_empty()).then(|| port.to_string()))
    }

    /// `.hostname`: lowercased, a zone kept as written; None when empty.
    pub fn hostname(&self) -> Option<String> {
        let (host, _) = self.hostinfo();
        if host.is_empty() {
            return None;
        }
        Some(match host.split_once('%') {
            Some((h, zone)) => format!("{}%{zone}", h.to_lowercase()),
            None => host.to_lowercase(),
        })
    }

    /// `.port`: Err for a port that is not ASCII digits or past 65535 (Python's ValueError).
    pub fn port(&self) -> Result<Option<u16>, BadUrl> {
        match self.hostinfo().1 {
            None => Ok(None),
            Some(p) if p.bytes().all(|c| c.is_ascii_digit()) => match p.parse::<u64>() {
                Ok(n) if n <= 65535 => Ok(Some(n as u16)),
                _ => Err(BadUrl),
            },
            Some(_) => Err(BadUrl),
        }
    }

    fn userinfo(&self) -> (Option<String>, Option<String>) {
        match self.netloc.rsplit_once('@') {
            Some((info, _)) => match info.split_once(':') {
                Some((u, p)) => (Some(u.to_string()), Some(p.to_string())),
                None => (Some(info.to_string()), None),
            },
            None => (None, None),
        }
    }

    /// `.username`.
    pub fn username(&self) -> Option<String> {
        self.userinfo().0
    }

    /// `.password`.
    pub fn password(&self) -> Option<String> {
        self.userinfo().1
    }
}

fn is_ipv6_literal(host: &str) -> bool {
    let bare = host.split_once('%').map(|(h, _)| h).unwrap_or(host);
    bare.parse::<Ipv6Addr>().is_ok()
}

/// urlsplit's or `.port`'s ValueError.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadUrl;

/// `urllib.parse.urlsplit`.
pub fn urlsplit(url: &str) -> Result<SplitUrl, BadUrl> {
    let url = url.trim_start_matches(|c: char| (c as u32) <= 0x20);
    let url: String = url.chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let mut rest = url.as_str();
    let mut parts = SplitUrl::default();
    if let Some(i) = rest.find(':') {
        let first = rest.chars().next().unwrap_or(' ');
        if i > 0 && first.is_ascii_alphabetic() && rest[..i].chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) {
            parts.scheme = rest[..i].to_ascii_lowercase();
            rest = &rest[i + 1..];
        }
    }
    if let Some(after) = rest.strip_prefix("//") {
        let delim = after.find(['/', '?', '#']).unwrap_or(after.len());
        parts.netloc = after[..delim].to_string();
        rest = &after[delim..];
        let open = parts.netloc.contains('[');
        let close = parts.netloc.contains(']');
        if open != close {
            return Err(BadUrl); // "Invalid IPv6 URL"
        }
        if open && close {
            let bracketed = parts.netloc.split_once('[').map(|(_, b)| b).unwrap_or("");
            let host = bracketed.split_once(']').map(|(h, _)| h).unwrap_or(bracketed);
            if let Some(future) = host.strip_prefix('v') {
                let ok = future.split_once('.').is_some_and(|(hex, tail)| {
                    !hex.is_empty() && hex.bytes().all(|c| c.is_ascii_hexdigit()) && !tail.is_empty()
                });
                if !ok {
                    return Err(BadUrl);
                }
            } else if !is_ipv6_literal(host) {
                return Err(BadUrl); // not an address, or an IPv4 one in brackets
            }
        }
    }
    let mut rest = rest.to_string();
    if let Some((before, frag)) = rest.split_once('#') {
        parts.fragment = frag.to_string();
        rest = before.to_string();
    }
    if let Some((before, query)) = rest.split_once('?') {
        parts.query = query.to_string();
        rest = before.to_string();
    }
    parts.path = rest;
    Ok(parts)
}

// ---- the address rules --------------------------------------------------------------------------------------

/// `ipaddress`'s IPv4 `_private_networks` (Python 3.13), minus `_private_networks_exceptions`.
const V4_PRIVATE: [([u8; 4], u8); 14] = [
    ([0, 0, 0, 0], 8),
    ([10, 0, 0, 0], 8),
    ([127, 0, 0, 0], 8),
    ([169, 254, 0, 0], 16),
    ([172, 16, 0, 0], 12),
    ([192, 0, 0, 0], 24),
    ([192, 0, 0, 170], 31),
    ([192, 0, 2, 0], 24),
    ([192, 168, 0, 0], 16),
    ([198, 18, 0, 0], 15),
    ([198, 51, 100, 0], 24),
    ([203, 0, 113, 0], 24),
    ([240, 0, 0, 0], 4),
    ([255, 255, 255, 255], 32),
];
const V4_EXCEPTIONS: [([u8; 4], u8); 2] = [([192, 0, 0, 9], 32), ([192, 0, 0, 10], 32)];

/// `ipaddress`'s IPv6 `_private_networks` (Python 3.13), minus `_private_networks_exceptions`.
const V6_PRIVATE: [(u128, u8); 11] = [
    (0x0000_0000_0000_0000_0000_0000_0000_0001, 128),
    (0, 128),
    (0x0000_0000_0000_0000_0000_ffff_0000_0000, 96),
    (0x0064_ff9b_0001_0000_0000_0000_0000_0000, 48),
    (0x0100_0000_0000_0000_0000_0000_0000_0000, 64),
    (0x2001_0000_0000_0000_0000_0000_0000_0000, 23),
    (0x2001_0db8_0000_0000_0000_0000_0000_0000, 32),
    (0x2002_0000_0000_0000_0000_0000_0000_0000, 16),
    (0x3fff_0000_0000_0000_0000_0000_0000_0000, 20),
    (0xfc00_0000_0000_0000_0000_0000_0000_0000, 7),
    (0xfe80_0000_0000_0000_0000_0000_0000_0000, 10),
];
const V6_EXCEPTIONS: [(u128, u8); 6] = [
    (0x2001_0001_0000_0000_0000_0000_0000_0001, 128),
    (0x2001_0001_0000_0000_0000_0000_0000_0002, 128),
    (0x2001_0003_0000_0000_0000_0000_0000_0000, 32),
    (0x2001_0004_0112_0000_0000_0000_0000_0000, 48),
    (0x2001_0020_0000_0000_0000_0000_0000_0000, 28),
    (0x2001_0030_0000_0000_0000_0000_0000_0000, 28),
];

fn v4_global(ip: Ipv4Addr) -> bool {
    let private = V4_PRIVATE.iter().any(|(n, p)| v4_in(ip, Ipv4Addr::from(*n), *p))
        && V4_EXCEPTIONS.iter().all(|(n, p)| !v4_in(ip, Ipv4Addr::from(*n), *p));
    !v4_in(ip, Ipv4Addr::new(100, 64, 0, 0), 10) && !private
}

fn v6_global(ip: Ipv6Addr) -> bool {
    let private = V6_PRIVATE.iter().any(|(n, p)| v6_in(ip, Ipv6Addr::from(*n), *p))
        && V6_EXCEPTIONS.iter().all(|(n, p)| !v6_in(ip, Ipv6Addr::from(*n), *p));
    !private
}

/// `_ip_public(ip)` with `nets` as this Mac's interface networks (`_local_networks()`; None when they cannot be
/// read): a public unicast address. An IPv6 form that carries an IPv4 one inside is judged by that IPv4 address
/// (mapped, 6to4, Teredo refused); NAT64 and the old IPv4-compatible form are refused outright. `ipaddress`'s
/// `is_global` table and `is_multicast` are reproduced. Every address inside a prefix on one of this Mac's own
/// interfaces is the LAN; when the interfaces cannot be read, no global IPv6 address is trusted.
pub fn ip_public(ip: IpAddr, nets: Option<&[IpNet]>) -> bool {
    // NAT64 (64:ff9b::/96, where a DNS64 gateway would translate it into a private IPv4) and the old
    // IPv4-compatible form (::a.b.c.d) are refused outright, since ipaddress calls both global (security review,
    // 2026-09-25).
    if let IpAddr::V6(v6) = ip {
        let bits = u128::from(v6);
        let nat64 = v6_in(v6, Ipv6Addr::from(0x0064_ff9b_0000_0000_0000_0000_0000_0000u128), 96)
            || v6_in(v6, Ipv6Addr::from(0x0064_ff9b_0001_0000_0000_0000_0000_0000u128), 48);
        let compat = v6_in(v6, Ipv6Addr::UNSPECIFIED, 96);
        let teredo = (bits >> 96) == 0x2001_0000;
        if nat64 || compat || teredo {
            return false;
        }
        if (bits >> 32) == 0xffff {
            return ip_public(IpAddr::V4(Ipv4Addr::from(bits as u32)), nets);
        }
        if (bits >> 112) == 0x2002 {
            return ip_public(IpAddr::V4(Ipv4Addr::from(((bits >> 80) & 0xffff_ffff) as u32)), nets);
        }
    }
    let (global, multicast) = match ip {
        IpAddr::V4(v4) => (v4_global(v4), v4_in(v4, Ipv4Addr::new(224, 0, 0, 0), 4)),
        IpAddr::V6(v6) => (v6_global(v6), (u128::from(v6) >> 120) == 0xff),
    };
    if !global || multicast {
        return false;
    }
    // A home network's IPv6 devices, this Mac among them, have global addresses: ipaddress calls them public, and
    // a page could reach them (re-verification, 2026-09-25).
    match nets {
        None => ip.is_ipv4(),
        Some(nets) => !nets.iter().any(|n| n.contains(ip)),
    }
}

/// `_ip_public(ip)` with this Mac's interfaces (cached a minute).
pub fn ip_public_here(ip: IpAddr) -> bool {
    let nets = local_networks();
    ip_public(ip, nets.as_deref())
}

/// The interfaces' networks, and when they were read.
type NetsCache = Option<(Instant, Vec<IpNet>)>;
static LOCAL_NETS: LazyLock<Mutex<NetsCache>> = LazyLock::new(|| Mutex::new(None));

/// `subprocess.run(cmd, capture_output=True, text=True, timeout=secs).stdout`; None when it cannot run or runs
/// past the timeout.
pub(crate) fn run_stdout(program: &str, args: &[&str], secs: f64) -> Option<String> {
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        out
    });
    let deadline = Instant::now() + Duration::from_secs_f64(secs);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let out = reader.join().ok()?;
    Some(String::from_utf8_lossy(&out).into_owned())
}

/// `_local_networks()`: the networks on this Mac's interfaces (`/sbin/ifconfig`: inet with netmask, inet6 with
/// prefixlen), cached for LOCAL_NETS_SECS; None when they cannot be read.
pub fn local_networks() -> Option<Vec<IpNet>> {
    {
        let cached = LOCAL_NETS.lock();
        if let Some((at, nets)) = cached.as_ref() {
            if at.elapsed().as_secs_f64() < LOCAL_NETS_SECS {
                return Some(nets.clone());
            }
        }
    }
    let out = run_stdout("/sbin/ifconfig", &[], 5.0)?;
    let found = parse_ifconfig(&out)?;
    *LOCAL_NETS.lock() = Some((Instant::now(), found.clone()));
    Some(found)
}

static INET6: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\binet6 ([0-9a-fA-F:]+)(?:%\S+)? prefixlen (\d+)").expect("a valid pattern"));
static INET4: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\binet (\d+\.\d+\.\d+\.\d+) netmask 0x([0-9a-fA-F]{8})").expect("a valid pattern")
});

/// `_local_networks`'s parse of `ifconfig`'s output (None when it names none).
pub fn parse_ifconfig(out: &str) -> Option<Vec<IpNet>> {
    let mut found = Vec::new();
    for c in INET6.captures_iter(out) {
        let (Ok(addr), Ok(prefix)) = (c[1].parse::<Ipv6Addr>(), c[2].parse::<u32>()) else { continue };
        if prefix > 128 {
            continue;
        }
        found.push(IpNet { addr: IpAddr::V6(addr), prefix: prefix as u8 });
    }
    for c in INET4.captures_iter(out) {
        let (Ok(addr), Ok(mask)) = (c[1].parse::<Ipv4Addr>(), u32::from_str_radix(&c[2], 16)) else { continue };
        found.push(IpNet { addr: IpAddr::V4(addr), prefix: mask.count_ones() as u8 });
    }
    (!found.is_empty()).then_some(found)
}

/// `_public(host)`: true when every address the host resolves to is a public one.
pub fn public(host: &str, rules: &AddressRules) -> bool {
    match (rules.lookup)(host, 0) {
        Ok(addrs) if !addrs.is_empty() => addrs.iter().all(|a| (rules.ip_public)(a.ip())),
        _ => false,
    }
}

/// `_connect_public((host, port), timeout)`: socket.create_connection, but the address connected to is the address
/// checked: one lookup, refused (FetchError "that link goes to a private or unknown address") unless every answer
/// is public, then a connection to one of those very addresses. Used by the render's guard proxy.
pub fn connect_public(host: &str, port: u16, timeout: f64, rules: &AddressRules) -> Result<TcpStream, ConnectError> {
    // check_url's own lookup and http.client's were two, so a name answering a public address to the first and
    // 127.0.0.1 to the second (DNS rebinding, TTL 0) reached the LAN (security review, 2026-09-25;
    // verification/Eve/WatchSsrf.lean).
    let addrs = pinned_addrs(host, port, rules)?;
    let limit = Duration::from_secs_f64(if timeout.is_finite() && timeout > 0.0 { timeout } else { 1e9 });
    let mut err: Option<std::io::Error> = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, limit) {
            Ok(sock) => {
                // socket.settimeout: every later read and write too
                let _ = sock.set_read_timeout(Some(limit));
                let _ = sock.set_write_timeout(Some(limit));
                return Ok(sock);
            }
            Err(e) => err = Some(e),
        }
    }
    Err(ConnectError::Io(err.unwrap_or_else(|| std::io::Error::other("no address to connect to"))))
}

/// `_connect_public`'s two failures: the guard's refusal, or the OS's.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error(transparent)]
    Refused(#[from] FetchError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// `_pinned(host, port)`: the addresses one lookup gave, every one public, for the pinning resolver
/// (`_PinnedHTTP` / `_PinnedHTTPS`' `_create_connection`).
pub fn pinned_addrs(host: &str, port: u16, rules: &AddressRules) -> Result<Vec<SocketAddr>, ConnectError> {
    let addrs = (rules.lookup)(host, port)
        .map_err(|e| ConnectError::Io(std::io::Error::other(format!("no address for the host ({:?})", e.kind()))))?;
    for a in &addrs {
        if !(rules.ip_public)(a.ip()) {
            return Err(ConnectError::Refused(FetchError::new(PRIVATE_ADDRESS)));
        }
    }
    Ok(addrs)
}

/// "" when `url` may be fetched, else why not: http(s) only, no credentials, and a public host (`resolve`,
/// default `_public`).
pub fn check_url(url: &str, resolve: Option<&dyn Fn(&str) -> bool>) -> String {
    match resolve {
        Some(resolve) => check_url_by(url, resolve),
        None => {
            let rules = AddressRules::system();
            check_url_by(url, &|h: &str| public(h, &rules))
        }
    }
}

fn check_url_by(url: &str, resolve: &dyn Fn(&str) -> bool) -> String {
    let Ok(parts) = urlsplit(url) else { return "that is not a web link".into() };
    let Some(host) = parts.hostname().filter(|_| parts.scheme == "http" || parts.scheme == "https") else {
        return "only http and https links can be watched".into();
    };
    if parts.username().is_some_and(|u| !u.is_empty()) || parts.password().is_some_and(|p| !p.is_empty()) {
        return "a link with a password in it is not watched".into();
    }
    if !resolve(&host) {
        return PRIVATE_ADDRESS.into();
    }
    String::new()
}

/// `check_url` under `rules` (its `resolve`, else `_public` over its lookup).
pub fn check_url_with(url: &str, rules: &AddressRules) -> String {
    match &rules.resolve {
        Some(resolve) => check_url_by(url, &|h: &str| resolve(h)),
        None => check_url_by(url, &|h: &str| public(h, rules)),
    }
}

/// `_origin(url)`: scheme, lowercased host, port (443/80 by scheme when absent).
pub fn origin(url: &str) -> (String, String, u16) {
    let Ok(parts) = urlsplit(url) else { return (String::new(), String::new(), 0) };
    let default = if parts.scheme == "https" { 443 } else { 80 };
    let port = parts.port().ok().flatten().filter(|p| *p != 0).unwrap_or(default);
    (parts.scheme.clone(), parts.hostname().unwrap_or_default().to_lowercase(), port)
}

/// `_GuardedRedirects.redirect_request`'s check of the next hop: Err "redirected somewhere not allowed (why)"
/// when `url_guard` or `check_url` refuses it.
pub fn check_redirect(newurl: &str, url_guard: Option<&UrlGuard>, rules: &AddressRules) -> Result<(), FetchError> {
    // A redirect is checked like the first URL: a public page cannot bounce the fetch onto the LAN.
    if let Some(guard) = url_guard {
        let why = guard(newurl);
        if !why.is_empty() {
            return Err(FetchError::new(format!("redirected somewhere not allowed ({why})")));
        }
    }
    let why = check_url_with(newurl, rules);
    if !why.is_empty() {
        return Err(FetchError::new(format!("redirected somewhere not allowed ({why})")));
    }
    Ok(())
}

/// Python's `str.capitalize()` of an ASCII header name.
fn capitalize(name: &str) -> String {
    let lower = name.to_lowercase();
    let mut chars = lower.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The headers a redirect keeps: all of them to the same origin, only REDIRECT_SAFE_HEADERS to another (no key,
/// OpenRouter's Authorization, TinyFish's X-API-Key, may follow one to another host, port or scheme).
pub fn redirect_headers(headers: &[(String, String)], from: &str, to: &str) -> Vec<(String, String)> {
    // urllib copies every header onto a redirect: no key may follow one to another host, another port, or down
    // from https to http. Only the plain request headers cross.
    if origin(to) == origin(from) {
        return headers.to_vec();
    }
    headers.iter().filter(|(k, _)| REDIRECT_SAFE_HEADERS.contains(&capitalize(k).as_str())).cloned().collect()
}

/// `_retry_after(headers)`: seconds to hold off: Retry-After (seconds, at most 12 digits, or an HTTP date), else a
/// Rate-Limit-Reset in epoch milliseconds (Ticketmaster's quota header). `get` reads one header; `now` is
/// `time.time()`.
pub fn retry_after(get: &dyn Fn(&str) -> Option<String>, now: f64) -> Option<f64> {
    // Ticketmaster sends no Retry-After (developer.ticketmaster.com).
    let raw = get("Retry-After").unwrap_or_default();
    let raw = super::page::py_strip(&raw);
    if raw.is_empty() {
        let reset = get("Rate-Limit-Reset").unwrap_or_default();
        let reset = super::page::py_strip(&reset);
        if !reset.is_empty() && reset.bytes().all(|c| c.is_ascii_digit()) && reset.len() <= 16 {
            let ms: f64 = reset.parse().ok()?;
            return Some((ms / 1000.0 - now).max(0.0));
        }
        return None;
    }
    if raw.bytes().all(|c| c.is_ascii_digit()) {
        // "²".isdigit() is True, and float("²") raises: ASCII digits only
        return if raw.len() <= 12 { raw.parse().ok() } else { None };
    }
    // email.utils drops the day name unread, as RFC 5322 lets a reader; chrono would check it against the date
    let dated = match raw.split_once(',') {
        Some((day, rest)) if day.trim().chars().all(|c| c.is_ascii_alphabetic()) => rest.trim(),
        _ => raw,
    };
    let when = chrono::DateTime::parse_from_rfc2822(dated).ok()?;
    let at = when.timestamp() as f64 + f64::from(when.timestamp_subsec_micros()) / 1e6;
    Some((at - now).max(0.0))
}

/// The resolver every hop's client uses: the one lookup, refused unless every answer is public, and only those
/// addresses connected to.
struct Pinned {
    rules: AddressRules,
    port: u16,
}

impl reqwest::dns::Resolve for Pinned {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let found = pinned_addrs(name.as_str(), self.port, &self.rules);
        let port = self.port;
        Box::pin(async move {
            match found {
                Ok(addrs) => {
                    let addrs: Vec<SocketAddr> = addrs.into_iter().map(|mut a| {
                        a.set_port(port);
                        a
                    }).collect();
                    Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
                }
                Err(ConnectError::Refused(f)) => Err(Box::new(f) as Box<dyn std::error::Error + Send + Sync>),
                Err(ConnectError::Io(e)) => Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>),
            }
        })
    }
}

fn fetch_error_in(e: &(dyn std::error::Error + 'static)) -> Option<FetchError> {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(err) = cur {
        if let Some(f) = err.downcast_ref::<FetchError>() {
            return Some(f.clone());
        }
        cur = err.source();
    }
    None
}

/// A reqwest failure as `_exchange` names it: the guard's own refusal as it was; a timeout, or anything after the
/// deadline fired, is too long; a malformed answer (a bad status line, a header past its limit) is unreadable;
/// anything else (refused, reset, no address) could not be reached.
fn exchange_error(e: &reqwest::Error, deadline: Instant) -> FetchError {
    if let Some(f) = fetch_error_in(e) {
        return f;
    }
    if e.is_timeout() || Instant::now() >= deadline {
        return FetchError::new(TOO_LONG);
    }
    if e.is_builder() {
        return FetchError::new(UNREADABLE);
    }
    if e.is_connect() {
        return FetchError::new(UNREACHABLE);
    }
    let chain = format!("{e:?}");
    if chain.contains("Parse(") || chain.contains("TooLarge") || chain.contains("invalid HTTP") {
        return FetchError::new(UNREADABLE);
    }
    FetchError::new(UNREACHABLE)
}

fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

fn header_of(headers: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
    headers.get(name).map(|v| latin1(v.as_bytes()))
}

/// `Message.get_content_charset()`: the first charset parameter, unquoted and lowercased.
fn content_charset(content_type: &str) -> Option<String> {
    for param in content_type.split(';').skip(1) {
        let Some((name, value)) = param.split_once('=') else { continue };
        if name.trim().eq_ignore_ascii_case("charset") {
            let value = value.trim().trim_matches('"').trim();
            if !value.is_ascii() {
                return None;
            }
            return Some(value.to_ascii_lowercase());
        }
    }
    None
}

/// One hop: the request on a client of its own, pinned to the addresses its lookup checked.
fn send_hop(
    url: &str,
    data: Option<&[u8]>,
    headers: &[(String, String)],
    deadline: Instant,
    rules: &AddressRules,
) -> Result<reqwest::blocking::Response, FetchError> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(FetchError::new(TOO_LONG));
    }
    let parsed = reqwest::Url::parse(url).map_err(|_| FetchError::new(UNREACHABLE))?;
    let (_, host, port) = origin(url);
    // An address literal is connected without a lookup: check it here, as _connect_public checks every address.
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let bare = bare.split_once('%').map(|(h, _)| h).unwrap_or(bare);
    if bare.parse::<IpAddr>().is_ok() {
        match pinned_addrs(bare, port, rules) {
            Ok(_) => {}
            Err(ConnectError::Refused(f)) => return Err(f),
            Err(ConnectError::Io(_)) => return Err(FetchError::new(UNREACHABLE)),
        }
    }
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .redirect(reqwest::redirect::Policy::none())
        .pool_max_idle_per_host(0)
        .dns_resolver(Arc::new(Pinned { rules: rules.clone(), port }))
        .timeout(left)
        .build()
        .map_err(|_| FetchError::new(UNREACHABLE))?;
    let method = if data.is_some() { reqwest::Method::POST } else { reqwest::Method::GET };
    let mut req = client.request(method, parsed);
    let has = |name: &str| headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name));
    for (k, v) in headers {
        req = req.header(k.as_str(), v.as_str());
    }
    // http.client's own: no compression asked for, one request per connection, a form body by default
    if !has("Accept-Encoding") {
        req = req.header("Accept-Encoding", "identity");
    }
    req = req.header("Connection", "close");
    if let Some(body) = data {
        if !has("Content-Type") {
            req = req.header("Content-Type", "application/x-www-form-urlencoded");
        }
        req = req.body(body.to_vec());
    }
    req.send().map_err(|e| exchange_error(&e, deadline))
}

/// One GET (or POST with `opts.data`): (status, body as text). FetchError for anything not 2xx, with the status
/// and Retry-After, and for a body past MAX_BYTES. The URL is checked here too, so no caller can skip the guard.
/// An optional additional URL restriction also applies to every redirect. Blocking: run it off the loop.
pub fn http_request(url: &str, opts: &HttpOptions) -> Result<(u16, String), FetchError> {
    let rules = opts.rules.clone().unwrap_or_else(AddressRules::system);
    if let Some(guard) = &opts.url_guard {
        let why = guard(url);
        if !why.is_empty() {
            return Err(FetchError::new(why));
        }
    }
    let why = check_url_with(url, &rules);
    if !why.is_empty() {
        return Err(FetchError::new(why));
    }
    // urllib keys headers by name.capitalize(): a caller's header replaces a default of any case.
    let mut headers: Vec<(String, String)> = vec![
        ("User-Agent".into(), USER_AGENT.into()),
        ("Accept".into(), "text/html,application/json;q=0.9,*/*;q=0.8".into()),
        ("Accept-Language".into(), "en-US,en;q=0.9".into()),
    ];
    for (k, v) in &opts.headers {
        match headers.iter_mut().find(|(name, _)| name.eq_ignore_ascii_case(k)) {
            Some(slot) => *slot = (k.clone(), v.clone()),
            None => headers.push((k.clone(), v.clone())),
        }
    }
    // For the whole answer: a socket timeout is per read, and a server trickling a byte a second, in the body or
    // in the headers, never trips it (re-verification, 2026-09-25).
    let timeout = if opts.timeout.is_finite() && opts.timeout > 0.0 { opts.timeout } else { 0.0 };
    let deadline = Instant::now() + Duration::from_secs_f64(timeout.min(1e9));
    let mut current = url.to_string();
    let mut data = opts.data.clone();
    let mut visited: HashMap<String, usize> = HashMap::new();
    loop {
        let resp = send_hop(&current, data.as_deref(), &headers, deadline, &rules)?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            return read_answer(resp, status, deadline);
        }
        let hdrs = resp.headers().clone();
        let get = |name: &str| header_of(&hdrs, name);
        let refused = || {
            FetchError::with_retry(format!("the site answered HTTP {status}"), status, retry_after(&get, crate::time_time()))
        };
        if !matches!(status, 301 | 302 | 303 | 307 | 308) {
            return Err(refused());
        }
        let Some(location) = get("Location").or_else(|| get("URI")) else { return Err(refused()) };
        let Ok(base) = reqwest::Url::parse(&current) else { return Err(refused()) };
        let Ok(next) = base.join(&location) else { return Err(refused()) };
        if !matches!(next.scheme(), "http" | "https" | "ftp") {
            return Err(refused());
        }
        let newurl = next.to_string();
        check_redirect(&newurl, opts.url_guard.as_ref(), &rules)?;
        // urllib follows 301-303 as a GET, and 307/308 only for a GET (a POST answered 307 is an error)
        if data.is_some() && matches!(status, 307 | 308) {
            return Err(refused());
        }
        let kept: Vec<(String, String)> = headers
            .iter()
            .filter(|(k, _)| !k.eq_ignore_ascii_case("content-length") && !k.eq_ignore_ascii_case("content-type"))
            .cloned()
            .collect();
        headers = redirect_headers(&kept, &current, &newurl);
        let seen = visited.get(&newurl).copied().unwrap_or(0);
        if seen >= 4 || visited.len() >= 10 {
            return Err(refused()); // urllib's max_repeats and max_redirections
        }
        visited.insert(newurl.clone(), seen + 1);
        data = None;
        current = newurl;
    }
}

/// `_exchange`'s read of a 2xx answer, under the deadline and the size cap.
fn read_answer(mut resp: reqwest::blocking::Response, status: u16, deadline: Instant) -> Result<(u16, String), FetchError> {
    let charset = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| content_charset(&latin1(v.as_bytes())))
        .filter(|c| !c.is_empty());
    let sized = resp.headers().contains_key(reqwest::header::CONTENT_LENGTH);
    let mut body: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    while body.len() <= MAX_BYTES {
        if Instant::now() > deadline {
            return Err(FetchError::new(TOO_LONG));
        }
        match resp.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => body.extend_from_slice(&buf[..n]),
            Err(e) => {
                if Instant::now() >= deadline || e.kind() == std::io::ErrorKind::TimedOut {
                    return Err(FetchError::new(TOO_LONG));
                }
                if sized {
                    break; // http.client's read1 ends a body cut short; a chunked one it calls incomplete
                }
                return Err(FetchError::new(UNREADABLE));
            }
        }
    }
    if body.len() > MAX_BYTES {
        return Err(FetchError::new("the page is too big to read"));
    }
    Ok((status, decode_body(&body, charset.as_deref())))
}

/// `_exchange`'s body decoding: `charset` (a name the codec registry does not know, or one that is not a text
/// codec, falls back to UTF-8), errors replaced.
pub fn decode_body(body: &[u8], charset: Option<&str>) -> String {
    let Some(name) = charset else { return String::from_utf8_lossy(body).into_owned() };
    let lower = name.trim().to_lowercase();
    let norm = lower.replace('_', "-");
    match norm.as_str() {
        "utf-8" | "utf8" | "u8" | "utf" | "cp65001" => return String::from_utf8_lossy(body).into_owned(),
        "ascii" | "us-ascii" | "646" | "us" | "ansi-x3.4-1968" | "iso646-us" => {
            return body.iter().map(|&b| if b < 0x80 { b as char } else { '\u{fffd}' }).collect();
        }
        "latin-1" | "latin1" | "latin" | "l1" | "iso-8859-1" | "iso8859-1" | "8859" | "cp819" | "iso-ir-100"
        | "iso-8859-1:1987" | "ibm819" => return latin1(body),
        _ => {}
    }
    let enc = encoding_rs::Encoding::for_label(lower.as_bytes()).or_else(|| encoding_rs::Encoding::for_label(norm.as_bytes()));
    match enc {
        // "replacement" is WHATWG's stand-in for encodings it refuses, not a codec Python has
        Some(enc) if enc != encoding_rs::REPLACEMENT => enc.decode_without_bom_handling(body).0.into_owned(),
        _ => String::from_utf8_lossy(body).into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn the_address_table_is_pythons() {
        let none: Option<&[IpNet]> = None;
        for public in ["93.184.215.14", "8.8.8.8", "192.0.0.9", "1.1.1.1"] {
            assert!(ip_public(ip(public), Some(&[])), "{public}");
        }
        for private in [
            "127.0.0.1", "10.1.2.3", "169.254.169.254", "100.64.0.1", "192.168.1.1", "172.16.5.5", "0.0.0.0",
            "224.0.0.1", "255.255.255.255", "198.18.0.1", "::1", "::", "fe80::1", "fc00::1", "2001:db8::1",
            "::ffff:127.0.0.1", "64:ff9b::7f00:1", "::7f00:1", "2001:0::1", "2002:7f00:1::", "ff02::1",
        ] {
            assert!(!ip_public(ip(private), Some(&[])), "{private}");
        }
        // a mapped or 6to4 public IPv4 is judged by it
        assert!(ip_public(ip("::ffff:8.8.8.8"), Some(&[])));
        assert!(ip_public(ip("2002:808:808::"), none));
        // unreadable interfaces: no global IPv6 is trusted
        assert!(!ip_public(ip("2606:4700::1111"), none));
        assert!(ip_public(ip("2606:4700::1111"), Some(&[])));
        // this Mac's own prefix is the LAN
        let lan = [IpNet { addr: ip("2600:1700:abcd::"), prefix: 64 }];
        assert!(!ip_public(ip("2600:1700:abcd::42"), Some(&lan)));
        assert!(ip_public(ip("2600:1700:abce::42"), Some(&lan)));
    }

    #[test]
    fn ifconfig_is_parsed() {
        let out = "en0: flags=8863\n\tinet6 fe80::1c%en0 prefixlen 64 secured scopeid 0x4\n\tinet 10.0.0.12 netmask 0xffffff00 broadcast 10.0.0.255\n\tinet6 2600:1700::5 prefixlen 64 autoconf\n";
        let nets = parse_ifconfig(out).unwrap();
        assert_eq!(nets.len(), 3);
        assert!(nets.iter().any(|n| n.contains(ip("10.0.0.200"))));
        assert!(nets.iter().any(|n| n.contains(ip("2600:1700::9"))));
        assert!(parse_ifconfig("lo0: nothing").is_none());
    }

    #[test]
    fn urlsplit_is_pythons() {
        let p = urlsplit("  HTTP://User:Pw@Shop.Example:8080/a?b=1#c").unwrap();
        assert_eq!((p.scheme.as_str(), p.path.as_str(), p.query.as_str(), p.fragment.as_str()), ("http", "/a", "b=1", "c"));
        assert_eq!(p.hostname().as_deref(), Some("shop.example"));
        assert_eq!(p.port(), Ok(Some(8080)));
        assert_eq!((p.username().as_deref(), p.password().as_deref()), (Some("User"), Some("Pw")));
        assert_eq!(urlsplit("http://[::1]:80/").unwrap().hostname().as_deref(), Some("::1"));
        assert!(urlsplit("http://[::1/").is_err());
        assert!(urlsplit("http://[127.0.0.1]/").is_err());
        assert!(urlsplit("http://h:99999/").unwrap().port().is_err());
        assert_eq!(urlsplit("javascript:alert(1)").unwrap().scheme, "javascript");
    }

    #[test]
    fn check_url_refuses_what_python_refuses() {
        let public = |h: &str| h.ends_with(".example");
        assert_eq!(check_url("https://shop.example/p", Some(&public)), "");
        assert_eq!(check_url("file:///etc/passwd", Some(&public)), "only http and https links can be watched");
        assert_eq!(check_url("https://user:pw@shop.example/", Some(&public)), "a link with a password in it is not watched");
        assert_eq!(check_url("https://@shop.example/", Some(&public)), "");
        assert_eq!(check_url("http://[::1/", Some(&public)), "that is not a web link");
        assert_eq!(check_url("https://evil.test/", Some(&public)), PRIVATE_ADDRESS);
    }

    #[test]
    fn retry_after_reads_seconds_dates_and_resets() {
        let now = 1_800_000_000.0;
        let h = |pairs: Vec<(&'static str, &'static str)>| {
            move |name: &str| pairs.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.to_string())
        };
        assert_eq!(retry_after(&h(vec![("Retry-After", " 7 ")]), now), Some(7.0));
        assert_eq!(retry_after(&h(vec![("Retry-After", "1234567890123")]), now), None);
        assert_eq!(retry_after(&h(vec![("Retry-After", "²")]), now), None);
        assert_eq!(retry_after(&h(vec![("Rate-Limit-Reset", "1800000030000")]), now), Some(30.0));
        let date = chrono::DateTime::parse_from_rfc2822("Fri, 15 Jan 2027 08:00:00 GMT").unwrap().timestamp() as f64;
        assert_eq!(retry_after(&h(vec![("Retry-After", "Fri, 15 Jan 2027 08:00:00 GMT")]), now), Some((date - now).max(0.0)));
        assert_eq!(retry_after(&h(vec![("Retry-After", "Thu, 01 Jan 2026 00:00:00 GMT")]), now), Some(0.0));
        assert_eq!(retry_after(&h(vec![]), now), None);
    }

    #[test]
    fn redirects_keep_only_plain_headers_across_origins() {
        let hs = vec![
            ("User-Agent".to_string(), "ua".to_string()),
            ("Authorization".to_string(), "Bearer k".to_string()),
            ("X-API-Key".to_string(), "k".to_string()),
            ("accept-language".to_string(), "en".to_string()),
        ];
        assert_eq!(redirect_headers(&hs, "https://a.example/x", "https://A.example:443/y").len(), 4);
        let kept = redirect_headers(&hs, "https://a.example/x", "http://a.example/y");
        assert_eq!(kept.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), vec!["User-Agent", "accept-language"]);
    }

    #[test]
    fn bodies_decode_as_python_does() {
        assert_eq!(decode_body("prix 12 €".as_bytes(), Some("utf-8")), "prix 12 €");
        assert_eq!(decode_body(b"caf\xe9", Some("latin-1")), "café");
        assert_eq!(decode_body(b"caf\xe9", Some("windows-1252")), "café");
        assert_eq!(decode_body(b"<p>hi</p>", Some("bogus-8")), "<p>hi</p>");
        assert_eq!(decode_body(b"aGk=", Some("base64")), "aGk=");
        assert_eq!(decode_body(b"\xff", None), "\u{fffd}");
        assert_eq!(content_charset("text/html; Charset=\"ISO-8859-1\""), Some("iso-8859-1".into()));
    }
}
