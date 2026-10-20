//! `urllib.parse` as Python 3.13 has it, for the three calls plan_contract makes: `urlsplit` (with the
//! `hostname`, `port`, `username` and `password` properties), `unquote` and `parse_qsl(keep_blank_values=True)`.
//! Every `ValueError` urlsplit or `.port` raises is an `Err(())` here.

use std::net::{Ipv4Addr, Ipv6Addr};

use unicode_normalization::UnicodeNormalization;

const SCHEME_CHARS: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+-.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SplitResult {
    pub scheme: String,
    pub netloc: String,
    pub path: String,
    pub query: String,
    pub fragment: String,
}

/// `s.partition(sep)`.
fn partition<'a>(s: &'a str, sep: &str) -> (&'a str, bool, &'a str) {
    match s.find(sep) {
        Some(i) => (&s[..i], true, &s[i + sep.len()..]),
        None => (s, false, ""),
    }
}

/// `s.rpartition(sep)`.
fn rpartition<'a>(s: &'a str, sep: &str) -> (&'a str, bool, &'a str) {
    match s.rfind(sep) {
        Some(i) => (&s[..i], true, &s[i + sep.len()..]),
        None => ("", false, s),
    }
}

/// `ipaddress.ip_address(text)`: Some(true) for IPv4, Some(false) for IPv6 (with an optional %scope), None when
/// it is neither.
fn ip_address(text: &str) -> Option<bool> {
    if text.parse::<Ipv4Addr>().is_ok() {
        return Some(true);
    }
    let (addr, has_scope, scope) = partition(text, "%");
    if has_scope && (scope.is_empty() || scope.contains('%')) {
        return None;
    }
    addr.parse::<Ipv6Addr>().ok().map(|_| false)
}

fn check_bracketed_host(hostname: &str) -> Result<(), ()> {
    if let Some(rest) = hostname.strip_prefix('v') {
        // \Av[a-fA-F0-9]+\..+\Z
        let hex = rest.chars().take_while(|c| c.is_ascii_hexdigit()).count();
        let after = &rest[hex..];
        let ok = hex > 0 && after.starts_with('.') && after.len() > 1 && !after[1..].contains('\n');
        return if ok { Ok(()) } else { Err(()) };
    }
    match ip_address(hostname) {
        Some(false) => Ok(()),
        _ => Err(()),
    }
}

fn check_bracketed_netloc(netloc: &str) -> Result<(), ()> {
    let hostname_and_port = rpartition(netloc, "@").2;
    let (before_bracket, have_open_br, bracketed) = partition(hostname_and_port, "[");
    let hostname = if have_open_br {
        if !before_bracket.is_empty() {
            return Err(());
        }
        let (hostname, _, port) = partition(bracketed, "]");
        if !port.is_empty() && !port.starts_with(':') {
            return Err(());
        }
        hostname
    } else {
        partition(hostname_and_port, ":").0
    };
    check_bracketed_host(hostname)
}

fn check_netloc(netloc: &str) -> Result<(), ()> {
    if netloc.is_empty() || netloc.is_ascii() {
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
        return Err(());
    }
    Ok(())
}

/// `urllib.parse.urlsplit(url)`.
pub(crate) fn urlsplit(url: &str) -> Result<SplitResult, ()> {
    let url = url.trim_start_matches(|c: char| (c as u32) <= 0x20);
    let url: String = url
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let mut scheme = String::new();
    let mut rest: &str = &url;
    if let Some(i) = rest.find(':') {
        let first = rest.chars().next();
        if i > 0
            && first.is_some_and(|c| c.is_ascii_alphabetic())
            && rest[..i].chars().all(|c| SCHEME_CHARS.contains(c))
        {
            scheme = rest[..i].to_ascii_lowercase();
            rest = &rest[i + 1..];
        }
    }
    let mut netloc = String::new();
    if rest.starts_with("//") {
        let delim = ['/', '?', '#']
            .iter()
            .filter_map(|c| rest[2..].find(*c).map(|i| i + 2))
            .min()
            .unwrap_or(rest.len());
        netloc = rest[2..delim].to_string();
        rest = &rest[delim..];
        let (open, close) = (netloc.contains('['), netloc.contains(']'));
        if open != close {
            return Err(());
        }
        if open && close {
            check_bracketed_netloc(&netloc)?;
        }
    }
    let (mut path, mut query, mut fragment) = (rest.to_string(), String::new(), String::new());
    if let Some((before, after)) = path.split_once('#') {
        fragment = after.to_string();
        path = before.to_string();
    }
    if let Some((before, after)) = path.split_once('?') {
        query = after.to_string();
        path = before.to_string();
    }
    check_netloc(&netloc)?;
    Ok(SplitResult {
        scheme,
        netloc,
        path,
        query,
        fragment,
    })
}

