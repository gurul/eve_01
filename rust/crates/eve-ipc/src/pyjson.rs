//! `json.loads(text)` as CPython 3.13's C scanner (`Modules/_json.c`) runs it,
//! so a malformed request is refused with the same `bad json: ...` text the
//! Python daemon sends: the same messages, at the same character positions,
//! formatted as `JSONDecodeError` formats them.
//!
//! The parser is iterative, so nesting depth costs heap, not stack. A request
//! nested deeper than [`MAX_DEPTH`] is refused the way Python's
//! `RecursionError` is: it is not a `ValueError`, so the IPC server sends no
//! reply.
//!
//! What a `serde_json::Value` cannot hold is handled as written in
//! `PARITY.md`: a lone surrogate escape becomes U+FFFD, an integer past 64 bits
//! becomes the nearest float, and `NaN`/`Infinity` (or a float literal that
//! overflows to infinity) is refused with Python's own text for that value.

use serde_json::{Map, Number, Value};

/// How many arrays and objects may be open at once. CPython's C recursion
/// limit lets a request nest about 9,990 levels before `RecursionError`; a
/// `Value` that deep would overflow a 2 MiB task stack when it is cloned,
/// printed or dropped (all recursive), so the port stops at 1,000.
pub const MAX_DEPTH: usize = 1000;

/// CPython's `sys.get_int_max_str_digits()` default.
const INT_MAX_STR_DIGITS: usize = 4300;

/// Why `loads` refused the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadsError {
    /// A `ValueError` (`JSONDecodeError` or the int-digits limit): the text is
    /// what `str(e)` gives.
    Value(String),
    /// A `RecursionError`, with its message.
    Recursion(String),
}

impl std::fmt::Display for LoadsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadsError::Value(m) | LoadsError::Recursion(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for LoadsError {}

impl LoadsError {
    /// The Python exception class.
    pub fn python_type(&self) -> &'static str {
        match self {
            LoadsError::Value(m) if m.starts_with("Exceeds the limit") || m.starts_with("Out of range float") => {
                "ValueError"
            }
            LoadsError::Value(_) => "JSONDecodeError",
            LoadsError::Recursion(_) => "RecursionError",
        }
    }
}

/// Python's `json.loads(s)` for a `str`.
pub fn loads(s: &str) -> Result<Value, LoadsError> {
    let chars: Vec<char> = s.chars().collect();
    let p = Parser { s: &chars, out_of_range: std::cell::Cell::new(None) };
    if chars.first() == Some(&'\u{feff}') {
        return Err(p.decode_error("Unexpected UTF-8 BOM (decode using utf-8-sig)", 0));
    }
    let idx = p.skip_ws(0);
    let (value, end) = match p.scan_once(idx) {
        Ok(v) => v,
        Err(Fail::Stop(at)) => return Err(p.decode_error("Expecting value", at)),
        Err(Fail::Error(e)) => return Err(e),
    };
    let end = p.skip_ws(end);
    if end != chars.len() {
        return Err(p.decode_error("Extra data", end));
    }
    // Python parsed it; only now is a value a `Value` cannot hold refused, so
    // a syntax error anywhere still reads as Python's.
    if let Some(repr) = p.out_of_range.get() {
        return Err(out_of_range(repr));
    }
    Ok(value)
}

enum Fail {
    /// `StopIteration(idx)`: no value starts here. It travels out of any
    /// nesting and `raw_decode` turns it into `Expecting value` at `idx`.
    Stop(usize),
    Error(LoadsError),
}

impl From<LoadsError> for Fail {
    fn from(e: LoadsError) -> Self {
        Fail::Error(e)
    }
}

enum Frame {
    Array(Vec<Value>),
    /// The object so far and the key whose value is being read.
    Object(Map<String, Value>, String),
}

struct Parser<'a> {
    s: &'a [char],
    /// The first NaN or infinity met, as Python's `repr` prints it.
    out_of_range: std::cell::Cell<Option<&'static str>>,
}

