//! Port of `chat_log.py`. The chat log: the last messages of each Telegram chat, kept on this Mac so a quiet spell
//! or a restart does not wipe what the chat said.
//!
//! Until 2026-10-08 the text brain's history lived in RAM only. Ten quiet minutes (`_close_chat`) or a restart
//! emptied it, and with memory off (the default, `transcripts::MEMORY_DEFAULT`) nothing else held the words. Asked
//! "before that", the model had nothing earlier to read and repeated the newest message as if it were the one
//! before. Work handed to an agent (a computer task, Codex, Claude Code) was never in the history either, so "did
//! you tell the agent?" had nothing to answer from.
//!
//! This log is the fix, on every install, memory on or off:
//!
//! - what it keeps: per chat, the owner's and eve's messages, and one short note per hand-off to an agent and its
//!   outcome (`who="event"`), each with its time and the run (`boot`) that wrote it;
//! - where: `~/.config/eve-bridge/chat/log.json` (`EVE_CHAT_LOG_FILE` moves it), the folder 0700 and the file
//!   0600, refused in a git work tree or a synced folder (the transcripts' guards);
//! - how long: the newest `EVE_CHAT_LOG_LINES` (300) lines per chat, none older than `EVE_CHAT_LOG_DAYS` (7) days,
//!   pruned on every write and at start;
//! - how it is written: the whole file, atomically (a 0600 temp file in the same folder, fsync, rename), so a crash
//!   leaves the old log or the new one, never half of one;
//! - how it is forgotten: `forget` drops every line that matches (memory's forget_apply calls it beside the
//!   transcripts, in the daemon and in `eve-bridge memory forget`), and `clear` empties a chat. A process that finds
//!   the file rewritten by another (the CLI's forget while the daemon runs) reads it again before its next read or
//!   write, so a forget is never undone by the daemon's next save;
//! - seeded once from the transcripts: an install that had memory on before the log existed starts with an empty
//!   log, so "before that" had nothing from before its first boot though the transcripts held it (live, 2026-10-08
//!   evening). `seed` copies a chat's earlier Telegram-channel say/image lines (the Eve Link app's turns ride that
//!   channel too) into the log once, oldest first, inside the log's own bounds, and marks the chat seeded in the
//!   file so it never runs twice. The seeded lines are ordinary lines (forget reaches them like any other);
//! - which owner message a recall answer quoted: an eve line can carry `quotes`, the seq of the owner line it quoted
//!   (telegram tags it when the turn called recent_messages), so "before that" after it is a lookup;
//! - off: `EVE_CHAT_LOG=0`. Then no file is written or read, and the chat behaves as before: its history is RAM
//!   only and is cleared after ten quiet minutes.
//!
//! The model reads it two ways (telegram): a window of the newest lines is its history every turn, with a gap marker
//! where time passed, and the `recent_messages` tool returns earlier lines word for word.
//!
//! Logs never carry what was said: counts and reasons only. Nothing here leaves the machine.

pub mod py;
#[cfg(test)]
mod tests;

use std::collections::{BTreeSet, HashMap};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use chrono::{DateTime, Datelike, Local};
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{Map, Value};
use tracing::{info, warn};

use eve_transcripts::py as tpy;
use eve_transcripts::{Dt, Transcripts};

/// On for every install (2026-10-08); `EVE_CHAT_LOG=0` turns it off.
pub const CHAT_LOG_DEFAULT: bool = true;
pub const DEFAULT_FILE: &str = "~/.config/eve-bridge/chat/log.json";
/// Per chat.
pub const MAX_LINES: usize = 300;
pub const MAX_DAYS: f64 = 7.0;
/// One message on disk, at most (characters).
pub const TEXT_CHARS: usize = 4000;
/// One hand-off note, at most (characters).
pub const NOTE_CHARS: usize = 200;
pub const VERSION: i64 = 1;
pub const WHO: [&str; 3] = ["user", "eve", "event"];
/// The run name of a line copied from the transcripts (`seed`).
pub const SEED_BOOT: &str = "seed";
/// The transcript channel this chat's lines are on (the app's too).
pub const SEED_CHANNEL: &str = "telegram";
/// The transcript kinds a chat's history shows.
pub const SEED_KINDS: [&str; 2] = ["say", "image"];
/// The transcript speaker → the log's speaker.
pub const SEED_WHO: [(&str, &str); 2] = [("owner", "user"), ("eve", "eve")];

