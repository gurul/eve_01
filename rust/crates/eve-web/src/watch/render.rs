//! watch.py's render section: the page as a browser sees it, in a child process with a hard deadline, behind a
//! guard proxy, with safe overlays pressed away.
//!
//! Python runs Playwright's Chromium in `python -m eve_bridge.watch --render URL TIMEOUT`; here the child is the
//! `eve-watch` bin (`eve-watch --render URL TIMEOUT`, [`render_main`]) and the browser is headless Chromium over
//! the Chrome DevTools Protocol through eve_browser_lane's driver (`CdpDriver::start`,
//! `launch_persistent_context` with a throwaway profile), started with `--proxy-server=http://127.0.0.1:<port>`,
//! `--proxy-bypass-list=<-loopback>`, `--disable-quic`, `--force-webrtc-ip-handling-policy=disable_non_proxied_udp`,
//! the watcher's user agent and `--lang=en-US`.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, LazyLock};
use std::time::{Duration, Instant};

use base64::Engine;
use eve_browser_lane::driver::PageHandle;
use regex::Regex;
use serde_json::{json, Value};

use super::net::{check_url, connect_public, urlsplit, AddressRules, ConnectError};
use super::page::{py_float_repr, py_str, truthy};
use super::{FetchError, USER_AGENT};

/// An overlay between the browser and the item (LEGO's "You are about to enter LEGO.com  [Continue]", 2026-09-25).
/// The render is a throwaway, signed-out context, so a button that only closes or continues is safe to press;
/// one that agrees, signs up, subscribes or pays never is. Cookie notices go through browser_lane's own rule
/// (a refusing or closing button only).
pub const OVERLAY_OK: &str = r"(?i)^\s*(continue|continue (to|shopping|to site)|close|dismiss|×|✕|x|no,? thanks|not now|maybe later|skip|stay here|stay on .{1,30}|enter( site)?)\s*$";
pub const OVERLAY_NEVER: &str = r"(?i)accept|agree|allow|sign ?(up|in)|subscribe|join|buy|pay|order|log ?in|register|start playing|download|install";
/// `_OVERLAY_JS`, byte for byte: the visible buttons of a modal covering the page, each tagged data-watch-overlay.
pub const OVERLAY_JS: &str = r#"() => {
  const vis = (e) => { const r = e.getBoundingClientRect(); if (r.width < 2 || r.height < 2) return false;
    const cs = getComputedStyle(e); return cs.visibility !== 'hidden' && cs.display !== 'none' && cs.opacity !== '0'; };
  const W = innerWidth, H = innerHeight;
  const boxes = Array.from(document.querySelectorAll('dialog[open],[role=dialog],[role=alertdialog],[aria-modal=true],' +
    '[class*=modal i],[class*=overlay i],[class*=popup i],[id*=modal i],[id*=overlay i]')).filter(vis).filter(b => {
      const r = b.getBoundingClientRect(); return r.width * r.height > 0.15 * W * H; });
  for (const e of document.querySelectorAll('[data-watch-overlay]')) e.removeAttribute('data-watch-overlay');
  const out = [];
  const nav = (e) => e.tagName === 'A' && /^(https?:|\/[^#]|[^#:]+\.)/i.test(e.getAttribute('href') || '');
  boxes.forEach((b, bi) => Array.from(b.querySelectorAll('button,[role=button],a')).filter(vis).filter(e => !nav(e)).forEach((e, i) => {
    const id = bi + '-' + i; e.setAttribute('data-watch-overlay', id);
    out.push({id, name: (e.getAttribute('aria-label') || e.innerText || e.title || '').trim().replace(/\s+/g, ' ').slice(0, 60)});
  }));
  return out; }"#;

pub const OVERLAY_SETTLE_MS: u64 = 1200;
/// at most: presses and quiet looks together
pub const OVERLAY_LOOKS: usize = 7;

pub const RENDER_TIMEOUT_SECS: f64 = 30.0;
pub const RENDER_SETTLE_SECS: f64 = 6.0;
pub const RENDER_VIEWPORT: (u32, u32) = (1280, 1600);
/// past the page's own timeout: overlays, the screenshot, Chromium's start and exit
pub const RENDER_DEADLINE_PAD_SECS: f64 = 30.0;
pub const PROXY_IDLE_SECS: f64 = 30.0;
/// `_ProxyHandler.DENIED`.
pub const DENIED: &[u8] = b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

static OVERLAY_OK_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(OVERLAY_OK).expect("a valid pattern"));
static OVERLAY_NEVER_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(OVERLAY_NEVER).expect("a valid pattern"));

