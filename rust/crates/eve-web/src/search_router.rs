//! Which provider answers a web search: a quick page reader, a many-source search engine, or live data services.
//!
//! Every web search eve makes goes through `answer` here: the brain's `web_search` tool (websearch::search, for
//! voice, Telegram and think) and the web reader body (web_reader::WebReaderAgent, for a public-web task). Three
//! providers, each good at a different job (2026-09-29):
//!
//! * **tinyfish** (tinyfish, web_reader::search_and_read + answer_from): free. Search returns titles, snippets and
//!   links; Fetch reads the top pages; a cheap model answers only from those pages. Good for one fact, opening
//!   hours, a definition, a price on a public page, what one page or doc says, a simple comparison from a few
//!   pages. Weak at many-source synthesis, breaking news and multi-hop research.
//! * **perplexity** (websearch::engine_search): OpenRouter's web_search server tool, about $0.005 a search plus the
//!   answer model. Good for synthesis across many sources, current events, multi-step research and anything
//!   ambiguous. The safe default.
//! * **firecrawl** (firecrawl): paid credits, with Alexandria's live data tools (Google Flights and Skyscanner
//!   fares, government records, ...). Worth it only when a data tool fits.
//!
//! Jev decides, asked the way browser_router asks it: the state is the request beside one line saying who is
//! asking; ALL THREE providers are the options of one Choice, on every Mac, whichever keys it holds (a pick whose
//! key is missing is masked in code to perplexity); beside the Choice, four absolute Nouls. Code combines them and
//! owns the rule: firecrawl only when its Choice probability AND the structured-data Noul clear their fitted gates;
//! tinyfish only when its probability AND the single-lookup Noul clear theirs AND both the changes-over-time Noul
//! and the many-sources Noul are under their maximums; everything else is perplexity. An error, a timeout, a
//! missing key, a provider that fails or finds nothing, today's Firecrawl cap spent: perplexity.
//!
//! Time (2026-09-29): a routed tinyfish or firecrawl attempt runs in a worker under an outer budget
//! (`SearchConfig.routed_secs`, 10 s for the brain; READER_ROUTED_SECS for the web reader); past it the router stops
//! waiting and perplexity answers with its own timeout. Cost: `cost_usd` is a float only when every part of the
//! answer was priced; otherwise None ("unpriced", as spend records it), never a partial figure.
//!
//! `SHIPPED` is true since the 2026-09-29 blind holdout passed its pre-registered bar (below GATES), so
//! `EVE_SEARCH_ROUTER=auto` routes whenever a second provider is usable. `off` leaves every search exactly as it
//! was: the brain on websearch's engine, the web reader on TinyFish.

use std::collections::HashSet;
use std::sync::{Arc, LazyLock};

use eve_decider::Predict;
use serde_json::{json, Value};

use crate::firecrawl::{Found, Ran, ToolCall};
use crate::watch::FetchError;
use crate::web_reader::Page;
use crate::websearch::{Opener, SearchConfig};
use crate::{Ask, Clock, Ctx};

pub const TINYFISH: &str = "tinyfish";
pub const PERPLEXITY: &str = "perplexity";
pub const FIRECRAWL: &str = "firecrawl";
pub const PROVIDERS: [&str; 3] = [PERPLEXITY, TINYFISH, FIRECRAWL];
pub const MODES: [&str; 3] = ["auto", "on", "off"];
/// a search is seconds; a slow router is a perplexity search, not a wait
pub const JEV_TIMEOUT_S: f64 = 2.0;
/// The web reader's routed attempt, at least: its tasks may take a Firecrawl search (30 s), a data tool (45 s) and
/// two model calls; past it the engine search, then Codex. The brain's is SearchConfig.routed_secs (10 s).
pub const READER_ROUTED_SECS: f64 = 90.0;

