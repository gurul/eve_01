//! The few Python behaviors these modules lean on, reproduced exactly so the files and answers match the Python
//! daemon's: `json.dumps`' default output, `float.__repr__`, `round`, `'%g'`, `statistics.median`,
//! `datetime.fromisoformat` (CPython 3.11+'s C parser) and `datetime.isoformat`.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Utc, Weekday};
use serde_json::Value;

// ---- numbers -------------------------------------------------------------------------------------------------------

/// `repr(x)` for a finite float: the shortest round-trip digits, fixed notation for exponents -4..16, else
/// `1e+16` style.
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
    let s = format!("{x:e}");
    let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let neg = mant.starts_with('-');
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if (-4..16).contains(&exp) {
        if exp >= 0 {
            let int_len = (exp + 1) as usize;
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
        out.push_str(&format!("e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs()));
    }
    out
}

/// `round(x, ndigits)`: the float nearest the correctly rounded decimal (ties to even on exact ties).
pub fn round_to(x: f64, ndigits: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.ndigits$}").parse().unwrap_or(x)
}

/// `round(x)`: an int, ties to even.
pub fn round_int(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// `f"{x:g}"`: six significant digits, trailing zeros dropped.
pub fn fmt_g(x: f64) -> String {
    if x == 0.0 {
        return if x.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let sci = format!("{x:.5e}");
    let (_, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let strip = |s: String| -> String {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        }
    };
    if (-4..6).contains(&exp) {
        let decimals = (5 - exp).max(0) as usize;
        strip(format!("{x:.decimals$}"))
    } else {
        let (mant, _) = sci.split_once('e').unwrap_or((&sci, "0"));
        format!("{}e{}{:02}", strip(mant.to_string()), if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

/// `statistics.median` of numbers (an empty list is the caller's to avoid).
pub fn median(values: &[f64]) -> f64 {
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// `math.ceil(x)` as an int.
pub fn ceil(x: f64) -> i64 {
    x.ceil() as i64
}

// ---- json ----------------------------------------------------------------------------------------------------------

/// `json.dumps(value)` with Python's defaults: `", "` and `": "` separators, `ensure_ascii`.
pub fn dumps(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value);
    out
}

fn write_value(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                out.push_str(&i.to_string());
            } else if let Some(u) = n.as_u64() {
                out.push_str(&u.to_string());
            } else {
                out.push_str(&float_repr(n.as_f64().unwrap_or(0.0)));
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
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_str(out, k);
                out.push_str(": ");
                write_value(out, v);
            }
            out.push('}');
        }
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
            ' '..='~' => out.push(c),
            _ => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
}

/// A JSON value as Python's `str()` would print it, for `str(args.get(...) or default)`: falsy values are `None`.
pub fn truthy_str(value: Option<&Value>) -> Option<String> {
    match value {
        None | Some(Value::Null) | Some(Value::Bool(false)) => None,
        Some(Value::Bool(true)) => Some("True".into()),
        Some(Value::String(s)) => (!s.is_empty()).then(|| s.clone()),
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                (i != 0).then(|| i.to_string())
            } else if let Some(u) = n.as_u64() {
                (u != 0).then(|| u.to_string())
            } else {
                let f = n.as_f64().unwrap_or(0.0);
                (f != 0.0).then(|| float_repr(f))
            }
        }
        Some(Value::Array(a)) => (!a.is_empty()).then(|| dumps(&Value::Array(a.clone())).replace("\"", "'")),
        Some(Value::Object(o)) => (!o.is_empty()).then(|| dumps(&Value::Object(o.clone())).replace("\"", "'")),
    }
}

// ---- text ----------------------------------------------------------------------------------------------------------

/// `" ".join(re.sub(r"[\x00-\x1f\x7f-\x9f  ]", " ", raw).split())`: one clean line.
pub fn one_line(raw: &str) -> String {
    let replaced: String = raw
        .chars()
        .map(|c| match c {
            '\u{00}'..='\u{1f}' | '\u{7f}'..='\u{9f}' | '\u{2028}' | '\u{2029}' => ' ',
            _ => c,
        })
        .collect();
    replaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Python's `str.isspace()`: Unicode White_Space plus the four separators `\x1c`-`\x1f`.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python's `text.strip()`.
pub fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// Python's `text.split()`: runs of whitespace separate, none empty.
pub fn split(text: &str) -> Vec<&str> {
    text.split(is_space).filter(|w| !w.is_empty()).collect()
}

/// Python's `len(text)`: code points.
pub fn chars_len(text: &str) -> usize {
    text.chars().count()
}

// ---- time ----------------------------------------------------------------------------------------------------------

/// What `datetime.fromisoformat` returns: a wall time and, when the string had one, its UTC offset in microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsoDateTime {
    pub naive: NaiveDateTime,
    pub offset_us: Option<i64>,
}

impl IsoDateTime {
    /// `astimezone(timezone.utc)` for an aware time; None when naive or out of datetime's range.
    pub fn to_utc(&self) -> Option<DateTime<Utc>> {
        let off = self.offset_us?;
        let utc = self.naive.checked_sub_signed(Duration::microseconds(off))?;
        if !(1..=9999).contains(&utc.year()) {
            return None; // Python: OverflowError
        }
        Some(utc.and_utc())
    }
}

fn byte(b: &[u8], i: usize) -> u8 {
    b.get(i).copied().unwrap_or(0)
}

fn parse_digits(b: &[u8], mut p: usize, n: usize) -> Option<(i64, usize)> {
    let mut var = 0i64;
    for _ in 0..n {
        let c = byte(b, p);
        p += 1;
        if !c.is_ascii_digit() {
            return None;
        }
        var = var * 10 + i64::from(c - b'0');
    }
    Some((var, p))
}

fn find_separator(b: &[u8]) -> Option<usize> {
    let len = b.len();
    if len == 7 {
        return Some(7);
    }
    if byte(b, 4) == b'-' {
        if byte(b, 5) == b'W' {
            if len < 8 {
                return None;
            }
            if len > 8 && byte(b, 8) == b'-' {
                if len == 9 {
                    return None;
                }
                if len > 10 && byte(b, 10).is_ascii_digit() {
                    return Some(8);
                }
                return Some(10);
            }
            Some(8)
        } else {
            Some(10)
        }
    } else if byte(b, 4) == b'W' {
        let mut idx = 7;
        while idx < len && byte(b, idx).is_ascii_digit() {
            idx += 1;
        }
        if idx < 9 {
            return Some(idx);
        }
        Some(if idx % 2 == 0 { 7 } else { 8 })
    } else {
        Some(8)
    }
}

fn parse_date(b: &[u8], len: usize) -> Option<(i64, i64, i64)> {
    let (year, mut p) = parse_digits(b, 0, 4)?;
    let uses_sep = byte(b, p) == b'-';
    if uses_sep {
        p += 1;
    }
    if byte(b, p) == b'W' {
        p += 1;
        let (week, q) = parse_digits(b, p, 2)?;
        p = q;
        let day = if p < len {
            if uses_sep {
                if byte(b, p) != b'-' {
                    return None;
                }
                p += 1;
            }
            parse_digits(b, p, 1)?.0
        } else {
            1
        };
        if !(1..=9999).contains(&year) || !(1..=7).contains(&day) {
            return None;
        }
        let wd = Weekday::try_from((day - 1) as u8).ok()?;
        let d = NaiveDate::from_isoywd_opt(year as i32, week as u32, wd)?;
        return Some((i64::from(d.year()), i64::from(d.month()), i64::from(d.day())));
    }
    let (month, q) = parse_digits(b, p, 2)?;
    p = q;
    if uses_sep {
        if byte(b, p) != b'-' {
            return None;
        }
        p += 1;
    }
    let (day, _) = parse_digits(b, p, 2)?;
    Some((year, month, day))
}

/// A string `datetime.fromisoformat` refuses (its ValueError).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotIso;

/// CPython 3.13's `parse_hh_mm_ss_ff` (the daemon's interpreter): Err for a malformed part, Ok((0 or 1, [h, m, s, us])) where 1 means more text
/// follows before the string ends.
fn parse_hh_mm_ss_ff(b: &[u8], start: usize, end: usize) -> Result<(i32, [i64; 4]), ()> {
    let mut vals = [0i64; 4];
    let mut p = start;
    let mut has_sep = true;
    for (i, val) in vals.iter_mut().take(3).enumerate() {
        let (v, q) = parse_digits(b, p, 2).ok_or(())?;
        *val = v;
        p = q;
        let c = byte(b, p);
        p += 1;
        if i == 0 {
            has_sep = c == b':';
        }
        if p >= end {
            return Ok((i32::from(c != 0), vals));
        } else if has_sep && c == b':' {
            continue;
        } else if c == b'.' || c == b',' {
            break;
        } else if !has_sep {
            p -= 1;
        } else {
            return Err(());
        }
    }
    let to_parse = end.saturating_sub(p).min(6);
    if to_parse == 0 {
        return Err(());
    }
    let (mut us, mut q) = parse_digits(b, p, to_parse).ok_or(())?;
    const CORRECTION: [i64; 5] = [100_000, 10_000, 1000, 100, 10];
    if to_parse < 6 {
        us *= CORRECTION[to_parse - 1];
    }
    vals[3] = us;
    while byte(b, q).is_ascii_digit() {
        q += 1;
    }
    Ok((i32::from(byte(b, q) != 0), vals))
}

fn parse_time(b: &[u8], start: usize, end: usize) -> Result<([i64; 4], Option<i64>), ()> {
    let mut tz = start;
    loop {
        let c = byte(b, tz);
        if c == b'Z' || c == b'+' || c == b'-' {
            break;
        }
        tz += 1;
        if tz >= end {
            break;
        }
    }
    let (rv, comps) = parse_hh_mm_ss_ff(b, start, tz)?;
    if tz == end {
        return if rv == 1 { Err(()) } else { Ok((comps, None)) };
    }
    if byte(b, tz) == b'Z' {
        return if byte(b, tz + 1) != 0 { Err(()) } else { Ok((comps, Some(0))) };
    }
    let sign = if byte(b, tz) == b'-' { -1 } else { 1 };
    let (rv, tzc) = parse_hh_mm_ss_ff(b, tz + 1, end)?;
    if rv != 0 {
        return Err(());
    }
    let secs = sign * (tzc[0] * 3600 + tzc[1] * 60 + tzc[2]);
    if secs == 0 {
        return Ok((comps, Some(0))); // a zero offset is UTC, whatever its microseconds
    }
    Ok((comps, Some(secs * 1_000_000 + sign * tzc[3])))
}

/// `datetime.fromisoformat(text)` as CPython 3.11+ parses it; Err for what it refuses.
pub fn fromisoformat(text: &str) -> Result<IsoDateTime, NotIso> {
    if chars_len(text) < 7 {
        return Err(NotIso);
    }
    let b = text.as_bytes();
    let len = b.len();
    let sep = find_separator(b).ok_or(NotIso)?;
    let (year, month, day) = parse_date(b, sep).ok_or(NotIso)?;
    let mut time = [0i64; 4];
    let mut offset = None;
    if len > sep {
        let c = b[sep];
        let step = if c & 0x80 == 0 {
            1
        } else {
            match c & 0xf0 {
                0xe0 => 3,
                0xf0 => 4,
                _ => 2,
            }
        };
        let (comps, off) = parse_time(b, sep + step, len).map_err(|_| NotIso)?;
        time = comps;
        offset = off;
    }
    if let Some(off) = offset {
        if off.abs() >= 86_400 * 1_000_000 {
            return Err(NotIso); // timezone(): strictly between -24 h and 24 h
        }
    }
    if !(1..=9999).contains(&year) {
        return Err(NotIso);
    }
    let date = NaiveDate::from_ymd_opt(year as i32, month as u32, day as u32).ok_or(NotIso)?;
    if time[0] > 23 || time[1] > 59 || time[2] > 59 {
        return Err(NotIso);
    }
    let t = NaiveTime::from_hms_micro_opt(time[0] as u32, time[1] as u32, time[2] as u32, time[3] as u32).ok_or(NotIso)?;
    Ok(IsoDateTime { naive: NaiveDateTime::new(date, t), offset_us: offset })
}

/// `dt.isoformat()` for an aware UTC datetime: `2026-10-12T17:55:00+00:00`, microseconds when not zero.
pub fn isoformat_utc(dt: &DateTime<Utc>) -> String {
    let n = dt.naive_utc();
    let mut out = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        n.year(),
        n.month(),
        n.day(),
        n.hour(),
        n.minute(),
        n.second()
    );
    let us = n.nanosecond() / 1000;
    if us != 0 {
        out.push_str(&format!(".{us:06}"));
    }
    out.push_str("+00:00");
    out
}

/// `dt.timestamp()` for an aware datetime: microseconds over 1e6, as Python computes it.
pub fn timestamp(dt: &DateTime<Utc>) -> f64 {
    dt.timestamp_micros() as f64 / 1e6
}

/// Python datetimes hold microseconds: drop anything finer.
pub fn trunc_micros(dt: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(dt.timestamp_micros()).unwrap_or(dt)
}

/// `datetime.fromtimestamp(ts, timezone.utc)`: the instant, rounded to microseconds half to even.
pub fn from_timestamp(ts: f64) -> DateTime<Utc> {
    let us = (ts * 1e6).round_ties_even();
    let us = if us.is_finite() { us as i64 } else { 0 };
    DateTime::from_timestamp_micros(us).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_float_repr_matches_python() {
        for (x, want) in [
            (0.0, "0.0"),
            (10.0, "10.0"),
            (37.7749, "37.7749"),
            (-122.4194, "-122.4194"),
            (1e-5, "1e-05"),
            (0.0001, "0.0001"),
            (1e16, "1e+16"),
            (1234567890123456.0, "1234567890123456.0"),
            (1.5e300, "1.5e+300"),
            (1760290500.123456, "1760290500.123456"),
        ] {
            assert_eq!(float_repr(x), want, "{x}");
        }
    }

    #[test]
    fn test_dumps_matches_python() {
        let v = json!({"lat": 1.5, "n": 1, "place": "Café \u{1F600} \"x\"\n", "none": null, "list": [true, false]});
        assert_eq!(
            dumps(&v),
            "{\"lat\": 1.5, \"n\": 1, \"place\": \"Caf\\u00e9 \\ud83d\\ude00 \\\"x\\\"\\n\", \"none\": null, \"list\": [true, false]}"
        );
        assert_eq!(dumps(&json!({})), "{}");
    }

    #[test]
    fn test_round_and_g() {
        assert_eq!(round_to(2.25, 1), 2.2);
        assert_eq!(round_to(37.774915, 5), 37.77492);
        assert_eq!(round_int(2.5), 2);
        assert_eq!(round_int(3.5), 4);
        for (x, want) in [(-90.0, "-90"), (100000.0, "100000"), (350.0, "350"), (0.0, "0"), (1e6, "1e+06"), (0.5, "0.5")] {
            assert_eq!(fmt_g(x), want);
        }
    }

    /// Every case below was run through CPython 3.13's `datetime.fromisoformat` (the daemon's interpreter): the
    /// expected value is its `isoformat()`, or None for a ValueError.
    #[test]
    fn test_fromisoformat_matches_cpython() {
        let cases: [(&str, Option<&str>); 29] = [
            ("2026-10-12T17:55:00+00:00", Some("2026-10-12T17:55:00+00:00")),
            ("2026-10-12 17:55+0000", Some("2026-10-12T17:55:00+00:00")),
            ("20261012T175500+0000", Some("2026-10-12T17:55:00+00:00")),
            ("2026-10-12T17+00:00", Some("2026-10-12T17:00:00+00:00")),
            ("2026-10-12T17:55:00.1234567+00:00", Some("2026-10-12T17:55:00.123456+00:00")),
            ("2026-W42-1T10:00+00:00", Some("2026-10-12T10:00:00+00:00")),
            ("2026-10-12T17:55:00,5+00:00", Some("2026-10-12T17:55:00.500000+00:00")),
            ("2026-10-12T24:00:00+00:00", None),
            ("2026-10-12T17:55:00+05", Some("2026-10-12T17:55:00+05:00")),
            ("2026-10-12T1755+00:00", Some("2026-10-12T17:55:00+00:00")),
            ("2026-10-12X17:55+00:00", Some("2026-10-12T17:55:00+00:00")),
            ("2026-10-12T17:55:00.12+00:00", Some("2026-10-12T17:55:00.120000+00:00")),
            ("2026-10-12T17:55:00+05:30:15", Some("2026-10-12T17:55:00+05:30:15")),
            ("2026-10-12", Some("2026-10-12T00:00:00")),
            ("2026-10-12T17:55:00-00:00", Some("2026-10-12T17:55:00+00:00")),
            ("2026-10-12T17:55:00+24:00", None),
            ("2026-10-12T17:55:00+0530", Some("2026-10-12T17:55:00+05:30")),
            ("2026-10-12T17:55:00Z", Some("2026-10-12T17:55:00+00:00")),
            ("2026-10-12T17:55:00.5Z", Some("2026-10-12T17:55:00.500000+00:00")),
            ("2026-10-12T175500Z", Some("2026-10-12T17:55:00+00:00")),
            ("2026-10-12T17:55:00.+00:00", Some("2026-10-12T17:55:00+00:00")),
            ("2026-1012T17:55+00:00", None),
            ("2026-10-12T17:5+00:00", None),
            ("2026-02-30T00:00+00:00", None),
            ("2026-10-12T17:55:00 +00:00", Some("2026-10-12T17:55:00+00:00")),
            ("2026-10-12T17:55:00.123+00", Some("2026-10-12T17:55:00.123000+00:00")),
            ("2026-10-12T17:55:60+00:00", None),
            ("2026-10-12T17:55:00+00:00:00.5", Some("2026-10-12T17:55:00+00:00")),
            ("2026-10-12T17:55:00+00", Some("2026-10-12T17:55:00+00:00")),
        ];
        for (text, want) in cases {
            let got = fromisoformat(text).ok();
            match (got, want) {
                (None, None) => {}
                (Some(g), Some(w)) => {
                    let n = g.naive;
                    let base = n.format("%Y-%m-%dT%H:%M:%S").to_string();
                    let us = n.nanosecond() / 1000;
                    // fixed-offset strings are compared by instant: the offset may carry seconds
                    let wp = fromisoformat_reference(w);
                    assert_eq!((base, us, g.offset_us.is_some()), wp, "{text}");
                }
                (g, w) => panic!("{text}: got {g:?}, want {w:?}"),
            }
        }
    }

    /// The fields of an `isoformat()` output, read by hand (not with the parser under test).
    fn fromisoformat_reference(w: &str) -> (String, u32, bool) {
        let base = w[..19].to_string();
        let rest = &w[19..];
        let (us, tz) = if let Some(r) = rest.strip_prefix('.') { (r[..6].parse().unwrap(), &r[6..]) } else { (0, rest) };
        (base, us, !tz.is_empty())
    }

    /// Python's json.loads reads back every float it wrote to the bit; serde_json does only with float_roundtrip
    /// (without it the stay's center in meta drifted by an ulp, and the visits' coordinates with it).
    #[test]
    fn test_floats_read_back_to_the_bit() {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..20_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let f = 41.0 + (x % 1_000_000_007) as f64 / 1_000_000_007.0 * 0.1;
            let text = format!("{{\"lat\": {}}}", float_repr(f));
            let back: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(back["lat"].as_f64().unwrap().to_bits(), f.to_bits(), "{text}");
        }
    }

    #[test]
    fn test_one_line_and_isoformat() {
        assert_eq!(one_line("  Market St\n\tSan Francisco\u{2028} "), "Market St San Francisco");
        let dt = DateTime::from_timestamp_micros(1_760_291_700_000_001).unwrap();
        assert_eq!(isoformat_utc(&dt), "2025-10-12T17:55:00.000001+00:00");
        assert_eq!(median(&[3.0, 1.0, 2.0, 10.0]), 2.5);
        // str.strip() and str.split() take \x1c-\x1f as whitespace too (Rust's trim does not)
        assert_eq!(strip("\u{1f} 2026 \u{1c}"), "2026");
        assert_eq!(split("a\u{1d}b  c\u{3000}"), ["a", "b", "c"]);
    }
}
