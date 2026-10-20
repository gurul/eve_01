//! eve's browser on local Steel (steel_local.py): the server start, sessions and release.
//! Port of bridge/tests/test_steel_local.py (the steel_local parts; the lane and Meet parts belong to their crates).

use super::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Answers steel_local's HTTP calls: /v1/health, /v1/sessions, /v1/sessions/release.
struct FakeSteel {
    up: Arc<AtomicBool>,
    ws: String,
    calls: Mutex<Vec<(String, String, Option<Value>)>>,
}

impl FakeSteel {
    fn new(up: bool) -> Self {
        Self::with_ws(up, "ws://127.0.0.1:3000/")
    }
    fn with_ws(up: bool, ws: &str) -> Self {
        FakeSteel { up: Arc::new(AtomicBool::new(up)), ws: ws.into(), calls: Mutex::new(Vec::new()) }
    }
    fn calls(&self) -> Vec<(String, String, Option<Value>)> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl Http for FakeSteel {
    async fn request(&self, method: &str, url: &str, body: Option<&Value>, _timeout: f64)
        -> Result<Map<String, Value>, HttpError> {
        let tail = url.split_once("/v1/").map(|(_, t)| t).unwrap_or(url);
        self.calls.lock().unwrap().push((method.into(), tail.into(), body.cloned()));
        if !self.up.load(Ordering::SeqCst) {
            return Err(HttpError("connection refused".into()));
        }
        let v = if url.ends_with("/v1/health") {
            serde_json::json!({"status": "ok"})
        } else if url.ends_with("/v1/sessions") {
            serde_json::json!({"id": "00000000-0000-4000-8000-000000000001", "websocketUrl": self.ws})
        } else {
            serde_json::json!({"success": true})
        };
        Ok(v.as_object().unwrap().clone())
    }
}

/// Records each start instead of running it; optionally brings the fake server up.
#[derive(Default)]
struct FakeSpawn {
    started: Mutex<Vec<Launch>>,
    bring_up: Option<Arc<AtomicBool>>,
}

impl Spawn for FakeSpawn {
    fn spawn(&self, launch: Launch) -> io::Result<()> {
        self.started.lock().unwrap().push(launch);
        if let Some(up) = &self.bring_up {
            up.store(true, Ordering::SeqCst); // it comes up after the start
        }
        Ok(())
    }
}

/// `sleep=lambda s: None`, with a clock that counts 0, 1, 2, … on each read (or stands still at 0).
struct FakeClock {
    t: AtomicU64,
    ticking: bool,
}

#[async_trait]
impl Clock for FakeClock {
    fn now(&self) -> f64 {
        if self.ticking { self.t.fetch_add(1, Ordering::SeqCst) as f64 } else { 0.0 }
    }
    async fn sleep(&self, _secs: f64) {}
}

static STILL: FakeClock = FakeClock { t: AtomicU64::new(0), ticking: false };

fn cfg(tmp: &Path) -> SteelConfig {
    cfg_url(tmp, DEFAULT_URL)
}

fn cfg_url(tmp: &Path, url: &str) -> SteelConfig {
    SteelConfig { url: url.into(), steel_dir: tmp.join("steel-browser"), profile: tmp.join("profile") }
}

fn deps<'a>(tmp: &Path, http: &'a dyn Http, spawn: &'a dyn Spawn, clock: &'a dyn Clock) -> Deps<'a> {
    Deps { http, spawn, clock, wait_secs: START_SECS, log_file: tmp.join("Logs").join("eve-steel.log") }
}

fn with_launcher(c: &SteelConfig) {
    std::fs::create_dir(&c.steel_dir).unwrap();
    std::fs::write(c.steel_dir.join(LAUNCHER), "#!/bin/sh\n").unwrap();
}

fn steel_err(e: &anyhow::Error) -> String {
    e.downcast_ref::<SteelError>().unwrap_or_else(|| panic!("not a SteelError: {e}")).0.clone()
}

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let m: Vec<(String, String)> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    move |k| m.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
}

