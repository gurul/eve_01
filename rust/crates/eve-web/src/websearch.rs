//! Shared web-search configuration for voice, text and deep reasoning.
//!
//! Perplexity through OpenRouter is the default whenever `OPENROUTER_API_KEY` is set (owner, 2026-09-24: "use this
//! benchmark to wire in good realtime search"). On OpenRouter's search benchmarks Perplexity led BrowseComp, HLE and
//! WideSearch on quality, value and speed with the model held fixed (BrowseComp, 2026-08-18: 89.0% with Claude Opus 5
//! against 82.2% on Exa), and it costs $0.005 a search against Exa's $0.007. One OpenRouter chat call carries the
//! `openrouter:web_search` server tool on the chosen engine, capped at `max_uses` searches; a cheap model writes the
//! short answer from the results, and the brain gets the answer and its sources through the `web_search` function
//! tool. (Until 2026-10-08 an OpenAI answer model was refused here, because OpenAI models ran on the OpenAI key;
//! since the owner moved every model call to his OpenRouter key, docs/openrouter.md, an `openai/*` answer model is
//! allowed.)
//!
//! Search engines do not serve a live clock (Exa could not say the time in Seattle, 2026-09-21, and Perplexity
//! cannot either, 2026-09-24), so the answer step is told the local date and time: a clock or "today" question is
//! answered from it, not from a page. Without an OpenRouter key there is no search (and no brain to ask for one):
//! `off`. `openai` (OpenAI's hosted `web_search` tool) is kept as an explicit choice only.
//! `EVE_WEB_SEARCH` = openrouter-perplexity | openrouter-exa | openai | off picks one explicitly.
//!
//! ROUTED (2026-09-29, search_router): when the search router is on (`EVE_SEARCH_ROUTER`), `configured` lists the
//! providers it may use in `SearchConfig.providers` and `search` hands each query to `search_router::answer`; with
//! `providers` empty, `search` is `engine_search`: exactly the search this module made before the router. A routed
//! TinyFish or Firecrawl attempt has `routed_secs` (EVE_SEARCH_ROUTED_SECS, default 10 s) as an outer budget.
//! `cost_usd` is a float, or None when a routed answer could not be priced. The engine search keeps its pre-router
//! figure: OpenRouter's usage.cost, else the search fee alone.
//!
//! LAST RESORT (2026-10-08, monid, docs/monid.md): when both OpenRouter engines failed, Monid's Google results
//! answer in the same shape (`fallback` "monid"), within the same budget and under its own daily caps. EVE_MONID=0
//! switches it off.
//!
//! The engines race on two threads (Python's ThreadPoolExecutor(max_workers=2)); the slower loser is left to run
//! out its own timeout in the background.

use std::collections::HashSet;
use std::sync::{mpsc, Arc, LazyLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, FixedOffset, Local, TimeZone, Utc};
use chrono_tz::Tz;
use eve_spend::RecordOpts;
use serde_json::{json, Map, Value};

use crate::{Clock, Ctx};

pub const URL: &str = "https://openrouter.ai/api/v1/chat/completions";
pub const ENGINES: [&str; 4] = ["openrouter-perplexity", "openrouter-exa", "openai", "off"];
/// eve's name -> OpenRouter's.
pub const OPENROUTER_ENGINES: [(&str, &str); 2] = [("openrouter-perplexity", "perplexity"), ("openrouter-exa", "exa")];
/// with OPENROUTER_API_KEY; without it, "off"
pub const DEFAULT_ENGINE: &str = "openrouter-perplexity";
/// Selected for synthesis by the quality/cost screen (2026-10-09); watches and reflection keep their own defaults.
pub const DEFAULT_MODEL: &str = "openai/gpt-6-luna";
pub const DEFAULT_RESULTS: i64 = 5;
/// searches per question: realtime answers, and at most $0.01 in search fees
pub const DEFAULT_MAX_USES: i64 = 2;
// 2026-10-08 evening: both OpenRouter engines took 15-27 s per search for hours (normally 4-6 s, p95 5.1 s), and the
// 12 s + 8 s budget set that morning turned every search into "the search timed out" (owner: "everything was working
// on telegram", where the limit had been 20 s). Now the engines race: the second starts when the first is slow
// (HEDGE_SECS) or fails, the first good answer wins, and the whole search waits at most DEFAULT_TIMEOUT_SECS.
/// the whole search, both engines
pub const DEFAULT_TIMEOUT_SECS: f64 = 25.0;
/// past the normal p95: start the other engine too
pub const HEDGE_SECS: f64 = 6.0;
/// kept for callers that read it: the other engine gets what is left
pub const FALLBACK_SECS: f64 = DEFAULT_TIMEOUT_SECS;
pub const FALLBACK_ENGINE: [(&str, &str); 2] =
    [("openrouter-perplexity", "openrouter-exa"), ("openrouter-exa", "openrouter-perplexity")];
/// a routed TinyFish/Firecrawl attempt, then the engine on its own timeout
pub const DEFAULT_ROUTED_SECS: f64 = 10.0;
pub const MAX_ANSWER_CHARS: usize = 1500;
pub const MAX_SNIPPET_CHARS: usize = 300;
pub const MAX_SOURCES: usize = 8;
/// Per search, up to 10 results (openrouter.ai/docs server-tools/web-search, 2026-09-24); the model's tokens come on
/// top. Used only when OpenRouter's reply carries no usage.cost.
pub const SEARCH_USD: [(&str, f64); 2] = [("perplexity", 0.005), ("exa", 0.007)];

