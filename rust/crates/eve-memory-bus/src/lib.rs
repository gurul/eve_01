//! MemoryBus: eve's memory, as it forms, on an in-process pub/sub bus.
//!
//! eve keeps two memories that go on this bus: the diary (what it sees) is a JSONL stream,
//! and a lesson is a SQLite row. This bus is the one place those become events the moment they
//! happen, so that anything else can watch: the rosbridge server fans them out to WebSocket
//! clients as ROS-style topics, and the claude-mem client stores them in the owner's claude-mem
//! so a Claude Code session can search eve's day.
//!
//! The daemon owns the one instance and binds it to its runtime. Publish is safe from any
//! thread: once the bus is bound, subscribers run on the bound runtime; before that, they run
//! inline. A subscriber that fails (returns an error or panics) is logged and kept.
//!
//! Topics carry a ROS-style type name so a rosbridge client can `subscribe` with `type`.
//! What is NOT on the bus, by design: anything said in a conversation (owner, 2026-09-23: the
//! transcripts and records stay in the memory folder), the learner's own words in a lesson
//! (spoken ideas and typed ideas stay in the lesson store, docs/learning.md § Privacy), and raw
//! audio or frames. An inbound `/eve/memory/remember` line becomes one transcript line
//! (channel "bus") for the nightly dream to judge — never a star.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::future::Future;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::future::BoxFuture;
use futures::FutureExt;
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{Map, Value};
use tokio::runtime::Handle;

pub const DEFAULT_ROSBRIDGE_HOST: &str = "127.0.0.1";
pub const DEFAULT_ROSBRIDGE_PORT: i64 = 9090;
const ON: [&str; 4] = ["1", "true", "yes", "on"];

/// A message on the bus: a JSON object, as Python's `dict[str, Any]`.
pub type Msg = Map<String, Value>;

/// Which sinks the daemon attaches to the bus. Everything is off unless asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusConfig {
    pub rosbridge: bool,
    pub rosbridge_host: String,
    pub rosbridge_port: i64,
    pub claude_mem: bool,
    pub claude_mem_url: Option<String>,
    pub mirror: bool,
}

impl Default for BusConfig {
    fn default() -> Self {
        BusConfig {
            rosbridge: false,
            rosbridge_host: DEFAULT_ROSBRIDGE_HOST.to_string(),
            rosbridge_port: DEFAULT_ROSBRIDGE_PORT,
            claude_mem: false,
            claude_mem_url: None,
            mirror: false,
        }
    }
}

/// EVE_ROSBRIDGE / EVE_ROSBRIDGE_HOST / EVE_ROSBRIDGE_PORT,
/// EVE_CLAUDE_MEM / EVE_CLAUDE_MEM_URL / EVE_CLAUDE_MEM_MIRROR (defaults to CLAUDE_MEM).
///
/// `environ` of `None` reads the process environment.
pub fn configured(environ: Option<&HashMap<String, String>>) -> BusConfig {
    let get = |name: &str| -> String {
        match environ {
            Some(env) => env.get(name).cloned().unwrap_or_default(),
            None => std::env::var(name).unwrap_or_default(),
        }
    };
    let flag = |name: &str, default: bool| -> bool {
        let raw = get(name).trim().to_lowercase();
        if raw.is_empty() {
            default
        } else {
            ON.contains(&raw.as_str())
        }
    };

    let mut port = DEFAULT_ROSBRIDGE_PORT;
    let raw_port = get("EVE_ROSBRIDGE_PORT").trim().to_string();
    if !raw_port.is_empty() {
        match python_int(&raw_port) {
            Some(p) => port = p,
            None => tracing::warn!(
                "memory bus: EVE_ROSBRIDGE_PORT={} is not a number; using {}",
                py_repr(&raw_port),
                port
            ),
        }
    }
    let claude_mem = flag("EVE_CLAUDE_MEM", false);
    let host = get("EVE_ROSBRIDGE_HOST").trim().to_string();
    let url = get("EVE_CLAUDE_MEM_URL").trim().to_string();
    BusConfig {
        rosbridge: flag("EVE_ROSBRIDGE", false),
        rosbridge_host: if host.is_empty() { DEFAULT_ROSBRIDGE_HOST.to_string() } else { host },
        rosbridge_port: port,
        claude_mem,
        claude_mem_url: if url.is_empty() { None } else { Some(url) },
        mirror: flag("EVE_CLAUDE_MEM_MIRROR", claude_mem),
    }
}

