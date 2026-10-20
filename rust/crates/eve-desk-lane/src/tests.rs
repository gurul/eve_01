//! Ports of bridge/tests/test_desk_lane.py (the DeskLane and codex_note tests) and of
//! test_decision_lane.py's desk-lane wiring test.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use eve_decisions::{Answer, Evaluate};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use super::*;

fn complete() -> Value {
    json!({"status": "complete", "reason": "", "clicked": ["Week"], "already": [], "sentence": "Done. I clicked Week."})
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

/// Python's FakeHelpers (+ the decision test's `decisions_first` subclass): the goals it was asked, the log line
/// lane_first writes, a result the test may swap.
#[derive(Clone, Default)]
struct Fake {
    result: Arc<Mutex<Value>>,
    goals: Arc<Mutex<Vec<String>>>,
    seen: Arc<Mutex<Vec<String>>>,
    panic_once: Arc<Mutex<bool>>,
}

struct FakeHelpers {
    fake: Fake,
    log: Option<LogFn>,
}

impl DeskHelpers for FakeHelpers {
    fn bind(&mut self, log: LogFn) {
        self.log = Some(log);
    }

    fn begin(&mut self) {}

    fn lane_first(&mut self, goal: &str) -> Value {
        if std::mem::take(&mut *self.fake.panic_once.lock()) {
            panic!("ax down");
        }
        self.fake.goals.lock().push(goal.to_string());
        if let Some(log) = &self.log {
            log("delegate route: …");
        }
        self.fake.result.lock().clone()
    }

    fn decisions_first(&mut self, goal: &str, _client: &mut dyn Evaluate) -> Value {
        self.fake.seen.lock().push(goal.to_string());
        json!({"status": "complete", "clicked": ["Week"], "sentence": "Done."})
    }
}

impl Fake {
    fn new(result: Value) -> Self {
        let f = Fake::default();
        *f.result.lock() = result;
        f
    }

    fn make(&self) -> MakeHelpers {
        let fake = self.clone();
        Box::new(move || Ok(Box::new(FakeHelpers { fake: fake.clone(), log: None }) as Box<dyn DeskHelpers>))
    }
}

/// The test's decisions client (the decision test's `Client()`), never called by the fake helpers.
struct NoClient;

impl Evaluate for NoClient {
    fn evaluate(&mut self, _: &Map<String, Value>, _: &[image::DynamicImage], _: &[Value]) -> Answer {
        Answer::new(Map::new())
    }
}

fn decisions_off() -> (DecisionsEnabled, DecisionsFromEnv) {
    (Box::new(|| false), Box::new(|| None))
}

fn lane(fake: &Fake) -> DeskLane {
    let (on, from_env) = decisions_off();
    DeskLane::with_decisions(fake.make(), Box::new(|| true), on, from_env)
}

// ---- DeskLane itself ----------------------------------------------------------------------------------------

#[tokio::test]
async fn test_the_lane_runs_lane_first_on_one_helpers_for_the_daemons_life() {
    let made = Arc::new(AtomicUsize::new(0));
    let fake = Fake::new(complete());
    let counted = Arc::clone(&made);
    let inner = fake.make();
    let make: MakeHelpers = Box::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
        inner()
    });
    let (on, from_env) = decisions_off();
    let lane = DeskLane::with_decisions(make, Box::new(|| true), on, from_env);
    for _ in 0..2 {
        let r = lane.attempt("switch to week view").await.expect("the lane tried");
        assert_eq!(r["status"], "complete");
        assert_eq!(r["log"], json!(["delegate route: …"]));
    }
    assert_eq!(made.load(Ordering::SeqCst), 1);
    assert_eq!(*fake.goals.lock(), vec!["switch to week view".to_string(); 2]);
}

