//! mem0, self-hosted: the meaning search over everything the owner has said to eve.
//!
//! records.py answers "what do you know about me" with a page and a keyword search. It misses a question asked in
//! other words than the record used ("where does my sister live" against "moved to the coast"). This is the layer
//! that catches it: mem0's open-source library (mem0ai, Apache-2.0), run on this computer, with its vector store
//! (Qdrant, on disk) and its history database both under `<memory>/mem0`. Only the two model calls leave the
//! machine: fact extraction and embeddings, both OpenAI models (owner, 2026-09-23: "it can go to openai"), both
//! through OpenRouter on the owner's key since 2026-10-08 (owner: "make everything use my openrouter key";
//! docs/openrouter.md). The embedder is the same text-embedding-3-small at 1536 dimensions it always was, so the
//! vectors already on disk stay comparable.
//!
//! It is an INDEX, not a source (owner, 2026-09-23: three stores — transcripts, records, mem0). The nightly dream
//! feeds it each finished conversation of the day, natively: what the owner said goes in as the user's messages and
//! what eve said as the assistant's, dated, with its channel, so mem0 extracts the owner's own facts rather than
//! facts about a note. Tool calls, commands and relays never go in. It can be rebuilt from the transcripts at any
//! time (`memory reindex`), which is why a failure here is never a loss.
//!
//! The brains never write it: they search it ([`OwnerMemory::search`], dated hits, 3 s at most) through the Memory
//! facade. A forget reaches it ([`OwnerMemory::find`] + [`OwnerMemory::forget`]), including the rows mem0's own
//! history database keeps.
//!
//! mem0 is a Python library and its store is in Python's own formats, so the opened Memory lives in a helper Python
//! process ([`crate::python::PythonMem0`], `src/mem0_helper.py`); everything around it — which conversations go in,
//! how, the search's cap and filters, tidy, forget and the history scrub — is here.
//!
//! Privacy, as code rather than hope:
//!
//! - mem0 ships PostHog telemetry, on by default and read at import time. `MEM0_TELEMETRY` is forced off before the
//!   first import, and the import is refused if telemetry still reads on.
//! - mem0 writes `~/.mem0/config.json` at import. `MEM0_DIR` points it inside eve's own folder.
//! - The folder is 0700 and its files 0600, set again every time it opens; nothing here is in git.
//!
//! Failure is silence: no key, no network, no mem0 installed — search returns nothing, a conversation that failed
//! to go in is tried again next night, and nothing about a conversation breaks. A failure to open is retried after
//! ten minutes, so one bad moment (a key not yet set, a lock held) does not last until restart.

use std::borrow::Borrow;
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use eve_records::py::{str_of, str_or_empty, truthy};
use eve_records::RecallConfig;
use eve_transcripts::py::{casefold, flat, fromisoformat, head, read_text, strip};
use eve_transcripts::{ConvInfo, Line, Transcripts, OUTSIDE_LEFT_OUT};
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Map, Value};
use tracing::{info, warn};

pub const DEFAULT_MODEL: &str = "gpt-6-luna";
pub const EXTRACT_MAX_OUTPUT_TOKENS: u32 = 2000;
pub const DEFAULT_EMBED_MODEL: &str = "text-embedding-3-small";
pub const EMBED_DIMS: u32 = 1536; // text-embedding-3-small
// EVE_MEM0_EMBEDDER: "openrouter" (the default, everything above) or "gemma" (an experiment, 2026-10-09:
// EmbeddingGemma on this Mac, so a memory search sends nothing out; tools/evals/embeddinggemma/VERDICT.md).
// google/embeddinggemma-300m is gated on Hugging Face (a license click per account); unsloth's copy is not, and
// its configs are byte-identical to Google's (same git blob ids) and its weights the same sizes. It is pinned to
// one revision and only ever read from the local Hugging Face cache: the daemon never downloads it. Gemma Terms
// of Use apply either way.
pub const EMBEDDERS: [&str; 2] = ["openrouter", "gemma"];
pub const GEMMA_MODEL: &str = "unsloth/embeddinggemma-300m";
pub const GEMMA_REVISION: &str = "bfa3c846ac738e62aa61806ef9112d34acb1dc5a";
pub const GEMMA_DIMS: u32 = 768; // its full width (the card: Matryoshka to 512/256/128 also)
pub const GEMMA_QUERY: &str = "query"; // its prompts: "task: search result | query: "
pub const GEMMA_DOCUMENT: &str = "document"; // and "title: none | text: "
pub const GEMMA_HOME: &str = "gemma"; // <mem0>/gemma: its own store, ledger and history; 1536 never touched
pub const OWNER: &str = "owner"; // the one user id: this is one person's memory
pub const INGESTED_CONVS: &str = "ingested_convs"; // one conversation id per line: what mem0 has read
pub const HISTORY_DB: &str = "history.db";
pub const MAX_HITS: usize = 5;
pub const MIN_SCORE: f64 = 0.25; // below this a hit is a guess, not a memory
pub const SEARCH_TIMEOUT_SECS: f64 = 3.0; // a hung embedding call must not stall a turn
pub const MAX_SEARCHES_IN_FLIGHT: usize = 2; // abandoned searches still running: past this, answer [] at once
pub const RETRY_AFTER_SECS: f64 = 600.0; // a failed open is tried again after ten minutes
pub const TIMEOUT_LOG_EVERY_SECS: f64 = 3600.0;
pub const CHUNK_MESSAGES: usize = 40; // messages per mem0.add call
pub const MESSAGE_CHARS: usize = 1500; // one message, clipped
pub const GET_ALL_CAP: usize = 10000;
pub const STAR: &str = "★"; // a candidate eve proposed, never the owner's own fact

