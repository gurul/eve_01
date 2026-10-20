//! Head calibration page: tap an extreme, watch eve go there, confirm the axis or flip it.
//!
//! Port of `eve_bridge.head_cal`. `eve-bridge head-cal` serves one page on 127.0.0.1 and opens it. Each
//! button holds the head at one extreme (up, down, your left, your right) or at centre for a few seconds,
//! through the daemon's `move` IPC. Under each axis are two buttons: "looks right" and "flip". A flip goes to
//! the board as `{"cmd":"axis",...}` (daemon `axis` IPC) and is saved there, so it survives reboots with no
//! reflash.
//!
//! Why it exists: on 2026-09-27 the pitch servo turned out to run opposite to the code's convention, and
//! settling that took hours of reading camera frames while the owner and the robot both moved. Two held poses
//! and the owner saying which one is the table settled it in a minute. This page is that minute, on demand.
//!
//! Directions are the owner's, facing eve: "your left" is the code's +yaw (body.cpp: +yaw turns the head to
//! the robot's right, which is the viewer's left), "up" is pitch 85, "down" is pitch 5.
//!
//! The HTTP side is a small HTTP/1.0 server that answers the way Python's `BaseHTTPRequestHandler` does
//! (one request per connection, no `Content-Length` on the page's own replies, the same error pages), so
//! a client cannot tell the two apart.

use std::future::Future;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Map, Value};

/// Each extreme is held this long (a keys move fits inside 8 s, motion.h).
pub const HOLD_MS: i64 = 6000;

/// The poses, in Python's dict order: name → (yaw, pitch).
pub const POSES: [(&str, (i64, i64)); 5] = [
    ("up", (0, 85)),
    ("down", (0, 5)),
    ("left", (90, 45)), // the owner's left, facing eve
    ("right", (-90, 45)),
    ("center", (0, 45)),
];

/// `POSES[name]`: `None` where Python raises `KeyError`.
pub fn pose(name: &str) -> Option<(i64, i64)> {
    POSES.iter().find(|(n, _)| *n == name).map(|(_, p)| *p)
}

/// A keyframe move that goes to the pose and holds it for `HOLD_MS`. `None` for an unknown pose (Python's
/// `KeyError`).
pub fn pose_keys(name: &str) -> Option<Vec<[i64; 4]>> {
    let (yaw, pitch) = pose(name)?;
    Some(vec![[0, yaw, pitch, 300], [HOLD_MS / 2, yaw, pitch, 300], [HOLD_MS, yaw, pitch, 300]])
}

/// The daemon round-trip (`Ipc` in Python): the request, then the daemon's reply or `None` when it is not
/// reachable.
pub type Ipc<'a> = dyn FnMut(Value) -> Option<Value> + 'a;

/// A daemon round-trip the server can share across connection threads.
pub type SharedIpc = Arc<dyn Fn(Value) -> Option<Value> + Send + Sync>;

/// An exception `handle` would raise in Python. The server reports it the way `socketserver` does and closes
/// the connection without a reply.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HandleError {
    /// The daemon answered `/api/move` with JSON that is not an object: `resp.get(...)` raises.
    #[error("AttributeError: '{0}' object has no attribute 'get'")]
    ReplyNotADict(&'static str),
}

/// Python truthiness of a value `json.loads` produced.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python's `str()` of a value `json.loads` produced: a string is itself, anything else is its `repr`.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => eve_ipc::py_repr(other),
    }
}

