//! TypeSafe's Jev behind `eve_decider::Decider`'s `predict(state, questions)` seam (port of `jev.py`).
//!
//! `Decider` is written against the System One signature, and Jev speaks that signature over HTTP:
//! `{"model", "state", "questions"}` in, `{"answers": {qid: {...}}}` out, each `choice` answer carrying
//! `choice` + `probabilities` + `confidence`. That is the envelope `Decider`'s parser already reads, so
//! this crate is a transport, not a translation: `predict` hands the decoded body straight back.
//!
//! The shape is taken from browser-use/jev-ultrafast (MIT, `jev_ultrafast/model.py`), which posts to the
//! same service: `state` may be an object, `criteria` values may be objects, and `answers` is keyed by the
//! question id the caller chose.
//!
//! TWO ROUTES, one envelope:
//!   "typesafe"   POST https://api.typesafe.ai/v1/systemone      Bearer TYPESAFE_API_KEY
//!   "openrouter" POST https://openrouter.ai/api/alpha/decisions Bearer OPENROUTER_API_KEY
//! The first is the one jev-ultrafast exercises and the default here. The second is alpha; it answers in
//! the same envelope and is the route this Mac runs (2026-09-21: the head classifier, the request router,
//! and tools/jev_step_eval.py's 155 four-question requests with 26 options each, 234 ms p50). A different
//! envelope would read as `error`, never a wrong click.
//!
//! WHAT THIS CHANGES ABOUT THE BUDGET: `Decider` trims the tail of the option list before predict to fit a
//! small decision head (its default is 256 tokens). Jev takes 32k. The budgets below are set past any real
//! AX menu, so `trim_options` never drops an option and `overflow` never fires — the two numbers the fast
//! lane gates on stay about the model, not about the wrapper.
//!
//! WHAT IT COSTS: Jev is a network call; TypeSafe publish 70-500 ms, this Mac measures 234 ms p50. Against
//! the fast lane's own cost model (+3.5 s a right click, -4.55 s a wrong one) half a second of latency is
//! small and accuracy is the whole question, which is why `timeout_s` is short and every failure escalates
//! to the planner rather than waiting.
//!
//! WHAT IT COSTS THE OTHER WAY: Jev is a third party, and the state carries the focused window's title and
//! visible text. The daemon only builds that state during a computer-use task the user spoke, but it is a
//! new egress and the README's "nothing leaves the Mac" no longer covers this lane when it is on. It is off
//! by default.
//!
//! `predict` is synchronous, as in Python (urllib): call it from a plain thread or `spawn_blocking`. Called
//! from inside a tokio runtime it moves the HTTP work to a scoped thread so reqwest's blocking client never
//! runs on a runtime worker.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::hash::BuildHasher;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Map, Value};

use eve_decider::{Decider, DeciderConfig, PredictError, WARMUP_CONTEXT, WARMUP_OBJECTIVE, WARMUP_OPTIONS};

#[cfg(test)]
mod tests;

// ---- constants -------------------------------------------------------------------------------------------

/// One route: where to post, which env var holds the key, and the model it defaults to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    pub url: &'static str,
    pub key_name: &'static str,
    pub default_model: &'static str,
}

/// `ROUTES`, in Python's dict order.
pub const ROUTES: [(&str, Route); 2] = [
    (
        "typesafe",
        Route { url: "https://api.typesafe.ai/v1/systemone", key_name: "TYPESAFE_API_KEY", default_model: "jev-latest" },
    ),
    (
        "openrouter",
        Route {
            url: "https://openrouter.ai/api/alpha/decisions",
            key_name: "OPENROUTER_API_KEY",
            default_model: "typesafe/jev-1.13",
        },
    ),
];
pub const DEFAULT_ROUTE: &str = "typesafe";
/// A click-path budget, not jev-ultrafast's 25 s browser budget.
pub const DEFAULT_TIMEOUT_S: f64 = 3.0;
/// Jev 1.13 context, per OpenRouter's /models endpoint (2026-09-21).
pub const MAX_LEN: i64 = 32000;
/// Past any real AX menu: `trim_options` must never drop an option.
pub const HEAD_MAX_LEN: i64 = 30000;
pub const RETRY_STATUS: [u16; 5] = [429, 500, 502, 503, 529];
/// jev-ultrafast retries twice with 0.5 s * 2**n; one click step affords less.
pub const RETRIES: u32 = 2;

