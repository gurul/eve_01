//! `tests/test_america_alexandria.py`: the default government-index transport, offline.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Value};

use super::run;
use crate::*;

const QUESTION: &str = "Where can I read the United States Code?";
const URL: &str = "https://uscode.house.gov/currency/currency.shtml";

fn args() -> Value {
    json!({"question": QUESTION, "jurisdiction": "U.S. federal"})
}

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// `Mock()` as an index: records each query and answers with `answer`.
struct Index {
    calls: Mutex<Vec<(String, String)>>,
    answer: Box<dyn Fn() -> anyhow::Result<Value> + Send + Sync>,
}

impl Index {
    fn new(answer: impl Fn() -> anyhow::Result<Value> + Send + Sync + 'static) -> Arc<Index> {
        Arc::new(Index { calls: Mutex::new(Vec::new()), answer: Box::new(answer) })
    }
}

impl GovIndex for Index {
    fn query(&self, question: &str, jurisdiction: &str) -> anyhow::Result<Value> {
        self.calls.lock().push((question.to_string(), jurisdiction.to_string()));
        (self.answer)()
    }
}

/// `Mock()` as a browser: records each ask.
#[derive(Default)]
struct Browser {
    asks: AtomicUsize,
}

#[async_trait]
impl AskBrowser for Browser {
    async fn ask(&self, _question: &str, _jurisdiction: &str) -> anyhow::Result<(String, Vec<Source>)> {
        self.asks.fetch_add(1, Ordering::SeqCst);
        Err(anyhow::anyhow!("browser attempted"))
    }

