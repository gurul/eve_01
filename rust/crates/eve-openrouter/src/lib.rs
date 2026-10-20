//! One key for every model eve calls: the owner's OpenRouter key.
//!
//! Owner, 2026-10-08: "make everything use my openrouter key, remove open ai and claude key". Before this, the chat,
//! the planner, the camera, memory, notes and the calls each built an OpenAI client on `OPENAI_API_KEY`, and the app
//! builder an Anthropic client on `ANTHROPIC_API_KEY`. Now every one of them gets its client here, and every client
//! points at OpenRouter with `OPENROUTER_API_KEY`.
//!
//! * [`client()`] / [`async_client()`] are OpenAI-shaped HTTP clients with OpenRouter's base URL, the key and eve's
//!   attribution headers. They rewrite the request's `model` as it is sent ([`model()`]), so the call sites keep
//!   their model names as they were ("gpt-6-luna") and OpenRouter gets "openai/gpt-6-luna". Responses, streaming,
//!   Chat Completions, embeddings, transcription and speech all go through the same request builder, so one rewrite
//!   covers them all (verified live 2026-10-08: function tools, encrypted reasoning, json_schema strict, images, PDFs
//!   and streaming work on OpenRouter's Responses API).
//! * [`anthropic_client()`] is an Anthropic-shaped client on OpenRouter's Anthropic endpoint
//!   (`https://openrouter.ai/api`), the same key, with the same rewrite ("claude-opus-5-5" →
//!   "anthropic/claude-opus-5.5").
//! * What OpenRouter does not have: `previous_response_id` (refused: "not supported on this proxy"), so a chain
//!   resends its history with `store: False` and the encrypted reasoning; OpenAI's realtime WebSockets (the desk voice
//!   and the call's live ears, both left on the OpenAI key as opt-ins); OpenAI's Decisions beta and the diarizing
//!   transcriber. docs/openrouter.md has the whole list.
//!
//! Spend (eve-spend) is recorded at each call site as before, under the provider `openrouter`; the model name in the
//! row is the call site's own, so the pricing tables still read it.
//!
//! The Python module hands out the openai / anthropic SDK clients. Rust has no such SDK, so this crate is that
//! client: the same base URLs, auth headers, default headers, timeouts, retry policy and model rewrite, over
//! `reqwest`. Bodies are free-form `serde_json::Value`s, exactly what the call sites pass to the SDK.

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;

use futures::{Stream, StreamExt};
use regex::Regex;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{json, Map, Value};

pub const KEY_ENV: &str = "OPENROUTER_API_KEY";
pub const BASE_URL: &str = "https://openrouter.ai/api/v1";
/// The Anthropic client adds /v1/messages itself.
pub const ANTHROPIC_BASE_URL: &str = "https://openrouter.ai/api";
/// The spend ledger's provider name for these calls.
pub const PROVIDER: &str = "openrouter";
/// eve's attribution headers, sent on every request (a caller's `default_headers` win on a clash).
pub const HEADERS: [(&str, &str); 2] = [("HTTP-Referer", "https://github.com/gurul/eve_01"), ("X-Title", "eve")];

/// The anthropic SDK's API version header, which the Anthropic-shaped endpoint expects.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
/// The SDKs' defaults: 10 minutes per request, 2 retries.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);
pub const DEFAULT_MAX_RETRIES: u32 = 2;

// Bare OpenAI model names: gpt-*, chatgpt-*, the o-series (o3, o4-mini), embeddings, transcribers, tts, whisper.
static OPENAI: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:gpt-|chatgpt-|o\d|text-embedding-|whisper|tts-|computer-use-)").expect("valid regex")
});
// Anthropic's API ids write the version with dashes ("claude-opus-5-5"); OpenRouter's with a dot ("claude-opus-5.5").
// Python's `\d` matches any Unicode digit and `$` also matches before a final newline; the stripped input has none.
static CLAUDE_VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(claude-[a-z]+)-(\d+)-(\d+)$").expect("valid regex"));

/// OPENROUTER_API_KEY is not set: no model call can be made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{msg}")]
pub struct NoKey {
    pub msg: String,
}

impl Default for NoKey {
    fn default() -> Self {
        NoKey { msg: format!("{KEY_ENV} is not set") }
    }
}

/// Everything a call through these clients can fail with. `NoKey` is raised before any request is built; `Status`
/// is the SDKs' APIStatusError (a non-2xx reply after the retries); `Http` their APIConnectionError/APITimeoutError.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    NoKey(#[from] NoKey),
    #[error("Error code: {status} - {body}")]
    Status { status: u16, body: String, headers: Box<HeaderMap> },
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(String),
}

