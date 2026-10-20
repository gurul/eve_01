//! Python's view of JSON values: `json.dumps(indent=2, ensure_ascii=False)` byte for byte, `repr`/`str` of a
//! parsed value, and Python's type names (for the errors Python would raise).

use std::io;

use serde::Serialize;
use serde_json::ser::{Formatter, PrettyFormatter};
use serde_json::Value;

/// `json.dumps(value, indent=2, ensure_ascii=False)`.
///
/// serde_json's pretty printer already matches Python's layout (two-space indent, `", "` between items as a
/// newline, `": "` after keys, `[]`/`{}` for empty containers) and its string escaping with `ensure_ascii=False`;
/// floats are written as Python's `repr` writes them.
pub fn dumps_indent2(value: &Value) -> String {
    let mut buf = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, PyFormatter { inner: PrettyFormatter::new() });
    value.serialize(&mut ser).expect("serializing a JSON value into memory cannot fail");
    String::from_utf8(buf).expect("serde_json writes UTF-8")
}

struct PyFormatter<'a> {
    inner: PrettyFormatter<'a>,
}

impl Formatter for PyFormatter<'_> {
    fn write_f64<W: ?Sized + io::Write>(&mut self, writer: &mut W, value: f64) -> io::Result<()> {
        writer.write_all(py_float_repr(value).as_bytes())
    }

    fn write_f32<W: ?Sized + io::Write>(&mut self, writer: &mut W, value: f32) -> io::Result<()> {
        self.write_f64(writer, f64::from(value))
    }

    fn begin_array<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.inner.begin_array(writer)
    }

    fn end_array<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.inner.end_array(writer)
    }

    fn begin_array_value<W: ?Sized + io::Write>(&mut self, writer: &mut W, first: bool) -> io::Result<()> {
        self.inner.begin_array_value(writer, first)
    }

    fn end_array_value<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.inner.end_array_value(writer)
    }

    fn begin_object<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.inner.begin_object(writer)
    }

    fn end_object<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.inner.end_object(writer)
    }

    fn begin_object_key<W: ?Sized + io::Write>(&mut self, writer: &mut W, first: bool) -> io::Result<()> {
        self.inner.begin_object_key(writer, first)
    }

    fn begin_object_value<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.inner.begin_object_value(writer)
    }

    fn end_object_value<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.inner.end_object_value(writer)
    }
}

/// `repr(float)` (which `json.dumps` uses): the shortest round-trip digits, in fixed notation when the decimal
/// exponent is in `[-4, 16)` and scientific (`1e+16`, `1e-05`) outside it. `json.dumps` spells the non-finite
/// values `NaN`, `Infinity` and `-Infinity`.
pub fn py_float_repr(value: f64) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    // `{:e}` gives the shortest round-trip digits: "-1.2345e-5", "1e16", "0e0".
    let sci = format!("{value:e}");
    let (mantissa, exp) = sci.split_once('e').expect("{:e} always has an exponent");
    let exp: i32 = exp.parse().expect("{:e} exponent is an integer");
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(m) => ("-", m),
        None => ("", mantissa),
    };
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let decpt = exp + 1;
    let body = if (-3..=16).contains(&decpt) {
        if decpt <= 0 {
            format!("0.{}{digits}", "0".repeat((-decpt) as usize))
        } else if decpt as usize >= digits.len() {
            format!("{digits}{}.0", "0".repeat(decpt as usize - digits.len()))
        } else {
            let (int, frac) = digits.split_at(decpt as usize);
            format!("{int}.{frac}")
        }
    } else {
        let (first, rest) = digits.split_at(1);
        let frac = if rest.is_empty() { String::new() } else { format!(".{rest}") };
        let esign = if exp < 0 { '-' } else { '+' };
        format!("{first}{frac}e{esign}{:02}", exp.abs())
    };
    format!("{sign}{body}")
}

/// Python's type name for a parsed JSON value.
pub fn py_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// `str(v)`: a string as itself, anything else as its `repr`.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// `repr(v)` of the value `json.load` gives.
pub fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() => match f {
                f if f.is_nan() => "nan".into(),
                f if f.is_infinite() && f > 0.0 => "inf".into(),
                f if f.is_infinite() => "-inf".into(),
                f => py_float_repr(f),
            },
            _ => n.to_string(),
        },
        Value::String(s) => str_repr(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter().map(|(k, v)| format!("{}: {}", str_repr(k), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// `repr(str)`: single quotes unless the text has a single quote and no double quote; backslash, the quote and
/// non-printable characters escaped.
fn str_repr(s: &str) -> String {
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
            c if (c as u32) < 0x20 || (0x7f..=0xa0).contains(&(c as u32)) || c == '\u{ad}' => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c if is_unprintable(c) => {
                let n = c as u32;
                if n <= 0xffff {
                    out.push_str(&format!("\\u{n:04x}"));
                } else {
                    out.push_str(&format!("\\U{n:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// The common non-printable characters beyond Latin-1 that Python's `str.isprintable` rejects: separators other
/// than the ASCII space, format characters, surrogates are impossible in a Rust `str`, and private-use areas.
fn is_unprintable(c: char) -> bool {
    matches!(c as u32,
        0x1680 | 0x2000..=0x200f | 0x2028..=0x202f | 0x205f..=0x2064 | 0x2066..=0x206f | 0x3000 | 0xfeff
        | 0xfff9..=0xfffb | 0xe000..=0xf8ff | 0xf0000..=0x10ffff)
}
