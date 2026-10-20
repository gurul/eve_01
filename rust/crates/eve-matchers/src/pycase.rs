//! Python `re`'s IGNORECASE matching, spelled out as explicit sets.
//!
//! Python does not fold case the way Rust's `(?i)` does. `re` lowers both
//! sides with `_sre.unicode_tolower` (the first character of the full
//! lowercase mapping, so `İ` lowers to `i`) and adds `_casefix._EXTRA_CASES`
//! (`i`/`ı`, `s`/`ſ`, `σ`/`ς`...). Rust folds by Unicode simple case folding,
//! which keeps `İ` and `ı` apart from `i`. So `(?i)i` matches `İ` and `ı` in
//! Python and not in Rust.
//!
//! The translator therefore never hands a case-insensitive literal or class
//! to Rust's `i` flag: it asks this module for the exact set of code points
//! Python's compiled program accepts (`_compiler._compile` LITERAL and
//! `_optimize_charset` + `IN_UNI_IGNORE` / `IN_IGNORE`) and emits that.

use std::sync::LazyLock;

/// `re._casefix._EXTRA_CASES` (CPython 3.13): maps the code of a lowercased
/// character to the codes of different lowercased characters with the same
/// uppercase.
const EXTRA_CASES: &[(u32, &[u32])] = &[
    (0x0069, &[0x0131]),
    (0x0073, &[0x017F]),
    (0x00B5, &[0x03BC]),
    (0x0131, &[0x0069]),
    (0x017F, &[0x0073]),
    (0x0345, &[0x03B9, 0x1FBE]),
    (0x0390, &[0x1FD3]),
    (0x03B0, &[0x1FE3]),
    (0x03B2, &[0x03D0]),
    (0x03B5, &[0x03F5]),
    (0x03B8, &[0x03D1]),
    (0x03B9, &[0x0345, 0x1FBE]),
    (0x03BA, &[0x03F0]),
    (0x03BC, &[0x00B5]),
    (0x03C0, &[0x03D6]),
    (0x03C1, &[0x03F1]),
    (0x03C2, &[0x03C3]),
    (0x03C3, &[0x03C2]),
    (0x03C6, &[0x03D5]),
    (0x03D0, &[0x03B2]),
    (0x03D1, &[0x03B8]),
    (0x03D5, &[0x03C6]),
    (0x03D6, &[0x03C0]),
    (0x03F0, &[0x03BA]),
    (0x03F1, &[0x03C1]),
    (0x03F5, &[0x03B5]),
    (0x0432, &[0x1C80]),
    (0x0434, &[0x1C81]),
    (0x043E, &[0x1C82]),
    (0x0441, &[0x1C83]),
    (0x0442, &[0x1C84, 0x1C85]),
    (0x044A, &[0x1C86]),
    (0x0463, &[0x1C87]),
    (0x1C80, &[0x0432]),
    (0x1C81, &[0x0434]),
    (0x1C82, &[0x043E]),
    (0x1C83, &[0x0441]),
    (0x1C84, &[0x0442, 0x1C85]),
    (0x1C85, &[0x0442, 0x1C84]),
    (0x1C86, &[0x044A]),
    (0x1C87, &[0x0463]),
    (0x1C88, &[0xA64B]),
    (0x1E61, &[0x1E9B]),
    (0x1E9B, &[0x1E61]),
    (0x1FBE, &[0x0345, 0x03B9]),
    (0x1FD3, &[0x0390]),
    (0x1FE3, &[0x03B0]),
    (0xA64B, &[0x1C88]),
    (0xFB05, &[0xFB06]),
    (0xFB06, &[0xFB05]),
];

/// Which case table is in force: `str` patterns default to Unicode;
/// `(?a)` switches to ASCII (`_sre.ascii_tolower`, no extra cases).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Mode {
    Unicode,
    Ascii,
}

/// `_sre.unicode_tolower` / `_sre.ascii_tolower`.
pub(crate) fn lower(mode: Mode, cp: u32) -> u32 {
    match mode {
        Mode::Ascii => {
            if (0x41..=0x5A).contains(&cp) {
                cp + 32
            } else {
                cp
            }
        }
        Mode::Unicode => char::from_u32(cp)
            .and_then(|c| c.to_lowercase().next())
            .map_or(cp, |c| c as u32),
    }
}

