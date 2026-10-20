//! Records: what eve knows about its owner, as files the owner can read and edit.
//!
//! Memory is three stores (owner, 2026-09-23: "simplify the memory system — too many stores"): the transcripts
//! (every word, transcripts), mem0 (meaning search, mem0_memory) and these records, the truth. The shape follows
//! the memory Instinct (the iMessage assistant) was found to use — reverse-engineered by Dhravya Shah, 2026-09-20 —
//! because it has one property the owner cares about: **the brains read; they never write.**
//!
//! ```text
//! <memory>/records/                its own git repository, sealed to this computer
//!     <id>.md                      one typed record per thing: a preference, a person, a project, a place.
//!                                  Frontmatter: id, type, aliases. Body: dated facts, [[id]] links.
//!     profile.md                   the one-pager every conversation starts with: life context, how much
//!                                  to ask before acting, how to talk, and an index of the records.
//!     starred.md                   what the owner said to remember, in their words, one dated line each.
//!     hidden.md                    what the owner said not to bring up again, one dated line each.
//!     days/<day>.md                the dream journal: what happened, what eve learned, what is still
//!                                  open, what it corrected.
//! ```
//!
//! Who writes, and nothing else does:
//!
//! 1. **"Remember that"** (voice, Telegram, the notes widget) → [`star`] appends one line to starred.md. The
//!    owner's voice is the human promoting a claim, so it counts on the next message: [`profile`] puts every star
//!    the page has not absorbed yet on top of it, verbatim.
//! 2. **The nightly dream** (dream) → [`reconcile_day`] reads one whole day of transcript beside the current
//!    records, profile and stars, and returns — in one model call — the records that change (whole), the profile
//!    as it should read now, the ids the owner said to forget, and the day's journal. Where the day contradicts a
//!    record, the record gets a dated correction and the journal says so. [`consolidate`] then merges the same
//!    thing filed under two ids and dates superseded facts. The model can shorten, merge and correct; owner edits
//!    survive because the current files are always its input.
//! 3. **Forget** (owner-confirmed, memory) → [`forget_lines`] removes matching lines everywhere here, and
//!    [`squash_history`] makes git forget them too. A forget that lands while a dream's model call is out would be
//!    undone by that dream's write (the model answered from the words before the forget), so every forget moves a
//!    generation (`.forgets`) and a dream writes only when the generation it pinned before reading the day has not
//!    moved ([`forgets_pinned`]; proved in verification/Eve/Forget.lean).
//!
//! **Don't mention this again** (the app's Memory card) → [`hide`] appends the owner's words to hidden.md. Nothing
//! is deleted: the profile and the searches leave out every line the words match ([`hidden_match`], by the one
//! matching rule, transcripts.match), and [`unhide`] brings them back (docs/stackchan/memory.md).
//!
//! Git is how nothing is lost otherwise: every dream is one commit, so a wrong edit is one revert away. The
//! repository root is the records folder itself — never the memory root, which holds the transcripts, and a
//! spoken word must never reach a git object. [`seal`] locks it to this computer.
//!
//! Search is grep (aliases are the index; the profile lists them so a brain knows what it can look up), plus
//! mem0's meaning search beside it, each hit dated so a newer fact can win a tie.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use chrono::{Local, NaiveDate, NaiveDateTime};
use indexmap::IndexMap;
use parking_lot::{Mutex, ReentrantMutex, ReentrantMutexGuard};
use regex::Regex;
use serde_json::{json, Map, Value};
use tracing::{info, warn};

use eve_transcripts::py as tpy;
use eve_transcripts::transcripts::Pred;

use crate::py::{self, iter_of, str_of, str_or_empty};
use crate::recall::RecallConfig;

pub const DEFAULT_MODEL: &str = "gpt-6-luna";
pub const DEFAULT_CONSOLIDATION_MODEL: &str = "gpt-6-luna";
/// One model call; the dream runs it on a worker thread.
pub const RECONCILE_TIMEOUT_SECS: f64 = 180.0;
/// Records whole + profile + journal; 4000 truncated busy days.
pub const MAX_OUTPUT_TOKENS: u32 = 16000;
pub const TYPES: [&str; 6] = ["preference", "person", "organization", "project", "place", "routine"];
pub const PROFILE_ID: &str = "profile";
pub const STARRED_FILE: &str = "starred.md";
pub const HIDDEN_FILE: &str = "hidden.md";
/// One hidden line: a fact, a sentence said, a few words.
pub const MAX_HIDDEN_CHARS: usize = 500;
pub const DAYS_DIR: &str = "days";
/// ~1.5k tokens: the one-pager stays one page.
pub const MAX_PROFILE_CHARS: usize = 6000;
/// The Live voice front has no tools, so no index, and less room.
pub const MAX_VOICE_PROFILE_CHARS: usize = 3000;
/// What stars() returns by default.
pub const MAX_STARS: usize = 40;
/// The stars a dream reads (the newest).
pub const MAX_STARS_READ: usize = 200;
pub const MAX_RECORD_CHARS: usize = 4000;
pub const MAX_FACTS: usize = 40;
pub const MAX_ALIASES: usize = 12;
pub const MAX_HITS: usize = 8;
pub const MAX_HIT_CHARS: usize = 200;
/// Above this one call cannot hold every record: skip, log once.
pub const MAX_CONSOLIDATE_CHARS: usize = 120_000;
/// A star at 01:00 belongs to the evening before, as a transcript does.
pub const DAY_START_HOUR: i64 = 4;
/// What a forgotten journal title reads.
pub const FORGOTTEN: &str = "(forgotten)";
pub const INDEX_HEADING: &str = "## Records you can search or read by id";
pub const FRESH_STARS_HEADING: &str = "## They asked you to remember (since the page below was written)";
pub const GIT_TIMEOUT_SECS: f64 = 30.0;
pub const GC_TIMEOUT_SECS: f64 = 180.0;

/// Python's `\s` (str patterns): Unicode whitespace plus the four ASCII separators.
const W: &str = r"[\s\x1c-\x1f]";

fn re(pattern: &str) -> Regex {
    Regex::new(&pattern.replace("{W}", W)).expect("records: a fixed pattern compiles")
}

static ID: LazyLock<Regex> = LazyLock::new(|| re(r"^[a-z0-9][a-z0-9-]{0,63}\n?\z"));
static DAY: LazyLock<Regex> = LazyLock::new(|| re(r"^\d{4}-\d{2}-\d{2}\n?\z"));
static DAY_FILE: LazyLock<Regex> = LazyLock::new(|| re(r"^(\d{4}-\d{2}-\d{2})\.md\n?\z"));
static LINK: LazyLock<Regex> = LazyLock::new(|| re(r"\[\[([a-z0-9-]+)\]\]"));
static WORD: LazyLock<Regex> = LazyLock::new(|| re(r"[a-z0-9]+"));
static FRONTMATTER: LazyLock<Regex> = LazyLock::new(|| re(r"(?s)\A---\n.*?\n---[ \t]*(?:\n|\z)"));
static STAR_LINE: LazyLock<Regex> = LazyLock::new(|| re(r"^{W}*[-*]{W}+(?:★{W}*)?(.+?){W}*\z"));
static STAR_DATE: LazyLock<Regex> = LazyLock::new(|| re(r"^(.*?){W}*\((\d{4}-\d{2}-\d{2})\)\n?\z"));
static UPDATED: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^updated:{W}*(\d{4}-\d{2}-\d{2}){W}*$"));
static RECORD: LazyLock<Regex> = LazyLock::new(|| re(r"(?s)\A---\n(.*?)\n---[ \t]*(?:\n|\z)(.*)"));
static FACT_ITEM: LazyLock<Regex> = LazyLock::new(|| re(r"^{W}*[-*]{W}+(.+?){W}*\z"));
static FACT_BULLET: LazyLock<Regex> = LazyLock::new(|| re(r"^{W}*[-*]{W}+[^\s\x1c-\x1f]"));
static TITLE: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^title:{W}*(.+?){W}*$"));
static HEADING1: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^#{W}+(.+?){W}*$"));

// ---- two switches over what a fact line carries (memory plan items 2 and 3, 2026-10-08) -----------------------
//
// EVE_MEMORY_VALIDITY=1: a fact line carries said / valid_from / valid_to. The dream closes a superseded fact by
// setting valid_to instead of writing a prose correction, and rewrites a plan whose date has passed into the past
// tense. profile() leaves closed facts out; search labels them "(until YYYY-MM-DD)". A night may close at most
// MAX_CLOSES_PER_NIGHT facts (a valid_to newly set, whatever its date). Past that, the night is still written, so a
// busy day is never stuck and never dreamt out of order: the closures over the budget are held open and written to
// the day's journal as plain dated corrections instead.
//
// EVE_MEMORY_SOURCES=1: every fact the dream writes carries src, "<day> <HH:MM>" of the transcript line it came
// from (the line's own date and time, as the dream reads them, and a ref memory_read accepts). Search shows it, and a
// forget that redacts a transcript line also forgets every fact pointing at it (memory.Memory._follow_sources).
//
// Both off by default. The meta goes at the end of the line, in braces: "- Rent is $1,180 a month. {said: 2026-08-29;
// valid_from: 2026-09-01; src: 2026-08-29 18:05}". A line without it is a plain fact, read and written unchanged,
// so every record file written before the switches loads as it was.

pub const VALIDITY_ENV: &str = "EVE_MEMORY_VALIDITY";
pub const SOURCES_ENV: &str = "EVE_MEMORY_SOURCES";
/// The guard on closures. A busy real day (a move: address, rent, commute, landlord) closes four or five facts;
/// more than six in one night is more likely the model misreading the day than the owner's life changing that
/// much, and a wrong closure hides a true fact from the profile. Reconcile and consolidate of one night share the
/// budget.
pub const MAX_CLOSES_PER_NIGHT: i64 = 6;
pub const FACT_KEYS: [&str; 4] = ["said", "valid_from", "valid_to", "src"];
static META: LazyLock<Regex> =
    LazyLock::new(|| re(r"{W}*\{{W}*((?:said|valid_from|valid_to|src){W}*:[^{}]*)\}{W}*\z"));
static SRC: LazyLock<Regex> = LazyLock::new(|| re(r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}\n?\z"));

/// Each record switch's value when its variable is not set: the memory eval's holdout verdict
/// (tests/fixtures/memory_eval/VERDICT.md, 2026-10-08). test_memory_verdict keeps the two equal.
pub const DEFAULTS: [(&str, bool); 2] = [(VALIDITY_ENV, false), (SOURCES_ENV, false)];

// ---- the seams: what Python tests monkeypatch ----------------------------------------------------------------

/// What one `git` run returned (`subprocess.CompletedProcess` with text output).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitOutput {
    pub returncode: i32,
    pub stdout: String,
    pub stderr: String,
}

/// A git run that did not happen or did not finish. Each prints as the Python exception it stands for.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    /// eve lite, or /usr/bin/git without the developer tools (Python raises `OSError`).
    #[error("git is not usable here (toolchain.git_usable)")]
    Unusable,
    /// `EVE_PROFILE` is neither full nor lite (Python's `ProfileError` out of `toolchain.git_usable`).
    #[error("{0}")]
    Profile(String),
    #[error(transparent)]
    Io(#[from] io::Error),
    /// `subprocess.TimeoutExpired`.
    #[error("Command {0:?} timed out after {1} seconds")]
    Timeout(Vec<String>, f64),
}

impl GitError {
    /// `type(e).__name__` of the Python exception.
    pub fn py_name(&self) -> &'static str {
        match self {
            GitError::Unusable => "OSError",
            GitError::Profile(_) => "ProfileError",
            GitError::Io(e) => tpy::os_error_name(e),
            GitError::Timeout(..) => "TimeoutExpired",
        }
    }
}

/// Runs one git argv (`["git", "-C", <dir>, ...]`) in place of starting a process.
pub type GitRunner = Arc<dyn Fn(&[String]) -> Result<GitOutput, GitError> + Send + Sync>;

/// What the Python tests patch with monkeypatch, as one scoped value: the environment this crate reads (the two
/// switches, git's environment, PATH for finding git, EVE_PROFILE), `shutil.which("git")`, `toolchain.git_usable`,
/// the toolchain's locations and platform, and `subprocess.run`. Set for the current thread with [`with_seams`];
/// unset fields read the real thing. Tests run in parallel threads, so nothing here touches the process.
#[derive(Clone, Default)]
pub struct Seams {
    pub env: Option<HashMap<String, String>>,
    pub which_git: Option<Option<String>>,
    pub git_usable: Option<bool>,
    pub toolchain: Option<eve_toolchain::Toolchain>,
    pub run: Option<GitRunner>,
}

thread_local! {
    static SEAMS: RefCell<Option<Seams>> = const { RefCell::new(None) };
}

/// Run `f` with `seams` in force on this thread (nested calls restore the outer ones).
pub fn with_seams<R>(seams: Seams, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Seams>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let prev = self.0.take();
            SEAMS.with(|s| *s.borrow_mut() = prev);
        }
    }
    let prev = SEAMS.with(|s| s.borrow_mut().replace(seams));
    let _restore = Restore(prev);
    f()
}

fn seams() -> Seams {
    SEAMS.with(|s| s.borrow().clone()).unwrap_or_default()
}

/// `os.environ` as this crate sees it.
fn environ() -> HashMap<String, String> {
    seams().env.unwrap_or_else(py::environ)
}

