//! Eve's diary: memory-aware, non-generic thoughts about the room.
//!
//! Port of `eve_bridge/diary.py`.
//!
//! The old note was a caption ("a desk with a monitor") — one sentence per
//! changed frame, no memory. This module gives the explorer a memory and a
//! voice, following the literature (docs/stackchan/personality.md § diary):
//!
//! - A memory stream (Park et al. 2023, Generative Agents): one JSONL record
//!   per frame with the thought, concrete observations, what changed, tags,
//!   novelty and importance, whether it was written, and when it was last
//!   retrieved. Retrieval for the next note scores recency x importance x
//!   relevance (tag overlap; no vector DB for a few hundred entries) and adds
//!   the last three written notes plus two notes from the same hour on earlier
//!   days ("third day in a row the chair is pushed in at three").
//! - A profile (Letta/MemGPT core block): ROOM (persistent objects and their
//!   usual state), HUMAN (habits with evidence), SELF (persona, running jokes,
//!   open questions), RULES (what to ignore). Rewritten by a nightly reflection.
//! - Non-generic thoughts (Verbalized Sampling; Kapur 2026 contrast sets): the
//!   model returns three candidate thoughts with probabilities; we take the
//!   least typical one that is specific (a token outside the baseline
//!   vocabulary) and not a near-repeat of the last ten.
//! - A write gate (Kang 2026 selective memory; Park's importance): write iff
//!   novelty >= 5 or importance >= 7 or the diary has been silent for 3 h.
//!   Unwritten candidates stay in memory and feed retrieval.
//! - Dreams (Park's reflection + Mem0 consolidation, shaped like a debrief
//!   system's day pass): once a day, or when the summed importance of new records
//!   passes 150, a text-only call rewrites the profile and appends two or three
//!   insights under "## Dreams" in the day's file, plus up to two "★ (candidate)"
//!   lines — things eve proposes it should never forget.
//! - Stars (the human's layer): `highlights.md` holds what the human starred from
//!   the diary window. It is append-only, never pruned, and always in the prompt
//!   context. eve never stars anything itself; it only proposes candidates.
//!
//! The layers, borrowed from the debrief memory system: memory.jsonl = episodes
//! (raw, immutable); YYYY-MM-DD.md = the day's diary (episodic, immutable);
//! profile.md = the semantic layer (rewired, never appended); highlights.md =
//! the starred layer (human-promoted, forever).
//!
//! The same call returns an appraisal — valence, arousal, a label — which the
//! daemon forwards to the board as `{"cmd":"emote"}` so what eve sees colours
//! how it feels (mood.cpp clamps the nudge to +-0.3).
//!
//! The dated Markdown diary keeps its line format, so the desktop widget shows
//! the thoughts unchanged.

mod prompts;
pub mod py;
#[cfg(test)]
mod tests;

use std::borrow::Borrow;
use std::collections::{BTreeSet, HashMap};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, LazyLock, Mutex as StdMutex};
use std::time::Duration;

use base64::Engine as _;
use chrono::{DateTime, Datelike, FixedOffset, Local, LocalResult, NaiveDateTime, TimeZone, Timelike};
use futures::future::BoxFuture;
use indexmap::IndexMap;
use parking_lot::{Mutex, MutexGuard};
use regex::Regex;
use serde_json::{json, Map, Value};

use eve_explore::{
    append_note, data_url, error_name, error_text, frame_image, normalise, output_text, Clock, Note, Responses,
    RuntimeError, Wall, ERROR_LOG_INTERVAL_SECS, NOTE_TIMEOUT_SECS,
};
use eve_photos::PhotoConfig;
use eve_spend::Ledger;
use eve_vision::{Frame, ValueError};

pub use eve_thought_screen::tag_overlap as jaccard;
pub use prompts::{ASK_FOR_JSON, DEFAULT_PROFILE, EXAMINE_PROMPT, REFLECT_PROMPT, SYSTEM_PROMPT};
use py::PyError;

pub const MEMORY_FILE: &str = "memory.jsonl";
pub const PROFILE_FILE: &str = "profile.md";
pub const HIGHLIGHTS_FILE: &str = "highlights.md";
pub const DREAMS_HEADER: &str = "## Dreams";
/// Park et al.
pub const RECENCY_DECAY_PER_HOUR: f64 = 0.995;
pub const RETRIEVE_TOP: usize = 5;
pub const SAME_HOUR_SAMPLES: usize = 2;
pub const WRITE_NOVELTY: i64 = 5;
pub const WRITE_IMPORTANCE: i64 = 7;
pub const SILENCE_WRITE_HOURS: f64 = 3.0;

// ---- the cool factor (photos) -------------------------------------------------------------------
// What makes eve keep a picture rather than only a sentence. The weights and
// cut-offs below are explained, with their sources, in
// docs/stackchan/personality.md § "Photos: what eve finds cool".
pub const COOL_WEIGHTS: [(&str, f64); 5] = [
    ("novelty", 0.30),    // unlike anything in the diary (absolute novelty)
    ("importance", 0.20), // eve would tell its human about it
    ("surprise", 0.20),   // unlike what THIS waypoint usually looks like
    ("presence", 0.15),   // a person, an animal, or something of eve's that moved
    ("want", 0.15),       // eve's own vote — bounded, never the whole decision
];
/// Arousal multiplies the score, it never adds to it.
pub const AROUSAL_GAIN: f64 = 0.5;
/// Novelty this high, sharing at most one tag with memory, ...
pub const FIRST_TIME_NOVELTY: i64 = 9;
/// ... is a first: floored so it clears any threshold.
pub const FIRST_TIME_SHARED_TAGS: usize = 1;
pub const FIRST_TIME_FLOOR: f64 = 0.70;

// Quality: the frames a photo would be wasted on.
/// Darker than this and there is nothing in it.
pub const QUALITY_MIN_MEAN: f64 = 24.0;
/// Blown out.
pub const QUALITY_MAX_MEAN: f64 = 232.0;
/// A flat wall, a lens against a surface.
pub const QUALITY_MIN_STD: f64 = 10.0;
pub const DULL_SUBJECTS: [&str; 2] = ["nothing", "screen"];

// Habituation.
/// How far back "have I photographed this?" looks.
pub const ALBUM_DAYS: f64 = 14.0;
/// Mean abs luma diff below this is the same picture again ...
pub const DUPLICATE_DIFF: f64 = 10.0;
/// ... unless it is this important, which supersedes the old one.
pub const DUPLICATE_OVERRIDE: i64 = 9;
/// Jaccard over tags: the same subject.
pub const SUBJECT_OVERLAP: f64 = 0.5;
/// One photo here drops this spot's interest by this much.
pub const WAYPOINT_DROP: f64 = 0.45;
pub const WAYPOINT_RECOVERY_HOURS: f64 = 8.0;
pub const WAYPOINT_FLOOR: f64 = 0.05;

// Threshold and budget.
/// Until there is a distribution to measure.
pub const COOL_THRESHOLD: f64 = 0.55;
pub const COOL_THRESHOLD_MIN: f64 = 0.40;
pub const COOL_THRESHOLD_MAX: f64 = 0.75;
pub const COOL_MAD_GAIN: f64 = 1.5;
/// Records the adaptive threshold is measured over.
pub const COOL_WINDOW: usize = 40;
pub const PHOTOS_PER_HOUR: usize = 2;
pub const PHOTOS_PER_DAY: usize = 8;
pub const CAPTION_CHARS: usize = 60;
/// Photos shown to the model so it knows what it already has.
pub const ALBUM_IN_CONTEXT: usize = 8;
/// Tags the human starred are worth this much extra want.
pub const TASTE_BONUS: f64 = 0.10;
/// Near-duplicate of a recent thought.
pub const REPEAT_JACCARD: f64 = 0.6;
/// Park's reflection trigger ...
pub const REFLECT_IMPORTANCE_SUM: i64 = 150;
/// ... or once a day from this hour.
pub const REFLECT_HOUR: u32 = 21;
pub const DEFAULT_REFLECTION_MODEL: &str = "gpt-6-luna";
/// Letta-style hard limit per block.
pub const PROFILE_BLOCK_CHARS: usize = 2000;
pub const THINK_MAX_OUTPUT_TOKENS: i64 = 700;
/// Reasoning counts against this cap, and it varies: the same photo and prompt
/// spent 128 reasoning tokens on one call and 256 on the next (bench
/// 2026-09-06), against 500. A short draw truncated the JSON mid-string and the
/// closer look was thrown away for "no JSON object in the reply". The answer
/// itself is about 150 tokens, so this is headroom for the thinking, not for a
/// longer reply.
pub const EXAMINE_MAX_OUTPUT_TOKENS: i64 = 1200;
/// Medium reasoning and the rewritten profile share this cap.
pub const REFLECT_MAX_OUTPUT_TOKENS: i64 = 8192;

// ---- local time, as Python's naive datetimes use it ----------------------------------------------

/// `when.timestamp()` for a naive local datetime. An ambiguous time (the hour
/// the clocks go back) is its first occurrence and a time in the spring-forward
/// gap takes the offset in force before the jump, as Python's `fold=0` does.
pub fn timestamp(when: &NaiveDateTime) -> f64 {
    let dt: DateTime<Local> = match Local.from_local_datetime(when) {
        LocalResult::Single(d) => d,
        LocalResult::Ambiguous(a, _) => a,
        LocalResult::None => {
            let before = *when - chrono::Duration::hours(3);
            let off = Local
                .offset_from_local_datetime(&before)
                .earliest()
                .unwrap_or_else(|| FixedOffset::east_opt(0).expect("zero offset"));
            match off.from_local_datetime(when) {
                LocalResult::Single(d) => d.with_timezone(&Local),
                _ => Local::now(),
            }
        }
    };
    dt.timestamp() as f64 + f64::from(dt.timestamp_subsec_micros()) / 1e6
}

/// `datetime.fromtimestamp(ts)`: the naive local time of a unix timestamp.
pub fn local(ts: f64) -> NaiveDateTime {
    let micros = (ts * 1e6).round_ties_even();
    DateTime::from_timestamp_micros(micros as i64)
        .map(|d| d.with_timezone(&Local).naive_local())
        .unwrap_or_default()
}

fn day_of(when: &NaiveDateTime) -> String {
    when.format("%Y-%m-%d").to_string()
}

fn day_of_ts(ts: f64) -> String {
    day_of(&local(ts))
}

fn now_local() -> NaiveDateTime {
    Local::now().naive_local()
}

// ---- records -----------------------------------------------------------------------------------