impl Error {
    /// The HTTP status of a `Status` error.
    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Status { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// The reply body of a `Status` error parsed as JSON, when it is JSON.
    pub fn body_json(&self) -> Option<Value> {
        match self {
            Error::Status { body, .. } => serde_json::from_str(body).ok(),
            _ => None,
        }
    }

    /// A timeout (the SDKs' APITimeoutError).
    pub fn is_timeout(&self) -> bool {
        matches!(self, Error::Http(e) if e.is_timeout())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Which upstream host OpenRouter should try first, per model family (fallbacks stay allowed). 2026-10-08:
/// OpenRouter mixed google/gemini-3.1-flash-lite across Google AI Studio (0.72 s, 100% up) and Vertex "global" (0% up
/// for the last 5 minutes, 72.7% over the day, one 504), and every web search waited on the sick one.
pub const PROVIDER_ORDER: &[(&str, &[&str])] = &[("google/", &["google-ai-studio"])];

/// OpenRouter's `provider` field for a chat completion on `model_id`: the preferred hosts first, fallbacks on;
/// None when there is no preference.
pub fn provider_prefs(model_id: &str) -> Option<Value> {
    for (prefix, order) in PROVIDER_ORDER {
        if model_id.starts_with(prefix) {
            return Some(json!({"order": order, "allow_fallbacks": true}));
        }
    }
    None
}

/// The OpenRouter key, stripped, or "". `env` is a mapping to read instead of the process environment.
pub fn key(env: Option<&HashMap<String, String>>) -> String {
    let raw = match env {
        Some(e) => e.get(KEY_ENV).cloned(),
        None => std::env::var(KEY_ENV).ok(),
    };
    py_strip(raw.as_deref().unwrap_or("")).to_string()
}

/// Whether eve can call a model at all: OPENROUTER_API_KEY is set.
pub fn available(env: Option<&HashMap<String, String>>) -> bool {
    !key(env).is_empty()
}

/// Python's `str.strip()` with no argument: Unicode whitespace on both ends.
fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace())
}

/// The OpenRouter id for a model name. An id that already names its provider ("openai/…", "deepgram/aura-2") is left
/// as it is; a bare OpenAI name gets "openai/"; a Claude name gets "anthropic/" and a dotted version.
pub fn model(name: &str) -> String {
    let n = py_strip(name);
    if n.is_empty() || n.contains('/') {
        return n.to_string();
    }
    if OPENAI.is_match(n) {
        return format!("openai/{n}");
    }
    if n.to_lowercase().starts_with("claude-") {
        return match CLAUDE_VERSION.captures(n) {
            Some(m) => format!("anthropic/{}-{}.{}", &m[1], &m[2], &m[3]),
            None => format!("anthropic/{n}"),
        };
    }
    n.to_string()
}

/// Put the OpenRouter id in a request's body, in place. Idempotent: a retry re-runs it on the same body. Returns
/// whether the body changed.
pub fn rewrite(body: &mut Value) -> bool {
    if let Some(Value::String(m)) = body.get("model") {
        let mapped = model(m);
        if &mapped != m {
            body["model"] = Value::String(mapped);
            return true;
        }
    }
    false
}

/// The key a client is built with: the argument when it is non-blank, else the environment's. Raises NoKey.
pub fn key_or_raise(api_key: Option<&str>, env: Option<&HashMap<String, String>>) -> std::result::Result<String, NoKey> {
    let given = py_strip(api_key.unwrap_or(""));
    let k = if given.is_empty() { key(env) } else { given.to_string() };
    if k.is_empty() {
        return Err(NoKey::default());
    }
    Ok(k)
}

/// What the Python call sites pass as keyword arguments to `client()` and friends.
#[derive(Debug, Clone, Default)]
pub struct ClientOptions {
    /// Per-request timeout (the SDKs' `timeout`, default 600 s).
    pub timeout: Option<Duration>,
    /// Retries on a connection error, 408, 409, 429 or 5xx (the SDKs' `max_retries`, default 2).
    pub max_retries: Option<u32>,
    /// Merged over [`HEADERS`]; these win on a clash.
    pub default_headers: Vec<(String, String)>,
    /// The HTTP client to send with (the SDKs' `http_client`), e.g. one with a proxy. Async clients only.
    pub http_client: Option<reqwest::Client>,
    /// Where requests go instead of the OpenRouter base URL. Only tests set this, to point at a local server;
    /// it plays the role of the Python tests' mock transport.
    pub base_url: Option<String>,
    /// The environment mapping to read the key from instead of the process environment.
    pub env: Option<HashMap<String, String>>,
}

impl ClientOptions {
    pub fn timeout(mut self, t: Duration) -> Self {
        self.timeout = Some(t);
        self
    }
    pub fn max_retries(mut self, n: u32) -> Self {
        self.max_retries = Some(n);
        self
    }
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.default_headers.push((name.into(), value.into()));
        self
    }
    pub fn http_client(mut self, c: reqwest::Client) -> Self {
        self.http_client = Some(c);
        self
    }
    pub fn base_url(mut self, u: impl Into<String>) -> Self {
        self.base_url = Some(u.into());
        self
    }
    pub fn env(mut self, e: HashMap<String, String>) -> Self {
        self.env = Some(e);
        self
    }
}

