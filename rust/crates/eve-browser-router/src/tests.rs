//! Port of bridge/tests/test_browser_router.py (the router half; the wiring half tests
//! app_reflex.ReflexFirstAgent and lives with that crate).
//!
//! The live, blind evidence is tools/route_eval.py --browser (browser_holdout2.json); these pin the shape of the
//! question and the safety rule the code owns.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::*;

fn routes_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../bridge/tests/fixtures/routes")
}

fn answer(p_reader: f64, accounts: f64, mac: f64, public: f64, show: f64, interact: f64) -> Value {
    json!({"answers": {"body": {"probabilities": {"web_reader": p_reader, "owner_computer": 1.0 - p_reader}},
                       "needs_accounts": {"noul": accounts}, "needs_mac": {"noul": mac},
                       "public_read_job": {"noul": public}, "show_owner": {"noul": show},
                       "needs_interaction": {"noul": interact}}})
}

fn zero() -> Arc<Clock> {
    Arc::new(|| 0.0)
}

fn replying(a: Value) -> Arc<Predict> {
    Arc::new(move |_state: &Value, _qs: &Value| Ok(a.clone()))
}

fn route_with(a: Value, goal: &str) -> &'static str {
    make_router(replying(a), zero(), GATES).route(goal).unwrap()
}

#[test]
fn test_router_needs_every_condition() {
    let route = |a: Value| route_with(a, "anything");
    assert_eq!(route(answer(0.99, 0.05, 0.02, 0.93, 0.1, 0.05)), WEB);
    assert_eq!(route(answer(0.99, 0.6, 0.02, 0.93, 0.1, 0.05)), CODEX); // needs the owner's accounts
    assert_eq!(route(answer(0.99, 0.05, 0.9, 0.93, 0.1, 0.05)), CODEX); // needs the Mac
    assert_eq!(route(answer(0.99, 0.05, 0.02, 0.93, 0.8, 0.05)), CODEX); // wants it on their screen
    assert_eq!(route(answer(0.99, 0.05, 0.02, 0.93, 0.1, 0.7)), CODEX); // needs typing into a site
    assert_eq!(route(answer(0.81, 0.05, 0.02, 0.93, 0.1, 0.05)), CODEX); // the Choice is not sure enough
    assert_eq!(route(answer(0.99, 0.05, 0.02, 0.4, 0.1, 0.05)), CODEX); // not a reading job

    let boom: Arc<Predict> = Arc::new(|_: &Value, _: &Value| Err(PredictError::new("TimeoutError", "")));
    assert_eq!(make_router(boom, zero(), GATES).route("x").unwrap(), CODEX);
}

/// A gate answer missing from the reply is not a "no": each safety gate reads as "yes" (1.0), so a private
/// request the model half-answered stays on the owner's computer (review A3, 2026-10-18).
#[test]
fn test_a_safety_answer_the_model_leaves_out_keeps_codex() {
    for gate in ["needs_accounts", "needs_mac", "show_owner", "needs_interaction"] {
        let mut a = answer(0.99, 0.05, 0.02, 0.93, 0.1, 0.05);
        a["answers"].as_object_mut().unwrap().shift_remove(gate);
        assert_eq!(route_with(a.clone(), "order history on amazon"), CODEX, "{gate}");
        a["answers"][gate] = json!({"noul": "yes"}); // not a number: same as missing
        assert_eq!(route_with(a, "order history on amazon"), CODEX, "{gate}");
    }
}

#[test]
fn test_router_state_is_the_request_only_and_the_bodies_are_structured() {
    let seen: Arc<Mutex<Option<(Value, Value)>>> = Arc::new(Mutex::new(None));
    let s = seen.clone();
    let predict = move |state: &Value, qs: &Value| {
        *s.lock().unwrap() = Some((state.clone(), qs.clone()));
        Ok(answer(0.0, 1.0, 1.0, 0.0, 1.0, 0.05))
    };
    ask(&predict, "  compare   laptops ", &|| 0.0).unwrap();
    let (state, qs) = seen.lock().unwrap().clone().unwrap();
    assert_eq!(state, json!({"request": "compare laptops"}));
    let criteria: HashSet<&str> = qs["body"]["criteria"].as_object().unwrap().keys().map(String::as_str).collect();
    let options: HashSet<&str> = OPTION_TO_BODY.iter().map(|(o, _)| *o).collect();
    assert_eq!(criteria, options);
    assert_eq!(criteria, HashSet::from(["owner_computer", "web_reader"]));
    for option in qs["body"]["criteria"].as_object().unwrap().values() {
        let keys: HashSet<&str> = option.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, HashSet::from(["what", "for", "not_for"]));
    }
    let nouls: HashSet<&str> =
        qs.as_object().unwrap().iter().filter(|(_, q)| q["type"] == "noul").map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        nouls,
        HashSet::from(["needs_accounts", "needs_mac", "show_owner", "needs_interaction", "public_read_job"])
    );
}

