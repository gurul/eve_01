//! Python 3.13's `urllib.parse.urlsplit`, its `username` / `password` / `hostname` / `port` properties,
//! `parse_qs` and `unquote`, as far as the vault uses them.
//!
//! Site binding is a security decision (a password must never fill on `chase.com.example`), and the Python
//! vault decides it with these exact functions. A WHATWG parser (the `url` crate) answers differently on
//! the odd inputs that matter here (an empty port, a userinfo with no name, a bare host), so the port keeps
//! Python's lenient splitter and its checks rather than a stricter one.

use std::net::Ipv6Addr;

use unicode_normalization::UnicodeNormalization;

/// A `ValueError` from the splitter or the `port` property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlError(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SplitResult {
    pub scheme: String,
    pub netloc: String,
    pub path: String,
    pub query: String,
    pub fragment: String,
}

fn is_scheme_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')
}

/// `urlsplit(url)` with no default scheme and fragments allowed.
pub fn urlsplit(url: &str) -> Result<SplitResult, UrlError> {
    // WHATWG C0 control or space is stripped from the front, and tab / CR / LF are removed everywhere.
    let url: String =
        url.trim_start_matches(|c: char| c <= ' ').chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let mut rest = url.as_str();
    let mut scheme = String::new();
    if let Some(i) = rest.find(':') {
        let first = rest.chars().next().unwrap_or(' ');
        if i > 0 && first.is_ascii_alphabetic() && rest[..i].chars().all(is_scheme_char) {
            scheme = rest[..i].to_ascii_lowercase();
            rest = &rest[i + 1..];
        }
    }
    let mut netloc = String::new();
    if rest.starts_with("//") {
        let after = &rest[2..];
        let delim = after.find(['/', '?', '#']).unwrap_or(after.len());
        netloc = after[..delim].to_string();
        rest = &after[delim..];
        let (open, close) = (netloc.contains('['), netloc.contains(']'));
        if open != close {
            return Err(UrlError("Invalid IPv6 URL".into()));
        }
        if open && close {
            check_bracketed_netloc(&netloc)?;
        }
    }
    let (rest, fragment) = match rest.split_once('#') {
        Some((a, b)) => (a, b.to_string()),
        None => (rest, String::new()),
    };
    let (path, query) = match rest.split_once('?') {
        Some((a, b)) => (a.to_string(), b.to_string()),
        None => (rest.to_string(), String::new()),
    };
    check_netloc(&netloc)?;
    Ok(SplitResult { scheme, netloc, path, query, fragment })
}

/// `_checknetloc`: a netloc whose NFKC form grows a URL delimiter is refused (a fullwidth solidus that
/// turns into `/` once some other layer normalizes it).
fn check_netloc(netloc: &str) -> Result<(), UrlError> {
    if netloc.is_ascii() {
        return Ok(());
    }
    let n: String = netloc.chars().filter(|c| !matches!(c, '@' | ':' | '#' | '?')).collect();
    let normalized: String = n.nfkc().collect();
    if n == normalized {
        return Ok(());
    }
    if normalized.chars().any(|c| matches!(c, '/' | '?' | '#' | '@' | ':')) {
        return Err(UrlError(format!(
            "netloc '{netloc}' contains invalid characters under NFKC normalization"
        )));
    }
    Ok(())
}

/// `_check_bracketed_netloc` and `_check_bracketed_host`.
fn check_bracketed_netloc(netloc: &str) -> Result<(), UrlError> {
    let host_and_port = netloc.rsplit_once('@').map_or(netloc, |(_, h)| h);
    let Some((before, bracketed)) = host_and_port.split_once('[') else {
        return Ok(());
    };
    if !before.is_empty() {
        return Err(UrlError("Invalid IPv6 URL".into()));
    }
    let (host, port) = bracketed.split_once(']').unwrap_or((bracketed, ""));
    if !port.is_empty() && !port.starts_with(':') {
        return Err(UrlError("Invalid IPv6 URL".into()));
    }
    if let Some(future) = host.strip_prefix('v') {
        // IPvFuture: v<hex>.<anything>
        let ok = future
            .split_once('.')
            .is_some_and(|(hex, tail)| !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()) && !tail.is_empty());
        if !ok {
            return Err(UrlError(format!("IPvFuture address is invalid: {host}")));
        }
        return Ok(());
    }
    let addr = host.split_once('%').map_or(host, |(a, _)| a);
    if addr.parse::<Ipv6Addr>().is_err() {
        return Err(UrlError("'".to_string() + host + "' does not appear to be an IPv4 or IPv6 address"));
    }
    Ok(())
}

impl SplitResult {
    fn userinfo(&self) -> (Option<&str>, Option<&str>) {
        match self.netloc.rsplit_once('@') {
            Some((info, _)) => match info.split_once(':') {
                Some((u, p)) => (Some(u), Some(p)),
                None => (Some(info), None),
            },
            None => (None, None),
        }
    }

