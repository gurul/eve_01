//! The few Python string and value semantics plan_contract leans on: `str.split()` / `strip()` whitespace,
//! `str.casefold()`, slicing by code point, `\w` word characters, `str()` / `repr()` of a JSON value and its
//! truthiness.

use serde_json::Value;
use unicode_general_category::{get_general_category, GeneralCategory};

/// Python's `str.isspace` for one character: Unicode White_Space plus the C0 separators U+001C..U+001F.
pub(crate) fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `" ".join(text.split())`.
pub(crate) fn squash(text: &str) -> String {
    text.split(is_space)
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `text.strip()`.
pub(crate) fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// `text[:limit]`, counted in code points.
pub(crate) fn take(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((at, _)) => text[..at].to_string(),
        None => text.to_string(),
    }
}

/// `len(text)` in code points.
pub(crate) fn len(text: &str) -> usize {
    text.chars().count()
}

/// `str.isalnum()` for one character: a letter or a number (general categories L* and N*).
pub(crate) fn is_alnum(c: char) -> bool {
    use GeneralCategory::*;
    matches!(
        get_general_category(c),
        UppercaseLetter
            | LowercaseLetter
            | TitlecaseLetter
            | ModifierLetter
            | OtherLetter
            | DecimalNumber
            | LetterNumber
            | OtherNumber
    )
}

/// Python's `\w` in a str pattern: alphanumerics and the underscore.
pub(crate) fn is_word(c: char) -> bool {
    c == '_' || is_alnum(c)
}

/// `str.casefold()`: the full lowercase mapping of each character, except where Unicode's full case folding
/// differs from it (the table below is every such character in Python 3.13's Unicode database).
pub(crate) fn casefold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match fold_special(c) {
            Some(folded) => out.push_str(&folded),
            None => out.extend(c.to_lowercase()),
        }
    }
    out
}

fn fold_special(c: char) -> Option<String> {
    let cp = c as u32;
    let chars = |cps: &[u32]| -> Option<String> {
        Some(cps.iter().filter_map(|&x| char::from_u32(x)).collect())
    };
    match cp {
        // Cherokee folds to the capital letters, which lowercasing would not.
        0x13A0..=0x13F5 => chars(&[cp]),
        0x13F8..=0x13FD => chars(&[cp - 8]),
        0xAB70..=0xABBF => chars(&[cp - 0xAB70 + 0x13A0]),
        // Greek with ypogegrammeni / prosgegrammeni: the base letter plus iota.
        0x1F80..=0x1FAF => {
            let base = [0x1F00, 0x1F20, 0x1F60][((cp - 0x1F80) / 16) as usize] + (cp - 0x1F80) % 8;
            chars(&[base, 0x3B9])
        }
        _ => {
            let folded: &[u32] = match cp {
                0x00B5 => &[0x3BC],
                0x00DF => &[0x73, 0x73],
                0x0149 => &[0x2BC, 0x6E],
                0x017F => &[0x73],
                0x01F0 => &[0x6A, 0x30C],
                0x0345 => &[0x3B9],
                0x0390 => &[0x3B9, 0x308, 0x301],
                0x03B0 => &[0x3C5, 0x308, 0x301],
                0x03C2 => &[0x3C3],
                0x03D0 => &[0x3B2],
                0x03D1 => &[0x3B8],
                0x03D5 => &[0x3C6],
                0x03D6 => &[0x3C0],
                0x03F0 => &[0x3BA],
                0x03F1 => &[0x3C1],
                0x03F5 => &[0x3B5],
                0x0587 => &[0x565, 0x582],
                0x1C80 => &[0x432],
                0x1C81 => &[0x434],
                0x1C82 => &[0x43E],
                0x1C83 => &[0x441],
                0x1C84 | 0x1C85 => &[0x442],
                0x1C86 => &[0x44A],
                0x1C87 => &[0x463],
                0x1C88 => &[0xA64B],
                0x1E96 => &[0x68, 0x331],
                0x1E97 => &[0x74, 0x308],
                0x1E98 => &[0x77, 0x30A],
                0x1E99 => &[0x79, 0x30A],
                0x1E9A => &[0x61, 0x2BE],
                0x1E9B => &[0x1E61],
                0x1E9E => &[0x73, 0x73],
                0x1F50 => &[0x3C5, 0x313],
                0x1F52 => &[0x3C5, 0x313, 0x300],
                0x1F54 => &[0x3C5, 0x313, 0x301],
                0x1F56 => &[0x3C5, 0x313, 0x342],
                0x1FB2 => &[0x1F70, 0x3B9],
                0x1FB3 | 0x1FBC => &[0x3B1, 0x3B9],
                0x1FB4 => &[0x3AC, 0x3B9],
                0x1FB6 => &[0x3B1, 0x342],
                0x1FB7 => &[0x3B1, 0x342, 0x3B9],
                0x1FBE => &[0x3B9],
                0x1FC2 => &[0x1F74, 0x3B9],
                0x1FC3 | 0x1FCC => &[0x3B7, 0x3B9],
                0x1FC4 => &[0x3AE, 0x3B9],
                0x1FC6 => &[0x3B7, 0x342],
                0x1FC7 => &[0x3B7, 0x342, 0x3B9],
                0x1FD2 => &[0x3B9, 0x308, 0x300],
                0x1FD3 => &[0x3B9, 0x308, 0x301],
                0x1FD6 => &[0x3B9, 0x342],
                0x1FD7 => &[0x3B9, 0x308, 0x342],
                0x1FE2 => &[0x3C5, 0x308, 0x300],
                0x1FE3 => &[0x3C5, 0x308, 0x301],
                0x1FE4 => &[0x3C1, 0x313],
                0x1FE6 => &[0x3C5, 0x342],
                0x1FE7 => &[0x3C5, 0x308, 0x342],
                0x1FF2 => &[0x1F7C, 0x3B9],
                0x1FF3 | 0x1FFC => &[0x3C9, 0x3B9],
                0x1FF4 => &[0x3CE, 0x3B9],
                0x1FF6 => &[0x3C9, 0x342],
                0x1FF7 => &[0x3C9, 0x342, 0x3B9],
                0xFB00 => &[0x66, 0x66],
                0xFB01 => &[0x66, 0x69],
                0xFB02 => &[0x66, 0x6C],
                0xFB03 => &[0x66, 0x66, 0x69],
                0xFB04 => &[0x66, 0x66, 0x6C],
                0xFB05 | 0xFB06 => &[0x73, 0x74],
                0xFB13 => &[0x574, 0x576],
                0xFB14 => &[0x574, 0x565],
                0xFB15 => &[0x574, 0x56B],
                0xFB16 => &[0x57E, 0x576],
                0xFB17 => &[0x574, 0x56D],
                _ => return None,
            };
            chars(folded)
        }
    }
}

