//! `eve-bridge memory`: the memory bus from a terminal (tail, recall: a rosbridge client) and the memory folder
//! itself (forget, dream, reindex: no daemon needed). cli.py's `_run_memory`, `_open_memory_store` and
//! `_run_memory_store`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Local;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

use eve_dream::{DreamIndex, DreamReport, Dreamer};
use eve_mem0_memory::OwnerMemory;
use eve_memory::Memory;
use eve_records::{DreamClient, RecallConfig};
use eve_transcripts::Transcripts;

use crate::argparse::Namespace;
use crate::io::Io;
use crate::py::{self, get, pstr, truthy};

pub const NOT_REACHABLE: &str =
    "eve-bridge: the memory bus is not reachable at {url}. Start the daemon with EVE_ROSBRIDGE=1 (see docs/memory-bus.md).";

fn not_reachable(url: &str, kind: &str) -> String {
    format!("{} ({kind})", NOT_REACHABLE.replace("{url}", url))
}

/// Why a rosbridge connection failed, as Python names the exception (`type(e).__name__`).
fn ws_error_name(e: &tokio_tungstenite::tungstenite::Error) -> &'static str {
    use tokio_tungstenite::tungstenite::Error as E;
    match e {
        E::Io(io) => match io.kind() {
            std::io::ErrorKind::ConnectionRefused => "ConnectionRefusedError",
            std::io::ErrorKind::ConnectionReset => "ConnectionResetError",
            std::io::ErrorKind::ConnectionAborted => "ConnectionAbortedError",
            std::io::ErrorKind::TimedOut => "TimeoutError",
            std::io::ErrorKind::BrokenPipe => "BrokenPipeError",
            _ => "OSError",
        },
        E::ConnectionClosed | E::AlreadyClosed => "ConnectionClosedOK",
        E::Protocol(_) => "ConnectionClosedError",
        E::Http(_) => "InvalidStatus",
        E::Url(_) => "InvalidURI",
        _ => "OSError",
    }
}

