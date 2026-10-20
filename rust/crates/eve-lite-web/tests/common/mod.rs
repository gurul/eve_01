//! The helpers lite_web's three test files share (test_lite_web_local.py's top half).
//!
//! A `Rig` is a real LocalServer (and, once a phone is added, a real LanServer) on ephemeral ports, with the real
//! web_auth objects, a real LocalTls and the real onboarding sign-ins in a tmp folder. Only the network is replaced:
//! the providers by a mock HttpTransport, WebRTC, the Composio bridge and the daemon's status by small fakes, and
//! the Local Network alert and probe by recorders (the real ones would make macOS ask about the local network). The
//! client is a raw HTTP client, so a test sets every header itself (Host, Origin, Cookie) and reads every byte back.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::FutureExt;
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use eve_brain::onboarding::{self, ComposioConnectLogin, OnboardingError, OpenRouterLogin, SignInError};
use eve_brain::seams::{Http, HttpError, HttpRequest, HttpResponse, HttpTransport};
use eve_lite_tls::LocalTls;
use eve_lite_web::*;
use eve_link::link_chat::door::LinkChatOptions;
use eve_link::link_chat::{self as link_chat_mod, LinkChat};
use eve_link::link_topics::{Bookmarks, Titler};
use eve_link::seams::{AppOut, Chief, DirectApps, DynRead, DynWrite, Headers, HearOptions, Inlet, InletFn};
use eve_link::updates::{UpdateSources, Updates, UpdatesOptions};
use eve_link_uploads::UploadStore;
use eve_transcripts::{TranscriptConfig, Transcripts};
use eve_web_auth::{Devices, MacLogin, PairingWindow};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, RootCertStore};

/// what the OpenRouter exchange mints here
pub const CANARY_KEY: &str = "CANARY-NOT-A-KEY-7f3c-openrouter";
/// Composio's tokens here
pub const CANARY_TOKEN: &str = "CANARY-NOT-A-KEY-7f3c-composio";
pub const IPHONE: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like \
                          Gecko) Version/18.0 Mobile/15E148 Safari/604.1";
pub const MAC_SAFARI: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) \
                              Version/18.0 Safari/605.1.15";
pub const HOSTNAME: &str = "Test-Mac";
pub const LAN_IP: &str = "192.168.50.7";
pub const INDEX: &[u8] = b"<!doctype html><title>eve</title><div id=root></div><!-- the test page -->";

/// > GZIP_MIN_BYTES
pub fn script_js() -> Vec<u8> {
    let mut out = b"export const eve = 1;\n".to_vec();
    for _ in 0..200 {
        out.extend_from_slice(b"console.log('eve lite web app');\n");
    }
    out
}

// ---- the client ------------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub cookies: Vec<String>,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| panic!("not JSON ({e}): {:?}", self.text()))
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn header(&self, name: &str) -> &str {
        self.headers.get(name).map(String::as_str).unwrap_or_else(|| panic!("no {name} in {:?}", self.headers))
    }
}

pub fn dechunk(raw: &[u8]) -> Vec<u8> {
    let (mut out, mut rest) = (Vec::new(), raw);
    loop {
        let at = rest.windows(2).position(|w| w == b"\r\n").expect("a chunk without its size line");
        let size = usize::from_str_radix(std::str::from_utf8(&rest[..at]).unwrap().trim(), 16).unwrap();
        rest = &rest[at + 2..];
        if size == 0 {
            return out;
        }
        out.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
}

pub fn parse_reply(raw: &[u8]) -> Reply {
    let at = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or_else(|| {
        panic!("no complete head in {:?}", String::from_utf8_lossy(&raw[..raw.len().min(200)]))
    });
    let (top, body) = (&raw[..at], &raw[at + 4..]);
    let text: String = top.iter().map(|b| *b as char).collect();
    let lines: Vec<&str> = text.split("\r\n").collect();
    let status: u16 = lines[0].split(' ').nth(1).unwrap().parse().unwrap();
    let mut headers: HashMap<String, String> = HashMap::new();
    let mut cookies = Vec::new();
    for line in &lines[1..] {
        let (name, value) = line.split_once(':').unwrap_or((line, ""));
        let (name, value) = (name.trim().to_lowercase(), value.trim().to_string());
        if name == "set-cookie" {
            cookies.push(value.clone());
        }
        let joined = match headers.get(&name) {
            Some(have) => format!("{have}, {value}"),
            None => value,
        };
        headers.insert(name, joined);
    }
    let body = if headers.get("transfer-encoding").map(String::as_str) == Some("chunked") {
        dechunk(body)
    } else {
        body.to_vec()
    };
    Reply { status, headers, cookies, body }
}

pub fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// A client that trusts `ca_pem` alone, as a phone that installed it.
pub fn client_trusting(ca_pem: &[u8]) -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    for c in CertificateDer::pem_slice_iter(ca_pem) {
        roots.add(c.unwrap()).unwrap();
    }
    Arc::new(
        ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// A browser that has not installed the CA: it trusts nothing here.
pub fn client_untrusting() -> Arc<ClientConfig> {
    Arc::new(
        ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(RootCertStore::empty())
            .with_no_client_auth(),
    )
}

/// What a connection reads until the server closes; what came before an error is kept (Python's reader.read()).
async fn read_all<R: tokio::io::AsyncRead + Unpin>(reader: &mut R) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => return Ok(out),
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if out.is_empty() => return Err(e),
            Err(_) => return Ok(out),
        }
    }
}

