//! The Python semantics chief_reflect leans on, so a lessons.jsonl line, a cleaned lesson and the reflection body
//! come out as the Python daemon makes them: `str.isspace` / `str.split()` / `str.splitlines()`, `str.casefold`,
//! the `\w` of a str pattern, `str()` / `repr()` of a JSON value, float `repr`, `float()` of a string,
//! `round(x, 3)`, `json.dumps`' default separators, pathlib `expanduser` and the OSError subclass names.

use std::io;
use std::path::PathBuf;

use serde_json::ser::Formatter;
use serde_json::Value;
use unicode_general_category::{get_general_category, GeneralCategory as G};

/// `str.isspace()` for one character: Unicode White_Space plus the information separators U+001C..U+001F.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `" ".join(s.split())`.
pub fn squash(s: &str) -> String {
    s.split(is_space).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// `s[:n]`, counted in characters.
pub fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// `len(s)`, in characters.
pub fn len(s: &str) -> usize {
    s.chars().count()
}

/// `str.splitlines()`: \n, \r, \r\n, \v, \f, \x1c, \x1d, \x1e, \x85, U+2028, U+2029; no trailing empty line.
pub fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let brk = matches!(
            c,
            '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if !brk {
            continue;
        }
        out.push(&s[start..i]);
        let mut next = i + c.len_utf8();
        if c == '\r' {
            if let Some(&(j, '\n')) = it.peek() {
                it.next();
                next = j + 1;
            }
        }
        start = next;
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// `str.casefold()`: Unicode full case folding.
pub fn casefold(s: &str) -> String {
    caseless::default_case_fold_str(s)
}

/// The `\w` of a Python str pattern: `str.isalnum()` (letters, and decimal, digit and numeric characters) or `_`.
pub fn is_word(c: char) -> bool {
    if c == '_' || c.is_ascii_alphanumeric() {
        return true;
    }
    matches!(
        get_general_category(c),
        G::UppercaseLetter
            | G::LowercaseLetter
            | G::TitlecaseLetter
            | G::ModifierLetter
            | G::OtherLetter
            | G::DecimalNumber
            | G::LetterNumber
            | G::OtherNumber
    )
}

/// Python's truthiness of a JSON value.
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

/// `str(v or "")`.
pub fn str_or_empty(v: Option<&Value>) -> String {
    if !truthy(v) {
        return String::new();
    }
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(other) => repr(other),
        None => String::new(),
    }
}

/// `repr(x)` of a float, CPython's shortest round-trip form ("1.5", "1e-05", "1e+16", "10.0").
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".to_string();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0" } else { "0.0" }.to_string();
    }
    let sci = format!("{x:e}");
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let neg = mant.starts_with('-');
    let digits: String = mant.chars().filter(char::is_ascii_digit).collect();
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if (-4..16).contains(&exp) {
        if exp >= 0 {
            let int_len = exp as usize + 1;
            if digits.len() <= int_len {
                out.push_str(&digits);
                out.push_str(&"0".repeat(int_len - digits.len()));
                out.push_str(".0");
            } else {
                out.push_str(&digits[..int_len]);
                out.push('.');
                out.push_str(&digits[int_len..]);
            }
        } else {
            out.push_str("0.");
            out.push_str(&"0".repeat((-exp - 1) as usize));
            out.push_str(&digits);
        }
    } else {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if exp < 0 { '-' } else { '+' });
        out.push_str(&format!("{:02}", exp.abs()));
    }
    out
}

/// A float as `json.dumps` writes it: its repr, or NaN / Infinity / -Infinity.
pub fn json_float(x: f64) -> String {
    if x.is_nan() {
        "NaN".to_string()
    } else if x.is_infinite() {
        if x > 0.0 { "Infinity" } else { "-Infinity" }.to_string()
    } else {
        float_repr(x)
    }
}

/// `round(x, 3)`: correctly rounded through the exact decimal, as CPython's.
pub fn round3(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.3}").parse().unwrap_or(x)
}