/// `str.isprintable()` for one character.
fn is_printable(c: char) -> bool {
    use GeneralCategory::*;
    if c == ' ' {
        return true;
    }
    !matches!(
        get_general_category(c),
        Control
            | Format
            | Surrogate
            | PrivateUse
            | Unassigned
            | LineSeparator
            | ParagraphSeparator
            | SpaceSeparator
    )
}

/// `repr(text)` for a str.
pub(crate) fn repr_str(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x7F && (c as u32) >= 0x20 => out.push(c),
            c if (c as u32) < 0x7F || c == '\u{7f}' => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c if is_printable(c) => out.push(c),
            c if (c as u32) < 0x100 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) < 0x10000 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push(quote);
    out
}

/// `repr(x)` for a float.
fn repr_float(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    // Shortest round-trip digits, then Python's layout: positional for 1e-4 <= |x| < 1e16, else exponent.
    let sci = format!("{x:e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if (-4..16).contains(&exp) {
        let plain = format!("{x}");
        if plain.contains('.') {
            plain
        } else {
            format!("{plain}.0")
        }
    } else {
        format!(
            "{mantissa}e{}{:02}",
            if exp < 0 { '-' } else { '+' },
            exp.abs()
        )
    }
}

/// `repr(value)` for the Python object json.loads would have made of this value.
pub(crate) fn repr(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => i.to_string(),
            (_, Some(u), _) => u.to_string(),
            (_, _, Some(f)) => repr_float(f),
            _ => n.to_string(),
        },
        Value::String(s) => repr_str(s),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(repr).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("{}: {}", repr_str(k), repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// `str(value)`: a string is itself, anything else its repr.
pub(crate) fn to_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => repr(other),
    }
}

/// Python truthiness of a JSON value.
pub(crate) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `str(value or "")` for an optional JSON value.
pub(crate) fn str_or_empty(value: Option<&Value>) -> String {
    match value {
        Some(v) if truthy(v) => to_str(v),
        _ => String::new(),
    }
}

/// `repr(tuple_of_str)`.
pub(crate) fn repr_tuple(items: &[&str]) -> String {
    let inner = items
        .iter()
        .map(|s| repr_str(s))
        .collect::<Vec<_>>()
        .join(", ");
    if items.len() == 1 {
        format!("({inner},)")
    } else {
        format!("({inner})")
    }
}
