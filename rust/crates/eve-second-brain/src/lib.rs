//! The second brain: the owner's own notes, as a markdown vault eve can write to and read from.
//!
//! Port of `bridge/src/eve_bridge/second_brain.py`. records.py is what eve knows about its owner and never writes
//! mid-conversation. This is the other thing: what the **owner** writes down — a thought texted from the bus, a
//! todo, a line for the journal, a meeting to distil — and wants back later, in order. The design is PARA (Tiago
//! Forte) with a vision folder in front and processes and journals behind, after "The AI Second Brain" (Patrick
//! Ellis, Feb 2026):
//!
//! ```text
//!     00-vision/      mission, identity, principles, long-term goals
//!     01-inbox/       quick capture: dump anything, sort later
//!     02-todos/       master.md, one list, P0..P3
//!     03-projects/    active, ideally fifteen or fewer
//!     04-areas/       ongoing life areas
//!     05-resources/   reference
//!     06-processes/   the SOPs, and the context packs the workflows compile
//!     07-events/      meeting notes
//!     08-journals/    daily/YYYY-MM-DD.md, reflections, brain dumps
//!     09-archive/     done, out of sight
//!     CLAUDE.md       the map, for an agent reading the vault cold
//! ```
//!
//! Four decisions, all code:
//!
//! 1. **Plain markdown in a folder, and the folder is the Obsidian vault.** No database, no index to rebuild, no
//!    format of ours. The default root is [`DEFAULT_VAULT`]; `EVE_VAULT` overrides it.
//! 2. **Capture is seconds and never asks.** A text becomes a file in `01-inbox/` with frontmatter; a todo is one
//!    line appended to the master list; a journal line is appended to today's page. Classification is a handful of
//!    prefix rules ([`classify_capture`]), not a model call: a capture that waits on a model is a capture the owner
//!    stops making.
//! 3. **The agent can edit with a saved previous version; it never deletes files.** Filing changes `status:` and
//!    moves the file; a name clash gets `-2`, `-3`. Edits require the revision just read and keep undo history.
//!    [`read_note`] refuses any path that leaves the root or enters `.obsidian/`.
//! 4. **Workflows are prompts compiled from the vault, and the model is the caller's.** [`workflow_prompt`] = the
//!    SOP in `06-processes/<name>.md` + a context pack compiled to one `<context>` block + whatever the owner
//!    pasted. This module makes no model call.
//!
//! It ships OFF ([`SECOND_BRAIN_DEFAULT`]): the text brain gains nine tools when it is on, and a vault skeleton is
//! written on first use.

mod py;
mod texts;

use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use chrono::{Local, NaiveDateTime, TimeZone};
use indexmap::IndexMap;
use parking_lot::ReentrantMutex;
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

pub use texts::{DAILY_PLAN_CALENDAR, INSTRUCTIONS_BLOCK};

// ---- errors --------------------------------------------------------------------------------------

/// What Python raises here. `Value` is ValueError (and its subclasses UnicodeDecodeError, JSONDecodeError), `Os` an
/// OSError subclass named by `kind`; both are refusals the tools report as `{"ok": false, "reason": …}`. `Other` is
/// any other exception (TypeError from a malformed pack), which the tools report as `"<tool> failed: <kind>"`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BrainError {
    #[error("{0}")]
    Value(String),
    #[error("{message}")]
    Os { kind: &'static str, message: String },
    #[error("{message}")]
    Other { kind: &'static str, message: String },
}

impl BrainError {
    /// The Python exception class name, for the log line and the "failed" reason.
    pub fn type_name(&self) -> &'static str {
        match self {
            BrainError::Value(_) => "ValueError",
            BrainError::Os { kind, .. } | BrainError::Other { kind, .. } => kind,
        }
    }
}

fn value_err<T>(message: impl Into<String>) -> Result<T, BrainError> {
    Err(BrainError::Value(message.into()))
}

fn type_err(message: impl Into<String>) -> BrainError {
    BrainError::Other { kind: "TypeError", message: message.into() }
}

// ---- constants -----------------------------------------------------------------------------------

pub const SECOND_BRAIN_DEFAULT: bool = false;
// How the default was found (2026-09-21): Obsidian 1.13.7 was installed on this Mac at 19:48 the same day. Its
// registry did not exist yet and no `.obsidian` folder existed anywhere, so there was no vault to join, and the
// second brain IS the vault: this folder, with a minimal .obsidian/app.json so Obsidian can open it (Open folder as
// vault). Had a vault existed, the root would have been `<that vault>/Second Brain/` instead.
pub static DEFAULT_VAULT: LazyLock<PathBuf> = LazyLock::new(|| home().join("Documents").join("Second Brain"));
pub const VAULT_GUIDE: &str = "CLAUDE.md";
pub const OBSIDIAN_DIR: &str = ".obsidian";
pub const TODOS_FILE: &str = "02-todos/master.md";
pub const JOURNAL_DIR: &str = "08-journals/daily";
pub const INBOX_DIR: &str = "01-inbox";
pub const PROJECTS_DIR: &str = "03-projects";
pub const AREAS_DIR: &str = "04-areas";
pub const PROCESSES_DIR: &str = "06-processes";
pub const PACKS_DIR: &str = "06-processes/packs";
pub const ARCHIVE_DIR: &str = "09-archive";
pub const PRIORITIES: [&str; 4] = ["P0", "P1", "P2", "P3"];
pub const DEFAULT_PRIORITY: &str = "P2";
pub const KINDS: [&str; 4] = ["note", "todo", "journal", "idea"];
pub const WORKFLOWS: [&str; 4] = ["daily-plan", "weekly-review", "triage-inbox", "distill-chat"];
// The day's plan is written by the brain that holds the calendar tools, in the same turn: the vault has no
// calendar, and a second model given only this prompt planned a day blind (2026-09-24 08:44, 39 s, 3 calls).
/// `WORKFLOW_NOTES`: the note returned with a compiled workflow, by name.
pub static WORKFLOW_NOTES: LazyLock<IndexMap<&'static str, &'static str>> =
    LazyLock::new(|| IndexMap::from([("daily-plan", texts::DAILY_PLAN_NOTE)]));
pub const SELECTS: [&str; 2] = ["modified_desc", "filename_desc"];
pub const MAX_SLUG_WORDS: usize = 6;
pub const MAX_SLUG_CHARS: usize = 48;
pub const MAX_TITLE_CHARS: usize = 80;
pub const MAX_SNIPPET_CHARS: usize = 200;
pub const MAX_NOTE_CHARS: usize = 6000; // read_note's default clip: a note is a page, not a book
pub const MAX_TOOL_TEXT_CHARS: usize = 4000; // a tool result goes back into a chat turn; a clipped note is honest
pub const MAX_PROMPT_CHARS: usize = 60000; // workflow prompts go into a model's input, which is bounded
pub const MAX_HITS: usize = 8;
pub const CHARS_PER_TOKEN: usize = 4; // the deck's estimate, and good enough for a budget
pub const DEFAULT_MAX_PER_GLOB: i64 = 10;
pub const HISTORY_DIR: &str = ".eve-history";
pub const TRASH_DIR: &str = ".eve-trash"; // the owner's deletes from the Notes app, restorable (restore_note)
pub const MAX_PREVIEW_CHARS: usize = 120;
/// `_NOTE_LOCK`: one re-entrant lock for every read-modify-write of a note, process-wide, as Python's RLock.
static NOTE_LOCK: ReentrantMutex<()> = ReentrantMutex::new(());

pub const PARA: [(&str, &str); 10] = [
    ("00-vision", "Mission, identity, principles, long-term goals"),
    ("01-inbox", "Quick capture: dump anything here, sort it later"),
    ("02-todos", "The master task list, one file, P0 to P3"),
    ("03-projects", "Active projects with an end (ideally fifteen or fewer), one folder or file each"),
    ("04-areas", "Ongoing areas of life with no end: health, money, home, family, craft"),
    ("05-resources", "Reference material: things worth keeping that are not about a project"),
    ("06-processes", "SOPs and reviews (daily, weekly, quarterly), and the context packs agents compile"),
    ("07-events", "Meeting and event notes, one file per event"),
    ("08-journals", "Reflections, brain dumps and the daily log (daily/YYYY-MM-DD.md)"),
    ("09-archive", "Completed and out of sight; the folder structure above is kept underneath"),
];
pub const PARA_FOLDERS: [&str; 10] = [
    "00-vision",
    "01-inbox",
    "02-todos",
    "03-projects",
    "04-areas",
    "05-resources",
    "06-processes",
    "07-events",
    "08-journals",
    "09-archive",
];

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a constant pattern compiles")
}

static WORD: LazyLock<Regex> = LazyLock::new(|| re(r"[a-z0-9]+"));
static PRIORITY: LazyLock<Regex> = LazyLock::new(|| re(r"(?is)\A\s*(p[0-3])\s+(.+)$"));
static FRONTMATTER: LazyLock<Regex> = LazyLock::new(|| re(r"(?s)\A---\n(.*?)\n---[ \t]*(?:\n|\z)(.*)"));
static HEADING: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^#\s+(.+?)\s*$"));
static OPEN_TODO: LazyLock<Regex> = LazyLock::new(|| re(r"\A\s*[-*]\s+\[ \]\s+(.+?)\s*$"));
// Python's `$` without MULTILINE also matches before one final newline; `\n?\z` keeps that.
static SAFE_SEGMENT: LazyLock<Regex> = LazyLock::new(|| re(r"\A[A-Za-z0-9][A-Za-z0-9 ._-]{0,79}\n?\z"));
static CHANGE_ID: LazyLock<Regex> = LazyLock::new(|| re(r"\A\d{8}T\d{12}-[a-f0-9]{32}\z"));
static PACK_NAME: LazyLock<Regex> = LazyLock::new(|| re(r"\A[a-z0-9][a-z0-9-]{0,40}\z"));
static INT: LazyLock<Regex> = LazyLock::new(|| re(r"\A-?[0-9]+\z"));
static PREVIEW_MARK: LazyLock<Regex> =
    LazyLock::new(|| re(r"\A(#{1,6}\s+|[-*+]\s+\[[ xX]\]\s+|[-*+]\s+|\d+[.)]\s+|>\s*)"));
