//! Python's view of the daemon's JSON answers, as cli.py reads and prints them: `dict.get`, truthiness, `str()`,
//! `json.dumps` with its separators, `indent` and `ensure_ascii`, and the few format specs the CLI uses.

use serde_json::Value;

static NULL: Value = Value::Null;

/// `d.get(key)` on a JSON value (None when it is not a dict or has no such key).
pub fn get<'a>(v: &'a Value, key: &str) -> &'a Value {
    v.get(key).unwrap_or(&NULL)
}

/// Python truthiness.
pub fn truthy(v: &Value) -> bool {
    eve_hooks::py_truthy(v)
}

/// `str(v)`.
pub fn pstr(v: &Value) -> String {
    eve_hooks::py_str(v)
}

/// `d.get(key, default)`: the value when the key is there (even null), else the default.
pub fn get_or(v: &Value, key: &str, default: Value) -> Value {
    match v.get(key) {
        Some(x) => x.clone(),
        None => default,
    }
}

/// `d.get(key) or default` as a string: `str()` of the value when it is truthy.
pub fn or_str(v: &Value, key: &str, default: &str) -> String {
    let x = get(v, key);
    if truthy(x) {
        pstr(x)
    } else {
        default.to_string()
    }
}

/// `x is True`.
pub fn is_true(v: &Value) -> bool {
    matches!(v, Value::Bool(true))
}

/// A number as Python holds it (int or float), for arithmetic in a format spec.
pub fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// `f"{n:,}"` for an int (thousands separated by commas); any other value as `str()`.
pub fn commas(v: &Value) -> String {
    let Some(i) = v.as_i64() else { return pstr(v) };
    let digits = i.unsigned_abs().to_string();
    let mut out = String::new();
    for (k, c) in digits.chars().enumerate() {
        if k > 0 && (digits.len() - k) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if i < 0 {
        format!("-{out}")
    } else {
        out
    }
}

fn escape_into(out: &mut String, s: &str, ascii: bool) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if ascii && (c as u32) > 0x7e => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{u:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn float_text(f: f64) -> String {
    if f.is_nan() {
        "NaN".into()
    } else if f.is_infinite() {
        if f > 0.0 { "Infinity".into() } else { "-Infinity".into() }
    } else {
        eve_vault::pystr::float_repr(f)
    }
}

fn dump_into(out: &mut String, v: &Value, indent: Option<usize>, level: usize, ascii: bool) {
    let newline = |out: &mut String, level: usize| {
        if let Some(n) = indent {
            out.push('\n');
            out.push_str(&" ".repeat(n * level));
        }
    };
    let item_sep = if indent.is_some() { "," } else { ", " };
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.is_f64() {
                out.push_str(&float_text(n.as_f64().unwrap_or(0.0)));
            } else {
                out.push_str(&n.to_string());
            }
        }
        Value::String(s) => escape_into(out, s, ascii),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (k, item) in items.iter().enumerate() {
                if k > 0 {
                    out.push_str(item_sep);
                }
                newline(out, level + 1);
                dump_into(out, item, indent, level + 1, ascii);
            }
            newline(out, level);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (k, (key, item)) in map.iter().enumerate() {
                if k > 0 {
                    out.push_str(item_sep);
                }
                newline(out, level + 1);
                escape_into(out, key, ascii);
                out.push_str(": ");
                dump_into(out, item, indent, level + 1, ascii);
            }
            newline(out, level);
            out.push('}');
        }
    }
}

/// `json.dumps(v, indent=indent, ensure_ascii=ascii)`.
pub fn dumps(v: &Value, indent: Option<usize>, ascii: bool) -> String {
    let mut out = String::new();
    dump_into(&mut out, v, indent, 0, ascii);
    out
}

/// `time.strftime(fmt, time.localtime(secs))`.
pub fn local_strftime(secs: f64, fmt: &str) -> Option<String> {
    use chrono::TimeZone;
    if !secs.is_finite() {
        return None;
    }
    let whole = secs.floor();
    let dt = chrono::Local.timestamp_opt(whole as i64, 0).single()?;
    Some(dt.format(fmt).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dumps_as_python() {
        let v = json!({"a": [1, 2.5, "é"], "b": {}, "c": null});
        assert_eq!(dumps(&v, None, true), r#"{"a": [1, 2.5, "\u00e9"], "b": {}, "c": null}"#);
        assert_eq!(dumps(&v, None, false), r#"{"a": [1, 2.5, "é"], "b": {}, "c": null}"#);
        assert_eq!(dumps(&json!({"x": [1]}), Some(2), true), "{\n  \"x\": [\n    1\n  ]\n}");
    }

    #[test]
    fn commas_group_thousands() {
        assert_eq!(commas(&json!(1834567)), "1,834,567");
        assert_eq!(commas(&json!(12)), "12");
        assert_eq!(commas(&json!(-1000)), "-1,000");
        assert_eq!(commas(&json!("?")), "?");
    }
}
