//! The slice of Python value semantics the tailer depends on: what `json.loads` returns for a transcript line,
//! truthiness, `int(x)`, `str.strip()` and the built-in `sum()` over floats.
//!
//! The tailer reads a record the way the Python reads it, so a record Python skips is skipped here and a record
//! Python counts is counted here. `serde_json` disagrees with `json.loads` on lone surrogate escapes (which a
//! truncated emoji in a tool result produces), `NaN`/`Infinity`, integers past 64 bits and nesting depth, so
//! transcript lines are parsed by [`loads_bytes`], a port of CPython's strict scanner.

use indexmap::IndexMap;

/// A JSON object as Python's `dict`: insertion order, a repeated key keeps its first position and its last value.
pub type PyDict = IndexMap<String, PyValue>;

/// A value `json.loads` can return.
#[derive(Debug, Clone, PartialEq)]
pub enum PyValue {
    None,
    Bool(bool),
    Int(PyInt),
    Float(f64),
    Str(String),
    List(Vec<PyValue>),
    Dict(PyDict),
}

/// A Python `int`. `value` saturates at the `i64` bounds; `float` is the integer converted to a float the way
/// `float(n)` does it (correctly rounded), infinite where Python's `float(n)` raises `OverflowError`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PyInt {
    pub value: i64,
    pub float: f64,
}

impl PyInt {
    pub fn small(value: i64) -> Self {
        PyInt { value, float: value as f64 }
    }

    /// `int * float` in Python converts the int first, and raises `OverflowError` past the float range.
    pub fn float_overflows(&self) -> bool {
        self.float.is_infinite()
    }
}

/// Dropping a value nested thousands deep would recurse once per level and overflow a worker thread's stack
/// (the scanner accepts 9995 levels), so children are released from a heap stack instead.
impl Drop for PyValue {
    fn drop(&mut self) {
        fn take_children(v: &mut PyValue, out: &mut Vec<PyValue>) {
            match v {
                PyValue::List(items) => out.append(items),
                PyValue::Dict(map) => out.extend(map.drain(..).map(|(_, v)| v)),
                _ => {}
            }
        }
        let mut pending = Vec::new();
        take_children(self, &mut pending);
        while let Some(mut v) = pending.pop() {
            take_children(&mut v, &mut pending);
        }
    }
}

impl PyValue {
    /// Python truthiness.
    pub fn truthy(&self) -> bool {
        match self {
            PyValue::None => false,
            PyValue::Bool(b) => *b,
            PyValue::Int(i) => i.value != 0,
            PyValue::Float(f) => *f != 0.0,
            PyValue::Str(s) => !s.is_empty(),
            PyValue::List(l) => !l.is_empty(),
            PyValue::Dict(d) => !d.is_empty(),
        }
    }

    pub fn as_dict(&self) -> Option<&PyDict> {
        match self {
            PyValue::Dict(d) => Some(d),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            PyValue::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The Python type name, for log lines.
    pub fn type_name(&self) -> &'static str {
        match self {
            PyValue::None => "NoneType",
            PyValue::Bool(_) => "bool",
            PyValue::Int(_) => "int",
            PyValue::Float(_) => "float",
            PyValue::Str(_) => "str",
            PyValue::List(_) => "list",
            PyValue::Dict(_) => "dict",
        }
    }
}

/// Why `int(x)` raised: the exception class Python would raise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntError {
    /// `ValueError`: a string that is not an integer literal, or `float('nan')`.
    Value(String),
    /// `TypeError`: a list or a dict.
    Type(String),
    /// `OverflowError`: `int(float('inf'))`.
    Overflow(String),
}

/// `int(v or 0)`, the way the tailer and `pricing.estimate_cost` read a token count.
pub fn int_or_zero(v: Option<&PyValue>) -> Result<PyInt, IntError> {
    match v {
        None => Ok(PyInt::small(0)),
        Some(v) if !v.truthy() => Ok(PyInt::small(0)),
        Some(v) => py_int(v),
    }
}

