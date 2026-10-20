//! IPC between hook scripts and the daemon. Port of `eve_bridge/ipc.py`.
//!
//! Protocol: line-delimited JSON, one request → one response, then close.
//!
//! Transport is resolved by [`make_transport`]:
//! * macOS / Linux → Unix domain socket (`/tmp/eve-bridge.sock`)
//! * Windows      → TCP loopback (`127.0.0.1:48765`)
//!
//! Request shapes (`evt` field discriminates):
//! ```text
//!   {"evt":"session_start","session_id":"...","transcript_path":"...","cwd":"..."}
//!   {"evt":"session_end","session_id":"..."}
//!   {"evt":"turn_begin","session_id":"...","prompt":"..."}
//!   {"evt":"turn_end","session_id":"...","summary":"..."}
//!   {"evt":"pretooluse","session_id":"...","tool_use_id":"...","tool_name":"...","hint":"..."}  ← BLOCKS
//!   {"evt":"posttooluse","session_id":"...","tool_use_id":"..."}
//!   {"evt":"lesson","action":"open|start|ideas|hint|check|step|status|recap|end","mode":"learn|help","topic":"...","level":"...","text":"..."}
//! ```
//!
//! Response shapes:
//! ```text
//!   {"ok":true}
//!   {"ok":true,"decision":"allow"|"deny"}  (for pretooluse)
//!   {"ok":true,"action":"...","answer":"...",...}  (for lesson; a refusal adds "reason" next to "error")
//!   {"ok":false,"error":"..."}
//! ```
//!
//! Replies are written the way Python's `json.dumps(obj, ensure_ascii=False)`
//! writes them (`", "` and `": "` separators, raw non-ASCII, Python float
//! repr), so both daemons put the same bytes on the socket.

use std::future::Future;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use futures::future::BoxFuture;
use futures::FutureExt;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

pub mod pyjson;
pub use pyjson::LoadsError;

// Platform defaults.
pub const DEFAULT_UNIX_PATH: &str = "/tmp/eve-bridge.sock";
pub const DEFAULT_TCP_HOST: &str = "127.0.0.1";
pub const DEFAULT_TCP_PORT: u16 = 48765;

/// The environment variable [`make_transport`] reads when no spec is given.
pub const SOCK_ENV: &str = "EVE_BRIDGE_SOCK";

/// asyncio's `StreamReader` default limit (`2 ** 16`): a request line whose
/// newline sits past this many bytes is dropped without a reply, as Python's
/// `readline()` raises on it.
pub const READ_LIMIT: usize = 65536;

/// asyncio's default listen backlog for `start_server` / `start_unix_server`.
const BACKLOG: i32 = 100;

/// asyncio's `ACCEPT_RETRY_DELAY`: the pause after an accept that failed for
/// lack of file descriptors or memory.
const ACCEPT_RETRY_DELAY: Duration = Duration::from_secs(1);

/// The `sys.platform` test the Python makes: Windows, or anything else.
/// Functions that branch on it take a `Platform` in their `_on` form so tests
/// can ask for the other platform's answer without patching globals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Windows,
    Posix,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Posix
        }
    }
}

/// A transport spec or constructor argument the Python refuses.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpecError {
    #[error("transport spec must be non-empty")]
    Empty,
    /// Carries the port as Python prints `int(port_s)`: digits, no leading zeros.
    #[error("port out of range: {0}")]
    PortOutOfRange(String),
    #[error("TCP IPC must bind to loopback only, got {}. Use '127.0.0.1:{port}' or ':{port}'.", py_str_repr(.host))]
    NotLoopback { host: String, port: u16 },
    #[error("Unix socket paths are not supported on Windows: {}. Use a TCP spec like '127.0.0.1:48765' or ':48765'.", py_str_repr(.0))]
    UnixPathOnWindows(String),
    #[error("UnixTransport is not supported on Windows. Use a TCP spec like '127.0.0.1:48765' instead.")]
    UnixUnsupported,
}

impl SpecError {
    /// The Python exception class this error is raised as.
    pub fn python_type(&self) -> &'static str {
        match self {
            SpecError::UnixUnsupported => "RuntimeError",
            _ => "ValueError",
        }
    }
}

/// AF_UNIX SOCK_STREAM transport for macOS/Linux.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnixTransport {
    pub path: String,
}

impl UnixTransport {
    pub fn new(path: impl Into<String>) -> Result<Self, SpecError> {
        Self::new_on(path, Platform::current())
    }

    pub fn new_on(path: impl Into<String>, platform: Platform) -> Result<Self, SpecError> {
        if platform == Platform::Windows {
            return Err(SpecError::UnixUnsupported);
        }
        Ok(UnixTransport { path: path.into() })
    }

    pub fn address(&self) -> String {
        self.path.clone()
    }

    #[cfg(unix)]
    async fn bind(&self) -> io::Result<Vec<Listener>> {
        use std::os::unix::fs::MetadataExt;
        let p = Path::new(&self.path);
        if p.exists() {
            let _ = std::fs::remove_file(p);
        }
        let listener = bind_unix(&self.path)?;
        // asyncio (3.13, `cleanup_socket=True`) notes the socket file's inode
        // so closing the server removes the file only if it is still this one.
        let inode = std::fs::metadata(&self.path).ok().map(|m| m.ino());
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(vec![Listener::Unix(listener, inode.map(|ino| (self.path.clone(), ino)))])
    }

