//! The rig for eve lite's daemon tests (test_lite_daemon.py's fixtures).
//!
//! Each test runs its body in a child process of this test binary (the test re-runs itself with `--exact`), on a
//! temporary home under eve lite's environment: the daemon reads HOME, EVE_PROFILE and the model key from its own
//! process, as the Python fixture sets them in `os.environ`, and tests in one process run on parallel threads that
//! must not share an environment. The parent:
//!
//! * makes the box (a home, a built web page, a short folder for the IPC socket) and the child's environment;
//! * cuts the network: every HTTP client in the child goes through a proxy here that answers 403 and remembers the
//!   host each connection asked for (Python's getaddrinfo guard); a daemon test asserts none was asked;
//! * checks the child ran the test (a marker line) and passed.
//!
//! The child installs a log subscriber that keeps every line (Python's caplog), applies eve lite's profile as the
//! fixture does, and runs the body on a tokio runtime.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use eve_lite::{LiteApps, LiteDaemon, LiteOptions, Seams};
use futures::future::BoxFuture;
use futures::FutureExt;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

pub const CANARY_KEY: &str = "CANARY-NOT-A-KEY-7f3c-openrouter";
pub const INDEX: &[u8] = b"<!doctype html><title>eve</title><div id=root></div><!-- the test page -->";
pub const MAC_SAFARI: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) \
                              Version/18.0 Safari/605.1.15";
pub const HOSTNAME: &str = "Test-Mac";
pub const LAN_IP: &str = "192.168.50.7";
/// the lite LaunchAgent's PATH (spec M8)
pub const LITE_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

const CHILD_ENV: &str = "EVE_LITE_CHILD";
const MARK: &str = "eve-lite child ran:";

// ---- the parent ------------------------------------------------------------------------------------------------

/// What the parent sets up for one child.
#[derive(Default)]
pub struct Child {
    /// OPENROUTER_API_KEY=CANARY_KEY (Python's `keyed()`)
    pub key: bool,
    /// EVE_PROFILE (None: unset; the daemon tests use "lite")
    pub profile: Option<String>,
    /// PATH (None: LITE_PATH)
    pub path: Option<String>,
    /// more variables
    pub env: Vec<(String, String)>,
    /// apply eve lite's profile to the environment before the body (the daemon tests' fixture)
    pub apply_profile: bool,
}

/// The box: a temporary home, a web page, a short socket folder.
pub struct Box_ {
    pub tmp: tempfile::TempDir,
    pub home: PathBuf,
    pub web: PathBuf,
    pub short: tempfile::TempDir,
    pub sock: PathBuf,
}

pub fn make_box() -> Box_ {
    let tmp = tempfile::tempdir().expect("tmp");
    let home = tmp.path().join("home");
    let web = tmp.path().join("web");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(web.join("assets")).unwrap();
    std::fs::write(web.join("index.html"), INDEX).unwrap();
    // macOS keeps a socket's path in 104 bytes, and the temporary folder can be longer
    let base = std::env::temp_dir();
    let dir = if base.to_string_lossy().len() <= 60 { base } else { PathBuf::from("/tmp") };
    let short = tempfile::Builder::new().prefix("evl-").tempdir_in(dir).expect("short");
    let sock = short.path().join("ipc.sock");
    Box_ { tmp, home, web, short, sock }
}

/// What the network proxy saw: the host of every connection (CONNECT host:port, or the absolute URL's host).
#[derive(Clone, Default)]
pub struct Asked(Arc<Mutex<Vec<String>>>);

impl Asked {
    pub fn hosts(&self) -> Vec<String> {
        self.0.lock().clone()
    }
}

/// A proxy on loopback that refuses everything and remembers what was asked.
fn start_proxy() -> (u16, Asked) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("proxy");
    let port = listener.local_addr().unwrap().port();
    let asked = Asked::default();
    let seen = asked.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let seen = seen.clone();
            std::thread::spawn(move || {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let mut line = String::new();
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                if reader.read_line(&mut line).is_ok() {
                    let target = line.split_whitespace().nth(1).unwrap_or("").to_string();
                    let host = if let Some(rest) = target.split("://").nth(1) {
                        rest.split(['/', ':']).next().unwrap_or("").to_string()
                    } else {
                        target.rsplit_once(':').map(|(h, _)| h.to_string()).unwrap_or(target)
                    };
                    if !host.is_empty() {
                        seen.0.lock().push(host);
                    }
                }
                let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            });
        }
    });
    (port, asked)
}

