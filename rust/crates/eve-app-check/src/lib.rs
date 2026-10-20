//! Test a freshly built app in a phone-sized headless browser before the owner ever sees it.
//!
//! Port of `bridge/src/eve_bridge/app_check.py`. apps_maker hands every app Claude writes to [`check_app`]. It
//! runs the page in Chromium at an iPhone's size with touch, with a stand-in Telegram (`window.Telegram.WebApp`:
//! theme and its CSS variables, MainButton, BackButton, haptics, popups that answer yes) and a stand-in
//! `window.eve` whose storage lives here, outside the page, so a page reload or a crash cannot lose it. Then it
//! uses the app the way a person would in the first minute:
//!
//! 1. Opens it (empty, or with `seed`: the owner's real data when an existing app is being changed) in
//!    Telegram's DARK theme, the one a fixed color breaks, and waits for it to settle.
//! 2. View by view: fills every visible field with a value that fits it (the example in its placeholder, a
//!    number inside its range, what a prefilled field already holds; a select keeps a real choice), taps every
//!    button, checkbox and tab it can reach, reading the page again whenever a tap re-renders it (at most
//!    [`MAX_PER_GROUP`] of a row of siblings such as an icon grid, [`MAX_TAPS`] in all). When a view is used up
//!    it presses the MainButton (the form's submit, or "New …" again), else Telegram's Back button. When a press
//!    shows an error next to a field, the empty or rejected fields are filled again. Destructive controls
//!    (delete, reset, clear…) come last in each view; a Cancel or Close after the MainButton had its chance. A
//!    control under a toast is tried again once the toast has had time to go.
//! 3. Closes the app the way Telegram does, with no warning: a save that only lands after that is lost on a
//!    phone. Then lets an hour pass on the page's clock with the app open (a running timer finishes).
//! 4. Opens it again in the light theme with the richest data it saved (the most records, so the photo shows
//!    the app in use; the screenshot), and once more at a small Android phone's width with the data as it was
//!    left, because an app that renders its own saved data wrongly is the most common way a tracker breaks on
//!    day two.
//!
//! What it reports is what a person would hit, each with the action that caused it: uncaught errors and console
//! errors, a tap that reloaded the whole page, requests to anywhere the app may not reach, a library that did
//! not load, a page wider than the phone or laid out for a desktop, an app that never saved after being used, a
//! save that arrives after closing, an app that froze, and on every view it visits: text that does not read in
//! dark mode, a selected tab that looks like the others, a native date field left in light colors, tap targets
//! too small for a finger, inputs small enough to make iPhone zoom, a chart shape stretched by a CSS rule, and a
//! control that stays covered. False alarms cost a repair round and can make a working app worse, so each check
//! only fires on something a person would really see.
//!
//! Nothing leaves the Mac except, when an app loads a library, requests to cdn.jsdelivr.net: every HTTP request
//! goes through the check's router, WebSockets are answered here and closed (an attempt is a finding), and
//! Chromium starts with a host resolver that knows no other name and with WebRTC kept off the network. The page
//! is served sandboxed, as the Mini App server serves it (miniapp.APP_CSP), so browser storage throws here as it
//! does there.
//!
//! That first minute is blind: it proves nothing broke, not that the app does what it is for. So when the maker
//! wrote journeys for the app (the core flow as steps, and the text it must show afterwards), each one is then
//! walked by Jev, in a fresh copy of this same sandbox with no data (eve-jev-verify, [`journeys`]). A journey
//! that does not go through is a problem in the same report; Jev being unreachable is "journeys not run", never
//! one.
//!
//! A check that breaks after the app opened keeps what it found: a renderer crash (an app that allocates without
//! end) is a finding, and only a checker that could not start at all is a skip.
//!
//! Python drives Chromium with Playwright; this crate drives it over the DevTools protocol ([`cdp`]).

pub mod cdp;
pub mod scripts;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use base64::Engine;
use chrono::{Datelike, NaiveDate};
use eve_jev_verify::{Journey, JourneyResult, JourneyRun, JevDown, PageError, Verifier, WalkOptions};
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use futures::{FutureExt, StreamExt};
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Map, Value};

pub use cdp::{Browser, Context, ContextOptions, Page};
pub use scripts::*;

pub const APP_URL: &str = "https://app.eve.test/apps/check/";
pub const APP_HOST: &str = "app.eve.test";
/// reached for real: an app's library
pub const ALLOWED_HOSTS: [&str; 1] = ["cdn.jsdelivr.net"];
/// answered here: the stand-in Telegram replaces its script
pub const STUBBED_HOSTS: [&str; 1] = ["telegram.org"];

/// A viewport in CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
}

/// an iPhone 14/15
pub const PHONE: Viewport = Viewport { width: 390, height: 844 };
/// a small Android phone
pub const SMALL_PHONE: Viewport = Viewport { width: 360, height: 740 };
pub const MAX_TAPS: usize = 40;
pub const MAX_FIELDS: usize = 30;
/// taps on siblings of one kind (an icon grid, a row of days)
pub const MAX_PER_GROUP: usize = 3;
/// a re-rendering tap ends a round; taps and time bound it
pub const MAX_ROUNDS: usize = 40;
pub const MAX_ISSUES: usize = 12;
pub const MAX_TRACE: usize = 200;
pub const SETTLE_MS: u64 = 600;
pub const TAP_WAIT_MS: u64 = 150;
pub const CLICK_TIMEOUT_MS: u64 = 1200;
pub const LOAD_TIMEOUT_MS: u64 = 15000;
/// an app stuck in a loop cannot hold a build forever
pub const CHECK_TIMEOUT_S: f64 = 60.0;
/// the clock jump at the end: a pomodoro or countdown finishes
pub const AN_HOUR_MS: u64 = 61 * 60 * 1000;
/// a save started by the close itself (pagehide) still counts
pub const CLOSE_GRACE_MS: u64 = 250;
/// how long a save that comes after the close is waited for
pub const LATE_MS: u64 = 900;
/// a toast over a control is waited out, this long at a time
pub const COVERED_WAIT_MS: u64 = 1000;
/// longer than any toast: something stays on top of the control
pub const COVERED_FOR_S: f64 = 3.5;
/// Telegram's bottom bar with the MainButton, in CSS pixels
pub const BAR_H: u32 = 100;
/// How the app is served, as the real server does (miniapp.APP_CSP starts with the same sandbox): an opaque
/// origin, so browser storage and cookies throw, and a form cannot navigate the page away.
pub const SANDBOX: &str =
    "sandbox allow-scripts allow-forms allow-modals allow-popups allow-popups-to-escape-sandbox allow-downloads";
/// No name resolves but the library CDN's (an IP address is refused too), and WebRTC never leaves by UDP: an
/// app's own code cannot reach the network or the Mac's local services during the check.
pub const CHROMIUM_ARGS: [&str; 2] = [
    "--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE cdn.jsdelivr.net",
    "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
];

/// Telegram's theme (themeParams) as the stand-in reports it, in its key order.
pub type Theme = [(&'static str, &'static str)];

/// Telegram's light stock theme.
pub const LIGHT: &Theme = &[
    ("bg_color", "#ffffff"),
    ("text_color", "#000000"),
    ("hint_color", "#8e8e93"),
    ("link_color", "#2f6fde"),
    ("button_color", "#2f6fde"),
    ("button_text_color", "#ffffff"),
    ("secondary_bg_color", "#f2f2f7"),
    ("header_bg_color", "#ffffff"),
    ("accent_text_color", "#2f6fde"),
    ("section_bg_color", "#ffffff"),
    ("section_header_text_color", "#6d6d72"),
    ("subtitle_text_color", "#8e8e93"),
    ("destructive_text_color", "#ff3b30"),
    ("bottom_bar_bg_color", "#f2f2f7"),
    ("section_separator_color", "#c8c7cc"),
];

/// Telegram's dark stock theme.
pub const DARK: &Theme = &[
    ("bg_color", "#212121"),
    ("text_color", "#ffffff"),
    ("hint_color", "#aaaaaa"),
    ("link_color", "#8774e1"),
    ("button_color", "#8774e1"),
    ("button_text_color", "#ffffff"),
    ("secondary_bg_color", "#0f0f0f"),
    ("header_bg_color", "#212121"),
    ("accent_text_color", "#8774e1"),
    ("section_bg_color", "#212121"),
    ("section_header_text_color", "#aaaaaa"),
    ("subtitle_text_color", "#aaaaaa"),
    ("destructive_text_color", "#ff595a"),
    ("bottom_bar_bg_color", "#212121"),
    ("section_separator_color", "#000000"),
];

fn theme_get(theme: &Theme, key: &str) -> &'static str {
    theme.iter().find(|(k, _)| *k == key).map(|(_, v)| *v).unwrap_or("")
}

/// Each look finding: its template, with `{action}` and `{items}` (Python's str.format, `{{` is a brace).
pub const LOOKS: [(&str, &str); 7] = [
    (
        "tiny",
        "Tap targets too small for a finger while {action}: {items} (under 32x32px with no wider hit area). \
Make them at least 44x44px, with padding or a ::after that widens the hit area.",
    ),
    (
        "font",
        "Inputs with text under 16px while {action}: {items}. iPhone zooms the page in when one is focused; \
use font-size: 16px or more on inputs, selects and textareas.",
    ),
    (
        "shape",
        "A chart shape is drawn at another size than its SVG attribute says while {action}: {items}. A CSS \
rule (width, height, x, y) on its class overrides the attribute; give SVG shapes their own classes \
with no layout rules.",
    ),
    (
        "contrast",
        "In Telegram's dark theme, text does not stand out from its background while {action}: {items} \
(contrast under 2.5:1). A color is likely fixed instead of coming from the theme variables.",
    ),
    (
        "selected",
        "In Telegram's dark theme, a selected item looks like the unselected one next to it while \
{action}: {items}. Give the selected state a clearly different fill (the button color with the \
button text color, or an accent underline) and weight.",
    ),
    (
        "picker",
        "In Telegram's dark theme, the date or time field {items} draws its picker in light colors (a dark \
icon on a dark field). Declare :root {{ color-scheme: light dark; }} and set \
document.documentElement.style.colorScheme from Telegram's colorScheme.",
    ),
    (
        "corner",
        "eve's Change pencil sits over the top-right corner of every view (a 44x44px button, 6px from the top \
and the right edge, scrolling with the page), and it covers {items} while {action}. Keep the top-right \
56x56px of every view free of controls: give the header padding-right: 56px.",
    ),
];

