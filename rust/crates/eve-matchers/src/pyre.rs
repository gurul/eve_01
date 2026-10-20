//! Python `re` syntax and semantics on top of `fancy-regex`.
//!
//! `matchers.toml` patterns are written for Python's `re` (the Python daemon
//! compiles them with `re.compile`), and the same file must mean the same
//! thing to both daemons. The Rust regex dialect differs in ways that change
//! answers, not just syntax:
//!
//! * `$` in Python (no MULTILINE) also matches just before a final `\n`;
//!   Rust's `$` matches only at the very end.
//! * `\Z` in Python is the absolute end; in fancy-regex it skips trailing
//!   newlines.
//! * `\w`, `\b`, `\s` follow Python's `str` predicates: `\w` is exactly
//!   letters, numbers and `_` (`[\p{L}\p{N}_]`), `\s` is `str.isspace`, which
//!   also holds for U+001C..U+001F. `\B` never matches an empty string.
//! * A `{` or `}` that is not a well-formed quantifier is a literal in
//!   Python and an error in Rust; `{,n}` means `{0,n}`.
//! * Escapes Python rejects (`\z`, `\e`, `\h`, `\k`, `\<`...) must stay bad
//!   regexes, and Python's octal escapes and `\b` (backspace) in a class must
//!   keep their meaning.
//! * `[` inside a class is a literal in Python and a nested class in Rust;
//!   `&&`, `--`, `~~` are set operators in Rust only.
//! * Inline global flags must lead the pattern in Python 3.11+; `(?x)`
//!   verbose mode and `(?#...)` comments follow Python's rules.
//! * Python refuses a variable-width look-behind, a repeat of nothing or of
//!   an anchor, a repeat of a repeat, and a conditional on a missing group;
//!   fancy-regex accepts some of those.
//!
//! `translate` rewrites a Python pattern into an equivalent fancy-regex
//! pattern, or returns the `re.error` message Python would raise.

use std::fmt::Write as _;

/// The character-class bodies for one mode (Unicode `str` default, or `(?a)`).
struct Classes {
    word: &'static str,
    digit: &'static str,
    space: &'static str,
}

const UNICODE: Classes = Classes {
    word: r"\p{L}\p{N}_",
    digit: r"\p{Nd}",
    // Rust's `\s` (Unicode White_Space) plus the four information separators
    // Python's `str.isspace` also counts.
    space: r"\s\x{1C}-\x{1F}",
};

const ASCII: Classes = Classes {
    word: r"A-Za-z0-9_",
    digit: r"0-9",
    space: r" \t\n\r\x{0B}\x{0C}",
};

/// A never-matching atom.
const NEVER: &str = "(?!)";

/// `_sre.MAXREPEAT`: the "unbounded" width.
const MAXREPEAT: u64 = 4_294_967_295;

/// Why Python's `re.compile` refuses a pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReError {
    /// `re.error`, with its message.
    Error(String),
    /// `OverflowError("the repetition number is too large")`: sre_parse
    /// raises it for a repeat count of `MAXREPEAT` or more, and it is not a
    /// `re.error`, so `matchers._compile` does not catch it.
    Overflow,
}

/// `re.error`-shaped message: the reason plus `at position N`.
fn err(msg: impl AsRef<str>, pos: usize) -> ReError {
    ReError::Error(format!("{} at position {}", msg.as_ref(), pos))
}

/// A code point as a Rust escape. A lone surrogate (`\ud800`) compiles in
/// Python but no Rust `&str` can hold one, so it never matches.
fn hex_char(out: &mut String, cp: u32) {
    if char::from_u32(cp).is_some() {
        let _ = write!(out, r"\x{{{cp:X}}}");
    } else {
        out.push_str(NEVER);
    }
}

fn is_oct(c: Option<char>) -> bool {
    matches!(c, Some('0'..='7'))
}

/// Python's inline flags in force at a point of the pattern.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Flags {
    /// IGNORECASE
    i: bool,
    /// MULTILINE
    m: bool,
    /// DOTALL
    s: bool,
    /// VERBOSE
    x: bool,
    /// ASCII
    a: bool,
}

impl Flags {
    /// Whether Rust's own `i` flag should be on: Unicode case folding.
    /// Under ASCII, Python folds only ASCII letters, which is spelled out
    /// per literal instead.
    fn rust_i(self) -> bool {
        self.i && !self.a
    }

    /// Whether literals must be ASCII-folded by hand.
    fn fold(self) -> bool {
        self.i && self.a
    }
}

/// What the last item in the current branch was, for quantifier checks and
/// for the width Python's look-behind check needs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Atom {
    /// A character, class, group or look-around, with its (min, max) width.
    Item(u64, u64),
    /// An anchor (`^ $ \A \Z \b \B`): repeating it is "nothing to repeat".
    At,
    /// A look-around group, translated from this output offset on.
    Look(usize),
    /// A repeated item: repeating it again is "multiple repeat".
    Repeat(u64, u64),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Root,
    Capture(usize),
    Plain,
    LookAhead,
    LookBehind,
    Cond,
}

