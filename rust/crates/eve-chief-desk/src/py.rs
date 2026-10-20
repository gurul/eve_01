//! The slice of Python the desk's files and rules depend on, so Rust reads and writes what CPython does:
//! `datetime.fromtimestamp` (half-even microseconds, the 1..9999 year range), a naive datetime's `timestamp()`,
//! `strptime(value, "%H:%M")` with its error texts, `int()` of a str, `json.dumps(separators=(",", ":"))`
//! (ASCII only), `round(x, 3)`, `repr` of a str, text-mode reads, pathlib's `expanduser`, the OSError subclass
//! names and Python truthiness.

use std::ffi::CString;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use chrono::{Local, NaiveDateTime, TimeDelta, TimeZone, Timelike};
use serde_json::Value;
use unicode_general_category::{get_general_category, GeneralCategory as G};

use crate::DeskError;

// ---- strings -----------------------------------------------------------------------------------

/// `str.isspace()`: Unicode White_Space plus the four separators U+001C..U+001F.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `s.strip()` with no argument.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `s[:n]`: the first `n` code points.
pub fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// `len(s)`.
pub fn len(s: &str) -> usize {
    s.chars().count()
}

/// Text mode's universal newlines on read: `\r\n` and a lone `\r` become `\n`.
pub fn universal_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn is_printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    !matches!(
        get_general_category(c),
        G::Control
            | G::Format
            | G::Surrogate
            | G::PrivateUse
            | G::Unassigned
            | G::LineSeparator
            | G::ParagraphSeparator
            | G::SpaceSeparator
    )
}

/// `repr(s)` for a str.
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
            c if (0x20..0x7f).contains(&(c as u32)) => out.push(c),
            c if (c as u32) >= 0x80 && is_printable(c) => out.push(c),
            c if (c as u32) < 0x100 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) < 0x10000 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push(quote);
    out
}

fn is_decimal(c: char) -> bool {
    get_general_category(c) == G::DecimalNumber
}

/// The value of a decimal digit (the `\d` of a str pattern, what `int()` reads), any script. Decimal digits are
/// encoded in contiguous runs of ten from 0 to 9, so the value is the distance from the run's start, modulo 10.
fn decimal_value(c: char) -> Option<u32> {
    if !is_decimal(c) {
        return None;
    }
    let mut start = c as u32;
    while let Some(prev) = start.checked_sub(1).and_then(char::from_u32) {
        if !is_decimal(prev) {
            break;
        }
        start -= 1;
    }
    Some((c as u32 - start) % 10)
}

/// `int(s)` of a str in base 10: whitespace stripped, an optional sign, decimal digits of any script with
/// single underscores between them. Err is Python's ValueError text.
pub fn int_of_str(s: &str) -> Result<i64, DeskError> {
    let bad = || DeskError::value(format!("invalid literal for int() with base 10: {}", take_chars(&repr_str(s), 200)));
    let body = strip(s);
    let (neg, digits) = match body.chars().next() {
        Some('-') => (true, &body[1..]),
        Some('+') => (false, &body[1..]),
        _ => (false, body),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return Err(bad());
    }
    let mut n: i64 = 0;
    for c in digits.chars() {
        if c == '_' {
            continue;
        }
        let d = decimal_value(c).ok_or_else(bad)?;
        n = n.saturating_mul(10).saturating_add(i64::from(d));
    }
    Ok(if neg { -n } else { n })
}

// ---- tomllib (TOML 1.0) ------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Lex {
    Code,
    Comment,
    Basic,
    Literal,
    MlBasic,
    MlLiteral,
}

