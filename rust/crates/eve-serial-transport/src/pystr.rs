//! The two bits of Python `str` behavior the transport leans on: `str.strip()`
//! and `repr(str)` (for the `bad json` debug line).

/// `str.isspace()` for one character: Unicode White_Space plus the four
/// information separators U+001C..U+001F, which CPython also treats as space.
pub(crate) fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()` with no argument.
pub(crate) fn py_strip(s: &str) -> &str {
    s.trim_matches(is_py_space)
}

/// `s[:n]` on a `str`: the first `n` code points.
pub(crate) fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// `str.isprintable()` for one character, without CPython's full Unicode
/// tables: control characters (Cc), line/paragraph separators (Zl, Zp), every
/// space separator but U+0020 (Zs), surrogates and the common format
/// characters (Cf) are not printable.
fn is_py_printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    if c.is_control() || c.is_whitespace() {
        return false;
    }
    !matches!(
        c,
        '\u{ad}'
            | '\u{600}'..='\u{605}'
            | '\u{61c}'
            | '\u{6dd}'
            | '\u{70f}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{e000}'..='\u{f8ff}'
            | '\u{f0000}'..='\u{ffffd}'
            | '\u{100000}'..='\u{10fffd}'
            | '\u{e0001}'
            | '\u{e0020}'..='\u{e007f}'
    )
}

/// `repr(s)` for a Python `str`.
pub(crate) fn py_repr(s: &str) -> String {
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
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c if (c as u32) < 0x7f => out.push(c),
            c if is_py_printable(c) => out.push(c),
            c if (c as u32) <= 0xff => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) <= 0xffff => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push(quote);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_matches_python_whitespace() {
        assert_eq!(py_strip(" \t\x1c{\"a\":1}\x1f\r\n\u{a0}"), "{\"a\":1}");
        assert_eq!(py_strip("\u{200b}x"), "\u{200b}x"); // ZWSP is not space in Python
    }

    #[test]
    fn take_chars_counts_code_points() {
        assert_eq!(take_chars("héllo", 2), "hé");
        assert_eq!(take_chars("ab", 5), "ab");
    }

    #[test]
    fn repr_matches_python() {
        assert_eq!(py_repr("abc"), "'abc'");
        assert_eq!(py_repr("it's"), "\"it's\"");
        assert_eq!(py_repr("'\""), "'\\'\"'");
        assert_eq!(py_repr("a\\b\n\t\r\x00\x7f"), "'a\\\\b\\n\\t\\r\\x00\\x7f'");
        assert_eq!(py_repr("é\u{a0}\u{fffd}\u{200b}"), "'é\\xa0\u{fffd}\\u200b'");
        assert_eq!(py_repr("{\"a\":"), "'{\"a\":'");
    }
}
