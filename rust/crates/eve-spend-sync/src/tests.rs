//! Ported from bridge/tests/test_spend.py (the spend_sync section), plus parity checks against Python's output.

use std::io::Write;
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use parking_lot::Mutex;
use serde_json::{json, Value};

use super::*;

// ---- fakes -------------------------------------------------------------------------------------------------

#[derive(Clone)]
enum Answer {
    Json(Value),
    Fail(OpenError),
    Bytes(Vec<u8>),
}

/// Answers by URL prefix; records each request's URL and headers. A route with several answers pops them in turn.
type Seen = Vec<(String, Vec<(String, String)>)>;

#[derive(Default)]
struct FakeOpener {
    routes: Mutex<Vec<(String, Vec<Answer>)>>,
    seen: Mutex<Seen>,
}

impl FakeOpener {
    fn new(routes: Vec<(&str, Vec<Answer>)>) -> Self {
        FakeOpener {
            routes: Mutex::new(routes.into_iter().map(|(p, a)| (p.to_string(), a)).collect()),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn add(&self, prefix: &str, answers: Vec<Answer>) {
        let mut routes = self.routes.lock();
        match routes.iter_mut().find(|(p, _)| p == prefix) {
            Some(route) => route.1 = answers,
            None => routes.push((prefix.to_string(), answers)),
        }
    }

    fn seen(&self) -> Seen {
        self.seen.lock().clone()
    }
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

impl Opener for FakeOpener {
    fn open(&self, url: &str, headers: &[(String, String)], _timeout: Duration) -> Result<Vec<u8>, OpenError> {
        self.seen.lock().push((url.to_string(), headers.to_vec()));
        let mut routes = self.routes.lock();
        for (prefix, answers) in routes.iter_mut() {
            if url.starts_with(prefix.as_str()) {
                // a route's answers are used in turn, and its last one keeps answering
                let answer = if answers.len() > 1 { answers.remove(0) } else { answers[0].clone() };
                return match answer {
                    Answer::Json(v) => Ok(serde_json::to_vec(&v).unwrap()),
                    Answer::Fail(e) => Err(e),
                    Answer::Bytes(b) => Ok(b),
                };
            }
        }
        Err(OpenError::Raised("AssertionError".into()))
    }
}

/// Python's NOW: 2026-09-24 18:00 UTC.
fn now() -> f64 {
    Utc.with_ymd_and_hms(2026, 9, 24, 18, 0, 0).unwrap().timestamp() as f64
}

fn env(pairs: &[(&str, &str)]) -> Env {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn openrouter_routes(total: f64, daily: f64) -> Vec<(&'static str, Vec<Answer>)> {
    vec![
        (
            OPENROUTER_KEY_URL,
            vec![Answer::Json(json!({"data": {"usage_daily": daily, "usage_monthly": 3.4, "usage": 9.9}}))],
        ),
        (OPENROUTER_CREDITS_URL, vec![Answer::Json(json!({"data": {"total_credits": 50, "total_usage": total}}))]),
    ]
}

fn openrouter(total: f64) -> FakeOpener {
    FakeOpener::new(openrouter_routes(total, 0.12))
}

fn approx(v: &Value, want: f64) -> bool {
    v.as_f64().is_some_and(|x| (x - want).abs() < 1e-9)
}

thread_local! {
    static LOG: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Every log line written while `f` runs on this thread.
///
/// One global subscriber writes into a per-thread buffer. A scoped (`with_default`) subscriber races with tests
/// on other threads over tracing's cached callsite interest and can miss the event, so it is not used.
fn capture<R>(f: impl FnOnce() -> R) -> (R, Vec<String>) {
    struct ThreadBuf;
    impl Write for ThreadBuf {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            LOG.with(|log| log.borrow_mut().extend_from_slice(data));
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let subscriber = tracing_subscriber::fmt().with_ansi(false).with_writer(|| ThreadBuf).without_time().finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
    LOG.with(|log| log.borrow_mut().clear());
    let out = f();
    let text = LOG.with(|log| String::from_utf8(std::mem::take(&mut *log.borrow_mut())).unwrap());
    (out, text.lines().map(str::to_string).collect())
}

// ---- ported from test_spend.py -----------------------------------------------------------------------------

#[test]
fn test_openrouter_key_daily_and_account_delta_between_days() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(&[("OPENROUTER_API_KEY", "or-test")]);
    sync_once_in(dir.path(), Some(&env), Some(now() - 86400.0), Some(&openrouter(10.0))).unwrap();
    sync_once_in(dir.path(), Some(&env), Some(now() - 3600.0), Some(&openrouter(10.5))).unwrap();
    let opener = openrouter(11.25);
    let state = sync_once_in(dir.path(), Some(&env), Some(now()), Some(&opener)).unwrap();
    assert_eq!(header(&opener.seen()[0].1, "Authorization"), Some("Bearer or-test"));
    let got = &reported(Some(&state), Some(now()), Some(&env))["openrouter"];
    assert!(got["status"] == "ok" && got["key_today"] == json!(0.12) && got["key_month"] == json!(3.4));
    assert!(approx(&got["account_today"], 1.25) && got["account_partial"] == json!(false));
}

#[test]
fn test_a_first_day_counts_from_its_own_first_snapshot_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(&[("OPENROUTER_API_KEY", "or-test")]);
    sync_once_in(dir.path(), Some(&env), Some(now() - 7200.0), Some(&openrouter(10.0))).unwrap();
    let state = sync_once_in(dir.path(), Some(&env), Some(now()), Some(&openrouter(10.4))).unwrap();
    let got = &reported(Some(&state), Some(now()), Some(&env))["openrouter"];
    assert!(approx(&got["account_today"], 0.4) && got["account_partial"] == json!(true));
}

fn openai_pages() -> Vec<Answer> {
    let day = Utc.with_ymd_and_hms(2026, 9, 24, 0, 0, 0).unwrap().timestamp();
    vec![
        Answer::Json(json!({"data": [{"start_time": day - 86400,
                                      "results": [{"amount": {"value": 1.5, "currency": "usd"}}]}],
                            "has_more": true, "next_page": "p2"})),
        Answer::Json(json!({"data": [{"start_time": day,
                                      "results": [{"amount": {"value": 0.25, "currency": "usd"}},
                                                  {"amount": {"value": 0.5, "currency": "usd"}}]}],
                            "has_more": false, "next_page": null})),
    ]
}

#[test]
fn test_openai_costs_are_read_only_with_an_admin_key_and_paged() {
    let dir = tempfile::tempdir().unwrap();
    let opener = FakeOpener::new(vec![(OPENAI_COSTS_URL, openai_pages())]);
    let none = FakeOpener::new(vec![]);
    // no keys: no calls at all
    assert_eq!(sync_once_in(dir.path(), Some(&env(&[])), Some(now()), Some(&none)).unwrap(), State::new());
    assert!(none.seen().is_empty());
    let env = env(&[("EVE_OPENAI_ADMIN_KEY", "admin-test")]);
    let state = sync_once_in(dir.path(), Some(&env), Some(now()), Some(&opener)).unwrap();
    let seen = opener.seen();
    let (first, second) = (&seen[0], &seen[1]);
    assert!(first.0.contains("bucket_width=1d") && first.0.contains("start_time=") && second.0.contains("page=p2"));
    assert_eq!(header(&first.1, "Authorization"), Some("Bearer admin-test"));
    let got = &reported(Some(&state), Some(now()), Some(&env))["openai"];
    assert!(approx(&got["today"], 0.75) && approx(&got["month"], 2.25) && got["status"] == "ok");
}

#[test]
fn test_anthropic_amounts_are_cents_and_its_headers_are_the_admin_ones() {
    let dir = tempfile::tempdir().unwrap();
    let report = json!({"data": [{"starting_at": "2026-09-24T00:00:00Z", "ending_at": "2026-09-25T00:00:00Z",
                                  "results": [{"amount": "123.45", "currency": "USD"},
                                              {"amount": "10", "currency": "USD"}]},
                                 {"starting_at": "2026-09-23T00:00:00Z", "results": []}],
                        "has_more": false, "next_page": null});
    let opener = FakeOpener::new(vec![(ANTHROPIC_COST_URL, vec![Answer::Json(report)])]);
    let env = env(&[("EVE_ANTHROPIC_ADMIN_KEY", "sk-ant-admin-test")]);
    let state = sync_once_in(dir.path(), Some(&env), Some(now()), Some(&opener)).unwrap();
    let (url, headers) = opener.seen()[0].clone();
    assert!(url.contains("starting_at=2026-08-25T00%3A00%3A00Z") && url.contains("bucket_width=1d"));
    assert_eq!(header(&headers, "X-api-key"), Some("sk-ant-admin-test"));
    assert_eq!(header(&headers, "Anthropic-version"), Some("2023-06-01"));
    let got = &reported(Some(&state), Some(now()), Some(&env))["anthropic"];
    assert!(approx(&got["today"], 1.3345) && got["status"] == "ok");
}

#[test]
fn test_a_failure_is_logged_once_kept_as_a_reason_and_breaks_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(&[("OPENROUTER_API_KEY", "or-test"), ("EVE_OPENAI_ADMIN_KEY", "admin-test")]);
    let refused = || Answer::Fail(OpenError::Http(403));
    let mut routes = openrouter_routes(10.0, 0.12);
    routes.push((OPENAI_COSTS_URL, vec![refused(), refused()]));
    let opener = FakeOpener::new(routes);
    let (state, lines) = capture(|| {
        sync_once_in(dir.path(), Some(&env), Some(now()), Some(&opener)).unwrap();
        for (prefix, answers) in openrouter_routes(10.0, 0.12) {
            opener.add(prefix, answers);
        }
        sync_once_in(dir.path(), Some(&env), Some(now() + 60.0), Some(&opener)).unwrap()
    });
    let got = reported(Some(&state), Some(now()), Some(&env));
    assert!(got["openai"]["status"] == "error" && got["openai"]["error"] == "HTTP 403");
    assert_eq!(got["openrouter"]["status"], "ok"); // the other provider still read
    assert_eq!(lines.iter().filter(|l| l.contains("openai") && l.contains("HTTP 403")).count(), 1);
    assert!(lines.iter().all(|l| !l.contains("admin-test")));
    assert!(lines.iter().any(|l| l
        .contains("spend: openai's own figures could not be read (HTTP 403); tried again next hour")));
}

#[test]
fn test_no_admin_key_says_which_one_to_add() {
    let got = reported(Some(&State::new()), Some(now()), Some(&env(&[])));
    assert_eq!(got["openai"], json!({"status": "no_key", "key_name": "EVE_OPENAI_ADMIN_KEY"}));
    assert_eq!(got["anthropic"], json!({"status": "no_key", "key_name": "EVE_ANTHROPIC_ADMIN_KEY"}));
    assert_eq!(got["openrouter"], json!({"status": "no_key"}));
}

#[tokio::test]
async fn test_the_loop_syncs_at_start_and_stops_on_shutdown() {
    let calls: Arc<Mutex<Vec<Option<Env>>>> = Arc::default();
    let seen = calls.clone();
    let sync: SyncFn = Arc::new(move |env: Option<&Env>| {
        seen.lock().push(env.cloned());
        Ok(())
    });
    let (stop, rx) = watch::channel(false);
    let task = tokio::spawn(run_loop_with(rx, Duration::from_secs(3600), Some(Env::new()), sync));
    for _ in 0..100 {
        if !calls.lock().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(2), task).await.unwrap().unwrap();
    assert_eq!(*calls.lock(), vec![Some(Env::new())]);
}

// ---- parity beyond the Python tests ------------------------------------------------------------------------

#[test]
fn the_state_file_is_byte_identical_to_pythons_sorted_dump() {
    let state = json!({"openrouter": {"at": 1758736800, "key_days": {"2026-09-24": 0.12},
                                      "key_month": {"month": "2026-09", "usd": 3.4},
                                      "account_days": {"2026-09-24": {"first": 10.0, "last": 11.25}}},
                       "openai": {"error": "HTTP 403", "days": {"2026-09-23": 1e-06, "2026-09-24": 1e16,
                                                                "x\u{e9}": -0.0}},
                       "anthropic": {"days": {}, "at": 5, "n": null, "t": true}});
    let dir = tempfile::tempdir().unwrap();
    let path = state_path_in(&dir.path().join("nested"));
    save_state_to(&path, state.as_object().unwrap()).unwrap();
    // json.dumps(state, sort_keys=True), from CPython 3.13
    let want = r#"{"anthropic": {"at": 5, "days": {}, "n": null, "t": true}, "openai": {"days": {"2026-09-23": 1e-06, "2026-09-24": 1e+16, "x\u00e9": -0.0}, "error": "HTTP 403"}, "openrouter": {"account_days": {"2026-09-24": {"first": 10.0, "last": 11.25}}, "at": 1758736800, "key_days": {"2026-09-24": 0.12}, "key_month": {"month": "2026-09", "usd": 3.4}}}"#;
    assert_eq!(std::fs::read_to_string(&path).unwrap(), want);
    assert!(!path.with_extension("tmp").exists());
    assert_eq!(load_state_from(&path), *state.as_object().unwrap());
}

#[test]
fn a_missing_or_odd_state_file_reads_as_empty_and_python_literals_are_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = state_path_in(dir.path());
    assert!(load_state_from(&path).is_empty());
    std::fs::write(&path, "[1, 2]").unwrap();
    assert!(load_state_from(&path).is_empty());
    std::fs::write(&path, "{torn").unwrap();
    assert!(load_state_from(&path).is_empty());
    std::fs::write(&path, r#"{"anthropic": {"days": {"2026-09-24": Infinity, "NaN": NaN}, "note": "NaN x"}}"#).unwrap();
    let state = load_state_from(&path);
    assert_eq!(state["anthropic"]["days"]["2026-09-24"], Value::Null);
    assert_eq!(state["anthropic"]["note"], "NaN x");
}

#[test]
fn urlencode_matches_pythons() {
    let params: Vec<(String, Value)> = vec![
        ("a".into(), json!("x y/é~")),
        ("b".into(), json!(2)),
        ("c".into(), json!(true)),
        ("d".into(), json!(["a"])),
        ("f".into(), json!(1.5)),
        ("g".into(), json!({"k": 1})),
    ];
    // urlencode({...}) from CPython 3.13
    assert_eq!(urlencode(&params), "a=x+y%2F%C3%A9~&b=2&c=True&d=%5B%27a%27%5D&f=1.5&g=%7B%27k%27%3A+1%7D");
}

#[test]
fn the_openai_request_asks_for_thirty_one_days_from_utc_midnight() {
    let dir = tempfile::tempdir().unwrap();
    let opener = FakeOpener::new(vec![(OPENAI_COSTS_URL, vec![Answer::Json(json!({"data": []}))])]);
    sync_once_in(dir.path(), Some(&env(&[("EVE_OPENAI_ADMIN_KEY", "k")])), Some(now()), Some(&opener)).unwrap();
    let start = Utc.with_ymd_and_hms(2026, 8, 25, 0, 0, 0).unwrap().timestamp();
    assert_eq!(opener.seen()[0].0, format!("{OPENAI_COSTS_URL}?start_time={start}&bucket_width=1d&limit=31"));
    let h = &opener.seen()[0].1;
    assert_eq!(header(h, "Accept"), Some("application/json"));
    assert_eq!(header(h, "User-Agent"), Some("eve-spend"));
}

#[test]
fn anthropic_amounts_parse_as_python_float_does_and_odd_ones_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let results = json!([{"amount": " 1_0.5 "}, {"amount": "1__0"}, {"amount": "abc"}, {"amount": true},
                         {"amount": {"x": 1}}, {"amount": 4}, {"amount": "-.5e-1", "currency": "usd"},
                         {"amount": "100", "currency": "EUR"}, "not a dict", {"amount": null}]);
    let report = json!({"data": [{"starting_at": "2026-09-24T00:00:00Z", "results": results}, 7.5, "x"]});
    let opener = FakeOpener::new(vec![(ANTHROPIC_COST_URL, vec![Answer::Json(report)])]);
    let env = env(&[("EVE_ANTHROPIC_ADMIN_KEY", "k")]);
    let state = sync_once_in(dir.path(), Some(&env), Some(now()), Some(&opener)).unwrap();
    // 10.5 + 1 + 4 - 0.05 cents
    assert!(approx(&state["anthropic"]["days"]["2026-09-24"], 0.1545));
    assert!(state["anthropic"].get("error").is_none());
}

#[test]
fn a_reply_that_is_not_an_object_or_not_json_is_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(&[("OPENROUTER_API_KEY", "k")]);
    for answer in [Answer::Json(json!([1])), Answer::Bytes(b"<html>".to_vec()), Answer::Bytes(vec![0xff, 0xfe]),
                   Answer::Bytes(b"{\"data\": 1e400}".to_vec())] {
        let opener = FakeOpener::new(vec![(OPENROUTER_KEY_URL, vec![answer])]);
        let state = sync_once_in(dir.path(), Some(&env), Some(now()), Some(&opener)).unwrap();
        assert_eq!(state["openrouter"]["error"], "unreadable reply");
    }
    let opener = FakeOpener::new(vec![
        (OPENROUTER_KEY_URL, vec![Answer::Json(json!({"data": "x"}))]),
        (OPENROUTER_CREDITS_URL, vec![Answer::Json(json!({"data": {}}))]),
    ]);
    let state = sync_once_in(dir.path(), Some(&env), Some(now()), Some(&opener)).unwrap();
    assert_eq!(state["openrouter"]["error"], "unreadable reply");
}

#[test]
fn errors_name_python_reasons_and_a_success_clears_the_error() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(&[("OPENROUTER_API_KEY", "k")]);
    for (fail, want) in [
        (OpenError::Unreachable("gaierror".into()), "unreachable (gaierror)"),
        (OpenError::TimedOut, "timed out"),
        (OpenError::Raised("ValueError".into()), "unreadable reply"),
        (OpenError::Raised("ConnectionResetError".into()), "timed out"),
        (OpenError::Raised("KeyError".into()), "KeyError"),
    ] {
        let opener = FakeOpener::new(vec![(OPENROUTER_KEY_URL, vec![Answer::Fail(fail)])]);
        let state = sync_once_in(dir.path(), Some(&env), Some(now()), Some(&opener)).unwrap();
        assert_eq!(state["openrouter"]["error"], want);
        assert_eq!(reported(Some(&state), Some(now()), Some(&env))["openrouter"]["status"], "error");
    }
    let state = sync_once_in(dir.path(), Some(&env), Some(now()), Some(&openrouter(1.0))).unwrap();
    assert!(state["openrouter"].get("error").is_none());
    assert_eq!(state["openrouter"]["at"], json!(now() as i64));
}

