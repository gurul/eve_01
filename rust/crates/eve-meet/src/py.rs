//! The few Python semantics meet.py's behavior rests on and that no upstream crate exposes: `difflib`'s matching
//! blocks (the caption merger), `urllib.parse.urlsplit` / `parse_qsl` / `urlencode` (the link check), `format(x,
//! "g")` (the owner's messages) and the conversions of a JSON value (`float()`, `int()`, iteration) with the
//! exception Python raises when one fails.

use std::collections::HashMap;

use eve_desktop_lanes::LaneError;
use eve_vault::pystr;
use serde_json::Value;

// ---- difflib ----------------------------------------------------------------------------------------------------

/// One `difflib.Match`: `a[a..a+size] == b[b..b+size]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    pub a: usize,
    pub b: usize,
    pub size: usize,
}

/// `difflib.SequenceMatcher(None, a, b, autojunk=False).get_matching_blocks()`, the closing `(len(a), len(b), 0)`
/// included, adjacent blocks merged, in order.
pub fn matching_blocks(a: &[String], b: &[String]) -> Vec<Block> {
    // b2j: every element of b to the indices where it appears, ascending. No junk, no popular elements.
    let mut b2j: HashMap<&str, Vec<usize>> = HashMap::new();
    for (j, x) in b.iter().enumerate() {
        b2j.entry(x.as_str()).or_default().push(j);
    }
    let (la, lb) = (a.len(), b.len());
    let mut queue = vec![(0usize, la, 0usize, lb)];
    let mut found: Vec<Block> = Vec::new();
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let m = find_longest_match(a, b, &b2j, alo, ahi, blo, bhi);
        if m.size > 0 {
            found.push(m);
            if alo < m.a && blo < m.b {
                queue.push((alo, m.a, blo, m.b));
            }
            if m.a + m.size < ahi && m.b + m.size < bhi {
                queue.push((m.a + m.size, ahi, m.b + m.size, bhi));
            }
        }
    }
    found.sort_by_key(|m| (m.a, m.b, m.size));
    let (mut i1, mut j1, mut k1) = (0usize, 0usize, 0usize);
    let mut out = Vec::new();
    for m in found {
        if i1 + k1 == m.a && j1 + k1 == m.b {
            k1 += m.size;
        } else {
            if k1 > 0 {
                out.push(Block { a: i1, b: j1, size: k1 });
            }
            (i1, j1, k1) = (m.a, m.b, m.size);
        }
    }
    if k1 > 0 {
        out.push(Block { a: i1, b: j1, size: k1 });
    }
    out.push(Block { a: la, b: lb, size: 0 });
    out
}

fn find_longest_match(
    a: &[String],
    b: &[String],
    b2j: &HashMap<&str, Vec<usize>>,
    alo: usize,
    ahi: usize,
    blo: usize,
    bhi: usize,
) -> Block {
    let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
    let mut j2len: HashMap<usize, usize> = HashMap::new();
    for (i, x) in a.iter().enumerate().take(ahi).skip(alo) {
        let mut newj2len: HashMap<usize, usize> = HashMap::new();
        if let Some(js) = b2j.get(x.as_str()) {
            for &j in js {
                if j < blo {
                    continue;
                }
                if j >= bhi {
                    break;
                }
                let k = j.checked_sub(1).and_then(|p| j2len.get(&p)).copied().unwrap_or(0) + 1;
                newj2len.insert(j, k);
                if k > bestsize {
                    besti = i + 1 - k;
                    bestj = j + 1 - k;
                    bestsize = k;
                }
            }
        }
        j2len = newj2len;
    }
    // nothing is junk: extend over equal elements on both sides, as difflib does
    while besti > alo && bestj > blo && a[besti - 1] == b[bestj - 1] {
        besti -= 1;
        bestj -= 1;
        bestsize += 1;
    }
    while besti + bestsize < ahi && bestj + bestsize < bhi && a[besti + bestsize] == b[bestj + bestsize] {
        bestsize += 1;
    }
    Block { a: besti, b: bestj, size: bestsize }
}

// ---- urllib.parse -----------------------------------------------------------------------------------------------

