//! Idle explorer: when Claude has been quiet for a while, look around and take notes.
//!
//! Port of `eve_bridge/explore.py`.
//!
//! The robot spends most of its day watching the user. When no Claude session
//! has done anything for `EVE_EXPLORE_AFTER_MIN` minutes, nothing is
//! waiting on a card, and the listen key is up, the daemon puts the board in
//! explore mode and walks its head through a slow pan plan. At each waypoint,
//! once the head has settled, it keeps one camera frame; if that frame looks
//! different from the last one it kept at that waypoint (or it has never kept
//! one there), it spends a note: the frame goes to a vision model, and the
//! one-sentence answer lands in a dated Markdown file.
//!
//! Wire contract (fixed; the firmware side is built against it):
//!
//! ```text
//! host -> board  {"cmd":"look","yaw":<deg -60..60>,"pitch":<deg 5..85>,"hold":<ms>}
//! host -> board  {"cmd":"mode","explore":true|false}
//! ```
//!
//! Frames keep arriving as `{"frame":{...}}` lines (see eve-vision); the
//! explorer only reads them, it never asks for them.
//!
//! The owner can also send it off by hand: `eve-bridge explore` (IPC
//! `{"evt":"explore","action":"start"}`) or "hey eve, go explore" (the
//! voice tool `go_explore`) call [`Explorer::request`], which starts a pan at
//! once and marks the explore *manual*: the idle timer no longer applies, so
//! a running Claude session or a hook event does not end it. What does end a
//! manual explore is a hard sign that the human wants the robot back — the
//! listen key, a touch on the board, a new wake word, a disconnect — or an
//! explicit [`Explorer::dismiss`] (`explore stop`).
//!
//! Two halves, kept apart so the schedule and the budget are testable without
//! a board, a Mac, or a network:
//!
//! * Pure core — [`Explorer`] is a state machine driven by `tick(...)` with an
//!   injected clock. It returns actions (`Mode`, `Look`, `Note`) and never
//!   performs I/O. [`TokenBucket`] is the notes-per-hour budget and
//!   [`ChangeDetector`] the "did the view change" gate.
//! * Adapters — [`OpenAINoteClient`] (the network call), [`NoteTaker`] (runs it
//!   on a thread with a timeout and appends to the notes file), and the
//!   `notes` / `notes-test` CLI subcommands.
//!
//! Budget: one note is at most one request with one low-detail image and
//! [`NOTE_MAX_OUTPUT_TOKENS`] output tokens. The token bucket caps requests at
//! `EVE_NOTES_PER_HOUR`, and the change detector keeps the bucket from
//! being spent on a wall that has not moved.

#[cfg(target_os = "macos")]
mod mac;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use base64::Engine as _;
use chrono::NaiveDateTime;
use parking_lot::Mutex;
use serde_json::{json, Value};

use eve_spend::Ledger;
use eve_vision::{clamp, encode_gray_png, Frame, ValueError};

// ---- configuration -----------------------------------------------------------

pub const DEFAULT_AFTER_MIN: f64 = 10.0;
pub const DEFAULT_NOTES_PER_HOUR: f64 = 6.0;
pub const DEFAULT_MODEL: &str = "gpt-6-luna";
pub const DEFAULT_NOTES_DIR: &str = "~/.config/eve-bridge/notes";
pub const DEFAULT_CYCLE_MIN: f64 = 15.0;

// Pan plan: the whole room the neck can reach. Nine yaws across the servo's
// usable travel at a level gaze, then the same nine coming back while looking
// down at the desk — a serpentine, so no two consecutive waypoints are more
// than 30 degrees apart and the head never has to swing the full span in one
// hold. One `look` every LOOK_INTERVAL_SECS with the hold matching it, so
// the board never falls back to its own idle motion between waypoints.
//
// The span is the yaw servo's own: the BSP configures the Feetech SCS0009 at
// id 1 with angleLimit ±128 degrees in position mode, so ±120 is everything
// there is minus a margin. There is no full turn-around to be had — 360
// degrees needs continuous-rotation mode, which gives up position feedback
// and winds the neck loom.
//
// Four pitch rows, not two. The old plan used 40 and 60, both of which turned
// out to point over the desk at the ceiling: the firmware's PITCH_LEVEL of 45
// is only a guess, because level depends on how the head was zeroed (NVS
// `servo/zero_pos_2`). Bench, 2026-09-06: at pitch 40 eve wrote "I can see
// two recessed lights above", and at pitch 20 it still reported "most of frame
// is ceiling" with a standing person's head at the bottom edge. So the rows
// start at the neck's floor and climb in even steps across the usable band —
// whatever level turns out to be on a given head, some rows are below it.
// An even number of rows also means the serpentine ends where it began, so the
// next cycle opens without a long swing back.
pub const PAN_YAWS: [i64; 9] = [-120, -90, -60, -30, 0, 30, 60, 90, 120];
pub const PAN_PITCHES: [i64; 4] = [5, 25, 45, 65];
pub static WAYPOINTS: LazyLock<Vec<(i64, i64)>> = LazyLock::new(|| {
    let mut out = Vec::with_capacity(PAN_YAWS.len() * PAN_PITCHES.len());
    for (i, &pitch) in PAN_PITCHES.iter().enumerate() {
        if i % 2 == 0 {
            out.extend(PAN_YAWS.iter().map(|&yaw| (yaw, pitch)));
        } else {
            out.extend(PAN_YAWS.iter().rev().map(|&yaw| (yaw, pitch)));
        }
    }
    out
});
pub const LOOK_INTERVAL_SECS: f64 = 6.0;
pub const LOOK_HOLD_MS: i64 = 6000;
/// The head needs a moment to arrive; sample the frame this long after the look.
pub const SETTLE_SECS: f64 = 2.0;

pub const YAW_RANGE: (i64, i64) = (-120, 120);
pub const PITCH_RANGE: (i64, i64) = (5, 85);

// Change detector: mean absolute luma difference (0..255) between 32x24
// thumbnails. 12 is above sensor noise and compression shimmer on a static
// room, below a person walking in or the lights changing.
pub const THUMB_W: usize = 32;
pub const THUMB_H: usize = 24;
pub const CHANGE_THRESHOLD: f64 = 12.0;
// Per-waypoint thumbnail bank behind ChangeDetector::surprise: 30 frames is
// about three pan cycles, enough for a median and a spread per cell; below 5
// there is no opinion to give.
pub const BANK_SIZE: usize = 30;
pub const BANK_MIN: usize = 5;
// Added to every per-cell spread so a perfectly still cell cannot divide by
// zero and turn sensor noise into surprise. 6 luma units is the observed
// frame-to-frame noise of the GC0308 on a static scene.
pub const NOISE_FLOOR: f64 = 6.0;

pub const NOTE_TIMEOUT_SECS: f64 = 20.0;
/// reasoning plus the short visible note
pub const NOTE_MAX_OUTPUT_TOKENS: i64 = 1024;
pub const ERROR_LOG_INTERVAL_SECS: f64 = 10.0 * 60.0;

pub const PROMPT: &str = "You are a small desk robot looking around a room. In one sentence, note \
what you see that a curious pet would remember (objects, people count \
without identifying them, light, changes). No preamble.";

