//! Ports of bridge/tests/test_typed_ask_laya_request.py and of the typed_ask parts of the tests that
//! exercise it from elsewhere (test_command_risk, test_jev_grouped, test_jev_step, test_quit_reflex,
//! test_task_router, test_voice_fast_head).

use std::cell::RefCell;
use std::collections::HashMap;

use anyhow::Result;
use eve_typed_ask as ta;
use eve_typed_ask::{PredictError, StepAnswer, STEP_NONE};
use indexmap::IndexMap;
use serde_json::{json, Map, Value};

fn clock() -> f64 {
    0.0
}

fn fail(kind: &str, msg: &str) -> anyhow::Error {
    PredictError::new(kind, msg).into()
}

// ---- test_typed_ask_laya_request.py ------------------------------------------------------------------

const APPS: [&str; 6] = ["Safari", "Google Chrome", "Preview", "System Settings", "Notes", "Warp"];

#[test]
fn test_shortlist_is_close_names_only() {
    let l = ta::LAYA_APP_SHORTLIST;
    assert_eq!(ta::app_shortlist("Get me Preview.", &APPS, l), vec!["Preview"]);
    assert_eq!(ta::app_shortlist("can you switch over to google chrome", &APPS, l)[0], "Google Chrome");
    assert_eq!(ta::app_shortlist("open safary", &APPS, l), vec!["Safari"]); // a close spelling
    assert!(ta::app_shortlist("play some jazz", &APPS, l).is_empty());
}

#[test]
fn test_laya_is_asked_short_choices_and_read_as_a_request_answer() {
    let seen: RefCell<Option<(Value, Value)>> = RefCell::new(None);
    let predict = |state: &Value, questions: &Value| -> Result<Value> {
        *seen.borrow_mut() = Some((state.clone(), questions.clone()));
        Ok(json!({"answers": {"kind": {"choice": "open", "probabilities": {"open": 0.9, "send": 0.04, "write": 0.02}},
                              "app": {"choice": "app0", "probabilities": {"app0": 0.97, "none": 0.03}}}}))
    };
    let a = ta::ask_laya_request(&predict, "Get me Preview.", &APPS, &ta::perf_counter);
    let (state, questions) = seen.borrow().clone().unwrap();
    assert_eq!(state, json!({"owner said": "Get me Preview."}));
    assert!(questions["kind"]["criteria"]
        .as_object()
        .unwrap()
        .values()
        .all(|v| v.as_str().unwrap().split_whitespace().count() <= 5));
    assert!(a.app == "Preview" && a.launch_only == 0.9 && (a.risky - 0.06).abs() < 1e-9);
}

#[test]
fn test_no_shortlist_means_no_app_question_and_a_failure_abstains() {
    let asked: RefCell<Vec<Value>> = RefCell::new(Vec::new());
    let predict = |_s: &Value, questions: &Value| -> Result<Value> {
        asked.borrow_mut().push(questions.clone());
        Ok(json!({"answers": {}}))
    };
    assert_eq!(ta::ask_laya_request(&predict, "play some jazz", &APPS, &ta::perf_counter).app, "");
    assert!(asked.borrow()[0].get("app").is_none());

    let boom = |_s: &Value, _q: &Value| -> Result<Value> { Err(fail("RuntimeError", "down")) };
    let a = ta::ask_laya_request(&boom, "Get me Preview.", &APPS, &ta::perf_counter);
    assert!(!a.error.is_empty() && ta::decide_request(&a, &ta::JEV_REQUEST_GATES) == "other");
    assert_eq!(a.error, "RuntimeError: down");
}

// ---- test_command_risk.py (the typed_ask parts) ----------------------------------------------------------

fn command_answers(destroys: f64, escapes: f64, publishes: f64, secrets: f64) -> Value {
    json!({"answers": {"destroys": {"type": "noul", "noul": destroys}, "escapes": {"type": "noul", "noul": escapes},
                       "publishes": {"type": "noul", "noul": publishes}, "secrets": {"type": "noul", "noul": secrets}}})
}

