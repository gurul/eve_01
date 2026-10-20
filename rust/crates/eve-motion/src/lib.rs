//! Named motions the board runs by itself. Port of `eve_bridge/motion.py`.
//!
//! eve used to move by being told one absolute pose at a time. A dance is a
//! sequence, so a rhythm cost a model round trip per beat and came out stepped.
//! The measurement that started this: median 2.3 s from the owner's words to the
//! head moving, and the delegated model call was 1.86 s of it — five times
//! everything else combined (log study, 2026-09-11).
//!
//! So the vocabulary moved onto the board. One JSON line names a rhythm and the
//! firmware runs it locally at its own tween rate (`motion.h`, `body.cpp`), which
//! means a whole dance costs one line instead of one line per beat, and the beats
//! are generated 25 times a second instead of arriving over USB.
//!
//! ```text
//! {"cmd":"move","kind":"osc","yaw_amp":25,"pitch_amp":12,"period_ms":900,
//!  "phase_deg":90,"cycles":8,"dwell_pct":8,"jitter_pct":10}
//! ```
//!
//! ### Who decides what is safe
//!
//! **The board does, and this module is only friendly.** Every number is passed
//! through `motion::admit()` on the firmware before a servo sees it: amplitude
//! shrinks to fit the travel left about the centre, the per-axis velocity budget
//! and the bout cap, and cycles are shed rather than beats shortened. A host bug,
//! or a model asking for 120 degrees at 10 Hz, gets something safe rather than
//! obeyed — verified against the shipped header, where that request comes back as
//! 15 degrees at 1.5 Hz.
//!
//! The presets here are checked against the same header, so what this module
//! predicts is what the board runs. `dance` asks for 25 degrees rather than the 28
//! it once did, because 28 is what the board reduces it to.
//!
//! ### Amplitude yields, never the beat
//!
//! When the owner asks for a faster dance, the rhythm is the salient thing.
//! Speeding a motion up shortens the period and the board then narrows the swing
//! to stay inside the velocity budget. A faster dance is a tighter dance, not a
//! refused one.
//!
//! Commands are `serde_json::Map`s (insertion-ordered, as the Python dicts are),
//! so a command serialises to the same JSON line the Python daemon sends.

use serde_json::{json, Map, Value};

/// A JSON object, as the Python `dict[str, Any]` commands are.
pub type Command = Map<String, Value>;

/// 127 on an axis means "leave this one alone" (`motion::KEEP`).
pub const KEEP: i64 = 127;
pub const MAX_KEYS: usize = 8;
pub const SPEED_MIN: f64 = 0.5;
pub const SPEED_MAX: f64 = 2.0;

/// The speed a key gets when a raw key leaves it out.
const DEFAULT_KEY_SPEED: i64 = 500;

/// The order the overridable rhythm fields are applied and written in.
const OSC_FIELDS: [&str; 8] = [
    "yaw_amp",
    "pitch_amp",
    "period_ms",
    "pitch_period_ms",
    "phase_deg",
    "cycles",
    "dwell_pct",
    "jitter_pct",
];

