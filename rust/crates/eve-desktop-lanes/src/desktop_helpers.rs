//! On-device senses and effectors for the desktop worker: what the model's exec_py code gets besides raw
//! PyAutoGUI. Port of `desktop_helpers.py`; its module docstring is kept here in substance.
//!
//! Every helper is a plain method on [`Helpers`]; [`Helpers::install`] names the helpers bound into the worker's
//! exec namespace and turns on the "acting" wrap of the input calls ([`Helpers::gui_click`], `gui_hotkey`,
//! `gui_press`) so the worker knows when a call acted (it then appends a settled screenshot). Each helper returns a
//! value AND logs one short sentence (≤ 80 chars, never the typed text) that the human reads on the robot.
//!
//! Perception is local, so it costs 0.04–0.31 s instead of a model turn (measured 2026-09-06 on a 3024x1964
//! display):
//!
//! - [`cg_capture`]: CGDisplayCreateImage of the main display, 0.037 s, BGRA → an RGB image pixel-identical to
//!   pyautogui.screenshot() (0.114 s).
//! - [`vision_ocr`]: VNRecognizeTextRequest on that CGImage, accurate 0.31 s / fast 0.042 s (fast merges
//!   neighbouring words). Boxes are normalized with a bottom-left origin; `screen_text` maps them to click points.
//! - [`ax_frontmost`]: NSWorkspace frontmost app + the AX focused-window title.
//!
//! Settle de-duplication: open_app / open_url settle inside the helper, and the worker's auto-screenshot used to
//! settle again (0.3 s typical, 1.5 s worst). Every acted site bumps the action sequence; a settle attempt that
//! finished records (clock, seq), including a timeout on an animated screen; `settled_screenshot` reuses that frame
//! when no input happened since and it is under SETTLE_REUSE_SECS old. Reuse does not claim it settled.
//!
//! Timing: every sense and effector adds its wall time to `timing` (ms, reset by `begin()`), so the worker can
//! attach {"capture", "resize", "ocr", "ax", "settle", "act", "decide", ...} to each reply. The keys are not
//! disjoint: "settle" is a wall time that contains its own captures, which also count under "capture"; read "exec"
//! for the total and the others for where it went.
//!
//! The Quartz / Vision / AX backends are `#[cfg(target_os = "macos")]`; off the Mac they fail the way pyobjc's
//! missing import made the Python fail (an error, or None / 0 where the Python caught it).

use std::cell::{RefCell, RefMut};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, OnceLock};

use image::{DynamicImage, GrayImage, Rgb, RgbImage};
use indexmap::IndexMap;
use serde::Serialize;
use serde_json::{json, Map, Value};

use eve_decisions::Evaluate;

use crate::ax_candidates::{AxRef, Candidate, Rect, Snapshot};
use crate::decision_lane::DecisionSenses;
use crate::fast_lane::{Effectors, LaneDecider, Senses, StepAsk};
use crate::{casefold, error_kind, py_collapse, py_is_space, py_len, py_strip, py_take, LaneError};

pub const HELPER_NAMES: [&str; 12] = [
    "open_app", "open_url", "frontmost", "screen_text", "find_text", "click_text", "click_element", "wait_for",
    "wait_settled", "type_text", "zoom", "observe",
];
/// bound by install() only when the fast lane is on
pub const FAST_LANE_HELPERS: [&str; 1] = ["delegate"];
/// a settled frame younger than this, with no input since, is the reply's picture
pub const SETTLE_REUSE_SECS: f64 = 0.3;
/// Roles an AX hit-test may climb to before clicking (handyman's snap: coordinates only need to land INSIDE the
/// control; the control absorbs the model's error).
pub const AX_PRESSABLE_ROLES: &[&str] = &[
    "AXButton", "AXLink", "AXCheckBox", "AXRadioButton", "AXMenuItem", "AXMenuBarItem", "AXPopUpButton", "AXTab",
    "AXTextField", "AXTextArea", "AXComboBox", "AXCell", "AXRow", "AXImage", "AXStaticText", "AXDisclosureTriangle",
    "AXIncrementor", "AXSlider", "AXSearchField",
];
pub const AX_MAX_HOPS: usize = 6;
pub const ACTION_NAMES: [&str; 17] = [
    "click", "doubleClick", "rightClick", "middleClick", "mouseDown", "mouseUp", "dragTo", "dragRel", "moveTo",
    "scroll", "hscroll", "press", "hotkey", "keyDown", "keyUp", "write", "typewrite",
];
pub const BROWSER_BUNDLES: &[&str] = &[
    "com.apple.Safari", "com.google.Chrome", "org.mozilla.firefox", "company.thebrowser.Browser",
    "com.brave.Browser", "com.microsoft.edgemac",
];
/// every wait is clamped here: 30 s + 0.31 s OCR stays under the 60 s exec deadline
pub const MAX_WAIT_SECS: f64 = 30.0;
/// Change detection on a 1/4-scale grey thumbnail (one thumb pixel = 2x2 points on Retina). Measured 2026-09-10 on
/// a 3024x1964 frame: a 14x14-point checkbox toggle is ~63 changed thumb pixels here. A pixel-count floor replaces
/// the old fraction: a text caret stays under it, a checkbox clears it.
pub const THUMB_DIVISOR: u32 = 4;
/// a thumb pixel counts as changed above this grey-level difference
pub const CHANGE_PIXEL_DELTA: u8 = 24;
/// noise floor in thumb pixels: a caret is ~8-16, a checkbox ~50-60
pub const CHANGE_MIN_PIXELS: u32 = 24;
/// the menu-bar clock ticks every minute; that strip never counts
pub const MENU_BAR_POINTS: i64 = 40;
pub const SCREEN_TEXT_MAX_LINES: usize = 120;
pub const SCREEN_TEXT_MAX_BYTES: usize = 6 * 1024;
pub const ZOOM_MAX_POINTS: (i64, i64) = (800, 600);

// AXPress on the element a snapshot saw (computah's fallback, 2026-10-06). The lane clicks with the mouse after a
// hit-test says the candidate is under its centre. When the hit-test misses (an overlay, a tooltip, a label drawn
// by a child the hit-test climbs past) it refused, and the step escalated to the planner. computah
// (github.com/musubipapi/computah, MIT, AXActor.swift) presses the SAME native element its observation read,
// through its AXPress action, under the same target checks; a run never sends both for one action and never repeats
// an input whose effect is unknown. Here: the element comes from the snapshot's walk (Snapshot::element_ref), and is
// re-read now: same pid, enabled, AXPress offered, frame within 2 pt of the candidate's, and its own title, when it
// has one, the candidate's label.
pub const AX_PRESS_ENV: &str = "EVE_AX_PRESS";
/// it acts only where the lane would have refused; the lane itself ships off
pub const AX_PRESS_DEFAULT: bool = true;
/// points the element may have moved and still be the same control
pub const AX_PRESS_SLACK: i64 = 2;

/// Words of an expectation that say what kind of control it is, not which one (`_matches`).
const MATCH_FILLER: &[&str] = &["the", "button", "link", "icon", "field", "menu", "tab", "item"];
static MATCH_WORDS: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"[a-z0-9]+").expect("valid regex"));
/// The roles ax_focused_element calls editable.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const EDITABLE_AX_ROLES: &[&str] = &["AXTextField", "AXTextArea", "AXComboBox", "AXSearchField"];

// ---- Python formatting --------------------------------------------------------------------------------

/// Python's `str.isprintable()` for one character, without the Unicode tables: control, format, separator and
/// private-use characters are escaped; unassigned code points are not detected.
fn py_printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    if c.is_control() {
        return false;
    }
    let u = c as u32;
    let separator = matches!(u, 0xA0 | 0x1680 | 0x2000..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000);
    let format = matches!(
        u,
        0xAD | 0x600..=0x605
            | 0x61C
            | 0x6DD
            | 0x70F
            | 0x890..=0x891
            | 0x8E2
            | 0x180E
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    );
    let private = matches!(u, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD);
    !(separator || format || private)
}

/// Python's `repr(str)`: single quotes unless the text holds a single quote and no double quote.
pub(crate) fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if py_printable(c) => out.push(c),
            c if (c as u32) < 0x100 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) < 0x10000 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push(quote);
    out
}

/// Python's `f"{v:g}"`: six significant digits, trailing zeros dropped, scientific below 1e-4 and from 1e6.
pub(crate) fn py_g(v: f64) -> String {
    if v.is_nan() {
        return "nan".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 { "inf".to_string() } else { "-inf".to_string() };
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0".to_string() } else { "0".to_string() };
    }
    let sci = format!("{v:.5e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let strip = |s: &str| -> String {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s.to_string()
        }
    };
    if !(-4..6).contains(&exp) {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{}{:02}", strip(mantissa), sign, exp.abs())
    } else {
        let decimals = (5 - exp).max(0) as usize;
        strip(&format!("{v:.decimals$}"))
    }
}

/// Python's `round(x)` on a float: half to even, as an int.
fn py_round(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// Python's `round(x, n)` on a float.
fn py_round_to(x: f64, n: i32) -> f64 {
    crate::py_round_n(x, n.max(0) as usize)
}

/// Python's `int(s)` of a string: surrounding whitespace, a sign, and single underscores between digits allowed.
fn py_int(s: &str) -> Option<i64> {
    let t = py_strip(s);
    let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
    let valid = !digits.is_empty()
        && !digits.starts_with('_')
        && !digits.ends_with('_')
        && !digits.contains("__")
        && digits.chars().all(|c| c.is_ascii_digit() || c == '_');
    if !valid {
        return None;
    }
    t.replace('_', "").parse::<i64>().ok()
}

/// Python's `str(tuple)` of a frame: "(x, y, w, h)".
fn tuple_repr(r: &Rect) -> String {
    format!("({}, {}, {}, {})", r.0, r.1, r.2, r.3)
}

/// Python's `str(v)` of a JSON value, for the few places a dict field is stringified.
fn py_str_value(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Python's `str(v or default)` of a JSON value.
fn py_str_or(v: Option<&Value>, default: &str) -> String {
    let truthy = match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    };
    match v {
        Some(v) if truthy => py_str_value(v),
        _ => default.to_string(),
    }
}

// ---- data -----------------------------------------------------------------------------------------

/// A CGImageRef of one capture (macOS); off the Mac there is none.
#[derive(Clone)]
pub struct CgImage(
    #[cfg(target_os = "macos")] pub objc2_core_foundation::CFRetained<objc2_core_graphics::CGImage>,
    #[cfg(not(target_os = "macos"))] pub (),
);

// SAFETY: a CGImage is immutable once created and Core Graphics documents it as thread-safe; the reference count is
// atomic.
unsafe impl Send for CgImage {}
unsafe impl Sync for CgImage {}

impl std::fmt::Debug for CgImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CgImage")
    }
}

/// One physical-pixel capture of the screen (2x on Retina).
#[derive(Debug)]
pub struct Frame {
    /// the CGImageRef, or None in tests
    pub cg: Option<CgImage>,
    pub width: u32,
    pub height: u32,
    pil: OnceLock<RgbImage>,
    thumb: OnceLock<GrayImage>,
}

impl Frame {
    pub fn new(cg: Option<CgImage>, width: u32, height: u32) -> Self {
        Frame { cg, width, height, pil: OnceLock::new(), thumb: OnceLock::new() }
    }

    /// Python `Frame.from_pil(img)`.
    pub fn from_pil(img: RgbImage) -> Self {
        let (width, height) = img.dimensions();
        let pil = OnceLock::new();
        let _ = pil.set(img);
        Frame { cg: None, width, height, pil, thumb: OnceLock::new() }
    }

    /// The frame as an RGB image (cached): the CGImage's BGRA rows, alpha dropped.
    pub fn pil(&self) -> anyhow::Result<&RgbImage> {
        if let Some(img) = self.pil.get() {
            return Ok(img);
        }
        let img = self.decode()?;
        Ok(self.pil.get_or_init(|| img))
    }

    #[cfg(target_os = "macos")]
    fn decode(&self) -> anyhow::Result<RgbImage> {
        let Some(cg) = &self.cg else {
            return Err(LaneError::err("TypeError", "the frame has no CGImage"));
        };
        let (data, bytes_per_row) = mac::image_bytes(&cg.0)
            .ok_or_else(|| LaneError::err("RuntimeError", "the CGImage has no data"))?;
        bgra_to_rgb(&data, self.width, self.height, bytes_per_row)
    }

