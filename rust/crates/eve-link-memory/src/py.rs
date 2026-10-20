//! The Python semantics `link_memory.py` leans on, so the port answers what the Python answers for any JSON a
//! phone or a memory store hands it: truthiness and `or`, `str()` of a value, `int()`, `list()` / iteration,
//! `dict()`, and whitespace (`strip`, `split`).

use anyhow::{anyhow, Result};
use serde_json::{Map, Value};

/// `str.isspace()` for one character: Unicode White_Space plus the four ASCII separators (FS, GS, RS, US)
/// that Python counts as whitespace and Rust does not.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `s.strip()` with no argument.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `" ".join(s.split())`.
pub fn squash_ws(s: &str) -> String {
    s.split(is_space).filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")
}

/// `s[:n]`, counted in characters (code points) as Python counts them.
pub fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Python's truth value of a JSON-shaped value; a missing key is `None`, so false.
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

/// `x.get(key)` on a JSON object; anything but an object has no keys.
pub fn get<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.as_object().and_then(|m| m.get(key))
}

/// `str(v or "")`: the value's `str()` when it is truthy, else "".
pub fn str_or_empty(v: Option<&Value>) -> String {
    match v {
        Some(x) if truthy(v) => py_str(x),
        _ => String::new(),
    }
}

/// `str(v or default)`.
pub fn str_or(v: Option<&Value>, default: &str) -> String {
    match v {
        Some(x) if truthy(v) => py_str(x),
        _ => default.to_string(),
    }
}

/// `int(v or 0)`: Err carries the Python exception, as `int()` would raise it.
pub fn int_or_zero(v: Option<&Value>) -> Result<i64> {
    match v {
        Some(x) if truthy(v) => py_int(x),
        _ => Ok(0),
    }
}

/// The Python type name of a JSON-shaped value, for the messages of the exceptions Python would raise.
fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
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

fn number_str(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        i.to_string()
    } else if let Some(u) = n.as_u64() {
        u.to_string()
    } else {
        float_repr(n.as_f64().unwrap_or(0.0))
    }
}

/// `str(value)` for a JSON-shaped value.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => number_str(n),
        Value::String(s) => s.clone(),
        Value::Array(_) | Value::Object(_) => py_repr(v),
    }
}

/// `repr(value)` for a JSON-shaped value.
pub fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => repr_str(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter().map(|(k, v)| format!("{}: {}", repr_str(k), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
        _ => py_str(v),
    }
}

/// `str.isprintable()` for one character, over the categories Python treats as not printable: controls,
/// format characters, separators other than the ASCII space, private use and non-characters.
fn is_printable(c: char) -> bool {
    let u = c as u32;
    !(u < 0x20
        || (0x7f..=0xa0).contains(&u)
        || u == 0xad
        || (0x600..=0x605).contains(&u)
        || u == 0x61c
        || u == 0x6dd
        || u == 0x70f
        || (0x890..=0x891).contains(&u)
        || u == 0x8e2
        || u == 0x1680
        || u == 0x180e
        || (0x2000..=0x200f).contains(&u)
        || (0x2028..=0x202f).contains(&u)
        || (0x205f..=0x2064).contains(&u)
        || (0x2066..=0x206f).contains(&u)
        || u == 0x3000
        || (0xd800..=0xf8ff).contains(&u)
        || (0xfdd0..=0xfdef).contains(&u)
        || u == 0xfeff
        || (0xfff9..=0xfffb).contains(&u)
        || (u & 0xfffe) == 0xfffe
        || u == 0x110bd
        || u == 0x110cd
        || (0x13430..=0x1343f).contains(&u)
        || (0x1bca0..=0x1bca3).contains(&u)
        || (0x1d173..=0x1d17a).contains(&u)
        || u == 0xe0001
        || (0xe0020..=0xe007f).contains(&u)
        || u >= 0xf0000)
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

/// `int(value)`: bools are 0 and 1, floats truncate, strings are read in base 10 (whitespace around, a sign,
/// single underscores between digits). A string that is not a number is a ValueError; a list, a dict or None a
/// TypeError.
pub fn py_int(v: &Value) -> Result<i64> {
    match v {
        Value::Bool(b) => Ok(*b as i64),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i)
            } else if let Some(u) = n.as_u64() {
                Ok(i64::try_from(u).unwrap_or(i64::MAX))
            } else {
                let f = n.as_f64().unwrap_or(0.0);
                if f.is_nan() {
                    Err(anyhow!("ValueError: cannot convert float NaN to integer"))
                } else if f.is_infinite() {
                    Err(anyhow!("OverflowError: cannot convert float infinity to integer"))
                } else {
                    Ok(f.trunc() as i64)
                }
            }
        }
        Value::String(s) => {
            parse_int(s).ok_or_else(|| anyhow!("ValueError: invalid literal for int() with base 10: {}", repr_str(s)))
        }
        other => Err(anyhow!(
            "TypeError: int() argument must be a string, a bytes-like object or a real number, not '{}'",
            type_name(other)
        )),
    }
}