static PREVIEW_INLINE: LazyLock<Regex> = LazyLock::new(|| re(r"[*_`~]|\[\[|\]\]"));

fn home() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(h) if !h.is_empty() => PathBuf::from(h),
        _ => dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
    }
}

// ---- results -------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CaptureResult {
    pub path: String,
    pub kind: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hit {
    pub path: String,
    pub title: String,
    pub snippet: String,
    pub score: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InboxItem {
    pub path: String,
    pub title: String,
    pub created: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Include {
    pub path: String,
    pub select: String,
    pub count: Option<i64>,
}

impl Include {
    /// `Include(path)`: select `filename_desc`, no count.
    pub fn new(path: impl Into<String>) -> Self {
        Include { path: path.into(), select: "filename_desc".to_string(), count: None }
    }

    pub fn with(path: impl Into<String>, select: &str, count: Option<i64>) -> Self {
        Include { path: path.into(), select: select.to_string(), count }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackDef {
    pub name: String,
    pub description: String,
    pub budget_tokens: i64,
    pub include: Vec<Include>,
    pub exclude: Vec<String>,
    pub prefer_files: Vec<String>,
    pub max_per_glob: i64,
}

impl PackDef {
    /// `PackDef(name, description, budget_tokens)` with the dataclass defaults for the rest.
    pub fn new(name: impl Into<String>, description: impl Into<String>, budget_tokens: i64) -> Self {
        PackDef {
            name: name.into(),
            description: description.into(),
            budget_tokens,
            include: Vec::new(),
            exclude: Vec::new(),
            prefer_files: Vec::new(),
            max_per_glob: DEFAULT_MAX_PER_GLOB,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Compiled {
    pub xml: String,
    pub files: Vec<String>,
    pub tokens: usize,
    pub dropped: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultConfig {
    pub enabled: bool,
    pub root: PathBuf,
}

/// A row of [`list_notes`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoteRow {
    pub path: String,
    pub folder: String,
    pub title: String,
    pub preview: String,
    pub modified: i64,
}

// ---- the skeleton --------------------------------------------------------------------------------

fn guide_text() -> String {
    let rows: Vec<String> = PARA.iter().map(|(name, what)| format!("| `{name}/` | {what} |")).collect();
    texts::GUIDE_TEMPLATE.replacen("{rows}", &rows.join("\n"), 1)
}

fn todos_skeleton() -> &'static str {
    texts::TODOS_SKELETON
}

fn vision_template() -> &'static str {
    texts::VISION_TEMPLATE
}

fn sop(name: &str) -> &'static str {
    match name {
        "daily-plan" => texts::SOP_DAILY_PLAN,
        "weekly-review" => texts::SOP_WEEKLY_REVIEW,
        "triage-inbox" => texts::SOP_TRIAGE_INBOX,
        _ => texts::SOP_DISTILL_CHAT,
    }
}

fn pack_text(name: &str) -> &'static str {
    match name {
        "daily-plan" => texts::PACK_DAILY_PLAN,
        "weekly-review" => texts::PACK_WEEKLY_REVIEW,
        "triage-inbox" => texts::PACK_TRIAGE_INBOX,
        _ => texts::PACK_DISTILL_CHAT,
    }
}

/// `_write_new`: write a file only if it does not exist; record what was written.
fn write_new(path: &Path, text: &str, created: &mut Vec<String>, root: &Path) -> Result<(), BrainError> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        py::mkdirs(parent)?;
    }
    py::write_text(path, text)?;
    created.push(py::posix(path.strip_prefix(root).unwrap_or(path)));
    Ok(())
}

/// A minimal `.obsidian/app.json` so Obsidian can open the root, unless the root is already inside a vault.
pub fn ensure_obsidian(root: &Path) -> Result<Vec<String>, BrainError> {
    for folder in root.ancestors() {
        let folder = if folder.as_os_str().is_empty() { Path::new(".") } else { folder };
        if folder.join(OBSIDIAN_DIR).is_dir() {
            return Ok(Vec::new());
        }
    }
    let mut created = Vec::new();
    write_new(&root.join(OBSIDIAN_DIR).join("app.json"), "{}\n", &mut created, root)?;
    Ok(created)
}

/// Create the PARA skeleton, the guide, the SOPs and the packs. Idempotent: never overwrites. Returns what it
/// created.
pub fn ensure_vault(root: &Path) -> Result<Vec<String>, BrainError> {
    let mut created = Vec::new();
    for (name, _) in PARA {
        py::mkdirs(&root.join(name))?;
    }
    py::mkdirs(&root.join(JOURNAL_DIR))?;
    py::mkdirs(&root.join(PACKS_DIR))?;
    write_new(&root.join(VAULT_GUIDE), &guide_text(), &mut created, root)?;
    write_new(&root.join(TODOS_FILE), todos_skeleton(), &mut created, root)?;
    write_new(&root.join("00-vision").join("README.md"), vision_template(), &mut created, root)?;
    for name in WORKFLOWS {
        write_new(&root.join(PROCESSES_DIR).join(format!("{name}.md")), sop(name), &mut created, root)?;
        write_new(&root.join(PACKS_DIR).join(format!("{name}.pack")), pack_text(name), &mut created, root)?;
    }
    created.extend(ensure_obsidian(root)?);
    if !created.is_empty() {
        tracing::info!("second brain: vault skeleton at {}: {} file(s) created", root.display(), created.len());
    }
    Ok(created)
}

// ---- paths and frontmatter -----------------------------------------------------------------------

/// The absolute path for a relative one, or ValueError when it leaves the root or enters a hidden folder.
fn resolve(root: &Path, rel_path: &str) -> Result<PathBuf, BrainError> {
    let rel = py::strip(rel_path).replace('\\', "/");
    if rel.is_empty() || rel.starts_with('/') || rel.starts_with('~') {
        return value_err("path must be relative to the vault");
    }
    if rel.contains('\0') {
        // os.path.realpath's lstat raises ValueError here, before any file is touched
        return value_err("lstat: embedded null character in path");
    }
    let root_r = py::resolve(root);
    let target = py::resolve(&root_r.join(&rel));
    if target != root_r && !target.starts_with(&root_r) {
        return value_err("path leaves the vault");
    }
    let rel_parts = py::relative_to(&target, &root_r)?;
    if py::parts(&rel_parts).iter().any(|p| p.starts_with('.')) {
        return value_err("hidden paths are off limits");
    }
    Ok(target)
}

/// `_rel`: the vault-relative posix path of a path, resolved; ValueError when it resolves outside the root.
fn rel(root: &Path, path: &Path) -> Result<String, BrainError> {
    let rel = py::relative_to(&py::resolve(path), &py::resolve(root))?;
    Ok(py::posix(&rel))
}

/// `(meta, body)`; meta is empty when there is no frontmatter. Tolerant: the owner edits these by hand.
pub fn split_frontmatter(text: &str) -> (IndexMap<String, String>, String) {
    let Some(m) = FRONTMATTER.captures(text) else {
        return (IndexMap::new(), text.to_string());
    };
    let mut meta = IndexMap::new();
    for line in py::splitlines(&m[1]) {
        if line.contains(':') && !line.starts_with([' ', '\t', '-']) {
            let (key, value) = line.split_once(':').unwrap_or((line, ""));
            meta.insert(py::strip(key).to_lowercase(), py::strip_chars(py::strip(value), "\"'").to_string());
        }
    }
    (meta, m[2].to_string())
}

/// The frontmatter prefix of a note (everything before the body), or "" when it has none.
fn frontmatter_prefix(text: &str) -> &str {
    match FRONTMATTER.captures(text).and_then(|m| m.get(2)) {
        Some(body) => &text[..body.start()],
        None => "",
    }
}

fn set_frontmatter(text: &str, key: &str, value: &str) -> String {
    let Some(m) = FRONTMATTER.captures(text) else {
        return format!("---\n{key}: {value}\n---\n{text}");
    };
    let mut head: Vec<String> = py::splitlines(&m[1]).into_iter().map(String::from).collect();
    match head.iter().position(|line| py::strip(line.split(':').next().unwrap_or("")).to_lowercase() == key) {
        Some(i) => head[i] = format!("{key}: {value}"),
        None => head.push(format!("{key}: {value}")),
    }
    format!("---\n{}\n---\n{}", head.join("\n"), &m[2])
}

pub fn title_of(text: &str, fallback: &str) -> String {
    let (meta, body) = split_frontmatter(text);
    if let Some(title) = meta.get("title").filter(|t| !t.is_empty()) {
        return py::clip(title, MAX_TITLE_CHARS).to_string();
    }
    if let Some(h) = HEADING.captures(&body) {
        return py::clip(&h[1], MAX_TITLE_CHARS).to_string();
    }
    for line in py::splitlines(&body) {
        if !py::strip(line).is_empty() {
            let cleaned = py::strip(py::lstrip_chars(py::strip(line), "-*# "));
            return py::clip(cleaned, MAX_TITLE_CHARS).to_string();
        }
    }
    fallback.to_string()
}

/// lowercase ascii hyphen-words: at most six words and 48 characters; never empty.
pub fn slugify(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let ascii: String = text.nfkd().filter(char::is_ascii).collect::<String>().to_lowercase();
    let words: Vec<&str> = WORD.find_iter(&ascii).take(MAX_SLUG_WORDS).map(|m| m.as_str()).collect();
    let joined = words.join("-");
    let slug = py::clip(&joined, MAX_SLUG_CHARS).trim_end_matches('-');
    if slug.is_empty() {
        "note".to_string()
    } else {
        slug.to_string()
    }
}

/// The same path, or `name-2`, `name-3` … when it is taken. Never overwrite.
fn unclashed(path: &Path) -> Result<PathBuf, BrainError> {
    if !path.exists() {
        return Ok(path.to_path_buf());
    }
    let name = py::file_name(path);
    let (stem, suffix) = py::stem_suffix(&name);
    for n in 2..1000 {
        let candidate = path.with_file_name(format!("{stem}-{n}{suffix}"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(BrainError::Os { kind: "FileExistsError", message: name })
}

// ---- capture -------------------------------------------------------------------------------------

/// `(kind, cleaned_text)` by prefix rules only. No model: capture must take seconds and never ask.
pub fn classify_capture(text: &str) -> (&'static str, String) {
    let raw = py::strip(text);
    let low = raw.to_lowercase();
    if low.starts_with("- [ ]") {
        return ("todo", py::strip(py::skip(raw, 5)).to_string());
    }
    for prefix in ["todo:", "task:", "todo ", "task "] {
        if low.starts_with(prefix) && py::char_len(raw) > prefix.len() {
            return ("todo", py::strip(py::skip(raw, prefix.len())).to_string());
        }
    }
    if PRIORITY.is_match(raw) {
        return ("todo", raw.to_string());
    }
    if low.starts_with("journal:") {
        return ("journal", py::strip(py::skip(raw, 8)).to_string());
    }
    if low.starts_with("dear diary") {
        let rest = py::strip(py::lstrip_chars(py::skip(raw, 10), " ,.:;!-"));
        return ("journal", if rest.is_empty() { raw.to_string() } else { rest.to_string() });
    }
    if low.starts_with("today i") {
        return ("journal", raw.to_string());
    }
    if low.starts_with("idea:") {
        return ("idea", py::strip(py::skip(raw, 5)).to_string());
    }
    if low.starts_with("note:") {
        return ("note", py::strip(py::skip(raw, 5)).to_string());
    }
    ("note", raw.to_string())
}

fn priority_of(text: &str) -> (String, String) {
    match PRIORITY.captures(text) {
        Some(m) => (m[1].to_uppercase(), py::strip(&m[2]).to_string()),
        None => (DEFAULT_PRIORITY.to_string(), py::strip(text).to_string()),
    }
}

/// Append `line` at the end of the `## heading` section (created at the end if missing).
fn append_under(text: &str, heading: &str, line: &str) -> String {
    #[cfg(test)]
    seams::before_append_under();
    let mut lines: Vec<String> = py::splitlines(text).into_iter().map(String::from).collect();
    let target = format!("## {heading}");
    let Some(start) = lines.iter().position(|ln| py::strip(ln) == target) else {
        if lines.last().map(|l| !py::strip(l).is_empty()).unwrap_or(false) {
            lines.push(String::new());
        }
        lines.push(target);
        lines.push(line.to_string());
        return lines.join("\n") + "\n";
    };
    let end = (start + 1..lines.len()).find(|&j| lines[j].starts_with("## ")).unwrap_or(lines.len());
    let mut insert = end;
    while insert > start + 1 && py::strip(&lines[insert - 1]).is_empty() {
        insert -= 1;
    }
    lines.insert(insert, line.to_string());
    if insert + 1 < lines.len() && lines[insert + 1].starts_with("## ") {
        lines.insert(insert + 1, String::new());
    }
    lines.join("\n") + "\n"
}

fn now_local() -> NaiveDateTime {
    Local::now().naive_local()
}

/// One text → one file (note, idea), one todo line, or one journal line. Seconds, no questions.
///
/// Python's defaults are `kind="note"`, `source="telegram"`, `now=None` (the local time).
pub fn capture(
    root: &Path,
    text: &str,
    kind: &str,
    source: &str,
    now: Option<NaiveDateTime>,
) -> Result<CaptureResult, BrainError> {
    let body = py::strip(text);
    if body.is_empty() {
        return value_err("nothing to capture");
    }
    if !KINDS.contains(&kind) {
        return value_err(format!("unknown kind {}", py::repr(kind)));
    }
    ensure_vault(root)?;
    let when = now.unwrap_or_else(now_local);
    let day = when.format("%Y-%m-%d").to_string();
    // Every write below holds NOTE_LOCK, as edit_note and undo_note do: an unlocked read-modify-write of todos.md
    // racing an edit_note on the same file silently lost one of the two changes.
    if kind == "todo" {
        let (priority, item) = priority_of(body);
        let item = py::collapse_ws(&item);
        let path = root.join(TODOS_FILE);
        {
            let _guard = NOTE_LOCK.lock();
            let current =
                if path.exists() { py::read_text(&path, false)? } else { todos_skeleton().to_string() };
            atomic_text(&path, &append_under(&current, &priority, &format!("- [ ] {item} (added {day})")))?;
        }
        tracing::info!("second brain: todo captured under {}", priority);
        return Ok(CaptureResult {
            path: TODOS_FILE.to_string(),
            kind: kind.to_string(),
            title: py::clip(&item, MAX_TITLE_CHARS).to_string(),
        });
    }
    if kind == "journal" {
        let path = root.join(JOURNAL_DIR).join(format!("{day}.md"));
        if let Some(parent) = path.parent() {
            py::mkdirs(parent)?;
        }
        let line = py::collapse_ws(body);
        {
            let _guard = NOTE_LOCK.lock();
            if !path.exists() {
                py::write_text(&path, &format!("# {day}\n\n"))?;
            }
            let mut fh = fs::OpenOptions::new().append(true).create(true).open(&path).map_err(|e| py::os_error(&e, &path))?;
            fh.write_all(format!("- {} {line}\n", when.format("%H:%M")).as_bytes())
                .map_err(|e| py::os_error(&e, &path))?;
        }
        tracing::info!("second brain: journal line appended to {}", py::file_name(&path));
        return Ok(CaptureResult {
            path: rel(root, &path)?,
            kind: kind.to_string(),
            title: py::clip(&line, MAX_TITLE_CHARS).to_string(),
        });
    }
    let first_line = py::splitlines(body).into_iter().next().unwrap_or("");
    let slug = slugify(first_line);
    let note = format!(
        "---\ncreated: {}\nsource: {source}\nkind: {kind}\nstatus: inbox\n---\n{body}\n",
        when.format("%Y-%m-%dT%H:%M")
    );
    let path = {
        let _guard = NOTE_LOCK.lock();
        let path = unclashed(&root.join(INBOX_DIR).join(format!("{}-{slug}.md", when.format("%Y-%m-%d-%H%M"))))?;
        py::write_text(&path, &note)?;
        path
    };
    tracing::info!("second brain: {} captured to {}", kind, py::file_name(&path));
    Ok(CaptureResult { path: rel(root, &path)?, kind: kind.to_string(), title: title_of(&note, &slug) })
}

// ---- reading -------------------------------------------------------------------------------------

fn notes(root: &Path, include_archive: bool) -> Vec<PathBuf> {
    if !root.is_dir() {
        return Vec::new();
    }
    let mut found = py::glob(root, "**/*.md").unwrap_or_default();
    found.sort_by_cached_key(|p| py::parts(p));
    let mut out = Vec::new();
    for path in found {
        let rel_parts = py::parts(path.strip_prefix(root).unwrap_or(&path));
        if rel_parts.iter().any(|p| p.starts_with('.')) {
            continue;
        }
        if !include_archive && rel_parts.first().map(String::as_str) == Some(ARCHIVE_DIR) {
            continue;
        }
        out.push(path);
    }
    out
}

/// Keyword scorer over every note: a word in the file name counts three, a line containing it one.
///
/// Python's defaults are `limit=MAX_HITS`, `include_archive=False`.
pub fn search(root: &Path, query: &str, limit: usize, include_archive: bool) -> Result<Vec<Hit>, BrainError> {
    let lowered = query.to_lowercase();
    let words: Vec<&str> = WORD.find_iter(&lowered).map(|m| m.as_str()).filter(|w| w.len() > 1).collect();
    if words.is_empty() {
        return Ok(Vec::new());
    }
    let mut hits = Vec::new();
    for path in notes(root, include_archive) {
        let rel_path = rel(root, &path)?;
        let Ok(text) = py::read_text(&path, true) else { continue };
        let rel_lower = rel_path.to_lowercase();
        let name_words: std::collections::HashSet<&str> = WORD.find_iter(&rel_lower).map(|m| m.as_str()).collect();
        let mut score = 3 * words.iter().filter(|w| name_words.contains(*w)).count() as i64;
        let (_, body) = split_frontmatter(&text);
        let (mut best_line, mut best_n) = (String::new(), 0);
        for line in py::splitlines(&body) {
            let low = line.to_lowercase();
            let n = words.iter().filter(|w| low.contains(*w)).count();
            if n > 0 {
                score += n as i64;
                if n > best_n {
                    best_line = py::strip(line).to_string();
                    best_n = n;
                }
            }
        }
        if score > 0 {
            hits.push(Hit {
                path: rel_path,
                title: title_of(&text, &py::stem(&path)),
                snippet: py::clip(&best_line, MAX_SNIPPET_CHARS).to_string(),
                score,
            });
        }
    }
    hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
    hits.truncate(limit.max(1));
    Ok(hits)
}

/// One note, clipped. Refuses anything outside the root or inside `.obsidian`. Python's default `max_chars` is
/// [`MAX_NOTE_CHARS`].
pub fn read_note(root: &Path, rel_path: &str, max_chars: usize) -> Result<Value, BrainError> {
    let path = match resolve(root, rel_path) {
        Ok(p) => p,
        Err(BrainError::Value(reason)) => return Ok(json!({"ok": false, "reason": reason})),
        Err(e) => return Err(e),
    };
    if !path.is_file() {
        return Ok(json!({"ok": false, "reason": "no such note"}));
    }
    let text = py::read_text(&path, true)?;
    let chars = py::char_len(&text);
    let rel_path = rel(root, &path)?;
    let undo_id = latest_edit(root, &rel_path, &text)?;
    Ok(json!({"ok": true, "path": rel_path, "title": title_of(&text, &py::stem(&path)),
              "text": py::clip(&text, max_chars), "clipped": chars > max_chars, "chars": chars,
              "revision": revision(&text), "undo_id": undo_id}))
}

// ---- editing -------------------------------------------------------------------------------------

fn revision(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// Private history, never exposed by search or context packs. Refuse redirected history folders.
fn history_dir(root: &Path, rel_path: &str) -> Result<PathBuf, BrainError> {
    let base = py::resolve(root).join(HISTORY_DIR);
    let folder = base.join(revision(rel_path));
    if py::is_symlink(&base) || py::is_symlink(&folder) {
        return value_err("note history must not be a symlink");
    }
    Ok(folder)
}

fn latest_edit(root: &Path, rel_path: &str, text: &str) -> Result<Option<String>, BrainError> {
    let folder = history_dir(root, rel_path)?;
    let rev = revision(text);
    let mut records = py::glob(&folder, "*.json")?;
    records.sort_by_cached_key(|p| py::parts(p));
    records.reverse();
    for record in records {
        if py::is_symlink(&record) {
            continue;
        }
        let Ok(raw) = py::read_text(&record, false) else { continue };
        let Ok(data) = serde_json::from_str::<Value>(&raw) else { continue };
        if data.get("path").and_then(Value::as_str) == Some(rel_path)
            && data.get("after").and_then(Value::as_str) == Some(rev.as_str())
        {
            return Ok(Some(py::stem(&record)));
        }
    }
    Ok(None)
}

/// Replace a complete file on the same filesystem, keeping its permissions.
fn atomic_text(path: &Path, text: &str) -> Result<(), BrainError> {
    #[cfg(test)]
    if let Some(result) = seams::atomic_text(path, text) {
        return result;
    }
    atomic_text_real(path, text)
}

fn atomic_text_real(path: &Path, text: &str) -> Result<(), BrainError> {
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).map(|m| m.permissions().mode() & 0o777).unwrap_or(0o600)
    };
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut file = tempfile::Builder::new().prefix(".eve-").tempfile_in(parent).map_err(|e| py::os_error(&e, parent))?;
    file.write_all(text.as_bytes()).map_err(|e| py::os_error(&e, file.path()))?;
    file.flush().map_err(|e| py::os_error(&e, file.path()))?;
    file.as_file().sync_all().map_err(|e| py::os_error(&e, file.path()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(file.path(), fs::Permissions::from_mode(mode)).map_err(|e| py::os_error(&e, file.path()))?;
    }
    // The temporary file is removed on drop unless the replace succeeded.
    let temporary = file.into_temp_path();
    py::replace(&temporary, path)?;
    let _ = temporary.keep();
    Ok(())
}

/// Replace one exact body passage, or append when old_text is empty. Save undo before writing.
pub fn edit_note(
    root: &Path,
    rel_path: &str,
    rev: &str,
    old_text: &str,
    new_text: &str,
) -> Result<Value, BrainError> {
    let _guard = NOTE_LOCK.lock();
    let path = resolve(root, rel_path)?;
    if !path.is_file() || py::suffix(&path) != ".md" {
        return value_err("no such markdown note");
    }
    let current = py::read_text(&path, false)?;
    if rev.is_empty() || revision(&current) != rev {
        return value_err("note changed; read_note again before editing");
    }
    let prefix = frontmatter_prefix(&current);
    let body = &current[prefix.len()..];
    let updated = if !old_text.is_empty() {
        if body.matches(old_text).count() != 1 {
            return value_err("old_text must match exactly once in the note body; read_note and use more context");
        }
        format!("{prefix}{}", body.replacen(old_text, new_text, 1))
    } else {
        if py::strip(new_text).is_empty() {
            return value_err("nothing to append");
        }
        let sep = if !current.is_empty() && !current.ends_with('\n') { "\n" } else { "" };
        format!("{current}{sep}{}\n", new_text.trim_end_matches('\n'))
    };
    if updated == current {
        return Ok(json!({"ok": true, "path": rel(root, &path)?, "changed": false, "revision": rev}));
    }
    let rel_path = rel(root, &path)?;
    let change_id = write_with_undo(root, &path, &rel_path, &current, &updated)?;
    tracing::info!("second brain: edited {}", py::file_name(&path));
    Ok(json!({"ok": true, "path": rel_path, "changed": true, "revision": revision(&updated), "undo_id": change_id}))
}

fn change_id() -> String {
    format!("{}-{}", now_local().format("%Y%m%dT%H%M%S%6f"), uuid::Uuid::new_v4().simple())
}

/// Save the previous text as an undo record, then replace the note. Returns the record's id (undo_note's).
fn write_with_undo(root: &Path, path: &Path, rel_path: &str, current: &str, updated: &str) -> Result<String, BrainError> {
    let folder = history_dir(root, rel_path)?;
    py::mkdir_mode(&folder, 0o700, true)?;
    let id = change_id();
    let record = folder.join(format!("{id}.json"));
    let data = format!(
        "{{\"path\": {}, \"before\": {}, \"after\": {}}}",
        py::json_str(rel_path),
        py::json_str(current),
        py::json_str(&revision(updated))
    );
    atomic_text(&record, &data)?;
    let attempt = (|| {
        if py::read_text(path, false)? != current {
            return value_err("note changed; read_note again before editing");
        }
        atomic_text(path, updated)
    })();
    if let Err(e) = attempt {
        // Retain the backup for inspection, but never offer a failed edit as undoable.
        py::rename(&record, &py::with_suffix(&record, ".failed"))?;
        return Err(e);
    }
    Ok(id)
}

/// Restore a saved edit only while its result is still the current note.
pub fn undo_note(root: &Path, rel_path: &str, undo_id: &str, rev: &str) -> Result<Value, BrainError> {
    let _guard = NOTE_LOCK.lock();
    let path = resolve(root, rel_path)?;
    if !path.is_file() || py::suffix(&path) != ".md" {
        return value_err("no such markdown note");
    }
    if !CHANGE_ID.is_match(undo_id) {
        return value_err("use an undo_id returned by edit_note or read_note");
    }
    let rel_path = rel(root, &path)?;
    let record = history_dir(root, &rel_path)?.join(format!("{undo_id}.json"));
    if py::is_symlink(&record) {
        return value_err("note history must not be a symlink");
    }
    let data: Value = parse_json(&py::read_text(&record, false)?)?;
    let current = py::read_text(&path, false)?;
    let before = match (&data, data.get("before")) {
        (Value::Object(_), Some(Value::String(before))) if data.get("path").and_then(Value::as_str) == Some(rel_path.as_str()) => {
            before.clone()
        }
        _ => return value_err("invalid note history"),
    };
    if rev.is_empty() || rev != revision(&current) || data.get("after").and_then(Value::as_str) != Some(rev) {
        return value_err("note changed since that edit; read_note before choosing what to undo");
    }
    atomic_text(&path, &before)?;
    py::rename(&record, &py::with_suffix(&record, ".undone"))?;
    tracing::info!("second brain: undid an edit to {}", py::file_name(&path));
    let undo = latest_edit(root, &rel_path, &before)?;
    Ok(json!({"ok": true, "path": rel_path, "revision": revision(&before), "undo_id": undo}))
}

/// `json.loads`: a malformed record is a ValueError (JSONDecodeError).
fn parse_json(text: &str) -> Result<Value, BrainError> {
    serde_json::from_str(text).map_err(|e| BrainError::Value(e.to_string()))
}

// ---- the owner's own edits (the Notes app, link_notes.py) ------------------------------------------
// The agent never deletes and edits one passage at a time (decision 3 above). The owner is not the agent: from the
// Notes app they replace a whole page, start one, move one and delete one. A delete is a move into TRASH_DIR, hidden
// like the history, so restore_note can put it back; nothing here unlinks a note.

/// The first line of a note's body that is not its title, without markdown's marks: a list row's second line. A
/// capture's title is its first line (title_of), so that line is skipped once rather than shown twice.
fn preview(body: &str, title: &str) -> String {
    let mut skipped = false;
    for line in py::splitlines(body) {
        let text = py::strip(line);
        if text.is_empty() || text.starts_with("# ") || text == "#" {
            continue;
        }
        let text = PREVIEW_MARK.replace(text, "");
        let text = PREVIEW_INLINE.replace_all(&text, "");
        let text = py::strip(&text);
        if !text.is_empty()
            && !skipped
            && !title.is_empty()
            && (text == title || text.starts_with(title) || py::strip(line).starts_with(title))
        {
            skipped = true;
            continue;
        }
        if !text.is_empty() {
            return py::clip(text, MAX_PREVIEW_CHARS).to_string();
        }
    }
    String::new()
}

/// Every note (the archive too), newest first: path, folder, title, preview and modified (unix seconds).
pub fn list_notes(root: &Path) -> Result<Vec<NoteRow>, BrainError> {
    let mut out = Vec::new();
    for path in notes(root, true) {
        let (Ok(text), Ok(modified)) = (py::read_text(&path, true), py::mtime(&path)) else { continue };
        let rel_path = rel(root, &path)?;
        let (_, body) = split_frontmatter(&text);
        let title = title_of(&text, &py::stem(&path));
        let folder = rel_path.rsplit_once('/').map(|(f, _)| f.to_string()).unwrap_or_default();
        out.push(NoteRow { preview: preview(&body, &title), path: rel_path, folder, title, modified: modified.trunc() as i64 });
    }
    out.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.path.cmp(&b.path)));
    Ok(out)
}

/// Where a note can be started or moved: the PARA folders and the project and area folders that exist.
pub fn note_folders(root: &Path) -> Result<Vec<String>, BrainError> {
    let mut folders: Vec<String> = PARA_FOLDERS.iter().map(|s| s.to_string()).collect();
    for parent in [PROJECTS_DIR, AREAS_DIR] {
        let base = root.join(parent);
        if base.is_dir() {
            let mut entries = py::iterdir(&base)?;
            entries.sort();
            for d in entries {
                let name = py::file_name(&d);
                if d.is_dir() && !py::is_symlink(&d) && SAFE_SEGMENT.is_match(&name) {
                    folders.push(format!("{parent}/{name}"));
                }
            }
        }
    }
    Ok(folders)
}

fn markdown_note(root: &Path, rel_path: &str) -> Result<PathBuf, BrainError> {
    let path = resolve(root, rel_path)?;
    if !path.is_file() || py::suffix(&path) != ".md" {
        return value_err("no such markdown note");
    }
    Ok(path)
}

/// Replace a note's body as the owner typed it; its frontmatter stays as it is. Undo history as edit_note.
pub fn write_note(root: &Path, rel_path: &str, rev: &str, body: &str) -> Result<Value, BrainError> {
    let _guard = NOTE_LOCK.lock();
    let path = markdown_note(root, rel_path)?;
    let current = py::read_text(&path, false)?;
    if rev.is_empty() || revision(&current) != rev {
        return value_err("note changed; read_note again before editing");
    }
    let prefix = frontmatter_prefix(&current);
    let text = body.replace("\r\n", "\n").replace('\r', "\n");
    let updated = format!(
        "{prefix}{}",
        if py::strip(&text).is_empty() { String::new() } else { format!("{}\n", text.trim_end_matches('\n')) }
    );
    let rel_path = rel(root, &path)?;
    if updated == current {
        let undo = latest_edit(root, &rel_path, &current)?;
        return Ok(json!({"ok": true, "path": rel_path, "changed": false, "revision": rev, "undo_id": undo}));
    }
    let change = write_with_undo(root, &path, &rel_path, &current, &updated)?;
    tracing::info!("second brain: the owner saved {}", py::file_name(&path));
    Ok(json!({"ok": true, "path": rel_path, "changed": true, "revision": revision(&updated), "undo_id": change,
              "title": title_of(&updated, &py::stem(&path))}))
}

/// A new page in a vault folder: `# <title>` and the body, named from the title, never over another file. Python's
/// default `body` is "".
pub fn create_note(root: &Path, folder: &str, title: &str, body: &str) -> Result<Value, BrainError> {
    let collapsed = py::collapse_ws(title);
    let title = match py::clip(&collapsed, MAX_TITLE_CHARS) {
        "" => "Untitled",
        t => t,
    };
    let dest_dir = destination(root, folder)?;
    let text = format!(
        "# {title}\n{}",
        if py::strip(body).is_empty() { String::new() } else { format!("\n{}\n", body.trim_matches('\n')) }
    );
    let path = {
        let _guard = NOTE_LOCK.lock();
        py::mkdirs(&dest_dir)?;
        let path = unclashed(&dest_dir.join(format!("{}.md", slugify(title))))?;
        let mut fh = fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| py::os_error(&e, &path))?;
        fh.write_all(text.as_bytes()).map_err(|e| py::os_error(&e, &path))?;
        path
    };
    tracing::info!("second brain: the owner started {}", py::file_name(&path));
    Ok(json!({"ok": true, "path": rel(root, &path)?, "title": title, "revision": revision(&text)}))
}

fn trash_dir(root: &Path) -> Result<PathBuf, BrainError> {
    let base = py::resolve(root).join(TRASH_DIR);
    if py::is_symlink(&base) {
        return value_err("the trash must not be a symlink");
    }
    Ok(base)
}

/// Move a note into the hidden trash, only while it is the version the owner saw. Returns the restore id.
pub fn trash_note(root: &Path, rel_path: &str, rev: &str) -> Result<Value, BrainError> {
    let (rel_path, trash_id, name) = {
        let _guard = NOTE_LOCK.lock();
        let path = markdown_note(root, rel_path)?;
        let current = py::read_text(&path, false)?;
        if rev.is_empty() || revision(&current) != rev {
            return value_err("note changed; read_note again before editing");
        }
        let rel_path = rel(root, &path)?;
        let trash_id = change_id();
        let folder = trash_dir(root)?.join(&trash_id);
        py::mkdir_mode(&folder, 0o700, false)?;
        atomic_text(&folder.join("record.json"), &format!("{{\"path\": {}}}", py::json_str(&rel_path)))?;
        let name = py::file_name(&path);
        py::replace(&path, &folder.join(&name))?;
        (rel_path, trash_id, name)
    };
    tracing::info!("second brain: the owner deleted {} (in the trash)", name);
    Ok(json!({"ok": true, "path": rel_path, "trash_id": trash_id}))
}

/// Put a deleted note back where it was; a file that took its name since keeps it, and this one gets `-2`.
pub fn restore_note(root: &Path, trash_id: &str) -> Result<Value, BrainError> {
    if !CHANGE_ID.is_match(trash_id) {
        return value_err("use a trash_id returned by trash_note");
    }
    let dest = {
        let _guard = NOTE_LOCK.lock();
        let folder = trash_dir(root)?.join(trash_id);
        let record = folder.join("record.json");
        if py::is_symlink(&folder) || !record.is_file() || py::is_symlink(&record) {
            return value_err("nothing to restore");
        }
        let data = parse_json(&py::read_text(&record, false)?)?;
        let Some(Value::String(rel_path)) = data.as_object().and_then(|d| d.get("path")) else {
            return value_err("invalid trash record");
        };
        let target = resolve(root, rel_path)?;
        let kept = folder.join(py::file_name(&target));
        if !kept.is_file() || py::is_symlink(&kept) {
            return value_err("nothing to restore");
        }
        if let Some(parent) = target.parent() {
            py::mkdirs(parent)?;
        }
        let dest = unclashed(&target)?;
        py::replace(&kept, &dest)?;
        py::rename(&record, &py::with_suffix(&record, ".restored"))?;
        dest
    };
    tracing::info!("second brain: the owner restored {}", py::file_name(&dest));
    Ok(json!({"ok": true, "path": rel(root, &dest)?}))
}

/// Move a note into another vault folder as it is (file_note also marks it filed; the owner's move does not).
pub fn move_note(root: &Path, rel_path: &str, into: &str) -> Result<Value, BrainError> {
    let (dest, dest_dir, name) = {
        let _guard = NOTE_LOCK.lock();
        let path = markdown_note(root, rel_path)?;
        let dest_dir = destination(root, into)?;
        if path.parent() == Some(py::resolve(&dest_dir).as_path()) {
            return Ok(json!({"ok": true, "path": rel(root, &path)?, "changed": false}));
        }
        py::mkdirs(&dest_dir)?;
        let name = py::file_name(&path);
        let dest = unclashed(&dest_dir.join(&name))?;
        py::replace(&path, &dest)?;
        (dest, dest_dir, name)
    };
    tracing::info!("second brain: the owner moved {} into {}", name, rel(root, &dest_dir)?);
    Ok(json!({"ok": true, "path": rel(root, &dest)?, "changed": true}))
}

/// Everything in `01-inbox/`, oldest first.
pub fn inbox(root: &Path) -> Result<Vec<InboxItem>, BrainError> {
    let mut items = Vec::new();
    let folder = root.join(INBOX_DIR);
    if !folder.is_dir() {
        return Ok(items);
    }
    for path in py::glob(&folder, "*.md")? {
        let text = py::read_text(&path, true)?;
        let (meta, _) = split_frontmatter(&text);
        let created = match meta.get("created").filter(|c| !c.is_empty()) {
            Some(c) => c.clone(),
            None => local_minute(py::mtime(&path)?),
        };
        items.push(InboxItem { path: rel(root, &path)?, title: title_of(&text, &py::stem(&path)), created });
    }
    items.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.path.cmp(&b.path)));
    Ok(items)
}

