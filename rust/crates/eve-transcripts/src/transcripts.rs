//! Every word, kept on this Mac: a per-day transcript store for voice and Telegram.
//!
//! Until 2026-09-23 eve kept no transcript. A conversation lived in RAM until it closed, went to one model call to
//! be distilled, and was dropped. One failed call, one restart, one crash, and the words were gone for good; a
//! Telegram chat could not see the voice chat that happened an hour ago; and nothing could answer "what exactly did
//! I say about it on Tuesday". The owner reversed the 2026-09-11 rule (owner, 2026-09-23): the raw words are now
//! written down, locally, the moment they are spoken or typed.
//!
//! Memory is now three stores (owner, 2026-09-23): these transcripts, the records and mem0. The transcripts are the
//! source; the nightly dream reads a whole day from here (`day_text`) and the other two are rebuilt from what it
//! finds.
//!
//! Two channels are the owner talking (`TALK`: voice and telegram). A third, `bus`, holds a line some other program
//! sent over the memory bus asking eve to keep it: it is in the day the dream reads and in search, labelled "sent",
//! but never in a prompt's today block, never in recent_conversation, and never the "last talked" time — it is not
//! the owner speaking.
//!
//! ### Where the words live, and why there
//!
//! `~/.config/eve-bridge/memory/transcripts/<YYYY-MM-DD>.jsonl`, one JSON object per line (`EVE_MEMORY_DIR` moves
//! the whole memory folder). Meeting notes eve was asked to take sit beside them, under `meetings/<date>/`. A day
//! starts at 04:00, not midnight, so a conversation at 01:00 belongs to the evening it continued. The words must
//! never reach a git object, a cloud folder or a backup, so the guards below refuse, with one WARNING naming the
//! rule, a root that is:
//!
//! - inside a git work tree (any parent holds `.git`);
//! - in a synced or backed-up user folder (iCloud, CloudStorage, Documents, Desktop);
//! - a symlink, or not owned by this user.
//!
//! The folder is 0700, each file 0600 (set with fchmod, whatever the umask says), and an empty
//! `.metadata_never_index` keeps Spotlight out. FileVault does the rest. Nothing is pruned: the owner keeps every day
//! (owner, 2026-09-23).
//!
//! ### Writes are one syscall; reads are cached
//!
//! `append` is synchronous and cheap: one `write` of one whole line on an `O_APPEND` descriptor, under a thread lock
//! and an `flock` (so the forget CLI in another process can rewrite a file safely). Failure is silence: an unwritable
//! disk counts `stats.failed` and logs once an hour, and the conversation carries on.
//!
//! Readers parse a file once per (mtime, size, inode) and skip torn or malformed lines, so a crash mid-write costs
//! one line, never the day.
//!
//! ### Logs never carry what was said
//!
//! Counts, channels and seconds only. The words are on disk for the owner and for eve's own recall; they do not
//! belong in a log file that gets pasted into bug reports.
//!
//! This module uses no network library. Nothing here leaves the machine.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Instant;

use chrono::{DateTime, Duration, FixedOffset, NaiveDate};
use parking_lot::{Mutex, RwLock};
use regex::Regex;
use serde_json::{json, Map, Value};
use tracing::{info, warn};

use crate::py;

pub const MEMORY_DEFAULT: bool = false; // owner, 2026-09-23: off in code, on in the owner's env
pub const DEFAULT_MEMORY_DIR: &str = "~/.config/eve-bridge/memory";
pub const TRANSCRIPTS_SUBDIR: &str = "transcripts";
pub const MEETINGS_SUBDIR: &str = "meetings"; // <root>/meetings/<date>/*.md: notes eve was asked to take
pub const DAY_START_HOUR: i64 = 4; // 04:00: the night belongs to the evening before
pub const VOICE_CHARS: usize = 4000; // today, in the Live voice prompt (latency-bound)
pub const BACKEND_CHARS: usize = 16000; // today, in the voice backend's prompt
pub const DAY_TEXT_CHARS: usize = 100_000; // one whole day, for the nightly dream

pub const TALK: [&str; 2] = ["voice", "telegram"]; // where the owner talks: the only channels a prompt block shows
pub const BUS: &str = "bus"; // a line sent over the memory bus: kept for the dream, never talk
pub const CHANNELS: [&str; 3] = ["voice", "telegram", BUS];
pub const WHO: [&str; 4] = ["owner", "eve", "claude", "system"];
// "media": a picture or file eve sent the owner, its caption as the text and the app's copy in `att`
// (telegram.TelegramInlet._send_media). For Eve Link's chat: no prompt block, history or index takes it, since the
// model already had the picture as its tool's result; `read` and `search` show it as any other line.
pub const KINDS: [&str; 7] = ["say", "tool", "command", "relay", "image", "close", "media"];
pub const RENDER_KINDS: [&str; 3] = ["say", "tool", "image"]; // what a prompt block shows by default
pub const MEETING: &str = "meeting"; // the search label for meeting notes
pub const NOTE: &str = "note"; // the search label for the retired session notes and day files
pub const FORGOTTEN: &str = "(forgotten)"; // what a redacted line's text becomes

pub const MAX_TEXT_CHARS: usize = 20000; // one line on disk
pub const VIA_CHARS: usize = 16; // a line's optional door ("app"), beside its channel
pub const LINE_CHARS: usize = 800; // one rendered line in a text-style block
pub const TOOL_LINE_CHARS: usize = 300;
pub const VOICE_LINE_CHARS: usize = 300; // voice blocks are small: shorter lines, more of them
pub const VOICE_TOOL_LINE_CHARS: usize = 150;
pub const HEADER_BUCKET: usize = 50; // the "N earlier lines" count moves in steps of this
pub const KEEP_AFTER_CUT: f64 = 0.6; // after an overflow, keep this share of the budget
pub const SEARCH_DAYS_BACK: i64 = 30;
pub const SEARCH_LIMIT: i64 = 12;
pub const SEARCH_HIT_CHARS: usize = 300;
pub const SEARCH_NEIGHBOUR_CHARS: usize = 200;
pub const SEARCH_RESULT_CHARS: usize = 3000;
pub const SEARCH_BUDGET_SECS: f64 = 1.5;
pub const READ_MAX_CHARS: i64 = 8000;
pub const FIND_CAP: usize = 10000;
pub const FAIL_LOG_EVERY_SECS: f64 = 3600.0;

pub const LOCK_FILE: &str = ".lock";
pub const NEVER_INDEX: &str = ".metadata_never_index";
/// Folders a copy of the words must never sit in: they sync to a cloud or get backed up off the machine.
pub const SYNCED_UNDER_HOME: [&str; 4] = [
    "Library/Mobile Documents",
    "Library/CloudStorage",
    "Documents",
    "Desktop",
];

pub const OUTSIDE_LEFT_OUT: &str = "(words from outside eve, left out)";

// Python's `$` also matches before a final "\n": `\n?\z` keeps that.
static DAY_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d{4}-\d{2}-\d{2})\.jsonl\n?\z").unwrap());
pub(crate) static DAY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}\n?\z").unwrap());
static DAY_NOTE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d{4}-\d{2}-\d{2})-.+\.md\n?\z").unwrap());
static HHMM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d{1,2}):(\d{2})\n?\z").unwrap());
pub(crate) static CONV: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z]-(\d{10,16})-[0-9a-f]{4}\n?\z").unwrap());
static PHRASE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""([^"]*)""#).unwrap());
static NOTE_TIME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d{2})(\d{2})-").unwrap());
static FRONTMATTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)\A---\n.*?\n---\n").unwrap());
static BULLET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(#{1,6}[\s\x1c-\x1f]+|[-*][\s\x1c-\x1f]+)").unwrap());

/// One transcript line as it is on disk: `{ts, ch, conv, who, kind, text}` plus whatever optional keys it carries.
pub type Line = Map<String, Value>;
/// An aware moment (Python's aware `datetime`).
pub type Dt = DateTime<FixedOffset>;
/// The clock a store reads (`wall`); tests inject one.
pub type Wall = Arc<dyn Fn() -> Dt + Send + Sync>;
/// A line predicate: [`match_query`]'s result.
pub type Pred = Box<dyn Fn(&str) -> bool + Send + Sync>;
/// Called with the day after each line is written.
pub type Listener = Arc<dyn Fn(&str) + Send + Sync>;

/// A moment, as a datetime or an ISO string (Python's `Stamp = Union[datetime, str]`).
#[derive(Debug, Clone, Copy)]
pub enum Stamp<'a> {
    At(Dt),
    Text(&'a str),
}

impl From<Dt> for Stamp<'_> {
    fn from(d: Dt) -> Self {
        Stamp::At(d)
    }
}

impl<'a> From<&'a str> for Stamp<'a> {
    fn from(s: &'a str) -> Self {
        Stamp::Text(s)
    }
}

impl<'a> From<&'a String> for Stamp<'a> {
    fn from(s: &'a String) -> Self {
        Stamp::Text(s.as_str())
    }
}

