//! Composio over its HTTP API: the calls the Python bridge makes through the `composio` 0.22 SDK (with
//! `composio_client` 1.43 under it), and the trait seam the bridge and its tests share.
//!
//! What the SDK did, kept here:
//!
//! * `Composio(provider=OpenAIResponsesProvider(strict=True), timeout=…)`: the key from `COMPOSIO_API_KEY` (an
//!   `ApiKeyNotProvidedError` without it), the base URL from `COMPOSIO_BASE_URL` or production, `x-api-key` and the
//!   SDK's request headers, two retries with Stainless's backoff on 408, 409, 429, 5xx and network errors.
//! * `sessions.create` / `sessions.use`: `POST` / `GET /api/v3.1/tool_router/session[/{id}]`, connection
//!   management on (the SDK's default), the toolkit list and multi-account config as eve passes them.
//! * `session.tools()`: every page of `GET …/session/{id}/tools` (500 a page), each tool's parameter descriptions
//!   enhanced and wrapped as a Responses-API function tool under strict mode (`strict_schema`).
//! * `session.execute`: `POST …/session/{id}/execute` with `tool_slug` and `arguments`.
//! * `connected_accounts.list(user_ids=…)`: `GET /api/v3.1/connected_accounts`, one page, as the SDK does.
//! * `tools.execute(…, dangerously_skip_version_check=True)`: first the tool's schema (`GET /api/v3.1/tools/{slug}`
//!   with the toolkit versions; a 400 or 404 is `ToolNotFoundError`), an empty string at a `file_uploadable` input
//!   dropped (`drop_empty_file_uploads`), then `POST /api/v3.1/tools/execute/{slug}` without retries (a read can
//!   still be a side effect upstream), the version of the schema's toolkit from `COMPOSIO_TOOLKIT_VERSION_<TOOLKIT>`
//!   or "latest", `log_id` and `session_info` dropped from the answer.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Map, Value};

use crate::strict_schema::to_strict_json_schema;

pub const PRODUCTION_BASE_URL: &str = "https://backend.composio.dev";
pub const DEFAULT_MAX_RETRIES: u32 = 2;
pub const TOOLS_PAGE_LIMIT: u32 = 500;
pub const SDK_VERSION: &str = "0.22.0";
/// `composio_client`'s version (the Stainless package under the SDK), as its headers carry it.
pub const CLIENT_VERSION: &str = "1.43.0";
pub const PROVIDER_NAME: &str = "openai_responses";
const NO_QUERY: &[(&str, String)] = &[];
const INITIAL_RETRY_DELAY: f64 = 0.5;
const MAX_RETRY_DELAY: f64 = 8.0;

/// An exception as the Python bridge reports it: its class name and its `str()`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ComposioError {
    pub kind: String,
    pub message: String,
}

impl ComposioError {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        ComposioError { kind: kind.into(), message: message.into() }
    }

    /// `type(e).__name__`.
    pub fn type_name(&self) -> &str {
        &self.kind
    }
}

/// One Composio session (the SDK's `ToolRouterSession`).
#[async_trait]
pub trait ComposioSession: Send + Sync {
    /// `session.session_id` ("" when the object has none).
    fn session_id(&self) -> String;
    /// `session.tools()`: the provider-wrapped tools, as the SDK hands them over.
    async fn tools(&self) -> Result<Vec<Value>, ComposioError>;
    /// `session.execute(slug, arguments=…)`: the response as a JSON value.
    async fn execute(&self, slug: &str, arguments: &Map<String, Value>) -> Result<Value, ComposioError>;
}

/// The SDK client (`Composio(...)`): its sessions, the owner's connected accounts and a direct tool run.
#[async_trait]
pub trait ComposioClient: Send + Sync {
    /// `sessions.create(user_id=…, toolkits=…, **extra)`; `multi_account` is the only extra eve passes.
    async fn create_session(
        &self,
        user_id: &str,
        toolkits: &Value,
        multi_account: Option<&Value>,
    ) -> Result<Arc<dyn ComposioSession>, ComposioError>;
    /// `sessions.use(session_id)`.
    async fn use_session(&self, session_id: &str) -> Result<Arc<dyn ComposioSession>, ComposioError>;
    /// `connected_accounts.list(user_ids=…).items`.
    async fn list_connected_accounts(&self, user_ids: &[String]) -> Result<Vec<Value>, ComposioError>;
    /// `tools.execute(slug, arguments=…, user_id=…, connected_account_id=…, dangerously_skip_version_check=True)`.
    async fn execute_tool(
        &self,
        slug: &str,
        arguments: &Map<String, Value>,
        user_id: &str,
        connected_account_id: &str,
    ) -> Result<Value, ComposioError>;
}

