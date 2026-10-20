//! eve keeps its eyes on whoever is talking to it — and works out where they went when it loses them.
//!
//! Port of `eve_bridge/follow.py`.
//!
//! The Mac already finds every face in every camera frame (vision) and tells the board where the
//! largest one is. What the board does with that is its own policy (gaze.cpp): it live-tracks the
//! OWNER, in its attention and dictation states. In a voice conversation it goes to one fixed
//! "facing you" pose and stays there; a face that is not the owner's gets a single glance in the
//! calm states and nothing more. So a guest talking to eve is never followed, and neither is the
//! owner once the conversation has opened — lean back, stand up, walk round the desk, and eve
//! keeps addressing the chair.
//!
//! This module is the policy for a conversation, on the host, with no reflash: the firmware accepts a
//! host pose in any state while a conversation is open, and "a host look owns the head from the
//! moment it lands until its hold runs out" (hostlook.h).
//!
//! IT FOLLOWS A PERSON, NOT A SIGHTING. The first version turned toward every face box it was handed,
//! and on a real face (2026-09-21) it swung ±20° around someone sitting still. The offsets logged with
//! each move showed why: a frame reaches this code about half a second after it was taken (JPEG on the
//! board, 115200 baud, a Vision request), so "0.6 s after the move" was still a picture from mid-swing,
//! stamped with a pose the head had already left. So there is a track:
//!
//! - a position and a VELOCITY in absolute head angles, averaged over sightings;
//! - nothing is evidence for SETTLE_SECS after a move, which covers the glide AND that pipeline delay;
//! - a sighting has to AGREE with where the track says the person should be (AGREE_DEG) before the head
//!   acts on it; one that does not is an outlier until a second one says the same, and then it is the
//!   truth (they really did jump, or it is someone else);
//! - moves are limited to MAX_STEP_DEG, led slightly by the velocity (LEAD_SECS) so a walking person is
//!   met rather than chased, inside a deadband, with the hold renewed while the face sits still.
//!
//! WHEN IT LOSES THEM IT REASONS, IN THIS ORDER, from what it knew a moment ago and what it has learned:
//!
//! 1. WHERE THEY WERE HEADING. If the last sightings were moving (SPEED_FAST deg/s), or the face left by a
//!    frame edge, it looks further the same way — one field-of-view step, then another. Someone who
//!    leaves quickly is followed at once (LEAVING_HOLD_SECS), not after the ordinary patience.
//! 2. WHERE THEY USUALLY ARE. A PresenceMap — a decayed histogram of the absolute angles at which faces
//!    have been followed, kept in ~/.config/eve-bridge/presence.json across restarts — gives the two
//!    likeliest places that have not just been tried. At a desk that is "the chair" and "standing".
//! 3. EITHER SIDE of where they were, one field-of-view step: at a desk people shift sideways more than
//!    anything else, and this is what finds them before the map has learned anything.
//! 4. UP AND DOWN at the last heading: they stood up, or sat back.
//!
//! Then it gives the head back, and it does NOT search again until it has really seen someone again: one
//! search per loss, at most MAX_SEARCHES a conversation. The first live run (2026-09-21) searched three
//! times back to back at an empty chair, which is how an anxious robot behaves.
//!
//! IT USES WHAT THE DAEMON KNOWS:
//! - WHO IS TALKING, from the conversation's phase: it follows in `wake`, `listening`, `asking` and
//!   `speaking`; in `thinking` (the glance aside) and `working` (head down at the desk) it hands the head
//!   back at once, so eve keeps its own expressions.
//! - WHICH FACE: with several, the one nearest the track, so a passer-by does not steal the gaze.
//! - WHAT THE OWNER ASKED FOR: "look left", look_around and find own the head for as long as they hold it.
//!   An explore, the dictation key and a lesson's listening pose own it too (`blocked`).
//!
//! WHEN A CONVERSATION OPENS AND NOBODY IS IN VIEW, it does not wait at the board's fixed pose hoping. On
//! 2026-09-21 the owner sat 23° to eve's left and a little below level, and from the fixed pose his face
//! was in a quarter of the frames, mostly at the edge. After ACQUIRE_AFTER_SECS with no confirmed face it
//! looks where they were last followed, then at the place the map says people most often are, then a
//! little lower there (with nothing learned yet: lower, lower still, then higher than the head is now),
//! and follows from there. That opening re-arms each time the conversation comes back
//! to a follow phase with nobody in view (a desk call hands the head back for every "thinking", and the
//! board's level pose looks over a seated owner: on 2026-09-27 its frames held only the top of his hair),
//! at most MAX_OPENINGS times a conversation.
//!
//! AND IT TELLS THE MEMORY BUS. Every change — seen, lost, found and how — is published on
//! `/eve/presence`, so rosbridge clients (Foxglove, `eve-bridge memory tail`) see eve's sense of
//! where its person is, live. The control loop itself reads the local map, never the bus or claude-mem:
//! a head cannot wait on an HTTP recall. Nothing here identifies anyone; the map holds angles, not faces.
//!
//! Concurrency: the follower is shared (`Arc<SpeakerFollower>`) between the vision callback, the phase
//! callback and the head's `on_move`. Its state sits behind a `parking_lot::Mutex` that is never held
//! across an `.await`, so — as in the single-threaded asyncio original — other calls can only interleave
//! at the sends.