#[derive(Debug, Clone, PartialEq)]
pub struct ExploreConfig {
    /// the idle start; explore itself always works when asked
    pub enabled: bool,
    pub after_secs: f64,
    pub notes_per_hour: f64,
    pub model: String,
    pub notes_dir: PathBuf,
    pub cycle_wait_secs: f64,
}

impl Default for ExploreConfig {
    fn default() -> Self {
        ExploreConfig {
            enabled: false,
            after_secs: DEFAULT_AFTER_MIN * 60.0,
            notes_per_hour: DEFAULT_NOTES_PER_HOUR,
            model: DEFAULT_MODEL.to_string(),
            notes_dir: py_path(&eve_vision::expanduser(DEFAULT_NOTES_DIR)),
            cycle_wait_secs: DEFAULT_CYCLE_MIN * 60.0,
        }
    }
}

/// Python's `str.isspace` for one character: Rust's White_Space plus the
/// information separators U+001C..U+001F, which Python also treats as space.
fn py_is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn py_strip(s: &str) -> &str {
    s.trim_matches(py_is_space)
}

/// Python's `float(str)`: decimal literals with single underscores between
/// digits, `inf` / `infinity` / `nan` in any case, an optional sign.
pub(crate) fn py_float(raw: &str) -> Option<f64> {
    let s = py_strip(raw);
    let (sign, body) = match s.as_bytes().first() {
        Some(b'+') => (1.0, &s[1..]),
        Some(b'-') => (-1.0, &s[1..]),
        _ => (1.0, s),
    };
    let lower = body.to_ascii_lowercase();
    if lower == "inf" || lower == "infinity" {
        return Some(sign * f64::INFINITY);
    }
    if lower == "nan" {
        return Some(f64::NAN);
    }
    let bytes = body.as_bytes();
    let mut cleaned = String::with_capacity(body.len());
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-' => cleaned.push(b as char),
            b'_' => {
                let before = i > 0 && bytes[i - 1].is_ascii_digit();
                let after = bytes.get(i + 1).is_some_and(u8::is_ascii_digit);
                if !(before && after) {
                    return None;
                }
            }
            _ => return None,
        }
    }
    if cleaned.is_empty() || cleaned.starts_with(['+', '-']) {
        return None;
    }
    cleaned.parse::<f64>().ok().map(|v| sign * v)
}

/// Python's `repr(str)`: single quotes unless the text holds a single quote
/// and no double quote; backslash, the quote and control characters escaped.
pub(crate) fn py_repr_str(s: &str) -> String {
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

/// Python's `%g`: six significant digits, trailing zeros dropped, exponent
/// form below 1e-4 and from 1e6.
pub(crate) fn py_g(v: f64) -> String {
    if v.is_nan() {
        return "nan".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let sci = format!("{v:.5e}");
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    fn strip(s: &str) -> String {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s.to_string()
        }
    }
    if !(-4..6).contains(&exp) {
        format!("{}e{}{:02}", strip(mant), if exp < 0 { '-' } else { '+' }, exp.abs())
    } else {
        strip(&format!("{:.*}", (5 - exp) as usize, v))
    }
}

/// Python's `f"{v:.0f}"`: ties to even on the binary value, `inf` / `nan` spelled as Python does.
pub(crate) fn fmt0(v: f64) -> String {
    if v.is_nan() {
        return "nan".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "inf".into() } else { "-inf".into() };
    }
    format!("{v:.0}")
}

fn env_lookup(env: Option<&HashMap<String, String>>, name: &str) -> String {
    match env {
        Some(m) => m.get(name).cloned().unwrap_or_default(),
        None => std::env::var(name).unwrap_or_default(),
    }
}

fn env_float(name: &str, default: f64, env: Option<&HashMap<String, String>>) -> f64 {
    let value = env_lookup(env, name);
    let raw = py_strip(&value);
    if raw.is_empty() {
        return default;
    }
    let Some(v) = py_float(raw) else {
        tracing::warn!("explore: {}={} is not a number; using {}", name, py_repr_str(raw), py_g(default));
        return default;
    };
    if v < 0.0 {
        tracing::warn!("explore: {}={} is negative; using {}", name, py_repr_str(raw), py_g(default));
        return default;
    }
    v
}

/// `pathlib.Path(p)`'s normal form: repeated and trailing slashes dropped and
/// `.` segments removed (a lone `.` stays), so `EVE_NOTES_DIR=/tmp/notes/`
/// prints as `/tmp/notes` the way Python prints it.
pub(crate) fn py_path(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        if c != Component::CurDir {
            out.push(c.as_os_str());
        }
    }
    if out.as_os_str().is_empty() && !p.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// Build the config from EVE_EXPLORE* / EVE_NOTES* env vars. `None` reads the process environment.
pub fn configured(environ: Option<&HashMap<String, String>>) -> ExploreConfig {
    // Exploring is explicit — "go explore", the CLI, a text — never something eve starts on its own from
    // the idle clock (owner, 2026-09-21). EVE_EXPLORE=1 turns the idle start back on.
    let explore = env_lookup(environ, "EVE_EXPLORE");
    let explore = if explore.is_empty() { "0".to_string() } else { explore };
    let enabled = matches!(py_strip(&explore).to_lowercase().as_str(), "1" | "true" | "yes" | "on");
    let notes_dir_raw = env_lookup(environ, "EVE_NOTES_DIR");
    let notes_dir = match py_strip(&notes_dir_raw) {
        "" => DEFAULT_NOTES_DIR,
        s => s,
    };
    let model_raw = env_lookup(environ, "EVE_NOTES_MODEL");
    let model = match py_strip(&model_raw) {
        "" => DEFAULT_MODEL,
        s => s,
    };
    ExploreConfig {
        enabled,
        after_secs: env_float("EVE_EXPLORE_AFTER_MIN", DEFAULT_AFTER_MIN, environ) * 60.0,
        notes_per_hour: env_float("EVE_NOTES_PER_HOUR", DEFAULT_NOTES_PER_HOUR, environ),
        model: model.to_string(),
        notes_dir: py_path(&eve_vision::expanduser(notes_dir)),
        cycle_wait_secs: env_float("EVE_EXPLORE_CYCLE_MIN", DEFAULT_CYCLE_MIN, environ) * 60.0,
    }
}

// ---- actions (what the daemon executes) ---------------------------------------

/// Enter (true) or leave (false) explore mode. `reason` is for the log.
#[derive(Debug, Clone, PartialEq)]
pub struct Mode {
    pub explore: bool,
    pub reason: String,
}