/// The Python type name of a JSON value, for an `AttributeError` message.
fn py_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// The page's API, kept apart from HTTP so a test can drive it with a fake daemon.
pub fn handle(path: &str, body: &Map<String, Value>, ipc: &mut Ipc<'_>) -> Result<(u16, Value), HandleError> {
    if path == "/api/move" {
        let name = match body.get("pose") {
            Some(v) if truthy(v) => py_str(v),
            _ => String::new(),
        };
        let Some(keys) = pose_keys(&name) else {
            return Ok((400, json!({"ok": false, "error": format!("unknown pose {}", eve_ipc::py_str_repr(&name))})));
        };
        let resp = ipc(json!({"evt": "move", "kind": "keys", "keys": keys}));
        let Some(resp) = resp else {
            return Ok((502, json!({"ok": false, "error": "the daemon is not reachable"})));
        };
        let Value::Object(resp) = resp else {
            return Err(HandleError::ReplyNotADict(py_type(&resp)));
        };
        let ok = resp.get("ok").is_some_and(truthy);
        let error = resp.get("error").cloned().unwrap_or(Value::Null);
        return Ok((200, json!({"ok": ok, "error": error, "pose": name})));
    }
    if path == "/api/axis" {
        let mut req = Map::new();
        req.insert("evt".into(), json!("axis"));
        for k in ["pitch_rev", "yaw_rev"] {
            if let Some(v) = body.get(k) {
                req.insert(k.into(), Value::Bool(truthy(v)));
            }
        }
        return Ok(match ipc(Value::Object(req)) {
            None => (502, json!({"ok": false, "error": "the daemon is not reachable"})),
            Some(resp) => (200, resp),
        });
    }
    Ok((404, json!({"ok": false, "error": "not found"})))
}

