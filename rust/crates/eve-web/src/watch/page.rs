//! watch.py's "a reading and reading pages" section: what one check found, and the page's own structured data
//! (JSON-LD offers, price meta tags, microdata), read with no model.
//!
//! Three pieces of the Python standard library this section leans on are reproduced here, privately, because
//! their exact behaviour decides what a page reads as: `html.unescape` (the HTML5 entity table, the legacy names
//! that need no semicolon, the longest-prefix rule), `json` with `strict=False` (raw control characters inside
//! strings, NaN and Infinity, integers of any size, `raw_decode`), and `html.parser.HTMLParser` as Python 3.13
//! tokenizes (the microdata reader is a subclass of it).

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use super::{AVAILABLE, MAX_PRICE, UNAVAILABLE};

/// What one check found. `value` is a price; `available` whether it can be bought or booked now; `hit` whether
/// the watched phrase is on the page. Any may be None: not known.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    pub value: Option<f64>,
    pub currency: String,
    pub available: Option<bool>,
    pub hit: Option<bool>,
    pub note: String,
    pub url: String,
    /// "quote", "structured", "model", "search", "ticketmaster", "text", "vision", "browser", "hosted"
    pub source: String,
    pub name: String,
    /// the vision model saw a bot check, not the page
    pub blocked: bool,
}

/// ISO 4217 currencies with 3 minor digits.
pub const THREE_DECIMALS: [&str; 7] = ["KWD", "BHD", "OMR", "JOD", "TND", "LYD", "IQD"];
/// `_NUMBER` (the class holds a no-break space U+00A0, a narrow no-break space U+202F, an apostrophe and a space).
pub const NUMBER: &str = "\\d[\\d.,\u{a0}\u{202f}' ]*\\d|\\d";
/// `_SIGNED`.
pub const SIGNED: &str = "[$€£¥₹₩₺₽]\\s*(\\d[\\d.,\u{a0}\u{202f}' ]*\\d|\\d)";
/// type="application/ld+json", unquoted as a minifier leaves it, or with parameters ("…; charset=utf-8")
pub const LD_TYPE: &str = r#"(?i)type\s*=\s*["']?\s*application/ld\+json\b"#;
pub const META_RE: &str = r"(?i)<meta\b[^>]*>";
pub const ATTR_RE: &str = r#"([a-zA-Z_:.-]+)\s*=\s*("[^"]*"|'[^']*'|[^\s>]+)"#;
/// Lines that only frame a script: comments, CDATA markers, HTML comment markers (after extruct's open PR #258).
pub const FRAMING_RE: &str = r"//|/\*|\*/|<!--|--!?>|<!\[CDATA\[|\]\]>|\s+";
pub const VOID: [&str; 14] =
    ["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"];
pub const MICRODATA_PROPS: [&str; 4] = ["price", "lowPrice", "priceCurrency", "availability"];
pub const OFFER_SCOPES: [&str; 7] = ["", "Offer", "AggregateOffer", "Product", "IndividualProduct", "ProductModel", "Event"];
/// hasVariant is walked: a ProductGroup's offers are its variants'
pub const NOT_THE_ITEM: [&str; 6] =
    ["isRelatedTo", "isSimilarTo", "isAccessoryOrSparePartFor", "isConsumableFor", "review", "isVariantOf"];
pub const ITEM_TYPES: &str = r"(?i)Product|Event|Book|Ticket|Vehicle|Game|Album|Movie|SoftwareApplication";
/// `_MAIN_OPEN` (a lookahead: fancy-regex).
pub const MAIN_OPEN: &str = r"(?i)<main(?=[\s>/])";

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a valid pattern")
}

// Python's `\s` is str.isspace, which also holds the information separators U+001C..U+001F.
static SIGNED_RE: LazyLock<Regex> =
    LazyLock::new(|| re("[$€£¥₹₩₺₽][\\s\\x1c-\\x1f]*(\\d[\\d.,\u{a0}\u{202f}' ]*\\d|\\d)"));
