//! Port of bridge/tests/test_science_search.py: the tool's schema and words, the arguments (validated and
//! clamped), the answer's shape (clipped, compact, linked), the network failures (429, 5xx, timeouts, a refusal)
//! over a mocked transport, the process's own rate ceiling, the owner's words kept out of the log, and the switch.
//!
//! No test here touches the network except the one marked live (ignored). Every search gets its own limiter so
//! tests running in parallel never share the process's ceiling.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use eve_science_search as ss;
use ss::{HttpRequest, HttpResponse, Limiter, SearchOptions, Transport, TransportError};

static NOW: LazyLock<DateTime<Utc>> = LazyLock::new(|| Utc.with_ymd_and_hms(2026, 10, 12, 12, 0, 0).unwrap());

fn args() -> Value {
    json!({"query": "effect of intermittent fasting on weight loss in adults", "k": null, "year_from": null,
           "year_to": null, "reviews_only": null, "open_access_only": null})
}

// Shaped like the service's /search answer (fields from its guide); the papers are made up.
fn paper_raw() -> Value {
    json!({"id": "W100", "doi": "10.1000/example.1", "title": "Fasting and weight: a trial", "date": "2020-02-01",
           "year": 2020, "type": "article", "authors": "Ada One | Ben Two | Cy Three | Di Four | Ed Five",
           "authors_count": 7, "venue": "Journal of Examples", "publisher": "Example Press", "is_oa": 1,
           "oa_status": "gold", "license": "cc-by", "citations": 42, "is_retracted": 0,
           "url": "https://doi.org/10.1000/example.1", "pdf_url": "https://example.org/a.pdf", "topic": "Diet",
           "field": "Medicine", "keywords": "fasting", "mesh": null, "pmcid": null, "smr": 0,
           "abstract": format!("<jats:p>OBJECTIVE: To test.</jats:p> {}", "word ".repeat(300)), "score": 0.6,
           "junk": 0.01, "openalex": "https://openalex.org/W100"})
}

fn body() -> Value {
    json!({"mode": "semantic", "results": [paper_raw()], "filters": {}, "timing": {"total_ms": 300.0}})
}

fn with_args(extra: Value) -> Value {
    let mut out = args();
    for (k, v) in extra.as_object().unwrap() {
        out[k] = v.clone();
    }
    out
}

fn obj(v: &Value) -> &Map<String, Value> {
    v.as_object().unwrap()
}

/// One reply of the mocked service: a status, a body (JSON or text) and headers, or a transport error.
#[derive(Clone)]
enum Reply {
    Answer(u16, Value, Vec<(String, String)>),
    Error(TransportError),
}

fn ok(status: u16, body: Value) -> Reply {
    Reply::Answer(status, body, Vec::new())
}

/// A mocked transport: each request takes the next of `replies` (the last one repeats).
struct Server {
    replies: Mutex<Vec<Reply>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl Server {
    fn new(replies: Vec<Reply>) -> Arc<Self> {
        Arc::new(Self { replies: Mutex::new(replies), requests: Mutex::new(Vec::new()) })
    }

    fn count(&self) -> usize {
        self.requests.lock().len()
    }

    fn params(&self, i: usize) -> Vec<(String, String)> {
        let url = self.requests.lock()[i].url.clone();
        let query = url.split_once('?').map(|(_, q)| q.to_string()).unwrap_or_default();
        query
            .split('&')
            .map(|pair| {
                let (k, v) = pair.split_once('=').unwrap();
                (decode(k), decode(v))
            })
            .collect()
    }
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).unwrap()
}

#[async_trait]
impl Transport for Server {
    async fn get(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.requests.lock().push(request);
        let reply = {
            let mut replies = self.replies.lock();
            if replies.len() > 1 {
                replies.remove(0)
            } else {
                replies[0].clone()
            }
        };
        match reply {
            Reply::Error(e) => Err(e),
            Reply::Answer(status, body, mut headers) => {
                let body = match body {
                    Value::String(s) => s.into_bytes(),
                    other => {
                        headers.push(("content-type".into(), "application/json".into()));
                        serde_json::to_vec(&other).unwrap()
                    }
                };
                Ok(HttpResponse { status, headers, body })
            }
        }
    }
}