#[test]
fn test_the_questions_are_absolute_nouls_over_a_redacted_command() {
    let q = ta::jev_command_questions();
    let qm = q.as_object().unwrap();
    assert_eq!(qm.keys().collect::<Vec<_>>(), ["destroys", "escapes", "publishes", "secrets"]);
    assert!(qm.values().all(|v| v["type"] == "noul" && v["instructions"].as_str().unwrap().contains("Do NOT count")));
    let seen: RefCell<Vec<(Value, Value)>> = RefCell::new(Vec::new());
    let predict = |state: &Value, questions: &Value| -> Result<Value> {
        seen.borrow_mut().push((state.clone(), questions.clone()));
        Ok(command_answers(0.9, 0.1, 0.2, 0.0))
    };
    let a = ta::ask_jev_command(&predict, "Bash", "rm -rf build/  --token=abcdefghijkl", "/Users/g/secret-repo", &clock);
    let (state, questions) = seen.borrow()[0].clone();
    assert_eq!(questions, q);
    assert!(state["tool"] == "Bash" && state["folder"] == "secret-repo" && !state.to_string().contains("/Users"));
    assert_eq!(state["command"], "rm -rf build/ --token=<redacted>"); // whitespace folded, secret gone
    assert_eq!(state["about"], ta::COMMAND_ABOUT);
    assert_eq!((a.destroys, a.escapes, a.publishes, a.secrets, a.error.as_str()), (0.9, 0.1, 0.2, 0.0, ""));
}