/// The real client, over HTTPS.
#[derive(Clone)]
pub struct HttpComposioClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    max_retries: u32,
    timeout_secs: i64,
    /// `get_toolkit_versions()`: lowercased toolkit -> version, from `COMPOSIO_TOOLKIT_VERSION_<TOOLKIT>`; empty
    /// is the SDK's global "latest".
    toolkit_versions: IndexMap<String, String>,
}

/// The process environment, read now.
fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

impl HttpComposioClient {
    /// `Composio(provider=OpenAIResponsesProvider(strict=True), timeout=timeout_secs)`: the key and base URL from
    /// the environment, as the SDK reads them.
    pub fn from_env(timeout_secs: i64) -> Result<Self, ComposioError> {
        let vars: Vec<(String, String)> = std::env::vars().collect();
        let api_key = env_var("COMPOSIO_API_KEY").unwrap_or_default();
        let base_url = env_var("COMPOSIO_BASE_URL").filter(|u| !u.is_empty());
        Self::new(&api_key, base_url.as_deref(), timeout_secs, &vars)
    }

    /// The client for this key; `base_url` None is production. `environ` carries the
    /// `COMPOSIO_TOOLKIT_VERSION_<TOOLKIT>` pins.
    pub fn new(
        api_key: &str,
        base_url: Option<&str>,
        timeout_secs: i64,
        environ: &[(String, String)],
    ) -> Result<Self, ComposioError> {
        if api_key.is_empty() {
            return Err(ComposioError::new(
                "ApiKeyNotProvidedError",
                "API Key not provided, either provide API key or export it as `COMPOSIO_API_KEY` or run \
                 `composio login`",
            ));
        }
        let timeout = Duration::from_secs(timeout_secs.max(0) as u64);
        // The SDK gives httpx one timeout for everything except connecting, which is 5 s by default; an explicit
        // timeout replaces both.
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(timeout)
            .build()
            .map_err(|e| ComposioError::new("APIConnectionError", e.to_string()))?;
        // `key.replace("COMPOSIO_TOOLKIT_VERSION_", "")`, lowercased; a later variable wins, as in a dict
        let mut toolkit_versions: IndexMap<String, String> = IndexMap::new();
        for (k, v) in environ {
            if k.starts_with("COMPOSIO_TOOLKIT_VERSION_") {
                toolkit_versions.insert(k.replace("COMPOSIO_TOOLKIT_VERSION_", "").to_lowercase(), v.clone());
            }
        }
        Ok(HttpComposioClient {
            http,
            base_url: base_url.unwrap_or(PRODUCTION_BASE_URL).trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            max_retries: DEFAULT_MAX_RETRIES,
            timeout_secs,
            toolkit_versions,
        })
    }

    /// `get_toolkit_version(toolkit, toolkit_versions)`.
    fn version_for(&self, toolkit: &str) -> String {
        self.toolkit_versions.get(&toolkit.to_lowercase()).cloned().unwrap_or_else(|| "latest".to_string())
    }

    /// The `toolkit_versions` query parameter as Stainless writes it: the global "latest", or one
    /// `toolkit_versions[<toolkit>]=<version>` per pin (nested format "brackets").
    fn toolkit_versions_query(&self) -> Vec<(String, String)> {
        if self.toolkit_versions.is_empty() {
            return vec![("toolkit_versions".to_string(), "latest".to_string())];
        }
        self.toolkit_versions.iter().map(|(k, v)| (format!("toolkit_versions[{k}]"), v.clone())).collect()
    }

