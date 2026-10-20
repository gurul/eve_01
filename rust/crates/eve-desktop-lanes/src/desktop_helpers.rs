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
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use image::{DynamicImage, GrayImage, RgbImage};
use indexmap::IndexMap;
use serde::Serialize;
use serde_json::{Map, Value};

use eve_decisions::Evaluate;

use crate::ax_candidates::{AxRef, Candidate, Rect, Snapshot};
use crate::decision_lane::DecisionSenses;
use crate::fast_lane::{Effectors, LaneDecider, Senses, StepAsk};

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
        todo!("desktop_helpers: Frame.pil")
    }

    /// 1/4-scale grayscale, for change detection (~5 ms on a 3024x1964 frame); Pillow's `convert("L").resize`.
    pub fn thumb(&self) -> anyhow::Result<&GrayImage> {
        todo!("desktop_helpers: Frame.thumb")
    }
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
        todo!("desktop_helpers: SystemClipboard.paste")
    }

    fn copy(&mut self, text: &str) -> anyhow::Result<()> {
        todo!("desktop_helpers: SystemClipboard.copy")
    }
}

/// Every backend of a Helpers, injectable for the tests (Python's keyword arguments to `Helpers(...)`).
/// `Backends::default()` is the real Mac.
pub struct Backends {
    pub capture: Box<dyn Fn() -> anyhow::Result<Frame> + Send>,
    /// `ocr(frame, level)`, level "accurate" | "fast"
    pub ocr: Box<dyn Fn(&Frame, &str) -> anyhow::Result<Vec<OcrBox>> + Send>,
    pub frontmost_fn: Box<dyn Fn() -> Front + Send>,
    pub element_at: Box<dyn Fn(i64, i64) -> anyhow::Result<Option<AxElementInfo>> + Send>,
    pub focused_element: Box<dyn Fn() -> Option<FocusedElement> + Send>,
    pub element_read: Box<dyn Fn(&AxRef) -> anyhow::Result<Option<LiveElement>> + Send>,
    pub element_press: Box<dyn Fn(&AxRef) -> anyhow::Result<i64> + Send>,
    /// The AX snapshot the lane adapter takes (Python `ax_snapshot(None, screen=...)`), given the screen in points.
    pub ax_snapshot: Box<dyn Fn(Option<(i64, i64)>) -> anyhow::Result<Snapshot> + Send>,
    /// `subprocess.run(argv, capture_output=True, timeout=…)` → the return code
    pub run: Box<dyn Fn(&[String], f64) -> anyhow::Result<i32> + Send>,
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
        todo!("desktop_helpers: the real backends")
    }
}

// ---- free functions ---------------------------------------------------------------------------------

/// Where two thumbnails differ: the bounding box (x, y, w, h) in thumb pixels, or None when fewer than
/// `min_pixels` changed. Rows above `skip_top` never count. Thumbnails of different sizes differ everywhere.
pub fn change_box(a_thumb: &GrayImage, b_thumb: &GrayImage, skip_top: u32, min_pixels: u32) -> Option<(u32, u32, u32, u32)> {
    todo!("desktop_helpers: change_box")
}

/// The app that owns a process, "" when unknown. AppKit first (0.3 ms), then libproc (0.02 ms).
pub fn app_name_for_pid(pid: i64) -> String {
    todo!("desktop_helpers: app_name_for_pid")
}

/// Python `_what`: "AXButton 'Play' in Spotify" — the role, the title (Python repr) when there is one, and the
/// owning app.
pub fn what(el: &AxElementInfo) -> String {
    todo!("desktop_helpers: _what")
}

/// The main display as a CGImage (physical pixels). Errors "screen capture failed — Screen Recording is not granted
/// to this python" when the capture comes back empty.
pub fn cg_capture() -> anyhow::Result<Frame> {
    todo!("desktop_helpers: cg_capture")
}

