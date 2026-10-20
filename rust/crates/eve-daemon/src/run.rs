//! `run()` and shutdown, the memory facade and the dream, closing the open conversations, draining the background
//! work, and the memory bus with its sinks (rosbridge, claude-mem) and services (remember, recall).
//!
//! The parts the Python tests drive on a stub daemon are free functions here too ([`build_memory_with`],
//! [`dream_loop`], [`close_conversations`], [`drain_jobs`], [`voice_memory_with`], [`start_sinks`],
//! [`stop_sinks`], [`observation_event`], [`lesson_event`], [`remember`], [`recall`]): the [`Daemon`] methods
//! call them with its own pieces, and a test calls them with its own.

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Weak};
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use crate::daemon::Daemon;
use crate::seams::{Environ, Inlet, Job, Learning, Obj};
use crate::{CLOSE_MARKERS_SECS, DRAIN_SECS, TODAY_BUILD_SECS};

use eve_claude_mem::{ClaudeMemMirror, ClaudeMemSink, MirrorOptions, SinkOptions};
use eve_memory::{MeaningIndex, Memory, MemoryOptions};
use eve_memory_bus::{BusConfig, MemoryBus};
use eve_mem0_memory::OwnerMemory;
use eve_records::{DreamClient, RecallConfig};
use eve_rosbridge::RosbridgeServer;
use eve_transcripts::{Append, TranscriptConfig, Transcripts, Today};

// ---- small Python helpers ------------------------------------------------------------------------------------

/// `time.time()`.
fn now_secs() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// Python's truthiness of a JSON value (`x or ""`).
fn truthy(v: Option<&Value>) -> bool {
    v.is_some_and(eve_transcripts::py::truthy)
}

/// `str(v)` of a JSON value as Python prints it.
fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::String(s) => s.clone(),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => i.to_string(),
            (None, Some(u), _) => u.to_string(),
            (_, _, Some(f)) => eve_transcripts::py::float_repr(f),
            _ => n.to_string(),
        },
        other => other.to_string(),
    }
}

/// `str(v or "")`.
fn str_or_empty(v: Option<&Value>) -> String {
    match v {
        Some(v) if truthy(Some(v)) => py_str(v),
        _ => String::new(),
    }
}

/// `" ".join(text.split())[:n]`.
fn squeeze_head(text: &str, n: usize) -> String {
    let flat = eve_transcripts::py::flat(text);
    eve_transcripts::py::head(&flat, n).to_string()
}

/// `int(v)` as Python reads a bus argument: an int, a float cut toward zero, a bool, a numeric string (sign,
/// surrounding whitespace, single underscores between digits); None for what `int()` raises on.
fn py_int(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Number(n) => n.as_i64().or_else(|| {
            let x = n.as_f64()?;
            x.is_finite().then(|| x.trunc() as i64)
        }),
        Value::String(s) => {
            let s = eve_transcripts::py::strip(s);
            let (neg, body) = match s.chars().next() {
                Some('-') => (true, &s[1..]),
                Some('+') => (false, &s[1..]),
                _ => (false, s),
            };
            if body.is_empty() || body.starts_with('_') || body.ends_with('_') || body.contains("__") {
                return None;
            }
            let digits: String = body.chars().filter(|c| *c != '_').collect();
            let n = eve_transcripts::py::int(&digits)?;
            Some(if neg { -n } else { n })
        }
        _ => None,
    }
}

/// `f"{type(e).__name__}: {e}"` for an OSError: `OSError: [Errno 48] Address already in use`.
fn os_error_text(e: &io::Error) -> String {
    let name = eve_transcripts::py::os_error_name(e);
    let text = match e.raw_os_error() {
        Some(n) => {
            let shown = e.to_string();
            let msg = shown.strip_suffix(&format!(" (os error {n})")).unwrap_or(&shown).to_string();
            format!("[Errno {n}] {msg}")
        }
        None => e.to_string(),
    };
    format!("{name}: {text}")
}

/// `type(e).__name__` of what a worker thread raised (a panic names itself).
fn join_error_name(e: &tokio::task::JoinError) -> &'static str {
    if e.is_cancelled() {
        "CancelledError"
    } else {
        "panic"
    }
}

// ---- the mem0 index as the facade and the dream hold it -------------------------------------------------------

/// mem0's `OwnerMemory` as the memory facade searches, finds and forgets through it (Python duck-typed the
/// index; the Rust facade takes [`MeaningIndex`]).
pub struct Mem0Index(pub Arc<OwnerMemory>);

impl MeaningIndex for Mem0Index {
    fn search(&self, query: &str) -> anyhow::Result<Vec<Value>> {
        Ok(self.0.search(query, eve_mem0_memory::MAX_HITS))
    }

    fn find(&self, pred: &dyn Fn(&str) -> bool) -> anyhow::Result<Vec<Value>> {
        Ok(self.0.find(pred).into_iter().map(|f| json!({"id": f.id, "text": f.text})).collect())
    }

    fn forget(&self, ids: &[String]) -> anyhow::Result<usize> {
        self.0.forget(ids).map_err(anyhow::Error::new)
    }

    fn forget_matching(&self, pred: &dyn Fn(&str) -> bool) -> Option<anyhow::Result<usize>> {
        Some(self.0.forget_matching(pred).map_err(anyhow::Error::new))
    }
}

/// Which mem0 index each built memory holds, so the dream is handed the same object (`memory.index`) the facade
/// searches: the facade keeps it as a [`MeaningIndex`], the dream takes it as a [`eve_dream::DreamIndex`].
/// A built memory (weakly) and its mem0 index.
type Mem0Entry = (Weak<Memory>, Arc<OwnerMemory>);

static MEM0_OF: LazyLock<Mutex<Vec<Mem0Entry>>> = LazyLock::new(|| Mutex::new(Vec::new()));