#[test]
fn a_bucket_past_year_9999_is_a_value_error_and_an_unlisted_data_a_type_error() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(&[("EVE_OPENAI_ADMIN_KEY", "k")]);
    for (data, want) in [(json!([{"start_time": 1e12}]), "ValueError"), (json!([{"start_time": 1e17}]), "OSError"),
                         (json!([{"start_time": 1e20}]), "OverflowError"), (json!(5), "TypeError")] {
        let opener = FakeOpener::new(vec![(OPENAI_COSTS_URL, vec![Answer::Json(json!({"data": data}))])]);
        let state = sync_once_in(dir.path(), Some(&env), Some(now()), Some(&opener)).unwrap();
        assert_eq!(state["openai"]["error"], want);
    }
}

#[test]
fn a_provider_row_that_is_not_an_object_fails_the_sync_as_python_does() {
    let dir = tempfile::tempdir().unwrap();
    let path = state_path_in(dir.path());
    std::fs::write(&path, r#"{"openrouter": [1]}"#).unwrap();
    let env = env(&[("OPENROUTER_API_KEY", "k")]);
    let err = sync_once_in(dir.path(), Some(&env), Some(now()), Some(&openrouter(1.0))).unwrap_err();
    assert_eq!(err, PyError("TypeError".into()));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"openrouter": [1]}"#); // nothing saved
}

