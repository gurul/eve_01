//! Port of test_science_search.py's log test, in its own test binary: tracing's per-callsite interest is
//! process-wide, so a scoped subscriber in a binary whose other tests log from the same callsites on other
//! threads can miss lines.

use std::io::Write;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Value};

use eve_science_search as ss;
use ss::{HttpRequest, HttpResponse, Limiter, SearchOptions, Transport, TransportError};

/// Answers every request with one status and body.
struct Fixed(u16, Value);

#[async_trait]
impl Transport for Fixed {
    async fn get(&self, _request: HttpRequest) -> Result<HttpResponse, TransportError> {
        Ok(HttpResponse { status: self.0, headers: Vec::new(), body: serde_json::to_vec(&self.1).unwrap() })
    }
}

async fn run(query: &str, transport: Fixed) -> Value {
    let args = json!({"query": query, "k": null, "year_from": null, "year_to": null, "reviews_only": null,
                      "open_access_only": null});
    let opts = SearchOptions {
        transport: Some(Arc::new(transport)),
        limiter: Some(Arc::new(Limiter::default())),
        sleep: Some(Arc::new(|_s| Box::pin(async {}))),
        now: None,
        clock: None,
    };
    ss::search(&args, &opts).await
}

/// A log writer that keeps every line.
#[derive(Clone, Default)]
struct Lines(Arc<Mutex<Vec<u8>>>);

impl Write for Lines {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn test_the_owners_words_never_reach_the_log() {
    let secret = "zebrafish fin regeneration";
    let lines = Lines::default();
    let sink = lines.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(ss::hide_queries(move || sink.clone()))
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let body = json!({"mode": "semantic", "results": [{"title": "A paper", "year": 2020}]});
    assert_eq!(run(secret, Fixed(200, body)).await["ok"], json!(true));
    run(secret, Fixed(500, json!("x"))).await;
    // httpx's request line, as a library would log it: the q value hidden, only on this service's URLs
    tracing::info!("HTTP Request: GET {}?q=zebrafish+fin&k=8 \"HTTP/1.1 200 OK\"", ss::SEARCH_URL);
    tracing::info!("GET https://example.org/?q=goldfish");
    let text = String::from_utf8(lines.0.lock().clone()).unwrap();
    assert!(!text.contains("zebrafish") && text.contains("science search:"), "{text}");
    assert!(text.contains("q=<hidden>&k=8") && text.contains("q=goldfish"), "{text}");

    let record = format!("HTTP Request: GET {}?q=zebrafish+fin&k=8 \"HTTP/1.1 200 OK\"", ss::SEARCH_URL);
    let filtered = ss::QueryFilter.filter(&record);
    assert!(!filtered.contains("zebrafish") && filtered.contains("q=<hidden>&k=8"));
    let other = ss::QueryFilter.filter("GET https://example.org/?q=zebrafish");
    assert!(other.contains("zebrafish"));
}