/// eve's headers with the caller's merged over them (`{**HEADERS, **default_headers}`).
pub fn merge_headers(extra: &[(String, String)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = HEADERS.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    for (k, v) in extra {
        if let Some(slot) = out.iter_mut().find(|(ok, _)| ok == k) {
            slot.1 = v.clone();
        } else {
            out.push((k.clone(), v.clone()));
        }
    }
    out
}

/// Which wire shape a client speaks: the OpenAI API (Bearer auth) or the Anthropic API (x-api-key + version).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flavor {
    OpenAi,
    Anthropic,
}

/// One part of a multipart form (transcriptions, file uploads). Kept as data, not a `reqwest` form, so a retry can
/// rebuild it and the `model` field can be rewritten like a JSON body's.
#[derive(Debug, Clone)]
pub enum Part {
    Text { name: String, value: String },
    File { name: String, filename: String, bytes: Vec<u8>, content_type: String },
}

impl Part {
    pub fn text(name: impl Into<String>, value: impl Into<String>) -> Self {
        Part::Text { name: name.into(), value: value.into() }
    }
    pub fn file(name: impl Into<String>, filename: impl Into<String>, bytes: Vec<u8>, content_type: impl Into<String>) -> Self {
        Part::File { name: name.into(), filename: filename.into(), bytes, content_type: content_type.into() }
    }
}

/// Rewrite the `model` field of a multipart form, in place, as [`rewrite`] does for JSON.
pub fn rewrite_parts(parts: &mut [Part]) -> bool {
    let mut changed = false;
    for p in parts.iter_mut() {
        if let Part::Text { name, value } = p {
            if name == "model" {
                let mapped = model(value);
                if &mapped != value {
                    *value = mapped;
                    changed = true;
                }
            }
        }
    }
    changed
}

/// The multipart form for an OpenAI-style request built from a JSON-like map of fields plus one file, the way the
/// SDK flattens `transcriptions.create(model=…, file=…, language=…)`: strings as-is, numbers and booleans as their
/// JSON text, arrays as repeated `name[]` fields, null skipped.
pub fn form_parts(fields: &Map<String, Value>, file: Option<Part>) -> Vec<Part> {
    let mut parts = Vec::new();
    for (k, v) in fields {
        push_field(&mut parts, k, v);
    }
    if let Some(f) = file {
        parts.push(f);
    }
    parts
}

fn push_field(parts: &mut Vec<Part>, k: &str, v: &Value) {
    match v {
        Value::Null => {}
        Value::String(s) => parts.push(Part::text(k, s.clone())),
        Value::Bool(b) => parts.push(Part::text(k, if *b { "true" } else { "false" })),
        Value::Number(n) => parts.push(Part::text(k, n.to_string())),
        Value::Array(items) => {
            for item in items {
                push_field(parts, &format!("{k}[]"), item);
            }
        }
        Value::Object(m) => {
            for (sk, sv) in m {
                push_field(parts, &format!("{k}[{sk}]"), sv);
            }
        }
    }
}

#[derive(Debug, Clone)]
enum Body {
    Empty,
    Json(Value),
    Multipart(Vec<Part>),
}

/// What both the async and the blocking client share: where to send, which headers, how to retry.
#[derive(Debug, Clone)]
struct Core {
    flavor: Flavor,
    api_key: String,
    base_url: String,
    headers: HeaderMap,
    timeout: Duration,
    max_retries: u32,
}

