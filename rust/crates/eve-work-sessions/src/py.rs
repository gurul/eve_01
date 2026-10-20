//! The few pieces of Python semantics work_sessions leans on, so both daemons read and write the same bytes:
//! `str.split()` / `strip()` / `splitlines()`, `shlex.quote`, `json.dumps` (ASCII-escaped, Python's float repr,
//! `indent=1`), `str()` / `float()` / `int()` of a JSON value, truthiness, `Path(...)` normalisation and `.name`,
//! the `OSError` subclass an errno raises, and `datetime.fromisoformat(...).timestamp()`.

use std::path::{Path, PathBuf};

use chrono::{Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use serde_json::Value;

/// `str.isspace` for one character.
pub fn is_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n'
            | '\u{0b}'
            | '\u{0c}'
            | '\r'
            | '\u{1c}'..='\u{1f}'
            | ' '
            | '\u{85}'
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
    )
}

/// `str.strip()`.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `str.rstrip()`.
pub fn rstrip(s: &str) -> &str {
    s.trim_end_matches(is_space)
}

/// `str.split()`: runs of whitespace separate, none at either end.
pub fn split(s: &str) -> Vec<&str> {
    s.split(is_space).filter(|w| !w.is_empty()).collect()
}

/// `" ".join(s.split())`.
pub fn join_words(s: &str) -> String {
    split(s).join(" ")
}

/// A line boundary for `str.splitlines()`.
fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// `str.splitlines()`: `\r\n` is one break, and a break at the very end makes no empty last line.
pub fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if is_line_break(c) {
            out.push(&s[start..i]);
            let mut end = i + c.len_utf8();
            if c == '\r' {
                if let Some(&(j, '\n')) = chars.peek() {
                    chars.next();
                    end = j + 1;
                }
            }
            start = end;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// `s[:n]`: the first `n` characters.
pub fn prefix(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// `len(s)`: characters, not bytes.
pub fn len(s: &str) -> usize {
    s.chars().count()
}

/// `shlex.quote`.
pub fn shlex_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    let safe = s.chars().all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if safe {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

// ---- json.dumps ---------------------------------------------------------------------------------------------

/// Python's `repr(float)`, as `json.dumps` writes it (`NaN`, `Infinity` for the non-finite).
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "NaN".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    // Rust's Debug is the shortest round-trip form and switches to an exponent at the same bounds as Python's
    // repr (below 1e-4, from 1e16); only the exponent's spelling differs: Python signs it and pads it to 2.
    let s = format!("{f:?}");
    match s.split_once('e') {
        Some((mantissa, exp)) => {
            let (sign, digits) = match exp.strip_prefix('-') {
                Some(d) => ('-', d),
                None => ('+', exp),
            };
            format!("{mantissa}e{sign}{digits:0>2}")
        }
        None => s,
    }
}

fn write_str(out: &mut String, s: &str) {
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
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_number(out: &mut String, n: &serde_json::Number) {
    if let Some(i) = n.as_i64() {
        out.push_str(&i.to_string());
    } else if let Some(u) = n.as_u64() {
        out.push_str(&u.to_string());
    } else {
        out.push_str(&float_repr(n.as_f64().unwrap_or(f64::NAN)));
    }
}

fn write_value(out: &mut String, v: &Value, indent: Option<usize>, depth: usize) {
    let newline = |out: &mut String, depth: usize| {
        if let Some(n) = indent {
            out.push('\n');
            out.push_str(&" ".repeat(n * depth));
        }
    };
    let item_sep = if indent.is_some() { "," } else { ", " };
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => write_number(out, n),
        Value::String(s) => write_str(out, s),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(item_sep);
                }
                newline(out, depth + 1);
                write_value(out, item, indent, depth + 1);
            }
            newline(out, depth);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(item_sep);
                }
                newline(out, depth + 1);
                write_str(out, k);
                out.push_str(": ");
                write_value(out, item, indent, depth + 1);
            }
            newline(out, depth);
            out.push('}');
        }
    }
}

/// `json.dumps(v)` (`indent` None) or `json.dumps(v, indent=n)`: ASCII-escaped, Python's separators.
pub fn dumps(v: &Value, indent: Option<usize>) -> String {
    let mut out = String::new();
    write_value(&mut out, v, indent, 0);
    out
}

// ---- str(), float(), int(), bool() of a JSON value ----------------------------------------------------------

/// `bool(v)`.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

fn repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
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
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

fn number_str(n: &serde_json::Number) -> String {
    let mut out = String::new();
    write_number(&mut out, n);
    match out.as_str() {
        "NaN" => "nan".to_string(),
        "Infinity" => "inf".to_string(),
        "-Infinity" => "-inf".to_string(),
        _ => out,
    }
}