pub const TOOL_NAME: &str = "web_search";
pub static WEB_SEARCH_TOOL: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "function", "name": TOOL_NAME, "strict": true,
        "description": "Search the web for current facts: news, scores, prices, weather, opening hours, anything after \
                        your training. Returns a short answer and the sources it came from. The current date and time are \
                        already in your context; a search is not needed for them.",
        "parameters": {"type": "object", "additionalProperties": false, "required": ["query"],
                       "properties": {"query": {"type": "string",
                                                "description": "What to look up, as a search query in plain words."}}},
    })
});
/// OpenAI's own, the default.
pub static HOSTED_TOOL: LazyLock<Value> = LazyLock::new(|| json!({"type": "web_search"}));

/// What the routed search adds to the tool's description, per provider it may use: the brain reads its own reach
/// here (a brain that does not know its search can price a flight never asks it to).
pub const ROUTED_REACH: [(&str, &str); 2] = [
    ("tinyfish", "a single fact or what one specific page says is read straight from the pages"),
    (
        "firecrawl",
        "live flight fares and official records come from live data services, so give the route, dates or name in the query",
    ),
];
pub const ROUTED_NOTE: &str =
    "Each query is routed to the search that fits it: news and questions that need many sources get a many-source search";

pub const SYSTEM: &str = "You are the answer step of eve's web search: eve, the owner's desk-robot assistant, searched the web \
for this query. Using only the web results provided, answer the query in at most four plain sentences \
with the concrete facts (numbers, names, dates) and say which source each comes from by its title. If the \
results do not answer it, say so in one sentence. No markdown, no lists.";
/// The clock the answer step reads: web pages are not a live clock, so the current time comes from here.
/// `{now}` and `{zone}` are filled by `clock`.
pub const CLOCK: &str = "The current local date and time is {now}, time zone {zone}. For a question about the current time or date, \
or what is happening today, use this clock, converting to another place's time zone when asked; do not \
take the time from a web page.";

/// of the budget, kept for Monid when it is on: it can then cover engines that hang
pub const MONID_RESERVE_SECS: f64 = 10.0;
/// OpenRouter refusing the account or the rate: Monid can still answer
pub const MONID_HTTP: [u16; 4] = [401, 402, 403, 429];

/// `OPENROUTER_ENGINES.get(engine)`.
pub fn openrouter_engine(engine: &str) -> Option<&'static str> {
    OPENROUTER_ENGINES.iter().find(|(k, _)| *k == engine).map(|(_, v)| *v)
}

/// `SEARCH_USD.get(engine, 0.005)`.
pub fn search_usd(engine: &str) -> f64 {
    SEARCH_USD.iter().find(|(k, _)| *k == engine).map(|(_, v)| *v).unwrap_or(0.005)
}

/// The search's settings. `Default` is Python's bare `SearchConfig()` (engine "openai": a bare config, no
/// environment read, never needs an OpenRouter key).
#[derive(Debug, Clone, PartialEq)]
pub struct SearchConfig {
    pub engine: String,
    pub model: String,
    pub max_results: i64,
    pub timeout_secs: f64,
    pub max_uses: i64,
    /// search_router's providers when routing is on; empty = the engine alone
    pub providers: Vec<String>,
    /// outer budget of a routed non-engine attempt (search_router::bounded)
    pub routed_secs: f64,
}

impl Default for SearchConfig {
    fn default() -> Self {
        SearchConfig {
            engine: "openai".into(),
            model: DEFAULT_MODEL.into(),
            max_results: DEFAULT_RESULTS,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_uses: DEFAULT_MAX_USES,
            providers: Vec::new(),
            routed_secs: DEFAULT_ROUTED_SECS,
        }
    }
}

/// One request `engine_once` sends: the URL, the JSON body, the headers (Python's `urllib.request.Request`).
#[derive(Debug, Clone, PartialEq)]
pub struct OpenRequest {
    pub url: String,
    pub body: Vec<u8>,
    pub headers: Vec<(String, String)>,
}

impl OpenRequest {
    /// `req.get_header(name)`, case-insensitive.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// What `urlopen` raised, as `engine_once` tells them apart.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum OpenError {
    /// `urllib.error.HTTPError`: the service answered this status.
    #[error("HTTP {0}")]
    Http(u16),
    /// `urllib.error.URLError`: unreachable; the reason's class name, for the log.
    #[error("unreachable ({0})")]
    Url(String),
    /// `TimeoutError` / `OSError`.
    #[error("timed out")]
    Timeout,
}

/// `opener(req, timeout=)`: the response body. The tests' fake; the real one is [`SystemOpener`].
pub trait Opener: Send + Sync {
    fn open(&self, req: &OpenRequest, timeout: f64) -> Result<Vec<u8>, OpenError>;
}

impl<F> Opener for F
where
    F: Fn(&OpenRequest, f64) -> Result<Vec<u8>, OpenError> + Send + Sync,
{
    fn open(&self, req: &OpenRequest, timeout: f64) -> Result<Vec<u8>, OpenError> {
        self(req, timeout)
    }
}

/// `urllib.request.urlopen`: one blocking POST (reqwest, rustls), honouring the environment's proxy as urllib does.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemOpener;

