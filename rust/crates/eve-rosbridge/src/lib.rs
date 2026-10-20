//! A rosbridge v2 server over the MemoryBus: eve's memory as ROS-style topics on a WebSocket.
//!
//! Any rosbridge client (roslibjs, roslibpy, Foxglove) connects to `ws://127.0.0.1:9090` and:
//!
//! - `subscribe` to a topic from memory_bus TOPICS (or a prefix ending in `*`) and receive
//!   every bus event as `{"op": "publish", "topic": ..., "msg": ...}`;
//! - `publish` to a known topic (`/eve/memory/remember`) and the message enters the bus;
//! - `call_service` on a bus service (`/eve/memory/recall`) and get a `service_response`.
//!
//! The daemon never waits on a client. Each client owns a bounded outbound queue and a writer
//! task: when the queue is full the oldest queued message is dropped and counted, logged once
//! per client. A client that disconnects takes its bus subscriptions with it.
//!
//! Protocol reference: rosbridge_suite ROSBRIDGE_PROTOCOL.md (ops advertise, unadvertise,
//! publish, subscribe, unsubscribe, call_service, service_response, status, set_level).
//! Compression and fragmentation are left out: `fragment_size` and `compression` are ignored.
//!
//! There is no path here that reads a lesson store: the bus already carries no learner words.
//!
//! **No browser pages.** A handshake that carries an `Origin` header comes from a web page, and
//! any page the owner opens could otherwise reach 127.0.0.1 and read or write eve's memory (v3
//! safety review): it is refused (HTTP 403). Desktop clients (roslibpy, Foxglove's desktop app, a
//! script) send no Origin and connect as before.
//!
//! The transport keeps the Python server's `websockets` settings: a keepalive ping every 20 s
//! that must be answered within 20 s, a 1 MiB message limit, a 10 s opening handshake.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Weak};
use std::time::{Duration, Instant};

use eve_memory_bus::{MemoryBus, Msg, ServiceError, Subscription};
use futures::stream::{SplitSink, SplitStream};
use futures::{FutureExt, SinkExt, StreamExt};
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{Map, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Notify};
use tokio::task::{AbortHandle, JoinHandle};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::WebSocketStream;

mod pyfmt;
pub use pyfmt::{py_float_repr, py_json, py_repr, py_str_repr};

pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: i64 = 9090;
pub const DEFAULT_QUEUE_SIZE: usize = 64;
/// Only a handshake with no Origin header is accepted (`None` stands for "no Origin header").
pub const NO_BROWSER_ORIGINS: &[Option<&str>] = &[None];

/// `websockets.serve(..., ping_interval=20, ping_timeout=20, max_size=1 << 20)` and the library's
/// default `open_timeout=10`, `close_timeout=10`.
const PING_INTERVAL: Duration = Duration::from_secs(20);
const PING_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_SIZE: usize = 1 << 20;
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

/// A monotonic clock in seconds, as Python's `time.monotonic`.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

/// Seconds since the first call: only differences matter, as with `time.monotonic()`.
pub fn monotonic() -> f64 {
    static START: LazyLock<Instant> = LazyLock::new(Instant::now);
    START.elapsed().as_secs_f64()
}

type WsSink = SplitSink<WebSocketStream<TcpStream>, Message>;
type WsRead = SplitStream<WebSocketStream<TcpStream>>;

/// The server's counters: Python's `stats` dict (`sent`, `dropped`, `clients_total`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub sent: u64,
    pub dropped: u64,
    pub clients_total: u64,
}

impl Stats {
    /// The counters as Python's dict, in its key order.
    pub fn as_map(&self) -> IndexMap<String, u64> {
        IndexMap::from([
            ("sent".to_string(), self.sent),
            ("dropped".to_string(), self.dropped),
            ("clients_total".to_string(), self.clients_total),
        ])
    }
}

#[derive(Default)]
struct Counters {
    sent: AtomicU64,
    dropped: AtomicU64,
    clients_total: AtomicU64,
}

/// One client subscription: the bus subscription plus the rosbridge bookkeeping.
struct Sub {
    topic: String,
    sub_id: Option<String>,
    bus_sub: Subscription,
}