/// The three providers, as the Choice's options. Facts only, from each provider's own docs and this repo's use of
/// it; the keys and the words name the job, never the brand.
pub static OPTIONS: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "quick_page_read": {
            "what": "a quick reader that runs one web search, reads the top few result pages, and answers only from \
                     what those few pages say",
            "for": ["one settled fact, number, date or definition that stays the same over time",
                    "opening hours, a phone number, an address, or a price shown on a public web page",
                    "what one specific public page, documentation page, policy or article says",
                    "a simple comparison of two or three things whose facts sit on a few pages"],
            "not_for": ["questions that need many sources weighed together: reviews, recommendations, 'best' lists",
                        "anything whose correct answer changes over time or depends on recent news: who holds a role \
                         now, the latest result or release, today's events",
                        "research that takes several searches one after another", "vague or open-ended questions",
                        "live fares, bookable prices or official records held by a data service"],
        },
        "deep_web_research": {
            "what": "a search engine that searches the web several times, reads many sources, and writes one answer \
                     that brings them together, with citations",
            "for": ["questions that need many sources weighed together: reviews, recommendations, pros and cons",
                    "anything whose correct answer changes over time: who holds a role now, the latest result or \
                     release, current events, news, and anything that happened today or this week",
                    "research that takes several steps or searches", "vague, broad or ambiguous questions",
                    "anything that fits none of the other options"],
            "not_for": ["one settled fact, definition, opening hours or page statement that one or two ordinary \
                         public pages give and that stays the same over time",
                        "live flight fares or bookable prices that a travel or fare-comparison service holds",
                        "a lookup in an official register or government records database"],
        },
        "live_data_service": {
            "what": "live data services queried directly with structured fields, such as flight search, fare \
                     comparison and government or official records lookups, answering from their own data rather \
                     than from web pages",
            "for": ["live flight fares or schedules, or the cheapest flight between two places on a date",
                    "hotel, travel or ticket prices for given dates from a booking or fare-comparison service",
                    "official records: company registrations, government registers, licences, public filings"],
            "not_for": ["facts, definitions, hours, news or summaries that ordinary web pages state",
                        "a request with no route, dates, name or identifier for a service to look up"],
        },
    })
});
pub const OPTION_TO_PROVIDER: [(&str, &str); 3] =
    [("quick_page_read", TINYFISH), ("deep_web_research", PERPLEXITY), ("live_data_service", FIRECRAWL)];

/// `PROVIDER_TO_OPTION[provider]`.
pub fn provider_to_option(provider: &str) -> Option<&'static str> {
    OPTION_TO_PROVIDER.iter().find(|(_, p)| *p == provider).map(|(o, _)| *o)
}

/// Who is asking, in one line: the text is sometimes a terse query the brain wrote, sometimes the owner's own
/// words.
pub const ABOUT: &str = "eve, the owner's desk-robot assistant, is about to search the web. The request is either a search query \
eve wrote while talking with the owner, or the owner's own words; judge what answering it needs.";

/// The Choice over ALL THREE providers and the four Nouls: the same question on every Mac, whichever keys it
/// holds (a pick that is not available is masked in make_router, never hidden from Jev), so fitted gates hold.
pub fn questions() -> Value {
    todo!("search_router.questions")
}

/// Jev's answers to `questions()`.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchAnswer {
    /// the Choice's probability for each provider's option
    pub p_tinyfish: f64,
    pub p_perplexity: f64,
    pub p_firecrawl: f64,
    /// needs_live_structured_data
    pub structured: f64,
    /// needs_many_sources_or_searches
    pub many: f64,
    /// single_lookup_or_page_read
    pub single: f64,
    /// answer_changes_over_time (default 0.0)
    pub changes: f64,
    pub ms: f64,
    pub error: String,
}

/// The fitted cut-offs. `Default` is Python's `SearchGates()`: nothing routed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SearchGates {
    /// the Choice's probability for firecrawl, at least
    pub firecrawl: f64,
    /// the structured-data Noul, at least
    pub structured: f64,
    /// the Choice's probability for tinyfish, at least
    pub tinyfish: f64,
    /// the single-lookup Noul, at least
    pub single: f64,
    /// the many-sources Noul, at most, for tinyfish
    pub many_max: f64,
    /// the changes-over-time Noul, at most, for tinyfish
    pub changes_max: f64,
}

impl Default for SearchGates {
    fn default() -> Self {
        SearchGates { firecrawl: 1.01, structured: 1.01, tinyfish: 1.01, single: 1.01, many_max: 0.0, changes_max: 0.0 }
    }
}

/// One routing decision.
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub provider: String,
    /// the Choice's probability for the provider chosen
    pub p: f64,
    /// Jev's time
    pub ms: f64,
    pub error: String,
}

impl Route {
    /// `Route(provider)`.
    pub fn new(provider: &str) -> Self {
        Route { provider: provider.to_string(), p: 0.0, ms: 0.0, error: String::new() }
    }
}

/// `route(text, available) -> Route`. An Err is a router that raised (`answer` treats it as perplexity, logging
/// the error's class name, [`RouteError::kind`]).
pub type RouteFn = Arc<dyn Fn(&str, &[String]) -> Result<Route, RouteError> + Send + Sync>;

