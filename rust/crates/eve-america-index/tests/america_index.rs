//! Port of bridge/tests/test_america_index.py: offline government-index contract, failure boundaries and shared
//! cap checks. Python's `offline` fixture (a mocked `firecrawl.meter`, a `watch.http_request` that fails the
//! test) is [`Harness`]: a recording meter and a context whose HTTP panics.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use eve_america_index::{
    self as index, canonical_url, official_guard, Fetch, GovernmentIndex, IndexFailure, Meter, Runner,
};
use eve_web::firecrawl::{self, DayCount};
use eve_web::watch::{self, net::AddressRules, FetchError, HttpOptions};
use eve_web::Ctx;
use serde_json::{json, Value};

const QUESTION: &str = "Where can I read the United States Code?";
const JURISDICTION: &str = "U.S. federal";
const URL: &str = "https://uscode.house.gov/currency/currency.shtml";
const PAGE: &str =
    "<html><nav>Ignore this</nav><main><h1>United States Code</h1><p>Official page text.</p></main></html>";

type MeterCall = (String, Option<Value>, String, Option<f64>);
type MakeFetch = Box<dyn Fn(&Rec<(String, f64)>) -> Fetch>;

#[derive(Clone, Default)]
struct Rec<T>(Arc<Mutex<Vec<T>>>);

