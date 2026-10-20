//! Python behaviors the AgentCard port must reproduce byte for byte:
//! `json.dumps` output (digests and the durable state hash these bytes),
//! `urllib.parse.urlsplit` acceptance, `float()` parsing, `os.path.expanduser`,
//! truthiness, and `re.sub` with `(?<!\w)…(?!\w)` lookarounds.

use std::fmt::Write as _;
use std::path::PathBuf;

use regex::Regex;
use serde_json::Value;

/// `json.dumps(value, sort_keys=…, ensure_ascii=…)` with the default
/// `", "` / `": "` separators.
pub fn dumps(value: &Value, sort_keys: bool, ensure_ascii: bool) -> String {
    let mut out = String::new();
    write_value(&mut out, value, sort_keys, ensure_ascii);
    out
}

fn write_value(out: &mut String, value: &Value, sort_keys: bool, ensure_ascii: bool) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                let _ = write!(out, "{i}");
            } else if let Some(u) = n.as_u64() {
                let _ = write!(out, "{u}");
            } else {
                out.push_str(&float_repr(n.as_f64().unwrap_or(f64::NAN)));
            }
        }
        Value::String(s) => write_str(out, s, ensure_ascii),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_value(out, item, sort_keys, ensure_ascii);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            if sort_keys {
                // Python sorts str keys by code point; UTF-8 byte order is the same.
                entries.sort_by(|a, b| a.0.cmp(b.0));
            }
            for (i, (k, v)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_str(out, k, ensure_ascii);
                out.push_str(": ");
                write_value(out, v, sort_keys, ensure_ascii);
            }
            out.push('}');
        }
    }
}

fn write_str(out: &mut String, s: &str, ensure_ascii: bool) {
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
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c if ensure_ascii && (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    let _ = write!(out, "\\u{:04x}", unit);
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Python `repr(float)`, as `json.dumps` writes it.
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        };
    }
    if f == 0.0 {
        return if f.is_sign_negative() {
            "-0.0".into()
        } else {
            "0.0".into()
        };
    }
    // Shortest round-trip digits, then Python's layout rule: scientific when
    // the decimal exponent is below -4 or at least 16.
    let sci = format!("{:e}", f);
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let negative = mantissa.starts_with('-');
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    let sign = if negative { "-" } else { "" };
    if !(-4..16).contains(&exp) {
        let mut m = digits[..1].to_string();
        if digits.len() > 1 {
            m.push('.');
            m.push_str(&digits[1..]);
        }
        let esign = if exp < 0 { '-' } else { '+' };
        return format!("{sign}{m}e{esign}{:02}", exp.abs());
    }
    let point = exp + 1; // digits before the decimal point
    if point <= 0 {
        format!("{sign}0.{}{}", "0".repeat((-point) as usize), digits)
    } else if (point as usize) >= digits.len() {
        format!(
            "{sign}{}{}.0",
            digits,
            "0".repeat(point as usize - digits.len())
        )
    } else {
        let (a, b) = digits.split_at(point as usize);
        format!("{sign}{a}.{b}")
    }
}

/// Python `float(str)`: surrounding whitespace allowed, `_` only between digits.
pub fn parse_float(text: &str) -> Option<f64> {
    let s = text.trim();
    if s.is_empty() {
        return None;
    }
    let cleaned = if s.contains('_') {
        let chars: Vec<char> = s.chars().collect();
        for (i, c) in chars.iter().enumerate() {
            if *c == '_' {
                let before = i
                    .checked_sub(1)
                    .map(|j| chars[j].is_ascii_digit())
                    .unwrap_or(false);
                let after = chars
                    .get(i + 1)
                    .map(|c| c.is_ascii_digit())
                    .unwrap_or(false);
                if !before || !after {
                    return None;
                }
            }
        }
        s.replace('_', "")
    } else {
        s.to_string()
    };
    let lower = cleaned.to_ascii_lowercase();
    let body = lower.trim_start_matches(['+', '-']);
    if lower.len() - body.len() > 1 {
        return None;
    }
    match body {
        "inf" | "infinity" | "nan" => {}
        _ => {
            if !body
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | '+' | '-'))
            {
                return None;
            }
        }
    }
    cleaned.parse::<f64>().ok()
}

