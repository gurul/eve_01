//! Watch Claude Code transcript files under `~/.claude/projects/` for token + entry updates.
//!
//! Hooks don't expose cumulative token counts, so we parse them out of the session JSONL files. Each assistant
//! message has a `usage.output_tokens` field that we sum per file, then aggregate across all files for the
//! grand total.
//!
//! Port of `eve_bridge/jsonl_tailer.py`. The file reading, record parsing and timestamp handling follow the
//! Python (and the CPython library behavior it leans on) exactly; see `PARITY.md`.

pub mod isotime;
pub mod py;
pub mod watch;

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::future::BoxFuture;
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{json, Value};

pub use isotime::{LocalZone, SystemZone, TimeError};
pub use py::{PyDict, PyValue};
pub use watch::{Awatch, Change, ChangeSource, FileChange, LazyAwatch, WatchConfig, WatchError};

/// Callback: async (tokens_cumulative, tokens_today, cost_cumulative, cost_today, entries).
/// Cost values are USD estimates; see `eve-pricing`. An error from it ends `run()` the way an exception does.
pub type TokensCallback =
    Arc<dyn Fn(i64, i64, f64, f64, Vec<(f64, String)>) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

/// Callback fired the moment a new assistant record with text content is parsed.
/// Async (transcript_path, text, uuid). Skipped for tool_use-only turns.
pub type AssistantTextCallback = Arc<dyn Fn(String, String, String) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

/// Makes the change source `run()` iterates, given the roots (the Python calls `awatch(*roots, ...)`).
pub type WatchFactory = Arc<dyn Fn(&[PathBuf]) -> Box<dyn ChangeSource> + Send + Sync>;

/// The file operations `_process_file` makes: `os.path.getsize` and `open(path, "rb")`. A seam so tests can
/// count opens and freeze a size snapshot, as the Python tests patch `builtins.open` and `os.path.getsize`.
pub trait TranscriptFs: Send + Sync {
    fn size(&self, path: &Path) -> io::Result<u64>;
    fn open(&self, path: &Path) -> io::Result<Box<dyn ReadSeek>>;
}

/// A readable, seekable file.
pub trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

/// The real filesystem.
#[derive(Debug, Default, Clone, Copy)]
pub struct StdFs;

impl TranscriptFs for StdFs {
    fn size(&self, path: &Path) -> io::Result<u64> {
        // os.path.getsize follows symlinks.
        Ok(std::fs::metadata(path)?.len())
    }

    fn open(&self, path: &Path) -> io::Result<Box<dyn ReadSeek>> {
        Ok(Box::new(File::open(path)?))
    }
}

/// Why one file's pass stopped early (the exceptions `_process_file` lets out; the callers log and go on).
#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Time(#[from] TimeError),
    #[error("maximum recursion depth exceeded while decoding a JSON array from a unicode string")]
    Recursion,
}

/// The tailer's bookkeeping. One writer at a time: the sweep and each change batch run on a blocking thread
/// with this locked, and nothing else touches it until they return.
pub(crate) struct TailerState {
    // file path → offset of the first byte not yet consumed
    pub(crate) offsets: HashMap<String, u64>,
    pub(crate) tokens_per_file: IndexMap<String, i64>,
    // day_key → sum of tokens_today across files (day_key is YYYY-MM-DD local)
    pub(crate) day_key: String,
    pub(crate) today_tokens_per_file: IndexMap<String, i64>,
    // Mirror of the tokens maps in USD, estimated at parse time, so changing pricing rates only affects new
    // records. Insertion-ordered like Python dicts, so the float sums add in the same order.
    pub(crate) cost_per_file: IndexMap<String, f64>,
    pub(crate) today_cost_per_file: IndexMap<String, f64>,
    // file path → assistant uuids already emitted, so the initial sweep on daemon startup doesn't re-fire the
    // callback for every historical assistant message.
    pub(crate) emitted_assistant_uuids: HashMap<String, HashSet<String>>,
    // Until the initial sweep returns, history seeds the UUIDs without firing the live callback for past turns.
    pub(crate) initial_sweep_done: bool,
    // Deferred callbacks collected while consuming records (sync), fired from the watch loop (async).
    pub(crate) pending_assistant_emits: Vec<(String, String, String)>,
    // `self.on_assistant_text is not None`, readable from the blocking thread.
    pub(crate) has_text_callback: bool,
    pub(crate) fs: Arc<dyn TranscriptFs>,
    pub(crate) zone: Arc<dyn LocalZone>,
}

