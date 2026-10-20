//! Owner approval for AgentCard's single-use purchase confirmations.
//!
//! The client owns cart/address binding, durable attempts and provider reconciliation.
//! This gateway owns the question's lifecycle and serializes confirmations. Stopping a
//! worker cannot undo a request that has already reached the provider.
//!
//! Port of `eve_bridge/agentcard_tools.py`.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

use parking_lot::Mutex;
use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value, json};
use tokio::sync::watch;
use tokio::task::AbortHandle;

pub const CONFIRM_TOOL: &str = "agentcard_confirm";
pub const NONCONFIRM_TOOLS: [&str; 3] = ["agentcard_merchants", "agentcard_buy", "agentcard_status"];

/// `_CART_HASH`: `[0-9a-f]{16}\Z`, used with `fullmatch`.
static CART_HASH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\A[0-9a-f]{16}\z").expect("cart hash regex"));

/// Why the owner question ended without an answer. Python's `ask_user` raised
/// `TimeoutError` or any other exception; the text of the latter is never shown.
#[derive(Debug, thiserror::Error)]
pub enum AskError {
    #[error("the owner question timed out")]
    Timeout,
    #[error("the owner question failed")]
    Failed(#[source] anyhow::Error),
}

/// `AskUser`: `ask_user(question, about)`, bound by the inlet to the owner's firm
/// yes/no UI. [`AgentCardTools::handle`] takes any `FnOnce(String, String) -> Future`;
/// this is the boxed form for storing one.
pub type AskUser = Arc<
    dyn Fn(String, String) -> Pin<Box<dyn Future<Output = Result<String, AskError>> + Send>> + Send + Sync,
>;

/// `prepare_confirm`'s dict: the JSON part (`ok`, `question`, `about`, or a failure
/// dict) and the opaque snapshot kept out of it.
#[derive(Debug)]
pub struct Prepared<S> {
    pub result: Value,
    pub snapshot: Option<S>,
}

/// `Service`: the AgentCard client. Calls are blocking, like the Python client, and
/// run on worker threads (`asyncio.to_thread` becomes `spawn_blocking`). An `Err`
/// or a panic is Python's `Exception`: its text never reaches the owner or the log.
pub trait Service: Send + Sync + 'static {
    type Snapshot: Send + 'static;

    fn handle(&self, name: &str, args: Value) -> anyhow::Result<Value>;

    fn prepare_confirm(&self, conversation_id: &str, cart_hashes: &[String])
    -> anyhow::Result<Prepared<Self::Snapshot>>;

    /// `cancelled` is the request's `threading.Event`: set by Stop, checked by the
    /// client before it dispatches.
    fn confirm(&self, snapshot: Self::Snapshot, cancelled: &Arc<AtomicBool>) -> anyhow::Result<Value>;
}

impl Service for eve_agentcard::AgentCard {
    type Snapshot = eve_agentcard::Snapshot;

    fn handle(&self, name: &str, args: Value) -> anyhow::Result<Value> {
        Ok(eve_agentcard::AgentCard::handle(self, name, &args))
    }

    fn prepare_confirm(&self, conversation_id: &str, cart_hashes: &[String]) -> anyhow::Result<Prepared<Self::Snapshot>> {
        let prepared = eve_agentcard::AgentCard::prepare_confirm(self, &json!(conversation_id), &json!(cart_hashes));
        Ok(Prepared { result: prepared.result, snapshot: prepared.snapshot })
    }

    fn confirm(&self, snapshot: Self::Snapshot, cancelled: &Arc<AtomicBool>) -> anyhow::Result<Value> {
        // Python's client can raise before dispatch; the gateway reports that as an unknown outcome.
        Ok(eve_agentcard::AgentCard::try_confirm(self, &snapshot, Some(cancelled.as_ref()))?)
    }
}

/// `_Confirmation.phase`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Queued,
    Preparing,
    Asking,
    Confirming,
}

struct ConfirmationState {
    phase: Phase,
    asking: Option<AbortHandle>,
}

/// `_Confirmation`. The flag is read by worker threads; the phase and the question
/// handle change together with it under one lock, so a Stop on another thread sees
/// the same atomic steps the Python event loop gave.
struct Confirmation {
    chat_id: i64,
    cancelled: Arc<AtomicBool>,
    state: Mutex<ConfirmationState>,
    /// Closed when the lifecycle task finishes (`request.task` for `wait_idle`).
    done: watch::Receiver<()>,
}

