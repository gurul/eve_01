//! The watch-rules unit's tests: test_watch_lean_conditions.py, test_watch_lean_limiter.py and
//! test_watch_lean_ticketmaster.py, plus the pure-function cases of test_watch.py that cover this unit (the
//! limiter, conditions, the store's defensive load, quotes, Ticketmaster's rules, config) and the rest of its API.
//!
//! Tests that drive a whole `Watcher` (its add, tick and read) are here as Python wrote them.

use std::sync::Arc;

use eve_web::watch::config::{expand_user, number};
use eve_web::watch::limiter::{local_day, LimiterOptions, RateLimiter};
use eve_web::watch::model::{model_options, model_reading, openrouter};
use eve_web::watch::rules::{alert_about, every_words, hours, stretch, when};
use eve_web::watch::ticketmaster::{iso, norm, sports, tm_events, tribute, tribute_words};
use eve_web::watch::{
    configured, describe, evaluate, money, now_line, pick_attraction, plain_note, quote_reading, ticketmaster_reading,
    FetchError, HttpOptions, Reading, Watch, WatchConfig, DEFAULT_MODEL, DEFAULT_RATE_PER_MIN, FLOOR_SECS, OPENROUTER_HOST,
    OPENROUTER_URL,
};
use eve_web::{Clock, Ctx, Environ};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

// ---- helpers ------------------------------------------------------------------------------------------------

/// The tests' clock: a time the test moves.
#[derive(Clone)]
struct TestClock(Arc<Mutex<f64>>);

impl TestClock {
    fn new(t: f64) -> Self {
        TestClock(Arc::new(Mutex::new(t)))
    }
    fn t(&self) -> f64 {
        *self.0.lock()
    }
    fn set(&self, t: f64) {
        *self.0.lock() = t;
    }
    fn add(&self, dt: f64) {
        *self.0.lock() += dt;
    }
    fn clock(&self) -> Clock {
        let inner = self.0.clone();
        Arc::new(move || *inner.lock())
    }
}

const T0: f64 = 1_800_000_000.0;

fn day_number() -> eve_web::watch::limiter::DayFn {
    Arc::new(|t: f64| ((t / 86400.0).floor() as i64).to_string())
}

/// test_watch_lean_limiter.py's `lim`: 6/min, burst 2, 20 s gap, 60 s base, 3600 s cap, 2 model calls a day.
fn lim_opts(clock: &TestClock) -> LimiterOptions {
    LimiterOptions {
        rate_per_min: 6.0,
        burst: 2,
        host_gap_secs: 20.0,
        backoff_base: 60.0,
        backoff_max: 3600.0,
        model_calls_per_day: 2,
        clock: clock.clock(),
        day: day_number(),
    }
}

fn lim(clock: &TestClock) -> RateLimiter {
    RateLimiter::new(lim_opts(clock))
}

fn price(v: Option<f64>) -> Reading {
    Reading { value: v, ..Reading::default() }
}

fn run(w: &mut Watch, values: &[Option<f64>]) -> Vec<Option<String>> {
    values.iter().map(|v| evaluate(w, &price(*v), true)).collect()
}

fn run_readings(w: &mut Watch, readings: &[Reading]) -> Vec<Option<String>> {
    readings.iter().map(|r| evaluate(w, r, true)).collect()
}

fn fired(alerts: &[Option<String>]) -> Vec<bool> {
    alerts.iter().map(Option::is_some).collect()
}

fn pct_watch(condition: &str, pct: f64) -> Watch {
    let mut w = Watch::new("w1", "page", "https://shop.example/p", "Thing", condition);
    w.value = Some(pct);
    w
}

/// `Watch.from_json(json.loads(json.dumps(w.to_json())))`.
fn restart(w: &Watch) -> Watch {
    let text = serde_json::to_string(&w.to_json()).unwrap();
    Watch::from_json(&serde_json::from_str(&text).unwrap()).expect("a watch")
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => panic!("not an object"),
    }
}

fn ctx_env(pairs: &[(&str, &str)]) -> Ctx {
    let env: Environ = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    Ctx::global().clone().with_env(env)
}