/// `page.content()`: the doctype and the document, serialized as Playwright serializes them.
const CONTENT_JS: &str = "() => { let r = ''; if (document.doctype) r = new XMLSerializer().serializeToString(document.doctype); \
if (document.documentElement) r += document.documentElement.outerHTML; return r; }";
/// The main navigation's HTTP status (Playwright's `response.status`); 0 when there was none.
const STATUS_JS: &str = "() => { const n = performance.getEntriesByType('navigation')[0]; \
return n && n.responseStatus ? n.responseStatus : 0; }";
/// Marks the page being read, so a sweep can tell it from popups.
const MARK_JS: &str = "() => { window.__eveWatchRead = true; return true; }";
const MARKED_JS: &str = "() => window.__eveWatchRead === true";

fn secs(x: f64) -> Duration {
    Duration::from_secs_f64(if x.is_finite() && x > 0.0 { x.min(1e9) } else { 0.0 })
}

/// The button that only gets an overlay out of the way, or None.
pub fn overlay_choice(buttons: &[Value]) -> Option<Value> {
    for b in buttons {
        let name = match b.get("name") {
            Some(v) if truthy(v) => py_str(v),
            _ => String::new(),
        };
        if !name.is_empty() && OVERLAY_OK_RE.is_match(&name) && !OVERLAY_NEVER_RE.is_match(&name) {
            return Some(b.clone());
        }
    }
    None
}

/// `_page_of(url)`: a page as scheme, host and path: a query or a fragment added by a reload is the same page.
pub fn page_of(url: &str) -> (String, String, String) {
    let Ok(parts) = urlsplit(url) else { return (String::new(), String::new(), String::new()) };
    let path = parts.path.trim_end_matches('/');
    let path = if path.is_empty() { "/".to_string() } else { path.to_string() };
    (parts.scheme.clone(), parts.hostname().unwrap_or_default().to_lowercase(), path)
}

/// `str(x)` of a button's id or name, as an f-string writes it.
fn label(v: Option<&Value>) -> String {
    v.map(py_str).unwrap_or_else(|| "None".into())
}

/// `_clear_overlays(page)`: overlays pressed away until two looks in a row, a moment apart, find nothing. Escape
/// first, then a cookie notice's refusing button (browser_lane's CONSENT_JS / consent_choice), then a continue/
/// close button on a modal covering the page, never a link that navigates. A click that went to another page
/// loads the item's page again, and pressing stops. What was pressed, for the log. Never fails.
pub fn clear_overlays(page: &dyn PageHandle) -> Vec<String> {
    // A modal can open just after the network settles: LEGO's did in one render in three.
    let mut pressed: Vec<String> = Vec::new();
    let mut quiet = 0;
    for _ in 0..OVERLAY_LOOKS {
        let step = (|| -> anyhow::Result<bool> {
            page.keyboard_press("Escape")?;
            let here = page.url();
            let found = page.evaluate(eve_browser_lane::CONSENT_JS, Some(&json!(eve_browser_lane::CONSENT_TEXT_PATTERN)))?;
            let choice = match &found {
                Value::Object(map) => {
                    let buttons = match map.get("buttons") {
                        Some(Value::Array(b)) => b.clone(),
                        Some(v) if truthy(v) => anyhow::bail!("the consent buttons are not a list"),
                        _ => Vec::new(),
                    };
                    eve_browser_lane::consent_choice(&buttons)
                }
                _ => None,
            };
            if let Some(choice) = choice {
                page.click(&format!("[data-eve-consent=\"{}\"]", label(choice.get("id"))), 2000)?;
                pressed.push(label(choice.get("name")));
            } else {
                let buttons = match page.evaluate(OVERLAY_JS, None)? {
                    Value::Array(b) => b,
                    v if !truthy(&v) => Vec::new(),
                    _ => anyhow::bail!("the overlay buttons are not a list"),
                };
                let Some(pick) = overlay_choice(&buttons) else {
                    quiet += 1;
                    if quiet >= 2 {
                        return Ok(true);
                    }
                    page.wait_for_timeout(OVERLAY_SETTLE_MS);
                    return Ok(false);
                };
                page.click(&format!("[data-watch-overlay=\"{}\"]", label(pick.get("id"))), 2000)?;
                pressed.push(label(pick.get("name")));
            }
            quiet = 0;
            page.wait_for_timeout(600);
            if page_of(&page.url()) != page_of(&here) {
                // the button went to another page, whose price is not the item's: the item's page again, and no
                // more pressing. A reload of the same page (a query or a hash added, LEGO's "Continue") is not
                // another page; Back from there landed on about:blank (2026-09-25).
                tracing::info!("watch: an overlay button went to another page; loading the item's page again");
                page.goto(&here, "domcontentloaded", 15_000)?;
                return Ok(true);
            }
            Ok(false)
        })();
        match step {
            Ok(false) => continue,
            // done, or a page mid-navigation, a detached button: read the page as it is
            Ok(true) | Err(_) => break,
        }
    }
    pressed
}