/// Where eve's Change pencil sits (apps_maker.EVE_JS, from the top-right corner of the page, in CSS pixels).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pencil {
    pub top: u32,
    pub right: u32,
    pub size: u32,
}

pub const PENCIL: Pencil = Pencil { top: 6, right: 6, size: 44 };

impl Pencil {
    /// `{"top": 6, "right": 6, "size": 44}`, as LOOK_JS reads it.
    pub fn as_dict(&self) -> Value {
        json!({"top": self.top, "right": self.right, "size": self.size})
    }
}

/// Python's `template.format(action=..., items=...)`: one pass, so a value is never read as a field.
fn format_look(template: &str, action: &str, items: &str) -> String {
    let mut out = String::with_capacity(template.len() + action.len() + items.len());
    let mut rest = template;
    while let Some(i) = rest.find(['{', '}']) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if let Some(t) = tail.strip_prefix("{{") {
            out.push('{');
            rest = t;
        } else if let Some(t) = tail.strip_prefix("}}") {
            out.push('}');
            rest = t;
        } else if let Some(t) = tail.strip_prefix("{action}") {
            out.push_str(action);
            rest = t;
        } else if let Some(t) = tail.strip_prefix("{items}") {
            out.push_str(items);
            rest = t;
        } else {
            out.push_str(&tail[..1]);
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

// ---- the report ------------------------------------------------------------------------------------------

/// What one check found.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckReport {
    pub ok: bool,
    pub issues: Vec<String>,
    pub taps: usize,
    pub fields: usize,
    pub saves: usize,
    pub screenshot: Option<Vec<u8>>,
    /// 'the journey "…"' (its last screen) or "the scripted check"
    pub photo_from: String,
    /// what the app had saved when the check ended (Null: nothing)
    pub saved: Value,
    /// the data the screenshot shows: the richest it saved (Null: nothing)
    pub photo_data: Value,
    pub secs: f64,
    /// why no check ran (no Chromium): not a pass, not a fail
    pub skipped: String,
    /// every action, in order: for a person reading a check
    pub trace: Vec<String>,
    /// Jev walking the maker's journeys (None: the app has none)
    pub journeys: Option<JourneyRun>,
}

impl Default for CheckReport {
    fn default() -> Self {
        Self {
            ok: true,
            issues: Vec::new(),
            taps: 0,
            fields: 0,
            saves: 0,
            screenshot: None,
            photo_from: String::new(),
            saved: Value::Null,
            photo_data: Value::Null,
            secs: 0.0,
            skipped: String::new(),
            trace: Vec::new(),
            journeys: None,
        }
    }
}

impl CheckReport {
    pub fn summary(&self) -> String {
        if !self.skipped.is_empty() {
            return format!("not tested ({})", self.skipped);
        }
        let walked = self.journeys.as_ref().map(JourneyRun::summary).unwrap_or_default();
        let head = if self.ok { "passed".to_string() } else { format!("{} issue(s)", self.issues.len()) };
        let mut s = format!("{head}: {} fields filled, {} taps, {} saves", self.fields, self.taps, self.saves);
        if !walked.is_empty() {
            s.push_str(&format!(", {walked}"));
        }
        s
    }

    /// What the check's use of the app created ("habits 2, habits.done 5"), or "no records": whether it got as
    /// far as the app's core flow, for a person reading an eval.
    pub fn made(&self) -> String {
        let shape = data_shape(&self.photo_data);
        if shape.is_empty() {
            "no records".into()
        } else {
            shape
        }
    }
}

fn nonempty_container(v: &Value) -> Option<usize> {
    match v {
        Value::Array(a) if !a.is_empty() => Some(a.len()),
        Value::Object(o) if !o.is_empty() => Some(o.len()),
        _ => None,
    }
}

/// What saved data holds, in a few words ("habits 2, habits.done 5"): whether the check got as far as making
/// the app's records, for a person reading the summary.
pub fn data_shape(value: &Value) -> String {
    let Value::Object(map) = value else { return String::new() };
    let mut counts: indexmap::IndexMap<String, usize> = indexmap::IndexMap::new();
    for (key, v) in map {
        let Some(n) = nonempty_container(v) else { continue };
        counts.insert(key.clone(), n);
        let items: Vec<&Value> = match v {
            Value::Array(a) => a.iter().collect(),
            Value::Object(o) => o.values().collect(),
            _ => Vec::new(),
        };
        for item in items {
            if let Value::Object(o) = item {
                for (k2, v2) in o {
                    if let Some(n2) = nonempty_container(v2) {
                        *counts.entry(format!("{key}.{k2}")).or_insert(0) += n2;
                    }
                }
            }
        }
    }
    counts.iter().take(6).map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(", ")
}

/// How much is in saved data: its count of values. Adding a record raises it, a delete lowers it.
pub fn richness(raw: Option<&str>) -> usize {
    fn count(v: &Value) -> usize {
        match v {
            Value::Object(o) => o.values().map(count).sum::<usize>() + 1,
            Value::Array(a) => a.iter().map(count).sum::<usize>() + 1,
            _ => 1,
        }
    }
    match raw {
        Some(r) if !r.is_empty() => serde_json::from_str::<Value>(r).map(|v| count(&v)).unwrap_or(0),
        _ => 0,
    }
}

// ---- one app's check: its storage and what went wrong ------------------------------------------------------

#[derive(Debug, Default)]
struct SessState {
    stored: Option<String>,
    richest: Option<String>,
    saves: usize,
    loads: usize,
    primary_taps: usize,
    action: String,
    trace: Vec<String>,
    /// Telegram closed the app: a save now is lost
    closed: bool,
    late: Option<String>,
    /// the kinds of look finding already reported
    looked: HashSet<String>,
    /// a page was opened: a failure from here on is the app's too
    opened: bool,
    crashed: bool,
}

/// One app's check: its storage and what went wrong, kept outside the page so a reload cannot lose them.
#[derive(Debug)]
pub struct Session {
    st: Mutex<SessState>,
    /// what went wrong, in order, each once (jev_verify reads the page's errors from here)
    pub issues: Mutex<Vec<String>>,
}

impl Session {
    /// `seed`: the data the app starts with, written as Python's `json.dumps` writes it (JSON null is Python's
    /// None: no seed).
    pub fn new(seed: Option<&Value>) -> Self {
        let stored = seed.filter(|s| !s.is_null()).map(py_dumps);
        Session {
            st: Mutex::new(SessState {
                richest: stored.clone(),
                stored,
                action: "opening the app".into(),
                ..Default::default()
            }),
            issues: Mutex::new(Vec::new()),
        }
    }

    pub fn note(&self, text: impl Into<String>) {
        let text = text.into();
        let mut issues = self.issues.lock();
        if !issues.contains(&text) {
            issues.push(text);
        }
    }

    pub fn step(&self, text: impl Into<String>) {
        let text = text.into();
        let mut st = self.st.lock();
        st.action = text.clone();
        if st.trace.len() < MAX_TRACE && st.trace.last() != Some(&text) {
            st.trace.push(text);
        }
    }

    /// What is being done now ("tapping \"Add\""), for the findings it causes.
    pub fn action(&self) -> String {
        self.st.lock().action.clone()
    }

    fn set_action(&self, text: String) {
        self.st.lock().action = text;
    }

    /// `eve.load()`: the raw JSON stored, or None.
    pub fn load(&self) -> Option<String> {
        let mut st = self.st.lock();
        st.loads += 1;
        st.stored.clone()
    }

    /// `eve.save(raw)`: after Telegram closed the app it only remembers the late save.
    pub fn save(&self, raw: String) -> bool {
        let mut st = self.st.lock();
        if st.closed {
            st.late = Some(raw);
            return true;
        }
        st.saves += 1;
        if richness(Some(&raw)) >= richness(st.richest.as_deref()) {
            st.richest = Some(raw.clone());
        }
        st.stored = Some(raw);
        true
    }

    /// What is stored, parsed (Null: nothing, or not JSON).
    pub fn data(&self) -> Value {
        parse_or_null(self.st.lock().stored.as_deref())
    }

    pub fn issues(&self) -> Vec<String> {
        self.issues.lock().clone()
    }

    pub fn trace(&self) -> Vec<String> {
        self.st.lock().trace.clone()
    }

    pub fn saves(&self) -> usize {
        self.st.lock().saves
    }

    pub fn loads(&self) -> usize {
        self.st.lock().loads
    }

    fn crashed(&self) -> bool {
        self.st.lock().crashed
    }
}

/// Python's `json.dumps(value)`: `", "` and `": "` separators, non-ASCII escaped (`ensure_ascii`), floats in
/// Python's repr. The seed is stored in these bytes, as Python stores it, so a save compared with it (the save
/// after closing) compares the same strings.
pub fn py_dumps(v: &Value) -> String {
    let mut out = String::new();
    dumps_into(&mut out, v);
    out
}

fn dumps_into(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => match (n.is_f64(), n.as_f64()) {
            (true, Some(f)) => out.push_str(&float_repr(f)),
            _ => out.push_str(&n.to_string()),
        },
        Value::String(s) => dumps_str(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dumps_into(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dumps_str(out, k);
                out.push_str(": ");
                dumps_into(out, item);
            }
            out.push('}');
        }
    }
}

fn dumps_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (' '..='~').contains(&c) => out.push(c),
            c => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", unit));
                }
            }
        }
    }
    out.push('"');
}