    async fn request<K: AsRef<str>>(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(K, String)],
        body: Option<&Value>,
        retries: bool,
    ) -> Result<Value, ComposioError> {
        let url = format!("{}{}", self.base_url, path);
        let max_retries = if retries { self.max_retries } else { 0 };
        let mut attempt: u32 = 0;
        loop {
            let mut req = self
                .http
                .request(method.clone(), &url)
                .header("x-api-key", &self.api_key)
                .header("x-request-id", uuid::Uuid::new_v4().simple().to_string())
                .header("x-framework", PROVIDER_NAME)
                .header("x-source", "PYTHON_SDK")
                .header("x-sdk-version", SDK_VERSION)
                .header("x-runtime", runtime_env())
                .header("Accept", "application/json")
                .header("User-Agent", format!("HttpClient/Python {CLIENT_VERSION}"))
                .header("X-Stainless-Lang", "python")
                .header("X-Stainless-Package-Version", CLIENT_VERSION)
                .header("X-Stainless-OS", stainless_os())
                .header("X-Stainless-Arch", stainless_arch())
                .header("X-Stainless-Async", "false")
                .header("x-stainless-retry-count", attempt.to_string())
                .header("x-stainless-read-timeout", self.timeout_secs.to_string());
            if !query.is_empty() {
                let pairs: Vec<(&str, &str)> = query.iter().map(|(k, v)| (k.as_ref(), v.as_str())).collect();
                req = req.query(&pairs);
            }
            if let Some(b) = body {
                req = req.json(b);
            }
            let remaining = max_retries - attempt;
            match req.send().await {
                Err(e) => {
                    let err = if e.is_timeout() {
                        ComposioError::new("APITimeoutError", "Request timed out.")
                    } else {
                        ComposioError::new("APIConnectionError", "Connection error.")
                    };
                    if remaining == 0 {
                        return Err(err);
                    }
                    tracing::debug!("composio: {} retrying ({} left)", err.kind, remaining);
                    tokio::time::sleep(retry_delay(max_retries, remaining, None)).await;
                }
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let headers = resp.headers().clone();
                    let text = resp.text().await.unwrap_or_default();
                    if (200..300).contains(&status) {
                        return serde_json::from_str(&text)
                            .map_err(|e| ComposioError::new("JSONDecodeError", e.to_string()));
                    }
                    if remaining > 0 && should_retry(status, &headers) {
                        tokio::time::sleep(retry_delay(max_retries, remaining, Some(&headers))).await;
                    } else {
                        return Err(status_error(status, &text));
                    }
                }
            }
            attempt += 1;
        }
    }
}

/// `HttpClient._runtime_env`: where the client runs, then the interpreter. The daemon's Python was CPython, and
/// the environment checks are the SDK's own (Colab, Jupyter and IPython cannot host this binary).
fn runtime_env() -> String {
    let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    let place = if set("AWS_LAMBDA_FUNCTION_NAME") {
        "AWS_LAMBDA"
    } else if set("FUNCTION_NAME") || set("K_SERVICE") {
        "GOOGLE_CLOUD_FUNCTION"
    } else if set("FUNCTIONS_WORKER_RUNTIME") {
        "AZURE_FUNCTION"
    } else if set("KAGGLE_KERNEL_RUN_TYPE") {
        "KAGGLE"
    } else if set("REPL_ID") || set("REPLIT_DB_URL") {
        "REPLIT"
    } else if set("GITHUB_ACTIONS") {
        "GITHUB_ACTIONS"
    } else if set("GITLAB_CI") {
        "GITLAB_CI"
    } else if set("CIRCLECI") {
        "CIRCLECI"
    } else if set("JENKINS_HOME") {
        "JENKINS"
    } else if std::path::Path::new("/.dockerenv").exists() || std::path::Path::new("/run/.containerenv").exists() {
        "DOCKER"
    } else if std::fs::read_to_string("/proc/1/cgroup").is_ok_and(|c| c.contains("docker")) {
        "CONTAINER"
    } else {
        "LOCAL"
    };
    format!("{place}_CPYTHON")
}

/// Stainless's `get_platform()`.
fn stainless_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "MacOS",
        "windows" => "Windows",
        "android" => "Android",
        "ios" => "iOS",
        "linux" => "Linux",
        "freebsd" => "FreeBSD",
        "openbsd" => "OpenBSD",
        _ => "Unknown",
    }
}

/// Stainless's `get_architecture()`.
fn stainless_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "arm" => "arm",
        "x86_64" => "x64",
        "x86" => "x32",
        _ => "unknown",
    }
}

/// Stainless's `_should_retry`.
fn should_retry(status: u16, headers: &reqwest::header::HeaderMap) -> bool {
    match headers.get("x-should-retry").and_then(|v| v.to_str().ok()) {
        Some("true") => return true,
        Some("false") => return false,
        _ => {}
    }
    status == 408 || status == 409 || status == 429 || status >= 500
}

/// Stainless's `_calculate_retry_timeout`: a reasonable Retry-After, else 0.5 s doubling to 8 s, less up to 25%.
fn retry_delay(max_retries: u32, remaining: u32, headers: Option<&reqwest::header::HeaderMap>) -> Duration {
    if let Some(h) = headers {
        let ms = h.get("retry-after-ms").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<f64>().ok());
        let secs = ms.map(|m| m / 1000.0).or_else(|| {
            h.get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<f64>().ok())
        });
        if let Some(s) = secs {
            if s > 0.0 && s <= 60.0 {
                return Duration::from_secs_f64(s);
            }
        }
    }
    let n = (max_retries - remaining).min(1000);
    let sleep = (INITIAL_RETRY_DELAY * 2f64.powi(n as i32)).min(MAX_RETRY_DELAY);
    let jitter = 1.0 - 0.25 * rand::random::<f64>();
    Duration::from_secs_f64((sleep * jitter).max(0.0))
}

