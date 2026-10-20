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

use std::collections::{HashMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, LazyLock};
use std::time::Duration;

use eve_decider::Predict;
use eve_spend::RecordOpts;
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Map, Value};

use crate::firecrawl;
use crate::watch::FetchError;
use crate::web_reader::{self, FirecrawlSearchFn, Page, ReadFn, ToolResult, ToolRunFn};
use crate::websearch::{self, py_g, py_round, py_round_int, py_strip, py_str_value, take_chars, truthy, words, EngineOpts, Opener, SearchConfig};
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
    let mut criteria = Map::new();
    for p in PROVIDERS {
        let option = provider_to_option(p).unwrap_or_default();
        criteria.insert(option.into(), OPTIONS.get(option).cloned().unwrap_or(Value::Null));
    }
    json!({
        "provider": {"type": "choice", "instructions": "Which of these should answer the request?",
                     "criteria": Value::Object(criteria)},
        "needs_live_structured_data": {"type": "noul", "instructions":
            "Does answering need live data that a booking, fare-comparison or official records service holds and \
             would be asked with specific fields: flight or train fares or schedules between two places, hotel or \
             ticket prices for given dates, or a lookup in a government or official register? Answer no for \
             facts, prices, hours or news that an ordinary web page states."},
        "answer_changes_over_time": {"type": "noul", "instructions":
            "Does the correct answer change over time, or depend on news or events of the last few weeks, so that \
             a web page written some months ago could now give a wrong answer? Yes for who holds a job or title \
             now, the latest version, result or standing, and current events. No for a settled fact that stays \
             the same, such as a definition, a date in history, or what a named document says."},
        "needs_many_sources_or_searches": {"type": "noul", "instructions":
            "Does answering need many sources weighed together (reviews, recommendations, pros and cons, 'best' \
             lists), or several searches one after another, each depending on what the last one found?"},
        "single_lookup_or_page_read": {"type": "noul", "instructions":
            "Can the whole answer come from one or two ordinary public web pages: a single fact, number, date, \
             definition, opening hours, phone number, a price shown on a page, or what one specific named page or \
             document says?"},
    })
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
    let t0 = clock();
    let state = json!({"about": ABOUT, "request": take_chars(&words(text), 500)});
    let failed = |error: String| SearchAnswer {
        p_tinyfish: 0.0,
        p_perplexity: 1.0,
        p_firecrawl: 0.0,
        structured: 0.0,
        many: 1.0,
        single: 0.0,
        changes: 1.0,
        ms: (clock() - t0) * 1000.0,
        error: take_chars(&error, 160),
    };
    // a model that fails abstains: perplexity, the safe default
    let result = match predict(&state, &questions()) {
        Ok(r) => r,
        Err(e) => return failed(format!("{}: {}", e.kind, e.message)),
    };
    // `result.get("answers") or {}`: a reply that is not a dict has no .get
    let answers = match &result {
        Value::Object(m) => m.get("answers").filter(|v| truthy(v)).and_then(Value::as_object).cloned().unwrap_or_default(),
        other => {
            return failed(format!("AttributeError: '{}' object has no attribute 'get'", websearch::py_type_name(other)))
        }
    };
    let number = |v: Option<&Value>| -> f64 {
        match v {
            Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
            _ => 0.0,
        }
    };
    let noul = |key: &str| number(answers.get(key).and_then(Value::as_object).and_then(|a| a.get("noul")));
    let probs = answers
        .get("provider")
        .and_then(Value::as_object)
        .and_then(|a| a.get("probabilities"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let p = |provider: &str| number(provider_to_option(provider).and_then(|o| probs.get(o)));
    SearchAnswer {
        p_tinyfish: p(TINYFISH),
        p_perplexity: p(PERPLEXITY),
        p_firecrawl: p(FIRECRAWL),
        structured: noul("needs_live_structured_data"),
        many: noul("needs_many_sources_or_searches"),
        single: noul("single_lookup_or_page_read"),
        changes: noul("answer_changes_over_time"),
        ms: (clock() - t0) * 1000.0,
        error: String::new(),
    }
}

/// FIRECRAWL or TINYFISH only when their conditions hold; anything else, including an error, is PERPLEXITY.
pub fn decide(a: &SearchAnswer, g: &SearchGates) -> &'static str {
    if !a.error.is_empty() {
        return PERPLEXITY;
    }
    if a.p_firecrawl >= g.firecrawl && a.structured >= g.structured {
        return FIRECRAWL;
    }
    if a.p_tinyfish >= g.tinyfish && a.single >= g.single && a.many <= g.many_max && a.changes <= g.changes_max {
        return TINYFISH;
    }
    PERPLEXITY
}