/// One rhythm preset: its name and its fields, in the Python dict's order.
pub type OscPreset = (&'static str, &'static [(&'static str, i64)]);

/// One bout each. Amplitudes in degrees about the centre, periods in ms.
/// `dwell_pct` flat-tops the sine so the head pauses at each extreme the way a
/// creature does; `jitter_pct` varies the swing cycle to cycle so it is not a
/// metronome. Both are aliveness rather than capability: set them to zero and
/// every motion still works.
pub static OSC: &[OscPreset] = &[
    (
        "sway",
        &[
            ("yaw_amp", 25),
            ("pitch_amp", 0),
            ("period_ms", 1600),
            ("cycles", 3),
            ("dwell_pct", 10),
            ("jitter_pct", 8),
        ],
    ),
    // side to side and up and down at once, a quarter cycle apart, so the head
    // traces an ellipse rather than a diagonal line
    (
        "dance",
        &[
            ("yaw_amp", 25),
            ("pitch_amp", 12),
            ("period_ms", 900),
            ("phase_deg", 90),
            ("cycles", 8),
            ("dwell_pct", 8),
            ("jitter_pct", 10),
        ],
    ),
    (
        "nod",
        &[
            ("yaw_amp", 0),
            ("pitch_amp", 10),
            ("period_ms", 700),
            ("cycles", 3),
            ("dwell_pct", 12),
            ("jitter_pct", 6),
        ],
    ),
    (
        "shake",
        &[
            ("yaw_amp", 18),
            ("pitch_amp", 0),
            ("period_ms", 650),
            ("cycles", 3),
            ("dwell_pct", 6),
            ("jitter_pct", 8),
        ],
    ),
    (
        "bounce",
        &[
            ("yaw_amp", 0),
            ("pitch_amp", 8),
            ("period_ms", 650),
            ("cycles", 4),
            ("dwell_pct", 10),
            ("jitter_pct", 10),
        ],
    ),
    (
        "wiggle",
        &[
            ("yaw_amp", 8),
            ("pitch_amp", 0),
            ("period_ms", 650),
            ("cycles", 5),
            ("dwell_pct", 0),
            ("jitter_pct", 15),
        ],
    ),
    // yaw at half the pitch rate: a figure of eight
    (
        "fig8",
        &[
            ("yaw_amp", 25),
            ("pitch_amp", 10),
            ("period_ms", 1600),
            ("pitch_period_ms", 800),
            ("cycles", 4),
            ("dwell_pct", 8),
            ("jitter_pct", 8),
        ],
    ),
];

/// One gesture preset: its name and its `[at_ms, yaw, pitch, speed]` keys.
pub type KeysPreset = (&'static str, &'static [[i64; 4]]);

/// Gestures whose SHAPE is the point, so they are keyframes rather than a
/// rhythm: `[at_ms, yaw, pitch, speed]`, 127 leaves an axis alone. Overshoot is
/// hand-authored here, the way the firmware's own sequences already are.
pub static KEYS: &[KeysPreset] = &[
    ("doubletake", &[[0, 25, KEEP, 900], [180, -12, KEEP, 900], [340, 0, KEEP, 500]]),
    ("shrug", &[[0, 14, 35, 800], [300, 14, 35, 200], [700, 0, 45, 250]]),
    ("tilt", &[[0, 12, 53, 300]]),
    ("perk", &[[0, KEEP, 70, 700]]),
    ("droop", &[[0, KEEP, 33, 200]]),
    (
        "lean_peek",
        &[[0, -70, 50, 200], [1800, -78, 58, 150], [3200, -78, 58, 150], [3400, 0, 45, 900]],
    ),
    ("home", &[[0, 0, 45, 400]]),
];

/// The fields of one rhythm preset (`OSC[name]`), or `None` (`name not in OSC`).
pub fn osc_preset(name: &str) -> Option<&'static [(&'static str, i64)]> {
    OSC.iter().find(|(n, _)| *n == name).map(|(_, f)| *f)
}

/// `OSC[name].get(field)`.
pub fn osc_field(name: &str, field: &str) -> Option<i64> {
    osc_preset(name)?.iter().find(|(k, _)| *k == field).map(|(_, v)| *v)
}

/// The keys of one gesture preset (`KEYS[name]`), or `None` (`name not in KEYS`).
pub fn keys_preset(name: &str) -> Option<&'static [[i64; 4]]> {
    KEYS.iter().find(|(n, _)| *n == name).map(|(_, k)| *k)
}

/// `sorted(OSC)`.
pub fn osc_names() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = OSC.iter().map(|(n, _)| *n).collect();
    v.sort_unstable();
    v
}

/// `sorted(KEYS)`.
pub fn keys_names() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = KEYS.iter().map(|(n, _)| *n).collect();
    v.sort_unstable();
    v
}

/// The exceptions `motion.py` raises. The daemon's `_ipc_move` catches only
/// `ValueError` and turns it into `{"ok": false, "error": str(e)}`; the other
/// kinds escape as they do in Python, so a caller can tell them apart.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MotionError {
    /// Python `ValueError`: a bad name, a short key, no keys, or a number
    /// `int()` cannot read.
    #[error("{0}")]
    Value(String),
    /// Python `TypeError`: a value of the wrong JSON type where a number or a
    /// list belongs.
    #[error("{0}")]
    Type(String),
    /// Python `IndexError`: an empty key handed to `predict`.
    #[error("{0}")]
    Index(String),
    /// Python `OverflowError`: an infinite number handed to `int()`.
    #[error("{0}")]
    Overflow(String),
}

impl MotionError {
    /// The Python exception class this stands for.
    pub fn python_type(&self) -> &'static str {
        match self {
            MotionError::Value(_) => "ValueError",
            MotionError::Type(_) => "TypeError",
            MotionError::Index(_) => "IndexError",
            MotionError::Overflow(_) => "OverflowError",
        }
    }
}