impl Mode {
    pub fn new(explore: bool, reason: impl Into<String>) -> Self {
        Mode { explore, reason: reason.into() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    pub yaw: i64,
    pub pitch: i64,
    pub hold_ms: i64,
}

impl Look {
    /// `Look(yaw, pitch)` with the default hold.
    pub fn new(yaw: i64, pitch: i64) -> Self {
        Look { yaw, pitch, hold_ms: LOOK_HOLD_MS }
    }

    pub fn with_hold(yaw: i64, pitch: i64, hold_ms: i64) -> Self {
        Look { yaw, pitch, hold_ms }
    }
}

/// Spend one note on this frame, taken at this gaze.
///
/// `thumb`, `diff` and `surprise` are what the change detector already
/// worked out about the frame and used to discard; the diary's photo gate
/// (diary) needs them, so they ride along instead of being recomputed.
#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub frame: Frame,
    pub yaw: i64,
    pub pitch: i64,
    /// 32x24 luma, or None when undecodable
    pub thumb: Option<Vec<u8>>,
    /// mean abs luma diff vs the last kept frame here
    pub diff: Option<f64>,
    /// robust z against this waypoint's history
    pub surprise: Option<f64>,
}

impl Note {
    /// `Note(frame, yaw, pitch)` with no detector measurements.
    pub fn new(frame: Frame, yaw: i64, pitch: i64) -> Self {
        Note { frame, yaw, pitch, thumb: None, diff: None, surprise: None }
    }
}

/// A pan cycle finished. The board stays in explore mode and looks around
/// the room on its own until the next cycle; nothing goes over the wire.
#[derive(Debug, Clone, PartialEq)]
pub struct Rest {
    pub reason: String,
}

impl Rest {
    pub fn new(reason: impl Into<String>) -> Self {
        Rest { reason: reason.into() }
    }
}

/// `Action = Union[Mode, Look, Note, Rest]`.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Mode(Mode),
    Look(Look),
    Note(Note),
    Rest(Rest),
}

/// `Explorer::request` cannot start: the reason is the message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ExploreRefused(pub String);

pub fn build_look_cmd(yaw: f64, pitch: f64, hold_ms: i64) -> Value {
    json!({
        "cmd": "look",
        "yaw": clamp(yaw, YAW_RANGE.0, YAW_RANGE.1),
        "pitch": clamp(pitch, PITCH_RANGE.0, PITCH_RANGE.1),
        "hold": hold_ms.max(0),
    })
}

pub fn build_mode_cmd(explore: bool) -> Value {
    json!({"cmd": "mode", "explore": explore})
}

// ---- budget -------------------------------------------------------------------

/// `per_hour` tokens refill continuously; the bucket holds at most one
/// hour's worth and starts full, so a fresh daemon can note a whole first
/// cycle and then settles to the hourly rate.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenBucket {
    pub rate: f64,
    pub capacity: f64,
    pub tokens: f64,
    at: f64,
}

impl TokenBucket {
    pub fn new(per_hour: f64, now: f64) -> Self {
        Self::with_capacity(per_hour, now, None)
    }

    pub fn with_capacity(per_hour: f64, now: f64, capacity: Option<f64>) -> Self {
        let capacity = capacity.unwrap_or_else(|| py_max(1.0, per_hour));
        TokenBucket {
            rate: py_max(0.0, per_hour) / 3600.0,
            capacity,
            tokens: if per_hour > 0.0 { capacity } else { 0.0 },
            at: now,
        }
    }

    fn refill(&mut self, now: f64) {
        if now > self.at {
            self.tokens = py_min(self.capacity, self.tokens + (now - self.at) * self.rate);
        }
        self.at = now;
    }

    pub fn available(&mut self, now: f64) -> f64 {
        self.refill(now);
        self.tokens
    }

    pub fn take(&mut self, now: f64) -> bool {
        self.refill(now);
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

/// Python's `max(a, b)`: the first argument unless the second is greater (so NaN in `b` loses).
fn py_max(a: f64, b: f64) -> f64 {
    if b > a {
        b
    } else {
        a
    }
}

/// Python's `min(a, b)`: the first argument unless the second is smaller.
fn py_min(a: f64, b: f64) -> f64 {
    if b < a {
        b
    } else {
        a
    }
}

// ---- change detector ----------------------------------------------------------

/// Box-average an 8-bit luma image down to `tw` x `th`. Runs once per
/// waypoint on a 160x120 frame.
pub fn downscale_gray(w: i64, h: i64, luma: &[u8], tw: usize, th: usize) -> Result<Vec<u8>, ValueError> {
    if luma.len() as i128 != w as i128 * h as i128 {
        return Err(ValueError(format!("luma is {} bytes, expected {}", luma.len(), w as i128 * h as i128)));
    }
    if w < tw as i64 || h < th as i64 {
        return Err(ValueError(format!("cannot downscale {w}x{h} to {tw}x{th}")));
    }
    let (w, h) = (w as usize, h as usize);
    let mut out = vec![0u8; tw * th];
    for ty in 0..th {
        let y0 = ty * h / th;
        let y1 = (y0 + 1).max((ty + 1) * h / th);
        for tx in 0..tw {
            let x0 = tx * w / tw;
            let x1 = (x0 + 1).max((tx + 1) * w / tw);
            let mut total: u64 = 0;
            for y in y0..y1 {
                let row = y * w;
                total += luma[row + x0..row + x1].iter().map(|&v| v as u64).sum::<u64>();
            }
            out[ty * tw + tx] = (total / ((y1 - y0) * (x1 - x0)) as u64) as u8;
        }
    }
    Ok(out)
}

pub fn mean_abs_diff(a: &[u8], b: &[u8]) -> Result<f64, ValueError> {
    if a.len() != b.len() || a.is_empty() {
        return Err(ValueError("thumbnails differ in size".into()));
    }
    let total: u64 = a.iter().zip(b).map(|(&x, &y)| (x as i64 - y as i64).unsigned_abs()).sum();
    Ok(total as f64 / a.len() as f64)
}

/// A thumbnailer turns a frame into THUMB_W*THUMB_H luma bytes, or None when
/// it cannot decode the frame (a JPEG on a host without ImageIO).
pub type Thumbnailer = Arc<dyn Fn(&Frame) -> Option<Vec<u8>> + Send + Sync>;

/// Default thumbnailer: pure Rust for gray frames, ImageIO for JPEG.
pub fn frame_thumb(frame: &Frame) -> Option<Vec<u8>> {
    if frame.fmt == "gray" {
        return downscale_gray(frame.w, frame.h, &frame.data, THUMB_W, THUMB_H).ok();
    }
    quartz_thumb(&frame.data, THUMB_W, THUMB_H)
}

/// Decode any ImageIO-readable file and draw it into a tw x th gray bitmap.
/// None off macOS, as in Python.
pub fn quartz_thumb(image: &[u8], tw: usize, th: usize) -> Option<Vec<u8>> {
    #[cfg(target_os = "macos")]
    {
        mac::quartz_thumb(image, tw, th)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (image, tw, th);
        None
    }
}

/// Centre a thumbnail on mid-grey, so the sun going in or a lamp being
/// dimmed does not read as the room changing.
pub fn normalise(thumb: &[u8]) -> Vec<u8> {
    let mean = thumb.iter().map(|&v| v as f64).sum::<f64>() / thumb.len() as f64;
    thumb
        .iter()
        .map(|&v| {
            // int() truncates toward zero, then the clamp.
            let t = (v as f64 - mean + 128.0).trunc();
            t.clamp(0.0, 255.0) as u8
        })
        .collect()
}

/// How odd this thumbnail is against what this waypoint usually looks like.
///
/// Per cell: |value - median| / (1.4826 * MAD + noise floor), averaged over
/// the 768 cells. Around 1 for a typical view of a familiar spot, 4 and up
/// when something real has changed.
///
/// The per-cell MAD is what makes this safe to act on. A cell that always
/// flickers — a monitor, a fan, leaves in a window — has a large MAD, so its
/// contribution collapses towards zero: the "noisy TV" that defeats plain
/// prediction-error curiosity cannot hold eve's attention (Burda et al.
/// 2018, Random Network Distillation). A small change against a still
/// background, on the other hand, scores high, which is the isolation effect
/// the memory literature describes (von Restorff).
///
/// Panics on an empty bank or a banked thumbnail shorter than `thumb`, where
/// Python raises IndexError.
pub fn robust_z(thumb: &[u8], bank: &VecDeque<Vec<u8>>, noise_floor: f64) -> f64 {
    let cur = normalise(thumb);
    let n = cur.len();
    let mut total = 0.0;
    let mut col: Vec<i64> = Vec::with_capacity(bank.len());
    let mut dev: Vec<i64> = Vec::with_capacity(bank.len());
    for (c, &value) in cur.iter().enumerate() {
        col.clear();
        col.extend(bank.iter().map(|b| b[c] as i64));
        col.sort_unstable();
        let mid = col.len() / 2;
        let median = col[mid];
        dev.clear();
        dev.extend(col.iter().map(|&v| (v - median).abs()));
        dev.sort_unstable();
        let mad = dev[mid];
        total += (value as i64 - median).abs() as f64 / (1.4826 * mad as f64 + noise_floor);
    }
    total / n as f64
}

/// Per-waypoint memory of the last frame a note was spent on.
///
/// `measure` returns the mean absolute difference against that memory, or
/// None when there is nothing to compare with (first visit, or a frame the
/// host cannot decode) — the caller treats None as "changed". `keep`
/// records a frame as the new reference for its waypoint.
///
/// It also keeps a small bank of recent thumbnails per waypoint, which
/// `surprise` scores a new one against (see [`robust_z`]). The bank holds
/// every frame the explorer considered, not only the ones a note was spent
/// on, and a frame is scored *before* it is banked, so a view is surprising
/// exactly once and then becomes the new normal within a few looks.
#[derive(Clone)]
pub struct ChangeDetector {
    pub threshold: f64,
    pub thumb: Thumbnailer,
    pub bank_size: usize,
    pub bank_min: usize,
    refs: HashMap<usize, Vec<u8>>,
    banks: HashMap<usize, VecDeque<Vec<u8>>>,
}

impl std::fmt::Debug for ChangeDetector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChangeDetector")
            .field("threshold", &self.threshold)
            .field("bank_size", &self.bank_size)
            .field("bank_min", &self.bank_min)
            .field("refs", &self.refs.len())
            .finish()
    }
}

