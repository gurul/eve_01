//! Owner-enabled eye cues from conversation text. One worker, one pending event, no sound or motion.
//!
//! Port of `eve_bridge/live_expressions.py`.
//!
//! The eye is picked by Jev (`EVE_EXPRESSION_BACKEND=jev`, the default since 2026-09-24) or by the original local
//! Laya checkpoint (`laya`; the trained heads did worse on fresh scenarios). Top-choice selection here is an
//! explicit policy, NOT a claim that a confidence gate passed. Failures emit nothing; the receiver (the retired
//! StackChan board, or Eve Link's voice-mode face) expires cues.
//!
//! Two instances run in the daemon. The board's is switched by `EVE_EXPRESSIONS_FILE` (off unless it says
//! `{"enabled": true}`). The phone's (owner, 2026-10-12: "route the expressions into the eyes") is on unless
//! `EVE_PHONE_EXPRESSIONS=0`, never reads or writes that file, and loads its model only when a phone call first
//! offers it a turn (`lazy`), so a Mac that never calls from the phone makes no Jev call for it.
//!
//! Python's single-threaded asyncio reads and writes the worker's attributes freely between awaits; here they live
//! in one [`State`] behind a mutex that is never held across an await or while a caller's callback runs.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eve_eye_model::{
    ttl_ms, ConversationContext, EyeAnswer, EyeError, EyeModel, JevEyeModel, JevEyeOptions, LiveEyeModel, LABELS,
    TTL_MAX_MS, TTL_MIN_MS, WARM_STATE,
};
use eve_jev::{Env, JevError, ProcessEnv};
use parking_lot::{Mutex, MutexGuard};
use serde_json::{json, Map, Value};
use tokio::sync::Notify;

#[cfg(test)]
mod tests;

/// `DEFAULT_MODEL` before `expanduser()`: the original local Laya checkpoint.
pub const DEFAULT_MODEL: &str = "~/.config/eve-bridge/models/laya-multilingual-mlx";
pub const BACKENDS: [&str; 2] = ["jev", "laya"];
pub const DEFAULT_BACKEND: &str = "jev";
pub const OFF_WORDS: [&str; 4] = ["0", "false", "off", "no"];
/// The board's switch file when `EVE_EXPRESSIONS_FILE` is unset.
pub const DEFAULT_SWITCH_FILE: &str = "~/.config/eve-bridge/expressions.json";
pub const POLICY: &str = "experimental top choice; not calibrated";

/// Seconds between two sent cues.
const MIN_GAP_S: f64 = 1.2;
/// A turn older than this (from offer to send) is stale.
const MAX_AGE_S: f64 = 4.0;
/// How long a non-lazy worker waits before retrying a failed load.
const RETRY_S: u64 = 10;
/// The board's own event history kept for `status`.
const BOARD_HISTORY: usize = 24;

// ---- switches -------------------------------------------------------------------------------------------

/// The phone's switch: on unless `EVE_PHONE_EXPRESSIONS` is 0, false, off or no. `phone_expressions_on()` with no
/// argument is `phone_expressions_on(&ProcessEnv)`.
pub fn phone_expressions_on<E: Env + ?Sized>(env: &E) -> bool {
    let value = py_lower(py_strip(&env.var("EVE_PHONE_EXPRESSIONS").unwrap_or_default()));
    !OFF_WORDS.contains(&value.as_str())
}

/// `jev` or `laya`; anything else (or nothing) is the default. One reader, so a typo is loud once.
pub fn backend_from_env<E: Env + ?Sized>(env: &E) -> &'static str {
    let value = py_lower(py_strip(&env.var("EVE_EXPRESSION_BACKEND").unwrap_or_default()));
    match BACKENDS.iter().find(|b| **b == value) {
        Some(backend) => backend,
        None => {
            if !value.is_empty() {
                let head: String = value.chars().take(20).collect();
                tracing::warn!(
                    "expressions: EVE_EXPRESSION_BACKEND={} is not one of ('jev', 'laya'); using {}",
                    py_repr(&head),
                    DEFAULT_BACKEND
                );
            }
            DEFAULT_BACKEND
        }
    }
}

