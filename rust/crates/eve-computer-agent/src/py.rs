//! The small pieces of Python semantics the agent leans on: truthiness, `str()` of a JSON value, `int()` and
//! `float()` of an env string, `repr()` of a string, `urlsplit`, `quote_plus`, `round` and `json.dumps`.

use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;

pub use eve_desktop_lanes::{py_collapse, py_round_n, py_strip};

/// Python truthiness of a JSON value.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

/// `repr()` of a str.
pub fn py_repr(s: &str) -> String {
    eve_decider::py_str_repr(s)
}

fn float_str(x: f64) -> String {
    if x.is_nan() {
        "nan".into()
    } else if x.is_infinite() {
        if x > 0.0 { "inf".into() } else { "-inf".into() }
    } else {
        eve_spend::py_float_repr(x)
    }
}

/// `repr()` of a JSON value as Python would print the decoded object.
pub fn value_repr(v: &Value) -> String {
    match v {
        Value::String(s) => py_repr(s),
        Value::Array(a) => format!("[{}]", a.iter().map(value_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter().map(|(k, v)| format!("{}: {}", py_repr(k), value_repr(v))).collect::<Vec<_>>().join(", ")
        ),
        other => value_str(other),
    }
}

/// `str()` of a JSON value as Python would print the decoded object.
pub fn value_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => {
            if n.is_f64() {
                float_str(n.as_f64().unwrap_or(0.0))
            } else {
                n.to_string()
            }
        }
        Value::String(s) => s.clone(),
        other => value_repr(other),
    }
}

/// `str(x or "")`.
pub fn str_or_empty(v: Option<&Value>) -> String {
    match v {
        Some(v) if truthy(v) => value_str(v),
        _ => String::new(),
    }
}

/// `int(x or default)` for the values a worker reply carries (ints, floats, bools, numeric strings).
pub fn int_or(v: Option<&Value>, default: i64) -> i64 {
    match v {
        Some(v) if truthy(v) => match v {
            Value::Bool(b) => i64::from(*b),
            Value::Number(n) => n.as_i64().unwrap_or_else(|| n.as_f64().map(|x| x.trunc() as i64).unwrap_or(default)),
            Value::String(s) => py_int_parse(py_strip(s)).unwrap_or(default),
            _ => default,
        },
        _ => default,
    }
}

/// Python's `int(text)` for a stripped string: optional sign, digits, single underscores between digits.
pub fn py_int_parse(text: &str) -> Option<i64> {
    let t = py_strip(text);
    let (sign, digits) = match t.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, t.strip_prefix('+').unwrap_or(t)),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let clean: String = digits.chars().filter(|c| *c != '_').collect();
    if !clean.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    clean.parse::<i64>().ok().map(|n| sign * n)
}

/// Python's `float(text)`: decimal and exponent forms, `inf`/`infinity`/`nan` in any case, underscores between
/// digits.
pub fn py_float_parse(text: &str) -> Option<f64> {
    let t = py_strip(text);
    if t.is_empty() {
        return None;
    }
    let body = t.trim_start_matches(['+', '-']);
    let lower = body.to_lowercase();
    if t.len() - body.len() > 1 {
        return None;
    }
    if lower == "inf" || lower == "infinity" || lower == "nan" {
        return t.to_lowercase().parse::<f64>().ok().or_else(|| {
            let neg = t.starts_with('-');
            let x = if lower == "nan" { f64::NAN } else { f64::INFINITY };
            Some(if neg { -x } else { x })
        });
    }
    let chars: Vec<char> = t.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if *c == '_' {
            let ok = i > 0 && i + 1 < chars.len() && chars[i - 1].is_ascii_digit() && chars[i + 1].is_ascii_digit();
            if !ok {
                return None;
            }
        }
    }
    let clean: String = chars.into_iter().filter(|c| *c != '_').collect();
    if !clean.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-')) {
        return None;
    }
    clean.parse::<f64>().ok()
}

