//! test_decider.py against a scripted predict: the one-question contract, every answer
//! rejection rule, the head-budget trim, the three prompt styles, the overflow flag,
//! the available flag and the predict lock.

use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex as PlMutex;

use super::*;

// ---- fakes -------------------------------------------------------------------------

fn menu() -> IndexMap<String, String> {
    opts(&[
        ("1", "click radio button: Week"),
        ("2", "click button: Today"),
        ("3", "click button: Add Event"),
        ("reobserve", "the screen is still changing, look again"),
        ("abstain", "none of these advances the objective"),
    ])
}

fn opts(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn probs(pairs: &[(&str, Value)]) -> Value {
    Value::Object(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
}

fn probs_ok() -> Vec<(&'static str, Value)> {
    vec![("1", json!(0.7)), ("2", json!(0.2)), ("3", json!(0.05)), ("reobserve", json!(0.03)), ("abstain", json!(0.02))]
}

fn answer_full(choice: &str, probs: Value, confidence: f64, tokens: i64) -> Value {
    json!({"answers": {"pick": {"type": "choice", "choice": choice, "probabilities": probs,
                                "confidence": confidence, "action": {"act_probability": 0.5}}},
           "usage": {"input_tokens": tokens, "output_tokens": 0}})
}

fn answer(choice: &str, probs: Value) -> Value {
    answer_full(choice, probs, 0.4, 300)
}

type Calls = Arc<PlMutex<Vec<(Value, Value)>>>;

/// Serves scripted results in order; records every (state, questions) it was asked.
fn fake_predict(
    results: Vec<Value>,
    raise_first: Option<PredictError>,
) -> (impl Fn(&Value, &Value) -> Result<Value, PredictError> + Send + Sync + 'static, Calls) {
    let calls: Calls = Arc::new(PlMutex::new(Vec::new()));
    let queue = PlMutex::new(VecDeque::from(results));
    let raise = PlMutex::new(raise_first);
    let seen = calls.clone();
    let predict = move |state: &Value, questions: &Value| {
        seen.lock().push((state.clone(), questions.clone()));
        if let Some(e) = raise.lock().take() {
            return Err(e);
        }
        Ok(queue.lock().pop_front().expect("predict ran out of scripted results"))
    };
    (predict, calls)
}

fn clock() -> Box<Clock> {
    let t = PlMutex::new(0.0_f64);
    Box::new(move || {
        let mut t = t.lock();
        *t += 0.005;
        *t
    })
}

fn config() -> DeciderConfig {
    DeciderConfig { clock: clock(), ..DeciderConfig::default() }
}

fn decider_with(results: Vec<Value>, config: DeciderConfig) -> (Decider, Calls) {
    let (p, calls) = fake_predict(results, None);
    (Decider::new(p, config).unwrap(), calls)
}

fn decider(results: Vec<Value>) -> (Decider, Calls) {
    decider_with(results, config())
}

fn go(d: &Decider, options: &IndexMap<String, String>) -> Choice {
    d.choose("go", "A", "", options, &[]).unwrap()
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6 * b.abs().max(1e-12)
}

// ---- a valid pick --------------------------------------------------------------------

#[test]
fn test_valid_pick_carries_p_top_margin_and_metadata() {
    let (d, p) = decider(vec![answer_full("1", probs(&probs_ok()), 0.46, 300)]);
    let c = d.choose("switch to week view", "Calendar", "title: September 2026", &menu(), &[]).unwrap();
    assert!(c.id == "1" && c.error.is_empty());
    assert!(approx(c.p_top, 0.7) && approx(c.margin, 0.5));
    assert!(approx(c.confidence, 0.46) && c.k == 5 && c.input_tokens == 300);
    assert!(!c.overflow && c.dropped_for_budget == 0 && approx(c.ms, 5.0));
    // option order, not the model's
    assert_eq!(c.probabilities.keys().collect::<Vec<_>>(), menu().keys().collect::<Vec<_>>());
    let calls = p.lock();
    let (_state, questions) = &calls[0];
    let q = questions.as_object().unwrap();
    assert_eq!(q.keys().collect::<Vec<_>>(), vec!["pick"]);
    assert_eq!(q["pick"]["type"], "choice");
    assert_eq!(q["pick"]["criteria"], json!(menu()));
}

#[test]
fn test_state_has_goal_first_and_never_repeats_the_candidates() {
    let (d, p) = decider(vec![answer("1", probs(&probs_ok()))]);
    d.choose("switch to week view", "Calendar", "title: September 2026", &menu(), &["clicked Today".to_string()])
        .unwrap();
    let state = p.lock()[0].0.clone();
    assert_eq!(state.as_object().unwrap().keys().collect::<Vec<_>>(), vec!["goal", "app", "context", "recent"]);
    assert_eq!(
        state,
        json!({"goal": "switch to week view", "app": "Calendar", "context": "title: September 2026",
               "recent": ["clicked Today"]})
    );
    let text = state.to_string();
    assert!(!text.contains("Week") && !text.contains("Add Event"));
}

#[test]
fn test_tie_within_tolerance_is_accepted_with_zero_margin() {
    let p = probs(&[("1", json!(0.5)), ("2", json!(0.5)), ("3", json!(0.0)), ("reobserve", json!(0.0)),
                    ("abstain", json!(0.0))]);
    let (d, _) = decider(vec![answer("2", p)]);
    let c = go(&d, &menu());
    assert!(c.id == "2" && c.margin == 0.0 && c.error.is_empty());
}

// ---- rejection rules -----------------------------------------------------------------

fn with(key: &str, value: Value) -> Value {
    let mut p = probs_ok();
    match p.iter_mut().find(|(k, _)| *k == key) {
        Some(slot) => slot.1 = value,
        None => p.push((Box::leak(key.to_string().into_boxed_str()), value)),
    }
    probs(&p)
}

#[test]
fn test_rejected_answers_have_empty_id_and_a_reason() {
    let without_3 = probs(&probs_ok().into_iter().filter(|(k, _)| *k != "3").collect::<Vec<_>>());
    // NaN and infinity have no JSON form: serde_json carries them as null, rejected by the same rule.
    let cases: Vec<(&str, Value, &str)> = vec![
        ("9", probs(&probs_ok()), "not an option"),
        ("1", without_3, "do not cover"),
        ("1", with("extra", json!(0.0)), "do not cover"),
        ("1", with("2", json!(f64::NAN)), "not in [0, 1]"),
        ("1", with("2", json!(f64::INFINITY)), "not in [0, 1]"),
        ("1", with("2", json!(1.5)), "not in [0, 1]"),
        ("1", with("2", json!(-0.1)), "not in [0, 1]"),
        ("1", with("2", json!("0.2")), "not in [0, 1]"),
        ("2", probs(&probs_ok()), "not the most probable"),
    ];
    for (choice, p, why) in cases {
        let (d, _) = decider(vec![answer(choice, p)]);
        let c = go(&d, &menu());
        assert!(c.id.is_empty() && c.error.contains(why) && c.k == 5, "{choice} {why}: {c:?}");
    }
}

#[test]
fn test_malformed_results_are_rejected_not_raised() {
    for result in [json!("nope"), json!({}), json!({"answers": {}}), json!({"answers": {"pick": "x"}}),
                   json!({"answers": {"pick": {}}})] {
        let (d, _) = decider(vec![result]);
        let c = go(&d, &menu());
        assert!(c.id.is_empty() && !c.error.is_empty());
    }
}

#[test]
fn test_predict_raising_becomes_an_error_choice() {
    let (p, _) = fake_predict(vec![], Some(PredictError::new("RuntimeError", "Metal device lost")));
    let d = Decider::new(p, config()).unwrap();
    let c = go(&d, &menu());
    assert!(c.id.is_empty() && c.error == "predict raised RuntimeError: Metal device lost");
    assert!(c.ms > 0.0 && c.k == 5);
}

#[test]
fn test_caller_contract_violations_raise() {
    let (d, _) = decider(vec![]);
    let e = d.choose("go", "A", "", &opts(&[("1", "click button: Today")]), &[]).unwrap_err();
    assert!(e.to_string().contains("reserved"));
    assert_eq!(e.to_string(), "options must include the reserved keys ('reobserve', 'abstain')");
    let reserved_only: IndexMap<String, String> =
        menu().into_iter().filter(|(k, _)| RESERVED.contains(&k.as_str())).collect();
    let e = d.choose("go", "A", "", &reserved_only, &[]).unwrap_err();
    assert!(e.to_string().contains("at least one real"));
    let e = d.choose("go", "A", "", &IndexMap::new(), &[]).unwrap_err();
    assert!(e.to_string().contains("non-empty"));
    assert_eq!(e.kind(), "ValueError");
    let e = Decider::new(|_: &Value, _: &Value| Ok(json!({})), DeciderConfig { style: "verbose".into(), ..config() })
        .unwrap_err();
    assert!(e.to_string().contains("style"));
    assert_eq!(e.to_string(), "style must be one of ('jev', 'compact', 'hinted'), not 'verbose'");
}

// ---- overflow ---------------------------------------------------------------------------

#[test]
fn test_overflow_flag_when_the_state_fills_the_sequence() {
    let (d, _) = decider_with(vec![answer_full("1", probs(&probs_ok()), 0.4, 1024)],
                              DeciderConfig { max_len: 1024, ..config() });
    assert!(d.choose("go", "A", &"x".repeat(5000), &menu(), &[]).unwrap().overflow);
    let (d, _) = decider_with(vec![answer_full("1", probs(&probs_ok()), 0.4, 1023)],
                              DeciderConfig { max_len: 1024, ..config() });
    assert!(!go(&d, &menu()).overflow);
}

// ---- the head-budget trim --------------------------------------------------------------

#[test]
fn test_trim_drops_the_last_real_option_first_and_never_the_reserved_two() {
    let options = opts(&[("1", "a b"), ("2", "c d"), ("3", "e f"), ("reobserve", "look again"), ("abstain", "none")]);
    // word counts: each real option costs 1 + 2 = 3, reobserve 3, abstain 2 → 14 total
    let (kept, dropped) = trim_options(&options, 14, None);
    assert!(kept == options && dropped == 0);
    let (kept, dropped) = trim_options(&options, 11, None);
    assert!(kept.keys().collect::<Vec<_>>() == ["1", "2", "reobserve", "abstain"] && dropped == 1);
    let (kept, dropped) = trim_options(&options, 8, None);
    assert!(kept.keys().collect::<Vec<_>>() == ["1", "reobserve", "abstain"] && dropped == 2);
    // never below one real option
    let (kept, dropped) = trim_options(&options, 1, None);
    assert!(kept.keys().collect::<Vec<_>>() == ["1", "reobserve", "abstain"] && dropped == 2);
}

#[test]
fn test_choose_trims_before_predict_and_reports_the_count() {
    let long = vec!["word"; 60].join(" "); // capped at 48 tokens + 1 marker
    let options = opts(&[("1", &long), ("2", &long), ("3", &long), ("4", &long), ("5", &long),
                         ("reobserve", "look again"), ("abstain", "none")]);
    // two capped options (49 each) + "look again" (3) + "none" (2) = 103 head tokens exactly
    let fits_two = 16 + instruction_tokens("compact", "", None, None).unwrap() as i64 + 103;
    let (d, p) = decider_with(
        vec![answer("1", probs(&[("1", json!(0.9)), ("2", json!(0.05)), ("reobserve", json!(0.03)),
                                 ("abstain", json!(0.02))]))],
        DeciderConfig { head_max_len: fits_two, ..config() },
    );
    let c = go(&d, &options);
    let sent = p.lock()[0].1["pick"]["criteria"].clone();
    assert_eq!(sent.as_object().unwrap().keys().collect::<Vec<_>>(), vec!["1", "2", "reobserve", "abstain"]);
    assert!(c.id == "1" && c.dropped_for_budget == 3 && c.k == 4);
    let (d, p) = decider_with(
        vec![answer("1", probs(&[("1", json!(0.9)), ("reobserve", json!(0.07)), ("abstain", json!(0.03))]))],
        DeciderConfig { head_max_len: fits_two - 1, ..config() },
    );
    let c = go(&d, &options);
    let sent = p.lock()[0].1["pick"]["criteria"].clone();
    assert_eq!(sent.as_object().unwrap().keys().collect::<Vec<_>>(), vec!["1", "reobserve", "abstain"]);
    assert!(c.dropped_for_budget == 4 && c.k == 3);
}

#[test]
fn test_option_and_instruction_token_estimates_use_the_injected_tokenizer() {
    let calls: Arc<PlMutex<Vec<String>>> = Arc::new(PlMutex::new(Vec::new()));
    let seen = calls.clone();
    let tokenize = move |text: &str| {
        seen.lock().push(text.to_string());
        7
    };
    assert_eq!(option_tokens("click button: Week", Some(&tokenize)), 8);
    // the 48-token cap
    assert_eq!(option_tokens(&vec!["w"; 200].join(" "), Some(&|_: &str| 200)), 49);
    assert_eq!(instruction_tokens("compact", "", None, Some(&tokenize)).unwrap(), 7);
    let calls = calls.lock();
    assert!(calls[0] == " click button: Week" && calls.last().unwrap().starts_with("choice question: "));
    assert_eq!(word_count("a  b c"), 3);
    assert_eq!(word_count("a\u{1f}b\u{a0}c"), 3); // Python's str.split() separators
}

#[test]
fn test_decider_uses_its_injected_tokenizer_for_the_budget() {
    // Every text costs 10 tokens: the head is 10, each option 11. With head_max_len 16 + 10 + 33
    // only three options fit, so one real option of the four goes.
    let options = opts(&[("1", "a"), ("2", "b"), ("reobserve", "c"), ("abstain", "d")]);
    let (d, p) = decider_with(
        vec![answer("1", probs(&[("1", json!(0.9)), ("reobserve", json!(0.07)), ("abstain", json!(0.03))]))],
        DeciderConfig { head_max_len: 16 + 10 + 33, tokenize: Some(Box::new(|_: &str| 10)), ..config() },
    );
    let c = go(&d, &options);
    assert!(c.id == "1" && c.dropped_for_budget == 1 && c.k == 3, "{c:?}");
    assert_eq!(p.lock()[0].1["pick"]["criteria"].as_object().unwrap().len(), 3);
}

// ---- styles ---------------------------------------------------------------------------

#[test]
fn test_three_styles_render_distinct_instructions() {
    let rendered: IndexMap<&str, String> =
        STYLES.iter().map(|s| (*s, render_instructions(s, "switch to week view", &menu()).unwrap())).collect();
    assert_eq!(STYLES, ["jev", "compact", "hinted"]);
    assert_eq!(rendered.values().collect::<HashSet<_>>().len(), 3);
    assert!(rendered["jev"].ends_with("Goal: switch to week view"));
    assert!(rendered["compact"].contains("reobserve") && rendered["compact"].contains("abstain"));
    assert!(rendered["hinted"].starts_with(&rendered["compact"]));
    // only Week shares a goal token
    assert!(rendered["hinted"].ends_with("matches: 1"));
    assert_eq!(render_instructions("hinted", "do something else", &menu()).unwrap(), rendered["compact"]);
    assert!(render_instructions("loud", "x", &menu()).is_err());
}

#[test]
fn test_hinted_lists_every_overlapping_real_option_and_never_the_reserved() {
    let options = opts(&[
        ("1", "click button: Today"),
        ("2", "click row: Today's Reminders"),
        ("3", "click button: Week"),
        ("reobserve", "the screen is still changing, look again"),
        ("abstain", "none of these advances the objective"),
    ]);
    let text = render_instructions("hinted", "show today's reminders", &options).unwrap();
    assert!(text.ends_with("matches: 1, 2"));
    let q = render_question("hinted", "show today's reminders", &options).unwrap();
    assert_eq!(q, json!({"type": "choice", "instructions": text, "criteria": options}));
    let keys: Vec<&String> = q.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["type", "instructions", "criteria"]);
}

#[test]
fn test_decider_sends_its_style_and_the_question_id_is_pick() {
    for style in STYLES {
        let (d, p) = decider_with(vec![answer("1", probs(&probs_ok()))],
                                  DeciderConfig { style: style.into(), ..config() });
        d.choose("switch to week view", "Calendar", "", &menu(), &[]).unwrap();
        let q = p.lock()[0].1.clone();
        assert_eq!(q.as_object().unwrap().keys().collect::<Vec<_>>(), vec!["pick"]);
        assert_eq!(q["pick"]["instructions"], render_instructions(style, "switch to week view", &menu()).unwrap());
    }
}

#[test]
fn test_jev_instructions_collapse_whitespace_in_the_goal() {
    let text = render_instructions("jev", "  switch\tto\n week   view ", &menu()).unwrap();
    assert_eq!(text, "Which single element should be acted on next to accomplish the goal? Goal: switch to week view");
}

// ---- the available flag ------------------------------------------------------------------

#[test]
fn test_a_bare_decider_is_not_available_until_a_loader_warms_it() {
    let (d, _) = decider(vec![]);
    assert!(!d.available && d.load_ms == 0.0 && d.warm_ms == 0.0);
    assert_eq!((d.max_len, d.head_max_len, d.style.as_str()), (1024, 256, "compact"));
}

// ---- the predict lock ---------------------------------------------------------------------

#[test]
fn test_predict_runs_under_the_lock() {
    let seen: Arc<PlMutex<Vec<bool>>> = Arc::new(PlMutex::new(Vec::new()));
    let holder: Arc<OnceLock<Arc<Decider>>> = Arc::new(OnceLock::new());
    let (seen2, holder2) = (seen.clone(), holder.clone());
    let predict = move |_: &Value, _: &Value| {
        seen2.lock().push(holder2.get().unwrap().predict_locked());
        Ok(answer("1", probs(&probs_ok())))
    };
    let d = Arc::new(Decider::new(predict, DeciderConfig::default()).unwrap());
    holder.set(d.clone()).unwrap();
    go(&d, &menu());
    assert_eq!(*seen.lock(), vec![true]);
    assert!(!d.predict_locked());
}

#[test]
fn test_predict_panicking_becomes_an_error_choice_and_frees_the_lock() {
    let d = Decider::new(|_: &Value, _: &Value| -> Result<Value, PredictError> { panic!("boom") }, config()).unwrap();
    let c = go(&d, &menu());
    assert!(c.id.is_empty() && c.error == "predict raised panic: boom" && c.k == 5);
    assert!(!d.predict_locked());
}

#[test]
fn test_predict_error_is_clipped_to_200_chars() {
    let (p, _) = fake_predict(vec![], Some(PredictError::new("JevError", "x".repeat(500))));
    let d = Decider::new(p, config()).unwrap();
    let c = go(&d, &menu());
    assert_eq!(c.error.chars().count(), 200);
    assert!(c.error.starts_with("predict raised JevError: xxx"));
}

// ---- render_option: the one wording the lane and the eval share ----------------------------

fn ns(label: Option<&str>, role: &str, value: Option<&str>) -> OptionControl {
    OptionControl { label: label.map(Into::into), role: Some(role.into()), value: value.map(Into::into) }
}

#[test]
fn test_render_option_label_role_value() {
    assert_eq!(render_option(&ns(Some("Week"), "radio button", Some(""))), "Week (radio button)");
    assert_eq!(render_option(&ns(Some("Month"), "radio button", Some("selected"))), "Month (radio button, selected)");
    assert_eq!(render_option(&ns(Some("  Add   Event "), "button", None)), "Add Event (button)");
    let long = render_option(&ns(Some(&"x".repeat(60)), "link", Some("")));
    assert_eq!(long, "x".repeat(40) + " (link)"); // the label is capped, the role never cut
    assert_eq!(render_option(&ns(Some("Loose"), "", Some(""))), "Loose");
}

// ---- parity details -----------------------------------------------------------------------

#[test]
fn test_rejection_messages_match_python_text() {
    let cases: Vec<(Value, &str)> = vec![
        (json!("nope"), "predict returned no dict"),
        (json!({"answers": {"pick": "x"}}), "predict returned no answer"),
        (answer("9", probs(&probs_ok())), "choice '9' is not an option"),
        (json!({"answers": {"pick": {"choice": 3}}}), "choice 3 is not an option"),
        (json!({"answers": {"pick": {}}}), "choice None is not an option"),
        (json!({"answers": {"pick": {"choice": "1", "probabilities": [0.1]}}}), "probabilities do not cover the options"),
        (answer("1", with("2", json!(true))), "probability of '2' is not in [0, 1]"),
        (answer("2", probs(&probs_ok())), "choice is not the most probable option"),
    ];
    for (result, why) in cases {
        let (d, _) = decider(vec![result]);
        assert_eq!(go(&d, &menu()).error, why);
    }
}

#[test]
fn test_rejection_keeps_usage_and_the_not_most_probable_keeps_probabilities() {
    let (d, _) = decider(vec![answer_full("2", probs(&probs_ok()), 0.4, 2000)]);
    let c = go(&d, &menu());
    assert!(c.input_tokens == 2000 && c.overflow && approx(c.probabilities["1"], 0.7));
    let (d, _) = decider(vec![answer_full("9", probs(&probs_ok()), 0.4, 300)]);
    let c = go(&d, &menu());
    assert!(c.input_tokens == 300 && c.probabilities.is_empty());
}

#[test]
fn test_input_tokens_must_be_an_int() {
    for (tokens, want) in [(json!(300.0), 0), (json!(true), 0), (json!("300"), 0), (json!(12), 12)] {
        let mut result = answer("1", probs(&probs_ok()));
        result["usage"]["input_tokens"] = tokens;
        let (d, _) = decider(vec![result]);
        assert_eq!(go(&d, &menu()).input_tokens, want);
    }
}

#[test]
fn test_confidence_outside_the_unit_interval_logs_as_zero() {
    let mut result = answer("1", probs(&probs_ok()));
    result["answers"]["pick"]["confidence"] = json!(3.0);
    let (d, _) = decider(vec![result]);
    let c = go(&d, &menu());
    assert!(c.error.is_empty() && c.confidence == 0.0);
}

#[test]
fn test_objective_tokens_casefold_and_stop_words() {
    let words = objective_tokens("Open THE Straße, ﬁle it and go to #42 now");
    let want: HashSet<String> = ["open", "strasse", "file"].iter().map(|s| s.to_string()).collect();
    assert_eq!(words, want);
    assert!(objective_tokens("a to of").is_empty());
}

#[test]
fn test_warmup_menu_shape() {
    assert_eq!(WARMUP_OPTIONS.len(), 12);
    assert_eq!(WARMUP_OPTIONS["2"], "click radio button: Week");
    assert_eq!(WARMUP_OPTIONS["10"], "click button: Inspector");
    assert_eq!(WARMUP_OPTIONS.keys().last().unwrap(), "abstain");
    assert!(WARMUP_CONTEXT.starts_with("title: September 2026; focused: radio button Month; text: Monday 1 Tuesday 2"));
    assert!(WARMUP_CONTEXT.ends_with("Saturday 41 Sunday 42"));
}

#[test]
fn test_py_repr_matches_python() {
    assert_eq!(py_str_repr("it's"), "\"it's\"");
    assert_eq!(py_str_repr("a\nb\\"), "'a\\nb\\\\'");
    assert_eq!(py_str_repr("\u{7}"), "'\\x07'");
    // Zs other than space, Cf, Co and Cn are escaped; letters outside ASCII are not.
    assert_eq!(py_str_repr("\u{a0}\u{200b}\u{e000}\u{378}é"), "'\\xa0\\u200b\\ue000\\u0378é'");
    assert_eq!(py_repr(&json!(1e-5)), "1e-05");
    assert_eq!(py_repr(&json!(1.0)), "1.0");
    assert_eq!(py_repr(&json!(1e16)), "1e+16");
    assert_eq!(py_repr(&json!([1, "a", null, true])), "[1, 'a', None, True]");
    assert_eq!(py_repr(&json!({"k": 0.5})), "{'k': 0.5}");
}
