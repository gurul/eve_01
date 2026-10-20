//! The few Python value semantics nyt.py leans on: truthiness (`a or b`), `str(x)`, `str.isspace`,
//! `str.strip`, and `int()` over the digits `\d` matches. NYT's answers are JSON, so the values are
//! `serde_json::Value`s.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

/// `bool(v)`; a missing key is Python's `None`.
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                i != 0
            } else if let Some(u) = n.as_u64() {
                u != 0
            } else {
                n.as_f64().is_some_and(|f| f != 0.0)
            }
        }
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `a or b`.
pub fn or<'a>(a: Option<&'a Value>, b: Option<&'a Value>) -> Option<&'a Value> {
    if truthy(a) {
        a
    } else {
        b
    }
}

/// `str(x)` for a JSON value (`None` for a missing key).
pub fn str_of(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => repr(other),
    }
}

/// `str(x if x is not None else "")`.
pub fn str_or_none_empty(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        other => str_of(other),
    }
}

/// `str(x or "")`.
pub fn str_or_empty(v: Option<&Value>) -> String {
    if truthy(v) {
        str_of(v)
    } else {
        String::new()
    }
}

/// Python's `repr` of a float: the shortest round-trip digits, positional between 1e-4 and 1e16.
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
    let sci = format!("{f:e}"); // shortest digits, e.g. "-1.5e-5"
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let neg = mant.starts_with('-');
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let sign = if neg { "-" } else { "" };
    if (-4..16).contains(&exp) {
        let n = digits.len() as i32;
        let body = if exp >= 0 {
            if exp + 1 >= n {
                format!("{digits}{}.0", "0".repeat((exp + 1 - n) as usize))
            } else {
                let (a, b) = digits.split_at((exp + 1) as usize);
                format!("{a}.{b}")
            }
        } else {
            format!("0.{}{digits}", "0".repeat((-exp - 1) as usize))
        };
        format!("{sign}{body}")
    } else {
        let (first, rest) = digits.split_at(1);
        let m = if rest.is_empty() { first.to_string() } else { format!("{first}.{rest}") };
        let es = if exp < 0 { "-" } else { "+" };
        format!("{sign}{m}e{es}{:02}", exp.abs())
    }
}

fn repr_str(s: &str) -> String {
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
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c if c == '\u{2028}' || c == '\u{2029}' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `repr` of what `json.loads` returns.
pub fn repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => {
            if n.is_f64() {
                float_repr(n.as_f64().unwrap_or(f64::NAN))
            } else {
                n.to_string()
            }
        }
        Value::String(s) => repr_str(s),
        Value::Array(a) => format!("[{}]", a.iter().map(repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter().map(|(k, v)| format!("{}: {}", repr_str(k), repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// `str.isspace` for one character (Python counts the four ASCII separators \x1c-\x1f too).
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()`.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `str.rstrip()`.
pub fn rstrip(s: &str) -> &str {
    s.trim_end_matches(is_space)
}

static ND: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d$").expect("valid regex"));

fn is_decimal(cp: u32) -> bool {
    char::from_u32(cp).is_some_and(|c| {
        let mut b = [0u8; 4];
        ND.is_match(c.encode_utf8(&mut b))
    })
}

/// The value of a character `\d` matches, as `int()` reads it (every Unicode decimal digit run starts at 0).
pub fn digit(c: char) -> Option<u32> {
    if let Some(d) = c.to_digit(10) {
        return Some(d);
    }
    let cp = c as u32;
    if !is_decimal(cp) {
        return None;
    }
    let mut start = cp;
    while start > 0 && is_decimal(start - 1) {
        start -= 1;
    }
    Some((cp - start) % 10)
}

/// `int(s)` over a run of `\d` characters, with an optional leading minus.
pub fn parse_digits(s: &str) -> Option<i64> {
    let (neg, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    if body.is_empty() {
        return None;
    }
    let mut n: i64 = 0;
    for c in body.chars() {
        n = n.checked_mul(10)?.checked_add(i64::from(digit(c)?))?;
    }
    Some(if neg { -n } else { n })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn float_repr_matches_python() {
        assert_eq!(float_repr(1.5), "1.5");
        assert_eq!(float_repr(100.0), "100.0");
        assert_eq!(float_repr(1e16), "1e+16");
        assert_eq!(float_repr(1.5e-5), "1.5e-05");
        assert_eq!(float_repr(0.0001), "0.0001");
        assert_eq!(float_repr(-2.25), "-2.25");
        assert_eq!(float_repr(123456789.125), "123456789.125");
    }

    #[test]
    fn repr_matches_python() {
        assert_eq!(repr(&json!({"a": [1, true, null, "it's"]})), r#"{'a': [1, True, None, "it's"]}"#);
        assert_eq!(str_of(Some(&json!(5))), "5");
        assert_eq!(str_or_empty(Some(&json!(0))), "");
        assert_eq!(str_or_none_empty(Some(&json!(0))), "0");
    }

    #[test]
    fn unicode_digits_read_like_int() {
        assert_eq!(parse_digits("\u{663}\u{660}"), Some(30)); // Arabic-Indic 3 0
        assert_eq!(parse_digits("-12"), Some(-12));
        assert_eq!(digit('x'), None);
    }
}