/// Run test `name` in a child with `child`'s environment on box `b`. The hosts the child asked the network for.
pub fn run_child(name: &str, b: &Box_, child: Child) -> (Asked, String) {
    let (proxy, asked) = start_proxy();
    let exe = std::env::current_exe().expect("exe");
    let mut cmd = Command::new(exe);
    cmd.args([name, "--exact", "--nocapture", "--test-threads=1"]);
    cmd.env_clear();
    let proxy_url = format!("http://127.0.0.1:{proxy}");
    let mut env: HashMap<String, String> = HashMap::from([
        ("PATH".into(), child.path.clone().unwrap_or_else(|| LITE_PATH.to_string())),
        ("HOME".into(), b.home.to_string_lossy().into_owned()),
        ("TMPDIR".into(), b.tmp.path().to_string_lossy().into_owned()),
        ("EVE_WEB_DIR".into(), b.web.to_string_lossy().into_owned()),
        ("EVE_LITE_TEST_SOCK".into(), b.sock.to_string_lossy().into_owned()),
        ("RUST_BACKTRACE".into(), "1".into()),
        (CHILD_ENV.into(), name.into()),
    ]);
    for var in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"] {
        env.insert(var.into(), proxy_url.clone());
    }
    env.insert("NO_PROXY".into(), "localhost,127.0.0.1,::1".into());
    env.insert("no_proxy".into(), "localhost,127.0.0.1,::1".into());
    if let Some(profile) = &child.profile {
        env.insert("EVE_PROFILE".into(), profile.clone());
    }
    if child.key {
        env.insert("OPENROUTER_API_KEY".into(), CANARY_KEY.into());
    }
    if child.apply_profile {
        env.insert("EVE_LITE_TEST_APPLY".into(), "1".into());
    }
    for (k, v) in child.env {
        env.insert(k, v);
    }
    cmd.envs(env);
    cmd.current_dir(&b.home);
    let out = cmd.output().expect("the child ran");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let tail = |s: &str| s.chars().rev().take(6000).collect::<Vec<_>>().into_iter().rev().collect::<String>();
    assert!(out.status.success(), "the child failed:\n--- stdout\n{}\n--- stderr\n{}", tail(&stdout), tail(&stderr));
    assert!(stdout.contains(&format!("{MARK} {name}")), "the child did not run {name}:\n{}", tail(&stdout));
    (asked, stderr)
}

/// The daemon tests' parent: a box under eve lite (EVE_PROFILE=lite, the profile applied), the child run, and no
/// host asked of the network (the Python fixture's own check).
pub fn daemon_test(name: &str, key: bool) {
    let b = make_box();
    let (asked, _) =
        run_child(name, &b, Child { key, profile: Some("lite".into()), apply_profile: true, ..Child::default() });
    assert_eq!(asked.hosts(), Vec::<String>::new(), "the daemon looked up a host on the network");
}

/// Whether this process is the child running `name`.
pub fn is_child(name: &str) -> bool {
    std::env::var(CHILD_ENV).as_deref() == Ok(name)
}

// ---- the child -------------------------------------------------------------------------------------------------

static LOG: LazyLock<Mutex<Vec<u8>>> = LazyLock::new(|| Mutex::new(Vec::new()));

#[derive(Clone, Copy)]
struct Keep;

impl Write for Keep {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        LOG.lock().extend_from_slice(buf);
        std::io::stderr().write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Keep {
    type Writer = Keep;
    fn make_writer(&'a self) -> Self::Writer {
        Keep
    }
}

/// Every log line the child wrote so far (caplog.text).
pub fn logs() -> String {
    String::from_utf8_lossy(&LOG.lock()).into_owned()
}

/// The child's start: the log kept, the profile applied when asked; then the marker line.
pub fn child_start(name: &str) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(eve_lite::log_filter("DEBUG"))
        .with_writer(Keep)
        .event_format(eve_lite::PyFormat)
        .try_init();
    if std::env::var("EVE_LITE_TEST_APPLY").is_ok() {
        // nothing from the shell running the tests is here (the parent cleared it); apply as cli.main does
        eve_profile::apply_process_env().expect("apply");
    }
    println!("{MARK} {name}");
}

