//! The watcher (watch.py's Watcher): ports of test_watch.py, test_watch_lean_scheduler.py and
//! test_watch_reverify.py. No network: every request goes to a fake lent in its place, and the clock is a value the
//! test moves. The few tests that need sites use loopback servers with the address checks opened for them.

use std::collections::{HashMap, VecDeque};
use std::io::{Read as _, Write as _};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use eve_web::watch::limiter::{LimiterOptions, RateLimiter};
use eve_web::watch::net::{self, AddressRules, HttpOptions, IpNet};
use eve_web::watch::page::{self, Reading};
use eve_web::watch::rules::{self, Watch};
use eve_web::watch::watcher::{Evaluate, Notify, RenderFn, WatchRef, Watcher, WatcherDeps};
use eve_web::watch::{self as watch, FetchError, WatchConfig};
use eve_web::{Ask, Clock, Ctx, Environ, Http};

// ---- the rig ----------------------------------------------------------------------------------------------------

/// The clock a test moves (Python's `Clock` class).
#[derive(Clone)]
struct TClock(Arc<Mutex<f64>>);

impl TClock {
    fn new() -> Self {
        TClock::at(1_800_000_000.0)
    }
    fn at(t: f64) -> Self {
        TClock(Arc::new(Mutex::new(t)))
    }
    fn t(&self) -> f64 {
        *self.0.lock()
    }
    fn set(&self, t: f64) {
        *self.0.lock() = t;
    }
    fn add(&self, secs: f64) {
        *self.0.lock() += secs;
    }
    fn clock(&self) -> Clock {
        let c = self.0.clone();
        Arc::new(move || *c.lock())
    }
}

fn opts(clock: &TClock) -> LimiterOptions {
    LimiterOptions {
        rate_per_min: 6.0,
        burst: 2,
        host_gap_secs: 20.0,
        backoff_base: 60.0,
        backoff_max: 600.0,
        model_calls_per_day: 2,
        clock: clock.clock(),
        day: Arc::new(|t: f64| ((t / 86400.0).floor() as i64).to_string()),
    }
}

/// test_watch.py's `limiter(clock, **kw)`.
fn limiter(clock: &TClock, f: impl FnOnce(&mut LimiterOptions)) -> RateLimiter {
    let mut o = opts(clock);
    f(&mut o);
    RateLimiter::new(o)
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6 * b.abs().max(1.0)
}

fn run<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a runtime").block_on(f)
}

fn spend_dir() -> PathBuf {
    std::env::temp_dir().join("eve-web-watcher-tests-spend")
}

fn config(path: &Path) -> WatchConfig {
    WatchConfig {
        enabled: true,
        path: path.to_path_buf(),
        rate_per_min: watch::DEFAULT_RATE_PER_MIN,
        burst: watch::DEFAULT_BURST,
        host_gap_secs: watch::DEFAULT_HOST_GAP_SECS,
        model_calls_per_day: watch::DEFAULT_MODEL_CALLS_PER_DAY,
        model: watch::DEFAULT_MODEL.to_string(),
        ticketmaster: false,
        browser: false,
        tls: false,
        hosted: false,
        hosted_calls_per_day: watch::DEFAULT_HOSTED_CALLS_PER_DAY,
        vision_model: watch::DEFAULT_MODEL.to_string(),
        search: eve_web::websearch::SearchConfig::default(),
    }
}

type FetchFn = Arc<dyn Fn(&str, &HttpOptions) -> Result<(u16, String), FetchError> + Send + Sync>;

/// A fetch the test can swap later (Python's `w._fetch = ...`).
#[derive(Clone)]
struct Swap(Arc<Mutex<FetchFn>>);

impl Swap {
    fn new(f: impl Fn(&str, &HttpOptions) -> Result<(u16, String), FetchError> + Send + Sync + 'static) -> Self {
        Swap(Arc::new(Mutex::new(Arc::new(f))))
    }
    fn set(&self, f: impl Fn(&str, &HttpOptions) -> Result<(u16, String), FetchError> + Send + Sync + 'static) {
        *self.0.lock() = Arc::new(f);
    }
}

impl Swap {
    /// As a `PageFn` (Python's `w._tls = ...` / `w._crawl = ...`): the same swappable fake, by URL alone.
    fn page(&self) -> eve_web::watch::watcher::PageFn {
        let me = self.clone();
        Arc::new(move |url: &str| {
            let f = me.0.lock().clone();
            f(url, &HttpOptions::default())
        })
    }
}

impl Http for Swap {
    fn request(&self, url: &str, opts: &HttpOptions) -> Result<(u16, String), FetchError> {
        let f = self.0.lock().clone();
        f(url, opts)
    }
}

type RenderInner = Arc<dyn Fn(&str) -> Result<(String, Vec<u8>), FetchError> + Send + Sync>;

/// A render the test can swap later (Python's `w._render = ...`).
#[derive(Clone)]
struct SwapRender(Arc<Mutex<RenderInner>>);

impl SwapRender {
    fn new(f: impl Fn(&str) -> Result<(String, Vec<u8>), FetchError> + Send + Sync + 'static) -> Self {
        SwapRender(Arc::new(Mutex::new(Arc::new(f))))
    }
    fn set(&self, f: impl Fn(&str) -> Result<(String, Vec<u8>), FetchError> + Send + Sync + 'static) {
        *self.0.lock() = Arc::new(f);
    }
    fn lent(&self) -> RenderFn {
        let me = self.clone();
        Arc::new(move |url: &str| {
            let f = me.0.lock().clone();
            f(url)
        })
    }
}

/// An ask the test can swap later (Python's `w._ask = ...`).
#[derive(Clone)]
struct SwapAsk(Arc<Mutex<Ask>>);

impl SwapAsk {
    fn new(f: impl Fn(&Value) -> Result<Value, FetchError> + Send + Sync + 'static) -> Self {
        SwapAsk(Arc::new(Mutex::new(Arc::new(f))))
    }
    fn lent(&self) -> Ask {
        let me = self.clone();
        Arc::new(move |body: &Value| {
            let f = me.0.lock().clone();
            f(body)
        })
    }
}

/// `make_watcher(path, fetch=, ask=, clock=, notify=, **cfg)` and its siblings in the other two files.
struct Make {
    fetch: Swap,
    ask: SwapAsk,
    render: SwapRender,
    clock: TClock,
    notify: Option<Notify>,
    env: Environ,
    offload: bool,
    limiter: Option<RateLimiter>,
    shared_limiter: Option<Arc<Mutex<RateLimiter>>>,
    evaluate: Option<Evaluate>,
    tls: Option<Swap>,
    crawl: Option<Swap>,
    ledger: Option<PathBuf>,
    cfg: Box<dyn FnOnce(&mut WatchConfig)>,
}

impl Make {
    fn new() -> Self {
        Make {
            fetch: Swap::new(|_, _| Ok((200, String::new()))),
            ask: SwapAsk::new(|_| Ok(json!({}))),
            render: SwapRender::new(|_| Err(FetchError::new("no render in this test"))),
            clock: TClock::new(),
            notify: None,
            env: Environ::new(),
            offload: false,
            limiter: None,
            shared_limiter: None,
            evaluate: None,
            tls: None,
            crawl: None,
            ledger: None,
            cfg: Box::new(|_| {}),
        }
    }
    fn fetch(mut self, f: impl Fn(&str, &HttpOptions) -> Result<(u16, String), FetchError> + Send + Sync + 'static) -> Self {
        self.fetch = Swap::new(f);
        self
    }
    fn ask(mut self, f: impl Fn(&Value) -> Result<Value, FetchError> + Send + Sync + 'static) -> Self {
        self.ask = SwapAsk::new(f);
        self
    }
    fn clock(mut self, c: &TClock) -> Self {
        self.clock = c.clone();
        self
    }
    fn notify(mut self, n: Notify) -> Self {
        self.notify = Some(n);
        self
    }
    fn cfg(mut self, f: impl FnOnce(&mut WatchConfig) + 'static) -> Self {
        self.cfg = Box::new(f);
        self
    }
    fn env(mut self, k: &str, v: &str) -> Self {
        self.env.insert(k.to_string(), v.to_string());
        self
    }
    /// test_watch.py's rig: a limiter that lets almost everything through.
    fn build(self, path: &Path) -> Watcher {
        let clock = self.clock.clone();
        let lim = self.limiter.unwrap_or_else(|| {
            limiter(&clock, |o| {
                o.burst = 50;
                o.rate_per_min = 600.0;
                o.host_gap_secs = 0.0;
                o.model_calls_per_day = 50;
            })
        });
        let ctx = Ctx::global()
            .clone()
            .with_env(self.env)
            .with_ledger(eve_spend::Ledger::at(self.ledger.clone().unwrap_or_else(spend_dir)))
            .with_http(Arc::new(self.fetch.clone()));
        let mut deps = WatcherDeps::new(ctx);
        deps.fetch = Arc::new(self.fetch.clone());
        deps.ask = self.ask.lent();
        deps.render = self.render.lent();
        deps.clock = clock.clock();
        deps.sleep = Arc::new(|_s: f64| -> BoxFuture<'static, ()> { Box::pin(async {}) });
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let rng: eve_web::watch::watcher::Rng = Box::new(move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        });
        deps.rng = Some(Arc::new(Mutex::new(rng)));
        deps.limiter = Some(self.shared_limiter.unwrap_or_else(|| Arc::new(Mutex::new(lim))));
        deps.offload = self.offload;
        deps.resolve = Arc::new(|host: &str| host.ends_with(".example"));
        deps.notify = self.notify;
        if let Some(e) = self.evaluate {
            deps.evaluate = e;
        }
        deps.tls = self.tls.map(|s| s.page());
        deps.crawl = self.crawl.map(|s| s.page());
        let mut cfg = config(path);
        (self.cfg)(&mut cfg);
        Watcher::new(cfg, deps)
    }
}

/// The plain rig over `path` and nothing else lent.
fn bare(path: &Path) -> Watcher {
    Make::new().build(path)
}

/// `async def notify(text) -> ok`: records each text; answers `up`.
fn recorder(sent: &Arc<Mutex<Vec<String>>>, up: &Arc<AtomicBool>) -> Notify {
    let sent = sent.clone();
    let up = up.clone();
    Arc::new(move |text: String, _about: String| -> BoxFuture<'static, anyhow::Result<bool>> {
        sent.lock().push(text);
        let ok = up.load(Ordering::SeqCst);
        Box::pin(async move { Ok(ok) })
    })
}

