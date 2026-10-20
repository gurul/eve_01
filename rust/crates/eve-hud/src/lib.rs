//! One-line status renderer for Claude Code's `statusLine` setting.
//!
//! Connects to the running daemon's local IPC channel, asks for a snapshot, and
//! prints a compact line. Designed to complement claude-hud rather than
//! replace it: we focus on the stick-specific signals (BLE connection,
//! encryption, battery) that Claude Code itself doesn't know about.
//!
//! Port of `bridge/src/eve_bridge/hud.py`.

use std::io::{self, Read, Write};
use std::time::Duration;

use serde_json::{Map, Value};

// Bar rendering. Keep the width compact — claude-hud already fills most of
// the statusLine, and we need to fit next to it.
pub const BAR_WIDTH: usize = 8;
pub const BAR_FULL: &str = "█";
pub const BAR_EMPTY: &str = "░";

// ANSI colour escapes. statusLine renders these; --ascii turns them off.
pub const ANSI_RESET: &str = "\x1b[0m";
pub const ANSI_RED: &str = "\x1b[31m";
pub const ANSI_YELLOW: &str = "\x1b[33m";
pub const ANSI_GREEN: &str = "\x1b[32m";

/// `_query_state`'s default timeout (0.5 s), for the connect and every read.
pub const QUERY_TIMEOUT: Duration = Duration::from_millis(500);

/// How long `run` waits for stdin before giving up on draining it (100 ms).
const STDIN_DRAIN_WAIT_MS: i32 = 100;

/// Python's `_bar`: a `width`-cell bar, `pct` clamped to 0..=100 and rounded
/// to the nearest cell.
pub fn bar(pct: i128, width: usize) -> String {
    let pct = pct.clamp(0, 100) as usize;
    let filled = (pct * width + 50) / 100; // round to nearest
    BAR_FULL.repeat(filled) + &BAR_EMPTY.repeat(width - filled)
}

/// Python's `_battery_color`: red at 15% or less, yellow at 40% or less.
pub fn battery_color(pct: i128) -> &'static str {
    if pct <= 15 {
        return ANSI_RED;
    }
    if pct <= 40 {
        return ANSI_YELLOW;
    }
    ANSI_GREEN
}

/// Compact human-readable token count. Caps at one decimal for readability.
pub fn format_tokens(n: i128) -> String {
    if n < 1_000 {
        return n.to_string();
    }
    if n < 1_000_000 {
        // 12.3K but show no decimal once we're past 100K (e.g. 850K not 850.0K)
        return if n < 100_000 { format!("{:.1}K", n as f64 / 1_000.0) } else { format!("{}K", n.div_euclid(1_000)) };
    }
    format!("{:.1}M", n as f64 / 1_000_000.0)
}

/// A JSON value Python's `isinstance(v, int)` accepts: an integer or a bool
/// (bool subclasses int). Returns the integer and Python's `str()` of it.
fn py_int(v: Option<&Value>) -> Option<(i128, String)> {
    match v? {
        Value::Bool(b) => Some((i128::from(*b), if *b { "True" } else { "False" }.to_string())),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some((i128::from(i), i.to_string()))
            } else {
                n.as_u64().map(|u| (i128::from(u), u.to_string()))
            }
        }
        _ => None,
    }
}

/// A JSON value Python's `isinstance(v, (int, float))` accepts, as a float.
fn py_number(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

/// Python truthiness of a JSON value; a missing key is `None`, which is falsy.
fn py_truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Python's `str()` of a value `json.loads` produced.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => eve_ipc::py_repr(other),
    }
}

/// Python's `_battery_segment`: `None` unless `battery_pct` is an int.
pub fn battery_segment(pct: Option<&Value>, ascii_only: bool) -> Option<String> {
    let (n, shown) = py_int(pct)?;
    let b = bar(n, BAR_WIDTH);
    if ascii_only {
        // Fall back to plain ASCII; no colours, no low-batt icon.
        return Some(format!("[{}] {shown}%", b.replace(BAR_FULL, "=").replace(BAR_EMPTY, "-")));
    }
    let icon = if n <= 15 { "🪫" } else { "🔋" };
    let color = battery_color(n);
    Some(format!("{icon} {color}{b}{ANSI_RESET} {shown}%"))
}

/// Best-effort: return the daemon's state dict or `None` if anything fails.
///
/// Sends `{"evt":"get_state"}` and reads up to the first newline (or EOF).
/// `socket_path` is an IPC path or `host:port`, resolved like the daemon does
/// (`EVE_BRIDGE_SOCK`, then the platform default).
pub fn query_state(socket_path: Option<&str>, timeout: Duration) -> Option<Value> {
    let buf = exchange(socket_path, timeout).ok()?;
    let line = match buf.iter().position(|&b| b == b'\n') {
        Some(i) => &buf[..i],
        None => &buf[..],
    };
    if line.is_empty() {
        return None;
    }
    let text = std::str::from_utf8(line).ok()?;
    let resp: Value = serde_json::from_str(text).ok()?;
    if !py_truthy(resp.get("ok")) {
        return None;
    }
    // `resp.get("state")`: a missing or null state is None.
    match resp.get("state") {
        None | Some(Value::Null) => None,
        Some(state) => Some(state.clone()),
    }
}