/// The label's hold (eye_model.TTL_MS) less the time the turn has already waited, in TTL_MIN_MS..TTL_MAX_MS.
pub fn expression_ttl_ms(label: &str, age_s: f64) -> i64 {
    let hold = ttl_ms(label).unwrap_or(TTL_MIN_MS) as f64;
    // int() truncates toward zero; `as` does too (and saturates where Python would raise on inf/nan).
    let ttl = (hold - age_s * 1000.0) as i64;
    ttl.clamp(TTL_MIN_MS, TTL_MAX_MS)
}

// ---- the seams --------------------------------------------------------------------------------------------

/// A Python exception as the worker reports it: `type(exc).__name__` and `str(exc)`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct Raised {
    pub kind: String,
    pub message: String,
}

impl Raised {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self { kind: kind.into(), message: message.into() }
    }

    /// `f"{type(exc).__name__}: {exc}"[:180]`, the line `status()["error"]` shows.
    pub fn describe(&self) -> String {
        format!("{}: {}", self.kind, self.message).chars().take(180).collect()
    }
}

impl From<EyeError> for Raised {
    fn from(e: EyeError) -> Self {
        Raised::new(e.kind(), e.to_string())
    }
}

impl From<JevError> for Raised {
    fn from(e: JevError) -> Self {
        Raised::new("JevError", e.to_string())
    }
}

/// What `await send(cmd)` gives back: `Ok(false)` is Python's `False` (the cue was not delivered); `Ok(true)` is
/// any other value (`True` or `None`); `Err` is an exception.
pub type SendFuture = Pin<Box<dyn Future<Output = Result<bool, Raised>> + Send>>;
/// `send(cmd)`: the board's `ble.send` or the phone's `PhoneCalls.send_expression`.
pub type SendFn = Arc<dyn Fn(Value) -> SendFuture + Send + Sync>;
/// `connected()` / `muted()`.
pub type Flag = Arc<dyn Fn() -> bool + Send + Sync>;
/// `phase()`: the conversation phase `status()` reports.
pub type PhaseFn = Arc<dyn Fn() -> String + Send + Sync>;
/// `clock()`: seconds, monotonic.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;
/// `model_factory()`: a ready model or the exception its load raised.
pub type ModelFactory = Arc<dyn Fn() -> Result<LoadedModel, Raised> + Send + Sync>;

/// Wrap an async closure as a [`SendFn`].
pub fn send_fn<F, Fut>(f: F) -> SendFn
where
    F: Fn(Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<bool, Raised>> + Send + 'static,
{
    Arc::new(move |cmd| Box::pin(f(cmd)))
}

/// A loaded model and its `model` attribute (Jev's model id; Laya and test doubles have none), which the ready
/// log line and `status()["model"]` read with `getattr(model, "model", ...)`.
#[derive(Clone)]
pub struct LoadedModel {
    pub model: Arc<dyn EyeModel>,
    pub name: Option<String>,
}

impl LoadedModel {
    pub fn new(model: Arc<dyn EyeModel>) -> Self {
        Self { model, name: None }
    }

    pub fn named(model: Arc<dyn EyeModel>, name: impl Into<String>) -> Self {
        Self { model, name: Some(name.into()) }
    }
}

impl std::fmt::Debug for LoadedModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedModel").field("name", &self.name).finish_non_exhaustive()
    }
}

/// `time.monotonic`: seconds since a fixed point in this process.
pub fn monotonic() -> f64 {
    static BASE: LazyLock<Instant> = LazyLock::new(Instant::now);
    BASE.elapsed().as_secs_f64()
}