/// `int(v)` for a value `json.loads` returns.
pub fn py_int(v: &PyValue) -> Result<PyInt, IntError> {
    match v {
        PyValue::None => Err(IntError::Type(
            "int() argument must be a string, a bytes-like object or a real number, not 'NoneType'".into(),
        )),
        PyValue::Bool(b) => Ok(PyInt::small(i64::from(*b))),
        PyValue::Int(i) => Ok(*i),
        PyValue::Float(f) => {
            if f.is_nan() {
                return Err(IntError::Value("cannot convert float NaN to integer".into()));
            }
            if f.is_infinite() {
                return Err(IntError::Overflow("cannot convert float infinity to integer".into()));
            }
            let t = f.trunc();
            // `as` saturates at the i64 bounds; the float keeps the exact truncated value.
            Ok(PyInt { value: t as i64, float: t })
        }
        PyValue::Str(s) => parse_int_str(s),
        PyValue::List(_) | PyValue::Dict(_) => Err(IntError::Type(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            v.type_name()
        ))),
    }
}

/// CPython's default `sys.get_int_max_str_digits()`.
pub const INT_MAX_STR_DIGITS: usize = 4300;

/// Whitespace `int(str)` tolerates around the literal: ASCII space, `\t\n\v\f\r`, and the non-ASCII characters
/// `str.isspace()` accepts (CPython maps those to a space before parsing; ASCII U+001C..U+001F stay as they are
/// and are rejected).
fn is_int_space(c: char) -> bool {
    if c.is_ascii() {
        matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r')
    } else {
        c.is_whitespace()
    }
}

/// The value of a Unicode decimal digit (category Nd), which `int()` reads in any script: "٥" and "５" are 5.
/// Unicode assigns Nd only in runs of ten, 0 to 9, so the value is the offset from the start of the run.
pub fn decimal_value(c: char) -> Option<u32> {
    use unicode_general_category::{get_general_category, GeneralCategory::DecimalNumber};
    if c.is_ascii_digit() {
        return c.to_digit(10);
    }
    if c.is_ascii() || get_general_category(c) != DecimalNumber {
        return None;
    }
    let mut start = c as u32;
    while let Some(prev) = start.checked_sub(1).and_then(char::from_u32) {
        if get_general_category(prev) != DecimalNumber {
            break;
        }
        start -= 1;
    }
    Some((c as u32 - start) % 10)
}

/// `int(s)` for base 10: surrounding whitespace, one sign, single underscores between digits, decimal digits of
/// any script, and at most [`INT_MAX_STR_DIGITS`] digits.
pub fn parse_int_str(s: &str) -> Result<PyInt, IntError> {
    let invalid = || IntError::Value(format!("invalid literal for int() with base 10: {}", py_repr(s)));
    let t = s.trim_matches(is_int_space);
    let (neg, body) = match t.chars().next() {
        Some('-') => (true, &t[1..]),
        Some('+') => (false, &t[1..]),
        _ => (false, t),
    };
    let mut digits = String::with_capacity(body.len());
    let mut prev_underscore = true; // a leading underscore is invalid
    for c in body.chars() {
        if c == '_' {
            if prev_underscore {
                return Err(invalid());
            }
            prev_underscore = true;
            continue;
        }
        let d = decimal_value(c).ok_or_else(invalid)?;
        digits.push(char::from_digit(d, 10).unwrap_or('0'));
        prev_underscore = false;
    }
    if digits.is_empty() || prev_underscore {
        return Err(invalid());
    }
    if digits.len() > INT_MAX_STR_DIGITS {
        return Err(IntError::Value(format!(
            "Exceeds the limit ({INT_MAX_STR_DIGITS} digits) for integer string conversion: value has {} digits; \
             use sys.set_int_max_str_digits() to increase the limit",
            digits.len()
        )));
    }
    Ok(int_from_ascii_digits(neg, &digits))
}

/// An integer from a run of ASCII digits and a sign: saturated `i64`, plus its correctly rounded float.
fn int_from_ascii_digits(neg: bool, digits: &str) -> PyInt {
    let signed = if neg { format!("-{digits}") } else { digits.to_string() };
    let value = signed.parse::<i64>().unwrap_or(if neg { i64::MIN } else { i64::MAX });
    let float = signed.parse::<f64>().unwrap_or(f64::INFINITY);
    PyInt { value, float }
}

/// Python's `str.isspace()` for one character: Unicode White_Space plus U+001C..U+001F.
pub fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python's `str.strip()`.
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(is_py_space)
}

/// `bytes.strip()` with no argument: ASCII space, `\t\n\r\v\f`.
pub fn bytes_strip(b: &[u8]) -> &[u8] {
    let ws = |c: &u8| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c);
    let start = b.iter().position(|c| !ws(c)).unwrap_or(b.len());
    let end = b.iter().rposition(|c| !ws(c)).map(|i| i + 1).unwrap_or(start);
    &b[start..end.max(start)]
}

