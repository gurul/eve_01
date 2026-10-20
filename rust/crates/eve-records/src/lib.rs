//! Port of `recall.py` and `records.py`: where eve's memory lives and the one clause a conversation opens with, and
//! the records — typed, git-tracked files the brains read, the owner stars into and hides from, and only the dream
//! writes.

pub mod py;
pub mod recall;
pub mod records;

pub use recall::{configured, legacy_mem0, legacy_store, opening_brief, BriefSource, RecallConfig};
pub use records::{
    apply_day, consolidate, forget_lines, forgets_pinned, hidden, hidden_match, hide, latest_day, load_records,
    make_client, parse_fact, parse_record, pin_forgets, profile, profile_for_voice, reconcile_day, render_fact,
    render_record, search, squash_history, star, stars, talking_section, unhide, with_seams, DayChange, DreamClient,
    Fact, MeaningSearch, OpenAIReconcileClient, Record, Records, RecordsReader, Seams,
};

#[cfg(test)]
mod recall_tests;
#[cfg(test)]
mod records_tests;
#[cfg(test)]
mod testutil;
