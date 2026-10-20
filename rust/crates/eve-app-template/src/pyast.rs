//! `eval_labels` reads bridge/tools/intent_eval.py "without importing it": Python walks `ast.parse(...)`'s module
//! body for top-level assignments to LABELS, TOOL_LABELS, CODE_ACTING, NONE and ASK and gathers every `str`
//! constant in each assigned value (in a dict display, only in its values).
//!
//! This is that walk over Python's own token rules: logical lines (brackets, backslash continuations, comments),
//! string literals with every prefix, escapes, implicit concatenation and f-strings (whose literal pieces are
//! constants too, as in Python 3.12+), and the statement shapes `ast.Assign` / `ast.AnnAssign` give a `Name` target.

use std::collections::BTreeSet;

/// The names whose assigned values hold the eval's labels.
pub const LABEL_NAMES: &[&str] = &["LABELS", "TOOL_LABELS", "CODE_ACTING", "NONE", "ASK"];

/// What `ast.parse` (or the walk) would raise; the text is Python's message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ParseError(pub String);

type Res<T> = Result<T, ParseError>;

fn err<T>(msg: impl Into<String>) -> Res<T> {
    Err(ParseError(msg.into()))
}

/// One piece of a string literal: literal text, or an f-string replacement field.
#[derive(Debug, Clone)]
pub(crate) enum Part {
    Lit(String),
    Field { expr: Vec<Tok>, spec: Option<Vec<Part>> },
}

#[derive(Debug, Clone)]
pub(crate) enum Tok {
    Name(String),
    Op(String),
    Num,
    Str { bytes: bool, fstring: bool, parts: Vec<Part> },
}

impl Tok {
    fn is_op(&self, op: &str) -> bool {
        matches!(self, Tok::Op(o) if o == op)
    }
    fn is_name(&self, name: &str) -> bool {
        matches!(self, Tok::Name(n) if n == name)
    }
}

/// Operators, longest first (Python's tokenizer is greedy). `!` alone is the f-string conversion mark.
const OPS: &[&str] = &[
    "**=", "//=", ">>=", "<<=", "...", "!=", "%=", "&=", "**", "*=", "+=", "-=", "->", "//", "/=", ":=", "<<", "<=",
    "==", ">=", ">>", "@=", "^=", "|=", "!", "%", "&", "(", ")", "*", "+", ",", "-", ".", "/", ":", ";", "<", "=",
    ">", "@", "[", "]", "^", "{", "|", "}", "~",
];

/// Where a token run stops: at the end of the source (a module), or at a replacement field's end (`}`), its
/// conversion (`!`), its spec (`:`) or its self-documenting `=`, all at bracket depth 0.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Module,
    Field,
}

/// One logical line: whether it starts at column 0 (a statement of the module body), and its tokens.
#[derive(Debug)]
pub(crate) struct Line {
    pub top: bool,
    pub toks: Vec<Tok>,
}

struct Lexer {
    s: Vec<char>,
    i: usize,
    line_start: usize,
}

/// Python identifiers start with XID_Start or `_`; `is_alphabetic` (letters and letter numbers) stands in for it.
fn is_id_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

/// XID_Continue: letters, digits, `_`, combining marks and the joiners.
fn is_id_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric() || (!c.is_ascii() && matches!(c, '\u{300}'..='\u{36f}' | '\u{200c}' | '\u{200d}'))
}

impl Lexer {
    fn new(src: &str) -> Self {
        Lexer { s: src.chars().collect(), i: 0, line_start: 0 }
    }

    fn peek(&self, k: usize) -> Option<char> {
        self.s.get(self.i + k).copied()
    }

    fn starts_with(&self, text: &str) -> bool {
        text.chars().enumerate().all(|(k, c)| self.peek(k) == Some(c))
    }