/// Incrementally reads every transcript JSONL, tracks file offsets so we only process new bytes, and
/// recomputes aggregates on change. (`JSONLTailer` in Python.)
pub struct JsonlTailer {
    pub on_update: TokensCallback,
    on_assistant_text: Option<AssistantTextCallback>,
    // Several roots because one daemon serves sessions from more than one Claude Code config home (see
    // eve-claude-home). Aggregates are keyed by file path, so they sum across roots without further bookkeeping.
    roots: Vec<PathBuf>,
    pub(crate) state: Arc<Mutex<TailerState>>,
    watch_factory: WatchFactory,
}

impl JsonlTailer {
    /// `JSONLTailer(on_update, roots=None, on_assistant_text=None)`. Empty or absent roots mean
    /// `transcript_roots()`.
    pub fn new(
        on_update: TokensCallback,
        roots: Option<Vec<PathBuf>>,
        on_assistant_text: Option<AssistantTextCallback>,
    ) -> Result<Self, eve_claude_home::HomeDirError> {
        let roots = match roots {
            Some(r) if !r.is_empty() => r,
            _ => eve_claude_home::transcript_roots(None)?,
        };
        let zone: Arc<dyn LocalZone> = Arc::new(SystemZone);
        // localtime() of the current time does not fail; an empty key only means the first pass resets the
        // (still empty) today maps.
        let day_key = today_key_in(zone.as_ref()).unwrap_or_default();
        let state = TailerState {
            offsets: HashMap::new(),
            tokens_per_file: IndexMap::new(),
            day_key,
            today_tokens_per_file: IndexMap::new(),
            cost_per_file: IndexMap::new(),
            today_cost_per_file: IndexMap::new(),
            emitted_assistant_uuids: HashMap::new(),
            initial_sweep_done: false,
            pending_assistant_emits: Vec::new(),
            has_text_callback: on_assistant_text.is_some(),
            fs: Arc::new(StdFs),
            zone,
        };
        Ok(JsonlTailer {
            on_update,
            on_assistant_text,
            roots,
            state: Arc::new(Mutex::new(state)),
            watch_factory: Arc::new(|roots: &[PathBuf]| {
                // step=500: watchfiles' default 50 ms woke this thread 20 times a second with nothing to do;
                // its own 1600 ms debounce already sets the latency.
                Box::new(LazyAwatch::new(roots.to_vec(), WatchConfig::from_env(500))) as Box<dyn ChangeSource>
            }),
        })
    }

    /// Replace the file watcher (tests drive `run()` with scripted batches).
    pub fn with_watch_factory(mut self, factory: WatchFactory) -> Self {
        self.watch_factory = factory;
        self
    }

    /// Replace the file operations.
    pub fn with_fs(self, fs: Arc<dyn TranscriptFs>) -> Self {
        self.state.lock().fs = fs;
        self
    }

