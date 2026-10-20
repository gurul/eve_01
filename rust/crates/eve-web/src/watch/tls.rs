//! watch.py's Chrome-like read (curl_cffi).
//!
//! Some shops refuse http_request whatever its headers say: its TLS handshake is not the Chrome its user agent
//! claims (LEGO answers 403; Best Buy never answers, 20 s). curl_cffi is libcurl built to shake hands as Chrome
//! does. Measured 2026-09-26 from this Mac: LEGO, Best Buy, Walmart and StubHub product pages all read in 1-2 s,
//! where http_request got 403 or a timeout on LEGO and Best Buy. A page GET only, never an API call.
//!
//! BOUNDARY: Chrome's TLS fingerprint has no Rust path on this Mac (rquest/wreq build BoringSSL, which needs
//! cmake). The one exchange of each hop runs in curl_cffi through a Python helper ([`HELPER`], `tls_helper.py`
//! beside this file, passed with `-I -c`): it says it is ready once curl_cffi is imported (Python imports it
//! before anything else, so the deadline starts after it), then for each hop reads one JSON request on stdin
//! (url, the pinned `host:port:ip`, the seconds left, MAX_BYTES) and answers one JSON line (status, headers,
//! primary_ip, body base64, or an error kind). Everything else stays here, as Python has it: every hop check_url'd and pinned to the address its one
//! lookup checked, redirects followed here (never by libcurl), the deadline, the size cap, and the check that the
//! address libcurl used is the pinned one. The interpreter: `EVE_WATCH_TLS_PYTHON`, else the bridge checkout's
//! venv (`bridge/.venv/bin/python3`, resolved from this crate's location at build time) when it exists, else
//! `python3` on PATH. `tls_available()` is that interpreter importing curl_cffi (asked once a process).

use std::io::{BufRead, BufReader, Write};
use std::net::IpAddr;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use base64::Engine;
use regex::Regex;
use serde_json::{json, Value};

use super::net::{check_url_with, decode_body, origin, retry_after, AddressRules};
use super::{FetchError, MAX_BYTES};
use crate::Ctx;

pub const TLS_IMPERSONATE: &str = "chrome";
/// redirects followed, each one checked
pub const TLS_MAX_HOPS: usize = 5;
pub const TLS_REDIRECTS: [u16; 5] = [301, 302, 303, 307, 308];
pub const TOO_LONG: &str = "the site took too long to answer";
/// The interpreter override (new; see the module docs).
pub const PYTHON_ENV: &str = "EVE_WATCH_TLS_PYTHON";
/// The curl_cffi helper, run with `-I -c`.
pub const HELPER: &str = include_str!("tls_helper.py");

/// The bridge checkout's venv, from this crate's place in it.
const BRIDGE_PYTHON: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/.venv/bin/python3");
/// How long past the read's own deadline the helper may take to say so before it is killed.
const HELPER_GRACE_SECS: f64 = 3.0;
/// How long the interpreter may take to start and import curl_cffi before the reader counts as not installed.
const HELPER_START_SECS: f64 = 30.0;
const PRIVATE_ADDRESS: &str = "that link goes to a private or unknown address";
const NOT_INSTALLED: &str = "the Chrome-like reader is not installed";

/// The Python that has curl_cffi: EVE_WATCH_TLS_PYTHON, else the bridge checkout's venv, else `python3`.
pub fn python(ctx: &Ctx) -> PathBuf {
    let chosen = ctx.get(PYTHON_ENV);
    let chosen = chosen.trim();
    if !chosen.is_empty() {
        return PathBuf::from(chosen);
    }
    let venv = PathBuf::from(BRIDGE_PYTHON);
    if venv.exists() {
        return venv;
    }
    PathBuf::from("python3")
}

/// curl_cffi can be imported (the helper's interpreter has it), asked once a process.
pub fn tls_available() -> bool {
    static HAVE: OnceLock<bool> = OnceLock::new();
    *HAVE.get_or_init(|| tls_available_with(Ctx::global()))
}