struct Frame {
    kind: Kind,
    /// Width over the finished alternatives, if any.
    alts: Option<(u64, u64)>,
    branches: usize,
    /// Width of the current branch so far, excluding `last`.
    cur: (u64, u64),
    last: Option<Atom>,
    /// Source position of the `(`, and output position of its translation.
    start: usize,
    out_start: usize,
    /// The flags to restore when a scoped `(?flags:...)` group closes.
    saved: Option<Flags>,
}

impl Frame {
    fn new(kind: Kind, start: usize, out_start: usize) -> Self {
        Frame {
            kind,
            alts: None,
            branches: 1,
            cur: (0, 0),
            last: None,
            start,
            out_start,
            saved: None,
        }
    }

    fn commit(&mut self) {
        let (lo, hi) = match self.last.take() {
            Some(Atom::Item(lo, hi) | Atom::Repeat(lo, hi)) => (lo, hi),
            Some(Atom::At | Atom::Look(_)) | None => (0, 0),
        };
        self.cur = (
            (self.cur.0 + lo).min(MAXREPEAT),
            (self.cur.1 + hi).min(MAXREPEAT),
        );
    }

    fn end_branch(&mut self) {
        self.commit();
        self.alts = Some(match self.alts {
            None => self.cur,
            Some((lo, hi)) => (lo.min(self.cur.0), hi.max(self.cur.1)),
        });
        self.cur = (0, 0);
    }
}

/// One member of a `[...]` class.
enum ClassItem {
    /// A single code point, with the source token for error messages.
    Lit(u32, String),
    /// A class body (`\w`, `\S`...), with its source token.
    Set(String, String),
}

impl ClassItem {
    fn token(&self) -> &str {
        match self {
            ClassItem::Lit(_, t) | ClassItem::Set(_, t) => t,
        }
    }

    /// Appends the member; returns how many members a Rust string can hit.
    fn emit(&self, body: &mut String, fold: bool) -> usize {
        match self {
            ClassItem::Lit(cp, _) => emit_range(body, *cp, *cp, fold),
            ClassItem::Set(set, _) => {
                body.push_str(set);
                1
            }
        }
    }
}

/// Appends `lo-hi` as escaped code points, clipped off the surrogate block
/// (Python strings may hold surrogates; Rust strings cannot). With `fold`
/// (`(?ai)`), the other case of every ASCII letter in the range joins it.
fn emit_range(body: &mut String, lo: u32, hi: u32, fold: bool) -> usize {
    if fold {
        for (a, z, shift) in [(0x61u32, 0x7Au32, -32i64), (0x41, 0x5A, 32)] {
            let (l, h) = (lo.max(a), hi.min(z));
            if l <= h {
                let (l, h) = ((l as i64 + shift) as u32, (h as i64 + shift) as u32);
                emit_range(body, l, h, false);
            }
        }
    }
    let (mut lo, mut hi) = (lo, hi);
    if (0xD800..=0xDFFF).contains(&lo) {
        lo = 0xE000;
    }
    if (0xD800..=0xDFFF).contains(&hi) {
        hi = 0xD7FF;
    }
    if lo > hi {
        return 0;
    }
    if lo == hi {
        let _ = write!(body, r"\x{{{lo:X}}}");
    } else {
        let _ = write!(body, r"\x{{{lo:X}}}-\x{{{hi:X}}}");
    }
    1
}

struct Translator {
    c: Vec<char>,
    i: usize,
    out: String,
    flags: Flags,
    /// Emit `\B` as never-matching: Python's `\B` fails on an empty string.
    empty_input: bool,
    saw_non_boundary: bool,
    frames: Vec<Frame>,
    groups: usize,
    group_widths: Vec<Option<(u64, u64)>>,
    names: Vec<(String, usize)>,
    cond_refs: Vec<(usize, usize)>,
}

impl Translator {
    fn peek(&self, off: usize) -> Option<char> {
        self.c.get(self.i + off).copied()
    }

    fn starts_with(&self, s: &str) -> bool {
        let n = s.chars().count();
        self.c
            .get(self.i..self.i + n)
            .is_some_and(|w| w.iter().copied().eq(s.chars()))
    }