use std::collections::HashMap;
use std::ffi::{CStr, CString, OsString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use eve_head::{build_look_cmd, clamp_pose, target_pose, CAMERA_HFOV_DEG};
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{Map, Value};
use tracing::{debug, info, warn};

pub const FOLLOW_PHASES: &[&str] = &["wake", "listening", "asking", "speaking"];
pub const REFUSED_DEG: f64 = 8.0; // the echoed pose this far from the asked pose after settling: the board said no
pub const SETTLE_SECS: f64 = 1.2; // after a move: the glide (≥ 0.3 s) plus ~0.5 s of frame pipeline, with margin
pub const DEADBAND_YAW: f64 = 6.0; // degrees off-centre before the head bothers to turn
pub const DEADBAND_PITCH: f64 = 5.0;
pub const SMOOTH: f64 = 0.5; // weight of a new agreeing sighting in the track's position
pub const VEL_SMOOTH: f64 = 0.3; // face boxes jitter by a few degrees; a velocity is an average, not a difference
pub const VEL_MIN_DT: f64 = 0.4; // two sightings closer together than this say nothing about speed
pub const AGREE_DEG: f64 = 14.0; // a sighting this close to where the track expects the person is the same person
pub const MAX_STEP_DEG: f64 = 20.0; // one move never swings further than this
pub const LEAD_SECS: f64 = 0.35; // aim this far ahead along the velocity
pub const HOLD_SECS: f64 = 4.0; // each pose owns the head this long …
pub const REFRESH_SECS: f64 = 2.5; // … and is renewed this often while the face stays put
pub const LOST_HOLD_SECS: f64 = 2.5; // a still person who vanished: people look down; wait this long
pub const LEAVING_HOLD_SECS: f64 = 0.8; // a person who was moving fast, or left by a frame edge: go after them now
pub const SPEED_FAST: f64 = 15.0; // deg/s: someone really leaving; a sitting person's box jitter reads as up to ~12
pub const STALE_TRACK_SECS: f64 = 6.0; // a track this old is history: forget it rather than search on it
pub const EDGE: i64 = 65; // |bx| or |by| at or past this is "by the edge of the frame"
pub const SEARCH_STEP_SECS: f64 = 1.6; // settle, then at least one clean frame, at each place it looks
pub const MAX_SEARCHES: usize = 3; // per conversation
pub const USUAL_PLACES: usize = 2;
pub const MAX_PLACES: usize = 6; // a search is at most this many looks (~10 s), likeliest first
pub const ACQUIRE_AFTER_SECS: f64 = 1.5; // a conversation opened and nobody is in view yet: look where they usually are
pub const LOWER_DEG: i64 = 18; // the opening's last look: this much lower than the usual place (a seated person)
pub const MAX_OPENINGS: usize = 4; // opening sequences per conversation (each at most three looks)
pub const MIN_CONF: i64 = 35; // the tracker's 0..100; weaker boxes are posters and lamps
pub const MIN_SIZE: i64 = 4; // a face under 4% of the frame width is across the room
pub const MAX_YAW: i64 = 100; // short of the neck's ±120: past that the robot is looking behind itself
pub const PRESENCE_TOPIC: &str = "/eve/presence";
pub const DEFAULT_PRESENCE_FILE: &str = "~/.config/eve-bridge/presence.json";

/// Sends one command dict to the board; true when it went out (same shape as `eve_head::Sender`).
pub type Sender = eve_head::Sender;
/// `publish(topic, msg)`: the memory bus. An `Err` (or a panic) is logged at debug and ignored.
pub type Publisher = Arc<dyn Fn(&str, Map<String, Value>) -> Result<(), String> + Send + Sync>;
/// A monotonic clock in seconds.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;
/// A wall clock in epoch seconds (`time.time`).
pub type Wall = Arc<dyn Fn() -> f64 + Send + Sync>;
/// The daemon's word on who else owns the head right now ("" = nobody).
pub type Blocked = Arc<dyn Fn() -> String + Send + Sync>;

/// One place to look: (yaw, pitch, why).
pub type Place = (i64, i64, String);

static MONO_START: LazyLock<Instant> = LazyLock::new(Instant::now);

/// `time.monotonic` stand-in: seconds since first use in this process.
pub fn monotonic() -> f64 {
    MONO_START.elapsed().as_secs_f64()
}

/// `time.time()`.
pub fn wall_time() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

pub fn is_follow_phase(phase: &str) -> bool {
    FOLLOW_PHASES.contains(&phase)
}

/// One face in a frame, vision's `FaceResult` shape: bx/by -100..100 from the frame centre (+ = right,
/// + = below), size = face width as % of frame width, conf 0..100.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Face {
    pub bx: i64,
    pub by: i64,
    pub size: i64,
    pub conf: i64,
}

impl Face {
    pub fn new(bx: i64, by: i64, size: i64, conf: i64) -> Self {
        Face { bx, by, size, conf }
    }
}

impl From<(i64, i64, i64, i64)> for Face {
    fn from((bx, by, size, conf): (i64, i64, i64, i64)) -> Self {
        Face { bx, by, size, conf }
    }
}

/// Python's `int(round(x))`: round half to even.
fn py_round(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// Python's `max(lo, min(hi, x))`, NaN included (it becomes `hi`).
fn clip(x: f64, lo: f64, hi: f64) -> f64 {
    let m = if x < hi { x } else { hi };
    if m > lo {
        m
    } else {
        lo
    }
}

fn clip_yaw(x: f64) -> f64 {
    clip(x, -(MAX_YAW as f64), MAX_YAW as f64)
}

/// Python's `round(x, ndigits)` for display: correctly rounded on the exact binary value.
fn py_round_to(x: f64, ndigits: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.ndigits$}").parse().unwrap_or(x)
}

/// A learned place, brought inside the poses the follower may ask for: the map's cells are 10° bins
/// and can sit on 0 or past MAX_YAW (2026-09-27: an opening look at yaw 120 pitch 0 turned the head right
/// round to face the wall behind it).
fn reachable(yaw: f64, pitch: f64) -> (i64, i64) {
    let (y, p, _) = clamp_pose(clip_yaw(yaw), pitch);
    (y, p)
}

/// The person being followed, in absolute head angles.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub yaw: f64,
    pub pitch: f64,
    pub seen_at: f64,
    pub vyaw: f64, // deg/s
    pub vpitch: f64,
    pub confirmed: u32,             // agreeing sightings in a row
    pub edge: (i64, i64),           // the last sighting's bx, by: which side of the frame they were on
    pub size: i64,
    pub doubt: Option<(f64, f64)>, // one sighting that did not agree, waiting for a second
}

impl Track {
    /// `Track(yaw, pitch, seen_at)` with the dataclass defaults.
    pub fn new(yaw: f64, pitch: f64, seen_at: f64) -> Self {
        Track { yaw, pitch, seen_at, vyaw: 0.0, vpitch: 0.0, confirmed: 1, edge: (0, 0), size: 0, doubt: None }
    }

    fn sighted(yaw: f64, pitch: f64, seen_at: f64, confirmed: u32, face: &Face) -> Self {
        Track { confirmed, edge: (face.bx, face.by), size: face.size, ..Track::new(yaw, pitch, seen_at) }
    }

    pub fn speed(&self) -> f64 {
        self.vyaw.hypot(self.vpitch)
    }
}

/// Where faces have been followed: a decayed histogram over absolute head angles.
///
/// BIN-degree cells; a cell's weight halves every HALF_LIFE_DAYS, so the map follows a moved desk.
/// It answers one question — "where is this person usually?" — and holds angles, never images.
pub struct PresenceMap {
    pub path: Option<PathBuf>,
    pub wall: Wall,
    /// Insertion-ordered, like the Python dict: equal weights keep their first-noted order.
    pub cells: IndexMap<(i64, i64), f64>,
    pub stamp: f64,
    pub dirty: bool,
}