struct Client {
    id: u64,
    name: String,
    queue: Mutex<VecDeque<String>>,
    /// 0 means unbounded, as `asyncio.Queue(maxsize=0)`.
    queue_size: usize,
    ready: Notify,
    subs: Mutex<Vec<Sub>>,
    advertised: Mutex<HashSet<String>>,
    writer: Mutex<Option<JoinHandle<()>>>,
    keepalive: Mutex<Option<JoinHandle<()>>>,
    tasks: Mutex<HashMap<u64, AbortHandle>>,
    next_task: AtomicU64,
    dropped: AtomicU64,
    warned: AtomicBool,
    closed: AtomicBool,
    sink: tokio::sync::Mutex<WsSink>,
    /// The payload of the keepalive ping still waiting for its pong.
    ping: Mutex<Option<Vec<u8>>>,
    pong: Notify,
    /// Set when the connection must end now (keepalive timeout).
    abort: watch::Sender<bool>,
}

impl Client {
    fn new(id: u64, peer: Option<SocketAddr>, queue_size: usize, sink: WsSink) -> Self {
        Client {
            id,
            name: client_name(peer),
            queue: Mutex::new(VecDeque::new()),
            queue_size,
            ready: Notify::new(),
            subs: Mutex::new(Vec::new()),
            advertised: Mutex::new(HashSet::new()),
            writer: Mutex::new(None),
            keepalive: Mutex::new(None),
            tasks: Mutex::new(HashMap::new()),
            next_task: AtomicU64::new(1),
            dropped: AtomicU64::new(0),
            warned: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            sink: tokio::sync::Mutex::new(sink),
            ping: Mutex::new(None),
            pong: Notify::new(),
            abort: watch::channel(false).0,
        }
    }

    async fn next_text(&self) -> String {
        loop {
            if let Some(text) = self.queue.lock().pop_front() {
                return text;
            }
            self.ready.notified().await;
        }
    }
}

/// `host:port` of the peer, or "client" when it is unknown.
fn client_name(peer: Option<SocketAddr>) -> String {
    match peer {
        Some(addr) => format!("{}:{}", addr.ip(), addr.port()),
        None => "client".to_string(),
    }
}

struct Running {
    listeners: Vec<JoinHandle<()>>,
    conns: Arc<Mutex<HashMap<u64, JoinHandle<()>>>>,
    closing: watch::Sender<bool>,
}

struct Shared {
    bus: MemoryBus,
    origins: Vec<Option<String>>,
    host: String,
    port: Mutex<i64>,
    queue_size: usize,
    clock: Clock,
    clients: Mutex<IndexMap<u64, Arc<Client>>>,
    stats: Counters,
    running: Mutex<Option<Running>>,
    next_conn: AtomicU64,
    /// (ping interval, pong timeout): `PING_INTERVAL`, `PING_TIMEOUT`; tests shorten them.
    keepalive: Mutex<(Duration, Duration)>,
}

/// The rosbridge server. Cloning shares the same server.
#[derive(Clone)]
pub struct RosbridgeServer {
    inner: Arc<Shared>,
}

impl std::fmt::Debug for RosbridgeServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RosbridgeServer").field("url", &self.url()).field("stats", &self.stats()).finish()
    }
}