/// The page as a browser sees it: the rendered HTML and a JPEG of the first screen. The browser runs in a child
/// process (`eve-watch --render URL TIMEOUT`, or `argv` for tests) in its own process group with a hard deadline
/// (`deadline` seconds, default `timeout + RENDER_DEADLINE_PAD_SECS`); past it the child, everything under it and
/// its group are killed ("the page took too long to render"). Blocking, so run it off the loop.
pub fn render(url: &str, timeout: f64, deadline: Option<f64>, argv: Option<Vec<String>>) -> Result<(String, Vec<u8>), FetchError> {
    // A page whose script spins after load held Playwright's calls (evaluate, keyboard) forever, and with them the
    // watcher's one lock (security review, 2026-09-25).
    let why = check_url(url, None);
    if !why.is_empty() {
        return Err(FetchError::new(why));
    }
    let cmd = argv.filter(|a| !a.is_empty()).unwrap_or_else(|| render_command(url, timeout));
    let mut command = Command::new(&cmd[0]);
    command.args(&cmd[1..]).stdout(Stdio::piped()).stderr(Stdio::null());
    // start_new_session=True: the child leads a session and process group of its own
    // SAFETY: setsid is async-signal-safe and touches no memory of this process.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|_| FetchError::new("the browser could not load the page"))?;
    let pid = child.id();
    let mut stdout = child.stdout.take();
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        if let Some(s) = stdout.as_mut() {
            let _ = s.read_to_end(&mut out);
        }
        let _ = tx.send(out);
    });
    let limit = Instant::now() + secs(deadline.unwrap_or(timeout + RENDER_DEADLINE_PAD_SECS));
    let out = match rx.recv_timeout(limit.saturating_duration_since(Instant::now())) {
        Ok(out) => {
            let mut exited = false;
            while Instant::now() < limit {
                if matches!(child.try_wait(), Ok(Some(_)) | Err(_)) {
                    exited = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            if exited {
                Some(out)
            } else {
                None
            }
        }
        Err(_) => None,
    };
    let Some(out) = out else {
        kill_tree(pid); // the child, its driver, and the Chromium they started
        // SAFETY: a signal to the child's own process group (setsid made its id the group's).
        unsafe {
            libc::killpg(pid as libc::pid_t, libc::SIGKILL);
        }
        let _ = child.wait();
        return Err(FetchError::new("the page took too long to render"));
    };
    let text = String::from_utf8_lossy(&out);
    let failed = || FetchError::new("the browser could not load the page");
    let answer: Value = serde_json::from_str(text.trim_matches([' ', '\t', '\n', '\r'])).map_err(|_| failed())?;
    let Value::Object(answer) = answer else { return Err(failed()) };
    if !answer.get("ok").is_some_and(truthy) {
        let reason = match answer.get("reason") {
            Some(v) if truthy(v) => py_str(v),
            _ => String::new(),
        };
        let status = match answer.get("status") {
            Some(Value::Number(n)) => n.as_i64().and_then(|s| u16::try_from(s).ok()).unwrap_or(0),
            Some(Value::Bool(b)) => u16::from(*b),
            _ => 0,
        };
        return Err(FetchError::with_status(if reason.is_empty() { "the browser could not load the page".into() } else { reason }, status));
    }
    let (Some(html), Some(shot)) = (answer.get("html"), answer.get("shot")) else { return Err(failed()) };
    let shot = base64::engine::general_purpose::STANDARD.decode(py_str(shot)).map_err(|_| failed())?;
    Ok((py_str(html), shot))
}

/// The child's command line: the `eve-watch` bin beside the running executable (else on PATH), `--render`, the
/// URL and the timeout.
pub fn render_command(url: &str, timeout: f64) -> Vec<String> {
    let mut exe = PathBuf::from("eve-watch");
    if let Ok(me) = std::env::current_exe() {
        // beside this binary, or one folder up from a test binary in target/<profile>/deps
        for dir in me.ancestors().skip(1).take(2) {
            let candidate = dir.join("eve-watch");
            if candidate.is_file() {
                exe = candidate;
                break;
            }
        }
    }
    vec![exe.to_string_lossy().into_owned(), "--render".into(), url.to_string(), py_float_repr(timeout)]
}

/// `_GuardProxy`: the browser's only way out, a local HTTP proxy on loopback that opens every connection through
/// `_connect_public`, so the address connected to is checked, whatever asked for it (a redirect's next hop, a
/// WebSocket, a popup's requests). Serves on a thread per connection until dropped or `shutdown`.
pub struct GuardProxy {
    pub port: u16,
    pub(crate) stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(crate) thread: Option<std::thread::JoinHandle<()>>,
}

impl GuardProxy {
    /// Listen on 127.0.0.1, an ephemeral port, and serve.
    pub fn start(rules: AddressRules) -> std::io::Result<GuardProxy> {
        // Playwright's page.route never saw a redirect's next hop, a WebSocket, or a popup's requests; a proxy sees
        // every connection (verification/Eve/WatchSsrf.lean, the fixed step).
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new().name("eve-watch-guard".into()).spawn(move || {
            for conn in listener.incoming() {
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(conn) = conn else { continue };
                let rules = rules.clone();
                let _ = std::thread::Builder::new().name("eve-watch-guard-conn".into()).spawn(move || proxy_handle(conn, &rules));
            }
        })?;
        Ok(GuardProxy { port, stop, thread: Some(thread) })
    }

    /// `proxy.shutdown(); proxy.server_close()`.
    pub fn shutdown(&mut self) {
        let Some(thread) = self.thread.take() else { return };
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&([127, 0, 0, 1], self.port).into(), Duration::from_secs(2)); // wake accept
        let _ = thread.join();
    }
}