// Python's `re.compile(r"https?://\S+|www\.\S+", re.I)`: Python's `\s` also takes U+001C-U+001F
static LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)https?://[^\s\x1c-\x1f]+|www\.[^\s\x1c-\x1f]+").unwrap());

fn off(raw: &str) -> bool {
    matches!(tpy::strip(raw).to_lowercase().as_str(), "0" | "false" | "no" | "off")
}

/// `_number`: `float(str(raw).strip())` when it is at least `low`, else `default` (also for a missing or unreadable
/// value).
fn number(raw: Option<&String>, default: f64, low: f64) -> f64 {
    // str(None) is "None", which float() refuses: the default
    match raw.and_then(|r| py::float_str(r)) {
        Some(v) if v >= low => v,
        _ => default,
    }
}

/// What the environment asks for.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatLogConfig {
    pub enabled: bool,
    pub path: PathBuf,
    pub max_lines: usize,
    pub max_days: f64,
}

impl Default for ChatLogConfig {
    fn default() -> Self {
        ChatLogConfig {
            enabled: CHAT_LOG_DEFAULT,
            path: tpy::expand_user(Path::new(DEFAULT_FILE)),
            max_lines: MAX_LINES,
            max_days: MAX_DAYS,
        }
    }
}

/// Where Python raises out of `configured`: `int(float("inf"))` for an infinite `EVE_CHAT_LOG_LINES`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot convert float infinity to integer")]
    Overflow,
}

/// `EVE_CHAT_LOG=0` turns the log off (it is on by default). `EVE_CHAT_LOG_FILE` moves the file, `EVE_CHAT_LOG_LINES`
/// and `EVE_CHAT_LOG_DAYS` bound it. `None` reads the process environment.
pub fn configured(environ: Option<&HashMap<String, String>>) -> Result<ChatLogConfig, ConfigError> {
    let owned;
    let env = match environ {
        Some(e) => e,
        None => {
            owned = std::env::vars().collect::<HashMap<_, _>>();
            &owned
        }
    };
    let enabled = match env.get("EVE_CHAT_LOG") {
        Some(raw) if !tpy::strip(raw).is_empty() => !off(raw),
        _ => CHAT_LOG_DEFAULT,
    };
    let file = env.get("EVE_CHAT_LOG_FILE").map(|s| tpy::strip(s)).unwrap_or("");
    let path = py::pure(&tpy::expand_user(Path::new(if file.is_empty() { DEFAULT_FILE } else { file })));
    let lines = number(env.get("EVE_CHAT_LOG_LINES"), MAX_LINES as f64, 1.0);
    if lines.is_infinite() {
        return Err(ConfigError::Overflow);
    }
    Ok(ChatLogConfig {
        enabled,
        path,
        // int(): cut toward zero (a value past usize saturates; Python's int has no bound)
        max_lines: lines.trunc() as usize,
        max_days: number(env.get("EVE_CHAT_LOG_DAYS"), MAX_DAYS, 0.01),
    })
}

/// A hand-off note's words: one line, links taken out (the owner has them in the chat; a page's link is not eve's to
/// repeat), at most `cap` characters. Used for anything an agent or a page wrote.
pub fn note_text(text: &str, cap: usize) -> String {
    let flat = tpy::flat(&LINK.replace_all(text, "(link)"));
    let n = flat.chars().count();
    if n <= cap {
        return flat;
    }
    // flat[:cap - 1]: with a cap of 0 that is every character but the last, as Python slices
    let keep = if cap == 0 { n - 1 } else { cap - 1 };
    format!("{}…", tpy::rstrip(tpy::head(&flat, keep)))
}

