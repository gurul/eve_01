//! Serialization for the Hardware Eve BLE wire protocol.
//!
//! Matches the JSON schemas in the claude-desktop-buddy REFERENCE.md.
//! Everything is newline-terminated UTF-8 JSON.
//!
//! Port of `eve_bridge/protocol.py`.

mod codec;

use std::fmt::Write as _;

use eve_state::State;
use serde_json::{Map, Number, Value};
use thiserror::Error;

pub use codec::{Codec, LookupError};

// Nordic UART Service UUIDs (standard)
/// central → peripheral (we write)
pub const NUS_RX_UUID: &str = "6e400002-b5a3-f393-e0a9-e50e24dcca9e";
/// peripheral → central (we notify)
pub const NUS_TX_UUID: &str = "6e400003-b5a3-f393-e0a9-e50e24dcca9e";

/// How often we send a keepalive heartbeat if nothing else changed (seconds).
pub const HEARTBEAT_KEEPALIVE: f64 = 10.0;

/// Max UTF-8 bytes for the text portion of each entry (before the "HH:MM " prefix).
/// CJK characters are 3 bytes each, so 60 bytes ≈ 20 CJK chars or 60 ASCII chars.
/// Enforced in bytes (not chars) so the firmware's line buffer never overflows.
pub const ENTRY_MAX_BYTES: usize = 60;

/// Replacement character used when we strip a codepoint the stick can't render.
/// Keep it to 1 ASCII char so it doesn't blow up byte budgets or fall into the
/// same trap the original codepoint would have (multi-byte UTF-8 sequences that
/// bitmap fonts can't map).
pub const UNRENDERABLE_REPLACEMENT: &str = "?";

/// Maps CJK target → Python codec name. Used by [`sanitize_for_stick`] and
/// [`encode`] to switch the wire format when the user has flashed a fork-only
/// firmware build that ships the matching font (see claude-desktop-buddy
/// fork: `m5stickc-plus-cjk-zh-cn` / `-zh-tw` / `-ja`).
pub const CJK_CODECS: [(&str, &str); 3] = [("zh-CN", "gbk"), ("zh-TW", "big5"), ("ja", "shift_jis")];

/// `CJK_CODECS.get(target)`.
pub fn cjk_codec(target: &str) -> Option<&'static str> {
    CJK_CODECS.iter().find(|(k, _)| *k == target).map(|(_, v)| *v)
}

/// Why `datetime.fromtimestamp` refused an entry's timestamp; the message is
/// the one Python raises (`ValueError`, `OverflowError` or `OSError`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TimestampError {
    #[error("{0}")]
    Value(String),
    #[error("{0}")]
    Overflow(String),
    #[error("{0}")]
    Os(String),
}

/// Build a heartbeat snapshot dict ready for [`encode`].
///
/// Entry order on the wire is **oldest-first**. The reference firmware's
/// drawHUD treats `lines[n-1]` as the newest (highlighted, shown at the
/// bottom of the 3-row HUD window); it'd otherwise hide our newest entry at
/// the top of its wrapped buffer. We keep `state.entries` newest-first
/// internally because that's cheaper to prepend to — reverse on serialize.
///
/// Errors only where Python's `datetime.fromtimestamp` raises for an entry's
/// timestamp (NaN, outside time_t, or a year outside 1..=9999).
pub fn build_heartbeat(
    state: &State,
    msg: Option<&str>,
    codec: Option<&str>,
) -> Result<Map<String, Value>, TimestampError> {
    let mut snapshot = Map::new();
    snapshot.insert("total".into(), state.total().into());
    snapshot.insert("running".into(), state.running_count().into());
    snapshot.insert("waiting".into(), state.waiting_count().into());
    let msg = match msg {
        Some(m) => m.to_owned(),
        None => default_msg(state),
    };
    snapshot.insert("msg".into(), sanitize_for_stick(&msg, codec).into());
    let mut entries = Vec::with_capacity(state.entries.len());
    for e in state.entries.iter().rev() {
        entries.push(Value::String(sanitize_for_stick(&format_entry(e.at, &e.text)?, codec)));
    }
    snapshot.insert("entries".into(), Value::Array(entries));
    snapshot.insert("tokens".into(), state.tokens_cumulative.into());
    snapshot.insert("tokens_today".into(), state.tokens_today.into());
    // Per-agent rows for the landscape monitor build. Firmware ignores the
    // key when it doesn't know it, so this stays wire-compatible with the
    // portrait/pet builds.
    let agents = state
        .agent_rows(None)
        .into_iter()
        .map(|row| {
            let mut m = Map::new();
            m.insert("n".into(), sanitize_for_stick(&row.n, codec).into());
            m.insert("s".into(), sanitize_for_stick(&row.s, codec).into());
            if let Some(t) = &row.t {
                m.insert("t".into(), sanitize_for_stick(t, codec).into());
            }
            Value::Object(m)
        })
        .collect();
    snapshot.insert("agents".into(), Value::Array(agents));
    // Pulse the firmware's celebrate animation (confetti + bouncing) for the
    // few seconds after a turn ends. Honoured by data.h:_applyJson which maps
    // this field onto recentlyCompleted, picked up by main.cpp:derive.
    if state.is_celebrating() {
        snapshot.insert("completed".into(), Value::Bool(true));
    }
    Ok(snapshot)
}

