//! Ports of bridge/tests/test_jev.py (the transport, with no socket) plus the jev parts of test_spend_wiring.py
//! and test_computer_agent.py (the meter), and parity tests for the request body and error paths.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use super::*;

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn questions() -> Value {
    json!({"pick": {"type": "choice", "instructions": "which?", "criteria": {"a": "A", "b": "B"}}})
}

fn answer() -> Value {
    json!({"answers": {"pick": {"choice": "a", "probabilities": {"a": 0.9, "b": 0.1}, "confidence": 0.8}},
           "usage": {"input_tokens": 40}})
}

/// A predict over `opener`, with its own meter and a ledger in a temp folder, so tests never touch the process's.
fn predict_with<O: Opener + 'static>(url: &str, model: &str, opener: O, dir: &Path) -> (JevPredict, Arc<Meter>) {
    let meter = Arc::new(Meter::new());
    let p = make_predict(
        url,
        "k",
        model,
        PredictOptions {
            opener: Some(Arc::new(opener)),
            meter: Some(meter.clone()),
            ledger: Some(eve_spend::Ledger::at(dir)),
            ..PredictOptions::default()
        },
    );
    (p, meter)
}

fn reply(v: &Value) -> Result<Vec<u8>, OpenError> {
    Ok(serde_json::to_vec(v).unwrap())
}

fn ledger_rows(dir: &Path) -> Vec<Map<String, Value>> {
    let mut rows = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let text = std::fs::read_to_string(entry.path()).unwrap();
        for line in text.lines() {
            if let Ok(Value::Object(o)) = serde_json::from_str(line) {
                rows.push(o);
            }
        }
    }
    rows
}

fn failure_text(r: Result<Value, PredictFailure>) -> String {
    match r {
        Ok(v) => panic!("expected a failure, got {v}"),
        Err(e) => e.to_string(),
    }
}

// ---- test_jev.py -----------------------------------------------------------------------------------------

#[test]
fn test_route_config_names_the_key_that_is_missing_and_lets_the_model_be_pinned() {
    assert_eq!(
        route_config(&env(&[("TYPESAFE_API_KEY", "k")]), "").unwrap(),
        ("https://api.typesafe.ai/v1/systemone".to_string(), "k".to_string(), "jev-latest".to_string())
    );
    let (url, key, model) = route_config(
        &env(&[("EVE_JEV_ROUTE", "openrouter"), ("OPENROUTER_API_KEY", " r "), ("EVE_JEV_MODEL", "typesafe/jev-1.13")]),
        "",
    )
    .unwrap();
    assert!(url.ends_with("/api/alpha/decisions") && key == "r" && model == "typesafe/jev-1.13");
    let e = route_config(&env(&[]), "").unwrap_err();
    assert!(e.0.contains("TYPESAFE_API_KEY is not set"), "{e}");
    let e = route_config(&env(&[("EVE_JEV_ROUTE", "carrier-pigeon"), ("TYPESAFE_API_KEY", "k")]), "").unwrap_err();
    assert!(e.0.contains("must be one of"), "{e}");
}

#[test]
fn test_predict_posts_one_request_and_hands_the_body_back() {
    let dir = tempfile::tempdir().unwrap();
    let seen: Arc<Mutex<Vec<(String, Option<String>, f64, Value)>>> = Arc::default();
    let log = seen.clone();
    let opener = move |r: &Request, timeout: f64| {
        log.lock().push((
            r.url.clone(),
            r.header("Authorization").map(str::to_string),
            timeout,
            serde_json::from_slice(&r.data).unwrap(),
        ));
        reply(&answer())
    };
    let meter = Arc::new(Meter::new());
    let predict = make_predict(
        "https://x/v1",
        "secret",
        "jev-1.13.0",
        PredictOptions {
            timeout_s: 1.5,
            opener: Some(Arc::new(opener)),
            meter: Some(meter.clone()),
            ledger: Some(eve_spend::Ledger::at(dir.path())),
            ..PredictOptions::default()
        },
    );
    let out = predict.predict(&json!({"owner_said": "look left"}), &questions()).unwrap();
    assert_eq!(out, answer());
    let seen = seen.lock();
    assert_eq!(seen.len(), 1);
    let (url, auth, timeout, body) = &seen[0];
    assert!(url == "https://x/v1" && auth.as_deref() == Some("Bearer secret") && *timeout == 1.5);
    assert_eq!(*body, json!({"model": "jev-1.13.0", "state": {"owner_said": "look left"}, "questions": questions()}));
    assert_eq!(meter.snapshot().calls, 1);
    assert_eq!(meter.snapshot().input_tokens, 40);
}

