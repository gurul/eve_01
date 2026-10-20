//! The slices of Python's `str`, `json`, `datetime` and `os.path` behavior these modules depend on, so the Rust
//! store writes the bytes the Python writes and reads every line the Python reads.

use std::io;
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, FixedOffset, Local, NaiveDate, NaiveDateTime, NaiveTime, Offset, TimeZone};
use serde_json::Value;

// ---- str ----------------------------------------------------------------------------------------------------------

/// `str.isspace()` for one character: Unicode White_Space plus the four ASCII separators Python also counts.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `" ".join(text.split())`: runs of whitespace become one space, none at either end.
pub fn flat(text: &str) -> String {
    text.split(is_space)
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `str.rstrip()`.
pub fn rstrip(text: &str) -> &str {
    text.trim_end_matches(is_space)
}

/// `str.strip()`.
pub fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// `len(text)`: code points.
pub fn len(text: &str) -> usize {
    text.chars().count()
}

/// `text[:n]` in code points.
pub fn head(text: &str, n: usize) -> &str {
    match text.char_indices().nth(n) {
        Some((i, _)) => &text[..i],
        None => text,
    }
}

/// `text if len(text) <= cap else text[: max(0, cap - 1)].rstrip() + "…"`.
pub fn clip(text: &str, cap: usize) -> String {
    if len(text) <= cap {
        return text.to_string();
    }
    format!("{}…", rstrip(head(text, cap.saturating_sub(1))))
}

/// `int(s)` for a run of decimal digits (what a `\d+` match holds): any Unicode `Nd` digit counts, as in Python.
/// None for anything else or a value past i64.
pub fn int(s: &str) -> Option<i64> {
    if s.is_empty() {
        return None;
    }
    let mut n: i64 = 0;
    for c in s.chars() {
        n = n.checked_mul(10)?.checked_add(digit_value(c)? as i64)?;
    }
    Some(n)
}

static ND: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"^\p{Nd}$").expect("the Nd pattern"));

fn is_nd(c: char) -> bool {
    let mut buf = [0u8; 4];
    ND.is_match(c.encode_utf8(&mut buf))
}

/// The value of one decimal digit. Unicode lays every `Nd` set out as ten consecutive code points from 0 to 9, so
/// the value is the count of `Nd` code points just before it, modulo ten.
fn digit_value(c: char) -> Option<u32> {
    if c.is_ascii_digit() {
        return Some(c as u32 - '0' as u32);
    }
    if !is_nd(c) {
        return None;
    }
    let mut run = 0u32;
    let mut cp = c as u32;
    while cp > 0 {
        match char::from_u32(cp - 1) {
            Some(p) if is_nd(p) => {
                run += 1;
                cp -= 1;
            }
            _ => break,
        }
    }
    Some(run % 10)
}

/// `str.casefold()`.
pub fn casefold(text: &str) -> String {
    caseless::default_case_fold_str(text)
}

/// `str.splitlines()`: every line boundary Python knows (`\r\n` is one).
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let boundary = matches!(
            c,
            '\n' | '\r'
                | '\u{0b}'
                | '\u{0c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if !boundary {
            continue;
        }
        out.push(&text[start..i]);
        let mut end = i + c.len_utf8();
        if c == '\r' {
            if let Some(&(j, '\n')) = chars.peek() {
                chars.next();
                end = j + 1;
            }
        }
        start = end;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// What reading a file in text mode gives (`open(path, encoding="utf-8", errors="replace").read()`): invalid
/// UTF-8 replaced, and universal newlines (`\r\n` and `\r` read as `\n`).
pub fn read_text(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    if !s.contains('\r') {
        return s.into_owned();
    }
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// Python's truthiness of a decoded JSON value.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

// ---- json ---------------------------------------------------------------------------------------------------------

/// `json.dumps(value, ensure_ascii=False)`: `", "` and `": "` separators, non-ASCII kept raw, floats in Python's repr.
pub fn dumps(v: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, v);
    out
}

fn write_value(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => {
            if n.is_f64() {
                out.push_str(&float_repr(n.as_f64().unwrap_or(0.0)));
            } else {
                out.push_str(&n.to_string());
            }
        }
        Value::String(s) => write_str(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_value(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_str(out, k);
                out.push_str(": ");
                write_value(out, item);
            }
            out.push('}');
        }
    }
}

fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `repr(float)` as `json.dumps` writes it.
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        };
    }
    if x == 0.0 {
        return if x.is_sign_negative() {
            "-0.0".into()
        } else {
            "0.0".into()
        };
    }
    let sci = format!("{:e}", x.abs());
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let sign = if x < 0.0 { "-" } else { "" };
    if (-4..16).contains(&exp) {
        if exp >= 0 {
            let int_len = exp as usize + 1;
            if digits.len() <= int_len {
                format!("{sign}{digits}{}.0", "0".repeat(int_len - digits.len()))
            } else {
                format!("{sign}{}.{}", &digits[..int_len], &digits[int_len..])
            }
        } else {
            format!("{sign}0.{}{digits}", "0".repeat((-exp - 1) as usize))
        }
    } else {
        let frac = if digits.len() > 1 {
            format!(".{}", &digits[1..])
        } else {
            String::new()
        };
        let esign = if exp < 0 { '-' } else { '+' };
        format!("{sign}{}{frac}e{esign}{:02}", &digits[..1], exp.abs())
    }
}

/// `json.loads` for one line: strict JSON plus the bare `NaN`, `Infinity` and `-Infinity` Python accepts (read
/// back as `null`, which a JSON value can hold). `None` where Python raises `ValueError`.
pub fn loads(s: &str) -> Option<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(s) {
        return Some(v);
    }
    let replaced = replace_nonfinite(s)?;
    serde_json::from_str::<Value>(&replaced).ok()
}

