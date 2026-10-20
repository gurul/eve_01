//! Python's reading of the JSON the app-server sends: `dict.get`, `d[k]`, `in`, `==` and hashing, each
//! failing where Python raises, so a malformed protocol message takes the same branch it takes in Python
//! (an `AttributeError` in the reader is "Invalid Codex event stream.", in an approval it is a refusal).

use serde_json::{Map, Value};

/// The exception Python would raise: its class name and a short detail.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{class}: {detail}")]
pub struct PyError {
    pub class: &'static str,
    pub detail: String,
}

impl PyError {
    pub fn new(class: &'static str, detail: impl Into<String>) -> Self {
        Self { class, detail: detail.into() }
    }
}

pub type PyResult<T> = Result<T, PyError>;

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

/// `obj` as a dict, or the `AttributeError` that `obj.get(...)` raises on anything else.
pub fn dict(obj: &Value) -> PyResult<&Map<String, Value>> {
    obj.as_object()
        .ok_or_else(|| PyError::new("AttributeError", format!("'{}' object has no attribute 'get'", type_name(obj))))
}

/// `obj.get(key)`: `None` (as `Value::Null`) when absent.
pub fn get<'a>(obj: &'a Value, key: &str) -> PyResult<&'a Value> {
    Ok(dict(obj)?.get(key).unwrap_or(&Value::Null))
}

/// `obj.get(key, default)`.
pub fn get_or<'a>(obj: &'a Value, key: &str, default: &'a Value) -> PyResult<&'a Value> {
    Ok(dict(obj)?.get(key).unwrap_or(default))
}

/// `obj[key]` with a string key: `KeyError` on a dict without it, `TypeError` on anything else.
pub fn index<'a>(obj: &'a Value, key: &str) -> PyResult<&'a Value> {
    match obj {
        Value::Object(m) => m.get(key).ok_or_else(|| PyError::new("KeyError", format!("'{key}'"))),
        other => Err(PyError::new("TypeError", format!("'{}' object is not subscriptable by str", type_name(other)))),
    }
}

/// Python truthiness.
pub fn truthy(v: &Value) -> bool {
    eve_vault::pystr::truthy(v)
}

/// `needle in container` for a string needle: a dict's keys, a list's items, a str's substrings.
pub fn contains(container: &Value, needle: &str) -> PyResult<bool> {
    match container {
        Value::Object(m) => Ok(m.contains_key(needle)),
        Value::Array(a) => Ok(a.iter().any(|v| v.as_str() == Some(needle))),
        Value::String(s) => Ok(s.contains(needle)),
        other => Err(PyError::new("TypeError", format!("argument of type '{}' is not iterable", type_name(other)))),
    }
}

/// Whether Python can hash the value (a set or dict lookup raises `TypeError` otherwise).
pub fn hashable(v: &Value) -> PyResult<()> {
    match v {
        Value::Array(_) | Value::Object(_) => {
            Err(PyError::new("TypeError", format!("unhashable type: '{}'", type_name(v))))
        }
        _ => Ok(()),
    }
}

/// `value in {a, b, ...}` over a set of strings (and `None` when `with_none`): hashing first, as Python does.
pub fn in_str_set(v: &Value, set: &[&str], with_none: bool) -> PyResult<bool> {
    hashable(v)?;
    Ok(match v {
        Value::Null => with_none,
        Value::String(s) => set.contains(&s.as_str()),
        _ => false,
    })
}

fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

/// Python `==` between two JSON values: numbers (and bools) by value, containers by content.
pub fn eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_i64(), y.as_i64(), x.as_u64(), y.as_u64()) {
            (Some(i), Some(j), _, _) => i == j,
            (_, _, Some(i), Some(j)) => i == j,
            _ => x.as_f64() == y.as_f64(),
        },
        (Value::Bool(_), Value::Number(_)) | (Value::Number(_), Value::Bool(_)) => number(a) == number(b),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| eq(p, q)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| eq(v, w)))
        }
        _ => a == b,
    }
}

/// `type(v) is int`: an integer, never a bool or a float.
pub fn exact_int(v: &Value) -> Option<i128> {
    match v {
        Value::Number(n) if !n.is_f64() => n.as_i64().map(i128::from).or_else(|| n.as_u64().map(i128::from)),
        _ => None,
    }
}

/// A dict key Python's `json.dumps` writes for this value (`str` as is, `int`, `float`, `True`/`False`/`None`
/// as JSON spells them); a list or dict cannot be a key (`TypeError: unhashable`).
pub fn json_key(v: &Value) -> PyResult<String> {
    hashable(v)?;
    Ok(match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) if n.is_f64() => eve_vault::pystr::float_repr(n.as_f64().unwrap_or(f64::NAN)),
        Value::Number(n) => n.to_string(),
        _ => unreachable!("hashable() refused containers"),
    })
}

/// Python's `str.strip()`.
pub fn strip(s: &str) -> &str {
    eve_vault::pystr::strip(s)
}

/// `json.dumps(obj)` with Python's defaults: `", "` and `": "` separators, non-ASCII escaped.
pub fn dumps(v: &Value) -> String {
    eve_vault::pystr::ensure_ascii(&eve_vault_mcp::dumps(v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn get_and_index_raise_where_python_raises() {
        assert_eq!(get(&json!({"a": 1}), "b").unwrap(), &Value::Null);
        assert_eq!(get(&json!(null), "a").unwrap_err().class, "AttributeError");
        assert_eq!(index(&json!({}), "a").unwrap_err().class, "KeyError");
        assert_eq!(index(&json!([1]), "a").unwrap_err().class, "TypeError");
    }

    #[test]
    fn contains_follows_the_container_type() {
        assert!(contains(&json!({"js": {}}), "js").unwrap());
        assert!(contains(&json!(["js"]), "js").unwrap());
        assert!(contains(&json!("a js b"), "js").unwrap());
        assert!(contains(&json!(3), "js").is_err());
        assert!(in_str_set(&json!([1]), &["a"], false).is_err());
        assert!(in_str_set(&Value::Null, &["a"], true).unwrap());
    }

    #[test]
    fn equality_is_pythons() {
        assert!(eq(&json!(1), &json!(1.0)));
        assert!(eq(&json!(true), &json!(1)));
        assert!(!eq(&json!("1"), &json!(1)));
        assert!(eq(&json!({"a": [1, 2.0]}), &json!({"a": [1.0, 2]})));
    }

    #[test]
    fn dumps_is_python_json_dumps() {
        assert_eq!(dumps(&json!({"id": 1, "method": "x", "params": {"t": "é"}})),
                   "{\"id\": 1, \"method\": \"x\", \"params\": {\"t\": \"\\u00e9\"}}");
    }

    #[test]
    fn json_keys_are_written_as_python_writes_them() {
        assert_eq!(json_key(&json!(1)).unwrap(), "1");
        assert_eq!(json_key(&json!(true)).unwrap(), "true");
        assert_eq!(json_key(&json!(null)).unwrap(), "null");
        assert_eq!(json_key(&json!(1.5)).unwrap(), "1.5");
        assert!(json_key(&json!([1])).is_err());
    }
}