#[test]
fn test_a_retryable_status_is_retried_and_everything_else_is_one_line_never_a_traceback() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(Mutex::new(0));
    let n = calls.clone();
    let flaky = move |_r: &Request, _t: f64| {
        *n.lock() += 1;
        if *n.lock() < 3 {
            return Err(OpenError::Http(529));
        }
        reply(&answer())
    };
    let naps: Arc<Mutex<Vec<f64>>> = Arc::default();
    let nap_log = naps.clone();
    let predict = make_predict(
        "https://x",
        "k",
        "m",
        PredictOptions {
            opener: Some(Arc::new(flaky)),
            sleep: Arc::new(move |s| nap_log.lock().push(s)),
            meter: Some(Arc::new(Meter::new())),
            ledger: Some(eve_spend::Ledger::at(dir.path())),
            ..PredictOptions::default()
        },
    );
    assert_eq!(predict.predict(&json!({}), &questions()).unwrap(), answer());
    assert_eq!(*calls.lock(), 3);
    assert_eq!(*naps.lock(), vec![0.2, 0.4]);

    let (refuse, _) = predict_with("https://x", "m", |_: &Request, _: f64| Err(OpenError::Http(401)), dir.path());
    assert!(failure_text(refuse.predict(&json!({}), &questions())).contains("HTTP 401"));

    let (down, _) =
        predict_with("https://x", "m", |_: &Request, _: f64| Err(OpenError::Url("no route to host".into())), dir.path());
    assert!(failure_text(down.predict(&json!({}), &questions())).contains("unreachable"));

    let (html, _) = predict_with("https://x", "m", |_: &Request, _: f64| Ok(b"<html>".to_vec()), dir.path());
    assert!(failure_text(html.predict(&json!({}), &questions())).contains("undecodable"));

    let (list, _) = predict_with("https://x", "m", |_: &Request, _: f64| reply(&json!([1, 2])), dir.path());
    assert!(failure_text(list.predict(&json!({}), &questions())).contains("not an object"));
}

#[test]
fn test_normalize_accepts_answers_at_the_top_level_and_passes_a_strange_body_through() {
    let flat = json!({"pick": answer()["answers"]["pick"], "usage": {"input_tokens": 40}});
    let normalized = normalize(flat, &questions());
    assert_eq!(normalized, answer());
    assert_eq!(normalize(answer(), &questions()), answer());
    let odd = json!({"result": "nope"});
    assert_eq!(normalize(odd.clone(), &questions()), odd); // the lane then reads "no answer": never a click
}

/// An opener that answers every warm-up with its first criterion.
fn first_choice(r: &Request, _timeout: f64) -> Result<Vec<u8>, OpenError> {
    let body: Value = serde_json::from_slice(&r.data).unwrap();
    let q = body["questions"].as_object().unwrap();
    let (qid, spec) = q.iter().next().unwrap();
    let crit = spec["criteria"].as_object().unwrap();
    let first = crit.keys().next().unwrap().clone();
    let probabilities: Map<String, Value> =
        crit.keys().map(|k| (k.clone(), json!(if *k == first { 1.0 } else { 0.0 }))).collect();
    reply(&json!({"answers": {qid.clone(): {"choice": first, "confidence": 0.9, "probabilities": probabilities}}}))
}

