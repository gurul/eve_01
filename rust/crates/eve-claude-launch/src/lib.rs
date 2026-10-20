//! Start a Claude Code session on the Mac from Telegram, by walking a short decision tree.
//!
//! Port of `eve_bridge/claude_launch.py`.
//!
//! Every coding folder lives under `~/Documents`, split into `personal` and `work`. From the phone,
//! "new claude" asks two questions and then opens a terminal window on the Mac running `claude` in the
//! folder picked:
//!
//! 1. **Personal or work?** (`EVE_CODE_AREAS`, default `personal,work`, under `EVE_CODE_ROOT`, default
//!    `~/Documents`). A recent session's number, or a folder name, skips ahead: the owner usually has one in
//!    mind.
//! 2. **General, or which folder?** "general" opens the area itself; a name opens that folder; "list" shows
//!    what is there. A name is matched forgivingly (case, dashes and spaces ignored, then prefix, then
//!    substring, then a close spelling); more than one match becomes a numbered choice.
//!
//! Which program starts is never a question: personal opens `claude`, work opens `era-code claude`
//! ([`AREA_HARNESS`]), both with `--dangerously-skip-permissions`. Words that name the other one explicitly —
//! "in claude instead of era code" — switch it for that session ([`split_harness`]). The text brain reaches the
//! same tree through its `start_coding_session` tool, so "open era maker in work" works without the code word.
//!
//! The words are code, not a model call, as "codex on" is. [`LaunchFlow`] is pure — it reads folder names and
//! never starts anything — so the tests drive it with a temporary tree. [`open_session`] is the only part that
//! touches the Mac: a Warp launch configuration (the terminal on this desk) or, with
//! `EVE_CLAUDE_TERMINAL=terminal`, a Terminal.app window. A folder path is only ever one that was listed from
//! the tree, never text from the chat, and it is shell-quoted either way.

mod difflib;

use std::fmt::Write as _;
use std::fs;
use std::future::Future;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock};

use eve_envfile::{is_py_space, py_strip, Environ, ProcessEnv};
use indexmap::IndexMap;
use regex::Regex;
use serde_json::{json, Value};

pub const DEFAULT_AREAS: [&str; 2] = ["personal", "work"];
/// A numbered choice longer than this is a list, not a question.
pub const MAX_CHOICES: usize = 10;
pub const MAX_RECENT: usize = 5;
/// An unanswered question this old is dropped; the next text is eve's.
pub const FLOW_TIMEOUT_SECS: f64 = 300.0;
pub const WARP_CONFIG_NAME: &str = "eve-claude";

pub const GENERAL_WORDS: [&str; 7] = ["general", "whole", "root", "all", "g", "the whole thing", "top"];
pub const LIST_WORDS: [&str; 9] = ["list", "ls", "?", "show", "folders", "list them", "show me", "which", "options"];

/// Python's `\s` (`str.isspace`) as a regex class: Unicode White_Space plus U+001C..U+001F.
const PY_S: &str = r"[\t\n\x0B\x0C\r\x1C-\x1F \x{85}\x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}]";
/// Python's `\S`.
const PY_NS: &str = r"[^\t\n\x0B\x0C\r\x1C-\x1F \x{85}\x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}]";

fn py_re(pattern: &str) -> Regex {
    Regex::new(&pattern.replace(r"\s", PY_S).replace(r"\S", PY_NS)).expect("claude-launch regex")
}

/// `"/new_claude"` is the / menu's form (`telegram.BOT_COMMANDS`): a Bot API command has underscores, not
/// spaces. Group 1 is the rest of the words. Python's `$` also matches before a final newline, hence `\n?\z`.
pub static TRIGGER: LazyLock<Regex> = LazyLock::new(|| {
    py_re(
        r"(?is)^/?(?:new[ _]?claude(?:[ _]session)?|claude[ _]?new|start[ _]claude|claude[ _]session)(?:\s+(.*))?\n?\z",
    )
});
/// era-maker-213: a worktree of era-maker, folded out of the list.
static NUMBERED: LazyLock<Regex> = LazyLock::new(|| py_re(r"^(.+)-\d+\n?\z"));
/// "2. eve (personal)": a tapped button carries its number.
static TAPPED: LazyLock<Regex> = LazyLock::new(|| py_re(r"^(\d+)\.\s"));

/// `TRIGGER.match(text)`: `None` when the words are not the code word, else the words after it (`""` when
/// there are none).
pub fn trigger_argument(text: &str) -> Option<String> {
    TRIGGER
        .captures(text)
        .map(|c| c.get(1).map(|m| m.as_str().to_string()).unwrap_or_default())
}

/// `Path.home()`: `HOME`, else the password database.
fn home() -> io::Result<PathBuf> {
    eve_envfile::expanduser(Path::new("~")).map_err(|e| io::Error::other(e.to_string()))
}

/// `RECENT_PATH`: `~/.config/eve-bridge/claude-launch.json`, fixed when first read as Python fixes it at
/// import.
pub static RECENT_PATH: LazyLock<PathBuf> = LazyLock::new(|| {
    home().unwrap_or_else(|_| PathBuf::from("~")).join(".config").join("eve-bridge").join("claude-launch.json")
});

fn env_str<E: Environ + ?Sized>(env: &E, key: &str) -> String {
    env.var(key).map(|v| v.to_string_lossy().into_owned()).unwrap_or_default()
}