impl Opener for SystemOpener {
    fn open(&self, req: &OpenRequest, timeout: f64) -> Result<Vec<u8>, OpenError> {
        let mut builder = reqwest::blocking::Client::builder();
        if timeout > 0.0 {
            if let Ok(d) = Duration::try_from_secs_f64(timeout) {
                builder = builder.timeout(d);
            }
        }
        let client = builder.build().map_err(|e| OpenError::Url(reqwest_reason(&e)))?;
        let mut rb = client.post(&req.url).body(req.body.clone());
        for (k, v) in &req.headers {
            rb = rb.header(k.as_str(), v.as_str());
        }
        let response = match rb.send() {
            Ok(r) => r,
            // urllib wraps a failure to connect (a refused socket, a failed lookup, a connect timeout) in URLError;
            // a timeout once connected is a bare TimeoutError.
            Err(e) if e.is_connect() => return Err(OpenError::Url(reqwest_reason(&e))),
            Err(e) if e.is_timeout() => return Err(OpenError::Timeout),
            Err(e) => return Err(OpenError::Url(reqwest_reason(&e))),
        };
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(OpenError::Http(status));
        }
        // a read that fails after the headers is urllib's OSError / TimeoutError
        response.bytes().map(|b| b.to_vec()).map_err(|_| OpenError::Timeout)
    }
}

/// The class name urllib's `URLError.reason` would have had, for the log.
fn reqwest_reason(e: &reqwest::Error) -> String {
    let mut source: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(e);
    while let Some(err) = source {
        if let Some(io) = err.downcast_ref::<std::io::Error>() {
            return match io.kind() {
                std::io::ErrorKind::ConnectionRefused => "ConnectionRefusedError",
                std::io::ErrorKind::ConnectionReset => "ConnectionResetError",
                std::io::ErrorKind::ConnectionAborted => "ConnectionAbortedError",
                std::io::ErrorKind::TimedOut => "TimeoutError",
                std::io::ErrorKind::PermissionDenied => "PermissionError",
                _ => "OSError",
            }
            .to_string();
        }
        let text = err.to_string().to_lowercase();
        if text.contains("dns") || text.contains("lookup") || text.contains("resolve") {
            return "gaierror".to_string();
        }
        if text.contains("certificate") || text.contains("tls") {
            return "SSLError".to_string();
        }
        source = err.source();
    }
    if e.is_timeout() {
        "TimeoutError".to_string()
    } else {
        "OSError".to_string()
    }
}

/// `search`'s and `engine_search`'s keyword arguments. `Default` is Python's defaults: `key=""` (then
/// OPENROUTER_API_KEY), `opener=None` (urlopen), `clock=time.perf_counter`, `system=SYSTEM`,
/// `feature=spend.SEARCH`, `monid_ok=True`; `hedge_secs` HEDGE_SECS.
#[derive(Clone)]
pub struct EngineOpts {
    pub key: String,
    pub opener: Option<Arc<dyn Opener>>,
    pub clock: Clock,
    pub system: String,
    pub feature: String,
    pub monid_ok: bool,
    /// HEDGE_SECS, a field so a test can shorten it (Python's tests patch the module constant).
    pub hedge_secs: f64,
}

impl Default for EngineOpts {
    fn default() -> Self {
        EngineOpts {
            key: String::new(),
            opener: None,
            clock: crate::perf_clock(),
            system: SYSTEM.to_string(),
            feature: eve_spend::SEARCH.to_string(),
            monid_ok: true,
            hedge_secs: HEDGE_SECS,
        }
    }
}

// ---- Python's str and number semantics, shared with search_router ------------------------------------------

/// `str.isspace()`: Unicode White_Space plus the ASCII information separators \x1c-\x1f, which Python counts.
pub(crate) fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `str.strip()` with no argument.
pub(crate) fn py_strip(s: &str) -> &str {
    s.trim_matches(is_py_space)
}

/// `" ".join(s.split())`.
pub(crate) fn words(s: &str) -> String {
    s.split(is_py_space).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// `s[:n]`: the first `n` characters (code points).
pub(crate) fn take_chars(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

/// `round(x, digits)`: correctly rounded, ties to even on the exact binary value (Rust's formatting does the same).
pub(crate) fn py_round(x: f64, digits: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.digits$}").parse().unwrap_or(x)
}

/// `round(x)` to an int: ties to even.
pub(crate) fn py_round_int(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// Python's truthiness of a decoded JSON value.
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

/// `str(v)` of a decoded JSON value: a str is itself, anything else its repr.
pub(crate) fn py_str_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => eve_decider::py_repr(other),
    }
}

/// `type(v).__name__` of a decoded JSON value.
pub(crate) fn py_type_name(v: &Value) -> &'static str {
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

/// Underscores Python's numeric literals allow (one between two digits), removed; None when misplaced.
fn py_underscores(s: &str) -> Option<String> {
    if !s.contains('_') {
        return Some(s.to_string());
    }
    let chars: Vec<char> = s.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if *c == '_' {
            let before = i.checked_sub(1).and_then(|j| chars.get(j)).is_some_and(|c| c.is_ascii_digit());
            let after = chars.get(i + 1).is_some_and(|c| c.is_ascii_digit());
            if !before || !after {
                return None;
            }
        }
    }
    Some(s.replace('_', ""))
}

/// `int(s)` of a str: None where Python raises ValueError. A number past i64 saturates (Python's ints do not
/// overflow, and every caller clamps).
pub(crate) fn py_int_str(s: &str) -> Option<i64> {
    let t = py_underscores(py_strip(s))?;
    let (neg, digits) = match t.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, t.strip_prefix('+').unwrap_or(&t)),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(match digits.parse::<i64>() {
        Ok(n) if neg => -n,
        Ok(n) => n,
        Err(_) if neg => i64::MIN,
        Err(_) => i64::MAX,
    })
}