/// mem0's extraction prompt addition (MemoryConfig.custom_instructions, verified in mem0ai 2.2.0).
pub const CUSTOM_INSTRUCTIONS: &str = concat!(
    "The user is the owner of eve, a personal sidekick on their Mac, and the assistant is eve. Extract facts about ",
    "the owner only: their life, work, people, plans, preferences and routines, as the owner said them. ",
    "A request to operate the robot or its tools (move, look, play a sound, set a timer, send a message, run ",
    "a command) is not a fact about the owner: extract nothing from it. Never extract what eve said about ",
    "itself, and never a guess eve made about the owner that the owner did not confirm."
);

/// mem0 builds its own openai clients in Memory.from_config and refuses to without a key. They are replaced at once
/// (`on_openrouter`, in the helper) and never send a request, so they get this placeholder, never a real key: no
/// key is ever in mem0's config.
pub const PLACEHOLDER_KEY: &str = "replaced-by-eve";

static DAY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d{4}-\d{2}-\d{2})").unwrap());

/// A forget that could not reach mem0 (not opened: the retry window, no key; or the listing failed). A 0 here
/// would read as "nothing to forget" while the memories stay in Qdrant, so memory.forget_apply reports the index as
/// failed instead.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Mem0ForgetError(pub String);

/// An exception raised on the mem0 side: its Python class name (what a log line names) and its message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{kind}: {message}")]
pub struct Mem0Error {
    pub kind: String,
    pub message: String,
}

impl Mem0Error {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self { kind: kind.into(), message: message.into() }
    }
}

/// The class name Python's `type(e).__name__` would log for this error.
pub fn error_name(e: &anyhow::Error) -> String {
    for cause in e.chain() {
        if let Some(m) = cause.downcast_ref::<Mem0Error>() {
            return m.kind.clone();
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            return eve_transcripts::py::os_error_name(io).to_string();
        }
        if let Some(s) = cause.downcast_ref::<rusqlite::Error>() {
            return sqlite_error_name(s).to_string();
        }
    }
    "Exception".to_string()
}

/// The `sqlite3` exception class Python raises for this error.
fn sqlite_error_name(e: &rusqlite::Error) -> &'static str {
    use rusqlite::ErrorCode;
    match e {
        rusqlite::Error::SqliteFailure(f, _) => match f.code {
            ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt => "DatabaseError",
            ErrorCode::ConstraintViolation => "IntegrityError",
            ErrorCode::TooBig => "DataError",
            ErrorCode::ApiMisuse => "InterfaceError",
            _ => "OperationalError",
        },
        rusqlite::Error::SqlInputError { .. } => "OperationalError",
        _ => "Error",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Mem0Config {
    pub enabled: bool,
    pub home: PathBuf,
    pub model: String,
    pub embed_model: String,
    pub embedder: String,
}

impl Default for Mem0Config {
    fn default() -> Self {
        Mem0Config {
            enabled: false,
            home: eve_transcripts::py::expand_user(Path::new("~/.config/eve-bridge/memory/mem0")),
            model: DEFAULT_MODEL.to_string(),
            embed_model: DEFAULT_EMBED_MODEL.to_string(),
            embedder: "openrouter".to_string(),
        }
    }
}

impl Mem0Config {
    /// `Mem0Config(enabled=..., home=...)` with every other field at its default.
    pub fn new(enabled: bool, home: impl Into<PathBuf>) -> Self {
        Mem0Config { enabled, home: home.into(), ..Default::default() }
    }
}

fn env_or_process(environ: Option<&HashMap<String, String>>) -> HashMap<String, String> {
    match environ {
        Some(e) => e.clone(),
        None => std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .collect(),
    }
}

/// `EVE_MEM0_EMBEDDER`: "gemma" for the local EmbeddingGemma, anything else (unset too) "openrouter".
pub fn embedder_of(environ: Option<&HashMap<String, String>>) -> String {
    let env = env_or_process(environ);
    let name = strip(env.get("EVE_MEM0_EMBEDDER").map(String::as_str).unwrap_or("")).to_lowercase();
    if EMBEDDERS.contains(&name.as_str()) {
        name
    } else {
        "openrouter".to_string()
    }
}

/// On with memory (`EVE_MEMORY=1`, or the older `EVE_RECORDS=1`); `EVE_MEM0=0` turns just this index off. Its home
/// is the memory root's `mem0/`; with `EVE_MEM0_EMBEDDER=gemma` it is `mem0/gemma/`, a store of its own (768
/// dimensions cannot share the 1536 one).
pub fn configured(recall_cfg: &RecallConfig, environ: Option<&HashMap<String, String>>) -> Mem0Config {
    let env = env_or_process(environ);
    let memory_on = eve_transcripts::configured(Some(&env)).enabled;
    let index_off = matches!(
        strip(env.get("EVE_MEM0").map(String::as_str).unwrap_or("")).to_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    );
    let enabled = memory_on && !index_off;
    if embedder_of(Some(&env)) == "gemma" {
        return Mem0Config {
            enabled,
            home: recall_cfg.mem0_dir().join(GEMMA_HOME),
            embed_model: GEMMA_MODEL.to_string(),
            embedder: "gemma".to_string(),
            ..Default::default()
        };
    }
    Mem0Config { enabled, home: recall_cfg.mem0_dir(), ..Default::default() }
}

/// `str(path)` of a pathlib path: repeated and trailing separators folded, as `PurePath` does.
pub(crate) fn path_str(p: &Path) -> String {
    p.components().collect::<PathBuf>().to_string_lossy().into_owned()
}

/// The Memory.from_config dict: mem0's openai provider for the two model calls (its clients are swapped for
/// OpenRouter's by the helper's `on_openrouter`), everything stored under `cfg.home`. With the gemma embedder,
/// mem0's huggingface provider loads EmbeddingGemma from the local cache (the helper's `on_gemma` adds its task
/// prompts).
pub fn mem0_config(cfg: &Mem0Config) -> Value {
    // mem0's name heuristic predates Luna: without the explicit flag it silently drops
    // reasoning_effort and sends sampling settings meant for non-reasoning models.
    let llm = json!({"provider": "openai", "config": {
        "model": cfg.model, "api_key": PLACEHOLDER_KEY, "reasoning_effort": "low",
        "is_reasoning_model": true, "max_tokens": EXTRACT_MAX_OUTPUT_TOKENS}});
    let qdrant = path_str(&cfg.home.join("qdrant"));
    let history = path_str(&cfg.home.join(HISTORY_DB));
    if cfg.embedder == "gemma" {
        return json!({
            "llm": llm,
            "embedder": {"provider": "huggingface", "config": {
                "model": GEMMA_MODEL, "embedding_dims": GEMMA_DIMS,
                // CPU, not the default MPS: on the GPU fp32 weights held 1.2 GB of unified
                // memory and a query took 151 ms; on CPU the process is 1.24 GB smaller and a
                // query takes ~20 ms (measured 2026-10-13).
                "model_kwargs": {"revision": GEMMA_REVISION, "local_files_only": true, "device": "cpu"}}},
            "vector_store": {"provider": "qdrant", "config": {
                "collection_name": "eve_gemma", "path": qdrant, "on_disk": true,
                "embedding_model_dims": GEMMA_DIMS}},
            "history_db_path": history,
            "custom_instructions": CUSTOM_INSTRUCTIONS,
        });
    }
    json!({
        "llm": llm,
        "embedder": {"provider": "openai", "config": {"model": cfg.embed_model, "api_key": PLACEHOLDER_KEY}},
        "vector_store": {"provider": "qdrant", "config": {
            "collection_name": "eve", "path": qdrant, "on_disk": true,
            "embedding_model_dims": EMBED_DIMS}},
        "history_db_path": history,
        "custom_instructions": CUSTOM_INSTRUCTIONS,
    })
}

/// `home` and every folder under it 0700, every file 0600. Symlinks are never followed.
pub fn make_private(home: &Path) {
    let _ = fs::create_dir_all(home);
    if fs::symlink_metadata(home).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        return;
    }
    let _ = fs::set_permissions(home, fs::Permissions::from_mode(0o700));
    // Top-down, each folder made 0700 before it is listed (os.walk does the same), errors skipped.
    for entry in walkdir::WalkDir::new(home).min_depth(1).follow_links(false).into_iter().flatten() {
        let ft = entry.file_type();
        if ft.is_symlink() {
            continue;
        }
        let mode = if ft.is_dir() { 0o700 } else { 0o600 };
        let _ = fs::set_permissions(entry.path(), fs::Permissions::from_mode(mode));
    }
}