fn sink() -> (Arc<Mutex<Vec<String>>>, Notify) {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let n = recorder(&sent, &Arc::new(AtomicBool::new(true)));
    (sent, n)
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

fn wref(w: Watch) -> WatchRef {
    Arc::new(Mutex::new(w))
}

fn mk(id: &str, kind: &str, target: &str, label: &str, condition: &str) -> Watch {
    Watch::new(id, kind, target, label, condition)
}

fn quote_body(price: f64) -> String {
    json!({"chart": {"result": [{"meta": {"symbol": "AAPL", "currency": "USD", "regularMarketPrice": price,
                                           "shortName": "Apple Inc."}}]}})
    .to_string()
}

fn quote(price: f64, currency: &str) -> String {
    json!({"chart": {"result": [{"meta": {"symbol": "X", "currency": currency, "regularMarketPrice": price}}]}})
        .to_string()
}

fn model_reply(obj: Value, fence: bool) -> Value {
    let text = obj.to_string();
    let content = if fence { format!("```json\n{text}\n```") } else { text };
    json!({"choices": [{"message": {"content": content}}],
           "usage": {"prompt_tokens": 900, "completion_tokens": 30, "cost": 0.0002}})
}

fn reply(obj: Value) -> Value {
    json!({"choices": [{"message": {"content": obj.to_string()}}]})
}

fn args(over: Value) -> Map<String, Value> {
    let mut base = obj(json!({"kind": "page", "target": "https://shop.example/p", "label": "Thing",
                              "condition": "below", "value": 10, "text": null, "every_minutes": null}));
    for (k, v) in obj(over) {
        base.insert(k, v);
    }
    base
}

fn apple(condition: &str, value: Value, every: Value) -> Map<String, Value> {
    obj(json!({"kind": "quote", "target": "AAPL", "label": "Apple", "condition": condition, "value": value,
               "text": null, "city": null, "every_minutes": every, "for_hours": null}))
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().expect("a temp dir")
}

// ---- G3: the rate limiter ---------------------------------------------------------------------------------------

#[test]
fn test_the_bucket_runs_dry_and_refills() {
    let clock = TClock::new();
    let mut lim = limiter(&clock, |o| o.host_gap_secs = 0.0);
    assert_eq!(lim.ready_in("a"), 0.0);
    lim.take("a");
    assert_eq!(lim.ready_in("b"), 0.0);
    lim.take("b");
    // two tokens spent at 6/min: the next one is 10 s away, for any host
    assert!(approx(lim.ready_in("c"), 10.0));
    clock.add(5.0);
    assert!(approx(lim.ready_in("c"), 5.0));
    clock.add(5.0);
    assert_eq!(lim.ready_in("c"), 0.0);
    // the bucket never holds more than its burst, however long it was quiet
    clock.add(3600.0);
    for host in ["x", "y"] {
        assert_eq!(lim.ready_in(host), 0.0);
        lim.take(host);
    }
    assert!(lim.ready_in("z") > 0.0);
}

#[test]
fn test_one_host_waits_out_its_gap_another_goes_at_once() {
    let clock = TClock::new();
    let mut lim = limiter(&clock, |o| {
        o.burst = 10;
        o.rate_per_min = 600.0;
    });
    lim.take("shop.example");
    assert!(approx(lim.ready_in("shop.example"), 20.0));
    assert_eq!(lim.ready_in("other.example"), 0.0);
    clock.add(20.0);
    assert_eq!(lim.ready_in("shop.example"), 0.0);
}

#[test]
fn test_retry_after_holds_the_host_and_backoff_doubles_capped() {
    let clock = TClock::new();
    let mut lim = limiter(&clock, |o| {
        o.burst = 10;
        o.rate_per_min = 600.0;
        o.host_gap_secs = 0.0;
    });
    // Retry-After longer than the base backoff wins
    assert_eq!(lim.penalize("h", Some(120.0)), 120.0);
    assert!(approx(lim.ready_in("h"), 120.0));
    assert_eq!(lim.ready_in("elsewhere"), 0.0);
    // without a Retry-After, the backoff doubles per strike: 60, 120 (strike 2), 240, 480, then the 600 cap
    assert_eq!(lim.penalize("h", None), 120.0);
    assert_eq!(lim.penalize("h", None), 240.0);
    assert_eq!(lim.penalize("h", None), 480.0);
    assert_eq!(lim.penalize("h", None), 600.0);
    assert_eq!(lim.penalize("h", None), 600.0);
    // a Retry-After past the cap is capped too
    assert_eq!(lim.penalize("h", Some(99999.0)), 600.0);
    // a success clears it: the next strike starts from the base again
    lim.clear("h");
    assert_eq!(lim.ready_in("h"), 0.0);
    assert_eq!(lim.penalize("h", None), 60.0);
}

#[test]
fn test_the_daily_model_cap_refuses_and_resets() {
    let clock = TClock::at(86400.0 * 100.0 + 10.0);
    let mut lim = limiter(&clock, |_| {});
    assert!(lim.model_ok());
    lim.model_used();
    assert!(lim.model_ok());
    lim.model_used();
    assert!(!lim.model_ok() && lim.model_calls_today() == 2);
    clock.add(86400.0); // the next day
    assert!(lim.model_ok() && lim.model_calls_today() == 0);
}

// ---- G4: reading a page -----------------------------------------------------------------------------------------

const JSON_LD_EVENT: &str = r#"<html><head><title>Big Show - Tickets</title>
<script type="application/ld+json">{"@context":"https://schema.org","@type":"MusicEvent","name":"Big Show Seattle",
"offers":[{"@type":"Offer","price":"189.50","priceCurrency":"USD","availability":"https://schema.org/SoldOut"},
{"@type":"AggregateOffer","lowPrice":85,"highPrice":400,"priceCurrency":"USD","availability":"https://schema.org/InStock"}]}
</script></head><body><h1>Big Show</h1><script>var x = "Sold out";</script><p>Tickets on sale now</p></body></html>"#;

const META_PRODUCT: &str = r#"<html><head><meta property="product:price:amount" content="1,299.00">
<meta property="product:price:currency" content="usd"><meta property="product:availability" content="out of stock">
<meta property="og:availability" content="oos"></head><body>Laptop</body></html>"#;

const ITEMPROP: &str = r#"<html><body><span itemprop="price" content="42.10">$42.10</span></body></html>"#;

const PLAIN: &str = r#"<html><head><title>Shop</title><style>.p{}</style></head><body><div>Now only $19.99 &amp; free shipping</div>
<script>window.price = 5</script></body></html>"#;

#[test]
fn test_structured_prices_are_read_without_a_model() {
    let r = page::structured(JSON_LD_EVENT);
    assert_eq!((r.value, r.currency.as_str(), r.available), (Some(85.0), "USD", Some(true))); // the lowest offer
    assert_eq!(r.name, "Big Show Seattle");
    let r = page::structured(META_PRODUCT);
    assert_eq!((r.value, r.currency.as_str()), (Some(1299.0), "USD"));
    let r = page::structured(ITEMPROP);
    assert!(r.value == Some(42.10) && r.currency.is_empty());
    assert_eq!(page::structured(PLAIN).value, None); // nothing stated: no guess
    // the positive control: a JSON-LD page read through the watcher makes zero model calls
    let asks = Arc::new(AtomicUsize::new(0));
    let a = asks.clone();
    let w = Make::new()
        .fetch(|_, _| Ok((200, JSON_LD_EVENT.to_string())))
        .ask(move |_| {
            a.fetch_add(1, Ordering::SeqCst);
            Ok(json!({}))
        })
        .build(Path::new("/nonexistent/w.json"));
    let mut item = mk("w1", "page", "https://tickets.example/show", "Big Show", "below");
    item.value = Some(100.0);
    item.every_secs = 600;
    let r = run(w.read(&wref(item))).unwrap();
    assert!(r.value == Some(85.0) && r.source == "structured" && asks.load(Ordering::SeqCst) == 0);
}

#[test]
fn test_a_page_without_structure_asks_the_model_once() {
    let dir = tmp();
    let asks: Arc<Mutex<Vec<Value>>> = Arc::default();
    let a = asks.clone();
    let make = Make::new().fetch(|_, _| Ok((200, PLAIN.to_string()))).ask(move |body| {
        a.lock().push(body.clone());
        Ok(model_reply(json!({"price": 19.99, "currency": "usd", "available": true,
                               "note": "In stock, ships free.", "url": "javascript:alert(1)"}), true))
    });
    let fetch = make.fetch.clone();
    let w = make.build(&dir.path().join("w.json"));
    let mut item = mk("w1", "page", "https://shop.example/p", "Mug", "below");
    item.value = Some(15.0);
    item.every_secs = 600;
    let item = wref(item);
    let r = run(w.read(&item)).unwrap();
    assert!(asks.lock().len() == 1 && r.value == Some(19.99) && r.currency == "USD" && r.available == Some(true));
    assert_eq!(r.url, ""); // not http(s): dropped
    let user = asks.lock()[0]["messages"][1]["content"].as_str().unwrap().to_string();
    assert!(user.contains("Now only $19.99 & free shipping") && !user.contains("window.price") && !user.contains(".p{}"));
    assert_eq!(w.limiter.lock().model_calls_today(), 1);
    // the model's answer is checked, not passed on: a string price, a non-bool availability are dropped
    let bad = watch::model_reading(&model_reply(json!({"price": "cheap", "available": "yes", "currency": "dollars"}), true), "model")
        .unwrap();
    assert_eq!((bad.value, bad.available, bad.currency.as_str()), (None, None, ""));
    assert!(watch::model_reading(&json!({"choices": [{"message": {"content": "I think it is $5"}}]}), "model").is_err());
    // the same page text again: the last answer is reused, no second call
    let again = run(w.read(&item)).unwrap();
    assert!(asks.lock().len() == 1 && again.value == Some(19.99) && again.available == Some(true));
    // a changed page past the day's cap: the model is not called at all
    w.limiter.lock().model_cap = 1;
    fetch.set(|_, _| Ok((200, PLAIN.replace("19.99", "17.49"))));
    let e = run(w.read(&item)).unwrap_err();
    assert!(e.reason.contains("used up"), "{}", e.reason);
    assert_eq!(asks.lock().len(), 1);
}

#[test]
fn test_a_phrase_is_found_by_code() {
    let dir = tmp();
    let asks = Arc::new(AtomicUsize::new(0));
    let a = asks.clone();
    let w = Make::new()
        .fetch(|_, _| Ok((200, JSON_LD_EVENT.to_string())))
        .ask(move |_| {
            a.fetch_add(1, Ordering::SeqCst);
            Ok(json!({}))
        })
        .build(&dir.path().join("w.json"));
    let mut item = mk("w1", "page", "https://tickets.example/show", "Big Show", "appears");
    item.text = "tickets   ON SALE now".into();
    item.every_secs = 600;
    let item = wref(item);
    assert_eq!(run(w.read(&item)).unwrap().hit, Some(true));
    // a phrase only inside a script is not on the page
    item.lock().text = "sold out".into();
    assert_eq!(run(w.read(&item)).unwrap().hit, Some(false));
    assert_eq!(asks.load(Ordering::SeqCst), 0);
}

// ---- G5: conditions fire on the edge ----------------------------------------------------------------------------

fn run_readings(w: &mut Watch, readings: &[Reading]) -> Vec<Option<String>> {
    readings.iter().map(|r| rules::evaluate(w, r, true)).collect()
}

fn p(v: f64) -> Reading {
    Reading { value: Some(v), currency: "USD".into(), ..Reading::default() }
}

fn fired(alerts: &[Option<String>]) -> Vec<bool> {
    alerts.iter().map(Option::is_some).collect()
}

#[test]
fn test_conditions_fire_on_the_edge_and_rearm() {
    let mut below = mk("w1", "quote", "AAPL", "AAPL", "below");
    below.value = Some(300.0);
    let alerts = run_readings(&mut below, &[p(310.0), p(305.0), p(299.0), p(290.0), p(301.0), p(298.0)]);
    assert_eq!(fired(&alerts), [false, false, true, false, false, true]);
    assert_eq!(alerts[2].as_deref(), Some("AAPL dropped to $299 (was $305), past your $300 mark."));
    // already below at the first reading: nothing, until it goes back over and under again
    let mut already = mk("w2", "quote", "AAPL", "AAPL", "below");
    already.value = Some(300.0);
    assert_eq!(fired(&run_readings(&mut already, &[p(290.0), p(280.0), p(305.0), p(295.0)])), [false, false, false, true]);

    let mut above = mk("w3", "quote", "BTC-USD", "Bitcoin", "above");
    above.value = Some(100000.0);
    assert_eq!(fired(&run_readings(&mut above, &[p(90000.0), p(100500.0), p(101000.0)])), [false, true, false]);

    let mut drop = mk("w4", "quote", "X", "X", "drop_pct");
    drop.value = Some(10.0);
    let alerts = run_readings(&mut drop, &[p(100.0), p(95.0), p(89.0), p(85.0), p(80.0)]);
    assert_eq!(fired(&alerts), [false, false, true, false, true]);
    assert!(alerts[2].as_deref() == Some("X is down 11.0% to $89 (from $100).") && drop.baseline == Some(80.0));
    let mut rise = mk("w5", "quote", "X", "X", "rise_pct");
    rise.value = Some(5.0);
    assert_eq!(fired(&run_readings(&mut rise, &[p(100.0), p(104.0), p(106.0)])), [false, false, true]);

    let mut change = mk("w6", "page", "https://t.example", "Seats", "change");
    let alerts = run_readings(&mut change, &[p(80.0), p(80.0), p(75.0), p(75.0)]);
    assert_eq!(fired(&alerts), [false, false, true, false]);
    let a2 = alerts[2].clone().unwrap();
    assert!(a2.starts_with("Seats changed: $75, down from $80.") && a2.ends_with("\nhttps://t.example"));

    let a = |on: Option<bool>| Reading { available: on, ..Reading::default() };
    let mut avail = mk("w7", "search", "tickets", "Tour", "available");
    let got = run_readings(&mut avail, &[a(Some(false)), a(None), a(Some(true)), a(Some(true)), a(Some(false)), a(Some(true))]);
    assert_eq!(fired(&got), [false, false, true, false, false, true]);
    let mut on_at_start = mk("w8", "search", "tickets", "Tour", "available");
    assert!(run_readings(&mut on_at_start, &[a(Some(true))]) == [None] && !on_at_start.armed);

    let h = |on: bool| Reading { hit: Some(on), ..Reading::default() };
    let mut phrase = mk("w9", "page", "https://t.example", "Show", "appears");
    phrase.text = "on sale".into();
    assert_eq!(fired(&run_readings(&mut phrase, &[h(false), h(true), h(true), h(false), h(true)])), [false, true, false, false, true]);
    assert_eq!(phrase.fired, 2);
}

// ---- G6: the store and the guards -------------------------------------------------------------------------------

fn ids(w: &Watcher) -> Vec<String> {
    w.snapshot().into_iter().map(|x| x.id).collect()
}

#[test]
fn test_watches_survive_a_restart() {
    let dir = tmp();
    let path = dir.path().join("sub").join("watches.json");
    let w = Make::new().fetch(|_, _| Ok((200, quote_body(310.0)))).build(&path);
    let out = run(w.add(&obj(json!({"kind": "quote", "target": "aapl", "label": "Apple", "condition": "below",
                                     "value": 300, "text": null, "every_minutes": null}))));
    assert!(out["ok"] == json!(true) && out["id"] == "w1" && out["now"] == "$310" && out["every"] == "15 min", "{out}");
    assert!(out.get("already").is_none());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(".watches-"))
        .collect();
    assert!(leftovers.is_empty()); // no temp file left behind
    let again = bare(&path);
    let got: Vec<_> = again
        .snapshot()
        .into_iter()
        .map(|x| (x.id, x.target, x.condition, x.value, x.last_value, x.checks))
        .collect();
    assert_eq!(got, [("w1".to_string(), "aapl".to_string(), "below".to_string(), Some(300.0), Some(310.0), 1)]);
    // an unreadable file is not a crash, and a bad entry is skipped
    let good = again.snapshot()[0].to_json();
    std::fs::write(&path, json!({"watches": [{"id": "w9", "kind": "nope"}, good]}).to_string()).unwrap();
    assert_eq!(ids(&bare(&path)), ["w1"]);
    std::fs::write(&path, "{not json").unwrap();
    assert!(bare(&path).snapshot().is_empty());
    // remove by id
    let b = dir.path().join("b.json");
    let w = Make::new().fetch(|_, _| Ok((200, quote_body(310.0)))).build(&b);
    run(w.add(&obj(json!({"kind": "quote", "target": "AAPL", "label": "Apple", "condition": "change", "value": null,
                          "text": null, "every_minutes": 1}))));
    assert_eq!(w.remove("W1"), json!({"ok": true, "removed": "Apple"}));
    assert!(bare(&b).snapshot().is_empty());
    assert_eq!(w.remove("w1")["ok"], json!(false));
}

#[test]
fn test_guards_refuse_private_urls_and_raise_short_intervals() {
    let dir = tmp();
    let public = |host: &str| host.ends_with(".example");
    assert_eq!(net::check_url("https://shop.example/p", Some(&public)), "");
    for bad in ["file:///etc/passwd", "ftp://shop.example/x", "https://user:pw@shop.example/", "javascript:alert(1)"] {
        assert_ne!(net::check_url(bad, Some(&public)), "", "{bad}");
    }
    // the real resolver: loopback, private, link-local (the cloud metadata address) and unknown names are refused
    for bad in [
        "http://127.0.0.1:8000/",
        "http://localhost/",
        "http://10.0.0.5/",
        "http://192.168.1.1/",
        "http://169.254.169.254/latest/meta-data/",
        "http://[::1]/",
        "http://no-such-host.invalid/",
    ] {
        assert_ne!(net::check_url(bad, None), "", "{bad}");
    }
    // the fetch itself refuses too, whoever calls it
    let e = net::http_request("http://127.0.0.1:1/", &HttpOptions::default()).unwrap_err();
    assert!(e.reason.contains("private"), "{}", e.reason);

    let w = Make::new().fetch(|_, _| Ok((200, JSON_LD_EVENT.to_string()))).build(&dir.path().join("w.json"));
    let ok = |v: Value| run(w.add(&args(v)))["ok"] == json!(true);
    assert!(!ok(json!({"target": "http://192.168.1.1/admin"})));
    assert!(!ok(json!({"condition": "below", "value": null})));
    assert!(!ok(json!({"condition": "appears", "text": null})));
    assert!(!ok(json!({"kind": "quote", "target": "AAPL", "condition": "available"})));
    assert!(!ok(json!({"kind": "ticketmaster", "target": "x", "condition": "available"}))); // no key
    assert!(!ok(json!({"condition": "drop_pct", "value": 100})));
    // every_minutes below the kind's floor is raised to it, and above the ceiling lowered
    let out = run(w.add(&args(json!({"every_minutes": 0.5}))));
    assert!(out["ok"] == json!(true) && w.snapshot().last().unwrap().every_secs == watch::floor_secs("page"));
    run(w.add(&args(json!({"every_minutes": 1_000_000_000i64}))));
    assert_eq!(w.snapshot().last().unwrap().every_secs, watch::MAX_EVERY_SECS);
    // the cap
    while w.snapshot().len() < watch::MAX_WATCHES {
        let n = w.snapshot().len();
        w.push(mk(&format!("x{n}"), "quote", "A", "A", "change"));
    }
    let out = run(w.add(&args(json!({}))));
    assert!(out["ok"] == json!(false) && out["reason"].as_str().unwrap().contains("remove one"));
}

// ---- G7: the scheduler ------------------------------------------------------------------------------------------

#[test]
fn test_a_due_watch_fires_and_texts_once() {
    let dir = tmp();
    let prices = Arc::new(Mutex::new(VecDeque::from([310.0, 305.0, 299.0, 295.0, 294.0])));
    let (sent, notify) = sink();
    let clock = TClock::new();
    let pr = prices.clone();
    let path = dir.path().join("w.json");
    let w = Make::new()
        .fetch(move |_, _| Ok((200, quote_body(pr.lock().pop_front().expect("a price")))))
        .clock(&clock)
        .notify(notify)
        .build(&path);
    let out = run(w.add(&apple("below", json!(300), json!(5))));
    assert_eq!(out["now"], "$310");
    for _ in 0..4 {
        clock.add(400.0); // past each next check (5 min ± 10%)
        run(w.tick());
    }
    assert_eq!(*sent.lock(), ["Apple dropped to $299 (was $305), past your $300 mark."]);
    let first = w.snapshot()[0].clone();
    assert!(first.checks == 5 && first.fired == 1);
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved["watches"][0]["fired"], json!(1)); // saved after each check
    // not due: a tick checks nothing
    let before = w.snapshot()[0].checks;
    run(w.tick());
    assert_eq!(w.snapshot()[0].checks, before);
}

#[test]
fn test_a_blocked_page_falls_back_to_search() {
    let dir = tmp();
    let fetched: Arc<Mutex<Vec<String>>> = Arc::default();
    let asks: Arc<Mutex<Vec<Value>>> = Arc::default();
    let (f, a) = (fetched.clone(), asks.clone());
    let w = Make::new()
        .fetch(move |url, _| {
            f.lock().push(url.to_string());
            Err(FetchError::with_status("the site answered HTTP 403", 403))
        })
        .ask(move |body| {
            a.lock().push(body.clone());
            Ok(model_reply(json!({"available": false, "price": null, "currency": "", "url": "https://tix.example/e/1",
                                   "note": "Presale starts Oct 3 (tix.example)."}), false))
        })
        .build(&dir.path().join("w.json"));
    let out = run(w.add(&args(json!({"target": "https://tix.example/e/1", "label": "Big Show",
                                     "condition": "available", "value": null}))));
    assert!(out["ok"] == json!(true) && out["now"] == "not available", "{out}");
    assert!(out["note"].as_str().unwrap().contains("web search"));
    assert_eq!(out["detail"], "Presale starts Oct 3 (tix.example).");
    let item = w.snapshot()[0].clone();
    assert!(item.via == "search" && item.every_secs >= watch::floor_secs("search") && w.host(&item) == watch::OPENROUTER_HOST);
    assert!(fetched.lock().len() == 1 && asks.lock().len() == 1);
    let asks = asks.lock();
    assert_eq!(asks[0]["tools"][0]["type"], "openrouter:web_search");
    assert!(asks[0]["messages"][1]["content"].as_str().unwrap().contains("Big Show"));
}

