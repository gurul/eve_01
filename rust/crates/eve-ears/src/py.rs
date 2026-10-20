//! The few Python built-ins whose exact behavior `ears.py` relies on: `str.strip`, `float()`, `repr()`,
//! `format(x, "g")`, `str(float)`, `os.path.expanduser` and `pathlib.Path`'s normalisation.

use std::ffi::{CStr, CString};

/// `str.isspace` for one character: Unicode White_Space plus the four separators U+001C..U+001F.
fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()`.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_py_space)
}

/// The zero of every Unicode decimal-digit (Nd) run of ten, Unicode 15.1 (CPython 3.13's tables).
const ND_ZEROS: [u32; 68] = [
    0x30, 0x660, 0x6F0, 0x7C0, 0x966, 0x9E6, 0xA66, 0xAE6, 0xB66, 0xBE6, 0xC66, 0xCE6, 0xD66, 0xDE6, 0xE50,
    0xED0, 0xF20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80, 0x1A90, 0x1B50, 0x1BB0, 0x1C40,
    0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0, 0xFF10, 0x104A0, 0x10D30, 0x11066,
    0x110F0, 0x11136, 0x111D0, 0x112F0, 0x11450, 0x114D0, 0x11650, 0x116C0, 0x11730, 0x118E0, 0x11950,
    0x11C50, 0x11D50, 0x11DA0, 0x11F50, 0x16A60, 0x16AC0, 0x16B50, 0x1D7CE, 0x1D7D8, 0x1D7E2, 0x1D7EC,
    0x1D7F6, 0x1E140, 0x1E2F0, 0x1E4F0, 0x1E950, 0x1FBF0,
];

/// `unicodedata.decimal(c)`.
fn unicode_decimal(c: char) -> Option<u8> {
    let cp = c as u32;
    ND_ZEROS.iter().find(|&&z| (z..z + 10).contains(&cp)).map(|&z| (cp - z) as u8)
}

/// `float(s)` on an already stripped string: decimal or exponent forms, `inf`/`infinity`/`nan` in any
/// case with a sign, and single underscores between digits.
pub fn parse_float(s: &str) -> Option<f64> {
    let s = strip(s);
    if s.is_empty() {
        return None;
    }
    let chars: Vec<char> = s.chars().collect();
    let mut cleaned = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            let before = i > 0 && chars[i - 1].is_ascii_digit();
            let after = chars.get(i + 1).is_some_and(|c| c.is_ascii_digit());
            if !(before && after) {
                return None;
            }
            continue;
        }
        if let Some(d) = unicode_decimal(c).filter(|_| !c.is_ascii()) {
            // Python accepts any Unicode decimal digit.
            cleaned.push(char::from(b'0' + d));
            continue;
        }
        if !c.is_ascii() {
            return None;
        }
        cleaned.push(c);
    }
    let lower = cleaned.to_ascii_lowercase();
    let body = lower.trim_start_matches(['+', '-']);
    if lower.len() - body.len() > 1 {
        return None;
    }
    match body {
        "inf" | "infinity" | "nan" => return lower.parse::<f64>().ok(),
        _ => {}
    }
    // Rust also accepts "infinity"-like words and nothing else beyond Python's grammar for digits.
    if !body.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | '+' | '-')) {
        return None;
    }
    lower.parse::<f64>().ok()
}

/// `repr(s)` for a string.
pub fn repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
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
            c if (c as u32) < 0x20 || (0x7f..=0xa0).contains(&(c as u32)) || c == '\u{ad}' => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Mantissa digits (no point) and decimal exponent of `x` rounded to `digits` significant digits, or
/// the shortest round-trip digits when `digits` is None. `x` is finite and non-zero.
fn sci_parts(x: f64, digits: Option<usize>) -> (String, i32) {
    let s = match digits {
        Some(d) => format!("{:.*e}", d.saturating_sub(1), x.abs()),
        None => format!("{:e}", x.abs()),
    };
    let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
    (mant.replace('.', ""), exp.parse().unwrap_or(0))
}

fn special(x: f64) -> Option<String> {
    if x.is_nan() {
        Some("nan".into())
    } else if x.is_infinite() {
        Some(if x > 0.0 { "inf".into() } else { "-inf".into() })
    } else if x == 0.0 {
        Some(if x.is_sign_negative() { "-0".into() } else { "0".into() })
    } else {
        None
    }
}

fn exp_suffix(exp: i32) -> String {
    format!("e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
}

/// Place the point in `digits` (exponent `exp`) for positional notation.
fn positional(digits: &str, exp: i32) -> String {
    if exp < 0 {
        format!("0.{}{}", "0".repeat((-exp - 1) as usize), digits)
    } else {
        let int_len = exp as usize + 1;
        if digits.len() <= int_len {
            format!("{}{}", digits, "0".repeat(int_len - digits.len()))
        } else {
            format!("{}.{}", &digits[..int_len], &digits[int_len..])
        }
    }
}

/// `format(x, "g")`: six significant digits, trailing zeros dropped, exponent outside 1e-4..1e6.
pub fn format_g(x: f64) -> String {
    if let Some(s) = special(x) {
        return s;
    }
    let (digits, exp) = sci_parts(x, Some(6));
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let sign = if x < 0.0 { "-" } else { "" };
    if (-4..6).contains(&exp) {
        format!("{sign}{}", positional(digits, exp))
    } else {
        let (head, tail) = digits.split_at(1);
        let mant = if tail.is_empty() { head.to_string() } else { format!("{head}.{tail}") };
        format!("{sign}{mant}{}", exp_suffix(exp))
    }
}

/// `str(x)` / `repr(x)` for a float: the shortest round-trip digits, positional between 1e-4 and 1e16.
pub fn float_str(x: f64) -> String {
    if let Some(s) = special(x) {
        return if s.ends_with('0') && !s.contains('n') { format!("{s}.0") } else { s };
    }
    let (digits, exp) = sci_parts(x, None);
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let sign = if x < 0.0 { "-" } else { "" };
    if (-4..16).contains(&exp) {
        let p = positional(digits, exp);
        if p.contains('.') {
            format!("{sign}{p}")
        } else {
            format!("{sign}{p}.0")
        }
    } else {
        let (head, tail) = digits.split_at(1);
        let mant = if tail.is_empty() { head.to_string() } else { format!("{head}.{tail}") };
        format!("{sign}{mant}{}", exp_suffix(exp))
    }
}

fn home_from_passwd(name: Option<&str>) -> Option<String> {
    // SAFETY: getpwuid/getpwnam return a pointer into static storage or null; the string is copied out
    // at once.
    unsafe {
        let pw = match name {
            None => libc::getpwuid(libc::getuid()),
            Some(n) => {
                let c = CString::new(n).ok()?;
                libc::getpwnam(c.as_ptr())
            }
        };
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        Some(CStr::from_ptr((*pw).pw_dir).to_string_lossy().into_owned())
    }
}

/// `os.path.expanduser`: `~` from HOME (else the password database), `~user` from the password
/// database; anything else, or an unknown user, unchanged.
pub fn expanduser(path: &str) -> String {
    if !path.starts_with('~') {
        return path.to_string();
    }
    let i = path[1..].find('/').map(|i| i + 1).unwrap_or(path.len());
    let home = if i == 1 {
        match std::env::var_os("HOME") {
            Some(h) => Some(h.to_string_lossy().into_owned()),
            None => home_from_passwd(None),
        }
    } else {
        home_from_passwd(Some(&path[1..i]))
    };
    let Some(home) = home else { return path.to_string() };
    let joined = format!("{}{}", home.trim_end_matches('/'), &path[i..]);
    if joined.is_empty() {
        "/".into()
    } else {
        joined
    }
}

/// `str(pathlib.Path(s))` on POSIX: repeated slashes collapse (a leading pair stays), `.` parts and a
/// trailing slash go; `..` stays.
pub fn path_str(s: &str) -> String {
    if s.is_empty() {
        return ".".into();
    }
    let root = if s.starts_with("//") && !s.starts_with("///") {
        "//"
    } else if s.starts_with('/') {
        "/"
    } else {
        ""
    };
    let parts: Vec<&str> = s.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    let body = parts.join("/");
    if root.is_empty() && body.is_empty() {
        ".".into()
    } else {
        format!("{root}{body}")
    }
}

/// `pathlib.Path(s).expanduser()` as a path.
pub fn expand_path(s: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(path_str(&expanduser(&path_str(s))))
}