/// Desktop sends on (re)connect: epoch seconds + timezone offset seconds.
pub fn build_time_sync() -> Map<String, Value> {
    let now = eve_state::wall_time().trunc() as i64;
    // `tm_gmtoff` is a C long: i64 here, i32 on 32-bit targets.
    #[allow(clippy::useless_conversion)]
    let offset = local_tm(now).map(|tm| i64::from(tm.tm_gmtoff)).unwrap_or(0);
    let mut m = Map::new();
    m.insert("time".into(), Value::Array(vec![now.into(), offset.into()]));
    m
}

/// Serialize obj as one-line JSON, newline-terminated.
///
/// With `codec` unset, falls back to standard UTF-8 JSON (`json.dumps` with
/// `separators=(",", ":")` and `ensure_ascii=False`). With `codec` set to a
/// non-UTF-8 name (`gbk` / `big5` / `shift_jis`), string *values* are encoded
/// in that codec instead — the bytes go straight into the JSON string between
/// quotes, which ArduinoJson 7 stores verbatim. Keys, numeric literals,
/// structural punctuation stay ASCII. Used by the CJK firmware build, where
/// the on-device font tables look up bytes in these legacy double-byte
/// encodings.
///
/// An unknown codec name errors (Python's `LookupError`) only once a string
/// value has to be encoded, as in Python.
pub fn encode(obj: &Value, codec: Option<&str>) -> Result<Vec<u8>, LookupError> {
    let mut out = Vec::new();
    match codec {
        None | Some("utf-8") => {
            let mut s = String::new();
            dumps_value(obj, &mut s);
            out.extend_from_slice(s.as_bytes());
        }
        Some(name) => {
            let resolved = Codec::lookup(name);
            encode_value(obj, &resolved, &mut out)?;
        }
    }
    out.push(b'\n');
    Ok(out)
}

/// `json.dumps(obj, separators=(",", ":"), ensure_ascii=False)`.
fn dumps_value(v: &Value, out: &mut String) {
    match v {
        Value::Object(m) => {
            out.push('{');
            for (i, (k, vv)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                dumps_string(k, out);
                out.push(':');
                dumps_value(vv, out);
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, item) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                dumps_value(item, out);
            }
            out.push(']');
        }
        Value::String(s) => dumps_string(s, out),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&py_number(n)),
        Value::Null => out.push_str("null"),
    }
}

/// The json module's `encode_basestring`: escapes `"`, `\` and C0 controls only.
fn dumps_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn encode_value(v: &Value, codec: &Result<Codec, LookupError>, out: &mut Vec<u8>) -> Result<(), LookupError> {
    match v {
        Value::Object(m) => {
            out.push(b'{');
            for (i, (k, vv)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                // Keys always go out as ASCII, unencodable characters as '?'.
                let key: String = k.chars().map(|c| if c.is_ascii() { c } else { '?' }).collect();
                encode_json_bytes(key.as_bytes(), out);
                out.push(b':');
                encode_value(vv, codec, out)?;
            }
            out.push(b'}');
        }
        Value::Array(a) => {
            out.push(b'[');
            for (i, item) in a.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                encode_value(item, codec, out)?;
            }
            out.push(b']');
        }
        Value::String(s) => encode_json_string(s, codec.clone()?, out),
        Value::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Number(n) => out.extend_from_slice(py_number(n).as_bytes()),
        Value::Null => out.extend_from_slice(b"null"),
    }
    Ok(())
}