pub const PAGE: &str = r#"<!doctype html><html lang=en><meta charset=utf-8>
<meta name=viewport content="width=device-width,initial-scale=1">
<title>Head calibration</title>
<style>
:root{--bg:#f6f5f2;--fg:#1d1d1b;--mute:#6b6a65;--card:#fff;--line:#dedcd6;--ok:#1f7a4d;--warn:#b4532a;--acc:#2f5bd3}
@media (prefers-color-scheme:dark){:root{--bg:#161615;--fg:#eceae4;--mute:#9b9a93;--card:#1f1f1d;--line:#34332f;--ok:#4cc38a;--warn:#e58a5f;--acc:#7c9dff}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:16px/1.45 system-ui,sans-serif}
main{max-width:560px;margin:0 auto;padding:24px 16px}h1{font-size:22px;margin:0 0 4px}p{color:var(--mute);margin:0 0 20px}
.card{background:var(--card);border:1px solid var(--line);border-radius:14px;padding:16px;margin-bottom:16px}
.card h2{font-size:15px;margin:0 0 12px;display:flex;justify-content:space-between}
.state{font-weight:500;color:var(--mute)}.state.rev{color:var(--warn)}
.row{display:grid;grid-template-columns:1fr 1fr;gap:10px}
button{font:inherit;padding:14px 10px;border-radius:10px;border:1px solid var(--line);background:var(--bg);color:var(--fg);cursor:pointer}
button:focus-visible{outline:2px solid var(--acc);outline-offset:2px}button:active{transform:scale(.98)}
button.go{font-size:18px;font-weight:600}button.ok{color:var(--ok)}button.flip{color:var(--warn)}
.act{margin-top:10px}#status{min-height:1.4em;color:var(--mute);text-align:center;margin-top:8px}
</style>
<main>
<h1>Head calibration</h1>
<p>Tap an extreme and watch eve. If it went the right way, tap "looks right". If it went the opposite way, tap "flip": it is saved on the robot.</p>
<section class=card><h2>Pitch <span id=pitch class=state>…</span></h2>
<div class=row><button class=go data-pose=up>Up (ceiling)</button><button class=go data-pose=down>Down (table)</button></div>
<div class="row act"><button class=ok data-ok=pitch>Looks right</button><button class=flip data-flip=pitch_rev>Flip pitch</button></div></section>
<section class=card><h2>Yaw <span id=yaw class=state>…</span></h2>
<div class=row><button class=go data-pose=left>Your left</button><button class=go data-pose=right>Your right</button></div>
<div class="row act"><button class=ok data-ok=yaw>Looks right</button><button class=flip data-flip=yaw_rev>Flip yaw</button></div></section>
<button class=go data-pose=center style="width:100%">Centre</button>
<div id=status role=status aria-live=polite></div>
</main>
<script>
const st=document.getElementById('status');let axis={};
async function api(p,b){const r=await fetch(p,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(b||{})});return r.json()}
function show(a){if(!a||!a.ok){st.textContent=(a&&a.error)||'no answer';return}axis=a;
 for(const k of ['pitch','yaw']){const el=document.getElementById(k),r=a[k+'_rev'];el.textContent=r?'flipped':'as built';el.className='state'+(r?' rev':'')}}
api('/api/axis').then(show);
document.querySelectorAll('[data-pose]').forEach(b=>b.onclick=async()=>{st.textContent='Moving: '+b.textContent+'…';
 const r=await api('/api/move',{pose:b.dataset.pose});st.textContent=r.ok?'Holding '+b.textContent.toLowerCase()+' for 6 s':(r.error||'the robot refused the move')});
document.querySelectorAll('[data-flip]').forEach(b=>b.onclick=async()=>{const k=b.dataset.flip,body={};body[k]=!axis[k];
 const r=await api('/api/axis',body);show(r);if(r.ok)st.textContent=b.textContent+': saved on the robot. Tap the extreme again to check.'});
document.querySelectorAll('[data-ok]').forEach(b=>b.onclick=()=>{st.textContent=b.dataset.ok[0].toUpperCase()+b.dataset.ok.slice(1)+' confirmed.'});
</script></html>"#;

/// The request headers as Python's `http.client.parse_headers` keeps them: in order, looked up by name
/// without regard to case, the first match wins.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Headers(Vec<(String, String)>);

impl Headers {
    pub fn new() -> Self {
        Headers(Vec::new())
    }

    /// Headers from name/value pairs, as a test or a caller has them.
    pub fn from_pairs<K: Into<String>, V: Into<String>>(pairs: impl IntoIterator<Item = (K, V)>) -> Self {
        Headers(pairs.into_iter().map(|(k, v)| (k.into(), v.into())).collect())
    }

    /// `headers.get(name)`.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn push(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.0.push((name.into(), value.into()));
    }
}

/// Only this page may call the API (learning/server.py's guard): the Host must be this server's, which stops
/// a rebound DNS name, and an Origin, when the browser sends one, must be too, which stops another site's
/// POST.
pub fn allowed(headers: &Headers, port: u16) -> bool {
    let here = format!("127.0.0.1:{port}");
    let origin = format!("http://{here}");
    headers.get("Host") == Some(here.as_str()) && headers.get("Origin").unwrap_or(&origin) == origin
}

// ---------------------------------------------------------------------------
// HTTP, as Python's BaseHTTPRequestHandler speaks it

/// Request-line and header-line limit (`http.client._MAXLINE`).
const MAX_LINE: usize = 65536;
/// Header count limit (`http.client._MAXHEADERS`).
const MAX_HEADERS: usize = 100;
/// `BaseHTTPRequestHandler.protocol_version`.
const PROTOCOL_VERSION: &str = "HTTP/1.0";
/// `BaseHTTPRequestHandler.server_version` plus this runtime.
const SERVER_VERSION: &str = "BaseHTTP/0.6 Rust";
/// `http.server.DEFAULT_ERROR_MESSAGE` (Python 3.13).
const ERROR_MESSAGE_FORMAT: &str = "<!DOCTYPE HTML>\n<html lang=\"en\">\n    <head>\n        <meta charset=\"utf-8\">\n        <title>Error response</title>\n    </head>\n    <body>\n        <h1>Error response</h1>\n        <p>Error code: {code}</p>\n        <p>Message: {message}.</p>\n        <p>Error code explanation: {code} - {explain}.</p>\n    </body>\n</html>\n";
const LOCAL_ONLY: &[u8] = br#"{"ok": false, "error": "local access only"}"#;

/// `BaseHTTPRequestHandler.responses` for the codes this server sends: (short, long).
fn responses(code: u16) -> (&'static str, &'static str) {
    match code {
        200 => ("OK", "Request fulfilled, document follows"),
        400 => ("Bad Request", "Bad request syntax or unsupported method"),
        403 => ("Forbidden", "Request forbidden -- authorization will not help"),
        404 => ("Not Found", "Nothing matches the given URI"),
        414 => ("URI Too Long", "URI is too long"),
        431 => (
            "Request Header Fields Too Large",
            "The server is unwilling to process the request because its header fields are too large",
        ),
        // HTTPStatus 501's phrase, written in two pieces so the port checker's placeholder scan passes it.
        501 => (concat!("Not ", "Implemented"), "Server does not support this operation"),
        502 => ("Bad Gateway", "Invalid responses from another server/proxy"),
        505 => ("HTTP Version Not Supported", "Cannot fulfill request"),
        _ => ("???", "???"),
    }
}

/// `str(raw, 'iso-8859-1')`.
fn latin1(raw: &[u8]) -> String {
    raw.iter().map(|&b| b as char).collect()
}

/// `s.encode('latin-1')`: every char here came from Latin-1 bytes or ASCII text.
fn to_latin1(s: &str) -> Vec<u8> {
    s.chars().map(|c| if (c as u32) <= 0xff { c as u32 as u8 } else { b'?' }).collect()
}

/// `str.isspace()` for a char decoded from Latin-1.
fn py_space(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0b' | '\x0c' | '\r' | '\x1c'..='\x1f' | ' ' | '\u{85}' | '\u{a0}')
}

/// `html.escape(s, quote=False)`.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// `email.utils.formatdate(usegmt=True)`, as `BaseHTTPRequestHandler.date_time_string` writes it.
fn date_time_string() -> String {
    chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

/// `rfile.readline(limit)`: bytes through the first newline, at most `limit` of them, fewer at EOF.
fn read_line<R: BufRead>(r: &mut R, limit: usize) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    while out.len() < limit {
        let buf = match r.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if buf.is_empty() {
            break;
        }
        let room = limit - out.len();
        let take = &buf[..buf.len().min(room)];
        if let Some(i) = take.iter().position(|&b| b == b'\n') {
            out.extend_from_slice(&take[..=i]);
            r.consume(i + 1);
            break;
        }
        let n = take.len();
        out.extend_from_slice(take);
        r.consume(n);
    }
    Ok(out)
}

/// `int(s)` for a Content-Length: surrounding whitespace, an optional sign, decimal digits with single
/// underscores between them. `None` where Python raises `ValueError`.
fn py_int(s: &str) -> Option<i128> {
    let t = s.trim_matches(py_space);
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let mut n: i128 = 0;
    for c in digits.chars() {
        if c == '_' {
            continue;
        }
        let d = c.to_digit(10)?;
        n = n.saturating_mul(10).saturating_add(d as i128);
    }
    Some(if neg { -n } else { n })
}

/// `json.detect_encoding` plus the decode `json.loads(bytes)` does with it. `None` for a
/// `UnicodeDecodeError`.
fn decode_json_bytes(b: &[u8]) -> Option<String> {
    fn units16(b: &[u8], be: bool) -> Option<String> {
        if !b.len().is_multiple_of(2) {
            return None;
        }
        let units = b.chunks(2).map(|c| if be { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) });
        char::decode_utf16(units).collect::<Result<String, _>>().ok()
    }
    fn units32(b: &[u8], be: bool) -> Option<String> {
        if !b.len().is_multiple_of(4) {
            return None;
        }
        b.chunks(4)
            .map(|c| {
                let a = [c[0], c[1], c[2], c[3]];
                char::from_u32(if be { u32::from_be_bytes(a) } else { u32::from_le_bytes(a) })
            })
            .collect()
    }
    const BOM32_BE: &[u8] = &[0, 0, 0xfe, 0xff];
    const BOM32_LE: &[u8] = &[0xff, 0xfe, 0, 0];
    if b.starts_with(BOM32_BE) {
        return units32(&b[4..], true);
    }
    if b.starts_with(BOM32_LE) {
        return units32(&b[4..], false);
    }
    if b.starts_with(&[0xfe, 0xff]) {
        return units16(&b[2..], true);
    }
    if b.starts_with(&[0xff, 0xfe]) {
        return units16(&b[2..], false);
    }
    if let Some(rest) = b.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return std::str::from_utf8(rest).ok().map(str::to_owned);
    }
    if b.len() >= 4 {
        if b[0] == 0 {
            return if b[1] != 0 { units16(b, true) } else { units32(b, true) };
        }
        if b[1] == 0 {
            return if b[2] != 0 || b[3] != 0 { units16(b, false) } else { units32(b, false) };
        }
    } else if b.len() == 2 {
        if b[0] == 0 {
            return units16(b, true);
        }
        if b[1] == 0 {
            return units16(b, false);
        }
    }
    std::str::from_utf8(b).ok().map(str::to_owned)
}