/// One line of the log.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// This line's number in the log, never reused.
    pub seq: i64,
    /// Wall clock, seconds.
    pub t: f64,
    pub chat: i64,
    /// "user", "eve" or "event" (a hand-off note).
    pub who: String,
    pub text: String,
    /// The transcript kind it was written as ("" when not written there).
    pub kind: String,
    /// The transcript conversation it was written in ("" when none).
    pub conv: String,
    /// The run of the daemon that wrote it.
    pub boot: String,
    /// A note's thread: a later note with the same key replaces it.
    pub key: String,
    /// An eve line: the seq of the owner line it quoted (0: none known).
    pub quotes: i64,
    /// eve wrote it from words it did not write (tool_gate).
    pub outside: bool,
    pub extra: Map<String, Value>,
}

impl Line {
    #[allow(clippy::too_many_arguments)]
    fn new(seq: i64, t: f64, chat: i64, who: &str, text: &str, kind: &str, conv: &str, boot: &str) -> Line {
        Line {
            seq,
            t,
            chat,
            who: who.to_string(),
            text: text.to_string(),
            kind: kind.to_string(),
            conv: conv.to_string(),
            boot: boot.to_string(),
            key: String::new(),
            quotes: 0,
            outside: false,
            extra: Map::new(),
        }
    }

    /// `dataclasses.asdict(line)` as `json.dumps(..., separators=(",", ":"))` writes it: the fields in order.
    fn write(&self, out: &mut String) {
        out.push_str(&format!("{{\"seq\":{},\"t\":", self.seq));
        py::write_float(out, self.t);
        out.push_str(&format!(",\"chat\":{},\"who\":", self.chat));
        py::write_str(out, &self.who);
        out.push_str(",\"text\":");
        py::write_str(out, &self.text);
        out.push_str(",\"kind\":");
        py::write_str(out, &self.kind);
        out.push_str(",\"conv\":");
        py::write_str(out, &self.conv);
        out.push_str(",\"boot\":");
        py::write_str(out, &self.boot);
        out.push_str(",\"key\":");
        py::write_str(out, &self.key);
        out.push_str(&format!(",\"quotes\":{},\"outside\":{},\"extra\":", self.quotes, self.outside));
        py::write_value(out, &Value::Object(self.extra.clone()));
        out.push('}');
    }
}

/// The optional keywords of [`ChatLog::append_with`].
#[derive(Debug, Clone, Default)]
pub struct Append {
    pub kind: String,
    pub conv: String,
    pub key: String,
    /// The line's time (the wall clock when None).
    pub at: Option<f64>,
    pub quotes: i64,
    pub outside: bool,
}

/// The clock a log reads (`time.time`); tests inject one.
pub type Wall = Arc<dyn Fn() -> f64 + Send + Sync>;

/// The keywords of [`ChatLog::with_options`].
#[derive(Clone)]
pub struct Options {
    pub max_lines: usize,
    pub max_days: f64,
    pub wall: Option<Wall>,
}

impl Default for Options {
    fn default() -> Self {
        Options { max_lines: MAX_LINES, max_days: MAX_DAYS, wall: None }
    }
}

/// What `seed` reads from the transcripts (`transcripts.Transcripts`, duck-typed in Python).
pub trait SeedStore {
    fn enabled(&self) -> bool;
    /// The transcript day a moment belongs to.
    fn day_of(&self, at: Dt) -> String;
    /// Every day with a transcript file, oldest first.
    fn days(&self) -> Vec<String>;
    /// Every well-formed line of one day, in the order written.
    fn lines(&self, day: &str) -> Vec<Arc<eve_transcripts::Line>>;
}

impl SeedStore for Transcripts {
    fn enabled(&self) -> bool {
        self.enabled
    }

    fn day_of(&self, at: Dt) -> String {
        Transcripts::day_of(self, at)
    }

