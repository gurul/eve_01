//! `build_mail`'s RFC 2822 message, byte for byte as Python's `email.message.EmailMessage(policy=SMTP)` writes
//! it (CPython 3.13): the headers folded by the email package's `_refold_parse_tree` (RFC 2047 encoded words
//! for non-ASCII text, lines kept to 78 characters), then `set_content`'s text part with the transfer encoding
//! its heuristic picks (7bit, 8bit, quoted-printable or base64), CRLF line ends throughout.

use std::collections::VecDeque;

use base64::Engine;

use crate::py::{is_space, splitlines};
use crate::Error;

const MAXLEN: usize = 78;
const LINESEP: &str = "\r\n";

fn is_wsp(c: char) -> bool {
    c == ' ' || c == '\t'
}

fn clen(s: &str) -> isize {
    s.chars().count() as isize
}

// ---- the parse tree the folder walks -----------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Tok {
    Term {
        text: String,
        ttype: &'static str,
        whitespace: bool,
        ew_allowed: bool,
        syntactic_break: bool,
    },
    List {
        ttype: &'static str,
        children: Vec<Tok>,
        ew_allowed: Option<bool>,
        /// `startswith_fws()` when the children are a rendering (a comment or a quoted string written with
        /// its delimiters) rather than the parsed tokens it is asked of.
        first_fws: Option<bool>,
    },
    /// `end_ew_not_allowed`: closes a stretch where encoded words are blocked.
    EndBlock,
}

impl Tok {
    fn value(text: &str, ttype: &'static str) -> Tok {
        Tok::Term { text: text.into(), ttype, whitespace: false, ew_allowed: true, syntactic_break: true }
    }

    fn fws(text: &str) -> Tok {
        Tok::Term { text: text.into(), ttype: "fws", whitespace: true, ew_allowed: true, syntactic_break: true }
    }

    fn list(ttype: &'static str, children: Vec<Tok>) -> Tok {
        Tok::List { ttype, children, ew_allowed: None, first_fws: None }
    }

    fn list_no_ew(ttype: &'static str, children: Vec<Tok>) -> Tok {
        Tok::List { ttype, children, ew_allowed: Some(false), first_fws: None }
    }

    fn text(&self) -> String {
        match self {
            Tok::Term { text, .. } => text.clone(),
            Tok::List { children, .. } => children.iter().map(Tok::text).collect(),
            Tok::EndBlock => String::new(),
        }
    }

    fn ttype(&self) -> &'static str {
        match self {
            Tok::Term { ttype, .. } | Tok::List { ttype, .. } => ttype,
            Tok::EndBlock => "wrap_as_ew_blocked",
        }
    }

    fn as_ew_allowed(&self) -> bool {
        match self {
            Tok::Term { ew_allowed, .. } => *ew_allowed,
            Tok::List { ew_allowed: Some(v), .. } => *v,
            Tok::List { children, .. } => children.iter().all(Tok::as_ew_allowed),
            Tok::EndBlock => true,
        }
    }

    fn syntactic_break(&self) -> bool {
        match self {
            Tok::Term { syntactic_break, .. } => *syntactic_break,
            _ => true,
        }
    }

    fn startswith_fws(&self) -> bool {
        match self {
            Tok::Term { whitespace, .. } => *whitespace,
            Tok::List { first_fws: Some(v), .. } => *v,
            Tok::List { children, .. } => children.first().map(Tok::startswith_fws).unwrap_or(false),
            Tok::EndBlock => false,
        }
    }
}

/// The header tree of an unstructured value (Subject, In-Reply-To, References): `get_unstructured`'s tokens,
/// with an encoded word the value itself holds read back into its text, as the email package's header parser
/// does before folding.
fn unstructured(value: &str) -> Tok {
    Tok::list("unstructured", unstructured_parts(value))
}

/// The tokens of `unstructured(value)`.
fn unstructured_parts(value: &str) -> Vec<Tok> {
    get_unstructured(value)
        .into_iter()
        .map(|t| match t {
            UTok::Fws(text) => Tok::fws(&text),
            UTok::EwWs => Tok::fws(""),
            UTok::Vtext(text) => Tok::value(&text, "utext"),
            UTok::Ew(children) => Tok::list(
                "encoded-word",
                children
                    .into_iter()
                    .map(|(ws, text)| if ws { Tok::fws(&text) } else { Tok::value(&text, "utext") })
                    .collect(),
            ),
        })
        .collect()
}

/// RFC 5322 atext as the email package reads it: anything but the specials and blanks.
fn atext_ok(piece: &str) -> bool {
    !piece.is_empty() && !piece.chars().any(|c| "()<>@,:;.\\\"[] \t".contains(c))
}

fn dot_atom(text: &str, leading_space: bool) -> Option<Tok> {
    let pieces: Vec<&str> = text.split('.').collect();
    if !pieces.iter().all(|p| atext_ok(p)) {
        return None;
    }
    let mut inner = Vec::new();
    for (i, p) in pieces.iter().enumerate() {
        if i > 0 {
            inner.push(Tok::value(".", "dot"));
        }
        inner.push(Tok::value(p, "atext"));
    }
    let text = Tok::List { ttype: "dot-atom-text", children: inner, ew_allowed: Some(true), first_fws: None };
    let mut children = Vec::new();
    if leading_space {
        children.push(Tok::list("cfws", vec![Tok::fws(" ")]));
    }
    children.push(text);
    Some(Tok::list("dot-atom", children))
}

/// One address of `get_address_list`'s tree: address > mailbox > addr-spec > local-part @ domain, each a
/// dot-atom. An address the email package would read as something else (a comment, a domain literal, a
/// quoted local part) is kept whole, as one word that is never encoded.
fn address(addr: &str, leading_space: bool) -> Tok {
    let spec = addr.rsplit_once('@').and_then(|(local, domain)| {
        let local = dot_atom(local, leading_space)?;
        let domain = dot_atom(domain, false)?;
        Some(Tok::list_no_ew(
            "addr-spec",
            vec![
                Tok::list_no_ew("local-part", vec![local]),
                Tok::value("@", "address-at-symbol"),
                Tok::list_no_ew("domain", vec![domain]),
            ],
        ))
    });
    let spec = spec.unwrap_or_else(|| {
        let text = if leading_space { format!(" {addr}") } else { addr.to_string() };
        Tok::Term { text, ttype: "addr-spec", whitespace: leading_space, ew_allowed: false, syntactic_break: true }
    });
    Tok::list("address", vec![Tok::list("mailbox", vec![spec])])
}

