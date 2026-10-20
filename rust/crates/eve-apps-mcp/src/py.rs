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

/// `json.loads(text)` for a `str`, as CPython parses it (eve-ipc's port of its scanner), with Python's
/// exception class and message: `JSONDecodeError: Expecting value: line 1 column 1 (char 0)`.
pub fn json_loads(text: &str) -> Result<Value, CallError> {
    eve_ipc::pyjson::loads(text).map_err(|e| CallError::new(e.python_type(), e.to_string()))
}

/// `json.loads(content)` for `bytes`, which is what httpx's `Response.json()` runs: the encoding detected as
/// `json.detect_encoding` does (UTF-32 or UTF-16 by BOM or by zero bytes, a UTF-8 BOM dropped, else UTF-8),
/// then the text parsed. Bytes that do not decode are Python's `UnicodeDecodeError`.
pub fn json_loads_bytes(content: &[u8]) -> Result<Value, CallError> {
    json_loads(&decode_json_bytes(content)?)
}

fn unicode_error(codec: &str, data: &[u8], start: usize, end: usize, reason: &str) -> CallError {
    let message = if end.saturating_sub(start) <= 1 {
        let byte = data.get(start).copied().unwrap_or(0);
        format!("'{codec}' codec can't decode byte 0x{byte:02x} in position {start}: {reason}")
    } else {
        format!("'{codec}' codec can't decode bytes in position {start}-{}: {reason}", end - 1)
    };
    CallError::new("UnicodeDecodeError", message)
}

fn decode_json_bytes(b: &[u8]) -> Result<String, CallError> {
    const BOM_UTF32_BE: &[u8] = &[0x00, 0x00, 0xFE, 0xFF];
    const BOM_UTF32_LE: &[u8] = &[0xFF, 0xFE, 0x00, 0x00];
    const BOM_UTF16_BE: &[u8] = &[0xFE, 0xFF];
    const BOM_UTF16_LE: &[u8] = &[0xFF, 0xFE];
    const BOM_UTF8: &[u8] = &[0xEF, 0xBB, 0xBF];
    if b.starts_with(BOM_UTF32_BE) {
        return decode_utf32(&b[4..], true, "utf-32", 4);
    }
    if b.starts_with(BOM_UTF32_LE) {
        return decode_utf32(&b[4..], false, "utf-32", 4);
    }
    if b.starts_with(BOM_UTF16_BE) {
        return decode_utf16(&b[2..], true, "utf-16", 2);
    }
    if b.starts_with(BOM_UTF16_LE) {
        return decode_utf16(&b[2..], false, "utf-16", 2);
    }
    if let Some(rest) = b.strip_prefix(BOM_UTF8) {
        return decode_utf8_strict(rest, "utf-8-sig", 3);
    }
    if b.len() >= 4 {
        if b[0] == 0 {
            return if b[1] != 0 {
                decode_utf16(b, true, "utf-16-be", 0)
            } else {
                decode_utf32(b, true, "utf-32-be", 0)
            };
        }
        if b[1] == 0 {
            return if b[2] != 0 || b[3] != 0 {
                decode_utf16(b, false, "utf-16-le", 0)
            } else {
                decode_utf32(b, false, "utf-32-le", 0)
            };
        }
    } else if b.len() == 2 {
        if b[0] == 0 {
            return decode_utf16(b, true, "utf-16-be", 0);
        }
        if b[1] == 0 {
            return decode_utf16(b, false, "utf-16-le", 0);
        }
    }
    decode_utf8_strict(b, "utf-8", 0)
}

/// UTF-8 with Python's error text; `offset` is how many bytes (a BOM) came before `b` in the body.
fn decode_utf8_strict(b: &[u8], codec: &str, offset: usize) -> Result<String, CallError> {
    match std::str::from_utf8(b) {
        Ok(s) => Ok(s.to_string()),
        Err(e) => {
            let start = e.valid_up_to();
            let (end, reason) = match e.error_len() {
                None => (b.len(), "unexpected end of data"),
                Some(n) => {
                    let lead = b[start];
                    let starts = matches!(lead, 0xC2..=0xF4);
                    (start + n, if n == 1 && !starts { "invalid start byte" } else { "invalid continuation byte" })
                }
            };
            let mut whole = vec![0u8; offset];
            whole.extend_from_slice(b);
            Err(unicode_error(codec, &whole, offset + start, offset + end, reason))
        }
    }
}