    /// Replace the local time zone.
    pub fn with_zone(self, zone: Arc<dyn LocalZone>) -> Self {
        {
            let mut st = self.state.lock();
            st.day_key = today_key_in(zone.as_ref()).unwrap_or_default();
            st.zone = zone;
        }
        self
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Current aggregates: (tokens_cumulative, tokens_today, cost_cumulative, cost_today).
    pub fn totals(&self) -> (i64, i64, f64, f64) {
        self.state.lock().totals()
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        for r in &self.roots {
            if !r.exists() {
                tracing::warn!("transcript root {} does not exist; creating", r.display());
                std::fs::create_dir_all(r)?;
            }
        }
        tracing::info!(
            "tailing transcripts: {}",
            self.roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>().join(", ")
        );

        // Initial sweep so aggregates are hot before any file event fires. Marked "sweep not done" so records
        // skip the live-emit callback during history replay; callbacks only fire for future writes.
        self.initial_sweep().await?;
        self.state.lock().initial_sweep_done = true;
        self.emit().await?;

        // Watch for changes: batches of (Change, path). Anything that goes wrong in here ends the tailer with a
        // log line rather than an error, as the Python's `except Exception: log.exception(...)` does.
        if let Err(e) = self.watch_loop().await {
            tracing::error!("jsonl tailer crashed: {e:#}");
        }
        Ok(())
    }

    async fn watch_loop(&self) -> anyhow::Result<()> {
        let mut source = (self.watch_factory)(&self.roots);
        while let Some(changes) = source.next_batch().await {
            self.handle_changes(changes?).await?;
            self.fire_pending_emits().await;
            self.emit().await?;
        }
        Ok(())
    }

    pub(crate) async fn fire_pending_emits(&self) {
        let to_fire = std::mem::take(&mut self.state.lock().pending_assistant_emits);
        let Some(cb) = self.on_assistant_text.as_ref() else {
            return;
        };
        for (path, text, uuid) in to_fire {
            if let Err(e) = cb(path, text, uuid).await {
                tracing::error!("on_assistant_text callback failed: {e:#}");
            }
        }
    }

    /// Every transcript under the roots, read from its start. On a Mac with a year of sessions that is hundreds
    /// of files and hundreds of megabytes, and done on the loop it held the daemon for 41 s at start (the loop
    /// watchdog named it, 2026-09-21): the board sat at "--:--" and the wake word waited. So the walk runs in a
    /// thread; nothing else touches the offsets until it returns.
    pub(crate) async fn initial_sweep(&self) -> anyhow::Result<()> {
        let state = self.state.clone();
        let roots = self.roots.clone();
        tokio::task::spawn_blocking(move || state.lock().sweep_all(&roots)).await?;
        Ok(())
    }

    /// Read and parse the batch in one worker. Awaited before emitting or processing another batch, so the
    /// maps still have one writer.
    pub(crate) async fn handle_changes(&self, changes: HashSet<FileChange>) -> anyhow::Result<()> {
        let state = self.state.clone();
        tokio::task::spawn_blocking(move || state.lock().process_changes(&changes)).await?;
        Ok(())
    }

    pub(crate) async fn emit(&self) -> anyhow::Result<()> {
        let (cumulative, today, cost_cumulative, cost_today) = self.totals();
        // Entries aren't produced by the tailer — hook events feed them directly. The argument stays for
        // future expansion.
        (self.on_update)(cumulative, today, cost_cumulative, cost_today, Vec::new()).await
    }
}

impl TailerState {
    pub(crate) fn totals(&self) -> (i64, i64, f64, f64) {
        let cumulative = self.tokens_per_file.values().fold(0i64, |a, b| a.saturating_add(*b));
        let today = self.today_tokens_per_file.values().fold(0i64, |a, b| a.saturating_add(*b));
        let cost_cumulative = py::py_fsum_builtin(self.cost_per_file.values().copied());
        let cost_today = py::py_fsum_builtin(self.today_cost_per_file.values().copied());
        (cumulative, today, cost_cumulative, cost_today)
    }

    pub(crate) fn sweep_all(&mut self, roots: &[PathBuf]) {
        for root in roots {
            rglob_jsonl(root, &mut |p: &Path| {
                let Some(path) = p.to_str() else {
                    tracing::debug!("initial sweep of {} failed: path is not UTF-8", p.display());
                    return;
                };
                if let Err(e) = self.process_file(path) {
                    tracing::debug!("initial sweep of {path} failed: {e}");
                }
            });
        }
    }

    pub(crate) fn process_changes(&mut self, changes: &HashSet<FileChange>) {
        for (change, path) in changes {
            if !path.ends_with(".jsonl") {
                continue;
            }
            if *change == Change::Deleted {
                self.offsets.remove(path);
                self.tokens_per_file.shift_remove(path);
                self.today_tokens_per_file.shift_remove(path);
                self.cost_per_file.shift_remove(path);
                self.today_cost_per_file.shift_remove(path);
                self.emitted_assistant_uuids.remove(path);
                continue;
            }
            if let Err(e) = self.process_file(path) {
                tracing::debug!("process {path} failed: {e}");
            }
        }
    }

