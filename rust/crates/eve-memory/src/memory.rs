//! Memory, one door: the facade both brains talk to, the one tool set, forget, and the move to one root.
//!
//! The owner asked for memory to be simpler (owner, 2026-09-23: "too many stores", "reconcile and minimize").
//! There are now three stores under one folder, and this module is the only thing a brain holds:
//!
//! ```text
//!     transcripts/   every word, written the moment it is said (transcripts.rs): the source
//!     records/       what is true about the owner, rewritten nightly by the dream (records.rs): the truth
//!     mem0/          a search by meaning, rebuilt from the transcripts (mem0_memory): the index
//!     archive/       the retired debrief store, moved here once, read-only, still searched
//! ```
//!
//! ### One tool set, the same on both channels
//!
//! Before this, the Telegram brain could search records and the voice brain could search nothing, so what
//! eve "knew" depended on how the owner reached it. Now both get `memory_search` (three dated groups:
//! record lines, recalled meanings, and the words themselves), `memory_read` (a record, or a stretch of a
//! day's transcript), and the two-step forget. Voice also gets `recent_conversation`: what was typed on the
//! other channel since this voice session opened.
//!
//! ### Forget is two calls and one confirmation
//!
//! "Forget that" used to remove one record file and leave the same words in nine other places. Now
//! `forget_preview` counts every place a match lives — transcripts, records (and their journal and stars),
//! the index, the archive, meeting notes — and hands back a single-use token that lives ten minutes and is
//! bound to that query. It never returns the matched words: a preview is a count, so the owner decides on
//! numbers, not on eve reading their secret back to them. `forget_apply` runs only with the token, after
//! the owner confirms in a new message. The git histories that held the words are squashed to one commit, so
//! the words are gone from history too. claude-mem and the Obsidian vault are not eve's stores, and the chief of
//! staff's ledger is not reached yet; the preview names them as not covered rather than pretending.
//!
//! ### The move, once
//!
//! `migrate` renames the old debrief store into the new root the first time memory starts with the new
//! layout. Renames only, on one volume: nothing is copied and nothing is deleted. A target that already
//! exists is skipped and counted, never overwritten. `.migrated` records the counts, and a second run
//! does nothing.
//!
//! Logs carry counts and milliseconds, never a word that was said, and never a file name (a day file's name
//! is a summary of the day).

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Weak};
use std::time::Instant;

use base64::Engine as _;
use chrono::{NaiveDate, NaiveDateTime};
use indexmap::IndexMap;
use parking_lot::{Mutex, RwLock};
use rand::RngCore;
use regex::Regex;
use serde_json::{json, Map, Value};
use tracing::{info, warn};

use eve_records::py::{error_name, iter_of, repr_str, str_of, str_or_empty, truthy};
use eve_records::{RecallConfig, RecordsReader};
use eve_transcripts::memory_index::{self as mi, DaySource, Embedder, MemoryIndex};
use eve_transcripts::py as tpy;
use eve_transcripts::transcripts::{
    configured, match_query, render, Dt, Pred, Ranker, Search, Today, Transcripts, MEETINGS_SUBDIR,
    READ_MAX_CHARS, SEARCH_DAYS_BACK, VOICE_CHARS,
};

pub const FORGET_TTL_SECS: f64 = 600.0; // a forget token lives ten minutes
pub const MAX_PENDING_TOKENS: usize = 16;
pub const RECENT_CHARS: usize = VOICE_CHARS; // recent_conversation: the other channel, newest kept
pub const MIGRATED_FILE: &str = ".migrated";
pub const RETRY_DAYS_BACK: i64 = 365; // EVE_MEMORY_RETRY: an empty search is tried again this far back
/// Places eve cannot forget in: named, never pretended. The chief ledger (chief_ledger, the cards and their
/// events under ~/.config/eve-bridge/chief) holds card titles and goals; it is named here until a forget
/// layer covers it (chief design, 2026-09-29, P6).
pub const NOT_COVERED: [&str; 3] = ["claude-mem", "vault", "chief ledger"];
pub const LAYERS: [&str; 5] = ["transcripts", "records", "index", "archive", "meetings"];
/// Named while no chat log is attached (`Memory::chat_log()` is None).
pub const CHAT_LOG_NOT_COVERED: &str = "telegram chat log";

// A forget of a sentence also takes what the dream wrote from it in its own words ("failed my driving test" →
// "failing the driving test"): in the stores the dream writes (records, the index, the archive), a line goes when
// it holds most of the sentence's distinctive words, compared by their first four letters. The transcripts keep
// the exact rule: they hold the owner's own words.
/// A query with fewer distinctive words than this forgets by the exact rule only.
pub const REWORD_MIN_WORDS: usize = 3;
/// And a line must hold at least this share of them (and at least REWORD_MIN_WORDS).
pub const REWORD_MIN_SHARE: f64 = 0.5;
static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-z0-9]+").expect("the word pattern"));
const COMMON_WORDS: &str = "a about after again all also am an and any are as at back be been before being but by can
could did do does doing done for from get got had has have having he her here him his how i if in into is it its
just me more most my no not now of off on once only or our out over really said she so some still such than
that the their them then there these they this those to too up very was we were what when where which while
who why will with would yes yet you your honestly actually basically";
static COMMON: LazyLock<HashSet<&'static str>> = LazyLock::new(|| COMMON_WORDS.split_whitespace().collect());

// The retired debrief store seeds HIGHLIGHTS.md with a worked example; only eve's own section is the owner's.
pub const OWN_SECTION: &str = "from talking";
pub const EXAMPLE_MARK: &str = "*(example)*";
pub const TEMPLATE_BANNER: &str = "> **Template.**";

// Python's `$` (no MULTILINE) also matches before a final newline: `\n?\z`. Python's `\s` adds \x1c-\x1f.
static DAY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}\n?\z").expect("the day pattern"));
static READ_REF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(\d{4}-\d{2}-\d{2})(?:[ T]+(\d{1,2}:\d{2})(?:[\s\x1c-\x1f]*-[\s\x1c-\x1f]*(\d{1,2}:\d{2}))?)?\n?\z")
        .expect("the read pattern")
});
static STAR_DATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\s\x1c-\x1f]*\((\d{4}-\d{2}-\d{2})\)[\s\x1c-\x1f]*\n?\z").expect("the star date pattern")
});
static FRONTMATTER_END: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\n---[ \t]*(?:\n|\z)").expect("the frontmatter pattern"));

fn keys(text: &str) -> HashSet<String> {
    let folded = tpy::casefold(text).replace('\'', "");
    WORD.find_iter(&folded)
        .map(|m| m.as_str())
        .filter(|w| !COMMON.contains(w) && (w.len() > 2 || w.bytes().all(|b| b.is_ascii_digit())))
        .map(|w| w[..w.len().min(4)].to_string())
        .collect()
}

/// The predicate for a line the dream wrote from `query` in other words, or None when `query` is too short
/// (or quoted) to judge that by its words.
pub fn reworded(query: &str) -> Option<Pred> {
    if query.contains('"') {
        return None;
    }
    let want = keys(query);
    if want.len() < REWORD_MIN_WORDS {
        return None;
    }
    let need = REWORD_MIN_WORDS.max((want.len() as f64 * REWORD_MIN_SHARE).ceil() as usize);
    Some(Box::new(move |text: &str| keys(text).intersection(&want).count() >= need))
}

/// `EVE_MEMORY=1` turns memory on; the older `EVE_RECORDS=1` still does. `None` reads the process environment.
pub fn enabled(environ: Option<&HashMap<String, String>>) -> bool {
    configured(environ).enabled
}

// ---- the tools ---------------------------------------------------------------------------------

fn tool(name: &str, description: &str, properties: Map<String, Value>) -> Value {
    let required: Vec<Value> = properties.keys().map(|k| Value::String(k.clone())).collect();
    json!({"type": "function", "name": name, "strict": true, "description": description,
           "parameters": {"type": "object", "additionalProperties": false,
                          "required": required, "properties": Value::Object(properties)}})
}