fn remember_mem0(memory: &Arc<Memory>, index: Arc<OwnerMemory>) {
    let mut all = MEM0_OF.lock();
    all.retain(|(m, _)| m.strong_count() > 0);
    all.push((Arc::downgrade(memory), index));
}

/// The mem0 index `memory` was built with, as the dream takes it; None when it was built without one.
pub fn mem0_of(memory: &Arc<Memory>) -> Option<Arc<OwnerMemory>> {
    memory.index.as_ref()?;
    let all = MEM0_OF.lock();
    all.iter().find(|(m, _)| m.as_ptr() == Arc::as_ptr(memory)).map(|(_, idx)| idx.clone())
}

// ---- building the memory --------------------------------------------------------------------------------------

/// `memory.migrate(cfg, legacy_store, legacy_mem0)`.
pub type MigrateFn = Arc<dyn Fn(&RecallConfig, &Path, &Path) -> io::Result<Value> + Send + Sync>;
/// `records.ensure_repo(repo)`.
pub type EnsureRepoFn = Arc<dyn Fn(&Path) -> bool + Send + Sync>;
/// `transcripts.Transcripts(config, archive=archive)`.
pub type TranscriptsFn = Arc<dyn Fn(TranscriptConfig, Option<PathBuf>) -> Transcripts + Send + Sync>;
/// `mem0_memory.shared(cfg)`.
pub type Mem0Fn = Arc<dyn Fn(&RecallConfig, Option<&Environ>) -> Option<Arc<OwnerMemory>> + Send + Sync>;

/// What `_build_memory` opens, in the order it opens them: the module functions Python's tests patch to record
/// that order. [`MemoryOpeners::default`] is the real ones.
#[derive(Clone)]
pub struct MemoryOpeners {
    pub migrate: MigrateFn,
    pub ensure_repo: EnsureRepoFn,
    pub transcripts: TranscriptsFn,
    pub mem0: Mem0Fn,
}

impl Default for MemoryOpeners {
    fn default() -> Self {
        MemoryOpeners {
            migrate: Arc::new(eve_memory::migrate),
            ensure_repo: Arc::new(eve_records::records::ensure_repo),
            transcripts: Arc::new(|config, archive| Transcripts::new(config, archive, None)),
            mem0: Arc::new(eve_mem0_memory::shared),
        }
    }
}

/// `_build_memory()`: the memory stack, or None while memory is off. Synchronous (renames and a few git calls):
/// the daemon calls it on a worker thread.
///
/// The move comes first and only with memory on (owner, 2026-09-23): with memory off nothing under
/// ~/.config/eve-bridge is touched. After the move the records folder is a repository of its own again (the old
/// one went to the archive), sealed so it can never be pushed.
pub fn build_memory_with(cfg: &RecallConfig, environ: Option<&Environ>, openers: &MemoryOpeners) -> Option<Arc<Memory>> {
    if !eve_memory::enabled(environ) {
        tracing::info!("memory: off (EVE_MEMORY) — nothing said is kept, and prompts carry no memory");
        return None;
    }
    let legacy_store = eve_records::legacy_store(environ);
    let legacy_mem0 = eve_records::legacy_mem0();
    if let Err(e) = (openers.migrate)(cfg, &legacy_store, &legacy_mem0) {
        // a failed move leaves the old store where it was
        tracing::warn!(
            "memory: the move to the memory folder failed ({}) — the old store is untouched",
            eve_transcripts::py::os_error_name(&e)
        );
    }
    (openers.ensure_repo)(&cfg.records_dir());
    let transcripts = Arc::new((openers.transcripts)(eve_transcripts::configured(environ), Some(cfg.archive_dir())));
    let index = (openers.mem0)(cfg, environ);
    tracing::info!("memory: on — transcripts, records{}", if index.is_some() { ", mem0 index" } else { " (no mem0 index)" });
    if let Some(idx) = &index {
        if eve_mem0_memory::embedder_of(environ) == "gemma" {
            // the local embedder loads in ~3 s; loaded now, the first search is not the one that waits
            let idx = idx.clone();
            let _ = std::thread::Builder::new().name("mem0-warm".into()).spawn(move || idx.warm());
        }
    }
    let facade_index: Option<Arc<dyn MeaningIndex>> =
        index.clone().map(|i| Arc::new(Mem0Index(i)) as Arc<dyn MeaningIndex>);
    // only the daemon runs the index worker
    let opts = MemoryOptions { background: true, environ: environ.cloned(), ..MemoryOptions::default() };
    let memory = Arc::new(Memory::with_options(cfg.clone(), transcripts, facade_index, opts));
    if let Some(idx) = index {
        remember_mem0(&memory, idx);
    }
    Some(memory)
}

// ---- the dream ---------------------------------------------------------------------------------------------

/// The nightly dream's loop over `memory` with `client`, until `shutdown` is set; None with no memory or no
/// client. `index` is the mem0 index the dream feeds (the memory's own, [`mem0_of`]).
pub fn dream_loop(
    memory: Option<&Arc<Memory>>,
    cfg: &RecallConfig,
    client: Option<Arc<dyn DreamClient + Send + Sync>>,
    index: Option<Arc<dyn eve_dream::DreamIndex>>,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> Option<impl Future<Output = ()> + Send + 'static> {
    let memory = memory?;
    let client = client?;
    let dreamer = eve_dream::Dreamer::new(cfg.clone(), memory.transcripts.clone(), client, index, None);
    Some(async move { dreamer.run_loop(shutdown, eve_dream::LOOP_INTERVAL_SECS).await })
}

// ---- closing up ----------------------------------------------------------------------------------------------

/// The close markers for `open_convs` ((channel, conv) pairs): a conversation with lines and no close gets one;
/// one that wrote nothing gets nothing. → how many were closed.
pub fn close_conversations(store: &Transcripts, open_convs: &[(String, String)]) -> usize {
    let mut closed = 0;
    for (ch, conv) in open_convs {
        if conv.is_empty() {
            continue;
        }
        let lines = store.conv_lines(conv);
        if !lines.is_empty() && !lines.iter().any(|ln| ln.get("kind").and_then(Value::as_str) == Some("close")) {
            closed += usize::from(store.close(ch, conv, None));
        }
    }
    closed
}