impl<T: Clone> Rec<T> {
    fn push(&self, item: T) {
        self.0.lock().unwrap().push(item);
    }
    fn all(&self) -> Vec<T> {
        self.0.lock().unwrap().clone()
    }
    fn count(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

fn offline_ctx(env: &[(&str, &str)]) -> Ctx {
    let env: HashMap<String, String> = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    Ctx::global().clone().with_env(env).with_http(Arc::new(|url: &str, _: &HttpOptions| -> Result<(u16, String), FetchError> {
        panic!("unexpected network request to {url}")
    }))
}

fn recording_meter(calls: &Rec<MeterCall>) -> Meter {
    let calls = calls.clone();
    Arc::new(move |what, credits, feature, price| {
        calls.push((what.to_string(), credits.map(|n| Value::Number(n.clone())), feature.to_string(), price));
        Ok(())
    })
}

fn row(url: Value, changes: &[(&str, Value)]) -> Value {
    let mut r = json!({"url": url, "title": "United States Code", "snippet": "Indexed official snippet.",
                       "updated_at": 1787585414000_i64});
    for (k, v) in changes {
        r[*k] = v.clone();
    }
    r
}

fn url_row(url: &str) -> Value {
    row(json!(url), &[])
}

fn reply(rows: &[Value], credits: Value) -> Value {
    json!({"data": {"results": rows.len(), "data": rows}, "credits": credits})
}

fn fixed_fetch(status: u16, body: &str, calls: &Rec<(String, f64)>) -> Fetch {
    let calls = calls.clone();
    let body = body.to_string();
    Arc::new(move |url, timeout| {
        calls.push((url.to_string(), timeout));
        Ok((status, body.clone()))
    })
}

fn failing_fetch(why: &str, calls: &Rec<(String, f64)>) -> Fetch {
    let calls = calls.clone();
    let why = why.to_string();
    Arc::new(move |url, timeout| {
        calls.push((url.to_string(), timeout));
        Err(anyhow::Error::new(FetchError::new(why.clone())))
    })
}

fn runner_of(payload: Value, calls: &Rec<(Value, f64)>) -> Runner {
    let calls = calls.clone();
    Arc::new(move |request, timeout| {
        calls.push((request.clone(), timeout));
        Ok(payload.clone())
    })
}

struct Harness {
    lookup: GovernmentIndex,
    runner: Rec<(Value, f64)>,
    fetch: Rec<(String, f64)>,
    meter: Rec<MeterCall>,
}

fn service_with(rows: &[Value], fetch: Option<Fetch>, env: &[(&str, &str)]) -> Harness {
    let runner = Rec::default();
    let fetches = Rec::default();
    let meter = Rec::default();
    let fetch = fetch.unwrap_or_else(|| fixed_fetch(200, PAGE, &fetches));
    let lookup = GovernmentIndex::default()
        .with_ctx(offline_ctx(env))
        .with_runner(runner_of(reply(rows, json!(2)), &runner))
        .with_fetch(fetch)
        .with_meter(recording_meter(&meter));
    Harness { lookup, runner, fetch: fetches, meter }
}

fn service(rows: &[Value]) -> Harness {
    service_with(rows, None, &[])
}

fn urls(result: &index::Evidence) -> Vec<String> {
    result.sources.iter().map(|s| s.url.clone()).collect()
}

fn failure(result: Result<index::Evidence, IndexFailure>) -> String {
    result.expect_err("the lookup should fail").0
}

#[test]
fn test_fixed_single_query_returns_source_evidence_and_meters_known_credits() {
    let h = service_with(&[url_row(URL)], None, &[("EVE_FIRECRAWL_USD", "0.004")]);
    let result = h.lookup.query(&format!(" {QUESTION} "), &format!(" {JURISDICTION} ")).unwrap();
    let runs = h.runner.all();
    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0].0,
        json!({"provider": "firecrawl-gov-index", "capability": "search", "options": {
            "query": format!("Jurisdiction: U.S. federal\nQuestion: {QUESTION}"), "top_k": 5, "full_content": false}})
    );
    assert!(0.0 < runs[0].1 && runs[0].1 <= firecrawl::TOOL_TIMEOUT_SECS);
    let fetches = h.fetch.all();
    assert_eq!(fetches.len(), 1);
    assert_eq!(fetches[0].0, URL);
    assert!(0.0 < fetches[0].1 && fetches[0].1 <= index::MAX_READ_SECS);
    assert_eq!(
        h.meter.all(),
        vec![("alexandria firecrawl-gov-index".to_string(), Some(json!(2)), "america".to_string(), Some(0.004))]
    );
    let value = result.to_value();
    assert!(result.ok && result.status == "sources_found" && value.get("answer").is_none());
    assert_eq!(result.provider, "firecrawl_gov_index");
    assert_eq!(result.question, QUESTION);
    assert_eq!(result.jurisdiction, JURISDICTION);
    assert_eq!(value["legal_advice"], json!(false));
    let source = &value["sources"][0];
    assert_eq!(
        *source,
        json!({"title": "United States Code", "url": URL, "currency": "unverified",
               "excerpt": "Indexed official snippet.", "page_text": "United States Code Official page text.",
               "read_status": "live", "text_truncated": false,
               "indexed_at": "2026-08-24T15:30:14.000Z", "fetched_at": source["fetched_at"]})
    );
    // Key order is Python's dict order.
    let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        ["ok", "status", "provider", "question", "jurisdiction", "retrieved_at", "legal_advice", "sources"]
    );
    let source_keys: Vec<&String> = source.as_object().unwrap().keys().collect();
    assert_eq!(
        source_keys,
        [
            "title", "url", "currency", "excerpt", "page_text", "read_status", "text_truncated", "indexed_at",
            "fetched_at"
        ]
    );
    let now = Utc::now();
    for stamp in [source["fetched_at"].as_str().unwrap(), &result.retrieved_at] {
        assert!(stamp.ends_with('Z') && stamp.len() == "2026-08-24T15:30:14.000Z".len(), "{stamp}");
        let at = DateTime::parse_from_rfc3339(stamp).unwrap();
        let age = (now - at.with_timezone(&Utc)).num_milliseconds();
        assert!((0..5000).contains(&age), "{stamp}");
    }
}

#[test]
fn test_sessions_tracking_fragments_and_query_order_are_canonical_before_read_or_dedup() {
    let base = "https://www.irs.gov/forms?form=1040&year=2025";
    let h = service(&[
        url_row("https://WWW.IRS.GOV:443/forms;jsessionid=one?year=2025&utm_source=mail&form=1040#section"),
        url_row("https://www.irs.gov/forms%3Bjsessionid%3Dtwo?form=1040&year=2025&SESSION_ID=private"),
        url_row(&format!("{base}&fbclid=tracker")),
        url_row(&format!("{base}&year=2024")),
    ]);
    let result = h.lookup.query(QUESTION, JURISDICTION).unwrap();
    let expected = vec![base.to_string(), format!("{base}&year=2024")];
    assert_eq!(urls(&result), expected);
    assert_eq!(h.fetch.all().into_iter().map(|(u, _)| u).collect::<Vec<_>>(), expected);
    assert_eq!(h.runner.count(), 1);
    assert!(!result.to_value().to_string().contains("private"));
}

