//! Python-compatible helpers, so the Rust lights write what the Python ones wrote: the bytes `json.dumps`
//! produces (spacing, `ensure_ascii`), `str()` and `repr()` of a JSON value for messages, `int()`, truthiness,
//! and `bytes.fromhex`.

use serde_json::Value;

/// How `json.dumps` separates items and keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// `json.dumps(v)`: `", "` and `": "`.
    Default,
    /// `json.dumps(v, separators=(",", ":"))`.
    Compact,
    /// `json.dumps(v, indent=n)`: `","` + newline and `": "`.
    Indent(usize),
}

/// `json.dumps(v)` with Python's defaults (`ensure_ascii=True`).
pub fn dumps(v: &Value) -> String {
    dumps_with(v, Style::Default)
}

/// `json.dumps(v, separators=(",", ":"))`.
pub fn dumps_compact(v: &Value) -> String {
    dumps_with(v, Style::Compact)
}

/// `json.dumps(v, indent=n)`.
pub fn dumps_indent(v: &Value, n: usize) -> String {
    dumps_with(v, Style::Indent(n))
}

pub fn dumps_with(v: &Value, style: Style) -> String {
    let mut out = String::new();
    write_value(&mut out, v, style, 0);
    out
}

fn write_value(out: &mut String, v: &Value, style: Style, level: usize) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&number_repr(n)),
        Value::String(s) => write_json_string(out, s),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                separator(out, style, level, i == 0);
                write_value(out, item, style, level + 1);
            }
            closing(out, style, level);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, item)) in map.iter().enumerate() {
                separator(out, style, level, i == 0);
                write_json_string(out, k);
                out.push_str(if style == Style::Compact { ":" } else { ": " });
                write_value(out, item, style, level + 1);
            }
            closing(out, style, level);
            out.push('}');
        }
    }
}

fn separator(out: &mut String, style: Style, level: usize, first: bool) {
    match style {
        Style::Default => {
            if !first {
                out.push_str(", ");
            }
        }
        Style::Compact => {
            if !first {
                out.push(',');
            }
        }
        Style::Indent(n) => {
            if !first {
                out.push(',');
            }
            out.push('\n');
            out.push_str(&" ".repeat(n * (level + 1)));
        }
    }
}

fn closing(out: &mut String, style: Style, level: usize) {
    if let Style::Indent(n) = style {
        out.push('\n');
        out.push_str(&" ".repeat(n * level));
    }
}

/// A JSON string as Python's encoder writes it with `ensure_ascii=True`: everything outside space..tilde is
/// escaped, `\uXXXX` in lower-case hex, astral characters as a surrogate pair.
fn write_json_string(out: &mut String, s: &str) {
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
                    out.push_str(&format!("\\u{:04x}", unit));
                }
            }
        }
    }
    out.push('"');
}

fn number_repr(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        i.to_string()
    } else if let Some(u) = n.as_u64() {
        u.to_string()
    } else {
        float_repr(n.as_f64().unwrap_or(0.0))
    }
}

/// `repr(float)`: the shortest round-trip digits, exponent form outside 1e-4..1e16 written `1e-05` / `1e+16`.
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let s = format!("{:?}", f);
    match s.split_once('e') {
        None => s,
        Some((mantissa, exp)) => {
            let (sign, digits) = match exp.strip_prefix('-') {
                Some(d) => ('-', d),
                None => ('+', exp),
            };
            format!("{mantissa}e{sign}{digits:0>2}")
        }
    }
}

/// `str(value)` of a JSON value: a string is itself, the rest as Python prints them.
pub fn str_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => repr(other),
    }
}