/// `datetime.fromtimestamp(ts).strftime("%Y-%m-%dT%H:%M")`.
fn local_minute(ts: f64) -> String {
    let secs = ts.floor() as i64;
    let nanos = ((ts - ts.floor()) * 1e9) as u32;
    match Local.timestamp_opt(secs, nanos) {
        chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) => t.format("%Y-%m-%dT%H:%M").to_string(),
        chrono::LocalResult::None => String::new(),
    }
}

fn status_of(path: &Path) -> Result<String, BrainError> {
    let candidates = if path.is_file() {
        vec![path.to_path_buf()]
    } else {
        vec![path.join("README.md"), path.join("index.md"), path.join(format!("{}.md", py::file_name(path)))]
    };
    for c in candidates {
        if c.is_file() {
            let (meta, _) = split_frontmatter(&py::read_text(&c, true)?);
            return Ok(meta.get("status").map(|s| s.to_lowercase()).unwrap_or_default());
        }
    }
    Ok(String::new())
}

/// Folder and file names under `03-projects/` whose frontmatter is not `status: done|archived`.
pub fn active_projects(root: &Path) -> Result<Vec<String>, BrainError> {
    let folder = root.join(PROJECTS_DIR);
    if !folder.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries = py::iterdir(&folder)?;
    entries.sort();
    let mut names = Vec::new();
    for entry in entries {
        let name = py::file_name(&entry);
        if name.starts_with('.') {
            continue;
        }
        if entry.is_file() && py::suffix(&entry) != ".md" {
            continue;
        }
        if matches!(status_of(&entry)?.as_str(), "done" | "archived") {
            continue;
        }
        names.push(if entry.is_file() { py::stem(&entry) } else { name });
    }
    Ok(names)
}