/// One memory record: a line of `memory.jsonl`.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub id: i64,
    /// unix seconds
    pub ts: f64,
    pub weekday: i64,
    pub hour: i64,
    pub yaw: i64,
    pub pitch: i64,
    pub thought: String,
    pub observations: Vec<String>,
    pub changed: Vec<String>,
    pub tags: Vec<String>,
    pub novelty: i64,
    pub importance: i64,
    pub written: bool,
    pub valence: i64,
    pub arousal: i64,
    pub label: String,
    pub last_accessed: f64,
    /// Photos. `cool` is stored on every record, photographed or not, because
    /// the adaptive threshold is measured from the distribution of all of them.
    pub cool: f64,
    pub surprise: f64,
    /// notes-dir-relative path, "" when none was kept
    pub photo: String,
    /// up to 60 characters, eve's words for the picture
    pub caption: String,
    /// base64 of the 32x24 luma, photo records only
    pub thumb: String,
    /// this photo was replaced by that record's
    pub superseded_by: i64,
}

impl Default for Record {
    fn default() -> Self {
        Record {
            id: 0,
            ts: 0.0,
            weekday: 0,
            hour: 0,
            yaw: 0,
            pitch: 0,
            thought: String::new(),
            observations: vec![],
            changed: vec![],
            tags: vec![],
            novelty: 5,
            importance: 3,
            written: false,
            valence: 0,
            arousal: 0,
            label: "calm".into(),
            last_accessed: 0.0,
            cool: 0.0,
            surprise: 0.0,
            photo: String::new(),
            caption: String::new(),
            thumb: String::new(),
            superseded_by: 0,
        }
    }
}

fn json_list(items: &[String]) -> String {
    format!("[{}]", items.iter().map(|s| py::json_str(s)).collect::<Vec<_>>().join(", "))
}

fn field_int(d: &Map<String, Value>, key: &str, default: Option<i64>) -> Result<i64, PyError> {
    match d.get(key) {
        None | Some(Value::Null) => default.ok_or_else(|| PyError::TypeError(format!("missing field {key:?}"))),
        Some(Value::Bool(b)) => Ok(i64::from(*b)),
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                return Ok(i);
            }
            match n.as_f64() {
                Some(f) if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.2e18 => Ok(f as i64),
                _ => Err(PyError::TypeError(format!("field {key:?} is not an integer"))),
            }
        }
        Some(_) => Err(PyError::TypeError(format!("field {key:?} is not an integer"))),
    }
}

fn field_float(d: &Map<String, Value>, key: &str, default: Option<f64>) -> Result<f64, PyError> {
    match d.get(key) {
        None | Some(Value::Null) => default.ok_or_else(|| PyError::TypeError(format!("missing field {key:?}"))),
        Some(Value::Bool(b)) => Ok(if *b { 1.0 } else { 0.0 }),
        Some(Value::Number(n)) => Ok(n.as_f64().unwrap_or(0.0)),
        Some(_) => Err(PyError::TypeError(format!("field {key:?} is not a number"))),
    }
}

fn field_str(d: &Map<String, Value>, key: &str, default: Option<&str>) -> Result<String, PyError> {
    match d.get(key) {
        None | Some(Value::Null) => {
            default.map(str::to_string).ok_or_else(|| PyError::TypeError(format!("missing field {key:?}")))
        }
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(PyError::TypeError(format!("field {key:?} is not a string"))),
    }
}

fn field_list(d: &Map<String, Value>, key: &str) -> Result<Vec<String>, PyError> {
    match d.get(key) {
        None | Some(Value::Null) => Ok(vec![]),
        Some(Value::Array(a)) => Ok(a.iter().map(py::str_of).collect()),
        Some(_) => Err(PyError::TypeError(format!("field {key:?} is not a list"))),
    }
}

impl Record {
    /// `json.dumps(self.__dict__, ensure_ascii=False)`: the dataclass field
    /// order, Python's `", "` / `": "` separators and float reprs.
    pub fn to_json(&self) -> String {
        let fields: [(&str, String); 23] = [
            ("id", self.id.to_string()),
            ("ts", json_float(self.ts)),
            ("weekday", self.weekday.to_string()),
            ("hour", self.hour.to_string()),
            ("yaw", self.yaw.to_string()),
            ("pitch", self.pitch.to_string()),
            ("thought", py::json_str(&self.thought)),
            ("observations", json_list(&self.observations)),
            ("changed", json_list(&self.changed)),
            ("tags", json_list(&self.tags)),
            ("novelty", self.novelty.to_string()),
            ("importance", self.importance.to_string()),
            ("written", if self.written { "true".into() } else { "false".into() }),
            ("valence", self.valence.to_string()),
            ("arousal", self.arousal.to_string()),
            ("label", py::json_str(&self.label)),
            ("last_accessed", json_float(self.last_accessed)),
            ("cool", json_float(self.cool)),
            ("surprise", json_float(self.surprise)),
            ("photo", py::json_str(&self.photo)),
            ("caption", py::json_str(&self.caption)),
            ("thumb", py::json_str(&self.thumb)),
            ("superseded_by", self.superseded_by.to_string()),
        ];
        let body: Vec<String> = fields.iter().map(|(k, v)| format!("\"{k}\": {v}")).collect();
        format!("{{{}}}", body.join(", "))
    }

    /// `Record.from_json(line)`: unknown keys are ignored, missing ones take
    /// the dataclass defaults; a line that is not JSON is a ValueError, and a
    /// line missing a required field (or of the wrong shape) a TypeError.
    pub fn from_json(line: &str) -> Result<Record, PyError> {
        let v: Value = serde_json::from_str(line).map_err(|e| PyError::ValueError(e.to_string()))?;
        let Value::Object(d) = v else {
            return Err(PyError::TypeError("a memory line is not an object".into()));
        };
        let dflt = Record::default();
        Ok(Record {
            id: field_int(&d, "id", None)?,
            ts: field_float(&d, "ts", None)?,
            weekday: field_int(&d, "weekday", None)?,
            hour: field_int(&d, "hour", None)?,
            yaw: field_int(&d, "yaw", None)?,
            pitch: field_int(&d, "pitch", None)?,
            thought: field_str(&d, "thought", None)?,
            observations: field_list(&d, "observations")?,
            changed: field_list(&d, "changed")?,
            tags: field_list(&d, "tags")?,
            novelty: field_int(&d, "novelty", Some(dflt.novelty))?,
            importance: field_int(&d, "importance", Some(dflt.importance))?,
            written: d.get("written").is_some_and(py::truthy),
            valence: field_int(&d, "valence", Some(0))?,
            arousal: field_int(&d, "arousal", Some(0))?,
            label: field_str(&d, "label", Some("calm"))?,
            last_accessed: field_float(&d, "last_accessed", Some(0.0))?,
            cool: field_float(&d, "cool", Some(0.0))?,
            surprise: field_float(&d, "surprise", Some(0.0))?,
            photo: field_str(&d, "photo", Some(""))?,
            caption: field_str(&d, "caption", Some(""))?,
            thumb: field_str(&d, "thumb", Some(""))?,
            superseded_by: field_int(&d, "superseded_by", Some(0))?,
        })
    }
}

/// A float as `json.dumps` writes it (`NaN` / `Infinity` included, as Python allows).
fn json_float(x: f64) -> String {
    eve_spend::py_float_repr(x)
}

/// What eve just thought, handed to whoever wants to show it.
#[derive(Debug, Clone, PartialEq)]
pub struct Thought {
    pub text: String,
    pub tags: Vec<String>,
    /// the diary kept it, rather than only the memory
    pub written: bool,
    pub photographed: bool,
    pub cool: f64,
    pub novelty: i64,
    pub importance: i64,
}

/// The board-side nudge from one appraisal. dv/da are -100..100 ints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Emote {
    pub dv: i64,
    pub da: i64,
    pub label: String,
}

impl Emote {
    pub fn new(dv: i64, da: i64, label: impl Into<String>) -> Self {
        Emote { dv, da, label: label.into() }
    }
}

pub fn build_emote_cmd(e: &Emote) -> Value {
    let label = if e.label.is_empty() { "calm" } else { e.label.as_str() };
    json!({"cmd": "emote", "dv": e.dv.clamp(-100, 100), "da": e.da.clamp(-100, 100), "label": py::head(label, 11)})
}

// ---- the memory ----------------------------------------------------------------------------------

static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-z][a-z'-]+").expect("word regex"));

pub fn words(text: &str) -> BTreeSet<String> {
    WORD.find_iter(&py::lower(text)).map(|m| m.as_str().to_string()).collect()
}

/// `Path.read_text(encoding="utf-8")`: strict UTF-8 and universal newlines
/// (`\r\n` and `\r` read as `\n`).
fn read_utf8(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    let text = String::from_utf8(bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(if text.contains('\r') { text.replace("\r\n", "\n").replace('\r', "\n") } else { text })
}

/// The memory stream (JSONL) and the profile (Markdown) on disk.
pub struct Memory {
    pub notes_dir: PathBuf,
    pub wall: Wall,
    pub records: Vec<Record>,
    pub profile: String,
    pub last_reflection_day: Option<String>,
    pub importance_since_reflection: i64,
    loaded: bool,
}

impl std::fmt::Debug for Memory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Memory")
            .field("notes_dir", &self.notes_dir)
            .field("records", &self.records.len())
            .field("last_reflection_day", &self.last_reflection_day)
            .field("importance_since_reflection", &self.importance_since_reflection)
            .finish()
    }
}

impl Memory {
    pub fn new(notes_dir: impl Into<PathBuf>) -> Self {
        Memory {
            notes_dir: notes_dir.into(),
            wall: Arc::new(now_local),
            records: vec![],
            profile: DEFAULT_PROFILE.to_string(),
            last_reflection_day: None,
            importance_since_reflection: 0,
            loaded: false,
        }
    }

    pub fn with_wall(mut self, wall: Wall) -> Self {
        self.wall = wall;
        self
    }

    // -- persistence --
    pub fn memory_path(&self) -> PathBuf {
        self.notes_dir.join(MEMORY_FILE)
    }

    pub fn profile_path(&self) -> PathBuf {
        self.notes_dir.join(PROFILE_FILE)
    }

    pub fn highlights_path(&self) -> PathBuf {
        self.notes_dir.join(HIGHLIGHTS_FILE)
    }

    /// `★ claim` lines (optionally as `- ★ claim`), newest last. Written by the human
    /// through the widget's Star button; never by eve.
    pub fn load_highlights(&self) -> std::io::Result<Vec<String>> {
        let mut out = vec![];
        let path = self.highlights_path();
        if path.exists() {
            for line in py::splitlines(&read_utf8(&path)?) {
                let t = py::strip(py::strip(line).trim_start_matches('-'));
                if let Some(rest) = t.strip_prefix('★') {
                    let rest = py::strip(rest);
                    if !rest.is_empty() {
                        out.push(rest.to_string());
                    }
                }
            }
        }
        Ok(out)
    }