impl Default for ChangeDetector {
    fn default() -> Self {
        Self::with_options(CHANGE_THRESHOLD, Arc::new(frame_thumb), BANK_SIZE, BANK_MIN)
    }
}

impl ChangeDetector {
    pub fn new(threshold: f64) -> Self {
        Self::with_options(threshold, Arc::new(frame_thumb), BANK_SIZE, BANK_MIN)
    }

    pub fn with_thumb(thumb: Thumbnailer) -> Self {
        Self::with_options(CHANGE_THRESHOLD, thumb, BANK_SIZE, BANK_MIN)
    }

    pub fn with_options(threshold: f64, thumb: Thumbnailer, bank_size: usize, bank_min: usize) -> Self {
        ChangeDetector { threshold, thumb, bank_size, bank_min, refs: HashMap::new(), banks: HashMap::new() }
    }

    /// The thumbnailer applied to a frame.
    pub fn thumb_of(&self, frame: &Frame) -> Option<Vec<u8>> {
        (self.thumb)(frame)
    }

    pub fn measure(&self, key: usize, frame: &Frame) -> Option<f64> {
        let reference = self.refs.get(&key)?;
        let cur = (self.thumb)(frame)?;
        if cur.len() != reference.len() {
            return None;
        }
        mean_abs_diff(&cur, reference).ok()
    }

    pub fn keep(&mut self, key: usize, frame: &Frame) {
        match (self.thumb)(frame) {
            None => {
                self.refs.remove(&key);
            }
            Some(cur) => {
                self.refs.insert(key, cur);
            }
        }
    }

    /// Score this thumbnail against the waypoint's bank, then bank it.
    /// None while the bank is too small to have an opinion.
    pub fn surprise(&mut self, key: usize, thumb: Option<&[u8]>) -> Option<f64> {
        let thumb = thumb?;
        let bank_size = self.bank_size;
        let bank = self.banks.entry(key).or_default();
        let score = if bank.len() >= self.bank_min { Some(robust_z(thumb, bank, NOISE_FLOOR)) } else { None };
        // read before write: score, then learn (a deque with maxlen drops the oldest)
        if bank_size == 0 {
            return score;
        }
        if bank.len() >= bank_size {
            bank.pop_front();
        }
        bank.push_back(normalise(thumb));
        score
    }

    pub fn banked(&self, key: usize) -> usize {
        self.banks.get(&key).map_or(0, VecDeque::len)
    }
}

// ---- the state machine --------------------------------------------------------

/// The explorer's state: `"off"`, `"exploring"` or `"resting"` in `status()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Off,
    Exploring,
    Resting,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Off => "off",
            State::Exploring => "exploring",
            State::Resting => "resting",
        }
    }
}

impl std::fmt::Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The keyword arguments of `Explorer(...)` past `config` and `now`.
#[derive(Debug, Clone)]
pub struct ExplorerOptions {
    pub bucket: Option<TokenBucket>,
    pub detector: Option<ChangeDetector>,
    pub waypoints: Vec<(i64, i64)>,
    pub look_interval: f64,
    pub settle: f64,
    pub hold_ms: i64,
    pub notes_enabled: bool,
}

impl Default for ExplorerOptions {
    fn default() -> Self {
        ExplorerOptions {
            bucket: None,
            detector: None,
            waypoints: WAYPOINTS.clone(),
            look_interval: LOOK_INTERVAL_SECS,
            settle: SETTLE_SECS,
            hold_ms: LOOK_HOLD_MS,
            notes_enabled: true,
        }
    }
}

/// Pure schedule. `tick` it about once a second with the current facts
/// and execute whatever it returns.
///
/// States: OFF (watching the user) -> EXPLORING (walking the pan plan) ->
/// RESTING (cycle done, waiting `cycle_wait_secs` before the next one).
/// The board is in explore mode through both EXPLORING and RESTING: while
/// resting the host holds no `look`, so the firmware looks around the room
/// on its own. Any activity — `idle_secs` dropping below `after_secs`,
/// the listen key, a disconnect — sends it back to OFF from either state,
/// and that is when `mode explore false` goes to the board.
///
/// `request` (the owner asked) enters EXPLORING from OFF or RESTING at
/// once and sets `manual`: while manual, `idle_secs` is ignored, so only
/// the listen key, a disconnect or `dismiss` ends it. `manual` clears
/// on every return to OFF.
#[derive(Debug, Clone)]
pub struct Explorer {
    pub config: ExploreConfig,
    pub bucket: TokenBucket,
    pub detector: ChangeDetector,
    pub waypoints: Vec<(i64, i64)>,
    pub look_interval: f64,
    pub settle: f64,
    pub hold_ms: i64,
    pub notes_enabled: bool,
    pub state: State,
    /// the owner asked; the idle timer does not apply
    pub manual: bool,
    /// why the current explore began (for status)
    pub started_reason: String,
    /// completed pan cycles
    pub cycles: u64,
    /// Note actions emitted
    pub notes: u64,
    /// frames that matched the reference
    pub skipped_same: u64,
    /// changed frames the bucket refused
    pub skipped_budget: u64,
    wp: usize,
    look_at: f64,
    sampled: bool,
    rest_until: f64,
}