/// `float(s)` of a str: None where Python raises ValueError.
pub(crate) fn py_float_str(s: &str) -> Option<f64> {
    let t = py_underscores(py_strip(s))?;
    if t.is_empty() {
        return None;
    }
    t.parse::<f64>().ok()
}

/// `int(v or 0)` of a decoded JSON value; Err is the exception's class name (ValueError, TypeError,
/// OverflowError).
pub(crate) fn py_int_or_zero(v: Option<&Value>) -> Result<i64, &'static str> {
    let Some(v) = v.filter(|v| truthy(v)) else { return Ok(0) };
    match v {
        Value::Bool(b) => Ok(i64::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i)
            } else if let Some(u) = n.as_u64() {
                Ok(i64::try_from(u).unwrap_or(i64::MAX))
            } else {
                let f = n.as_f64().unwrap_or(0.0);
                if f.is_nan() {
                    Err("ValueError")
                } else if f.is_infinite() {
                    Err("OverflowError")
                } else {
                    Ok(f.trunc() as i64)
                }
            }
        }
        Value::String(s) => py_int_str(s).ok_or("ValueError"),
        _ => Err("TypeError"),
    }
}

/// `"%g" % x`.
pub(crate) fn py_g(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let sci = format!("{x:.5e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let strip = |s: &str| -> String {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s.to_string()
        }
    };
    if !(-4..6).contains(&exp) {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", strip(mantissa), exp.abs())
    } else {
        let decimals = usize::try_from(5 - exp).unwrap_or(0);
        strip(&format!("{x:.decimals$}"))
    }
}

/// `log.warning("... %r is not one of %s ...")`'s tuple text.
fn engines_repr() -> String {
    let parts: Vec<String> = ENGINES.iter().map(|e| eve_decider::py_str_repr(e)).collect();
    format!("({})", parts.join(", "))
}

// ---- the configuration and the request ----------------------------------------------------------------------

/// EVE_WEB_SEARCH = openrouter-perplexity | openrouter-exa | openai | off. Unset: openrouter-perplexity when
/// OPENROUTER_API_KEY is set, else off. EVE_WEB_SEARCH_MODEL, _RESULTS (1-10) and _MAX_USES (1-5) tune the call;
/// any OpenRouter model id answers, openai/* included. `providers` is search_router::available: empty unless
/// EVE_SEARCH_ROUTER routes on this engine. EVE_SEARCH_ROUTED_SECS (1-60, default 10) is a routed attempt's
/// outer budget.
pub fn configured(ctx: &Ctx) -> SearchConfig {
    let key = py_strip(&ctx.get("OPENROUTER_API_KEY")).to_string();
    let mut raw = py_strip(&ctx.get("EVE_WEB_SEARCH")).to_lowercase();
    if !raw.is_empty() && !ENGINES.contains(&raw.as_str()) {
        tracing::warn!(
            "web search: EVE_WEB_SEARCH={} is not one of {}; using the default",
            eve_decider::py_str_repr(&raw),
            engines_repr()
        );
        raw = String::new();
    }
    let mut engine = if !raw.is_empty() {
        raw
    } else if !key.is_empty() {
        DEFAULT_ENGINE.to_string()
    } else {
        "off".to_string()
    };
    if openrouter_engine(&engine).is_some() && key.is_empty() {
        tracing::warn!("web search: asked for {engine} but OPENROUTER_API_KEY is not set; search is off");
        engine = "off".to_string();
    }
    let model_raw = ctx.get("EVE_WEB_SEARCH_MODEL");
    let model = py_strip(if model_raw.is_empty() { DEFAULT_MODEL } else { &model_raw }).to_string();
    let int_or = |name: &str, default: i64, lo: i64, hi: i64| -> i64 {
        let raw = ctx.get(name);
        if raw.is_empty() {
            return default.clamp(lo, hi);
        }
        match py_int_str(&raw) {
            Some(n) => n.clamp(lo, hi),
            None => default,
        }
    };
    let max_results = int_or("EVE_WEB_SEARCH_RESULTS", DEFAULT_RESULTS, 1, 10);
    let max_uses = int_or("EVE_WEB_SEARCH_MAX_USES", DEFAULT_MAX_USES, 1, 5);
    let routed_raw = ctx.get("EVE_SEARCH_ROUTED_SECS");
    let routed = if routed_raw.is_empty() {
        DEFAULT_ROUTED_SECS
    } else {
        match py_float_str(&routed_raw) {
            // max(1.0, min(60.0, x)): Python's min/max keep the first argument against a NaN
            Some(x) => {
                let low = if x < 60.0 { x } else { 60.0 };
                if low > 1.0 {
                    low
                } else {
                    1.0
                }
            }
            None => DEFAULT_ROUTED_SECS,
        }
    };
    let providers = crate::search_router::available(ctx, &engine);
    SearchConfig {
        engine,
        model,
        max_results,
        timeout_secs: DEFAULT_TIMEOUT_SECS,
        max_uses,
        providers,
        routed_secs: routed,
    }
}