    pub fn load(&mut self) -> anyhow::Result<()> {
        if self.loaded {
            return Ok(());
        }
        self.loaded = true;
        let path = self.memory_path();
        if path.exists() {
            for line in py::splitlines(&read_utf8(&path)?) {
                let line = py::strip(line);
                if line.is_empty() {
                    continue;
                }
                match Record::from_json(line) {
                    Ok(r) => self.records.push(r),
                    Err(_) => tracing::warn!("diary: skipping a malformed memory line"),
                }
            }
        }
        let profile = self.profile_path();
        if profile.exists() {
            let text = read_utf8(&profile)?;
            let text = py::strip(&text);
            if !text.is_empty() {
                self.profile = text.to_string();
            }
        }
        let meta = self.notes_dir.join("reflection.json");
        if meta.exists() {
            if let Ok(text) = read_utf8(&meta) {
                if let Ok(d) = serde_json::from_str::<Value>(&text) {
                    let Value::Object(d) = d else {
                        anyhow::bail!("'{}' object has no attribute 'get'", py_type_of(&d));
                    };
                    self.last_reflection_day = match d.get("day") {
                        Some(Value::String(s)) => Some(s.clone()),
                        Some(Value::Null) | None => None,
                        Some(other) => Some(py::str_of(other)),
                    };
                    if let Some(n) = meta_int(d.get("importance_since")) {
                        self.importance_since_reflection = n;
                    }
                }
            }
        }
        Ok(())
    }

    fn ensure_dir(&self) -> std::io::Result<()> {
        if !self.notes_dir.exists() {
            std::fs::create_dir_all(&self.notes_dir)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&self.notes_dir, std::fs::Permissions::from_mode(0o700));
            }
        }
        Ok(())
    }

    pub fn add(&mut self, rec: Record) -> std::io::Result<()> {
        self.ensure_dir()?;
        let line = rec.to_json() + "\n";
        let importance = rec.importance;
        self.records.push(rec);
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(self.memory_path())?;
        f.write_all(line.as_bytes())?;
        self.importance_since_reflection += importance;
        self.save_meta();
        Ok(())
    }

    /// Persist every record (after last_accessed / importance updates).
    pub fn rewrite(&self) -> std::io::Result<()> {
        self.ensure_dir()?;
        let tmp = self.notes_dir.join("memory.tmp");
        let body: String = self.records.iter().map(|r| r.to_json() + "\n").collect();
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, self.memory_path())
    }

    pub fn save_profile(&mut self, text: &str) -> std::io::Result<()> {
        self.ensure_dir()?;
        let t = py::strip(text);
        self.profile = if t.is_empty() { DEFAULT_PROFILE.to_string() } else { t.to_string() };
        std::fs::write(self.profile_path(), format!("{}\n", self.profile))
    }

    /// `reflection.json`; a write that fails is ignored.
    pub fn save_meta(&self) {
        let day = match &self.last_reflection_day {
            Some(d) => py::json_str_ascii(d),
            None => "null".into(),
        };
        let body = format!("{{\"day\": {day}, \"importance_since\": {}}}", self.importance_since_reflection);
        let _ = std::fs::write(self.notes_dir.join("reflection.json"), body);
    }

    // -- queries --
    pub fn next_id(&self) -> i64 {
        self.records.last().map(|r| r.id + 1).unwrap_or(1)
    }

    pub fn written(&self) -> Vec<&Record> {
        self.records.iter().filter(|r| r.written).collect()
    }

    pub fn last_written_ts(&self) -> Option<f64> {
        self.records.iter().rev().find(|r| r.written).map(|r| r.ts)
    }

    /// Park scoring: recency + importance + relevance, min-max normalised.
    /// The chosen records are marked as retrieved now.
    pub fn retrieve(
        &mut self,
        tags: &BTreeSet<String>,
        now_ts: f64,
        top: usize,
        exclude: Option<&BTreeSet<i64>>,
    ) -> Vec<Record> {
        let pool: Vec<usize> = (0..self.records.len())
            .filter(|&i| exclude.is_none_or(|ex| ex.is_empty() || !ex.contains(&self.records[i].id)))
            .collect();
        if pool.is_empty() {
            return vec![];
        }
        let rec: Vec<f64> = pool
            .iter()
            .map(|&i| {
                let r = &self.records[i];
                let since = if r.last_accessed != 0.0 { r.last_accessed } else { r.ts };
                RECENCY_DECAY_PER_HOUR.powf(py::max(0.0, (now_ts - since) / 3600.0))
            })
            .collect();
        let imp: Vec<f64> = pool.iter().map(|&i| self.records[i].importance as f64 / 10.0).collect();
        let rel: Vec<f64> = pool
            .iter()
            .map(|&i| {
                let r = &self.records[i];
                let mut theirs: BTreeSet<String> = r.tags.iter().cloned().collect();
                theirs.extend(words(&r.thought));
                jaccard(tags, &theirs)
            })
            .collect();
        fn norm(xs: &[f64]) -> Vec<f64> {
            let lo = xs.iter().copied().fold(f64::INFINITY, py::min);
            let hi = xs.iter().copied().fold(f64::NEG_INFINITY, py::max);
            xs.iter().map(|x| if hi > lo { (x - lo) / (hi - lo) } else { 0.5 }).collect()
        }
        let (a, b, c) = (norm(&rec), norm(&imp), norm(&rel));
        let mut ranked: Vec<(f64, usize)> = (0..pool.len()).map(|k| (a[k] + b[k] + c[k], pool[k])).collect();
        // sorted(..., reverse=True) is stable: equal scores keep their order.
        ranked.sort_by(|x, y| y.0.total_cmp(&x.0));
        ranked.truncate(top);
        ranked
            .into_iter()
            .map(|(_, i)| {
                self.records[i].last_accessed = now_ts;
                self.records[i].clone()
            })
            .collect()
    }

    pub fn same_hour(&self, when: &NaiveDateTime, n: usize) -> Vec<&Record> {
        let today = day_of(when);
        let mut out: Vec<&Record> = self
            .records
            .iter()
            .rev()
            .filter(|r| r.written && r.hour == i64::from(when.hour()) && day_of_ts(r.ts) != today)
            .take(n)
            .collect();
        out.reverse();
        out
    }

    pub fn unwritten_today(&self, when: &NaiveDateTime) -> Vec<&Record> {
        let today = day_of(when);
        self.records.iter().filter(|r| !r.written && day_of_ts(r.ts) == today).collect()
    }

    pub fn today(&self, when: &NaiveDateTime) -> Vec<&Record> {
        let today = day_of(when);
        self.records.iter().filter(|r| day_of_ts(r.ts) == today).collect()
    }

    /// Words the profile and the last ten written thoughts already use.
    pub fn baseline_vocabulary(&self) -> BTreeSet<String> {
        let mut vocab = words(&self.profile);
        let written = self.written();
        for r in &written[written.len().saturating_sub(10)..] {
            vocab.extend(words(&r.thought));
        }
        vocab
    }

    /// MemoryBank-style: unretrieved records lose importance, 2 % per day.
    pub fn decay(&mut self, now_ts: f64) -> std::io::Result<()> {
        let mut changed = false;
        for r in &mut self.records {
            let since = if r.last_accessed != 0.0 { r.last_accessed } else { r.ts };
            let days = (now_ts - since) / 86400.0;
            if days >= 1.0 && r.importance > 1 {
                let scaled = (r.importance as f64 * 0.98_f64.powf(days)).round_ties_even();
                let lowered = (scaled as i64).max(1);
                // Restart the clock only when the whole number drops. Restarting it every pass meant a daily
                // pass never lost anything: round(7 * 0.98) is 7 again.
                if lowered < r.importance {
                    r.importance = lowered;
                    r.last_accessed = now_ts;
                    changed = true;
                }
            }
        }
        if changed {
            self.rewrite()?;
        }
        Ok(())
    }
}

fn py_type_of(v: &Value) -> &'static str {
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

/// `int(d.get("importance_since", 0))`, None where Python's int() raises.
fn meta_int(v: Option<&Value>) -> Option<i64> {
    match v {
        None => Some(0),
        Some(Value::Bool(b)) => Some(i64::from(*b)),
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f.trunc() as i64)),
        Some(Value::String(s)) => {
            let t = py::strip(s);
            let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
            // int() takes single underscores between digits only ("1_2", not "_12", "12_" or "1__2").
            if !digits.is_empty() && digits.split('_').all(|g| !g.is_empty() && g.chars().all(|c| c.is_ascii_digit())) {
                t.replace('_', "").parse().ok()
            } else {
                None
            }
        }
        Some(_) => None,
    }
}

// ---- the prompt context and the pick ---------------------------------------------------------------

/// `_fmt`: one record in the context.
fn fmt_record(rec: &Record) -> String {
    let when = local(rec.ts);
    let mut s = format!("[{}] {} yaw={:+} pitch={} — {}", rec.id, when.format("%a %H:%M"), rec.yaw, rec.pitch, rec.thought);
    if !rec.changed.is_empty() {
        s.push_str(&format!(" (changed: {})", rec.changed.join("; ")));
    }
    s
}

/// `_fmt_photo`: one album entry in the context.
fn fmt_photo(rec: &Record) -> String {
    let when = local(rec.ts);
    let caption = if rec.caption.is_empty() { &rec.thought } else { &rec.caption };
    format!("[{}] {} yaw={:+} — {}", rec.id, when.format("%a %H:%M"), rec.yaw, caption)
}

fn lines_or(lines: Vec<String>, empty: &str) -> String {
    let joined = lines.join("\n");
    if joined.is_empty() {
        empty.to_string()
    } else {
        joined
    }
}

fn tail<T>(v: &[T], n: usize) -> &[T] {
    &v[v.len().saturating_sub(n)..]
}

/// The text the vision call gets alongside the photo (~2k tokens).
pub fn build_context(
    memory: &mut Memory,
    when: &NaiveDateTime,
    yaw: i64,
    pitch: i64,
    guess_tags: &BTreeSet<String>,
) -> std::io::Result<String> {
    let now_ts = timestamp(when);
    let last3: Vec<Record> = tail(&memory.written(), 3).iter().map(|r| (*r).clone()).collect();
    let exclude: BTreeSet<i64> = last3.iter().map(|r| r.id).collect();
    let older = memory.retrieve(guess_tags, now_ts, RETRIEVE_TOP, Some(&exclude));
    let same: Vec<String> = memory.same_hour(when, SAME_HOUR_SAMPLES).into_iter().map(fmt_record).collect();
    let unwritten: Vec<String> =
        tail(&memory.unwritten_today(when), 6).iter().map(|r| format!("- {}", r.thought)).collect();
    let stars = memory.load_highlights()?;
    let shelf = album(&memory.records, now_ts);
    let parts = [
        format!("PROFILE:\n{}", py::strip(&memory.profile)),
        format!(
            "STARRED BY MY HUMAN (never forget, never contradict):\n{}",
            lines_or(tail(&stars, 12).iter().map(|h| format!("★ {h}")).collect(), "(nothing starred yet)")
        ),
        format!("LAST NOTES:\n{}", lines_or(last3.iter().map(fmt_record).collect(), "(none yet)")),
        format!("OLDER MEMORIES:\n{}", lines_or(older.iter().map(fmt_record).collect(), "(none)")),
        format!("SAME HOUR ON EARLIER DAYS ({}:00):\n{}", when.format("%H"), lines_or(same, "(none)")),
        format!("TODAY'S UNWRITTEN CANDIDATES (do not repeat):\n{}", lines_or(unwritten, "(none)")),
        format!(
            "MY ALBUM (pictures I already keep — do not ask for one of these again unless it changed):\n{}",
            lines_or(tail(&shelf, ALBUM_IN_CONTEXT).iter().map(|r| fmt_photo(r)).collect(), "(no photos yet)")
        ),
        format!(
            "NOW: {}, head yaw={:+} pitch={} (a low pitch looks down at the desk, a high one up at the room).",
            when.format("%A %H:%M"),
            yaw,
            pitch
        ),
        ASK_FOR_JSON.to_string(),
    ];
    Ok(parts.join("\n\n"))
}