/// A line's string field ("" when missing or not a string). Every parsed line has its six fields as strings.
pub fn field<'a>(ln: &'a Line, key: &str) -> &'a str {
    ln.get(key).and_then(Value::as_str).unwrap_or("")
}

// ---- config ---------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptConfig {
    pub enabled: bool,
    pub root: PathBuf,
    /// The home folder the guards check against (`Path.home()` when None). Tests set it instead of `$HOME`.
    pub home: Option<PathBuf>,
}

impl Default for TranscriptConfig {
    fn default() -> Self {
        TranscriptConfig {
            enabled: MEMORY_DEFAULT,
            root: py::expand_user(Path::new(DEFAULT_MEMORY_DIR)).join(TRANSCRIPTS_SUBDIR),
            home: None,
        }
    }
}

impl TranscriptConfig {
    pub fn new(enabled: bool, root: impl Into<PathBuf>) -> Self {
        TranscriptConfig {
            enabled,
            root: root.into(),
            home: None,
        }
    }
}

fn on(raw: Option<&String>) -> bool {
    matches!(
        raw.map(|s| py::strip(s).to_lowercase()).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

/// `EVE_MEMORY=1` turns memory on (the older `EVE_RECORDS=1` does too). `EVE_MEMORY_DIR` moves the memory folder;
/// the transcripts are its `transcripts/`. `None` reads the process environment.
pub fn configured(environ: Option<&HashMap<String, String>>) -> TranscriptConfig {
    let owned;
    let env = match environ {
        Some(e) => e,
        None => {
            owned = std::env::vars().collect::<HashMap<_, _>>();
            &owned
        }
    };
    let enabled = on(env.get("EVE_MEMORY")) || on(env.get("EVE_RECORDS")) || MEMORY_DEFAULT;
    let raw = env
        .get("EVE_MEMORY_DIR")
        .map(|s| py::strip(s).to_string())
        .unwrap_or_default();
    let base = py::expand_user(Path::new(if raw.is_empty() {
        DEFAULT_MEMORY_DIR
    } else {
        &raw
    }));
    TranscriptConfig {
        enabled,
        root: base.join(TRANSCRIPTS_SUBDIR),
        home: None,
    }
}

// ---- guards ---------------------------------------------------------------------------------------------------------

/// Why `root` must not hold transcripts, or "" when it may. Checked before anything is created.
pub fn refusal(root: &Path) -> String {
    refusal_in(root, &py::home())
}

/// [`refusal`] against an explicit home folder. Where Python's `refusal` raises (a parent it cannot even lstat),
/// this names the failure as `_prepare` does, so the folder is refused either way.
pub fn refusal_in(root: &Path, home: &Path) -> String {
    refusal_checked(root, home).unwrap_or_else(|e| unchecked(&e))
}

fn unchecked(e: &io::Error) -> String {
    format!("the folder could not be checked ({})", py::os_error_name(e))
}

/// pathlib's `_ignore_error`: the errors `exists()` and `is_symlink()` read as "not there".
fn ignored(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(libc::ENOENT) | Some(libc::ENOTDIR) | Some(libc::EBADF) | Some(libc::ELOOP)
    )
}

/// `Path.is_symlink()`: false when missing, an error it does not ignore raised.
fn is_symlink(p: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(p) {
        Ok(m) => Ok(m.file_type().is_symlink()),
        Err(e) if ignored(&e) => Ok(false),
        Err(e) => Err(e),
    }
}

/// `Path.exists()`: false when missing, an error it does not ignore raised.
fn exists(p: &Path) -> io::Result<bool> {
    match fs::metadata(p) {
        Ok(_) => Ok(true),
        Err(e) if ignored(&e) => Ok(false),
        Err(e) => Err(e),
    }
}

/// `refusal` with Python's raises as errors.
fn refusal_checked(root: &Path, home: &Path) -> io::Result<String> {
    let root = py::abspath(&py::expand_user_in(root, home));
    if is_symlink(&root)? {
        return Ok("the folder is a symlink".into());
    }
    let real = py::resolve(&root).unwrap_or_else(|_| root.clone());
    for rel in SYNCED_UNDER_HOME {
        let plain = home.join(rel);
        let mut bases = vec![plain.clone()];
        if exists(&plain)? {
            if let Ok(r) = py::resolve(&plain) {
                bases.push(r);
            }
        }
        for base in bases {
            if real.starts_with(&base) || root.starts_with(&base) {
                return Ok(format!(
                    "the folder is under ~/{rel}, which syncs or backs up off this computer"
                ));
            }
        }
    }
    for p in real.ancestors() {
        match exists(&p.join(".git")) {
            Ok(true) => {
                return Ok(
                    "the folder is inside a git work tree, and a word must never reach a git object".into(),
                )
            }
            Ok(false) => {}
            // an unreadable parent: it cannot be vouched for
            Err(_) => return Ok("a parent folder cannot be checked for a git work tree".into()),
        }
    }
    Ok(String::new())
}

/// Create the folder private and Spotlight-proof. → the rule that failed, or "".
pub fn prepare(root: &Path) -> String {
    prepare_in(root, &py::home())
}

/// [`prepare`] against an explicit home folder.
pub fn prepare_in(root: &Path, home: &Path) -> String {
    let why = match refusal_checked(root, home) {
        Ok(why) => why,
        Err(e) => return unchecked(&e),
    };
    if !why.is_empty() {
        return why;
    }
    let made = (|| -> io::Result<Option<&'static str>> {
        fs::create_dir_all(root)?;
        let st = fs::symlink_metadata(root)?;
        if st.file_type().is_symlink() {
            return Ok(Some("the folder is a symlink"));
        }
        // SAFETY: getuid has no preconditions and cannot fail.
        if st.uid() != unsafe { libc::getuid() } {
            return Ok(Some("the folder is not owned by this user"));
        }
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        let marker = root.join(NEVER_INDEX);
        if fs::metadata(&marker).is_err() {
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&marker)?;
        }
        Ok(None)
    })();
    match made {
        Ok(Some(why)) => why.to_string(),
        Ok(None) => String::new(),
        Err(e) => format!(
            "the folder could not be prepared ({})",
            py::os_error_name(&e)
        ),
    }
}

// ---- matching (shared with forget) ------------------------------------------------------------------------------------

/// The one matching rule for search, find, redact and forget: case-insensitive; every term longer than one character
/// must appear in the line; a "quoted phrase" must appear as written (spacing aside). → a predicate over a line's
/// text, or None when the query has no usable term. (Python's `match`.)
pub fn match_query(query: &str) -> Option<Pred> {
    let phrases: Vec<String> = PHRASE
        .captures_iter(query)
        .map(|c| py::casefold(&py::flat(&c[1])))
        .collect();
    let rest = PHRASE.replace_all(query, " ").replace('"', " ");
    let strip_chars: &[char] = &[
        '.', ',', ';', ':', '!', '?', '(', ')', '[', ']', '{', '}', '\'', '`', '*',
    ];
    let terms = rest
        .split(py::is_space)
        .filter(|t| !t.is_empty())
        .map(|t| py::casefold(t.trim_matches(strip_chars)));
    let needles: Vec<String> = phrases
        .into_iter()
        .chain(terms)
        .filter(|n| py::len(n) > 1)
        .collect();
    if needles.is_empty() {
        return None;
    }
    Some(Box::new(move |text: &str| {
        let hay = py::casefold(&py::flat(text));
        needles.iter().all(|n| hay.contains(n.as_str()))
    }))
}

// ---- small helpers ----------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TranscriptStats {
    pub appended: u64,
    pub failed: u64,
    pub rejected: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvInfo {
    pub conv: String,
    pub ch: String,
    pub started: Dt,
    pub ended: Dt,
    pub closed: bool,
}

/// `_parse_ts`: an ISO string as an aware moment (a naive one read as local time), or None.
pub fn parse_ts(ts: &str) -> Option<Dt> {
    let (naive, off) = py::fromisoformat(ts)?;
    match off {
        Some(o) => naive.and_local_timezone(o).single(),
        None => Some(py::local_aware(naive)),
    }
}

fn stamp_ts(ts: Stamp<'_>) -> Option<Dt> {
    match ts {
        Stamp::At(d) => Some(d),
        Stamp::Text(s) => parse_ts(s),
    }
}

/// `_flat`: whitespace runs to one space.
pub fn flat(text: &str) -> String {
    py::flat(text)
}

/// `_clip`: at most `cap` characters, an ellipsis when cut.
pub fn clip(text: &str, cap: usize) -> String {
    py::clip(text, cap)
}

/// `_conv_epoch`: the local moment a conversation id was minted, or None for an id of another shape.
pub fn conv_epoch(conv: &str) -> Option<Dt> {
    let m = CONV.captures(conv)?;
    let ms: i64 = py::int(&m[1])?;
    let utc = DateTime::from_timestamp(
        ms.div_euclid(1000),
        (ms.rem_euclid(1000) * 1_000_000) as u32,
    )?;
    let local = utc.with_timezone(&chrono::Local).fixed_offset();
    // datetime.fromtimestamp raises past year 9999 (a 16-digit id): Python answers None
    (1..=9999).contains(&chrono::Datelike::year(&local)).then_some(local)
}