#[test]
fn test_secrets_never_leave_in_a_command() {
    let red = ta::redact_command;
    assert_eq!(
        red(r#"curl -H "Authorization: Bearer abcdefgh12345678" https://api.example.com"#),
        r#"curl -H "Authorization: Bearer <redacted>" https://api.example.com"#
    );
    assert_eq!(
        red("export OPENAI_API_KEY=sk-abcdefghijklmnop && python x.py"),
        "export OPENAI_API_KEY=<redacted> && python x.py"
    );
    assert_eq!(red("AWS_SECRET_ACCESS_KEY='abc/def+ghi' aws s3 ls"), "AWS_SECRET_ACCESS_KEY=<redacted> aws s3 ls");
    assert_eq!(red("gh auth login --with-token ghp_abcdefghijklmnopqrstuvwxyz"), "gh auth login --with-token <redacted>");
    assert_eq!(red("mysql --password=hunter2 db"), "mysql --password=<redacted> db");
    assert_eq!(red("psql -p 5432 --password mypw"), "psql -p 5432 --password <redacted>");
    // positive control: an ordinary command is exactly itself, so the redactor is not hiding everything
    for plain in ["pytest -q tests/", "git commit -m 'fix KEY handling'", "ls -la ~/.config", "cat .env.example"] {
        assert_eq!(red(plain), plain);
    }
    // a long blob (a SHA, a JWT) goes; the judgement does not depend on it
    assert_eq!(red("git log 1234567890abcdef1234567890abcdef12345678"), "git log <redacted>");
}

#[test]
fn test_any_gate_reached_is_risky_and_an_error_is_unknown() {
    let g = ta::CommandGates { destroys: 0.7, escapes: 0.7, publishes: 0.7, secrets: 0.7 };
    let safe = ta::CommandAnswer::new(0.1, 0.2, 0.3, 0.05);
    assert_eq!(ta::decide_command(&safe, &g), ta::CommandVerdict::new("safe", "", safe.clone()));
    let two = ta::CommandAnswer::new(0.9, 0.1, 0.75, 0.0);
    let v = ta::decide_command(&two, &g);
    assert!(v.decision == "risky" && v.why == "destroys data, publishes or sends");
    let edge = ta::CommandAnswer::new(0.0, 0.7, 0.0, 0.0); // at the gate counts
    assert_eq!(ta::decide_command(&edge, &g).decision, "risky");
    let err = ta::CommandAnswer { error: "HTTP 503".into(), ..ta::CommandAnswer::new(0.0, 0.0, 0.0, 0.0) };
    assert_eq!(ta::decide_command(&err, &g), ta::CommandVerdict::new("unknown", "HTTP 503", err.clone()));

    let boom = |_s: &Value, _q: &Value| -> Result<Value> { Err(fail("RuntimeError", "no network")) };
    let a = ta::ask_jev_command(&boom, "Bash", "ls", "/r", &clock);
    assert!(a.error.starts_with("RuntimeError") && ta::decide_command(&a, &g).decision == "unknown");
    let empty = |_s: &Value, _q: &Value| -> Result<Value> { Ok(json!({})) };
    assert_eq!(ta::ask_jev_command(&empty, "Bash", "ls", "/r", &clock).error, "predict returned no answers");
    let asker = ta::make_jev_command_asker(
        |_s: &Value, _q: &Value| -> Result<Value> { Ok(command_answers(0.0, 0.0, 0.95, 0.0)) },
        clock,
        Some(g),
    );
    assert_eq!(asker("Bash", "git push --force", "/r").decision, "risky");
}

#[test]
fn test_fit_command_gates_allows_no_miss() {
    let risky = [ta::CommandAnswer::new(0.6, 0.0, 0.0, 0.0), ta::CommandAnswer::new(0.0, 0.0, 0.85, 0.0)];
    let safe = [ta::CommandAnswer::new(0.5, 0.1, 0.0, 0.0), ta::CommandAnswer::new(0.0, 0.0, 0.8, 0.0)];
    let all: Vec<_> = risky.iter().chain(safe.iter()).cloned().collect();
    let g = ta::fit_command_gates(&all, &[true, true, false, false]);
    assert!(risky.iter().all(|a| ta::decide_command(a, &g).decision == "risky"));
    assert!(g.destroys == 0.6 && g.publishes == 0.8); // the strictest gate that still catches every risky one
    assert!(g.escapes == 0.9 && g.secrets == 0.9); // unused properties sit high: they cry no wolf
    assert_eq!(ta::decide_command(&safe[1], &g).decision, "risky"); // the one unavoidable wolf
}

// ---- test_jev_grouped.py (the typed_ask parts) --------------------------------------------------------

fn menu(n: usize) -> IndexMap<String, String> {
    (1..=n).map(|i| (i.to_string(), format!("Control {i} (button)"))).collect()
}

fn choice_answer(pick: &str, keys: &[String], p: f64) -> Value {
    let rest = (1.0 - p) / (keys.len().saturating_sub(1).max(1)) as f64;
    let probs: Map<String, Value> = keys.iter().map(|k| (k.clone(), json!(if k == pick { p } else { rest }))).collect();
    json!({"choice": pick, "probabilities": probs, "confidence": p})
}

/// A predict that answers each choice question with `pick(qid, criteria)`, nouls with fixed values.
struct Fake<F: Fn(&str, &[String]) -> String> {
    pick: F,
    nouls: (f64, f64, f64),
    final_nouls: Option<(f64, f64, f64)>,
    calls: RefCell<Vec<(Value, Value)>>,
}

impl<F: Fn(&str, &[String]) -> String> Fake<F> {
    fn new(pick: F) -> Self {
        Self { pick, nouls: (0.9, 0.1, 0.1), final_nouls: None, calls: RefCell::new(Vec::new()) }
    }

    fn call(&self, state: &Value, questions: &Value) -> Result<Value> {
        self.calls.borrow_mut().push((state.clone(), questions.clone()));
        let qs = questions.as_object().unwrap();
        let nouls = match self.final_nouls {
            Some(f) if qs.contains_key("target") => f,
            _ => self.nouls,
        };
        let mut answers = Map::new();
        for (qid, q) in qs {
            if q["type"] == "choice" {
                let keys: Vec<String> = q["criteria"].as_object().unwrap().keys().cloned().collect();
                answers.insert(qid.clone(), choice_answer(&(self.pick)(qid, &keys), &keys, 0.95));
            }
        }
        answers.insert("present".into(), json!({"noul": nouls.0}));
        answers.insert("already_done".into(), json!({"noul": nouls.1}));
        answers.insert("risky".into(), json!({"noul": nouls.2}));
        Ok(json!({"answers": answers}))
    }
}

const NO_RECENT: &[&str] = &[];

fn grouped<F: Fn(&str, &[String]) -> String>(fake: &Fake<F>, step: &str, options: &IndexMap<String, String>) -> StepAnswer {
    ta::ask_jev_step_grouped(&|s: &Value, q: &Value| fake.call(s, q), step, "App", "w", options, NO_RECENT, &clock, ta::STEP_GROUP)
}

fn pick_if(want: &'static str) -> impl Fn(&str, &[String]) -> String {
    move |_q: &str, keys: &[String]| if keys.iter().any(|k| k == want) { want.into() } else { STEP_NONE.into() }
}

#[test]
fn test_a_menu_that_fits_one_group_is_asked_exactly_as_before() {
    let options = menu(25);
    let (old, new) = (Fake::new(|_q: &str, _k: &[String]| "7".to_string()), Fake::new(|_q: &str, _k: &[String]| "7".to_string()));
    let a = ta::ask_jev_step(&|s: &Value, q: &Value| old.call(s, q), "press control 7", "App", "w", &options, NO_RECENT, &clock);
    let b = grouped(&new, "press control 7", &options);
    assert!(*old.calls.borrow() == *new.calls.borrow() && new.calls.borrow().len() == 1); // the same single request
    assert!(a == b && b.target == "7" && b.requests == 1);
}

#[test]
fn test_a_long_menu_is_one_request_of_parallel_groups_then_one_final() {
    let options = menu(80);
    let fake = Fake::new(pick_if("63"));
    let a = grouped(&fake, "press control 63", &options);
    let calls = fake.calls.borrow();
    assert!(calls.len() == 2 && a.requests == 2 && a.k == 81);
    let first = calls[0].1.as_object().unwrap();
    let mut parts: Vec<&String> = first.keys().filter(|q| q.starts_with("target_part")).collect();
    parts.sort();
    assert_eq!(parts, ["target_part0", "target_part1", "target_part2", "target_part3"]);
    assert!(["present", "already_done", "risky"].iter().all(|k| first.contains_key(*k)));
    let sizes: Vec<usize> = parts.iter().map(|p| first[*p]["criteria"].as_object().unwrap().len() - 1).collect();
    assert_eq!(sizes, [25, 25, 25, 5]); // each group plus its none
    assert!(parts.iter().all(|p| first[*p]["criteria"][STEP_NONE] == ta::STEP_PART_NONE));
    assert_eq!(calls[0].0["controls"].as_array().unwrap().len(), 80); // the nouls see the whole window
    assert!(a.target == "63" && ta::JEV_STEP_GATES.decide(&a) == "63");
}

#[test]
fn test_the_final_round_offers_each_groups_top_three_in_menu_order() {
    let fake = Fake::new(pick_if("40"));
    grouped(&fake, "press control 40", &menu(60));
    let calls = fake.calls.borrow();
    let offered: Vec<&String> =
        calls[1].1["target"]["criteria"].as_object().unwrap().keys().filter(|k| *k != STEP_NONE).collect();
    assert!(offered.len() == 9 && offered.iter().any(|k| *k == "40")); // three groups × three finalists
    let mut sorted = offered.clone();
    sorted.sort_by_key(|k| k.parse::<usize>().unwrap());
    assert_eq!(offered, sorted); // menu order, not probability order
}

#[test]
fn test_every_group_answering_none_abstains_without_a_second_request() {
    let fake = Fake::new(|_q: &str, _k: &[String]| STEP_NONE.to_string());
    let a = grouped(&fake, "press the moon", &menu(70));
    assert!(fake.calls.borrow().len() == 1 && a.target == STEP_NONE);
    assert_eq!(ta::JEV_STEP_GATES.decide(&a), STEP_NONE);
}

#[test]
fn test_risk_from_the_first_round_is_kept_even_if_the_final_reads_it_lower() {
    let mut fake = Fake::new(pick_if("30"));
    fake.nouls = (0.9, 0.1, 0.8);
    fake.final_nouls = Some((0.9, 0.1, 0.1));
    let a = grouped(&fake, "delete control 30", &menu(40));
    assert!(a.risky == 0.8 && ta::JEV_STEP_GATES.decide(&a) == "confirm");
}

#[test]
fn test_an_answer_outside_its_group_is_an_error_and_never_a_press() {
    let fake = Fake::new(|_q: &str, _k: &[String]| "999".to_string());
    let a = grouped(&fake, "x", &menu(30));
    assert!(!a.error.is_empty() && a.target.is_empty() && ta::JEV_STEP_GATES.decide(&a) == STEP_NONE);
    assert_eq!(fake.calls.borrow().len(), 1);
    assert_eq!(a.error, "target_part0 '999' is not an option");
}

#[test]
fn test_a_failing_predict_abstains() {
    let boom = |_s: &Value, _q: &Value| -> Result<Value> { Err(fail("TimeoutError", "slow")) };
    let a = ta::ask_jev_step_grouped(&boom, "x", "App", "w", &menu(30), NO_RECENT, &clock, ta::STEP_GROUP);
    assert!(a.error.starts_with("TimeoutError") && ta::JEV_STEP_GATES.decide(&a) == STEP_NONE);
    // an io timeout with no Python name reads as Python's TimeoutError too
    let io = |_s: &Value, _q: &Value| -> Result<Value> {
        Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "slow").into())
    };
    let b = ta::ask_jev_step_grouped(&io, "x", "App", "w", &menu(30), NO_RECENT, &clock, ta::STEP_GROUP);
    assert_eq!(b.error, "TimeoutError: slow");
}

