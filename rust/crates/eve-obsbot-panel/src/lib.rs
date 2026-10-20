//! The OBSBOT camera in Eve Link's Apps tab (docs/obsbot.md; owner, 2026-10-11: "build in an app into eve for
//! obsbot"): the camera's live picture, hold-to-turn arrows, zoom, center, sleep and wake, tracking and the AI mode.
//! Port of `eve_bridge/obsbot_panel.py`.
//!
//! The phone never talks to the camera. Every press comes through the tunnel to the daemon with the link token
//! (link_chat, `POST /api/link/controls/obsbot`), and this panel drives the camera (eve-obsbot).
//!
//! Holding an arrow is Pika's hold-to-drive (eve-pika): the app sends `hold` with the press's id about every 150 ms;
//! the first starts the camera's continuous USB turn (eve-obsbot-usb: it turns until told to stop), and the rest only
//! renew it. The gimbal stops on `stop`, when no beat came for HEARTBEAT_SECS, on `close` and when the daemon stops.
//! A late beat of a press that already ended is refused, so it never starts the camera turning again.
//!
//! The picture: ffmpeg reads the webcam through AVFoundation, alongside OBSBOT Center (macOS lets both), and writes
//! MJPEG (measured 2026-10-11: first frame 0.45 s after ffmpeg starts, ~18 KB a frame at 640 px). One ffmpeg serves
//! every viewer ([`Capture`]) and stops a few seconds after the last one leaves, so the camera is read only while a
//! screen is open. The app asks for it over WebRTC first (`{"action": "rtc", "sdp"}`, pika_rtc's wire: straight from
//! this Mac, past the tunnel), and falls back to `GET /api/link/controls/obsbot/stream`, Pika's multipart format
//! through the tunnel. Through the tunnel the frames came in bursts (gaps up to 0.7 s, 2026-10-11), as Pika's did.
//! The daemon needs the macOS camera permission; without it the screen says so in one line.
//!
//! On by default when OBSBOT Center is installed; `EVE_OBSBOT=0` turns it off.

mod capture;
mod panel;
mod rtc;

use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde_json::{json, Value};

pub use capture::{Capture, FrameResult, FrameStream, FramesFn, VideoError, Viewer};
pub use panel::{Camera, MakeCamera, ObsbotPanel, PanelOptions, Turner, UsbTurner};

use eve_obsbot::py;

/// no beat from the app for this long: stop turning
pub const HEARTBEAT_SECS: f64 = 0.45;
/// the USB continuous turn's speed: slow enough to stop where you meant
pub const HOLD_SPEED: i64 = eve_obsbot_usb::TURN_SPEED;
/// ffmpeg's first frame (0.45 s measured; the camera may be waking)
pub const VIDEO_FIRST_SECS: f64 = 6.0;
/// no new frame for this long: the video ended
pub const VIDEO_FRAME_SECS: f64 = 4.0;
pub const MAX_VIDEO_BUFFER: usize = 1 << 20;
pub const VIDEO_WIDTH: u32 = 640;
pub const VIDEO_FPS: u32 = 15;
/// the last viewer left: keep ffmpeg this long, for a reconnect
pub const LINGER_SECS: f64 = 3.0;
/// WebRTC viewers at once; past this the oldest is closed
pub const MAX_PEERS: usize = 4;
pub static DEVICE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[(\d+)\] (.*OBSBOT.*)$").expect("DEVICE_RE"));
/// ended press ids remembered, to refuse their late beats
pub const ENDED_KEEP: usize = 64;

pub const VIDEO_OFF_LINE: &str = "The camera's picture isn't available. Is the Tiny 3 plugged in?";
pub const VIDEO_DENIED_LINE: &str =
    "The Mac didn't let eve read the camera. Allow it in System Settings, Privacy, Camera.";
pub const NO_FFMPEG_LINE: &str = "The picture needs ffmpeg on the Mac (brew install ffmpeg).";

// ---- the models' camera (chat, app chat, voice; owner, 2026-10-11: "go for it build that") --------------------
// The models see tilt with + up and pan with + to the left, the way the picture moves; the camera's own pitch is +
// down (eve-obsbot), so tilt = -pitch at this boundary and nowhere else.

