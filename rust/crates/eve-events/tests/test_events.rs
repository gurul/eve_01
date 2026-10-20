//! test_events.py: Ticketmaster's Discovery search, with no network (the fetch is a fake). Plus the events part of
//! test_find_image.py.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use chrono_tz::America::Los_Angeles as PACIFIC; // a real zone: the query follows its daylight saving
use eve_events::{self as events, Events, EventsOptions, Limiter, OwnerNow};
use eve_web::watch::{self, FetchError, LimiterOptions, RateLimiter};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

const KEY_VALUE: &str = "tm-test-key-value";

fn key() -> HashMap<String, String> {
    HashMap::from([("TICKETMASTER_API_KEY".to_string(), KEY_VALUE.to_string())])
}

/// a Thursday evening
fn now() -> OwnerNow {
    OwnerNow::named(PACIFIC.with_ymd_and_hms(2026, 10, 8, 18, 30, 0).unwrap())
}

fn event(name: &str, price: Option<Value>, genre: &str) -> Value {
    let mut e = json!({
        "name": name, "id": "x", "url": format!("https://www.ticketmaster.com/{}/event/0F00", name.to_lowercase()),
        "dates": {"start": {"localDate": "2026-10-10", "localTime": "19:30:00"}, "status": {"code": "onsale"}},
        "classifications": [{"segment": {"name": "Music"}, "genre": {"name": genre}}],
        "_embedded": {"venues": [{"name": "Neptune Theatre", "city": {"name": "Seattle"},
                                  "state": {"stateCode": "WA"}}]},
        "images": [{"url": format!("https://img.invalid/{}", "x".repeat(500))}], "sales": {"public": {}},
        "info": "y".repeat(900),
    });
    if let Some(p) = price {
        e["priceRanges"] = json!([p]);
    }
    e
}

fn ev(name: &str) -> Value {
    event(name, None, "Rock")
}

fn payload(evs: Vec<Value>, total: i64) -> String {
    json!({"_embedded": {"events": evs}, "page": {"totalElements": total}}).to_string()
}

type MakeError = Arc<dyn Fn() -> anyhow::Error + Send + Sync>;

#[derive(Clone)]
struct Fetch {
    body: String,
    error: Option<MakeError>,
    delay: f64,
    urls: Arc<Mutex<Vec<String>>>,
}

impl Fetch {
    fn new(body: String) -> Self {
        Fetch { body, error: None, delay: 0.0, urls: Arc::default() }
    }

    fn failing(error: MakeError) -> Self {
        Fetch { error: Some(error), ..Fetch::new(String::new()) }
    }

    fn urls(&self) -> Vec<String> {
        self.urls.lock().clone()
    }

    fn query(&self) -> HashMap<String, String> {
        let last = self.urls().last().cloned().unwrap();
        let q = last.split_once('?').unwrap().1.to_string();
        q.split('&')
            .map(|kv| {
                let (k, v) = kv.split_once('=').unwrap();
                (unquote(k), unquote(v))
            })
            .collect()
    }

    fn as_fetch(&self) -> events::Fetch {
        let me = self.clone();
        Arc::new(move |url: &str, _headers: &[(String, String)]| {
            me.urls.lock().push(url.to_string());
            if me.delay > 0.0 {
                std::thread::sleep(Duration::from_secs_f64(me.delay));
            }
            if let Some(e) = &me.error {
                return Err(e());
            }
            Ok((200, me.body.clone()))
        })
    }
}

/// `urllib.parse.unquote_plus`.
fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' => {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8(out).unwrap()
}

struct Finder {
    fetch: Option<events::Fetch>,
    environ: HashMap<String, String>,
    limiter: Option<events::LimiterFn>,
    timeout_s: f64,
    r#where: events::WhereFn,
}