/// A route that costs: a request that is not a quick read sent to the reader, or anything that does not need a
/// data service sent to the paid one. Sending anything to perplexity is never wrong, only slower or dearer.
pub fn wrong(said: &str, truth: &str) -> bool {
    (said == TINYFISH && truth != TINYFISH) || (said == FIRECRAWL && truth != FIRECRAWL)
}

pub const GRID: [f64; 6] = [0.5, 0.6, 0.7, 0.8, 0.9, 0.95];
pub const MAX_GRID: [f64; 5] = [0.05, 0.1, 0.2, 0.3, 0.5];

/// The cut-offs that route the most requests away from perplexity, each to its labelled provider, with ZERO
/// wrong routes. Ties go to the stricter setting. Firecrawl is fitted first and tinyfish under it. Panics where
/// Python's `zip(strict=True)` raises (answers and truths of different lengths).
pub fn fit(answers: &[SearchAnswer], truths: &[&str]) -> SearchGates {
    assert_eq!(answers.len(), truths.len(), "zip() arguments have different lengths");
    let routes = |g: &SearchGates| -> Option<Vec<&'static str>> {
        let said: Vec<&'static str> = answers.iter().map(|a| decide(a, g)).collect();
        if said.iter().zip(truths).any(|(s, t)| wrong(s, t)) {
            None
        } else {
            Some(said)
        }
    };
    // tuples compare left to right: the most right routes first, then the stricter setting
    let better = |key: &[f64], best: &Option<Vec<f64>>| match best {
        None => true,
        Some(b) => key.partial_cmp(b.as_slice()) == Some(std::cmp::Ordering::Greater),
    };
    let mut fc_best: Option<Vec<f64>> = None;
    let mut fc = (1.01, 1.01);
    for p_fc in GRID {
        for structured in GRID {
            let g = SearchGates { firecrawl: p_fc, structured, ..SearchGates::default() };
            let Some(said) = routes(&g) else { continue };
            let right = said.iter().zip(truths).filter(|(s, t)| **s == **t && **s == FIRECRAWL).count() as f64;
            let key = vec![right, p_fc, structured];
            if better(&key, &fc_best) {
                fc_best = Some(key);
                fc = (p_fc, structured);
            }
        }
    }
    let mut best: Option<Vec<f64>> = None;
    let mut chosen = SearchGates { firecrawl: fc.0, structured: fc.1, ..SearchGates::default() };
    for p_tf in GRID {
        for single in GRID {
            for many in MAX_GRID {
                for changes in MAX_GRID {
                    let g = SearchGates {
                        firecrawl: fc.0,
                        structured: fc.1,
                        tinyfish: p_tf,
                        single,
                        many_max: many,
                        changes_max: changes,
                    };
                    let Some(said) = routes(&g) else { continue };
                    let right =
                        said.iter().zip(truths).filter(|(s, t)| **s == **t && **s != PERPLEXITY).count() as f64;
                    let key = vec![right, p_tf, single, -many, -changes];
                    if better(&key, &best) {
                        best = Some(key);
                        chosen = g;
                    }
                }
            }
        }
    }
    chosen
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
    Arc::new(move |text: &str, available: &[String]| -> Result<Route, RouteError> {
        let a = ask(&*predict, text, &clock);
        let mut said = decide(&a, &gates.unwrap_or(GATES));
        let mut error = a.error.clone();
        if !available.iter().any(|p| p == said) {
            if error.is_empty() {
                error = format!("{said} is not available");
            }
            said = PERPLEXITY;
        }
        let p = match said {
            TINYFISH => a.p_tinyfish,
            FIRECRAWL => a.p_firecrawl,
            _ => a.p_perplexity,
        };
        Ok(Route { provider: said.to_string(), p, ms: a.ms, error })
    })
}