/// Open items per priority from the master list.
pub fn todos(root: &Path) -> Result<IndexMap<String, Vec<String>>, BrainError> {
    let mut out: IndexMap<String, Vec<String>> = PRIORITIES.iter().map(|p| (p.to_string(), Vec::new())).collect();
    let path = root.join(TODOS_FILE);
    if !path.is_file() {
        return Ok(out);
    }
    let mut current = String::new();
    for line in py::splitlines(&py::read_text(&path, true)?) {
        if let Some(rest) = line.strip_prefix("## ") {
            current = py::strip(rest).to_uppercase();
            continue;
        }
        if let Some(m) = OPEN_TODO.captures(line) {
            if let Some(list) = out.get_mut(&current) {
                list.push(m[1].to_string());
            }
        }
    }
    Ok(out)
}

// ---- filing --------------------------------------------------------------------------------------

/// A PARA folder, or `03-projects/<x>` / `04-areas/<x>`; anything else is refused.
fn destination(root: &Path, into: &str) -> Result<PathBuf, BrainError> {
    let rel_path = py::strip(into).trim_matches('/').replace('\\', "/");
    let parts: Vec<&str> = if rel_path.is_empty() { Vec::new() } else { rel_path.split('/').collect() };
    let refuse = || value_err(format!("not a vault folder: {}", py::repr(into)));
    if parts.is_empty() || !PARA_FOLDERS.contains(&parts[0]) {
        return refuse();
    }
    let dest = if parts.len() == 1 {
        root.join(parts[0])
    } else if parts.len() == 2 && [PROJECTS_DIR, AREAS_DIR].contains(&parts[0]) && SAFE_SEGMENT.is_match(parts[1]) {
        root.join(parts[0]).join(parts[1])
    } else {
        return refuse();
    };
    // Resolved, so a symlinked folder cannot point outside: unchecked, a note was written or moved out of the
    // vault, and then _rel raised and the caller was told it failed after the file had gone
    let (root_r, dest_r) = (py::resolve(root), py::resolve(&dest));
    if dest_r == root_r || !dest_r.starts_with(&root_r) {
        return value_err("path leaves the vault");
    }
    Ok(dest_r)
}