/// The overrides `osc_command` takes after the preset and the speed. `None` is
/// Python's `None`: the preset's value (or no field at all) stands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OscParams {
    pub yaw_amp: Option<i64>,
    pub pitch_amp: Option<i64>,
    pub period_ms: Option<i64>,
    pub pitch_period_ms: Option<i64>,
    pub phase_deg: Option<i64>,
    pub cycles: Option<i64>,
    pub dwell_pct: Option<i64>,
    pub jitter_pct: Option<i64>,
    pub center_yaw: Option<i64>,
    pub center_pitch: Option<i64>,
}

impl OscParams {
    /// The keyword arguments the daemon's `_ipc_move` builds from an IPC
    /// request: every one of the ten fields whose value is present and not
    /// null, read with Python's `int()` (so `"12"`, `12.9` and `true` read as
    /// 12, 12 and 1, and `"abc"` is the same ValueError the daemon reports).
    pub fn from_request(req: &Map<String, Value>) -> Result<Self, MotionError> {
        let get = |key: &str| -> Result<Option<i64>, MotionError> {
            match req.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(v) => py_int(v).map(Some),
            }
        };
        Ok(OscParams {
            yaw_amp: get("yaw_amp")?,
            pitch_amp: get("pitch_amp")?,
            period_ms: get("period_ms")?,
            pitch_period_ms: get("pitch_period_ms")?,
            phase_deg: get("phase_deg")?,
            cycles: get("cycles")?,
            dwell_pct: get("dwell_pct")?,
            jitter_pct: get("jitter_pct")?,
            center_yaw: get("center_yaw")?,
            center_pitch: get("center_pitch")?,
        })
    }

    fn rhythm_field(&self, key: &str) -> Option<i64> {
        match key {
            "yaw_amp" => self.yaw_amp,
            "pitch_amp" => self.pitch_amp,
            "period_ms" => self.period_ms,
            "pitch_period_ms" => self.pitch_period_ms,
            "phase_deg" => self.phase_deg,
            "cycles" => self.cycles,
            "dwell_pct" => self.dwell_pct,
            "jitter_pct" => self.jitter_pct,
            _ => None,
        }
    }
}

fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    // Python's `lo if v < lo else (hi if v > hi else v)`: NaN falls through.
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

/// One `move` line for a rhythm. A preset is a starting point, not a cage.
///
/// `speed` above 1 shortens the period, which is what "faster" means to a
/// person. The swing then narrows on the board to stay inside the velocity
/// budget: the beat is kept and the amplitude yields.
pub fn osc_command(preset: Option<&str>, speed: f64, params: &OscParams) -> Result<Command, MotionError> {
    let name = preset.unwrap_or("");
    let mut base: Vec<(&'static str, i64)> = match osc_preset(name) {
        Some(fields) => fields.to_vec(),
        None if !name.is_empty() => {
            return Err(MotionError::Value(format!(
                "{} is not a rhythm; try {}",
                py_repr_str(name),
                osc_names().join(", ")
            )))
        }
        None => Vec::new(),
    };
    for key in OSC_FIELDS {
        if let Some(value) = params.rhythm_field(key) {
            set_field(&mut base, key, value);
        }
    }

    let factor = clamp(speed, SPEED_MIN, SPEED_MAX);
    if factor != 1.0 {
        for key in ["period_ms", "pitch_period_ms"] {
            if let Some(v) = get_field(&base, key) {
                if v != 0 {
                    let scaled = py_round_int(v as f64 / factor)?;
                    set_field(&mut base, key, scaled);
                }
            }
        }
    }

    let mut out = Command::new();
    out.insert("cmd".into(), json!("move"));
    out.insert("kind".into(), json!("osc"));
    for key in OSC_FIELDS {
        if let Some(v) = get_field(&base, key) {
            out.insert(key.into(), json!(v));
        }
    }
    // A centre is optional on purpose: without one the board centres the rhythm
    // on the pose the head is already holding, so a motion grows out of where it
    // is rather than snapping somewhere first.
    if let Some(v) = params.center_yaw {
        out.insert("center_yaw".into(), json!(v));
    }
    if let Some(v) = params.center_pitch {
        out.insert("center_pitch".into(), json!(v));
    }
    Ok(out)
}