static NUMBER_RE: LazyLock<Regex> = LazyLock::new(|| re(NUMBER));
static DIGIT_RE: LazyLock<Regex> = LazyLock::new(|| re(r"^\d$"));
static LD_TYPE_RE: LazyLock<Regex> = LazyLock::new(|| re(r#"(?i)type[\s\x1c-\x1f]*=[\s\x1c-\x1f]*["']?[\s\x1c-\x1f]*application/ld\+json\b"#));
static META_TAG_RE: LazyLock<Regex> = LazyLock::new(|| re(META_RE));
static ATTR_PAIR_RE: LazyLock<Regex> =
    LazyLock::new(|| re(r#"([a-zA-Z_:.-]+)[\s\x1c-\x1f]*=[\s\x1c-\x1f]*("[^"]*"|'[^']*'|[^\s\x1c-\x1f>]+)"#));
static FRAMING: LazyLock<Regex> =
    LazyLock::new(|| re(r"//|/\*|\*/|<!--|--!?>|<!\[CDATA\[|\]\]>|[\s\x1c-\x1f]+"));
static ITEM_TYPES_RE: LazyLock<Regex> = LazyLock::new(|| re(ITEM_TYPES));
static TAG_RE: LazyLock<Regex> = LazyLock::new(|| re(r"<[^>]+>"));
static CHARREF: LazyLock<Regex> =
    LazyLock::new(|| re(r"&(#[0-9]+;?|#[xX][0-9a-fA-F]+;?|[^\t\n\x0c <&#;]{1,32};?)"));
static ATTR_CHARREF: LazyLock<Regex> = LazyLock::new(|| re(r"&(#[0-9]+|#[xX][0-9a-fA-F]+|[a-zA-Z][a-zA-Z0-9]*)[;=]?"));

// ---- Python's str helpers ---------------------------------------------------------------------------------

/// `str.isspace`: Unicode White_Space plus the information separators U+001C..U+001F.
pub(crate) fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `" ".join(s.split())`.
pub(crate) fn collapse(s: &str) -> String {
    s.split(py_isspace).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// `s.strip()`.
pub(crate) fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

/// `s[:n]` in characters.
pub(crate) fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `str.splitlines()`: \n, \r, \r\n, \v, \f, \x1c, \x1d, \x1e, \x85,  ,  .
fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let breaks = matches!(
            c,
            '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if breaks {
            out.push(&s[start..i]);
            let mut next = i + c.len_utf8();
            if c == '\r' {
                if let Some(&(j, '\n')) = chars.peek() {
                    chars.next();
                    next = j + 1;
                }
            }
            start = next;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// `repr(float)`: the shortest digits that read back, in Python's layout (`1.0`, `1e+16`, `1.5e-05`).
pub(crate) fn py_float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let sci = format!("{x:e}");
    let (mant, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let neg = mant.starts_with('-');
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let sign = if neg { "-" } else { "" };
    if !(-4..16).contains(&exp) {
        let rest = &digits[1..];
        let mantissa = if rest.is_empty() { digits[..1].to_string() } else { format!("{}.{}", &digits[..1], rest) };
        let esign = if exp < 0 { "-" } else { "+" };
        return format!("{sign}{mantissa}e{esign}{:02}", exp.abs());
    }
    let point = exp + 1;
    let body = if point <= 0 {
        format!("0.{}{}", "0".repeat((-point) as usize), digits)
    } else if point as usize >= digits.len() {
        format!("{}{}.0", digits, "0".repeat(point as usize - digits.len()))
    } else {
        format!("{}.{}", &digits[..point as usize], &digits[point as usize..])
    };
    format!("{sign}{body}")
}

fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || (0x7f..0xa0).contains(&(c as u32)) => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => py_repr_str(s),
        other => py_str(other),
    }
}

/// `str(value)` for a value `json.loads` made.
pub(crate) fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => {
            if n.is_f64() {
                py_float_repr(n.as_f64().unwrap_or(0.0))
            } else {
                n.to_string()
            }
        }
        Value::String(s) => s.clone(),
        Value::Array(items) => format!("[{}]", items.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter().map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Python's truthiness of a JSON value.
pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

/// `str(x or "")`.
fn str_or_empty(v: Option<&Value>) -> String {
    match v {
        Some(v) if truthy(v) => py_str(v),
        _ => String::new(),
    }
}

// ---- html.unescape ----------------------------------------------------------------------------------------

/// The HTML5 names that also stand without their semicolon (`html.entities.html5`'s keys with no ";").
const LEGACY_ENTITIES: [&str; 106] = [
    "AElig", "AMP", "Aacute", "Acirc", "Agrave", "Aring", "Atilde", "Auml", "COPY", "Ccedil", "ETH", "Eacute", "Ecirc",
    "Egrave", "Euml", "GT", "Iacute", "Icirc", "Igrave", "Iuml", "LT", "Ntilde", "Oacute", "Ocirc", "Ograve", "Oslash",
    "Otilde", "Ouml", "QUOT", "REG", "THORN", "Uacute", "Ucirc", "Ugrave", "Uuml", "Yacute", "aacute", "acirc", "acute",
    "aelig", "agrave", "amp", "aring", "atilde", "auml", "brvbar", "ccedil", "cedil", "cent", "copy", "curren", "deg",
    "divide", "eacute", "ecirc", "egrave", "eth", "euml", "frac12", "frac14", "frac34", "gt", "iacute", "icirc", "iexcl",
    "igrave", "iquest", "iuml", "laquo", "lt", "macr", "micro", "middot", "nbsp", "not", "ntilde", "oacute", "ocirc",
    "ograve", "ordf", "ordm", "oslash", "otilde", "ouml", "para", "plusmn", "pound", "quot", "raquo", "reg", "sect",
    "shy", "sup1", "sup2", "sup3", "szlig", "thorn", "times", "uacute", "ucirc", "ugrave", "uml", "uuml", "yacute", "yen",
    "yuml",
];

fn named_entity(name: &str) -> Option<&'static str> {
    let table = &html_escape::NAMED_ENTITIES;
    let lookup = |n: &str| table.binary_search_by(|(k, _)| (*k).cmp(n.as_bytes())).ok().map(|i| table[i].1);
    match name.strip_suffix(';') {
        Some(bare) => lookup(bare),
        None if LEGACY_ENTITIES.binary_search(&name).is_ok() => lookup(name),
        None => None,
    }
}

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

fn replace_charref(s: &str) -> String {
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
    if let Some(v) = named_entity(s) {
        return v.to_string();
    }
    let cuts: Vec<usize> = s.char_indices().map(|(i, _)| i).collect();
    for x in (2..cuts.len()).rev() {
        let cut = cuts[x];
        if let Some(v) = named_entity(&s[..cut]) {
            return format!("{v}{}", &s[cut..]);
        }
    }
    format!("&{s}")
}

/// `html.unescape(s)`.
pub(crate) fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    CHARREF.replace_all(s, |c: &regex::Captures| replace_charref(&c[1])).into_owned()
}

/// html.parser's `_unescape_attrvalue`: a numeric reference always, a named one only as an exact HTML5 name not
/// followed by "=".
fn unescape_attrvalue(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    ATTR_CHARREF
        .replace_all(s, |c: &regex::Captures| {
            let whole = &c[0];
            if whole.starts_with("&#") {
                return unescape(whole);
            }
            if !whole.ends_with('=') && named_entity(&whole[1..]).is_some() {
                return unescape(whole);
            }
            whole.to_string()
        })
        .into_owned()
}

// ---- json, strict=False -----------------------------------------------------------------------------------

/// Python's json decoder (`strict=False`): raw control characters in strings, NaN / Infinity / -Infinity,
/// integers of any size. A number no float holds (an integer past 1.8e308, `1e400`, NaN) has no JSON form, so it
/// is kept as the string "Infinity", "-Infinity" or "NaN", which no price, availability or name rule accepts.
struct Json<'a> {
    s: &'a str,
    b: &'a [u8],
}

const JSON_DEPTH: usize = 990;

fn json_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | b'\r') {
        i += 1;
    }
    i
}

fn nonfinite(x: f64) -> Value {
    if x.is_nan() {
        Value::String("NaN".into())
    } else if x > 0.0 {
        Value::String("Infinity".into())
    } else {
        Value::String("-Infinity".into())
    }
}

impl<'a> Json<'a> {
    fn new(s: &'a str) -> Self {
        Json { s, b: s.as_bytes() }
    }

    fn value(&self, i: usize, depth: usize) -> Option<(Value, usize)> {
        if depth > JSON_DEPTH {
            return None;
        }
        let b = self.b;
        let c = *b.get(i)?;
        match c {
            b'"' => self.string(i + 1).map(|(s, e)| (Value::String(s), e)),
            b'{' => self.object(i + 1, depth),
            b'[' => self.array(i + 1, depth),
            b'n' if b[i..].starts_with(b"null") => Some((Value::Null, i + 4)),
            b't' if b[i..].starts_with(b"true") => Some((Value::Bool(true), i + 4)),
            b'f' if b[i..].starts_with(b"false") => Some((Value::Bool(false), i + 5)),
            _ => {
                if let Some(r) = self.number(i) {
                    return Some(r);
                }
                if b[i..].starts_with(b"NaN") {
                    Some((nonfinite(f64::NAN), i + 3))
                } else if b[i..].starts_with(b"Infinity") {
                    Some((nonfinite(f64::INFINITY), i + 8))
                } else if b[i..].starts_with(b"-Infinity") {
                    Some((nonfinite(f64::NEG_INFINITY), i + 9))
                } else {
                    None
                }
            }
        }
    }

    fn number(&self, i: usize) -> Option<(Value, usize)> {
        let b = self.b;
        let mut j = i;
        if b.get(j) == Some(&b'-') {
            j += 1;
        }
        match b.get(j) {
            Some(b'0') => j += 1,
            Some(c) if c.is_ascii_digit() => {
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
            }
            _ => return None,
        }
        let int_end = j;
        let mut float = false;
        if b.get(j) == Some(&b'.') && b.get(j + 1).is_some_and(u8::is_ascii_digit) {
            j += 2;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            float = true;
        }
        if matches!(b.get(j), Some(b'e' | b'E')) {
            let mut k = j + 1;
            if matches!(b.get(k), Some(b'+' | b'-')) {
                k += 1;
            }
            if b.get(k).is_some_and(u8::is_ascii_digit) {
                while k < b.len() && b[k].is_ascii_digit() {
                    k += 1;
                }
                j = k;
                float = true;
            }
        }
        let text = &self.s[i..j];
        if !float {
            debug_assert_eq!(int_end, j);
            if let Ok(n) = text.parse::<i64>() {
                return Some((Value::Number(n.into()), j));
            }
            if let Ok(n) = text.parse::<u64>() {
                return Some((Value::Number(n.into()), j));
            }
            // float(int): correctly rounded, or past a float's range
            let x: f64 = text.parse().unwrap_or(f64::INFINITY);
            return Some((Number::from_f64(x).map(Value::Number).unwrap_or_else(|| nonfinite(x)), j));
        }
        let x: f64 = text.parse().ok()?;
        Some((Number::from_f64(x).map(Value::Number).unwrap_or_else(|| nonfinite(x)), j))
    }