fn props(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

const SEARCH_OPENING: &str = "Search your memory of the owner. Use it before you answer a question about their life, their plans, \
their taste or a past conversation, and before you say that you do not know. It returns three groups, \
each with dates: 'hits' are lines from your records, 'recalled' are memories found by meaning, and \
'said' are the words from past conversations, meetings and old notes, with the line before and after.";
// memory_search as the three search switches shape it (memory_index). Each sentence states what is true with
// its switch on; with every switch off this is MEMORY_SEARCH, word for word. Fixed for the process, so the tool
// list stays a stable, cached prefix.
const SEARCH_KEYWORD: &str = " Every word of the query must be in a 'said' line, so use few words. Put a phrase in double \
quotes to find it as written.";
const SEARCH_RANKED: &str = " The 'said' lines come best match first, ranked by meaning and by the words they share with \
the query, so name the thing itself: the person, the place or the object.";
const SEARCH_TIME: &str = " When the question names a time (yesterday, last week, in May, two weeks ago, a date), pass it as \
from and to.";
const SEARCH_RETRY: &str = " When a search finds nothing, search once more with other words (a synonym, or the one most \
telling word) before you say it is not in your memory.";
/// Only where code enforces it: the Telegram brain.
const SEARCH_CAP: &str = " A turn has at most three memory calls.";
const TIME_ARG: &str = "A day as year-month-day, or the owner's own words for the time, like yesterday, last week, in May \
or two weeks ago. Null for no limit.";

pub static MEMORY_SEARCH: LazyLock<Value> = LazyLock::new(|| {
    tool(
        "memory_search",
        &format!("{SEARCH_OPENING}{SEARCH_KEYWORD}"),
        props(&[
            ("query", json!({"type": "string", "description": "A few words, as the owner would say them."})),
            ("days_back", json!({"type": ["integer", "null"],
                                 "description": "How many days of conversation to search. Null for the last 30 days."})),
        ]),
    )
});
pub static MEMORY_READ: LazyLock<Value> = LazyLock::new(|| {
    tool(
        "memory_read",
        "Read one thing from memory in full. Give a record id to read that record: the ids are in your profile \
and in search results. Give a day to read what was said on that day: write the day as year-month-day. \
After the day, you can add a start time, or a start time and an end time joined by a hyphen, in \
24-hour hours:minutes. When the text stops early, read again from the time it gives.",
        props(&[(
            "ref",
            json!({"type": "string", "description": "A record id, or a day with an optional time or time range."}),
        )]),
    )
});
pub static FORGET_PREVIEW: LazyLock<Value> = LazyLock::new(|| {
    tool(
        "forget_preview",
        "Use this only when the owner explicitly asks you to forget something. It counts every place in your \
memory that holds those words and returns a token. It changes nothing and does not show the words. \
Tell the owner the counts and ask them to confirm. Do not call forget_apply in the same turn.",
        props(&[("query", json!({"type": "string", "description": "The words to forget, as few as identify them."}))]),
    )
});
pub static FORGET_APPLY: LazyLock<Value> = LazyLock::new(|| {
    tool(
        "forget_apply",
        "Forget, permanently, what forget_preview counted. Call it only after the owner confirms in a new \
message. The token works once and expires after ten minutes; when it has expired, preview again.",
        props(&[("token", json!({"type": "string", "description": "The token from forget_preview."}))]),
    )
});
pub static RECENT_CONVERSATION: LazyLock<Value> = LazyLock::new(|| {
    tool(
        "recent_conversation",
        "Read what the owner and you said on the other channel, typed in chat, since this voice conversation \
started. Use it when the owner refers to something they just wrote to you.",
        Map::new(),
    )
});

pub static TOOLS: LazyLock<Vec<Value>> = LazyLock::new(|| {
    vec![MEMORY_SEARCH.clone(), MEMORY_READ.clone(), FORGET_PREVIEW.clone(), FORGET_APPLY.clone()]
});
pub static VOICE_TOOLS: LazyLock<Vec<Value>> = LazyLock::new(|| {
    let mut v = TOOLS.clone();
    v.push(RECENT_CONVERSATION.clone());
    v
});

/// The memory_search schema for these switches. All off: MEMORY_SEARCH itself. `capped`: the caller counts
/// memory calls per turn (the Telegram brain does; voice does not), so the description may say so.
pub fn memory_search_tool(hybrid: bool, timeargs: bool, retry: bool, capped: bool) -> Value {
    if !(hybrid || timeargs || retry) {
        return MEMORY_SEARCH.clone();
    }
    let mut text = format!("{SEARCH_OPENING}{}", if hybrid { SEARCH_RANKED } else { SEARCH_KEYWORD });
    if timeargs {
        text.push_str(SEARCH_TIME);
    }
    if retry {
        text.push_str(SEARCH_RETRY);
        if capped {
            text.push_str(SEARCH_CAP);
        }
    }
    let mut properties = MEMORY_SEARCH["parameters"]["properties"].as_object().cloned().unwrap_or_default();
    if timeargs {
        properties.insert(
            "from".into(),
            json!({"type": ["string", "null"], "description": format!("The first day to search. {TIME_ARG}")}),
        );
        properties.insert(
            "to".into(),
            json!({"type": ["string", "null"], "description": format!("The last day to search. {TIME_ARG}")}),
        );
    }
    tool("memory_search", &text, properties)
}

// ---- markdown line removal (archive and meeting notes) --------------------------------------------

/// (frontmatter including its closing fence, body). Forget never touches frontmatter: it holds ids.
fn split_frontmatter(text: &str) -> (&str, &str) {
    if !text.starts_with("---\n") {
        return ("", text);
    }
    match FRONTMATTER_END.find_at(text, 3) {
        Some(m) => (&text[..m.end()], &text[m.end()..]),
        None => ("", text),
    }
}

/// `str.splitlines(keepends=True)`: every line boundary Python knows, each kept on its line.
fn splitlines_keepends(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let boundary = matches!(
            c,
            '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if !boundary {
            continue;
        }
        let mut end = i + c.len_utf8();
        if c == '\r' {
            if let Some(&(j, '\n')) = chars.peek() {
                chars.next();
                end = j + 1;
            }
        }
        out.push(&text[start..end]);
        start = end;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// `os.path.lexists`: there, or a symlink (even a broken one).
fn lexists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// `entry.is_dir()` as os.walk asks it: a directory, or a symlink to one.
fn is_dir_following(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_dir())
}

fn walk_markdown(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else {
        return; // os.walk ignores a folder it cannot list
    };
    let mut subdirs = Vec::new();
    for entry in rd.flatten() {
        let p = entry.path();
        let name = entry.file_name();
        if is_dir_following(&p) {
            if name != ".git" && !is_symlink(&p) {
                subdirs.push(p);
            }
        } else if name.as_bytes().ends_with(b".md") && !is_symlink(&p) {
            out.push(p);
        }
    }
    for d in subdirs {
        walk_markdown(&d, out);
    }
}

/// Every regular .md file under `folder`, never inside a .git and never through a symlink.
fn markdown_files(folder: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !folder.is_dir() || is_symlink(folder) {
        return out;
    }
    walk_markdown(folder, &mut out);
    out.sort();
    out
}

fn token_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    rand::rng().fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Replace `path` whole: a temp file beside it with the same mode, fsync, rename.
pub fn atomic_rewrite(path: &Path, text: &str) -> io::Result<()> {
    let mode = fs::metadata(path)?.permissions().mode() & 0o7777;
    let mut tmp_name = OsString::from(".");
    tmp_name.push(path.file_name().unwrap_or_else(|| OsStr::new("")));
    tmp_name.push(format!(".{}.{}.tmp", std::process::id(), token_hex(4)));
    let tmp = path.with_file_name(tmp_name);
    let written = (|| -> io::Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&tmp)?;
        f.set_permissions(fs::Permissions::from_mode(mode))?;
        f.write_all(text.as_bytes())?;
        // os.fsync: plain fsync(2) (std's sync_all is F_FULLFSYNC on macOS)
        if unsafe { libc::fsync(f.as_raw_fd()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        drop(f);
        fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// A file the forget could not rewrite: its words are still on disk. `removed` is what did go.
#[derive(Debug, thiserror::Error)]
#[error("{failed} markdown file(s) could not be rewritten")]
pub struct MarkdownForgetError {
    pub removed: usize,
    pub failed: usize,
}

/// Remove every body line `pred` matches from the markdown under `folder`. → lines (to be) removed.
/// A file that cannot be rewritten does not count, and after trying every file the forget returns
/// MarkdownForgetError, so forget_apply never reports words still on disk as forgotten.
pub fn forget_markdown(folder: &Path, pred: &dyn Fn(&str) -> bool, dry_run: bool) -> Result<usize, MarkdownForgetError> {
    forget_markdown_with(folder, pred, dry_run, &atomic_rewrite)
}

/// [`forget_markdown`] with the rewrite step given (the seam a test patches `_atomic_rewrite` through).
pub fn forget_markdown_with(
    folder: &Path,
    pred: &dyn Fn(&str) -> bool,
    dry_run: bool,
    rewrite: &dyn Fn(&Path, &str) -> io::Result<()>,
) -> Result<usize, MarkdownForgetError> {
    let (mut total, mut failed) = (0usize, 0usize);
    for path in markdown_files(folder) {
        let Ok(raw) = fs::read(&path) else {
            continue;
        };
        let text = tpy::read_text(&raw);
        let (head, body) = split_frontmatter(&text);
        let rows = splitlines_keepends(body);
        let kept: Vec<&str> = rows.iter().copied().filter(|r| !pred(r)).collect();
        let gone = rows.len() - kept.len();
        if gone == 0 {
            continue;
        }
        if dry_run {
            total += gone;
            continue;
        }
        let mut new_text = String::with_capacity(text.len());
        new_text.push_str(head);
        for r in &kept {
            new_text.push_str(r);
        }
        if let Err(e) = rewrite(&path, &new_text) {
            failed += 1;
            warn!(
                "memory: a markdown file could not be rewritten during forget ({})",
                tpy::os_error_name(&e)
            );
            continue;
        }
        total += gone;
    }
    if failed > 0 {
        return Err(MarkdownForgetError { removed: total, failed });
    }
    Ok(total)
}

// ---- the stores the facade lends out --------------------------------------------------------------

/// records.py as the facade calls it (Python imports it when first used, and tests swap it): the one-pager, the
/// stars, the hidden list, the reader, forget, and the history. [`EveRecords`] is the real one.
pub trait RecordsStore: Send + Sync {
    fn profile(&self, cfg: &RecallConfig) -> anyhow::Result<String>;
    fn profile_for_voice(&self, cfg: &RecallConfig) -> anyhow::Result<String>;
    fn talking_section(&self, cfg: &RecallConfig) -> anyhow::Result<String>;
    fn star(&self, cfg: &RecallConfig, text: &str, when: Option<NaiveDateTime>) -> anyhow::Result<Option<String>>;
    fn hide(&self, cfg: &RecallConfig, text: &str) -> anyhow::Result<Option<String>>;
    fn unhide(&self, cfg: &RecallConfig, text: &str) -> anyhow::Result<bool>;
    fn hidden(&self, cfg: &RecallConfig) -> anyhow::Result<Vec<String>>;
    fn hidden_match(&self, cfg: &RecallConfig) -> anyhow::Result<Option<Pred>>;
    /// `RecordsReader(cfg, None).search(query)`.
    fn search(&self, cfg: &RecallConfig, query: &str) -> anyhow::Result<Value>;
    /// `RecordsReader(cfg, None).get(rid)`.
    fn get(&self, cfg: &RecallConfig, rid: &str) -> anyhow::Result<Value>;
    fn forget_lines(&self, cfg: &RecallConfig, pred: &dyn Fn(&str) -> bool, dry_run: bool) -> anyhow::Result<usize>;
    fn squash_history(&self, repo: &Path) -> anyhow::Result<bool>;
    fn seal(&self, repo: &Path) -> anyhow::Result<()>;
}

/// The records on disk (eve-records).
#[derive(Debug, Clone, Copy, Default)]
pub struct EveRecords;

impl RecordsStore for EveRecords {
    fn profile(&self, cfg: &RecallConfig) -> anyhow::Result<String> {
        Ok(eve_records::profile(cfg))
    }

    fn profile_for_voice(&self, cfg: &RecallConfig) -> anyhow::Result<String> {
        Ok(eve_records::profile_for_voice(cfg))
    }

    fn talking_section(&self, cfg: &RecallConfig) -> anyhow::Result<String> {
        Ok(eve_records::talking_section(cfg))
    }

    fn star(&self, cfg: &RecallConfig, text: &str, when: Option<NaiveDateTime>) -> anyhow::Result<Option<String>> {
        Ok(eve_records::star(cfg, text, when))
    }

    fn hide(&self, cfg: &RecallConfig, text: &str) -> anyhow::Result<Option<String>> {
        Ok(eve_records::hide(cfg, text, None))
    }

    fn unhide(&self, cfg: &RecallConfig, text: &str) -> anyhow::Result<bool> {
        Ok(eve_records::unhide(cfg, text))
    }

    fn hidden(&self, cfg: &RecallConfig) -> anyhow::Result<Vec<String>> {
        Ok(eve_records::hidden(cfg))
    }

    fn hidden_match(&self, cfg: &RecallConfig) -> anyhow::Result<Option<Pred>> {
        Ok(eve_records::hidden_match(cfg))
    }

    fn search(&self, cfg: &RecallConfig, query: &str) -> anyhow::Result<Value> {
        Ok(RecordsReader::new(cfg.clone(), None).search(query))
    }

    fn get(&self, cfg: &RecallConfig, rid: &str) -> anyhow::Result<Value> {
        Ok(RecordsReader::new(cfg.clone(), None).get(rid))
    }

    fn forget_lines(&self, cfg: &RecallConfig, pred: &dyn Fn(&str) -> bool, dry_run: bool) -> anyhow::Result<usize> {
        Ok(eve_records::forget_lines(cfg, pred, dry_run))
    }

    fn squash_history(&self, repo: &Path) -> anyhow::Result<bool> {
        Ok(eve_records::squash_history(repo))
    }

    fn seal(&self, repo: &Path) -> anyhow::Result<()> {
        eve_records::records::seal(repo).map_err(anyhow::Error::from)
    }
}

/// The search by meaning (mem0_memory.OwnerMemory) as the facade uses it: `search`, `find`, `forget`, and, when
/// the index has one, `forget_matching` (find and delete in one hold of its lock).
pub trait MeaningIndex: Send + Sync {
    /// Rows `{"text", "day", "channel"[, "src"]}` (or plain strings), best first.
    fn search(&self, query: &str) -> anyhow::Result<Vec<Value>>;
    /// Every memory whose text `pred` matches, as `{"id", "text", ...}`.
    fn find(&self, pred: &dyn Fn(&str) -> bool) -> anyhow::Result<Vec<Value>>;
    /// Delete these memory ids → how many went.
    fn forget(&self, ids: &[String]) -> anyhow::Result<usize>;
    /// Find and delete in one step. None: this index has no such method (the facade then finds, then forgets).
    fn forget_matching(&self, _pred: &dyn Fn(&str) -> bool) -> Option<anyhow::Result<usize>> {
        None
    }
}

/// The Telegram chat log as forget reaches it (the "chat_log" layer).
pub trait ChatLogLayer: Send + Sync {
    fn count(&self, pred: &dyn Fn(&str) -> bool) -> anyhow::Result<usize>;
    fn forget(&self, pred: &dyn Fn(&str) -> bool) -> anyhow::Result<usize>;
}

impl ChatLogLayer for eve_chat_log::ChatLog {
    fn count(&self, pred: &dyn Fn(&str) -> bool) -> anyhow::Result<usize> {
        Ok(eve_chat_log::ChatLog::count(self, pred))
    }

    fn forget(&self, pred: &dyn Fn(&str) -> bool) -> anyhow::Result<usize> {
        Ok(eve_chat_log::ChatLog::forget(self, pred))
    }
}

/// The ranked index as the transcripts hold it: through a weak reference, so the index (which holds the
/// transcripts) and the transcripts (which hold the ranker) do not keep each other alive.
struct WeakRanker(Weak<MemoryIndex>);

impl Ranker for WeakRanker {
    fn search(
        &self,
        query: &str,
        first_day: &str,
        last_day: &str,
        limit: usize,
        channels: Option<&[String]>,
    ) -> anyhow::Result<Option<Value>> {
        match self.0.upgrade() {
            Some(idx) => Ranker::search(&*idx, query, first_day, last_day, limit, channels),
            None => Ok(None),
        }
    }
}

// ---- the facade ----------------------------------------------------------------------------------

/// A clock in seconds that only moves forward (`time.monotonic`): it times the forget tokens.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;
/// How a markdown file is replaced during a forget ([`atomic_rewrite`]).
pub type Rewriter = Arc<dyn Fn(&Path, &str) -> io::Result<()> + Send + Sync>;

static PROCESS_START: LazyLock<Instant> = LazyLock::new(Instant::now);

/// `time.monotonic()`: seconds on a clock that never goes back.
pub fn monotonic() -> f64 {
    PROCESS_START.elapsed().as_secs_f64()
}

#[derive(Debug, Clone)]
struct Pending {
    query: String,
    expires: f64,
}

/// What [`Memory::with_options`] takes beyond the stores. Every field left at its default is the daemon's.
#[derive(Clone, Default)]
pub struct MemoryOptions {
    /// Times the forget tokens (None: [`monotonic`]); tests inject one.
    pub clock: Option<Clock>,
    /// Start the search index's worker (its build and its embeddings). Only the daemon passes true.
    pub background: bool,
    /// Where the three search switches are read (None: the process environment).
    pub environ: Option<HashMap<String, String>>,
    /// The ranked index's embedder (None: memory_index's openai_embedder).
    pub embedder: Option<Embedder>,
    /// records.py (None: [`EveRecords`]).
    pub records: Option<Arc<dyn RecordsStore>>,
    /// How a markdown file is rewritten during a forget (None: [`atomic_rewrite`]).
    pub rewrite: Option<Rewriter>,
}

/// What both brains are lent. Built once by the daemon; every method is synchronous and never fails, so
/// callers run the slow ones (search, forget) through `spawn_blocking`.
///
/// `transcripts` is public: the brains append each turn through it. `index` is mem0 (or None when it is
/// off). The forget, dream and reindex commands and the eval open the ranked index without its worker, so a
/// short-lived process sends nothing to the embeddings endpoint and holds no lock a forget waits on.
pub struct Memory {
    pub cfg: RecallConfig,
    pub transcripts: Arc<Transcripts>,
    pub index: Option<Arc<dyn MeaningIndex>>,
    clock: Clock,
    pending: Mutex<IndexMap<String, Pending>>,
    // The Telegram chat log, set by the daemon when it keeps one: forget reaches it as the "chat_log" layer.
    // None: no such layer is counted or named.
    chat_log: RwLock<Option<Arc<dyn ChatLogLayer>>>,
    /// The search switches (memory_index), read once: the tool schemas must not change mid-process.
    pub hybrid: bool,
    pub timeargs: bool,
    pub retry: bool,
    pub ranked: Option<Arc<MemoryIndex>>,
    records: Arc<dyn RecordsStore>,
    rewrite: Rewriter,
}

impl std::fmt::Debug for Memory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Memory")
            .field("cfg", &self.cfg)
            .field("index", &self.index.is_some())
            .field("hybrid", &self.hybrid)
            .field("timeargs", &self.timeargs)
            .field("retry", &self.retry)
            .finish()
    }
}

fn unavailable() -> Value {
    json!({"ok": false, "reason": "memory is unavailable right now"})
}

/// `int(v)` as Python reads a tool argument: a bool is 0 or 1, a float is cut toward zero, a string of
/// (Unicode) digits with an optional sign and single underscores is read; anything else is not a number.
fn py_int(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(i)
            } else if n.as_u64().is_some() {
                Some(i64::MAX)
            } else {
                let x = n.as_f64()?;
                if x.is_finite() {
                    Some(x.trunc() as i64)
                } else {
                    None
                }
            }
        }
        Value::String(s) => {
            let s = tpy::strip(s);
            let (neg, body) = match s.chars().next() {
                Some('-') => (true, &s[1..]),
                Some('+') => (false, &s[1..]),
                _ => (false, s),
            };
            if body.is_empty() || body.starts_with('_') || body.ends_with('_') || body.contains("__") {
                return None;
            }
            let digits: String = body.chars().filter(|c| *c != '_').collect();
            let n = tpy::int(&digits)?;
            Some(if neg { -n } else { n })
        }
        _ => None,
    }
}