// ---- configuration --------------------------------------------------------------------------------------

#[test]
fn test_steel_settings_have_local_defaults_and_can_be_moved() {
    let d = configured_from(env(&[]));
    let home = std::env::var("HOME").unwrap();
    assert_eq!(d.url, "http://127.0.0.1:3000");
    assert_eq!(d.profile, PathBuf::from(format!("{}/.config/eve-bridge/steel-profile", home.trim_end_matches('/'))));
    assert_eq!(d.steel_dir, PathBuf::from(format!("{}/Documents/personal/steel-browser", home.trim_end_matches('/'))));
    assert_eq!(d, SteelConfig::default());
    let m = configured_from(env(&[
        ("EVE_STEEL_URL", "http://localhost:3100/"),
        ("EVE_STEEL_DIR", "/opt/steel"),
        ("EVE_STEEL_PROFILE", "/tmp/p"),
    ]));
    assert_eq!(
        (m.url.as_str(), m.steel_dir.as_path(), m.profile.as_path()),
        ("http://localhost:3100", Path::new("/opt/steel"), Path::new("/tmp/p"))
    );
}

#[test]
fn blank_settings_mean_the_defaults_and_values_are_trimmed() {
    let b = configured_from(env(&[("EVE_STEEL_URL", "  "), ("EVE_STEEL_DIR", " "), ("EVE_STEEL_PROFILE", "")]));
    assert_eq!(b, SteelConfig::default());
    let s = configured_from(env(&[("EVE_STEEL_URL", " http://127.0.0.1:3000/// "), ("EVE_STEEL_DIR", " /opt//steel/ ")]));
    assert_eq!(s.url, "http://127.0.0.1:3000");
    assert_eq!(s.steel_dir.to_str(), Some("/opt/steel")); // str(Path) as pathlib spells it
    assert_eq!(configured_from(env(&[("EVE_STEEL_URL", "/")])).url, DEFAULT_URL); // "/" strips to nothing
}

#[test]
fn paths_expand_and_normalise_like_pathlib() {
    let home = std::env::var("HOME").unwrap();
    assert_eq!(py_path("~").to_str().unwrap(), home.trim_end_matches('/'));
    assert_eq!(py_path("./a/./b/").to_str(), Some("a/b"));
    assert_eq!(py_path(".").to_str(), Some("."));
    assert_eq!(py_path("//x").to_str(), Some("//x"));
    assert_eq!(py_path("///x").to_str(), Some("/x"));
    assert_eq!(expanduser("~nobody-here-at-all/x"), "~nobody-here-at-all/x");
    assert_eq!(expanduser("a/~"), "a/~");
}

// ---- urlsplit -------------------------------------------------------------------------------------------

#[test]
fn loopback_hosts_are_read_like_urlsplit() {
    for u in ["http://127.0.0.1:3000", "http://LOCALHOST:3100", "http://[::1]:3000", "ws://127.0.0.1:3000/", "http://u:p@localhost"] {
        assert!(loopback(u).unwrap(), "{u}");
    }
    for u in ["http://192.168.1.20:3000", "https://api.steel.dev", "127.0.0.1:3000", "", "http://localhost.evil.com"] {
        assert!(!loopback(u).unwrap(), "{u}");
    }
    assert_eq!(hostname("http://[FE80::1%EN0]:3").unwrap().as_deref(), Some("fe80::1%EN0"));
    assert!(hostname("http://[::1:3000").is_err()); // Invalid IPv6 URL
    assert!(hostname("http://[127.0.0.1]:3000").is_err());
}