    /// The module: its logical lines.
    fn lines(&mut self) -> Res<Vec<Line>> {
        let mut out = Vec::new();
        let mut toks: Vec<Tok> = Vec::new();
        let mut top = false;
        let mut depth: usize = 0;
        while let Some(c) = self.peek(0) {
            match c {
                ' ' | '\t' | '\x0c' => self.i += 1,
                '#' => {
                    while self.peek(0).is_some_and(|c| c != '\n' && c != '\r') {
                        self.i += 1;
                    }
                }
                '\\' if matches!(self.peek(1), Some('\n') | Some('\r')) => {
                    self.i += if self.peek(1) == Some('\r') && self.peek(2) == Some('\n') { 3 } else { 2 };
                    self.line_start = self.i;
                }
                '\\' => return err("unexpected character after line continuation character"),
                '\n' | '\r' => {
                    self.i += if c == '\r' && self.peek(1) == Some('\n') { 2 } else { 1 };
                    self.line_start = self.i;
                    if depth == 0 && !toks.is_empty() {
                        out.push(Line { top, toks: std::mem::take(&mut toks) });
                    }
                }
                _ => {
                    if toks.is_empty() && depth == 0 {
                        top = self.i == self.line_start;
                    }
                    let tok = self.token(&mut depth, Mode::Module)?;
                    if let Some(t) = tok {
                        toks.push(t);
                    }
                }
            }
        }
        if depth > 0 {
            return err("unexpected EOF in multi-line statement");
        }
        if !toks.is_empty() {
            out.push(Line { top, toks });
        }
        Ok(out)
    }

    /// One token at the cursor (not whitespace, a comment or a line end). None when `mode` says to stop here.
    fn token(&mut self, depth: &mut usize, mode: Mode) -> Res<Option<Tok>> {
        let c = self.peek(0).unwrap_or('\0');
        if c == '"' || c == '\'' {
            return self.string("").map(Some);
        }
        if is_id_start(c) {
            let start = self.i;
            while self.peek(0).is_some_and(is_id_continue) {
                self.i += 1;
            }
            let word: String = self.s[start..self.i].iter().collect();
            if matches!(self.peek(0), Some('"') | Some('\''))
                && matches!(word.to_ascii_lowercase().as_str(), "r" | "u" | "b" | "br" | "rb" | "f" | "fr" | "rf")
            {
                return self.string(&word).map(Some);
            }
            return Ok(Some(Tok::Name(word)));
        }
        if c.is_ascii_digit() || (c == '.' && self.peek(1).is_some_and(|d| d.is_ascii_digit())) {
            while let Some(d) = self.peek(0) {
                if d.is_ascii_alphanumeric() || d == '_' || d == '.' {
                    let exp = matches!(d, 'e' | 'E') && matches!(self.peek(1), Some('+') | Some('-'));
                    self.i += if exp { 2 } else { 1 };
                } else {
                    break;
                }
            }
            return Ok(Some(Tok::Num));
        }
        let Some(op) = OPS.iter().find(|op| self.starts_with(op)) else {
            return err(format!("invalid character '{c}' (U+{:04X})", c as u32));
        };
        // in a replacement field, `:` starts the format spec even before `=` (`{x:=10}` is x formatted with "=10")
        if mode == Mode::Field && *depth == 0 && matches!(*op, "}" | "!" | ":" | "=" | ":=") {
            return Ok(None);
        }
        match *op {
            "(" | "[" | "{" => *depth += 1,
            ")" | "]" | "}" => {
                if *depth == 0 {
                    return err(format!("unmatched '{op}'"));
                }
                *depth -= 1;
            }
            _ => {}
        }
        self.i += op.chars().count();
        Ok(Some(Tok::Op(op.to_string())))
    }

    /// A string literal whose prefix (already consumed) is `prefix`; the cursor is on its opening quote.
    fn string(&mut self, prefix: &str) -> Res<Tok> {
        let p = prefix.to_ascii_lowercase();
        let (raw, bytes, fstring) = (p.contains('r'), p.contains('b'), p.contains('f'));
        let q = self.peek(0).unwrap_or('"');
        let triple = self.peek(1) == Some(q) && self.peek(2) == Some(q);
        self.i += if triple { 3 } else { 1 };
        let close: String = if triple { [q; 3].iter().collect() } else { q.to_string() };
        let parts = if fstring {
            self.fstring_parts(&close, raw, false)?
        } else {
            let start = self.i;
            loop {
                match self.peek(0) {
                    None => return err("unterminated string literal"),
                    Some('\\') => self.i += 2,
                    Some('\n') | Some('\r') if !triple => return err("unterminated string literal"),
                    Some(_) if self.starts_with(&close) => break,
                    Some(_) => self.i += 1,
                }
            }
            let body: String = self.s[start..self.i].iter().collect();
            self.i += close.chars().count();
            let text = if bytes { String::new() } else { body_text(&body, raw)? };
            vec![Part::Lit(text)]
        };
        Ok(Tok::Str { bytes, fstring, parts })
    }