/// What a client frame carried: text, or bytes (which `json.loads` also accepts).
enum Raw<'a> {
    Text(&'a str),
    Bytes(&'a [u8]),
}

impl RosbridgeServer {
    /// `RosbridgeServer(bus, host, port, queue_size)` with the monotonic clock.
    pub fn new(bus: MemoryBus, host: &str, port: i64, queue_size: usize) -> Self {
        Self::with_clock(bus, host, port, queue_size, Arc::new(monotonic))
    }

    /// The defaults: 127.0.0.1:9090, a queue of 64.
    pub fn with_defaults(bus: MemoryBus) -> Self {
        Self::new(bus, DEFAULT_HOST, DEFAULT_PORT, DEFAULT_QUEUE_SIZE)
    }

    pub fn with_clock(bus: MemoryBus, host: &str, port: i64, queue_size: usize, clock: Clock) -> Self {
        RosbridgeServer {
            inner: Arc::new(Shared {
                bus,
                origins: NO_BROWSER_ORIGINS.iter().map(|o| o.map(str::to_string)).collect(),
                host: host.to_string(),
                port: Mutex::new(port),
                queue_size,
                clock,
                clients: Mutex::new(IndexMap::new()),
                stats: Counters::default(),
                running: Mutex::new(None),
                next_conn: AtomicU64::new(1),
                keepalive: Mutex::new((PING_INTERVAL, PING_TIMEOUT)),
            }),
        }
    }

    pub fn host(&self) -> &str {
        &self.inner.host
    }

    /// The port: after `start()` with port 0, the one the OS gave.
    pub fn port(&self) -> i64 {
        *self.inner.port.lock()
    }

    /// The accepted Origin header values; `None` is "no Origin header".
    pub fn origins(&self) -> &[Option<String>] {
        &self.inner.origins
    }

    pub fn queue_size(&self) -> usize {
        self.inner.queue_size
    }

    pub fn url(&self) -> String {
        format!("ws://{}:{}", self.inner.host, self.port())
    }

    pub fn stats(&self) -> Stats {
        let s = &self.inner.stats;
        Stats {
            sent: s.sent.load(Ordering::SeqCst),
            dropped: s.dropped.load(Ordering::SeqCst),
            clients_total: s.clients_total.load(Ordering::SeqCst),
        }
    }

    /// How many clients are connected now.
    pub fn client_count(&self) -> usize {
        self.inner.clients.lock().len()
    }

    pub fn is_serving(&self) -> bool {
        self.inner.running.lock().is_some()
    }

    // -- lifecycle --

    /// Bind every address `host` resolves to and start accepting. A second call does nothing.
    pub async fn start(&self) -> io::Result<()> {
        if self.is_serving() {
            return Ok(());
        }
        let port = self.port();
        let port: u16 = u16::try_from(port)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bind(): port must be 0-65535."))?;
        let mut listeners = Vec::new();
        let mut last_err = None;
        for addr in tokio::net::lookup_host((self.inner.host.as_str(), port)).await? {
            match TcpListener::bind(addr).await {
                Ok(l) => listeners.push(l),
                Err(e) => {
                    last_err = Some(e);
                    break;
                }
            }
        }
        if let Some(e) = last_err {
            // create_server fails as a whole when any address cannot be bound.
            return Err(e);
        }
        if listeners.is_empty() {
            return Err(io::Error::new(io::ErrorKind::AddrNotAvailable, "getaddrinfo returned an empty list"));
        }
        // port 0 asks the OS for a free one; record what it gave us.
        if let Ok(addr) = listeners[0].local_addr() {
            *self.inner.port.lock() = addr.port() as i64;
        }
        let names: Vec<String> = listeners.iter().filter_map(|l| l.local_addr().ok()).map(py_sockname).collect();
        let conns: Arc<Mutex<HashMap<u64, JoinHandle<()>>>> = Arc::default();
        let (closing, _) = watch::channel(false);
        let tasks = listeners
            .into_iter()
            .map(|l| tokio::spawn(accept_loop(self.inner.clone(), l, conns.clone(), closing.subscribe())))
            .collect();
        {
            let mut running = self.inner.running.lock();
            if running.is_some() {
                // Lost a race with a concurrent start(): keep the first server.
                for t in tasks {
                    let t: JoinHandle<()> = t;
                    t.abort();
                }
                return Ok(());
            }
            *running = Some(Running { listeners: tasks, conns, closing });
        }
        for name in names {
            tracing::info!("server listening on {name}");
        }
        tracing::info!("rosbridge: serving eve's memory at {}", self.url());
        Ok(())
    }

    /// Close every client, stop listening, and wait (up to the close timeout) for the
    /// connections to finish.
    pub async fn stop(&self) {
        let Some(running) = self.inner.running.lock().take() else { return };
        tracing::info!("server closing");
        for t in &running.listeners {
            t.abort();
        }
        for t in running.listeners {
            let _ = t.await;
        }
        let clients: Vec<Arc<Client>> = self.inner.clients.lock().values().cloned().collect();
        for client in clients {
            close_client(&self.inner, &client).await;
        }
        let _ = running.closing.send(true);
        let handles: Vec<JoinHandle<()>> = running.conns.lock().drain().map(|(_, h)| h).collect();
        let deadline = tokio::time::Instant::now() + CLOSE_TIMEOUT;
        for h in handles {
            let abort = h.abort_handle();
            if tokio::time::timeout_at(deadline, h).await.is_err() {
                abort.abort();
            }
        }
        tracing::info!("server closed");
        let s = self.stats();
        tracing::info!(
            "rosbridge: stopped ({} sent, {} dropped, {} clients over its life)",
            s.sent,
            s.dropped,
            s.clients_total
        );
    }
}

// -- accepting --

async fn accept_loop(
    shared: Arc<Shared>,
    listener: TcpListener,
    conns: Arc<Mutex<HashMap<u64, JoinHandle<()>>>>,
    closing: watch::Receiver<bool>,
) {
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::error!("rosbridge: accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let id = shared.next_conn.fetch_add(1, Ordering::SeqCst);
        let mut map = conns.lock();
        let task_conns = Arc::downgrade(&conns);
        let handle = tokio::spawn(handle_conn(shared.clone(), stream, peer, closing.clone(), id, task_conns));
        map.insert(id, handle);
    }
}

/// The handshake check `websockets` runs for `origins=[None]`, with its status codes and bodies.
fn check_origin(origins: &[Option<String>], req: &Request) -> Result<(), ErrorResponse> {
    let values: Vec<_> = req.headers().get_all("origin").iter().collect();
    if values.len() > 1 {
        return Err(reject(StatusCode::BAD_REQUEST, "invalid Origin header: multiple values"));
    }
    let origin = values.first().map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned());
    if origins.contains(&origin) {
        return Ok(());
    }
    let why = match origin.as_deref() {
        None => "missing Origin header".to_string(),
        Some("") => "empty Origin header".to_string(),
        Some(o) => format!("invalid Origin header: {o}"),
    };
    Err(reject(StatusCode::FORBIDDEN, &why))
}