/// `code_root(environ)`: `EVE_CODE_ROOT` (with `~` expanded), else `~/Documents`.
pub fn code_root<E: Environ + ?Sized>(env: &E) -> PathBuf {
    let raw = env_str(env, "EVE_CODE_ROOT");
    let raw = py_strip(&raw);
    if !raw.is_empty() {
        return eve_envfile::expanduser(Path::new(raw)).unwrap_or_else(|_| py_path(raw));
    }
    home().unwrap_or_else(|_| PathBuf::from("~")).join("Documents")
}

/// `code_root()` over the process environment.
pub fn code_root_from_process() -> PathBuf {
    code_root(&ProcessEnv)
}

/// `area_names(environ)`: `EVE_CODE_AREAS` split on commas, blanks dropped; [`DEFAULT_AREAS`] when none.
pub fn area_names<E: Environ + ?Sized>(env: &E) -> Vec<String> {
    let raw = env_str(env, "EVE_CODE_AREAS");
    let names: Vec<String> = raw.split(',').map(py_strip).filter(|p| !p.is_empty()).map(String::from).collect();
    if names.is_empty() {
        DEFAULT_AREAS.iter().map(|a| a.to_string()).collect()
    } else {
        names
    }
}

/// The folders in an area, by name, sorted case-blind. Hidden folders and files are not projects. An area
/// that cannot be read has none.
pub fn folders(area: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(area) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            return Vec::new();
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_dir() && !name.starts_with('.') {
            names.push(name);
        }
    }
    names.sort_by_key(|n| n.to_lowercase());
    names
}

/// `_norm`: lower case, with whitespace, `_`, `-` and `.` removed.
fn norm(text: &str) -> String {
    text.to_lowercase().chars().filter(|c| !(is_py_space(*c) || matches!(c, '_' | '-' | '.'))).collect()
}

/// Python's `match(query, names)` (`match` is a Rust keyword): the folders a typed name means, best first.
/// One exact (normalised) hit wins outright; then every prefix hit, then every substring hit (shortest
/// first, so era-maker comes before era-maker-213), then up to five close spellings.
pub fn match_names<S: AsRef<str>>(query: &str, names: &[S]) -> Vec<String> {
    let q = norm(query);
    if q.is_empty() {
        return Vec::new();
    }
    let names: Vec<&str> = names.iter().map(|n| n.as_ref()).collect();
    if let Some(exact) = names.iter().find(|n| norm(n) == q) {
        return vec![exact.to_string()];
    }
    let by_len = |n: &&str| (n.chars().count(), n.to_lowercase());
    let mut prefix: Vec<&str> = names.iter().copied().filter(|n| norm(n).starts_with(&q)).collect();
    if !prefix.is_empty() {
        prefix.sort_by_key(by_len);
        return prefix.into_iter().map(String::from).collect();
    }
    let mut inside: Vec<&str> = names.iter().copied().filter(|n| norm(n).contains(&q)).collect();
    if !inside.is_empty() {
        inside.sort_by_key(by_len);
        return inside.into_iter().map(String::from).collect();
    }
    // A dict: a later name with the same normal form replaces the earlier one, in the earlier one's place.
    let mut normed: IndexMap<String, &str> = IndexMap::new();
    for n in &names {
        normed.insert(norm(n), n);
    }
    let keys: Vec<&String> = normed.keys().collect();
    difflib::get_close_matches(&q, &keys, 5, 0.7).into_iter().map(|c| normed[&c].to_string()).collect()
}

/// The list as the phone shows it: `(name, hidden numbered copies)`. `era-maker-213` and its twenty siblings
/// fold into `era-maker` when `era-maker` itself is there; they stay reachable by their full name.
pub fn menu<S: AsRef<str>>(names: &[S]) -> Vec<(String, usize)> {
    let present: std::collections::HashSet<&str> = names.iter().map(|n| n.as_ref()).collect();
    let mut folded: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut shown: Vec<String> = Vec::new();
    for name in names {
        let name = name.as_ref();
        match NUMBERED.captures(name).map(|c| c[1].to_string()) {
            Some(base) if present.contains(base.as_str()) => *folded.entry(base).or_insert(0) += 1,
            _ => shown.push(name.to_string()),
        }
    }
    shown.into_iter().map(|n| {
        let k = folded.get(&n).copied().unwrap_or(0);
        (n, k)
    }).collect()
}

// ---- which harness --------------------------------------------------------------------------------

/// The two ways the owner starts a session, and which area uses which (owner, 2026-09-23): personal is plain
/// Claude Code, work is Era's. Never asked; only an explicit "in claude instead" changes it. Both skip Claude
/// Code's permission prompts: the phone cannot answer them.
pub const HARNESSES: [(&str, &str); 2] = [
    ("claude", "claude --dangerously-skip-permissions"),
    ("era-code", "era-code claude --dangerously-skip-permissions"),
];
pub const AREA_HARNESS: [(&str, &str); 2] = [("personal", "claude"), ("work", "era-code")];

/// `HARNESSES[name]`: the command line a harness runs.
pub fn harness_command(name: &str) -> Option<&'static str> {
    HARNESSES.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
}

fn is_harness(name: &str) -> bool {
    harness_command(name).is_some()
}

const H: &str = r"(era[- ]?code|claude(?: code)?)";
// Said explicitly, anywhere in the words: "instead of era code" means the other one; "in claude", "with
// era-code", "claude instead" name it. The phrase is removed before the folder is matched.
static INSTEAD_OF: LazyLock<Regex> = LazyLock::new(|| {
    py_re(&format!(r"(?i)[,\s]*\b(?:but\s+)?(?:(?:i\s+)?want\s+it\s+)?(?:in\s+\S+\s+)?instead\s+of\s+{H}\b"))
});
static NAMED: LazyLock<Regex> = LazyLock::new(|| {
    py_re(&format!(
        r"(?i)[,\s]*\b(?:but\s+)?(?:(?:i\s+)?want\s+it\s+)?(?:in|with|using|use|via|on|as)\s+{H}(?:\s+instead)?\b"
    ))
});
static BARE_INSTEAD: LazyLock<Regex> = LazyLock::new(|| py_re(&format!(r"(?i)[,\s]*\b{H}\s+instead\b")));