fn no_sleep() -> ss::Sleep {
    Arc::new(|_s| Box::pin(async {}))
}

fn opts(server: &Arc<Server>) -> SearchOptions {
    SearchOptions {
        transport: Some(server.clone()),
        limiter: Some(Arc::new(Limiter::default())),
        sleep: Some(no_sleep()),
        now: Some(*NOW),
        clock: None,
    }
}

async fn run(args: &Value, server: &Arc<Server>) -> Value {
    ss::search(args, &opts(server)).await
}

async fn run_with(args: &Value, server: &Arc<Server>, limiter: Arc<Limiter>) -> Value {
    ss::search(args, &SearchOptions { limiter: Some(limiter), ..opts(server) }).await
}

fn on() -> HashMap<String, String> {
    HashMap::from([(ss::SWITCH.to_string(), "1".to_string())])
}

/// `json.dumps(v, ensure_ascii=False)`: Python's separators (", " and ": ").
fn py_dumps(v: &Value) -> String {
    match v {
        Value::Array(a) => format!("[{}]", a.iter().map(py_dumps).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter().map(|(k, v)| format!("{}: {}", Value::String(k.clone()), py_dumps(v))).collect::<Vec<_>>().join(", ")
        ),
        other => other.to_string(),
    }
}

fn len(s: &str) -> usize {
    s.chars().count()
}

// ---- the tool ---------------------------------------------------------------------------------------------------