    /// An f-string's pieces up to `close` (or, `in_spec`, up to the `}` ending a format spec), consuming `close`.
    fn fstring_parts(&mut self, close: &str, raw: bool, in_spec: bool) -> Res<Vec<Part>> {
        let mut parts = Vec::new();
        let mut lit = String::new();
        loop {
            let Some(c) = self.peek(0) else { return err("unterminated f-string literal") };
            if in_spec && c == '}' {
                break;
            }
            if !in_spec && self.starts_with(close) {
                self.i += close.chars().count();
                break;
            }
            if close.chars().count() == 1 && (c == '\n' || c == '\r') {
                return err("unterminated f-string literal");
            }
            match c {
                '\\' => {
                    lit.push('\\');
                    self.i += 1;
                    if !raw && self.peek(0) == Some('N') && self.peek(1) == Some('{') {
                        while let Some(d) = self.peek(0) {
                            lit.push(d);
                            self.i += 1;
                            if d == '}' {
                                break;
                            }
                        }
                    } else if let Some(d) = self.peek(0) {
                        if d != '{' && d != '}' {
                            lit.push(d);
                            self.i += 1;
                        }
                    }
                }
                '{' if !in_spec && self.peek(1) == Some('{') => {
                    lit.push('{');
                    self.i += 2;
                }
                '}' if !in_spec && self.peek(1) == Some('}') => {
                    lit.push('}');
                    self.i += 2;
                }
                '}' => return err("f-string: single '}' is not allowed"),
                '{' => {
                    parts.push(Part::Lit(body_text(&std::mem::take(&mut lit), raw)?));
                    self.i += 1;
                    let field_start = self.i;
                    let expr = self.field_expr()?;
                    if expr.is_empty() {
                        return err("f-string: valid expression required before '}'");
                    }
                    if self.peek(0) == Some('=') {
                        self.i += 1;
                        while matches!(self.peek(0), Some(' ') | Some('\t') | Some('\n') | Some('\r') | Some('\x0c')) {
                            self.i += 1;
                        }
                        // the self-documenting text, `{x = }`, is a literal before the value
                        parts.push(Part::Lit(self.s[field_start..self.i].iter().collect()));
                    }
                    if self.peek(0) == Some('!') {
                        self.i += 1;
                        while self.peek(0).is_some_and(is_id_continue) {
                            self.i += 1;
                        }
                        while matches!(self.peek(0), Some(' ') | Some('\t')) {
                            self.i += 1;
                        }
                    }
                    let mut spec = None;
                    if self.peek(0) == Some(':') {
                        self.i += 1;
                        spec = Some(self.fstring_parts(close, raw, true)?);
                    }
                    if self.peek(0) != Some('}') {
                        return err("f-string: expecting '}'");
                    }
                    self.i += 1;
                    parts.push(Part::Field { expr, spec });
                }
                _ => {
                    lit.push(c);
                    self.i += 1;
                }
            }
        }
        parts.push(Part::Lit(body_text(&lit, raw)?));
        Ok(parts)
    }

    /// A replacement field's expression tokens, stopping before its `}`, `!`, `:` or `=` at depth 0.
    fn field_expr(&mut self) -> Res<Vec<Tok>> {
        let mut toks = Vec::new();
        let mut depth = 0usize;
        loop {
            match self.peek(0) {
                None => return err("f-string: expecting '}'"),
                Some(' ' | '\t' | '\n' | '\r' | '\x0c') => self.i += 1,
                Some('#') => return err("f-string expression part cannot include '#'"),
                Some(_) => match self.token(&mut depth, Mode::Field)? {
                    Some(t) => toks.push(t),
                    None => return Ok(toks),
                },
            }
        }
    }
}