/// On when set to 1/true/yes/on, off when set to anything else, and `DEFAULTS` when not set (or empty).
pub fn switch(name: &str, environ_: Option<&HashMap<String, String>>) -> bool {
    let raw = match environ_ {
        Some(e) => e.get(name).cloned(),
        None => environ().get(name).cloned(),
    };
    let raw = tpy::strip(raw.as_deref().unwrap_or("")).to_lowercase();
    if raw.is_empty() {
        return DEFAULTS.iter().find(|(k, _)| *k == name).map(|(_, v)| *v).unwrap_or(false);
    }
    matches!(raw.as_str(), "1" | "true" | "yes" | "on")
}

pub fn validity_on(environ_: Option<&HashMap<String, String>>) -> bool {
    switch(VALIDITY_ENV, environ_)
}

pub fn sources_on(environ_: Option<&HashMap<String, String>>) -> bool {
    switch(SOURCES_ENV, environ_)
}

// ---- facts ------------------------------------------------------------------------------------------------------

/// One fact line, split: its text and its meta (each "" when the line does not carry it).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fact {
    pub text: String,
    pub said: String,
    pub valid_from: String,
    pub valid_to: String,
    pub src: String,
}

impl Fact {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into(), ..Self::default() }
    }

    /// valid_to set and reached by `today` (YYYY-MM-DD). A fact with a valid_to still ahead is open.
    pub fn closed(&self, today: &str) -> bool {
        !self.valid_to.is_empty() && (today.is_empty() || self.valid_to.as_str() <= today)
    }

    fn get(&self, key: &str) -> &str {
        match key {
            "said" => &self.said,
            "valid_from" => &self.valid_from,
            "valid_to" => &self.valid_to,
            "src" => &self.src,
            _ => "",
        }
    }

    fn set(&mut self, key: &str, value: String) {
        match key {
            "said" => self.said = value,
            "valid_from" => self.valid_from = value,
            "valid_to" => self.valid_to = value,
            "src" => self.src = value,
            _ => {}
        }
    }
}

/// A fact line → Fact. A line with no trailing meta is all text: old files read as they were written.
pub fn parse_fact(line: &str) -> Fact {
    let Some(m) = META.captures(line) else {
        return Fact::new(line);
    };
    let whole = m.get(0).expect("group 0");
    let mut fact = Fact::new(tpy::rstrip(&line[..whole.start()]));
    for part in m[1].split(';') {
        let (key, sep, value) = match part.find(':') {
            Some(i) => (&part[..i], true, &part[i + 1..]),
            None => (part, false, ""),
        };
        let key = tpy::strip(key).to_lowercase();
        if sep && FACT_KEYS.contains(&key.as_str()) {
            fact.set(&key, tpy::flat(value));
        }
    }
    fact
}

/// Fact → its line. No meta, no braces: a plain fact renders back to exactly its text.
pub fn render_fact(fact: &Fact) -> String {
    let meta: Vec<String> =
        FACT_KEYS.iter().filter(|k| !fact.get(k).is_empty()).map(|k| format!("{k}: {}", fact.get(k))).collect();
    if meta.is_empty() {
        fact.text.clone()
    } else {
        format!("{} {{{}}}", fact.text, meta.join("; "))
    }
}

/// A fact line as a search hit shows it: a closed fact labelled "(until YYYY-MM-DD)" in front, the src after
/// ("(source YYYY-MM-DD HH:MM)"), the said date kept; braces never shown.
pub fn fact_display(line: &str) -> String {
    let f = parse_fact(line);
    if f.text == line {
        return line.to_string();
    }
    let mut out = if f.valid_to.is_empty() { f.text.clone() } else { format!("(until {}) {}", f.valid_to, f.text) };
    if !f.said.is_empty() && !f.text.contains(&f.said) {
        out += &format!(" (said {})", f.said);
    }
    if !f.src.is_empty() {
        out += &format!(" (source {})", f.src);
    }
    out
}

/// The memory's day of `when` (a day starts at 04:00).
fn memory_day(when: NaiveDateTime) -> String {
    (when - chrono::Duration::hours(DAY_START_HOUR)).format("%Y-%m-%d").to_string()
}

fn today() -> String {
    memory_day(Local::now().naive_local())
}

fn date_or_empty(value: Option<&Value>) -> String {
    let s = tpy::flat(&str_or_empty(value));
    if DAY.is_match(&s) { s } else { String::new() }
}

// ---- records on disk --------------------------------------------------------------------------

/// One typed record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Record {
    pub id: String,
    pub r#type: String,
    pub aliases: Vec<String>,
    pub facts: Vec<String>,
    pub updated: String,
}

impl Record {
    pub fn new(id: &str, rtype: &str, aliases: &[&str], facts: &[&str]) -> Self {
        Self {
            id: id.into(),
            r#type: rtype.into(),
            aliases: aliases.iter().map(|s| s.to_string()).collect(),
            facts: facts.iter().map(|s| s.to_string()).collect(),
            updated: String::new(),
        }
    }

    pub fn links(&self) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for fact in &self.facts {
            for c in LINK.captures_iter(fact) {
                let target = &c[1];
                if !seen.iter().any(|s| s == target) && target != self.id {
                    seen.push(target.to_string());
                }
            }
        }
        seen
    }
}

/// The records by id, in the order they were read (file name order).
pub type Records = IndexMap<String, Record>;

