//! Port of `notes_widget.py`. Desktop notes widget: the robot's idle-explorer observations on the Mac desktop.
//!
//! The idle explorer writes one line per observation to `~/.config/eve-bridge/notes/YYYY-MM-DD.md`:
//!
//! ```text
//! - HH:MM yaw=+20 pitch=40 — <sentence>
//! ```
//!
//! This crate shows the newest of those lines in a borderless, non-activating panel that sits just above the
//! desktop icons — a wallboard, not an app. It has no Dock icon and no menu-bar entry; right-click gives "Open notes
//! folder" and "Quit". It is dragged by its background and remembers where it was put in
//! `~/.config/eve-bridge/widget.json`.
//!
//! The module splits into two halves so the file logic is testable without a GUI:
//!
//! * pure helpers — note file discovery, parsing, ordering, rendering, position persistence. No AppKit; safe on any
//!   platform.
//! * [`run`] — the AppKit panel (`panel.rs`). Refuses to start anywhere but macOS.
//!
//! Refresh: a file-watcher thread watches the notes directory and a 30 s `NSTimer` polls as a backstop (a file
//! written by `open(..., "a")` from another process can arrive without a FSEvents callback in edge cases). Both paths
//! land on the main thread via `performSelectorOnMainThread`.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use chrono::{Datelike, Duration, Local, NaiveDate};
use eve_records::RecallConfig;
use eve_transcripts::py as tpy;
use regex::Regex;
use serde_json::Value;
use tracing::warn;

#[cfg(target_os = "macos")]
mod panel;

#[cfg(test)]
mod tests;

pub const NOTES_DIR_ENV: &str = "EVE_NOTES_DIR";
pub const MEMORY_DIR_ENV: &str = "EVE_MEMORY_DIR";

/// `CONFIG_DIR`: `~/.config/eve-bridge`.
pub fn config_dir() -> PathBuf {
    tpy::home().join(".config").join("eve-bridge")
}

/// `DEFAULT_NOTES_DIR`.
pub fn default_notes_dir() -> PathBuf {
    config_dir().join("notes")
}

/// `DEFAULT_MEMORY_DIR`.
pub fn default_memory_dir() -> PathBuf {
    config_dir().join("memory")
}

/// `POSITION_PATH`.
pub fn position_path() -> PathBuf {
    config_dir().join("widget.json")
}

pub const TITLE: &str = "eve";
pub const EMPTY_TEXT: &str = "Nothing yet. eve writes here after it has looked around, and after you have talked to it.\n\n\
Say \u{201c}hey eve\u{201d} to talk. Say \u{201c}remember that\u{201d} to keep something for good.";
// The two provenances, kept apart on purpose: what eve heard you say is quotable, what it thinks it saw is not. The
// widget labels them rather than mixing them into one list (see recall.py).
pub const SAID: &str = "said";
pub const SEEN: &str = "seen";
pub const KIND_LABELS: [(&str, &str); 2] = [(SAID, "Talking"), (SEEN, "Looking")];
pub const STARS_LABEL: &str = "Remembered for good";
pub const MAX_STARS: usize = 8;
/// A journal line: the day's page, not a moment.
pub const NO_TIME: &str = "--:--";
/// What forget leaves in a journal: never shown.
pub const FORGOTTEN: &str = "(forgotten)";
pub const MAX_NOTES: usize = 40;
pub const WIDGET_SIZE: (f64, f64) = (360.0, 420.0);
pub const SCREEN_MARGIN: f64 = 24.0;
pub const POLL_SECS: f64 = 30.0;
pub const FONT_SIZE: f64 = 13.0;

/// `- HH:MM <rest>`. The rest is shown verbatim (yaw/pitch prefix included) — the explorer decides what a note looks
/// like, the widget only lists them. (Python's `\s` also counts `\x1c`-`\x1f`; its `$` is absorbed by `\s*`.)
pub static LINE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^- (\d{1,2}:\d{2}) (.+?)[\s\x1c-\x1f]*\z").expect("LINE_RE"));
/// Python's `$` also matches before one trailing newline.
pub static FILE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d{4})-(\d{2})-(\d{2})\.md\n?\z").expect("FILE_RE"));

