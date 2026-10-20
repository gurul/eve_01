//! The few Python behaviors spend_sync leans on: `json.loads` / `json.dumps(sort_keys=True)`, `round`, `str()`,
//! truthiness, `urlencode`, `float(str)`, `sum` over floats, and `datetime.fromtimestamp`'s range errors.

use chrono::{Local, NaiveDate, TimeZone};
use serde_json::Value;

use crate::PyError;

/// Python's truthiness of a JSON value (a missing value is `None`, falsy).
pub(crate) fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(m)) => !m.is_empty(),
    }
}

/// `for item in (value or [])`: a list's items. A dict or a string iterates keys or characters, which are never
/// dicts, so every caller skips them; a truthy number or `True` is not iterable (`TypeError`).
pub(crate) fn iter_items(value: Option<&Value>) -> Result<Vec<&Value>, PyError> {
    if !truthy(value) {
        return Ok(Vec::new());
    }
    match value {
        Some(Value::Array(items)) => Ok(items.iter().collect()),
        Some(Value::Object(_)) | Some(Value::String(_)) => Ok(Vec::new()),
        _ => Err(PyError::new("TypeError")),
    }
}

/// Python's `round(x, n)` for a float (correctly rounded, ties to even on the exact value).
pub fn py_round(x: f64, digits: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.digits$}").parse().unwrap_or(x)
}

/// Python's `repr()` of a str.
fn py_str_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || (0x7f..0xa0).contains(&(c as u32)) => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `repr()` of a JSON value (a list or dict inside `str()`).
fn py_repr(value: &Value) -> String {
    match value {
        Value::String(s) => py_str_repr(s),
        Value::Array(items) => format!("[{}]", items.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter().map(|(k, v)| format!("{}: {}", py_str_repr(k), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
        other => py_str_of(other),
    }
}

/// Python's `str()` of a JSON value.
fn py_str_of(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => {
            if n.is_f64() {
                eve_spend::py_float_repr(n.as_f64().unwrap_or(0.0))
            } else {
                n.to_string()
            }
        }
        Value::String(s) => s.clone(),
        Value::Array(_) | Value::Object(_) => py_repr(value),
    }
}

/// `str(value or default)`.
pub(crate) fn py_str(value: Option<&Value>, default: &str) -> String {
    match value {
        Some(v) if truthy(Some(v)) => py_str_of(v),
        _ => default.to_string(),
    }
}

/// `urllib.parse.quote_plus(s, safe="")`.
fn quote_plus(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `urllib.parse.urlencode(params)`: `str()` of each value, `quote_plus`-encoded, in order.
pub fn urlencode(params: &[(String, Value)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", quote_plus(k), quote_plus(&py_str_of(v))))
        .collect::<Vec<_>>()
        .join("&")
}

/// Python's `float(str)`: surrounding whitespace allowed, `_` only between digits, `inf`/`nan` spelled out.
pub(crate) fn py_float_from_str(s: &str) -> Option<f64> {
    let t = s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c));
    let chars: Vec<char> = t.chars().collect();
    let mut clean = String::with_capacity(t.len());
    for (i, c) in chars.iter().enumerate() {
        if *c == '_' {
            let prev = i.checked_sub(1).and_then(|j| chars.get(j)).is_some_and(|p| p.is_ascii_digit());
            let next = chars.get(i + 1).is_some_and(|n| n.is_ascii_digit());
            if !(prev && next) {
                return None;
            }
            continue;
        }
        clean.push(*c);
    }
    let lower = clean.to_ascii_lowercase();
    let body = lower.trim_start_matches(['+', '-']);
    if lower.len() - body.len() > 1 {
        return None;
    }
    match body {
        "inf" | "infinity" | "nan" => {}
        _ if !body.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | '+' | '-')) => return None,
        _ => {}
    }
    clean.parse().ok()
}

/// `sum()` of floats as CPython 3.12+ adds them (Neumaier's compensated summation).
#[derive(Default)]
pub(crate) struct Neumaier {
    sum: f64,
    c: f64,
    pub(crate) count: usize,
}

impl Neumaier {
    pub(crate) fn add(&mut self, x: f64) {
        let t = self.sum + x;
        if self.sum.abs() >= x.abs() {
            self.c += (self.sum - t) + x;
        } else {
            self.c += (x - t) + self.sum;
        }
        self.sum = t;
        self.count += 1;
    }

    pub(crate) fn total(&self) -> f64 {
        // don't let the compensation turn an infinite or overflowed sum into NaN
        if self.c != 0.0 && self.c.is_finite() {
            self.sum + self.c
        } else {
            self.sum
        }
    }
}