/// The real mem0 Memory, with telemetry off and its home inside ours: the helper Python process (see
/// [`crate::python`]). Errs when that cannot be had.
pub fn open_mem0(cfg: &Mem0Config) -> anyhow::Result<Box<dyn Mem0Backend>> {
    fs::create_dir_all(&cfg.home)?;
    Ok(Box::new(crate::python::PythonMem0::open(
        cfg.clone(),
        crate::python::default_python(),
        Vec::new(),
        eve_spend::Ledger::default(),
    )?))
}

/// The calls eve makes on mem0's `Memory` (mem0ai 2.2). The real one is the helper process; tests pass a fake.
pub trait Mem0Backend: Send {
    /// `memory.search(query, filters=filters, top_k=top_k)`.
    fn search(&mut self, query: &str, filters: &Value, top_k: usize) -> anyhow::Result<Value>;
    /// `memory.get_all(filters=filters, top_k=top_k)`.
    fn get_all(&mut self, filters: &Value, top_k: usize) -> anyhow::Result<Value>;
    /// `memory.add(messages, user_id=user_id, metadata=metadata)`.
    fn add(&mut self, messages: &Value, user_id: &str, metadata: &Value) -> anyhow::Result<Value>;
    /// `memory.delete(memory_id)`.
    fn delete(&mut self, memory_id: &str) -> anyhow::Result<Value>;
    /// `memory.embedding_model.embed(text, memory_action)`. `Ok(false)` when this Memory has no embedder to call.
    fn embed(&mut self, _text: &str, _memory_action: &str) -> anyhow::Result<bool> {
        Ok(false)
    }
}

/// What makes the Memory: [`open_mem0`] by default; tests pass a fake.
pub type Factory = Arc<dyn Fn(&Mem0Config) -> anyhow::Result<Box<dyn Mem0Backend>> + Send + Sync>;
/// Monotonic seconds (tests inject one).
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

fn monotonic() -> f64 {
    static START: LazyLock<Instant> = LazyLock::new(Instant::now);
    START.elapsed().as_secs_f64()
}