fn address_list(addrs: &[String]) -> Tok {
    let mut children = Vec::new();
    for (i, a) in addrs.iter().enumerate() {
        if i > 0 {
            children.push(Tok::Term {
                text: ",".into(),
                ttype: "list-separator",
                whitespace: false,
                ew_allowed: false,
                syntactic_break: false,
            });
        }
        children.push(address(a, i > 0));
    }
    Tok::list("address-list", children)
}

// ---- In-Reply-To and References: parse_message_ids -------------------------------------------------------------
//
// CPython 3.13.14, the bridge's interpreter, reads both headers as a list of msg-ids (ReferencesHeader), not as
// unstructured text: a msg-id is never encoded and is broken only at its blanks, and a value that stops being a
// msg-id list at some point keeps that rest as unstructured text (InvalidMessageID). The parsers below build the
// email package's tree for that grammar (`get_msg_id` and what it calls), which is what the folder walks.

const ATOM_ENDS: &str = "\\(@\t)>.,[<] ;:\"";
const SPECIALS: &str = "\\(@)>.,[<];:\"";
const PHRASE_ENDS: &str = "\\@)>,[<];:";

/// A parse that failed the way HeaderParseError does (None), or crashed (an Err the caller sees).
type Parsed<'a> = Result<Option<(Tok, &'a str)>, Error>;

fn first(v: &str) -> Option<char> {
    v.chars().next()
}

fn starts_in(v: &str, set: &str) -> bool {
    first(v).is_some_and(|c| set.contains(c))
}

fn starts_cfws(v: &str) -> bool {
    starts_in(v, " \t(")
}

fn ptext(text: &str) -> Tok {
    Tok::value(text, "ptext")
}

/// A list whose `startswith_fws()` is that of the parsed tokens it renders.
fn rendered(ttype: &'static str, children: Vec<Tok>, first_fws: bool) -> Tok {
    Tok::List { ttype, children, ew_allowed: None, first_fws: Some(first_fws) }
}

/// `_get_ptext_to_endchars(value, endchars)`: the first blank-free fragment up to an unescaped end character,
/// quoted pairs read as their character. A value that starts with a blank makes CPython reach an unset local
/// (`pos`), an UnboundLocalError that nothing catches.
fn ptext_to_endchars<'a>(value: &'a str, endchars: &str) -> Result<(String, &'a str), Error> {
    if value.is_empty() {
        return Ok((String::new(), value));
    }
    let frag_end = value.find(is_wsp).unwrap_or(value.len());
    if frag_end == 0 {
        return Err(Error::Other {
            name: "UnboundLocalError",
            msg: "cannot access local variable 'pos' where it is not associated with a value".into(),
        });
    }
    let mut out = String::new();
    let mut escape = false;
    for (i, c) in value[..frag_end].char_indices() {
        if c == '\\' {
            if escape {
                escape = false;
            } else {
                escape = true;
                continue;
            }
        }
        if escape {
            escape = false;
        } else if endchars.contains(c) {
            return Ok((out, &value[i..]));
        }
        out.push(c);
    }
    Ok((out, &value[frag_end..]))
}

fn fws_tok(value: &str) -> (Tok, &str) {
    let (ws, rest) = get_fws(value);
    (Tok::fws(&ws), rest)
}

/// `get_comment`, rendered as the folder unpacks one: "(", its text with ( ) and \ quoted, ")".
fn get_comment(value: &str) -> Result<(Tok, &str), Error> {
    let mut v = &value[1..];
    let mut children = vec![ptext("(")];
    let mut first_fws = None;
    while !v.is_empty() && !v.starts_with(')') {
        let (tok, rest, fws) = if v.starts_with(is_wsp) {
            let (t, r) = fws_tok(v);
            (t, r, true)
        } else if v.starts_with('(') {
            let (t, r) = get_comment(v)?;
            let f = t.startswith_fws();
            (t, r, f)
        } else {
            let (text, r) = ptext_to_endchars(v, "()")?;
            (ptext(&text.replace('\\', "\\\\").replace('(', "\\(").replace(')', "\\)")), r, false)
        };
        first_fws.get_or_insert(fws);
        children.push(tok);
        v = rest;
    }
    v = v.strip_prefix(')').unwrap_or(v);
    children.push(ptext(")"));
    Ok((rendered("comment", children, first_fws.unwrap_or(false)), v))
}

/// `get_cfws`: blanks and comments.
fn get_cfws(mut value: &str) -> Result<(Tok, &str), Error> {
    let mut children = Vec::new();
    while starts_cfws(value) {
        let (tok, rest) = if value.starts_with('(') { get_comment(value)? } else { fws_tok(value) };
        children.push(tok);
        value = rest;
    }
    Ok((Tok::list("cfws", children), value))
}

/// `[CFWS]` where the grammar allows it: pushed onto `children`.
fn opt_cfws<'a>(value: &'a str, children: &mut Vec<Tok>) -> Result<&'a str, Error> {
    if !starts_cfws(value) {
        return Ok(value);
    }
    let (tok, rest) = get_cfws(value)?;
    children.push(tok);
    Ok(rest)
}

/// `get_bare_quoted_string`, rendered as the folder unpacks one: the quote marks, and each piece with " and \
/// quoted. An encoded word inside is read as plain text.
fn get_bare_quoted_string(value: &str) -> Result<(Tok, &str), Error> {
    let mut v = &value[1..];
    let mut children = vec![ptext("\"")];
    let mut first_fws = None;
    if let Some(rest) = v.strip_prefix('"') {
        children.push(ptext("\""));
        return Ok((rendered("bare-quoted-string", children, false), rest));
    }
    while !v.is_empty() && !v.starts_with('"') {
        let (text, rest, fws) = if v.starts_with(is_wsp) {
            let (ws, r) = get_fws(v);
            (ws, r, true)
        } else {
            let (t, r) = ptext_to_endchars(v, "\"")?;
            (t, r, false)
        };
        first_fws.get_or_insert(fws);
        children.push(ptext(&text.replace('\\', "\\\\").replace('"', "\\\"")));
        v = rest;
    }
    v = v.strip_prefix('"').unwrap_or(v);
    children.push(ptext("\""));
    Ok((rendered("bare-quoted-string", children, first_fws.unwrap_or(false)), v))
}