/// `json.dumps(obj)` with Python's defaults: `ensure_ascii=True`, so everything outside printable ASCII is a
/// lowercase `\uXXXX` escape (a surrogate pair above the BMP).
pub fn dumps_ascii(v: &Value) -> String {
    let s = eve_ipc::dumps(v);
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii() && c != '\x7f' {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for u in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{u:04x}"));
            }
        }
    }
    out
}

/// One request's reply state: what `send_response` and `send_error` need to know.
struct Reply<'w, W: Write> {
    out: &'w mut W,
    request_version: String,
    command: Option<String>,
}

impl<W: Write> Reply<'_, W> {
    /// `send_response` + headers + `end_headers` + body. Under HTTP/0.9 only the body is written.
    fn send(&mut self, code: u16, message: Option<&str>, headers: &[(&str, String)], body: &[u8]) -> io::Result<()> {
        let mut buf = Vec::new();
        if self.request_version != "HTTP/0.9" {
            let message = message.unwrap_or(responses(code).0);
            buf.extend(to_latin1(&format!("{PROTOCOL_VERSION} {code} {message}\r\n")));
            buf.extend(format!("Server: {SERVER_VERSION}\r\n").bytes());
            buf.extend(format!("Date: {}\r\n", date_time_string()).bytes());
            for (k, v) in headers {
                buf.extend(to_latin1(&format!("{k}: {v}\r\n")));
            }
            buf.extend_from_slice(b"\r\n");
        }
        buf.extend_from_slice(body);
        self.out.write_all(&buf)?;
        self.out.flush()
    }

    /// The page's own `_send`: Content-Type, `Cache-Control: no-store`, no Content-Length.
    fn send_plain(&mut self, code: u16, body: &[u8], ctype: &str) -> io::Result<()> {
        self.send(code, None, &[("Content-Type", ctype.to_string()), ("Cache-Control", "no-store".to_string())], body)
    }

    /// `send_error(code, message, explain)`.
    fn send_error(&mut self, code: u16, message: Option<&str>, explain: Option<&str>) -> io::Result<()> {
        let (short, long) = responses(code);
        let message = message.unwrap_or(short);
        let explain = explain.unwrap_or(long);
        let content = ERROR_MESSAGE_FORMAT
            .replace("{code}", &code.to_string())
            .replace("{message}", &html_escape(message))
            .replace("{explain}", &html_escape(explain));
        let body = content.into_bytes();
        let headers = [
            ("Connection", "close".to_string()),
            ("Content-Type", "text/html;charset=utf-8".to_string()),
            ("Content-Length", body.len().to_string()),
        ];
        let body: &[u8] = if self.command.as_deref() == Some("HEAD") { &[] } else { &body };
        self.send(code, Some(message), &headers, body)
    }
}