fn label_of(kind: &str) -> &'static str {
    KIND_LABELS.iter().find(|(k, _)| *k == kind).map(|(_, l)| *l).unwrap_or("")
}

/// One line the widget shows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Note {
    pub day: NaiveDate,
    pub time: String,
    pub text: String,
    /// [`SEEN`]: an observation. [`SAID`]: from a conversation.
    pub kind: String,
}

impl Note {
    /// `Note(day, time, text)`: an observation ([`SEEN`]).
    pub fn new(day: NaiveDate, time: impl Into<String>, text: impl Into<String>) -> Self {
        Self::with_kind(day, time, text, SEEN)
    }

    /// `Note(day, time, text, kind)`.
    pub fn with_kind(day: NaiveDate, time: impl Into<String>, text: impl Into<String>, kind: &str) -> Self {
        Self { day, time: time.into(), text: text.into(), kind: kind.to_string() }
    }
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// `Path(raw)`: the lexical normalisation pathlib does (`//` collapsed, `.` parts and a trailing `/` dropped).
fn py_path(raw: &str) -> PathBuf {
    let p = Path::new(raw);
    let mut out = PathBuf::new();
    for c in p.components() {
        if c != Component::CurDir {
            out.push(c.as_os_str());
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// The env var as `os.environ.get` sees it: unset is None, a non-UTF-8 value is read lossily.
fn env_var(name: &str) -> Option<String> {
    std::env::var_os(name).map(|v| v.to_string_lossy().into_owned())
}

/// `Path(raw).expanduser() if raw else default`.
fn dir_from(raw: Option<&str>, default: PathBuf) -> PathBuf {
    match raw {
        Some(r) if !r.is_empty() => tpy::expand_user(&py_path(r)),
        _ => default,
    }
}

/// Where the explorer writes. `EVE_NOTES_DIR` overrides for tests/smoke runs.
pub fn notes_dir() -> PathBuf {
    dir_from(env_var(NOTES_DIR_ENV).as_deref(), default_notes_dir())
}

/// [`notes_dir`] against an explicit environment (tests must not mutate the process environment).
pub fn notes_dir_in(environ: &HashMap<String, String>) -> PathBuf {
    dir_from(environ.get(NOTES_DIR_ENV).map(String::as_str), default_notes_dir())
}

/// eve's memory folder (recall.py). `EVE_MEMORY_DIR` overrides.
pub fn memory_dir() -> PathBuf {
    dir_from(env_var(MEMORY_DIR_ENV).as_deref(), default_memory_dir())
}

/// [`memory_dir`] against an explicit environment.
pub fn memory_dir_in(environ: &HashMap<String, String>) -> PathBuf {
    dir_from(environ.get(MEMORY_DIR_ENV).map(String::as_str), default_memory_dir())
}

fn recall_cfg(store: &Path) -> RecallConfig {
    RecallConfig::new(store, notes_dir())
}

/// The claims the owner promoted by saying "remember that" (records/starred.md). Newest last. A widget shows less,
/// it never breaks: any failure is an empty list.
pub fn stars(store: &Path, limit: usize) -> Vec<String> {
    let cfg = recall_cfg(store);
    std::panic::catch_unwind(|| eve_records::stars(&cfg, limit)).unwrap_or_default()
}

/// A dream journal (records/days/<day>.md) as the one or two lines worth showing.
///
/// The title says what it was about; a debt of eve's own is the line the owner most wants to see, because it is the
/// thing eve has not done yet.
pub fn parse_conversation(text: &str) -> Vec<String> {
    let mut title = String::new();
    let mut owes: Vec<String> = Vec::new();
    for line in tpy::splitlines(text) {
        let stripped = tpy::strip(line);
        if title.is_empty() && stripped.starts_with("# ") {
            title = tpy::strip(&stripped[2..]).to_string();
        } else if let Some(rest) = stripped.strip_prefix("- ") {
            if rest.to_lowercase().starts_with("eve owes") {
                owes.push(tpy::strip(rest).to_string());
            }
        }
    }
    let mut out = Vec::new();
    if !title.is_empty() && !title.contains(FORGOTTEN) {
        out.push(title);
    }
    out.extend(owes.into_iter().take(2).filter(|o| !o.contains(FORGOTTEN)));
    out
}

/// The dream journal: one file per day eve talked, written the night after (dream.py).
pub fn journal_dir(store: &Path) -> PathBuf {
    store.join("records").join("days")
}

fn iso(day: NaiveDate) -> String {
    format!("{:04}-{:02}-{:02}", day.year(), day.month(), day.day())
}

fn today_or(today: Option<NaiveDate>) -> NaiveDate {
    today.unwrap_or_else(|| Local::now().date_naive())
}

/// `(today, today - timedelta(days=1))`.
fn two_days(today: NaiveDate) -> Vec<NaiveDate> {
    let mut days = vec![today];
    if let Some(y) = today.checked_sub_signed(Duration::days(1)) {
        days.push(y);
    }
    days
}

/// `path.read_text(encoding="utf-8", errors="replace")`.
fn read_text(path: &Path) -> io::Result<String> {
    fs::read(path).map(|raw| tpy::read_text(&raw))
}

/// What was said, from the dream journals of today and yesterday, newest day first. A journal has no clock time:
/// it is one page for the whole day, written the night after.
pub fn collect_conversations(store: &Path, today: Option<NaiveDate>, limit: usize) -> Vec<Note> {
    let today = today_or(today);
    let mut out = Vec::new();
    for day in two_days(today) {
        let path = journal_dir(store).join(format!("{}.md", iso(day)));
        let Ok(text) = read_text(&path) else {
            continue;
        };
        for line in parse_conversation(&text) {
            out.push(Note::with_kind(day, NO_TIME, line, SAID));
            if out.len() >= limit {
                return out;
            }
        }
    }
    out
}

/// Today's and yesterday's note files that exist, newest day first.
pub fn note_files(directory: &Path, today: Option<NaiveDate>) -> Vec<PathBuf> {
    let today = today_or(today);
    two_days(today)
        .into_iter()
        .map(|day| directory.join(format!("{}.md", iso(day))))
        .filter(|p| p.is_file())
        .collect()
}

/// The day a note file is for, from its name, or None.
pub fn day_of(path: &Path) -> Option<NaiveDate> {
    let name = path.file_name()?.to_string_lossy();
    let m = FILE_RE.captures(&name)?;
    let y = tpy::int(&m[1])?;
    let mo = tpy::int(&m[2])?;
    let d = tpy::int(&m[3])?;
    // Python's date covers years 1..=9999 only.
    if !(1..=9999).contains(&y) {
        return None;
    }
    NaiveDate::from_ymd_opt(y as i32, u32::try_from(mo).ok()?, u32::try_from(d).ok()?)
}

/// Note lines in file order (oldest first). Non-matching lines are skipped.
pub fn parse_notes(text: &str, day: NaiveDate) -> Vec<Note> {
    tpy::splitlines(text)
        .into_iter()
        .filter_map(|line| LINE_RE.captures(line).map(|m| Note::new(day, &m[1], &m[2])))
        .collect()
}

/// Newest `limit` notes across today + yesterday, newest first.
pub fn collect_notes(directory: &Path, today: Option<NaiveDate>, limit: usize) -> Vec<Note> {
    let mut out: Vec<Note> = Vec::new();
    for path in note_files(directory, today) {
        let Some(day) = day_of(&path) else {
            continue;
        };
        let text = match read_text(&path) {
            Ok(t) => t,
            Err(e) => {
                warn!("notes-widget: cannot read {}: {}", path.display(), e);
                continue;
            }
        };
        out.extend(parse_notes(&text, day).into_iter().rev());
        if out.len() >= limit {
            break;
        }
    }
    out.truncate(limit);
    out
}

/// "Today · <day>", "Yesterday · <day>", or the ISO date.
pub fn day_label(day: NaiveDate, today: Option<NaiveDate>) -> String {
    let today = today_or(today);
    if day == today {
        return format!("Today · {}", iso(day));
    }
    if today.checked_sub_signed(Duration::days(1)) == Some(day) {
        return format!("Yesterday · {}", iso(day));
    }
    iso(day)
}

/// Group newest-first notes by day, and within a day by provenance.
///
/// A day that holds both kinds gets a sub-header for each, because what eve heard and what eve saw are different
/// kinds of claim and the widget must not let them read as one list.
pub fn render_sections(notes: &[Note], today: Option<NaiveDate>) -> Vec<(String, Vec<Note>)> {
    let today = today_or(today);
    let mut by_day: Vec<(String, Vec<Note>)> = Vec::new();
    for note in notes {
        let label = day_label(note.day, Some(today));
        if by_day.last().is_none_or(|(l, _)| *l != label) {
            by_day.push((label, Vec::new()));
        }
        if let Some((_, group)) = by_day.last_mut() {
            group.push(note.clone());
        }
    }
    let mut sections = Vec::new();
    for (label, group) in by_day {
        let kinds: Vec<&str> = [SAID, SEEN].into_iter().filter(|k| group.iter().any(|n| n.kind == *k)).collect();
        if kinds.len() < 2 {
            sections.push((label, group));
            continue;
        }
        for (i, kind) in kinds.iter().enumerate() {
            let head = if i == 0 { format!("{label} · {}", label_of(kind)) } else { label_of(kind).to_string() };
            sections.push((head, group.iter().filter(|n| n.kind == *kind).cloned().collect()));
        }
    }
    sections
}

/// Plain-text rendering — what `--once` prints and what the panel shows.
pub fn render_text(notes: &[Note], today: Option<NaiveDate>, starred: &[String]) -> String {
    if notes.is_empty() && starred.is_empty() {
        return EMPTY_TEXT.to_string();
    }
    let mut lines: Vec<String> = Vec::new();
    if !starred.is_empty() {
        lines.push(STARS_LABEL.to_string());
        lines.extend(starred.iter().rev().map(|claim| format!("\u{2605}  {claim}")));
    }
    for (label, group) in render_sections(notes, today) {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(label);
        lines.extend(group.iter().map(|n| format!("{}  {}", n.time, n.text)));
    }
    lines.join("\n")
}

/// Everything eve remembers of the last two days, plus the starred layer.
///
/// The widget is the interface for all of it (owner instruction 2026-09-11), so this is the one call that gathers
/// it. Either source being absent is normal.
pub fn collect_everything(
    notes_directory: &Path,
    store: &Path,
    today: Option<NaiveDate>,
    limit: usize,
) -> (Vec<Note>, Vec<String>) {
    let mut both = collect_conversations(store, today, limit);
    both.extend(collect_notes(notes_directory, today, limit));
    // sorted(..., key=(day, time), reverse=True): stable, equal keys keep their order.
    both.sort_by(|a, b| (b.day, &b.time).cmp(&(a.day, &a.time)));
    both.truncate(limit);
    (both, stars(store, MAX_STARS))
}

/// Python's `float(s)` for a string: surrounding whitespace, `_` between digits, any Unicode decimal digit,
/// `inf`/`infinity`/`nan` in any case.
fn py_float_str(s: &str) -> Option<f64> {
    let s = tpy::strip(s);
    let chars: Vec<char> = s.chars().collect();
    let mut clean = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            let digit = |j: Option<&char>| j.is_some_and(|c| tpy::int(&c.to_string()).is_some());
            if i == 0 || !digit(chars.get(i - 1)) || !digit(chars.get(i + 1)) {
                return None;
            }
            continue;
        }
        if !c.is_ascii() {
            let d = tpy::int(&c.to_string())?;
            clean.push(char::from(b'0' + d as u8));
            continue;
        }
        clean.push(c);
    }
    clean.parse::<f64>().ok()
}

/// `float(value)` for a decoded JSON value: None where Python raises `ValueError`/`TypeError`.
fn py_float(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => py_float_str(s),
        _ => None,
    }
}

/// Saved bottom-left origin in screen points, or None when absent/invalid.
pub fn load_position(path: &Path) -> Option<(f64, f64)> {
    let raw = fs::read(path).ok()?;
    let text = String::from_utf8(raw).ok()?;
    let data = tpy::loads(&text)?;
    let obj = data.as_object()?;
    Some((py_float(obj.get("x")?)?, py_float(obj.get("y")?)?))
}

/// `round(x, 1)`: correctly rounded to one decimal, ties to even on the exact binary value.
fn round1(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.1}").parse().unwrap_or(x)
}