/// The least typical candidate that is specific and not a near-repeat.
/// Errs where Python's `float(p)` raises (a `p` that is not a number).
pub fn pick_thought(
    candidates: &[Value],
    recent: &[String],
    baseline_vocab: &BTreeSet<String>,
) -> Result<Option<String>, PyError> {
    let recent_sets: Vec<BTreeSet<String>> = recent.iter().map(|t| words(t)).collect();
    let mut ranked: Vec<(f64, String)> = vec![];
    for c in candidates {
        let Value::Object(m) = c else { continue };
        let text = m.get("text").map(py::str_of).unwrap_or_default();
        if py::strip(&text).is_empty() {
            continue;
        }
        let p = match m.get("p") {
            Some(v) if py::truthy(v) => py::float(v)?,
            _ => 0.5,
        };
        ranked.push((if p == 0.0 { 0.0 } else { p }, text));
    }
    ranked.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut fallback: Option<String> = None;
    for (_, raw) in ranked {
        let text = py::collapse(&raw);
        let ws = words(&text);
        if recent_sets.iter().any(|r| jaccard(&ws, r) > REPEAT_JACCARD) {
            continue;
        }
        if fallback.is_none() {
            fallback = Some(text.clone());
        }
        if ws.difference(baseline_vocab).next().is_some() {
            // says something the baseline does not
            return Ok(Some(text));
        }
    }
    Ok(fallback)
}

static FENCE_OPEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^```[a-zA-Z]*\n?").expect("fence regex"));
static FENCE_CLOSE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n?```$").expect("fence regex"));

/// Tolerant JSON: strip fences, find the outermost object. Errs with a
/// `ValueError` (no object, not an object) or a `serde_json::Error` (Python's
/// `JSONDecodeError`).
pub fn parse_reply(text: &str) -> anyhow::Result<Map<String, Value>> {
    let mut t = py::strip(text).to_string();
    if t.starts_with("```") {
        t = FENCE_OPEN.replace(&t, "").into_owned();
        t = FENCE_CLOSE.replace(&t, "").into_owned();
    }
    let (Some(start), Some(end)) = (t.find('{'), t.rfind('}')) else {
        return Err(ValueError("no JSON object in the reply".into()).into());
    };
    let slice = if end >= start { &t[start..=end] } else { "" };
    let d: Value = serde_json::from_str(slice)?;
    match d {
        Value::Object(m) => Ok(m),
        _ => Err(ValueError("reply is not an object".into()).into()),
    }
}

/// `_int`: a number field clamped to `lo..=hi`, `default` when it is not a
/// number. An infinite value escapes as Python's OverflowError.
fn int_field(d: &Map<String, Value>, key: &str, default: i64, lo: i64, hi: i64) -> Result<i64, PyError> {
    let x = match d.get(key) {
        None => default as f64,
        Some(v) => match py::float(v) {
            Ok(x) => x,
            Err(PyError::TypeError(_) | PyError::ValueError(_)) => return Ok(default),
            Err(e) => return Err(e),
        },
    };
    let r = match py::round_int(x) {
        Ok(r) => r,
        Err(PyError::ValueError(_)) => return Ok(default),
        Err(e) => return Err(e),
    };
    Ok(if r > hi as f64 {
        hi
    } else if r < lo as f64 {
        lo
    } else {
        r as i64
    })
}

pub fn should_write(novelty: i64, importance: i64, last_written_ts: Option<f64>, now_ts: f64) -> bool {
    if novelty >= WRITE_NOVELTY || importance >= WRITE_IMPORTANCE {
        return true;
    }
    match last_written_ts {
        None => true,
        Some(ts) => (now_ts - ts) >= SILENCE_WRITE_HOURS * 3600.0,
    }
}

// ---- the cool factor: is this worth a picture? ---------------------------------------------------

/// `_clip(v, 0, 1)`.
fn clip(v: f64) -> f64 {
    clip_to(v, 0.0, 1.0)
}

fn clip_to(v: f64, lo: f64, hi: f64) -> f64 {
    py::max(lo, py::min(hi, v))
}

pub fn median(xs: &[f64]) -> f64 {
    let mut s = xs.to_vec();
    s.sort_by(f64::total_cmp);
    let n = s.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    }
}

/// Median absolute deviation: a spread that a couple of odd values cannot move.
pub fn mad(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let m = median(xs);
    median(&xs.iter().map(|x| (x - m).abs()).collect::<Vec<_>>())
}

/// False for a frame there is no point keeping: too dark to read, blown
/// out, or so flat it is a wall. Roughly two in five frames from a camera
/// nobody is aiming are unusable this way (Doherty et al., CIVR 2008), and a
/// picture of nothing is worse than no picture.
pub fn thumb_quality(thumb: Option<&[u8]>) -> bool {
    let Some(thumb) = thumb.filter(|t| !t.is_empty()) else {
        return false;
    };
    let n = thumb.len() as f64;
    let mean = thumb.iter().map(|&v| u64::from(v)).sum::<u64>() as f64 / n;
    if mean < QUALITY_MIN_MEAN || mean > QUALITY_MAX_MEAN {
        return false;
    }
    let var = thumb.iter().map(|&v| (f64::from(v) - mean).powi(2)).sum::<f64>() / n;
    var.sqrt() >= QUALITY_MIN_STD
}

/// The model's novelty rating, ranked against its own recent ratings.
///
/// A model that only ever answers 4 to 6 still produces the full range here,
/// because the spread it is measured against shrinks with it. Clipping at
/// zero rather than allowing negatives keeps an ordinary frame from
/// subtracting from the other evidence.
pub fn novelty_component(novelty: i64, recent: &[i64]) -> f64 {
    let (med, spread) = if recent.len() < 20 {
        (5.0, 1.0)
    } else {
        let xs: Vec<f64> = recent.iter().map(|&v| v as f64).collect();
        (median(&xs), py::max(1.0, 1.4826 * mad(&xs)))
    };
    clip((novelty as f64 - med) / spread / 3.0)
}

/// People and animals first; then eve's own things having moved.
pub fn presence_component(subject: &str, changed: &[String]) -> f64 {
    if subject == "person" || subject == "animal" {
        return 1.0;
    }
    if changed.is_empty() {
        0.0
    } else {
        0.4
    }
}

/// eve's own taste, learned from its human rather than shipped with it.
///
/// Every claim the human starred is read as a small standing vote for the
/// words in it: a tag that appears in a starred line adds `TASTE_BONUS` to
/// what eve wanted, capped so taste tilts the score without deciding it.
/// This is the one part of the gate that changes with eve's life instead of
/// being fixed at build time.
pub fn taste_bonus(tags: &[String], highlights: &[String]) -> f64 {
    if tags.is_empty() || highlights.is_empty() {
        return 0.0;
    }
    let mut starred: BTreeSet<String> = BTreeSet::new();
    for line in highlights {
        starred.extend(words(line));
    }
    let hits = tags.iter().filter(|t| starred.contains(t.as_str())).count();
    py::min(2.0 * TASTE_BONUS, hits as f64 * TASTE_BONUS)
}

/// How much eve wants to keep this picture, in 0..1, with its parts.
///
/// Two channels open the gate and either can do it on its own: the view is
/// unlike anything in memory (novelty), or unlike what this spot usually
/// looks like (surprise). What eve feels about it multiplies the result
/// rather than adding to it, so a strong feeling makes an interesting view
/// much more keepable and a dull one only slightly. A frame that is too dark,
/// blown out, flat, or that the model itself called nothing scores zero
/// whatever else is true.
#[allow(clippy::too_many_arguments)]
pub fn cool_factor(
    novelty: i64,
    importance: i64,
    arousal: i64,
    surprise: Option<f64>,
    subject: &str,
    changed: &[String],
    want: f64,
    recent_novelty: &[i64],
    thumb: Option<&[u8]>,
    observations: &[String],
) -> (f64, IndexMap<&'static str, f64>) {
    let mut parts: IndexMap<&'static str, f64> = IndexMap::new();
    parts.insert("novelty", novelty_component(novelty, recent_novelty));
    parts.insert("importance", clip((importance - 4) as f64 / 5.0));
    parts.insert("surprise", surprise.map(|s| clip((s - 1.0) / 3.0)).unwrap_or(0.5));
    parts.insert("presence", presence_component(subject, changed));
    parts.insert("want", clip(want));
    let mut raw = 0.0;
    for (k, w) in COOL_WEIGHTS {
        raw += w * parts[k];
    }
    let mut cool = raw * (1.0 + AROUSAL_GAIN * arousal.abs() as f64 / 100.0);
    if !thumb_quality(thumb) || DULL_SUBJECTS.contains(&subject) || observations.is_empty() {
        cool = 0.0;
    }
    parts.insert("arousal", arousal.abs() as f64 / 100.0);
    (py::round_to(cool, 3), parts)
}

/// The photos eve still has in mind: every photographed record inside the
/// window (`ALBUM_DAYS`), not superseded.
pub fn album<R: Borrow<Record>>(records: &[R], now_ts: f64) -> Vec<&Record> {
    album_within(records, now_ts, ALBUM_DAYS)
}

/// `album(records, now_ts, days)`.
pub fn album_within<R: Borrow<Record>>(records: &[R], now_ts: f64, days: f64) -> Vec<&Record> {
    let horizon = now_ts - days * 86400.0;
    records
        .iter()
        .map(Borrow::borrow)
        .filter(|r| !r.photo.is_empty() && r.ts >= horizon && r.superseded_by == 0)
        .collect()
}

