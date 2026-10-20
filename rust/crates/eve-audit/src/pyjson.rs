//! The slice of Python's `json` and float behavior the audit log depends on,
//! so Rust writes the same bytes `json.dumps(entry, ensure_ascii=False)` does
//! and reads every line `json.loads` accepts.

use serde_json::Value;

/// `json.dumps(value, ensure_ascii=False)`: `", "` and `": "` separators,
/// non-ASCII kept raw, floats in Python's repr.
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

/// `repr(float)` as `json.dumps` writes it: shortest round-trip digits,
/// scientific below 1e-4 and from 1e16, `NaN` / `Infinity` / `-Infinity`.
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
    let sci = shortest_sci(x.abs());
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

/// The shortest scientific form of a finite positive `x` that reads back as
/// `x`, nearest to `x` among those (ties to even), as `repr` picks it.
/// Rust's `{:e}` also gives the shortest digits but can take the other
/// neighbour on an exact tie (`593349443380239.25` → `…239.3`, where Python
/// writes `…239.2`); exact formatting at each precision rounds half to even.
fn shortest_sci(x: f64) -> String {
    for prec in 0..17 {
        let s = format!("{:.*e}", prec, x);
        if s.parse::<f64>().ok() == Some(x) {
            return s;
        }
    }
    format!("{:.16e}", x)
}

/// Python's `round(x, ndigits)` for a float: correctly rounded, ties to even
/// on the exact binary value; NaN and infinities come back unchanged.
pub fn round_ndigits(x: f64, ndigits: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    let r: f64 = format!("{:.*}", ndigits, x).parse().unwrap_or(x);
    // Python keeps the sign of a value that rounds to zero.
    if r == 0.0 && x.is_sign_negative() {
        -0.0
    } else {
        r
    }
}

/// `json.loads` for one line: strict JSON, plus the bare `NaN`, `Infinity`
/// and `-Infinity` Python accepts (read back as `null`, since a JSON value
/// cannot hold them). `None` where Python raises `ValueError`.
pub fn loads_lenient(s: &str) -> Option<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(s) {
        return Some(v);
    }
    let replaced = replace_nonfinite(s)?;
    serde_json::from_str::<Value>(&replaced).ok()
}

/// Swap bare `NaN` / `Infinity` / `-Infinity` tokens outside strings for
/// `null`; `None` when there are none (nothing to retry).
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
        let token = ["-Infinity", "Infinity", "NaN"].into_iter().find(|t| rest.starts_with(t));
        if let Some(t) = token {
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
