//! Port of bridge/tests/test_grade_router.py: the grade router's rule in code, its fail-safe paths, and the kept
//! verdict. Plus parity checks against the fixtures the Python tools wrote.

use super::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc as std_mpsc, Mutex as StdMutex};

const INF: f64 = f64::INFINITY;

fn bridge() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../bridge")
}

fn fixture(name: &str) -> Value {
    let p = bridge().join("tests/fixtures").join(name);
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap()
}

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn grader(f: impl Fn(&str, &str) -> anyhow::Result<GradeAnswer> + Send + Sync + 'static) -> Grader {
    Arc::new(f)
}

/// A clock that returns the given seconds in turn: `iter([...]).__next__`.
fn clock_of(values: &[f64]) -> impl FnMut() -> f64 {
    let mut it = values.to_vec().into_iter();
    move || it.next().expect("the clock ran out")
}

#[test]
fn test_decide_thinks_only_above_the_gate() {
    assert_eq!(decide(&GradeAnswer::new(0.9, 200.0), 0.65, INF), THINK);
    assert_eq!(decide(&GradeAnswer::new(0.65, 200.0), 0.65, INF), THINK);
    assert_eq!(decide(&GradeAnswer::new(0.64, 200.0), 0.65, INF), INSTANT);
}

#[test]
fn test_an_error_or_a_late_answer_is_instant_todays_behaviour() {
    for a in [GradeAnswer::failed(0.99, 200.0, "TimeoutError"), GradeAnswer::new(0.99, 900.0)] {
        assert_eq!(decide(&a, 0.5, 800.0), INSTANT);
        assert_eq!(decide(&GradeAnswer::new(0.99, 200.0), 0.5, 800.0), THINK); // positive control
    }
}

#[test]
fn test_ask_reads_the_noul_and_survives_a_broken_reply() {
    let mut clock = clock_of(&[0.0, 0.2, 0.0, 0.1, 0.0, 0.1]);
    let good = |_: &Value, _: &Value| Ok(json!({"answers": {"think": {"noul": 0.8}}}));
    let a = ask(&good, "plan my week", "", &mut clock).unwrap();
    assert_eq!(a.noul, 0.8);
    assert!((a.ms - 200.0).abs() < 1e-9);
    let boolean = |_: &Value, _: &Value| Ok(json!({"answers": {"think": {"noul": true}}}));
    assert_eq!(ask(&boolean, "x", "", &mut clock).unwrap().noul, 0.0);
    let boom = |_: &Value, _: &Value| -> anyhow::Result<Value> { Err(Raised::new("RuntimeError", "down").into()) };
    let e = ask(&boom, "x", "", &mut clock).unwrap();
    assert!(e.error.starts_with("RuntimeError"), "{}", e.error);
    assert_eq!(e.error, "RuntimeError: down");
}

#[test]
fn test_the_fitted_gate_ships_inside_the_package() {
    // It lived under tests/fixtures, which no wheel carries: an installed eve fell back to 0.5 without a word
    let pkg = bridge().join("src/eve_bridge").canonicalize().unwrap();
    assert_eq!(GATE_FILE.parent().unwrap().canonicalize().unwrap(), pkg);
    assert!(GATE_FILE.exists());
    let pyproject = std::fs::read_to_string(bridge().join("pyproject.toml")).unwrap();
    let section = pyproject.split("[tool.setuptools.package-data]").nth(1).unwrap();
    let globs = section.split("\n[").next().unwrap();
    assert!(globs.contains("\"grade_gate.json\""));
    assert_eq!(load_gate(&GATE_FILE), 0.65);
    // and the binary carries the same bytes
    assert_eq!(GATE_JSON, std::fs::read_to_string(&*GATE_FILE).unwrap());
    assert_eq!(load_shipped_gate(), 0.65);
}

#[test]
fn test_grade_turn_reads_the_gate_file_once() {
    let reads = AtomicUsize::new(0);
    let load = || {
        reads.fetch_add(1, Ordering::SeqCst);
        0.65
    };
    let cache = GateCache::new();
    let g = grader(|_, _| Ok(GradeAnswer::new(0.9, 5.0)));
    for _ in 0..3 {
        assert_eq!(grade_turn_in(&cache, &load, "plan it", "", Some(&g), None, TIMEOUT_S).grade, THINK);
    }
    assert_eq!(reads.load(Ordering::SeqCst), 1);
}

