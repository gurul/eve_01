//! The Python semantics chief_receipt.py leans on, so the port answers what the Python answers: whitespace
//! (`str.strip`, `str.split`), `s[:n]`, `str.casefold`, `int()` on a string, `str()` and truth of the JSON
//! values an adapter is handed, and `f"{x:.Nf}"` (Python 3.13).

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

/// `str.casefold()`: Unicode full case folding.
pub fn casefold(s: &str) -> String {
    caseless::default_case_fold_str(s)
}

/// The first `n` characters (code points) of `s`: Python's `s[:n]`.
pub fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// The last `n` items of a list: Python's `xs[-n:]` (all of it when shorter; nothing when n is 0).
pub fn last<T>(xs: Vec<T>, n: usize) -> Vec<T> {
    let skip = xs.len().saturating_sub(n);
    xs.into_iter().skip(skip).collect()
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

/// `str(v or "")`: "" for a falsy value, the text of a string, else Python's `str()` of the value.
pub fn str_or_empty(v: Option<&Value>) -> String {
    match v {
        _ if !truthy(v) => String::new(),
        Some(Value::String(s)) => s.clone(),
        other => repr(other),
    }
}

// ---- int() ---------------------------------------------------------------------------------------

fn is_decimal(c: char) -> bool {
    get_general_category(c) == G::DecimalNumber
}

/// The value of a decimal digit (`str.isdecimal()`), any script. Decimal digits are always encoded in
/// contiguous runs of ten from 0 to 9, so the value is the distance from the start of the stretch, modulo 10.
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

/// `int(s)` for a str, base 10: surrounding whitespace, one sign, decimal digits of any script with single
/// underscores between them. Err is Python's ValueError text. The value saturates at i128's range, far past any
/// count a check can hold.
pub fn int(s: &str) -> Result<i128, String> {
    // CPython: "invalid literal for int() with base %d: %.200R"
    let invalid = || format!("invalid literal for int() with base 10: {}", take_chars(&repr_str(s), 200));
    let body = strip(s);
    let (neg, digits) = match body.chars().next() {
        Some('+') => (false, &body[1..]),
        Some('-') => (true, &body[1..]),
        _ => (false, body),
    };
    let mut after_underscore = true; // no leading underscore
    let mut count = 0usize;
    let mut value: i128 = 0;
    for c in digits.chars() {
        if c == '_' {
            if after_underscore {
                return Err(invalid());
            }
            after_underscore = true;
            continue;
        }
        let d = decimal_value(c).ok_or_else(invalid)?;
        after_underscore = false;
        count += 1;
        value = value.saturating_mul(10).saturating_add(i128::from(d));
    }
    if count == 0 || after_underscore {
        return Err(invalid());
    }
    if count > 4300 {
        return Err(format!(
            "Exceeds the limit (4300 digits) for integer string conversion: value has {count} digits; use \
             sys.set_int_max_str_digits() to increase the limit"
        ));
    }
    Ok(if neg { -value } else { value })
}

// ---- formatting ------------------------------------------------------------------------------------

/// `f"{x:.{prec}f}"`: fixed notation, correctly rounded (half to even on the exact value, as CPython), with
/// Python's spelling of nan and inf.
pub fn fixed(x: f64, prec: usize) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    format!("{x:.prec$}")
}

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

/// `repr(value)` for a JSON value (`None` for a missing key); `str()` of a non-string value is the same.
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn int_reads_what_python_int_reads() {
        assert_eq!(int("3"), Ok(3));
        assert_eq!(int(" 12\n"), Ok(12));
        assert_eq!(int("+7"), Ok(7));
        assert_eq!(int("-2"), Ok(-2));
        assert_eq!(int("1_0"), Ok(10));
        assert_eq!(int("007"), Ok(7));
        assert_eq!(int("\u{0663}"), Ok(3)); // ARABIC-INDIC DIGIT THREE
        assert_eq!(int("\u{ff11}\u{ff12}"), Ok(12)); // fullwidth
        for bad in ["", " ", "abc", "1__0", "_1", "1_", "+", "- 1", "1.5", "\u{b2}", "0x10"] {
            assert!(int(bad).is_err(), "{bad:?}");
        }
        assert_eq!(int("abc"), Err("invalid literal for int() with base 10: 'abc'".into()));
        assert_eq!(int("it's"), Err("invalid literal for int() with base 10: \"it's\"".into()));
        assert_eq!(int(&"9".repeat(60)), Ok(i128::MAX));
    }

    #[test]
    fn str_of_a_json_value_is_pythons() {
        assert_eq!(str_or_empty(None), "");
        assert_eq!(str_or_empty(Some(&json!(0))), "");
        assert_eq!(str_or_empty(Some(&json!(false))), "");
        assert_eq!(str_or_empty(Some(&json!("x"))), "x");
        assert_eq!(str_or_empty(Some(&json!(12))), "12");
        assert_eq!(str_or_empty(Some(&json!(true))), "True");
        assert_eq!(str_or_empty(Some(&json!(1e16))), "1e+16");
        assert_eq!(str_or_empty(Some(&json!(0.5))), "0.5");
        assert_eq!(str_or_empty(Some(&json!(["a", 1, null]))), "['a', 1, None]");
        assert_eq!(str_or_empty(Some(&json!({"k": "v"}))), "{'k': 'v'}");
    }

    #[test]
    fn casefold_and_whitespace_are_pythons() {
        assert_eq!(casefold("Straße ORDER"), "strasse order");
        assert_eq!(squash("a\u{1f}b\u{a0} c"), "a b c");
        assert_eq!(strip("\u{1c} x \u{3000}"), "x");
        assert_eq!(fixed(0.125, 2), "0.12");
        assert_eq!(fixed(f64::NAN, 0), "nan");
        assert_eq!(last(vec![1, 2, 3], 2), vec![2, 3]);
        assert_eq!(last(vec![1], 5), vec![1]);
    }
}
