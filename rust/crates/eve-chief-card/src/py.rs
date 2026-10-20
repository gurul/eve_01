//! The Python semantics chief_card.py leans on, so the port answers what the Python answers: whitespace
//! (`str.split`, `str.strip`), `s[:n]`, `repr()` of the JSON values a refusal names, `str.casefold`, `str.isdigit`
//! and `int()` on a digit string, the `\d` digits `strptime` reads, and `os.path.realpath` (Python 3.13,
//! strict=False).

use std::collections::HashMap;

use serde_json::Value;
use unicode_general_category::{get_general_category, GeneralCategory as G};

/// `str.isspace()` for one character: Unicode White_Space plus the four ASCII separators (FS, GS, RS, US)
/// that Python counts as whitespace and Rust does not.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `s.strip()` with no argument.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `s.split()` with no argument.
pub fn split(s: &str) -> impl Iterator<Item = &str> {
    s.split(is_space).filter(|p| !p.is_empty())
}

/// `" ".join(s.split())`.
pub fn squash(s: &str) -> String {
    split(s).collect::<Vec<_>>().join(" ")
}

/// The first `n` characters (code points) of `s`: Python's `s[:n]`.
pub fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// `len(s)`: code points, not bytes.
pub fn len(s: &str) -> usize {
    s.chars().count()
}

/// Python's truth value of a JSON value; a missing key is `None`, so false.
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `str.casefold()`: lowercasing plus the full case foldings that differ from it in Latin and Greek text
/// (the matched consequential words and a goal's first word are compared against ASCII words).
pub fn casefold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            'ß' | 'ẞ' => out.push_str("ss"),
            'ſ' => out.push('s'),
            'ς' => out.push('σ'),
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

// ---- digits ----------------------------------------------------------------------------------------

/// The characters `str.isdigit()` accepts that are not decimal digits (Numeric_Type=Digit: superscripts,
/// circled digits, ...), from CPython 3.13's unicodedata (Unicode 15.1). `int()` refuses them.
const DIGIT_NOT_DECIMAL: &[(u32, u32)] = &[
    (0xB2, 0xB3), (0xB9, 0xB9), (0x1369, 0x1371), (0x19DA, 0x19DA), (0x2070, 0x2070), (0x2074, 0x2079),
    (0x2080, 0x2089), (0x2460, 0x2468), (0x2474, 0x247C), (0x2488, 0x2490), (0x24EA, 0x24EA), (0x24F5, 0x24FD),
    (0x24FF, 0x24FF), (0x2776, 0x277E), (0x2780, 0x2788), (0x278A, 0x2792), (0x10A40, 0x10A43),
    (0x10E60, 0x10E68), (0x11052, 0x1105A), (0x1F100, 0x1F10A),
];

fn is_decimal(c: char) -> bool {
    get_general_category(c) == G::DecimalNumber
}