/// Whether `text` leans on something the `toml` crate (TOML 1.1) reads and CPython's `tomllib` (TOML 1.0)
/// refuses with TOMLDecodeError: a byte-order mark, a `\x` or `\e` escape, a newline or a trailing comma in an
/// inline table, or a time without its seconds. Everything else the two read alike.
pub fn toml_1_1_only(text: &str) -> bool {
    if text.starts_with('\u{feff}') {
        return true;
    }
    let chars: Vec<char> = text.chars().collect();
    let three = |i: usize, q: char| chars.get(i) == Some(&q) && chars.get(i + 1) == Some(&q) && chars.get(i + 2) == Some(&q);
    // the text with strings and comments blanked, for the time check
    let mut code: Vec<u8> = Vec::with_capacity(chars.len());
    let mut stack: Vec<char> = Vec::new();
    let mut last = '\n';
    let mut state = Lex::Code;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match state {
            Lex::Code => {
                match c {
                    '#' => state = Lex::Comment,
                    '"' if three(i, '"') => {
                        state = Lex::MlBasic;
                        i += 2;
                    }
                    '\'' if three(i, '\'') => {
                        state = Lex::MlLiteral;
                        i += 2;
                    }
                    '"' => state = Lex::Basic,
                    '\'' => state = Lex::Literal,
                    '{' | '[' => stack.push(c),
                    '}' => {
                        if last == ',' && stack.last() == Some(&'{') {
                            return true;
                        }
                        stack.pop();
                    }
                    ']' => {
                        stack.pop();
                    }
                    '\n' if stack.last() == Some(&'{') => return true,
                    _ => {}
                }
                if state == Lex::Code {
                    code.push(if c.is_ascii() { c as u8 } else { b' ' });
                    if !c.is_whitespace() {
                        last = c;
                    }
                } else {
                    code.push(b' ');
                    last = '"';
                }
            }
            Lex::Comment => {
                if c == '\n' {
                    state = Lex::Code;
                    if stack.last() == Some(&'{') {
                        return true;
                    }
                }
                code.push(if c == '\n' { b'\n' } else { b' ' });
            }
            Lex::Basic | Lex::MlBasic => {
                if c == '\\' {
                    if matches!(chars.get(i + 1), Some('x') | Some('e')) {
                        return true;
                    }
                    i += 1;
                    code.push(b' ');
                } else if state == Lex::Basic && c == '"' {
                    state = Lex::Code;
                } else if state == Lex::MlBasic && three(i, '"') {
                    // a closing run may hold up to two more quotes, which belong to the string
                    i += 2;
                    while chars.get(i + 1) == Some(&'"') {
                        i += 1;
                    }
                    state = Lex::Code;
                }
                code.push(b' ');
            }
            Lex::Literal | Lex::MlLiteral => {
                if state == Lex::Literal && c == '\'' {
                    state = Lex::Code;
                } else if state == Lex::MlLiteral && three(i, '\'') {
                    i += 2;
                    while chars.get(i + 1) == Some(&'\'') {
                        i += 1;
                    }
                    state = Lex::Code;
                }
                code.push(b' ');
            }
        }
        i += 1;
    }
    // a time with no seconds: HH:MM that is not part of a longer HH:MM:SS
    let b = &code;
    (0..b.len().saturating_sub(4)).any(|i| {
        b[i].is_ascii_digit()
            && b[i + 1].is_ascii_digit()
            && b[i + 2] == b':'
            && b[i + 3].is_ascii_digit()
            && b[i + 4].is_ascii_digit()
            && (i == 0 || !(b[i - 1].is_ascii_digit() || b[i - 1] == b':'))
            && b.get(i + 5) != Some(&b':')
    })
}

// ---- strptime("%H:%M") -------------------------------------------------------------------------

fn ascii_in(c: Option<&char>, lo: char, hi: char) -> bool {
    c.is_some_and(|c| (lo..=hi).contains(c))
}

fn digit(c: Option<&char>) -> bool {
    c.is_some_and(|c| is_decimal(*c))
}

/// `re.match(r"(?P<H>2[0-3]|[0-1]\d|\d):(?P<M>[0-5]\d|\d)", s)` (CPython 3.13 `_strptime`'s `%H:%M`), with the
/// regex's alternation order and backtracking: (hour chars, minute chars, chars matched).
fn match_hhmm(s: &[char]) -> Option<(usize, usize)> {
    let hours: [usize; 3] = [2, 2, 1];
    for (alt, width) in hours.iter().enumerate() {
        let ok = match alt {
            0 => s.first() == Some(&'2') && ascii_in(s.get(1), '0', '3'),
            1 => ascii_in(s.first(), '0', '1') && digit(s.get(1)),
            _ => digit(s.first()),
        };
        if !ok || s.get(*width) != Some(&':') {
            continue;
        }
        let m = width + 1;
        if ascii_in(s.get(m), '0', '5') && digit(s.get(m + 1)) {
            return Some((*width, m + 2));
        }
        if digit(s.get(m)) {
            return Some((*width, m + 1));
        }
    }
    None
}

/// `datetime.strptime(value, "%H:%M").strftime("%H:%M")`. Err is Python's ValueError text.
pub fn strptime_hhmm(value: &str) -> Result<String, DeskError> {
    let chars: Vec<char> = value.chars().collect();
    let Some((h_len, end)) = match_hhmm(&chars) else {
        return Err(DeskError::value(format!("time data {} does not match format '%H:%M'", repr_str(value))));
    };
    if end != chars.len() {
        let rest: String = chars[end..].iter().collect();
        return Err(DeskError::value(format!("unconverted data remains: {rest}")));
    }
    let num = |cs: &[char]| cs.iter().filter_map(|c| decimal_value(*c)).fold(0u32, |n, d| n * 10 + d);
    let h = num(&chars[..h_len]);
    let m = num(&chars[h_len + 1..end]);
    Ok(format!("{h:02}:{m:02}"))
}