/// `get_atext`: a run of characters that are not ATOM_ENDS.
fn atext_len(value: &str) -> usize {
    value.find(|c| ATOM_ENDS.contains(c)).unwrap_or(value.len())
}

/// `get_atom`: [CFWS] 1*atext [CFWS].
fn get_atom(value: &str) -> Parsed<'_> {
    let mut children = Vec::new();
    let mut v = opt_cfws(value, &mut children)?;
    let n = atext_len(v);
    if n == 0 {
        return Ok(None);
    }
    children.push(Tok::value(&v[..n], "atext"));
    v = opt_cfws(&v[n..], &mut children)?;
    Ok(Some((Tok::list("atom", children), v)))
}

/// `get_dot_atom_text`: atext runs joined by single dots.
fn get_dot_atom_text(value: &str) -> Option<(Tok, &str)> {
    if first(value).is_none_or(|c| ATOM_ENDS.contains(c)) {
        return None;
    }
    let mut v = value;
    let mut children = Vec::new();
    let mut last_dot = false;
    while first(v).is_some_and(|c| !ATOM_ENDS.contains(c)) {
        let n = atext_len(v);
        children.push(Tok::value(&v[..n], "atext"));
        v = &v[n..];
        last_dot = false;
        if let Some(rest) = v.strip_prefix('.') {
            children.push(Tok::value(".", "dot"));
            v = rest;
            last_dot = true;
        }
    }
    if last_dot {
        return None;
    }
    Some((Tok::List { ttype: "dot-atom-text", children, ew_allowed: Some(true), first_fws: None }, v))
}

/// `get_dot_atom`: [CFWS] dot-atom-text [CFWS].
fn get_dot_atom(value: &str) -> Parsed<'_> {
    let mut children = Vec::new();
    let v = opt_cfws(value, &mut children)?;
    let Some((text, v)) = get_dot_atom_text(v) else { return Ok(None) };
    children.push(text);
    let v = opt_cfws(v, &mut children)?;
    Ok(Some((Tok::list("dot-atom", children), v)))
}

/// `get_quoted_string`: [CFWS] bare-quoted-string [CFWS].
fn get_quoted_string(value: &str) -> Result<(Tok, &str), Error> {
    let mut children = Vec::new();
    let v = opt_cfws(value, &mut children)?;
    let (bare, v) = get_bare_quoted_string(v)?;
    children.push(bare);
    let v = opt_cfws(v, &mut children)?;
    Ok((Tok::list("quoted-string", children), v))
}

/// Put `leader` in front of a token's children (`token[:0] = [leader]`).
fn with_leader(tok: Tok, leader: Option<Tok>) -> Tok {
    match (tok, leader) {
        (Tok::List { ttype, mut children, ew_allowed, first_fws }, Some(l)) => {
            children.insert(0, l);
            let first_fws = first_fws.map(|_| children[0].startswith_fws());
            Tok::List { ttype, children, ew_allowed, first_fws }
        }
        (tok, _) => tok,
    }
}

/// `get_word`: an atom or a quoted string, either after CFWS.
fn get_word(value: &str) -> Parsed<'_> {
    let (leader, v) = if starts_cfws(value) {
        let (l, r) = get_cfws(value)?;
        (Some(l), r)
    } else {
        (None, value)
    };
    if v.is_empty() {
        return Ok(None);
    }
    if v.starts_with('"') {
        let (tok, rest) = get_quoted_string(v)?;
        return Ok(Some((with_leader(tok, leader), rest)));
    }
    if starts_in(v, SPECIALS) {
        return Ok(None);
    }
    Ok(get_atom(v)?.map(|(tok, rest)| (with_leader(tok, leader), rest)))
}

/// `get_obs_local_part`: words, dots, CFWS and stray backslashes up to a phrase end.
fn get_obs_local_part(value: &str) -> Parsed<'_> {
    let mut v = value;
    let mut children = Vec::new();
    while let Some(c) = first(v) {
        if c != '\\' && PHRASE_ENDS.contains(c) {
            break;
        }
        if c == '.' {
            children.push(Tok::value(".", "dot"));
            v = &v[1..];
            continue;
        }
        if c == '\\' {
            children.push(Tok::value("\\", "misplaced-special"));
            v = &v[1..];
            continue;
        }
        match get_word(v)? {
            Some((tok, rest)) => {
                children.push(tok);
                v = rest;
            }
            None if starts_cfws(v) => {
                let (tok, rest) = get_cfws(v)?;
                children.push(tok);
                v = rest;
            }
            None => return Ok(None),
        }
    }
    if children.is_empty() {
        return Ok(None);
    }
    Ok(Some((Tok::list_no_ew("obs-local-part", children), v)))
}

/// `get_dtext`.
fn get_dtext(value: &str) -> Result<(Tok, &str), Error> {
    let (text, rest) = ptext_to_endchars(value, "[]")?;
    Ok((ptext(&text), rest))
}

/// `get_no_fold_literal`: "[" dtext "]".
fn get_no_fold_literal(value: &str) -> Parsed<'_> {
    let Some(v) = value.strip_prefix('[') else { return Ok(None) };
    let (dtext, v) = get_dtext(v)?;
    let Some(v) = v.strip_prefix(']') else { return Ok(None) };
    let children = vec![Tok::value("[", "no-fold-literal-start"), dtext, Tok::value("]", "no-fold-literal-end")];
    Ok(Some((Tok::list_no_ew("no-fold-literal", children), v)))
}