impl Confirmation {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Set the flag and withdraw the owner question if it is still open.
    fn cancel(&self) {
        let mut state = self.state.lock();
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(asking) = state.asking.take() {
            asking.abort();
        }
    }

    /// Move to `phase` unless Stop got there first.
    fn enter(&self, phase: Phase) -> bool {
        let mut state = self.state.lock();
        if self.is_cancelled() {
            return false;
        }
        state.phase = phase;
        true
    }

    fn phase(&self) -> Phase {
        self.state.lock().phase
    }
}

/// `_not_submitted`.
pub fn not_submitted(reason: &str) -> Value {
    json!({"ok": false, "status": "not_submitted", "reason": reason})
}

const STOPPED: &str = "Purchase confirmation was stopped before submission.";
const NOT_PREPARED: &str = "AgentCard could not prepare a purchase confirmation.";
const UNUSABLE: &str = "AgentCard did not return a usable result.";

fn unknown_outcome() -> Value {
    json!({"ok": false, "status": "unknown", "reason": "The purchase outcome is unknown; \
check AgentCard status before attempting another confirmation."})
}

/// The `except Exception` branch: never log private response or exception text.
fn failure_in(phase: Phase) -> Value {
    if phase == Phase::Confirming {
        return unknown_outcome();
    }
    not_submitted("AgentCard could not obtain owner approval; no confirmation was submitted.")
}

/// Python's `str.strip()` whitespace: Unicode White_Space plus `\x1c`-`\x1f`.
fn is_blank(text: &str) -> bool {
    text.chars().all(|c| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
}

/// `stop()`'s dict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StopReport {
    pub stopped: usize,
    pub in_flight: bool,
    pub line: String,
}

impl StopReport {
    pub fn to_value(&self) -> Value {
        json!({"stopped": self.stopped, "in_flight": self.in_flight, "line": self.line})
    }
}

struct Inner<S> {
    service: Arc<S>,
    lock: tokio::sync::Mutex<()>,
    requests: Mutex<Vec<Arc<Confirmation>>>,
    closed: AtomicBool,
}

impl<S> Inner<S> {
    fn discard(&self, request: &Arc<Confirmation>) {
        self.requests.lock().retain(|r| !Arc::ptr_eq(r, request));
    }
}

/// One confirmation at a time, from preparation until its worker returns.
///
/// `ask_user(question, about)` must be bound to the owner's firm yes/no UI by
/// the inlet. This gateway independently requires a whole bare approval. Cloning
/// shares the gateway; cancellation flags cross to the blocking workers.
pub struct AgentCardTools<S: Service> {
    inner: Arc<Inner<S>>,
}

impl<S: Service> Clone for AgentCardTools<S> {
    fn clone(&self) -> Self {
        AgentCardTools { inner: self.inner.clone() }
    }
}

/// Withdraws the request when the caller stops awaiting `handle` (Python's
/// `except asyncio.CancelledError` around the shielded task).
struct CallerGuard {
    request: Arc<Confirmation>,
    armed: bool,
}

impl Drop for CallerGuard {
    fn drop(&mut self) {
        if self.armed {
            // The lifecycle task retains the serialization lock and observes its
            // worker. Otherwise cancellation could admit another confirmation
            // while the first thread is still preparing or sending a purchase.
            self.request.cancel();
        }
    }
}

/// The lifecycle task's `finally`, plus its `except CancelledError` when the task
/// itself is dropped before finishing.
struct WorkerGuard<S> {
    inner: Arc<Inner<S>>,
    request: Arc<Confirmation>,
    finished: bool,
    _done: watch::Sender<()>,
}

impl<S> Drop for WorkerGuard<S> {
    fn drop(&mut self) {
        if !self.finished {
            self.request.cancel();
        }
        self.inner.discard(&self.request);
    }
}

impl<S: Service> AgentCardTools<S> {
    pub fn new(service: Arc<S>) -> Self {
        AgentCardTools {
            inner: Arc::new(Inner {
                service,
                lock: tokio::sync::Mutex::new(()),
                requests: Mutex::new(Vec::new()),
                closed: AtomicBool::new(false),
            }),
        }
    }