/// `path.with_suffix(".json.tmp")`.
fn tmp_path(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let stem = match name.rfind('.') {
        Some(i) if i > 0 && i < name.len() - 1 => &name[..i],
        _ => name.as_str(),
    };
    path.with_file_name(format!("{stem}.json.tmp"))
}

/// Write `{"x": …, "y": …}` (rounded to 0.1 pt) through a temp file and an atomic rename.
pub fn save_position(x: f64, y: f64, path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let tmp = tmp_path(path);
    let body = format!("{{\"x\": {}, \"y\": {}}}", tpy::float_repr(round1(x)), tpy::float_repr(round1(y)));
    fs::write(&tmp, body)?;
    fs::rename(&tmp, path)
}

/// x, y, w, h — AppKit bottom-left origin.
pub type Rect = (f64, f64, f64, f64);

/// Top-right corner of `screen` (the visible frame), inset by `margin`.
pub fn default_position(screen: Rect, size: (f64, f64), margin: f64) -> (f64, f64) {
    let (sx, sy, sw, sh) = screen;
    let (w, h) = size;
    (sx + sw - w - margin, sy + sh - h - margin)
}

/// True when the widget's centre lands on some screen. A saved position from an unplugged external display must not
/// strand the widget off-screen.
pub fn position_on_screens(pos: Option<(f64, f64)>, screens: &[Rect], size: (f64, f64)) -> bool {
    let Some((x, y)) = pos else {
        return false;
    };
    let (cx, cy) = (x + size.0 / 2.0, y + size.1 / 2.0);
    screens.iter().any(|&(sx, sy, sw, sh)| sx <= cx && cx <= sx + sw && sy <= cy && cy <= sy + sh)
}