#[test]
fn test_invalid_nonofficial_credential_or_private_literal_links_are_not_read() {
    let strings = [
        "http://www.irs.gov/a",
        "https://www.irs.gov.evil.example/a",
        "https://gov/a",
        "https://evil.example/a",
        "https://user:secret@www.irs.gov/a",
        "https://@www.irs.gov/a",
        "https://www.irs.gov:8443/a",
        "https://127.0.0.1/a",
        "https://[::1]/a",
        "https://localhost/a",
        "https://192.168.1.1/a",
        "file:///tmp/a",
        "https://www.irs.gov/a\\b",
        "https://www.irs.gov/a\nb",
        "https://www.irs.gov/a%00b",
        "https://www.irs.gov/a%5Cb",
        "https://www.irs.gov/a%xx",
        "https://www.irs.gov/a?q=%0a",
        "https://bad_host.gov/a",
        "https://www.irs.gov../a",
        "https://[broken/a",
        "https://www.irs.gov:bad/a",
    ];
    let mut values: Vec<Value> = strings.iter().map(|s| json!(s)).collect();
    values.push(Value::Null);
    values.push(json!({}));
    for value in values {
        // A real official positive control prevents an absence-only assertion.
        let h = service(&[row(value.clone(), &[]), url_row("https://www.army.mil/legal")]);
        let result = h.lookup.query(QUESTION, JURISDICTION).unwrap();
        assert_eq!(urls(&result), ["https://www.army.mil/legal"], "{value}");
        let fetches = h.fetch.all();
        assert!(fetches.len() == 1 && fetches[0].0 == "https://www.army.mil/legal", "{value}");
        if let Some(s) = value.as_str() {
            assert!(!official_guard(s).is_empty(), "{s}");
        }
        assert_eq!(official_guard("https://www.army.mil/legal"), "");
    }
}

#[test]
fn test_canonicalization_retains_statute_identifiers_repeated_values_and_encoded_path() {
    assert_eq!(
        canonical_url("https://uscode.house.gov/%7Ecode%20page?title=42&section=1983&id=x&sid=y&x=b&x=a").as_deref(),
        Some("https://uscode.house.gov/~code%20page?id=x&section=1983&sid=y&title=42&x=b&x=a")
    );
    assert_eq!(canonical_url("https://www.irs.gov.").as_deref(), Some("https://www.irs.gov/"));
}

#[test]
fn test_canonical_unicode_query_expansion_respects_final_url_limit() {
    assert_eq!(
        canonical_url("https://www.irs.gov/?query=benefits").as_deref(),
        Some("https://www.irs.gov/?query=benefits")
    );
    let value = format!("https://www.irs.gov/?query={}", "\u{e9}".repeat(400));
    assert!(value.chars().count() < 2000 && canonical_url(&value).is_none());
    let h = service(&[url_row(&value), url_row(URL)]);
    assert_eq!(urls(&h.lookup.query(QUESTION, JURISDICTION).unwrap()), [URL]);
    assert_eq!(h.fetch.count(), 1);
}

#[test]
fn test_only_first_five_rows_and_three_unique_sources_can_be_read() {
    let rows: Vec<Value> = (0..20).map(|n| url_row(&format!("https://www.irs.gov/page/{n}"))).collect();
    let h = service(&rows);
    let result = h.lookup.query(QUESTION, JURISDICTION).unwrap();
    assert!(result.sources.len() == 3 && h.fetch.count() == 3);
    assert_eq!(urls(&result), (0..3).map(|n| format!("https://www.irs.gov/page/{n}")).collect::<Vec<_>>());
    assert_eq!(h.runner.count(), 1);
    let mut rows: Vec<Value> = (0..5).map(|_| url_row("https://evil.example/page")).collect();
    rows.push(url_row(URL));
    let h = service(&rows);
    assert!(failure(h.lookup.query(QUESTION, JURISDICTION)).contains("no usable official sources"));
    assert_eq!(h.fetch.count(), 0);
}