/// Python's keyword arguments to `LiveExpressions(send, *, ...)`, plus `env` for `os.environ`.
#[derive(Clone)]
pub struct Options {
    /// Default: always connected.
    pub connected: Flag,
    /// Default: never muted. Kept as Python keeps it; the worker never sends a chirp.
    pub muted: Flag,
    /// Default: `"idle"`.
    pub phase: PhaseFn,
    /// The board's switch file; `None` reads `EVE_EXPRESSIONS_FILE` (default `~/.config/eve-bridge/expressions.json`).
    /// Ignored when `enabled` is given.
    pub path: Option<PathBuf>,
    /// `None` is `_load`: the backend's model, warmed with one real answer.
    pub model_factory: Option<ModelFactory>,
    /// Default: [`monotonic`].
    pub clock: Clock,
    /// `Some`: the switch is the caller's and no file is read or written.
    pub enabled: Option<bool>,
    /// Load the model at the first offer, not at start.
    pub lazy: bool,
    /// The environment the constructor (and the default model load) reads; `None` is `os.environ`.
    pub env: Option<Arc<dyn Env + Send + Sync>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            connected: Arc::new(|| true),
            muted: Arc::new(|| false),
            phase: Arc::new(|| "idle".to_string()),
            path: None,
            model_factory: None,
            clock: Arc::new(monotonic),
            enabled: None,
            lazy: false,
            env: None,
        }
    }
}

// ---- the worker -------------------------------------------------------------------------------------------

/// The one pending turn: `(event, who, state, offered, generation)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub event: u32,
    pub who: String,
    pub state: String,
    pub offered: f64,
    pub generation: u64,
}

/// The worker's mutable attributes, as Python keeps them on `self`.
#[derive(Debug)]
pub struct State {
    pub enabled: bool,
    pub model: Option<LoadedModel>,
    pub ready: bool,
    pub error: String,
    pub pending: Option<Pending>,
    pub seq: u32,
    pub generation: u64,
    pub last_text: Option<(String, String)>,
    pub context: ConversationContext,
    pub last_sent_at: f64,
    pub last: Option<Value>,
    pub board: Option<Value>,
    pub board_history: Vec<Value>,
    pub sent: u64,
    pub dropped: u64,
    pub closed: bool,
}

impl State {
    /// `_next_id`: 32-bit, never 0.
    fn next_id(&mut self) -> u32 {
        self.seq = match self.seq.wrapping_add(1) {
            0 => 1,
            n => n,
        };
        self.seq
    }
}

pub struct LiveExpressions {
    send: SendFn,
    connected: Flag,
    muted: Flag,
    phase: PhaseFn,
    clock: Clock,
    /// Load the model at the first offer, not at start.
    pub lazy: bool,
    /// The board's switch file; `None` when `enabled` was given (the switch is the caller's).
    pub path: Option<PathBuf>,
    pub backend: &'static str,
    pub model_path: PathBuf,
    factory: ModelFactory,
    state: Mutex<State>,
    wake: Notify,
}

impl std::fmt::Debug for LiveExpressions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveExpressions")
            .field("lazy", &self.lazy)
            .field("path", &self.path)
            .field("backend", &self.backend)
            .field("model_path", &self.model_path)
            .field("state", &*self.state.lock())
            .finish_non_exhaustive()
    }
}

impl LiveExpressions {
    pub fn new(send: SendFn, options: Options) -> Self {
        let env: Arc<dyn Env + Send + Sync> = options.env.clone().unwrap_or_else(|| Arc::new(ProcessEnv));
        let mut path = None;
        let mut enabled = options.enabled.unwrap_or(false);
        if options.enabled.is_none() {
            // `Path(path or os.environ.get("EVE_EXPRESSIONS_FILE", default)).expanduser()`
            let raw = match options.path.as_ref().filter(|p| !p.as_os_str().is_empty()) {
                Some(p) => p.to_string_lossy().into_owned(),
                None => env.var("EVE_EXPRESSIONS_FILE").unwrap_or_else(|| DEFAULT_SWITCH_FILE.to_string()),
            };
            let p = py_path(&raw, &*env);
            enabled = read_switch(&p);
            path = Some(p);
        }
        let backend = backend_from_env(&*env);
        let model_path = match env.var("EVE_EXPRESSION_MODEL") {
            Some(value) => py_path(&value, &*env),
            None => py_path(DEFAULT_MODEL, &*env),
        };
        let factory = options.model_factory.clone().unwrap_or_else(|| {
            let (model_path, env) = (model_path.clone(), env.clone());
            Arc::new(move || load(backend, &model_path, &*env))
        });
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let seq = ((now_ms & 0xFFFF_FFFF) as u32).max(1); // survives host restarts
        Self {
            send,
            connected: options.connected,
            muted: options.muted,
            phase: options.phase,
            clock: options.clock,
            lazy: options.lazy,
            path,
            backend,
            model_path,
            factory,
            state: Mutex::new(State {
                enabled,
                model: None,
                ready: false,
                error: String::new(),
                pending: None,
                seq,
                generation: 0,
                last_text: None,
                context: ConversationContext::new(),
                last_sent_at: f64::NEG_INFINITY,
                last: None,
                board: None,
                board_history: Vec::new(),
                sent: 0,
                dropped: 0,
                closed: false,
            }),
            wake: Notify::new(),
        }
    }

