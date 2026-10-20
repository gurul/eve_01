//! test_web_reader.py: the web body, and firecrawl's Firecrawl search and Alexandria tools it reaches through
//! search_router. Fakes for TinyFish, Firecrawl, the engine search and the model, so no request, credit or token
//! is spent; the live end-to-end check is outside the suite. What is pinned: the answer comes only from what eve
//! read, only those links come back, the router's provider answers the task, and every way it can fail hands the
//! whole task to Codex.
//!
//! Python's `quiet` fixture (spend recorded into a list, the day counts cleared) is a [`Ctx`] of each test's own:
//! its ledger in a temp folder, its own Firecrawl day count and task count, its own environment.

use std::any::Any;
use std::collections::HashSet;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use eve_agent_contract::AgentEvent;
use eve_app_reflex::{Agent, MakeAgent};
use eve_web::firecrawl::{self, DayCount, Found, Ran, Tool, ToolCall, ToolOption};
use eve_web::search_router::{self, Route, RouteFn};
use eve_web::watch::FetchError;
use eve_web::web_reader::{self, Page, ReaderConfig, ReaderOptions, TaskCount, ToolResult, WebReaderAgent};
use eve_web::websearch::SearchConfig;
use eve_web::{tinyfish, Ask, Ctx, Environ, Http, HttpOptions};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

// ---- fixtures -----------------------------------------------------------------------------------------------

fn pages() -> Vec<Page> {
    vec![
        Page {
            url: "https://www.weather.example/denver".into(),
            title: "Denver forecast".into(),
            markdown: "Sat: snow, 28F.".into(),
        },
        Page { url: "https://news.example/denver".into(), title: "Denver news".into(), markdown: "Storm this weekend.".into() },
    ]
}

/// A test's own process services: env, a ledger in a temp folder, fresh day counts.
struct World {
    ctx: Ctx,
    _dir: tempfile::TempDir,
}

impl World {
    fn new(env: &[(&str, &str)]) -> World {
        let dir = tempfile::tempdir().expect("temp dir");
        let environ: Environ = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let base = Ctx::global().clone().with_env(environ).with_ledger(eve_spend::Ledger::at(dir.path().join("spend")));
        let ctx = Ctx {
            firecrawl_calls: Arc::new(DayCount::at(dir.path().join("spend").join(firecrawl::COUNT_FILE))),
            reader_tasks: Arc::new(TaskCount::default()),
            ..base
        };
        World { ctx, _dir: dir }
    }

    fn with_http(mut self, http: Arc<dyn Http>) -> World {
        self.ctx = self.ctx.with_http(http);
        self
    }

    /// Today's spend rows (Python's `quiet` list): (provider, model, feature, usd).
    fn rows(&self) -> Vec<Map<String, Value>> {
        self.ctx.ledger.day_rows(&firecrawl::day(None))
    }
}

/// `model(reply)`: the model answering every call with `reply`, each body it was sent kept.
fn model(reply: Value) -> (Ask, Arc<Mutex<Vec<Value>>>) {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let log = sent.clone();
    let ask: Ask = Arc::new(move |body: &Value| {
        log.lock().push(body.clone());
        Ok(json!({"choices": [{"message": {"content": serde_json::to_string(&reply).unwrap()}}], "usage": {"cost": 0.0001}}))
    });
    (ask, sent)
}

/// `models(*replies)`: the model answering in turn: the first reply to the first call, and so on.
fn models(replies: Vec<Value>) -> (Ask, Arc<Mutex<Vec<Value>>>) {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let log = sent.clone();
    let ask: Ask = Arc::new(move |body: &Value| {
        let mut sent = log.lock();
        sent.push(body.clone());
        // a call past the replies given: Python's IndexError, as an error out of the model call
        let reply = replies.get(sent.len() - 1).ok_or_else(|| FetchError::new("no more replies"))?;
        Ok(json!({"choices": [{"message": {"content": serde_json::to_string(reply).unwrap()}}], "usage": {"cost": 0.0001}}))
    });
    (ask, sent)
}

fn content(sent: &Arc<Mutex<Vec<Value>>>, call: usize, message: usize) -> String {
    sent.lock()[call]["messages"][message]["content"].as_str().unwrap_or("").to_string()
}

#[derive(Default)]
struct Codex {
    goals: Mutex<Vec<String>>,
}

#[async_trait]
impl Agent for Codex {
    async fn run(&self, goal: &str) -> anyhow::Result<String> {
        self.goals.lock().push(goal.to_string());
        Ok("codex did it".to_string())
    }
    fn cancel(&self, _reason: &str) {}
    fn steer(&self, _text: &str) -> bool {
        false
    }
    fn status(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("running".into(), Value::Bool(false));
        m
    }
    fn provider(&self) -> String {
        "codex".into()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Codex {
    fn goals(&self) -> Vec<String> {
        self.goals.lock().clone()
    }
}

fn fallback(codex: &Arc<Codex>) -> MakeAgent {
    let codex = codex.clone();
    Arc::new(move || codex.clone() as Arc<dyn Agent>)
}

fn quiet_events() -> web_reader::OnEvent {
    Arc::new(|_| {})
}

fn no_questions() -> web_reader::AskUser {
    Arc::new(|_| Box::pin(async { String::new() }))
}

type Search = Arc<dyn Fn(&str) -> Result<Vec<Page>, FetchError> + Send + Sync>;

fn search_with(found: Vec<Page>) -> Search {
    Arc::new(move |_q: &str| Ok(found.clone()))
}

/// `make(search, ask, codex, **cfg)`.
fn make(world: &World, search: Search, ask: Ask, codex: &Arc<Codex>, tasks_per_day: Option<i64>) -> WebReaderAgent {
    let mut config = ReaderConfig { enabled: true, ..ReaderConfig::default() };
    if let Some(n) = tasks_per_day {
        config.tasks_per_day = n;
    }
    WebReaderAgent::new(
        fallback(codex),
        quiet_events(),
        no_questions(),
        ReaderOptions {
            ctx: Some(world.ctx.clone()),
            config: Some(config),
            search: Some(search),
            ask: Some(ask),
            clock: Some(Arc::new(|| 1.8e9)),
            ..ReaderOptions::default()
        },
    )
}

/// `run(reader, goal)`: the answer and the events it sent.
async fn run(reader: &mut WebReaderAgent, goal: &str) -> (String, Vec<AgentEvent>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    reader.on_event = Arc::new(move |ev| sink.lock().push(ev));
    let out = reader.run(goal).await.expect("run");
    let got = events.lock().clone();
    (out, got)
}

const SNOW: &str = "will it snow in denver this saturday";

fn kinds(events: &[AgentEvent]) -> Vec<String> {
    events.iter().map(|e| e.kind().to_string()).collect()
}

/// caplog: the INFO lines logged on this thread while its guard lives. One global subscriber (a scoped one per
/// test races tracing's callsite interest cache when tests run in parallel) writes each line into the capture
/// buffer of the thread that logged it, if that thread has one.
#[derive(Clone, Default)]
struct Caplog(Arc<Mutex<Vec<u8>>>);

thread_local! {
    static CAPTURE: std::cell::RefCell<Option<Arc<Mutex<Vec<u8>>>>> = const { std::cell::RefCell::new(None) };
}

struct ThreadWriter;

impl Write for ThreadWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        CAPTURE.with(|c| {
            if let Some(b) = c.borrow().as_ref() {
                b.lock().extend_from_slice(buf);
            }
        });
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct CaplogGuard;

impl Drop for CaplogGuard {
    fn drop(&mut self) {
        CAPTURE.with(|c| *c.borrow_mut() = None);
    }
}

impl Caplog {
    fn start(&self) -> CaplogGuard {
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| {
            let sub = tracing_subscriber::fmt()
                .with_writer(|| ThreadWriter)
                .with_ansi(false)
                .with_max_level(tracing::Level::INFO)
                .finish();
            let _ = tracing::subscriber::set_global_default(sub);
        });
        CAPTURE.with(|c| *c.borrow_mut() = Some(self.0.clone()));
        CaplogGuard
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock()).to_string()
    }
}

