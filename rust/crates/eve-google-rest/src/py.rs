//! The few Python semantics `google_rest.py` leans on, so the port answers what the Python answers: truthiness,
//! `str()` / `repr()` of JSON values, `int()`, whitespace, `str.splitlines`, `str.strip` and the lenient
//! base64 decoder (`binascii.a2b_base64` in its default, non-strict mode).

use serde_json::Value;

use crate::Error;

/// `str.isspace()` for one character: Unicode White_Space plus the four ASCII separators (FS, GS, RS, US)
/// that Python counts as whitespace and Rust does not.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `s.strip()` with no argument.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `s.rstrip()` with no argument.
pub fn rstrip(s: &str) -> &str {
    s.trim_end_matches(is_space)
}

/// `s.split()` with no argument: runs of whitespace separate, empty pieces dropped.
pub fn split_ws(s: &str) -> Vec<&str> {
    s.split(is_space).filter(|p| !p.is_empty()).collect()
}

/// `" ".join(s.split())`.
pub fn squash_ws(s: &str) -> String {
    split_ws(s).join(" ")
}

/// `str.splitlines()` (no keepends): \n, \r, \r\n, \v, \f, \x1c, \x1d, \x1e, \x85,  ,   end a line.
pub fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let brk = matches!(
            c,
            '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if !brk {
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

/// The first `n` characters (code points) of `s`: Python's `s[:n]`.
pub fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// The byte offset of character `n` in `s` (the length when `s` is shorter).
pub fn char_to_byte(s: &str, n: usize) -> usize {
    s.char_indices().nth(n).map(|(i, _)| i).unwrap_or(s.len())
}

/// Python's truth value of a JSON-shaped value.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python's type name of a JSON-shaped value, as errors print it.
pub fn type_name(v: &Value) -> &'static str {
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

/// `repr(float)`: the shortest round-trip digits, fixed between 1e-4 and 1e16, else scientific.
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let sci = format!("{x:e}");
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let neg = mant.starts_with('-');
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let sign = if neg { "-" } else { "" };
    if (-4..16).contains(&exp) {
        let point = exp + 1;
        let body = if point <= 0 {
            format!("0.{}{}", "0".repeat((-point) as usize), digits)
        } else if point as usize >= digits.len() {
            format!("{}{}.0", digits, "0".repeat(point as usize - digits.len()))
        } else {
            format!("{}.{}", &digits[..point as usize], &digits[point as usize..])
        };
        format!("{sign}{body}")
    } else {
        let m = if digits.len() > 1 { format!("{}.{}", &digits[..1], &digits[1..]) } else { digits };
        format!("{sign}{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

fn number_str(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        i.to_string()
    } else if let Some(u) = n.as_u64() {
        u.to_string()
    } else {
        float_repr(n.as_f64().unwrap_or(0.0))
    }
}

/// `str(value)` for a JSON-shaped value.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => number_str(n),
        Value::String(s) => s.clone(),
        Value::Array(_) | Value::Object(_) => py_repr(v),
    }
}

/// `repr(value)` for a JSON-shaped value.
pub fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => repr_str(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter().map(|(k, v)| format!("{}: {}", repr_str(k), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
        _ => py_str(v),
    }
}

/// `str.isprintable()` for one character, over the categories Python treats as not printable: controls,
/// format characters, separators other than the ASCII space, private use and non-characters.
fn is_printable(c: char) -> bool {
    let u = c as u32;
    !(u < 0x20
        || (0x7f..=0xa0).contains(&u)
        || u == 0xad
        || (0x600..=0x605).contains(&u)
        || u == 0x61c
        || u == 0x6dd
        || u == 0x70f
        || (0x890..=0x891).contains(&u)
        || u == 0x8e2
        || u == 0x1680
        || u == 0x180e
        || (0x2000..=0x200f).contains(&u)
        || (0x2028..=0x202f).contains(&u)
        || (0x205f..=0x2064).contains(&u)
        || (0x2066..=0x206f).contains(&u)
        || u == 0x3000
        || (0xd800..=0xf8ff).contains(&u)
        || (0xfdd0..=0xfdef).contains(&u)
        || u == 0xfeff
        || (0xfff9..=0xfffb).contains(&u)
        || (u & 0xfffe) == 0xfffe
        || u == 0x110bd
        || u == 0x110cd
        || (0x13430..=0x1343f).contains(&u)
        || (0x1bca0..=0x1bca3).contains(&u)
        || (0x1d173..=0x1d17a).contains(&u)
        || u == 0xe0001
        || (0xe0020..=0xe007f).contains(&u)
        || u >= 0xf0000)
}

/// `repr(str)`: single quotes unless the text holds a single quote and no double quote.
pub fn repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x7f && (c as u32) >= 0x20 => out.push(c),
            c if is_printable(c) => out.push(c),
            c if (c as u32) < 0x100 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) < 0x10000 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push(quote);
    out
}