#[test]
fn test_grade_turn_is_instant_without_a_grader_on_timeout_and_on_a_raise() {
    assert_eq!(grade_turn("hi", "", None, None, TIMEOUT_S), Grade::default());
    let (hold_tx, hold_rx) = std_mpsc::channel::<()>();
    let hold_rx = StdMutex::new(hold_rx);
    let slow_grader = grader(move |_, _| {
        let _ = hold_rx.lock().unwrap().recv_timeout(Duration::from_secs(1));
        Ok(GradeAnswer::new(0.99, 5.0))
    });
    let slow = grade_turn("hi", "", Some(&slow_grader), None, 0.05);
    let _ = hold_tx.send(());
    assert_eq!((slow.grade, slow.error.as_str()), (INSTANT, "timeout"));

    let boom = grader(|_, _| Err(Raised::new("ValueError", "x").into()));
    let raised = grade_turn("hi", "", Some(&boom), Some(0.5), TIMEOUT_S);
    assert_eq!(raised.grade, INSTANT);
    assert_eq!(raised.error, "ValueError");
    let good = grader(|_, _| Ok(GradeAnswer::new(0.9, 5.0)));
    let ok = grade_turn("plan it", "", Some(&good), Some(0.5), TIMEOUT_S); // positive control
    assert!(ok.grade == THINK && ok.asked);
}

#[test]
fn test_the_log_line_carries_numbers_never_the_message() {
    let line = Grade::new(THINK, 0.91, 230.0, true, "").log_line();
    assert_eq!(line, "think (noul 0.91); jev 230 ms");
}

#[test]
fn test_fit_keeps_the_highest_gate_with_the_recall() {
    let answers: Vec<GradeAnswer> = [0.9, 0.8, 0.7, 0.2].iter().map(|n| GradeAnswer::new(*n, 1.0)).collect();
    let (gate, rec) = fit(&answers, &[vec!["think"], vec!["think"], vec!["think"], vec!["instant"]]).unwrap();
    assert!(gate == 0.7 && rec == 1.0);
}

#[test]
fn test_the_question_names_only_the_live_grade_and_the_state_drops_long_text() {
    let q = questions();
    let keys: Vec<&String> = q.as_object().unwrap().keys().collect();
    assert_eq!(keys, vec![THINK]);
    assert_eq!(state(&"x".repeat(900), "")["request"].as_str().unwrap().chars().count(), 500);
}

#[test]
fn test_shipped_equals_the_kept_holdout_verdict() {
    // tools/grade_eval.py check_default: SHIPPED is what the kept holdout rows say
    let kept = fixture("grade_result.json");
    let rows = kept["rows"].as_array().unwrap();
    let answers: Vec<GradeAnswer> = rows.iter().map(|r| GradeAnswer::from_json(r).unwrap()).collect();
    let grades: Vec<&str> = rows.iter().map(|r| r["grade"].as_str().unwrap()).collect();
    let acceptable: Vec<Vec<&str>> = rows
        .iter()
        .map(|r| r["acceptable"].as_array().unwrap().iter().map(|g| g.as_str().unwrap()).collect())
        .collect();
    let verdict = bar(&grades, &acceptable, &answers).unwrap();
    assert_eq!(SHIPPED, verdict.shipped, "SHIPPED is {SHIPPED}; the verdict says {}", verdict.shipped);
    assert!(verdict.shipped); // "passed"
    // the whole dict, key for key, as Python wrote it
    assert_eq!(verdict.to_json(), kept["bar"]);
    assert_eq!(
        serde_json::to_string(&verdict).unwrap(),
        serde_json::to_string(&kept["bar"]).unwrap()
    );
    // the kept grades are the shipped gate's decisions
    for (a, g) in answers.iter().zip(&grades) {
        assert_eq!(decide(a, kept["gate"].as_f64().unwrap(), INF), *g);
    }
}

// ---- parity beyond the Python tests ------------------------------------------------------------------------

