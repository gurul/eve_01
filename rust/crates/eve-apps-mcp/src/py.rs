//! The Python behaviors these two modules lean on beyond what `eve_google_rest::py` already provides:
//! `json.dumps` (flat with `", "`/`": "`, and `indent=2` as `json.dump` writes the token files), `float()` of a
//! value, `os.path.expanduser`, `quote_plus`, and `==` between a reply's JSON-RPC id and the int sent.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use eve_google_rest::py::{float_repr, py_repr, strip, type_name};
use serde_json::Value;

use crate::apps_mcp::CallError;

/// `json.dumps(value, ensure_ascii=…)`: the default `", "` / `": "` separators, keys in insertion order.
pub fn dumps(value: &Value, ensure_ascii: bool) -> String {
    let mut out = String::new();
    write_value(&mut out, value, ensure_ascii, None, 0);
    out
}

/// `json.dumps(value, indent=2)` (ensure_ascii on, as `json.dump` defaults): `","` at line ends and `": "`.
pub fn dumps_indent2(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value, true, Some(2), 0);
    out
}

fn newline(out: &mut String, indent: usize, depth: usize) {
    out.push('\n');
    for _ in 0..indent * depth {
        out.push(' ');
    }
}

fn write_value(
    out: &mut String,
    value: &Value,
    ensure_ascii: bool,
    indent: Option<usize>,
    depth: usize,
) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                let _ = write!(out, "{i}");
            } else if let Some(u) = n.as_u64() {
                let _ = write!(out, "{u}");
            } else {
                let f = n.as_f64().unwrap_or(f64::NAN);
                // json.dumps writes the non-finite floats by these names
                out.push_str(&if f.is_nan() {
                    "NaN".to_string()
                } else if f.is_infinite() {
                    if f > 0.0 {
                        "Infinity".to_string()
                    } else {
                        "-Infinity".to_string()
                    }
                } else {
                    float_repr(f)
                });
            }
        }
        Value::String(s) => write_str(out, s, ensure_ascii),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(if indent.is_some() { "," } else { ", " });
                }
                if let Some(n) = indent {
                    newline(out, n, depth + 1);
                }
                write_value(out, item, ensure_ascii, indent, depth + 1);
            }
            if let Some(n) = indent {
                newline(out, n, depth);
            }
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(if indent.is_some() { "," } else { ", " });
                }
                if let Some(n) = indent {
                    newline(out, n, depth + 1);
                }
                write_str(out, k, ensure_ascii);
                out.push_str(": ");
                write_value(out, v, ensure_ascii, indent, depth + 1);
            }
            if let Some(n) = indent {
                newline(out, n, depth);
            }
            out.push('}');
        }
    }
}

fn write_str(out: &mut String, s: &str, ensure_ascii: bool) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            // ensure_ascii escapes everything outside space..tilde, DEL included
            c if ensure_ascii && (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    let _ = write!(out, "\\u{:04x}", unit);
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `float(text)` for a string: Python's literal forms (surrounding whitespace, `_` between digits, `inf`,
/// `infinity`, `nan` in any case, with a sign). None where Python raises ValueError.
pub fn parse_float(text: &str) -> Option<f64> {
    let t = strip(text);
    if t.is_empty() {
        return None;
    }
    let (sign, body) = match t.as_bytes()[0] {
        b'+' => (1.0, &t[1..]),
        b'-' => (-1.0, &t[1..]),
        _ => (1.0, t),
    };
    let lower = body.to_ascii_lowercase();
    if lower == "inf" || lower == "infinity" {
        return Some(sign * f64::INFINITY);
    }
    if lower == "nan" {
        return Some(f64::NAN);
    }
    // underscores only between two digits
    let chars: Vec<char> = body.chars().collect();
    let mut clean = String::with_capacity(body.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            let before = i > 0 && chars[i - 1].is_ascii_digit();
            let after = chars.get(i + 1).is_some_and(|n| n.is_ascii_digit());
            if !(before && after) {
                return None;
            }
            continue;
        }
        if !(c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-')) {
            return None;
        }
        clean.push(c);
    }
    if clean.is_empty() || clean.starts_with(['+', '-']) {
        return None;
    }
    clean.parse::<f64>().ok().map(|f| sign * f)
}