impl Core {
    fn new(flavor: Flavor, api_key: Option<&str>, default_base: &str, opts: &ClientOptions) -> Result<Core> {
        let k = key_or_raise(api_key, opts.env.as_ref())?;
        let mut headers = HeaderMap::new();
        headers.insert(reqwest::header::ACCEPT, HeaderValue::from_static("application/json"));
        match flavor {
            Flavor::OpenAi => {
                let v = HeaderValue::from_str(&format!("Bearer {k}"))
                    .map_err(|_| Error::Invalid("the API key is not a valid header value".into()))?;
                headers.insert(reqwest::header::AUTHORIZATION, v);
            }
            Flavor::Anthropic => {
                let v = HeaderValue::from_str(&k)
                    .map_err(|_| Error::Invalid("the API key is not a valid header value".into()))?;
                headers.insert(HeaderName::from_static("x-api-key"), v);
                headers.insert(HeaderName::from_static("anthropic-version"), HeaderValue::from_static(ANTHROPIC_VERSION));
            }
        }
        for (name, value) in merge_headers(&opts.default_headers) {
            let n = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| Error::Invalid(format!("invalid header name {name:?}")))?;
            let v = HeaderValue::from_str(&value).map_err(|_| Error::Invalid(format!("invalid header value for {name}")))?;
            headers.insert(n, v);
        }
        Ok(Core {
            flavor,
            api_key: k,
            base_url: opts.base_url.clone().unwrap_or_else(|| default_base.to_string()).trim_end_matches('/').to_string(),
            headers,
            timeout: opts.timeout.unwrap_or(DEFAULT_TIMEOUT),
            max_retries: opts.max_retries.unwrap_or(DEFAULT_MAX_RETRIES),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }

    /// The body as it is sent: the model rewritten. Run on every attempt, so a retry sees the same body.
    fn prepare(&self, body: &mut Body) {
        match body {
            Body::Json(v) => {
                rewrite(v);
            }
            Body::Multipart(parts) => {
                rewrite_parts(parts);
            }
            Body::Empty => {}
        }
    }
}

/// Whether the SDKs retry this reply: `x-should-retry` decides when present, else 408, 409, 429 and 5xx.
fn should_retry(status: u16, headers: &HeaderMap) -> bool {
    match headers.get("x-should-retry").and_then(|v| v.to_str().ok()) {
        Some("true") => return true,
        Some("false") => return false,
        _ => {}
    }
    matches!(status, 408 | 409 | 429) || status >= 500
}

/// How long to wait before retry number `attempt` (0-based), as the SDKs compute it: the server's `retry-after-ms`
/// or `retry-after` when it is between 0 and 60 s, else 0.5 s doubling up to 8 s, less up to 25% jitter.
fn retry_delay(attempt: u32, headers: Option<&HeaderMap>) -> Duration {
    if let Some(h) = headers {
        let ms = h.get("retry-after-ms").and_then(|v| v.to_str().ok()).and_then(|s| s.trim().parse::<f64>().ok());
        let secs = ms.map(|m| m / 1000.0).or_else(|| {
            let raw = h.get("retry-after").and_then(|v| v.to_str().ok())?;
            raw.trim().parse::<f64>().ok().or_else(|| {
                let when = chrono::DateTime::parse_from_rfc2822(raw.trim()).ok()?;
                Some((when.timestamp_millis() - chrono::Utc::now().timestamp_millis()) as f64 / 1000.0)
            })
        });
        if let Some(s) = secs {
            if s > 0.0 && s <= 60.0 {
                return Duration::from_secs_f64(s);
            }
        }
    }
    let base = (0.5 * 2f64.powi(attempt as i32)).min(8.0);
    let jitter = 1.0 - 0.25 * rand::random::<f64>();
    Duration::from_secs_f64(base * jitter)
}

fn reqwest_form(parts: Vec<Part>) -> Result<reqwest::multipart::Form> {
    let mut form = reqwest::multipart::Form::new();
    for p in parts {
        form = match p {
            Part::Text { name, value } => form.text(name, value),
            Part::File { name, filename, bytes, content_type } => form.part(
                name,
                reqwest::multipart::Part::bytes(bytes)
                    .file_name(filename)
                    .mime_str(&content_type)
                    .map_err(|e| Error::Invalid(format!("bad content type {content_type:?}: {e}")))?,
            ),
        };
    }
    Ok(form)
}

fn blocking_form(parts: Vec<Part>) -> Result<reqwest::blocking::multipart::Form> {
    let mut form = reqwest::blocking::multipart::Form::new();
    for p in parts {
        form = match p {
            Part::Text { name, value } => form.text(name, value),
            Part::File { name, filename, bytes, content_type } => form.part(
                name,
                reqwest::blocking::multipart::Part::bytes(bytes)
                    .file_name(filename)
                    .mime_str(&content_type)
                    .map_err(|e| Error::Invalid(format!("bad content type {content_type:?}: {e}")))?,
            ),
        };
    }
    Ok(form)
}

/// A reply body as JSON; an empty body is `null`, a non-JSON body its text as a JSON string (the SDKs return the
/// text for `response_format="text"` transcriptions).
fn parse_body(bytes: &[u8]) -> Value {
    if bytes.iter().all(|b| b.is_ascii_whitespace()) {
        return Value::Null;
    }
    serde_json::from_slice(bytes).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(bytes).into_owned()))
}