/// Python's `int(str)` for a base-10 literal: optional sign, ASCII digits, single underscores
/// between digits, surrounding whitespace ignored.
fn python_int(raw: &str) -> Option<i64> {
    let s = raw.trim();
    let (neg, body) = match s.as_bytes().first() {
        Some(b'+') => (false, &s[1..]),
        Some(b'-') => (true, &s[1..]),
        _ => (false, s),
    };
    if body.is_empty() || body.starts_with('_') || body.ends_with('_') || body.contains("__") {
        return None;
    }
    let mut value: i64 = 0;
    for c in body.chars() {
        if c == '_' {
            continue;
        }
        let d = c.to_digit(10)? as i64;
        value = value.checked_mul(10)?.checked_add(d)?;
    }
    Some(if neg { -value } else { value })
}

/// Python's `repr()` of a str, for the characters an env value can hold.
fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// topic -> type. The msg shapes are documented in docs/memory-bus.md.
pub const TOPICS: &[(&str, &str)] = &[
    ("/eve/memory/observation", "eve_msgs/Observation"), // diary: a written thought about the room
    ("/eve/memory/lesson", "eve_msgs/LessonEvent"),      // a lesson action and eve's feedback
    ("/eve/memory/remember", "eve_msgs/Remember"),       // inbound: someone asks eve to keep a line
    ("/eve/state", "eve_msgs/AgentState"),               // idle / wake / listening / thinking / speaking
    ("/eve/presence", "eve_msgs/Presence"),              // follow: seen / lost / found (and how), with the angles
    ("/claude/observation", "eve_msgs/ClaudeObservation"), // mirrored from the owner's claude-mem stream
];
/// service -> type. args and values shapes are in docs/memory-bus.md.
pub const SERVICES: &[(&str, &str)] = &[
    ("/eve/memory/recall", "eve_msgs/Recall"), // {"query": str, "limit": int} -> {"results": [...]}
];
pub const RECENT_PER_TOPIC: usize = 20;

/// The catalogue as an ordered map, as Python's `TOPICS` dict.
pub fn topics_map() -> IndexMap<String, String> {
    TOPICS.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// The service catalogue as an ordered map, as Python's `SERVICES` dict.
pub fn services_map() -> IndexMap<String, String> {
    SERVICES.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

type SyncFn = dyn Fn(&str, &Msg) -> anyhow::Result<()> + Send + Sync;
type AsyncFn = dyn Fn(String, Msg) -> BoxFuture<'static, ()> + Send + Sync;
type ServiceFn = dyn Fn(Msg) -> BoxFuture<'static, anyhow::Result<Msg>> + Send + Sync;

/// A subscriber. Python's callbacks either return a value (run inline) or a coroutine (scheduled
/// on the bound loop); here the two are separate variants.
#[derive(Clone)]
pub enum Callback {
    Sync(Arc<SyncFn>),
    Async(Arc<AsyncFn>),
}

impl fmt::Debug for Callback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Callback::Sync(_) => f.write_str("Callback::Sync"),
            Callback::Async(_) => f.write_str("Callback::Async"),
        }
    }
}

/// A service handler: args in, values out, awaited.
pub type ServiceHandler = Arc<ServiceFn>;

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub topic: String,
    pub msg: Msg,
    pub time: f64,
}

#[derive(Debug, Clone)]
pub struct Subscription {
    pub topic: String,
    pub callback: Callback,
    pub id: u64,
}

/// Why `call_service` failed: no such service (Python's `KeyError`), or the handler failed.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("no such service: {0}")]
    NoSuchService(String),
    #[error(transparent)]
    Failed(#[from] anyhow::Error),
}

/// The runtime the bus delivers on, and whether it has shut down (Python's `loop.is_closed()`).
struct Bound {
    handle: Handle,
    closed: Arc<AtomicBool>,
}

/// Flips `closed` when the runtime drops its tasks at shutdown.
struct ClosedGuard(Arc<AtomicBool>);

impl Drop for ClosedGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