    /// Read new bytes since the last offset. Parse each line, accumulate tokens + entries.
    pub(crate) fn process_file(&mut self, path: &str) -> Result<(), ProcessError> {
        let Ok(size) = self.fs.size(Path::new(path)) else {
            return Ok(());
        };
        let mut start = self.offsets.get(path).copied().unwrap_or(0);
        if start > size {
            // File was truncated/rotated. Reset.
            start = 0;
            self.tokens_per_file.shift_remove(path);
            self.today_tokens_per_file.shift_remove(path);
            self.cost_per_file.shift_remove(path);
            self.today_cost_per_file.shift_remove(path);
            self.offsets.insert(path.to_string(), 0);
        }
        if start == size {
            return Ok(());
        }

        let current_day = today_key_in(self.zone.as_ref())?;
        if current_day != self.day_key {
            // Day rolled over — reset today-only counters.
            self.day_key = current_day.clone();
            self.today_tokens_per_file.clear();
            self.today_cost_per_file.clear();
        }

        // Buffered line reads avoid holding a transcript and its split copy in RAM. Limit every read to the size
        // snapshot, so a busy writer cannot extend this batch forever. Incomplete trailing lines stay unconsumed.
        let mut f = BufReader::new(self.fs.open(Path::new(path))?);
        f.seek(SeekFrom::Start(start))?;
        let mut offset = start;
        let mut raw = Vec::new();
        while offset < size {
            raw.clear();
            (&mut f).take(size - offset).read_until(b'\n', &mut raw)?;
            if !raw.ends_with(b"\n") {
                break;
            }
            offset += raw.len() as u64;
            self.offsets.insert(path.to_string(), offset);
            if py::bytes_strip(&raw).is_empty() {
                continue;
            }
            let obj = match py::loads_bytes(&raw) {
                Ok(v) => v,
                Err(py::JsonError::Value(_)) => continue,
                Err(py::JsonError::Recursion) => return Err(ProcessError::Recursion),
            };
            if let PyValue::Dict(obj) = &obj {
                self.consume_obj(path, obj, &current_day)?;
            }
        }
        Ok(())
    }

    pub(crate) fn consume_obj(&mut self, path: &str, obj: &PyDict, current_day: &str) -> Result<(), TimeError> {
        // Claude Code transcript entries can nest differently across versions. Check a few common shapes.
        let msg = match obj.get("message") {
            Some(PyValue::Dict(m)) => m,
            _ => obj,
        };

        // A new assistant record with text fires the live callback.
        if matches!(msg.get("role"), Some(PyValue::Str(r)) if r == "assistant") {
            let content = msg.get("content");
            if let Some(PyValue::Str(record_uuid)) = obj.get("uuid") {
                if !record_uuid.is_empty() {
                    let seen = self.emitted_assistant_uuids.entry(path.to_string()).or_default();
                    if !self.initial_sweep_done {
                        // Seed only records actually consumed by the initial sweep. A second full scan can see
                        // newly appended records beyond the offset and wrongly suppress their live callbacks.
                        seen.insert(record_uuid.clone());
                    } else if !seen.contains(record_uuid) && self.has_text_callback {
                        if let Some(PyValue::List(blocks)) = content {
                            for block in blocks {
                                let PyValue::Dict(block) = block else { continue };
                                if !matches!(block.get("type"), Some(PyValue::Str(t)) if t == "text") {
                                    continue;
                                }
                                let Some(PyValue::Str(text)) = block.get("text") else { continue };
                                let text = py::py_strip(text);
                                if text.is_empty() {
                                    continue;
                                }
                                seen.insert(record_uuid.clone());
                                self.pending_assistant_emits.push((path.to_string(), text.to_string(), record_uuid.clone()));
                                break;
                            }
                        }
                    }
                }
            }
        }

        let Some(PyValue::Dict(usage)) = msg.get("usage") else {
            return Ok(());
        };
        let (out, cost) = match usage_tokens_and_cost(msg.get("model"), usage) {
            Ok(Some(pair)) => pair,
            Ok(None) => return Ok(()),
            Err(()) => {
                // A foreign/torn record's metadata must not prevent subsequent complete records in this file
                // from being counted.
                tracing::debug!("invalid transcript usage metadata in {path}");
                return Ok(());
            }
        };
        let t = self.tokens_per_file.entry(path.to_string()).or_insert(0);
        *t = t.saturating_add(out);
        *self.cost_per_file.entry(path.to_string()).or_insert(0.0) += cost;
        // Only attribute to today's counter if the record's own timestamp falls within the current local day.
        // Records without a timestamp contribute to cumulative only.
        if record_is_today_in(self.zone.as_ref(), obj.get("timestamp"), current_day)? {
            let t = self.today_tokens_per_file.entry(path.to_string()).or_insert(0);
            *t = t.saturating_add(out);
            *self.today_cost_per_file.entry(path.to_string()).or_insert(0.0) += cost;
        }
        Ok(())
    }
}

/// `int(usage.get("output_tokens") or 0)`, then `pricing.estimate_cost(msg.get("model") or "", usage)`.
/// `Ok(None)` for a zero count; `Err(())` where the Python raises one of the caught exceptions
/// (`ValueError`, `TypeError`, `AttributeError`, `OverflowError`).
fn usage_tokens_and_cost(model: Option<&PyValue>, usage: &PyDict) -> Result<Option<(i64, f64)>, ()> {
    let int_of = |key: &str, from: &PyDict| py::int_or_zero(from.get(key)).map_err(|_| ());
    let out = int_of("output_tokens", usage)?;
    if out.value == 0 {
        return Ok(None);
    }
    // `model or ""` must be a str: `.lower()` on anything else raises AttributeError.
    let model = match model {
        Some(m) if m.truthy() => m.as_str().ok_or(())?,
        _ => "",
    };
    let inp = int_of("input_tokens", usage)?;
    let cache_read = int_of("cache_read_input_tokens", usage)?;
    // `usage.get("cache_creation") or {}`: anything truthy but a dict has no `.get`.
    let empty = PyDict::new();
    let breakdown = match usage.get("cache_creation") {
        Some(v) if v.truthy() => v.as_dict().ok_or(())?,
        _ => &empty,
    };
    let cw5 = int_of("ephemeral_5m_input_tokens", breakdown)?;
    let cw1h = int_of("ephemeral_1h_input_tokens", breakdown)?;
    let flat = if cw5.value == 0 && cw1h.value == 0 { int_of("cache_creation_input_tokens", usage)? } else { py::PyInt::small(0) };
    // `n * rate` converts the int to a float first, which raises OverflowError past the float range.
    if [inp, out, cache_read, cw5, cw1h, flat].iter().any(|i| i.float_overflows()) {
        return Err(());
    }
    // Every field is now a plain integer, so eve-pricing reads exactly the values Python's int() produced.
    let normalized: Value = json!({
        "input_tokens": inp.value,
        "output_tokens": out.value,
        "cache_read_input_tokens": cache_read.value,
        "cache_creation": {
            "ephemeral_5m_input_tokens": cw5.value,
            "ephemeral_1h_input_tokens": cw1h.value,
        },
        "cache_creation_input_tokens": flat.value,
    });
    Ok(Some((out.value, eve_pricing::estimate_cost(model, &normalized))))
}

/// `Path(root).rglob("*.jsonl")` as CPython 3.13 walks it: the root's matches first, then a stack of
/// directories (last pushed, first scanned), yielding each subdirectory's matches as the subdirectory is met.
/// Symlinked directories are not descended; entries of any kind whose name ends in `.jsonl` match;
/// unreadable directories are skipped.
pub fn rglob_jsonl(root: &Path, visit: &mut dyn FnMut(&Path)) {
    fn scandir(dir: &Path) -> Option<Vec<std::fs::DirEntry>> {
        std::fs::read_dir(dir).ok().map(|it| it.filter_map(Result::ok).collect())
    }
    fn select_wildcard(dir: &Path, visit: &mut dyn FnMut(&Path)) {
        for entry in scandir(dir).unwrap_or_default() {
            let name = entry.file_name();
            if name.as_encoded_bytes().ends_with(b".jsonl") {
                visit(&entry.path());
            }
        }
    }
    select_wildcard(root, visit);
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Some(entries) = scandir(&dir) else { continue };
        for entry in entries {
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                let p = entry.path();
                select_wildcard(&p, visit);
                stack.push(p);
            }
        }
    }
}