#[test]
fn test_source_text_limits_and_untrusted_index_content_are_bounded() {
    let fetches = Rec::default();
    let body = format!("<main>{}</main>", "p".repeat(9000));
    let h = service_with(
        &[row(
            json!(URL),
            &[
                ("title", json!("t".repeat(1000))),
                ("snippet", json!("e".repeat(3000))),
                ("content", json!("PRIVATE_IGNORED_CONTENT")),
            ],
        )],
        Some(fixed_fetch(200, &body, &fetches)),
        &[],
    );
    let source = h.lookup.query(QUESTION, JURISDICTION).unwrap().sources.remove(0);
    assert_eq!(source.title.chars().count(), 500);
    assert!(source.excerpt.chars().count() == 2000 && source.page_text.chars().count() == 6000);
    assert!(source.text_truncated && source.currency == "unverified");
    assert!(!serde_json::to_string(&source).unwrap().contains("PRIVATE_IGNORED_CONTENT"));
}

#[test]
fn test_failed_or_unreadable_fresh_reads_keep_indexed_excerpt_without_freshness_claim() {
    let too_big = "x".repeat(watch::MAX_BYTES + 1);
    let cases: Vec<MakeFetch> = vec![
        Box::new(|r| failing_fetch("PRIVATE_QUESTION Bearer PRIVATE_KEY provider account error", r)),
        Box::new(|r| fixed_fetch(403, "PRIVATE_KEY", r)),
        Box::new(|r| fixed_fetch(200, "<script>hidden</script>", r)),
        Box::new(move |r| fixed_fetch(200, &too_big, r)),
    ];
    for make in cases {
        let fetches = Rec::default();
        let h = service_with(&[url_row(URL)], Some(make(&fetches)), &[]);
        let source = h.lookup.query(QUESTION, JURISDICTION).unwrap().sources.remove(0);
        assert_eq!(source.excerpt, "Indexed official snippet.");
        assert!(source.page_text.is_empty() && source.fetched_at.is_none());
        assert!(source.read_status == "indexed_only" && source.currency == "unverified");
        assert!(!serde_json::to_string(&source).unwrap().contains("PRIVATE_"));
        assert!(h.runner.count() == 1 && fetches.count() == 1);
    }
}

#[test]
fn test_blank_excerpt_needs_live_text_to_be_usable() {
    let h = service(&[row(json!(URL), &[("snippet", json!(""))])]);
    assert!(!h.lookup.query(QUESTION, JURISDICTION).unwrap().sources[0].page_text.is_empty());
    let fetches = Rec::default();
    let h = service_with(&[row(json!(URL), &[("snippet", json!(""))])], Some(failing_fetch("PRIVATE_BODY", &fetches)), &[]);
    assert!(failure(h.lookup.query(QUESTION, JURISDICTION)).contains("no usable official sources"));
    assert_eq!(fetches.count(), 1);
}

#[test]
fn test_pdf_and_binary_documents_never_become_live_text_with_html_positive_control() {
    let fetches: Rec<(String, f64)> = Rec::default();
    let calls = fetches.clone();
    let fetch: Fetch = Arc::new(move |url, timeout| {
        calls.push((url.to_string(), timeout));
        let body = if calls.count() == 1 { "\u{feff} \n%PDF-1.7\nA decoded binary document." } else { PAGE };
        Ok((200, body.to_string()))
    });
    let h = service_with(&[url_row(&format!("{URL}?format=pdf")), url_row(URL)], Some(fetch), &[]);
    let sources = h.lookup.query(QUESTION, JURISDICTION).unwrap().sources;
    assert_eq!(sources[0].read_status, "indexed_only");
    assert!(sources[0].page_text.is_empty() && sources[0].fetched_at.is_none());
    assert!(sources[1].read_status == "live" && sources[1].page_text.contains("Official page text."));
    assert_eq!(fetches.count(), 2);
    assert!(sources.iter().all(|s| s.currency == "unverified"));
}