impl Drop for GuardProxy {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn starts_with_ci(line: &[u8], prefix: &[u8]) -> bool {
    line.len() >= prefix.len() && line[..prefix.len()].eq_ignore_ascii_case(prefix)
}

fn deny(client: &TcpStream) {
    let mut c = client;
    let _ = c.write_all(DENIED);
}

/// `_ProxyHandler.handle`: one client connection. CONNECT tunnels to host:port; a plain http request with an
/// absolute URI is sent once (proxy headers dropped, Connection: close, no chunked or huge upload) and its answer
/// relayed; anything refused gets DENIED.
pub fn proxy_handle(client: TcpStream, rules: &AddressRules) {
    let idle = secs(PROXY_IDLE_SECS);
    let _ = client.set_read_timeout(Some(idle));
    let _ = client.set_write_timeout(Some(idle));
    let mut head: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 65536];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match (&client).read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(_) if head.len() > 65536 => return,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
    }
    let split = head.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(head.len());
    let top = &head[..split];
    let rest = head.get(split + 4..).unwrap_or(&[]).to_vec();
    let lines = split_crlf(top);
    let first: Vec<&[u8]> = lines[0].splitn(3, |&c| c == b' ').collect();
    if first.len() != 3 {
        return;
    }
    let (method, target, version) = (first[0], first[1], first[2]);
    let target: String = target.iter().map(|&b| b as char).collect(); // latin-1
    let upstream = if method.eq_ignore_ascii_case(b"CONNECT") {
        // https, wss: a tunnel to host:port
        let (host, port) = target.rsplit_once(':').unwrap_or(("", target.as_str()));
        let Ok(port) = port.trim().parse::<u16>() else { return deny(&client) };
        let host = host.trim_matches(|c| c == '[' || c == ']');
        let upstream = match connect_public(host, port, 20.0, rules) {
            Ok(u) => u,
            Err(ConnectError::Refused(_)) | Err(ConnectError::Io(_)) => return deny(&client),
        };
        if (&client).write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").is_err() {
            return deny(&client);
        }
        if !rest.is_empty() && (&upstream).write_all(&rest).is_err() {
            return deny(&client);
        }
        upstream
    } else {
        // plain http: an absolute URI, one request
        let Ok(parts) = urlsplit(&target) else { return deny(&client) };
        let Some(hostname) = parts.hostname().filter(|_| parts.scheme == "http") else { return deny(&client) };
        let Ok(port) = parts.port() else { return deny(&client) };
        let port = port.filter(|p| *p != 0).unwrap_or(80);
        let upstream = match connect_public(&hostname, port, 20.0, rules) {
            Ok(u) => u,
            Err(_) => return deny(&client),
        };
        let path = format!("{}{}", if parts.path.is_empty() { "/" } else { &parts.path }, if parts.query.is_empty() { String::new() } else { format!("?{}", parts.query) });
        let kept: Vec<&[u8]> = lines[1..]
            .iter()
            .copied()
            .filter(|h| !(starts_with_ci(h, b"proxy-") || starts_with_ci(h, b"connection:") || starts_with_ci(h, b"keep-alive:")))
            .collect();
        let length = kept.iter().find(|h| starts_with_ci(h, b"content-length:")).map(|h| {
            let value = String::from_utf8_lossy(&h[b"content-length:".len()..]).trim().to_string();
            if value.is_empty() { Some(0i64) } else { value.parse::<i64>().ok() }
        });
        let length = match length {
            None => 0,
            Some(Some(n)) => n,
            Some(None) => return deny(&client),
        };
        if kept.iter().any(|h| starts_with_ci(h, b"transfer-encoding:")) || !(0..=10_000_000).contains(&length) {
            return deny(&client); // a chunked or huge upload: not needed to read a page
        }
        let length = length as usize;
        if path.chars().any(|c| (c as u32) > 255) {
            return deny(&client); // path.encode("latin-1") raised
        }
        let mut request: Vec<u8> = Vec::new();
        request.extend_from_slice(method);
        request.push(b' ');
        request.extend(path.chars().map(|c| c as u32 as u8));
        request.push(b' ');
        request.extend_from_slice(version);
        request.extend_from_slice(b"\r\n");
        for h in &kept {
            request.extend_from_slice(h);
            request.extend_from_slice(b"\r\n");
        }
        request.extend_from_slice(b"Connection: close\r\n\r\n");
        if (&upstream).write_all(&request).is_err() {
            return deny(&client);
        }
        let mut body: Vec<u8> = rest[..rest.len().min(length)].to_vec();
        while body.len() < length {
            let want = (length - body.len()).min(65536);
            match (&client).read(&mut buf[..want]) {
                Ok(0) => break,
                Ok(n) => body.extend_from_slice(&buf[..n]),
                Err(_) => return deny(&client),
            }
        }
        if (&upstream).write_all(&body).is_err() {
            return deny(&client);
        }
        // One request per connection: the answer goes back, and nothing more the client sends is carried to this
        // host (a second request on a kept-alive connection went to the first server; re-verification)
        relay_one_way(upstream, client);
        return;
    };
    pipe(client, upstream);
}