#[test]
fn a_day_map_is_pruned_to_its_newest_keep_days() {
    let dir = tempfile::tempdir().unwrap();
    let mut days = serde_json::Map::new();
    let start = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
    for i in (0..410).rev() {
        days.insert((start + ChronoDuration::days(i)).format("%Y-%m-%d").to_string(), json!(1.0));
    }
    let state = json!({"openai": {"days": days, "at": 1}, "other": 3});
    std::fs::write(state_path_in(dir.path()), state.to_string()).unwrap();
    let state = sync_once_in(dir.path(), Some(&env(&[])), Some(now()), None).unwrap();
    let kept = state["openai"]["days"].as_object().unwrap();
    assert_eq!(kept.len(), KEEP_DAYS);
    assert_eq!(kept.keys().next().unwrap(), "2024-01-11");
    assert_eq!(kept.keys().next_back().unwrap(), &(start + ChronoDuration::days(409)).format("%Y-%m-%d").to_string());
}

#[test]
fn reported_month_is_int_zero_with_no_day_this_month_and_counts_bools() {
    let env = env(&[("EVE_OPENAI_ADMIN_KEY", "k"), ("EVE_ANTHROPIC_ADMIN_KEY", "k")]);
    let state = json!({"openai": {"days": {"2026-08-01": 2.0}, "at": 1},
                       "anthropic": {"days": {"2026-09-01": true, "2026-09-02": 0.1, "2026-09-03": "x"}}});
    let got = reported(state.as_object(), Some(now()), Some(&env));
    assert_eq!(got["openai"]["month"], json!(0));
    assert!(got["openai"]["month"].is_i64());
    assert_eq!(got["openai"]["today"], Value::Null);
    assert!(approx(&got["anthropic"]["month"], 1.1));
    assert_eq!(got["anthropic"]["status"], "pending");
    let empty = json!({"openai": {"days": {}}});
    assert_eq!(reported(empty.as_object(), Some(now()), Some(&env))["openai"]["month"], Value::Null);
}