fn get_field(base: &[(&'static str, i64)], key: &str) -> Option<i64> {
    base.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

fn set_field(base: &mut Vec<(&'static str, i64)>, key: &'static str, value: i64) {
    match base.iter_mut().find(|(k, _)| *k == key) {
        Some(slot) => slot.1 = value,
        None => base.push((key, value)),
    }
}

/// One `move` line for a keyframe gesture.
///
/// `keys` is the raw list a request carries (`req.get("keys")`): each key is
/// `[at_ms, yaw, pitch]` with an optional speed, every number read with
/// Python's `int()`. `None` means "use the preset". Only the first
/// [`MAX_KEYS`] keys are read.
pub fn keys_command(preset: Option<&str>, keys: Option<&[Value]>) -> Result<Command, MotionError> {
    let keys: Vec<Vec<Value>> = match keys {
        None => {
            let Some(rows) = preset.and_then(keys_preset) else {
                let shown = preset.map(py_repr_str).unwrap_or_else(|| "None".to_string());
                return Err(MotionError::Value(format!(
                    "{shown} is not a gesture; try {}",
                    keys_names().join(", ")
                )));
            };
            rows.iter().map(|k| k.iter().map(|n| json!(n)).collect()).collect()
        }
        Some(raw) => raw
            .iter()
            .take(MAX_KEYS)
            .map(|k| match k {
                Value::Array(items) => Ok(items.clone()),
                other => Err(MotionError::Type(format!(
                    "object of type '{}' has no len()",
                    py_type_name(other)
                ))),
            })
            .collect::<Result<_, _>>()?,
    };
    let mut rows: Vec<Value> = Vec::new();
    for k in keys.iter().take(MAX_KEYS) {
        if k.len() < 3 {
            return Err(MotionError::Value(
                "a key is [at_ms, yaw, pitch] with an optional speed".to_string(),
            ));
        }
        let (at, yaw, pitch) = (py_int(&k[0])?, py_int(&k[1])?, py_int(&k[2])?);
        let speed = if k.len() > 3 { py_int(&k[3])? } else { DEFAULT_KEY_SPEED };
        rows.push(json!([at, yaw, pitch, speed]));
    }
    if rows.is_empty() {
        return Err(MotionError::Value("no keys".to_string()));
    }
    let mut out = Command::new();
    out.insert("cmd".into(), json!("move"));
    out.insert("kind".into(), json!("keys"));
    out.insert("keys".into(), Value::Array(rows));
    Ok(out)
}

/// One `move` line for a keyframe gesture from integer rows (the CLI and
/// test path, where the keys are already numbers).
pub fn keys_command_rows(rows: &[Vec<i64>]) -> Result<Command, MotionError> {
    let raw: Vec<Value> = rows.iter().map(|r| json!(r)).collect();
    keys_command(None, Some(&raw))
}

pub fn stop_command() -> Command {
    let mut out = Command::new();
    out.insert("cmd".into(), json!("move"));
    out.insert("kind".into(), json!("stop"));
    out
}

/// What the host expects, before the board's own admission.
///
/// Only ever used to describe a request and to sanity-check it in tests. The
/// board is the authority and will reduce anything this gets wrong, which is why
/// nothing here enforces a limit.
pub fn predict(command: &Command) -> Result<Command, MotionError> {
    let mut out = Command::new();
    if command.get("kind").and_then(Value::as_str) == Some("keys") {
        let keys: &[Value] = match command.get("keys") {
            Some(v) if py_truthy(v) => match v {
                Value::Array(items) => items,
                other => {
                    return Err(MotionError::Type(format!(
                        "'{}' object is not iterable",
                        py_type_name(other)
                    )))
                }
            },
            _ => &[],
        };
        let mut last: i64 = 0;
        for (i, k) in keys.iter().enumerate() {
            let first = match k {
                Value::Array(items) => items
                    .first()
                    .ok_or_else(|| MotionError::Index("list index out of range".to_string()))?,
                other => {
                    return Err(MotionError::Type(format!(
                        "'{}' object is not subscriptable",
                        py_type_name(other)
                    )))
                }
            };
            let at = py_int(first)?;
            last = if i == 0 { at } else { last.max(at) };
        }
        out.insert("kind".into(), json!("keys"));
        out.insert("keys".into(), json!(keys.len()));
        out.insert("ms".into(), json!(last.saturating_add(600)));
        return Ok(out);
    }
    let int_or = |key: &str, default: i64| -> Result<i64, MotionError> {
        match command.get(key) {
            None => Ok(default),
            Some(v) => py_int(v),
        }
    };
    let cycles = int_or("cycles", 4)?;
    let period = int_or("period_ms", 900)?;
    let yaw = int_or("yaw_amp", 0)?;
    let pitch = int_or("pitch_amp", 0)?;
    let freq = if period != 0 { 1000.0 / period as f64 } else { 0.0 };
    out.insert("kind".into(), json!("osc"));
    out.insert("ms".into(), json!(cycles.saturating_mul(period)));
    out.insert("yaw_amp".into(), json!(yaw));
    out.insert("pitch_amp".into(), json!(pitch));
    out.insert("period_ms".into(), json!(period));
    // Peak speed of a sine at this amplitude and rate. The board's budget is
    // expressed the same way, so this is comparable to it.
    out.insert("peak_yaw_dps".into(), json!(py_round1(2.0 * std::f64::consts::PI * freq * yaw as f64)));
    out.insert(
        "peak_pitch_dps".into(),
        json!(py_round1(2.0 * std::f64::consts::PI * freq * pitch as f64)),
    );
    Ok(out)
}

/// One line for a log or a terminal.
pub fn describe(command: &Command) -> Result<String, MotionError> {
    let p = predict(command)?;
    let int = |k: &str| p.get(k).and_then(Value::as_i64).unwrap_or(0);
    let float = |k: &str| py_float_repr(p.get(k).and_then(Value::as_f64).unwrap_or(0.0));
    if p.get("kind").and_then(Value::as_str) == Some("keys") {
        return Ok(format!("{} keys over {} ms", int("keys"), int("ms")));
    }
    Ok(format!(
        "yaw {}° pitch {}° every {} ms for {} ms (peak {}/{} °/s asked)",
        int("yaw_amp"),
        int("pitch_amp"),
        int("period_ms"),
        int("ms"),
        float("peak_yaw_dps"),
        float("peak_pitch_dps"),
    ))
}

// ---------------------------------------------------------------------------
// Python semantics the module leans on.

/// Python's `type(v).__name__` for a JSON value.
fn py_type_name(v: &Value) -> &'static str {
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

/// Python's `bool(v)` for a JSON value.
fn py_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python's `int(v)` for a JSON value: ints as they are, floats truncated
/// toward zero, bools as 0/1, and strings of base-10 digits (surrounding
/// whitespace, a sign and single underscores between digits allowed).
pub fn py_int(v: &Value) -> Result<i64, MotionError> {
    match v {
        Value::Bool(b) => Ok(i64::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                return Ok(i);
            }
            if let Some(u) = n.as_u64() {
                return Ok(i64::try_from(u).unwrap_or(i64::MAX));
            }
            let f = n.as_f64().unwrap_or(0.0);
            if f.is_nan() {
                Err(MotionError::Value("cannot convert float NaN to integer".to_string()))
            } else if f.is_infinite() {
                Err(MotionError::Overflow("cannot convert float infinity to integer".to_string()))
            } else {
                Ok(f.trunc() as i64)
            }
        }
        Value::String(s) => parse_py_int_str(s).ok_or_else(|| {
            MotionError::Value(format!("invalid literal for int() with base 10: {}", py_repr_str(s)))
        }),
        other => Err(MotionError::Type(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            py_type_name(other)
        ))),
    }
}

fn parse_py_int_str(s: &str) -> Option<i64> {
    let t = s.trim();
    let (neg, digits) = match t.as_bytes().first()? {
        b'-' => (true, &t[1..]),
        b'+' => (false, &t[1..]),
        _ => (false, t),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let mut value: i64 = 0;
    for c in digits.chars() {
        if c == '_' {
            continue;
        }
        let d = i64::from(c.to_digit(10)?);
        value = value.saturating_mul(10).saturating_add(d);
    }
    Some(if neg { -value } else { value })
}

/// Python's `int(round(x))`: ties to even, NaN and infinity refused.
fn py_round_int(x: f64) -> Result<i64, MotionError> {
    if x.is_nan() {
        return Err(MotionError::Value("cannot convert float NaN to integer".to_string()));
    }
    if x.is_infinite() {
        return Err(MotionError::Overflow("cannot convert float infinity to integer".to_string()));
    }
    Ok(x.round_ties_even() as i64)
}

/// Python's `round(x, 1)`: the nearest one-decimal value, judged on the exact
/// binary value with ties to even. Rust's `{:.1}` formats exactly that way, and
/// parsing it back gives the same float Python returns.
fn py_round1(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.1}").parse().unwrap_or(x)
}

/// Python's `repr(float)`, which is also its `str` and f-string form.
fn py_float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".to_string();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    let a = x.abs();
    if a != 0.0 && !(1e-4..1e16).contains(&a) {
        // Shortest round-trip digits, exponent written the Python way.
        let e = format!("{x:e}");
        let (mant, exp) = e.split_once('e').unwrap_or((&e, "0"));
        let exp: i32 = exp.parse().unwrap_or(0);
        let sign = if exp < 0 { '-' } else { '+' };
        return format!("{mant}e{sign}{:02}", exp.abs());
    }
    let s = format!("{x}");
    if s.contains('.') {
        s
    } else {
        format!("{s}.0")
    }
}

/// Python's `repr(str)`.
pub fn py_repr_str(s: &str) -> String {
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
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (0x80..0xa0).contains(&(c as u32)) => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

#[cfg(test)]
mod tests;