/// `_JEV`: the router made once a process, with the route (url, model) it was made for.
type JevCache = Option<((String, String), RouteFn)>;
static JEV: LazyLock<Mutex<JevCache>> = LazyLock::new(|| Mutex::new(None));

/// Jev over EVE_JEV_ROUTE (2 s timeout), made once a process (remade when the route's url or model changes), or
/// None (no route: every search stays on perplexity).
pub fn jev_router(ctx: &Ctx) -> Option<RouteFn> {
    let (url, key, model) = eve_jev::route_config(ctx, "").ok()?;
    let mut cached = JEV.lock();
    if let Some((route, router)) = cached.as_ref() {
        if route.0 == url && route.1 == model {
            return Some(router.clone());
        }
    }
    let predict = eve_jev::make_predict(
        &url,
        &key,
        &model,
        eve_jev::PredictOptions {
            timeout_s: JEV_TIMEOUT_S,
            ledger: Some(ctx.ledger.clone()),
            ..eve_jev::PredictOptions::default()
        },
    );
    let router = make_router(Arc::new(predict.decider_fn()), crate::perf_clock(), None);
    *cached = Some(((url, model), router.clone()));
    Some(router)
}

/// EVE_SEARCH_ROUTER = auto (the default: on when SHIPPED) | on (on before the fit, for a live trial) | off
/// (websearch's engine and the TinyFish reader, exactly as before the router). 1/true/yes are on, 0/false/no off.
pub fn mode(ctx: &Ctx) -> String {
    let given = ctx.get("EVE_SEARCH_ROUTER");
    let mut raw = py_strip(if given.is_empty() { "auto" } else { &given }).to_lowercase();
    if ["1", "true", "yes"].contains(&raw.as_str()) {
        raw = "on".into();
    }
    if ["0", "false", "no"].contains(&raw.as_str()) {
        raw = "off".into();
    }
    if !MODES.contains(&raw.as_str()) {
        tracing::warn!(
            "search router: EVE_SEARCH_ROUTER={} is not one of ('auto', 'on', 'off'); auto",
            eve_decider::py_str_repr(&raw)
        );
        raw = "auto".into();
    }
    raw
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
    let m = mode(ctx);
    if m == "off" || (m == "auto" && !shipped) {
        return Vec::new();
    }
    if websearch::openrouter_engine(engine).is_none() || py_strip(&ctx.get("OPENROUTER_API_KEY")).is_empty() {
        return Vec::new();
    }
    if eve_jev::route_config(ctx, "").is_err() {
        return Vec::new();
    }
    let mut found = vec![PERPLEXITY.to_string()];
    if !crate::tinyfish::api_key(ctx).is_empty() {
        found.push(TINYFISH.to_string());
    }
    if !firecrawl::api_key(ctx).is_empty() {
        found.push(FIRECRAWL.to_string());
    }
    if found.len() > 1 {
        found
    } else {
        Vec::new()
    }
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
    pub read: Option<ReadFn>,
    /// firecrawl::search, with tools
    pub firecrawl_search: Option<FirecrawlSearchFn>,
    /// firecrawl::run_tool
    pub run_tool: Option<ToolRunFn>,
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
    // LINK, with `\S` spelled as Python's: not str.isspace(), which also counts the separators \x1c-\x1f
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&LINK.replace(r"\S+", r"[^\s\x1c-\x1f]+")).expect("the link pattern compiles")
    });
    let kept = RE.replace_all(text, |c: &regex::Captures<'_>| {
        let link = &c[0];
        if read.contains(link.trim_end_matches(['.', ',', ';', ':', '!', '?'])) {
            link.to_string()
        } else {
            "(link removed)".to_string()
        }
    });
    words(&kept)
}