impl Finder {
    fn new(fetch: &Fetch) -> Self {
        // never the real phone's fix
        Finder { fetch: Some(fetch.as_fetch()), environ: key(), limiter: None, timeout_s: events::TIMEOUT_S,
                 r#where: Arc::new(|| Ok(None)) }
    }

    fn environ(mut self, env: HashMap<String, String>) -> Self {
        self.environ = env;
        self
    }

    fn limiter(mut self, lim: Arc<dyn Limiter>) -> Self {
        self.limiter = Some(Arc::new(move || Ok(Some(lim.clone()))));
        self
    }

    fn timeout(mut self, t: f64) -> Self {
        self.timeout_s = t;
        self
    }

    fn at(mut self, here: (f64, f64)) -> Self {
        self.r#where = Arc::new(move || Ok(Some(here)));
        self
    }

    fn build(self) -> Events {
        Events::new(EventsOptions {
            fetch: self.fetch,
            limiter: self.limiter,
            now: Some(Arc::new(now)),
            environ: Some(self.environ),
            timeout_s: self.timeout_s,
            offload: true,
            r#where: Some(self.r#where),
        })
    }
}

fn args(over: Value) -> Map<String, Value> {
    let mut base = json!({"city": "", "near": false, "keyword": "", "category": "", "start_date": "", "end_date": ""});
    for (k, v) in over.as_object().unwrap() {
        base[k] = v.clone();
    }
    base.as_object().unwrap().clone()
}

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

#[tokio::test]
async fn test_a_weekend_in_a_city_becomes_the_discovery_query_in_utc() {
    let fetch = Fetch::new(payload(vec![ev("Band")], 42));
    let out = Finder::new(&fetch)
        .build()
        .search(&args(json!({"city": "Seattle, wa", "category": "music", "keyword": "indie rock",
                             "start_date": "2026-10-10", "end_date": "2026-10-11"})))
        .await;
    assert!(out["ok"] == json!(true) && out["events"].as_array().unwrap().len() == 1);
    let q = fetch.query();
    assert!(fetch.urls().last().unwrap().starts_with(&format!("{}/events.json?", watch::TM_API)));
    assert!(q["city"] == "Seattle" && q["stateCode"] == "WA" && q["classificationName"] == "music");
    assert!(q["keyword"] == "indie rock" && q["sort"] == "date,asc" && q["size"] == events::MAX_EVENTS.to_string());
    // Saturday 00:00 to Sunday 23:59:59 Pacific (UTC-7), in UTC with a Z and no fraction
    assert!(q["startDateTime"] == "2026-10-10T07:00:00Z" && q["endDateTime"] == "2026-10-12T06:59:59Z");
    // the key goes first, as urlencode({"apikey": key, **q}) puts it
    assert!(fetch.urls()[0].contains(&format!("events.json?apikey={KEY_VALUE}&size=8&sort=date%2Casc&")));
}

#[test]
fn test_today_starts_now_and_no_dates_means_from_now_with_no_end() {
    let q = events::query(&args(json!({"start_date": "2026-10-08"})), &now(), None).unwrap();
    assert!(q["startDateTime"] == "2026-10-09T01:30:00Z" && !q.contains_key("endDateTime"));
    let q = events::query(&args(json!({})), &now(), None).unwrap();
    assert!(q["startDateTime"] == "2026-10-09T01:30:00Z" && !q.contains_key("city") && !q.contains_key("classificationName"));
}

#[tokio::test]
async fn test_a_bad_date_is_a_reason_and_no_request() {
    for bad in [json!({"start_date": "next friday"}), json!({"start_date": "2026-10-12", "end_date": "2026-10-10"})] {
        let fetch = Fetch::new(payload(vec![], 42));
        let out = Finder::new(&fetch).build().search(&args(bad)).await;
        assert!(out["ok"] == json!(false) && out["reason"].as_str().unwrap().contains("YYYY-MM-DD") && fetch.urls().is_empty());
    }
}

#[test]
fn test_each_event_is_trimmed_to_what_a_reply_needs() {
    let body = payload(
        vec![event("Band", Some(json!({"currency": "usd", "min": 35, "max": 89.5})), "Rock"), event("Other", None, "Undefined")],
        566,
    );
    let out = events::parse(&serde_json::from_str(&body).unwrap());
    assert_eq!(out["total"], json!(566));
    let evs = out["events"].as_array().unwrap();
    let (first, second) = (&evs[0], &evs[1]);
    assert_eq!(*first, json!({"name": "Band", "date": "2026-10-10", "time": "19:30", "venue": "Neptune Theatre",
                              "city": "Seattle, WA", "status": "onsale", "genre": "Rock",
                              "url": "https://www.ticketmaster.com/band/event/0F00", "price": "USD 35-89.50"}));
    // unknown, never free; "Undefined" is no genre
    assert!(second.get("price").is_none() && second["genre"] == json!(""));
    // the raw event is kilobytes (json.dumps puts a space after each : and ,)
    assert!(eve_places::py::dumps(first).len() < 400);
}

#[test]
fn test_at_most_max_events_and_none_found_says_so() {
    let out = events::parse(&serde_json::from_str(&payload((0..12).map(|i| ev(&format!("B{i}"))).collect(), 42)).unwrap());
    assert_eq!(out["events"].as_array().unwrap().len(), events::MAX_EVENTS);
    let empty = events::parse(&json!({"page": {"totalElements": 0}}));
    assert!(empty["ok"] == json!(true) && empty["events"] == json!([]) && empty["note"].as_str().unwrap().contains("no events"));
}

#[tokio::test]
async fn test_no_key_is_a_clear_reason_and_no_tool() {
    let fetch = Fetch::new(payload(vec![], 42));
    let finder = Finder::new(&fetch).environ(HashMap::new()).build();
    assert!(finder.tools().is_empty() && !finder.enabled());
    let out = finder.search(&args(json!({"city": "Seattle"}))).await;
    assert!(out == json!({"ok": false, "reason": events::OFF_REASON}) && fetch.urls().is_empty());
    assert_eq!(Finder::new(&fetch).build().tools(), vec![events::TOOL.clone()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_a_slow_answer_times_out_quickly_and_never_raises() {
    let mut fetch = Fetch::new(payload(vec![], 42));
    fetch.delay = 1.0;
    let t0 = Instant::now();
    let out = Finder::new(&fetch).timeout(0.1).build().search(&args(json!({"city": "Seattle"}))).await;
    let waited = t0.elapsed().as_secs_f64(); // the turn's wait (the worker thread finishes on its own)
    assert_eq!(out, json!({"ok": false, "reason": "Ticketmaster did not answer in time"}));
    assert!(waited < 0.9);
}

#[tokio::test]
async fn test_failures_are_reasons_by_status_and_never_carry_the_key() {
    let cases: Vec<(MakeError, &str)> = vec![
        (Arc::new(|| FetchError::with_status("the site answered HTTP 401", 401).into()), "refused the key"),
        (Arc::new(|| FetchError::with_status("the site answered HTTP 429", 429).into()), "slow down"),
        (Arc::new(|| FetchError::new("the site took too long to answer").into()), "did not answer in time"),
        (Arc::new(|| FetchError::new("the site could not be reached").into()), "could not be reached"),
        (Arc::new(|| anyhow::anyhow!("boom {KEY_VALUE}")), "could not be reached"),
    ];
    for (error, reason) in cases {
        let out = Finder::new(&Fetch::failing(error)).build().search(&args(json!({"city": "Seattle"}))).await;
        assert!(out["ok"] == json!(false) && out["reason"].as_str().unwrap().contains(reason), "{out}");
        assert!(!out.to_string().contains(KEY_VALUE));
    }
}

#[tokio::test]
async fn test_an_unreadable_answer_is_a_reason() {
    let out = Finder::new(&Fetch::new("<html>nope</html>".into())).build().search(&args(json!({}))).await;
    assert_eq!(out, json!({"ok": false, "reason": "Ticketmaster sent an unreadable answer"}));
    // a JSON answer that is not an object is unreadable too
    let out = Finder::new(&Fetch::new("[1, 2]".into())).build().search(&args(json!({}))).await;
    assert_eq!(out, json!({"ok": false, "reason": "Ticketmaster sent an unreadable answer"}));
}

#[tokio::test]
async fn test_a_search_is_counted_by_the_watchers_limiter() {
    struct Taker(Mutex<Vec<String>>);
    impl Limiter for Taker {
        // only take: the others raise AttributeError in Python, which each call site swallows
        fn backed_off(&self, _: &str) -> anyhow::Result<f64> {
            anyhow::bail!("no backed_off")
        }
        fn take(&self, host: &str) -> anyhow::Result<()> {
            self.0.lock().push(host.to_string());
            Ok(())
        }
        fn penalize(&self, _: &str, _: Option<f64>) -> anyhow::Result<f64> {
            anyhow::bail!("no penalize")
        }
        fn clear(&self, _: &str) -> anyhow::Result<()> {
            anyhow::bail!("no clear")
        }
    }
    let lim = Arc::new(Taker(Mutex::new(Vec::new())));
    Finder::new(&Fetch::new(payload(vec![], 42))).limiter(lim.clone()).build().search(&args(json!({}))).await;
    assert_eq!(*lim.0.lock(), vec![watch::TM_HOST.to_string()]);
}

#[test]
fn test_the_tool_is_strict_and_sits_in_the_always_carried_events_toolset() {
    let params = &events::TOOL["parameters"];
    assert_eq!(events::TOOL["strict"], json!(true));
    let required: std::collections::BTreeSet<&str> =
        params["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    let props: std::collections::BTreeSet<&str> = params["properties"].as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(required, props);
    // (toolset_router's ALWAYS[EVENTS] is checked in eve-toolset-router's tests)
    assert_eq!(events::TOOL_NAMES, ["events_search"]);
    // its line names watch_add only when the watcher offers the ticketmaster kind
    assert!(!events::instructions(false).contains("watch_add") && events::instructions(true).contains("watch_add"));
}

#[test]
fn test_the_tool_is_pythons_json_byte_for_byte() {
    // json.dumps(events.TOOL), printed by the Python module (2026-10-20)
    let want = r#"{"type": "function", "name": "events_search", "strict": true, "description": "Find concerts, sports games, theatre, comedy and other ticketed events on Ticketmaster by place, dates, kind or name. Returns up to 8 upcoming events with venue, local date and time, price range when listed, and the ticket link.", "parameters": {"type": "object", "additionalProperties": false, "required": ["city", "near", "keyword", "category", "start_date", "end_date"], "properties": {"city": {"type": "string", "description": "The city, optionally with its state code (\"Seattle\", \"Portland, OR\"); empty for near the owner or anywhere."}, "near": {"type": "boolean", "description": "True when the owner means near where they are now (near me, nearby, around here, in town) and names no city. A search with no city and no keyword is always near them."}, "keyword": {"type": "string", "description": "An artist, team, show, venue or genre the owner named; empty for any."}, "category": {"type": "string", "enum": ["music", "sports", "arts & theatre", "film", "family", ""], "description": "The kind of event, or empty for every kind."}, "start_date": {"type": "string", "description": "First day, YYYY-MM-DD in the owner's time zone; empty for from now."}, "end_date": {"type": "string", "description": "Last day, YYYY-MM-DD in the owner's time zone; empty for no end."}}}}"#;
    assert_eq!(eve_places::py::dumps(&events::TOOL), want);
    let line = "Concerts, games, shows and other ticketed events are events_search: what is on in a place or on dates (\"anything on in Seattle this weekend\"), when a team plays next, whether an artist is touring or coming to a city, and tickets for any of them. A team's schedule (its next game, its next home game) and an artist's tour dates are here too: the team or artist goes in keyword, and the city stays empty unless the owner names one (for a home game, the team's own city). It reads Ticketmaster directly and answers in about a second. Work out the dates from today's date in your note.";
    assert_eq!(events::instructions(false), line);
    assert_eq!(events::instructions(true), format!("{line} To be told when tickets for one go on sale, that is watch_add with kind ticketmaster."));
}

// ---- day boundaries across daylight saving (review, 2026-10-08) -------------------------------------------------

#[test]
fn test_a_day_after_the_clocks_change_ends_at_that_day_s_own_midnight() {
    // 2026-11-01 is the end of daylight saving in Los Angeles: Monday 2026-11-02 ends at Tuesday 00:00 PST (UTC-8)
    let now = OwnerNow::named(PACIFIC.with_ymd_and_hms(2026, 10, 30, 12, 0, 0).unwrap());
    let q = events::query(&args(json!({"start_date": "2026-11-02", "end_date": "2026-11-02"})), &now, None).unwrap();
    assert!(q["startDateTime"] == "2026-11-02T08:00:00Z" && q["endDateTime"] == "2026-11-03T07:59:59Z");
}

/// datetime.now().astimezone() carries today's fixed offset (PDT, -7); a day after the change is PST all the same.
/// Was 06:59:59Z (an hour off) before the fix. The Mac's zone is lent here (the daemon lends `Local`), since tests
/// must not set TZ; `fixed_offset_child` below checks the same through `Local` itself, in a child with TZ set.
#[test]
fn test_a_fixed_offset_now_reads_each_day_in_the_mac_s_own_zone() {
    let now = OwnerNow::fixed(PACIFIC.with_ymd_and_hms(2026, 10, 30, 12, 0, 0).unwrap().fixed_offset());
    assert!(matches!(now.zone, events::Zone::Fixed(o) if o.local_minus_utc() == -7 * 3600));
    let q = events::query_in(&args(json!({"start_date": "2026-11-02", "end_date": "2026-11-02"})), &now, None, &PACIFIC)
        .unwrap();
    assert!(q["startDateTime"] == "2026-11-02T08:00:00Z" && q["endDateTime"] == "2026-11-03T07:59:59Z");

    // and through chrono's Local, in a child process of this test binary with TZ set
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "fixed_offset_child", "--ignored", "--nocapture"])
        .env("TZ", "America/Los_Angeles")
        .env("EVE_EVENTS_TZ_CHILD", "1")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success() && text.contains("1 passed") && text.contains("child checked"), "{text}");
}

#[test]
#[ignore = "run by test_a_fixed_offset_now_reads_each_day_in_the_mac_s_own_zone with TZ set"]
fn fixed_offset_child() {
    if std::env::var("EVE_EVENTS_TZ_CHILD").as_deref() != Ok("1") {
        return;
    }
    let local = chrono::Local.with_ymd_and_hms(2026, 10, 30, 12, 0, 0).unwrap();
    let now = OwnerNow::fixed(local.fixed_offset());
    assert!(matches!(now.zone, events::Zone::Fixed(o) if o.local_minus_utc() == -7 * 3600));
    let q = events::query(&args(json!({"start_date": "2026-11-02", "end_date": "2026-11-02"})), &now, None).unwrap();
    assert!(q["startDateTime"] == "2026-11-02T08:00:00Z" && q["endDateTime"] == "2026-11-03T07:59:59Z");
    println!("child checked");
}

// ---- the watcher's limiter, honoured as the watches honour it ---------------------------------------------------

#[derive(Default)]
struct FakeLimiter {
    held: f64,
    wait: f64,
    taken: Mutex<Vec<String>>,
    penalized: Mutex<Vec<(String, Option<f64>)>>,
    cleared: Mutex<Vec<String>>,
}

impl FakeLimiter {
    #[allow(dead_code)]
    fn ready_in(&self, _host: &str) -> f64 {
        self.wait
    }
}

impl Limiter for FakeLimiter {
    fn backed_off(&self, _host: &str) -> anyhow::Result<f64> {
        Ok(self.held)
    }
    fn take(&self, host: &str) -> anyhow::Result<()> {
        self.taken.lock().push(host.to_string());
        Ok(())
    }
    fn penalize(&self, host: &str, retry_after: Option<f64>) -> anyhow::Result<f64> {
        self.penalized.lock().push((host.to_string(), retry_after));
        Ok(30.0)
    }
    fn clear(&self, host: &str) -> anyhow::Result<()> {
        self.cleared.lock().push(host.to_string());
        Ok(())
    }
}

#[tokio::test]
async fn test_a_back_off_ticketmaster_asked_for_is_kept_with_no_request() {
    let fetch = Fetch::new(payload(vec![], 42));
    let lim = Arc::new(FakeLimiter { held: 42.0, ..Default::default() });
    let out = Finder::new(&fetch).limiter(lim.clone()).build().search(&args(json!({}))).await;
    assert_eq!(out, json!({"ok": false, "reason": "Ticketmaster asked to slow down; try again in 42 s"}));
    assert!(fetch.urls().is_empty() && lim.taken.lock().is_empty());
}

/// 2026-10-18: a retry three seconds after a first search was refused by the watches' 20 s host gap.
#[tokio::test]
async fn test_the_watches_pacing_never_refuses_the_owner_s_search() {
    let fetch = Fetch::new(payload(vec![], 42));
    let lim = Arc::new(FakeLimiter { wait: 17.0, ..Default::default() });
    let started = Instant::now();
    let out = Finder::new(&fetch).limiter(lim.clone()).build().search(&args(json!({}))).await;
    assert!(out["ok"] == json!(true) && fetch.urls().len() == 1 && started.elapsed().as_secs_f64() < 1.0);
    assert!(*lim.taken.lock() == vec![watch::TM_HOST] && *lim.cleared.lock() == vec![watch::TM_HOST]);
}

#[tokio::test]
async fn test_the_real_limiter_lets_a_second_search_through_at_once() {
    let lim = Arc::new(Mutex::new(RateLimiter::new(LimiterOptions::default())));
    let fetch = Fetch::new(payload(vec![ev("A")], 42));
    let finder = Events::new(EventsOptions { limiter: Some(events::limiter_of(lim)), ..options(&fetch) });
    assert_eq!(finder.search(&args(json!({"city": "Seattle"}))).await["ok"], json!(true));
    assert!(finder.search(&args(json!({"city": "Seattle"}))).await["ok"] == json!(true) && fetch.urls().len() == 2);
}

fn options(fetch: &Fetch) -> EventsOptions {
    EventsOptions {
        fetch: Some(fetch.as_fetch()),
        now: Some(Arc::new(now)),
        environ: Some(key()),
        r#where: Some(Arc::new(|| Ok(None))),
        ..EventsOptions::default()
    }
}

#[tokio::test]
async fn test_a_429_backs_the_shared_key_off_for_the_watches_too() {
    let lim = Arc::new(FakeLimiter::default());
    let error: MakeError = Arc::new(|| FetchError::with_retry("the site answered HTTP 429", 429, Some(20.0)).into());
    let out = Finder::new(&Fetch::failing(error)).limiter(lim.clone()).build().search(&args(json!({}))).await;
    assert!(out["reason"].as_str().unwrap().contains("slow down"));
    assert!(*lim.penalized.lock() == vec![(watch::TM_HOST.to_string(), Some(20.0))] && lim.cleared.lock().is_empty());
}

#[tokio::test]
async fn test_a_500_backs_off_only_when_it_said_how_long() {
    for (retry, penalized) in [(None, false), (Some(0.0), false), (Some(5.0), true)] {
        let lim = Arc::new(FakeLimiter::default());
        let error: MakeError = Arc::new(move || FetchError::with_retry("the site answered HTTP 502", 502, retry).into());
        let out = Finder::new(&Fetch::failing(error)).limiter(lim.clone()).build().search(&args(json!({}))).await;
        assert_eq!(out, json!({"ok": false, "reason": "Ticketmaster answered HTTP 502"}));
        assert_eq!(!lim.penalized.lock().is_empty(), penalized);
    }
}

// ---- near the owner (2026-10-18: "Concerts near me" searched everywhere) ----------------------------------------

const HERE: (f64, f64) = (41.88, -87.63);

#[tokio::test]
async fn test_near_me_searches_around_the_owner() {
    for over in [json!({"category": "music"}), json!({"near": true, "keyword": "jazz"})] {
        let fetch = Fetch::new(payload(vec![ev("A")], 42));
        let out = Finder::new(&fetch).at(HERE).build().search(&args(over)).await;
        let q = fetch.query();
        assert!(q["latlong"] == "41.88,-87.63" && q["radius"] == events::NEAR_RADIUS_MI.to_string() && q["unit"] == "miles");
        assert!(!q.contains_key("city") && out["ok"] == json!(true) && out["near"].as_str().unwrap().contains("miles"));
    }
}

#[tokio::test]
async fn test_a_named_city_or_an_artist_s_tour_is_not_near() {
    for over in [json!({"city": "Portland, OR", "near": true}), json!({"keyword": "Mariners"})] {
        let asked = Arc::new(Mutex::new(Vec::<i32>::new()));
        let fetch = Fetch::new(payload(vec![ev("A")], 42));
        let mut finder = Finder::new(&fetch);
        let a = asked.clone();
        finder.r#where = Arc::new(move || {
            a.lock().push(1);
            Ok(Some(HERE))
        });
        let out = finder.build().search(&args(over)).await;
        assert!(!fetch.query().contains_key("latlong") && out.get("near").is_none() && asked.lock().is_empty());
    }
}