/// `repr(float)`: shortest round-trip digits, scientific below 1e-4 and from 1e16.
fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let sci = format!("{:e}", x.abs());
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let sign = if x < 0.0 { "-" } else { "" };
    if (-4..16).contains(&exp) {
        if exp >= 0 {
            let int_len = exp as usize + 1;
            if digits.len() <= int_len {
                format!("{sign}{digits}{}.0", "0".repeat(int_len - digits.len()))
            } else {
                format!("{sign}{}.{}", &digits[..int_len], &digits[int_len..])
            }
        } else {
            format!("{sign}0.{}{digits}", "0".repeat((-exp - 1) as usize))
        }
    } else {
        let frac = if digits.len() > 1 { format!(".{}", &digits[1..]) } else { String::new() };
        let esign = if exp < 0 { '-' } else { '+' };
        format!("{sign}{}{frac}e{esign}{:02}", &digits[..1], exp.abs())
    }
}

/// Python's `s.splitlines()[0]`: the text up to its first line boundary (Python knows more of them than `\n`);
/// None for an empty string, where Python's `[0]` raises.
fn py_first_line(s: &str) -> Option<&str> {
    if s.is_empty() {
        return None;
    }
    let end = s
        .find(['\n', '\r', '\u{0b}', '\u{0c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}', '\u{2029}'])
        .unwrap_or(s.len());
    Some(&s[..end])
}

fn parse_or_null(raw: Option<&str>) -> Value {
    raw.and_then(|r| serde_json::from_str(r).ok()).unwrap_or(Value::Null)
}

static WHERE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&(regex::escape(APP_HOST) + r"/apps/check/[^:\s]*:(\d+):\d+")).unwrap());

/// The first line of the app's own code in a stack (" (line 212)"), so a repair can go straight to it.
pub fn where_(stack: &str) -> String {
    WHERE.captures(stack).map(|m| format!(" (line {})", &m[1])).unwrap_or_default()
}

// ---- what a person would type ----------------------------------------------------------------------------

/// What a person would type, by what the field is for. First match wins, so the specific comes first.
const NUMBERS: [(&str, f64); 14] = [
    (r"\breps?\b|repetitions", 8.0),
    (r"\bsets?\b", 3.0),
    (r"weight|\bkg\b|\blbs?\b|pounds", 60.0),
    (r"minutes?|\bmins?\b", 25.0),
    (r"hours?|\bhrs?\b", 2.0),
    (r"\bml\b|millilit", 250.0),
    (r"pages?", 20.0),
    (r"steps", 8000.0),
    (r"calories|kcal", 450.0),
    (r"distance|\bkm\b|miles", 5.0),
    (r"budget|limit|allowance", 400.0),
    (r"amount|price|cost|spent|paid|total|income|salary|[$€£₹]", 24.5),
    (r"rating|stars?|score", 4.0),
    (r"goal|target", 8.0),
];
const WORDS: [(&str, &str); 10] = [
    (r"habit", "Read 10 pages"),
    (r"exercise|workout|lift|movement", "Bench press"),
    (r"\bbook", "The Hobbit"),
    (r"author|writer", "Ursula K. Le Guin"),
    (r"task|to-?do|chore|project", "Write the report"),
    (r"item|product|grocer|ingredient|shopping", "Milk"),
    (r"categor", "Groceries"),
    (r"trip|event|occasion", "Lisbon weekend"),
    (r"place|city|location|destination|where", "Lisbon"),
    (r"person|people|friend|member|participant|payer|who\b|contact|first name|your name", "Alex"),
];

static NUMBERS_RX: LazyLock<Vec<(Regex, f64)>> =
    LazyLock::new(|| NUMBERS.iter().map(|(rx, v)| (Regex::new(&format!("(?i){rx}")).unwrap(), *v)).collect());
static WORDS_RX: LazyLock<Vec<(Regex, &'static str)>> =
    LazyLock::new(|| WORDS.iter().map(|(rx, v)| (Regex::new(&format!("(?i){rx}")).unwrap(), *v)).collect());
static PRIMARY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*(\+|(add|save|create|log|done|check.?in|start|new|record|track|mark)\b)").unwrap()
});
static NOTE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)note|comment|descr|memo|journal|thought|detail").unwrap());
static EXAMPLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*(?:e\.?\s?g\.?|for example|for instance|such as|like|try)\s*[:,]?\s*(.+)$").unwrap()
});
static EXAMPLE_SPLIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r",\s|\s+or\s+|\s[·/|]\s").unwrap());
static SEARCH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"search|filter").unwrap());

/// Python's str.isspace.
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

fn py_strip(s: &str) -> &str {
    s.trim_matches(py_space)
}

fn first_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// The example in a placeholder ("e.g. Stretch for 5 minutes"): the first one, without quotes or dots.
pub fn example(placeholder: &str) -> String {
    // Python's `$` also matches before a final newline
    let caps = EXAMPLE
        .captures(placeholder)
        .or_else(|| placeholder.strip_suffix('\n').and_then(|p| EXAMPLE.captures(p)));
    let Some(m) = caps else { return String::new() };
    let group = m.get(1).map(|g| g.as_str()).unwrap_or("");
    let first = EXAMPLE_SPLIT.split(group).next().unwrap_or("");
    let cleaned = py_strip(first).trim_matches(['"', '\'', '“', '”', '‘', '’']).trim_end_matches(['.', '…']);
    first_chars(py_strip(cleaned), 60)
}

fn get_str<'a>(ctl: &'a Map<String, Value>, key: &str) -> &'a str {
    ctl.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Python's float(): surrounding whitespace and single underscores between digits allowed.
fn py_float(s: &str) -> Option<f64> {
    let t = py_strip(s);
    if t.is_empty() {
        return None;
    }
    let cleaned: String = if t.contains('_') {
        let chars: Vec<char> = t.chars().collect();
        for (i, c) in chars.iter().enumerate() {
            if *c == '_'
                && !(i > 0
                    && chars[i - 1].is_ascii_digit()
                    && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit()))
            {
                return None;
            }
        }
        t.replace('_', "")
    } else {
        t.to_string()
    };
    let lower = cleaned.to_ascii_lowercase();
    let body = lower.trim_start_matches(['+', '-']);
    if matches!(body, "inf" | "infinity" | "nan") {
        return cleaned.parse::<f64>().ok().or(Some(if lower.starts_with('-') { f64::NEG_INFINITY } else { f64::INFINITY }));
    }
    if !body.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    cleaned.parse::<f64>().ok()
}

/// Python's truthiness of a float: 0.0 is false, NaN is true.
fn truthy(v: f64) -> bool {
    v != 0.0 || v.is_nan()
}

/// Python's `f"{value:.2f}".rstrip("0").rstrip(".")`, "-0" read as "0". None where Python's round() raises.
fn number_text(value: f64) -> String {
    let text = if value.is_nan() {
        "nan".to_string()
    } else if value.is_infinite() {
        if value > 0.0 { "inf".into() } else { "-inf".into() }
    } else {
        let t = format!("{value:.2}");
        t.trim_end_matches('0').trim_end_matches('.').to_string()
    };
    if text == "-0" {
        "0".into()
    } else {
        text
    }
}

/// A number for a numeric field: its current value when it has one, else one that fits its label, kept inside
/// min/max and on its step (a type=number field without a step takes whole numbers). None where Python's
/// round() raises (a NaN or infinite value on a step), which the check reads as a field it cannot fill.
pub fn number(ctl: &Map<String, Value>, words: &str, alt: bool) -> Option<String> {
    let current = py_float(get_str(ctl, "value"));
    let mut value = NUMBERS_RX.iter().find(|(rx, _)| rx.is_match(words)).map(|(_, v)| *v).unwrap_or(12.0);
    if let (Some(c), false) = (current, alt) {
        value = c;
    }
    let (lo, hi) = (py_float(get_str(ctl, "min")), py_float(get_str(ctl, "max")));
    let step = match py_float(get_str(ctl, "step")) {
        Some(s) if truthy(s) => Some(s),
        _ if get_str(ctl, "type") == "number" => Some(1.0),
        _ => None,
    };
    if let (true, Some(l)) = (alt, lo) {
        value = l + step.unwrap_or(1.0);
    }
    match (lo, hi) {
        (Some(l), Some(h)) if !(l <= value && value <= h) => value = (l + h) / 2.0,
        (Some(l), _) if value < l => value = l,
        (_, Some(h)) if value > h => value = h,
        _ => {}
    }
    if let Some(step) = step {
        let base = match lo {
            Some(l) if truthy(l) => l,
            _ => 0.0,
        };
        let q = (value - base) / step;
        if !q.is_finite() {
            return None; // Python: round() of nan or inf raises
        }
        value = base + q.round_ties_even() * step;
        if let Some(h) = hi {
            if value > h {
                value -= step;
            }
        }
    }
    Some(number_text(value))
}

/// What a person would type into this field (a control as CONTROLS_JS reads it). `alt`: a second try after the
/// app rejected the first. None where Python raises (see [`number`]): the field is skipped.
pub fn value_for(ctl: &Map<String, Value>, today: NaiveDate, alt: bool) -> Option<String> {
    let t = get_str(ctl, "type");
    let tag = ctl.get("tag").and_then(Value::as_str).unwrap_or("input");
    let named = match ctl.get("named") {
        Some(v) => v.as_str().unwrap_or(""),
        None => get_str(ctl, "label"),
    };
    let placeholder = get_str(ctl, "placeholder");
    let words = format!("{named} {placeholder}").to_lowercase();
    let iso = today.format("%Y-%m-%d").to_string();
    let week = today.iso_week();
    let by_type = match t {
        "date" => Some(iso.clone()),
        "time" => Some("08:30".to_string()),
        "datetime-local" => Some(format!("{iso}T08:30")),
        "month" => Some(iso[..7].to_string()),
        "week" => Some(format!("{}-W{:02}", week.year(), week.week())),
        "email" => Some("owner@example.com".to_string()),
        "url" => Some("https://example.com".to_string()),
        "tel" => Some("5550100".to_string()),
        _ => None,
    };
    if by_type.is_some() {
        return by_type;
    }
    let inputmode = get_str(ctl, "inputmode");
    if t == "number"
        || inputmode == "numeric"
        || inputmode == "decimal"
        || ((t.is_empty() || t == "text") && NUMBERS_RX.iter().any(|(rx, _)| rx.is_match(named)))
    {
        return number(ctl, &words, alt);
    }
    if t == "search" || SEARCH.is_match(&words) {
        return Some(String::new()); // typing a search would hide the list it filters
    }
    let value = get_str(ctl, "value");
    if !py_strip(value).is_empty() && !alt {
        return Some(value.to_string()); // what the app put there (an edit form): kept, retyped
    }
    let ex = example(placeholder);
    if !ex.is_empty() {
        return Some(ex);
    }
    if tag == "textarea" || NOTE.is_match(&words) {
        return Some("Felt good today.".into());
    }
    Some(WORDS_RX.iter().find(|(rx, _)| rx.is_match(&words)).map(|(_, v)| *v).unwrap_or("Morning run").to_string())
}