fn replace_nonfinite(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    let mut changed = false;
    let mut in_str = false;
    let mut escaped = false;
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        if in_str {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            rest = &rest[c.len_utf8()..];
            continue;
        }
        if let Some(t) = ["-Infinity", "Infinity", "NaN"]
            .into_iter()
            .find(|t| rest.starts_with(t))
        {
            out.push_str("null");
            rest = &rest[t.len()..];
            changed = true;
            continue;
        }
        if c == '"' {
            in_str = true;
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    changed.then_some(out)
}

// ---- datetime -----------------------------------------------------------------------------------------------------

/// `datetime.fromisoformat(s)` (Python 3.11): the wall-clock time and its UTC offset, if it carries one.
/// Dates `YYYY-MM-DD` or `YYYYMMDD`; any single separator; times `HH[:MM[:SS[.f…]]]` or `HHMM[SS]`; offsets `Z`,
/// `±HH[:MM[:SS[.ffffff]]]` or `±HHMM[SS]`.
pub fn fromisoformat(s: &str) -> Option<(NaiveDateTime, Option<FixedOffset>)> {
    let b = s.as_bytes();
    // Every slice below is taken with `get`: a non-ASCII character where a digit belongs is a ValueError in
    // Python, never a panic here.
    let (date, rest) = if b.len() >= 10 && b[4] == b'-' && b[7] == b'-' {
        (
            date_of(s.get(0..4)?, s.get(5..7)?, s.get(8..10)?)?,
            s.get(10..)?,
        )
    } else if b.len() >= 8 && b[..8].iter().all(u8::is_ascii_digit) {
        (date_of(&s[0..4], &s[4..6], &s[6..8])?, &s[8..])
    } else {
        return None;
    };
    let mut it = rest.chars();
    let Some(_sep) = it.next() else {
        return Some((date.and_time(NaiveTime::MIN), None));
    };
    let time_part = it.as_str();
    let tz_at = time_part.find(['Z', '+', '-']);
    let (clock, tz) = match tz_at {
        Some(i) => (&time_part[..i], Some(&time_part[i..])),
        None => (time_part, None),
    };
    let time = time_of(clock)?;
    let offset = match tz {
        None => None,
        Some("Z") => Some(FixedOffset::east_opt(0)?),
        Some(t) => Some(offset_of(t)?),
    };
    Some((date.and_time(time), offset))
}

fn digits(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn date_of(y: &str, m: &str, d: &str) -> Option<NaiveDate> {
    let year = digits(y)? as i32;
    if year < 1 {
        return None;
    }
    NaiveDate::from_ymd_opt(year, digits(m)?, digits(d)?)
}

fn time_of(clock: &str) -> Option<NaiveTime> {
    let (hms, frac) = match clock.find(['.', ',']) {
        Some(i) => (&clock[..i], Some(&clock[i + 1..])),
        None => (clock, None),
    };
    let parts: Vec<&str> = if hms.contains(':') {
        hms.split(':').collect()
    } else {
        if !hms.is_ascii() || hms.len() % 2 != 0 || hms.len() > 6 || hms.is_empty() {
            return None;
        }
        (0..hms.len()).step_by(2).map(|i| &hms[i..i + 2]).collect()
    };
    if parts.is_empty() || parts.len() > 3 || parts.iter().any(|p| p.len() != 2) {
        return None;
    }
    let h = digits(parts[0])?;
    let m = parts.get(1).map(|p| digits(p)).unwrap_or(Some(0))?;
    let sec = parts.get(2).map(|p| digits(p)).unwrap_or(Some(0))?;
    let micro = match frac {
        None => 0,
        Some(f) => {
            if parts.len() != 3 || f.is_empty() || !f.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let six: String = f.chars().chain(std::iter::repeat('0')).take(6).collect();
            six.parse().ok()?
        }
    };
    NaiveTime::from_hms_micro_opt(h, m, sec, micro)
}

fn offset_of(t: &str) -> Option<FixedOffset> {
    let sign = match t.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let body = &t[1..];
    let body = match body.find(['.', ',']) {
        Some(i) => &body[..i],
        None => body,
    };
    let parts: Vec<&str> = if body.contains(':') {
        body.split(':').collect()
    } else {
        if !body.is_ascii() || body.is_empty() || body.len() % 2 != 0 || body.len() > 6 {
            return None;
        }
        (0..body.len())
            .step_by(2)
            .map(|i| &body[i..i + 2])
            .collect()
    };
    if parts.is_empty() || parts.len() > 3 || parts.iter().any(|p| p.len() != 2) {
        return None;
    }
    let h = digits(parts[0])? as i32;
    let m = parts.get(1).map(|p| digits(p)).unwrap_or(Some(0))? as i32;
    let s = parts.get(2).map(|p| digits(p)).unwrap_or(Some(0))? as i32;
    if h > 23 || m > 59 || s > 59 {
        return None;
    }
    FixedOffset::east_opt(sign * (h * 3600 + m * 60 + s))
}

/// `naive.astimezone()`: a naive time read as this computer's local time.
pub fn local_aware(naive: NaiveDateTime) -> DateTime<FixedOffset> {
    match Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => dt.fixed_offset(),
        chrono::LocalResult::Ambiguous(a, _) => a.fixed_offset(),
        chrono::LocalResult::None => {
            // a time the clocks skipped: read with the offset in force just after the gap, as mktime does
            let off = Local.offset_from_utc_datetime(&naive).fix();
            off.from_utc_datetime(
                &(naive - chrono::Duration::seconds(off.local_minus_utc() as i64)),
            )
        }
    }
}

/// `datetime.now().astimezone()`.
pub fn now_local() -> DateTime<FixedOffset> {
    Local::now().fixed_offset()
}

// ---- os -----------------------------------------------------------------------------------------------------------

/// The home folder `Path.home()` names: `$HOME`, else the account's.
pub fn home() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(h) if !h.is_empty() => PathBuf::from(h),
        _ => dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
    }
}

/// `Path.expanduser()` for `~` and `~/…` (against `home`).
pub fn expand_user_in(p: &Path, home: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if s == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = s.strip_prefix("~/") {
        return home.join(rest);
    }
    p.to_path_buf()
}

/// `Path.expanduser()` against [`home`].
pub fn expand_user(p: &Path) -> PathBuf {
    if p.as_os_str().to_string_lossy().starts_with('~') {
        expand_user_in(p, &home())
    } else {
        p.to_path_buf()
    }
}

/// `os.path.abspath`: absolute against the working directory, `.` and `..` folded away lexically.
pub fn abspath(p: &Path) -> PathBuf {
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(p)
    };
    let mut out = PathBuf::from("/");
    for c in joined.components() {
        match c {
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(n) => out.push(n),
        }
    }
    out
}

/// `Path.resolve()` (not strict): symlinks resolved in the part of the path that exists, the rest kept as written.
pub fn resolve(p: &Path) -> io::Result<PathBuf> {
    let abs = abspath(p);
    let mut existing = abs.as_path();
    let mut rest: Vec<&std::ffi::OsStr> = Vec::new();
    loop {
        if std::fs::symlink_metadata(existing).is_ok() {
            break;
        }
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent;
            }
            _ => break,
        }
    }
    let mut out = std::fs::canonicalize(existing)?;
    for name in rest.iter().rev() {
        out.push(name);
    }
    Ok(out)
}