#[tokio::test]
async fn test_near_me_with_no_known_place_searches_everywhere_and_says_so() {
    let fetch = Fetch::new(payload(vec![ev("A")], 42));
    let out = Finder::new(&fetch).build().search(&args(json!({"near": true}))).await;
    assert!(!fetch.query().contains_key("latlong") && out["note"] == json!(events::NO_HERE));
}

#[test]
fn test_here_is_the_phone_s_fresh_fix_else_the_weather_place_rounded() {
    let tmp = tempfile::tempdir().unwrap();
    let loc = tmp.path().join("loc.json");
    let env: HashMap<String, String> = HashMap::from([
        ("EVE_PHONE_LOCATION_FILE".into(), loc.to_string_lossy().into_owned()),
        ("EVE_WEATHER_LAT".into(), "40.71277".into()),
        ("EVE_WEATHER_LON".into(), "-74.00597".into()),
    ]);
    assert_eq!(events::here(Some(&env)), Some((40.71, -74.01)));
    let fix = eve_places::phone_location::parse(
        &json!({"lat": 41.878114, "lon": -87.629798, "accuracy_m": 10, "at": Utc::now().to_rfc3339(), "place": null,
                "speed_mps": null}),
        None,
    )
    .unwrap();
    assert!(eve_places::phone_location::store(&fix, Some(&loc)).unwrap());
    assert_eq!(events::here(Some(&env)), Some((41.88, -87.63)));
    let none = HashMap::from([("EVE_PHONE_LOCATION_FILE".to_string(), tmp.path().join("none.json").to_string_lossy().into_owned())]);
    assert_eq!(events::here(Some(&none)), None);
    let mut off = env.clone();
    off.insert("EVE_PHONE_LOCATION".into(), "0".into());
    off.insert("EVE_WEATHER_LAT".into(), "x".into());
    assert_eq!(events::here(Some(&off)), None);
}