/// `_drain_background()` over `jobs`: wait up to `secs` for them to finish, then cancel whatever is left.
pub async fn drain_jobs(jobs: Vec<Job>, secs: f64) {
    let pending: Vec<Job> = jobs.into_iter().filter(|j| !j.done()).collect();
    if pending.is_empty() {
        return;
    }
    let all = futures::future::join_all(pending.iter().map(Job::join));
    let _ = tokio::time::timeout(Duration::from_secs_f64(secs.max(0.0)), all).await;
    let still: Vec<&Job> = pending.iter().filter(|j| !j.done()).collect();
    for t in &still {
        t.cancel();
    }
    if !still.is_empty() {
        futures::future::join_all(still.iter().map(|j| j.join())).await;
        tracing::warn!("shutdown: {} background task(s) cut off after {:.0} s", still.len(), secs);
    }
}

// ---- a voice conversation's memory ---------------------------------------------------------------------------

/// What a voice conversation opens with: today's lines for the Live prompt and for the backend (`memory.today`).
pub type TodayBuild = Arc<dyn Fn(&Memory) -> (String, String) + Send + Sync>;

/// The real [`TodayBuild`]: `(memory.today(VOICE_CHARS, style="voice"), memory.today(BACKEND_CHARS))`.
pub fn today_blocks() -> TodayBuild {
    Arc::new(|memory: &Memory| {
        let voice = Today { style: "voice".into(), ..Today::default() };
        (
            memory.today(eve_transcripts::transcripts::VOICE_CHARS as i64, &voice),
            memory.today(eve_transcripts::transcripts::BACKEND_CHARS as i64, &Today::default()),
        )
    })
}

/// `_voice_memory(memory)`: (conv, today, backend_today) for a new voice conversation. The two today blocks are
/// built on a worker thread within `budget` seconds: a slow disk opens the conversation without them rather than
/// holding the owner's first word.
pub async fn voice_memory_with(memory: Option<Arc<Memory>>, budget: f64, build: TodayBuild) -> (String, String, String) {
    let Some(memory) = memory else {
        return (String::new(), String::new(), String::new());
    };
    let conv = memory.transcripts.new_conv("voice");
    let worker = tokio::task::spawn_blocking(move || build(&memory));
    let (today, backend_today) = match tokio::time::timeout(Duration::from_secs_f64(budget.max(0.0)), worker).await {
        Ok(Ok(blocks)) => blocks,
        Ok(Err(e)) => {
            tracing::warn!("memory: today's lines unavailable ({})", join_error_name(&e));
            (String::new(), String::new())
        }
        Err(_) => {
            tracing::info!(
                "memory: today's lines took over {:.0} ms — this conversation opens without them",
                budget * 1000.0
            );
            (String::new(), String::new())
        }
    };
    (conv, today, backend_today)
}

// ---- the memory bus and its sinks ----------------------------------------------------------------------------

/// The opt-in sinks that started: the rosbridge server, the claude-mem sink and its mirror.
#[derive(Default)]
pub struct Sinks {
    pub rosbridge: Option<Arc<RosbridgeServer>>,
    pub claude_mem_sink: Option<ClaudeMemSink<MemoryBus>>,
    pub claude_mem_mirror: Option<ClaudeMemMirror<MemoryBus>>,
}

/// The sinks `cfg` (memory_bus.configured) turns on, started on `bus`. A sink that cannot start is logged and
/// skipped: the daemon boots the same with the flags on and the port busy. Logs the one boot line.
pub async fn start_sinks(bus: &MemoryBus, cfg: &BusConfig) -> Sinks {
    let mut sinks = Sinks::default();
    let mut rosbridge_url = "off".to_string();
    let mut claude_url = "off".to_string();
    if cfg.rosbridge {
        let server = RosbridgeServer::new(bus.clone(), &cfg.rosbridge_host, cfg.rosbridge_port, eve_rosbridge::DEFAULT_QUEUE_SIZE);
        match server.start().await {
            Ok(()) => {
                rosbridge_url = server.url();
                sinks.rosbridge = Some(Arc::new(server));
            }
            Err(e) => tracing::warn!("memory bus: rosbridge did not start ({})", os_error_text(&e)),
        }
    }
    if cfg.claude_mem {
        let shared = Arc::new(bus.clone());
        let mut sink = ClaudeMemSink::new(
            shared.clone(),
            SinkOptions { project: "eve".into(), url: cfg.claude_mem_url.clone(), ..SinkOptions::default() },
        );
        sink.start();
        claude_url = match cfg.claude_mem_url.as_deref() {
            Some(u) if !u.is_empty() => u.to_string(),
            _ => eve_claude_mem::worker_url(), // the sink's own url
        };
        sinks.claude_mem_sink = Some(sink);
        if cfg.mirror {
            let mut mirror =
                ClaudeMemMirror::new(shared, MirrorOptions { url: cfg.claude_mem_url.clone(), ..MirrorOptions::default() });
            mirror.start();
            sinks.claude_mem_mirror = Some(mirror);
        }
    }
    tracing::info!(
        "memory bus: rosbridge {}, claude-mem {}, mirror {}",
        rosbridge_url,
        claude_url,
        if sinks.claude_mem_mirror.is_some() { "on" } else { "off" }
    );
    sinks
}

/// The claude-mem threads' `stop(wait=2.0)`.
const SINK_STOP_WAIT_SECS: f64 = 2.0;