/// A record file → Record, or None when it is not one. Tolerant: the owner edits these by hand.
pub fn parse_record(text: &str) -> Option<Record> {
    if !text.starts_with("---") {
        return None;
    }
    let m = RECORD.captures(text)?;
    let (head, body) = (m.get(1).map_or("", |g| g.as_str()), m.get(2).map_or("", |g| g.as_str()));
    let mut meta: HashMap<String, String> = HashMap::new();
    for line in tpy::splitlines(head) {
        if let Some(i) = line.find(':') {
            meta.insert(tpy::strip(&line[..i]).to_lowercase(), tpy::strip(&line[i + 1..]).to_string());
        }
    }
    let rid = meta.get("id").cloned().unwrap_or_default();
    if !ID.is_match(&rid) {
        return None;
    }
    let mut rtype = tpy::strip(meta.get("type").map_or("", |s| s.as_str())).to_lowercase();
    if rtype.is_empty() {
        rtype = "preference".into();
    }
    let raw_aliases = tpy::strip(meta.get("aliases").map_or("", |s| s.as_str())).trim_matches(['[', ']']).to_string();
    let aliases = raw_aliases
        .split(',')
        .map(|a| tpy::strip(a).trim_matches(['"', '\'']).to_string())
        .filter(|a| !a.is_empty())
        .collect();
    let facts = tpy::splitlines(body)
        .into_iter()
        .filter_map(|ln| FACT_ITEM.captures(ln).map(|c| tpy::strip(&c[1]).to_string()))
        .collect();
    Some(Record { id: rid, r#type: rtype, aliases, facts, updated: meta.get("updated").cloned().unwrap_or_default() })
}

pub fn render_record(rec: &Record) -> String {
    let mut lines = vec![
        "---".to_string(),
        format!("id: {}", rec.id),
        format!("type: {}", rec.r#type),
        format!("aliases: [{}]", rec.aliases.join(", ")),
    ];
    if !rec.updated.is_empty() {
        lines.push(format!("updated: {}", rec.updated));
    }
    lines.push("---".into());
    lines.extend(rec.facts.iter().map(|f| format!("- {f}")));
    lines.join("\n") + "\n"
}

/// The records folder, and the root of its own git repository (never the memory root).
pub fn records_dir(cfg: &RecallConfig) -> PathBuf {
    cfg.records_dir()
}

/// `sorted(folder.glob("*.md"))`: every entry whose name ends in `.md`, hidden ones included, by name.
fn md_files(folder: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(".md"))
        .collect();
    names.sort();
    names.into_iter().map(|n| folder.join(n)).collect()
}

fn stem(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn name(path: &Path) -> String {
    path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

pub fn load_records(cfg: &RecallConfig) -> Records {
    let mut out = Records::new();
    let folder = records_dir(cfg);
    if !folder.is_dir() {
        return out;
    }
    for path in md_files(&folder) {
        let n = name(&path);
        if stem(&path) == PROFILE_ID || n == STARRED_FILE || n == HIDDEN_FILE {
            continue;
        }
        let Ok(text) = read_text(&path) else {
            continue;
        };
        if let Some(rec) = parse_record(&text) {
            if rec.id == stem(&path) {
                out.insert(rec.id.clone(), rec);
            }
        }
    }
    out
}

/// Whole-file write, atomic (a reader never sees half a record), private to this user.
fn write(path: &Path, text: &str) -> io::Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::Builder::new().prefix(&format!(".{}.", name(path))).suffix(".tmp").tempfile_in(parent)?;
    tmp.write_all(text.as_bytes())?;
    fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o600))?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// `Path.read_text(encoding="utf-8")`: strict UTF-8 (an invalid byte is an error, `InvalidData`), and universal
/// newlines, so a hand-edited file with `\r\n` or `\r` line ends reads with `\n` as Python reads it.
fn read_text(path: impl AsRef<Path>) -> io::Result<String> {
    let raw = fs::read_to_string(path)?;
    if !raw.contains('\r') {
        return Ok(raw);
    }
    Ok(raw.replace("\r\n", "\n").replace('\r', "\n"))
}

fn make_dir(folder: &Path) -> io::Result<()> {
    fs::create_dir_all(folder)?;
    let _ = fs::set_permissions(folder, fs::Permissions::from_mode(0o700));
    Ok(())
}

// One writer at a time: the dream (a worker thread), a star (a conversation), a forget (a tool call), and the CLI
// in another process. A thread lock inside the process, an flock on the folder across processes; the flock is
// taken once per thread (a second descriptor in the same process would wait on the first).
static WRITE_LOCK: ReentrantMutex<()> = ReentrantMutex::new(());
thread_local! {
    static HELD: Cell<usize> = const { Cell::new(0) };
}
// starred.md alone has its own short lock: a star comes from a live conversation and must never wait behind a
// dream's commit or a forget's gc. forget_lines takes it too while it rewrites starred.md.
static STAR_LOCK: Mutex<()> = Mutex::new(());
// hidden.md likewise: a hide comes from the app and never waits for a dream.
static HIDE_LOCK: Mutex<()> = Mutex::new(());

/// The hold of [`locked`]: released (flock, then the thread lock) when dropped.
struct Held {
    fd: Option<File>,
    depth: usize,
    _guard: ReentrantMutexGuard<'static, ()>,
}

impl Drop for Held {
    fn drop(&mut self) {
        HELD.with(|h| h.set(self.depth));
        if let Some(f) = self.fd.take() {
            // SAFETY: flock on a descriptor this hold owns.
            unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

fn locked(folder: &Path) -> Held {
    let guard = WRITE_LOCK.lock();
    let depth = HELD.with(|h| h.get());
    let mut fd = None;
    if depth == 0 && make_dir(folder).is_ok() {
        if let Ok(f) = File::open(folder) {
            // SAFETY: flock on a descriptor we own; a failure leaves the descriptor open, as in Python.
            unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) };
            fd = Some(f);
        }
    }
    HELD.with(|h| h.set(depth + 1));
    Held { fd, depth, _guard: guard }
}

// The forget generation. The dream reads the day and the records, calls a model for minutes with the lock
// released, then writes; a forget that ran in between would be undone by that write, and its commit would land
// after the forget squashed the history. So a forget moves the generation inside the same hold of the lock in which
// it scrubs the records, the dream pins it before it reads anything, and a write whose pin moved is dropped: the
// night is dreamt again, from what is left. On disk because the CLI forgets from another process; in memory too, so
// a failed write of the file cannot hide a forget from this process.
pub const FORGETS_FILE: &str = ".forgets";
/// Forgets begun in this process, per records folder (Python keeps one count for the process; eve has one records
/// folder, so the two agree, and a forget in one folder never voids a dream of another).
static FORGETS: LazyLock<Mutex<HashMap<PathBuf, u64>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
thread_local! {
    /// The generation this thread's dream pinned, or None.
    static PIN: RefCell<Option<Generation>> = const { RefCell::new(None) };
    /// Closures this night, per thread: the dream's night is one forgets_pinned hold, and reconcile and consolidate
    /// of that night share MAX_CLOSES_PER_NIGHT. Outside a night each call has the whole budget.
    static NIGHT_CLOSED: Cell<i64> = const { Cell::new(0) };
}

type Generation = (u64, String);

/// Call with locked(folder) held.
fn generation(folder: &Path) -> Generation {
    let disk = read_text(folder.join(FORGETS_FILE)).map(|s| tpy::strip(&s).to_string()).unwrap_or_default();
    (FORGETS.lock().get(folder).copied().unwrap_or(0), disk)
}

/// `int(disk) + 1` for a run of ASCII digits, in decimal, any length.
fn plus_one(disk: &str) -> String {
    let mut digits: Vec<u8> = disk.trim_start_matches('0').bytes().collect();
    let mut i = digits.len();
    loop {
        if i == 0 {
            digits.insert(0, b'1');
            break;
        }
        i -= 1;
        if digits[i] == b'9' {
            digits[i] = b'0';
        } else {
            digits[i] += 1;
            break;
        }
    }
    String::from_utf8(digits).unwrap_or_else(|_| "1".into())
}

/// Move the generation. Call with locked(folder) held, in the hold that scrubs the records.
fn bump(folder: &Path) {
    let _g = WRITE_LOCK.lock();
    *FORGETS.lock().entry(folder.to_path_buf()).or_insert(0) += 1;
    if !folder.is_dir() {
        return;
    }
    let disk = generation(folder).1;
    let next = if !disk.is_empty() && disk.bytes().all(|b| b.is_ascii_digit()) { plus_one(&disk) } else { "1".into() };
    if let Err(e) = write(&folder.join(FORGETS_FILE), &format!("{next}\n")) {
        warn!("records: could not note a forget on disk ({})", tpy::os_error_name(&e));
    }
}

/// The generation a write is checked against: the dream's pin, or now. Call with locked(folder) held.
fn pinned(folder: &Path) -> Generation {
    PIN.with(|p| p.borrow().clone()).unwrap_or_else(|| generation(folder))
}

/// The hold of [`pin_forgets`]: the pin is let go when dropped.
pub struct ForgetsPin {
    outer: bool,
}

impl Drop for ForgetsPin {
    fn drop(&mut self) {
        if !self.outer {
            PIN.with(|p| *p.borrow_mut() = None);
            NIGHT_CLOSED.with(|n| n.set(0));
        }
    }
}

/// Pin the forget generation for this thread before reading what a model will rewrite (the day's transcript
/// included). reconcile_day and consolidate inside the hold write nothing when a forget ran since. A pin inside a pin
/// keeps the outer one.
pub fn pin_forgets(cfg: &RecallConfig) -> ForgetsPin {
    let folder = records_dir(cfg);
    let outer = PIN.with(|p| p.borrow().is_some());
    if !outer {
        {
            let _held = locked(&folder);
            let gen = generation(&folder);
            PIN.with(|p| *p.borrow_mut() = Some(gen));
        }
        NIGHT_CLOSED.with(|n| n.set(0)); // one night, one closure budget (MAX_CLOSES_PER_NIGHT)
    }
    ForgetsPin { outer }
}

/// `with forgets_pinned(cfg): f()`.
pub fn forgets_pinned<R>(cfg: &RecallConfig, f: impl FnOnce() -> R) -> R {
    let _pin = pin_forgets(cfg);
    f()
}

// ---- stars: the owner's "remember that" ---------------------------------------------------------

const STARRED_HEAD: &str = "# Starred\n\nWhat the owner asked eve to remember, in their words, oldest first.\n\n";

fn norm(text: &str) -> String {
    tpy::casefold(tpy::flat(text).trim_matches([' ', '.', ',', ';', ':', '!']))
}

/// The dated lines of starred.md or hidden.md as (text, day), oldest first. An undated line has day "".
fn dated_rows(raw: &str) -> Vec<(String, String)> {
    let body = FRONTMATTER.replacen(raw, 1, "");
    let mut rows = Vec::new();
    for line in tpy::splitlines(&body) {
        let Some(m) = STAR_LINE.captures(line) else {
            continue;
        };
        let item = m.get(1).map_or("", |g| g.as_str());
        let (text, day) = match STAR_DATE.captures(item) {
            Some(d) => (tpy::strip(&d[1]).to_string(), d[2].to_string()),
            None => (item.to_string(), String::new()),
        };
        if !text.is_empty() {
            rows.push((text, day));
        }
    }
    rows
}

/// Every star as (text, day), oldest first. An undated hand-typed line has day "".
fn star_rows(cfg: &RecallConfig) -> Vec<(String, String)> {
    match read_text(records_dir(cfg).join(STARRED_FILE)) {
        Ok(raw) => dated_rows(&raw),
        Err(_) => Vec::new(),
    }
}

fn star_line(text: &str, day: &str) -> String {
    if day.is_empty() { format!("- {text}") } else { format!("- {text} ({day})") }
}

/// Append one line to an append-only owner file (starred.md, hidden.md): created 0600 with `head`, never through a
/// link, a newline first when the last line was left open.
fn append_line(path: &Path, line: &str, head: &str) -> io::Result<()> {
    let f = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    f.set_permissions(fs::Permissions::from_mode(0o600))?;
    let size = f.metadata()?.len();
    let mut data = format!("{line}\n");
    if size == 0 {
        data = format!("{head}{data}");
    } else {
        let mut last = [0u8; 1];
        let n = f.read_at(&mut last, size - 1)?;
        if n != 1 || last[0] != b'\n' {
            data = format!("\n{data}");
        }
    }
    (&f).write_all(data.as_bytes())
}

/// Keep one claim for good, because the owner said so. → the line as stored, or None.
///
/// A human promotes and an agent only proposes; the owner's voice IS the human, so "remember that" is a promotion
/// (owner, 2026-09-11). Append-only here; saying it twice (any case, any trailing punctuation) returns the line
/// already there. Dated by the memory's day, which starts at 04:00, so a star at 01:00 is the evening's and
/// tonight's dream of that evening reads it. One appended line; it never waits for a dream or a forget, so a
/// conversation can call it. `when` is a local wall time (Python's naive datetime); None is now.
pub fn star(cfg: &RecallConfig, text: &str, when: Option<NaiveDateTime>) -> Option<String> {
    let mut claim = tpy::strip(tpy::flat(text).trim_matches([' ', '.', ',', ';', ':'])).to_string();
    if claim.starts_with('★') {
        claim = tpy::strip(claim.trim_start_matches(['★', ' '])).to_string();
    }
    if tpy::len(&claim) < 4 {
        return None;
    }
    let first = claim.chars().next()?;
    if first.is_lowercase() {
        claim = first.to_uppercase().collect::<String>() + &claim[first.len_utf8()..];
    }
    let day = memory_day(when.unwrap_or_else(|| Local::now().naive_local()));
    let folder = records_dir(cfg);
    let res = (|| -> io::Result<String> {
        let _g = STAR_LOCK.lock();
        let key = norm(&claim);
        for (old_text, old_day) in star_rows(cfg) {
            if norm(&old_text) == key {
                return Ok(star_line(&old_text, &old_day));
            }
        }
        make_dir(&folder)?;
        let line = star_line(&claim, &day);
        append_line(&folder.join(STARRED_FILE), &line, STARRED_HEAD)?;
        info!("records: starred one line");
        Ok(line)
    })();
    match res {
        Ok(line) => Some(line),
        Err(e) => {
            warn!("records: could not star it ({})", tpy::os_error_name(&e));
            None
        }
    }
}

/// The starred claims, text only (no date), newest last. Python's default limit is MAX_STARS.
pub fn stars(cfg: &RecallConfig, limit: usize) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }
    let rows = star_rows(cfg);
    let from = rows.len().saturating_sub(limit);
    rows[from..].iter().map(|(t, _)| t.clone()).collect()
}

// ---- hidden: the owner's "don't mention this again" ----------------------------------------------
//
// ChatGPT's memory: "Don't mention this again reduces future references to the information. It does not delete the
// original source." Here: one line in hidden.md, and every line its words match is left out of what eve reads first
// (the profile, both fronts) and of what it finds (records, meanings, the words said). The records, the transcripts
// and mem0 are not touched; unhide is the line going.

const HIDDEN_HEAD: &str = "# Hidden\n\nWhat the owner asked eve not to bring up again. Nothing was deleted.\n\n";

/// Every hidden line as (words, day), oldest first.
fn hidden_rows(cfg: &RecallConfig) -> Vec<(String, String)> {
    match read_text(records_dir(cfg).join(HIDDEN_FILE)) {
        Ok(raw) => dated_rows(&raw),
        Err(_) => Vec::new(),
    }
}

/// The hidden words, oldest first.
pub fn hidden(cfg: &RecallConfig) -> Vec<String> {
    hidden_rows(cfg).into_iter().map(|(t, _)| t).collect()
}

/// transcripts.match: the one matching rule for search, forget, and this.
fn matcher(query: &str) -> Option<Pred> {
    eve_transcripts::match_query(query)
}

/// A predicate true for a line any hidden entry matches (transcripts.match: a "quoted phrase" as written, otherwise
/// every word), or None when nothing is hidden.
pub fn hidden_match(cfg: &RecallConfig) -> Option<Pred> {
    let preds: Vec<Pred> = hidden(cfg).iter().filter_map(|t| matcher(t)).collect();
    if preds.is_empty() {
        return None;
    }
    Some(Box::new(move |text: &str| preds.iter().any(|p| p(text))))
}

/// Don't bring these words up again. → the line as stored (the one already there when the same words, any case,
/// are hidden), or None when there is no word to match or the file could not be written. Deletes nothing.
pub fn hide(cfg: &RecallConfig, text: &str, when: Option<NaiveDateTime>) -> Option<String> {
    let flat = tpy::flat(text);
    let words = tpy::strip(tpy::head(flat.trim_matches([' ', '.', ',', ';', ':']), MAX_HIDDEN_CHARS)).to_string();
    if words.is_empty() || matcher(&words).is_none() {
        return None;
    }
    let day = memory_day(when.unwrap_or_else(|| Local::now().naive_local()));
    let folder = records_dir(cfg);
    let res = (|| -> io::Result<String> {
        let _g = HIDE_LOCK.lock();
        let key = norm(&words);
        for (old_text, old_day) in hidden_rows(cfg) {
            if norm(&old_text) == key {
                return Ok(star_line(&old_text, &old_day));
            }
        }
        make_dir(&folder)?;
        let line = star_line(&words, &day);
        append_line(&folder.join(HIDDEN_FILE), &line, HIDDEN_HEAD)?;
        info!("records: hid one line");
        Ok(line)
    })();
    match res {
        Ok(line) => Some(line),
        Err(e) => {
            warn!("records: could not hide it ({})", tpy::os_error_name(&e));
            None
        }
    }
}

/// Take a hidden line back (the same words, any case, as hide dedupes them). → whether one went.
pub fn unhide(cfg: &RecallConfig, text: &str) -> bool {
    let key = norm(tpy::head(&tpy::flat(text), MAX_HIDDEN_CHARS));
    if key.is_empty() {
        return false;
    }
    let path = records_dir(cfg).join(HIDDEN_FILE);
    let res = (|| -> io::Result<usize> {
        let _g = HIDE_LOCK.lock();
        let raw = read_text(&path)?;
        let mut kept: Vec<&str> = Vec::new();
        let mut removed = 0;
        for line in raw.split('\n') {
            if let Some(m) = STAR_LINE.captures(line) {
                let item = m.get(1).map_or("", |g| g.as_str());
                let words = match STAR_DATE.captures(item) {
                    Some(d) => d.get(1).map_or("", |g| g.as_str()).to_string(),
                    None => item.to_string(),
                };
                if norm(&words) == key {
                    removed += 1;
                    continue;
                }
            }
            kept.push(line);
        }
        if removed == 0 {
            return Ok(0);
        }
        write(&path, &kept.join("\n"))?;
        Ok(removed)
    })();
    match res {
        Ok(0) => false,
        Ok(n) => {
            info!("records: unhid {n} line(s)");
            true
        }
        Err(e) => {
            let name = if e.kind() == io::ErrorKind::InvalidData { "UnicodeDecodeError" } else { tpy::os_error_name(&e) };
            warn!("records: could not unhide it ({name})");
            false
        }
    }
}

type HidePred<'a> = Option<&'a dyn Fn(&str) -> bool>;

fn as_hide(p: &Option<Pred>) -> HidePred<'_> {
    p.as_ref().map(|b| {
        let f: &dyn Fn(&str) -> bool = &**b;
        f
    })
}

/// `text` without the body lines (never a heading or a front-matter line) a hidden entry matches.
fn drop_hidden(text: &str, hide_pred: HidePred<'_>) -> String {
    let Some(pred) = hide_pred else {
        return text.to_string();
    };
    if text.is_empty() {
        return text.to_string();
    }
    let (head, body) = match FRONTMATTER.find(text) {
        Some(fm) => (&text[..fm.end()], &text[fm.end()..]),
        None => ("", text),
    };
    let kept: Vec<&str> = body
        .split('\n')
        .filter(|ln| tpy::strip(ln).is_empty() || ln.trim_start_matches(tpy::is_space).starts_with('#') || !pred(ln))
        .collect();
    format!("{head}{}", kept.join("\n"))
}

// ---- what the brains may do: search and read ---------------------------------------------------

fn tokens(text: &str) -> Vec<String> {
    WORD.find_iter(&text.to_lowercase()).map(|m| m.as_str().to_string()).collect()
}

/// Keyword search, no vectors. A hit is one fact line. Score: query words found in the record's id or aliases count
/// three (that is what aliases are for), words in the line itself count one. A fact `hide` matches (hidden_match) is
/// never a hit. Each hit is `{"id", "line"}`, plus `"src"` when the fact carries one. Python's default limit is
/// MAX_HITS.
pub fn search(records: &Records, query: &str, limit: usize, hide: HidePred<'_>) -> Vec<Value> {
    let words: Vec<String> = tokens(query).into_iter().filter(|w| w.chars().count() > 1).collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<(usize, bool, String, Fact, String)> = Vec::new();
    let today = today();
    for rec in records.values() {
        let name_words: HashSet<String> = tokens(&format!("{} {}", rec.id, rec.aliases.join(" "))).into_iter().collect();
        let name_score = 3 * words
            .iter()
            .filter(|w| name_words.contains(*w) || (w.chars().count() > 3 && name_words.iter().any(|n| n.contains(w.as_str()))))
            .count();
        for line in &rec.facts {
            let fact = parse_fact(line);
            if hide.is_some_and(|h| h(&fact.text)) {
                continue;
            }
            let fact_words: HashSet<String> = tokens(&fact.text).into_iter().collect();
            let score = name_score + words.iter().filter(|w| fact_words.contains(*w)).count();
            if score > 0 {
                let closed = fact.closed(&today);
                hits.push((score, closed, rec.id.clone(), fact, line.clone()));
            }
        }
    }
    // on a tie, what is still true comes first
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    hits.into_iter()
        .take(limit)
        .map(|(_, _, rid, fact, line)| {
            let mut hit = Map::new();
            hit.insert("id".into(), Value::String(rid));
            hit.insert("line".into(), Value::String(tpy::head(&fact_display(&line), MAX_HIT_CHARS).to_string()));
            if !fact.src.is_empty() {
                hit.insert("src".into(), Value::String(fact.src));
            }
            Value::Object(hit)
        })
        .collect()
}