/// A rosbridge failure: either the bus is not reachable (exit 2, as Python's caught OSError/TimeoutError), or a
/// URL that is not one (Python's InvalidURI, which nothing catches: a traceback and exit 1).
enum Fail {
    Unreachable(&'static str),
    Uncaught(String),
}

fn fail_of(e: tokio_tungstenite::tungstenite::Error) -> Fail {
    use tokio_tungstenite::tungstenite::Error as E;
    match e {
        E::Url(u) => Fail::Uncaught(format!("InvalidURI: {u}")),
        other => Fail::Unreachable(ws_error_name(&other)),
    }
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(url: &str) -> Result<Ws, Fail> {
    match tokio::time::timeout(Duration::from_secs(3), tokio_tungstenite::connect_async(url)).await {
        Err(_) => Err(Fail::Unreachable("TimeoutError")),
        Ok(Err(e)) => Err(fail_of(e)),
        Ok(Ok((ws, _))) => Ok(ws),
    }
}

fn text_of(msg: Message) -> Option<String> {
    match msg {
        Message::Text(t) => Some(t.to_string()),
        Message::Binary(b) => Some(String::from_utf8_lossy(&b).into_owned()),
        _ => None,
    }
}

/// The topics `tail` subscribes to: the one named, or every known topic under a prefix ending in `*`.
pub fn tail_topics(topic: &str) -> Vec<String> {
    let Some(prefix) = topic.strip_suffix('*') else {
        return vec![topic.to_string()];
    };
    let known: Vec<String> =
        eve_memory_bus::TOPICS.iter().filter(|(t, _)| t.starts_with(prefix)).map(|(t, _)| t.to_string()).collect();
    if known.is_empty() {
        vec![topic.to_string()]
    } else {
        known
    }
}

fn topic_type(topic: &str) -> &'static str {
    eve_memory_bus::TOPICS.iter().find(|(t, _)| *t == topic).map(|(_, ty)| *ty).unwrap_or("")
}

/// One published message as `tail` prints it: `HH:MM:SS <topic> <body as JSON>`. None for anything else.
pub fn tail_line(raw: &str) -> Option<String> {
    let msg: Value = serde_json::from_str(raw).ok()?;
    if get(&msg, "op") != &json!("publish") {
        return None;
    }
    let body = if truthy(get(&msg, "msg")) { get(&msg, "msg").clone() } else { json!({}) };
    let now = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    let time = get(&body, "time");
    let secs = if truthy(time) {
        match time {
            Value::Number(n) => n.as_f64().unwrap_or_else(now),
            Value::String(s) => crate::argparse::py_float(s).unwrap_or_else(now),
            Value::Bool(b) => f64::from(u8::from(*b)),
            _ => now(),
        }
    } else {
        now()
    };
    let stamp = py::local_strftime(secs, "%H:%M:%S").unwrap_or_default();
    Some(format!("{stamp} {} {}", pstr(get(&msg, "topic")), py::dumps(&body, None, false)))
}

async fn tail(url: &str, topic: &str, io: &Io) -> Result<i32, Fail> {
    let topics = tail_topics(topic);
    let mut ws = connect(url).await?;
    for (i, t) in topics.iter().enumerate() {
        let sub = json!({"op": "subscribe", "id": format!("tail-{i}"), "topic": t, "type": topic_type(t)});
        ws.send(Message::text(py::dumps(&sub, None, true))).await.map_err(fail_of)?;
    }
    io.warn(&format!("memory: listening on {} (Ctrl-C to stop)", topics.join(", ")));
    while let Some(next) = ws.next().await {
        let msg = next.map_err(|e| match e {
            tokio_tungstenite::tungstenite::Error::ConnectionClosed => Fail::Unreachable("ConnectionClosedOK"),
            other => fail_of(other),
        });
        let msg = match msg {
            Ok(m) => m,
            // `async for raw in ws` ends quietly on a normal close; an abnormal one raises ConnectionClosedError,
            // which is not an OSError: Python's traceback.
            Err(Fail::Unreachable("ConnectionClosedOK")) => break,
            Err(Fail::Unreachable(kind)) => return Err(Fail::Uncaught(kind.to_string())),
            Err(other) => return Err(other),
        };
        if let Some(raw) = text_of(msg) {
            if let Some(line) = tail_line(&raw) {
                io.say(&line);
            }
        }
    }
    Ok(0)
}

/// What `recall` prints for one service response, and its exit code. None: not the answer to our call.
pub fn recall_answer(msg: &Value, io: &Io) -> Option<i32> {
    if get(msg, "op") != &json!("service_response") || get(msg, "id") != &json!("recall-1") {
        return None;
    }
    let values = if truthy(get(msg, "values")) { get(msg, "values").clone() } else { json!({}) };
    if !truthy(&py::get_or(msg, "result", json!(true))) || truthy(get(&values, "error")) {
        io.warn(&format!("recall: {}", py::or_str(&values, "error", "the service refused")));
        return Some(1);
    }
    let results = match get(&values, "results") {
        Value::Array(a) => a.clone(),
        _ => Vec::new(),
    };
    if results.is_empty() {
        io.say("recall: nothing found");
    }
    for r in &results {
        let line = format!(
            "#{} {} {}",
            pstr(&py::get_or(r, "id", json!("?"))),
            pstr(&py::get_or(r, "time", json!(""))),
            pstr(&py::get_or(r, "title", json!("")))
        );
        io.say(line.trim_end());
        if truthy(get(r, "text")) {
            io.say(&format!("    {}", pstr(get(r, "text"))));
        }
    }
    Some(0)
}

async fn recall(url: &str, query: &str, limit: i64, io: &Io) -> Result<i32, Fail> {
    let mut ws = connect(url).await?;
    let call = json!({"op": "call_service", "id": "recall-1", "service": "/eve/memory/recall",
                      "args": {"query": query, "limit": limit}});
    ws.send(Message::text(py::dumps(&call, None, true))).await.map_err(fail_of)?;
    let wait = async {
        while let Some(next) = ws.next().await {
            let msg = match next {
                Ok(m) => m,
                Err(tokio_tungstenite::tungstenite::Error::ConnectionClosed) => break,
                Err(e) => return Err(Fail::Uncaught(ws_error_name(&e).to_string())),
            };
            let Some(raw) = text_of(msg) else { continue };
            let msg: Value = serde_json::from_str(&raw).map_err(|e| Fail::Uncaught(format!("JSONDecodeError: {e}")))?;
            if let Some(rc) = recall_answer(&msg, io) {
                return Ok(Some(rc));
            }
        }
        Ok(None)
    };
    match tokio::time::timeout(Duration::from_secs(20), wait).await {
        Err(_) => Err(Fail::Unreachable("TimeoutError")),
        Ok(Err(e)) => Err(e),
        Ok(Ok(Some(rc))) => Ok(rc),
        Ok(Ok(None)) => {
            io.warn("recall: no answer from the service");
            Ok(2)
        }
    }
}

/// `eve-bridge memory tail|recall`: a rosbridge client on the daemon's memory bus.
pub fn run_memory(args: &Namespace, io: &Io) -> i32 {
    let url = args.str("url").unwrap_or_default();
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => return super::report::<i32>(Err(e.into())),
    };
    let result = rt.block_on(async {
        let work = async {
            if args.str("memory_cmd").as_deref() == Some("tail") {
                tail(&url, &args.str("topic").unwrap_or_default(), io).await
            } else {
                recall(&url, &args.str("query").unwrap_or_default(), args.get("limit").int().unwrap_or(5), io).await
            }
        };
        tokio::select! {
            r = work => r,
            _ = tokio::signal::ctrl_c() => Ok(0),   // KeyboardInterrupt: return 0
        }
    });
    match result {
        Ok(rc) => rc,
        Err(Fail::Unreachable(kind)) => {
            io.warn(&not_reachable(&url, kind));
            2
        }
        Err(Fail::Uncaught(why)) => {
            io.warn(&format!("eve-bridge: {why}"));
            1
        }
    }
}