// ---- the check -------------------------------------------------------------------------------------------

/// Who walks the journeys.
#[derive(Clone, Default)]
pub enum VerifierChoice {
    /// Jev on this Mac's route, if set (jev_verify.from_env)
    #[default]
    FromEnv,
    /// none: the journeys are not run ("no verifier")
    None,
    Given(Arc<Verifier>),
}

/// The scripted pass's use of the app (`_use`): replaceable so a test can make it crash the page.
pub type UseFn = Arc<
    dyn for<'a> Fn(&'a Page, &'a Session, &'a mut CheckReport, NaiveDate, &'static Theme) -> BoxFuture<'a, anyhow::Result<()>>
        + Send
        + Sync,
>;

/// How one journey is walked (`jev_verify.walk`): replaceable so a test can make it break the page.
pub type WalkFn = Arc<
    dyn for<'a> Fn(&'a Page, &'a Journey, &'a Verifier, WalkOptions<'a>) -> BoxFuture<'a, anyhow::Result<JourneyResult>>
        + Send
        + Sync,
>;

/// `check_app`'s keyword arguments, and the module constants a Python test patches.
#[derive(Clone)]
pub struct CheckOptions {
    /// the data the app starts with (an existing app's real data when it is being changed); copied, never
    /// written back
    pub seed: Option<Value>,
    /// a running Chromium to reuse; None launches one for this check
    pub browser: Option<Arc<Browser>>,
    /// the Chromium to launch when `browser` is None (None: Playwright's, found by `cdp::find_chromium`); a
    /// path that is not there is no Chromium
    pub executable: Option<std::path::PathBuf>,
    /// the maker's journeys, walked after the scripted pass
    pub journeys: Option<Vec<Journey>>,
    pub verifier: VerifierChoice,
    /// CHECK_TIMEOUT_S
    pub check_timeout_s: f64,
    /// jev_verify.EVAL_TIMEOUT_S for the journeys
    pub eval_timeout_s: f64,
    /// replaces `_use` (tests)
    pub use_fn: Option<UseFn>,
    /// replaces `jev_verify.walk` (tests)
    pub walk_fn: Option<WalkFn>,
}

impl Default for CheckOptions {
    fn default() -> Self {
        Self {
            seed: None,
            browser: None,
            executable: None,
            journeys: None,
            verifier: VerifierChoice::FromEnv,
            check_timeout_s: CHECK_TIMEOUT_S,
            eval_timeout_s: eve_jev_verify::EVAL_TIMEOUT_S,
            use_fn: None,
            walk_fn: None,
        }
    }
}

/// `type(e).__name__` for a failure: a page error's kind, else "Error".
fn kind_of(e: &anyhow::Error) -> String {
    if let Some(p) = e.downcast_ref::<PageError>() {
        return p.kind.clone();
    }
    if e.downcast_ref::<JevDown>().is_some() {
        return "JevDown".into();
    }
    if e.downcast_ref::<cdp::NoChromium>().is_some() {
        return "NoChromium".into();
    }
    "Error".into()
}

enum Stop {
    /// the scripted pass did not finish within the check's time
    Timeout,
    Failed(anyhow::Error),
}

/// Run `html` like a person's first minute with it. Never fails; no Chromium is a skip.
///
/// `seed` is the data the app starts with (an existing app's real data when it is being changed, so a data
/// migration is tested too); it is copied, never written back. `browser` reuses a running Chromium.
///
/// `journeys` (the maker's, eve-jev-verify) run after the scripted pass, each on a fresh app with no data, and
/// `verifier` walks them (default: Jev on this Mac's route, `from_env`; None: journeys not run). A journey that
/// does not go through is a problem in this report like any other; a verifier that is down is "journeys not run
/// (why)" and never a problem.
pub async fn check_app(html: &str, opts: CheckOptions) -> CheckReport {
    let t0 = Instant::now();
    let browser: Arc<Browser> = match &opts.browser {
        Some(b) => b.clone(),
        None => match opts.executable.clone().or_else(cdp::find_chromium).filter(|p| p.is_file()) {
            None => return CheckReport { skipped: "Chromium is not installed".into(), ..Default::default() },
            Some(exe) => match Browser::launch_at(&exe, &CHROMIUM_ARGS).await {
                Ok(b) => Arc::new(b),
                Err(e) => {
                    let kind = kind_of(&e);
                    tracing::warn!("app-check: the check could not run ({kind}: {})", first_chars(&e.to_string(), 200));
                    return CheckReport {
                        skipped: format!("the check could not run ({kind})"),
                        secs: t0.elapsed().as_secs_f64(),
                        ..Default::default()
                    };
                }
            },
        },
    };
    let html: Arc<str> = Arc::from(html);
    let mut report = CheckReport::default();
    let sess = Arc::new(Session::new(opts.seed.as_ref()));
    let today = chrono::Local::now().date_naive();
    let outcome = both(&browser, &html, &sess, &mut report, today, &opts).await;
    if opts.browser.is_none() {
        browser.close().await;
    }
    match outcome {
        Ok(()) => {}
        Err(Stop::Timeout) => {
            sess.note(format!(
                "The app stopped responding while {}: nothing finished within {:.0} s (an endless loop, or work \
                 that never yields).",
                sess.action(),
                opts.check_timeout_s
            ));
        }
        Err(Stop::Failed(e)) => {
            let text = e.to_string();
            let (opened, crashed) = {
                let st = sess.st.lock();
                (st.opened, st.crashed)
            };
            let has_issues = !sess.issues.lock().is_empty();
            if opened && (crashed || text.to_lowercase().contains("crash")) {
                // a page the app crashed is the app's problem
                sess.note(format!(
                    "The app crashed the page while {} (the browser tab died: most likely memory that grows \
                     without end, or a loop that allocates on every turn).",
                    sess.action()
                ));
            } else if !(opened && has_issues) {
                // a checker that breaks is a skip, never a failed build
                let kind = kind_of(&e);
                tracing::warn!("app-check: the check could not run ({kind}: {})", first_chars(&text, 200));
                return CheckReport {
                    skipped: format!("the check could not run ({kind})"),
                    secs: t0.elapsed().as_secs_f64(),
                    ..Default::default()
                };
            } else {
                // it broke part way: what it found so far still stands
                tracing::warn!("app-check: the check stopped early ({}: {})", kind_of(&e), first_chars(&text, 200));
            }
        }
    }
    let (saves, loads, primary_taps) = {
        let st = sess.st.lock();
        (st.saves, st.loads, st.primary_taps)
    };
    let crashed_finding = sess.crashed() || sess.issues.lock().iter().any(|i| i.starts_with("The app crashed the page"));
    if crashed_finding {
        // a dead tab saves nothing: the crash is the finding
    } else if saves == 0 && loads > 0 && report.taps > 0 && (report.fields > 0 || primary_taps > 0) {
        // Python's `seed is not None` (JSON null is None)
        sess.note(if opts.seed.as_ref().is_some_and(|s| !s.is_null()) {
            "Opened with the owner's saved data, the app never called eve.save after being used: most likely it \
             could not read that data (its migrate() does not convert this shape), so the owner cannot use their \
             data in this version."
        } else {
            "After its fields were filled and its buttons tapped (Add, Done and the like), the app never called \
             eve.save, so nothing entered would survive closing it."
        });
    } else if loads == 0 && (report.fields > 0 || report.taps > 0) {
        sess.note("The app never called eve.load, so it cannot show anything it saved before.");
    }
    report.saves = saves;
    report.saved = sess.data();
    if report.photo_data.is_null() {
        report.photo_data = report.saved.clone();
    }
    report.photo_from =
        if report.screenshot.as_ref().is_some_and(|s| !s.is_empty()) { "the scripted check".into() } else { String::new() };
    let shot = report
        .journeys
        .as_ref()
        .and_then(|j| j.results.iter().find(|r| r.passed() && r.screenshot.as_ref().is_some_and(|s| !s.is_empty())))
        .map(|r| (r.screenshot.clone(), r.data.clone(), r.name.clone()));
    if let Some((screenshot, data, name)) = shot {
        // The owner's picture is the app doing what it is for: the end of a journey that passed (the splitter
        // showing who owes whom), not the scripted pass's reopened screen, which can be a setup card, or look
        // right on an app whose journeys failed.
        report.screenshot = screenshot;
        report.photo_data = data;
        report.photo_from = format!("the journey \"{name}\"");
    }
    let issues = sess.issues();
    report.issues = issues.iter().take(MAX_ISSUES).cloned().collect();
    if let Some(run) = &report.journeys {
        // the core purpose first: a journey that fails says more than a look finding, so it is never cut
        let walked = run.issues();
        let cap = MAX_ISSUES.max(walked.len());
        let mut merged = walked.clone();
        merged.extend(report.issues.iter().filter(|i| !walked.contains(i)).cloned());
        merged.truncate(cap);
        report.issues = merged;
    }
    report.ok = report.issues.is_empty();
    report.trace = sess.trace();
    report.secs = t0.elapsed().as_secs_f64();
    report
}