// ---- time --------------------------------------------------------------------------------------

/// `time.time()`.
pub fn time_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// `datetime.fromtimestamp(ts)`: the naive local time, the fraction rounded to the microsecond half to even as
/// `_PyTime_ObjectToTimeval` does. Err where Python raises (NaN, out of time_t, a year outside 1..9999).
pub fn fromtimestamp(ts: f64) -> Result<NaiveDateTime, DeskError> {
    if ts.is_nan() {
        return Err(DeskError::value("Invalid value NaN (not a number)"));
    }
    let mut intpart = ts.trunc();
    let mut floatpart = ((ts - intpart) * 1e6).round_ties_even();
    if floatpart >= 1e6 {
        floatpart -= 1e6;
        intpart += 1.0;
    } else if floatpart < 0.0 {
        floatpart += 1e6;
        intpart -= 1.0;
    }
    // time_t is i64; the double must be strictly inside its range
    if !(-9.223_372_036_854_776e18..9.223_372_036_854_776e18).contains(&intpart) {
        return Err(DeskError::new("OverflowError", "timestamp out of range for platform time_t"));
    }
    let secs = intpart as i64;
    let Some(at) = Local.timestamp_opt(secs, 0).earliest() else {
        return Err(DeskError::new("OSError", "Value too large to be stored in data type"));
    };
    let naive = at.naive_local();
    let year = chrono::Datelike::year(&naive);
    if !(1..=9999).contains(&year) {
        return Err(DeskError::value(format!("year {year} is out of range")));
    }
    Ok(naive + TimeDelta::microseconds(floatpart as i64))
}

fn local_secs(u: i64) -> Option<i64> {
    Local.timestamp_opt(u, 0).single().map(|d| d.naive_local().and_utc().timestamp())
}

/// `naive.timestamp()` (fold 0) as CPython's `local_to_seconds` reckons it: an ambiguous local time is the
/// earlier instant, a time in a spring-forward gap is read with the offset before the gap.
pub fn naive_timestamp(at: &NaiveDateTime) -> Option<f64> {
    let utc = at.and_utc();
    let t = utc.timestamp();
    let micro = f64::from(utc.timestamp_subsec_micros()) / 1e6;
    let a = local_secs(t)? - t;
    let u1 = t - a;
    let t1 = local_secs(u1)?;
    let b = if t1 == t {
        let u2 = u1 - 24 * 3600;
        let b = local_secs(u2)? - u2;
        if a == b {
            return Some(u1 as f64 + micro);
        }
        b
    } else {
        t1 - u1
    };
    let u2 = t - b;
    let t2 = local_secs(u2)?;
    let secs = if t2 == t {
        u2
    } else if t1 == t {
        u1
    } else {
        u1.max(u2)
    };
    Some(secs as f64 + micro)
}

/// `t.replace(hour=h, minute=m, second=0, microsecond=0)`; Err is the ValueError for an hour or minute out of
/// range.
pub fn replace_hm(t: &NaiveDateTime, h: i64, m: i64) -> Result<NaiveDateTime, DeskError> {
    if !(0..=23).contains(&h) {
        return Err(DeskError::value("hour must be in 0..23"));
    }
    if !(0..=59).contains(&m) {
        return Err(DeskError::value("minute must be in 0..59"));
    }
    t.with_nanosecond(0)
        .and_then(|t| t.with_second(0))
        .and_then(|t| t.with_minute(m as u32))
        .and_then(|t| t.with_hour(h as u32))
        .ok_or_else(|| DeskError::value("hour must be in 0..23"))
}

/// A naive datetime plus one day; Err is Python's OverflowError past year 9999.
pub fn plus_day(t: &NaiveDateTime) -> Result<NaiveDateTime, DeskError> {
    t.checked_add_signed(TimeDelta::days(1))
        .filter(|t| chrono::Datelike::year(t) <= 9999)
        .ok_or_else(|| DeskError::new("OverflowError", "date value out of range"))
}

// ---- numbers and json --------------------------------------------------------------------------

