//! Port of bridge/tests/test_search_router.py (Jev picks the provider for every web search; the one routed entry
//! point, `answer`) and bridge/tests/test_websearch.py (one OpenRouter call with the web search server tool, read
//! back as an answer and sources). Fakes only: no Jev call, no search, no token.
//!
//! Python's fixtures become helpers: `quiet` (spend patched away, a fresh Firecrawl count) is [`Fx::new`], a
//! context with its own spend folder, its own Firecrawl count file and EVE_MONID=0 (conftest's
//! `_no_paid_monid_runs`). Module patches become the seams `Paths`, `AnswerOpts.route`, `EngineOpts.opener`.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, FixedOffset, TimeZone, Utc};
use eve_web::firecrawl::{self, DayCount, Found, ToolCall};
use eve_web::search_router::{self as sr, AnswerOpts, Paths, Route, RouteError, RouteFn, SearchAnswer, SearchGates};
use eve_web::watch::FetchError;
use eve_web::web_reader::{FirecrawlSearchFn, Page, ReadFn, ToolRunFn};
use eve_web::websearch::{self as ws, EngineOpts, OpenError, OpenRequest, Opener, SearchConfig};
use eve_web::{Ask, Clock, Ctx, HttpOptions};
use serde_json::{json, Value};

const ALL: [&str; 3] = [sr::PERPLEXITY, sr::TINYFISH, sr::FIRECRAWL];
const OPEN: SearchGates =
    SearchGates { firecrawl: 0.8, structured: 0.8, tinyfish: 0.8, single: 0.8, many_max: 0.3, changes_max: 0.3 };

fn all() -> Vec<String> {
    ALL.iter().map(|s| s.to_string()).collect()
}

fn strings(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| s.to_string()).collect()
}

/// A test's process: its own environment, spend folder and Firecrawl count, Monid off.
struct Fx {
    ctx: Ctx,
    _dir: tempfile::TempDir,
}

impl Fx {
    fn new(env: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().expect("a temp dir");
        let mut map: HashMap<String, String> = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        map.entry("EVE_MONID".into()).or_insert_with(|| "0".into());
        let ctx = Ctx {
            ledger: eve_spend::Ledger::at(dir.path()),
            firecrawl_calls: Arc::new(DayCount::at(dir.path().join(firecrawl::COUNT_FILE))),
            ..Ctx::global().clone()
        }
        .with_env(map);
        Fx { ctx, _dir: dir }
    }
}

fn env_of(pairs: &[(&str, &str)]) -> Ctx {
    let map: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    Ctx::global().clone().with_env(map)
}

fn zero_clock() -> Clock {
    Arc::new(|| 0.0)
}

/// `iter([...]).__next__`: each call the next value (the last one repeats).
fn ticks(values: &[f64]) -> Clock {
    let values = values.to_vec();
    let at = Mutex::new(0usize);
    Arc::new(move || {
        let mut i = at.lock().unwrap();
        let v = values[(*i).min(values.len() - 1)];
        *i += 1;
        v
    })
}

/// Captured `tracing` lines (caplog): the lines the calling thread logs inside `f`.
fn caplog<R>(f: impl FnOnce() -> R) -> (R, String) {
    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let out = tracing::subscriber::with_default(subscriber, f);
    let text = String::from_utf8_lossy(&buf.0.lock().unwrap()).into_owned();
    (out, text)
}

fn jev(p_tf: f64, p_fc: f64, structured: f64, many: f64, single: f64, changes: f64) -> Value {
    json!({"answers": {"provider": {"probabilities": {"quick_page_read": p_tf, "live_data_service": p_fc,
                                                      "deep_web_research": (1.0 - p_tf - p_fc).max(0.0)}},
                       "needs_live_structured_data": {"noul": structured},
                       "needs_many_sources_or_searches": {"noul": many},
                       "answer_changes_over_time": {"noul": changes},
                       "single_lookup_or_page_read": {"noul": single}}})
}

fn answer_of(p_tf: f64, p_pp: f64, p_fc: f64, structured: f64, many: f64, single: f64) -> SearchAnswer {
    SearchAnswer {
        p_tinyfish: p_tf,
        p_perplexity: p_pp,
        p_firecrawl: p_fc,
        structured,
        many,
        single,
        changes: 0.0,
        ms: 0.0,
        error: String::new(),
    }
}

// ---- the question and the rule ------------------------------------------------------------------------------

#[test]
fn the_state_is_the_request_and_jev_always_sees_all_three_options_as_structure() {
    let seen: Arc<Mutex<Vec<(Value, Value)>>> = Arc::default();
    let s = seen.clone();
    let predict = move |state: &Value, qs: &Value| -> Result<Value, eve_decider::PredictError> {
        s.lock().unwrap().push((state.clone(), qs.clone()));
        Ok(jev(0.0, 0.0, 0.0, 1.0, 0.0, 0.0))
    };
    sr::ask(&predict, "  opening   hours ", &zero_clock());
    let (state, qs) = seen.lock().unwrap()[0].clone();
    assert_eq!(state, json!({"about": sr::ABOUT, "request": "opening hours"}));
    let criteria: HashSet<String> = qs["provider"]["criteria"].as_object().unwrap().keys().cloned().collect();
    let options: HashSet<String> = sr::OPTION_TO_PROVIDER.iter().map(|(o, _)| o.to_string()).collect();
    assert_eq!(criteria, options);
    assert_eq!(criteria, strings(&["quick_page_read", "deep_web_research", "live_data_service"]).into_iter().collect());
    for option in qs["provider"]["criteria"].as_object().unwrap().values() {
        let keys: HashSet<&str> = option.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, HashSet::from(["what", "for", "not_for"]));
    }
    let nouls: HashSet<&str> =
        qs.as_object().unwrap().iter().filter(|(_, q)| q["type"] == "noul").map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        nouls,
        HashSet::from([
            "needs_live_structured_data",
            "answer_changes_over_time",
            "needs_many_sources_or_searches",
            "single_lookup_or_page_read"
        ])
    );
    // the wording names the job, never a brand: Jev reads what a provider does
    let words = serde_json::to_string(&sr::questions()).unwrap().to_lowercase();
    for brand in ["tinyfish", "perplexity", "firecrawl", "alexandria", "openrouter"] {
        assert!(!words.contains(brand), "{brand}");
    }
    // the quick-read boundary is written on both sides
    let join = |v: &Value| v.as_array().unwrap().iter().map(|s| s.as_str().unwrap()).collect::<Vec<_>>().join(" ");
    assert!(join(&sr::OPTIONS["quick_page_read"]["not_for"]).contains("changes over time"));
    assert!(join(&sr::OPTIONS["deep_web_research"]["not_for"]).contains("stays the same over time"));
}

#[test]
fn the_question_never_varies_with_the_keys_and_a_missing_pick_is_masked() {
    for available in [all(), strings(&[sr::PERPLEXITY, sr::TINYFISH]), strings(&[sr::PERPLEXITY, sr::FIRECRAWL])] {
        let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
        let s = seen.clone();
        let predict = move |_: &Value, qs: &Value| -> Result<Value, eve_decider::PredictError> {
            s.lock().unwrap().push(qs.clone());
            Ok(jev(0.97, 0.01, 0.02, 0.05, 0.97, 0.0)) // a clear quick read
        };
        let r = sr::make_router(Arc::new(predict), zero_clock(), Some(OPEN))("opening hours", &available).unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![sr::questions()]); // the full three-option question, every Mac
        if available.iter().any(|p| p == sr::TINYFISH) {
            assert!(r.provider == sr::TINYFISH && r.error.is_empty());
        } else {
            assert!(r.provider == sr::PERPLEXITY && r.error == "tinyfish is not available");
        }
    }
}

#[test]
fn decide_needs_both_the_choice_and_the_noul_and_else_is_perplexity() {
    let said = |a: [f64; 5], changes: f64, error: &str| -> &'static str {
        let mut ans = answer_of(a[0], 0.0, a[1], a[2], a[3], a[4]);
        ans.changes = changes;
        ans.error = error.to_string();
        sr::decide(&ans, &OPEN)
    };
    assert_eq!(said([0.05, 0.9, 0.9, 0.2, 0.1], 0.0, ""), sr::FIRECRAWL);
    assert_eq!(said([0.05, 0.9, 0.5, 0.2, 0.1], 0.0, ""), sr::PERPLEXITY); // the Choice alone is not a gate
    assert_eq!(said([0.05, 0.5, 0.95, 0.2, 0.1], 0.0, ""), sr::PERPLEXITY); // nor the Noul alone
    assert_eq!(said([0.9, 0.05, 0.05, 0.1, 0.9], 0.0, ""), sr::TINYFISH);
    assert_eq!(said([0.9, 0.05, 0.05, 0.6, 0.9], 0.0, ""), sr::PERPLEXITY); // many sources: never the reader
    assert_eq!(said([0.9, 0.05, 0.05, 0.1, 0.9], 0.6, ""), sr::PERPLEXITY); // an answer that moves: never either
    assert_eq!(said([0.9, 0.05, 0.05, 0.1, 0.9], 0.3, ""), sr::TINYFISH); // at its maximum, still the reader
    assert_eq!(said([0.9, 0.05, 0.05, 0.1, 0.5], 0.0, ""), sr::PERPLEXITY); // not a single lookup
    assert_eq!(said([0.5, 0.05, 0.05, 0.1, 0.9], 0.0, ""), sr::PERPLEXITY);
    assert_eq!(said([0.9, 0.9, 0.9, 0.1, 0.9], 0.0, ""), sr::FIRECRAWL); // a data service first when both clear
    assert_eq!(said([0.9, 0.05, 0.05, 0.1, 0.9], 0.0, "TimeoutError"), sr::PERPLEXITY);
}