impl std::fmt::Debug for PresenceMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PresenceMap")
            .field("path", &self.path)
            .field("cells", &self.cells)
            .field("stamp", &self.stamp)
            .field("dirty", &self.dirty)
            .finish()
    }
}

impl PresenceMap {
    pub const BIN: i64 = 10;
    pub const HALF_LIFE_DAYS: f64 = 14.0;
    pub const MAX_CELLS: usize = 120;

    /// `PresenceMap(path)` on the wall clock.
    pub fn new(path: Option<PathBuf>) -> Self {
        Self::with_wall(path, Arc::new(wall_time))
    }

    /// `PresenceMap(path, wall=...)`.
    pub fn with_wall(path: Option<PathBuf>, wall: Wall) -> Self {
        let stamp = wall();
        PresenceMap { path, wall, cells: IndexMap::new(), stamp, dirty: false }
    }

    pub fn cell(yaw: f64, pitch: f64) -> (i64, i64) {
        let b = Self::BIN as f64;
        (py_round(yaw / b) * Self::BIN, py_round(pitch / b) * Self::BIN)
    }

    fn decay(&mut self) {
        let now = (self.wall)();
        let days = ((now - self.stamp) / 86400.0).max(0.0);
        if days > 0.01 {
            let k = 0.5f64.powf(days / Self::HALF_LIFE_DAYS);
            self.cells = self.cells.iter().filter(|(_, w)| *w * k >= 0.05).map(|(c, w)| (*c, w * k)).collect();
            self.stamp = now;
        }
    }

    /// `note(yaw, pitch)` with the default weight 1.0.
    pub fn note(&mut self, yaw: f64, pitch: f64) {
        self.note_weight(yaw, pitch, 1.0)
    }

    pub fn note_weight(&mut self, yaw: f64, pitch: f64, weight: f64) {
        let c = Self::cell(yaw, pitch);
        *self.cells.entry(c).or_insert(0.0) += weight;
        self.dirty = true;
    }

    /// `top(n)`: no exclusions, min_weight 3.0.
    pub fn top(&mut self, n: usize) -> Vec<(i64, i64)> {
        self.top_with(n, &[], 3.0)
    }

    /// `top(n, exclude=...)`.
    pub fn top_excluding(&mut self, n: usize, exclude: &[(f64, f64)]) -> Vec<(i64, i64)> {
        self.top_with(n, exclude, 3.0)
    }

    /// The n heaviest cells, skipping any within a cell's width of a place in `exclude`.
    pub fn top_with(&mut self, n: usize, exclude: &[(f64, f64)], min_weight: f64) -> Vec<(i64, i64)> {
        self.decay();
        let mut out = Vec::new();
        for ((y, p), w) in self.sorted_by_weight() {
            if w < min_weight || out.len() >= n {
                break;
            }
            let b = Self::BIN as f64;
            if exclude.iter().any(|&(ey, ep)| (y as f64 - ey).abs() <= b && (p as f64 - ep).abs() <= b) {
                continue;
            }
            out.push((y, p));
        }
        out
    }

    /// `sorted(cells.items(), key=lambda kv: -kv[1])`: heaviest first, stable for equal weights.
    fn sorted_by_weight(&self) -> Vec<((i64, i64), f64)> {
        let mut v: Vec<((i64, i64), f64)> = self.cells.iter().map(|(c, w)| (*c, *w)).collect();
        v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        v
    }

    /// Read the map from `path`; no map yet, or an unreadable one, starts learning again.
    pub fn load(mut self) -> Self {
        let Some(path) = self.path.clone() else {
            return self;
        };
        match read_presence(&path) {
            Some((cells, stamp)) => {
                self.cells = cells;
                match stamp {
                    Some(Some(s)) => {
                        self.stamp = s;
                        self.decay();
                    }
                    Some(None) => {
                        self.stamp = (self.wall)();
                        self.decay();
                    }
                    None => self.cells = IndexMap::new(), // an unreadable stamp: start learning again
                }
            }
            None => self.cells = IndexMap::new(),
        }
        self
    }

    /// Write the map (heaviest MAX_CELLS cells) when something new was noted. True when it wrote.
    pub fn save(&mut self) -> bool {
        let Some(path) = self.path.clone() else {
            return false;
        };
        if !self.dirty {
            return false;
        }
        self.decay();
        let cells: Vec<((i64, i64), f64)> = self.sorted_by_weight().into_iter().take(Self::MAX_CELLS).collect();
        let mut body = format!("{{\"stamp\": {}, \"bin_deg\": {}, \"cells\": {{", py_float_repr(self.stamp), Self::BIN);
        for (i, ((y, p), w)) in cells.iter().enumerate() {
            if i > 0 {
                body.push_str(", ");
            }
            body.push_str(&format!("\"{y},{p}\": {}", py_float_repr(py_round_to(*w, 2))));
        }
        body.push_str("}}");
        let write = || -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)?;
                }
            }
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, body.as_bytes())?;
            std::fs::rename(&tmp, &path)
        };
        match write() {
            Ok(()) => {
                self.dirty = false;
                true
            }
            Err(e) => {
                warn!("follow: could not save the presence map ({})", e);
                false
            }
        }
    }
}

type Cells = IndexMap<(i64, i64), f64>;

/// The file's cells, and its stamp: `Some(Some(x))` a stamp, `Some(None)` a falsy/missing one (use the wall
/// clock), `None` one `float()` refuses. `None` overall: the file or its cells could not be read.
fn read_presence(path: &Path) -> Option<(Cells, Option<Option<f64>>)> {
    let text = std::fs::read_to_string(path).ok()?;
    let data: Value = serde_json::from_str(&text).ok()?;
    let obj = data.as_object()?;
    let mut cells = IndexMap::new();
    match obj.get("cells") {
        Some(v) if py_truthy(v) => {
            let m = v.as_object()?;
            for (k, v) in m {
                let mut parts = k.split(',');
                let y = py_int(parts.next()?)?;
                let p = py_int(parts.next()?)?;
                cells.insert((y, p), py_float(v)?);
            }
        }
        _ => {}
    }
    let stamp = match obj.get("stamp") {
        Some(v) if py_truthy(v) => py_float(v).map(Some),
        _ => Some(None),
    };
    Some((cells, stamp))
}