/// `_stop_memory_sinks()` over `sinks`: the mirror, the sink, then the server.
pub async fn stop_sinks(sinks: Sinks) {
    let Sinks { rosbridge, claude_mem_sink, claude_mem_mirror } = sinks;
    if let Some(mut mirror) = claude_mem_mirror {
        mirror.stop(Duration::from_secs_f64(SINK_STOP_WAIT_SECS)).await;
    }
    if let Some(mut sink) = claude_mem_sink {
        sink.stop(Duration::from_secs_f64(SINK_STOP_WAIT_SECS)).await;
    }
    if let Some(server) = rosbridge {
        server.stop().await;
    }
}

/// `/eve/memory/observation`: a diary thought the diary KEPT. Never an unwritten candidate, never a photo.
/// `now` stands in for a record with no time.
pub fn observation_event(rec: &eve_diary::Record, now: f64) -> Obj {
    let mut m = Map::new();
    m.insert("thought".into(), json!(rec.thought));
    m.insert("observations".into(), json!(rec.observations));
    m.insert("changed".into(), json!(rec.changed));
    m.insert("tags".into(), json!(rec.tags));
    m.insert("importance".into(), json!(rec.importance));
    m.insert("novelty".into(), json!(rec.novelty));
    m.insert("time".into(), json!(if rec.ts != 0.0 { rec.ts } else { now }));
    m
}

/// `/eve/memory/lesson`: a lesson moved on. Only the action, the stage, the lesson's topic and level, and eve's
/// OWN feedback: the learner's ideas, strokes, images and spoken words never leave the lesson store. `summary` is
/// the active lesson's summary (None when there is none).
pub fn lesson_event(summary: Option<&Obj>, action: &str, stage: &str, feedback: &str, now: f64) -> Obj {
    let (mut topic, mut level, mut mode, mut lesson_id) = (String::new(), String::new(), String::new(), String::new());
    let mut stage = stage.to_string();
    if let Some(summary) = summary {
        topic = str_or_empty(summary.get("topic"));
        level = str_or_empty(summary.get("level"));
        mode = str_or_empty(summary.get("mode"));
        lesson_id = str_or_empty(summary.get("id"));
        if stage.is_empty() {
            stage = str_or_empty(summary.get("stage"));
        }
    }
    let mut m = Map::new();
    m.insert("action".into(), json!(if action.is_empty() { "notice" } else { action }));
    m.insert("stage".into(), json!(stage));
    m.insert("topic".into(), json!(topic));
    m.insert("level".into(), json!(level));
    m.insert("mode".into(), json!(mode));
    m.insert("lesson_id".into(), json!(lesson_id));
    m.insert("feedback".into(), json!(eve_transcripts::py::flat(feedback)));
    m.insert("time".into(), json!(now));
    m
}

/// `_on_remember(msg)`: someone on the bus asked eve to keep a line. eve never stars for itself, and a bus line
/// is not something the owner said: it becomes one transcript line (channel "bus", a tool line) for the nightly
/// dream to judge, beside everything else that day. With memory off it is dropped.
pub fn remember(memory: Option<&Memory>, msg: &Obj) {
    let text = squeeze_head(&str_or_empty(msg.get("text")), 500);
    if text.is_empty() {
        return;
    }
    let Some(memory) = memory else {
        tracing::info!("memory bus: remember dropped — memory is off");
        return;
    };
    let title = squeeze_head(&str_or_empty(msg.get("title")), 80);
    let line = if title.is_empty() { text } else { format!("{title}: {text}") };
    let store = &memory.transcripts;
    let bus = eve_transcripts::transcripts::BUS;
    let conv = store.new_conv(bus);
    let kept = store.append_with(bus, &conv, "system", "tool", &line, Append { tool: "remember".into(), ..Append::default() });
    if kept {
        store.close(bus, &conv, None);
    }
    tracing::info!("memory bus: remember -> {}", if kept { "a transcript line" } else { "not written" });
}

/// `_recall_service(args)`: /eve/memory/recall, a search of the owner's claude-mem for eve's memories.
/// `claude_mem_on`: the sink is running; `url`: the configured worker (None asks claude-mem where it is).
pub async fn recall(claude_mem_on: bool, args: &Obj, url: Option<&str>) -> Obj {
    let mut out = Map::new();
    if !claude_mem_on {
        out.insert("results".into(), json!([]));
        out.insert("error".into(), json!("claude-mem is off (EVE_CLAUDE_MEM=1)"));
        return out;
    }
    let query = squeeze_head(&str_or_empty(args.get("query")), 500);
    let limit = match args.get("limit") {
        None => Some(5),
        Some(v) => py_int(v),
    }
    .map_or(5, |n| n.clamp(1, 20));
    let results = eve_claude_mem::recall(&query, limit, url, Duration::from_secs_f64(5.0), "eve").await;
    out.insert("results".into(), serde_json::to_value(results).unwrap_or_else(|_| json!([])));
    out
}

// ---- the daemon ----------------------------------------------------------------------------------------------

/// EVE_LEARNING's off words.
fn off_word(v: &str) -> bool {
    matches!(eve_transcripts::py::strip(v).to_lowercase().as_str(), "0" | "false" | "no" | "off")
}