/// `_next_day`: the calendar day after a `YYYY-MM-DD` day (None for an impossible date).
pub fn next_day(day: &str) -> Option<String> {
    // strptime("%Y-%m-%d") reads any decimal digit, as int() does
    let mut parts = day.trim_end_matches('\n').splitn(3, '-');
    let (y, m, d) = (parts.next()?, parts.next()?, parts.next()?);
    let year = py::int(y)?;
    if !(1..=9999).contains(&year) {
        return None;
    }
    let d = NaiveDate::from_ymd_opt(
        year as i32,
        u32::try_from(py::int(m)?).ok()?,
        u32::try_from(py::int(d)?).ok()?,
    )?;
    let next = d + Duration::days(1);
    (chrono::Datelike::year(&next) <= 9999).then(|| next.format("%Y-%m-%d").to_string())
}

/// A line's speaker as a prompt names it: "Owner", "eve (web search)", "Owner (photo)", "Owner (to Claude)".
pub fn speaker(line: &Line) -> String {
    let base = match line.get("who").and_then(Value::as_str) {
        Some("owner") => "Owner",
        Some("eve") => "eve",
        Some("claude") => "Claude",
        _ => "system",
    };
    match line.get("kind").and_then(Value::as_str) {
        Some("tool") => {
            let raw = line
                .get("tool")
                .map(eve_memory_sources::py_str_or_empty)
                .unwrap_or_default();
            let tool = if raw.is_empty() {
                "a tool".to_string()
            } else {
                raw
            };
            format!("{base} ({})", tool.replace('_', " "))
        }
        Some("image") => format!("{base} (photo)"),
        Some("relay") => format!("{base} (to Claude)"),
        _ => base.to_string(),
    }
}

/// How a line reached eve: spoken (voice), texted (Telegram) or sent (the memory bus, not the owner).
pub fn mode(ch: &str) -> &'static str {
    match ch {
        "voice" => "spoken",
        BUS => "sent",
        _ => "texted",
    }
}

/// Whether a line's words came from outside eve: kept as such (`out`), or a tool line whose tool returns outside
/// text (tool_gate.taints, tool_gate.RESULT_LINES).
pub fn outside_words(line: &Line) -> bool {
    if line.get("out").is_some_and(py::truthy) {
        return true;
    }
    if field(line, "kind") != "tool" {
        return false;
    }
    let tool = line
        .get("tool")
        .map(eve_memory_sources::py_str_or_empty)
        .unwrap_or_default();
    eve_tool_gate::RESULT_LINES.contains(&tool)
        || eve_tool_gate::taints(&tool, &eve_tool_gate::Args::new())
}

/// One transcript line as a prompt line: 'HH:MM (spoken|texted|sent) Owner: …'. Style "voice" clips shorter.
pub fn render_line(line: &Line, style: &str) -> String {
    let clock = parse_ts(field(line, "ts"))
        .map(|t| format!("{} ", t.format("%H:%M")))
        .unwrap_or_default();
    let tool = field(line, "kind") == "tool";
    let cap = match (style == "voice", tool) {
        (true, true) => VOICE_TOOL_LINE_CHARS,
        (true, false) => VOICE_LINE_CHARS,
        (false, true) => TOOL_LINE_CHARS,
        (false, false) => LINE_CHARS,
    };
    let text = line
        .get("text")
        .map(eve_memory_sources::py_str_or_empty)
        .unwrap_or_default();
    format!(
        "{clock}({}) {}: {}",
        mode(field(line, "ch")),
        speaker(line),
        py::clip(&py::flat(&text), cap)
    )
}

/// Lines rendered one per row, oldest first as given. "" for none.
pub fn render<L: std::borrow::Borrow<Line>>(lines: &[L], style: &str) -> String {
    lines
        .iter()
        .map(|ln| render_line(ln.borrow(), style))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `_parse_line`: one well-formed line, or None for a torn or malformed one.
pub fn parse_line(raw: &str) -> Option<Line> {
    let Value::Object(obj) = py::loads(raw)? else {
        return None;
    };
    if !["ts", "ch", "conv", "who", "kind", "text"]
        .iter()
        .all(|k| obj.get(*k).is_some_and(Value::is_string))
    {
        return None;
    }
    parse_ts(field(&obj, "ts"))?;
    Some(obj)
}

/// The ranked search a store may hand the transcript part of a search to (memory_index.MemoryIndex).
pub trait Ranker: Send + Sync {
    /// `{"hits", "found"}` for the best `limit` rounds between two days, or Ok(None) when it cannot rank.
    fn search(
        &self,
        query: &str,
        first_day: &str,
        last_day: &str,
        limit: usize,
        channels: Option<&[String]>,
    ) -> anyhow::Result<Option<Value>>;
}

/// The optional keywords of [`Transcripts::append_with`].
#[derive(Debug, Clone, Default)]
pub struct Append {
    pub tool: String,
    pub now: Option<Dt>,
    pub fsync: bool,
    pub via: String,
    pub att: Option<Vec<Value>>,
    pub mid: String,
    pub outside: bool,
    pub src: Option<Value>,
}

/// The optional keywords of [`Transcripts::today_block_with`].
#[derive(Debug, Clone)]
pub struct Today {
    pub exclude_conv: Option<String>,
    pub exclude_tail: i64,
    pub kinds: Vec<String>,
    pub style: String,
    pub now: Option<Dt>,
    pub exclude: Vec<(String, i64)>,
}

impl Default for Today {
    fn default() -> Self {
        Today {
            exclude_conv: None,
            exclude_tail: 0,
            kinds: RENDER_KINDS.iter().map(|k| k.to_string()).collect(),
            style: "text".into(),
            now: None,
            exclude: Vec::new(),
        }
    }
}

/// The optional keywords of [`Transcripts::search_with`].
#[derive(Debug, Clone)]
pub struct Search {
    pub days_back: i64,
    pub limit: i64,
    pub channel: String,
    pub since: String,
    pub until: String,
    pub loose: bool,
}

impl Default for Search {
    fn default() -> Self {
        Search {
            days_back: SEARCH_DAYS_BACK,
            limit: SEARCH_LIMIT,
            channel: String::new(),
            since: String::new(),
            until: String::new(),
            loose: false,
        }
    }
}

type FileKey = (i128, u64, u64);

#[derive(Clone)]
enum Parsed {
    Lines(Arc<Vec<Arc<Line>>>),
    Doc(Arc<Vec<String>>),
}

#[derive(Default)]
struct LastState {
    last: Option<Dt>,
    scanned: bool,
}

// ---- the store --------------------------------------------------------------------------------------------------------

/// The transcript store. One instance per daemon, shared by both channels and every reader.
///
/// Search also reads the meeting notes under `<root>/meetings/<date>/` and, when `archive` is given (the retired
/// debrief store), its session notes `sessions/<date>/*.md` and day files `<date>-*.md`. `wall` is the clock (aware
/// datetimes); tests inject one.
pub struct Transcripts {
    pub config: TranscriptConfig,
    pub root: PathBuf,
    pub archive: Option<PathBuf>,
    pub enabled: bool,
    /// How long one search may read before it stops and says truncated (SEARCH_BUDGET_SECS).
    pub search_budget_secs: f64,
    wall: Wall,
    stats: Mutex<TranscriptStats>,
    write_lock: Mutex<()>, // writers: append, close, redact
    cache: Mutex<HashMap<PathBuf, (FileKey, Parsed)>>,
    lock_fd: Option<File>,
    last: Mutex<LastState>,
    minted: Mutex<HashSet<String>>,
    last_fail_log: Mutex<Option<Instant>>,
    // Called with the day after each line is written (MemoryIndex::notify: a set and an Event, safe on the event
    // loop). A listener that panics is dropped from that call, never the write.
    listeners: Mutex<Vec<Listener>>,
    // The ranked search (MemoryIndex) when EVE_MEMORY_HYBRID is on; None: keyword AND only.
    ranker: RwLock<Option<Arc<dyn Ranker>>>,
}

impl std::fmt::Debug for Transcripts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transcripts")
            .field("root", &self.root)
            .field("enabled", &self.enabled)
            .finish()
    }
}