/// The tool(s) a brain offers for the web: the function tool (routed when there are providers), the hosted one,
/// or nothing.
pub fn tools_for(config: &SearchConfig) -> Vec<Value> {
    if openrouter_engine(&config.engine).is_some() {
        return vec![if config.providers.is_empty() { WEB_SEARCH_TOOL.clone() } else { routed_tool(&config.providers) }];
    }
    if config.engine == "openai" {
        return vec![HOSTED_TOOL.clone()];
    }
    Vec::new()
}

/// WEB_SEARCH_TOOL with its description telling the brain what the routed search reaches.
pub fn routed_tool(providers: &[String]) -> Value {
    let reach: String = providers
        .iter()
        .filter_map(|p| ROUTED_REACH.iter().find(|(k, _)| k == p).map(|(_, r)| format!("; {r}")))
        .collect();
    let note = format!("{ROUTED_NOTE}{reach}.");
    let mut tool = WEB_SEARCH_TOOL.clone();
    if let Some(obj) = tool.as_object_mut() {
        let description = obj.get("description").and_then(Value::as_str).unwrap_or_default().to_string();
        obj.insert("description".into(), Value::String(format!("{description} {note}")));
    }
    tool
}

/// The local zone's abbreviation at `ts` (`datetime.astimezone().tzname()`, e.g. "PDT").
#[cfg(unix)]
fn local_tzname(ts: i64) -> String {
    let t = ts as libc::time_t;
    // SAFETY: localtime_r writes into the zeroed `tm` we own; tm_zone, when set, points at a static C string.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() || tm.tm_zone.is_null() {
            return Local.timestamp_opt(ts, 0).single().map(|d| d.format("UTC%:z").to_string()).unwrap_or_default();
        }
        std::ffi::CStr::from_ptr(tm.tm_zone).to_string_lossy().into_owned()
    }
}

#[cfg(not(unix))]
fn local_tzname(ts: i64) -> String {
    Local.timestamp_opt(ts, 0).single().map(|d| d.format("UTC%:z").to_string()).unwrap_or_default()
}

/// `_clock(environ, now)`: CLOCK filled with the local time ("%A %Y-%m-%d %H:%M") in EVE_TIMEZONE (when it names a
/// zone; else the machine's), and the zone's name (the key, else the local tzname).
pub fn clock(ctx: &Ctx, now: Option<DateTime<FixedOffset>>) -> String {
    let zone_name = py_strip(&ctx.get("EVE_TIMEZONE")).to_string();
    // ZoneInfo reads the zone files, which macOS's file system finds whatever the case; the key is kept as given.
    let zone = if zone_name.is_empty() { None } else { Tz::from_str_insensitive(&zone_name).ok() };
    let instant: DateTime<Utc> = now.map(|n| n.with_timezone(&Utc)).unwrap_or_else(Utc::now);
    let (text, name) = match zone {
        Some(tz) => (instant.with_timezone(&tz).format("%A %Y-%m-%d %H:%M").to_string(), zone_name),
        None => (
            instant.with_timezone(&Local).format("%A %Y-%m-%d %H:%M").to_string(),
            local_tzname(instant.timestamp()),
        ),
    };
    CLOCK.replacen("{now}", &text, 1).replacen("{zone}", &name, 1)
}

/// Reasoning models need room for both thinking and the short answer; keep older override budgets.
pub fn answer_options(model: &str, max_tokens: i64) -> Map<String, Value> {
    let model_id = eve_openrouter::model(model);
    let v = if model_id.starts_with("openai/gpt-6") {
        json!({"max_tokens": max_tokens.max(2048), "reasoning": {"effort": "low"}})
    } else if model_id == "google/gemini-3.8-flash" {
        json!({"max_tokens": max_tokens.max(2048), "temperature": 0, "reasoning": {"effort": "low"}})
    } else if model_id == "anthropic/claude-haiku-5.5" {
        json!({"max_tokens": max_tokens})
    } else if model_id.starts_with("inception/") {
        // Mercury reasons by default; low effort keeps the answer one burst (79 reasoning tokens, 2026-10-19)
        json!({"max_tokens": max_tokens.max(1024), "reasoning": {"effort": "low"}})
    } else {
        json!({"max_tokens": max_tokens, "temperature": 0})
    };
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// The exact OpenRouter body: the cheap answer model, the web search server tool on the engine with its budget,
/// the clock and the answer-box rules (`system`, default SYSTEM) as the system turn, the query as the user turn.
pub fn request(ctx: &Ctx, config: &SearchConfig, query: &str, now: Option<DateTime<FixedOffset>>, system: &str) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), Value::String(config.model.clone()));
    body.insert(
        "tools".into(),
        json!([{"type": "openrouter:web_search",
                "parameters": {"engine": openrouter_engine(&config.engine).unwrap_or("perplexity"),
                               "max_results": config.max_results, "max_uses": config.max_uses}}]),
    );
    body.insert(
        "messages".into(),
        json!([{"role": "system", "content": format!("{system} {}", clock(ctx, now))},
               {"role": "user", "content": take_chars(&words(query), 500)}]),
    );
    for (k, v) in answer_options(&config.model, 400) {
        body.insert(k, v);
    }
    body.insert("usage".into(), json!({"include": true}));
    if let Some(prefs) = eve_openrouter::provider_prefs(&config.model) {
        body.insert("provider".into(), prefs);
    }
    Value::Object(body)
}