/// The harness a word names: anything starting "era" is era-code, the rest is claude.
pub fn harness_name(word: &str) -> &'static str {
    if word.to_lowercase().starts_with("era") {
        "era-code"
    } else {
        "claude"
    }
}

fn cut(text: &str, start: usize, end: usize) -> String {
    let joined = format!("{}{}", &text[..start], &text[end..]);
    joined.trim_matches([' ', ',', '.']).to_string()
}

/// (the words without the harness phrase, the harness the owner asked for or `None`).
pub fn split_harness(text: &str) -> (String, Option<&'static str>) {
    if let Some(c) = INSTEAD_OF.captures(text) {
        let m = c.get(0).expect("whole match");
        let other = if harness_name(&c[1]) == "era-code" { "claude" } else { "era-code" };
        return (cut(text, m.start(), m.end()), Some(other));
    }
    for pattern in [&*NAMED, &*BARE_INSTEAD] {
        if let Some(c) = pattern.captures(text) {
            let m = c.get(0).expect("whole match");
            return (cut(text, m.start(), m.end()), Some(harness_name(&c[1])));
        }
    }
    (text.to_string(), None)
}

/// `folder.relative_to(root).parts`, or `None` when `folder` is not under `root`.
fn relative_parts(folder: &Path, root: &Path) -> Option<Vec<String>> {
    let rel = folder.strip_prefix(root).ok()?;
    Some(
        rel.components()
            .filter(|c| !matches!(c, Component::CurDir))
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect(),
    )
}

/// The harness the folder's area uses: the first part under `root` names the area; anything else is claude.
pub fn default_harness(folder: &Path, root: &Path) -> &'static str {
    let Some(parts) = relative_parts(folder, root) else {
        return "claude";
    };
    let Some(area) = parts.first() else {
        return "claude";
    };
    let area = area.to_lowercase();
    AREA_HARNESS.iter().find(|(a, _)| *a == area).map(|(_, h)| *h).unwrap_or("claude")
}

// ---- recent sessions ----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Recent {
    pub folder: PathBuf,
    pub harness: String,
}

impl Recent {
    pub fn new(folder: impl Into<PathBuf>, harness: impl Into<String>) -> Self {
        Recent { folder: folder.into(), harness: harness.into() }
    }
}

/// `Path(text)`: pathlib's normal form — repeated slashes and `.` parts dropped, a trailing slash dropped,
/// exactly two leading slashes kept, and `.` for nothing.
fn py_path(text: &str) -> PathBuf {
    let root = if text.starts_with("//") && !text.starts_with("///") {
        "//"
    } else if text.starts_with('/') {
        "/"
    } else {
        ""
    };
    let parts: Vec<&str> = text.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    let joined = format!("{root}{}", parts.join("/"));
    PathBuf::from(if joined.is_empty() { ".".to_string() } else { joined })
}

/// `str(path)` as Python writes it.
fn py_str(path: &Path) -> String {
    py_path(&path.to_string_lossy()).to_string_lossy().into_owned()
}

/// `json.dumps(value)` with its default `ensure_ascii=True`: every character outside space..`~` written as
/// `\uXXXX` (lower-case hex, surrogate pairs above the BMP). serde_json already escapes the control characters
/// as Python does.
fn ensure_ascii(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if (c as u32) < 0x7f {
            out.push(c);
        } else {
            let mut buf = [0u16; 2];
            for unit in c.encode_utf16(&mut buf) {
                let _ = write!(out, "\\u{unit:04x}");
            }
        }
    }
    out
}

/// `json.dumps(text)` for one string.
fn json_str(text: &str) -> String {
    ensure_ascii(&serde_json::to_string(text).expect("a string serializes"))
}

/// The recent sessions saved at `path`, newest first, at most [`MAX_RECENT`]. A missing or unreadable file,
/// and rows that are not `{"folder": str, "harness": a known harness}`, count as none.
pub fn load_recent(path: &Path) -> Vec<Recent> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(data) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let rows: Vec<Value> = match data.as_object().and_then(|d| d.get("recent")) {
        Some(Value::Array(rows)) => rows.clone(),
        _ => Vec::new(),
    };
    rows.iter()
        .filter_map(|r| {
            let r = r.as_object()?;
            let folder = r.get("folder")?.as_str()?;
            let harness = r.get("harness")?.as_str()?;
            is_harness(harness).then(|| Recent::new(py_path(folder), harness))
        })
        .take(MAX_RECENT)
        .collect()
}

/// `load_recent()` at [`RECENT_PATH`].
pub fn load_recent_default() -> Vec<Recent> {
    load_recent(&RECENT_PATH)
}