async fn both(
    b: &Browser,
    html: &Arc<str>,
    sess: &Arc<Session>,
    report: &mut CheckReport,
    today: NaiveDate,
    opts: &CheckOptions,
) -> Result<(), Stop> {
    let limit = Duration::from_secs_f64(opts.check_timeout_s.max(0.0));
    match tokio::time::timeout(limit, go(b, html, sess, report, today, opts)).await {
        Err(_) => {
            // the app hangs: every journey would wait on it too, for JOURNEY_TIMEOUT_S, and find nothing more
            if let Some(js) = opts.journeys.as_ref().filter(|j| !j.is_empty()) {
                report.journeys = Some(JourneyRun {
                    total: js.len(),
                    not_run: "the app stopped responding in the scripted check".into(),
                    ..Default::default()
                });
            }
            Err(Stop::Timeout)
        }
        Ok(Err(e)) => {
            walk_all(b, html, report, opts).await; // a scripted pass that broke still leaves the journeys to walk
            Err(Stop::Failed(e))
        }
        Ok(Ok(())) => {
            walk_all(b, html, report, opts).await;
            Ok(())
        }
    }
}

async fn go(
    b: &Browser,
    html: &Arc<str>,
    sess: &Arc<Session>,
    report: &mut CheckReport,
    today: NaiveDate,
    opts: &CheckOptions,
) -> anyhow::Result<()> {
    let run_opts = RunOptions { dark: true, interact: true, label: "", shoot: false };
    run(b, html, sess, report, today, PHONE, run_opts, opts).await?;
    let after = sess.st.lock().stored.clone();
    {
        let mut st = sess.st.lock();
        st.stored = st.richest.clone().or(after.clone());
    }
    report.photo_data = sess.data();
    let shoot = RunOptions { dark: false, interact: false, label: "reopening the app with the data it saved", shoot: true };
    report.screenshot = run(b, html, sess, report, today, PHONE, shoot, opts).await?;
    sess.st.lock().stored = after;
    let small = RunOptions { dark: false, interact: false, label: "reopening the app on a 360px-wide phone", shoot: false };
    run(b, html, sess, report, today, SMALL_PHONE, small, opts).await?;
    Ok(())
}

async fn walk_all(b: &Browser, html: &Arc<str>, report: &mut CheckReport, opts: &CheckOptions) {
    let Some(js) = opts.journeys.as_ref().filter(|j| !j.is_empty()) else { return };
    let (verifier, why) = match &opts.verifier {
        VerifierChoice::FromEnv => {
            let (v, why) = eve_jev_verify::from_process_env();
            (v.map(Arc::new), why)
        }
        VerifierChoice::None => (None, "no verifier".to_string()),
        VerifierChoice::Given(v) => (Some(v.clone()), String::new()),
    };
    report.journeys = Some(journeys(b, html, js, verifier.as_deref(), &why, opts).await);
}

#[derive(Clone, Copy)]
struct RunOptions<'a> {
    dark: bool,
    interact: bool,
    label: &'a str,
    shoot: bool,
}

/// One open of the app. Shooting: returns the screenshot. Issues go to `sess`.
#[allow(clippy::too_many_arguments)]
async fn run(
    b: &Browser,
    html: &Arc<str>,
    sess: &Arc<Session>,
    report: &mut CheckReport,
    today: NaiveDate,
    viewport: Viewport,
    ro: RunOptions<'_>,
    opts: &CheckOptions,
) -> anyhow::Result<Option<Vec<u8>>> {
    let theme = if ro.dark { DARK } else { LIGHT };
    let (ctx, page, loaded) = open(b, html, sess, viewport, ro.dark, ro.interact, ro.label).await?;
    let result = async {
        if !loaded {
            return Ok(None);
        }
        if ro.interact {
            match &opts.use_fn {
                Some(f) => f(&page, sess, report, today, theme).await?,
                None => use_app(&page, sess, report, today, theme).await?,
            }
        }
        sess.step(if ro.label.is_empty() { "using the app" } else { ro.label });
        look(&page, sess, theme, ro.dark).await;
        layout(&page, sess, viewport).await?;
        if ro.interact {
            close(&page, sess).await?;
        }
        if ro.shoot {
            return Ok(Some(photo(&page, &ctx, viewport).await?));
        }
        Ok(None)
    }
    .await;
    ctx.close().await;
    result
}

/// `__checkCfg` as Python's json.dumps writes it.
fn check_cfg(theme: &Theme, dark: bool) -> String {
    let pairs: Vec<String> =
        theme.iter().map(|(k, v)| format!("{}: {}", Value::from(*k), Value::from(*v))).collect();
    format!("window.__checkCfg = {{\"theme\": {{{}}}, \"scheme\": \"{}\"}};", pairs.join(", "), if dark { "dark" } else { "light" })
}

/// The functions the stand-ins call back into the checker (`expose_function`), and WebSockets answered here
/// (Playwright's `route_web_socket`): each socket is reported, then closed, and nothing leaves the page.
const BRIDGE_JS: &str = r#"(() => {
  const send = window.__checkBinding;
  if (typeof send !== "function" || window.__checkDeliver) return;
  const waiting = new Map(); let seq = 0;
  const call = (name, args) => new Promise((resolve) => { const id = ++seq; waiting.set(id, resolve);
    send(JSON.stringify({ name, id, args })); });
  window.__eveLoad = (...args) => call("load", args);
  window.__eveSave = (...args) => call("save", args);
  Object.defineProperty(window, "__checkDeliver", { value: (id, value) => {
    const r = waiting.get(id); if (r) { waiting.delete(id); r(value); } } });
  const sockets = new Map(); let sockets_seq = 0;
  const finish = (ws, code, reason) => {
    if (ws.readyState === 3) return; ws.readyState = 3;
    const e = new CloseEvent("close", { code, reason, wasClean: true });
    if (typeof ws.onclose === "function") ws.onclose(e);
    ws.dispatchEvent(e);
  };
  class WebSocket extends EventTarget {
    constructor(url, protocols) {
      super();
      const u = new URL(String(url), location.href);
      if (u.protocol === "http:") u.protocol = "ws:"; else if (u.protocol === "https:") u.protocol = "wss:";
      if (u.protocol !== "ws:" && u.protocol !== "wss:")
        throw new DOMException(`Failed to construct 'WebSocket': The URL's scheme must be either 'http', 'https', 'ws', or 'wss'. '${u.protocol}' is not allowed.`, "SyntaxError");
      this.url = u.href; this.protocol = ""; this.extensions = ""; this.binaryType = "blob"; this.bufferedAmount = 0;
      this.readyState = 0; this.onopen = null; this.onmessage = null; this.onerror = null; this.onclose = null;
      const id = ++sockets_seq; sockets.set(id, this);
      send(JSON.stringify({ name: "socket", id, args: [this.url] }));
    }
    send() { if (this.readyState === 0) throw new DOMException("Failed to execute 'send' on 'WebSocket': Still in CONNECTING state.", "InvalidStateError"); }
    close(code, reason) { if (this.readyState >= 2) return; this.readyState = 2; finish(this, code || 1000, reason || ""); }
  }
  for (const [k, v] of [["CONNECTING", 0], ["OPEN", 1], ["CLOSING", 2], ["CLOSED", 3]]) {
    Object.defineProperty(WebSocket, k, { value: v }); Object.defineProperty(WebSocket.prototype, k, { value: v }); }
  window.WebSocket = WebSocket;
  Object.defineProperty(window, "__checkSocketClosed", { value: (id) => {
    const ws = sockets.get(id); if (ws) { sockets.delete(id); finish(ws, 1000, ""); } } });
})();"#;

const BINDING: &str = "__checkBinding";

/// Python's `str(value)` for what a page script returned.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        other => other.to_string(),
    }
}

fn host_of(url: &str) -> &str {
    if url.starts_with("http://") || url.starts_with("https://") {
        url.split('/').nth(2).unwrap_or("")
    } else {
        ""
    }
}

/// Every finding of one page wired to `sess`: errors, console errors, reloads, a crash, WebSockets, failed
/// libraries, and the router that serves the app and fences everything else.
async fn wire(
    page: Page,
    mut events: tokio::sync::mpsc::UnboundedReceiver<cdp::Event>,
    sess: Arc<Session>,
    html: Arc<str>,
    armed: Arc<AtomicBool>,
) {
    let mut loads = 0usize;
    while let Some(ev) = events.recv().await {
        let p = &ev.params;
        match ev.method.as_str() {
            "Runtime.exceptionThrown" => {
                let details = p.get("exceptionDetails").cloned().unwrap_or(Value::Null);
                let (message, stack) = cdp::page_error(&details);
                // Python's handler takes `.splitlines()[0]`, which raises on an empty message: no finding then
                if let Some(line) = py_first_line(&message) {
                    let msg = first_chars(line, 300);
                    sess.note(format!("Uncaught error while {}: {msg}{}", sess.action(), where_(&stack)));
                }
            }
            "Runtime.consoleAPICalled" if p.get("type").and_then(Value::as_str) == Some("error") => {
                let text = console_text(p.get("args"));
                if !text.starts_with("Failed to load resource") {
                    sess.note(format!("Console error while {}: {}", sess.action(), first_chars(&text, 300)));
                }
            }
            "Log.entryAdded" => {
                let entry = p.get("entry").cloned().unwrap_or(Value::Null);
                let level = entry.get("level").and_then(Value::as_str).unwrap_or("");
                let source = entry.get("source").and_then(Value::as_str).unwrap_or("");
                let text = entry.get("text").and_then(Value::as_str).unwrap_or("");
                if level == "error" && source != "worker" && !text.starts_with("Failed to load resource") {
                    sess.note(format!("Console error while {}: {}", sess.action(), first_chars(text, 300)));
                }
            }
            "Page.loadEventFired" if armed.load(Ordering::SeqCst) => {
                loads += 1;
                if loads > 1 {
                    sess.note(format!(
                        "The whole page reloaded while {} (a <form> submitted without event.preventDefault(), or a \
                         link to another page), which throws away what is on screen.",
                        sess.action()
                    ));
                }
            }
            "Inspector.targetCrashed" => sess.st.lock().crashed = true,
            "Network.responseReceived" => {
                let status = p.pointer("/response/status").and_then(Value::as_f64).unwrap_or(0.0);
                let url = p.pointer("/response/url").and_then(Value::as_str).unwrap_or("");
                if status >= 400.0 && url.split('/').nth(2) == Some(ALLOWED_HOSTS[0]) {
                    sess.note(format!("A library did not load (HTTP {}): {}", status as i64, first_chars(url, 160)));
                }
            }
            // what a request or a call means for the session is settled here, in the order the page made them
            // (two saves in a row land in that order); only the reply to the page runs on its own
            "Fetch.requestPaused" => {
                let id = p.get("requestId").and_then(Value::as_str).unwrap_or("").to_string();
                let url = p.pointer("/request/url").and_then(Value::as_str).unwrap_or("");
                let reply = route(&sess, &html, url);
                let page = page.clone();
                tokio::spawn(async move {
                    let _ = reply.send(&page, &id).await;
                });
            }
            "Runtime.bindingCalled" if p.get("name").and_then(Value::as_str) == Some(BINDING) => {
                let context = p.get("executionContextId").and_then(Value::as_i64).unwrap_or(0);
                let payload: Value =
                    serde_json::from_str(p.get("payload").and_then(Value::as_str).unwrap_or("")).unwrap_or(Value::Null);
                if let Some(expression) = answer(&sess, &payload) {
                    let page = page.clone();
                    tokio::spawn(async move {
                        let _ = page.evaluate_in(context, &expression).await;
                    });
                }
            }
            _ => {}
        }
    }
}