/// `tls_available()` for `ctx`'s interpreter, asked now (no cache).
pub fn tls_available_with(ctx: &Ctx) -> bool {
    let exe = python(ctx);
    let probe = "import importlib.util; print(1 if importlib.util.find_spec('curl_cffi') else 0)";
    super::net::run_stdout(&exe.to_string_lossy(), &["-I", "-c", probe], 30.0).is_some_and(|out| out.trim() == "1")
}

/// `_pin(host, port)`: one lookup; every answer public, or the host is refused; the first answer is the address
/// used. The connection is then pinned to it (libcurl's RESOLVE), so there is no second lookup to rebind.
pub fn pin(host: &str, port: u16, rules: &AddressRules) -> Result<String, FetchError> {
    let refused = || FetchError::new(PRIVATE_ADDRESS);
    let addrs = (rules.lookup)(host, port).map_err(|_| refused())?;
    if addrs.is_empty() || !addrs.iter().all(|a| (rules.ip_public)(a.ip())) {
        return Err(refused());
    }
    Ok(addrs[0].ip().to_string())
}

/// One page GET with Chrome's TLS fingerprint: (status, body as text). The same guards as http_request, by other
/// means (see the module docs). FetchError otherwise. Blocking: run it off the loop.
pub fn tls_request(url: &str, timeout: f64) -> Result<(u16, String), FetchError> {
    tls_request_with(Ctx::global(), url, timeout, &AddressRules::system())
}

/// The helper process for one read: its hops go in on stdin, its answers come back a line each.
struct Helper {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
}

