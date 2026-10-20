//! Port of `memory_index.py` and `transcripts.py`: the local per-day transcript store (guards, day boundary, prompt
//! blocks, search, forget) and the ranked index over it with the three memory-search switches.

pub mod memory_index;
pub mod py;
pub mod transcripts;

pub use memory_index::{
    fts_query, index_path, open_index, openai_embedder, owner_day, resolve_args, resolve_when,
    switch, terms, unit_vec, units_of, widen, DaySource, Embedder, IndexForgetError, IndexStats,
    MemoryIndex, Tuning, Unit,
};
pub use transcripts::{
    configured, match_query, parse_line, parse_ts, prepare, refusal, render, render_line, speaker,
    Append, ConvInfo, Dt, Line, Ranker, Search, Stamp, Today, TranscriptConfig, TranscriptStats,
    Transcripts, FORGOTTEN, OUTSIDE_LEFT_OUT, TALK,
};

#[cfg(test)]
mod memory_index_tests;
#[cfg(test)]
mod transcripts_tests;