#[test]
fn test_repeated_errors_back_off_and_tell_once() {
    let dir = tmp();
    let (sent, notify) = sink();
    let clock = TClock::new();
    #[derive(Clone)]
    struct State {
        fail: bool,
        retry_after: Option<f64>,
        status: u16,
    }
    let state = Arc::new(Mutex::new(State { fail: true, retry_after: None, status: 500 }));
    let s = state.clone();
    let w = Make::new()
        .fetch(move |_, _| {
            let st = s.lock().clone();
            if st.fail {
                return Err(FetchError::with_retry(format!("the site answered HTTP {}", st.status), st.status, st.retry_after));
            }
            Ok((200, quote_body(100.0)))
        })
        .clock(&clock)
        .notify(notify)
        .build(&dir.path().join("w.json"));
    let out = run(w.add(&apple("change", Value::Null, json!(5))));
    assert!(out["ok"] == json!(true) && out["now"].as_str().unwrap().starts_with("the first check failed")); // kept
    let item = w.watches()[0].clone();
    let mut gaps = Vec::new();
    for _ in 0..5 {
        clock.set(item.lock().next_at + 1.0);
        run(w.tick());
        gaps.push(item.lock().next_at - clock.t());
    }
    // the watch's own interval stretches with each failure, up to MAX_ERROR_STRETCH times
    let every = item.lock().every_secs as f64;
    let most = gaps.iter().copied().fold(f64::MIN, f64::max);
    assert!(gaps[0] < gaps[1] && gaps[1] < gaps[2] && most <= every * watch::MAX_ERROR_STRETCH as f64 * 1.1, "{gaps:?}");
    assert_eq!(item.lock().errors, 6);
    assert_eq!(*sent.lock(), ["I can't read Apple right now (the site answered HTTP 500). I'll keep trying, less often."]);
    // a 429 with Retry-After holds the host in the limiter (this rig's limiter caps a backoff at 600 s)
    {
        let mut st = state.lock();
        st.status = 429;
        st.retry_after = Some(300.0);
    }
    clock.set(item.lock().next_at + 1.0);
    run(w.tick());
    assert!(approx(w.limiter.lock().backed_off("finance.yahoo.com"), 300.0));
    // while it is held, a due watch on that host is pushed back, not fetched
    let snap = item.lock().clone();
    let checks = snap.checks + snap.errors;
    item.lock().next_at = clock.t();
    run(w.tick());
    let snap = item.lock().clone();
    assert!(snap.checks + snap.errors == checks && snap.next_at >= clock.t() + 299.0);
    // recovery is said once, and clears the host's backoff
    state.lock().fail = false;
    clock.add(1000.0);
    item.lock().next_at = clock.t();
    run(w.tick());
    assert!(sent.lock().last().unwrap() == "I can read Apple again." && item.lock().errors == 0);
    assert_eq!(w.limiter.lock().backed_off("finance.yahoo.com"), 0.0);
}