#[test]
fn the_tuning_answers_refit_to_the_shipped_gate() {
    // tools/grade_eval.py --tune over the cached answers wrote grade_gate.json: the same fit gives the same gate
    let answers: Vec<GradeAnswer> = fixture("grade_tuning_answers.json")["answers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| GradeAnswer::from_json(r).unwrap())
        .collect();
    let cases = fixture("grade_tuning.json");
    let acceptable: Vec<Vec<String>> = cases["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["acceptable"].as_array().unwrap().iter().map(|g| g.as_str().unwrap().to_string()).collect())
        .collect();
    let (gate, recall) = fit(&answers, &acceptable).unwrap();
    let kept: Value = serde_json::from_str(GATE_JSON).unwrap();
    assert_eq!(gate, kept["gate"].as_f64().unwrap());
    assert_eq!(py_round(recall, 3), kept["tuning_recall"].as_f64().unwrap());
}

#[test]
fn fit_and_bar_fail_as_python_does() {
    let a = vec![GradeAnswer::new(0.9, 1.0)];
    let e = fit(&a, &[vec!["instant"]]).unwrap_err();
    assert_eq!((e.kind.as_str(), e.message.as_str()), ("ValueError", "the tuning set has no think-only case"));
    let e = fit(&a, &[vec!["think"], vec!["think"]]).unwrap_err();
    assert_eq!(e.message, "zip() argument 2 is longer than argument 1");
    let e = bar(&["think", "think"], &[vec!["think"]], &a).unwrap_err();
    assert_eq!(e.message, "zip() argument 2 is shorter than argument 1");
    // no gate reaches the recall: the lowest gate, and the recall it got
    let low: Vec<GradeAnswer> = [0.01, 0.9].iter().map(|n| GradeAnswer::new(*n, 1.0)).collect();
    assert_eq!(fit(&low, &[vec!["think"], vec!["think"]]).unwrap(), (0.05, 0.5));
}

#[test]
fn an_empty_bar_fails_every_gate() {
    let none: [&str; 0] = [];
    let b = bar(&none, &[] as &[Vec<&str>], &[]).unwrap();
    assert_eq!((b.recall, b.over_think, b.errors), (0.0, 1.0, 1.0));
    assert!(b.p95_ms.is_infinite() && !b.shipped);
    let keys: Vec<String> = b.to_json().as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        ["n", "think_only", "instant_only", "recall", "1", "over_think", "2", "p95_ms", "3", "errors", "4", "shipped"]
    );
}

#[test]
fn ask_on_a_malformed_reply_errs_or_raises_where_python_does() {
    let mut clock = clock_of(&[0.0, 0.1, 0.0, 0.1, 0.0, 0.1, 0.0]);
    // predict(...).get on a list: inside the try, an answer with an error
    let list = |_: &Value, _: &Value| Ok(json!([1]));
    let a = ask(&list, "x", "", &mut clock).unwrap();
    assert_eq!(a.error, "AttributeError: 'list' object has no attribute 'get'");
    // falsy answers are {}: no Noul, no error
    let empty = |_: &Value, _: &Value| Ok(json!({"answers": []}));
    assert_eq!(ask(&empty, "x", "", &mut clock).unwrap(), GradeAnswer::new(0.0, 100.0));
    // an integer Noul is read
    let int = |_: &Value, _: &Value| Ok(json!({"answers": {"think": {"noul": 1}}}));
    assert_eq!(ask(&int, "x", "", &mut clock).unwrap().noul, 1.0);
    // answers.get on a truthy string: outside the try, raised
    let s = |_: &Value, _: &Value| Ok(json!({"answers": "yes"}));
    let e = ask(&s, "x", "", &mut clock).unwrap_err();
    assert_eq!((e.kind.as_str(), e.message.as_str()), ("AttributeError", "'str' object has no attribute 'get'"));
    // the error text is clipped to 160 characters
    let mut clock = clock_of(&[0.0, 0.1]);
    let long = |_: &Value, _: &Value| -> anyhow::Result<Value> { Err(Raised::new("OSError", "y".repeat(400)).into()) };
    assert_eq!(ask(&long, "x", "", &mut clock).unwrap().error.chars().count(), 160);
}

#[test]
fn ask_sends_the_state_and_the_question_and_names_a_jev_failure() {
    let seen: Arc<StdMutex<Vec<Value>>> = Arc::default();
    let s2 = seen.clone();
    let opener = move |req: &eve_jev::Request, _t: f64| -> Result<Vec<u8>, eve_jev::OpenError> {
        s2.lock().unwrap().push(serde_json::from_slice(&req.data).unwrap());
        Err(eve_jev::OpenError::Http(400))
    };
    let predict = eve_jev::make_predict(
        "https://jev.test/v1",
        "k",
        "m",
        eve_jev::PredictOptions {
            timeout_s: TIMEOUT_S,
            opener: Some(Arc::new(opener)),
            meter: Some(Arc::new(eve_jev::Meter::new())),
            ..Default::default()
        },
    );
    let call = predict.anyhow_fn();
    let a = ask(&call, "  plan\n my   week ", "hi\tthere", &mut perf_counter).unwrap();
    assert_eq!(a.error, "JevError: HTTP 400 from https://jev.test/v1");
    assert_eq!(decide(&a, 0.0, INF), INSTANT);
    let body = &seen.lock().unwrap()[0];
    assert_eq!(body["model"], "m");
    assert_eq!(
        body["state"],
        json!({"about": ABOUT, "previous": "hi there", "request": "plan my week"})
    );
    assert_eq!(body["questions"], json!({"think": {"type": "noul", "instructions": NOUL}}));
}

#[test]
fn a_jev_answer_through_the_real_predict_grades_think() {
    let dir = tempfile::tempdir().unwrap();
    let opener = |_: &eve_jev::Request, _t: f64| -> Result<Vec<u8>, eve_jev::OpenError> {
        Ok(br#"{"answers": {"think": {"noul": 0.9}}, "usage": {}}"#.to_vec())
    };
    let predict = eve_jev::make_predict(
        "https://jev.test/v1",
        "k",
        "m",
        eve_jev::PredictOptions {
            opener: Some(Arc::new(opener)),
            meter: Some(Arc::new(eve_jev::Meter::new())),
            ledger: Some(eve_spend::Ledger::at(dir.path())),
            ..Default::default()
        },
    );
    let call = predict.anyhow_fn();
    let g = grader(move |t, p| ask(&call, t, p, &mut perf_counter).map_err(anyhow::Error::from));
    let out = grade_turn("plan my week", "", Some(&g), Some(0.65), TIMEOUT_S);
    assert_eq!((out.grade, out.noul, out.asked, out.error.as_str()), (THINK, 0.9, true, ""));
}

#[test]
fn a_grader_that_panics_is_not_an_answer() {
    let g = grader(|_, _| panic!("the grader broke"));
    let out = grade_turn("hi", "", Some(&g), Some(0.5), TIMEOUT_S);
    assert_eq!((out.grade, out.error.as_str(), out.asked), (INSTANT, "not an answer", true));
}

#[test]
fn the_log_line_without_jev_and_with_an_error() {
    assert_eq!(Grade::default().log_line(), "instant; jev off");
    let failed = Grade::new(INSTANT, 0.0, 801.4, true, "timeout").log_line();
    assert_eq!(failed, "instant (noul 0.00); jev 801 ms (timeout)");
}

#[test]
fn mode_reads_eve_grade_router() {
    assert_eq!(mode(&env(&[])), "auto");
    assert_eq!(mode(&env(&[("EVE_GRADE_ROUTER", "")])), "auto");
    assert_eq!(mode(&env(&[("EVE_GRADE_ROUTER", " OFF ")])), "off");
    assert_eq!(mode(&env(&[("EVE_GRADE_ROUTER", "on")])), "on");
    assert_eq!(mode(&env(&[("EVE_GRADE_ROUTER", "sometimes")])), "auto");
}

#[test]
fn test_grade_router_off_makes_no_grader() {
    // test_link_grade.py: EVE_GRADE_ROUTER=off makes no grader
    let base = [("EVE_JEV_ROUTE", "openrouter"), ("OPENROUTER_API_KEY", "sk-test")];
    let mut off = env(&base);
    off.insert("EVE_GRADE_ROUTER".into(), "off".into());
    assert!(make_grader(&off).is_none());
    // no key for the route: no grader either
    assert!(make_grader(&env(&[("EVE_JEV_ROUTE", "openrouter")])).is_none());
    // positive control, and the grader is made once a process per route
    let a = make_grader(&env(&base)).expect("a grader");
    let b = make_grader(&env(&base)).expect("a grader");
    assert!(Arc::ptr_eq(&a, &b));
    let mut other = env(&base);
    other.insert("EVE_JEV_MODEL".into(), "another-model".into());
    let c = make_grader(&other).expect("a grader");
    assert!(!Arc::ptr_eq(&a, &c));
}

#[test]
fn grade_answers_round_trip_their_json() {
    let a = GradeAnswer::failed(0.123456, 481.84, "x");
    assert_eq!(a.to_json(), json!({"noul": 0.1235, "ms": 481.8, "error": "x"}));
    assert_eq!(
        GradeAnswer::from_json(&json!({"noul": null, "ms": "12.5", "error": 0})).unwrap(),
        GradeAnswer::new(0.0, 12.5)
    );
    assert_eq!(GradeAnswer::from_json(&json!({})).unwrap(), GradeAnswer::default());
    let e = GradeAnswer::from_json(&json!({"noul": "high"})).unwrap_err();
    assert_eq!((e.kind.as_str(), e.message.as_str()), ("ValueError", "could not convert string to float: 'high'"));
    let e = GradeAnswer::from_json(&json!({"ms": [1]})).unwrap_err();
    assert_eq!(e.kind, "TypeError");
}

#[test]
fn load_gate_falls_back_on_a_missing_or_broken_file() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("grade_gate.json");
    assert_eq!(load_gate(&p), DEFAULT_GATE);
    std::fs::write(&p, "not json").unwrap();
    assert_eq!(load_gate(&p), DEFAULT_GATE);
    std::fs::write(&p, r#"{"gate": "0.7"}"#).unwrap();
    assert_eq!(load_gate(&p), 0.7);
    std::fs::write(&p, r#"{"gate": 0.4, "tuning_recall": 1.0}"#).unwrap();
    assert_eq!(load_gate(&p), 0.4);
}

#[test]
fn the_grid_is_pythons_rounded_steps() {
    for (i, g) in GRID.iter().enumerate() {
        assert_eq!(*g, py_round(0.05 * (i + 1) as f64, 2));
    }
    assert_eq!(GRADES, ["instant", "think"]);
}