#[test]
fn test_the_switch_picks_the_asker_and_its_menu() {
    let env = |v: Option<&str>| -> HashMap<String, String> {
        v.map(|v| HashMap::from([(ta::GROUPS_ENV.to_string(), v.to_string())])).unwrap_or_default()
    };
    let predict = |_s: &Value, _q: &Value| -> Result<Value> { Ok(json!({})) };
    let on = ta::make_step_asker(predict, clock, Some(&env(Some("1"))));
    let off = ta::make_step_asker(predict, clock, Some(&env(Some("0"))));
    assert!(on.grouped && on.max_options == Some(ta::GROUPED_MAX_OPTIONS));
    assert!(!off.grouped && off.max_options.is_none());
    let unset = ta::make_step_asker(predict, clock, Some(&env(None)));
    assert_eq!(unset.grouped, ta::GROUPS_DEFAULT);
    for (raw, want) in [(" YES ", true), ("On", true), ("off", false), ("maybe", ta::GROUPS_DEFAULT)] {
        assert_eq!(ta::groups_on(Some(&env(Some(raw)))), want, "{raw}");
    }
}

#[test]
fn test_the_step_asker_runs_the_function_the_switch_picked() {
    let fake = Fake::new(pick_if("63"));
    let env = HashMap::from([(ta::GROUPS_ENV.to_string(), "1".to_string())]);
    let on = ta::make_step_asker(|s: &Value, q: &Value| fake.call(s, q), clock, Some(&env));
    let a = on.ask("press control 63", "App", "w", &menu(80), NO_RECENT);
    assert!(a.target == "63" && a.requests == 2);
    let fake = Fake::new(pick_if("63"));
    let off = ta::make_step_asker(|s: &Value, q: &Value| fake.call(s, q), clock, Some(&HashMap::new()));
    let b = off.ask("press control 63", "App", "w", &menu(80), NO_RECENT);
    assert!(b.target == "63" && b.requests == 1 && b.k == 81);
}