/// `Path(raw).expanduser()` for `~` and `~/...`.
pub fn expanduser(raw: &str) -> PathBuf {
    let home = || std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from).or_else(dirs_home);
    if raw == "~" {
        if let Some(h) = home() {
            return h;
        }
    } else if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(h) = home() {
            return h.join(rest);
        }
    }
    PathBuf::from(raw)
}

fn dirs_home() -> Option<PathBuf> {
    #[allow(deprecated)]
    std::env::home_dir()
}

/// The first `n` characters.
pub fn take(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// `round(x, n)`.
pub fn round(x: f64, n: usize) -> f64 {
    py_round_n(x, n)
}

/// `urllib.parse.quote_plus`: letters, digits and `_.-~` kept, a space as `+`, every other byte `%XX`.
pub fn quote_plus(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `urllib.parse.urlsplit`'s parts as the agent reads them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SplitUrl {
    pub scheme: String,
    pub netloc: String,
    pub path: String,
    pub query: String,
    pub fragment: String,
}

impl SplitUrl {
    /// `.hostname or ""`: the netloc without userinfo and port, lowercased; an IPv6 literal without brackets.
    pub fn hostname(&self) -> String {
        let after_user = self.netloc.rsplit_once('@').map(|(_, h)| h).unwrap_or(&self.netloc);
        let host = if let Some(rest) = after_user.strip_prefix('[') {
            rest.split_once(']').map(|(h, _)| h).unwrap_or(rest)
        } else {
            after_user.split_once(':').map(|(h, _)| h).unwrap_or(after_user)
        };
        host.to_lowercase()
    }
}

/// `urllib.parse.urlsplit(url)`: a scheme only when it is a valid one (letters first, then letters, digits,
/// `+-.`), a netloc only after `//`, then path, `?query` and `#fragment`. Tabs and newlines are removed, and
/// leading C0 controls and spaces stripped, as Python does.
pub fn urlsplit(url: &str) -> SplitUrl {
    let cleaned: String = url.trim_start_matches(|c: char| c <= ' ').chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let mut rest: &str = &cleaned;
    let mut out = SplitUrl::default();
    if let Some(i) = rest.find(':') {
        let cand = &rest[..i];
        let valid = cand.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && cand.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
        if valid {
            out.scheme = cand.to_lowercase();
            rest = &rest[i + 1..];
        }
    }
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        out.netloc = after[..end].to_string();
        rest = &after[end..];
    }
    if let Some((before, frag)) = rest.split_once('#') {
        out.fragment = frag.to_string();
        rest = before;
    }
    if let Some((before, q)) = rest.split_once('?') {
        out.query = q.to_string();
        rest = before;
    }
    out.path = rest.to_string();
    out
}

/// `json.dumps(value, ensure_ascii=False)`: Python's `", "` and `": "` separators and float repr, non-ASCII
/// written as itself.
pub fn dumps_unicode(value: &Value) -> String {
    struct PyFormatter;
    impl serde_json::ser::Formatter for PyFormatter {
        fn begin_array_value<W: ?Sized + std::io::Write>(&mut self, w: &mut W, first: bool) -> std::io::Result<()> {
            if first {
                Ok(())
            } else {
                w.write_all(b", ")
            }
        }
        fn begin_object_key<W: ?Sized + std::io::Write>(&mut self, w: &mut W, first: bool) -> std::io::Result<()> {
            if first {
                Ok(())
            } else {
                w.write_all(b", ")
            }
        }
        fn begin_object_value<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
            w.write_all(b": ")
        }
        fn write_f64<W: ?Sized + std::io::Write>(&mut self, w: &mut W, value: f64) -> std::io::Result<()> {
            w.write_all(eve_spend::py_float_repr(value).as_bytes())
        }
    }
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, PyFormatter);
    if value.serialize(&mut ser).is_err() {
        return String::new();
    }
    String::from_utf8(out).unwrap_or_default()
}

/// `json.dumps(value)` with Python's defaults (`ensure_ascii`): the worker's request lines.
pub fn dumps(value: &Value) -> String {
    eve_jev::py_dumps(value)
}

/// A float as a JSON number (`round(...)` results).
pub fn num(x: f64) -> Value {
    serde_json::Number::from_f64(x).map(Value::Number).unwrap_or(Value::Null)
}