impl Daemon {
    /// `run()`: everything the daemon does until shutdown: the IPC server, the link, the tailer, the heartbeat,
    /// the poller, the ears, the explorer, the memory, the dream, the text door, the Mini App, the learning
    /// server, the bus's sinks, the update check; then the orderly close.
    pub async fn run(self: &Arc<Self>) -> anyhow::Result<()> {
        crate::startup::log_permission_config_summary(&self.matchers);
        // Memory first: the one-time move into the memory folder must run before anything opens a store,
        // and the IPC server below can already write meeting notes into it.
        self.open_memory().await;
        // The voice SDK's resources package imports in 12–20 s inside this process (GIL contention with vision and
        // the keyword spotter). Pay it now, off the loop, so the first "hey eve" does not (voice_agent.warm_live_import).
        let _ = std::thread::Builder::new()
            .name("warm-voice-sdk".into())
            .spawn(eve_brain::voice_agent::warm_live_import);
        if let Some(ipc) = &self.ipc {
            ipc.start().await?;
        }
        let handle = tokio::runtime::Handle::current();
        if !off_word(&self.env("EVE_LEARNING").unwrap_or_else(|| "1".into())) {
            self.start_learning(&handle);
        }
        self.bus.bind_loop(handle.clone());
        self.start_memory_sinks().await;
        let weak = self.me.clone();
        let listen_stop = eve_listen_key::start_listen_key(
            move |on| {
                if let Some(d) = weak.upgrade() {
                    d.on_listen_key(on);
                }
            },
            &handle,
            None,
        );
        self.st.lock().listen_stop = listen_stop;
        self.start_ears(&handle);
        self.vision.set_detect(eve_vision::make_detector());
        let identity = self.make_identity();
        self.st.lock().identity = identity.clone();
        self.vision.set_identity(identity.map(|i| i as Arc<dyn eve_vision::Identity>));
        self.scene.set_client(eve_scene::make_scene_client(self.scene.config(), self.environ()));
        let intent = eve_intent::make_intent_classifier(self.environ());
        // The slow brain behind the voice (think.py): high-effort reasoning with web search, for the backend's
        // think_hard tool.
        let thinker = eve_brain::think::make_thinker(
            &eve_brain::think::configured(self.environ(), &self.voice_cfg.backend_model),
            self.environ(),
        );
        {
            let mut st = self.st.lock();
            st.intent = intent;
            st.thinker = thinker;
        }
        {
            let sound = self.sound.lock();
            if sound.muted() {
                tracing::info!(
                    "sound: muted (owner's choice, {}) — the head and lights still move",
                    sound.path.display()
                );
            }
        }
        let mut tasks: Vec<Job> = Vec::new();
        let expressions = self.expressions.clone();
        tasks.push(Job::spawn("laya-expressions", async move { expressions.run().await }));
        let me = self.clone();
        tasks.push(Job::spawn("ipc", async move {
            if let Some(ipc) = &me.ipc {
                if let Err(e) = ipc.serve_forever().await {
                    tracing::debug!("ipc: stopped ({e})");
                }
            }
        }));
        let link = self.ble.clone();
        tasks.push(Job::spawn("ble", async move { link.run().await }));
        if let Some(controller) = self.controller.clone() {
            tasks.push(Job::spawn("controller", async move { controller.run().await }));
        }
        if let Some(jsonl) = self.jsonl.clone() {
            tasks.push(Job::spawn("jsonl", async move {
                if let Err(e) = jsonl.run().await {
                    tracing::debug!("jsonl: stopped ({e})");
                }
            }));
        }
        let me = self.clone();
        tasks.push(Job::spawn("heartbeat", async move { me.heartbeat_loop().await }));
        let me = self.clone();
        tasks.push(Job::spawn("on-connect", async move { me.on_ble_connected().await }));
        let me = self.clone();
        tasks.push(Job::spawn("status-poller", async move { me.status_poller().await }));
        let me = self.clone();
        tasks.push(Job::spawn("update-check", async move { me.update_check_loop().await }));
        let me = self.clone();
        tasks.push(Job::spawn("vision-stats", async move { me.vision_stats_loop().await }));
        // The diary (diary.py): memory-aware thoughts plus an appraisal the board turns into a feeling. Same
        // take() shape as NoteTaker. The loop always runs so `eve-bridge explore` and "hey eve, go explore"
        // work; EVE_EXPLORE=0 only turns off the idle start.
        let client = eve_diary::make_diary_client(&self.explore_cfg.model, self.environ());
        self.explorer.lock().notes_enabled = client.is_some();
        if let Some(client) = client {
            let taker = self.diary_taker(client);
            self.st.lock().notes = Some(Arc::new(taker));
        }
        let me = self.clone();
        tasks.push(Job::spawn("explore", async move { me.explore_loop().await }));
        let me = self.clone();
        tasks.push(Job::spawn("thought-captions", async move { me.thought_caption_loop().await }));
        // codex_warm.py: one Codex agent started ahead of time, so a hard task's handoff is the turn only.
        let (warm_on, warm_age) = eve_codex_warm::configured();
        crate::agents::warn_removed_computer(self.environ());
        let factory: eve_codex_warm::Factory<eve_codex_computer::CodexComputerAgent> =
            Arc::new(|on_event, ask_user: eve_codex_warm::AskUser| {
                let ask: eve_codex_computer::Ask = Arc::new(move |q: String| {
                    let ask_user = ask_user.clone();
                    Box::pin(async move { Ok(ask_user(q).await) })
                });
                eve_codex_computer::CodexComputerAgent::new(on_event, ask, eve_codex_computer::Options::default())
            });
        let warm = Arc::new(eve_codex_warm::WarmCodex::new(factory, warm_on && self.agent_cfg.enabled, warm_age));
        self.st.lock().codex_warm = Some(warm.clone());
        tasks.push(Job::spawn("codex-warm", async move { warm.refresh_loop_default().await }));
        // The fast path into the owner's logged-in Chrome (chrome_lane.py): ONE attached lane for the daemon's
        // life, so Chrome's "Allow remote debugging?" is answered once per Chrome session (over Telegram).
        let lane = self.make_chrome_lane();
        self.st.lock().chrome_lane = lane;
        let inlet = self.make_telegram();
        if let Some(inlet) = inlet {
            {
                let mut st = self.st.lock();
                st.telegram = Some(inlet.clone() as Arc<dyn Inlet>);
                st.telegram_inlet = Some(inlet.clone());
            }
            let door = inlet.clone();
            tasks.push(Job::spawn("telegram", async move { door.run().await }));
            let _ = self.command_risk(); // one log line at start: the Auto Mode gate's mode
            // The watcher (watch.py): prices, quotes and ticket releases on a schedule, texted through the door.
            // Its notify (the door's tell_owner) is lent when it is made (_make_telegram): the Rust watcher takes it
            // at construction.
            let watcher = self.st.lock().watcher.clone();
            if let Some(watcher) = watcher {
                tasks.push(Job::spawn("watch", async move { watcher.run().await }));
            }
            // The chief of staff (chief.py), when EVE_CHIEF has it on: its loop, beside the watcher's. It sends
            // through the door's own tell_owner (chief_notify), built with it in _make_telegram.
            if let Some(chief) = Inlet::chief(&*inlet) {
                tasks.push(Job::spawn("chief", async move { chief.run().await }));
            }
            // The Meet notetaker (meet.py) texts through the same door (title MEET_TITLE): in the call, the lobby,
            // the notes. The Rust Meeter takes that notify when it is made (_make_meeter).
        }
        // The "Ask Claude" Mini App (miniapp.py): the owner's menu button opens a Claude chat inside Telegram,
        // served here through a Cloudflare quick tunnel. Off unless EVE_MINIAPP=1.
        self.start_miniapp(&mut tasks);
        // Updates (updates.py, docs/updates.md): the Eve Link pane's cards, rebuilt in the background every
        // EVE_UPDATES_EVERY_MIN in day time and once at start when stale. Only while the app's door is up;
        // EVE_UPDATES=0 turns it off.
        let miniapp = self.st.lock().miniapp.clone();
        let link_chat = miniapp.and_then(|m| m.server.link.lock().clone());
        if let Some(link_chat) = link_chat {
            let updates = link_chat.updates().clone();
            if updates.enabled() {
                let rx = self.shutdown_rx();
                tasks.push(Job::spawn("updates", async move {
                    updates.run_loop(rx, eve_link::updates::TICK_SECS).await
                }));
            }
        }
        // The providers' own spend figures (spend_sync.py): OpenRouter always, OpenAI and Anthropic with an admin
        // key; at start and hourly, on a worker thread. The Spending view shows them beside eve's own meter.
        let rx = self.shutdown_rx();
        let env = self.environ().cloned();
        tasks.push(Job::spawn("spend-sync", async move {
            eve_spend_sync::run_loop(rx, Duration::from_secs_f64(eve_spend_sync::SYNC_SECS), env).await
        }));
        // The places history (places.py): hourly, raw fixes and visits past their days go, and home and work are
        // inferred again. Nothing runs while EVE_PLACES is off or nothing has been kept.
        // (eve_places' loop future holds its watch borrow across an await, so it is not Send: it runs on a blocking
        // thread of the same runtime and ends by itself on the shutdown flag.)
        let rx = self.shutdown_rx();
        let places = tokio::task::spawn_blocking(move || {
            tokio::runtime::Handle::current().block_on(eve_places::places::run_loop(rx, Duration::from_secs_f64(3600.0)))
        });
        tasks.push(Job::spawn("places", async move {
            let _ = places.await;
        }));
        // The nightly dream (dream.py): the one automatic writer of the records and the mem0 index. It watches the
        // shutdown event itself and is drained, not cancelled, at the end: a night half written is worse than a
        // restart a few seconds late.
        self.start_dream();
        if !self.explore_cfg.enabled {
            tracing::info!(
                "explore: eve explores only when asked (`eve-bridge explore`, \"go explore\", a text); \
                 EVE_EXPLORE=1 turns the idle start on"
            );
        }
        let mut rx = self.shutdown_rx();
        let _ = rx.wait_for(|set| *set).await;
        self.close_down(tasks).await;
        Ok(())
    }