/// Run `body` as the child of test `name` on a tokio runtime; in the parent, run the child.
pub fn child_async<F>(name: &str, key: bool, body: impl FnOnce() -> F)
where
    F: std::future::Future<Output = ()>,
{
    if is_child(name) {
        child_start(name);
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(body());
        rt.shutdown_timeout(Duration::from_secs(2));
        return;
    }
    daemon_test(name, key);
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap())
}

pub fn sock() -> PathBuf {
    PathBuf::from(std::env::var("EVE_LITE_TEST_SOCK").unwrap())
}

pub fn signed_in_to_composio(home: &Path) -> PathBuf {
    let path = home.join(".config/eve-bridge/composio-connect.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        serde_json::to_vec(&json!({"version": 1, "client_id": "eve-lite-test-client", "access_token": "placeholder",
                                   "refresh_token": "placeholder", "expires_at": 4102444800_i64}))
        .unwrap(),
    )
    .unwrap();
    set_mode(&path, 0o600);
    path
}

pub fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

pub fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

// ---- stand-ins for what would leave the Mac ----------------------------------------------------------------------

/// spend_sync's loop, which would read OpenRouter's figures: counts its starts and waits for the shutdown.
#[derive(Clone, Default)]
pub struct SpendLoop(pub Arc<Mutex<usize>>);

impl SpendLoop {
    pub fn started(&self) -> usize {
        *self.0.lock()
    }
}

/// composio_connect.ConnectBridge as the daemon uses it: built, started, closed.
pub struct FakeApps {
    me: std::sync::Weak<FakeApps>,
    pub token_path: PathBuf,
    pub starts: Mutex<usize>,
    pub closed: Mutex<bool>,
    pub needs_sign_in: Mutex<bool>,
    /// called when it is closed (a test's view of the stop's order)
    pub on_close: Mutex<Option<std::boxed::Box<dyn Fn() + Send + Sync>>>,
}

impl LiteApps for FakeApps {
    fn started(&self) -> bool {
        *self.starts.lock() > 0
    }
    fn needs_sign_in(&self) -> bool {
        *self.needs_sign_in.lock()
    }
    fn start(&self) {
        *self.starts.lock() += 1;
    }
    fn close(&self) -> BoxFuture<'static, ()> {
        *self.closed.lock() = true;
        if let Some(f) = self.on_close.lock().as_ref() {
            f();
        }
        async {}.boxed()
    }
    fn door(&self) -> Option<Arc<dyn eve_lite_web::AppsDoor>> {
        self.me.upgrade().map(|me| me as Arc<dyn eve_lite_web::AppsDoor>)
    }
    fn brain(&self) -> Option<Arc<eve_composio_tools::ComposioBridge>> {
        None
    }
}

#[async_trait::async_trait]
impl eve_lite_web::AppsDoor for FakeApps {
    fn needs_sign_in(&self) -> anyhow::Result<bool> {
        Ok(*self.needs_sign_in.lock())
    }
    async fn connections(&self, _toolkits: Option<&[&str]>) -> anyhow::Result<indexmap::IndexMap<String, bool>> {
        Ok(indexmap::IndexMap::new())
    }
    async fn connect_link(&self, _toolkit: &str) -> anyhow::Result<Option<String>> {
        Ok(None)
    }
}

/// The bridges the daemon made.
#[derive(Clone, Default)]
pub struct Made(pub Arc<Mutex<Vec<Arc<FakeApps>>>>);

impl Made {
    pub fn all(&self) -> Vec<Arc<FakeApps>> {
        self.0.lock().clone()
    }
}

/// Network and macOS stand-ins a test reads back.
#[derive(Clone, Default)]
pub struct Stubs {
    pub spend: SpendLoop,
    pub made: Made,
    pub alerts: Arc<Mutex<usize>>,
    pub probes: Arc<Mutex<Vec<String>>>,
}