#[test]
fn the_router_never_picks_what_is_not_available_and_a_failure_is_perplexity() {
    let route = sr::make_router(
        Arc::new(|_: &Value, _: &Value| Ok(jev(0.05, 0.95, 0.95, 0.1, 0.1, 0.0))),
        zero_clock(),
        Some(OPEN),
    );
    assert_eq!(route("cheapest flight", &all()).unwrap().provider, sr::FIRECRAWL);
    assert!((route("cheapest flight", &all()).unwrap().p - 0.95).abs() < 1e-9);
    assert_eq!(route("cheapest flight", &strings(&[sr::PERPLEXITY, sr::TINYFISH])).unwrap().provider, sr::PERPLEXITY);

    let boom = |_: &Value, _: &Value| Err(eve_decider::PredictError::new("TimeoutError", "jev slow"));
    let r = sr::make_router(Arc::new(boom), zero_clock(), Some(OPEN))("x", &all()).unwrap();
    assert!(r.provider == sr::PERPLEXITY && r.error.contains("TimeoutError"));
    assert_eq!(r.error, "TimeoutError: jev slow");
    // an empty or odd envelope is no answer, and no answer is perplexity
    let empty = sr::make_router(Arc::new(|_: &Value, _: &Value| Ok(json!({"answers": {}}))), zero_clock(), Some(OPEN));
    assert_eq!(empty("x", &all()).unwrap().provider, sr::PERPLEXITY);
    let odd = sr::make_router(Arc::new(|_: &Value, _: &Value| Ok(json!([1, 2]))), zero_clock(), Some(OPEN));
    assert_eq!(odd("x", &all()).unwrap().provider, sr::PERPLEXITY);
}

#[test]
fn fit_allows_zero_wrong_routes_and_prefers_the_stricter_tie() {
    let mut moves = answer_of(0.97, 0.0, 0.01, 0.02, 0.02, 0.95);
    moves.changes = 0.25;
    let answers = vec![
        answer_of(0.97, 0.0, 0.01, 0.02, 0.05, 0.97), // tinyfish
        answer_of(0.92, 0.0, 0.01, 0.02, 0.25, 0.9),  // perplexity: not to the reader
        moves,                                        // perplexity: the answer moves
        answer_of(0.02, 0.0, 0.97, 0.97, 0.05, 0.1),  // firecrawl
        answer_of(0.02, 0.0, 0.85, 0.9, 0.3, 0.2),    // tinyfish-labelled: never firecrawl
        answer_of(0.2, 0.7, 0.1, 0.1, 0.9, 0.1),      // perplexity
    ];
    let truths = [sr::TINYFISH, sr::PERPLEXITY, sr::PERPLEXITY, sr::FIRECRAWL, sr::TINYFISH, sr::PERPLEXITY];
    let g = sr::fit(&answers, &truths);
    let said: Vec<&str> = answers.iter().map(|a| sr::decide(a, &g)).collect();
    assert_eq!(said, vec![sr::TINYFISH, sr::PERPLEXITY, sr::PERPLEXITY, sr::FIRECRAWL, sr::PERPLEXITY, sr::PERPLEXITY]);
    assert!(!said.iter().zip(truths).any(|(s, t)| sr::wrong(s, t)));
    assert!(g.tinyfish == 0.95 && g.firecrawl == 0.95 && g.many_max == 0.05 && g.changes_max == 0.05);
    // nothing safe to route: the gates route nothing
    let sure = answer_of(0.99, 0.0, 0.0, 0.0, 0.0, 0.99);
    let nothing = sr::fit(std::slice::from_ref(&sure), &[sr::PERPLEXITY]);
    assert_eq!(sr::decide(&sure, &nothing), sr::PERPLEXITY);
}

#[test]
#[should_panic(expected = "different lengths")]
fn fit_refuses_answers_and_truths_of_different_lengths() {
    sr::fit(&[answer_of(0.9, 0.0, 0.0, 0.0, 0.0, 0.9)], &[]);
}

#[test]
fn wrong_is_the_costly_direction_only() {
    assert!(sr::wrong(sr::TINYFISH, sr::PERPLEXITY) && sr::wrong(sr::TINYFISH, sr::FIRECRAWL));
    assert!(sr::wrong(sr::FIRECRAWL, sr::TINYFISH) && sr::wrong(sr::FIRECRAWL, sr::PERPLEXITY));
    assert!(!sr::wrong(sr::PERPLEXITY, sr::TINYFISH) && !sr::wrong(sr::PERPLEXITY, sr::FIRECRAWL));
}

#[test]
fn the_placeholder_gates_route_nothing() {
    let sure = answer_of(1.0, 0.0, 1.0, 1.0, 0.0, 1.0);
    assert_eq!(sr::decide(&sure, &SearchGates::default()), sr::PERPLEXITY);
}

// ---- the switch ---------------------------------------------------------------------------------------------

const KEYS: [(&str, &str); 4] = [
    ("OPENROUTER_API_KEY", "or-test"),
    ("TYPESAFE_API_KEY", "ts-test"),
    ("TINYFISH_API_KEY", "tf-test"),
    ("FIRECRAWL_API_KEY", "fc-test"),
];

fn keys_with(extra: &[(&'static str, &'static str)], without: &[&str]) -> Ctx {
    let mut pairs: Vec<(&str, &str)> = KEYS.iter().copied().filter(|(k, _)| !without.contains(k)).collect();
    pairs.extend_from_slice(extra);
    env_of(&pairs)
}

#[test]
fn available_follows_the_mode_the_keys_and_the_engine() {
    let engine = "openrouter-perplexity";
    let none: Vec<String> = Vec::new();
    assert_eq!(sr::available_shipped(&keys_with(&[], &[]), engine, false), none); // auto, not shipped
    let on = [("EVE_SEARCH_ROUTER", "on")];
    assert_eq!(sr::available_shipped(&keys_with(&on, &[]), engine, false), all());
    assert_eq!(sr::available_shipped(&keys_with(&[("EVE_SEARCH_ROUTER", "off")], &[]), engine, false), none);
    assert_eq!(sr::available_shipped(&keys_with(&on, &[]), "openai", false), none); // no OpenRouter engine
    assert_eq!(sr::available_shipped(&keys_with(&on, &["OPENROUTER_API_KEY"]), engine, false), none);
    assert_eq!(sr::available_shipped(&keys_with(&on, &["TYPESAFE_API_KEY"]), engine, false), none); // no Jev route
    assert_eq!(
        sr::available_shipped(&keys_with(&on, &["FIRECRAWL_API_KEY"]), engine, false),
        strings(&[sr::PERPLEXITY, sr::TINYFISH])
    );
    // perplexity alone is no routing
    assert_eq!(sr::available_shipped(&keys_with(&on, &["FIRECRAWL_API_KEY", "TINYFISH_API_KEY"]), engine, false), none);
    assert_eq!(sr::available_shipped(&keys_with(&[], &[]), engine, true), all()); // auto, shipped
    assert_eq!(sr::available(&keys_with(&[], &[]), engine), all()); // SHIPPED is true
    assert_eq!(sr::available_shipped(&keys_with(&[("EVE_SEARCH_ROUTER", "off")], &[]), engine, true), none); // off beats shipped
    assert_eq!(sr::mode(&env_of(&[("EVE_SEARCH_ROUTER", "sometimes")])), "auto");
    assert_eq!(sr::mode(&env_of(&[("EVE_SEARCH_ROUTER", " YES ")])), "on");
    assert_eq!(sr::mode(&env_of(&[("EVE_SEARCH_ROUTER", "0")])), "off");
    assert_eq!(sr::mode(&env_of(&[])), "auto");
}

// ---- the routed search ----------------------------------------------------------------------------------------

fn pages() -> Vec<Page> {
    vec![Page {
        url: "https://hours.example/exploratorium".into(),
        title: "Hours".into(),
        markdown: "Sunday 10am-5pm.".into(),
    }]
}