// ---- the async client -------------------------------------------------------------------------------------------

/// An async OpenAI- or Anthropic-shaped client on OpenRouter. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Client {
    core: Core,
    http: reqwest::Client,
}

impl Client {
    fn build(flavor: Flavor, api_key: Option<&str>, default_base: &str, opts: ClientOptions) -> Result<Client> {
        let core = Core::new(flavor, api_key, default_base, &opts)?;
        let http = match opts.http_client {
            Some(c) => c,
            None => reqwest::Client::builder().build()?,
        };
        Ok(Client { core, http })
    }

    /// The base URL every path is joined to.
    pub fn base_url(&self) -> &str {
        &self.core.base_url
    }

    /// The key this client sends.
    pub fn api_key(&self) -> &str {
        &self.core.api_key
    }

    /// The headers sent on every request (auth, version, eve's attribution).
    pub fn default_headers(&self) -> &HeaderMap {
        &self.core.headers
    }

    pub fn timeout(&self) -> Duration {
        self.core.timeout
    }

    pub fn max_retries(&self) -> u32 {
        self.core.max_retries
    }

    /// A copy with other per-request options (the SDKs' `with_options`).
    pub fn with_options(&self, timeout: Option<Duration>, max_retries: Option<u32>) -> Client {
        let mut c = self.clone();
        if let Some(t) = timeout {
            c.core.timeout = t;
        }
        if let Some(n) = max_retries {
            c.core.max_retries = n;
        }
        c
    }

    /// Send with the retry policy; returns the 2xx reply, or `Status` with the last non-2xx reply.
    async fn send(&self, method: reqwest::Method, path: &str, mut body: Body) -> Result<reqwest::Response> {
        self.core.prepare(&mut body);
        let url = self.core.url(path);
        let mut attempt = 0u32;
        loop {
            let mut req = self
                .http
                .request(method.clone(), &url)
                .headers(self.core.headers.clone())
                .timeout(self.core.timeout);
            req = match &body {
                Body::Empty => req,
                Body::Json(v) => req.header(reqwest::header::CONTENT_TYPE, "application/json").body(serde_json::to_vec(v)?),
                Body::Multipart(parts) => req.multipart(reqwest_form(parts.clone())?),
            };
            let retries_left = self.core.max_retries.saturating_sub(attempt);
            match req.send().await {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    if resp.status().is_success() {
                        return Ok(resp);
                    }
                    let headers = resp.headers().clone();
                    if retries_left > 0 && should_retry(status, &headers) {
                        let wait = retry_delay(attempt, Some(&headers));
                        tracing::info!("Retrying request to {} in {} seconds", path, wait.as_secs_f64());
                        tokio::time::sleep(wait).await;
                        attempt += 1;
                        continue;
                    }
                    let text = resp.text().await.unwrap_or_default();
                    return Err(Error::Status { status, body: text, headers: Box::new(headers) });
                }
                Err(e) => {
                    if retries_left > 0 && (e.is_connect() || e.is_timeout() || e.is_request()) {
                        tokio::time::sleep(retry_delay(attempt, None)).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(Error::Http(e));
                }
            }
        }
    }

    /// POST a JSON body to `path` (relative to the base URL) and return the JSON reply.
    pub async fn post(&self, path: &str, body: Value) -> Result<Value> {
        let resp = self.send(reqwest::Method::POST, path, Body::Json(body)).await?;
        Ok(parse_body(&resp.bytes().await?))
    }

    /// POST a JSON body and return the reply's raw bytes (speech audio).
    pub async fn post_bytes(&self, path: &str, body: Value) -> Result<Vec<u8>> {
        let resp = self.send(reqwest::Method::POST, path, Body::Json(body)).await?;
        Ok(resp.bytes().await?.to_vec())
    }