/// `found.get("results", [])` of a dict, else `found or []`, keeping the dict rows. A value Python could not
/// iterate (a number, `True`) is its `TypeError`.
fn rows(found: &Value) -> anyhow::Result<Vec<Map<String, Value>>> {
    let empty = Value::Array(Vec::new());
    let list = match found {
        Value::Object(o) => o.get("results").unwrap_or(&empty),
        other if !truthy(other) => &empty,
        other => other,
    };
    match list {
        Value::Array(a) => Ok(a.iter().filter_map(|r| r.as_object().cloned()).collect()),
        // a string iterates characters and a dict its keys: never a dict row
        Value::String(_) | Value::Object(_) => Ok(Vec::new()),
        Value::Null => Err(Mem0Error::new("TypeError", "'NoneType' object is not iterable").into()),
        Value::Bool(_) => Err(Mem0Error::new("TypeError", "'bool' object is not iterable").into()),
        Value::Number(n) => Err(Mem0Error::new(
            "TypeError",
            format!("'{}' object is not iterable", if n.is_f64() { "float" } else { "int" }),
        )
        .into()),
    }
}

fn norm(text: &str) -> String {
    casefold(flat(text).trim_matches(|c| " .,;:!".contains(c)))
}

fn meta_of(row: &Map<String, Value>) -> Map<String, Value> {
    row.get("metadata").and_then(Value::as_object).cloned().unwrap_or_default()
}

fn src_of(row: &Map<String, Value>) -> String {
    flat(&str_or_empty(meta_of(row).get("src")))
}

/// What a forget's match reads for one memory: its text, and each of its srcs the way a record line carries one
/// (the owner line's, and eve's line before it when the unit held one), so a forget that follows a redacted line's
/// pointer (memory.Memory._follow_sources) reaches it, whichever of the two lines was forgotten.
fn match_text(row: &Map<String, Value>) -> String {
    let meta = meta_of(row);
    let mut parts = vec![str_or_empty(row.get("memory"))];
    for s in [src_of(row), flat(&str_or_empty(meta.get("src_before")))] {
        if !s.is_empty() {
            parts.push(format!("{{src: {s}}}"));
        }
    }
    parts.join(" ")
}

fn sources_on() -> bool {
    eve_records::records::sources_on(None)
}

/// A transcript line's ts → "YYYY-MM-DD HH:MM", its own date and time (the src form). "" when unreadable.
fn stamp(ts: Option<&Value>) -> String {
    let raw = str_of(ts.unwrap_or(&Value::Null));
    match fromisoformat(&raw) {
        Some((naive, _)) => naive.format("%Y-%m-%d %H:%M").to_string(),
        None => String::new(),
    }
}

/// The day a memory is from: its own `day`, else the date in a legacy note path, else when mem0 made it.
fn day_of(row: &Map<String, Value>) -> String {
    let meta = meta_of(row);
    for raw in [meta.get("day"), meta.get("note"), row.get("created_at")] {
        if let Some(m) = DAY.captures(&str_or_empty(raw)) {
            return m[1].to_string();
        }
    }
    String::new()
}

/// `float(v or 0)`, with Python's ValueError/TypeError read as 0.
fn score_of(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        // Rust's float grammar is Python's (signs, exponents, inf/infinity/nan in any case) less the digit
        // underscores, which Python allows between digits.
        Some(Value::String(s)) => {
            let t = strip(s);
            let underscores_ok = !t.starts_with('_')
                && !t.ends_with('_')
                && t.match_indices('_').all(|(i, _)| {
                    t[..i].chars().next_back().is_some_and(|c| c.is_ascii_digit())
                        && t[i + 1..].chars().next().is_some_and(|c| c.is_ascii_digit())
                });
            if !underscores_ok {
                return 0.0;
            }
            t.replace('_', "").parse::<f64>().unwrap_or(0.0)
        }
        _ => 0.0,
    }
}

/// One mem0 message: `{"role": "user" | "assistant", "content": ...}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    pub fn to_value(&self) -> Value {
        json!({"role": self.role, "content": self.content})
    }
}

/// One `add`: its messages, the owner line's src and eve's line's src before it ("" without one).
pub type Unit = (Vec<Message>, String, String);

/// One memory a forget may take: `{"id", "text"}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub id: String,
    pub text: String,
}

/// What `ingest_day` reads of the transcripts (`transcripts.Transcripts`; a test wraps one).
pub trait Conversations {
    fn conversations(&self, day: &str) -> Vec<ConvInfo>;
    fn conv_lines(&self, conv: &str) -> Vec<Arc<Line>>;
}

impl Conversations for Transcripts {
    fn conversations(&self, day: &str) -> Vec<ConvInfo> {
        Transcripts::conversations(self, day)
    }
    fn conv_lines(&self, conv: &str) -> Vec<Arc<Line>> {
        Transcripts::conv_lines(self, conv)
    }
}

struct State {
    mem: Option<Box<dyn Mem0Backend>>,
    failed_at: Option<f64>,
    forgets: u64, // forgets so far; an ingest that saw it move adds nothing more
}

struct Inner {
    cfg: Mem0Config,
    home: PathBuf,
    factory: Factory,
    clock: Clock,
    state: Mutex<State>,
    in_flight: Mutex<usize>,
    last_timeout_log: Mutex<f64>,
    search_timeout: Mutex<Duration>,
}

/// mem0 for one owner. `factory` makes the Memory ([`open_mem0`] by default; tests pass a fake); `clock` is
/// monotonic seconds (tests inject one).
///
/// Every call to mem0 is serialised (a local Qdrant store is one writer at a time). Search runs on a thread of its
/// own so it can be abandoned after SEARCH_TIMEOUT_SECS without holding up the process's exit; the rest is called
/// from the dream's worker thread.
pub struct OwnerMemory {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for OwnerMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnerMemory").field("cfg", &self.inner.cfg).finish_non_exhaustive()
    }
}