    fn days(&self) -> Vec<String> {
        Transcripts::days(self)
    }

    fn lines(&self, day: &str) -> Vec<Arc<eve_transcripts::Line>> {
        Transcripts::lines(self, day)
    }
}

type FileStamp = (u64, i128, u64);

#[derive(Default)]
struct State {
    lines: Vec<Line>,
    seq: i64,
    // chats already seeded from the transcripts (kept in the file)
    seeded: BTreeSet<i64>,
    // moves on forget and clear: a reader's cached window is stale
    generation: u64,
    // why the folder may not hold the log, or ""
    refused: String,
    failed: u64,
    // the file as this process last read or wrote it
    stamp: Option<FileStamp>,
}

/// The log, loaded once and kept in RAM; every change rewrites the file atomically. Thread-safe; never fails from a
/// write (a disk that refuses costs the persistence, never the chat).
pub struct ChatLog {
    pub path: PathBuf,
    pub max_lines: usize,
    pub max_days: f64,
    /// This run; a line from another run was before a restart.
    pub boot: String,
    wall: Wall,
    state: Mutex<State>,
}

impl std::fmt::Debug for ChatLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatLog")
            .field("path", &self.path)
            .field("max_lines", &self.max_lines)
            .field("max_days", &self.max_days)
            .field("boot", &self.boot)
            .finish()
    }
}

fn now_secs() -> f64 {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    d.as_secs_f64()
}

fn token_hex4() -> String {
    hex::encode(rand::random::<[u8; 4]>())
}

impl ChatLog {
    /// A log at `path` with the default bounds and the system clock.
    pub fn new(path: impl AsRef<Path>) -> ChatLog {
        ChatLog::with_options(path, Options::default())
    }

    pub fn with_options(path: impl AsRef<Path>, opts: Options) -> ChatLog {
        let log = ChatLog {
            path: py::pure(&tpy::expand_user(path.as_ref())),
            max_lines: opts.max_lines.max(1),
            max_days: opts.max_days,
            boot: token_hex4(),
            wall: opts.wall.unwrap_or_else(|| Arc::new(now_secs)),
            state: Mutex::new(State::default()),
        };
        log.load();
        log
    }

    /// Moves on forget, clear, seed and a re-read: a reader's cached window is stale.
    pub fn generation(&self) -> u64 {
        self.state.lock().generation
    }

    /// Why the folder may not hold the log, or "".
    pub fn refused(&self) -> String {
        self.state.lock().refused.clone()
    }

    fn wall(&self) -> f64 {
        (self.wall)()
    }

    // -- reading --

    /// This chat's lines, oldest first, within the bounds.
    pub fn lines(&self, chat: i64) -> Vec<Line> {
        let mut st = self.state.lock();
        self.refresh(&mut st);
        self.prune(&mut st);
        st.lines.iter().filter(|ln| ln.chat == chat).cloned().collect()
    }

    /// How many lines' text matches.
    pub fn count(&self, pred: impl Fn(&str) -> bool) -> usize {
        let mut st = self.state.lock();
        self.refresh(&mut st);
        st.lines.iter().filter(|ln| pred(&ln.text)).count()
    }

    // -- writing --

    /// [`ChatLog::append_with`] with no optional keywords.
    pub fn append(&self, chat: i64, who: &str, text: &str) -> Option<Line> {
        self.append_with(chat, who, text, Append::default())
    }