// ---- the memory folder ---------------------------------------------------------------------------------------

/// The mem0 index as reindex and the dream use it (`OwnerMemory`; a fake in tests).
pub trait StoreIndex: Send + Sync {
    /// The index's own folder (`index.home`); None: the config's mem0 folder.
    fn home(&self) -> Option<PathBuf>;
    fn ingest_day(&self, transcripts: &Transcripts, day: &str) -> usize;
    fn tidy(&self) -> usize;
    /// Whether mem0 opened (`index._mem is not None`).
    fn is_open(&self) -> bool;
    /// The same index for the dreamer.
    fn as_dream(&self) -> Arc<dyn DreamIndex>;
}

/// mem0's OwnerMemory as a [`StoreIndex`].
pub struct Mem0Store(pub Arc<OwnerMemory>);

impl StoreIndex for Mem0Store {
    fn home(&self) -> Option<PathBuf> {
        Some(self.0.home().to_path_buf())
    }
    fn ingest_day(&self, transcripts: &Transcripts, day: &str) -> usize {
        self.0.ingest_day(transcripts, day)
    }
    fn tidy(&self) -> usize {
        self.0.tidy()
    }
    fn is_open(&self) -> bool {
        self.0.is_open()
    }
    fn as_dream(&self) -> Arc<dyn DreamIndex> {
        Arc::new(OwnerMemoryDream(self.0.clone()))
    }
}

struct OwnerMemoryDream(Arc<OwnerMemory>);

impl DreamIndex for OwnerMemoryDream {
    fn ingest_day(&self, transcripts: &Transcripts, day: &str) -> anyhow::Result<usize> {
        DreamIndex::ingest_day(&*self.0, transcripts, day)
    }
    fn tidy(&self) -> anyhow::Result<usize> {
        DreamIndex::tidy(&*self.0)
    }
}

/// What `_open_memory_store` returns: (RecallConfig, Memory), and the memory's mem0 index as reindex needs it.
pub struct Store {
    pub cfg: RecallConfig,
    pub memory: Memory,
    pub index: Option<Arc<dyn StoreIndex>>,
}

/// The dreamer as the dream command drives it (`dream.Dreamer`; a fake in tests).
pub trait NightDreamer {
    fn due_nights(&self, now: chrono::DateTime<Local>) -> Vec<String>;
    fn dream(&self, day: &str) -> DreamReport;
}

struct RealDreamer(Dreamer);

impl NightDreamer for RealDreamer {
    fn due_nights(&self, now: chrono::DateTime<Local>) -> Vec<String> {
        self.0.due_nights(now)
    }
    fn dream(&self, day: &str) -> DreamReport {
        // asyncio.run(dreamer.dream(day))
        match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
            Ok(rt) => rt.block_on(self.0.dream(day)),
            Err(e) => {
                let mut report = DreamReport::new(day);
                report.failed.push(format!("runtime ({e})"));
                report
            }
        }
    }
}

pub type Client = Arc<dyn DreamClient + Send + Sync>;
pub type MakeDreamer =
    Box<dyn Fn(RecallConfig, Arc<Transcripts>, Client, Option<Arc<dyn DreamIndex>>) -> Box<dyn NightDreamer>>;