/// `get_domain_literal`: [CFWS] "[" [FWS] dtext [FWS] "]" [CFWS], closed for it where the value ends.
fn get_domain_literal(value: &str) -> Parsed<'_> {
    let mut children = Vec::new();
    let v = opt_cfws(value, &mut children)?;
    let Some(mut v) = v.strip_prefix('[') else { return Ok(None) };
    children.push(Tok::value("[", "domain-literal-start"));
    let close = |mut children: Vec<Tok>, v| {
        children.push(Tok::value("]", "domain-literal-end"));
        Ok(Some((Tok::list_no_ew("domain-literal", children), v)))
    };
    if v.is_empty() {
        return close(children, v);
    }
    if v.starts_with(is_wsp) {
        let (t, r) = fws_tok(v);
        children.push(t);
        v = r;
    }
    let (dtext, rest) = get_dtext(v)?;
    children.push(dtext);
    v = rest;
    if v.is_empty() {
        return close(children, v);
    }
    if v.starts_with(is_wsp) {
        let (t, r) = fws_tok(v);
        children.push(t);
        v = r;
    }
    if v.is_empty() {
        return close(children, v);
    }
    let Some(v) = v.strip_prefix(']') else { return Ok(None) };
    children.push(Tok::value("]", "domain-literal-end"));
    let v = opt_cfws(v, &mut children)?;
    Ok(Some((Tok::list_no_ew("domain-literal", children), v)))
}

/// `get_domain`: a dot-atom, a domain literal, or atoms joined by dots (obs-domain).
fn get_domain(value: &str) -> Parsed<'_> {
    let (leader, v) = if starts_cfws(value) {
        let (l, r) = get_cfws(value)?;
        (Some(l), r)
    } else {
        (None, value)
    };
    if v.is_empty() {
        return Ok(None);
    }
    if v.starts_with('[') {
        let Some((tok, rest)) = get_domain_literal(v)? else { return Ok(None) };
        return Ok(Some((Tok::list_no_ew("domain", vec![with_leader(tok, leader)]), rest)));
    }
    let Some((tok, mut v)) = (match get_dot_atom(v)? {
        Some(found) => Some(found),
        None => get_atom(v)?,
    }) else {
        return Ok(None);
    };
    if v.starts_with('@') {
        return Ok(None);
    }
    let tok = with_leader(tok, leader);
    let mut children = vec![tok];
    if v.starts_with('.') {
        if let Some(Tok::List { ttype: "dot-atom", children: inner, .. }) = children.first().cloned() {
            children = inner;
        }
        while let Some(rest) = v.strip_prefix('.') {
            children.push(Tok::value(".", "dot"));
            let Some((atom, rest)) = get_atom(rest)? else { return Ok(None) };
            children.push(atom);
            v = rest;
        }
    }
    Ok(Some((Tok::list_no_ew("domain", children), v)))
}

/// `get_msg_id`: [CFWS] "<" id-left "@" id-right ">" [CFWS]. A missing ">" is written anyway, as the email
/// package does.
fn get_msg_id(value: &str) -> Parsed<'_> {
    let mut children = Vec::new();
    let v = opt_cfws(value, &mut children)?;
    let Some(v) = v.strip_prefix('<') else { return Ok(None) };
    children.push(Tok::value("<", "msg-id-start"));
    let left = match get_dot_atom_text(v) {
        Some(found) => Some(found),
        None => get_obs_local_part(v)?,
    };
    let Some((left, mut v)) = left else { return Ok(None) };
    children.push(left);
    if !v.starts_with('@') {
        if let Some(rest) = v.strip_prefix('>') {
            children.push(Tok::value(">", "msg-id-end"));
            v = rest;
        }
        return Ok(Some((Tok::list_no_ew("msg-id", children), v)));
    }
    children.push(Tok::value("@", "address-at-symbol"));
    let v = &v[1..];
    let right = match get_dot_atom_text(v) {
        Some(found) => Some(found),
        None => match get_no_fold_literal(v)? {
            Some(found) => Some(found),
            None => get_domain(v)?,
        },
    };
    let Some((right, v)) = right else { return Ok(None) };
    children.push(right);
    let v = v.strip_prefix('>').unwrap_or(v);
    children.push(Tok::value(">", "msg-id-end"));
    let v = opt_cfws(v, &mut children)?;
    Ok(Some((Tok::list_no_ew("msg-id", children), v)))
}

/// `parse_message_ids(value)`: the msg-ids in order, a comma read as a blank, and the rest from the first
/// thing that is not a msg-id kept as unstructured text that is never encoded.
fn message_ids(value: &str) -> Result<Tok, Error> {
    let mut children = Vec::new();
    let mut v = value;
    while !v.is_empty() {
        if let Some(rest) = v.strip_prefix(',') {
            children.push(Tok::fws(" "));
            v = rest;
            continue;
        }
        match get_msg_id(v)? {
            Some((tok, rest)) => {
                children.push(tok);
                v = rest;
            }
            None => {
                children.push(Tok::list_no_ew("invalid-message-id", unstructured_parts(v)));
                break;
            }
        }
    }
    Ok(Tok::list("message-id-list", children))
}

// ---- RFC 2047 encoded words ----------------------------------------------------------------------------------

fn q_safe(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-!*+/".contains(&b)
}

/// `email._encoded_words.encode(string, charset)`: q unless b is shorter by five characters or more.
fn ew_encode(s: &str, charset: &str) -> String {
    let bytes = s.as_bytes();
    let qlen: usize = bytes.iter().map(|&b| if q_safe(b) || b == b' ' { 1 } else { 3 }).sum();
    let blen = bytes.len() / 3 * 4 + if bytes.len() % 3 != 0 { 4 } else { 0 };
    if (qlen as isize) - (blen as isize) < 5 {
        let mut enc = String::with_capacity(qlen);
        for &b in bytes {
            if b == b' ' {
                enc.push('_');
            } else if q_safe(b) {
                enc.push(b as char);
            } else {
                enc.push_str(&format!("={b:02X}"));
            }
        }
        format!("=?{charset}?q?{enc}?=")
    } else {
        format!("=?{charset}?b?{}?=", base64::engine::general_purpose::STANDARD.encode(bytes))
    }
}

/// One token of `get_unstructured`'s list.
#[derive(Debug)]
enum UTok {
    /// A run of blanks.
    Fws(String),
    /// The blank between two encoded words, which reads as nothing (`EWWhiteSpaceTerminal`).
    EwWs,
    /// An encoded word: its decoded text as (is-blank, text) pieces.
    Ew(Vec<(bool, String)>),
    Vtext(String),
}