/// A router that raised: Python's exception, by class name and message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{kind}: {message}")]
pub struct RouteError {
    pub kind: String,
    pub message: String,
}

/// Jev's answers to questions(), always the full three-option question. A predict that fails abstains:
/// perplexity, the safe default, with `error` `"{kind}: {message}"` (at most 160 characters).
pub fn ask(predict: &Predict, text: &str, clock: &Clock) -> SearchAnswer {
    let _ = (predict, text, clock);
    todo!("search_router.ask")
}

/// FIRECRAWL or TINYFISH only when their conditions hold; anything else, including an error, is PERPLEXITY.
pub fn decide(a: &SearchAnswer, g: &SearchGates) -> &'static str {
    let _ = (a, g);
    todo!("search_router.decide")
}

/// A route that costs: a request that is not a quick read sent to the reader, or anything that does not need a
/// data service sent to the paid one. Sending anything to perplexity is never wrong, only slower or dearer.
pub fn wrong(said: &str, truth: &str) -> bool {
    let _ = (said, truth);
    todo!("search_router.wrong")
}

pub const GRID: [f64; 6] = [0.5, 0.6, 0.7, 0.8, 0.9, 0.95];
pub const MAX_GRID: [f64; 5] = [0.05, 0.1, 0.2, 0.3, 0.5];

/// The cut-offs that route the most requests away from perplexity, each to its labelled provider, with ZERO
/// wrong routes. Ties go to the stricter setting. Firecrawl is fitted first and tinyfish under it. Panics where
/// Python's `zip(strict=True)` raises (answers and truths of different lengths).
pub fn fit(answers: &[SearchAnswer], truths: &[&str]) -> SearchGates {
    let _ = (answers, truths);
    todo!("search_router.fit")
}

// History (2026-09-29): a first fit on search_tuning.json alone, scored once on the blind search_holdout.json
// (80 requests), routed 43 away from perplexity with one wrong route: "who's the CEO of OpenAI" (labelled
// perplexity: the answer changes) sent to tinyfish at p 0.99, single 0.94, many 0.07. That miss split the old
// many-sources-or-news Noul in two (answer_changes_over_time, needs_many_sources_or_searches), put the quick-read
// boundary into deep_web_research.not_for too, and fixed the question to all three options on every Mac. That
// holdout is seen data now: the next fit is on search_tuning.json + search_holdout.json and scores ONCE on the
// blind search_holdout2.json.
//
// Refit and scored 2026-09-29 (tools/route_eval.py --search, live Jev on EVE_JEV_ROUTE, run once): fitted
// with zero wrong routes on the 147 seen requests (search_tuning.json 67 + search_holdout.json 80), then scored
// ONCE on search_holdout2.json, written blind AFTER the wording change above and never read before this run.
// n=90 (labelled tinyfish 32, firecrawl 18, perplexity 40). Routed to tinyfish 22, right 21 (precision 95.5%);
// to firecrawl 15, right 15; to perplexity 53, right 39. Away from perplexity 37. Coverage: 36 of the 50
// non-perplexity requests went to their labelled provider (21/32 tinyfish, 15/18 firecrawl); 75 of 90 right
// overall. Jev latency p50 189 ms, p95 313 ms, 0 errors. The one wrong route, verbatim: "what's the tallest
// building in the world": said tinyfish, truth perplexity (tinyfish 0.97 firecrawl 0.00 structured 0.04 changes
// 0.37 many 0.06 single 0.92). Bar: (a) 37 >= 20 pass, (b) 0 wrong to firecrawl pass, (c) 95.5% >= 95% pass:
// SHIPPED. Most misses are page reads (prices, hours on a named site) whose changes-over-time Noul sits above 0.5;
// they fall to perplexity, which is slower, never wrong. Nothing was tuned after seeing it. Caveat found in review:
// holdout2 repeats two seen requests word for word ("what time does the trader joe's on masonic open tomorrow", a
// miss; "how many ounces in a cup", a right tinyfish route) and nearly repeats the CEO case. Without the two exact
// repeats (c) is 20 of 21, 95.2%: still a pass, by one route. A new blind set should be written before any refit.
pub const GATES: SearchGates =
    SearchGates { firecrawl: 0.95, structured: 0.7, tinyfish: 0.8, single: 0.6, many_max: 0.1, changes_max: 0.5 };