    /// Every attribute Python reads or sets on the worker. Never hold it across an await.
    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock()
    }

    pub fn enabled(&self) -> bool {
        self.state.lock().enabled
    }

    pub fn ready(&self) -> bool {
        self.state.lock().ready
    }

    pub fn pending(&self) -> Option<Pending> {
        self.state.lock().pending.clone()
    }

    pub fn sent(&self) -> u64 {
        self.state.lock().sent
    }

    pub fn dropped(&self) -> u64 {
        self.state.lock().dropped
    }

    pub fn error(&self) -> String {
        self.state.lock().error.clone()
    }

    pub fn last(&self) -> Option<Value> {
        self.state.lock().last.clone()
    }

    /// `self.connected()`.
    pub fn connected(&self) -> bool {
        (self.connected)()
    }

    /// `self.muted()`.
    pub fn muted(&self) -> bool {
        (self.muted)()
    }

    /// `self.phase()`.
    pub fn phase(&self) -> String {
        (self.phase)()
    }

    /// One turn of conversation for the eye. Returns the event id the cue will carry, or `None` when it is not
    /// taken (off, closed, an unknown speaker, empty, or the same line again). A newer turn replaces a pending
    /// one, which counts as dropped.
    pub fn offer(&self, who: &str, text: &str) -> Option<u32> {
        if !["user", "assistant", "diary", "demo"].contains(&who) {
            return None;
        }
        {
            let st = self.state.lock();
            if !st.enabled || st.closed {
                return None;
            }
        }
        let text = last_chars(&py_split_join(text), 800);
        if text.is_empty() {
            return None;
        }
        // clock() twice, as Python reads it: once for the context, once for the offer time.
        let (now_state, now_offered) = ((self.clock)(), (self.clock)());
        let event = {
            let mut st = self.state.lock();
            if !st.enabled || st.closed {
                return None;
            }
            if st.last_text.as_ref().is_some_and(|(w, t)| w == who && *t == text) {
                return None;
            }
            st.last_text = Some((who.to_string(), text.clone()));
            if st.pending.is_some() {
                st.dropped += 1;
            }
            let event = st.next_id();
            st.generation += 1;
            let state = st.context.state(who, &text, now_state);
            st.pending =
                Some(Pending { event, who: who.to_string(), state, offered: now_offered, generation: st.generation });
            event
        };
        self.wake.notify_one();
        Some(event)
    }

    /// The owner's switch. Off clears any pending turn and the conversation, and clears the face if one is
    /// connected. The board's instance persists the choice in its switch file (written atomically).
    pub async fn set_enabled(&self, on: bool) -> Result<(), Raised> {
        {
            let mut st = self.state.lock();
            st.enabled = on;
            st.generation += 1;
            st.pending = None;
            st.last_text = None;
            st.context = ConversationContext::new();
        }
        if let Some(path) = &self.path {
            write_switch(path, on)?;
        }
        self.wake.notify_one();
        if !on && self.connected() {
            let id = self.state.lock().next_id();
            (self.send)(json!({"cmd": "expression", "id": id, "clear": true})).await?;
        }
        Ok(())
    }

    /// What the board reports it showed (an `expression` event with an integer `id`), kept for `status`.
    pub fn observe(&self, event: &Value) {
        let Some(obj) = event.as_object() else { return };
        // isinstance(x, int): a bool is an int too.
        let is_int = match obj.get("id") {
            Some(Value::Bool(_)) => true,
            Some(Value::Number(n)) => n.is_i64() || n.is_u64(),
            _ => false,
        };
        if !is_int {
            return;
        }
        let mut st = self.state.lock();
        st.board = Some(event.clone());
        st.board_history.push(event.clone());
        let excess = st.board_history.len().saturating_sub(BOARD_HISTORY);
        st.board_history.drain(..excess);
    }

    pub fn status(&self) -> Value {
        let mut out = {
            let st = self.state.lock();
            let model = if self.backend == "jev" {
                st.model.as_ref().and_then(|m| m.name.clone()).unwrap_or_default()
            } else {
                self.model_path.to_string_lossy().into_owned()
            };
            let mut out = Map::new();
            out.insert("enabled".into(), json!(st.enabled));
            out.insert("ready".into(), json!(st.ready));
            out.insert("backend".into(), json!(self.backend));
            out.insert("model".into(), json!(model));
            out.insert("policy".into(), json!(POLICY));
            out.insert("error".into(), json!(st.error));
            out.insert("sent".into(), json!(st.sent));
            out.insert("dropped".into(), json!(st.dropped));
            out.insert("last".into(), st.last.clone().unwrap_or(Value::Null));
            out.insert("board".into(), st.board.clone().unwrap_or(Value::Null));
            out.insert("board_history".into(), Value::Array(st.board_history.clone()));
            out
        };
        out.insert("phase".into(), json!(self.phase()));
        Value::Object(out)
    }

    /// The top choice of a model answer `{"probabilities": [...]}`: a distribution over LABELS (one finite number
    /// in 0..1 per label, bools refused, summing to 1 within 1e-4). Returns the label and its probability; anything
    /// else is `ValueError: invalid expression probabilities`.
    pub fn label(answer: &Value) -> Result<(&'static str, f64), Raised> {
        let invalid = || Raised::new("ValueError", "invalid expression probabilities");
        let values = answer.get("probabilities").and_then(Value::as_array).ok_or_else(invalid)?;
        let numbers: Vec<f64> = values
            .iter()
            .map(|v| match v {
                Value::Number(n) => n.as_f64(),
                _ => None, // bool, str, None, list, dict
            })
            .collect::<Option<_>>()
            .ok_or_else(invalid)?;
        Self::label_values(&numbers)
    }

    /// [`LiveExpressions::label`] over an [`EyeAnswer`]'s probabilities.
    pub fn label_values(values: &[f64]) -> Result<(&'static str, f64), Raised> {
        if values.len() != LABELS.len()
            || values.iter().any(|x| !x.is_finite() || !(0.0..=1.0).contains(x))
            || (py_sum(values) - 1.0).abs() > 1e-4
        {
            return Err(Raised::new("ValueError", "invalid expression probabilities"));
        }
        // max(range(n), key=values.__getitem__): the first maximum.
        let mut best = 0;
        for (i, x) in values.iter().enumerate() {
            if *x > values[best] {
                best = i;
            }
        }
        Ok((LABELS[best], values[best]))
    }

    /// The worker: loads the model (lazily, if asked), then turns the newest pending offer into one `expression`
    /// command at a time, at most one per 1.2 s, dropping turns older than 4 s, turns made stale by a switch, and
    /// turns with nothing connected. Runs until the task is dropped or aborted; then the worker is closed and
    /// takes no more offers.
    pub async fn run(self: Arc<Self>) {
        let _closing = CloseOnDrop(self.clone());
        loop {
            let now = (self.clock)();
            let step = {
                let mut st = self.state.lock();
                if st.closed {
                    Step::Exit
                } else if st.enabled && st.model.is_none() && (!self.lazy || st.pending.is_some()) {
                    Step::Load
                } else if !st.enabled || st.pending.is_none() {
                    Step::Wait
                } else if MIN_GAP_S - (now - st.last_sent_at) > 0.0 {
                    Step::Sleep(MIN_GAP_S - (now - st.last_sent_at))
                } else {
                    match st.pending.take() {
                        Some(item) => Step::Work(item, st.model.clone()),
                        None => Step::Wait,
                    }
                }
            };
            match step {
                Step::Exit => return,
                Step::Load => self.load_model().await,
                Step::Wait => self.wake.notified().await,
                // take the newest pending event after the delay
                // asyncio.sleep(inf) never returns; tokio clamps Duration::MAX to its far future.
                Step::Sleep(delay) => {
                    tokio::time::sleep(Duration::try_from_secs_f64(delay).unwrap_or(Duration::MAX)).await
                }
                Step::Work(item, model) => self.work(item, model).await,
            }
        }
    }

    async fn load_model(&self) {
        let factory = self.factory.clone();
        match blocking(move || factory()).await {
            Ok(loaded) => {
                let name = loaded.name.clone();
                {
                    let mut st = self.state.lock();
                    st.model = Some(loaded);
                    st.ready = true;
                    st.error.clear();
                }
                let who = if self.backend == "laya" {
                    "local Laya".to_string()
                } else {
                    format!("Jev ({})", name.as_deref().unwrap_or("?"))
                };
                tracing::info!("expressions: {who} ready (top-choice policy)");
            }
            Err(exc) => {
                // optional worker must never crash the daemon
                let error = exc.describe();
                {
                    let mut st = self.state.lock();
                    st.ready = false;
                    st.error = error.clone();
                    if self.lazy && st.pending.is_some() {
                        st.pending = None; // a lazy worker retries at the next offer, not forever
                        st.dropped += 1;
                    }
                }
                tracing::warn!("expressions: {error}");
                tokio::time::sleep(Duration::from_secs(RETRY_S)).await;
            }
        }
    }

    async fn work(&self, item: Pending, model: Option<LoadedModel>) {
        if (self.clock)() - item.offered > MAX_AGE_S || !self.connected() {
            self.state.lock().dropped += 1;
            return;
        }
        match self.pick(&item, model).await {
            Ok(true) => {}
            Ok(false) => self.state.lock().dropped += 1,
            Err(exc) => {
                let error = exc.describe();
                {
                    let mut st = self.state.lock();
                    st.error = error.clone();
                    st.dropped += 1;
                }
                tracing::warn!("expressions: {error}");
            }
        }
    }

    /// One turn: `Ok(true)` sent, `Ok(false)` dropped as stale, `Err` the exception the turn raised.
    async fn pick(&self, item: &Pending, model: Option<LoadedModel>) -> Result<bool, Raised> {
        let model = model.ok_or_else(|| Raised::new("AttributeError", "'NoneType' object has no attribute 'predict'"))?;
        let state = item.state.clone();
        let answer: EyeAnswer = blocking(move || model.model.predict(&state).map_err(Raised::from)).await?;
        let (label, p_top) = Self::label_values(&answer.probabilities)?;
        let age = (self.clock)() - item.offered;
        let (enabled, generation) = {
            let st = self.state.lock();
            (st.enabled, st.generation)
        };
        if !enabled || item.generation != generation || age > MAX_AGE_S || !self.connected() {
            return Ok(false);
        }
        // int() of a non-finite hold (only a clock answering nan or inf gets here) raises, and the turn is dropped.
        let hold = ttl_ms(label).unwrap_or(TTL_MIN_MS) as f64 - age * 1000.0;
        if hold.is_nan() {
            return Err(Raised::new("ValueError", "cannot convert float NaN to integer"));
        }
        if hold.is_infinite() {
            return Err(Raised::new("OverflowError", "cannot convert float infinity to integer"));
        }
        let ttl = expression_ttl_ms(label, age);
        let cmd = json!({"cmd": "expression", "id": item.event, "label": label, "ttl_ms": ttl, "chirp": false});
        if !(self.send)(cmd).await? {
            return Err(Raised::new("RuntimeError", "expression send failed"));
        }
        let sent_at = (self.clock)();
        let event_to_send_ms = ((self.clock)() - item.offered) * 1000.0;
        {
            let mut st = self.state.lock();
            st.last_sent_at = sent_at;
            st.sent += 1;
            st.last = Some(json!({
                "id": item.event,
                "source": item.who,
                "label": label,
                "p_top": p_top,
                "model_ms": answer.ms,
                "event_to_send_ms": event_to_send_ms,
                "chirp_requested": false,
            }));
            st.error.clear();
        }
        tracing::info!(
            "expressions: id={} source={} label={} model_ms={}",
            item.event,
            item.who,
            label,
            py_float_repr(answer.ms)
        );
        Ok(true)
    }
}