/// The answer model: each call the next reply, as a JSON text in a chat completion that reports a cost.
fn model(replies: Vec<Value>) -> (Ask, Arc<Mutex<Vec<Value>>>) {
    let sent: Arc<Mutex<Vec<Value>>> = Arc::default();
    let s = sent.clone();
    let ask: Ask = Arc::new(move |body: &Value| {
        let mut sent = s.lock().unwrap();
        sent.push(body.clone());
        let reply = &replies[sent.len() - 1];
        Ok(json!({"choices": [{"message": {"content": serde_json::to_string(reply).unwrap()}}],
                  "usage": {"prompt_tokens": 800, "completion_tokens": 30, "cost": 0.0002}}))
    });
    (ask, sent)
}

type Calls = Arc<Mutex<Vec<(String, String, String)>>>;

/// The many-source engine, faked: what it was asked, and one answer with one source.
fn engine(answer: &str) -> (sr::PerplexityFn, Calls) {
    let calls: Calls = Arc::default();
    let c = calls.clone();
    let answer = answer.to_string();
    let run: sr::PerplexityFn = Arc::new(move |query: &str, system: &str, feature: &str| {
        c.lock().unwrap().push((query.into(), system.into(), feature.into()));
        json!({"ok": true, "answer": answer, "sources": [{"title": "S", "url": "https://s.example/a", "snippet": ""}],
               "usage": {"in": 1, "out": 1, "cost": 0.006}, "ms": 900, "cost_usd": 0.006})
    });
    (run, calls)
}

const PERPLEXITY_SAYS: &str = "Perplexity says 10 to 5.";

fn cfg() -> SearchConfig {
    SearchConfig { engine: "openrouter-perplexity".into(), providers: all(), ..SearchConfig::default() }
}

fn to(provider: &'static str) -> RouteFn {
    Arc::new(move |_: &str, _: &[String]| Ok(Route { provider: provider.into(), p: 0.97, ms: 180.0, error: String::new() }))
}

fn opts(paths: Paths, route: RouteFn) -> AnswerOpts {
    AnswerOpts { paths, route: Some(route), ..AnswerOpts::default() }
}

fn keys_of(v: &Value) -> HashSet<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

fn shape() -> HashSet<String> {
    strings(&["ok", "answer", "sources", "usage", "ms", "cost_usd"]).into_iter().collect()
}

#[test]
fn a_routed_quick_read_answers_in_the_brains_result_shape() {
    let fx = Fx::new(&[]);
    let (per, calls) = engine(PERPLEXITY_SAYS);
    let (ask, _) = model(vec![json!({"answered": true, "answer": "Open 10am to 5pm on Sunday.", "sources": [1]})]);
    let paths = Paths { perplexity: Some(per), read: Some(Arc::new(|_: &str| Ok(pages()))), ask: Some(ask), ..Paths::default() };
    let o = AnswerOpts { clock: ticks(&[0.0, 1.2]), ..opts(paths, to(sr::TINYFISH)) };
    let (out, log) = caplog(|| sr::answer(&fx.ctx, "secret query words", &cfg(), &o));
    assert_eq!(keys_of(&out), shape());
    assert_eq!(out["answer"], "Open 10am to 5pm on Sunday.");
    assert_eq!(out["ms"], 1200);
    assert_eq!(
        out["sources"],
        json!([{"title": "Hours", "url": "https://hours.example/exploratorium", "snippet": "Sunday 10am-5pm."}])
    );
    assert_eq!(out["usage"], json!({"in": 800, "out": 30, "cost": 0.0002}));
    assert_eq!(out["cost_usd"], 0.0002);
    assert!(calls.lock().unwrap().is_empty());
    assert!(log.contains("search router: tinyfish (p=0.97, jev 180 ms)"), "{log}");
    assert!(!log.contains("secret"));
}

#[test]
fn a_routed_provider_that_fails_is_followed_by_one_perplexity_search() {
    let reads: Vec<ReadFn> = vec![
        Arc::new(|_: &str| Ok(Vec::new())),
        Arc::new(|_: &str| Err(FetchError::with_status("TinyFish: the site answered HTTP 429", 429))),
        Arc::new(|_: &str| panic!("boom")),
    ];
    for read in reads {
        let fx = Fx::new(&[]);
        let (per, calls) = engine(PERPLEXITY_SAYS);
        let (ask, _) = model(vec![json!({"answered": true, "answer": "x", "sources": []})]);
        let paths = Paths { perplexity: Some(per), read: Some(read), ask: Some(ask), ..Paths::default() };
        let out = sr::answer(&fx.ctx, "q", &cfg(), &opts(paths, to(sr::TINYFISH)));
        assert!(out["ok"] == true && out["answer"] == PERPLEXITY_SAYS);
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].1 == ws::SYSTEM && calls[0].2 == eve_spend::SEARCH);
    }
}

#[test]
fn a_pages_non_answer_is_followed_by_perplexity_too() {
    let fx = Fx::new(&[]);
    let (per, _) = engine(PERPLEXITY_SAYS);
    let (ask, _) = model(vec![json!({"answered": false, "answer": "", "sources": []})]);
    let paths = Paths { perplexity: Some(per), read: Some(Arc::new(|_: &str| Ok(pages()))), ask: Some(ask), ..Paths::default() };
    assert_eq!(sr::answer(&fx.ctx, "q", &cfg(), &opts(paths, to(sr::TINYFISH)))["answer"], PERPLEXITY_SAYS);
}

#[test]
fn a_router_that_fails_or_names_a_missing_provider_is_perplexity() {
    let fx = Fx::new(&[]);
    let (per, calls) = engine(PERPLEXITY_SAYS);
    let broken: RouteFn =
        Arc::new(|_: &str, _: &[String]| Err(RouteError { kind: "RuntimeError".into(), message: "jev down".into() }));
    let paths = Paths { perplexity: Some(per), ..Paths::default() };
    let (out, log) = caplog(|| {
        let first = sr::answer(&fx.ctx, "q", &cfg(), &opts(paths.clone(), broken));
        let only_tf = SearchConfig { providers: strings(&[sr::PERPLEXITY, sr::TINYFISH]), ..cfg() };
        let second = sr::answer(&fx.ctx, "q", &only_tf, &opts(paths.clone(), to(sr::FIRECRAWL)));
        (first, second)
    });
    assert!(out.0["ok"] == true && out.1["ok"] == true);
    assert_eq!(calls.lock().unwrap().len(), 2);
    assert!(log.contains("firecrawl is not available"), "{log}");
    assert!(log.contains("search router: perplexity (p=0.00, jev 0 ms, RuntimeError)"), "{log}");
}

#[test]
fn routing_off_is_the_engine_for_the_brain_and_tinyfish_for_the_reader() {
    let fx = Fx::new(&[]);
    let (per, calls) = engine(PERPLEXITY_SAYS);
    let read: Arc<Mutex<Vec<String>>> = Arc::default();
    let r = read.clone();
    let (ask, _) = model(vec![json!({"answered": true, "answer": "x", "sources": []})]);
    let paths = Paths {
        perplexity: Some(per),
        read: Some(Arc::new(move |q: &str| {
            r.lock().unwrap().push(q.to_string());
            Ok(Vec::new())
        })),
        ask: Some(ask),
        ..Paths::default()
    };
    let never: RouteFn = Arc::new(|_: &str, _: &[String]| panic!("routing is off: Jev is never asked"));
    let off = SearchConfig { engine: "openrouter-perplexity".into(), ..SearchConfig::default() };
    assert_eq!(sr::answer(&fx.ctx, "q", &off, &opts(paths.clone(), never.clone()))["ok"], true);
    assert!(read.lock().unwrap().is_empty());
    let out = sr::answer(&fx.ctx, "q", &off, &AnswerOpts { reader: true, ..opts(paths, never) });
    assert_eq!(out, json!({"ok": false, "reason": "TinyFish found no readable pages"}));
    assert_eq!(*read.lock().unwrap(), vec!["q".to_string()]);
    assert_eq!(calls.lock().unwrap().len(), 1); // no perplexity for the reader
}

#[test]
fn links_the_search_did_not_read_are_removed_whichever_provider_answered() {
    let fx = Fx::new(&[]);
    let (per, _) = engine("Book at https://s.example/a or https://evil.example/pay now.");
    let out = sr::answer(&fx.ctx, "q", &cfg(), &opts(Paths { perplexity: Some(per), ..Paths::default() }, to(sr::PERPLEXITY)));
    assert_eq!(out["answer"], "Book at https://s.example/a or (link removed) now.");
}