pub fn get(records: &Records, rid: &str) -> Option<String> {
    records
        .get(&tpy::strip(rid).to_lowercase())
        .map(|rec| tpy::head(&render_record(rec), MAX_RECORD_CHARS).to_string())
}

/// The part of the profile that says what can be looked up.
pub fn render_index(records: &Records) -> String {
    if records.is_empty() {
        return String::new();
    }
    let rows: Vec<String> = records
        .values()
        .map(|rec| {
            if rec.aliases.is_empty() {
                format!("- {} ({})", rec.id, rec.r#type)
            } else {
                let shown: Vec<&str> = rec.aliases.iter().take(6).map(|s| s.as_str()).collect();
                format!("- {} ({}): {}", rec.id, rec.r#type, shown.join(", "))
            }
        })
        .collect();
    format!("{INDEX_HEADING}\n{}", rows.join("\n"))
}

fn clip_lines(text: &str, cap: usize) -> String {
    if tpy::len(text) <= cap {
        return text.to_string();
    }
    let cut = tpy::head(text, cap.saturating_sub(2));
    let kept = match cut.rfind('\n') {
        Some(i) => &cut[..i],
        None => cut,
    };
    format!("{kept}\n…")
}

/// Stars the page has not absorbed yet (dated after its `updated:`; an undated star counts as new), less the hidden
/// ones.
fn fresh_block(cfg: &RecallConfig, updated: &str, hide: HidePred<'_>) -> String {
    let fresh: Vec<String> = star_rows(cfg)
        .iter()
        .filter(|(t, d)| (d.is_empty() || updated.is_empty() || d.as_str() > updated) && !hide.is_some_and(|h| h(t)))
        .map(|(t, d)| star_line(t, d))
        .collect();
    if fresh.is_empty() { String::new() } else { format!("{FRESH_STARS_HEADING}\n{}", fresh.join("\n")) }
}

fn profile_page(cfg: &RecallConfig) -> String {
    read_text(records_dir(cfg).join(format!("{PROFILE_ID}.md")))
        .map(|s| tpy::strip(&s).to_string())
        .unwrap_or_default()
}

/// The page without its closed fact lines, and with the braces off its open ones. A page with no fact meta (every
/// page written with the validity switch off) comes back as it is.
fn open_only(page: &str) -> String {
    if !page.contains('{') {
        return page.to_string();
    }
    let today = today();
    let mut out: Vec<String> = Vec::new();
    for line in page.split('\n') {
        let f = parse_fact(line);
        if f.text == line {
            out.push(line.to_string());
        } else if !f.closed(&today) {
            out.push(f.text);
        }
    }
    out.join("\n")
}

fn updated_of(text: &str) -> String {
    UPDATED.captures(text).map(|c| c[1].to_string()).unwrap_or_default()
}

/// The one-pager, bounded, for the start of a turn. "" when there is none yet.
///
/// A star has to count on the next message, not after tonight's dream, so stars the page has not absorbed yet go on
/// top, verbatim. With no page yet, the stars alone are the page. A line that carries fact meta (a fact copied from
/// a record) is shown only while it is open, and without its braces. A line a hidden entry matches is left out
/// (hide); the page on disk keeps it.
pub fn profile(cfg: &RecallConfig) -> String {
    let hide_pred = hidden_match(cfg);
    let mut text = drop_hidden(&open_only(&profile_page(cfg)), as_hide(&hide_pred));
    let block = fresh_block(cfg, &updated_of(&text), as_hide(&hide_pred));
    if !block.is_empty() {
        // above the page's first section, under its frontmatter, title and intro: a reader (the app's Memory card)
        // strips frontmatter only at the top and joins loose lines to the fact before them, so a block above the
        // frontmatter glued "id: profile updated: …" onto the last star
        let at = text.find("\n## ");
        text = if text.is_empty() {
            block
        } else {
            match at {
                None => format!("{text}\n\n{block}"),
                Some(at) => format!("{}\n\n{block}\n\n{}", text[..at].trim_end_matches('\n'), &text[at + 1..]),
            }
        };
    }
    clip_lines(&text, MAX_PROFILE_CHARS)
}

/// `text` without the '## heading' section (up to the next '## ' heading or the end).
fn drop_section(text: &str, heading: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut skipping = false;
    for line in tpy::splitlines(text) {
        if line.starts_with("## ") {
            skipping = tpy::strip(line) == heading;
        }
        if !skipping {
            out.push(line);
        }
    }
    tpy::strip(&out.join("\n")).to_string()
}

/// The profile for the Live voice front: no frontmatter, no record index (it has no tools to use the ids with, and
/// the index is most of the page), at most MAX_VOICE_PROFILE_CHARS.
pub fn profile_for_voice(cfg: &RecallConfig) -> String {
    let hide_pred = hidden_match(cfg);
    let page = drop_hidden(&open_only(&profile_page(cfg)), as_hide(&hide_pred));
    let block = fresh_block(cfg, &updated_of(&page), as_hide(&hide_pred));
    let page = drop_section(&FRONTMATTER.replacen(&page, 1, ""), INDEX_HEADING);
    let text = if !page.is_empty() && !block.is_empty() { format!("{block}\n\n{page}") } else { format!("{block}{page}") };
    clip_lines(tpy::strip(&text), MAX_VOICE_PROFILE_CHARS)
}

pub const TALKING_HEADING: &str = "## How they like to talk";

/// The profile page's "How they like to talk" section, its heading and lines, from the whole page (closed facts and
/// hidden lines dropped, never clipped here); "" when the page has none. For the small-talk lane
/// (small_talk.voice_from_profile clips it). Not read from profile_for_voice: that one is cut to
/// MAX_VOICE_PROFILE_CHARS with fresh stars on top, and this section, the page's last, is the first to go.
pub fn talking_section(cfg: &RecallConfig) -> String {
    let hide_pred = hidden_match(cfg);
    let page = drop_hidden(&open_only(&profile_page(cfg)), as_hide(&hide_pred));
    let mut out: Vec<&str> = Vec::new();
    let mut inside = false;
    for line in tpy::splitlines(&page) {
        if line.starts_with("## ") {
            inside = tpy::strip(line) == TALKING_HEADING;
        }
        if inside {
            out.push(line);
        }
    }
    tpy::strip(&out.join("\n")).to_string()
}

fn journal_title(path: &Path) -> String {
    let Ok(raw) = read_text(path) else {
        return String::new();
    };
    if let Some(fm) = FRONTMATTER.find(&raw) {
        if let Some(t) = TITLE.captures(fm.as_str()) {
            return t[1].to_string();
        }
    }
    HEADING1.captures(&raw).map(|h| h[1].to_string()).unwrap_or_default()
}

/// (day, title) of the newest dream journal, or None when there is none.
pub fn latest_day(cfg: &RecallConfig) -> Option<(String, String)> {
    let folder = records_dir(cfg).join(DAYS_DIR);
    let rd = fs::read_dir(&folder).ok()?;
    let mut days: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|n| DAY_FILE.captures(&n).map(|m| m[1].to_string()))
        .collect();
    days.sort();
    let last = days.pop()?;
    let title = journal_title(&folder.join(format!("{last}.md")));
    Some((last, title))
}

fn recalled_line(hit: &Map<String, Value>) -> String {
    let day = str_or_empty(hit.get("day"));
    let channel = str_or_empty(hit.get("channel"));
    let stamp: Vec<&str> = [day.as_str(), channel.as_str()].into_iter().filter(|x| !x.is_empty()).collect();
    let text = match hit.get("text") {
        Some(v) => str_of(v),
        None => String::new(),
    };
    if stamp.is_empty() { text } else { format!("({}) {text}", stamp.join(", ")) }
}

/// The mem0 index as the reader uses it (mem0_memory.OwnerMemory): a search by meaning that returns rows with
/// `text`, `day` and `channel`.
pub trait MeaningSearch {
    fn search(&self, query: &str) -> anyhow::Result<Vec<Value>>;
}

/// Read-only, re-read from disk per call (the owner may have edited). `recall` is the mem0 index or None.
pub struct RecordsReader<'a> {
    pub cfg: RecallConfig,
    pub recall: Option<&'a dyn MeaningSearch>,
}

impl<'a> RecordsReader<'a> {
    pub fn new(cfg: RecallConfig, recall: Option<&'a dyn MeaningSearch>) -> Self {
        Self { cfg, recall }
    }

    pub fn profile(&self) -> String {
        profile(&self.cfg)
    }

    pub fn search(&self, query: &str) -> Value {
        let hide_pred = hidden_match(&self.cfg);
        let hits = search(&load_records(&self.cfg), query, MAX_HITS, as_hide(&hide_pred));
        let mut recalled: Vec<Value> = Vec::new();
        if let Some(index) = self.recall {
            match index.search(query) {
                Ok(rows) => {
                    recalled = rows
                        .iter()
                        .filter_map(|h| h.as_object())
                        .filter(|h| !hide_pred.as_ref().is_some_and(|p| p(&str_or_empty(h.get("text")))))
                        .map(|h| Value::String(recalled_line(h)))
                        .collect();
                }
                // the meaning search failing is just no meaning hits
                Err(e) => warn!("records: meaning search failed ({})", py::error_name(&e)),
            }
        }
        let mut out = Map::new();
        out.insert("ok".into(), Value::Bool(true));
        let none = hits.is_empty() && recalled.is_empty();
        out.insert("hits".into(), Value::Array(hits));
        if !recalled.is_empty() {
            out.insert("recalled".into(), Value::Array(recalled)); // "(day, channel) fact": dated, so the newer one wins a tie
        }
        if none {
            out.insert("note".into(), Value::String("nothing matched".into()));
        }
        Value::Object(out)
    }

    pub fn get(&self, rid: &str) -> Value {
        let mut records = load_records(&self.cfg);
        if let Some(pred) = hidden_match(&self.cfg) {
            // a hidden fact is not read back either; the file keeps it
            for rec in records.values_mut() {
                rec.facts.retain(|f| !pred(&parse_fact(f).text));
            }
        }
        match get(&records, rid) {
            Some(text) if !text.is_empty() => json!({"ok": true, "record": text}),
            _ => json!({"ok": false, "reason": format!("no record {}", py::repr_str(rid))}),
        }
    }
}

// ---- git: history is how nothing is lost ---------------------------------------------------------

// A commit never asks for a signature or an identity: this repository belongs to eve, on this machine.
const COMMIT_CFG: [&str; 6] = ["-c", "user.name=eve", "-c", "user.email=eve@localhost", "-c", "commit.gpgsign=false"];

fn run_process(argv: &[String], env: &HashMap<String, String>, timeout: f64) -> Result<GitOutput, GitError> {
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = out_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = err_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let deadline = Instant::now() + Duration::from_secs_f64(timeout);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = out_reader.join();
            let _ = err_reader.join();
            return Err(GitError::Timeout(argv.to_vec(), timeout));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    Ok(GitOutput {
        returncode: status.code().unwrap_or(-1),
        stdout: tpy::read_text(&stdout),
        stderr: tpy::read_text(&stderr),
    })
}

/// `git -C <cwd> <args>` with the environment less every `GIT_*` variable (never another repo's GIT_DIR). Refused,
/// before any process, where git is not usable (eve lite, or /usr/bin/git on a Mac without the developer tools,
/// where it opens their install dialog instead of running): every caller reads that as "no git" and keeps the
/// records without history.
pub fn git(cwd: &Path, args: &[&str], timeout: f64) -> Result<GitOutput, GitError> {
    let s = seams();
    let env: HashMap<String, String> = environ().into_iter().filter(|(k, _)| !k.starts_with("GIT_")).collect();
    let usable = match s.git_usable {
        Some(u) => u,
        None => s
            .toolchain
            .clone()
            .unwrap_or_else(eve_toolchain::Toolchain::system)
            .git_usable(&env)
            .map_err(|e| GitError::Profile(e.0.clone()))?,
    };
    if !usable {
        return Err(GitError::Unusable);
    }
    let mut argv: Vec<String> = vec!["git".into(), "-C".into(), cwd.to_string_lossy().into_owned()];
    argv.extend(args.iter().map(|a| a.to_string()));
    match &s.run {
        Some(run) => run(&argv),
        None => run_process(&argv, &env, timeout),
    }
}

fn git_(cwd: &Path, args: &[&str]) -> Result<GitOutput, GitError> {
    git(cwd, args, GIT_TIMEOUT_SECS)
}

/// `shutil.which("git")`.
fn which_git() -> Option<String> {
    match seams().which_git {
        Some(w) => w,
        None => eve_toolchain::which_in("git", &environ()),
    }
}