/// `float(value)` for a JSON value, with Python's ValueError / TypeError.
pub fn py_float(v: &Value) -> Result<f64, CallError> {
    match v {
        Value::Number(n) => Ok(n.as_f64().unwrap_or(f64::NAN)),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => parse_float(s).ok_or_else(|| {
            CallError::new(
                "ValueError",
                format!("could not convert string to float: {}", py_repr(v)),
            )
        }),
        other => Err(CallError::new(
            "TypeError",
            format!(
                "float() argument must be a string or a real number, not '{}'",
                type_name(other)
            ),
        )),
    }
}

/// `os.path.expanduser`: a leading `~` (alone or before a slash) is the home directory ($HOME first).
pub fn expanduser(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if text != "~" && !text.starts_with("~/") {
        return path.to_path_buf();
    }
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(dirs::home_dir);
    let Some(home) = home else {
        return path.to_path_buf();
    };
    let rest = text[1..].trim_start_matches('/');
    if rest.is_empty() {
        home
    } else {
        home.join(rest)
    }
}

/// `urllib.parse.quote_plus` with its default safe set: letters, digits and `_.-~` kept, a space as `+`.
pub fn quote_plus(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// `urlencode(pairs)`: each key and value through `quote_plus`, joined by `&`.
pub fn urlencode(pairs: &[(&str, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", quote_plus(k), quote_plus(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// A value as httpx writes it into a form or query: true/false lowercase, None empty, anything else `str()`.
pub fn form_value(v: &Value) -> String {
    match v {
        Value::Bool(true) => "true".into(),
        Value::Bool(false) => "false".into(),
        Value::Null => String::new(),
        other => eve_google_rest::py::py_str(other),
    }
}

/// `value == id` in Python for the int id a request carried: a float equal to it and a bool (an int in
/// Python) match too.
pub fn id_matches(value: Option<&Value>, id: i64) -> bool {
    match value {
        Some(Value::Number(n)) => match n.as_i64() {
            Some(i) => i == id,
            None => n.as_f64() == Some(id as f64),
        },
        Some(Value::Bool(b)) => i64::from(*b) == id,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dumps_matches_python() {
        let v = json!({"a": [1, 2.5, "é"], "b": {}, "c": [], "d": null, "e": 1e16});
        assert_eq!(
            dumps(&v, false),
            r#"{"a": [1, 2.5, "é"], "b": {}, "c": [], "d": null, "e": 1e+16}"#
        );
        assert_eq!(
            dumps(&v, true),
            r#"{"a": [1, 2.5, "\u00e9"], "b": {}, "c": [], "d": null, "e": 1e+16}"#
        );
        assert_eq!(
            dumps_indent2(&json!({"fetched": 1000.0, "repos": [{"name": "x", "description": "😀"}], "e": []})),
            "{\n  \"fetched\": 1000.0,\n  \"repos\": [\n    {\n      \"name\": \"x\",\n      \"description\": \
             \"\\ud83d\\ude00\"\n    }\n  ],\n  \"e\": []\n}"
        );
    }

    #[test]
    fn float_parses_as_python() {
        assert_eq!(parse_float(" 3 "), Some(3.0));
        assert_eq!(parse_float("-3"), Some(-3.0));
        assert_eq!(parse_float("1_000.5"), Some(1000.5));
        assert_eq!(parse_float("1__0"), None);
        assert_eq!(parse_float("Infinity"), Some(f64::INFINITY));
        assert!(parse_float("nan").unwrap().is_nan());
        assert_eq!(parse_float("abc"), None);
        assert_eq!(parse_float(""), None);
        assert_eq!(parse_float("--1"), None);
        assert_eq!(parse_float("1e3"), Some(1000.0));
        assert_eq!(parse_float(".5"), Some(0.5));
    }

    #[test]
    fn ids_compare_as_python() {
        assert!(id_matches(Some(&json!(1)), 1));
        assert!(id_matches(Some(&json!(1.0)), 1));
        assert!(id_matches(Some(&json!(true)), 1));
        assert!(!id_matches(Some(&json!("1")), 1));
        assert!(!id_matches(None, 1));
    }

    #[test]
    fn quote_plus_keeps_pythons_safe_set() {
        assert_eq!(quote_plus("a b~c*d/é"), "a+b~c%2Ad%2F%C3%A9");
    }
}