/// `int(value)`: bools are 0 and 1, floats truncate, strings are read in base 10 (whitespace around, a sign,
/// single underscores between digits). A string that is not a number is a ValueError, as Python's; a list, a
/// dict or None is a TypeError, which `call_tool` does not catch.
pub fn py_int(v: &Value) -> Result<i64, Error> {
    match v {
        Value::Bool(b) => Ok(*b as i64),
        Value::Number(n) => Ok(if let Some(i) = n.as_i64() {
            i
        } else if let Some(u) = n.as_u64() {
            i64::try_from(u).unwrap_or(i64::MAX)
        } else {
            let f = n.as_f64().unwrap_or(0.0);
            if f.is_nan() {
                return Err(Error::value("cannot convert float NaN to integer"));
            }
            f.trunc() as i64
        }),
        Value::String(s) => parse_int(s).ok_or_else(|| {
            Error::value(format!("invalid literal for int() with base 10: {}", repr_str(s)))
        }),
        other => Err(Error::Type(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            type_name(other)
        ))),
    }
}

fn parse_int(s: &str) -> Option<i64> {
    let t = strip(s);
    let (neg, body) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if body.is_empty() || body.starts_with('_') || body.ends_with('_') || body.contains("__") {
        return None;
    }
    let mut n: i64 = 0;
    for c in body.chars() {
        if c == '_' {
            continue;
        }
        let d = c.to_digit(10)? as i64;
        n = n.saturating_mul(10).saturating_add(d);
    }
    Some(if neg { -n } else { n })
}

/// `base64.urlsafe_b64decode(data)` as `_decode` calls it, None where Python raises (non-ASCII text, a
/// dangling character, missing padding). Characters outside the alphabet are skipped, as the non-strict
/// decoder does, and the first complete pad ends the data.
pub fn b64decode_urlsafe(data: &str) -> Option<Vec<u8>> {
    if !data.is_ascii() {
        return None;
    }
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    let (mut quad, mut left, mut pads) = (0u8, 0u32, 0u8);
    for b in data.bytes() {
        if b == b'=' {
            if quad >= 2 {
                pads += 1;
                if quad + pads >= 4 {
                    return Some(out);
                }
            }
            continue;
        }
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => continue,
        } as u32;
        pads = 0;
        match quad {
            0 => {
                quad = 1;
                left = v;
            }
            1 => {
                quad = 2;
                out.push(((left << 2) | (v >> 4)) as u8);
                left = v & 0x0f;
            }
            2 => {
                quad = 3;
                out.push(((left << 4) | (v >> 2)) as u8);
                left = v & 0x03;
            }
            _ => {
                quad = 0;
                out.push(((left << 6) | v) as u8);
                left = 0;
            }
        }
    }
    if quad != 0 {
        return None;
    }
    Some(out)
}

/// Python's `bytes.decode("utf-8", "replace")`.
pub fn decode_utf8(data: &[u8]) -> String {
    String::from_utf8_lossy(data).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn splitlines_matches_python() {
        assert_eq!(splitlines("a\r\nb\rc\nd\u{85}e\u{2028}f\x0bg\x1ch\n"), ["a", "b", "c", "d", "e", "f", "g", "h"]);
        assert_eq!(splitlines("plain text\n\n\nend"), ["plain text", "", "", "end"]);
        assert!(splitlines("").is_empty());
    }

    #[test]
    fn repr_and_str_match_python() {
        assert_eq!(repr_str("Ana"), "'Ana'");
        assert_eq!(repr_str("it's"), "\"it's\"");
        assert_eq!(repr_str("a\nb\\"), "'a\\nb\\\\'");
        assert_eq!(repr_str("Hi\u{202e}\u{a0}é"), "'Hi\\u202e\\xa0é'");
        assert_eq!(py_str(&json!(null)), "None");
        assert_eq!(py_str(&json!([1, "a", true, null])), "[1, 'a', True, None]");
        assert_eq!(py_str(&json!({"k": 1.0})), "{'k': 1.0}");
        assert_eq!(float_repr(1e16), "1e+16");
        assert_eq!(float_repr(1.5e-5), "1.5e-05");
        assert_eq!(float_repr(0.0001), "0.0001");
        assert_eq!(float_repr(123.0), "123.0");
        assert_eq!(float_repr(2.5), "2.5");
    }

    #[test]
    fn int_matches_python() {
        assert_eq!(py_int(&json!(" 12 ")).unwrap(), 12);
        assert_eq!(py_int(&json!("1_000")).unwrap(), 1000);
        assert_eq!(py_int(&json!("-3")).unwrap(), -3);
        assert_eq!(py_int(&json!(7.9)).unwrap(), 7);
        assert_eq!(py_int(&json!(true)).unwrap(), 1);
        assert_eq!(py_int(&json!("1__0")).unwrap_err().to_string(), "invalid literal for int() with base 10: '1__0'");
        assert_eq!(py_int(&json!("abc")).unwrap_err().to_string(), "invalid literal for int() with base 10: 'abc'");
        let e = py_int(&json!([1])).unwrap_err();
        assert_eq!(e.type_name(), "TypeError");
        assert_eq!(e.to_string(), "int() argument must be a string, a bytes-like object or a real number, not 'list'");
    }

    #[test]
    fn base64_is_as_lenient_as_pythons() {
        assert_eq!(b64decode_urlsafe("aGVsbG8").unwrap_or_default(), b"");
        assert_eq!(b64decode_urlsafe("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(b64decode_urlsafe("aGV$sbG8=").unwrap(), b"hello");
        assert_eq!(b64decode_urlsafe("aGVsbG8=trailing").unwrap(), b"hello");
        assert!(b64decode_urlsafe("!!!not base64***").is_none());
        assert!(b64decode_urlsafe("é").is_none());
        assert_eq!(b64decode_urlsafe("-_-_").unwrap(), vec![0xfb, 0xff, 0xbf]);
    }
}
