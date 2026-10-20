//! The few Python builtins tool_gate leans on, with Python's semantics: truthiness, `str()`, `str.strip()`,
//! `str.split()`, `str.capitalize()`, `re`'s `\w`, `unicodedata.category(c) == "Cf"`, and
//! `json.dumps(v, ensure_ascii=False, sort_keys=True)`.

use serde_json::Value;
use unicode_general_category::{get_general_category, GeneralCategory as G};

/// Python's `bool(v)` for a JSON value.
pub fn py_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python's `str(v)` for a JSON value: a string is itself, anything else its repr.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => eve_app_registry::py_repr(other),
    }
}

/// Python's `str.isspace()` for one character: Unicode White_Space plus the four separators U+001C-U+001F.
pub(crate) fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python's `str.strip()`.
pub(crate) fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// Python's `str.split()` with no argument.
pub(crate) fn split(s: &str) -> Vec<&str> {
    s.split(is_space).filter(|w| !w.is_empty()).collect()
}

/// Python's `str.capitalize()`: the first character in title case, the rest lower case. Title case differs from
/// upper case for the Latin digraphs and the ligatures SpecialCasing lists; those are mapped here.
pub(crate) fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut out = match first {
        '\u{1c4}'..='\u{1c6}' => "\u{1c5}".to_string(),
        '\u{1c7}'..='\u{1c9}' => "\u{1c8}".to_string(),
        '\u{1ca}'..='\u{1cc}' => "\u{1cb}".to_string(),
        '\u{1f1}'..='\u{1f3}' => "\u{1f2}".to_string(),
        '\u{df}' => "Ss".to_string(),
        '\u{fb00}' => "Ff".to_string(),
        '\u{fb01}' => "Fi".to_string(),
        '\u{fb02}' => "Fl".to_string(),
        '\u{fb03}' => "Ffi".to_string(),
        '\u{fb04}' => "Ffl".to_string(),
        '\u{fb05}' | '\u{fb06}' => "St".to_string(),
        '\u{587}' => "\u{535}\u{582}".to_string(),
        '\u{fb13}' => "\u{544}\u{576}".to_string(),
        '\u{fb14}' => "\u{544}\u{565}".to_string(),
        '\u{fb15}' => "\u{544}\u{56b}".to_string(),
        '\u{fb16}' => "\u{54e}\u{576}".to_string(),
        '\u{fb17}' => "\u{544}\u{56d}".to_string(),
        c => c.to_uppercase().collect(),
    };
    out.push_str(&chars.as_str().to_lowercase());
    out
}

/// `unicodedata.category(c) == "Cf"`.
pub(crate) fn is_format(c: char) -> bool {
    get_general_category(c) == G::Format
}

/// `re`'s `\w` on a str pattern: a letter, a number, or the underscore (Python's `str.isalnum()` or `_`).
pub(crate) fn is_word(c: char) -> bool {
    c == '_'
        || matches!(
            get_general_category(c),
            G::UppercaseLetter
                | G::LowercaseLetter
                | G::TitlecaseLetter
                | G::ModifierLetter
                | G::OtherLetter
                | G::DecimalNumber
                | G::LetterNumber
                | G::OtherNumber
        )
}

/// Python's `json.dumps(v, ensure_ascii=False, sort_keys=True)`: ", " and ": " separators, keys sorted at every
/// level, floats as `repr(float)`, and only `"`, `\` and the C0 controls escaped.
pub(crate) fn dumps_sorted(v: &Value) -> String {
    let mut out = String::new();
    write(v, &mut out);
    out
}

fn write(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                out.push_str(&n.to_string());
            } else {
                out.push_str(&eve_app_registry::py_repr(v));
            }
        }
        Value::String(s) => write_str(s, out),
        Value::Array(a) => {
            out.push('[');
            for (i, item) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write(item, out);
            }
            out.push(']');
        }
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_str(k, out);
                out.push_str(": ");
                write(&m[k], out);
            }
            out.push('}');
        }
    }
}

fn write_str(s: &str, out: &mut String) {
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
            c => out.push(c),
        }
    }
    out.push('"');
}