/// How much of the score survives the fact that eve has been here before.
///
/// Three layers, all read back out of the memory file, so restarting the
/// daemon cannot make eve forget what it already photographed:
///
/// * the same picture again is refused outright, unless it is important
///   enough to supersede the one already kept (the returned record);
/// * each earlier photo of the same subject divides the score by a growing
///   root, so a second picture of the cat is worth about seven tenths of the
///   first and a fifth about four tenths;
/// * each recent photo at this waypoint dulls the spot for a few hours,
///   which is what keeps a burst of pictures from coming out of one corner.
pub fn habituation<'a, R: Borrow<Record>>(
    album_records: &'a [R],
    tags: &[String],
    yaw: i64,
    pitch: i64,
    thumb: Option<&[u8]>,
    now_ts: f64,
    importance: i64,
) -> (f64, Option<&'a Record>) {
    let here: Vec<&Record> =
        album_records.iter().map(Borrow::borrow).filter(|r| (r.yaw, r.pitch) == (yaw, pitch)).collect();
    let mut duplicate: Option<&Record> = None;
    if let Some(thumb) = thumb {
        let cur = normalise(thumb);
        for r in &here {
            if let Some(other) = thumb_bytes(r) {
                if other.len() == cur.len() && mean_abs(&cur, &normalise(&other)) < DUPLICATE_DIFF {
                    duplicate = Some(r);
                    break;
                }
            }
        }
    }
    if let Some(dup) = duplicate {
        if importance < DUPLICATE_OVERRIDE {
            return (0.0, None);
        }
        return (1.0, Some(dup)); // keep the new one, retire the old
    }
    let tagset: BTreeSet<String> = tags.iter().cloned().collect();
    let same_subject = album_records
        .iter()
        .map(Borrow::borrow)
        .filter(|r| {
            !tagset.is_empty() && jaccard(&tagset, &r.tags.iter().cloned().collect()) >= SUBJECT_OVERLAP
        })
        .count();
    let mut spent = 0.0;
    for r in &here {
        spent += WAYPOINT_DROP * (-(now_ts - r.ts) / (WAYPOINT_RECOVERY_HOURS * 3600.0)).exp();
    }
    let spot = py::max(WAYPOINT_FLOOR, 1.0 - spent);
    (spot / ((1 + same_subject) as f64).sqrt(), None)
}

/// `base64.b64decode(..., validate=True)`: the standard alphabet with its padding, trailing bits not checked.
const B64_LENIENT: base64::engine::GeneralPurpose = base64::engine::GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    base64::engine::GeneralPurposeConfig::new().with_decode_allow_trailing_bits(true),
);

/// `_thumb_bytes`: a record's stored thumbnail, or None.
fn thumb_bytes(rec: &Record) -> Option<Vec<u8>> {
    if rec.thumb.is_empty() {
        return None;
    }
    B64_LENIENT.decode(rec.thumb.as_bytes()).ok()
}

/// `_mean_abs`: the two thumbnails have the same length.
fn mean_abs(a: &[u8], b: &[u8]) -> f64 {
    let total: u64 = a.iter().zip(b).map(|(&x, &y)| u64::from(x.abs_diff(y))).sum();
    total as f64 / a.len() as f64
}

/// How cool a view has to be today. The bar is the middle of what eve has
/// been seeing plus a margin, so a lively week makes it pickier and a dull one
/// does not make it desperate; the clamp keeps it from drifting anywhere silly.
pub fn cool_threshold(recent_cools: &[f64]) -> f64 {
    if recent_cools.len() < 20 {
        return COOL_THRESHOLD;
    }
    let t = median(recent_cools) + COOL_MAD_GAIN * 1.4826 * mad(recent_cools);
    py::round_to(py::max(COOL_THRESHOLD_MIN, py::min(COOL_THRESHOLD_MAX, t)), 3)
}

/// At most two in any hour and eight in a day, counted from the records
/// themselves so a restart cannot reset the allowance.
pub fn photo_budget_left<R: Borrow<Record>>(album_records: &[R], when: &NaiveDateTime) -> bool {
    let now_ts = timestamp(when);
    let shelf: Vec<&Record> = album_records.iter().map(Borrow::borrow).collect();
    let last_hour = shelf.iter().filter(|r| now_ts - r.ts < 3600.0).count();
    let today = day_of(when);
    let same_day = shelf.iter().filter(|r| day_of_ts(r.ts) == today).count();
    last_hour < PHOTOS_PER_HOUR && same_day < PHOTOS_PER_DAY
}

// ---- the network seam ----------------------------------------------------------------------------

/// The three model calls. Blocking; each returns the JSON reply text or errs.
pub trait DiaryClient: Send + Sync {
    /// The JSON reply text for one photo + context.
    fn think(&self, image: &[u8], mime: &str, context: &str) -> anyhow::Result<String>;
    /// The JSON reply text for a text-only reflection.
    fn reflect(&self, prompt: &str) -> anyhow::Result<String>;
    /// A proper look at a full-size photo eve kept.
    fn examine(&self, image: &[u8], mime: &str, context: &str) -> anyhow::Result<String>;
}

/// The OpenRouter blocking client, built on first use: reqwest's blocking
/// client panics when it is built on an async runtime thread, and the calls
/// run on the diary's worker thread. The key is checked up front.
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
        Ok(client.responses_create(body)?)
    }
}

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
                Some(Value::String(s)) => py::repr_str(s),
                _ => "None".into(),
            };
            format!("IncompleteDetails(reason={reason})")
        }
        Some(other) => other.to_string(),
    }
}

/// Responses API; one low-detail image per thought, JSON object output.
///
/// `examine` is the exception: a photo eve decided to keep is worth
/// looking at properly, so that one call sends the full-size frame at high
/// detail. Low detail collapses a picture into about 85 tokens, which is
/// enough to say "a room" and not enough to say "a sewing machine" — which is
/// exactly what happened on the bench, 2026-09-06.
pub struct OpenAIDiaryClient {
    pub model: String,
    pub reflection_model: String,
    client: Box<dyn Responses>,
    ledger: Ledger,
}

impl OpenAIDiaryClient {
    /// The OpenRouter client with `timeout` seconds and no retries, reflecting
    /// on `DEFAULT_REFLECTION_MODEL`. Errs with `NoKey` when no key is found.
    pub fn new(model: &str, api_key: Option<&str>, timeout: f64) -> Result<Self, eve_openrouter::Error> {
        let limit = if timeout.is_finite() {
            Duration::try_from_secs_f64(timeout.max(0.0)).unwrap_or(Duration::ZERO)
        } else {
            Duration::MAX
        };
        let opts = eve_openrouter::ClientOptions::default().timeout(limit).max_retries(0);
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
        OpenAIDiaryClient {
            model: model.to_string(),
            reflection_model: DEFAULT_REFLECTION_MODEL.to_string(),
            client,
            ledger: Ledger::default(),
        }
    }

    /// The `reflection_model=` keyword.
    pub fn with_reflection_model(mut self, model: &str) -> Self {
        self.reflection_model = model.to_string();
        self
    }

    /// Meter into this ledger instead of the configured spend folder.
    pub fn with_ledger(mut self, ledger: Ledger) -> Self {
        self.ledger = ledger;
        self
    }

    fn look(&self, instructions: &str, image: &[u8], mime: &str, context: &str, detail: &str, cap: i64) -> anyhow::Result<String> {
        let resp = self.client.create(json!({
            "model": self.model,
            "instructions": instructions,
            "input": [{"role": "user", "content": [
                {"type": "input_text", "text": context},
                {"type": "input_image", "image_url": data_url(image, mime), "detail": detail},
            ]}],
            "text": {"format": {"type": "json_object"}},
            "max_output_tokens": cap,
            "reasoning": {"effort": "low"},
        }))?;
        self.ledger.record_response(eve_spend::EXPLORING, &resp, &self.model, eve_openrouter::PROVIDER, None);
        let text = py::strip(&output_text(&resp)).to_string();
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

impl DiaryClient for OpenAIDiaryClient {
    fn think(&self, image: &[u8], mime: &str, context: &str) -> anyhow::Result<String> {
        self.look(SYSTEM_PROMPT, image, mime, context, "low", THINK_MAX_OUTPUT_TOKENS)
    }

    fn examine(&self, image: &[u8], mime: &str, context: &str) -> anyhow::Result<String> {
        self.look(EXAMINE_PROMPT, image, mime, context, "high", EXAMINE_MAX_OUTPUT_TOKENS)
    }

    fn reflect(&self, prompt: &str) -> anyhow::Result<String> {
        let resp = self.client.create(json!({
            "model": self.reflection_model,
            "instructions": REFLECT_PROMPT,
            "input": prompt,
            "text": {"format": {"type": "json_object"}},
            "max_output_tokens": REFLECT_MAX_OUTPUT_TOKENS,
            "reasoning": {"effort": "medium"},
        }))?;
        self.ledger.record_response(eve_spend::EXPLORING, &resp, &self.reflection_model, eve_openrouter::PROVIDER, None);
        let text = py::strip(&output_text(&resp)).to_string();
        if text.is_empty() {
            return Err(RuntimeError("empty reflection".into()).into());
        }
        Ok(text)
    }
}

// ---- the async side ------------------------------------------------------------------------------

/// A diary call was cancelled: `stop()` dropped it before it ran (Python's
/// `CancelledError` out of the cancelled executor future).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("diary call cancelled")]
pub struct DiaryCancelled;

pub type SendEmote = Arc<dyn Fn(Emote) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;
pub type Snapshot = Arc<dyn Fn() -> BoxFuture<'static, anyhow::Result<Option<Frame>>> + Send + Sync>;
pub type OnThought = Arc<dyn Fn(&Thought) + Send + Sync>;
pub type OnWritten = Arc<dyn Fn(&Record) + Send + Sync>;

type Job = Box<dyn FnOnce() + Send>;

/// The work queue the worker thread drains, and whether it was shut down.
#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    shut: bool,
}

/// The single-worker executor: one thread named `diary_0` running calls in
/// order. Shutting it down (`shutdown(wait=False, cancel_futures=True)`) lets
/// the running call finish and cancels the queued ones at once: each dropped
/// job drops its answer channel, so whoever awaits it sees the cancellation
/// immediately, as Python's cancelled futures do.
struct Executor {
    shared: Arc<(StdMutex<Queue>, Condvar)>,
}

impl Executor {
    fn spawn() -> std::io::Result<Self> {
        let shared: Arc<(StdMutex<Queue>, Condvar)> = Arc::default();
        let worker = shared.clone();
        std::thread::Builder::new().name("diary_0".into()).spawn(move || {
            let (lock, ready) = &*worker;
            loop {
                let job = {
                    let mut q = lock.lock().unwrap_or_else(|p| p.into_inner());
                    loop {
                        if let Some(job) = q.jobs.pop_front() {
                            break job;
                        }
                        if q.shut {
                            return;
                        }
                        q = ready.wait(q).unwrap_or_else(|p| p.into_inner());
                    }
                };
                job();
            }
        })?;
        Ok(Executor { shared })
    }

    /// Queue a job; false when the executor was already shut down.
    fn submit(&self, job: Job) -> Result<(), Job> {
        let (lock, ready) = &*self.shared;
        let mut q = lock.lock().unwrap_or_else(|p| p.into_inner());
        if q.shut {
            return Err(job);
        }
        q.jobs.push_back(job);
        ready.notify_one();
        Ok(())
    }

