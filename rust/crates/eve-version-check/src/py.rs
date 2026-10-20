//! The few pieces of Python value semantics the version check depends on:
//! `str.strip()`, `str()` / `repr()` of a JSON-loaded value, `float()` of one,
//! `repr(float)`, and `json.dump`'s default (`ensure_ascii=True`) string and
//! float encoding. Kept here so the cache file reads and writes exactly as the
//! Python daemon's does.

use serde_json::Value;

/// `str.isspace()` for one character: Unicode `White_Space` plus the ASCII
/// information separators `\x1c`..`\x1f`, which Python also strips.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()` with no argument.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `repr(float)`: the shortest round-trip digits, positional for exponents in
/// `-4..16`, scientific (`1e+16`, `1e-05`) outside it, and always a `.0` on an
/// integral positional value.
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".to_string() } else { "-inf".to_string() };
    }
    let sci = format!("{f:e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(m) => ("-", m),
        None => ("", mantissa),
    };
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    if digits.bytes().all(|b| b == b'0') {
        return format!("{sign}0.0");
    }
    if (-4..16).contains(&exp) {
        let point = exp + 1;
        let body = if point <= 0 {
            format!("0.{}{digits}", "0".repeat((-point) as usize))
        } else if point as usize >= digits.len() {
            format!("{digits}{}.0", "0".repeat(point as usize - digits.len()))
        } else {
            let (a, b) = digits.split_at(point as usize);
            format!("{a}.{b}")
        };
        format!("{sign}{body}")
    } else {
        let (first, rest) = digits.split_at(1);
        let mant = if rest.is_empty() { first.to_string() } else { format!("{first}.{rest}") };
        let esign = if exp < 0 { '-' } else { '+' };
        format!("{sign}{mant}e{esign}{:02}", exp.abs())
    }
}

/// True when Python's `str.isprintable()` is false for `c` and `repr()` would
/// escape it: control characters, the line and paragraph separators, the
/// space separators other than ' ', and the common format characters.
fn non_printable(c: char) -> bool {
    let u = c as u32;
    u < 0x20
        || (0x7f..=0xa0).contains(&u)
        || u == 0xad
        || u == 0x1680
        || (0x2000..=0x200f).contains(&u)
        || (0x2028..=0x202f).contains(&u)
        || (0x205f..=0x2064).contains(&u)
        || u == 0x3000
        || u == 0xfeff
}

/// `repr(str)`: single quotes unless the text holds a `'` and no `"`.
pub fn str_repr(s: &str) -> String {
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
            c if non_printable(c) => {
                let u = c as u32;
                if u < 0x100 {
                    out.push_str(&format!("\\x{u:02x}"));
                } else if u < 0x10000 {
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

fn number_str(n: &serde_json::Number) -> String {
    if n.is_i64() || n.is_u64() {
        n.to_string()
    } else {
        float_repr(n.as_f64().unwrap_or(f64::NAN))
    }
}

/// `repr()` of a value `json.load` produced.
pub fn repr(v: &Value) -> String {
    match v {
        Value::String(s) => str_repr(s),
        Value::Array(items) => {
            format!("[{}]", items.iter().map(repr).collect::<Vec<_>>().join(", "))
        }
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("{}: {}", str_repr(k), repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => str_of(other),
    }
}

/// `str()` of a value `json.load` produced.
pub fn str_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => number_str(n),
        other => repr(other),
    }
}

/// `float(s)` for a string: surrounding whitespace allowed, single
/// underscores between digits allowed, `nan` / `inf` / `infinity` in any case
/// with an optional sign. `None` where Python raises `ValueError`.
pub fn parse_float(s: &str) -> Option<f64> {
    let s = strip(s);
    if s.is_empty() {
        return None;
    }
    let cleaned: String = if s.contains('_') {
        let chars: Vec<char> = s.chars().collect();
        for (i, c) in chars.iter().enumerate() {
            if *c == '_' {
                let before = i > 0 && chars[i - 1].is_ascii_digit();
                let after = chars.get(i + 1).is_some_and(|n| n.is_ascii_digit());
                if !(before && after) {
                    return None;
                }
            }
        }
        chars.into_iter().filter(|c| *c != '_').collect()
    } else {
        s.to_string()
    };
    let unsigned = cleaned.trim_start_matches(['+', '-']);
    if cleaned.len() - unsigned.len() > 1 {
        return None;
    }
    let lower = unsigned.to_ascii_lowercase();
    if lower == "nan" || lower == "inf" || lower == "infinity" {
        let neg = cleaned.starts_with('-');
        return Some(match (lower.as_str(), neg) {
            ("nan", _) => f64::NAN,
            (_, true) => f64::NEG_INFINITY,
            _ => f64::INFINITY,
        });
    }
    // Python's grammar: digits with an optional '.', an optional exponent.
    // Rust's parser also takes "inf"/"nan" spellings (handled above) and
    // nothing else beyond Python's, but check the characters to be sure.
    if !unsigned.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-')) {
        return None;
    }
    cleaned.parse::<f64>().ok()
}

/// `float(x)` for a value `json.load` produced; `None` where Python raises
/// `TypeError` or `ValueError`.
pub fn float_of(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => parse_float(s),
        _ => None,
    }
}