/// `top.split(b"\r\n")`: the head's lines, split on CRLF only (a bare LF stays inside its line).
fn split_crlf(top: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i + 1 < top.len() {
        if top[i] == b'\r' && top[i + 1] == b'\n' {
            out.push(&top[start..i]);
            i += 2;
            start = i;
        } else {
            i += 1;
        }
    }
    out.push(&top[start..]);
    out
}

fn close(s: &TcpStream) {
    let _ = s.shutdown(Shutdown::Both);
}

/// `_pipe(a, b)`: bytes both ways until either side closes or goes quiet for PROXY_IDLE_SECS.
pub fn pipe(a: TcpStream, b: TcpStream) {
    let mut fds = [
        libc::pollfd { fd: a.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: b.as_raw_fd(), events: libc::POLLIN, revents: 0 },
    ];
    let mut buf = vec![0u8; 65536];
    let wait_ms = (PROXY_IDLE_SECS * 1000.0) as libc::c_int;
    'relay: loop {
        for f in fds.iter_mut() {
            f.revents = 0;
        }
        // SAFETY: two pollfd entries owned by this frame, for sockets that stay open until the end of it.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, wait_ms) };
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        if ready == 0 {
            break; // quiet both ways
        }
        for (i, f) in fds.iter().enumerate() {
            if f.revents == 0 {
                continue;
            }
            let (src, dst) = if i == 0 { (&a, &b) } else { (&b, &a) };
            match (&*src).read(&mut buf) {
                Ok(0) | Err(_) => break 'relay,
                Ok(n) => {
                    if (&*dst).write_all(&buf[..n]).is_err() {
                        break 'relay;
                    }
                }
            }
        }
    }
    close(&a);
    close(&b);
}