    /// One line → the line kept (None for empty text or an unknown speaker). A note whose `key` is the chat's newest
    /// line's key replaces that line, so a run of one agent's answers is one note. `outside`: the line carries words
    /// eve did not write; it is kept so, and marked in tool_gate's MARKS on every load, so a restart does not launder
    /// it into eve's own words (v3 safety review).
    pub fn append_with(&self, chat: i64, who: &str, text: &str, o: Append) -> Option<Line> {
        if !WHO.contains(&who) {
            return None;
        }
        let mut text = tpy::strip(text).to_string();
        if text.is_empty() {
            return None;
        }
        if text.chars().count() > TEXT_CHARS {
            text = format!("{}…", tpy::head(&text, TEXT_CHARS - 1));
        }
        let mut st = self.state.lock();
        self.refresh(&mut st);
        let now = o.at.unwrap_or_else(|| self.wall());
        let last = st.lines.iter().rposition(|ln| ln.chat == chat);
        let line = match last {
            Some(i) if !o.key.is_empty() && st.lines[i].key == o.key => {
                let last = &mut st.lines[i];
                last.text = text;
                last.t = now;
                last.boot = self.boot.clone();
                last.outside = last.outside || o.outside;
                last.clone()
            }
            _ => {
                st.seq += 1;
                let mut line = Line::new(st.seq, now, chat, who, &text, &o.kind, &o.conv, &self.boot);
                line.key = o.key;
                line.quotes = o.quotes;
                line.outside = o.outside;
                st.lines.push(line.clone());
                line
            }
        };
        self.prune(&mut st);
        self.save(&mut st);
        // the line as kept: a later prune of the same call cannot have dropped the line just written unless it is
        // already past the age bound, and Python hands it back then too
        Some(line)
    }