    /// POST a multipart form and return the JSON reply.
    pub async fn post_multipart(&self, path: &str, parts: Vec<Part>) -> Result<Value> {
        let resp = self.send(reqwest::Method::POST, path, Body::Multipart(parts)).await?;
        Ok(parse_body(&resp.bytes().await?))
    }

    /// GET `path` and return the JSON reply.
    pub async fn get(&self, path: &str) -> Result<Value> {
        let resp = self.send(reqwest::Method::GET, path, Body::Empty).await?;
        Ok(parse_body(&resp.bytes().await?))
    }

    /// POST a JSON body and return the open reply, for the caller to read as it arrives.
    pub async fn post_raw(&self, path: &str, body: Value) -> Result<reqwest::Response> {
        self.send(reqwest::Method::POST, path, Body::Json(body)).await
    }

    /// POST a JSON body with `"stream": true` and yield each server-sent event's JSON data.
    pub async fn post_stream(&self, path: &str, mut body: Value) -> Result<impl Stream<Item = Result<SseEvent>>> {
        if let Value::Object(m) = &mut body {
            m.insert("stream".into(), Value::Bool(true));
        }
        let resp = self.send(reqwest::Method::POST, path, Body::Json(body)).await?;
        Ok(sse_events(resp))
    }

    // ---- the OpenAI resources the call sites use -------------------------------------------------------------

    /// `responses.create`.
    pub async fn responses_create(&self, body: Value) -> Result<Value> {
        self.post("/responses", body).await
    }

    /// `responses.create(stream=True)`.
    pub async fn responses_stream(&self, body: Value) -> Result<impl Stream<Item = Result<SseEvent>>> {
        self.post_stream("/responses", body).await
    }

    /// `chat.completions.create`.
    pub async fn chat_completions_create(&self, body: Value) -> Result<Value> {
        self.post("/chat/completions", body).await
    }

    /// `chat.completions.create(stream=True)`.
    pub async fn chat_completions_stream(&self, body: Value) -> Result<impl Stream<Item = Result<SseEvent>>> {
        self.post_stream("/chat/completions", body).await
    }

    /// `embeddings.create`.
    pub async fn embeddings_create(&self, body: Value) -> Result<Value> {
        self.post("/embeddings", body).await
    }

    /// `audio.transcriptions.create`: `fields` are the keyword arguments (model, language, prompt, …).
    pub async fn audio_transcriptions_create(&self, fields: Map<String, Value>, file: Part) -> Result<Value> {
        self.post_multipart("/audio/transcriptions", form_parts(&fields, Some(file))).await
    }

    /// `audio.speech.create`: the audio bytes.
    pub async fn audio_speech_create(&self, body: Value) -> Result<Vec<u8>> {
        self.post_bytes("/audio/speech", body).await
    }

    /// `messages.create` on the Anthropic-shaped client.
    pub async fn messages_create(&self, body: Value) -> Result<Value> {
        self.post("/v1/messages", body).await
    }

    /// `messages.stream` on the Anthropic-shaped client.
    pub async fn messages_stream(&self, body: Value) -> Result<impl Stream<Item = Result<SseEvent>>> {
        self.post_stream("/v1/messages", body).await
    }

    /// Whether this is the Anthropic-shaped client.
    pub fn is_anthropic(&self) -> bool {
        self.core.flavor == Flavor::Anthropic
    }
}

/// One server-sent event: its `event:` name (if any) and its `data:` parsed as JSON (a string when it is not JSON).
#[derive(Debug, Clone, PartialEq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: Value,
}

/// Parse a streaming reply into its events, ending at `data: [DONE]` or the end of the body.
pub fn sse_events(resp: reqwest::Response) -> impl Stream<Item = Result<SseEvent>> {
    let bytes = resp.bytes_stream();
    futures::stream::unfold(
        (bytes, Vec::<u8>::new(), SseState::default(), false),
        |(mut bytes, mut buf, mut state, done)| async move {
            if done {
                return None;
            }
            loop {
                if let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = buf.drain(..=pos).collect();
                    let line = String::from_utf8_lossy(&line);
                    let line = line.trim_end_matches(['\n', '\r']);
                    match state.feed(line) {
                        Feed::Event(ev) => return Some((Ok(ev), (bytes, buf, state, false))),
                        Feed::Done => return None,
                        Feed::More => continue,
                    }
                }
                match bytes.next().await {
                    Some(Ok(chunk)) => buf.extend_from_slice(&chunk),
                    Some(Err(e)) => return Some((Err(Error::Http(e)), (bytes, buf, state, true))),
                    None => {
                        // the last event may lack its blank line
                        if !buf.is_empty() {
                            let line = String::from_utf8_lossy(&buf).into_owned();
                            buf.clear();
                            if let Feed::Event(ev) = state.feed(line.trim_end_matches('\r')) {
                                return Some((Ok(ev), (bytes, buf, state, true)));
                            }
                        }
                        return state.flush().map(|ev| (Ok(ev), (bytes, buf, state, true)));
                    }
                }
            }
        },
    )
}

