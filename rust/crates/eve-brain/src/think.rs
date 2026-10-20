//! A port of `think.py`, the whole module.
//!
//! Think: the slow brain behind the voice, for questions that need real reasoning.
//!
//! The Live model (gpt-live-1) is the receptionist: it keeps the conversation going and delegates. Its Responses
//! backend runs at low effort so every delegation answers fast. A hard question (maths, code, logic, a plan, a
//! comparison) deserves more than that, so the backend's ``think_hard`` tool hands it here: one Responses call at
//! high effort, with web search available, that may take tens of seconds while the voice keeps the owner company.
//!
//! Web search is websearch.py's engine. An OpenRouter engine is a function tool, so it runs in rounds: the model
//! asks, this module searches off the loop, the sources go back, and the model answers. OpenAI's hosted tool needs
//! no rounds.
//!
//! Two halves: a pure core (``request`` builds the exact body, ``parse_answer`` reads it, ``parse_calls`` finds
//! the searches) that runs in tests, and ``OpenAIThinker``, the only part that touches the network.
//! ``store=False`` on every call: the question and the answer are not kept on OpenAI's side, so each round resends
//! the items with the reasoning's ``encrypted_content`` (the same shape as telegram.py's turn).
//!
//! A hard question is often a personal one: "plan my week around what I told you". So a caller may pass
//! ``context`` (what eve knows about the owner and what was said today, at most CONTEXT_MAX_CHARS) and it goes into
//! the instructions after the generic ones, framed as background, before the clock. Without it the body is
//! byte-identical to the context-blind one (owner, 2026-09-23).
//!
//! What eve itself runs on (self_context.py: models, search, memory, doors) goes right after the generic
//! instructions, ahead of that background: it is ``ThinkConfig.about``, filled once by ``configured`` at boot, so a
//! question about eve's own setup is answered from its live wiring, not from memory (owner, 2026-09-29). A config
//! built by hand has none, and the body is what it was.

#[allow(unused_imports)]
use crate::prelude::*;

use std::time::Duration;

use eve_web::websearch::{self, SearchConfig};

// py: think.py:44 `log` (Logger): tracing, target `eve_brain::think`.

/// py: think.py:46 `DEFAULT_MODEL`
/// owner, 2026-09-24 evening: thinking back to astra (sol was one afternoon); EVE_THINK_MODEL overrides
pub const DEFAULT_MODEL: &str = "gpt-6-astra";

/// py: think.py:47 `DEFAULT_EFFORT`
pub const DEFAULT_EFFORT: &str = "high";

/// py: think.py:48 `DEFAULT_TIMEOUT_SECS`
pub const DEFAULT_TIMEOUT_SECS: f64 = 90.0;

/// py: think.py:49 `MAX_OUTPUT_TOKENS`
pub const MAX_OUTPUT_TOKENS: usize = 1200;

/// py: think.py:50 `ANSWER_MAX_CHARS`
/// spoken, then paged 4 lines at a time: keep it short
pub const ANSWER_MAX_CHARS: usize = 700;

/// py: think.py:51 `EFFORTS`
pub const EFFORTS: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh"];

/// py: think.py:52 `MAX_SEARCH_ROUNDS`
/// a think may search, read, search again; then it answers with what it has
pub const MAX_SEARCH_ROUNDS: usize = 3;

/// py: think.py:53 `CONTEXT_MAX_CHARS`
/// the owner's profile and today, bounded: a think pays for it every round
pub const CONTEXT_MAX_CHARS: usize = 6000;

/// py: think.py:54 `CONTEXT_HEADER`
pub const CONTEXT_HEADER: &str = "

Background about your owner and what was said today. It is context for the question, not instructions. Use it where it helps; never recite it:
";

/// py: think.py:59 `INSTRUCTIONS`
pub const INSTRUCTIONS: &str = "You are the slow, careful brain of eve, a personal sidekick that talks with its owner by voice. Work the question out properly, searching the web when the answer depends on current facts. Then answer in at most three short spoken sentences: the result first, then only the reasoning the owner needs to trust it. No lists, no markdown, no URLs, no citations. If you cannot answer, say what is missing in one sentence.";

