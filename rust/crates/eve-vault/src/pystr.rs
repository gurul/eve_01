//! The few Python string and value semantics the vault's behavior depends on: `str.isspace`, `str.split()`,
//! `str.strip()`, `repr()` of a string, `str()` and truthiness of a JSON value, `int()` of a string, and
//! `json.dumps`' default `ensure_ascii` escaping. Each one decides an owner-visible message or what a file
//! holds, so the port keeps Python's answer rather than Rust's nearest equivalent.

use serde_json::Value;

/// Python's `str.isspace()` for one character: Unicode White_Space plus the four ASCII separators
/// (U+001C..U+001F) that Python also counts.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `s.split()` with no argument.
pub fn split(s: &str) -> Vec<&str> {
    s.split(is_space).filter(|w| !w.is_empty()).collect()
}

/// `s.strip()` with no argument.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `" ".join(s.split())`.
pub fn squash(s: &str) -> String {
    split(s).join(" ")
}

/// The first `n` characters (`s[:n]`).
pub fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `str.isprintable()` for one character: everything but the "Other" and "Separator" categories (Cc, Cf,
/// Cs, Co, Cn, Zl, Zp, Zs), except the ASCII space.
fn is_printable(c: char) -> bool {
    use unicode_general_category::{get_general_category, GeneralCategory as G};
    if c == ' ' {
        return true;
    }
    !matches!(
        get_general_category(c),
        G::Control
            | G::Format
            | G::Surrogate
            | G::PrivateUse
            | G::Unassigned
            | G::LineSeparator
            | G::ParagraphSeparator
            | G::SpaceSeparator
    )
}

/// `repr(s)` for a `str`: single quotes unless the text holds a single quote and no double quote.
pub fn repr(s: &str) -> String {
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
            c if is_printable(c) => out.push(c),
            c if (c as u32) < 0x100 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) < 0x10000 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push(quote);
    out
}

/// Python's `repr(float)`: the shortest digits that round-trip, fixed notation for 1e-4 <= |x| < 1e16 and
/// scientific (`1e+16`, `1e-05`) outside it.
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let sci = format!("{:e}", f.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    let sign = if f < 0.0 { "-" } else { "" };
    if (-4..16).contains(&exp) {
        let n = digits.len() as i32;
        let body = if exp >= 0 {
            if n <= exp + 1 {
                format!("{}{}.0", digits, "0".repeat((exp + 1 - n) as usize))
            } else {
                format!("{}.{}", &digits[..(exp + 1) as usize], &digits[(exp + 1) as usize..])
            }
        } else {
            format!("0.{}{}", "0".repeat((-exp - 1) as usize), digits)
        };
        format!("{sign}{body}")
    } else {
        let m = if digits.len() > 1 { format!("{}.{}", &digits[..1], &digits[1..]) } else { digits };
        let e = if exp < 0 { format!("-{:02}", -exp) } else { format!("+{:02}", exp) };
        format!("{sign}{m}e{e}")
    }
}

/// `repr()` of a JSON value as Python holds it (`None`, `True`, `'text'`, `[1, 'a']`, `{'k': 1}`).
pub fn value_repr(v: &Value) -> String {
    match v {
        Value::String(s) => repr(s),
        Value::Array(items) => format!("[{}]", items.iter().map(value_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter().map(|(k, v)| format!("{}: {}", repr(k), value_repr(v))).collect::<Vec<_>>().join(", ")
        ),
        other => value_str(other),
    }
}

/// `str()` of a JSON value as Python holds it.
pub fn value_str(v: &Value) -> String {
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
                float_repr(n.as_f64().unwrap_or(f64::NAN))
            }
        }
        Value::String(s) => s.clone(),
        other => value_repr(other),
    }
}

/// Python truthiness of a JSON value.
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

/// `str(x or "")` for an optional JSON value.
pub fn str_or_empty(v: Option<&Value>) -> String {
    match v {
        Some(v) if truthy(v) => value_str(v),
        _ => String::new(),
    }
}

/// `int(text)`: surrounding whitespace, an optional sign, decimal digits with single underscores between
/// them. A number too large for i128 saturates (Python has no limit; every caller only range-checks it).
pub fn int_of_str(text: &str) -> Option<i128> {
    let t = strip(text);
    let (neg, body) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if body.is_empty() || body.starts_with('_') || body.ends_with('_') || body.contains("__") {
        return None;
    }
    let mut n: i128 = 0;
    for c in body.chars() {
        if c == '_' {
            continue;
        }
        let d = c.to_digit(10).or_else(|| unicode_decimal(c))? as i128;
        n = n.saturating_mul(10).saturating_add(d);
    }
    Some(if neg { -n } else { n })
}