/// The route named `name` (already lower-cased), if there is one.
pub fn route(name: &str) -> Option<&'static Route> {
    ROUTES.iter().find(|(n, _)| *n == name).map(|(_, r)| r)
}

// ---- errors ----------------------------------------------------------------------------------------------

/// One line, no traceback: `Decider::choose` turns this into `Choice.error`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct JevError(pub String);

/// What a `predict` call can fail with: a [`JevError`] for everything `jev.py` catches, or an exception
/// Python's urllib lets through uncaught (a read timeout while waiting for the reply raises a bare
/// `TimeoutError`, which reaches the caller unmetered as `predict raised TimeoutError: timed out`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PredictFailure {
    #[error("{0}")]
    Jev(#[from] JevError),
    #[error("{message}")]
    Raised { kind: String, message: String },
}

impl PredictFailure {
    /// The Python exception class this failure stands for.
    pub fn kind(&self) -> &str {
        match self {
            PredictFailure::Jev(_) => "JevError",
            PredictFailure::Raised { kind, .. } => kind,
        }
    }
}

impl From<PredictFailure> for PredictError {
    fn from(e: PredictFailure) -> Self {
        PredictError::new(e.kind(), e.to_string())
    }
}

/// What `load` raises: Python's `RuntimeError` (no key, a failed warm-up) or the `ValueError` `Decider`
/// raises for a bad style. `Display` is `str(e)`; `kind` is `type(e).__name__`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct LoadError {
    pub kind: &'static str,
    pub message: String,
}

impl LoadError {
    fn runtime(message: impl Into<String>) -> Self {
        Self { kind: "RuntimeError", message: message.into() }
    }
}

// ---- the environment -------------------------------------------------------------------------------------

/// Python's `Mapping[str, str]` for the env: `env.get(name)`.
pub trait Env {
    fn var(&self, name: &str) -> Option<String>;
}

impl<S: BuildHasher> Env for HashMap<String, String, S> {
    fn var(&self, name: &str) -> Option<String> {
        self.get(name).cloned()
    }
}

impl Env for BTreeMap<String, String> {
    fn var(&self, name: &str) -> Option<String> {
        self.get(name).cloned()
    }
}

/// `os.environ`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnv;

impl Env for ProcessEnv {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

/// `env.get(name) or ""`.
fn get<E: Env + ?Sized>(env: &E, name: &str) -> String {
    env.var(name).unwrap_or_default()
}

// ---- the meter -------------------------------------------------------------------------------------------

/// [`Meter::snapshot`]: the dict Python returns, keys in the same order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct MeterSnapshot {
    pub calls: i64,
    pub input_tokens: i64,
    pub ms: i64,
    pub errors: i64,
}

#[derive(Debug, Default)]
struct MeterState {
    calls: i64,
    input_tokens: i64,
    ms: f64,
    errors: i64,
}

/// What Jev has been asked so far in this process: calls, input tokens (the only billed kind) and
/// milliseconds, from every `predict` this crate makes. The computer agent snapshots it at the start and the
/// end of a run and writes the difference into the run log's bill (`eve_pricing::estimate_jev_cost`).
#[derive(Debug, Default)]
pub struct Meter {
    state: Mutex<MeterState>,
}

/// A JSON integer that is not a bool, as `isinstance(x, int) and not isinstance(x, bool)` reads it.
/// Integers past `i64` saturate.
fn json_int(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Number(n) if n.is_i64() => n.as_i64(),
        Value::Number(n) if n.is_u64() => Some(i64::MAX),
        _ => None,
    }
}

/// `payload.get("usage")` when the payload is an object and its usage is one.
fn usage_of(payload: &Value) -> Option<&Map<String, Value>> {
    payload.as_object()?.get("usage")?.as_object()
}