/// {"ok", "answer", "sources": [{"title", "url", "snippet"}], "usage"} from a chat completion. A reply with no
/// annotations is still an answer; a reply with no text is not. Err is Python's ValueError, with its message.
pub fn parse(payload: &Value) -> Result<Value, String> {
    let no_choices = || "no choices in the reply".to_string();
    let obj = payload.as_object().ok_or_else(no_choices)?;
    let choices = obj.get("choices").and_then(Value::as_array).filter(|c| !c.is_empty()).ok_or_else(no_choices)?;
    let empty = Map::new();
    // `(choices[0] or {}).get("message") or {}`: a truthy non-dict has no .get (Python's AttributeError)
    let first = &choices[0];
    let first_map = match first {
        v if !truthy(v) => &empty,
        Value::Object(m) => m,
        other => return Err(format!("'{}' object has no attribute 'get'", py_type_name(other))),
    };
    let message = match first_map.get("message") {
        Some(v) if !truthy(v) => &empty,
        None => &empty,
        Some(Value::Object(m)) => m,
        Some(other) => return Err(format!("'{}' object has no attribute 'get'", py_type_name(other))),
    };
    let content = message.get("content").filter(|v| truthy(v)).map(py_str_value).unwrap_or_default();
    let answer = take_chars(&words(&content), MAX_ANSWER_CHARS);
    let mut sources: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let annotations: &[Value] = match message.get("annotations") {
        None => &[],
        Some(v) if !truthy(v) => &[],
        Some(Value::Array(a)) => a,
        // iterating a dict or a str yields keys or characters, never a dict: nothing to read
        Some(Value::Object(_)) | Some(Value::String(_)) => &[],
        Some(other) => return Err(format!("'{}' object is not iterable", py_type_name(other))),
    };
    for ann in annotations {
        let Some(cite) = ann.as_object().and_then(|a| a.get("url_citation")).and_then(Value::as_object) else {
            continue;
        };
        let field = |name: &str| cite.get(name).filter(|v| truthy(v)).map(py_str_value).unwrap_or_default();
        let url = py_strip(&field("url")).to_string();
        if url.is_empty() || seen.contains(&url) {
            continue;
        }
        seen.insert(url.clone());
        sources.push(json!({"title": take_chars(&words(&field("title")), 120), "url": take_chars(&url, 500),
                            "snippet": take_chars(&words(&field("content")), MAX_SNIPPET_CHARS)}));
        if sources.len() >= MAX_SOURCES {
            break;
        }
    }
    if answer.is_empty() && sources.is_empty() {
        return Err("the reply had neither an answer nor a source".into());
    }
    let usage = obj.get("usage").and_then(Value::as_object).unwrap_or(&empty);
    let tin = py_int_or_zero(usage.get("prompt_tokens")).map_err(|k| format!("{k} in usage.prompt_tokens"))?;
    let tout = py_int_or_zero(usage.get("completion_tokens")).map_err(|k| format!("{k} in usage.completion_tokens"))?;
    let mut u = Map::new();
    u.insert("in".into(), json!(tin));
    u.insert("out".into(), json!(tout));
    // isinstance(cost, (int, float)): a bool is an int to Python
    match usage.get("cost") {
        Some(Value::Number(n)) => {
            u.insert("cost".into(), json!(n.as_f64().unwrap_or(0.0)));
        }
        Some(Value::Bool(b)) => {
            u.insert("cost".into(), json!(if *b { 1.0 } else { 0.0 }));
        }
        _ => {}
    }
    Ok(json!({"ok": true, "answer": answer, "sources": sources, "usage": Value::Object(u)}))
}

// ---- the search -----------------------------------------------------------------------------------------------

/// The brain's web search, blocking (callers run it off the loop). Never fails: {"ok": False, "reason": …}
/// instead. With `config.providers` it is search_router::answer (Jev picks the provider; the same result shape);
/// without, engine_search. `config` None is `configured(ctx)`. Logs the shape and the time, never the query.
pub fn search(ctx: &Ctx, query: &str, config: Option<&SearchConfig>, opts: &EngineOpts) -> Value {
    let cfg = config.cloned().unwrap_or_else(|| configured(ctx));
    if !cfg.providers.is_empty() {
        let routed = crate::search_router::AnswerOpts {
            key: opts.key.clone(),
            opener: opts.opener.clone(),
            clock: opts.clock.clone(),
            ..Default::default()
        };
        return crate::search_router::answer(ctx, query, &cfg, &routed);
    }
    engine_search(ctx, query, Some(&cfg), opts)
}

/// `FALLBACK_ENGINE.get(engine)`.
fn fallback_engine(engine: &str) -> Option<&'static str> {
    FALLBACK_ENGINE.iter().find(|(k, _)| *k == engine).map(|(_, v)| *v)
}

/// `out.get(name)` of a result object.
fn field<'a>(out: &'a Value, name: &str) -> Option<&'a Value> {
    out.as_object().and_then(|o| o.get(name))
}

/// `out.get("ok") is not False`.
fn not_false(out: &Value) -> bool {
    !matches!(field(out, "ok"), Some(Value::Bool(false)))
}

/// `int(out.get("http") or 0)`.
fn http_of(out: &Value) -> i64 {
    py_int_or_zero(field(out, "http")).unwrap_or(0)
}

/// engine_search's `clean`: the attempt's private fields off.
fn clean(mut out: Value) -> Value {
    if let Some(o) = out.as_object_mut() {
        o.shift_remove("transient");
        o.shift_remove("failed_ms");
        o.shift_remove("http");
    }
    out
}