    fn string(&self, mut i: usize) -> Option<(String, usize)> {
        let b = self.b;
        let mut out = String::new();
        loop {
            let start = i;
            while i < b.len() && b[i] != b'"' && b[i] != b'\\' {
                i += 1;
            }
            out.push_str(&self.s[start..i]);
            match b.get(i)? {
                b'"' => return Some((out, i + 1)),
                _ => {
                    let esc = *b.get(i + 1)?;
                    i += 2;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4(i)?;
                            i += 4;
                            let mut code = hi;
                            if (0xd800..=0xdbff).contains(&hi) && b[i..].starts_with(b"\\u") {
                                if let Some(lo) = self.hex4(i + 2) {
                                    if (0xdc00..=0xdfff).contains(&lo) {
                                        code = 0x10000 + (((hi - 0xd800) << 10) | (lo - 0xdc00));
                                        i += 6;
                                    }
                                }
                            }
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        }
                        _ => return None,
                    }
                }
            }
        }
    }

    fn hex4(&self, i: usize) -> Option<u32> {
        let h = self.s.get(i..i + 4)?;
        if !h.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        u32::from_str_radix(h, 16).ok()
    }

    fn object(&self, i: usize, depth: usize) -> Option<(Value, usize)> {
        let b = self.b;
        let mut map = Map::new();
        let mut i = json_ws(b, i);
        if b.get(i) == Some(&b'}') {
            return Some((Value::Object(map), i + 1));
        }
        loop {
            if b.get(i) != Some(&b'"') {
                return None;
            }
            let (key, e) = self.string(i + 1)?;
            i = json_ws(b, e);
            if b.get(i) != Some(&b':') {
                return None;
            }
            i = json_ws(b, i + 1);
            let (value, e) = self.value(i, depth + 1)?;
            map.insert(key, value);
            i = json_ws(b, e);
            match b.get(i)? {
                b'}' => return Some((Value::Object(map), i + 1)),
                b',' => i = json_ws(b, i + 1),
                _ => return None,
            }
        }
    }

    fn array(&self, i: usize, depth: usize) -> Option<(Value, usize)> {
        let b = self.b;
        let mut items = Vec::new();
        let mut i = json_ws(b, i);
        if b.get(i) == Some(&b']') {
            return Some((Value::Array(items), i + 1));
        }
        loop {
            let (value, e) = self.value(i, depth + 1)?;
            items.push(value);
            i = json_ws(b, e);
            match b.get(i)? {
                b']' => return Some((Value::Array(items), i + 1)),
                b',' => i = json_ws(b, i + 1),
                _ => return None,
            }
        }
    }
}

/// `json.loads(text, strict=False)`.
fn json_loads(text: &str) -> Option<Value> {
    if text.starts_with('\u{feff}') {
        return None;
    }
    let p = Json::new(text);
    let start = json_ws(text.as_bytes(), 0);
    let (value, end) = p.value(start, 0)?;
    (json_ws(text.as_bytes(), end) == text.len()).then_some(value)
}

// ---- prices and availability ------------------------------------------------------------------------------

/// Python's `float()` reads every Unicode decimal digit; each run of them is 0-9 in order.
fn ascii_digits(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii() {
            out.push(c);
            continue;
        }
        let is_digit = |ch: char| DIGIT_RE.is_match(ch.encode_utf8(&mut [0u8; 4]));
        if !is_digit(c) {
            out.push(c);
            continue;
        }
        let mut zero = c as u32;
        while let Some(prev) = char::from_u32(zero.wrapping_sub(1)) {
            if !is_digit(prev) {
                break;
            }
            zero -= 1;
        }
        let _ = i;
        out.push(char::from_digit((c as u32 - zero) % 10, 10).unwrap_or('0'));
    }
    out
}

/// `_price(raw, currency)`: a price from JSON-LD or a tag: a number, or a string like "1,299.00", "$85" or
/// "85.00 USD". In a string with several numbers the one after a currency sign wins ("Qty 3 · $45.00" is 45, not
/// 3). The last of "," or "." is the decimal point when 1-2 digits follow it ("1.299,00", "12,5"), or 3 in a
/// three-decimal currency such as KWD; otherwise every separator groups thousands ("1,299", "1.299.000").
/// changedetection.io's heuristic, re-implemented. Anything not finite, negative, or past MAX_PRICE (an integer
/// past a float included) is no price; a bool is no price.
pub fn price(raw: &Value, currency: &str) -> Option<f64> {
    let value = match raw {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => price_text(s, currency)?,
        _ => return None,
    };
    (value.is_finite() && (0.0..MAX_PRICE).contains(&value)).then_some(value)
}

fn price_text(raw: &str, currency: &str) -> Option<f64> {
    let found = match SIGNED_RE.captures(raw) {
        Some(c) => c.get(1)?.as_str().to_string(),
        None => NUMBER_RE.find(raw)?.as_str().to_string(),
    };
    let digits: String = ascii_digits(&found).chars().filter(|c| !matches!(c, '\u{a0}' | '\u{202f}' | '\'' | ' ')).collect();
    let chars: Vec<char> = digits.chars().collect();
    let last = chars.iter().rposition(|&c| c == ',' || c == '.');
    let separators = chars.iter().filter(|&&c| c == ',' || c == '.').count();
    let text = match last {
        Some(last) => {
            let after = chars.len() - last - 1;
            let three = THREE_DECIMALS.contains(&currency.to_uppercase().as_str()) && after == 3 && separators == 1;
            if (1..=2).contains(&after) || three {
                let head: String = chars[..last].iter().filter(|&&c| c != ',' && c != '.').collect();
                let tail: String = chars[last + 1..].iter().collect();
                format!("{head}.{tail}")
            } else {
                chars.iter().filter(|&&c| c != ',' && c != '.').collect()
            }
        }
        None => digits.clone(),
    };
    text.parse::<f64>().ok()
}

/// `_availability`: schema.org availability by its last path segment, letters only: true, false, or None.
pub fn availability(raw: &Value) -> Option<bool> {
    let Value::String(raw) = raw else { return None };
    let stripped = py_strip(raw);
    if stripped.is_empty() {
        return None;
    }
    let trimmed = stripped.trim_end_matches('/');
    let last = trimmed.rsplit('/').next().unwrap_or("");
    let word: String = last.to_lowercase().chars().filter(|c| c.is_ascii_lowercase()).collect();
    if AVAILABLE.contains(&word.as_str()) {
        return Some(true);
    }
    if UNAVAILABLE.contains(&word.as_str()) {
        return Some(false);
    }
    None
}

// ---- scripts, comments and blocks -------------------------------------------------------------------------

fn find_from(hay: &str, needle: &str, from: usize) -> Option<usize> {
    hay.get(from..)?.find(needle).map(|p| p + from)
}

/// `_ld_blocks`: the text of every ld+json script, in one linear pass (comments dropped first). A script with no
/// close tag ends the scan, as it does in a browser.
pub fn ld_blocks(page: &str) -> Vec<String> {
    // A lazy `<script…>(.*?)</script>` regex rescans to the end of the page for every unclosed opening, which is
    // quadratic: 250 KB of openings stalled the loop for seconds (security review, 2026-09-25).
    let page = drop_comments(page); // an old price in <!-- <script …ld+json> --> is not read
    let low = page.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(start) = find_from(&low, "<script", i) {
        let Some(end) = find_from(&low, ">", start) else { break };
        let Some(close) = find_from(&low, "</script", end) else {
            break; // nothing closes after this one, so nothing later can
        };
        if LD_TYPE_RE.is_match(&page[start..end]) {
            out.push(page[end + 1..close].to_string());
        }
        i = close + 8;
    }
    out
}