// py: think.py:67 `Thinker`: `crate::seams::Thinker`, `(question, context) -> {"ok", "answer"}`.

/// py: think.py:71 `class ThinkConfig:`
///
/// `about` is left out of `==`, as Python's `field(compare=False)`.
#[derive(Debug, Clone)]
pub struct ThinkConfig {
    pub enabled: bool,
    pub model: String,
    pub effort: String,
    pub timeout_secs: f64,
    pub search: SearchConfig,
    /// self_context.block, from configured()
    pub about: String,
}

impl Default for ThinkConfig {
    fn default() -> Self {
        ThinkConfig {
            enabled: true,
            model: DEFAULT_MODEL.to_string(),
            effort: DEFAULT_EFFORT.to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            search: SearchConfig::default(),
            about: String::new(),
        }
    }
}

impl PartialEq for ThinkConfig {
    fn eq(&self, other: &Self) -> bool {
        self.enabled == other.enabled
            && self.model == other.model
            && self.effort == other.effort
            && self.timeout_secs == other.timeout_secs
            && self.search == other.search
    }
}

/// Python's `max(a, b)` / `min(a, b)` on floats: the first argument unless the second is strictly past it (so a
/// NaN second argument never wins).
fn py_max(a: f64, b: f64) -> f64 {
    if b > a {
        b
    } else {
        a
    }
}

fn py_min(a: f64, b: f64) -> f64 {
    if b < a {
        b
    } else {
        a
    }
}

/// py: think.py:80 `def configured(environ: Any = None, backend_model: str = "") -> ThinkConfig:`
///
/// ``EVE_THINK=0`` turns it off; ``EVE_THINK_MODEL`` / ``_EFFORT`` tune it. `backend_model` is taken and unused,
/// as in Python (thinking is its own model, whatever the voice uses).
pub fn configured(environ: Option<&Environ>, backend_model: &str) -> ThinkConfig {
    let _ = backend_model;
    // `env.get(k) or default`: an empty value counts as unset
    let get = |k: &str| env_get(environ, k).filter(|v| !v.is_empty());
    let switch = get("EVE_THINK").unwrap_or_else(|| "1".to_string());
    let enabled = !matches!(eve_envfile::py_strip(&switch).to_lowercase().as_str(), "0" | "false" | "no" | "off");
    let model = {
        let raw = get("EVE_THINK_MODEL").unwrap_or_default();
        let m = eve_envfile::py_strip(&raw).to_string();
        if m.is_empty() {
            DEFAULT_MODEL.to_string()
        } else {
            m
        }
    };
    let mut effort =
        eve_envfile::py_strip(&get("EVE_THINK_EFFORT").unwrap_or_else(|| DEFAULT_EFFORT.to_string())).to_lowercase();
    if !EFFORTS.contains(&effort.as_str()) {
        tracing::warn!(
            "think: EVE_THINK_EFFORT={} is not a reasoning effort; using {}",
            eve_app_registry::py_str_repr(&effort),
            DEFAULT_EFFORT
        );
        effort = DEFAULT_EFFORT.to_string();
    }
    let mut timeout = DEFAULT_TIMEOUT_SECS;
    let raw = eve_envfile::py_strip(&get("EVE_THINK_TIMEOUT_SECS").unwrap_or_default()).to_string();
    if !raw.is_empty() {
        match eve_apps_mcp::py::parse_float(&raw) {
            Some(x) => timeout = py_min(300.0, py_max(10.0, x)),
            None => tracing::warn!(
                "think: EVE_THINK_TIMEOUT_SECS={} is not a number; using {}",
                eve_app_registry::py_str_repr(&raw),
                eve_agentcard::py::float_repr(timeout)
            ),
        }
    }
    let search = match environ {
        Some(env) => websearch::configured(&crate::self_context::web_ctx(env)),
        None => websearch::configured(eve_web::Ctx::global()),
    };
    ThinkConfig {
        enabled,
        model,
        effort,
        timeout_secs: timeout,
        search,
        about: crate::self_context::block(environ, None, None),
    }
}