fn resolved(p: &Path) -> PathBuf {
    tpy::resolve(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Whether `repo` is the root of its own git repository (not a folder inside some other one).
fn own_repo(repo: &Path) -> Result<bool, GitError> {
    let top = git_(repo, &["rev-parse", "--show-toplevel"])?;
    Ok(top.returncode == 0 && resolved(Path::new(tpy::strip(&top.stdout))) == resolved(repo))
}

/// `repo` as a sealed git repository of its own. False (with one log line) when git is not there, or when `repo`
/// sits inside another repository: forcing an add there would put the owner's memory in it.
pub fn ensure_repo(repo: &Path) -> bool {
    if which_git().is_none() {
        warn!("records: git is not installed — records will be kept, but without history");
        return false;
    }
    let res = (|| -> Result<bool, GitError> {
        make_dir(repo)?;
        let top = git_(repo, &["rev-parse", "--show-toplevel"])?;
        if top.returncode == 0 {
            if resolved(Path::new(tpy::strip(&top.stdout))) == resolved(repo) {
                seal(repo)?; // an existing repository gets the same lock, every start
                return Ok(true);
            }
            warn!("records: {} is inside another git repository — kept without history", repo.display());
            return Ok(false);
        }
        let r = git_(repo, &["init", "-q"])?;
        if r.returncode != 0 {
            warn!("records: git init failed (exit {})", r.returncode);
            return Ok(false);
        }
        git_(repo, &["config", "user.name", "eve"])?;
        git_(repo, &["config", "user.email", "eve@localhost"])?;
        seal(repo)?;
        Ok(true)
    })();
    match res {
        Ok(b) => b,
        Err(e) => {
            warn!("records: git unavailable ({})", e.py_name());
            false
        }
    }
}

// Every URL shape git can push to, rewritten to one it cannot. A second lock beside the pre-push hook, because
// `git push --no-verify` skips hooks but never skips pushInsteadOf.
pub const PUSH_PREFIXES: [&str; 9] = ["https://", "http://", "ssh://", "git://", "git@", "file://", "/", "~", "."];
const NO_PUSH_URL: &str = "local-only-never-pushed:";
const PRE_PUSH: &str = "#!/bin/sh
# eve's memory lives on this computer only (records.py seal). Pushing it anywhere is refused.
echo \"eve memory is local-only: push refused\" >&2
exit 1
";

/// Lock the repository to this computer: no remote, no push, ever.
///
/// The owner's memory is private (owner, 2026-09-23: "nothing should leak out ever"). The history exists so a bad
/// dream is one revert away, not so it can go anywhere. So on every start any remote is removed, every push URL is
/// rewritten to one that cannot resolve, and a pre-push hook refuses — three independent locks, so none of them
/// failing alone lets it out.
pub fn seal(repo: &Path) -> Result<(), GitError> {
    let remotes = git_(repo, &["remote"])?.stdout;
    for name in remotes.split(tpy::is_space).filter(|s| !s.is_empty()) {
        git_(repo, &["remote", "remove", name])?;
        warn!("records: removed a git remote from the memory store — it stays on this computer");
    }
    let key = format!("url.{NO_PUSH_URL}.pushInsteadOf");
    for prefix in PUSH_PREFIXES {
        let pattern = format!("^{}$", py::re_escape(prefix));
        git_(repo, &["config", "--replace-all", &key, prefix, &pattern])?;
    }
    let hooks = repo.join(".git").join("hooks");
    fs::create_dir_all(&hooks)?;
    fs::write(hooks.join("pre-push"), PRE_PUSH)?;
    fs::set_permissions(hooks.join("pre-push"), fs::Permissions::from_mode(0o755))?;
    let hooks_path = hooks.to_string_lossy().into_owned();
    git_(repo, &["config", "core.hooksPath", &hooks_path])?; // a global hooksPath must not bypass it
    Ok(())
}

/// Commit everything under `repo`. The short hash, or None when there was nothing or git failed.
///
/// Forced (an ignore file must not quietly cost the history), and so only in the folder's own repository.
pub fn commit(repo: &Path, message: &str) -> Option<String> {
    let res = (|| -> Result<Option<String>, GitError> {
        if !own_repo(repo)? {
            return Ok(None);
        }
        git_(repo, &["add", "-A", "--force", "."])?;
        if git_(repo, &["diff", "--cached", "--quiet"])?.returncode == 0 {
            return Ok(None);
        }
        let mut args: Vec<&str> = COMMIT_CFG.to_vec();
        args.extend(["commit", "-q", "--no-verify", "-m", message]);
        let r = git_(repo, &args)?;
        if r.returncode != 0 {
            warn!("records: commit failed (exit {})", r.returncode);
            return Ok(None);
        }
        let sha = tpy::strip(&git_(repo, &["rev-parse", "--short", "HEAD"])?.stdout).to_string();
        Ok(if sha.is_empty() { None } else { Some(sha) })
    })();
    match res {
        Ok(s) => s,
        Err(e) => {
            warn!("records: commit failed ({})", e.py_name());
            None
        }
    }
}

/// Make git forget: the current tree becomes the only commit, and every older object is pruned.
///
/// A forget has to reach history too, or the forgotten line is one `git log -p` away. Everything under the tree is
/// committed first, the branch is pointed at one parentless commit of it, every other ref and every reflog entry
/// goes, and gc prunes what nothing reaches any more. False when `repo` is not the root of its own repository (never
/// someone else's) or git failed.
pub fn squash_history(repo: &Path) -> bool {
    let res = (|| -> Result<bool, GitError> {
        if which_git().is_none() || !own_repo(repo)? {
            return Ok(false);
        }
        let _held = locked(repo);
        git_(repo, &["add", "-A", "--force", "."])?;
        if git_(repo, &["rev-parse", "--verify", "-q", "HEAD"])?.returncode != 0 {
            if git_(repo, &["diff", "--cached", "--quiet"])?.returncode == 0 {
                return Ok(true); // no history at all: nothing to forget
            }
            let mut args: Vec<&str> = COMMIT_CFG.to_vec();
            args.extend(["commit", "-q", "--no-verify", "-m", "memory"]);
            if git_(repo, &args)?.returncode != 0 {
                return Ok(false);
            }
        }
        let tree = git_(repo, &["write-tree"])?;
        if tree.returncode != 0 {
            return Ok(false);
        }
        let tree_id = tpy::strip(&tree.stdout).to_string();
        let mut args: Vec<&str> = COMMIT_CFG.to_vec();
        args.extend(["commit-tree", &tree_id, "-m", "memory (history squashed by a forget)"]);
        let new = git_(repo, &args)?;
        if new.returncode != 0 {
            return Ok(false);
        }
        let head = tpy::strip(&git_(repo, &["symbolic-ref", "-q", "HEAD"])?.stdout).to_string();
        let branch = if head.is_empty() { "refs/heads/main".to_string() } else { head };
        let new_id = tpy::strip(&new.stdout).to_string();
        if git_(repo, &["update-ref", &branch, &new_id])?.returncode != 0 {
            return Ok(false);
        }
        if git_(repo, &["symbolic-ref", "HEAD", &branch])?.returncode != 0 {
            return Ok(false);
        }
        git_(repo, &["reset", "-q"])?; // the index matches the new commit
        let refs = git_(repo, &["for-each-ref", "--format=%(refname)"])?.stdout;
        for r in refs.split(tpy::is_space).filter(|s| !s.is_empty()) {
            if r != branch {
                git_(repo, &["update-ref", "-d", r])?;
            }
        }
        for leftover in ["ORIG_HEAD", "FETCH_HEAD", "MERGE_HEAD"] {
            let _ = fs::remove_file(repo.join(".git").join(leftover));
        }
        git_(repo, &["reflog", "expire", "--expire=now", "--expire-unreachable=now", "--all"])?;
        let gc = git(
            repo,
            &["-c", "gc.reflogExpire=now", "-c", "gc.reflogExpireUnreachable=now", "gc", "-q", "--prune=now"],
            GC_TIMEOUT_SECS,
        )?;
        Ok(gc.returncode == 0)
    })();
    match res {
        Ok(b) => b,
        Err(e) => {
            warn!("records: could not squash the history ({})", e.py_name());
            false
        }
    }
}

/// Whatever is on disk now — the owner's hand edits and new stars included — goes into history first, as its own
/// commit, so the dream's commit is exactly what the model changed and reverts cleanly.
fn as_found(repo: &Path, git: bool, what: &str) {
    if git {
        commit(repo, &format!("as found before {what}"));
    }
}

// ---- the dream: the only automatic writer ----------------------------------------------------------

// The reconcile prompt, in pieces: the plain prompt (both switches off) is the pieces joined, word for word what it
// was; a switch swaps in its own piece. Each combination is one fixed string, so the prompt cache still holds.
const R_HEAD: &str = "You are eve, your owner's personal sidekick, keeping the records of what you know about your owner.
Tonight you read one whole day of what was said between you, on voice and by text, beside the records as
they are now, the profile page, and what the owner asked you to remember. Return the records that should
change, whole; the profile as it should read now; the ids of records the owner asked you to forget; and your
journal of the day.

Records are facts about durable things: a preference, a person, an organization, a project, a place, a
routine. One record per thing, id in lowercase-with-hyphens, with aliases: every word the owner might use
for it. ";
const R_FACT_PLAIN: &str = "Each fact is one line, stated as fact, with its date: \"Loves pasta (said 2026-09-15).\" ";
const R_FACT_META_HEAD: &str = "Each fact is one line of text, stated as fact, with its fields beside it.";
const R_FIELD_DATES: &str = " said: the date of
the line that said it. valid_from: the date it became true (the said date when the line gives no other).
valid_to: the date it stopped being true, empty while it is still true.";
const R_FIELD_SRC: &str = " src: the date and time the
transcript line it came from starts with, YYYY-MM-DD HH:MM, so the line can be found again; a fact from
several lines takes the line that says it most plainly, and a fact you keep from before keeps its src.";
const R_FACT_TEXT_DATE: &str = " The text still
ends with its date, as in \"Loves pasta (said 2026-09-15).\".";
const R_REST: &str = "Link related
records with [[id]] inside a fact. Keep records short: merge examples into traits, drop incidental detail,
move what belongs to a project into that project's record. What happened once belongs in the journal, not
in a record. Only what the owner said is a fact about them; what eve said or did is not, and neither is a
request to operate the robot or its tools. Return only records that change or are new.

";
const R_CORRECTIONS_PLAIN: &str = "Corrections: when the day contradicts a record, never delete the old fact silently. Replace it with the
correction and its date, \"Now prefers the 9 am slot (changed 2026-09-20; was 8 am).\", and add one line
saying what changed to the journal's corrections. Anything the owner said to forget is removed: list the
record's id under forget, or return the record without that fact. That is the only removal.

";
const R_CORRECTIONS_VALIDITY: &str = "Corrections: when the day contradicts a fact that is still true, never delete it and never reword it.
Close it: return it as it was, with valid_to set to the date of the line that changed it, and add the new
fact beside it with valid_from that date. Add one line saying what changed to the journal's corrections. A
fact with a valid_to is history: keep it as it is. Anything the owner said to forget is removed: list the
record's id under forget, or return the record without that fact. That is the only removal.

Plans that have passed: a fact about something the owner meant to do or attend on a date before this day (an
appointment, a trip, a deadline) is no longer a plan. Return its record with that fact rewritten in the past
tense, as what happened (\"Had a dentist cleaning on 2026-09-16.\"), or as what did not happen when the day
says so. Its dates stay.

";
const R_PROFILE: &str = "The profile is the page you read before every conversation. Three short sections, plain prose or a few
bullets each: \"Life context\" (their name first, when you know it, then who they are, what is going on, the
people and projects that matter now), \"Acting on their behalf\" (what they want done without asking and what
needs a yes first), \"How they like to talk\" (tone, length, what annoys them). Dates where they matter.
Nothing that is not in the records, the stars or the day.";
const R_PROFILE_VALIDITY: &str = " Only what is true now: no fact with a valid_to, and a
plan whose date has passed is written in the past tense or left out.";
const R_TAIL: &str = "

What the owner asked you to remember is in their words, each with its date. Those are the most reliable
facts you have: fold every real fact about them into the records and the profile, and keep it there. A line
that is not a fact about them — a stray question, a half sentence the microphone caught — you leave out.

Dates: every transcript line starts with the real date and time it was said. A transcript day runs from
04:00 to 04:00, so its last lines can carry the next calendar date; they belong in this journal, under
their own date. Date a fact by the date of the line that said it, and read \"tomorrow\", \"Friday\" or \"tonight\"
from that line's date too.

The journal: a title of a few words for the day; what happened (one line per conversation or event,
starting with the real date and time as the transcript gives it, YYYY-MM-DD HH:MM); what you learned about
the owner; what is still open (a question left unanswered, a promise eve made); the plans; and the
corrections. A plan is anything the owner means to do or attend (an appointment, a trip, a deadline, a
project milestone): what it is, when it happens (YYYY-MM-DD, with HH:MM when given; empty when the day gave
no date), and the date it was said. The day it happens and the day it was said are different dates; keep
both. Short lines. An empty list when there is nothing.";

fn fact_piece(validity: bool, sources: bool) -> String {
    if !(validity || sources) {
        return R_FACT_PLAIN.to_string();
    }
    let mut fields = String::new();
    if validity {
        fields += R_FIELD_DATES;
    }
    if sources {
        fields += R_FIELD_SRC;
    }
    if !validity {
        fields += R_FACT_TEXT_DATE;
    }
    format!("{R_FACT_META_HEAD}{fields} ")
}

/// The reconcile instructions for this combination of switches. Both off: RECONCILE_PROMPT itself.
pub fn reconcile_prompt(validity: bool, sources: bool) -> String {
    [
        R_HEAD,
        &fact_piece(validity, sources),
        R_REST,
        if validity { R_CORRECTIONS_VALIDITY } else { R_CORRECTIONS_PLAIN },
        R_PROFILE,
        if validity { R_PROFILE_VALIDITY } else { "" },
        R_TAIL,
    ]
    .concat()
}

pub static RECONCILE_PROMPT: LazyLock<String> = LazyLock::new(|| reconcile_prompt(false, false));

const C_HEAD: &str = "You are eve, your owner's personal sidekick, tidying the records of what you know about your owner.
You are shown every record and the profile page. Return only what should change:

- The same thing filed under two ids (a person, a place or a project under two names): keep one id, return
  it whole with the aliases of both and every fact of both, and list the other id under merged with the id
  it went into. Never lose a fact in a merge.
";
const C_CONTRADICT_PLAIN: &str = "- Two facts that contradict each other: keep the newest dated fact as it is, and rewrite the older one as
  \"(until YYYY-MM-DD) <the older fact>\", dated by when the newer one was said.
";
const C_CONTRADICT_VALIDITY: &str = "- Two facts with no valid_to that contradict each other: keep the newest as it is, and close the older one:
  set its valid_to to the date the newer one was said, and change nothing else in it.
";
const C_TAIL: &str = "- A fact repeated in one record: keep one copy.

Records you do not change are not returned. Change nothing else: the wording is the owner's as much as
yours, and they may have edited it by hand.";
const C_KEEP_FIELDS: &str = " Every fact you return keeps its fields as they are, except a valid_to
this list tells you to set.";

/// The consolidate instructions for this combination of switches. Both off: CONSOLIDATE_PROMPT itself.
pub fn consolidate_prompt(validity: bool, sources: bool) -> String {
    [
        C_HEAD,
        if validity { C_CONTRADICT_VALIDITY } else { C_CONTRADICT_PLAIN },
        C_TAIL,
        if validity || sources { C_KEEP_FIELDS } else { "" },
    ]
    .concat()
}

pub static CONSOLIDATE_PROMPT: LazyLock<String> = LazyLock::new(|| consolidate_prompt(false, false));

fn list_schema() -> Value {
    json!({"type": "array", "items": {"type": "string"}})
}

fn record_base() -> Value {
    json!({"type": "object", "additionalProperties": false,
           "required": ["id", "type", "aliases", "facts"],
           "properties": {"id": {"type": "string"}, "type": {"type": "string", "enum": TYPES},
                          "aliases": list_schema(), "facts": list_schema()}})
}

fn fact_field(key: &str) -> Value {
    match key {
        "text" => json!({"type": "string"}),
        "said" => json!({"type": "string", "description": "the date of the line that said it: YYYY-MM-DD"}),
        "valid_from" => json!({"type": "string", "description": "the date it became true: YYYY-MM-DD"}),
        "valid_to" => json!({"type": "string", "description": "the date it stopped being true: YYYY-MM-DD, or empty"}),
        _ => json!({"type": "string", "description": "the date and time its transcript line starts with: YYYY-MM-DD HH:MM"}),
    }
}

fn plans_schema() -> Value {
    json!({"type": "array", "items": {
        "type": "object", "additionalProperties": false, "required": ["what", "when", "said"],
        "properties": {"what": {"type": "string"},
                       "when": {"type": "string", "description": "when it happens: YYYY-MM-DD [HH:MM], or empty"},
                       "said": {"type": "string", "description": "the date of the line that said it: YYYY-MM-DD"}}}})
}

fn record_schema(validity: bool, sources: bool) -> Value {
    let mut rec = record_base();
    if !(validity || sources) {
        return rec;
    }
    let mut keys = vec!["text"];
    if validity {
        keys.extend(["said", "valid_from", "valid_to"]);
    }
    if sources {
        keys.push("src");
    }
    let props: Map<String, Value> = keys.iter().map(|k| (k.to_string(), fact_field(k))).collect();
    let fact = json!({"type": "object", "additionalProperties": false, "required": keys, "properties": props});
    rec["properties"]["facts"] = json!({"type": "array", "items": fact});
    rec
}

/// Both switches off: RECONCILE_SCHEMA itself. On, facts are objects with the switched-on fields.
pub fn reconcile_schema(validity: bool, sources: bool) -> Value {
    json!({
        "type": "object", "additionalProperties": false,
        "required": ["records", "forget", "profile", "journal"],
        "properties": {
            "records": {"type": "array", "items": record_schema(validity, sources)},
            "forget": {"type": "array", "items": {"type": "string"},
                       "description": "ids of records the owner asked to forget entirely"},
            "profile": {"type": "object", "additionalProperties": false,
                        "required": ["life_context", "acting", "talking"],
                        "properties": {"life_context": list_schema(), "acting": list_schema(), "talking": list_schema()}},
            "journal": {"type": "object", "additionalProperties": false,
                        "required": ["title", "happened", "learned", "open", "plans", "corrections"],
                        "properties": {"title": {"type": "string"}, "happened": list_schema(), "learned": list_schema(),
                                       "open": list_schema(), "plans": plans_schema(), "corrections": list_schema()}},
        },
    })
}

/// Both switches off: CONSOLIDATE_SCHEMA itself.
pub fn consolidate_schema(validity: bool, sources: bool) -> Value {
    json!({
        "type": "object", "additionalProperties": false,
        "required": ["records", "merged"],
        "properties": {
            "records": {"type": "array", "items": record_schema(validity, sources)},
            "merged": {"type": "array", "items": {
                "type": "object", "additionalProperties": false, "required": ["id", "into"],
                "properties": {"id": {"type": "string"}, "into": {"type": "string"}}}},
        },
    })
}

pub static RECONCILE_SCHEMA: LazyLock<Value> = LazyLock::new(|| reconcile_schema(false, false));
pub static CONSOLIDATE_SCHEMA: LazyLock<Value> = LazyLock::new(|| consolidate_schema(false, false));

/// What one night's reconcile changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayChange {
    /// existing records rewritten or forgotten
    pub changed: usize,
    /// records new tonight
    pub created: usize,
    /// dated corrections the journal lists
    pub corrections: usize,
    /// days/<day>.md
    pub journal: PathBuf,
    /// facts closed by a valid_to tonight (the validity switch)
    pub closed: usize,
}