/// `_relay_one_way(src, dst)`: src's bytes to dst until src closes or goes quiet, then both closed.
pub fn relay_one_way(src: TcpStream, dst: TcpStream) {
    let _ = src.set_read_timeout(Some(secs(PROXY_IDLE_SECS)));
    let mut buf = vec![0u8; 65536];
    loop {
        match (&src).read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if (&dst).write_all(&buf[..n]).is_err() {
                    break;
                }
            }
        }
    }
    close(&src);
    close(&dst);
}

/// `_descendants(pid)`: every process under `pid` (`ps -A -o pid=,ppid=`).
pub fn descendants(pid: u32) -> Vec<u32> {
    // Playwright's driver, and Chromium, which runs in a process group of its own, so killing the child's group
    // misses it (seen with ps, 2026-09-25).
    let Some(out) = super::net::run_stdout("ps", &["-A", "-o", "pid=,ppid="], 5.0) else { return Vec::new() };
    let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
    for line in out.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() == 2 {
            if let (Ok(p), Ok(pp)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
                if parts.iter().all(|s| s.bytes().all(|c| c.is_ascii_digit())) {
                    kids.entry(pp).or_default().push(p);
                }
            }
        }
    }
    let mut found: Vec<u32> = Vec::new();
    let mut todo = vec![pid];
    while let Some(next) = todo.pop() {
        for child in kids.get(&next).cloned().unwrap_or_default() {
            if !found.contains(&child) {
                found.push(child);
                todo.push(child);
            }
        }
    }
    found
}

fn sigkill(pid: u32) {
    let Ok(pid) = libc::pid_t::try_from(pid) else { return };
    // SAFETY: kill(2) with a pid read from ps; a process that is gone or not ours is an ignored error.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
}

/// `_kill_tree(pid)`: SIGKILL to every descendant, then `pid`.
pub fn kill_tree(pid: u32) {
    for p in descendants(pid) {
        sigkill(p);
    }
    sigkill(pid);
}

/// `_kill_tree_below(pid)`: SIGKILL to every descendant of `pid`.
pub fn kill_tree_below(pid: u32) {
    for p in descendants(pid) {
        sigkill(p);
    }
}

/// Closes every page of `context` but the one being read (marked first): a popup is closed unread.
fn close_popups(context: &dyn eve_browser_lane::driver::ContextHandle, page: &dyn PageHandle) {
    if page.evaluate(MARK_JS, None).is_err() {
        return;
    }
    let Ok(pages) = context.pages() else { return };
    for other in pages {
        if !matches!(other.evaluate(MARKED_JS, None), Ok(Value::Bool(true))) {
            let _ = other.close();
        }
    }
}

/// The browser failed: Playwright's message, " in time" when it was a timeout.
fn browser_failed(e: &anyhow::Error) -> FetchError {
    let text = format!("{e:#}").to_lowercase();
    FetchError::new(format!("the browser could not load the page{}", if text.contains("timeout") { " in time" } else { "" }))
}