    #[cfg(not(unix))]
    async fn bind(&self) -> io::Result<Vec<Listener>> {
        Err(io::Error::new(io::ErrorKind::Unsupported, SpecError::UnixUnsupported.to_string()))
    }

    /// Open a blocking client socket; `timeout` caps the connect and then every
    /// read and write, as Python's `settimeout` does.
    pub fn sync_connect(&self, timeout: Duration) -> io::Result<SyncStream> {
        #[cfg(unix)]
        {
            let stream = connect_unix(&self.path, timeout)?;
            let stream = SyncStream::Unix(stream);
            stream.set_timeout(timeout)?;
            Ok(stream)
        }
        #[cfg(not(unix))]
        {
            let _ = timeout;
            Err(io::Error::new(io::ErrorKind::Unsupported, SpecError::UnixUnsupported.to_string()))
        }
    }

    /// True iff some other process is actively accepting here. A socket file
    /// nobody accepts on is removed as stale.
    pub fn is_in_use(&self) -> bool {
        if !Path::new(&self.path).exists() {
            return false;
        }
        #[cfg(unix)]
        {
            match connect_unix(&self.path, Duration::from_millis(500)) {
                Ok(_stream) => true,
                Err(e) if matches!(e.kind(), io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound) => {
                    self.cleanup_stale();
                    false
                }
                Err(_) => true,
            }
        }
        #[cfg(not(unix))]
        {
            true
        }
    }

    pub fn cleanup_stale(&self) {
        let p = Path::new(&self.path);
        if p.exists() {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// AF_INET SOCK_STREAM transport restricted to localhost.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TcpLoopbackTransport {
    pub host: String,
    pub port: u16,
}

const ALLOWED_HOSTS: [&str; 2] = ["127.0.0.1", "localhost"];

impl TcpLoopbackTransport {
    /// An empty `host` means [`DEFAULT_TCP_HOST`].
    pub fn new(host: &str, port: u16) -> Result<Self, SpecError> {
        let host = if host.is_empty() { DEFAULT_TCP_HOST } else { host };
        if !ALLOWED_HOSTS.contains(&host) {
            return Err(SpecError::NotLoopback { host: host.to_string(), port });
        }
        Ok(TcpLoopbackTransport { host: host.to_string(), port })
    }

    pub fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Like `asyncio.start_server(host=...)`: listen on every address the host
    /// resolves to (`localhost` gives 127.0.0.1 and ::1), skipping a family the
    /// machine has disabled.
    async fn bind(&self) -> io::Result<Vec<Listener>> {
        let mut addrs: Vec<SocketAddr> = Vec::new();
        for a in tokio::net::lookup_host((self.host.as_str(), self.port)).await? {
            if !addrs.contains(&a) {
                addrs.push(a);
            }
        }
        let mut out = Vec::new();
        for addr in &addrs {
            match bind_tcp(*addr) {
                Ok(l) => out.push(Listener::Tcp(l)),
                Err(e) => {
                    let msg = format!(
                        "error while attempting to bind on address {}: {}",
                        py_sockaddr_repr(addr),
                        py_os_error_str(&e).to_lowercase()
                    );
                    if e.raw_os_error() == Some(libc::EADDRNOTAVAIL) {
                        // Assume the family is not enabled (bpo-30945).
                        tracing::debug!("{msg}");
                        continue;
                    }
                    // `OSError(err.errno, msg)`: str() puts the errno in front.
                    let text = match e.raw_os_error() {
                        Some(n) => format!("[Errno {n}] {msg}"),
                        None => msg,
                    };
                    return Err(io::Error::new(e.kind(), text));
                }
            }
        }
        if out.is_empty() {
            let all: Vec<String> = addrs.iter().map(py_sockaddr_repr).collect();
            return Err(io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                format!("could not bind on any address out of [{}]", all.join(", ")),
            ));
        }
        Ok(out)
    }

    /// Open a blocking client socket. An AF_INET socket, so only IPv4
    /// addresses of the host are tried, the first one only.
    pub fn sync_connect(&self, timeout: Duration) -> io::Result<SyncStream> {
        let addr = self.ipv4_addr()?;
        let stream: TcpStream = connect_socket(socket2::Domain::IPV4, &addr.into(), timeout)?.into();
        let stream = SyncStream::Tcp(stream);
        stream.set_timeout(timeout)?;
        Ok(stream)
    }

    fn ipv4_addr(&self) -> io::Result<SocketAddr> {
        (self.host.as_str(), self.port)
            .to_socket_addrs()?
            .find(|a| a.is_ipv4())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no IPv4 address for {}", self.host)))
    }

    pub fn is_in_use(&self) -> bool {
        match self.ipv4_addr() {
            Ok(addr) => connect_socket(socket2::Domain::IPV4, &addr.into(), Duration::from_millis(500)).is_ok(),
            Err(_) => false,
        }
    }

    pub fn cleanup_stale(&self) {}
}

/// Abstract local IPC channel. Both async server and sync client sides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transport {
    Unix(UnixTransport),
    Tcp(TcpLoopbackTransport),
}