/// Put `folder` first in the recent list at `path` (dropping its older row), keeping [`MAX_RECENT`]. A file
/// that cannot be written is logged, not raised.
pub fn remember(folder: &Path, harness: &str, path: &Path) {
    let folder = py_path(&folder.to_string_lossy());
    let mut rows = vec![Recent::new(folder.clone(), harness)];
    rows.extend(load_recent(path).into_iter().filter(|r| r.folder != folder));
    rows.truncate(MAX_RECENT);
    let data = json!({"recent": rows.iter().map(|r| json!({"folder": py_str(&r.folder), "harness": r.harness}))
        .collect::<Vec<_>>()});
    let text = ensure_ascii(&serde_json::to_string_pretty(&data).expect("json serializes")) + "\n";
    let written = path
        .parent()
        .map_or(Ok(()), |p| if p.as_os_str().is_empty() { Ok(()) } else { fs::create_dir_all(p) })
        .and_then(|_| fs::write(path, text));
    if let Err(e) = written {
        tracing::warn!("claude-launch: could not save recent folders ({})", py_error_name(&e));
    }
}

/// The name of the `OSError` subclass Python raises for this error.
pub fn py_error_name(e: &io::Error) -> &'static str {
    match e.raw_os_error() {
        Some(libc::ENOENT) => "FileNotFoundError",
        Some(libc::EACCES) | Some(libc::EPERM) => "PermissionError",
        Some(libc::EEXIST) => "FileExistsError",
        Some(libc::ENOTDIR) => "NotADirectoryError",
        Some(libc::EISDIR) => "IsADirectoryError",
        Some(libc::EINTR) => "InterruptedError",
        Some(libc::EAGAIN) | Some(libc::EALREADY) | Some(libc::EINPROGRESS) => "BlockingIOError",
        Some(libc::ECHILD) => "ChildProcessError",
        Some(libc::EPIPE) | Some(libc::ESHUTDOWN) => "BrokenPipeError",
        Some(libc::ECONNABORTED) => "ConnectionAbortedError",
        Some(libc::ECONNREFUSED) => "ConnectionRefusedError",
        Some(libc::ECONNRESET) => "ConnectionResetError",
        Some(libc::ESRCH) => "ProcessLookupError",
        Some(libc::ETIMEDOUT) => "TimeoutError",
        _ => "OSError",
    }
}

// ---- the decision tree --------------------------------------------------------------------------

/// What the flow says next. `folder` and `harness` are set when it has decided: start there.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub text: String,
    pub folder: Option<PathBuf>,
    pub harness: Option<String>,
    pub done: bool,
    pub title: String,
    /// Tap targets on the phone; each sends its own text.
    pub buttons: Vec<String>,
}

impl Step {
    pub fn new(text: impl Into<String>) -> Self {
        Step {
            text: text.into(),
            folder: None,
            harness: None,
            done: false,
            title: "New Claude session".to_string(),
            buttons: Vec::new(),
        }
    }

    fn done(mut self) -> Self {
        self.done = true;
        self
    }

    fn buttons(mut self, buttons: Vec<String>) -> Self {
        self.buttons = buttons;
        self
    }
}

/// What a bare number means right now: a recent session or a folder.
#[derive(Debug, Clone, PartialEq)]
pub enum Choice {
    Recent(Recent),
    Folder(PathBuf),
}

impl Choice {
    pub fn folder(&self) -> &Path {
        match self {
            Choice::Recent(r) => &r.folder,
            Choice::Folder(p) => p,
        }
    }
}

/// A clock in seconds, `time.monotonic` by default.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