/// `Duration` for a wait of `secs` (clamped at zero; an unrepresentable wait is a very long one).
fn wait_for(secs: f64) -> Duration {
    Duration::try_from_secs_f64(secs.max(0.0)).unwrap_or(Duration::from_secs(u64::from(u32::MAX)))
}

/// One search on the engine (Perplexity through OpenRouter by default), the search before the router and the
/// router's safe default. Hedged across the two OpenRouter engines (HEDGE_SECS), the whole search waiting at most
/// `config.timeout_secs`; when BOTH engines failed (and neither with a 4xx other than 429), Monid's Google results
/// get one try with what is left of the budget and answer with `fallback` "monid". Never fails.
pub fn engine_search(ctx: &Ctx, query: &str, config: Option<&SearchConfig>, opts: &EngineOpts) -> Value {
    let cfg = config.cloned().unwrap_or_else(|| configured(ctx));
    let Some(other) = fallback_engine(&cfg.engine) else {
        return clean(engine_once(ctx, query, &cfg, opts));
    };
    let budget = cfg.timeout_secs;
    let use_monid = opts.monid_ok && ctx.monid.mode(ctx.env.as_deref()) != "off";
    // with Monid on, the engines race for the budget less Monid's reserve, so a hang still leaves it time
    let race = if use_monid && budget > MONID_RESERVE_SECS + opts.hedge_secs { budget - MONID_RESERVE_SECS } else { budget };

    // Both engines failed: Monid once with what is left of the budget (monid, at most 12 s), else the first
    // engine's failure. Never after a 4xx other than 429 (a request the engines refused is not retried).
    let last_resort = |failures: [&Value; 2], started: Instant, deadline: Instant| -> Value {
        let original = clean(failures[0].clone());
        let refused = failures.iter().any(|f| {
            let code = http_of(f);
            (400..500).contains(&code) && !MONID_HTTP.iter().any(|m| i64::from(*m) == code)
        });
        if refused || !use_monid {
            return original;
        }
        tracing::info!("web search: both engines failed; trying Monid");
        // `deadline - time.monotonic()`: negative once the budget is spent
        let now = Instant::now();
        let left = if deadline >= now {
            deadline.duration_since(now).as_secs_f64()
        } else {
            -now.duration_since(deadline).as_secs_f64()
        };
        let monid_opts = eve_monid::SearchOpts { environ: ctx.env.as_ref().map(|e| (**e).clone()), ..Default::default() };
        let mut out = ctx.monid.web_search(query, left, &monid_opts);
        if !field(&out, "ok").is_some_and(truthy) {
            return original;
        }
        if let Some(o) = out.as_object_mut() {
            // the whole wait, engines and Monid
            o.insert("ms".into(), json!(py_round_int(started.elapsed().as_secs_f64() * 1000.0)));
        }
        out
    };

    // ThreadPoolExecutor(max_workers=2): each engine on its own thread; a slower loser keeps running in the
    // background until its own timeout, and its answer is dropped with the channel.
    let (tx, rx) = mpsc::channel::<(usize, Value)>();
    let spawn = |idx: usize, c: SearchConfig| -> Option<Value> {
        let (ctx2, q, o, tx2, c2) = (ctx.clone(), query.to_string(), opts.clone(), tx.clone(), c.clone());
        let spawned = std::thread::Builder::new().name("websearch".into()).spawn(move || {
            let out = engine_once(&ctx2, &q, &c2, &o);
            let _ = tx2.send((idx, out));
        });
        match spawned {
            Ok(_) => None,
            Err(_) => Some(engine_once(ctx, query, &c, opts)),
        }
    };

    let started = Instant::now();
    let mut first_failure: Option<Value> = None;
    let first_done: Option<Value> = match spawn(0, cfg.clone()) {
        Some(inline) => Some(inline),
        None => rx.recv_timeout(wait_for(opts.hedge_secs.min(race))).ok().map(|(_, v)| v),
    };
    let done = first_done.is_some();
    if let Some(out) = first_done {
        if use_monid && MONID_HTTP[..3].iter().any(|m| i64::from(*m) == http_of(&out)) {
            // OpenRouter refused the account (401/402/403): the other engine is the same account, so straight to
            // the last resort
            return last_resort([&out, &out], started, started + wait_for(budget));
        }
        if not_false(&out) || !field(&out, "transient").is_some_and(truthy) {
            return clean(out);
        }
        first_failure = Some(out);
    }
    let hedge_at = started.elapsed().as_secs_f64();
    let left = (race - hedge_at).max(1.0);
    let why = match &first_failure {
        Some(f) => format!("failed ({})", field(f, "reason").map(py_str_value).unwrap_or_else(|| "None".into())),
        None => "is slow".to_string(),
    };
    tracing::info!("web search: {} {}; starting {} too", cfg.engine, why, other);
    let second_cfg = SearchConfig { engine: other.to_string(), timeout_secs: left, ..cfg.clone() };
    let mut pending: Vec<usize> = if done { vec![1] } else { vec![0, 1] };
    let mut second_failure: Option<Value> = None;
    let deadline = started + wait_for(race);
    if let Some(inline) = spawn(1, second_cfg) {
        // no thread to be had: the other engine ran here
        let _ = tx.send((1, inline));
    }
    drop(tx);
    while !pending.is_empty() {
        let Ok((idx, out)) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) else { break };
        pending.retain(|p| *p != idx);
        if not_false(&out) {
            let mut out = clean(out);
            if idx == 1 {
                if let Some(o) = out.as_object_mut() {
                    o.insert("fallback".into(), json!(other));
                    let ms = o.get("ms").and_then(Value::as_f64).unwrap_or(0.0);
                    // the whole wait, both engines
                    o.insert("ms".into(), json!(py_round_int(hedge_at * 1000.0 + ms)));
                }
            }
            return out;
        }
        if idx == 0 {
            first_failure = Some(out);
        } else {
            second_failure = Some(out);
        }
    }
    let total = started + wait_for(budget);
    if let (Some(f), Some(s)) = (&first_failure, &second_failure) {
        return last_resort([f, s], started, total);
    }
    let timed_out = json!({"ok": false, "reason": "the search timed out", "transient": true});
    if use_monid {
        tracing::warn!("web search: no engine answered in {race:.0} s");
        return last_resort(
            [first_failure.as_ref().unwrap_or(&timed_out), second_failure.as_ref().unwrap_or(&timed_out)],
            started,
            total,
        );
    }
    if let Some(f) = first_failure {
        return clean(f);
    }
    tracing::warn!("web search: no engine answered in {budget:.0} s");
    json!({"ok": false, "reason": "the search timed out"})
}