#[test]
fn keep_read_links_keeps_a_read_link_before_its_punctuation_and_collapses_whitespace() {
    let read: HashSet<String> = HashSet::from(["https://a.example/x".to_string()]);
    assert_eq!(
        sr::keep_read_links("See  https://a.example/x.\nNot www.b.example/y!", &read),
        "See https://a.example/x. Not (link removed)"
    );
    assert_eq!(sr::keep_read_links("HTTPS://A.example/x", &read), "(link removed)");
    // Python's \S stops at the information separators, which str.split() also splits on
    assert_eq!(sr::keep_read_links("https://a.example/x\x1cmore", &read), "https://a.example/x more");
}

#[test]
fn the_reader_hears_not_found_as_no_answer() {
    let fx = Fx::new(&[]);
    let (per, calls) = engine("  not found.");
    let paths = Paths { perplexity: Some(per), ..Paths::default() };
    let out = sr::answer(&fx.ctx, "q", &cfg(), &AnswerOpts { reader: true, ..opts(paths, to(sr::PERPLEXITY)) });
    assert_eq!(out, json!({"ok": false, "reason": "the search did not answer it"}));
    assert_eq!(calls.lock().unwrap()[0].1, sr::READER_SYSTEM);
}

// ---- the time budget, the Firecrawl cap, the cost ---------------------------------------------------------------

#[test]
fn a_slow_routed_provider_is_abandoned_at_its_budget_and_perplexity_answers() {
    let fx = Fx::new(&[]);
    let (per, calls) = engine(PERPLEXITY_SAYS);
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let stopped = Arc::new(AtomicBool::new(false));
    let rel = release.clone();
    // a reader that hangs past any per-call timeout
    let slow_read = move |_: &str| -> Result<Vec<Page>, FetchError> {
        let (lock, cv) = &*rel;
        let guard = lock.lock().unwrap();
        let _ = cv.wait_timeout_while(guard, Duration::from_secs(5), |released| !*released).unwrap();
        Ok(pages())
    };
    let st = stopped.clone();
    // reached only if the worker ignored being abandoned
    let ask: Ask = Arc::new(move |_: &Value| {
        st.store(true, Ordering::SeqCst);
        Ok(json!({"choices": [{"message": {"content": "{}"}}]}))
    });
    let config = SearchConfig { routed_secs: 0.05, ..cfg() };
    let paths = Paths { perplexity: Some(per), read: Some(Arc::new(slow_read)), ask: Some(ask), ..Paths::default() };
    let t0 = Instant::now();
    let (out, log) = caplog(|| sr::answer(&fx.ctx, "q", &config, &opts(paths, to(sr::TINYFISH))));
    let waited = t0.elapsed();
    assert!(out["ok"] == true && out["answer"] == PERPLEXITY_SAYS && calls.lock().unwrap().len() == 1);
    assert!(waited < Duration::from_secs(2)); // the budget, not the reader's hang
    assert!(log.contains("tinyfish did not answer (no answer within 0.05 s); perplexity takes it"), "{log}");
    {
        let (lock, cv) = &*release;
        *lock.lock().unwrap() = true; // the abandoned worker wakes, sees it was dropped,
        cv.notify_all();
    }
    std::thread::sleep(Duration::from_millis(100)); // and stops before its model call
    assert!(!stopped.load(Ordering::SeqCst));
}

#[test]
fn the_budget_is_the_brains_setting_and_longer_but_bounded_for_the_reader() {
    // Python spies on `bounded`; here the budget shows in what it lets through: a reader that takes 0.3 s is
    // abandoned under the brain's 0.05 s and waited for under the reader's READER_ROUTED_SECS.
    let fx = Fx::new(&[]);
    let (ask, _) = model(vec![
        json!({"answered": true, "answer": "From the page.", "sources": [1]}),
        json!({"answered": true, "answer": "From the page.", "sources": [1]}),
    ]);
    let slow: ReadFn = Arc::new(|_: &str| {
        std::thread::sleep(Duration::from_millis(300));
        Ok(pages())
    });
    let config = SearchConfig { routed_secs: 0.05, ..cfg() };
    let (per, calls) = engine(PERPLEXITY_SAYS);
    let paths = Paths { perplexity: Some(per), read: Some(slow), ask: Some(ask), ..Paths::default() };
    let brain = sr::answer(&fx.ctx, "q", &config, &opts(paths.clone(), to(sr::TINYFISH)));
    assert_eq!(brain["answer"], PERPLEXITY_SAYS);
    let reader = sr::answer(&fx.ctx, "q", &config, &AnswerOpts { reader: true, ..opts(paths.clone(), to(sr::TINYFISH)) });
    assert_eq!(reader["answer"], "From the page.");
    // perplexity: its own timeout, unwrapped
    sr::answer(&fx.ctx, "q", &config, &opts(paths, to(sr::PERPLEXITY)));
    assert_eq!(calls.lock().unwrap().len(), 2);
    assert_eq!(ws::DEFAULT_ROUTED_SECS, 10.0);
    const { assert!(sr::READER_ROUTED_SECS < 120.0) };
    assert_eq!(ws::configured(&env_of(&[("EVE_SEARCH_ROUTED_SECS", "4")])).routed_secs, 4.0);
    assert_eq!(ws::configured(&env_of(&[("EVE_SEARCH_ROUTED_SECS", "600")])).routed_secs, 60.0);
    assert_eq!(ws::configured(&env_of(&[("EVE_SEARCH_ROUTED_SECS", "soon")])).routed_secs, 10.0);
    assert_eq!(ws::configured(&env_of(&[("EVE_SEARCH_ROUTED_SECS", "0.2")])).routed_secs, 1.0);
}

#[test]
fn bounded_returns_the_work_or_its_failure() {
    assert_eq!(sr::bounded(Box::new(|_| Ok(json!({"ok": true}))), 1.0), json!({"ok": true}));
    assert_eq!(sr::bounded(Box::new(|_| Ok(json!({}))), 1.0), json!({"ok": false, "reason": "no answer"}));
    assert_eq!(sr::bounded(Box::new(|_| Err("ValueError".into())), 1.0), json!({"ok": false, "reason": "ValueError"}));
    let late = sr::bounded(
        Box::new(|_| {
            std::thread::sleep(Duration::from_millis(300));
            Ok(json!({"ok": true}))
        }),
        0.0,
    );
    assert_eq!(late, json!({"ok": false, "reason": "no answer within 0 s"}));
}

/// A fake `watch.http_request` that records each URL and answers `body`.
fn http_answering(sent: Arc<Mutex<Vec<String>>>, body: Value) -> Arc<dyn eve_web::Http> {
    Arc::new(move |url: &str, _: &HttpOptions| -> Result<(u16, String), FetchError> {
        sent.lock().unwrap().push(url.to_string());
        Ok((200, body.to_string()))
    })
}

#[test]
fn the_firecrawl_cap_is_one_count_for_both_surfaces_per_local_day() {
    let sent: Arc<Mutex<Vec<String>>> = Arc::default();
    let body = json!({"success": true, "creditsUsed": 1, "data": {"web": [], "alexandria": [{"data": {}, "creditsCost": 1}]}});
    let fx = Fx::new(&[("FIRECRAWL_API_KEY", "fc-test")]);
    assert_eq!(firecrawl::per_day(&env_of(&[])), 5);
    assert_eq!(firecrawl::per_day(&env_of(&[("EVE_FIRECRAWL_PER_DAY", "x")])), 5);
    let ctx = Ctx {
        firecrawl_calls: fx.ctx.firecrawl_calls.clone(),
        ..env_of(&[("FIRECRAWL_API_KEY", "fc-test"), ("EVE_FIRECRAWL_PER_DAY", "3")])
    }
    .with_ledger(fx.ctx.ledger.clone())
    .with_http(http_answering(sent.clone(), body));
    let search = |q: &str| firecrawl::search(&ctx, q, firecrawl::RESULTS, firecrawl::SEARCH_TIMEOUT_SECS, false);
    search("a").unwrap(); // the brain's search
    let call = ToolCall { provider: "p".into(), capability: "c".into(), options: Default::default(), name: "n".into() };
    firecrawl::run_tool(&ctx, &call, firecrawl::TOOL_TIMEOUT_SECS).unwrap(); // a data tool: a call too
    search("b").unwrap(); // the web reader's
    assert_eq!(*sent.lock().unwrap(), strings(&[firecrawl::SEARCH_URL, firecrawl::TOOL_URL, firecrawl::SEARCH_URL]));
    assert_eq!(firecrawl::left(&ctx, None), 0);
    let e = search("d").unwrap_err(); // never sent
    assert!(e.reason.contains("today's 3 calls are used up"), "{}", e.reason);
    assert_eq!(sent.lock().unwrap().len(), 3);
    assert_eq!(e.host, firecrawl::HOST);
    let tomorrow = eve_web::time_time() + 86400.0;
    assert_eq!(firecrawl::left(&ctx, Some(tomorrow)), 3);
    assert!(firecrawl::take(&ctx, Some(tomorrow)));
}