/// `time.monotonic()`: seconds on the system's monotonic clock (on macOS the one `mach_absolute_time` reads,
/// as Python's), so a flow that never asked is long expired, as in Python.
pub fn monotonic() -> f64 {
    #[cfg(target_vendor = "apple")]
    let clock = libc::CLOCK_UPTIME_RAW;
    #[cfg(not(target_vendor = "apple"))]
    let clock = libc::CLOCK_MONOTONIC;
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a valid out-parameter for the call.
    unsafe { libc::clock_gettime(clock, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9
}

/// The characters whose title case (what `str.capitalize` puts first) is not their upper case, generated from
/// Python 3's `str.capitalize`.
#[rustfmt::skip]
const TITLE_CASE: [(char, &str); 135] = [
    ('\u{DF}', "\u{53}\u{73}"), ('\u{1C4}', "\u{1C5}"), ('\u{1C5}', "\u{1C5}"), ('\u{1C6}', "\u{1C5}"),
    ('\u{1C7}', "\u{1C8}"), ('\u{1C8}', "\u{1C8}"), ('\u{1C9}', "\u{1C8}"), ('\u{1CA}', "\u{1CB}"),
    ('\u{1CB}', "\u{1CB}"), ('\u{1CC}', "\u{1CB}"), ('\u{1F1}', "\u{1F2}"), ('\u{1F2}', "\u{1F2}"),
    ('\u{1F3}', "\u{1F2}"), ('\u{587}', "\u{535}\u{582}"), ('\u{10D0}', "\u{10D0}"), ('\u{10D1}', "\u{10D1}"),
    ('\u{10D2}', "\u{10D2}"), ('\u{10D3}', "\u{10D3}"), ('\u{10D4}', "\u{10D4}"), ('\u{10D5}', "\u{10D5}"),
    ('\u{10D6}', "\u{10D6}"), ('\u{10D7}', "\u{10D7}"), ('\u{10D8}', "\u{10D8}"), ('\u{10D9}', "\u{10D9}"),
    ('\u{10DA}', "\u{10DA}"), ('\u{10DB}', "\u{10DB}"), ('\u{10DC}', "\u{10DC}"), ('\u{10DD}', "\u{10DD}"),
    ('\u{10DE}', "\u{10DE}"), ('\u{10DF}', "\u{10DF}"), ('\u{10E0}', "\u{10E0}"), ('\u{10E1}', "\u{10E1}"),
    ('\u{10E2}', "\u{10E2}"), ('\u{10E3}', "\u{10E3}"), ('\u{10E4}', "\u{10E4}"), ('\u{10E5}', "\u{10E5}"),
    ('\u{10E6}', "\u{10E6}"), ('\u{10E7}', "\u{10E7}"), ('\u{10E8}', "\u{10E8}"), ('\u{10E9}', "\u{10E9}"),
    ('\u{10EA}', "\u{10EA}"), ('\u{10EB}', "\u{10EB}"), ('\u{10EC}', "\u{10EC}"), ('\u{10ED}', "\u{10ED}"),
    ('\u{10EE}', "\u{10EE}"), ('\u{10EF}', "\u{10EF}"), ('\u{10F0}', "\u{10F0}"), ('\u{10F1}', "\u{10F1}"),
    ('\u{10F2}', "\u{10F2}"), ('\u{10F3}', "\u{10F3}"), ('\u{10F4}', "\u{10F4}"), ('\u{10F5}', "\u{10F5}"),
    ('\u{10F6}', "\u{10F6}"), ('\u{10F7}', "\u{10F7}"), ('\u{10F8}', "\u{10F8}"), ('\u{10F9}', "\u{10F9}"),
    ('\u{10FA}', "\u{10FA}"), ('\u{10FD}', "\u{10FD}"), ('\u{10FE}', "\u{10FE}"), ('\u{10FF}', "\u{10FF}"),
    ('\u{1F80}', "\u{1F88}"), ('\u{1F81}', "\u{1F89}"), ('\u{1F82}', "\u{1F8A}"), ('\u{1F83}', "\u{1F8B}"),
    ('\u{1F84}', "\u{1F8C}"), ('\u{1F85}', "\u{1F8D}"), ('\u{1F86}', "\u{1F8E}"), ('\u{1F87}', "\u{1F8F}"),
    ('\u{1F88}', "\u{1F88}"), ('\u{1F89}', "\u{1F89}"), ('\u{1F8A}', "\u{1F8A}"), ('\u{1F8B}', "\u{1F8B}"),
    ('\u{1F8C}', "\u{1F8C}"), ('\u{1F8D}', "\u{1F8D}"), ('\u{1F8E}', "\u{1F8E}"), ('\u{1F8F}', "\u{1F8F}"),
    ('\u{1F90}', "\u{1F98}"), ('\u{1F91}', "\u{1F99}"), ('\u{1F92}', "\u{1F9A}"), ('\u{1F93}', "\u{1F9B}"),
    ('\u{1F94}', "\u{1F9C}"), ('\u{1F95}', "\u{1F9D}"), ('\u{1F96}', "\u{1F9E}"), ('\u{1F97}', "\u{1F9F}"),
    ('\u{1F98}', "\u{1F98}"), ('\u{1F99}', "\u{1F99}"), ('\u{1F9A}', "\u{1F9A}"), ('\u{1F9B}', "\u{1F9B}"),
    ('\u{1F9C}', "\u{1F9C}"), ('\u{1F9D}', "\u{1F9D}"), ('\u{1F9E}', "\u{1F9E}"), ('\u{1F9F}', "\u{1F9F}"),
    ('\u{1FA0}', "\u{1FA8}"), ('\u{1FA1}', "\u{1FA9}"), ('\u{1FA2}', "\u{1FAA}"), ('\u{1FA3}', "\u{1FAB}"),
    ('\u{1FA4}', "\u{1FAC}"), ('\u{1FA5}', "\u{1FAD}"), ('\u{1FA6}', "\u{1FAE}"), ('\u{1FA7}', "\u{1FAF}"),
    ('\u{1FA8}', "\u{1FA8}"), ('\u{1FA9}', "\u{1FA9}"), ('\u{1FAA}', "\u{1FAA}"), ('\u{1FAB}', "\u{1FAB}"),
    ('\u{1FAC}', "\u{1FAC}"), ('\u{1FAD}', "\u{1FAD}"), ('\u{1FAE}', "\u{1FAE}"), ('\u{1FAF}', "\u{1FAF}"),
    ('\u{1FB2}', "\u{1FBA}\u{345}"), ('\u{1FB3}', "\u{1FBC}"), ('\u{1FB4}', "\u{386}\u{345}"), ('\u{1FB7}', "\u{391}\u{342}\u{345}"),
    ('\u{1FBC}', "\u{1FBC}"), ('\u{1FC2}', "\u{1FCA}\u{345}"), ('\u{1FC3}', "\u{1FCC}"), ('\u{1FC4}', "\u{389}\u{345}"),
    ('\u{1FC7}', "\u{397}\u{342}\u{345}"), ('\u{1FCC}', "\u{1FCC}"), ('\u{1FF2}', "\u{1FFA}\u{345}"), ('\u{1FF3}', "\u{1FFC}"),
    ('\u{1FF4}', "\u{38F}\u{345}"), ('\u{1FF7}', "\u{3A9}\u{342}\u{345}"), ('\u{1FFC}', "\u{1FFC}"), ('\u{FB00}', "\u{46}\u{66}"),
    ('\u{FB01}', "\u{46}\u{69}"), ('\u{FB02}', "\u{46}\u{6C}"), ('\u{FB03}', "\u{46}\u{66}\u{69}"), ('\u{FB04}', "\u{46}\u{66}\u{6C}"),
    ('\u{FB05}', "\u{53}\u{74}"), ('\u{FB06}', "\u{53}\u{74}"), ('\u{FB13}', "\u{544}\u{576}"), ('\u{FB14}', "\u{544}\u{565}"),
    ('\u{FB15}', "\u{544}\u{56B}"), ('\u{FB16}', "\u{54E}\u{576}"), ('\u{FB17}', "\u{544}\u{56D}"),
];

/// Python's `str.capitalize`: the first character in title case, the rest in lower case.
fn capitalize(text: &str) -> String {
    let Some(first) = text.chars().next() else {
        return String::new();
    };
    let head: String = match TITLE_CASE.binary_search_by_key(&first, |(c, _)| *c) {
        Ok(i) => TITLE_CASE[i].1.to_string(),
        Err(_) => first.to_uppercase().collect(),
    };
    // The rest is lowered in the context of the whole text, as Python lowers it (a final sigma after the first
    // letter is still final); only the final sigma depends on context, so the first letter's own lower case is
    // a fixed number of characters to skip.
    let skip = first.to_lowercase().count();
    head + &text.to_lowercase().chars().skip(skip).collect::<String>()
}

/// Python's `str.isdigit()` then `int()`, for the ASCII digits a phone sends: `Some(n)` for a number (a number
/// too large to hold is `u64::MAX`, out of every range), `None` for words.
fn number(word: &str) -> Option<u64> {
    if word.is_empty() || !word.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(word.parse().unwrap_or(u64::MAX))
}

/// The two-question tree. Pure: it reads folder names and never starts anything.
#[derive(Clone)]
pub struct LaunchFlow {
    pub root: PathBuf,
    pub areas: Vec<String>,
    pub recent: Vec<Recent>,
    pub clock: Clock,
    pub area: Option<String>,
    /// Said explicitly; otherwise the area decides.
    pub harness: Option<String>,
    /// What a bare number means right now.
    pub choices: Vec<Choice>,
    pub asked_at: f64,
}

impl std::fmt::Debug for LaunchFlow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchFlow")
            .field("root", &self.root)
            .field("areas", &self.areas)
            .field("recent", &self.recent)
            .field("area", &self.area)
            .field("harness", &self.harness)
            .field("choices", &self.choices)
            .field("asked_at", &self.asked_at)
            .finish_non_exhaustive()
    }
}