#[test]
fn test_the_listing_is_plain_and_the_switch_reads_the_environment() {
    let dir = tmp();
    let w = Make::new().fetch(|_, _| Ok((200, quote_body(310.0)))).build(&dir.path().join("w.json"));
    assert!(w.listing().contains("not watching anything"));
    run(w.add(&apple("below", json!(300), Value::Null)));
    assert_eq!(w.listing().lines().next().unwrap(), "w1 · Apple: at or below $300 · now $310 · every 15 min");
    let env = |pairs: &[(&str, &str)]| -> Ctx {
        Ctx::global().clone().with_env(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
    };
    assert!(watch::configured(&env(&[])).enabled);
    assert!(!watch::configured(&env(&[("EVE_WATCH", "0")])).enabled);
    assert!(watch::make_watcher(Some(watch::configured(&env(&[("EVE_WATCH", "off")])))).is_none());
    let cfg = watch::configured(&env(&[("EVE_WATCH_RATE", "abc"), ("EVE_WATCH_HOST_GAP", "5"), ("EVE_WATCH_MODEL", "openai/gpt-x")]));
    assert!(cfg.rate_per_min == watch::DEFAULT_RATE_PER_MIN && cfg.host_gap_secs == 5.0);
    assert_eq!(cfg.model, "openai/gpt-x"); // allowed since every model call is on OpenRouter (2026-10-08)
    assert_eq!(watch::configured(&env(&[])).model, watch::DEFAULT_MODEL);
}

#[test]
fn test_make_watcher_builds_the_daemons_watcher_when_on() {
    // make_watcher(config): with EVE_WATCH on, the daemon's watcher over the given list (nothing is fetched yet)
    let dir = tmp();
    let w = watch::make_watcher(Some(config(&dir.path().join("w.json")))).expect("on");
    assert!(w.snapshot().is_empty() && w.load_problem().is_empty());
}

// ---- the swarm's ports: lenient JSON-LD, microdata, prices, quotes, the limiter's clock -------------------------

#[test]
fn test_messy_structured_data_is_still_read() {
    // JSON-LD with a raw newline inside a string, CDATA framing, and two objects back to back
    let messy = "<script type=\"application/ld+json\">//<![CDATA[\n{\"@type\":\"Product\",\"name\":\"Desk\nLamp\",\
                 \"offers\":{\"price\":\"49,99\",\"priceCurrency\":\"EUR\",\"availability\":\"InStock\"}}\n\
                 {\"@type\":\"BreadcrumbList\"}\n//]]></script>";
    let r = page::structured(messy);
    assert_eq!((r.value, r.currency.as_str(), r.available), (Some(49.99), "EUR", Some(true)));
    // the item's name beats a founder's in an earlier block
    let two = r#"<script type="application/ld+json">{"@type":"Organization","founder":{"@type":"Person","name":"A Founder"}}</script><script type="application/ld+json">{"@type":"Product","name":"Falcon","offers":{"price":9}}</script>"#;
    assert_eq!(page::structured(two).name, "Falcon");
    // an entity-escaped block
    let escaped = r#"<script type="application/ld+json">{&quot;offers&quot;:{&quot;price&quot;:12}}</script>"#;
    assert_eq!(page::structured(escaped).value, Some(12.0));
    // microdata: price in the element's text, availability in a link's href, attributes in any order
    let micro = concat!(
        r#"<div itemscope itemtype="https://schema.org/Product"><h1>Mug</h1>"#,
        r#"<div itemprop="offers" itemscope itemtype="https://schema.org/Offer">"#,
        r#"<span itemprop="price">$1,299.00</span><meta content="USD" itemprop="priceCurrency">"#,
        "<link itemprop='availability' href='https://schema.org/OutOfStock'></div>",
        r#"<div itemprop="review" itemscope itemtype="https://schema.org/Review">"#,
        r#"<span itemprop="price">5</span></div></div>"#
    );
    let r = page::structured(micro);
    assert_eq!((r.value, r.currency.as_str(), r.available), (Some(1299.0), "USD", Some(false)));
    // a price inside a review's scope is not the item's
    let only_review = r#"<div itemscope itemtype="https://schema.org/Review"><span itemprop="price">5</span></div>"#;
    assert_eq!(page::structured(only_review).value, None);
    // European and grouped prices
    for (raw, want) in [("1.299,00 €", 1299.0), ("12,5", 12.5), ("1,299", 1299.0), ("CHF 1'250.50", 1250.5)] {
        assert_eq!(page::price(&json!(raw), ""), Some(want), "{raw}");
    }
}

#[test]
fn test_quotes_read_minor_units_and_unknown_symbols() {
    let dir = tmp();
    let now = 1_800_000_000.0;
    let mut meta = obj(json!({"symbol": "VOD.L", "currency": "GBp", "regularMarketPrice": 7250,
                              "currentTradingPeriod": {"regular": {"start": now - 50000.0, "end": now - 20000.0}},
                              "fulldayPrice": 7300}));
    let r = rules::quote_reading(&meta, 7250.0, now);
    assert!(r.value == Some(72.5) && r.currency == "GBP" && r.note == "after hours £73", "{r:?}");
    // inside the session, no extended-hours note
    meta["currentTradingPeriod"]["regular"] = json!({"start": now - 100.0, "end": now + 100.0});
    assert_eq!(rules::quote_reading(&meta, 7250.0, now).note, "");
    let sent: Arc<Mutex<Vec<HttpOptions>>> = Arc::default();
    let s = sent.clone();
    let w = Make::new()
        .fetch(move |_, opts| {
            s.lock().push(opts.clone());
            Ok((200, quote_body(1.0)))
        })
        .build(&dir.path().join("h.json"));
    run(w.read(&wref(mk("q", "quote", "AAPL", "A", "change")))).unwrap();
    assert_eq!(sent.lock()[0].header("User-Agent"), Some("Mozilla/5.0")); // a full Chrome UA gets Yahoo's 429
    let missing = json!({"chart": {"result": null, "error": {"code": "Not Found", "description": "delisted"}}}).to_string();
    let w = Make::new().fetch(move |_, _| Ok((200, missing.clone()))).build(&dir.path().join("w.json"));
    let out = run(w.add(&obj(json!({"kind": "quote", "target": "NOPE", "label": "x", "condition": "change",
                                     "value": null, "text": null, "city": null, "every_minutes": null}))));
    assert_eq!(out, json!({"ok": false, "reason": "no such symbol on Yahoo Finance: NOPE"}));
    assert!(w.snapshot().is_empty());
}

#[test]
fn test_a_clock_that_steps_back_mints_no_tokens() {
    let clock = TClock::new();
    let mut lim = limiter(&clock, |o| {
        o.burst = 1;
        o.rate_per_min = 6.0;
        o.host_gap_secs = 0.0;
    });
    lim.take("a");
    clock.add(-30.0); // the clock steps back: waiting longer is the safe answer
    assert!(lim.ready_in("a") >= 10.0);
    clock.add(30.0); // and forward again to where it was: still nothing refilled
    assert!(approx(lim.ready_in("a"), 10.0));
    // a Rate-Limit-Reset in epoch ms is honoured when there is no Retry-After (Ticketmaster's quota header)
    let now = eve_web::time_time();
    let reset = (((now + 60.0) * 1000.0) as i64).to_string();
    let got = net::retry_after(&|name: &str| (name == "Rate-Limit-Reset").then(|| reset.clone()), now).unwrap();
    assert!((58.0..=60.0).contains(&got), "{got}");
}

// ---- vision: a page that only shows its price in a browser ------------------------------------------------------

const SPA: &str = "<html><head><title>Loading</title></head><body><div id=root></div><script src=app.js></script></body></html>";

#[test]
fn test_a_js_page_is_rendered_then_seen() {
    let dir = tmp();
    let asks: Arc<Mutex<Vec<Value>>> = Arc::default();
    let renders: Arc<Mutex<Vec<String>>> = Arc::default();
    let a = asks.clone();
    let ask = move |body: &Value| {
        a.lock().push(body.clone());
        if body["messages"][1]["content"].is_array() {
            // the screenshot
            return Ok(model_reply(json!({"price": 64.0, "currency": "USD", "available": true, "note": "Get tickets button live."}), true));
        }
        Ok(model_reply(json!({"price": null, "currency": "", "available": null, "note": "The page is empty."}), true))
    };
    let plain_fetches = Arc::new(AtomicUsize::new(0));
    let pf = plain_fetches.clone();
    let make = Make::new()
        .fetch(move |_, _| {
            pf.fetch_add(1, Ordering::SeqCst);
            Ok((200, SPA.to_string()))
        })
        .ask(ask.clone())
        .cfg(|c| c.browser = true);
    let r = renders.clone();
    make.render.set(move |url| {
        r.lock().push(url.to_string());
        Ok((SPA.to_string(), b"\xff\xd8jpeg".to_vec()))
    });
    let render = make.render.clone();
    let w = make.build(&dir.path().join("w.json"));
    let out = run(w.add(&args(json!({"target": "https://seats.example/show", "label": "Show", "condition": "below", "value": 80}))));
    assert!(out["ok"] == json!(true) && out["now"] == "$64, available", "{out}");
    assert!(out["note"].as_str().unwrap().contains("renders it and looks at it"));
    let item = w.watches()[0].clone();
    let snap = item.lock().clone();
    assert!(snap.via == "browser" && snap.every_secs >= watch::floor_secs("browser"));
    // the text model once (nothing there), then the vision model with the screenshot as an image
    assert_eq!(asks.lock().len(), 2);
    let image = asks.lock()[1]["messages"][1]["content"][1].clone();
    assert!(image["type"] == "image_url" && image["image_url"]["url"].as_str().unwrap().starts_with("data:image/jpeg;base64,"));
    // from now on it goes straight to the browser: no plain fetch, no text model
    let fetched = plain_fetches.load(Ordering::SeqCst);
    run(w.read(&item)).unwrap();
    assert_eq!(plain_fetches.load(Ordering::SeqCst), fetched, "a browser page is not fetched plainly");
    assert!(renders.lock().len() == 2 && asks.lock().len() == 3);
    // a rendered page whose DOM carries JSON-LD needs no model at all
    render.set(|_| Ok((JSON_LD_EVENT.to_string(), Vec::new())));
    assert!(run(w.read(&item)).unwrap().value == Some(85.0) && asks.lock().len() == 3);
    // without the browser the page stays on its text reading
    let rendered = Arc::new(AtomicUsize::new(0));
    let rd = rendered.clone();
    let make = Make::new().fetch(|_, _| Ok((200, SPA.to_string()))).ask(ask).cfg(|c| c.browser = false);
    make.render.set(move |_| {
        rd.fetch_add(1, Ordering::SeqCst);
        Err(FetchError::new("no browser configured"))
    });
    let plain = make.build(&dir.path().join("p.json"));
    let out = run(plain.add(&args(json!({"target": "https://seats.example/show", "condition": "below", "value": 80}))));
    assert!(out["ok"] == json!(true) && plain.snapshot()[0].via.is_empty());
    assert_eq!(rendered.load(Ordering::SeqCst), 0, "no browser configured");
}

#[test]
fn test_a_refused_page_tries_the_browser_then_search() {
    let dir = tmp();
    let asks: Arc<Mutex<Vec<Value>>> = Arc::default();
    let a = asks.clone();
    let make = Make::new()
        .fetch(|_, _| Err(FetchError::with_status("the site answered HTTP 403", 403)))
        .ask(move |body| {
            a.lock().push(body.clone());
            Ok(model_reply(json!({"available": false, "note": "Not yet."}), true))
        })
        .cfg(|c| c.browser = true);
    make.render.set(|_| Err(FetchError::with_status("the site answered HTTP 403", 403)));
    let w = make.build(&dir.path().join("w.json"));
    let out = run(w.add(&args(json!({"target": "https://tix.example/e/1", "label": "Show", "condition": "available", "value": null}))));
    assert!(out["ok"] == json!(true) && w.snapshot()[0].via == "search" && out["now"] == "not available", "{out}");
    assert_eq!(asks.lock()[0]["tools"][0]["type"], "openrouter:web_search");
}

// ---- Ticketmaster -----------------------------------------------------------------------------------------------

fn iso(t: f64) -> String {
    chrono::DateTime::from_timestamp(t as i64, 0).unwrap().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn tm_event(code: &str, public: Option<(f64, Option<f64>)>, presale: Option<(f64, f64)>, price: Option<f64>, eid: &str, url: &str) -> Value {
    let mut sales = Map::new();
    if let Some((start, end)) = public {
        let mut p = obj(json!({"startDateTime": iso(start)}));
        if let Some(end) = end {
            p.insert("endDateTime".into(), json!(iso(end)));
        }
        sales.insert("public".into(), Value::Object(p));
    }
    if let Some((start, end)) = presale {
        sales.insert("presales".into(), json!([{"name": "Fan presale", "startDateTime": iso(start), "endDateTime": iso(end)}]));
    }
    let mut e = json!({"id": eid, "name": "Big Show", "url": url,
                       "dates": {"status": {"code": code}, "start": {"localDate": "2026-12-01"}},
                       "sales": sales, "_embedded": {"attractions": [{"id": "K8vZ"}]}});
    if let Some(price) = price {
        e["priceRanges"] = json!([{"type": "standard", "currency": "USD", "min": price, "max": price * 3.0}]);
    }
    e
}

const TM_URL: &str = "https://www.ticketmaster.com/show/event/0E0050681F51BA4C";

fn ev(code: &str) -> Map<String, Value> {
    obj(tm_event(code, None, None, None, "vvG1", TM_URL))
}

#[test]
fn test_ticketmaster_statuses_follow_its_own_definitions() {
    let now = 1_800_000_000.0;
    let day = 86400.0;
    let one = |e: Value| obj(e);
    // offsale before the public sale: watched, not available, and the note says when it opens
    let r = watch::ticketmaster_reading(&[one(tm_event("offsale", Some((now + 3.0 * day, None)), None, None, "vvG1", TM_URL))], now);
    assert!(r.available == Some(false) && r.note.contains("1 date listed, 0 on sale now.") && r.note.contains("the public sale opens"), "{}", r.note);
    // a presale window open now counts, even while the status is offsale
    let r = watch::ticketmaster_reading(
        &[one(tm_event("offsale", Some((now + day, None)), Some((now - 60.0, now + 3600.0)), None, "vvG1", TM_URL))],
        now,
    );
    assert_eq!(r.available, Some(true));
    // onsale and rescheduled are on sale; postponed and canceled are not; an ended offsale is dropped
    assert_eq!(watch::ticketmaster_reading(&[ev("onsale")], now).available, Some(true));
    assert_eq!(watch::ticketmaster_reading(&[ev("rescheduled")], now).available, Some(true));
    assert_eq!(watch::ticketmaster_reading(&[ev("postponed"), ev("canceled")], now).available, Some(false));
    let r = watch::ticketmaster_reading(&[one(tm_event("offsale", Some((now - 9.0 * day, Some(now - day))), None, None, "vvG1", TM_URL))], now);
    assert!(r.available == Some(false) && r.note.starts_with("0 dates listed"));
    // no priceRanges: the price is unknown, never 0; with one, the lowest min
    assert_eq!(watch::ticketmaster_reading(&[ev("onsale")], now).value, None);
    let two = [
        one(tm_event("onsale", None, None, Some(89.5), "vvG1", TM_URL)),
        one(tm_event("onsale", None, None, Some(45.0), "vvG1", TM_URL)),
    ];
    assert_eq!(watch::ticketmaster_reading(&two, now).value, Some(45.0));
    assert_eq!(watch::ticketmaster_reading(&[], now).available, Some(false));
}

#[test]
fn test_ticketmaster_resolves_the_artist_and_matches_links() {
    let dir = tmp();
    let attractions = json!({"_embedded": {"attractions": [
        {"id": "T1", "name": "Big Band Tribute", "aliases": ["big band"], "upcomingEvents": {"_total": 90}},
        {"id": "K8vZ", "name": "Big Band", "upcomingEvents": {"_total": 12}},
        {"id": "K9", "name": "Big Band Theory", "upcomingEvents": {"_total": 40}}]}});
    assert_eq!(watch::pick_attraction("big band!", &obj(attractions.clone())), "K8vZ");
    assert_eq!(watch::pick_attraction("nobody", &obj(attractions.clone())), "");
    let calls: Arc<Mutex<Vec<String>>> = Arc::default();
    let now = 1_800_000_000.0;
    let c = calls.clone();
    let w = Make::new()
        .fetch(move |url, _| {
            c.lock().push(url.to_string());
            if url.contains("/attractions.json") {
                return Ok((200, attractions.to_string()));
            }
            let mut other = tm_event("onsale", None, None, None, "x", "https://www.ticketmaster.com/x/event/1111111111111111");
            other["_embedded"] = json!({"attractions": [{"id": "OTHER"}]});
            let first = tm_event("offsale", Some((now + 86400.0, None)), None, None, "vvG1", TM_URL);
            Ok((200, json!({"_embedded": {"events": [first, other]}}).to_string()))
        })
        .clock(&TClock::at(now))
        .env("TICKETMASTER_API_KEY", "tm-test")
        .cfg(|c| c.ticketmaster = true)
        .build(&dir.path().join("w.json"));
    let kinds = w.tools()[0]["parameters"]["properties"]["kind"]["enum"].clone();
    assert!(kinds.as_array().unwrap().contains(&json!("ticketmaster")));
    assert!(w.instructions().contains("kind ticketmaster"));
    let out = run(w.add(&obj(json!({"kind": "ticketmaster", "target": "Big Band", "label": "Big Band", "condition": "available",
                                     "value": null, "text": null, "city": "Seattle", "every_minutes": null}))));
    assert!(out["ok"] == json!(true) && out["now"] == "not available" && w.snapshot()[0].ref_ == "K8vZ", "{out}");
    {
        let calls = calls.lock();
        assert!(calls[1].contains("attractionId=K8vZ") && calls[1].contains("city=Seattle") && calls[1].contains("apikey=tm-test"), "{}", calls[1]);
    }
    // the other attraction's event in the same page of results is not counted
    assert!(out["detail"].as_str().unwrap().contains("1 date listed"));
    // a price condition is refused, with the reason
    let refused = run(w.add(&obj(json!({"kind": "ticketmaster", "target": "Big Band", "label": "x", "condition": "below",
                                         "value": 50, "text": null, "city": null, "every_minutes": null}))));
    assert!(refused["ok"] == json!(false) && refused["reason"].as_str().unwrap().contains("no longer publishes ticket prices"));
    // a ticketmaster.com link's hex id is matched against each event's url, never fetched as an API id
    let item = wref(mk(
        "w9",
        "ticketmaster",
        "https://www.ticketmaster.com/big-band-seattle-12-01-2026/event/0E0050681F51BA4C",
        "Show",
        "available",
    ));
    calls.lock().clear();
    let r = run(w.read(&item)).unwrap();
    let first = calls.lock()[0].clone();
    assert!(r.available == Some(false) && !first.contains("/events/0E0050681F51BA4C") && first.contains("keyword=big+band+seattle"), "{first}");
    // without the key the kind is not offered
    let plain = bare(&dir.path().join("p.json"));
    let kinds = plain.tools()[0]["parameters"]["properties"]["kind"]["enum"].clone();
    assert!(!kinds.as_array().unwrap().contains(&json!("ticketmaster")));
}

// ---- after the Lean pass: delivery, attribution, currency, the store (2026-09-25) --------------------------------

#[test]
fn test_an_alert_telegram_refused_is_sent_again() {
    let dir = tmp();
    let path = dir.path().join("w.json");
    let prices = Arc::new(Mutex::new(VecDeque::from([310.0, 290.0, 291.0])));
    let sends: Arc<Mutex<Vec<String>>> = Arc::default();
    let up = Arc::new(AtomicBool::new(false));
    let clock = TClock::new();
    let pr = prices.clone();
    let w = Make::new()
        .fetch(move |_, _| Ok((200, quote_body(pr.lock().pop_front().expect("a price")))))
        .clock(&clock)
        .notify(recorder(&sends, &up))
        .build(&path);
    run(w.add(&apple("below", json!(300), json!(5))));
    clock.add(400.0);
    run(w.tick()); // fires, but Telegram refuses it
    let item = w.watches()[0].clone();
    assert!(sends.lock().len() == 1 && item.lock().pending.starts_with("Apple dropped to $290"));
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved["watches"][0]["pending"], json!(item.lock().pending)); // survives a restart
    up.store(true, Ordering::SeqCst);
    run(w.tick()); // not due: only the pending alert goes
    let sends = sends.lock();
    assert!(sends[1] == sends[0] && item.lock().pending.is_empty() && sends.len() == 2);
}

#[test]
fn test_a_model_services_429_holds_the_model_service_not_the_page() {
    let clock = TClock::new();
    let w = Make::new()
        .clock(&clock)
        .fetch(|_, _| Ok((200, "<p>no price here</p>".to_string())))
        .ask(|_| Err(FetchError::with_retry("the site answered HTTP 429", 429, Some(120.0)).at(watch::OPENROUTER_HOST)))
        .build(Path::new("/nonexistent/w.json"));
    let mut item = mk("w1", "page", "https://shop.example/p", "x", "below");
    item.value = Some(5.0);
    item.every_secs = 600;
    assert!(run(w.check(&wref(item), false)).is_err());
    let (model_hold, page_hold) = {
        let lim = w.limiter.lock();
        (lim.backed_off(watch::OPENROUTER_HOST), lim.backed_off("shop.example"))
    };
    assert!(model_hold >= 119.0 && page_hold == 0.0);
    // the backoff ladder never overflows, however many strikes
    let mut lim = limiter(&clock, |_| {});
    let mut secs = 0.0;
    for _ in 0..1100 {
        secs = lim.penalize("h", None);
    }
    assert_eq!(secs, 600.0);
}

#[test]
fn test_a_reading_in_another_currency_is_no_price() {
    let mut w = mk("w1", "page", "https://shop.example/p", "Lamp", "below");
    w.value = Some(50.0);
    let r = |v: f64, c: &str| Reading { value: Some(v), currency: c.into(), ..Reading::default() };
    assert_eq!(rules::evaluate(&mut w, &r(60.0, "USD"), true), None);
    assert_eq!(rules::evaluate(&mut w, &r(45.0, "EUR"), true), None); // 45 EUR is not under $50
    assert!(w.last_value == Some(60.0) && w.currency == "USD");
    assert!(rules::evaluate(&mut w, &r(45.0, "USD"), true).is_some());
    // a change that is both a price move and an availability flip says both
    let mut c = mk("w2", "page", "https://shop.example/p", "Lamp", "change");
    rules::evaluate(&mut c, &Reading { available: Some(false), ..r(80.0, "USD") }, true);
    let alert = rules::evaluate(&mut c, &Reading { available: Some(true), ..r(70.0, "USD") }, true).unwrap();
    assert!(alert.contains("down from $80") && alert.contains("available now"), "{alert}");
}

#[test]
fn test_a_hand_edited_store_never_stops_the_loop() {
    let dir = tmp();
    let path = dir.path().join("w.json");
    let mut good = mk("w1", "quote", "AAPL", "A", "change");
    good.every_secs = 900;
    let good = obj(good.to_json());
    let with = |pairs: Value| {
        let mut m = good.clone();
        for (k, v) in obj(pairs) {
            m.insert(k, v);
        }
        Value::Object(m)
    };
    let doc = json!({"watches": [
        with(json!({"id": "W1"})),                                                        // upper case: read as w1
        with(json!({"label": "dup"})),                                                    // a second w1: dropped
        with(json!({"id": "w2", "next_at": "soon", "every_secs": 1.0, "armed": "yes"})), // wrong types: defaults
        {"id": "w3", "kind": "quote", "target": ["AAPL"], "condition": "change"},        // target not a string
    ]});
    std::fs::write(&path, doc.to_string()).unwrap();
    let w = bare(&path);
    assert_eq!(ids(&w), ["w1", "w2"]);
    let w2 = w.snapshot()[1].clone();
    assert!(w2.next_at == 0.0 && w2.armed && w2.every_secs == watch::floor_secs("quote"));
    assert!(w.remove("W1")["ok"] == json!(true) && ids(&w) == ["w2"]);
    run(w.tick()); // sorting by next_at no longer raises
}

#[test]
fn test_ticketmaster_far_future_dates_and_a_retried_lookup() {
    let dir = tmp();
    let now = 1_800_000_000.0;
    let mut far = tm_event("offsale", Some((now + 86400.0, None)), None, None, "vvG1", TM_URL);
    far["sales"]["public"]["startDateTime"] = json!("9999-12-31T23:59:59Z");
    let r = watch::ticketmaster_reading(&[obj(far)], now);
    assert!(r.available == Some(false) && r.note.contains("opens later"), "{}", r.note);
    let calls: Arc<Mutex<Vec<String>>> = Arc::default();
    let c = calls.clone();
    let clock = TClock::at(now);
    let w = Make::new()
        .fetch(move |url, _| {
            c.lock().push(url.to_string());
            let body = if url.contains("events.json") { json!({"_embedded": {"events": []}}) } else { json!({}) };
            Ok((200, body.to_string()))
        })
        .clock(&clock)
        .env("TICKETMASTER_API_KEY", "tm-test")
        .cfg(|c| c.ticketmaster = true)
        .build(&dir.path().join("w.json"));
    let item = wref(mk("w1", "ticketmaster", "New Act", "New Act", "available"));
    let lookups = || calls.lock().iter().filter(|c| c.contains("attractions.json")).count();
    run(w.read(&item)).unwrap();
    assert!(item.lock().ref_ == "-" && lookups() == 1);
    run(w.read(&item)).unwrap(); // the same day: not looked up again
    assert_eq!(lookups(), 1);
    clock.add(86401.0);
    run(w.read(&item)).unwrap(); // a day later: looked up again
    assert_eq!(lookups(), 2);
}

// ---- pause, resume, and a watch for a period (owner, 2026-09-25) -------------------------------------------------

#[test]
fn test_a_paused_watch_is_not_checked_and_resumes_by_itself_or_on_ask() {
    let dir = tmp();
    let path = dir.path().join("w.json");
    let fetched = Arc::new(AtomicUsize::new(0));
    let clock = TClock::new();
    let f = fetched.clone();
    let w = Make::new()
        .fetch(move |_, _| {
            f.fetch_add(1, Ordering::SeqCst);
            Ok((200, quote_body(310.0)))
        })
        .clock(&clock)
        .build(&path);
    run(w.add(&apple("below", json!(300), json!(5))));
    let item = w.watches()[0].clone();
    let out = run(w.handle("watch_pause", &obj(json!({"id": "W1", "for_hours": null}))));
    assert!(out == json!({"ok": true, "paused": "Apple", "until": "until you resume it"}) && item.lock().paused);
    let n = fetched.load(Ordering::SeqCst);
    for _ in 0..3 {
        clock.add(3600.0);
        run(w.tick());
    }
    assert!(fetched.load(Ordering::SeqCst) == n && item.lock().checks == 1); // paused: no checks, its state kept
    assert!(w.listing().lines().next().unwrap().contains("paused"));
    let out = run(w.handle("watch_resume", &obj(json!({"id": "w1"}))));
    assert!(out["ok"] == json!(true) && !item.lock().paused);
    run(w.tick());
    assert_eq!(fetched.load(Ordering::SeqCst), n + 1); // checked again right away
    // a pause for a while ends by itself
    run(w.handle("watch_pause", &obj(json!({"id": "w1", "for_hours": 2}))));
    assert!(w.listing().lines().next().unwrap().contains("paused until"));
    clock.add(3600.0);
    run(w.tick());
    assert!(item.lock().paused && fetched.load(Ordering::SeqCst) == n + 1);
    clock.add(3601.0);
    run(w.tick());
    assert!(!item.lock().paused && fetched.load(Ordering::SeqCst) == n + 2);
    // it survives a restart paused
    run(w.handle("watch_pause", &obj(json!({"id": "w1", "for_hours": null}))));
    assert!(bare(&path).snapshot()[0].paused);
    // an unknown id and a bad period are said, not raised
    assert_eq!(run(w.handle("watch_pause", &obj(json!({"id": "w9", "for_hours": null}))))["ok"], json!(false));
    assert_eq!(run(w.handle("watch_pause", &obj(json!({"id": "w1", "for_hours": -3}))))["ok"], json!(false));
}

#[test]
fn test_a_watch_for_a_period_ends_by_itself_and_says_so() {
    let dir = tmp();
    let path = dir.path().join("w.json");
    let (told, notify) = sink();
    let clock = TClock::new();
    let w = Make::new().fetch(|_, _| Ok((200, quote_body(310.0)))).clock(&clock).notify(notify).build(&path);
    let mut a = apple("below", json!(300), json!(5));
    a.insert("for_hours".into(), json!(24));
    let out = run(w.add(&a));
    assert!(out["ok"] == json!(true) && w.listing().lines().next().unwrap().contains("until"));
    assert!(run(w.tick()) <= 24.0 * 3600.0); // the loop wakes for the end, at the latest
    clock.add(23.0 * 3600.0);
    run(w.tick());
    assert_eq!(w.snapshot().len(), 1);
    clock.add(3601.0);
    run(w.tick());
    assert!(w.snapshot().is_empty());
    assert_eq!(told.lock().last().unwrap(), "I've stopped watching Apple: the time you gave it is up.");
    assert!(bare(&path).snapshot().is_empty());
    // an end set later, and cleared
    run(w.add(&apple("change", Value::Null, Value::Null)));
    let wid = w.snapshot()[0].id.clone();
    assert_ne!(run(w.handle("watch_set_end", &obj(json!({"id": wid, "for_hours": 48}))))["ends"], "no end");
    assert_eq!(run(w.handle("watch_set_end", &obj(json!({"id": wid, "for_hours": null}))))["ends"], "no end");
    assert_eq!(w.snapshot()[0].ends_at, 0.0);
    let mut a = apple("change", Value::Null, Value::Null);
    a.insert("for_hours".into(), json!(1_000_000_000i64));
    let refused = run(w.add(&a));
    assert!(refused["ok"] == json!(false) && refused["reason"].as_str().unwrap().contains("year"));
}

#[test]
fn test_the_pause_tools_are_offered_to_the_brain() {
    let dir = tmp();
    let w = bare(&dir.path().join("w.json"));
    let tools = w.tools();
    let names: Vec<&str> = tools.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["watch_add", "watch_list", "watch_remove", "watch_pause", "watch_resume", "watch_set_end"]);
    assert!(tools[0]["parameters"]["required"].as_array().unwrap().contains(&json!("for_hours")));
    assert!(w.instructions().contains("watch_pause") && w.instructions().contains("watch_set_end"));
}

#[test]
fn test_watch_list_and_an_unknown_tool() {
    // handle(): watch_list summarises each watch; an unknown tool name is said, not raised
    let dir = tmp();
    let w = Make::new().fetch(|_, _| Ok((200, quote_body(310.0)))).build(&dir.path().join("w.json"));
    run(w.add(&apple("below", json!(300), Value::Null)));
    let out = run(w.handle("watch_list", &Map::new()));
    assert_eq!(
        out,
        json!({"ok": true, "watches": [{"id": "w1", "label": "Apple", "kind": "quote", "watching_for": "at or below $300",
                                        "now": "$310", "every": "15 min", "alerts_sent": 0}]})
    );
    assert_eq!(run(w.handle("watch_nope", &Map::new())), json!({"ok": false, "reason": "unknown watch tool watch_nope"}));
    assert_eq!(w.resume("w1"), json!({"ok": true, "resumed": "Apple", "note": "it was not paused"}));
    assert_eq!(w.remove(" W7 "), json!({"ok": false, "reason": "no watch with id 'w7'; watch_list shows the ids"}));
}

// ---- test_watch_lean_scheduler.py: the interleavings WatchScheduler.lean names ----------------------------------

enum Step {
    Price(f64),
    Fail(FetchError),
}

/// A fetch that answers from `script`. When `hold` is set, the next call signals `entered` and blocks until
/// `release`: the check is then mid-flight.
#[derive(Clone)]
struct Gated {
    script: Arc<Mutex<VecDeque<Step>>>,
    calls: Arc<AtomicUsize>,
    hold: Arc<AtomicBool>,
    entered: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
}