/// `repr(value)` of a JSON value, as Python's `!r` writes it.
pub fn repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Number(n) => number_repr(n),
        Value::String(s) => repr_str(s),
        Value::Array(items) => format!("[{}]", items.iter().map(repr).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter().map(|(k, v)| format!("{}: {}", repr_str(k), repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// `repr(str)`: single quotes unless the text has a single quote and no double one.
pub fn repr_str(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

/// Python truthiness of a JSON value.
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

/// The Python type name of a JSON value, for error messages.
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

/// `isinstance(v, int)`: a JSON integer, or a bool (a bool is an int in Python).
pub fn is_int(v: &Value) -> bool {
    matches!(v, Value::Bool(_)) || matches!(v, Value::Number(n) if !n.is_f64())
}

/// `isinstance(v, (int, float))`.
pub fn is_number(v: &Value) -> bool {
    matches!(v, Value::Bool(_) | Value::Number(_))
}

/// An error Python would raise converting a value: its exception name and message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyError {
    pub kind: &'static str,
    pub message: String,
}

/// `int(value)`: an int is itself, a float truncates, a bool is 0 or 1, a string is parsed.
pub fn int_of(v: &Value) -> Result<i64, PyError> {
    match v {
        Value::Bool(b) => Ok(*b as i64),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i)
            } else if n.as_u64().is_some() {
                Ok(i64::MAX)
            } else {
                let f = n.as_f64().unwrap_or(0.0);
                if f.is_nan() {
                    Err(PyError { kind: "ValueError", message: "cannot convert float NaN to integer".into() })
                } else if f.is_infinite() {
                    Err(PyError { kind: "OverflowError", message: "cannot convert float infinity to integer".into() })
                } else {
                    Ok(f.trunc() as i64)
                }
            }
        }
        Value::String(s) => parse_int(s).ok_or_else(|| PyError {
            kind: "ValueError",
            message: format!("invalid literal for int() with base 10: {}", repr_str(s)),
        }),
        other => Err(PyError {
            kind: "TypeError",
            message: format!(
                "int() argument must be a string, a bytes-like object or a real number, not '{}'",
                type_name(other)
            ),
        }),
    }
}

/// Python's `str.isspace()` for one character: Unicode White_Space plus the separators U+001C..U+001F.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `text.strip()`.
pub fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// Python's `\w` in a str pattern: `str.isalnum()` (every letter and number category) or `_`. Rust's own
/// `\w` also takes combining marks and every connector punctuation, and leaves out `½` and `²`.
pub fn is_word(c: char) -> bool {
    use unicode_general_category::{get_general_category, GeneralCategory::*};
    c == '_'
        || matches!(
            get_general_category(c),
            UppercaseLetter
                | LowercaseLetter
                | TitlecaseLetter
                | ModifierLetter
                | OtherLetter
                | DecimalNumber
                | LetterNumber
                | OtherNumber
        )
}

/// The value of a Unicode decimal digit (category Nd, what Python's `\d` matches and `int()` reads): "５" is 5.
/// Unicode assigns Nd only in runs of ten, 0 to 9, so the value is the offset from the start of the run.
pub fn decimal_value(c: char) -> Option<u32> {
    use unicode_general_category::{get_general_category, GeneralCategory::DecimalNumber};
    if c.is_ascii_digit() {
        return c.to_digit(10);
    }
    if get_general_category(c) != DecimalNumber {
        return None;
    }
    let mut start = c as u32;
    while let Some(prev) = start.checked_sub(1).and_then(char::from_u32) {
        if get_general_category(prev) != DecimalNumber {
            break;
        }
        start -= 1;
    }
    Some((c as u32 - start) % 10)
}

/// `int(digits)` for a run of decimal digits (any script), as a regex's `\d+` group gives them.
pub fn digits_value(digits: &str) -> Option<i64> {
    if digits.is_empty() {
        return None;
    }
    let mut n: i64 = 0;
    for c in digits.chars() {
        n = n.saturating_mul(10).saturating_add(decimal_value(c)? as i64);
    }
    Some(n)
}