#[test]
fn test_the_tool_is_strict_with_every_argument_required_and_nullable() {
    assert_eq!(ss::TOOL_NAMES, ["science_search"]);
    assert_eq!(*ss::TOOLS, vec![ss::TOOL.clone()]);
    let params = &ss::TOOL["parameters"];
    assert_eq!(ss::TOOL["strict"], json!(true));
    assert_eq!(params["additionalProperties"], json!(false));
    let mut required: Vec<String> =
        params["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    let mut props: Vec<String> = obj(&params["properties"]).keys().cloned().collect();
    let mut names: Vec<String> = obj(&args()).keys().cloned().collect();
    required.sort();
    props.sort();
    names.sort();
    assert_eq!(required, props);
    assert_eq!(props, names);
    assert_eq!(params["properties"]["query"]["type"], json!("string"));
    for name in ["k", "year_from", "year_to", "reviews_only", "open_access_only"] {
        assert!(params["properties"][name]["type"].as_array().unwrap().contains(&json!("null")), "{name}");
    }
    assert_eq!(params["properties"]["k"]["description"], json!("How many papers, 1 to 20; null for 8."));
}

#[test]
fn test_the_description_says_when_to_use_it_and_how_to_cite() {
    let text = ss::TOOL["description"].as_str().unwrap();
    for words in [
        "research",
        "studies",
        "review",
        "DOI",
        "Do not use it for news",
        "prices",
        "how-to",
        "everyday facts",
        "title and year",
        "link",
        "only abstracts were read",
        "thin",
    ] {
        assert!(text.contains(words), "{words}");
    }
    // it names no other tool: a name in a prompt is a value the model may fill a slot with
    assert!(!text.contains("_search") && !text.contains("web_search"));
}

// ---- the arguments ----------------------------------------------------------------------------------------------

#[test]
fn test_the_defaults_and_the_query_as_sent() {
    let p = ss::params(&args(), Some(*NOW)).unwrap();
    assert_eq!(Value::Object(p), json!({"q": args()["query"], "k": ss::DEFAULT_K}));
}

#[test]
fn test_k_is_clamped_to_one_through_twenty() {
    let cases = [
        (json!(null), 8),
        (json!(0), 1),
        (json!(-3), 1),
        (json!(1), 1),
        (json!(20), 20),
        (json!(50), 20),
        (json!(5.0), 5),
        (json!("12"), 12),
        (json!(true), 8),
        (json!("many"), 8),
        // float("nan") cannot travel in JSON; a float out of range falls back the same way
        (json!(1e12), 8),
        (json!(5.9), 5),
        (json!(" -4 "), 1),
        (json!("1234567890"), 8),
        (json!(u64::MAX), 20),
        // Python's pattern takes "12\x1c" but int() raises on it; Rust reads it as no number
        (json!("12\u{1c}"), 8),
        (json!("\u{a0}12\u{3000}"), 12),
    ];
    for (k, sent) in cases {
        assert_eq!(ss::params(&with_args(json!({"k": k.clone()})), Some(*NOW)).unwrap()["k"], json!(sent), "{k}");
    }
}

#[test]
fn test_years_are_clamped_and_put_in_order() {
    let p = ss::params(&with_args(json!({"year_from": 2024, "year_to": 2018})), Some(*NOW)).unwrap();
    assert_eq!((p["year_from"].clone(), p["year_to"].clone()), (json!(2018), json!(2024)));
    let p = ss::params(&with_args(json!({"year_from": 5, "year_to": 99999})), Some(*NOW)).unwrap();
    assert_eq!((p["year_from"].clone(), p["year_to"].clone()), (json!(ss::MIN_YEAR), json!(2027)));
    let p = ss::params(&with_args(json!({"year_from": 2020})), Some(*NOW)).unwrap();
    assert!(p["year_from"] == json!(2020) && !p.contains_key("year_to"));
    assert!(!ss::params(&with_args(json!({"year_from": "recent"})), Some(*NOW)).unwrap().contains_key("year_from"));
}

#[test]
fn test_the_flags_become_the_services_filters() {
    let p = ss::params(&with_args(json!({"reviews_only": true, "open_access_only": true})), Some(*NOW)).unwrap();
    assert!(p["type"] == json!("review") && p["open_access"] == json!("true"));
    let p = ss::params(&with_args(json!({"reviews_only": false, "open_access_only": null})), Some(*NOW)).unwrap();
    assert!(!p.contains_key("type") && !p.contains_key("open_access"));
    let p = ss::params(&with_args(json!({"reviews_only": " Yes ", "open_access_only": 1})), Some(*NOW)).unwrap();
    assert!(p["type"] == json!("review") && !p.contains_key("open_access")); // only True itself, or a word
}

#[test]
fn test_the_query_is_one_clean_line_clipped() {
    let p = ss::params(&with_args(json!({"query": "  gut\u{0} microbiome\n\tand autism\u{7f}  "})), Some(*NOW)).unwrap();
    assert_eq!(p["q"], json!("gut microbiome and autism"));
    let long = ss::params(&with_args(json!({"query": "sleep ".repeat(400)})), Some(*NOW)).unwrap()["q"].clone();
    let long = long.as_str().unwrap();
    assert!(len(long) <= ss::MAX_QUERY_CHARS && !long.ends_with('…') && !long.ends_with(' '));
    let q = "\"gut microbiome\" AND (autism OR ASD) NOT mice";
    // clean_text's clip is text[:limit - 1]: a limit of 0 keeps all but the last character, as Python's slice does
    assert_eq!(ss::clean_str("abc", 0), "ab…");
    assert_eq!(ss::clean_str("", 0), "");
    // the service's own syntax passes untouched
    assert_eq!(ss::params(&with_args(json!({"query": q})), Some(*NOW)).unwrap()["q"], json!(q));
}

#[tokio::test]
async fn test_a_query_too_short_is_refused_without_a_request() {
    for query in [json!(""), json!(" "), json!("x"), json!("\u{0}\u{1}"), json!(null), json!(7)] {
        let server = Server::new(vec![ok(200, body())]);
        let out = run(&with_args(json!({"query": query})), &server).await;
        assert_eq!(out["ok"], json!(false));
        assert!(out["reason"].as_str().unwrap().contains("at least"));
        assert_eq!(server.count(), 0);
    }
}

// ---- the answer -------------------------------------------------------------------------------------------------

#[test]
fn test_a_paper_is_compact_clipped_and_linked() {
    let p = ss::paper(obj(&paper_raw()));
    assert!(p["title"] == json!("Fasting and weight: a trial") && p["year"] == json!(2020) && p["type"] == json!("article"));
    assert!(p["venue"] == json!("Journal of Examples") && p["citations"] == json!(42) && p["open_access"] == json!(true));
    assert_eq!(p["authors"], json!("Ada One, Ben Two, Cy Three and 4 more")); // 7 in all, 3 shown
    assert!(p["doi"] == json!("10.1000/example.1") && p["link"] == json!("https://doi.org/10.1000/example.1"));
    assert_eq!(p["page"], json!("https://sciencemadereadable.com/?work=10.1000/example.1"));
    assert!(!p.contains_key("plain_words"));
    let abs = p["abstract"].as_str().unwrap();
    assert!(abs.starts_with("OBJECTIVE: To test. word") && !abs.contains('<'));
    assert!(len(abs) <= ss::ABSTRACT_CHARS && abs.ends_with('…'));
    // only what a model needs: no scores, junk, keywords, MeSH, PDF or publisher
    let keys: Vec<&str> = p.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["title", "year", "type", "venue", "authors", "citations", "open_access", "doi", "link", "page", "abstract"]
    );
}

