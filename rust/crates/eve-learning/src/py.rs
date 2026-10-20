//! The Python behaviors this port reproduces: `json.dumps` bytes (stored lessons, blob hashes and sizes, the
//! dashboard file, HTTP bodies and the tutor request), `str()` / `repr()` of a JSON value, truthiness, number
//! equality, and `str` slicing and whitespace by code point.

use std::fmt::Write as _;

use serde_json::{Map, Value};

pub use eve_envfile::{is_py_space, py_strip};

/// `json.dumps(value, ensure_ascii=...)` with the default `", "` / `": "` separators and `allow_nan=True`.
pub fn dumps(value: &Value, ensure_ascii: bool) -> String {
    let mut out = String::new();
    write_value(&mut out, value, ensure_ascii);
    out
}

fn write_value(out: &mut String, value: &Value, ensure_ascii: bool) {
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
                if f.is_nan() {
                    out.push_str("NaN");
                } else if f.is_infinite() {
                    out.push_str(if f > 0.0 { "Infinity" } else { "-Infinity" });
                } else {
                    out.push_str(&float_repr(f));
                }
            }
        }
        Value::String(s) => write_json_str(out, s, ensure_ascii),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_value(out, item, ensure_ascii);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_json_str(out, k, ensure_ascii);
                out.push_str(": ");
                write_value(out, v, ensure_ascii);
            }
            out.push('}');
        }
    }
}

fn write_json_str(out: &mut String, s: &str, ensure_ascii: bool) {
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
            c if ensure_ascii && (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `repr(float)` for a finite float: the shortest digits that round-trip, positional for exponents -4..16,
/// scientific (`1e-05`, `1.5e+16`) otherwise, and always a `.0` on a whole number.
pub fn float_repr(f: f64) -> String {
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let sci = format!("{:e}", f.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    let n = digits.len() as i32;
    let sign = if f < 0.0 { "-" } else { "" };
    let body = if (-4..16).contains(&exp) {
        if exp >= 0 {
            if n <= exp + 1 {
                format!("{digits}{}.0", "0".repeat((exp + 1 - n) as usize))
            } else {
                format!("{}.{}", &digits[..(exp + 1) as usize], &digits[(exp + 1) as usize..])
            }
        } else {
            format!("0.{}{digits}", "0".repeat((-exp - 1) as usize))
        }
    } else {
        let rest = if n > 1 { format!(".{}", &digits[1..]) } else { String::new() };
        let esign = if exp < 0 { '-' } else { '+' };
        format!("{}{rest}e{esign}{:02}", &digits[..1], exp.abs())
    };
    format!("{sign}{body}")
}

/// `repr(str)`: single quotes unless the text holds a single quote and no double quote.
pub fn repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
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
            c if (c as u32) < 0x20 || (0x7f..=0xa0).contains(&(c as u32)) || c == '\u{ad}' => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c if matches!(c as u32, 0x2028 | 0x2029 | 0xfeff) || (0xd800..=0xdfff).contains(&(c as u32)) => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `repr()` of a JSON value as Python holds it (`True`, `None`, `'text'`, `[1, 2]`, `{'a': 1}`).
pub fn repr(v: &Value) -> String {
    match v {
        Value::String(s) => repr_str(s),
        Value::Array(items) => format!("[{}]", items.iter().map(repr).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter().map(|(k, v)| format!("{}: {}", repr_str(k), repr(v))).collect::<Vec<_>>().join(", ")
        ),
        other => py_str(other),
    }
}

/// `str()` of a JSON value.
pub fn py_str(v: &Value) -> String {
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
                let f = n.as_f64().unwrap_or(f64::NAN);
                if f.is_nan() {
                    "nan".into()
                } else if f.is_infinite() {
                    if f > 0.0 { "inf".into() } else { "-inf".into() }
                } else {
                    float_repr(f)
                }
            }
        }
        Value::String(s) => s.clone(),
        other => repr(other),
    }
}

/// Python truthiness.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

/// `str(x or "")`, the way Python reads an optional text field.
pub fn str_or_empty(v: Option<&Value>) -> String {
    match v {
        Some(v) if truthy(v) => py_str(v),
        _ => String::new(),
    }
}

/// `isinstance(v, int)`: an integer, or a bool (Python's bool is an int).
pub fn as_int(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(*b as i64),
        Value::Number(n) => n.as_i64(),
        _ => None,
    }
}

/// A number or bool as Python compares it.
pub fn as_num(v: &Value) -> Option<f64> {
    match v {
        Value::Bool(b) => Some(*b as i64 as f64),
        Value::Number(n) => n.as_i64().map(|i| i as f64).or_else(|| n.as_f64()),
        _ => None,
    }
}