    #[cfg(not(target_os = "macos"))]
    fn decode(&self) -> anyhow::Result<RgbImage> {
        Err(LaneError::err("ModuleNotFoundError", "No module named 'Quartz'"))
    }

    /// 1/4-scale grayscale, for change detection (~5 ms on a 3024x1964 frame); Pillow's `convert("L").resize`.
    pub fn thumb(&self) -> anyhow::Result<&GrayImage> {
        if let Some(t) = self.thumb.get() {
            return Ok(t);
        }
        let (w, h) = ((self.width / THUMB_DIVISOR).max(1), (self.height / THUMB_DIVISOR).max(1));
        let grey = to_luma(self.pil()?);
        let t = image::imageops::resize(&grey, w, h, image::imageops::FilterType::CatmullRom);
        Ok(self.thumb.get_or_init(|| t))
    }
}

/// `Image.frombuffer("RGBA", size, data, "raw", "BGRA", bytes_per_row, 1).convert("RGB")`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn bgra_to_rgb(data: &[u8], width: u32, height: u32, bytes_per_row: usize) -> anyhow::Result<RgbImage> {
    let (w, h) = (width as usize, height as usize);
    let needed = if h == 0 { 0 } else { bytes_per_row * (h - 1) + w * 4 };
    if bytes_per_row < w * 4 || data.len() < needed {
        return Err(LaneError::err("ValueError", "buffer is not large enough"));
    }
    let mut out = RgbImage::new(width, height);
    for y in 0..h {
        let row = &data[y * bytes_per_row..y * bytes_per_row + w * 4];
        for (x, px) in row.chunks_exact(4).enumerate() {
            out.put_pixel(x as u32, y as u32, Rgb([px[2], px[1], px[0]]));
        }
    }
    Ok(out)
}

/// Pillow's RGB → "L": ITU-R 601-2 luma with Pillow's fixed-point rounding.
fn to_luma(img: &RgbImage) -> GrayImage {
    let (w, h) = img.dimensions();
    let mut out = GrayImage::new(w, h);
    for (x, y, px) in img.enumerate_pixels() {
        let [r, g, b] = px.0;
        let l = (u32::from(r) * 19595 + u32::from(g) * 38470 + u32::from(b) * 7471 + 0x8000) >> 16;
        out.put_pixel(x, y, image::Luma([l.min(255) as u8]));
    }
    out
}

/// Pillow's `img.crop(box)`: the area outside the image comes back black.
fn crop_padded(img: &RgbImage, x0: i64, y0: i64, x1: i64, y1: i64) -> RgbImage {
    let (w, h) = ((x1 - x0).max(0) as u32, (y1 - y0).max(0) as u32);
    let mut out = RgbImage::new(w, h);
    let (iw, ih) = (i64::from(img.width()), i64::from(img.height()));
    for oy in 0..h {
        let sy = y0 + i64::from(oy);
        if sy < 0 || sy >= ih {
            continue;
        }
        for ox in 0..w {
            let sx = x0 + i64::from(ox);
            if sx < 0 || sx >= iw {
                continue;
            }
            out.put_pixel(ox, oy, *img.get_pixel(sx as u32, sy as u32));
        }
    }
    out
}

/// `{"app", "bundle", "title", "pid"}` of the frontmost app (Python's EMPTY_FRONT is `Front::default()`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Front {
    pub app: String,
    pub bundle: String,
    pub title: String,
    pub pid: i64,
}

/// One Vision observation: normalized box, bottom-left origin.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OcrBox {
    pub text: String,
    pub bx: f64,
    pub by: f64,
    pub bw: f64,
    pub bh: f64,
    pub conf: f64,
}

/// One screen_text line: `{"text", "x", "y", "w", "h", "conf"}` in click coordinates.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TextLine {
    pub text: String,
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
    pub conf: f64,
}

/// find_text's hit: the line plus its centre `cx`, `cy`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FoundText {
    #[serde(flatten)]
    pub line: TextLine,
    pub cx: i64,
    pub cy: i64,
}

/// What wait_for answers: Python's True / False (gone=True) and the item / None (gone=False).
#[derive(Debug, Clone, PartialEq)]
pub enum Waited {
    /// gone=True: the text went away
    Gone,
    /// gone=True: still on screen at the timeout
    StillThere,
    /// gone=False: the text appeared
    Found(FoundText),
    /// gone=False: not seen before the timeout
    NotSeen,
}

/// ax_element_at's answer: `{"role", "title", "frame", "pressable", "app"}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AxElementInfo {
    pub role: String,
    pub title: String,
    pub frame: Option<Rect>,
    pub pressable: bool,
    pub app: String,
}

/// ax_focused_element's answer: `{"role", "title", "frame", "app", "editable"}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FocusedElement {
    pub role: String,
    pub title: String,
    pub frame: Option<Rect>,
    pub app: String,
    pub editable: bool,
}

/// ax_read's answer: `{"role", "title", "frame", "enabled", "pid", "actions"}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LiveElement {
    pub role: String,
    pub title: String,
    pub frame: Option<Rect>,
    pub enabled: bool,
    pub pid: i64,
    pub actions: Vec<String>,
}

/// PyAutoGUI, as far as the helpers use it (desktop_worker supplies the real one).
pub trait Gui {
    /// The screen in points.
    fn size(&self) -> (i64, i64);
    fn click(&mut self, x: i64, y: i64, clicks: u32) -> anyhow::Result<()>;
    fn hotkey(&mut self, keys: &[&str]) -> anyhow::Result<()>;
    fn press(&mut self, key: &str) -> anyhow::Result<()>;
}

/// pyperclip, as far as type_text uses it.
pub trait Clipboard {
    fn paste(&mut self) -> anyhow::Result<String>;
    fn copy(&mut self, text: &str) -> anyhow::Result<()>;
}

/// The real clipboard (pyperclip → arboard).
#[derive(Debug, Default)]
pub struct SystemClipboard;

impl Clipboard for SystemClipboard {
    fn paste(&mut self) -> anyhow::Result<String> {
        let mut board = arboard::Clipboard::new().map_err(|e| LaneError::err("PyperclipException", e.to_string()))?;
        board.get_text().map_err(|e| LaneError::err("PyperclipException", e.to_string()))
    }

    fn copy(&mut self, text: &str) -> anyhow::Result<()> {
        let mut board = arboard::Clipboard::new().map_err(|e| LaneError::err("PyperclipException", e.to_string()))?;
        board.set_text(text.to_string()).map_err(|e| LaneError::err("PyperclipException", e.to_string()))
    }
}

/// `capture()` → one frame.
pub type CaptureFn = Box<dyn Fn() -> anyhow::Result<Frame> + Send>;
/// `ocr(frame, level)`.
pub type OcrFn = Box<dyn Fn(&Frame, &str) -> anyhow::Result<Vec<OcrBox>> + Send>;
/// `element_at(x, y)`.
pub type ElementAtFn = Box<dyn Fn(i64, i64) -> anyhow::Result<Option<AxElementInfo>> + Send>;
/// `element_read(ref)`.
pub type ElementReadFn = Box<dyn Fn(&AxRef) -> anyhow::Result<Option<LiveElement>> + Send>;
/// `element_press(ref)` → the AXError code.
pub type ElementPressFn = Box<dyn Fn(&AxRef) -> anyhow::Result<i64> + Send>;
/// `ax_snapshot(screen)`.
pub type SnapshotFn = Box<dyn Fn(Option<(i64, i64)>) -> anyhow::Result<Snapshot> + Send>;
/// `run(argv, timeout)` → the return code.
pub type RunFn = Box<dyn Fn(&[String], f64) -> anyhow::Result<i32> + Send>;

/// Every backend of a Helpers, injectable for the tests (Python's keyword arguments to `Helpers(...)`).
/// `Backends::default()` is the real Mac.
pub struct Backends {
    pub capture: CaptureFn,
    /// `ocr(frame, level)`, level "accurate" | "fast"
    pub ocr: OcrFn,
    pub frontmost_fn: Box<dyn Fn() -> Front + Send>,
    pub element_at: ElementAtFn,
    pub focused_element: Box<dyn Fn() -> Option<FocusedElement> + Send>,
    pub element_read: ElementReadFn,
    pub element_press: ElementPressFn,
    /// The AX snapshot the lane adapter takes (Python `ax_snapshot(None, screen=...)`), given the screen in points.
    pub ax_snapshot: SnapshotFn,
    /// `subprocess.run(argv, capture_output=True, timeout=…)` → the return code
    pub run: RunFn,
    /// None: the system clipboard, made on first use
    pub clipboard: Option<Box<dyn Clipboard + Send>>,
    pub clock: Box<dyn Fn() -> f64 + Send>,
    pub sleep: Box<dyn Fn(f64) + Send>,
    pub now: Box<dyn Fn() -> chrono::NaiveDateTime + Send>,
    pub menu_bar_points: i64,
    pub fast_lane: bool,
    /// `model` mode's decider, or None
    pub decider: Option<Box<dyn LaneDecider + Send>>,
}

impl Default for Backends {
    fn default() -> Self {
        Backends {
            capture: Box::new(cg_capture),
            ocr: Box::new(vision_ocr),
            frontmost_fn: Box::new(ax_frontmost),
            element_at: Box::new(|x, y| Ok(ax_element_at(x, y))),
            focused_element: Box::new(ax_focused_element),
            element_read: Box::new(|el| Ok(ax_read(el))),
            element_press: Box::new(ax_press),
            ax_snapshot: Box::new(|screen| {
                Ok(crate::ax_candidates::ax_snapshot(
                    None,
                    crate::ax_candidates::DEFAULT_MAX_NODES,
                    crate::ax_candidates::DEFAULT_BUDGET_SECS,
                    None,
                    screen,
                ))
            }),
            run: Box::new(run_subprocess),
            clipboard: None,
            // time.monotonic
            clock: Box::new(crate::perf_counter),
            sleep: Box::new(|secs| {
                if secs > 0.0 && secs.is_finite() {
                    std::thread::sleep(std::time::Duration::from_secs_f64(secs));
                }
            }),
            now: Box::new(|| chrono::Local::now().naive_local()),
            menu_bar_points: MENU_BAR_POINTS,
            fast_lane: false,
            decider: None,
        }
    }
}