pub const OFF_REASON: &str =
    "The OBSBOT camera is off on this Mac (no OBSBOT camera is plugged in, or EVE_OBSBOT=0).";
pub const NOT_SET_UP_LINE: &str = "The camera didn't load on this Mac. Run: eve-bridge obsbot setup";
pub const ACTIONS: [&str; 10] =
    ["turn", "point", "center", "zoom", "wake", "sleep", "track_on", "track_off", "mode", "preset"];
/// a turn with no amount
pub const TURN_DEGREES: i64 = 20;
/// "a bit", "a little"
pub const BIT_DEGREES: i64 = 10;
/// "a lot", "all the way" is the range's end
pub const LOT_DEGREES: i64 = 45;
/// "zoom in" with no amount
pub const ZOOM_STEP: i64 = 25;
pub const TILT_RANGE: (i64, i64) = (-(eve_obsbot::PITCH_RANGE.1 as i64), -(eve_obsbot::PITCH_RANGE.0 as i64));

/// The models' two strict tools, as Python's `TOOLS` list of dicts (key order kept).
pub static TOOLS: LazyLock<Vec<Value>> = LazyLock::new(|| {
    let mut modes: Vec<Value> = eve_obsbot::keys(&eve_obsbot::AI_MODES).into_iter().map(Value::from).collect();
    modes.push(Value::Null);
    vec![
        json!({"type": "function", "name": "camera_status", "strict": true,
               "description": "The owner's OBSBOT webcam on the Mac right now: awake or asleep, where it points (pan \
degrees left or right of straight ahead, tilt degrees up or down), the zoom in percent, and its saved presets.",
               "parameters": {"type": "object", "additionalProperties": false, "required": [], "properties": {}}}),
        json!({"type": "function", "name": "camera_control", "strict": true,
               "description": "Move the owner's OBSBOT webcam on the Mac: turn it (a direction and degrees from where \
it points), point it (pan and tilt from straight ahead), center it, zoom (a level or a change), wake it or put it to \
sleep, turn AI tracking on or off, set the AI mode, or go to a saved preset. Pass null for what you don't use.",
               "parameters": {"type": "object", "additionalProperties": false,
                              "required": ["action", "direction", "degrees", "pan", "tilt", "zoom", "zoom_change",
                                           "mode", "preset"],
                              "properties": {
                                  "action": {"type": "string", "enum": ACTIONS.to_vec()},
                                  "direction": {"type": ["string", "null"],
                                                "enum": ["left", "right", "up", "down", null],
                                                "description": "turn: which way, as the picture moves; or null."},
                                  "degrees": {"type": ["integer", "null"],
                                              "description": format!(
                                                  "turn: how far, 1-120; null for {TURN_DEGREES}. \"A bit\" is \
{BIT_DEGREES}, \"a lot\" {LOT_DEGREES}.")},
                                  "pan": {"type": ["integer", "null"],
                                          "description": "point: degrees from straight ahead, -129 to 129, + is \
left; or null to keep it."},
                                  "tilt": {"type": ["integer", "null"],
                                           "description": "point: degrees from level, -59 to 59, + is up; or null \
to keep it."},
                                  "zoom": {"type": ["integer", "null"], "description": "zoom: 0-100 percent; or null."},
                                  "zoom_change": {"type": ["integer", "null"],
                                                  "description": format!(
                                                      "zoom: a change such as {ZOOM_STEP} or -{ZOOM_STEP}, when no \
level was asked; or null.")},
                                  "mode": {"type": ["string", "null"], "enum": modes,
                                           "description": "mode: single follows one person, group frames everyone, \
voice follows who speaks, desk looks down at the desk, hand follows a hand, whiteboard frames a whiteboard; or null."},
                                  "preset": {"type": ["integer", "null"],
                                             "description": "preset: 1, 2 or 3, saved in OBSBOT Center; or null."}}}}),
    ]
});

pub const TOOL_NAMES: [&str; 2] = ["camera_status", "camera_control"];