/// A short `repr()` of a string for error messages (single quotes, backslash escapes for quotes and controls).
fn py_repr(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        match c {
            '\'' => out.push_str("\\'"),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// Python's built-in `sum()` over floats, which since 3.12 adds with Neumaier compensation, so a total matches
/// the Python one to the last bit for the same order of terms.
pub fn py_fsum_builtin<I: IntoIterator<Item = f64>>(values: I) -> f64 {
    let mut f_result = 0.0_f64;
    let mut c = 0.0_f64;
    for x in values {
        let t = f_result + x;
        if f_result.abs() >= x.abs() {
            c += (f_result - t) + x;
        } else {
            c += (x - t) + f_result;
        }
        f_result = t;
    }
    // Avoid losing the sign on a negative result, and don't let adding the compensation convert an infinite or
    // overflowed sum to a NaN.
    if c != 0.0 && c.is_finite() {
        f_result += c;
    }
    f_result
}

// ---- json.loads(bytes) ---------------------------------------------------------------------------------------

/// Why `json.loads` failed, by the exception class Python raises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonError {
    /// `ValueError` (`JSONDecodeError`, `UnicodeDecodeError`, the int digit limit): the tailer skips the line.
    Value(String),
    /// `RecursionError`: nesting deeper than the interpreter's C recursion budget. Not a `ValueError`, so in the
    /// tailer it escapes the line loop and ends that file's pass.
    Recursion,
}

impl std::fmt::Display for JsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JsonError::Value(m) => write!(f, "{m}"),
            JsonError::Recursion => write!(f, "maximum recursion depth exceeded while decoding a JSON array from a unicode string"),
        }
    }
}

impl std::error::Error for JsonError {}

/// The deepest container nesting `json.loads` decodes when the tailer calls it. CPython 3.13 gives each thread a
/// C recursion budget of 10000 and the scanner spends one unit per container; from the worker thread
/// `asyncio.to_thread` runs the tailer on, 9995 nested arrays decode and 9996 raise `RecursionError` (measured).
pub const MAX_JSON_DEPTH: usize = 9995;

/// `json.loads(raw)` for a `bytes` argument: the encoding is detected as `json.detect_encoding` does, decoded with
/// `surrogatepass`, then scanned strictly.
pub fn loads_bytes(raw: &[u8]) -> Result<PyValue, JsonError> {
    let text = decode(raw)?;
    loads_str(&text)
}

/// `json.detect_encoding`.
fn detect_encoding(b: &[u8]) -> &'static str {
    const BOM_UTF32_BE: &[u8] = b"\x00\x00\xfe\xff";
    const BOM_UTF32_LE: &[u8] = b"\xff\xfe\x00\x00";
    const BOM_UTF16_BE: &[u8] = b"\xfe\xff";
    const BOM_UTF16_LE: &[u8] = b"\xff\xfe";
    const BOM_UTF8: &[u8] = b"\xef\xbb\xbf";
    if b.starts_with(BOM_UTF32_BE) || b.starts_with(BOM_UTF32_LE) {
        return "utf-32";
    }
    if b.starts_with(BOM_UTF16_BE) || b.starts_with(BOM_UTF16_LE) {
        return "utf-16";
    }
    if b.starts_with(BOM_UTF8) {
        return "utf-8-sig";
    }
    if b.len() >= 4 {
        if b[0] == 0 {
            return if b[1] != 0 { "utf-16-be" } else { "utf-32-be" };
        }
        if b[1] == 0 {
            return if b[2] != 0 || b[3] != 0 { "utf-16-le" } else { "utf-32-le" };
        }
    } else if b.len() == 2 {
        if b[0] == 0 {
            return "utf-16-be";
        }
        if b[1] == 0 {
            return "utf-16-le";
        }
    }
    "utf-8"
}

fn decode_error(codec: &str, why: &str) -> JsonError {
    JsonError::Value(format!("'{codec}' codec can't decode bytes: {why}"))
}