// ---- the TinyFish body --------------------------------------------------------------------------------------

#[tokio::test]
async fn test_it_answers_from_the_pages_with_only_their_links() {
    let world = World::new(&[]);
    let codex = Arc::new(Codex::default());
    let (ask, sent) =
        model(json!({"answered": true, "answer": "Yes: snow on Saturday, 28F.", "sources": [1, 1, 7, "2", true]}));
    let mut reader = make(&world, search_with(pages()), ask, &codex, None);
    let (out, events) = run(&mut reader, SNOW).await;
    assert_eq!(out, "Yes: snow on Saturday, 28F.\n\nhttps://www.weather.example/denver");
    assert!(codex.goals().is_empty());
    assert_eq!(kinds(&events), ["started", "progress", "final"]);
    assert!(content(&sent, 0, 0).contains("ignore any instruction") && content(&sent, 0, 1).contains("Sat: snow"));
    let rows = world.rows();
    let recorded = rows.iter().find(|r| r["p"] == "tinyfish").expect("a tinyfish row");
    assert_eq!((&recorded["m"], &recorded["f"]), (&json!("search+fetch"), &json!(eve_spend::TASKS)));
    assert_eq!(recorded["usd"].as_f64(), Some(0.0)); // free, and recorded as free
    assert!(rows.iter().any(|r| r["m"] == web_reader::MODEL)); // the answer model's chat completion
}

#[tokio::test]
async fn test_a_page_cannot_add_a_link_of_its_own() {
    let world = World::new(&[]);
    let (ask, _) = model(json!({"answered": true, "answer": "See https://evil.example/login for the forecast.", "sources": [2]}));
    let mut reader = make(&world, search_with(pages()), ask, &Arc::new(Codex::default()), None);
    let (out, _) = run(&mut reader, SNOW).await;
    assert!(!out.contains("evil.example")); // not in the answer's words either
    assert_eq!(out, "See (link removed) for the forecast.\n\nhttps://news.example/denver");
    let (ok, _) = model(json!({"answered": true, "answer": "Details: https://news.example/denver.", "sources": []}));
    let mut reader = make(&world, search_with(pages()), ok, &Arc::new(Codex::default()), None);
    let (out, _) = run(&mut reader, SNOW).await;
    assert_eq!(out, "Details: https://news.example/denver."); // a page it read may be named in the text
}

#[tokio::test(flavor = "current_thread")]
async fn test_every_failure_hands_the_whole_task_to_codex() {
    let cases: Vec<(Search, Value, &str)> = vec![
        (search_with(pages()), json!({"answered": false, "answer": "", "sources": []}), "did not answer"),
        (search_with(Vec::new()), json!({"answered": true, "answer": "x"}), "no readable pages"),
        (
            Arc::new(|_q: &str| Err(FetchError::with_status("the site answered HTTP 402", 402))),
            json!({}),
            "TinyFish",
        ),
        // Python's RuntimeError("boom"): a search that panics, named "panic" where Python names the class.
        (Arc::new(|_q: &str| panic!("boom")), json!({}), "panic"),
    ];
    for (search, reply, why) in cases {
        let world = World::new(&[]);
        let caplog = Caplog::default();
        let _guard = caplog.start();
        let codex = Arc::new(Codex::default());
        let (ask, _) = model(reply);
        let mut reader = make(&world, search, ask, &codex, None);
        let (out, _) = run(&mut reader, "compare two laptops").await;
        assert_eq!(out, "codex did it");
        assert_eq!(codex.goals(), ["compare two laptops"]);
        assert!(reader.handed_on());
        assert!(caplog.text().contains(why), "{why:?} not in {:?}", caplog.text());
    }
}