impl Stubs {
    /// The seams: the spend figures' loop and the Composio bridge replaced (the Python autouse fixture).
    pub fn seams(&self) -> Seams {
        let spend = self.spend.clone();
        let made = self.made.clone();
        Seams {
            spend_loop: Arc::new(move |mut shutdown: tokio::sync::watch::Receiver<bool>| {
                *spend.0.lock() += 1;
                async move {
                    let _ = shutdown.wait_for(|set| *set).await;
                }
                .boxed()
            }),
            make_bridge: Arc::new(move |token_path: &Path| {
                let apps = Arc::new_cyclic(|me| FakeApps {
                    me: me.clone(),
                    token_path: token_path.to_path_buf(),
                    starts: Mutex::new(0),
                    closed: Mutex::new(false),
                    needs_sign_in: Mutex::new(false),
                    on_close: Mutex::new(None),
                });
                made.0.lock().push(apps.clone());
                apps as Arc<dyn LiteApps>
            }),
            ..Seams::default()
        }
    }

    /// WebContext's keywords for a test: ephemeral ports, the LAN door on loopback, and macOS and the network
    /// replaced; `extra` last.
    pub fn web_options(&self, extra: Option<eve_lite::WebOptions>) -> eve_lite::WebOptions {
        let alerts = self.alerts.clone();
        let probes = self.probes.clone();
        Arc::new(move |p: &mut eve_lite_web::Parts| {
            p.local_ports = vec![0];
            p.lan_ports = vec![0];
            p.lan_hosts = vec!["127.0.0.1".into()];
            p.hostname = Arc::new(|| Ok(HOSTNAME.to_string()));
            p.addresses = Arc::new(|| Ok(vec![LAN_IP.to_string()]));
            let alerts = alerts.clone();
            p.alert = Arc::new(move || {
                *alerts.lock() += 1;
                Ok(())
            });
            let probes = probes.clone();
            p.probe = Arc::new(move |peer: &std::net::SocketAddr| {
                probes.lock().push(peer.to_string());
                Ok("ok".to_string())
            });
            p.credit = Arc::new(|_key: String| async { Ok(json!({"ok": true, "usd": 4.25, "funded": true})) }.boxed());
            p.restart_delay = 0.0;
            if let Some(extra) = &extra {
                extra(p);
            }
        })
    }

    pub fn options(&self) -> LiteOptions {
        LiteOptions {
            socket_path: Some(sock().to_string_lossy().into_owned()),
            web_options: Some(self.web_options(None)),
            seams: self.seams(),
            ..LiteOptions::default()
        }
    }
}

// ---- running a daemon ------------------------------------------------------------------------------------------

pub type RunTask = tokio::task::JoinHandle<anyhow::Result<i32>>;

/// A LiteDaemon serving (on_ready seen): the daemon and its run task, whose result is the exit code.
pub async fn start(mut options: LiteOptions) -> (Arc<LiteDaemon>, RunTask) {
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let tx = Arc::new(Mutex::new(Some(tx)));
    let earlier = options.on_ready.clone();
    options.on_ready = Some(Arc::new(move |d: &Arc<LiteDaemon>| {
        if let Some(earlier) = &earlier {
            earlier(d)?;
        }
        if let Some(tx) = tx.lock().take() {
            let _ = tx.send(());
        }
        Ok(())
    }));
    let daemon = LiteDaemon::new(options);
    let runner = daemon.clone();
    let mut task: RunTask = tokio::spawn(async move { runner.run().await });
    tokio::select! {
        ready = tokio::time::timeout(Duration::from_secs(60), rx) => {
            assert!(matches!(ready, Ok(Ok(()))), "the daemon was not ready in time");
        }
        done = &mut task => panic!("the daemon stopped before it was ready: {:?}", done.map(|r| r.map_err(|e| e.to_string()))),
    }
    (daemon, task)
}

/// Shut the daemon down and wait for its exit code.
pub async fn finish(daemon: &Arc<LiteDaemon>, task: RunTask) -> i32 {
    daemon.shutdown();
    exit_of(task).await
}

pub async fn exit_of(task: RunTask) -> i32 {
    tokio::time::timeout(Duration::from_secs(60), task).await.expect("stopped in time").expect("joined").expect("ran")
}

