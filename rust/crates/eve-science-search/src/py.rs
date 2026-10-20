//! The few Python value semantics science_search.py leans on: truthiness (`a or b`), `str(x)`, `str.isspace`,
//! `str.strip`, `int()` over the digits `\d` matches, `float()` of a string, and `urllib.parse.quote` /
//! `quote_plus`. The service's answers are JSON, so the values are `serde_json::Value`s. (The same helpers
//! eve-nyt keeps private, with `repr` escaping every character Python's `str.isprintable` refuses.)

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

/// `str(x)` for a JSON value (`None` for a missing key).
pub fn str_of(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => repr(other),
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

/// `isinstance(x, int) and not isinstance(x, bool)` for a JSON value.
pub fn is_int(v: Option<&Value>) -> bool {
    matches!(v, Some(Value::Number(n)) if n.is_i64() || n.is_u64())
}

/// The value of a JSON integer as i64 (a u64 past i64's range saturates: every caller clamps it lower).
pub fn as_int(n: &serde_json::Number) -> Option<i64> {
    n.as_i64().or_else(|| n.as_u64().map(|_| i64::MAX))
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

/// What `str.isprintable` refuses: the Other and Separator categories, the plain space aside.
static NONPRINTABLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[\p{C}\p{Z}]$").expect("valid regex"));

fn printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    let mut b = [0u8; 4];
    !NONPRINTABLE.is_match(c.encode_utf8(&mut b))
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
            c if !printable(c) => {
                let cp = c as u32;
                if cp < 0x100 {
                    out.push_str(&format!("\\x{cp:02x}"));
                } else if cp < 0x10000 {
                    out.push_str(&format!("\\u{cp:04x}"));
                } else {
                    out.push_str(&format!("\\U{cp:08x}"));
                }
            }
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

/// The whitespace `int()` and `float()` strip around a number: `str.isspace` but for the four ASCII separators
/// \x1c-\x1f, which CPython keeps (they sit below 127, where only space, \t, \n, \v, \f and \r count).
pub fn is_number_space(c: char) -> bool {
    is_space(c) && !('\u{1c}'..='\u{1f}').contains(&c)
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

/// `float(s)` for a string: surrounding whitespace stripped (`is_number_space`: "12\x1c" raises in Python), any
/// Unicode decimal digit read as its value, an underscore only between two digits, and "inf", "infinity" and "nan"
/// in any case. None where Python raises ValueError.
pub fn parse_float(s: &str) -> Option<f64> {
    let chars: Vec<char> = s
        .trim_matches(is_number_space)
        .chars()
        .map(|c| digit(c).and_then(|d| char::from_digit(d, 10)).unwrap_or(c))
        .collect();
    let mut text = String::with_capacity(chars.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            let before = i > 0 && chars[i - 1].is_ascii_digit();
            let after = chars.get(i + 1).is_some_and(|n| n.is_ascii_digit());
            if !(before && after) {
                return None;
            }
            continue;
        }
        text.push(c);
    }
    if text.is_empty() {
        return None;
    }
    text.parse::<f64>().ok()
}

/// `float(x)` for a JSON value: a number, a bool (True is 1.0), or a string as `parse_float` reads it; None where
/// Python raises TypeError or ValueError.
pub fn float_of(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => parse_float(s),
        _ => None,
    }
}

fn percent_encode(s: &str, safe: &str, plus: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let c = b as char;
        if b.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '~') || (b.is_ascii() && safe.contains(c)) {
            out.push(c);
        } else if plus && b == b' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `urllib.parse.quote(s, safe=safe)`.
pub fn quote(s: &str, safe: &str) -> String {
    percent_encode(s, safe, false)
}

/// `urllib.parse.quote_plus(s, safe="")`, as `urlencode` (and so httpx's query string) writes each key and value.
pub fn quote_plus(s: &str) -> String {
    percent_encode(s, "", true)
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
    }

    #[test]
    fn repr_matches_python() {
        assert_eq!(repr(&json!({"a": [1, true, null, "it's"]})), r#"{'a': [1, True, None, "it's"]}"#);
        assert_eq!(repr(&json!("a\u{3000}b\u{200b}\u{1f600}\u{e000}")), "'a\\u3000b\\u200b\u{1f600}\\ue000'");
        assert_eq!(repr(&json!("\u{a0}x\u{7f}")), "'\\xa0x\\x7f'");
        assert_eq!(str_of(Some(&json!(5))), "5");
        assert_eq!(str_or_empty(Some(&json!(0))), "");
        assert_eq!(str_or_empty(Some(&json!(true))), "True");
        assert_eq!(str_or_empty(Some(&json!(1.0))), "1.0");
    }

    #[test]
    fn unicode_digits_read_like_int() {
        assert_eq!(parse_digits("\u{663}\u{660}"), Some(30)); // Arabic-Indic 3 0
        assert_eq!(parse_digits("-12"), Some(-12));
        assert_eq!(digit('x'), None);
    }

    #[test]
    fn float_reads_like_python() {
        assert_eq!(parse_float(" 12 "), Some(12.0));
        assert_eq!(parse_float("12.5"), Some(12.5));
        assert_eq!(parse_float("1_000"), Some(1000.0));
        assert_eq!(parse_float("1__0"), None);
        assert_eq!(parse_float("_1"), None);
        assert_eq!(parse_float("1e3"), Some(1000.0));
        assert_eq!(parse_float("\u{661}\u{662}"), Some(12.0));
        assert!(parse_float("Infinity").is_some_and(f64::is_infinite));
        assert!(parse_float("nan").is_some_and(f64::is_nan));
        assert_eq!(parse_float("None"), None);
        assert_eq!(parse_float(""), None);
        // float() strips Unicode spaces but keeps the ASCII separators, so "12\x1c" is no number
        assert_eq!(parse_float("\u{a0}12\u{3000}"), Some(12.0));
        assert_eq!(parse_float("12\u{1c}"), None);
        assert_eq!(float_of(Some(&json!(true))), Some(1.0));
        assert_eq!(float_of(Some(&json!(null))), None);
        assert_eq!(float_of(Some(&json!([1]))), None);
    }

    #[test]
    fn quote_matches_urllib() {
        assert_eq!(quote("10.1/a b#c", "/:"), "10.1/a%20b%23c");
        assert_eq!(quote("é~*", ""), "%C3%A9~%2A");
        assert_eq!(quote_plus("a b&c=d/~"), "a+b%26c%3Dd%2F~");
    }
}
