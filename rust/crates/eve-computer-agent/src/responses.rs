//! The real Responses client: responses.create (plain and streamed) on OpenRouter, with a TLS drop retried on a
//! fresh connection.
//!
//! The SDK retries its own connection errors, but a TLS read that fails AFTER the handshake came out of anyio as
//! a bare ssl.SSLError, which httpcore2 left unmapped and the SDK neither wrapped nor retried. Live, 2026-09-28:
//! three phone-call turns in one morning died on one each, "telegram: turn failed: SSLError".
//!
//! One immediate retry on the same client was not enough. Live, 2026-09-30 11:11:31: "TLS dropped mid-request
//! (SSLV3_ALERT_BAD_RECORD_MAC); once more", then 120 ms later "turn failed: SSLError". The retry went back into
//! the same client, so the same connection pool, with no pause. The pool closes only the one connection that
//! raised; any other keep-alive connection in that pool is handed out first, and one opened in the same window is
//! as stale as the one that just failed. The cure: each retry waits a moment and goes out on a NEW client (a new
//! pool, so a new TCP and TLS connection), and the old client is closed once nothing is using it.
//!
//! In Rust the same failure is a reply body that breaks after the headers arrived (reqwest's body or decode
//! error): eve-openrouter's own retry loop has already retried what the SDK retried (connect, timeout, request),
//! so what reaches this layer as [`SslError`] is exactly the class Python saw as `ssl.SSLError`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::StreamExt;
use parking_lot::Mutex;
use serde_json::Value;

use crate::agent::{CreateResponse, SleepFn};
use crate::{is_a, py, runtime, PyError};

/// the wait before each retry, one entry per retry
pub const TLS_BACKOFF: [f64; 2] = [0.3, 1.0];
/// so at most three tries
pub const TLS_RETRIES: usize = TLS_BACKOFF.len();

/// The connection's TLS layer broke mid-request (Python's `ssl.SSLError`): `reason` is the OpenSSL reason code
/// when one is known ("SSLV3_ALERT_BAD_RECORD_MAC"), else "".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SslError {
    pub reason: String,
    pub message: String,
}

impl SslError {
    pub fn new(reason: impl Into<String>, message: impl Into<String>) -> Self {
        SslError { reason: reason.into(), message: message.into() }
    }
}

/// `e.reason or type(e).__name__`.
fn drop_reason(e: &anyhow::Error) -> String {
    e.chain()
        .find_map(|c| c.downcast_ref::<SslError>())
        .map(|s| if s.reason.is_empty() { "SSLError".to_string() } else { s.reason.clone() })
        .unwrap_or_else(|| "SSLError".to_string())
}

fn is_tls_drop(e: &anyhow::Error) -> bool {
    is_a::<SslError>(e) || e.chain().any(|c| c.downcast_ref::<PyError>().is_some_and(|p| p.kind == "SSLError"))
}

/// The connection to the model broke before its answer arrived: a TLS drop, a reset socket, or the client's own
/// connection error after its retries. Not an API refusal, not a bug in eve.
pub fn is_connection_drop(e: &anyhow::Error) -> bool {
    use std::io::ErrorKind as K;
    for cause in e.chain() {
        if cause.downcast_ref::<SslError>().is_some() {
            return true;
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            if matches!(io.kind(), K::ConnectionReset | K::ConnectionAborted | K::ConnectionRefused | K::BrokenPipe) {
                return true;
            }
        }
        if let Some(eve_openrouter::Error::Http(_)) = cause.downcast_ref::<eve_openrouter::Error>() {
            return true;
        }
        if let Some(p) = cause.downcast_ref::<PyError>() {
            const DROPS: [&str; 8] = [
                "SSLError",
                "ConnectionError",
                "ConnectionResetError",
                "ConnectionAbortedError",
                "ConnectionRefusedError",
                "BrokenPipeError",
                "APIConnectionError",
                "APITimeoutError",
            ];
            if DROPS.contains(&p.kind.as_str()) {
                return true;
            }
        }
    }
    false
}

/// What the creators need from a client: `responses.create`, `responses.stream` and `close`. The OpenRouter
/// client implements it; tests pass fakes.
#[async_trait]
pub trait ResponsesClient: Send + Sync {
    /// `responses.create(**request).model_dump(exclude_none=True)`. A `"timeout"` key is the request's timeout in
    /// seconds, not part of the body (as the SDK reads it).
    async fn create(&self, request: Value) -> anyhow::Result<Value>;