fn load_opts(opener: Option<Arc<dyn Opener>>, dir: &Path) -> LoadOptions {
    LoadOptions {
        opener,
        meter: Some(Arc::new(Meter::new())),
        ledger: Some(eve_spend::Ledger::at(dir)),
        ..LoadOptions::default()
    }
}

#[test]
fn test_load_warms_up_and_a_failed_warm_up_is_a_one_line_runtime_error() {
    let dir = tempfile::tempdir().unwrap();
    let key = env(&[("TYPESAFE_API_KEY", "k")]);
    let d = load(&key, load_opts(Some(Arc::new(first_choice)), dir.path())).unwrap();
    assert!(d.available && d.style == "jev" && d.max_len == MAX_LEN && d.head_max_len == HEAD_MAX_LEN);

    let e = load(&env(&[]), load_opts(None, dir.path())).unwrap_err();
    assert!(e.kind == "RuntimeError" && e.message.contains("TYPESAFE_API_KEY is not set"), "{e:?}");

    let empty = |_: &Request, _: f64| reply(&json!({"answers": {}}));
    let e = load(&key, load_opts(Some(Arc::new(empty)), dir.path())).unwrap_err();
    assert!(e.kind == "RuntimeError" && e.message.contains("warm-up predict failed"), "{e:?}");
}

#[test]
fn test_timeout_from_env_never_raises() {
    assert_eq!(timeout_from_env(&env(&[])), DEFAULT_TIMEOUT_S);
    assert_eq!(timeout_from_env(&env(&[("EVE_JEV_TIMEOUT", " ")])), DEFAULT_TIMEOUT_S);
    assert_eq!(timeout_from_env(&env(&[("EVE_JEV_TIMEOUT", "abc")])), DEFAULT_TIMEOUT_S);
    assert_eq!(timeout_from_env(&env(&[("EVE_JEV_TIMEOUT", "-2")])), DEFAULT_TIMEOUT_S);
    assert_eq!(timeout_from_env(&env(&[("EVE_JEV_TIMEOUT", "inf")])), DEFAULT_TIMEOUT_S);
    assert_eq!(timeout_from_env(&env(&[("EVE_JEV_TIMEOUT", "5")])), 5.0);
}

// ---- test_spend_wiring.py: test_a_jev_decision_is_metered_on_its_routes_bill ---------------------------

#[test]
fn test_a_jev_decision_is_metered_on_its_routes_bill() {
    let cases = [
        ("https://api.typesafe.ai/v1/systemone", json!({"input_tokens": 1_000_000}), "typesafe", 0.042, "priced"),
        (
            "https://openrouter.ai/api/alpha/decisions",
            json!({"input_tokens": 10, "cost": 0.001}),
            "openrouter",
            0.001,
            "reported",
        ),
    ];
    for (url, usage, provider, usd, src) in cases {
        let dir = tempfile::tempdir().unwrap();
        let payload = json!({"answers": {"q": {"choice": "a"}}, "usage": usage});
        let body = payload.clone();
        let (predict, _) = predict_with(url, "typesafe/jev-1.13", move |_: &Request, _: f64| reply(&body), dir.path());
        predict.predict(&json!({}), &json!({"q": {}})).unwrap();
        let rows = ledger_rows(dir.path());
        assert_eq!(rows.len(), 1, "{rows:?}");
        let row = &rows[0];
        assert_eq!(row["p"], provider);
        assert_eq!(row["f"], "jev");
        assert!((row["usd"].as_f64().unwrap() - usd).abs() < 1e-12, "{row:?}");
        assert_eq!(row["src"], src);
    }
}