/// `_usage`: the answer model's usage over its calls, websearch::parse's shape; `cost` only when every call
/// reported one.
pub fn usage(payloads: &[Value]) -> Value {
    let (mut tin, mut tout) = (0i64, 0i64);
    let mut costs: Vec<f64> = Vec::new();
    for p in payloads {
        let Some(u) = p.as_object().and_then(|o| o.get("usage")).and_then(Value::as_object) else { continue };
        tin = tin.saturating_add(websearch::py_int_or_zero(u.get("prompt_tokens")).unwrap_or(0));
        tout = tout.saturating_add(websearch::py_int_or_zero(u.get("completion_tokens")).unwrap_or(0));
        // a JSON bool is its own variant, so `not isinstance(cost, bool)` holds by construction
        if let Some(Value::Number(n)) = u.get("cost") {
            costs.push(n.as_f64().unwrap_or(0.0));
        }
    }
    let mut out = Map::new();
    out.insert("in".into(), json!(tin));
    out.insert("out".into(), json!(tout));
    if !payloads.is_empty() && costs.len() == payloads.len() {
        out.insert("cost".into(), json!(py_round(costs.iter().sum::<f64>(), 6)));
    }
    Value::Object(out)
}

/// `_result`: websearch's result shape from a page-reading provider: the pages the answer used become its
/// sources. `cost_usd` is the answer model's reported cost plus `extra_usd` (a Firecrawl route's credits, default
/// 0.0), or None when either is unknown.
pub fn result(answer: &str, pages: &[Page], used: &[String], payloads: &[Value], extra_usd: Option<f64>) -> Value {
    // `{p["url"]: p for p in pages}`: a later page with the same link wins
    let by_url: HashMap<&str, &Page> = pages.iter().map(|p| (p.url.as_str(), p)).collect();
    let sources: Vec<Value> = used
        .iter()
        .filter_map(|u| by_url.get(u.as_str()).map(|p| (u, p)))
        .map(|(u, p)| {
            json!({"title": take_chars(&words(&p.title), 120), "url": take_chars(u, 500),
                   "snippet": take_chars(&words(&p.markdown), websearch::MAX_SNIPPET_CHARS)})
        })
        .take(websearch::MAX_SOURCES)
        .collect();
    let usage = usage(payloads);
    let cost = usage.get("cost").and_then(Value::as_f64);
    let cost_usd = match (cost, extra_usd) {
        (Some(c), Some(x)) => json!(py_round(c + x, 4)),
        _ => Value::Null,
    };
    json!({"ok": true, "answer": take_chars(answer, websearch::MAX_ANSWER_CHARS), "sources": sources,
           "usage": usage, "cost_usd": cost_usd})
}

/// `{"ok": False, "reason": reason}`.
fn failure(reason: impl Into<String>) -> Value {
    json!({"ok": false, "reason": reason.into()})
}

/// `out.get(name)` of a result object.
fn field<'a>(out: &'a Value, name: &str) -> Option<&'a Value> {
    out.as_object().and_then(|o| o.get(name))
}