enum Step {
    Exit,
    Load,
    Wait,
    Sleep(f64),
    Work(Pending, Option<LoadedModel>),
}

/// Python's `finally: self.closed = True` (the executor's shutdown has no counterpart: a blocking call already
/// running finishes and its result is dropped, as Python's running future does).
struct CloseOnDrop(Arc<LiveExpressions>);

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        self.0.state.lock().closed = true;
    }
}

/// `loop.run_in_executor(self.executor, f)`: off the event loop. A panic is reported as an exception.
async fn blocking<T, F>(f: F) -> Result<T, Raised>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, Raised> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result,
        Err(e) => {
            let message = match e.try_into_panic() {
                Ok(payload) => payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "panic".to_string()),
                Err(_) => "cancelled".to_string(),
            };
            Err(Raised::new("RuntimeError", message))
        }
    }
}

/// `_load`: Laya from `model_path` or Jev from the env, then one real answer (ready means a real answer came back).
fn load(backend: &str, model_path: &Path, env: &(dyn Env + Send + Sync)) -> Result<LoadedModel, Raised> {
    let loaded = if backend == "laya" {
        LoadedModel::new(Arc::new(LiveEyeModel::new(model_path)?))
    } else {
        let model = JevEyeModel::new(env, JevEyeOptions::default())?;
        let name = model.model.clone();
        LoadedModel::named(Arc::new(model), name)
    };
    loaded.model.predict(WARM_STATE)?;
    Ok(loaded)
}