impl Parser<'_> {
    /// Note a float `Value` cannot hold; parsing goes on with a stand-in.
    fn unrepresentable(&self, repr: &'static str) -> Value {
        if self.out_of_range.get().is_none() {
            self.out_of_range.set(Some(repr));
        }
        Value::Null
    }

    fn at(&self, i: usize) -> Option<char> {
        self.s.get(i).copied()
    }

    fn skip_ws(&self, mut i: usize) -> usize {
        while matches!(self.at(i), Some(' ' | '\t' | '\n' | '\r')) {
            i += 1;
        }
        i
    }

    fn matches(&self, i: usize, word: &str) -> bool {
        let n = word.chars().count();
        i + n <= self.s.len() && self.s[i..i + n].iter().copied().eq(word.chars())
    }

    /// `JSONDecodeError(msg, doc, pos)`: `"<msg>: line L column C (char P)"`.
    fn decode_error(&self, msg: &str, pos: usize) -> LoadsError {
        let before = &self.s[..pos.min(self.s.len())];
        let lineno = before.iter().filter(|&&c| c == '\n').count() + 1;
        let colno = match before.iter().rposition(|&c| c == '\n') {
            Some(nl) => pos - nl,
            None => pos + 1,
        };
        LoadsError::Value(format!("{msg}: line {lineno} column {colno} (char {pos})"))
    }

    fn enter(&self, depth: usize, what: &str) -> Result<(), LoadsError> {
        if depth >= MAX_DEPTH {
            return Err(LoadsError::Recursion(format!(
                "maximum recursion depth exceeded while decoding a JSON {what} from a unicode string"
            )));
        }
        Ok(())
    }

    /// `scan_once_unicode`, with the nesting kept on a heap stack.
    fn scan_once(&self, start: usize) -> Result<(Value, usize), Fail> {
        let mut stack: Vec<Frame> = Vec::new();
        let mut idx = start;
        'value: loop {
            let (mut value, mut next) = match self.at(idx) {
                None => return Err(Fail::Stop(idx)),
                Some('"') => {
                    let (s, end) = self.scanstring(idx + 1)?;
                    (Value::String(s), end)
                }
                Some('{') => {
                    self.enter(stack.len(), "object")?;
                    let i = self.skip_ws(idx + 1);
                    if self.at(i) == Some('}') {
                        (Value::Object(Map::new()), i + 1)
                    } else {
                        let (key, i) = self.object_key(i)?;
                        stack.push(Frame::Object(Map::new(), key));
                        idx = i;
                        continue 'value;
                    }
                }
                Some('[') => {
                    self.enter(stack.len(), "array")?;
                    let i = self.skip_ws(idx + 1);
                    if self.at(i) == Some(']') {
                        (Value::Array(Vec::new()), i + 1)
                    } else {
                        stack.push(Frame::Array(Vec::new()));
                        idx = i;
                        continue 'value;
                    }
                }
                Some('n') if self.matches(idx, "null") => (Value::Null, idx + 4),
                Some('t') if self.matches(idx, "true") => (Value::Bool(true), idx + 4),
                Some('f') if self.matches(idx, "false") => (Value::Bool(false), idx + 5),
                Some('N') if self.matches(idx, "NaN") => (self.unrepresentable("nan"), idx + 3),
                Some('I') if self.matches(idx, "Infinity") => (self.unrepresentable("inf"), idx + 8),
                Some('-') if self.matches(idx, "-Infinity") => (self.unrepresentable("-inf"), idx + 9),
                Some(_) => self.match_number(idx)?,
            };
            // A value is complete: hand it to the innermost open container,
            // closing containers for as long as their closing bracket follows.
            loop {
                let Some(frame) = stack.last_mut() else {
                    return Ok((value, next));
                };
                let i = self.skip_ws(next);
                match frame {
                    Frame::Array(items) => {
                        items.push(value);
                        if self.at(i) == Some(']') {
                            value = Value::Array(std::mem::take(items));
                            stack.pop();
                            next = i + 1;
                            continue;
                        }
                        if self.at(i) != Some(',') {
                            return Err(self.decode_error("Expecting ',' delimiter", i).into());
                        }
                        let comma = i;
                        let j = self.skip_ws(i + 1);
                        if self.at(j) == Some(']') {
                            return Err(self.decode_error("Illegal trailing comma before end of array", comma).into());
                        }
                        idx = j;
                        continue 'value;
                    }
                    Frame::Object(map, key) => {
                        map.insert(std::mem::take(key), value);
                        if self.at(i) == Some('}') {
                            value = Value::Object(std::mem::take(map));
                            stack.pop();
                            next = i + 1;
                            continue;
                        }
                        if self.at(i) != Some(',') {
                            return Err(self.decode_error("Expecting ',' delimiter", i).into());
                        }
                        let comma = i;
                        let j = self.skip_ws(i + 1);
                        if self.at(j) == Some('}') {
                            return Err(self.decode_error("Illegal trailing comma before end of object", comma).into());
                        }
                        let (k, j) = self.object_key(j)?;
                        *key = k;
                        idx = j;
                        continue 'value;
                    }
                }
            }
        }
    }

    /// A key, its `:` and the whitespace after it; returns where the value starts.
    fn object_key(&self, i: usize) -> Result<(String, usize), Fail> {
        if self.at(i) != Some('"') {
            return Err(self.decode_error("Expecting property name enclosed in double quotes", i).into());
        }
        let (key, i) = self.scanstring(i + 1)?;
        let i = self.skip_ws(i);
        if self.at(i) != Some(':') {
            return Err(self.decode_error("Expecting ':' delimiter", i).into());
        }
        Ok((key, self.skip_ws(i + 1)))
    }

    /// `scanstring_unicode(pystr, end, strict=True)`: `end` is the index just
    /// past the opening quote; returns the string and the index past the
    /// closing one.
    fn scanstring(&self, mut end: usize) -> Result<(String, usize), LoadsError> {
        let len = self.s.len();
        let begin = end - 1;
        let mut out = String::new();
        loop {
            // Find the end of the string or the next escape.
            let mut next = end;
            let mut c = None;
            while next < len {
                let d = self.s[next];
                if d == '"' || d == '\\' {
                    c = Some(d);
                    break;
                }
                if (d as u32) <= 0x1f {
                    return Err(self.decode_error("Invalid control character at", next));
                }
                next += 1;
            }
            let Some(c) = c else {
                return Err(self.decode_error("Unterminated string starting at", begin));
            };
            out.extend(&self.s[end..next]);
            next += 1;
            if c == '"' {
                return Ok((out, next));
            }
            if next == len {
                return Err(self.decode_error("Unterminated string starting at", begin));
            }
            let esc = self.s[next];
            if esc != 'u' {
                end = next + 1;
                let ch = match esc {
                    '"' => '"',
                    '\\' => '\\',
                    '/' => '/',
                    'b' => '\u{8}',
                    'f' => '\u{c}',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    _ => return Err(self.decode_error("Invalid \\escape", end - 2)),
                };
                out.push(ch);
                continue;
            }
            next += 1;
            end = next + 4;
            if end >= len {
                return Err(self.decode_error("Invalid \\uXXXX escape", next - 1));
            }
            let mut cp = match self.hex4(next) {
                Some(v) => v,
                None => return Err(self.decode_error("Invalid \\uXXXX escape", end - 5)),
            };
            // A high surrogate followed by an escaped low one is one character.
            if (0xD800..=0xDBFF).contains(&cp)
                && end + 6 < len
                && self.s[end] == '\\'
                && self.s[end + 1] == 'u'
            {
                end += 6;
                let low = match self.hex4(end - 4) {
                    Some(v) => v,
                    None => return Err(self.decode_error("Invalid \\uXXXX escape", end - 5)),
                };
                if (0xDC00..=0xDFFF).contains(&low) {
                    cp = 0x10000 + (((cp - 0xD800) << 10) | (low - 0xDC00));
                } else {
                    end -= 6;
                }
            }
            // A lone surrogate has no `char`; Python keeps it, the port writes U+FFFD.
            out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
        }
    }

    fn hex4(&self, at: usize) -> Option<u32> {
        let mut v = 0u32;
        for &d in &self.s[at..at + 4] {
            v = (v << 4) | d.to_digit(16)?;
        }
        Some(v)
    }

    /// `_match_number_unicode`: the longest number at `start`, backing off a
    /// dangling `.` or exponent the way the C scanner does.
    fn match_number(&self, start: usize) -> Result<(Value, usize), Fail> {
        let len = self.s.len();
        let digit = |i: usize| self.at(i).is_some_and(|c| c.is_ascii_digit());
        let mut idx = start;
        if self.at(idx) == Some('-') {
            idx += 1;
            if idx >= len {
                return Err(Fail::Stop(start));
            }
        }
        match self.at(idx) {
            Some('1'..='9') => {
                idx += 1;
                while digit(idx) {
                    idx += 1;
                }
            }
            Some('0') => idx += 1,
            _ => return Err(Fail::Stop(start)),
        }
        let mut is_float = false;
        if idx + 1 < len && self.s[idx] == '.' && digit(idx + 1) {
            is_float = true;
            idx += 2;
            while digit(idx) {
                idx += 1;
            }
        }
        if idx + 1 < len && matches!(self.s[idx], 'e' | 'E') {
            let e_start = idx;
            idx += 1;
            if idx + 1 < len && matches!(self.s[idx], '-' | '+') {
                idx += 1;
            }
            while digit(idx) {
                idx += 1;
            }
            if digit(idx - 1) {
                is_float = true;
            } else {
                idx = e_start;
            }
        }
        let text: String = self.s[start..idx].iter().collect();
        let value = match number(&text, is_float)? {
            Ok(v) => v,
            Err(repr) => self.unrepresentable(repr),
        };
        Ok((value, idx))
    }
}