    fn shutdown(self) {
        let (lock, ready) = &*self.shared;
        let dropped: VecDeque<Job> = {
            let mut q = lock.lock().unwrap_or_else(|p| p.into_inner());
            q.shut = true;
            std::mem::take(&mut q.jobs)
        };
        // Dropped outside the lock: each one cancels its awaiting caller.
        drop(dropped);
        ready.notify_all();
    }
}

enum Failure {
    TimedOut,
    Cancelled,
    Raised(anyhow::Error),
}

impl Failure {
    fn name(&self) -> String {
        match self {
            Failure::TimedOut => "TimeoutError".into(),
            Failure::Cancelled => "CancelledError".into(),
            Failure::Raised(e) => raised_name(e),
        }
    }
}

fn raised_name(e: &anyhow::Error) -> String {
    if let Some(p) = e.downcast_ref::<PyError>() {
        return p.name().into();
    }
    error_name(e).into()
}

fn panic_message(p: Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".into())
}

fn str_or_empty(v: Option<&Value>) -> String {
    match v {
        Some(v) if py::truthy(v) => py::str_of(v),
        _ => String::new(),
    }
}

/// `[str(x)[:n] for x in (v or []) if str(x).strip()]`, after `map`.
fn str_items(v: Option<&Value>, map: impl Fn(&str) -> String) -> Result<Vec<String>, PyError> {
    Ok(py::iter_or_empty(v)?
        .iter()
        .map(py::str_of)
        .filter(|s| !py::strip(s).is_empty())
        .map(|s| map(&s))
        .collect())
}

/// Turns a Note action into a memory record, maybe a diary line, and an
/// Emote for the board. Drop-in for `eve_explore::NoteTaker` (same `take` shape).
pub struct DiaryTaker {
    pub client: Arc<dyn DiaryClient>,
    memory: Mutex<Memory>,
    /// Only the records the diary KEPT (written to the day file), the moment they are: the
    /// daemon puts them on its memory bus. Unwritten candidates stay private to the memory.
    pub on_written: Option<OnWritten>,
    pub notes_dir: PathBuf,
    pub send_emote: Option<SendEmote>,
    /// Asks the board for one full-resolution frame. None (or a None answer)
    /// means eve keeps the small streamed frame instead — a slightly worse
    /// picture is better than no picture.
    pub snapshot: Option<Snapshot>,
    pub photo_config: PhotoConfig,
    /// Every thought eve has, so the daemon can decide whether to put it
    /// on the robot's own screen (thought_screen). The diary does not
    /// decide that: what is worth remembering and what is worth showing a
    /// human are different questions.
    pub on_thought: Option<OnThought>,
    pub timeout: f64,
    clock: Clock,
    wall: Wall,
    taken: AtomicU64,
    written: AtomicU64,
    failed: AtomicU64,
    photographed: AtomicU64,
    examined: AtomicU64,
    reflections: AtomicU64,
    err_logged_at: Mutex<f64>,
    executor: Mutex<Option<Executor>>,
    reflecting: AtomicBool,
}

impl DiaryTaker {
    /// Defaults: `NOTE_TIMEOUT_SECS`, the monotonic clock, local wall time,
    /// the photo caps from the environment, and no callbacks.
    pub fn new(client: Arc<dyn DiaryClient>, notes_dir: impl Into<PathBuf>) -> Self {
        let notes_dir = notes_dir.into();
        let wall: Wall = Arc::new(now_local);
        DiaryTaker {
            client,
            memory: Mutex::new(Memory::new(notes_dir.clone()).with_wall(wall.clone())),
            on_written: None,
            notes_dir,
            send_emote: None,
            snapshot: None,
            photo_config: eve_photos::configured(None),
            on_thought: None,
            timeout: NOTE_TIMEOUT_SECS,
            clock: Arc::new(eve_vision::monotonic),
            wall,
            taken: AtomicU64::new(0),
            written: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            photographed: AtomicU64::new(0),
            examined: AtomicU64::new(0),
            reflections: AtomicU64::new(0),
            err_logged_at: Mutex::new(f64::NEG_INFINITY),
            executor: Mutex::new(None),
            reflecting: AtomicBool::new(false),
        }
    }

    pub fn with_send_emote(mut self, f: SendEmote) -> Self {
        self.send_emote = Some(f);
        self
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
        self.memory.get_mut().wall = wall.clone();
        self.wall = wall;
        self
    }

    pub fn with_snapshot(mut self, snapshot: Snapshot) -> Self {
        self.snapshot = Some(snapshot);
        self
    }

    pub fn with_photo_config(mut self, config: PhotoConfig) -> Self {
        self.photo_config = config;
        self
    }

    pub fn with_on_thought(mut self, f: OnThought) -> Self {
        self.on_thought = Some(f);
        self
    }

    pub fn with_on_written(mut self, f: OnWritten) -> Self {
        self.on_written = Some(f);
        self
    }

    /// The memory (records, profile, reflection bookkeeping), locked.
    pub fn memory(&self) -> MutexGuard<'_, Memory> {
        self.memory.lock()
    }

    /// records added
    pub fn taken(&self) -> u64 {
        self.taken.load(Ordering::SeqCst)
    }

    /// diary lines written
    pub fn written(&self) -> u64 {
        self.written.load(Ordering::SeqCst)
    }

    pub fn failed(&self) -> u64 {
        self.failed.load(Ordering::SeqCst)
    }

    /// pictures kept
    pub fn photographed(&self) -> u64 {
        self.photographed.load(Ordering::SeqCst)
    }

    /// ... and looked at properly afterwards
    pub fn examined(&self) -> u64 {
        self.examined.load(Ordering::SeqCst)
    }

    pub fn reflections(&self) -> u64 {
        self.reflections.load(Ordering::SeqCst)
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
        if let Err(job) = ex.submit(job) {
            // Only reachable if a shut executor were still installed: start a fresh one.
            let fresh = Executor::spawn()?;
            let _ = fresh.submit(job);
            *guard = Some(fresh);
        }
        Ok(())
    }