#[test]
fn test_invalid_index_timestamp_is_null() {
    // NaN, infinity and 10**400 cannot be held by a JSON value; index::indexed_at's f64 path rejects them in
    // the python_cases table instead.
    for value in [json!(true), json!("1787585414000"), Value::Null, json!(-1)] {
        let h = service(&[row(json!(URL), &[("updated_at", value.clone())])]);
        assert_eq!(h.lookup.query(QUESTION, JURISDICTION).unwrap().sources[0].indexed_at, None, "{value}");
    }
}

#[test]
fn test_millisecond_precision_and_first_ranked_duplicate_metadata_are_preserved() {
    let h = service(&[
        row(json!(URL), &[("updated_at", json!(1787585414123_i64)), ("snippet", json!("First ranked excerpt."))]),
        row(
            json!(format!("{URL};jsessionid=duplicate")),
            &[("updated_at", json!(1787586414999_i64)), ("snippet", json!("Different duplicate excerpt."))],
        ),
    ]);
    let result = h.lookup.query(QUESTION, JURISDICTION).unwrap();
    assert!(result.sources.len() == 1 && h.fetch.count() == 1);
    assert_eq!(result.sources[0].indexed_at.as_deref(), Some("2026-08-24T15:30:14.123Z"));
    assert_eq!(result.sources[0].excerpt, "First ranked excerpt.");
}

#[test]
fn test_unreported_or_invalid_credit_cost_is_metered_unpriced() {
    // NaN, infinity and 10**400 cannot reach Rust as JSON numbers (serde_json refuses them).
    for credits in [json!(true), json!(-2), json!("2"), Value::Null] {
        let meter = Rec::default();
        let runner = Rec::default();
        let fetches = Rec::default();
        let lookup = GovernmentIndex::default()
            .with_ctx(offline_ctx(&[]))
            .with_runner(runner_of(reply(&[url_row(URL)], credits.clone()), &runner))
            .with_fetch(fixed_fetch(200, PAGE, &fetches))
            .with_meter(recording_meter(&meter));
        assert!(lookup.query(QUESTION, JURISDICTION).unwrap().ok);
        assert_eq!(meter.all()[0].1, None, "{credits}");
    }
}

#[test]
fn test_default_source_reader_passes_only_url_timeout_and_official_redirect_guard() {
    let seen: Rec<(String, HttpOptions)> = Rec::default();
    let calls = seen.clone();
    let ctx = offline_ctx(&[]).with_http(Arc::new(move |url: &str, opts: &HttpOptions| -> Result<(u16, String), FetchError> {
        calls.push((url.to_string(), opts.clone()));
        Ok((200, PAGE.to_string()))
    }));
    let runner = Rec::default();
    let lookup = GovernmentIndex::default()
        .with_ctx(ctx)
        .with_runner(runner_of(reply(&[url_row(URL)], json!(2)), &runner))
        .with_meter(recording_meter(&Rec::default()));
    assert_eq!(lookup.query(QUESTION, JURISDICTION).unwrap().sources[0].read_status, "live");
    let calls = seen.all();
    assert_eq!(calls.len(), 1);
    let (url, opts) = &calls[0];
    assert_eq!(url, URL);
    assert!(0.0 < opts.timeout && opts.timeout <= index::MAX_READ_SECS);
    let guard = opts.url_guard.as_ref().expect("the official guard is passed");
    assert_eq!(guard("https://www.irs.gov/redirected"), "");
    assert!(!guard("https://evil.example/redirected").is_empty());
    assert!(!guard("http://www.irs.gov/redirected").is_empty());
    assert_eq!(official_guard("https://www.irs.gov/redirected"), "");
    assert!(!official_guard("https://evil.example/redirected").is_empty());
    assert!(!official_guard("http://www.irs.gov/redirected").is_empty());
    assert!(opts.headers.is_empty() && opts.data.is_none());
}