/// `subprocess.run(argv, capture_output=True, timeout=timeout).returncode`: output captured and dropped,
/// TimeoutExpired (the child killed) past `timeout`, a negative signal number for a child killed by a signal.
fn run_subprocess(argv: &[String], timeout: f64) -> anyhow::Result<i32> {
    use std::io::Read;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let Some((program, args)) = argv.split_first() else {
        return Err(LaneError::err("IndexError", "list index out of range"));
    };
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            let kind = if e.kind() == std::io::ErrorKind::NotFound { "FileNotFoundError" } else { "OSError" };
            LaneError::err(kind, format!("{e}: {}", py_repr(program)))
        })?;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            if let Some(mut p) = pipe {
                let mut sink = Vec::new();
                let _ = p.read_to_end(&mut sink);
            }
        })
    };
    let out = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let deadline = Instant::now() + Duration::from_secs_f64(timeout.max(0.0));
    loop {
        if let Some(status) = child.try_wait()? {
            let _ = out.join();
            let _ = err.join();
            return Ok(status.code().unwrap_or_else(|| -status.signal().unwrap_or(0)));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = out.join();
            let _ = err.join();
            let shown: Vec<String> = argv.iter().map(|a| py_repr(a)).collect();
            return Err(LaneError::err(
                "TimeoutExpired",
                format!("Command '[{}]' timed out after {timeout} seconds", shown.join(", ")),
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ---- free functions ---------------------------------------------------------------------------------

/// Where two thumbnails differ: the bounding box (x, y, w, h) in thumb pixels, or None when fewer than
/// `min_pixels` changed. Rows above `skip_top` never count. Thumbnails of different sizes differ everywhere.
pub fn change_box(a_thumb: &GrayImage, b_thumb: &GrayImage, skip_top: u32, min_pixels: u32) -> Option<(u32, u32, u32, u32)> {
    if a_thumb.dimensions() != b_thumb.dimensions() {
        return Some((0, 0, b_thumb.width(), b_thumb.height()));
    }
    let (w, h) = a_thumb.dimensions();
    let mut count: u32 = 0;
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for y in skip_top.min(h)..h {
        for x in 0..w {
            let a = a_thumb.get_pixel(x, y).0[0];
            let b = b_thumb.get_pixel(x, y).0[0];
            if a.abs_diff(b) > CHANGE_PIXEL_DELTA {
                count += 1;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    if count < min_pixels || count == 0 {
        return None;
    }
    Some((x0, y0, x1 - x0, y1 - y0))
}

/// The app that owns a process, "" when unknown. AppKit first (0.3 ms), then libproc (0.02 ms).
pub fn app_name_for_pid(pid: i64) -> String {
    #[cfg(target_os = "macos")]
    {
        if let Ok(p) = i32::try_from(pid) {
            if let Some((name, _)) = crate::ax_ffi::running_app(p) {
                if !name.is_empty() {
                    return name;
                }
            }
            let mut buf = [0u8; 256];
            // SAFETY: the buffer is 256 writable bytes, the size passed.
            let n = unsafe { libc::proc_name(p, buf.as_mut_ptr().cast(), 256) };
            if n > 0 {
                let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                return String::from_utf8_lossy(&buf[..end]).into_owned();
            }
        }
        String::new()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        String::new()
    }
}

/// Python `_what`: "AXButton 'Play' in Spotify" — the role, the title (Python repr) when there is one, and the
/// owning app.
pub fn what(el: &AxElementInfo) -> String {
    let role = if el.role.is_empty() { "element" } else { el.role.as_str() };
    let text = if el.title.is_empty() { role.to_string() } else { format!("{role} {}", py_repr(&el.title)) };
    if el.app.is_empty() {
        text
    } else {
        format!("{text} in {}", el.app)
    }
}

/// The main display as a CGImage (physical pixels). Errors "screen capture failed — Screen Recording is not granted
/// to this python" when the capture comes back empty.
pub fn cg_capture() -> anyhow::Result<Frame> {
    #[cfg(target_os = "macos")]
    {
        let Some(cg) = mac::capture_main_display() else {
            return Err(LaneError::err(
                "RuntimeError",
                "screen capture failed — Screen Recording is not granted to this python",
            ));
        };
        let (w, h) = mac::image_size(&cg);
        Ok(Frame::new(Some(CgImage(cg)), w as u32, h as u32))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(LaneError::err("ModuleNotFoundError", "No module named 'Quartz'"))
    }
}

/// Vision text recognition. Boxes are normalized, bottom-left origin: bx, by, bw, bh.
pub fn vision_ocr(frame: &Frame, level: &str) -> anyhow::Result<Vec<OcrBox>> {
    #[cfg(target_os = "macos")]
    {
        let Some(cg) = &frame.cg else {
            return Err(LaneError::err("RuntimeError", "vision_ocr needs a CGImage frame"));
        };
        mac::recognize_text(&cg.0, level == "fast")
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (frame, level);
        Err(LaneError::err("ModuleNotFoundError", "No module named 'Vision'"))
    }
}

/// Python `_ax_focused_pid`: the app that has keyboard focus, from AX — where a key press actually goes. 0 on error
/// (kAXErrorCannotComplete, -25204, was seen once while a terminal held focus).
pub fn ax_focused_pid() -> i64 {
    #[cfg(target_os = "macos")]
    {
        use crate::ax_ffi::{AxAttr, AxElement};
        match AxElement::system_wide().attribute("AXFocusedApplication") {
            Ok(AxAttr::Element(app)) => match app.pid() {
                Ok(pid) if pid != 0 => i64::from(pid),
                _ => 0,
            },
            _ => 0,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        0
    }
}

/// Python `_window_list_pid`: the owner of the frontmost normal (layer 0) on-screen window. 0 when there is none.
pub fn window_list_pid() -> i64 {
    #[cfg(target_os = "macos")]
    {
        mac::window_list_pid()
    }
    #[cfg(not(target_os = "macos"))]
    {
        0
    }
}

/// Python `_workspace_pid`: NSWorkspace's idea of the frontmost app — last resort only, see `focused_pid`.
pub fn workspace_pid() -> i64 {
    #[cfg(target_os = "macos")]
    {
        i64::from(crate::ax_ffi::workspace_frontmost_pid())
    }
    #[cfg(not(target_os = "macos"))]
    {
        0
    }
}

/// Python `_focused_pid`: the frontmost app's pid, from sources that stay current in a long-lived process (AX,
/// then the window list, then NSWorkspace — whose notifications only arrive while a run loop spins).
///
/// NSWorkspace.frontmostApplication() is updated by notifications that only arrive while a run loop spins, and the
/// desktop worker has none. Measured 2026-09-10: after `open -a Finder` it still said Warp, while AX and the window
/// list both said Finder; in a real run open_app waited out its full 8 s twice and the model went hunting in the Dock.
pub fn focused_pid() -> i64 {
    focused_pid_from(&[&ax_focused_pid, &window_list_pid, &workspace_pid])
}

/// focused_pid over injectable probes, in order: the first non-zero pid, else 0.
pub fn focused_pid_from(probes: &[&dyn Fn() -> i64]) -> i64 {
    for probe in probes {
        let pid = probe();
        if pid != 0 {
            return pid;
        }
    }
    0
}

/// `{"app","bundle","title","pid"}` of the frontmost app; title "" on any AX error.
pub fn ax_frontmost() -> Front {
    ax_frontmost_for_pid(focused_pid())
}

/// ax_frontmost for a known pid (0: the empty answer, Python EMPTY_FRONT).
pub fn ax_frontmost_for_pid(pid: i64) -> Front {
    if pid == 0 {
        return Front::default();
    }
    #[cfg(target_os = "macos")]
    {
        use crate::ax_ffi::{AxAttr, AxElement};
        let (name, bundle) = i32::try_from(pid).ok().and_then(crate::ax_ffi::running_app).unwrap_or_default();
        let app = if name.is_empty() { app_name_for_pid(pid) } else { name };
        let mut info = Front { app, bundle, title: String::new(), pid };
        if let Ok(p) = i32::try_from(pid) {
            if let Ok(AxAttr::Element(window)) = AxElement::application(p).attribute("AXFocusedWindow") {
                if let Ok(title) = window.attribute("AXTitle") {
                    if mac::truthy(&title) {
                        info.title = mac::py_str(&title);
                    }
                }
            }
        }
        info
    }
    #[cfg(not(target_os = "macos"))]
    {
        Front { app: app_name_for_pid(pid), bundle: String::new(), title: String::new(), pid }
    }
}

/// The accessibility element under a point (points, top-left origin), climbed to the nearest pressable ancestor.
/// None when nothing is there or Accessibility is not granted. Bench 2026-09-06: 1-24 ms.
pub fn ax_element_at(x: i64, y: i64) -> Option<AxElementInfo> {
    #[cfg(target_os = "macos")]
    {
        mac::element_at(x, y)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (x, y);
        None
    }
}

/// EVE_AX_PRESS: "0/false/no/off" off, "1/true/yes/on" on, anything else AX_PRESS_DEFAULT. `environ` None reads
/// the process environment.
pub fn ax_press_on(environ: Option<&HashMap<String, String>>) -> bool {
    let raw = match environ {
        Some(env) => env.get(AX_PRESS_ENV).cloned().unwrap_or_default(),
        None => std::env::var(AX_PRESS_ENV).unwrap_or_default(),
    };
    let raw = py_strip(&raw).to_lowercase();
    match raw.as_str() {
        "0" | "false" | "no" | "off" => false,
        "1" | "true" | "yes" | "on" => true,
        _ => AX_PRESS_DEFAULT,
    }
}

/// A live element now, or None when it is gone (or is not an AX element).
pub fn ax_read(el: &AxRef) -> Option<LiveElement> {
    #[cfg(target_os = "macos")]
    {
        mac::read(el.downcast_ref::<crate::ax_ffi::AxElement>()?)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = el;
        None
    }
}

/// AXUIElementPerformAction(el, AXPress): the AXError code, 0 on success.
pub fn ax_press(el: &AxRef) -> anyhow::Result<i64> {
    #[cfg(target_os = "macos")]
    {
        let Some(element) = el.downcast_ref::<crate::ax_ffi::AxElement>() else {
            return Err(LaneError::err("TypeError", "Expecting an AXUIElementRef"));
        };
        Ok(i64::from(element.perform_action("AXPress")))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = el;
        Err(LaneError::err("ModuleNotFoundError", "No module named 'ApplicationServices'"))
    }
}

/// Python `_matches`: does the model's expectation ("the blue Sign in button") describe this element?
pub fn matches(expect: &str, role: &str, title: &str) -> bool {
    let e = casefold(expect);
    let t = casefold(title);
    if !t.is_empty() && (e.contains(&t) || t.contains(&e)) {
        return true;
    }
    let words: HashSet<&str> = MATCH_WORDS
        .find_iter(&e)
        .map(|m| m.as_str())
        .filter(|w| w.len() > 2 && !MATCH_FILLER.contains(w))
        .collect();
    let hits = words.iter().filter(|w| t.contains(**w)).count();
    if !words.is_empty() && hits as f64 / words.len() as f64 >= 0.5 {
        return true;
    }
    // "the text field" against an untitled AXTextField: compare the role word without "AX"
    let role_word = casefold(&role.chars().skip(2).collect::<String>());
    let squeezed: String = e.chars().filter(|c| !py_is_space(*c)).collect();
    t.is_empty() && squeezed.contains(&role_word)
}

/// The element that has keyboard focus right now, or None. Read fresh every call — the lane checks it right after a
/// click, before pasting.
pub fn ax_focused_element() -> Option<FocusedElement> {
    #[cfg(target_os = "macos")]
    {
        mac::focused_element()
    }
    #[cfg(not(target_os = "macos"))]
    {
        // no pyobjc, no focus check (the effector then refuses)
        None
    }
}

/// Python `_norm`: whitespace collapsed, case-folded.
pub fn norm(text: &str) -> String {
    casefold(&py_collapse(text))
}

// ---- the macOS layer ---------------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod mac {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::AllocAnyThread;
    use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType};
    use objc2_core_graphics::{
        kCGNullWindowID, kCGWindowLayer, kCGWindowOwnerPID, CGDataProvider, CGImage, CGMainDisplayID,
        CGWindowListCopyWindowInfo, CGWindowListOption,
    };
    use objc2_foundation::{NSArray, NSDictionary, NSString};
    use objc2_vision::{
        VNImageRequestHandler, VNRecognizeTextRequest, VNRequest, VNRequestTextRecognitionLevel,
    };

    use super::{app_name_for_pid, AxElementInfo, FocusedElement, LiveElement, OcrBox, AX_MAX_HOPS,
        AX_PRESSABLE_ROLES, EDITABLE_AX_ROLES};
    use crate::ax_candidates::Rect;
    use crate::ax_ffi::{AxAttr, AxElement};
    use crate::LaneError;

    pub(super) fn capture_main_display() -> Option<CFRetained<CGImage>> {
        #[allow(deprecated)]
        objc2_core_graphics::CGDisplayCreateImage(CGMainDisplayID())
    }

    pub(super) fn image_size(cg: &CGImage) -> (usize, usize) {
        (CGImage::width(Some(cg)), CGImage::height(Some(cg)))
    }

    /// The CGImage's raw bytes and bytes per row.
    pub(super) fn image_bytes(cg: &CGImage) -> Option<(Vec<u8>, usize)> {
        let provider = CGImage::data_provider(Some(cg))?;
        let data = CGDataProvider::data(Some(&provider))?;
        Some((data.to_vec(), CGImage::bytes_per_row(Some(cg))))
    }

    pub(super) fn recognize_text(cg: &CGImage, fast: bool) -> anyhow::Result<Vec<OcrBox>> {
        objc2::rc::autoreleasepool(|_| {
            let req = VNRecognizeTextRequest::new();
            req.setRecognitionLevel(if fast {
                VNRequestTextRecognitionLevel::Fast
            } else {
                VNRequestTextRecognitionLevel::Accurate
            });
            req.setUsesLanguageCorrection(false);
            let options: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::new();
            // SAFETY: an empty options dictionary is of the documented type.
            let handler =
                unsafe { VNImageRequestHandler::initWithCGImage_options(VNImageRequestHandler::alloc(), cg, &options) };
            let as_request: &VNRequest = &req;
            let requests = NSArray::from_slice(&[as_request]);
            if let Err(err) = handler.performRequests_error(&requests) {
                return Err(LaneError::err("RuntimeError", format!("Vision OCR failed: {err}")));
            }
            let mut out = Vec::new();
            if let Some(results) = req.results() {
                for obs in results.iter() {
                    let cands = obs.topCandidates(1);
                    let Some(top) = cands.iter().next() else {
                        continue;
                    };
                    // SAFETY: boundingBox is a plain struct getter on a live observation.
                    let bbox = unsafe { obs.boundingBox() };
                    out.push(OcrBox {
                        text: top.string().to_string(),
                        bx: bbox.origin.x,
                        by: bbox.origin.y,
                        bw: bbox.size.width,
                        bh: bbox.size.height,
                        conf: f64::from(top.confidence()),
                    });
                }
            }
            Ok(out)
        })
    }

    pub(super) fn window_list_pid() -> i64 {
        let opts = CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
        let Some(list) = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) else {
            return 0;
        };
        // SAFETY: CGWindowListCopyWindowInfo answers an array of dictionaries keyed by CFString.
        let list: CFRetained<CFArray<CFDictionary<CFString, CFType>>> = unsafe { CFRetained::cast_unchecked(list) };
        // SAFETY: the key constants are immutable statics exported by Core Graphics.
        let (layer_key, pid_key) = unsafe { (kCGWindowLayer, kCGWindowOwnerPID) };
        let number = |d: &CFDictionary<CFString, CFType>, key: &CFString| -> Option<i64> {
            d.get(key)?.downcast::<CFNumber>().ok()?.as_i64()
        };
        for w in list.iter() {
            if number(&w, layer_key) == Some(0) {
                if let Some(pid) = number(&w, pid_key) {
                    if pid != 0 {
                        return pid;
                    }
                }
            }
        }
        0
    }

    /// Python truthiness of an attribute value.
    pub(super) fn truthy(v: &AxAttr) -> bool {
        match v {
            AxAttr::Null => false,
            AxAttr::Bool(b) => *b,
            AxAttr::Int(i) => *i != 0,
            AxAttr::Float(f) => *f != 0.0,
            AxAttr::Str(s) => !s.is_empty(),
            AxAttr::Array(a) => !a.is_empty(),
            AxAttr::Point(..) | AxAttr::Size(..) | AxAttr::Element(_) => true,
        }
    }

    /// Python `str(v)` of an attribute value, as far as a title or a role is ever anything else.
    pub(super) fn py_str(v: &AxAttr) -> String {
        match v {
            AxAttr::Null => "None".to_string(),
            AxAttr::Bool(true) => "True".to_string(),
            AxAttr::Bool(false) => "False".to_string(),
            AxAttr::Int(i) => i.to_string(),
            AxAttr::Float(f) => format!("{f:?}"),
            AxAttr::Str(s) => s.clone(),
            other => format!("{other:?}"),
        }
    }

    /// Python's `attr(el, name)`: the value when the read succeeded and is not None.
    fn attr(el: &AxElement, name: &str) -> Option<AxAttr> {
        match el.attribute(name) {
            Ok(AxAttr::Null) | Err(_) => None,
            Ok(v) => Some(v),
        }
    }

    /// `attr(el, a) or attr(el, b) or ""`, as a string.
    fn first_text(el: &AxElement, names: &[&str]) -> String {
        names
            .iter()
            .filter_map(|n| attr(el, n))
            .find(truthy)
            .map(|v| py_str(&v))
            .unwrap_or_default()
    }

    /// The (x, y, w, h) frame from AXPosition and AXSize, truncated to ints; None unless both read.
    fn frame(el: &AxElement) -> Option<Rect> {
        match (attr(el, "AXPosition"), attr(el, "AXSize")) {
            (Some(AxAttr::Point(x, y)), Some(AxAttr::Size(w, h))) => Some((x as i64, y as i64, w as i64, h as i64)),
            _ => None,
        }
    }

    fn describe(el: &AxElement) -> AxElementInfo {
        let role = attr(el, "AXRole").filter(truthy).map(|v| py_str(&v)).unwrap_or_default();
        let mut title = first_text(el, &["AXTitle", "AXDescription"]);
        if title.is_empty() {
            if let Some(AxAttr::Str(val)) = attr(el, "AXValue") {
                title = val.chars().take(60).collect();
            }
        }
        let actions = el.action_names().unwrap_or_default();
        let pressable = actions.iter().any(|a| a == "AXPress") || AX_PRESSABLE_ROLES.contains(&role.as_str());
        AxElementInfo { role, title, frame: frame(el), pressable, app: String::new() }
    }

    pub(super) fn element_at(x: i64, y: i64) -> Option<AxElementInfo> {
        let el = AxElement::system_wide().element_at_position(x as f32, y as f32).ok()?;
        // Which app the point belongs to. A click aimed at Spotify that lands on Warp's scroll area reads
        // "AXScrollArea in Warp" — the wrong-app input the logs showed.
        let app = match el.pid() {
            Ok(pid) if pid != 0 => app_name_for_pid(i64::from(pid)),
            _ => String::new(),
        };
        let mut cur = el;
        let mut first: Option<AxElementInfo> = None;
        let mut found: Option<AxElementInfo> = None;
        for _ in 0..AX_MAX_HOPS {
            let d = describe(&cur);
            if first.is_none() {
                first = Some(d.clone());
            }
            if d.pressable && (!d.title.is_empty() || d.role != "AXGroup") {
                found = Some(d);
                break;
            }
            match attr(&cur, "AXParent") {
                Some(AxAttr::Element(parent)) => cur = parent,
                _ => break,
            }
        }
        let mut result = found.or(first)?;
        result.app = app;
        Some(result)
    }

    pub(super) fn read(el: &AxElement) -> Option<LiveElement> {
        let role = attr(el, "AXRole")?;
        let frame = frame(el);
        let actions = el.action_names().unwrap_or_default();
        let pid = el.pid().map(i64::from).unwrap_or(0);
        let enabled = match attr(el, "AXEnabled") {
            None => true,
            Some(v) => truthy(&v),
        };
        Some(LiveElement {
            role: py_str(&role),
            title: first_text(el, &["AXTitle", "AXDescription"]),
            frame,
            enabled,
            pid,
            actions,
        })
    }

    pub(super) fn focused_element() -> Option<FocusedElement> {
        let Ok(AxAttr::Element(el)) = AxElement::system_wide().attribute("AXFocusedUIElement") else {
            return None;
        };
        let role = attr(&el, "AXRole").filter(truthy).map(|v| py_str(&v)).unwrap_or_default();
        let title = first_text(&el, &["AXTitle", "AXDescription"]);
        let app = match el.pid() {
            Ok(pid) if pid != 0 => app_name_for_pid(i64::from(pid)),
            _ => String::new(),
        };
        let editable = EDITABLE_AX_ROLES.contains(&role.as_str());
        Some(FocusedElement { role, title, frame: frame(&el), app, editable })
    }
}

// ---- the helpers ------------------------------------------------------------------------------------

/// `delegate(...)`'s keyword arguments, with Python's defaults (`Default`: max_steps 4).
#[derive(Debug, Clone, PartialEq)]
pub struct DelegateCall {
    pub objective: String,
    pub text: Option<String>,
    pub key: Option<String>,
    pub done_when: Option<String>,
    pub approve: Option<String>,
    pub max_steps: i64,
    pub dry_run: bool,
    /// `steps=[…]`: an ordered list of exact labels, one click each (fast_lane::run_script)
    pub steps: Option<Vec<String>>,
}

impl Default for DelegateCall {
    fn default() -> Self {
        DelegateCall {
            objective: String::new(),
            text: None,
            key: None,
            done_when: None,
            approve: None,
            max_steps: 4,
            dry_run: false,
            steps: None,
        }
    }
}

/// The model-facing helpers. Every backend is injectable for the tests (`Backends`).
pub struct Helpers {
    pub gui: Box<dyn Gui + Send>,
    pub fast_lane: bool,
    /// `model` mode's decider (eve_decider::Decider over jev), or None
    pub decider: Option<Box<dyn LaneDecider + Send>>,
    /// what the worker reports: off | loading | ready (…) | failed: …
    pub fast_lane_status: String,
    /// fast_lane::DECIDE_MODES: who picks a step (the worker sets it from env)
    pub lane_decide: String,
    /// typed_ask's step asker bound to a predict (the worker sets it), or None
    pub step_asker: Option<Box<dyn StepAsk + Send>>,
    /// the router (lane_router) may run before the planner
    pub lane_first_on: bool,
    pub acted: bool,
    /// one short sentence for the human (Python `self.log`)
    pub log: Box<dyn Fn(&str) + Send>,
    /// an image for the model (Python `self.display`)
    pub display: Box<dyn Fn(RgbImage) + Send>,
    /// ms per sense/effector for the current exec call
    pub timing: IndexMap<String, f64>,
    /// The environment EVE_AX_PRESS is read from; None reads the process environment (the tests' seam for
    /// Python's monkeypatch.setenv).
    pub ax_press_environ: Option<HashMap<String, String>>,
    pub(crate) installed: bool,
    pub(crate) menu_bar_points: i64,
    pub(crate) capture_fn: CaptureFn,
    pub(crate) ocr: OcrFn,
    pub(crate) frontmost_fn: Box<dyn Fn() -> Front + Send>,
    pub(crate) element_at: ElementAtFn,
    pub(crate) focused_element: Box<dyn Fn() -> Option<FocusedElement> + Send>,
    pub(crate) element_read: ElementReadFn,
    pub(crate) element_press: ElementPressFn,
    pub(crate) ax_snapshot: SnapshotFn,
    pub(crate) run: RunFn,
    /// "AXButton 'Search'" under the last raw click
    pub(crate) last_click: Option<String>,
    pub(crate) clipboard: Option<Box<dyn Clipboard + Send>>,
    pub(crate) clock: Box<dyn Fn() -> f64 + Send>,
    pub(crate) sleep: Box<dyn Fn(f64) + Send>,
    pub(crate) now: Box<dyn Fn() -> chrono::NaiveDateTime + Send>,
    pub(crate) frame: Option<Arc<Frame>>,
    pub(crate) last_thumb: Option<GrayImage>,
    pub(crate) elapsed: f64,
    /// bumped by every acted site
    pub(crate) action_seq: u64,
    /// (clock, action_seq) when a settle last finished
    pub(crate) settled: Option<(f64, u64)>,
}

impl Helpers {
    pub fn new(gui: Box<dyn Gui + Send>, backends: Backends) -> Self {
        Helpers {
            gui,
            fast_lane: backends.fast_lane,
            decider: backends.decider,
            fast_lane_status: "off".to_string(),
            lane_decide: "keyword".to_string(),
            step_asker: None,
            lane_first_on: false,
            acted: false,
            log: Box::new(|_| {}),
            display: Box::new(|_| {}),
            timing: IndexMap::new(),
            ax_press_environ: None,
            installed: false,
            menu_bar_points: backends.menu_bar_points,
            capture_fn: backends.capture,
            ocr: backends.ocr,
            frontmost_fn: backends.frontmost_fn,
            element_at: backends.element_at,
            focused_element: backends.focused_element,
            element_read: backends.element_read,
            element_press: backends.element_press,
            ax_snapshot: backends.ax_snapshot,
            run: backends.run,
            last_click: None,
            clipboard: backends.clipboard,
            clock: backends.clock,
            sleep: backends.sleep,
            now: backends.now,
            frame: None,
            last_thumb: None,
            elapsed: 0.0,
            action_seq: 0,
            settled: None,
        }
    }

    // -- wiring --

    /// Bind the helpers into the exec namespace and make every input call mark `acted`: returns the names bound
    /// (HELPER_NAMES, plus FAST_LANE_HELPERS when the fast lane is on) and turns on the acting wrap. Idempotent.
    pub fn install(&mut self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = HELPER_NAMES.to_vec();
        if self.fast_lane {
            names.extend(FAST_LANE_HELPERS);
        }
        self.installed = true;
        names
    }

    /// The input sequence number (Python `_action_seq`): bumped by every site that clicks, types or presses keys.
    pub fn action_seq(&self) -> u64 {
        self.action_seq
    }

    /// What is under the last raw click (Python `_last_click`), for the [after] line.
    pub fn last_click(&self) -> Option<&str> {
        self.last_click.as_deref()
    }

    /// Python `_acting(original)`: run one PyAutoGUI input call `name` through the wrap — mark `acted` and bump the
    /// action sequence, record what is under a click-family call given a point ("click", "doubleClick",
    /// "rightClick" with `point`), and time the call under "act". Before install() it is the bare call. The worker
    /// routes every ACTION_NAMES call it makes on the model's behalf through here.
    pub fn gui_action<T>(
        &mut self,
        name: &str,
        point: Option<(i64, i64)>,
        call: impl FnOnce(&mut (dyn Gui + Send)) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        if !self.installed {
            return call(self.gui.as_mut());
        }
        self.mark_acted();
        if let (true, Some((x, y))) = (["click", "doubleClick", "rightClick"].contains(&name), point) {
            // What is under a raw click, for the [after] line (handyman-style verification of the model's
            // coordinates — it sees "clicked on AXLink 'Weather'").
            self.last_click = self.describe_point(x, y);
        }
        let t0 = (self.clock)();
        let result = call(self.gui.as_mut());
        self.add_timing("act", t0);
        result
    }

    /// Python `_acting(pyautogui.click)`: a raw click through the wrap (marks acted, records what is under a
    /// 2-argument click, times "act"); before install() it is the bare call.
    pub fn gui_click(&mut self, x: i64, y: i64, clicks: u32) -> anyhow::Result<()> {
        self.gui_action("click", Some((x, y)), |g| g.click(x, y, clicks))
    }

    /// Python `_acting(pyautogui.hotkey)`.
    pub fn gui_hotkey(&mut self, keys: &[&str]) -> anyhow::Result<()> {
        self.gui_action("hotkey", None, |g| g.hotkey(keys))
    }

    /// Python `_acting(pyautogui.press)`.
    pub fn gui_press(&mut self, key: &str) -> anyhow::Result<()> {
        self.gui_action("press", None, |g| g.press(key))
    }

    pub fn bind(&mut self, log: Box<dyn Fn(&str) + Send>, display: Box<dyn Fn(RgbImage) + Send>) {
        self.log = log;
        self.display = display;
    }

    pub fn begin(&mut self) {
        self.acted = false;
        self.last_click = None;
        self.timing = IndexMap::new();
    }

    /// Every site that clicks, types or presses keys: `acted` for the auto-screenshot, the sequence number so a
    /// settle from before this input is never reused after it.
    pub(crate) fn mark_acted(&mut self) {
        self.acted = true;
        self.action_seq += 1;
    }

    pub(crate) fn add_timing(&mut self, key: &str, t0: f64) {
        let ms = ((self.clock)() - t0) * 1000.0;
        *self.timing.entry(key.to_string()).or_insert(0.0) += ms;
    }

    /// The current call's timing, rounded to 0.01 ms, for the worker reply.
    pub fn timing_ms(&self) -> IndexMap<String, f64> {
        self.timing.iter().map(|(k, v)| (k.clone(), py_round_to(*v, 2))).collect()
    }

    fn say(&self, line: &str) {
        (self.log)(line);
    }

    pub(crate) fn front(&mut self) -> Front {
        let t0 = (self.clock)();
        let front = (self.frontmost_fn)();
        self.add_timing("ax", t0);
        front
    }

    pub(crate) fn describe_point(&mut self, x: i64, y: i64) -> Option<String> {
        let t0 = (self.clock)();
        let el = (self.element_at)(x, y);
        self.add_timing("ax", t0);
        // a hit-test failure only costs the [after] detail
        match el {
            Ok(Some(el)) => Some(what(&el)),
            _ => None,
        }
    }

    /// Click the control under (x, y) — snapped to its centre — after checking it is the one you meant. `expect` is
    /// a few words from the screenshot ("Sign in button"). A mismatch does NOT click; it tells you what is there.
    pub fn click_element(&mut self, x: i64, y: i64, expect: &str, clicks: u32) -> anyhow::Result<String> {
        let t0 = (self.clock)();
        // no element means a raw click, reported as such
        let el = (self.element_at)(x, y).ok().flatten();
        self.add_timing("ax", t0);
        let Some(el) = el else {
            self.mark_acted();
            self.gui_click(x, y, clicks)?;
            let line = format!("clicked at ({x},{y}) (no accessibility element there to check)");
            self.say(&line);
            return Ok(line);
        };
        let role = if el.role.is_empty() { "element" } else { el.role.as_str() };
        let what_el = what(&el);
        if !expect.is_empty() && !matches(expect, role, &el.title) {
            let line = format!(
                "did not click: under ({x},{y}) is {what_el}, not {}. Look again (screen_text / zoom).",
                py_repr(expect)
            );
            self.say(&line);
            return Ok(line);
        }
        let (mut cx, mut cy) = (x, y);
        if let Some((fx, fy, fw, fh)) = el.frame {
            if fw > 0 && fh > 0 {
                cx = (fx as f64 + fw as f64 / 2.0) as i64;
                cy = (fy as f64 + fh as f64 / 2.0) as i64;
            }
        }
        self.mark_acted();
        self.last_click = Some(what_el.clone());
        self.gui_click(cx, cy, clicks)?;
        let line = format!("clicked {what_el} at ({cx},{cy})");
        self.say(&line);
        Ok(line)
    }

    // -- apps and pages --

    /// Launch or focus an app with `open -a`, then wait (≤ MAX_WAIT_SECS; Python default 8) until it is frontmost.
    /// ValueError for a path or an option, RuntimeError "no app called …" when `open` fails.
    pub fn open_app(&mut self, name: &str, wait: f64) -> anyhow::Result<String> {
        self.mark_acted();
        if name.contains('/') || name.starts_with('-') {
            return Err(LaneError::err(
                "ValueError",
                format!("open_app: {} must be an app name, not a path or an option", py_repr(name)),
            ));
        }
        let argv = vec!["open".to_string(), "-a".to_string(), name.to_string()];
        let rc = (self.run)(&argv, 10.0)?;
        if rc != 0 {
            return Err(LaneError::err("RuntimeError", format!("no app called {}", py_repr(name))));
        }
        let wait = wait.min(MAX_WAIT_SECS);
        let want = casefold(name);
        let t0 = (self.clock)();
        let mut front = self.front();
        while casefold(&front.app) != want && (self.clock)() - t0 < wait {
            (self.sleep)(0.1);
            front = self.front();
        }
        let elapsed = (self.clock)() - t0;
        let text = if casefold(&front.app) == want {
            format!("opened {name} (frontmost after {elapsed:.1} s)")
        } else {
            let other = if front.app.is_empty() { "something else" } else { front.app.as_str() };
            format!("opened {name} but {other} is still frontmost after {wait:.1} s")
        };
        self.settle(1.5, 0.25)?;
        self.say(&text);
        Ok(text)
    }

    /// Open a URL in the default browser (or `app`) and wait (Python default 8) for it to come up. Only http,
    /// https and mailto.
    pub fn open_url(&mut self, url: &str, app: Option<&str>, wait: f64) -> anyhow::Result<String> {
        self.mark_acted();
        // urlsplit's own ValueError (a bad bracketed host) propagates with its message, as in Python
        let scheme = crate::ax_candidates::try_urlsplit(url)
            .map_err(|e| LaneError::err("ValueError", e.0))?
            .scheme
            .to_lowercase();
        if !["http", "https", "mailto"].contains(&scheme.as_str()) {
            return Err(LaneError::err("ValueError", "open_url: only http, https and mailto URLs"));
        }
        if let Some(a) = app {
            if a.contains('/') || a.starts_with('-') {
                return Err(LaneError::err("ValueError", format!("open_url: {} must be an app name", py_repr(a))));
            }
        }
        // Python's `if app` treats "" as no app
        let app = app.filter(|a| !a.is_empty());
        let before = self.front();
        let mut argv = vec!["open".to_string()];
        if let Some(a) = app {
            argv.push("-a".to_string());
            argv.push(a.to_string());
        }
        argv.push(url.to_string());
        let rc = (self.run)(&argv, 10.0)?;
        if rc != 0 {
            let suffix = app.map(|a| format!(" in {}", py_repr(a))).unwrap_or_default();
            return Err(LaneError::err("RuntimeError", format!("could not open {}{suffix}", py_repr(url))));
        }
        let wait = wait.min(MAX_WAIT_SECS);
        let t0 = (self.clock)();
        // A named app must itself come to the front: another browser being up is not arrival (bench 2026-09-06:
        // Chrome in front made Safari "done").
        let arrived = |front: &Front| -> bool {
            if let Some(a) = app {
                return casefold(&front.app) == casefold(a);
            }
            if BROWSER_BUNDLES.contains(&front.bundle.as_str()) {
                return true;
            }
            front.title != before.title
        };
        let mut front = self.front();
        while !arrived(&front) && (self.clock)() - t0 < wait {
            (self.sleep)(0.2);
            front = self.front();
        }
        let text = if arrived(&front) {
            let shown = if front.app.is_empty() { "the browser" } else { front.app.as_str() };
            format!("opened {url} in {shown}")
        } else {
            let other = if front.app.is_empty() { "something else" } else { front.app.as_str() };
            format!("opened {url} but {other} is still frontmost after {wait:.1} s")
        };
        self.settle(2.0, 0.25)?;
        self.say(&text);
        Ok(text)
    }

    pub fn frontmost(&mut self) -> Front {
        let front = self.front();
        self.say(&format!("frontmost: {} — {}", front.app, py_repr(&front.title)));
        front
    }

    // -- reading the screen --

    /// OCR the screen: lines in click coordinates, top to bottom, capped (Python level default "accurate").
    pub fn screen_text(&mut self, region: Option<Rect>, level: &str) -> anyhow::Result<Vec<TextLine>> {
        let lines = self.lines(region, level)?;
        let total = lines.len();
        let mut kept: Vec<TextLine> = Vec::new();
        let mut size = 0usize;
        for item in lines {
            size += item.text.len() + 48;
            if kept.len() >= SCREEN_TEXT_MAX_LINES || size > SCREEN_TEXT_MAX_BYTES {
                break;
            }
            kept.push(item);
        }
        if kept.len() < total {
            kept.push(TextLine {
                text: format!("({} more lines; use region=)", total - kept.len()),
                x: 0,
                y: 0,
                w: 0,
                h: 0,
                conf: 0.0,
            });
        }
        self.say(&format!("screen_text: {total} lines"));
        Ok(kept)
    }

    /// Python `_lines`.
    pub(crate) fn lines(&mut self, region: Option<Rect>, level: &str) -> anyhow::Result<Vec<TextLine>> {
        let frame = self.capture()?;
        let (w_pts, h_pts) = self.size();
        let (w_pts, h_pts) = (w_pts as f64, h_pts as f64);
        let t0 = (self.clock)();
        let boxes = (self.ocr)(&frame, level);
        self.add_timing("ocr", t0);
        let boxes = boxes?;
        let mut items: Vec<TextLine> = Vec::new();
        for r in boxes {
            let x = py_round(r.bx * w_pts);
            let y = py_round((1.0 - r.by - r.bh) * h_pts);
            let w = py_round(r.bw * w_pts);
            let h = py_round(r.bh * h_pts);
            if let Some((rx, ry, rw, rh)) = region {
                let (cx, cy) = (x as f64 + w as f64 / 2.0, y as f64 + h as f64 / 2.0);
                let inside = rx as f64 <= cx
                    && cx <= (rx + rw) as f64
                    && ry as f64 <= cy
                    && cy <= (ry + rh) as f64;
                if !inside {
                    continue;
                }
            }
            items.push(TextLine { text: r.text, x, y, w, h, conf: py_round_to(r.conf, 3) });
        }
        items.sort_by_key(|i| (i.y.div_euclid(8), i.x));
        Ok(items)
    }

    /// The first on-screen line equal to `text` (else containing it), with its centre cx, cy.
    pub fn find_text(&mut self, text: &str, region: Option<Rect>, level: &str) -> anyhow::Result<Option<FoundText>> {
        let item = self.find(text, region, level)?;
        match &item {
            None => self.say(&format!("{} is not on screen", py_repr(text))),
            Some(i) => self.say(&format!("found {} at ({},{})", py_repr(text), i.cx, i.cy)),
        }
        Ok(item)
    }

    /// Python `_find`.
    pub(crate) fn find(&mut self, text: &str, region: Option<Rect>, level: &str) -> anyhow::Result<Option<FoundText>> {
        let needle = py_strip(&casefold(text)).to_string();
        if needle.is_empty() {
            return Ok(None);
        }
        let lines = self.lines(region, level)?;
        let hit = lines
            .iter()
            .find(|i| py_strip(&casefold(&i.text)) == needle)
            .or_else(|| lines.iter().find(|i| casefold(&i.text).contains(&needle)));
        Ok(hit.map(|h| FoundText {
            line: h.clone(),
            cx: h.x + h.w.div_euclid(2),
            cy: h.y + h.h.div_euclid(2),
        }))
    }

    /// Find `text` with accurate OCR (waiting up to `timeout`, Python default 3) and click its centre. LookupError
    /// when it is not on screen.
    ///
    /// Fast OCR can miss a label that screen_text() just read, so it must not gate a click behind repeated false
    /// negatives and another planner turn.
    pub fn click_text(&mut self, text: &str, region: Option<Rect>, timeout: f64, clicks: u32) -> anyhow::Result<String> {
        let item = match self.wait_for_inner(text, false, timeout, 0.3, region, "accurate")? {
            Waited::Found(item) => item,
            _ => return Err(LaneError::err("LookupError", format!("{} is not on screen", py_repr(text)))),
        };
        self.mark_acted();
        self.gui_click(item.cx, item.cy, clicks)?;
        let line = format!("clicked {} at ({},{})", py_repr(text), item.cx, item.cy);
        self.say(&line);
        Ok(line)
    }

    /// Poll (fast OCR) until `text` is on screen — or, with gone, until it has gone (Python defaults: timeout 6,
    /// interval 0.3).
    pub fn wait_for(
        &mut self,
        text: &str,
        gone: bool,
        timeout: f64,
        interval: f64,
        region: Option<Rect>,
    ) -> anyhow::Result<Waited> {
        let result = self.wait_for_inner(text, gone, timeout, interval, region, "fast")?;
        let elapsed = self.elapsed;
        let timeout = timeout.min(MAX_WAIT_SECS);
        let t = py_repr(text);
        let line = match &result {
            Waited::Gone => format!("{t} went away after {elapsed:.1} s"),
            Waited::StillThere => format!("{t} is still on screen after {timeout:.1} s"),
            Waited::Found(_) => format!("{t} appeared after {elapsed:.1} s"),
            Waited::NotSeen => format!("{t} not seen in {timeout:.1} s"),
        };
        self.say(&line);
        Ok(result)
    }

    /// Python `_wait_for` (level "fast" unless given).
    pub(crate) fn wait_for_inner(
        &mut self,
        text: &str,
        gone: bool,
        timeout: f64,
        interval: f64,
        region: Option<Rect>,
        level: &str,
    ) -> anyhow::Result<Waited> {
        let timeout = timeout.min(MAX_WAIT_SECS);
        let t0 = (self.clock)();
        loop {
            let item = self.find(text, region, level)?;
            self.elapsed = (self.clock)() - t0;
            if gone && item.is_none() {
                return Ok(Waited::Gone);
            }
            if !gone {
                if let Some(item) = item {
                    if level == "fast" {
                        let confirmed = self.find(text, region, "accurate")?;
                        return Ok(Waited::Found(confirmed.unwrap_or(item)));
                    }
                    return Ok(Waited::Found(item));
                }
            }
            if (self.clock)() - t0 >= timeout {
                return Ok(if gone { Waited::StillThere } else { Waited::NotSeen });
            }
            (self.sleep)(interval);
        }
    }

    /// Wait until two consecutive captures `interval` apart look the same (Python defaults: timeout 5, interval
    /// 0.25).
    pub fn wait_settled(&mut self, timeout: f64, interval: f64) -> anyhow::Result<bool> {
        let settled = self.settle(timeout, interval)?;
        if settled {
            self.say(&format!("screen settled after {:.1} s", self.elapsed));
        } else {
            self.say(&format!("screen still changing after {:.1} s", timeout.min(MAX_WAIT_SECS)));
        }
        Ok(settled)
    }

    /// Python `_settle(timeout, interval=0.25)`: two captures `interval` apart that look the same; false past
    /// `timeout`. Remembers a completed attempt, including a timeout, for the auto-screenshot.
    pub(crate) fn settle(&mut self, timeout: f64, interval: f64) -> anyhow::Result<bool> {
        let timeout = timeout.min(MAX_WAIT_SECS);
        let t0 = (self.clock)();
        let result = self.settle_loop(timeout, interval, t0);
        self.add_timing("settle", t0);
        result
    }

    /// Python `_settle_loop`.
    pub(crate) fn settle_loop(&mut self, timeout: f64, interval: f64, t0: f64) -> anyhow::Result<bool> {
        let frame = self.capture()?;
        let mut a = frame.thumb()?.clone();
        let skip = self.skip_rows(&frame);
        if timeout <= 0.0 {
            self.elapsed = 0.0;
            return Ok(true);
        }
        loop {
            (self.sleep)(interval);
            let b = self.capture()?.thumb()?.clone();
            self.elapsed = (self.clock)() - t0;
            if change_box(&a, &b, skip, CHANGE_MIN_PIXELS).is_none() {
                self.settled = Some(((self.clock)(), self.action_seq));
                return Ok(true);
            }
            a = b;
            if (self.clock)() - t0 >= timeout {
                self.settled = Some(((self.clock)(), self.action_seq));
                return Ok(false);
            }
        }
    }

    // -- typing --

    /// Type any text (clipboard paste: emoji and accents survive), optionally pressing enter.
    pub fn type_text(&mut self, text: &str, submit: bool) -> anyhow::Result<String> {
        self.mark_acted();
        let mut clipboard: Box<dyn Clipboard + Send> =
            self.clipboard.take().unwrap_or_else(|| Box::new(SystemClipboard));
        // a non-text clipboard is not worth failing over
        let old = clipboard.paste().unwrap_or_default();
        let pasted = clipboard.copy(text).and_then(|()| {
            self.gui_hotkey(&["command", "v"])?;
            (self.sleep)(0.15);
            clipboard.copy(&old)
        });
        self.clipboard = Some(clipboard);
        pasted?;
        let mut line = format!("typed {} characters", py_len(text));
        if submit {
            self.gui_press("enter")?;
            line.push_str(" and pressed enter");
        }
        self.say(&line);
        Ok(line)
    }

    // -- looking --

    /// Display a close-up of that point region at the frame's physical scale.
    pub fn zoom(&mut self, x: i64, y: i64, w: i64, h: i64) -> anyhow::Result<String> {
        let frame = self.capture()?;
        let (w_pts, h_pts) = self.size();
        let scale = if w_pts != 0 { f64::from(frame.width) / w_pts as f64 } else { 1.0 };
        let (x, y) = (x.max(0), y.max(0));
        let w = w.min(ZOOM_MAX_POINTS.0).min(w_pts - x).max(1);
        let h = h.min(ZOOM_MAX_POINTS.1).min(h_pts - y).max(1);
        let (x0, y0) = (py_round(x as f64 * scale), py_round(y as f64 * scale));
        let (x1, y1) = (py_round((x + w) as f64 * scale), py_round((y + h) as f64 * scale));
        let crop = crop_padded(frame.pil()?, x0, y0, x1, y1);
        (self.display)(crop);
        let s = py_g(scale);
        let line = format!(
            "zoom of points ({x},{y},{w},{h}) at {s}x: a pixel (px,py) in this image is point (x+px/{s}, y+py/{s})"
        );
        self.say(&line);
        Ok(line)
    }

    /// A fresh screenshot plus the context line.
    pub fn observe(&mut self, note: &str) -> anyhow::Result<String> {
        let shot = self.screenshot()?;
        (self.display)(shot);
        let mut line = self.context_line();
        if !note.is_empty() {
            line.push_str(&format!(" — {note}"));
        }
        self.say(&line);
        Ok(line)
    }

    // -- used by the worker --

    /// The screen in points, like pyautogui.screenshot() but from a CG capture.
    pub fn screenshot(&mut self) -> anyhow::Result<RgbImage> {
        let frame = self.capture()?;
        self.points(&frame)
    }

    /// Wait (up to max_wait) for the screen to stop changing, then the screenshot of that last frame. When a helper
    /// already waited after the last input and that frame is under SETTLE_REUSE_SECS old, reuse it.
    ///
    /// Reuse happens even if animation exhausted the wait (open_app, open_url, wait_settled); a timeout still
    /// returned false to its caller.
    pub fn settled_screenshot(&mut self, max_wait: f64) -> anyhow::Result<RgbImage> {
        if max_wait > 0.0 && !self.settle_is_fresh() {
            self.settle(max_wait, 0.25)?;
        } else if max_wait <= 0.0 {
            self.capture()?;
        }
        let frame = self
            .frame
            .clone()
            .ok_or_else(|| LaneError::err("AssertionError", "settled_screenshot without a frame"))?;
        self.points(&frame)
    }

    pub(crate) fn settle_is_fresh(&self) -> bool {
        let (Some((at, seq)), Some(_)) = (self.settled, &self.frame) else {
            return false;
        };
        let age = (self.clock)() - at;
        seq == self.action_seq && (0.0..SETTLE_REUSE_SECS).contains(&age)
    }

    // -- the fast lane --

    /// Hand a narrow run of clicks on labelled controls in the frontmost app to the lane. `steps` is an ordered list
    /// of exact labels, one click each (fast_lane::run_script); otherwise `objective` is one run
    /// (fast_lane::run_delegate). Who decides is `lane_decide`. Returns the one-line result the model reads; every
    /// step and the final line are logged for the human.
    pub fn delegate(&mut self, call: DelegateCall) -> String {
        let lane_decide = self.lane_decide_mode();
        let script_decide = self.script_decide();
        // The decider and the asker are lent to the lane while the adapter holds the Helpers.
        let decider = self.decider.take();
        let asker = self.step_asker.take();
        let (line, decide_ms, log_lines) = {
            let adapter = LaneAdapter::new(self, call.approve.as_deref());
            let clock = || adapter.now();
            let decider_ref: Option<&dyn LaneDecider> = decider.as_deref().map(|d| d as &dyn LaneDecider);
            if let Some(steps) = &call.steps {
                let objectives: Vec<String> = steps.iter().map(|s| py_take(s, 200)).collect();
                let script = crate::fast_lane::run_script(
                    &objectives,
                    &adapter,
                    &adapter,
                    decider_ref,
                    crate::fast_lane::ScriptOptions {
                        decide: script_decide,
                        approve: call.approve.clone(),
                        dry_run: call.dry_run,
                        clock: &clock,
                        ..Default::default()
                    },
                );
                let ms: Vec<f64> = script.results.iter().flat_map(|r| r.steps.iter().map(|s| s.decide_ms)).collect();
                (script.line, ms, script.log_lines)
            } else {
                let objective = py_take(&call.objective, 200);
                let text = call.text.as_deref().map(|t| py_take(t, 200));
                let max_steps = call.max_steps.clamp(1, 6);
                let result = crate::fast_lane::run_delegate(
                    &objective,
                    &adapter,
                    &adapter,
                    decider_ref,
                    crate::fast_lane::DelegateOptions {
                        text,
                        key: call.key.clone(),
                        done_when: call.done_when.clone(),
                        approve: call.approve.clone(),
                        max_steps,
                        dry_run: call.dry_run,
                        clock: &clock,
                        decide: lane_decide,
                        asker: asker.as_deref().map(|a| a as &dyn StepAsk),
                        ..Default::default()
                    },
                );
                let ms: Vec<f64> = result.steps.iter().map(|s| s.decide_ms).collect();
                (result.line, ms, result.log_lines)
            }
        };
        self.decider = decider;
        self.step_asker = asker;
        for ms in decide_ms {
            *self.timing.entry("decide".to_string()).or_insert(0.0) += ms;
        }
        for l in &log_lines {
            self.say(l);
        }
        line
    }

    /// Python `_lane_decide`: "jev" only while an asker exists: a hosted model that is not configured must not turn
    /// the lane off, so the keyword gate decides alone instead.
    pub fn lane_decide_mode(&self) -> String {
        if self.lane_decide == "jev" && self.step_asker.is_none() {
            "keyword".to_string()
        } else {
            self.lane_decide.clone()
        }
    }

    /// Python `_script_decide`: a script's steps are exact labels the planner read off the screen: the keyword
    /// gate's case.
    pub fn script_decide(&self) -> String {
        if self.lane_decide == "jev" {
            "keyword".to_string()
        } else {
            self.lane_decide.clone()
        }
    }

    /// The front window as the planner is shown it: `label (role, value)` per pressable or editable control, in the
    /// lane's own ranking order — never the window title, never a field's contents (Python default max_lines 80).
    /// `{"app", "lines", "dialog", "truncated"}`.
    pub fn outline(&mut self, max_lines: usize) -> anyhow::Result<Value> {
        use crate::ax_candidates::{needs_confirmation, rank_candidates, Kinds};

        let screen = self.size();
        let snap = (self.ax_snapshot)(Some(screen))?;
        let (pressable, _) = rank_candidates(&snap, "", max_lines as i64, Kinds::Pressable, false, true);
        let (fields, _) = rank_candidates(&snap, "", 10, Kinds::Editable, false, false);
        let mut lines: Vec<String> = pressable
            .iter()
            .map(|c| {
                let asks = if needs_confirmation(c, &snap.href(&c.id)) { " [asks the human first]" } else { "" };
                format!("{}{asks}", eve_decider::render_option(c))
            })
            .collect();
        for c in fields.iter().filter(|c| !c.secure) {
            let label = py_take(&py_collapse(&c.label), 40);
            let label = if label.is_empty() { "unlabelled".to_string() } else { label };
            lines.push(format!("{label} ({}, a text field)", c.role));
        }
        lines.truncate(max_lines);
        Ok(json!({
            "app": snap.app,
            "lines": lines,
            "dialog": !snap.dialog_text.is_empty(),
            "truncated": snap.truncated,
        }))
    }

    /// Walk a plan_contract plan over this desktop (plan_executor::run_plan). Never fails: returns
    /// PlanResult::to_dict(). `approved` is the JSON object {step index as a string: what the human was asked}.
    pub fn run_plan(
        &mut self,
        plan: &Value,
        request: &str,
        start: i64,
        approved: Option<&Map<String, Value>>,
        dry_run: bool,
        success_rejected: bool,
    ) -> Value {
        use crate::plan_executor::{PlanOptions, PlanResult};

        let source = py_str_or(plan.get("source"), "astra");
        let parsed = match eve_plan_contract::parse_plan_with(
            plan,
            &py_take(request, 600),
            eve_plan_contract::ParseOptions { source: &source, ..Default::default() },
        ) {
            Ok(p) => p,
            Err(e) => {
                let mut out = PlanResult::new("unavailable");
                out.reason = py_take(&format!("bad plan: {}", e.0), 200);
                return out.to_dict();
            }
        };
        let start = start.max(0);
        // Python's `{int(k): str(v) ...}` runs inside the executor's try: a key that is not an int is an error result.
        let mut approved_map: HashMap<i64, String> = HashMap::new();
        let mut bad_key: Option<String> = None;
        for (k, v) in approved.into_iter().flatten() {
            match py_int(k) {
                Some(i) => {
                    approved_map.insert(i, py_str_value(v));
                }
                None => {
                    bad_key = Some(k.clone());
                    break;
                }
            }
        }
        let asker = self.step_asker.take();
        let decide = if asker.is_some() { "jev" } else { "keyword" }.to_string();
        let planned_for = py_take(&py_str_or(plan.get("app"), ""), 80);
        let result = if let Some(k) = bad_key {
            let mut out = PlanResult::new("partial");
            out.next_index = start as usize;
            out.reason =
                py_take(&format!("error:ValueError: invalid literal for int() with base 10: {}", py_repr(&k)), 200);
            out
        } else {
            let adapter = LaneAdapter::new(self, None);
            let clock = || adapter.now();
            let open_app = |name: &str| adapter.helpers().open_app(name, 8.0);
            let open_url = |url: &str| adapter.helpers().open_url(url, None, 8.0);
            let frontmost_app = || adapter.helpers().front().app;
            crate::plan_executor::run_plan(
                &parsed,
                &adapter,
                &adapter,
                asker.as_deref().map(|a| a as &dyn StepAsk),
                &open_app,
                &open_url,
                &frontmost_app,
                PlanOptions {
                    start: start as usize,
                    approved: approved_map,
                    decide,
                    dry_run,
                    planned_for,
                    success_rejected,
                    clock: &clock,
                    ..Default::default()
                },
            )
        };
        self.step_asker = asker;
        for l in &result.log_lines {
            self.say(l);
        }
        result.to_dict()
    }

    /// The router (lane_router::route) over this desktop: try to finish `goal` with keyword-decided clicks before
    /// any planner call. Returns RouteResult::to_dict(); never fails.
    pub fn lane_first(&mut self, goal: &str, dry_run: bool) -> Value {
        let result = {
            let adapter = LaneAdapter::new(self, None);
            let front = adapter.helpers().front().app;
            let clock = || adapter.now();
            crate::lane_router::route(
                &py_take(goal, 400),
                &adapter,
                &adapter,
                crate::lane_router::RouteOptions { frontmost_app: front, dry_run, clock: &clock, ..Default::default() },
            )
        };
        for l in &result.log_lines {
            self.say(l);
        }
        result.to_dict()
    }

    /// The opt-in visual loop, using the same hit-test and AXPress effectors as the keyword lane.
    pub fn decisions_first(&mut self, goal: &str, client: &mut dyn Evaluate, dry_run: bool) -> Value {
        let result = {
            let adapter = LaneAdapter::new(self, None);
            crate::decision_lane::route(
                goal,
                &adapter,
                &adapter,
                client,
                None,
                crate::decision_lane::Gates::default(),
                dry_run,
            )
        };
        for l in &result.log_lines {
            self.say(l);
        }
        result.to_dict()
    }

    /// "frontmost: {app} — {title!r}; screen {w}x{h}; {HH:MM}".
    pub fn context_line(&mut self) -> String {
        let front = self.front();
        let (w_pts, h_pts) = self.size();
        format!(
            "frontmost: {} — {}; screen {w_pts}x{h_pts}; {}",
            front.app,
            py_repr(&front.title),
            (self.now)().format("%H:%M")
        )
    }

    /// What changed since the previous exec ended, and where, in click coordinates. "[after your input]" when this
    /// step clicked, typed or pressed keys, "[after]" when it only looked — so "[after your input] … screen:
    /// unchanged" means the input did nothing visible, which the agent uses to gate its repeat note and its
    /// final-answer check.
    pub fn after_line(&mut self) -> anyhow::Result<String> {
        let frame = self.capture()?;
        let thumb = frame.thumb()?.clone();
        let mut bx = None;
        if let Some(last) = &self.last_thumb {
            bx = change_box(last, &thumb, self.skip_rows(&frame), CHANGE_MIN_PIXELS);
        }
        self.last_thumb = Some(thumb);
        let screen = match bx {
            None => "unchanged".to_string(),
            Some((bx, by, bw, bh)) => {
                let scale = self.thumb_scale(&frame);
                let s = |v: u32| py_round(f64::from(v) * scale);
                let (x, y, w, h) = (s(bx), s(by), s(bw), s(bh));
                let (w_pts, h_pts) = self.size();
                if w_pts != 0 && h_pts != 0 && (w * h) as f64 >= 0.6 * (w_pts * h_pts) as f64 {
                    "changed (most of the screen)".to_string()
                } else {
                    format!("changed around ({x},{y},{w},{h})")
                }
            }
        };
        let front = self.front();
        let head = if self.acted { "[after your input]" } else { "[after]" };
        let tail = match &self.last_click {
            Some(what) if !what.is_empty() => format!("; clicked on {what}"),
            _ => String::new(),
        };
        Ok(format!("{head} frontmost: {} — {}; screen: {screen}{tail}", front.app, py_repr(&front.title)))
    }

    // -- internals --

    /// Points per thumb pixel (2.0 on a 2x Retina frame).
    pub(crate) fn thumb_scale(&self, frame: &Frame) -> f64 {
        let (w_pts, _) = self.size();
        if frame.width != 0 {
            (w_pts * i64::from(THUMB_DIVISOR)) as f64 / f64::from(frame.width)
        } else {
            1.0
        }
    }

    /// Thumb rows covered by the menu bar, which never count as a change.
    pub(crate) fn skip_rows(&self, frame: &Frame) -> u32 {
        if self.menu_bar_points <= 0 {
            return 0;
        }
        let rows = (self.menu_bar_points as f64 / self.thumb_scale(frame)).trunc();
        if rows.is_finite() && rows >= 0.0 {
            (rows as u32).saturating_add(1)
        } else {
            1
        }
    }

    pub(crate) fn capture(&mut self) -> anyhow::Result<Arc<Frame>> {
        let t0 = (self.clock)();
        let frame = (self.capture_fn)();
        self.add_timing("capture", t0);
        let frame = Arc::new(frame?);
        self.frame = Some(frame.clone());
        Ok(frame)
    }

    /// The screen in points (`pyautogui.size()`).
    pub(crate) fn size(&self) -> (i64, i64) {
        self.gui.size()
    }

    /// The frame resized to points (Lanczos), timed under "resize".
    pub(crate) fn points(&mut self, frame: &Frame) -> anyhow::Result<RgbImage> {
        let t0 = (self.clock)();
        let mut img = frame.pil()?.clone();
        let (w, h) = self.size();
        if (i64::from(img.width()), i64::from(img.height())) != (w, h) {
            if w <= 0 || h <= 0 {
                return Err(LaneError::err("ValueError", "height and width must be > 0"));
            }
            img = image::imageops::resize(&img, w as u32, h as u32, image::imageops::FilterType::Lanczos3);
        }
        self.add_timing("resize", t0);
        Ok(img)
    }
}

// ---- the fast lane's senses and effectors ---------------------------------------------------------

/// fast_lane's senses and effectors over one Helpers (Python `_LaneAdapter`). Every input goes through the same
/// wrapped calls as the model's own code, so `acted`, the timing dict and the auto-screenshot rule all apply.
/// `&self` methods (the lane lends it as both senses and effectors); the Helpers and the adapter's state sit behind
/// RefCells.
pub struct LaneAdapter<'a> {
    h: RefCell<&'a mut Helpers>,
    approve: RefCell<String>,
    /// the screen at the latest snapshot
    thumb0: RefCell<Option<GrayImage>>,
    /// the screen at the snapshot before that: the step's start
    thumb_before: RefCell<Option<GrayImage>>,
    snapshot: RefCell<Option<Snapshot>>,
}

impl<'a> LaneAdapter<'a> {
    /// `_LaneAdapter(helpers, approve)`: `approve` is kept normalised (`norm`).
    pub fn new(helpers: &'a mut Helpers, approve: Option<&str>) -> Self {
        let approve = match approve {
            Some(a) if !a.is_empty() => norm(a),
            _ => String::new(),
        };
        LaneAdapter {
            h: RefCell::new(helpers),
            approve: RefCell::new(approve),
            thumb0: RefCell::new(None),
            thumb_before: RefCell::new(None),
            snapshot: RefCell::new(None),
        }
    }

    /// The Helpers behind the adapter (open_app / open_url / frontmost for run_plan).
    pub fn helpers(&self) -> RefMut<'_, &'a mut Helpers> {
        self.h.borrow_mut()
    }

    /// The approval the effector gate compares against (normalised), "" for none.
    pub fn approve(&self) -> String {
        self.approve.borrow().clone()
    }

    /// The Helpers' clock, for the lane's own `clock=`.
    fn now(&self) -> f64 {
        let h = self.h.borrow();
        (h.clock)()
    }

    /// The screen baselines (Python `_thumb_before`, `_thumb0`): the step's start and the latest snapshot's screen.
    pub fn set_thumbs(&self, before: Option<GrayImage>, latest: Option<GrayImage>) {
        *self.thumb_before.borrow_mut() = before;
        *self.thumb0.borrow_mut() = latest;
    }

    /// The snapshot the effectors check a candidate against (Python `_snapshot`).
    pub fn set_snapshot(&self, snapshot: Option<Snapshot>) {
        *self.snapshot.borrow_mut() = snapshot;
    }

    /// Any pixel change below the macOS menu bar (32 rows) invalidates a pending visual decision.
    pub fn image_matches(before: &DynamicImage, after: &DynamicImage) -> bool {
        use image::GenericImageView;

        if before.dimensions() != after.dimensions() || before.height() <= 32 {
            return false;
        }
        let (a, b) = (before.to_rgb8(), after.to_rgb8());
        let (w, h) = a.dimensions();
        (32..h).all(|y| (0..w).all(|x| a.get_pixel(x, y) == b.get_pixel(x, y)))
    }

    /// Python `_centre`.
    pub fn centre(c: &Candidate) -> (i64, i64) {
        let (x, y, w, h) = c.frame;
        ((x as f64 + w as f64 / 2.0) as i64, (y as f64 + h as f64 / 2.0) as i64)
    }

    /// Python `_same_control`: is the hit-tested element the candidate? Equal titles, or — for a row / cell / link
    /// whose label came from a descendant static text — an element inside the candidate's frame (± 2 pt) whose own
    /// title is empty or part of that label.
    ///
    /// The descendant case is System Settings, Finder, Music, Photos and Notes sidebars.
    pub fn same_control(el: &AxElementInfo, c: &Candidate) -> bool {
        let title = norm(&el.title);
        let label = norm(&c.label);
        if !title.is_empty() && title == label {
            return true;
        }
        let Some((fx, fy, fw, fh)) = el.frame else {
            return false;
        };
        let (x, y, w, h) = c.frame;
        let inside = fx >= x - 2 && fy >= y - 2 && fx + fw <= x + w + 2 && fy + fh <= y + h + 2;
        inside && (title.is_empty() || label.contains(&title) || title.contains(&label))
    }

    /// Python `_press_instead`: the hit-test missed: AXPress the element the snapshot read for `c`, after checking it
    /// is still that control, or return `refusal` unchanged. No mouse input was sent before this, and none is sent
    /// here.
    pub(crate) fn press_instead(&self, c: &Candidate, refusal: String) -> String {
        use crate::ax_candidates::is_sensitive;

        let snap_cell = self.snapshot.borrow();
        let mut h = self.h.borrow_mut();
        let Some(snap) = snap_cell.as_ref() else {
            return refusal;
        };
        // off, or `c` is not from the snapshot just taken
        if !ax_press_on(h.ax_press_environ.as_ref()) || snap.get(&c.id) != Some(c) {
            return refusal;
        }
        let Some(el_ref) = snap.element_ref(&c.id) else {
            return refusal;
        };
        if is_sensitive(&c.label) && *self.approve.borrow() != norm(&c.label) {
            return format!("refused: {} is a sensitive control without approval", py_repr(&c.label));
        }
        // an element that cannot be read is gone
        let live = (h.element_read)(el_ref).ok().flatten();
        let Some(live) = live else {
            return refusal;
        };
        let title = norm(&live.title);
        let moved = match live.frame {
            None => true,
            Some((fx, fy, fw, fh)) => {
                let (x, y, w, hh) = c.frame;
                [(fx, x), (fy, y), (fw, w), (fh, hh)].iter().any(|(a, b)| (a - b).abs() > AX_PRESS_SLACK)
            }
        };
        if live.pid != snap.pid
            || !live.enabled
            || !live.actions.iter().any(|a| a == "AXPress")
            || moved
            || (!title.is_empty() && title != norm(&c.label))
        {
            return refusal;
        }
        let role = if live.role.is_empty() { c.role.as_str() } else { live.role.as_str() };
        let what = format!("{role} {}", py_repr(&c.label));
        h.last_click = Some(what.clone());
        h.mark_acted();
        let err = match (h.element_press)(el_ref) {
            Ok(code) => code,
            Err(e) => {
                tracing::warn!("delegate: AXPress raised {}", error_kind(&e));
                -1
            }
        };
        if err != 0 {
            // Sent, and the app answered an error: some apps do that after acting. The effect is unknown, so this is
            // not a refusal (that would let the lane try again); the lane's own check after it decides.
            return format!("pressed {what} with AXPress, which answered error {err}; the effect is unknown");
        }
        let missed = refusal.strip_prefix("refused: ").unwrap_or_else(|| {
            let cut = refusal.char_indices().nth(9).map_or(refusal.len(), |(i, _)| i);
            &refusal[cut..]
        });
        format!("pressed {what} with AXPress (the hit-test missed: {missed})")
    }
}

impl Senses for LaneAdapter<'_> {
    /// Python `_LaneAdapter.snapshot`: an AX snapshot, timed under "ax", and the screen thumbnail baseline.
    fn snapshot(&self) -> anyhow::Result<Snapshot> {
        let mut h = self.h.borrow_mut();
        let t0 = (h.clock)();
        let screen = h.size();
        let snap = (h.ax_snapshot)(Some(screen));
        h.add_timing("ax", t0);
        let snap = snap?;
        *self.snapshot.borrow_mut() = Some(snap.clone());
        let previous = self.thumb0.borrow_mut().take();
        *self.thumb_before.borrow_mut() = previous;
        // no baseline means screen_changed() answers None
        let thumb = h.capture().and_then(|f| f.thumb().cloned());
        match thumb {
            Ok(t) => *self.thumb0.borrow_mut() = Some(t),
            Err(_) => {
                *self.thumb0.borrow_mut() = None;
                h.frame = None;
            }
        }
        Ok(snap)
    }

    fn text_visible(&self, s: &str) -> anyhow::Result<bool> {
        let needle = norm(s);
        if needle.is_empty() {
            return Ok(false);
        }
        if let Some(snap) = self.snapshot.borrow().as_ref() {
            if snap.elements.iter().any(|c| norm(&c.label).contains(&needle) || norm(&c.value).contains(&needle)) {
                return Ok(true);
            }
            if norm(&snap.title).contains(&needle) {
                return Ok(true);
            }
        }
        // no OCR, no sighting
        let lines = match self.h.borrow_mut().lines(None, "fast") {
            Ok(lines) => lines,
            Err(_) => return Ok(false),
        };
        Ok(lines.iter().any(|ln| norm(&ln.text).contains(&needle)))
    }

    fn focused(&self) -> Option<Candidate> {
        let el = {
            let h = self.h.borrow();
            (h.focused_element)()
        }?;
        Some(Candidate {
            id: "focused".to_string(),
            role: crate::ax_candidates::role_name(&el.role, ""),
            label: el.title,
            value: String::new(),
            frame: el.frame.unwrap_or((0, 0, 0, 0)),
            actions: Vec::new(),
            enabled: true,
            app: el.app,
            in_content: false,
            in_dialog: false,
            secure: false,
            editable: el.editable,
        })
    }

    fn frontmost_pid(&self) -> anyhow::Result<i64> {
        Ok(self.h.borrow_mut().front().pid)
    }

    /// Did the screen change since the step began? The lane asks this AFTER its post-click snapshot, so the step's
    /// start is the snapshot before the latest one.
    ///
    /// Until 2026-09-21 this compared the latest snapshot's frame with a fresh capture — the settled screen with
    /// itself — and so could never see a change: only a control whose AX value flips (a radio button) was ever
    /// verified, and a plain button ("next year") always read as unknown.
    fn screen_changed(&self) -> anyhow::Result<Option<bool>> {
        let thumb0 = self.thumb0.borrow();
        let Some(latest) = thumb0.as_ref() else {
            return Ok(None);
        };
        let mut h = self.h.borrow_mut();
        let answer = (|| -> anyhow::Result<bool> {
            if let Some(before) = self.thumb_before.borrow().as_ref() {
                let frame = match h.frame.clone() {
                    Some(f) => f,
                    None => h.capture()?,
                };
                return Ok(change_box(before, latest, h.skip_rows(&frame), CHANGE_MIN_PIXELS).is_some());
            }
            let frame = h.capture()?;
            Ok(change_box(latest, frame.thumb()?, h.skip_rows(&frame), CHANGE_MIN_PIXELS).is_some())
        })();
        Ok(answer.ok())
    }
}

impl Effectors for LaneAdapter<'_> {
    fn click_candidate(&self, c: &Candidate) -> anyhow::Result<String> {
        use crate::ax_candidates::is_sensitive;

        let (cx, cy) = Self::centre(c);
        let hit = {
            let h = self.h.borrow();
            (h.element_at)(cx, cy)
        };
        let el = match hit {
            // a failed hit-test is a refusal, never a click
            Err(e) => {
                return Ok(self.press_instead(c, format!("refused: hit-test failed ({})", error_kind(&e))));
            }
            Ok(None) => return Ok(self.press_instead(c, format!("refused: nothing under ({cx},{cy}) any more"))),
            Ok(Some(el)) => el,
        };
        let title = el.title.clone();
        if !el.app.is_empty() && !c.app.is_empty() && casefold(&el.app) != casefold(&c.app) {
            // Another app's window is over the point: the owner sees that window, so nothing is pressed under it.
            return Ok(format!("refused: ({cx},{cy}) is in {}, not {}", el.app, c.app));
        }
        if !Self::same_control(&el, c) {
            let refusal = format!("refused: under ({cx},{cy}) is {}, not {}", what(&el), py_repr(&c.label));
            return Ok(self.press_instead(c, refusal));
        }
        let approve = self.approve();
        if (is_sensitive(&title) || is_sensitive(&c.label)) && approve != norm(&title) && approve != norm(&c.label) {
            // Python `{title or c.label!r}`: the repr covers whichever is shown
            let shown = py_repr(if title.is_empty() { &c.label } else { &title });
            return Ok(format!("refused: {shown} is a sensitive control without approval"));
        }
        let mut h = self.h.borrow_mut();
        h.last_click = Some(what(&el));
        if !h.installed {
            h.mark_acted();
        }
        h.gui_click(cx, cy, 1)?;
        Ok(format!("clicked {} at ({cx},{cy})", what(&el)))
    }

    fn focus_and_type(&self, c: &Candidate, text: &str) -> anyhow::Result<String> {
        let line = self.click_candidate(c)?;
        if line.starts_with("refused:") {
            return Ok(line);
        }
        {
            let h = self.h.borrow();
            (h.sleep)(0.1);
        }
        let fe = match self.focused() {
            Some(f) if f.editable && f.frame != (0, 0, 0, 0) => f,
            other => {
                let what = match other {
                    Some(f) => format!("{} {}", f.role, py_repr(&f.label)),
                    None => "nothing".to_string(),
                };
                return Ok(format!("refused: focus is on {what}, not the {} {}", c.role, py_repr(&c.label)));
            }
        };
        let (a, b) = (fe.frame, c.frame);
        if [(a.0, b.0), (a.1, b.1), (a.2, b.2), (a.3, b.3)].iter().any(|(p, q)| (p - q).abs() > 2) {
            return Ok(format!(
                "refused: focus moved to {} {} at {}, not the {} {}",
                fe.role,
                py_repr(&fe.label),
                tuple_repr(&fe.frame),
                c.role,
                py_repr(&c.label)
            ));
        }
        self.h.borrow_mut().type_text(text, false)
    }

    fn press(&self, key: &str) -> anyhow::Result<String> {
        let mut h = self.h.borrow_mut();
        if !h.installed {
            h.mark_acted();
        }
        h.gui_press(key)?;
        Ok(format!("pressed {key}"))
    }

    fn settle(&self, secs: f64) -> anyhow::Result<f64> {
        let mut h = self.h.borrow_mut();
        h.settle(secs, 0.25)?;
        Ok(h.elapsed)
    }

    /// plan_executor._grant: the adapter has an `approve` attribute.
    fn grant(&self, approved: &str) {
        *self.approve.borrow_mut() = approved.to_string();
    }
}

impl DecisionSenses for LaneAdapter<'_> {
    /// Capture AX and screen together, returning a detached image in screen points. The loop reobserves both
    /// before acting: neither half is assumed atomic with the other.
    fn observe(&self) -> anyhow::Result<(Snapshot, DynamicImage)> {
        let snap = Senses::snapshot(self)?;
        let mut h = self.h.borrow_mut();
        let Some(frame) = h.frame.clone() else {
            return Err(LaneError::err("RuntimeError", "no screen frame"));
        };
        let img = h.points(&frame)?;
        Ok((snap, DynamicImage::ImageRgb8(img)))
    }

    fn image_matches(&self, before: &DynamicImage, after: &DynamicImage) -> bool {
        LaneAdapter::image_matches(before, after)
    }

    fn frontmost_pid(&self) -> anyhow::Result<i64> {
        Senses::frontmost_pid(self)
    }
}