/// `_render_here(url, timeout)`: the render itself, in this process: headless Chromium behind the guard proxy
/// (loopback included), popups closed unread. Waits for the network to settle (RENDER_SETTLE_SECS) and presses
/// safe overlays away. FetchError otherwise.
pub fn render_here(url: &str, timeout: f64, rules: &AddressRules) -> Result<(String, Vec<u8>), FetchError> {
    if eve_browser_lane::cdp::find_chromium(true).is_none() {
        return Err(FetchError::new("no browser for pages that need one (Playwright is not installed)"));
    }
    let mut proxy = GuardProxy::start(rules.clone()).map_err(|_| FetchError::new("the browser failed (OSError)"))?;
    let profile = tempfile::tempdir().map_err(|_| FetchError::new("the browser failed (OSError)"))?;
    let mut driver = eve_browser_lane::cdp::CdpDriver::new();
    let result = (|| -> Result<(String, Vec<u8>), FetchError> {
        use eve_browser_lane::driver::Driver;
        // <-loopback> takes loopback off Chromium's built-in bypass list: the guard sees it too
        let args: Vec<String> = vec![
            format!("--proxy-server=http://127.0.0.1:{}", proxy.port),
            "--proxy-bypass-list=<-loopback>".into(),
            "--disable-quic".into(),
            "--force-webrtc-ip-handling-policy=disable_non_proxied_udp".into(),
            format!("--user-agent={USER_AGENT}"),
            "--lang=en-US".into(),
        ];
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let context = driver
            .launch_persistent_context(profile.path(), true, RENDER_VIEWPORT, &args)
            .map_err(|e| browser_failed(&e))?;
        let page = match context.pages().map_err(|e| browser_failed(&e))?.into_iter().next() {
            Some(p) => p,
            None => context.new_page().map_err(|e| browser_failed(&e))?,
        };
        page.goto(url, "domcontentloaded", (timeout * 1000.0).max(0.0) as u64).map_err(|e| browser_failed(&e))?;
        close_popups(&*context, &*page); // any page after this one is a popup: unread
        if let Ok(Value::Number(n)) = page.evaluate(STATUS_JS, None) {
            let status = n.as_u64().unwrap_or(0);
            if status >= 400 {
                let status = u16::try_from(status).unwrap_or(u16::MAX);
                return Err(FetchError::with_status(format!("the site answered HTTP {status}"), status));
            }
        }
        // a page that never goes quiet is read as it is
        let _ = page.wait_for_load_state("networkidle", (RENDER_SETTLE_SECS * 1000.0) as u64);
        let pressed = clear_overlays(&*page);
        if !pressed.is_empty() {
            tracing::info!("watch: pressed {} overlay button(s) away before reading the page", pressed.len());
        }
        close_popups(&*context, &*page);
        let html = match page.evaluate(CONTENT_JS, None).map_err(|e| browser_failed(&e))? {
            Value::String(s) => s,
            other => py_str(&other),
        };
        let shot = page.screenshot_jpeg(70, None).map_err(|e| browser_failed(&e))?;
        Ok((html, shot))
    })();
    {
        use eve_browser_lane::driver::Driver;
        let _ = driver.stop();
    }
    proxy.shutdown();
    result
}

/// `_render_watchdog(parent, deadline)`: the render child's own deadline: when it passes, or the parent is gone,
/// everything under this process is killed and the process exits (status 3). `deadline` is on
/// `std::time::Instant`.
pub fn render_watchdog(parent: u32, deadline: std::time::Instant, poll: f64) {
    // The daemon died mid-render and nothing would ever kill this child (re-verification, 2026-09-25).
    // SAFETY: getppid and getpid cannot fail.
    while Instant::now() < deadline && unsafe { libc::getppid() } as u32 == parent {
        std::thread::sleep(secs(poll));
    }
    kill_tree_below(std::process::id());
    // SAFETY: _exit ends the process at once, as os._exit does.
    unsafe { libc::_exit(3) }
}

/// Python's `float(text)`.
fn py_float(text: &str) -> Option<f64> {
    text.trim().replace('_', "").parse::<f64>().ok()
}

/// `_render_main(argv)`: `eve-watch --render URL [TIMEOUT]`: one render, its answer as one JSON line on stdout
/// (`{"ok": true, "html", "shot"}` or `{"ok": false, "reason", "status"}`). Returns the exit status (0).
pub fn render_main(argv: &[String]) -> i32 {
    let url = argv.get(1).cloned().unwrap_or_default();
    let timeout = match argv.get(2) {
        Some(raw) => match py_float(raw) {
            Some(t) => t,
            None => return 1, // float() raised: a traceback, and no answer
        },
        None => RENDER_TIMEOUT_SECS,
    };
    // SAFETY: getppid cannot fail.
    let parent = unsafe { libc::getppid() } as u32;
    let deadline = Instant::now() + secs(timeout + RENDER_DEADLINE_PAD_SECS);
    std::thread::spawn(move || render_watchdog(parent, deadline, 1.0));
    let rendered = std::panic::catch_unwind(|| render_here(&url, timeout, &AddressRules::system()));
    // the parent reads one JSON line whatever happened here
    let answer = match rendered {
        Ok(Ok((page, shot))) => {
            json!({"ok": true, "html": page, "shot": base64::engine::general_purpose::STANDARD.encode(shot)})
        }
        Ok(Err(e)) => json!({"ok": false, "reason": e.reason, "status": e.status}),
        Err(_) => json!({"ok": false, "reason": "the browser failed (panic)", "status": 0}),
    };
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(answer.to_string().as_bytes());
    let _ = out.flush();
    0
}
