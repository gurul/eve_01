//! Python's `html.unescape` (the same port eve-google-rest and eve-nyt keep private): numeric references (with the
//! HTML5 replacements for the C1 range and the invalid code points) and named references, including the legacy names that need no semicolon and the
//! longest-prefix rule for an unknown name.

use std::sync::LazyLock;

use regex::{Captures, Regex};

use crate::html5_entities::HTML5;

static CHARREF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"&(#[0-9]+;?|#[xX][0-9a-fA-F]+;?|[^\t\n\x0c <&#;]{1,32};?)").expect("valid regex"));

fn invalid_charref(n: u32) -> Option<&'static str> {
    Some(match n {
        0x00 => "\u{fffd}",
        0x0d => "\r",
        0x80 => "\u{20ac}",
        0x81 => "\u{81}",
        0x82 => "\u{201a}",
        0x83 => "\u{0192}",
        0x84 => "\u{201e}",
        0x85 => "\u{2026}",
        0x86 => "\u{2020}",
        0x87 => "\u{2021}",
        0x88 => "\u{02c6}",
        0x89 => "\u{2030}",
        0x8a => "\u{0160}",
        0x8b => "\u{2039}",
        0x8c => "\u{0152}",
        0x8d => "\u{8d}",
        0x8e => "\u{017d}",
        0x8f => "\u{8f}",
        0x90 => "\u{90}",
        0x91 => "\u{2018}",
        0x92 => "\u{2019}",
        0x93 => "\u{201c}",
        0x94 => "\u{201d}",
        0x95 => "\u{2022}",
        0x96 => "\u{2013}",
        0x97 => "\u{2014}",
        0x98 => "\u{02dc}",
        0x99 => "\u{2122}",
        0x9a => "\u{0161}",
        0x9b => "\u{203a}",
        0x9c => "\u{0153}",
        0x9d => "\u{9d}",
        0x9e => "\u{017e}",
        0x9f => "\u{0178}",
        _ => return None,
    })
}

fn invalid_codepoint(n: u32) -> bool {
    matches!(n, 0x1..=0x8 | 0xb | 0xe..=0x1f | 0x7f..=0x9f | 0xfdd0..=0xfdef) || (n & 0xfffe) == 0xfffe
}

fn entity(name: &str) -> Option<&'static str> {
    HTML5.binary_search_by(|(k, _)| (*k).cmp(name)).ok().map(|i| HTML5[i].1)
}

fn replace(caps: &Captures) -> String {
    let s = &caps[1];
    if let Some(num) = s.strip_prefix('#') {
        let digits = num.trim_end_matches(';');
        let n = match digits.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16),
            None => digits.parse::<u32>(),
        }
        .unwrap_or(u32::MAX);
        if let Some(r) = invalid_charref(n) {
            return r.to_string();
        }
        if (0xd800..=0xdfff).contains(&n) || n > 0x10ffff {
            return "\u{fffd}".to_string();
        }
        if invalid_codepoint(n) {
            return String::new();
        }
        return char::from_u32(n).map(String::from).unwrap_or_else(|| "\u{fffd}".into());
    }
    if let Some(v) = entity(s) {
        return v.to_string();
    }
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    for x in (2..chars.len()).rev() {
        let cut = chars[x].0;
        if let Some(v) = entity(&s[..cut]) {
            return format!("{v}{}", &s[cut..]);
        }
    }
    format!("&{s}")
}

/// `html.unescape(s)`.
pub fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    CHARREF.replace_all(s, |c: &Captures| replace(c)).into_owned()
}