    /// Copy this chat's earlier lines from the transcripts into the log, once → lines copied. Only what the log does
    /// not have: lines older than the chat's oldest kept line, from conversations the log has no line of; the
    /// Telegram channel's say/image lines of the owner and eve (the Eve Link app's turns are on that channel); none
    /// forgotten; within max_days, and only as many as fit under max_lines beside the chat's lines, the newest of
    /// them. They go in before every kept line, oldest first, as run `SEED_BOOT`. The chat is marked seeded in the
    /// file, so this never runs twice, even after a forget empties the chat. Never fails.
    pub fn seed(&self, chat: i64, store: Option<&dyn SeedStore>) -> usize {
        let Some(store) = store.filter(|s| s.enabled()) else {
            return 0;
        };
        {
            let mut st = self.state.lock();
            self.refresh(&mut st);
            if st.seeded.contains(&chat) {
                return 0;
            }
        }
        let cutoff = self.wall() - self.max_days * 86400.0;
        let first = match local_moment(cutoff) {
            Ok(at) => store.day_of(at),
            Err(name) => {
                // a transcript that cannot be read costs the seed, never the chat
                warn!("chat log: not seeded from the transcripts ({})", name);
                return 0;
            }
        };
        let mut rows: Vec<(f64, &'static str, String, String, String, bool)> = Vec::new();
        for day in store.days() {
            if day < first {
                continue;
            }
            for row in store.lines(&day) {
                let who = SEED_WHO
                    .iter()
                    .find(|(k, _)| row.get("who").and_then(Value::as_str) == Some(k))
                    .map(|(_, v)| *v);
                let text = match row.get("text") {
                    Some(v) if tpy::truthy(v) => tpy::strip(&eve_tool_gate::py_str(v)).to_string(),
                    _ => String::new(),
                };
                let ch = row.get("ch").and_then(Value::as_str);
                let kind = row.get("kind").and_then(Value::as_str).unwrap_or("");
                let Some(who) = who else { continue };
                if ch != Some(SEED_CHANNEL)
                    || !SEED_KINDS.contains(&kind)
                    || text.is_empty()
                    || text == eve_transcripts::FORGOTTEN
                {
                    continue;
                }
                let ts = row.get("ts").and_then(Value::as_str).unwrap_or("");
                if let Some(at) = eve_transcripts::parse_ts(ts) {
                    let t = timestamp(&at);
                    if t >= cutoff {
                        let conv = match row.get("conv") {
                            Some(v) if tpy::truthy(v) => eve_tool_gate::py_str(v),
                            _ => String::new(),
                        };
                        let out = row.get("out").is_some_and(tpy::truthy);
                        rows.push((t, who, text, kind.to_string(), conv, out));
                    }
                }
            }
        }
        rows.sort_by(|a, b| a.0.total_cmp(&b.0));
        let k;
        {
            let mut st = self.state.lock();
            self.refresh(&mut st);
            if st.seeded.contains(&chat) {
                return 0;
            }
            let mine: Vec<&Line> = st.lines.iter().filter(|ln| ln.chat == chat).collect();
            let oldest = mine.iter().map(|ln| ln.t).fold(f64::INFINITY, f64::min);
            let convs: BTreeSet<&str> =
                mine.iter().filter(|ln| !ln.conv.is_empty()).map(|ln| ln.conv.as_str()).collect();
            let picked: Vec<_> = rows.into_iter().filter(|r| r.0 < oldest && !convs.contains(r.4.as_str())).collect();
            let room = self.max_lines.saturating_sub(mine.len());
            let picked = py_tail(picked, room);
            // numbered before every kept line: the log's order is its seq order. Every number moves up by the count,
            // so none is reused; a reader's cached window is stale (generation)
            k = picked.len();
            let shift = k as i64;
            for ln in st.lines.iter_mut() {
                ln.seq += shift;
                if ln.quotes != 0 {
                    // the owner line it quoted moved up by k too (0: none known)
                    ln.quotes += shift;
                }
            }
            let mut new: Vec<Line> = picked
                .into_iter()
                .enumerate()
                .map(|(i, (t, who, text, kind, conv, out))| {
                    let mut ln =
                        Line::new(i as i64 + 1, t, chat, who, tpy::head(&text, TEXT_CHARS), &kind, &conv, SEED_BOOT);
                    ln.outside = out;
                    ln
                })
                .collect();
            new.append(&mut st.lines);
            st.lines = new;
            st.seq += shift;
            st.seeded.insert(chat);
            st.generation += 1;
            self.prune(&mut st);
            self.save(&mut st);
        }
        info!("chat log: {} lines seeded from the transcripts", k);
        k
    }

    /// Drop every line whose text matches → lines dropped. The file is rewritten at once.
    pub fn forget(&self, pred: impl Fn(&str) -> bool) -> usize {
        let dropped = {
            let mut st = self.state.lock();
            self.refresh(&mut st);
            let before = st.lines.len();
            st.lines.retain(|ln| !pred(&ln.text));
            let dropped = before - st.lines.len();
            if dropped > 0 {
                st.generation += 1;
                self.save(&mut st);
            }
            dropped
        };
        if dropped > 0 {
            info!("chat log: forgot {} lines", dropped);
        }
        dropped
    }

    /// Empty one chat (or all of them) → lines dropped.
    pub fn clear(&self, chat: Option<i64>) -> usize {
        let mut st = self.state.lock();
        self.refresh(&mut st);
        let before = st.lines.len();
        match chat {
            None => st.lines.clear(),
            Some(c) => st.lines.retain(|ln| ln.chat != c),
        }
        let dropped = before - st.lines.len();
        if dropped > 0 {
            st.generation += 1;
            self.save(&mut st);
        }
        dropped
    }

    // -- bounds --

    /// Older than max_days, or past max_lines in its chat: gone (from RAM here, from disk on the next save).
    fn prune(&self, st: &mut State) {
        let cutoff = self.wall() - self.max_days * 86400.0;
        st.lines.retain(|ln| ln.t >= cutoff);
        let mut per_chat: HashMap<i64, usize> = HashMap::new();
        for ln in &st.lines {
            *per_chat.entry(ln.chat).or_default() += 1;
        }
        if per_chat.values().any(|&n| n > self.max_lines) {
            // the newest max_lines of each chat
            let mut seen: HashMap<i64, usize> = HashMap::new();
            let mut keep = vec![false; st.lines.len()];
            for (i, ln) in st.lines.iter().enumerate().rev() {
                let n = seen.entry(ln.chat).or_default();
                *n += 1;
                keep[i] = *n <= self.max_lines;
            }
            let mut it = keep.into_iter();
            st.lines.retain(|_| it.next().unwrap_or(false));
        }
    }

    // -- disk --

    fn file_stamp(&self) -> Option<FileStamp> {
        let m = fs::metadata(&self.path).ok()?;
        Some((m.ino(), i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec()), m.size()))
    }