#[derive(Debug, Default)]
struct SseState {
    event: Option<String>,
    data: Vec<String>,
}

enum Feed {
    Event(SseEvent),
    Done,
    More,
}

impl SseState {
    fn feed(&mut self, line: &str) -> Feed {
        if line.is_empty() {
            return match self.flush() {
                Some(ev) if ev.data == Value::String("[DONE]".into()) => Feed::Done,
                Some(ev) => Feed::Event(ev),
                None => Feed::More,
            };
        }
        if line.starts_with(':') {
            return Feed::More;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => self.data.push(value.to_string()),
            _ => {}
        }
        Feed::More
    }

    fn flush(&mut self) -> Option<SseEvent> {
        if self.data.is_empty() {
            self.event = None;
            return None;
        }
        let raw = self.data.join("\n");
        self.data.clear();
        let data = serde_json::from_str(&raw).unwrap_or(Value::String(raw));
        Some(SseEvent { event: self.event.take(), data })
    }
}

// ---- the blocking client ----------------------------------------------------------------------------------------

/// A blocking OpenAI-shaped client on OpenRouter (the SDK's `OpenAI`). Do not use it on an async runtime thread.
#[derive(Debug, Clone)]
pub struct BlockingClient {
    core: Core,
    http: reqwest::blocking::Client,
}

impl BlockingClient {
    pub fn base_url(&self) -> &str {
        &self.core.base_url
    }

    pub fn api_key(&self) -> &str {
        &self.core.api_key
    }

    pub fn default_headers(&self) -> &HeaderMap {
        &self.core.headers
    }

    pub fn timeout(&self) -> Duration {
        self.core.timeout
    }

    pub fn max_retries(&self) -> u32 {
        self.core.max_retries
    }

    fn send(&self, method: reqwest::Method, path: &str, mut body: Body) -> Result<reqwest::blocking::Response> {
        self.core.prepare(&mut body);
        let url = self.core.url(path);
        let mut attempt = 0u32;
        loop {
            let mut req = self
                .http
                .request(method.clone(), &url)
                .headers(self.core.headers.clone())
                .timeout(self.core.timeout);
            req = match &body {
                Body::Empty => req,
                Body::Json(v) => req.header(reqwest::header::CONTENT_TYPE, "application/json").body(serde_json::to_vec(v)?),
                Body::Multipart(parts) => req.multipart(blocking_form(parts.clone())?),
            };
            let retries_left = self.core.max_retries.saturating_sub(attempt);
            match req.send() {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    if resp.status().is_success() {
                        return Ok(resp);
                    }
                    let headers = resp.headers().clone();
                    if retries_left > 0 && should_retry(status, &headers) {
                        std::thread::sleep(retry_delay(attempt, Some(&headers)));
                        attempt += 1;
                        continue;
                    }
                    let text = resp.text().unwrap_or_default();
                    return Err(Error::Status { status, body: text, headers: Box::new(headers) });
                }
                Err(e) => {
                    if retries_left > 0 && (e.is_connect() || e.is_timeout() || e.is_request()) {
                        std::thread::sleep(retry_delay(attempt, None));
                        attempt += 1;
                        continue;
                    }
                    return Err(Error::Http(e));
                }
            }
        }
    }

    pub fn post(&self, path: &str, body: Value) -> Result<Value> {
        let resp = self.send(reqwest::Method::POST, path, Body::Json(body))?;
        Ok(parse_body(&resp.bytes()?))
    }

    pub fn post_bytes(&self, path: &str, body: Value) -> Result<Vec<u8>> {
        let resp = self.send(reqwest::Method::POST, path, Body::Json(body))?;
        Ok(resp.bytes()?.to_vec())
    }

    pub fn post_multipart(&self, path: &str, parts: Vec<Part>) -> Result<Value> {
        let resp = self.send(reqwest::Method::POST, path, Body::Multipart(parts))?;
        Ok(parse_body(&resp.bytes()?))
    }

    pub fn get(&self, path: &str) -> Result<Value> {
        let resp = self.send(reqwest::Method::GET, path, Body::Empty)?;
        Ok(parse_body(&resp.bytes()?))
    }

    pub fn responses_create(&self, body: Value) -> Result<Value> {
        self.post("/responses", body)
    }

    pub fn chat_completions_create(&self, body: Value) -> Result<Value> {
        self.post("/chat/completions", body)
    }

    pub fn embeddings_create(&self, body: Value) -> Result<Value> {
        self.post("/embeddings", body)
    }

    pub fn audio_transcriptions_create(&self, fields: Map<String, Value>, file: Part) -> Result<Value> {
        self.post_multipart("/audio/transcriptions", form_parts(&fields, Some(file)))
    }

    pub fn audio_speech_create(&self, body: Value) -> Result<Vec<u8>> {
        self.post_bytes("/audio/speech", body)
    }
}