#[tokio::test]
async fn test_without_accessibility_the_lane_is_off_and_never_builds_helpers() {
    let made = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&made);
    let make: MakeHelpers = Box::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
        Err(anyhow::anyhow!("never built"))
    });
    let (on, from_env) = decisions_off();
    let lane = DeskLane::with_decisions(make, Box::new(|| false), on, from_env);
    assert!(lane.attempt("switch to week view").await.is_none());
    assert!(lane.attempt("switch to week view").await.is_none());
    assert_eq!(made.load(Ordering::SeqCst), 0);
    assert_eq!(lane.broken(), "no Accessibility access for the daemon");
}

#[tokio::test]
async fn test_a_lane_that_cannot_start_is_off_for_good() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let make: MakeHelpers = Box::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
        Err(eve_desktop_lanes::LaneError::err("ImportError", "no pyautogui"))
    });
    let (on, from_env) = decisions_off();
    let lane = DeskLane::with_decisions(make, Box::new(|| true), on, from_env);
    assert!(lane.attempt("x").await.is_none() && lane.attempt("x").await.is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(lane.broken(), "ImportError: no pyautogui");
}

#[tokio::test]
async fn test_a_goal_is_capped_before_it_reaches_the_router() {
    let fake = Fake::new(complete());
    lane(&fake).attempt(&"x".repeat(5000)).await;
    assert_eq!(fake.goals.lock()[0].chars().count(), MAX_GOAL_CHARS);
    // characters, not bytes
    let fake = Fake::new(complete());
    lane(&fake).attempt(&"é".repeat(5000)).await;
    assert_eq!(fake.goals.lock()[0], "é".repeat(MAX_GOAL_CHARS));
}

#[test]
fn test_the_switch_turns_it_off_and_defaults_on() {
    // The Daemon._desk half of test_the_daemon_makes_one_lane_and_the_switch_turns_it_off belongs to the daemon.
    let env = |v: &str| HashMap::from([(ENV.to_string(), v.to_string())]);
    assert!(!enabled(Some(&env("0"))));
    assert!(enabled(Some(&env("1"))));
    assert_eq!(enabled(Some(&HashMap::new())), DEFAULT);
    for off in ["false", " NO ", "Off"] {
        assert!(!enabled(Some(&env(off))), "{off}");
    }
    for on in ["true", "YES", " on"] {
        assert!(enabled(Some(&env(on))), "{on}");
    }
    assert_eq!(enabled(Some(&env("maybe"))), DEFAULT);
    assert_eq!(enabled(Some(&env(""))), DEFAULT);
}

#[test]
fn test_codex_note_names_the_clicks_only_when_there_were_some() {
    assert_eq!(codex_note(&obj(json!({"clicked": []}))), "");
    assert_eq!(codex_note(&obj(json!({}))), "");
    assert_eq!(codex_note(&obj(json!({"clicked": null}))), "");
    let note = codex_note(&obj(json!({"clicked": ["Year", "Next"]})));
    assert!(note.contains("'Year', then 'Next'") && note.contains("Look at the screen first"));
    assert_eq!(
        note,
        "\n\nBefore you started, eve's fast lane already clicked 'Year', then 'Next' in the frontmost window, and \
         could not confirm the request is finished. Look at the screen first; do not click those again unless the \
         screen shows they did nothing."
    );
    // repr() picks double quotes for a label with an apostrophe
    assert!(codex_note(&obj(json!({"clicked": ["Today's"]}))).contains("clicked \"Today's\" in"));
}

#[test]
fn test_unknown_effect_is_partial_and_named_in_codex_handoff() {
    // test_decision_lane.py: the partial result's `clicked` is what codex_note names.
    let partial = obj(json!({"status": "partial", "reason": "unverified", "clicked": ["Week"], "sentence": ""}));
    assert!(codex_note(&partial).contains("'Week'"));
}