impl Explorer {
    pub const OFF: State = State::Off;
    pub const EXPLORING: State = State::Exploring;
    pub const RESTING: State = State::Resting;

    pub fn new(config: ExploreConfig, now: f64) -> Self {
        Self::with_options(config, now, ExplorerOptions::default())
    }

    pub fn with_options(config: ExploreConfig, now: f64, opts: ExplorerOptions) -> Self {
        let bucket = opts.bucket.unwrap_or_else(|| TokenBucket::new(config.notes_per_hour, now));
        Explorer {
            config,
            bucket,
            detector: opts.detector.unwrap_or_default(),
            waypoints: opts.waypoints,
            look_interval: opts.look_interval,
            settle: opts.settle,
            hold_ms: opts.hold_ms,
            notes_enabled: opts.notes_enabled,
            state: State::Off,
            manual: false,
            started_reason: String::new(),
            cycles: 0,
            notes: 0,
            skipped_same: 0,
            skipped_budget: 0,
            wp: 0,
            look_at: f64::NEG_INFINITY,
            sampled: false,
            rest_until: 0.0,
        }
    }

    pub fn active(&self) -> bool {
        self.state == State::Exploring
    }

    /// True while the board has been put in explore mode (panning or resting).
    pub fn on_board(&self) -> bool {
        matches!(self.state, State::Exploring | State::Resting)
    }

    pub fn waypoint(&self) -> (i64, i64) {
        self.waypoints[self.wp]
    }

    /// True while a frame for the current waypoint is due and not yet taken.
    pub fn wants_frame(&self, now: f64) -> bool {
        self.active() && !self.sampled && now - self.look_at >= self.settle
    }

    pub fn reset(&mut self) {
        self.state = State::Off;
        self.manual = false;
        self.sampled = false;
    }

    fn blocker(&self, idle_secs: f64, listening: bool, connected: bool) -> Option<&'static str> {
        if !connected {
            return Some("board disconnected");
        }
        if listening {
            return Some("listen key");
        }
        if idle_secs < self.config.after_secs && !self.manual {
            return Some("activity");
        }
        None
    }

    /// A snapshot for `eve-bridge explore status` and the log.
    pub fn status(&self) -> Value {
        let waypoint = if self.active() {
            let (yaw, pitch) = self.waypoint();
            json!([yaw, pitch])
        } else {
            Value::Null
        };
        json!({
            "state": self.state.as_str(),
            "manual": self.manual,
            "reason": self.started_reason,
            "waypoint": waypoint,
            "cycles": self.cycles,
            "notes": self.notes,
        })
    }

    /// The owner asked for an explore: start a pan now, ignoring idle time.
    ///
    /// From OFF the actions include `Mode(true)`; from RESTING the board is
    /// already in explore mode, so only the first `Look` goes out; while
    /// EXPLORING the current pan restarts from its first waypoint (no
    /// `Mode`). Refuses with [`ExploreRefused`] on a hard blocker.
    pub fn request(
        &mut self,
        now: f64,
        reason: &str,
        listening: bool,
        connected: bool,
    ) -> Result<Vec<Action>, ExploreRefused> {
        if let Some(blocker) = self.blocker(f64::INFINITY, listening, connected) {
            return Err(ExploreRefused(blocker.to_string()));
        }
        let was_on_board = self.on_board();
        self.manual = true;
        let actions = self.start(now, f64::INFINITY, Some(reason.to_string()));
        if was_on_board {
            return Ok(actions.into_iter().filter(|a| !matches!(a, Action::Mode(_))).collect());
        }
        Ok(actions)
    }

    /// End an explore (manual or idle) now. `Mode(false)` goes out only
    /// when the board was put in explore mode; `manual` always clears.
    pub fn dismiss(&mut self, reason: &str) -> Vec<Action> {
        let on_board = self.on_board();
        self.reset();
        if on_board {
            vec![Action::Mode(Mode::new(false, reason))]
        } else {
            vec![]
        }
    }

    pub fn tick(
        &mut self,
        now: f64,
        idle_secs: f64,
        listening: bool,
        frame: Option<&Frame>,
        connected: bool,
    ) -> Vec<Action> {
        let blocker = self.blocker(idle_secs, listening, connected);
        match self.state {
            State::Off => {
                if blocker.is_none() && self.config.enabled {
                    return self.start(now, idle_secs, None);
                }
                return vec![];
            }
            State::Resting => {
                if let Some(blocker) = blocker {
                    // The board is still in explore mode (looking around on its
                    // own); hand the head back to the persona.
                    self.reset();
                    return vec![Action::Mode(Mode::new(false, blocker))];
                }
                if now >= self.rest_until {
                    // A manual explore keeps its reason across cycles.
                    let reason = if self.manual { Some(self.started_reason.clone()) } else { None };
                    return self.start(now, idle_secs, reason);
                }
                return vec![];
            }
            State::Exploring => {}
        }
        if let Some(blocker) = blocker {
            self.reset();
            return vec![Action::Mode(Mode::new(false, blocker))];
        }
        let mut actions = Vec::new();
        if let Some(frame) = frame {
            if self.wants_frame(now) {
                self.sampled = true;
                actions.extend(self.consider(now, frame));
            }
        }
        if now - self.look_at >= self.look_interval {
            self.wp += 1;
            self.sampled = false;
            if self.wp >= self.waypoints.len() {
                self.cycles += 1;
                self.state = State::Resting;
                self.rest_until = now + self.config.cycle_wait_secs;
                actions.push(Action::Rest(Rest::new(format!(
                    "cycle complete, board looks around on its own; next pan in {} min",
                    fmt0(self.config.cycle_wait_secs / 60.0)
                ))));
            } else {
                self.look_at = now;
                let (yaw, pitch) = self.waypoint();
                actions.push(Action::Look(Look::with_hold(yaw, pitch, self.hold_ms)));
            }
        }
        actions
    }

    fn start(&mut self, now: f64, idle_secs: f64, reason: Option<String>) -> Vec<Action> {
        self.state = State::Exploring;
        self.wp = 0;
        self.look_at = now;
        self.sampled = false;
        self.started_reason = reason.unwrap_or_else(|| format!("idle {} min", fmt0(idle_secs / 60.0)));
        let (yaw, pitch) = self.waypoint();
        vec![
            Action::Mode(Mode::new(true, self.started_reason.clone())),
            Action::Look(Look::with_hold(yaw, pitch, self.hold_ms)),
        ]
    }

    fn consider(&mut self, now: f64, frame: &Frame) -> Vec<Action> {
        if !self.notes_enabled {
            return vec![];
        }
        // Every considered frame is measured and banked, spent or not: the
        // waypoint's idea of "usual" should include the boring views too.
        let thumb = self.detector.thumb_of(frame);
        let diff = self.detector.measure(self.wp, frame);
        let surprise = self.detector.surprise(self.wp, thumb.as_deref());
        if !(diff.is_none() || diff.is_some_and(|d| d > self.detector.threshold)) {
            self.skipped_same += 1;
            return vec![];
        }
        if !self.bucket.take(now) {
            self.skipped_budget += 1;
            return vec![];
        }
        self.detector.keep(self.wp, frame);
        self.notes += 1;
        let (yaw, pitch) = self.waypoint();
        vec![Action::Note(Note { frame: frame.clone(), yaw, pitch, thumb, diff, surprise })]
    }
}