// ---- test_jev_step.py (the asker and its gates) ----------------------------------------------------------

fn step_envelope(target: &str, options: &[String], p: f64) -> Value {
    let mut keys: Vec<String> = options.to_vec();
    keys.push(STEP_NONE.into());
    let rest = (1.0 - p) / (keys.len() - 1).max(1) as f64;
    let probs: Map<String, Value> = keys.iter().map(|k| (k.clone(), json!(if k == target { p } else { rest }))).collect();
    json!({"answers": {
        "target": {"type": "choice", "choice": target, "probabilities": probs},
        "present": {"type": "noul", "noul": 0.95},
        "already_done": {"type": "noul", "noul": 0.05},
        "risky": {"type": "noul", "noul": 0.05}}})
}

/// An asker that picks the option whose text starts with `label`, and records what it was sent.
fn saying<'a>(label: &'static str, seen: &'a RefCell<Vec<(Value, Value)>>) -> impl Fn(&Value, &Value) -> Result<Value> + 'a {
    move |state: &Value, questions: &Value| {
        seen.borrow_mut().push((state.clone(), questions.clone()));
        let crit = questions["target"]["criteria"].as_object().unwrap();
        let options: Vec<(String, String)> = crit
            .iter()
            .filter(|(k, _)| *k != STEP_NONE)
            .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
            .collect();
        let target = options.iter().find(|(_, v)| v.starts_with(label)).map(|(k, _)| k.clone()).unwrap_or(STEP_NONE.into());
        Ok(step_envelope(&target, &options.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(), 0.95))
    }
}