pub static INSTRUCTIONS: LazyLock<String> = LazyLock::new(|| {
    format!(
        "The owner's OBSBOT webcam, the Mac's camera (take_photo takes its pictures).
What you can do: camera_control turns it (a direction and degrees from where it points: a turn with no amount is
{TURN_DEGREES}°, \"a bit\" {BIT_DEGREES}°, \"a lot\" {LOT_DEGREES}°), points it (pan + left, tilt + up, from straight
ahead), centers it, zooms (\"zoom in\" is zoom_change {ZOOM_STEP}), wakes it or puts it to sleep, turns AI tracking on
or off, sets the AI mode and goes to a saved preset. camera_status reads where it points, the zoom and whether it
is awake.
Verify: turns, points, centering and zoom are read back from the camera: say where it ended up, in plain words
(\"a little to the left, level\"). Tracking and the AI mode are not read back: say what you set.
To aim at something by name (\"the door\", \"my desk\"), you need to see the room first: take_photo, then turn toward
it, and say if you are guessing. While tracking is on, the camera follows a person and won't stay where it was
pointed: turn tracking off first when the owner wants it to hold still.
Limits: moving, zoom and waking always work. Tracking, the AI mode, presets and sleep are OBSBOT's own features and
work only while the OBSBOT Center app runs on the Mac; when one can't, say so in one line."
    )
});

// ---- Python numbers, as the replies carry them ---------------------------------------------------------------

/// A Python int or float from a reply, for the arithmetic `where` and `-pitch` do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum PyNum {
    Int(i64),
    Float(f64),
}

impl PyNum {
    pub(crate) fn of(v: &Value) -> Option<PyNum> {
        match v {
            Value::Bool(b) => Some(PyNum::Int(i64::from(*b))),
            Value::Number(n) => match n.as_i64() {
                Some(i) => Some(PyNum::Int(i)),
                None => n.as_f64().map(PyNum::Float),
            },
            _ => None,
        }
    }

    fn f(self) -> f64 {
        match self {
            PyNum::Int(i) => i as f64,
            PyNum::Float(f) => f,
        }
    }

    pub(crate) fn neg(self) -> PyNum {
        match self {
            PyNum::Int(i) => i.checked_neg().map_or(PyNum::Float(-(i as f64)), PyNum::Int),
            PyNum::Float(f) => PyNum::Float(-f),
        }
    }

    fn abs(self) -> PyNum {
        match self {
            PyNum::Int(i) => i.checked_abs().map_or(PyNum::Float((i as f64).abs()), PyNum::Int),
            PyNum::Float(f) => PyNum::Float(f.abs()),
        }
    }

    pub(crate) fn to_value(self) -> Value {
        match self {
            PyNum::Int(i) => Value::from(i),
            PyNum::Float(f) => serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number),
        }
    }

    fn py_str(self) -> String {
        match self {
            PyNum::Int(i) => i.to_string(),
            PyNum::Float(f) => py::float_repr(f),
        }
    }
}

/// Where the camera points, in the owner's words: "20° left, 10° up", "straight ahead". Python's `where`.
pub fn where_(pan: Option<i64>, pitch: Option<i64>) -> String {
    where_value(&pan.map_or(Value::Null, Value::from), &pitch.map_or(Value::Null, Value::from))
}

/// `where` over a reply's values (an int or a float each; anything else reads as unknown).
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub(crate) fn where_value(pan: &Value, pitch: &Value) -> String {
    let (Some(pan), Some(pitch)) = (PyNum::of(pan), PyNum::of(pitch)) else {
        return "somewhere I couldn't read".to_string();
    };
    let tilt = pitch.neg();
    let mut parts: Vec<String> = Vec::new();
    // Python's `abs(pan) <= 2` is false for NaN, so NaN is named, as there
    if !(pan.abs().f() <= 2.0) {
        parts.push(format!("{}° {}", pan.abs().py_str(), if pan.f() > 0.0 { "left" } else { "right" }));
    }
    if tilt.abs().f() > 2.0 {
        parts.push(format!("{}° {}", tilt.abs().py_str(), if tilt.f() > 0.0 { "up" } else { "down" }));
    }
    if parts.is_empty() {
        "straight ahead".to_string()
    } else {
        parts.join(", ")
    }
}

// ---- the code path: a message that is only a camera command names the camera -----------------------------------

/// A camera command the code understood.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Command {
    /// status, turn, center, zoom, wake, sleep, track_on, track_off, preset
    pub action: String,
    pub direction: String,
    pub degrees: Option<i64>,
    pub zoom: Option<i64>,
    pub zoom_change: Option<i64>,
    pub preset: Option<i64>,
}