// ---- note client (the network) ------------------------------------------------

/// Blocking. One sentence about the image. Errs on any failure.
pub trait NoteClient: Send + Sync {
    fn describe(&self, image: &[u8], mime: &str) -> anyhow::Result<String>;
}

/// Python's `RuntimeError`: what a note client raises for an empty answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct RuntimeError(pub String);

/// The bytes to send for a frame and their MIME type.
pub fn frame_image(frame: &Frame) -> Result<(Vec<u8>, &'static str), ValueError> {
    if frame.fmt == "jpeg" {
        return Ok((frame.data.clone(), "image/jpeg"));
    }
    Ok((encode_gray_png(frame.w, frame.h, &frame.data)?, "image/png"))
}

pub fn data_url(image: &[u8], mime: &str) -> String {
    format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(image))
}

/// The one call `OpenAINoteClient` makes: the Responses API's `responses.create`.
pub trait Responses: Send + Sync {
    fn create(&self, body: Value) -> anyhow::Result<Value>;
}

impl Responses for eve_openrouter::BlockingClient {
    fn create(&self, body: Value) -> anyhow::Result<Value> {
        Ok(self.responses_create(body)?)
    }
}

/// The OpenRouter blocking client, built on first use. reqwest's blocking
/// client panics when it is built on an async runtime thread, and the daemon
/// makes the note client from async code; the first `describe` runs on the
/// note taker's worker thread, where building it is allowed. The key is
/// checked up front, so a missing key still fails at construction as
/// `openrouter.client` raises `NoKey`.
struct LazyOpenRouter {
    api_key: String,
    opts: eve_openrouter::ClientOptions,
    client: std::sync::OnceLock<eve_openrouter::BlockingClient>,
}

impl Responses for LazyOpenRouter {
    fn create(&self, body: Value) -> anyhow::Result<Value> {
        let client = match self.client.get() {
            Some(c) => c,
            None => {
                let built = eve_openrouter::client(Some(&self.api_key), self.opts.clone())?;
                self.client.get_or_init(|| built)
            }
        };
        client.create(body)
    }
}

/// The openai SDK's `Response.output_text`: every `output_text` part of every `message` item, joined.
pub fn output_text(resp: &Value) -> String {
    let mut texts = String::new();
    for item in resp.get("output").and_then(Value::as_array).into_iter().flatten() {
        if item.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        for part in item.get("content").and_then(Value::as_array).into_iter().flatten() {
            if part.get("type").and_then(Value::as_str) == Some("output_text") {
                if let Some(t) = part.get("text").and_then(Value::as_str) {
                    texts.push_str(t);
                }
            }
        }
    }
    texts
}

/// `" ".join(text.split())`.
fn collapse_ws(text: &str) -> String {
    text.split(py_is_space).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// How the SDK's typed reply prints `status` and `incomplete_details` in an f-string.
fn py_status(resp: &Value) -> String {
    match resp.get("status") {
        Some(Value::String(s)) => s.clone(),
        None | Some(Value::Null) => "None".into(),
        Some(other) => other.to_string(),
    }
}

fn py_incomplete(resp: &Value) -> String {
    match resp.get("incomplete_details") {
        None | Some(Value::Null) => "None".into(),
        Some(Value::Object(m)) => {
            let reason = match m.get("reason") {
                Some(Value::String(s)) => py_repr_str(s),
                _ => "None".into(),
            };
            format!("IncompleteDetails(reason={reason})")
        }
        Some(other) => other.to_string(),
    }
}

/// Responses API, one `input_image` as a base64 data URL at low detail.
///
/// Low detail means a fixed small token cost per image regardless of its
/// size; `max_output_tokens` caps the answer; `reasoning.effort=low`
/// keeps a reasoning model from spending that cap on thinking. Retries are
/// off — a failed note is skipped, not retried into the budget.
pub struct OpenAINoteClient {
    pub model: String,
    client: Box<dyn Responses>,
    ledger: Ledger,
}

impl OpenAINoteClient {
    /// The OpenRouter client with `timeout` seconds and no retries. Errs with `NoKey` when no key is found.
    pub fn new(model: &str, api_key: Option<&str>, timeout: f64) -> Result<Self, eve_openrouter::Error> {
        let opts = eve_openrouter::ClientOptions::default()
            .timeout(Duration::try_from_secs_f64(timeout).unwrap_or(Duration::ZERO))
            .max_retries(0);
        Self::with_options(model, api_key, opts)
    }

    /// As `new`, with the client options given in full (tests point `base_url` at a local server).
    pub fn with_options(
        model: &str,
        api_key: Option<&str>,
        opts: eve_openrouter::ClientOptions,
    ) -> Result<Self, eve_openrouter::Error> {
        let api_key = eve_openrouter::key_or_raise(api_key, opts.env.as_ref())?;
        let lazy = LazyOpenRouter { api_key, opts, client: std::sync::OnceLock::new() };
        Ok(Self::with_responses(model, Box::new(lazy)))
    }

    /// Any `responses.create` (the Python tests swap `_client` for a fake SDK).
    pub fn with_responses(model: &str, client: Box<dyn Responses>) -> Self {
        OpenAINoteClient { model: model.to_string(), client, ledger: Ledger::default() }
    }

    /// Meter into this ledger instead of the configured spend folder.
    pub fn with_ledger(mut self, ledger: Ledger) -> Self {
        self.ledger = ledger;
        self
    }
}

impl NoteClient for OpenAINoteClient {
    fn describe(&self, image: &[u8], mime: &str) -> anyhow::Result<String> {
        let resp = self.client.create(json!({
            "model": self.model,
            "input": [{
                "role": "user",
                "content": [
                    {"type": "input_text", "text": PROMPT},
                    {"type": "input_image", "image_url": data_url(image, mime), "detail": "low"},
                ],
            }],
            "max_output_tokens": NOTE_MAX_OUTPUT_TOKENS,
            "reasoning": {"effort": "low"},
        }))?;
        self.ledger.record_response(eve_spend::EXPLORING, &resp, &self.model, eve_openrouter::PROVIDER, None);
        let text = collapse_ws(&output_text(&resp));
        if text.is_empty() {
            return Err(RuntimeError(format!(
                "empty answer (status={}, incomplete={})",
                py_status(&resp),
                py_incomplete(&resp)
            ))
            .into());
        }
        Ok(text)
    }
}

/// The real client, or None (with one warning) when notes cannot run. `None` env reads the process environment.
pub fn make_note_client(config: &ExploreConfig, environ: Option<&HashMap<String, String>>) -> Option<Arc<dyn NoteClient>> {
    let key = eve_openrouter::key(environ);
    if key.is_empty() {
        tracing::warn!(
            "explore: OPENROUTER_API_KEY not set — the robot will look around but take no notes \
             (put it in ~/.config/eve-bridge/env)"
        );
        return None;
    }
    match OpenAINoteClient::new(&config.model, Some(&key), NOTE_TIMEOUT_SECS) {
        Ok(c) => Some(Arc::new(c)),
        Err(e) => {
            tracing::warn!("explore: note client could not be built ({}) — no notes", e);
            None
        }
    }
}

/// Python's exception class name for an error out of a note client, as the log prints it.
pub fn error_name(e: &anyhow::Error) -> &'static str {
    if e.downcast_ref::<RuntimeError>().is_some() {
        return "RuntimeError";
    }
    if e.downcast_ref::<ValueError>().is_some() {
        return "ValueError";
    }
    if e.downcast_ref::<serde_json::Error>().is_some() {
        return "JSONDecodeError";
    }
    if e.downcast_ref::<std::io::Error>().is_some() {
        return "OSError";
    }
    if let Some(o) = e.downcast_ref::<eve_openrouter::Error>() {
        return match o {
            eve_openrouter::Error::NoKey(_) => "NoKey",
            eve_openrouter::Error::Status { status, .. } => match status {
                400 => "BadRequestError",
                401 => "AuthenticationError",
                403 => "PermissionDeniedError",
                404 => "NotFoundError",
                409 => "ConflictError",
                422 => "UnprocessableEntityError",
                429 => "RateLimitError",
                s if *s >= 500 => "InternalServerError",
                _ => "APIStatusError",
            },
            eve_openrouter::Error::Http(h) if h.is_timeout() => "APITimeoutError",
            eve_openrouter::Error::Http(_) => "APIConnectionError",
            eve_openrouter::Error::Json(_) => "APIResponseValidationError",
            eve_openrouter::Error::Invalid(_) => "ValueError",
        };
    }
    "Exception"
}