#[test]
fn test_default_guarded_reader_cannot_connect_to_private_dns() {
    let connections: Rec<String> = Rec::default();
    let seen = connections.clone();
    let rules = AddressRules {
        lookup: Arc::new(move |host: &str, _port: u16| {
            seen.push(host.to_string());
            Err(std::io::Error::other("private connection attempted"))
        }),
        ip_public: Arc::new(|_| false),
        // check_url's resolve: "private address" for every host, deterministically.
        resolve: Some(Arc::new(|_: &str| false)),
    };
    // Exercise the real guarded reader while its DNS rejection is deterministic.
    let ctx = offline_ctx(&[]).with_http(Arc::new(move |url: &str, opts: &HttpOptions| {
        let opts = HttpOptions { rules: Some(rules.clone()), ..opts.clone() };
        watch::net::http_request(url, &opts)
    }));
    let runner = Rec::default();
    let lookup = GovernmentIndex::default()
        .with_ctx(ctx)
        .with_runner(runner_of(reply(&[url_row(URL)], json!(2)), &runner))
        .with_meter(recording_meter(&Rec::default()));
    let source = lookup.query(QUESTION, JURISDICTION).unwrap().sources.remove(0);
    assert!(source.read_status == "indexed_only" && source.fetched_at.is_none());
    assert_eq!(connections.count(), 0);
}

#[test]
fn test_malformed_provider_result_fails_safely_without_source_reads() {
    for payload in [Value::Null, json!([]), json!({}), json!({"data": []}), json!({"data": {"data": {}}})] {
        let runner = Rec::default();
        let fetches = Rec::default();
        let lookup = GovernmentIndex::default()
            .with_ctx(offline_ctx(&[]))
            .with_runner(runner_of(payload.clone(), &runner))
            .with_fetch(fixed_fetch(200, PAGE, &fetches))
            .with_meter(recording_meter(&Rec::default()));
        assert!(failure(lookup.query(QUESTION, JURISDICTION)).contains("unreadable source result"), "{payload}");
        assert_eq!(runner.count(), 1);
        assert_eq!(fetches.count(), 0);
    }
}

#[test]
fn test_upstream_exception_and_meter_exception_never_reveal_private_bodies_or_retry() {
    let runs: Rec<f64> = Rec::default();
    let seen = runs.clone();
    let fetches = Rec::default();
    let meter = Rec::default();
    let lookup = GovernmentIndex::default()
        .with_ctx(offline_ctx(&[]))
        .with_runner(Arc::new(move |_, timeout| {
            seen.push(timeout);
            Err(anyhow::anyhow!("PRIVATE_KEY PRIVATE_QUESTION"))
        }))
        .with_fetch(fixed_fetch(200, PAGE, &fetches))
        .with_meter(recording_meter(&meter));
    let why = failure(lookup.query(QUESTION, JURISDICTION));
    assert_eq!(why, "Government index is unavailable. No alternate provider was called.");
    assert!(!why.contains("PRIVATE_"));
    assert_eq!(runs.count(), 1);
    assert_eq!(fetches.count(), 0);
    assert_eq!(meter.count(), 0);
    let runner = Rec::default();
    let lookup = GovernmentIndex::default()
        .with_ctx(offline_ctx(&[]))
        .with_runner(runner_of(reply(&[url_row(URL)], json!(2)), &runner))
        .with_fetch(fixed_fetch(200, PAGE, &fetches))
        .with_meter(Arc::new(|_, _, _, _| Err(anyhow::anyhow!("PRIVATE_KEY"))));
    let why = failure(lookup.query(QUESTION, JURISDICTION));
    assert!(why.contains("usage could not be recorded") && !why.contains("PRIVATE_"));
    assert_eq!(runner.count(), 1);
    assert_eq!(fetches.count(), 0);
}