/// `_engine_once`: engine_search's one attempt. A failure worth one more try on the other engine carries
/// `transient`; an HTTP failure carries `http`; a timed-out or unreachable one `failed_ms`.
pub fn engine_once(ctx: &Ctx, query: &str, config: &SearchConfig, opts: &EngineOpts) -> Value {
    let api_key = if opts.key.is_empty() { py_strip(&ctx.get("OPENROUTER_API_KEY")).to_string() } else { opts.key.clone() };
    if api_key.is_empty() {
        return json!({"ok": false, "reason": "web search is not set up on this computer (no OpenRouter key)"});
    }
    if py_strip(query).is_empty() {
        return json!({"ok": false, "reason": "empty query"});
    }
    let send: Arc<dyn Opener> = opts.opener.clone().unwrap_or_else(|| Arc::new(SystemOpener));
    let body = eve_jev::py_dumps(&request(ctx, config, query, None, &opts.system)).into_bytes();
    let req = OpenRequest {
        url: URL.to_string(),
        body,
        headers: vec![
            ("Authorization".into(), format!("Bearer {api_key}")),
            ("Content-Type".into(), "application/json".into()),
            ("HTTP-Referer".into(), "https://github.com/gurucharan/eve".into()),
            ("X-Title".into(), "eve".into()),
        ],
    };
    let clock = &opts.clock;
    let t0 = clock();
    let failed_ms = || (clock() - t0) * 1000.0;
    let parsed = match send.open(&req, config.timeout_secs) {
        Ok(raw) => match std::str::from_utf8(&raw) {
            Ok(text) => match serde_json::from_str::<Value>(text) {
                Ok(payload) => parse(&payload),
                Err(e) => Err(e.to_string()),
            },
            Err(e) => Err(format!("'utf-8' codec can't decode bytes: {e}")),
        },
        Err(OpenError::Http(code)) => {
            tracing::warn!("web search: HTTP {code} from OpenRouter");
            return json!({"ok": false, "reason": format!("the search service answered HTTP {code}"), "http": code,
                          "transient": code == 429 || code >= 500, "failed_ms": failed_ms()});
        }
        Err(OpenError::Url(reason)) => {
            tracing::warn!("web search: OpenRouter unreachable ({reason})");
            return json!({"ok": false, "reason": "the search service is unreachable", "transient": true,
                          "failed_ms": failed_ms()});
        }
        Err(OpenError::Timeout) => {
            tracing::warn!("web search: timed out after {:.0} s", config.timeout_secs);
            return json!({"ok": false, "reason": "the search timed out", "transient": true, "failed_ms": failed_ms()});
        }
    };
    let mut result = match parsed {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("web search: unreadable reply ({e})");
            return json!({"ok": false, "reason": "the search service sent an unreadable reply"});
        }
    };
    let ms = (clock() - t0) * 1000.0;
    let engine = openrouter_engine(&config.engine).unwrap_or("perplexity");
    let usage = result.get("usage").cloned().unwrap_or_else(|| json!({}));
    let measured = usage.get("cost").and_then(Value::as_f64);
    let cost_usd = py_round(measured.unwrap_or_else(|| search_usd(engine)), 4);
    let n_sources = result.get("sources").and_then(Value::as_array).map_or(0, Vec::len);
    if let Some(o) = result.as_object_mut() {
        o.insert("ms".into(), json!(py_round_int(ms)));
        o.insert("cost_usd".into(), json!(cost_usd));
    }
    // The daily meter takes OpenRouter's own figure only: without it the answer model's tokens are unknown, so
    // the line is unpriced rather than the search fee alone passed off as the whole cost.
    let note = if measured.is_some() {
        String::new()
    } else {
        format!("no usage.cost; search fee alone ~${}", eve_spend::py_float_repr(search_usd(engine)))
    };
    ctx.ledger.record(
        "openrouter",
        &config.model,
        &opts.feature,
        measured,
        RecordOpts::new()
            .tokens([
                ("in", usage.get("in").cloned().unwrap_or(Value::Null)),
                ("out", usage.get("out").cloned().unwrap_or(Value::Null)),
            ])
            .source(if measured.is_some() { "reported" } else { "priced" })
            .note(&note),
    );
    tracing::info!("web search: {n_sources} sources in {ms:.0} ms via {} on {engine}, ${cost_usd:.4}", config.model);
    result
}
