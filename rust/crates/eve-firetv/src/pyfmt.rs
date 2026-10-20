//! The few Python builtins whose exact behavior the port depends on: truthiness, `str()`, `int()`, `round()`,
//! `repr()` of a string, `str.splitlines()` and `json.dump(..., indent=2)` (ensure_ascii on), so that a file or a
//! line written here is the one the Python daemon writes.

use serde_json::Value;

/// Python's truth value of a JSON value.
pub fn py_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `str(v)` for a value that came from JSON.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// `repr(v)` for a value that came from JSON.
pub fn py_repr(v: &Value) -> String {
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
                py_float_repr(n.as_f64().unwrap_or(f64::NAN))
            }
        }
        Value::String(s) => py_repr_str(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter().map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// `repr(s)` for a string: single quotes unless the string has a single quote and no double one.
pub fn py_repr_str(s: &str) -> String {
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
            c if (c as u32) < 0x20 || (0x7f..0xa0).contains(&(c as u32)) => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `repr(f)` for a float: the shortest digits that round-trip, positional for exponents -4..16, else `1e+16`.
pub fn py_float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let sci = format!("{:e}", f.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    let n = digits.len() as i32;
    let sign = if f.is_sign_negative() { "-" } else { "" };
    let body = if (-4..16).contains(&exp) {
        if exp >= n - 1 {
            format!("{digits}{}.0", "0".repeat((exp - (n - 1)) as usize))
        } else if exp >= 0 {
            let (a, b) = digits.split_at((exp + 1) as usize);
            format!("{a}.{b}")
        } else {
            format!("0.{}{digits}", "0".repeat((-exp - 1) as usize))
        }
    } else {
        let (a, b) = digits.split_at(1);
        let frac = if b.is_empty() { String::new() } else { format!(".{b}") };
        let esign = if exp < 0 { '-' } else { '+' };
        format!("{a}{frac}e{esign}{:02}", exp.abs())
    };
    format!("{sign}{body}")
}

/// `int(v)`, or None where Python raises TypeError or ValueError.
pub fn py_int(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(i)
            } else if let Some(u) = n.as_u64() {
                Some(i64::try_from(u).unwrap_or(i64::MAX))
            } else {
                n.as_f64().filter(|f| f.is_finite()).map(|f| f.trunc() as i64)
            }
        }
        Value::String(s) => py_int_str(s),
        _ => None,
    }
}

/// `int(s)` for a string: surrounding whitespace, a sign, digits with single underscores between them.
pub fn py_int_str(s: &str) -> Option<i64> {
    // `str.strip()` whitespace: Unicode White_Space plus the four separators \x1c-\x1f
    let t = s.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c));
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let clean: String = digits.chars().filter(|c| *c != '_').collect();
    // int() reads the decimal digits of any script ("５５５５" is 5555)
    let n = parse_decimal(&clean)?;
    Some(if neg { -n } else { n })
}

static UNICODE_DIGIT: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"^\d$").expect("digit regex"));

fn is_decimal(c: char) -> bool {
    let mut buf = [0u8; 4];
    UNICODE_DIGIT.is_match(c.encode_utf8(&mut buf))
}

/// The value of a decimal digit of any script (what `\d` matches and `int()` reads), else None. Unicode encodes
/// every decimal digit set as a contiguous run 0..9, so the value is the distance from the run's start, mod 10.
pub fn digit_value(c: char) -> Option<i64> {
    if c.is_ascii_digit() {
        return Some(i64::from(c as u8 - b'0'));
    }
    if !is_decimal(c) {
        return None;
    }
    let mut back = 0i64;
    let mut cur = c as u32;
    while let Some(prev) = cur.checked_sub(1).and_then(char::from_u32) {
        if !is_decimal(prev) {
            break;
        }
        back += 1;
        cur -= 1;
    }
    Some(back % 10)
}

/// `int(s)` for a run of `\d` digits (any script); saturates where Python's int would grow.
pub fn parse_decimal(s: &str) -> Option<i64> {
    if s.is_empty() {
        return None;
    }
    let mut n: i64 = 0;
    for c in s.chars() {
        n = n.saturating_mul(10).saturating_add(digit_value(c)?);
    }
    Some(n)
}