#[test]
fn test_fit_never_allows_an_unsafe_route() {
    let answers = [
        BodyAnswer::new(0.99, 0.05, 0.02, 0.95, 0.1, 0.05),
        BodyAnswer::new(0.97, 0.25, 0.02, 0.9, 0.1, 0.05),
        BodyAnswer::new(0.99, 0.02, 0.02, 0.95, 0.1, 0.45),
    ];
    let g = fit(&answers, &[WEB, CODEX, CODEX]).unwrap();
    let said: Vec<&str> = answers.iter().map(|a| decide(a, &g)).collect();
    assert_eq!(said, [WEB, CODEX, CODEX]);
}

fn cases(name: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(routes_dir().join(name)).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    v["cases"].as_array().unwrap().clone()
}

#[test]
fn test_the_shipped_gates_are_the_fitted_ones_and_every_set_is_labelled() {
    // Not the route-nothing placeholder.
    const { assert!(SHIPPED) };
    const { assert!(GATES.auto <= 1.0 && GATES.public <= 1.0) };
    for name in ["browser_tuning.json", "browser_holdout.json", "browser_holdout2.json"] {
        let cs = cases(name);
        assert!(!cs.is_empty(), "{name}");
        assert!(cs.iter().all(|c| c["reader"] == WEB || c["reader"] == CODEX), "{name}");
    }
    let seen: HashSet<String> = ["browser_tuning.json", "browser_holdout.json"]
        .iter()
        .flat_map(|n| cases(n))
        .map(|c| c["goal"].as_str().unwrap().to_lowercase())
        .collect();
    let blind: Vec<String> =
        cases("browser_holdout2.json").iter().map(|c| c["goal"].as_str().unwrap().to_lowercase()).collect();
    let blind_set: HashSet<String> = blind.iter().cloned().collect();
    // The blind set repeats no seen request.
    assert!(seen.is_disjoint(&blind_set) && blind.len() == blind_set.len());
}

// ---- parity: the edges Python's code takes ----------------------------------------------------------------

#[test]
fn a_failed_predict_abstains_with_its_class_name_and_ms() {
    let ticks = Arc::new(Mutex::new(vec![1.0, 1.25]));
    let t = ticks.clone();
    let clock = move || t.lock().unwrap().remove(0);
    let predict =
        |_: &Value, _: &Value| Err(PredictError::new("JevError", "x".repeat(300)));
    let a = ask(&predict, "weather", &clock).unwrap();
    assert_eq!((a.p_auto, a.accounts, a.mac, a.public, a.show, a.interact), (0.0, 1.0, 1.0, 0.0, 1.0, 1.0));
    assert_eq!(a.ms, 250.0);
    assert_eq!(a.error.chars().count(), 160);
    assert!(a.error.starts_with("JevError: xxx"));
    assert_eq!(decide(&a, &BodyGates::new(0.0, 1.0, 1.0, 0.0, 1.0, 1.0)), CODEX); // an error is always Codex

    let bare = |_: &Value, _: &Value| Err(PredictError::new("TimeoutError", ""));
    assert_eq!(ask(&bare, "x", &|| 0.0).unwrap().error, "TimeoutError: ");
}

#[test]
fn a_reply_that_is_not_a_dict_is_caught_but_misshapen_answers_raise() {
    let list = |_: &Value, _: &Value| Ok(json!([1, 2]));
    let a = ask(&list, "x", &|| 0.0).unwrap();
    assert_eq!(a.error, "AttributeError: 'list' object has no attribute 'get'");

    // Falsy answers read as {}: every gate fails closed.
    for empty in [json!({}), json!({"answers": null}), json!({"answers": []}), json!({"answers": 0})] {
        let a = ask(&move |_: &Value, _: &Value| Ok(empty.clone()), "x", &|| 0.0).unwrap();
        assert_eq!((a.p_auto, a.accounts, a.mac, a.public, a.show, a.interact), (0.0, 1.0, 1.0, 0.0, 1.0, 1.0));
        assert!(a.error.is_empty());
    }

    // Outside Python's try: a truthy non-dict raises AttributeError out of ask (and the router).
    let raises = |reply: Value| ask(&move |_: &Value, _: &Value| Ok(reply.clone()), "x", &|| 0.0).unwrap_err();
    assert_eq!(raises(json!({"answers": "yes"})).message, "'str' object has no attribute 'get'");
    assert_eq!(raises(json!({"answers": {"body": [1]}})).message, "'list' object has no attribute 'get'");
    assert_eq!(raises(json!({"answers": {"body": {"probabilities": 3}}})).message, "'int' object has no attribute 'get'");
    let mut a = answer(0.99, 0.05, 0.02, 0.93, 0.1, 0.05);
    a["answers"]["needs_mac"] = json!(true);
    let e = raises(a.clone());
    assert_eq!((e.kind, e.message.as_str()), ("AttributeError", "'bool' object has no attribute 'get'"));
    let r = make_router(replying(a), zero(), GATES);
    assert!(r.route("x").is_err());
}