/// `_sre.unicode_toupper` (the first character of the full uppercase
/// mapping, as CPython's `_PyUnicode_ToUppercase` returns it).
fn upper(cp: u32) -> u32 {
    char::from_u32(cp)
        .and_then(|c| c.to_uppercase().next())
        .map_or(cp, |c| c as u32)
}

/// `_sre.unicode_iscased` / `_sre.ascii_iscased`.
pub(crate) fn iscased(mode: Mode, cp: u32) -> bool {
    match mode {
        Mode::Ascii => cp < 128 && (cp as u8).is_ascii_alphabetic(),
        Mode::Unicode => lower(Mode::Unicode, cp) != cp || upper(cp) != cp,
    }
}

fn fixes(mode: Mode, cp: u32) -> &'static [u32] {
    if mode == Mode::Ascii {
        return &[];
    }
    EXTRA_CASES
        .binary_search_by_key(&cp, |&(k, _)| k)
        .map_or(&[], |i| EXTRA_CASES[i].1)
}

/// Every cased code point, ascending.
fn cased(mode: Mode) -> &'static [u32] {
    static UNICODE: LazyLock<Vec<u32>> = LazyLock::new(|| {
        (0..=0x10FFFFu32)
            .filter(|&cp| iscased(Mode::Unicode, cp))
            .collect()
    });
    static ASCII: LazyLock<Vec<u32>> =
        LazyLock::new(|| (0x41..=0x5Au32).chain(0x61..=0x7A).collect());
    match mode {
        Mode::Unicode => &UNICODE,
        Mode::Ascii => &ASCII,
    }
}

/// One member of a class as sre_parse hands it to the compiler.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Item {
    Lit(u32),
    Range(u32, u32),
}

/// What `_optimize_charset` builds under IGNORECASE: a BMP map of lowered
/// members (`charmap`, with the extra cases), lowered literals outside the
/// BMP, and ranges that reach past it (`RANGE_UNI_IGNORE` under Unicode,
/// plain `RANGE` under ASCII). A character matches when its lowercase is in
/// the set (`IN_UNI_IGNORE` / `IN_IGNORE`).
struct Lowered {
    mode: Mode,
    bmp: Vec<u64>,
    wide: Vec<u32>,
    ranges: Vec<(u32, u32)>,
}

impl Lowered {
    fn new(mode: Mode) -> Self {
        Lowered {
            mode,
            bmp: vec![0; 0x10000 / 64],
            wide: Vec::new(),
            ranges: Vec::new(),
        }
    }

    fn set(&mut self, cp: u32) {
        self.bmp[(cp / 64) as usize] |= 1 << (cp % 64);
    }

    fn has(&self, cp: u32) -> bool {
        cp < 0x10000 && self.bmp[(cp / 64) as usize] & (1 << (cp % 64)) != 0
    }

    /// `charmap[av] = 1` plus the extra cases of `av`.
    fn add_lowered(&mut self, l: u32) {
        self.set(l);
        for &k in fixes(self.mode, l) {
            self.set(k);
        }
    }

    /// Whether `_sre`'s charset test accepts `cp` once lowered.
    fn accepts(&self, cp: u32) -> bool {
        let l = lower(self.mode, cp);
        self.has(l)
            || self.wide.contains(&l)
            || self.ranges.iter().any(|&(lo, hi)| {
                (lo..=hi).contains(&l)
                    || (self.mode == Mode::Unicode && (lo..=hi).contains(&upper(l)))
            })
    }

    /// Every code point accepted, as sorted, merged ranges. An uncased
    /// character lowers to itself, so it is in exactly when it is in one of
    /// the parts; a cased one is tested on its lowercase.
    fn members(&self) -> Vec<(u32, u32)> {
        let mut parts: Vec<(u32, u32)> = Vec::new();
        let mut cp = 0u32;
        while cp < 0x10000 {
            if self.has(cp) {
                let start = cp;
                while cp < 0x10000 && self.has(cp) {
                    cp += 1;
                }
                parts.push((start, cp - 1));
            } else {
                cp += 1;
            }
        }
        parts.extend(self.wide.iter().map(|&w| (w, w)));
        parts.extend(self.ranges.iter().copied());
        let parts = merge(parts);

        let cased = cased(self.mode);
        let mut out = Vec::new();
        // The uncased characters of the parts.
        for &(lo, hi) in &parts {
            let mut start = lo;
            let first = cased.partition_point(|&c| c < lo);
            for &c in cased[first..].iter().take_while(|&&c| c <= hi) {
                if c > start {
                    out.push((start, c - 1));
                }
                start = c + 1;
            }
            if start <= hi {
                out.push((start, hi));
            }
        }
        // The cased characters whose lowercase is in.
        out.extend(
            cased
                .iter()
                .filter(|&&c| self.accepts(c))
                .map(|&c| (c, c)),
        );
        merge(out)
    }
}