#[tokio::test]
async fn test_daemon_wiring_only_tries_decisions_after_zero_input_keyword_mismatch() {
    let fake = Fake::new(json!({"status": "none", "reason": "no_match"}));
    let make_lane = |fake: &Fake| {
        DeskLane::with_decisions(
            fake.make(),
            Box::new(|| true),
            Box::new(|| true),
            Box::new(|| Some(Box::new(NoClient) as Box<dyn Evaluate + Send>)),
        )
    };
    let r = make_lane(&fake).attempt("next seven days").await.expect("the lane tried");
    assert_eq!(r["status"], "complete");
    assert_eq!(*fake.seen.lock(), vec!["next seven days".to_string()]);
    // both routes' lines ride the one log
    assert_eq!(r["log"], json!(["delegate route: …"]));
    for result in [
        json!({"status": "complete"}),
        json!({"status": "partial", "reason": "unverified"}),
        json!({"status": "none", "reason": "confirm"}),
    ] {
        *fake.result.lock() = result;
        make_lane(&fake).attempt("x").await;
    }
    assert_eq!(*fake.seen.lock(), vec!["next seven days".to_string()]);
}

#[tokio::test]
async fn test_decisions_off_or_without_a_client_keep_the_keyword_result() {
    for (on, client) in [(false, true), (true, false)] {
        let fake = Fake::new(json!({"status": "none", "reason": "ambiguous_target"}));
        let from_env: DecisionsFromEnv = if client {
            Box::new(|| Some(Box::new(NoClient) as Box<dyn Evaluate + Send>))
        } else {
            Box::new(|| None)
        };
        let lane = DeskLane::with_decisions(fake.make(), Box::new(|| true), Box::new(move || on), from_env);
        let r = lane.attempt("x").await.expect("the lane tried");
        assert_eq!(r["reason"], "ambiguous_target");
        assert!(fake.seen.lock().is_empty());
    }
}

#[tokio::test]
async fn test_a_log_the_result_already_carries_is_kept() {
    let fake = Fake::new(json!({"status": "none", "reason": "question", "log": ["own"]}));
    let r = lane(&fake).attempt("what is this?").await.expect("the lane tried");
    assert_eq!(r["log"], json!(["own"]));
}

#[tokio::test]
async fn test_a_lane_that_panics_or_answers_no_dict_never_costs_the_task() {
    let fake = Fake::new(complete());
    let lane = lane(&fake);
    *fake.panic_once.lock() = true;
    assert!(lane.attempt("switch to week view").await.is_none());
    // the lock is released and the helpers kept: the next attempt runs
    assert_eq!(lane.attempt("switch to week view").await.expect("the lane tried")["status"], "complete");
    *fake.result.lock() = json!(["not", "a", "dict"]);
    assert!(lane.attempt("switch to week view").await.is_none());
    assert_eq!(lane.broken(), "");
}

#[test]
fn test_real_helpers_answer_through_the_trait() {
    use eve_desktop_lanes::desktop_helpers::{Backends, Front, Gui, Helpers};

    struct NoGui;
    impl Gui for NoGui {
        fn size(&self) -> (i64, i64) {
            (1512, 982)
        }
        fn click(&mut self, _: i64, _: i64, _: u32) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("no clicks in this test"))
        }
        fn hotkey(&mut self, _: &[&str]) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("no keys in this test"))
        }
        fn press(&mut self, _: &str) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("no keys in this test"))
        }
    }
    let backends = Backends {
        frontmost_fn: Box::new(Front::default),
        ax_snapshot: Box::new(|_| Err(eve_desktop_lanes::LaneError::err("RuntimeError", "ax down"))),
        capture: Box::new(|| Err(eve_desktop_lanes::LaneError::err("RuntimeError", "no screen"))),
        sleep: Box::new(|_| {}),
        ..Backends::default()
    };
    let mut helpers: Box<dyn DeskHelpers> = Box::new(Helpers::new(Box::new(NoGui), backends));
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = Arc::clone(&lines);
    helpers.bind(Box::new(move |l: &str| sink.lock().push(l.to_string())));
    helpers.begin();
    let r = helpers.lane_first("switch to week view");
    let status = r["status"].as_str().expect("a RouteResult dict");
    assert!(["none", "refused", "unavailable"].contains(&status), "{r}");
}