#[test]
fn the_choice_probability_takes_a_bool_but_a_noul_does_not() {
    let mut a = answer(0.0, 0.05, 0.02, 0.93, 0.1, 0.05);
    a["answers"]["body"]["probabilities"]["web_reader"] = json!(true);
    a["answers"]["public_read_job"]["noul"] = json!(true);
    let got = ask(&move |_: &Value, _: &Value| Ok(a.clone()), "x", &|| 0.0).unwrap();
    assert_eq!(got.p_auto, 1.0);
    assert_eq!(got.public, 0.0);
    let mut b = answer(0.99, 0.05, 0.02, 1.0, 0.1, 0.05);
    b["answers"]["needs_mac"]["noul"] = json!(0);
    b["answers"]["body"]["probabilities"]["web_reader"] = json!(1);
    let got = ask(&move |_: &Value, _: &Value| Ok(b.clone()), "x", &|| 0.0).unwrap();
    assert_eq!((got.p_auto, got.mac), (1.0, 0.0));
}

#[test]
fn the_request_collapses_python_whitespace() {
    let seen = Arc::new(Mutex::new(Value::Null));
    let s = seen.clone();
    let predict = move |state: &Value, _: &Value| {
        *s.lock().unwrap() = state.clone();
        Ok(json!({}))
    };
    ask(&predict, "\u{1f}a\t\nb\u{a0}c\u{3000} ", &|| 0.0).unwrap();
    assert_eq!(*seen.lock().unwrap(), json!({"request": "a b c"}));
}

#[test]
fn the_questions_keep_python_order_and_wording() {
    let qs = questions();
    let keys: Vec<&str> = qs.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["body", "needs_accounts", "needs_mac", "show_owner", "needs_interaction", "public_read_job"]);
    assert_eq!(qs["body"]["instructions"], "Which of the two should carry out the owner's request?");
    assert_eq!(
        qs["needs_mac"]["instructions"],
        "Does the request involve anything outside public websites: an application on the Mac, a file or folder on \
         the Mac, the desktop or system settings, a local, private or staging web address, or playing sound or \
         video for the owner to watch or hear?"
    );
    assert_eq!(
        BODIES["owner_computer"]["for"][1],
        "anything in a Mac app, a file on the Mac, or a local or staging website"
    );
    assert_eq!(BODIES["web_reader"]["not_for"].as_array().unwrap().len(), 4);
    let order: Vec<&str> = BODIES.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(order, ["owner_computer", "web_reader"]);
    // The criteria serialize exactly as Python's json.dumps would.
    let dumped = eve_jev::py_dumps(&qs["body"]["criteria"]["web_reader"]["what"]);
    assert_eq!(
        dumped,
        "\"a web-reading service that searches the public web and reads public web pages in its own browsers, with \
         no accounts, no cookies and no access to the Mac, and reports what it found back as text; it does not \
         click, type or fill in anything\""
    );
}

#[test]
fn constants_and_defaults_match_python() {
    assert_eq!((CODEX, WEB, AUTO), ("codex", "web", "web"));
    assert_eq!(OPTION_TO_BODY, [("owner_computer", "codex"), ("web_reader", "web")]);
    assert_eq!(BodyGates::default(), BodyGates::new(0.9, 0.1, 0.1, 0.9, 0.5, 0.5));
    assert_eq!(GATES, BodyGates::new(0.6, 0.1, 0.1, 0.95, 0.5, 0.2));
    assert_eq!(GRID, [0.5, 0.6, 0.7, 0.8, 0.9, 0.95]);
    assert_eq!(MAX_GRID, [0.05, 0.1, 0.2, 0.3, 0.5]);
    let a = BodyAnswer::new(0.1, 0.2, 0.3, 0.4, 0.5, 0.6);
    assert_eq!((a.ms, a.error.as_str()), (0.0, ""));
}

#[test]
fn decide_uses_strict_maxima_and_inclusive_minima() {
    let g = GATES;
    assert_eq!(decide(&BodyAnswer::new(0.95, 0.1, 0.1, 0.6, 0.5, 0.2), &g), WEB); // every value on its edge
    assert_eq!(decide(&BodyAnswer::new(0.949, 0.1, 0.1, 0.6, 0.5, 0.2), &g), CODEX);
    assert_eq!(decide(&BodyAnswer::new(0.95, 0.1, 0.1, 0.59, 0.5, 0.2), &g), CODEX);
    assert_eq!(decide(&BodyAnswer::new(0.95, 0.1, 0.1, 0.6, 0.5, 0.21), &g), CODEX);
}