impl Command {
    /// `Command(action)` with every other field at its default.
    pub fn new(action: &str) -> Command {
        Command { action: action.to_string(), ..Default::default() }
    }

    pub fn turn(direction: &str, degrees: i64) -> Command {
        Command { direction: direction.to_string(), degrees: Some(degrees), ..Command::new("turn") }
    }

    pub fn zoom_to(zoom: i64) -> Command {
        Command { zoom: Some(zoom), ..Command::new("zoom") }
    }

    pub fn zoom_by(change: i64) -> Command {
        Command { zoom_change: Some(change), ..Command::new("zoom") }
    }

    pub fn preset(n: i64) -> Command {
        Command { preset: Some(n), ..Command::new("preset") }
    }
}

const LEAD: &str = r"(?:(?:hey|ok|okay)\s+)?(?:eve\s+)?(?:(?:please|pls|can you|could you|would you)\s+)?";
const TAIL: &str = r"(?:\s+(?:please|pls|thanks|thank you|eve|now))?";
const CAM: &str = r"(?:the\s+|my\s+)?(?:camera|webcam|cam|obsbot)";
const TURN_VERB: &str = r"(?:turn|move|pan|tilt|point|swing|rotate|look)";
const AMOUNT_NAMED: &str = r"(?:\s+(?:by\s+)?(?:(?P<bit>a\s+(?:bit|little|touch|tad)|slightly)|(?P<lot>a\s+lot|a\s+bunch|way)|(?P<deg>\d{1,3})\s*(?:°|degrees?|deg)))?";
const DIR_NAMED: &str = r"(?:to\s+the\s+)?(?P<dir>left|right|up|down)";
const ZOOM_AMOUNT: &str = r"(?:\s+(?:(?P<bit>a\s+(?:bit|little|touch))|(?P<all>all\s+the\s+way)))?";

/// Python's `int()` of what `\d{1,3}` matched: `\d` is any Unicode decimal digit, and int() reads them all ("٣٠"
/// is 30). Unicode keeps every run of decimal digits contiguous and in order from 0, so a digit's value is how many
/// decimal digits come right before it, modulo 10.
fn digits(s: &str) -> Option<i64> {
    use unicode_general_category::{get_general_category, GeneralCategory as G};
    let decimal = |c: char| get_general_category(c) == G::DecimalNumber;
    let mut n: i64 = 0;
    for c in s.chars() {
        let value = match c.to_digit(10) {
            Some(v) => i64::from(v),
            None if decimal(c) => {
                let mut before = 0;
                let mut cp = c as u32;
                while let Some(prev) = cp.checked_sub(1).and_then(char::from_u32) {
                    if !decimal(prev) {
                        break;
                    }
                    before += 1;
                    cp -= 1;
                }
                before % 10
            }
            None => return None,
        };
        n = n.checked_mul(10)?.checked_add(value)?;
    }
    Some(n)
}

fn group<'t>(m: &Captures<'t>, name: &str) -> Option<&'t str> {
    m.name(name).map(|g| g.as_str()).filter(|s| !s.is_empty())
}

fn turn_of(m: &Captures) -> Option<Command> {
    let deg = if group(m, "bit").is_some() {
        BIT_DEGREES
    } else if group(m, "lot").is_some() {
        LOT_DEGREES
    } else if let Some(d) = group(m, "deg") {
        digits(d)?
    } else {
        TURN_DEGREES
    };
    if !(1..=120).contains(&deg) {
        return None; // an odd amount: the model gets it
    }
    Some(Command::turn(group(m, "dir").unwrap_or(""), deg))
}

fn zoom_step(m: &Captures, sign: i64) -> Option<Command> {
    if group(m, "all").is_some() {
        return Some(Command::zoom_to(if sign > 0 { 100 } else { 0 }));
    }
    let step = if group(m, "bit").is_some() { ZOOM_STEP / 2 } else { ZOOM_STEP };
    Some(Command::zoom_by(sign * step))
}