fn py_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python's `int(str)`: surrounding whitespace, a sign, digits with single underscores between them.
fn py_int(s: &str) -> Option<i64> {
    let t = s.trim();
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let clean: String = digits.chars().filter(|c| *c != '_').collect();
    if !clean.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let n: i64 = clean.parse().ok()?;
    Some(if neg { -n } else { n })
}

/// Python's `float(v)` for a JSON value: numbers, bools, numeric strings; anything else is a TypeError.
fn py_float(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => {
            let t = s.trim();
            if t.contains('_') {
                let ok = t.as_bytes().windows(3).all(|w| w[1] != b'_' || (w[0].is_ascii_digit() && w[2].is_ascii_digit()))
                    && !t.starts_with('_')
                    && !t.ends_with('_');
                if !ok {
                    return None;
                }
                t.replace('_', "").parse().ok()
            } else {
                t.parse().ok()
            }
        }
        _ => None,
    }
}

/// Python's `repr(float)` as `json.dumps` writes it.
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
    let a = x.abs();
    if !(1e-4..1e16).contains(&a) {
        let s = format!("{x:e}");
        let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
        let e: i32 = exp.parse().unwrap_or(0);
        let sign = if e < 0 { '-' } else { '+' };
        return format!("{mant}e{sign}{:02}", e.abs());
    }
    let s = format!("{x}");
    if s.contains('.') {
        s
    } else {
        s + ".0"
    }
}

/// One search for someone it lost (`_Search`).
#[derive(Debug, Clone, PartialEq)]
pub struct Search {
    pub places: Vec<Place>, // (yaw, pitch, why) still to look at
    pub started_at: f64,
    pub step_at: f64,
    pub looking: String, // the reason for the place it is looking at now
    pub tried: Vec<(f64, f64)>,
}

impl Search {
    fn new(places: Vec<Place>, started_at: f64) -> Self {
        Search { places, started_at, step_at: f64::NEG_INFINITY, looking: String::new(), tried: Vec::new() }
    }
}

/// The keyword arguments of `SpeakerFollower(...)`, with the same defaults.
pub struct FollowerOptions {
    pub clock: Clock,
    pub enabled: bool,
    pub blocked: Blocked,
    pub presence: Option<PresenceMap>,
    pub publish: Option<Publisher>,
}

impl Default for FollowerOptions {
    fn default() -> Self {
        FollowerOptions {
            clock: Arc::new(monotonic),
            enabled: true,
            blocked: Arc::new(String::new),
            presence: None,
            publish: None,
        }
    }
}

struct Inner {
    phase: String,
    owner_hold_until: f64,
    track: Option<Track>,
    commanded: Option<(i64, i64)>, // the last pose sent
    last_move_at: f64,
    holding: bool, // a follower pose currently owns the head
    search: Option<Search>,
    moves: usize,
    searches: usize,
    found_by: Vec<String>, // how each re-acquisition happened, this conversation
    noted_at: f64,
    announced: String,       // the last presence state published
    opened_at: Option<f64>,  // when the current stretch of follow phases began
    acquired: bool,          // this stretch's opening sequence has been planned
    opening: Vec<Place>,     // its looks still to make
    opening_at: f64,
    openings: usize,                 // opening sequences this conversation
    last_seen: Option<(f64, f64)>,   // where someone was last followed
    presence: PresenceMap,
}

/// What `on_faces` does after its synchronous part.
enum Next {
    Nothing,
    Lost,
    Look(i64, i64, &'static str, Face),
    Renew(i64, i64),
}

/// Turn the head toward the person eve is talking with, and find them again (the module docs).
pub struct SpeakerFollower {
    pub send: Sender,
    pub clock: Clock,
    pub enabled: bool,
    pub blocked: Blocked, // the daemon's word on who else owns the head right now ("" = nobody)
    pub publish: Option<Publisher>,
    st: Mutex<Inner>,
}

impl SpeakerFollower {
    /// `SpeakerFollower(send)` with every default.
    pub fn new(send: Sender) -> Self {
        Self::with(send, FollowerOptions::default())
    }

    pub fn with(send: Sender, opts: FollowerOptions) -> Self {
        SpeakerFollower {
            send,
            clock: opts.clock,
            enabled: opts.enabled,
            blocked: opts.blocked,
            publish: opts.publish,
            st: Mutex::new(Inner {
                phase: "idle".into(),
                owner_hold_until: f64::NEG_INFINITY,
                track: None,
                commanded: None,
                last_move_at: f64::NEG_INFINITY,
                holding: false,
                search: None,
                moves: 0,
                searches: 0,
                found_by: Vec::new(),
                noted_at: f64::NEG_INFINITY,
                announced: String::new(),
                opened_at: None,
                acquired: false,
                opening: Vec::new(),
                opening_at: f64::NEG_INFINITY,
                openings: 0,
                last_seen: None,
                presence: opts.presence.unwrap_or_else(|| PresenceMap::new(None)),
            }),
        }
    }