    pub fn service(&self) -> &Arc<S> {
        &self.inner.service
    }

    pub fn active(&self, chat_id: Option<i64>) -> bool {
        self.inner.requests.lock().iter().any(|r| chat_id.is_none_or(|c| r.chat_id == c))
    }

    /// Withdraw matching pending approvals; signal workers before dispatch.
    ///
    /// Confirmation workers are retained until they finish. `in_flight` is
    /// conservative: the worker may already have submitted, even if it has not
    /// yet produced a receipt. It does not mean the purchase was cancelled.
    pub fn stop(&self, chat_id: Option<i64>) -> StopReport {
        let mut stopped = 0;
        let mut in_flight = false;
        let requests: Vec<Arc<Confirmation>> = self.inner.requests.lock().clone();
        for request in requests {
            if chat_id.is_some_and(|c| request.chat_id != c) {
                continue;
            }
            let mut state = request.state.lock();
            if state.phase == Phase::Confirming {
                in_flight = true;
            } else if !request.is_cancelled() {
                stopped += 1;
            }
            request.cancelled.store(true, Ordering::SeqCst);
            if let Some(asking) = state.asking.take() {
                asking.abort();
            }
        }
        let line = if in_flight {
            let line = "A purchase request may already be in flight. Stopping cannot undo it; \
check its AgentCard status before trying again.";
            if stopped > 0 { format!("Stopped pending purchase confirmations. {line}") } else { line.to_string() }
        } else if stopped > 0 {
            "Stopped. No pending purchase confirmation will be submitted.".to_string()
        } else {
            "No purchase confirmation is waiting.".to_string()
        };
        StopReport { stopped, in_flight, line }
    }

    pub fn shutdown(&self) -> StopReport {
        self.inner.closed.store(true, Ordering::SeqCst);
        self.stop(None)
    }

    /// Observe outstanding workers without cancelling a submitted request.
    pub async fn wait_idle(&self) {
        let waits: Vec<watch::Receiver<()>> = self.inner.requests.lock().iter().map(|r| r.done.clone()).collect();
        for mut done in waits {
            while done.changed().await.is_ok() {}
        }
    }

    /// `chat_id` is the owner chat (Python: an `int` that is not a `bool`);
    /// `None` is any other principal.
    pub async fn handle<F, Fut>(&self, name: &str, args: &Value, chat_id: Option<i64>, ask_user: F) -> Value
    where
        F: FnOnce(String, String) -> Fut + Send + 'static,
        Fut: Future<Output = Result<String, AskError>> + Send + 'static,
    {
        if self.inner.closed.load(Ordering::SeqCst) {
            return not_submitted("AgentCard is shutting down; no new request was submitted.");
        }
        let Some(fields) = args.as_object() else {
            return not_submitted("AgentCard arguments must be an object.");
        };
        if NONCONFIRM_TOOLS.contains(&name) {
            let service = self.inner.service.clone();
            let (name, args) = (name.to_string(), args.clone());
            // Errors can contain private provider bodies: none of their text is kept.
            return match tokio::task::spawn_blocking(move || service.handle(&name, args)).await {
                Ok(Ok(result)) if result.is_object() => result,
                _ => json!({"ok": false, "reason": UNUSABLE}),
            };
        }
        if name != CONFIRM_TOOL {
            return not_submitted("Unknown AgentCard tool.");
        }
        let Some((conversation, hashes)) = selection(fields) else {
            return not_submitted("Name the conversation and one to eight distinct exact cart hashes.");
        };
        let Some(chat_id) = chat_id else {
            return not_submitted("An owner chat is required for purchase approval.");
        };
        // Freeze the model's list before any await or worker handoff. The opaque
        // client snapshot later binds the cart, address and durable state version.
        let (done_tx, done_rx) = watch::channel(());
        let request = Arc::new(Confirmation {
            chat_id,
            cancelled: Arc::new(AtomicBool::new(false)),
            state: Mutex::new(ConfirmationState { phase: Phase::Queued, asking: None }),
            done: done_rx,
        });
        self.inner.requests.lock().push(request.clone());
        let mut guard = WorkerGuard { inner: self.inner.clone(), request: request.clone(), finished: false, _done: done_tx };
        let inner = self.inner.clone();
        let worker_request = request.clone();
        let task = tokio::spawn(async move {
            let out = confirm(&inner, &worker_request, conversation, hashes, ask_user).await;
            guard.finished = true;
            drop(guard);
            out
        });
        let mut caller = CallerGuard { request: request.clone(), armed: true };
        let out = task.await;
        caller.armed = false;
        match out {
            Ok(value) => value,
            Err(_) => failure_in(request.phase()),
        }
    }
}