/// Write a JSON-escaped string with bytes encoded in `codec`.
///
/// Chars unrepresentable in the codec become '?' (codec error='replace').
/// Inside the quoted body we only escape JSON's metabytes: backslash,
/// double-quote, and C0 controls. Codec high-bit bytes go through raw.
fn encode_json_string(s: &str, codec: Codec, out: &mut Vec<u8>) {
    encode_json_bytes(&codec.encode_replace(s), out);
}

/// The byte-level escaping of `_encode_json_string`. It runs on the encoded
/// bytes, so a double-byte character whose second byte is 0x5C gets that byte
/// escaped too, exactly as the Python daemon sends it.
fn encode_json_bytes(raw: &[u8], out: &mut Vec<u8>) {
    out.push(b'"');
    for &byte in raw {
        match byte {
            0x22 => out.extend_from_slice(b"\\\""),
            0x5C => out.extend_from_slice(b"\\\\"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x09 => out.extend_from_slice(b"\\t"),
            0x0A => out.extend_from_slice(b"\\n"),
            0x0D => out.extend_from_slice(b"\\r"),
            b if b < 0x20 => out.extend_from_slice(format!("\\u{b:04x}").as_bytes()),
            b => out.push(b),
        }
    }
    out.push(b'"');
}

/// `str(n)` / `json.dumps(n)` for an int or a float.
fn py_number(n: &Number) -> String {
    if let Some(i) = n.as_i64() {
        i.to_string()
    } else if let Some(u) = n.as_u64() {
        u.to_string()
    } else {
        py_float_repr(n.as_f64().unwrap_or(0.0))
    }
}

/// `repr(float)`: the shortest round-tripping digits, positional when the
/// decimal exponent is in -4..16, else `d.ddde+XX`.
fn py_float_repr(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    // `{:e}` gives the shortest round-trip digits, e.g. "-1.2345e17".
    let sci = format!("{:e}", x.abs());
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let sign = if x < 0.0 { "-" } else { "" };
    let decpt = exp + 1;
    let body = if decpt > -4 && decpt <= 16 {
        let n = digits.len() as i32;
        if decpt <= 0 {
            format!("0.{}{}", "0".repeat((-decpt) as usize), digits)
        } else if decpt >= n {
            format!("{}{}.0", digits, "0".repeat((decpt - n) as usize))
        } else {
            format!("{}.{}", &digits[..decpt as usize], &digits[decpt as usize..])
        }
    } else {
        let (first, rest) = digits.split_at(1);
        let mantissa = if rest.is_empty() { first.to_owned() } else { format!("{first}.{rest}") };
        let esign = if exp < 0 { '-' } else { '+' };
        format!("{mantissa}e{esign}{:02}", exp.abs())
    };
    format!("{sign}{body}")
}

// ---- line reassembly for stick → daemon stream ----

/// BLE notifications fragment at the MTU boundary. Collect until newline.
#[derive(Debug, Default, Clone)]
pub struct LineAssembler {
    buf: Vec<u8>,
}

impl LineAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Value> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=nl).take(nl).collect();
            let line = py_bytes_strip(&line);
            if line.is_empty() {
                continue;
            }
            // Drop malformed line rather than poison the stream.
            let Ok(text) = std::str::from_utf8(line) else { continue };
            if let Ok(v) = serde_json::from_str::<Value>(text) {
                out.push(v);
            }
        }
        out
    }
}

/// `bytes.strip()`: ASCII space, `\t`, `\n`, `\r`, `\x0b` and `\x0c` (Rust's
/// `trim_ascii` keeps `\x0b`).
fn py_bytes_strip(b: &[u8]) -> &[u8] {
    let ws = |c: &u8| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c);
    let start = b.iter().position(|c| !ws(c)).unwrap_or(b.len());
    let end = b.iter().rposition(|c| !ws(c)).map_or(start, |i| i + 1);
    &b[start..end]
}

// ---- sanitization ----