/// A console message's text, as Playwright joins its arguments.
fn console_text(args: Option<&Value>) -> String {
    let Some(Value::Array(args)) = args else { return String::new() };
    args.iter()
        .map(|a| match a.get("value") {
            Some(Value::String(s)) => s.clone(),
            Some(v) if !v.is_null() => v.to_string(),
            _ => match a.get("type").and_then(Value::as_str) {
                Some("undefined") => "undefined".into(),
                _ => a
                    .get("unserializableValue")
                    .or_else(|| a.get("description"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| if a.get("subtype").and_then(Value::as_str) == Some("null") { "null".into() } else { String::new() }),
            },
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The page called back: `eve.load`, `eve.save`, or a WebSocket being opened. The answer is the script that
/// hands the result back to the page.
fn answer(sess: &Session, payload: &Value) -> Option<String> {
    let id = payload.get("id").and_then(Value::as_i64).unwrap_or(0);
    let first = payload.pointer("/args/0").cloned().unwrap_or(Value::Null);
    match payload.get("name").and_then(Value::as_str).unwrap_or("") {
        "load" => {
            let value = sess.load().map(Value::String).unwrap_or(Value::Null);
            Some(format!("window.__checkDeliver({id}, {value})"))
        }
        "save" => {
            let raw = match first {
                Value::String(s) => s,
                other => other.to_string(),
            };
            let ok = sess.save(raw);
            Some(format!("window.__checkDeliver({id}, {ok})"))
        }
        "socket" => {
            let url = py_str(&first);
            sess.note(format!(
                "The app tried to open a WebSocket to {} while {}; it may not reach the network, and must keep \
                 its data with eve.save.",
                first_chars(&url, 80),
                sess.action()
            ));
            Some(format!("window.__checkSocketClosed({id})"))
        }
        _ => None,
    }
}

/// What the router does with one request.
enum Reply {
    Fulfill(u16, Vec<(&'static str, &'static str)>, Arc<str>),
    Continue,
    Abort,
}

impl Reply {
    async fn send(self, page: &Page, id: &str) -> anyhow::Result<()> {
        match self {
            Reply::Fulfill(status, headers, body) => page.fulfill(id, status, &headers, &body).await,
            Reply::Continue => page.continue_request(id).await,
            Reply::Abort => page.abort(id).await,
        }
    }
}

/// Every request: the app itself, served sandboxed; Telegram's script and eve.js answered empty; the app host's
/// other files not found; the library CDN reached for real; anything else refused and reported.
fn route(sess: &Session, html: &Arc<str>, url: &str) -> Reply {
    let host = host_of(url);
    let bare = url.split('#').next().unwrap_or("").split('?').next().unwrap_or("");
    if bare == APP_URL {
        Reply::Fulfill(
            200,
            vec![("Content-Type", "text/html; charset=utf-8"), ("Content-Security-Policy", SANDBOX)],
            html.clone(),
        )
    } else if STUBBED_HOSTS.iter().any(|h| host.ends_with(h)) || (host == APP_HOST && url.ends_with("/eve.js")) {
        Reply::Fulfill(200, vec![("Content-Type", "application/javascript")], Arc::from(""))
    } else if host == APP_HOST {
        Reply::Fulfill(404, Vec::new(), Arc::from("")) // favicon and the like: not the app's fault
    } else if ALLOWED_HOSTS.contains(&host) {
        Reply::Continue
    } else {
        let shown = if host.is_empty() { first_chars(url, 60) } else { host.to_string() };
        sess.note(format!(
            "The app tried to reach {shown} while {}; it may only load libraries from cdn.jsdelivr.net and must \
             keep its data with eve.save.",
            sess.action()
        ));
        Reply::Abort
    }
}

/// A new phone-sized context with the app open in it, served and fenced exactly as the server does, the
/// stand-ins installed and every finding wired to `sess`: (context, page, whether it loaded). The context
/// closes when dropped. `clock`: the page's clock is the checker's, for the hour jump at the end.
async fn open(
    b: &Browser,
    html: &Arc<str>,
    sess: &Arc<Session>,
    viewport: Viewport,
    dark: bool,
    clock: bool,
    label: &str,
) -> anyhow::Result<(Context, Page, bool)> {
    let theme = if dark { DARK } else { LIGHT };
    let ctx = b
        .new_context(ContextOptions {
            width: viewport.width,
            height: viewport.height,
            device_scale_factor: 2.0,
            is_mobile: true,
            has_touch: true,
            locale: "en-US".into(),
            color_scheme: if dark { "dark" } else { "light" }.into(),
        })
        .await?;
    let page = ctx.new_page().await?;
    sess.step(if label.is_empty() { "opening the app" } else { label });
    let armed = Arc::new(AtomicBool::new(false));
    let events = page.take_events().ok_or_else(|| PageError::new("Error", "the page's events are taken"))?;
    tokio::spawn(wire(page.clone(), events, sess.clone(), html.clone(), armed.clone()));
    page.add_binding(BINDING).await?;
    page.add_init_script(BRIDGE_JS).await?;
    page.route_all().await?;
    page.add_init_script(&check_cfg(theme, dark)).await?;
    page.add_init_script(STUBS_JS).await?;
    if clock {
        // runs in real time until the hour jump at the end; no clock control: the hour is skipped, the rest runs
        let _ = page.install_clock().await;
    }
    sess.st.lock().opened = true;
    armed.store(true, Ordering::SeqCst);
    if let Err(e) = page.goto(APP_URL, LOAD_TIMEOUT_MS).await {
        // a page that never loads is the app's problem, and a finding
        if !kind_of(&e).contains("Timeout") {
            return Err(e);
        }
        sess.note(format!(
            "The app did not finish loading within {} s ({}).",
            LOAD_TIMEOUT_MS / 1000,
            if label.is_empty() { "first open" } else { label }
        ));
        return Ok((ctx, page, false));
    }
    page.wait_for_timeout(SETTLE_MS).await;
    Ok((ctx, page, true))
}

/// Walk every journey side by side, each in its own fresh context with no saved data. Never fails.
///
/// A page that breaks under one journey (a crashed tab, a reload that destroyed its context, a page that stops
/// answering) is that journey's failure: the app's, like the scripted pass says of a crash. Jev down, the time
/// limit or a checker fault stops the journeys still running; those are "not run (why)", because a verifier
/// that cannot answer must never block a build, and the journeys that already finished keep their result: a
/// failure seen before Jev went down is still a problem.
pub async fn journeys(
    b: &Browser,
    html: &Arc<str>,
    js: &[Journey],
    verifier: Option<&Verifier>,
    why: &str,
    opts: &CheckOptions,
) -> JourneyRun {
    let mut run = JourneyRun { total: js.len(), ..Default::default() };
    let Some(verifier) = verifier else {
        run.not_run = if why.is_empty() { "no verifier".into() } else { why.to_string() };
        tracing::info!("app-check: journeys not run ({})", run.not_run);
        return run;
    };
    let t0 = Instant::now();
    let mut finished: BTreeMap<usize, JourneyResult> = BTreeMap::new();
    {
        let mut pending: FuturesUnordered<_> = js
            .iter()
            .enumerate()
            .map(|(n, j)| async move { (n, one_journey(b, html, j, verifier, opts).await) })
            .collect();
        let deadline = tokio::time::Instant::now() + Duration::from_secs_f64(eve_jev_verify::JOURNEY_TIMEOUT_S);
        let mut take = |n: usize, r: anyhow::Result<JourneyResult>, run: &mut JourneyRun| match r {
            Ok(res) => {
                finished.insert(n, res);
            }
            Err(e) => {
                if let Some(down) = e.downcast_ref::<JevDown>() {
                    if run.not_run.is_empty() {
                        run.not_run = format!("Jev did not answer: {down}");
                    }
                } else if run.not_run.is_empty() {
                    // a checker fault is never the app's
                    run.not_run = format!("the journeys could not run ({})", kind_of(&e));
                    tracing::warn!(
                        "app-check: journeys could not run ({}: {})",
                        kind_of(&e),
                        first_chars(&e.to_string(), 200)
                    );
                }
            }
        };
        while !pending.is_empty() && run.not_run.is_empty() {
            match tokio::time::timeout_at(deadline, pending.next()).await {
                Err(_) => {
                    run.not_run =
                        format!("the journeys took longer than {:.0} s", eve_jev_verify::JOURNEY_TIMEOUT_S);
                    break;
                }
                Ok(None) => break,
                Ok(Some((n, r))) => {
                    take(n, r, &mut run);
                    // the others that finished at the same moment count too
                    while let Some(Some((n, r))) = pending.next().now_or_never() {
                        take(n, r, &mut run);
                    }
                }
            }
        }
        // dropping the journeys still going stops them asking Jev and closes their pages: they must not go on
        // after this check returned, into a browser its caller may close
    }
    run.results = finished.into_values().collect();
    run.secs = t0.elapsed().as_secs_f64();
    run.jev = verifier.calls.as_dict();
    let num = |k: &str| run.jev.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    tracing::info!(
        "app-check: {} in {:.1} s (Jev {} calls, {} ms, p50 {} ms, {} input tokens, ${:.5}, {} errors)",
        run.summary(),
        run.secs,
        num("calls") as i64,
        num("ms") as i64,
        num("p50_ms") as i64,
        num("input_tokens") as i64,
        num("usd"),
        num("errors") as i64
    );
    run
}

async fn one_journey(
    b: &Browser,
    html: &Arc<str>,
    j: &Journey,
    verifier: &Verifier,
    opts: &CheckOptions,
) -> anyhow::Result<JourneyResult> {
    let sess = Arc::new(Session::new(None));
    let at = AtomicUsize::new(0);
    let label = format!("opening the app for the journey \"{}\"", j.name);
    let (ctx, page, loaded) = open(b, html, &sess, PHONE, false, false, &label).await?;
    let result = async {
        if !loaded {
            let mut r = JourneyResult::new(&j.name, "step");
            r.steps = j.steps.len();
            r.step = 1;
            r.reason = "the app did not finish loading.".into();
            r.evidence = "(nothing)".into();
            return Ok(r);
        }
        let on_step = |text: &str| {
            at.fetch_add(1, Ordering::SeqCst);
            sess.step(text);
        };
        let walk_opts = WalkOptions { errors: Some(&sess.issues), on_step: Some(&on_step), eval_timeout_s: opts.eval_timeout_s };
        let walked = match &opts.walk_fn {
            Some(f) => f(&page, j, verifier, walk_opts).await,
            None => eve_jev_verify::walk(&page, j, verifier, walk_opts).await,
        };
        let mut res = match walked {
            Ok(res) => res,
            Err(e) if e.downcast_ref::<PageError>().is_some() => {
                let n = at.load(Ordering::SeqCst).max(1);
                let text = e.to_string();
                let first = first_chars(py_first_line(&text).unwrap_or(""), 160);
                let mut r = JourneyResult::new(&j.name, "step");
                r.steps = j.steps.len();
                r.step = n;
                r.evidence = "(the page broke)".into();
                r.step_words = j.steps.get(n - 1).map(|s| s.words()).unwrap_or_default();
                r.reason = format!("the page stopped working during step {n} ({}: {first}).", kind_of(&e));
                return Ok(r);
            }
            Err(e) => return Err(e),
        };
        if res.passed() {
            match photo(&page, &ctx, PHONE).await {
                Ok(shot) => {
                    res.screenshot = Some(shot);
                    res.data = sess.data();
                }
                Err(e) => {
                    // no picture from this journey; the result stands
                    tracing::info!("app-check: no picture from the journey {:?} ({})", j.name, kind_of(&e));
                }
            }
        }
        Ok(res)
    }
    .await;
    ctx.close().await;
    result
}

async fn layout(page: &Page, sess: &Session, viewport: Viewport) -> anyhow::Result<()> {
    let lay = page.evaluate(LAYOUT_JS, None).await?;
    let int = |k: &str| lay.get(k).and_then(Value::as_f64).unwrap_or(0.0) as i64;
    // A phone browser widens the layout to fit what overflows, so innerWidth grows too: the overflow is how far
    // the page (layout plus scroll) reaches past the screen.
    let over = int("inner") + int("wide").max(0) - viewport.width as i64;
    let meta = lay.get("meta").and_then(Value::as_bool).unwrap_or(false);
    if !meta {
        sess.note(format!(
            "The page is laid out for a desktop ({}px wide on a {}px phone): it needs <meta name=\"viewport\" \
             content=\"width=device-width, initial-scale=1\">.",
            int("inner"),
            viewport.width
        ));
    } else if over > 2 {
        sess.note(format!(
            "The page is {over}px wider than a {}px phone while {}, so it shrinks or scrolls sideways.",
            viewport.width,
            sess.action()
        ));
    }
    if int("text") == 0 {
        sess.note(format!("The screen is blank while {}: nothing is rendered.", sess.action()));
    }
    Ok(())
}

/// The look checks (LOOK_JS) on the view on screen now; each kind of finding is reported once.
async fn look(page: &Page, sess: &Session, theme: &Theme, dark: bool) {
    let arg = json!({"dark": dark, "pageBg": rgb(theme_get(theme, "bg_color")), "pencil": PENCIL.as_dict()});
    // a page mid-navigation: the next view is checked instead
    let Ok(Value::Object(found)) = page.evaluate(LOOK_JS, Some(arg)).await else { return };
    for (kind, items) in &found {
        let items: Vec<String> = items.as_array().map(|a| a.iter().map(py_str).collect()).unwrap_or_default();
        if items.is_empty() {
            continue;
        }
        let Some((_, template)) = LOOKS.iter().find(|(k, _)| k == kind) else { continue };
        let fresh = sess.st.lock().looked.insert(kind.clone());
        if fresh {
            sess.note(format_look(template, &sess.action(), &items.join("; ")));
        }
    }
}

/// "#212121" → "rgb(33, 33, 33)".
pub fn rgb(hex_color: &str) -> String {
    let h = hex_color.trim_start_matches('#');
    let part = |i: usize| h.get(i..i + 2).and_then(|p| u8::from_str_radix(p, 16).ok()).unwrap_or(0);
    format!("rgb({}, {}, {})", part(0), part(2), part(4))
}

/// Telegram closes the app without warning: a save that only comes after that never reaches the Mac. Then the
/// other way an app is left: the phone locks with it open for an hour (a running timer finishes) and wakes up
/// again.
async fn close(page: &Page, sess: &Session) -> anyhow::Result<()> {
    sess.step("closing the app");
    page.evaluate(CLOSE_JS, Some(json!("hidden"))).await?;
    page.wait_for_timeout(CLOSE_GRACE_MS).await;
    let at_close = {
        let mut st = sess.st.lock();
        st.closed = true;
        st.stored.clone()
    };
    page.wait_for_timeout(LATE_MS).await;
    let late = {
        let mut st = sess.st.lock();
        st.closed = false;
        st.late.take()
    };
    if late.is_some() && late != at_close {
        sess.note(
            "The app saved a change only after it was closed (a delayed save, such as a debounce). Telegram closes \
             the app without warning, so that change is lost: save straight after each change, or also save on \
             visibilitychange (hidden) and pagehide.",
        );
    }
    sess.step("leaving the app open on a locked phone for an hour");
    page.evaluate(CLOSE_JS, Some(json!("visible"))).await?;
    if let Err(e) = page.clock_fast_forward(AN_HOUR_MS).await {
        // a timer the app set threw: it comes out of the jump, not as a page error
        let text = e.to_string();
        let first = text.split('\n').next().unwrap_or("");
        if first.contains("Error") {
            let msg = first.split_once(": ").map(|(_, m)| m).unwrap_or(first);
            sess.note(format!("Uncaught error while {}: {}{}", sess.action(), first_chars(msg, 300), where_(&text)));
        }
    }
    page.wait_for_timeout(SETTLE_MS).await;
    Ok(())
}

/// The app as the owner sees it: when the MainButton shows, the page gets the screen above Telegram's bottom
/// bar (as on the phone) and the bar is drawn under it, never over the page's content.
pub async fn photo(page: &Page, ctx: &Context, viewport: Viewport) -> anyhow::Result<Vec<u8>> {
    let bar = page.evaluate(BAR_JS, None).await?;
    let has_bar = value_truthy(&bar);
    if has_bar {
        page.evaluate("() => document.documentElement.style.setProperty('--tg-safe-area-inset-bottom', '0px')", None)
            .await?;
        page.set_viewport_size(viewport.width, viewport.height - BAR_H).await?;
        page.wait_for_timeout(TAP_WAIT_MS).await;
    }
    page.evaluate("() => window.scrollTo(0, 0)", None).await?;
    if !has_bar {
        return page.screenshot(Some(80)).await;
    }
    let shot = base64::engine::general_purpose::STANDARD.encode(page.screenshot(None).await?);
    let frame = ctx.new_page().await?;
    let field = |k: &str| py_str(bar.get(k).unwrap_or(&Value::Null));
    frame
        .set_content(&format!(
            "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
             <body style=\"margin:0;background:{}\"><img alt=\"\" src=\"data:image/png;base64,{shot}\" \
             style=\"display:block;width:{}px;height:{}px\">\
             <div style=\"padding:8px 16px 42px\"><div style=\"height:50px;border-radius:12px;display:flex;\
             align-items:center;justify-content:center;font:600 17px -apple-system,system-ui,sans-serif;\
             background:{};color:{}\" id=\"b\"></div></div></body>",
            field("bg"),
            viewport.width,
            viewport.height - BAR_H,
            field("color"),
            field("textColor"),
        ))
        .await?;
    frame
        .evaluate("(t) => { document.getElementById('b').textContent = t; }", Some(bar.get("text").cloned().unwrap_or(Value::Null)))
        .await?;
    frame.screenshot(Some(80)).await
}

fn ctl_str<'a>(c: &'a Map<String, Value>, k: &str) -> &'a str {
    get_str(c, k)
}

/// Python's truthiness of a JSON value.
fn value_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn ctl_bool(c: &Map<String, Value>, k: &str) -> bool {
    c.get(k).is_some_and(value_truthy)
}

fn ctl_int(c: &Map<String, Value>, k: &str) -> i64 {
    c.get(k).and_then(Value::as_f64).unwrap_or(0.0) as i64
}

async fn controls(page: &Page, kind: &str) -> anyhow::Result<Vec<Map<String, Value>>> {
    let found = page.evaluate(CONTROLS_JS, Some(json!(kind))).await?;
    Ok(found
        .as_array()
        .map(|a| a.iter().filter_map(|c| c.as_object().cloned()).collect())
        .unwrap_or_default())
}

/// A person's first minute, view by view. What it saved is kept by `sess` (the richest data too).
pub async fn use_app(
    page: &Page,
    sess: &Session,
    report: &mut CheckReport,
    today: NaiveDate,
    theme: &Theme,
) -> anyhow::Result<()> {
    let mut tapped: HashSet<(String, String)> = HashSet::new();
    let mut filled: HashMap<(String, String, String), usize> = HashMap::new();
    let mut pressed: HashMap<String, usize> = HashMap::new();
    let mut groups: HashMap<String, usize> = HashMap::new();
    let mut covered: indexmap::IndexMap<(String, String), Instant> = indexmap::IndexMap::new(); // first seen covered
    let (mut waits, mut refill) = (0usize, false);
    for _ in 0..MAX_ROUNDS {
        if report.taps >= MAX_TAPS {
            break;
        }
        look(page, sess, theme, true).await;
        let mut moved = fill(page, sess, report, today, &mut filled, refill).await?;
        refill = false;
        let mut ctls: Vec<Map<String, Value>> = controls(page, "taps")
            .await?
            .into_iter()
            .filter(|c| {
                !tapped.contains(&(ctl_str(c, "label").to_string(), ctl_str(c, "tag").to_string()))
                    && groups.get(ctl_str(c, "group")).copied().unwrap_or(0) < MAX_PER_GROUP
            })
            .collect();
        // the likely "Add" first, before a tab hides it; destructive last in each view, so it is used first; a
        // way out (Cancel, Close) very last
        ctls.sort_by_key(|c| (ctl_bool(c, "leave"), ctl_bool(c, "destructive"), !ctl_bool(c, "primary")));
        for ctl in &ctls {
            if report.taps >= MAX_TAPS {
                break;
            }
            let (primary, leave) = (ctl_bool(ctl, "primary"), ctl_bool(ctl, "leave"));
            // Once the view's own Add-like taps are done, its MainButton goes before anything that may leave the
            // view or empty it (a form is submitted before its Cancel; "Add expense" before "Settings").
            if (!primary || leave) && press(page, sess, "MainButton", &mut pressed, true, !leave).await? {
                moved = true;
                refill = rejected(page).await;
                break;
            }
            let reach = page.evaluate(REACH_JS, Some(json!(ctl_str(ctl, "id")))).await?;
            let key = (ctl_str(ctl, "label").to_string(), ctl_str(ctl, "tag").to_string());
            let state = reach.get("state").and_then(Value::as_str).unwrap_or("");
            if state == "gone" {
                moved = true; // the page re-rendered: read it again
                break;
            }
            let named = if ctl_str(ctl, "label").is_empty() { ctl_str(ctl, "tag") } else { ctl_str(ctl, "label") };
            if state == "covered" {
                let first = *covered.entry(key.clone()).or_insert_with(Instant::now);
                if first.elapsed().as_secs_f64() >= COVERED_FOR_S {
                    tapped.insert(key);
                    if reach.get("small").is_some_and(value_truthy) {
                        let by = reach.get("by").map(py_str).unwrap_or_default();
                        sess.note(format!(
                            "\"{named}\" cannot be tapped: \"{by}\" stays on top of it (after {}). A toast or bar \
                             pinned to the screen must not cover controls: hide it when the view changes, and keep \
                             content clear of it.",
                            sess.action()
                        ));
                    }
                }
                continue;
            }
            tapped.insert(key);
            if state != "ok" {
                continue;
            }
            sess.step(format!("tapping \"{named}\""));
            let selector = format!("[data-check-id='{}']", ctl_str(ctl, "id"));
            if page.click(&selector, CLICK_TIMEOUT_MS).await.is_err() {
                continue; // moved or covered between the check and the tap: skip it
            }
            report.taps += 1;
            *groups.entry(ctl_str(ctl, "group").to_string()).or_insert(0) += 1;
            if primary {
                sess.st.lock().primary_taps += 1;
            }
            moved = true;
            page.wait_for_timeout(TAP_WAIT_MS).await;
            if primary {
                refill = rejected(page).await;
            }
        }
        if moved {
            continue;
        }
        let waiting = covered.keys().any(|k| !tapped.contains(k));
        if waiting && waits < 5 {
            waits += 1; // a toast on top: give it time to go
            page.wait_for_timeout(COVERED_WAIT_MS).await;
            continue;
        }
        // This view is used up: its primary action (again: "New habit" opens the form a second time), then back
        // out of it; when there is neither, the app has been seen.
        let before = errors(page).await;
        let main = press(page, sess, "MainButton", &mut pressed, false, false).await?;
        let secondary = press(page, sess, "SecondaryButton", &mut pressed, false, false).await?;
        if main || secondary {
            let after = errors(page).await;
            refill = !after.is_empty() && after != before;
            continue;
        }
        if !press(page, sess, "BackButton", &mut pressed, false, false).await? {
            break;
        }
    }
    page.wait_for_timeout(TAP_WAIT_MS).await;
    Ok(())
}

async fn errors(page: &Page) -> String {
    match page.evaluate(ERRORS_JS, None).await {
        Ok(v) => py_str(&v),
        Err(_) => String::new(), // mid-navigation
    }
}

/// Did the last press show an error next to a field: then its fields are filled again.
async fn rejected(page: &Page) -> bool {
    !errors(page).await.is_empty()
}

/// Each field once: typed into (a prefilled one with what it holds, so its change handler runs), then left, as a
/// finger leaves it for the next control; a select keeps a real choice, else takes its second option. `refill`:
/// the app just rejected what was typed, so the empty or rejected fields get a second value (once each).
async fn fill(
    page: &Page,
    sess: &Session,
    report: &mut CheckReport,
    today: NaiveDate,
    filled: &mut HashMap<(String, String, String), usize>,
    refill: bool,
) -> anyhow::Result<bool> {
    let mut moved = false;
    for ctl in controls(page, "fields").await? {
        let (label, tag, type_) = (ctl_str(&ctl, "label"), ctl_str(&ctl, "tag"), ctl_str(&ctl, "type"));
        let key = (label.to_string(), tag.to_string(), type_.to_string());
        // a field seen before is filled again when it is empty (the same form opened anew), or once more when
        // the app rejected what was typed
        let value_now = ctl_str(&ctl, "value");
        let again = filled.get(&key).is_some_and(|n| *n < 3)
            && tag != "select"
            && (value_now.is_empty() || (refill && ctl_bool(&ctl, "rejected")));
        if (filled.contains_key(&key) && !again) || report.fields >= MAX_FIELDS {
            continue;
        }
        *filled.entry(key).or_insert(0) += 1;
        let selector = format!("[data-check-id='{}']", ctl_str(&ctl, "id"));
        let shown = [label, type_, tag].into_iter().find(|s| !s.is_empty()).unwrap_or("");
        sess.step(format!("typing into the field \"{shown}\""));
        let done: anyhow::Result<bool> = async {
            if tag == "select" {
                if ctl_int(&ctl, "options") < 2 {
                    return Ok(false);
                }
                let chosen = ctl_int(&ctl, "chosen");
                let keep = chosen > 0 || (chosen == 0 && !ctl_str(&ctl, "chosenValue").is_empty());
                page.select_index(&selector, if keep { chosen } else { 1 }, CLICK_TIMEOUT_MS).await?;
            } else {
                let Some(value) = value_for(&ctl, today, again && !value_now.is_empty()) else { return Ok(false) };
                if value.is_empty() {
                    return Ok(false);
                }
                page.fill(&selector, &value, CLICK_TIMEOUT_MS).await?;
                page.evaluate_on(&selector, "(el) => el.blur()", CLICK_TIMEOUT_MS).await?;
            }
            Ok(true)
        }
        .await;
        // a field that cannot be filled is skipped, not an app bug
        if !matches!(done, Ok(true)) {
            continue;
        }
        report.fields += 1;
        moved = true;
        page.wait_for_timeout(40).await;
    }
    Ok(moved)
}

const PRESSABLE_JS: &str = "(n) => { const b = window.Telegram.WebApp[n]; return b && b.isVisible \
&& b.isActive !== false ? (b.text || n) : null; }";

/// Press Telegram's MainButton / SecondaryButton / BackButton when the app shows it. Each MainButton or
/// SecondaryButton label twice at most (`fresh`: only one never pressed; `primary_only`: only when its label
/// reads like Add, Save or Create); Back as often as there is a view to back out of.
async fn press(
    page: &Page,
    sess: &Session,
    name: &str,
    pressed: &mut HashMap<String, usize>,
    fresh: bool,
    primary_only: bool,
) -> anyhow::Result<bool> {
    let label = page.evaluate(PRESSABLE_JS, Some(json!(name))).await?;
    if label.is_null() {
        return Ok(false);
    }
    let label = py_str(&label);
    if primary_only && !PRIMARY.is_match(&label) {
        return Ok(false);
    }
    // counted per view: "Add expense" that opens the form and "Add expense" that submits it are two buttons
    let view = py_str(&page.evaluate(VIEW_JS, None).await?);
    let key = format!("{name}:{label}@{view}");
    let cap = if fresh {
        1
    } else if name == "BackButton" {
        MAX_ROUNDS
    } else {
        2
    };
    if pressed.get(&key).copied().unwrap_or(0) >= cap {
        return Ok(false);
    }
    *pressed.entry(key).or_insert(0) += 1;
    let before = sess.action();
    sess.step(if name != "BackButton" {
        format!("pressing Telegram's {name} (\"{label}\")")
    } else {
        "pressing Telegram's Back button".to_string()
    });
    let done = match page.evaluate(PRESS_JS, Some(json!(name))).await {
        Ok(v) => v,
        Err(e) => {
            // the app's handler threw: evaluate re-raises it as ours
            let text = e.to_string();
            sess.note(format!(
                "Uncaught error while {}: {}",
                sess.action(),
                first_chars(py_first_line(&text).unwrap_or(""), 300)
            ));
            return Ok(true);
        }
    };
    if done.is_null() {
        sess.set_action(before); // nothing was pressed: later issues belong to the last action
        return Ok(false);
    }
    page.wait_for_timeout(TAP_WAIT_MS).await;
    Ok(true)
}

#[cfg(test)]
mod tests;