fn parse_int(s: &str) -> Option<i64> {
    let t = strip(s);
    let (neg, body) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if body.is_empty() || body.starts_with('_') || body.ends_with('_') || body.contains("__") {
        return None;
    }
    let mut n: i64 = 0;
    for c in body.chars() {
        if c == '_' {
            continue;
        }
        let d = c.to_digit(10)? as i64;
        n = n.saturating_mul(10).saturating_add(d);
    }
    Some(if neg { -n } else { n })
}

/// `list(v or [])` / `for x in v or []`: a list as it is, a dict's keys, a string's characters. A truthy number or
/// `True` is not iterable (TypeError).
pub fn iter_or_empty(v: Option<&Value>) -> Result<Vec<Value>> {
    match v {
        Some(x) if truthy(v) => match x {
            Value::Array(a) => Ok(a.clone()),
            Value::Object(o) => Ok(o.keys().map(|k| Value::String(k.clone())).collect()),
            Value::String(s) => Ok(s.chars().map(|c| Value::String(c.to_string())).collect()),
            other => Err(anyhow!("TypeError: '{}' object is not iterable", type_name(other))),
        },
        _ => Ok(Vec::new()),
    }
}

/// A JSON object key for a Python dict key, as `json.dumps` writes it: a string as it is, a number as its `str()`,
/// `True`/`False`/`None` as `true`/`false`/`null`. A list or a dict cannot be a key (TypeError).
fn json_key(k: &Value) -> Result<String> {
    match k {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(number_str(n)),
        Value::Bool(b) => Ok(if *b { "true" } else { "false" }.into()),
        Value::Null => Ok("null".into()),
        other => Err(anyhow!("TypeError: unhashable type: '{}'", type_name(other))),
    }
}