/// Python's `str(sock.getsockname())`: `('127.0.0.1', 9090)`, or `('::1', 9090, 0, 0)` for IPv6.
fn py_sockname(addr: SocketAddr) -> String {
    match addr {
        SocketAddr::V4(a) => format!("('{}', {})", a.ip(), a.port()),
        SocketAddr::V6(a) => format!("('{}', {}, {}, {})", a.ip(), a.port(), a.flowinfo(), a.scope_id()),
    }
}

fn http_date() -> String {
    chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

/// `ServerProtocol.reject`: a short plain-text response.
fn reject(status: StatusCode, why: &str) -> ErrorResponse {
    let body = format!("Failed to open a WebSocket connection: {why}.\n");
    let mut resp = ErrorResponse::new(Some(body.clone()));
    *resp.status_mut() = status;
    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&http_date()) {
        h.insert("Date", v);
    }
    h.insert("Connection", HeaderValue::from_static("close"));
    h.insert("Content-Length", HeaderValue::from(body.len()));
    h.insert("Content-Type", HeaderValue::from_static("text/plain; charset=utf-8"));
    resp
}

fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default().max_message_size(Some(MAX_SIZE)).max_frame_size(Some(MAX_SIZE))
}

async fn handle_conn(
    shared: Arc<Shared>,
    stream: TcpStream,
    peer: SocketAddr,
    mut closing: watch::Receiver<bool>,
    conn_id: u64,
    conns: Weak<Mutex<HashMap<u64, JoinHandle<()>>>>,
) {
    let origins = shared.origins.clone();
    let callback = move |req: &Request, mut resp: Response| {
        check_origin(&origins, req)?;
        if let Ok(v) = HeaderValue::from_str(&http_date()) {
            resp.headers_mut().insert("Date", v);
        }
        Ok(resp)
    };
    let handshake = tokio_tungstenite::accept_hdr_async_with_config(stream, callback, Some(ws_config()));
    let ws = tokio::select! {
        _ = closing.wait_for(|c| *c) => None,
        r = tokio::time::timeout(OPEN_TIMEOUT, handshake) => match r {
            Ok(Ok(ws)) => {
                tracing::info!("connection open");
                Some(ws)
            }
            Ok(Err(WsError::Http(resp))) => {
                let status = resp.status();
                tracing::info!(
                    "connection rejected ({} {})",
                    status.as_u16(),
                    status.canonical_reason().unwrap_or("")
                );
                None
            }
            Ok(Err(e)) => {
                tracing::error!(error = %e, "opening handshake failed");
                None
            }
            // websockets logs nothing when the opening handshake times out.
            Err(_) => None,
        },
    };
    if let Some(ws) = ws {
        serve_client(&shared, ws, Some(peer)).await;
    }
    if let Some(conns) = conns.upgrade() {
        conns.lock().remove(&conn_id);
    }
}