/// `out.get("ok")`, as Python's truthiness.
fn is_ok(out: &Value) -> bool {
    field(out, "ok").is_some_and(truthy)
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
    // `paths.ask or watch.openrouter`, resolved only by the providers that ask a model
    let ask = || -> Ask { opts.paths.ask.clone().unwrap_or_else(|| crate::watch::model::openrouter_ask(ctx)) };
    let feature = opts.feature.as_str();
    if provider == TINYFISH {
        let found = match &opts.paths.read {
            Some(read) => read(query),
            None => web_reader::search_and_read(ctx, query, web_reader::RESULTS),
        };
        let pages = match found {
            Ok(pages) => pages,
            Err(e) => {
                let reason = if e.reason.starts_with("TinyFish") { e.reason } else { format!("TinyFish: {}", e.reason) };
                return Ok(failure(reason));
            }
        };
        ctx.ledger.record(
            "tinyfish",
            "search+fetch",
            feature,
            Some(0.0),
            RecordOpts::new().note(&format!("{} pages, free", pages.len())),
        );
        if (opts.cancelled)() {
            return Ok(failure("stopped"));
        }
        if pages.is_empty() {
            return Ok(failure("TinyFish found no readable pages"));
        }
        let (text, used, payload) =
            web_reader::answer_parts(query, &pages, &ask(), None, None, None).map_err(|_| "FetchError".to_string())?;
        ctx.ledger.record_chat_completion(feature, &payload, web_reader::MODEL, "openrouter");
        if text.is_empty() {
            return Ok(failure(format!("the {} pages did not answer it", pages.len())));
        }
        return Ok(result(&text, &pages, &used, &[payload], Some(0.0)));
    }
    if provider == FIRECRAWL {
        return via_firecrawl(ctx, query, &ask(), opts);
    }
    // perplexity: today's websearch code, with the reader's answered-or-not rule when the reader asks
    let system = if opts.reader { READER_SYSTEM } else { websearch::SYSTEM };
    let mut out = match &opts.paths.perplexity {
        Some(perplexity) => perplexity(query, system, feature),
        None => {
            // the reader needs an answer to ITS question or NOT_FOUND; Monid's raw Google snippets are neither, so
            // the reader never pays for them (review, 2026-10-08)
            let engine = EngineOpts {
                key: opts.key.clone(),
                opener: opts.opener.clone(),
                clock: opts.clock.clone(),
                system: system.to_string(),
                feature: feature.to_string(),
                monid_ok: !opts.reader,
                ..EngineOpts::default()
            };
            websearch::engine_search(ctx, query, Some(config), &engine)
        }
    };
    let answer_text = || field(&out, "answer").map(py_str_value).unwrap_or_default();
    if opts.reader && is_ok(&out) && py_strip(&answer_text()).to_uppercase().starts_with(NOT_FOUND) {
        return Ok(failure("the search did not answer it"));
    }
    if is_ok(&out) {
        let read: HashSet<String> = field(&out, "sources")
            .and_then(Value::as_array)
            .map(|sources| {
                sources.iter().filter_map(|s| s.get("url").map(py_str_value)).collect::<HashSet<String>>()
            })
            .unwrap_or_default();
        let kept = keep_read_links(&answer_text(), &read);
        if let Some(o) = out.as_object_mut() {
            o.insert("answer".into(), Value::String(kept));
        }
    }
    Ok(out)
}

/// `_firecrawl`: Firecrawl search with Alexandria's tools; the model answers from the pages or picks ONE tool,
/// which eve runs and the model answers from. A tool that fails (or needs terms eve never accepts) leaves the
/// pages.
pub fn via_firecrawl(ctx: &Ctx, query: &str, ask: &Ask, opts: &AnswerOpts) -> Result<Value, String> {
    let feature = opts.feature.as_str();
    let fetch_error = |_: FetchError| "FetchError".to_string();
    let price = firecrawl::usd_per_credit(ctx);
    let found = match &opts.paths.firecrawl_search {
        Some(search) => search(query),
        None => firecrawl::search(ctx, query, firecrawl::RESULTS, firecrawl::SEARCH_TIMEOUT_SECS, true),
    };
    let found = match found {
        Ok(found) => found,
        Err(e) => {
            let reason = if e.reason.starts_with("Firecrawl") { e.reason } else { format!("Firecrawl: {}", e.reason) };
            return Ok(failure(reason));
        }
    };
    firecrawl::meter(ctx, "search", found.credits.as_ref(), feature, price);
    // None: Firecrawl did not say what the call cost
    let mut credits_used = credits(found.credits.as_ref());
    let (pages, tools) = (found.pages, found.tools);
    if (opts.cancelled)() {
        return Ok(failure("stopped"));
    }
    if pages.is_empty() && tools.is_empty() {
        return Ok(failure("Firecrawl found no readable pages"));
    }
    let (mut text, mut used, payload) =
        web_reader::answer_parts(query, &pages, ask, None, Some(&tools), None).map_err(fetch_error)?;
    ctx.ledger.record_chat_completion(feature, &payload, web_reader::MODEL, "openrouter");
    let call = if !tools.is_empty() && text.is_empty() {
        firecrawl::pick_tool(&payload, &tools).map_err(fetch_error)?
    } else {
        None
    };
    let mut payloads = vec![payload];
    if let Some(call) = call.filter(|_| !(opts.cancelled)()) {
        let name = call.name.clone();
        tracing::info!("search router: Alexandria {}/{}", call.provider, call.capability);
        (opts.progress)(&format!("Checking {name}…"));
        let ran = match &opts.paths.run_tool {
            Some(run) => run(&call),
            None => firecrawl::run_tool(ctx, &call, firecrawl::TOOL_TIMEOUT_SECS),
        };
        let ran = match ran {
            Ok(ran) => Some(ran),
            Err(e) => {
                tracing::info!("search router: {}; the pages answer instead", e.reason);
                None
            }
        };
        if let Some(ran) = &ran {
            firecrawl::meter(ctx, &format!("alexandria {}", call.provider), ran.credits.as_ref(), feature, price);
            let more = credits(ran.credits.as_ref());
            credits_used = match (credits_used, more) {
                (Some(a), Some(b)) => Some(a + b),
                _ => None,
            };
        }
        if (opts.cancelled)() {
            return Ok(failure("stopped"));
        }
        let tool_result = ran.map(|ran| ToolResult { name, data: ran.data });
        if tool_result.is_some() || !pages.is_empty() {
            let (t, u, payload) =
                web_reader::answer_parts(query, &pages, ask, None, None, tool_result.as_ref()).map_err(fetch_error)?;
            ctx.ledger.record_chat_completion(feature, &payload, web_reader::MODEL, "openrouter");
            text = t;
            used = u;
            payloads.push(payload);
        }
    }
    if text.is_empty() {
        return Ok(failure(format!("the {} pages did not answer it", pages.len())));
    }
    // Credits with no price on this plan (EVE_FIRECRAWL_USD unset), or credits Firecrawl did not report:
    // the route's cost is unknown, so cost_usd is None rather than the answer model's share passed off as all of it.
    let extra = match credits_used {
        None => None,
        Some(c) if price.is_none() && c != 0.0 => None,
        Some(c) => Some(c * price.unwrap_or(0.0)),
    };
    Ok(result(&text, &pages, &used, &payloads, extra))
}

