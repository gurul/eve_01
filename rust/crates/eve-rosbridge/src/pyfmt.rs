//! Python's text forms the protocol shows: `json.dumps(..., ensure_ascii=False)` for every
//! outbound frame, and `repr()` for the values quoted in status messages.

use serde_json::Value;

/// `json.dumps(value, ensure_ascii=False)`: `", "` and `": "` separators, floats as `repr`.
pub fn py_json(value: &Value) -> String {
    let mut out = String::new();
    write_json(value, &mut out);
    out
}

fn write_json(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => out.push_str(&i.to_string()),
            (_, Some(u)) => out.push_str(&u.to_string()),
            _ => out.push_str(&py_float_repr(n.as_f64().unwrap_or(f64::NAN))),
        },
        // serde_json escapes exactly what Python escapes with ensure_ascii=False:
        // `"`, `\`, and control characters below U+0020 (\b \f \n \r \t or \u00XX).
        Value::String(s) => out.push_str(&serde_json::to_string(s).unwrap_or_default()),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_json(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&serde_json::to_string(k).unwrap_or_default());
                out.push_str(": ");
                write_json(v, out);
            }
            out.push('}');
        }
    }
}

/// Python's `float.__repr__`: the shortest round-trip digits, fixed notation for exponents
/// -4..16, otherwise `1e+16` / `1.5e-05`.
pub fn py_float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".to_string();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0" } else { "0.0" }.to_string();
    }
    let sci = format!("{:e}", x.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let sign = if x < 0.0 { "-" } else { "" };
    if (-4..16).contains(&exp) {
        if exp >= 0 {
            let int_len = exp as usize + 1;
            let (int_part, frac) = if digits.len() > int_len {
                (digits[..int_len].to_string(), digits[int_len..].to_string())
            } else {
                (format!("{digits}{}", "0".repeat(int_len - digits.len())), "0".to_string())
            };
            format!("{sign}{int_part}.{frac}")
        } else {
            format!("{sign}0.{}{digits}", "0".repeat((-exp - 1) as usize))
        }
    } else {
        let mant = if digits.len() > 1 { format!("{}.{}", &digits[..1], &digits[1..]) } else { digits };
        let esign = if exp < 0 { '-' } else { '+' };
        format!("{sign}{mant}e{esign}{:02}", exp.abs())
    }
}

/// Python's `repr()` of a parsed JSON value: `None`, `True`, `'text'`, `[1, 2]`, `{'k': 1}`.
pub fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => i.to_string(),
            (_, Some(u)) => u.to_string(),
            _ => py_float_repr(n.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(s) => py_str_repr(s),
        Value::Array(items) => format!("[{}]", items.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter().map(|(k, v)| format!("{}: {}", py_str_repr(k), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Characters `str.isprintable()` rejects, beyond the C0/C1 controls: the space separators other
/// than U+0020, the line and paragraph separators, the common format characters, private use.
fn non_printable(c: char) -> bool {
    let u = c as u32;
    matches!(u,
        0x00..=0x1f | 0x7f..=0xa0 | 0xad
        | 0x0600..=0x0605 | 0x061c | 0x06dd | 0x070f | 0x08e2 | 0x1680 | 0x180e
        | 0x2000..=0x200f | 0x2028..=0x202f | 0x205f..=0x2064 | 0x2066..=0x206f
        | 0x3000 | 0xd800..=0xf8ff | 0xfeff | 0xfff9..=0xfffb
        | 0x110bd | 0x110cd | 0x1bca0..=0x1bca3 | 0x1d173..=0x1d17a
        | 0xe0001 | 0xe0020..=0xe007f | 0xf0000..=0x10ffff)
}

/// Python's `repr()` of a str.
pub fn py_str_repr(s: &str) -> String {
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
            c if non_printable(c) => {
                let u = c as u32;
                if u <= 0xff {
                    out.push_str(&format!("\\x{u:02x}"));
                } else if u <= 0xffff {
                    out.push_str(&format!("\\u{u:04x}"));
                } else {
                    out.push_str(&format!("\\U{u:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}