    /// `responses.stream(**request)`: every event goes to `on_event` (its JSON, with `"type"` and, for a text
    /// delta, `"delta"`), and the final response comes back as the stream helper's `get_final_response()`
    /// dumped with `exclude_none`.
    async fn stream(&self, request: Value, on_event: &(dyn for<'v> Fn(&'v Value) + Send + Sync)) -> anyhow::Result<Value>;

    /// Close the client's connection pool. Never fails.
    async fn close(&self) {}
}

/// The request's `"timeout"` (seconds) taken out of the body and applied to a copy of the client.
fn split_timeout(client: &eve_openrouter::Client, mut request: Value) -> (eve_openrouter::Client, Value) {
    let t = request.as_object_mut().and_then(|m| m.shift_remove("timeout")).and_then(|v| v.as_f64());
    let c = match t.and_then(|s| Duration::try_from_secs_f64(s).ok()) {
        Some(d) => client.with_options(Some(d), None),
        None => client.clone(),
    };
    (c, request)
}

/// An eve-openrouter failure as the SDK raised it: a reply body that broke after the headers is the TLS drop.
fn map_err(e: eve_openrouter::Error) -> anyhow::Error {
    if let eve_openrouter::Error::Http(h) = &e {
        if !h.is_timeout() && (h.is_body() || h.is_decode()) {
            let message = format!("{h}");
            return anyhow::Error::new(e).context(SslError::new("", message));
        }
    }
    anyhow::Error::new(e)
}

#[async_trait]
impl ResponsesClient for eve_openrouter::Client {
    async fn create(&self, request: Value) -> anyhow::Result<Value> {
        let (client, body) = split_timeout(self, request);
        let reply = client.responses_create(body).await.map_err(map_err)?;
        Ok(eve_spend::sdk_dump(&reply))
    }

    async fn stream(&self, request: Value, on_event: &(dyn for<'v> Fn(&'v Value) + Send + Sync)) -> anyhow::Result<Value> {
        let (client, body) = split_timeout(self, request);
        let events = client.responses_stream(body).await.map_err(map_err)?;
        futures::pin_mut!(events);
        let mut completed: Option<Value> = None;
        while let Some(ev) = events.next().await {
            let ev = ev.map_err(map_err)?;
            let data = ev.data;
            let error = data.get("error").filter(|e| py::truthy(e));
            if ev.event.as_deref() == Some("error") || error.is_some() {
                let message = error
                    .and_then(|e| e.get("message"))
                    .map(py::value_str)
                    .unwrap_or_else(|| "An error occurred during streaming".to_string());
                return Err(PyError::err("APIError", message));
            }
            if data.get("type").and_then(Value::as_str) == Some("response.completed") {
                completed = data.get("response").cloned();
            }
            on_event(&data);
        }
        match completed {
            Some(r) => Ok(eve_spend::sdk_dump(&r)),
            None => Err(runtime("Didn't receive a `response.completed` event.")),
        }
    }
}

/// The Responses client: ``client`` (tests), else the client on OpenRouter with OPENROUTER_API_KEY (eve-openrouter;
/// owner, 2026-10-08: one key for every model). ``api_key`` overrides the OpenRouter key.
pub fn openai_client(
    api_key: Option<&str>,
    client: Option<Arc<dyn ResponsesClient>>,
) -> anyhow::Result<Arc<dyn ResponsesClient>> {
    if let Some(c) = client {
        return Ok(c);
    }
    let c = eve_openrouter::async_client(api_key, eve_openrouter::ClientOptions::default())?;
    Ok(Arc::new(c))
}

/// The concrete OpenRouter client `openai_client` builds, for callers that need its base URL or headers;
/// `env` stands in for the process environment (tests).
pub fn openrouter_client(api_key: Option<&str>, env: Option<HashMap<String, String>>) -> anyhow::Result<eve_openrouter::Client> {
    let mut opts = eve_openrouter::ClientOptions::default();
    if let Some(e) = env {
        opts = opts.env(e);
    }
    Ok(eve_openrouter::async_client(api_key, opts)?)
}

/// A client factory: each call makes a new client (a new pool).
pub type ClientFactory = Arc<dyn Fn() -> anyhow::Result<Arc<dyn ResponsesClient>> + Send + Sync>;

fn key_of(c: &Arc<dyn ResponsesClient>) -> usize {
    Arc::as_ptr(c) as *const () as usize
}

struct SlotState {
    client: Arc<dyn ResponsesClient>,
    busy: HashMap<usize, usize>,
    retired: HashMap<usize, Arc<dyn ResponsesClient>>,
}