/// A string as `json.dump` writes it by default (`ensure_ascii=True`):
/// everything outside printable ASCII becomes `\uXXXX`, astral characters a
/// surrogate pair.
pub fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
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
            ' '..='~' => out.push(c),
            c => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
    out
}

/// A float as `json.dump` writes it: `repr(float)`, with `NaN`, `Infinity`
/// and `-Infinity` for the non-finite values (`allow_nan=True`).
pub fn json_float(f: f64) -> String {
    if f.is_nan() {
        "NaN".to_string()
    } else if f == f64::INFINITY {
        "Infinity".to_string()
    } else if f == f64::NEG_INFINITY {
        "-Infinity".to_string()
    } else {
        float_repr(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn float_repr_matches_python() {
        let cases = [
            (1700000000.0, "1700000000.0"),
            (1760890000.123456, "1760890000.123456"),
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.5, "1.5"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1e16, "1e+16"),
            (1.5e16, "1.5e+16"),
            (123456789012345.6, "123456789012345.6"),
            (9999999999999998.0, "9999999999999998.0"),
            (0.1, "0.1"),
            (-2.5e-7, "-2.5e-07"),
            (1e100, "1e+100"),
            (f64::NAN, "nan"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
        ];
        for (f, want) in cases {
            assert_eq!(float_repr(f), want, "{f}");
        }
    }

    #[test]
    fn strip_takes_python_whitespace() {
        assert_eq!(strip("\x1c v0.1.0 \u{3000}\n"), "v0.1.0");
        assert_eq!(strip(""), "");
    }

    #[test]
    fn str_of_matches_python_str() {
        assert_eq!(str_of(&json!("0.1.2")), "0.1.2");
        assert_eq!(str_of(&json!(null)), "None");
        assert_eq!(str_of(&json!(true)), "True");
        assert_eq!(str_of(&json!(12)), "12");
        assert_eq!(str_of(&json!(-3)), "-3");
        assert_eq!(str_of(&json!(1.0)), "1.0");
        assert_eq!(str_of(&json!([1, "a", null])), "[1, 'a', None]");
        assert_eq!(str_of(&json!({"a": "it's"})), "{'a': \"it's\"}");
        assert_eq!(str_of(&json!(["a\nb\u{7}"])), "['a\\nb\\x07']");
    }

    #[test]
    fn parse_float_matches_python_float() {
        assert_eq!(parse_float("1700000000"), Some(1700000000.0));
        assert_eq!(parse_float(" 1.5 "), Some(1.5));
        assert_eq!(parse_float("1_000.5"), Some(1000.5));
        assert_eq!(parse_float("5."), Some(5.0));
        assert_eq!(parse_float(".5"), Some(0.5));
        assert_eq!(parse_float("1e3"), Some(1000.0));
        assert_eq!(parse_float("-Infinity"), Some(f64::NEG_INFINITY));
        assert!(parse_float("NaN").unwrap().is_nan());
        assert_eq!(parse_float("1__0"), None);
        assert_eq!(parse_float("_1"), None);
        assert_eq!(parse_float("abc"), None);
        assert_eq!(parse_float(""), None);
        assert_eq!(parse_float("+-1"), None);
        assert_eq!(parse_float("0x10"), None);
    }

    #[test]
    fn float_of_follows_python_float() {
        assert_eq!(float_of(&json!(1700000000)), Some(1700000000.0));
        assert_eq!(float_of(&json!(true)), Some(1.0));
        assert_eq!(float_of(&json!("2.5")), Some(2.5));
        assert_eq!(float_of(&json!(null)), None);
        assert_eq!(float_of(&json!([1])), None);
    }

    #[test]
    fn json_str_is_ensure_ascii() {
        assert_eq!(json_str("v0.1.2"), "\"v0.1.2\"");
        assert_eq!(json_str("a\"b\\c\n\u{7f}"), "\"a\\\"b\\\\c\\n\\u007f\"");
        assert_eq!(json_str("é😀"), "\"\\u00e9\\ud83d\\ude00\"");
        assert_eq!(json_str("\u{1}\u{8}\u{c}"), "\"\\u0001\\b\\f\"");
    }

    #[test]
    fn json_float_writes_python_tokens() {
        assert_eq!(json_float(1700000000.0), "1700000000.0");
        assert_eq!(json_float(f64::NAN), "NaN");
        assert_eq!(json_float(f64::INFINITY), "Infinity");
        assert_eq!(json_float(f64::NEG_INFINITY), "-Infinity");
    }
}