#[test]
fn reported_openrouter_rows_keep_pythons_keys_and_a_saved_row_shows_without_a_key() {
    let state = json!({"openrouter": {"at": 9, "key_days": {"2026-09-24": 0.5},
                                      "key_month": {"month": "2026-08", "usd": 2.0}}});
    let got = &reported(state.as_object(), Some(now()), Some(&env(&[])))["openrouter"];
    let keys: Vec<&String> = got.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["status", "error", "at", "key_today", "key_day", "key_month", "account_today",
                      "account_partial"]);
    assert_eq!(got["key_month"], Value::Null); // last month's figure is not this month's
    assert_eq!(got["key_today"], json!(0.5));
    assert_eq!(got["key_day"], "2026-09-24");
    assert_eq!(got["account_today"], Value::Null);
    assert_eq!(got["account_partial"], json!(false));
}

#[test]
fn the_account_delta_never_goes_negative() {
    let day = NaiveDate::from_ymd_opt(2026, 9, 24).unwrap();
    let days = json!({"2026-09-23": {"last": 5.0}, "2026-09-24": {"first": 4.0, "last": 4.5}});
    assert_eq!(account_day(Some(&days), day), (Some(0.0), false));
    let days = json!({"2026-09-24": {"first": 4.0, "last": 4.5}});
    assert_eq!(account_day(Some(&days), day), (Some(0.5), true));
    let days = json!({"2026-09-24": {"last": 4.5}});
    assert_eq!(account_day(Some(&days), day), (None, true));
    let days = json!({"2026-09-24": {"last": true}});
    assert_eq!(account_day(Some(&days), day), (None, false));
}