    /// Run one blocking client call on the worker, with `timeout` seconds.
    async fn call<F>(&self, timeout: f64, f: F) -> Result<String, Failure>
    where
        F: FnOnce(&dyn DiaryClient) -> anyhow::Result<String> + Send + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel::<anyhow::Result<String>>();
        let client = self.client.clone();
        let job: Job = Box::new(move || {
            // The caller stopped waiting (it timed out) before the worker reached this call: a
            // cancelled future that never started, which ThreadPoolExecutor skips rather than runs.
            if tx.is_closed() {
                return;
            }
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&*client)))
                .unwrap_or_else(|p| Err(RuntimeError(panic_message(p)).into()));
            let _ = tx.send(result);
        });
        if let Err(e) = self.submit(job) {
            return Err(Failure::Raised(e.into()));
        }
        let received = if timeout.is_infinite() && timeout > 0.0 {
            Ok(rx.await)
        } else {
            let limit = Duration::try_from_secs_f64(timeout.max(0.0)).unwrap_or(Duration::ZERO);
            tokio::time::timeout(limit, rx).await
        };
        match received {
            Err(_) => Err(Failure::TimedOut),
            Ok(Err(_dropped)) => Err(Failure::Cancelled),
            Ok(Ok(Err(e))) => Err(Failure::Raised(e)),
            Ok(Ok(Ok(text))) => Ok(text),
        }
    }

    /// One client call and `parse_reply` of its answer.
    async fn ask<F>(&self, timeout: f64, f: F) -> Result<Map<String, Value>, Failure>
    where
        F: FnOnce(&dyn DiaryClient) -> anyhow::Result<String> + Send + 'static,
    {
        let raw = self.call(timeout, f).await?;
        parse_reply(&raw).map_err(Failure::Raised)
    }

    /// Think about the note's frame: a memory record, maybe a diary line
    /// (its path), an emote, and maybe a dreams pass in the background.
    /// `Ok(None)` is a skipped note or one kept only in memory; `Err` is what
    /// Python lets escape `take` (a frame that cannot be encoded, a file that
    /// cannot be written, a reply shaped so that Python's built-ins raise, or
    /// [`DiaryCancelled`] after `stop()`).
    pub async fn take(self: &Arc<Self>, note: &Note) -> anyhow::Result<Option<PathBuf>> {
        let (when, now_ts, image, mime, context) = {
            let mut m = self.memory.lock();
            m.load()?;
            let when = (self.wall)();
            let now_ts = timestamp(&when);
            m.decay(now_ts)?;
            // The pan stays cheap: eve thinks about the small streamed frame it
            // already has. Recognising things is not this call's job — a photo it
            // decides to keep gets a proper look afterwards (see look_closer).
            let (image, mime) = frame_image(&note.frame)?;
            // A first cheap guess at relevance for retrieval: the last note's tags.
            let guess: BTreeSet<String> =
                m.written().last().map(|r| r.tags.iter().cloned().collect()).unwrap_or_default();
            let context = build_context(&mut m, &when, note.yaw, note.pitch, &guess)?;
            (when, now_ts, image, mime, context)
        };
        let reply = match self.ask(self.timeout, move |c| c.think(&image, mime, &context)).await {
            Ok(r) => r,
            Err(Failure::Cancelled) => return Err(DiaryCancelled.into()),
            Err(f) => {
                self.failed.fetch_add(1, Ordering::SeqCst);
                self.log_error(&f);
                return Ok(None);
            }
        };
        let rec = {
            let m = self.memory.lock();
            self.record(&m, &reply, &when, note.yaw, note.pitch)?
        };
        let Some(mut rec) = rec else {
            self.failed.fetch_add(1, Ordering::SeqCst);
            self.log_error(&Failure::Raised(RuntimeError("no usable thought in the reply".into()).into()));
            return Ok(None);
        };
        let keep_photo = self.consider_photo(&mut rec, note, &reply, &when).await?;
        let write = keep_photo || {
            let m = self.memory.lock();
            should_write(rec.novelty, rec.importance, m.last_written_ts(), now_ts)
        };
        rec.written = write;
        self.memory.lock().add(rec.clone())?;
        self.taken.fetch_add(1, Ordering::SeqCst);
        let mut path: Option<PathBuf> = None;
        if write {
            let extra = if rec.photo.is_empty() { None } else { Some(eve_photos::photo_line(&rec.photo)) };
            let p = append_note(&self.notes_dir, &when, note.yaw, note.pitch, &rec.thought, extra.as_deref())?;
            self.written.fetch_add(1, Ordering::SeqCst);
            tracing::info!(
                "diary: {} (novelty {}, importance {}) -> {}",
                rec.label,
                rec.novelty,
                rec.importance,
                p.display()
            );
            if let Some(cb) = &self.on_written {
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cb(&rec))).is_err() {
                    tracing::error!("diary: on_written failed");
                }
            }
            path = Some(p);
        } else {
            tracing::info!(
                "diary: kept in memory, not written (novelty {}, importance {}): {}",
                rec.novelty,
                rec.importance,
                rec.thought
            );
        }
        if let Some(cb) = &self.on_thought {
            let thought = Thought {
                text: rec.thought.clone(),
                tags: rec.tags.clone(),
                written: rec.written,
                photographed: !rec.photo.is_empty(),
                cool: rec.cool,
                novelty: rec.novelty,
                importance: rec.importance,
            };
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cb(&thought))).is_err() {
                tracing::error!("diary: on_thought failed");
            }
        }
        if let Some(send) = &self.send_emote {
            if let Err(e) = send(Emote::new(rec.valence, rec.arousal, rec.label.clone())).await {
                tracing::debug!("diary: emote send failed: {}", error_text(&e));
            }
        }
        if self.reflection_due(&when) {
            self.reflecting.store(true, Ordering::SeqCst);
            let me = self.clone();
            tokio::spawn(async move {
                if let Err(e) = me.reflect_inner(Some(when), true).await {
                    tracing::error!("Task exception was never retrieved: diary-reflect: {}", e);
                }
            });
        }
        Ok(path)
    }

    /// A photo the owner asked for ("hey eve, take a picture of this").
    ///
    /// Kept whatever the cool factor says and outside the photo budget — the
    /// owner's judgment outranks eve's — written to the diary at once, then
    /// looked at properly so the record and the voice's answer carry a real
    /// caption. None when the frame is not a JPEG or the shelf refused it.
    /// (Python's defaults: `said=""`, `yaw=0`, `pitch=45`.)
    pub async fn keep(&self, frame: Option<&Frame>, said: &str, yaw: i64, pitch: i64) -> anyhow::Result<Option<Record>> {
        let Some(frame) = frame.filter(|f| f.fmt == "jpeg") else {
            return Ok(None);
        };
        let said = py::head(&py::collapse(said), 120);
        let (mut rec, when) = {
            let mut m = self.memory.lock();
            m.load()?;
            let when = (self.wall)();
            let ts = timestamp(&when);
            let thought =
                if said.is_empty() { "A picture you asked for".to_string() } else { format!("A picture you asked for: {said}") };
            static ALPHA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-z]+").expect("alpha regex"));
            let lowered = py::lower(&said);
            let mut tags = vec!["asked".to_string()];
            tags.extend(ALPHA.find_iter(&lowered).map(|t| t.as_str().to_string()).filter(|t| t.len() > 3).take(4));
            let rec = Record {
                id: m.next_id(),
                ts,
                weekday: i64::from(when.weekday().num_days_from_monday()),
                hour: i64::from(when.hour()),
                yaw,
                pitch,
                thought,
                tags,
                novelty: 5,
                importance: 6,
                valence: 30,
                arousal: 20,
                label: "glad".into(),
                last_accessed: ts,
                cool: 1.0,
                ..Record::default()
            };
            (rec, when)
        };
        let Some(rel) = eve_photos::save(&self.notes_dir, &when, rec.id, &frame.data, Some(self.photo_config)) else {
            return Ok(None);
        };
        rec.photo = rel.clone();
        let from_said = py::head(&said, CAPTION_CHARS);
        rec.caption = if from_said.is_empty() { py::head(&rec.thought, CAPTION_CHARS) } else { from_said };
        // The proper look names what is in the picture; the framing stays the owner's.
        self.look_closer(&mut rec, frame).await?;
        if !rec.caption.is_empty() {
            rec.thought = format!("A picture you asked for: {}", rec.caption);
        }
        rec.written = true;
        self.memory.lock().add(rec.clone())?;
        self.taken.fetch_add(1, Ordering::SeqCst);
        self.photographed.fetch_add(1, Ordering::SeqCst);
        append_note(&self.notes_dir, &when, rec.yaw, rec.pitch, &rec.thought, Some(&eve_photos::photo_line(&rel)))?;
        self.written.fetch_add(1, Ordering::SeqCst);
        tracing::info!("diary: photo on request -> {}", rel);
        Ok(Some(rec))
    }

    /// Score the view, and if eve finds it cool enough, keep the picture.
    ///
    /// Writes `cool` and `surprise` onto the record whether or not a photo
    /// is kept — the threshold is measured from that distribution — and sets
    /// `photo`, `caption` and `thumb` when one is.
    async fn consider_photo(
        &self,
        rec: &mut Record,
        note: &Note,
        reply: &Map<String, Value>,
        when: &NaiveDateTime,
    ) -> anyhow::Result<bool> {
        let now_ts = timestamp(when);
        let empty = Map::new();
        let photo = match reply.get("photo") {
            Some(Value::Object(m)) => m,
            _ => &empty,
        };
        let subject = py::lower(py::strip(&str_or_empty(photo.get("subject"))));
        let caption = py::head(&py::collapse(&str_or_empty(photo.get("caption"))), CAPTION_CHARS);
        let want = match photo.get("want") {
            None => 0.0,
            Some(v) => py::float(v).unwrap_or(0.0),
        };

        let (cool, threshold, h, superseded, parts) = {
            let m = self.memory.lock();
            let want = py::min(1.0, want + taste_bonus(&rec.tags, &m.load_highlights()?));
            let recent = tail(&m.records, COOL_WINDOW);
            let recent_novelty: Vec<i64> = recent.iter().map(|r| r.novelty).collect();
            let (mut cool, parts) = cool_factor(
                rec.novelty,
                rec.importance,
                rec.arousal,
                note.surprise,
                &subject,
                &rec.changed,
                want,
                &recent_novelty,
                note.thumb.as_deref(),
                &rec.observations,
            );
            // A genuine first — nothing in memory shares more than one tag with it —
            // is kept whatever the arithmetic says.
            if cool > 0.0 && rec.novelty >= FIRST_TIME_NOVELTY {
                let mine: BTreeSet<&String> = rec.tags.iter().collect();
                let shared = recent
                    .iter()
                    .map(|r| r.tags.iter().collect::<BTreeSet<&String>>().intersection(&mine).count())
                    .max()
                    .unwrap_or(0);
                if shared <= FIRST_TIME_SHARED_TAGS {
                    cool = py::max(cool, FIRST_TIME_FLOOR);
                }
            }

            let shelf = album(&m.records, now_ts);
            let (h, sup) =
                habituation(&shelf, &rec.tags, note.yaw, note.pitch, note.thumb.as_deref(), now_ts, rec.importance);
            let superseded = sup.and_then(|s| m.records.iter().position(|r| std::ptr::eq(r, s)));
            let cool = py::round_to(cool * h, 3);
            rec.cool = cool;
            rec.surprise = py::round_to(note.surprise.unwrap_or(0.0), 3);

            let cools: Vec<f64> = recent.iter().map(|r| r.cool).filter(|c| *c != 0.0).collect();
            let threshold = cool_threshold(&cools);
            if cool < threshold {
                tracing::debug!("diary: no photo (cool {:.2} < {:.2})", cool, threshold);
                return Ok(false);
            }
            if !photo_budget_left(&shelf, when) {
                tracing::info!(
                    "diary: cool {:.2} but the photo budget is spent ({}/h, {}/day)",
                    cool,
                    PHOTOS_PER_HOUR,
                    PHOTOS_PER_DAY
                );
                return Ok(false);
            }
            (cool, threshold, h, superseded, parts)
        };

        let frame = self.photo_frame(note).await;
        if frame.fmt != "jpeg" {
            tracing::info!("diary: cool {:.2} but no picture to keep (fmt {})", cool, frame.fmt);
            return Ok(false);
        }
        let Some(rel) = eve_photos::save(&self.notes_dir, when, rec.id, &frame.data, Some(self.photo_config)) else {
            return Ok(false);
        };
        rec.photo = rel.clone();
        rec.caption = if !caption.is_empty() {
            caption
        } else if let Some(first) = rec.observations.first() {
            py::head(first, CAPTION_CHARS)
        } else {
            py::head(&rec.thought, CAPTION_CHARS)
        };
        if let Some(thumb) = note.thumb.as_deref().filter(|t| !t.is_empty()) {
            rec.thumb = base64::engine::general_purpose::STANDARD.encode(thumb);
        }
        // The picture is kept; now look at it properly. The pan is over for
        // this waypoint either way, so the extra call costs nothing the human
        // waits for, and it is the difference between "a white object" and "a
        // sewing machine".
        self.look_closer(rec, &frame).await?;
        if let Some(i) = superseded {
            if let Some(old) = self.memory.lock().records.get_mut(i) {
                old.superseded_by = rec.id;
            }
        }
        self.photographed.fetch_add(1, Ordering::SeqCst);
        let summary: Vec<String> = parts
            .iter()
            .map(|(k, v)| format!("{}{:.2}", k.chars().next().map(|c| c.to_ascii_uppercase()).unwrap_or(' '), v))
            .collect();
        tracing::info!("diary: photo (cool {:.2} >= {:.2}; {} h={:.2}) -> {}", cool, threshold, summary.join(" "), h, rel);
        Ok(true)
    }

    /// A second, unhurried look at a photo eve decided to keep.
    ///
    /// The thought that came out of the pan was formed from a 160x120
    /// thumbnail at the API's coarsest setting: enough to notice that
    /// something changed, not enough to say what it is. This call sends the
    /// full-size picture at high detail and asks what is actually there, then
    /// replaces the guess with what it found — the caption, the diary
    /// sentence, and the tags a future retrieval will match on.
    ///
    /// Failure is not fatal: the thumbnail's thought stands, and the photo is
    /// still kept. eve is allowed to have looked and still not be sure.
    async fn look_closer(&self, rec: &mut Record, frame: &Frame) -> anyhow::Result<bool> {
        let (image, mime) = frame_image(frame)?;
        let saw = rec.observations.join("; ");
        let context = format!(
            "What you thought when you glanced at this while panning: {}\nWhat you thought you saw: {}\nHead at yaw={:+} pitch={}.\n\n{}",
            rec.thought,
            if saw.is_empty() { "(nothing specific)" } else { saw.as_str() },
            rec.yaw,
            rec.pitch,
            ASK_FOR_JSON
        );
        let reply = match self.ask(self.timeout, move |c| c.examine(&image, mime, &context)).await {
            Ok(r) => r,
            Err(Failure::Cancelled) => return Err(DiaryCancelled.into()),
            Err(f) => {
                // the first thought stands
                self.log_error(&f);
                tracing::info!("diary: could not look closer at {} ({})", rec.photo, f.name());
                return Ok(false);
            }
        };

        let sees: Vec<String> = str_items(reply.get("sees"), |s| py::head(s, 120))?.into_iter().take(8).collect();
        let caption = py::head(&py::collapse(&str_or_empty(reply.get("caption"))), CAPTION_CHARS);
        let thought = py::head(&py::collapse(&str_or_empty(reply.get("thought"))), 400);
        let objects: Vec<String> =
            str_items(reply.get("objects"), |s| py::head(&py::lower(s), 24))?.into_iter().take(8).collect();
        let confident = reply.get("confident") == Some(&Value::Bool(true));

        if !sees.is_empty() {
            rec.observations = sees;
        }
        if !caption.is_empty() {
            rec.caption = caption;
        }
        if !objects.is_empty() {
            // The named things lead: retrieval and the photo gate's "same
            // subject again" both match on tags, and "sewing machine" is worth
            // more to both than "object".
            let mut seen: Vec<String> = vec![];
            for t in objects.into_iter().chain(rec.tags.iter().cloned()) {
                if !seen.contains(&t) {
                    seen.push(t);
                }
            }
            seen.truncate(8);
            rec.tags = seen;
        }
        if !thought.is_empty() && confident {
            tracing::info!(
                "diary: looked closer — {} becomes {}",
                py::repr_str(&py::head(&rec.thought, 40)),
                py::repr_str(&py::head(&thought, 40))
            );
            rec.thought = thought;
        }
        self.examined.fetch_add(1, Ordering::SeqCst);
        Ok(true)
    }

    /// The best picture available right now: the board's full-size
    /// snapshot, or the streamed frame eve already has when the board
    /// cannot take one.
    async fn photo_frame(&self, note: &Note) -> Frame {
        if let Some(snapshot) = &self.snapshot {
            match snapshot().await {
                Ok(Some(shot)) => return shot,
                Ok(None) => {}
                // a failed snapshot is not a failed thought
                Err(e) => tracing::warn!("diary: snapshot failed: {}: {}", raised_name(&e), error_text(&e)),
            }
        }
        note.frame.clone()
    }

    /// `_record`: the reply as a memory record, or None when it has no usable thought.
    fn record(
        &self,
        m: &Memory,
        reply: &Map<String, Value>,
        when: &NaiveDateTime,
        yaw: i64,
        pitch: i64,
    ) -> Result<Option<Record>, PyError> {
        let cands: Vec<Value> = match reply.get("thoughts") {
            Some(Value::Array(a)) => a.clone(),
            _ => {
                let single = reply.get("thought").map(py::str_of).unwrap_or_default();
                let single = py::strip(&single);
                if single.is_empty() {
                    vec![]
                } else {
                    vec![json!({"text": single, "p": 0.5})]
                }
            }
        };
        let recent: Vec<String> = tail(&m.records, 10).iter().map(|r| r.thought.clone()).collect();
        let Some(thought) = pick_thought(&cands, &recent, &m.baseline_vocabulary())?.filter(|t| !t.is_empty()) else {
            return Ok(None);
        };
        let tags: Vec<String> =
            str_items(reply.get("tags"), |s| py::head(&py::lower(s), 24))?.into_iter().take(8).collect();
        let obs: Vec<String> = str_items(reply.get("observations"), |s| py::head(s, 120))?.into_iter().take(8).collect();
        let changed: Vec<String> = str_items(reply.get("changed"), |s| py::head(s, 120))?.into_iter().take(8).collect();
        let ts = timestamp(when);
        let label = py::lower(py::strip(&match reply.get("label") {
            Some(v) if py::truthy(v) => py::str_of(v),
            _ => "calm".to_string(),
        }));
        let label = py::head(&label, 11);
        Ok(Some(Record {
            id: m.next_id(),
            ts,
            weekday: i64::from(when.weekday().num_days_from_monday()),
            hour: i64::from(when.hour()),
            yaw,
            pitch,
            thought: py::head(&thought, 400),
            observations: obs,
            changed,
            tags,
            novelty: int_field(reply, "novelty", 5, 1, 10)?,
            importance: int_field(reply, "importance", 3, 1, 10)?,
            valence: int_field(reply, "valence", 0, -100, 100)?,
            arousal: int_field(reply, "arousal", 0, -100, 100)?,
            label: if label.is_empty() { "calm".into() } else { label },
            last_accessed: ts,
            ..Record::default()
        }))
    }

    // -- reflection --
    fn reflection_due(&self, when: &NaiveDateTime) -> bool {
        if self.reflecting.load(Ordering::SeqCst) {
            return false;
        }
        let m = self.memory.lock();
        if m.last_reflection_day.as_deref() == Some(day_of(when).as_str()) {
            return false;
        }
        if m.importance_since_reflection >= REFLECT_IMPORTANCE_SUM {
            return true;
        }
        when.hour() >= REFLECT_HOUR && !m.today(when).is_empty()
    }

    /// One text-only call — eve's dreams: insights into today's diary under
    /// `## Dreams`, a rewritten profile, and up to two ★ (candidate) lines.
    pub async fn reflect(&self, when: Option<NaiveDateTime>) -> anyhow::Result<bool> {
        self.reflect_inner(when, false).await
    }

    /// `claimed`: `take` already set the reflecting flag when it scheduled
    /// this pass, so it is released wherever Python would not have set it.
    async fn reflect_inner(&self, when: Option<NaiveDateTime>, claimed: bool) -> anyhow::Result<bool> {
        let release = || {
            if claimed {
                self.reflecting.store(false, Ordering::SeqCst);
            }
        };
        let (when, prompt) = {
            let mut m = self.memory.lock();
            if let Err(e) = m.load() {
                release();
                return Err(e);
            }
            let when = when.unwrap_or_else(|| (self.wall)());
            let todays = m.today(&when);
            if todays.is_empty() {
                release();
                return Ok(false);
            }
            self.reflecting.store(true, Ordering::SeqCst);
            let body: Vec<String> = tail(&todays, 60)
                .iter()
                .map(|r| {
                    let obs = r.observations.join("; ");
                    let changed = r.changed.join("; ");
                    format!(
                        "[{}] {} obs: {} | changed: {} | thought: {} | tags: {}",
                        r.id,
                        local(r.ts).format("%H:%M"),
                        if obs.is_empty() { "-" } else { obs.as_str() },
                        if changed.is_empty() { "-" } else { changed.as_str() },
                        r.thought,
                        r.tags.join(", ")
                    )
                })
                .collect();
            let stars = match m.load_highlights() {
                Ok(s) => s,
                Err(e) => {
                    drop(m);
                    self.failed.fetch_add(1, Ordering::SeqCst);
                    self.log_error(&Failure::Raised(e.into()));
                    self.reflecting.store(false, Ordering::SeqCst);
                    return Ok(false);
                }
            };
            let stars = lines_or(stars.iter().map(|h| format!("★ {h}")).collect(), "(nothing starred yet)");
            let prompt = format!(
                "PROFILE:\n{}\n\nSTARRED BY MY HUMAN:\n{}\n\nTODAY ({}):\n{}\n\n{}",
                py::strip(&m.profile),
                stars,
                when.format("%A %Y-%m-%d"),
                body.join("\n"),
                ASK_FOR_JSON
            );
            (when, prompt)
        };
        let reply = match self.ask(self.timeout * 3.0, move |c| c.reflect(&prompt)).await {
            Ok(r) => r,
            Err(Failure::Cancelled) => return Err(DiaryCancelled.into()),
            Err(f) => {
                self.failed.fetch_add(1, Ordering::SeqCst);
                self.log_error(&f);
                self.reflecting.store(false, Ordering::SeqCst);
                return Ok(false);
            }
        };
        let insights: Vec<String> = py::iter_or_empty(reply.get("insights"))?
            .iter()
            .map(|i| py::strip(&py::str_of(i)).to_string())
            .filter(|i| !i.is_empty())
            .take(3)
            .collect();
        let profile = py::strip(&str_or_empty(reply.get("profile"))).to_string();
        if !profile.is_empty() && ["## ROOM", "## HUMAN", "## SELF", "## RULES"].iter().all(|h| profile.contains(h)) {
            self.memory.lock().save_profile(&cap_blocks(&profile))?;
        }
        let cands: Vec<String> = py::iter_or_empty(reply.get("star_candidates"))?
            .iter()
            .map(|c| py::strip(&py::str_of(c)).to_string())
            .filter(|c| !c.is_empty())
            .take(2)
            .collect();
        if !insights.is_empty() || !cands.is_empty() {
            let path = self.notes_dir.join(format!("{}.md", when.format("%Y-%m-%d")));
            let mut text = format!("\n{DREAMS_HEADER}\n");
            for i in &insights {
                text.push_str(&format!("- {i}\n"));
            }
            for c in &cands {
                text.push_str(&format!("- ★ (candidate) {c}\n"));
            }
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
            f.write_all(text.as_bytes())?;
        }
        {
            let mut m = self.memory.lock();
            m.last_reflection_day = Some(day_of(&when));
            m.importance_since_reflection = 0;
            m.save_meta();
        }
        self.reflections.fetch_add(1, Ordering::SeqCst);
        self.reflecting.store(false, Ordering::SeqCst);
        tracing::info!(
            "diary: dreams done ({} insights, {} star candidates, profile {})",
            insights.len(),
            cands.len(),
            if profile.is_empty() { "kept" } else { "updated" }
        );
        Ok(true)
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
            Failure::Cancelled => "CancelledError: ".to_string(),
            Failure::Raised(e) => format!("{}: {}", raised_name(e), error_text(e)),
        };
        tracing::warn!(
            "diary: thought failed — {} (further failures muted for {} min)",
            what,
            (ERROR_LOG_INTERVAL_SECS / 60.0).floor() as i64
        );
    }
}