/// Python's `ValueError` for a float JSON cannot carry
/// (`json.dumps(..., allow_nan=False)` says the same).
fn out_of_range(repr: &str) -> LoadsError {
    LoadsError::Value(format!("Out of range float values are not JSON compliant: {repr}"))
}

/// `int(text)` or `float(text)` for a literal the scanner matched; the inner
/// error is the `repr` of a float that overflowed to infinity.
fn number(text: &str, is_float: bool) -> Result<Result<Value, &'static str>, LoadsError> {
    if !is_float {
        let digits = text.trim_start_matches('-').len();
        if digits > INT_MAX_STR_DIGITS {
            return Err(LoadsError::Value(format!(
                "Exceeds the limit ({INT_MAX_STR_DIGITS} digits) for integer string conversion: value has {digits} digits; use sys.set_int_max_str_digits() to increase the limit"
            )));
        }
        if let Ok(i) = text.parse::<i64>() {
            return Ok(Ok(Value::Number(i.into())));
        }
        if let Ok(u) = text.parse::<u64>() {
            return Ok(Ok(Value::Number(u.into())));
        }
    }
    // Rust's parse rounds correctly, as Python's float() does. An integer past
    // 64 bits lands here too and becomes the nearest float.
    let f: f64 = text.parse().unwrap_or(f64::NAN);
    Ok(match Number::from_f64(f) {
        Some(n) if f.is_finite() => Ok(Value::Number(n)),
        _ if f.is_nan() => Err("nan"),
        _ if f > 0.0 => Err("inf"),
        _ => Err("-inf"),
    })
}