/// What `urllib.parse.urlsplit` returns, with the attributes meet.py reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitUrl {
    pub scheme: String,
    pub netloc: String,
    pub path: String,
    pub query: String,
    pub fragment: String,
}

/// `urlsplit` raised `ValueError` (unbalanced or bad brackets, an NFKC-confusable netloc).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlError;

const SCHEME_CHARS: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+-.";

/// `urllib.parse.urlsplit(url)` (Python 3.11+).
pub fn urlsplit(url: &str) -> Result<SplitUrl, UrlError> {
    let url = url.trim_start_matches(|c: char| (c as u32) <= 0x20);
    let url: String = url.chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let mut scheme = String::new();
    let mut rest: &str = &url;
    if let Some(i) = url.find(':') {
        let first = url.chars().next();
        if i > 0 && first.is_some_and(|c| c.is_ascii_alphabetic()) && url[..i].chars().all(|c| SCHEME_CHARS.contains(c))
        {
            scheme = url[..i].to_ascii_lowercase();
            rest = &url[i + 1..];
        }
    }
    let mut netloc = String::new();
    if let Some(after) = rest.strip_prefix("//") {
        let delim = after.find(['/', '?', '#']).unwrap_or(after.len());
        netloc = after[..delim].to_string();
        rest = &after[delim..];
        let (open, close) = (netloc.contains('['), netloc.contains(']'));
        if open != close {
            return Err(UrlError);
        }
        if open && close {
            check_bracketed_netloc(&netloc)?;
        }
    }
    let (mut path, mut fragment) = (rest, "");
    if let Some((p, f)) = path.split_once('#') {
        (path, fragment) = (p, f);
    }
    let mut query = "";
    if let Some((p, q)) = path.split_once('?') {
        (path, query) = (p, q);
    }
    check_netloc(&netloc)?;
    Ok(SplitUrl { scheme, netloc, path: path.into(), query: query.into(), fragment: fragment.into() })
}

/// `_checknetloc`: a non-ASCII netloc whose NFKC form gains one of `/?#@:` is refused.
fn check_netloc(netloc: &str) -> Result<(), UrlError> {
    use unicode_normalization::UnicodeNormalization;
    if netloc.is_ascii() {
        return Ok(());
    }
    let n: String = netloc.chars().filter(|c| !matches!(c, '@' | ':' | '#' | '?')).collect();
    let normalized: String = n.nfkc().collect();
    if n == normalized {
        return Ok(());
    }
    if normalized.chars().any(|c| "/?#@:".contains(c)) {
        return Err(UrlError);
    }
    Ok(())
}

/// `_check_bracketed_netloc`: the bracketed host must be an IP address (or a `v…` IPvFuture).
fn check_bracketed_netloc(netloc: &str) -> Result<(), UrlError> {
    let host_port = netloc.rsplit_once('@').map_or(netloc, |(_, h)| h);
    let hostname = match host_port.split_once('[') {
        Some((before, bracketed)) => {
            if !before.is_empty() {
                return Err(UrlError);
            }
            let (hostname, port) = bracketed.split_once(']').unwrap_or((bracketed, ""));
            if !port.is_empty() && !port.starts_with(':') {
                return Err(UrlError);
            }
            hostname
        }
        None => host_port.split_once(':').map_or(host_port, |(h, _)| h),
    };
    // `_check_bracketed_host`
    if let Some(rest) = hostname.strip_prefix('v') {
        // IPvFuture: `\Av[a-fA-F0-9]+\..+\Z` (the newlines `.` would refuse are gone before the split)
        let ok = rest.split_once('.').is_some_and(|(hex, tail)| {
            !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()) && !tail.is_empty()
        });
        return if ok { Ok(()) } else { Err(UrlError) };
    }
    // `ipaddress.ip_address`: an IPv6 address, with an optional non-empty `%scope` holding no `%`; an IPv4
    // address parses but "cannot be in brackets"
    if let Some((addr, scope)) = hostname.split_once('%') {
        let ok = !scope.is_empty() && !scope.contains('%') && addr.parse::<std::net::Ipv6Addr>().is_ok();
        return if ok { Ok(()) } else { Err(UrlError) };
    }
    if hostname.parse::<std::net::Ipv6Addr>().is_ok() {
        Ok(())
    } else {
        Err(UrlError)
    }
}

