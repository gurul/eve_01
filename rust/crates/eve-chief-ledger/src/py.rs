//! The slice of Python the ledger's files and lines depend on, so Rust writes the bytes CPython writes and reads
//! every file CPython reads: `json.dump(indent=1)` and `json.dumps(separators=(",", ":"), ensure_ascii=False)`,
//! `json.loads` with its non-finite numbers, `repr`, `str.split`/`casefold`/`splitlines`, `round`, pathlib's
//! `expanduser`, a naive datetime's `timestamp()`, and the OSError subclass names.

use std::ffi::CString;
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use chrono::{Local, NaiveDateTime, TimeZone};
use serde_json::{Map, Value};
use unicode_general_category::{get_general_category, GeneralCategory as G};

// ---- strings -----------------------------------------------------------------------------------

/// `str.isspace()`: Unicode White_Space plus the four separators U+001C..U+001F.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `s.strip()` with no argument.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `" ".join(s.split())`.
pub fn squash(s: &str) -> String {
    s.split(is_space).filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")
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

/// `str.casefold()` where it differs from `to_lowercase` and the result holds ASCII letters (the only letters the
/// ledger's word match keeps), plus the Greek final sigma.
pub fn casefold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            'ß' | 'ẞ' => out.push_str("ss"),
            'ſ' => out.push('s'),
            'ς' => out.push('σ'),
            'ŉ' => out.push_str("\u{2bc}n"),
            'ǰ' => out.push_str("j\u{30c}"),
            'ẖ' => out.push_str("h\u{331}"),
            'ẗ' => out.push_str("t\u{308}"),
            'ẘ' => out.push_str("w\u{30a}"),
            'ẙ' => out.push_str("y\u{30a}"),
            'ẚ' => out.push_str("a\u{2be}"),
            'ﬀ' => out.push_str("ff"),
            'ﬁ' => out.push_str("fi"),
            'ﬂ' => out.push_str("fl"),
            'ﬃ' => out.push_str("ffi"),
            'ﬄ' => out.push_str("ffl"),
            'ﬅ' | 'ﬆ' => out.push_str("st"),
            'İ' => out.push_str("i\u{307}"),
            _ => out.extend(c.to_lowercase()),
        }
    }
    out
}

/// `str.splitlines(keepends=True)`: every line boundary Python knows, `\r\n` as one.
pub fn splitlines_keepends(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = text.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let end = match c {
            '\r' => {
                if let Some(&(j, '\n')) = it.peek() {
                    it.next();
                    j + 1
                } else {
                    i + 1
                }
            }
            '\n' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}' => {
                i + c.len_utf8()
            }
            _ => continue,
        };
        out.push(&text[start..end]);
        start = end;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// Text mode's universal newlines on read: `\r\n` and a lone `\r` become `\n`.
pub fn universal_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// `bytes.strip()` is empty: only ASCII whitespace.
pub fn bytes_blank(b: &[u8]) -> bool {
    b.iter().all(|c| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c))
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

/// `str(value)` of what `json.loads` would have made: a str is itself, everything else its `repr`.
pub fn str_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => repr_value(other),
    }
}

/// `repr()` of the Python object a JSON value stands for (`None`, `True`, `{'a': 1}`, ...).
pub fn repr_value(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => number_text(n),
        Value::String(s) => repr_str(s),
        Value::Array(items) => format!("[{}]", items.iter().map(repr_value).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter().map(|(k, v)| format!("{}: {}", repr_str(k), repr_value(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

// ---- floats ------------------------------------------------------------------------------------

/// `repr(float)`: the shortest round-trip digits, positional from 1e-4 up to 1e16, else `1e+16` style.
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

fn number_text(n: &serde_json::Number) -> String {
    if n.is_f64() {
        float_repr(n.as_f64().unwrap_or(0.0))
    } else {
        n.to_string()
    }
}

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

// ---- json --------------------------------------------------------------------------------------

fn write_str(out: &mut String, s: &str, ascii: bool) {
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
            c if ascii && (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", unit));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_scalar(out: &mut String, v: &Value, ascii: bool) -> bool {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => out.push_str(&number_text(n)),
        Value::String(s) => write_str(out, s, ascii),
        _ => return false,
    }
    true
}

/// `json.dumps(v, separators=(",", ":"), ensure_ascii=False)`.
pub fn dumps_compact(v: &Value) -> String {
    fn go(out: &mut String, v: &Value) {
        if write_scalar(out, v, false) {
            return;
        }
        match v {
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    go(out, item);
                }
                out.push(']');
            }
            Value::Object(map) => {
                out.push('{');
                for (i, (k, item)) in map.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_str(out, k, false);
                    out.push(':');
                    go(out, item);
                }
                out.push('}');
            }
            _ => {}
        }
    }
    let mut out = String::new();
    go(&mut out, v);
    out
}

/// `json.dump(v, f, indent=1)`: ASCII only, `", "` inside a line becomes `","` + newline, `": "` after a key.
pub fn dumps_indent1(v: &Value) -> String {
    fn go(out: &mut String, v: &Value, level: usize) {
        if write_scalar(out, v, true) {
            return;
        }
        match v {
            Value::Array(items) if items.is_empty() => out.push_str("[]"),
            Value::Object(map) if map.is_empty() => out.push_str("{}"),
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push('\n');
                    out.push_str(&" ".repeat(level + 1));
                    go(out, item, level + 1);
                }
                out.push('\n');
                out.push_str(&" ".repeat(level));
                out.push(']');
            }
            Value::Object(map) => {
                out.push('{');
                for (i, (k, item)) in map.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push('\n');
                    out.push_str(&" ".repeat(level + 1));
                    write_str(out, k, true);
                    out.push_str(": ");
                    go(out, item, level + 1);
                }
                out.push('\n');
                out.push_str(&" ".repeat(level));
                out.push('}');
            }
            _ => {}
        }
    }
    let mut out = String::new();
    go(&mut out, v, 0);
    out
}