/// The civil year of a day count since 1970-01-01 (Howard Hinnant's algorithm).
fn year_of_days(days: i64) -> i64 {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    yoe + era * 400 + i64::from(m <= 2)
}

/// `datetime.fromtimestamp(ts)`'s split into seconds and microseconds (ties to even), or the exception it raises:
/// `ValueError` for NaN and for a year outside 1..9999, `OverflowError` past `time_t`, `OSError` where the C
/// library cannot hold the year.
fn split_timestamp(ts: f64) -> Result<(i64, u32), PyError> {
    if ts.is_nan() {
        return Err(PyError::new("ValueError"));
    }
    // time_t is 64 bits: the whole seconds must lie in [-2**63, 2**63)
    const TIME_T: f64 = 9_223_372_036_854_775_808.0;
    if !ts.is_finite() || !(-TIME_T..TIME_T).contains(&ts.floor()) {
        return Err(PyError::new("OverflowError"));
    }
    let mut secs = ts.floor();
    let mut us = ((ts - secs) * 1e6).round_ties_even();
    if us >= 1e6 {
        secs += 1.0;
        us -= 1e6;
    }
    let secs = secs as i64;
    let year = year_of_days(secs.div_euclid(86_400));
    if year - 1900 > i64::from(i32::MAX) || year - 1900 < i64::from(i32::MIN) {
        return Err(PyError::new("OSError"));
    }
    if !(1..=9999).contains(&year) {
        return Err(PyError::new("ValueError"));
    }
    Ok((secs, us as u32))
}

/// `datetime.fromtimestamp(ts, timezone.utc).date()`.
pub(crate) fn utc_date(ts: f64) -> Result<NaiveDate, PyError> {
    let (secs, us) = split_timestamp(ts)?;
    chrono::DateTime::from_timestamp(secs, us * 1000)
        .map(|d| d.date_naive())
        .ok_or_else(|| PyError::new("ValueError"))
}

/// `datetime.fromtimestamp(ts).date()`, in the local zone.
pub(crate) fn local_date(ts: f64) -> Result<NaiveDate, PyError> {
    let (secs, us) = split_timestamp(ts)?;
    let date = Local
        .timestamp_opt(secs, us * 1000)
        .earliest()
        .map(|d| d.date_naive())
        .ok_or_else(|| PyError::new("ValueError"))?;
    if !(1..=9999).contains(&chrono::Datelike::year(&date)) {
        return Err(PyError::new("ValueError"));
    }
    Ok(date)
}

/// The Python exception name an OS error raises, for the one log line a failed save leaves.
pub(crate) fn os_error_name(e: &std::io::Error) -> &'static str {
    match e.raw_os_error() {
        Some(libc::ENOENT) => "FileNotFoundError",
        Some(libc::EEXIST) => "FileExistsError",
        Some(libc::ENOTDIR) => "NotADirectoryError",
        Some(libc::EISDIR) => "IsADirectoryError",
        Some(libc::EACCES) | Some(libc::EPERM) => "PermissionError",
        _ => "OSError",
    }
}

/// Python's `json.loads` for an object or array: the `NaN` / `Infinity` / `-Infinity` literals it accepts are
/// read as `null`, since a JSON value cannot hold them. `None` when the text is not JSON.
pub fn py_loads(text: &str) -> Option<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        return Some(v);
    }
    let mut out = String::with_capacity(text.len());
    let (mut in_str, mut escaped) = (false, false);
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if in_str {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            rest = &rest[c.len_utf8()..];
            continue;
        }
        if c == '"' {
            in_str = true;
        }
        let token = ["-Infinity", "Infinity", "NaN"].into_iter().find(|t| rest.starts_with(t));
        if let Some(token) = token {
            out.push_str("null");
            rest = &rest[token.len()..];
            continue;
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    serde_json::from_str(&out).ok()
}

/// A JSON string as Python's `json.dumps` writes it (`ensure_ascii`).
fn dump_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            _ => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
}

fn dump(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.is_f64() {
                out.push_str(&eve_spend::py_float_repr(n.as_f64().unwrap_or(0.0)));
            } else {
                out.push_str(&n.to_string());
            }
        }
        Value::String(s) => dump_str(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump(item, out);
            }
            out.push(']');
        }
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_str(k, out);
                out.push_str(": ");
                dump(&m[k], out);
            }
            out.push('}');
        }
    }
}

/// `json.dumps(value, sort_keys=True)`: Python's default separators, ASCII escapes and float repr.
pub fn py_dumps_sorted(value: &Value) -> String {
    let mut out = String::new();
    dump(value, &mut out);
    out
}