/// UTF-16 under `surrogatepass`: a lone surrogate cannot live in a Rust string, so it reads as U+FFFD (as
/// eve-ipc's parser reads a lone `\ud800` escape).
fn decode_utf16(b: &[u8], big: bool, codec: &str, offset: usize) -> Result<String, CallError> {
    let whole_units = b.len() / 2 * 2;
    let units = b[..whole_units]
        .chunks_exact(2)
        .map(|c| if big { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) });
    let text: String = char::decode_utf16(units).map(|r| r.unwrap_or('\u{FFFD}')).collect();
    if whole_units != b.len() {
        let mut whole = vec![0u8; offset];
        whole.extend_from_slice(b);
        let at = offset + whole_units;
        return Err(unicode_error(codec, &whole, at, whole.len(), "truncated data"));
    }
    Ok(text)
}

fn decode_utf32(b: &[u8], big: bool, codec: &str, offset: usize) -> Result<String, CallError> {
    let mut whole = vec![0u8; offset];
    whole.extend_from_slice(b);
    let mut out = String::new();
    for (i, c) in b.chunks(4).enumerate() {
        let at = offset + i * 4;
        if c.len() < 4 {
            return Err(unicode_error(codec, &whole, at, whole.len(), "truncated data"));
        }
        let cp = if big {
            u32::from_be_bytes([c[0], c[1], c[2], c[3]])
        } else {
            u32::from_le_bytes([c[0], c[1], c[2], c[3]])
        };
        match char::from_u32(cp) {
            Some(ch) => out.push(ch),
            // a surrogate, passed by surrogatepass
            None if (0xD800..=0xDFFF).contains(&cp) => out.push('\u{FFFD}'),
            None => return Err(unicode_error(codec, &whole, at, at + 4, "code point not in range(0x110000)")),
        }
    }
    Ok(out)
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
    fn json_loads_reads_and_refuses_as_python() {
        // json.loads(b'...') as httpx's Response.json() runs it (texts from CPython 3.13)
        assert_eq!(json_loads_bytes(b"{\"a\": 1}").unwrap(), json!({"a": 1}));
        assert_eq!(json_loads_bytes(b"\xef\xbb\xbf[1]").unwrap(), json!([1]));
        assert_eq!(json_loads_bytes(&[0xFF, 0xFE, b'[', 0, b'2', 0, b']', 0]).unwrap(), json!([2]));
        assert_eq!(json_loads_bytes(&[0, b'[', 0, b'3', 0, b']']).unwrap(), json!([3]));
        let e = json_loads_bytes(b"<html>").unwrap_err();
        assert_eq!((e.kind.as_str(), e.message.as_str()), ("JSONDecodeError", "Expecting value: line 1 column 1 (char 0)"));
        let e = json_loads_bytes(b"{\"a\": 1} x").unwrap_err();
        assert_eq!(e.message, "Extra data: line 1 column 10 (char 9)");
        let e = json_loads_bytes(b"\xff{}").unwrap_err();
        assert_eq!(
            (e.kind.as_str(), e.message.as_str()),
            ("UnicodeDecodeError", "'utf-8' codec can't decode byte 0xff in position 0: invalid start byte")
        );
        let e = json_loads_bytes(b"[\"\xe2\x82\"]").unwrap_err();
        assert_eq!(e.message, "'utf-8' codec can't decode bytes in position 2-3: invalid continuation byte");
        let e = json_loads_bytes(b"[\"\xe2").unwrap_err();
        assert_eq!(e.message, "'utf-8' codec can't decode byte 0xe2 in position 2: unexpected end of data");
        assert_eq!(json_loads("\u{feff}{}").unwrap_err().message, "Unexpected UTF-8 BOM (decode using utf-8-sig): line 1 column 1 (char 0)");
    }

    #[test]
    fn quote_plus_keeps_pythons_safe_set() {
        assert_eq!(quote_plus("a b~c*d/é"), "a+b~c%2Ad%2F%C3%A9");
    }
}