/// The SDK's status error: the class Stainless picks by status, and `Error code: N - <body>` with the body as
/// Python prints the parsed JSON.
pub fn status_error(status: u16, text: &str) -> ComposioError {
    let kind = match status {
        400 => "BadRequestError",
        401 => "AuthenticationError",
        403 => "PermissionDeniedError",
        404 => "NotFoundError",
        409 => "ConflictError",
        422 => "UnprocessableEntityError",
        429 => "RateLimitError",
        s if s >= 500 => "InternalServerError",
        _ => "APIStatusError",
    };
    let trimmed = crate::py_strip(text);
    let message = match serde_json::from_str::<Value>(trimmed) {
        Ok(body) => format!("Error code: {status} - {}", repr(&body)),
        Err(_) if !trimmed.is_empty() => trimmed.to_string(),
        Err(_) => format!("Error code: {status}"),
    };
    ComposioError::new(kind, message)
}

/// Python's `str()` of a parsed JSON body (a string is itself, anything else its repr).
fn repr(v: &Value) -> String {
    eve_tool_gate::py_str(v)
}

/// The SDK's `enhance_schema_descriptions`: a type hint on each scalar parameter's description, and a note on each
/// required one. A schema without `properties` is returned as it is.
pub fn enhance_schema_descriptions(schema: &mut Map<String, Value>) {
    // `_param in (schema.get("required") or [])`: a list compares items, a string is a substring test, a dict its keys
    let required = schema.get("required").cloned().unwrap_or(Value::Null);
    let is_required = |param: &str| match &required {
        Value::Array(r) => r.iter().any(|x| x.as_str() == Some(param)),
        Value::String(r) => r.contains(param),
        Value::Object(r) => r.contains_key(param),
        _ => false,
    };
    let Some(Value::Object(props)) = schema.get_mut("properties") else { return };
    for (param, prop) in props.iter_mut() {
        let Value::Object(prop) = prop else { continue };
        if let Some(Value::String(t)) = prop.get("type") {
            if matches!(t.as_str(), "string" | "integer" | "number" | "boolean") {
                let ext = format!("Please provide a value of type {t}.");
                let description = prop.get("description").and_then(Value::as_str).unwrap_or("");
                let description = description.trim_end_matches('.');
                let text = if description.is_empty() { ext } else { format!("{description}. {ext}") };
                prop.insert("description".into(), Value::from(text));
            }
        }
        if is_required(param) {
            let text = match prop.get("description") {
                Some(Value::String(d)) if !d.is_empty() => {
                    format!("{}. This parameter is required.", d.trim_end_matches('.'))
                }
                _ => "This parameter is required.".to_string(),
            };
            prop.insert("description".into(), Value::from(text));
        }
    }
}

/// `OpenAIResponsesProvider(strict=True).wrap_tool`: the tool as a Responses-API function tool.
pub fn wrap_tool(tool: &Map<String, Value>) -> Result<Value, ComposioError> {
    let slug = tool.get("slug").cloned().unwrap_or(Value::Null);
    let description = tool.get("description").cloned().unwrap_or(Value::Null);
    let params = match tool.get("input_parameters") {
        None | Some(Value::Null) => None,
        Some(Value::Object(p)) => {
            let mut p = p.clone();
            enhance_schema_descriptions(&mut p);
            Some(Value::Object(p))
        }
        Some(other) => {
            return Err(ComposioError::new(
                "TypeError",
                format!("argument of type '{}' is not iterable", py_type(other)),
            ))
        }
    };
    let Some(parameters) = params else {
        return Err(ComposioError::new("TypeError", "argument of type 'NoneType' is not iterable"));
    };
    let strict = to_strict_json_schema(&parameters).map_err(|e| ComposioError::new("ValueError", e.to_string()))?;
    let name = slug.as_str().unwrap_or_default().to_string();
    if !strict.unsupported.is_empty() {
        let reasons: Vec<String> = strict
            .unsupported
            .iter()
            .map(|u| {
                let p = if u.path.is_empty() { "<root>" } else { u.path.as_str() };
                format!("{p}: {} ({})", u.keyword, u.detail)
            })
            .collect();
        tracing::warn!(
            "OpenAIResponsesProvider: tool \"{}\" is sent without strict mode because its schema cannot be \
             expressed as strict structured outputs: {}",
            name,
            reasons.join("; ")
        );
        return Ok(json!({"type": "function", "name": slug, "description": description,
                          "parameters": strict.source, "strict": false}));
    }
    if strict.total_changes > 0 {
        let changes: Vec<String> = strict.changes.iter().map(|c| format!("{}: {}", c.path, c.reason)).collect();
        tracing::debug!(
            "OpenAIResponsesProvider: strict mode rewrote {} node(s) of tool \"{}\": {}",
            strict.total_changes,
            name,
            changes.join("; ")
        );
    }
    Ok(json!({"type": "function", "name": slug, "description": description,
              "parameters": strict.schema, "strict": true}))
}