type Make = fn(&Captures) -> Option<Command>;

fn sub(form: &str) -> String {
    form.replace("{cam}", CAM)
        .replace("{verb}", TURN_VERB)
        .replace("{dir}", DIR_NAMED)
        .replace("{amount}", AMOUNT_NAMED)
        .replace("{zamount}", ZOOM_AMOUNT)
}

/// The patterns in Python's order, each anchored at both ends with the lead and the tail around it (`re.fullmatch`).
static PATTERNS: LazyLock<Vec<(Regex, Make)>> = LazyLock::new(|| {
    let mut forms: Vec<(String, Make)> = vec![
        // "camera left", "turn the camera left a bit", "pan the webcam right 30 degrees", "camera to the left"
        (sub(r"(?:{verb}\s+)?{cam}\s+{dir}{amount}"), turn_of),
        (sub(r"{verb}\s+{dir}{amount}\s+(?:on\s+|with\s+)?{cam}"), turn_of),
        // "center the camera", "camera center", "camera straight ahead", "reset the camera"
        (
            sub(r"(?:center|centre|recenter|reset|straighten)\s+{cam}|{cam}\s+(?:to\s+(?:the\s+)?)?(?:center|centre|straight\s+ahead|middle|home)|point\s+{cam}\s+straight(?:\s+ahead)?"),
            |_| Some(Command::new("center")),
        ),
    ];
    // "zoom the camera in", "camera zoom in a bit", "zoom in on the camera" (the camera named), "camera zoom 50%"
    let zoom_forms = [r"zoom\s+{cam}\s+{way}{zamount}", r"{cam}\s+zoom\s+{way}{zamount}", r"zoom\s+{way}{zamount}\s+(?:on|with)\s+{cam}"];
    for (way, make) in [("in", (|m: &Captures| zoom_step(m, 1)) as Make), ("out", |m: &Captures| zoom_step(m, -1))] {
        for form in zoom_forms {
            forms.push((sub(&form.replace("{way}", way)), make));
        }
    }
    forms.extend([
        (sub(r"unzoom\s+{cam}"), (|_: &Captures| Some(Command::zoom_to(0))) as Make),
        (
            sub(r"(?:set\s+)?{cam}\s+zoom\s+(?:to\s+)?(?P<z>\d{1,3})\s*%?|zoom\s+{cam}\s+to\s+(?P<z2>\d{1,3})\s*%?"),
            |m| {
                let z = group(m, "z").or_else(|| group(m, "z2"))?;
                Some(Command::zoom_to(digits(z)?))
            },
        ),
        // "wake the camera", "camera on" (it wakes: OBSBOT's sleep is its off), "camera off", "put the camera to sleep"
        (
            sub(r"wake\s+(?:up\s+)?{cam}(?:\s+up)?|(?:turn|switch)\s+on\s+{cam}|(?:turn|switch)\s+{cam}\s+on|{cam}\s+(?:on|wake(?:\s+up)?)"),
            |_| Some(Command::new("wake")),
        ),
        (
            sub(r"(?:turn|switch)\s+off\s+{cam}|(?:turn|switch)\s+{cam}\s+off|{cam}\s+(?:off|sleep)|put\s+{cam}\s+to\s+sleep"),
            |_| Some(Command::new("sleep")),
        ),
        // "where is the camera pointing", "camera status"
        (
            sub(r"{cam}\s+status|where(?:'s|\s+is)\s+{cam}\s+(?:pointing|pointed|looking|aimed)|is\s+{cam}\s+(?:on|awake|asleep)"),
            |_| Some(Command::new("status")),
        ),
        // "camera track me", "camera follow me", "camera stop tracking", "stop the camera tracking"
        (
            sub(r"{cam}\s+(?:track|follow)\s+me|(?:turn\s+on|start)\s+{cam}\s+tracking|{cam}\s+tracking\s+on"),
            |_| Some(Command::new("track_on")),
        ),
        (
            sub(r"{cam}\s+stop\s+(?:tracking|following)(?:\s+me)?|stop\s+{cam}\s+(?:tracking|following)(?:\s+me)?|(?:turn\s+off)\s+{cam}\s+tracking|{cam}\s+tracking\s+off"),
            |_| Some(Command::new("track_off")),
        ),
        // "camera preset 2", "camera to preset 1"
        (
            sub(r"{cam}\s+(?:to\s+)?preset\s+(?P<n>[123])|(?:go\s+to\s+)?preset\s+(?P<n2>[123])\s+on\s+{cam}"),
            |m| {
                let n = group(m, "n").or_else(|| group(m, "n2"))?;
                Some(Command::preset(n.parse().ok()?))
            },
        ),
    ]);
    forms
        .into_iter()
        .map(|(p, make)| (Regex::new(&format!("^(?:{LEAD}(?:{p}){TAIL})$")).expect("obsbot pattern"), make))
        .collect()
});