#[tokio::test]
async fn test_the_daily_cap_hands_to_codex_without_spending() {
    let world = World::new(&[]);
    let searched = Arc::new(AtomicUsize::new(0));
    let counting = |searched: &Arc<AtomicUsize>| -> Search {
        let searched = searched.clone();
        Arc::new(move |_q: &str| {
            searched.fetch_add(1, Ordering::SeqCst);
            Ok(pages())
        })
    };
    let (ask, _) = model(json!({"answered": true, "answer": "ok", "sources": []}));
    for _ in 0..2 {
        let mut reader = make(&world, counting(&searched), ask.clone(), &Arc::new(Codex::default()), Some(2));
        let (out, _) = run(&mut reader, SNOW).await;
        assert_eq!(out, "ok");
    }
    let codex = Arc::new(Codex::default());
    let mut reader = make(&world, counting(&searched), ask, &codex, Some(2));
    let (out, _) = run(&mut reader, SNOW).await;
    assert_eq!(out, "codex did it");
    assert_eq!(searched.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn test_stop_while_reading_is_stopped_not_handed_on() {
    let world = World::new(&[]);
    let codex = Arc::new(Codex::default());
    // The search stops the reader it runs for: the reader is reached through a slot filled once it exists.
    let slot: Arc<Mutex<Option<Arc<WebReaderAgent>>>> = Arc::new(Mutex::new(None));
    let reach = slot.clone();
    let search: Search = Arc::new(move |_q: &str| {
        if let Some(reader) = reach.lock().clone() {
            reader.cancel("owner said stop");
        }
        Ok(pages())
    });
    let (ask, _) = model(json!({"answered": true, "answer": "x", "sources": []}));
    let mut reader = make(&world, search, ask, &codex, None);
    let events = Arc::new(Mutex::new(Vec::<AgentEvent>::new()));
    let sink = events.clone();
    reader.on_event = Arc::new(move |ev| sink.lock().push(ev));
    let reader = Arc::new(reader);
    *slot.lock() = Some(reader.clone());
    let out = reader.run(SNOW).await.expect("run");
    *slot.lock() = None;
    assert_eq!(out, "Stopped.");
    assert_eq!(events.lock().last().map(|e| e.kind().to_string()), Some("cancelled".to_string()));
    assert!(codex.goals().is_empty());
    assert!(!reader.handed_on() && !reader.running());
}

// ---- the TinyFish requests ----------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Sent {
    url: String,
    body: Option<Value>,
    headers: Vec<(String, String)>,
}

impl Sent {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

/// A fake watch.http_request answering TinyFish's search (GET) and fetch (POST); records what was sent.
fn tinyfish_api(results: Value, read: Value, errors: Result<Value, FetchError>) -> (Arc<dyn Http>, Arc<Mutex<Vec<Sent>>>) {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let log = sent.clone();
    let fake = move |url: &str, opts: &HttpOptions| -> Result<(u16, String), FetchError> {
        log.lock().push(Sent {
            url: url.to_string(),
            body: opts.data.as_ref().map(|d| serde_json::from_slice(d).expect("a JSON body")),
            headers: opts.headers.clone(),
        });
        if url.starts_with(tinyfish::SEARCH_URL) {
            let n = results.as_array().map(Vec::len).unwrap_or(0);
            return Ok((200, json!({"query": "q", "results": results, "total_results": n, "page": 0}).to_string()));
        }
        let errors = errors.clone()?;
        Ok((200, json!({"results": read, "errors": errors}).to_string()))
    };
    (Arc::new(fake), sent)
}

#[test]
fn test_the_search_and_fetch_requests() {
    let mut results: Vec<Value> = "abcd"
        .chars()
        .enumerate()
        .map(|(i, c)| {
            json!({"position": i + 1, "url": format!("https://{c}.example"), "title": c.to_ascii_uppercase().to_string(),
                   "snippet": format!("{c} snippet")})
        })
        .collect();
    results.push(json!({"title": "no link"}));
    let (api, sent) = tinyfish_api(
        Value::Array(results),
        json!([{"url": "https://b.example", "title": "B page", "text": "b text"},
               {"url": "https://a.example", "title": null, "text": "a text"},
               {"url": "https://c.example", "text": "   "}]), // read but empty: its snippet stands in
        Ok(json!([{"url": "https://x.example", "error": "timeout"}])),
    );
    let world = World::new(&[("TINYFISH_API_KEY", "tf-test")]).with_http(api);
    let got = web_reader::search_and_read(&world.ctx, "  will   it snow ", web_reader::RESULTS).expect("pages");
    let sent = sent.lock().clone();
    let (search, fetch) = (&sent[0], &sent[1]);
    assert_eq!(sent.len(), 2);
    assert_eq!(search.url, format!("{}?query=will+it+snow", tinyfish::SEARCH_URL));
    assert!(search.body.is_none());
    assert_eq!(search.header("X-API-Key"), Some("tf-test"));
    assert!(search.header("Authorization").is_none());
    assert_eq!(fetch.url, tinyfish::FETCH_URL);
    assert_eq!(fetch.header("X-API-Key"), Some("tf-test"));
    let body = fetch.body.as_ref().unwrap();
    assert_eq!(body["urls"], json!(["https://a.example", "https://b.example", "https://c.example"])); // the top 3
    assert_eq!(body["format"], "markdown");
    assert_eq!(body["ttl"], json!(web_reader::TTL_SECS));
    assert_eq!(
        got,
        vec![
            // search order
            Page { url: "https://a.example".into(), title: "A".into(), markdown: "a text".into() },
            Page { url: "https://b.example".into(), title: "B page".into(), markdown: "b text".into() },
            Page { url: "https://c.example".into(), title: "C".into(), markdown: "c snippet".into() },
        ]
    );
}

#[test]
fn test_a_failed_fetch_answers_from_the_snippets() {
    let (api, _) = tinyfish_api(
        json!([{"url": "https://a.example", "title": "A", "snippet": "a snippet"},
               {"url": "https://b.example", "title": "B", "snippet": ""}]),
        json!([]),
        Err(FetchError::with_status("the site answered HTTP 429", 429)),
    );
    let world = World::new(&[("TINYFISH_API_KEY", "tf-test")]).with_http(api);
    assert_eq!(
        web_reader::search_and_read(&world.ctx, "q", web_reader::RESULTS).unwrap(),
        vec![Page { url: "https://a.example".into(), title: "A".into(), markdown: "a snippet".into() }]
    );
}

#[test]
fn test_a_failed_search_is_a_fetch_error_from_tinyfish() {
    let refused: Arc<dyn Http> =
        Arc::new(|_: &str, _: &HttpOptions| Err(FetchError::with_status("the site answered HTTP 401", 401)));
    let world = World::new(&[("TINYFISH_API_KEY", "tf-test")]).with_http(refused.clone());
    let e = web_reader::search_and_read(&world.ctx, "q", web_reader::RESULTS).unwrap_err();
    assert!(e.reason.contains("TinyFish: the site answered HTTP 401"));
    assert_eq!((e.host.as_str(), e.status), (tinyfish::SEARCH_HOST, 401));
    let world = World::new(&[]).with_http(refused);
    let e = web_reader::search_and_read(&world.ctx, "q", web_reader::RESULTS).unwrap_err();
    assert!(e.reason.contains("TINYFISH_API_KEY"));
}

#[test]
fn test_the_switch() {
    let shipped = eve_browser_router::SHIPPED;
    let cfg = |env: &[(&str, &str)], shipped: bool| web_reader::configured_shipped(&World::new(env).ctx, shipped);
    assert!(!web_reader::configured(&World::new(&[]).ctx).enabled); // no key
    assert_eq!(web_reader::configured(&World::new(&[("TINYFISH_API_KEY", "tf-x")]).ctx).enabled, shipped);
    assert!(!cfg(&[("TINYFISH_API_KEY", "tf-x"), ("EVE_WEB_READER", "0")], shipped).enabled);
    assert!(!cfg(&[("TINYFISH_API_KEY", "tf-x")], false).enabled); // held gates: off
    assert!(cfg(&[("TINYFISH_API_KEY", "tf-x"), ("EVE_WEB_READER", "1")], false).enabled);
    assert_eq!(cfg(&[("EVE_WEB_READER_TASKS", "9")], shipped).tasks_per_day, 9);
}

// ---- routed: the search router picks this body's provider (search_router) ------------------------------------

fn routed_config() -> SearchConfig {
    SearchConfig {
        engine: "openrouter-perplexity".into(),
        providers: vec!["perplexity".into(), "tinyfish".into(), "firecrawl".into()],
        ..SearchConfig::default()
    }
}

/// The fakes `routed` lends; one left out fails the test if it runs (Python's `unused`, which pytest.fail()s).
#[derive(Default)]
struct Fakes {
    search: Option<Search>,
    ask: Option<Ask>,
    fc_search: Option<web_reader::FirecrawlSearchFn>,
    tool_runner: Option<web_reader::ToolRunFn>,
    perplexity: Option<search_router::PerplexityFn>,
}

fn routed(world: &World, provider: &str, codex: &Arc<Codex>, fakes: Fakes, unused: &Arc<AtomicBool>) -> WebReaderAgent {
    let flag = |u: &Arc<AtomicBool>| {
        let u = u.clone();
        move || u.store(true, Ordering::SeqCst)
    };
    let (f1, f2, f3, f4, f5) = (flag(unused), flag(unused), flag(unused), flag(unused), flag(unused));
    let provider = provider.to_string();
    let route: RouteFn = Arc::new(move |_text: &str, _available: &[String]| {
        Ok(Route { provider: provider.clone(), p: 0.96, ms: 200.0, error: String::new() })
    });
    WebReaderAgent::new(
        fallback(codex),
        quiet_events(),
        no_questions(),
        ReaderOptions {
            ctx: Some(world.ctx.clone()),
            config: Some(ReaderConfig { enabled: true, search: routed_config(), ..ReaderConfig::default() }),
            clock: Some(Arc::new(|| 1.8e9)),
            search: Some(fakes.search.unwrap_or_else(|| {
                Arc::new(move |_q: &str| {
                    f1();
                    Err(FetchError::new("this provider should not run"))
                })
            })),
            ask: Some(fakes.ask.unwrap_or_else(|| {
                Arc::new(move |_b: &Value| {
                    f2();
                    Err(FetchError::new("this provider should not run"))
                })
            })),
            firecrawl_search: Some(fakes.fc_search.unwrap_or_else(|| {
                Arc::new(move |_q: &str| {
                    f3();
                    Err(FetchError::new("this provider should not run"))
                })
            })),
            tool_runner: Some(fakes.tool_runner.unwrap_or_else(|| {
                Arc::new(move |_c: &ToolCall| {
                    f4();
                    Err(FetchError::new("this provider should not run"))
                })
            })),
            perplexity: Some(fakes.perplexity.unwrap_or_else(|| {
                Arc::new(move |_q: &str, _s: &str, _f: &str| {
                    f5();
                    json!({"ok": false, "reason": "this provider should not run"})
                })
            })),
            route: Some(route),
            ..ReaderOptions::default()
        },
    )
}

type EngineCalls = Arc<Mutex<Vec<(String, String, String)>>>;

fn engine(answer: &str) -> (search_router::PerplexityFn, EngineCalls) {
    let calls: EngineCalls = Arc::new(Mutex::new(Vec::new()));
    let log = calls.clone();
    let answer = answer.to_string();
    let run: search_router::PerplexityFn = Arc::new(move |query: &str, system: &str, feature: &str| {
        log.lock().push((query.to_string(), system.to_string(), feature.to_string()));
        json!({"ok": true, "answer": answer, "usage": {"in": 1, "out": 1}, "ms": 900, "cost_usd": 0.005,
               "sources": [{"title": "Review A", "url": "https://reviews.example/a", "snippet": ""},
                           {"title": "Review B", "url": "https://reviews.example/b", "snippet": ""}]})
    });
    (run, calls)
}

#[tokio::test]
async fn test_a_many_source_task_is_answered_by_the_engine_search_with_its_citations() {
    let world = World::new(&[]);
    let unused = Arc::new(AtomicBool::new(false));
    let (per, calls) = engine("The Sony and the Bose lead most reviews (Review A, Review B).");
    let mut reader =
        routed(&world, "perplexity", &Arc::new(Codex::default()), Fakes { perplexity: Some(per), ..Fakes::default() }, &unused);
    let (out, events) = run(&mut reader, "best noise cancelling headphones this year").await;
    assert_eq!(
        out,
        "The Sony and the Bose lead most reviews (Review A, Review B).\n\nhttps://reviews.example/a\nhttps://reviews.example/b"
    );
    let calls = calls.lock().clone();
    assert_eq!(calls[0].1, search_router::READER_SYSTEM);
    assert_eq!(calls[0].2, eve_spend::TASKS);
    assert_eq!(reader.provider(), "web-reader");
    assert_eq!(reader.status()["search_provider"], "perplexity");
    assert_eq!(kinds(&events), ["started", "progress", "final"]);
    assert!(!unused.load(Ordering::SeqCst));
}

#[tokio::test]
async fn test_an_engine_search_that_did_not_answer_hands_the_task_to_codex() {
    let world = World::new(&[]);
    let unused = Arc::new(AtomicBool::new(false));
    let codex = Arc::new(Codex::default());
    let (per, _) = engine("NOT FOUND");
    let mut reader = routed(&world, "perplexity", &codex, Fakes { perplexity: Some(per), ..Fakes::default() }, &unused);
    let (out, _) = run(&mut reader, "compare two laptops").await;
    assert_eq!(out, "codex did it");
    assert_eq!(codex.goals(), ["compare two laptops"]);
    assert!(!unused.load(Ordering::SeqCst));
}

#[tokio::test]
async fn test_a_routed_reader_that_fails_tries_the_engine_search_before_codex() {
    let world = World::new(&[]);
    let unused = Arc::new(AtomicBool::new(false));
    let (per, calls) = engine("Open 10am to 5pm.");
    let fakes = Fakes { search: Some(search_with(Vec::new())), perplexity: Some(per), ..Fakes::default() };
    let mut reader = routed(&world, "tinyfish", &Arc::new(Codex::default()), fakes, &unused);
    let (out, _) = run(&mut reader, "exploratorium hours sunday").await;
    assert!(out.starts_with("Open 10am to 5pm."));
    assert_eq!(calls.lock().len(), 1);
    assert_eq!(reader.status()["search_provider"], "perplexity");
    assert!(!unused.load(Ordering::SeqCst));
}

// ---- Alexandria: data tools beside the pages (firecrawl, routed) ---------------------------------------------

fn flights() -> Tool {
    let opt = |name: &str, kind: &str, required: bool| ToolOption {
        name: name.into(),
        kind: kind.into(),
        about: String::new(),
        required,
    };
    Tool {
        provider: "flights-google-com".into(),
        capability: "flights/search_flights".into(),
        name: "Search Google Flights".into(),
        about: "One-way fares.".into(),
        options: vec![
            opt("origin", "string", true),
            opt("destination", "string", true),
            opt("departure_date", "string", true),
            opt("adults", "number", false),
        ],
    }
}

fn fares() -> Value {
    json!({"flights": [{"price": 129, "airline": "JetBlue", "url": "https://www.google.com/travel/flights/b1"}]})
}

fn pick() -> Value {
    json!({"answered": false, "tool": {"id": 1, "options": {"origin": "SFO", "destination": "JFK",
                                                          "departure_date": "2026-10-02", "adults": 1,
                                                          "api_key": "x", "nested": {"a": 1}}}})
}

#[tokio::test]
async fn test_a_fitting_tool_is_run_and_answers_with_only_its_own_links() {
    let world = World::new(&[("EVE_FIRECRAWL_USD", "0.004")]);
    let unused = Arc::new(AtomicBool::new(false));
    let ran = Arc::new(Mutex::new(Vec::<ToolCall>::new()));
    let searched = Arc::new(Mutex::new(Vec::<String>::new()));
    let (ask, sent) = models(vec![
        pick(),
        json!({"answered": true, "answer": "Cheapest: JetBlue $129 https://www.google.com/travel/flights/b1 or https://evil.example/pay",
               "sources": ["T"]}),
    ]);
    let (s, r) = (searched.clone(), ran.clone());
    let fakes = Fakes {
        ask: Some(ask),
        fc_search: Some(Arc::new(move |q: &str| {
            s.lock().push(q.to_string());
            Ok(Found { pages: pages(), tools: vec![flights()], credits: Some(4.into()) })
        })),
        tool_runner: Some(Arc::new(move |call: &ToolCall| {
            r.lock().push(call.clone());
            Ok(Ran { data: fares(), credits: Some(5.into()) })
        })),
        ..Fakes::default()
    };
    let mut reader = routed(&world, "firecrawl", &Arc::new(Codex::default()), fakes, &unused);
    let (out, events) = run(&mut reader, "cheapest flight SFO to JFK this Friday").await;
    assert_eq!(searched.lock().len(), 1); // one search, with its tools
    // Only the declared, plain options reach Firecrawl; the invented ones are dropped.
    let ran = ran.lock().clone();
    assert_eq!(ran.len(), 1);
    assert_eq!(
        ran[0].alexandria(),
        json!({"provider": "flights-google-com", "capability": "flights/search_flights",
               "options": {"origin": "SFO", "destination": "JFK", "departure_date": "2026-10-02", "adults": 1}})
    );
    assert_eq!(out, "Cheapest: JetBlue $129 https://www.google.com/travel/flights/b1 or (link removed)");
    assert!(events.iter().any(|e| e.text() == "Checking Search Google Flights…"));
    assert!(content(&sent, 0, 1).contains("Tools:") && content(&sent, 1, 1).contains("[T]"));
    let records: Vec<_> = world.rows().into_iter().filter(|r| r["p"] == "firecrawl").collect();
    let heads: Vec<(Value, Value, Value)> = records.iter().map(|r| (r["p"].clone(), r["m"].clone(), r["f"].clone())).collect();
    assert_eq!(
        heads,
        vec![
            (json!("firecrawl"), json!("search"), json!(eve_spend::TASKS)),
            (json!("firecrawl"), json!("alexandria flights-google-com"), json!(eve_spend::TASKS)),
        ]
    );
    assert!((records[0]["usd"].as_f64().unwrap() - 0.016).abs() < 1e-9);
    assert!((records[1]["usd"].as_f64().unwrap() - 0.02).abs() < 1e-9);
    assert_eq!(reader.status()["search_provider"], "firecrawl");
    assert!(!unused.load(Ordering::SeqCst));
}

#[tokio::test]
async fn test_a_tool_pick_that_does_not_check_out_runs_nothing() {
    for pick in [
        json!({"answered": false, "tool": {"id": 2, "options": {}}}), // not a tool the search offered
        json!({"answered": false, "tool": {"id": 1, "options": {"origin": "SFO"}}}), // a required option missing
        json!({"answered": false, "tool": "flights"}),
    ] {
        let world = World::new(&[]);
        let unused = Arc::new(AtomicBool::new(false));
        let codex = Arc::new(Codex::default());
        let (per, calls) = engine("NOT FOUND");
        let (ask, _) = models(vec![pick]);
        let fakes = Fakes {
            ask: Some(ask),
            perplexity: Some(per),
            fc_search: Some(Arc::new(|_q: &str| Ok(Found { pages: pages(), tools: vec![flights()], credits: None }))),
            ..Fakes::default()
        };
        let mut reader = routed(&world, "firecrawl", &codex, fakes, &unused);
        let (out, _) = run(&mut reader, SNOW).await;
        assert_eq!(out, "codex did it");
        assert_eq!(calls.lock().len(), 1);
        assert!(!unused.load(Ordering::SeqCst)); // the tool runner would have flagged it had it run
    }
}

#[tokio::test]
async fn test_a_failed_tool_or_unaccepted_terms_falls_back_to_the_pages() {
    let world = World::new(&[]);
    let unused = Arc::new(AtomicBool::new(false));
    let (ask, sent) = models(vec![pick(), json!({"answered": true, "answer": "Fares start near $130.", "sources": [1]})]);
    let fakes = Fakes {
        ask: Some(ask),
        tool_runner: Some(Arc::new(|_c: &ToolCall| {
            Err(FetchError::new("Alexandria flights-google-com/flights/search_flights: THIRD_PARTY_DATA_TERMS_REQUIRED"))
        })),
        fc_search: Some(Arc::new(|_q: &str| Ok(Found { pages: pages(), tools: vec![flights()], credits: None }))),
        ..Fakes::default()
    };
    let mut reader = routed(&world, "firecrawl", &Arc::new(Codex::default()), fakes, &unused);
    let (out, _) = run(&mut reader, SNOW).await;
    assert_eq!(out, "Fares start near $130.\n\nhttps://www.weather.example/denver");
    assert!(!content(&sent, 1, 1).contains("[T]"));
    assert!(!unused.load(Ordering::SeqCst));
}

// ---- the Firecrawl requests ---------------------------------------------------------------------------------

fn recording(answer: impl Fn(&str) -> String + Send + Sync + 'static) -> (Arc<dyn Http>, Arc<Mutex<Vec<Sent>>>) {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let log = sent.clone();
    let fake = move |url: &str, opts: &HttpOptions| -> Result<(u16, String), FetchError> {
        log.lock().push(Sent {
            url: url.to_string(),
            body: opts.data.as_ref().map(|d| serde_json::from_slice(d).expect("a JSON body")),
            headers: opts.headers.clone(),
        });
        Ok((200, answer(url)))
    };
    (Arc::new(fake), sent)
}

#[test]
fn test_the_firecrawl_search_request() {
    let (api, sent) = recording(|_| {
        json!({"success": true, "creditsUsed": 5, "data": {"web": [
            {"url": "https://a.example", "title": "A", "markdown": "text"},
            {"url": "https://b.example", "title": "B", "markdown": ""},          // nothing to read: dropped
            {"metadata": {"sourceURL": "https://c.example"}, "description": "desc only"}]}})
        .to_string()
    });
    let world = World::new(&[("FIRECRAWL_API_KEY", "fc-test")]).with_http(api);
    let got = firecrawl::search(&world.ctx, "  will   it snow ", firecrawl::RESULTS, firecrawl::SEARCH_TIMEOUT_SECS, false)
        .expect("found");
    let sent = sent.lock()[0].clone();
    assert_eq!(sent.url, firecrawl::SEARCH_URL);
    assert_eq!(sent.header("Authorization"), Some("Bearer fc-test"));
    let body = sent.body.unwrap();
    assert_eq!(body["query"], "will it snow");
    assert_eq!(body["limit"], json!(firecrawl::RESULTS));
    assert_eq!(body["scrapeOptions"]["formats"], json!(["markdown"]));
    assert!(body["scrapeOptions"]["maxAge"].as_i64().unwrap() > 0);
    assert_eq!(got.pages.iter().map(|p| p.url.as_str()).collect::<Vec<_>>(), ["https://a.example", "https://c.example"]);
    assert_eq!(got.credits, Some(5.into()));
}

#[test]
fn test_the_alexandria_requests() {
    let (api, sent) = recording(|url| {
        if url == firecrawl::TOOL_URL {
            return json!({"success": true, "data": {"alexandria": [
                {"provider": "p", "capability": "c", "creditsCost": 5, "data": fares()}]}})
            .to_string();
        }
        json!({"success": true, "creditsUsed": 4, "data": {
            "web": [{"url": "https://a.example", "markdown": "text"}],
            "tools": [{"provider": "flights-google-com", "capability": "flights/search_flights",
                       "name": "Search Google Flights", "description": "Fares",
                       "options": [{"name": "origin", "type": "string", "required": true}]},
                      {"name": "no provider"}]}})
        .to_string()
    });
    let world = World::new(&[("FIRECRAWL_API_KEY", "fc-test")]).with_http(api);
    let got = firecrawl::search(&world.ctx, "flights", firecrawl::RESULTS, firecrawl::SEARCH_TIMEOUT_SECS, true).unwrap();
    {
        let sent = sent.lock();
        let body = sent[0].body.as_ref().unwrap();
        assert_eq!(body["sources"], json!([{"type": "web"}, {"type": "alexandria"}]));
        assert_eq!(body["toolDetail"], "full");
    }
    assert_eq!(got.tools.iter().map(|t| t.provider.as_str()).collect::<Vec<_>>(), ["flights-google-com"]);
    assert!(got.tools[0].options[0].required);
    let call = ToolCall { provider: "p".into(), capability: "c".into(), options: Map::new(), name: String::new() };
    let ran = firecrawl::run_tool(&world.ctx, &call, firecrawl::TOOL_TIMEOUT_SECS).unwrap();
    {
        let sent = sent.lock();
        assert_eq!(sent[1].url, firecrawl::TOOL_URL);
        assert_eq!(sent[1].body.as_ref().unwrap()["alexandria"][0]["provider"], "p");
    }
    assert_eq!(ran, Ran { data: fares(), credits: Some(5.into()) });
    firecrawl::search(&world.ctx, "flights", firecrawl::RESULTS, firecrawl::SEARCH_TIMEOUT_SECS, false).unwrap();
    let sent = sent.lock();
    let body = sent[2].body.as_ref().unwrap();
    assert_eq!(body["sources"], json!([{"type": "web"}]));
    assert!(body.get("toolDetail").is_none());
}

#[test]
fn test_a_firecrawl_failure_is_a_fetch_error_from_firecrawl() {
    let (api, _) = recording(|_| {
        json!({"success": true, "data": {"alexandria": [{"provider": "p", "capability": "c", "error": "upstream 500"}]}})
            .to_string()
    });
    let world = World::new(&[("FIRECRAWL_API_KEY", "fc-test")]).with_http(api);
    let call = ToolCall { provider: "p".into(), capability: "c".into(), options: Map::new(), name: String::new() };
    let e = firecrawl::run_tool(&world.ctx, &call, firecrawl::TOOL_TIMEOUT_SECS).unwrap_err();
    assert!(e.reason.contains("upstream 500"));
    assert_eq!(e.host, firecrawl::HOST);

    let refused: Arc<dyn Http> =
        Arc::new(|_: &str, _: &HttpOptions| Err(FetchError::with_status("the site answered HTTP 402", 402)));
    let world = World::new(&[("FIRECRAWL_API_KEY", "fc-test")]).with_http(refused.clone());
    let e = firecrawl::search(&world.ctx, "q", firecrawl::RESULTS, firecrawl::SEARCH_TIMEOUT_SECS, false).unwrap_err();
    assert!(e.reason.contains("Firecrawl: the site answered HTTP 402"));
    assert_eq!((e.status, e.host.as_str()), (402, firecrawl::HOST));
    let world = World::new(&[]).with_http(refused);
    let e = firecrawl::search(&world.ctx, "q", firecrawl::RESULTS, firecrawl::SEARCH_TIMEOUT_SECS, false).unwrap_err();
    assert!(e.reason.contains("FIRECRAWL_API_KEY"));
}

#[test]
fn test_a_credits_price_is_read_or_left_unpriced() {
    let price = |env: &[(&str, &str)]| firecrawl::usd_per_credit(&World::new(env).ctx);
    assert_eq!(price(&[]), None);
    assert_eq!(price(&[("EVE_FIRECRAWL_USD", "0.004")]), Some(0.004));
    assert_eq!(price(&[("EVE_FIRECRAWL_USD", "cheap")]), None);
    assert_eq!(price(&[("EVE_FIRECRAWL_USD", "7")]), None);
}

#[tokio::test]
async fn test_the_reader_takes_the_routers_providers_and_the_off_switch_keeps_tinyfish() {
    let keys = [
        ("OPENROUTER_API_KEY", "r"),
        ("TYPESAFE_API_KEY", "ts-test"),
        ("TINYFISH_API_KEY", "tf-x"),
        ("FIRECRAWL_API_KEY", "fc-x"),
        ("EVE_WEB_READER", "1"),
    ];
    let with = |mode: &str| {
        let mut env = keys.to_vec();
        env.push(("EVE_SEARCH_ROUTER", mode));
        web_reader::configured(&World::new(&env).ctx)
    };
    assert_eq!(with("on").search.providers, ["perplexity", "tinyfish", "firecrawl"]);
    let off = with("off");
    assert!(off.search.providers.is_empty() && off.enabled);
    let world = World::new(&[]);
    let (ask, _) = model(json!({"answered": true, "answer": "ok", "sources": []}));
    let mut reader = make(&world, search_with(pages()), ask, &Arc::new(Codex::default()), None);
    assert_eq!(reader.provider(), "tinyfish");
    assert_eq!(run(&mut reader, SNOW).await.0, "ok");
}

// ---- beyond the Python file: the pieces it reaches through fakes ---------------------------------------------

#[test]
fn firecrawl_daily_cap_is_shared_counted_and_survives_a_restart() {
    let world = World::new(&[("EVE_FIRECRAWL_PER_DAY", "2")]);
    let now = Some(1.8e9);
    assert_eq!(firecrawl::left(&world.ctx, now), 2);
    assert!(firecrawl::take(&world.ctx, now));
    assert_eq!(firecrawl::left(&world.ctx, now), 1);
    // the saved count, in Python's json.dumps form
    let path = firecrawl::count_path(&world.ctx.firecrawl_calls);
    let day = firecrawl::day(now);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), format!("{{\"day\": \"{day}\", \"calls\": 1}}"));
    // a restart: a fresh count over the same file takes the day's count back
    let restarted = Ctx { firecrawl_calls: Arc::new(DayCount::at(&path)), ..world.ctx.clone() };
    assert_eq!(firecrawl::left(&restarted, now), 1);
    assert!(firecrawl::take(&restarted, now));
    assert!(!firecrawl::take(&restarted, now));
    assert_eq!(firecrawl::left(&restarted, now), 0);
    // another day starts fresh
    assert_eq!(firecrawl::left(&restarted, Some(1.8e9 + 86400.0 * 2.0)), 2);
}