impl Transcripts {
    pub fn new(config: TranscriptConfig, archive: Option<PathBuf>, wall: Option<Wall>) -> Self {
        let home = config.home.clone().unwrap_or_else(py::home);
        let root = py::expand_user_in(&config.root, &home);
        let archive = archive.map(|a| py::expand_user_in(&a, &home));
        let mut t = Transcripts {
            config: config.clone(),
            root,
            archive,
            enabled: false,
            search_budget_secs: SEARCH_BUDGET_SECS,
            wall: wall.unwrap_or_else(|| Arc::new(py::now_local)),
            stats: Mutex::new(TranscriptStats::default()),
            write_lock: Mutex::new(()),
            cache: Mutex::new(HashMap::new()),
            lock_fd: None,
            last: Mutex::new(LastState::default()),
            minted: Mutex::new(HashSet::new()),
            last_fail_log: Mutex::new(None),
            listeners: Mutex::new(Vec::new()),
            ranker: RwLock::new(None),
        };
        if !config.enabled {
            return t;
        }
        let why = prepare_in(&t.root, &home);
        if !why.is_empty() {
            warn!("transcripts: off — {}: {}", why, t.root.display());
            return t;
        }
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(t.root.join(LOCK_FILE))
        {
            Ok(f) => t.lock_fd = Some(f),
            Err(e) => warn!(
                "transcripts: no lock file ({}); another process could race a forget",
                py::os_error_name(&e)
            ),
        }
        t.enabled = true;
        info!(
            "transcripts: on — {} (a day starts at {:02}:00)",
            t.root.display(),
            DAY_START_HOUR
        );
        t
    }

    /// The counters: lines appended, writes failed, lines rejected.
    pub fn stats(&self) -> TranscriptStats {
        *self.stats.lock()
    }

    /// Call `listener` with the day after each line is written.
    pub fn add_listener(&self, listener: Listener) {
        self.listeners.lock().push(listener);
    }

    /// Attach (or with None detach) the ranked search.
    pub fn set_ranker(&self, ranker: Option<Arc<dyn Ranker>>) {
        *self.ranker.write() = ranker;
    }

    pub fn ranker(&self) -> Option<Arc<dyn Ranker>> {
        self.ranker.read().clone()
    }

    // -- time ----------------------------------------------------------------------------------------------------------

    pub fn now(&self) -> Dt {
        (self.wall)()
    }