fn str_items(v: Option<&Value>) -> Vec<String> {
    iter_of(v.filter(|v| py::truthy(v))).iter().map(str_of).collect()
}

pub fn render_profile(prof: &Map<String, Value>, records: &Records, day: &str) -> String {
    let section = |title: &str, key: &str| -> String {
        let lines: Vec<String> = str_items(prof.get(key))
            .iter()
            .map(|x| tpy::strip(x).to_string())
            .filter(|x| !x.is_empty())
            .collect();
        let body = if lines.is_empty() {
            "- (nothing yet)".to_string()
        } else {
            lines.iter().map(|l| format!("- {l}")).collect::<Vec<_>>().join("\n")
        };
        format!("## {title}\n{body}")
    };
    let mut parts = vec![
        format!("---\nid: {PROFILE_ID}\nupdated: {day}\n---"),
        "# What eve knows about its owner".to_string(),
        format!("Dreamt from conversations up to {day}. What was said since is in today's transcript."),
        section("Life context", "life_context"),
        section("Acting on their behalf", "acting"),
        section("How they like to talk", "talking"),
    ];
    let index = render_index(records);
    if !index.is_empty() {
        parts.push(index);
    }
    parts.join("\n\n") + "\n"
}

/// Rewrite only the profile's record index, from the records as they are now.
fn reindex_profile(cfg: &RecallConfig) -> io::Result<()> {
    let path = records_dir(cfg).join(format!("{PROFILE_ID}.md"));
    let page = profile_page(cfg);
    if page.is_empty() {
        return Ok(());
    }
    let body = drop_section(&page, INDEX_HEADING);
    let index = render_index(&load_records(cfg));
    let text = if index.is_empty() { format!("{body}\n") } else { format!("{body}\n\n{index}\n") };
    write(&path, &text)
}

pub fn render_journal(journal: &Map<String, Value>, day: &str) -> String {
    let items = |key: &str| -> Vec<String> {
        str_items(journal.get(key)).iter().filter(|x| !tpy::strip(x).is_empty()).map(|x| tpy::flat(x)).collect()
    };
    let mut title = tpy::flat(&str_or_empty(journal.get("title")));
    if title.is_empty() {
        title = day.to_string();
    }
    let mut parts = vec![format!("---\nday: {day}\ntitle: {title}\n---"), format!("# {title}")];
    for (heading, key) in [
        ("What happened", "happened"),
        ("What eve learned", "learned"),
        ("Still open", "open"),
        ("Plans", "plans"),
        ("Corrections", "corrections"),
    ] {
        let rows = if key == "plans" { plan_rows(journal.get(key)) } else { items(key) };
        let body = if rows.is_empty() {
            "- (nothing)".to_string()
        } else {
            rows.iter().map(|r| format!("- {r}")).collect::<Vec<_>>().join("\n")
        };
        parts.push(format!("## {heading}\n{body}"));
    }
    parts.join("\n\n") + "\n"
}

/// One line per plan: when it happens apart from when it was said, e.g. "Dentist (when 2026-09-30 10:00; said
/// 2026-09-23)". Malformed entries are skipped.
fn plan_rows(plans: Option<&Value>) -> Vec<String> {
    let flat = |v: Option<&Value>| tpy::flat(&str_or_empty(v));
    let mut rows = Vec::new();
    let Some(Value::Array(plans)) = plans else {
        return rows;
    };
    for plan in plans {
        let Some(plan) = plan.as_object() else {
            continue;
        };
        let what = flat(plan.get("what"));
        if what.is_empty() {
            continue;
        }
        let (when, said) = (flat(plan.get("when")), flat(plan.get("said")));
        let when_part = if when.is_empty() { ": no date yet".to_string() } else { format!(" {when}") };
        let said_part = if said.is_empty() { ")".to_string() } else { format!("; said {said})") };
        rows.push(format!("{what} (when{when_part}{said_part}"));
    }
    rows
}

/// One fact from a model reply → its line. A string is taken as a line (a plain fact, or one with meta); an object
/// ({text, said, valid_from, valid_to, src}, the switched-on fields) is rendered. A date that is not YYYY-MM-DD is
/// dropped, and so is a src that is not one of `srcs` (the lines the model was shown, and the srcs the records
/// already carry): a src must point at a real line. "" when there is no text.
fn fact_line(raw: &Value, srcs: Option<&HashSet<String>>) -> String {
    let mut fact = match raw {
        Value::Object(o) => Fact {
            text: parse_fact(&tpy::flat(&str_or_empty(o.get("text")))).text,
            said: date_or_empty(o.get("said")),
            valid_from: date_or_empty(o.get("valid_from")),
            valid_to: date_or_empty(o.get("valid_to")),
            src: tpy::flat(&str_or_empty(o.get("src"))),
        },
        other => parse_fact(&tpy::flat(&str_or_empty(Some(other)))),
    };
    if !fact.src.is_empty() && (!SRC.is_match(&fact.src) || srcs.is_some_and(|s| !s.contains(&fact.src))) {
        fact.src.clear();
    }
    if fact.text.is_empty() { String::new() } else { render_fact(&fact) }
}

/// At most MAX_FACTS. Over it, the closed facts go first (the oldest valid_to first), then the newest open ones are
/// cut, as before the switches.
fn cap_facts(facts: Vec<String>) -> Vec<String> {
    if facts.len() <= MAX_FACTS {
        return facts;
    }
    let parsed: Vec<Fact> = facts.iter().map(|f| parse_fact(f)).collect();
    let mut closed: Vec<usize> = (0..parsed.len()).filter(|&i| !parsed[i].valid_to.is_empty()).collect();
    closed.sort_by(|a, b| parsed[*a].valid_to.cmp(&parsed[*b].valid_to));
    let drop: HashSet<usize> = closed.into_iter().take(facts.len() - MAX_FACTS).collect();
    facts.into_iter().enumerate().filter(|(i, _)| !drop.contains(i)).map(|(_, f)| f).take(MAX_FACTS).collect()
}

fn record_from(raw: &Value, day: &str, srcs: Option<&HashSet<String>>) -> Option<Record> {
    let raw = raw.as_object()?;
    let rid = tpy::strip(&str_or_empty(raw.get("id"))).to_lowercase();
    if !ID.is_match(&rid) || rid == PROFILE_ID || rid == "starred" || rid == "hidden" {
        return None;
    }
    let facts: Vec<String> =
        iter_of(raw.get("facts")).iter().map(|f| fact_line(f, srcs)).filter(|l| !l.is_empty()).collect();
    if facts.is_empty() {
        return None;
    }
    let rtype = str_or_empty(raw.get("type"));
    let aliases: Vec<String> = iter_of(raw.get("aliases"))
        .iter()
        .map(|a| tpy::strip(&str_of(a)).to_string())
        .filter(|a| !a.is_empty())
        .take(MAX_ALIASES)
        .collect();
    Some(Record {
        id: rid,
        r#type: if TYPES.contains(&rtype.as_str()) { rtype } else { "preference".into() },
        aliases,
        facts: cap_facts(facts),
        updated: day.to_string(),
    })
}