#[test]
fn test_a_paper_with_little_on_record() {
    let p = ss::paper(obj(&json!({"id": "W7", "title": "Untitled\u{0} note", "authors": "", "is_oa": 0, "smr": 1,
                                  "openalex": "https://openalex.org/W7", "url": "javascript:alert(1)", "year": "2020",
                                  "citations": true})));
    assert!(p["title"] == json!("Untitled note") && p["authors"].is_null() && p["abstract"].is_null());
    assert!(p["doi"].is_null() && p["link"] == json!("https://openalex.org/W7")); // never a non-web url
    assert_eq!(p["page"], json!("https://sciencemadereadable.com/?work=W7"));
    assert_eq!(p["plain_words"], json!("https://sciencemadereadable.com/?paper=W7"));
    assert!(p["year"].is_null() && p["citations"].is_null() && p["open_access"] == json!(false));
    let two = ss::paper(obj(&json!({"authors": "A | B", "authors_count": 2})));
    assert_eq!(two["authors"], json!("A, B"));
    let odd = ss::paper(obj(&json!({"doi": "10.1/a b#c", "smr": 1})));
    assert_eq!(odd["link"], json!("https://doi.org/10.1/a b#c"));
    assert_eq!(odd["page"], json!("https://sciencemadereadable.com/?work=10.1/a%20b%23c"));
    // the keys keep Python's order: plain_words lands before the abstract
    let keys: Vec<&str> = odd.keys().map(String::as_str).collect();
    assert_eq!(keys[keys.len() - 2..], ["plain_words", "abstract"]);
    // smr == 1 is true for 1.0 and True too; a count below the names shown is ignored
    assert!(ss::paper(obj(&json!({"id": "W8", "smr": 1.0}))).contains_key("plain_words"));
    assert!(ss::paper(obj(&json!({"id": "W8", "smr": true}))).contains_key("plain_words"));
    assert_eq!(ss::paper(obj(&json!({"authors": "A|B|C|D", "authors_count": 1})))["authors"], json!("A, B, C and 1 more"));
    // entities and tags in the abstract read as text
    let html = ss::paper(obj(&json!({"abstract": "<b>Fish</b> &amp; chips &lt;3 &#x41;&copy"})));
    assert_eq!(html["abstract"], json!("Fish & chips <3 A©"));
}