impl UTok {
    fn text(&self) -> String {
        match self {
            UTok::Fws(t) | UTok::Vtext(t) => t.clone(),
            UTok::EwWs => String::new(),
            UTok::Ew(children) => children.iter().map(|(_, t)| t.as_str()).collect(),
        }
    }
}

/// `get_fws`: the leading run `str.lstrip()` removes, and the rest.
fn get_fws(value: &str) -> (String, &str) {
    let rest = value.trim_start_matches(is_space);
    (value[..value.len() - rest.len()].to_string(), rest)
}

/// `_wsp_splitter(value, 1)` joined back: the text up to the first blank, and the rest from that blank on.
fn wsp_split(value: &str) -> (&str, &str) {
    let end = value.find(is_wsp).unwrap_or(value.len());
    (&value[..end], &value[end..])
}

/// `_encoded_words.decode_b`: strict with the padding mended, else the lenient decoder, else the raw bytes.
fn decode_b(encoded: &[u8]) -> Vec<u8> {
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    let strict = GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        GeneralPurposeConfig::new()
            .with_decode_allow_trailing_bits(true)
            .with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );
    let pad_err = encoded.len() % 4;
    let mut padded = encoded.to_vec();
    if pad_err != 0 {
        padded.extend_from_slice(&b"==="[..4 - pad_err]);
    }
    if let Ok(v) = strict.decode(&padded) {
        return v;
    }
    // validate=False reads the standard alphabet only: the url-safe characters are skipped like any other junk
    let lenient = |b: &[u8]| -> Option<Vec<u8>> {
        let text: String = b.iter().map(|&c| if c == b'-' || c == b'_' { '!' } else { c as char }).collect();
        crate::py::b64decode_urlsafe(&text)
    };
    if let Some(v) = lenient(encoded) {
        return v;
    }
    let mut more = encoded.to_vec();
    more.extend_from_slice(b"==");
    lenient(&more).unwrap_or_else(|| encoded.to_vec())
}

/// `_encoded_words.decode_q`: `_` is a blank, `=XX` a byte.
fn decode_q(encoded: &[u8]) -> Vec<u8> {
    let b: Vec<u8> = encoded.iter().map(|&c| if c == b'_' { b' ' } else { c }).collect();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'=' && i + 2 < b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() {
            let hex = [b[i + 1], b[i + 2]];
            out.push(u8::from_str_radix(std::str::from_utf8(&hex).unwrap_or("00"), 16).unwrap_or(0));
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// `_encoded_words.decode(ew)`: None where it raises ValueError or KeyError (not five `?` fields, text that
/// is not ASCII, a transfer encoding other than q or b).
fn ew_decode(word: &str) -> Option<String> {
    let fields: Vec<&str> = word.split('?').collect();
    let [_, charset, cte, cte_string, _] = fields[..] else { return None };
    let charset = charset.split('*').next().unwrap_or("");
    if !cte_string.is_ascii() {
        return None;
    }
    let bytes = match cte.to_ascii_lowercase().as_str() {
        "q" => decode_q(cte_string.as_bytes()),
        "b" => decode_b(cte_string.as_bytes()),
        _ => return None,
    };
    let name = charset.to_ascii_lowercase().replace('_', "-");
    Some(match name.as_str() {
        "latin-1" | "latin1" | "iso-8859-1" | "iso8859-1" | "l1" => bytes.iter().map(|&b| b as char).collect(),
        // utf-8 and its aliases; ascii and an unknown charset read the ASCII bytes as they are (Python keeps
        // the others as surrogate escapes, which a Rust string cannot hold: they become U+FFFD)
        _ => String::from_utf8_lossy(&bytes).into_owned(),
    })
}

static RFC2047: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"=\?[^?]*\?[qQbB]\?.*?\?=").expect("valid regex"));

enum EwErr {
    /// `_InvalidEwError`: shaped like an encoded word but not decodable.
    Invalid,
    /// `HeaderParseError`: no `?=` at all.
    Parse,
}

/// `get_encoded_word(value, 'utext')`: the word at the start of `value` and what follows it.
fn get_encoded_word(value: &str) -> Result<(UTok, &str), EwErr> {
    let body = &value[2..];
    let Some((tok, remstr)) = body.split_once("?=") else { return Err(EwErr::Parse) };
    let mut tok = tok.to_string();
    let mut rest = remstr;
    let hex = |c: Option<char>| c.is_some_and(|c| c.is_ascii_hexdigit());
    let mut rc = remstr.chars();
    if remstr.chars().count() > 1 && hex(rc.next()) && hex(rc.next()) && tok.matches('?').count() < 2 {
        // the ? after the transfer encoding was followed by an =XX escape
        let (more, after) = remstr.split_once("?=").unwrap_or((remstr, ""));
        tok = format!("{tok}?={more}");
        rest = after;
    }
    let text = ew_decode(&format!("=?{tok}?=")).ok_or(EwErr::Invalid)?;
    let mut children = Vec::new();
    let mut t = text.as_str();
    while !t.is_empty() {
        if t.starts_with(is_wsp) {
            let (ws, after) = get_fws(t);
            children.push((true, ws));
            t = after;
            continue;
        }
        let (word, after) = wsp_split(t);
        children.push((false, word.to_string()));
        t = after;
    }
    Ok((UTok::Ew(children), rest))
}

/// `get_unstructured(value)`.
fn get_unstructured(value: &str) -> Vec<UTok> {
    let mut out: Vec<UTok> = Vec::new();
    let mut value = value;
    while !value.is_empty() {
        if value.starts_with(is_wsp) {
            let (ws, rest) = get_fws(value);
            out.push(UTok::Fws(ws));
            value = rest;
            continue;
        }
        let mut valid_ew = true;
        if value.starts_with("=?") {
            match get_encoded_word(value) {
                Ok((ew, rest)) => {
                    let mut have_ws = true;
                    if !out.is_empty() && !matches!(out.last(), Some(UTok::Fws(_) | UTok::EwWs)) {
                        have_ws = false;
                    }
                    let n = out.len();
                    if have_ws && n > 1 && matches!(out[n - 2], UTok::Ew(_)) {
                        out[n - 1] = UTok::EwWs;
                    }
                    out.push(ew);
                    value = rest;
                    continue;
                }
                Err(EwErr::Invalid) => valid_ew = false,
                Err(EwErr::Parse) => {}
            }
        }
        let (mut tok, mut rest) = wsp_split(value);
        if valid_ew && RFC2047.is_match(tok) {
            // an encoded word inside an atom is split off, to be read on the next round
            // (never at the start: there get_encoded_word has read it or refused it as invalid)
            let at = value.find("=?").unwrap_or(value.len());
            if at > 0 {
                tok = &value[..at];
                rest = &value[at..];
            }
        }
        out.push(UTok::Vtext(tok.to_string()));
        value = rest;
    }
    out
}