// ---- the switch file --------------------------------------------------------------------------------------

/// `json.loads(path.read_text()).get("enabled") is True`; an unreadable, non-JSON or non-object file is off.
fn read_switch(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else { return false };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(obj)) => obj.get("enabled") == Some(&Value::Bool(true)),
        _ => false,
    }
}

/// `{"enabled": on}` + newline, written to `<stem>.tmp` and renamed over the switch file.
fn write_switch(path: &Path, on: bool) -> Result<(), Raised> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| os_error(&e, parent, None))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, format!("{{\"enabled\": {}}}\n", if on { "true" } else { "false" }))
        .map_err(|e| os_error(&e, &tmp, None))?;
    std::fs::rename(&tmp, path).map_err(|e| os_error(&e, &tmp, Some(path)))
}

/// An `OSError` as Python names and prints it: `[Errno 13] Permission denied: '/x'` (`'src' -> 'dst'` for a rename).
fn os_error(e: &std::io::Error, path: &Path, dst: Option<&Path>) -> Raised {
    let kind = match e.raw_os_error() {
        Some(2) => "FileNotFoundError",
        Some(1) | Some(13) => "PermissionError",
        Some(17) => "FileExistsError",
        Some(20) => "NotADirectoryError",
        Some(21) => "IsADirectoryError",
        _ => "OSError",
    };
    let shown = e.to_string();
    let strerror = match shown.rfind(" (os error ") {
        Some(at) => shown[..at].to_string(),
        None => shown,
    };
    let message = match (e.raw_os_error(), dst) {
        (Some(code), Some(dst)) => format!(
            "[Errno {code}] {strerror}: {} -> {}",
            py_repr(&path.to_string_lossy()),
            py_repr(&dst.to_string_lossy())
        ),
        (Some(code), None) => format!("[Errno {code}] {strerror}: {}", py_repr(&path.to_string_lossy())),
        (None, _) => strerror,
    };
    Raised::new(kind, message)
}

