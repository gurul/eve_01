//! Hook scripts invoked by Claude Code's hook system.
//!
//! Port of `eve_bridge.hooks`: one module per hook script, plus the tiny
//! synchronous transport client they share ([`client`], from `_client.py`).
//! Each hook is also a binary (`eve-hook-<name>`), the Rust form of
//! `python -m eve_bridge.hooks.<name>`.
//!
//! Every hook has a `run` that takes the stdin bytes, the daemon round-trip
//! and stdout as parameters (the seam the Python tests reached with
//! `monkeypatch`), and a `main` that wires them to the real process.

use std::fmt;
use std::io::{Read, Write};

use serde_json::Value;

pub mod client;
pub mod notification;
pub mod permission_request;
pub mod posttooluse;
pub mod pretooluse;
pub mod session_end;
pub mod session_start;
pub mod stop;
pub mod user_prompt_submit;

/// The daemon round-trip a hook makes: `post(event, timeout=...)`. Returns the
/// daemon's reply, or `None` when it is unreachable, slow or unreadable.
pub type Poster<'a> = dyn FnMut(Value, f64) -> Option<Value> + 'a;

/// An exception the Python hook would not catch. Python prints the traceback
/// and the process exits 1; Claude Code then shows a non-blocking hook error
/// and carries on, so the Rust hook does the same.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct HookError {
    /// The Python exception class (`AttributeError`, `TypeError`, `OSError`).
    pub kind: String,
    pub message: String,
}

impl fmt::Display for HookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl HookError {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        HookError { kind: kind.into(), message: message.into() }
    }

    /// `'<type>' object has no attribute '<attr>'`.
    pub fn no_attribute(v: &Value, attr: &str) -> Self {
        HookError::new("AttributeError", format!("'{}' object has no attribute '{}'", py_type_name(v), attr))
    }

    /// `'<type>' object is not iterable`.
    pub fn not_iterable(v: &Value) -> Self {
        HookError::new("TypeError", format!("'{}' object is not iterable", py_type_name(v)))
    }

    pub(crate) fn os(e: std::io::Error) -> Self {
        let kind = if e.kind() == std::io::ErrorKind::BrokenPipe { "BrokenPipeError" } else { "OSError" };
        HookError::new(kind, e.to_string())
    }
}

/// The hook entry point every binary shares: stdin in, the real daemon client,
/// stdout out. An uncaught Python exception becomes the last traceback line on
/// stderr and exit status 1.
pub fn run_main(run: impl FnOnce(&[u8], &mut Poster<'_>, &mut dyn Write) -> Result<i32, HookError>) -> i32 {
    let result: Result<i32, HookError> = (|| {
        let mut raw = Vec::new();
        std::io::stdin().lock().read_to_end(&mut raw).map_err(HookError::os)?;
        let mut post = |event: Value, timeout: f64| {
            client::post(&event, None, timeout, client::DEFAULT_TIMEOUT_SECS)
        };
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let code = run(&raw, &mut post, &mut out)?;
        out.flush().map_err(HookError::os)?;
        Ok(code)
    })();
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("Traceback (most recent call last):\n{e}");
            1
        }
    }
}

/// Write one line to stdout as `sys.stdout.write(json.dumps(out) + "\n")`.
pub(crate) fn write_json_line(out: &mut dyn Write, value: &Value) -> Result<(), HookError> {
    let mut line = eve_ipc::dumps(value);
    line.push('\n');
    out.write_all(line.as_bytes()).map_err(HookError::os)
}

/// `payload.get(key, default)` on whatever `json.loads` returned: anything but
/// a dict raises AttributeError, as in Python.
pub fn payload_get(payload: &Value, key: &str, default: Value) -> Result<Value, HookError> {
    match payload {
        Value::Object(map) => Ok(map.get(key).cloned().unwrap_or(default)),
        other => Err(HookError::no_attribute(other, "get")),
    }
}

/// `payload.get(key, "")`, the form every hook uses.
pub fn payload_str(payload: &Value, key: &str) -> Result<Value, HookError> {
    payload_get(payload, key, Value::String(String::new()))
}

/// Python's `type(v).__name__` for a value `json.loads` can produce.
pub fn py_type_name(v: &Value) -> &'static str {
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

/// Python truthiness: `None`, `False`, zero, and empty str / list / dict are false.
pub fn py_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => i != 0,
            (None, Some(u), _) => u != 0,
            (None, None, Some(f)) => f != 0.0,
            _ => true,
        },
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

/// Python's `str(v)`: a string as is, anything else as its repr.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => eve_ipc::py_repr(other),
    }
}

/// `str.isspace` for one character: Rust's `char::is_whitespace` plus the four
/// ASCII separators (U+001C..U+001F) Python also counts.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python's `str.strip()` with no arguments.
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

#[cfg(test)]
mod tests;