    // -- read-outs (the Python attributes) --
    pub fn phase(&self) -> String {
        self.st.lock().phase.clone()
    }
    pub fn holding(&self) -> bool {
        self.st.lock().holding
    }
    pub fn track(&self) -> Option<Track> {
        self.st.lock().track.clone()
    }
    pub fn search(&self) -> Option<Search> {
        self.st.lock().search.clone()
    }
    pub fn commanded(&self) -> Option<(i64, i64)> {
        self.st.lock().commanded
    }
    pub fn last_move_at(&self) -> f64 {
        self.st.lock().last_move_at
    }
    pub fn moves(&self) -> usize {
        self.st.lock().moves
    }
    pub fn searches(&self) -> usize {
        self.st.lock().searches
    }
    pub fn found_by(&self) -> Vec<String> {
        self.st.lock().found_by.clone()
    }
    pub fn owner_hold_until(&self) -> f64 {
        self.st.lock().owner_hold_until
    }
    /// `_acquired`: this stretch's opening sequence has been planned (or someone was followed).
    pub fn acquired(&self) -> bool {
        self.st.lock().acquired
    }
    /// Work with the presence map (it is the follower's own, as `follower.presence` in Python).
    pub fn with_presence<R>(&self, f: impl FnOnce(&mut PresenceMap) -> R) -> R {
        f(&mut self.st.lock().presence)
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    // -- context from the daemon --
    pub fn active(&self) -> bool {
        if !self.enabled {
            return false;
        }
        let (in_phase, hold_until) = {
            let st = self.st.lock();
            (is_follow_phase(&st.phase), st.owner_hold_until)
        };
        in_phase && (self.blocked)().is_empty() && self.now() >= hold_until
    }

    /// The conversation's phase changed. Leaving a follow phase hands the head back at once.
    pub async fn on_phase(&self, phase: &str) {
        let was = self.active();
        let entering = is_follow_phase(phase) && !is_follow_phase(&self.st.lock().phase);
        // the clock is read only for a new stretch, as in Python (never under the state lock)
        let now = if entering { Some(self.now()) } else { None };
        {
            let mut st = self.st.lock();
            st.phase = phase.to_string();
            if let Some(now) = now {
                // a new stretch: nobody in view by then earns a fresh opening
                st.opened_at = Some(now);
                st.acquired = false;
            }
        }
        if was && !self.active() {
            self.release(&format!("phase {phase}")).await;
            self.st.lock().opening.clear();
        }
        if phase == "idle" {
            // the conversation is over: keep what was learned, start fresh
            let mut st = self.st.lock();
            if st.presence.save() {
                info!("follow: presence map saved ({} places)", st.presence.cells.len());
            }
            st.track = None;
            st.search = None;
            st.searches = 0;
            st.found_by.clear();
            st.announced.clear();
            st.opened_at = None;
            st.acquired = false;
            st.opening.clear();
            st.openings = 0;
            st.last_seen = None;
        }
    }

    /// The owner asked for a pose (move_head, look_around, find): it is theirs for as long as it is held.
    pub fn owner_moved(&self, hold_secs: f64) {
        let now = self.now();
        let mut st = self.st.lock();
        // Python's max(0.0, x): a NaN hold is 0
        st.owner_hold_until = now + if hold_secs > 0.0 { hold_secs } else { 0.0 };
        st.holding = false;
        st.search = None;
        st.track = None; // by the time the hold ends the track is history: start again from what is seen
    }

    /// Give the head back to the board's own behaviour now (a look with hold 0).
    pub async fn release(&self, why: &str) {
        let cmd = {
            let st = self.st.lock();
            if st.holding {
                st.commanded
            } else {
                None
            }
        };
        if let Some((y, p)) = cmd {
            (self.send)(build_look_cmd(y, p, 0)).await;
            info!("follow: let go ({})", why);
        }
        let mut st = self.st.lock();
        st.holding = false;
        st.search = None;
    }

    /// One event on the memory bus per change of state (and per find), never per frame. Returns the
    /// message to publish once the state lock is dropped.
    fn say(&self, st: &mut Inner, state: &str, extra: &[(&str, Value)]) -> Option<Map<String, Value>> {
        if self.publish.is_none() || (state == st.announced && state != "found") {
            return None;
        }
        st.announced = state.to_string();
        let mut msg = Map::new();
        msg.insert("state".into(), Value::from(state));
        msg.insert("time".into(), Value::from(wall_time()));
        msg.insert("phase".into(), Value::from(st.phase.clone()));
        for (k, v) in extra {
            msg.insert((*k).to_string(), v.clone());
        }
        if let Some(t) = &st.track {
            msg.insert("yaw".into(), Value::from(py_round_to(t.yaw, 1)));
            msg.insert("pitch".into(), Value::from(py_round_to(t.pitch, 1)));
            msg.insert("speed".into(), Value::from(py_round_to(t.speed(), 1)));
        }
        Some(msg)
    }

    fn emit(&self, msg: Option<Map<String, Value>>) {
        let (Some(msg), Some(publish)) = (msg, self.publish.as_ref()) else {
            return;
        };
        // the bus is an audience, never a dependency
        match catch_unwind(AssertUnwindSafe(|| publish(PRESENCE_TOPIC, msg))) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => debug!("follow: publish failed: {}", e),
            Err(_) => debug!("follow: publish failed (panicked)"),
        }
    }

    // -- the frames --
    /// (absolute yaw, pitch, the face) to consider in this frame: nearest the track, else the largest.
    fn pick(st: &Inner, faces: &[Face], pose_yaw: f64, pose_pitch: f64, now: f64) -> Option<(f64, f64, Face)> {
        let usable: Vec<Face> = faces.iter().copied().filter(|f| f.conf >= MIN_CONF && f.size >= MIN_SIZE).collect();
        if usable.is_empty() {
            return None;
        }
        let angles: Vec<(i64, i64)> = usable
            .iter()
            .map(|f| {
                let (y, p, _) = target_pose(pose_yaw, pose_pitch, f.bx as f64, f.by as f64);
                (y, p)
            })
            .collect();
        let mut i = 0;
        match &st.track {
            Some(t) if now - t.seen_at <= LOST_HOLD_SECS + SEARCH_STEP_SECS => {
                let (ey, ep) = Self::expected(t, now);
                let d = |k: usize| (angles[k].0 as f64 - ey).hypot(angles[k].1 as f64 - ep);
                for k in 1..usable.len() {
                    if d(k) < d(i) {
                        i = k; // min(): the first of equals
                    }
                }
            }
            _ => {
                for k in 1..usable.len() {
                    if usable[k].size > usable[i].size {
                        i = k; // max(): the first of equals
                    }
                }
            }
        }
        Some((angles[i].0 as f64, angles[i].1 as f64, usable[i]))
    }

    // Not f64::clamp: a NaN gap must read as 0, as Python's min(1.5, max(0.0, nan)) does.
    #[allow(clippy::manual_clamp)]
    fn expected(t: &Track, now: f64) -> (f64, f64) {
        let dt = (now - t.seen_at).max(0.0).min(1.5); // a velocity is only believed for a moment
        (t.yaw + t.vyaw * dt, t.pitch + t.vpitch * dt)
    }