impl SplitUrl {
    /// `(userinfo, hostinfo)` of `netloc.rpartition('@')`.
    fn userinfo(&self) -> (Option<&str>, &str) {
        match self.netloc.rsplit_once('@') {
            Some((u, h)) => (Some(u), h),
            None => (None, self.netloc.as_str()),
        }
    }

    /// `.username`.
    pub fn username(&self) -> Option<String> {
        self.userinfo().0.map(|u| u.split_once(':').map_or(u, |(n, _)| n).to_string())
    }

    /// `.password`.
    pub fn password(&self) -> Option<String> {
        self.userinfo().0.and_then(|u| u.split_once(':').map(|(_, p)| p.to_string()))
    }

    /// `_hostinfo`: (hostname, port text), an empty port being None.
    fn hostinfo(&self) -> (String, Option<String>) {
        let (_, hostinfo) = self.userinfo();
        let (hostname, port) = match hostinfo.split_once('[') {
            Some((_, bracketed)) => {
                let (h, after) = bracketed.split_once(']').unwrap_or((bracketed, ""));
                (h, after.split_once(':').map_or("", |(_, p)| p))
            }
            None => hostinfo.split_once(':').unwrap_or((hostinfo, "")),
        };
        (hostname.to_string(), (!port.is_empty()).then(|| port.to_string()))
    }

    /// `.hostname`: lowercased up to a `%` zone, None when empty.
    pub fn hostname(&self) -> Option<String> {
        let (h, _) = self.hostinfo();
        if h.is_empty() {
            return None;
        }
        Some(match h.split_once('%') {
            Some((host, zone)) => format!("{}%{zone}", host.to_lowercase()),
            None => h.to_lowercase(),
        })
    }

    /// `.port`: Ok(None) without one; Err where Python raises `ValueError` (not ASCII digits, or out of range).
    pub fn port(&self) -> Result<Option<u32>, String> {
        let Some(p) = self.hostinfo().1 else { return Ok(None) };
        if !p.bytes().all(|c| c.is_ascii_digit()) {
            return Err(format!("Port could not be cast to integer value as {}", pystr::repr(&p)));
        }
        let trimmed = p.trim_start_matches('0');
        if trimmed.len() > 5 {
            return Err("Port out of range 0-65535".into());
        }
        let n: u32 = if trimmed.is_empty() { 0 } else { trimmed.parse().unwrap_or(u32::MAX) };
        if n > 65535 {
            return Err("Port out of range 0-65535".into());
        }
        Ok(Some(n))
    }
}

