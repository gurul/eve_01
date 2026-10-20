//! watch-pages: test_watch_security.py, test_watch_tls.py, test_watch_shapes_replay.py and test_watch_tinyfish.py,
//! as far as they test the page readers (structured data, the guarded HTTP read, the Chrome-like read, the hosted
//! read, the render and its guard proxy). Servers are real sockets on loopback standing in for a public site; the
//! address checks are `AddressRules`, which the Python tests monkeypatch as `check_url`, `_ip_public` and
//! `socket.getaddrinfo`.
//!
//! The Chrome-like read runs its curl_cffi helper with EVE_WATCH_TLS_PYTHON (from this process's environment)
//! or the bridge venv; those tests are skipped, as Python's are, when that interpreter has no curl_cffi.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eve_web::watch::net::{http_request, ip_public_here, AddressRules, HttpOptions};
use eve_web::watch::page::{main_text, price, structured};
use eve_web::watch::render::{clear_overlays, overlay_choice, page_of, render, render_command, GuardProxy, DENIED};
use eve_web::watch::tls::{tls_available_with, tls_request_with, tls_worth, TLS_MAX_HOPS, TOO_LONG};
use eve_web::watch::{fetch_hosted, FetchError, MAX_BYTES};
use eve_web::Ctx;
use serde_json::{json, Value};

// ---- local servers ------------------------------------------------------------------------------

/// One request as the test server read it.
#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Req {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

type Handler = Arc<dyn Fn(&Req, &mut TcpStream) + Send + Sync>;

/// A threaded HTTP/1.1 server on `host`, one request per connection; stopped when dropped.
struct Server {
    port: u16,
    stop: Arc<AtomicBool>,
    host: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&format!("{}:{}", self.host, self.port).parse().unwrap(), Duration::from_secs(1));
    }
}

fn read_req(stream: &mut TcpStream) -> Option<Req> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => return None,
        }
    }
    let text = String::from_utf8_lossy(&head).to_string();
    let mut lines = text.split("\r\n");
    let first: Vec<&str> = lines.next()?.splitn(3, ' ').collect();
    if first.len() < 2 {
        return None;
    }
    let headers: Vec<(String, String)> = lines
        .filter(|l| !l.is_empty())
        .filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
        .collect();
    let mut req = Req { method: first[0].into(), path: first[1].into(), headers, body: Vec::new() };
    let length: usize = req.header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut body = vec![0u8; length];
    if stream.read_exact(&mut body).is_err() {
        return None;
    }
    req.body = body;
    Some(req)
}

fn serve_on(host: &str, handler: Handler) -> Server {
    let listener = TcpListener::bind(format!("{host}:0")).expect("a loopback port");
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            if flag.load(Ordering::SeqCst) {
                break;
            }
            let Ok(mut conn) = conn else { continue };
            let handler = handler.clone();
            std::thread::spawn(move || {
                if let Some(req) = read_req(&mut conn) {
                    handler(&req, &mut conn);
                }
            });
        }
    });
    Server { port, stop, host: host.to_string() }
}

fn serve(handler: impl Fn(&Req, &mut TcpStream) + Send + Sync + 'static) -> Server {
    serve_on("127.0.0.1", Arc::new(handler))
}