// ---- the constructors -------------------------------------------------------------------------------------------

/// A blocking OpenAI-shaped client on OpenRouter (`timeout`, `max_retries` pass through). Raises NoKey.
pub fn client(api_key: Option<&str>, opts: ClientOptions) -> Result<BlockingClient> {
    let core = Core::new(Flavor::OpenAi, api_key, BASE_URL, &opts)?;
    let http = reqwest::blocking::Client::builder().build()?;
    Ok(BlockingClient { core, http })
}

/// An async OpenAI-shaped client on OpenRouter. Raises NoKey.
pub fn async_client(api_key: Option<&str>, opts: ClientOptions) -> Result<Client> {
    Client::build(Flavor::OpenAi, api_key, BASE_URL, opts)
}

/// An async Anthropic-shaped client on OpenRouter's Anthropic endpoint. Raises NoKey.
pub fn anthropic_client(api_key: Option<&str>, opts: ClientOptions) -> Result<Client> {
    Client::build(Flavor::Anthropic, api_key, ANTHROPIC_BASE_URL, opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_policy_matches_the_sdks() {
        let h = HeaderMap::new();
        for s in [408, 409, 429, 500, 502, 503, 504] {
            assert!(should_retry(s, &h), "{s}");
        }
        for s in [400, 401, 403, 404, 422] {
            assert!(!should_retry(s, &h), "{s}");
        }
        let mut no = HeaderMap::new();
        no.insert("x-should-retry", HeaderValue::from_static("false"));
        assert!(!should_retry(503, &no));
        let mut yes = HeaderMap::new();
        yes.insert("x-should-retry", HeaderValue::from_static("true"));
        assert!(should_retry(400, &yes));
    }

    #[test]
    fn retry_delay_honours_retry_after_and_caps_backoff() {
        let mut h = HeaderMap::new();
        h.insert("retry-after", HeaderValue::from_static("2"));
        assert_eq!(retry_delay(0, Some(&h)), Duration::from_secs(2));
        let mut ms = HeaderMap::new();
        ms.insert("retry-after-ms", HeaderValue::from_static("250"));
        assert_eq!(retry_delay(3, Some(&ms)), Duration::from_millis(250));
        let d = retry_delay(10, None);
        assert!(d <= Duration::from_secs(8) && d >= Duration::from_secs(6));
        let d0 = retry_delay(0, None);
        assert!(d0 <= Duration::from_millis(500) && d0 >= Duration::from_millis(375));
    }

    #[test]
    fn sse_state_parses_events_and_done() {
        let mut s = SseState::default();
        assert!(matches!(s.feed("event: response.output_text.delta"), Feed::More));
        assert!(matches!(s.feed("data: {\"delta\":\"hi\"}"), Feed::More));
        match s.feed("") {
            Feed::Event(ev) => {
                assert_eq!(ev.event.as_deref(), Some("response.output_text.delta"));
                assert_eq!(ev.data, json!({"delta": "hi"}));
            }
            _ => panic!("expected an event"),
        }
        assert!(matches!(s.feed(": keepalive"), Feed::More));
        s.feed("data: [DONE]");
        assert!(matches!(s.feed(""), Feed::Done));
    }

    #[test]
    fn form_parts_flatten_like_the_sdk() {
        let fields = json!({"model": "gpt-4o-transcribe", "language": "en", "temperature": 0, "x": null,
                            "timestamp_granularities": ["word"]});
        let parts = form_parts(fields.as_object().unwrap(), None);
        let names: Vec<_> = parts
            .iter()
            .map(|p| match p {
                Part::Text { name, value } => format!("{name}={value}"),
                Part::File { name, .. } => name.clone(),
            })
            .collect();
        assert_eq!(names, ["model=gpt-4o-transcribe", "language=en", "temperature=0", "timestamp_granularities[]=word"]);
    }
}