/// `_credits`: a reply's `credits` as a float, None when it is not a number.
pub fn credits(reply: Option<&serde_json::Number>) -> Option<f64> {
    reply.and_then(serde_json::Number::as_f64)
}

/// `abandoned()`: true once `bounded` stopped waiting.
pub type Abandoned = Arc<dyn Fn() -> bool + Send + Sync>;
/// `bounded`'s work: `work(abandoned)`, Err the class name of what it raised.
pub type Work = Box<dyn FnOnce(Abandoned) -> Result<Value, String> + Send>;

/// `work(abandoned)` in a worker thread (carrying the caller's spend job, eve_spend::carry), waited on for
/// `budget` seconds at most: its result, or {"ok": False, "reason": "no answer within {budget:g} s"} once the
/// budget is spent; `abandoned()` then turns true so the worker stops at its next step, and its result is
/// dropped. A worker that panics or errs is {"ok": False, "reason": its class name}.
pub fn bounded(work: Work, budget: f64) -> Value {
    let gone = Arc::new(AtomicBool::new(false));
    let abandoned: Abandoned = {
        let gone = gone.clone();
        Arc::new(move || gone.load(Ordering::SeqCst))
    };
    let (tx, rx) = mpsc::channel::<Value>();
    // The caller's context goes with the work: the spend rows a chief card's search writes keep its card id
    // (spend.job; a bare Thread starts with an empty context, 2026-09-29).
    let run = eve_spend::carry(move || {
        let out = match catch_unwind(AssertUnwindSafe(|| work(abandoned))) {
            Ok(Ok(out)) => out,
            // anything at all: the next provider
            Ok(Err(kind)) => failure(kind),
            Err(_) => failure("panic"),
        };
        let _ = tx.send(out);
    });
    if std::thread::Builder::new().name("search-router".into()).spawn(run).is_err() {
        return failure("no answer");
    }
    let waited = match Duration::try_from_secs_f64(budget.max(0.0)) {
        Ok(d) => rx.recv_timeout(d).map_err(|e| matches!(e, mpsc::RecvTimeoutError::Timeout)),
        Err(_) => rx.recv().map_err(|_| false),
    };
    match waited {
        Ok(out) if truthy(&out) => out,
        Ok(_) | Err(false) => failure("no answer"),
        Err(true) => {
            gone.store(true, Ordering::SeqCst);
            failure(format!("no answer within {} s", py_g(budget)))
        }
    }
}