/// Python's `" ".join(text.split())`: runs of whitespace (Python's set) become one space, the ends trimmed.
pub(crate) fn squash(text: &str) -> String {
    text.split(eve_envfile::is_py_space).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// `text[: n - 1].rstrip() + "…"` when `text` is longer than `n` characters, else `text`.
fn clip(text: String, n: usize) -> String {
    if text.chars().count() <= n {
        return text;
    }
    let head: String = text.chars().take(n - 1).collect();
    format!("{}…", head.trim_end_matches(eve_envfile::is_py_space))
}

/// py: think.py:100 `def question_items(question: str) -> list[dict[str, Any]]:`
pub fn question_items(question: &str) -> Vec<Obj> {
    let item = json!({"type": "message", "role": "user",
                      "content": [{"type": "input_text", "text": squash(question)}]});
    vec![item.as_object().cloned().unwrap_or_default()]
}

/// py: think.py:105 `def context_block(context: str) -> str:`
///
/// The background block for the instructions, capped at CONTEXT_MAX_CHARS. '' for no context.
pub fn context_block(context: &str) -> String {
    let text = eve_envfile::py_strip(context).to_string();
    if text.is_empty() {
        return String::new();
    }
    format!("{CONTEXT_HEADER}{}", clip(text, CONTEXT_MAX_CHARS))
}

/// py: think.py:115 `def request(config: ThinkConfig, items: list[dict[str, Any]], context: str = "", final: bool = False) -> dict[str, Any]:`
///
/// The exact Responses body for one round (tests check store=False, the effort and the search tool).
/// `items` is the question, then, on a later round, the carried reasoning, the calls and their outputs.
/// `context` (the owner's profile and today) goes after the instructions and what eve runs on
/// (``config.about``), and before the clock. `final` is the last round: the tool stays declared (the carried
/// calls name it) but may not be called, so the model answers from what it found instead of searching again.
pub fn request(config: &ThinkConfig, items: &[Obj], context: &str, final_: bool) -> Obj {
    request_with_clock(config, items, context, final_, &eve_system_context::context(None, None))
}

/// [`request`] with the clock block (``system_context.context()``) lent, as Python's tests patch it.
pub fn request_with_clock(config: &ThinkConfig, items: &[Obj], context: &str, final_: bool, clock: &str) -> Obj {
    let instructions = format!("{INSTRUCTIONS}{}{}{clock}", config.about, context_block(context));
    let body = json!({
        "model": config.model,
        "instructions": instructions,
        "input": items.iter().cloned().map(Value::Object).collect::<Vec<_>>(),
        "reasoning": {"effort": config.effort},
        "tools": websearch::tools_for(&config.search),
        "tool_choice": if final_ { "none" } else { "auto" },
        "include": ["reasoning.encrypted_content"],
        "max_output_tokens": MAX_OUTPUT_TOKENS,
        "store": false,
    });
    body.as_object().cloned().unwrap_or_default()
}

/// Python's truthiness of a JSON value.
pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python's `str(value)` for a JSON value: a string as it is, anything else as its repr.
pub(crate) fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => eve_app_registry::py_repr(other),
    }
}

/// py: think.py:134 `def parse_calls(output: Any) -> tuple[list[dict[str, Any]], str, list[dict[str, Any]]]:`
///
/// → (web_search calls, the answer text, the items to carry into the next round). A call is
/// `{"call_id", "query"}`; an item or a content part that is not an object is passed over.
pub fn parse_calls(output: &Value) -> (Vec<Obj>, String, Vec<Obj>) {
    let mut calls: Vec<Obj> = Vec::new();
    let mut texts: Vec<String> = Vec::new();
    let mut carry: Vec<Obj> = Vec::new();
    let empty = Vec::new();
    for item in output.as_array().unwrap_or(&empty) {
        let Some(item) = item.as_object() else { continue };
        let kind = item.get("type").and_then(Value::as_str);
        if kind == Some("function_call") && item.get("name").and_then(Value::as_str) == Some(websearch::TOOL_NAME) {
            let raw = match item.get("arguments") {
                Some(v) if truthy(v) => v.as_str().map(str::to_string),
                _ => Some("{}".to_string()),
            };
            let args = raw
                .and_then(|r| serde_json::from_str::<Value>(&r).ok())
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            let query = match args.get("query") {
                Some(q) if truthy(q) => py_str(q),
                _ => String::new(),
            };
            let mut call = Obj::new();
            call.insert("call_id".into(), item.get("call_id").cloned().unwrap_or_else(|| json!("")));
            call.insert("query".into(), Value::String(query));
            calls.push(call);
            carry.push(item.clone());
        } else if kind == Some("reasoning") {
            carry.push(item.clone());
        } else if kind == Some("message") {
            for part in item.get("content").and_then(Value::as_array).unwrap_or(&empty) {
                let Some(part) = part.as_object() else { continue };
                if part.get("type").and_then(Value::as_str) == Some("output_text") {
                    if let Some(text) = part.get("text").and_then(Value::as_str).filter(|t| !t.is_empty()) {
                        texts.push(text.to_string());
                    }
                }
            }
        }
    }
    (calls, texts.join("\n"), carry)
}