/// `raw.decode(detect_encoding(raw), 'surrogatepass')`. A surrogate code point (which `surrogatepass` lets
/// through) cannot live in a Rust string, so it becomes U+FFFD.
fn decode(raw: &[u8]) -> Result<String, JsonError> {
    match detect_encoding(raw) {
        "utf-8" => decode_utf8(raw, "utf-8"),
        "utf-8-sig" => decode_utf8(&raw[3..], "utf-8"),
        "utf-16" => {
            // The BOM picks the byte order and is dropped.
            if raw.starts_with(b"\xff\xfe") {
                decode_utf16(&raw[2..], false)
            } else {
                decode_utf16(&raw[2..], true)
            }
        }
        "utf-16-be" => decode_utf16(raw, true),
        "utf-16-le" => decode_utf16(raw, false),
        "utf-32" => {
            if raw.starts_with(b"\xff\xfe\x00\x00") {
                decode_utf32(&raw[4..], false)
            } else {
                decode_utf32(&raw[4..], true)
            }
        }
        "utf-32-be" => decode_utf32(raw, true),
        _ => decode_utf32(raw, false),
    }
}

fn decode_utf8(mut b: &[u8], codec: &str) -> Result<String, JsonError> {
    let mut out = String::with_capacity(b.len());
    loop {
        match std::str::from_utf8(b) {
            Ok(s) => {
                out.push_str(s);
                return Ok(out);
            }
            Err(e) => {
                let good = e.valid_up_to();
                // SAFETY-free: from_utf8 vouched for the prefix.
                out.push_str(std::str::from_utf8(&b[..good]).unwrap_or_default());
                let rest = &b[good..];
                // `surrogatepass` accepts a UTF-8 encoded surrogate: ED A0..BF 80..BF.
                if rest.len() >= 3 && rest[0] == 0xed && (0xa0..=0xbf).contains(&rest[1]) && (0x80..=0xbf).contains(&rest[2])
                {
                    out.push('\u{fffd}');
                    b = &rest[3..];
                    continue;
                }
                return Err(decode_error(codec, "invalid data"));
            }
        }
    }
}

fn decode_utf16(b: &[u8], big_endian: bool) -> Result<String, JsonError> {
    if !b.len().is_multiple_of(2) {
        return Err(decode_error(if big_endian { "utf-16-be" } else { "utf-16-le" }, "truncated data"));
    }
    let units = b.chunks_exact(2).map(|p| if big_endian { u16::from_be_bytes([p[0], p[1]]) } else { u16::from_le_bytes([p[0], p[1]]) });
    // surrogatepass keeps unpaired surrogates; they become U+FFFD here.
    Ok(char::decode_utf16(units).map(|r| r.unwrap_or('\u{fffd}')).collect())
}

fn decode_utf32(b: &[u8], big_endian: bool) -> Result<String, JsonError> {
    let codec = if big_endian { "utf-32-be" } else { "utf-32-le" };
    if !b.len().is_multiple_of(4) {
        return Err(decode_error(codec, "truncated data"));
    }
    let mut out = String::with_capacity(b.len() / 4);
    for q in b.chunks_exact(4) {
        let cp = if big_endian { u32::from_be_bytes([q[0], q[1], q[2], q[3]]) } else { u32::from_le_bytes([q[0], q[1], q[2], q[3]]) };
        if (0xd800..=0xdfff).contains(&cp) {
            out.push('\u{fffd}');
        } else {
            out.push(char::from_u32(cp).ok_or_else(|| decode_error(codec, "code point not in range(0x110000)"))?);
        }
    }
    Ok(out)
}

/// `json.loads(s)` after decoding: CPython's strict scanner (`strict=True`, no hooks).
pub fn loads_str(s: &str) -> Result<PyValue, JsonError> {
    let mut p = Parser { s: s.as_bytes(), text: s, i: 0 };
    p.skip_ws();
    let v = p.value()?;
    p.skip_ws();
    if p.i != p.s.len() {
        return Err(p.err("Extra data"));
    }
    Ok(v)
}

/// One open container while the scanner works iteratively (no native recursion, so 9995 levels do not need a
/// deep thread stack).
enum Frame {
    List(Vec<PyValue>),
    Dict(PyDict, String),
}

struct Parser<'a> {
    s: &'a [u8],
    text: &'a str,
    i: usize,
}