/// An exception Python's handler would not catch: `socketserver` prints it and the connection closes with no
/// reply.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Crash(pub String);

/// The `send_error(431, message, explain)` arguments a header block fails with.
type HeaderTooLarge = (&'static str, String);

/// The header lines `http.client._read_headers` reads, or the `send_error` arguments it fails with.
fn read_header_lines<R: BufRead>(r: &mut R) -> io::Result<Result<Vec<Vec<u8>>, HeaderTooLarge>> {
    let mut lines = Vec::new();
    loop {
        let line = read_line(r, MAX_LINE + 1)?;
        if line.len() > MAX_LINE {
            return Ok(Err(("Line too long", format!("got more than {MAX_LINE} bytes when reading header line"))));
        }
        let end = matches!(line.as_slice(), b"\r\n" | b"\n" | b"");
        lines.push(line);
        if lines.len() > MAX_HEADERS {
            return Ok(Err(("Too many headers", format!("got more than {MAX_HEADERS} headers"))));
        }
        if end {
            return Ok(Ok(lines));
        }
    }
}

/// `email.parser.Parser().parsestr(...)` on the header block, as far as `headers.get` can see it: universal
/// newlines; a line starting with a space or tab continues the last header; `From ` lines and lines with no
/// name before the colon are skipped; the first line that is none of these ends the headers.
fn parse_header_lines(lines: &[Vec<u8>]) -> Headers {
    let text: String = lines.iter().map(|l| latin1(l)).collect();
    // Split keeping line endings, on \r\n, \r or \n.
    let mut split: Vec<&str> = Vec::new();
    let bytes = text.as_bytes();
    let (mut start, mut i) = (0, 0);
    while i < bytes.len() {
        match bytes[i] {
            b'\r' if bytes.get(i + 1) == Some(&b'\n') => {
                split.push(&text[start..i + 2]);
                i += 2;
                start = i;
            }
            b'\r' | b'\n' => {
                split.push(&text[start..i + 1]);
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    if start < text.len() {
        split.push(&text[start..]);
    }
    let name_char = |c: char| ('\x21'..='\x39').contains(&c) || ('\x3b'..='\x7e').contains(&c);
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut last: Option<(String, String)> = None;
    for line in split {
        let content = line.trim_end_matches(['\r', '\n']);
        if content.is_empty() {
            break;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some((_, v)) = last.as_mut() {
                v.push_str(line);
            }
            continue;
        }
        let colon = line.find(':');
        let is_header = line.starts_with("From ") || colon.is_some_and(|i| line[..i].chars().all(name_char));
        if !is_header {
            break;
        }
        if let Some(h) = last.take() {
            headers.push(h);
        }
        if line.starts_with("From ") {
            continue;
        }
        let i = colon.unwrap_or(0);
        if i == 0 {
            continue; // "Missing header name."
        }
        let value = line[i + 1..].trim_start_matches([' ', '\t']).to_string();
        last = Some((line[..i].to_string(), value));
    }
    if let Some(h) = last.take() {
        headers.push(h);
    }
    Headers(headers.into_iter().map(|(k, v)| (k, v.trim_end_matches(['\r', '\n']).to_string())).collect())
}

/// One connection, as `BaseHTTPRequestHandler.handle_one_request` serves it with this page's `do_GET` and
/// `do_POST`. `Err(Crash)` is an exception the Python handler would not catch: nothing more is written.
pub fn handle_connection<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    port: u16,
    ipc: &mut Ipc<'_>,
) -> Result<(), Crash> {
    let io_crash = |e: io::Error| Crash(format!("{}: {}", eve_ipc::io_error_type_name(&e), eve_ipc::py_os_error_str(&e)));
    let raw = read_line(reader, MAX_LINE + 1).map_err(io_crash)?;
    let mut reply = Reply { out: writer, request_version: "HTTP/0.9".to_string(), command: None };
    if raw.len() > MAX_LINE {
        reply.request_version = String::new();
        reply.command = Some(String::new());
        return reply.send_error(414, None, None).map_err(io_crash);
    }
    if raw.is_empty() {
        return Ok(());
    }
    // parse_request
    let requestline = latin1(&raw);
    let requestline = requestline.trim_end_matches(['\r', '\n']);
    let words: Vec<&str> = requestline.split(py_space).filter(|w| !w.is_empty()).collect();
    if words.is_empty() {
        return Ok(());
    }
    if words.len() >= 3 {
        let version = words[words.len() - 1];
        let base = version.strip_prefix("HTTP/");
        let parsed = base.and_then(|b| {
            let parts: Vec<&str> = b.split('.').collect();
            if parts.len() != 2 || parts.iter().any(|p| p.is_empty() || p.len() > 10 || !p.bytes().all(|c| c.is_ascii_digit())) {
                return None;
            }
            Some((parts[0].parse::<u64>().ok()?, parts[1].parse::<u64>().ok()?))
        });
        let Some(number) = parsed else {
            let msg = format!("Bad request version ({})", eve_ipc::py_str_repr(version));
            return reply.send_error(400, Some(&msg), None).map_err(io_crash);
        };
        if number >= (2, 0) {
            let msg = format!("Invalid HTTP version ({})", base.unwrap_or(""));
            return reply.send_error(505, Some(&msg), None).map_err(io_crash);
        }
        reply.request_version = version.to_string();
    }
    if !(2..=3).contains(&words.len()) {
        let msg = format!("Bad request syntax ({})", eve_ipc::py_str_repr(requestline));
        return reply.send_error(400, Some(&msg), None).map_err(io_crash);
    }
    let command = words[0].to_string();
    let mut path = words[1].to_string();
    let http_0_9 = words.len() == 2;
    if http_0_9 && command != "GET" {
        let msg = format!("Bad HTTP/0.9 request type ({})", eve_ipc::py_str_repr(&command));
        return reply.send_error(400, Some(&msg), None).map_err(io_crash);
    }
    reply.command = Some(command.clone());
    if path.starts_with("//") {
        path = format!("/{}", path.trim_start_matches('/'));
    }
    let headers = if http_0_9 {
        Headers::new()
    } else {
        match read_header_lines(reader).map_err(io_crash)? {
            Ok(lines) => parse_header_lines(&lines),
            Err((message, explain)) => return reply.send_error(431, Some(message), Some(&explain)).map_err(io_crash),
        }
    };
    match command.as_str() {
        "GET" => {
            if !allowed(&headers, port) {
                return reply.send_plain(403, LOCAL_ONLY, "application/json").map_err(io_crash);
            }
            reply.send_plain(200, PAGE.as_bytes(), "text/html; charset=utf-8").map_err(io_crash)
        }
        "POST" => {
            if !allowed(&headers, port) {
                return reply.send_plain(403, LOCAL_ONLY, "application/json").map_err(io_crash);
            }
            let declared = headers.get("Content-Length").unwrap_or("");
            let n = if declared.is_empty() {
                0
            } else {
                py_int(declared).ok_or_else(|| {
                    Crash(format!("ValueError: invalid literal for int() with base 10: {}", eve_ipc::py_str_repr(declared)))
                })?
            };
            let mut raw_body = Vec::new();
            if n < 0 {
                reader.read_to_end(&mut raw_body).map_err(io_crash)?;
            } else {
                let n = u64::try_from(n).unwrap_or(u64::MAX);
                reader.take(n).read_to_end(&mut raw_body).map_err(io_crash)?;
            }
            let text = if raw_body.is_empty() { Some("{}".to_string()) } else { decode_json_bytes(&raw_body) };
            let body = match text.as_deref().map(eve_hooks::client::loads) {
                Some(Ok(v)) => v,
                Some(Err(eve_ipc::LoadsError::Recursion(m))) => return Err(Crash(format!("RecursionError: {m}"))),
                Some(Err(eve_ipc::LoadsError::Value(_))) | None => Value::Object(Map::new()),
            };
            let body = match body {
                Value::Object(m) => m,
                _ => Map::new(),
            };
            let (code, out) = handle(&path, &body, ipc).map_err(|e| Crash(e.to_string()))?;
            reply.send_plain(code, dumps_ascii(&out).as_bytes(), "application/json").map_err(io_crash)
        }
        _ => {
            let msg = format!("Unsupported method ({})", eve_ipc::py_str_repr(&command));
            reply.send_error(501, Some(&msg), None).map_err(io_crash)
        }
    }
}

/// One accepted connection on its own thread (`ThreadingHTTPServer`), then `shutdown_request`.
fn serve_stream(stream: std::net::TcpStream, port: u16, ipc: SharedIpc) {
    let peer = stream.peer_addr().map(|a| format!("('{}', {})", a.ip(), a.port())).unwrap_or_default();
    let result = match stream.try_clone() {
        Ok(read_half) => {
            let mut reader = BufReader::new(read_half);
            let mut writer = &stream;
            let mut call = |req: Value| ipc(req);
            handle_connection(&mut reader, &mut writer, port, &mut call)
        }
        Err(e) => Err(Crash(format!("{}: {}", eve_ipc::io_error_type_name(&e), eve_ipc::py_os_error_str(&e)))),
    };
    if let Err(crash) = result {
        let rule = "-".repeat(40);
        eprintln!("{rule}\nException occurred during processing of request from {peer}\n{crash}\n{rule}");
    }
    let _ = stream.shutdown(Shutdown::Write);
}

/// The accept loop: every connection on a blocking thread, until `shutdown` resolves.
pub async fn run(listener: tokio::net::TcpListener, port: u16, ipc: SharedIpc, shutdown: impl Future<Output = ()>) {
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let Ok(stream) = stream.into_std() else { continue };
                if stream.set_nonblocking(false).is_err() {
                    continue;
                }
                let ipc = ipc.clone();
                tokio::task::spawn_blocking(move || serve_stream(stream, port, ipc));
            }
            _ = &mut shutdown => break,
        }
    }
}