/// `_today_key()`: today's local date as `YYYY-MM-DD`.
pub fn today_key() -> Result<String, TimeError> {
    today_key_in(&SystemZone)
}

/// `_today_key()` against an explicit zone.
pub fn today_key_in(zone: &dyn LocalZone) -> Result<String, TimeError> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    isotime::local_day_of_timestamp(zone, now).map(isotime::format_day)
}

/// `_record_is_today(ts, current_day)`: parse an ISO 8601 timestamp (optionally Z-suffixed) and return true if
/// it falls on the current local day. Claude Code writes timestamps in UTC with a 'Z' suffix; we convert to
/// local time before comparing. `Err` where Python's `astimezone()` raises (it is not caught there).
pub fn record_is_today(ts: Option<&PyValue>, current_day: &str) -> Result<bool, TimeError> {
    record_is_today_in(&SystemZone, ts, current_day)
}

/// `_record_is_today` against an explicit zone.
pub fn record_is_today_in(zone: &dyn LocalZone, ts: Option<&PyValue>, current_day: &str) -> Result<bool, TimeError> {
    let Some(PyValue::Str(ts)) = ts else {
        return Ok(false);
    };
    if ts.is_empty() {
        return Ok(false);
    }
    // fromisoformat doesn't accept a trailing 'Z' until 3.11; normalize.
    let Some(dt) = isotime::fromisoformat(&ts.replace('Z', "+00:00")) else {
        return Ok(false);
    };
    Ok(isotime::format_day(isotime::astimezone_date(zone, &dt)?) == current_day)
}

#[cfg(test)]
mod tests;