    fn classes(&self) -> &'static Classes {
        if self.flags.a {
            &ASCII
        } else {
            &UNICODE
        }
    }

    fn frame(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("root frame")
    }

    fn atom(&mut self, a: Atom) {
        let f = self.frame();
        f.commit();
        f.last = Some(a);
    }

    fn item(&mut self) {
        self.atom(Atom::Item(1, 1));
    }

    /// Leading `(?aiLmsux)` groups: the only place Python 3.11+ accepts
    /// global flags.
    fn leading_flags(&mut self) -> Result<(), ReError> {
        let mut seen_u = false;
        loop {
            if !self.starts_with("(?") {
                return Ok(());
            }
            let start = self.i;
            let mut k = self.i + 2;
            let mut letters = String::new();
            while let Some(&ch) = self.c.get(k) {
                if "aiLmsux".contains(ch) {
                    letters.push(ch);
                    k += 1;
                } else {
                    break;
                }
            }
            if letters.is_empty() || self.c.get(k) != Some(&')') {
                return Ok(());
            }
            for (n, ch) in letters.chars().enumerate() {
                match ch {
                    'L' => {
                        return Err(err(
                            "bad inline flags: cannot use 'L' flag with a str pattern",
                            start + 3 + n,
                        ))
                    }
                    'a' => self.flags.a = true,
                    'u' => seen_u = true,
                    'm' => self.flags.m = true,
                    'x' => self.flags.x = true,
                    'i' => self.flags.i = true,
                    's' => self.flags.s = true,
                    _ => {}
                }
            }
            if self.flags.a && seen_u {
                return Err(err(
                    "bad inline flags: flags 'a', 'u' and 'L' are incompatible",
                    k,
                ));
            }
            self.i = k + 1;
        }
    }

    /// The Rust flags for the leading global flags. Python's MULTILINE
    /// `^`/`$` match at every line edge, as Rust's `(?m)` does; the
    /// default-mode `$` is rewritten instead.
    fn emit_global_flags(&mut self) {
        let mut on = String::new();
        if self.flags.rust_i() {
            on.push('i');
        }
        if self.flags.m {
            on.push('m');
        }
        if self.flags.s {
            on.push('s');
        }
        if !on.is_empty() {
            let _ = write!(self.out, "(?{on})");
        }
    }

    /// A literal code point, ASCII-folded by hand under `(?ai)`.
    fn literal(&mut self, cp: u32) {
        if self.flags.fold() {
            if let Some(c) = char::from_u32(cp).filter(char::is_ascii_alphabetic) {
                let _ = write!(
                    self.out,
                    "[{}{}]",
                    c.to_ascii_lowercase(),
                    c.to_ascii_uppercase()
                );
                return;
            }
        }
        hex_char(&mut self.out, cp);
    }

    fn run(mut self) -> Result<(String, bool), ReError> {
        self.leading_flags()?;
        self.emit_global_flags();
        self.frames.push(Frame::new(Kind::Root, 0, self.out.len()));
        while let Some(ch) = self.peek(0) {
            if self.flags.x {
                if " \t\n\r\x0b\x0c".contains(ch) {
                    self.i += 1;
                    continue;
                }
                if ch == '#' {
                    while let Some(n) = self.peek(0) {
                        self.i += 1;
                        if n == '\n' {
                            break;
                        }
                    }
                    continue;
                }
            }
            match ch {
                '\\' => self.escape()?,
                '[' => {
                    self.class()?;
                    self.item();
                }
                '{' => self.brace()?,
                '*' | '+' | '?' => {
                    let (lo, hi) = match ch {
                        '*' => (0, MAXREPEAT),
                        '+' => (1, MAXREPEAT),
                        _ => (0, 1),
                    };
                    self.repeat(lo, hi, &ch.to_string(), 1)?;
                }
                '$' => {
                    if self.flags.m {
                        self.out.push('$');
                    } else {
                        self.out.push_str(r"(?=\n?\z)");
                    }
                    self.i += 1;
                    self.atom(Atom::At);
                }
                '^' => {
                    self.out.push('^');
                    self.i += 1;
                    self.atom(Atom::At);
                }
                '|' => {
                    self.out.push('|');
                    self.i += 1;
                    let f = self.frame();
                    f.end_branch();
                    f.branches += 1;
                }
                '(' => self.group()?,
                ')' => self.close()?,
                ']' => {
                    self.out.push_str(r"\]");
                    self.i += 1;
                    self.item();
                }
                '}' => {
                    self.out.push_str(r"\}");
                    self.i += 1;
                    self.item();
                }
                _ => {
                    if self.flags.fold() && ch.is_ascii_alphabetic() {
                        self.literal(ch as u32);
                    } else {
                        self.out.push(ch);
                    }
                    self.i += 1;
                    self.item();
                }
            }
        }
        if self.frames.len() > 1 {
            let f = self.frames.last().expect("frame");
            return Err(err("missing ), unterminated subpattern", f.start));
        }
        for &(g, pos) in &self.cond_refs {
            if g > self.groups {
                return Err(err(format!("invalid group reference {g}"), pos));
            }
        }
        Ok((self.out, self.saw_non_boundary))
    }

    /// Applies a quantifier of width (lo, hi), spelled `text` in the output
    /// and `src_len` characters in the source, to the last atom, with its
    /// lazy `?` or possessive `+` suffix.
    fn repeat(&mut self, lo: u64, hi: u64, text: &str, src_len: usize) -> Result<(), ReError> {
        let at = self.i;
        let last = self.frame().last;
        let (alo, ahi) = match last {
            None | Some(Atom::At) => return Err(err("nothing to repeat", at)),
            Some(Atom::Repeat(..)) => return Err(err("multiple repeat", at)),
            Some(Atom::Look(out_start)) => {
                // Python repeats a zero-width look-around; Rust refuses to.
                // At least once is the assertion itself; optional is the
                // assertion tried first (greedy) or last (lazy), then nothing.
                self.i += src_len;
                let lazy = self.peek(0) == Some('?');
                if let Some('?' | '+') = self.peek(0) {
                    self.i += 1;
                }
                if lo == 0 {
                    let look = self.out.split_off(out_start);
                    if lazy {
                        let _ = write!(self.out, "(?:|{look})");
                    } else {
                        let _ = write!(self.out, "(?:{look}|)");
                    }
                }
                self.frame().last = Some(Atom::Repeat(0, 0));
                return Ok(());
            }
            Some(Atom::Item(alo, ahi)) => (alo, ahi),
        };
        self.out.push_str(text);
        self.i += src_len;
        if let Some(sfx @ ('?' | '+')) = self.peek(0) {
            self.out.push(sfx);
            self.i += 1;
        }
        let w = (
            alo.saturating_mul(lo).min(MAXREPEAT),
            ahi.saturating_mul(hi).min(MAXREPEAT),
        );
        self.frame().last = Some(Atom::Repeat(w.0, w.1));
        Ok(())
    }

    /// `{`: a quantifier when it reads `{m}`, `{m,}`, `{,n}`, `{m,n}` or `{,}`,
    /// else a literal brace (sre_parse's rule).
    fn brace(&mut self) -> Result<(), ReError> {
        let mut k = self.i + 1;
        let digits = |k: &mut usize, c: &[char]| {
            let mut s = String::new();
            while let Some(&d) = c.get(*k) {
                if d.is_ascii_digit() {
                    s.push(d);
                    *k += 1;
                } else {
                    break;
                }
            }
            s
        };
        let lo = digits(&mut k, &self.c);
        let mut hi = None;
        if self.c.get(k) == Some(&',') {
            k += 1;
            hi = Some(digits(&mut k, &self.c));
        }
        if self.c.get(k) != Some(&'}') || (lo.is_empty() && hi.is_none()) {
            self.out.push_str(r"\{");
            self.i += 1;
            self.item();
            return Ok(());
        }
        let parse = |s: &str| -> Result<u64, ReError> {
            let v: u64 = s.parse().unwrap_or(u64::MAX);
            if v >= MAXREPEAT {
                return Err(ReError::Overflow);
            }
            Ok(v)
        };
        let min = if lo.is_empty() { 0 } else { parse(&lo)? };
        let (text, max) = match &hi {
            None => (format!("{{{min}}}"), min),
            Some(h) if h.is_empty() => (format!("{{{min},}}"), MAXREPEAT),
            Some(h) => {
                let max = parse(h)?;
                if max < min {
                    return Err(err("min repeat greater than max repeat", self.i + 1));
                }
                (format!("{{{min},{max}}}"), max)
            }
        };
        self.repeat(min, max, &text, k + 1 - self.i)
    }

    fn word_boundary(&mut self, negate: bool) {
        let w = self.classes().word;
        if negate {
            self.saw_non_boundary = true;
            if self.empty_input {
                self.out.push_str(NEVER);
                return;
            }
            let _ = write!(self.out, "(?:(?<=[{w}])(?=[{w}])|(?<![{w}])(?![{w}]))");
        } else {
            let _ = write!(self.out, "(?:(?<=[{w}])(?![{w}])|(?<![{w}])(?=[{w}]))");
        }
    }

    /// Reads exactly `n` hex digits after `\x`, `\u` or `\U` at `self.i`.
    fn hex_digits(&self, n: usize) -> Result<u32, ReError> {
        let mut s = String::new();
        for k in 0..n {
            match self.c.get(self.i + 2 + k) {
                Some(d) if d.is_ascii_hexdigit() => s.push(*d),
                _ => {
                    let got: String = self.c[self.i..(self.i + 2 + k).min(self.c.len())]
                        .iter()
                        .collect();
                    return Err(err(format!("incomplete escape {got}"), self.i));
                }
            }
        }
        u32::from_str_radix(&s, 16).map_err(|e| err(e.to_string(), self.i))
    }

    /// The escapes that mean one code point inside and outside a class:
    /// control letters and `\x`/`\u`/`\U`/`\N`. Returns the code point and
    /// the source length, or None when `e` is not one of them.
    fn literal_escape(&self, e: char) -> Result<Option<(u32, usize)>, ReError> {
        Ok(Some(match e {
            'a' => (0x07, 2),
            'f' => (0x0C, 2),
            'n' => (0x0A, 2),
            'r' => (0x0D, 2),
            't' => (0x09, 2),
            'v' => (0x0B, 2),
            'x' => (self.hex_digits(2)?, 4),
            'u' => (self.hex_digits(4)?, 6),
            'U' => {
                let cp = self.hex_digits(8)?;
                if cp > 0x10FFFF {
                    let got: String = self.c[self.i..self.i + 10].iter().collect();
                    return Err(err(format!("bad escape {got}"), self.i));
                }
                (cp, 10)
            }
            'N' => {
                // Python resolves `\N{NAME}` against its Unicode name table;
                // this port carries no name table, so the pattern is refused
                // (and skipped with the usual warning). See PARITY.md.
                return Err(err(
                    r"named Unicode escape \N{...} is unsupported in the Rust port",
                    self.i,
                ));
            }
            _ => return Ok(None),
        }))
    }

    fn common_escape(&mut self, e: char) -> Result<bool, ReError> {
        let Some((cp, len)) = self.literal_escape(e)? else {
            return Ok(false);
        };
        self.literal(cp);
        self.i += len;
        Ok(true)
    }

    fn escape(&mut self) -> Result<(), ReError> {
        let Some(e) = self.peek(1) else {
            return Err(err("bad escape (end of pattern)", self.i));
        };
        let cls = self.classes();
        match e {
            'A' => {
                self.out.push_str(r"\A");
                self.i += 2;
                self.atom(Atom::At);
            }
            'Z' => {
                self.out.push_str(r"\z");
                self.i += 2;
                self.atom(Atom::At);
            }
            'b' | 'B' => {
                self.word_boundary(e == 'B');
                self.i += 2;
                self.atom(Atom::At);
            }
            'd' | 'D' | 's' | 'S' | 'w' | 'W' => {
                let body = match e.to_ascii_lowercase() {
                    'd' => cls.digit,
                    's' => cls.space,
                    _ => cls.word,
                };
                let neg = if e.is_ascii_uppercase() { "^" } else { "" };
                let _ = write!(self.out, "[{neg}{body}]");
                self.i += 2;
                self.item();
            }
            '0' => {
                // `\0` plus up to two more octal digits.
                let mut s = String::from("0");
                let mut k = self.i + 2;
                while s.len() < 3 && is_oct(self.c.get(k).copied()) {
                    s.push(self.c[k]);
                    k += 1;
                }
                let cp = u32::from_str_radix(&s, 8).unwrap_or(0);
                self.literal(cp);
                self.i = k;
                self.item();
            }
            '1'..='9' => {
                // Octal escape *or* group reference, as sre_parse decides.
                let mut digits = String::from(e);
                let mut k = self.i + 2;
                if let Some(&d2) = self.c.get(k) {
                    if d2.is_ascii_digit() {
                        digits.push(d2);
                        k += 1;
                        let d3 = self.c.get(k).copied();
                        if is_oct(Some(e)) && is_oct(Some(d2)) && is_oct(d3) {
                            digits.push(d3.unwrap_or('0'));
                            k += 1;
                            let cp = u32::from_str_radix(&digits, 8).unwrap_or(0);
                            if cp > 0o377 {
                                return Err(err(
                                    format!(
                                        "octal escape value \\{digits} outside of range 0-0o377"
                                    ),
                                    self.i,
                                ));
                            }
                            self.literal(cp);
                            self.i = k;
                            self.item();
                            return Ok(());
                        }
                    }
                }
                let g: usize = digits.parse().unwrap_or(usize::MAX);
                let width = match self.group_widths.get(g.wrapping_sub(1)) {
                    Some(Some(w)) if g >= 1 => *w,
                    Some(None) => {
                        return Err(err("cannot refer to an open group", self.i));
                    }
                    _ => {
                        return Err(err(format!("invalid group reference {g}"), self.i + 1));
                    }
                };
                // Wrapped so a literal digit after a two-digit reference
                // stays a literal.
                let _ = write!(self.out, r"(?:\{digits})");
                self.i = k;
                self.atom(Atom::Item(width.0, width.1));
            }
            _ if self.common_escape(e)? => self.item(),
            _ if e.is_ascii_alphabetic() => {
                return Err(err(format!("bad escape \\{e}"), self.i));
            }
            _ => {
                self.literal(e as u32);
                self.i += 2;
                self.item();
            }
        }
        Ok(())
    }

    /// One class token at `self.i`: a character or an escape. sre_parse's
    /// `_class_escape`.
    fn class_item(&mut self) -> Result<ClassItem, ReError> {
        let ch = self.peek(0).expect("class item");
        if ch != '\\' {
            self.i += 1;
            return Ok(ClassItem::Lit(ch as u32, ch.to_string()));
        }
        let Some(e) = self.peek(1) else {
            return Err(err("bad escape (end of pattern)", self.i));
        };
        let token: String = [ch, e].iter().collect();
        let cls = self.classes();
        Ok(match e {
            'd' | 's' | 'w' | 'D' | 'S' | 'W' => {
                let body = match e.to_ascii_lowercase() {
                    'd' => cls.digit,
                    's' => cls.space,
                    _ => cls.word,
                };
                self.i += 2;
                if e.is_ascii_uppercase() {
                    ClassItem::Set(format!("[^{body}]"), token)
                } else {
                    ClassItem::Set(body.to_string(), token)
                }
            }
            // Backspace inside a class.
            'b' => {
                self.i += 2;
                ClassItem::Lit(0x08, token)
            }
            '0'..='7' => {
                let mut s = String::from(e);
                let mut k = self.i + 2;
                while s.len() < 3 && is_oct(self.c.get(k).copied()) {
                    s.push(self.c[k]);
                    k += 1;
                }
                let cp = u32::from_str_radix(&s, 8).unwrap_or(0);
                if cp > 0o377 {
                    return Err(err(
                        format!("octal escape value \\{s} outside of range 0-0o377"),
                        self.i,
                    ));
                }
                let token: String = self.c[self.i..k].iter().collect();
                self.i = k;
                ClassItem::Lit(cp, token)
            }
            '8' | '9' => return Err(err(format!("bad escape \\{e}"), self.i)),
            _ => match self.literal_escape(e)? {
                Some((cp, len)) => {
                    let token: String = self.c[self.i..self.i + len].iter().collect();
                    self.i += len;
                    ClassItem::Lit(cp, token)
                }
                None if e.is_ascii_alphabetic() => {
                    return Err(err(format!("bad escape \\{e}"), self.i));
                }
                None => {
                    self.i += 2;
                    ClassItem::Lit(e as u32, token)
                }
            },
        })
    }

    /// `[...]` parsed as sre_parse does (a leading `]` is literal, `[` and
    /// `&`/`~`/`--` are literals, `a-z` is a range only between two single
    /// characters), and emitted with every member escaped so Rust's nested
    /// classes and set operators never apply.
    fn class(&mut self) -> Result<(), ReError> {
        let start = self.i;
        self.i += 1;
        let negate = self.peek(0) == Some('^');
        if negate {
            self.i += 1;
        }
        let fold = self.flags.fold();
        let mut body = String::new();
        let mut members = 0usize;
        let mut first = true;
        loop {
            let Some(ch) = self.peek(0) else {
                return Err(err("unterminated character set", start));
            };
            if ch == ']' && !first {
                self.i += 1;
                break;
            }
            first = false;
            let item1 = self.class_item()?;
            if self.peek(0) == Some('-') {
                self.i += 1;
                let Some(that) = self.peek(0) else {
                    return Err(err("unterminated character set", start));
                };
                if that == ']' {
                    self.i += 1;
                    members += item1.emit(&mut body, fold);
                    members += ClassItem::Lit('-' as u32, String::new()).emit(&mut body, fold);
                    break;
                }
                let item2 = self.class_item()?;
                match (&item1, &item2) {
                    (ClassItem::Lit(lo, _), ClassItem::Lit(hi, _)) if hi >= lo => {
                        members += emit_range(&mut body, *lo, *hi, fold);
                    }
                    _ => {
                        return Err(err(
                            format!("bad character range {}-{}", item1.token(), item2.token()),
                            start,
                        ));
                    }
                }
            } else {
                members += item1.emit(&mut body, fold);
            }
        }
        if members == 0 {
            // Only lone surrogates: nothing a Rust string holds is in the set.
            self.out.push_str(if negate { r"[\s\S]" } else { NEVER });
        } else {
            self.out.push('[');
            if negate {
                self.out.push('^');
            }
            self.out.push_str(&body);
            self.out.push(']');
        }
        Ok(())
    }

    fn open(&mut self, kind: Kind, lead: &str) {
        let start = self.i;
        let out_start = self.out.len();
        self.frame().commit();
        self.out.push_str(lead);
        self.i += lead.chars().count();
        self.frames.push(Frame::new(kind, start, out_start));
    }

    /// Reads source characters up to `close` (a group name or a condition),
    /// consuming the `close`.
    fn read_name(&mut self, close: char) -> Result<String, ReError> {
        let at = self.i;
        let mut inner = String::new();
        while let Some(ch) = self.peek(0) {
            self.i += 1;
            if ch == close {
                if inner.is_empty() {
                    return Err(err("missing group name", at));
                }
                return Ok(inner);
            }
            inner.push(ch);
        }
        Err(err(format!("missing {close}, unterminated name"), at))
    }

    /// The group a name refers to, as sre_parse resolves it.
    fn named_group(&self, name: &str, at: usize) -> Result<usize, ReError> {
        if !is_identifier(name) {
            return Err(err(
                format!("bad character in group name {}", py_repr(name)),
                at,
            ));
        }
        self.names
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, g)| *g)
            .ok_or_else(|| err(format!("unknown group name {}", py_repr(name)), at))
    }

    fn group(&mut self) -> Result<(), ReError> {
        if self.peek(1) != Some('?') {
            self.groups += 1;
            self.group_widths.push(None);
            self.open(Kind::Capture(self.groups), "(");
            return Ok(());
        }
        let at = self.i + 1;
        for (lead, kind) in [
            ("(?:", Kind::Plain),
            ("(?>", Kind::Plain),
            ("(?=", Kind::LookAhead),
            ("(?!", Kind::LookAhead),
            ("(?<=", Kind::LookBehind),
            ("(?<!", Kind::LookBehind),
        ] {
            if self.starts_with(lead) {
                self.open(kind, lead);
                return Ok(());
            }
        }
        if self.starts_with("(?P<") {
            let start = self.i;
            self.i += 4;
            let name = self.read_name('>')?;
            if !is_identifier(&name) {
                return Err(err(
                    format!("bad character in group name {}", py_repr(&name)),
                    start + 4,
                ));
            }
            if self.names.iter().any(|(n, _)| *n == name) {
                return Err(err(
                    format!("redefinition of group name {}", py_repr(&name)),
                    start + 4,
                ));
            }
            self.groups += 1;
            self.group_widths.push(None);
            self.names.push((name.clone(), self.groups));
            let out_start = self.out.len();
            self.frame().commit();
            let _ = write!(self.out, "(?P<{name}>");
            self.frames
                .push(Frame::new(Kind::Capture(self.groups), start, out_start));
            return Ok(());
        }
        if self.starts_with("(?P=") {
            // Named backreference: the group must exist and be closed; it
            // has that group's width.
            let at = self.i + 4;
            self.i += 4;
            let name = self.read_name(')')?;
            let g = self.named_group(&name, at)?;
            let Some(width) = self.group_widths[g - 1] else {
                return Err(err("cannot refer to an open group", at));
            };
            self.frame().commit();
            let _ = write!(self.out, "(?P={name})");
            self.atom(Atom::Item(width.0, width.1));
            return Ok(());
        }
        if self.starts_with("(?(") {
            let start = self.i;
            let out_start = self.out.len();
            self.i += 3;
            let pos = self.i;
            let cond = self.read_name(')')?;
            self.frame().commit();
            if cond.chars().all(|c| c.is_ascii_digit()) {
                let g: usize = cond.parse().unwrap_or(usize::MAX);
                if g == 0 {
                    return Err(err("bad group number", pos));
                }
                self.cond_refs.push((g, pos));
                let _ = write!(self.out, "(?({g})");
            } else {
                self.named_group(&cond, pos)?;
                let _ = write!(self.out, "(?(<{cond}>)");
            }
            self.frames.push(Frame::new(Kind::Cond, start, out_start));
            return Ok(());
        }
        if self.starts_with("(?#") {
            // A comment ends at the first `)`; an escape (`\)`) is one token
            // in Python's tokenizer and does not end it.
            let mut k = self.i + 3;
            while let Some(&ch) = self.c.get(k) {
                if ch == '\\' {
                    k += 2;
                    continue;
                }
                k += 1;
                if ch == ')' {
                    self.i = k;
                    return Ok(());
                }
            }
            return Err(err("missing ), unterminated comment", self.i));
        }
        if self.starts_with("(?P") {
            let x: String = self.peek(3).map(String::from).unwrap_or_default();
            return Err(err(format!("unknown extension ?P{x}"), at));
        }
        if self.starts_with("(?<") {
            let x: String = self.peek(3).map(String::from).unwrap_or_default();
            return Err(err(format!("unknown extension ?<{x}"), at));
        }
        // Inline flags: `(?flags:...)` / `(?on-off:...)` are scoped and fine
        // anywhere; a bare `(?flags)` here is not at the start.
        let mut k = self.i + 2;
        let mut on = String::new();
        let mut off = String::new();
        let mut minus = false;
        while let Some(&ch) = self.c.get(k) {
            if "aiLmsux".contains(ch) {
                if minus {
                    off.push(ch);
                } else {
                    on.push(ch);
                }
                k += 1;
            } else if ch == '-' && !minus {
                minus = true;
                k += 1;
            } else {
                break;
            }
        }
        if k > self.i + 2 {
            match self.c.get(k) {
                Some(')') if !minus => {
                    return Err(err(
                        "global flags not at the start of the expression",
                        self.i,
                    ));
                }
                Some(':') => {
                    if on.contains('L') {
                        return Err(err(
                            "bad inline flags: cannot use 'L' flag with a str pattern",
                            k,
                        ));
                    }
                    if off.chars().any(|ch| "auL".contains(ch)) {
                        return Err(err(
                            "bad inline flags: cannot turn off flags 'a', 'u' and 'L'",
                            k,
                        ));
                    }
                    if minus && off.is_empty() {
                        return Err(err("missing flag", k));
                    }
                    let old = self.flags;
                    let mut new = old;
                    for (set, val) in [(&on, true), (&off, false)] {
                        for ch in set.chars() {
                            match ch {
                                'i' => new.i = val,
                                'm' => new.m = val,
                                's' => new.s = val,
                                'x' => new.x = val,
                                'a' => new.a = val,
                                _ => {}
                            }
                        }
                    }
                    // The Rust flags that change: `i` follows Unicode folding
                    // only (ASCII folding is done per literal), `m` and `s`
                    // pass through; `x` and `a` are handled here.
                    let (mut r_on, mut r_off) = (String::new(), String::new());
                    for (ch, was, now) in [
                        ('i', old.rust_i(), new.rust_i()),
                        ('m', old.m, new.m),
                        ('s', old.s, new.s),
                    ] {
                        if now && !was {
                            r_on.push(ch);
                        } else if was && !now {
                            r_off.push(ch);
                        }
                    }
                    let mut lead = String::from("(?");
                    lead.push_str(&r_on);
                    if !r_off.is_empty() {
                        lead.push('-');
                        lead.push_str(&r_off);
                    }
                    lead.push(':');
                    let start = self.i;
                    let out_start = self.out.len();
                    self.frame().commit();
                    self.out.push_str(&lead);
                    self.i = k + 1;
                    let mut frame = Frame::new(Kind::Plain, start, out_start);
                    frame.saved = Some(old);
                    self.frames.push(frame);
                    self.flags = new;
                    return Ok(());
                }
                _ => {}
            }
        }
        let x: String = self.peek(2).map(String::from).unwrap_or_default();
        if x.is_empty() {
            return Err(err("unexpected end of pattern", at + 1));
        }
        Err(err(format!("unknown extension ?{x}"), at))
    }

    fn close(&mut self) -> Result<(), ReError> {
        if self.frames.len() <= 1 {
            return Err(err("unbalanced parenthesis", self.i));
        }
        let mut f = self.frames.pop().expect("frame");
        f.end_branch();
        if let Some(saved) = f.saved {
            self.flags = saved;
        }
        let (lo, hi) = f.alts.unwrap_or((0, 0));
        self.out.push(')');
        self.i += 1;
        match f.kind {
            Kind::LookBehind | Kind::LookAhead => {
                if f.kind == Kind::LookBehind && lo != hi {
                    return Err(err("look-behind requires fixed-width pattern", f.start));
                }
                self.atom(Atom::Look(f.out_start));
            }
            Kind::Capture(g) => {
                self.group_widths[g - 1] = Some((lo, hi));
                self.atom(Atom::Item(lo, hi));
            }
            Kind::Cond => {
                if f.branches > 2 {
                    return Err(err(
                        "conditional backref with more than two branches",
                        f.start,
                    ));
                }
                let lo = if f.branches < 2 { 0 } else { lo };
                self.atom(Atom::Item(lo, hi));
            }
            Kind::Plain | Kind::Root => self.atom(Atom::Item(lo, hi)),
        }
        Ok(())
    }
}

