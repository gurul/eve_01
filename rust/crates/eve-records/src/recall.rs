//! What eve carries from one conversation into the next: where memory lives, and the opening brief.
//!
//! eve already remembers what it SEES: diary keeps a memory stream of looks. It remembered nothing of what was
//! SAID, and every conversation opened from nothing (owner request 2026-09-11). This module is the cheapest part
//! of the fix: the place memory lives on disk, and the one clause a conversation opens with.
//!
//! ### Where the memory lives
//!
//! One folder, `~/.config/eve-bridge/memory` (`EVE_MEMORY_DIR` overrides). The owner asked for fewer stores
//! (owner, 2026-09-23: "simplify the memory system — too many stores"), so there are three and an archive, each a
//! method of [`RecallConfig`]:
//!
//! ```text
//! transcripts/   every word, written the moment it is said (transcripts): the source
//! records/       what is true about the owner, rewritten nightly by the dream (records, dream)
//! mem0/          a search by meaning, rebuilt from the transcripts (mem0_memory)
//! archive/       the retired debrief store, moved here once (memory.migrate), read-only, still searched
//! ```
//!
//! The retired store's own location is still read, once, as the source of that move: `EVE_DEBRIEF_DIR` or
//! `~/.config/eve-bridge/debrief` ([`legacy_store`]), and the old mem0 home `~/.config/eve-bridge/mem0`
//! ([`legacy_mem0`]).
//!
//! ### Why the opening brief does not search
//!
//! A conversation opens on a recency question, and similarity has no clock (measured 2026-09-11: asked "what did
//! we talk about yesterday", a semantic search returned a note about opening lines). So the brief reads the newest
//! things only: when the transcripts last saw a word said, and, when that was before today, the title of the
//! dream's journal for that day. Search is left to the `memory_search` tool, mid-conversation.
//!
//! ### The two rules that keep it from being creepy
//!
//! 1. **At most one clause.** [`opening_brief`] returns one short sentence or nothing. A robot that recites a
//!    dossier at you is worse company than one that forgot.
//! 2. **What eve said is quotable; what eve saw is not.** This module reads only the spoken memory.
//!
//! Silence is a valid answer, and it is the right one with memory off and on the first ever conversation: the
//! brief is "", the instructions are byte-identical to a memory-less session, and eve greets exactly as it always
//! did. It never announces that it has no memories.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};
use eve_transcripts::py;
use eve_transcripts::{Dt, Transcripts};

use crate::records;

pub const DEFAULT_STORE: &str = "~/.config/eve-bridge/memory";
pub const DEFAULT_NOTES: &str = "~/.config/eve-bridge/notes";
/// The retired debrief store: read once, by the migration.
pub const LEGACY_STORE: &str = "~/.config/eve-bridge/debrief";
/// The retired mem0 home: read once, by the migration.
pub const LEGACY_MEM0: &str = "~/.config/eve-bridge/mem0";
pub const MAX_BRIEF_CHARS: usize = 240;
/// A talk this recent is the same conversation to a person.
pub const RECENT_SECS: i64 = 900;
/// A journal title forget redacted: never quoted.
pub const FORGOTTEN: &str = "(forgotten)";

/// The memory root and the notes folder.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RecallConfig {
    pub store: PathBuf,
    pub notes: PathBuf,
}

impl RecallConfig {
    pub fn new(store: impl Into<PathBuf>, notes: impl Into<PathBuf>) -> Self {
        Self { store: store.into(), notes: notes.into() }
    }

    // The simplified memory (owner, 2026-09-23): three stores and an archive under one root.
    pub fn transcripts_dir(&self) -> PathBuf {
        self.store.join("transcripts")
    }

    pub fn records_dir(&self) -> PathBuf {
        self.store.join("records")
    }

    pub fn mem0_dir(&self) -> PathBuf {
        self.store.join("mem0")
    }

    pub fn archive_dir(&self) -> PathBuf {
        self.store.join("archive")
    }
}

fn env_or_process(environ: Option<&HashMap<String, String>>) -> HashMap<String, String> {
    match environ {
        Some(e) => e.clone(),
        None => crate::py::environ(),
    }
}

/// `(env.get(name) or "").strip() or default`, expanded as `Path.expanduser` does.
fn path_from(env: &HashMap<String, String>, name: &str, default: &str) -> PathBuf {
    let raw = env.get(name).map(|s| py::strip(s)).unwrap_or("");
    let raw = if raw.is_empty() { default } else { raw };
    py::expand_user(Path::new(raw))
}

/// The memory root (`EVE_MEMORY_DIR`) and the notes folder (`EVE_NOTES_DIR`). `None` reads the process
/// environment.
pub fn configured(environ: Option<&HashMap<String, String>>) -> RecallConfig {
    let env = env_or_process(environ);
    RecallConfig {
        store: path_from(&env, "EVE_MEMORY_DIR", DEFAULT_STORE),
        notes: path_from(&env, "EVE_NOTES_DIR", DEFAULT_NOTES),
    }
}