#[test]
fn a_reply_without_usage_is_recorded_unpriced_with_a_note() {
    let dir = tempfile::tempdir().unwrap();
    meter_spend_in(&eve_spend::Ledger::at(dir.path()), "https://api.typesafe.ai/v1/systemone", "jev-latest", &json!({"answers": {}}));
    let rows = ledger_rows(dir.path());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["usd"], Value::Null);
    assert_eq!(rows[0]["unpriced"], true);
    assert_eq!(rows[0]["note"], "no usage in the reply");
    // a bool cost is not a cost, and a bool token count is not a count
    let dir = tempfile::tempdir().unwrap();
    meter_spend_in(&eve_spend::Ledger::at(dir.path()), "https://openrouter.ai/x", "m", &json!({"usage": {"cost": true, "input_tokens": true}}));
    let rows = ledger_rows(dir.path());
    assert_eq!((rows[0]["p"].as_str(), rows[0]["src"].as_str(), rows[0]["usd"].as_f64()), (Some("openrouter"), Some("priced"), Some(0.0)));
}

// ---- test_computer_agent.py: the meter the run bill reads ---------------------------------------------

#[test]
fn the_meter_counts_calls_tokens_ms_and_errors_and_the_delta_is_key_by_key() {
    let m = Meter::new();
    m.add(&json!({"usage": {"input_tokens": 500}}), 230.0, false); // a Jev call made before the run
    let before = m.snapshot();
    m.add(&json!({"usage": {"input_tokens": 3000}}), 240.0, false); // a step asked mid-run
    m.add(&json!({}), 3000.0, true); // and one that failed
    let d = Meter::delta(&before, &m.snapshot());
    assert_eq!(d, MeterSnapshot { calls: 2, input_tokens: 3000, ms: 3240, errors: 1 });
    assert_eq!(serde_json::to_string(&d).unwrap(), r#"{"calls":2,"input_tokens":3000,"ms":3240,"errors":1}"#);
    // bools, floats, negatives and a non-object usage add no tokens
    m.add(&json!({"usage": {"input_tokens": true}}), 0.0, false);
    m.add(&json!({"usage": {"input_tokens": 1.5}}), 0.0, false);
    m.add(&json!({"usage": {"input_tokens": -9}}), 0.0, false);
    m.add(&json!({"usage": [1]}), 0.0, false);
    m.add(&json!([1]), 0.0, false);
    assert_eq!(m.snapshot().input_tokens, 3500);
    assert_eq!(m.snapshot().calls, 8);
    // round() is half to even
    let r = Meter::new();
    r.add(&json!({}), 0.5, false);
    assert_eq!(r.snapshot().ms, 0);
    r.add(&json!({}), 1.0, false);
    assert_eq!(r.snapshot().ms, 2);
}

// ---- parity ----------------------------------------------------------------------------------------------

#[test]
fn route_config_reads_the_route_argument_first_and_reports_names_as_python_does() {
    let e = env(&[("EVE_JEV_ROUTE", "openrouter"), ("TYPESAFE_API_KEY", "t")]);
    assert_eq!(route_config(&e, " TypeSafe ").unwrap().0, "https://api.typesafe.ai/v1/systemone");
    assert_eq!(
        route_config(&env(&[("OPENROUTER_API_KEY", "r"), ("EVE_JEV_ROUTE", "openrouter")]), "").unwrap().2,
        "typesafe/jev-1.13"
    );
    assert_eq!(
        route_config(&env(&[("EVE_JEV_ROUTE", "carrier-pigeon")]), "").unwrap_err().0,
        "EVE_JEV_ROUTE must be one of ('typesafe', 'openrouter'), not 'carrier-pigeon'"
    );
    // a blank route after strip is not the default: Python's `or` saw a non-empty string
    assert_eq!(
        route_config(&env(&[("EVE_JEV_ROUTE", "  "), ("TYPESAFE_API_KEY", "t")]), "").unwrap_err().0,
        "EVE_JEV_ROUTE must be one of ('typesafe', 'openrouter'), not ''"
    );
    assert_eq!(
        route_config(&env(&[("EVE_JEV_ROUTE", "openrouter"), ("OPENROUTER_API_KEY", "  ")]), "").unwrap_err().0,
        "OPENROUTER_API_KEY is not set (route openrouter)"
    );
    // a blank model pin keeps the route's default
    assert_eq!(route_config(&env(&[("TYPESAFE_API_KEY", "t"), ("EVE_JEV_MODEL", " ")]), "").unwrap().2, "jev-latest");
}

#[test]
fn timeout_from_env_parses_like_python_float() {
    let t = |raw: &str| timeout_from_env(&env(&[("EVE_JEV_TIMEOUT", raw)]));
    assert_eq!(t(" 2.5 "), 2.5);
    assert_eq!(t("1_0"), 10.0);
    assert_eq!(t("1e1"), 10.0);
    assert_eq!(t(".5"), 0.5);
    assert_eq!(t("_1"), DEFAULT_TIMEOUT_S);
    assert_eq!(t("1__0"), DEFAULT_TIMEOUT_S);
    assert_eq!(t("nan"), DEFAULT_TIMEOUT_S);
    assert_eq!(t("0"), DEFAULT_TIMEOUT_S);
    assert_eq!(t("Infinity"), DEFAULT_TIMEOUT_S);
}

#[test]
fn the_request_body_is_python_json_dumps() {
    let dir = tempfile::tempdir().unwrap();
    let seen: Arc<Mutex<Vec<Request>>> = Arc::default();
    let log = seen.clone();
    let (p, _) = predict_with(
        "https://x",
        "m",
        move |r: &Request, _: f64| {
            log.lock().push(r.clone());
            reply(&answer())
        },
        dir.path(),
    );
    p.predict(&json!({"title": "Café ✓ 😀", "n": [1, 0.5, 1e-5]}), &json!({"q": {"type": "noul"}})).unwrap();
    let r = &seen.lock()[0];
    assert_eq!(
        String::from_utf8(r.data.clone()).unwrap(),
        r#"{"model": "m", "state": {"title": "Caf\u00e9 \u2713 \ud83d\ude00", "n": [1, 0.5, 1e-05]}, "questions": {"q": {"type": "noul"}}}"#
    );
    assert_eq!(r.method, "POST");
    assert_eq!(r.header("content-type"), Some("application/json"));
}

#[test]
fn failures_are_metered_as_errors_and_a_bare_exception_passes_through_unmetered() {
    let dir = tempfile::tempdir().unwrap();
    let (p, meter) = predict_with("https://x", "m", |_: &Request, _: f64| Err(OpenError::Http(503)), dir.path());
    let naps = Arc::new(Mutex::new(0));
    let n = naps.clone();
    let p = make_predict(
        p.url(),
        "k",
        p.model(),
        PredictOptions {
            opener: Some(Arc::new(|_: &Request, _: f64| Err(OpenError::Http(503)))),
            sleep: Arc::new(move |_| *n.lock() += 1),
            meter: Some(meter.clone()),
            ledger: Some(eve_spend::Ledger::at(dir.path())),
            ..PredictOptions::default()
        },
    );
    let e = p.predict(&json!({}), &questions()).unwrap_err();
    assert_eq!(e.to_string(), "HTTP 503 from https://x");
    assert_eq!(e.kind(), "JevError");
    assert_eq!(*naps.lock(), 2);
    assert_eq!((meter.snapshot().calls, meter.snapshot().errors), (1, 1));
    assert!(ledger_rows(dir.path()).is_empty()); // a failed call costs nothing on the spend ledger

    let (slow, meter) = predict_with(
        "https://x",
        "m",
        |_: &Request, _: f64| Err(OpenError::Raised { kind: "TimeoutError".into(), message: "timed out".into() }),
        dir.path(),
    );
    let e = slow.predict(&json!({}), &questions()).unwrap_err();
    assert_eq!((e.kind(), e.to_string().as_str()), ("TimeoutError", "timed out"));
    assert_eq!(meter.snapshot().calls, 0);

    let (bad, _) = predict_with("https://x", "m", |_: &Request, _: f64| Ok(vec![0xff, 0xfe]), dir.path());
    assert_eq!(failure_text(bad.predict(&json!({}), &questions())), "undecodable response from https://x: UnicodeDecodeError");
    let (bad, _) = predict_with("https://x", "m", |_: &Request, _: f64| Ok(b"nope".to_vec()), dir.path());
    assert_eq!(failure_text(bad.predict(&json!({}), &questions())), "undecodable response from https://x: JSONDecodeError");
    let (s, _) = predict_with("https://x", "m", |_: &Request, _: f64| reply(&json!("hi")), dir.path());
    assert_eq!(failure_text(s.predict(&json!({}), &questions())), "https://x returned str, not an object");
    let (u, _) = predict_with("https://x", "m", |_: &Request, _: f64| Err(OpenError::Url("timed out".into())), dir.path());
    assert_eq!(failure_text(u.predict(&json!({}), &questions())), "https://x unreachable: timed out");
}

#[test]
fn the_decider_sees_a_jev_error_by_its_python_class() {
    let dir = tempfile::tempdir().unwrap();
    let (p, _) = predict_with("https://x", "m", |_: &Request, _: f64| Err(OpenError::Http(401)), dir.path());
    let e = (p.decider_fn())(&json!({}), &questions()).unwrap_err();
    assert_eq!((e.kind.as_str(), e.message.as_str()), ("JevError", "HTTP 401 from https://x"));
    let a = (p.anyhow_fn())(&json!({}), &questions()).unwrap_err();
    assert_eq!(a.downcast_ref::<PredictFailure>().map(PredictFailure::kind), Some("JevError"));
}

#[test]
fn normalize_keeps_rest_order_and_an_existing_answers_slot() {
    let q = json!({"b": {}, "a": {}});
    let flat = json!({"answers": "x", "a": {"choice": "1"}, "usage": {}, "b": {"choice": "2"}});
    let out = normalize(flat, &q);
    assert_eq!(serde_json::to_string(&out).unwrap(), r#"{"answers":{"b":{"choice":"2"},"a":{"choice":"1"}},"usage":{}}"#);
    // a question key whose value is not an object: passed through
    let half = json!({"a": {"choice": "1"}, "b": "2"});
    assert_eq!(normalize(half.clone(), &q), half);
    // no questions: passed through
    let any = json!({"x": {}});
    assert_eq!(normalize(any.clone(), &json!({})), any);
}

#[test]
fn load_reads_the_timeout_from_env_and_rejects_a_bad_style_as_value_error() {
    let dir = tempfile::tempdir().unwrap();
    let seen: Arc<Mutex<Vec<f64>>> = Arc::default();
    let log = seen.clone();
    let opener = move |r: &Request, t: f64| {
        log.lock().push(t);
        first_choice(r, t)
    };
    let e = env(&[("TYPESAFE_API_KEY", "k"), ("EVE_JEV_TIMEOUT", "1.25")]);
    let d = load(&e, load_opts(Some(Arc::new(opener.clone())), dir.path())).unwrap();
    assert!(d.available && d.warm_ms >= 0.0 && d.load_ms >= 0.0);
    let mut opts = load_opts(Some(Arc::new(opener)), dir.path());
    opts.timeout_s = 0.75;
    load(&e, opts).unwrap();
    assert_eq!(*seen.lock(), vec![1.25, 0.75]);

    let mut opts = load_opts(Some(Arc::new(first_choice)), dir.path());
    opts.style = "loud".into();
    let err = load(&e, opts).unwrap_err();
    assert_eq!(err.kind, "ValueError");
    assert!(err.message.contains("style must be one of"), "{err}");

    let mut opts = load_opts(Some(Arc::new(first_choice)), dir.path());
    opts.route = "openrouter".into();
    assert_eq!(load(&e, opts).unwrap_err().message, "OPENROUTER_API_KEY is not set (route openrouter)");
}

#[test]
fn the_real_transport_reports_an_unreachable_host_as_one_line() {
    // 127.0.0.1:9 (discard) refuses on any Mac; no request leaves the machine.
    let dir = tempfile::tempdir().unwrap();
    let meter = Arc::new(Meter::new());
    let p = make_predict(
        "http://127.0.0.1:9/v1",
        "k",
        "m",
        PredictOptions {
            timeout_s: 1.0,
            meter: Some(meter.clone()),
            ledger: Some(eve_spend::Ledger::at(dir.path())),
            ..PredictOptions::default()
        },
    );
    let e = p.predict(&json!({}), &questions()).unwrap_err();
    assert!(e.to_string().starts_with("http://127.0.0.1:9/v1 unreachable: "), "{e}");
    assert_eq!(meter.snapshot().errors, 1);
}

#[test]
fn the_real_transport_works_inside_a_tokio_runtime() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let p = make_predict(
            "http://127.0.0.1:9/v1",
            "k",
            "m",
            PredictOptions {
                timeout_s: 1.0,
                meter: Some(Arc::new(Meter::new())),
                ledger: Some(eve_spend::Ledger::at(dir.path())),
                ..PredictOptions::default()
            },
        );
        assert!(p.predict(&json!({}), &questions()).unwrap_err().to_string().contains("unreachable"));
        drop(p);
    });
}