struct InFlight(Arc<Inner>);

impl Drop for InFlight {
    fn drop(&mut self) {
        *self.0.in_flight.lock() -= 1;
    }
}

impl Inner {
    /// The Memory, opened on first use. Call with the state lock held. None while off.
    fn memory<'a>(&self, st: &'a mut State) -> Option<&'a mut Box<dyn Mem0Backend>> {
        if st.mem.is_none() {
            if let Some(failed) = st.failed_at {
                if (self.clock)() - failed < RETRY_AFTER_SECS {
                    return None;
                }
            }
            make_private(&self.home);
            match (self.factory)(&self.cfg) {
                Ok(mem) => {
                    st.mem = Some(mem);
                    make_private(&self.home); // what opening just created, too
                    st.failed_at = None;
                    info!(
                        "mem0: on — {} extracts, {} embeds, stored in {}",
                        self.cfg.model,
                        self.cfg.embed_model,
                        self.home.display()
                    );
                }
                // not installed, no key, telemetry refused: all are "off"
                Err(e) => {
                    st.failed_at = Some((self.clock)());
                    warn!(
                        "mem0: unavailable ({}) — tried again in {} minutes",
                        error_name(&e),
                        (RETRY_AFTER_SECS / 60.0).floor() as i64
                    );
                }
            }
        }
        st.mem.as_mut()
    }

    fn search_now(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Value>> {
        let found = {
            let mut st = self.state.lock();
            let Some(mem) = self.memory(&mut st) else {
                return Ok(Vec::new());
            };
            mem.search(query, &json!({"user_id": OWNER}), limit)?
        };
        let mut out = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for r in rows(&found)? {
            let text = flat(&str_or_empty(r.get("memory")));
            let score = score_of(r.get("score").filter(|v| truthy(v)));
            if text.is_empty() || score < MIN_SCORE || text.contains(STAR) || seen.contains(&norm(&text)) {
                continue;
            }
            seen.insert(norm(&text));
            let meta = meta_of(&r);
            let mut hit = Map::new();
            hit.insert("text".into(), Value::String(text));
            hit.insert("day".into(), Value::String(day_of(&r)));
            hit.insert("channel".into(), Value::String(str_or_empty(meta.get("channel"))));
            let src = src_of(&r);
            if !src.is_empty() {
                hit.insert("src".into(), Value::String(src));
            }
            out.push(Value::Object(hit));
        }
        out.truncate(limit);
        Ok(out)
    }
}

impl OwnerMemory {
    /// `OwnerMemory(cfg, factory)`: `None` opens the real mem0 ([`open_mem0`]).
    pub fn new(cfg: Mem0Config, factory: Option<Factory>) -> Self {
        Self::with_clock(cfg, factory, Arc::new(monotonic))
    }

    /// The same with an injected clock (monotonic seconds).
    pub fn with_clock(cfg: Mem0Config, factory: Option<Factory>, clock: Clock) -> Self {
        let factory = factory.unwrap_or_else(|| Arc::new(open_mem0));
        let home = cfg.home.clone();
        OwnerMemory {
            inner: Arc::new(Inner {
                cfg,
                home,
                factory,
                clock,
                state: Mutex::new(State { mem: None, failed_at: None, forgets: 0 }),
                in_flight: Mutex::new(0),
                last_timeout_log: Mutex::new(-TIMEOUT_LOG_EVERY_SECS),
                search_timeout: Mutex::new(Duration::from_secs_f64(SEARCH_TIMEOUT_SECS)),
            }),
        }
    }

    pub fn cfg(&self) -> &Mem0Config {
        &self.inner.cfg
    }

    pub fn home(&self) -> &Path {
        &self.inner.home
    }

    /// How long a search is waited for (SEARCH_TIMEOUT_SECS; tests shorten it).
    pub fn set_search_timeout(&self, secs: f64) {
        *self.inner.search_timeout.lock() = Duration::from_secs_f64(secs.max(0.0));
    }

    // -- opening -------------------------------------------------------------------------------

    /// Open the store and embed one word, on the caller's thread (the daemon calls it on one of its own). With the
    /// local embedder the first open loads the model (~3 s), longer than a search's SEARCH_TIMEOUT_SECS cap, so the
    /// first search after a start answered without mem0 (live, 2026-10-09). Never fails.
    pub fn warm(&self) {
        let mut st = self.inner.state.lock();
        let Some(mem) = self.inner.memory(&mut st) else {
            return;
        };
        // a failed warm-up costs the first search its speed, nothing else
        match mem.embed("warm", "search") {
            Ok(_) => info!("mem0: warm"),
            Err(e) => warn!("mem0: warm-up failed ({})", error_name(&e)),
        }
    }

    // -- reading -------------------------------------------------------------------------------

