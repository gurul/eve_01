//! Port of `mem0_memory.py`: the local mem0 index fed each day's conversations natively, searched dated (3 s at
//! most), tidied, and forgettable down to mem0's own history rows. mem0 itself is a Python library, so the opened
//! Memory runs in a helper Python process (`python`, `src/mem0_helper.py`); see PARITY.md, Boundaries.

pub mod mem0_memory;
pub mod python;

pub use mem0_memory::{
    configured, embedder_of, error_name, make_private, mem0_config, open_mem0, shared, shared_with, Clock,
    Conversations, Factory, Found, Mem0Backend, Mem0Config, Mem0Error, Mem0ForgetError, Message, OwnerMemory, Unit,
    CHUNK_MESSAGES, CUSTOM_INSTRUCTIONS, DEFAULT_EMBED_MODEL, DEFAULT_MODEL, EMBEDDERS, EMBED_DIMS,
    EXTRACT_MAX_OUTPUT_TOKENS, GEMMA_DIMS, GEMMA_DOCUMENT, GEMMA_HOME, GEMMA_MODEL, GEMMA_QUERY, GEMMA_REVISION,
    GET_ALL_CAP, HISTORY_DB, INGESTED_CONVS, MAX_HITS, MAX_SEARCHES_IN_FLIGHT, MESSAGE_CHARS, MIN_SCORE, OWNER,
    PLACEHOLDER_KEY, RETRY_AFTER_SECS, SEARCH_TIMEOUT_SECS, STAR, TIMEOUT_LOG_EVERY_SECS,
};
pub use python::{available, available_with, default_python, record_spend, PythonMem0, HELPER, PYTHON_ENV};

#[cfg(test)]
mod tests;