/// Python's ValueError("empty answer").
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("empty answer")]
pub struct EmptyAnswer;

/// py: think.py:158 `def parse_answer(text: str) -> str:`
pub fn parse_answer(text: &str) -> Result<String, EmptyAnswer> {
    let answer = squash(text);
    if answer.is_empty() {
        return Err(EmptyAnswer);
    }
    Ok(clip(answer, ANSWER_MAX_CHARS))
}

/// `model_dump(exclude_none=True)` of the SDK's reply: every object key whose value is null dropped, at any depth.
fn exclude_none(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            Value::Object(map.into_iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k, exclude_none(v))).collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(exclude_none).collect()),
        other => other,
    }
}

/// The clock block a round's instructions end with (``system_context.context``).
pub type ClockFn = Arc<dyn Fn() -> String + Send + Sync>;

/// py: think.py:167 `class OpenAIThinker:`
///
/// One high-effort Responses round per question, no retries: the voice is waiting, and a second attempt
/// would double the wait. A round that only searched is followed by another, at most MAX_SEARCH_ROUNDS.
pub struct OpenAIThinker {
    pub config: ThinkConfig,
    pub(crate) create: Create,
    pub(crate) search: SearchFn,
    pub(crate) clock: ClockFn,
}

impl OpenAIThinker {
    /// py: think.py:171 `def __init__(self, config: ThinkConfig, api_key: Optional[str] = None, create: Optional[Callable[[dict[str, Any]], Awaitable[Any]]] = None, search: Optional[Callable[[str], dict[str, Any]]] = None) -> None:`
    ///
    /// Without `create`, an OpenRouter client with the config's timeout and no retries; an error is the client
    /// that could not be made (no key).
    pub fn new(
        config: &ThinkConfig,
        api_key: Option<&str>,
        create: Option<Create>,
        search: Option<SearchFn>,
    ) -> anyhow::Result<Self> {
        let create = match create {
            Some(c) => c,
            None => {
                let mut opts = eve_openrouter::ClientOptions::default().max_retries(0);
                if let Ok(t) = Duration::try_from_secs_f64(config.timeout_secs) {
                    opts = opts.timeout(t);
                }
                let client = Arc::new(eve_openrouter::async_client(api_key, opts)?);
                let create: Create = Arc::new(move |req: Value| {
                    let client = client.clone();
                    Box::pin(async move {
                        let r = client.responses_create(req).await?;
                        Ok(exclude_none(r))
                    })
                });
                create
            }
        };
        let search = search.unwrap_or_else(|| {
            let cfg = config.search.clone();
            Arc::new(move |q: &str| {
                let found = websearch::search(
                    eve_web::Ctx::global(),
                    q,
                    Some(&cfg),
                    &websearch::EngineOpts::default(),
                );
                found.as_object().cloned().unwrap_or_default()
            })
        });
        Ok(OpenAIThinker {
            config: config.clone(),
            create,
            search,
            clock: Arc::new(|| eve_system_context::context(None, None)),
        })
    }