/// `" ".join(text.split())`: Python's whitespace runs folded to one space, the ends dropped.
fn squeeze(text: &str) -> String {
    text.split(tpy::is_space).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// A `from`/`to` argument as `resolve_args` reads it: a falsy value (null, "", 0, false, [], {}) is never read
/// as a time and never marks the time unread, so it is no value at all; anything else is its `str()`.
fn time_arg(v: Option<&Value>) -> Option<String> {
    match v {
        Some(v) if truthy(v) => Some(str_of(v)),
        _ => None,
    }
}

fn iso(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

impl Memory {
    /// The daemon's defaults except the worker: see [`Memory::with_options`].
    pub fn new(cfg: RecallConfig, transcripts: Arc<Transcripts>, index: Option<Arc<dyn MeaningIndex>>) -> Memory {
        Memory::with_options(cfg, transcripts, index, MemoryOptions::default())
    }

    pub fn with_options(
        cfg: RecallConfig,
        transcripts: Arc<Transcripts>,
        index: Option<Arc<dyn MeaningIndex>>,
        opts: MemoryOptions,
    ) -> Memory {
        let env = opts.environ.as_ref();
        let hybrid = mi::switch("hybrid", env);
        let timeargs = mi::switch("timeargs", env);
        let retry = mi::switch("retry", env);
        let mut ranked = None;
        if hybrid {
            let source: Arc<dyn DaySource> = transcripts.clone();
            ranked = mi::open_index(&cfg.store, source, opts.embedder.clone(), true);
            if let Some(r) = &ranked {
                transcripts.set_ranker(Some(Arc::new(WeakRanker(Arc::downgrade(r)))));
                let weak = Arc::downgrade(r);
                transcripts.add_listener(Arc::new(move |day: &str| {
                    if let Some(r) = weak.upgrade() {
                        r.notify(day);
                    }
                }));
                if opts.background {
                    r.start();
                }
            }
        }
        Memory {
            cfg,
            transcripts,
            index,
            clock: opts.clock.unwrap_or_else(|| Arc::new(monotonic)),
            pending: Mutex::new(IndexMap::new()),
            chat_log: RwLock::new(None),
            hybrid,
            timeargs,
            retry,
            ranked,
            records: opts.records.unwrap_or_else(|| Arc::new(EveRecords)),
            rewrite: opts.rewrite.unwrap_or_else(|| Arc::new(atomic_rewrite)),
        }
    }

    /// The Telegram chat log forget reaches, or None.
    pub fn chat_log(&self) -> Option<Arc<dyn ChatLogLayer>> {
        self.chat_log.read().clone()
    }

    /// Attach (or with None detach) the Telegram chat log.
    pub fn set_chat_log(&self, log: Option<Arc<dyn ChatLogLayer>>) {
        *self.chat_log.write() = log;
    }

    // -- what goes in the prompt ---------------------------------------------------------------

    /// The one-pager (≤ 6000 chars), with stars the dream has not absorbed yet on top. "" on any failure.
    pub fn profile(&self) -> String {
        self.records.profile(&self.cfg).unwrap_or_else(|e| {
            warn!("memory: profile unavailable ({})", error_name(&e));
            String::new()
        })
    }

    /// The profile without the record index (≤ 3000 chars): the Live voice prompt is latency-bound.
    pub fn profile_for_voice(&self) -> String {
        self.records.profile_for_voice(&self.cfg).unwrap_or_else(|e| {
            warn!("memory: voice profile unavailable ({})", error_name(&e));
            String::new()
        })
    }

    /// The profile's "How they like to talk" section, whole (records.talking_section). "" on any failure.
    pub fn talking_section(&self) -> String {
        self.records.talking_section(&self.cfg).unwrap_or_else(|e| {
            warn!("memory: talking section unavailable ({})", error_name(&e));
            String::new()
        })
    }

    /// Today on both channels, within `max_chars` (see Transcripts::today_block_with).
    pub fn today(&self, max_chars: i64, o: &Today) -> String {
        self.transcripts.today_block_with(max_chars, o)
    }

    /// The owner said "remember that": into starred.md, on the profile from the next message.
    pub fn star(&self, text: &str) -> Option<String> {
        self.records.star(&self.cfg, text, None).unwrap_or_else(|e| {
            warn!("memory: star failed ({})", error_name(&e));
            None
        })
    }

    // -- don't mention this again (records.hide; the app's Memory card) ------------------------------

    /// Leave out every line these words match, from the profile and every search, until unhidden. Deletes
    /// nothing. → the line as kept in hidden.md, or None.
    pub fn hide(&self, text: &str) -> Option<String> {
        self.records.hide(&self.cfg, text).unwrap_or_else(|e| {
            warn!("memory: hide failed ({})", error_name(&e));
            None
        })
    }

    pub fn unhide(&self, text: &str) -> bool {
        self.records.unhide(&self.cfg, text).unwrap_or_else(|e| {
            warn!("memory: unhide failed ({})", error_name(&e));
            false
        })
    }

    pub fn hidden(&self) -> Vec<String> {
        self.records.hidden(&self.cfg).unwrap_or_else(|e| {
            warn!("memory: hidden list unavailable ({})", error_name(&e));
            Vec::new()
        })
    }

    fn hidden_match(&self) -> Option<Pred> {
        // an unreadable hidden list hides nothing
        self.records.hidden_match(&self.cfg).unwrap_or_else(|e| {
            warn!("memory: hidden list unavailable ({})", error_name(&e));
            None
        })
    }

    // -- tools -------------------------------------------------------------------------------------

    /// The memory tools, strict schemas, stable order. Copies: a caller may annotate its own.
    pub fn tools(&self, voice: bool) -> Vec<Value> {
        let search = memory_search_tool(self.hybrid, self.timeargs, self.retry, !voice);
        let rest = if voice { &VOICE_TOOLS[1..] } else { &TOOLS[1..] };
        std::iter::once(search).chain(rest.iter().cloned()).collect()
    }

    /// Run one memory tool. `since` and `channel` serve recent_conversation: the session's start and
    /// the caller's own channel, which is left out. Never fails: a tool failure is an answer, never a crash.
    pub fn handle_tool(&self, name: &str, args: &Value, since: Option<Dt>, channel: &str) -> Value {
        let empty = Map::new();
        let args = args.as_object().unwrap_or(&empty);
        let run = || -> anyhow::Result<Option<Value>> {
            Ok(Some(match name {
                "memory_search" => {
                    let query = str_or_empty(args.get("query"));
                    if self.timeargs {
                        self.search_with(&query, args.get("days_back"), args.get("from"), args.get("to"))
                    } else {
                        self.search(&query, args.get("days_back"))
                    }
                }
                "memory_read" => self.read(&str_or_empty(args.get("ref")))?,
                "forget_preview" => self.forget_preview(&str_or_empty(args.get("query"))),
                "forget_apply" => self.forget_apply(&str_or_empty(args.get("token"))),
                "recent_conversation" => self.recent_conversation(since, channel),
                _ => return Ok(None),
            }))
        };
        match catch_unwind(AssertUnwindSafe(run)) {
            Ok(Ok(Some(out))) => out,
            Ok(Ok(None)) => json!({"ok": false, "reason": format!("unknown tool {}", repr_str(name))}),
            Ok(Err(e)) => {
                warn!("memory: {} failed ({})", name, error_name(&e));
                unavailable()
            }
            Err(_) => {
                warn!("memory: {} failed (panic)", name);
                unavailable()
            }
        }
    }

    /// [`Memory::search_with`] with no time range.
    pub fn search(&self, query: &str, days_back: Option<&Value>) -> Value {
        self.search_with(query, days_back, None, None)
    }

    /// Three dated groups: record lines, recalled meanings, and the words said.
    ///
    /// With EVE_MEMORY_TIMEARGS, `date_from`/`date_to` (a day, or words like "last week") limit the words
    /// said to a range (memory_index::resolve_args). Without the ranked index a range that finds nothing is read
    /// again with half the telling words ("fewer_words"); still nothing, it is widened once ("widened"). With
    /// EVE_MEMORY_RETRY, words said that find nothing are searched again by code over a year (and, without
    /// the ranked index, with half the words), and the result says to search once more with other words.
    pub fn search_with(
        &self,
        query: &str,
        days_back: Option<&Value>,
        date_from: Option<&Value>,
        date_to: Option<&Value>,
    ) -> Value {
        let started = Instant::now();
        if match_query(query).is_none() {
            return json!({"ok": false, "reason": "the query has no word to search for"});
        }
        let days = match days_back {
            None | Some(Value::Null) => SEARCH_DAYS_BACK,
            Some(v) => py_int(v).unwrap_or(SEARCH_DAYS_BACK),
        };
        let date_from = date_from.filter(|v| !v.is_null());
        let date_to = date_to.filter(|v| !v.is_null());
        std::thread::scope(|scope| {
            let mut out = Map::new();
            out.insert("ok".into(), Value::Bool(true));
            out.insert("hits".into(), json!([]));
            out.insert("recalled".into(), json!([]));
            out.insert("said".into(), json!([]));
            // The index search is an embedding round-trip (mem0, its own 3 s cap); it runs on a thread while the
            // records and the transcripts are read here, and is joined at the end (2026-10-09 review: it was first,
            // and the local reads waited the whole round-trip). Its result, order and cap are the same.
            let joiner = self.index.as_deref().map(|index| {
                std::thread::Builder::new()
                    .name("memory-recall".into())
                    .spawn_scoped(scope, move || index.search(query))
                    .map_err(|_| index)
            });
            match self.records.search(&self.cfg, query) {
                Ok(got) => {
                    out.insert("hits".into(), Value::Array(iter_of(got.get("hits"))));
                }
                Err(e) => warn!("memory: records search failed ({})", error_name(&e)),
            }
            let mut rng: Option<(NaiveDate, NaiveDate)> = None;
            if self.timeargs && (date_from.is_some() || date_to.is_some()) {
                let (a, b) = (time_arg(date_from), time_arg(date_to));
                let (got, unread) = mi::resolve_args(a.as_deref(), b.as_deref(), self.transcripts.now());
                rng = got;
                if unread {
                    out.insert(
                        "time_note".into(),
                        json!("a time could not be read as a date; searched without it"),
                    );
                }
                if let Some((lo, hi)) = rng {
                    out.insert("range".into(), json!({"from": iso(lo), "to": iso(hi)}));
                }
            }
            let has = |v: &Value, key: &str| v.get(key).is_some_and(truthy);
            let mut said = self.said(query, days, rng, false);
            if rng.is_some() && !has(&said, "hits") && !has(&said, "ranked") {
                // Inside a named time, half the telling words are enough: the range already keeps it narrow.
                said = self.said(query, days, rng, true);
                if has(&said, "hits") {
                    out.insert("fewer_words".into(), Value::Bool(true));
                }
            }
            if let Some(r) = rng {
                if !has(&said, "hits") {
                    let today = mi::owner_day(self.transcripts.now());
                    let wide = mi::widen(r, today);
                    let loose = !has(&said, "ranked");
                    said = self.said(query, days, Some(wide), loose);
                    out.insert("widened".into(), json!({"from": iso(wide.0), "to": iso(wide.1)}));
                }
            }
            if self.retry && !has(&said, "hits") {
                let wide = days.max(RETRY_DAYS_BACK);
                said = self.said(query, wide, None, false);
                if !has(&said, "hits") && !has(&said, "ranked") {
                    said = self.said(query, wide, None, true);
                }
                let tail = if has(&said, "ranked") { "" } else { ", with fewer words" };
                out.insert("searched_again".into(), json!(format!("the last {wide} days{tail}")));
            }
            let hide = self.hidden_match();
            let hidden = |text: &str| hide.as_ref().is_some_and(|p| p(text));
            let said_hits: Vec<Value> = iter_of(said.get("hits"))
                .into_iter()
                .filter(|h| !(h.is_object() && hidden(&str_or_empty(h.get("text")))))
                .collect();
            out.insert("said".into(), Value::Array(said_hits));
            if has(&said, "truncated") {
                out.insert("truncated".into(), Value::Bool(true));
            }
            if let Some(joiner) = joiner {
                let got = match joiner {
                    Ok(handle) => handle.join().unwrap_or_else(|_| Err(anyhow::anyhow!("the index search panicked"))),
                    // no thread to be had: the search runs here
                    Err(index) => index.search(query),
                };
                match got {
                    Ok(rows) => {
                        let recalled: Vec<Value> = rows
                            .iter()
                            .filter(|r| {
                                let text = if r.is_object() { str_or_empty(r.get("text")) } else { str_of(r) };
                                !hidden(&text)
                            })
                            .map(|r| Value::String(recalled_line(r)))
                            .collect();
                        out.insert("recalled".into(), Value::Array(recalled));
                    }
                    Err(e) => warn!("memory: index search failed ({})", error_name(&e)),
                }
            }
            let empty = |k: &str| out.get(k).and_then(Value::as_array).is_none_or(|a| a.is_empty());
            if empty("hits") && empty("recalled") && empty("said") {
                let note = if self.retry {
                    "nothing matched; search once more with other words (a synonym, or the one most telling word) \
before you say it is not in your memory"
                } else {
                    "nothing matched"
                };
                out.insert("note".into(), json!(note));
            }
            let n = |k: &str| out.get(k).and_then(Value::as_array).map_or(0, Vec::len);
            info!(
                "memory: search → {} hits, {} recalled, {} said in {} ms",
                n("hits"),
                n("recalled"),
                n("said"),
                started.elapsed().as_millis()
            );
            Value::Object(out)
        })
    }

    fn said(&self, query: &str, days: i64, rng: Option<(NaiveDate, NaiveDate)>, loose: bool) -> Value {
        let mut o = Search { days_back: days, loose, ..Search::default() };
        if let Some((lo, hi)) = rng {
            o.since = iso(lo);
            o.until = iso(hi);
        }
        self.transcripts.search_with(query, &o)
    }

    /// One recalled memory as the model reads it: "(day or src, channel) text".
    pub fn recalled(row: &Value) -> String {
        recalled_line(row)
    }

    /// A day or a stretch of one (transcript), else a record id.
    pub fn read(&self, reference: &str) -> anyhow::Result<Value> {
        let reference = squeeze(reference);
        if reference.is_empty() {
            return Ok(json!({"ok": false, "reason": "nothing to read"}));
        }
        if let Some(m) = READ_REF.captures(&reference) {
            let day = m.get(1).map_or("", |g| g.as_str());
            let start = m.get(2).map_or("", |g| g.as_str());
            let end = m.get(3).map_or("", |g| g.as_str());
            let text = self.transcripts.read(day, start, end, READ_MAX_CHARS);
            if text.is_empty() {
                return Ok(json!({"ok": false, "reason": "nothing was said then"}));
            }
            return Ok(json!({"ok": true, "day": day, "text": text}));
        }
        self.records.get(&self.cfg, &reference)
    }

    /// The other channel since `since`, newest kept within RECENT_CHARS.
    pub fn recent_conversation(&self, since: Option<Dt>, channel: &str) -> Value {
        let Some(since) = since else {
            return json!({"ok": false, "reason": "no conversation is open"});
        };
        let rows: Vec<String> = self
            .transcripts
            .lines_since(since, channel)
            .iter()
            .map(|ln| render(std::slice::from_ref(ln), "voice"))
            .collect();
        let mut kept: Vec<&str> = Vec::new();
        let mut used = 0usize;
        for r in rows.iter().rev() {
            let n = tpy::len(r);
            if used + n + 1 > RECENT_CHARS {
                break;
            }
            kept.push(r);
            used += n + 1;
        }
        kept.reverse();
        if kept.is_empty() {
            return json!({"ok": true, "text": "", "note": "nothing on the other channel since this conversation began"});
        }
        json!({"ok": true, "text": kept.join("\n"), "omitted": rows.len() - kept.len()})
    }

    // -- forget ------------------------------------------------------------------------------------

    /// (the search index a forget must clean, whether it was opened just for this). The live one when the
    /// hybrid switch is on; with it off, the file left from when it was on, opened without an embedder, so its
    /// words are counted and forgotten too. (None, false) when there is no file.
    fn ranked_for_forget(&self) -> (Option<Arc<MemoryIndex>>, bool) {
        if let Some(r) = &self.ranked {
            return (Some(r.clone()), false);
        }
        if !mi::index_path(&self.cfg.store).exists() {
            return (None, false);
        }
        let source: Arc<dyn DaySource> = self.transcripts.clone();
        let idx = mi::open_index(&self.cfg.store, source, None, false);
        let opened = idx.is_some();
        (idx, opened)
    }

    fn count(&self, query: &str, pred: &dyn Fn(&str) -> bool) -> IndexMap<&'static str, usize> {
        let mut layers: IndexMap<&'static str, usize> = LAYERS.iter().map(|k| (*k, 0)).collect();
        layers.insert("transcripts", self.transcripts.redact(query, true));
        match self.records.forget_lines(&self.cfg, pred, true) {
            Ok(n) => {
                layers.insert("records", n);
            }
            Err(e) => warn!("memory: forget count failed for records ({})", error_name(&e)),
        }
        if let Some(index) = &self.index {
            match index.find(pred) {
                Ok(rows) => {
                    layers.insert("index", rows.len());
                }
                Err(e) => warn!("memory: forget count failed for the index ({})", error_name(&e)),
            }
        }
        let (ranked, opened) = self.ranked_for_forget();
        if let Some(ranked) = ranked {
            layers["index"] += ranked.count(pred);
            if opened {
                ranked.close();
            }
        }
        let dry = |folder: &Path| forget_markdown(folder, pred, true).unwrap_or_else(|e| e.removed);
        layers.insert("archive", dry(&self.cfg.archive_dir()));
        layers.insert("meetings", dry(&self.cfg.transcripts_dir().join(MEETINGS_SUBDIR)));
        if let Some(log) = self.chat_log() {
            let n = log.count(pred).unwrap_or_else(|e| {
                warn!("memory: forget count failed for the chat log ({})", error_name(&e));
                0
            });
            layers.insert("chat_log", n);
        }
        layers
    }

    /// The places this forget does not reach. The Telegram chat log is one of them while none is attached: a
    /// forget never claims "everywhere" while its words stay on disk (review, 2026-10-08).
    fn not_covered(&self) -> Vec<&'static str> {
        let mut out = NOT_COVERED.to_vec();
        if self.chat_log().is_none() {
            out.push(CHAT_LOG_NOT_COVERED);
        }
        out
    }

    /// `pred`, widened to every record fact and index memory whose src points at a transcript line `query`
    /// matches, so a fact the dream wrote in other words goes with the line it came from. Whatever
    /// EVE_MEMORY_SOURCES says now: a src written while it was on is still followed after it is turned off.
    /// Also widened to a line holding most of the query's words in other forms (`reworded`). Call before the
    /// transcripts are redacted.
    fn follow_sources(&self, query: &str, pred: Pred) -> Pred {
        let mut marks: HashSet<String> = HashSet::new();
        for ln in self.transcripts.find(query) {
            let ts = match ln.get("ts") {
                Some(Value::String(s)) => s.clone(),
                Some(other) => str_of(other),
                None => "None".into(),
            };
            if let Some((naive, _)) = tpy::fromisoformat(&ts) {
                marks.insert(format!("src: {}", naive.format("%Y-%m-%d %H:%M")));
            }
        }
        let pred: Pred = match reworded(query) {
            Some(alt) => Box::new(move |text: &str| pred(text) || alt(text)),
            None => pred,
        };
        if marks.is_empty() {
            return pred;
        }
        Box::new(move |text: &str| pred(text) || marks.iter().any(|m| text.contains(m.as_str())))
    }

    /// Count what `query` matches in every store and mint a token. Changes nothing; shows no words.
    pub fn forget_preview(&self, query: &str) -> Value {
        let started = Instant::now();
        let query = squeeze(query);
        let Some(pred) = match_query(&query) else {
            return json!({"ok": false, "reason": "the query has no word to match"});
        };
        let pred = self.follow_sources(&query, pred);
        let layers = self.count(&query, &*pred);
        let total: usize = layers.values().sum();
        let mut out = Map::new();
        out.insert("ok".into(), Value::Bool(true));
        out.insert("total".into(), json!(total));
        out.insert("layers".into(), layers_json(&layers));
        out.insert("not_covered".into(), json!(self.not_covered()));
        if total > 0 {
            let mut raw = [0u8; 12];
            rand::rng().fill_bytes(&mut raw);
            let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
            let now = (self.clock)();
            {
                let mut pending = self.pending.lock();
                pending.retain(|_, p| p.expires > now);
                while pending.len() >= MAX_PENDING_TOKENS {
                    pending.shift_remove_index(0);
                }
                pending.insert(token.clone(), Pending { query: query.clone(), expires: now + FORGET_TTL_SECS });
            }
            out.insert("token".into(), json!(token));
            out.insert("expires_in".into(), json!(FORGET_TTL_SECS as i64));
        } else {
            out.insert("token".into(), json!(""));
            out.insert("expires_in".into(), json!(0));
            out.insert("note".into(), json!("nothing in memory matches"));
        }
        info!("memory: forget preview → {} matches in {} ms", total, started.elapsed().as_millis());
        Value::Object(out)
    }

    /// Forget what the token's preview counted, in every store. Single use.
    pub fn forget_apply(&self, token: &str) -> Value {
        let started = Instant::now();
        let pending = self.pending.lock().shift_remove(token);
        let pending = match pending {
            Some(p) if p.expires > (self.clock)() => p,
            _ => return json!({"ok": false, "reason": "the token is unknown, used or expired; preview again"}),
        };
        let Some(pred) = match_query(&pending.query) else {
            // a preview refused such a query
            return json!({"ok": false, "reason": "the query has no word to match"});
        };
        let pred = self.follow_sources(&pending.query, pred); // before the redact: it reads the lines' times
        let mut done: IndexMap<&'static str, usize> = LAYERS.iter().map(|k| (*k, 0)).collect();
        let mut failed: Vec<&'static str> = Vec::new(); // layers a step failed in: the words may still be there
        done.insert("transcripts", self.transcripts.redact(&pending.query, false));
        match self.records.forget_lines(&self.cfg, &*pred, false) {
            Ok(n) => {
                done.insert("records", n);
            }
            Err(e) => {
                failed.push("records");
                warn!("memory: forget failed in records ({})", error_name(&e));
            }
        }
        if let Some(index) = &self.index {
            // mem0: find and delete in one hold of its lock, when it can
            let got = index.forget_matching(&*pred).unwrap_or_else(|| {
                let ids: Vec<String> = index
                    .find(&*pred)?
                    .iter()
                    .filter(|r| r.is_object() && r.get("id").is_some_and(truthy))
                    .map(|r| str_or_empty(r.get("id")))
                    .collect();
                if ids.is_empty() {
                    Ok(0)
                } else {
                    index.forget(&ids)
                }
            });
            match got {
                Ok(n) => {
                    done.insert("index", n);
                }
                Err(e) => {
                    failed.push("index");
                    warn!("memory: forget failed in the index ({})", error_name(&e));
                }
            }
        }
        // the search index, switch on or off: rounds, FTS, vectors
        let (ranked, opened) = self.ranked_for_forget();
        if let Some(ranked) = ranked {
            match ranked.forget(&*pred) {
                Ok(n) => done["index"] += n,
                Err(e) => {
                    // IndexForgetError: the words may remain; say so
                    if !failed.contains(&"index") {
                        failed.push("index");
                    }
                    warn!("memory: forget failed in the search index (IndexForgetError: {})", e.0);
                }
            }
            if opened {
                ranked.close();
            }
        }
        for (layer, folder) in [
            ("archive", self.cfg.archive_dir()),
            ("meetings", self.cfg.transcripts_dir().join(MEETINGS_SUBDIR)),
        ] {
            match forget_markdown_with(&folder, &*pred, false, &*self.rewrite) {
                Ok(n) => {
                    done.insert(layer, n);
                }
                Err(e) => {
                    failed.push(layer);
                    done.insert(layer, e.removed);
                }
            }
        }
        if let Some(log) = self.chat_log() {
            let n = log.forget(&*pred).unwrap_or_else(|e| {
                failed.push("chat_log");
                warn!("memory: forget failed in the chat log ({})", error_name(&e));
                0
            });
            done.insert("chat_log", n);
        }
        let total: usize = done.values().sum();
        let mut squashed: Vec<&str> = Vec::new();
        // History held the words too: the records repo committed every change, and the archive repo still
        // holds the old records and notes in its commits. Both become one commit of what is left.
        if total > 0 {
            for (name, repo) in [("records", self.cfg.records_dir()), ("archive", self.cfg.archive_dir())] {
                if repo.join(".git").is_dir() {
                    match self.records.squash_history(&repo) {
                        Ok(true) => squashed.push(name),
                        Ok(false) => {}
                        Err(e) => warn!("memory: history squash failed for {} ({})", name, error_name(&e)),
                    }
                }
            }
        }
        info!(
            "memory: forgot {} lines ({}) in {} ms{}",
            total,
            done.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", "),
            started.elapsed().as_millis(),
            if failed.is_empty() { String::new() } else { format!("; failed in {}", failed.join(", ")) }
        );
        let mut out = Map::new();
        out.insert("ok".into(), Value::Bool(failed.is_empty()));
        out.insert("total".into(), json!(total));
        out.insert("layers".into(), layers_json(&done));
        out.insert("history_squashed".into(), json!(squashed));
        out.insert("not_covered".into(), json!(self.not_covered()));
        if !failed.is_empty() {
            out.insert("failed".into(), json!(failed));
            out.insert(
                "reason".into(),
                json!(format!(
                    "the forget did not finish in: {}; the words may still be there. Preview and forget again.",
                    failed.join(", ")
                )),
            );
        }
        Value::Object(out)
    }
}