impl Drop for DiaryTaker {
    fn drop(&mut self) {
        self.stop();
    }
}

/// `_cap_blocks`: each `## ` block of the profile cut to `PROFILE_BLOCK_CHARS`.
fn cap_blocks(profile: &str) -> String {
    // re.split(r"(?=^## )", profile, flags=re.M): split before every line that starts with "## ".
    let mut cuts: Vec<usize> = vec![0];
    for (i, _) in profile.match_indices("## ") {
        if i != 0 && profile.as_bytes()[i - 1] == b'\n' {
            cuts.push(i);
        }
    }
    cuts.push(profile.len());
    let mut out = String::new();
    for w in cuts.windows(2) {
        out.push_str(&py::head(&profile[w[0]..w[1]], PROFILE_BLOCK_CHARS));
    }
    py::strip(&out).to_string()
}

/// The real client, or None (with one warning) when the diary cannot run.
/// `None` env reads the process environment.
pub fn make_diary_client(model: &str, environ: Option<&HashMap<String, String>>) -> Option<Arc<dyn DiaryClient>> {
    let key = eve_openrouter::key(environ);
    if key.is_empty() {
        tracing::warn!(
            "diary: OPENROUTER_API_KEY not set — the robot will look around but keep no diary \
             (put it in ~/.config/eve-bridge/env)"
        );
        return None;
    }
    match OpenAIDiaryClient::new(model, Some(&key), NOTE_TIMEOUT_SECS) {
        Ok(c) => Some(Arc::new(c)),
        Err(e) => {
            tracing::warn!("diary: client could not be built ({}) — no diary", e);
            None
        }
    }
}