/// `_drop_comments`: the page without <!-- … --> comments, in one linear pass, the pieces joined by a space; an
/// unclosed comment hides the rest.
pub fn drop_comments(page: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut i = 0;
    while let Some(start) = find_from(page, "<!--", i) {
        out.push(&page[i..start]);
        match find_from(page, "-->", start + 4) {
            Some(close) => i = close + 3,
            None => return out.join(" "),
        }
    }
    out.push(&page[i..]);
    out.join(" ")
}

/// The next `<!--` or `<name` (a whole name: followed by whitespace, `>` or `/`) at or after `from`:
/// (start, end of the match, the name; "" for a comment).
fn next_block<'n>(page: &str, names: &[&'n str], from: usize) -> Option<(usize, usize, &'n str)> {
    let b = page.as_bytes();
    let mut i = from;
    while let Some(lt) = find_from(page, "<", i) {
        if b[lt..].starts_with(b"<!--") {
            return Some((lt, lt + 4, ""));
        }
        for name in names {
            let end = lt + 1 + name.len();
            if end <= b.len() && b[lt + 1..end].eq_ignore_ascii_case(name.as_bytes()) {
                if let Some(c) = page.get(end..).and_then(|rest| rest.chars().next()) {
                    if py_isspace(c) || c == '>' || c == '/' {
                        return Some((lt, end, name));
                    }
                }
            }
        }
        i = lt + 1;
    }
    None
}

/// `</name` followed by whitespace or `>`, in the lowercased page.
fn find_closer(low: &str, name: &str, from: usize) -> Option<usize> {
    let needle = format!("</{name}");
    let mut i = from;
    while let Some(p) = find_from(low, &needle, i) {
        if let Some(c) = low.get(p + needle.len()..).and_then(|rest| rest.chars().next()) {
            if py_isspace(c) || c == '>' {
                return Some(p);
            }
        }
        i = p + 1;
    }
    None
}

/// `_drop_blocks`: the page without its `names` elements and its comments, in one linear pass. A tag matches only
/// as a whole name (`<nav>` and `<nav class=…>`, never `<nav-drawer>`). An unclosed block hides the rest.
pub fn drop_blocks(page: &str, names: &[&str]) -> String {
    let low = page.to_ascii_lowercase();
    let mut out: Vec<&str> = Vec::new();
    let mut i = 0;
    while let Some((start, end, name)) = next_block(page, names, i) {
        out.push(&page[i..start]);
        if name.is_empty() {
            match find_from(page, "-->", end) {
                Some(close) => i = close + 3,
                None => return out.join(" "),
            }
            continue;
        }
        let Some(close) = find_closer(&low, &name.to_ascii_lowercase(), end) else {
            return out.join(" ");
        };
        i = match find_from(&low, ">", close) {
            Some(gt) => gt + 1,
            None => page.len(),
        };
    }
    out.push(&page[i..]);
    out.join(" ")
}

/// `_ld_values`: every JSON value in one ld+json script, leniently: raw control characters inside strings are
/// allowed (strict=False), an entity-escaped block is unescaped, framing lines (CDATA, comments) at either end are
/// dropped, and several values back to back are all read. A block that still does not parse gives nothing.
pub fn ld_values(block: &str) -> Vec<Value> {
    let mut text = py_strip(block).to_string();
    if text.contains("&quot;") || text.starts_with('&') {
        text = unescape(&text);
    }
    if let Some(v) = json_loads(&text) {
        return vec![v];
    }
    let mut lines = splitlines(&text);
    while lines.first().is_some_and(|l| FRAMING.replace_all(l, "").is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| FRAMING.replace_all(l, "").is_empty()) {
        lines.pop();
    }
    let joined = lines.join("\n");
    let text = py_strip(&joined);
    let parser = Json::new(text);
    let b = text.as_bytes();
    let mut values = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let Some((value, end)) = parser.value(i, 0) else { break };
        values.push(value);
        i = end;
        while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\r' | b'\n' | b';' | b',') {
            i += 1;
        }
    }
    values
}

// ---- html.parser, as Python 3.13 tokenizes ----------------------------------------------------------------

fn tag_ws(c: u8) -> bool {
    matches!(c, b'\t' | b'\n' | b'\r' | 0x0c | b' ')
}

/// One attribute value after `=`: a quoted value, or a bare one (possibly empty). None: an unclosed quote.
fn value_at(b: &[u8], p: usize) -> Option<usize> {
    match b.get(p) {
        Some(&q @ (b'\'' | b'"')) => b[p + 1..].iter().position(|&c| c == q).map(|x| p + 1 + x + 1),
        _ => {
            let mut e = p;
            while e < b.len() && !(b[e] == b'>' || tag_ws(b[e])) {
                e += 1;
            }
            Some(e)
        }
    }
}

/// `([\t\n\r\f ]*=[\t\n\r\f ]*(value))?` at `j`: (value start, value end, match end), with the regex engine's one
/// backtrack (an unclosed quote after spaces reads as an empty value before the last space).
fn value_group(b: &[u8], j: usize) -> Option<(usize, usize, usize)> {
    let mut p = j;
    while p < b.len() && tag_ws(b[p]) {
        p += 1;
    }
    if b.get(p) != Some(&b'=') {
        return None;
    }
    p += 1;
    let ws0 = p;
    while p < b.len() && tag_ws(b[p]) {
        p += 1;
    }
    if let Some(e) = value_at(b, p) {
        return Some((p, e, e));
    }
    if p > ws0 {
        return Some((p - 1, p - 1, p - 1));
    }
    None
}

fn attr_name_end(b: &[u8], k: usize) -> Option<usize> {
    if k == 0 || k >= b.len() {
        return None;
    }
    if !(matches!(b[k - 1], b'\'' | b'"' | b'/') || tag_ws(b[k - 1])) {
        return None;
    }
    if tag_ws(b[k]) || b[k] == b'/' || b[k] == b'>' {
        return None;
    }
    let mut j = k + 1;
    while j < b.len() && !(tag_ws(b[j]) || matches!(b[j], b'/' | b'=' | b'>')) {
        j += 1;
    }
    Some(j)
}

/// `locatetagend.match(rawdata, p)`'s end (`p` is at the tag name's letter).
fn locate_tag_end(b: &[u8], p: usize) -> usize {
    let mut i = p + 1;
    while i < b.len() && !(tag_ws(b[i]) || b[i] == b'/' || b[i] == b'>') {
        i += 1;
    }
    while i < b.len() && (tag_ws(b[i]) || b[i] == b'/') {
        i += 1;
    }
    while let Some(mut k) = attr_name_end(b, i) {
        if let Some((_, _, e)) = value_group(b, k) {
            k = e;
        }
        while k < b.len() && (tag_ws(b[k]) || b[k] == b'/') {
            k += 1;
        }
        i = k;
    }
    if b.get(i) == Some(&b'>') {
        i += 1;
    }
    i
}

/// `(?:[\t\n\r\f ]|/(?!>))*` from `j`.
fn skip_attr_gap(b: &[u8], mut j: usize) -> usize {
    loop {
        if j < b.len() && (tag_ws(b[j]) || (b[j] == b'/' && b.get(j + 1) != Some(&b'>'))) {
            j += 1;
        } else {
            return j;
        }
    }
}