impl Meter {
    pub const fn new() -> Self {
        Self { state: Mutex::new(MeterState { calls: 0, input_tokens: 0, ms: 0.0, errors: 0 }) }
    }

    /// One call: its reply (for `usage.input_tokens`), its milliseconds, and whether it failed.
    pub fn add(&self, payload: &Value, ms: f64, error: bool) {
        let tokens = usage_of(payload).and_then(|u| json_int(u.get("input_tokens")));
        let mut s = self.state.lock();
        s.calls += 1;
        s.ms += ms;
        s.errors += i64::from(error);
        if let Some(t) = tokens {
            s.input_tokens = s.input_tokens.saturating_add(t.max(0));
        }
    }

    /// `{"calls", "input_tokens", "ms", "errors"}`, `ms` rounded half to even as Python's `round`.
    pub fn snapshot(&self) -> MeterSnapshot {
        let s = self.state.lock();
        MeterSnapshot { calls: s.calls, input_tokens: s.input_tokens, ms: s.ms.round_ties_even() as i64, errors: s.errors }
    }

    /// `after - before`, key by key.
    pub fn delta(before: &MeterSnapshot, after: &MeterSnapshot) -> MeterSnapshot {
        MeterSnapshot {
            calls: after.calls - before.calls,
            input_tokens: after.input_tokens - before.input_tokens,
            ms: after.ms - before.ms,
            errors: after.errors - before.errors,
        }
    }
}

/// The process-wide meter every `predict` adds to.
pub static METER: Meter = Meter::new();

// ---- the spend meter -------------------------------------------------------------------------------------

/// One answered decision into the daily spend meter (eve-spend): OpenRouter's own `usage.cost` when its reply
/// carries one, else the input tokens at Jev's rate (`eve_pricing::JEV_INPUT_PER_M`; output is free). The
/// provider is the route's host, so the owner sees which bill it lands on.
pub fn meter_spend(url: &str, model: &str, payload: &Value) {
    meter_spend_in(&eve_spend::Ledger::default(), url, model, payload)
}

/// [`meter_spend`] into a given ledger.
pub fn meter_spend_in(ledger: &eve_spend::Ledger, url: &str, model: &str, payload: &Value) {
    let empty = Map::new();
    let usage = usage_of(payload).unwrap_or(&empty);
    let provider = if url.contains("openrouter.ai") { "openrouter" } else { "typesafe" };
    let tokens = json_int(usage.get("input_tokens")).unwrap_or(0);
    let cost = match usage.get("cost") {
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    };
    if let Some(cost) = cost {
        let opts = eve_spend::RecordOpts::new().tokens([("in", json!(tokens))]).source("reported");
        ledger.record(provider, model, eve_spend::JEV, Some(cost), opts);
    } else if !usage.is_empty() {
        let opts = eve_spend::RecordOpts::new().tokens([("in", json!(tokens))]);
        ledger.record(provider, model, eve_spend::JEV, Some(eve_pricing::estimate_jev_cost(tokens)), opts);
    } else {
        let opts = eve_spend::RecordOpts::new().note("no usage in the reply");
        ledger.record(provider, model, eve_spend::JEV, None, opts);
    }
}

// ---- configuration ---------------------------------------------------------------------------------------

/// Python's `float(raw)` for an already stripped string: Rust's parser plus Python's digit-group
/// underscores (`1_000`), which are allowed only between two digits.
fn py_float(raw: &str) -> Option<f64> {
    let cleaned;
    let text = if raw.contains('_') {
        let bytes = raw.as_bytes();
        for (i, b) in bytes.iter().enumerate() {
            if *b == b'_' {
                let before = i > 0 && bytes[i - 1].is_ascii_digit();
                let after = bytes.get(i + 1).is_some_and(u8::is_ascii_digit);
                if !(before && after) {
                    return None;
                }
            }
        }
        cleaned = raw.replace('_', "");
        cleaned.as_str()
    } else {
        raw
    };
    text.parse::<f64>().ok()
}

