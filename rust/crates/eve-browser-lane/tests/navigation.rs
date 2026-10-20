//! Port of test_browser_navigation_freshness.py and test_webmcp_probe.py: fresh navigation observations and
//! negative controls; the WebMCP probe is measurement only, once per page, host and tool names only.

mod common;

use std::sync::Arc;

use common::*;
use eve_browser_lane as bl;
use eve_browser_lane::driver::{PageHandle, SelectBy};
use eve_desktop_lanes::{error_message, LaneError};
use parking_lot::Mutex;
use serde_json::{json, Value};

struct NavState {
    failures: usize,
    error: String,
    reads: usize,
    waits: Vec<u64>,
    now: f64,
    url_override: Option<String>,
}

#[derive(Clone)]
struct NavigatingPage(Arc<Mutex<NavState>>);

impl NavigatingPage {
    fn new(failures: usize, error: &str) -> NavigatingPage {
        NavigatingPage(Arc::new(Mutex::new(NavState {
            failures,
            error: error.into(),
            reads: 0,
            waits: vec![],
            now: 0.0,
            url_override: None,
        })))
    }

    fn default() -> NavigatingPage {
        NavigatingPage::new(1, "Execution context was destroyed, most likely because of a navigation")
    }

    fn clock(&self) -> bl::ClockFn {
        let me = self.0.clone();
        Arc::new(move || me.lock().now)
    }
}

fn nope() -> anyhow::Error {
    LaneError::err("AttributeError", "not on this fake")
}