impl Gated {
    fn new(script: Vec<Step>) -> Self {
        Gated {
            script: Arc::new(Mutex::new(script.into())),
            calls: Arc::default(),
            hold: Arc::default(),
            entered: Arc::default(),
            release: Arc::default(),
        }
    }
    fn answer(&self) -> Result<(u16, String), FetchError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.hold.swap(false, Ordering::SeqCst) {
            self.entered.store(true, Ordering::SeqCst);
            let until = Instant::now() + Duration::from_secs(5);
            while !self.release.load(Ordering::SeqCst) {
                assert!(Instant::now() < until, "the test never released the fetch");
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        match self.script.lock().pop_front().expect("a scripted answer") {
            Step::Price(p) => Ok((200, quote_body(p))),
            Step::Fail(e) => Err(e),
        }
    }
}

/// test_watch_lean_scheduler.py's `make`: the fetch runs off the loop (offload=True), as in the daemon.
fn make_gated(path: &Path, fetch: &Gated, sent: &Arc<Mutex<Vec<String>>>, clock: &TClock, ask: Option<SwapAsk>) -> Watcher {
    let g = fetch.clone();
    let mut make = Make::new()
        .fetch(move |_, _| g.answer())
        .clock(clock)
        .notify(recorder(sent, &Arc::new(AtomicBool::new(true))));
    if let Some(ask) = ask {
        make.ask = ask;
    }
    make.offload = true;
    make.limiter = Some(limiter(clock, |o| {
        o.rate_per_min = 600.0;
        o.burst = 50;
        o.host_gap_secs = 0.0;
        o.model_calls_per_day = 50;
    }));
    make.build(path)
}

fn add_apple(w: &Watcher, condition: &str) -> Value {
    let value = if condition == "below" { json!(300) } else { Value::Null };
    run(w.add(&obj(json!({"kind": "quote", "target": "AAPL", "label": "Apple", "condition": condition,
                          "value": value, "text": null, "every_minutes": 5}))))
}

/// tick picks the watch and its fetch starts; the owner's watch_remove runs; then the fetch answers.
async fn remove_mid_check(w: &Watcher, fetch: &Gated, wid: &str) -> Value {
    fetch.hold.store(true, Ordering::SeqCst);
    let owner = async {
        while !fetch.entered.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let out = w.handle("watch_remove", &obj(json!({"id": wid}))).await; // the brain's tool call, on the loop
        fetch.release.store(true, Ordering::SeqCst);
        out
    };
    let (_, out) = tokio::join!(w.tick(), owner);
    out
}

#[test]
fn test_an_alert_in_flight_is_not_texted_after_the_watch_is_removed() {
    let dir = tmp();
    let sent: Arc<Mutex<Vec<String>>> = Arc::default();
    let clock = TClock::new();
    let fetch = Gated::new(vec![Step::Price(310.0), Step::Price(299.0)]);
    let w = make_gated(&dir.path().join("w.json"), &fetch, &sent, &clock, None);
    assert_eq!(add_apple(&w, "below")["id"], "w1");
    clock.add(400.0); // past the next check (5 min ± 10%)
    let out = run(remove_mid_check(&w, &fetch, "w1"));
    assert_eq!(out, json!({"ok": true, "removed": "Apple"}));
    assert!(w.snapshot().is_empty() && fetch.calls.load(Ordering::SeqCst) == 2);
    assert!(sent.lock().is_empty(), "a removed watch texted the owner: {:?}", sent.lock());
}

#[test]
fn test_an_error_notice_in_flight_is_not_texted_after_the_watch_is_removed() {
    let dir = tmp();
    let sent: Arc<Mutex<Vec<String>>> = Arc::default();
    let clock = TClock::new();
    let fetch = Gated::new(vec![Step::Price(310.0), Step::Fail(FetchError::with_status("the site answered HTTP 500", 500))]);
    let w = make_gated(&dir.path().join("w.json"), &fetch, &sent, &clock, None);
    add_apple(&w, "change");
    w.watches()[0].lock().errors = 3; // three failures already in this streak
    clock.add(400.0);
    run(remove_mid_check(&w, &fetch, "w1"));
    assert!(w.snapshot().is_empty());
    assert!(sent.lock().is_empty(), "a removed watch texted the owner: {:?}", sent.lock());
}

#[test]
fn test_a_recovery_notice_in_flight_is_not_texted_after_the_watch_is_removed() {
    let dir = tmp();
    let sent: Arc<Mutex<Vec<String>>> = Arc::default();
    let clock = TClock::new();
    let fetch = Gated::new(vec![Step::Price(310.0), Step::Price(305.0)]);
    let w = make_gated(&dir.path().join("w.json"), &fetch, &sent, &clock, None);
    add_apple(&w, "change");
    {
        let first = w.watches()[0].clone();
        let mut g = first.lock();
        g.errors = 5; // the owner was told it cannot be read
        g.told_error = true;
        g.next_at = clock.t();
    }
    run(remove_mid_check(&w, &fetch, "w1"));
    assert!(w.snapshot().is_empty());
    assert!(sent.lock().is_empty(), "a removed watch texted the owner: {:?}", sent.lock());
}

#[test]
fn test_a_removed_watch_id_is_never_handed_out_again() {
    let dir = tmp();
    let path = dir.path().join("w.json");
    let sent: Arc<Mutex<Vec<String>>> = Arc::default();
    let clock = TClock::new();
    let w = make_gated(&path, &Gated::new(vec![Step::Price(310.0), Step::Price(310.0), Step::Price(310.0)]), &sent, &clock, None);
    let first = add_apple(&w, "below")["id"].as_str().unwrap().to_string();
    assert_eq!(w.remove(&first)["ok"], json!(true));
    let second = add_apple(&w, "below")["id"].as_str().unwrap().to_string();
    assert_ne!(second, first, "the removed watch's id {first} was handed out again");
    // and not after a restart either: the store remembers the ids it has handed out
    assert_eq!(w.remove(&second)["ok"], json!(true));
    let again = make_gated(&path, &Gated::new(vec![Step::Price(310.0)]), &sent, &clock, None);
    let third = add_apple(&again, "below")["id"].as_str().unwrap().to_string();
    assert!(third != first && third != second, "a restart handed out {third} again");
}

async fn ticks_like_the_loop(w: &Watcher, clock: &TClock, n: usize) {
    for _ in 0..n {
        // run(): a tick that raises is logged, then retried
        let _ = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(w.tick())).await;
        clock.add(60.0);
    }
}

#[test]
fn test_a_rule_that_raises_does_not_starve_the_watches_after_it() {
    let dir = tmp();
    let sent: Arc<Mutex<Vec<String>>> = Arc::default();
    let clock = TClock::new();
    let fetched: Arc<Mutex<Vec<String>>> = Arc::default();
    let f = fetched.clone();
    let ask = SwapAsk::new(|_| {
        Ok(json!({"choices": [{"message": {"content": json!({"price": 0, "currency": "USD", "available": true,
                                                               "note": "Listed as free."}).to_string()}}]}))
    });
    let mut make = Make::new()
        .fetch(move |url, _| {
            f.lock().push(url.to_string());
            let body = if url.contains("yahoo") {
                quote_body(305.0)
            } else {
                "<html><body><h1>Lamp</h1><p>Free today</p></body></html>".to_string()
            };
            Ok((200, body))
        })
        .clock(&clock)
        .notify(recorder(&sent, &Arc::new(AtomicBool::new(true))));
    make.ask = ask;
    make.offload = true;
    let w = make.build(&dir.path().join("w.json"));
    let out = run(w.add(&obj(json!({"kind": "page", "target": "https://shop.example/lamp", "label": "Lamp",
                                     "condition": "drop_pct", "value": 10, "text": null, "every_minutes": null}))));
    assert!(out["ok"] == json!(true) && w.snapshot()[0].baseline == Some(0.0), "{out}");
    add_apple(&w, "change");
    let (lamp, apple) = (w.watches()[0].clone(), w.watches()[1].clone());
    lamp.lock().next_at = clock.t() - 10.0; // both due; the lamp first in next_at order
    apple.lock().next_at = clock.t();
    let checks = apple.lock().checks;
    run(ticks_like_the_loop(&w, &clock, 3));
    assert!(apple.lock().checks > checks, "the quote watch was never checked: the raising watch starved it");
    let pages = fetched.lock().iter().filter(|u| u.contains("shop.example")).count();
    assert_eq!(pages, 2, "the raising page was refetched every tick: {:?}", fetched.lock());
}

#[test]
fn test_a_reader_that_raises_does_not_starve_the_watches_after_it() {
    let dir = tmp();
    let sent: Arc<Mutex<Vec<String>>> = Arc::default();
    let clock = TClock::new();
    let fetch = Gated::new(vec![Step::Price(310.0), Step::Price(305.0), Step::Price(305.0)]);
    let asks = Arc::new(AtomicUsize::new(0));
    let a = asks.clone();
    let ask = SwapAsk::new(move |_| {
        a.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"choices": [{"message": "upstream hiccup"}]}))
    });
    let w = make_gated(&dir.path().join("w.json"), &fetch, &sent, &clock, Some(ask));
    add_apple(&w, "change");
    let apple = w.watches()[0].clone();
    let mut broken = mk("w2", "search", "tour tickets", "Tour", "available");
    broken.every_secs = 3600;
    broken.next_at = clock.t() - 10.0;
    w.push(broken);
    apple.lock().next_at = clock.t(); // due, after the broken one in next_at order
    let checks = apple.lock().checks;
    run(ticks_like_the_loop(&w, &clock, 3));
    assert!(apple.lock().checks > checks, "the quote watch was never checked: the raising watch starved it");
    assert_eq!(asks.load(Ordering::SeqCst), 1, "the raising watch was asked again on every tick");
}

// ---- test_watch_lean_limiter.py's two Watcher races (left to this unit by watch-rules) ----------------------------

const YAHOO: &str = "finance.yahoo.com";

/// watch_add holds the check lock on a slow Yahoo read; meanwhile the loop's tick lets a second Yahoo watch through
/// the limiter (the 20 s host gap has passed, no backoff yet), takes its token and waits on the lock. Yahoo answers
/// the first read 429 Retry-After 3000; the lock passes to the tick, whose check was let through before that answer.
/// Every later Yahoo read is refused with no Retry-After. Returns the limiter, and what `backed_off(yahoo)` was at
/// each moment a Yahoo read went out. (Python lends a `_call` that holds the first read; here the read runs off the
/// loop and a gate holds it.)
fn race(dir: &Path) -> (Arc<Mutex<RateLimiter>>, Vec<f64>) {
    let clock = TClock::new();
    let lim = Arc::new(Mutex::new(RateLimiter::new(LimiterOptions {
        clock: clock.clock(),
        day: Arc::new(|t: f64| ((t / 86400.0).floor() as i64).to_string()),
        ..LimiterOptions::default()
    })));
    let sent: Arc<Mutex<Vec<f64>>> = Arc::default();
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let (l, s, e, r, c) = (lim.clone(), sent.clone(), entered.clone(), release.clone(), clock.clone());
    let mut make = Make::new().clock(&clock).fetch(move |_, _| {
        let held = l.lock().backed_off(YAHOO);
        let n = {
            let mut s = s.lock();
            s.push(held);
            s.len()
        };
        if n == 1 {
            e.store(true, Ordering::SeqCst);
            let until = Instant::now() + Duration::from_secs(5);
            while !r.load(Ordering::SeqCst) {
                assert!(Instant::now() < until, "the test never released the first read");
                std::thread::sleep(Duration::from_millis(2));
            }
            c.add(10.0);
            return Err(FetchError::with_retry("the site answered HTTP 429", 429, Some(3000.0)));
        }
        Err(FetchError::with_retry("the site answered HTTP 429", 429, None))
    });
    make.offload = true;
    make.shared_limiter = Some(lim.clone());
    make.cfg = Box::new(|_| {});
    let w = make.build(&dir.join("w.json"));
    let quote_args = |symbol: &str, label: &str| {
        obj(json!({"kind": "quote", "target": symbol, "label": label, "condition": "change", "value": null,
                   "text": null, "field": null, "every_minutes": null}))
    };
    let mut other = w.validate(&quote_args("MSFT", "Microsoft")).expect("a valid watch");
    other.next_at = clock.t() + 25.0;
    w.push(other);
    let adding = quote_args("AAPL", "Apple");
    run(async {
        let scene = async {
            while !entered.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(5)).await; // add takes its token and holds the lock
            }
            assert_eq!(sent.lock().len(), 1);
            clock.add(25.0); // past the host gap; MSFT is due
            let open = async {
                for _ in 0..50 {
                    tokio::task::yield_now().await; // tick: ready_in == 0, take, wait on the lock
                }
                release.store(true, Ordering::SeqCst);
            };
            tokio::join!(w.tick(), open);
        };
        tokio::join!(w.add(&adding), scene);
    });
    let sent = sent.lock().clone();
    (lim, sent)
}

#[test]
fn test_a_stale_gate_cannot_shorten_the_hosts_backoff() {
    // Bug 1 through the Watcher: whatever the tick's stale check does, Yahoo's 3000 s must still stand.
    let dir = tmp();
    let (lim, _) = race(dir.path());
    assert!(lim.lock().backed_off(YAHOO) >= 2990.0);
}

#[test]
fn test_no_read_goes_to_a_host_inside_its_hold() {
    // Lean: current_violates_gate. The tick's MSFT check passed ready_in before Yahoo's 429 landed; under the lock
    // it must see the hold and push the watch back, not read.
    let dir = tmp();
    let (_, sent) = race(dir.path());
    assert_eq!(sent[0], 0.0); // add's own read went before any hold
    assert!(sent[1..].iter().all(|x| *x <= 0.0), "a read went while the host was held: {sent:?}");
}

// ---- test_watch_reverify.py: the loop, nothing a watch does stops the others -------------------------------------

/// test_watch_reverify.py's `make`: RateLimiter(rate_per_min=600, burst=50, host_gap_secs=0, clock=clock).
fn make_rv(dir: &Path, make: Make) -> Watcher {
    let mut make = make;
    let clock = make.clock.clone();
    make.limiter = Some(limiter(&clock, |o| {
        *o = LimiterOptions {
            rate_per_min: 600.0,
            burst: 50,
            host_gap_secs: 0.0,
            clock: clock.clock(),
            ..LimiterOptions::default()
        };
    }));
    make.build(&dir.join("w.json"))
}

fn raising_rule() -> Evaluate {
    Arc::new(|_w: &mut Watch, _r: &Reading, _shown: bool| -> Option<String> {
        let zero = std::hint::black_box(0);
        Some((1 / zero).to_string())
    })
}

#[test]
fn test_a_thousand_failures_in_a_row_never_overflow_the_stretch() {
    let dir = tmp();
    let make = Make::new().fetch(|_, _| Err(FetchError::with_status("the site answered HTTP 500", 500)));
    let clock = make.clock.clone();
    let w = make_rv(dir.path(), make);
    let mut item = mk("w1", "quote", "X", "X", "change");
    item.every_secs = 60;
    item.errors = 1100;
    let item = w.push(item);
    run(w.run_one(&item)); // 2.0 ** 1100 raised OverflowError out of the tick
    let g = item.lock();
    assert!(g.errors == 1101 && g.next_at <= clock.t() + 60.0 * watch::MAX_ERROR_STRETCH as f64 * 1.2);
}

#[test]
fn test_a_rule_that_raises_builds_a_streak_and_the_owner_hears() {
    let dir = tmp();
    let (told, notify) = sink();
    let mut make = Make::new().fetch(|_, _| Ok((200, quote(10.0, "USD")))).notify(notify);
    make.evaluate = Some(raising_rule());
    let w = make_rv(dir.path(), make);
    let mut item = mk("w1", "quote", "X", "Apple", "change");
    item.every_secs = 60;
    let item = w.push(item);
    for _ in 0..watch::TELL_AFTER_ERRORS {
        item.lock().next_at = 0.0;
        run(w.run_one(&item));
    }
    assert_eq!(item.lock().errors, watch::TELL_AFTER_ERRORS); // it stayed at 1 when the reset ran first
    assert!(told.lock().last().is_some_and(|t| t.starts_with("I can't read Apple right now")));
}

#[test]
fn test_a_first_check_that_raises_keeps_the_watch() {
    let dir = tmp();
    let mut make = Make::new().fetch(|_, _| Ok((200, quote(10.0, "USD"))));
    make.evaluate = Some(raising_rule());
    let w = make_rv(dir.path(), make);
    let out = run(w.add(&obj(json!({"kind": "quote", "target": "X", "label": "X", "condition": "change", "value": null,
                                     "text": null, "city": null, "every_minutes": null}))));
    assert!(out["ok"] == json!(true) && out["now"].as_str().unwrap().starts_with("the first check failed"));
    assert_eq!(w.snapshot().len(), 1);
}

