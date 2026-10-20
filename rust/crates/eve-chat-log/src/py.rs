//! The Python builtins chat_log.py leans on: `float()`, `int()`, `pathlib`'s pure-path spelling, and the compact
//! `json.dumps(ensure_ascii=False, separators=(",", ":"))` the log file is written with.

use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use eve_transcripts::py::{float_repr, strip};

static ND: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\p{Nd}$").unwrap());

/// The value of a Unicode decimal digit (`unicodedata.decimal`): Nd digits come in runs of ten from zero.
fn digit_value(c: char) -> Option<u32> {
    if let Some(v) = c.to_digit(10) {
        return Some(v);
    }
    let is_digit = |ch: char| ND.is_match(ch.encode_utf8(&mut [0u8; 4]));
    if !is_digit(c) {
        return None;
    }
    let mut back = 0u32;
    let mut code = c as u32;
    while code > 0 {
        code -= 1;
        match char::from_u32(code) {
            Some(prev) if is_digit(prev) => back += 1,
            _ => break,
        }
    }
    Some(back % 10)
}

/// Non-ASCII decimal digits as ASCII ones, and `_` dropped where it stands between two digits; None for an
/// underscore anywhere else (Python's numeric literal rule for `int()` and `float()`).
fn ascii_digits(raw: &str) -> Option<String> {
    let chars: Vec<char> = raw
        .chars()
        .map(|c| match digit_value(c) {
            Some(d) if !c.is_ascii_digit() => char::from_digit(d, 10).unwrap_or(c),
            _ => c,
        })
        .collect();
    let mut out = String::with_capacity(chars.len());
    for (i, c) in chars.iter().enumerate() {
        if *c == '_' {
            let before = i > 0 && chars[i - 1].is_ascii_digit();
            let after = chars.get(i + 1).is_some_and(|c| c.is_ascii_digit());
            if !(before && after) {
                return None;
            }
        } else {
            out.push(*c);
        }
    }
    Some(out)
}

/// Python's `float(str)`: surrounding whitespace ignored, an optional sign, `inf`/`infinity`/`nan` in any case,
/// decimal literals with digit-separating underscores and any Unicode decimal digit. None where Python raises.
pub fn float_str(raw: &str) -> Option<f64> {
    let s = ascii_digits(strip(raw))?;
    if s.is_empty() {
        return None;
    }
    // Rust's grammar is Python's here, except that it also takes a leading '+' after the sign ("+-1"): never valid
    let body = s.strip_prefix(['+', '-']).unwrap_or(&s);
    if body.starts_with(['+', '-']) {
        return None;
    }
    s.parse::<f64>().ok()
}

/// Python's `int(str)` in base 10: surrounding whitespace ignored, an optional sign, digits (any Unicode decimal
/// digit) with single underscores between them. None where Python raises, and for a value past i64.
pub fn int_str(raw: &str) -> Option<i64> {
    let s = ascii_digits(strip(raw))?;
    let (neg, digits) = match s.as_bytes().first() {
        Some(b'+') => (false, &s[1..]),
        Some(b'-') => (true, &s[1..]),
        _ => (false, s.as_str()),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let v: i128 = digits.parse().ok()?;
    i64::try_from(if neg { -v } else { v }).ok()
}

/// Python's `int(v)` of a decoded JSON value: an int is itself, a float is cut toward zero, a bool is 0 or 1, a
/// string is parsed. None where Python raises (`None`, a list, a dict, NaN, an unreadable string) and for a value
/// past i64 (Python's ints have no bound; see PARITY.md).
pub fn int_value(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(i)
            } else {
                let f = n.as_f64()?;
                let cut = f.trunc();
                if f.is_finite() && cut >= i64::MIN as f64 && cut < i64::MAX as f64 {
                    Some(cut as i64)
                } else {
                    None
                }
            }
        }
        Value::String(s) => int_str(s),
        _ => None,
    }
}

/// Python's `float(v)` of a decoded JSON value. None where Python raises.
pub fn float_value(v: &Value) -> Option<f64> {
    match v {
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        Value::String(s) => float_str(s),
        _ => None,
    }
}

/// `pathlib.PurePosixPath(p)`: repeated slashes and `.` parts gone, no trailing slash, "" spelled ".".
pub fn pure(p: &Path) -> PathBuf {
    let parts: PathBuf = p.components().filter(|c| !matches!(c, Component::CurDir)).collect();
    if parts.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        parts
    }
}

/// `Path.parent`: "." for a bare name.
pub fn parent(p: &Path) -> PathBuf {
    match p.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        Some(_) => PathBuf::from("."),
        None => p.to_path_buf(),
    }
}

/// `json.dumps(s, ensure_ascii=False)` of one string.
pub fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `json.dumps(f)`: `repr(float)`, with `NaN` and `Infinity` as Python writes them.
pub fn write_float(out: &mut String, f: f64) {
    out.push_str(&float_repr(f));
}

/// `json.dumps(v, ensure_ascii=False, separators=(",", ":"))` of a decoded JSON value.
pub fn write_value(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => {
            if n.is_f64() {
                write_float(out, n.as_f64().unwrap_or(0.0));
            } else {
                out.push_str(&n.to_string());
            }
        }
        Value::String(s) => write_str(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_str(out, k);
                out.push(':');
                write_value(out, item);
            }
            out.push('}');
        }
    }
}