// -- one client --

async fn serve_client(shared: &Arc<Shared>, ws: WebSocketStream<TcpStream>, peer: Option<SocketAddr>) {
    let (sink, read) = ws.split();
    let id = shared.next_conn.fetch_add(1, Ordering::SeqCst);
    let client = Arc::new(Client::new(id, peer, shared.queue_size, sink));
    shared.clients.lock().insert(id, client.clone());
    shared.stats.clients_total.fetch_add(1, Ordering::SeqCst);
    *client.writer.lock() = Some(tokio::spawn(write_loop(shared.clone(), client.clone())));
    *client.keepalive.lock() = Some(tokio::spawn(keepalive_loop(client.clone(), *shared.keepalive.lock())));
    read_loop(shared, &client, read).await;
    close_client(shared, &client).await;
}

async fn read_loop(shared: &Arc<Shared>, client: &Arc<Client>, mut read: WsRead) {
    let mut abort = client.abort.subscribe();
    loop {
        let item = tokio::select! {
            _ = abort.wait_for(|a| *a) => return,
            item = read.next() => item,
        };
        let outcome = match item {
            None => return,
            Some(Err(WsError::Capacity(e))) => {
                use tokio_tungstenite::tungstenite::error::CapacityError;
                let reason = match e {
                    CapacityError::MessageTooLong { size, max_size } => {
                        format!("over size limit ({size} > {max_size} bytes)")
                    }
                    other => other.to_string(),
                };
                send_close(client, CloseCode::Size, &reason).await;
                return;
            }
            Some(Err(_)) => return,
            Some(Ok(Message::Text(t))) => on_message(shared, client, Raw::Text(t.as_str())),
            Some(Ok(Message::Binary(b))) => on_message(shared, client, Raw::Bytes(&b)),
            Some(Ok(Message::Pong(p))) => {
                let mut ping = client.ping.lock();
                if ping.as_deref() == Some(&p[..]) {
                    *ping = None;
                    client.pong.notify_one();
                }
                Ok(())
            }
            Some(Ok(_)) => Ok(()),
        };
        if let Err(e) = outcome {
            // An exception out of the handler: the Python handler's `finally` closes the client,
            // then websockets logs it.
            close_client(shared, client).await;
            tracing::error!(error = %e, "connection handler failed");
            return;
        }
    }
}

async fn send_close(client: &Client, code: CloseCode, reason: &str) {
    let frame = CloseFrame { code, reason: reason.to_string().into() };
    let fut = async {
        let mut sink = client.sink.lock().await;
        let _ = sink.send(Message::Close(Some(frame))).await;
    };
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, fut).await;
}

async fn close_client(shared: &Shared, client: &Arc<Client>) {
    if shared.clients.lock().shift_remove(&client.id).is_none() {
        return;
    }
    client.closed.store(true, Ordering::SeqCst);
    let subs: Vec<Sub> = client.subs.lock().drain(..).collect();
    for sub in &subs {
        shared.bus.unsubscribe(&sub.bus_sub);
    }
    let tasks: Vec<AbortHandle> = client.tasks.lock().drain().map(|(_, t)| t).collect();
    for task in tasks {
        task.abort();
    }
    let keepalive = client.keepalive.lock().take();
    if let Some(k) = keepalive {
        k.abort();
        let _ = k.await;
    }
    let writer = client.writer.lock().take();
    if let Some(w) = writer {
        w.abort();
        let _ = w.await;
    }
    send_close(client, CloseCode::Normal, "").await;
}