/// `urllib.parse.unquote(s.replace('+', ' '))`: `%XX` decoded as UTF-8 with `errors='replace'`.
fn unquote_plus(s: &str) -> String {
    let s = s.replace('+', " ");
    if !s.contains('%') {
        return s;
    }
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let (h, l) = (bytes[i + 1], bytes[i + 2]);
            if h.is_ascii_hexdigit() && l.is_ascii_hexdigit() {
                let digit = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
                out.push(digit(h) * 16 + digit(l));
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `urllib.parse.parse_qsl(query)`: `&`-separated, a field without `=` or with an empty value dropped.
pub fn parse_qsl(query: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for field in query.split('&') {
        if field.is_empty() {
            continue;
        }
        let Some((name, value)) = field.split_once('=') else { continue };
        if value.is_empty() {
            continue;
        }
        out.push((unquote_plus(name), unquote_plus(value)));
    }
    out
}

/// `urllib.parse.quote_plus(s, safe='')`.
pub fn quote_plus(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `urllib.parse.urlencode(pairs)`.
pub fn urlencode(pairs: &[(String, String)]) -> String {
    pairs.iter().map(|(k, v)| format!("{}={}", quote_plus(k), quote_plus(v))).collect::<Vec<_>>().join("&")
}

// ---- format(x, "g") ---------------------------------------------------------------------------------------------

/// `f"{x:g}"`: six significant digits, trailing zeros dropped, exponent form below 1e-4 and from 1e6.
pub fn fmt_g(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let sci = format!("{x:.5e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let strip = |s: &str| -> String {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s.to_string()
        }
    };
    if (-4..6).contains(&exp) {
        let decimals = (5 - exp).max(0) as usize;
        strip(&format!("{x:.decimals$}"))
    } else {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", strip(mantissa), exp.abs())
    }
}

// ---- JSON values as Python sees them ----------------------------------------------------------------------------

/// `type(v).__name__` for a JSON value.
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

/// `float(v)`, or the `ValueError` / `TypeError` Python raises.
pub fn float_of(v: &Value) -> anyhow::Result<f64> {
    match v {
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => Ok(n.as_f64().unwrap_or(f64::NAN)),
        Value::String(s) => eve_notes::py_float(s)
            .ok_or_else(|| LaneError::err("ValueError", format!("could not convert string to float: {}", pystr::repr(s)))),
        other => Err(LaneError::err(
            "TypeError",
            format!("float() argument must be a string or a real number, not '{}'", type_name(other)),
        )),
    }
}

/// `int(v)`, or the `ValueError` / `TypeError` Python raises.
pub fn int_of(v: &Value) -> anyhow::Result<i128> {
    if let Some(n) = pystr::int_of_value(v) {
        return Ok(n);
    }
    Err(match v {
        Value::String(s) => {
            LaneError::err("ValueError", format!("invalid literal for int() with base 10: {}", pystr::repr(s)))
        }
        Value::Number(_) => LaneError::err("OverflowError", "cannot convert float infinity to integer"),
        other => LaneError::err(
            "TypeError",
            format!(
                "int() argument must be a string, a bytes-like object or a real number, not '{}'",
                type_name(other)
            ),
        ),
    })
}

/// `for x in v`: a list's items, a dict's keys, a string's characters; anything else is not iterable.
pub fn iterate(v: &Value) -> anyhow::Result<Vec<Value>> {
    match v {
        Value::Array(items) => Ok(items.clone()),
        Value::Object(map) => Ok(map.keys().map(|k| Value::String(k.clone())).collect()),
        Value::String(s) => Ok(s.chars().map(|c| Value::String(c.to_string())).collect()),
        other => Err(LaneError::err("TypeError", format!("'{}' object is not iterable", type_name(other)))),
    }
}

/// `x.get(...)` on something that is not a dict.
pub fn no_get(v: &Value) -> anyhow::Error {
    LaneError::err("AttributeError", format!("'{}' object has no attribute 'get'", type_name(v)))
}

/// `v or default` iterated: a falsy value is the empty list.
pub fn iterate_or_empty(v: Option<&Value>) -> anyhow::Result<Vec<Value>> {
    match v {
        Some(v) if pystr::truthy(v) => iterate(v),
        _ => Ok(Vec::new()),
    }
}

/// `str(e)` for an OSError: `[Errno N] text: 'path'`, with the class name as the kind.
pub fn os_error(e: &std::io::Error, path: &std::path::Path) -> anyhow::Error {
    let kind = eve_transcripts::py::os_error_name(e);
    let message = match e.raw_os_error() {
        Some(code) => {
            let text = std::io::Error::from_raw_os_error(code).to_string();
            let text = text.split(" (os error").next().unwrap_or(&text).to_string();
            format!("[Errno {code}] {text}: {}", pystr::repr(&path.display().to_string()))
        }
        None => e.to_string(),
    };
    LaneError::err(kind, message)
}

/// `type(e).__name__`: the Python class an error stands for.
pub fn error_name(e: &anyhow::Error) -> String {
    if e.chain().any(|c| c.downcast_ref::<LaneError>().is_some()) {
        return eve_desktop_lanes::error_kind(e);
    }
    eve_notes::error_name(e)
}

/// `str(e)`.
pub fn error_text(e: &anyhow::Error) -> String {
    eve_desktop_lanes::error_message(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn matching_blocks_follow_difflib() {
        // difflib.SequenceMatcher(None, "a b c d".split(), "b c x d".split(), autojunk=False)
        let got = matching_blocks(&words("a b c d"), &words("b c x d"));
        assert_eq!(got, vec![Block { a: 1, b: 0, size: 2 }, Block { a: 3, b: 3, size: 1 }, Block { a: 4, b: 4, size: 0 }]);
        let got = matching_blocks(&words("x y"), &words("p q"));
        assert_eq!(got, vec![Block { a: 2, b: 2, size: 0 }]);
        // adjacent blocks found separately are merged
        let got = matching_blocks(&words("a b a b"), &words("a b a b"));
        assert_eq!(got, vec![Block { a: 0, b: 0, size: 4 }, Block { a: 4, b: 4, size: 0 }]);
    }

    #[test]
    fn urlsplit_reads_what_python_reads() {
        let u = urlsplit("https://User:pw@Meet.Google.com:443/abc?authuser=1#frag").unwrap();
        assert_eq!(u.scheme, "https");
        assert_eq!(u.username().as_deref(), Some("User"));
        assert_eq!(u.password().as_deref(), Some("pw"));
        assert_eq!(u.hostname().as_deref(), Some("meet.google.com"));
        assert_eq!(u.port(), Ok(Some(443)));
        assert_eq!((u.path.as_str(), u.query.as_str(), u.fragment.as_str()), ("/abc", "authuser=1", "frag"));
        assert!(urlsplit("https://[::1/x").is_err());
        // _check_bracketed_host, as Python 3.13 answers each
        assert!(urlsplit("https://[::1]/x").is_ok());
        assert!(urlsplit("https://[1.2.3.4]/x").is_err());
        assert!(urlsplit("https://[v1.a b]/x").is_ok());
        assert!(urlsplit("https://[v1.é]/x").is_ok());
        assert!(urlsplit("https://[fe80::1%25eth0]/x").is_ok());
        assert!(urlsplit("https://[fe80::1%]/x").is_err());
        assert!(urlsplit("https://[::1%a%b]/x").is_err());
        assert!(urlsplit("https://[1.2.3.4%x]/x").is_err());
        assert!(urlsplit("https://meet.google.com:x/").unwrap().port().is_err());
        assert_eq!(urlsplit("https://meet.google.com:/").unwrap().port(), Ok(None));
        let at = urlsplit("https://@meet.google.com/").unwrap();
        assert_eq!(at.username().as_deref(), Some(""));
        assert_eq!(at.password(), None);
    }

    #[test]
    fn query_helpers_match_urllib() {
        assert_eq!(
            parse_qsl("authuser=a%40b.com&pli=1&x&y=&&z=1+2"),
            vec![("authuser".into(), "a@b.com".into()), ("pli".into(), "1".into()), ("z".into(), "1 2".into())]
        );
        assert_eq!(urlencode(&[("authuser".into(), "a@b.com".into()), ("hl".into(), "en".into())]), "authuser=a%40b.com&hl=en");
        assert_eq!(quote_plus("a b+c~é"), "a+b%2Bc~%C3%A9");
    }

    #[test]
    fn fmt_g_is_pythons() {
        for (x, s) in [(2.0, "2"), (15.0, "15"), (0.25, "0.25"), (1.5, "1.5"), (12.3, "12.3"), (0.0, "0"),
                       (123456.7, "123457"), (1234567.0, "1.23457e+06"), (0.00001, "1e-05"), (120.0, "120")] {
            assert_eq!(fmt_g(x), s, "{x}");
        }
    }

    #[test]
    fn conversions_raise_as_python_does() {
        assert_eq!(float_of(&Value::from("1.5")).unwrap(), 1.5);
        assert_eq!(error_text(&float_of(&Value::from("x")).unwrap_err()), "could not convert string to float: 'x'");
        assert_eq!(error_name(&float_of(&serde_json::json!([1])).unwrap_err()), "TypeError");
        assert_eq!(int_of(&Value::from(" 7 ")).unwrap(), 7);
        assert_eq!(int_of(&Value::from(7.9)).unwrap(), 7);
        assert_eq!(error_name(&int_of(&Value::Null).unwrap_err()), "TypeError");
        assert_eq!(iterate(&Value::from("ab")).unwrap(), vec![Value::from("a"), Value::from("b")]);
        assert!(iterate(&Value::from(3)).is_err());
    }
}