fn srcs_of(records: &Records) -> HashSet<String> {
    records.values().flat_map(|r| r.facts.iter().map(|f| parse_fact(f).src)).filter(|s| !s.is_empty()).collect()
}

/// Every "YYYY-MM-DD HH:MM" a line of the dream's day text starts with: the srcs a fact may point at.
pub fn day_srcs(day_text: &str) -> HashSet<String> {
    tpy::splitlines(day_text)
        .into_iter()
        .map(|line| tpy::head(line, 16))
        .filter(|h| SRC.is_match(h))
        .map(|h| h.to_string())
        .collect()
}

/// (record index, fact index) of every fact in `after` with a valid_to it did not have in `before`, in order. Any
/// date counts: a valid_to set ahead still hides the fact once that day comes.
fn new_closures(before: &Records, after: &[Record]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (ri, rec) in after.iter().enumerate() {
        let was: HashSet<String> = before
            .get(&rec.id)
            .map(|old| {
                old.facts.iter().map(|f| parse_fact(f)).filter(|f| !f.valid_to.is_empty()).map(|f| norm(&f.text)).collect()
            })
            .unwrap_or_default();
        for (i, f) in rec.facts.iter().map(|f| parse_fact(f)).enumerate() {
            if !f.valid_to.is_empty() && !was.contains(&norm(&f.text)) {
                out.push((ri, i));
            }
        }
    }
    out
}

/// How many facts `after` closes that `before` had open (see new_closures).
fn closures(before: &Records, after: &[Record]) -> usize {
    new_closures(before, after).len()
}

/// Keep the first `left` new closures in `after`; reopen the rest (valid_to cleared, in place). → one plain dated
/// correction per reopened fact, for the night's journal, so what the model saw is still written down.
fn hold_closures(before: &Records, after: &mut [Record], left: i64) -> Vec<String> {
    let mut held = Vec::new();
    let skip = left.max(0) as usize;
    for (ri, i) in new_closures(before, after).into_iter().skip(skip) {
        let mut f = parse_fact(&after[ri].facts[i]);
        held.push(format!("No longer true from {}: {}", f.valid_to, f.text));
        f.valid_to.clear();
        after[ri].facts[i] = render_fact(&f);
    }
    held
}