#[test]
fn test_invalid_input_never_calls_provider() {
    // Python's None question or jurisdiction cannot reach a `&str`.
    let long_q = "q".repeat(4001);
    let long_j = "j".repeat(101);
    for (question, jurisdiction) in
        [("", JURISDICTION), (" ", JURISDICTION), (QUESTION, ""), (long_q.as_str(), JURISDICTION), (QUESTION, long_j.as_str())]
    {
        let runner = Rec::default();
        let fetches = Rec::default();
        let lookup = GovernmentIndex::default()
            .with_ctx(offline_ctx(&[]))
            .with_runner(runner_of(json!({}), &runner))
            .with_fetch(fixed_fetch(200, PAGE, &fetches));
        assert!(failure(lookup.query(question, jurisdiction)).contains("question and U.S. jurisdiction"));
        assert_eq!(runner.count(), 0);
        assert_eq!(fetches.count(), 0);
    }
    // At the limits the provider is called.
    let h = service(&[url_row(URL)]);
    assert!(h.lookup.query(&"q".repeat(4000), &"j".repeat(100)).is_ok());
}

#[test]
fn test_invalid_timeout_is_rejected() {
    // True, None, "55" and 10**400 cannot reach an f64 parameter.
    for timeout in [0.0, -1.0, f64::INFINITY, f64::NAN] {
        let err = GovernmentIndex::new(timeout).err().expect("rejected");
        assert!(err.to_string().contains("finite and positive"));
    }
    assert_eq!(GovernmentIndex::new(120.0).unwrap().timeout_secs(), index::MAX_QUERY_SECS);
    assert_eq!(GovernmentIndex::new(12.0).unwrap().timeout_secs(), 12.0);
}

#[test]
fn test_read_timeout_uses_remaining_total_budget_and_stops_new_reads() {
    let clock = Arc::new(Mutex::new(0.0_f64));
    let calls: Rec<(String, f64)> = Rec::default();
    let (c1, c2, c3) = (clock.clone(), clock.clone(), calls.clone());
    let runner: Runner = Arc::new(move |_, timeout| {
        assert_eq!(timeout, 12.0);
        *c1.lock().unwrap() = 9.0;
        Ok(reply(&[url_row(URL), url_row("https://www.irs.gov/second"), url_row("https://www.irs.gov/third")], json!(2)))
    });
    let fetch: Fetch = Arc::new(move |url, timeout| {
        c3.push((url.to_string(), timeout));
        *c2.lock().unwrap() += timeout;
        Err(anyhow::anyhow!("PRIVATE_QUESTION"))
    });
    let reading = clock.clone();
    let result = GovernmentIndex::new(12.0)
        .unwrap()
        .with_ctx(offline_ctx(&[]))
        .with_runner(runner)
        .with_fetch(fetch)
        .with_meter(recording_meter(&Rec::default()))
        .with_clock(Arc::new(move || *reading.lock().unwrap()))
        .query(QUESTION, JURISDICTION)
        .unwrap();
    assert_eq!(calls.all(), vec![(URL.to_string(), 3.0)]);
    assert!(result.sources.len() == 3 && result.sources.iter().all(|s| s.read_status == "indexed_only"));
    let runner = Rec::default();
    let fetches = Rec::default();
    let lookup = GovernmentIndex::new(4.0)
        .unwrap()
        .with_ctx(offline_ctx(&[]))
        .with_runner(runner_of(json!({}), &runner))
        .with_fetch(fixed_fetch(200, PAGE, &fetches));
    assert!(failure(lookup.query(QUESTION, JURISDICTION)).contains("no time left to start"));
    assert_eq!(runner.count(), 0);
    assert_eq!(fetches.count(), 0);
}