async fn write_loop(shared: Arc<Shared>, client: Arc<Client>) {
    loop {
        let text = client.next_text().await;
        let mut sink = client.sink.lock().await;
        if sink.send(Message::text(text)).await.is_err() {
            return;
        }
        shared.stats.sent.fetch_add(1, Ordering::SeqCst);
    }
}

/// `websockets` keepalive: a ping every 20 s; no matching pong within 20 s closes with 1011.
async fn keepalive_loop(client: Arc<Client>, (ping_interval, ping_timeout): (Duration, Duration)) {
    let mut latency = Duration::ZERO;
    loop {
        // If ping_timeout > latency > ping_interval, the next ping goes out as soon as the pong
        // arrives: the period is then longer than ping_interval.
        tokio::time::sleep(ping_interval.saturating_sub(latency)).await;
        let payload = rand::random::<u32>().to_be_bytes().to_vec();
        *client.ping.lock() = Some(payload.clone());
        {
            let mut sink = client.sink.lock().await;
            if sink.send(Message::Ping(payload.into())).await.is_err() {
                return;
            }
        }
        let sent = Instant::now();
        let acked = tokio::time::timeout(ping_timeout, async {
            loop {
                if client.ping.lock().is_none() {
                    return;
                }
                client.pong.notified().await;
            }
        })
        .await;
        latency = sent.elapsed();
        if acked.is_err() {
            tracing::debug!("- timed out waiting for keepalive pong");
            send_close(&client, CloseCode::Error, "keepalive ping timeout").await;
            let _ = client.abort.send(true);
            return;
        }
    }
}

/// Queue one outbound message. Full queue: drop the oldest, never block the caller.
fn enqueue(shared: &Shared, client: &Client, message: &Value) {
    let text = py_json(message);
    {
        let mut queue = client.queue.lock();
        while client.queue_size > 0 && queue.len() >= client.queue_size {
            queue.pop_front();
            client.dropped.fetch_add(1, Ordering::SeqCst);
            shared.stats.dropped.fetch_add(1, Ordering::SeqCst);
            if !client.warned.swap(true, Ordering::SeqCst) {
                tracing::warn!("rosbridge: {} is not keeping up; dropping its oldest messages", client.name);
            }
        }
        queue.push_back(text);
    }
    client.ready.notify_one();
}

fn status(shared: &Shared, client: &Client, level: &str, msg: &str, msg_id: Option<&str>) {
    let mut out = Map::new();
    out.insert("op".into(), "status".into());
    out.insert("level".into(), level.into());
    out.insert("msg".into(), msg.into());
    if let Some(id) = msg_id {
        out.insert("id".into(), id.into());
    }
    enqueue(shared, client, &Value::Object(out));
}

// -- inbound ops --

/// One inbound frame. `Err` is what Python raises out of the handler (it ends the connection).
fn on_message(shared: &Arc<Shared>, client: &Arc<Client>, raw: Raw<'_>) -> Result<(), String> {
    let parsed: Result<Value, ()> = match raw {
        Raw::Text(s) => serde_json::from_str(s).map_err(|_| ()),
        Raw::Bytes(b) => {
            let b = b.strip_prefix(b"\xef\xbb\xbf".as_slice()).unwrap_or(b);
            serde_json::from_slice(b).map_err(|_| ())
        }
    };
    let Ok(data) = parsed else {
        status(shared, client, "error", "message is not valid JSON", None);
        return Ok(());
    };
    let Value::Object(data) = data else {
        status(shared, client, "error", "message must be a JSON object", None);
        return Ok(());
    };
    let op = data.get("op");
    let msg_id = id_of(data.get("id"));
    let msg_id = msg_id.as_deref();
    match op.and_then(Value::as_str).unwrap_or("") {
        "subscribe" => op_subscribe(shared, client, &data, msg_id),
        "unsubscribe" => op_unsubscribe(shared, client, &data, msg_id),
        "advertise" => op_advertise(client, &data),
        "unadvertise" => return op_unadvertise(client, &data),
        "publish" => op_publish(shared, client, &data, msg_id),
        "call_service" => op_call_service(shared, client, &data, msg_id),
        // set_level is accepted and ignored.
        "set_level" => {}
        _ => {
            let shown = op.map(py_repr).unwrap_or_else(|| "None".to_string());
            status(shared, client, "error", &format!("unsupported op: {shown}"), msg_id);
        }
    }
    Ok(())
}

