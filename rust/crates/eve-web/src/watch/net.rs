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

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use super::FetchError;

/// One network (an interface's address and prefix length), as `ipaddress.ip_network(..., strict=False)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpNet {
    pub addr: IpAddr,
    pub prefix: u8,
}

impl IpNet {
    /// `ip in net` (same family only).
    pub fn contains(&self, ip: IpAddr) -> bool {
        let _ = ip;
        todo!("watch: ip in network")
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

impl AddressRules {
    /// getaddrinfo, `_ip_public` with this Mac's interfaces, `_public`.
    pub fn system() -> Self {
        todo!("watch: the system address rules")
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

/// `_ip_public(ip)` with `nets` as this Mac's interface networks (`_local_networks()`; None when they cannot be
/// read): a public unicast address. An IPv6 form that carries an IPv4 one inside is judged by that IPv4 address
/// (mapped, 6to4, Teredo refused); NAT64 and the old IPv4-compatible form are refused outright. `ipaddress`'s
/// `is_global` table and `is_multicast` are reproduced. Every address inside a prefix on one of this Mac's own
/// interfaces is the LAN; when the interfaces cannot be read, no global IPv6 address is trusted.
pub fn ip_public(ip: IpAddr, nets: Option<&[IpNet]>) -> bool {
    let _ = (ip, nets);
    todo!("watch._ip_public")
}

/// `_ip_public(ip)` with this Mac's interfaces (cached a minute).
pub fn ip_public_here(ip: IpAddr) -> bool {
    let _ = ip;
    todo!("watch._ip_public")
}

/// `_local_networks()`: the networks on this Mac's interfaces (`/sbin/ifconfig`: inet with netmask, inet6 with
/// prefixlen), cached for LOCAL_NETS_SECS; None when they cannot be read.
pub fn local_networks() -> Option<Vec<IpNet>> {
    todo!("watch._local_networks")
}

/// `_local_networks`'s parse of `ifconfig`'s output (None when it names none).
pub fn parse_ifconfig(out: &str) -> Option<Vec<IpNet>> {
    let _ = out;
    todo!("watch._local_networks: the parse")
}

/// `_public(host)`: true when every address the host resolves to is a public one.
pub fn public(host: &str, rules: &AddressRules) -> bool {
    let _ = (host, rules);
    todo!("watch._public")
}

/// `_connect_public((host, port), timeout)`: socket.create_connection, but the address connected to is the address
/// checked: one lookup, refused (FetchError "that link goes to a private or unknown address") unless every answer
/// is public, then a connection to one of those very addresses. Used by the render's guard proxy.
pub fn connect_public(host: &str, port: u16, timeout: f64, rules: &AddressRules) -> Result<std::net::TcpStream, ConnectError> {
    let _ = (host, port, timeout, rules);
    todo!("watch._connect_public")
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
    let _ = (host, port, rules);
    todo!("watch._PinnedHTTP")
}

/// "" when `url` may be fetched, else why not: http(s) only, no credentials, and a public host (`resolve`,
/// default `_public`).
pub fn check_url(url: &str, resolve: Option<&dyn Fn(&str) -> bool>) -> String {
    let _ = (url, resolve);
    todo!("watch.check_url")
}

/// `check_url` under `rules` (its `resolve`, else `_public` over its lookup).
pub fn check_url_with(url: &str, rules: &AddressRules) -> String {
    let _ = (url, rules);
    todo!("watch.check_url")
}

/// `_origin(url)`: scheme, lowercased host, port (443/80 by scheme when absent).
pub fn origin(url: &str) -> (String, String, u16) {
    let _ = url;
    todo!("watch._origin")
}

/// `_GuardedRedirects.redirect_request`'s check of the next hop: Err "redirected somewhere not allowed (why)"
/// when `url_guard` or `check_url` refuses it.
pub fn check_redirect(newurl: &str, url_guard: Option<&UrlGuard>, rules: &AddressRules) -> Result<(), FetchError> {
    let _ = (newurl, url_guard, rules);
    todo!("watch._GuardedRedirects")
}

/// The headers a redirect keeps: all of them to the same origin, only REDIRECT_SAFE_HEADERS to another (no key,
/// OpenRouter's Authorization, TinyFish's X-API-Key, may follow one to another host, port or scheme).
pub fn redirect_headers(headers: &[(String, String)], from: &str, to: &str) -> Vec<(String, String)> {
    let _ = (headers, from, to);
    todo!("watch._GuardedRedirects: the headers")
}

/// `_retry_after(headers)`: seconds to hold off: Retry-After (seconds, at most 12 digits, or an HTTP date), else a
/// Rate-Limit-Reset in epoch milliseconds (Ticketmaster's quota header). `get` reads one header; `now` is
/// `time.time()`.
pub fn retry_after(get: &dyn Fn(&str) -> Option<String>, now: f64) -> Option<f64> {
    let _ = (get, now);
    todo!("watch._retry_after")
}

/// One GET (or POST with `opts.data`): (status, body as text). FetchError for anything not 2xx, with the status
/// and Retry-After, and for a body past MAX_BYTES. The URL is checked here too, so no caller can skip the guard.
/// An optional additional URL restriction also applies to every redirect. Blocking: run it off the loop.
pub fn http_request(url: &str, opts: &HttpOptions) -> Result<(u16, String), FetchError> {
    let _ = (url, opts);
    todo!("watch.http_request")
}

/// `_exchange`'s body decoding: `charset` (a name the codec registry does not know, or one that is not a text
/// codec, falls back to UTF-8), errors replaced.
pub fn decode_body(body: &[u8], charset: Option<&str>) -> String {
    let _ = (body, charset);
    todo!("watch._exchange: the charset")
}