/// `str(get_unstructured(value))`: the value with its encoded words read back, and the blank between two
/// encoded words dropped, as RFC 2047 says a reader does.
fn unstructured_str(value: &str) -> String {
    get_unstructured(value).iter().map(UTok::text).collect()
}

// ---- the folder: email._header_value_parser._refold_parse_tree -------------------------------------------

fn steal_trailing_wsp(lines: &mut Vec<String>) -> String {
    let mut wsp = String::new();
    if let Some(last) = lines.last_mut() {
        if let Some(c) = last.chars().last().filter(|&c| is_wsp(c)) {
            last.pop();
            wsp.push(c);
            if last.is_empty() {
                lines.pop();
            }
        }
    }
    wsp
}

fn last(lines: &mut Vec<String>) -> &mut String {
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines.last_mut().expect("lines is not empty")
}

fn fold_as_ew(
    to_encode: &str,
    lines: &mut Vec<String>,
    last_ew: Option<usize>,
    charset: &str,
    last_word_is_ew: bool,
) -> Result<Option<usize>, Error> {
    // `to_encode[0]` and `to_encode[-1]` on an empty word: what CPython raises
    let index_error = || Error::Other { name: "IndexError", msg: "string index out of range".into() };
    let mut to_encode = to_encode.to_string();
    if let Some(le) = last_ew {
        let line = last(lines);
        let at = crate::py::char_to_byte(line, le);
        to_encode = unstructured_str(&format!("{}{}", &line[at..], to_encode));
        line.truncate(at);
    } else if last_word_is_ew {
        let line = last(lines);
        let kept = line.trim_end_matches(is_wsp).len();
        let leading = line[kept..].to_string();
        line.truncate(kept);
        if !leading.is_empty() {
            line.push(' ');
        }
        to_encode = leading + &to_encode;
    } else if to_encode.is_empty() {
        return Err(index_error());
    } else if to_encode.starts_with(is_wsp) {
        let lead = to_encode.remove(0);
        if clen(last(lines)) == MAXLEN as isize {
            let wsp = steal_trailing_wsp(lines);
            lines.push(wsp);
        }
        last(lines).push(lead);
    }
    let mut trailing = String::new();
    if to_encode.is_empty() {
        return Err(index_error());
    }
    if to_encode.ends_with(is_wsp) {
        trailing.push(to_encode.pop().expect("ends with a blank"));
    }
    let mut new_last_ew = match last_ew {
        None => clen(last(lines)) as usize,
        Some(le) => le,
    };
    let encode_as = if charset == "us-ascii" { "utf-8" } else { charset };
    let chrome = (encode_as.len() + 7) as isize;
    while !to_encode.is_empty() {
        let remaining = MAXLEN as isize - clen(last(lines));
        let text_space = remaining - chrome;
        if text_space <= 0 {
            let wsp = steal_trailing_wsp(lines);
            lines.push(if wsp.is_empty() { " ".into() } else { wsp });
            new_last_ew = clen(last(lines)) as usize;
            continue;
        }
        let mut word = crate::py::take_chars(&to_encode, text_space as usize).to_string();
        let mut encoded = ew_encode(&word, encode_as);
        while clen(&encoded) - remaining > 0 {
            word.pop();
            encoded = ew_encode(&word, encode_as);
        }
        last(lines).push_str(&encoded);
        to_encode = to_encode[word.len()..].to_string();
        if !to_encode.is_empty() {
            lines.push(" ".into());
            new_last_ew = clen(last(lines)) as usize;
        }
    }
    last(lines).push_str(&trailing);
    Ok(Some(new_last_ew))
}

fn refold(tree: Vec<Tok>) -> Result<String, Error> {
    let maxlen = MAXLEN as isize;
    let mut lines: Vec<String> = vec![String::new()];
    let mut last_word_is_ew = false;
    let mut last_ew: Option<usize> = None;
    let mut last_charset: Option<&'static str> = None;
    let mut blocked = 0usize;
    let mut want_encoding = false;
    let mut parts: VecDeque<Tok> = tree.into();
    while let Some(part) = parts.pop_front() {
        if matches!(part, Tok::EndBlock) {
            blocked = blocked.saturating_sub(1);
            continue;
        }
        let tstr = part.text();
        if !want_encoding {
            want_encoding = if matches!(part.ttype(), "ptext" | "vtext") {
                tstr.chars().any(|c| "()<>@,:;.\\\"[]\r\n".contains(c))
            } else {
                tstr.contains(['\r', '\n'])
            };
        }
        let charset: &'static str = if tstr.is_ascii() {
            "us-ascii"
        } else {
            want_encoding = true;
            "utf-8"
        };
        if want_encoding && blocked == 0 {
            if !part.as_ew_allowed() {
                want_encoding = false;
                last_ew = None;
                if part.syntactic_break() {
                    if let Tok::List { children, ttype, .. } = &part {
                        // a msg-id folds as itself (MsgID.fold): it may not be broken
                        let folded = if matches!(*ttype, "msg-id" | "invalid-message-id") {
                            format!("{}{LINESEP}", part.text())
                        } else {
                            refold(children.clone())?
                        };
                        let encoded = &folded[..folded.len() - LINESEP.len()];
                        if !encoded.contains(LINESEP) {
                            if clen(encoded) > maxlen - clen(last(&mut lines)) {
                                let wsp = steal_trailing_wsp(&mut lines);
                                lines.push(wsp);
                            }
                            last(&mut lines).push_str(encoded);
                            last_word_is_ew = false;
                            continue;
                        }
                    }
                }
            }
            if let Tok::List { children, .. } = &part {
                for c in children.iter().rev() {
                    parts.push_front(c.clone());
                }
                want_encoding = false;
                continue;
            } else if part.as_ew_allowed() {
                if last_ew.is_some()
                    && Some(charset) != last_charset
                    && (last_charset == Some("unknown-8bit") || (last_charset == Some("utf-8") && charset != "us-ascii"))
                {
                    last_ew = None;
                }
                last_ew = fold_as_ew(&tstr, &mut lines, last_ew, charset, last_word_is_ew)?;
                last_word_is_ew = true;
                last_charset = Some(charset);
                want_encoding = false;
                continue;
            } else {
                last_ew = None;
                want_encoding = false;
            }
        }
        if clen(&tstr) <= maxlen - clen(last(&mut lines)) {
            last(&mut lines).push_str(&tstr);
            last_word_is_ew = last_word_is_ew && tstr.trim_matches(is_wsp).is_empty();
            continue;
        }
        if part.syntactic_break() && clen(&tstr) < maxlen {
            let wsp = steal_trailing_wsp(&mut lines);
            if !wsp.is_empty() || part.startswith_fws() {
                lines.push(wsp + &tstr);
                last_word_is_ew = last_word_is_ew && last(&mut lines).trim_matches(is_wsp).is_empty();
                last_ew = None;
                continue;
            }
        }
        if let Tok::List { children, .. } = &part {
            let mut newparts: Vec<Tok> = children.clone();
            if !part.as_ew_allowed() {
                blocked += 1;
                newparts.push(Tok::EndBlock);
            }
            for c in newparts.into_iter().rev() {
                parts.push_front(c);
            }
            continue;
        }
        if part.as_ew_allowed() && blocked == 0 {
            parts.push_front(part);
            want_encoding = true;
            continue;
        }
        let wsp = steal_trailing_wsp(&mut lines);
        if !wsp.is_empty() || part.startswith_fws() {
            lines.push(wsp + &tstr);
        } else {
            last(&mut lines).push_str(&tstr);
        }
        last_word_is_ew = last_word_is_ew && tstr.trim_matches(is_wsp).is_empty();
    }
    Ok(lines.join(LINESEP) + LINESEP)
}