impl LaunchFlow {
    /// `LaunchFlow(root)`: the default areas, no recent sessions, `time.monotonic`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        LaunchFlow {
            root: root.into(),
            areas: DEFAULT_AREAS.iter().map(|a| a.to_string()).collect(),
            recent: Vec::new(),
            clock: Arc::new(monotonic),
            area: None,
            harness: None,
            choices: Vec::new(),
            asked_at: 0.0,
        }
    }

    pub fn with_areas<S: AsRef<str>>(mut self, areas: &[S]) -> Self {
        self.areas = areas.iter().map(|a| a.as_ref().to_string()).collect();
        self
    }

    pub fn with_recent(mut self, recent: Vec<Recent>) -> Self {
        self.recent = recent;
        self
    }

    pub fn with_clock(mut self, clock: impl Fn() -> f64 + Send + Sync + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    pub fn expired(&self) -> bool {
        (self.clock)() - self.asked_at > FLOW_TIMEOUT_SECS
    }

    fn existing_areas(&self) -> Vec<String> {
        self.areas.iter().filter(|a| self.root.join(a).is_dir()).cloned().collect()
    }

    fn label(&self, folder: &Path) -> String {
        match relative_parts(folder, &self.root) {
            Some(parts) if parts.len() > 1 => format!("{} ({})", parts[parts.len() - 1], parts[0]),
            Some(parts) if parts.len() == 1 => format!("{} (general)", parts[0]),
            _ => folder.to_string_lossy().into_owned(),
        }
    }

    /// The first question, or straight on when "new claude <words>" already says some of it.
    pub fn start(&mut self, argument: &str) -> Step {
        self.asked_at = (self.clock)();
        if py_strip(argument).is_empty() {
            self.ask_area()
        } else {
            self.answer(argument)
        }
    }

    /// The text brain's way in (`start_coding_session`): what it understood, each part optional. Whatever is
    /// missing becomes the next question, exactly as with the code word.
    pub fn request(&mut self, area: &str, folder: &str, harness: &str) -> Step {
        self.asked_at = (self.clock)();
        if is_harness(harness) {
            self.harness = Some(harness.to_string());
        }
        let wanted = py_strip(area).to_lowercase();
        let chosen = self.existing_areas().into_iter().find(|a| a.to_lowercase() == wanted);
        let folder = py_strip(folder);
        match chosen {
            None if folder.is_empty() => self.ask_area(),
            None => self.answer(folder),
            Some(chosen) if folder.is_empty() => self.ask_scope(&chosen),
            Some(chosen) => self.scope_answer(&chosen, folder),
        }
    }

    fn ask_area(&mut self) -> Step {
        let areas = self.existing_areas();
        if areas.is_empty() {
            return Step::new(format!("I can't find {} under {}.", self.areas.join(", "), self.root.display()))
                .done();
        }
        self.area = None;
        self.choices = Vec::new();
        let mut text = areas.iter().map(|a| capitalize(a)).collect::<Vec<_>>().join(" or ") + "?";
        let mut buttons: Vec<String> = areas.iter().map(|a| capitalize(a)).collect();
        let recent: Vec<Recent> = self.recent.iter().filter(|r| r.folder.is_dir()).cloned().collect();
        if !recent.is_empty() {
            let numbered: Vec<String> =
                recent.iter().enumerate().map(|(i, r)| format!("{}. {}", i + 1, self.label(&r.folder))).collect();
            self.choices = recent.into_iter().map(Choice::Recent).collect();
            text += &format!("\n\nOr a recent one:\n{}", numbered.join("\n"));
            buttons.extend(numbered);
        }
        text += "\n\nA folder name works too. cancel stops.";
        buttons.push("Cancel".into());
        Step::new(text).buttons(buttons)
    }

    fn ask_scope(&mut self, area: &str) -> Step {
        self.area = Some(area.to_string());
        self.choices = Vec::new();
        Step::new(format!("{}: general, or which folder? Say list to see them.", capitalize(area)))
            .buttons(vec!["List".into(), "General".into(), "Cancel".into()])
    }

    /// The owner's reply to the question asked last.
    pub fn answer(&mut self, text: &str) -> Step {
        self.asked_at = (self.clock)();
        let (mut said, harness) = split_harness(py_strip(text).trim_end_matches(['.', '!']));
        if let Some(h) = harness {
            self.harness = Some(h.to_string());
        }
        if !self.choices.is_empty() {
            if let Some(c) = TAPPED.captures(&said) {
                said = c[1].to_string();
            }
        }
        let word = said.to_lowercase();
        if !self.choices.is_empty() {
            if let Some(n) = number(&word) {
                let len = self.choices.len();
                if n < 1 || n > len as u64 {
                    return Step::new(format!("Pick a number from 1 to {len}."));
                }
                let folder = self.choices[(n - 1) as usize].folder().to_path_buf();
                return self.open(folder);
            }
        }
        if said.is_empty() {
            return match self.area.clone() {
                None => self.ask_area(),
                Some(area) => self.ask_scope(&area),
            };
        }
        let areas = self.existing_areas();
        let Some(current) = self.area.clone() else {
            // "work", "w", "work era-hub-api", "personal general", or just a folder name.
            let (head, rest) = said.split_once(' ').unwrap_or((said.as_str(), ""));
            let head_l = head.to_lowercase();
            let one_letter = head.chars().count() == 1;
            let area = areas
                .iter()
                .find(|a| a.to_lowercase() == head_l || (one_letter && a.to_lowercase().starts_with(&head_l)))
                .cloned();
            if let Some(area) = area {
                return if py_strip(rest).is_empty() {
                    self.ask_scope(&area)
                } else {
                    let rest = rest.to_string();
                    self.scope_answer(&area, &rest)
                };
            }
            if LIST_WORDS.contains(&word.as_str()) {
                return self.ask_area();
            }
            return self.pick(&said, &areas);
        };
        self.scope_answer(&current, &said)
    }

    fn scope_answer(&mut self, area: &str, said: &str) -> Step {
        self.area = Some(area.to_string());
        let word = py_strip(said).to_lowercase();
        let word = word.trim_end_matches(['.', '!']);
        if GENERAL_WORDS.contains(&word) {
            return self.open(self.root.join(area));
        }
        if LIST_WORDS.contains(&word) {
            return self.list(area);
        }
        self.pick(said, &[area.to_string()])
    }

    fn list(&mut self, area: &str) -> Step {
        let names = folders(&self.root.join(area));
        if names.is_empty() {
            return Step::new(format!("{} has no folders. Say general to open it.", capitalize(area)));
        }
        let shown = menu(&names);
        self.choices = shown.iter().map(|(n, _)| Choice::Folder(self.root.join(area).join(n))).collect();
        let lines: Vec<String> = shown
            .iter()
            .enumerate()
            .map(|(i, (n, k))| {
                let more = if *k > 0 { format!(" (+{k} numbered)") } else { String::new() };
                format!("{}. {n}{more}", i + 1)
            })
            .collect();
        let mut buttons: Vec<String> = shown.iter().enumerate().map(|(i, (n, _))| format!("{}. {n}", i + 1)).collect();
        buttons.extend(["General".to_string(), "Cancel".to_string()]);
        let mut step = Step::new(lines.join("\n") + "\n\nTap one, or reply a number or a name, or general.")
            .buttons(buttons);
        step.title = format!("{} folders", capitalize(area));
        step
    }

    fn pick(&mut self, said: &str, areas: &[String]) -> Step {
        let hits: Vec<PathBuf> = areas
            .iter()
            .flat_map(|a| {
                match_names(said, &folders(&self.root.join(a)))
                    .into_iter()
                    .map(|n| self.root.join(a).join(n))
                    .collect::<Vec<_>>()
            })
            .collect();
        if hits.len() == 1 {
            return self.open(hits[0].clone());
        }
        let place = if areas.len() == 1 { areas[0].clone() } else { areas.join(" or ") };
        if hits.is_empty() {
            let hint = if areas.len() == 1 { " Say list to see them." } else { "" };
            return Step::new(format!("No folder like \"{said}\" in {place}.{hint}"));
        }
        let more = if hits.len() > MAX_CHOICES {
            format!("\n…and {} more; type more of the name.", hits.len() - MAX_CHOICES)
        } else {
            String::new()
        };
        let choices: Vec<PathBuf> = hits.into_iter().take(MAX_CHOICES).collect();
        let numbered: Vec<String> =
            choices.iter().enumerate().map(|(i, p)| format!("{}. {}", i + 1, self.label(p))).collect();
        self.choices = choices.into_iter().map(Choice::Folder).collect();
        let mut buttons = numbered.clone();
        buttons.push("Cancel".into());
        Step::new(format!("Which one?\n{}{more}", numbered.join("\n"))).buttons(buttons)
    }

    fn open(&mut self, folder: PathBuf) -> Step {
        self.choices = Vec::new();
        let harness = match &self.harness {
            Some(h) => h.clone(),
            None => default_harness(&folder, &self.root).to_string(),
        };
        let command = harness_command(&harness).unwrap_or_default();
        let mut step = Step::new(format!("Opening {} with {command}.", self.label(&folder))).done();
        step.folder = Some(folder);
        step.harness = Some(harness);
        step
    }
}