fn layers_json(layers: &IndexMap<&'static str, usize>) -> Value {
    Value::Object(layers.iter().map(|(k, v)| (k.to_string(), json!(v))).collect())
}

/// `Memory._recalled`: a recalled row as "(src or day, channel) text"; src is the line's day and time.
fn recalled_line(row: &Value) -> String {
    if !row.is_object() {
        return str_of(row);
    }
    let day = str_or_empty(row.get("day"));
    let ch = str_or_empty(row.get("channel"));
    let text = str_or_empty(row.get("text"));
    let src = str_or_empty(row.get("src"));
    let first = if src.is_empty() { day } else { src };
    let tag = [first, ch].into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(", ");
    if tag.is_empty() {
        text
    } else {
        format!("({tag}) {text}")
    }
}

// ---- the one-time move -------------------------------------------------------------------------

/// `_file_count`: a file or a symlink is one; a folder is every file os.walk lists under it (a symlink to a
/// folder is neither counted nor followed).
fn file_count(path: &Path) -> usize {
    if is_symlink(path) || path.is_file() {
        return 1;
    }
    fn walk(dir: &Path) -> usize {
        let Ok(rd) = fs::read_dir(dir) else {
            return 0;
        };
        let mut n = 0;
        for entry in rd.flatten() {
            let p = entry.path();
            if is_dir_following(&p) {
                if !is_symlink(&p) {
                    n += walk(&p);
                }
            } else {
                n += 1;
            }
        }
        n
    }
    walk(path)
}