impl Transport {
    /// Human-readable address for logs and CLI hints.
    pub fn address(&self) -> String {
        match self {
            Transport::Unix(t) => t.address(),
            Transport::Tcp(t) => t.address(),
        }
    }

    /// Start a server bound to this transport; every accepted connection is
    /// handed to `on_conn` on a task of its own.
    pub async fn start_server(&self, on_conn: ConnHandler) -> io::Result<Server> {
        let listeners = match self {
            Transport::Unix(t) => t.bind().await?,
            Transport::Tcp(t) => t.bind().await?,
        };
        Ok(Server::spawn(listeners, on_conn))
    }

    /// Open a blocking client socket.
    pub fn sync_connect(&self, timeout: Duration) -> io::Result<SyncStream> {
        match self {
            Transport::Unix(t) => t.sync_connect(timeout),
            Transport::Tcp(t) => t.sync_connect(timeout),
        }
    }

    /// True iff some other process is actively accepting here.
    pub fn is_in_use(&self) -> bool {
        match self {
            Transport::Unix(t) => t.is_in_use(),
            Transport::Tcp(t) => t.is_in_use(),
        }
    }

    /// Remove leftover artifacts, if any.
    pub fn cleanup_stale(&self) {
        match self {
            Transport::Unix(t) => t.cleanup_stale(),
            Transport::Tcp(t) => t.cleanup_stale(),
        }
    }

    pub fn as_unix(&self) -> Option<&UnixTransport> {
        match self {
            Transport::Unix(t) => Some(t),
            Transport::Tcp(_) => None,
        }
    }

    pub fn as_tcp(&self) -> Option<&TcpLoopbackTransport> {
        match self {
            Transport::Tcp(t) => Some(t),
            Transport::Unix(_) => None,
        }
    }
}

/// Parse a local IPC transport spec.
///
/// `host:port` and `:port` mean TCP loopback. Anything else is a Unix socket
/// path, which is unsupported on Windows.
pub fn parse_spec(spec: &str) -> Result<Transport, SpecError> {
    parse_spec_on(spec, Platform::current())
}

pub fn parse_spec_on(spec: &str, platform: Platform) -> Result<Transport, SpecError> {
    if spec.is_empty() {
        return Err(SpecError::Empty);
    }

    if let Some(i) = spec.rfind(':') {
        let (host, port_s) = (&spec[..i], &spec[i + 1..]);
        let mut host_chars = host.chars();
        let is_drive_letter = matches!((host_chars.next(), host_chars.next()), (Some(c), None) if c.is_alphabetic())
            && (port_s.starts_with('\\') || port_s.starts_with('/'));
        let is_digits = !port_s.is_empty() && port_s.bytes().all(|b| b.is_ascii_digit());
        if !is_drive_letter && is_digits && !spec.contains('\\') && !host.contains('/') {
            let trimmed = port_s.trim_start_matches('0');
            let shown = if trimmed.is_empty() { "0" } else { trimmed };
            let port = match shown.parse::<u32>() {
                Ok(p) if p > 0 && p < 65536 => p as u16,
                _ => return Err(SpecError::PortOutOfRange(shown.to_string())),
            };
            let host = if host.is_empty() { DEFAULT_TCP_HOST } else { host };
            return TcpLoopbackTransport::new(host, port).map(Transport::Tcp);
        }
    }

    if platform == Platform::Windows {
        return Err(SpecError::UnixPathOnWindows(spec.to_string()));
    }
    Ok(Transport::Unix(UnixTransport { path: spec.to_string() }))
}

/// Platform-appropriate default transport spec.
pub fn default_spec() -> String {
    default_spec_on(Platform::current())
}

pub fn default_spec_on(platform: Platform) -> String {
    match platform {
        Platform::Windows => format!("{DEFAULT_TCP_HOST}:{DEFAULT_TCP_PORT}"),
        Platform::Posix => DEFAULT_UNIX_PATH.to_string(),
    }
}

/// Resolve a Transport from explicit CLI spec, env var, or platform default.
/// An empty spec or an empty `EVE_BRIDGE_SOCK` counts as unset.
pub fn make_transport(spec: Option<&str>) -> Result<Transport, SpecError> {
    let env = std::env::var(SOCK_ENV).ok();
    make_transport_from(spec, env.as_deref(), Platform::current())
}

/// [`make_transport`] with the environment value and platform passed in.
pub fn make_transport_from(spec: Option<&str>, env: Option<&str>, platform: Platform) -> Result<Transport, SpecError> {
    let chosen = match (spec.filter(|s| !s.is_empty()), env.filter(|s| !s.is_empty())) {
        (Some(s), _) => s.to_string(),
        (None, Some(e)) => e.to_string(),
        (None, None) => default_spec_on(platform),
    };
    parse_spec_on(&chosen, platform)
}

// ---------------------------------------------------------------------------
// Sockets

/// `settimeout(timeout)` then `connect(addr)`: a zero timeout is a
/// non-blocking connect, which may fail with `EINPROGRESS` / `EAGAIN`.
fn connect_socket(domain: socket2::Domain, addr: &socket2::SockAddr, timeout: Duration) -> io::Result<socket2::Socket> {
    use socket2::{Socket, Type};
    let socket = Socket::new(domain, Type::STREAM, None)?;
    if timeout.is_zero() {
        socket.set_nonblocking(true)?;
        socket.connect(addr)?;
    } else {
        socket.connect_timeout(addr, timeout)?;
    }
    Ok(socket)
}