// ---- Python value semantics ---------------------------------------------------------------------------------

/// Python's `str.isspace()`: Unicode White_Space plus the ASCII separators U+001C..U+001F.
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn py_strip(s: &str) -> &str {
    s.trim_matches(py_space)
}

fn py_lower(s: &str) -> String {
    s.to_lowercase()
}

/// `" ".join(text.split())`.
fn py_split_join(text: &str) -> String {
    text.split(py_space).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// `text[-n:]`, by code point.
fn last_chars(text: &str, n: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(n)).collect()
}

/// `sum(values)` over floats, as Python 3.12+ adds them (Neumaier compensation).
fn py_sum(values: &[f64]) -> f64 {
    let (mut total, mut c) = (0.0f64, 0.0f64);
    for &x in values {
        let t = total + x;
        if total.abs() >= x.abs() {
            c += (total - t) + x;
        } else {
            c += (x - t) + total;
        }
        total = t;
    }
    if c != 0.0 && c.is_finite() {
        total += c;
    }
    total
}

/// `repr(float)`: shortest round trip, `.0` on integral values, exponent form outside 1e-4..1e16.
fn py_float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let a = x.abs();
    if !(1e-4..1e16).contains(&a) {
        let s = format!("{x:e}");
        let (mantissa, exp) = s.split_once('e').unwrap_or((&s, "0"));
        let exp: i32 = exp.parse().unwrap_or(0);
        let sign = if exp < 0 { '-' } else { '+' };
        return format!("{mantissa}e{sign}{:02}", exp.abs());
    }
    let s = format!("{x}");
    if s.contains('.') {
        s
    } else {
        format!("{s}.0")
    }
}