/// `EVE_JEV_TIMEOUT` in seconds, or [`DEFAULT_TIMEOUT_S`] when it is unset, blank, not a number, or not a
/// positive finite one. One reader for every caller: a typo here used to raise out of the asker's builder
/// (desktop_worker, browser_lane) or quietly switch the command-risk gate off (daemon).
pub fn timeout_from_env<E: Env + ?Sized>(env: &E) -> f64 {
    let raw = get(env, "EVE_JEV_TIMEOUT");
    let raw = raw.trim();
    if raw.is_empty() {
        return DEFAULT_TIMEOUT_S;
    }
    let seconds = py_float(raw).unwrap_or(f64::NAN);
    if !seconds.is_finite() || seconds <= 0.0 {
        let shown: String = raw.chars().take(20).collect();
        tracing::warn!(
            "jev: EVE_JEV_TIMEOUT={} is not a positive number of seconds; using {:.1}",
            eve_decider::py_str_repr(&shown),
            DEFAULT_TIMEOUT_S
        );
        return DEFAULT_TIMEOUT_S;
    }
    seconds
}

/// `(url, key, model)` for `route` ("" reads `EVE_JEV_ROUTE`, then [`DEFAULT_ROUTE`]). Fails when the route's
/// key is unset.
pub fn route_config<E: Env + ?Sized>(env: &E, route_name: &str) -> Result<(String, String, String), JevError> {
    let chosen = if !route_name.is_empty() {
        route_name.to_string()
    } else {
        let from_env = get(env, "EVE_JEV_ROUTE");
        if from_env.is_empty() {
            DEFAULT_ROUTE.to_string()
        } else {
            from_env
        }
    };
    let name = chosen.trim().to_lowercase();
    let Some(r) = route(&name) else {
        let names: Vec<String> = ROUTES.iter().map(|(n, _)| eve_decider::py_str_repr(n)).collect();
        return Err(JevError(format!(
            "EVE_JEV_ROUTE must be one of ({}), not {}",
            names.join(", "),
            eve_decider::py_str_repr(&name)
        )));
    };
    let key = get(env, r.key_name).trim().to_string();
    if key.is_empty() {
        return Err(JevError(format!("{} is not set (route {name})", r.key_name)));
    }
    let pinned = get(env, "EVE_JEV_MODEL").trim().to_string();
    let model = if pinned.is_empty() { r.default_model.to_string() } else { pinned };
    Ok((r.url.to_string(), key, model))
}

// ---- the transport seam ----------------------------------------------------------------------------------

/// The request `predict` hands its opener: urllib's `Request` as the tests read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub data: Vec<u8>,
}

impl Request {
    /// `request.get_header(name)`, case-insensitive.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// How an open fails, in urllib's terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// `urllib.error.HTTPError`: the server answered with a non-2xx status.
    Http(u16),
    /// `urllib.error.URLError`: the request never got an answer (`reason` is its text).
    Url(String),
    /// A `ValueError` or `TypeError` (Python's class name), e.g. a URL urllib cannot open.
    Value(String),
    /// Any other exception urllib lets through, e.g. a read timeout's bare `TimeoutError`.
    Raised { kind: String, message: String },
}

/// The socket seam (Python's `opener`, urllib's `urlopen(request, timeout=...)`): the raw reply body.
pub trait Opener: Send + Sync {
    fn open(&self, request: &Request, timeout_s: f64) -> Result<Vec<u8>, OpenError>;
}

impl<F> Opener for F
where
    F: Fn(&Request, f64) -> Result<Vec<u8>, OpenError> + Send + Sync,
{
    fn open(&self, request: &Request, timeout_s: f64) -> Result<Vec<u8>, OpenError> {
        self(request, timeout_s)
    }
}

/// Run `f` on a plain thread when called from inside a tokio runtime, where reqwest's blocking client may not
/// be built, used or dropped. Python reaches this code from a plain thread; this keeps an async caller safe.
fn off_runtime<R: Send>(f: impl FnOnce() -> R + Send) -> Option<R> {
    if tokio::runtime::Handle::try_current().is_err() {
        return Some(f());
    }
    std::thread::scope(|s| s.spawn(f).join().ok())
}

