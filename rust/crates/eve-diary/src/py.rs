//! The few Python built-ins the diary leans on, with Python's semantics:
//! `str()` and `repr()` of a decoded JSON value, `float()`, `round()`,
//! `str.strip()` / `split()` / `splitlines()`, `min` / `max`, iteration of
//! `x or []`, and slicing by code point.

use serde_json::{Map, Value};

/// The Python exception a built-in would raise. Callers catch some kinds (as
/// the Python catches `TypeError` / `ValueError`) and let the rest escape.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PyError {
    #[error("{0}")]
    TypeError(String),
    #[error("{0}")]
    ValueError(String),
    #[error("{0}")]
    OverflowError(String),
}

impl PyError {
    /// The class name, as `type(e).__name__` prints it.
    pub fn name(&self) -> &'static str {
        match self {
            PyError::TypeError(_) => "TypeError",
            PyError::ValueError(_) => "ValueError",
            PyError::OverflowError(_) => "OverflowError",
        }
    }
}

/// Python's `str.isspace` set: Unicode whitespace plus the four ASCII separators `\x1c`-`\x1f`.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `str.strip()`.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `" ".join(s.split())`.
pub fn collapse(s: &str) -> String {
    s.split(is_space).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// `s[:n]` by code point.
pub fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `str.lower()`.
pub fn lower(s: &str) -> String {
    s.to_lowercase()
}

/// `str.splitlines()`: the line boundaries Python's str recognises.
pub fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let is_break = matches!(
            c,
            '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if !is_break {
            continue;
        }
        out.push(&s[start..i]);
        let mut end = i + c.len_utf8();
        if c == '\r' {
            if let Some(&(j, '\n')) = chars.peek() {
                chars.next();
                end = j + 1;
            }
        }
        start = end;
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Python's `min(a, b)` on floats: `b` only when `b < a` (so a NaN second argument loses).
pub fn min(a: f64, b: f64) -> f64 {
    if b < a {
        b
    } else {
        a
    }
}

/// Python's `max(a, b)` on floats: `b` only when `b > a`.
pub fn max(a: f64, b: f64) -> f64 {
    if b > a {
        b
    } else {
        a
    }
}

/// `round(x, n)` for a float: correctly rounded, ties to even on the exact binary value.
pub fn round_to(x: f64, ndigits: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.ndigits$}").parse().unwrap_or(x)
}

/// `int(round(x))`: ties to even; NaN is a ValueError, infinity an OverflowError.
pub fn round_int(x: f64) -> Result<f64, PyError> {
    if x.is_nan() {
        return Err(PyError::ValueError("cannot convert float NaN to integer".into()));
    }
    if x.is_infinite() {
        return Err(PyError::OverflowError("cannot convert float infinity to integer".into()));
    }
    Ok(x.round_ties_even())
}

/// Python's truthiness of a decoded JSON value.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn type_name(v: &Value) -> &'static str {
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

/// `for x in (v or [])`: a list's items, a string's characters, a dict's keys;
/// nothing for a falsy value; a TypeError for a number or `True`.
pub fn iter_or_empty(v: Option<&Value>) -> Result<Vec<Value>, PyError> {
    let Some(v) = v.filter(|v| truthy(v)) else {
        return Ok(vec![]);
    };
    match v {
        Value::Array(a) => Ok(a.clone()),
        Value::String(s) => Ok(s.chars().map(|c| Value::String(c.to_string())).collect()),
        Value::Object(o) => Ok(o.keys().map(|k| Value::String(k.clone())).collect()),
        other => Err(PyError::TypeError(format!("'{}' object is not iterable", type_name(other)))),
    }
}

/// `float(s)` for a str: Rust's parser plus the underscores between digits Python allows.
pub fn parse_float(raw: &str) -> Option<f64> {
    let s = strip(raw);
    if s.is_empty() {
        return None;
    }
    // Rust's parser takes `inf`, `infinity` and `nan` in any case with a sign, as Python does.
    if s.contains('_') {
        let b = s.as_bytes();
        for (i, &c) in b.iter().enumerate() {
            if c == b'_' {
                let ok = i > 0 && i + 1 < b.len() && b[i - 1].is_ascii_digit() && b[i + 1].is_ascii_digit();
                if !ok {
                    return None;
                }
            }
        }
        return s.replace('_', "").parse::<f64>().ok();
    }
    s.parse::<f64>().ok()
}

/// `float(v)` for a decoded JSON value.
pub fn float(v: &Value) -> Result<f64, PyError> {
    match v {
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => Ok(n.as_f64().unwrap_or(0.0)),
        Value::String(s) => parse_float(s)
            .ok_or_else(|| PyError::ValueError(format!("could not convert string to float: {}", repr_str(s)))),
        other => Err(PyError::TypeError(format!(
            "float() argument must be a string or a real number, not '{}'",
            type_name(other)
        ))),
    }
}

/// `repr(x)` / `str(x)` of a float.
pub fn float_str(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    eve_spend::py_float_repr(x)
}

/// `str(v)` of a decoded JSON value.
pub fn str_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => repr(other),
    }
}

/// `repr(v)` of a decoded JSON value.
pub fn repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                float_str(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => repr_str(s),
        Value::Array(a) => format!("[{}]", a.iter().map(repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => repr_map(o),
    }
}

fn repr_map(o: &Map<String, Value>) -> String {
    let items: Vec<String> = o.iter().map(|(k, v)| format!("{}: {}", repr_str(k), repr(v))).collect();
    format!("{{{}}}", items.join(", "))
}

/// Characters `str.isprintable()` rejects, as far as a log line can tell.
fn non_printable(c: char) -> bool {
    let u = c as u32;
    u < 0x20
        || (0x7f..=0xa0).contains(&u)
        || u == 0xad
        || u == 0x1680
        || u == 0x180e
        || (0x2000..=0x200f).contains(&u)
        || (0x2028..=0x202f).contains(&u)
        || (0x205f..=0x206f).contains(&u)
        || u == 0x3000
        || (0xd800..=0xdfff).contains(&u)
        || (0xe000..=0xf8ff).contains(&u)
        || u == 0xfeff
        || (0xfff9..=0xfffb).contains(&u)
        || u >= 0xf0000
}

/// `repr(s)` of a str.
pub fn repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if non_printable(c) => {
                let u = c as u32;
                if u <= 0xff {
                    out.push_str(&format!("\\x{u:02x}"));
                } else if u <= 0xffff {
                    out.push_str(&format!("\\u{u:04x}"));
                } else {
                    out.push_str(&format!("\\U{u:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `json.dumps(s)` of a str with `ensure_ascii=False` (serde_json escapes the same set).
pub fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// `json.dumps(s)` of a str with the default `ensure_ascii=True`.
pub fn json_str_ascii(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for ch in json_str(s).chars() {
        if ch.is_ascii() {
            out.push(ch);
        } else {
            let mut buf = [0u16; 2];
            for unit in ch.encode_utf16(&mut buf) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}