/// `repr(str)`: single quotes unless the text has one and no double quote; escapes as Python prints them.
fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if !py_printable(c) => {
                let n = c as u32;
                if n < 0x100 {
                    out.push_str(&format!("\\x{n:02x}"));
                } else if n < 0x10000 {
                    out.push_str(&format!("\\u{n:04x}"));
                } else {
                    out.push_str(&format!("\\U{n:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `str.isprintable()` for one character: not a control, separator (other than space) or format character.
fn py_printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    !(c.is_control()
        || py_space(c)
        || matches!(c as u32, 0xad | 0x600..=0x605 | 0x61c | 0x6dd | 0x70f | 0x180e | 0x200b..=0x200f
            | 0x202a..=0x202e | 0x2060..=0x2064 | 0x2066..=0x206f | 0xfeff | 0xfff9..=0xfffb
            | 0xd800..=0xdfff | 0xe000..=0xf8ff | 0xf0000..=0x10ffff))
}

/// `Path(raw).expanduser()`: pathlib's normalization (repeated slashes and `.` parts collapsed, no trailing slash,
/// `""` is `.`, exactly two leading slashes kept), then a leading `~` replaced by `$HOME` (from `env`, else the
/// account's home directory).
fn py_path<E: Env + ?Sized>(raw: &str, env: &E) -> PathBuf {
    let normal = py_normpath(raw);
    if normal == "~" || normal.starts_with("~/") {
        let home = env
            .var("HOME")
            .or_else(|| dirs::home_dir().map(|h| h.to_string_lossy().into_owned()))
            .unwrap_or_default();
        let home = home.trim_end_matches('/');
        let home = if home.is_empty() { "/" } else { home };
        return PathBuf::from(py_normpath(&format!("{home}/{}", &normal[1..])));
    }
    PathBuf::from(normal)
}

fn py_normpath(raw: &str) -> String {
    let slashes = raw.len() - raw.trim_start_matches('/').len();
    let root = match slashes {
        0 => "",
        2 => "//",
        _ => "/",
    };
    let parts: Vec<&str> = raw.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    let joined = format!("{root}{}", parts.join("/"));
    if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}