fn raw_of(rec: &Record) -> Value {
    json!({"id": rec.id, "type": rec.r#type, "aliases": rec.aliases, "facts": rec.facts})
}

fn closes_left() -> i64 {
    MAX_CLOSES_PER_NIGHT - NIGHT_CLOSED.with(|n| n.get())
}

fn spend_closes(n: i64) {
    if PIN.with(|p| p.borrow().is_some()) {
        NIGHT_CLOSED.with(|c| c.set(c.get() + n));
    }
}

fn same(a: Option<&Record>, b: &Record) -> bool {
    a.is_some_and(|a| a.r#type == b.r#type && a.aliases == b.aliases && a.facts == b.facts)
}

/// Write one dream: changed records, forgotten records, the profile, the journal. Malformed entries are skipped,
/// never fatal; nothing is written outside the records folder. `srcs`: the srcs a fact may carry (None: any
/// well-formed one).
pub fn apply_day(
    cfg: &RecallConfig,
    result: &Map<String, Value>,
    day: &str,
    srcs: Option<&HashSet<String>>,
) -> io::Result<DayChange> {
    let folder = records_dir(cfg);
    make_dir(&folder)?;
    let before = load_records(cfg);
    let (mut changed, mut created) = (0, 0);
    let mut written: Vec<Record> = Vec::new();
    for raw in iter_of(result.get("records")) {
        let Some(rec) = record_from(&raw, day, srcs) else {
            continue;
        };
        written.push(rec.clone());
        if same(before.get(&rec.id), &rec) {
            continue;
        }
        write(&folder.join(format!("{}.md", rec.id)), &render_record(&rec))?;
        if before.contains_key(&rec.id) {
            changed += 1;
        } else {
            created += 1;
        }
    }
    for rid in iter_of(result.get("forget")) {
        let rid = tpy::strip(&str_of(&rid)).to_lowercase();
        let path = folder.join(format!("{rid}.md"));
        if ID.is_match(&rid) && rid != PROFILE_ID && before.contains_key(&rid) && path.exists() {
            fs::remove_file(&path)?; // gone from the working tree; git history keeps it until a forget
            changed += 1;
        }
    }
    let empty = Map::new();
    let prof = result.get("profile").and_then(Value::as_object).unwrap_or(&empty);
    write(&folder.join(format!("{PROFILE_ID}.md")), &render_profile(prof, &load_records(cfg), day))?;
    let journal = result.get("journal").and_then(Value::as_object).unwrap_or(&empty);
    let journal_path = folder.join(DAYS_DIR).join(format!("{day}.md"));
    write(&journal_path, &render_journal(journal, day))?;
    let corrections = str_items(journal.get("corrections")).iter().filter(|c| !tpy::strip(c).is_empty()).count();
    Ok(DayChange { changed, created, corrections, journal: journal_path, closed: closures(&before, &written) })
}

/// Why a model reply was not a dream (each named as the Python exception it stands for).
#[derive(Debug, thiserror::Error)]
pub enum CallError {
    /// `json.loads` failed.
    #[error("the reply is not JSON")]
    Json,
    /// The JSON is not an object.
    #[error("not an object")]
    NotObject,
    /// The model returned no text.
    #[error("empty reply")]
    EmptyReply,
}

impl CallError {
    pub fn py_name(&self) -> &'static str {
        match self {
            CallError::Json => "JSONDecodeError",
            CallError::NotObject => "ValueError",
            CallError::EmptyReply => "RuntimeError",
        }
    }
}

/// The dream's two model calls: each takes the body and returns the reply's JSON text.
/// ([`OpenAIReconcileClient`], or a fake.)
pub trait DreamClient {
    fn reconcile(&self, body: &str) -> anyhow::Result<String>;
    fn consolidate(&self, body: &str) -> anyhow::Result<String>;
}

fn call(client: &dyn DreamClient, method: &str, body: &str) -> Option<Map<String, Value>> {
    let res = (|| -> anyhow::Result<Map<String, Value>> {
        let text = if method == "reconcile" { client.reconcile(body)? } else { client.consolidate(body)? };
        match tpy::loads(&text) {
            Some(Value::Object(o)) => Ok(o),
            Some(_) => Err(CallError::NotObject.into()),
            None => Err(CallError::Json.into()),
        }
    })();
    match res {
        Ok(o) => Some(o),
        Err(e) => {
            // a failed call is a night without a dream, never a crash
            warn!("records: the {method} call failed ({})", py::error_name(&e));
            None
        }
    }
}

fn render_all(records: &Records) -> String {
    records.values().map(render_record).collect::<Vec<_>>().join("\n\n")
}

/// One day of transcript into the records, the profile and the day's journal, in ONE model call.
///
/// Blocking: the dream runs it on a worker thread. → the change, or None when the day is empty, the day is not a
/// date or the call failed — and then nothing is written. One commit "dream: <day>" (after an as-found commit of
/// whatever the owner changed by hand).
pub fn reconcile_day(cfg: &RecallConfig, client: &dyn DreamClient, day: &str, day_text: &str) -> Option<DayChange> {
    if !DAY.is_match(day) || tpy::strip(day_text).is_empty() {
        return None;
    }
    let Ok(date) = NaiveDate::parse_from_str(day, "%Y-%m-%d") else {
        warn!("records: {day:?} is not a date — not dreamt");
        return None;
    };
    let folder = records_dir(cfg);
    let (git_ok, current, page, star_rows_, since) = {
        let _held = locked(&folder);
        let git_ok = ensure_repo(&folder);
        as_found(&folder, git_ok, &format!("dream: {day}"));
        let current = load_records(cfg);
        let page = profile_page(cfg);
        let rows = star_rows(cfg);
        let from = rows.len().saturating_sub(MAX_STARS_READ);
        let rows = rows[from..].to_vec();
        (git_ok, current, page, rows, pinned(&folder))
    };
    let srcs: Option<HashSet<String>> = if sources_on(None) {
        let mut s = day_srcs(day_text);
        s.extend(srcs_of(&current));
        Some(s)
    } else {
        None
    };
    let rendered = render_all(&current);
    let mut body = format!("## Records now\n\n{}", if rendered.is_empty() { "(none yet)" } else { &rendered });
    body += &format!("\n\n## The profile page now\n\n{}", if page.is_empty() { "(none yet)" } else { &page });
    if !star_rows_.is_empty() {
        let lines: Vec<String> = star_rows_.iter().map(|(t, d)| star_line(t, d)).collect();
        body += &format!("\n\n## What the owner said to remember for good\n\n{}", lines.join("\n"));
    }
    let after = (date + chrono::Duration::days(1)).format("%Y-%m-%d").to_string();
    body += &format!(
        "\n\n## Everything said in the transcript day {day}\n\nIt runs from {h:02}:00 on {day} to {h:02}:00 on {after}. \
         Each line starts with the real date and time it was said.\n\n{day_text}",
        h = DAY_START_HOUR
    );
    let mut result = call(client, "reconcile", &body)?;
    let res = (|| -> io::Result<Option<(DayChange, Option<String>)>> {
        let _held = locked(&folder);
        if generation(&folder) != since {
            info!("records: a forget ran during the dream of a day — nothing written, it is dreamt again");
            return Ok(None);
        }
        let mut proposed: Vec<Record> =
            iter_of(result.get("records")).iter().filter_map(|raw| record_from(raw, day, srcs.as_ref())).collect();
        let (closes, left) = (closures(&load_records(cfg), &proposed) as i64, closes_left());
        if closes > left {
            let held = hold_closures(&load_records(cfg), &mut proposed, left);
            warn!(
                "records: the dream of a day would close {closes} facts, over the {MAX_CLOSES_PER_NIGHT} a night may — {} held \
                 open and written as corrections",
                held.len()
            );
            let mut journal = result.get("journal").and_then(Value::as_object).cloned().unwrap_or_default();
            let mut corrections = iter_of(journal.get("corrections").filter(|v| py::truthy(v)));
            corrections.extend(held.into_iter().map(Value::String));
            journal.insert("corrections".into(), Value::Array(corrections));
            result.insert("records".into(), Value::Array(proposed.iter().map(raw_of).collect()));
            result.insert("journal".into(), Value::Object(journal));
        }
        let change = apply_day(cfg, &result, day, srcs.as_ref())?;
        spend_closes(change.closed as i64);
        let sha = if git_ok { commit(&folder, &format!("dream: {day}")) } else { None };
        Ok(Some((change, sha)))
    })();
    match res {
        Ok(Some((change, sha))) => {
            info!(
                "records: dreamt {day} — {} changed, {} new, {} corrected, {} closed{}",
                change.changed,
                change.created,
                change.corrections,
                change.closed,
                sha.map(|s| format!(", commit {s}")).unwrap_or_default()
            );
            Some(change)
        }
        Ok(None) => None,
        Err(e) => {
            warn!("records: could not write the dream of a day ({})", tpy::os_error_name(&e));
            None
        }
    }
}

fn rewrite_links(facts: &[String], renames: &IndexMap<String, String>, own: &str) -> Vec<String> {
    facts
        .iter()
        .map(|fact| {
            LINK.replace_all(fact, |c: &regex::Captures| {
                let target = renames.get(&c[1]).cloned().unwrap_or_else(|| c[1].to_string());
                // a record never links to itself
                if target == own { target } else { format!("[[{target}]]") }
            })
            .into_owned()
        })
        .collect()
}

/// One model call over every record and the profile: merge the same thing filed under two ids, date superseded
/// facts "(until …)". → (records changed, records removed). (0, 0) and no commit when nothing changed, when the call
/// failed, or when the records are over MAX_CONSOLIDATE_CHARS (logged once).
///
/// The model returns the merged record whole and says which id went into which; the code, not the model, rewrites
/// every [[old]] link to [[new]] and joins the aliases, so a merge can never strand a link or drop a name.
pub fn consolidate(cfg: &RecallConfig, client: &dyn DreamClient) -> (usize, usize) {
    let folder = records_dir(cfg);
    let (current, page, since) = {
        let _held = locked(&folder);
        (load_records(cfg), profile_page(cfg), pinned(&folder))
    };
    if current.len() < 2 && !current.values().any(|r| r.facts.len() > 1) {
        return (0, 0);
    }
    let rendered = render_all(&current);
    let size = tpy::len(&rendered);
    if size > MAX_CONSOLIDATE_CHARS {
        warn!("records: {size} chars of records is over one call's room — not consolidated tonight");
        return (0, 0);
    }
    let body = format!(
        "## Records\n\n{rendered}\n\n## The profile page\n\n{}",
        if page.is_empty() { "(none yet)" } else { &page }
    );
    let Some(result) = call(client, "consolidate", &body) else {
        return (0, 0);
    };
    let day = Local::now().format("%Y-%m-%d").to_string();
    let srcs = if sources_on(None) { Some(srcs_of(&current)) } else { None };
    let res = (|| -> io::Result<(usize, usize)> {
        let _held = locked(&folder);
        if generation(&folder) != since {
            // a forget ran during the call: its answer holds the words
            info!("records: a forget ran during the consolidation — nothing written");
            return Ok((0, 0));
        }
        let latest = load_records(cfg); // a star or an edit may have landed during the call
        let mut renames: IndexMap<String, String> = IndexMap::new();
        for m in iter_of(result.get("merged")) {
            let Some(m) = m.as_object() else {
                continue;
            };
            let old = tpy::strip(&str_or_empty(m.get("id"))).to_lowercase();
            let into = tpy::strip(&str_or_empty(m.get("into"))).to_lowercase();
            if old != into && latest.contains_key(&old) && ID.is_match(&into) && into != PROFILE_ID {
                renames.insert(old, into);
            }
        }
        let mut returned: Records = Records::new();
        for raw in iter_of(result.get("records")) {
            if let Some(r) = record_from(&raw, &day, srcs.as_ref()) {
                returned.insert(r.id.clone(), r);
            }
        }
        let mut list: Vec<Record> = returned.values().cloned().collect();
        let (mut closes, left) = (closures(&latest, &list) as i64, closes_left());
        if closes > left {
            // over the budget: the first ones close, the rest stay open
            hold_closures(&latest, &mut list, left);
            for rec in list.drain(..) {
                returned.insert(rec.id.clone(), rec);
            }
            warn!(
                "records: the consolidation would close {closes} facts, over what is left of the {MAX_CLOSES_PER_NIGHT} a night \
                 may — {} held open",
                closes - left.max(0)
            );
            closes = left.max(0);
        }
        spend_closes(closes);
        let olds: Vec<String> = renames.keys().cloned().collect();
        for old in olds {
            let Some(into) = renames.get(&old).cloned() else {
                continue;
            };
            let target = returned.get(&into).or_else(|| latest.get(&into)).cloned();
            let Some(target) = target.filter(|_| !renames.contains_key(&into)) else {
                // merged into nothing, or into a merged id: skip
                renames.shift_remove(&old);
                continue;
            };
            if !returned.contains_key(&into) {
                returned.insert(
                    into.clone(),
                    Record {
                        id: target.id.clone(),
                        r#type: target.r#type.clone(),
                        aliases: target.aliases.clone(),
                        facts: target.facts.clone(),
                        updated: day.clone(),
                    },
                );
            }
            let merged = returned.get_mut(&into).expect("inserted above");
            let mut names: Vec<String> = vec![old.clone()];
            names.extend(latest[&old].aliases.iter().cloned());
            for alias in names {
                let have: HashSet<String> = merged.aliases.iter().map(|a| a.to_lowercase()).collect();
                if !have.contains(&alias.to_lowercase()) && alias != into {
                    merged.aliases.push(alias);
                }
            }
            merged.aliases.truncate(MAX_ALIASES * 2);
        }
        let (mut changed, mut removed) = (0, 0);
        for rid in renames.keys() {
            match fs::remove_file(folder.join(format!("{rid}.md"))) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            removed += 1;
        }
        for (rid, rec) in &latest {
            // links into a merged id now point at its new home
            if renames.contains_key(rid) || returned.contains_key(rid) {
                continue;
            }
            let facts = rewrite_links(&rec.facts, &renames, rid);
            if facts != rec.facts {
                returned.insert(
                    rid.clone(),
                    Record {
                        id: rec.id.clone(),
                        r#type: rec.r#type.clone(),
                        aliases: rec.aliases.clone(),
                        facts,
                        updated: day.clone(),
                    },
                );
            }
        }
        for (rid, rec) in returned.iter_mut() {
            if renames.contains_key(rid) {
                continue;
            }
            rec.facts = rewrite_links(&rec.facts, &renames, rid);
            if same(latest.get(rid), rec) {
                continue;
            }
            write(&folder.join(format!("{rid}.md")), &render_record(rec))?;
            changed += 1;
        }
        if changed > 0 || removed > 0 {
            reindex_profile(cfg)?;
            if ensure_repo(&folder) {
                commit(&folder, "dream: consolidate");
            }
        }
        Ok((changed, removed))
    })();
    match res {
        Ok((changed, removed)) => {
            if changed > 0 || removed > 0 {
                info!("records: consolidated — {changed} changed, {removed} merged away");
            }
            (changed, removed)
        }
        Err(e) => {
            warn!("records: could not write the consolidation ({})", tpy::os_error_name(&e));
            (0, 0)
        }
    }
}

// ---- forget: owner-confirmed, every line, every file ---------------------------------------------

/// Frontmatter keeps its lines (a file must still parse), but not what it says of a forgotten thing: a matching
/// alias is dropped from the list, a matching title reads FORGOTTEN, and a record whose own id matches is about that
/// thing and goes whole (None). → (values removed, new head or None).
fn forget_head(head: &str, matches: &dyn Fn(&str) -> bool, record: bool) -> (usize, Option<String>) {
    let mut out: Vec<String> = Vec::new();
    let mut removed = 0;
    for line in head.split('\n') {
        let (key, sep, value) = match line.find(':') {
            Some(i) => (&line[..i], true, &line[i + 1..]),
            None => (line, false, ""),
        };
        let key = tpy::strip(key).to_lowercase();
        if !sep || tpy::strip(line).is_empty() || !matches(line) {
            out.push(line.to_string());
        } else if key == "id" && record {
            return (removed + 1, None);
        } else if key == "aliases" {
            let items: Vec<String> = tpy::strip(value)
                .trim_matches(['[', ']'])
                .split(',')
                .map(|a| tpy::strip(a).to_string())
                .filter(|a| !a.is_empty())
                .collect();
            let kept: Vec<&String> = items.iter().filter(|a| !matches(a.trim_matches(['"', '\'']))).collect();
            removed += items.len() - kept.len();
            out.push(format!("aliases: [{}]", kept.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
        } else if key == "title" {
            removed += 1;
            out.push(format!("title: {FORGOTTEN}"));
        } else {
            out.push(line.to_string());
        }
    }
    (removed, Some(out.join("\n")))
}

/// (lines removed, new text or None when the file should go).
fn forget_in(path: &Path, matches: &dyn Fn(&str) -> bool, record: bool) -> io::Result<(usize, Option<String>)> {
    let raw = read_text(path)?;
    let (head, body) = match FRONTMATTER.find(&raw) {
        Some(fm) => (&raw[..fm.end()], &raw[fm.end()..]),
        None => ("", raw.as_str()),
    };
    let (mut removed, new_head) = if head.is_empty() { (0, Some(String::new())) } else { forget_head(head, matches, record) };
    let mut kept: Vec<&str> = Vec::new();
    for line in body.split('\n') {
        if !tpy::strip(line).is_empty() && matches(line) {
            removed += 1;
        } else {
            kept.push(line);
        }
    }
    let Some(new_head) = new_head else {
        return Ok((removed + kept.iter().filter(|ln| !tpy::strip(ln).is_empty()).count(), None));
    };
    if removed == 0 {
        return Ok((0, Some(raw.clone())));
    }
    if record && !kept.iter().any(|ln| FACT_BULLET.is_match(ln)) {
        return Ok((removed, None)); // a record with no fact left is only a name: it goes
    }
    Ok((removed, Some(format!("{new_head}{}", kept.join("\n")))))
}

/// Remove every line `matches` accepts from the records, the profile, starred.md and the journals. → how many lines.
/// Frontmatter lines stay (the files must still parse) but lose what matched: an alias, a journal title. A record
/// whose id matches, or that is left with no fact, is removed whole. One commit "forget: N lines" (squash_history is
/// the caller's next step, so git forgets too). A real forget moves the forget generation in the same hold of the
/// lock, even when nothing here matched: a dream out on a model call may still hold the words.
pub fn forget_lines(cfg: &RecallConfig, matches: &dyn Fn(&str) -> bool, dry_run: bool) -> usize {
    let folder = records_dir(cfg);
    if !folder.is_dir() {
        if !dry_run {
            bump(&folder);
        }
        return 0;
    }
    let mut total = 0;
    let _held = locked(&folder);
    if !dry_run {
        bump(&folder);
    }
    let mut paths = md_files(&folder);
    paths.extend(md_files(&folder.join(DAYS_DIR)));
    for path in paths {
        let n_ = name(&path);
        let is_record = path.parent() == Some(folder.as_path())
            && stem(&path) != PROFILE_ID
            && n_ != STARRED_FILE
            && n_ != HIDDEN_FILE;
        let Ok((n, text)) = forget_in(&path, matches, is_record) else {
            continue;
        };
        if n == 0 {
            continue;
        }
        total += n;
        if dry_run {
            continue;
        }
        let res = (|| -> io::Result<()> {
            match text {
                None => fs::remove_file(&path),
                Some(_) if n_ == STARRED_FILE || n_ == HIDDEN_FILE => {
                    // a line that landed since the read is kept
                    let _g = if n_ == STARRED_FILE { STAR_LOCK.lock() } else { HIDE_LOCK.lock() };
                    if let (_, Some(text2)) = forget_in(&path, matches, false)? {
                        write(&path, &text2)?;
                    }
                    Ok(())
                }
                Some(text) => write(&path, &text),
            }
        })();
        if let Err(e) = res {
            warn!("records: could not forget in one file ({})", tpy::os_error_name(&e));
        }
    }
    if total > 0 && !dry_run && ensure_repo(&folder) {
        commit(&folder, &format!("forget: {total} lines"));
    }
    total
}

// ---- the dream's model client ----------------------------------------------------------------------

/// The openai SDK's `Response.output_text`: every `output_text` part of every `message` item, joined.
fn output_text(resp: &Value) -> String {
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

/// The dream's two calls, each one strict-JSON Responses call. store=False: the day's words are not kept on the
/// provider's side (owner, 2026-09-23: "nothing should leak out ever").
#[derive(Debug, Clone)]
pub struct OpenAIReconcileClient {
    pub model: String,
    pub consolidation_model: String,
    api_key: String,
    /// The client options (timeout RECONCILE_TIMEOUT_SECS); a test points `base_url` at a local server.
    pub opts: eve_openrouter::ClientOptions,
    /// Where the calls are metered (the spend ledger).
    pub ledger: eve_spend::Ledger,
}

impl OpenAIReconcileClient {
    /// `OpenAIReconcileClient(model, api_key, consolidation_model=...)`. A blank key falls back to
    /// OPENROUTER_API_KEY; none at all is `NoKey`, as `openrouter.client` raises.
    pub fn new(
        model: &str,
        api_key: Option<&str>,
        consolidation_model: &str,
    ) -> Result<Self, eve_openrouter::NoKey> {
        let opts = eve_openrouter::ClientOptions::default().timeout(Duration::from_secs_f64(RECONCILE_TIMEOUT_SECS));
        let api_key = eve_openrouter::key_or_raise(api_key, None)?;
        Ok(Self {
            model: model.to_string(),
            consolidation_model: consolidation_model.to_string(),
            api_key,
            opts,
            ledger: eve_spend::Ledger::default(),
        })
    }

    fn call_model(&self, model: &str, instructions: &str, body: &str, name: &str, schema: Value) -> anyhow::Result<String> {
        let request = json!({
            "model": model, "instructions": instructions, "input": body,
            "text": {"format": {"type": "json_schema", "name": name, "strict": true, "schema": schema}},
            "max_output_tokens": MAX_OUTPUT_TOKENS, "reasoning": {"effort": "low"}, "store": false,
        });
        // the blocking client runs on a thread of its own: it must never be built or dropped on an async runtime
        let (key, opts) = (self.api_key.clone(), self.opts.clone());
        let resp = std::thread::scope(|s| {
            s.spawn(move || -> Result<Value, eve_openrouter::Error> {
                eve_openrouter::client(Some(&key), opts)?.responses_create(request)
            })
            .join()
            .unwrap_or_else(|_| Err(eve_openrouter::Error::Invalid("the model call panicked".into())))
        })?;
        self.ledger.record_response(eve_spend::MEMORY, &resp, model, "openrouter", None);
        let text = tpy::strip(&output_text(&resp)).to_string();
        if text.is_empty() {
            return Err(CallError::EmptyReply.into());
        }
        Ok(text)
    }
}

impl DreamClient for OpenAIReconcileClient {
    fn reconcile(&self, body: &str) -> anyhow::Result<String> {
        let (v, s) = (validity_on(None), sources_on(None));
        self.call_model(&self.model, &reconcile_prompt(v, s), body, "dream_day", reconcile_schema(v, s))
    }

    fn consolidate(&self, body: &str) -> anyhow::Result<String> {
        let (v, s) = (validity_on(None), sources_on(None));
        self.call_model(&self.consolidation_model, &consolidate_prompt(v, s), body, "dream_consolidate", consolidate_schema(v, s))
    }
}

/// The dream's real client, or None (with one log line) when there is no key. `None` reads the process environment.
pub fn make_client(environ_: Option<&HashMap<String, String>>, model: &str) -> Option<OpenAIReconcileClient> {
    let env = match environ_ {
        Some(e) => e.clone(),
        None => environ(),
    };
    let key = eve_openrouter::key(Some(&env));
    if key.is_empty() {
        warn!("records: OPENROUTER_API_KEY not set — the records are read but never dreamt");
        return None;
    }
    match OpenAIReconcileClient::new(model, Some(&key), DEFAULT_CONSOLIDATION_MODEL) {
        Ok(c) => Some(c),
        Err(e) => {
            warn!("records: no client ({e}) — the records are read but never dreamt");
            None
        }
    }
}