/// `data` as it is, and everything the server writes until it closes.
pub async fn send_raw(port: u16, data: &[u8], tls: Option<Arc<ClientConfig>>, secs: f64) -> io::Result<Vec<u8>> {
    let tcp = TcpStream::connect(("127.0.0.1", port)).await?;
    let go = async {
        match tls {
            None => {
                let mut tcp = tcp;
                tcp.write_all(data).await?;
                tcp.flush().await?;
                read_all(&mut tcp).await
            }
            Some(cfg) => {
                let name = ServerName::try_from("localhost").unwrap();
                let mut conn = tokio_rustls::TlsConnector::from(cfg).connect(name, tcp).await?;
                conn.write_all(data).await?;
                conn.flush().await?;
                read_all(&mut conn).await
            }
        }
    };
    tokio::time::timeout(Duration::from_secs_f64(secs), go)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the server never closed"))?
}

/// What a door sends back for `data`: empty when it closes without a byte, a reset included.
pub async fn answer_to(port: u16, data: &[u8]) -> Vec<u8> {
    send_raw(port, data, None, 5.0).await.unwrap_or_default()
}

/// One request's options (test_lite_web_local.http's keywords).
#[derive(Clone, Default)]
pub struct Req {
    pub cookie: Option<String>,
    /// None: `localhost:<port>`; Some(""): no Host
    pub host: Option<String>,
    /// None: the page's own for a POST; Some(None): no Origin
    pub origin: Option<Option<String>>,
    /// None: JSON for a POST; Some(""): no Content-Type
    pub ctype: Option<String>,
    pub headers: Vec<(String, String)>,
    pub tls: Option<Arc<ClientConfig>>,
}

impl Req {
    pub fn new() -> Self {
        Req::default()
    }
    pub fn cookie(mut self, c: impl Into<String>) -> Self {
        self.cookie = Some(c.into());
        self
    }
    pub fn maybe_cookie(mut self, c: Option<&str>) -> Self {
        self.cookie = c.map(str::to_string);
        self
    }
    pub fn host(mut self, h: impl Into<String>) -> Self {
        self.host = Some(h.into());
        self
    }
    pub fn origin(mut self, o: Option<&str>) -> Self {
        self.origin = Some(o.map(str::to_string));
        self
    }
    pub fn ctype(mut self, c: &str) -> Self {
        self.ctype = Some(c.to_string());
        self
    }
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
    pub fn tls(mut self, cfg: Arc<ClientConfig>) -> Self {
        self.tls = Some(cfg);
        self
    }
}

/// A request's body: none, JSON, or raw bytes.
#[derive(Clone)]
pub enum Body {
    None,
    Json(Value),
    Raw(Vec<u8>),
}

impl From<Value> for Body {
    fn from(v: Value) -> Self {
        Body::Json(v)
    }
}

impl From<&[u8]> for Body {
    fn from(b: &[u8]) -> Self {
        Body::Raw(b.to_vec())
    }
}

impl From<Vec<u8>> for Body {
    fn from(b: Vec<u8>) -> Self {
        Body::Raw(b)
    }
}

impl From<()> for Body {
    fn from(_: ()) -> Self {
        Body::None
    }
}

pub fn request_bytes(port: u16, method: &str, path: &str, body: Body, req: &Req) -> Vec<u8> {
    let host = req.host.clone().unwrap_or_else(|| format!("localhost:{port}"));
    let scheme = if req.tls.is_some() { "https" } else { "http" };
    let data = match body {
        Body::None => Vec::new(),
        Body::Json(v) => eve_apps_mcp::py::dumps(&v, true).into_bytes(),
        Body::Raw(b) => b,
    };
    let mut lines = vec![format!("{method} {path} HTTP/1.1")];
    if !host.is_empty() {
        lines.push(format!("Host: {host}"));
    }
    let origin = match &req.origin {
        None => (method != "GET" && method != "HEAD").then(|| format!("{scheme}://{host}")),
        Some(o) => o.clone(),
    };
    if let Some(origin) = origin {
        lines.push(format!("Origin: {origin}"));
    }
    if let Some(cookie) = req.cookie.as_ref().filter(|c| !c.is_empty()) {
        lines.push(format!("Cookie: {cookie}"));
    }
    if method != "GET" && method != "HEAD" {
        let ctype = req.ctype.clone().unwrap_or_else(|| "application/json".to_string());
        if !ctype.is_empty() {
            lines.push(format!("Content-Type: {ctype}"));
        }
        lines.push(format!("Content-Length: {}", data.len()));
    }
    lines.extend(req.headers.iter().map(|(k, v)| format!("{k}: {v}")));
    let mut out = (lines.join("\r\n") + "\r\n\r\n").into_bytes();
    out.extend_from_slice(&data);
    out
}