fn py_type(v: &Value) -> &'static str {
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

/// Pydantic's `model_dump()` of a response model: its declared fields first, in order (absent optional ones as
/// None), then whatever else the server sent.
fn dump(body: Value, declared: &[(&str, bool)], exclude: &[&str]) -> Value {
    let Value::Object(mut map) = body else { return body };
    let mut out = Map::new();
    for (field, optional) in declared {
        match map.shift_remove(*field) {
            Some(v) => {
                out.insert(field.to_string(), v);
            }
            None if *optional => {
                out.insert(field.to_string(), Value::Null);
            }
            None => {}
        }
    }
    out.extend(map);
    for key in exclude {
        out.shift_remove(*key);
    }
    Value::Object(out)
}

// ---- `FileHelper.drop_empty_file_uploads` and `dereference_json_schema(on_unresolved="sentinel")` ----------------
//
// The SDK runs every direct tool call through this, with automatic upload off (eve's `Composio(...)` never turns it
// on): an empty string at a `file_uploadable` input is omitted, never sent as "" (composio issue #4233). Everything
// else, explicit nulls included, is forwarded untouched.

const MAX_REF_CHAIN_DEPTH: usize = 100;
const MAX_NODE_DEPTH: usize = 512;
const UNRESOLVED_REF_DESCRIPTION: &str = "Schema shape unresolved at the source — validate loosely. \
                                          See https://github.com/ComposioHQ/composio/issues/3307.";

fn ref_error(message: String) -> ComposioError {
    ComposioError::new("JSONSchemaRefResolutionError", message)
}

/// `_has_file_property`: the flag on the schema, any of its definitions, variants, properties or items.
fn has_file_property(schema: &Value, property: &str) -> bool {
    let Value::Object(schema) = schema else { return false };
    if schema.get(property).is_some_and(eve_tool_gate::py_truthy) {
        return true;
    }
    for key in ["$defs", "definitions"] {
        if let Some(Value::Object(defs)) = schema.get(key) {
            if defs.values().any(|d| has_file_property(d, property)) {
                return true;
            }
        }
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(Value::Array(variants)) = schema.get(key) {
            if variants.iter().any(|v| has_file_property(v, property)) {
                return true;
            }
        }
    }
    if let Some(Value::Object(props)) = schema.get("properties") {
        if props.values().any(|p| has_file_property(p, property)) {
            return true;
        }
    }
    match schema.get("items") {
        Some(Value::Array(items)) => items.iter().any(|i| has_file_property(i, property)),
        Some(items @ Value::Object(_)) => has_file_property(items, property),
        _ => false,
    }
}

fn file_uploadable(schema: &Value) -> bool {
    has_file_property(schema, "file_uploadable")
}

fn cycle_break_sentinel() -> Map<String, Value> {
    Map::from_iter([("type".to_string(), json!("object")), ("additionalProperties".to_string(), json!(true))])
}

/// `_try_resolve_pointer`: a local JSON Pointer (`#/a/b/0`) against the root; None when it does not resolve.
fn resolve_pointer<'a>(root: &'a Value, pointer: &str) -> Option<&'a Value> {
    if pointer == "#" || pointer.is_empty() {
        return Some(root);
    }
    let rest = pointer.strip_prefix("#/")?;
    let mut cursor = root;
    for raw in rest.split('/') {
        let segment = raw.replace("~1", "/").replace("~0", "~");
        cursor = match cursor {
            Value::Array(items) => {
                let index = crate::py_int(&segment)?;
                usize::try_from(index).ok().and_then(|i| items.get(i))?
            }
            Value::Object(map) => map.get(&segment)?,
            _ => return None,
        };
    }
    Some(cursor)
}