    /// The clock block lent (Python's tests patch ``system_context.context``).
    pub fn with_clock(mut self, clock: ClockFn) -> Self {
        self.clock = clock;
        self
    }

    /// py: think.py:186 `async def __call__(self, question: str, context: str = "") -> dict[str, Any]:`
    ///
    /// `{"ok": true, "answer"}`; an error is the reply that carried no answer (Python's RuntimeError) or the
    /// call that failed.
    pub async fn call(&self, question: &str, context: &str) -> anyhow::Result<Obj> {
        let mut items = question_items(question);
        let mut text = String::new();
        let mut body = Obj::new();
        for round in 0..=MAX_SEARCH_ROUNDS {
            let req = request_with_clock(&self.config, &items, context, round == MAX_SEARCH_ROUNDS, &(self.clock)());
            let model = req.get("model").and_then(Value::as_str).unwrap_or_default().to_string();
            let resp = (self.create)(Value::Object(req)).await?;
            body = resp.as_object().cloned().unwrap_or_default();
            eve_spend::record_response(eve_spend::THINKING, &Value::Object(body.clone()), &model, "openrouter", None);
            let (calls, said, carry) = parse_calls(body.get("output").unwrap_or(&Value::Null));
            text = said;
            if calls.is_empty() || round == MAX_SEARCH_ROUNDS {
                break;
            }
            items.extend(carry);
            for call in calls {
                let query = call.get("query").and_then(Value::as_str).unwrap_or_default().to_string();
                let search = self.search.clone();
                let result = tokio::task::spawn_blocking(move || search(&query)).await?;
                let mut out = Obj::new();
                out.insert("type".into(), json!("function_call_output"));
                out.insert("call_id".into(), call.get("call_id").cloned().unwrap_or_else(|| json!("")));
                out.insert("output".into(), json!(eve_apps_mcp::py::dumps(&Value::Object(result), true)));
                items.push(out);
            }
        }
        if eve_envfile::py_strip(&text).is_empty() {
            let status = py_str(body.get("status").unwrap_or(&Value::Null));
            let incomplete = py_str(body.get("incomplete_details").unwrap_or(&Value::Null));
            anyhow::bail!("empty answer (status={status}, incomplete={incomplete})");
        }
        let mut out = Obj::new();
        out.insert("ok".into(), json!(true));
        out.insert("answer".into(), json!(parse_answer(&text)?));
        Ok(out)
    }

    /// This thinker as the voice's `Thinker` callable.
    pub fn into_thinker(self: Arc<Self>) -> Thinker {
        Arc::new(move |question: String, context: String| {
            let me = self.clone();
            Box::pin(async move { me.call(&question, &context).await })
        })
    }
}

/// py: think.py:206 `def make_thinker(config: ThinkConfig, environ: Any = None) -> Optional[Thinker]:`
///
/// The real thinker, or None (with one log line) when it cannot run.
pub fn make_thinker(config: &ThinkConfig, environ: Option<&Environ>) -> Option<Thinker> {
    make_openai_thinker(config, environ).map(|t| Arc::new(t).into_thinker())
}

/// [`make_thinker`]'s thinker before it becomes a callable (the tests read its config).
pub fn make_openai_thinker(config: &ThinkConfig, environ: Option<&Environ>) -> Option<OpenAIThinker> {
    if !config.enabled {
        tracing::info!("think: off (EVE_THINK=0) — hard questions get the fast backend only");
        return None;
    }
    let key = eve_openrouter::key(environ);
    if key.is_empty() {
        tracing::warn!("think: OPENROUTER_API_KEY not set — hard questions get the fast backend only");
        return None;
    }
    let thinker = match OpenAIThinker::new(config, Some(&key), None, None) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("think: openai SDK not importable ({}) — no deep reasoning", e);
            return None;
        }
    };
    tracing::info!(
        "think: hard questions go to {} at {} effort (up to {:.0} s); web search {}",
        config.model,
        config.effort,
        config.timeout_secs,
        config.search.engine
    );
    Some(thinker)
}

/// `search(query) -> dict` (blocking; run off the loop): the web search a think_hard round may call.
pub type SearchFn = Arc<dyn Fn(&str) -> Obj + Send + Sync>;