fn move_with_status(root: &Path, src: &Path, dest_dir: &Path, name: &str, status: &str) -> Result<String, BrainError> {
    py::mkdirs(dest_dir)?;
    let dest = unclashed(&dest_dir.join(name))?;
    let text = py::read_text(src, true)?;
    py::write_text(&dest, &set_frontmatter(&text, "status", status))?;
    py::unlink(src)?;
    rel(root, &dest)
}

/// Move a note into a PARA folder, mark it `status: filed`, return its new relative path.
pub fn file_note(root: &Path, rel_path: &str, into: &str) -> Result<String, BrainError> {
    let src = resolve(root, rel_path)?;
    if !src.is_file() || py::suffix(&src) != ".md" {
        return value_err("no such note");
    }
    let dest_dir = destination(root, into)?;
    let name = py::file_name(&src);
    let new_rel = move_with_status(root, &src, &dest_dir, &name, "filed")?;
    tracing::info!("second brain: filed {} into {}", name, rel(root, &dest_dir)?);
    Ok(new_rel)
}

// ---- context packs -------------------------------------------------------------------------------

/// A value of the pack's YAML subset, with Python's truthiness, `str()`, `int()` and iteration.
#[derive(Debug, Clone, PartialEq)]
enum Yaml {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    List(Vec<Yaml>),
    Map(IndexMap<String, Yaml>),
}