// PRE-REGISTERED BAR (written 2026-09-29 before any scoring of the fresh holdout): on the fresh blind holdout,
// (a) at least 20 requests routed away from perplexity, (b) zero wrong routes to firecrawl, (c) precision on
// routes to tinyfish >= 95%. Pass all three -> SHIPPED=True. Otherwise SHIPPED=False. Scored exactly once; nothing
// tuned after seeing it. tools/route_eval.py --search prints this verdict. EVE_SEARCH_ROUTER=auto routes only
// when True. Scored 2026-09-29 on search_holdout2.json (n=90): (a) 37 routed, (b) 0 wrong to firecrawl, (c) 21 of 22
// to tinyfish right, 95.5%. All three pass.
pub const SHIPPED: bool = true;

/// `route(text, available) -> Route`: Jev's answer to the full question under the gates (GATES when none are
/// given), then masked: a pick that is not `available` is perplexity (the question itself never changes with the
/// keys, so the fitted gates hold on every Mac).
pub fn make_router(predict: Arc<Predict>, clock: Clock, gates: Option<SearchGates>) -> RouteFn {
    let _ = (predict, clock, gates);
    todo!("search_router.make_router")
}

/// Jev over EVE_JEV_ROUTE (2 s timeout), made once a process (remade when the route's url or model changes), or
/// None (no route: every search stays on perplexity).
pub fn jev_router(ctx: &Ctx) -> Option<RouteFn> {
    let _ = ctx;
    todo!("search_router.jev_router")
}

/// EVE_SEARCH_ROUTER = auto (the default: on when SHIPPED) | on (on before the fit, for a live trial) | off
/// (websearch's engine and the TinyFish reader, exactly as before the router). 1/true/yes are on, 0/false/no off.
pub fn mode(ctx: &Ctx) -> String {
    let _ = ctx;
    todo!("search_router.mode")
}

/// The providers routing may use, perplexity first, or empty when routing is off. On only when the mode allows it,
/// a Jev route is configured, and perplexity runs through OpenRouter (`engine` is websearch's; every provider's
/// answer model is on OpenRouter too, so without it nothing here can answer); then TinyFish with
/// TINYFISH_API_KEY and Firecrawl with FIRECRAWL_API_KEY. Perplexity alone is not routing: empty.
pub fn available(ctx: &Ctx, engine: &str) -> Vec<String> {
    available_shipped(ctx, engine, SHIPPED)
}

/// `available` with SHIPPED as a parameter (Python's tests patch the module constant).
pub fn available_shipped(ctx: &Ctx, engine: &str, shipped: bool) -> Vec<String> {
    let _ = (ctx, engine, shipped);
    todo!("search_router.available")
}

// ---- the one routed entry point -----------------------------------------------------------------------------

/// `(query, system, feature) -> result` (websearch's result shape).
pub type PerplexityFn = Arc<dyn Fn(&str, &str, &str) -> Value + Send + Sync>;

/// Each provider's calls, as seams for the tests; None is the real call, resolved when used.
#[derive(Clone, Default)]
pub struct Paths {
    /// (query, system, feature) -> result
    pub perplexity: Option<PerplexityFn>,
    /// web_reader::search_and_read
    pub read: Option<Arc<dyn Fn(&str) -> Result<Vec<Page>, FetchError> + Send + Sync>>,
    /// firecrawl::search, with tools
    pub firecrawl_search: Option<Arc<dyn Fn(&str) -> Result<Found, FetchError> + Send + Sync>>,
    /// firecrawl::run_tool
    pub run_tool: Option<Arc<dyn Fn(&ToolCall) -> Result<Ran, FetchError> + Send + Sync>>,
    /// watch::openrouter
    pub ask: Option<Ask>,
}

pub const NOT_FOUND: &str = "NOT FOUND";
/// The web reader needs to know when a many-source search did not answer the task, so Codex can take it.
pub const READER_SYSTEM: &str = "You are the answer step of eve's web search: eve, the owner's desk-robot assistant, searched \
the web for the owner's request. Using only the web results provided, answer the request in at \
most four plain sentences with the concrete facts (numbers, names, dates) and say which source \
each comes from by its title. If the results do not answer it, reply with exactly: \
NOT FOUND. No markdown, no lists.";
/// `_LINK`.
pub const LINK: &str = r"(?i)(?:https?://|www\.)\S+";

/// A link in an answer's own words survives only when eve read it (a page it fetched, a source the search cited,
/// a link a data tool returned); any other becomes "(link removed)" (WatchLink.lean, the watcher's lesson).
/// Whitespace is collapsed.
pub fn keep_read_links(text: &str, read: &HashSet<String>) -> String {
    let _ = (text, read);
    todo!("search_router.keep_read_links")
}