/// THE routed search, for both surfaces: websearch's result shape ({"ok", "answer", "sources", "usage", "ms",
/// "cost_usd"}, cost_usd None when unpriced) or {"ok": False, "reason"}. Never fails. `config.providers` empty
/// (routing off): the brain gets websearch's engine and the web reader (`reader`) gets TinyFish, each exactly as
/// before the router. Otherwise Jev picks among `config.providers`, and a routed provider that fails, finds
/// nothing, runs out of its budget or would pass today's Firecrawl cap is followed by one perplexity search.
/// Logs one line per routed search: the provider, the Choice's probability and Jev's milliseconds, never the query.
pub fn answer(ctx: &Ctx, query: &str, config: &SearchConfig, opts: &AnswerOpts) -> Value {
    let clock = opts.clock.clone();
    let t0 = clock();
    let providers = config.providers.clone();
    let order: Vec<String> = if providers.is_empty() {
        vec![(if opts.reader { TINYFISH } else { PERPLEXITY }).to_string()]
    } else {
        let router = opts.route.clone().or_else(|| jev_router(ctx));
        let mut r = match router {
            None => Route { error: "no Jev route".into(), ..Route::new(PERPLEXITY) },
            // a router that fails is a perplexity search
            Some(router) => match catch_unwind(AssertUnwindSafe(|| router(query, &providers))) {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => Route { error: e.kind, ..Route::new(PERPLEXITY) },
                Err(_) => Route { error: "panic".into(), ..Route::new(PERPLEXITY) },
            },
        };
        if !providers.contains(&r.provider) {
            let error = if r.error.is_empty() { format!("{} is not available", r.provider) } else { r.error.clone() };
            r = Route { provider: PERPLEXITY.into(), p: r.p, ms: r.ms, error };
        }
        if r.provider == FIRECRAWL && firecrawl::left(ctx, None) <= 0 {
            r = Route {
                provider: PERPLEXITY.into(),
                p: r.p,
                ms: r.ms,
                error: format!("today's {} Firecrawl calls are used up", firecrawl::per_day(ctx)),
            };
        }
        let error = if r.error.is_empty() { String::new() } else { format!(", {}", r.error) };
        tracing::info!("search router: {} (p={:.2}, jev {:.0} ms{error})", r.provider, r.p, r.ms);
        let mut order = vec![r.provider.clone()];
        if r.provider != PERPLEXITY {
            order.push(PERPLEXITY.into());
        }
        order
    };
    let budget = if opts.reader { config.routed_secs.max(READER_ROUTED_SECS) } else { config.routed_secs };
    let mut out = failure("no provider ran");
    for provider in &order {
        (opts.tried)(provider);
        let attempt = {
            let (ctx, query, config, opts, provider) =
                (ctx.clone(), query.to_string(), config.clone(), opts.clone(), provider.clone());
            move |abandoned: Abandoned| -> Result<Value, String> {
                let outer = opts.cancelled.clone();
                let opts = AnswerOpts { cancelled: Arc::new(move || outer() || abandoned()), ..opts };
                via(&ctx, &provider, &query, &config, &opts)
            }
        };
        out = if !providers.is_empty() && provider != PERPLEXITY {
            // a routed provider: never past its budget
            bounded(Box::new(attempt), budget)
        } else {
            // perplexity, or routing off: its own timeouts, as before
            match catch_unwind(AssertUnwindSafe(|| attempt(Arc::new(|| false)))) {
                Ok(Ok(out)) => out,
                // anything at all: the next provider, or the caller's fallback
                Ok(Err(kind)) => failure(format!("{provider}: {kind}")),
                Err(_) => failure(format!("{provider}: panic")),
            }
        };
        let stopped = field(&out, "reason").and_then(Value::as_str) == Some("stopped");
        if is_ok(&out) || stopped || (opts.cancelled)() {
            break;
        }
        if order.len() > 1 {
            let reason = field(&out, "reason").map(py_str_value).unwrap_or_else(|| "None".into());
            tracing::info!("search router: {provider} did not answer ({reason}); perplexity takes it");
        }
    }
    if is_ok(&out) && field(&out, "ms").is_none() {
        if let Some(o) = out.as_object_mut() {
            o.insert("ms".into(), json!(py_round_int((clock() - t0) * 1000.0)));
        }
    }
    out
}