#[test]
fn ports_are_read_like_urlsplit() {
    assert_eq!(port("http://127.0.0.1:3100").unwrap(), Some(3100));
    assert_eq!(port("http://127.0.0.1").unwrap(), None);
    assert_eq!(port("http://127.0.0.1:").unwrap(), None);
    assert_eq!(port("http://[::1]:0080/x").unwrap(), Some(80));
    assert_eq!(
        port("http://127.0.0.1:abc").unwrap_err().0,
        "Port could not be cast to integer value as 'abc'"
    );
    assert_eq!(port("http://127.0.0.1:70000").unwrap_err().0, "Port out of range 0-65535");
}

// ---- the server -----------------------------------------------------------------------------------------

#[tokio::test]
async fn test_a_running_server_is_used_and_nothing_is_started() {
    let tmp = tempfile::tempdir().unwrap();
    let (steel, spawn) = (FakeSteel::new(true), FakeSpawn::default());
    ensure_server_with(&cfg(tmp.path()), &deps(tmp.path(), &steel, &spawn, &STILL)).await.unwrap();
    assert!(spawn.started.lock().unwrap().is_empty());
}

#[tokio::test]
async fn test_a_server_that_is_down_is_started_on_loopback_with_eves_profile() {
    let tmp = tempfile::tempdir().unwrap();
    let c = cfg(tmp.path());
    with_launcher(&c);
    let steel = FakeSteel::new(false);
    let spawn = FakeSpawn { bring_up: Some(steel.up.clone()), ..Default::default() };
    let d = deps(tmp.path(), &steel, &spawn, &STILL);
    ensure_server_with(&c, &d).await.unwrap();
    let started = spawn.started.lock().unwrap();
    assert_eq!(started.len(), 1);
    let s = &started[0];
    assert_eq!(s.argv, vec![c.steel_dir.join(LAUNCHER).to_string_lossy().into_owned()]);
    assert!(s.start_new_session);
    assert_eq!(s.cwd, c.steel_dir);
    assert_eq!(s.env_var("HOST"), Some(OsStr::new("127.0.0.1")));
    assert_eq!(s.env_var("PORT"), Some(OsStr::new("3000")));
    assert_eq!(s.env_var("CHROME_USER_DATA_DIR"), Some(c.profile.as_os_str()));
    if let Some(path) = std::env::var_os("PATH") {
        assert_eq!(s.env_var("PATH"), Some(path.as_os_str())); // the daemon's environment is passed on
    }
    assert!(c.profile.is_dir());
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&c.profile).unwrap().permissions().mode() & 0o777, 0o700);
    assert!(d.log_file.is_file());
}

#[tokio::test]
async fn the_port_comes_from_the_url_and_zero_means_3000() {
    let tmp = tempfile::tempdir().unwrap();
    for (url, want) in [("http://localhost:3100", "3100"), ("http://localhost:0", "3000"), ("http://localhost", "3000")] {
        let c = SteelConfig { url: url.into(), ..cfg(tmp.path()) };
        if !c.steel_dir.exists() {
            with_launcher(&c);
        }
        let steel = FakeSteel::new(false);
        let spawn = FakeSpawn { bring_up: Some(steel.up.clone()), ..Default::default() };
        ensure_server_with(&c, &deps(tmp.path(), &steel, &spawn, &STILL)).await.unwrap();
        assert_eq!(spawn.started.lock().unwrap()[0].env_var("PORT"), Some(OsStr::new(want)), "{url}");
    }
}

#[tokio::test]
async fn a_malformed_port_is_not_a_steel_error() {
    let tmp = tempfile::tempdir().unwrap();
    let c = cfg_url(tmp.path(), "http://127.0.0.1:x");
    with_launcher(&c);
    let (steel, spawn) = (FakeSteel::new(false), FakeSpawn::default());
    let e = ensure_server_with(&c, &deps(tmp.path(), &steel, &spawn, &STILL)).await.unwrap_err();
    assert!(e.downcast_ref::<SteelError>().is_none() && e.downcast_ref::<UrlError>().is_some());
    assert!(spawn.started.lock().unwrap().is_empty());
}