impl Yaml {
    fn truthy(&self) -> bool {
        match self {
            Yaml::Null => false,
            Yaml::Bool(b) => *b,
            Yaml::Int(n) => *n != 0,
            Yaml::Str(s) => !s.is_empty(),
            Yaml::List(l) => !l.is_empty(),
            Yaml::Map(m) => !m.is_empty(),
        }
    }

    fn type_name(&self) -> &'static str {
        match self {
            Yaml::Null => "NoneType",
            Yaml::Bool(_) => "bool",
            Yaml::Int(_) => "int",
            Yaml::Str(_) => "str",
            Yaml::List(_) => "list",
            Yaml::Map(_) => "dict",
        }
    }

    fn py_repr(&self) -> String {
        match self {
            Yaml::Str(s) => py::repr(s),
            other => other.py_str(),
        }
    }

    /// `str(value)`.
    fn py_str(&self) -> String {
        match self {
            Yaml::Null => "None".to_string(),
            Yaml::Bool(true) => "True".to_string(),
            Yaml::Bool(false) => "False".to_string(),
            Yaml::Int(n) => n.to_string(),
            Yaml::Str(s) => s.clone(),
            Yaml::List(l) => format!("[{}]", l.iter().map(Yaml::py_repr).collect::<Vec<_>>().join(", ")),
            Yaml::Map(m) => format!(
                "{{{}}}",
                m.iter().map(|(k, v)| format!("{}: {}", py::repr(k), v.py_repr())).collect::<Vec<_>>().join(", ")
            ),
        }
    }

    /// `int(value)`.
    fn py_int(&self) -> Result<i64, BrainError> {
        match self {
            Yaml::Int(n) => Ok(*n),
            Yaml::Bool(b) => Ok(i64::from(*b)),
            Yaml::Str(s) => parse_py_int(s),
            other => Err(type_err(format!(
                "int() argument must be a string, a bytes-like object or a real number, not '{}'",
                other.type_name()
            ))),
        }
    }

    /// `for item in value`.
    fn py_iter(&self) -> Result<Vec<Yaml>, BrainError> {
        match self {
            Yaml::List(l) => Ok(l.clone()),
            Yaml::Str(s) => Ok(s.chars().map(|c| Yaml::Str(c.to_string())).collect()),
            Yaml::Map(m) => Ok(m.keys().map(|k| Yaml::Str(k.clone())).collect()),
            other => Err(type_err(format!("'{}' object is not iterable", other.type_name()))),
        }
    }

    fn get(&self, key: &str) -> Option<&Yaml> {
        match self {
            Yaml::Map(m) => m.get(key),
            _ => None,
        }
    }

    /// `d.get(key) or default`.
    fn get_or<'a>(&'a self, key: &str, default: &'a Yaml) -> &'a Yaml {
        self.get(key).filter(|v| v.truthy()).unwrap_or(default)
    }
}

/// `int(s)` for a string: surrounding whitespace, a sign, ASCII digits with single underscores between them.
fn parse_py_int(s: &str) -> Result<i64, BrainError> {
    let invalid = || BrainError::Value(format!("invalid literal for int() with base 10: {}", py::repr(s)));
    let t = py::strip(s);
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
        || !digits.chars().all(|c| c.is_ascii_digit() || c == '_')
    {
        return Err(invalid());
    }
    let clean: String = digits.chars().filter(|c| *c != '_').collect();
    let n: i64 = clean.parse().map_err(|_| invalid())?;
    Ok(if neg { -n } else { n })
}

fn scalar(raw: &str) -> Yaml {
    let v = py::strip(raw);
    let chars: Vec<char> = v.chars().collect();
    if chars.len() >= 2 && chars[0] == chars[chars.len() - 1] && (chars[0] == '"' || chars[0] == '\'') {
        return Yaml::Str(chars[1..chars.len() - 1].iter().collect());
    }
    if v == "[]" {
        return Yaml::List(Vec::new());
    }
    if matches!(v, "null" | "~" | "") {
        return Yaml::Null;
    }
    let low = v.to_lowercase();
    if low == "true" || low == "false" {
        return Yaml::Bool(low == "true");
    }
    if INT.is_match(v) {
        if let Ok(n) = v.parse::<i64>() {
            return Yaml::Int(n);
        }
    }
    Yaml::Str(v.to_string())
}

fn is_item(content: &str) -> bool {
    content.starts_with("- ") || content == "-"
}

/// Parse the YAML-subset block starting at `lines[i]` with the given indent: a mapping or a list.
fn parse_block(lines: &[(usize, String)], mut i: usize, indent: usize) -> (Yaml, usize) {
    if i >= lines.len() {
        return (Yaml::Null, i);
    }
    if is_item(&lines[i].1) {
        let mut items = Vec::new();
        while i < lines.len() && lines[i].0 == indent && is_item(&lines[i].1) {
            let content = &lines[i].1;
            let body = py::strip(py::skip(content, 1));
            if !body.is_empty() && body.contains(':') && !body.starts_with(['"', '\'']) {
                // a list of mappings: "- path: x" then "  select: y" at indent + 2
                let (key, value) = body.split_once(':').unwrap_or((body, ""));
                let mut mapping = IndexMap::new();
                mapping.insert(py::strip(key).to_string(), scalar(value));
                i += 1;
                while i < lines.len() && lines[i].0 > indent && !lines[i].1.starts_with("- ") {
                    let (k, v) = lines[i].1.split_once(':').unwrap_or((lines[i].1.as_str(), ""));
                    mapping.insert(py::strip(k).to_string(), scalar(v));
                    i += 1;
                }
                items.push(Yaml::Map(mapping));
            } else {
                items.push(scalar(body));
                i += 1;
            }
        }
        return (Yaml::List(items), i);
    }
    let mut mapping = IndexMap::new();
    while i < lines.len() && lines[i].0 == indent {
        let content = &lines[i].1;
        if content.starts_with("- ") {
            break;
        }
        let (key, value) = content.split_once(':').unwrap_or((content.as_str(), ""));
        let key = py::strip(key).to_string();
        if !py::strip(value).is_empty() {
            mapping.insert(key, scalar(value));
            i += 1;
        } else {
            i += 1;
            if i < lines.len() && lines[i].0 > indent {
                let (nested, next) = parse_block(lines, i, lines[i].0);
                mapping.insert(key, nested);
                i = next;
            } else {
                mapping.insert(key, Yaml::Null);
            }
        }
    }
    (Yaml::Map(mapping), i)
}