    async fn close(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

/// A transport that records every request it is given.
#[derive(Default)]
struct Http {
    posts: AtomicUsize,
}

#[async_trait]
impl ChatTransport for Http {
    async fn post(&self, _url: &str, _h: &[(&str, &str)], _b: Vec<u8>, _l: Limits) -> Result<ChatResponse, HttpError> {
        self.posts.fetch_add(1, Ordering::SeqCst);
        Err(HttpError("legacy HTTP attempted".into()))
    }
}

#[test]
fn test_default_factory_uses_index_only_when_enabled_and_key_is_configured() {
    assert_eq!(configured(Some(&env(&[]))).unwrap().transport, Transport::Alexandria);
    assert!(make_america(Some(&env(&[("FIRECRAWL_API_KEY", "mock-key")]))).unwrap().is_none());
    let missing = make_america(Some(&env(&[("EVE_AMERICA", "1")]))).unwrap().unwrap();
    assert!(!missing.has_index());
    assert_eq!(run(missing.handle("america_ask", &args())).unwrap()["status"], json!("unavailable"));
    let service = make_america(Some(&env(&[("EVE_AMERICA", "1"), ("FIRECRAWL_API_KEY", "mock-key")]))).unwrap().unwrap();
    assert!(service.has_index() && !service.has_browser());
    // GovernmentIndex(timeout_secs=55.0)
    assert_eq!(index_budget(&service.config), 55.0);
    // a blank key is no key
    let blank = make_america(Some(&env(&[("EVE_AMERICA", "1"), ("FIRECRAWL_API_KEY", "  ")]))).unwrap().unwrap();
    assert!(!blank.has_index());
}

#[test]
fn test_index_validation_precedes_any_external_call() {
    let mut history = args();
    history["history"] = json!(["PRIVATE_HISTORY"]);
    let mut no_jurisdiction = args();
    no_jurisdiction["jurisdiction"] = Value::Null;
    let mut blank = args();
    blank["question"] = json!(" ");
    for (case, status) in [(history, "invalid_request"), (no_jurisdiction, "needs_jurisdiction"), (blank, "invalid_request")] {
        let index = Index::new(|| Ok(json!({"ok": true})));
        let service = America::new(AmericaConfig::on(true)).with_index(index.clone());
        assert_eq!(run(service.handle("america_ask", &case)).unwrap()["status"], json!(status));
        assert!(index.calls.lock().is_empty());
    }
}

#[test]
fn test_index_failure_does_not_call_browser_or_http() {
    let index = Index::new(|| {
        Err(anyhow::Error::new(eve_america_index::IndexFailure("Government index is unavailable.".to_string())))
    });
    let browser = Arc::new(Browser::default());
    let http = Arc::new(Http::default());
    let service = America::new(AmericaConfig::on(true))
        .with_index(index.clone())
        .with_browser(browser.clone())
        .with_transport(http.clone());
    let result = run(service.handle("america_ask", &args())).unwrap();
    assert_eq!(
        result,
        json!({"ok": false, "status": "upstream_unavailable", "reason": "Government index is unavailable.",
               "america_url": AMERICA_URL})
    );
    assert_eq!(*index.calls.lock(), vec![(QUESTION.to_string(), "U.S. federal".to_string())]);
    assert_eq!(browser.asks.load(Ordering::SeqCst), 0);
    assert_eq!(http.posts.load(Ordering::SeqCst), 0);
}

#[test]
fn test_index_runs_off_event_loop_and_timeout_does_not_retry() {
    let gate = Arc::new((StdMutex::new((false, false)), Condvar::new())); // (entered, released)
    let worker_gate = gate.clone();
    let index = Index::new(move || {
        let (lock, cv) = &*worker_gate;
        let mut state = lock.lock().unwrap();
        state.0 = true;
        cv.notify_all();
        let (state, timeout) = cv.wait_timeout_while(state, Duration::from_secs(2), |s| !s.1).unwrap();
        assert!(!timeout.timed_out() || state.1, "test worker was not released");
        Ok(json!({"ok": true}))
    });
    let service = America::new(AmericaConfig { timeout_secs: 0.05, ..AmericaConfig::on(true) }).with_index(index.clone());
    let release = gate.clone();
    let result = run(async move {
        let result = service.handle("america_ask", &args()).await;
        // release the worker before the runtime waits for its blocking threads
        let (lock, cv) = &*release;
        lock.lock().unwrap().1 = true;
        cv.notify_all();
        result
    })
    .unwrap();
    assert!(gate.0.lock().unwrap().0, "the query ran");
    assert_eq!(result["status"], json!("upstream_unavailable"));
    assert!(result["reason"].as_str().unwrap().contains("timed out"));
    assert_eq!(*index.calls.lock(), vec![(QUESTION.to_string(), "U.S. federal".to_string())]);
}

/// The parts of `test_real_index_bundle_reaches_menu_and_call_synthesis` that are this crate's: the real
/// GovernmentIndex behind `america_ask`, its source bundle, and the bundle's saved contract. The Telegram and
/// call paths around it are the telegram crate's.
#[test]
fn test_real_index_bundle_reaches_menu_and_call_synthesis() {
    let calls: Arc<Mutex<Vec<Value>>> = Arc::default();
    let reads: Arc<Mutex<Vec<String>>> = Arc::default();
    let (c, r) = (calls.clone(), reads.clone());
    let runner: eve_america_index::Runner = Arc::new(move |request: &Value, timeout: f64| {
        c.lock().push(request.clone());
        assert!(timeout > 0.0);
        Ok(json!({"data": {"results": 2, "data": [
            {"url": format!("{URL};jsessionid=abc"), "title": "U.S. Code currency", "snippet": "Code information",
             "updated_at": 1787585414000_i64},
            {"url": format!("{URL};jsessionid=def"), "title": "Duplicate", "snippet": "Duplicate"},
        ]}, "credits": 2}))
    });
    let fetch: eve_america_index::Fetch = Arc::new(move |url: &str, timeout: f64| {
        r.lock().push(url.to_string());
        assert!(timeout > 0.0);
        Ok((
            200,
            "<html><body><main><h1>United States Code</h1><p>Official currency information.</p></main></body></html>"
                .to_string(),
        ))
    });
    let meter: eve_america_index::Meter = Arc::new(|_, _, _, _| Ok(()));
    let index = eve_america_index::GovernmentIndex::default().with_runner(runner).with_fetch(fetch).with_meter(meter);
    let service = America::new(AmericaConfig::on(true)).with_index(Arc::new(index));
    assert!(service.instructions().contains("sources_found"));
    assert!(service.tools().contains(&TOOL));
    let result = run(service.handle("america_ask", &args())).unwrap();
    assert_eq!(result["ok"], json!(true));
    assert_eq!(result["status"], json!("sources_found"));
    assert_eq!(result["provider"], json!("firecrawl_gov_index"));
    assert!(result.get("answer").is_none());
    let sources = result["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0]["url"], json!(URL));
    assert_eq!(sources[0]["read_status"], json!("live"));
    assert!(sources[0]["page_text"].as_str().unwrap().contains("Official currency information."));
    assert_eq!(sources[0]["currency"], json!("unverified"));
    assert_eq!(calls.lock().len(), 1);
    assert_eq!(*reads.lock(), vec![URL.to_string()]);
    assert!(!serde_json::to_string(&*calls.lock()).unwrap().contains("PRIVATE_HISTORY_SENTINEL"));

    let contract: Value = serde_json::from_str(
        &std::fs::read_to_string(super::repo_root().join("docs/contracts/america-alexandria.json")).unwrap(),
    )
    .unwrap();
    // The local JSON artifact declares its source-bundle schema; never validate Alexandria source output against
    // the legacy America.gov generated answer.
    assert_eq!(contract["type"], json!("object"));
    assert_eq!(contract["additionalProperties"], json!(false));
    let mut required: Vec<&str> = contract["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    let mut keys: Vec<&str> = result.as_object().unwrap().keys().map(String::as_str).collect();
    let mut props: Vec<&str> = contract["properties"].as_object().unwrap().keys().map(String::as_str).collect();
    required.sort();
    keys.sort();
    props.sort();
    assert_eq!(required, keys);
    assert_eq!(keys, props);
    for (name, value) in result.as_object().unwrap() {
        if let Some(c) = contract["properties"][name].get("const") {
            assert_eq!(value, c, "{name}");
        }
    }
}