    /// The learning server (learning/server.py) on EVE_LEARNING_PORT, its notices handed to the loop.
    fn start_learning(self: &Arc<Self>, handle: &tokio::runtime::Handle) {
        let port = self.env("EVE_LEARNING_PORT").unwrap_or_else(|| "48766".into());
        let port = match py_int(&Value::String(port.clone())).and_then(|p| u16::try_from(p).ok()) {
            Some(p) => p,
            None => {
                tracing::error!("learning: could not start the local workspace (ValueError: invalid port {port:?})");
                return;
            }
        };
        let weak = self.me.clone();
        let loop_handle = handle.clone();
        let notify: eve_learning::server::Notify = Arc::new(move |text: &str, stage: &str| {
            // loop.call_soon_threadsafe(self._learning_notice, text, stage)
            let (text, stage, weak) = (text.to_string(), stage.to_string(), weak.clone());
            loop_handle.spawn(async move {
                if let Some(d) = weak.upgrade() {
                    d.learning_notice(&text, &stage);
                }
            });
            Ok(())
        });
        let listener: Arc<dyn eve_learning::Listener> = self.make_listener(handle);
        let opts = eve_learning::server::StartOptions {
            port,
            notify: Some(notify),
            listener: Some(listener),
            ..eve_learning::server::StartOptions::default()
        };
        match eve_learning::server::start(opts) {
            Ok(server) => {
                let server: Arc<dyn Learning> = Arc::new(server);
                tracing::info!("learning: {}", server.url());
                self.st.lock().learning_server = Some(server);
            }
            Err(e) => tracing::error!("learning: could not start the local workspace ({})", os_error_text(&e)),
        }
    }

    /// The diary's note taker, lent the daemon's emote, snapshot, thought screen and memory bus.
    fn diary_taker(self: &Arc<Self>, client: Arc<dyn eve_diary::DiaryClient>) -> eve_diary::DiaryTaker {
        let (w1, w2, w3, w4) = (self.me.clone(), self.me.clone(), self.me.clone(), self.me.clone());
        eve_diary::DiaryTaker::new(client, self.explore_cfg.notes_dir.clone())
            .with_send_emote(Arc::new(move |e| {
                let w = w1.clone();
                Box::pin(async move {
                    if let Some(d) = w.upgrade() {
                        d.send_emote(e).await;
                    }
                    Ok(())
                })
            }))
            .with_snapshot(Arc::new(move || {
                let w = w2.clone();
                Box::pin(async move {
                    match w.upgrade() {
                        Some(d) => Ok(d.take_snapshot().await),
                        None => Ok(None),
                    }
                })
            }))
            .with_on_thought(Arc::new(move |t: &eve_diary::Thought| {
                if let Some(d) = w3.upgrade() {
                    d.show_thought(t);
                }
            }))
            .with_on_written(Arc::new(move |rec: &eve_diary::Record| {
                if let Some(d) = w4.upgrade() {
                    d.publish_observation(rec);
                }
            }))
    }