/// Vision text recognition. Boxes are normalized, bottom-left origin: bx, by, bw, bh.
pub fn vision_ocr(frame: &Frame, level: &str) -> anyhow::Result<Vec<OcrBox>> {
    todo!("desktop_helpers: vision_ocr")
}

/// Python `_ax_focused_pid`: the app that has keyboard focus, from AX — where a key press actually goes. 0 on error.
pub fn ax_focused_pid() -> i64 {
    todo!("desktop_helpers: _ax_focused_pid")
}

/// Python `_window_list_pid`: the owner of the frontmost normal (layer 0) on-screen window. 0 when there is none.
pub fn window_list_pid() -> i64 {
    todo!("desktop_helpers: _window_list_pid")
}

/// Python `_workspace_pid`: NSWorkspace's idea of the frontmost app — last resort only, see `focused_pid`.
pub fn workspace_pid() -> i64 {
    todo!("desktop_helpers: _workspace_pid")
}

/// Python `_focused_pid`: the frontmost app's pid, from sources that stay current in a long-lived process (AX,
/// then the window list, then NSWorkspace — whose notifications only arrive while a run loop spins).
pub fn focused_pid() -> i64 {
    focused_pid_from(&[&ax_focused_pid, &window_list_pid, &workspace_pid])
}

/// focused_pid over injectable probes, in order: the first non-zero pid, else 0.
pub fn focused_pid_from(probes: &[&dyn Fn() -> i64]) -> i64 {
    todo!("desktop_helpers: _focused_pid")
}

/// `{"app","bundle","title","pid"}` of the frontmost app; title "" on any AX error.
pub fn ax_frontmost() -> Front {
    todo!("desktop_helpers: ax_frontmost")
}

/// The accessibility element under a point (points, top-left origin), climbed to the nearest pressable ancestor.
/// None when nothing is there or Accessibility is not granted. Bench 2026-09-06: 1-24 ms.
pub fn ax_element_at(x: i64, y: i64) -> Option<AxElementInfo> {
    todo!("desktop_helpers: ax_element_at")
}

/// EVE_AX_PRESS: "0/false/no/off" off, "1/true/yes/on" on, anything else AX_PRESS_DEFAULT. `environ` None reads
/// the process environment.
pub fn ax_press_on(environ: Option<&HashMap<String, String>>) -> bool {
    todo!("desktop_helpers: ax_press_on")
}

/// A live element now, or None when it is gone (or is not an AX element).
pub fn ax_read(el: &AxRef) -> Option<LiveElement> {
    todo!("desktop_helpers: ax_read")
}

/// AXUIElementPerformAction(el, AXPress): the AXError code, 0 on success.
pub fn ax_press(el: &AxRef) -> anyhow::Result<i64> {
    todo!("desktop_helpers: ax_press")
}

/// Python `_matches`: does the model's expectation ("the blue Sign in button") describe this element?
pub fn matches(expect: &str, role: &str, title: &str) -> bool {
    todo!("desktop_helpers: _matches")
}

/// The element that has keyboard focus right now, or None. Read fresh every call — the lane checks it right after a
/// click, before pasting.
pub fn ax_focused_element() -> Option<FocusedElement> {
    todo!("desktop_helpers: ax_focused_element")
}