/// The value of a non-ASCII Unicode decimal digit (Nd), which Python's `int()` accepts. Unicode encodes
/// every decimal digit set as a contiguous run 0..9, and adjacent sets (the mathematical digits) follow one
/// another in whole runs of ten, so a digit's value is its distance from the start of its Nd run, mod 10.
fn unicode_decimal(c: char) -> Option<u32> {
    use unicode_general_category::{get_general_category, GeneralCategory};
    let is_nd = |u: u32| char::from_u32(u).is_some_and(|c| get_general_category(c) == GeneralCategory::DecimalNumber);
    let u = c as u32;
    if !is_nd(u) {
        return None;
    }
    let mut start = u;
    while start > 0 && is_nd(start - 1) {
        start -= 1;
    }
    Some((u - start) % 10)
}

/// `int(v)` for a JSON value: ints as they are, floats truncated, bools as 0/1, strings parsed.
pub fn int_of_value(v: &Value) -> Option<i128> {
    match v {
        Value::Bool(b) => Some(*b as i128),
        Value::Number(n) => n
            .as_i64()
            .map(i128::from)
            .or_else(|| n.as_u64().map(i128::from))
            .or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f.trunc() as i128)),
        Value::String(s) => int_of_str(s),
        _ => None,
    }
}

/// Python's `json.dumps` default `ensure_ascii=True`: every character outside printable ASCII left raw by
/// serde_json (DEL and everything non-ASCII) becomes `\uXXXX`, astral characters as a surrogate pair.
/// serde_json already escapes quotes, backslashes and C0 controls the way Python does.
pub fn ensure_ascii(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        if (c as u32) < 0x7f {
            out.push(c);
            continue;
        }
        let mut units = [0u16; 2];
        for u in c.encode_utf16(&mut units) {
            out.push_str(&format!("\\u{:04x}", u));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repr_matches_python() {
        assert_eq!(repr("v_zz"), "'v_zz'");
        assert_eq!(repr("it's"), "\"it's\"");
        assert_eq!(repr("a'b\"c"), "'a\\'b\"c'");
        assert_eq!(repr("a\\b\n\x01\u{a0}é"), "'a\\\\b\\n\\x01\\xa0é'");
        // Python 3.13: unassigned (U+0378, U+D7FF) and format (U+E0001) characters are escaped
        assert_eq!(repr("\u{378}\u{d7ff}\u{e0001}😀"), "'\\u0378\\ud7ff\\U000e0001😀'");
    }

    #[test]
    fn float_repr_matches_python() {
        assert_eq!(float_repr(1.0), "1.0");
        assert_eq!(float_repr(0.1), "0.1");
        assert_eq!(float_repr(1e16), "1e+16");
        assert_eq!(float_repr(1.5e-5), "1.5e-05");
        assert_eq!(float_repr(123.456), "123.456");
        assert_eq!(float_repr(0.0001), "0.0001");
        assert_eq!(float_repr(-2.5), "-2.5");
    }

    #[test]
    fn value_str_and_truthiness_match_python() {
        assert_eq!(value_str(&json!(null)), "None");
        assert_eq!(value_str(&json!(true)), "True");
        assert_eq!(value_str(&json!(7)), "7");
        assert_eq!(value_str(&json!(7.0)), "7.0");
        assert_eq!(value_str(&json!([1, "a"])), "[1, 'a']");
        assert_eq!(value_str(&json!({"k": null})), "{'k': None}");
        assert_eq!(str_or_empty(Some(&json!(0))), "");
        assert_eq!(str_or_empty(Some(&json!(""))), "");
        assert_eq!(str_or_empty(None), "");
        assert_eq!(str_or_empty(Some(&json!(12))), "12");
    }

    #[test]
    fn int_of_str_matches_python() {
        assert_eq!(int_of_str(" 8 "), Some(8));
        assert_eq!(int_of_str("+1_0"), Some(10));
        assert_eq!(int_of_str("-3"), Some(-3));
        assert_eq!(int_of_str("1__0"), None);
        assert_eq!(int_of_str("_1"), None);
        assert_eq!(int_of_str("x"), None);
        assert_eq!(int_of_str(""), None);
        assert_eq!(int_of_str("٣"), Some(3));
        assert_eq!(int_of_str("𝟖"), Some(8)); // MATHEMATICAL BOLD DIGIT EIGHT, the first of five runs
        assert_eq!(int_of_str("𝟿"), Some(9)); // MATHEMATICAL MONOSPACE DIGIT NINE, the last
        assert_eq!(int_of_str("𑁩"), Some(3)); // BRAHMI DIGIT THREE
        assert_eq!(int_of_str("²"), None); // a digit (isdigit) but not a decimal: int() refuses it
    }

    #[test]
    fn split_and_strip_use_python_whitespace() {
        assert_eq!(split(" a\x1cb  c\u{a0}"), vec!["a", "b", "c"]);
        assert_eq!(strip("\x1f x \n"), "x");
    }

    #[test]
    fn ensure_ascii_escapes_like_python() {
        assert_eq!(ensure_ascii("{\"a\":\"é\u{7f}😀\"}"), "{\"a\":\"\\u00e9\\u007f\\ud83d\\ude00\"}");
    }
}