#[test]
fn routes_and_constants_match_python() {
    assert_eq!(ROUTES.map(|(n, _)| n), ["typesafe", "openrouter"]);
    assert_eq!(route("openrouter").unwrap().key_name, "OPENROUTER_API_KEY");
    assert!(route("nope").is_none());
    assert_eq!((DEFAULT_ROUTE, DEFAULT_TIMEOUT_S, MAX_LEN, HEAD_MAX_LEN, RETRIES), ("typesafe", 3.0, 32000, 30000, 2));
    assert_eq!(RETRY_STATUS, [429, 500, 502, 503, 529]);
}

#[test]
fn a_url_is_read_as_urllib_reads_it() {
    let dir = tempfile::tempdir().unwrap();
    // Request() raises a bare ValueError for a URL with no scheme, before any send and unmetered.
    let (p, meter) = predict_with("x", "m", |_: &Request, _: f64| reply(&answer()), dir.path());
    let e = p.predict(&json!({}), &questions()).unwrap_err();
    assert_eq!((e.kind(), e.to_string().as_str()), ("ValueError", "unknown url type: 'x'"));
    assert_eq!(meter.snapshot().calls, 0);
    // the opener sees Request.full_url: unwrapped and stripped
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = seen.clone();
    let (p, _) = predict_with(
        " <URL: https://h/v1> ",
        "m",
        move |r: &Request, _: f64| {
            log.lock().push(r.url.clone());
            reply(&answer())
        },
        dir.path(),
    );
    p.predict(&json!({}), &questions()).unwrap();
    assert_eq!(*seen.lock(), vec!["https://h/v1".to_string()]);
    // the real transport: urlopen's URLErrors for a scheme it cannot open and a missing host
    for (url, said) in [
        ("ftp2://x", "ftp2://x unreachable: unknown url type: ftp2"),
        ("http://", "http:// unreachable: no host given"),
        ("http:/x", "http:/x unreachable: no host given"),
    ] {
        let meter = Arc::new(Meter::new());
        let p = make_predict(
            url,
            "k",
            "m",
            PredictOptions { meter: Some(meter.clone()), ledger: Some(eve_spend::Ledger::at(dir.path())), ..PredictOptions::default() },
        );
        assert_eq!(p.predict(&json!({}), &questions()).unwrap_err().to_string(), said);
        assert_eq!(meter.snapshot().errors, 1);
    }
}