/// `os.path.realpath`: symlinks resolved where the path exists, the rest as written.
fn realpath(p: &Path) -> PathBuf {
    tpy::resolve(p).unwrap_or_else(|_| p.to_path_buf())
}

fn same(a: &Path, b: &Path) -> bool {
    realpath(a) == realpath(b)
}

fn private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
    Ok(())
}

fn lstrip_chars<'a>(s: &'a str, chars: &[char]) -> &'a str {
    s.trim_start_matches(|c| chars.contains(&c))
}

/// A `YYYY-MM-DD` read as `strptime("%Y-%m-%d")` reads it: ASCII digits only (the pattern that found it takes any
/// decimal digit, strptime does not), year 1 to 9999.
fn strptime_day(s: &str) -> Option<NaiveDateTime> {
    if !s.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        return None;
    }
    let parts: Vec<&str> = s.split('-').collect();
    let [y, m, d] = parts[..] else {
        return None;
    };
    let (y, m, d) = (tpy::int(y)?, tpy::int(m)?, tpy::int(d)?);
    if y < 1 {
        return None;
    }
    NaiveDate::from_ymd_opt(i32::try_from(y).ok()?, u32::try_from(m).ok()?, u32::try_from(d).ok()?)?
        .and_hms_opt(0, 0, 0)
}