/// `re.search(rf"\b{re.escape(needle)}\b", hay)` with Python's word characters.
pub fn contains_bounded(hay: &str, needle: &str) -> bool {
    let (Some(first), Some(last)) = (needle.chars().next(), needle.chars().next_back()) else {
        // \b\b on its own: a boundary anywhere
        let chars: Vec<char> = hay.chars().collect();
        return (0..=chars.len()).any(|i| {
            let before = i.checked_sub(1).map(|j| is_word(chars[j])).unwrap_or(false);
            let after = chars.get(i).map(|c| is_word(*c)).unwrap_or(false);
            before != after
        });
    };
    for (at, _) in hay.char_indices() {
        if !hay[at..].starts_with(needle) {
            continue;
        }
        let before = hay[..at].chars().next_back().map(is_word).unwrap_or(false);
        let after = hay[at + needle.len()..].chars().next().map(is_word).unwrap_or(false);
        if before != is_word(first) && is_word(last) != after {
            return true;
        }
    }
    false
}

/// `int(text)` for base 10: surrounding whitespace, a sign, underscores between digits, and decimal digits of
/// any script.
fn parse_int(s: &str) -> Option<i64> {
    let t = strip(s);
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let clean: String = digits.chars().filter(|c| *c != '_').collect();
    let n = digits_value(&clean)?;
    Some(if neg { -n } else { n })
}

/// `bytes.fromhex(text)`: pairs of hex digits, ASCII whitespace allowed between them.
pub fn fromhex(s: &str) -> Result<Vec<u8>, PyError> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let bad = |pos: usize| PyError {
        kind: "ValueError",
        message: format!("non-hexadecimal number found in fromhex() arg at position {pos}"),
    };
    while i < chars.len() {
        if chars[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let hi = chars[i].to_digit(16).filter(|_| chars[i].is_ascii()).ok_or_else(|| bad(i))?;
        let lo = chars
            .get(i + 1)
            .and_then(|c| if c.is_ascii() { c.to_digit(16) } else { None })
            .ok_or_else(|| bad(i + 1))?;
        out.push((hi * 16 + lo) as u8);
        i += 2;
    }
    Ok(out)
}

/// `round(x)` on a float: half to even, as Python 3 rounds.
pub fn round_half_even(x: f64) -> f64 {
    x.round_ties_even()
}

/// `s[:1].upper() + s[1:]`.
pub fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// The last `n` characters, as `s[-n:]`.
pub fn tail(s: &str, n: usize) -> String {
    let count = s.chars().count();
    s.chars().skip(count.saturating_sub(n)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dumps_matches_python_spacing() {
        let v = json!({"msg": {"cmd": "turn", "data": {"value": 0}}});
        assert_eq!(dumps(&v), r#"{"msg": {"cmd": "turn", "data": {"value": 0}}}"#);
        assert_eq!(dumps_compact(&json!({"state": true, "r": 0})), r#"{"state":true,"r":0}"#);
        assert_eq!(
            dumps_indent(&json!({"lights": [{"name": "caf\u{e9}"}], "x": []}), 2),
            "{\n  \"lights\": [\n    {\n      \"name\": \"caf\\u00e9\"\n    }\n  ],\n  \"x\": []\n}"
        );
        assert_eq!(dumps(&json!("\u{1F600}")), "\"\\ud83d\\ude00\"");
    }

    #[test]
    fn repr_and_floats() {
        assert_eq!(repr_str("plaid"), "'plaid'");
        assert_eq!(repr_str("it's"), "\"it's\"");
        assert_eq!(repr(&json!({"code": -1})), "{'code': -1}");
        assert_eq!(float_repr(3.0), "3.0");
        assert_eq!(float_repr(1e-5), "1e-05");
        assert_eq!(float_repr(1e16), "1e+16");
        assert_eq!(str_of(&json!(3.4)), "3.4");
    }

    #[test]
    fn int_and_hex() {
        assert_eq!(int_of(&json!("50")), Ok(50));
        assert_eq!(int_of(&json!(30.7)), Ok(30));
        assert_eq!(int_of(&json!(true)), Ok(1));
        assert!(int_of(&json!("x")).is_err());
        assert_eq!(fromhex("00 1f").unwrap(), vec![0, 31]);
        assert_eq!(fromhex("").unwrap(), Vec::<u8>::new());
        assert!(fromhex("not hex").is_err());
        assert_eq!(round_half_even(127.5), 128.0);
        assert_eq!(round_half_even(126.5), 126.0);
    }
}