/// `tagfind_tolerant.match(rawdata, p)`: (name end, match end).
fn tagfind(b: &[u8], p: usize) -> (usize, usize) {
    let mut j = p + 1;
    while j < b.len() && !(tag_ws(b[j]) || b[j] == b'/' || b[j] == b'>') {
        j += 1;
    }
    (j, skip_attr_gap(b, j))
}

/// `attrfind_tolerant.match(rawdata, k)`: (name range, value range when there is a value indicator, end).
#[allow(clippy::type_complexity)]
fn attrfind(b: &[u8], k: usize) -> Option<((usize, usize), Option<(usize, usize)>, usize)> {
    let name_end = attr_name_end(b, k)?;
    let mut j = name_end;
    let mut value = None;
    if let Some((vs, ve, e)) = value_group(b, j) {
        value = Some((vs, ve));
        j = e;
    }
    Some(((k, name_end), value, skip_attr_gap(b, j)))
}

const CDATA_CONTENT_ELEMENTS: [&str; 6] = ["script", "style", "xmp", "iframe", "noembed", "noframes"];
const RCDATA_CONTENT_ELEMENTS: [&str; 2] = ["textarea", "title"];

/// One open element: (tag, itemscope, the text being captured for its properties, its properties).
type Open = (String, bool, Option<String>, Vec<String>);

/// `_Microdata(HTMLParser)`.
#[derive(Default)]
struct Microdata {
    rawdata: String,
    cdata_elem: Option<String>,
    escapable: bool,
    found: Vec<(String, String)>,
    stack: Vec<Open>,
    scopes: Vec<String>,
}

impl Microdata {
    fn new() -> Self {
        Microdata { escapable: true, ..Default::default() }
    }

    fn in_offer(&self) -> bool {
        OFFER_SCOPES.contains(&self.scopes.last().map(String::as_str).unwrap_or(""))
    }

    fn handle_starttag(&mut self, tag: &str, attrs: &[(String, Option<String>)]) {
        let mut a: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
        for (k, v) in attrs {
            a.insert(k.as_str(), v.as_deref().unwrap_or(""));
        }
        let get = |k: &str| a.get(k).copied();
        let scoped = a.contains_key("itemscope");
        let props: Vec<String> = if scoped {
            Vec::new()
        } else {
            get("itemprop")
                .unwrap_or("")
                .split(py_isspace)
                .filter(|p| !p.is_empty() && MICRODATA_PROPS.contains(p))
                .map(String::from)
                .collect()
        };
        let void = VOID.contains(&tag);
        let mut capture = None;
        if !props.is_empty() && self.in_offer() {
            let value = if tag == "meta" {
                get("content")
            } else if (tag == "link" || tag == "a") && get("href").is_some_and(|h| !h.is_empty()) {
                get("href")
            } else if tag == "data" || tag == "meter" {
                get("value")
            } else {
                get("content")
            };
            match value {
                Some(v) if !v.is_empty() => {
                    self.found.extend(props.iter().map(|p| (p.clone(), v.to_string())));
                }
                _ if !void => capture = Some(String::new()),
                _ => {}
            }
        }
        if void {
            return;
        }
        let itemtype = get("itemtype").unwrap_or("").to_string();
        self.stack.push((tag.to_string(), scoped, capture, props));
        if scoped {
            let t = py_strip(&itemtype).trim_end_matches('/');
            self.scopes.push(t.rsplit('/').next().unwrap_or("").to_string());
        }
    }

    fn handle_startendtag(&mut self, tag: &str, attrs: &[(String, Option<String>)]) {
        self.handle_starttag(tag, attrs);
        if !VOID.contains(&tag) {
            self.handle_endtag(tag);
        }
    }

    fn handle_endtag(&mut self, tag: &str) {
        let Some(i) = self.stack.iter().rposition(|e| e.0 == tag) else {
            return; // a stray end tag closes nothing
        };
        while self.stack.len() > i {
            let Some((_, scoped, capture, props)) = self.stack.pop() else { break };
            if let Some(text) = capture {
                let text = collapse(&text);
                if !text.is_empty() {
                    self.found.extend(props.iter().map(|p| (p.clone(), text.clone())));
                }
            }
            if scoped {
                self.scopes.pop();
            }
        }
    }

    fn handle_data(&mut self, data: &str) {
        for entry in self.stack.iter_mut() {
            if let Some(c) = entry.2.as_mut() {
                c.push_str(data);
            }
        }
    }

    fn data(&mut self, raw: &str, i: usize, j: usize) {
        if self.escapable {
            self.handle_data(&unescape(&raw[i..j]));
        } else {
            self.handle_data(&raw[i..j]);
        }
    }

    fn set_cdata_mode(&mut self, elem: &str, escapable: bool) {
        self.cdata_elem = Some(elem.to_lowercase());
        self.escapable = escapable;
    }

    fn clear_cdata_mode(&mut self) {
        self.cdata_elem = None;
        self.escapable = true;
    }

    /// `self.interesting.search(rawdata, i)` in CDATA mode: `</elem` followed by whitespace, `/` or `>`.
    fn interesting_cdata(&self, raw: &str, i: usize) -> Option<usize> {
        let elem = self.cdata_elem.as_deref()?;
        if elem == "plaintext" {
            return Some(raw.len());
        }
        let b = raw.as_bytes();
        let mut p = i;
        while let Some(lt) = find_from(raw, "</", p) {
            let end = lt + 2 + elem.len();
            if end < b.len() && b[lt + 2..end].eq_ignore_ascii_case(elem.as_bytes()) {
                let c = b[end];
                if tag_ws(c) || c == b'/' || c == b'>' {
                    return Some(lt);
                }
            }
            p = lt + 1;
        }
        None
    }

    fn feed(&mut self, data: &str) {
        self.rawdata.push_str(data);
        self.goahead(false);
    }

    fn close(&mut self) {
        self.goahead(true);
    }

    fn goahead(&mut self, end: bool) {
        let raw = std::mem::take(&mut self.rawdata);
        let b = raw.as_bytes();
        let n = b.len();
        let mut i = 0;
        while i < n {
            let j = if self.cdata_elem.is_none() {
                match find_from(&raw, "<", i) {
                    Some(j) => j,
                    None => {
                        // a charref cut in half at the end: wait for the rest (the same data either way at close)
                        let lo = raw[i..].char_indices().rev().nth(33).map(|(x, _)| x + i).unwrap_or(i);
                        if let Some(amp) = raw[lo..].rfind('&').map(|x| x + lo) {
                            if !b[amp..].iter().any(|&c| tag_ws(c) || c == b';') {
                                break;
                            }
                        }
                        n
                    }
                }
            } else {
                match self.interesting_cdata(&raw, i) {
                    Some(j) => j,
                    None => break,
                }
            };
            if i < j {
                self.data(&raw, i, j);
            }
            i = j;
            if i == n {
                break;
            }
            // b[i] is '<' here: outside CDATA the next '<'; inside it, its closing tag
            let starttagopen = b.get(i + 1).is_some_and(u8::is_ascii_alphabetic);
            let rest = &b[i..];
            let k: Option<usize> = if starttagopen {
                self.parse_starttag(&raw, i)
            } else if rest.starts_with(b"</") {
                self.parse_endtag(&raw, i)
            } else if rest.starts_with(b"<!--") {
                parse_comment(b, i)
            } else if rest.starts_with(b"<?") {
                find_from(&raw, ">", i + 2).map(|p| p + 1)
            } else if rest.starts_with(b"<!") {
                parse_html_declaration(&raw, i)
            } else if i + 1 < n || end {
                self.handle_data("<");
                Some(i + 1)
            } else {
                break;
            };
            let k = match k {
                Some(k) => k,
                None => {
                    if !end {
                        break;
                    }
                    if !starttagopen && rest.starts_with(b"</") && i + 2 == n {
                        self.handle_data("</");
                    }
                    n
                }
            };
            i = k;
        }
        if end && i < n {
            self.data(&raw, i, n);
            i = n;
        }
        self.rawdata = raw[i..].to_string();
    }