#[test]
fn a_spent_firecrawl_day_routes_to_perplexity_with_the_reason_logged() {
    let fx = Fx::new(&[("EVE_FIRECRAWL_PER_DAY", "1")]);
    assert!(firecrawl::take(&fx.ctx, None)); // the reader spent today's one call
    let (per, calls) = engine(PERPLEXITY_SAYS);
    let never: FirecrawlSearchFn =
        Arc::new(|_: &str| panic!("the cap is spent: Firecrawl is never called"));
    let paths = Paths { perplexity: Some(per), firecrawl_search: Some(never), ..Paths::default() };
    let (out, log) = caplog(|| sr::answer(&fx.ctx, "q", &cfg(), &opts(paths, to(sr::FIRECRAWL))));
    assert!(out["ok"] == true && calls.lock().unwrap().len() == 1);
    assert!(
        log.contains("search router: perplexity (p=0.97, jev 180 ms, today's 1 Firecrawl calls are used up)"),
        "{log}"
    );
}

fn answered() -> Value {
    json!({"answered": true, "answer": "Open 10am to 5pm on Sunday.", "sources": [1]})
}

fn fc_pages(credits: Option<i64>) -> Found {
    Found { pages: pages(), tools: Vec::new(), credits: credits.map(serde_json::Number::from) }
}

#[test]
fn cost_usd_is_a_number_only_when_every_part_is_priced() {
    let unpriced_model: Ask = Arc::new(|_: &Value| {
        Ok(json!({"choices": [{"message": {"content": serde_json::to_string(&answered()).unwrap()}}],
                  "usage": {"prompt_tokens": 800, "completion_tokens": 30}})) // no cost reported
    });
    let fc = |fx: &Fx, ask: Ask, found: Found| -> Value {
        let (per, _) = engine(PERPLEXITY_SAYS);
        let search: FirecrawlSearchFn = Arc::new(move |_: &str| Ok(found.clone()));
        let paths = Paths { perplexity: Some(per), firecrawl_search: Some(search), ask: Some(ask), ..Paths::default() };
        sr::answer(&fx.ctx, "q", &cfg(), &opts(paths, to(sr::FIRECRAWL)))
    };
    let unset = Fx::new(&[]);
    let out = fc(&unset, model(vec![answered()]).0, fc_pages(Some(4)));
    assert_eq!(keys_of(&out), shape()); // the same shape
    assert_eq!(out["cost_usd"], Value::Null); // credits with no price: unpriced, not the model's share
    let priced = Fx::new(&[("EVE_FIRECRAWL_USD", "0.004")]);
    let cost = fc(&priced, model(vec![answered()]).0, fc_pages(Some(4)))["cost_usd"].as_f64().unwrap();
    assert!((cost - (0.0002 + 4.0 * 0.004)).abs() < 1e-9);
    assert_eq!(fc(&priced, unpriced_model.clone(), fc_pages(Some(4)))["cost_usd"], Value::Null); // the model reported none
    assert_eq!(fc(&priced, model(vec![answered()]).0, fc_pages(None))["cost_usd"], Value::Null); // Firecrawl did not say
    let (per, _) = engine(PERPLEXITY_SAYS);
    let paths = Paths {
        perplexity: Some(per),
        read: Some(Arc::new(|_: &str| Ok(pages()))),
        ask: Some(unpriced_model),
        ..Paths::default()
    };
    let tf = sr::answer(&priced.ctx, "q", &cfg(), &opts(paths, to(sr::TINYFISH)));
    assert_eq!(keys_of(&tf), keys_of(&out));
    assert_eq!(tf["cost_usd"], Value::Null);
    assert_eq!(tf["usage"], json!({"in": 800, "out": 30}));
}

#[test]
fn a_firecrawl_route_runs_the_one_tool_the_model_picks_and_answers_from_it() {
    let fx = Fx::new(&[("EVE_FIRECRAWL_USD", "0.004")]);
    let tool: firecrawl::Tool = serde_json::from_value(json!({
        "provider": "g", "capability": "flights/search_flights", "name": "Search flights", "about": "Live fares.",
        "options": [{"name": "origin", "type": "string", "about": "Airport IATA code.", "required": true}]}))
    .unwrap();
    let found = Found { pages: pages(), tools: vec![tool], credits: Some(2.into()) };
    let search: FirecrawlSearchFn = Arc::new(move |_: &str| Ok(found.clone()));
    let ran_calls: Arc<Mutex<Vec<ToolCall>>> = Arc::default();
    let rc = ran_calls.clone();
    let run: ToolRunFn = Arc::new(move |call: &ToolCall| {
        rc.lock().unwrap().push(call.clone());
        Ok(firecrawl::Ran { data: json!({"cheapest": "$120"}), credits: Some(3.into()) })
    });
    let (ask, sent) = model(vec![
        json!({"answered": false, "answer": "", "sources": [], "tool": {"id": 1, "options": {"origin": "SFO"}}}),
        json!({"answered": true, "answer": "The cheapest is $120.", "sources": [1]}),
    ]);
    let progress: Arc<Mutex<Vec<String>>> = Arc::default();
    let pr = progress.clone();
    let paths = Paths { firecrawl_search: Some(search), run_tool: Some(run), ask: Some(ask), ..Paths::default() };
    let o = AnswerOpts { progress: Arc::new(move |t: &str| pr.lock().unwrap().push(t.to_string())), ..opts(paths, to(sr::FIRECRAWL)) };
    let out = sr::answer(&fx.ctx, "cheapest flight from SFO", &cfg(), &o);
    assert_eq!(out["answer"], "The cheapest is $120.", "{out}");
    assert_eq!(sent.lock().unwrap().len(), 2);
    assert_eq!(ran_calls.lock().unwrap().len(), 1);
    assert_eq!(*progress.lock().unwrap(), vec!["Checking Search flights…".to_string()]);
    // two model calls at 0.0002 and five credits at 0.004
    assert!((out["cost_usd"].as_f64().unwrap() - (0.0004 + 5.0 * 0.004)).abs() < 1e-9);
}

#[test]
fn usage_and_result_read_the_answer_models_calls() {
    assert_eq!(sr::usage(&[]), json!({"in": 0, "out": 0}));
    let payloads = [
        json!({"usage": {"prompt_tokens": 10, "completion_tokens": 2, "cost": 0.0001}}),
        json!({"usage": {"prompt_tokens": 5, "completion_tokens": 1, "cost": true}}),
    ];
    assert_eq!(sr::usage(&payloads), json!({"in": 15, "out": 3})); // a bool is not a cost
    let page = Page { url: "https://a.example".into(), title: " A \n page ".into(), markdown: "x".repeat(400) };
    let out = sr::result("answer", std::slice::from_ref(&page), &strings(&["https://a.example", "https://unread.example"]), &payloads[..1], Some(0.0));
    assert_eq!(out["sources"].as_array().unwrap().len(), 1);
    assert_eq!(out["sources"][0]["title"], "A page");
    assert_eq!(out["sources"][0]["snippet"].as_str().unwrap().len(), ws::MAX_SNIPPET_CHARS);
    assert_eq!(out["cost_usd"], 0.0001);
    assert_eq!(sr::result("a", &[], &[], &payloads[..1], None)["cost_usd"], Value::Null);
}

// ---- the fixtures ----------------------------------------------------------------------------------------------

fn routes_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../bridge/tests/fixtures/routes")
}

#[test]
fn the_tuning_set_is_labelled_and_mixed() {
    let data: Value = serde_json::from_str(&std::fs::read_to_string(routes_dir().join("search_tuning.json")).unwrap()).unwrap();
    let cases = data["cases"].as_array().unwrap();
    let n = cases.len() as f64;
    assert!(cases.len() >= 60 && data.get("reader_note").is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty())));
    assert!(cases.iter().all(|c| ALL.contains(&c["provider"].as_str().unwrap()) && !c["goal"].as_str().unwrap().trim().is_empty()));
    let count = |p: &str| cases.iter().filter(|c| c["provider"] == p).count() as f64;
    assert!((0.3..=0.5).contains(&(count(sr::TINYFISH) / n)) && (0.3..=0.5).contains(&(count(sr::PERPLEXITY) / n)));
    assert!((0.12..=0.3).contains(&(count(sr::FIRECRAWL) / n)));
    assert!(cases.iter().filter(|c| c.get("tricky").is_some_and(|t| t.as_bool().unwrap_or(false))).count() >= 10);
    let goals: Vec<String> = cases.iter().map(|c| c["goal"].as_str().unwrap().to_lowercase()).collect();
    let unique: HashSet<&String> = goals.iter().collect();
    assert_eq!(goals.len(), unique.len());
    let holdout = routes_dir().join("search_holdout.json");
    if holdout.exists() {
        // the blind set repeats no tuning request
        let blind: Value = serde_json::from_str(&std::fs::read_to_string(holdout).unwrap()).unwrap();
        for c in blind["cases"].as_array().unwrap() {
            assert!(!unique.contains(&c["goal"].as_str().unwrap().to_lowercase()));
        }
    }
}