/// `id` when it is a str or an int (bools are ints in Python), as `str(id)`.
fn id_of(id: Option<&Value>) -> Option<String> {
    match id? {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(if *b { "True" } else { "False" }.to_string()),
        Value::Number(n) if n.is_i64() || n.is_u64() => Some(n.to_string()),
        _ => None,
    }
}

fn known_topic(shared: &Shared, topic: &str) -> bool {
    let topics = shared.bus.topics();
    if topics.contains_key(topic) {
        return true;
    }
    if let Some(prefix) = topic.strip_suffix('*') {
        return topics.keys().any(|t| t.starts_with(prefix));
    }
    false
}

/// `throttle_rate` in ms, when it is a number, as seconds (never negative).
fn throttle_secs(throttle: Option<&Value>) -> f64 {
    let ms = match throttle {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        _ => return 0.0,
    };
    ms.max(0.0) / 1000.0
}

fn op_subscribe(shared: &Arc<Shared>, client: &Arc<Client>, data: &Msg, msg_id: Option<&str>) {
    let topic = match data.get("topic") {
        Some(Value::String(t)) if !t.is_empty() => t.clone(),
        _ => {
            status(shared, client, "error", "subscribe needs a topic", msg_id);
            return;
        }
    };
    let throttle = throttle_secs(data.get("throttle_rate"));
    let last_sent = Arc::new(Mutex::new(f64::NEG_INFINITY));
    let weak_client = Arc::downgrade(client);
    let weak_shared = Arc::downgrade(shared);
    let clock = shared.clock.clone();
    let on_event = move |evt_topic: &str, msg: &Msg| {
        let now = clock();
        {
            let mut last = last_sent.lock();
            if throttle != 0.0 && now - *last < throttle {
                return;
            }
            *last = now;
        }
        let (Some(client), Some(shared)) = (weak_client.upgrade(), weak_shared.upgrade()) else { return };
        let mut out = Map::new();
        out.insert("op".into(), "publish".into());
        out.insert("topic".into(), evt_topic.into());
        out.insert("msg".into(), Value::Object(msg.clone()));
        enqueue(&shared, &client, &Value::Object(out));
    };
    {
        let mut subs = client.subs.lock();
        if client.closed.load(Ordering::SeqCst) {
            // The client is being closed: a subscription now would outlive it.
            return;
        }
        let bus_sub = shared.bus.subscribe(&topic, on_event);
        subs.push(Sub { topic: topic.clone(), sub_id: msg_id.map(str::to_string), bus_sub });
    }
    if !known_topic(shared, &topic) {
        status(shared, client, "error", &format!("unknown topic {topic}; subscribed anyway"), msg_id);
    }
}

fn op_unsubscribe(shared: &Shared, client: &Client, data: &Msg, msg_id: Option<&str>) {
    let topic = data.get("topic").and_then(Value::as_str);
    let mut subs = client.subs.lock();
    let mut keep = Vec::with_capacity(subs.len());
    for sub in subs.drain(..) {
        let same_topic = Some(sub.topic.as_str()) == topic;
        let same_id = msg_id.is_none() || sub.sub_id.as_deref() == msg_id;
        if same_topic && same_id {
            shared.bus.unsubscribe(&sub.bus_sub);
        } else {
            keep.push(sub);
        }
    }
    *subs = keep;
}

fn op_advertise(client: &Client, data: &Msg) {
    if let Some(Value::String(topic)) = data.get("topic") {
        client.advertised.lock().insert(topic.clone());
    }
}

fn op_unadvertise(client: &Client, data: &Msg) -> Result<(), String> {
    match data.get("topic") {
        Some(Value::String(topic)) => {
            client.advertised.lock().remove(topic);
            Ok(())
        }
        // `set.discard` of a list or dict raises.
        Some(Value::Array(_)) => Err("TypeError: unhashable type: 'list'".to_string()),
        Some(Value::Object(_)) => Err("TypeError: unhashable type: 'dict'".to_string()),
        _ => Ok(()),
    }
}