/// Python's `\w` in a str pattern: `str.isalnum()` (a letter or a number, by general category) or `_`. Rust's
/// `\w` differs (it takes combining marks and connector punctuation, and leaves out numbers such as `²`).
fn py_word(c: char) -> bool {
    use unicode_general_category::{get_general_category, GeneralCategory as G};
    c == '_'
        || matches!(
            get_general_category(c),
            G::UppercaseLetter
                | G::LowercaseLetter
                | G::TitlecaseLetter
                | G::ModifierLetter
                | G::OtherLetter
                | G::DecimalNumber
                | G::LetterNumber
                | G::OtherNumber
        )
}

/// Python's `_norm`: lower, `’` to `'`, `re.sub(r"[^\w%°'\s-]", " ")`, `re.sub(r"\s+", " ")`, `strip()`.
fn norm(text: &str) -> String {
    let t = text.to_lowercase().replace('’', "'");
    let mut out = String::with_capacity(t.len());
    let mut space = false;
    for c in t.chars() {
        let c = if py_word(c) || matches!(c, '%' | '°' | '\'' | '-') || py::is_space(c) { c } else { ' ' };
        if py::is_space(c) {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    out
}

/// A Command when the whole message is a camera command that names the camera, else None (the model answers).
/// Anchored at both ends: "turn the camera to the door" leaves words over, so the model gets it. Reads no state.
pub fn r#match(text: &str) -> Option<Command> {
    let t = norm(text);
    if t.is_empty() || t.chars().count() > 100 {
        return None;
    }
    for (pattern, make) in PATTERNS.iter() {
        if let Some(m) = pattern.captures(&t) {
            let cmd = make(&m);
            if cmd.as_ref().is_some_and(|c| c.zoom.is_some_and(|z| z > 100)) {
                return None;
            }
            return cmd;
        }
    }
    None
}

/// On unless EVE_OBSBOT=0, while an OBSBOT camera is plugged in or OBSBOT Center is installed. `None` reads the
/// process environment.
pub fn enabled(environ: Option<&HashMap<String, String>>) -> bool {
    let flag = match environ {
        Some(env) => env.get("EVE_OBSBOT").cloned(),
        None => std::env::var("EVE_OBSBOT").ok(),
    };
    py::strip(flag.as_deref().unwrap_or("1")) != "0"
        && (Path::new(eve_obsbot::APP_PATH).exists() || eve_obsbot_usb::present(&eve_obsbot_usb::SubprocessRunner))
}

pub fn find_ffmpeg() -> Option<String> {
    if let Ok(p) = which::which("ffmpeg") {
        return Some(p.to_string_lossy().into_owned());
    }
    ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"].into_iter().find(|p| Path::new(p).exists()).map(String::from)
}

/// Python's `_num`: a number from the app or a model, or None (a bool is not one).
pub fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => py::parse_float(s),
        _ => None,
    }
}

/// The panel, or None when it is off (EVE_OBSBOT=0) or OBSBOT Center isn't installed. Making it sends nothing.
pub fn make_panel(environ: Option<&HashMap<String, String>>) -> Option<ObsbotPanel> {
    if enabled(environ) {
        Some(ObsbotPanel::new(PanelOptions { environ: environ.cloned(), ..Default::default() }))
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