/// Python `==` between two JSON values: numbers (and bools) by value, everything else structurally.
pub fn py_eq(a: &Value, b: &Value) -> bool {
    match (as_num(a), as_num(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// `s[:n]` by code point.
pub fn take_chars(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

/// `s[-n:]` by code point.
pub fn last_chars(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count <= n {
        return s.to_string();
    }
    s.chars().skip(count - n).collect()
}

/// `len(s)`: code points.
pub fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// `s.rstrip()`.
pub fn py_rstrip(s: &str) -> &str {
    s.trim_end_matches(is_py_space)
}

/// `" ".join(s.split())`.
pub fn split_join(s: &str) -> String {
    s.split(is_py_space).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// `re.sub(r"\s", "", s)`.
pub fn drop_space(s: &str) -> String {
    s.chars().filter(|c| !is_py_space(*c)).collect()
}

/// `s.lower()` (Unicode lowercase).
pub fn py_lower(s: &str) -> String {
    s.to_lowercase()
}

/// A dict field as text, `""` when missing or not a string (`d.get(k, "")` read as a str).
pub fn get_str<'a>(m: &'a Map<String, Value>, key: &str) -> &'a str {
    m.get(key).and_then(Value::as_str).unwrap_or("")
}

/// `{**a, **b}`: a's keys in order with b's values over them, then b's new keys.
pub fn merged(a: &Map<String, Value>, b: &Map<String, Value>) -> Map<String, Value> {
    let mut out = a.clone();
    for (k, v) in b {
        out.insert(k.clone(), v.clone());
    }
    out
}

/// `int(text)` for a header value: surrounding whitespace, an optional sign, digits with single underscores
/// between them. The error is Python's own message.
pub fn parse_int(text: &str) -> Result<i64, String> {
    let bad = || format!("invalid literal for int() with base 10: {}", repr_str(text));
    let t = py_strip(text);
    let (neg, digits) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return Err(bad());
    }
    let clean: String = digits.chars().filter(|c| *c != '_').collect();
    if !clean.chars().all(|c| c.is_ascii_digit()) {
        // Python's int() also reads other Unicode decimal digits; a header never carries them.
        return Err(bad());
    }
    let n: i128 = clean.parse().unwrap_or(i128::MAX);
    let n = if neg { -n } else { n };
    Ok(n.clamp(i64::MIN as i128, i64::MAX as i128) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn float_repr_matches_python() {
        for (f, want) in [
            (1.0, "1.0"),
            (0.5, "0.5"),
            (1e-05, "1e-05"),
            (0.0001, "0.0001"),
            (1e16, "1e+16"),
            (1234567890123456.0, "1234567890123456.0"),
            (1729400000.123456, "1729400000.123456"),
            (-2.5e-7, "-2.5e-07"),
            (1.5e300, "1.5e+300"),
            (0.1, "0.1"),
        ] {
            assert_eq!(float_repr(f), want);
        }
    }

    #[test]
    fn dumps_matches_python() {
        let v = json!({"a": [1, 2.5, true, null], "é": "x\"\n", "f": 3.0});
        assert_eq!(dumps(&v, false), "{\"a\": [1, 2.5, true, null], \"é\": \"x\\\"\\n\", \"f\": 3.0}");
        assert_eq!(dumps(&v, true), "{\"a\": [1, 2.5, true, null], \"\\u00e9\": \"x\\\"\\n\", \"f\": 3.0}");
        assert_eq!(dumps(&json!("😀"), true), "\"\\ud83d\\ude00\"");
    }

    #[test]
    fn repr_and_str_match_python() {
        assert_eq!(repr_str("foo"), "'foo'");
        assert_eq!(repr_str("it's"), "\"it's\"");
        assert_eq!(repr_str("a'b\"c"), "'a\\'b\"c'");
        assert_eq!(py_str(&json!(null)), "None");
        assert_eq!(py_str(&json!([1, "a", true])), "[1, 'a', True]");
        assert_eq!(py_str(&json!({"k": null})), "{'k': None}");
        assert_eq!(py_str(&json!(2.0)), "2.0");
    }

    #[test]
    fn parse_int_matches_python() {
        assert_eq!(parse_int(" 12 "), Ok(12));
        assert_eq!(parse_int("1_000"), Ok(1000));
        assert_eq!(parse_int("-3"), Ok(-3));
        assert_eq!(parse_int("abc"), Err("invalid literal for int() with base 10: 'abc'".to_string()));
        assert!(parse_int("").is_err() && parse_int("1__0").is_err() && parse_int("_1").is_err());
    }
}