/// `send(status, body, **headers)`: a sized answer.
fn send(stream: &mut TcpStream, status: u16, body: &[u8], headers: &[(&str, &str)]) {
    let mut out = format!("HTTP/1.1 {status} X\r\n");
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n\r\n", body.len()));
    let _ = stream.write_all(out.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

/// The loopback test servers pass both address checks (Python's `loopback_is_public` fixture).
fn loopback_is_public() -> AddressRules {
    AddressRules { resolve: Some(Arc::new(|_: &str| true)), ip_public: Arc::new(|_: IpAddr| true), ..AddressRules::system() }
}

fn opts(timeout: f64, rules: AddressRules) -> HttpOptions {
    HttpOptions { timeout, rules: Some(rules), ..HttpOptions::default() }
}

// ---- test_watch_security.py ----------------------------------------------------------------------

/// check_url resolves the host, then the connection resolves it again. A name that answers a public address
/// first and 127.0.0.1 second must not reach the loopback service.
#[test]
fn dns_rebinding_between_the_check_and_the_connect_is_refused() {
    let hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = hits.clone();
    let srv = serve(move |req, s| {
        seen.lock().unwrap().push(req.path.clone());
        send(s, 200, b"<title>router admin</title>LAN SECRET", &[("Content-Type", "text/html")]);
    });
    let lookups = Arc::new(AtomicUsize::new(0));
    let rules = AddressRules {
        lookup: Arc::new(move |host: &str, port: u16| {
            if host == "rebind.attacker.example" {
                let n = lookups.fetch_add(1, Ordering::SeqCst) + 1;
                let ip: IpAddr = if n == 1 { "93.184.215.14".parse().unwrap() } else { "127.0.0.1".parse().unwrap() };
                return Ok(vec![SocketAddr::new(ip, port)]);
            }
            Ok((host, port).to_socket_addrs()?.collect())
        }),
        ip_public: Arc::new(ip_public_here),
        resolve: None,
    };
    let out = http_request(&format!("http://rebind.attacker.example:{}/admin", srv.port), &opts(3.0, rules));
    assert!(out.is_err(), "{out:?}");
    assert!(hits.lock().unwrap().is_empty(), "the loopback service was reached");
}

#[test]
fn an_unknown_charset_is_a_fetch_error() {
    let srv = serve(|_, s| send(s, 200, b"<p>hi</p>", &[("Content-Type", "text/html; charset=bogus-8")]));
    // a FetchError, or a read in a charset that exists (a fix too)
    if let Ok((_, body)) = http_request(&format!("http://127.0.0.1:{}/", srv.port), &opts(3.0, loopback_is_public())) {
        assert!(body.contains("hi"));
    }
}

#[test]
fn a_garbage_status_line_is_a_fetch_error() {
    let srv = serve(|_, s| {
        let _ = s.write_all(b"HELLO THERE\r\n\r\n");
    });
    assert!(http_request(&format!("http://127.0.0.1:{}/", srv.port), &opts(3.0, loopback_is_public())).is_err());
}

/// structured() and main_text() on 250 KB of unclosed ld+json script openings: Python's lazy scans were quadratic
/// (400 KB stalled the loop 17 s). Python asserts the event loop never stalls a second; here, each read is quick.
#[test]
fn a_crafted_page_never_blocks_the_event_loop() {
    let unit = "<script type=\"application/ld+json\">";
    let page = unit.repeat(250_000 / unit.len());
    let t = Instant::now();
    let r = structured(&page);
    assert!(r.value.is_none());
    let _ = main_text(&page);
    let took = t.elapsed().as_secs_f64();
    assert!(took < 2.0, "the crafted page took {took:.1} s");
}

/// A POST answered 302 to another host becomes a GET there, and must not carry the Authorization header
/// (Python replays it through `openrouter()`; the redirect rule is http_request's).
#[test]
fn a_cross_host_redirect_never_carries_the_openrouter_key() {
    let seen = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
    let saw = seen.clone();
    let other = serve(move |req, s| {
        saw.lock().unwrap().push(req.header("authorization").map(str::to_string));
        send(s, 200, b"{}", &[("Content-Type", "application/json")]);
    });
    let po = other.port;
    let bounce = serve(move |_, s| send(s, 302, b"", &[("Location", &format!("http://localhost:{po}/elsewhere"))]));
    let o = HttpOptions {
        data: Some(br#"{"model": "x"}"#.to_vec()),
        headers: vec![("Authorization".into(), "Bearer sk-or-v1-TESTKEY".into()), ("Content-Type".into(), "application/json".into())],
        timeout: 5.0,
        url_guard: None,
        rules: Some(loopback_is_public()),
    };
    let _ = http_request(&format!("http://127.0.0.1:{}/api/v1/chat/completions", bounce.port), &o); // refusing is a fix too
    for auth in seen.lock().unwrap().iter() {
        assert!(!auth.clone().unwrap_or_default().contains("TESTKEY"), "the key went to another host");
    }
}

/// The timeout is the whole answer's, not each socket read's.
#[test]
fn a_trickling_server_is_cut_at_the_deadline() {
    let srv = serve(|_, s| {
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n");
        for _ in 0..8 {
            if s.write_all(b"x").and_then(|_| s.flush()).is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(800));
        }
    });
    let t = Instant::now();
    let _ = http_request(&format!("http://127.0.0.1:{}/", srv.port), &opts(1.0, loopback_is_public()));
    let took = t.elapsed().as_secs_f64();
    assert!(took < 4.0, "ran {took:.1} s with timeout=1.0");
}

/// A render whose child never answers is killed at its deadline, with the grandchild it started.
#[test]
fn a_render_that_never_answers_is_killed_at_its_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("grandchild.pid");
    let script = format!("sleep 600 & echo $! > '{}'; sleep 600", pidfile.display());
    let t = Instant::now();
    let err = render("https://93.184.215.14/", 30.0, Some(3.0), Some(vec!["sh".into(), "-c".into(), script])).unwrap_err();
    assert!(err.reason.contains("too long to render"), "{err:?}");
    assert!(t.elapsed().as_secs_f64() < 10.0);
    assert_gone(&pidfile, "the render's grandchild (Chromium's stand-in) outlived the deadline");
}

/// test_watch_reverify's twin: a grandchild in a session of its own (as Playwright starts Chromium) goes too.
#[test]
fn a_render_that_hangs_is_killed_with_everything_it_started() {
    if std::process::Command::new("python3").arg("-c").arg("").status().map(|s| !s.success()).unwrap_or(true) {
        eprintln!("skipped: no python3 to start a grandchild in its own session");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("gc.pid");
    let child = format!(
        "import subprocess, sys, time\ng = subprocess.Popen(['sleep', '600'], start_new_session=True)\nopen({:?}, 'w').write(str(g.pid))\ntime.sleep(600)\n",
        pidfile.display().to_string()
    );
    let err = render("https://93.184.215.14/", 30.0, Some(3.0), Some(vec!["python3".into(), "-c".into(), child])).unwrap_err();
    assert!(err.reason.contains("too long to render"), "{err:?}");
    assert_gone(&pidfile, "the grandchild in its own session outlived the render's deadline");
}

fn assert_gone(pidfile: &std::path::Path, why: &str) {
    let pid: i32 = std::fs::read_to_string(pidfile).unwrap().trim().parse().unwrap();
    for _ in 0..50 {
        // SAFETY: signal 0 only asks whether the process exists.
        if unsafe { libc::kill(pid, 0) } != 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // SAFETY: the test's own stray process.
    unsafe { libc::kill(pid, libc::SIGKILL) };
    panic!("{why}");
}

/// The guard proxy carries one request per connection: a second request on a kept-alive connection never
/// reaches the first server.
#[test]
fn the_proxy_carries_one_request_per_connection() {
    let got = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = got.clone();
    let srv = serve(move |req, s| {
        seen.lock().unwrap().push(req.path.clone());
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        // keep the connection open for a second request, as an HTTP/1.1 server does
        if let Some(req) = read_req(s) {
            seen.lock().unwrap().push(req.path);
        }
    });
    let mut proxy = GuardProxy::start(AddressRules { ip_public: Arc::new(|_: IpAddr| true), ..AddressRules::system() }).unwrap();
    let mut c = TcpStream::connect(("127.0.0.1", proxy.port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(40))).unwrap();
    let port = srv.port;
    c.write_all(
        format!("GET http://127.0.0.1:{port}/first HTTP/1.1\r\nHost: x\r\n\r\nGET http://127.0.0.1:{port}/second HTTP/1.1\r\nHost: y\r\n\r\n")
            .as_bytes(),
    )
    .unwrap();
    let mut data = Vec::new();
    let _ = c.read_to_end(&mut data);
    assert_eq!(*got.lock().unwrap(), vec!["/first".to_string()]);
    assert_eq!(String::from_utf8_lossy(&data).matches("HTTP/1.").count(), 1);
    proxy.shutdown();
}

/// The guard refuses a tunnel and a plain request to a private address (loopback, under the real rules), and
/// drops the proxy's own headers on the way to a public one.
#[test]
fn the_proxy_refuses_private_addresses_and_strips_proxy_headers() {
    let got = Arc::new(Mutex::new(Vec::<Req>::new()));
    let seen = got.clone();
    let srv = serve(move |req, s| {
        seen.lock().unwrap().push(req.clone());
        send(s, 200, b"ok", &[]);
    });
    let mut real = GuardProxy::start(AddressRules::system()).unwrap();
    for request in [
        format!("CONNECT 127.0.0.1:{} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n", srv.port),
        format!("GET http://127.0.0.1:{}/lan HTTP/1.1\r\nHost: x\r\n\r\n", srv.port),
        "GET https://example.com/ HTTP/1.1\r\nHost: x\r\n\r\n".to_string(),
        format!("POST http://127.0.0.1:{}/up HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n", srv.port),
    ] {
        let mut c = TcpStream::connect(("127.0.0.1", real.port)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        c.write_all(request.as_bytes()).unwrap();
        let mut data = Vec::new();
        let _ = c.read_to_end(&mut data);
        assert_eq!(data, DENIED, "{request}");
    }
    real.shutdown();
    assert!(got.lock().unwrap().is_empty());

    let mut open = GuardProxy::start(AddressRules { ip_public: Arc::new(|_: IpAddr| true), ..AddressRules::system() }).unwrap();
    let mut c = TcpStream::connect(("127.0.0.1", open.port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(40))).unwrap();
    c.write_all(
        format!(
            "POST http://127.0.0.1:{}/p?q=1 HTTP/1.1\r\nHost: x\r\nProxy-Authorization: secret\r\nConnection: keep-alive\r\nKeep-Alive: 5\r\nContent-Length: 3\r\n\r\nabc",
            srv.port
        )
        .as_bytes(),
    )
    .unwrap();
    let mut data = Vec::new();
    let _ = c.read_to_end(&mut data);
    assert!(data.starts_with(b"HTTP/1.1 200"), "{}", String::from_utf8_lossy(&data));
    let reqs = got.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    let req = &reqs[0];
    assert_eq!((req.method.as_str(), req.path.as_str(), req.body.as_slice()), ("POST", "/p?q=1", b"abc".as_slice()));
    assert!(req.header("proxy-authorization").is_none() && req.header("keep-alive").is_none());
    assert_eq!(req.header("connection"), Some("close"));
    drop(reqs);
    open.shutdown();
}

// ---- the browser (a real Chromium: live) ------------------------------------------------------------

/// Every connection the browser makes goes through the guard proxy, which checks the address it connects to:
/// a redirect's next hop, a subresource redirect, an iframe, fetch, a WebSocket, a popup and a beacon to the
/// "LAN" (127.0.0.1, private for this test) are refused, and the page's own asset (on ::1, public for this test)
/// loads. Run in-process (render_here) so the address rule can be set for the test.
#[test]
#[ignore = "live: starts a real headless Chromium (EVE_LIVE=1 in Python)"]
fn the_render_guard_covers_redirects_websockets_and_popups() {
    use eve_web::watch::render::render_here;
    let lan_hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let page_hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let lh = lan_hits.clone();
    let lan = serve(move |req, s| {
        lh.lock().unwrap().push(format!("{}{}", req.path, if req.header("upgrade").is_some() { " [websocket]" } else { "" }));
        send(s, 200, b"<h1>LAN-SECRET-4242</h1>", &[("Content-Type", "text/html")]);
    });
    let base = format!("http://127.0.0.1:{}", lan.port);
    let pl = lan.port;
    let ph = page_hits.clone();
    let page = serve_on(
        "[::1]",
        Arc::new(move |req: &Req, s: &mut TcpStream| {
            ph.lock().unwrap().push(req.path.clone());
            match req.path.as_str() {
                "/redir" => send(s, 302, b"", &[("Location", &format!("{base}/top-redirect"))]),
                "/r2" => send(s, 302, b"", &[("Location", &format!("{base}/sub-redirect"))]),
                "/ok.png" => send(s, 200, b"", &[("Content-Type", "image/png")]),
                _ => {
                    let body = format!(
                        "<h1>$20</h1><img src=\"/ok.png\"><img src=\"/r2\"><iframe src=\"{base}/iframe\"></iframe>\
<script>fetch(\"{base}/fetch\").catch(() => {{}});try {{ new WebSocket(\"ws://127.0.0.1:{pl}/ws\"); }} catch (e) {{}}\
try {{ window.open(\"{base}/popup\"); }} catch (e) {{}}try {{ navigator.sendBeacon(\"{base}/beacon\", \"x\"); }} catch (e) {{}}</script>"
                    );
                    send(s, 200, body.as_bytes(), &[("Content-Type", "text/html")]);
                }
            }
        }),
    );
    let origin = format!("http://[::1]:{}", page.port);
    let rules = AddressRules { ip_public: Arc::new(|ip: IpAddr| ip.is_ipv6()), ..AddressRules::system() };
    if let Ok((html, _)) = render_here(&format!("{origin}/redir"), 15.0, &rules) {
        assert!(!html.contains("LAN-SECRET-4242")); // a refused top-level redirect is a fix too
    }
    let (html, _) = render_here(&format!("{origin}/page"), 15.0, &rules).expect("the page renders");
    assert!(html.contains("$20"));
    assert!(page_hits.lock().unwrap().iter().any(|p| p == "/ok.png"), "the guard is blocking everything");
    assert!(lan_hits.lock().unwrap().is_empty(), "the LAN service was reached: {:?}", lan_hits.lock().unwrap());
}

// ---- overlays (render's clear_overlays, page_of, overlay_choice) --------------------------------------

#[test]
fn only_a_button_that_closes_or_continues_is_pressed() {
    let b = |id: &str, name: &str| json!({"id": id, "name": name});
    assert_eq!(overlay_choice(&[b("0-0", "Accept all"), b("0-1", "Continue")]), Some(b("0-1", "Continue")));
    assert_eq!(overlay_choice(&[b("0-0", "Sign up"), b("0-1", "Subscribe now")]), None);
    assert_eq!(overlay_choice(&[b("0-0", "  no thanks ")]), Some(b("0-0", "  no thanks ")));
    assert_eq!(overlay_choice(&[b("0-0", "Continue to checkout and pay")]), None);
    assert_eq!(overlay_choice(&[json!({"id": "0-0", "name": null}), b("0-1", "×")]), Some(b("0-1", "×")));
    assert_eq!(overlay_choice(&[b("0-0", "Stay on LEGO.com")]), Some(b("0-0", "Stay on LEGO.com")));
}

#[test]
fn a_query_or_a_hash_is_the_same_page() {
    assert_eq!(page_of("https://Shop.example/p/?x=1#y"), page_of("https://shop.example/p"));
    assert_eq!(page_of("https://shop.example"), ("https".into(), "shop.example".into(), "/".into()));
    assert_ne!(page_of("https://shop.example/p"), page_of("https://shop.example/q"));
}

/// A scripted page for clear_overlays: each OVERLAY_JS look answers the next list; a click may move the page.
struct FakePage {
    url: Mutex<String>,
    looks: Mutex<Vec<Value>>,
    consent: Mutex<Vec<Value>>,
    clicks: Mutex<Vec<String>>,
    gotos: Mutex<Vec<String>>,
    moves_to: Option<String>,
}

impl FakePage {
    fn new(looks: Vec<Value>, moves_to: Option<&str>) -> FakePage {
        FakePage {
            url: Mutex::new("https://shop.example/p".into()),
            looks: Mutex::new(looks),
            consent: Mutex::new(Vec::new()),
            clicks: Mutex::new(Vec::new()),
            gotos: Mutex::new(Vec::new()),
            moves_to: moves_to.map(str::to_string),
        }
    }
}

impl eve_browser_lane::driver::PageHandle for FakePage {
    fn evaluate(&self, script: &str, _arg: Option<&Value>) -> anyhow::Result<Value> {
        if script == eve_browser_lane::CONSENT_JS {
            let mut c = self.consent.lock().unwrap();
            return Ok(if c.is_empty() { json!({"buttons": []}) } else { c.remove(0) });
        }
        let mut looks = self.looks.lock().unwrap();
        Ok(if looks.is_empty() { json!([]) } else { looks.remove(0) })
    }
    fn title(&self) -> anyhow::Result<String> {
        Ok(String::new())
    }
    fn url(&self) -> String {
        self.url.lock().unwrap().clone()
    }
    fn inner_text(&self, _: &str, _: u64) -> anyhow::Result<String> {
        Ok(String::new())
    }
    fn count(&self, _: &str) -> anyhow::Result<usize> {
        Ok(0)
    }
    fn scroll_into_view_if_needed(&self, _: &str, _: u64) -> anyhow::Result<()> {
        Ok(())
    }
    fn click(&self, selector: &str, _: u64) -> anyhow::Result<()> {
        self.clicks.lock().unwrap().push(selector.to_string());
        if let Some(to) = &self.moves_to {
            *self.url.lock().unwrap() = to.clone();
        }
        Ok(())
    }
    fn fill(&self, _: &str, _: &str, _: u64) -> anyhow::Result<()> {
        Ok(())
    }
    fn select_option(&self, _: &str, _: eve_browser_lane::driver::SelectBy<'_>, _: u64) -> anyhow::Result<()> {
        Ok(())
    }
    fn evaluate_on(&self, _: &str, _: &str) -> anyhow::Result<Value> {
        Ok(Value::Null)
    }
    fn keyboard_type(&self, _: &str, _: u64) -> anyhow::Result<()> {
        Ok(())
    }
    fn keyboard_press(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    fn wait_for_timeout(&self, _: u64) {}
    fn wait_for_load_state(&self, _: &str, _: u64) -> anyhow::Result<()> {
        Ok(())
    }
    fn goto(&self, url: &str, _: &str, _: u64) -> anyhow::Result<()> {
        self.gotos.lock().unwrap().push(url.to_string());
        *self.url.lock().unwrap() = url.to_string();
        Ok(())
    }
    fn bring_to_front(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn close(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn screenshot_jpeg(&self, _: u8, _: Option<(&str, &str)>) -> anyhow::Result<Vec<u8>> {
        Ok(Vec::new())
    }
}

#[test]
fn an_overlay_is_pressed_away_until_two_quiet_looks() {
    let modal = json!([{"id": "0-0", "name": "Accept all"}, {"id": "0-1", "name": "Continue"}]);
    let page = FakePage::new(vec![modal, json!([]), json!([])], None);
    assert_eq!(clear_overlays(&page), vec!["Continue".to_string()]);
    assert_eq!(*page.clicks.lock().unwrap(), vec!["[data-watch-overlay=\"0-1\"]".to_string()]);
    // a cookie notice goes through browser_lane's rule: a refusing button only
    let page = FakePage::new(vec![], None);
    page.consent.lock().unwrap().push(json!({"buttons": [{"id": "c1", "name": "Reject all"}]}));
    let pressed = clear_overlays(&page);
    let expect = eve_browser_lane::consent_choice(&[json!({"id": "c1", "name": "Reject all"})]);
    assert_eq!(pressed.len(), usize::from(expect.is_some()));
}

#[test]
fn a_button_that_went_to_another_page_loads_the_items_page_again() {
    let modal = json!([{"id": "0-0", "name": "Continue"}]);
    let page = FakePage::new(vec![modal.clone(), modal], Some("https://other.example/home"));
    assert_eq!(clear_overlays(&page), vec!["Continue".to_string()]);
    assert_eq!(*page.gotos.lock().unwrap(), vec!["https://shop.example/p".to_string()]);
    assert_eq!(page.clicks.lock().unwrap().len(), 1, "pressing stops after a button that left the page");
    // a reload of the same page (a query added) is not another page
    let page = FakePage::new(vec![json!([{"id": "0-0", "name": "Continue"}])], Some("https://shop.example/p?entered=1"));
    assert_eq!(clear_overlays(&page), vec!["Continue".to_string()]);
    assert!(page.gotos.lock().unwrap().is_empty());
}

#[test]
fn the_render_child_is_the_eve_watch_bin() {
    let cmd = render_command("https://shop.example/p", 30.0);
    assert_eq!(&cmd[1..], &["--render".to_string(), "https://shop.example/p".into(), "30.0".into()]);
    assert!(std::path::Path::new(&cmd[0]).is_file(), "{cmd:?}");
    // a timeout that is not a number: no answer, status 1 (float() raised in Python)
    let status = std::process::Command::new(&cmd[0]).args(["--render", "https://shop.example/p", "soon"]).status().unwrap();
    assert_eq!(status.code(), Some(1));
}

#[test]
fn a_private_link_is_never_rendered() {
    let err = render("http://192.168.1.1/admin", 5.0, None, Some(vec!["false".into()])).unwrap_err();
    assert!(err.reason.contains("private or unknown"), "{err:?}");
}

// ---- test_watch_shapes_replay.py -------------------------------------------------------------------

const HUGE_DIGITS: usize = 400;

fn huge() -> String {
    format!("1{}", "0".repeat(HUGE_DIGITS))
}

/// A huge integer price is dropped, not raised.
#[test]
fn a_huge_integer_price_is_dropped_not_raised() {
    let page = format!(
        "<script type=\"application/ld+json\">{{\"@type\":\"Product\",\"name\":\"X\",\"offers\":{{\"price\":{}}}}}</script>",
        huge()
    );
    assert_eq!(structured(&page).value, None);
    assert_eq!(price(&Value::String(huge()), ""), None);
}

#[test]
fn a_meta_itemprop_price_in_a_review_is_not_the_items() {
    let page = "<div itemscope itemtype=\"https://schema.org/Product\"><h1>Mug</h1>\
<div itemprop=\"offers\" itemscope itemtype=\"https://schema.org/Offer\">\
<span itemprop=\"price\">$1,299.00</span><meta content=\"USD\" itemprop=\"priceCurrency\">\
<link itemprop='availability' href='https://schema.org/OutOfStock'></div>\
<div itemprop=\"review\" itemscope itemtype=\"https://schema.org/Review\">\
<meta itemprop=\"price\" content=\"5\"></div></div>";
    let r = structured(page);
    assert_eq!((r.value, r.currency.as_str()), (Some(1299.0), "USD"));
}

#[test]
fn a_later_products_meta_price_is_not_the_items_and_the_currency_is_kept() {
    let page = "<div itemscope itemtype=\"https://schema.org/Product\"><h1 itemprop=\"name\">Camera X</h1>\
<div itemprop=\"offers\" itemscope itemtype=\"https://schema.org/Offer\">\
<meta itemprop=\"price\" content=\"499.00\"><meta itemprop=\"priceCurrency\" content=\"EUR\"></div></div>\
<section class=\"also-like\"><div itemscope itemtype=\"https://schema.org/Product\">\
<span itemprop=\"name\">Lens cap</span><div itemprop=\"offers\" itemscope itemtype=\"https://schema.org/Offer\">\
<meta itemprop=\"price\" content=\"9.99\"><meta itemprop=\"priceCurrency\" content=\"EUR\"></div></div></section>";
    let r = structured(page);
    assert_eq!((r.value, r.currency.as_str()), (Some(499.0), "EUR"));
}

#[test]
fn json_ld_related_products_and_member_prices_are_not_the_items() {
    let related = "<script type=\"application/ld+json\">{\"@type\":\"Product\",\"name\":\"Camera\",\"offers\":{\"@type\":\"Offer\",\
\"price\":499,\"availability\":\"https://schema.org/OutOfStock\"},\"isRelatedTo\":[{\"@type\":\"Product\",\
\"name\":\"Strap\",\"offers\":{\"@type\":\"Offer\",\"price\":19,\"availability\":\"https://schema.org/InStock\"}}]}\
</script>";
    let r = structured(related);
    assert_eq!((r.value, r.available), (Some(499.0), Some(false)));
    let member = "<script type=\"application/ld+json\">{\"@type\":\"Product\",\"name\":\"Jacket\",\"offers\":{\"@type\":\"Offer\",\
\"price\":120,\"priceCurrency\":\"USD\",\"priceSpecification\":[{\"@type\":\"UnitPriceSpecification\",\
\"price\":120,\"priceCurrency\":\"USD\"},{\"@type\":\"UnitPriceSpecification\",\"price\":96,\"priceCurrency\":\"USD\",\
\"validForMemberTier\":{\"@id\":\"https://shop.example/member-plan#gold\"}}]}}</script>";
    assert_eq!(structured(member).value, Some(120.0));
}

#[test]
fn main_text_keeps_the_items_own_header_and_custom_elements() {
    let article = "<body><nav>Home Shop</nav><main><article><header><h1>Widget</h1><p class=price>$49.00</p>\
</header><p>Great widget.</p></article></main><footer>Gift cards from $10</footer></body>";
    let text = main_text(article);
    assert!(text.contains("$49.00") && !text.contains("Gift cards"), "{text}");
    let custom = "<body><nav-drawer>menu</nav-drawer><div id=product><h1>Widget</h1><p>$49.00 In stock</p></div>\
<footer><nav>About</nav></footer></body>";
    assert!(main_text(custom).contains("$49.00"));
}

#[test]
fn json_ld_with_an_unquoted_or_parameterised_type_is_read() {
    let body = r#"{"@type":"Product","name":"A","offers":{"price":"49.99","priceCurrency":"USD"}}"#;
    assert_eq!(structured(&format!("<script type=application/ld+json>{body}</script>")).value, Some(49.99));
    assert_eq!(structured(&format!("<script type=\"application/ld+json; charset=utf-8\">{body}</script>")).value, Some(49.99));
}

// ---- test_watch_tls.py -----------------------------------------------------------------------------

/// The helper's context: EVE_WATCH_TLS_PYTHON from this process, plus `extra`; None (skip) without curl_cffi.
fn tls_ctx(extra: &[(&str, &str)]) -> Option<Ctx> {
    let mut env: HashMap<String, String> = HashMap::new();
    if let Ok(py) = std::env::var("EVE_WATCH_TLS_PYTHON") {
        env.insert("EVE_WATCH_TLS_PYTHON".into(), py);
    }
    for (k, v) in extra {
        env.insert((*k).into(), (*v).into());
    }
    let ctx = Ctx::global().clone().with_env(env);
    if tls_available_with(&ctx) {
        Some(ctx)
    } else {
        eprintln!("skipped: curl_cffi is not installed for the Chrome-like reader's interpreter");
        None
    }
}

#[test]
fn tls_reads_a_page_and_its_charset() {
    let Some(ctx) = tls_ctx(&[]) else { return };
    let srv = serve(|_, s| send(s, 200, b"prix 12 \xa4", &[("Content-Type", "text/html; charset=latin-1")]));
    let (status, body) = tls_request_with(&ctx, &format!("http://127.0.0.1:{}/p", srv.port), 20.0, &loopback_is_public()).unwrap();
    assert!(status == 200 && body.starts_with("prix 12"), "{body}");
    assert_eq!(body, "prix 12 \u{a4}");
}

/// `pinned.example` does not exist in real DNS: the read succeeds only if libcurl connects to the address eve's
/// one lookup answered, which is the pin.
#[test]
fn tls_the_connection_goes_to_the_pinned_address_not_a_second_lookup() {
    let Some(ctx) = tls_ctx(&[]) else { return };
    let srv = serve(|_, s| send(s, 200, b"<html>pinned</html>", &[]));
    let rules = AddressRules {
        lookup: Arc::new(|host: &str, port: u16| {
            let host = if host == "pinned.example" { "127.0.0.1" } else { host };
            Ok((host, port).to_socket_addrs()?.collect())
        }),
        ..loopback_is_public()
    };
    let (_, body) = tls_request_with(&ctx, &format!("http://pinned.example:{}/", srv.port), 20.0, &rules).unwrap();
    assert_eq!(body, "<html>pinned</html>");
}

/// check_url let it through (its lookup saw a public address); the pin's own lookup sees loopback, and the real
/// address check refuses it before any connection.
#[test]
fn tls_a_name_answering_a_private_address_is_refused_at_the_connect() {
    let Some(ctx) = tls_ctx(&[]) else { return };
    let rules = AddressRules {
        lookup: Arc::new(|_: &str, port: u16| Ok(vec![SocketAddr::new("127.0.0.1".parse().unwrap(), port)])),
        ip_public: Arc::new(ip_public_here),
        resolve: Some(Arc::new(|_: &str| true)),
    };
    let err = tls_request_with(&ctx, "http://rebind.example/", 20.0, &rules).unwrap_err();
    assert!(err.reason.contains("private or unknown"), "{err:?}");
}

#[test]
fn tls_a_redirect_is_checked_like_the_first_link() {
    let Some(ctx) = tls_ctx(&[]) else { return };
    let srv = serve(|_, s| send(s, 302, b"", &[("Location", "http://169.254.169.254/latest/meta-data/")]));
    let rules = AddressRules { resolve: Some(Arc::new(|host: &str| host == "127.0.0.1")), ..loopback_is_public() };
    let err = tls_request_with(&ctx, &format!("http://127.0.0.1:{}/", srv.port), 20.0, &rules).unwrap_err();
    assert!(err.reason.contains("redirected somewhere not allowed"), "{err:?}");
}

#[test]
fn tls_redirects_are_followed_then_capped() {
    let Some(ctx) = tls_ctx(&[]) else { return };
    let hops = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = hops.clone();
    let srv = serve(move |req, s| {
        seen.lock().unwrap().push(req.path.clone());
        match req.path.as_str() {
            "/end" => send(s, 200, b"arrived", &[]),
            "/loop" => send(s, 301, b"", &[("Location", "/loop")]),
            _ => send(s, 307, b"", &[("Location", "/end")]),
        }
    });
    let rules = loopback_is_public();
    assert_eq!(tls_request_with(&ctx, &format!("http://127.0.0.1:{}/start", srv.port), 20.0, &rules).unwrap().1, "arrived");
    assert_eq!(*hops.lock().unwrap(), vec!["/start".to_string(), "/end".into()]);
    let err = tls_request_with(&ctx, &format!("http://127.0.0.1:{}/loop", srv.port), 20.0, &rules).unwrap_err();
    assert!(err.reason.contains("redirected too many times"), "{err:?}");
    assert_eq!(hops.lock().unwrap().iter().filter(|p| *p == "/loop").count(), TLS_MAX_HOPS + 1);
}

/// MAX_BYTES is a constant here (Python patches it to 1000), so the pages are MAX_BYTES and a little.
#[test]
fn tls_a_page_past_the_size_cap_is_refused() {
    let Some(ctx) = tls_ctx(&[]) else { return };
    let srv = serve(|req, s| {
        if req.path == "/sized" {
            send(s, 200, &vec![b'x'; MAX_BYTES + 5000], &[]);
            return;
        }
        // no Content-Length: only the read loop can stop it
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n");
        let chunk = vec![b'y'; 64 * 1024];
        for _ in 0..(MAX_BYTES / chunk.len() + 4) {
            if s.write_all(&chunk).is_err() {
                return;
            }
        }
    });
    for path in ["/sized", "/streamed"] {
        let err = tls_request_with(&ctx, &format!("http://127.0.0.1:{}{path}", srv.port), 20.0, &loopback_is_public()).unwrap_err();
        assert!(err.reason.contains("too big"), "{path}: {err:?}");
    }
}

/// A proxy would make the connection, and the pin with it, moot. Port 9 on loopback has nothing listening, so
/// a read that used the proxy would fail.
#[test]
fn tls_a_proxy_in_the_environment_is_ignored() {
    let proxy = "http://127.0.0.1:9";
    let names = ["http_proxy", "HTTP_PROXY", "https_proxy", "HTTPS_PROXY", "all_proxy", "ALL_PROXY"];
    let mut extra: Vec<(&str, &str)> = names.iter().map(|n| (*n, proxy)).collect();
    extra.extend([("no_proxy", ""), ("NO_PROXY", "")]);
    let Some(ctx) = tls_ctx(&extra) else { return };
    let srv = serve(|_, s| send(s, 200, b"direct", &[]));
    assert_eq!(tls_request_with(&ctx, &format!("http://127.0.0.1:{}/", srv.port), 20.0, &loopback_is_public()).unwrap().1, "direct");
}

#[test]
fn tls_a_refusal_carries_its_status_and_retry_after() {
    let Some(ctx) = tls_ctx(&[]) else { return };
    let srv = serve(|req, s| {
        if req.path == "/slow-down" {
            send(s, 429, b"no", &[("Retry-After", "7")]);
        } else {
            send(s, 403, b"<html>Access Denied</html>", &[]);
        }
    });
    let rules = loopback_is_public();
    let e = tls_request_with(&ctx, &format!("http://127.0.0.1:{}/", srv.port), 20.0, &rules).unwrap_err();
    assert_eq!((e.status, e.reason.as_str()), (403, "the site answered HTTP 403"));
    let e = tls_request_with(&ctx, &format!("http://127.0.0.1:{}/slow-down", srv.port), 20.0, &rules).unwrap_err();
    assert_eq!((e.status, e.retry_after), (429, Some(7.0)));
}

#[test]
fn tls_a_trickling_server_is_cut_at_the_deadline() {
    let Some(ctx) = tls_ctx(&[]) else { return };
    let srv = serve(|_, s| {
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\n\r\n");
        for _ in 0..100 {
            if s.write_all(b"z").and_then(|_| s.flush()).is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
    });
    let t = Instant::now();
    let err = tls_request_with(&ctx, &format!("http://127.0.0.1:{}/", srv.port), 1.0, &loopback_is_public()).unwrap_err();
    assert!(err.reason.contains("took too long"), "{err:?}");
    assert!(t.elapsed().as_secs_f64() < 4.0, "{:?}", t.elapsed());
}

#[test]
fn tls_only_other_schemes_and_credentials_are_refused_before_any_lookup() {
    let ctx = Ctx::global();
    for url in ["file:///etc/passwd", "ftp://example.com/", "https://user:pw@example.com/"] {
        assert!(tls_request_with(ctx, url, 20.0, &AddressRules::system()).is_err(), "{url}");
    }
}

#[test]
fn which_failures_are_worth_the_chrome_like_read() {
    assert!(tls_worth(&FetchError::with_status("the site answered HTTP 403", 403)));
    assert!(tls_worth(&FetchError::with_status("the site answered HTTP 401", 401)));
    assert!(tls_worth(&FetchError::new(TOO_LONG)));
    assert!(!tls_worth(&FetchError::with_retry("the site answered HTTP 429", 429, Some(5.0)))); // honoured, not dodged
    assert!(!tls_worth(&FetchError::with_status("the site answered HTTP 404", 404)));
    assert!(!tls_worth(&FetchError::new("the site could not be reached")));
    assert!(!tls_worth(&FetchError::with_status("the site answered HTTP 403", 403).at("openrouter.ai")));
}

// ---- test_watch_tinyfish.py: fetch_hosted ------------------------------------------------------------

const URL: &str = "https://shop.example/p";

/// fetch_hosted()'s requests through a fake `Ctx::http`, answered by `reply` (a payload, or a FetchError).
#[derive(Clone)]
struct Sent {
    calls: Arc<Mutex<Vec<(String, Value, HttpOptions)>>>,
    reply: Arc<Mutex<Result<Value, FetchError>>>,
}

fn sent() -> (Sent, Ctx) {
    let s = Sent {
        calls: Arc::new(Mutex::new(Vec::new())),
        reply: Arc::new(Mutex::new(Ok(json!({
            "results": [{"url": URL, "final_url": URL, "text": "<main>$85</main>", "format": "html"}], "errors": []
        })))),
    };
    let fake = s.clone();
    let http = move |url: &str, o: &HttpOptions| -> Result<(u16, String), FetchError> {
        let body: Value = serde_json::from_slice(o.data.as_deref().unwrap_or(b"null")).unwrap_or(Value::Null);
        fake.calls.lock().unwrap().push((url.to_string(), body, o.clone()));
        fake.reply.lock().unwrap().clone().map(|v| (200, v.to_string()))
    };
    let env: HashMap<String, String> = [("TINYFISH_API_KEY".to_string(), "tf-test".to_string())].into();
    let ctx = Ctx::global().clone().with_env(env).with_http(Arc::new(http));
    (s, ctx)
}

fn failed(error: &str, status: u16) -> Value {
    let mut e = json!({"url": URL, "error": error});
    if status != 0 {
        e["status"] = json!(status);
    }
    json!({"results": [], "errors": [e]})
}

fn yes(_: &str) -> bool {
    true
}

#[test]
#[ignore = "cross-unit: tinyfish::fetch belongs to another unit (still a stub in the skeleton); un-ignore at integration"]
fn hosted_it_asks_for_a_fresh_page() {
    let (sent, ctx) = sent();
    let (status, html) = fetch_hosted(&ctx, URL, 70.0, Some(&yes)).unwrap();
    assert_eq!((status, html.as_str()), (200, "<main>$85</main>"));
    let calls = sent.calls.lock().unwrap();
    let (url, body, o) = &calls[0];
    assert_eq!(url, eve_web::tinyfish::FETCH_URL);
    assert_eq!(o.header("X-API-Key"), Some("tf-test"));
    // ttl 0: a live read, never TinyFish's cache; a watch is about now
    assert_eq!((&body["urls"], &body["ttl"], &body["format"]), (&json!([URL]), &json!(0), &json!("html")));
    assert!(body["per_url_timeout_ms"].as_f64().unwrap() < o.timeout * 1000.0); // TinyFish gives up first
}

#[test]
#[ignore = "cross-unit: tinyfish::fetch belongs to another unit (still a stub in the skeleton); un-ignore at integration"]
fn hosted_the_pages_own_refusal_moves_the_ladder_tinyfishs_never_does() {
    let (sent, ctx) = sent();
    let host = eve_web::tinyfish::FETCH_HOST;
    for (reply, status) in [
        (failed("target_http_error", 403), 403),
        (failed("bot_blocked", 0), 403),
        (failed("login_required", 0), 401),
        (failed("page_not_found", 404), 404),
    ] {
        *sent.reply.lock().unwrap() = Ok(reply);
        let e = fetch_hosted(&ctx, URL, 70.0, Some(&yes)).unwrap_err();
        assert!(e.status == status && e.host.is_empty(), "{e:?}"); // the site's: check() moves the watch on
    }
    for code in ["timeout", "proxy_error", "empty_content"] {
        *sent.reply.lock().unwrap() = Ok(failed(code, 0));
        let e = fetch_hosted(&ctx, URL, 70.0, Some(&yes)).unwrap_err();
        assert!(e.reason.contains(code) && e.host == host && e.status == 0, "{e:?}");
    }
    let mut last = None;
    for status in [401u16, 402, 429] {
        let retry = if status == 429 { Some(30.0) } else { None };
        *sent.reply.lock().unwrap() = Err(FetchError::with_retry(format!("the site answered HTTP {status}"), status, retry));
        let e = fetch_hosted(&ctx, URL, 70.0, Some(&yes)).unwrap_err();
        assert!(e.host == host && e.status == status, "{e:?}");
        last = Some(e);
    }
    assert_eq!(last.unwrap().retry_after, Some(30.0)); // the limiter holds TinyFish, not the shop
    *sent.reply.lock().unwrap() = Ok(json!({"results": [{"url": URL, "text": "  "}], "errors": []}));
    let e = fetch_hosted(&ctx, URL, 70.0, Some(&yes)).unwrap_err();
    assert!(e.reason.contains("empty page") && e.host == host, "{e:?}");
}

#[test]
#[ignore = "cross-unit: tinyfish::fetch belongs to another unit (still a stub in the skeleton); un-ignore at integration"]
fn hosted_a_redirected_or_renamed_single_result_still_counts() {
    let (sent, ctx) = sent();
    *sent.reply.lock().unwrap() = Ok(json!({"results": [{"url": format!("{URL}/"), "text": "<p>ok</p>"}], "errors": []}));
    assert_eq!(fetch_hosted(&ctx, URL, 70.0, Some(&yes)).unwrap(), (200, "<p>ok</p>".to_string()));
}

#[test]
#[ignore = "cross-unit: tinyfish::fetch belongs to another unit (still a stub in the skeleton); un-ignore at integration"]
fn hosted_no_key_no_request() {
    let (sent, ctx) = sent();
    let ctx = ctx.with_env(HashMap::new());
    let e = fetch_hosted(&ctx, URL, 70.0, Some(&yes)).unwrap_err();
    assert!(e.reason.contains("TINYFISH_API_KEY") && e.host == eve_web::tinyfish::FETCH_HOST, "{e:?}");
    assert!(sent.calls.lock().unwrap().is_empty(), "no key, no request");
}

#[test]
fn hosted_a_private_link_is_never_sent_to_tinyfish() {
    let (sent, ctx) = sent();
    let e = fetch_hosted(&ctx, "http://192.168.1.1/admin", 70.0, None).unwrap_err();
    assert!(e.reason.contains("private or unknown"), "{e:?}");
    assert!(sent.calls.lock().unwrap().is_empty(), "a private link is not sent");
}

// ---- differential: the page readers against Python's ---------------------------------------------------

/// Pages and Python's readings of them (structured, main_text, visible_text, _microdata), as a JSON list at
/// EVE_WATCH_PAGES_DIFF (made by running watch.py over generated pages); every reading must match.
#[test]
#[ignore = "differential: needs EVE_WATCH_PAGES_DIFF, a case file made with Python's watch.py"]
fn the_page_readers_match_python() {
    use eve_web::watch::page::{microdata, visible_text};
    let path = std::env::var("EVE_WATCH_PAGES_DIFF").expect("EVE_WATCH_PAGES_DIFF");
    let cases: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let mut bad = Vec::new();
    for (i, c) in cases.iter().enumerate() {
        let page = c["page"].as_str().unwrap();
        let r = structured(page);
        let got = json!({"value": r.value, "currency": r.currency, "available": r.available, "name": r.name,
                         "source": r.source, "note": r.note, "main": main_text(page), "visible": visible_text(page),
                         "micro": microdata(page).into_iter().map(|(k, v)| vec![k, v]).collect::<Vec<_>>()});
        for key in ["value", "currency", "available", "name", "source", "note", "main", "visible", "micro"] {
            if c[key].is_null() && key == "micro" {
                continue;
            }
            let same = match (c[key].as_f64(), got[key].as_f64()) {
                (Some(a), Some(b)) => a == b,
                _ => c[key] == got[key],
            };
            if !same {
                bad.push(format!("case {i} {key}: python {} rust {}", c[key], got[key]));
            }
        }
    }
    assert!(bad.is_empty(), "{} of {} cases differ:\n{}", bad.len(), cases.len(), bad.iter().take(30).cloned().collect::<Vec<_>>().join("\n"));
}