fn printable(c: char) -> bool {
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

/// `repr(s)` of a str: single quotes unless it holds a ' and no ", the escapes CPython writes.
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
            c if printable(c) => out.push(c),
            c if (c as u32) < 0x100 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) < 0x10000 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push(quote);
    out
}

/// `str(v)` of a value `json.loads` returned: a str is itself; the rest are their repr.
pub fn repr(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                n.to_string()
            } else {
                float_repr(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => s.clone(),
        Value::Array(a) => format!("[{}]", a.iter().map(nested).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter().map(|(k, v)| format!("{}: {}", repr_str(k), nested(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

fn nested(v: &Value) -> String {
    match v {
        Value::String(s) => repr_str(s),
        other => repr(other),
    }
}

/// `float(s)` of a str: stripped; an optional sign, then digits (single underscores between digits), an optional
/// fraction and exponent, or inf / infinity / nan in any case. None where Python raises ValueError.
pub fn float_of_str(s: &str) -> Option<f64> {
    let t = s.trim_matches(is_space);
    let (sign, body) = match t.strip_prefix('-') {
        Some(rest) => (-1.0, rest),
        None => (1.0, t.strip_prefix('+').unwrap_or(t)),
    };
    let lower = body.to_ascii_lowercase();
    if lower == "inf" || lower == "infinity" {
        return Some(sign * f64::INFINITY);
    }
    if lower == "nan" {
        return Some(f64::NAN);
    }
    let bytes = body.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'_' {
            let ok = i > 0 && bytes[i - 1].is_ascii_digit() && bytes.get(i + 1).is_some_and(u8::is_ascii_digit);
            if !ok {
                return None;
            }
        } else if !(b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-')) {
            return None;
        }
    }
    let clean: String = body.chars().filter(|c| *c != '_').collect();
    // Rust's grammar is Python's for these characters (digits, ".", "e", a signed exponent).
    if clean.is_empty() || clean.starts_with(['+', '-']) {
        return None;
    }
    clean.parse::<f64>().ok().map(|x| sign * x)
}

/// `float(v or 0.0)` of a value `json.loads` returned; None where Python raises (ValueError, TypeError).
pub fn float_or_zero(v: Option<&Value>) -> Option<f64> {
    if !truthy(v) {
        return Some(0.0);
    }
    match v {
        Some(Value::Bool(_)) => Some(1.0),
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => float_of_str(s),
        _ => None,
    }
}

/// The OSError subclass CPython raises for an error, by errno; "OSError" otherwise.
pub fn os_error_name(e: &io::Error) -> &'static str {
    let Some(code) = e.raw_os_error() else {
        return match e.kind() {
            io::ErrorKind::NotFound => "FileNotFoundError",
            io::ErrorKind::PermissionDenied => "PermissionError",
            io::ErrorKind::AlreadyExists => "FileExistsError",
            io::ErrorKind::NotADirectory => "NotADirectoryError",
            io::ErrorKind::IsADirectory => "IsADirectoryError",
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
        libc::EAGAIN | libc::EALREADY | libc::EINPROGRESS => "BlockingIOError",
        libc::ECHILD => "ChildProcessError",
        libc::EPIPE | libc::ESHUTDOWN => "BrokenPipeError",
        libc::ECONNABORTED => "ConnectionAbortedError",
        libc::ECONNREFUSED => "ConnectionRefusedError",
        libc::ECONNRESET => "ConnectionResetError",
        libc::ESRCH => "ProcessLookupError",
        libc::ETIMEDOUT => "TimeoutError",
        _ => "OSError",
    }
}

fn user_home(user: Option<&str>) -> Option<String> {
    if user.is_none() {
        if let Some(h) = std::env::var_os("HOME") {
            return Some(h.to_string_lossy().into_owned());
        }
    }
    // SAFETY: getpwuid / getpwnam return a pointer into static storage or NULL; it is read at once and copied.
    unsafe {
        let pw = match user {
            None => libc::getpwuid(libc::getuid()),
            Some(name) => {
                let c = std::ffi::CString::new(name).ok()?;
                libc::getpwnam(c.as_ptr())
            }
        };
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        Some(std::ffi::CStr::from_ptr((*pw).pw_dir).to_string_lossy().into_owned())
    }
}

/// pathlib's `Path(p).expanduser()`: a leading `~` or `~user` becomes that home ($HOME first for `~`). A home
/// that cannot be found leaves the path as it is.
pub fn expanduser(p: &str) -> PathBuf {
    let Some(rest) = p.strip_prefix('~') else {
        return PathBuf::from(p);
    };
    let (user, tail) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    match user_home(if user.is_empty() { None } else { Some(user) }) {
        Some(home) => {
            let home = home.trim_end_matches('/');
            PathBuf::from(format!("{home}{tail}"))
        }
        None => PathBuf::from(p),
    }
}

/// `json.dumps`' default separators: ", " between items and ": " after a key.
pub struct PyFormatter;

impl Formatter for PyFormatter {
    fn begin_array_value<W: ?Sized + io::Write>(&mut self, writer: &mut W, first: bool) -> io::Result<()> {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_key<W: ?Sized + io::Write>(&mut self, writer: &mut W, first: bool) -> io::Result<()> {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_value<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        writer.write_all(b": ")
    }
}

/// `json.dumps(v, ensure_ascii=False)`.
pub fn dumps(v: &Value) -> String {
    let mut buf = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, PyFormatter);
    serde::Serialize::serialize(v, &mut ser).expect("a JSON value always serializes");
    String::from_utf8(buf).expect("serde_json writes UTF-8")
}

/// `json.dumps(s)` of one str (separators do not matter for a scalar).
pub fn dumps_str(s: &str) -> String {
    serde_json::to_string(s).expect("a str always serializes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn float_repr_matches_cpython() {
        for (x, want) in [
            (1.5, "1.5"),
            (10.0, "10.0"),
            (1e16, "1e+16"),
            (1e-5, "1e-05"),
            (0.0001, "0.0001"),
            (1790000000.123, "1790000000.123"),
            (123456789012345.6, "123456789012345.6"),
            (-2.5e-7, "-2.5e-07"),
            (1.2345e20, "1.2345e+20"),
        ] {
            assert_eq!(float_repr(x), want, "{x}");
        }
        assert_eq!(round3(1.23456), 1.235);
    }

    #[test]
    fn repr_and_str_of_values() {
        assert_eq!(str_or_empty(Some(&json!(0))), "");
        assert_eq!(str_or_empty(Some(&json!(true))), "True");
        assert_eq!(str_or_empty(Some(&json!(3))), "3");
        assert_eq!(str_or_empty(Some(&json!(["a", 1, null, {"k": "it's"}]))), "['a', 1, None, {'k': \"it's\"}]");
        assert_eq!(repr_str("a\u{1}\n"), "'a\\x01\\n'");
    }

    #[test]
    fn splitlines_and_squash() {
        assert_eq!(splitlines("a\r\nb\rc\u{2028}d\n"), vec!["a", "b", "c", "d"]);
        assert_eq!(squash("  a \u{1c} b\u{a0}c "), "a b c");
    }

    #[test]
    fn float_of_str_follows_python() {
        assert_eq!(float_of_str(" 12.5 "), Some(12.5));
        assert_eq!(float_of_str("1_000"), Some(1000.0));
        assert_eq!(float_of_str("-Infinity"), Some(f64::NEG_INFINITY));
        assert_eq!(float_of_str("1__0"), None);
        assert_eq!(float_of_str("abc"), None);
        assert_eq!(float_of_str("--1"), None);
        assert_eq!(float_or_zero(Some(&json!([1]))), None);
        assert_eq!(float_or_zero(Some(&json!([]))), Some(0.0));
    }

    #[test]
    fn dumps_uses_pythons_default_separators() {
        assert_eq!(dumps(&json!({"a": [1, "é"], "b": {"c": true}})), "{\"a\": [1, \"é\"], \"b\": {\"c\": true}}");
    }
}