fn repr(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Number(n) => number_str(n),
        Value::String(s) => repr_str(s),
        Value::Array(items) => format!("[{}]", items.iter().map(repr).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter().map(|(k, v)| format!("{}: {}", repr_str(k), repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// `str(v)`.
pub fn to_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => repr(other),
    }
}

/// `str(v or "")`.
pub fn str_or_empty(v: Option<&Value>) -> String {
    match v {
        Some(v) if truthy(v) => to_str(v),
        _ => String::new(),
    }
}

/// Digits with single underscores between them, as `int()` and `float()` accept them.
fn without_underscores(s: &str) -> Option<String> {
    if !s.contains('_') {
        return Some(s.to_string());
    }
    let chars: Vec<char> = s.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if *c == '_' {
            let before = i.checked_sub(1).and_then(|j| chars.get(j)).is_some_and(|c| c.is_ascii_digit());
            let after = chars.get(i + 1).is_some_and(|c| c.is_ascii_digit());
            if !(before && after) {
                return None;
            }
        }
    }
    Some(s.replace('_', ""))
}

/// `float(v)`; None where Python raises `TypeError` or `ValueError`.
pub fn to_float(v: &Value) -> Option<f64> {
    match v {
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let t = without_underscores(strip(s))?;
            let lower = t.to_ascii_lowercase();
            let body = lower.trim_start_matches(['+', '-']);
            if !matches!(body, "inf" | "infinity" | "nan") && !body.chars().any(|c| c.is_ascii_digit()) {
                return None;
            }
            t.parse::<f64>().ok()
        }
        _ => None,
    }
}

/// `float(v or 0)`.
pub fn float_or_zero(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(v) if truthy(v) => to_float(v),
        _ => Some(0.0),
    }
}

/// `int(v)`; None where Python raises (`TypeError`, `ValueError`, `OverflowError` for a non-finite float).
pub fn to_int(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Number(n) => match n.as_i64() {
            Some(i) => Some(i),
            None => n.as_f64().filter(|f| f.is_finite()).map(|f| f.trunc() as i64),
        },
        Value::String(s) => {
            let t = without_underscores(strip(s))?;
            let digits = t.strip_prefix(['+', '-']).unwrap_or(&t);
            if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            t.parse::<i64>().ok()
        }
        _ => None,
    }
}

// ---- paths -------------------------------------------------------------------------------------------------

/// `Path(p)`: pathlib's POSIX normalisation (empty and `.` parts dropped, exactly two leading slashes kept).
pub fn normalize(p: &Path) -> PathBuf {
    let raw = p.to_string_lossy();
    let root = if raw.starts_with("//") && !raw.starts_with("///") {
        "//"
    } else if raw.starts_with('/') {
        "/"
    } else {
        ""
    };
    let parts: Vec<&str> = raw.split('/').filter(|s| !s.is_empty() && *s != ".").collect();
    if root.is_empty() && parts.is_empty() {
        return PathBuf::from(".");
    }
    PathBuf::from(format!("{root}{}", parts.join("/")))
}

/// `Path(p).name`: the last part, `""` for a root or `.`.
pub fn name(p: &Path) -> String {
    let n = normalize(p);
    let s = n.to_string_lossy();
    if s == "." {
        return String::new();
    }
    s.rsplit('/').next().unwrap_or("").to_string()
}

/// The name of the `OSError` subclass Python raises for this error (`type(e).__name__`).
pub fn os_error_name(e: &std::io::Error) -> &'static str {
    let Some(code) = e.raw_os_error() else {
        return match e.kind() {
            std::io::ErrorKind::NotFound => "FileNotFoundError",
            std::io::ErrorKind::PermissionDenied => "PermissionError",
            std::io::ErrorKind::AlreadyExists => "FileExistsError",
            std::io::ErrorKind::InvalidData => "UnicodeDecodeError",
            _ => "OSError",
        };
    };
    match code {
        libc::EEXIST => "FileExistsError",
        libc::ENOENT => "FileNotFoundError",
        libc::EACCES | libc::EPERM => "PermissionError",
        libc::ENOTDIR => "NotADirectoryError",
        libc::EISDIR => "IsADirectoryError",
        libc::EINTR => "InterruptedError",
        libc::ESRCH => "ProcessLookupError",
        libc::ECHILD => "ChildProcessError",
        libc::ETIMEDOUT => "TimeoutError",
        libc::EPIPE | libc::ESHUTDOWN => "BrokenPipeError",
        libc::ECONNABORTED => "ConnectionAbortedError",
        libc::ECONNREFUSED => "ConnectionRefusedError",
        libc::ECONNRESET => "ConnectionResetError",
        libc::EAGAIN | libc::EALREADY | libc::EINPROGRESS => "BlockingIOError",
        _ => "OSError",
    }
}

// ---- datetime.fromisoformat(...).timestamp() ----------------------------------------------------------------