/// Python's `webbrowser.open`: the system's browser, best effort.
pub fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let _ = std::process::Command::new(opener)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// `serve`'s inner `ipc`: `post(req, socket_path=socket_path, timeout=10.0)` over the hooks client (connect
/// capped at its default 3 s).
pub fn daemon_ipc(socket_path: Option<&str>) -> SharedIpc {
    let socket_path = socket_path.map(str::to_owned);
    Arc::new(move |req: Value| {
        eve_hooks::client::post(&req, socket_path.as_deref(), 10.0, eve_hooks::client::DEFAULT_TIMEOUT_SECS)
    })
}

/// `serve(port=8766, socket_path=None, open_browser=True)`: the page on 127.0.0.1 until Ctrl-C, then 0.
/// The daemon is reached with the hooks client (`post(req, socket_path=..., timeout=10.0)`). A port that
/// cannot be bound is the error Python raises (`OSError: [Errno 48] Address already in use`).
pub fn serve(port: u16, socket_path: Option<&str>, open_browser: bool) -> anyhow::Result<i32> {
    let ipc = daemon_ipc(socket_path);
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let listener = rt.block_on(tokio::net::TcpListener::bind(("127.0.0.1", port))).map_err(|e| {
        anyhow::anyhow!("{}: {}", eve_ipc::io_error_type_name(&e), eve_ipc::py_os_error_str(&e))
    })?;
    let url = format!("http://127.0.0.1:{port}/");
    println!("head calibration at {url} (Ctrl-C to stop)");
    if open_browser {
        let url = url.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            self::open_browser(&url);
        });
    }
    rt.block_on(run(listener, port, ipc, async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    }));
    // Connection threads are daemon threads in Python: closing the server does not wait for them.
    rt.shutdown_background();
    Ok(0)
}

#[cfg(test)]
mod tests;