/// ★ lines from eve's own "From talking" section of the old HIGHLIGHTS.md, never the seeded example.
pub fn owner_stars(text: &str) -> Vec<(String, Option<NaiveDateTime>)> {
    let mut out = Vec::new();
    let mut inside = false;
    for row in tpy::splitlines(text) {
        let s = tpy::strip(row);
        if s.starts_with('#') {
            inside = lstrip_chars(s, &['#', ' ']).to_lowercase().starts_with(OWN_SECTION);
            continue;
        }
        if !inside || s.contains(TEMPLATE_BANNER) {
            continue;
        }
        let line = tpy::strip(lstrip_chars(s, &['-', '*', ' ']));
        let Some(rest) = line.strip_prefix('★') else {
            continue;
        };
        let mut claim = tpy::strip(rest).to_string();
        if claim.is_empty() || claim.contains(EXAMPLE_MARK) || claim.to_lowercase().starts_with("(candidate)") {
            continue;
        }
        let mut when = None;
        if let Some(m) = STAR_DATE.captures(&claim) {
            let whole = m.get(0).map_or(claim.len(), |g| g.start());
            let date = m.get(1).map_or("", |g| g.as_str()).to_string();
            claim = tpy::strip(&claim[..whole]).to_string();
            when = strptime_day(&date);
        }
        if !claim.is_empty() {
            out.push((claim, when));
        }
    }
    out
}