fn deref_walk(
    root: &Value,
    node: &Value,
    visited: &[String],
    chain_depth: usize,
    node_depth: usize,
) -> Result<Value, ComposioError> {
    if node_depth >= MAX_NODE_DEPTH {
        return Err(ref_error(format!("JSON Schema node depth exceeded cap ({MAX_NODE_DEPTH})")));
    }
    let map = match node {
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(deref_walk(root, item, visited, chain_depth, node_depth + 1)?);
            }
            return Ok(Value::Array(out));
        }
        Value::Object(map) => map,
        other => return Ok(other.clone()),
    };
    let reference = map.get("$ref").and_then(Value::as_str).filter(|r| r.starts_with('#'));
    let Some(reference) = reference else {
        if let Some(Value::String(external)) = map.get("$ref") {
            tracing::warn!("Leaving external $ref untouched: {}", external);
        }
        let mut cloned = Map::new();
        for (k, v) in map {
            cloned.insert(k.clone(), deref_walk(root, v, visited, chain_depth, node_depth + 1)?);
        }
        return Ok(Value::Object(cloned));
    };
    if chain_depth >= MAX_REF_CHAIN_DEPTH {
        return Err(ref_error(format!(
            "JSON Schema $ref chain exceeded depth cap ({MAX_REF_CHAIN_DEPTH}): {reference}"
        )));
    }
    if visited.iter().any(|v| v == reference) {
        return Ok(Value::Object(cycle_break_sentinel()));
    }
    let target = match resolve_pointer(root, reference) {
        Some(t) => t.clone(),
        None => {
            let mut sentinel = cycle_break_sentinel();
            sentinel.insert("description".into(), json!(UNRESOLVED_REF_DESCRIPTION));
            Value::Object(sentinel)
        }
    };
    let mut next: Vec<String> = visited.to_vec();
    next.push(reference.to_string());
    let resolved = deref_walk(root, &target, &next, chain_depth + 1, node_depth + 1)?;
    // sibling keywords next to `$ref` win on collision (Draft 2020-12)
    let siblings: Vec<(&String, &Value)> = map.iter().filter(|(k, _)| k.as_str() != "$ref").collect();
    let Value::Object(mut merged) = resolved else { return Ok(resolved) };
    if siblings.is_empty() {
        return Ok(Value::Object(merged));
    }
    for (k, v) in siblings {
        merged.insert(k.clone(), deref_walk(root, v, visited, chain_depth, node_depth + 1)?);
    }
    Ok(Value::Object(merged))
}

/// `dereference_json_schema(schema, on_unresolved="sentinel")`: local `$ref`s inlined, `$defs` and `definitions`
/// dropped from the root.
fn dereference_json_schema(schema: &Value) -> Result<Value, ComposioError> {
    if !schema.is_object() {
        return Ok(schema.clone());
    }
    let mut out = deref_walk(schema, schema, &[], 0, 0)?;
    if let Value::Object(map) = &mut out {
        map.shift_remove("$defs");
        map.shift_remove("definitions");
    }
    Ok(out)
}

fn schema_variants(schema: &Map<String, Value>) -> Vec<&Value> {
    let mut out = Vec::new();
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(Value::Array(variants)) = schema.get(key) {
            out.extend(variants.iter().filter(|v| v.is_object()));
        }
    }
    out
}

/// `_json_schema_type_matches_value`.
fn type_matches_value(schema: &Map<String, Value>, value: &Value) -> bool {
    match schema.get("type") {
        Some(Value::Array(types)) => types.iter().any(|t| {
            let mut one = schema.clone();
            one.insert("type".into(), t.clone());
            type_matches_value(&one, value)
        }),
        None | Some(Value::Null) => {
            if schema.contains_key("properties") {
                value.is_object()
            } else if schema.contains_key("items") {
                value.is_array()
            } else {
                false
            }
        }
        Some(Value::String(t)) => match t.as_str() {
            "array" => value.is_array(),
            "object" => value.is_object(),
            "string" => value.is_string(),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => false,
        },
        Some(_) => false,
    }
}

/// `_is_array_shaped_schema`.
fn is_array_shaped(schema: &Map<String, Value>) -> bool {
    match schema.get("type") {
        Some(Value::Array(types)) => types.iter().any(|t| t == "array"),
        None | Some(Value::Null) => schema.contains_key("items"),
        Some(t) => t == "array",
    }
}

/// `_find_uploadable_schema_variant`: the file-bearing variant whose shape fits the value, else the first one.
fn uploadable_variant<'a>(schema: &'a Map<String, Value>, value: &Value) -> Option<&'a Map<String, Value>> {
    let candidates: Vec<&Map<String, Value>> = schema_variants(schema)
        .into_iter()
        .filter(|v| file_uploadable(v))
        .filter_map(Value::as_object)
        .collect();
    if !value.is_null() {
        if let Some(c) = candidates.iter().find(|c| type_matches_value(c, value)) {
            return Some(c);
        }
    }
    candidates.first().copied()
}