/// `parse_digits`: exactly `n` ASCII digits at `p`.
fn digits_at(s: &[u8], p: usize, n: usize) -> Option<u32> {
    let part = s.get(p..p + n)?;
    if !part.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(part.iter().fold(0u32, |acc, d| acc * 10 + u32::from(d - b'0')))
}

/// CPython 3.13's `parse_hh_mm_ss_ff` over `s`, where `term` is the byte just past it (`0` at the end of the
/// string, else the offset's sign). → ([hour, minute, second], microsecond, whether anything follows), None
/// where it fails. A fraction may follow the hour, the minutes or the seconds; digits past the sixth are skipped.
fn parse_hh_mm_ss_ff(s: &[u8], term: u8) -> Option<([u32; 3], u32, bool)> {
    let at = |p: usize| if p < s.len() { s[p] } else { term };
    let mut vals = [0u32; 3];
    let mut p = 0;
    let mut has_sep = true;
    for (i, val) in vals.iter_mut().enumerate() {
        *val = digits_at(s, p, 2)?;
        p += 2;
        let c = at(p);
        p += 1;
        if i == 0 {
            has_sep = c == b':';
        }
        if p >= s.len() {
            return Some((vals, 0, c != 0));
        } else if has_sep && c == b':' {
            continue;
        } else if c == b'.' || c == b',' {
            break;
        } else if !has_sep {
            p -= 1;
        } else {
            return None;
        }
    }
    let to_parse = (s.len() - p).min(6);
    if to_parse == 0 {
        return None;
    }
    let mut micro = digits_at(s, p, to_parse)?;
    p += to_parse;
    for _ in to_parse..6 {
        micro *= 10;
    }
    while at(p).is_ascii_digit() {
        p += 1;
    }
    Some((vals, micro, at(p) != 0))
}

/// `datetime.fromisoformat(stamp.replace("Z", "+00:00")).timestamp()` as CPython 3.13 (the daemon's
/// interpreter) computes it, None where it raises `ValueError`. A stamp without an offset is local time, as
/// Python reads a naive datetime. Week dates (`2026-W42-1`) are not read.
pub fn iso_timestamp(stamp: &str) -> Option<f64> {
    let s = stamp.replace('Z', "+00:00");
    let b = s.as_bytes();
    if b.len() < 7 {
        return None;
    }
    let sep_at = if b.len() == 7 {
        7
    } else if b[4] == b'-' {
        10
    } else {
        8
    };
    // The date: YYYY-MM-DD or YYYYMMDD.
    let year = digits_at(b, 0, 4)? as i32;
    let mut p = 4;
    let uses_sep = b[4] == b'-';
    if uses_sep {
        p += 1;
    }
    let month = digits_at(b, p, 2)?;
    p += 2;
    if uses_sep {
        if b.get(p) != Some(&b'-') {
            return None;
        }
        p += 1;
    }
    let day = digits_at(b, p, 2)?;
    let date = NaiveDate::from_ymd_opt(year, month, day)?;
    let (hms, micro, offset) = if sep_at >= b.len() {
        ([0, 0, 0], 0, None)
    } else {
        // Any one character separates the date from the time.
        let sep_len = s.get(sep_at..)?.chars().next()?.len_utf8();
        let t = &b[sep_at + sep_len..];
        if t.is_empty() {
            return None;
        }
        let tz_at = t.iter().position(|c| matches!(c, b'Z' | b'+' | b'-')).unwrap_or(t.len());
        let term = t.get(tz_at).copied().unwrap_or(0);
        let (hms, micro, junk) = parse_hh_mm_ss_ff(&t[..tz_at], term)?;
        if tz_at == t.len() {
            if junk {
                return None;
            }
            (hms, micro, None)
        } else {
            let sign: i64 = if t[tz_at] == b'-' { -1 } else { 1 };
            let (tz, tz_micro, tz_junk) = parse_hh_mm_ss_ff(&t[tz_at + 1..], 0)?;
            if tz_junk {
                return None;
            }
            let off_us = (i64::from(tz[0]) * 3600 + i64::from(tz[1]) * 60 + i64::from(tz[2])) * 1_000_000
                + i64::from(tz_micro);
            if off_us >= 86_400 * 1_000_000 {
                return None; // a UTC offset is strictly within a day
            }
            (hms, micro, Some(sign * off_us))
        }
    };
    let [h, m, sec] = hms;
    if h > 23 || m > 59 || sec > 59 {
        return None;
    }
    let naive = NaiveDateTime::new(date, NaiveTime::from_hms_opt(h, m, sec)?);
    match offset {
        Some(off_us) => {
            let utc_us = naive.and_utc().timestamp() * 1_000_000 + i64::from(micro) - off_us;
            Some(utc_us as f64 / 1e6)
        }
        None => {
            let local = Local.from_local_datetime(&naive).earliest()?;
            Some((local.timestamp() * 1_000_000 + i64::from(micro)) as f64 / 1e6)
        }
    }
}
