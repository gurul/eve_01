//! The few pieces of Python's `str` that jev_verify's text handling leans on: `casefold`, the character
//! classes `re` uses for `\w` and `\d` in a `str` pattern, `isdigit`, `splitlines` and `strip`. The tables
//! (`pychars.rs`) are generated from CPython 3.13, the daemon's interpreter, so a word Jev's shortlist or the
//! code's vetoes count is the word Python counts.

use crate::pychars::{ALPHA, DECIMAL, DIGIT, FOLDS, LOWER, NUMERIC};

fn in_ranges(table: &[(u32, u32)], c: char) -> bool {
    let cp = c as u32;
    table
        .binary_search_by(|&(lo, hi)| {
            if hi < cp {
                std::cmp::Ordering::Less
            } else if lo > cp {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// `c.isalpha()`.
pub fn is_alpha(c: char) -> bool {
    in_ranges(&ALPHA, c)
}

/// `c.isdecimal()`: what `\d` matches in a `str` pattern.
pub fn is_decimal(c: char) -> bool {
    in_ranges(&DECIMAL, c)
}

/// `c.isdigit()`.
pub fn is_digit(c: char) -> bool {
    in_ranges(&DIGIT, c)
}

/// `c.isnumeric()`.
pub fn is_numeric(c: char) -> bool {
    in_ranges(&NUMERIC, c)
}

/// What `\w` matches in a `str` pattern: `isalnum()` (alpha, decimal, digit, numeric) or `_`.
pub fn is_word(c: char) -> bool {
    c == '_' || is_alpha(c) || is_decimal(c) || is_digit(c) || is_numeric(c)
}

/// `[^\W\d_]`: a word character that is neither a decimal digit nor `_`.
pub fn is_letter(c: char) -> bool {
    is_word(c) && c != '_' && !is_decimal(c)
}

/// `str.casefold()`: each character's full case folding, with no context (no final sigma).
pub fn casefold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if let Ok(i) = FOLDS.binary_search_by_key(&c, |&(k, _)| k) {
            out.push_str(FOLDS[i].1);
        } else if let Ok(i) = LOWER.binary_search_by_key(&c, |&(k, _)| k) {
            out.push_str(LOWER[i].1);
        } else {
            out.push(c);
        }
    }
    out
}

/// `c.isspace()`.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `s.strip()`.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `s.splitlines()`: the line boundaries Python's `str.splitlines` knows (`\r\n` is one).
pub fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if matches!(
            c,
            '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        ) {
            out.push(&s[start..i]);
            let mut end = i + c.len_utf8();
            if c == '\r' {
                if let Some(&(j, '\n')) = chars.peek() {
                    chars.next();
                    end = j + 1;
                }
            }
            start = end;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// `s[:n]` on characters.
pub fn take(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `len(s)` in characters.
pub fn len(s: &str) -> usize {
    s.chars().count()
}

/// `round(x)` to an integer: half to even, as Python 3's `round`.
pub fn round_int(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// `round(x, n)` for a small `n`: the correctly rounded decimal, as Python formats it.
pub fn round_to(x: f64, n: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.n$}").parse().unwrap_or(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_match_cpython() {
        assert!(is_alpha('é') && !is_alpha('1') && !is_alpha('_'));
        assert!(is_decimal('٣') && !is_decimal('²') && is_digit('²') && is_numeric('½') && !is_digit('½'));
        assert!(is_letter('²') && is_letter('½') && !is_letter('3') && !is_letter('_') && !is_letter('-'));
        assert!(!is_word('\u{301}'));
    }

    #[test]
    fn casefold_is_pythons() {
        assert_eq!(casefold("Straße ΣΑΣ"), "strasse σασ");
        assert_eq!(casefold("İ"), "i\u{307}");
        assert_eq!(casefold("1-DAY"), "1-day");
    }

    #[test]
    fn splitlines_is_pythons() {
        assert_eq!(splitlines("a\r\nb\rc\n\nd\u{2028}e\n"), vec!["a", "b", "c", "", "d", "e"]);
        assert_eq!(splitlines(""), Vec::<&str>::new());
    }

    #[test]
    fn rounding_is_pythons() {
        assert_eq!(round_int(2.5), 2);
        assert_eq!(round_int(3.5), 4);
        assert_eq!(round_to(0.96, 3), 0.96);
        assert_eq!(round_to(0.0005, 3), 0.001);
        assert_eq!(round_to(1.23456789e-5, 6), 1.2e-5);
    }
}