#[test]
fn firecrawl_per_day_reads_numbers_as_python_does() {
    let n = |v: &str| firecrawl::per_day(&World::new(&[("EVE_FIRECRAWL_PER_DAY", v)]).ctx);
    assert_eq!(firecrawl::per_day(&World::new(&[]).ctx), firecrawl::DEFAULT_PER_DAY);
    assert_eq!(n("7"), 7);
    assert_eq!(n(" 3.9 "), 3);
    assert_eq!(n("1_0"), 10);
    assert_eq!(n("-4"), 0);
    assert_eq!(n("1e9"), 10000);
    assert_eq!(n("many"), firecrawl::DEFAULT_PER_DAY);
    assert_eq!(n("nan"), firecrawl::DEFAULT_PER_DAY);
}

#[test]
fn a_spent_day_never_reaches_firecrawl() {
    let reached = Arc::new(AtomicBool::new(false));
    let r = reached.clone();
    let http: Arc<dyn Http> = Arc::new(move |_: &str, _: &HttpOptions| {
        r.store(true, Ordering::SeqCst);
        Ok((200, "{}".to_string()))
    });
    let world = World::new(&[("FIRECRAWL_API_KEY", "fc-test"), ("EVE_FIRECRAWL_PER_DAY", "0")]).with_http(http);
    let e = firecrawl::search(&world.ctx, "q", 3, 30.0, false).unwrap_err();
    assert_eq!(e.reason, "Firecrawl: today's 0 calls are used up (EVE_FIRECRAWL_PER_DAY)");
    assert_eq!(e.host, firecrawl::HOST);
    assert!(!reached.load(Ordering::SeqCst));
}