/// The key of the stand-in object [`loads`] puts where `json.loads` makes a float that is not finite.
pub const NONFINITE_KEY: &str = "\u{0}py:nonfinite";

/// Is this the stand-in for a non-finite float?
pub fn is_nonfinite(v: &Value) -> bool {
    matches!(v, Value::Object(m) if m.len() == 1 && m.contains_key(NONFINITE_KEY))
}

/// What `json.loads` reads where it makes a non-finite float.
#[derive(Clone, Copy)]
pub enum NonFinite {
    /// A one-key object ([`NONFINITE_KEY`]) every typed check in chief_card refuses as it refuses an inf or a
    /// nan, and that is truthy as they are. The ledger's own shape checks treat it as a float.
    Marker,
    /// `null`: for event rows, which no check reads as a number.
    Null,
}

/// `json.loads(text)`: strict JSON plus what CPython also takes, the bare `NaN`, `Infinity` and `-Infinity` and
/// a number too big for a float (`1e999` reads as inf). serde_json refuses those, so they are swapped first for
/// what `nonfinite` says. Err where Python raises (or where serde_json refuses something else).
pub fn loads(text: &str, nonfinite: NonFinite) -> Result<Value, serde_json::Error> {
    match serde_json::from_str::<Value>(text) {
        Ok(v) => Ok(v),
        Err(e) => match replace_nonfinite(text, nonfinite) {
            Some(swapped) => serde_json::from_str::<Value>(&swapped),
            None => Err(e),
        },
    }
}

fn replace_nonfinite(s: &str, nonfinite: NonFinite) -> Option<String> {
    let stand_in = match nonfinite {
        NonFinite::Marker => "{\"\\u0000py:nonfinite\":null}",
        NonFinite::Null => "null",
    };
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut changed = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' {
            // copy the string through, escapes and all
            let start = i;
            i += 1;
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' => i += 2,
                    b'"' => {
                        i += 1;
                        break;
                    }
                    _ => i += 1,
                }
            }
            let end = i.min(bytes.len());
            out.push_str(&s[start..end]);
            continue;
        }
        let rest = &s[i..];
        if let Some(t) = ["-Infinity", "Infinity", "NaN"].into_iter().find(|t| rest.starts_with(t)) {
            out.push_str(stand_in);
            i += t.len();
            changed = true;
            continue;
        }
        if b == b'-' || b.is_ascii_digit() {
            let len = rest
                .bytes()
                .take_while(|c| c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.' | b'e' | b'E'))
                .count();
            let run = &rest[..len];
            match run.parse::<f64>() {
                Ok(f) if f.is_infinite() => {
                    out.push_str(stand_in);
                    changed = true;
                }
                _ => out.push_str(run),
            }
            i += len;
            continue;
        }
        let c = rest.chars().next().unwrap_or('\0');
        out.push(c);
        i += c.len_utf8().max(1);
    }
    changed.then_some(out)
}

/// `isinstance(v, dict)` for a loaded value (the non-finite stand-in is a float).
pub fn as_dict(v: &Value) -> Option<&Map<String, Value>> {
    match v {
        Value::Object(m) if !is_nonfinite(v) => Some(m),
        _ => None,
    }
}

// ---- errors ------------------------------------------------------------------------------------

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
        Some(libc::EPIPE) | Some(libc::ESHUTDOWN) => "BrokenPipeError",
        Some(libc::ECONNABORTED) => "ConnectionAbortedError",
        Some(libc::ECONNREFUSED) => "ConnectionRefusedError",
        Some(libc::ECONNRESET) => "ConnectionResetError",
        Some(libc::ECHILD) => "ChildProcessError",
        Some(libc::ESRCH) => "ProcessLookupError",
        Some(_) => "OSError",
        None => match e.kind() {
            io::ErrorKind::NotFound => "FileNotFoundError",
            io::ErrorKind::PermissionDenied => "PermissionError",
            io::ErrorKind::AlreadyExists => "FileExistsError",
            _ => "OSError",
        },
    }
}

// ---- files -------------------------------------------------------------------------------------

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