/// One header line (or folded lines), ending CRLF, as `BaseHeader.fold` writes it.
fn fold_header(name: &str, value: Tok) -> Result<String, Error> {
    let label = Tok::list_no_ew("header-label", vec![Tok::value(name, "header-name"), Tok::value(":", "header-sep")]);
    let mut tree = vec![label];
    let has_value = matches!(&value, Tok::List { children, .. } if !children.is_empty());
    if has_value {
        tree.push(Tok::list("cfws", vec![Tok::fws(" ")]));
    }
    tree.push(value);
    // `_refold_parse_tree` walks `list(parse_tree)`: the Header's parts, never the Header itself
    refold(tree)
}

/// The generator's `verify_generated_headers` check: a folded header whose next line does not start with a
/// blank would start a new header, so it is refused (HeaderWriteError) rather than written.
fn verify_generated(folded: &str) -> Result<(), Error> {
    let body = folded.strip_suffix(LINESEP).unwrap_or(folded);
    let b = body.as_bytes();
    let bad = (0..b.len()).any(|i| match b[i] {
        // NEWLINE_WITHOUT_FWSP: \r\n[^ \t] | \r[^ \n\t] | \n[^ \t]
        b'\r' => matches!(b.get(i + 1), Some(c) if !matches!(c, b' ' | b'\n' | b'\t')),
        b'\n' => matches!(b.get(i + 1), Some(c) if !matches!(c, b' ' | b'\t')),
        _ => false,
    });
    if bad {
        return Err(Error::HeaderWrite(format!(
            "folded header contains newline: {}",
            crate::py::repr_bytes(folded.as_bytes())
        )));
    }
    Ok(())
}

/// `fold_binary`'s `folded.encode("ascii")`: a folded header that still holds non-ASCII text (a msg-id is never
/// encoded) is a UnicodeEncodeError naming the first run of such characters.
fn ascii_only(folded: &str) -> Result<(), Error> {
    let chars: Vec<char> = folded.chars().collect();
    let Some(start) = chars.iter().position(|c| !c.is_ascii()) else { return Ok(()) };
    let end = start + chars[start..].iter().take_while(|c| !c.is_ascii()).count();
    let msg = if end - start == 1 {
        let u = chars[start] as u32;
        let shown = if u < 0x100 {
            format!("\\x{u:02x}")
        } else if u < 0x10000 {
            format!("\\u{u:04x}")
        } else {
            format!("\\U{u:08x}")
        };
        format!("'ascii' codec can't encode character '{shown}' in position {start}: ordinal not in range(128)")
    } else {
        format!("'ascii' codec can't encode characters in position {start}-{}: ordinal not in range(128)", end - 1)
    };
    Err(Error::Value { name: "UnicodeEncodeError", msg })
}

/// The SMTP policy's check on a header value: one line by `str.splitlines`.
fn one_line_header(value: &str) -> Result<(), Error> {
    if splitlines(value).len() > 1 {
        return Err(Error::value("Header values may not contain linefeed or carriage return characters"));
    }
    Ok(())
}

// ---- the body: email.contentmanager._encode_text ----------------------------------------------------------