/// One request; an error when the connection or its TLS fails.
pub async fn try_http(port: u16, method: &str, path: &str, body: impl Into<Body>, req: Req) -> io::Result<Reply> {
    let raw = request_bytes(port, method, path, body.into(), &req);
    let got = send_raw(port, &raw, req.tls.clone(), 20.0).await?;
    if got.is_empty() {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "closed without an answer"));
    }
    Ok(parse_reply(&got))
}

pub async fn http(port: u16, method: &str, path: &str, body: impl Into<Body>, req: Req) -> Reply {
    try_http(port, method, path, body, req).await.unwrap_or_else(|e| panic!("{method} {path}: {e}"))
}

/// The value a Set-Cookie of `name` gives.
pub fn cookie_of(reply: &Reply, name: &str) -> String {
    for line in &reply.cookies {
        let pair = line.split(';').next().unwrap_or("");
        if let Some(v) = pair.strip_prefix(&format!("{name}=")) {
            return v.to_string();
        }
    }
    panic!("no {name} cookie in {:?}", reply.cookies)
}

/// Wait for `pred()` to hold: work the door does in the background.
pub async fn until(pred: impl Fn() -> bool) {
    for _ in 0..1000 {
        if pred() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting")
}

/// `asyncio.run`: a current-thread runtime, so a test's captured logs see every task's lines.
pub fn run<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
}

// ---- logs --------------------------------------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock()).into_owned()
    }
}

impl io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Every log line written while `f` runs (caplog at DEBUG), and its result. One process-wide subscriber writes every
/// line to one buffer: a scoped one misses lines whose callsite another test's thread reached first. The window
/// can hold another test's lines too, which only makes a "never logs" check stricter.
pub fn with_logs<T>(f: impl FnOnce() -> T) -> (T, String) {
    static SINK: std::sync::OnceLock<Captured> = std::sync::OnceLock::new();
    let sink = SINK
        .get_or_init(|| {
            let captured = Captured::default();
            let writer = captured.clone();
            let subscriber = tracing_subscriber::fmt()
                .with_writer(move || writer.clone())
                .with_max_level(tracing::Level::TRACE)
                .with_ansi(false)
                .finish();
            tracing::subscriber::set_global_default(subscriber).expect("one global subscriber");
            captured
        })
        .clone();
    let from = sink.0.lock().len();
    let out = f();
    let text = String::from_utf8_lossy(&sink.0.lock()[from..]).into_owned();
    (out, text)
}

// ---- the fakes ---------------------------------------------------------------------------------------------------

/// lite_rtc.RtcHub as lite_web uses it: answer, close_device, voice_ready.
pub struct FakeRtc {
    pub offers: Mutex<Vec<(String, String, String)>>,
    pub closed: Mutex<Vec<String>>,
    pub voice: AtomicBool,
}

impl FakeRtc {
    pub fn new() -> Arc<Self> {
        Arc::new(FakeRtc { offers: Mutex::new(Vec::new()), closed: Mutex::new(Vec::new()), voice: AtomicBool::new(true) })
    }
}

#[async_trait]
impl RtcDoor for FakeRtc {
    async fn answer(&self, device_id: &str, sdp: &Value, peer_addr: &str) -> Result<Value, RtcFailure> {
        let Some(sdp) = sdp.as_str().filter(|s| s.starts_with("v=0")) else {
            return Err(RtcFailure::Refused("the offer must be SDP of at most 20000 characters".into()));
        };
        self.offers.lock().push((device_id.to_string(), sdp.to_string(), peer_addr.to_string()));
        Ok(json!({"sdp": "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\n", "voice": self.voice.load(Ordering::SeqCst)}))
    }

    async fn close_device(&self, device_id: &str) -> anyhow::Result<()> {
        self.closed.lock().push(device_id.to_string());
        Ok(())
    }

    fn voice_ready(&self) -> anyhow::Result<bool> {
        Ok(self.voice.load(Ordering::SeqCst))
    }
}