/// `round(x, ndigits)` for a float: correctly rounded, ties to even on the exact binary value.
pub fn round_ndigits(x: f64, ndigits: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    let r: f64 = format!("{:.*}", ndigits, x).parse().unwrap_or(x);
    if r == 0.0 && x.is_sign_negative() {
        -0.0
    } else {
        r
    }
}

/// `repr(float)` as `json.dumps` writes it: the shortest round-trip digits, positional from 1e-4 up to 1e16,
/// else `1e+16` style; the non-finite as `NaN`, `Infinity`, `-Infinity`.
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
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
        let frac = if digits.len() > 1 { format!(".{}", &digits[1..]) } else { String::new() };
        let esign = if exp < 0 { '-' } else { '+' };
        format!("{sign}{}{frac}e{esign}{:02}", &digits[..1], exp.abs())
    }
}

/// A JSON string as `json.dumps` (ensure_ascii) writes it.
pub fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
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
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", unit));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Python truthiness of what `json.loads` would have made (None, False, 0, "", [], {} are false).
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

// ---- errors and files --------------------------------------------------------------------------

/// `type(e).__name__` of the OSError CPython raises for this error.
pub fn os_error_name(e: &io::Error) -> &'static str {
    match e.raw_os_error() {
        Some(libc::ENOENT) => "FileNotFoundError",
        Some(libc::EACCES) | Some(libc::EPERM) => "PermissionError",
        Some(libc::EEXIST) => "FileExistsError",
        Some(libc::EISDIR) => "IsADirectoryError",
        Some(libc::ENOTDIR) => "NotADirectoryError",
        Some(libc::EINTR) => "InterruptedError",
        Some(libc::EAGAIN) | Some(libc::EALREADY) | Some(libc::EINPROGRESS) => "BlockingIOError",
        Some(libc::ETIMEDOUT) => "TimeoutError",
        Some(_) => "OSError",
        None => match e.kind() {
            io::ErrorKind::NotFound => "FileNotFoundError",
            io::ErrorKind::PermissionDenied => "PermissionError",
            io::ErrorKind::AlreadyExists => "FileExistsError",
            _ => "OSError",
        },
    }
}

/// `path.mkdir(mode=0o700, parents=True, exist_ok=True)`: the parents with the default mode, the folder itself
/// 0o700 (less the umask); an existing folder is fine, an existing file is FileExistsError.
pub fn mkdir_700(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.is_dir() {
            std::fs::create_dir_all(parent)?;
        }
    }
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(e) => Err(e),
    }
}