#[test]
fn python_float_sum_and_round_match() {
    let mut s = py::Neumaier::default();
    for _ in 0..10 {
        s.add(0.1);
    }
    assert_eq!(s.total(), 1.0); // sum([0.1] * 10) == 1.0 on CPython 3.12+
    let mut s = py::Neumaier::default();
    for x in [1e16, 1.0, -1e16] {
        s.add(x);
    }
    assert_eq!(s.total(), 1.0);
    assert_eq!(py_round(2.675, 2), 2.67);
    assert_eq!(py_round(0.1234565, 6), 0.123456);
    assert_eq!(py_round(1.0000005, 6), 1.000001);
    assert_eq!(py_round(0.0000125, 5), 1e-05);
}

#[test]
fn the_default_opener_names_a_refused_connection() {
    // port 1 on loopback: nothing listens there
    let err = get_json("http://127.0.0.1:1/x", &[], None, Some(Duration::from_secs(5))).unwrap_err();
    assert_eq!(err, ProviderError::Sync(SyncError("unreachable (ConnectionRefusedError)".into())));
}

#[tokio::test]
async fn the_loop_logs_a_failed_sync_and_keeps_going() {
    let calls: Arc<Mutex<usize>> = Arc::default();
    let seen = calls.clone();
    let sync: SyncFn = Arc::new(move |_env: Option<&Env>| {
        *seen.lock() += 1;
        Err(PyError("TypeError".into()))
    });
    let (stop, rx) = watch::channel(false);
    let task = tokio::spawn(run_loop_with(rx, Duration::from_millis(5), None, sync));
    for _ in 0..200 {
        if *calls.lock() >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(2), task).await.unwrap().unwrap();
    assert!(*calls.lock() >= 3);
}