fn exchange(socket_path: Option<&str>, timeout: Duration) -> io::Result<Vec<u8>> {
    let transport =
        eve_ipc::make_transport(socket_path).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
    let mut s = transport.sync_connect(timeout)?;
    s.write_all(b"{\"evt\":\"get_state\"}\n")?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = match s.read(&mut chunk) {
            Ok(n) => n,
            // Python's recv retries EINTR on its own.
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if chunk[..n].contains(&b'\n') {
            break;
        }
    }
    Ok(buf)
}

/// Render the state dict to a single line suitable for statusLine.
///
/// When state is `None` (daemon off / unreachable) we say so quietly rather
/// than printing nothing, so the user notices if the bridge died.
pub fn format_line(state: Option<&Map<String, Value>>, ascii_only: bool) -> String {
    let Some(state) = state else {
        return if ascii_only { "eve: off" } else { "🐾 off" }.to_string();
    };

    if !py_truthy(state.get("ble_connected")) {
        return if ascii_only { "eve: disc" } else { "🐾 ∅" }.to_string();
    }

    let mut parts: Vec<String> = Vec::new();

    // Battery — rendered as a short coloured progress bar. Red ≤15, yellow ≤40.
    if let Some(battery) = battery_segment(state.get("battery_pct"), ascii_only) {
        parts.push(battery);
    }

    // Encryption
    match state.get("sec") {
        Some(Value::Bool(true)) => parts.push(if ascii_only { "lock" } else { "🔒" }.to_string()),
        Some(Value::Bool(false)) => parts.push(if ascii_only { "UNSEC" } else { "⚠UNSEC" }.to_string()),
        // sec=None (no status ack yet) → omit
        _ => {}
    }

    // Today's token output and estimated API cost. Tokens appear at 1K+; cost
    // appears at ~1¢+ so the line stays clean during idle sessions. Tokens are
    // the input that drives cost, so they sit together with tokens first.
    if let Some((tokens_today, _)) = py_int(state.get("tokens_today")) {
        if tokens_today >= 1_000 {
            parts.push(format_tokens(tokens_today));
        }
    }
    if let Some(cost_today) = py_number(state.get("cost_today")) {
        if cost_today >= 0.01 {
            parts.push(format!("${cost_today:.2}"));
        }
    }

    // Session activity (only if > 0)
    let running = state.get("running");
    if py_truthy(running) {
        if let Some(running) = running {
            parts.push(format!("{}run", py_str(running)));
        }
    }

    // New-release nudge — sits at the end so it doesn't push more important
    // state off-screen. Yellow to catch the eye without screaming.
    if let Some(Value::String(update)) = state.get("update_available") {
        if !update.is_empty() {
            if ascii_only {
                parts.push(format!("up {update}"));
            } else {
                parts.push(format!("{ANSI_YELLOW}↑ {update}{ANSI_RESET}"));
            }
        }
    }

    if ascii_only {
        return if parts.is_empty() { "eve: ok".to_string() } else { format!("eve: {}", parts.join(" ")) };
    }
    if parts.is_empty() {
        "🐾 ok".to_string()
    } else {
        format!("🐾 {}", parts.join(" "))
    }
}

/// Render whatever `query_state` returned: a dict is formatted, anything else
/// reads as off.
pub fn format_state(state: Option<&Value>, ascii_only: bool) -> String {
    format_line(state.and_then(Value::as_object), ascii_only)
}

/// Drain stdin without blocking on it. Claude Code's statusLine feeds JSON;
/// we don't use it today, but reading avoids leaving it unread in the pipe.
fn drain_stdin() {
    use std::io::IsTerminal;
    let stdin = io::stdin();
    if stdin.is_terminal() {
        return;
    }
    if stdin_ready(STDIN_DRAIN_WAIT_MS) {
        let mut sink = Vec::new();
        let _ = stdin.lock().read_to_end(&mut sink);
    }
}

/// `select.select([sys.stdin], [], [], 0.1)`: is stdin readable (data, EOF or
/// error) within `wait_ms`?
#[cfg(unix)]
fn stdin_ready(wait_ms: i32) -> bool {
    let mut fds = libc::pollfd { fd: libc::STDIN_FILENO, events: libc::POLLIN, revents: 0 };
    // SAFETY: one valid pollfd for the duration of the call.
    let rc = unsafe { libc::poll(&mut fds, 1, wait_ms) };
    if rc <= 0 {
        return false;
    }
    // select raises on a bad descriptor; the Python swallows that and moves on.
    fds.revents & libc::POLLNVAL == 0 && fds.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0
}

/// Off unix Python's `select` refuses a non-socket and the OSError is
/// swallowed, so stdin is left alone.
#[cfg(not(unix))]
fn stdin_ready(_wait_ms: i32) -> bool {
    false
}

/// Entry point called from the CLI (`eve-bridge hud [--ascii] [--socket X]`).
/// Consumes stdin if Claude Code sent any (statusLine passes session JSON via
/// stdin) but doesn't block on it. Always returns 0.
pub fn run(ascii_only: bool, socket_path: Option<&str>) -> i32 {
    drain_stdin();
    let state = query_state(socket_path, QUERY_TIMEOUT);
    let line = format_state(state.as_ref(), ascii_only) + "\n";
    let mut out = io::stdout().lock();
    let _ = out.write_all(line.as_bytes());
    let _ = out.flush();
    0
}

#[cfg(test)]
mod tests;