#[test]
fn test_the_answer_says_how_it_searched_and_what_it_read() {
    let out = ss::shape(&body(), "q");
    assert!(out["ok"] == json!(true) && out["count"] == json!(1) && out["searched"] == json!("by meaning"));
    assert!(out["note"].as_str().unwrap().to_lowercase().contains("abstracts") && out.get("matching_papers").is_none());
    let kw = ss::shape(&json!({"mode": "keyword", "matches": 1234, "results": [paper_raw(), "junk", null]}), "q");
    assert!(kw["searched"] == json!("by exact words") && kw["matching_papers"] == json!(1234) && kw["count"] == json!(1));
    let fell = ss::shape(&json!({"mode": "semantic", "fallback": "no keyword match", "results": []}), "q");
    assert!(fell["searched"].as_str().unwrap().contains("no paper had the exact words"));
    assert!(fell["note"].as_str().unwrap().starts_with("No paper matched"));
    assert_eq!(ss::shape(&json!({"detail": "x"}), "q")["ok"], json!(false));
    assert_eq!(ss::shape(&json!([]), "q")["ok"], json!(false));
    // the answer's keys in Python's order
    let keys: Vec<&str> = obj(&kw).keys().map(String::as_str).collect();
    assert_eq!(keys, ["ok", "query", "searched", "count", "results", "note", "matching_papers"]);
}

#[test]
fn test_twenty_papers_stay_small_enough_for_a_models_context() {
    let mut big = paper_raw();
    big["title"] = json!("T".repeat(2000));
    big["venue"] = json!("V".repeat(900));
    let out = ss::shape(&json!({"mode": "semantic", "results": vec![big; 20]}), "q");
    // each paper at most about 1,400 characters even with the longest title and venue, so 20 stay under ~7k tokens
    for paper in out["results"].as_array().unwrap() {
        assert!(len(&py_dumps(paper)) <= 1400);
    }
    let typical = ss::shape(&json!({"mode": "semantic", "results": vec![paper_raw(); ss::DEFAULT_K as usize]}), "q");
    assert!(len(&py_dumps(&typical)) < 8 * 1100);
    assert!(len(out["results"][0]["title"].as_str().unwrap()) <= ss::TITLE_CHARS);
    assert!(len(out["results"][0]["venue"].as_str().unwrap()) <= ss::VENUE_CHARS);
}