#[test]
fn test_a_bare_currency_never_locks_the_watch_and_a_foreign_price_is_a_counted_error() {
    let dir = tmp();
    let mut w = mk("w1", "page", "https://shop.example/p", "Lamp", "drop_pct");
    w.value = Some(10.0);
    let r = |v: Option<f64>, c: &str| Reading { value: v, currency: c.into(), ..Reading::default() };
    rules::evaluate(&mut w, &r(Some(100.0), "USD"), true);
    rules::evaluate(&mut w, &Reading { available: Some(true), ..r(None, "EUR") }, true); // no price: the currency stays
    assert_eq!(w.currency, "USD");
    assert_eq!(rules::evaluate(&mut w, &r(Some(80.0), "EUR"), true), None); // never compared across currencies
    assert!(rules::evaluate(&mut w, &r(Some(85.0), "USD"), true).is_some());
    let readings = Arc::new(Mutex::new(VecDeque::from([quote(100.0, "USD"), quote(90.0, "EUR")])));
    let rd = readings.clone();
    let wr = make_rv(dir.path(), Make::new().fetch(move |_, _| Ok((200, rd.lock().pop_front().expect("a reading")))));
    let mut item = mk("w2", "quote", "X", "X", "below");
    item.value = Some(95.0);
    item.every_secs = 60;
    let item = wr.push(item);
    run(wr.run_one(&item));
    item.lock().next_at = 0.0;
    run(wr.run_one(&item));
    let snap = item.lock().clone();
    assert!(snap.errors == 1 && snap.last_error.contains("EUR"));
}

#[test]
fn test_openrouter_refusals_never_move_the_page_or_refuse_the_watch() {
    let dir = tmp();
    let w = make_rv(
        dir.path(),
        Make::new()
            .fetch(|_, _| Ok((200, "<p>no price here</p>".to_string())))
            .ask(|_| Err(FetchError::with_status("the site answered HTTP 401", 401).at(watch::OPENROUTER_HOST))),
    );
    let mut item = mk("w1", "page", "https://shop.example/p", "x", "below");
    item.value = Some(5.0);
    item.every_secs = 600;
    let item = wref(item);
    assert!(run(w.check(&item, false)).is_err());
    assert_eq!(item.lock().via, ""); // a bad OpenRouter key said nothing about the page
    let b = dir.path().join("b");
    let w2 = make_rv(
        &b,
        Make::new()
            .fetch(|_, _| Ok((200, "<p>no price here</p>".to_string())))
            .ask(|_| Err(FetchError::with_status("the site answered HTTP 404", 404).at(watch::OPENROUTER_HOST))),
    );
    let out = run(w2.add(&obj(json!({"kind": "page", "target": "https://shop.example/p", "label": "x", "condition": "below",
                                      "value": 5, "text": null, "city": null, "every_minutes": null}))));
    assert!(out["ok"] == json!(true) && out["now"].as_str().unwrap().starts_with("the first check failed")); // kept
}

#[test]
fn test_a_model_call_waits_out_openrouters_hold() {
    let dir = tmp();
    let asks = Arc::new(AtomicUsize::new(0));
    let a = asks.clone();
    let w = make_rv(
        dir.path(),
        Make::new().fetch(|_, _| Ok((200, "<p>no price here</p>".to_string()))).ask(move |_| {
            a.fetch_add(1, Ordering::SeqCst);
            Ok(json!({}))
        }),
    );
    w.limiter.lock().penalize(watch::OPENROUTER_HOST, Some(3000.0));
    let mut item = mk("w1", "page", "https://shop.example/p", "x", "below");
    item.value = Some(5.0);
    item.every_secs = 600;
    let e = run(w.read(&wref(item))).unwrap_err();
    assert!(e.reason.contains("asked me to wait"), "{}", e.reason);
    assert_eq!(asks.load(Ordering::SeqCst), 0);
}

#[test]
fn test_a_browser_page_that_loses_its_price_is_a_failed_read() {
    let dir = tmp();
    let make = Make::new().ask(|_| Ok(reply(json!({"price": null, "available": null})))).cfg(|c| c.browser = true);
    make.render.set(|_| Ok(("<p>gone</p>".to_string(), b"jpeg".to_vec())));
    let w = make_rv(dir.path(), make);
    let mut item = mk("w1", "page", "https://shop.example/p", "x", "below");
    item.value = Some(5.0);
    item.every_secs = 900;
    item.via = "browser".into();
    let e = run(w.read(&wref(item))).unwrap_err();
    assert!(e.reason.contains("does not show a price"), "{}", e.reason);
}

#[test]
fn test_a_phrase_watch_never_falls_back_to_search() {
    let dir = tmp();
    let w = make_rv(
        dir.path(),
        Make::new().fetch(|_, _| Err(FetchError::with_status("the site answered HTTP 403", 403))).cfg(|c| c.browser = false),
    );
    let mut item = mk("w1", "page", "https://tix.example/e", "x", "appears");
    item.text = "Buy tickets".into();
    item.every_secs = 600;
    let item = wref(item);
    assert!(run(w.check(&item, false)).is_err());
    let snap = item.lock().clone();
    assert!(snap.via.is_empty() && snap.errors == 1);
}

#[test]
fn test_no_link_hides_behind_words_from_a_search_url_or_a_ticketmaster_note() {
    let bad = "https://www.ticketmaster.com/[Buy](https://tix-checkout.example/pay)";
    assert_eq!(watch::model_reading(&reply(json!({"available": true, "url": bad})), "search").unwrap().url, "");
    assert!(!watch::model_reading(&reply(json!({"available": true, "url": "https://good.example/e/1"})), "search").unwrap().url.is_empty());
    let now = 1_800_000_000.0;
    let event = obj(json!({"id": "e", "name": "Show", "url": "https://www.ticketmaster.com/e/1",
                           "dates": {"status": {"code": "offsale"}, "start": {"localDate": "2026-12-01"}},
                           "sales": {"public": {"startDateTime": "2027-01-01T00:00:00Z"},
                                     "presales": [{"name": "[Claim your code](https://tix-claim.example/sso)",
                                                   "startDateTime": "2026-01-01T00:00:00Z", "endDateTime": "2027-06-01T00:00:00Z"}]}}));
    let mut w = mk("w1", "ticketmaster", "Show", "Show", "available");
    w.checks = 1;
    w.armed = true;
    let alert = rules::evaluate(&mut w, &watch::ticketmaster_reading(&[event], now), true).unwrap();
    // telegram_format.compose turns "[words](link)" into a link; no such markup is left for it
    assert!(!alert.contains("](") && !alert.contains('['), "{alert}");
}

#[test]
fn test_a_ticketmaster_price_past_a_float_is_no_price() {
    // serde_json holds no number past a float (Python's json reads 1e400 as inf and 10**400 as an int): the
    // largest finite one stands in, past MAX_PRICE all the same
    let event = obj(json!({"id": "e", "name": "Show", "url": "https://www.ticketmaster.com/e/1",
                           "dates": {"status": {"code": "onsale"}},
                           "priceRanges": [{"min": f64::MAX, "currency": "USD"}, {"min": 1e300, "currency": "USD"}]}));
    let r = watch::ticketmaster_reading(&[event], 1_800_000_000.0);
    assert!(r.value.is_none() && r.available == Some(true));
    assert!(rules::money(Some(f64::INFINITY), "USD") == "?" && rules::money(Some(f64::NAN), "USD") == "?");
}

// ---- the store --------------------------------------------------------------------------------------------------

#[test]
fn test_a_hostile_store_is_read_defensively_and_never_raises() {
    let dir = tmp();
    let mut good = mk("w1", "quote", "AAPL", "A", "below");
    good.value = Some(300.0);
    good.every_secs = 900;
    let good = obj(good.to_json());
    let with = |pairs: Value| {
        let mut m = good.clone();
        for (k, v) in obj(pairs) {
            m.insert(k, v);
        }
        Value::Object(m).to_string()
    };
    let second = with(json!({"id": "w2"}));
    let raw = format!(
        "{{\"watches\": [{}, {}, \"next_at\": 1{}, \"baseline\": NaN}}, {}]}}",
        with(json!({"value": "300", "last_value": null})),
        &second[..second.len() - 1],
        "0".repeat(400),
        with(json!({"id": format!("w{}", "9".repeat(5000))}))
    );
    std::fs::write(dir.path().join("w.json"), raw).unwrap();
    let w = make_rv(dir.path(), Make::new());
    assert_eq!(ids(&w), ["w1", "w2"]);
    let s = w.snapshot();
    assert!(s[0].value.is_none() && s[1].next_at == 0.0 && s[1].baseline.is_none());
    assert!(w.listing().starts_with("w1 ·")); // /watch answers
    run(w.tick());
}

#[test]
fn test_an_unreadable_store_is_kept_aside_and_the_owner_is_told() {
    let dir = tmp();
    std::fs::write(dir.path().join("w.json"), r#"{"watches": [{"id": "w7", "kind": "quote", "target": "X",}]}"#).unwrap(); // a typo
    let (told, notify) = sink();
    let w = make_rv(dir.path(), Make::new().notify(notify).fetch(|_, _| Ok((200, quote(5.0, "USD")))));
    let aside: Vec<PathBuf> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("w.json.bad-"))
        .collect();
    assert!(w.snapshot().is_empty() && aside.len() == 1 && std::fs::read_to_string(&aside[0]).unwrap().contains("\"w7\""));
    run(w.add(&obj(json!({"kind": "quote", "target": "X", "label": "X", "condition": "change", "value": null,
                          "text": null, "city": null, "every_minutes": null}))));
    assert_eq!(w.snapshot()[0].id, "w8"); // ids go on from the old file's
    run(w.tick());
    assert!(told.lock().first().is_some_and(|t| t.starts_with("I couldn't read my watch list")));
}

#[test]
fn test_a_huge_store_loads_quickly() {
    let dir = tmp();
    let mut item = mk("w1", "quote", "X", "X", "change");
    item.every_secs = 900;
    let item = obj(item.to_json());
    let list: Vec<Value> = (1..=20000)
        .map(|i| {
            let mut m = item.clone();
            m.insert("id".into(), json!(format!("w{i}")));
            Value::Object(m)
        })
        .collect();
    std::fs::write(dir.path().join("w.json"), json!({"watches": list}).to_string()).unwrap();
    let t0 = Instant::now();
    let w = make_rv(dir.path(), Make::new());
    assert!(w.snapshot().len() == watch::MAX_WATCHES && t0.elapsed().as_secs_f64() < 3.0);
}

// ---- pending alerts ---------------------------------------------------------------------------------------------

#[test]
fn test_pending_alerts_queue_back_off_and_skip_removed_watches() {
    let dir = tmp();
    let sends: Arc<Mutex<Vec<String>>> = Arc::default();
    let up = Arc::new(AtomicBool::new(false));
    let clock = TClock::new();
    let prices = Arc::new(Mutex::new(VecDeque::from([310.0, 290.0, 310.0, 280.0])));
    let pr = prices.clone();
    let w = make_rv(
        dir.path(),
        Make::new()
            .fetch(move |_, _| Ok((200, quote(pr.lock().pop_front().expect("a price"), "USD"))))
            .notify(recorder(&sends, &up))
            .clock(&clock),
    );
    let mut item = mk("w1", "quote", "X", "Apple", "below");
    item.value = Some(300.0);
    item.every_secs = 60;
    let item = w.push(item);
    for _ in 0..4 {
        item.lock().next_at = 0.0;
        run(w.run_one(&item));
    }
    let pending = item.lock().pending.clone();
    assert!(pending.contains("dropped to $290") && pending.contains("dropped to $280"), "{pending}"); // oldest first
    assert!(pending.find("$290").unwrap() < pending.find("$280").unwrap());
    let n = sends.lock().len();
    run(w.tick()); // inside the backoff: no resend storm
    run(w.tick());
    assert!(sends.lock().len() <= n + 1);
    w.remove("w1");
    up.store(true, Ordering::SeqCst);
    clock.add(watch::PENDING_RETRY_SECS + 1.0);
    run(w.tick());
    assert!(!sends.lock()[n + 1..].iter().any(|s| s.contains("dropped"))); // a removed watch's alert is not resent
}

// ---- headers and bodies the server controls ---------------------------------------------------------------------

#[test]
fn test_odd_retry_headers_are_no_hold_not_a_crash() {
    let now = eve_web::time_time();
    let one = |name: &'static str, value: String| move |n: &str| (n == name).then(|| value.clone());
    assert_eq!(net::retry_after(&one("Retry-After", "²".into()), now), None);
    assert_eq!(net::retry_after(&one("Retry-After", "9".repeat(400)), now), None);
    assert_eq!(net::retry_after(&one("Rate-Limit-Reset", "9".repeat(400)), now), None);
}

/// A loopback server: each connection's requests are read (heads only) and answered by `handler`, which writes
/// the response itself and says whether to read another request on the same connection.
fn serve(handler: impl Fn(&mut TcpStream, &str) -> bool + Send + Sync + 'static) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().unwrap().port();
    let handler = Arc::new(handler);
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            let handler = handler.clone();
            std::thread::spawn(move || loop {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    match conn.read(&mut byte) {
                        Ok(1) => head.push(byte[0]),
                        _ => return,
                    }
                }
                if !handler(&mut conn, &String::from_utf8_lossy(&head)) {
                    return;
                }
            });
        }
    });
    port
}

fn header_of<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

/// `loopback_is_public`: check_url passes and every address is public, for these tests' loopback servers.
fn open_rules() -> AddressRules {
    AddressRules { ip_public: Arc::new(|_ip: IpAddr| true), resolve: Some(Arc::new(|_h: &str| true)), ..AddressRules::system() }
}

fn open_opts() -> HttpOptions {
    HttpOptions { rules: Some(open_rules()), ..HttpOptions::default() }
}

#[test]
fn test_a_non_text_charset_is_read_as_utf8() {
    let port = serve(|c, _| {
        let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=base64\r\nContent-Length: 10\r\nConnection: close\r\n\r\n<p>$12</p>");
        false
    });
    assert_eq!(net::http_request(&format!("http://127.0.0.1:{port}/"), &open_opts()).unwrap().1, "<p>$12</p>");
}