/// Python `_norm`: whitespace collapsed, case-folded.
pub fn norm(text: &str) -> String {
    todo!("desktop_helpers: _norm")
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
    pub(crate) installed: bool,
    pub(crate) menu_bar_points: i64,
    pub(crate) capture_fn: Box<dyn Fn() -> anyhow::Result<Frame> + Send>,
    pub(crate) ocr: Box<dyn Fn(&Frame, &str) -> anyhow::Result<Vec<OcrBox>> + Send>,
    pub(crate) frontmost_fn: Box<dyn Fn() -> Front + Send>,
    pub(crate) element_at: Box<dyn Fn(i64, i64) -> anyhow::Result<Option<AxElementInfo>> + Send>,
    pub(crate) focused_element: Box<dyn Fn() -> Option<FocusedElement> + Send>,
    pub(crate) element_read: Box<dyn Fn(&AxRef) -> anyhow::Result<Option<LiveElement>> + Send>,
    pub(crate) element_press: Box<dyn Fn(&AxRef) -> anyhow::Result<i64> + Send>,
    pub(crate) ax_snapshot: Box<dyn Fn(Option<(i64, i64)>) -> anyhow::Result<Snapshot> + Send>,
    pub(crate) run: Box<dyn Fn(&[String], f64) -> anyhow::Result<i32> + Send>,
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
        todo!("desktop_helpers: Helpers.__init__")
    }

    // -- wiring --

    /// Bind the helpers into the exec namespace and make every input call mark `acted`: returns the names bound
    /// (HELPER_NAMES, plus FAST_LANE_HELPERS when the fast lane is on) and turns on the acting wrap. Idempotent.
    pub fn install(&mut self) -> Vec<&'static str> {
        todo!("desktop_helpers: Helpers.install")
    }

    /// Python `_acting(pyautogui.click)`: a raw click through the wrap (marks acted, records what is under a
    /// 2-argument click, times "act"); before install() it is the bare call.
    pub fn gui_click(&mut self, x: i64, y: i64, clicks: u32) -> anyhow::Result<()> {
        todo!("desktop_helpers: Helpers._acting(click)")
    }

    /// Python `_acting(pyautogui.hotkey)`.
    pub fn gui_hotkey(&mut self, keys: &[&str]) -> anyhow::Result<()> {
        todo!("desktop_helpers: Helpers._acting(hotkey)")
    }

    /// Python `_acting(pyautogui.press)`.
    pub fn gui_press(&mut self, key: &str) -> anyhow::Result<()> {
        todo!("desktop_helpers: Helpers._acting(press)")
    }

    pub fn bind(&mut self, log: Box<dyn Fn(&str) + Send>, display: Box<dyn Fn(RgbImage) + Send>) {
        todo!("desktop_helpers: Helpers.bind")
    }

    pub fn begin(&mut self) {
        todo!("desktop_helpers: Helpers.begin")
    }

    /// Every site that clicks, types or presses keys: `acted` for the auto-screenshot, the sequence number so a
    /// settle from before this input is never reused after it.
    pub(crate) fn mark_acted(&mut self) {
        todo!("desktop_helpers: Helpers._mark_acted")
    }

    pub(crate) fn add_timing(&mut self, key: &str, t0: f64) {
        todo!("desktop_helpers: Helpers._add_timing")
    }

    /// The current call's timing, rounded to 0.01 ms, for the worker reply.
    pub fn timing_ms(&self) -> IndexMap<String, f64> {
        todo!("desktop_helpers: Helpers.timing_ms")
    }

    pub(crate) fn front(&mut self) -> Front {
        todo!("desktop_helpers: Helpers._front")
    }

    pub(crate) fn describe_point(&mut self, x: i64, y: i64) -> Option<String> {
        todo!("desktop_helpers: Helpers._describe_point")
    }

    /// Click the control under (x, y) — snapped to its centre — after checking it is the one you meant. `expect` is
    /// a few words from the screenshot ("Sign in button"). A mismatch does NOT click; it tells you what is there.
    pub fn click_element(&mut self, x: i64, y: i64, expect: &str, clicks: u32) -> anyhow::Result<String> {
        todo!("desktop_helpers: Helpers.click_element")
    }

    // -- apps and pages --

    /// Launch or focus an app with `open -a`, then wait (≤ MAX_WAIT_SECS; Python default 8) until it is frontmost.
    /// ValueError for a path or an option, RuntimeError "no app called …" when `open` fails.
    pub fn open_app(&mut self, name: &str, wait: f64) -> anyhow::Result<String> {
        todo!("desktop_helpers: Helpers.open_app")
    }

    /// Open a URL in the default browser (or `app`) and wait (Python default 8) for it to come up. Only http,
    /// https and mailto.
    pub fn open_url(&mut self, url: &str, app: Option<&str>, wait: f64) -> anyhow::Result<String> {
        todo!("desktop_helpers: Helpers.open_url")
    }

    pub fn frontmost(&mut self) -> Front {
        todo!("desktop_helpers: Helpers.frontmost")
    }

    // -- reading the screen --

    /// OCR the screen: lines in click coordinates, top to bottom, capped (Python level default "accurate").
    pub fn screen_text(&mut self, region: Option<Rect>, level: &str) -> anyhow::Result<Vec<TextLine>> {
        todo!("desktop_helpers: Helpers.screen_text")
    }

    /// Python `_lines`.
    pub(crate) fn lines(&mut self, region: Option<Rect>, level: &str) -> anyhow::Result<Vec<TextLine>> {
        todo!("desktop_helpers: Helpers._lines")
    }

    /// The first on-screen line equal to `text` (else containing it), with its centre cx, cy.
    pub fn find_text(&mut self, text: &str, region: Option<Rect>, level: &str) -> anyhow::Result<Option<FoundText>> {
        todo!("desktop_helpers: Helpers.find_text")
    }

    /// Python `_find`.
    pub(crate) fn find(&mut self, text: &str, region: Option<Rect>, level: &str) -> anyhow::Result<Option<FoundText>> {
        todo!("desktop_helpers: Helpers._find")
    }

    /// Find `text` with accurate OCR (waiting up to `timeout`, Python default 3) and click its centre. LookupError
    /// when it is not on screen.
    pub fn click_text(&mut self, text: &str, region: Option<Rect>, timeout: f64, clicks: u32) -> anyhow::Result<String> {
        todo!("desktop_helpers: Helpers.click_text")
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
        todo!("desktop_helpers: Helpers.wait_for")
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
        todo!("desktop_helpers: Helpers._wait_for")
    }

    /// Wait until two consecutive captures `interval` apart look the same (Python defaults: timeout 5, interval
    /// 0.25).
    pub fn wait_settled(&mut self, timeout: f64, interval: f64) -> anyhow::Result<bool> {
        todo!("desktop_helpers: Helpers.wait_settled")
    }

    /// Python `_settle(timeout, interval=0.25)`: two captures `interval` apart that look the same; false past
    /// `timeout`. Remembers a completed attempt, including a timeout, for the auto-screenshot.
    pub(crate) fn settle(&mut self, timeout: f64, interval: f64) -> anyhow::Result<bool> {
        todo!("desktop_helpers: Helpers._settle")
    }

    /// Python `_settle_loop`.
    pub(crate) fn settle_loop(&mut self, timeout: f64, interval: f64, t0: f64) -> anyhow::Result<bool> {
        todo!("desktop_helpers: Helpers._settle_loop")
    }

    // -- typing --

    /// Type any text (clipboard paste: emoji and accents survive), optionally pressing enter.
    pub fn type_text(&mut self, text: &str, submit: bool) -> anyhow::Result<String> {
        todo!("desktop_helpers: Helpers.type_text")
    }

    // -- looking --

    /// Display a close-up of that point region at the frame's physical scale.
    pub fn zoom(&mut self, x: i64, y: i64, w: i64, h: i64) -> anyhow::Result<String> {
        todo!("desktop_helpers: Helpers.zoom")
    }

    /// A fresh screenshot plus the context line.
    pub fn observe(&mut self, note: &str) -> anyhow::Result<String> {
        todo!("desktop_helpers: Helpers.observe")
    }

    // -- used by the worker --

    /// The screen in points, like pyautogui.screenshot() but from a CG capture.
    pub fn screenshot(&mut self) -> anyhow::Result<RgbImage> {
        todo!("desktop_helpers: Helpers.screenshot")
    }

    /// Wait (up to max_wait) for the screen to stop changing, then the screenshot of that last frame. When a helper
    /// already waited after the last input and that frame is under SETTLE_REUSE_SECS old, reuse it.
    pub fn settled_screenshot(&mut self, max_wait: f64) -> anyhow::Result<RgbImage> {
        todo!("desktop_helpers: Helpers.settled_screenshot")
    }

    pub(crate) fn settle_is_fresh(&self) -> bool {
        todo!("desktop_helpers: Helpers._settle_is_fresh")
    }

    // -- the fast lane --

    /// Hand a narrow run of clicks on labelled controls in the frontmost app to the lane. `steps` is an ordered list
    /// of exact labels, one click each (fast_lane::run_script); otherwise `objective` is one run
    /// (fast_lane::run_delegate). Who decides is `lane_decide`. Returns the one-line result the model reads; every
    /// step and the final line are logged for the human.
    pub fn delegate(&mut self, call: DelegateCall) -> String {
        todo!("desktop_helpers: Helpers.delegate")
    }

    /// Python `_lane_decide`: "jev" only while an asker exists: a hosted model that is not configured must not turn
    /// the lane off, so the keyword gate decides alone instead.
    pub fn lane_decide_mode(&self) -> String {
        todo!("desktop_helpers: Helpers._lane_decide")
    }

    /// Python `_script_decide`: a script's steps are exact labels the planner read off the screen: the keyword
    /// gate's case.
    pub fn script_decide(&self) -> String {
        todo!("desktop_helpers: Helpers._script_decide")
    }

    /// The front window as the planner is shown it: `label (role, value)` per pressable or editable control, in the
    /// lane's own ranking order — never the window title, never a field's contents (Python default max_lines 80).
    /// `{"app", "lines", "dialog", "truncated"}`.
    pub fn outline(&mut self, max_lines: usize) -> anyhow::Result<Value> {
        todo!("desktop_helpers: Helpers.outline")
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
        todo!("desktop_helpers: Helpers.run_plan")
    }

    /// The router (lane_router::route) over this desktop: try to finish `goal` with keyword-decided clicks before
    /// any planner call. Returns RouteResult::to_dict(); never fails.
    pub fn lane_first(&mut self, goal: &str, dry_run: bool) -> Value {
        todo!("desktop_helpers: Helpers.lane_first")
    }

    /// The opt-in visual loop, using the same hit-test and AXPress effectors as the keyword lane.
    pub fn decisions_first(&mut self, goal: &str, client: &mut dyn Evaluate, dry_run: bool) -> Value {
        todo!("desktop_helpers: Helpers.decisions_first")
    }

    /// "frontmost: {app} — {title!r}; screen {w}x{h}; {HH:MM}".
    pub fn context_line(&mut self) -> String {
        todo!("desktop_helpers: Helpers.context_line")
    }

    /// What changed since the previous exec ended, and where, in click coordinates. "[after your input]" when this
    /// step clicked, typed or pressed keys, "[after]" when it only looked.
    pub fn after_line(&mut self) -> anyhow::Result<String> {
        todo!("desktop_helpers: Helpers.after_line")
    }

    // -- internals --

    /// Points per thumb pixel (2.0 on a 2x Retina frame).
    pub(crate) fn thumb_scale(&self, frame: &Frame) -> f64 {
        todo!("desktop_helpers: Helpers._thumb_scale")
    }

    /// Thumb rows covered by the menu bar, which never count as a change.
    pub(crate) fn skip_rows(&self, frame: &Frame) -> u32 {
        todo!("desktop_helpers: Helpers._skip_rows")
    }

    pub(crate) fn capture(&mut self) -> anyhow::Result<Arc<Frame>> {
        todo!("desktop_helpers: Helpers._capture")
    }

    /// The screen in points (`pyautogui.size()`).
    pub(crate) fn size(&self) -> (i64, i64) {
        todo!("desktop_helpers: Helpers._size")
    }

    /// The frame resized to points (Lanczos), timed under "resize".
    pub(crate) fn points(&mut self, frame: &Frame) -> anyhow::Result<RgbImage> {
        todo!("desktop_helpers: Helpers._points")
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
        todo!("desktop_helpers: _LaneAdapter.__init__")
    }

    /// The Helpers behind the adapter (open_app / open_url / frontmost for run_plan).
    pub fn helpers(&self) -> RefMut<'_, &'a mut Helpers> {
        self.h.borrow_mut()
    }

    /// The approval the effector gate compares against (normalised), "" for none.
    pub fn approve(&self) -> String {
        self.approve.borrow().clone()
    }

    /// Any pixel change below the macOS menu bar (32 rows) invalidates a pending visual decision.
    pub fn image_matches(before: &DynamicImage, after: &DynamicImage) -> bool {
        todo!("desktop_helpers: _LaneAdapter.image_matches")
    }

    /// Python `_centre`.
    pub fn centre(c: &Candidate) -> (i64, i64) {
        todo!("desktop_helpers: _LaneAdapter._centre")
    }

    /// Python `_same_control`: is the hit-tested element the candidate? Equal titles, or — for a row / cell / link
    /// whose label came from a descendant static text — an element inside the candidate's frame (± 2 pt) whose own
    /// title is empty or part of that label.
    pub fn same_control(el: &AxElementInfo, c: &Candidate) -> bool {
        todo!("desktop_helpers: _LaneAdapter._same_control")
    }

    /// Python `_press_instead`: the hit-test missed: AXPress the element the snapshot read for `c`, after checking it
    /// is still that control, or return `refusal` unchanged. No mouse input was sent before this, and none is sent
    /// here.
    pub(crate) fn press_instead(&self, c: &Candidate, refusal: String) -> String {
        todo!("desktop_helpers: _LaneAdapter._press_instead")
    }
}