    fn parse_starttag(&mut self, raw: &str, i: usize) -> Option<usize> {
        let b = raw.as_bytes();
        let endpos = locate_tag_end(b, i + 1);
        if b[endpos - 1] != b'>' {
            return None;
        }
        let (name_end, mut k) = tagfind(b, i + 1);
        let tag = raw[i + 1..name_end].to_lowercase();
        let mut attrs: Vec<(String, Option<String>)> = Vec::new();
        while k < endpos {
            let Some(((ns, ne), value, e)) = attrfind(b, k) else { break };
            let value = value.map(|(vs, ve)| {
                let v = &raw[vs..ve];
                let unquoted = if v.len() >= 2
                    && ((v.starts_with('\'') && v.ends_with('\'')) || (v.starts_with('"') && v.ends_with('"')))
                {
                    &v[1..v.len() - 1]
                } else if v == "'" || v == "\"" {
                    // a lone quote is its own first and last character
                    ""
                } else {
                    v
                };
                if unquoted.is_empty() {
                    String::new()
                } else {
                    unescape_attrvalue(unquoted)
                }
            });
            attrs.push((raw[ns..ne].to_lowercase(), value));
            k = e;
        }
        let tail = if k <= endpos { py_strip(&raw[k..endpos]) } else { "" };
        if tail != ">" && tail != "/>" {
            let text = raw[i..endpos].to_string();
            self.handle_data(&text);
            return Some(endpos);
        }
        if tail.ends_with("/>") {
            self.handle_startendtag(&tag, &attrs);
        } else {
            self.handle_starttag(&tag, &attrs);
            if CDATA_CONTENT_ELEMENTS.contains(&tag.as_str()) || tag == "plaintext" {
                self.set_cdata_mode(&tag, false);
            } else if RCDATA_CONTENT_ELEMENTS.contains(&tag.as_str()) {
                self.set_cdata_mode(&tag, true);
            }
        }
        Some(endpos)
    }

    fn parse_endtag(&mut self, raw: &str, i: usize) -> Option<usize> {
        let b = raw.as_bytes();
        find_from(raw, ">", i + 2)?;
        if !b.get(i + 2).is_some_and(u8::is_ascii_alphabetic) {
            if b.get(i + 2) == Some(&b'>') {
                return Some(i + 3); // </> is ignored
            }
            return find_from(raw, ">", i + 2).map(|p| p + 1); // a bogus comment
        }
        let j = locate_tag_end(b, i + 2);
        if b[j - 1] != b'>' {
            return None;
        }
        let (name_end, _) = tagfind(b, i + 2);
        let tag = raw[i + 2..name_end].to_lowercase();
        self.handle_endtag(&tag);
        self.clear_cdata_mode();
        Some(j)
    }
}

/// `parse_comment`: `--!?>` searched, else `-?>` right after `<!--`.
fn parse_comment(b: &[u8], i: usize) -> Option<usize> {
    let mut p = i + 4;
    while p + 1 < b.len() {
        if b[p] == b'-' && b[p + 1] == b'-' {
            if b.get(p + 2) == Some(&b'!') && b.get(p + 3) == Some(&b'>') {
                return Some(p + 4);
            }
            if b.get(p + 2) == Some(&b'>') {
                return Some(p + 3);
            }
        }
        p += 1;
    }
    match (b.get(i + 4), b.get(i + 5)) {
        (Some(b'>'), _) => Some(i + 5),
        (Some(b'-'), Some(b'>')) => Some(i + 6),
        _ => None,
    }
}

fn parse_html_declaration(raw: &str, i: usize) -> Option<usize> {
    let b = raw.as_bytes();
    if b[i..].starts_with(b"<!--") {
        return parse_comment(b, i);
    }
    if b[i..].starts_with(b"<![CDATA[") {
        return find_from(raw, "]]>", i + 9).map(|j| j + 3);
    }
    if b.len() >= i + 9 && b[i..i + 9].eq_ignore_ascii_case(b"<!doctype") {
        return find_from(raw, ">", i + 9).map(|p| p + 1);
    }
    find_from(raw, ">", i + 2).map(|p| p + 1)
}

/// `microdata(page)` (`_Microdata`, an HTMLParser): itemprop price / lowPrice / priceCurrency / availability as
/// (property, value), valued by the W3C microdata rules, kept only when the nearest itemscope is an offer or a
/// product. Never fails.
pub fn microdata(page: &str) -> Vec<(String, String)> {
    // meta's content, link and a's href, data and meter's value, else a content attribute, else the element's
    // text; a price inside a nested review or rating block is not taken for the item's (extruct's w3cmicrodata
    // scoping).
    let mut parser = Microdata::new();
    parser.feed(page);
    parser.close();
    parser.found
}

/// `_attrs(tag)`: one tag's attributes, names lowercased, values unquoted and entity-decoded.
pub fn attrs(tag: &str) -> Map<String, Value> {
    let mut out = Map::new();
    for c in ATTR_PAIR_RE.captures_iter(tag) {
        let value = c[2].trim_matches(|ch| ch == '"' || ch == '\'');
        out.insert(c[1].to_lowercase(), Value::String(unescape(value)));
    }
    out
}

/// `_offers`: every Offer-like object under `node` (anything with a price, lowPrice or availability), to depth
/// 12; a loyalty tier's price, a unit price and another product's offers are skipped.
pub fn offers(node: &Value, out: &mut Vec<Map<String, Value>>, depth: usize) {
    if depth > 12 {
        return;
    }
    let map = match node {
        Value::Array(items) => {
            for item in items {
                offers(item, out, depth + 1);
            }
            return;
        }
        Value::Object(map) => map,
        _ => return,
    };
    if map.contains_key("validForMemberTier") {
        return; // a loyalty tier's price is not what the owner pays
    }
    if map.contains_key("referenceQuantity") {
        return; // "2.40 per 100 g" is a unit price, not the item's price
    }
    if ["price", "lowPrice", "availability"].iter().any(|k| map.contains_key(*k)) {
        out.push(map.clone());
    }
    for (key, value) in map {
        if NOT_THE_ITEM.contains(&key.as_str()) {
            continue; // another product's offer is not this one's
        }
        if value.is_array() || value.is_object() {
            offers(value, out, depth + 1);
        }
    }
}

/// `_typed_name`: a Product's or an Event's name (to depth 6).
pub fn typed_name(node: &Value, depth: usize) -> String {
    // a site's JSON-LD often names its founder or its organisation before the product; LEGO's named Ole Kirk
    // Kristiansen first, 2026-09-25
    if depth > 6 {
        return String::new();
    }
    match node {
        Value::Array(items) => items.iter().map(|i| typed_name(i, depth + 1)).find(|n| !n.is_empty()).unwrap_or_default(),
        Value::Object(map) => {
            let kinds = match map.get("@type") {
                Some(Value::Array(ks)) => ks.iter().map(py_str).collect::<Vec<_>>().join(" "),
                other => str_or_empty(other),
            };
            if let Some(Value::String(name)) = map.get("name") {
                if ITEM_TYPES_RE.is_match(&kinds) && !kinds.contains("Offer") {
                    return name.clone();
                }
            }
            map.values().map(|v| typed_name(v, depth + 1)).find(|n| !n.is_empty()).unwrap_or_default()
        }
        _ => String::new(),
    }
}