/// `str(e)` as the SDK would print it: its connection and timeout errors carry fixed messages.
pub fn error_text(e: &anyhow::Error) -> String {
    if let Some(eve_openrouter::Error::Http(h)) = e.downcast_ref::<eve_openrouter::Error>() {
        return if h.is_timeout() { "Request timed out.".into() } else { "Connection error.".into() };
    }
    e.to_string()
}

// ---- notes file ---------------------------------------------------------------

pub fn note_line(when: &NaiveDateTime, yaw: i64, pitch: i64, text: &str) -> String {
    format!("- {} yaw={:+} pitch={} — {}", when.format("%H:%M"), yaw, pitch, text)
}

pub fn notes_path(notes_dir: &Path, when: &NaiveDateTime) -> PathBuf {
    notes_dir.join(format!("{}.md", when.format("%Y-%m-%d")))
}

/// Append one line to today's file, and `extra` (a Markdown image line
/// for a photo) under it. The directory is created private (0700).
pub fn append_note(
    notes_dir: &Path,
    when: &NaiveDateTime,
    yaw: i64,
    pitch: i64,
    text: &str,
    extra: Option<&str>,
) -> std::io::Result<PathBuf> {
    if !notes_dir.exists() {
        std::fs::create_dir_all(notes_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(notes_dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    let path = notes_path(notes_dir, when);
    let mut buf = note_line(when, yaw, pitch, text);
    buf.push('\n');
    if let Some(extra) = extra.filter(|e| !e.is_empty()) {
        buf.push_str(extra);
        buf.push('\n');
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    f.write_all(buf.as_bytes())?;
    Ok(path)
}

/// Python's `str.splitlines()`.
fn py_splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let is_break = matches!(
            c,
            '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if !is_break {
            continue;
        }
        out.push(&s[start..i]);
        let mut end = i + c.len_utf8();
        if c == '\r' {
            if let Some(&(j, '\n')) = chars.peek() {
                chars.next();
                end = j + 1;
            }
        }
        start = end;
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Python's `seq[-n:]` for any sign of `n` (`n == 0` handled by the caller).
fn py_tail<T: Clone>(v: &[T], n: i64) -> Vec<T> {
    let len = v.len() as i64;
    let start = if n > 0 { (len - n).max(0) } else { (-n).min(len) };
    v[start as usize..].to_vec()
}

/// The most recent `last` note lines across dated files, oldest first.
/// `last=0` means all. Each line is prefixed with its date.
pub fn read_notes(notes_dir: &Path, last: i64) -> std::io::Result<Vec<String>> {
    if !notes_dir.is_dir() {
        return Ok(vec![]);
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(notes_dir)?
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_str().is_some_and(|n| n.ends_with(".md")))
        .map(|e| e.path())
        .collect();
    files.sort();
    let mut out: Vec<String> = Vec::new();
    for path in files.iter().rev() {
        let bytes = std::fs::read(path)?;
        let text = String::from_utf8(bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let day = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let mut chunk: Vec<String> = py_splitlines(&text)
            .into_iter()
            .filter(|ln| !py_strip(ln).is_empty())
            .map(|ln| format!("{day} {ln}"))
            .collect();
        chunk.append(&mut out);
        out = chunk;
        if last != 0 && out.len() as i64 >= last {
            break;
        }
    }
    Ok(if last != 0 { py_tail(&out, last) } else { out })
}

// ---- async side: run the call with a timeout, write the file ------------------

pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;
pub type Wall = Arc<dyn Fn() -> NaiveDateTime + Send + Sync>;

/// `take` was cancelled: `stop()` dropped the queued call before it ran
/// (Python's `CancelledError` out of the cancelled executor future).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("note cancelled")]
pub struct NoteCancelled;

type Job = Box<dyn FnOnce() + Send>;

/// The single-worker executor: one thread named `notes_0` running jobs in
/// order. Shutting it down (Python's `shutdown(wait=False,
/// cancel_futures=True)`) lets the running call finish and drops queued ones.
struct Executor {
    tx: std_mpsc::Sender<Job>,
    shut: Arc<AtomicBool>,
}

impl Executor {
    fn spawn() -> std::io::Result<Self> {
        let (tx, rx) = std_mpsc::channel::<Job>();
        let shut = Arc::new(AtomicBool::new(false));
        let flag = shut.clone();
        std::thread::Builder::new().name("notes_0".into()).spawn(move || {
            while let Ok(job) = rx.recv() {
                if flag.load(Ordering::SeqCst) {
                    drop(job);
                    continue;
                }
                job();
            }
        })?;
        Ok(Executor { tx, shut })
    }

    fn shutdown(self) {
        self.shut.store(true, Ordering::SeqCst);
    }
}

enum Failure {
    TimedOut,
    Raised(anyhow::Error),
}

/// Runs `NoteClient::describe` on a worker thread with a timeout and
/// appends the sentence to the notes file. Errors are logged at most once
/// per `ERROR_LOG_INTERVAL_SECS` and the note is skipped.
pub struct NoteTaker {
    pub client: Arc<dyn NoteClient>,
    pub notes_dir: PathBuf,
    pub timeout: f64,
    clock: Clock,
    wall: Wall,
    taken: AtomicU64,
    failed: AtomicU64,
    err_logged_at: Mutex<f64>,
    executor: Mutex<Option<Executor>>,
}

impl NoteTaker {
    /// Defaults: `NOTE_TIMEOUT_SECS`, the monotonic clock and local wall time.
    pub fn new(client: Arc<dyn NoteClient>, notes_dir: impl Into<PathBuf>) -> Self {
        NoteTaker {
            client,
            notes_dir: notes_dir.into(),
            timeout: NOTE_TIMEOUT_SECS,
            clock: Arc::new(eve_vision::monotonic),
            wall: Arc::new(|| chrono::Local::now().naive_local()),
            taken: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            err_logged_at: Mutex::new(f64::NEG_INFINITY),
            executor: Mutex::new(None),
        }
    }

    pub fn with_timeout(mut self, timeout: f64) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    pub fn with_wall(mut self, wall: Wall) -> Self {
        self.wall = wall;
        self
    }

    pub fn taken(&self) -> u64 {
        self.taken.load(Ordering::SeqCst)
    }

    pub fn failed(&self) -> u64 {
        self.failed.load(Ordering::SeqCst)
    }

    pub fn stop(&self) {
        if let Some(ex) = self.executor.lock().take() {
            ex.shutdown();
        }
    }

    fn submit(&self, job: Job) -> std::io::Result<()> {
        let mut guard = self.executor.lock();
        if guard.is_none() {
            *guard = Some(Executor::spawn()?);
        }
        let ex = guard.as_ref().expect("executor just set");
        if let Err(std_mpsc::SendError(job)) = ex.tx.send(job) {
            // The worker thread is gone (it can only end by panicking): start a fresh one.
            let fresh = Executor::spawn()?;
            let _ = fresh.tx.send(job);
            *guard = Some(fresh);
        }
        Ok(())
    }

    /// Describe the note's frame and append the sentence. `Ok(None)` is a
    /// skipped note (the call failed or timed out); `Err` is what Python lets
    /// escape `take`: a frame that cannot be encoded, a notes file that
    /// cannot be written, or [`NoteCancelled`] after `stop()`.
    pub async fn take(&self, note: &Note) -> anyhow::Result<Option<PathBuf>> {
        let (image, mime) = frame_image(&note.frame)?;
        let (tx, rx) = tokio::sync::oneshot::channel::<anyhow::Result<String>>();
        let client = self.client.clone();
        self.submit(Box::new(move || {
            // asyncio.wait_for cancels the executor future when it gives up (a
            // timeout, or the awaiting task cancelled), and a cancelled future
            // that has not started never runs: a call queued behind a slow one
            // is not made once nobody is waiting for it, so it costs nothing.
            if tx.is_closed() {
                return;
            }
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| client.describe(&image, mime)))
                .unwrap_or_else(|p| {
                    let msg = p
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| p.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "panic".into());
                    Err(RuntimeError(msg).into())
                });
            let _ = tx.send(result);
        }))?;
        // asyncio.wait_for: an infinite timeout waits for as long as it takes; zero or
        // less only takes an answer that is already there.
        let waited = if self.timeout == f64::INFINITY {
            Ok(rx.await)
        } else {
            let limit = Duration::try_from_secs_f64(self.timeout).unwrap_or(Duration::ZERO);
            tokio::time::timeout(limit, rx).await
        };
        let text = match waited {
            Err(_) => return Ok(self.fail(Failure::TimedOut)),
            Ok(Err(_dropped)) => return Err(NoteCancelled.into()),
            Ok(Ok(Err(e))) => return Ok(self.fail(Failure::Raised(e))),
            Ok(Ok(Ok(text))) => text,
        };
        let path = append_note(&self.notes_dir, &(self.wall)(), note.yaw, note.pitch, &text, None)?;
        self.taken.fetch_add(1, Ordering::SeqCst);
        tracing::info!("explore: note -> {}", path.display());
        Ok(Some(path))
    }

    fn fail(&self, failure: Failure) -> Option<PathBuf> {
        self.failed.fetch_add(1, Ordering::SeqCst);
        self.log_error(&failure);
        None
    }

    fn log_error(&self, failure: &Failure) {
        let now = (self.clock)();
        {
            let mut at = self.err_logged_at.lock();
            if now - *at < ERROR_LOG_INTERVAL_SECS {
                return;
            }
            *at = now;
        }
        let what = match failure {
            Failure::TimedOut => "timed out".to_string(),
            Failure::Raised(e) => format!("{}: {}", error_name(e), error_text(e)),
        };
        tracing::warn!(
            "explore: note failed — {} (further failures muted for {} min)",
            what,
            (ERROR_LOG_INTERVAL_SECS / 60.0).floor() as i64
        );
    }
}

impl Drop for NoteTaker {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---- CLI -----------------------------------------------------------------------

/// `eve-bridge notes [--last N]`: print recent notes.
pub fn run_notes_cli(last: i64, config: Option<&ExploreConfig>) -> i32 {
    run_notes_cli_to(last, config, &mut std::io::stdout(), &mut std::io::stderr())
}

/// `run_notes_cli` writing to the given streams.
pub fn run_notes_cli_to(last: i64, config: Option<&ExploreConfig>, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let owned;
    let cfg = match config {
        Some(c) => c,
        None => {
            owned = configured(None);
            &owned
        }
    };
    let lines = match read_notes(&cfg.notes_dir, last) {
        Ok(lines) => lines,
        Err(e) => {
            let _ = writeln!(err, "notes: {e}");
            return 1;
        }
    };
    if lines.is_empty() {
        let _ = writeln!(out, "no notes yet in {}", cfg.notes_dir.display());
        return 0;
    }
    for ln in lines {
        let _ = writeln!(out, "{ln}");
    }
    0
}

/// `str(OSError)` as Python prints it: `[Errno 2] No such file or directory: 'path'`.
fn py_os_error(e: &std::io::Error, path: &str) -> String {
    match e.raw_os_error() {
        Some(code) => {
            // SAFETY: strerror returns a valid NUL-terminated string.
            let msg = unsafe { std::ffi::CStr::from_ptr(libc::strerror(code)) }.to_string_lossy().into_owned();
            format!("[Errno {code}] {msg}: {}", py_repr_str(path))
        }
        None => e.to_string(),
    }
}

/// `eve-bridge notes-test <image>`: one real call, print the sentence. Blocking: run it off an async runtime thread.
pub fn run_notes_test(path: &str, config: Option<&ExploreConfig>) -> i32 {
    run_notes_test_with(
        path,
        config,
        |cfg| make_note_client(cfg, None),
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
}

/// `run_notes_test` with the client factory and the streams given.
pub fn run_notes_test_with(
    path: &str,
    config: Option<&ExploreConfig>,
    make_client: impl FnOnce(&ExploreConfig) -> Option<Arc<dyn NoteClient>>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let owned;
    let cfg = match config {
        Some(c) => c,
        None => {
            owned = configured(None);
            &owned
        }
    };
    let image = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            let _ = writeln!(err, "notes-test: cannot read {path}: {}", py_os_error(&e, path));
            return 2;
        }
    };
    let mime = if path.to_lowercase().ends_with(".png") { "image/png" } else { "image/jpeg" };
    let Some(client) = make_client(cfg) else {
        let _ = writeln!(err, "notes-test: no note client (OPENROUTER_API_KEY missing or openai not installed)");
        return 2;
    };
    let t0 = std::time::Instant::now();
    let text = match client.describe(&image, mime) {
        Ok(t) => t,
        Err(e) => {
            let _ = writeln!(err, "notes-test: {}: {}", error_name(&e), error_text(&e));
            return 1;
        }
    };
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    let _ = writeln!(out, "{} ({} ms): {}", cfg.model, fmt0(ms), text);
    0
}