fn merge(mut v: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    v.sort_unstable();
    let mut out: Vec<(u32, u32)> = Vec::with_capacity(v.len());
    for (lo, hi) in v {
        match out.last_mut() {
            Some(last) if lo <= last.1.saturating_add(1) => last.1 = last.1.max(hi),
            _ => out.push((lo, hi)),
        }
    }
    out
}

/// A literal under IGNORECASE (`_compile`'s LITERAL branch): `None` when the
/// character is not cased (Python matches it exactly), else every code point
/// whose lowercase is the literal's lowercase or one of its extra cases.
pub(crate) fn literal(mode: Mode, cp: u32) -> Option<Vec<(u32, u32)>> {
    if !iscased(mode, cp) {
        return None;
    }
    let l = lower(mode, cp);
    let mut set = Lowered::new(mode);
    if l < 0x10000 {
        set.add_lowered(l);
    } else {
        set.wide.push(l);
    }
    Some(set.members())
}

/// A `[...]` class's literals and ranges under IGNORECASE
/// (`_optimize_charset` with `fixup`): `None` when no member is cased
/// (Python emits a plain, case-sensitive `IN`), else the code points the
/// case-insensitive set accepts, before any negation. Category members
/// (`\w`, `\d`, `\s` and their negations) are not passed in: each holds for
/// a character exactly when it holds for its lowercase, so they are emitted
/// as they are.
pub(crate) fn class(mode: Mode, items: &[Item]) -> Option<Vec<(u32, u32)>> {
    let mut set = Lowered::new(mode);
    let mut hascased = false;
    for &item in items {
        match item {
            Item::Lit(cp) => {
                let l = lower(mode, cp);
                if l < 0x10000 {
                    set.add_lowered(l);
                } else {
                    // The charmap IndexError path: the lowered literal goes
                    // to the tail, without extra cases.
                    set.wide.push(l);
                }
                if iscased(mode, l) {
                    hascased = true;
                }
            }
            Item::Range(lo, hi) => {
                // BMP members go to the charmap in order; the first one
                // whose lowercase falls outside it sends the whole range to
                // the tail as a range test.
                let mut overflowed = false;
                for i in lo..=hi {
                    let l = lower(mode, i);
                    if l >= 0x10000 {
                        overflowed = true;
                        break;
                    }
                    set.add_lowered(l);
                }
                if overflowed {
                    set.ranges.push((lo, hi));
                    hascased = true;
                } else if !hascased {
                    hascased = (lo..=hi).any(|i| iscased(mode, i));
                }
            }
        }
    }
    hascased.then(|| set.members())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has(set: &[(u32, u32)], c: char) -> bool {
        set.iter().any(|&(lo, hi)| (lo..=hi).contains(&(c as u32)))
    }

    #[test]
    fn dotted_and_dotless_i_join_i() {
        for c in ['i', 'I', 'İ', 'ı'] {
            let set = literal(Mode::Unicode, c as u32).unwrap();
            for x in ['i', 'I', 'İ', 'ı'] {
                assert!(has(&set, x), "{c} {x}");
            }
            assert!(!has(&set, 'j'));
        }
        let ascii = literal(Mode::Ascii, 'i' as u32).unwrap();
        assert_eq!(ascii, vec![(0x49, 0x49), (0x69, 0x69)]);
    }

    #[test]
    fn uncased_literal_is_exact() {
        assert!(literal(Mode::Unicode, '1' as u32).is_none());
        assert!(literal(Mode::Ascii, 'é' as u32).is_none());
        assert!(class(Mode::Unicode, &[Item::Range(0x30, 0x39)]).is_none());
    }

    #[test]
    fn sharp_s_and_kelvin_follow_lowercase() {
        let ss = literal(Mode::Unicode, 'ß' as u32).unwrap();
        assert!(has(&ss, 'ẞ') && !has(&ss, 's'));
        let k = class(Mode::Unicode, &[Item::Range('a' as u32, 'k' as u32)]).unwrap();
        assert!(has(&k, '\u{212A}') && has(&k, 'İ') && has(&k, 'ı') && !has(&k, 'ſ'));
    }
}