    /// Fold one sighting into the track. True when the track may be acted on.
    fn update(st: &mut Inner, y: f64, p: f64, face: &Face, now: f64) -> bool {
        let searching = st.search.is_some();
        let Some(t) = st.track.as_mut().filter(|t| now - t.seen_at <= LOST_HOLD_SECS + 8.0 * SEARCH_STEP_SECS) else {
            st.track = Some(Track::sighted(y, p, now, 1, face));
            return false; // one sighting is a candidate, not a person
        };
        if searching && t.confirmed >= 2 {
            // While it is looking for someone it lost, the first face it finds IS the find: the track's
            // old position is where they were, not where a sighting has to agree with.
            st.track = Some(Track::sighted(y, p, now, 2, face));
            return true;
        }
        let (ey, ep) = Self::expected(t, now);
        if (y - ey).hypot(p - ep) <= AGREE_DEG {
            let dt = now - t.seen_at;
            if (VEL_MIN_DT..=2.0).contains(&dt) {
                t.vyaw += VEL_SMOOTH * ((y - t.yaw) / dt - t.vyaw);
                t.vpitch += VEL_SMOOTH * ((p - t.pitch) / dt - t.vpitch);
            }
            t.yaw += SMOOTH * (y - t.yaw);
            t.pitch += SMOOTH * (p - t.pitch);
            t.seen_at = now;
            t.confirmed += 1;
            t.doubt = None;
            t.edge = (face.bx, face.by);
            t.size = face.size;
            return t.confirmed >= 2;
        }
        if let Some((dy, dp)) = t.doubt {
            if (y - dy).hypot(p - dp) <= AGREE_DEG {
                // twice in a row somewhere else: that is where they are (or who is here) now
                st.track = Some(Track::sighted(y, p, now, 2, face));
                return true;
            }
        }
        t.doubt = Some((y, p)); // once is an outlier: a blurred frame, a poster, a reflection
        false
    }

    /// Every processed camera frame lands here, with the pose it was taken at.
    pub async fn on_faces(&self, faces: &[Face], pose_yaw: Option<f64>, pose_pitch: Option<f64>) {
        let (pose_yaw, pose_pitch) = match (self.active(), pose_yaw, pose_pitch) {
            (true, Some(y), Some(p)) => (y, p),
            _ => {
                if self.holding() && !self.active() {
                    // someone else owns the head now: hand it over at once
                    let b = (self.blocked)();
                    self.release(if b.is_empty() { "standing down" } else { &b }).await;
                }
                return;
            }
        };
        let now = self.now();
        let (next, msg) = {
            let mut st = self.st.lock();
            if now - st.last_move_at < SETTLE_SECS {
                return; // this frame was taken while the head was still swinging
            }
            if let (true, Some((cy, cp))) = (st.holding, st.commanded) {
                if (pose_yaw - cy as f64).abs() > REFUSED_DEG || (pose_pitch - cp as f64).abs() > REFUSED_DEG {
                    // The board kept the head (it refuses a host look while it wants the owner's attention,
                    // or outside a conversation, hostlook.h): the pose it echoes is the truth, not the one
                    // asked for. Building on the ask walked the head to its limits at a face that never
                    // moved (2026-09-27).
                    info!(
                        "follow: the board kept the head at yaw {:.0} pitch {:.0} (asked {}/{})",
                        pose_yaw, pose_pitch, cy, cp
                    );
                    st.holding = false;
                    st.commanded = None;
                    st.track = None;
                }
            }
            match Self::pick(&st, faces, pose_yaw, pose_pitch, now) {
                None => (Next::Lost, None),
                Some((y, p, face)) => {
                    let was_searching = st.search.clone();
                    if !Self::update(&mut st, y, p, &face, now) {
                        (Next::Nothing, None)
                    } else {
                        let t = st.track.clone().expect("update() returned true with a track");
                        st.acquired = true; // someone is followed: this stretch's opening is spent
                        st.opening.clear();
                        st.last_seen = Some((t.yaw, t.pitch));
                        let msg = if let Some(s) = was_searching {
                            let how = if s.looking.is_empty() { "they came back".to_string() } else { s.looking };
                            st.found_by.push(how.clone());
                            st.search = None;
                            info!("follow: found them again ({}) at yaw {:.0} pitch {:.0}", how, t.yaw, t.pitch);
                            self.say(&mut st, "found", &[("how", Value::from(how))])
                        } else {
                            self.say(&mut st, "seen", &[])
                        };
                        if now - st.noted_at >= 1.0 {
                            // learn where people are, about once a second
                            st.noted_at = now;
                            st.presence.note(t.yaw, t.pitch);
                        }
                        let (ay, ap) = (t.yaw + t.vyaw * LEAD_SECS, t.pitch + t.vpitch * LEAD_SECS);
                        let base = match (st.holding, st.commanded) {
                            (true, Some((cy, cp))) => (cy as f64, cp as f64),
                            _ => (pose_yaw, pose_pitch),
                        };
                        let (dy, dp) = (ay - base.0, ap - base.1);
                        let far = dy.abs() >= DEADBAND_YAW || dp.abs() >= DEADBAND_PITCH;
                        let next = if far {
                            let scale = (MAX_STEP_DEG / dy.abs().max(dp.abs())).min(1.0);
                            let want_y = clip_yaw(base.0 + dy * scale);
                            let (yy, pp, _) = clamp_pose(want_y, base.1 + dp * scale);
                            Next::Look(yy, pp, "follow", face)
                        } else if st.holding && now - st.last_move_at >= REFRESH_SECS && st.commanded.is_some() {
                            // in the deadband: renew the hold so the board's own pose cannot pull the head
                            // away mid-sentence
                            let (cy, cp) = st.commanded.unwrap_or_default();
                            Next::Renew(cy, cp)
                        } else if !st.holding {
                            // already looking at them from the board's own pose: take the head so it stays
                            // that way
                            Next::Look(py_round(pose_yaw), py_round(pose_pitch), "hold", face)
                        } else {
                            Next::Nothing
                        };
                        (next, msg)
                    }
                }
            }
        };
        self.emit(msg);
        match next {
            Next::Nothing => {}
            Next::Lost => self.lost(now, pose_yaw, pose_pitch).await,
            Next::Look(y, p, why, face) => {
                self.look(y, p, now, why, Some(face)).await;
            }
            Next::Renew(y, p) => {
                if (self.send)(build_look_cmd(y, p, (HOLD_SECS * 1000.0) as i64)).await {
                    self.st.lock().last_move_at = now - SETTLE_SECS; // a renewal moves nothing: do not blind the next frame
                }
            }
        }
    }

    async fn look(&self, yaw: i64, pitch: i64, now: f64, why: &str, face: Option<Face>) -> bool {
        if !(self.send)(build_look_cmd(yaw, pitch, (HOLD_SECS * 1000.0) as i64)).await {
            return false;
        }
        let mut st = self.st.lock();
        let moved = st.commanded != Some((yaw, pitch));
        st.commanded = Some((yaw, pitch));
        st.holding = true;
        st.last_move_at = if moved { now } else { now - SETTLE_SECS };
        if moved && why == "follow" {
            st.moves += 1;
            let (ty, tp, ts) = st.track.as_ref().map_or((0.0, 0.0, 0.0), |t| (t.yaw, t.pitch, t.speed()));
            let (bx, by) = face.map_or((0, 0), |f| (f.bx, f.by));
            info!(
                "follow: → yaw {} pitch {} (face bx={} by={}; track {:.0}/{:.0} moving {:.0}°/s)",
                yaw, pitch, bx, by, ty, tp, ts
            );
        } else if moved && why != "hold" {
            info!("follow: looking {} → yaw {} pitch {}", why, yaw, pitch);
        }
        true
    }