/// `_any_name`: the first name of anything not an offer or an organisation (to depth 4).
pub fn any_name(node: &Value, depth: usize) -> String {
    if depth > 4 {
        return String::new();
    }
    match node {
        Value::Array(items) => items.iter().map(|i| any_name(i, depth + 1)).find(|n| !n.is_empty()).unwrap_or_default(),
        Value::Object(map) => {
            if let Some(Value::String(name)) = map.get("name") {
                let excluded = matches!(map.get("@type"), Some(Value::String(t))
                    if t == "Offer" || t == "AggregateOffer" || t == "Organization");
                if !excluded {
                    return name.clone();
                }
            }
            map.values().map(|v| any_name(v, depth + 1)).find(|n| !n.is_empty()).unwrap_or_default()
        }
        _ => String::new(),
    }
}

fn attr<'m>(m: &'m Map<String, Value>, key: &str) -> Option<&'m str> {
    m.get(key).and_then(Value::as_str)
}

/// `m.get("property") or m.get("name") or ""`.
fn meta_key(m: &Map<String, Value>) -> String {
    attr(m, "property")
        .filter(|s| !s.is_empty())
        .or_else(|| attr(m, "name").filter(|s| !s.is_empty()))
        .unwrap_or("")
        .to_string()
}

/// The price and availability a page states about itself, with no model: JSON-LD offers first (the lowest price
/// among them: a ticket page lists many), then product/og price meta tags, then itemprop price.
pub fn structured(page: &str) -> Reading {
    let mut reading = Reading { source: "structured".into(), ..Reading::default() };
    let mut prices: Vec<(f64, String)> = Vec::new();
    let mut avail: Vec<bool> = Vec::new();
    let blocks: Vec<Value> = ld_blocks(page).iter().flat_map(|b| ld_values(b)).collect();
    // a Product's or an Event's name from any block beats the first name in the first block
    reading.name = blocks.iter().map(|b| typed_name(b, 0)).find(|n| !n.is_empty()).unwrap_or_default();
    if reading.name.is_empty() {
        reading.name = blocks.iter().map(|b| any_name(b, 0)).find(|n| !n.is_empty()).unwrap_or_default();
    }
    for data in &blocks {
        let mut found = Vec::new();
        offers(data, &mut found, 0);
        for offer in &found {
            let currency = str_or_empty(offer.get("priceCurrency"));
            let raw = match offer.get("lowPrice") {
                Some(v) if !v.is_null() => v,
                _ => offer.get("price").unwrap_or(&Value::Null),
            };
            if let Some(p) = price(raw, &currency) {
                if p > 0.0 {
                    prices.push((p, currency.clone()));
                }
            }
            if let Some(a) = availability(offer.get("availability").unwrap_or(&Value::Null)) {
                avail.push(a);
            }
        }
    }
    let metas: Vec<Map<String, Value>> = META_TAG_RE.find_iter(page).map(|m| attrs(m.as_str())).collect();
    if prices.is_empty() {
        for m in &metas {
            // itemprop is microdata's: scoped there
            let key = meta_key(m).to_lowercase();
            if ["product:price:amount", "og:price:amount", "price"].contains(&key.as_str()) {
                let p = price(m.get("content").unwrap_or(&Value::Null), "");
                if let Some(p) = p.filter(|p| *p > 0.0) {
                    let cur = metas
                        .iter()
                        .find(|x| {
                            ["product:price:currency", "og:price:currency", "pricecurrency"]
                                .contains(&meta_key(x).to_lowercase().as_str())
                        })
                        .map(|x| attr(x, "content").unwrap_or("").to_string())
                        .unwrap_or_default();
                    prices.push((p, cur));
                }
            }
        }
    }
    let micro = if prices.is_empty() || avail.is_empty() { microdata(page) } else { Vec::new() };
    if prices.is_empty() {
        // The first priced offer on the page: microdata is spread through the markup, and a later one is as likely
        // a "you may also like" item as another tier of this one.
        let cur = micro.iter().find(|(k, _)| k == "priceCurrency").map(|(_, v)| v.clone()).unwrap_or_default();
        let first = micro
            .iter()
            .filter(|(k, _)| k == "lowPrice" || k == "price")
            .filter_map(|(_, v)| price(&Value::String(v.clone()), ""))
            .find(|p| *p != 0.0);
        if let Some(first) = first {
            prices.push((first, cur));
        }
    }
    if avail.is_empty() {
        avail.extend(
            micro.iter().filter(|(k, _)| k == "availability").filter_map(|(_, v)| availability(&Value::String(v.clone()))),
        );
    }
    if avail.is_empty() {
        for m in &metas {
            let key = meta_key(m).to_lowercase();
            if ["product:availability", "og:availability", "availability"].contains(&key.as_str()) {
                let content = attr(m, "content").unwrap_or("");
                let raw = if content.is_empty() { m.get("href").cloned().unwrap_or(Value::Null) } else { Value::String(content.into()) };
                let mut a = availability(&raw);
                if a.is_none() && ["in stock", "instock", "available"].contains(&py_strip(content).to_lowercase().as_str()) {
                    a = Some(true);
                }
                if let Some(a) = a {
                    avail.push(a);
                }
            }
        }
    }
    if let Some(low) = prices.iter().fold(None::<&(f64, String)>, |best, pc| match best {
        Some(b) if b.0 <= pc.0 => Some(b),
        _ => Some(pc),
    }) {
        reading.value = Some(low.0);
        reading.currency = take_chars(&low.1.to_uppercase(), 8);
    }
    if !avail.is_empty() {
        reading.available = Some(avail.iter().any(|a| *a)); // one ticket tier on sale is on sale
    }
    if reading.name.is_empty() {
        let low = page.to_ascii_lowercase();
        let start = low.find("<title");
        let end = start.and_then(|s| find_from(&low, ">", s));
        let close = end.and_then(|e| find_from(&low, "</title", e));
        reading.name = match (end, close) {
            (Some(end), Some(close)) => take_chars(&collapse(&unescape(page.get(end + 1..close).unwrap_or(""))), 120),
            _ => String::new(),
        };
    }
    reading
}

/// The words a person sees: no scripts, styles or tags, entities decoded, whitespace collapsed.
pub fn visible_text(page: &str) -> String {
    let text = drop_blocks(page, &["script", "style", "noscript", "template", "svg"]);
    let text = TAG_RE.replace_all(&text, " ");
    collapse(&unescape(&text))
}

/// The visible text of the item, for a model: the page's <main> when it has one, else the page, without the
/// site's nav, footer and aside. A <header> is kept.
pub fn main_text(page: &str) -> String {
    // A footer's gift-card price is not the item's; an article's own header holds its name and price
    // (correctness review, 2026-09-25); the prompt says to ignore cart totals.
    let mut page = page;
    if let Some((start, _, _)) = next_main(page) {
        if let Some(close) = page.to_ascii_lowercase().rfind("</main") {
            if close > start {
                page = &page[start..close];
            }
        }
    }
    // scripts first: a template string "<footer class=…" inside one opened a footer that swallowed the item
    let page = drop_blocks(page, &["script", "style", "noscript", "template", "svg"]);
    visible_text(&drop_blocks(&page, &["nav", "footer", "aside"]))
}