fn op_publish(shared: &Shared, client: &Client, data: &Msg, msg_id: Option<&str>) {
    let topic = data.get("topic");
    let known = match topic {
        Some(Value::String(t)) => shared.bus.topics().contains_key(t.as_str()),
        _ => false,
    };
    if !known {
        let shown = topic.map(py_repr).unwrap_or_else(|| "None".to_string());
        status(shared, client, "error", &format!("cannot publish to unknown topic {shown}"), msg_id);
        return;
    }
    let Some(Value::Object(msg)) = data.get("msg") else {
        status(shared, client, "error", "publish needs an object msg", msg_id);
        return;
    };
    if let Some(Value::String(t)) = topic {
        shared.bus.publish(t, msg.clone());
    }
}

fn op_call_service(shared: &Arc<Shared>, client: &Arc<Client>, data: &Msg, msg_id: Option<&str>) {
    let service = match data.get("service") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => {
            status(shared, client, "error", "call_service needs a service", msg_id);
            return;
        }
    };
    let args = match data.get("args") {
        Some(Value::Object(a)) => a.clone(),
        _ => Msg::new(),
    };
    let task_id = client.next_task.fetch_add(1, Ordering::SeqCst);
    let mut tasks = client.tasks.lock();
    let shared_t = shared.clone();
    let client_t = client.clone();
    let msg_id = msg_id.map(str::to_string);
    let handle = tokio::spawn(async move {
        call(&shared_t, &client_t, &service, args, msg_id.as_deref()).await;
        client_t.tasks.lock().remove(&task_id);
    });
    tasks.insert(task_id, handle.abort_handle());
}

/// Python's `type(e).__name__` for the error kinds a Rust service handler can carry.
fn error_kind(e: &ServiceError) -> &'static str {
    match e {
        ServiceError::NoSuchService(_) => "KeyError",
        ServiceError::Failed(e) => {
            if e.downcast_ref::<tokio::time::error::Elapsed>().is_some() {
                "TimeoutError"
            } else if e.downcast_ref::<serde_json::Error>().is_some() {
                "JSONDecodeError"
            } else if e.downcast_ref::<io::Error>().is_some() {
                "OSError"
            } else {
                "Error"
            }
        }
    }
}

/// Python's `str(e)`: a KeyError shows the repr of its argument.
fn error_text(e: &ServiceError) -> String {
    match e {
        ServiceError::NoSuchService(_) => py_str_repr(&e.to_string()),
        ServiceError::Failed(e) => format!("{e:#}"),
    }
}

async fn call(shared: &Shared, client: &Client, service: &str, args: Msg, msg_id: Option<&str>) {
    let mut out = Map::new();
    out.insert("op".into(), "service_response".into());
    out.insert("service".into(), service.into());
    if let Some(id) = msg_id {
        out.insert("id".into(), id.into());
    }
    let error = |kind: &str, text: &str| -> Value {
        tracing::warn!("rosbridge: service {} failed: {}: {}", service, kind, text);
        let mut values = Map::new();
        values.insert("error".into(), format!("{kind}: {text}").into());
        Value::Object(values)
    };
    if !shared.bus.has_service(service) {
        let mut values = Map::new();
        values.insert("error".into(), "no such service".into());
        out.insert("result".into(), false.into());
        out.insert("values".into(), Value::Object(values));
    } else {
        match AssertUnwindSafe(shared.bus.call_service(service, args)).catch_unwind().await {
            Ok(Ok(values)) => {
                out.insert("result".into(), true.into());
                out.insert("values".into(), Value::Object(values));
            }
            Ok(Err(e)) => {
                out.insert("result".into(), false.into());
                out.insert("values".into(), error(error_kind(&e), &error_text(&e)));
            }
            Err(panic) => {
                let text = if let Some(s) = panic.downcast_ref::<&str>() {
                    (*s).to_string()
                } else if let Some(s) = panic.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "panic".to_string()
                };
                out.insert("result".into(), false.into());
                out.insert("values".into(), error("Error", &text));
            }
        }
    }
    enqueue(shared, client, &Value::Object(out));
}

#[cfg(test)]
mod tests;