/// composio_connect.ConnectBridge as Settings → Apps uses it.
pub struct FakeBridge {
    pub connected: Mutex<HashMap<String, bool>>,
    pub link: Mutex<Option<String>>,
    pub needs_sign_in: AtomicBool,
    pub asked: Mutex<Vec<String>>,
}

impl FakeBridge {
    pub fn new(connected: &[(&str, bool)]) -> Arc<Self> {
        Arc::new(FakeBridge {
            connected: Mutex::new(connected.iter().map(|(k, v)| (k.to_string(), *v)).collect()),
            link: Mutex::new(Some("https://connect.composio.dev/link/ln_test".to_string())),
            needs_sign_in: AtomicBool::new(false),
            asked: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl AppsDoor for FakeBridge {
    fn needs_sign_in(&self) -> anyhow::Result<bool> {
        Ok(self.needs_sign_in.load(Ordering::SeqCst))
    }

    async fn connections(&self, toolkits: Option<&[&str]>) -> anyhow::Result<IndexMap<String, bool>> {
        let all = eve_composio_connect::app_slugs();
        let asked = toolkits.unwrap_or(&all);
        let have = self.connected.lock();
        Ok(asked.iter().map(|t| (t.to_string(), have.get(*t).copied().unwrap_or(false))).collect())
    }

    async fn connect_link(&self, toolkit: &str) -> anyhow::Result<Option<String>> {
        self.asked.lock().push(toolkit.to_string());
        if self.connected.lock().get(toolkit).copied().unwrap_or(false) {
            return Ok(None);
        }
        Ok(self.link.lock().clone())
    }
}

/// What a test makes of the certificate's context: the real one, counted, or broken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsMode {
    Real,
    Broken,
}

/// lite_tls.LocalTls, with `context` a test can count or break (Python's tests set `r.ctx.tls.context`).
pub struct TestTls {
    pub inner: LocalTls,
    pub mode: Mutex<TlsMode>,
    pub contexts: AtomicUsize,
}

impl TlsDoor for TestTls {
    fn context(&self) -> anyhow::Result<Arc<rustls::ServerConfig>> {
        self.contexts.fetch_add(1, Ordering::SeqCst);
        match *self.mode.lock() {
            TlsMode::Real => self.inner.context(),
            TlsMode::Broken => Err(io::Error::other("the certificate's folder is gone").into()),
        }
    }
    fn refresh(&self, addresses: &[String], hostname: &str) -> anyhow::Result<bool> {
        self.inner.refresh(addresses.iter().map(String::as_str), hostname)
    }
    fn names(&self) -> anyhow::Result<HashSet<String>> {
        self.inner.names()
    }
    fn ca_pem(&self) -> anyhow::Result<Vec<u8>> {
        self.inner.ca_pem()
    }
    fn mobileconfig(&self) -> anyhow::Result<Vec<u8>> {
        self.inner.mobileconfig()
    }
}

/// onboarding.OpenRouterLogin, with `finish` a test can make leak (Python's tests set `r.ctx.openrouter.finish`).
pub struct TestOpenRouter {
    pub inner: OpenRouterLogin,
    pub leaky: AtomicBool,
}

#[async_trait]
impl OpenRouterSignIn for TestOpenRouter {
    fn start(&self, callback_url: &str) -> Result<String, SignInError> {
        self.inner.start(callback_url, "")
    }
    async fn finish(&self, code: &str, state: &str, error: &str) -> Result<String, OnboardingError> {
        if self.leaky.load(Ordering::SeqCst) {
            return Err(OnboardingError::new(&format!("OpenRouter said: bad key {CANARY_KEY}"), 502));
        }
        self.inner.finish(code.to_string(), state.to_string(), error).await
    }
}

/// What the rig's fakes saw and answer.
pub struct RigState {
    pub status: Mutex<Value>,
    pub provider_calls: Mutex<Vec<String>>,
    pub credit_calls: AtomicUsize,
    pub credit_answer: Mutex<Value>,
    pub register_status: Mutex<u16>,
    pub restarts: AtomicUsize,
    pub alerts: AtomicUsize,
    pub probes: Mutex<Vec<SocketAddr>>,
}

/// The providers (httpx.MockTransport): OpenRouter's key exchange and Composio's registration and token.
pub struct Providers(pub Arc<RigState>);

fn resp(status: u16, body: Value) -> HttpResponse {
    HttpResponse {
        status,
        headers: vec![("content-type".into(), "application/json".into())],
        body: serde_json::to_vec(&body).unwrap(),
    }
}

#[async_trait]
impl HttpTransport for Providers {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let url = request.url.clone();
        self.0.provider_calls.lock().push(url.clone());
        if url == onboarding::OPENROUTER_KEYS_URL {
            return Ok(resp(200, json!({"key": CANARY_KEY})));
        }
        if url == onboarding::CONNECT_REGISTER_URL {
            let status = *self.0.register_status.lock();
            if status != 201 {
                return Ok(resp(status, json!({"error": "invalid_redirect_uri"})));
            }
            let asked: Value = serde_json::from_slice(&request.body).unwrap();
            return Ok(resp(201, json!({"client_id": "eve-lite-test-client", "redirect_uris": asked["redirect_uris"]})));
        }
        if url == onboarding::CONNECT_TOKEN_URL {
            return Ok(resp(200, json!({"access_token": format!("{CANARY_TOKEN}-access"),
                                       "refresh_token": format!("{CANARY_TOKEN}-refresh"), "expires_in": 3600})));
        }
        Ok(HttpResponse { status: 404, headers: Vec::new(), body: Vec::new() })
    }
}

/// A built web page as WP-11 lays it out: index.html and one flat assets folder.
pub fn make_web(folder: &Path) -> PathBuf {
    std::fs::create_dir_all(folder.join("assets")).unwrap();
    std::fs::write(folder.join("index.html"), INDEX).unwrap();
    std::fs::write(folder.join("assets").join("eve-abc123.js"), script_js()).unwrap();
    std::fs::write(folder.join("assets").join("eve-abc123.css"), b"body{margin:0}\n").unwrap();
    folder.to_path_buf()
}

/// What a Rig is made with (the Rig dataclass's fields).
pub struct RigOpts {
    pub link: Option<Arc<dyn LinkServe>>,
    pub apps: Option<Arc<FakeBridge>>,
    pub environ: Option<Vec<(String, String)>>,
    pub web: Option<PathBuf>,
    pub peer_allowed: Option<PeerAllowedFn>,
    pub local_ports: Vec<u16>,
    pub lan_ports: Vec<u16>,
    pub register_status: u16,
}

impl Default for RigOpts {
    fn default() -> Self {
        RigOpts {
            link: None,
            apps: None,
            environ: None,
            web: None,
            peer_allowed: None,
            local_ports: vec![0],
            lan_ports: vec![0],
            register_status: 201,
        }
    }
}

pub fn env(pairs: &[(&str, &str)]) -> Option<Vec<(String, String)>> {
    Some(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
}

/// A LocalServer on an ephemeral loopback port.
pub struct Rig {
    pub tmp: PathBuf,
    pub ctx: Arc<WebContext>,
    pub local: LocalServer,
    pub port: u16,
    pub state: Arc<RigState>,
    pub rtc: Arc<FakeRtc>,
    pub tls: Arc<TestTls>,
    pub openrouter: Arc<TestOpenRouter>,
    pub devices: Arc<Devices>,
    pub pairing: Arc<PairingWindow>,
    pub mac_login: Arc<MacLogin>,
}

impl Rig {
    pub async fn start(tmp: &Path) -> Rig {
        Rig::with(tmp, RigOpts::default()).await
    }

    pub async fn with(tmp: &Path, opts: RigOpts) -> Rig {
        Rig::try_with(tmp, opts).await.unwrap_or_else(|e| panic!("the rig did not start: {e}"))
    }

    pub async fn try_with(tmp: &Path, opts: RigOpts) -> Result<Rig, StartError> {
        std::fs::create_dir_all(tmp).unwrap();
        let state = Arc::new(RigState {
            status: Mutex::new(json!({"state": "READY"})),
            provider_calls: Mutex::new(Vec::new()),
            credit_calls: AtomicUsize::new(0),
            credit_answer: Mutex::new(json!({"ok": true, "usd": 4.25, "funded": true})),
            register_status: Mutex::new(opts.register_status),
            restarts: AtomicUsize::new(0),
            alerts: AtomicUsize::new(0),
            probes: Mutex::new(Vec::new()),
        });
        let http: Http = Arc::new(Providers(Arc::clone(&state)));
        let web_dir = opts.web.clone().unwrap_or_else(|| make_web(&tmp.join("web")));
        let devices = Arc::new(Devices::new(tmp.join("web-devices.json")));
        let pairing = Arc::new(PairingWindow::new());
        let mac_login = Arc::new(MacLogin::new());
        let tls = Arc::new(TestTls { inner: LocalTls::new(tmp.join("web-tls")), mode: Mutex::new(TlsMode::Real), contexts: AtomicUsize::new(0) });
        let openrouter = Arc::new(TestOpenRouter { inner: OpenRouterLogin::new(Some(http.clone()), None), leaky: AtomicBool::new(false) });
        let composio = Arc::new(ComposioConnectLogin::new(Some(&tmp.join("composio-connect.json")), "", Some(http), None));
        let rtc = FakeRtc::new();
        let mut parts = Parts::new(devices.clone(), pairing.clone(), mac_login.clone(), tls.clone(), tmp.join("web.json"));
        parts.link = opts.link.clone();
        parts.rtc = Some(rtc.clone());
        parts.openrouter = Some(openrouter.clone());
        parts.composio_login = Some(composio);
        parts.apps = opts.apps.clone().map(|a| a as Arc<dyn AppsDoor>);
        let st = Arc::clone(&state);
        parts.status = Some(Arc::new(move || {
            let v = st.status.lock().clone();
            async move { Ok(v) }.boxed()
        }));
        let st = Arc::clone(&state);
        parts.restart = Some(Arc::new(move || {
            st.restarts.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }.boxed()
        }));
        parts.web_dir = Some(web_dir);
        parts.environ = WebEnv::map(opts.environ.clone().unwrap_or_else(|| vec![("EVE_PROFILE".into(), "lite".into())]));
        parts.env_path = Some(tmp.join("env"));
        let st = Arc::clone(&state);
        parts.credit = Arc::new(move |_key: String| {
            st.credit_calls.fetch_add(1, Ordering::SeqCst);
            let v = st.credit_answer.lock().clone();
            async move { Ok(v) }.boxed()
        });
        parts.hostname = Arc::new(|| Ok(HOSTNAME.to_string()));
        parts.addresses = Arc::new(|| Ok(vec![LAN_IP.to_string()]));
        let st = Arc::clone(&state);
        parts.alert = Arc::new(move || {
            st.alerts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        let st = Arc::clone(&state);
        parts.probe = Arc::new(move |peer: &SocketAddr| {
            st.probes.lock().push(*peer);
            Ok("ok".to_string())
        });
        parts.local_hosts = vec!["127.0.0.1".into()];
        parts.local_ports = opts.local_ports.clone();
        parts.lan_hosts = vec!["127.0.0.1".into()];
        parts.lan_ports = opts.lan_ports.clone();
        parts.restart_delay = 0.0;
        if let Some(allowed) = opts.peer_allowed.clone() {
            parts.peer_allowed = allowed;
        }
        let ctx = WebContext::new(parts);
        let local = LocalServer::new(Arc::clone(&ctx));
        let port = local.start().await?;
        Ok(Rig { tmp: tmp.to_path_buf(), ctx, local, port, state, rtc, tls, openrouter, devices, pairing, mac_login })
    }

    pub async fn stop(&self) {
        self.local.stop().await;
    }

    pub fn trust(&self) -> Arc<ClientConfig> {
        client_trusting(&std::fs::read(&self.tls.inner.ca_cert_path).unwrap())
    }

    pub fn set_status(&self, v: Value) {
        *self.state.status.lock() = v;
    }

    pub fn restarts(&self) -> usize {
        self.state.restarts.load(Ordering::SeqCst)
    }

    pub fn provider_calls(&self) -> Vec<String> {
        self.state.provider_calls.lock().clone()
    }

    pub fn lan_port(&self) -> u16 {
        let lan = self.ctx.shared.lan().expect("the LAN door was made");
        assert!(lan.running(), "the LAN door is not open");
        lan.port()
    }

    pub async fn mac(&self, method: &str, path: &str, body: impl Into<Body>, req: Req) -> Reply {
        http(self.port, method, path, body, req).await
    }

    pub async fn lan(&self, method: &str, path: &str, body: impl Into<Body>, req: Req) -> Reply {
        let port = self.lan_port();
        let req = if req.tls.is_none() { req.tls(self.trust()) } else { req };
        http(port, method, path, body, req).await
    }

    /// The Mac's browser signs in as `eve-bridge web open` has it do: its eve_mac token.
    pub async fn login(&self) -> String {
        self.login_as(MAC_SAFARI).await
    }

    pub async fn login_as(&self, ua: &str) -> String {
        let code = self.mac_login.issue();
        let reply = self.mac("POST", MAC_LOGIN_PATH, json!({"code": code}), Req::new().header("User-Agent", ua)).await;
        assert_eq!(reply.status, 200, "{}", reply.text());
        cookie_of(&reply, eve_web_auth::COOKIE_MAC)
    }

    pub async fn open_pairing(&self, mac_token: &str) -> Value {
        let reply = self.mac("POST", PAIR_OPEN_PATH, json!({}), Req::new().cookie(format!("eve_mac={mac_token}"))).await;
        assert_eq!(reply.status, 200, "{}", reply.text());
        reply.json()
    }

    /// A phone pairs through the QR code's secret, the window opened by the Mac's page (signed in now unless
    /// `mac_token` is given): its eve_dev token.
    pub async fn pair(&self, mac_token: Option<&str>) -> String {
        let token = match mac_token {
            Some(t) => t.to_string(),
            None => self.login().await,
        };
        let opened = self.open_pairing(&token).await;
        let secret = secret_of(&opened, 0);
        let reply = self.lan("POST", PAIR_PATH, json!({"secret": secret}), Req::new().header("User-Agent", IPHONE)).await;
        assert_eq!(reply.status, 200, "{}", reply.text());
        cookie_of(&reply, eve_web_auth::COOKIE_DEV)
    }
}

/// The pairing secret in link `i`'s URL (after #p=).
pub fn secret_of(opened: &Value, i: usize) -> String {
    opened["links"][i]["url"].as_str().unwrap().split_once("#p=").unwrap().1.to_string()
}

pub fn device_labels(devices: &Devices) -> Vec<String> {
    devices.list().into_iter().map(|d| d.label).collect()
}

/// A URL's query fields (parse_qs, the first value each).
pub fn query_of(url: &str) -> HashMap<String, String> {
    let q = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    let q = q.split('#').next().unwrap_or("");
    let mut out = HashMap::new();
    for field in q.split('&').filter(|f| !f.is_empty()) {
        let (k, v) = field.split_once('=').unwrap_or((field, ""));
        out.entry(unquote(k)).or_insert_with(|| unquote(v));
    }
    out
}

fn unquote(s: &str) -> String {
    let s = s.replace('+', " ");
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() {
            let hex = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
            out.push(hex(b[i + 1]) * 16 + hex(b[i + 2]));
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `html.escape(line)`.
pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#x27;")
}

pub fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len().max(1)).any(|w| w == needle)
}

// ---- the chat (test_link_chat's FakeInlet) --------------------------------------------------------------------

pub type Script = Arc<dyn Fn(Arc<FakeInlet>, String) -> BoxFuture<'static, ()> + Send + Sync>;

/// The inlet as link_chat uses it: hear, owner_busy, app_out and transcripts. `script` runs as the turn would.
pub struct FakeInlet {
    me: Weak<FakeInlet>,
    app_out: Mutex<Option<AppOut>>,
    transcripts: Option<Arc<Transcripts>>,
    pub heard: Mutex<Vec<(String, bool, Option<String>)>>,
    pub busy: AtomicBool,
    script: Option<Script>,
    uploads: Mutex<Option<Arc<UploadStore>>>,
}

impl FakeInlet {
    pub fn new(store: Option<Arc<Transcripts>>, script: Option<Script>) -> Arc<Self> {
        Arc::new_cyclic(|me| FakeInlet {
            me: me.clone(),
            app_out: Mutex::new(None),
            transcripts: store,
            heard: Mutex::new(Vec::new()),
            busy: AtomicBool::new(false),
            script,
            uploads: Mutex::new(None),
        })
    }

    pub fn out(&self, app: &str, text: &str) {
        let out = self.app_out.lock().clone().expect("app_out is set");
        out(app, text, "say");
    }

    pub fn heard(&self) -> Vec<(String, bool, Option<String>)> {
        self.heard.lock().clone()
    }
}

impl Inlet for FakeInlet {
    fn chat_ready(&self) -> bool {
        true
    }

    fn hear(&self, text: &str, opts: HearOptions) -> anyhow::Result<()> {
        self.heard.lock().push((text.to_string(), opts.on_call, opts.app.clone()));
        let app = opts.app.clone().unwrap_or_default();
        if let Some(script) = self.script.clone() {
            self.busy.store(true, Ordering::SeqCst);
            let me = self.me.upgrade().expect("the inlet");
            tokio::spawn(script(me, app));
        }
        Ok(())
    }

    fn owner_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    fn set_app_out(&self, out: AppOut) {
        *self.app_out.lock() = Some(out);
    }

    fn set_link_uploads(&self, uploads: Arc<UploadStore>) {
        *self.uploads.lock() = Some(uploads);
    }

    fn transcripts(&self) -> Option<Arc<Transcripts>> {
        self.transcripts.clone()
    }
}

pub fn store_at(tmp: &Path) -> Arc<Transcripts> {
    let store = Transcripts::new(TranscriptConfig::new(true, tmp.join("transcripts")), None, None);
    assert!(store.enabled);
    Arc::new(store)
}

pub fn two_lines() -> Script {
    Arc::new(|inlet: Arc<FakeInlet>, app: String| {
        async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            inlet.out(&app, "First line.");
            tokio::time::sleep(Duration::from_millis(20)).await;
            inlet.out(&app, "Second line.");
            inlet.busy.store(false, Ordering::SeqCst);
        }
        .boxed()
    })
}

/// Busy and never done: it says one line and keeps the turn open.
pub fn forever() -> Script {
    Arc::new(|inlet: Arc<FakeInlet>, app: String| {
        async move {
            inlet.out(&app, "Working on it.");
        }
        .boxed()
    })
}

struct NoSources;

#[async_trait]
impl UpdateSources for NoSources {
    fn apps(&self, _app: &str) -> Option<Arc<dyn DirectApps>> {
        None
    }
    fn vault(&self) -> Option<PathBuf> {
        None
    }
    fn chief(&self) -> Option<Arc<dyn Chief>> {
        None
    }
    fn records_dir(&self) -> Option<PathBuf> {
        None
    }
    fn composio(&self) -> anyhow::Result<Option<Arc<dyn eve_link::seams::ComposioRead>>> {
        Ok(None)
    }
    fn hidden(&self) -> anyhow::Result<Option<eve_transcripts::transcripts::Pred>> {
        Ok(None)
    }
}

/// `link_door`: the real LinkChat, its stores in `tmp`.
pub fn link_door(tmp: &Path, inlet: Arc<FakeInlet>) -> Arc<LinkChat> {
    std::fs::create_dir_all(tmp).unwrap();
    let env = HashMap::from([
        ("EVE_PHONE_LOCATION_FILE".to_string(), tmp.join("phone-location.json").display().to_string()),
        ("EVE_PLACES_FILE".to_string(), tmp.join("places.db").display().to_string()),
    ]);
    let opts = LinkChatOptions {
        settle_secs: 0.01,
        uploads: Some(Arc::new(UploadStore::new(Some(tmp.join("uploads"))))),
        hidden: Some(Arc::new(link_chat_mod::HiddenJobs::new(Some(&tmp.join("hidden-jobs.json"))))),
        bookmarks: Some(Arc::new(Bookmarks::new(Some(&tmp.join("bm.json"))))),
        titler: Some(Arc::new(Titler::new(
            Some(&tmp.join("titles.json")),
            Some(HashMap::from([("EVE_TOPIC_TITLES".to_string(), "0".to_string())])),
            None,
        ))),
        environ: Some(env),
        updates: Some(Arc::new(Updates::new(UpdatesOptions {
            path: Some(tmp.join("updates.json")),
            sources: Some(Arc::new(NoSources)),
            client: Some(Arc::new(|| None)),
            environ: Some(HashMap::from([("EVE_UPDATES".to_string(), "0".to_string())])),
            ..UpdatesOptions::default()
        }))),
        ..LinkChatOptions::default()
    };
    let lent: InletFn = Arc::new(move || Some(inlet.clone() as Arc<dyn Inlet>));
    Arc::new(LinkChat::new(lent, opts))
}

/// (each chunk's content, whether [DONE] came last, how many pings).
pub fn events(body: &[u8]) -> (Vec<String>, bool, usize) {
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.ends_with("\n\n"), "{text:?}");
    let (mut contents, mut done, mut pings) = (Vec::new(), false, 0);
    for event in text[..text.len() - 2].split("\n\n") {
        assert!(!done, "nothing may follow [DONE]");
        if event == ": ping" {
            pings += 1;
        } else if event == "data: [DONE]" {
            done = true;
        } else {
            assert!(event.starts_with("data: ") && !event.contains('\n'), "{event:?}");
            let chunk: Value = serde_json::from_str(&event["data: ".len()..]).unwrap();
            contents.push(chunk["choices"][0]["delta"]["content"].as_str().unwrap().to_string());
        }
    }
    (contents, done, pings)
}

/// One request LinkChat got, as it got it.
#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: Headers,
    /// the token check's answer for each of the spy's probes
    pub checks: Vec<(String, bool)>,
}