/// Python truthiness of an optional JSON value (`None`/missing is falsy).
pub fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Python's `str` regex `\w`.
pub fn is_word(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// `re.sub(r"(?<!\w)" + re.escape(needle) + r"(?!\w)", replacement, text, flags=re.IGNORECASE)`.
pub fn sub_word(text: &str, needle: &str, replacement: &str) -> String {
    let Ok(re) = Regex::new(&format!("(?i){}", regex::escape(needle))) else {
        return text.to_string();
    };
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    let mut pos = 0;
    while pos <= text.len() {
        let Some(m) = re.find_at(text, pos) else {
            break;
        };
        let before_ok = text[..m.start()]
            .chars()
            .next_back()
            .is_none_or(|c| !is_word(c));
        let after_ok = text[m.end()..].chars().next().is_none_or(|c| !is_word(c));
        if before_ok && after_ok {
            out.push_str(&text[last..m.start()]);
            out.push_str(replacement);
            last = m.end();
            if m.end() > m.start() {
                pos = m.end();
                continue;
            }
            match text[m.end()..].chars().next() {
                Some(c) => pos = m.end() + c.len_utf8(),
                None => break,
            }
        } else {
            // Python retries one character later, so an overlapping match can still win.
            match text[m.start()..].chars().next() {
                Some(c) => pos = m.start() + c.len_utf8(),
                None => break,
            }
        }
    }
    out.push_str(&text[last..]);
    out
}

/// The parts of `urllib.parse.urlsplit` this module reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitUrl {
    pub scheme: String,
    pub netloc: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlError;

const SCHEME_CHARS: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+-.";

/// `urllib.parse.urlsplit` (Python 3.12), including its `ValueError`s for
/// unbalanced or invalid brackets and NFKC-confusable netlocs.
pub fn urlsplit(url: &str) -> Result<SplitUrl, UrlError> {
    // Leading C0 control or space is stripped, and tab/CR/LF removed.
    let trimmed = url.trim_start_matches(|c: char| (c as u32) <= 0x20);
    let url: String = trimmed
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let mut scheme = String::new();
    let mut rest: &str = &url;
    if let Some(i) = url.find(':') {
        let first = url.chars().next();
        if i > 0
            && first.is_some_and(|c| c.is_ascii_alphabetic())
            && url[..i].chars().all(|c| SCHEME_CHARS.contains(c))
        {
            scheme = url[..i].to_ascii_lowercase();
            rest = &url[i + 1..];
        }
    }
    let mut netloc = String::new();
    if rest.starts_with("//") {
        let after = &rest[2..];
        let delim = after.find(['/', '?', '#']).unwrap_or(after.len());
        netloc = after[..delim].to_string();
        rest = &after[delim..];
        let open = netloc.contains('[');
        let close = netloc.contains(']');
        if open != close {
            return Err(UrlError);
        }
        if open && close {
            check_bracketed_netloc(&netloc)?;
        }
    }
    let mut path = rest;
    if let Some(i) = path.find('#') {
        path = &path[..i];
    }
    if let Some(i) = path.find('?') {
        path = &path[..i];
    }
    check_netloc(&netloc)?;
    Ok(SplitUrl {
        scheme,
        netloc,
        path: path.to_string(),
    })
}

fn check_netloc(netloc: &str) -> Result<(), UrlError> {
    use unicode_normalization::UnicodeNormalization;
    if netloc.is_ascii() {
        return Ok(());
    }
    let n: String = netloc
        .chars()
        .filter(|c| !matches!(c, '@' | ':' | '#' | '?'))
        .collect();
    let normalized: String = n.nfkc().collect();
    if n == normalized {
        return Ok(());
    }
    if normalized.chars().any(|c| "/?#@:".contains(c)) {
        return Err(UrlError);
    }
    Ok(())
}

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
    check_bracketed_host(hostname)
}

fn check_bracketed_host(hostname: &str) -> Result<(), UrlError> {
    if hostname.starts_with('v') {
        static FUTURE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
            Regex::new(r"(?s)\Av[a-fA-F0-9]+\..+\z").expect("valid regex")
        });
        return if FUTURE.is_match(hostname) {
            Ok(())
        } else {
            Err(UrlError)
        };
    }
    if hostname.parse::<std::net::Ipv4Addr>().is_ok() {
        return Err(UrlError);
    }
    let (addr, zone) = match hostname.split_once('%') {
        Some((a, z)) => (a, Some(z)),
        None => (hostname, None),
    };
    if let Some(z) = zone {
        if z.is_empty() || z.contains('%') {
            return Err(UrlError);
        }
    }
    addr.parse::<std::net::Ipv6Addr>()
        .map(|_| ())
        .map_err(|_| UrlError)
}

impl SplitUrl {
    fn userinfo(&self) -> Option<&str> {
        self.netloc.rsplit_once('@').map(|(u, _)| u)
    }

    fn hostinfo(&self) -> (&str, Option<&str>) {
        let host_port = self
            .netloc
            .rsplit_once('@')
            .map_or(self.netloc.as_str(), |(_, h)| h);
        let (hostname, port) = match host_port.split_once('[') {
            Some((_, bracketed)) => {
                let (hostname, rest) = bracketed.split_once(']').unwrap_or((bracketed, ""));
                (hostname, rest.split_once(':').map_or("", |(_, p)| p))
            }
            None => host_port.split_once(':').unwrap_or((host_port, "")),
        };
        (hostname, if port.is_empty() { None } else { Some(port) })
    }

    pub fn username(&self) -> Option<&str> {
        self.userinfo()
            .map(|u| u.split_once(':').map_or(u, |(n, _)| n))
    }

    pub fn password(&self) -> Option<&str> {
        self.userinfo()
            .and_then(|u| u.split_once(':').map(|(_, p)| p))
    }