/// A one-shot server on loopback that reads the request and answers `reply` (or just hangs up).
fn serve_once(reply: Option<&'static [u8]>) -> u16 {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let mut buf = [0u8; 65536];
        let _ = conn.read(&mut buf);
        if let Some(reply) = reply {
            let _ = conn.write_all(reply);
        }
    });
    port
}

#[test]
fn the_default_opener_fails_after_connecting_as_urllib_does() {
    // what CPython's get_json gives against the same servers
    let cases: [(Option<&'static [u8]>, ProviderError); 5] = [
        (Some(b"HELLO\r\n\r\n"), ProviderError::Other(PyError("BadStatusLine".into()))),
        (None, ProviderError::Sync(SyncError("timed out".into()))),
        (
            Some(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{\"a\":"),
            ProviderError::Other(PyError("IncompleteRead".into())),
        ),
        (Some(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{]"), ProviderError::Sync(SyncError("unreadable reply".into()))),
        (Some(b"HTTP/1.1 500 X\r\nContent-Length: 0\r\n\r\n"), ProviderError::Sync(SyncError("HTTP 500".into()))),
    ];
    for (reply, want) in cases {
        let port = serve_once(reply);
        let got = get_json(&format!("http://127.0.0.1:{port}/x"), &[], None, Some(Duration::from_secs(5)));
        assert_eq!(got.unwrap_err(), want, "reply {:?}", reply.map(String::from_utf8_lossy));
    }
}

#[test]
fn a_local_day_is_read_where_python_reads_it() {
    // 10000-01-01T00:00:00Z: Python's UTC datetime refuses it; the local one is fine west of Greenwich
    let ts = 253_402_300_800.0;
    assert_eq!(py::utc_date(ts), Err(PyError("ValueError".into())));
    let west = chrono::Local.timestamp_opt(253_402_300_800 - 1, 0).single().is_some_and(|d| {
        chrono::Datelike::year(&d.date_naive()) == 9999 && d.offset().local_minus_utc() < 0
    });
    if west {
        assert_eq!(py::local_date(ts).map(|d| d.format("%Y-%m-%d").to_string()), Ok("9999-12-31".to_string()));
    } else {
        assert!(py::local_date(ts).is_err());
    }
    assert_eq!(py::local_date(1e12), Err(PyError("ValueError".into())));
    assert_eq!(py::local_date(1e17), Err(PyError("OSError".into())));
}