    // -- losing them, and working out where they went --
    /// The places to look, likeliest first (the module docs have the reasoning).
    pub fn plan_search(&self, t: &Track) -> Vec<Place> {
        let mut st = self.st.lock();
        Self::plan_search_in(&mut st.presence, t)
    }

    fn plan_search_in(presence: &mut PresenceMap, t: &Track) -> Vec<Place> {
        let mut places: Vec<Place> = Vec::new();
        let step = CAMERA_HFOV_DEG / 3.0; // 22°: what was at the frame's edge lands near its middle
        // A heading is believed only from a real exit: fast motion seen over at least three sightings, or
        // leaving by a frame edge. Two noisy boxes make a "velocity" of 8°/s out of a person sitting still,
        // and the first live search (2026-09-21) went looking up and to the right on the strength of one.
        let moving = t.confirmed >= 3 && t.speed() >= SPEED_FAST;
        let dir_y: i64 = if moving && t.vyaw.abs() >= SPEED_FAST / 2.0 {
            if t.vyaw > 0.0 {
                1
            } else {
                -1
            }
        } else if t.edge.0 >= EDGE {
            1
        } else if t.edge.0 <= -EDGE {
            -1
        } else {
            0
        };
        let dir_p: i64 = if moving && t.vpitch.abs() >= SPEED_FAST / 2.0 {
            if t.vpitch > 0.0 {
                1
            } else {
                -1
            }
        } else if t.edge.1 >= EDGE {
            -1 // +by is down: lower pitch
        } else if t.edge.1 <= -EDGE {
            1
        } else {
            0
        };
        if dir_y != 0 || dir_p != 0 {
            for k in [1.0, 2.0] {
                let (y, p, _) = clamp_pose(
                    clip_yaw(t.yaw + dir_y as f64 * step * k),
                    t.pitch + dir_p as f64 * step * 0.6 * k,
                );
                places.push((y, p, "where they were heading".into()));
            }
        }
        let mut tried: Vec<(f64, f64)> = places.iter().map(|(y, p, _)| (*y as f64, *p as f64)).collect();
        tried.push((t.yaw, t.pitch));
        for (y, p) in presence.top_excluding(USUAL_PLACES, &tried) {
            let (ry, rp) = reachable(y as f64, p as f64);
            places.push((ry, rp, "where they usually are".into()));
        }
        let heading = py_round(clip_yaw(t.yaw));
        let from = tried.len() - 1;
        let usual: Vec<(f64, f64)> = places[from.min(places.len())..].iter().map(|(y, p, _)| (*y as f64, *p as f64)).collect();
        tried.extend(usual);
        let bin = PresenceMap::BIN as f64;
        for side in [-1.0, 1.0] {
            // a sideways shift is the commonest way to leave a frame
            let (y, p, _) = clamp_pose(clip_yaw(t.yaw + side * step), t.pitch);
            if !tried.iter().any(|&(ty, tp)| (y as f64 - ty).abs() <= bin && (p as f64 - tp).abs() <= bin) {
                places.push((y, p, "either side".into()));
            }
        }
        for pitch in [62i64, 30] {
            // they stood up; they sat back
            if (pitch as f64 - t.pitch).abs() > DEADBAND_PITCH {
                places.push((heading, pitch, "up and down".into()));
            }
        }
        places.truncate(MAX_PLACES);
        places
    }

    fn plan_opening(st: &mut Inner, pose_yaw: f64, pose_pitch: f64) -> Vec<Place> {
        let mut places: Vec<Place> = Vec::new();
        if let Some((ly, lp)) = st.last_seen {
            let (y, p, _) = clamp_pose(ly, lp);
            places.push((y, p, "where they were".into()));
        }
        let usual = st.presence.top(1);
        if let Some(&(uy, up)) = usual.first() {
            let (y, p) = reachable(uy as f64, up as f64);
            places.push((y, p, "where they usually are".into()));
            let (y, p) = reachable(uy as f64, (up - LOWER_DEG) as f64);
            places.push((y, p, "a little lower".into()));
        }
        if places.is_empty() {
            // Nothing learned yet (a first conversation, a cleared map): scan from where the head is,
            // downward first — a desk robot's person is more often below its level gaze than above it.
            for (k, why) in [(-1i64, "lower"), (-2, "lower still"), (1, "higher")] {
                let (y, p, _) = clamp_pose(pose_yaw, pose_pitch + (k * LOWER_DEG) as f64);
                places.push((y, p, why.into()));
            }
        }
        let mut kept: Vec<Place> = Vec::new();
        for (y, p, why) in places {
            // never the pose it already has, never the same place twice
            if (y as f64 - pose_yaw).abs() < DEADBAND_YAW && (p as f64 - pose_pitch).abs() < DEADBAND_PITCH {
                continue;
            }
            if kept.iter().any(|(ky, kp, _)| (((y - ky).abs() as f64) < DEADBAND_YAW) && (((p - kp).abs() as f64) < DEADBAND_PITCH)) {
                continue;
            }
            kept.push((y, p, why));
        }
        kept
    }