/// The client the next call uses, and a way to swap it for a fresh one after a TLS drop.
///
/// A swapped-out client is closed as soon as no call is still using it (a concurrent turn that is mid-request
/// on the old client finishes there first). With a fixed ``client`` (tests) and no factory, a swap hands back
/// the same object, as before.
pub struct ClientSlot {
    factory: ClientFactory,
    state: Mutex<SlotState>,
}

impl ClientSlot {
    pub fn new(factory: ClientFactory) -> anyhow::Result<Self> {
        let client = factory()?;
        Ok(ClientSlot { factory, state: Mutex::new(SlotState { client, busy: HashMap::new(), retired: HashMap::new() }) })
    }

    pub fn lease(&self) -> Arc<dyn ResponsesClient> {
        let mut s = self.state.lock();
        let c = s.client.clone();
        *s.busy.entry(key_of(&c)).or_insert(0) += 1;
        c
    }

    pub async fn release(&self, c: &Arc<dyn ResponsesClient>) {
        let old = {
            let mut s = self.state.lock();
            let k = key_of(c);
            let n = s.busy.get(&k).copied().unwrap_or(1).saturating_sub(1);
            if n > 0 {
                s.busy.insert(k, n);
                return;
            }
            s.busy.remove(&k);
            s.retired.remove(&k)
        };
        if let Some(old) = old {
            close_quietly(old.as_ref()).await;
        }
    }

    /// Called by a call whose request on ``bad`` dropped: the next lease is a new client. A second call that
    /// dropped on the same ``bad`` finds it already replaced and shares the new one.
    pub fn replace(&self, bad: &Arc<dyn ResponsesClient>) -> anyhow::Result<()> {
        {
            let s = self.state.lock();
            if key_of(&s.client) != key_of(bad) {
                return Ok(());
            }
        }
        let fresh = (self.factory)()?;
        if key_of(&fresh) == key_of(bad) {
            return Ok(());
        }
        let mut s = self.state.lock();
        if key_of(&s.client) != key_of(bad) {
            return Ok(());
        }
        s.client = fresh;
        // closed by the release of its last lease
        s.retired.insert(key_of(bad), bad.clone());
        Ok(())
    }
}

async fn close_quietly(client: &dyn ResponsesClient) {
    // a broken pool failing to close is not the caller's problem
    client.close().await;
}

/// The creators' seams (Python's keyword arguments): `client` (one fixed client), `client_factory` (a new client
/// per swap) and `sleep`; `api_key` overrides the OpenRouter key of the default client.
#[derive(Clone, Default)]
pub struct CreatorOptions {
    pub api_key: Option<String>,
    pub client: Option<Arc<dyn ResponsesClient>>,
    pub client_factory: Option<ClientFactory>,
    pub sleep: Option<SleepFn>,
}

fn slot(o: &CreatorOptions) -> anyhow::Result<ClientSlot> {
    if let Some(f) = &o.client_factory {
        return ClientSlot::new(f.clone());
    }
    if let Some(c) = &o.client {
        let c = c.clone();
        return ClientSlot::new(Arc::new(move || Ok(c.clone())));
    }
    let key = o.api_key.clone();
    ClientSlot::new(Arc::new(move || openai_client(key.as_deref(), None)))
}

fn sleeper(o: &CreatorOptions) -> SleepFn {
    o.sleep.clone().unwrap_or_else(|| {
        Arc::new(|s: f64| {
            Box::pin(async move {
                if let Ok(d) = Duration::try_from_secs_f64(s.max(0.0)) {
                    tokio::time::sleep(d).await;
                }
            })
        })
    })
}

