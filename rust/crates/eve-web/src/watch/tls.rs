//! watch.py's Chrome-like read (curl_cffi).
//!
//! Some shops refuse http_request whatever its headers say: its TLS handshake is not the Chrome its user agent
//! claims (LEGO answers 403; Best Buy never answers, 20 s). curl_cffi is libcurl built to shake hands as Chrome
//! does. Measured 2026-09-26 from this Mac: LEGO, Best Buy, Walmart and StubHub product pages all read in 1-2 s,
//! where http_request got 403 or a timeout on LEGO and Best Buy. A page GET only, never an API call.
//!
//! BOUNDARY: Chrome's TLS fingerprint has no Rust path on this Mac (rquest/wreq build BoringSSL, which needs
//! cmake). The one exchange of each hop runs in curl_cffi through a Python helper ([`HELPER`], `tls_helper.py`
//! beside this file, passed with `-c`): it reads one JSON request on stdin (url, the pinned `host:port:ip`, the
//! seconds left, MAX_BYTES) and answers one JSON line (status, headers, primary_ip, body base64, or an error
//! kind). Everything else stays here, as Python has it: every hop check_url'd and pinned to the address its one
//! lookup checked, redirects followed here (never by libcurl), the deadline, the size cap, and the check that the
//! address libcurl used is the pinned one. The interpreter: `EVE_WATCH_TLS_PYTHON`, else the bridge checkout's
//! venv (`bridge/.venv/bin/python3`, resolved from this crate's location at build time) when it exists, else
//! `python3` on PATH. `tls_available()` is that interpreter importing curl_cffi (asked once a process).

use super::net::AddressRules;
use super::FetchError;
use crate::Ctx;

pub const TLS_IMPERSONATE: &str = "chrome";
/// redirects followed, each one checked
pub const TLS_MAX_HOPS: usize = 5;
pub const TLS_REDIRECTS: [u16; 5] = [301, 302, 303, 307, 308];
pub const TOO_LONG: &str = "the site took too long to answer";
/// The interpreter override (new; see the module docs).
pub const PYTHON_ENV: &str = "EVE_WATCH_TLS_PYTHON";
/// The curl_cffi helper, run with `-c`.
pub const HELPER: &str = include_str!("tls_helper.py");

/// The Python that has curl_cffi: EVE_WATCH_TLS_PYTHON, else the bridge checkout's venv, else `python3`.
pub fn python(ctx: &Ctx) -> std::path::PathBuf {
    let _ = ctx;
    todo!("watch.tls: the interpreter")
}

/// curl_cffi can be imported (the helper's interpreter has it), asked once a process.
pub fn tls_available() -> bool {
    todo!("watch.tls_available")
}

/// `_pin(host, port)`: one lookup; every answer public, or the host is refused; the first answer is the address
/// used. The connection is then pinned to it (libcurl's RESOLVE), so there is no second lookup to rebind.
pub fn pin(host: &str, port: u16, rules: &AddressRules) -> Result<String, FetchError> {
    let _ = (host, port, rules);
    todo!("watch._pin")
}

/// One page GET with Chrome's TLS fingerprint: (status, body as text). The same guards as http_request, by other
/// means (see the module docs). FetchError otherwise. Blocking: run it off the loop.
pub fn tls_request(url: &str, timeout: f64) -> Result<(u16, String), FetchError> {
    let _ = (url, timeout);
    todo!("watch.tls_request")
}

/// `tls_request` under `rules` and `ctx` (the interpreter's environment), the tests' seam.
pub fn tls_request_with(ctx: &Ctx, url: &str, timeout: f64, rules: &AddressRules) -> Result<(u16, String), FetchError> {
    let _ = (ctx, url, timeout, rules);
    todo!("watch.tls_request")
}

/// A plain read the Chrome-like read may get past: the site's own refusal (401/403) or a site that never
/// answered. Never a rate limit (429 is honoured, not dodged), a missing page, or OpenRouter's refusal.
pub fn tls_worth(e: &FetchError) -> bool {
    let _ = e;
    todo!("watch.tls_worth")
}