    async fn lost(&self, now: f64, pose_yaw: f64, pose_pitch: f64) {
        enum Then {
            Done,
            Look(i64, i64, String),
            GiveUp(f64),
            Exhausted,
        }
        let (then, msg) = {
            let mut st = self.st.lock();
            let followed = st.track.as_ref().is_some_and(|t| t.confirmed >= 2);
            if !followed {
                // Nobody is being followed, so nothing was lost — but a stretch of conversation that opens on
                // an empty frame gets a short opening: where they were last, where they usually are, a little
                // lower.
                if let (false, Some(opened_at), true) = (st.acquired, st.opened_at, st.search.is_none()) {
                    if now - opened_at >= ACQUIRE_AFTER_SECS {
                        st.acquired = true;
                        if st.openings < MAX_OPENINGS {
                            st.opening = Self::plan_opening(&mut st, pose_yaw, pose_pitch);
                            if !st.opening.is_empty() {
                                st.openings += 1;
                            }
                        }
                    }
                }
                // one stray box is no find
                let quiet = st.track.as_ref().is_none_or(|t| now - t.seen_at >= SEARCH_STEP_SECS);
                if !st.opening.is_empty() && quiet && now - st.opening_at >= SEARCH_STEP_SECS {
                    let (y, p, why) = st.opening.remove(0);
                    st.opening_at = now;
                    (Then::Look(y, p, format!("{why}, to start with")), None)
                } else {
                    (Then::Done, None)
                }
            } else {
                let t = st.track.clone().expect("checked above");
                let gone = now - t.seen_at;
                if st.search.is_none() && gone > STALE_TRACK_SECS + LOST_HOLD_SECS {
                    // The follower was standing down (an asked-for pose, thinking, working) while this track
                    // aged. Its velocity and its frame edge describe a moment long gone (live, 2026-09-21: a
                    // search "where they were heading" launched off a 17-second-old track). Forget it; follow
                    // what is seen.
                    st.track = None;
                    return;
                }
                let mut msg = None;
                let mut early: Option<Then> = None;
                if st.search.is_none() {
                    let leaving = (t.confirmed >= 3 && t.speed() >= SPEED_FAST) || t.edge.0.abs() >= EDGE || t.edge.1.abs() >= EDGE;
                    if gone < (if leaving { LEAVING_HOLD_SECS } else { LOST_HOLD_SECS }) {
                        return;
                    }
                    if st.searches >= MAX_SEARCHES {
                        early = Some(if st.holding { Then::GiveUp(gone) } else { Then::Done });
                    } else {
                        st.searches += 1;
                        let places = Self::plan_search_in(&mut st.presence, &t);
                        st.search = Some(Search::new(places, now));
                        let how = if t.speed() >= SPEED_FAST {
                            format!("moving {:.0}°/s", t.speed())
                        } else if leaving {
                            "left by the frame edge".to_string()
                        } else {
                            "they were still".to_string()
                        };
                        let mut whys: Vec<&str> = Vec::new();
                        for (_, _, w) in &st.search.as_ref().expect("just set").places {
                            if !whys.contains(&w.as_str()) {
                                whys.push(w.as_str());
                            }
                        }
                        info!("follow: lost them {:.1} s ago ({}) — looking: {}", gone, how, whys.join(", then "));
                        msg = self.say(&mut st, "lost", &[("leaving", Value::from(leaving))]);
                    }
                }
                match early {
                    Some(e) => (e, None),
                    None => {
                        let s = st.search.as_mut().expect("a search is running");
                        if now - s.step_at < SEARCH_STEP_SECS {
                            (Then::Done, msg)
                        } else if s.places.is_empty() {
                            (Then::Exhausted, msg)
                        } else {
                            let (y, p, why) = s.places.remove(0);
                            s.step_at = now;
                            s.looking = why.clone();
                            s.tried.push((y as f64, p as f64));
                            (Then::Look(y, p, why), msg)
                        }
                    }
                }
            }
        };
        self.emit(msg);
        match then {
            Then::Done => {}
            Then::Look(y, p, why) => {
                self.look(y, p, now, &why, None).await;
            }
            Then::GiveUp(gone) => {
                self.release(&format!("no face for {gone:.1} s")).await;
                let msg = {
                    let mut st = self.st.lock();
                    self.say(&mut st, "lost", &[])
                };
                self.emit(msg);
            }
            Then::Exhausted => {
                self.release("looked where it could think of, nobody there").await;
                self.st.lock().track = None; // one search per loss: nothing more until someone is SEEN again
            }
        }
    }
}

// -- configuration --

fn env_lookup(environ: Option<&HashMap<String, String>>, key: &str) -> Option<String> {
    match environ {
        Some(m) => m.get(key).cloned(),
        None => std::env::var_os(key).map(|v| v.to_string_lossy().into_owned()),
    }
}

/// `EVE_FOLLOW_SPEAKER` (default on). `None` reads the process environment.
pub fn configured(environ: Option<&HashMap<String, String>>) -> bool {
    let v = env_lookup(environ, "EVE_FOLLOW_SPEAKER").filter(|s| !s.is_empty()).unwrap_or_else(|| "1".into());
    !matches!(v.trim().to_lowercase().as_str(), "0" | "false" | "no" | "off")
}

/// `EVE_PRESENCE_FILE`, else `~/.config/eve-bridge/presence.json`, with `~` expanded. `None` reads the
/// process environment.
pub fn presence_path(environ: Option<&HashMap<String, String>>) -> PathBuf {
    let raw = env_lookup(environ, "EVE_PRESENCE_FILE").filter(|s| !s.is_empty()).unwrap_or_else(|| DEFAULT_PRESENCE_FILE.into());
    expanduser(&raw, std::env::var_os("HOME"))
}

/// `Path(p).expanduser()`: `~` from `$HOME` (else the password database), `~user` from the password
/// database. A home that cannot be found leaves the path as it was (pathlib would raise).
fn expanduser(path: &str, home: Option<OsString>) -> PathBuf {
    let p = PathBuf::from(path);
    if !path.starts_with('~') {
        return p;
    }
    let i = path.find('/').unwrap_or(path.len());
    let name = &path[1..i];
    let userhome: Option<String> = if name.is_empty() {
        match home {
            Some(h) => Some(h.to_string_lossy().into_owned()),
            None => pw_home(None),
        }
    } else {
        pw_home(Some(name))
    };
    let Some(userhome) = userhome else {
        return p;
    };
    let trimmed = userhome.trim_end_matches('/');
    let joined = format!("{trimmed}{}", &path[i..]);
    // pathlib normalises away a doubled slash; posixpath gives "/" for an empty result
    PathBuf::from(if joined.is_empty() { "/".to_string() } else { joined }).components().collect()
}

#[cfg(unix)]
fn pw_home(name: Option<&str>) -> Option<String> {
    let mut buf = vec![0 as libc::c_char; 16384];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let rc = match name {
        Some(n) => {
            let c = CString::new(n).ok()?;
            // SAFETY: every pointer is valid for the call and buf outlives the read of pw_dir below.
            unsafe { libc::getpwnam_r(c.as_ptr(), &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result) }
        }
        // SAFETY: as above.
        None => unsafe { libc::getpwuid_r(libc::getuid(), &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result) },
    };
    if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
        return None;
    }
    // SAFETY: pw_dir points into buf, NUL-terminated by getpw*_r.
    Some(unsafe { CStr::from_ptr(pwd.pw_dir) }.to_string_lossy().into_owned())
}

#[cfg(not(unix))]
fn pw_home(_name: Option<&str>) -> Option<String> {
    None
}

#[cfg(test)]
mod tests;