impl Senses for LaneAdapter<'_> {
    /// Python `_LaneAdapter.snapshot`: an AX snapshot, timed under "ax", and the screen thumbnail baseline.
    fn snapshot(&self) -> anyhow::Result<Snapshot> {
        todo!("desktop_helpers: _LaneAdapter.snapshot")
    }

    fn text_visible(&self, s: &str) -> anyhow::Result<bool> {
        todo!("desktop_helpers: _LaneAdapter.text_visible")
    }

    fn focused(&self) -> Option<Candidate> {
        todo!("desktop_helpers: _LaneAdapter.focused")
    }

    fn frontmost_pid(&self) -> anyhow::Result<i64> {
        todo!("desktop_helpers: _LaneAdapter.frontmost_pid")
    }

    /// Did the screen change since the step began? The lane asks this AFTER its post-click snapshot, so the step's
    /// start is the snapshot before the latest one.
    fn screen_changed(&self) -> anyhow::Result<Option<bool>> {
        todo!("desktop_helpers: _LaneAdapter.screen_changed")
    }
}

impl Effectors for LaneAdapter<'_> {
    fn click_candidate(&self, c: &Candidate) -> anyhow::Result<String> {
        todo!("desktop_helpers: _LaneAdapter.click_candidate")
    }

    fn focus_and_type(&self, c: &Candidate, text: &str) -> anyhow::Result<String> {
        todo!("desktop_helpers: _LaneAdapter.focus_and_type")
    }

    fn press(&self, key: &str) -> anyhow::Result<String> {
        todo!("desktop_helpers: _LaneAdapter.press")
    }

    fn settle(&self, secs: f64) -> anyhow::Result<f64> {
        todo!("desktop_helpers: _LaneAdapter.settle")
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
        todo!("desktop_helpers: _LaneAdapter.observe")
    }

    fn image_matches(&self, before: &DynamicImage, after: &DynamicImage) -> bool {
        LaneAdapter::image_matches(before, after)
    }

    fn frontmost_pid(&self) -> anyhow::Result<i64> {
        Senses::frontmost_pid(self)
    }
}