/// Strip characters the stick's font can't safely render.
///
/// Two regimes, controlled by `codec`:
///
/// - `codec` is `None` (stock firmware): strip everything outside printable
///   ASCII + tab. CJK becomes `?` because the default 5×7 font has no
///   glyphs for it and history shows the firmware crashes (LoadProhibited
///   from out-of-bounds font-table reads) when high-bit codepoints reach
///   the GLCD path. Two earlier sanitizer revisions both bit us — see git
///   log on protocol.py and the README quirk #1 narrative.
///
/// - `codec` in the [`CJK_CODECS`] values (CJK firmware variant flashed):
///   pass through every character representable in the target codec
///   (`gbk` covers GB2312, `big5` covers BIG5, `shift_jis` covers
///   JIS X 0208). Characters not in the codec — most emoji, mismatched
///   scripts — become `?` via the codec's `errors='replace'` mechanism.
///   The downstream [`encode`] call serializes string values as
///   target-codec bytes, which the firmware's font tables index directly.
pub fn sanitize_for_stick(text: &str, codec: Option<&str>) -> String {
    if text.is_empty() {
        return String::new();
    }
    match codec {
        None | Some("utf-8") => sanitize_ascii(text),
        // Codec-aware regime: round-trip through the target encoding so any
        // character not representable in it becomes the codec's replacement
        // byte (Python writes a literal '?' on encode-error='replace'), and the
        // result decodes back to a string of representable characters.
        Some(name) => match Codec::lookup(name) {
            Ok(c) => c.roundtrip_replace(text),
            // Unknown codec — fall back to the conservative ASCII policy.
            Err(_) => sanitize_ascii(text),
        },
    }
}

fn sanitize_ascii(text: &str) -> String {
    text.chars()
        .map(|ch| if (' '..='~').contains(&ch) || ch == '\t' { ch } else { '?' })
        .collect()
}

// ---- internals ----

/// Python's `str.strip()`: Unicode whitespace plus U+001C..U+001F.
fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

fn local_tm(t: i64) -> Result<libc::tm, std::io::Error> {
    let timet: libc::time_t = t as libc::time_t;
    // SAFETY: an all-zero `tm` is a valid out-parameter for localtime_r.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are valid for the duration of the call.
    let r = unsafe { libc::localtime_r(&timet, &mut tm) };
    if r.is_null() {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(tm)
    }
}

/// `datetime.fromtimestamp(at).strftime("%H:%M")`, with Python's rounding to
/// the microsecond (half to even) and its errors.
fn local_hhmm(at: f64) -> Result<String, TimestampError> {
    if at.is_nan() {
        return Err(TimestampError::Value("Invalid value NaN (not a number)".into()));
    }
    let mut int_part = at.trunc();
    let frac = (at - int_part) * 1e6;
    let us = frac.round_ties_even();
    if us >= 1e6 {
        int_part += 1.0;
    } else if us < 0.0 {
        int_part -= 1.0;
    }
    // time_t is 64-bit on every platform eve runs on.
    if !(-9.223_372_036_854_776e18..9.223_372_036_854_776e18).contains(&int_part) {
        return Err(TimestampError::Overflow("timestamp out of range for platform time_t".into()));
    }
    let tm = local_tm(int_part as i64).map_err(|e| TimestampError::Os(e.to_string()))?;
    let year = i64::from(tm.tm_year) + 1900;
    if !(1..=9999).contains(&year) {
        return Err(TimestampError::Value(format!("year {year} is out of range")));
    }
    Ok(format!("{:02}:{:02}", tm.tm_hour, tm.tm_min))
}

/// Format an entry for the wire: `HH:MM text`. REFERENCE.md shows
/// `10:42 git push`.
pub fn format_entry(at: f64, text: &str) -> Result<String, TimestampError> {
    let hhmm = local_hhmm(at)?;
    let text = text.replace('\n', " ");
    let text = py_strip(&text);
    Ok(format!("{hhmm} {}", truncate_utf8_bytes(text, ENTRY_MAX_BYTES)))
}

/// Truncate text so its UTF-8 encoding fits within max_bytes, appending '…' if cut.
///
/// Truncates at a codepoint boundary so no Chinese character is split.
/// '…' is 3 bytes; budget is reduced accordingly before slicing.
pub fn truncate_utf8_bytes(text: &str, max_bytes: usize) -> String {
    let encoded = text.as_bytes();
    if encoded.len() <= max_bytes {
        return text.to_owned();
    }
    // reserve space for '…' (3 UTF-8 bytes)
    if max_bytes <= 3 {
        return "…".into();
    }
    let mut end = max_bytes - 3;
    // If the byte right after the cut is a continuation byte (10xxxxxx), the
    // boundary falls mid-codepoint. Walk back to the lead byte of that
    // incomplete sequence and exclude the whole codepoint.
    while end > 0 && (encoded[end] & 0b1100_0000) == 0b1000_0000 {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// `_default_msg`: the heartbeat's status line when the caller gives none.
pub fn default_msg(state: &State) -> String {
    if state.running_count() > 0 {
        return format!("{} running", state.running_count());
    }
    if state.total() > 0 {
        return format!("{} idle", state.total());
    }
    "no sessions".into()
}

#[cfg(test)]
mod tests;