/// The text of the innermost cause, as urllib's `URLError.reason` names the socket error.
fn innermost(e: &(dyn std::error::Error + 'static)) -> String {
    let mut cur = e;
    while let Some(next) = cur.source() {
        cur = next;
    }
    cur.to_string()
}

/// A reqwest failure in urllib's terms: a failure to connect or send is a `URLError`; a timeout waiting for
/// the reply is the bare `TimeoutError` urllib lets through; a broken body read is an `OSError`.
fn transport_error(e: &reqwest::Error) -> OpenError {
    if e.is_timeout() {
        if e.is_connect() {
            return OpenError::Url("timed out".into());
        }
        return OpenError::Raised { kind: "TimeoutError".into(), message: "timed out".into() };
    }
    if e.is_builder() {
        return OpenError::Value("ValueError".into());
    }
    if e.is_body() || e.is_decode() {
        return OpenError::Raised { kind: "OSError".into(), message: innermost(e) };
    }
    OpenError::Url(innermost(e))
}

/// urllib's `urlopen` over reqwest's blocking client, built once per `predict` with its timeout.
struct HttpOpener {
    client: Mutex<Option<reqwest::blocking::Client>>,
}

impl HttpOpener {
    fn send(&self, request: &Request, timeout_s: f64) -> Result<Vec<u8>, OpenError> {
        // socket.settimeout raises ValueError for a negative or non-finite timeout; jev.py reads it as undecodable.
        let timeout = Duration::try_from_secs_f64(timeout_s).map_err(|_| OpenError::Value("ValueError".into()))?;
        let client = {
            let mut held = self.client.lock();
            if held.is_none() {
                let built = reqwest::blocking::Client::builder()
                    .timeout(timeout)
                    .connect_timeout(timeout)
                    .build()
                    .map_err(|e| transport_error(&e))?;
                *held = Some(built);
            }
            held.clone().ok_or_else(|| OpenError::Url("no client".into()))?
        };
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|_| OpenError::Value("ValueError".into()))?;
        let mut builder = client.request(method, &request.url).body(request.data.clone());
        for (k, v) in &request.headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        let response = builder.send().map_err(|e| transport_error(&e))?;
        let status = response.status();
        if !status.is_success() {
            return Err(OpenError::Http(status.as_u16()));
        }
        Ok(response.bytes().map_err(|e| transport_error(&e))?.to_vec())
    }
}

impl Opener for HttpOpener {
    fn open(&self, request: &Request, timeout_s: f64) -> Result<Vec<u8>, OpenError> {
        off_runtime(|| self.send(request, timeout_s))
            .unwrap_or_else(|| Err(OpenError::Raised { kind: "RuntimeError".into(), message: "transport thread panicked".into() }))
    }
}

impl Drop for HttpOpener {
    fn drop(&mut self) {
        if let Some(client) = self.client.get_mut().take() {
            off_runtime(move || drop(client));
        }
    }
}

// ---- predict ---------------------------------------------------------------------------------------------

/// Python's keyword arguments to `make_predict`, with the same defaults, plus the meters as seams.
#[derive(Clone)]
pub struct PredictOptions {
    pub timeout_s: f64,
    /// `opener=`: stands in for urllib's `urlopen` so tests run without a socket. `None` is the real HTTPS.
    pub opener: Option<Arc<dyn Opener>>,
    /// `sleep=`: the back-off between retries.
    pub sleep: Arc<dyn Fn(f64) + Send + Sync>,
    /// The call meter; `None` is the process-wide [`METER`].
    pub meter: Option<Arc<Meter>>,
    /// The spend ledger; `None` is the configured one (`eve_spend::Ledger::default()`).
    pub ledger: Option<eve_spend::Ledger>,
}