#[cfg(unix)]
fn connect_unix(path: &str, timeout: Duration) -> io::Result<std::os::unix::net::UnixStream> {
    let addr = socket2::SockAddr::unix(path)?;
    Ok(connect_socket(socket2::Domain::UNIX, &addr, timeout)?.into())
}

#[cfg(unix)]
fn bind_unix(path: &str) -> io::Result<tokio::net::UnixListener> {
    use socket2::{Domain, SockAddr, Socket, Type};
    let addr = SockAddr::unix(path)?;
    let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
    if let Err(e) = socket.bind(&addr) {
        if e.kind() == io::ErrorKind::AddrInUse {
            // asyncio's `OSError(errno.EADDRINUSE, f"Address {path!r} is already in use")`.
            return Err(io::Error::new(
                e.kind(),
                format!("[Errno {}] Address {} is already in use", libc::EADDRINUSE, py_str_repr(path)),
            ));
        }
        return Err(e);
    }
    socket.listen(BACKLOG)?;
    socket.set_nonblocking(true)?;
    let std_listener: std::os::unix::net::UnixListener = socket.into();
    tokio::net::UnixListener::from_std(std_listener)
}

fn bind_tcp(addr: SocketAddr) -> io::Result<tokio::net::TcpListener> {
    use socket2::{Domain, Socket, Type};
    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, None)?;
    #[cfg(not(windows))]
    socket.set_reuse_address(true)?;
    if addr.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    socket.bind(&addr.into())?;
    socket.listen(BACKLOG)?;
    socket.set_nonblocking(true)?;
    let std_listener: std::net::TcpListener = socket.into();
    tokio::net::TcpListener::from_std(std_listener)
}

/// Python's repr of a socket address tuple, as asyncio prints it in bind errors.
fn py_sockaddr_repr(addr: &SocketAddr) -> String {
    match addr {
        SocketAddr::V4(a) => format!("('{}', {})", a.ip(), a.port()),
        SocketAddr::V6(a) => format!("('{}', {}, {}, {})", a.ip(), a.port(), a.flowinfo(), a.scope_id()),
    }
}

/// A blocking client connection from [`Transport::sync_connect`].
#[derive(Debug)]
pub enum SyncStream {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
    Tcp(TcpStream),
}

impl SyncStream {
    /// Python's `settimeout`: one deadline for every read and write. Zero
    /// means non-blocking, as `settimeout(0.0)` does.
    pub fn set_timeout(&self, timeout: Duration) -> io::Result<()> {
        let nonblocking = timeout.is_zero();
        let t = if nonblocking { None } else { Some(timeout) };
        match self {
            #[cfg(unix)]
            SyncStream::Unix(s) => {
                s.set_nonblocking(nonblocking)?;
                s.set_read_timeout(t)?;
                s.set_write_timeout(t)
            }
            SyncStream::Tcp(s) => {
                s.set_nonblocking(nonblocking)?;
                s.set_read_timeout(t)?;
                s.set_write_timeout(t)
            }
        }
    }

    /// Half-close the write side, so the server's `readline` sees EOF.
    pub fn shutdown_write(&self) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            SyncStream::Unix(s) => s.shutdown(std::net::Shutdown::Write),
            SyncStream::Tcp(s) => s.shutdown(std::net::Shutdown::Write),
        }
    }
}

impl Read for SyncStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            SyncStream::Unix(s) => s.read(buf),
            SyncStream::Tcp(s) => s.read(buf),
        }
    }
}

impl Write for SyncStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            SyncStream::Unix(s) => s.write(buf),
            SyncStream::Tcp(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            SyncStream::Unix(s) => s.flush(),
            SyncStream::Tcp(s) => s.flush(),
        }
    }
}

/// One accepted server-side connection.
#[derive(Debug)]
pub enum IpcStream {
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
    Tcp(tokio::net::TcpStream),
}