/// The value of a decimal digit (`str.isdecimal()`, the `\d` of a str pattern), any script. Decimal digits are
/// always encoded in contiguous runs of ten from 0 to 9, so the value is the distance from the start of the
/// contiguous stretch, modulo 10.
pub fn decimal_value(c: char) -> Option<u32> {
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

/// `str.isdigit()` for one character.
fn is_digit(c: char) -> bool {
    is_decimal(c) || DIGIT_NOT_DECIMAL.iter().any(|&(lo, hi)| (lo..=hi).contains(&(c as u32)))
}

/// `s.isdigit()`: not empty, every character a digit.
pub fn isdigit(s: &str) -> bool {
    !s.is_empty() && s.chars().all(is_digit)
}

/// The value of a run of decimal digits (spaces skipped, as `int()` strips them), saturating.
pub fn digits_value(s: &str) -> u64 {
    s.chars()
        .filter_map(decimal_value)
        .fold(0u64, |n, d| n.saturating_mul(10).saturating_add(u64::from(d)))
}

/// `int(s)` for a string whose stripped form `isdigit()`: Python's ValueError text when a digit is not decimal
/// or there are more than 4300 digits, else the value (saturating at u64::MAX, past every cap here).
pub fn int_of_digits(s: &str) -> Result<u64, String> {
    let body = strip(s);
    if body.chars().any(|c| !is_decimal(c)) {
        // CPython: "invalid literal for int() with base %d: %.200R"
        return Err(format!("invalid literal for int() with base 10: {}", take_chars(&repr_str(s), 200)));
    }
    let count = body.chars().count();
    if count > 4300 {
        return Err(format!(
            "Exceeds the limit (4300 digits) for integer string conversion: value has {count} digits; use \
             sys.set_int_max_str_digits() to increase the limit"
        ));
    }
    Ok(digits_value(body))
}

// ---- repr ------------------------------------------------------------------------------------------

/// `repr(float)`: the shortest round-trip digits, fixed between 1e-4 and 1e16, else scientific.
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let sci = format!("{x:e}");
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let neg = mant.starts_with('-');
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let sign = if neg { "-" } else { "" };
    if (-4..16).contains(&exp) {
        let point = exp + 1;
        let body = if point <= 0 {
            format!("0.{}{}", "0".repeat((-point) as usize), digits)
        } else if point as usize >= digits.len() {
            format!("{}{}.0", digits, "0".repeat(point as usize - digits.len()))
        } else {
            format!("{}.{}", &digits[..point as usize], &digits[point as usize..])
        };
        format!("{sign}{body}")
    } else {
        let m = if digits.len() > 1 { format!("{}.{}", &digits[..1], &digits[1..]) } else { digits };
        format!("{sign}{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

/// `f"{x:.{prec}f}"`: fixed notation, correctly rounded, with Python's spelling of nan and inf.
pub fn fixed(x: f64, prec: usize) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    format!("{x:.prec$}")
}

/// `repr(value)` for a JSON value (`None` for a missing key).
pub fn repr(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".into(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => "False".into(),
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                float_repr(n.as_f64().unwrap_or(0.0))
            }
        }
        Some(Value::String(s)) => repr_str(s),
        Some(Value::Array(a)) => format!("[{}]", a.iter().map(|x| repr(Some(x))).collect::<Vec<_>>().join(", ")),
        Some(Value::Object(o)) => format!(
            "{{{}}}",
            o.iter().map(|(k, x)| format!("{}: {}", repr_str(k), repr(Some(x)))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// `repr(tuple_of_str)`: `('a', 'b')`, `('a',)`, `()`.
pub fn tuple_repr<S: AsRef<str>>(items: &[S]) -> String {
    let parts: Vec<String> = items.iter().map(|s| repr_str(s.as_ref())).collect();
    match parts.len() {
        1 => format!("({},)", parts[0]),
        _ => format!("({})", parts.join(", ")),
    }
}

/// `str.isprintable()` for one character: false for the categories Python counts as not printable (Cc, Cf,
/// Cs, Co, Cn, Zl, Zp, and Zs other than the ASCII space).
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

/// `repr(str)`: single quotes unless the text holds a single quote and no double quote.
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
            c if is_printable(c) => out.push(c),
            c if (c as u32) < 0x100 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) < 0x10000 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push(quote);
    out
}

// ---- paths -----------------------------------------------------------------------------------------

/// `os.lstat`'s ValueError for a path with a NUL in it: Python raises it out of `realpath` (it is not an
/// OSError, so strict=False does not swallow it).
pub const NUL_IN_PATH: &str = "lstat: embedded null character in path";

/// `os.path.expanduser("~")` as `Path.home()` reads it: $HOME when it is set (trailing slashes cut, "/" when
/// that leaves nothing), else the account's home folder.
pub fn home_dir() -> String {
    match std::env::var_os("HOME") {
        Some(h) => {
            let h = h.to_string_lossy().into_owned();
            let cut = h.trim_end_matches('/');
            if cut.is_empty() {
                "/".into()
            } else {
                cut.to_string()
            }
        }
        None => dirs::home_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into()),
    }
}