impl Default for PredictOptions {
    fn default() -> Self {
        Self {
            timeout_s: DEFAULT_TIMEOUT_S,
            opener: None,
            sleep: Arc::new(|s: f64| {
                if let Ok(d) = Duration::try_from_secs_f64(s) {
                    std::thread::sleep(d)
                }
            }),
            meter: None,
            ledger: None,
        }
    }
}

impl fmt::Debug for PredictOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PredictOptions")
            .field("timeout_s", &self.timeout_s)
            .field("opener", &self.opener.is_some())
            .field("ledger", &self.ledger)
            .finish_non_exhaustive()
    }
}

struct Inner {
    url: String,
    key: String,
    model: String,
    timeout_s: f64,
    opener: Arc<dyn Opener>,
    sleep: Arc<dyn Fn(f64) + Send + Sync>,
    meter: Option<Arc<Meter>>,
    ledger: eve_spend::Ledger,
}

/// A `predict(state, questions)` that posts one decision request and returns the body. Cheap to clone.
#[derive(Clone)]
pub struct JevPredict {
    inner: Arc<Inner>,
}

impl fmt::Debug for JevPredict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print the key.
        f.debug_struct("JevPredict")
            .field("url", &self.inner.url)
            .field("model", &self.inner.model)
            .field("timeout_s", &self.inner.timeout_s)
            .finish_non_exhaustive()
    }
}

/// A `predict(state, questions)` that posts one decision request and returns the body.
///
/// `options.opener` takes the request and the timeout so the tests run without a socket. Fails with a
/// [`JevError`] on anything that is not a decoded JSON object; `Decider` catches it.
pub fn make_predict(url: &str, key: &str, model: &str, options: PredictOptions) -> JevPredict {
    let opener: Arc<dyn Opener> = match options.opener {
        Some(o) => o,
        None => Arc::new(HttpOpener { client: Mutex::new(None) }),
    };
    JevPredict {
        inner: Arc::new(Inner {
            url: url.to_string(),
            key: key.to_string(),
            model: model.to_string(),
            timeout_s: options.timeout_s,
            opener,
            sleep: options.sleep,
            meter: options.meter,
            ledger: options.ledger.unwrap_or_default(),
        }),
    }
}

/// `type(payload).__name__` for a decoded JSON value.
fn py_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

impl JevPredict {
    pub fn url(&self) -> &str {
        &self.inner.url
    }

    pub fn model(&self) -> &str {
        &self.inner.model
    }

    pub fn timeout_s(&self) -> f64 {
        self.inner.timeout_s
    }

    fn meter(&self) -> &Meter {
        self.inner.meter.as_deref().unwrap_or(&METER)
    }

    /// One decision request: the decoded body, normalized (see [`normalize`]).
    pub fn predict(&self, state: &Value, questions: &Value) -> Result<Value, PredictFailure> {
        let me = &*self.inner;
        let body = py_dumps(&json!({"model": me.model, "state": state, "questions": questions})).into_bytes();
        let t0 = Instant::now();
        let elapsed = || t0.elapsed().as_secs_f64() * 1000.0;
        let fail = |msg: String| -> PredictFailure {
            self.meter().add(&json!({}), elapsed(), true);
            PredictFailure::Jev(JevError(msg))
        };
        for attempt in 0..=RETRIES {
            let request = Request {
                url: me.url.clone(),
                method: "POST".into(),
                headers: vec![
                    ("Authorization".into(), format!("Bearer {}", me.key)),
                    ("Content-Type".into(), "application/json".into()),
                ],
                data: body.clone(),
            };
            let raw = match me.opener.open(&request, me.timeout_s) {
                Ok(raw) => raw,
                Err(OpenError::Http(code)) => {
                    if RETRY_STATUS.contains(&code) && attempt < RETRIES {
                        (me.sleep)(0.2 * f64::from(2u32.pow(attempt)));
                        continue;
                    }
                    return Err(fail(format!("HTTP {code} from {}", me.url)));
                }
                Err(OpenError::Url(reason)) => return Err(fail(format!("{} unreachable: {reason}", me.url))),
                Err(OpenError::Value(kind)) => return Err(fail(format!("undecodable response from {}: {kind}", me.url))),
                Err(OpenError::Raised { kind, message }) => return Err(PredictFailure::Raised { kind, message }),
            };
            let text = match std::str::from_utf8(&raw) {
                Ok(t) => t,
                Err(_) => return Err(fail(format!("undecodable response from {}: UnicodeDecodeError", me.url))),
            };
            let payload: Value = match serde_json::from_str(text) {
                Ok(v) => v,
                Err(_) => return Err(fail(format!("undecodable response from {}: JSONDecodeError", me.url))),
            };
            if !payload.is_object() {
                return Err(fail(format!("{} returned {}, not an object", me.url, py_type_name(&payload))));
            }
            self.meter().add(&payload, elapsed(), false);
            meter_spend_in(&me.ledger, &me.url, &me.model, &payload);
            return Ok(normalize(payload, questions));
        }
        Err(fail(format!("{} kept returning a retryable status", me.url)))
    }