/// What the memory-folder commands open and build with: the store, the model client and the dreamer.
pub struct Seams {
    pub open: Box<dyn Fn(&Io) -> Option<Store>>,
    pub make_client: Box<dyn Fn() -> Option<Client>>,
    pub dreamer: MakeDreamer,
}

impl Seams {
    /// The real ones: the memory folder this process's environment names, records' client, the real dreamer.
    pub fn process() -> Seams {
        Seams {
            open: Box::new(|io| open_memory_store(None, io)),
            make_client: Box::new(|| {
                eve_records::make_client(None, eve_records::records::DEFAULT_MODEL).map(|c| Arc::new(c) as Client)
            }),
            dreamer: real_dreamer(),
        }
    }
}

/// `dream.Dreamer(cfg, transcripts, client, index)`.
pub fn real_dreamer() -> MakeDreamer {
    Box::new(|cfg, transcripts, client, index| Box::new(RealDreamer(Dreamer::new(cfg, transcripts, client, index, None))))
}

/// (RecallConfig, Memory) over the memory folder, for the offline memory commands. None (with a line on stderr)
/// while memory is off or before the daemon has made the folder: the one-time move is the daemon's. `environ`
/// None reads the process environment.
pub fn open_memory_store(environ: Option<&HashMap<String, String>>, io: &Io) -> Option<Store> {
    if !eve_memory::enabled(environ) {
        io.warn("memory: off — set EVE_MEMORY=1 (as the daemon has it) to use this");
        return None;
    }
    let cfg = eve_records::recall::configured(environ);
    if !cfg.store.join(eve_memory::MIGRATED_FILE).exists() {
        io.warn(&format!("memory: {} is not set up yet — start the daemon once with EVE_MEMORY=1", cfg.store.display()));
        return None;
    }
    let store = Arc::new(Transcripts::new(eve_transcripts::configured(environ), Some(cfg.archive_dir()), None));
    let owner = eve_mem0_memory::shared(&cfg, environ);
    let meaning = owner.clone().map(|o| Arc::new(eve_lite::Mem0Index(o)) as Arc<dyn eve_memory::MeaningIndex>);
    let memory = Memory::new(cfg.clone(), store, meaning);
    // The Telegram chat log, as the daemon keeps it: forget reaches it here too. The daemon reads the file again
    // before its next write when this process rewrote it (chat_log.ChatLog._refresh), so the forget holds.
    match eve_chat_log::open_configured(environ) {
        Ok(log) => memory.set_chat_log(log.map(|l| Arc::new(l) as Arc<dyn eve_memory::ChatLogLayer>)),
        Err(_) => io.warn("memory: the Telegram chat log could not be opened (OverflowError)"),
    }
    let index = owner.map(|o| Arc::new(Mem0Store(o)) as Arc<dyn StoreIndex>);
    Some(Store { cfg, memory, index })
}

fn layers_text(v: &Value) -> String {
    match get(v, "layers") {
        Value::Object(m) => m.iter().map(|(k, n)| format!("{k} {}", pstr(n))).collect::<Vec<_>>().join(", "),
        _ => String::new(),
    }
}

fn names(v: &Value) -> Vec<String> {
    match v {
        Value::Array(a) => a.iter().map(pstr).collect(),
        _ => Vec::new(),
    }
}

/// `int(x or 0)` for a count in an answer.
fn count(v: &Value) -> i64 {
    match v {
        Value::Number(n) => n.as_i64().unwrap_or_else(|| n.as_f64().map(|f| f as i64).unwrap_or(0)),
        Value::Bool(b) => i64::from(*b),
        Value::String(s) => crate::argparse::py_int(s).unwrap_or(0),
        _ => 0,
    }
}