/// A literal's text: raw as written, else with Python's escapes decoded.
fn body_text(body: &str, raw: bool) -> Res<String> {
    let body = body.replace("\r\n", "\n").replace('\r', "\n");
    if raw {
        return Ok(body);
    }
    let c: Vec<char> = body.chars().collect();
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while i < c.len() {
        if c[i] != '\\' {
            out.push(c[i]);
            i += 1;
            continue;
        }
        let Some(&e) = c.get(i + 1) else {
            out.push('\\');
            break;
        };
        i += 2;
        match e {
            '\n' => {}
            '\\' => out.push('\\'),
            '\'' => out.push('\''),
            '"' => out.push('"'),
            'a' => out.push('\x07'),
            'b' => out.push('\x08'),
            'f' => out.push('\x0c'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'v' => out.push('\x0b'),
            '0'..='7' => {
                let mut v = e.to_digit(8).unwrap_or(0);
                for _ in 0..2 {
                    match c.get(i).and_then(|d| d.to_digit(8)) {
                        Some(d) => {
                            v = v * 8 + d;
                            i += 1;
                        }
                        None => break,
                    }
                }
                out.push(char::from_u32(v).unwrap_or('\u{fffd}'));
            }
            'x' | 'u' | 'U' => {
                let n = match e {
                    'x' => 2,
                    'u' => 4,
                    _ => 8,
                };
                let digits: String = c.iter().skip(i).take(n).collect();
                if digits.chars().count() != n || !digits.chars().all(|d| d.is_ascii_hexdigit()) {
                    return err(format!("(unicode error) 'unicodeescape' codec can't decode bytes: truncated \\{e}XX escape"));
                }
                i += n;
                let v = u32::from_str_radix(&digits, 16).unwrap_or(0);
                if v > 0x10ffff {
                    return err("(unicode error) 'unicodeescape' codec can't decode bytes: illegal Unicode character");
                }
                // a lone surrogate, which a Python str can hold and a Rust String cannot, becomes U+FFFD
                out.push(char::from_u32(v).unwrap_or('\u{fffd}'));
            }
            'N' => {
                if c.get(i) != Some(&'{') {
                    return err("(unicode error) 'unicodeescape' codec can't decode bytes: malformed \\N character escape");
                }
                let Some(end) = c[i..].iter().position(|&d| d == '}') else {
                    return err("(unicode error) 'unicodeescape' codec can't decode bytes: malformed \\N character escape");
                };
                let name: String = c[i + 1..i + end].iter().collect();
                i += end + 1;
                match unicode_names2::character(&name.to_uppercase()) {
                    Some(ch) => out.push(ch),
                    None => {
                        return err("(unicode error) 'unicodeescape' codec can't decode bytes: unknown Unicode character name")
                    }
                }
            }
            other => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    Ok(out)
}

/// Every str constant `ast.walk` meets in these tokens.
fn constants(toks: &[Tok], out: &mut BTreeSet<String>) -> Res<()> {
    let mut i = 0;
    while i < toks.len() {
        if !matches!(toks[i], Tok::Str { .. }) {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < toks.len() && matches!(toks[j], Tok::Str { .. }) {
            j += 1;
        }
        run_constants(&toks[i..j], out)?;
        i = j;
    }
    Ok(())
}

/// Adjacent string literals are one node: a `Constant` (bytes are not str), or with any f-string a `JoinedStr`
/// whose neighbouring literal pieces are merged and whose empty ones are dropped.
fn run_constants(run: &[Tok], out: &mut BTreeSet<String>) -> Res<()> {
    let mut any_f = false;
    let mut any_bytes = false;
    for t in run {
        if let Tok::Str { bytes, fstring, .. } = t {
            any_bytes |= *bytes;
            any_f |= *fstring;
        }
    }
    if any_bytes {
        if any_f || run.iter().any(|t| matches!(t, Tok::Str { bytes: false, .. })) {
            return err("cannot mix bytes and nonbytes literals");
        }
        return Ok(());
    }
    let parts = run.iter().flat_map(|t| match t {
        Tok::Str { parts, .. } => parts.iter(),
        _ => [].iter(),
    });
    if !any_f {
        out.insert(parts.filter_map(|p| if let Part::Lit(s) = p { Some(s.as_str()) } else { None }).collect());
        return Ok(());
    }
    joined_constants(parts, out)
}

fn joined_constants<'a>(parts: impl Iterator<Item = &'a Part>, out: &mut BTreeSet<String>) -> Res<()> {
    let mut lit = String::new();
    for part in parts {
        match part {
            Part::Lit(s) => lit.push_str(s),
            Part::Field { expr, spec } => {
                if !lit.is_empty() {
                    out.insert(std::mem::take(&mut lit));
                }
                constants(expr, out)?;
                if let Some(spec) = spec {
                    joined_constants(spec.iter(), out)?;
                }
            }
        }
    }
    if !lit.is_empty() {
        out.insert(lit);
    }
    Ok(())
}

fn opens(t: &Tok) -> bool {
    matches!(t, Tok::Op(o) if o == "(" || o == "[" || o == "{")
}

fn closes(t: &Tok) -> bool {
    matches!(t, Tok::Op(o) if o == ")" || o == "]" || o == "}")
}

/// The positions of `op` at depth 0 in `toks`, up to the first depth-0 `lambda` when `stop_at_lambda`.
fn top_level(toks: &[Tok], op: &str, stop_at_lambda: bool) -> Vec<usize> {
    let mut depth = 0usize;
    let mut out = Vec::new();
    for (k, t) in toks.iter().enumerate() {
        if opens(t) {
            depth += 1;
        } else if closes(t) {
            depth = depth.saturating_sub(1);
        } else if depth == 0 {
            if stop_at_lambda && t.is_name("lambda") {
                break;
            }
            if t.is_op(op) {
                out.push(k);
            }
        }
    }
    out
}

/// The str constants of one assigned value: a dict display gives only its values' (and `**` spreads').
fn value_constants(value: &[Tok], out: &mut BTreeSet<String>) -> Res<()> {
    match dict_values(value) {
        Some(values) => {
            for v in values {
                constants(v, out)?;
            }
            Ok(())
        }
        None => constants(value, out),
    }
}

/// `value`'s entries' values when it is exactly a dict display (`ast.Dict`); None for anything else (a set, a
/// comprehension, a call, `{...} | {...}`).
fn dict_values(value: &[Tok]) -> Option<Vec<&[Tok]>> {
    if value.len() < 2 || !value[0].is_op("{") || !value[value.len() - 1].is_op("}") {
        return None;
    }
    let inner = &value[1..value.len() - 1];
    // the closing brace must match the opening one
    let mut depth = 0usize;
    for t in inner {
        if opens(t) {
            depth += 1;
        } else if closes(t) {
            depth = depth.checked_sub(1)?;
        }
    }
    if inner.is_empty() {
        return Some(Vec::new());
    }
    if inner.iter().enumerate().any(|(k, t)| t.is_name("for") && depth_at(inner, k) == 0) {
        return None; // a comprehension
    }
    let mut entries: Vec<&[Tok]> = Vec::new();
    let mut start = 0;
    for k in top_level(inner, ",", false) {
        entries.push(&inner[start..k]);
        start = k + 1;
    }
    if start < inner.len() {
        entries.push(&inner[start..]);
    }
    let mut values = Vec::new();
    for entry in entries {
        if entry.first().is_some_and(|t| t.is_op("**")) {
            values.push(&entry[1..]);
            continue;
        }
        let colon = key_colon(entry)?;
        values.push(&entry[colon + 1..]);
    }
    Some(values)
}

fn depth_at(toks: &[Tok], at: usize) -> usize {
    let mut depth = 0usize;
    for t in &toks[..at] {
        if opens(t) {
            depth += 1;
        } else if closes(t) {
            depth = depth.saturating_sub(1);
        }
    }
    depth
}

/// The `:` between a dict entry's key and value: the first at depth 0 that no `lambda` before it claims.
fn key_colon(entry: &[Tok]) -> Option<usize> {
    let mut depth = 0usize;
    let mut lambdas = 0usize;
    for (k, t) in entry.iter().enumerate() {
        if opens(t) {
            depth += 1;
        } else if closes(t) {
            depth = depth.saturating_sub(1);
        } else if depth == 0 {
            if t.is_name("lambda") {
                lambdas += 1;
            } else if t.is_op(":") {
                if lambdas == 0 {
                    return Some(k);
                }
                lambdas -= 1;
            }
        }
    }
    None
}

/// A compound statement's first word: its line's statements after the `:` are not the module body's.
const COMPOUND: &[&str] = &["if", "elif", "else", "for", "while", "with", "def", "class", "try", "except", "finally", "async"];

/// The str constants assigned at the top level of `src` to any of `names`, as Python's `eval_labels` gathers them.
pub fn labels_in(src: &str, names: &[&str]) -> Result<BTreeSet<String>, ParseError> {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let lines = Lexer::new(src).lines()?;
    let mut out = BTreeSet::new();
    for line in lines {
        if !line.top {
            continue;
        }
        if line.toks.first().is_some_and(|t| t.is_op("@") || matches!(t, Tok::Name(n) if COMPOUND.contains(&n.as_str()))) {
            continue;
        }
        let mut start = 0;
        let mut stmts: Vec<&[Tok]> = Vec::new();
        for k in top_level(&line.toks, ";", false) {
            stmts.push(&line.toks[start..k]);
            start = k + 1;
        }
        stmts.push(&line.toks[start..]);
        for stmt in stmts {
            assigned(stmt, names, &mut out)?;
        }
    }
    Ok(out)
}

/// One simple statement: `NAME = ... = value` or `NAME: annotation = value`, a target in redundant parentheses
/// included; anything else assigns nothing we read.
fn assigned(stmt: &[Tok], names: &[&str], out: &mut BTreeSet<String>) -> Res<()> {
    let opened = stmt.iter().take_while(|t| t.is_op("(")).count();
    let Some(Tok::Name(target)) = stmt.get(opened) else { return Ok(()) };
    let after = opened + 1;
    if stmt.len() < after + opened || !stmt[after..after + opened].iter().all(|t| t.is_op(")")) {
        return Ok(());
    }
    let rest = &stmt[after + opened..];
    let Some(first) = rest.first() else { return Ok(()) };
    if !names.contains(&target.as_str()) {
        return Ok(());
    }
    if first.is_op("=") {
        let eqs = top_level(rest, "=", true);
        let last = *eqs.last().unwrap_or(&0);
        value_constants(&rest[last + 1..], out)?;
    } else if first.is_op(":") {
        match top_level(rest, "=", false).first() {
            Some(&eq) => value_constants(&rest[eq + 1..], out)?,
            // `NAME: annotation` with no value: Python's walk of None raises
            None => return err("'NoneType' object has no attribute '_fields'"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(src: &str) -> Vec<String> {
        labels_in(src, LABEL_NAMES).unwrap().into_iter().collect()
    }

    #[test]
    fn reads_the_shapes_python_s_walk_reads() {
        // expected sets produced by the Python 3.13 `eval_labels` over the same sources
        assert_eq!(labels("NONE, ASK = \"none\", \"ask\"\nLABELS = (\"a\", \"b\") + x.labels() + (\"c\",)\n"),
                   ["a", "b", "c"]);
        assert_eq!(labels("TOOL_LABELS: dict[str, str] = {\"k\": \"v\", **dict.fromkeys((\"n1\", \"n2\"), \"n\")}\n"),
                   ["n", "n1", "n2", "v"]);
        assert_eq!(labels("CODE_ACTING = frozenset({\"s\", \"t\"}) | {a.label for a in apps if a.x == \"q\"}\n"),
                   ["q", "s", "t"]);
        assert_eq!(labels("LABELS = {\"k\": \"v\" for k in y}\n"), ["k", "v"]);
        assert_eq!(labels("LABELS = \"a\" 'b' \"\"\"c\"\"\"\n"), ["abc"]);
        assert_eq!(labels("LABELS = ''\n"), [""]);
        assert_eq!(labels("LABELS = b'x', r'\\n', '\\x41\\101\\u00e9\\N{bullet}'\n"), ["AAé•", "\\n"]);
        assert_eq!(labels("LABELS = f'a{x!r:>{w}}b' 'c' f'{y=}'\n"), [">", "a", "bcy="]);
        assert_eq!(labels("if x: LABELS = ('no',)\nclass A:\n    LABELS = ('no',)\nx = 1; ASK = 'yes'\n"), ["yes"]);
        assert_eq!(labels("x = LABELS = 'no'\nLABELS = y = 'yes'\nLABELS += ('no',)\n(NONE) = 'p'\n"), ["p", "yes"]);
        assert_eq!(labels("S = '''\nLABELS = ('no',)\n'''\n# LABELS = 'no'\nLABELS = \\\n  'cont'\n"), ["cont"]);
        assert_eq!(labels("LABELS = {lambda: 'l': 'v', 'k2': lambda x='d': 'w'}\n"), ["d", "v", "w"]);
        assert!(labels_in("NONE: str\n", LABEL_NAMES).is_err());
        assert!(labels_in("LABELS = ('a'\n", LABEL_NAMES).is_err());
    }
}