#[test]
fn firecrawl_request_bodies_are_pythons_json() {
    let (api, _) = recording(|_| "{}".to_string());
    let bodies = Arc::new(Mutex::new(Vec::<String>::new()));
    let b = bodies.clone();
    let http: Arc<dyn Http> = Arc::new(move |url: &str, opts: &HttpOptions| {
        b.lock().push(String::from_utf8(opts.data.clone().unwrap()).unwrap());
        api.request(url, opts)
    });
    let world = World::new(&[("FIRECRAWL_API_KEY", "fc-test")]).with_http(http);
    let found = firecrawl::search(&world.ctx, "caf\u{e9}", 3, 30.0, false).unwrap();
    assert_eq!(found, Found::default());
    assert_eq!(
        bodies.lock()[0],
        "{\"query\": \"caf\\u00e9\", \"limit\": 3, \"sources\": [{\"type\": \"web\"}], \"timeout\": 25000, \
         \"scrapeOptions\": {\"formats\": [\"markdown\"], \"onlyMainContent\": true, \"maxAge\": 3600000}}"
    );
}

#[test]
fn a_tool_that_needs_an_id_looked_up_first_is_not_offered() {
    let item = |name: &str, required: bool| {
        json!({"provider": "skyscanner", "capability": "flights/search_flights",
               "options": [{"name": name, "type": "string", "required": required}]})
    };
    assert!(firecrawl::tool(&item("destination_entity_id", true)).is_none());
    assert!(firecrawl::tool(&item("IDS", true)).is_none());
    assert!(firecrawl::tool(&item("place_id\n", true)).is_none()); // Python's `$` before a final newline
    assert!(firecrawl::tool(&item("destination_entity_id", false)).is_some()); // optional: offered
    assert!(firecrawl::tool(&item("origin", true)).is_some()); // "id" in the text is no sign
    assert!(firecrawl::tool(&item("valid", true)).is_some());
    let t = firecrawl::tool(&json!({"provider": "p", "capability": "c", "name": "N", "description": "D",
                                    "options": [{"name": "a", "about": "x".repeat(400), "required": "yes"},
                                                {"type": "string"}, "junk"]}))
    .unwrap();
    assert_eq!(t.options.len(), 1);
    assert_eq!(t.options[0].about.len(), 300);
    assert!(!t.options[0].required); // only `true` is required
    assert_eq!((t.name.as_str(), t.about.as_str()), ("N", "D"));
    assert!(firecrawl::tool(&json!({"provider": "", "capability": "c"})).is_none());
}