/// `eve-bridge memory forget|dream|reindex`: the memory folder itself, no daemon needed. Prints counts, never the
/// matched words.
pub fn run_memory_store(args: &Namespace, io: &Io, seams: &Seams) -> i32 {
    let Some(store) = (seams.open)(io) else { return 1 };
    let Store { cfg, memory, index } = store;
    let cmd = args.str("memory_cmd").unwrap_or_default();

    if cmd == "forget" {
        let preview = memory.forget_preview(&args.str("query").unwrap_or_default());
        if !truthy(get(&preview, "ok")) {
            io.warn(&format!("forget: {}", py::or_str(&preview, "reason", "refused")));
            return 1;
        }
        let total = count(get(&preview, "total"));
        if total == 0 {
            io.say("forget: nothing in memory matches");
            return 0;
        }
        io.say(&format!("forget: {total} matching lines ({})", layers_text(&preview)));
        io.say(&format!("        not covered: {}", names(get(&preview, "not_covered")).join(", ")));
        let answer = io.ask("Forget them everywhere, for good? [y/N] ").unwrap_or_default();
        if !["y", "yes"].contains(&eve_envfile::py_strip(&answer).to_lowercase().as_str()) {
            io.say("forget: nothing changed");
            return 0;
        }
        let token = py::or_str(&preview, "token", "");
        let done = memory.forget_apply(&token);
        if !truthy(get(&done, "ok")) {
            io.warn(&format!("forget: {}", py::or_str(&done, "reason", "failed")));
            return 1;
        }
        let squashed = names(get(&done, "history_squashed")).join(", ");
        let squashed = if squashed.is_empty() { "none".to_string() } else { squashed };
        io.say(&format!(
            "forget: {} lines forgotten ({}); history squashed: {squashed}",
            pstr(&py::get_or(&done, "total", json!(0))),
            layers_text(&done)
        ));
        return 0;
    }

    if cmd == "dream" {
        let day = args.str("day");
        if let Some(d) = &day {
            if !is_day(d) {
                io.warn("dream: --day takes YYYY-MM-DD");
                return 2;
            }
        }
        let Some(client) = (seams.make_client)() else {
            io.warn("dream: no model client (OPENROUTER_API_KEY and the openai SDK are needed)");
            return 1;
        };
        let now = Local::now();
        if let Some(d) = &day {
            if d.as_str() >= memory.transcripts.day_of(now.fixed_offset()).as_str() {
                // due_nights holds the current day back; --day must too. Dreamt now, it is marked dreamt, and
                // tonight's dream skips everything said after this.
                io.warn(&format!("dream: {d} is not over yet; it is dreamt the night after"));
                return 2;
            }
        }
        let dreamer = (seams.dreamer)(cfg, memory.transcripts.clone(), client, index.as_ref().map(|i| i.as_dream()));
        let days = match day.filter(|d| !d.is_empty()) {
            Some(d) => vec![d],
            None => dreamer.due_nights(now),
        };
        if days.is_empty() {
            io.say("dream: no night is due");
            return 0;
        }
        let mut failed = 0;
        for day in days {
            let report = dreamer.dream(&day);
            if report.empty {
                io.say(&format!("dream: {day} — nothing said"));
            } else if report.dreamt {
                io.say(&format!("dream: {day} — {}", Dreamer::summary(&report)));
            } else {
                failed += 1;
                io.warn(&format!("dream: {day} — not dreamt (failed: {})", report.failed.join(", ")));
            }
        }
        return if failed > 0 { 1 } else { 0 };
    }

    // reindex
    let Some(index) = index else {
        io.warn("reindex: the mem0 index is off (EVE_MEM0=0, or mem0ai is not installed)");
        return 1;
    };
    let ledger = index.home().unwrap_or_else(|| cfg.mem0_dir()).join(eve_mem0_memory::INGESTED_CONVS);
    match std::fs::remove_file(&ledger) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            io.warn(&format!("reindex: could not reset the index ledger ({})", eve_ipc::io_error_type_name(&e)));
            return 1;
        }
    }
    let days = memory.transcripts.days();
    let added: usize = days.iter().map(|day| index.ingest_day(&memory.transcripts, day)).sum();
    let tidied = index.tidy();
    if !index.is_open() {
        io.warn("reindex: the index did not open — stop the daemon first (one opener at a time)");
        return 1;
    }
    io.say(&format!("reindex: {} days read, {added} memories added, {tidied} duplicates removed", days.len()));
    0
}

/// `re.fullmatch(r"\d{4}-\d{2}-\d{2}", day)` (Python's `\d` is any Unicode decimal digit).
fn is_day(day: &str) -> bool {
    let chars: Vec<char> = day.chars().collect();
    chars.len() == 10
        && chars.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == '-' } else { unicode_digit(*c) })
}

fn unicode_digit(c: char) -> bool {
    use unicode_general_category::{get_general_category, GeneralCategory};
    get_general_category(c) == GeneralCategory::DecimalNumber
}