#[tokio::test]
async fn test_a_server_that_never_answers_is_an_error_naming_the_log() {
    let tmp = tempfile::tempdir().unwrap();
    let c = cfg(tmp.path());
    with_launcher(&c);
    let (steel, spawn) = (FakeSteel::new(false), FakeSpawn::default());
    let clock = FakeClock { t: AtomicU64::new(0), ticking: true };
    let d = Deps { wait_secs: 5.0, ..deps(tmp.path(), &steel, &spawn, &clock) };
    let e = ensure_server_with(&c, &d).await.unwrap_err();
    let msg = steel_err(&e);
    assert!(msg.contains("eve-steel.log"), "{msg}");
    assert_eq!(msg, format!("Steel did not answer within 5 s: see {}", d.log_file.display()));
    // one health check before the start, then one per half-second tick until the deadline (t = 1, 2, 3, 4)
    assert_eq!(steel.calls().len(), 5);
}

#[tokio::test]
async fn test_no_checkout_means_a_plain_error_and_no_start() {
    let tmp = tempfile::tempdir().unwrap();
    let c = cfg(tmp.path());
    let (steel, spawn) = (FakeSteel::new(false), FakeSpawn::default());
    let e = ensure_server_with(&c, &deps(tmp.path(), &steel, &spawn, &STILL)).await.unwrap_err();
    let msg = steel_err(&e);
    assert!(msg.contains("docs/steel.md"), "{msg}");
    assert_eq!(
        msg,
        format!("Steel is not running and {} is missing: see docs/steel.md", c.steel_dir.join(LAUNCHER).display())
    );
    assert!(spawn.started.lock().unwrap().is_empty());
    assert!(!c.profile.exists());
}

#[tokio::test]
async fn test_steel_off_this_mac_is_refused_before_any_request() {
    for url in ["http://192.168.1.20:3000", "https://api.steel.dev"] {
        let tmp = tempfile::tempdir().unwrap();
        let (steel, spawn) = (FakeSteel::new(true), FakeSpawn::default());
        let e = ensure_server_with(&cfg_url(tmp.path(), url), &deps(tmp.path(), &steel, &spawn, &STILL))
            .await
            .unwrap_err();
        let msg = steel_err(&e);
        assert!(msg.contains("must be on this Mac"), "{msg}");
        assert_eq!(msg, format!("EVE_STEEL_URL must be on this Mac (127.0.0.1 or localhost), not {url}"));
        assert!(steel.calls().is_empty());
    }
}

// ---- sessions -------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_a_session_returns_its_local_websocket() {
    let tmp = tempfile::tempdir().unwrap();
    let (steel, spawn) = (FakeSteel::new(true), FakeSpawn::default());
    let ws = start_session_with(&cfg(tmp.path()), &deps(tmp.path(), &steel, &spawn, &STILL), false).await.unwrap();
    assert_eq!(ws, "ws://127.0.0.1:3000/");
    assert!(steel.calls().contains(&("POST".into(), "sessions".into(), Some(serde_json::json!({"blockAds": true})))));
}

#[tokio::test]
async fn test_a_visible_session_asks_for_a_window() {
    let tmp = tempfile::tempdir().unwrap();
    let (steel, spawn) = (FakeSteel::new(true), FakeSpawn::default());
    start_session_with(&cfg(tmp.path()), &deps(tmp.path(), &steel, &spawn, &STILL), true).await.unwrap();
    let want = serde_json::json!({"blockAds": true, "headless": false});
    assert!(steel.calls().contains(&("POST".into(), "sessions".into(), Some(want))));
}

#[tokio::test]
async fn test_a_session_without_a_local_websocket_is_refused() {
    for ws in ["", "http://127.0.0.1:3000/", "ws://203.0.113.9:3000/"] {
        let tmp = tempfile::tempdir().unwrap();
        let (steel, spawn) = (FakeSteel::with_ws(true, ws), FakeSpawn::default());
        let e = start_session_with(&cfg(tmp.path()), &deps(tmp.path(), &steel, &spawn, &STILL), false)
            .await
            .unwrap_err();
        let msg = steel_err(&e);
        assert!(msg.contains("websocket"), "{msg}");
        let shown = if ws.is_empty() { "none" } else { ws };
        assert_eq!(msg, format!("Steel answered without a local websocket URL ({shown})"));
    }
}