struct State {
    subs: IndexMap<String, Vec<Subscription>>,
    recent: HashMap<String, VecDeque<Event>>,
    next_id: u64,
}

struct Inner {
    state: Mutex<State>,
    services: Mutex<HashMap<String, ServiceHandler>>,
    bound: Mutex<Option<Bound>>,
    clock: Box<dyn Fn() -> f64 + Send + Sync>,
    dropped: AtomicU64,
}

/// The bus. Cloning shares the same bus.
#[derive(Clone)]
pub struct MemoryBus {
    inner: Arc<Inner>,
}

impl Default for MemoryBus {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for MemoryBus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryBus")
            .field("subscriptions", &self.subscription_count())
            .field("dropped", &self.dropped())
            .finish()
    }
}

fn wall_clock() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

impl MemoryBus {
    /// A bus stamping events with the wall clock (Python's `time.time`).
    pub fn new() -> Self {
        Self::with_clock(wall_clock)
    }

    pub fn with_clock(clock: impl Fn() -> f64 + Send + Sync + 'static) -> Self {
        MemoryBus {
            inner: Arc::new(Inner {
                state: Mutex::new(State { subs: IndexMap::new(), recent: HashMap::new(), next_id: 1 }),
                services: Mutex::new(HashMap::new()),
                bound: Mutex::new(None),
                clock: Box::new(clock),
                dropped: AtomicU64::new(0),
            }),
        }
    }

    /// Async subscribers that could not be scheduled because no live runtime was bound.
    pub fn dropped(&self) -> u64 {
        self.inner.dropped.load(Ordering::SeqCst)
    }

    // -- wiring --