#[test]
fn the_firecrawl_cap_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(firecrawl::COUNT_FILE);
    let with_count = || Ctx { firecrawl_calls: Arc::new(DayCount::at(&path)), ..env_of(&[("EVE_FIRECRAWL_PER_DAY", "3")]) };
    let now = 1.8e9;
    let ctx = with_count();
    assert!(firecrawl::take(&ctx, Some(now)) && firecrawl::take(&ctx, Some(now)));
    assert_eq!(firecrawl::left(&ctx, Some(now)), 1);
    let ctx = with_count(); // the daemon restarts the same day
    assert_eq!(firecrawl::left(&ctx, Some(now)), 1); // the day's two calls were kept
    assert!(firecrawl::take(&ctx, Some(now)) && !firecrawl::take(&ctx, Some(now)));
    let ctx = with_count();
    assert_eq!(firecrawl::left(&ctx, Some(now + 86400.0)), 3); // the next day starts fresh
    std::fs::write(&path, "not json").unwrap();
    let ctx = with_count();
    assert_eq!(firecrawl::left(&ctx, Some(now)), 3); // an unreadable file counts as nothing
}

#[test]
fn a_tool_that_needs_an_id_looked_up_first_is_not_offered() {
    let lookup = json!({"provider": "p", "capability": "flights/search_flights", "name": "Search", "options": [
        {"name": "destination_entity_id", "type": "string", "about": "Destination entity id.", "required": true}]});
    let plain = json!({"provider": "g", "capability": "flights/search_flights", "name": "Search", "options": [
        {"name": "origin", "type": "string", "required": true,
         "about": "Airport IATA code or Google city id returned by suggest_places."},
        {"name": "departure_date", "type": "string", "about": "YYYY-MM-DD", "required": true},
        {"name": "selection_id", "type": "string", "about": "a chosen outbound id", "required": false}]});
    assert!(firecrawl::tool(&lookup).is_none());
    assert!(firecrawl::tool(&plain).is_some()); // an optional id does not matter
}

// ================================================================================================================
// websearch (test_websearch.py)
// ================================================================================================================

fn payload() -> Value {
    json!({
        "choices": [{"message": {
            "role": "assistant",
            "content": "  Warriors beat the Lakers 112 to 104 last night,\n per ESPN.  ",
            "annotations": [
                {"type": "url_citation", "url_citation": {"url": "https://espn.com/x", "title": "Warriors 112, Lakers 104",
                                                          "content": format!("Curry scored 31 {}", "z".repeat(400))}},
                {"type": "url_citation", "url_citation": {"url": "https://espn.com/x", "title": "dup", "content": ""}},
                {"type": "url_citation", "url_citation": {"url": "https://nba.com/y", "title": "Box score"}},
                {"type": "other"},
            ]}}],
        "usage": {"prompt_tokens": 1200, "completion_tokens": 60},
    })
}

fn reply(body: &Value) -> Result<Vec<u8>, OpenError> {
    Ok(serde_json::to_vec(body).unwrap())
}

fn opener(f: impl Fn(&OpenRequest, f64) -> Result<Vec<u8>, OpenError> + Send + Sync + 'static) -> Option<Arc<dyn Opener>> {
    Some(Arc::new(f))
}

fn engine_of(req: &OpenRequest) -> String {
    let body: Value = serde_json::from_slice(&req.body).unwrap();
    body["tools"][0]["parameters"]["engine"].as_str().unwrap().to_string()
}

fn eo(key: &str, open: Option<Arc<dyn Opener>>) -> EngineOpts {
    EngineOpts { key: key.into(), opener: open, ..EngineOpts::default() }
}

fn perplexity_cfg() -> SearchConfig {
    SearchConfig { engine: "openrouter-perplexity".into(), ..SearchConfig::default() }
}

#[test]
fn configured_uses_perplexity_with_an_openrouter_key_and_no_search_without() {
    let cfg = ws::configured(&env_of(&[("OPENROUTER_API_KEY", "r")]));
    assert_eq!(
        cfg,
        SearchConfig {
            engine: "openrouter-perplexity".into(),
            model: "openai/gpt-6-luna".into(),
            max_results: 5,
            max_uses: 2,
            ..SearchConfig::default()
        }
    );
    assert_eq!(ws::tools_for(&cfg), vec![ws::WEB_SEARCH_TOOL.clone()]);
    // without an OpenRouter key there is no brain to search for (2026-10-08): off, never OpenAI's hosted tool
    for env in [
        vec![],
        vec![("EVE_WEB_SEARCH", "openrouter-perplexity")],
        vec![("EVE_WEB_SEARCH", "openrouter-exa")],
        vec![("OPENAI_API_KEY", "k")],
    ] {
        let cfg = ws::configured(&env_of(&env));
        assert!(cfg.engine == "off" && ws::tools_for(&cfg).is_empty());
    }
    let cfg = ws::configured(&env_of(&[("OPENROUTER_API_KEY", "r"), ("EVE_WEB_SEARCH", "openai")])); // explicit only
    assert!(cfg.engine == "openai" && ws::tools_for(&cfg) == vec![json!({"type": "web_search"})]);
    assert_eq!(ws::configured(&env_of(&[("OPENROUTER_API_KEY", "r"), ("EVE_WEB_SEARCH", "bing")])).engine, "openrouter-perplexity");
    assert_eq!(ws::configured(&env_of(&[("OPENROUTER_API_KEY", "r"), ("EVE_WEB_SEARCH", "openrouter-exa")])).engine, "openrouter-exa");
    assert_eq!(SearchConfig::default().engine, "openai"); // a bare config never needs an OpenRouter key
    assert_eq!(ws::WEB_SEARCH_TOOL["strict"], true);
    assert_eq!(ws::WEB_SEARCH_TOOL["parameters"]["additionalProperties"], false);
    assert!(ws::tools_for(&ws::configured(&env_of(&[("EVE_WEB_SEARCH", "off"), ("OPENROUTER_API_KEY", "r")]))).is_empty());
    let tuned = ws::configured(&env_of(&[
        ("OPENROUTER_API_KEY", "r"),
        ("EVE_WEB_SEARCH_MODEL", "z-ai/glm-5.3-flash"),
        ("EVE_WEB_SEARCH_RESULTS", "40"),
        ("EVE_WEB_SEARCH_MAX_USES", "9"),
    ]));
    assert_eq!((tuned.model.as_str(), tuned.max_results, tuned.max_uses), ("z-ai/glm-5.3-flash", 10, 5));
    let odd = ws::configured(&env_of(&[("OPENROUTER_API_KEY", "r"), ("EVE_WEB_SEARCH_RESULTS", "many"), ("EVE_WEB_SEARCH_MAX_USES", " 0 ")]));
    assert_eq!((odd.max_results, odd.max_uses), (5, 1));
}

#[test]
fn an_openai_answer_model_is_allowed_through_openrouter() {
    // 2026-09-24 the owner kept OpenAI models off OpenRouter; 2026-10-08 he moved every model call onto it
    let cfg = ws::configured(&env_of(&[("OPENROUTER_API_KEY", "r"), ("EVE_WEB_SEARCH_MODEL", "openai/gpt-6-luna")]));
    assert_eq!(cfg.model, "openai/gpt-6-luna");
}

#[test]
fn the_request_is_one_chat_call_with_the_search_server_tool_and_the_clock() {
    let now: DateTime<FixedOffset> = Utc.with_ymd_and_hms(2026, 9, 24, 18, 36, 0).unwrap().fixed_offset();
    let config = SearchConfig { engine: "openrouter-perplexity".into(), max_results: 3, max_uses: 2, ..SearchConfig::default() };
    let body = ws::request(&env_of(&[("EVE_TIMEZONE", "America/Los_Angeles")]), &config, "  who won\n the game ", Some(now), ws::SYSTEM);
    assert!(body["model"] == "openai/gpt-6-luna" && body.get("plugins").is_none());
    assert_eq!(
        body["tools"],
        json!([{"type": "openrouter:web_search", "parameters": {"engine": "perplexity", "max_results": 3, "max_uses": 2}}])
    );
    assert_eq!(body["messages"][1], json!({"role": "user", "content": "who won the game"}));
    let system = body["messages"][0]["content"].as_str().unwrap();
    assert!(body["messages"][0]["role"] == "system" && body["max_tokens"] == 2048 && body["usage"] == json!({"include": true}));
    // the answer step reads the local clock, converted to the owner's zone: pages are not a live clock
    assert!(system.contains("Thursday 2026-09-24 11:36") && system.contains("America/Los_Angeles"), "{system}");
    assert!(system.starts_with(ws::SYSTEM));
    let exa = ws::request(&env_of(&[]), &SearchConfig { engine: "openrouter-exa".into(), ..SearchConfig::default() }, "x", Some(now), ws::SYSTEM);
    assert_eq!(exa["tools"][0]["parameters"]["engine"], "exa");
    // an unknown zone is the machine's own
    let local = ws::clock(&env_of(&[("EVE_TIMEZONE", "Mars/Olympus")]), Some(now));
    assert!(!local.contains("Mars/Olympus"));
}

