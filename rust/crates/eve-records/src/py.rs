//! The few Python builtins records.py leans on that eve-transcripts' `py` does not cover: `str()` and `repr()` of a
//! JSON value (what a model reply is), truthiness, iteration of whatever a reply holds, `re.escape`, and the
//! exception class name a log line prints.

use serde_json::Value;

/// `os.environ` as a map. `vars_os`, lossy: `std::env::vars` panics on a variable that is not UTF-8, where
/// `os.environ` never raises.
pub fn environ() -> std::collections::HashMap<String, String> {
    std::env::vars_os().map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned())).collect()
}

/// Python truthiness of a JSON value: null, false, 0, "", [] and {} are false.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `str.isprintable()` for one character: false for the categories Python counts as not printable (Cc, Cf, Cs,
/// Co, Cn, Zl, Zp, and Zs other than the ASCII space).
fn is_printable(c: char) -> bool {
    use unicode_general_category::{get_general_category, GeneralCategory as G};
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

/// `repr(s)` of a Python `str`: single quotes unless the text holds a single quote and no double quote, and every
/// character `str.isprintable()` rejects escaped (`\xNN`, `\uNNNN`, `\UNNNNNNNN`).
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

fn num_str(n: &serde_json::Number) -> String {
    if n.is_i64() || n.is_u64() {
        n.to_string()
    } else {
        let x = n.as_f64().unwrap_or(0.0);
        if x.is_infinite() {
            if x > 0.0 { "inf".into() } else { "-inf".into() }
        } else if x.is_nan() {
            "nan".into()
        } else {
            eve_transcripts::py::float_repr(x)
        }
    }
}

/// `repr()` of a JSON value as Python prints the value `json.loads` made of it.
pub fn repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => num_str(n),
        Value::String(s) => repr_str(s),
        Value::Array(a) => format!("[{}]", a.iter().map(repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => {
            format!("{{{}}}", o.iter().map(|(k, v)| format!("{}: {}", repr_str(k), repr(v))).collect::<Vec<_>>().join(", "))
        }
    }
}

/// `str()` of a JSON value: a string is itself, anything else its repr.
pub fn str_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => repr(other),
    }
}

/// `str(v or "")`: "" for a missing or falsy value.
pub fn str_or_empty(v: Option<&Value>) -> String {
    match v {
        Some(v) if truthy(v) => str_of(v),
        _ => String::new(),
    }
}

/// `for x in (v or [])`: a list's items, a string's characters, a dict's keys; nothing for a falsy value. (A
/// number or `True` is not iterable in Python and raises; here it is nothing.)
pub fn iter_of(v: Option<&Value>) -> Vec<Value> {
    match v {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::String(s)) => s.chars().map(|c| Value::String(c.to_string())).collect(),
        Some(Value::Object(o)) => o.keys().map(|k| Value::String(k.clone())).collect(),
        _ => Vec::new(),
    }
}

/// `re.escape` (Python 3.7+): only the regex metacharacters and whitespace are escaped.
pub fn re_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "()[]{}?*+-|^$\\.&~# \t\n\r\u{0b}\u{0c}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The exception class name Python would log (`type(e).__name__`) for an error that reached a log line.
pub fn error_name(e: &anyhow::Error) -> String {
    for cause in e.chain() {
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            return eve_transcripts::py::os_error_name(io).to_string();
        }
        if let Some(g) = cause.downcast_ref::<crate::records::GitError>() {
            return g.py_name().to_string();
        }
        if let Some(r) = cause.downcast_ref::<crate::records::CallError>() {
            return r.py_name().to_string();
        }
        if let Some(o) = cause.downcast_ref::<eve_openrouter::Error>() {
            return match o {
                eve_openrouter::Error::NoKey(_) => "NoKey",
                eve_openrouter::Error::Status { status, .. } => match status {
                    400 => "BadRequestError",
                    401 => "AuthenticationError",
                    403 => "PermissionDeniedError",
                    404 => "NotFoundError",
                    409 => "ConflictError",
                    422 => "UnprocessableEntityError",
                    429 => "RateLimitError",
                    s if *s >= 500 => "InternalServerError",
                    _ => "APIStatusError",
                },
                eve_openrouter::Error::Http(h) if h.is_timeout() => "APITimeoutError",
                eve_openrouter::Error::Http(_) => "APIConnectionError",
                eve_openrouter::Error::Json(_) => "JSONDecodeError",
                eve_openrouter::Error::Invalid(_) => "ValueError",
            }
            .to_string();
        }
        if cause.downcast_ref::<serde_json::Error>().is_some() {
            return "JSONDecodeError".into();
        }
    }
    "Exception".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repr_and_str_match_python() {
        assert_eq!(repr_str("nope"), "'nope'");
        assert_eq!(repr_str("it's"), "\"it's\"");
        assert_eq!(repr_str("a'b\"c"), "'a\\'b\"c'");
        assert_eq!(repr_str("a\nb\\"), "'a\\nb\\\\'");
        // what str.isprintable() rejects is escaped (values from Python's repr)
        assert_eq!(repr_str("a\u{ad}b"), "'a\\xadb'");
        assert_eq!(repr_str("x\u{2028}y"), "'x\\u2028y'");
        assert_eq!(repr_str("nb\u{a0}sp"), "'nb\\xa0sp'");
        assert_eq!(repr_str("\u{200b}"), "'\\u200b'");
        assert_eq!(repr_str("e\u{e0001}"), "'e\\U000e0001'");
        assert_eq!(repr_str("caf\u{e9}"), "'caf\u{e9}'");
        assert_eq!(repr_str("\u{7f}\u{85}"), "'\\x7f\\x85'");
        assert_eq!(str_of(&json!(null)), "None");
        assert_eq!(str_of(&json!(5)), "5");
        assert_eq!(str_of(&json!(1.5)), "1.5");
        assert_eq!(str_of(&json!(true)), "True");
        assert_eq!(str_of(&json!(["a", 1])), "['a', 1]");
        assert_eq!(str_of(&json!({"k": null})), "{'k': None}");
        assert_eq!(str_or_empty(Some(&json!(0))), "");
        assert_eq!(str_or_empty(None), "");
    }

    #[test]
    fn re_escape_matches_python() {
        assert_eq!(re_escape("https://"), "https://");
        assert_eq!(re_escape("git@"), "git@");
        assert_eq!(re_escape("~"), "\\~");
        assert_eq!(re_escape("."), "\\.");
        assert_eq!(re_escape("a-b c"), "a\\-b\\ c");
    }

    #[test]
    fn iteration_follows_python() {
        assert_eq!(iter_of(Some(&json!("ab"))), vec![json!("a"), json!("b")]);
        assert_eq!(iter_of(Some(&json!({"x": 1}))), vec![json!("x")]);
        assert!(iter_of(Some(&json!(null))).is_empty());
        assert!(iter_of(None).is_empty());
    }
}