    /// The transcript day a moment belongs to: the local date, with the day starting at DAY_START_HOUR.
    pub fn day_of<'a>(&self, ts: impl Into<Stamp<'a>>) -> String {
        let dt = stamp_ts(ts.into()).unwrap_or_else(|| self.now());
        (dt - Duration::hours(DAY_START_HOUR))
            .format("%Y-%m-%d")
            .to_string()
    }

    pub fn path_of(&self, day: &str) -> PathBuf {
        self.root.join(format!("{day}.jsonl"))
    }

    /// Every day with a transcript file, oldest first.
    pub fn days(&self) -> Vec<String> {
        if !self.enabled {
            return Vec::new();
        }
        let Ok(rd) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut out: Vec<String> = rd
            .flatten()
            .filter_map(|e| {
                e.file_name()
                    .to_str()
                    .and_then(|n| DAY_FILE.captures(n).map(|m| m[1].to_string()))
            })
            .collect();
        out.sort();
        out
    }

    // -- writing -------------------------------------------------------------------------------------------------------

    /// A conversation id nobody else has: '<v|t>-<epoch ms>-<4 hex>'.
    pub fn new_conv(&self, ch: &str) -> String {
        let prefix: String = ch.chars().next().unwrap_or('x').to_lowercase().collect();
        loop {
            let ms = chrono::Utc::now().timestamp_millis();
            let conv = format!("{prefix}-{ms}-{}", hex::encode(rand::random::<[u8; 2]>()));
            let mut minted = self.minted.lock();
            if !minted.contains(&conv) {
                minted.insert(conv.clone());
                if minted.len() > 4096 {
                    *minted = HashSet::from([conv.clone()]);
                }
                return conv;
            }
        }
    }

    fn flock(&self, op: libc::c_int) {
        if let Some(f) = &self.lock_fd {
            // SAFETY: the descriptor is open for the store's lifetime; flock only takes an fd and an op.
            unsafe {
                libc::flock(f.as_raw_fd(), op);
            }
        }
    }

    fn fail(&self, what: &str, e: &io::Error) {
        let failed = {
            let mut s = self.stats.lock();
            s.failed += 1;
            s.failed
        };
        let mut last = self.last_fail_log.lock();
        if last.is_none_or(|t| t.elapsed().as_secs_f64() >= FAIL_LOG_EVERY_SECS) {
            *last = Some(Instant::now());
            warn!(
                "transcripts: {} failed ({}; {} failures so far) — the conversation goes on",
                what,
                py::os_error_name(e),
                failed
            );
        }
    }

    /// Write one line with no optional keywords. See [`Transcripts::append_with`].
    pub fn append(&self, ch: &str, conv: &str, who: &str, kind: &str, text: &str) -> bool {
        self.append_with(ch, conv, who, kind, text, Append::default())
    }

    /// Write one line. Synchronous (one write), never fails loudly. → whether the line reached the disk.
    ///
    /// `mid`: which Eve Link app message an owner's line is (link_reactions keeps its reaction by it); only written
    /// when given.
    ///
    /// `via`: which door the words came through when the channel alone does not say (`"app"`: the Eve Link app's
    /// chat, which rides the telegram channel's brain). Only written when given, so every older line, which has
    /// none, loads as before. Each listener is called after a line is written, from whatever thread wrote it; one
    /// that panics is ignored. `att`: the attachments an owner's line from the app carried (`[{"id", "name",
    /// "kind"}]`); only written when given. `outside`: eve wrote the line from words it did not write (a tainted
    /// turn's reply, a task's sentence): written as `"out": 1`, and every read of the line marks it in
    /// tool_gate.MARKS, so a prompt that shows it starts tainted after a restart too (v3 safety review). `src`: the
    /// memory lines a reply leaned on (memory_sources; the app shows them as Sources under it); only written when
    /// given, on eve's reply line of a turn that read memory, and never read into a prompt.
    pub fn append_with(
        &self,
        ch: &str,
        conv: &str,
        who: &str,
        kind: &str,
        text: &str,
        opts: Append,
    ) -> bool {
        if !self.enabled {
            return false;
        }
        if !CHANNELS.contains(&ch)
            || !WHO.contains(&who)
            || !KINDS.contains(&kind)
            || conv.is_empty()
        {
            self.stats.lock().rejected += 1;
            return false;
        }
        let at = opts.now.unwrap_or_else(|| self.now());
        let mut rec = Map::new();
        rec.insert(
            "ts".into(),
            json!(at.format("%Y-%m-%dT%H:%M:%S%:z").to_string()),
        );
        rec.insert("ch".into(), json!(ch));
        rec.insert("conv".into(), json!(conv));
        rec.insert("who".into(), json!(who));
        rec.insert("kind".into(), json!(kind));
        rec.insert("text".into(), json!(py::head(text, MAX_TEXT_CHARS)));
        if !opts.tool.is_empty() {
            rec.insert("tool".into(), json!(py::head(&opts.tool, 80)));
        }
        if !opts.via.is_empty() {
            rec.insert("via".into(), json!(py::head(&opts.via, VIA_CHARS)));
        }
        if !opts.mid.is_empty() {
            rec.insert("mid".into(), json!(py::head(&opts.mid, 64)));
        }
        if let Some(att) = opts.att.as_ref().filter(|a| !a.is_empty()) {
            let kept: Vec<Value> = att
                .iter()
                .take(8)
                .filter_map(Value::as_object)
                .map(|x| {
                    let mut o = Map::new();
                    for k in ["id", "name", "kind"] {
                        let v = x.get(k).map(eve_memory_sources::py_str).unwrap_or_default();
                        o.insert(k.into(), json!(py::head(&v, 200)));
                    }
                    Value::Object(o)
                })
                .collect();
            rec.insert("att".into(), Value::Array(kept));
        }
        if opts.outside {
            rec.insert("out".into(), json!(1));
            eve_tool_gate::MARKS.add([field(&rec, "text")]);
        }
        if let Some(src) = opts.src.as_ref().filter(|s| py::truthy(s)) {
            let kept = eve_memory_sources::sanitize(src);
            if !kept.is_empty() {
                rec.insert("src".into(), eve_memory_sources::to_value(&kept));
            }
        }
        let mut data = (py::dumps(&Value::Object(rec)) + "\n").into_bytes();
        let day = self.day_of(at);
        let path = self.path_of(&day);
        {
            let _w = self.write_lock.lock();
            self.flock(libc::LOCK_EX);
            let wrote = (|| -> io::Result<()> {
                let f = OpenOptions::new()
                    .read(true)
                    .append(true)
                    .create(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(&path)?;
                f.set_permissions(fs::Permissions::from_mode(0o600))?;
                let size = f.metadata()?.len();
                // A crash mid-write left a torn last line: start on a fresh one, so only that line is lost.
                if size > 0 {
                    let mut last = [0u8; 1];
                    let n = f.read_at(&mut last, size - 1)?;
                    if n == 0 || last[0] != b'\n' {
                        data.insert(0, b'\n');
                    }
                }
                (&f).write_all(&data)?;
                // SAFETY: the descriptor is open; fsync takes only an fd (plain fsync, as os.fsync).
                if opts.fsync && unsafe { libc::fsync(f.as_raw_fd()) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            })();
            self.flock(libc::LOCK_UN);
            if let Err(e) = wrote {
                self.fail("append", &e);
                return false;
            }
            self.stats.lock().appended += 1;
            if kind != "close" && TALK.contains(&ch) {
                let mut last = self.last.lock();
                if last.last.is_none_or(|l| at > l) {
                    last.last = Some(at);
                }
            }
        }
        self.tell(&day);
        true
    }

    fn tell(&self, day: &str) {
        let listeners = self.listeners.lock().clone();
        for listener in listeners {
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(day)));
            if ran.is_err() {
                warn!("transcripts: a listener failed (panic)");
            }
        }
    }

    /// The conversation is over: a close marker, flushed to the platter.
    pub fn close(&self, ch: &str, conv: &str, now: Option<Dt>) -> bool {
        self.append_with(
            ch,
            conv,
            "system",
            "close",
            "",
            Append {
                now,
                fsync: true,
                ..Append::default()
            },
        )
    }

    // -- reading -------------------------------------------------------------------------------------------------------

    fn cached(&self, path: &Path, parse: impl FnOnce(&str) -> Parsed) -> Option<Parsed> {
        let st = match fs::metadata(path) {
            Ok(st) => st,
            Err(_) => {
                self.cache.lock().remove(path);
                return None;
            }
        };
        let key: FileKey = (
            st.mtime() as i128 * 1_000_000_000 + st.mtime_nsec() as i128,
            st.size(),
            st.ino(),
        );
        if let Some((k, v)) = self.cache.lock().get(path) {
            if *k == key {
                return Some(v.clone());
            }
        }
        let raw = fs::read(path).ok()?;
        let parsed = parse(&py::read_text(&raw));
        self.cache
            .lock()
            .insert(path.to_path_buf(), (key, parsed.clone()));
        Some(parsed)
    }

    fn invalidate(&self, path: &Path) {
        self.cache.lock().remove(path);
    }

    /// Every well-formed line of one day, in the order written. Torn or malformed lines are skipped. The lines are
    /// shared with the cache.
    pub fn lines(&self, day: &str) -> Vec<Arc<Line>> {
        if !self.enabled || !DAY.is_match(day) {
            return Vec::new();
        }
        let parsed = self.cached(&self.path_of(day), |raw| {
            Parsed::Lines(Arc::new(
                py::splitlines(raw)
                    .into_iter()
                    .filter(|r| !py::strip(r).is_empty())
                    .filter_map(parse_line)
                    .map(Arc::new)
                    .collect(),
            ))
        });
        let got: Vec<Arc<Line>> = match parsed {
            Some(Parsed::Lines(l)) => l.as_ref().clone(),
            _ => Vec::new(),
        };
        // outside words, after a restart too
        eve_tool_gate::MARKS.add(
            got.iter()
                .filter(|o| o.get("out").is_some_and(py::truthy))
                .map(|o| field(o, "text").to_string()),
        );
        got
    }

    fn conv_day(&self, conv: &str, first_ts: &str) -> String {
        match conv_epoch(conv) {
            Some(born) => self.day_of(born),
            None => self.day_of(first_ts),
        }
    }

    /// Every line of one conversation, even one that ran past the day boundary.
    pub fn conv_lines(&self, conv: &str) -> Vec<Arc<Line>> {
        let days = match conv_epoch(conv) {
            Some(born) => {
                let d = self.day_of(born);
                let mut v = vec![d.clone()];
                v.extend(next_day(&d));
                v
            }
            None => self.days(),
        };
        days.iter()
            .flat_map(|d| self.lines(d))
            .filter(|ln| field(ln, "conv") == conv)
            .collect()
    }

    /// The conversations that began on `day` (by their id's birth time), oldest first. A conversation that ran past
    /// the day boundary is read whole, so `closed` and `ended` are true.
    pub fn conversations(&self, day: &str) -> Vec<ConvInfo> {
        if !DAY.is_match(day) {
            return Vec::new();
        }
        let Some(next) = next_day(day) else {
            return Vec::new();
        };
        let mut order: Vec<String> = Vec::new();
        let mut grouped: HashMap<String, Vec<Arc<Line>>> = HashMap::new();
        for ln in self.lines(day).into_iter().chain(self.lines(&next)) {
            let conv = field(&ln, "conv").to_string();
            if !grouped.contains_key(&conv) {
                order.push(conv.clone());
            }
            grouped.entry(conv).or_default().push(ln);
        }
        let mut out = Vec::new();
        for conv in order {
            let rows = &grouped[&conv];
            if self.conv_day(&conv, field(&rows[0], "ts")) != day {
                continue;
            }
            let mut talk: Vec<Dt> = rows
                .iter()
                .filter(|r| field(r, "kind") != "close")
                .filter_map(|r| parse_ts(field(r, "ts")))
                .collect();
            if talk.is_empty() {
                talk = rows
                    .iter()
                    .filter_map(|r| parse_ts(field(r, "ts")))
                    .collect();
            }
            let (Some(started), Some(ended)) =
                (talk.iter().min().copied(), talk.iter().max().copied())
            else {
                continue;
            };
            out.push(ConvInfo {
                conv: conv.clone(),
                ch: field(&rows[0], "ch").to_string(),
                started,
                ended,
                closed: rows.iter().any(|r| field(r, "kind") == "close"),
            });
        }
        out.sort_by_key(|c| c.started);
        out
    }

    /// When anything was last said or done on either channel (close markers and bus lines do not count).
    pub fn last_turn_at(&self) -> Option<Dt> {
        {
            let last = self.last.lock();
            if last.scanned {
                return last.last;
            }
        }
        let mut newest: Option<Dt> = None;
        let days = self.days();
        for day in days.iter().rev().take(2) {
            for ln in self.lines(day) {
                if field(&ln, "kind") == "close" || !TALK.contains(&field(&ln, "ch")) {
                    continue;
                }
                if let Some(ts) = parse_ts(field(&ln, "ts")) {
                    if newest.is_none_or(|n| ts > n) {
                        newest = Some(ts);
                    }
                }
            }
            if newest.is_some() {
                break;
            }
        }
        let mut last = self.last.lock();
        if let Some(n) = newest {
            if last.last.is_none_or(|l| n > l) {
                last.last = Some(n);
            }
        }
        last.scanned = true;
        last.last
    }

    /// Lines strictly newer than `ts`, from the talk channels other than `exclude_ch`, of the render kinds, oldest
    /// first.
    pub fn lines_since<'a>(&self, ts: impl Into<Stamp<'a>>, exclude_ch: &str) -> Vec<Arc<Line>> {
        self.lines_since_kinds(ts, exclude_ch, &RENDER_KINDS)
    }

    /// [`Transcripts::lines_since`] with its own set of kinds.
    pub fn lines_since_kinds<'a>(
        &self,
        ts: impl Into<Stamp<'a>>,
        exclude_ch: &str,
        kinds: &[&str],
    ) -> Vec<Arc<Line>> {
        let Some(since) = stamp_ts(ts.into()) else {
            return Vec::new();
        };
        let (first, last) = (self.day_of(since), self.day_of(self.now()));
        let mut out = Vec::new();
        for day in self.days() {
            if first <= day && day <= last {
                for ln in self.lines(&day) {
                    let ch = field(&ln, "ch");
                    let ok = parse_ts(field(&ln, "ts")).is_some_and(|at| at > since)
                        && ch != exclude_ch
                        && TALK.contains(&ch)
                        && kinds.contains(&field(&ln, "kind"));
                    if ok {
                        out.push(ln);
                    }
                }
            }
        }
        out
    }

    /// Today in the text style, nothing excluded. See [`Transcripts::today_block_with`].
    pub fn today_block(&self, max_chars: i64) -> String {
        self.today_block_with(max_chars, &Today::default())
    }

    /// Today, both talk channels (never a bus line), oldest first, within `max_chars`. "" for an empty day.
    ///
    /// `exclude_conv` drops a conversation the caller already shows: all of it when `exclude_tail` is 0, or only its
    /// newest `exclude_tail` say/image lines (the turns a chat history keeps). `exclude` does the latter for several
    /// conversations at once ({conv: newest say/image lines to leave out}): a Telegram history read from the chat
    /// log can span a conversation a quiet spell closed and the one open now.
    ///
    /// Over budget, the NEWEST whole lines are kept under a fixed '(about N earlier lines today…)' header. The cut
    /// moves only when the budget overflows, and then back to KEEP_AFTER_CUT of it, so the block stays a stable
    /// prefix across many appends: prompt caching keeps working.
    pub fn today_block_with(&self, max_chars: i64, o: &Today) -> String {
        if !self.enabled || max_chars <= 0 {
            return String::new();
        }
        let day = self.day_of(o.now.unwrap_or_else(|| self.now()));
        let mut rows: Vec<Arc<Line>> = self
            .lines(&day)
            .into_iter()
            .filter(|ln| {
                o.kinds.iter().any(|k| k == field(ln, "kind")) && TALK.contains(&field(ln, "ch"))
            })
            .collect();
        let drop_tail = |rows: Vec<Arc<Line>>, conv: &str, tail: i64| -> Vec<Arc<Line>> {
            let turn_ids: Vec<usize> = rows
                .iter()
                .enumerate()
                .filter(|(_, ln)| {
                    field(ln, "conv") == conv && matches!(field(ln, "kind"), "say" | "image")
                })
                .map(|(i, _)| i)
                .collect();
            let from = turn_ids.len().saturating_sub(tail as usize);
            let drop: HashSet<usize> = turn_ids[from..].iter().copied().collect();
            rows.into_iter()
                .enumerate()
                .filter(|(i, _)| !drop.contains(i))
                .map(|(_, ln)| ln)
                .collect()
        };
        if let Some(conv) = o.exclude_conv.as_deref().filter(|c| !c.is_empty()) {
            if o.exclude_tail <= 0 {
                rows.retain(|ln| field(ln, "conv") != conv);
            } else {
                rows = drop_tail(rows, conv, o.exclude_tail);
            }
        }
        for (conv, tail) in &o.exclude {
            if *tail <= 0 {
                continue;
            }
            rows = drop_tail(rows, conv, *tail);
        }
        let rendered: Vec<String> = rows.iter().map(|ln| render_line(ln, &o.style)).collect();
        if rendered.is_empty() {
            return String::new();
        }
        let lens: Vec<i64> = rendered.iter().map(|r| py::len(r) as i64).collect();
        let total: i64 = lens.iter().map(|l| l + 1).sum::<i64>() - 1;
        if total <= max_chars {
            return rendered.join("\n");
        }
        let header_room = py::len(&Self::header(1_000_000)) as i64 + 1;
        let room = (max_chars - header_room).max(0);
        let keep_room = (room as f64 * KEEP_AFTER_CUT) as i64;
        let (mut start, mut size) = (0usize, 0i64); // size = len("\n".join(rendered[start:i + 1]))
        for i in 0..rendered.len() {
            size += lens[i] + if i > start { 1 } else { 0 };
            if size > room {
                // Overflow: cut from the front until the tail fits in keep_room, so the next many appends extend
                // this block instead of shifting it (the hysteresis that keeps the cache warm).
                while start < i && size > keep_room {
                    size -= lens[start] + 1;
                    start += 1;
                }
                if size > room {
                    // one line alone is over the whole budget
                    start = i + 1;
                    size = 0;
                }
            }
        }
        let kept = &rendered[start..];
        if kept.is_empty() {
            return String::new();
        }
        let block = format!("{}\n{}", Self::header(start), kept.join("\n"));
        if py::len(&block) as i64 <= max_chars {
            block
        } else {
            String::new()
        }
    }

    fn header(dropped: usize) -> String {
        let bucket = HEADER_BUCKET.max(dropped.div_ceil(HEADER_BUCKET) * HEADER_BUCKET);
        format!("(about {bucket} earlier lines today: memory_search finds them)")
    }

    /// One whole day for the nightly dream: 'YYYY-MM-DD HH:MM spoken|texted|sent Owner/eve[ (tool)]: text', say, tool
    /// and image lines, oldest first, each whole. The date is the line's own calendar date: a line at 00:08 stays in
    /// the day before (the day runs 04:00 to 04:00) but says the date it was said, so the dream's journal never files
    /// it under the wrong date (days/2026-09-23.md did, for 2026-09-24 00:08). Over budget, the NEWEST lines are kept
    /// under a '(N earlier lines omitted)' header. "" for a day with nothing said.
    ///
    /// Outside words never reach the dream (v3 safety review: a page's or a task's words became profile facts and
    /// records). A tool line whose result came from outside eve (tool_gate.taints, a task's sentence) and a line kept
    /// as outside words (`out`: a reply written after a read) keep their place and their speaker, with the words
    /// replaced by OUTSIDE_LEFT_OUT; `outside=true` keeps them whole (never for the dream).
    pub fn day_text(&self, day: &str, max_chars: i64, outside: bool) -> String {
        let mut rows: Vec<String> = Vec::new();
        for ln in self.lines(day) {
            if !RENDER_KINDS.contains(&field(&ln, "kind")) || field(&ln, "text").is_empty() {
                continue;
            }
            let Some(at) = parse_ts(field(&ln, "ts")) else {
                continue;
            };
            let text = if outside || !outside_words(&ln) {
                py::flat(field(&ln, "text"))
            } else {
                OUTSIDE_LEFT_OUT.to_string()
            };
            rows.push(format!(
                "{} {} {}: {}",
                at.format("%Y-%m-%d %H:%M"),
                mode(field(&ln, "ch")),
                speaker(&ln),
                text
            ));
        }
        if rows.is_empty() || max_chars <= 0 {
            return String::new();
        }
        let size: i64 = rows.iter().map(|r| py::len(r) as i64 + 1).sum::<i64>() - 1;
        if size <= max_chars {
            return rows.join("\n");
        }
        let room =
            max_chars - py::len(&format!("({} earlier lines omitted)", rows.len())) as i64 - 1;
        if room < 0 {
            return String::new();
        }
        let mut kept: Vec<&String> = Vec::new();
        let mut used: i64 = 0;
        for r in rows.iter().rev() {
            let l = py::len(r) as i64;
            if used + l + if kept.is_empty() { 0 } else { 1 } > room {
                break;
            }
            kept.push(r);
            used += l + if kept.len() > 1 { 1 } else { 0 };
        }
        kept.reverse();
        let mut out = format!("({} earlier lines omitted)", rows.len() - kept.len());
        if !kept.is_empty() {
            out.push('\n');
            out.push_str(
                &kept
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        out
    }

    /// One day between two HH:MM times (inclusive; times after midnight count as late), oldest first.
    pub fn read(&self, day: &str, start: &str, end: &str, max_chars: i64) -> String {
        if !DAY.is_match(day) {
            return String::new();
        }
        let (lo, hi) = (minute(start, 0), minute(end, 24 * 60 - 1));
        let mut out: Vec<String> = Vec::new();
        let mut size: i64 = 0;
        for ln in self.lines(day) {
            if field(&ln, "kind") == "close" {
                continue;
            }
            let Some(at) = parse_ts(field(&ln, "ts")) else {
                continue;
            };
            let hhmm = at.format("%H:%M").to_string();
            let m = minute(&hhmm, 0);
            if !(lo <= m && m <= hi) {
                continue;
            }
            let row = render_line(&ln, "text");
            let l = py::len(&row) as i64;
            if size + l + 1 > max_chars - 60 {
                out.push(format!("(more after this: read again from {hhmm})"));
                break;
            }
            out.push(row);
            size += l + 1;
        }
        out.join("\n")
    }

    // -- search --------------------------------------------------------------------------------------------------------

    /// Days that have markdown to search: meeting notes, and the archive's session notes and day files.
    fn doc_days(&self, channels: &[&str]) -> BTreeSet<String> {
        let mut days = BTreeSet::new();
        let listing = |folder: &Path| -> Vec<String> {
            fs::read_dir(folder)
                .map(|rd| {
                    rd.flatten()
                        .filter_map(|e| e.file_name().to_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        };
        if channels.contains(&MEETING) {
            days.extend(
                listing(&self.root.join(MEETINGS_SUBDIR))
                    .into_iter()
                    .filter(|n| DAY.is_match(n)),
            );
        }
        if let (true, Some(archive)) = (channels.contains(&NOTE), &self.archive) {
            days.extend(
                listing(&archive.join("sessions"))
                    .into_iter()
                    .filter(|n| DAY.is_match(n)),
            );
            days.extend(
                listing(archive)
                    .into_iter()
                    .filter_map(|n| DAY_NOTE.captures(&n).map(|m| m[1].to_string())),
            );
        }
        days
    }

    /// A markdown note's content lines: no frontmatter, comments, quotes or blank lines; bullets bare.
    fn parse_doc(raw: &str) -> Vec<String> {
        let body = FRONTMATTER.replacen(raw, 1, "");
        let mut rows = Vec::new();
        for r in py::splitlines(&body) {
            let r = py::strip(r);
            if r.is_empty() || r.starts_with("<!--") || r.starts_with('>') || r == "---" {
                continue;
            }
            rows.push(py::flat(&BULLET.replacen(r, 1, "")));
        }
        rows
    }

    /// (channel, HH:MM or "", content lines) for each markdown note of one calendar day.
    fn docs(&self, day: &str, channels: &[&str]) -> Vec<(&'static str, String, Arc<Vec<String>>)> {
        let md = |folder: &Path| -> Vec<PathBuf> {
            let Ok(rd) = fs::read_dir(folder) else {
                return Vec::new();
            };
            let mut names: Vec<String> = rd
                .flatten()
                .filter_map(|e| e.file_name().to_str().map(String::from))
                .collect();
            names.sort();
            names
                .into_iter()
                .filter(|n| n.ends_with(".md"))
                .map(|n| folder.join(n))
                .collect()
        };
        let mut files: Vec<(&'static str, PathBuf)> = Vec::new();
        if channels.contains(&MEETING) {
            files.extend(
                md(&self.root.join(MEETINGS_SUBDIR).join(day))
                    .into_iter()
                    .map(|f| (MEETING, f)),
            );
        }
        if let (true, Some(archive)) = (channels.contains(&NOTE), &self.archive) {
            files.extend(
                md(&archive.join("sessions").join(day))
                    .into_iter()
                    .map(|f| (NOTE, f)),
            );
            let prefix = format!("{day}-");
            files.extend(
                md(archive)
                    .into_iter()
                    .filter(|f| {
                        f.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with(&prefix))
                    })
                    .map(|f| (NOTE, f)),
            );
        }
        let mut out = Vec::new();
        for (ch, path) in files {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            // a day file has no time
            let hhmm = if DAY_NOTE.is_match(&name) {
                String::new()
            } else {
                NOTE_TIME
                    .captures(&name)
                    .map(|m| format!("{}:{}", &m[1], &m[2]))
                    .unwrap_or_default()
            };
            let doc = match self.cached(&path, |raw| Parsed::Doc(Arc::new(Self::parse_doc(raw)))) {
                Some(Parsed::Doc(d)) => d,
                _ => Arc::new(Vec::new()),
            };
            out.push((ch, hhmm, doc));
        }
        out
    }

    /// A keyword search with every default. See [`Transcripts::search_with`].
    pub fn search(&self, query: &str) -> Value {
        self.search_with(query, &Search::default())
    }

    /// Dated hits: {"ok", "hits": [{day, time, ch, who, text, before, after}], "truncated"}. Three sources: the
    /// transcripts (ch voice|telegram|bus), meeting notes (ch 'meeting') and the archive's notes (ch 'note'). The
    /// days searched are the last `days_back`, or `since` to `until` (year-month-day, inclusive) when given. The JSON
    /// result stays within 3000 chars and the search within 1.5 s; either cut sets truncated.
    ///
    /// With a ranker attached (EVE_MEMORY_HYBRID), the transcript hits are its best rounds, best first ("ranked":
    /// true), and the notes follow by keyword; when it cannot rank, this is the keyword search. Keyword search: every
    /// term must match, a quoted phrase as written, newest day first. `loose` asks for at least half the telling
    /// words instead, the hits with the most of them first (memory_index.terms).
    pub fn search_with(&self, query: &str, o: &Search) -> Value {
        let Some(pred) = match_query(query) else {
            return json!({"ok": false, "error": "The query has no word to search for.", "hits": [], "truncated": false});
        };
        if !self.enabled {
            return json!({"ok": true, "hits": [], "truncated": false});
        }
        let days_back = if o.days_back < 1 {
            SEARCH_DAYS_BACK
        } else {
            o.days_back.min(3650)
        };
        let limit = (if o.limit == 0 { SEARCH_LIMIT } else { o.limit }).clamp(1, 50) as usize;
        let docs = [MEETING, NOTE];
        let all: Vec<&str> = CHANNELS.iter().chain(docs.iter()).copied().collect();
        let channels: Vec<&str> = if all.contains(&o.channel.as_str()) {
            vec![o.channel.as_str()]
        } else {
            all.clone()
        };
        let today = self.day_of(self.now());
        let lo = if DAY.is_match(&o.since) {
            o.since.clone()
        } else {
            let t = NaiveDate::parse_from_str(&today, "%Y-%m-%d").expect("a formatted day parses");
            (t - Duration::days(days_back))
                .format("%Y-%m-%d")
                .to_string()
        };
        let hi = if DAY.is_match(&o.until) {
            o.until.clone().min(today.clone())
        } else {
            today.clone()
        };
        let ranker = self.ranker();
        if let (Some(ranker), true) = (ranker, channels.iter().any(|c| CHANNELS.contains(c))) {
            let talk: Vec<String> = channels
                .iter()
                .filter(|c| CHANNELS.contains(c))
                .map(|c| c.to_string())
                .collect();
            let narrowed = if talk.len() == CHANNELS.len() {
                None
            } else {
                Some(talk.as_slice())
            };
            let got = match ranker.search(
                query,
                &lo,
                &hi,
                limit.min(crate::memory_index::TOP_K),
                narrowed,
            ) {
                Ok(g) => g,
                Err(e) => {
                    warn!("transcripts: ranked search failed ({})", error_name(&e));
                    None
                }
            };
            if let Some(got) = got {
                let mut hits: Vec<Value> = got
                    .get("hits")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let mut truncated = false;
                if channels.iter().any(|c| docs.contains(c)) {
                    let doc_channels: Vec<&str> = channels
                        .iter()
                        .filter(|c| docs.contains(c))
                        .copied()
                        .collect();
                    let (more, t) = self.keyword(
                        &*pred,
                        &doc_channels,
                        &lo,
                        &hi,
                        limit.saturating_sub(hits.len()),
                        false,
                    );
                    truncated = t;
                    hits.extend(more);
                }
                return capped(
                    json!({"ok": true, "hits": hits, "truncated": truncated, "ranked": true}),
                );
            }
        }
        if o.loose {
            let words = crate::memory_index::terms(query);
            if !words.is_empty() {
                let need = words.len().div_ceil(2).max(1);
                let present = |text: &str| -> Vec<&String> {
                    let hay = py::casefold(&py::flat(text));
                    words.iter().filter(|w| hay.contains(w.as_str())).collect()
                };
                let (mut found, truncated) = self.keyword(
                    &|t: &str| present(t).len() >= need,
                    &channels,
                    &lo,
                    &hi,
                    FIND_CAP,
                    true,
                );
                // The rarer a word in what matched, the more a line holding it counts (an IDF over the hits); among
                // equals the newest stays first.
                let hit_text = |h: &Value| {
                    h.get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string()
                };
                let n = found.len();
                let weight: HashMap<&String, f64> = words
                    .iter()
                    .map(|w| {
                        let df = found
                            .iter()
                            .filter(|h| present(&hit_text(h)).contains(&w))
                            .count();
                        (
                            w,
                            if df > 0 {
                                (1.0 + n as f64 / df as f64).ln()
                            } else {
                                0.0
                            },
                        )
                    })
                    .collect();
                let score = |h: &Value| -> f64 {
                    -present(&hit_text(h))
                        .iter()
                        .map(|w| weight[*w])
                        .sum::<f64>()
                };
                let mut keyed: Vec<(f64, Value)> =
                    found.drain(..).map(|h| (score(&h), h)).collect();
                keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                let hits: Vec<Value> = keyed.into_iter().take(limit).map(|(_, h)| h).collect();
                return capped(
                    json!({"ok": true, "hits": hits, "truncated": truncated || n > limit}),
                );
            }
        }
        let (hits, truncated) = self.keyword(&*pred, &channels, &lo, &hi, limit, false);
        capped(json!({"ok": true, "hits": hits, "truncated": truncated}))
    }

    /// Hits `pred` matches between days `lo` and `hi`, newest day first, stopping at `limit`; with `every`, every
    /// day is read (within the time budget) before the cut.
    fn keyword(
        &self,
        pred: &dyn Fn(&str) -> bool,
        channels: &[&str],
        lo: &str,
        hi: &str,
        limit: usize,
        every: bool,
    ) -> (Vec<Value>, bool) {
        if limit == 0 {
            return (Vec::new(), false);
        }
        let t_days: BTreeSet<String> = if channels.iter().any(|c| CHANNELS.contains(c)) {
            self.days().into_iter().collect()
        } else {
            BTreeSet::new()
        };
        let d_days = self.doc_days(channels);
        let started = Instant::now();
        let mut hits: Vec<Value> = Vec::new();
        let mut truncated = false;
        let mut all: Vec<&String> = t_days
            .union(&d_days)
            .filter(|d| lo <= d.as_str() && d.as_str() <= hi)
            .collect();
        all.sort_by(|a, b| b.cmp(a));
        for day in all {
            if started.elapsed().as_secs_f64() > self.search_budget_secs {
                truncated = true;
                break;
            }
            let mut found: Vec<(i64, Value)> = Vec::new();
            if t_days.contains(day) {
                let rows = self.lines(day);
                for i in (0..rows.len()).rev() {
                    let ln = &rows[i];
                    if field(ln, "kind") == "close"
                        || !channels.contains(&field(ln, "ch"))
                        || !pred(field(ln, "text"))
                    {
                        continue;
                    }
                    let at = parse_ts(field(ln, "ts"));
                    let hhmm = at
                        .map(|a| a.format("%H:%M").to_string())
                        .unwrap_or_default();
                    let key = if at.is_some() { minute(&hhmm, 0) } else { 0 };
                    found.push((
                        key,
                        json!({
                            "day": day, "time": hhmm, "ch": field(ln, "ch"), "who": field(ln, "who"),
                            "text": py::clip(&py::flat(field(ln, "text")), SEARCH_HIT_CHARS),
                            "before": neighbour(&rows, i, -1), "after": neighbour(&rows, i, 1),
                        }),
                    ));
                }
            }
            if d_days.contains(day) {
                for (ch, hhmm, doc) in self.docs(day, channels) {
                    for (i, text) in doc.iter().enumerate() {
                        if pred(text) {
                            found.push((
                                minute(&hhmm, -1),
                                json!({
                                    "day": day, "time": hhmm, "ch": ch, "who": "notes",
                                    "text": py::clip(text, SEARCH_HIT_CHARS),
                                    "before": if i > 0 { py::clip(&doc[i - 1], SEARCH_NEIGHBOUR_CHARS) } else { String::new() },
                                    "after": if i + 1 < doc.len() { py::clip(&doc[i + 1], SEARCH_NEIGHBOUR_CHARS) } else { String::new() },
                                }),
                            ));
                        }
                    }
                }
            }
            found.sort_by_key(|f| std::cmp::Reverse(f.0));
            hits.extend(found.into_iter().map(|(_, h)| h));
            if !every && hits.len() >= limit {
                truncated = truncated || hits.len() > limit;
                hits.truncate(limit);
                break;
            }
        }
        if every && hits.len() > limit {
            truncated = true;
            hits.truncate(limit);
        }
        (hits, truncated)
    }

    // -- forget (W11) --------------------------------------------------------------------------------------------------

    /// Every line on any day that `query` matches (the search rule), oldest first, as the line plus its "day". Lines
    /// already forgotten are not matched again. [] for a query with no usable term.
    pub fn find(&self, query: &str) -> Vec<Line> {
        let Some(pred) = match_query(query) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for day in self.days() {
            for ln in self.lines(&day) {
                let text = field(&ln, "text");
                if field(&ln, "kind") != "close" && text != FORGOTTEN && pred(text) {
                    let mut hit = ln.as_ref().clone();
                    hit.insert("day".into(), json!(day));
                    out.push(hit);
                    if out.len() >= FIND_CAP {
                        return out;
                    }
                }
            }
        }
        out
    }

    /// Forget: every matching line keeps its envelope (time, channel, conversation, speaker) and its text becomes
    /// "(forgotten)", and so does eve's next line in that conversation when the match was the owner's (the answer can
    /// carry the fact). A reply whose Sources (`src`, memory_sources) quote the words loses those sources and keeps
    /// its text; a forgotten line keeps none. A torn line that matches is dropped. Each changed file is rewritten
    /// atomically (a 0600 temp file in the same folder, fsync, rename) under the writers' lock, so a concurrent
    /// append is never lost. `dry_run` counts and writes nothing. → lines forgotten.
    pub fn redact(&self, query: &str, dry_run: bool) -> usize {
        let Some(pred) = match_query(query) else {
            return 0;
        };
        if !self.enabled {
            return 0;
        }
        let mut total = 0;
        for day in self.days() {
            let path = self.path_of(&day);
            let _w = self.write_lock.lock();
            self.flock(libc::LOCK_EX);
            match self.redact_file(&path, &*pred, dry_run) {
                Ok(n) => total += n,
                Err(e) => self.fail("forget", &e),
            }
            self.flock(libc::LOCK_UN);
            self.invalidate(&path);
        }
        if total > 0 && !dry_run {
            info!("transcripts: forgot {} lines", total);
        }
        total
    }

    fn redact_file(
        &self,
        path: &Path,
        pred: &dyn Fn(&str) -> bool,
        dry_run: bool,
    ) -> io::Result<usize> {
        let raw = match fs::read(path) {
            Ok(r) => r,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };
        let mut changed = 0;
        let mut out: Vec<Vec<u8>> = Vec::new();
        let mut reply_to = String::new(); // the conversation whose owner line was just forgotten: eve's answer
        for chunk in raw.split(|b| *b == b'\n') {
            // bytes.strip(): ASCII whitespace, vertical tab included
            if chunk.iter().all(|b| b" \t\n\r\x0b\x0c".contains(b)) {
                continue;
            }
            let text = String::from_utf8_lossy(chunk);
            let Some(mut obj) = parse_line(&text) else {
                if pred(&text) {
                    // a torn line holding the words: it goes entirely
                    changed += 1;
                    continue;
                }
                out.push(chunk.to_vec());
                continue;
            };
            let conv = obj
                .get("conv")
                .map(eve_memory_sources::py_str_or_empty)
                .unwrap_or_default();
            let who = field(&obj, "who").to_string();
            let live = field(&obj, "kind") != "close";
            let hit = live && field(&obj, "text") != FORGOTTEN && pred(field(&obj, "text"));
            // eve's next line in that conversation answered the forgotten one ("Okay, no driving then."): it goes
            let answer = !hit
                && !reply_to.is_empty()
                && conv == reply_to
                && who == "eve"
                && live
                && field(&obj, "text") != FORGOTTEN;
            if conv == reply_to && (who != "owner" || !live) {
                reply_to.clear();
            }
            if hit && who == "owner" && !conv.is_empty() {
                reply_to = conv.clone();
            }
            if hit || answer {
                obj.insert("text".into(), json!(FORGOTTEN));
                obj.shift_remove("src"); // a forgotten reply keeps no sources (memory_sources)
                out.push(py::dumps(&Value::Object(obj)).into_bytes());
                changed += 1;
            } else if live
                && eve_memory_sources::mentions(obj.get("src").unwrap_or(&Value::Null), pred)
            {
                // a reply's Sources quoted the forgotten words: those sources go, the reply stays
                let kept: Vec<Value> = obj
                    .get("src")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter(|x| {
                                !matches!(x, Value::Object(o)
                                    if pred(&eve_memory_sources::py_str_or_empty(o.get("text").unwrap_or(&Value::Null))))
                            })
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                if kept.is_empty() {
                    obj.shift_remove("src");
                } else {
                    obj.insert("src".into(), Value::Array(kept));
                }
                out.push(py::dumps(&Value::Object(obj)).into_bytes());
                changed += 1;
            } else {
                out.push(chunk.to_vec());
            }
        }
        if changed == 0 || dry_run {
            return Ok(changed);
        }
        let mut data = Vec::new();
        for c in out {
            data.extend_from_slice(&c);
            data.push(b'\n');
        }
        atomic_write(path, &data)?;
        Ok(changed)
    }
}

/// `_minute`: minutes since the day's start hour, so 01:00 sorts after 23:00.
fn minute(hhmm: &str, default: i64) -> i64 {
    let Some(m) = HHMM.captures(py::strip(hhmm)) else {
        return default;
    };
    let h: i64 = py::int(&m[1]).unwrap_or(0) % 24;
    let mi: i64 = py::int(&m[2]).unwrap_or(0).min(59);
    (h - DAY_START_HOUR).rem_euclid(24) * 60 + mi
}

fn neighbour(rows: &[Arc<Line>], i: usize, step: isize) -> String {
    let conv = field(&rows[i], "conv");
    let mut j = i as isize + step;
    while j >= 0 && (j as usize) < rows.len() {
        let ln = &rows[j as usize];
        if field(ln, "conv") == conv
            && field(ln, "kind") != "close"
            && !field(ln, "text").is_empty()
        {
            return py::clip(
                &format!("{}: {}", speaker(ln), py::flat(field(ln, "text"))),
                SEARCH_NEIGHBOUR_CHARS,
            );
        }
        j += step;
    }
    String::new()
}

fn capped(mut result: Value) -> Value {
    loop {
        let len = py::len(&py::dumps(&result));
        let hits = result
            .get_mut("hits")
            .and_then(Value::as_array_mut)
            .expect("a result has hits");
        if hits.is_empty() || len <= SEARCH_RESULT_CHARS {
            return result;
        }
        hits.pop();
        result["truncated"] = json!(true);
    }
}

fn error_name(e: &anyhow::Error) -> String {
    if let Some(io) = e.downcast_ref::<io::Error>() {
        return py::os_error_name(io).to_string();
    }
    if e.downcast_ref::<rusqlite::Error>().is_some() {
        return "OperationalError".into();
    }
    "Exception".into()
}

fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("transcript");
    let tmp = path.with_file_name(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        hex::encode(rand::random::<[u8; 4]>())
    ));
    let written = (|| -> io::Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&tmp)?;
        f.set_permissions(fs::Permissions::from_mode(0o600))?;
        f.write_all(data)?;
        // SAFETY: the descriptor is open; fsync takes only an fd (plain fsync, as os.fsync).
        if unsafe { libc::fsync(f.as_raw_fd()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        drop(f);
        fs::rename(&tmp, path)
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if let Some(parent) = path.parent() {
        if let Ok(d) = File::open(parent) {
            // SAFETY: as above.
            unsafe {
                libc::fsync(d.as_raw_fd());
            }
        }
    }
    Ok(())
}

impl crate::memory_index::DaySource for Transcripts {
    fn enabled(&self) -> bool {
        self.enabled
    }
    fn days(&self) -> Vec<String> {
        Transcripts::days(self)
    }
    fn path_of(&self, day: &str) -> PathBuf {
        Transcripts::path_of(self, day)
    }
    fn lines(&self, day: &str) -> Vec<Arc<Line>> {
        Transcripts::lines(self, day)
    }
}