/// Move the old debrief store and the old mem0 folder under the new memory root, once. See [`migrate_with`].
pub fn migrate(cfg: &RecallConfig, legacy_store: &Path, legacy_mem0: &Path) -> io::Result<Value> {
    migrate_with(cfg, legacy_store, legacy_mem0, &EveRecords, &|a: &Path, b: &Path| fs::rename(a, b))
}

/// Move the old debrief store and the old mem0 folder under the new memory root, once.
///
/// legacy/records → records/; the owner's ★ lines in HIGHLIGHTS "From talking" → records/starred.md
/// (through records.star, so the format is records'); legacy_mem0 → mem0/; legacy/notes/<date> →
/// transcripts/meetings/<date>; everything else in legacy (sessions, day files, INDEX, HIGHLIGHTS, .git)
/// → archive/, and the archive's repository is sealed. Renames only: nothing is copied or deleted, and a
/// rename across volumes is refused rather than turned into copy-and-delete. A target that exists is
/// skipped and counted. Idempotent: `<root>/.migrated` holds the counts, and while it exists this
/// returns them without touching anything. When a move failed, no marker is written, so the next start
/// tries again (a moved source is simply no longer there). An error out of this is one Python raises too: the
/// memory root or an old folder that cannot be made or listed.
pub fn migrate_with(
    cfg: &RecallConfig,
    legacy_store: &Path,
    legacy_mem0: &Path,
    records: &dyn RecordsStore,
    rename: &dyn Fn(&Path, &Path) -> io::Result<()>,
) -> io::Result<Value> {
    let root = tpy::expand_user(&cfg.store);
    let marker = root.join(MIGRATED_FILE);
    if marker.exists() {
        let prior = fs::read(&marker)
            .ok()
            .and_then(|raw| String::from_utf8(raw).ok())
            .and_then(|s| tpy::loads(&s));
        let mut out = Map::new();
        out.insert("ok".into(), Value::Bool(true));
        out.insert("already".into(), Value::Bool(true));
        if let Some(Value::Object(prior)) = prior {
            for (k, v) in prior {
                out.insert(k, v);
            }
        }
        return Ok(Value::Object(out));
    }
    let legacy = tpy::expand_user(legacy_store);
    let old_mem0 = tpy::expand_user(legacy_mem0);
    let mut counts: IndexMap<&'static str, usize> =
        ["records", "stars", "mem0", "meetings", "archive", "skipped", "failed"].iter().map(|k| (*k, 0)).collect();
    let with_counts = |head: Vec<(&str, Value)>, counts: &IndexMap<&'static str, usize>| -> Value {
        let mut out = Map::new();
        for (k, v) in head {
            out.insert(k.to_string(), v);
        }
        for (k, v) in counts {
            out.insert(k.to_string(), json!(v));
        }
        Value::Object(out)
    };
    let (lr, rr) = (realpath(&legacy), realpath(&root));
    let (lb, rb) = (lr.as_os_str().as_bytes(), rr.as_os_str().as_bytes());
    let under = |a: &[u8], b: &[u8]| a.len() > b.len() && a.starts_with(b) && a[b.len()] == b'/';
    if lb == rb || under(rb, lb) || under(lb, rb) {
        warn!("memory: the old store and the new memory folder overlap — nothing moved");
        return Ok(with_counts(
            vec![("ok", json!(false)), ("reason", json!("the old store and the memory folder overlap"))],
            &counts,
        ));
    }
    let started = Instant::now();
    private_dir(&root)?;

    let mv = |src: &Path, dst: &Path, key: &'static str, counts: &mut IndexMap<&'static str, usize>| -> bool {
        if !lexists(src) {
            return false;
        }
        if lexists(dst) {
            counts["skipped"] += 1;
            warn!("memory: migration skipped one {} entry: its target already exists", key);
            return false;
        }
        let n = file_count(src);
        let moved = (|| -> io::Result<()> {
            private_dir(dst.parent().unwrap_or(Path::new("/")))?;
            rename(src, dst)
        })();
        if let Err(e) = moved {
            counts["failed"] += 1;
            warn!("memory: migration could not move one {} entry ({})", key, tpy::os_error_name(&e));
            return false;
        }
        counts[key] += n;
        true
    };

    let have_legacy = legacy.is_dir() && !is_symlink(&legacy);
    if is_symlink(&legacy) {
        warn!("memory: the old store is a symlink — it is left where it is");
    }
    let mut hold: HashSet<OsString> = HashSet::new(); // entries that must stay put this run, for the retry
    if have_legacy {
        mv(&legacy.join("records"), &cfg.records_dir(), "records", &mut counts);
        let highlights = legacy.join("HIGHLIGHTS.md");
        if highlights.is_file() {
            let stars = fs::read(&highlights).map(|raw| owner_stars(&tpy::read_text(&raw))).unwrap_or_default();
            for (claim, when) in stars {
                match records.star(cfg, &claim, when) {
                    Ok(Some(_)) => counts["stars"] += 1,
                    Ok(None) => {}
                    Err(e) => {
                        counts["failed"] += 1;
                        hold.insert(OsString::from("HIGHLIGHTS.md")); // the next start reads the stars from here again
                        warn!("memory: migration could not carry a star ({})", error_name(&e));
                    }
                }
            }
        }
    }
    if old_mem0.is_dir() && !is_symlink(&old_mem0) && !same(&old_mem0, &cfg.mem0_dir()) {
        mv(&old_mem0, &cfg.mem0_dir(), "mem0", &mut counts);
    }
    if have_legacy {
        let notes = legacy.join("notes");
        if notes.is_dir() && !is_symlink(&notes) {
            let meetings = cfg.transcripts_dir().join(MEETINGS_SUBDIR);
            for entry in sorted_names(&notes)? {
                let is_day = entry.to_str().is_some_and(|s| DAY.is_match(s));
                if is_day && notes.join(&entry).is_dir() {
                    mv(&notes.join(&entry), &meetings.join(&entry), "meetings", &mut counts);
                }
            }
        }
        for entry in sorted_names(&legacy)? {
            if !hold.contains(&entry) {
                mv(&legacy.join(&entry), &cfg.archive_dir().join(&entry), "archive", &mut counts);
            }
        }
    }
    if cfg.archive_dir().join(".git").is_dir() {
        if let Err(e) = records.seal(&cfg.archive_dir()) {
            warn!("memory: could not seal the archive's repository ({})", error_name(&e));
        }
    }
    let ms = started.elapsed().as_millis();
    info!(
        "memory: migration moved {} record files, {} stars, {} mem0 files, {} meeting files, {} archive files; \
{} skipped, {} failed, in {} ms",
        counts["records"],
        counts["stars"],
        counts["mem0"],
        counts["meetings"],
        counts["archive"],
        counts["skipped"],
        counts["failed"],
        ms
    );
    if counts["failed"] == 0 {
        let mut body = Map::new();
        for (k, v) in &counts {
            body.insert(k.to_string(), json!(v));
        }
        body.insert(
            "at".into(),
            json!(chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%:z").to_string()),
        );
        let written = (|| -> io::Result<()> {
            let mut f = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&marker)?;
            f.write_all(format!("{}\n", tpy::dumps(&Value::Object(body))).as_bytes())
        })();
        if let Err(e) = written {
            warn!("memory: could not write the migration marker ({})", tpy::os_error_name(&e));
        }
    }
    let ok = counts["failed"] == 0;
    Ok(with_counts(vec![("ok", json!(ok)), ("already", json!(false))], &counts))
}

