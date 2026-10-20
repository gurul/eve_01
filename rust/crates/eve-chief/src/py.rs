//! The Python semantics chief.py leans on: `str()`, `repr()`, `strip`, `round`, the format specs it uses
//! (`.2f`, `,.2f`, `.0f`, `g`), `datetime.fromtimestamp` and a naive datetime's `timestamp()`.

use chrono::{Local, NaiveDate, NaiveDateTime, TimeDelta, TimeZone};
use serde_json::Value;

/// Python's `str.isspace`: Unicode White_Space plus the separators U+001C..U+001F.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()`.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `str.rstrip(chars)`.
pub fn rstrip_chars<'a>(s: &'a str, chars: &str) -> &'a str {
    s.trim_end_matches(|c| chars.contains(c))
}

/// `s[:n]` in code points.
pub fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Python truthiness of a JSON value (None for a missing key).
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `repr(str)`: single quotes unless the text holds a single quote and no double one; backslash, the quote and
/// the control characters escaped.
pub fn repr_str(s: &str) -> String {
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
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if ('\u{80}'..='\u{a0}').contains(&c) => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `repr(x)` of a decoded JSON value.
pub fn repr_value(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => number_str(n),
        Value::String(s) => repr_str(s),
        Value::Array(a) => format!("[{}]", a.iter().map(repr_value).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter().map(|(k, v)| format!("{}: {}", repr_str(k), repr_value(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

fn number_str(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        i.to_string()
    } else if let Some(u) = n.as_u64() {
        u.to_string()
    } else {
        float_repr(n.as_f64().unwrap_or(0.0))
    }
}

/// `str(x)` of a decoded JSON value.
pub fn str_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => repr_value(other),
    }
}

/// `str(x or "")`.
pub fn str_or_empty(v: Option<&Value>) -> String {
    match v {
        Some(x) if truthy(Some(x)) => str_value(x),
        _ => String::new(),
    }
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

/// `f"{x:.{prec}f}"`.
pub fn fixed(x: f64, prec: usize) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    format!("{x:.prec$}")
}

/// `f"{x:,.2f}"`: fixed with two decimals and a comma between each group of three integer digits.
pub fn thousands_2f(x: f64) -> String {
    let s = fixed(x, 2);
    let (sign, body) = match s.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", s.as_str()),
    };
    let Some((int, frac)) = body.split_once('.') else {
        return s;
    };
    if !int.chars().all(|c| c.is_ascii_digit()) {
        return s;
    }
    let mut grouped = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("{sign}{grouped}.{frac}")
}

/// `f"{x:g}"`: six significant digits, trailing zeros dropped, scientific below 1e-4 and from 1e6.
pub fn fmt_g(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let sci = format!("{x:.5e}");
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if (-4..6).contains(&exp) {
        let decimals = (5 - exp).max(0) as usize;
        let s = format!("{x:.decimals$}");
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        }
    } else {
        let m = if mant.contains('.') { mant.trim_end_matches('0').trim_end_matches('.') } else { mant };
        format!("{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
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

/// `json.dumps(v, ensure_ascii=False, separators=(",", ":"))`, floats as Python writes them.
pub fn dumps_compact(v: &Value) -> String {
    let mut out = String::new();
    write_json(v, &mut out);
    out
}

fn write_json(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.is_f64() {
                let f = n.as_f64().unwrap_or(0.0);
                out.push_str(&if f.is_nan() {
                    "NaN".to_string()
                } else if f.is_infinite() {
                    if f > 0.0 { "Infinity".to_string() } else { "-Infinity".to_string() }
                } else {
                    float_repr(f)
                });
            } else {
                out.push_str(&n.to_string());
            }
        }
        Value::String(s) => out.push_str(&serde_json::to_string(s).unwrap_or_default()),
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(x, out);
            }
            out.push(']');
        }
        Value::Object(o) => {
            out.push('{');
            for (i, (k, x)) in o.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).unwrap_or_default());
                out.push(':');
                write_json(x, out);
            }
            out.push('}');
        }
    }
}

// ---- time --------------------------------------------------------------------------------------

/// `time.time()`.
pub fn time_now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// `datetime.fromtimestamp(ts)`: the naive local time, to the microsecond (half to even). None where Python
/// raises (not finite, out of range).
pub fn fromtimestamp(ts: f64) -> Option<NaiveDateTime> {
    if !ts.is_finite() {
        return None;
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
    if !(-9.223_372_036_854_776e18..9.223_372_036_854_776e18).contains(&intpart) {
        return None;
    }
    let at = Local.timestamp_opt(intpart as i64, 0).earliest()?;
    let naive = at.naive_local();
    if !(1..=9999).contains(&chrono::Datelike::year(&naive)) {
        return None;
    }
    Some(naive + TimeDelta::microseconds(floatpart as i64))
}

/// `date.fromtimestamp(ts)`.
pub fn local_date(ts: f64) -> Option<NaiveDate> {
    fromtimestamp(ts).map(|t| t.date())
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

/// `secrets.token_hex(n)`.
pub fn token_hex(n: usize) -> String {
    let bytes: Vec<u8> = (0..n).map(|_| rand::random::<u8>()).collect();
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_follow_python() {
        assert_eq!(fmt_g(0.5), "0.5");
        assert_eq!(fmt_g(1.0), "1");
        assert_eq!(fmt_g(2.0), "2");
        assert_eq!(fmt_g(0.25), "0.25");
        assert_eq!(fmt_g(1e-05), "1e-05");
        assert_eq!(fmt_g(1234567.0), "1.23457e+06");
        assert_eq!(fmt_g(100000.0), "100000");
        assert_eq!(thousands_2f(549.0), "549.00");
        assert_eq!(thousands_2f(1234567.5), "1,234,567.50");
        assert_eq!(float_repr(549.0), "549.0");
        assert_eq!(float_repr(1e16), "1e+16");
        assert_eq!(float_repr(0.0001), "0.0001");
        assert_eq!(repr_str("x"), "'x'");
        assert_eq!(repr_str("it's"), "\"it's\"");
        assert_eq!(round_ndigits(0.12345, 4), 0.1235);
        assert_eq!(rstrip_chars("pick one. ", ". "), "pick one");
    }
}