/// `_drop_empty_file_value`: None means the key or item is omitted.
fn drop_empty_leaf(value: &Value) -> Option<Value> {
    if value.as_str() == Some("") {
        None
    } else {
        Some(value.clone())
    }
}

/// `_substitute_file_upload_value` with the drop-empty leaf; None means omitted from its parent.
fn substitute_file_upload_value(value: &Value, schema: Option<&Value>) -> Option<Value> {
    let Some(Value::Object(s)) = schema else { return Some(value.clone()) };
    let whole = schema.expect("matched above");
    if s.get("file_uploadable").is_some_and(eve_tool_gate::py_truthy) {
        return drop_empty_leaf(value);
    }
    if value.is_null() && is_array_shaped(s) && file_uploadable(whole) {
        return drop_empty_leaf(value);
    }
    if let Some(variant) = uploadable_variant(s, value) {
        // An empty string cannot match an array-only file input, but it may match another non-file string variant.
        if value.as_str() == Some("") && is_array_shaped(variant) {
            let matching_non_file = schema_variants(s)
                .into_iter()
                .any(|v| !file_uploadable(v) && v.as_object().is_some_and(|m| type_matches_value(m, value)));
            if matching_non_file {
                return Some(value.clone());
            }
            return drop_empty_leaf(value);
        }
        return substitute_file_upload_value(value, Some(&Value::Object(variant.clone())));
    }
    if value.as_str() == Some("") && is_array_shaped(s) && file_uploadable(whole) {
        return drop_empty_leaf(value);
    }
    if let (Value::Object(v), Some(properties)) = (value, s.get("properties")) {
        let mut processed = Map::new();
        for (key, item) in v {
            let item_schema = properties.as_object().and_then(|p| p.get(key));
            if let Some(kept) = substitute_file_upload_value(item, item_schema) {
                processed.insert(key.clone(), kept);
            }
        }
        return Some(Value::Object(processed));
    }
    if let (Value::Array(v), Some(items)) = (value, s.get("items")) {
        let items_schema = match items {
            Value::Array(list) => list.first(),
            other => Some(other),
        };
        let kept: Vec<Value> = v.iter().filter_map(|item| substitute_file_upload_value(item, items_schema)).collect();
        return Some(Value::Array(kept));
    }
    Some(value.clone())
}

/// `FileHelper.drop_empty_file_uploads(tool, request)`: the arguments with every `""` at a `file_uploadable` leaf
/// omitted; untouched when the tool has no file input.
pub fn drop_empty_file_uploads(
    input_parameters: Option<&Value>,
    arguments: &Map<String, Value>,
) -> Result<Map<String, Value>, ComposioError> {
    let Some(schema) = input_parameters.filter(|s| file_uploadable(s)) else { return Ok(arguments.clone()) };
    let schema = dereference_json_schema(schema)?;
    match substitute_file_upload_value(&Value::Object(arguments.clone()), Some(&schema)) {
        Some(Value::Object(processed)) => Ok(processed),
        other => Err(ComposioError::new(
            "AssertionError",
            format!(
                "expected dict from _substitute_file_upload_value at the root; got {}",
                other.as_ref().map(py_type).unwrap_or("object")
            ),
        )),
    }
}

struct HttpSession {
    client: HttpComposioClient,
    session_id: String,
}

#[async_trait]
impl ComposioSession for HttpSession {
    fn session_id(&self) -> String {
        self.session_id.clone()
    }

    async fn tools(&self) -> Result<Vec<Value>, ComposioError> {
        let path = format!("/api/v3.1/tool_router/session/{}/tools", urlencoding::encode(&self.session_id));
        let mut raw: Vec<Value> = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut query = vec![("limit", TOOLS_PAGE_LIMIT.to_string())];
            if let Some(c) = &cursor {
                query.insert(0, ("cursor", c.clone()));
            }
            let page = self.client.request(reqwest::Method::GET, &path, &query, None, true).await?;
            if let Some(Value::Array(items)) = page.get("items") {
                raw.extend(items.iter().cloned());
            }
            cursor = page.get("next_cursor").and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        let mut out = Vec::with_capacity(raw.len());
        for item in &raw {
            let empty = Map::new();
            let tool = item.as_object().unwrap_or(&empty);
            out.push(wrap_tool(tool)?);
        }
        Ok(out)
    }

    async fn execute(&self, slug: &str, arguments: &Map<String, Value>) -> Result<Value, ComposioError> {
        let path = format!("/api/v3.1/tool_router/session/{}/execute", urlencoding::encode(&self.session_id));
        let body = json!({"tool_slug": slug, "arguments": arguments});
        let resp = self.client.request(reqwest::Method::POST, &path, NO_QUERY, Some(&body), true).await?;
        Ok(dump(resp, &[("data", false), ("error", true), ("log_id", false)], &[]))
    }
}