/// A `.pack` file → PackDef. A hand-written YAML subset: scalars, lists, lists of flat mappings, one nesting.
pub fn parse_pack(text: &str) -> Result<PackDef, BrainError> {
    let mut lines: Vec<(usize, String)> = Vec::new();
    for raw in py::splitlines(text) {
        let stripped =
            if py::lstrip(raw).starts_with('#') { "" } else { py::rstrip(raw.split(" #").next().unwrap_or("")) };
        if py::strip(stripped).is_empty() {
            continue;
        }
        let indent = py::char_len(stripped) - py::char_len(py::lstrip(stripped));
        lines.push((indent, py::strip(stripped).to_string()));
    }
    let first = lines.first().map(|l| l.0).unwrap_or(0);
    let (data, _) = parse_block(&lines, 0, first);
    if !matches!(data, Yaml::Map(_)) || !data.get("name").map(Yaml::truthy).unwrap_or(false) {
        return value_err("a pack needs at least a name");
    }
    let empty = Yaml::List(Vec::new());
    let mut includes = Vec::new();
    for item in data.get_or("include", &empty).py_iter()? {
        match &item {
            Yaml::Str(s) => includes.push(Include::new(s.clone())),
            Yaml::Map(_) if item.get("path").map(Yaml::truthy).unwrap_or(false) => {
                let default_select = Yaml::Str("filename_desc".to_string());
                let select = item.get_or("select", &default_select).py_str();
                if !SELECTS.contains(&select.as_str()) {
                    return value_err(format!("unknown select {}", py::repr(&select)));
                }
                let count = match item.get("count") {
                    None | Some(Yaml::Null) => None,
                    Some(c) => Some(c.py_int()?),
                };
                let path = item.get("path").map(Yaml::py_str).unwrap_or_default();
                includes.push(Include { path, select, count });
            }
            _ => {}
        }
    }
    let rules = match data.get("rules") {
        Some(r @ Yaml::Map(_)) => r.clone(),
        _ => Yaml::Map(IndexMap::new()),
    };
    let prefer = rules.get_or("prefer_files", &empty).py_iter()?;
    let default_max = Yaml::Int(DEFAULT_MAX_PER_GLOB);
    let max_per = match rules.get("max_per_glob").filter(|v| v.truthy()) {
        Some(v) => v.clone(),
        None => data.get_or("max_per_glob", &default_max).clone(),
    };
    let default_budget = Yaml::Int(4000);
    let name = data.get("name").map(Yaml::py_str).unwrap_or_default();
    let description = data.get("description").filter(|v| v.truthy()).map(Yaml::py_str).unwrap_or_default();
    let budget_tokens = data.get_or("budget_tokens", &default_budget).py_int()?;
    let exclude = data.get_or("exclude", &empty).py_iter()?.iter().map(Yaml::py_str).collect();
    Ok(PackDef {
        name,
        description,
        budget_tokens,
        include: includes,
        exclude,
        prefer_files: prefer.iter().map(Yaml::py_str).collect(),
        max_per_glob: max_per.py_int()?,
    })
}

pub fn load_pack(root: &Path, name: &str) -> Result<PackDef, BrainError> {
    if !WORKFLOWS.contains(&name) && !PACK_NAME.is_match(name) {
        return value_err(format!("not a pack name: {}", py::repr(name)));
    }
    let path = root.join(PACKS_DIR).join(format!("{name}.pack"));
    if !path.is_file() {
        return value_err(format!("no pack named {}", py::repr(name)));
    }
    parse_pack(&py::read_text(&path, false)?)
}

fn excluded(rel_path: &str, patterns: &[String]) -> bool {
    if rel_path.starts_with('.') || format!("/{rel_path}").contains(&format!("/{OBSIDIAN_DIR}/")) {
        return true;
    }
    patterns.iter().any(|pat| {
        py::fnmatch(rel_path, pat) || py::fnmatch(rel_path, &format!("{}/*", pat.trim_end_matches('/')))
    })
}

/// `seq[:limit]` with Python's negative-limit slicing.
fn slice_end(len: usize, limit: i64) -> usize {
    if limit >= 0 {
        (limit as usize).min(len)
    } else {
        len.saturating_sub(limit.unsigned_abs() as usize)
    }
}

fn matches(root: &Path, inc: &Include, exclude: &[String], max_per_glob: i64) -> Result<Vec<PathBuf>, BrainError> {
    let found: Vec<PathBuf> = if inc.path.contains(['*', '?', '[']) {
        py::glob(root, &inc.path)?.into_iter().filter(|p| p.is_file()).collect()
    } else {
        let p = root.join(py::pure_path(&inc.path));
        if p.is_file() {
            vec![p]
        } else {
            Vec::new()
        }
    };
    let mut keyed = Vec::new();
    for p in found {
        let rel_path = rel(root, &p)?;
        if !excluded(&rel_path, exclude) {
            keyed.push((p, rel_path));
        }
    }
    if inc.select == "modified_desc" {
        let mut timed = Vec::new();
        for (p, r) in keyed {
            let t = py::mtime(&p)?;
            timed.push((t, p, r));
        }
        timed.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.2.cmp(&b.2)));
        keyed = timed.into_iter().map(|(_, p, r)| (p, r)).collect();
    } else {
        keyed.sort_by(|a, b| b.1.cmp(&a.1));
    }
    let limit = inc.count.unwrap_or(max_per_glob).min(max_per_glob);
    keyed.truncate(slice_end(keyed.len(), limit));
    Ok(keyed.into_iter().map(|(p, _)| p).collect())
}

/// Globs → files → one `<context>` block within the budget. Preferred files first; the tail is dropped.
pub fn compile_pack(root: &Path, pack: &PackDef, now: Option<NaiveDateTime>) -> Result<Compiled, BrainError> {
    let mut ordered: Vec<String> = Vec::new();
    for inc in &pack.include {
        for p in matches(root, inc, &pack.exclude, pack.max_per_glob)? {
            let rel_path = rel(root, &p)?;
            if !ordered.contains(&rel_path) {
                ordered.push(rel_path);
            }
        }
    }
    let preferred: Vec<String> = pack.prefer_files.iter().filter(|p| ordered.contains(p)).cloned().collect();
    let mut rest: Vec<String> = ordered.into_iter().filter(|p| !preferred.contains(p)).collect();
    let mut ordered = preferred;
    ordered.append(&mut rest);
    let mut contents: HashMap<String, String> = HashMap::new();
    for r in &ordered {
        contents.insert(r.clone(), py::read_text(&root.join(r), true)?);
    }
    let when = now.unwrap_or_else(now_local);
    let head = format!("<context pack=\"{}\" compiled=\"{}\">\n", pack.name, when.format("%Y-%m-%d"));
    let tail = "</context>";
    let render = |files: &[String]| -> String {
        let mut out = head.clone();
        for r in files {
            out.push_str(&format!("<file path=\"{r}\">\n{}\n</file>\n", py::rstrip(&contents[r])));
        }
        out.push_str(tail);
        out
    };
    let mut kept = ordered;
    let mut dropped: Vec<String> = Vec::new();
    let mut xml = render(&kept);
    while !kept.is_empty() && (py::char_len(&xml) / CHARS_PER_TOKEN) as i64 > pack.budget_tokens {
        if let Some(last) = kept.pop() {
            dropped.insert(0, last);
        }
        xml = render(&kept);
    }
    let tokens = py::char_len(&xml) / CHARS_PER_TOKEN;
    tracing::info!(
        "second brain: pack {} compiled: {} file(s), {} dropped, ~{} tokens",
        pack.name,
        kept.len(),
        dropped.len(),
        tokens
    );
    Ok(Compiled { xml, files: kept, tokens, dropped })
}

// ---- workflows -----------------------------------------------------------------------------------

/// The SOP + the compiled pack + what the owner supplied, as one prompt. Pure: the caller picks the model.
pub fn workflow_prompt(root: &Path, name: &str, extra: &str, now: Option<NaiveDateTime>) -> Result<String, BrainError> {
    if !WORKFLOWS.contains(&name) {
        return value_err(format!("unknown workflow {}", py::repr(name)));
    }
    ensure_vault(root)?;
    let when = now.unwrap_or_else(now_local);
    let sop_text = py::read_text(&root.join(PROCESSES_DIR).join(format!("{name}.md")), false)?;
    let compiled = compile_pack(root, &load_pack(root, name)?, Some(when))?;
    let projects = active_projects(root)?;
    let mut parts = vec![
        py::rstrip(&sop_text).to_string(),
        format!("Today is {}.", when.format("%A %Y-%m-%d")),
        format!("Active projects: {}.", if projects.is_empty() { "none listed".to_string() } else { projects.join(", ") }),
        compiled.xml,
    ];
    if name == "daily-plan" {
        // Here, not in the SOP: a vault written before 2026-09-24 keeps its old SOP file.
        parts.insert(2, DAILY_PLAN_CALENDAR.to_string());
    }
    let extra = py::strip(extra);
    if !extra.is_empty() {
        let tag = if name == "distill-chat" { "transcript" } else { "request" };
        parts.push(format!("<{tag}>\n{}\n</{tag}>", py::clip(extra, MAX_PROMPT_CHARS)));
    } else if name == "distill-chat" {
        parts.push("<transcript>\n(no transcript was supplied: ask the owner to paste it)\n</transcript>".to_string());
    }
    Ok(parts.join("\n\n"))
}

// ---- the text brain's tools ----------------------------------------------------------------------