impl SplitResult {
    /// `_hostinfo`: (hostname, port) as written, port None when empty.
    fn hostinfo(&self) -> (&str, Option<&str>) {
        let hostinfo = rpartition(&self.netloc, "@").2;
        let (_, have_open_br, bracketed) = partition(hostinfo, "[");
        let (hostname, port) = if have_open_br {
            let (hostname, _, port) = partition(bracketed, "]");
            (hostname, partition(port, ":").2)
        } else {
            let (hostname, _, port) = partition(hostinfo, ":");
            (hostname, port)
        };
        (hostname, if port.is_empty() { None } else { Some(port) })
    }

    /// `.hostname`: lowercased, a scoped IPv6 address's zone kept as written; None when empty.
    pub fn hostname(&self) -> Option<String> {
        let hostname = self.hostinfo().0;
        if hostname.is_empty() {
            return None;
        }
        let (host, percent, zone) = partition(hostname, "%");
        Some(format!(
            "{}{}{}",
            host.to_lowercase(),
            if percent { "%" } else { "" },
            zone
        ))
    }

    /// `.port`: Err where Python raises ValueError (not ASCII digits, or outside 0..=65535).
    pub fn port(&self) -> Result<Option<u32>, ()> {
        let Some(port) = self.hostinfo().1 else {
            return Ok(None);
        };
        if !port.chars().all(|c| c.is_ascii_digit()) {
            return Err(());
        }
        let digits = port.trim_start_matches('0');
        if digits.len() > 5 {
            return Err(());
        }
        let n: u32 = if digits.is_empty() {
            0
        } else {
            digits.parse().map_err(|_| ())?
        };
        if n > 65535 {
            return Err(());
        }
        Ok(Some(n))
    }

    /// `(.username, .password)`.
    pub fn userinfo(&self) -> (Option<&str>, Option<&str>) {
        let (userinfo, have_info, _) = rpartition(&self.netloc, "@");
        if !have_info {
            return (None, None);
        }
        let (username, have_password, password) = partition(userinfo, ":");
        (
            Some(username),
            if have_password { Some(password) } else { None },
        )
    }
}

/// `_unquote_impl` on an ASCII run: `%XX` with two hex digits is that byte, any other `%` stays.
fn unquote_bytes(run: &str) -> Vec<u8> {
    let bytes = run.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit()
        {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("00");
            out.push(u8::from_str_radix(hex, 16).unwrap_or(0));
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

/// `urllib.parse.unquote(text)`: each ASCII run is percent-decoded and read as UTF-8 (invalid bytes become
/// U+FFFD); characters outside ASCII pass through.
pub(crate) fn unquote(text: &str) -> String {
    if !text.contains('%') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut run_start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        match (c.is_ascii(), run_start) {
            (true, None) => run_start = Some(i),
            (false, Some(start)) => {
                out.push_str(&String::from_utf8_lossy(&unquote_bytes(&text[start..i])));
                run_start = None;
                out.push(c);
            }
            (false, None) => out.push(c),
            (true, Some(_)) => {}
        }
    }
    if let Some(start) = run_start {
        out.push_str(&String::from_utf8_lossy(&unquote_bytes(&text[start..])));
    }
    out
}

/// `urllib.parse.parse_qsl(qs, keep_blank_values=True)`.
pub(crate) fn parse_qsl(qs: &str) -> Vec<(String, String)> {
    qs.split('&')
        .filter(|field| !field.is_empty())
        .map(|field| {
            let (name, _, value) = partition(field, "=");
            (
                unquote(&name.replace('+', " ")),
                unquote(&value.replace('+', " ")),
            )
        })
        .collect()
}