#[test]
fn parse_reads_the_answer_and_the_citations_once_each() {
    let out = ws::parse(&payload()).unwrap();
    assert!(out["ok"] == true && out["answer"] == "Warriors beat the Lakers 112 to 104 last night, per ESPN.");
    let urls: Vec<&str> = out["sources"].as_array().unwrap().iter().map(|s| s["url"].as_str().unwrap()).collect();
    assert_eq!(urls, vec!["https://espn.com/x", "https://nba.com/y"]); // deduplicated
    assert_eq!(out["sources"][0]["title"], "Warriors 112, Lakers 104");
    assert_eq!(out["sources"][0]["snippet"].as_str().unwrap().chars().count(), ws::MAX_SNIPPET_CHARS);
    assert_eq!(out["sources"][1]["snippet"], "");
    assert_eq!(out["usage"], json!({"in": 1200, "out": 60}));
    // no annotations is still an answer; no answer and no source is not
    assert_eq!(ws::parse(&json!({"choices": [{"message": {"content": "It is 3pm."}}]})).unwrap()["sources"], json!([]));
    assert!(ws::parse(&json!({"choices": [{"message": {"content": "   "}}]})).is_err());
    assert!(ws::parse(&json!({"error": "x"})).is_err());
}

#[test]
fn search_posts_once_and_never_raises() {
    // No key on this machine, whatever the shell has: with one, the no-key case made a real, paid search.
    let fx = Fx::new(&[]);
    let seen: Arc<Mutex<Vec<(String, Value, f64)>>> = Arc::default();
    let s = seen.clone();
    let open = opener(move |req: &OpenRequest, timeout: f64| {
        s.lock().unwrap().push((req.url.clone(), serde_json::from_slice(&req.body).unwrap(), timeout));
        assert_eq!(req.header("Authorization"), Some("Bearer r"));
        reply(&payload())
    });
    let config = perplexity_cfg();
    let out = ws::search(&fx.ctx, "who won", Some(&config), &EngineOpts { clock: ticks(&[0.0, 0.42]), ..eo("r", open) });
    // no usage.cost in the reply: the engine's list price per search
    assert!(out["ok"] == true && out["ms"] == 420 && out["cost_usd"] == 0.005 && out["sources"].as_array().unwrap().len() == 2);
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].0, ws::URL);
        assert_eq!(seen[0].1["tools"][0]["parameters"]["engine"], "perplexity");
        assert_eq!(seen[0].2, ws::DEFAULT_TIMEOUT_SECS);
    }
    let priced = opener(|_: &OpenRequest, _: f64| {
        let mut p = payload();
        p["usage"]["cost"] = json!(0.0056);
        reply(&p)
    });
    assert_eq!(ws::search(&fx.ctx, "who won", Some(&config), &eo("r", priced))["cost_usd"], 0.0056); // OpenRouter's own figure
    // failures are one reason each
    assert_eq!(
        ws::search(&fx.ctx, "x", None, &eo("", None)),
        json!({"ok": false, "reason": "web search is not set up on this computer (no OpenRouter key)"})
    );
    assert_eq!(ws::search(&fx.ctx, "   ", None, &eo("r", None)), json!({"ok": false, "reason": "empty query"}));
    let http_error = opener(|_: &OpenRequest, _: f64| Err(OpenError::Http(429)));
    assert_eq!(
        ws::search(&fx.ctx, "x", Some(&config), &eo("r", http_error)),
        json!({"ok": false, "reason": "the search service answered HTTP 429"})
    );
    let unreachable = opener(|_: &OpenRequest, _: f64| Err(OpenError::Url("OSError".into())));
    assert_eq!(ws::search(&fx.ctx, "x", Some(&config), &eo("r", unreachable))["reason"], "the search service is unreachable");
    let garbage = opener(|_: &OpenRequest, _: f64| reply(&json!({"choices": []})));
    assert_eq!(ws::search(&fx.ctx, "x", Some(&config), &eo("r", garbage))["reason"], "the search service sent an unreadable reply");
    let slow = opener(|_: &OpenRequest, _: f64| Err(OpenError::Timeout));
    assert_eq!(ws::search(&fx.ctx, "x", Some(&config), &eo("r", slow))["reason"], "the search timed out");
}

#[test]
fn an_engine_search_writes_one_spend_line() {
    let fx = Fx::new(&[]);
    let open = opener(|_: &OpenRequest, _: f64| reply(&payload()));
    ws::search(&fx.ctx, "who won", Some(&perplexity_cfg()), &eo("r", open));
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let rows = fx.ctx.ledger.day_rows(&today);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["p"], "openrouter");
    assert_eq!(rows[0]["f"], eve_spend::SEARCH);
    assert_eq!(rows[0]["usd"], Value::Null);
}

// ---- routed (search_router): the same tool, the same result, whichever provider answers ------------------------

const ROUTER_KEYS: [(&str, &str); 5] = [
    ("OPENROUTER_API_KEY", "r"),
    ("TYPESAFE_API_KEY", "ts-test"),
    ("TINYFISH_API_KEY", "tf-test"),
    ("FIRECRAWL_API_KEY", "fc-test"),
    ("EVE_SEARCH_ROUTER", "on"),
];

fn router_keys(set: &[(&'static str, &'static str)], without: &[&str]) -> Ctx {
    let mut map: HashMap<&str, &str> = ROUTER_KEYS.iter().copied().filter(|(k, _)| !without.contains(k)).collect();
    for (k, v) in set {
        map.insert(k, v);
    }
    env_of(&map.into_iter().collect::<Vec<_>>())
}

#[test]
fn configured_lists_the_routed_providers_and_the_tool_says_what_they_reach() {
    // "on" routes whether or not the gates shipped
    let cfg = ws::configured(&router_keys(&[], &[]));
    assert_eq!(cfg.providers, strings(&["perplexity", "tinyfish", "firecrawl"]));
    let tool = &ws::tools_for(&cfg)[0];
    assert!(tool["name"] == ws::TOOL_NAME && tool["parameters"] == ws::WEB_SEARCH_TOOL["parameters"]);
    let description = tool["description"].as_str().unwrap();
    assert!(description.contains("many-source") && description.contains("flight fares"));
    let no_fc = ws::configured(&router_keys(&[], &["FIRECRAWL_API_KEY"]));
    assert!(!ws::tools_for(&no_fc)[0]["description"].as_str().unwrap().contains("flight fares")); // never a reach it lacks
    // the off switch always, and auto until the gates ship: the engine alone, the tool exactly as before
    let off = ws::configured(&router_keys(&[("EVE_SEARCH_ROUTER", "off")], &[]));
    assert!(off.providers.is_empty() && ws::tools_for(&off) == vec![ws::WEB_SEARCH_TOOL.clone()]);
    let auto_ctx = router_keys(&[("EVE_SEARCH_ROUTER", "auto")], &[]);
    let auto = ws::configured(&auto_ctx);
    const { assert!(sr::SHIPPED) };
    assert_eq!(auto.providers, all()); // shipped
    assert_ne!(ws::tools_for(&auto), vec![ws::WEB_SEARCH_TOOL.clone()]);
    assert!(sr::available_shipped(&auto_ctx, &auto.engine, false).is_empty()); // not shipped: the engine alone
    assert!(ws::configured(&router_keys(&[("EVE_WEB_SEARCH", "openai")], &[])).providers.is_empty());
    assert_eq!(
        ws::routed_tool(&all())["description"],
        format!("{} {}; {}; {}.", ws::WEB_SEARCH_TOOL["description"].as_str().unwrap(), ws::ROUTED_NOTE, ws::ROUTED_REACH[0].1, ws::ROUTED_REACH[1].1)
    );
}