/// `os.fsync(fd)`: the plain fsync(2), not the F_FULLFSYNC `File::sync_all` uses on macOS.
pub fn fsync(file: &std::fs::File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: the descriptor belongs to `file`, which outlives the call.
    if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

// ---- paths -------------------------------------------------------------------------------------

/// `str(PurePosixPath(raw))`: repeated and trailing slashes and `.` parts dropped, `..` kept, "" is ".", and a
/// leading `//` (exactly two) kept as POSIX allows.
fn pure_path(raw: &str) -> String {
    if raw.is_empty() {
        return ".".into();
    }
    let root = if raw.starts_with("//") && !raw.starts_with("///") {
        "//"
    } else if raw.starts_with('/') {
        "/"
    } else {
        ""
    };
    let parts: Vec<&str> = raw.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    let joined = parts.join("/");
    if root.is_empty() && joined.is_empty() {
        ".".into()
    } else {
        format!("{root}{joined}")
    }
}

fn home_of(user: &str) -> Option<String> {
    let raw = if user.is_empty() {
        match std::env::var_os("HOME") {
            Some(h) => h.to_string_lossy().into_owned(),
            None => passwd_dir(None)?,
        }
    } else {
        passwd_dir(Some(user))?
    };
    let cut = raw.trim_end_matches('/');
    Some(if cut.is_empty() { "/".into() } else { cut.to_string() })
}

fn passwd_dir(user: Option<&str>) -> Option<String> {
    // SAFETY: getpwnam/getpwuid return a pointer into static storage or NULL; the fields are read at once.
    unsafe {
        let pw = match user {
            Some(name) => {
                let c = CString::new(name).ok()?;
                libc::getpwnam(c.as_ptr())
            }
            None => libc::getpwuid(libc::getuid()),
        };
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        Some(std::ffi::CStr::from_ptr((*pw).pw_dir).to_string_lossy().into_owned())
    }
}

/// `Path(raw).expanduser()`: a leading `~` or `~user` part replaced by that home folder. An unknown user is
/// left as written (pathlib raises RuntimeError there).
pub fn expanduser(raw: &str) -> PathBuf {
    let norm = pure_path(raw);
    if !norm.starts_with('~') {
        return PathBuf::from(norm);
    }
    let (first, rest) = match norm.split_once('/') {
        Some((f, r)) => (f, Some(r)),
        None => (norm.as_str(), None),
    };
    let Some(home) = home_of(&first[1..]) else {
        return PathBuf::from(norm);
    };
    let cut = home.trim_end_matches('/');
    let joined = match rest {
        Some(r) => format!("{cut}/{r}"),
        None if cut.is_empty() => "/".to_string(),
        None => cut.to_string(),
    };
    PathBuf::from(pure_path(&joined))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strptime_hhmm_reads_and_refuses_as_python_does() {
        // python3 -c 'from datetime import datetime as d; d.strptime(X, "%H:%M")'
        assert_eq!(strptime_hhmm("8:5").unwrap(), "08:05");
        assert_eq!(strptime_hhmm("23:59").unwrap(), "23:59");
        assert_eq!(strptime_hhmm("\u{0668}:\u{0663}").unwrap(), "08:03");
        // [0-1] and [0-5] are ASCII only, so a two-digit Arabic-Indic hour does not match
        assert!(strptime_hhmm("\u{0660}\u{0668}:\u{0663}\u{0660}").is_err());
        assert_eq!(strptime_hhmm("late").unwrap_err().message, "time data 'late' does not match format '%H:%M'");
        assert_eq!(strptime_hhmm("24:00").unwrap_err().message, "time data '24:00' does not match format '%H:%M'");
        assert_eq!(strptime_hhmm("08:61").unwrap_err().message, "unconverted data remains: 1");
        assert_eq!(strptime_hhmm("08:0012").unwrap_err().message, "unconverted data remains: 12");
        assert_eq!(strptime_hhmm("").unwrap_err().message, "time data '' does not match format '%H:%M'");
    }

    #[test]
    fn toml_1_1_only_finds_what_tomllib_refuses() {
        // python3 -c 'import tomllib; tomllib.loads(X)' raises TOMLDecodeError for each of these
        for t in [
            "a = 08:00",
            r#"a = "\x32""#,
            r#"a = "\e""#,
            "a = {b=1,\n c=2}",
            "a = {b=1,}",
            "a = 1979-05-27 07:32",
            "\u{feff}a=1",
            r#"a = """x\e""""#,
            "a = {b=1 # c\n}",
        ] {
            assert!(toml_1_1_only(t), "{t:?}");
        }
        // and reads these
        for t in [
            "a = 08:00:00",
            "a = 1979-05-27 07:32:00",
            "a = [1,\n2,]",
            r"a = '''x\e'''",
            r#"a = "x:12:34""#,
            "a = {b=[1,\n2]}",
            "a = \"\"\"x\"\"\"\"\nb = {c=1}",
            "# 08:00\na = \"\\\\x\"",
            "a = { b = 1 , c = '{,}' }",
        ] {
            assert!(!toml_1_1_only(t), "{t:?}");
        }
    }

    #[test]
    fn int_of_str_follows_int() {
        assert_eq!(int_of_str(" 08").unwrap(), 8);
        assert_eq!(int_of_str("+1_0").unwrap(), 10);
        assert_eq!(int_of_str("-3").unwrap(), -3);
        assert!(int_of_str("1__0").is_err() && int_of_str("").is_err() && int_of_str("x").is_err());
    }

    #[test]
    fn json_and_floats_follow_python() {
        assert_eq!(json_str("é\n\"x"), "\"\\u00e9\\n\\\"x\"");
        assert_eq!(json_str("\u{1F600}"), "\"\\ud83d\\ude00\"");
        assert_eq!(float_repr(1_790_000_000.123), "1790000000.123");
        assert_eq!(float_repr(5.0), "5.0");
        assert_eq!(float_repr(1e16), "1e+16");
        assert_eq!(round_ndigits(1_790_000_000.123_4, 3), 1_790_000_000.123);
    }

    #[test]
    fn fromtimestamp_refuses_what_python_refuses() {
        assert!(fromtimestamp(f64::NAN).is_err());
        assert!(fromtimestamp(f64::INFINITY).is_err());
        assert!(fromtimestamp(1e300).is_err());
        assert!(fromtimestamp(1e12).is_err()); // year 33658
        assert!(fromtimestamp(0.0).is_ok());
        let t = fromtimestamp(1_790_000_000.999_999_7).unwrap();
        assert_eq!(t.and_utc().timestamp_subsec_micros(), 0); // rounded up to the next second
    }
}