#[test]
fn test_government_lookup_and_existing_search_share_one_persistent_firecrawl_cap() {
    let dir = tempfile::tempdir().unwrap();
    let count_file = dir.path().join(firecrawl::COUNT_FILE);
    let sent: Rec<(String, Value)> = Rec::default();
    let calls = sent.clone();
    let http = Arc::new(move |url: &str, opts: &HttpOptions| -> Result<(u16, String), FetchError> {
        assert_eq!(opts.header("Authorization"), Some("Bearer offline-key"));
        assert!(opts.timeout > 0.0);
        let body: Value = serde_json::from_slice(opts.data.as_deref().unwrap()).unwrap();
        calls.push((url.to_string(), body));
        if url == firecrawl::SEARCH_URL {
            return Ok((200, json!({"success": true, "creditsUsed": 1, "data": {"web": []}}).to_string()));
        }
        Ok((
            200,
            json!({"success": true, "data": {"alexandria": [
                {"creditsCost": 2, "data": reply(&[url_row(URL)], json!(2))["data"]}]}})
            .to_string(),
        ))
    });
    let make_ctx = || {
        let mut ctx = offline_ctx(&[("FIRECRAWL_API_KEY", "offline-key"), ("EVE_FIRECRAWL_PER_DAY", "2")])
            .with_http(http.clone())
            .with_ledger(eve_spend::Ledger::at(dir.path()));
        ctx.firecrawl_calls = Arc::new(DayCount::at(&count_file));
        ctx
    };
    let ctx = make_ctx();
    firecrawl::search(&ctx, "Existing shared-brain search", firecrawl::RESULTS, firecrawl::SEARCH_TIMEOUT_SECS, false)
        .unwrap();
    let fetches = Rec::default();
    let fetch = fixed_fetch(200, PAGE, &fetches);
    let lookup = GovernmentIndex::default().with_ctx(ctx.clone()).with_fetch(fetch.clone());
    assert!(lookup.query(QUESTION, JURISDICTION).unwrap().ok);
    assert_eq!(firecrawl::left(&ctx, None), 0);
    // Simulate a restart: the shared count reloads from the suite's temporary ledger.
    let lookup = GovernmentIndex::default().with_ctx(make_ctx()).with_fetch(fetch);
    assert!(failure(lookup.query(QUESTION, JURISDICTION)).contains("Government index is unavailable"));
    let sent = sent.all();
    assert_eq!(sent.iter().map(|(u, _)| u.as_str()).collect::<Vec<_>>(), [firecrawl::SEARCH_URL, firecrawl::TOOL_URL]);
    assert_eq!(sent[1].1["alexandria"][0]["provider"], "firecrawl-gov-index");
    assert_eq!(fetches.count(), 1);
    // The default meter wrote the lookup into the spend ledger with its credits.
    let ledger: String = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .map(|e| std::fs::read_to_string(e.path()).unwrap())
        .collect();
    assert!(ledger.contains("alexandria firecrawl-gov-index") && ledger.contains("2 credits"), "{ledger}");
}

/// Python 3.13's own answers (tests/python_cases.json, generated from america_index.py) for canonical_url,
/// _text, _indexed_at and _readable_body.
#[test]
fn test_python_reference_cases() {
    let cases: Value = serde_json::from_str(include_str!("python_cases.json")).unwrap();
    for case in cases["canonical"].as_array().unwrap() {
        let input = case["in"].as_str().unwrap();
        assert_eq!(canonical_url(input).map(Value::String).unwrap_or(Value::Null), case["out"], "{input:?}");
        let guard = official_guard(input);
        assert_eq!(guard.is_empty(), !case["out"].is_null());
    }
    for case in cases["text"].as_array().unwrap() {
        let (text, truncated) = index::text(&case["in"], case["limit"].as_u64().unwrap() as usize);
        assert_eq!((json!(text), json!(truncated)), (case["text"].clone(), case["truncated"].clone()), "{case}");
    }
    for case in cases["indexed_at"].as_array().unwrap() {
        assert_eq!(index::indexed_at(&case["in"]).map(Value::String).unwrap_or(Value::Null), case["out"], "{case}");
    }
    for case in cases["readable"].as_array().unwrap() {
        assert_eq!(json!(index::readable_body(case["in"].as_str().unwrap())), case["out"], "{case}");
    }
    assert!(!index::readable_body(&"x".repeat(watch::MAX_BYTES + 1)));
    assert!(index::readable_body(&"x".repeat(watch::MAX_BYTES)));
    // Multi-byte text is measured in characters, as Python's len() does.
    assert!(index::readable_body(&"\u{e9}".repeat(watch::MAX_BYTES)));
}