    /// The memories that mean what `query` asks, best first: `[{"text", "day", "channel"[, "src"]}]`, at most
    /// `limit`, each scoring MIN_SCORE or more. `[]` when there are none, mem0 is off, or it took longer than
    /// SEARCH_TIMEOUT_SECS (the call is abandoned, not waited for).
    pub fn search(&self, query: &str, limit: usize) -> Vec<Value> {
        let query = flat(query);
        if query.is_empty() {
            return Vec::new();
        }
        {
            let mut n = self.inner.in_flight.lock();
            if *n >= MAX_SEARCHES_IN_FLIGHT {
                return Vec::new(); // earlier searches are still stuck: do not pile up threads
            }
            *n += 1;
        }
        let guard = InFlight(self.inner.clone());
        let inner = self.inner.clone();
        let (tx, rx) = mpsc::channel();
        let limit = limit.clamp(1, MAX_HITS);
        let spawned = std::thread::Builder::new().name("mem0-search".into()).spawn(move || {
            let _guard = guard;
            let rows = inner.search_now(&query, limit);
            drop(_guard);
            let _ = tx.send(rows);
        });
        if let Err(e) = spawned {
            warn!("mem0: search failed ({})", eve_transcripts::py::os_error_name(&e));
            return Vec::new();
        }
        let timeout = *self.inner.search_timeout.lock();
        match rx.recv_timeout(timeout) {
            Ok(Ok(rows)) => rows,
            Ok(Err(e)) => {
                warn!("mem0: search failed ({})", error_name(&e));
                Vec::new()
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                warn!("mem0: search failed (panic)");
                Vec::new()
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let now = (self.inner.clock)();
                let mut last = self.inner.last_timeout_log.lock();
                if now - *last >= TIMEOUT_LOG_EVERY_SECS {
                    *last = now;
                    warn!("mem0: search took over {:.0} s — answered without it", timeout.as_secs_f64());
                }
                Vec::new()
            }
        }
    }

    fn all(mem: &mut Box<dyn Mem0Backend>) -> anyhow::Result<Vec<Map<String, Value>>> {
        rows(&mem.get_all(&json!({"user_id": OWNER}), GET_ALL_CAP)?)
    }

    /// Every memory whose text `matcher` accepts: `[{"id", "text"}]`. `[]` when mem0 is off or failed.
    pub fn find(&self, matcher: &dyn Fn(&str) -> bool) -> Vec<Found> {
        let rows = {
            let mut st = self.inner.state.lock();
            let Some(mem) = self.inner.memory(&mut st) else {
                return Vec::new();
            };
            match Self::all(mem) {
                Ok(rows) => rows,
                Err(e) => {
                    warn!("mem0: listing failed ({})", error_name(&e));
                    return Vec::new();
                }
            }
        };
        rows.iter()
            .filter(|r| r.get("id").is_some_and(truthy) && matcher(&match_text(r)))
            .map(|r| Found { id: str_of(&r["id"]), text: str_or_empty(r.get("memory")) })
            .collect()
    }

    // -- writing (the dream and a forget only) ----------------------------------------------------

    fn done(&self) -> HashSet<String> {
        match fs::read(self.inner.home.join(INGESTED_CONVS)) {
            Ok(raw) => read_text(&raw).split('\n').filter(|s| !s.is_empty()).map(str::to_string).collect(),
            Err(_) => HashSet::new(),
        }
    }