/// responses.create as a dict-in/dict-out coroutine (the seam tests fake). A TLS drop is retried after
/// TLS_BACKOFF on a fresh client. That is safe: the request is a model call with no side effect of its own, and
/// a tool call it asks for runs only after its answer has arrived, which a dropped request never did. Fails
/// (NoKey) when no client can be made.
pub fn make_response_creator(options: CreatorOptions) -> anyhow::Result<CreateResponse> {
    let slot = Arc::new(slot(&options)?);
    let sleep = sleeper(&options);
    Ok(Arc::new(move |request: Value| {
        let slot = slot.clone();
        let sleep = sleep.clone();
        Box::pin(async move {
            #[allow(clippy::needless_range_loop)] // Python: for attempt in range(TLS_RETRIES + 1), indexing the backoff only before a retry
            for attempt in 0..=TLS_RETRIES {
                let c = slot.lease();
                let r = c.create(request.clone()).await;
                // the swap comes before the release, so the release of the last lease closes the dropped client
                let swapped = match &r {
                    Err(e) if is_tls_drop(e) && attempt < TLS_RETRIES => Some(slot.replace(&c)),
                    _ => None,
                };
                slot.release(&c).await;
                let e = match r {
                    Ok(v) => return Ok(v),
                    Err(e) => e,
                };
                match swapped {
                    None => return Err(e),
                    Some(Err(replace_failed)) => return Err(replace_failed),
                    Some(Ok(())) => tracing::warn!(
                        "responses: TLS dropped mid-request ({}); retrying in {:.1} s on a new connection",
                        drop_reason(&e),
                        TLS_BACKOFF[attempt]
                    ),
                }
                sleep(TLS_BACKOFF[attempt]).await;
            }
            Err(runtime("unreachable"))
        }) as BoxFuture<'static, anyhow::Result<Value>>
    }))
}

/// Where a streamed reply's text goes, a piece at a time.
pub type TextSink = Arc<dyn Fn(&str) + Send + Sync>;
/// `create(request, on_text) -> response`.
pub type StreamResponse = Arc<dyn Fn(Value, TextSink) -> BoxFuture<'static, anyhow::Result<Value>> + Send + Sync>;

/// responses.create, streamed: ``on_text`` gets each piece of the reply's text as it is written, and the finished
/// response comes back as the same dict ``make_response_creator`` returns (the Telegram call reads a reply out a
/// sentence at a time). A TLS drop is retried as there, but only while nothing has been handed to ``on_text``:
/// once a sentence is being read out, a retry would read it out twice.
pub fn make_stream_creator(options: CreatorOptions) -> anyhow::Result<StreamResponse> {
    let slot = Arc::new(slot(&options)?);
    let sleep = sleeper(&options);
    Ok(Arc::new(move |request: Value, on_text: TextSink| {
        let slot = slot.clone();
        let sleep = sleep.clone();
        Box::pin(async move {
            #[allow(clippy::needless_range_loop)] // Python: for attempt in range(TLS_RETRIES + 1), indexing the backoff only before a retry
            for attempt in 0..=TLS_RETRIES {
                let spoke = Arc::new(AtomicBool::new(false));
                let c = slot.lease();
                let r = {
                    let spoke = spoke.clone();
                    let on_text = on_text.clone();
                    let on_event = move |event: &Value| {
                        if event.get("type").and_then(Value::as_str) == Some("response.output_text.delta") {
                            spoke.store(true, Ordering::SeqCst);
                            on_text(&event.get("delta").map(py::value_str).unwrap_or_default());
                        }
                    };
                    c.stream(request.clone(), &on_event).await
                };
                let retry = matches!(&r, Err(e) if is_tls_drop(e)) && attempt < TLS_RETRIES && !spoke.load(Ordering::SeqCst);
                let swapped = if retry { Some(slot.replace(&c)) } else { None };
                slot.release(&c).await;
                let e = match r {
                    Ok(v) => return Ok(plain_response(v)),
                    Err(e) => e,
                };
                match swapped {
                    None => return Err(e),
                    Some(Err(replace_failed)) => return Err(replace_failed),
                    Some(Ok(())) => tracing::warn!(
                        "responses: TLS dropped mid-stream ({}); retrying in {:.1} s on a new connection",
                        drop_reason(&e),
                        TLS_BACKOFF[attempt]
                    ),
                }
                sleep(TLS_BACKOFF[attempt]).await;
            }
            Err(runtime("unreachable"))
        }) as BoxFuture<'static, anyhow::Result<Value>>
    }))
}

/// A streamed response as ``responses.create`` returns it. The SDK's stream helper adds what it parsed
/// (``parsed_arguments`` on a strict tool's call, ``parsed`` on text), and the API refuses those fields when the
/// call is sent back as input in the next round: live, 2026-09-25, a rundown on a phone call failed with
/// "Unknown parameter: 'input[2].parsed_arguments'" (the Composio tools are strict).
pub fn plain_response(mut response: Value) -> Value {
    if let Some(Value::Array(items)) = response.get_mut("output") {
        for item in items.iter_mut() {
            if let Value::Object(m) = item {
                m.shift_remove("parsed_arguments");
                if let Some(Value::Array(parts)) = m.get_mut("content") {
                    for part in parts.iter_mut() {
                        if let Value::Object(p) = part {
                            p.shift_remove("parsed");
                        }
                    }
                }
            }
        }
    }
    response
}