#[tokio::test]
async fn test_handle_answers_only_its_own_tool() {
    let fetch = Fetch::new(payload(vec![ev("A")], 42));
    let finder = Finder::new(&fetch).build();
    assert_eq!(finder.handle("watch_add", &args(json!({}))).await, json!({"ok": false, "reason": "not an events tool"}));
    assert_eq!(finder.handle(events::TOOL_NAME, &args(json!({"city": "Seattle"}))).await["ok"], json!(true));
}

// ---- test_find_image.py ------------------------------------------------------------------------------------------

#[test]
fn test_an_event_keeps_ticketmasters_widest_16_9_picture() {
    let imgs = json!([{"url": "https://s1.ticketm.net/a_3_2.jpg", "ratio": "3_2", "width": 3000},
                      {"url": "https://s1.ticketm.net/a_16_9_S.jpg", "ratio": "16_9", "width": 640},
                      {"url": "https://s1.ticketm.net/a_16_9_L.jpg", "ratio": "16_9", "width": 2048},
                      {"url": "https://s1.ticketm.net/fallback.jpg", "ratio": "16_9", "width": 4000, "fallback": true}]);
    assert_eq!(events::trim(&obj(json!({"name": "Show", "images": imgs})))["image"], json!("https://s1.ticketm.net/a_16_9_L.jpg"));
    assert!(events::trim(&obj(json!({"name": "Show", "images": [imgs[0].clone()]}))).get("image").is_none());
    // of equal widths the first is kept, as max() keeps it
    let same = json!([{"url": "https://a/1.jpg", "ratio": "16_9", "width": 1024},
                      {"url": "https://a/2.jpg", "ratio": "16_9", "width": "1024"}]);
    assert_eq!(events::best_image(Some(&same)), "https://a/1.jpg");
}