    fn mark(&self, conv: &str) -> bool {
        let opened = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.inner.home.join(INGESTED_CONVS));
        match opened.and_then(|mut f| f.write_all(format!("{conv}\n").as_bytes())) {
            Ok(()) => true,
            Err(e) => {
                warn!(
                    "mem0: could not mark a conversation read ({})",
                    eve_transcripts::py::os_error_name(&e)
                );
                false
            }
        }
    }

    /// A conversation's lines as mem0 messages: what the owner said → user, what eve said → assistant. Everything
    /// else (tools, commands, relays, photos, close markers, Claude) stays out.
    ///
    /// A line kept as outside words (`out`: eve's reply written after a read, a task's sentence) keeps its place
    /// with its words replaced by transcripts.OUTSIDE_LEFT_OUT, as the dream's day_text does (round-4 review): a
    /// page's or a mail's words never become a memory of the owner.
    pub fn messages<L: Borrow<Line>>(lines: &[L]) -> Vec<Message> {
        let mut out = Vec::new();
        for ln in lines {
            let ln = ln.borrow();
            let role = match ln.get("who") {
                Some(Value::String(w)) if w == "owner" => "user",
                Some(Value::String(w)) if w == "eve" => "assistant",
                _ => continue,
            };
            let mut text = flat(&str_or_empty(ln.get("text")));
            if ln.get("kind") != Some(&Value::String("say".into())) || text.is_empty() {
                continue;
            }
            if ln.get("out").is_some_and(truthy) {
                text = OUTSIDE_LEFT_OUT.to_string();
            }
            out.push(Message { role: role.to_string(), content: head(&text, MESSAGE_CHARS).to_string() });
        }
        out
    }

    /// A conversation as one unit per owner line, for EVE_MEMORY_SOURCES: (messages, src, src_before), where the
    /// messages are eve's line just before it (when it is eve's) and the owner's line, src is the owner line's
    /// "YYYY-MM-DD HH:MM" and src_before is eve's line's ("" without one). So every memory mem0 makes from a unit
    /// points at both lines it may have come from.
    pub fn line_units<L: Borrow<Line>>(lines: &[L]) -> Vec<Unit> {
        let mut units = Vec::new();
        let mut prev: Option<(Message, String)> = None;
        for ln in lines {
            let ln = ln.borrow();
            let mut msg = Self::messages(std::slice::from_ref(ln));
            let Some(msg) = msg.pop() else {
                continue;
            };
            if msg.role == "user" {
                let src = stamp(ln.get("ts"));
                match prev.take() {
                    Some((eve, before)) => units.push((vec![eve, msg], src, before)),
                    None => units.push((vec![msg], src, String::new())),
                }
            } else {
                prev = Some((msg, stamp(ln.get("ts"))));
            }
        }
        units
    }

    /// Feed each conversation of `day` that mem0 has not read to it. → how many went in.
    ///
    /// One `add` per CHUNK_MESSAGES messages, metadata {day, channel, conv}. With EVE_MEMORY_SOURCES on, one `add`
    /// per owner line instead ([`Self::line_units`]), its metadata also carrying src, the line's date and time. A
    /// conversation is marked read only when every chunk went in; one that failed is tried again next time (mem0's
    /// own update step and `tidy` absorb a chunk read twice). A conversation in which the owner said nothing is
    /// marked read and not sent.
    pub fn ingest_day(&self, transcripts: &dyn Conversations, day: &str) -> usize {
        let mut added = 0;
        let done = self.done();
        let by_line = sources_on();
        let since = self.inner.state.lock().forgets; // before a word is read: a forget after it voids what was read
        for info in transcripts.conversations(day) {
            if done.contains(&info.conv) {
                continue;
            }
            let lines = transcripts.conv_lines(&info.conv);
            let units: Vec<Unit> = if by_line {
                Self::line_units(&lines)
            } else {
                Self::messages(&lines)
                    .chunks(CHUNK_MESSAGES)
                    .map(|c| (c.to_vec(), String::new(), String::new()))
                    .collect()
            };
            let chunks: Vec<Unit> = units.into_iter().filter(|u| u.0.iter().any(|m| m.role == "user")).collect();
            let mut ok = true;
            for (chunk, src, src_before) in &chunks {
                let mut st = self.inner.state.lock();
                if st.forgets != since {
                    info!("mem0: a forget ran while a day was read — the rest is read again next night");
                    return added;
                }
                let Some(mem) = self.inner.memory(&mut st) else {
                    return added;
                };
                let mut meta = Map::new();
                meta.insert("day".into(), Value::String(day.to_string()));
                meta.insert("channel".into(), Value::String(info.ch.clone()));
                meta.insert("conv".into(), Value::String(info.conv.clone()));
                if !src.is_empty() {
                    meta.insert("src".into(), Value::String(src.clone()));
                }
                if !src_before.is_empty() {
                    meta.insert("src_before".into(), Value::String(src_before.clone()));
                }
                let messages = Value::Array(chunk.iter().map(Message::to_value).collect());
                if let Err(e) = mem.add(&messages, OWNER, &Value::Object(meta)) {
                    warn!("mem0: a conversation did not go in ({}); tried again next time", error_name(&e));
                    ok = false;
                    break;
                }
            }
            if !ok {
                continue;
            }
            {
                let _st = self.inner.state.lock();
                make_private(&self.inner.home);
                if !self.mark(&info.conv) {
                    return added;
                }
            }
            if !chunks.is_empty() {
                added += 1;
            }
        }
        if added > 0 {
            info!("mem0: read {} conversation(s) of one day", added);
        }
        added
    }

    fn delete(mem: &mut Box<dyn Mem0Backend>, ids: &[String]) -> Vec<String> {
        let mut gone = Vec::new();
        for mid in ids {
            match mem.delete(mid) {
                Ok(_) => gone.push(mid.clone()),
                // already gone, or mem0 failed: the rest still go
                Err(e) => warn!("mem0: could not delete one memory ({})", error_name(&e)),
            }
        }
        gone
    }

    /// Delete exact duplicates (same text, any case or spacing; the oldest stays) and anything carrying a ★ (a
    /// candidate eve proposed, which only the owner may promote). → how many went.
    pub fn tidy(&self) -> usize {
        let gone = {
            let mut st = self.inner.state.lock();
            let Some(mem) = self.inner.memory(&mut st) else {
                return 0;
            };
            let mut rows = match Self::all(mem) {
                Ok(rows) => rows,
                Err(e) => {
                    warn!("mem0: listing failed ({})", error_name(&e));
                    return 0;
                }
            };
            rows.sort_by_key(|r| str_or_empty(r.get("created_at")));
            let mut seen: HashSet<String> = HashSet::new();
            let mut doomed = Vec::new();
            for r in &rows {
                let text = str_or_empty(r.get("memory"));
                let Some(mid) = r.get("id").filter(|v| truthy(v)) else {
                    continue;
                };
                let key = norm(&text);
                if text.contains(STAR) || seen.contains(&key) {
                    doomed.push(str_of(mid));
                } else {
                    seen.insert(key);
                }
            }
            Self::delete(mem, &doomed)
        };
        if !gone.is_empty() {
            info!("mem0: tidied {} memories", gone.len());
        }
        gone.len()
    }

    /// Delete these memories, then everything mem0's history database still holds of them. → how many memories
    /// were deleted. Errs with Mem0ForgetError when mem0 is not open.
    ///
    /// mem0's `delete` removes the vector but writes the old text into its history table (a DELETE row), and every
    /// earlier ADD/UPDATE row keeps it too; its messages table keeps the last raw messages it was fed. So after the
    /// deletes: every history row of these ids goes, the messages table is emptied (it is only mem0's extraction
    /// context, and a forgotten line may be in it), with secure_delete on, and the databases are vacuumed so the
    /// text does not linger in free pages.
    pub fn forget<S: AsRef<str>>(&self, ids: &[S]) -> Result<usize, Mem0ForgetError> {
        let ids: Vec<String> =
            ids.iter().map(|i| i.as_ref().to_string()).filter(|i| !strip(i).is_empty()).collect();
        if ids.is_empty() {
            return Ok(0);
        }
        let gone = {
            let mut st = self.inner.state.lock();
            st.forgets += 1;
            let Some(mem) = self.inner.memory(&mut st) else {
                return Err(Mem0ForgetError("mem0 is not open".into()));
            };
            let gone = Self::delete(mem, &ids);
            self.scrub(&ids);
            gone
        };
        info!("mem0: forgot {} memories", gone.len());
        Ok(gone.len())
    }

    /// `find` and `forget` in one hold of the lock, so nothing `ingest_day` adds can land between the listing and
    /// the deletes; and an ingest that read its conversation before this adds nothing after it. → how many memories
    /// were deleted. Errs with Mem0ForgetError when mem0 cannot be reached, as `forget` does.
    pub fn forget_matching(&self, matcher: &dyn Fn(&str) -> bool) -> Result<usize, Mem0ForgetError> {
        let gone = {
            let mut st = self.inner.state.lock();
            st.forgets += 1;
            let Some(mem) = self.inner.memory(&mut st) else {
                return Err(Mem0ForgetError("mem0 is not open".into()));
            };
            let rows = match Self::all(mem) {
                Ok(rows) => rows,
                Err(e) => {
                    let name = error_name(&e);
                    warn!("mem0: listing failed ({})", name);
                    return Err(Mem0ForgetError(name));
                }
            };
            let ids: Vec<String> = rows
                .iter()
                .filter(|r| r.get("id").is_some_and(truthy) && matcher(&match_text(r)))
                .map(|r| str_of(&r["id"]))
                .collect();
            let gone = if ids.is_empty() { Vec::new() } else { Self::delete(mem, &ids) };
            if !ids.is_empty() {
                self.scrub(&ids);
            }
            gone
        };
        info!("mem0: forgot {} memories", gone.len());
        Ok(gone.len())
    }

    fn scrub(&self, ids: &[String]) {
        let path = self.inner.home.join(HISTORY_DB);
        if path.exists() {
            if let Err(e) = scrub_history(&path, ids) {
                warn!("mem0: could not scrub the history database ({})", sqlite_error_name(&e));
            }
        }
        let mut dbs = vec![path];
        let mut found: Vec<PathBuf> = walkdir::WalkDir::new(self.inner.home.join("qdrant"))
            .follow_links(false)
            .into_iter()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".sqlite"))
            .map(|e| e.into_path())
            .collect();
        found.sort();
        dbs.extend(found);
        for db in dbs {
            let is_link = fs::symlink_metadata(&db).map(|m| m.file_type().is_symlink()).unwrap_or(false);
            if db.exists() && !is_link {
                if let Err(e) = vacuum(&db) {
                    warn!("mem0: could not compact a database ({})", sqlite_error_name(&e));
                }
            }
        }
    }
}

