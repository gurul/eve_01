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

use std::sync::{Arc, LazyLock};

use chrono::{DateTime, FixedOffset};
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
        let _ = (req, timeout);
        todo!("websearch: urllib.request.urlopen")
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

/// EVE_WEB_SEARCH = openrouter-perplexity | openrouter-exa | openai | off. Unset: openrouter-perplexity when
/// OPENROUTER_API_KEY is set, else off. EVE_WEB_SEARCH_MODEL, _RESULTS (1-10) and _MAX_USES (1-5) tune the call;
/// any OpenRouter model id answers, openai/* included. `providers` is search_router::available: empty unless
/// EVE_SEARCH_ROUTER routes on this engine. EVE_SEARCH_ROUTED_SECS (1-60, default 10) is a routed attempt's
/// outer budget.
pub fn configured(ctx: &Ctx) -> SearchConfig {
    let _ = ctx;
    todo!("websearch.configured")
}

/// The tool(s) a brain offers for the web: the function tool (routed when there are providers), the hosted one,
/// or nothing.
pub fn tools_for(config: &SearchConfig) -> Vec<Value> {
    let _ = config;
    todo!("websearch.tools_for")
}

/// WEB_SEARCH_TOOL with its description telling the brain what the routed search reaches.
pub fn routed_tool(providers: &[String]) -> Value {
    let _ = providers;
    todo!("websearch.routed_tool")
}

/// `_clock(environ, now)`: CLOCK filled with the local time ("%A %Y-%m-%d %H:%M") in EVE_TIMEZONE (when it names a
/// zone; else the machine's), and the zone's name (the key, else the local tzname).
pub fn clock(ctx: &Ctx, now: Option<DateTime<FixedOffset>>) -> String {
    let _ = (ctx, now);
    todo!("websearch._clock")
}

/// Reasoning models need room for both thinking and the short answer; keep older override budgets.
pub fn answer_options(model: &str, max_tokens: i64) -> Map<String, Value> {
    let _ = (model, max_tokens);
    todo!("websearch.answer_options")
}

/// The exact OpenRouter body: the cheap answer model, the web search server tool on the engine with its budget,
/// the clock and the answer-box rules (`system`, default SYSTEM) as the system turn, the query as the user turn.
pub fn request(ctx: &Ctx, config: &SearchConfig, query: &str, now: Option<DateTime<FixedOffset>>, system: &str) -> Value {
    let _ = (ctx, config, query, now, system);
    todo!("websearch.request")
}

/// {"ok", "answer", "sources": [{"title", "url", "snippet"}], "usage"} from a chat completion. A reply with no
/// annotations is still an answer; a reply with no text is not. Err is Python's ValueError, with its message.
pub fn parse(payload: &Value) -> Result<Value, String> {
    let _ = payload;
    todo!("websearch.parse")
}

/// The brain's web search, blocking (callers run it off the loop). Never fails: {"ok": False, "reason": …}
/// instead. With `config.providers` it is search_router::answer (Jev picks the provider; the same result shape);
/// without, engine_search. `config` None is `configured(ctx)`. Logs the shape and the time, never the query.
pub fn search(ctx: &Ctx, query: &str, config: Option<&SearchConfig>, opts: &EngineOpts) -> Value {
    let _ = (ctx, query, config, opts);
    todo!("websearch.search")
}

/// One search on the engine (Perplexity through OpenRouter by default), the search before the router and the
/// router's safe default. Hedged across the two OpenRouter engines (HEDGE_SECS), the whole search waiting at most
/// `config.timeout_secs`; when BOTH engines failed (and neither with a 4xx other than 429), Monid's Google results
/// get one try with what is left of the budget and answer with `fallback` "monid". Never fails.
pub fn engine_search(ctx: &Ctx, query: &str, config: Option<&SearchConfig>, opts: &EngineOpts) -> Value {
    let _ = (ctx, query, config, opts);
    todo!("websearch.engine_search")
}

/// `_engine_once`: engine_search's one attempt. A failure worth one more try on the other engine carries
/// `transient`; an HTTP failure carries `http`; a timed-out or unreachable one `failed_ms`.
pub fn engine_once(ctx: &Ctx, query: &str, config: &SearchConfig, opts: &EngineOpts) -> Value {
    let _ = (ctx, query, config, opts);
    todo!("websearch._engine_once")
}