fn reply(v: Value) -> Value {
    json!({"choices": [{"message": {"content": v.to_string()}}]})
}

#[test]
fn pick_tool_reads_a_number_given_as_text_and_only_plain_values() {
    let tools = vec![flights()];
    let call = firecrawl::pick_tool(
        &reply(json!({"tool": {"id": " 1 ", "options": {"origin": "SFO", "destination": ["JFK", "EWR"],
                                                          "departure_date": "2026-10-02", "adults": null}}})),
        &tools,
    )
    .unwrap()
    .unwrap();
    assert_eq!(call.name, "Search Google Flights");
    assert_eq!(Value::Object(call.options), json!({"origin": "SFO", "destination": ["JFK", "EWR"], "departure_date": "2026-10-02"}));
    for id in [json!(true), json!(0), json!(1.0), json!(-1), json!("one")] {
        let got = firecrawl::pick_tool(&reply(json!({"tool": {"id": id, "options": {}}})), &tools).unwrap();
        assert!(got.is_none());
    }
    assert!(firecrawl::pick_tool(&json!({"choices": []}), &tools).is_err());
    assert!(firecrawl::pick_tool(&json!({"choices": [{"message": {"content": "no json"}}]}), &tools).is_err());
}

#[test]
fn run_tool_reports_why_it_failed() {
    let world_with = |answer: Value| {
        let (api, _) = recording(move |_| answer.to_string());
        World::new(&[("FIRECRAWL_API_KEY", "fc-test")]).with_http(api)
    };
    let call = ToolCall { provider: "p".into(), capability: "c".into(), options: Map::new(), name: String::new() };
    let why = |answer: Value| firecrawl::run_tool(&world_with(answer).ctx, &call, 45.0).unwrap_err().reason;
    assert_eq!(why(json!({"success": false, "code": "THIRD_PARTY_DATA_TERMS_REQUIRED"})),
               "Alexandria p/c: THIRD_PARTY_DATA_TERMS_REQUIRED");
    assert_eq!(why(json!({"success": true, "data": {"alexandria": [{}]}})), "Alexandria p/c: no data");
    assert_eq!(why(json!({"data": []})), "Alexandria p/c: no data");
    let ok = firecrawl::run_tool(&world_with(json!({"data": {"alexandria": [{"data": null, "creditsCost": 1.5}]}})).ctx, &call, 45.0)
        .unwrap();
    assert_eq!(ok.data, Value::Null);
    assert_eq!(ok.credits.and_then(|n| n.as_f64()), Some(1.5));
}