    /// This predict in `eve_decider::Decider`'s seam (`Fn(&Value, &Value) -> Result<Value, PredictError>`).
    pub fn decider_fn(&self) -> impl Fn(&Value, &Value) -> Result<Value, PredictError> + Send + Sync + 'static {
        let me = self.clone();
        move |state: &Value, questions: &Value| me.predict(state, questions).map_err(PredictError::from)
    }

    /// This predict as `Fn(&Value, &Value) -> anyhow::Result<Value>` (the typed askers' seam). The error is a
    /// [`PredictFailure`], so callers can downcast it for the Python class name.
    pub fn anyhow_fn(&self) -> impl Fn(&Value, &Value) -> anyhow::Result<Value> + Send + Sync + 'static {
        let me = self.clone();
        move |state: &Value, questions: &Value| me.predict(state, questions).map_err(anyhow::Error::from)
    }
}

/// The body as `Decider` reads it: `{"answers": {qid: answer}, "usage": {...}}`.
///
/// TypeSafe's own service already answers in that envelope (jev_ultrafast/model.py reads
/// `result["answers"][qid]`), and so does OpenRouter's /api/alpha/decisions (the route this Mac runs), so the
/// common path returns `payload` untouched. The one shape worth accepting beside it is the answers keyed at
/// the top level. Anything else is passed through unchanged and fails in the parser as "predict returned no
/// answer", which the lane reads as unavailable — a wrong envelope must never become a click.
pub fn normalize(payload: Value, questions: &Value) -> Value {
    let Value::Object(obj) = payload else {
        return payload;
    };
    if obj.get("answers").is_some_and(Value::is_object) {
        return Value::Object(obj);
    }
    let keys: Vec<&String> = questions.as_object().map(|q| q.keys().collect()).unwrap_or_default();
    if !keys.is_empty() && keys.iter().all(|k| obj.get(k.as_str()).is_some_and(Value::is_object)) {
        let mut answers = Map::new();
        let mut rest = Map::new();
        for (k, v) in obj {
            if keys.iter().any(|q| **q == k) {
                answers.insert(k, v);
            } else {
                rest.insert(k, v);
            }
        }
        // {k: payload[k] for k in keys}: in the questions' order.
        let mut ordered = Map::new();
        for k in keys {
            if let Some(v) = answers.remove(k.as_str()) {
                ordered.insert(k.clone(), v);
            }
        }
        // {**rest, "answers": ...}: an existing "answers" key keeps its place.
        rest.insert("answers".into(), Value::Object(ordered));
        return Value::Object(rest);
    }
    Value::Object(obj)
}

// ---- load ------------------------------------------------------------------------------------------------

/// Python's keyword arguments to `load`, with the same defaults.
#[derive(Clone)]
pub struct LoadOptions {
    pub style: String,
    /// "" reads `EVE_JEV_ROUTE`.
    pub route: String,
    /// 0 reads `EVE_JEV_TIMEOUT` ([`timeout_from_env`]).
    pub timeout_s: f64,
    pub opener: Option<Arc<dyn Opener>>,
    /// The call meter and spend ledger seams of [`PredictOptions`].
    pub meter: Option<Arc<Meter>>,
    pub ledger: Option<eve_spend::Ledger>,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self { style: "jev".into(), route: String::new(), timeout_s: 0.0, opener: None, meter: None, ledger: None }
    }
}