fn opts(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[test]
fn test_one_request_carries_the_choice_with_none_and_three_nouls() {
    let seen = RefCell::new(Vec::new());
    let predict = saying("Week", &seen);
    let a = ta::ask_jev_step(&predict, "switch to week view", "Calendar", "title: September 2026",
                             &opts(&[("1", "Day (radio button)"), ("2", "Week (radio button)")]), NO_RECENT, &clock);
    assert_eq!(seen.borrow().len(), 1);
    let q = seen.borrow()[0].1.clone();
    let types: Vec<(String, String)> =
        q.as_object().unwrap().iter().map(|(k, v)| (k.clone(), v["type"].as_str().unwrap().to_string())).collect();
    assert_eq!(
        types,
        [("target", "choice"), ("present", "noul"), ("already_done", "noul"), ("risky", "noul")]
            .map(|(a, b)| (a.to_string(), b.to_string()))
    );
    assert_eq!(q["target"]["criteria"].as_object().unwrap().keys().collect::<Vec<_>>(), ["1", "2", STEP_NONE]);
    assert!(a.target == "2" && a.k == 3 && a.error.is_empty() && a.margin > 0.8);
}

#[test]
fn test_the_nouls_can_see_the_controls_because_the_state_lists_them() {
    let seen = RefCell::new(Vec::new());
    let predict = saying("Week", &seen);
    ta::ask_jev_step(&predict, "switch to  week view", "Calendar", "title: x", &opts(&[("1", "Day (radio button)")]),
                     &["opened Calendar"], &clock);
    let state = seen.borrow()[0].0.clone();
    assert_eq!(state["controls"], json!(["Day (radio button)"]));
    assert_eq!(state["step"], "switch to week view");
    assert_eq!(state["done_so_far"], json!(["opened Calendar"]));
    assert_eq!(state["about"], ta::STEP_ABOUT);
}

#[test]
fn test_a_failing_or_malformed_answer_abstains() {
    let boom = |_s: &Value, _q: &Value| -> Result<Value> { Err(fail("RuntimeError", "HTTP 503")) };
    let a = ta::ask_jev_step(&boom, "x", "A", "", &opts(&[("1", "One")]), NO_RECENT, &clock);
    assert!(a.target.is_empty() && a.error.contains("503") && ta::JEV_STEP_GATES.decide(&a) == STEP_NONE);
    let stranger = |_s: &Value, _q: &Value| -> Result<Value> {
        Ok(json!({"answers": {"target": {"choice": "99", "probabilities": {"99": 1.0}}}}))
    };
    let b = ta::ask_jev_step(&stranger, "x", "A", "", &opts(&[("1", "One")]), NO_RECENT, &clock);
    assert!(b.target.is_empty() && b.error.contains("not an option"));
    assert_eq!(b.error, "target '99' is not an option");
    assert_eq!((b.risky, b.k), (1.0, 2));
}

#[test]
fn test_gates_press_only_when_the_absolute_and_the_relative_both_clear() {
    let g = ta::StepGates { present: 0.9, p_target: 0.9, margin: 0.3, risky_max: 0.5, ..Default::default() };
    let s = StepAnswer::new;
    assert_eq!(g.decide(&s("2", 0.95, 0.9, 0.95, 0.0, 0.1)), "2");
    assert_eq!(g.decide(&s("2", 0.95, 0.9, 0.5, 0.0, 0.1)), STEP_NONE);
    assert_eq!(g.decide(&s("2", 0.6, 0.5, 0.95, 0.0, 0.1)), STEP_NONE);
    assert_eq!(g.decide(&s("2", 0.95, 0.1, 0.95, 0.0, 0.1)), STEP_NONE);
    assert_eq!(g.decide(&s(STEP_NONE, 0.99, 0.9, 0.95, 0.0, 0.1)), STEP_NONE);
    // risk outranks a perfect pick
    assert_eq!(g.decide(&s("2", 0.99, 0.9, 0.99, 0.0, 0.6)), "confirm");
    // and an answer already in effect is done
    assert_eq!(g.decide(&s("2", 0.99, 0.9, 0.99, 0.8, 0.1)), "done");
}

#[test]
fn test_fit_allows_no_wrong_press_and_prefers_the_stricter_tie() {
    let s = StepAnswer::new;
    let answers = [
        s("1", 0.95, 0.9, 0.95, 0.0, 0.0),
        s("2", 0.95, 0.9, 0.95, 0.0, 0.0),
        s("3", 0.75, 0.5, 0.65, 0.0, 0.0), // a wrong pick, moderately sure
        s(STEP_NONE, 0.9, 0.8, 0.1, 0.0, 0.0),
    ];
    let g = ta::fit_step_gates(&answers, &["1", "2", "9", "abstain"], 0);
    let said: Vec<String> = answers.iter().map(|a| g.decide(a)).collect();
    assert_eq!(said, ["1", "2", STEP_NONE, STEP_NONE]);
    assert!(g.present == 0.9 && g.p_target == 0.9); // the strictest setting that still presses both
    assert!(g.already_done == 0.7 && g.risky_max == 0.3); // not fitted: the defaults
}

// ---- test_quit_reflex.py (the typed_ask parts) -----------------------------------------------------------

#[test]
fn test_decide_quit_and_the_fit_allow_no_wrong_quit() {
    let g = ta::QuitGates { one: 0.5, all: 0.6, app: 0.95 };
    let q = ta::QuitAnswer::new;
    assert_eq!(ta::decide_quit(&q(0.9, 0.1, "Slack", 1.0), &g), "Slack");
    assert_eq!(ta::decide_quit(&q(0.9, 0.1, "Slack", 0.9), &g), "");
    assert_eq!(ta::decide_quit(&q(0.1, 0.9, "", 0.9), &g), "*");
    assert_eq!(ta::decide_quit(&ta::QuitAnswer { error: "x".into(), ..q(0.9, 0.9, "Slack", 1.0) }, &g), "");
    let answers = [q(0.97, 0.05, "Music", 1.0), q(0.72, 0.05, "Spotify", 1.0)];
    let fitted = ta::fit_quit_gates(&answers, &["Music", ""]);
    assert!(ta::decide_quit(&answers[0], &fitted) == "Music" && ta::decide_quit(&answers[1], &fitted).is_empty());
}

#[test]
fn test_jev_quit_request_is_absolute_nouls_and_the_installed_list() {
    let q = ta::jev_quit_questions(&["Spotify", "Safari"]);
    assert!(q["quit_one"]["type"] == "noul" && q["quit_all"]["type"] == "noul");
    assert_eq!(q["app"]["criteria"], json!({"app0": "Spotify", "app1": "Safari", "none": "no installed application is to be quit"}));
}

#[test]
fn test_the_quit_asker_reads_the_app_and_abstains_on_failure() {
    let apps: Vec<String> = ["Spotify", "Safari"].map(String::from).to_vec();
    let asker = ta::make_jev_quit_asker(
        |s: &Value, _q: &Value| -> Result<Value> {
            assert_eq!(s, &json!({"about": "A spoken request to a desk robot that can operate the owner's Mac.",
                                  "request": "quit Safari"}));
            Ok(json!({"answers": {"quit_one": {"noul": 0.9}, "quit_all": {"noul": 0.0},
                                  "app": {"choice": "app1", "probabilities": {"app1": 0.99, "none": 0.01}}}}))
        },
        clock,
    );
    assert_eq!(asker(" quit   Safari ", &apps), "Safari");
    let boom = ta::make_jev_quit_asker(|_s: &Value, _q: &Value| -> Result<Value> { Err(fail("TimeoutError", "")) }, clock);
    assert_eq!(boom("Spotify, quit it.", &apps), "");
    let a = ta::ask_jev_quit(&|_s: &Value, _q: &Value| -> Result<Value> { Err(fail("TimeoutError", "")) }, "x", &apps, &clock);
    assert_eq!(a.error, "TimeoutError: ");
}

// ---- test_task_router.py (the typed_ask parts) -----------------------------------------------------------

const ROUTER_APPS: [&str; 5] = ["Mail", "Spotify", "Notes", "Safari", "TV"];

#[test]
fn test_jev_is_asked_in_its_own_idiom_and_gated_by_absolute_answers() {
    let seen: RefCell<Option<(Value, Value)>> = RefCell::new(None);
    let apps: Vec<String> = ROUTER_APPS.map(String::from).to_vec();
    let asker = ta::make_jev_request_asker(
        |state: &Value, questions: &Value| -> Result<Value> {
            *seen.borrow_mut() = Some((state.clone(), questions.clone()));
            Ok(json!({"answers": {"launch_only": {"type": "noul", "noul": 0.93},
                                  "app": {"choice": "app3", "probabilities": {"app3": 0.97, "none": 0.01}},
                                  "web_search": {"type": "noul", "noul": 0.02},
                                  "risky_action": {"type": "noul", "noul": 0.03}}}))
        },
        clock,
    );
    assert_eq!(asker("Take me to Safari.", &apps), "Safari"); // APPS[3]
    let (state, q) = seen.borrow().clone().unwrap();
    let types: Vec<(String, String)> =
        q.as_object().unwrap().iter().map(|(k, v)| (k.clone(), v["type"].as_str().unwrap().to_string())).collect();
    assert_eq!(
        types,
        [("launch_only", "noul"), ("app", "choice"), ("web_search", "noul"), ("risky_action", "noul")]
            .map(|(a, b)| (a.to_string(), b.to_string()))
    );
    assert!(q["app"]["criteria"]["app3"] == "Safari" && q["app"]["criteria"].get("none").is_some());
    assert!(state["request"] == "Take me to Safari." && state.as_object().unwrap().len() == 2);
    let risky = ta::RequestAnswer::new(0.99, "Mail", 0.99, 0.0, 0.6);
    assert_eq!(ta::decide_request(&risky, &ta::JEV_REQUEST_GATES), "other"); // risk outranks everything
    let unsure = ta::RequestAnswer::new(0.6, "Mail", 0.99, 0.0, 0.0);
    assert_eq!(ta::decide_request(&unsure, &ta::JEV_REQUEST_GATES), "other"); // under the gate: abstain
    let down = ta::make_jev_request_asker(|_s: &Value, _q: &Value| -> Result<Value> { Err(fail("OSError", "down")) }, clock);
    assert_eq!(down("x", &apps), "");
}

#[test]
fn test_each_model_is_asked_differently_for_a_head_pose() {
    let laya = ta::LAYA_HEAD_QUESTIONS.as_object().unwrap();
    let jevq = &*ta::JEV_HEAD_QUESTIONS;
    // laya: relative choices only, short options, the gate is a choice too
    assert!(laya.values().all(|q| q["type"] == "choice"));
    let longest = laya
        .values()
        .flat_map(|q| q["criteria"].as_object().unwrap().values().map(|v| v.as_str().unwrap().split_whitespace().count()))
        .max()
        .unwrap();
    assert!(longest <= 6);
    assert!(laya.values().map(|q| q["criteria"].as_object().unwrap().len()).max().unwrap() <= 7);
    // jev: an absolute noul gate, with literal instructions that say what does NOT count
    assert!(jevq["is_head"]["type"] == "noul" && jevq["is_head"]["instructions"].as_str().unwrap().contains("scroll"));
    assert!(jevq["direction"]["criteria"].get("none").is_some());
    let a = ta::HeadAnswer::new(0.9, "left", 0.9, "little", 0.8);
    assert!(a.pose() == "bit_left" && ta::Gates { gate: 0.3, direction: 0.7 }.decide(&a) == "bit_left");
    assert_eq!(ta::Gates { gate: 0.95, direction: 0.7 }.decide(&a), "none");
    assert!(ta::HeadAnswer::new(0.9, "up", 0.9, "far", 0.9).pose() == "up");
    assert!(ta::HeadAnswer::new(0.9, "desk", 0.9, "far", 0.9).pose() == "desk");
    assert_eq!(ta::HeadAnswer::new(0.9, "right", 0.9, "far", 0.9).pose(), "far_right");
    assert_eq!(ta::HeadAnswer::new(0.9, "sideways", 0.9, "far", 0.9).pose(), "none");
    let fitted = ta::fit_gates(&[a.clone(), ta::HeadAnswer::new(0.4, "down", 0.95, "normal", 0.9)], &["bit_left", "none"], 0);
    assert!(fitted.decide(&a) == "bit_left" && fitted.gate > 0.4); // a false move is never bought
}

#[test]
fn test_describe_counts_options_and_their_longest_wording() {
    let laya = ta::describe(&ta::LAYA_HEAD_QUESTIONS).unwrap();
    assert_eq!(
        laya,
        "kind: choice of 5 (≤6 words each); direction: choice of 7 (≤5 words each); size: choice of 3 (≤3 words each)"
    );
    assert!(ta::describe(&ta::JEV_HEAD_QUESTIONS).unwrap().starts_with("is_head: noul; direction: choice of 8"));
    assert!(ta::describe(&json!({"x": {"criteria": {}}})).unwrap_err().to_string().contains("KeyError"));
}

// ---- test_voice_fast_head.py (the typed_ask parts) ---------------------------------------------------------

#[test]
fn test_the_jev_head_asker_answers_a_pose_under_the_shipped_gates() {
    let seen: RefCell<Option<Value>> = RefCell::new(None);
    let asker = ta::make_jev_head_asker(
        |state: &Value, _q: &Value| -> Result<Value> {
            *seen.borrow_mut() = Some(state.clone());
            Ok(json!({"answers": {"is_head": {"noul": 0.94},
                                  "direction": {"choice": "left", "probabilities": {"left": 0.97, "right": 0.01}},
                                  "size": {"choice": "little", "probabilities": {"little": 0.9, "normal": 0.1}}}}))
        },
        clock,
    );
    assert_eq!(asker("a little   to the left"), "bit_left");
    assert_eq!(seen.borrow().clone().unwrap(), json!({"about": ta::JEV_STATE_NOTE, "owner_said": "a little to the left"}));
    assert!(ta::POSES.contains(&"bit_left"));
    let unsure = ta::HeadAnswer::new(0.2, "left", 0.95, "normal", 0.9);
    assert_eq!(ta::JEV_HEAD_GATES.decide(&unsure), "none");
    let dead = ta::make_jev_head_asker(|_s: &Value, _q: &Value| -> Result<Value> { Err(fail("OSError", "")) }, clock);
    assert_eq!(dead("look left"), "none");
}

#[test]
fn test_laya_head_reads_the_head_kind_as_its_gate() {
    let predict = |state: &Value, _q: &Value| -> Result<Value> {
        assert_eq!(state, &json!({"owner said": "look up"}));
        Ok(json!({"answers": {"kind": {"choice": "head", "probabilities": {"head": 0.8, "talk": 0.2}},
                              "direction": {"choice": "up", "probabilities": {"up": 1.0}}}}))
    };
    let a = ta::ask_laya_head(&predict, " look  up ", &clock);
    assert_eq!((a.gate, a.direction.as_str(), a.p_direction, a.size.as_str(), a.p_size), (0.8, "up", 1.0, "normal", 0.0));
    assert_eq!(a.raw.len(), 2);
    let boom = |_s: &Value, _q: &Value| -> Result<Value> { Err(fail("RuntimeError", &"x".repeat(300))) };
    let b = ta::ask_laya_head(&boom, "look up", &clock);
    assert_eq!(b.error.chars().count(), 160);
    assert_eq!((b.direction.as_str(), b.size.as_str()), ("none", "normal"));
}

#[test]
fn test_fit_request_gates_fires_no_unsafe_reflex() {
    let r = ta::RequestAnswer::new;
    let answers = [r(0.95, "Mail", 0.99, 0.0, 0.0), r(0.75, "Notes", 0.99, 0.0, 0.0), r(0.0, "", 0.0, 0.9, 0.0),
                   r(0.0, "", 0.0, 0.65, 0.0)];
    // the second launch is planner-only: no gate may fire it
    let g = ta::fit_request_gates(&answers, &["launch", "other", "search", "search"], &[false, true, false, false],
                                  &[true, true, true, true]);
    let said: Vec<&str> = answers.iter().map(|a| ta::decide_request(a, &g)).collect();
    assert_eq!(said, ["launch", "other", "search", "search"]);
    assert_eq!((g.launch_only, g.app, g.web_search, g.risky_max), (0.95, 0.8, 0.6, 0.05));
}