/// `sorted(os.listdir(folder))`: names in code-point (byte) order.
fn sorted_names(folder: &Path) -> io::Result<Vec<OsString>> {
    let mut names: Vec<OsString> = fs::read_dir(folder)?.map(|e| e.map(|e| e.file_name())).collect::<io::Result<_>>()?;
    names.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    Ok(names)
}


#[cfg(test)]
mod tests {
    use super::*;

    fn sorted_keys(text: &str) -> Vec<String> {
        let mut k: Vec<String> = keys(text).into_iter().collect();
        k.sort();
        k
    }

    #[test]
    fn test_keys_and_reworded_read_words_as_python_does() {
        // expected values from the Python (_keys, reworded) on the same inputs
        let lines = ["strasse meissen oel cafe 12", "the gym on fifth avenue", "financial files leaked", "x"];
        let cases: [(&str, &[&str], Option<[bool; 4]>); 4] = [
            ("Stra\u{df}e 12 Mei\u{df}en \u{d6}l caf\u{e9}", &["12", "caf", "meis", "stra"], Some([true, false, false, false])),
            ("I don't wanna go to the GYM on 5th Ave 2026", &["2026", "5th", "ave", "dont", "gym", "wann"],
             Some([false, false, false, false])),
            ("\u{fb01}nancial \u{fb01}les were LEAKED yesterday", &["file", "fina", "leak", "yest"], Some([false, false, true, false])),
            ("a an the", &[], None),
        ];
        for (q, want, preds) in cases {
            assert_eq!(sorted_keys(q), want, "{q}");
            let got = reworded(q).map(|p| lines.map(&*p));
            assert_eq!(got, preds, "{q}");
        }
    }

    #[test]
    fn test_frontmatter_splits_as_python_does() {
        assert_eq!(split_frontmatter("---\n---\nbody"), ("---\n---\n", "body"));
        assert_eq!(split_frontmatter("---\nid: x\n---  \t"), ("---\nid: x\n---  \t", ""));
        assert_eq!(split_frontmatter("---\nid\n--- x\nrest"), ("", "---\nid\n--- x\nrest"));
        assert_eq!(split_frontmatter("no fence"), ("", "no fence"));
    }

    #[test]
    fn test_owner_stars_cut_a_date_strptime_cannot_read() {
        let text = "## From Talking\n* \u{2605} Likes rain (2026-13-40)\n- \u{2605}   Plain star  \n###   from talking more\n\
-\u{2605}x (\u{662}\u{660}\u{662}\u{666}-\u{660}\u{669}-\u{661}\u{661})\n";
        let got = owner_stars(text);
        let want: Vec<(String, Option<NaiveDateTime>)> =
            vec![("Likes rain".into(), None), ("Plain star".into(), None), ("x".into(), None)];
        assert_eq!(got, want);
    }

    #[test]
    fn test_tool_arguments_are_read_as_python_int_reads_them() {
        assert_eq!(py_int(&json!(true)), Some(1));
        assert_eq!(py_int(&json!(7)), Some(7));
        assert_eq!(py_int(&json!(-2.9)), Some(-2));
        assert_eq!(py_int(&json!(" +1_0 ")), Some(10));
        assert_eq!(py_int(&json!("\u{663}")), Some(3));
        for bad in [json!("1__0"), json!("_1"), json!("1.5"), json!(""), json!([1]), json!({"a": 1})] {
            assert_eq!(py_int(&bad), None, "{bad}");
        }
        assert_eq!(squeeze("  a\t\u{1c}b \u{3000} c "), "a b c");
    }
}