// ---- opening the terminal -----------------------------------------------------------------------

/// `EVE_CLAUDE_TERMINAL` (`warp` or `terminal`); otherwise Warp when it is installed.
pub fn terminal_app<E: Environ + ?Sized>(env: &E) -> String {
    let raw = py_strip(&env_str(env, "EVE_CLAUDE_TERMINAL")).to_lowercase();
    if raw == "warp" || raw == "terminal" {
        return raw;
    }
    if Path::new("/Applications/Warp.app").exists() { "warp" } else { "terminal" }.to_string()
}

/// A Warp launch configuration: one window, one tab, `command` in `folder`. Warp finds a configuration by its
/// `name` (warp://launch/<name>; a file path is not matched, tried 2026-09-23) and rereads the file on each
/// launch, so one fixed name is rewritten every time. JSON strings are valid YAML scalars, so a folder name
/// with a colon or a quote cannot break the file.
pub fn warp_config(folder: &Path, command: &str) -> String {
    let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    format!(
        "---\nname: {}\nwindows:\n  - tabs:\n      - title: {}\n        layout:\n          cwd: {}\n          commands:\n            - exec: {}\n",
        json_str(WARP_CONFIG_NAME),
        json_str(&name),
        json_str(&py_str(folder)),
        json_str(command),
    )
}