impl AsyncRead for IpcStream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            IpcStream::Unix(s) => Pin::new(s).poll_read(cx, buf),
            IpcStream::Tcp(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for IpcStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            #[cfg(unix)]
            IpcStream::Unix(s) => Pin::new(s).poll_write(cx, buf),
            IpcStream::Tcp(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            IpcStream::Unix(s) => Pin::new(s).poll_flush(cx),
            IpcStream::Tcp(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            IpcStream::Unix(s) => Pin::new(s).poll_shutdown(cx),
            IpcStream::Tcp(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

enum Listener {
    /// The listener, and the socket path with the inode it was bound with.
    #[cfg(unix)]
    Unix(tokio::net::UnixListener, Option<(String, u64)>),
    Tcp(tokio::net::TcpListener),
}

impl Listener {
    async fn accept(&self) -> io::Result<IpcStream> {
        match self {
            #[cfg(unix)]
            Listener::Unix(l, _) => l.accept().await.map(|(s, _)| IpcStream::Unix(s)),
            Listener::Tcp(l) => l.accept().await.map(|(s, _)| IpcStream::Tcp(s)),
        }
    }

    fn local_address(&self) -> String {
        match self {
            #[cfg(unix)]
            Listener::Unix(l, _) => l
                .local_addr()
                .ok()
                .and_then(|a| a.as_pathname().map(|p| p.display().to_string()))
                .unwrap_or_default(),
            Listener::Tcp(l) => l.local_addr().map(|a| a.to_string()).unwrap_or_default(),
        }
    }

    fn cleanup(&self) -> Option<(String, u64)> {
        match self {
            #[cfg(unix)]
            Listener::Unix(_, c) => c.clone(),
            Listener::Tcp(_) => None,
        }
    }
}

/// Per-connection callback for [`Transport::start_server`].
pub type ConnHandler = Arc<dyn Fn(IpcStream) -> BoxFuture<'static, ()> + Send + Sync>;

/// A running server, the counterpart of `asyncio.Server`.
pub struct Server {
    closed: watch::Sender<bool>,
    accept_tasks: parking_lot::Mutex<Vec<JoinHandle<()>>>,
    conns_tx: parking_lot::Mutex<Option<mpsc::Sender<()>>>,
    conns_rx: tokio::sync::Mutex<mpsc::Receiver<()>>,
    addresses: Vec<String>,
    /// Unix socket files to remove on close, if still the ones bound.
    unix_cleanup: parking_lot::Mutex<Vec<(String, u64)>>,
    serving_forever: std::sync::atomic::AtomicBool,
}

impl Server {
    fn spawn(listeners: Vec<Listener>, on_conn: ConnHandler) -> Server {
        let (closed, _) = watch::channel(false);
        // Every live connection task holds a clone of `conns_tx`; the channel
        // reports closed once the server's copy and all of theirs are gone.
        let (conns_tx, conns_rx) = mpsc::channel(1);
        let addresses = listeners.iter().map(Listener::local_address).collect();
        let unix_cleanup = listeners.iter().filter_map(Listener::cleanup).collect();
        let tasks = listeners
            .into_iter()
            .map(|l| tokio::spawn(accept_loop(l, on_conn.clone(), conns_tx.clone(), closed.subscribe())))
            .collect();
        Server {
            closed,
            accept_tasks: parking_lot::Mutex::new(tasks),
            conns_tx: parking_lot::Mutex::new(Some(conns_tx)),
            conns_rx: tokio::sync::Mutex::new(conns_rx),
            addresses,
            unix_cleanup: parking_lot::Mutex::new(unix_cleanup),
            serving_forever: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// The bound addresses: the socket path, or `ip:port` per listening socket.
    pub fn addresses(&self) -> &[String] {
        &self.addresses
    }

    pub fn is_serving(&self) -> bool {
        !*self.closed.borrow()
    }

    /// Stop accepting. Connections already accepted run to the end. A Unix
    /// socket file is removed if it is still the one this server bound
    /// (asyncio 3.13's `cleanup_socket`).
    pub fn close(&self) {
        if self.closed.send_replace(true) {
            return;
        }
        for t in self.accept_tasks.lock().iter() {
            t.abort();
        }
        self.conns_tx.lock().take();
        for (path, inode) in std::mem::take(&mut *self.unix_cleanup.lock()) {
            remove_if_same_inode(&path, inode);
        }
    }

    /// Wait until the server is closed and every accepted connection is done
    /// (Python 3.12+ `Server.wait_closed`).
    pub async fn wait_closed(&self) {
        let mut rx = self.closed.subscribe();
        let _ = rx.wait_for(|c| *c).await;
        let tasks: Vec<JoinHandle<()>> = std::mem::take(&mut *self.accept_tasks.lock());
        for t in tasks {
            let _ = t.await;
        }
        let mut conns = self.conns_rx.lock().await;
        while conns.recv().await.is_some() {}
    }

    /// Run until [`Server::close`] is called. Like asyncio, refuses a server
    /// that is closed or already being served forever.
    pub async fn serve_forever(&self) -> anyhow::Result<()> {
        use std::sync::atomic::Ordering;
        if self.serving_forever.swap(true, Ordering::SeqCst) {
            anyhow::bail!(NamedError::new("RuntimeError", "server is already being awaited on serve_forever()"));
        }
        if !self.is_serving() {
            self.serving_forever.store(false, Ordering::SeqCst);
            anyhow::bail!(NamedError::new("RuntimeError", "server is closed"));
        }
        let mut rx = self.closed.subscribe();
        let _ = rx.wait_for(|c| *c).await;
        self.serving_forever.store(false, Ordering::SeqCst);
        Ok(())
    }
}

#[cfg(unix)]
fn remove_if_same_inode(path: &str, inode: u64) {
    use std::os::unix::fs::MetadataExt;
    match std::fs::metadata(path) {
        Ok(m) if m.ino() == inode => {
            if let Err(e) = std::fs::remove_file(path) {
                if e.kind() != io::ErrorKind::NotFound {
                    tracing::error!("Unable to clean up listening UNIX socket {}: {}", py_str_repr(path), e);
                }
            }
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => tracing::error!("Unable to clean up listening UNIX socket {}: {}", py_str_repr(path), e),
    }
}

#[cfg(not(unix))]
fn remove_if_same_inode(_path: &str, _inode: u64) {}

impl Drop for Server {
    fn drop(&mut self) {
        for t in self.accept_tasks.lock().iter() {
            t.abort();
        }
    }
}

async fn accept_loop(listener: Listener, on_conn: ConnHandler, conns: mpsc::Sender<()>, mut closed: watch::Receiver<bool>) {
    loop {
        let res = tokio::select! {
            _ = closed.wait_for(|c| *c) => return,
            r = listener.accept() => r,
        };
        match res {
            Ok(stream) => {
                let guard = conns.clone();
                let fut = on_conn(stream);
                tokio::spawn(async move {
                    fut.await;
                    drop(guard);
                });
            }
            Err(e) => match e.kind() {
                io::ErrorKind::ConnectionAborted | io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock => {}
                _ if matches!(e.raw_os_error(), Some(libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM)) => {
                    tracing::error!("socket.accept() out of system resource: {e}");
                    tokio::time::sleep(ACCEPT_RETRY_DELAY).await;
                }
                _ => tracing::error!("socket.accept() failed: {e}"),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// The IPC server

/// Handler signature: async (request) -> response. A request is whatever
/// JSON value the line held (normally an object).
pub type Handler = Arc<dyn Fn(Value) -> BoxFuture<'static, anyhow::Result<Value>> + Send + Sync>;

/// Wrap an async closure as a [`Handler`].
pub fn handler<F, Fut>(f: F) -> Handler
where
    F: Fn(Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = anyhow::Result<Value>> + Send + 'static,
{
    Arc::new(move |req| f(req).boxed())
}

/// An error that names its Python exception class, so the IPC reply reads
/// `"<kind>: <message>"` exactly as the Python's `f"{type(e).__name__}: {e}"`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct NamedError {
    pub kind: String,
    pub message: String,
}

impl NamedError {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        NamedError { kind: kind.into(), message: message.into() }
    }
}

/// The Python class name an error reports as in an IPC reply.
pub fn error_type_name(e: &anyhow::Error) -> String {
    if let Some(n) = e.downcast_ref::<NamedError>() {
        return n.kind.clone();
    }
    if let Some(s) = e.downcast_ref::<SpecError>() {
        return s.python_type().to_string();
    }
    if let Some(l) = e.downcast_ref::<LoadsError>() {
        return l.python_type().to_string();
    }
    if let Some(io) = e.downcast_ref::<io::Error>() {
        return io_error_type_name(io).to_string();
    }
    if let Some(j) = e.downcast_ref::<serde_json::Error>() {
        return match j.classify() {
            serde_json::error::Category::Syntax | serde_json::error::Category::Eof => "JSONDecodeError",
            _ => "ValueError",
        }
        .to_string();
    }
    "Exception".to_string()
}

/// The `OSError` subclass Python raises for an I/O error: by errno, as
/// `OSError(errno, ...)` picks it, or by kind for errors with no errno.
pub fn io_error_type_name(e: &io::Error) -> &'static str {
    if let Some(n) = e.raw_os_error() {
        return match n {
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
        };
    }
    match e.kind() {
        io::ErrorKind::WouldBlock => "BlockingIOError",
        io::ErrorKind::BrokenPipe => "BrokenPipeError",
        io::ErrorKind::ConnectionAborted => "ConnectionAbortedError",
        io::ErrorKind::ConnectionRefused => "ConnectionRefusedError",
        io::ErrorKind::ConnectionReset => "ConnectionResetError",
        io::ErrorKind::AlreadyExists => "FileExistsError",
        io::ErrorKind::NotFound => "FileNotFoundError",
        io::ErrorKind::IsADirectory => "IsADirectoryError",
        io::ErrorKind::NotADirectory => "NotADirectoryError",
        io::ErrorKind::Interrupted => "InterruptedError",
        io::ErrorKind::PermissionDenied => "PermissionError",
        io::ErrorKind::TimedOut => "TimeoutError",
        _ => "OSError",
    }
}

/// `str(OSError)`: `[Errno N] <strerror>` for an error from the OS, else the
/// error's own text.
pub fn py_os_error_str(e: &io::Error) -> String {
    match e.raw_os_error() {
        Some(n) if e.get_ref().is_none() => {
            let text = e.to_string();
            let strerror = text.strip_suffix(&format!(" (os error {n})")).unwrap_or(&text);
            format!("[Errno {n}] {strerror}")
        }
        _ => e.to_string(),
    }
}

/// The text after `"<Type>: "` in an error reply: `str(e)`.
pub fn error_message(e: &anyhow::Error) -> String {
    match e.downcast_ref::<io::Error>() {
        Some(io) => py_os_error_str(io),
        None => e.to_string(),
    }
}

/// Listens on a transport, dispatches one request per connection.
///
/// `socket_path` is a transport spec (path or `host:port`). Name kept for
/// back-compat; the daemon passes it through unmodified.
pub struct IpcServer {
    handler: Handler,
    transport: Transport,
    server: parking_lot::Mutex<Option<Arc<Server>>>,
}

impl IpcServer {
    pub fn new(handler: Handler, socket_path: Option<&str>) -> Result<Self, SpecError> {
        Ok(Self::with_transport(handler, make_transport(socket_path)?))
    }

    pub fn with_transport(handler: Handler, transport: Transport) -> Self {
        IpcServer { handler, transport, server: parking_lot::Mutex::new(None) }
    }

    /// Transport-specific address string for logs / errors.
    pub fn address(&self) -> String {
        self.transport.address()
    }

    pub fn transport(&self) -> &Transport {
        &self.transport
    }

    /// The running server, once [`IpcServer::start`] returned.
    pub fn server(&self) -> Option<Arc<Server>> {
        self.server.lock().clone()
    }

    pub async fn start(&self) -> io::Result<()> {
        let h = self.handler.clone();
        let on_conn: ConnHandler = Arc::new(move |stream| handle_conn(h.clone(), stream).boxed());
        let server = self.transport.start_server(on_conn).await?;
        *self.server.lock() = Some(Arc::new(server));
        tracing::info!("ipc listening at {}", self.transport.address());
        Ok(())
    }

    /// Serve until stopped; on the way out the server is closed and its
    /// connections are waited for (Python's `async with server`).
    pub async fn serve_forever(&self) -> anyhow::Result<()> {
        let Some(server) = self.server() else {
            anyhow::bail!("ipc server not started");
        };
        let res = server.serve_forever().await;
        if res.is_ok() {
            server.close();
            server.wait_closed().await;
        }
        res
    }

    pub async fn stop(&self) {
        let server = self.server();
        if let Some(server) = server {
            server.close();
            server.wait_closed().await;
        }
        self.transport.cleanup_stale();
    }
}

/// Read one request line, answer it, close. Generic over the stream so it
/// can be driven by an in-memory pipe.
pub async fn handle_conn<S>(handler: Handler, mut stream: S)
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    serve_one(&handler, &mut stream).await;
    let _ = stream.shutdown().await;
}

async fn serve_one<S>(handler: &Handler, stream: &mut S)
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let line = match read_line(stream).await {
        Ok(line) => line,
        Err(e) => {
            // Python's readline raises here, outside the handler's try: asyncio
            // logs it and the connection closes with no reply.
            let kind = if e.kind() == io::ErrorKind::InvalidData { "ValueError" } else { io_error_type_name(&e) };
            tracing::error!("Unhandled exception in client_connected_cb: {kind}: {}", py_os_error_str(&e));
            return;
        }
    };
    if line.is_empty() {
        return;
    }
    let req = match decode_request(&line) {
        Ok(v) => v,
        Err(BadRequest::Reply(msg)) => {
            reply(stream, &serde_json::json!({"ok": false, "error": format!("bad json: {msg}")})).await;
            return;
        }
        Err(BadRequest::Unhandled(e)) => {
            tracing::error!("Unhandled exception in client_connected_cb: {}: {e}", e.python_type());
            return;
        }
    };
    // Handler faults shouldn't kill the server.
    let call = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(req.clone())));
    let outcome = match call {
        Ok(fut) => std::panic::AssertUnwindSafe(fut).catch_unwind().await,
        Err(panic) => Err(panic),
    };
    let resp = match outcome {
        Ok(Ok(resp)) => resp,
        Ok(Err(e)) => {
            tracing::error!("handler error for req={}: {e:?}", py_repr(&req));
            serde_json::json!({"ok": false, "error": format!("{}: {}", error_type_name(&e), error_message(&e))})
        }
        Err(panic) => {
            let msg = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "handler panicked".to_string());
            tracing::error!("handler error for req={}: panic: {msg}", py_repr(&req));
            serde_json::json!({"ok": false, "error": format!("Exception: {msg}")})
        }
    };
    reply(stream, &resp).await;
}

/// asyncio's `StreamReader.readline()` with the default 64 KiB limit: the line
/// through its `\n`, or whatever came before EOF. A newline past the limit,
/// or more than the limit without one, is an error.
async fn read_line<S: AsyncRead + Unpin>(stream: &mut S) -> io::Result<Vec<u8>> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; READ_LIMIT];
    let mut searched = 0usize;
    loop {
        if let Some(i) = buf[searched..].iter().position(|&b| b == b'\n') {
            let isep = searched + i;
            if isep > READ_LIMIT {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Separator is found, but chunk is longer than limit",
                ));
            }
            buf.truncate(isep + 1);
            return Ok(buf);
        }
        searched = buf.len();
        if buf.len() > READ_LIMIT {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Separator is not found, and chunk exceed the limit"));
        }
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(buf);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// A request line Python's `json.loads(line.decode("utf-8"))` refuses.
#[derive(Debug)]
enum BadRequest {
    /// A `ValueError` / `UnicodeDecodeError`: answered `bad json: <text>`.
    Reply(String),
    /// Anything else (a `RecursionError`): no reply.
    Unhandled(LoadsError),
}

/// `json.loads(line.decode("utf-8"))`, with the error text the reply carries.
fn decode_request(line: &[u8]) -> Result<Value, BadRequest> {
    let text = std::str::from_utf8(line).map_err(|e| BadRequest::Reply(utf8_error_message(line, &e)))?;
    pyjson::loads(text).map_err(|e| match e {
        LoadsError::Value(msg) => BadRequest::Reply(msg),
        e @ LoadsError::Recursion(_) => BadRequest::Unhandled(e),
    })
}

/// Python's `UnicodeDecodeError` text for a strict UTF-8 decode.
fn utf8_error_message(bytes: &[u8], e: &std::str::Utf8Error) -> String {
    let start = e.valid_up_to();
    let lead = bytes[start];
    let (end, reason) = match e.error_len() {
        _ if matches!(lead, 0x80..=0xC1 | 0xF5..=0xFF) => (start + 1, "invalid start byte"),
        Some(n) => (start + n, "invalid continuation byte"),
        None => (bytes.len(), "unexpected end of data"),
    };
    if end - start == 1 {
        format!("'utf-8' codec can't decode byte 0x{lead:02x} in position {start}: {reason}")
    } else {
        format!("'utf-8' codec can't decode bytes in position {start}-{}: {reason}", end - 1)
    }
}

async fn reply<S: AsyncWrite + Unpin>(stream: &mut S, obj: &Value) {
    let mut data = dumps(obj);
    data.push('\n');
    let res = async {
        stream.write_all(data.as_bytes()).await?;
        stream.flush().await
    }
    .await;
    match res {
        Ok(()) => {}
        Err(e) if matches!(e.kind(), io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe) => {
            // Hook clients enforce their own timeout and may hang up before
            // reading the reply (seen during serial-reconnect storms). Their
            // loss — must not become an unhandled connection error.
            tracing::debug!("ipc: client hung up before reply was flushed");
        }
        Err(e) => tracing::error!(
            "Unhandled exception in client_connected_cb: {}: {}",
            io_error_type_name(&e),
            py_os_error_str(&e)
        ),
    }
}

// ---------------------------------------------------------------------------
// Python-compatible text

/// `json.dumps(obj, ensure_ascii=False)`: `", "` and `": "` separators,
/// non-ASCII written as is, floats as Python's `repr`.
pub fn dumps(value: &Value) -> String {
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, PyFormatter);
    serde::Serialize::serialize(value, &mut ser).expect("serializing a JSON value into memory cannot fail");
    String::from_utf8(out).expect("serde_json writes UTF-8")
}

struct PyFormatter;

impl serde_json::ser::Formatter for PyFormatter {
    fn begin_array_value<W: ?Sized + Write>(&mut self, writer: &mut W, first: bool) -> io::Result<()> {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_key<W: ?Sized + Write>(&mut self, writer: &mut W, first: bool) -> io::Result<()> {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_value<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        writer.write_all(b": ")
    }

    fn write_f64<W: ?Sized + Write>(&mut self, writer: &mut W, value: f64) -> io::Result<()> {
        let s = if value.is_nan() {
            "NaN".to_string()
        } else if value.is_infinite() {
            if value > 0.0 { "Infinity" } else { "-Infinity" }.to_string()
        } else {
            py_float_repr(value)
        };
        writer.write_all(s.as_bytes())
    }

    fn write_f32<W: ?Sized + Write>(&mut self, writer: &mut W, value: f32) -> io::Result<()> {
        self.write_f64(writer, value as f64)
    }
}

/// Python's `repr(float)`: shortest round-trip digits, fixed notation for
/// exponents in -4..16, otherwise `d.ddde+XX`.
pub fn py_float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0" } else { "0.0" }.to_string();
    }
    let sign = if f < 0.0 { "-" } else { "" };
    let sci = format!("{:e}", f.abs());
    let (mant, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let body = if (-4..16).contains(&exp) {
        if exp >= 0 {
            let int_len = exp as usize + 1;
            if digits.len() <= int_len {
                format!("{digits}{}.0", "0".repeat(int_len - digits.len()))
            } else {
                format!("{}.{}", &digits[..int_len], &digits[int_len..])
            }
        } else {
            format!("0.{}{digits}", "0".repeat((-exp - 1) as usize))
        }
    } else {
        let m = if digits.len() == 1 { digits.clone() } else { format!("{}.{}", &digits[..1], &digits[1..]) };
        format!("{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    };
    format!("{sign}{body}")
}

/// Python's `repr(str)`.
pub fn py_str_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if !py_printable(c) => {
                let cp = c as u32;
                if cp <= 0xff {
                    out.push_str(&format!("\\x{cp:02x}"));
                } else if cp <= 0xffff {
                    out.push_str(&format!("\\u{cp:04x}"));
                } else {
                    out.push_str(&format!("\\U{cp:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `str.isprintable()` for one character: not a control, format, separator
/// (other than space) or private-use character.
fn py_printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    if c.is_control() {
        return false;
    }
    !matches!(c as u32,
        0x00A0 | 0x00AD | 0x061C | 0x1680 | 0x180E
        | 0x2000..=0x200F | 0x2028..=0x202F | 0x205F..=0x2064 | 0x2066..=0x206F
        | 0x3000 | 0xFEFF | 0xFFF9..=0xFFFB
        | 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD
        | 0xE0001 | 0xE0020..=0xE007F)
}

/// Python's `repr()` of the value `json.loads` would have produced: dicts as
/// `{'k': v}`, `True`/`False`/`None`, floats as `repr(float)`.
pub fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => i.to_string(),
            (None, Some(u), _) => u.to_string(),
            (None, None, Some(f)) => py_float_repr(f),
            _ => n.to_string(),
        },
        Value::String(s) => py_str_repr(s),
        Value::Array(items) => format!("[{}]", items.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter().map(|(k, v)| format!("{}: {}", py_str_repr(k), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

#[cfg(test)]
mod tests;