/// The class name Python gives an `OSError` with this errno (what a log line names).
pub fn os_error_name(e: &io::Error) -> &'static str {
    match e.raw_os_error() {
        Some(libc::EACCES) | Some(libc::EPERM) => "PermissionError",
        Some(libc::ENOENT) => "FileNotFoundError",
        Some(libc::EEXIST) => "FileExistsError",
        Some(libc::EISDIR) => "IsADirectoryError",
        Some(libc::ENOTDIR) => "NotADirectoryError",
        Some(libc::EINTR) => "InterruptedError",
        Some(libc::EAGAIN) => "BlockingIOError",
        Some(libc::ETIMEDOUT) => "TimeoutError",
        Some(libc::ECHILD) => "ChildProcessError",
        Some(libc::ESRCH) => "ProcessLookupError",
        Some(libc::EPIPE) => "BrokenPipeError",
        Some(libc::ECONNREFUSED) => "ConnectionRefusedError",
        Some(libc::ECONNRESET) => "ConnectionResetError",
        Some(libc::ECONNABORTED) => "ConnectionAbortedError",
        _ => "OSError",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitlines_knows_pythons_boundaries() {
        assert_eq!(splitlines("a\r\nb\rc\u{2028}d\n"), vec!["a", "b", "c", "d"]);
        assert_eq!(splitlines(""), Vec::<&str>::new());
        assert_eq!(splitlines("x\n\ny"), vec!["x", "", "y"]);
    }

    #[test]
    fn clip_and_flat_match_python() {
        assert_eq!(flat("  a \n b\x1c c "), "a b c");
        assert_eq!(clip("abcdef", 4), "abc…");
        assert_eq!(clip("ab  cdef", 4), "ab…");
        assert_eq!(clip("abc", 3), "abc");
    }

    #[test]
    fn dumps_matches_python_json() {
        let v = serde_json::json!({"ts": "x", "n": 1, "f": 1e16, "s": "é\n\u{1}", "a": [1, 2.5]});
        assert_eq!(
            dumps(&v),
            r#"{"ts": "x", "n": 1, "f": 1e+16, "s": "é\n\u0001", "a": [1, 2.5]}"#
        );
    }

    #[test]
    fn fromisoformat_reads_what_python_reads() {
        let (n, o) = fromisoformat("2026-09-21T09:05:00+02:00").unwrap();
        assert_eq!(n.to_string(), "2026-09-21 09:05:00");
        assert_eq!(o.unwrap().local_minus_utc(), 7200);
        let (n, o) = fromisoformat("2026-09-21 09:05").unwrap();
        assert_eq!(n.to_string(), "2026-09-21 09:05:00");
        assert!(o.is_none());
        assert!(fromisoformat("2026-09-21T09:05:00Z").unwrap().1.is_some());
        assert!(fromisoformat("2026-09-21T09:05:00.123456-0430").is_some());
        assert!(fromisoformat("2026-09-21").is_some());
        assert!(fromisoformat("2026-02-30").is_none());
        assert!(fromisoformat("nonsense").is_none());
        assert!(fromisoformat("").is_none());
    }

    #[test]
    fn int_reads_every_decimal_digit_as_python_does() {
        assert_eq!(int("2026"), Some(2026));
        assert_eq!(int("\u{663}"), Some(3)); // Arabic-Indic three
        assert_eq!(int("\u{966}\u{969}"), Some(3)); // Devanagari zero three
        assert_eq!(int("\u{1D7D7}"), Some(9)); // mathematical bold nine
        assert_eq!(int("\u{FF11}\u{FF12}"), Some(12)); // fullwidth
        assert_eq!(int("\u{b2}"), None); // superscript two: isdigit, but int() refuses it
        assert_eq!(int(""), None);
        assert_eq!(int("12a"), None);
    }

    #[test]
    fn fromisoformat_never_panics_on_a_character_where_a_digit_belongs() {
        assert!(fromisoformat("2026-09-0\u{e9}").is_none());
        assert!(fromisoformat("2026-09-21Ta\u{e9}1").is_none());
        assert!(fromisoformat("2026-09-21T09:05+0\u{e9}").is_none());
        assert!(fromisoformat("2026-09-21T0\u{e9}").is_none());
    }

    #[test]
    fn abspath_folds_dots() {
        assert_eq!(abspath(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
    }
}
