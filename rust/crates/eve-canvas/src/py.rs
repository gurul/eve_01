//! The Python semantics `canvas.py` leans on, so the port answers what the Python answers: truthiness and `or`,
//! `str()` / `repr()` of JSON values, `int()`, whitespace (`strip`, `split`), and the URL quoting of
//! `urllib.parse.urlencode` and `urllib.parse.quote`.

use serde_json::Value;

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

/// `" ".join(s.split())`.
pub fn squash_ws(s: &str) -> String {
    s.split(is_space).filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")
}

/// The first `n` characters (code points) of `s`: Python's `s[:n]`.
pub fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Python's truth value of a JSON-shaped value; a missing key is `None`, so false.
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `a or b`, as a JSON value (a missing key is `None`, which is `null`).
pub fn or(a: Option<&Value>, b: Option<&Value>) -> Value {
    if truthy(a) {
        a.cloned().unwrap_or(Value::Null)
    } else {
        b.cloned().unwrap_or(Value::Null)
    }
}

/// `str(v or "")`: the value's `str()` when it is truthy, else "".
pub fn str_or_empty(v: Option<&Value>) -> String {
    if truthy(v) {
        v.map(py_str).unwrap_or_default()
    } else {
        String::new()
    }
}

/// `x.get(key)` on a JSON object; anything but an object has no keys.
pub fn get<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.as_object().and_then(|m| m.get(key))
}

/// `x.get(key) if isinstance(x.get(key), dict) else {}`.
pub fn dict_or_empty<'a>(v: &'a Value, key: &str) -> &'a Value {
    static EMPTY: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| Value::Object(Default::default()));
    match get(v, key) {
        Some(d @ Value::Object(_)) => d,
        _ => &EMPTY,
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

/// `str(x.get(key))`: a missing key is `None`.
pub fn py_str_opt(v: Option<&Value>) -> String {
    v.map(py_str).unwrap_or_else(|| "None".into())
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
/// single underscores between digits). Err carries the Python exception's class name: a string that is not a
/// number is a ValueError, a list, a dict or None a TypeError.
pub fn py_int(v: &Value) -> Result<i64, &'static str> {
    match v {
        Value::Bool(b) => Ok(*b as i64),
        Value::Number(n) => Ok(if let Some(i) = n.as_i64() {
            i
        } else if let Some(u) = n.as_u64() {
            i64::try_from(u).unwrap_or(i64::MAX)
        } else {
            let f = n.as_f64().unwrap_or(0.0);
            if f.is_nan() {
                return Err("ValueError");
            }
            if f.is_infinite() {
                return Err("OverflowError");
            }
            f.trunc() as i64
        }),
        Value::String(s) => parse_int(s).ok_or("ValueError"),
        _ => Err("TypeError"),
    }
}

/// `int(x[key])`: a missing key is a KeyError.
pub fn py_int_key(v: &Value, key: &str) -> Result<i64, &'static str> {
    match get(v, key) {
        Some(x) => py_int(x),
        None => Err("KeyError"),
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

/// Percent-encodes `s`'s UTF-8 bytes, leaving ASCII letters, digits, `_.-~` and the bytes in `safe` as they
/// are; `plus` turns a space into `+` (`quote_plus`).
fn quote_bytes(s: &str, safe: &[u8], plus: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || b"_.-~".contains(&b) || safe.contains(&b) {
            out.push(b as char);
        } else if plus && b == b' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `urllib.parse.quote(s)` (safe="/").
pub fn quote(s: &str) -> String {
    quote_bytes(s, b"/", false)
}

/// `urllib.parse.urlencode(params)` for a list of string pairs (`quote_plus` on each side).
pub fn urlencode(params: &[(String, String)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", quote_bytes(k, b"", true), quote_bytes(v, b"", true)))
        .collect::<Vec<_>>()
        .join("&")
}

/// The parts of `urllib.parse.urlsplit(url)` that `canvas.py` reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitUrl {
    /// Lowercased, as urlsplit gives it.
    pub scheme: String,
    pub netloc: String,
}

impl SplitUrl {
    /// `.hostname`: the netloc without user info or port, lowercased; None when empty.
    pub fn hostname(&self) -> Option<String> {
        let host = self.netloc.rsplit_once('@').map(|(_, h)| h).unwrap_or(&self.netloc);
        let host = if let Some(rest) = host.strip_prefix('[') {
            rest.split_once(']').map(|(h, _)| h).unwrap_or(rest)
        } else {
            host.split_once(':').map(|(h, _)| h).unwrap_or(host)
        };
        if host.is_empty() {
            None
        } else {
            Some(host.to_lowercase())
        }
    }
}

/// `urllib.parse.urlsplit(url)`'s scheme and netloc. Err where Python raises ValueError (an unbalanced IPv6
/// bracket in the netloc).
pub fn urlsplit(url: &str) -> Result<SplitUrl, &'static str> {
    // Python strips leading C0 controls and spaces, and drops every tab and newline.
    let url: String = url.trim_start_matches(|c: char| c <= ' ').chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let mut rest = url.as_str();
    let mut scheme = String::new();
    if let Some(i) = rest.find(':') {
        let cand = &rest[..i];
        let mut chars = cand.chars();
        if chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        {
            scheme = cand.to_ascii_lowercase();
            rest = &rest[i + 1..];
        }
    }
    let mut netloc = String::new();
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        netloc = after[..end].to_string();
        if netloc.contains('[') != netloc.contains(']') {
            return Err("ValueError");
        }
    }
    Ok(SplitUrl { scheme, netloc })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn urlencode_and_quote_match_python() {
        let p = vec![("include[]".to_string(), "term".to_string()), ("search_term".into(), "HW 3/é".into())];
        assert_eq!(urlencode(&p), "include%5B%5D=term&search_term=HW+3%2F%C3%A9");
        assert_eq!(quote("a3 guide/x"), "a3%20guide/x");
    }

    #[test]
    fn urlsplit_and_hostname() {
        let s = urlsplit("HTTPS://User@Canvas.Example.edu:8443/api/v1?x").unwrap();
        assert_eq!(s.scheme, "https");
        assert_eq!(s.netloc, "User@Canvas.Example.edu:8443");
        assert_eq!(s.hostname().as_deref(), Some("canvas.example.edu"));
        assert_eq!(urlsplit("https:///path").unwrap().hostname(), None);
        assert!(urlsplit("https://[::1/x").is_err());
        assert_eq!(urlsplit("canvas.example.edu").unwrap().scheme, "");
    }

    #[test]
    fn int_str_and_truth() {
        assert_eq!(py_int(&json!("101")), Ok(101));
        assert_eq!(py_int(&json!(101.9)), Ok(101));
        assert_eq!(py_int(&json!("abc")), Err("ValueError"));
        assert_eq!(py_int(&json!(null)), Err("TypeError"));
        assert_eq!(py_str(&json!(3.0)), "3.0");
        assert_eq!(py_str_opt(None), "None");
        assert!(!truthy(Some(&json!(0))));
        assert_eq!(or(Some(&json!("")), None), Value::Null);
        assert_eq!(repr_str("chem 142"), "'chem 142'");
        assert_eq!(repr_str("it's"), "\"it's\"");
    }
}