/// test_watch.py's `_iso`: a UTC time as Ticketmaster writes it.
fn tm_iso(t: f64) -> String {
    chrono::DateTime::from_timestamp(t as i64, 0).unwrap().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// test_watch.py's `tm_event`.
fn tm_event(code: &str, public: Option<(f64, Option<f64>)>, presale: Option<(f64, f64)>, price: Option<f64>) -> Map<String, Value> {
    let mut sales = Map::new();
    if let Some((start, end)) = public {
        let mut p = Map::new();
        p.insert("startDateTime".into(), json!(tm_iso(start)));
        if let Some(end) = end {
            p.insert("endDateTime".into(), json!(tm_iso(end)));
        }
        sales.insert("public".into(), Value::Object(p));
    }
    if let Some((start, end)) = presale {
        sales.insert(
            "presales".into(),
            json!([{"name": "Fan presale", "startDateTime": tm_iso(start), "endDateTime": tm_iso(end)}]),
        );
    }
    let mut e = obj(json!({"id": "vvG1", "name": "Big Show", "url": "https://www.ticketmaster.com/show/event/0E0050681F51BA4C",
        "dates": {"status": {"code": code}, "start": {"localDate": "2026-12-01"}},
        "sales": Value::Object(sales), "_embedded": {"attractions": [{"id": "K8vZ"}]}}));
    if let Some(p) = price {
        e.insert("priceRanges".into(), json!([{"type": "standard", "currency": "USD", "min": p, "max": p * 3.0}]));
    }
    e
}

// ==== test_watch_lean_conditions.py ============================================================================

// ---- the bug: a baseline of 0 (Lean: current_violates, current_violates_after_a_drop_to_zero) ---------

#[test]
fn test_a_first_price_of_zero_does_not_raise_on_the_next_reading() {
    let mut w = pct_watch("rise_pct", 10.0);
    let alerts = run(&mut w, &[Some(0.0), Some(5.0)]);
    assert_eq!(alerts, vec![None, None]);
    assert_eq!(w.baseline, Some(5.0));
    assert!(run(&mut w, &[Some(6.0)])[0].is_some()); // +20% from the new baseline alerts
}

#[test]
fn test_a_drop_to_zero_alerts_then_the_watch_keeps_working_across_a_restart() {
    let mut w = pct_watch("drop_pct", 10.0);
    let alerts = run(&mut w, &[Some(2.0), Some(0.0)]);
    assert!(alerts[0].is_none() && alerts[1].is_some());
    assert_eq!(w.baseline, Some(0.0));
    let mut w = restart(&w); // the 0 baseline was saved with the check that set it
    assert_eq!(run(&mut w, &[Some(1.0)]), vec![None]);
    assert_eq!(w.baseline, Some(1.0));
}

fn quote_body(price: f64, symbol: &str) -> String {
    json!({"chart": {"result": [{"meta": {"symbol": symbol, "currency": "USD", "regularMarketPrice": price,
        "shortName": symbol}}]}})
    .to_string()
}

// ---- float rounding at the boundary (not in the Lean model, which compares exactly) --------------

#[test]
fn test_an_exact_percentage_move_alerts() {
    let mut drop = pct_watch("drop_pct", 10.0);
    assert!(run(&mut drop, &[Some(3.0), Some(2.7)])[1].is_some()); // (3.0 - 2.7) / 3.0 * 100 == 9.999999999999996
}

// ---- pinned behaviour the model proves or relies on ----------------------------------------------

#[test]
fn test_a_restart_keeps_every_field_evaluate_reads() {
    // every_secs as validate() sets it: the store clamps a loaded interval to the kind's floor
    let mut w = Watch::new("w1", "page", "https://shop.example/p", "Thing", "below");
    w.value = Some(300.0);
    w.checks = 3;
    w.last_value = Some(310.5);
    w.last_available = Some(false);
    w.last_hit = Some(true);
    w.baseline = Some(299.25);
    w.armed = false;
    w.every_secs = 1800;
    let back = restart(&w);
    assert_eq!(
        (back.checks, back.armed, back.baseline, back.last_value, back.last_available, back.last_hit),
        (3, false, Some(299.25), Some(310.5), Some(false), Some(true))
    );
    assert_eq!(back, w);
}

#[test]
fn test_an_unknown_first_reading_then_one_that_holds_alerts_once() {
    // Lean: unknown_start_alerts. Intended: the owner was told "no reading yet", so this is news.
    let mut w = Watch::new("w1", "page", "https://shop.example/p", "Thing", "below");
    w.value = Some(300.0);
    let alerts = run(&mut w, &[None, Some(250.0), Some(240.0), None, Some(230.0)]);
    assert_eq!(fired(&alerts), vec![false, true, false, false, false]);
    let mut avail = Watch::new("w2", "search", "tickets", "Tour", "available");
    let got: Vec<Option<String>> = [None, Some(true), None, Some(true), Some(false), Some(true)]
        .iter()
        .map(|a| evaluate(&mut avail, &Reading { available: *a, ..Reading::default() }, true))
        .collect();
    assert_eq!(fired(&got), vec![false, true, false, false, false, true]);
}

#[test]
fn test_a_priceless_first_reading_leaves_the_baseline_to_the_first_price() {
    // Lean: none_first_sets_baseline_later.
    let mut w = pct_watch("drop_pct", 10.0);
    assert_eq!(run(&mut w, &[None, Some(100.0)]), vec![None, None]);
    assert_eq!(w.baseline, Some(100.0));
    assert!(run(&mut w, &[Some(90.0)])[0].is_some());
    assert_eq!(w.baseline, Some(90.0));
}

#[test]
fn test_change_compares_against_the_last_known_value() {
    let mut w = Watch::new("w1", "page", "https://shop.example/p", "Thing", "change");
    let got: Vec<Option<String>> = [
        (None, None),
        (Some(80.0), None),
        (None, Some(true)),
        (Some(80.0), Some(true)),
        (None, Some(false)),
        (Some(75.0), Some(false)),
    ]
    .iter()
    .map(|(v, a)| evaluate(&mut w, &Reading { value: *v, available: *a, ..Reading::default() }, true))
    .collect();
    assert_eq!(fired(&got), vec![false, false, false, false, true, true]);
}

// ==== test_watch_lean_limiter.py ===============================================================================

// ---- bug 1: a second penalty shortens a backoff that is still in force --------------------------------------

#[test]
fn test_a_second_penalty_never_shortens_a_backoff_in_force() {
    // Lean: current_violates_backoff. Retry-After 3000, then a refusal with none (strike 2: 120 s): the host is
    // still held for what is left of the 3000 s.
    let clock = TestClock::new(T0);
    let mut rl = lim(&clock);
    assert_eq!(rl.penalize("h", Some(3000.0)), 3000.0);
    clock.add(10.0);
    rl.penalize("h", None);
    assert!(rl.backed_off("h") >= 2990.0);
}

// ---- bug 2: the daily model cap resets when the day goes back ------------------------------------------------

#[test]
fn test_a_day_that_used_its_cap_stays_used_when_the_clock_comes_back() {
    // Lean: current_violates_model_cap.
    let clock = TestClock::new(86400.0 * 100.0 + 86390.0);
    let mut rl = lim(&clock);
    rl.model_used();
    rl.model_used();
    assert!(!rl.model_ok());
    clock.add(20.0); // D+1: a new day, a fresh cap
    assert!(rl.model_ok());
    clock.add(-20.0); // back on D
    assert!(!rl.model_ok());
}

// ---- the properties that held: pinned against the real code --------------------------------------------------

#[test]
fn test_back_and_forth_clock_mints_nothing_a_monotone_clock_would_not() {
    // Lean: cur_bucket_matches_reference. After any wiggle the bucket equals one refilled on the max-so-far clock.
    let clock = TestClock::new(T0);
    let mut rl = RateLimiter::new(LimiterOptions { burst: 3, host_gap_secs: 0.0, ..lim_opts(&clock) });
    for h in ["a", "b", "c"] {
        rl.take(h);
    }
    for step in [5.0, -30.0, 40.0, -25.0, 7.0, -100.0, 103.0] {
        clock.add(step);
        rl.ready_in("x");
    }
    // the furthest the clock ever got is +15 over the takes: 15 s at 6/min is 1.5 tokens, never more
    assert!((rl.tokens() - 1.5).abs() < 1e-9, "{}", rl.tokens());
}

#[test]
fn test_ready_then_take_never_goes_into_debt() {
    let clock = TestClock::new(T0);
    let mut rl = RateLimiter::new(LimiterOptions { burst: 2, host_gap_secs: 0.0, ..lim_opts(&clock) });
    for step in [0.0, 0.0, 0.0, 3.0, 4.0, 10.0, 1.0, 9.0, 30.0] {
        clock.add(step);
        if rl.ready_in("a") == 0.0 {
            rl.take("a");
            assert!(rl.tokens() >= 0.0);
        }
    }
}

#[test]
fn test_penalty_bounds_hold() {
    let clock = TestClock::new(T0);
    let mut rl = RateLimiter::new(LimiterOptions { backoff_max: 600.0, ..lim_opts(&clock) });
    for ra in [None, Some(5.0), Some(999.0), None, Some(0.0), Some(30.0), None, Some(10_000.0)] {
        let secs = rl.penalize("h", ra);
        assert!(secs <= 600.0 && secs >= ra.unwrap_or(0.0).min(600.0));
    }
    rl.clear("h");
    assert_eq!(rl.backed_off("h"), 0.0);
    assert_eq!(rl.penalize("h", None), 60.0);
}

// ==== test_watch_lean_ticketmaster.py ==========================================================================

// ---- review: TRIBUTE false negatives -----------------------------------------------------------------------

#[test]
fn test_an_exact_name_with_a_tribute_word_is_the_act_itself() {
    let payload = obj(json!({"_embedded": {"attractions": [
        {"id": "K1", "name": "The Jimi Hendrix Experience", "upcomingEvents": {"_total": 3}},
        {"id": "T1", "name": "Experience Unlimited", "upcomingEvents": {"_total": 9}}]}}));
    assert_eq!(pick_attraction("The Jimi Hendrix Experience", &payload), "K1");
    assert_eq!(pick_attraction("Experience Unlimited", &payload), "T1");
    // still skipped: the name adds a tribute word the owner's words lack
    let tribute_act = obj(json!({"_embedded": {"attractions": [
        {"id": "T2", "name": "Jimi Hendrix Experience Tribute", "aliases": ["the jimi hendrix experience"]}]}}));
    assert_eq!(pick_attraction("The Jimi Hendrix Experience", &tribute_act), "");
    let big = obj(json!({"_embedded": {"attractions": [{"id": "T3", "name": "Big Band Tribute", "aliases": ["big band"]}]}}));
    assert_eq!(pick_attraction("big band", &big), "");
}

// ==== test_watch.py: the cases that are this unit's ===========================================================

fn limiter_kw(clock: &TestClock) -> LimiterOptions {
    LimiterOptions { backoff_max: 600.0, ..lim_opts(clock) }
}

#[test]
fn test_the_bucket_runs_dry_and_refills() {
    let clock = TestClock::new(T0);
    let mut lim = RateLimiter::new(LimiterOptions { host_gap_secs: 0.0, ..limiter_kw(&clock) });
    assert_eq!(lim.ready_in("a"), 0.0);
    lim.take("a");
    assert_eq!(lim.ready_in("b"), 0.0);
    lim.take("b");
    // two tokens spent at 6/min: the next one is 10 s away, for any host
    assert!((lim.ready_in("c") - 10.0).abs() < 1e-6);
    clock.add(5.0);
    assert!((lim.ready_in("c") - 5.0).abs() < 1e-6);
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
    let clock = TestClock::new(T0);
    let mut lim = RateLimiter::new(LimiterOptions { burst: 10, rate_per_min: 600.0, ..limiter_kw(&clock) });
    lim.take("shop.example");
    assert!((lim.ready_in("shop.example") - 20.0).abs() < 1e-6);
    assert_eq!(lim.ready_in("other.example"), 0.0);
    clock.add(20.0);
    assert_eq!(lim.ready_in("shop.example"), 0.0);
}

#[test]
fn test_retry_after_holds_the_host_and_backoff_doubles_capped() {
    let clock = TestClock::new(T0);
    let mut lim =
        RateLimiter::new(LimiterOptions { burst: 10, rate_per_min: 600.0, host_gap_secs: 0.0, ..limiter_kw(&clock) });
    assert_eq!(lim.penalize("h", Some(120.0)), 120.0);
    assert!((lim.ready_in("h") - 120.0).abs() < 1e-6);
    assert_eq!(lim.ready_in("elsewhere"), 0.0);
    for want in [120.0, 240.0, 480.0, 600.0, 600.0] {
        assert_eq!(lim.penalize("h", None), want);
    }
    assert_eq!(lim.penalize("h", Some(99999.0)), 600.0);
    lim.clear("h");
    assert_eq!(lim.ready_in("h"), 0.0);
    assert_eq!(lim.penalize("h", None), 60.0);
}

#[test]
fn test_the_daily_model_cap_refuses_and_resets() {
    let clock = TestClock::new(86400.0 * 100.0 + 10.0);
    let mut lim = RateLimiter::new(limiter_kw(&clock));
    assert!(lim.model_ok());
    lim.model_used();
    assert!(lim.model_ok());
    lim.model_used();
    assert!(!lim.model_ok() && lim.model_calls_today() == 2);
    clock.add(86400.0);
    assert!(lim.model_ok() && lim.model_calls_today() == 0);
}

#[test]
fn test_a_clock_that_steps_back_mints_no_tokens() {
    let clock = TestClock::new(T0);
    let mut lim =
        RateLimiter::new(LimiterOptions { burst: 1, rate_per_min: 6.0, host_gap_secs: 0.0, ..limiter_kw(&clock) });
    lim.take("a");
    clock.add(-30.0); // waiting longer is the safe answer
    assert!(lim.ready_in("a") >= 10.0);
    clock.add(30.0); // and forward again to where it was: still nothing refilled
    assert!((lim.ready_in("a") - 10.0).abs() < 1e-6);
}

#[test]
fn test_the_backoff_ladder_never_overflows() {
    // test_a_model_services_429_holds_the_model_service_not_the_page's limiter half
    let clock = TestClock::new(T0);
    let mut lim = RateLimiter::new(limiter_kw(&clock));
    let mut secs = 0.0;
    for _ in 0..1100 {
        secs = lim.penalize("h", None);
    }
    assert_eq!(secs, 600.0);
}

#[test]
fn test_conditions_fire_on_the_edge_and_rearm() {
    let p = |v: f64| Reading { value: Some(v), currency: "USD".into(), ..Reading::default() };
    let mut below = Watch::new("w1", "quote", "AAPL", "AAPL", "below");
    below.value = Some(300.0);
    let alerts = run_readings(&mut below, &[p(310.0), p(305.0), p(299.0), p(290.0), p(301.0), p(298.0)]);
    assert_eq!(fired(&alerts), vec![false, false, true, false, false, true]);
    assert_eq!(alerts[2].as_deref(), Some("AAPL dropped to $299 (was $305), past your $300 mark."));
    let mut already = Watch::new("w2", "quote", "AAPL", "AAPL", "below");
    already.value = Some(300.0);
    let got = run_readings(&mut already, &[p(290.0), p(280.0), p(305.0), p(295.0)]);
    assert_eq!(fired(&got), vec![false, false, false, true]);

    let mut above = Watch::new("w3", "quote", "BTC-USD", "Bitcoin", "above");
    above.value = Some(100000.0);
    assert_eq!(fired(&run_readings(&mut above, &[p(90000.0), p(100500.0), p(101000.0)])), vec![false, true, false]);

    let mut drop = Watch::new("w4", "quote", "X", "X", "drop_pct");
    drop.value = Some(10.0);
    let alerts = run_readings(&mut drop, &[p(100.0), p(95.0), p(89.0), p(85.0), p(80.0)]);
    assert_eq!(fired(&alerts), vec![false, false, true, false, true]);
    assert_eq!(alerts[2].as_deref(), Some("X is down 11.0% to $89 (from $100)."));
    assert_eq!(drop.baseline, Some(80.0));
    let mut rise = Watch::new("w5", "quote", "X", "X", "rise_pct");
    rise.value = Some(5.0);
    assert_eq!(fired(&run_readings(&mut rise, &[p(100.0), p(104.0), p(106.0)])), vec![false, false, true]);

    let mut change = Watch::new("w6", "page", "https://t.example", "Seats", "change");
    let alerts = run_readings(&mut change, &[p(80.0), p(80.0), p(75.0), p(75.0)]);
    assert_eq!(fired(&alerts), vec![false, false, true, false]);
    let a2 = alerts[2].as_deref().unwrap();
    assert!(a2.starts_with("Seats changed: $75, down from $80.") && a2.ends_with("\nhttps://t.example"));

    let a = |on: Option<bool>| Reading { available: on, ..Reading::default() };
    let mut avail = Watch::new("w7", "search", "tickets", "Tour", "available");
    let got = run_readings(&mut avail, &[a(Some(false)), a(None), a(Some(true)), a(Some(true)), a(Some(false)), a(Some(true))]);
    assert_eq!(fired(&got), vec![false, false, true, false, false, true]);
    let mut on_at_start = Watch::new("w8", "search", "tickets", "Tour", "available");
    assert_eq!(run_readings(&mut on_at_start, &[a(Some(true))]), vec![None]);
    assert!(!on_at_start.armed);

    let h = |on: Option<bool>| Reading { hit: on, ..Reading::default() };
    let mut phrase = Watch::new("w9", "page", "https://t.example", "Show", "appears");
    phrase.text = "on sale".into();
    let got = run_readings(&mut phrase, &[h(Some(false)), h(Some(true)), h(Some(true)), h(Some(false)), h(Some(true))]);
    assert_eq!(fired(&got), vec![false, true, false, false, true]);
    assert_eq!(phrase.fired, 2);
}

#[test]
fn test_a_reading_in_another_currency_is_no_price() {
    let r = |v: f64, cur: &str, av: Option<bool>| Reading { value: Some(v), currency: cur.into(), available: av, ..Reading::default() };
    let mut w = Watch::new("w1", "page", "https://shop.example/p", "Lamp", "below");
    w.value = Some(50.0);
    assert!(evaluate(&mut w, &r(60.0, "USD", None), true).is_none());
    assert!(evaluate(&mut w, &r(45.0, "EUR", None), true).is_none()); // 45 EUR is not under $50
    assert_eq!(w.last_value, Some(60.0));
    assert_eq!(w.currency, "USD");
    assert!(evaluate(&mut w, &r(45.0, "USD", None), true).is_some());
    // a change that is both a price move and an availability flip says both
    let mut c = Watch::new("w2", "page", "https://shop.example/p", "Lamp", "change");
    evaluate(&mut c, &r(80.0, "USD", Some(false)), true);
    let alert = evaluate(&mut c, &r(70.0, "USD", Some(true)), true).unwrap();
    assert!(alert.contains("down from $80") && alert.contains("available now"));
}

#[test]
fn test_a_first_reading_the_owner_never_saw_fires() {
    let mut w = Watch::new("w1", "quote", "AAPL", "Apple", "below");
    w.value = Some(300.0);
    let alert = evaluate(&mut w, &Reading { value: Some(290.0), currency: "USD".into(), ..Reading::default() }, false);
    assert_eq!(alert.as_deref(), Some("Apple dropped to $290, past your $300 mark."));
    assert!(!w.armed && w.fired == 1);
}

#[test]
fn test_search_and_ticketmaster_alerts_carry_the_note_and_the_link() {
    let mut w = Watch::new("w1", "search", "tickets for X in Seattle", "X tour", "available");
    evaluate(&mut w, &Reading { available: Some(false), ..Reading::default() }, true);
    let r = Reading {
        available: Some(true),
        value: Some(85.0),
        currency: "USD".into(),
        note: "On sale [now]   at the box office".into(),
        url: "https://tix.example/x".into(),
        ..Reading::default()
    };
    assert_eq!(
        evaluate(&mut w, &r, true).as_deref(),
        Some("X tour is available now. From $85. On sale (now) at the box office\nhttps://tix.example/x")
    );
    // a page alert links the page the owner gave, never a URL from the reading, and carries no note
    let mut p = Watch::new("w2", "page", "https://shop.example/p", "Lamp", "available");
    evaluate(&mut p, &Reading { available: Some(false), ..Reading::default() }, true);
    let alert = evaluate(&mut p, &Reading { available: Some(true), note: "a note".into(), ..Reading::default() }, true);
    assert_eq!(alert.as_deref(), Some("Lamp is available now.\nhttps://shop.example/p"));
}

#[test]
fn test_a_hand_edited_watch_is_read_defensively() {
    let good = obj(Watch::new("w1", "quote", "AAPL", "A", "change").to_json());
    let with = |extra: Value| {
        let mut m = good.clone();
        m.extend(obj(extra));
        Value::Object(m)
    };
    // upper case: read as w1
    assert_eq!(Watch::from_json(&with(json!({"id": " W1 "}))).unwrap().id, "w1");
    // wrong types: defaults
    let w2 = Watch::from_json(&with(json!({"id": "w2", "next_at": "soon", "every_secs": 1.0, "armed": "yes"}))).unwrap();
    assert_eq!(w2.next_at, 0.0);
    assert!(w2.armed);
    assert_eq!(w2.every_secs, FLOOR_SECS.iter().find(|(k, _)| *k == "quote").unwrap().1);
    // target not a string, unknown kind or condition, a bad id, not an object: none
    for bad in [
        json!({"id": "w3", "kind": "quote", "target": ["AAPL"], "condition": "change"}),
        json!({"id": "w3", "kind": "stock", "target": "AAPL", "condition": "change"}),
        json!({"id": "w3", "kind": "quote", "target": "AAPL", "condition": "soon"}),
        json!({"id": "x3", "kind": "quote", "target": "AAPL", "condition": "change"}),
        json!({"id": "w1234567890", "kind": "quote", "target": "AAPL", "condition": "change"}),
        json!({"id": "w3", "kind": "quote", "target": "   ", "condition": "change"}),
        json!(["w3"]),
    ] {
        assert!(Watch::from_json(&bad).is_none(), "{bad}");
    }
    // numbers: not finite or past 1e15 is none; an int field truncates; an optional float takes null
    let w = Watch::from_json(&with(json!({"value": 1e16, "last_value": null, "checks": 7.9, "errors": -5,
        "fired": 5_000_000, "created": 12.5, "baseline": true}))).unwrap();
    assert_eq!((w.value, w.last_value, w.checks, w.errors, w.fired, w.created, w.baseline), (None, None, 7, 0, 1_000_000, 12.5, None));
    // the label is collapsed, else the target; strings are cut to 4000; via one of the rungs; a browser floor
    let w = Watch::from_json(&with(json!({"label": "  Big \n  Lamp ", "via": "browser", "every_secs": 60,
        "pending": "x".repeat(5000), "ref": "K8vZ", "cached": {"price": 5}}))).unwrap();
    assert_eq!((w.label.as_str(), w.via.as_str(), w.every_secs, w.pending.len(), w.ref_.as_str()), ("Big Lamp", "browser", 900, 4000, "K8vZ"));
    assert_eq!(w.cached["price"], json!(5));
    let w = Watch::from_json(&with(json!({"label": " ", "via": "teleport", "every_secs": 10_000_000}))).unwrap();
    assert_eq!((w.label.as_str(), w.via.as_str(), w.every_secs), ("AAPL", "", 7 * 24 * 3600));
    // an unknown key is dropped
    assert!(!obj(Watch::from_json(&with(json!({"surprise": 1}))).unwrap().to_json()).contains_key("surprise"));
}

#[test]
fn test_to_json_writes_every_field_in_order() {
    let w = Watch::new("w1", "quote", "AAPL", "A", "change");
    let keys: Vec<String> = obj(w.to_json()).keys().cloned().collect();
    assert_eq!(keys.first().map(String::as_str), Some("id"));
    assert_eq!(keys.len(), 35);
    assert!(keys.contains(&"ref".to_string()) && !keys.contains(&"ref_".to_string()));
    assert_eq!(keys.last().map(String::as_str), Some("cached"));
    assert_eq!(w.to_json()["created"], json!(0.0));
    assert_eq!(w.to_json()["checks"], json!(0));
}

#[test]
fn test_quotes_read_minor_units_inside_the_session() {
    let now = T0;
    let meta = obj(json!({"symbol": "VOD.L", "currency": "GBp", "regularMarketPrice": 7250,
        "currentTradingPeriod": {"regular": {"start": now - 100.0, "end": now + 100.0}}}));
    let r = quote_reading(&meta, 7250.0, now);
    assert_eq!((r.value, r.currency.as_str(), r.source.as_str(), r.name.as_str(), r.note.as_str()), (Some(72.5), "GBP", "quote", "VOD.L", ""));
    let meta = obj(json!({"currency": "usd", "shortName": "", "longName": "Apple Inc."}));
    let r = quote_reading(&meta, 1.0 / 3.0, now);
    assert_eq!((r.value, r.currency.as_str(), r.name.as_str()), (Some(0.333333), "USD", "Apple Inc."));
}

#[test]
fn test_quotes_read_minor_units_and_unknown_symbols() {
    let now = T0;
    let mut meta = obj(json!({"symbol": "VOD.L", "currency": "GBp", "regularMarketPrice": 7250,
        "currentTradingPeriod": {"regular": {"start": now - 50000.0, "end": now - 20000.0}}, "fulldayPrice": 7300}));
    let r = quote_reading(&meta, 7250.0, now);
    assert_eq!((r.value, r.currency.as_str(), r.note.as_str()), (Some(72.5), "GBP", "after hours £73"));
    meta.insert("currentTradingPeriod".into(), json!({"regular": {"start": now - 100.0, "end": now + 100.0}}));
    assert_eq!(quote_reading(&meta, 7250.0, now).note, "");
    meta.insert("currentTradingPeriod".into(), json!({"regular": {"start": now + 100.0, "end": now + 200.0}}));
    assert_eq!(quote_reading(&meta, 7250.0, now).note, "before hours £73");
}

#[test]
fn test_ticketmaster_statuses_follow_its_own_definitions() {
    let now = T0;
    let day = 86400.0;
    // offsale before the public sale: watched, not available, and the note says when it opens
    let r = ticketmaster_reading(&[tm_event("offsale", Some((now + 3.0 * day, None)), None, None)], now);
    assert_eq!(r.available, Some(false));
    assert!(r.note.contains("1 date listed, 0 on sale now.") && r.note.contains("the public sale opens"), "{}", r.note);
    assert!(r.note.ends_with(" for the 2026-12-01 show."), "{}", r.note);
    // a presale window open now counts, even while the status is offsale
    let r = ticketmaster_reading(&[tm_event("offsale", Some((now + day, None)), Some((now - 60.0, now + 3600.0)), None)], now);
    assert_eq!(r.available, Some(true));
    // onsale and rescheduled are on sale; postponed and canceled are not; an ended offsale is dropped
    assert_eq!(ticketmaster_reading(&[tm_event("onsale", None, None, None)], now).available, Some(true));
    assert_eq!(ticketmaster_reading(&[tm_event("rescheduled", None, None, None)], now).available, Some(true));
    let gone = [tm_event("postponed", None, None, None), tm_event("canceled", None, None, None)];
    assert_eq!(ticketmaster_reading(&gone, now).available, Some(false));
    let r = ticketmaster_reading(&[tm_event("offsale", Some((now - 9.0 * day, Some(now - day))), None, None)], now);
    assert_eq!(r.available, Some(false));
    assert!(r.note.starts_with("0 dates listed"), "{}", r.note);
    // no priceRanges: the price is unknown, never 0
    assert_eq!(ticketmaster_reading(&[tm_event("onsale", None, None, None)], now).value, None);
    let r = ticketmaster_reading(&[], now);
    assert_eq!((r.available, r.note.as_str()), (Some(false), "No matching events are listed on Ticketmaster yet."));
    // the link and the name come from the first event in the pool
    let r = ticketmaster_reading(&[tm_event("onsale", None, None, None)], now);
    assert_eq!((r.url.as_str(), r.name.as_str(), r.source.as_str()), ("https://www.ticketmaster.com/show/event/0E0050681F51BA4C", "Big Show", "ticketmaster"));
}

#[test]
fn test_ticketmaster_takes_the_lowest_listed_price() {
    let now = T0;
    let events = [tm_event("onsale", None, None, Some(89.5)), tm_event("onsale", None, None, Some(45.0))];
    let r = ticketmaster_reading(&events, now);
    assert_eq!((r.value, r.currency.as_str()), (Some(45.0), "USD"));
}

#[test]
fn test_ticketmaster_far_future_dates_open_later() {
    let now = T0;
    let mut far = tm_event("offsale", Some((now + 86400.0, None)), None, None);
    far["sales"]["public"]["startDateTime"] = json!("9999-12-31T23:59:59Z");
    let r = ticketmaster_reading(&[far], now);
    assert_eq!(r.available, Some(false));
    assert!(r.note.contains("opens later"), "{}", r.note);
}

#[test]
fn test_ticketmaster_resolves_the_artist() {
    let attractions = obj(json!({"_embedded": {"attractions": [
        {"id": "T1", "name": "Big Band Tribute", "aliases": ["big band"], "upcomingEvents": {"_total": 90}},
        {"id": "K8vZ", "name": "Big Band", "upcomingEvents": {"_total": 12}},
        {"id": "K9", "name": "Big Band Theory", "upcomingEvents": {"_total": 40}}]}}));
    assert_eq!(pick_attraction("big band!", &attractions), "K8vZ");
    assert_eq!(pick_attraction("nobody", &attractions), "");
    assert_eq!(pick_attraction("big band", &obj(json!({}))), "");
}

#[test]
fn test_tribute_words_and_games() {
    let words = tribute_words("A Tribute To Queen vs. The Experience");
    assert_eq!(words, ["tribute", "vs", "experience"].iter().map(|s| s.to_string()).collect());
    assert!(tribute("Golden State Warriors vs. Los Angeles Lakers", "LA Lakers", false));
    assert!(!tribute("Golden State Warriors vs. Los Angeles Lakers", "LA Lakers", true));
    assert!(!tribute("Warriors vs Lakers", "Warriors vs Lakers", false));
    assert!(tribute("Beatles vs Stones - A Musical Showdown", "Beatles", false));
    let game = obj(json!({"classifications": [{"segment": {"name": "Music"}}, {"segment": {"name": "Sports"}}]}));
    assert!(sports(&game));
    assert!(!sports(&obj(json!({"classifications": [{"segment": null}, "Sports"]}))));
    assert_eq!(norm("  The  Jimi-Hendrix  Experience! "), "the jimi hendrix experience");
    let events = tm_events(&obj(json!({"_embedded": {"events": [{"id": "a"}, "junk", 3, {"id": "b"}]}})));
    assert_eq!(events.len(), 2);
    assert!(tm_events(&obj(json!({"_embedded": null}))).is_empty());
}

#[test]
fn test_iso_reads_ticketmasters_times() {
    assert_eq!(iso(&json!("2027-01-15T08:00:00Z")), Some(1_800_000_000.0));
    assert_eq!(iso(&json!("2027-01-15T08:00:00+00:00")), iso(&json!("2027-01-15T08:00:00Z")));
    assert_eq!(iso(&json!("2027-01-15T09:00:00+01:00")), iso(&json!("2027-01-15T08:00:00Z")));
    assert_eq!(iso(&json!("2027-01-15T08:00:00.5Z")).map(|t| t.fract()), Some(0.5));
    assert_eq!(iso(&json!("20270115T080000Z")), iso(&json!("2027-01-15T08:00:00Z")));
    assert!(iso(&json!("2027-01-15")).is_some()); // local midnight
    for bad in [json!(""), json!("soon"), json!("2027-13-01T00:00:00Z"), json!(1_800_000_000), json!(null), json!("2027-01-15T25:00:00Z")] {
        assert_eq!(iso(&bad), None, "{bad}");
    }
}

#[test]
fn test_money_reads_like_a_price() {
    assert_eq!(money(Some(1299.0), "USD"), "$1,299");
    assert_eq!(money(Some(1299.5), "usd"), "$1,299.50");
    assert_eq!(money(Some(85.0), "CHF"), "85 CHF");
    assert_eq!(money(Some(85.0), ""), "85");
    assert_eq!(money(Some(0.00001234), "USD"), "$0.00001234");
    assert_eq!(money(Some(0.5), "EUR"), "€0.5");
    assert_eq!(money(Some(0.123456789), "USD"), "$0.123457");
    assert_eq!(money(Some(1_234_567.0), "GBP"), "£1,234,567");
    assert_eq!(money(Some(0.0), "USD"), "$0.00");
    assert_eq!(money(Some(-5.5), "USD"), "$-5.50");
    assert_eq!(money(None, "USD"), "?");
    assert_eq!(money(Some(f64::NAN), "USD"), "?");
    assert_eq!(money(Some(999.999), "CAD"), "CA$1,000");
}

#[test]
fn test_describe_and_now_line() {
    let mut w = Watch::new("w1", "quote", "AAPL", "Apple", "below");
    w.value = Some(300.0);
    w.currency = "USD".into();
    assert_eq!(describe(&w), "at or below $300");
    assert_eq!(alert_about(&w), "(I texted a watch alert: w1, Apple, at or below $300.)");
    assert_eq!(now_line(&w), "no reading yet");
    w.last_value = Some(310.0);
    w.last_note = "after hours $311".into();
    assert_eq!(now_line(&w), "$310 (after hours $311)");
    w.condition = "drop_pct".into();
    w.value = Some(12.5);
    assert_eq!(describe(&w), "a 12.5% drop");
    w.condition = "rise_pct".into();
    w.value = None;
    assert_eq!(describe(&w), "a 0% rise");
    let mut p = Watch::new("w2", "page", "https://t.example", "Show", "appears");
    p.text = "on sale".into();
    p.last_available = Some(false);
    p.last_hit = Some(true);
    assert_eq!(describe(&p), "\"on sale\" on the page");
    assert_eq!(now_line(&p), "not available, phrase present");
    p.condition = "available".into();
    assert_eq!(describe(&p), "in stock or on sale");
    assert_eq!(now_line(&p), "not available");
    p.condition = "change".into();
    assert_eq!(describe(&p), "any price change");
}

#[test]
fn test_plain_note_hides_no_link() {
    assert_eq!(plain_note("  see [Buy](https://evil.example)\n now "), "see (Buy)(https://evil.example) now");
    assert_eq!(plain_note(&"x".repeat(300)).len(), 200);
}

#[test]
fn test_periods_intervals_and_stretch() {
    assert_eq!(hours(&Value::Null), (None, String::new()));
    assert_eq!(hours(&json!(2)), (Some(2.0), String::new()));
    assert_eq!(hours(&json!(1.5)), (Some(1.5), String::new()));
    for bad in [json!(-3), json!(0), json!(true), json!("2")] {
        assert_eq!(hours(&bad), (None, "the period must be a positive number of hours".to_string()), "{bad}");
    }
    assert_eq!(hours(&json!(1_000_000_000)), (None, "a period of at most a year".to_string()));
    assert_eq!(every_words(900), "15 min");
    assert_eq!(every_words(90), "2 min"); // 1.5 rounds to even
    assert_eq!(every_words(150), "2 min"); // 2.5 rounds to even
    assert_eq!(every_words(6 * 3600), "6 h");
    assert_eq!(every_words(5400), "1.5 h");
    assert_eq!(every_words(7 * 86400), "7 d");
    assert_eq!(every_words(129_600), "1.5 d");
    assert_eq!([0, 1, 2, 3, 4, 5, 1100].map(stretch), [1.0, 1.0, 2.0, 4.0, 8.0, 8.0, 8.0]);
}

#[test]
fn test_when_reads_a_local_time() {
    let shown = when(T0);
    let local = chrono::DateTime::from_timestamp(T0 as i64, 0).unwrap().with_timezone(&chrono::Local);
    assert_eq!(shown, local.format("%a %b %d, %I:%M %p").to_string().replace(" 0", " "));
    assert!(!shown.contains(" 0"));
    assert_eq!(when(f64::NAN), "later");
    assert_eq!(when(253_402_300_799.0), "later"); // 9999-12-31: past a local datetime
    assert_eq!(when(1e300), "later");
    assert_eq!(local_day(T0), local.format("%Y-%m-%d").to_string());
}

#[test]
fn test_model_options_by_model() {
    assert_eq!(Value::Object(model_options(DEFAULT_MODEL)), json!({"temperature": 0, "reasoning": {"enabled": false}}));
    assert_eq!(Value::Object(model_options("anthropic/claude-haiku-5.5")), json!({}));
    assert_eq!(Value::Object(model_options("claude-haiku-5-5")), json!({}));
    assert_eq!(Value::Object(model_options("gpt-6-mini")), json!({}));
    assert_eq!(Value::Object(model_options("google/gemini-3.1-flash-lite")), json!({"temperature": 0}));
}

fn model_reply(data: Value) -> Value {
    json!({"choices": [{"message": {"content": format!("```json\n{data}\n```")}}]})
}

#[test]
fn test_a_models_json_is_checked() {
    let bad = model_reading(&model_reply(json!({"price": "cheap", "available": "yes", "currency": "dollars"})), "model").unwrap();
    assert_eq!((bad.value, bad.available, bad.currency.as_str()), (None, None, ""));
    let err = model_reading(&json!({"choices": [{"message": {"content": "I think it is $5"}}]}), "model").unwrap_err();
    assert_eq!(err.reason, "the model did not answer with JSON");
    let r = model_reading(
        &model_reply(json!({"price": null, "available": true, "currency": " usd ", "note": "On   sale [now]",
            "url": "https://tix.example/a", "blocked": true})),
        "search",
    )
    .unwrap();
    assert_eq!(
        (r.available, r.currency.as_str(), r.note.as_str(), r.url.as_str(), r.source.as_str(), r.blocked),
        (Some(true), "USD", "On sale (now)", "https://tix.example/a", "search", true)
    );
    // a url only from a search, and only a plain one; blocked only when exactly true
    let r = model_reading(&model_reply(json!({"url": "https://tix.example/a", "blocked": "yes"})), "model").unwrap();
    assert_eq!((r.url.as_str(), r.blocked), ("", false));
    let r = model_reading(&model_reply(json!({"url": "https://real.example/[Buy](https://evil.example)"})), "search").unwrap();
    assert_eq!(r.url, "");
}

type Sent = Arc<Mutex<Vec<(String, HttpOptions)>>>;

fn fake_http(sent: Sent, answer: Result<(u16, String), FetchError>) -> Arc<dyn eve_web::Http> {
    Arc::new(move |url: &str, opts: &HttpOptions| {
        sent.lock().push((url.to_string(), opts.clone()));
        answer.clone()
    })
}

#[test]
fn test_openrouter_sends_one_completion() {
    let sent: Sent = Arc::new(Mutex::new(Vec::new()));
    let ctx = ctx_env(&[("OPENROUTER_API_KEY", " sk-test ")])
        .with_http(fake_http(sent.clone(), Ok((200, json!({"choices": []}).to_string()))));
    let out = openrouter(&ctx, &json!({"model": "google/gemini-3.1-flash-lite", "messages": []}), 30.0).unwrap();
    assert_eq!(out, json!({"choices": []}));
    let sent = sent.lock();
    let (url, opts) = &sent[0];
    assert_eq!(url, OPENROUTER_URL);
    assert_eq!(opts.timeout, 30.0);
    let body: Value = serde_json::from_slice(opts.data.as_ref().unwrap()).unwrap();
    assert_eq!(body["provider"], json!({"order": ["google-ai-studio"], "allow_fallbacks": true}));
    let headers: Vec<(&str, &str)> = opts.headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    assert_eq!(
        headers,
        vec![
            ("Authorization", "Bearer sk-test"),
            ("Content-Type", "application/json"),
            ("Accept", "application/json"),
            ("HTTP-Referer", "https://github.com/gurul/eve_01"),
            ("X-Title", "eve watch"),
        ]
    );
}

#[test]
fn test_openrouter_refusals() {
    let none: Sent = Arc::new(Mutex::new(Vec::new()));
    let ctx = ctx_env(&[]).with_http(fake_http(none.clone(), Ok((200, "{}".into()))));
    let err = openrouter(&ctx, &json!({"model": "x"}), 30.0).unwrap_err();
    assert_eq!(err.reason, "no OpenRouter key on this computer (OPENROUTER_API_KEY)");
    assert!(none.lock().is_empty());
    // a 429 is OpenRouter's, never the page's host
    let refused = FetchError::with_retry("the site answered HTTP 429", 429, Some(120.0));
    let ctx = ctx_env(&[("OPENROUTER_API_KEY", "k")]).with_http(fake_http(none.clone(), Err(refused)));
    let err = openrouter(&ctx, &json!({"model": "x", "provider": {}}), 30.0).unwrap_err();
    assert_eq!((err.status, err.retry_after, err.host.as_str()), (429, Some(120.0), OPENROUTER_HOST));
    let body: Value = serde_json::from_slice(none.lock()[0].1.data.as_ref().unwrap()).unwrap();
    assert_eq!(body["provider"], json!({})); // a provider the caller named is kept
    for text in ["not json", "[1, 2]"] {
        let ctx = ctx_env(&[("OPENROUTER_API_KEY", "k")]).with_http(fake_http(none.clone(), Ok((200, text.into()))));
        assert_eq!(openrouter(&ctx, &json!({}), 30.0).unwrap_err().reason, "the model service sent an unreadable reply");
    }
}

#[test]
fn test_number_settings_are_clamped() {
    let ctx = ctx_env(&[("A", "abc"), ("B", " 5 "), ("C", "1e9"), ("D", "-3"), ("E", "nan"), ("F", "1_000"), ("G", "")]);
    assert_eq!(number(&ctx, "A", 12.0, 1.0, 600.0), 12.0);
    assert_eq!(number(&ctx, "B", 12.0, 1.0, 600.0), 5.0);
    assert_eq!(number(&ctx, "C", 12.0, 1.0, 600.0), 600.0);
    assert_eq!(number(&ctx, "D", 12.0, 1.0, 600.0), 1.0);
    assert_eq!(number(&ctx, "E", 12.0, 1.0, 600.0), 600.0);
    assert_eq!(number(&ctx, "F", 12.0, 1.0, 6000.0), 1000.0);
    assert_eq!(number(&ctx, "G", 12.0, 1.0, 600.0), 12.0);
    assert_eq!(number(&ctx, "unset", 12.0, 1.0, 600.0), 12.0);
}

#[test]
fn test_expand_user() {
    let home = std::env::var("HOME").unwrap();
    assert_eq!(expand_user("~/.config/eve-bridge/watches.json"), std::path::PathBuf::from(format!("{home}/.config/eve-bridge/watches.json")));
    assert_eq!(expand_user("/tmp/w.json"), std::path::PathBuf::from("/tmp/w.json"));
    assert_eq!(expand_user("~no-such-user-here/w.json"), std::path::PathBuf::from("~no-such-user-here/w.json"));
    assert_eq!(WatchConfig::default().path, std::path::PathBuf::from(format!("{home}/.config/eve-bridge/watches.json")));
}

#[test]
fn test_the_switch_reads_the_environment() {
    let off_extras = [("EVE_WATCH_TLS", "0"), ("EVE_WATCH_BROWSER", "0")];
    let cfg = |pairs: &[(&str, &str)]| {
        let mut all: Vec<(&str, &str)> = off_extras.to_vec();
        all.extend_from_slice(pairs);
        configured(&ctx_env(&all))
    };
    assert!(cfg(&[]).enabled);
    assert!(!cfg(&[("EVE_WATCH", "0")]).enabled);
    assert!(!cfg(&[("EVE_WATCH", "off")]).enabled);
    let c = cfg(&[("EVE_WATCH_RATE", "abc"), ("EVE_WATCH_HOST_GAP", "5"), ("EVE_WATCH_MODEL", "openai/gpt-x")]);
    assert_eq!((c.rate_per_min, c.host_gap_secs, c.model.as_str()), (DEFAULT_RATE_PER_MIN, 5.0, "openai/gpt-x"));
    assert_eq!(cfg(&[]).model, DEFAULT_MODEL);
    let c = cfg(&[("EVE_WATCH_BURST", "7.9"), ("EVE_WATCH_FILE", "/tmp/w.json"), ("TICKETMASTER_API_KEY", " k "),
        ("TINYFISH_API_KEY", "t"), ("EVE_WATCH_HOSTED_CALLS", "20000")]);
    assert_eq!((c.burst, c.path.to_str().unwrap(), c.ticketmaster, c.hosted, c.hosted_calls_per_day, c.browser, c.tls),
        (7, "/tmp/w.json", true, true, 10000, false, false));
    assert!(!cfg(&[("TINYFISH_API_KEY", "t"), ("EVE_WATCH_HOSTED", "off")]).hosted);
}

// ==== the Watcher-driven tests (test_watch_lean_conditions.py, test_watch_lean_ticketmaster.py) ===============

mod watcher_driven {
    use super::*;
    use eve_web::watch::watcher::{Notify, WatchRef, WatcherDeps};
    use eve_web::watch::Watcher;
    use futures::FutureExt;
    use std::path::Path;

    type Fetch = Arc<dyn Fn(&str) -> Result<(u16, String), FetchError> + Send + Sync>;

    struct Rig {
        watcher: Watcher,
        sent: Arc<Mutex<Vec<String>>>,
    }

    /// test_watch.py's `make_watcher`: fakes for everything that leaves the Mac, the given clock, a limiter that
    /// never holds a test up.
    fn make_watcher(path: &Path, fetch: Fetch, clock: &TestClock, ticketmaster: bool) -> Rig {
        let sent: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let ctx = ctx_env(&[("TICKETMASTER_API_KEY", "tm-test")]);
        let mut deps = WatcherDeps::new(ctx);
        let log = sent.clone();
        let notify: Notify = Arc::new(move |text: String, _about: String| {
            log.lock().push(text);
            async { Ok(true) }.boxed()
        });
        deps.notify = Some(notify);
        deps.fetch = Arc::new(move |url: &str, _opts: &HttpOptions| fetch(url));
        deps.ask = Arc::new(|_body: &Value| Ok(json!({})));
        deps.clock = clock.clock();
        deps.sleep = Arc::new(|_s: f64| async {}.boxed());
        let rng: eve_web::watch::watcher::Rng = Box::new(|| 0.5);
        deps.rng = Some(Arc::new(Mutex::new(rng)));
        let limiter = RateLimiter::new(LimiterOptions {
            burst: 50,
            rate_per_min: 600.0,
            host_gap_secs: 0.0,
            model_calls_per_day: 50,
            ..LimiterOptions { backoff_max: 600.0, ..lim_opts(clock) }
        });
        deps.limiter = Some(Arc::new(Mutex::new(limiter)));
        deps.offload = false;
        deps.resolve = Arc::new(|host: &str| host.ends_with(".example"));
        let config = WatchConfig { enabled: true, path: path.to_path_buf(), ticketmaster, ..WatchConfig::default() };
        Rig { watcher: Watcher::new(config, deps), sent }
    }

    fn tm_add_args(target: &str, label: &str) -> Map<String, Value> {
        obj(json!({"kind": "ticketmaster", "target": target, "label": label, "condition": "available",
            "value": null, "text": null, "city": null, "field": null, "every_minutes": null}))
    }

    #[tokio::test]
    async fn test_a_zero_baseline_does_not_stop_the_other_watches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.json");
        let clock = TestClock::new(T0);
        // a rise watch whose first price was 0, due first; an ordinary watch due after it
        let mut zero = Watch::new("w1", "quote", "FREE", "Free", "rise_pct");
        zero.value = Some(10.0);
        zero.checks = 1;
        zero.last_value = Some(0.0);
        zero.baseline = Some(0.0);
        zero.every_secs = 900;
        zero.next_at = clock.t() - 100.0;
        let mut apple = Watch::new("w2", "quote", "AAPL", "Apple", "below");
        apple.value = Some(300.0);
        apple.checks = 1;
        apple.last_value = Some(310.0);
        apple.every_secs = 900;
        apple.next_at = clock.t() - 50.0;
        std::fs::write(&path, json!({"version": 1, "last_id": 2, "watches": [zero.to_json(), apple.to_json()]}).to_string())
            .unwrap();
        let fetch: Fetch = Arc::new(|url: &str| {
            Ok((200, if url.contains("FREE") { quote_body(5.0, "FREE") } else { quote_body(290.0, "AAPL") }))
        });
        let rig = make_watcher(&path, fetch, &clock, false);
        rig.watcher.tick().await;
        let watches: Vec<WatchRef> = rig.watcher.watches();
        assert_eq!(watches[1].lock().checks, 2);
        assert_eq!(*rig.sent.lock(), vec!["Apple dropped to $290 (was $310), past your $300 mark.".to_string()]);
        assert!(watches[0].lock().next_at > clock.t()); // the zero watch was read and rescheduled
    }

    #[tokio::test]
    async fn test_a_sale_open_at_the_first_background_check_is_texted() {
        let dir = tempfile::tempdir().unwrap();
        let clock = TestClock::new(T0);
        let down = Arc::new(Mutex::new(true));
        let (c, d) = (clock.clone(), down.clone());
        let fetch: Fetch = Arc::new(move |url: &str| {
            if *d.lock() {
                return Err(FetchError::with_status("the site answered HTTP 503", 503));
            }
            if url.contains("/attractions.json") {
                return Ok((200, json!({"_embedded": {"attractions": [{"id": "K8vZ", "name": "Big Band",
                    "upcomingEvents": {"_total": 1}}]}}).to_string()));
            }
            // a presale open since a minute ago; the public sale a day away
            let event = tm_event("offsale", Some((c.t() + 86400.0, None)), Some((c.t() - 60.0, c.t() + 3600.0)), None);
            Ok((200, json!({"_embedded": {"events": [event]}}).to_string()))
        });
        let rig = make_watcher(&dir.path().join("w.json"), fetch, &clock, true);
        let out = rig.watcher.add(&tm_add_args("Big Band", "Big Band")).await;
        assert_eq!(out["ok"], json!(true));
        assert!(out["now"].as_str().unwrap().starts_with("the first check failed"));
        let item = rig.watcher.watches()[0].clone();
        assert_eq!(item.lock().checks, 0);
        *down.lock() = false;
        rig.watcher.limiter.lock().clear(eve_web::watch::TM_HOST);
        clock.set(item.lock().next_at + 1.0);
        rig.watcher.tick().await;
        let got = { let w = item.lock(); (w.checks, w.last_available) };
        assert_eq!(got, (1, Some(true)));
        let sent = rig.sent.lock();
        assert!(sent.len() == 1 && sent[0].starts_with("Big Band is available now."), "{sent:?}");
    }

    #[tokio::test]
    async fn test_a_reading_shown_in_the_add_reply_still_never_texts() {
        let dir = tempfile::tempdir().unwrap();
        let clock = TestClock::new(T0);
        let fetch: Fetch = Arc::new(|url: &str| {
            if url.contains("/attractions.json") {
                return Ok((200, json!({"_embedded": {"attractions": [{"id": "K8vZ", "name": "Big Band"}]}}).to_string()));
            }
            Ok((200, json!({"_embedded": {"events": [tm_event("onsale", None, None, None)]}}).to_string()))
        });
        let rig = make_watcher(&dir.path().join("w.json"), fetch, &clock, true);
        let out = rig.watcher.add(&tm_add_args("Big Band", "Big Band")).await;
        assert_eq!(out["now"], json!("available"));
        assert!(out.get("already").is_some());
        for _ in 0..3 {
            let next = rig.watcher.watches()[0].lock().next_at;
            clock.set(next + 1.0);
            rig.watcher.tick().await;
        }
        assert!(rig.sent.lock().is_empty());
    }

    #[tokio::test]
    async fn test_the_keyword_search_keeps_games() {
        let dir = tempfile::tempdir().unwrap();
        let clock = TestClock::new(T0);
        let mut game = tm_event("onsale", None, None, None);
        game.insert("name".into(), json!("Golden State Warriors vs. Los Angeles Lakers"));
        game.insert("classifications".into(), json!([{"segment": {"name": "Sports"}}]));
        let mut show = tm_event("onsale", None, None, None);
        show.insert("name".into(), json!("Beatles vs Stones - A Musical Showdown"));
        show.insert("classifications".into(), json!([{"segment": {"name": "Music"}}]));
        let serve = |event: Map<String, Value>| -> Fetch {
            Arc::new(move |url: &str| {
                if url.contains("/attractions.json") {
                    return Ok((200, json!({"_embedded": {"attractions": []}}).to_string()));
                }
                Ok((200, json!({"_embedded": {"events": [event.clone()]}}).to_string()))
            })
        };
        let rig = make_watcher(&dir.path().join("w.json"), serve(game), &clock, true);
        for words in ["Warriors vs Lakers", "LA Lakers"] {
            let item: WatchRef = Arc::new(Mutex::new(Watch::new("w1", "ticketmaster", words, words, "available")));
            let r = rig.watcher.read(&item).await.unwrap();
            assert!(item.lock().ref_ == "-" && r.available == Some(true), "{words}");
        }
        // a music tribute billed "X vs Y" is still dropped when the owner asked for the band
        let rig2 = make_watcher(&dir.path().join("w2.json"), serve(show), &clock, true);
        let item: WatchRef = Arc::new(Mutex::new(Watch::new("w2", "ticketmaster", "Beatles", "Beatles", "available")));
        assert_eq!(rig2.watcher.read(&item).await.unwrap().available, Some(false));
    }
}