impl Helper {
    /// The helper started and curl_cffi imported (its first line says which), or "not installed".
    fn start(ctx: &Ctx) -> Result<Helper, FetchError> {
        let mut cmd = Command::new(python(ctx));
        cmd.args(["-I", "-c", HELPER]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        if let Some(env) = &ctx.env {
            cmd.envs(env.iter());
        }
        let mut child = cmd.spawn().map_err(|_| FetchError::new(NOT_INSTALLED))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or_else(|| FetchError::new(NOT_INSTALLED))?;
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let helper = Helper { child, stdin, lines };
        match helper.lines.recv_timeout(Duration::from_secs_f64(HELPER_START_SECS)) {
            Ok(line) if serde_json::from_str::<Value>(&line).is_ok_and(|v| v.get("ready") == Some(&Value::Bool(true))) => {
                Ok(helper)
            }
            _ => Err(FetchError::new(NOT_INSTALLED)),
        }
    }

    /// One hop's answer.
    fn ask(&mut self, hop: &Value, wait: Duration) -> Answer {
        let Some(stdin) = self.stdin.as_mut() else { return Answer::Gone };
        let line = format!("{hop}\n");
        if stdin.write_all(line.as_bytes()).and_then(|_| stdin.flush()).is_err() {
            return Answer::Gone;
        }
        match self.lines.recv_timeout(wait) {
            Ok(line) => serde_json::from_str(&line).map(Answer::Line).unwrap_or(Answer::Gone),
            Err(mpsc::RecvTimeoutError::Timeout) => Answer::Late,
            Err(mpsc::RecvTimeoutError::Disconnected) => Answer::Gone,
        }
    }
}

/// What a hop got back from the helper.
enum Answer {
    Line(Value),
    /// nothing by the deadline and its grace
    Late,
    /// the helper ended, or said something that is not JSON
    Gone,
}

impl Drop for Helper {
    fn drop(&mut self) {
        drop(self.stdin.take()); // EOF: a helper between hops exits by itself
        let deadline = Instant::now() + Duration::from_millis(200);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
                _ => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

static CHARSET: OnceLock<Regex> = OnceLock::new();

/// `tls_request` under `rules` and `ctx` (the interpreter's environment), the tests' seam.
pub fn tls_request_with(ctx: &Ctx, url: &str, timeout: f64, rules: &AddressRules) -> Result<(u16, String), FetchError> {
    // Python imports curl_cffi before anything else: without it, every read is "not installed"
    let mut helper = Helper::start(ctx)?;
    let re = CHARSET.get_or_init(|| Regex::new(r"(?i)charset=([\w.:-]+)").expect("a valid pattern"));
    let timeout = if timeout.is_finite() && timeout > 0.0 { timeout.min(1e9) } else { 0.0 };
    let deadline = Instant::now() + Duration::from_secs_f64(timeout);
    let mut url = url.to_string();
    for hop in 0..=TLS_MAX_HOPS {
        let why = check_url_with(&url, rules);
        if !why.is_empty() {
            return Err(FetchError::new(if hop == 0 { why } else { format!("redirected somewhere not allowed ({why})") }));
        }
        let (_, host, port) = origin(&url);
        let ip = pin(&host, port, rules)?;
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(FetchError::new(TOO_LONG));
        }
        let request = json!({
            "url": url,
            "resolve": format!("{host}:{port}:{ip}"),
            "left": left.as_secs_f64(),
            "max_bytes": MAX_BYTES,
            "impersonate": TLS_IMPERSONATE,
        });
        let wait = left + Duration::from_secs_f64(HELPER_GRACE_SECS);
        let answer = match helper.ask(&request, wait) {
            Answer::Line(a) => a,
            // libcurl's own total timeout never came back: the helper is killed (dropped), the site was too slow
            Answer::Late => return Err(FetchError::new(TOO_LONG)),
            Answer::Gone => {
                return Err(FetchError::new(if Instant::now() >= deadline { TOO_LONG } else { "the site could not be reached" }));
            }
        };
        if let Some(kind) = answer.get("error").and_then(Value::as_str) {
            return Err(FetchError::new(match kind {
                "missing" => NOT_INSTALLED,
                "too_big" => "the page is too big to read",
                "timeout" => TOO_LONG,
                _ if Instant::now() >= deadline => TOO_LONG,
                _ => "the site could not be reached",
            }));
        }
        let used = answer.get("primary_ip").and_then(Value::as_str).unwrap_or("");
        let used = used.trim_matches(|c| c == '[' || c == ']');
        let same = match (used.parse::<IpAddr>(), ip.parse::<IpAddr>()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        };
        if !same {
            return Err(FetchError::new(PRIVATE_ADDRESS));
        }
        let status = answer.get("status").and_then(Value::as_u64).unwrap_or(0).min(u16::MAX as u64) as u16;
        let headers: Vec<(String, String)> = answer
            .get("headers")
            .and_then(Value::as_array)
            .map(|pairs| {
                pairs
                    .iter()
                    .filter_map(|p| {
                        let k = p.get(0)?.as_str()?;
                        let v = p.get(1)?.as_str()?;
                        Some((k.to_string(), v.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let get = |name: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone());
        let location = get("location").unwrap_or_default();
        if TLS_REDIRECTS.contains(&status) && !location.is_empty() {
            url = match reqwest::Url::parse(&url).and_then(|base| base.join(&location)) {
                Ok(next) => next.to_string(),
                Err(_) => location,
            };
            continue;
        }
        if !(200..300).contains(&status) {
            return Err(FetchError::with_retry(
                format!("the site answered HTTP {status}"),
                status,
                retry_after(&get, crate::time_time()),
            ));
        }
        let body = base64::engine::general_purpose::STANDARD
            .decode(answer.get("body").and_then(Value::as_str).unwrap_or(""))
            .map_err(|_| FetchError::new("the site could not be reached"))?;
        let content_type = get("content-type").unwrap_or_default();
        let charset = re.captures(&content_type).map(|c| c[1].to_string());
        return Ok((status, decode_body(&body, Some(charset.as_deref().unwrap_or("utf-8")))));
    }
    Err(FetchError::new("the site redirected too many times"))
}

/// A plain read the Chrome-like read may get past: the site's own refusal (401/403) or a site that never
/// answered. Never a rate limit (429 is honoured, not dodged), a missing page, or OpenRouter's refusal.
pub fn tls_worth(e: &FetchError) -> bool {
    e.host.is_empty() && (e.status == 401 || e.status == 403 || (e.status == 0 && e.reason == TOO_LONG))
}