/// The real LinkChat with each request's arguments kept as it got them.
pub struct SpyLink {
    pub link: Arc<LinkChat>,
    pub seen: Mutex<Vec<Seen>>,
    pub probes: Mutex<Vec<String>>,
}

impl SpyLink {
    pub fn new(link: Arc<LinkChat>) -> Arc<Self> {
        Arc::new(SpyLink { link, seen: Mutex::new(Vec::new()), probes: Mutex::new(Vec::new()) })
    }
}

#[async_trait]
impl LinkServe for SpyLink {
    async fn serve(
        &self,
        method: &str,
        path: &str,
        query: &str,
        headers: &Headers,
        reader: &mut DynRead,
        writer: &mut DynWrite,
        token_check: &(dyn for<'s> Fn(&'s str) -> bool + Send + Sync + 'static),
    ) -> io::Result<()> {
        let checks = self.probes.lock().iter().map(|p| (p.clone(), token_check(p))).collect();
        self.seen.lock().push(Seen {
            method: method.to_string(),
            path: path.to_string(),
            query: query.to_string(),
            headers: headers.clone(),
            checks,
        });
        LinkServe::serve(&*self.link, method, path, query, headers, reader, writer, token_check).await
    }

    fn close(&self) {
        LinkServe::close(&*self.link);
    }

    async fn settle(&self) {
        LinkServe::settle(&*self.link).await;
    }
}