/// One IPC request, as `eve-bridge web` sends it: one JSON line, one back.
pub async fn ask(sock: &Path, req: &[u8]) -> Value {
    let mut stream = tokio::net::UnixStream::connect(sock).await.expect("connect");
    stream.write_all(req).await.unwrap();
    stream.write_all(b"\n").await.unwrap();
    let mut reader = tokio::io::BufReader::new(stream);
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(20), reader.read_line(&mut line)).await.expect("answer").unwrap();
    serde_json::from_str(&line).expect("json")
}

pub async fn ask_json(sock: &Path, req: Value) -> Value {
    ask(sock, req.to_string().as_bytes()).await
}

/// One HTTP answer.
#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    pub cookies: Vec<String>,
}

impl Reply {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("json body")
    }

    pub fn cookie(&self, name: &str) -> String {
        for line in &self.cookies {
            let pair = line.split(';').next().unwrap_or("");
            if pair.starts_with(&format!("{name}=")) {
                return pair.to_string();
            }
        }
        panic!("no {name} cookie in {:?}", self.cookies);
    }
}

/// One request to the Mac's page, as its own browser sends it (Host, and Origin on a POST).
pub async fn http(port: u16, method: &str, path: &str, body: Option<Value>, cookie: &str) -> Reply {
    let host = format!("localhost:{port}");
    let data = body.map(|b| b.to_string().into_bytes()).unwrap_or_default();
    let mut lines = vec![format!("{method} {path} HTTP/1.1"), format!("Host: {host}"), format!("User-Agent: {MAC_SAFARI}")];
    if method != "GET" {
        lines.push(format!("Origin: http://{host}"));
        lines.push("Content-Type: application/json".into());
        lines.push(format!("Content-Length: {}", data.len()));
    }
    if !cookie.is_empty() {
        lines.push(format!("Cookie: {cookie}"));
    }
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.expect("connect");
    let mut raw = (lines.join("\r\n") + "\r\n\r\n").into_bytes();
    raw.extend_from_slice(&data);
    stream.write_all(&raw).await.unwrap();
    let mut got = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), stream.read_to_end(&mut got)).await.expect("answered").unwrap();
    let split = got.windows(4).position(|w| w == b"\r\n\r\n").expect("a head");
    let head = String::from_utf8_lossy(&got[..split]).into_owned();
    let payload = got[split + 4..].to_vec();
    let mut head_lines = head.split("\r\n");
    let status: u16 = head_lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
    let mut headers = HashMap::new();
    let mut cookies = Vec::new();
    for line in head_lines {
        let (name, value) = line.split_once(':').unwrap_or((line, ""));
        let name = name.trim().to_lowercase();
        if name == "set-cookie" {
            cookies.push(value.trim().to_string());
        }
        headers.insert(name, value.trim().to_string());
    }
    let body = if headers.get("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        dechunk(&payload)
    } else {
        payload
    };
    Reply { status, headers, body, cookies }
}

fn dechunk(mut raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(end) = raw.windows(2).position(|w| w == b"\r\n") {
        let size = usize::from_str_radix(String::from_utf8_lossy(&raw[..end]).split(';').next().unwrap().trim(), 16)
            .unwrap_or(0);
        raw = &raw[end + 2..];
        if size == 0 || raw.len() < size {
            break;
        }
        out.extend_from_slice(&raw[..size]);
        raw = &raw[(size + 2).min(raw.len())..];
    }
    out
}

/// The Mac's browser signs in as `eve-bridge web open` has it: a code over the IPC, then the page. Its cookie.
pub async fn mac_sign_in(sock: &Path, port: u16) -> String {
    let login = ask_json(sock, json!({"evt": "web_login"})).await;
    assert_eq!(login["ok"], json!(true));
    assert_eq!(login["port"], json!(port));
    let reply = http(port, "POST", eve_lite_web::MAC_LOGIN_PATH, Some(json!({"code": login["code"]})), "").await;
    assert_eq!(reply.status, 200, "{}", String::from_utf8_lossy(&reply.body));
    reply.cookie(eve_web_auth::COOKIE_MAC)
}

pub async fn until(pred: impl Fn() -> bool, secs: f64) {
    for _ in 0..((secs / 0.01) as usize) {
        if pred() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting");
}

/// Read a file whole, as text.
pub fn read(path: &Path) -> String {
    let mut s = String::new();
    std::fs::File::open(path).unwrap().read_to_string(&mut s).unwrap();
    s
}