/// `_usage`: the answer model's usage over its calls, websearch::parse's shape; `cost` only when every call
/// reported one.
pub fn usage(payloads: &[Value]) -> Value {
    let _ = payloads;
    todo!("search_router._usage")
}

/// `_result`: websearch's result shape from a page-reading provider: the pages the answer used become its
/// sources. `cost_usd` is the answer model's reported cost plus `extra_usd` (a Firecrawl route's credits, default
/// 0.0), or None when either is unknown.
pub fn result(answer: &str, pages: &[Page], used: &[String], payloads: &[Value], extra_usd: Option<f64>) -> Value {
    let _ = (answer, pages, used, payloads, extra_usd);
    todo!("search_router._result")
}

/// The keyword arguments `via` and `answer` share. `Default` is Python's defaults: `reader=False`, `Paths()`,
/// `feature=spend.SEARCH`, never cancelled, progress and tried ignored, `key=""`, `opener=None`,
/// `clock=time.perf_counter`, `route=None` (jev_router), `shipped` SHIPPED.
#[derive(Clone)]
pub struct AnswerOpts {
    pub reader: bool,
    pub paths: Paths,
    pub route: Option<RouteFn>,
    pub feature: String,
    pub cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
    pub progress: Arc<dyn Fn(&str) + Send + Sync>,
    pub tried: Arc<dyn Fn(&str) + Send + Sync>,
    pub key: String,
    pub opener: Option<Arc<dyn Opener>>,
    pub clock: Clock,
}

impl Default for AnswerOpts {
    fn default() -> Self {
        AnswerOpts {
            reader: false,
            paths: Paths::default(),
            route: None,
            feature: eve_spend::SEARCH.to_string(),
            cancelled: Arc::new(|| false),
            progress: Arc::new(|_| {}),
            tried: Arc::new(|_| {}),
            key: String::new(),
            opener: None,
            clock: crate::perf_clock(),
        }
    }
}

/// One provider, no fallback: websearch's result shape, or {"ok": False, "reason"}. Errs only with what the
/// provider's own code raises past FetchError (`answer` catches everything), as the exception's class name.
pub fn via(ctx: &Ctx, provider: &str, query: &str, config: &SearchConfig, opts: &AnswerOpts) -> Result<Value, String> {
    let _ = (ctx, provider, query, config, opts);
    todo!("search_router.via")
}

/// `_firecrawl`: Firecrawl search with Alexandria's tools; the model answers from the pages or picks ONE tool,
/// which eve runs and the model answers from. A tool that fails (or needs terms eve never accepts) leaves the
/// pages.
pub fn via_firecrawl(ctx: &Ctx, query: &str, ask: &Ask, opts: &AnswerOpts) -> Result<Value, String> {
    let _ = (ctx, query, ask, opts);
    todo!("search_router._firecrawl")
}

/// `_credits`: a reply's `credits` as a float, None when it is not a number.
pub fn credits(reply: Option<&serde_json::Number>) -> Option<f64> {
    let _ = reply;
    todo!("search_router._credits")
}

/// `work(abandoned)` in a worker thread (carrying the caller's spend job, eve_spend::carry), waited on for
/// `budget` seconds at most: its result, or {"ok": False, "reason": "no answer within {budget:g} s"} once the
/// budget is spent; `abandoned()` then turns true so the worker stops at its next step, and its result is
/// dropped. A worker that panics or errs is {"ok": False, "reason": its class name}.
pub fn bounded(
    work: Box<dyn FnOnce(Arc<dyn Fn() -> bool + Send + Sync>) -> Result<Value, String> + Send>,
    budget: f64,
) -> Value {
    let _ = (work, budget);
    todo!("search_router.bounded")
}

/// THE routed search, for both surfaces: websearch's result shape ({"ok", "answer", "sources", "usage", "ms",
/// "cost_usd"}, cost_usd None when unpriced) or {"ok": False, "reason"}. Never fails. `config.providers` empty
/// (routing off): the brain gets websearch's engine and the web reader (`reader`) gets TinyFish, each exactly as
/// before the router. Otherwise Jev picks among `config.providers`, and a routed provider that fails, finds
/// nothing, runs out of its budget or would pass today's Firecrawl cap is followed by one perplexity search.
/// Logs one line per routed search: the provider, the Choice's probability and Jev's milliseconds, never the query.
pub fn answer(ctx: &Ctx, query: &str, config: &SearchConfig, opts: &AnswerOpts) -> Value {
    let _ = (ctx, query, config, opts);
    todo!("search_router.answer")
}