    /// `run()`'s `finally`: the orderly close.
    async fn close_down(self: &Arc<Self>, tasks: Vec<Job>) {
        // Withdraw purchase consent before any slow device/service teardown yields. A confirmation already sent
        // still journals its outcome.
        let (agentcard, inlet) = {
            let st = self.st.lock();
            (st.agentcard.clone(), st.telegram.clone())
        };
        if let Some(card) = agentcard {
            if let Some(inlet) = &inlet {
                inlet.shutdown_purchases();
            }
            card.close();
        }
        // Leave explore mode and stop the camera before the transport task is cancelled — its reader owns the port
        // close, so a send after that goes nowhere.
        self.stop_explore("daemon stopping").await;
        let audition = self.st.lock().expression_audition.clone();
        if let Some(audition) = audition {
            audition.cancel();
            audition.join().await;
        }
        self.send_cam(false).await;
        self.vision.stop();
        let meeter = self.st.lock().meeter.clone();
        if let Some(meeter) = meeter {
            meeter.close().await; // a call in progress: its notes are written
        }
        self.close_notes().await; // notes in progress, the phone's and the room's
        let music = self.st.lock().music_mode.clone();
        if let Some(music) = music {
            music.close().await;
        }
        // eve's apps that were made: a held Bluetooth light goes back to the phone app, the car stops, the
        // camera's picture ends (re-entrant: the Mini App closes some too)
        self.close_hubs().await;
        let canvas = self.st.lock().canvas.clone().flatten();
        if let Some(canvas) = canvas {
            canvas.close().await;
        }
        let lane = self.st.lock().chrome_lane.clone();
        if let Some(lane) = lane {
            if let Err(e) = lane.close().await {
                // eve's tab only; the owner's Chrome stays
                tracing::debug!("chrome lane: close failed ({e})");
            }
        }
        for t in &tasks {
            t.cancel();
        }
        let pending: Vec<Job> = self.st.lock().pending_turn_ends.values().cloned().collect();
        for pend in &pending {
            if !pend.done() {
                pend.cancel();
            }
        }
        futures::future::join_all(tasks.iter().chain(pending.iter()).map(Job::join)).await;
        self.cancel_active_task("the daemon is restarting").await;
        let conversation = self.st.lock().conversation.clone();
        if let Some(conv) = conversation {
            if !conv.done() {
                conv.cancel();
                conv.join().await;
            }
        }
        let ears = self.st.lock().ears.clone();
        if let Some(ears) = ears {
            ears.stop();
        }
        let listen_stop = self.st.lock().listen_stop.take();
        if let Some(stop) = listen_stop {
            stop();
        }
        self.close_open_conversations().await;
        self.drain_background().await;
        self.ble.stop().await;
        if let Some(ipc) = &self.ipc {
            ipc.stop().await;
        }
        self.stop_memory_sinks().await;
        let server = self.st.lock().learning_server.take();
        if let Some(server) = server {
            let _ = tokio::task::spawn_blocking(move || server.shutdown()).await;
        }
    }

    /// `shutdown()`: set the shutdown flag; `run()` returns once its close is done.
    pub async fn shutdown(&self) {
        self.shutdown.send_replace(true);
    }

    /// `_close_notes()`: notes still recording when the daemon stops are written now, transcript only, so a restart
    /// never loses them: they live only in memory until written (review, 2026-10-04, of notes on the phone: "notes
    /// never lost"). Each gets `phone_call::NOTES_SHUTDOWN_SECS`; the phone's are texted as usual.
    pub async fn close_notes(self: &Arc<Self>) {
        let miniapp = self.st.lock().miniapp.clone();
        let phone = miniapp.and_then(|m| m.server.calls.lock().clone()).and_then(|calls| calls.notes());
        if let Some(phone) = phone {
            if phone.active() {
                // shutting down: best effort
                phone.stop("the daemon stopped", true).await;
            }
        }
        let room = self.st.lock().room_notes.clone();
        if let Some(room) = room {
            if room.active() {
                room.set_quick(true);
                room.request_stop("the daemon stopped");
                if let Some(task) = room.task() {
                    let secs = eve_phone_call::phone_call::NOTES_SHUTDOWN_SECS;
                    if tokio::time::timeout(Duration::from_secs_f64(secs), task).await.is_err() {
                        tracing::warn!("notes: not written within {:.0} s of shutdown", secs);
                    }
                }
            }
        }
    }

    /// `_build_memory()`: the memory facade (records migrated, transcripts, mem0), None while memory is off.
    pub fn build_memory(&self) -> Option<Arc<eve_memory::Memory>> {
        build_memory_with(&self.recall_cfg, self.environ(), &MemoryOpeners::default())
    }

    /// `_open_memory()`: build the memory off the loop and keep it as `memory`.
    pub async fn open_memory(self: &Arc<Self>) {
        let me = self.clone();
        let memory = match tokio::task::spawn_blocking(move || me.build_memory()).await {
            Ok(memory) => memory,
            Err(e) => {
                // memory is a nicety: the daemon runs without it
                tracing::warn!("memory: could not open ({}) — running without memory", join_error_name(&e));
                None
            }
        };
        self.st.lock().memory = memory;
    }

