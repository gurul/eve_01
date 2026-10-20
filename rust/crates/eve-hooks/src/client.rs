//! Tiny synchronous transport client used by hook scripts (`hooks/_client.py`).
//!
//! Hooks are short-lived subprocesses, so the client side stays synchronous even
//! though the shared IPC transport can also start async servers for the daemon.
//!
//! Transport (Unix socket vs TCP loopback) is resolved by
//! [`eve_ipc::make_transport`]; the `socket_path` argument is forwarded as a
//! *transport spec*. On POSIX this is typically a path; on Windows it's
//! `host:port`.
//!
//! If the daemon is unreachable or slow, we return `None` so the caller can
//! degrade gracefully (i.e., don't block Claude Code's normal flow).

use std::borrow::Cow;
use std::io::{self, Read, Write};
use std::time::Duration;

use serde_json::{Map, Value};

/// How long a hook is willing to wait for the daemon before giving up.
/// PreToolUse overrides this to a much larger value for the BLE round-trip.
pub const DEFAULT_TIMEOUT_SECS: f64 = 3.0;

/// The `_clean` half of `read_hook_input`, done on the JSON text: Python's
/// `json.loads` accepts a `\uXXXX` escape for a lone surrogate and `_clean`
/// then turns it into `?` (`encode("utf-8", errors="replace")`). A Rust string
/// cannot hold a lone surrogate, so each such escape becomes a literal `?`
/// before parsing; a well-formed high/low pair is left for the parser to join.
pub fn replace_lone_surrogates(text: &str) -> Cow<'_, str> {
    if !text.contains("\\u") {
        return Cow::Borrowed(text);
    }
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut last = 0; // start of the not-yet-copied text
    let mut i = 0;
    let mut in_string = false;
    let mut changed = false;
    while i < b.len() {
        match b[i] {
            b'"' => {
                in_string = !in_string;
                i += 1;
            }
            b'\\' if in_string => {
                if b.get(i + 1) == Some(&b'u') {
                    if let Some(cp) = hex4(b, i + 2) {
                        if (0xD800..0xDC00).contains(&cp) {
                            let low = if b.get(i + 6) == Some(&b'\\') && b.get(i + 7) == Some(&b'u') {
                                hex4(b, i + 8)
                            } else {
                                None
                            };
                            if matches!(low, Some(l) if (0xDC00..0xE000).contains(&l)) {
                                i += 12;
                                continue;
                            }
                        }
                        if (0xD800..0xE000).contains(&cp) {
                            out.push_str(&text[last..i]);
                            out.push('?');
                            i += 6;
                            last = i;
                            changed = true;
                            continue;
                        }
                        i += 6;
                        continue;
                    }
                }
                // Any other escape: skip the backslash and the escaped byte.
                i += 2;
            }
            _ => i += 1,
        }
    }
    if !changed {
        return Cow::Borrowed(text);
    }
    out.push_str(&text[last.min(text.len())..]);
    Cow::Owned(out)
}

fn hex4(b: &[u8], at: usize) -> Option<u32> {
    let digits = b.get(at..at + 4)?;
    let s = std::str::from_utf8(digits).ok()?;
    if !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(s, 16).ok()
}

/// `json.loads(text)`, with lone surrogate escapes read as `?`.
pub fn loads(text: &str) -> Result<Value, serde_json::Error> {
    serde_json::from_str(&replace_lone_surrogates(text))
}

/// `read_hook_input` on bytes already read: empty → `{}`; decoded as UTF-8
/// with errors replaced; JSON that does not parse → `{}`. The parsed value is
/// returned whatever its type, as `json.loads` gives it.
pub fn read_hook_input_from(raw: &[u8]) -> Value {
    if raw.is_empty() {
        return Value::Object(Map::new());
    }
    // Force UTF-8 regardless of the console encoding: Claude Code writes UTF-8 JSON.
    let data = String::from_utf8_lossy(raw);
    loads(&data).unwrap_or_else(|_| Value::Object(Map::new()))
}

/// Read Claude Code's JSON hook payload from stdin.
pub fn read_hook_input() -> io::Result<Value> {
    let mut raw = Vec::new();
    io::stdin().lock().read_to_end(&mut raw)?;
    Ok(read_hook_input_from(&raw))
}

/// Send one JSON event, read one JSON response, close. Returns `None` on any error.
///
/// `connect_timeout` caps the connect (callers pass [`DEFAULT_TIMEOUT_SECS`],
/// Python's default). `timeout` caps waiting for the daemon's response once
/// connected. Separating the two prevents port-exhaustion stalls from blocking
/// for the full `timeout` value (e.g. 320 s for pretooluse) at the connect step.
pub fn post(event: &Value, socket_path: Option<&str>, timeout: f64, connect_timeout: f64) -> Option<Value> {
    let buf = exchange(event, socket_path, timeout, connect_timeout)?;
    let line = buf.split(|&c| c == b'\n').next().unwrap_or(&[]);
    if line.is_empty() {
        return None;
    }
    let text = std::str::from_utf8(line).ok()?;
    loads(text).ok()
}

/// The socket half of `post`: the bytes read up to the first newline or EOF.
/// The stream is closed when it drops.
fn exchange(event: &Value, socket_path: Option<&str>, timeout: f64, connect_timeout: f64) -> Option<Vec<u8>> {
    let transport = eve_ipc::make_transport(socket_path).ok()?;
    let connect_timeout = Duration::try_from_secs_f64(connect_timeout).ok()?;
    let timeout = Duration::try_from_secs_f64(timeout).ok()?;
    let mut s = transport.sync_connect(connect_timeout).ok()?;
    // Switch to the (possibly longer) read timeout after connect succeeds.
    s.set_timeout(timeout).ok()?;
    let mut line = eve_ipc::dumps(event);
    line.push('\n');
    s.write_all(line.as_bytes()).ok()?;
    // Read until newline.
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match s.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if chunk[..n].contains(&b'\n') {
                    break;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    Some(buf)
}