/// `os.chmod(path, 0o600)`.
pub fn chmod_600(path: &Path) -> io::Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
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
pub fn pure_path(raw: &str) -> String {
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

/// `os.path.expanduser("~")`: `$HOME`, else the account's passwd entry, trailing slashes cut ("/" if nothing
/// is left).
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
    let joined = match rest {
        Some(r) => format!("{home}/{r}"),
        None => home,
    };
    PathBuf::from(pure_path(&joined))
}

// ---- time --------------------------------------------------------------------------------------

/// `time.time()`.
pub fn time_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
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

/// `datetime.fromtimestamp(ts).date().isoformat()`; None where Python raises (out of range, not finite).
pub fn local_day(ts: f64) -> Option<String> {
    if !ts.is_finite() {
        return None;
    }
    // fromtimestamp rounds to the microsecond, half to even
    let micros = (ts * 1e6).round_ties_even();
    if micros.abs() > 9.0e18 {
        return None;
    }
    let micros = micros as i64;
    let secs = micros.div_euclid(1_000_000);
    let nanos = (micros.rem_euclid(1_000_000) * 1000) as u32;
    let at = Local.timestamp_opt(secs, nanos).earliest()?;
    Some(at.date_naive().format("%Y-%m-%d").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn float_repr_follows_python() {
        assert_eq!(float_repr(1_790_000_000.0), "1790000000.0");
        assert_eq!(float_repr(0.5), "0.5");
        assert_eq!(float_repr(1e16), "1e+16");
        assert_eq!(float_repr(1234567890123456.0), "1234567890123456.0");
        assert_eq!(float_repr(0.0001), "0.0001");
        assert_eq!(float_repr(0.00001), "1e-05");
        assert_eq!(float_repr(-2.5e-7), "-2.5e-07");
        assert_eq!(round_ndigits(1.0005, 3), 1.0);
        assert_eq!(round_ndigits(1_790_000_000.123_4, 3), 1_790_000_000.123);
    }

    #[test]
    fn indent_one_matches_json_dump() {
        // python3 -c 'import json;print(json.dumps({"a":[1,2.0,{}],"b":{"c":"é\u2028\x7f"},"d":[]},indent=1))'
        let v = json!({"a": [1, 2.0, {}], "b": {"c": "é\u{2028}\u{7f}"}, "d": []});
        assert_eq!(
            dumps_indent1(&v),
            "{\n \"a\": [\n  1,\n  2.0,\n  {}\n ],\n \"b\": {\n  \"c\": \"\\u00e9\\u2028\\u007f\"\n },\n \"d\": []\n}"
        );
        assert_eq!(dumps_indent1(&json!({"x": "\u{1F600}"})), "{\n \"x\": \"\\ud83d\\ude00\"\n}");
    }

    #[test]
    fn compact_matches_json_dumps() {
        let v = json!({"t": 1.5, "e": "x", "s": "é\n\u{1}", "l": [1, null, true]});
        assert_eq!(dumps_compact(&v), "{\"t\":1.5,\"e\":\"x\",\"s\":\"é\\n\\u0001\",\"l\":[1,null,true]}");
    }

    #[test]
    fn loads_takes_what_python_takes() {
        assert!(loads("[1e999]", NonFinite::Null).unwrap()[0].is_null());
        assert!(is_nonfinite(&loads("{\"a\": -1e999}", NonFinite::Marker).unwrap()["a"]));
        assert!(loads("[NaN, Infinity, -Infinity]", NonFinite::Null).unwrap().as_array().unwrap().iter().all(Value::is_null));
        assert_eq!(loads("[\"1e999 NaN\", 2]", NonFinite::Null).unwrap(), json!(["1e999 NaN", 2]));
        assert!(loads("{\"a\": oops", NonFinite::Null).is_err());
        assert!(loads("[\"\\\"NaN\", NaN]", NonFinite::Null).unwrap()[1].is_null());
    }

    #[test]
    fn repr_and_strings_follow_python() {
        assert_eq!(repr_value(&json!({"a": 1, "b": [true, null, "it's"], "c": 1.0})),
                   "{'a': 1, 'b': [True, None, \"it's\"], 'c': 1.0}");
        assert_eq!(repr_str("a\u{7f}\u{e9}\u{200b}"), "'a\\x7fé\\u200b'");
        assert_eq!(squash(" a \u{1c} b\u{3000}c "), "a b c");
        assert_eq!(casefold("STRAẞE ﬁle"), "strasse file");
        assert_eq!(splitlines_keepends("a\r\nb\rc\u{2028}d\n\ne"), vec!["a\r\n", "b\r", "c\u{2028}", "d\n", "\n", "e"]);
    }

    #[test]
    fn paths_follow_pathlib() {
        assert_eq!(pure_path("/x//y/./"), "/x/y");
        assert_eq!(pure_path("./a"), "a");
        assert_eq!(pure_path(""), ".");
        assert_eq!(pure_path("//a"), "//a");
        let home = home_of("").unwrap();
        assert_eq!(expanduser("~/.config/eve-bridge/chief"), PathBuf::from(format!("{home}/.config/eve-bridge/chief")));
        assert_eq!(expanduser("/x/~"), PathBuf::from("/x/~"));
        assert_eq!(expanduser("~no-such-user-here/x"), PathBuf::from("~no-such-user-here/x"));
    }
}