    /// `_start_dream()`: the nightly dream, held in `background` so shutdown drains it instead of cancelling it.
    /// No memory or no model client (records.make_client logs why): no dream.
    pub fn start_dream(self: &Arc<Self>) -> Option<Job> {
        let memory = self.st.lock().memory.clone()?;
        let client = eve_records::make_client(self.environ(), eve_records::records::DEFAULT_MODEL)?;
        let client: Arc<dyn DreamClient + Send + Sync> = Arc::new(client);
        let index = mem0_of(&memory).map(|i| i as Arc<dyn eve_dream::DreamIndex>);
        let fut = dream_loop(Some(&memory), &self.recall_cfg, Some(client), index, self.shutdown_rx())?;
        Some(self.spawn("memory-dream", fut))
    }

    /// `_close_open_conversations()`: a restart must not leave a conversation open in the transcripts: the voice
    /// session and the text inlet write their own close markers on the way out, and this is the backstop for any
    /// that did not (a task cancelled before it ran), within CLOSE_MARKERS_SECS. A conversation that wrote nothing
    /// gets nothing.
    pub async fn close_open_conversations(self: &Arc<Self>) {
        let (memory, inlet, voice_conv) = {
            let st = self.st.lock();
            (st.memory.clone(), st.telegram.clone(), st.voice_conv.clone())
        };
        let Some(memory) = memory else { return };
        let text_conv = inlet.as_ref().and_then(|i| i.conversation()).unwrap_or_default();
        let open_convs = vec![("voice".to_string(), voice_conv), ("telegram".to_string(), text_conv)];
        let store = memory.transcripts.clone();
        let worker = tokio::task::spawn_blocking(move || close_conversations(&store, &open_convs));
        let closed = match tokio::time::timeout(Duration::from_secs_f64(CLOSE_MARKERS_SECS), worker).await {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => {
                tracing::warn!("memory: close markers not written at shutdown ({})", join_error_name(&e));
                return;
            }
            Err(_) => {
                tracing::warn!("memory: close markers not written at shutdown (TimeoutError)");
                return;
            }
        };
        if let Some(inlet) = &inlet {
            if inlet.conversation().is_some() {
                inlet.forget_conversation(); // closed above: a late _close_chat must not close it twice
            }
        }
        self.st.lock().voice_conv = String::new();
        if closed > 0 {
            tracing::info!("memory: {} open conversation(s) closed at shutdown", closed);
        }
    }

    /// `_drain_background()`: wait up to DRAIN_SECS for the background jobs (the nightly dream), then cancel what
    /// is left.
    pub async fn drain_background(&self) {
        drain_jobs(self.background_jobs(), DRAIN_SECS).await;
    }

    /// `_voice_memory(memory)`: (conv, today, backend_today) for a voice conversation's opening: today's lines
    /// when they build within TODAY_BUILD_SECS.
    pub async fn voice_memory(self: &Arc<Self>, memory: Option<Arc<eve_memory::Memory>>) -> (String, String, String) {
        voice_memory_with(memory, TODAY_BUILD_SECS, today_blocks()).await
    }

    /// `_start_memory_sinks()`: the recall service and the remember line on the bus, then the rosbridge server and
    /// the claude-mem sink and mirror, when configured.
    pub async fn start_memory_sinks(self: &Arc<Self>) {
        let weak = self.me.clone();
        self.bus.register_service("/eve/memory/recall", move |args: Obj| {
            let weak = weak.clone();
            async move {
                match weak.upgrade() {
                    Some(d) => Ok(d.recall_service(args).await),
                    None => Err(anyhow::anyhow!("the daemon has stopped")),
                }
            }
        });
        let weak = self.me.clone();
        self.bus.subscribe("/eve/memory/remember", move |_topic: &str, msg: &Obj| {
            if let Some(d) = weak.upgrade() {
                d.on_remember(msg);
            }
        });
        let sinks = start_sinks(&self.bus, &self.bus_cfg).await;
        let mut st = self.st.lock();
        st.rosbridge = sinks.rosbridge;
        st.claude_mem_sink = sinks.claude_mem_sink;
        st.claude_mem_mirror = sinks.claude_mem_mirror;
    }

    /// `_stop_memory_sinks()`.
    pub async fn stop_memory_sinks(self: &Arc<Self>) {
        let sinks = {
            let mut st = self.st.lock();
            Sinks {
                rosbridge: st.rosbridge.take(),
                claude_mem_sink: st.claude_mem_sink.take(),
                claude_mem_mirror: st.claude_mem_mirror.take(),
            }
        };
        stop_sinks(sinks).await;
    }

    /// `_publish_observation(rec)`: a diary thought the diary KEPT, onto the bus. Never an unwritten candidate,
    /// never a photo.
    pub fn publish_observation(&self, rec: &eve_diary::Record) {
        self.bus.publish("/eve/memory/observation", observation_event(rec, now_secs()));
    }

    /// `_publish_lesson(action, stage, feedback)`: a lesson event onto the bus.
    pub fn publish_lesson(&self, action: &str, stage: &str, feedback: &str) {
        let server = self.st.lock().learning_server.clone();
        // a missing lesson is not an error worth a caption
        let summary = server.and_then(|s| s.active_summary());
        self.bus.publish("/eve/memory/lesson", lesson_event(summary.as_ref(), action, stage, feedback, now_secs()));
    }

    /// `_on_remember(msg)`: the bus's "remember" service: a line into the transcripts for the dream.
    pub fn on_remember(&self, msg: &Obj) {
        let memory = self.st.lock().memory.clone();
        remember(memory.as_deref(), msg);
    }

    /// `_recall_service(args)`: the bus's "recall" service: claude-mem's search.
    pub async fn recall_service(self: &Arc<Self>, args: Obj) -> Obj {
        let on = self.st.lock().claude_mem_sink.is_some();
        recall(on, &args, self.bus_cfg.claude_mem_url.as_deref()).await
    }
}