/// `_MAIN_OPEN.search(page)`: `<main` followed by whitespace, `>` or `/` (the comment alternative never asked).
fn next_main(page: &str) -> Option<(usize, usize, &'static str)> {
    let b = page.as_bytes();
    let mut i = 0;
    while let Some(lt) = find_from(page, "<", i) {
        let end = lt + 5;
        if end <= b.len() && b[lt + 1..end].eq_ignore_ascii_case(b"main") {
            if let Some(c) = page.get(end..).and_then(|rest| rest.chars().next()) {
                if py_isspace(c) || c == '>' || c == '/' {
                    return Some((lt, end, "main"));
                }
            }
        }
        i = lt + 1;
    }
    None
}

/// A phrase, case and spacing aside.
pub fn phrase_on(text: &str, phrase: &str) -> bool {
    let want = collapse(&phrase.to_lowercase());
    !want.is_empty() && collapse(&text.to_lowercase()).contains(&want)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unescape_follows_python() {
        assert_eq!(unescape("a &amp; b &lt;c&gt; &quot;"), "a & b <c> \"");
        assert_eq!(unescape("&ampx &notit; &#65;&#x42; &#0; &#128;"), "&x ¬it; AB \u{fffd} €");
        assert_eq!(unescape("&bogus; &"), "&bogus; &");
    }

    #[test]
    fn json_is_lenient_like_python() {
        assert_eq!(json_loads("{\"a\": \"x\ny\"}").unwrap()["a"], json!("x\ny"));
        assert_eq!(json_loads(" [1, 2.5, NaN] ").unwrap()[2], json!("NaN"));
        assert_eq!(json_loads(&format!("{{\"p\": 1{}}}", "0".repeat(400))).unwrap()["p"], json!("Infinity"));
        assert!(json_loads("{\"a\": 1,}").is_none());
        assert!(json_loads("{} {}").is_none());
        assert_eq!(json_loads("{\"a\":1,\"a\":2}").unwrap(), json!({"a": 2}));
        assert_eq!(json_loads("\"\\ud83d\\ude00\"").unwrap(), json!("😀"));
    }

    #[test]
    fn float_repr_matches_python() {
        assert_eq!(py_float_repr(30.0), "30.0");
        assert_eq!(py_float_repr(1.5), "1.5");
        assert_eq!(py_float_repr(1e16), "1e+16");
        assert_eq!(py_float_repr(0.00001234), "1.234e-05");
        assert_eq!(py_float_repr(0.0001), "0.0001");
        assert_eq!(py_float_repr(123456789012345.6), "123456789012345.6");
    }

    #[test]
    fn prices_read_as_changedetection_does() {
        let p = |s: &str| price(&json!(s), "");
        assert_eq!(p("1,299.00"), Some(1299.0));
        assert_eq!(p("1.299,00"), Some(1299.0));
        assert_eq!(p("12,5"), Some(12.5));
        assert_eq!(p("1,299"), Some(1299.0));
        assert_eq!(p("1.299.000"), Some(1_299_000.0));
        assert_eq!(p("Qty 3 · $45.00"), Some(45.0));
        assert_eq!(p("85.00 USD"), Some(85.0));
        assert_eq!(p("1\u{a0}299,50 €"), Some(1299.5));
        assert_eq!(p("٨٥"), Some(85.0));
        assert_eq!(price(&json!("1.250"), "KWD"), Some(1.25));
        assert_eq!(price(&json!("1.250"), "USD"), Some(1250.0));
        assert_eq!(price(&json!(true), ""), None);
        assert_eq!(price(&json!(-5), ""), None);
        assert_eq!(price(&json!(2e12), ""), None);
        assert_eq!(p("call us"), None);
    }

    #[test]
    fn availability_by_its_last_segment() {
        assert_eq!(availability(&json!("https://schema.org/InStock")), Some(true));
        assert_eq!(availability(&json!("http://schema.org/OutOfStock/")), Some(false));
        assert_eq!(availability(&json!("Sold_Out")), Some(false));
        assert_eq!(availability(&json!("maybe")), None);
        assert_eq!(availability(&json!(" ")), None);
        assert_eq!(availability(&json!(1)), None);
    }

    #[test]
    fn microdata_scopes_and_values() {
        let page = r#"<div itemscope itemtype="https://schema.org/Product"><div itemprop="offers" itemscope
            itemtype="https://schema.org/Offer"><span itemprop="price">$1,299.00 </span><meta itemprop=priceCurrency
            content=USD><link itemprop="availability" href="https://schema.org/InStock"/></div>
            <div itemprop="review" itemscope itemtype="https://schema.org/Review"><span itemprop="price">5</span></div></div>"#;
        assert_eq!(
            microdata(page),
            vec![
                ("price".to_string(), "$1,299.00".to_string()),
                ("priceCurrency".to_string(), "USD".to_string()),
                ("availability".to_string(), "https://schema.org/InStock".to_string()),
            ]
        );
        // a bare itemprop with no scope at all is the page's own
        assert_eq!(microdata(r#"<span itemprop="price" content="42.10">$42.10</span>"#), vec![("price".into(), "42.10".into())]);
        // script text is raw, and entities in data decode
        assert_eq!(microdata("<p itemprop=price>&pound;5<script>x</p></script></p>"), vec![("price".into(), "£5x</p>".into())]);
        assert_eq!(microdata("<span itemprop='price' content=\"a&amp;b\"></span>"), vec![("price".into(), "a&b".into())]);
        assert!(microdata("<<<>>><span itemprop=price").is_empty());
    }

    #[test]
    fn blocks_are_dropped_by_whole_name() {
        assert_eq!(drop_blocks("a<nav>x</nav>b<nav-drawer>y</nav-drawer>c", &["nav"]), "a b<nav-drawer>y</nav-drawer>c");
        assert_eq!(drop_blocks("a<!-- x -->b<script>1</script >c", &["script"]), "a b c");
        assert_eq!(drop_blocks("a<style>never closed", &["style"]), "a");
        assert_eq!(drop_comments("a<!--x-->b<!--y"), "a b");
    }

    #[test]
    fn ld_values_read_framed_and_concatenated_blocks() {
        assert_eq!(ld_values("<!--\n{\"a\":1}\n-->"), vec![json!({"a": 1})]);
        assert_eq!(ld_values("//<![CDATA[\n{\"a\":1};{\"b\":2}\n//]]>"), vec![json!({"a": 1}), json!({"b": 2})]);
        assert_eq!(ld_values("{&quot;a&quot;:1}"), vec![json!({"a": 1})]);
        assert!(ld_values("{not json").is_empty());
    }

    #[test]
    fn names_and_text() {
        assert_eq!(typed_name(&json!([{"@type": "Organization", "name": "Org"}, {"@type": ["Thing", "Product"], "name": "P"}]), 0), "P");
        assert_eq!(any_name(&json!({"@type": "Offer", "name": "o", "x": {"name": "n"}}), 0), "n");
        assert_eq!(visible_text("<p>a &amp;  b</p><script>x</script><svg><t>z</t></svg>"), "a & b");
        assert!(phrase_on("Tickets  ON sale now", "on   sale"));
        assert!(!phrase_on("anything", "  "));
        assert_eq!(attrs(r#"<meta Property="og:price:amount" content='1&amp;2'>"#)["property"], json!("og:price:amount"));
    }
}