#[test]
fn fit_with_nothing_safe_routes_nothing_and_ties_go_strict() {
    // No WEB-labelled task: every setting routes 0 right, so the tie goes to the strictest: the highest public
    // and auto cut-offs and the lowest maxima.
    let answers = [BodyAnswer::new(0.2, 0.9, 0.9, 0.1, 0.9, 0.9)];
    assert_eq!(fit(&answers, &[CODEX]).unwrap(), BodyGates::new(0.95, 0.05, 0.05, 0.95, 0.05, 0.05));
    // A task that passes every gate but is labelled CODEX: nothing is allowed to route it.
    let unsafe_one = [BodyAnswer::new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0)];
    assert_eq!(fit(&unsafe_one, &[CODEX]).unwrap(), BodyGates::new(1.01, 0.0, 0.0, 1.01, 0.0, 0.0));
}

#[test]
fn fit_with_mismatched_lengths_raises_like_a_strict_zip() {
    let answers = [BodyAnswer::new(0.2, 0.9, 0.9, 0.1, 0.9, 0.9)];
    assert_eq!(fit(&answers, &[CODEX, WEB]).unwrap_err().0, "zip() argument 2 is longer than argument 1");
    let two = [answers[0].clone(), answers[0].clone()];
    assert_eq!(fit(&two, &[CODEX]).unwrap_err().0, "zip() argument 2 is shorter than argument 1");
    // An unsafe route found before the mismatch short-circuits any(), so no grid point ever reaches the zip.
    let unsafe_first = [BodyAnswer::new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0), answers[0].clone()];
    assert_eq!(fit(&unsafe_first, &[CODEX]).unwrap(), BodyGates::new(1.01, 0.0, 0.0, 1.01, 0.0, 0.0));
}

#[test]
fn jev_router_is_none_without_a_key_and_asks_with_a_two_second_budget() {
    let none: BTreeMap<String, String> = BTreeMap::new();
    assert!(jev_router(&none).is_none());
    let bad: BTreeMap<String, String> =
        [("EVE_JEV_ROUTE".to_string(), "nowhere".to_string()), ("TYPESAFE_API_KEY".to_string(), "k".to_string())]
            .into();
    assert!(jev_router(&bad).is_none());
    let ok: BTreeMap<String, String> = [("TYPESAFE_API_KEY".to_string(), "k".to_string())].into();
    let r = jev_router(&ok).expect("a router");
    assert_eq!(r.gates(), GATES);
    let p = jev_predict(&ok).unwrap();
    assert_eq!((p.url(), p.model(), p.timeout_s()), ("https://api.typesafe.ai/v1/systemone", "jev-latest", 2.0));
}

#[test]
fn the_ms_is_measured_on_the_clock() {
    let ticks = Arc::new(Mutex::new(vec![10.0, 10.198]));
    let t = ticks.clone();
    let clock = move || t.lock().unwrap().remove(0);
    let a = ask(&|_: &Value, _: &Value| Ok(answer(0.99, 0.05, 0.02, 0.93, 0.1, 0.05)), "x", &clock).unwrap();
    assert!((a.ms - 198.0).abs() < 1e-6);
    assert_eq!(a.p_auto, 0.99);
}


#[test]
fn fit_picks_the_gates_python_picks() {
    // Expected values measured by running Python's browser_router.fit on the same answers.
    let answers = [
        BodyAnswer::new(0.99, 0.05, 0.02, 0.95, 0.1, 0.05),
        BodyAnswer::new(0.97, 0.25, 0.02, 0.9, 0.1, 0.05),
        BodyAnswer::new(0.99, 0.02, 0.02, 0.95, 0.1, 0.45),
        BodyAnswer::new(0.96, 0.08, 0.15, 0.7, 0.3, 0.1),
        BodyAnswer::new(0.92, 0.01, 0.01, 0.85, 0.2, 0.15),
    ];
    assert_eq!(
        fit(&answers, &[WEB, CODEX, CODEX, WEB, WEB]).unwrap(),
        BodyGates::new(0.7, 0.1, 0.2, 0.9, 0.3, 0.2)
    );
    assert_eq!(
        fit(&answers[..3], &[WEB, CODEX, CODEX]).unwrap(),
        BodyGates::new(0.95, 0.05, 0.05, 0.95, 0.1, 0.05)
    );
}

/// The whole question set, byte for byte: `tests/fixtures/python_questions.json` is Python's
/// `json.dumps(browser_router.questions())`, so any drift in the prompt text or key order fails here.
#[test]
fn the_questions_dump_byte_identical_to_python() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/python_questions.json");
    let python = std::fs::read_to_string(path).unwrap();
    assert_eq!(eve_jev::py_dumps(&questions()), python);
}