/// Where the retired debrief store is: the source `memory.migrate` moves into the archive.
pub fn legacy_store(environ: Option<&HashMap<String, String>>) -> PathBuf {
    path_from(&env_or_process(environ), "EVE_DEBRIEF_DIR", LEGACY_STORE)
}

/// Where the retired mem0 index lived, before it moved under the memory root.
pub fn legacy_mem0() -> PathBuf {
    py::expand_user(Path::new(LEGACY_MEM0))
}

// ---- the brief ---------------------------------------------------------------------------------

/// What the brief reads of the transcripts: whether memory is on, when anything was last said, and which memory
/// day a moment belongs to. [`Transcripts`] is the real one; a test can be a broken store.
pub trait BriefSource {
    fn enabled(&self) -> bool;
    fn last_turn_at(&self) -> anyhow::Result<Option<Dt>>;
    fn day_of(&self, when: Dt) -> anyhow::Result<String>;
}

impl BriefSource for Transcripts {
    fn enabled(&self) -> bool {
        self.enabled
    }

    fn last_turn_at(&self) -> anyhow::Result<Option<Dt>> {
        Ok(Transcripts::last_turn_at(self))
    }

    fn day_of(&self, when: Dt) -> anyhow::Result<String> {
        Ok(Transcripts::day_of(self, when))
    }
}

/// An aware local datetime (Python's `when.astimezone()`).
fn local(when: Dt) -> DateTime<Local> {
    when.with_timezone(&Local)
}

/// "earlier today" / "yesterday" / "N days ago", or "" when unknowable or too recent to mention.
///
/// A clock that has gone backwards is the same answer as no clock: say nothing about time.
pub fn gap_clause(then: DateTime<Local>, now: DateTime<Local>) -> String {
    let span = now - then;
    if span < chrono::Duration::zero() {
        return String::new();
    }
    let days = (now.date_naive() - then.date_naive()).num_days();
    if days <= 0 {
        // secs < RECENT_SECS, compared as total_seconds() (a float) does
        return if span < chrono::Duration::seconds(RECENT_SECS) { String::new() } else { "earlier today".into() };
    }
    if days == 1 {
        return "yesterday".into();
    }
    if days <= 6 {
        return format!("{days} days ago");
    }
    if days <= 13 {
        return "last week".into();
    }
    "a while ago".into()
}

/// The dream journal's title for `day`, or "" when that day has no journal (yet) or it was forgotten.
pub fn journal_title(cfg: &RecallConfig, day: &str) -> String {
    let Some((latest, title)) = records::latest_day(cfg) else {
        return String::new();
    };
    if latest != day {
        return String::new();
    }
    let title = py::flat(&title);
    let title = title.trim_end_matches('.');
    if title.is_empty() || title == day || title.contains(FORGOTTEN) || title.ends_with('?') {
        return String::new();
    }
    title.to_string()
}

/// One short sentence for the top of a conversation, or "".
///
/// The gap comes first because it is what a person notices. When the last talk was before today and the dream has
/// written that day's journal, its title rides along: "You last talked yesterday, about the trip." Never a
/// question, never longer than MAX_BRIEF_CHARS. "" when memory is off (no transcripts).
pub fn opening_brief(cfg: &RecallConfig, transcripts: Option<&dyn BriefSource>, now: Option<Dt>) -> String {
    let Some(t) = transcripts else {
        return String::new();
    };
    if !t.enabled() {
        return String::new();
    }
    let parts = (|| -> anyhow::Result<Option<(String, String)>> {
        let Some(last) = t.last_turn_at()? else {
            return Ok(None);
        };
        let then = local(last);
        let when = local(now.unwrap_or_else(py::now_local));
        let gap = gap_clause(then, when);
        if gap.is_empty() {
            return Ok(None);
        }
        let mut title = String::new();
        if then.date_naive() < when.date_naive() {
            title = journal_title(cfg, &t.day_of(then.fixed_offset())?);
        }
        Ok(Some((gap, title)))
    })();
    let (gap, title) = match parts {
        Ok(Some(p)) => p,
        Ok(None) => return String::new(),
        Err(e) => {
            // a brief is a nicety: never a failed conversation
            tracing::debug!("recall: brief unavailable ({})", crate::py::error_name(&e));
            return String::new();
        }
    };
    let mut brief =
        if title.is_empty() { format!("You last talked {gap}.") } else { format!("You last talked {gap}, about: {title}.") };
    if py::len(&brief) > MAX_BRIEF_CHARS {
        let cut = py::head(&brief, MAX_BRIEF_CHARS - 1);
        let word = match cut.rfind(' ') {
            Some(i) => &cut[..i],
            None => cut,
        };
        brief = format!("{}.", word.trim_end_matches([',', ';', ':']));
    }
    brief
}