/// `dict(v or {})`: a dict copied; a list of pairs (two-item lists or two-character strings) made a dict; anything
/// else raises as `dict()` would.
pub fn dict_or_empty(v: Option<&Value>) -> Result<Map<String, Value>> {
    let x = match v {
        Some(x) if truthy(v) => x,
        _ => return Ok(Map::new()),
    };
    match x {
        Value::Object(o) => Ok(o.clone()),
        Value::Array(items) => {
            let mut out = Map::new();
            for (i, item) in items.iter().enumerate() {
                let (k, val) = match item {
                    Value::Array(pair) if pair.len() == 2 => (json_key(&pair[0])?, pair[1].clone()),
                    Value::String(s) if s.chars().count() == 2 => {
                        let mut cs = s.chars();
                        let a = cs.next().map(String::from).unwrap_or_default();
                        let b = cs.next().map(String::from).unwrap_or_default();
                        (a, Value::String(b))
                    }
                    Value::Array(pair) => {
                        return Err(anyhow!(
                            "ValueError: dictionary update sequence element #{i} has length {}; 2 is required",
                            pair.len()
                        ))
                    }
                    Value::String(s) => {
                        return Err(anyhow!(
                            "ValueError: dictionary update sequence element #{i} has length {}; 2 is required",
                            s.chars().count()
                        ))
                    }
                    Value::Object(o) if o.len() == 2 => {
                        // iterating a dict gives its keys: {"a": .., "b": ..} is the pair ("a", "b")
                        let mut ks = o.keys();
                        let a = ks.next().cloned().unwrap_or_default();
                        let b = ks.next().cloned().unwrap_or_default();
                        (a, Value::String(b))
                    }
                    Value::Object(o) => {
                        return Err(anyhow!(
                            "ValueError: dictionary update sequence element #{i} has length {}; 2 is required",
                            o.len()
                        ))
                    }
                    other => {
                        return Err(anyhow!(
                            "TypeError: cannot convert dictionary update sequence element #{i} to a sequence ({})",
                            type_name(other)
                        ))
                    }
                };
                out.insert(k, val);
            }
            Ok(out)
        }
        Value::String(s) => Err(anyhow!(
            "ValueError: dictionary update sequence element #0 has length 1; 2 is required ({})",
            repr_str(s)
        )),
        other => Err(anyhow!("TypeError: '{}' object is not iterable", type_name(other))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn whitespace_matches_python_split_and_strip() {
        assert_eq!(squash_ws("  a\u{1f}b \n\tc\u{a0} "), "a b c");
        assert_eq!(strip("\u{1c} tok \u{2028}"), "tok");
        assert_eq!(take_chars("héllo", 2), "hé");
        assert_eq!(take_chars("hi", 5), "hi");
    }

    #[test]
    fn str_and_int_follow_python() {
        assert_eq!(py_str(&json!(true)), "True");
        assert_eq!(py_str(&json!(1.5)), "1.5");
        assert_eq!(py_str(&json!(1e20)), "1e+20");
        assert_eq!(py_str(&json!(3.0)), "3.0");
        assert_eq!(py_str(&json!(["a", 1, null])), "['a', 1, None]");
        assert_eq!(py_str(&json!({"k": "it's"})), "{'k': \"it's\"}");
        assert_eq!(py_int(&json!("  1_000 ")).unwrap(), 1000);
        assert_eq!(py_int(&json!(3.9)).unwrap(), 3);
        assert!(py_int(&json!("3.0")).unwrap_err().to_string().starts_with("ValueError"));
        assert!(py_int(&json!([1])).unwrap_err().to_string().starts_with("TypeError"));
        assert_eq!(int_or_zero(Some(&json!(""))).unwrap(), 0);
        assert_eq!(int_or_zero(None).unwrap(), 0);
    }

    #[test]
    fn iteration_and_dict_follow_python() {
        assert_eq!(iter_or_empty(Some(&json!({"a": 1, "b": 2}))).unwrap(), vec![json!("a"), json!("b")]);
        assert_eq!(iter_or_empty(Some(&json!("ab"))).unwrap(), vec![json!("a"), json!("b")]);
        assert_eq!(iter_or_empty(Some(&json!(0))).unwrap(), Vec::<Value>::new());
        assert!(iter_or_empty(Some(&json!(5))).is_err());
        assert_eq!(Value::Object(dict_or_empty(Some(&json!([["a", 1], "bc"]))).unwrap()), json!({"a": 1, "b": "c"}));
        assert_eq!(Value::Object(dict_or_empty(Some(&json!([[1, true]]))).unwrap()), json!({"1": true}));
        assert!(dict_or_empty(Some(&json!("x"))).is_err());
        assert!(dict_or_empty(Some(&json!(3))).is_err());
        assert_eq!(dict_or_empty(Some(&json!([]))).unwrap(), Map::new());
    }
}