fn connect(path: &Path) -> rusqlite::Result<rusqlite::Connection> {
    let con = rusqlite::Connection::open(path)?;
    con.busy_timeout(Duration::from_secs(10))?;
    Ok(con)
}

fn scrub_history(path: &Path, ids: &[String]) -> rusqlite::Result<()> {
    let mut con = connect(path)?;
    con.execute_batch("PRAGMA secure_delete = ON")?;
    let tables: HashSet<String> = {
        let mut stmt = con.prepare("SELECT name FROM sqlite_master WHERE type='table'")?;
        let names = stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<_>>()?;
        names
    };
    let tx = con.transaction()?;
    for table in ["history", "history_old"] {
        if tables.contains(table) {
            let cols: HashSet<String> = {
                let mut stmt = tx.prepare(&format!("PRAGMA table_info({table})"))?;
                let cols = stmt.query_map([], |r| r.get::<_, String>(1))?.collect::<rusqlite::Result<_>>()?;
                cols
            };
            if cols.contains("memory_id") {
                let mut stmt = tx.prepare(&format!("DELETE FROM {table} WHERE memory_id = ?"))?;
                for id in ids {
                    stmt.execute([id])?;
                }
            }
        }
    }
    if tables.contains("messages") {
        tx.execute("DELETE FROM messages", [])?;
    }
    tx.commit()
}

fn vacuum(path: &Path) -> rusqlite::Result<()> {
    connect(path)?.execute_batch("VACUUM")
}

impl eve_records::MeaningSearch for OwnerMemory {
    fn search(&self, query: &str) -> anyhow::Result<Vec<Value>> {
        Ok(OwnerMemory::search(self, query, MAX_HITS))
    }
}

static SHARED: LazyLock<Mutex<HashMap<String, Arc<OwnerMemory>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// The daemon's one OwnerMemory (a local Qdrant store takes one opener), or None when memory or the index is
/// switched off, or mem0ai is not installed.
pub fn shared(recall_cfg: &RecallConfig, environ: Option<&HashMap<String, String>>) -> Option<Arc<OwnerMemory>> {
    shared_with(recall_cfg, environ, crate::python::available)
}

/// [`shared`] with the installed-check as a seam (Python's tests patch `available`).
pub fn shared_with(
    recall_cfg: &RecallConfig,
    environ: Option<&HashMap<String, String>>,
    available: impl FnOnce() -> bool,
) -> Option<Arc<OwnerMemory>> {
    let cfg = configured(recall_cfg, environ);
    if !cfg.enabled {
        return None;
    }
    if !available() {
        warn!("mem0: mem0ai is not installed — memory search stays records and transcripts");
        return None;
    }
    let key = path_str(&cfg.home);
    let mut shared = SHARED.lock();
    Some(shared.entry(key).or_insert_with(|| Arc::new(OwnerMemory::new(cfg, None))).clone())
}
