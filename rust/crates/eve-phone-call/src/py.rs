//! The few Python semantics the call depends on: `str.isspace`/`strip`/`split`, `str.isalpha`, `len` in code
//! points, `round`, and `json.dumps` with its default separators and ASCII escapes (the bytes the phone gets).

use serde_json::Value;
use unicode_general_category::{get_general_category, GeneralCategory};

/// `str.isspace` for one character.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()`.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `str.split()` (no argument): runs of whitespace, no empty pieces.
pub fn split_ws(s: &str) -> Vec<&str> {
    s.split(is_space).filter(|p| !p.is_empty()).collect()
}

/// `str.isalpha` for one character: a letter (Lu, Ll, Lt, Lm, Lo).
pub fn is_alpha(c: char) -> bool {
    matches!(
        get_general_category(c),
        GeneralCategory::UppercaseLetter
            | GeneralCategory::LowercaseLetter
            | GeneralCategory::TitlecaseLetter
            | GeneralCategory::ModifierLetter
            | GeneralCategory::OtherLetter
    )
}

/// `len(s)`: code points, not bytes.
pub fn len(s: &str) -> usize {
    s.chars().count()
}

/// `s[:n]` in code points.
pub fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `s[n:]` in code points.
pub fn tail(s: &str, n: usize) -> String {
    s.chars().skip(n).collect()
}

/// `round(x)` to an int: ties to even.
pub fn round_int(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// `round(x, n)` for a float: Rust's fixed-precision formatting rounds the exact binary value, as CPython does.
pub fn round_to(x: f64, n: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.n$}").parse().unwrap_or(x)
}

/// `str(v or "")`: "" for a falsy value, else `str(v)`.
pub fn str_or_empty(v: Option<&Value>) -> String {
    match v {
        Some(v) if eve_records::py::truthy(v) => eve_records::py::str_of(v),
        _ => String::new(),
    }
}

/// `isinstance(v, int)`: an integer, and a bool (a Python bool is an int).
pub fn is_int(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Number(n)) => n.is_i64() || n.is_u64(),
        Some(Value::Bool(_)) => true,
        _ => false,
    }
}

/// The int a JSON value is as a dict key (`True` is 1), for an [`is_int`] value.
pub fn as_int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_u64().map(|u| u as i64)),
        Value::Bool(b) => Some(*b as i64),
        _ => None,
    }
}

/// `repr(float)`: the shortest round-trip digits, positional from 1e-4 to below 1e16, else with an exponent.
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let sci = format!("{f:e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let (neg, mantissa) = match mantissa.strip_prefix('-') {
        Some(m) => (true, m),
        None => (false, mantissa),
    };
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
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
        let rest = &digits[1..];
        let m = if rest.is_empty() { digits[..1].to_string() } else { format!("{}.{}", &digits[..1], rest) };
        let es = if exp < 0 { "-" } else { "+" };
        format!("{sign}{m}e{es}{:02}", exp.abs())
    }
}

fn push_str(out: &mut String, s: &str) {
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
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn push_value(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.is_f64() {
                out.push_str(&float_repr(n.as_f64().unwrap_or(0.0)));
            } else {
                out.push_str(&n.to_string());
            }
        }
        Value::String(s) => push_str(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                push_value(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                push_str(out, k);
                out.push_str(": ");
                push_value(out, item);
            }
            out.push('}');
        }
    }
}

/// `json.dumps(v)` with the defaults: `", "` and `": "`, ASCII only.
pub fn dumps(v: &Value) -> String {
    let mut out = String::new();
    push_value(&mut out, v);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dumps_matches_python_json_dumps() {
        assert_eq!(dumps(&json!({"type": "state", "state": "notes", "started": 1729.5, "words": 0})),
                   r#"{"type": "state", "state": "notes", "started": 1729.5, "words": 0}"#);
        assert_eq!(dumps(&json!({"text": "caf\u{e9} \u{1f600}\n"})), r#"{"text": "caf\u00e9 \ud83d\ude00\n"}"#);
        assert_eq!(dumps(&json!([1.0, 1e16, 1e-5, 0.0001, 123456789.125, null, true])),
                   "[1.0, 1e+16, 1e-05, 0.0001, 123456789.125, null, true]");
    }

    #[test]
    fn rounding_is_pythons() {
        assert_eq!(round_int(0.5), 0);
        assert_eq!(round_int(1.5), 2);
        assert_eq!(round_int(2.5), 2);
        assert_eq!(round_to(2.675, 2), 2.67);
        assert_eq!(round_to(1760000000.12345, 3), 1760000000.123);
    }

    #[test]
    fn whitespace_and_letters_are_pythons() {
        assert_eq!(split_ws(" a\u{1f}b\u{a0} c "), vec!["a", "b", "c"]);
        assert_eq!(strip("\u{1c} x \n"), "x");
        assert!(is_alpha('あ') && is_alpha('é') && !is_alpha('1') && !is_alpha('。'));
    }
}