impl Parser<'_> {
    fn err(&self, msg: &str) -> JsonError {
        JsonError::Value(format!("{msg}: char {}", self.text[..self.i.min(self.text.len())].chars().count()))
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn starts(&self, lit: &str) -> bool {
        self.s[self.i..].starts_with(lit.as_bytes())
    }

    /// A scalar, or `None` when the next byte opens a container (the caller pushes a frame).
    fn scalar(&mut self) -> Result<Option<PyValue>, JsonError> {
        let Some(c) = self.peek() else {
            return Err(self.err("Expecting value"));
        };
        let v = match c {
            b'"' => {
                self.i += 1;
                PyValue::Str(self.string()?)
            }
            b'{' | b'[' => return Ok(None),
            b'n' if self.starts("null") => {
                self.i += 4;
                PyValue::None
            }
            b't' if self.starts("true") => {
                self.i += 4;
                PyValue::Bool(true)
            }
            b'f' if self.starts("false") => {
                self.i += 5;
                PyValue::Bool(false)
            }
            b'N' if self.starts("NaN") => {
                self.i += 3;
                PyValue::Float(f64::NAN)
            }
            b'I' if self.starts("Infinity") => {
                self.i += 8;
                PyValue::Float(f64::INFINITY)
            }
            b'-' if self.starts("-Infinity") => {
                self.i += 9;
                PyValue::Float(f64::NEG_INFINITY)
            }
            b'-' | b'0'..=b'9' => self.number()?,
            _ => return Err(self.err("Expecting value")),
        };
        Ok(Some(v))
    }

    fn number(&mut self) -> Result<PyValue, JsonError> {
        let start = self.i;
        let mut j = self.i;
        if self.s.get(j) == Some(&b'-') {
            j += 1;
        }
        match self.s.get(j) {
            Some(b'0') => j += 1,
            Some(b'1'..=b'9') => {
                while matches!(self.s.get(j), Some(b'0'..=b'9')) {
                    j += 1;
                }
            }
            _ => return Err(self.err("Expecting value")),
        }
        let int_end = j;
        let mut is_float = false;
        if self.s.get(j) == Some(&b'.') && matches!(self.s.get(j + 1), Some(b'0'..=b'9')) {
            j += 2;
            while matches!(self.s.get(j), Some(b'0'..=b'9')) {
                j += 1;
            }
            is_float = true;
        }
        if matches!(self.s.get(j), Some(b'e' | b'E')) {
            let mut k = j + 1;
            if matches!(self.s.get(k), Some(b'+' | b'-')) {
                k += 1;
            }
            if matches!(self.s.get(k), Some(b'0'..=b'9')) {
                while matches!(self.s.get(k), Some(b'0'..=b'9')) {
                    k += 1;
                }
                j = k;
                is_float = true;
            }
        }
        self.i = j;
        let lit = &self.text[start..j];
        if is_float {
            // float(numstr): correctly rounded, infinite past the range.
            return Ok(PyValue::Float(lit.parse::<f64>().unwrap_or(f64::INFINITY)));
        }
        let (neg, digits) = match lit.strip_prefix('-') {
            Some(d) => (true, d),
            None => (false, &self.text[start..int_end]),
        };
        if digits.len() > INT_MAX_STR_DIGITS {
            return Err(JsonError::Value(format!(
                "Exceeds the limit ({INT_MAX_STR_DIGITS} digits) for integer string conversion: value has {} digits; \
                 use sys.set_int_max_str_digits() to increase the limit",
                digits.len()
            )));
        }
        Ok(PyValue::Int(int_from_ascii_digits(neg, digits)))
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let h = self.s.get(self.i..self.i + 4).ok_or_else(|| self.err("Invalid \\uXXXX escape"))?;
        let mut v = 0u32;
        for &b in h {
            let d = (b as char).to_digit(16).ok_or_else(|| self.err("Invalid \\uXXXX escape"))?;
            v = v * 16 + d;
        }
        self.i += 4;
        Ok(v)
    }

    /// The body of a string after its opening quote.
    fn string(&mut self) -> Result<String, JsonError> {
        let mut out = String::new();
        loop {
            let run_start = self.i;
            while let Some(b) = self.peek() {
                if b == b'"' || b == b'\\' || b < 0x20 {
                    break;
                }
                self.i += 1;
            }
            out.push_str(&self.text[run_start..self.i]);
            match self.peek() {
                None => return Err(self.err("Unterminated string starting at")),
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let Some(e) = self.peek() else {
                        return Err(self.err("Unterminated string starting at"));
                    };
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let u = self.hex4()?;
                            if (0xd800..=0xdbff).contains(&u) && self.s.get(self.i) == Some(&b'\\') && self.s.get(self.i + 1) == Some(&b'u') {
                                let save = self.i;
                                self.i += 2;
                                let lo = self.hex4()?;
                                if (0xdc00..=0xdfff).contains(&lo) {
                                    let cp = 0x10000 + (((u - 0xd800) << 10) | (lo - 0xdc00));
                                    out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                                    continue;
                                }
                                // Not a pair: the first stays lone and the second escape is read on its own.
                                self.i = save;
                            }
                            // A lone surrogate is a valid Python str but cannot be a Rust one.
                            out.push(char::from_u32(u).unwrap_or('\u{fffd}'));
                        }
                        _ => {
                            self.i -= 2;
                            return Err(self.err("Invalid \\escape"));
                        }
                    }
                }
                Some(_) => return Err(self.err("Invalid control character at")),
            }
        }
    }

    fn value(&mut self) -> Result<PyValue, JsonError> {
        let mut stack: Vec<Frame> = Vec::new();
        // Parse a value; when it completes, attach it to the frame on top.
        'value: loop {
            let mut done = match self.scalar()? {
                Some(v) => v,
                None => {
                    if stack.len() >= MAX_JSON_DEPTH {
                        return Err(JsonError::Recursion);
                    }
                    let open = self.s[self.i];
                    self.i += 1;
                    self.skip_ws();
                    if open == b'[' {
                        if self.peek() == Some(b']') {
                            self.i += 1;
                            PyValue::List(Vec::new())
                        } else {
                            stack.push(Frame::List(Vec::new()));
                            continue 'value;
                        }
                    } else if self.peek() == Some(b'}') {
                        self.i += 1;
                        PyValue::Dict(PyDict::new())
                    } else {
                        let key = self.object_key()?;
                        stack.push(Frame::Dict(PyDict::new(), key));
                        continue 'value;
                    }
                }
            };
            // Attach `done` to its parent, closing every container that ends right after it.
            loop {
                let Some(top) = stack.last_mut() else {
                    return Ok(done);
                };
                self.skip_ws();
                match top {
                    Frame::List(items) => {
                        items.push(done);
                        match self.peek() {
                            Some(b',') => {
                                self.i += 1;
                                self.skip_ws();
                                if self.peek() == Some(b']') {
                                    return Err(self.err("Illegal trailing comma before end of array"));
                                }
                                continue 'value;
                            }
                            Some(b']') => {
                                self.i += 1;
                            }
                            _ => return Err(self.err("Expecting ',' delimiter")),
                        }
                    }
                    Frame::Dict(map, key) => {
                        map.insert(std::mem::take(key), done);
                        match self.peek() {
                            Some(b',') => {
                                self.i += 1;
                                self.skip_ws();
                                if self.peek() == Some(b'}') {
                                    return Err(self.err("Illegal trailing comma before end of object"));
                                }
                                let k = self.object_key()?;
                                if let Some(Frame::Dict(_, key)) = stack.last_mut() {
                                    *key = k;
                                }
                                continue 'value;
                            }
                            Some(b'}') => {
                                self.i += 1;
                            }
                            _ => return Err(self.err("Expecting ',' delimiter")),
                        }
                    }
                }
                done = match stack.pop() {
                    Some(Frame::List(items)) => PyValue::List(items),
                    Some(Frame::Dict(map, _)) => PyValue::Dict(map),
                    // The top frame was checked above; an empty stack cannot reach here.
                    None => return Err(self.err("Expecting value")),
                };
            }
        }
    }

    /// `"key"` then `:`, leaving the cursor at the value.
    fn object_key(&mut self) -> Result<String, JsonError> {
        if self.peek() != Some(b'"') {
            return Err(self.err("Expecting property name enclosed in double quotes"));
        }
        self.i += 1;
        let key = self.string()?;
        self.skip_ws();
        if self.peek() != Some(b':') {
            return Err(self.err("Expecting ':' delimiter"));
        }
        self.i += 1;
        self.skip_ws();
        Ok(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(v: i64) -> PyValue {
        PyValue::Int(PyInt::small(v))
    }

    // Each expectation below was checked against `json.loads` / `int()` under Python 3.13.

    #[test]
    fn loads_accepts_what_python_accepts() {
        assert_eq!(loads_bytes(b"NaN").map(|v| matches!(v, PyValue::Float(f) if f.is_nan())), Ok(true));
        assert_eq!(loads_bytes(b"Infinity"), Ok(PyValue::Float(f64::INFINITY)));
        assert_eq!(loads_bytes(b"-Infinity"), Ok(PyValue::Float(f64::NEG_INFINITY)));
        assert_eq!(loads_bytes(b"1e400"), Ok(PyValue::Float(f64::INFINITY)));
        assert_eq!(loads_bytes(b"-0"), Ok(int(0)));
        assert_eq!(loads_bytes(b"1E5"), Ok(PyValue::Float(100000.0)));
        assert_eq!(loads_bytes(b" 1 "), Ok(int(1)));
        assert_eq!(loads_bytes(b"true "), Ok(PyValue::Bool(true)));
        assert_eq!(loads_bytes(br#"{"a" : 1}"#).unwrap().as_dict().unwrap()["a"], int(1));
        assert_eq!(loads_bytes(br#""\u00e9""#), Ok(PyValue::Str("é".into())));
        assert_eq!(loads_bytes(br#""\uD83D\uDE00""#), Ok(PyValue::Str("😀".into())));
        assert_eq!(loads_bytes(br#""\ud800\udc00""#), Ok(PyValue::Str("𐀀".into())));
        assert_eq!(loads_bytes(b"\"a\x7f\""), Ok(PyValue::Str("a\x7f".into())));
        // Lone surrogates decode in Python; here they become U+FFFD instead of failing the whole record.
        assert_eq!(loads_bytes(br#""\ud800""#), Ok(PyValue::Str("\u{fffd}".into())));
        assert_eq!(loads_bytes(br#""\udc00\ud800""#), Ok(PyValue::Str("\u{fffd}\u{fffd}".into())));
        assert_eq!(loads_bytes(b"\"\xed\xa0\x80\""), Ok(PyValue::Str("\u{fffd}".into())));
        // detect_encoding
        assert_eq!(loads_bytes(b"\xef\xbb\xbf{\"a\":1}").unwrap().as_dict().unwrap()["a"], int(1));
        assert_eq!(loads_bytes(b"\xff\xfe1\x00"), Ok(int(1)));
        assert_eq!(loads_bytes(b"\xfe\xff\x001"), Ok(int(1)));
        assert_eq!(loads_bytes(b"1\x00"), Ok(int(1)));
        assert_eq!(loads_bytes(b"\x001"), Ok(int(1)));
        assert_eq!(loads_bytes(b"1\x00\x00\x00"), Ok(int(1)));
        assert_eq!(loads_bytes(b"\x00\x00\x001"), Ok(int(1)));
    }

    #[test]
    fn loads_rejects_what_python_rejects() {
        for bad in [
            &b"-NaN"[..],
            b"1.",
            b"01",
            b"\"\t\"",
            b"[1,]",
            b"{\"a\":1,}",
            b"\"\\x\"",
            b"nul",
            b"[1 2]",
            b"1 2",
            b"\"\\u12\"",
            b"-",
            b"-a",
            b"2.5e",
            b"0x1",
            b"\"\\U0041\"",
            b"\xed\xa0\x80",
            b"\"\xff\"",
            b"\xef\xbb\xbf\xef\xbb\xbf1",
            b"",
            b"{1:2}",
            b"[",
            b"\"abc",
        ] {
            assert!(matches!(loads_bytes(bad), Err(JsonError::Value(_))), "{:?}", String::from_utf8_lossy(bad));
        }
        let many = "5".repeat(4301);
        assert!(matches!(loads_bytes(many.as_bytes()), Err(JsonError::Value(_))));
        let ok = "5".repeat(4300);
        assert!(matches!(loads_bytes(ok.as_bytes()), Ok(PyValue::Int(PyInt { value: i64::MAX, .. }))));
    }

    #[test]
    fn loads_nesting_limit_is_a_recursion_error() {
        let n = MAX_JSON_DEPTH;
        let ok = format!("{}{}", "[".repeat(n), "]".repeat(n));
        assert!(loads_bytes(ok.as_bytes()).is_ok());
        let deep = format!("{}{}", "[".repeat(n + 1), "]".repeat(n + 1));
        assert_eq!(loads_bytes(deep.as_bytes()), Err(JsonError::Recursion));
        let dicts = format!("{}1{}", "{\"a\":".repeat(n + 1), "}".repeat(n + 1));
        assert_eq!(loads_bytes(dicts.as_bytes()), Err(JsonError::Recursion));
        // Raised on opening, before the rest is checked.
        assert_eq!(loads_bytes(format!("{}garbage", "[".repeat(n + 1)).as_bytes()), Err(JsonError::Recursion));
    }

    #[test]
    fn loads_keeps_dict_order_and_last_duplicate() {
        let v = loads_bytes(br#"{"b":1,"a":2,"b":3}"#).unwrap();
        let d = v.as_dict().unwrap();
        assert_eq!(d.keys().collect::<Vec<_>>(), ["b", "a"]);
        assert_eq!(d["b"], int(3));
        let nested = loads_bytes(br#"[{"x":[1,{"y":[]}],"z":{}},2]"#).unwrap();
        let PyValue::List(items) = &nested else { panic!("list") };
        assert_eq!(items.len(), 2);
        assert_eq!(items[1], int(2));
    }

    #[test]
    fn int_parses_like_python() {
        let ok = |s: &str| parse_int_str(s).map(|i| i.value);
        assert_eq!(ok("٥"), Ok(5));
        assert_eq!(ok(" +1_0 "), Ok(10));
        assert_eq!(ok("\x0b5\x0c"), Ok(5));
        assert_eq!(ok("\u{85}5\u{2000}"), Ok(5));
        assert_eq!(ok("\u{3000}5"), Ok(5));
        assert_eq!(ok("٥_٥"), Ok(55));
        assert_eq!(ok("007"), Ok(7));
        assert_eq!(ok("0_7"), Ok(7));
        assert_eq!(ok("１２"), Ok(12));
        assert_eq!(ok("𝟓"), Ok(5));
        assert_eq!(ok("-0"), Ok(0));
        for bad in ["\x1c5\x1f", "1__0", "_1", "1_", "- 5", "0x5", "5.0", "", "+_5", "5\0", "+", "\u{180e}5"] {
            assert!(matches!(parse_int_str(bad), Err(IntError::Value(_))), "{bad:?}");
        }
        assert!(parse_int_str(&"5".repeat(4300)).is_ok());
        assert!(matches!(parse_int_str(&"5".repeat(4301)), Err(IntError::Value(_))));
        assert!(matches!(parse_int_str(&format!("{}5", "5_".repeat(4300))), Err(IntError::Value(_))));
        let big = parse_int_str(&"9".repeat(400)).unwrap();
        assert_eq!(big.value, i64::MAX);
        assert!(big.float_overflows());
    }

    #[test]
    fn int_of_values_like_python() {
        assert_eq!(int_or_zero(None), Ok(PyInt::small(0)));
        assert_eq!(int_or_zero(Some(&PyValue::Str(String::new()))), Ok(PyInt::small(0)));
        assert_eq!(int_or_zero(Some(&PyValue::List(vec![]))), Ok(PyInt::small(0)));
        assert_eq!(int_or_zero(Some(&PyValue::Float(-0.0))), Ok(PyInt::small(0)));
        assert_eq!(int_or_zero(Some(&PyValue::Bool(true))), Ok(PyInt::small(1)));
        assert_eq!(int_or_zero(Some(&PyValue::Float(2.9))).map(|i| i.value), Ok(2));
        assert_eq!(int_or_zero(Some(&PyValue::Float(-2.9))).map(|i| i.value), Ok(-2));
        assert!(matches!(int_or_zero(Some(&PyValue::Float(f64::NAN))), Err(IntError::Value(_))));
        assert!(matches!(int_or_zero(Some(&PyValue::Float(f64::INFINITY))), Err(IntError::Overflow(_))));
        assert!(matches!(int_or_zero(Some(&PyValue::List(vec![int(1)]))), Err(IntError::Type(_))));
        assert!(matches!(int_or_zero(Some(&PyValue::Str("abc".into()))), Err(IntError::Value(_))));
        let huge = int_or_zero(Some(&PyValue::Float(1e300))).unwrap();
        assert_eq!(huge.value, i64::MAX);
        assert!(!huge.float_overflows());
    }

    #[test]
    fn strip_helpers_match_python() {
        assert_eq!(py_strip("\u{1c} hi \u{2000}\n"), "hi");
        assert_eq!(bytes_strip(b"\x0b\x0c \r\n"), b"");
        assert_eq!(bytes_strip(b" {} \n"), b"{}");
    }

    #[test]
    fn builtin_sum_compensates_like_python() {
        // sum([0.1] * 10) == 1.0 in Python 3.12+, where naive addition gives 0.9999999999999999.
        assert_eq!(py_fsum_builtin(std::iter::repeat_n(0.1, 10)), 1.0);
        // sum([1e100, 1.0, -1e100, 1.0]) == 2.0 in Python 3.12+.
        assert_eq!(py_fsum_builtin([1e100, 1.0, -1e100, 1.0]), 2.0);
        assert_eq!(py_fsum_builtin([]), 0.0);
        assert_eq!(py_fsum_builtin([f64::INFINITY, 1.0]), f64::INFINITY);
    }
}