#[test]
fn meter_records_credits_priced_or_not() {
    let world = World::new(&[]);
    firecrawl::meter(&world.ctx, "search", Some(&4.into()), eve_spend::SEARCH, Some(0.004));
    firecrawl::meter(&world.ctx, "search", Some(&4.into()), eve_spend::SEARCH, None);
    firecrawl::meter(&world.ctx, "search", None, eve_spend::SEARCH, Some(0.004));
    firecrawl::meter(&world.ctx, "search", serde_json::Number::from_f64(2.5).as_ref(), eve_spend::SEARCH, Some(0.0));
    let rows = world.rows();
    let usd: Vec<Option<f64>> = rows.iter().map(|r| r["usd"].as_f64()).collect();
    assert_eq!(usd, [Some(0.016), None, None, Some(0.0)]);
    let notes: Vec<&str> = rows.iter().map(|r| r["note"].as_str().unwrap()).collect();
    assert_eq!(notes, ["4 credits", "4 credits", "? credits", "2.5 credits"]);
}

#[test]
fn tinyfish_search_keeps_only_web_links_and_quotes_the_query() {
    let (api, sent) = recording(|_| {
        json!({"results": [{"url": "ftp://x.example"}, {"url": 7}, "junk", {"url": "HTTPS://A.example", "title": null},
                           {"url": "https://b.example", "snippet": "s"}, {"url": "https://c.example"}]})
        .to_string()
    });
    let world = World::new(&[("TINYFISH_API_KEY", " tf-test\n")]).with_http(api);
    let got = tinyfish::search(&world.ctx, "caf\u{e9} & co/2", 2, 15.0).unwrap();
    assert_eq!(
        got.iter().map(|r| r.url.as_str()).collect::<Vec<_>>(),
        ["HTTPS://A.example", "https://b.example"]
    );
    assert_eq!((got[0].title.as_str(), got[1].snippet.as_str()), ("", "s"));
    let sent = sent.lock()[0].clone();
    assert_eq!(sent.url, format!("{}?query=caf%C3%A9+%26+co%2F2", tinyfish::SEARCH_URL));
    assert_eq!(sent.header("X-API-Key"), Some("tf-test"));
    assert!(sent.header("Content-Type").is_none());
    // A limit of 0 still keeps one result: Python appends, then counts.
    assert_eq!(tinyfish::search(&world.ctx, "q", 0, 15.0).unwrap().len(), 1);
}