/// `shlex.quote`: one word for a POSIX shell.
pub fn shlex_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    let safe = |c: char| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c);
    if s.chars().all(safe) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// `_run`: run a program, returning its exit code (minus the signal that killed it) and the first 200
/// characters of its stderr, or of its stdout when stderr is empty. A program that cannot start is an error.
pub async fn run(argv: Vec<String>) -> io::Result<(i32, String)> {
    let (program, args) = argv.split_first().ok_or_else(|| io::Error::other("nothing to run"))?;
    let out = tokio::process::Command::new(program).args(args).output().await?;
    let code = match out.status.code() {
        Some(c) => c,
        None => {
            use std::os::unix::process::ExitStatusExt;
            out.status.signal().map(|s| -s).unwrap_or(0)
        }
    };
    let bytes = if out.stderr.is_empty() { out.stdout } else { out.stderr };
    let text = String::from_utf8_lossy(&bytes);
    Ok((code, py_strip(&text).chars().take(200).collect()))
}

/// The optional arguments of `open_session`.
#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
    /// Overrides the harness's command line (the live check uses an echo).
    pub command: Option<String>,
    /// `warp` or `terminal`; [`terminal_app`] decides when unset.
    pub app: Option<String>,
    /// Where Warp's launch configurations live; `~/.warp/launch_configurations` when unset.
    pub warp_dir: Option<PathBuf>,
    /// The recent-sessions file; [`RECENT_PATH`] when unset.
    pub recent_path: Option<PathBuf>,
}

/// Open a terminal window on the Mac running the harness in `folder`. Returns the line for the chat.
pub async fn open_session(folder: &Path, harness: &str, opts: OpenOptions) -> io::Result<String> {
    open_session_with(folder, harness, opts, run).await
}

/// [`open_session`] with the program runner given (the tests' seam).
pub async fn open_session_with<F, Fut>(folder: &Path, harness: &str, opts: OpenOptions, run: F) -> io::Result<String>
where
    F: Fn(Vec<String>) -> Fut,
    Fut: Future<Output = io::Result<(i32, String)>>,
{
    let folder = eve_envfile::realpath(folder)?;
    if !folder.is_dir() {
        return Ok(format!("{} is not a folder any more.", folder.display()));
    }
    let command = match opts.command.filter(|c| !c.is_empty()) {
        Some(c) => c,
        None => harness_command(harness)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("KeyError: '{harness}'")))?
            .to_string(),
    };
    let app = match opts.app.filter(|a| !a.is_empty()) {
        Some(a) => a,
        None => terminal_app(&ProcessEnv),
    };
    let (code, err) = if app == "warp" {
        let config_dir = match opts.warp_dir {
            Some(d) => d,
            None => home()?.join(".warp").join("launch_configurations"),
        };
        let written = fs::create_dir_all(&config_dir)
            .and_then(|_| fs::write(config_dir.join(format!("{WARP_CONFIG_NAME}.yaml")), warp_config(&folder, &command)));
        if let Err(e) = written {
            return Ok(format!("I couldn't write the Warp launch configuration ({}).", py_error_name(&e)));
        }
        run(vec!["open".into(), format!("warp://launch/{WARP_CONFIG_NAME}")]).await?
    } else {
        let line = format!("cd {} && {command}", shlex_quote(&folder.to_string_lossy()));
        let escaped = line.replace('\\', "\\\\").replace('"', "\\\"");
        run(vec![
            "osascript".into(),
            "-e".into(),
            format!("tell application \"Terminal\" to do script \"{escaped}\""),
            "-e".into(),
            "tell application \"Terminal\" to activate".into(),
        ])
        .await?
    };
    if code != 0 {
        tracing::warn!("claude-launch: {} did not open ({})", app, err);
        let name = if app == "warp" { "Warp" } else { "Terminal" };
        let reason = if err.is_empty() { "no reason given" } else { err.as_str() };
        return Ok(format!("I couldn't open {name}: {reason}"));
    }
    let recent_path = opts.recent_path.unwrap_or_else(|| RECENT_PATH.clone());
    remember(&folder, harness, &recent_path);
    let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(format!("{harness} is starting in {name} on the Mac. Say claude on to drive it from here."))
}

#[cfg(test)]
mod tests;