    pub fn hostname(&self) -> Option<String> {
        let (hostname, _) = self.hostinfo();
        if hostname.is_empty() {
            return None;
        }
        Some(match hostname.split_once('%') {
            Some((h, zone)) => format!("{}%{}", h.to_lowercase(), zone),
            None => hostname.to_lowercase(),
        })
    }

    /// `SplitResult.port`: `Err` where Python raises `ValueError`.
    pub fn port(&self) -> Result<Option<u32>, UrlError> {
        match self.hostinfo().1 {
            None => Ok(None),
            Some(p) => {
                if p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()) {
                    return Err(UrlError);
                }
                let trimmed = p.trim_start_matches('0');
                if trimmed.len() > 5 {
                    return Err(UrlError);
                }
                let n: u32 = if trimmed.is_empty() {
                    0
                } else {
                    trimmed.parse().map_err(|_| UrlError)?
                };
                if n > 65535 {
                    return Err(UrlError);
                }
                Ok(Some(n))
            }
        }
    }
}

/// `os.path.expanduser`.
pub fn expanduser(path: &str) -> PathBuf {
    if !path.starts_with('~') {
        return PathBuf::from(path);
    }
    let end = path.find('/').unwrap_or(path.len());
    let name = &path[1..end];
    let home = if name.is_empty() {
        match std::env::var_os("HOME") {
            Some(h) => Some(h.to_string_lossy().into_owned()),
            None => dirs::home_dir().map(|h| h.to_string_lossy().into_owned()),
        }
    } else {
        user_home(name)
    };
    let Some(home) = home else {
        return PathBuf::from(path);
    };
    let home = home.trim_end_matches('/');
    let joined = format!("{home}{}", &path[end..]);
    PathBuf::from(if joined.is_empty() {
        "/".to_string()
    } else {
        joined
    })
}

#[cfg(unix)]
fn user_home(name: &str) -> Option<String> {
    let cname = std::ffi::CString::new(name).ok()?;
    // SAFETY: getpwnam returns a pointer into static storage or null; we copy
    // the directory out immediately.
    unsafe {
        let pw = libc::getpwnam(cname.as_ptr());
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        Some(
            std::ffi::CStr::from_ptr((*pw).pw_dir)
                .to_string_lossy()
                .into_owned(),
        )
    }
}

#[cfg(not(unix))]
fn user_home(_name: &str) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dumps_matches_python_default_layout() {
        let v = json!({"b": [1, "é\n"], "a": null, "c": true});
        assert_eq!(
            dumps(&v, true, true),
            r#"{"a": null, "b": [1, "\u00e9\n"], "c": true}"#
        );
        assert_eq!(
            dumps(&v, false, false),
            "{\"b\": [1, \"é\\n\"], \"a\": null, \"c\": true}"
        );
        assert_eq!(dumps(&json!("😀"), false, true), r#""\ud83d\ude00""#);
    }

    #[test]
    fn float_repr_matches_python() {
        assert_eq!(float_repr(180.0), "180.0");
        assert_eq!(float_repr(1e16), "1e+16");
        assert_eq!(float_repr(1e-5), "1e-05");
        assert_eq!(float_repr(0.0001), "0.0001");
        assert_eq!(float_repr(1.5), "1.5");
        assert_eq!(float_repr(123456.789), "123456.789");
    }

    #[test]
    fn parse_float_matches_python() {
        assert_eq!(parse_float(" 120 "), Some(120.0));
        assert_eq!(parse_float("1_20"), Some(120.0));
        assert_eq!(parse_float("1__20"), None);
        assert_eq!(parse_float("bad"), None);
        assert!(parse_float("nan").unwrap().is_nan());
        assert_eq!(parse_float("inf"), Some(f64::INFINITY));
        assert_eq!(parse_float("-1"), Some(-1.0));
        assert_eq!(parse_float("1e3"), Some(1000.0));
    }

    #[test]
    fn sub_word_respects_python_lookarounds() {
        assert_eq!(sub_word("ba-a-a x", "a-a", "[r]"), "ba-[r] x");
        assert_eq!(sub_word("Ada ada adam", "ada", "[r]"), "[r] [r] adam");
        assert_eq!(
            sub_word("call +14155551234.", "+14155551234", "[r]"),
            "call [r]."
        );
    }

    #[test]
    fn urlsplit_matches_python() {
        let u = urlsplit("HTTPS://user:pw@Vault.AgentCard.sh:443/authorize?id=1#x").unwrap();
        assert_eq!(u.scheme, "https");
        assert_eq!(u.hostname().as_deref(), Some("vault.agentcard.sh"));
        assert_eq!(u.username(), Some("user"));
        assert_eq!(u.password(), Some("pw"));
        assert_eq!(u.port(), Ok(Some(443)));
        assert_eq!(u.path, "/authorize");
        assert!(urlsplit("https://[::1/x").is_err());
        assert!(urlsplit("https://[1.2.3.4]/x").is_err());
        assert!(urlsplit("https://[::1]:80/x").is_ok());
        assert!(urlsplit("https://h:x/").unwrap().port().is_err());
        assert_eq!(urlsplit("https://h:/").unwrap().port(), Ok(None));
        assert!(urlsplit("https://a\u{ff0f}b/").is_err());
    }
}