    /// After this, callbacks run on `handle`'s runtime even when publish() is called elsewhere.
    pub fn bind_loop(&self, handle: Handle) {
        let closed = Arc::new(AtomicBool::new(false));
        let guard = ClosedGuard(closed.clone());
        handle.spawn(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        });
        *self.inner.bound.lock() = Some(Bound { handle, closed });
    }

    /// Subscribe to one topic, or to a prefix ending in '*' ("/eve/memory/*").
    pub fn subscribe(&self, topic: &str, callback: impl Fn(&str, &Msg) + Send + Sync + 'static) -> Subscription {
        self.subscribe_callback(
            topic,
            Callback::Sync(Arc::new(move |t: &str, m: &Msg| {
                callback(t, m);
                Ok(())
            })),
        )
    }

    /// As `subscribe`, for a callback that can fail; an error is logged like a raised exception.
    pub fn subscribe_fallible(
        &self,
        topic: &str,
        callback: impl Fn(&str, &Msg) -> anyhow::Result<()> + Send + Sync + 'static,
    ) -> Subscription {
        self.subscribe_callback(topic, Callback::Sync(Arc::new(callback)))
    }

    /// Subscribe with an async callback: each event's future is spawned on the bound runtime.
    pub fn subscribe_async<F, Fut>(&self, topic: &str, callback: F) -> Subscription
    where
        F: Fn(String, Msg) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.subscribe_callback(topic, Callback::Async(Arc::new(move |t, m| callback(t, m).boxed())))
    }

    pub fn subscribe_callback(&self, topic: &str, callback: Callback) -> Subscription {
        let mut state = self.inner.state.lock();
        let sub = Subscription { topic: topic.to_string(), callback, id: state.next_id };
        state.next_id += 1;
        state.subs.entry(topic.to_string()).or_default().push(sub.clone());
        sub
    }

    pub fn unsubscribe(&self, sub: &Subscription) {
        let mut state = self.inner.state.lock();
        state.subs.entry(sub.topic.clone()).or_default().retain(|s| s.id != sub.id);
    }

    /// How many subscriptions are live, across every topic and prefix.
    pub fn subscription_count(&self) -> usize {
        self.inner.state.lock().subs.values().map(Vec::len).sum()
    }

    pub fn register_service<F, Fut>(&self, name: &str, handler: F)
    where
        F: Fn(Msg) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<Msg>> + Send + 'static,
    {
        let handler: ServiceHandler = Arc::new(move |args| handler(args).boxed());
        self.inner.services.lock().insert(name.to_string(), handler);
    }

    pub fn has_service(&self, name: &str) -> bool {
        self.inner.services.lock().contains_key(name)
    }

    pub async fn call_service(&self, name: &str, args: Msg) -> Result<Msg, ServiceError> {
        let handler = self.inner.services.lock().get(name).cloned();
        match handler {
            None => Err(ServiceError::NoSuchService(name.to_string())),
            Some(h) => Ok(h(args).await?),
        }
    }

    // -- events --

    /// Deliver `msg` on `topic`. Safe from any thread. Never fails for a subscriber's error.
    pub fn publish(&self, topic: &str, msg: Msg) -> Event {
        let event = Event { topic: topic.to_string(), msg, time: (self.inner.clock)() };
        let mut targets: Vec<Subscription>;
        {
            let mut state = self.inner.state.lock();
            let ring = state.recent.entry(topic.to_string()).or_insert_with(|| VecDeque::with_capacity(RECENT_PER_TOPIC));
            if ring.len() == RECENT_PER_TOPIC {
                ring.pop_front();
            }
            ring.push_back(event.clone());
            targets = state.subs.get(topic).cloned().unwrap_or_default();
            for (pattern, subs) in &state.subs {
                if let Some(prefix) = pattern.strip_suffix('*') {
                    if pattern != topic && topic.starts_with(prefix) {
                        targets.extend(subs.iter().cloned());
                    }
                }
            }
        }
        let live = self.live_handle();
        match live {
            Some(handle) => {
                let on_loop = Handle::try_current().map(|h| h.id() == handle.id()).unwrap_or(false);
                if on_loop {
                    self.inner.deliver(&targets, &event);
                } else {
                    let inner = self.inner.clone();
                    let ev = event.clone();
                    handle.spawn(async move { inner.deliver(&targets, &ev) });
                }
            }
            None => self.inner.deliver(&targets, &event),
        }
        event
    }

    fn live_handle(&self) -> Option<Handle> {
        self.inner.live_handle()
    }

    /// The last `RECENT_PER_TOPIC` events on `topic`, oldest first.
    pub fn recent(&self, topic: &str) -> Vec<Event> {
        self.recent_n(topic, RECENT_PER_TOPIC as isize)
    }

    /// Python's `recent(topic, n)`: the slice `[-n:]` of the ring (so `n == 0` is the whole ring).
    pub fn recent_n(&self, topic: &str, n: isize) -> Vec<Event> {
        let state = self.inner.state.lock();
        let Some(ring) = state.recent.get(topic) else { return Vec::new() };
        let len = ring.len() as isize;
        let start = if n == 0 {
            0
        } else if n > 0 {
            (len - n).max(0)
        } else {
            (-n).min(len)
        };
        ring.iter().skip(start as usize).cloned().collect()
    }

    pub fn topics(&self) -> IndexMap<String, String> {
        topics_map()
    }

    pub fn services(&self) -> IndexMap<String, String> {
        services_map()
    }
}

impl Inner {
    fn live_handle(&self) -> Option<Handle> {
        let bound = self.bound.lock();
        bound.as_ref().filter(|b| !b.closed.load(Ordering::SeqCst)).map(|b| b.handle.clone())
    }

    fn deliver(&self, targets: &[Subscription], event: &Event) {
        for sub in targets {
            match &sub.callback {
                Callback::Sync(f) => match catch_unwind(AssertUnwindSafe(|| f(&event.topic, &event.msg))) {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        tracing::error!(error = %format!("{e:#}"), "memory bus: subscriber failed on {}", event.topic)
                    }
                    Err(p) => tracing::error!(
                        error = %panic_text(p.as_ref()),
                        "memory bus: subscriber failed on {}",
                        event.topic
                    ),
                },
                Callback::Async(f) => {
                    let made = catch_unwind(AssertUnwindSafe(|| f(event.topic.clone(), event.msg.clone())));
                    let fut = match made {
                        Ok(fut) => fut,
                        Err(p) => {
                            tracing::error!(
                                error = %panic_text(p.as_ref()),
                                "memory bus: subscriber failed on {}",
                                event.topic
                            );
                            continue;
                        }
                    };
                    match self.live_handle() {
                        Some(handle) => {
                            handle.spawn(fut);
                        }
                        None => {
                            drop(fut);
                            self.dropped.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }
            }
        }
    }
}

fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic".to_string()
    }
}

#[cfg(test)]
mod tests;