    /// Under the lock: when another process rewrote the file since this one last read or wrote it (the CLI's
    /// `memory forget` while the daemon runs), read it again, so the next save does not bring back what that process
    /// removed (review, 2026-10-08). One stat per call; nothing when the file is unchanged.
    fn refresh(&self, st: &mut State) {
        if !st.refused.is_empty() {
            return;
        }
        if self.file_stamp() == st.stamp {
            return;
        }
        let seq = st.seq;
        st.lines.clear();
        st.seeded.clear();
        self.read(st);
        st.seq = st.seq.max(seq); // a number is never reused, even one the other process dropped
        st.generation += 1; // a reader's cached window is stale
        self.prune(st);
    }

    fn load(&self) {
        let folder = py::parent(&self.path);
        let refused = if folder.exists() { eve_transcripts::refusal(&folder) } else { String::new() };
        let mut st = self.state.lock();
        st.refused = refused;
        if !st.refused.is_empty() {
            warn!("chat log: not kept on disk — {}", st.refused);
            return;
        }
        self.read(&mut st);
        self.prune(&mut st);
        info!("chat log: {} lines loaded", st.lines.len());
    }

    /// The file into the lines (appended, sorted by seq) and its stamp remembered. Never fails.
    fn read(&self, st: &mut State) {
        st.stamp = self.file_stamp();
        let raw = match fs::read(&self.path) {
            Ok(r) => r,
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => return,
            Err(e) => {
                warn!("chat log: could not be read ({}); starting empty", tpy::os_error_name(&e));
                return;
            }
        };
        let Some(doc) = std::str::from_utf8(&raw).ok().and_then(tpy::loads) else {
            warn!("chat log: the file is not readable JSON; starting empty");
            return;
        };
        let Value::Object(doc) = doc else {
            return;
        };
        if let Some(Value::Array(seeded)) = doc.get("seeded") {
            for chat in seeded {
                // isinstance(chat, int) and not a bool: a JSON integer
                if let Some(c) = chat.as_i64() {
                    st.seeded.insert(c);
                }
            }
        }
        if let Some(Value::Array(rows)) = doc.get("lines") {
            for row in rows {
                if let Some(line) = parse(row) {
                    st.lines.push(line);
                }
            }
        }
        st.lines.sort_by_key(|ln| ln.seq);
        st.seq = st.lines.iter().map(|ln| ln.seq).max().unwrap_or(0);
    }

    fn save(&self, st: &mut State) {
        if !st.refused.is_empty() {
            return;
        }
        let folder = py::parent(&self.path);
        if !folder.exists() {
            let why = eve_transcripts::prepare(&folder);
            if !why.is_empty() {
                warn!("chat log: not kept on disk — {}", why);
                st.refused = why;
                return;
            }
        }
        let data = dump(st);
        let name = self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let tmp = folder.join(format!(".{}.{}.{}.tmp", name, std::process::id(), token_hex4()));
        let wrote = (|| -> io::Result<()> {
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&tmp)?;
            f.set_permissions(fs::Permissions::from_mode(0o600))?;
            f.write_all(data.as_bytes())?;
            // os.fsync: plain fsync(2) (Rust's sync_all is F_FULLFSYNC on macOS)
            // SAFETY: the descriptor is open for the life of `f`.
            if unsafe { libc::fsync(f.as_raw_fd()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            drop(f);
            fs::rename(&tmp, &self.path)
        })();
        match wrote {
            Ok(()) => st.stamp = self.file_stamp(),
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                st.failed += 1;
                if st.failed == 1 || st.failed.is_multiple_of(100) {
                    warn!("chat log: not written ({}); {} failed writes", tpy::os_error_name(&e), st.failed);
                }
            }
        }
    }
}

