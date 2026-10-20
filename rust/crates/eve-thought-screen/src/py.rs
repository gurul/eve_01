//! The few Python behaviors the refusal reasons depend on, reproduced exactly: `str.split()` whitespace,
//! `repr()` of a string and of a tuple of strings, `format(x, ".Nf")`, and negative-index slicing.

/// `str.isspace()` for one character: Rust's `White_Space` plus the four ASCII information separators
/// (U+001C..U+001F), which Python also splits on.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `" ".join(text.split())`: runs of whitespace become one space, none at either end.
pub fn squash(text: &str) -> String {
    text.split(is_space).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// `bool(text.strip())`.
pub fn has_content(text: &str) -> bool {
    text.chars().any(|c| !is_space(c))
}

/// `str.lower()`, from CPython's own case table (`unicode_tables.rs`), so a letter Rust's newer Unicode data
/// would lowercase stays as the daemon's Python leaves it. U+03A3 takes the final-sigma rule from context, as
/// both languages do; Rust's `to_lowercase` decides that one.
pub fn lower(text: &str) -> String {
    if text.is_ascii() {
        return text.to_ascii_lowercase();
    }
    let table = crate::unicode_tables::LOWER;
    let mut whole: Option<String> = None;
    let mut out = String::with_capacity(text.len());
    for (i, c) in text.char_indices() {
        if c == '\u{3a3}' {
            // Every other character lowers the same alone as in context, so the sigma's form in the whole
            // string sits right after the lowered prefix.
            let all = whole.get_or_insert_with(|| text.to_lowercase());
            let at = text[..i].to_lowercase().len();
            out.push(all[at..].chars().next().unwrap_or('\u{3c3}'));
            continue;
        }
        match table.binary_search_by_key(&(c as u32), |&(k, _)| k) {
            Ok(j) => out.push_str(table[j].1),
            Err(_) => out.push(c),
        }
    }
    out
}

/// `str.isprintable()` for one character, read from CPython's own table (`unicode_tables.rs`, generated from
/// the Python the daemon runs): controls, format characters, separators other than the ASCII space, surrogates,
/// private use and unassigned code points are not printable.
fn is_printable(c: char) -> bool {
    let u = c as u32;
    let i = crate::unicode_tables::NOT_PRINTABLE.partition_point(|&(_, hi)| hi < u);
    !matches!(crate::unicode_tables::NOT_PRINTABLE.get(i), Some(&(lo, _)) if lo <= u)
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

/// `repr(tuple_of_three_str)`.
pub fn repr_triple(t: &(String, String, String)) -> String {
    format!("({}, {}, {})", repr_str(&t.0), repr_str(&t.1), repr_str(&t.2))
}

/// `format(x, f".{precision}f")`: Rust's fixed formatting rounds the exact binary value half to even, as
/// Python does; only the names of the non-finite values differ.
pub fn fixed(x: f64, precision: usize) -> String {
    if x.is_nan() {
        "nan".to_string()
    } else if x.is_infinite() {
        if x > 0.0 { "inf".to_string() } else { "-inf".to_string() }
    } else {
        format!("{x:.precision$}")
    }
}

/// The start index of Python's `seq[-n:]` over a sequence of `len` items (`n` may be zero or negative:
/// `seq[-0:]` is the whole sequence).
pub fn tail_start(len: usize, n: i64) -> usize {
    slice_index(len, n.saturating_neg())
}

/// A Python slice bound resolved against `len`: negative counts from the end, both clamp to `0..=len`.
pub fn slice_index(len: usize, i: i64) -> usize {
    let len_i = len as i64;
    if i < 0 {
        (i + len_i).max(0) as usize
    } else {
        i.min(len_i) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_matches_python_rounding() {
        // Each expected value is what CPython 3.12 prints for f"{v:.{p}f}".
        let cases: [(f64, usize, &str); 12] = [
            (0.5, 0, "0"),
            (1.5, 0, "2"),
            (2.5, 0, "2"),
            (-0.4, 0, "-0"),
            (0.125, 2, "0.12"),
            (0.675, 2, "0.68"),
            (0.705, 2, "0.70"),
            (f64::NAN, 2, "nan"),
            (f64::INFINITY, 0, "inf"),
            (f64::NEG_INFINITY, 2, "-inf"),
            (239.5, 0, "240"),
            (-0.0, 2, "-0.00"),
        ];
        for (v, p, want) in cases {
            assert_eq!(fixed(v, p), want, "{v} .{p}f");
        }
    }

    #[test]
    fn repr_matches_python() {
        assert_eq!(repr_str("vent"), "'vent'");
        assert_eq!(repr_str("don't"), "\"don't\"");
        assert_eq!(repr_str("a'b\"c"), "'a\\'b\"c'");
        assert_eq!(repr_str("x\u{200b}y\u{0}"), "'x\\u200by\\x00'");
        assert_eq!(repr_str("café ☕"), "'café ☕'");
        let t = ("don't".to_string(), "a-b".to_string(), "x".to_string());
        assert_eq!(repr_triple(&t), "(\"don't\", 'a-b', 'x')");
    }

    #[test]
    fn repr_escapes_what_python_3_13_has_not_assigned() {
        // Unassigned in CPython 3.13's Unicode 15.1, so repr escapes them; Rust's own data (16.0) knows U+1C89.
        assert_eq!(repr_str("a\u{378}b\u{10d50}"), "'a\\u0378b\\U00010d50'");
        assert_eq!(repr_str("\u{1c89}"), "'\\u1c89'");
    }

    #[test]
    fn lower_matches_python() {
        // Each expected value is what CPython 3.13 returns for text.lower().
        let cases = [
            ("ΟΔΟΣ", "οδος"),
            ("Σ", "σ"),
            ("ΣΑ", "σα"),
            ("ΑΣ ΒΣ", "ας βς"),
            ("ΑΣ.", "ας."),
            ("İΣ", "i\u{307}ς"),
            ("ΑΣ'Β", "ασ'β"),
            ("ΑΣ'", "ας'"),
            ("x Σ y", "x σ y"),
            ("ΑΣΣ", "ασς"),
            ("Vent CEILING", "vent ceiling"),
            ("\u{212a}elvin", "kelvin"),
            // Garay capital letters arrived in Unicode 16.0; Python 3.13 leaves them as they are.
            ("\u{10d50}\u{1c89}", "\u{10d50}\u{1c89}"),
        ];
        for (text, want) in cases {
            assert_eq!(lower(text), want, "{text:?}");
        }
    }

    #[test]
    fn squash_splits_like_python() {
        assert_eq!(squash("  a\t b\n\u{1c}c\u{a0}d  "), "a b c d");
        assert_eq!(squash("   "), "");
        assert!(!has_content(" \u{1f}\u{3000}"));
        assert!(has_content(" x "));
    }

    #[test]
    fn slicing_matches_python() {
        // [0,1,2,3,4][-2:] -> start 3; [-0:] -> 0; [-9:] -> 0; [--2:] i.e. [2:] -> 2
        assert_eq!(tail_start(5, 2), 3);
        assert_eq!(tail_start(5, 0), 0);
        assert_eq!(tail_start(5, 9), 0);
        assert_eq!(tail_start(5, -2), 2);
        assert_eq!(tail_start(5, -9), 5);
        assert_eq!(slice_index(5, -3), 2);
        assert_eq!(slice_index(5, 0), 0);
    }
}