/// `round(x)` with no digits: half to even, as an int.
pub fn py_round(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// `str.splitlines()`: every line break Python knows, no empty last line.
pub fn splitlines(s: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let is_break = matches!(
            c,
            '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if !is_break {
            continue;
        }
        lines.push(&s[start..i]);
        let mut next = i + c.len_utf8();
        if c == '\r' {
            if let Some((j, '\n')) = chars.peek().copied() {
                chars.next();
                next = j + 1;
            }
        }
        start = next;
    }
    if start < s.len() {
        lines.push(&s[start..]);
    }
    lines
}

fn json_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
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
}

fn json_value(v: &Value, indent: usize, level: usize, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                out.push_str(&n.to_string());
            } else {
                out.push_str(&py_float_repr(n.as_f64().unwrap_or(f64::NAN)));
            }
        }
        Value::String(s) => json_str(s, out),
        Value::Array(a) => {
            if a.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('\n');
                out.push_str(&" ".repeat(indent * (level + 1)));
                json_value(item, indent, level + 1, out);
            }
            out.push('\n');
            out.push_str(&" ".repeat(indent * level));
            out.push(']');
        }
        Value::Object(o) => {
            if o.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, item)) in o.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('\n');
                out.push_str(&" ".repeat(indent * (level + 1)));
                json_str(k, out);
                out.push_str(": ");
                json_value(item, indent, level + 1, out);
            }
            out.push('\n');
            out.push_str(&" ".repeat(indent * level));
            out.push('}');
        }
    }
}

/// `json.dumps(v, indent=indent)`: Python's layout and its ASCII-only escaping, no trailing newline.
pub fn json_dumps_indent(v: &Value, indent: usize) -> String {
    let mut out = String::new();
    json_value(v, indent, 0, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_float_repr_like_python() {
        for (f, want) in [
            (1.0, "1.0"),
            (0.1, "0.1"),
            (1e16, "1e+16"),
            (1.5e-7, "1.5e-07"),
            (0.0001, "0.0001"),
            (123456.789, "123456.789"),
            (-2.5, "-2.5"),
            (1e15, "1000000000000000.0"),
        ] {
            assert_eq!(py_float_repr(f), want, "{f}");
        }
    }

    #[test]
    fn test_json_dumps_indent_like_python() {
        let v = json!({"host": "192.0.2.40", "port": 5555, "mdns": "", "model": "Ünï", "x": [], "y": {}, "z": [1, {"a": null}]});
        assert_eq!(
            json_dumps_indent(&v, 2),
            "{\n  \"host\": \"192.0.2.40\",\n  \"port\": 5555,\n  \"mdns\": \"\",\n  \"model\": \"\\u00dcn\\u00ef\",\n  \
             \"x\": [],\n  \"y\": {},\n  \"z\": [\n    1,\n    {\n      \"a\": null\n    }\n  ]\n}"
        );
    }

    #[test]
    fn test_int_and_repr_and_splitlines() {
        assert_eq!(py_int(&json!(" 12 ")), Some(12));
        assert_eq!(py_int(&json!("1_000")), Some(1000));
        assert_eq!(py_int(&json!("12.5")), None);
        assert_eq!(py_int(&json!(7.9)), Some(7));
        assert_eq!(py_int(&json!("\u{ff15}\u{ff15}")), Some(55)); // fullwidth digits, as int() reads them
        assert_eq!(py_int(&json!("-7")), Some(-7));
        assert_eq!(py_int(&json!("\u{b2}")), None);
        assert_eq!(py_repr_str("power"), "'power'");
        assert_eq!(py_repr_str("it's"), "\"it's\"");
        assert_eq!(splitlines("a\r\nb\nc\n"), vec!["a", "b", "c"]);
        assert_eq!(splitlines(""), Vec::<&str>::new());
        assert_eq!(py_round(2.5), 2);
        assert_eq!(py_round(3.5), 4);
        assert_eq!(parse_decimal("\u{663}7"), Some(37));
        assert_eq!(parse_decimal("\u{1d7d9}"), Some(1)); // MATHEMATICAL DOUBLE-STRUCK DIGIT ONE
        assert_eq!(parse_decimal("\u{b2}"), None); // superscript two is not \d
        assert_eq!(digit_value('9'), Some(9));
    }
}