// ---------------------------------------------------------------------------
// The file watcher's filter (watchfiles.DefaultFilter)
// ---------------------------------------------------------------------------

const IGNORE_DIRS: [&str; 11] = [
    "__pycache__",
    ".git",
    ".hg",
    ".svn",
    ".tox",
    ".venv",
    ".idea",
    "node_modules",
    ".mypy_cache",
    ".pytest_cache",
    ".hypothesis",
];

static IGNORE_ENTITY: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [r"\.py[cod]\z", r"\.___jb_...___\z", r"\.sw.\z", r"~\z", r"^\.\#", r"^\.DS_Store\z", r"^flycheck_"]
        .into_iter()
        .map(|p| Regex::new(p).expect("watch filter"))
        .collect()
});

/// watchfiles' `DefaultFilter`: a change under a VCS/cache folder, or to an editor swap/backup file, is not a
/// change worth a refresh. The records folder is a git repo, so its `.git` churn is the case that matters.
pub(crate) fn change_matters(path: &Path) -> bool {
    let s = path.to_string_lossy();
    let parts: Vec<&str> = s.trim_start_matches('/').split('/').collect();
    if parts.iter().any(|p| IGNORE_DIRS.contains(p)) {
        return false;
    }
    let entity = parts.last().copied().unwrap_or("");
    !IGNORE_ENTITY.iter().any(|r| r.is_match(entity))
}

// ---------------------------------------------------------------------------
// AppKit panel
// ---------------------------------------------------------------------------

/// Show the widget. Blocks in `NSApp.run()` unless `once`. `→` the process exit code.
///
/// `once` builds the panel, spins the run loop for one second so the window is actually created and drawn, prints
/// what it rendered, and exits — a smoke test for non-interactive runs. `position_path` None is [`position_path`].
pub fn run(
    once: bool,
    directory: Option<PathBuf>,
    position_path: Option<PathBuf>,
    store: Option<PathBuf>,
) -> anyhow::Result<i32> {
    #[cfg(target_os = "macos")]
    {
        let position_path = position_path.unwrap_or_else(self::position_path);
        panel::run(once, directory, position_path, store)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (once, directory, position_path, store);
        eprintln!("eve-bridge: notes-widget is macOS-only (AppKit)");
        Ok(2)
    }
}