fn session_from(client: &HttpComposioClient, body: &Value) -> Arc<dyn ComposioSession> {
    let session_id = match body.get("session_id") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    };
    Arc::new(HttpSession { client: client.clone(), session_id })
}

#[async_trait]
impl ComposioClient for HttpComposioClient {
    async fn create_session(
        &self,
        user_id: &str,
        toolkits: &Value,
        multi_account: Option<&Value>,
    ) -> Result<Arc<dyn ComposioSession>, ComposioError> {
        let mut body = Map::new();
        body.insert("user_id".into(), Value::from(user_id));
        body.insert("manage_connections".into(), json!({"enable": true}));
        // a bare list is the SDK's shorthand for {"enable": list}
        let toolkits = match toolkits {
            Value::Array(_) => json!({"enable": toolkits}),
            other => other.clone(),
        };
        if !toolkits.is_null() {
            body.insert("toolkits".into(), toolkits);
        }
        if let Some(Value::Object(ma)) = multi_account {
            let mut ma = ma.clone();
            if crate::truthy(ma.get("enable")) && !ma.contains_key("require_explicit_selection") {
                ma.insert("require_explicit_selection".into(), Value::Bool(true));
            }
            body.insert("multi_account".into(), Value::Object(ma));
        }
        let resp = self
            .request(reqwest::Method::POST, "/api/v3.1/tool_router/session", NO_QUERY, Some(&Value::Object(body)), true)
            .await?;
        Ok(session_from(self, &resp))
    }

    async fn use_session(&self, session_id: &str) -> Result<Arc<dyn ComposioSession>, ComposioError> {
        if session_id.is_empty() {
            return Err(ComposioError::new(
                "ValueError",
                "Expected a non-empty value for `session_id` but received ''",
            ));
        }
        let path = format!("/api/v3.1/tool_router/session/{}", urlencoding::encode(session_id));
        let resp = self.request(reqwest::Method::GET, &path, NO_QUERY, None, true).await?;
        Ok(session_from(self, &resp))
    }

    async fn list_connected_accounts(&self, user_ids: &[String]) -> Result<Vec<Value>, ComposioError> {
        // composio_client's querystring writes a list comma-joined (array_format="comma"): user_ids=a,b
        let query: Vec<(&str, String)> =
            if user_ids.is_empty() { Vec::new() } else { vec![("user_ids", user_ids.join(","))] };
        let resp = self.request(reqwest::Method::GET, "/api/v3.1/connected_accounts", &query, None, true).await?;
        Ok(match resp.get("items") {
            Some(Value::Array(items)) => items.clone(),
            _ => Vec::new(),
        })
    }

    async fn execute_tool(
        &self,
        slug: &str,
        arguments: &Map<String, Value>,
        user_id: &str,
        connected_account_id: &str,
    ) -> Result<Value, ComposioError> {
        if slug.is_empty() {
            return Err(ComposioError::new("ValueError", "Expected a non-empty value for `tool_slug` but received ''"));
        }
        // get_raw_composio_tool_by_slug: the schema first (retried like any read); an unknown slug is ToolNotFoundError
        let path = format!("/api/v3.1/tools/{}", urlencoding::encode(slug));
        let tool = match self.request(reqwest::Method::GET, &path, &self.toolkit_versions_query(), None, true).await {
            Ok(tool) => tool,
            // APIStatusError with status 400 or 404
            Err(e) if e.kind == "BadRequestError" || e.kind == "NotFoundError" => {
                return Err(ComposioError::new("ToolNotFoundError", format!("Tool with slug {slug} not found")));
            }
            Err(e) => return Err(e),
        };
        // _toolkit_slug(tool, "unknown")
        let toolkit = match tool.get("toolkit") {
            Some(Value::Object(kit)) => kit.get("slug").and_then(Value::as_str).filter(|s| !s.is_empty()),
            _ => None,
        }
        .unwrap_or("unknown");
        let arguments = drop_empty_file_uploads(tool.get("input_parameters"), arguments)?;
        let body = json!({
            "arguments": arguments,
            "connected_account_id": connected_account_id,
            "user_id": user_id,
            "version": self.version_for(toolkit),
        });
        let path = format!("/api/v3.1/tools/execute/{}", urlencoding::encode(slug));
        let resp = self.request(reqwest::Method::POST, &path, NO_QUERY, Some(&body), false).await?;
        Ok(dump(
            resp,
            &[("data", false), ("error", true), ("successful", false), ("log_id", true), ("session_info", true)],
            &["log_id", "session_info"],
        ))
    }
}