/// Python's `str.isidentifier()`, close enough for group names: a letter or
/// `_`, then letters, digits or `_`.
fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_')
}

/// Rewrites a Python `re` pattern into a fancy-regex pattern with the same
/// matches, or returns the `re.error` message Python would raise.
///
/// Returns the pattern for ordinary input and, when the pattern uses `\B`,
/// a second one for the empty string (where Python's `\B` never matches).
pub(crate) fn translate(pattern: &str) -> Result<(String, Option<String>), ReError> {
    let run = |empty_input: bool| {
        Translator {
            c: pattern.chars().collect(),
            i: 0,
            out: String::with_capacity(pattern.len() + 16),
            flags: Flags::default(),
            empty_input,
            saw_non_boundary: false,
            frames: Vec::new(),
            groups: 0,
            group_widths: Vec::new(),
            names: Vec::new(),
            cond_refs: Vec::new(),
        }
        .run()
    };
    let (out, non_boundary) = run(false)?;
    let empty = if non_boundary {
        Some(run(true)?.0)
    } else {
        None
    };
    Ok((out, empty))
}

/// Python's `repr()` of a `str`, for the bad-regex warning (`%r`).
pub(crate) fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            // Not printable to Python: control characters and every
            // separator but the space.
            c if c.is_control() || (c.is_whitespace() && c != ' ') => {
                let cp = c as u32;
                let _ = match cp {
                    0..=0xff => write!(out, "\\x{cp:02x}"),
                    0x100..=0xffff => write!(out, "\\u{cp:04x}"),
                    _ => write!(out, "\\U{cp:08x}"),
                };
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repr_matches_python() {
        assert_eq!(py_repr("[unclosed"), "'[unclosed'");
        assert_eq!(py_repr("it's"), "\"it's\"");
        assert_eq!(py_repr("a'b\"c"), "'a\\'b\"c'");
        assert_eq!(
            py_repr("\\d\n\t\u{a0}\u{2028}"),
            "'\\\\d\\n\\t\\xa0\\u2028'"
        );
    }

    #[test]
    fn dollar_and_z_follow_python() {
        assert_eq!(translate("^ls$").unwrap().0, r"^ls(?=\n?\z)");
        assert_eq!(translate("(?m)ls$").unwrap().0, "(?m)ls$");
        assert_eq!(translate(r"a\Z").unwrap().0, r"a\z");
    }

    #[test]
    fn errors_carry_python_messages() {
        assert_eq!(
            translate("[unclosed").unwrap_err(),
            ReError::Error("unterminated character set at position 0".into())
        );
        assert_eq!(
            translate(r"\z").unwrap_err(),
            ReError::Error(r"bad escape \z at position 0".into())
        );
        assert_eq!(
            translate("a(?i)b").unwrap_err(),
            ReError::Error("global flags not at the start of the expression at position 1".into())
        );
        assert_eq!(translate("a{4294967295}").unwrap_err(), ReError::Overflow);
    }

    #[test]
    fn non_boundary_gets_an_empty_input_variant() {
        let (_, empty) = translate(r"\B").unwrap();
        assert_eq!(empty.as_deref(), Some(NEVER));
        assert!(translate(r"\b").unwrap().1.is_none());
    }
}