/// The nine tool schemas the text brain gets when the second brain is on, verbatim from Python.
pub static SECOND_BRAIN_TOOLS: LazyLock<Vec<Value>> =
    LazyLock::new(|| serde_json::from_str(texts::TOOLS_JSON).expect("the tool schemas are valid JSON"));
pub const SECOND_BRAIN_TOOL_NAMES: [&str; 9] = [
    "capture_note",
    "search_notes",
    "read_note",
    "edit_note",
    "undo_note",
    "list_inbox",
    "file_note",
    "list_todos",
    "second_brain_workflow",
];

fn clip_text(text: &str, limit: usize) -> String {
    if py::char_len(text) <= limit {
        text.to_string()
    } else {
        format!("{} …", py::rstrip(py::clip(text, limit)))
    }
}

/// `str(value)` for a JSON value, as Python's `str()` renders what json.loads gave it.
fn value_str(v: &Value) -> String {
    fn repr(v: &Value) -> String {
        match v {
            Value::String(s) => py::repr(s),
            other => value_str(other),
        }
    }
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => i.to_string(),
            (_, Some(u), _) => u.to_string(),
            (_, _, Some(f)) if f.fract() == 0.0 && f.is_finite() => format!("{f:.1}"),
            (_, _, Some(f)) => f.to_string(),
            _ => n.to_string(),
        },
        Value::String(s) => s.clone(),
        Value::Array(a) => format!("[{}]", a.iter().map(repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter().map(|(k, v)| format!("{}: {}", py::repr(k), repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `str(args.get(key) or "")`.
fn arg(args: &Map<String, Value>, key: &str) -> String {
    args.get(key).filter(|v| truthy(v)).map(value_str).unwrap_or_default()
}

/// One tool call → one JSON-serialisable value. Never fails: a failure is `{"ok": false, "reason": …}`.
pub fn dispatch(root: &Path, name: &str, args: &Value) -> Value {
    // Telegram can serve multiple owner turns on worker threads. Serialize vault tool operations.
    let _guard = NOTE_LOCK.lock();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dispatch_inner(root, name, args))) {
        Ok(Ok(value)) => value,
        Ok(Err(e @ (BrainError::Value(_) | BrainError::Os { .. }))) => {
            tracing::info!("second brain: {} refused: {}", name, e.type_name());
            json!({"ok": false, "reason": e.to_string()})
        }
        Ok(Err(e)) => {
            tracing::error!("second brain: {} failed: {}", name, e);
            json!({"ok": false, "reason": format!("{name} failed: {}", e.type_name())})
        }
        Err(_) => {
            tracing::error!("second brain: {} failed", name);
            json!({"ok": false, "reason": format!("{name} failed: panic")})
        }
    }
}

fn dispatch_inner(root: &Path, name: &str, args: &Value) -> Result<Value, BrainError> {
    let empty = Map::new();
    let args = args.as_object().unwrap_or(&empty);
    match name {
        "capture_note" => {
            let raw = py::strip(&arg(args, "text")).to_string();
            if raw.is_empty() {
                return Ok(json!({"ok": false, "reason": "nothing to capture"}));
            }
            let (classified, text) = classify_capture(&raw);
            let mut kind = classified.to_string();
            match args.get("kind") {
                None | Some(Value::Null) => {}
                Some(wanted) => match wanted.as_str().filter(|k| KINDS.contains(k)) {
                    Some(k) => kind = k.to_string(),
                    None => {
                        return Ok(json!({"ok": false, "reason": format!("kind must be one of {}", KINDS.join(", "))}))
                    }
                },
            }
            let res = capture(root, &text, &kind, "telegram", None)?;
            let line = if kind == "note" || kind == "idea" {
                format!("Saved to {}", res.path)
            } else {
                let place = match kind.as_str() {
                    "todo" => "the todo list",
                    "journal" => "today's journal",
                    _ => "the inbox",
                };
                format!("Added to {place} ({})", res.path)
            };
            Ok(json!({"ok": true, "kind": res.kind, "path": res.path, "title": res.title, "line": line}))
        }
        "search_notes" => {
            let hits = search(root, &arg(args, "query"), MAX_HITS, false)?;
            let count = hits.len();
            Ok(json!({"ok": true, "hits": hits, "count": count}))
        }
        "read_note" => {
            let mut res = read_note(root, &arg(args, "path"), MAX_NOTE_CHARS)?;
            if res["ok"] == json!(true) {
                let text = res["text"].as_str().unwrap_or("").to_string();
                if py::char_len(&text) > MAX_TOOL_TEXT_CHARS {
                    res["text"] = json!(clip_text(&text, MAX_TOOL_TEXT_CHARS));
                    res["clipped"] = json!(true);
                }
            }
            Ok(res)
        }
        "list_inbox" => {
            let items = inbox(root)?;
            let count = items.len();
            Ok(json!({"ok": true, "items": items, "count": count}))
        }
        "edit_note" => edit_note(root, &arg(args, "path"), &arg(args, "revision"), &arg(args, "old_text"), &arg(args, "new_text")),
        "undo_note" => undo_note(root, &arg(args, "path"), &arg(args, "undo_id"), &arg(args, "revision")),
        "file_note" => {
            let new = file_note(root, &arg(args, "path"), &arg(args, "into"))?;
            Ok(json!({"ok": true, "path": new}))
        }
        "list_todos" => Ok(json!({"ok": true, "todos": todos(root)?})),
        "second_brain_workflow" => {
            let wf = arg(args, "name");
            let prompt = workflow_prompt(root, &wf, &arg(args, "extra"), None)?;
            let tokens = py::char_len(&prompt) / CHARS_PER_TOKEN;
            let note = WORKFLOW_NOTES.get(wf.as_str()).copied().unwrap_or("hand this prompt to think_hard");
            Ok(json!({"ok": true, "name": wf, "prompt": prompt, "tokens": tokens, "note": note}))
        }
        _ => Ok(json!({"ok": false, "reason": format!("unknown tool {name}")})),
    }
}

// ---- config --------------------------------------------------------------------------------------

/// `EVE_SECOND_BRAIN=1` turns the layer on; `EVE_VAULT` moves the vault root. `None` reads the process
/// environment, as Python's `environ=None`.
pub fn configured(environ: Option<&HashMap<String, String>>) -> VaultConfig {
    let get = |key: &str| -> Option<String> {
        match environ {
            Some(env) => env.get(key).cloned(),
            None => std::env::var_os(key).map(|v| v.to_string_lossy().into_owned()),
        }
    };
    let switch_raw = get("EVE_SECOND_BRAIN").filter(|v| !v.is_empty());
    let switch = py::strip(&switch_raw.unwrap_or_else(|| if SECOND_BRAIN_DEFAULT { "1" } else { "0" }.to_string()))
        .to_lowercase();
    let raw_root = get("EVE_VAULT").unwrap_or_default();
    let raw_root = py::strip(&raw_root);
    let root = if raw_root.is_empty() { DEFAULT_VAULT.clone() } else { py::pure_path(&py::expanduser(raw_root)) };
    VaultConfig { enabled: matches!(switch.as_str(), "1" | "true" | "yes" | "on"), root }
}

// ---- test seams ----------------------------------------------------------------------------------
// Python's tests monkeypatch `_atomic_text`, `os.replace` and `_append_under`. Here each is a thread-local hook,
// compiled only into the unit tests, so one test's fault cannot leak into another running in parallel.

#[cfg(test)]
pub(crate) mod seams {
    use super::BrainError;
    use std::cell::RefCell;
    use std::io;
    use std::path::Path;

    type AtomicHook = Box<dyn Fn(&Path, &str) -> Result<(), BrainError>>;
    type ReplaceHook = Box<dyn Fn(&Path, &Path) -> Option<io::Error>>;
    type AppendHook = Box<dyn Fn()>;

    thread_local! {
        static ATOMIC: RefCell<Option<AtomicHook>> = const { RefCell::new(None) };
        static REPLACE: RefCell<Option<ReplaceHook>> = const { RefCell::new(None) };
        static APPEND: RefCell<Option<AppendHook>> = const { RefCell::new(None) };
    }

    pub fn set_atomic_text(hook: impl Fn(&Path, &str) -> Result<(), BrainError> + 'static) {
        ATOMIC.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
    }

    pub fn set_replace(hook: impl Fn(&Path, &Path) -> Option<io::Error> + 'static) {
        REPLACE.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
    }

    pub fn set_before_append_under(hook: impl Fn() + 'static) {
        APPEND.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
    }

    pub fn clear() {
        ATOMIC.with(|h| *h.borrow_mut() = None);
        REPLACE.with(|h| *h.borrow_mut() = None);
        APPEND.with(|h| *h.borrow_mut() = None);
    }

    /// The real writer, for a hook that wraps it.
    pub fn real_atomic_text(path: &Path, text: &str) -> Result<(), BrainError> {
        super::atomic_text_real(path, text)
    }

    pub(crate) fn atomic_text(path: &Path, text: &str) -> Option<Result<(), BrainError>> {
        let hook = ATOMIC.with(|h| h.borrow_mut().take())?;
        let result = hook(path, text);
        ATOMIC.with(|h| {
            let mut slot = h.borrow_mut();
            if slot.is_none() {
                *slot = Some(hook);
            }
        });
        Some(result)
    }

    pub(crate) fn replace(src: &Path, dst: &Path) -> Option<io::Error> {
        REPLACE.with(|h| h.borrow().as_ref().and_then(|hook| hook(src, dst)))
    }

    pub(crate) fn before_append_under() {
        let hook = APPEND.with(|h| h.borrow_mut().take());
        if let Some(hook) = hook {
            hook();
            APPEND.with(|h| {
                let mut slot = h.borrow_mut();
                if slot.is_none() {
                    *slot = Some(hook);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests;