/// The argument check: exactly `conversation_id` (non-blank) and `cart_hashes`
/// (one to eight distinct exact hashes).
fn selection(fields: &Map<String, Value>) -> Option<(String, Vec<String>)> {
    if fields.len() != 2 {
        return None;
    }
    let conversation = fields.get("conversation_id")?.as_str()?;
    let hashes = fields.get("cart_hashes")?.as_array()?;
    if is_blank(conversation) || !(1..=8).contains(&hashes.len()) {
        return None;
    }
    let mut selected: Vec<String> = Vec::with_capacity(hashes.len());
    for hash in hashes {
        let hash = hash.as_str()?;
        if !CART_HASH.is_match(hash) || selected.iter().any(|h| h == hash) {
            return None;
        }
        selected.push(hash.to_string());
    }
    Some((conversation.to_string(), selected))
}

/// `_confirm`: the lifecycle task. It holds the serialization lock from preparation
/// until the confirm worker returns.
async fn confirm<S, F, Fut>(
    inner: &Arc<Inner<S>>,
    request: &Arc<Confirmation>,
    conversation: String,
    hashes: Vec<String>,
    ask_user: F,
) -> Value
where
    S: Service,
    F: FnOnce(String, String) -> Fut + Send + 'static,
    Fut: Future<Output = Result<String, AskError>> + Send + 'static,
{
    let _serial = inner.lock.lock().await;
    if !request.enter(Phase::Preparing) {
        return not_submitted(STOPPED);
    }
    let service = inner.service.clone();
    let prepared = match tokio::task::spawn_blocking(move || service.prepare_confirm(&conversation, &hashes)).await {
        Ok(Ok(prepared)) => prepared,
        _ => return failure_in(Phase::Preparing),
    };
    if request.is_cancelled() {
        return not_submitted(STOPPED);
    }
    let Prepared { result, snapshot } = prepared;
    let Some(fields) = result.as_object() else {
        return not_submitted(NOT_PREPARED);
    };
    if fields.get("ok") != Some(&Value::Bool(true)) {
        return result;
    }
    let question = fields.get("question").and_then(Value::as_str);
    let about = fields.get("about").and_then(Value::as_str);
    let (Some(snapshot), Some(question), Some(about)) = (snapshot, question, about) else {
        return not_submitted(NOT_PREPARED);
    };
    if is_blank(question) {
        return not_submitted(NOT_PREPARED);
    }
    // Built outside the request lock: the inlet's callable may itself call stop().
    let question_future = ask_user(question.to_string(), about.to_string());
    let asking = {
        let mut state = request.state.lock();
        if request.is_cancelled() {
            return not_submitted(STOPPED);
        }
        state.phase = Phase::Asking;
        let asking = tokio::spawn(question_future);
        state.asking = Some(asking.abort_handle());
        asking
    };
    let answer = match asking.await {
        Ok(Ok(answer)) => answer,
        Ok(Err(AskError::Timeout)) => {
            return not_submitted("The owner did not approve in time; no purchase was submitted.");
        }
        Err(join) if join.is_cancelled() => {
            request.cancel();
            return not_submitted(STOPPED);
        }
        Ok(Err(AskError::Failed(_))) | Err(_) => return failure_in(Phase::Asking),
    };
    if request.is_cancelled() {
        return not_submitted(STOPPED);
    }
    if eve_consent::bare_decision(&answer) != "allow" {
        return not_submitted("The owner did not approve; do not retry this purchase automatically.");
    }
    if !request.enter(Phase::Confirming) {
        return not_submitted(STOPPED);
    }
    let service = inner.service.clone();
    let cancelled = request.cancelled.clone();
    match tokio::task::spawn_blocking(move || service.confirm(snapshot, &cancelled)).await {
        Ok(Ok(result)) if result.is_object() => result,
        _ => unknown_outcome(),
    }
}

#[cfg(test)]
mod tests;