#[test]
fn test_the_key_never_follows_a_redirect_to_another_port() {
    let seen: Arc<Mutex<Option<Option<String>>>> = Arc::default();
    let s = seen.clone();
    let po = serve(move |c, head| {
        *s.lock() = Some(header_of(head, "Authorization").map(str::to_string));
        let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
        false
    });
    let pb = serve(move |c, _| {
        let _ = c.write_all(format!("HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{po}/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes());
        false
    });
    let opts = HttpOptions { headers: vec![("Authorization".into(), "Bearer sk-TEST".into())], ..open_opts() };
    let _ = net::http_request(&format!("http://127.0.0.1:{pb}/"), &opts);
    let got = seen.lock().clone();
    assert!(matches!(got, Some(None)), "{got:?}");
}

#[test]
fn test_no_credential_header_follows_a_redirect_to_another_origin() {
    let seen: Arc<Mutex<HashMap<String, String>>> = Arc::default();
    let s = seen.clone();
    let po = serve(move |c, head| {
        for line in head.lines().skip(1) {
            if let Some((k, v)) = line.split_once(':') {
                s.lock().insert(k.trim().to_lowercase(), v.trim().to_string());
            }
        }
        let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
        false
    });
    let pb = serve(move |c, _| {
        let _ = c.write_all(format!("HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{po}/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes());
        false
    });
    let headers = [("X-API-Key", "tf-TEST"), ("x-goog-api-key", "g-TEST"), ("Proxy-Authorization", "Basic x"), ("Accept", "application/json")];
    let opts = HttpOptions { headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(), ..open_opts() };
    let _ = net::http_request(&format!("http://127.0.0.1:{pb}/"), &opts);
    let lowered = seen.lock().clone();
    assert!(!["x-api-key", "x-goog-api-key", "proxy-authorization"].iter().any(|k| lowered.contains_key(*k)), "{lowered:?}");
    assert_eq!(lowered.get("accept").map(String::as_str), Some("application/json")); // a plain header still crosses
}

#[test]
fn test_trickled_headers_are_cut_at_the_deadline() {
    let port = serve(|c, _| {
        let _ = c.write_all(b"HTTP/1.1 200 OK\r\n");
        for _ in 0..10 {
            if c.write_all(b"X-Slow: y\r\n").is_err() {
                break;
            }
            let _ = c.flush();
            std::thread::sleep(Duration::from_millis(800));
        }
        false
    });
    let t0 = Instant::now();
    let opts = HttpOptions { timeout: 1.5, ..open_opts() };
    let e = net::http_request(&format!("http://127.0.0.1:{port}/"), &opts).unwrap_err();
    assert!(e.reason.contains("too long"), "{}", e.reason);
    assert!(t0.elapsed().as_secs_f64() < 4.0);
}

// ---- parsing ----------------------------------------------------------------------------------------------------

#[test]
fn test_main_text_survives_a_template_string_and_a_custom_nav() {
    let page = "<body><script>var tpl = \"<footer class=x>\";</script><main><h1>Lamp</h1><p>$49.00</p></main><footer>Gift cards $10</footer></body>";
    assert!(page::main_text(page).contains("$49.00"));
    let nav = "<body><nav><nav-drawer>menu</nav-drawer><span>Cart $0.00</span></nav><p>Lamp $49.00</p></body>";
    let text = page::main_text(nav);
    assert!(text.contains("$49.00") && !text.contains("Cart"), "{text}");
}

#[test]
fn test_structured_skips_comments_and_unit_prices_and_reads_variants() {
    let commented = concat!(
        r#"<!-- <script type="application/ld+json">{"offers":{"price":1}}</script> -->"#,
        r#"<script type="application/ld+json">{"@type":"Product","offers":{"price":20}}</script>"#
    );
    assert_eq!(page::structured(commented).value, Some(20.0));
    let unit = concat!(
        r#"<script type="application/ld+json">{"@type":"Product","offers":{"@type":"Offer","price":"12.00","#,
        r#""priceSpecification":[{"@type":"UnitPriceSpecification","price":2.40,"#,
        r#""referenceQuantity":{"value":100,"unitCode":"GRM"}}]}}</script>"#
    );
    assert_eq!(page::structured(unit).value, Some(12.0));
    let group = concat!(
        r#"<script type="application/ld+json">{"@type":"ProductGroup","name":"Tee","hasVariant":["#,
        r#"{"@type":"Product","offers":{"price":25,"availability":"InStock"}},"#,
        r#"{"@type":"Product","offers":{"price":22,"availability":"OutOfStock"}}]}</script>"#
    );
    let r = page::structured(group);
    assert_eq!((r.value, r.available), (Some(22.0), Some(true)));
}

#[test]
fn test_prices_by_the_currency_sign_and_three_decimal_currencies() {
    assert!(page::price(&json!("Qty 3 · $45.00"), "") == Some(45.0) && page::price(&json!("2 for $30"), "") == Some(30.0));
    assert!(page::price(&json!("19.990"), "KWD") == Some(19.99) && page::price(&json!("19.990"), "USD") == Some(19990.0));
    assert_eq!(page::price(&json!(10_000_000_000_000i64), ""), None);
}

// ---- the network guard ------------------------------------------------------------------------------------------

#[test]
fn test_this_macs_own_ipv6_prefixes_are_the_lan() {
    // Python patches is_global to call 2001:db8:: (documentation space) global; a real global prefix stands in
    let lan = [IpNet { addr: "2606:4700:88c0::".parse().unwrap(), prefix: 64 }];
    assert!(!net::ip_public("2606:4700:88c0::95ab".parse().unwrap(), Some(&lan)));
    assert!(net::ip_public("2606:4700:ffff::1".parse().unwrap(), Some(&lan)));
    // the interfaces cannot be read
    assert!(!net::ip_public("2606:4700:ffff::1".parse().unwrap(), None));
    assert!(net::ip_public("93.184.215.14".parse().unwrap(), None));
}

#[test]
fn test_the_real_interfaces_are_read_and_this_macs_own_address_is_refused() {
    let nets = net::local_networks().expect("the interfaces");
    assert!(nets.iter().any(|n| n.addr.is_ipv4()));
    let ours = nets.iter().find(|n| match n.addr {
        IpAddr::V6(a) => {
            let s = a.segments()[0];
            (0x2000..0x4000).contains(&s) && !(a.segments()[0] == 0x2001 && a.segments()[1] == 0x0db8)
        }
        IpAddr::V4(_) => false,
    });
    let Some(ours) = ours else {
        eprintln!("skipped: no global IPv6 prefix on this Mac's interfaces right now");
        return;
    };
    let IpAddr::V6(a) = ours.addr else { return };
    let bits = u128::from(a);
    let mask = if ours.prefix == 0 { 0 } else { u128::MAX << (128 - u32::from(ours.prefix)) };
    let one_in = std::net::Ipv6Addr::from((bits & mask) + 1);
    assert!(!net::ip_public_here(IpAddr::V6(one_in)));
}

#[test]
fn test_the_proxy_carries_one_request_per_connection() {
    let got: Arc<Mutex<Vec<String>>> = Arc::default();
    let g = got.clone();
    let port = serve(move |c, head| {
        let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
        g.lock().push(path);
        let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        true
    });
    let mut proxy = eve_web::watch::render::GuardProxy::start(open_rules()).expect("the proxy");
    let mut c = TcpStream::connect(("127.0.0.1", proxy.port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let two = format!(
        "GET http://127.0.0.1:{port}/first HTTP/1.1\r\nHost: x\r\n\r\nGET http://127.0.0.1:{port}/second HTTP/1.1\r\nHost: y\r\n\r\n"
    );
    c.write_all(two.as_bytes()).unwrap();
    let mut data = Vec::new();
    let _ = c.read_to_end(&mut data);
    proxy.shutdown();
    let text = String::from_utf8_lossy(&data);
    assert!(*got.lock() == ["/first"] && text.matches("HTTP/1.").count() == 1, "{:?} {text}", got.lock());
}

#[test]
fn test_a_render_that_hangs_is_killed_with_everything_it_started() {
    // Chromium runs in a process group of its own, so killing the child's group missed it (ps, 2026-09-25). The
    // child here starts its grandchild in a new session, as Playwright does with Chromium: both must go.
    let dir = tmp();
    let pidfile = dir.path().join("gc.pid");
    let child = format!(
        "import subprocess, sys, time\n\
         g = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(600)'], start_new_session=True)\n\
         open({:?}, 'w').write(str(g.pid))\n\
         time.sleep(600)\n",
        pidfile.display().to_string()
    );
    let argv = vec!["python3".to_string(), "-c".to_string(), child];
    let e = watch::render("https://93.184.215.14/", 30.0, Some(3.0), Some(argv)).unwrap_err();
    assert!(e.reason.contains("too long to render"), "{}", e.reason);
    let gc: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
    let mut gone = false;
    for _ in 0..50 {
        // SAFETY: signal 0 only asks whether the process exists.
        if unsafe { libc::kill(gc, 0) } != 0 {
            gone = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if !gone {
        // SAFETY: the grandchild this test started.
        unsafe { libc::kill(gc, 9) };
        panic!("the grandchild in its own session outlived the render's deadline");
    }
}

// ---- the chat ---------------------------------------------------------------------------------------------------

#[test]
fn test_an_alert_reaches_the_history_only_as_code_built_words() {
    let dir = tmp();
    let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    let s = seen.clone();
    let notify: Notify = Arc::new(move |text: String, about: String| -> BoxFuture<'static, anyhow::Result<bool>> {
        s.lock().push((text, about));
        Box::pin(async { Ok(true) })
    });
    let w = make_rv(dir.path(), Make::new().notify(notify).fetch(|_, _| Ok((200, quote(290.0, "USD")))));
    let mut item = mk("w3", "quote", "X", "Apple", "below");
    item.value = Some(300.0);
    item.every_secs = 60;
    item.checks = 1;
    item.armed = true;
    item.last_value = Some(310.0);
    let item = w.push(item);
    run(w.run_one(&item));
    let (text, about) = seen.lock().last().cloned().unwrap();
    assert!(text.starts_with("Apple dropped to") && about == "(I texted a watch alert: w3, Apple, at or below $300.)", "{text} / {about}");
}

// ---- live: the browser ------------------------------------------------------------------------------------------

#[test]
#[ignore = "live: needs headless Chromium and test_watch_security's LAN rig (watch-pages unit)"]
fn test_negative_control_the_guard_test_sees_a_leak_when_the_guard_is_open() {
    // test_watch_security's guard test is only worth something if it can fail: with every address let through,
    // the same page must reach the LAN service. The page rig lives with the render's own tests.
    let rules = open_rules();
    let _ = eve_web::watch::render::render_here("http://127.0.0.1:9/page", 15.0, &rules);
}

#[test]
#[ignore = "live: needs headless Chromium"]
fn test_an_overlay_button_that_navigates_is_undone() {
    let port = serve(|c, head| {
        let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
        let body: &str = match path.as_str() {
            "/other" => "<h1>Other item</h1><p>$1.00</p>",
            "/reload?entered=1" => "<h1>Lamp</h1><p>$49.00</p><p>entered</p>",
            "/reload" => "<h1>Lamp</h1><p>$49.00</p><div class=modal style=\"position:fixed;inset:0;background:#fff\"><button onclick=\"location.search='?entered=1'\">Continue</button></div>",
            _ => "<h1>Lamp</h1><p id=price>$49.00</p><div class=modal style=\"position:fixed;inset:0;background:#fff\"><a href=\"/other\">Shop now</a><button onclick=\"location.href='/other'\">Continue</button></div>",
        };
        let _ = c.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
        false
    });
    let rules = open_rules();
    let (html, _) = eve_web::watch::render::render_here(&format!("http://127.0.0.1:{port}/"), 15.0, &rules).unwrap();
    assert!(html.contains("$49.00") && !html.contains("Other item")); // never another page's price
    // a button that reloads the same page with a query (LEGO's "Continue") is not another page: kept
    let (html, _) = eve_web::watch::render::render_here(&format!("http://127.0.0.1:{port}/reload"), 15.0, &rules).unwrap();
    assert!(html.contains("$49.00") && html.contains("entered"));
}

// ---- test_watch_tls.py: where the Chrome-like read sits on the ladder ------------------------------------------

/// test_watch_tls.py / test_watch_tinyfish.py's `refused(status)`: every read answers HTTP `status`.
fn refused(status: u16) -> impl Fn(&str, &HttpOptions) -> Result<(u16, String), FetchError> + Send + Sync + 'static {
    move |_, _| Err(FetchError::with_status(format!("the site answered HTTP {status}"), status))
}

/// A fake reader that records each URL and answers JSON_LD_EVENT (Python's `crawler(pages)` and the tls lambdas).
fn recording(seen: &Arc<Mutex<Vec<String>>>) -> impl Fn(&str, &HttpOptions) -> Result<(u16, String), FetchError> + Send + Sync + 'static {
    let seen = seen.clone();
    move |url, _| {
        seen.lock().push(url.to_string());
        Ok((200, JSON_LD_EVENT.to_string()))
    }
}

fn below_100(target: &str) -> Map<String, Value> {
    args(json!({"target": target, "condition": "below", "value": 100}))
}

#[test]
fn test_a_refused_page_is_read_as_chrome_would_and_remembered() {
    let dir = tmp();
    let path = dir.path().join("w.json");
    let tls_calls: Arc<Mutex<Vec<String>>> = Arc::default();
    let mut make = Make::new().fetch(refused(403)).cfg(|c| c.tls = true);
    make.tls = Some(Swap::new(recording(&tls_calls)));
    let fetch = make.fetch.clone();
    let w = make.build(&path);
    let out = run(w.add(&below_100("https://shop.example/p")));
    assert!(out["ok"] == json!(true) && out["now"].as_str().unwrap().contains("85"), "{out}");
    assert_eq!(*tls_calls.lock(), vec!["https://shop.example/p".to_string()]);
    let item = w.watches()[0].clone();
    {
        let g = item.lock();
        // no browser floor
        assert!(g.via == "tls" && g.every_secs == watch::default_every_secs("page"), "{} {}", g.via, g.every_secs);
    }
    // from now on straight to the Chrome-like read: the plain read is not tried again
    fetch.set(|_, _| panic!("a tls page is not fetched plainly"));
    run(w.read(&item)).unwrap();
    assert_eq!(tls_calls.lock().len(), 2);
    // and it survives a restart
    let again = Make::new().cfg(|c| c.tls = true).build(&path);
    assert_eq!(again.snapshot()[0].via, "tls");
}

#[test]
fn test_a_site_that_never_answers_is_read_as_chrome_would() {
    let dir = tmp();
    let mut make = Make::new().fetch(|_, _| Err(FetchError::new(watch::tls::TOO_LONG))).cfg(|c| c.tls = true);
    make.tls = Some(Swap::new(|_, _| Ok((200, JSON_LD_EVENT.to_string()))));
    let w = make.build(&dir.path().join("w.json"));
    assert_eq!(run(w.add(&below_100("https://shop.example/p")))["ok"], json!(true));
    assert_eq!(w.snapshot()[0].via, "tls");
}

#[test]
fn test_a_rate_limit_is_honoured_not_dodged() {
    let dir = tmp();
    let mut make = Make::new().fetch(refused(429)).cfg(|c| c.tls = true);
    make.tls = Some(Swap::new(|_, _| panic!("a 429 is not retried another way")));
    let w = make.build(&dir.path().join("w.json"));
    let out = run(w.add(&below_100("https://shop.example/p")));
    assert!(out["now"].as_str().unwrap().contains("first check failed"), "{out}");
    assert_eq!(w.snapshot()[0].via, "");
}

#[test]
fn test_when_the_chrome_like_read_is_refused_too_the_browser_comes_next() {
    let dir = tmp();
    let renders: Arc<Mutex<Vec<String>>> = Arc::default();
    let mut make = Make::new().fetch(refused(403)).cfg(|c| {
        c.tls = true;
        c.browser = true;
    });
    make.tls = Some(Swap::new(refused(403)));
    let r = renders.clone();
    make.render = SwapRender::new(move |url| {
        r.lock().push(url.to_string());
        Ok((JSON_LD_EVENT.to_string(), Vec::new()))
    });
    let w = make.build(&dir.path().join("w.json"));
    let out = run(w.add(&below_100("https://shop.example/p")));
    assert_eq!(out["ok"], json!(true), "{out}");
    assert_eq!(*renders.lock(), vec!["https://shop.example/p".to_string()]);
    assert_eq!(w.snapshot()[0].via, "browser");
}

#[test]
fn test_off_means_off() {
    let dir = tmp();
    let mut make = Make::new().fetch(refused(403)).cfg(|c| c.tls = false);
    make.tls = Some(Swap::new(|_, _| panic!("the Chrome-like read is off")));
    let w = make.build(&dir.path().join("w.json"));
    run(w.add(&below_100("https://shop.example/p")));
    let via = w.snapshot()[0].via.clone();
    assert!(via.is_empty() || via == "search", "{via}");
    // make_watcher lends the real reader; the Watcher alone lends none: with tls on in the config and no reader
    // lent, a refused page skips the Chrome-like rung
    let bare_deps = Make::new().fetch(refused(403)).cfg(|c| c.tls = true);
    let w = bare_deps.build(&dir.path().join("x.json"));
    run(w.add(&below_100("https://shop.example/p")));
    assert_ne!(w.snapshot()[0].via, "tls");
}

#[test]
fn test_the_tls_switch() {
    // test_watch_tls.py's test_the_switch: `_tls_on` is auto by default (on when the reader is here), "0" is off
    let ctx = |pairs: &[(&str, &str)]| {
        let env: Environ = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Ctx::global().clone().with_env(env)
    };
    assert_eq!(watch::config::tls_on(&ctx(&[])), watch::tls_available());
    assert!(!watch::config::tls_on(&ctx(&[("EVE_WATCH_TLS", "0")])));
    assert!(!watch::config::tls_on(&ctx(&[("EVE_WATCH_TLS", "off")])));
    if !watch::tls_available() {
        // Python patches tls_available to False: on means off when the reader is missing
        assert!(!watch::config::tls_on(&ctx(&[("EVE_WATCH_TLS", "1")])));
    } else {
        assert!(watch::config::tls_on(&ctx(&[("EVE_WATCH_TLS", "1")])));
    }
}

// ---- test_watch_tinyfish.py: the hosted rung on the watcher's ladder -------------------------------------------

/// Every spend line the test's own ledger holds (Python's `spent` fixture).
fn spent_rows(dir: &Path) -> Vec<Map<String, Value>> {
    let mut rows = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            for line in std::fs::read_to_string(e.path()).unwrap_or_default().lines() {
                if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(line) {
                    rows.push(m);
                }
            }
        }
    }
    rows
}

/// `make_watcher(path, fetch=refused(403), hosted=True, ...)` with its own ledger in `dir/spend`.
fn hosted_make(dir: &Path, crawl: Swap) -> Make {
    let mut make = Make::new().fetch(refused(403)).cfg(|c| c.hosted = true);
    make.crawl = Some(crawl);
    make.ledger = Some(dir.join("spend"));
    make
}

#[test]
fn test_a_page_every_local_reader_is_refused_is_read_through_tinyfish() {
    let dir = tmp();
    let crawled: Arc<Mutex<Vec<String>>> = Arc::default();
    let mut make = hosted_make(dir.path(), Swap::new(recording(&crawled))).cfg(|c| {
        c.hosted = true;
        c.tls = true;
        c.browser = true;
    });
    make.tls = Some(Swap::new(refused(403)));
    make.render = SwapRender::new(|_| Err(FetchError::with_status("the page shows a bot check", 403)));
    let (fetch, tls, render) = (make.fetch.clone(), make.tls.clone().unwrap(), make.render.clone());
    let w = make.build(&dir.path().join("w.json"));
    let out = run(w.add(&below_100("https://shop.example/p")));
    let item = w.watches()[0].clone();
    assert!(out["ok"] == json!(true) && out["now"].as_str().unwrap().contains("85"), "{out}");
    assert!(out["note"].as_str().unwrap_or("").contains("TinyFish"), "{out}");
    {
        let g = item.lock();
        assert!(g.via == "hosted" && g.every_secs >= watch::floor_secs("hosted"), "{} {}", g.via, g.every_secs);
    }
    assert_eq!(*crawled.lock(), vec!["https://shop.example/p".to_string()]);
    let rows = spent_rows(&dir.path().join("spend"));
    assert!(!rows.is_empty(), "no spend line");
    let first = &rows[0];
    assert_eq!(
        (first["p"].as_str(), first["m"].as_str(), first["f"].as_str(), first["usd"].as_f64()),
        (Some("tinyfish"), Some("fetch"), Some(eve_spend::WATCH), Some(0.0))
    );
    // from now on straight to TinyFish; nothing local is tried
    fetch.set(|_, _| panic!("a hosted page is not read locally"));
    tls.set(|_, _| panic!("a hosted page is not read locally"));
    render.set(|_| panic!("a hosted page is not rendered"));
    run(w.read(&item)).unwrap();
    assert_eq!(crawled.lock().len(), 2);
    assert!(w.listing().contains("(through TinyFish)"), "{}", w.listing());
}

#[test]
fn test_without_the_browser_tinyfish_comes_straight_after_the_chrome_like_read() {
    let dir = tmp();
    let crawled: Arc<Mutex<Vec<String>>> = Arc::default();
    let mut make = hosted_make(dir.path(), Swap::new(recording(&crawled))).cfg(|c| {
        c.hosted = true;
        c.tls = true;
        c.browser = false;
    });
    make.tls = Some(Swap::new(refused(403)));
    let w = make.build(&dir.path().join("w.json"));
    assert_eq!(run(w.add(&below_100("https://shop.example/p")))["ok"], json!(true));
    assert_eq!(w.snapshot()[0].via, "hosted");
    assert_eq!(crawled.lock().len(), 1);
}

#[test]
fn test_a_page_tinyfish_is_refused_too_goes_to_search() {
    let dir = tmp();
    let w = hosted_make(dir.path(), Swap::new(refused(403))).build(&dir.path().join("w.json"));
    run(w.add(&below_100("https://shop.example/p")));
    assert_eq!(w.snapshot()[0].via, "search");
}

#[test]
fn test_tinyfishs_own_trouble_never_moves_the_watch() {
    let dir = tmp();
    let rate_limited = Swap::new(|_, _| {
        Err(FetchError {
            status: 429,
            ..FetchError::with_host("TinyFish: the site answered HTTP 429", eve_web::tinyfish::FETCH_HOST)
        })
    });
    let w = hosted_make(dir.path(), rate_limited).build(&dir.path().join("w.json"));
    run(w.add(&below_100("https://shop.example/p")));
    let item = w.snapshot()[0].clone();
    assert!(item.via == "hosted" && item.errors == 1 && item.last_error.contains("429"), "{} {} {}", item.via, item.errors, item.last_error);
}

#[test]
fn test_the_daily_cap() {
    let dir = tmp();
    let crawled: Arc<Mutex<Vec<String>>> = Arc::default();
    let clock = TClock::new();
    let w = hosted_make(dir.path(), Swap::new(recording(&crawled)))
        .clock(&clock)
        .cfg(|c| {
            c.hosted = true;
            c.hosted_calls_per_day = 2;
        })
        .build(&dir.path().join("w.json"));
    run(w.add(&below_100("https://shop.example/p")));
    let item = w.watches()[0].clone();
    run(w.read(&item)).unwrap();
    let e = run(w.read(&item)).unwrap_err();
    assert!(e.reason.contains("TinyFish reads are used up"), "{}", e.reason);
    assert_eq!(e.host, eve_web::tinyfish::FETCH_HOST);
    assert_eq!(crawled.lock().len(), 2);
    clock.add(86400.0); // the next day
    run(w.read(&item)).unwrap();
    assert_eq!(crawled.lock().len(), 3);
}

#[test]
fn test_off_without_a_key() {
    let dir = tmp();
    let mut make = Make::new().fetch(refused(403)).cfg(|c| c.hosted = false);
    make.crawl = Some(Swap::new(|_, _| panic!("TinyFish is off")));
    let w = make.build(&dir.path().join("w.json"));
    run(w.add(&below_100("https://shop.example/p")));
    assert_eq!(w.snapshot()[0].via, "search");
    // the Watcher alone lends no hosted reader: hosted on in the config, none lent, and a refused page goes to search
    let w = Make::new().fetch(refused(403)).cfg(|c| c.hosted = true).build(&dir.path().join("x.json"));
    run(w.add(&below_100("https://shop.example/p")));
    assert_eq!(w.snapshot()[0].via, "search");
}

#[test]
fn test_a_hosted_page_moves_to_search_when_tinyfish_is_turned_off() {
    let dir = tmp();
    let path = dir.path().join("w.json");
    let w = hosted_make(dir.path(), Swap::new(recording(&Arc::default()))).build(&path);
    run(w.add(&below_100("https://shop.example/p")));
    assert_eq!(w.snapshot()[0].via, "hosted");
    // the key removed, the daemon restarted
    let searched: Arc<Mutex<Vec<Value>>> = Arc::default();
    let s = searched.clone();
    let mut later = Make::new().cfg(|c| c.hosted = false).ask(move |body| {
        s.lock().push(body.clone());
        Ok(reply(json!({"price": 80.0, "currency": "USD", "available": null, "note": ""})))
    });
    later.crawl = Some(Swap::new(|_, _| panic!("TinyFish is off")));
    let later = later.build(&path);
    let item = later.watches()[0].clone();
    let r = run(later.read(&item)).unwrap();
    assert_eq!(r.value, Some(80.0));
    let g = item.lock();
    assert!(g.via == "search" && g.every_secs >= watch::floor_secs("search"), "{} {}", g.via, g.every_secs);
    let asks = searched.lock();
    // one search: the model call that carries the web search tool
    assert!(asks.len() == 1 && asks[0]["tools"][0]["type"] == json!("openrouter:web_search"), "{asks:?}");
}

#[test]
fn test_the_via_survives_a_restart() {
    let dir = tmp();
    let path = dir.path().join("w.json");
    let w = hosted_make(dir.path(), Swap::new(recording(&Arc::default()))).build(&path);
    run(w.add(&below_100("https://shop.example/p")));
    let again = Make::new().cfg(|c| c.hosted = true).build(&path);
    let item = again.snapshot()[0].clone();
    assert!(item.via == "hosted" && item.every_secs >= watch::floor_secs("hosted"), "{} {}", item.via, item.every_secs);
}

#[test]
fn test_the_hosted_switches() {
    // test_watch_tinyfish.py's test_the_switches
    let ctx = |pairs: &[(&str, &str)]| {
        let env: Environ = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Ctx::global().clone().with_env(env)
    };
    assert!(!watch::configured(&ctx(&[])).hosted);
    let on = watch::configured(&ctx(&[("TINYFISH_API_KEY", "tf-x")]));
    assert!(on.hosted && on.hosted_calls_per_day == watch::DEFAULT_HOSTED_CALLS_PER_DAY);
    assert!(!watch::configured(&ctx(&[("TINYFISH_API_KEY", "tf-x"), ("EVE_WATCH_HOSTED", "0")])).hosted);
    assert_eq!(watch::configured(&ctx(&[("EVE_WATCH_HOSTED_CALLS", "5")])).hosted_calls_per_day, 5);
}

// ---- test_watch_shapes_replay.py and test_watch_security.py: the Watcher halves -----------------------------------

/// A JSON integer literal no float can hold (Python's `"1" + "0" * 400`).
static HUGE: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| format!("1{}", "0".repeat(400)));

fn no_price() -> impl Fn(&Value) -> Result<Value, FetchError> + Send + Sync + 'static {
    |_| Ok(json!({"choices": [{"message": {"content": "{\"price\": null, \"available\": null, \"note\": \"\"}"}}]}))
}

#[test]
fn test_one_page_with_a_huge_price_does_not_starve_the_other_watches() {
    assert_eq!(HUGE.len(), 401);
    let dir = tmp();
    let clock = TClock::new();
    let page = format!(
        r#"<script type="application/ld+json">{{"@type":"Product","name":"X","offers":{{"price":{}}}}}</script>"#,
        *HUGE
    );
    let fetched: Arc<Mutex<Vec<String>>> = Arc::default();
    let f = fetched.clone();
    let w = Make::new()
        .fetch(move |url, _| {
            f.lock().push(url.to_string());
            Ok((200, if url.contains("yahoo") { quote(250.0, "USD") } else { page.clone() }))
        })
        .ask(no_price())
        .clock(&clock)
        .build(&dir.path().join("w.json"));
    let mut x = mk("w1", "page", "https://shop.example/x", "X", "below");
    x.value = Some(100.0);
    x.every_secs = 1800;
    x.next_at = clock.t() - 10.0;
    let mut a = mk("w2", "quote", "AAPL", "Apple", "below");
    a.value = Some(300.0);
    a.every_secs = 900;
    a.next_at = clock.t() - 5.0;
    w.push(x);
    let apple = w.push(a);
    run(ticks_like_the_loop(&w, &clock, 4));
    assert!(apple.lock().checks >= 1, "the quote watch was never checked");
    let shop = fetched.lock().iter().filter(|u| u.contains("shop.example")).count();
    assert_eq!(shop, 1, "{:?}", fetched.lock());
}

#[test]
fn test_a_page_that_never_states_its_price_is_reported_not_silent() {
    let dir = tmp();
    let clock = TClock::new();
    let (told, notify) = sink();
    let page = "<html><body><main><h1>Widget</h1><p>Call for price</p></main></body></html>";
    let w = Make::new()
        .fetch(move |_, _| Ok((200, page.to_string())))
        .ask(no_price())
        .clock(&clock)
        .notify(notify)
        .cfg(|c| c.browser = false)
        .build(&dir.path().join("w.json"));
    let mut widget = mk("w1", "page", "https://shop.example/x", "Widget", "below");
    widget.value = Some(50.0);
    widget.every_secs = 300;
    widget.next_at = clock.t();
    let item = w.push(widget);
    for _ in 0..(watch::TELL_AFTER_ERRORS + 4) {
        run(w.tick());
        let next = item.lock().next_at;
        clock.set((clock.t() + 60.0).max(next));
    }
    assert!(item.lock().errors >= watch::TELL_AFTER_ERRORS, "errors {}", item.lock().errors);
    assert!(told.lock().iter().any(|t| t.starts_with("I can't read Widget right now")), "{:?}", told.lock());
}

#[test]
fn test_money_never_prints_scientific_notation() {
    assert_eq!(watch::money(Some(0.00001234), "USD"), "$0.00001234");
    assert_eq!(watch::money(Some(0.5), "USD"), "$0.5");
    assert_eq!(watch::money(Some(12.5), "USD"), "$12.50");
    let mut w = mk("w1", "quote", "SHIB-USD", "Shiba", "below");
    w.value = Some(0.00001);
    w.currency = "USD".to_string();
    assert!(!watch::describe(&w).contains("e-"), "{}", watch::describe(&w));
}

#[test]
fn test_the_huge_price_in_a_model_answer_is_dropped_not_raised() {
    // test_a_huge_integer_price_is_dropped_not_raised's model_reading half (the structured half is in watch_pages)
    let payload = json!({"choices": [{"message": {"content": format!("{{\"price\": {}, \"available\": true}}", *HUGE)}}]});
    assert_eq!(watch::model_reading(&payload, "model").unwrap().value, None);
}

#[test]
fn test_one_watch_whose_read_crashes_never_starves_the_others() {
    let dir = tmp();
    let clock = TClock::new();
    // Python's fetch raises LookupError (no FetchError); the Rust reader's equivalent is a panic
    let w = Make::new()
        .fetch(|url, _| {
            if url.contains("bad.example") {
                panic!("unknown encoding: bogus-8");
            }
            Ok((200, r#"<meta property="product:price:amount" content="50">"#.to_string()))
        })
        .clock(&clock)
        .cfg(|c| {
            c.browser = false;
            c.host_gap_secs = 0.0;
        })
        .build(&dir.path().join("w.json"));
    let mut a = mk("w1", "page", "https://bad.example/", "A", "below");
    a.value = Some(5.0);
    a.every_secs = 300;
    a.next_at = clock.t() - 10.0;
    let mut b = mk("w2", "page", "https://good.example/", "B", "below");
    b.value = Some(5.0);
    b.every_secs = 300;
    b.next_at = clock.t() - 5.0;
    let a = w.push(a);
    let b = w.push(b);
    run(ticks_like_the_loop(&w, &clock, 10));
    assert!(b.lock().checks >= 1, "the healthy watch was never checked");
    let (errors, next_at) = {
        let g = a.lock();
        (g.errors, g.next_at)
    };
    assert!(errors >= 1 && next_at > clock.t() - 600.0, "the broken watch was never counted or rescheduled");
}

#[test]
fn test_a_page_alert_links_only_the_page_the_owner_gave() {
    let dir = tmp();
    let (sent, notify) = sink();
    let answers = Arc::new(Mutex::new(VecDeque::from(vec![
        reply(json!({"available": false, "note": "Sold out."})),
        reply(json!({"available": true, "note": "Back.", "url": "https://kettle-restock.example/login"})),
    ])));
    let a = answers.clone();
    let clock = TClock::new();
    let w = Make::new()
        .fetch(|_, _| Ok((200, "<p>Kettle</p>".to_string())))
        .ask(move |_| Ok(a.lock().pop_front().expect("only two answers")))
        .notify(notify)
        .clock(&clock)
        .cfg(|c| c.browser = false)
        .build(&dir.path().join("w.json"));
    let out = run(w.add(&obj(json!({"kind": "page", "target": "https://shop.example/kettle", "label": "Blue Kettle",
                                     "condition": "available", "value": null, "text": null, "city": null,
                                     "every_minutes": null}))));
    assert_eq!(out["ok"], json!(true), "{out}");
    {
        // the page changed: a fresh model reading, due now
        let wt = w.watches()[0].clone();
        let mut g = wt.lock();
        g.digest.clear();
        g.next_at = 0.0;
    }
    w.limiter.lock().clear("shop.example");
    run(w.tick());
    let sent = sent.lock();
    assert!(!sent.is_empty(), "the second reading should have fired");
    let last = sent.last().unwrap();
    assert!(!last.contains("kettle-restock.example"), "{last}");
    assert!(last.trim_end().ends_with("https://shop.example/kettle"), "{last}");
}

#[test]
fn test_a_search_note_cannot_hide_a_link_behind_words() {
    let mut wt = mk("w1", "search", "tickets for X in Seattle", "X in Seattle", "available");
    wt.checks = 1;
    wt.armed = true;
    let reading = watch::model_reading(
        &reply(json!({"available": true, "note": "[Presale code: claim here](https://tix-claim.example/sso)"})),
        "search",
    )
    .unwrap();
    let alert = watch::evaluate(&mut wt, &reading, true).expect("an alert");
    let composed = eve_telegram_format::compose(&alert, Some("Watching"), None);
    assert!(!composed.contains("<a href"), "{composed}");
}

// ---- test_watch_reverify.py: the render child dies with its parent ------------------------------------------------

/// render_watchdog ends the process (`_exit(3)`), so it runs in a child: this test binary again, running only this
/// test with EVE_WEB_WATCHDOG_CHILD set. The child starts a grandchild (`sleep 60`), prints its pid, and runs the
/// watchdog with a parent pid that is not its parent (Python's `parent=-12345`: the parent is gone). The watchdog
/// must kill everything below the child and exit 3.
#[test]
fn test_the_render_child_dies_with_its_parent() {
    if std::env::var_os("EVE_WEB_WATCHDOG_CHILD").is_some() {
        // never waited on: render_watchdog kills it and ends this process, as the test means it to
        #[allow(clippy::zombie_processes)]
        let grandchild = std::process::Command::new("sleep").arg("60").spawn().expect("sleep");
        println!("GRANDCHILD {}", grandchild.id());
        std::io::Write::flush(&mut std::io::stdout()).ok();
        let deadline = Instant::now() + Duration::from_secs(3600);
        watch::render::render_watchdog(u32::MAX - 12344, deadline, 0.01);
        unreachable!("render_watchdog returned");
    }
    let me = std::env::current_exe().expect("the test binary");
    let out = std::process::Command::new(me)
        .args(["--exact", "test_the_render_child_dies_with_its_parent", "--nocapture", "--test-threads=1"])
        .env("EVE_WEB_WATCHDOG_CHILD", "1")
        .output()
        .expect("the child ran");
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    let pid: i32 = text
        .lines()
        .find_map(|l| l.split("GRANDCHILD ").nth(1))
        .and_then(|p| p.split_whitespace().next()?.parse().ok())
        .unwrap_or_else(|| panic!("no grandchild pid in {text}"));
    // the grandchild was killed (it is the child's orphan now; launchd reaps it)
    let gone = (0..200).any(|_| {
        // SAFETY: signal 0 only asks whether the process exists.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        if alive {
            std::thread::sleep(Duration::from_millis(10));
        }
        !alive
    });
    assert!(gone, "the render child's own child outlived it");
}