/// `bytes.splitlines()`: \n, \r and \r\n end a line.
fn byte_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < data.len() {
        match data[i] {
            b'\n' => {
                out.push(&data[start..i]);
                start = i + 1;
            }
            b'\r' => {
                out.push(&data[start..i]);
                if data.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start < data.len() {
        out.push(&data[start..]);
    }
    out
}

fn qp_body_safe(b: u8) -> bool {
    (b' '..=b'~').contains(&b) && b != b'=' || b == b'\t'
}

/// `quoprimime.body_encode(body, maxlinelen)` over bytes read as latin-1, lines joined with "\n".
fn qp_body_encode(body: &[u8], maxlinelen: usize) -> String {
    if body.is_empty() {
        return String::new();
    }
    let mut translated = String::with_capacity(body.len());
    for &b in body {
        if qp_body_safe(b) || b == b'\r' || b == b'\n' {
            translated.push(b as char);
        } else {
            translated.push_str(&format!("={b:02X}"));
        }
    }
    let soft_break = "=\n";
    let maxlinelen1 = maxlinelen - 1;
    let mut encoded: Vec<String> = Vec::new();
    for line in splitlines(&translated) {
        let l = line.as_bytes();
        let mut start: isize = 0;
        let laststart = l.len() as isize - 1 - maxlinelen as isize;
        while start <= laststart {
            let stop = (start + maxlinelen1 as isize) as usize;
            let s = start as usize;
            if l[stop - 2] == b'=' {
                encoded.push(line[s..stop - 1].to_string());
                start = stop as isize - 2;
            } else if l[stop - 1] == b'=' {
                encoded.push(line[s..stop].to_string());
                start = stop as isize - 1;
            } else {
                encoded.push(format!("{}=", &line[s..stop]));
                start = stop as isize;
            }
        }
        let s = start as usize;
        match l.last() {
            Some(&c) if c == b' ' || c == b'\t' => {
                let room = start - laststart;
                let quoted = format!("={c:02X}");
                let q = if room >= 3 {
                    quoted
                } else if room == 2 {
                    format!("{}{soft_break}", c as char)
                } else {
                    format!("{soft_break}{quoted}")
                };
                encoded.push(format!("{}{q}", &line[s..l.len() - 1]));
            }
            _ => encoded.push(line[s..].to_string()),
        }
    }
    if matches!(body.last(), Some(b'\r') | Some(b'\n')) {
        encoded.push(String::new());
    }
    encoded.join("\n")
}

fn b2a_base64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data) + "\n"
}

/// `_encode_text(body, "utf-8", None, SMTP)`: (cte, payload with "\n" line ends).
fn encode_text(body: &str) -> (&'static str, Vec<u8>) {
    let data = body.as_bytes();
    let lines = byte_lines(data);
    let embedded = |ls: &[&[u8]]| -> Vec<u8> {
        let mut v = ls.join(&b"\r\n"[..]);
        v.extend_from_slice(b"\r\n");
        v
    };
    let normal = |ls: &[&[u8]]| -> Vec<u8> {
        let mut v = ls.join(&b"\n"[..]);
        v.push(b'\n');
        v
    };
    if lines.iter().map(|l| l.len()).max().unwrap_or(0) <= MAXLEN {
        let n = normal(&lines);
        return (if n.is_ascii() { "7bit" } else { "8bit" }, n);
    }
    let sniff = embedded(&lines[..lines.len().min(10)]);
    let sniff_qp = qp_body_encode(&sniff, MAXLEN);
    if sniff_qp.len() > b2a_base64(&sniff).len() {
        let body = embedded(&lines);
        let mut out = String::new();
        for chunk in body.chunks(MAXLEN / 4 * 3) {
            out.push_str(&b2a_base64(chunk));
        }
        ("base64", out.into_bytes())
    } else if lines.len() <= 10 {
        ("quoted-printable", sniff_qp.into_bytes())
    } else {
        ("quoted-printable", qp_body_encode(&normal(&lines), MAXLEN).into_bytes())
    }
}

/// The message bytes `EmailMessage(policy=SMTP).as_bytes()` gives for these headers and this text body.
pub fn message_bytes(
    to: &[String],
    cc: &[String],
    bcc: &[String],
    subject: &str,
    body: &str,
    reply_headers: &[(String, String)],
) -> Result<Vec<u8>, Error> {
    // Each header is checked and parsed when it is set (`msg[k] = v`), in order, before anything is written:
    // the policy refuses a value with a line break, and the parser's own failures surface there.
    let mut headers: Vec<(&str, Tok)> = vec![("To", address_list(to))];
    if !cc.is_empty() {
        headers.push(("Cc", address_list(cc)));
    }
    if !bcc.is_empty() {
        headers.push(("Bcc", address_list(bcc)));
    }
    one_line_header(subject)?;
    headers.push(("Subject", unstructured(subject)));
    for (k, v) in reply_headers {
        if !v.is_empty() {
            one_line_header(v)?;
            // the header registry reads In-Reply-To and References as msg-id lists, any other as text
            let value = if matches!(k.to_ascii_lowercase().as_str(), "in-reply-to" | "references") {
                message_ids(v)?
            } else {
                unstructured(v)
            };
            headers.push((k.as_str(), value));
        }
    }
    let mut out = String::new();
    for (name, value) in headers {
        let folded = fold_header(name, value)?;
        ascii_only(&folded)?;
        verify_generated(&folded)?;
        out.push_str(&folded);
    }
    let (cte, payload) = encode_text(body);
    out.push_str("Content-Type: text/plain; charset=\"utf-8\"\r\n");
    out.push_str(&format!("Content-Transfer-Encoding: {cte}\r\n"));
    out.push_str("MIME-Version: 1.0\r\n\r\n");
    let mut bytes = out.into_bytes();
    for (i, line) in payload.split(|&b| b == b'\n').enumerate() {
        if i > 0 {
            bytes.extend_from_slice(b"\r\n");
        }
        bytes.extend_from_slice(line);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_words_round_trip() {
        for s in ["Café", "Ça va? Très", "日本語 world", "x y", "😀"] {
            let ew = ew_encode(s, "utf-8");
            assert_eq!(ew_decode(&ew).as_deref(), Some(s), "{ew}");
        }
        assert_eq!(ew_encode("Café", "utf-8"), "=?utf-8?q?Caf=C3=A9?=");
        assert_eq!(unstructured_str("=?utf-8?q?a?= =?utf-8?q?b?= c"), "ab c");
    }

    #[test]
    fn a_short_ascii_message_is_7bit() {
        let raw = message_bytes(&["a@b.co".into()], &[], &[], "Hi", "Hello\nthere", &[]).unwrap();
        assert_eq!(
            String::from_utf8(raw).unwrap(),
            "To: a@b.co\r\nSubject: Hi\r\nContent-Type: text/plain; charset=\"utf-8\"\r\n\
             Content-Transfer-Encoding: 7bit\r\nMIME-Version: 1.0\r\n\r\nHello\r\nthere\r\n"
        );
    }

    #[test]
    fn a_header_with_another_line_is_refused() {
        let err = message_bytes(&["a@b.co".into()], &[], &[], "a\u{2028}b", "x", &[]).unwrap_err();
        assert_eq!(err.to_string(), "Header values may not contain linefeed or carriage return characters");
    }
}