// ---- the request ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_one_search_one_get_with_the_arguments() {
    let server = Server::new(vec![ok(200, body())]);
    let out = run(
        &with_args(json!({"k": 5, "year_from": 2019, "reviews_only": true, "open_access_only": true})),
        &server,
    )
    .await;
    assert!(out["ok"] == json!(true) && out["results"][0]["title"] == json!("Fasting and weight: a trial"));
    assert_eq!(server.count(), 1);
    let req = server.requests.lock()[0].clone();
    assert!(req.url.starts_with(&format!("{}?", ss::SEARCH_URL)));
    let want: Vec<(String, String)> = [
        ("q", args()["query"].as_str().unwrap()),
        ("k", "5"),
        ("year_from", "2019"),
        ("type", "review"),
        ("open_access", "true"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    assert_eq!(server.params(0), want);
    // spaces travel as "+", as httpx writes them
    assert!(req.url.contains("q=effect+of+intermittent"));
    assert!(req.header("user-agent").unwrap().contains("eve-bridge") && req.header("authorization").is_none());
    assert_eq!(req.header("accept"), Some("application/json"));
}

#[tokio::test]
async fn test_a_429_is_never_retried_and_says_how_long_to_wait() {
    let server = Server::new(vec![Reply::Answer(
        429,
        json!({"detail": "slow down", "retry_after": 17}),
        vec![("Retry-After".into(), "12".into())],
    )]);
    let out = run(&args(), &server).await;
    assert!(out["ok"] == json!(false) && out["retry_after_s"] == json!(12));
    let reason = out["reason"].as_str().unwrap();
    assert!(reason.contains("12 seconds") && reason.contains("too many") && server.count() == 1);
    let body_only = Server::new(vec![ok(429, json!({"retry_after": 17}))]);
    assert_eq!(run(&args(), &body_only).await["retry_after_s"], json!(17));
    let nothing = Server::new(vec![ok(429, json!("busy"))]);
    let out = run(&args(), &nothing).await;
    assert!(out["reason"].as_str().unwrap().contains("a minute") && out.get("retry_after_s").is_none());
    // clamped to 1..3600, and read like float(): "2.9" is 2
    let huge = Server::new(vec![Reply::Answer(429, json!("x"), vec![("retry-after".into(), "99999".into())])]);
    assert_eq!(run(&args(), &huge).await["retry_after_s"], json!(3600));
    let zero = Server::new(vec![Reply::Answer(429, json!("x"), vec![("Retry-After".into(), " 0 ".into())])]);
    assert_eq!(run(&args(), &zero).await["retry_after_s"], json!(1));
    let frac = Server::new(vec![Reply::Answer(429, json!({"retry_after": "4"}), vec![("Retry-After".into(), "2.9".into())])]);
    assert_eq!(run(&args(), &frac).await["retry_after_s"], json!(2));
    let date = Server::new(vec![Reply::Answer(
        429,
        json!({"retry_after": "4"}),
        vec![("Retry-After".into(), "Wed, 21 Oct 2026 07:28:00 GMT".into())],
    )]);
    assert_eq!(run(&args(), &date).await["retry_after_s"], json!(4));
    // a repeated header reads as httpx joins it ("12, 13"), which is no number: the body's hint
    let twice = Server::new(vec![Reply::Answer(
        429,
        json!({"retry_after": 17}),
        vec![("Retry-After".into(), "12".into()), ("retry-after".into(), "13".into())],
    )]);
    assert_eq!(run(&args(), &twice).await["retry_after_s"], json!(17));
    // the header is str.strip()ped before float(); the body's value goes to float() as it is, and float() keeps
    // the separators \x1c-\x1f (so "9\x1c" is no number) while it strips a no-break space
    let sep = Server::new(vec![ok(429, json!({"retry_after": "9\u{1c}"}))]);
    assert!(run(&args(), &sep).await.get("retry_after_s").is_none());
    let nbsp = Server::new(vec![ok(429, json!({"retry_after": "\u{a0}9\u{a0}"}))]);
    assert_eq!(run(&args(), &nbsp).await["retry_after_s"], json!(9));
}

#[tokio::test]
async fn test_a_5xx_is_retried_once() {
    for status in [500u16, 502, 503] {
        let server = Server::new(vec![ok(status, json!("down")), ok(200, body())]);
        assert_eq!(run(&args(), &server).await["ok"], json!(true));
        assert_eq!(server.count(), 2);
        let server = Server::new(vec![ok(status, json!("down"))]);
        let out = run(&args(), &server).await;
        assert!(out["ok"] == json!(false) && out["reason"].as_str().unwrap().contains(&format!("HTTP {status}")));
        assert_eq!(server.count(), 2);
    }
}

#[tokio::test]
async fn test_a_timeout_or_a_dropped_connection_is_retried_once() {
    let server = Server::new(vec![Reply::Error(TransportError::Timeout), ok(200, body())]);
    assert_eq!(run(&args(), &server).await["ok"], json!(true));
    assert_eq!(server.count(), 2);
    let server = Server::new(vec![Reply::Error(TransportError::Timeout)]);
    let out = run(&args(), &server).await;
    assert_eq!(out, json!({"ok": false, "reason": "Ask science took too long to answer."}));
    assert_eq!(server.count(), 2);
    let server = Server::new(vec![Reply::Error(TransportError::Failed("ConnectError".into()))]);
    let out = run(&args(), &server).await;
    assert_eq!(out, json!({"ok": false, "reason": "Ask science could not be reached."}));
    assert_eq!(server.count(), 2);
}

/// A transport that never answers: the attempt's own timeout ends it.
struct Hang;

#[async_trait]
impl Transport for Hang {
    async fn get(&self, _request: HttpRequest) -> Result<HttpResponse, TransportError> {
        std::future::pending().await
    }
}

#[tokio::test(start_paused = true)]
async fn test_a_request_that_hangs_times_out_within_the_budget() {
    let out = ss::search(
        &args(),
        &SearchOptions {
            transport: Some(Arc::new(Hang)),
            limiter: Some(Arc::new(Limiter::default())),
            sleep: Some(no_sleep()),
            now: Some(*NOW),
            clock: None,
        },
    )
    .await;
    assert_eq!(out, json!({"ok": false, "reason": "Ask science took too long to answer."}));
}

#[tokio::test]
async fn test_no_retry_when_the_budget_is_spent() {
    let ticks = Arc::new(Mutex::new(vec![0.0, 0.0, ss::BUDGET_SECS - 1.0, ss::BUDGET_SECS]));
    let clock: ss::Clock = {
        let ticks = ticks.clone();
        Arc::new(move || {
            let mut t = ticks.lock();
            if t.is_empty() {
                ss::BUDGET_SECS
            } else {
                t.remove(0)
            }
        })
    };
    let server = Server::new(vec![Reply::Error(TransportError::Timeout), ok(200, body())]);
    let o = SearchOptions {
        limiter: Some(Arc::new(Limiter::new(Arc::new(|| 0.0)))),
        clock: Some(clock),
        ..opts(&server)
    };
    let out = ss::search(&args(), &o).await;
    assert_eq!(out["ok"], json!(false));
    assert_eq!(server.count(), 1);
}

#[tokio::test]
async fn test_a_refusal_says_why_and_is_not_retried() {
    let server = Server::new(vec![ok(422, json!({"detail": [{"msg": "q: String should have at most 2000 characters"}]}))]);
    let out = run(&args(), &server).await;
    let reason = out["reason"].as_str().unwrap();
    assert!(out["ok"] == json!(false) && reason.contains("HTTP 422") && reason.contains("2000 characters"));
    assert_eq!(server.count(), 1);
    assert_eq!(
        run(&args(), &Server::new(vec![ok(404, json!("<html>"))])).await["reason"],
        json!("Ask science refused the search (HTTP 404).")
    );
    // a string detail, and a redirect (never followed) is a refusal too
    assert_eq!(
        run(&args(), &Server::new(vec![ok(400, json!({"detail": "bad\nfilter"}))])).await["reason"],
        json!("Ask science refused the search (HTTP 400): bad filter")
    );
    assert_eq!(
        run(&args(), &Server::new(vec![ok(301, json!("moved"))])).await["reason"],
        json!("Ask science refused the search (HTTP 301).")
    );
}

#[tokio::test]
async fn test_an_answer_that_is_not_json_is_said_plainly() {
    let out = run(&args(), &Server::new(vec![ok(200, json!("<html>maintenance</html>"))])).await;
    assert_eq!(out, json!({"ok": false, "reason": "Ask science sent an answer eve could not read."}));
}

/// An answer whose abstract carries a lone surrogate escape: json.loads reads it, so it is an answer, not "could not
/// read" (the surrogate reads as U+FFFD; a real pair stays the character it encodes).
#[tokio::test]
async fn test_an_answer_with_a_lone_surrogate_is_still_read() {
    let raw = r#"{"mode": "semantic", "results": [{"title": "A \ud800 B \ud83d\ude00 \"q\" \\ud800", "year": 2020}]}"#;
    let server = Server::new(vec![ok(200, json!(raw))]);
    let out = run(&args(), &server).await;
    assert_eq!(out["ok"], json!(true), "{out}");
    assert_eq!(out["results"][0]["title"], json!("A \u{fffd} B \u{1f600} \"q\" \\ud800"));
    let broken = Server::new(vec![ok(200, json!(r#"{"results": [}"#))]);
    assert_eq!(run(&args(), &broken).await["reason"], json!("Ask science sent an answer eve could not read."));
}

#[tokio::test]
async fn test_the_process_keeps_under_its_own_ceiling() {
    let clock = Arc::new(Mutex::new(0.0f64));
    let limiter = {
        let clock = clock.clone();
        Arc::new(Limiter::new(Arc::new(move || *clock.lock())))
    };
    let server = Server::new(vec![ok(200, body())]);
    for _ in 0..ss::MAX_PER_MINUTE {
        assert_eq!(run_with(&args(), &server, limiter.clone()).await["ok"], json!(true));
    }
    let out = run_with(&args(), &server, limiter.clone()).await;
    assert!(out["ok"] == json!(false) && out["retry_after_s"] == json!(60) && server.count() == ss::MAX_PER_MINUTE);
    *clock.lock() = ss::RATE_WINDOW_SECS;
    assert_eq!(run_with(&args(), &server, limiter.clone()).await["ok"], json!(true));
    // a retry is a request too: it takes a slot, and with none left the first failure is the answer
    let full = Arc::new(Limiter::new(Arc::new(|| 0.0)));
    for _ in 0..ss::MAX_PER_MINUTE - 1 {
        full.take();
    }
    let flaky = Server::new(vec![ok(503, json!("down")), ok(200, body())]);
    let out = run_with(&args(), &flaky, full).await;
    assert!(out["ok"] == json!(false) && out["reason"].as_str().unwrap().contains("HTTP 503") && flaky.count() == 1);
}

#[test]
fn test_the_limiter_resets() {
    let limiter = Limiter::new(Arc::new(|| 5.0));
    for _ in 0..ss::MAX_PER_MINUTE {
        assert_eq!(limiter.take(), 0.0);
    }
    assert_eq!(limiter.take(), 60.0);
    limiter.reset();
    assert_eq!(limiter.take(), 0.0);
}

// ---- the switch -------------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_it_ships_on_and_the_switch_turns_it_off() {
    let empty = HashMap::new();
    assert!(ss::enabled(Some(&empty)) && ss::tools(Some(&empty)) == vec![ss::TOOL.clone()]);
    for on in ["1", "true", "on", "yes", ""] {
        assert!(ss::enabled(Some(&HashMap::from([(ss::SWITCH.to_string(), on.to_string())]))), "{on}");
    }
    let server = Server::new(vec![ok(200, body())]);
    for off in ["0", "false", "no", "off", " OFF "] {
        let env = HashMap::from([(ss::SWITCH.to_string(), off.to_string())]);
        assert!(!ss::enabled(Some(&env)) && ss::tools(Some(&env)).is_empty());
        let out = ss::handle("science_search", &args(), Some(&env), &opts(&server)).await;
        assert_eq!(out, json!({"ok": false, "reason": ss::OFF_REASON}));
    }
    assert_eq!(server.count(), 0);
}

#[tokio::test]
async fn test_handle_refuses_other_names_and_non_objects() {
    let server = Server::new(vec![ok(200, body())]);
    let out = ss::handle("web_search", &args(), Some(&on()), &opts(&server)).await;
    assert_eq!(out, json!({"ok": false, "reason": "web_search is not the science search tool"}));
    let out = ss::handle("science_search", &json!(["q"]), Some(&on()), &opts(&server)).await;
    assert_eq!(out, json!({"ok": false, "reason": "the arguments must be an object"}));
    assert_eq!(server.count(), 0);
    let out = ss::handle("science_search", &args(), Some(&on()), &opts(&server)).await;
    assert_eq!(out["ok"], json!(true));
}

// ---- the live service -------------------------------------------------------------------------------------------

#[tokio::test]
#[ignore = "live: one real search against the public service (one of its 30 a minute)"]
async fn test_live_one_real_search() {
    let out = ss::search(&with_args(json!({"k": 3})), &SearchOptions::default()).await;
    assert_eq!(out["ok"], json!(true), "{out}");
    let count = out["count"].as_u64().unwrap();
    assert!((1..=3).contains(&count));
    for p in out["results"].as_array().unwrap() {
        assert!(!p["title"].as_str().unwrap_or("").is_empty() && !p["link"].as_str().unwrap_or("").is_empty());
        assert!(p["page"].as_str().unwrap().starts_with(ss::SITE));
    }
}