/// `json.dumps({"version", "seeded", "lines"}, ensure_ascii=False, separators=(",", ":"))`.
fn dump(st: &State) -> String {
    let mut out = format!("{{\"version\":{VERSION},\"seeded\":[");
    let seeded: Vec<String> = st.seeded.iter().map(i64::to_string).collect();
    out.push_str(&seeded.join(","));
    out.push_str("],\"lines\":[");
    for (i, ln) in st.lines.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        ln.write(&mut out);
    }
    out.push_str("]}");
    out
}

/// `seq[len(seq) - room:] if room else []` with Python's slice rules: a negative start counts from the end and is
/// clamped at 0. So when `room` is more than the rows but less than twice as many, the start lands inside the list
/// and only its last `2 * len - room` rows are kept; kept so (see PARITY.md, Differences).
fn py_tail<T>(seq: Vec<T>, room: usize) -> Vec<T> {
    if room == 0 {
        return Vec::new();
    }
    let n = seq.len() as i128;
    let mut start = n - room as i128;
    if start < 0 {
        start = (n + start).max(0);
    }
    let start = start as usize;
    seq.into_iter().skip(start).collect()
}

/// `datetime.fromtimestamp(t).astimezone()`: the local moment (microseconds, half to even). Err: the name of the
/// exception Python raises for a moment it cannot hold.
fn local_moment(t: f64) -> Result<Dt, &'static str> {
    if !t.is_finite() {
        return Err(if t.is_nan() { "ValueError" } else { "OverflowError" });
    }
    let micros = (t * 1e6).round_ties_even();
    if micros.abs() >= 9.2e18 {
        return Err("OverflowError");
    }
    let at = DateTime::from_timestamp_micros(micros as i64).ok_or("ValueError")?;
    let local = at.with_timezone(&Local);
    if !(1..=9999).contains(&local.year()) {
        return Err("ValueError");
    }
    Ok(local.fixed_offset())
}

/// An aware datetime's `.timestamp()`: whole microseconds over 10**6.
fn timestamp(at: &Dt) -> f64 {
    at.timestamp_micros() as f64 / 1e6
}

/// One row of the file → a line, or None for a row that cannot be one.
fn parse(row: &Value) -> Option<Line> {
    let Value::Object(row) = row else {
        return None;
    };
    let text_of = |key: &str| -> String {
        match row.get(key) {
            Some(v) if tpy::truthy(v) => eve_tool_gate::py_str(v),
            _ => String::new(),
        }
    };
    let line = Line {
        seq: py::int_value(row.get("seq")?)?,
        t: py::float_value(row.get("t")?)?,
        chat: py::int_value(row.get("chat")?)?,
        who: eve_tool_gate::py_str(row.get("who")?),
        text: eve_tool_gate::py_str(row.get("text")?),
        kind: text_of("kind"),
        conv: text_of("conv"),
        boot: text_of("boot"),
        key: text_of("key"),
        quotes: match row.get("quotes") {
            Some(v) if tpy::truthy(v) => py::int_value(v)?,
            _ => 0,
        },
        outside: row.get("outside").is_some_and(tpy::truthy),
        extra: Map::new(),
    };
    if line.outside {
        // outside words read back from the file: marked again
        eve_tool_gate::MARKS.add([line.text.as_str()]);
    }
    (WHO.contains(&line.who.as_str()) && !line.text.is_empty()).then_some(line)
}

/// The daemon's log, or None when `EVE_CHAT_LOG=0`.
pub fn open_configured(environ: Option<&HashMap<String, String>>) -> Result<Option<ChatLog>, ConfigError> {
    let cfg = configured(environ)?;
    if !cfg.enabled {
        info!("chat log: off (EVE_CHAT_LOG=0) — the chat's history is RAM only and clears when it goes quiet");
        return Ok(None);
    }
    Ok(Some(ChatLog::with_options(
        &cfg.path,
        Options { max_lines: cfg.max_lines, max_days: cfg.max_days, wall: None },
    )))
}