/// Steel is up for health but refuses the session itself.
struct RefusesSessions;

#[async_trait]
impl Http for RefusesSessions {
    async fn request(&self, _m: &str, url: &str, _b: Option<&Value>, _t: f64) -> Result<Map<String, Value>, HttpError> {
        if url.ends_with("/v1/health") {
            Ok(serde_json::json!({"status": "ok"}).as_object().unwrap().clone())
        } else {
            Err(HttpError("HTTP Error 500: Internal Server Error".into()))
        }
    }
}

#[tokio::test]
async fn a_refused_session_is_a_steel_error_with_its_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let spawn = FakeSpawn::default();
    let e = start_session_with(&cfg(tmp.path()), &deps(tmp.path(), &RefusesSessions, &spawn, &STILL), false)
        .await
        .unwrap_err();
    assert_eq!(steel_err(&e), "Steel refused a new session: HTTP Error 500: Internal Server Error");
}

#[tokio::test]
async fn test_release_is_quiet_when_steel_is_gone() {
    let tmp = tempfile::tempdir().unwrap();
    let steel = FakeSteel::new(false);
    release_with(&cfg(tmp.path()), &steel).await; // no error
    assert_eq!(steel.calls(), vec![("POST".into(), "sessions/release".into(), Some(serde_json::json!({})))]);
}

#[test]
fn values_read_like_python_str() {
    assert_eq!(or_default(Some(&serde_json::json!("abc")), "?"), "abc");
    assert_eq!(or_default(Some(&serde_json::json!(0)), "?"), "?");
    assert_eq!(or_default(Some(&serde_json::json!(true)), "?"), "True");
    assert_eq!(or_default(Some(&serde_json::json!(1.0)), "?"), "1.0");
    assert_eq!(or_default(Some(&serde_json::json!(["a"])), "?"), "['a']");
    assert_eq!(or_default(None, ""), "");
    assert_eq!(py_dumps(&serde_json::json!({"blockAds": true, "headless": false})), r#"{"blockAds": true, "headless": false}"#);
    assert_eq!(py_dumps(&serde_json::json!({"k": "é\n", "n": [1, 2.5]})), "{\"k\": \"\\u00e9\\n\", \"n\": [1, 2.5]}");
}

// ---- the real HTTP client and launcher ----------------------------------------------------------------------

/// A one-request HTTP server on loopback: returns the raw request it read.
async fn serve_once(status: &str, body: &str) -> (String, tokio::task::JoinHandle<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (status, body) = (status.to_string(), body.to_string());
    let h = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = s.read(&mut chunk).await.unwrap();
            buf.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&buf).to_string();
            if let Some(i) = text.find("\r\n\r\n") {
                let len = text[..i]
                    .lines()
                    .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                    .unwrap_or(0);
                if buf.len() >= i + 4 + len || n == 0 {
                    break;
                }
            }
            if n == 0 {
                break;
            }
        }
        let resp = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        s.write_all(resp.as_bytes()).await.unwrap();
        String::from_utf8_lossy(&buf).to_string()
    });
    (format!("http://{addr}"), h)
}

