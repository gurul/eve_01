//! Port of `memory.py`: the one door both brains hold onto memory — the facade over the transcripts, the
//! records and the search indexes, the one memory tool set, the two-step forget across every store, and the
//! one-time move of the old debrief store under the memory root.

pub mod memory;

pub use memory::{
    atomic_rewrite, enabled, forget_markdown, forget_markdown_with, memory_search_tool, migrate, migrate_with,
    monotonic, owner_stars, reworded, ChatLogLayer, Clock, EveRecords, MarkdownForgetError, MeaningIndex, Memory,
    MemoryOptions, RecordsStore, Rewriter, FORGET_APPLY, FORGET_PREVIEW, MEMORY_READ, MEMORY_SEARCH, MIGRATED_FILE,
    RECENT_CHARS, RECENT_CONVERSATION, TOOLS, VOICE_TOOLS,
};

#[cfg(test)]
mod testutil;
#[cfg(test)]
mod memory_tests;
#[cfg(test)]
mod search_tests;
#[cfg(test)]
mod records_tests;