/// A `Decider` backed by Jev, warmed up once so `available` means a real answer came back.
///
/// `env` is Python's `env` (pass [`ProcessEnv`] for `os.environ`, Python's `None`). The warm-up menu is `eve_decider::WARMUP_OPTIONS`; any failure
/// is a one-line [`LoadError`], which the desktop worker turns into the lane's "failed: …" status.
pub fn load<E: Env + ?Sized>(source: &E, options: LoadOptions) -> Result<Decider, LoadError> {
    let t0 = Instant::now();
    let seconds = if options.timeout_s != 0.0 { options.timeout_s } else { timeout_from_env(source) };
    let (url, key, model) = route_config(source, &options.route).map_err(|e| LoadError::runtime(e.0))?;
    let predict = make_predict(
        &url,
        &key,
        &model,
        PredictOptions {
            timeout_s: seconds,
            opener: options.opener,
            meter: options.meter,
            ledger: options.ledger,
            ..PredictOptions::default()
        },
    );
    let config = DeciderConfig { max_len: MAX_LEN, head_max_len: HEAD_MAX_LEN, style: options.style, ..DeciderConfig::default() };
    let mut decider = Decider::new(predict.decider_fn(), config)
        .map_err(|e| LoadError { kind: e.kind(), message: e.to_string() })?;
    decider.load_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let t1 = Instant::now();
    let warm = decider
        .choose(WARMUP_OBJECTIVE, "Calendar", &WARMUP_CONTEXT, &WARMUP_OPTIONS, &[])
        .map_err(|e| LoadError { kind: e.kind(), message: e.to_string() })?;
    decider.warm_ms = t1.elapsed().as_secs_f64() * 1000.0;
    if !warm.error.is_empty() {
        return Err(LoadError::runtime(format!("jev warm-up predict failed: {}", warm.error)));
    }
    decider.available = true;
    Ok(decider)
}

// ---- the request body ------------------------------------------------------------------------------------

/// `json.dumps(value)` with Python's defaults: `", "` and `": "` separators, `ensure_ascii` escapes, Python's
/// float repr.
pub fn py_dumps(value: &Value) -> String {
    struct PyFormatter;
    impl serde_json::ser::Formatter for PyFormatter {
        fn begin_array_value<W: ?Sized + std::io::Write>(&mut self, w: &mut W, first: bool) -> std::io::Result<()> {
            if first {
                Ok(())
            } else {
                w.write_all(b", ")
            }
        }
        fn begin_object_key<W: ?Sized + std::io::Write>(&mut self, w: &mut W, first: bool) -> std::io::Result<()> {
            if first {
                Ok(())
            } else {
                w.write_all(b", ")
            }
        }
        fn begin_object_value<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
            w.write_all(b": ")
        }
        fn write_f64<W: ?Sized + std::io::Write>(&mut self, w: &mut W, value: f64) -> std::io::Result<()> {
            w.write_all(eve_spend::py_float_repr(value).as_bytes())
        }
        fn write_string_fragment<W: ?Sized + std::io::Write>(&mut self, w: &mut W, fragment: &str) -> std::io::Result<()> {
            // ensure_ascii: everything outside ' '..'~' as \uXXXX (UTF-16, surrogate pairs past the BMP).
            let mut start = 0;
            for (i, c) in fragment.char_indices() {
                if (' '..='~').contains(&c) {
                    continue;
                }
                w.write_all(&fragment.as_bytes()[start..i])?;
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    write!(w, "\\u{:04x}", u)?;
                }
                start = i + c.len_utf8();
            }
            w.write_all(&fragment.as_bytes()[start..])
        }
    }
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, PyFormatter);
    if Serialize::serialize(value, &mut ser).is_err() {
        return String::new();
    }
    String::from_utf8(out).unwrap_or_default()
}