impl PageHandle for NavigatingPage {
    fn evaluate(&self, script: &str, _arg: Option<&Value>) -> anyhow::Result<Value> {
        let mut st = self.0.lock();
        st.reads += 1;
        if st.reads <= st.failures {
            return Err(LaneError::err("Error", st.error.clone()));
        }
        if script == bl::COLLECT_JS {
            let url = st.url_override.clone().unwrap_or_else(|| "https://test/destination".into());
            return Ok(json!({"title": "Destination", "url": url, "text_hash": "Fresh body", "elements": []}));
        }
        Ok(json!("Fresh destination body"))
    }
    fn title(&self) -> anyhow::Result<String> {
        Ok("Destination".into())
    }
    fn url(&self) -> String {
        String::new()
    }
    fn inner_text(&self, _s: &str, _t: u64) -> anyhow::Result<String> {
        Err(nope())
    }
    fn count(&self, _s: &str) -> anyhow::Result<usize> {
        Err(nope())
    }
    fn scroll_into_view_if_needed(&self, _s: &str, _t: u64) -> anyhow::Result<()> {
        Err(nope())
    }
    fn click(&self, _s: &str, _t: u64) -> anyhow::Result<()> {
        Err(nope())
    }
    fn fill(&self, _s: &str, _v: &str, _t: u64) -> anyhow::Result<()> {
        Err(nope())
    }
    fn select_option(&self, _s: &str, _c: SelectBy<'_>, _t: u64) -> anyhow::Result<()> {
        Err(nope())
    }
    fn evaluate_on(&self, _s: &str, _script: &str) -> anyhow::Result<Value> {
        Err(nope())
    }
    fn keyboard_type(&self, _t: &str, _d: u64) -> anyhow::Result<()> {
        Err(nope())
    }
    fn keyboard_press(&self, _k: &str) -> anyhow::Result<()> {
        Err(nope())
    }
    fn wait_for_timeout(&self, ms: u64) {
        let mut st = self.0.lock();
        st.waits.push(ms);
        st.now += ms as f64 / 1000.0;
    }
    fn wait_for_load_state(&self, _s: &str, _t: u64) -> anyhow::Result<()> {
        Ok(())
    }
    fn goto(&self, _u: &str, _w: &str, _t: u64) -> anyhow::Result<()> {
        Err(nope())
    }
    fn bring_to_front(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn close(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn screenshot_jpeg(&self, _q: u8, _m: Option<(&str, &str)>) -> anyhow::Result<Vec<u8>> {
        Err(nope())
    }
}

#[test]
fn test_snapshot_retries_navigation_race_and_reads_destination() {
    let page = NavigatingPage::default();
    let senses = bl::Page::new(Box::new(page.clone())).with_clock(page.clock());
    let observed = senses.snapshot().unwrap();
    assert_eq!(observed.title, "Destination");
    assert!(observed.context_lines.contains(&"url: https://test/destination".to_string()));
    let st = page.0.lock();
    assert_eq!(st.reads, 2);
    assert_eq!(st.waits, [bl::NAV_READ_POLL_MS]);
}

#[test]
fn test_navigation_metadata_preserves_destination_or_omits_ambiguity() {
    for (url, expected) in [
        ("HTTPS://Example.TEST:443/guide", "https://example.test/guide"),
        ("http://example.test:80", "http://example.test/"),
        ("https://example.test:8443/guide", "https://example.test:8443/guide"),
        ("https://[::1]:443/guide", "https://[::1]/guide"),
        ("https://other.test/guide", "https://other.test/guide"),
        ("https://example.test/other", "https://example.test/other"),
        ("https://owner:password@example.test/guide", ""),
        ("https://owner@example.test/guide", ""),
        ("https://example.test/guide?lang=en", ""),
        ("https://example.test/guide#section", ""),
        ("https://example.test/guide?", ""),
        ("https://example.test/guide#", ""),
        ("javascript:void(0)", ""),
        ("file:///guide", ""),
        ("/relative", ""),
        ("https://example.test:invalid/guide", ""),
        ("https://example.test/\nsecret", ""),
    ] {
        assert_eq!(bl::navigation_destination(url), expected, "{url:?}");
    }
}

#[test]
fn test_host_context_survives_query_fragment_without_leaking_them() {
    for (url, expected) in [
        ("https://EXAMPLE.test/mail?q=1#inbox", "example.test"),
        ("https://example.test/", "example.test"),
        ("https://owner:password@example.test/mail#inbox", ""),
        ("https://owner@example.test/mail#inbox", ""),
        ("javascript:void(0)", ""),
        ("https://[invalid/", ""),
    ] {
        assert_eq!(bl::observed_site(url), expected, "{url}");
    }
}

#[test]
fn test_outline_metadata_is_attached_to_visible_labelled_links_only() {
    let raw = json!({"title": "Test", "url": "https://test/home", "elements": [
        {"id": "1", "role": "link", "name": "Guide", "href": "https://test/guide"},
        {"id": "2", "role": "link", "name": "Guide elsewhere", "href": "https://other/guide"},
        {"id": "3", "role": "link", "name": "Other path", "href": "https://test/other"},
        {"id": "4", "role": "link", "name": "Query", "href": "https://test/guide?q=1"},
        {"id": "5", "role": "link", "name": "Offscreen", "href": "https://test/below", "offscreen": true},
        {"id": "6", "role": "link", "name": "", "href": "https://test/nameless"},
        {"id": "7", "role": "button", "name": "Button", "href": "https://test/button"}]});
    let lines = bl::outline_lines(&bl::snapshot_from_page(&raw, 1, 0.0).unwrap(), bl::OUTLINE_LIMIT);
    assert_eq!(
        lines,
        [
            "link: Guide; destination: https://test/guide",
            "link: Guide elsewhere; destination: https://other/guide",
            "link: Other path; destination: https://test/other",
            "link: Query",
            "link: Offscreen",
            "button: Button",
        ]
    );
}

#[test]
fn test_current_url_is_full_snapshot_evidence_without_another_read() {
    let page = NavigatingPage::new(0, "Execution context was destroyed, most likely because of a navigation");
    let senses = bl::Page::new(Box::new(page.clone())).with_clock(page.clock());
    assert_eq!(senses.current_url(), "");
    assert_eq!(page.0.lock().reads, 0);
    page.0.lock().url_override = Some("https://test/destination?q=1#section".into());
    senses.snapshot().unwrap();
    assert_eq!(senses.current_url(), "https://test/destination?q=1#section");
    assert_eq!(page.0.lock().reads, 1);
    page.0.lock().failures = 1000;
    let err = senses.snapshot().unwrap_err();
    assert!(error_message(&err).contains("Execution context was destroyed"));
    assert_eq!(senses.current_url(), "");
}

#[test]
fn test_text_oracle_retries_navigation_race_with_fresh_body() {
    let page = NavigatingPage::new(1, "Cannot find context with specified id");
    let senses = bl::Page::new(Box::new(page.clone())).with_clock(page.clock());
    assert!(senses.text_visible("Fresh destination body"));
    assert!(!senses.text_visible("missing expectation"));
    assert_eq!(page.0.lock().waits, [bl::NAV_READ_POLL_MS]);
}

#[test]
fn test_unknown_read_error_is_not_retried_or_reported_as_visible() {
    let page = NavigatingPage::new(10, "Target page, context or browser has been closed");
    let senses = bl::Page::new(Box::new(page.clone())).with_clock(page.clock());
    let err = senses.snapshot().unwrap_err();
    assert!(error_message(&err).contains("has been closed"));
    assert!(!senses.text_visible("Fresh destination body"));
    let st = page.0.lock();
    assert!(st.waits.is_empty() && st.reads == 2);
}

#[test]
fn test_repeated_navigation_errors_are_bounded_and_never_reuse_snapshot() {
    for clock_moves in [true, false] {
        let page = NavigatingPage::new(0, "Execution context was destroyed, most likely because of a navigation");
        let clock: bl::ClockFn = if clock_moves { page.clock() } else { Arc::new(|| 0.0) };
        let senses = bl::Page::new(Box::new(page.clone())).with_clock(clock);
        assert_eq!(senses.snapshot().unwrap().title, "Destination");
        page.0.lock().failures = 1000;
        let err = senses.snapshot().unwrap_err();
        assert!(error_message(&err).contains("Execution context was destroyed"));
        assert!(senses.last_snapshot().is_none());
        let st = page.0.lock();
        assert!(st.waits.len() as u64 <= bl::SNAPSHOT_TIMEOUT_MS / bl::NAV_READ_POLL_MS);
        assert!(st.reads as u64 <= bl::SNAPSHOT_TIMEOUT_MS / bl::NAV_READ_POLL_MS + 2);
    }
}

// ---- test_webmcp_probe.py -------------------------------------------------------------------------------------

struct ProbeState {
    url: String,
    tools: Option<Vec<String>>,
    fail: bool,
    probes: usize,
}

#[derive(Clone)]
struct ProbePage(Arc<Mutex<ProbeState>>);

impl ProbePage {
    fn new(url: &str, tools: Option<&[&str]>, fail: bool) -> ProbePage {
        ProbePage(Arc::new(Mutex::new(ProbeState {
            url: url.into(),
            tools: tools.map(|t| t.iter().map(|s| s.to_string()).collect()),
            fail,
            probes: 0,
        })))
    }
    fn probes(&self) -> usize {
        self.0.lock().probes
    }
}

impl PageHandle for ProbePage {
    fn evaluate(&self, script: &str, _arg: Option<&Value>) -> anyhow::Result<Value> {
        let mut st = self.0.lock();
        if script == bl::COLLECT_JS {
            return Ok(json!({"title": "T", "url": st.url, "text_hash": "x", "elements": []}));
        }
        assert_eq!(script, bl::WEBMCP_JS);
        st.probes += 1;
        if st.fail {
            return Err(LaneError::err("Error", "probe blew up"));
        }
        Ok(match &st.tools {
            None => json!({"api": false}),
            Some(t) => json!({"api": true, "names": t}),
        })
    }
    fn title(&self) -> anyhow::Result<String> {
        Ok("T".into())
    }
    fn url(&self) -> String {
        self.0.lock().url.clone()
    }
    fn inner_text(&self, _s: &str, _t: u64) -> anyhow::Result<String> {
        Err(nope())
    }
    fn count(&self, _s: &str) -> anyhow::Result<usize> {
        Err(nope())
    }
    fn scroll_into_view_if_needed(&self, _s: &str, _t: u64) -> anyhow::Result<()> {
        Err(nope())
    }
    fn click(&self, _s: &str, _t: u64) -> anyhow::Result<()> {
        Err(nope())
    }
    fn fill(&self, _s: &str, _v: &str, _t: u64) -> anyhow::Result<()> {
        Err(nope())
    }
    fn select_option(&self, _s: &str, _c: SelectBy<'_>, _t: u64) -> anyhow::Result<()> {
        Err(nope())
    }
    fn evaluate_on(&self, _s: &str, _script: &str) -> anyhow::Result<Value> {
        Err(nope())
    }
    fn keyboard_type(&self, _t: &str, _d: u64) -> anyhow::Result<()> {
        Err(nope())
    }
    fn keyboard_press(&self, _k: &str) -> anyhow::Result<()> {
        Err(nope())
    }
    fn wait_for_timeout(&self, _ms: u64) {}
    fn wait_for_load_state(&self, _s: &str, _t: u64) -> anyhow::Result<()> {
        Ok(())
    }
    fn goto(&self, _u: &str, _w: &str, _t: u64) -> anyhow::Result<()> {
        Err(nope())
    }
    fn bring_to_front(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn close(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn screenshot_jpeg(&self, _q: u8, _m: Option<(&str, &str)>) -> anyhow::Result<Vec<u8>> {
        Err(nope())
    }
}

fn probe_lines(lines: &[String]) -> Vec<String> {
    lines.iter().filter(|l| l.contains("webmcp probe")).cloned().collect()
}

#[test]
fn test_off_by_default_so_direct_pages_read_once() {
    let page = ProbePage::new("https://shop.test/cart", None, false);
    bl::Page::new(Box::new(page.clone())).snapshot().unwrap();
    assert_eq!(page.probes(), 0);
}

#[test]
fn test_logs_host_and_names_once_per_page() {
    let page = ProbePage::new("https://shop.test/cart?secret=1#frag", Some(&["add_to_cart", "checkout\nINJECT"]), false);
    let ((), lines) = capture_logs(|| {
        let senses = bl::Page::new(Box::new(page.clone())).with_webmcp_probe(true);
        senses.snapshot().unwrap();
        senses.snapshot().unwrap();
        page.0.lock().url = "https://shop.test/cart?secret=2".into(); // same page, new query: not probed again
        senses.snapshot().unwrap();
        assert_eq!(page.probes(), 1);
        page.0.lock().url = "https://shop.test/checkout".into();
        senses.snapshot().unwrap();
        assert_eq!(page.probes(), 2);
    });
    assert_eq!(probe_lines(&lines).len(), 2); // the first three looks probed once; the new page once more
    assert_eq!(
        probe_lines(&lines)[0],
        "browser lane: webmcp probe host=shop.test api=True tools=2 names=add_to_cart,checkoutINJECT"
    );
}

#[test]
fn test_page_without_api_is_still_counted() {
    let ((), lines) = capture_logs(|| {
        bl::Page::new(Box::new(ProbePage::new("https://plain.test/", None, false))).with_webmcp_probe(true).snapshot().unwrap();
    });
    assert_eq!(probe_lines(&lines), ["browser lane: webmcp probe host=plain.test api=False tools=- names=-"]);
}

#[test]
fn test_non_web_pages_are_not_probed() {
    for url in ["about:blank", "file:///tmp/x.html", "chrome://newtab/", ""] {
        let page = ProbePage::new(url, Some(&["x"]), false);
        bl::Page::new(Box::new(page.clone())).with_webmcp_probe(true).snapshot().unwrap();
        assert_eq!(page.probes(), 0, "{url}");
    }
}

#[test]
fn test_failed_probe_is_silent_and_never_fails_the_snapshot() {
    let page = ProbePage::new("https://broken.test/", None, true);
    let (snap, lines) = capture_logs(|| bl::Page::new(Box::new(page.clone())).with_webmcp_probe(true).snapshot().unwrap());
    assert_eq!(snap.title, "T");
    assert_eq!(page.probes(), 1);
    assert!(probe_lines(&lines).is_empty());
}