#[tokio::test]
async fn the_http_client_speaks_json_like_urllib() {
    let (url, h) = serve_once("200 OK", r#"{"status": "ok"}"#).await;
    let c = SteelConfig { url, ..SteelConfig::default() };
    assert!(healthy(&c, &DefaultHttp).await);
    let req = h.await.unwrap();
    assert!(req.starts_with("GET /v1/health HTTP/1.1\r\n"), "{req}");
    assert!(req.to_ascii_lowercase().contains("content-type: application/json"), "{req}");

    let (url, h) = serve_once("200 OK", "").await;
    let body = serde_json::json!({"blockAds": true, "headless": false});
    let got = DefaultHttp.request("POST", &format!("{url}/v1/sessions"), Some(&body), HTTP_TIMEOUT).await.unwrap();
    assert!(got.is_empty()); // an empty reply reads as {}
    let req = h.await.unwrap();
    assert!(req.starts_with("POST /v1/sessions HTTP/1.1\r\n"), "{req}");
    assert!(req.ends_with("\r\n\r\n{\"blockAds\": true, \"headless\": false}"), "{req}");
}

#[tokio::test]
async fn the_http_client_fails_on_error_status_bad_json_and_no_server() {
    let (url, h) = serve_once("503 Service Unavailable", r#"{"status": "ok"}"#).await;
    let e = DefaultHttp.request("GET", &format!("{url}/v1/health"), None, 2.0).await.unwrap_err();
    assert_eq!(e.0, "HTTP Error 503: Service Unavailable");
    h.await.unwrap();

    let (url, h) = serve_once("200 OK", "not json").await;
    assert!(!healthy(&SteelConfig { url, ..SteelConfig::default() }, &DefaultHttp).await);
    h.await.unwrap();

    let (url, h) = serve_once("200 OK", r#"{"status": "starting"}"#).await;
    assert!(!healthy(&SteelConfig { url, ..SteelConfig::default() }, &DefaultHttp).await);
    h.await.unwrap();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    assert!(!healthy(&SteelConfig { url: format!("http://{addr}"), ..SteelConfig::default() }, &DefaultHttp).await);
}

#[tokio::test]
async fn the_launcher_runs_in_its_own_session_with_its_env_cwd_and_log() {
    let tmp = tempfile::tempdir().unwrap();
    let script = tmp.path().join(LAUNCHER);
    std::fs::write(
        &script,
        "#!/bin/sh\necho \"$HOST $PORT $CHROME_USER_DATA_DIR\"\npwd -P\n\
         [ \"$(ps -o pgid= -p $$ | tr -d ' ')\" = \"$$\" ] && echo leader\necho done >&2\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let log = tmp.path().join("eve-steel.log");
    std::fs::write(&log, "before\n").unwrap();
    let out = std::fs::OpenOptions::new().append(true).create(true).open(&log).unwrap();
    let env = vec![
        ("PATH".into(), std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into())),
        ("HOST".into(), "127.0.0.1".into()),
        ("PORT".into(), "3000".into()),
        ("CHROME_USER_DATA_DIR".into(), "/p".into()),
    ];
    ProcessSpawn
        .spawn(Launch { argv: vec![script.to_string_lossy().into_owned()], env, cwd: tmp.path().into(), out, start_new_session: true })
        .unwrap();
    let mut text = String::new();
    for _ in 0..100 {
        text = std::fs::read_to_string(&log).unwrap();
        if text.contains("done") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let cwd = std::fs::canonicalize(tmp.path()).unwrap();
    assert!(text.starts_with("before\n127.0.0.1 3000 /p\n"), "{text}");
    assert!(text.contains(&format!("{}\n", cwd.display())), "{text}");
    assert!(text.contains("leader\n"), "{text}");
}

// ---- live: the local Steel server on this Mac -----------------------------------------------------------

#[tokio::test]
#[ignore = "live: needs EVE_LIVE=1 and a steel-browser checkout on this Mac"]
async fn test_live_a_session_through_local_steel_starting_it_when_down() {
    if std::env::var("EVE_LIVE").as_deref() != Ok("1") {
        return;
    }
    let c = configured();
    if !c.steel_dir.join(LAUNCHER).is_file() {
        eprintln!("no steel-browser checkout at {}", c.steel_dir.display());
        return;
    }
    let ws = start_session(&c, false).await.unwrap();
    assert!(ws.starts_with("ws://") && loopback(&ws).unwrap(), "{ws}");
    release(&c).await;
}