/// `os.path.realpath(filename)` with strict=False, as CPython 3.13 walks it: every symlink resolved, a part
/// that does not exist (or cannot be read) kept as written, a loop left where it starts. Err only for a NUL in
/// the path, which Python raises as ValueError.
pub fn realpath(filename: &str) -> Result<String, String> {
    const SEP: char = '/';
    // None marks "the symlink below me is resolved"; the symlink's own path follows it.
    let mut rest: Vec<Option<String>> = filename.split(SEP).rev().map(|s| Some(s.to_string())).collect();
    let mut part_count = rest.len();
    let mut path = if filename.starts_with(SEP) {
        "/".to_string()
    } else {
        std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "/".into())
    };
    let mut seen: HashMap<String, Option<String>> = HashMap::new();
    while part_count > 0 {
        let name = match rest.pop() {
            Some(Some(name)) => name,
            Some(None) => {
                if let Some(Some(link)) = rest.pop() {
                    seen.insert(link, Some(path.clone()));
                }
                continue;
            }
            None => break,
        };
        part_count -= 1;
        if name.is_empty() || name == "." {
            continue;
        }
        if name == ".." {
            let cut = path.rfind(SEP).unwrap_or(0);
            path = if cut == 0 { "/".to_string() } else { path[..cut].to_string() };
            continue;
        }
        let newpath = if path == "/" { format!("/{name}") } else { format!("{path}/{name}") };
        if newpath.contains('\0') {
            return Err(NUL_IN_PATH.to_string());
        }
        let meta = match std::fs::symlink_metadata(&newpath) {
            Ok(m) => m,
            Err(_) => {
                path = newpath;
                continue;
            }
        };
        if !meta.file_type().is_symlink() {
            path = newpath;
            continue;
        }
        if let Some(known) = seen.get(&newpath) {
            match known {
                Some(resolved) => path = resolved.clone(),
                // not resolved yet: a symlink loop, left as it is
                None => path = newpath,
            }
            continue;
        }
        let target = match std::fs::read_link(&newpath) {
            Ok(t) => t.to_string_lossy().into_owned(),
            Err(_) => {
                path = newpath;
                continue;
            }
        };
        if target.starts_with(SEP) {
            path = "/".to_string();
        }
        seen.insert(newpath.clone(), None);
        rest.push(Some(newpath));
        rest.push(None);
        let parts: Vec<Option<String>> = target.split(SEP).rev().map(|s| Some(s.to_string())).collect();
        part_count += parts.len();
        rest.extend(parts);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn whitespace_is_pythons() {
        assert_eq!(squash(" a\u{1c}b\u{a0} c\n"), "a b c");
        assert_eq!(strip("\u{1f} x \u{3000}"), "x");
        assert_eq!(take_chars("éàü", 2), "éà");
    }

    #[test]
    fn digits_read_as_int_reads_them() {
        assert_eq!(decimal_value('٣'), Some(3));
        assert_eq!(decimal_value('7'), Some(7));
        assert_eq!(decimal_value('\u{1D7D9}'), Some(1)); // mathematical double-struck one, mid-stretch
        assert_eq!(decimal_value('²'), None);
        assert!(isdigit("²3") && !isdigit("") && !isdigit("3a"));
        assert_eq!(int_of_digits(" ٣ "), Ok(3));
        assert_eq!(int_of_digits("²").unwrap_err(), "invalid literal for int() with base 10: '²'");
        assert!(int_of_digits(&"1".repeat(5000)).unwrap_err().contains("value has 5000 digits"));
    }

    #[test]
    fn repr_matches_python() {
        assert_eq!(repr(Some(&json!(1.5))), "1.5");
        assert_eq!(repr(Some(&json!(1e20))), "1e+20");
        assert_eq!(repr(None), "None");
        assert_eq!(repr(Some(&json!({"a": [true, null]}))), "{'a': [True, None]}");
        assert_eq!(repr_str("it's"), "\"it's\"");
        assert_eq!(repr_str("a\u{1}\u{200b}"), "'a\\x01\\u200b'");
        assert_eq!(tuple_repr(&["research"]), "('research',)");
        assert_eq!(tuple_repr(&["a", "b"]), "('a', 'b')");
        assert_eq!(tuple_repr::<&str>(&[]), "()");
    }

    #[test]
    fn fixed_rounds_as_python_does() {
        // f"{x:.2f}" rounds the exact binary value, ties to even
        assert_eq!(fixed(0.125, 2), "0.12");
        assert_eq!(fixed(0.375, 2), "0.38");
        assert_eq!(fixed(2.005, 2), "2.00");
        assert_eq!(fixed(2.5, 0), "2");
        assert_eq!(fixed(0.5, 0), "0");
        assert_eq!(fixed(-0.0, 2), "-0.00");
        assert_eq!(fixed(f64::NAN, 0), "nan");
    }

    #[test]
    fn casefold_folds_the_long_s() {
        assert_eq!(casefold("ſEND"), "send");
        assert_eq!(casefold("Straße"), "strasse");
    }

    #[test]
    fn realpath_follows_links_and_keeps_missing_parts() {
        let tmp = tempfile::tempdir().unwrap();
        let base = realpath(tmp.path().to_str().unwrap()).unwrap();
        std::fs::create_dir(format!("{base}/real")).unwrap();
        std::os::unix::fs::symlink(format!("{base}/real"), format!("{base}/link")).unwrap();
        assert_eq!(realpath(&format!("{base}/link/x/../y")).unwrap(), format!("{base}/real/y"));
        assert_eq!(realpath(&format!("{base}//./missing/")).unwrap(), format!("{base}/missing"));
        std::os::unix::fs::symlink(format!("{base}/loop"), format!("{base}/loop")).unwrap();
        assert_eq!(realpath(&format!("{base}/loop/z")).unwrap(), format!("{base}/loop/z"));
        assert_eq!(realpath("/a\0b").unwrap_err(), NUL_IN_PATH);
        assert_eq!(realpath("/..").unwrap(), "/");
    }
}