#[test]
fn tinyfish_unreadable_answers_are_errors_from_tinyfish() {
    for body in ["not json", "[1, 2]"] {
        let (api, _) = recording(move |_| body.to_string());
        let world = World::new(&[("TINYFISH_API_KEY", "tf")]).with_http(api);
        let e = tinyfish::fetch(&world.ctx, &["https://a.example".to_string()], "markdown", None, 30.0).unwrap_err();
        assert_eq!((e.reason.as_str(), e.host.as_str()), ("TinyFish sent an unreadable answer", tinyfish::FETCH_HOST));
    }
}

#[test]
fn tinyfish_fetch_keys_results_and_errors_by_url() {
    let (api, sent) = recording(|_| {
        json!({"results": [{"url": "https://a.example", "text": "a"}, {"text": "no url"}, {"url": "https://a.example", "text": "again"}],
               "errors": [{"url": "https://b.example", "error": "bot_blocked"}]})
        .to_string()
    });
    let world = World::new(&[("TINYFISH_API_KEY", "tf")]).with_http(api);
    let urls: Vec<String> = (0..12).map(|i| format!("https://{i}.example")).collect();
    let (results, errors) = tinyfish::fetch(&world.ctx, &urls, "html", Some(0), 2.0).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results["https://a.example"]["text"], "again");
    assert_eq!(errors["https://b.example"]["error"], "bot_blocked");
    assert_eq!(tinyfish::page_refusal("bot_blocked"), Some(403));
    let sent = sent.lock()[0].clone();
    let body = sent.body.clone().unwrap();
    assert_eq!(body["urls"].as_array().unwrap().len(), tinyfish::MAX_URLS);
    assert_eq!((body["format"].clone(), body["ttl"].clone()), (json!("html"), json!(0)));
    assert_eq!(body["per_url_timeout_ms"], 1000); // max(1, min(110, timeout - 5)) seconds
    assert_eq!(sent.header("Content-Type"), Some("application/json"));
}

#[test]
fn urls_in_reads_string_values_only() {
    let got = web_reader::urls_in(&json!({"a": "see https://x.example/a), or www.y.example.",
                                          "b": [1, {"c": "https://z.example/q?x=1!"}], "d": null}));
    let want: HashSet<String> =
        ["https://x.example/a", "www.y.example", "https://z.example/q?x=1"].iter().map(|s| s.to_string()).collect();
    assert_eq!(got, want);
    assert!(web_reader::urls_in(&json!(5)).is_empty());
}

#[test]
fn answer_parts_lists_the_tools_or_the_tool_data_and_reads_only_page_sources() {
    let now = chrono::NaiveDate::from_ymd_opt(2026, 10, 2).unwrap().and_hms_opt(9, 5, 0).unwrap();
    let (ask, sent) = models(vec![
        json!({"answered": true, "answer": "  Snow,\n  28F. ", "sources": [2, 2, 1, 3]}),
        json!({"answered": true, "answer": "JetBlue https://www.google.com/travel/flights/b1", "sources": ["T"]}),
        json!({"answered": "yes", "answer": "x"}),
    ]);
    let tools = vec![flights()];
    let (text, used, _) = web_reader::answer_parts("snow?", &pages(), &ask, Some(now), Some(&tools), None).unwrap();
    assert_eq!(text, "Snow, 28F.");
    assert_eq!(used, ["https://news.example/denver", "https://www.weather.example/denver"]);
    let user = content(&sent, 0, 1);
    assert!(user.starts_with("Now: Friday 02 October 2026, 09:05\nRequest: snow?\n\nPages:\n\n[1] Denver forecast\n"));
    assert!(user.contains(
        "\n\nTools:\n\n(1) Search Google Flights: One-way fares.\nOptions: [{\"name\": \"origin\", \"type\": \"string\", \"about\": \"\", \"required\": true}"
    ));
    assert_eq!(content(&sent, 0, 0), format!("{}{}", web_reader::ANSWER_SYSTEM, web_reader::TOOLS_SYSTEM));
    let result = ToolResult { name: "Search Google Flights".into(), data: json!({"fare": "€129", "url": "https://www.google.com/travel/flights/b1"}) };
    let (text, used) = web_reader::answer_from("fare?", &pages(), &ask, Some(now), Some(&tools), Some(&result)).unwrap();
    assert_eq!((text.as_str(), used["usage"].clone()), ("JetBlue https://www.google.com/travel/flights/b1", json!({"cost": 0.0001})));
    let user = content(&sent, 1, 1);
    assert!(user.ends_with("\n\n---\n\n[T] Search Google Flights\n{\"fare\": \"€129\", \"url\": \"https://www.google.com/travel/flights/b1\"}"));
    assert!(!user.contains("Tools:"));
    assert_eq!(content(&sent, 1, 0), format!("{}{}", web_reader::ANSWER_SYSTEM, web_reader::TOOL_DATA_SYSTEM));
    // answered must be exactly true
    let (text, _) = web_reader::answer_from("q", &pages(), &ask, Some(now), None, None).unwrap();
    assert_eq!(text, "");
    let body = sent.lock()[0].clone();
    assert_eq!(body["model"], web_reader::MODEL);
    assert_eq!(body["usage"], json!({"include": true}));
}

#[test]
fn the_task_cap_keeps_the_last_seven_days() {
    let counts = TaskCount::default();
    let cfg = ReaderConfig { tasks_per_day: 1, ..ReaderConfig::default() };
    for d in 0..10 {
        assert!(web_reader::take_task(&counts, &cfg, 1.8e9 + 86400.0 * d as f64));
    }
    assert!(!web_reader::take_task(&counts, &cfg, 1.8e9 + 86400.0 * 9.0));
    assert_eq!(counts.0.lock().len(), 7);
}

#[tokio::test]
async fn status_steer_and_cancel_forward_to_codex_once_it_took_over() {
    let world = World::new(&[]);
    let codex = Arc::new(Codex::default());
    let (ask, _) = model(json!({"answered": false}));
    let mut reader = make(&world, search_with(pages()), ask, &codex, None);
    assert!(!reader.steer("faster"));
    assert!(reader.current().is_none() && reader.browser_shot().await.is_none());
    let (out, _) = run(&mut reader, "compare two laptops").await;
    assert_eq!(out, "codex did it");
    assert_eq!(reader.final_text(), "codex did it");
    assert_eq!(reader.goal(), "compare two laptops");
    let status = reader.status();
    assert_eq!(
        status.keys().cloned().collect::<Vec<_>>(),
        ["running", "provider", "goal", "handed_on", "search_provider"]
    );
    assert_eq!(status["handed_on"], true);
    assert_eq!(status["search_provider"], "tinyfish");
    assert!(reader.current().is_some() && !reader.browser_used());
    // no screenshot reader lent: none
    assert!(reader.browser_shot().await.is_none());
}