    fn hostinfo(&self) -> (&str, Option<&str>) {
        let hostinfo = self.netloc.rsplit_once('@').map_or(self.netloc.as_str(), |(_, h)| h);
        let (host, port) = match hostinfo.split_once('[') {
            Some((_, bracketed)) => {
                let (host, after) = bracketed.split_once(']').unwrap_or((bracketed, ""));
                (host, after.split_once(':').map_or("", |(_, p)| p))
            }
            None => hostinfo.split_once(':').unwrap_or((hostinfo, "")),
        };
        (host, if port.is_empty() { None } else { Some(port) })
    }

    pub fn username(&self) -> Option<&str> {
        self.userinfo().0
    }

    pub fn password(&self) -> Option<&str> {
        self.userinfo().1
    }

    /// `u.username or u.password` as Python tests it: an empty name with no password is falsy.
    pub fn has_credentials(&self) -> bool {
        let (u, p) = self.userinfo();
        u.is_some_and(|s| !s.is_empty()) || p.is_some_and(|s| !s.is_empty())
    }

    /// The host, lowercased (a `%zone` keeps its case); None when there is none.
    pub fn hostname(&self) -> Option<String> {
        let (host, _) = self.hostinfo();
        if host.is_empty() {
            return None;
        }
        Some(match host.split_once('%') {
            Some((h, zone)) => format!("{}%{}", h.to_lowercase(), zone),
            None => host.to_lowercase(),
        })
    }

    /// The port: None when absent, an error when it is not ASCII digits or out of 0-65535.
    pub fn port(&self) -> Result<Option<u16>, UrlError> {
        let Some(port) = self.hostinfo().1 else {
            return Ok(None);
        };
        if !port.chars().all(|c| c.is_ascii_digit()) {
            return Err(UrlError(format!("Port could not be cast to integer value as {}", crate::pystr::repr(port))));
        }
        let digits = port.trim_start_matches('0');
        if digits.len() > 5 {
            return Err(UrlError("Port out of range 0-65535".into()));
        }
        let n: u32 = if digits.is_empty() { 0 } else { digits.parse().unwrap_or(u32::MAX) };
        u16::try_from(n).map(Some).map_err(|_| UrlError("Port out of range 0-65535".into()))
    }
}

/// `unquote(s)`: %XX sequences decoded as UTF-8, invalid bytes replaced.
pub fn unquote(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let (hi, lo) = (bytes[i + 1], bytes[i + 2]);
            if hi.is_ascii_hexdigit() && lo.is_ascii_hexdigit() {
                let nibble = |b: u8| (b as char).to_digit(16).unwrap_or(0) as u8;
                out.push(nibble(hi) << 4 | nibble(lo));
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `parse_qs(query)`: `&`-separated pairs, `+` as a space, blank values dropped; each name maps to its
/// values in order, names in order of first appearance.
pub fn parse_qs(query: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        let name = unquote(&name.replace('+', " "));
        let value = unquote(&value.replace('+', " "));
        match out.iter_mut().find(|(n, _)| *n == name) {
            Some((_, values)) => values.push(value),
            None => out.push((name, vec![value])),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlsplit_matches_python() {
        let u = urlsplit("HTTPS://user:pw@Secure.Chase.com:8443/login?x=1#f").unwrap();
        assert_eq!(u.scheme, "https");
        assert_eq!(u.netloc, "user:pw@Secure.Chase.com:8443");
        assert_eq!((u.path.as_str(), u.query.as_str(), u.fragment.as_str()), ("/login", "x=1", "f"));
        assert_eq!(u.username(), Some("user"));
        assert_eq!(u.password(), Some("pw"));
        assert_eq!(u.hostname().as_deref(), Some("secure.chase.com"));
        assert_eq!(u.port(), Ok(Some(8443)));
        let bare = urlsplit("not a url").unwrap();
        assert_eq!((bare.scheme.as_str(), bare.netloc.as_str(), bare.path.as_str()), ("", "", "not a url"));
        let js = urlsplit("javascript:alert(1)").unwrap();
        assert_eq!((js.scheme.as_str(), js.path.as_str()), ("javascript", "alert(1)"));
        assert!(urlsplit("https://chase.com:x/").unwrap().port().is_err());
        assert!(urlsplit("https://chase.com:70000/").unwrap().port().is_err());
        assert_eq!(urlsplit("https://chase.com:/").unwrap().port(), Ok(None));
        assert_eq!(urlsplit("https://[::1]:8080/").unwrap().hostname().as_deref(), Some("::1"));
        assert!(urlsplit("https://[::1/").is_err());
        assert!(urlsplit("https://[nothost]/").is_err());
        assert!(urlsplit("https://evil\u{ff0f}.com/").is_err());
        assert!(!urlsplit("https://@chase.com/").unwrap().has_credentials());
        assert!(urlsplit("https://user@chase.com/").unwrap().has_credentials());
    }

    #[test]
    fn parse_qs_and_unquote_match_python() {
        let q = parse_qs("secret=AB%3D&issuer=A+B&digits=&flag&secret=CD");
        assert_eq!(
            q,
            vec![("secret".to_string(), vec!["AB=".to_string(), "CD".to_string()]),
                 ("issuer".to_string(), vec!["A B".to_string()])]
        );
        assert_eq!(unquote("a%2"), "a%2");
        assert_eq!(unquote("%C3%A9%zz"), "é%zz");
    }
}