#[test]
fn search_keeps_its_shape_and_cost_fields_for_every_provider() {
    for provider in ["tinyfish", "firecrawl", "perplexity"] {
        // priced, so every provider's cost is a number
        let fx = Fx::new(&[("EVE_FIRECRAWL_USD", "0.004")]);
        let ran: Arc<Mutex<Vec<String>>> = Arc::default();
        let page = Page { url: "https://p.example/a".into(), title: "Page A".into(), markdown: "The fact is 42.".into() };
        let (r1, r2, p1, p2) = (ran.clone(), ran.clone(), page.clone(), page);
        let ask: Ask = Arc::new(|_: &Value| {
            Ok(json!({"choices": [{"message": {"content": serde_json::to_string(&json!({"answered": true, "answer": "42.", "sources": [1]})).unwrap()}}],
                      "usage": {"prompt_tokens": 700, "completion_tokens": 20, "cost": 0.0001}}))
        });
        let paths = Paths {
            read: Some(Arc::new(move |_: &str| {
                r1.lock().unwrap().push("tinyfish".into());
                Ok(vec![p1.clone()])
            })),
            firecrawl_search: Some(Arc::new(move |_: &str| {
                r2.lock().unwrap().push("firecrawl".into());
                Ok(Found { pages: vec![p2.clone()], tools: Vec::new(), credits: Some(4.into()) })
            })),
            ask: Some(ask),
            ..Paths::default()
        };
        let engine_calls: Arc<Mutex<Vec<String>>> = Arc::default();
        let ec = engine_calls.clone();
        let open = opener(move |req: &OpenRequest, _: f64| {
            ec.lock().unwrap().push(req.url.clone());
            reply(&payload())
        });
        let route: RouteFn = Arc::new(move |_: &str, _: &[String]| {
            Ok(Route { provider: provider.into(), p: 0.9, ms: 150.0, error: String::new() })
        });
        let o = AnswerOpts { key: "r".into(), opener: open, ..opts(paths, route) };
        let out = sr::answer(&fx.ctx, "what is the answer", &cfg(), &o);
        assert_eq!(keys_of(&out), shape(), "{provider}: {out}");
        assert_eq!(out["ok"], true);
        let sources = out["sources"].as_array().unwrap();
        assert!(!sources.is_empty());
        assert!(sources.iter().all(|s| keys_of(s) == strings(&["title", "url", "snippet"]).into_iter().collect()));
        assert!(out["usage"].get("in").is_some() && out["usage"].get("out").is_some() && out["cost_usd"].is_f64());
        let expect_ran = if provider != "perplexity" { vec![provider.to_string()] } else { Vec::new() };
        assert_eq!(*ran.lock().unwrap(), expect_ran);
        let expect_engine = if provider == "perplexity" { vec![ws::URL.to_string()] } else { Vec::new() };
        assert_eq!(*engine_calls.lock().unwrap(), expect_engine);
    }
}

#[test]
fn the_off_switch_is_todays_engine_search_exactly() {
    let fx = Fx::new(&[]);
    let off_ctx = Ctx { env: router_keys(&[("EVE_SEARCH_ROUTER", "off")], &[]).env, ..fx.ctx.clone() };
    let cfg = ws::configured(&off_ctx);
    let open = opener(|_: &OpenRequest, _: f64| reply(&payload()));
    let out = ws::search(&fx.ctx, "who won", Some(&cfg), &EngineOpts { clock: ticks(&[0.0, 0.42]), ..eo("r", open) });
    assert!(out["ms"] == 420 && out["cost_usd"] == 0.005);
    assert_eq!(out["answer"], "Warriors beat the Lakers 112 to 104 last night, per ESPN.");
}

#[test]
fn a_hung_or_unreachable_engine_search_is_tried_once_on_the_other_engine() {
    // 2026-10-08: one Perplexity search hung 20 s and the owner was told the search timed out.
    let fx = Fx::new(&[]);
    let config = perplexity_cfg();
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let s = seen.clone();
    let first_hangs = opener(move |req: &OpenRequest, _: f64| {
        let engine = engine_of(req);
        s.lock().unwrap().push(engine.clone());
        if engine == "perplexity" {
            return Err(OpenError::Timeout);
        }
        reply(&payload())
    });
    let out = ws::search(&fx.ctx, "who is president", Some(&config), &eo("r", first_hangs));
    assert!(out["ok"] == true && out["fallback"] == "openrouter-exa");
    assert!(out.get("transient").is_none() && out.get("failed_ms").is_none());
    assert_eq!(*seen.lock().unwrap(), strings(&["perplexity", "exa"]));

    seen.lock().unwrap().clear();
    let s = seen.clone();
    let both_fail = opener(move |_: &OpenRequest, _: f64| {
        s.lock().unwrap().push("x".into());
        Err(OpenError::Url("OSError".into()))
    });
    assert_eq!(
        ws::search(&fx.ctx, "x", Some(&config), &eo("r", both_fail)),
        json!({"ok": false, "reason": "the search service is unreachable"})
    );
    assert_eq!(seen.lock().unwrap().len(), 2); // one more try, never a loop

    seen.lock().unwrap().clear();
    let s = seen.clone();
    let bad_request = opener(move |_: &OpenRequest, _: f64| {
        s.lock().unwrap().push("x".into());
        Err(OpenError::Http(400))
    });
    assert_eq!(ws::search(&fx.ctx, "x", Some(&config), &eo("r", bad_request))["reason"], "the search service answered HTTP 400");
    assert_eq!(seen.lock().unwrap().len(), 1); // a 400 is not tried again
}

#[test]
fn a_slow_engine_is_raced_by_the_other_and_the_first_answer_wins() {
    // 2026-10-08 evening: both engines took 15-27 s; a 12 s limit failed every search the old 20 s one answered.
    let fx = Fx::new(&[]);
    let config = SearchConfig { timeout_secs: 5.0, ..perplexity_cfg() };
    let started: Arc<Mutex<Vec<String>>> = Arc::default();
    let release = Arc::new(AtomicBool::new(false));
    let (s, r) = (started.clone(), release.clone());
    let perplexity_slow = opener(move |req: &OpenRequest, _: f64| {
        let engine = engine_of(req);
        s.lock().unwrap().push(engine.clone());
        if engine == "perplexity" {
            // slow, but would answer
            let t0 = Instant::now();
            while !r.load(Ordering::SeqCst) && t0.elapsed() < Duration::from_secs(2) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        reply(&payload())
    });
    let out = ws::search(&fx.ctx, "concerts in seattle", Some(&config), &EngineOpts { hedge_secs: 0.05, ..eo("r", perplexity_slow) });
    release.store(true, Ordering::SeqCst);
    assert!(out["ok"] == true && out["fallback"] == "openrouter-exa");
    assert_eq!(*started.lock().unwrap(), strings(&["perplexity", "exa"]));
}

#[test]
fn a_fast_engine_never_starts_the_other() {
    let fx = Fx::new(&[]);
    let started: Arc<Mutex<Vec<String>>> = Arc::default();
    let s = started.clone();
    let fast = opener(move |req: &OpenRequest, _: f64| {
        s.lock().unwrap().push(engine_of(req));
        reply(&payload())
    });
    let out = ws::search(&fx.ctx, "who won", Some(&perplexity_cfg()), &eo("r", fast));
    assert!(out["ok"] == true && out.get("fallback").is_none());
    assert_eq!(*started.lock().unwrap(), strings(&["perplexity"]));
    const { assert!(ws::DEFAULT_TIMEOUT_SECS >= 20.0) }; // never below what Telegram had
}

#[test]
fn the_whole_search_gives_up_at_its_budget() {
    let fx = Fx::new(&[]);
    let config = SearchConfig { timeout_secs: 0.3, ..perplexity_cfg() };
    let stop = Arc::new(AtomicBool::new(false));
    let st = stop.clone();
    let both_hang = opener(move |_: &OpenRequest, _: f64| {
        let t0 = Instant::now();
        while !st.load(Ordering::SeqCst) && t0.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(5));
        }
        reply(&payload())
    });
    let t0 = Instant::now();
    let out = ws::search(&fx.ctx, "x", Some(&config), &EngineOpts { hedge_secs: 0.05, ..eo("r", both_hang) });
    stop.store(true, Ordering::SeqCst);
    assert_eq!(out, json!({"ok": false, "reason": "the search timed out"}));
    assert!(t0.elapsed() < Duration::from_millis(1500));
}

#[test]
fn answer_options_asks_mercury_for_low_reasoning_with_room() {
    // Mercury reasons by default; without the low effort and the room it can spend the budget before the answer
    let opts = ws::answer_options("inception/mercury-3", 400);
    assert_eq!(Value::Object(opts), json!({"max_tokens": 1024, "reasoning": {"effort": "low"}}));
    let body = ws::request(
        &env_of(&[]),
        &SearchConfig { engine: "openrouter-perplexity".into(), model: "inception/mercury-3".into(), ..SearchConfig::default() },
        "q",
        None,
        ws::SYSTEM,
    );
    assert!(body.get("temperature").is_none());
    assert_eq!(Value::Object(ws::answer_options("deepseek/x", 400)), json!({"max_tokens": 400, "temperature": 0}));
    let gemini = ws::request(
        &env_of(&[]),
        &SearchConfig { model: "google/gemini-3.8-flash".into(), ..SearchConfig::default() },
        "q",
        None,
        ws::SYSTEM,
    );
    let keys: Vec<&String> = gemini.as_object().unwrap().keys().collect();
    assert_eq!(keys, vec!["model", "tools", "messages", "max_tokens", "temperature", "reasoning", "usage", "provider"]);
}
