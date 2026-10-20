//! plan_executor and task_router: ports of bridge/tests/test_plan_executor.py, test_task_router.py (the classifier
//! parts), and the parts of test_plan_success_guard.py, test_plan_url_expectation.py, test_quit_reflex.py,
//! test_site_names.py and test_vault_fill.py that exercise these two modules.
//!
//! The fakes are test_fast_lane's and test_jev_step's: nothing here opens an app, presses a key or a socket.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use serde_json::{json, Map, Value};

use eve_browser_intent::{expected_outcome, IntentAsker, Kind, RequestOutcome};
use eve_desktop_lanes::ax_candidates::{is_sensitive, Candidate, Snapshot};
use eve_desktop_lanes::fast_lane::{Effectors, Senses, StepAsk, DEFAULT_DECIDE, LANE_FIRST_DEFAULT};
use eve_desktop_lanes::lane_router::is_question;
use eve_desktop_lanes::plan_executor::{holds, named_field, run_plan, PlanOptions, PlanResult};
use eve_desktop_lanes::task_router as tr;
use eve_plan_contract::{self as pc, Expect, ParseOptions, Plan};
use eve_typed_ask::{ask_jev_step, StepAnswer, STEP_NONE};

// ---- fakes (test_fast_lane / test_jev_step) -------------------------------------------------------------------

/// test_fast_lane.Clock: 1 ms a reading.
struct FakeClock(Cell<f64>);

impl FakeClock {
    fn new() -> Self {
        FakeClock(Cell::new(0.0))
    }

    fn tick(&self) -> f64 {
        self.0.set(self.0.get() + 0.001);
        self.0.get()
    }
}

#[derive(Clone)]
struct C {
    value: &'static str,
    secure: bool,
    in_dialog: bool,
    in_content: bool,
    app: &'static str,
    actions: Option<Vec<&'static str>>,
}

impl Default for C {
    fn default() -> Self {
        C { value: "", secure: false, in_dialog: false, in_content: false, app: "Calendar", actions: None }
    }
}

/// test_fast_lane.cand.
fn cand_with(cid: &str, role: &str, label: &str, o: C) -> Candidate {
    let n: i64 = cid.parse().unwrap();
    let editable = ["text field", "search field", "text area", "combo box"].contains(&role);
    let actions: Vec<String> = match o.actions {
        Some(a) => a.into_iter().map(str::to_string).collect(),
        None if editable => Vec::new(),
        None => vec!["AXPress".to_string()],
    };
    Candidate {
        id: cid.to_string(),
        role: role.to_string(),
        label: label.to_string(),
        value: o.value.to_string(),
        frame: (100 + 70 * n, 100, 60, 30),
        actions,
        enabled: true,
        app: o.app.to_string(),
        in_content: o.in_content,
        in_dialog: o.in_dialog,
        secure: o.secure,
        editable,
    }
}

fn cand(cid: &str, role: &str, label: &str) -> Candidate {
    cand_with(cid, role, label, C::default())
}

thread_local! {
    static SEQ: Cell<i64> = const { Cell::new(0) };
}

struct S {
    app: &'static str,
    bundle: &'static str,
    title: String,
    dialog_text: &'static str,
    focused: Option<Candidate>,
}

impl Default for S {
    fn default() -> Self {
        S { app: "Calendar", bundle: "com.apple.iCal", title: "September 2026".into(), dialog_text: "", focused: None }
    }
}

/// test_fast_lane.snap.
fn snap_with(cands: Vec<Candidate>, o: S) -> Snapshot {
    let seq = SEQ.with(|s| {
        s.set(s.get() + 1);
        s.get()
    });
    Snapshot {
        seq,
        app: o.app.to_string(),
        bundle: o.bundle.to_string(),
        title: o.title.clone(),
        pid: 42,
        node_count: cands.len() as i64 + 3,
        elements: cands,
        context_lines: vec![format!("title: {}", o.title)],
        focused: o.focused,
        dialog_text: o.dialog_text.to_string(),
        secs: 0.05,
        ..Default::default()
    }
}

fn snap(cands: Vec<Candidate>) -> Snapshot {
    snap_with(cands, S::default())
}

fn calendar(week_selected: bool) -> Snapshot {
    snap(vec![
        cand("0", "button", "Add Event"),
        cand("1", "radio button", "Day"),
        cand_with("2", "radio button", "Week", C { value: if week_selected { "selected" } else { "" }, ..C::default() }),
        cand_with("3", "radio button", "Month", C { value: if week_selected { "" } else { "selected" }, ..C::default() }),
        cand("4", "button", "Today"),
        cand("5", "button", "Delete Event"),
        cand("6", "search field", "Search"),
    ])
}

fn message_compose() -> Snapshot {
    snap_with(
        vec![cand("0", "button", "Emoji"), cand("1", "button", "Audio"), cand("2", "text area", "Message")],
        S { app: "Messages", bundle: "com.apple.MobileSMS", title: "Maya Patel".into(), ..S::default() },
    )
}

fn notes_search() -> Snapshot {
    snap_with(
        vec![cand("0", "button", "New Note"), cand("1", "button", "Folders"), cand("2", "search field", "Search")],
        S { app: "Notes", bundle: "com.apple.Notes", title: "Notes".into(), ..S::default() },
    )
}

/// test_fast_lane.Senses.
struct FakeSenses {
    snapshots: Vec<Snapshot>,
    visible: Vec<HashSet<String>>,
    changed: Vec<Option<bool>>,
    calls: Cell<usize>,
}

impl FakeSenses {
    fn new(snapshots: Vec<Snapshot>) -> Self {
        FakeSenses { snapshots, visible: vec![HashSet::new()], changed: vec![None], calls: Cell::new(0) }
    }

    fn changed(mut self, changed: &[Option<bool>]) -> Self {
        self.changed = changed.to_vec();
        self
    }

    fn visible(mut self, visible: &[&[&str]]) -> Self {
        self.visible = visible.iter().map(|v| v.iter().map(|s| s.to_string()).collect()).collect();
        self
    }
}

impl Senses for FakeSenses {
    fn snapshot(&self) -> anyhow::Result<Snapshot> {
        let calls = self.calls.get();
        let s = self.snapshots[calls.min(self.snapshots.len() - 1)].clone();
        self.calls.set(calls + 1);
        Ok(s)
    }

    fn text_visible(&self, s: &str) -> anyhow::Result<bool> {
        // the visibility set that goes with the LATEST snapshot handed out
        let idx = self.calls.get().saturating_sub(1).min(self.visible.len() - 1);
        Ok(self.visible[idx].contains(s))
    }

    fn focused(&self) -> Option<Candidate> {
        None
    }

    fn frontmost_pid(&self) -> anyhow::Result<i64> {
        Ok(42)
    }

    fn screen_changed(&self) -> anyhow::Result<Option<bool>> {
        let idx = self.calls.get().saturating_sub(2).min(self.changed.len() - 1);
        Ok(self.changed[idx])
    }
}

/// test_fast_lane.Effectors, with the optional abilities of test_plan_executor's subclasses (ConsentEffectors,
/// GatedEffectors) and test_vault_fill's page.
#[derive(Default)]
struct FakeEffectors {
    clicks: RefCell<Vec<String>>,
    typed: RefCell<Vec<(String, String)>>,
    presses: RefCell<Vec<String>>,
    settles: RefCell<Vec<f64>>,
    /// ConsentEffectors: what clear_consent says
    consent: Option<String>,
    cleared: Cell<usize>,
    /// GatedEffectors: a sensitive label is refused unless the grant names it
    gated: bool,
    approve: RefCell<String>,
    /// test_vault_fill.Effectors: what fill_from_vault says
    vault: Option<String>,
    asked: RefCell<Vec<String>>,
}

impl Effectors for FakeEffectors {
    fn click_candidate(&self, c: &Candidate) -> anyhow::Result<String> {
        if self.gated && is_sensitive(&c.label) && *self.approve.borrow() != eve_desktop_lanes::plan_executor::norm(&c.label) {
            return Ok(format!("refused: '{}' is a sensitive control without approval", c.label));
        }
        self.clicks.borrow_mut().push(c.label.clone());
        Ok(format!("clicked {} '{}'", c.role, c.label))
    }

    fn focus_and_type(&self, c: &Candidate, text: &str) -> anyhow::Result<String> {
        self.typed.borrow_mut().push((c.label.clone(), text.to_string()));
        Ok(format!("typed {} characters into '{}'", text.chars().count(), c.label))
    }

    fn press(&self, key: &str) -> anyhow::Result<String> {
        self.presses.borrow_mut().push(key.to_string());
        Ok(format!("pressed {key}"))
    }

    fn settle(&self, secs: f64) -> anyhow::Result<f64> {
        self.settles.borrow_mut().push(secs);
        Ok(0.3)
    }

    fn fill_from_vault(&self, handle: &str) -> Option<anyhow::Result<String>> {
        let said = self.vault.as_ref()?;
        self.asked.borrow_mut().push(handle.to_string());
        Some(Ok(said.clone()))
    }

    fn clear_consent(&self) -> Option<anyhow::Result<String>> {
        let said = self.consent.as_ref()?;
        self.cleared.set(self.cleared.get() + 1);
        Some(Ok(said.clone()))
    }

    fn grant(&self, approved: &str) {
        *self.approve.borrow_mut() = approved.to_string();
    }
}

impl FakeEffectors {
    fn clicks(&self) -> Vec<String> {
        self.clicks.borrow().clone()
    }

    fn typed(&self) -> Vec<(String, String)> {
        self.typed.borrow().clone()
    }

    fn presses(&self) -> Vec<String> {
        self.presses.borrow().clone()
    }
}

/// test_jev_step.envelope.
fn envelope(target: &str, options: &[String], p: f64, present: f64, already_done: f64, risky: f64) -> Value {
    let mut keys: Vec<String> = options.to_vec();
    keys.push(STEP_NONE.to_string());
    let rest = (1.0 - p) / (keys.len() as f64 - 1.0).max(1.0);
    let probabilities: Map<String, Value> =
        keys.iter().map(|k| (k.clone(), json!(if k == target { p } else { rest }))).collect();
    json!({"answers": {
        "target": {"type": "choice", "choice": target, "probabilities": probabilities},
        "present": {"type": "noul", "noul": present},
        "already_done": {"type": "noul", "noul": already_done},
        "risky": {"type": "noul", "noul": risky}}})
}

/// test_jev_step.asker_saying: picks the option whose text starts with `label` ("none" picks none).
struct Saying {
    label: &'static str,
    p: f64,
    risky: f64,
}

fn asker_saying(label: &'static str) -> Saying {
    Saying { label, p: 0.95, risky: 0.05 }
}

impl StepAsk for Saying {
    fn ask(
        &self,
        step: &str,
        app: &str,
        context: &str,
        options: &IndexMap<String, String>,
        recent: &[String],
    ) -> StepAnswer {
        let predict = |_state: &Value, questions: &Value| -> anyhow::Result<Value> {
            let criteria = questions["target"]["criteria"].as_object().unwrap();
            let keys: Vec<String> = criteria.keys().filter(|k| *k != STEP_NONE).cloned().collect();
            let target = keys
                .iter()
                .find(|k| criteria[k.as_str()].as_str().unwrap_or("").starts_with(self.label))
                .cloned()
                .unwrap_or_else(|| STEP_NONE.to_string());
            Ok(envelope(&target, &keys, self.p, 0.95, 0.05, self.risky))
        };
        let clock = FakeClock::new();
        ask_jev_step(&predict, step, app, context, options, recent, &|| clock.tick())
    }
}

/// open_app / open_url / frontmost_app, recorded.
struct Desk {
    front: RefCell<String>,
    fail: bool,
    opened: RefCell<Vec<String>>,
    /// what open_url adds after the URL (" in Safari" in test_plan_executor, "" in test_plan_success_guard)
    url_suffix: &'static str,
}

impl Desk {
    fn new(front: &str) -> Self {
        Desk { front: RefCell::new(front.to_string()), fail: false, opened: RefCell::new(Vec::new()), url_suffix: " in Safari" }
    }

    fn open_app(&self, name: &str) -> anyhow::Result<String> {
        if self.fail {
            return Err(eve_desktop_lanes::LaneError::err("RuntimeError", format!("no app called '{name}'")));
        }
        self.opened.borrow_mut().push(name.to_string());
        *self.front.borrow_mut() = name.to_string();
        Ok(format!("opened {name} (frontmost after 0.4 s)"))
    }

    fn open_url(&self, url: &str) -> anyhow::Result<String> {
        self.opened.borrow_mut().push(url.to_string());
        Ok(format!("opened {url}{}", self.url_suffix))
    }

    fn frontmost_app(&self) -> String {
        self.front.borrow().clone()
    }

    fn opened(&self) -> Vec<String> {
        self.opened.borrow().clone()
    }
}

/// test_plan_executor.plan: every step with the defaults, then parse_plan.
fn plan_full(steps: Vec<Value>, request: &str, final_say: &str, success: Value) -> Plan {
    let full: Vec<Value> = steps.into_iter().map(full_step).collect();
    pc::parse_plan(
        &json!({"needs_eyes": false, "why": "", "steps": full, "final_say": final_say, "success": success}),
        request,
    )
    .unwrap()
}

fn full_step(s: Value) -> Value {
    let mut base = json!({"kind": "click", "target": "", "label_hint": "", "text": "", "key": "", "expect": null,
                          "consequential": false});
    for (k, v) in s.as_object().unwrap() {
        base[k] = v.clone();
    }
    base
}

fn plan(steps: Vec<Value>, request: &str) -> Plan {
    plan_full(steps, request, "All set.", Value::Null)
}

/// test_plan_executor.go.
fn go(
    p: &Plan,
    senses: &dyn Senses,
    eff: &dyn Effectors,
    desk: &Desk,
    asker: Option<&dyn StepAsk>,
    kw: impl FnOnce(&mut PlanOptions<'_>),
) -> PlanResult {
    let clock = FakeClock::new();
    let tick = || clock.tick();
    let mut opts = PlanOptions { clock: &tick, ..PlanOptions::default() };
    kw(&mut opts);
    run_plan(
        p,
        senses,
        eff,
        asker,
        &|name| desk.open_app(name),
        &|url| desk.open_url(url),
        &|| desk.frontmost_app(),
        opts,
    )
}

fn keyword(o: &mut PlanOptions<'_>) {
    o.decide = "keyword".to_string();
}

fn none(_: &mut PlanOptions<'_>) {}

fn effects(r: &PlanResult) -> Vec<&str> {
    r.ledger.iter().map(|e| e.effect.as_str()).collect()
}

// ==== test_plan_executor.py: walking a plan =====================================================================

#[test]
fn test_a_two_step_plan_runs_with_no_model_when_the_labels_are_exact() {
    let eff = FakeEffectors::default();
    let senses = FakeSenses::new(vec![calendar(false), calendar(true), calendar(true), calendar(true)])
        .changed(&[Some(true), Some(true)]);
    let p = plan(
        vec![json!({"kind": "open_app", "target": "Calendar"}),
             json!({"kind": "click", "target": "the week view", "label_hint": "Week"})],
        "",
    );
    let r = go(&p, &senses, &eff, &Desk::new("Finder"), None, keyword);
    assert!(r.status == "complete" && r.sentence == "All set.", "{}", r.to_dict());
    assert_eq!(eff.clicks(), ["Week"]);
    assert_eq!(effects(&r), ["confirmed", "confirmed"]);
    assert_eq!(r.ledger.iter().map(|e| e.how.as_str()).collect::<Vec<_>>(), ["helper", "keyword"]);
}

#[test]
fn test_the_description_is_tried_when_the_hint_finds_nothing() {
    let eff = FakeEffectors::default();
    let asker = asker_saying("Week");
    let p = plan(vec![json!({"kind": "click", "target": "the seven day view", "label_hint": "Weekly"})], "");
    let senses = FakeSenses::new(vec![calendar(false), calendar(false), calendar(true)]).changed(&[Some(true)]);
    let r = go(&p, &senses, &eff, &Desk::new("Calendar"), Some(&asker), none);
    assert!(r.status == "complete" && eff.clicks() == ["Week"] && r.ledger[0].how == "jev", "{}", r.to_dict());
}

#[test]
fn test_a_step_no_control_answers_hands_back_with_zero_input() {
    let eff = FakeEffectors::default();
    let p = plan(vec![json!({"kind": "click", "target": "the print button"})], "");
    let r = go(&p, &FakeSenses::new(vec![calendar(false)]), &eff, &Desk::new("Calendar"), Some(&asker_saying("none")), none);
    assert!(r.status == "none" && r.next_index == 0 && eff.clicks().is_empty() && r.reason == "jev_none", "{}", r.to_dict());
}

#[test]
fn test_jev_refusing_the_gates_pick_presses_nothing() {
    let eff = FakeEffectors::default();
    let p = plan(
        vec![json!({"kind": "click", "target": "week", "label_hint": "Week"}),
             json!({"kind": "click", "target": "the print button"})],
        "",
    );
    // the keyword gate proposes Week, Jev's top answer is none: a veto, under the hint and under the description
    let r = go(&p, &FakeSenses::new(vec![calendar(false)]), &eff, &Desk::new("Calendar"), Some(&asker_saying("none")), |o| {
        o.decide = "jev".into()
    });
    assert!(r.status == "none" && r.next_index == 0 && r.reason == "jev_veto" && eff.clicks().is_empty(), "{}", r.to_dict());
}

#[test]
fn test_a_later_failure_is_partial_and_the_ledger_says_what_was_applied() {
    let eff = FakeEffectors::default();
    let p = plan(
        vec![json!({"kind": "click", "target": "week", "label_hint": "Week"}),
             json!({"kind": "click", "target": "the print button"})],
        "",
    );
    let senses = FakeSenses::new(vec![calendar(false), calendar(true), calendar(true)]).changed(&[Some(true)]);
    let r = go(&p, &senses, &eff, &Desk::new("Calendar"), None, keyword);
    assert!(r.status == "partial" && r.next_index == 1 && r.reason == "no_match" && eff.clicks() == ["Week"], "{}", r.to_dict());
    assert_eq!(effects(&r), ["confirmed", "refused"]);
}

#[test]
fn test_a_click_that_changes_nothing_stops_the_plan() {
    let eff = FakeEffectors::default();
    let same = snap(vec![cand("1", "button", "Refresh"), cand("2", "button", "Today")]);
    let p = plan(
        vec![json!({"kind": "click", "target": "refresh", "label_hint": "Refresh"}),
             json!({"kind": "click", "target": "today", "label_hint": "Today"})],
        "",
    );
    let senses = FakeSenses::new(vec![same.clone(), same.clone(), same]).changed(&[Some(false)]);
    let r = go(&p, &senses, &eff, &Desk::new("Calendar"), None, keyword);
    assert!(r.status == "partial" && eff.clicks() == ["Refresh"] && r.ledger[0].effect == "suspected_noop", "{}", r.to_dict());
    assert_eq!(r.reason, "the screen did not change");
}

#[test]
fn test_an_unconfirmed_step_is_never_reported_as_plain_success() {
    let eff = FakeEffectors::default();
    let same = snap(vec![cand("1", "button", "Refresh"), cand("2", "button", "Today")]);
    let p = plan(vec![json!({"kind": "click", "target": "refresh", "label_hint": "Refresh"})], "");
    let senses = FakeSenses::new(vec![same.clone(), same]).changed(&[None]);
    let r = go(&p, &senses, &eff, &Desk::new("Calendar"), None, keyword);
    assert!(r.status == "complete" && r.ledger[0].effect == "unverifiable", "{}", r.to_dict());
    assert!(r.sentence.contains("couldn't confirm") && r.sentence != "All set.");
}

#[test]
fn test_an_expectation_that_does_not_hold_stops_the_plan() {
    let eff = FakeEffectors::default();
    let p = plan(
        vec![json!({"kind": "click", "target": "week", "label_hint": "Week",
                    "expect": {"kind": "title_contains", "value": "2031"}})],
        "",
    );
    let senses = FakeSenses::new(vec![calendar(false), calendar(true)]).changed(&[Some(true)]);
    let r = go(&p, &senses, &eff, &Desk::new("Calendar"), None, keyword);
    assert!(r.status == "partial" && r.reason.contains("did not see it") && eff.clicks() == ["Week"], "{}", r.to_dict());
}

// ==== test_plan_executor.py: what only the human can approve ===================================================

#[test]
fn test_the_plans_own_flag_stops_before_any_input_and_resumes_on_a_yes() {
    let eff = FakeEffectors::default();
    let p = plan(vec![json!({"kind": "click", "target": "week", "label_hint": "Week", "consequential": true})], "");
    let r = go(&p, &FakeSenses::new(vec![calendar(false)]), &eff, &Desk::new("Calendar"), None, keyword);
    assert!(r.status == "needs_human" && r.next_index == 0 && eff.clicks().is_empty() && r.confirm.contains("week"));
    let senses = FakeSenses::new(vec![calendar(false), calendar(true)]).changed(&[Some(true)]);
    let confirm = r.confirm.clone();
    let r2 = go(&p, &senses, &eff, &Desk::new("Calendar"), None, |o| {
        o.decide = "keyword".into();
        o.start = 0;
        o.approved = HashMap::from([(0, confirm)]);
    });
    assert!(r2.status == "complete" && eff.clicks() == ["Week"], "{}", r2.to_dict());
}

#[test]
fn test_a_sensitive_control_needs_the_human_whatever_the_plan_says() {
    let eff = FakeEffectors::default();
    let p = plan(
        vec![json!({"kind": "click", "target": "delete the event", "label_hint": "Delete Event", "consequential": false})],
        "",
    );
    let r = go(&p, &FakeSenses::new(vec![calendar(false)]), &eff, &Desk::new("Calendar"), None, keyword);
    assert!(r.status == "needs_human" && r.confirm == "Delete Event" && eff.clicks().is_empty(), "{}", r.to_dict());
    // the human's yes lets exactly that label through, once
    let senses = FakeSenses::new(vec![calendar(false), calendar(false)]).changed(&[Some(true)]);
    let confirm = r.confirm.clone();
    let r2 = go(&p, &senses, &eff, &Desk::new("Calendar"), None, |o| {
        o.decide = "keyword".into();
        o.approved = HashMap::from([(0, confirm)]);
    });
    assert!(r2.status == "complete" && eff.clicks() == ["Delete Event"], "{}", r2.to_dict());
}

#[test]
fn test_jevs_risky_answer_stops_a_step_the_code_table_does_not_know() {
    let eff = FakeEffectors::default();
    let p = plan(vec![json!({"kind": "click", "target": "week", "label_hint": "Week"})], "");
    let risky = Saying { risky: 0.9, ..asker_saying("Week") };
    let r = go(&p, &FakeSenses::new(vec![calendar(false)]), &eff, &Desk::new("Calendar"), Some(&risky), none);
    assert!(r.status == "needs_human" && r.confirm == "Week" && eff.clicks().is_empty(), "{}", r.to_dict());
    let senses = FakeSenses::new(vec![calendar(false), calendar(true)]).changed(&[Some(true)]);
    let r2 = go(&p, &senses, &eff, &Desk::new("Calendar"), Some(&risky), |o| {
        o.approved = HashMap::from([(0, "Week".to_string())]);
    });
    assert!(r2.status == "complete" && eff.clicks() == ["Week"], "{}", r2.to_dict());
}

#[test]
fn test_a_dictated_search_may_be_submitted() {
    let eff = FakeEffectors::default();
    let p = plan(
        vec![json!({"kind": "type", "target": "the search field", "text": "blue adapter"}),
             json!({"kind": "press_key", "key": "return"})],
        "search my notes for blue adapter",
    );
    let r = go(&p, &FakeSenses::new(vec![notes_search()]), &eff, &Desk::new("Notes"), None, none);
    assert!(r.status == "complete", "{}", r.to_dict());
    assert_eq!(eff.typed(), [("Search".to_string(), "blue adapter".to_string())]);
    assert_eq!(eff.presses(), ["return"]);
}

#[test]
fn test_composed_text_is_typed_but_never_submitted_without_a_yes() {
    let eff = FakeEffectors::default();
    let p = plan(
        vec![json!({"kind": "type", "target": "the message field", "text": "Running ten minutes late"}),
             json!({"kind": "press_key", "key": "return"})],
        "tell Maya I'm late",
    );
    let r = go(&p, &FakeSenses::new(vec![message_compose()]), &eff, &Desk::new("Messages"), None, none);
    assert!(r.status == "needs_human" && r.next_index == 1 && eff.presses().is_empty(), "{}", r.to_dict());
    assert_eq!(eff.typed(), [("Message".to_string(), "Running ten minutes late".to_string())]);
    assert!(r.confirm.contains("Running ten minutes late"));
}

#[test]
fn test_composed_text_into_a_shell_asks_before_the_first_key() {
    let eff = FakeEffectors::default();
    let shell = snap_with(
        vec![cand("1", "text area", "shell")],
        S { app: "Terminal", bundle: "com.apple.Terminal", title: "zsh".into(), ..S::default() },
    );
    let p = plan(vec![json!({"kind": "type", "target": "the terminal", "text": "rm -rf build"})], "clean the build folder");
    let r = go(&p, &FakeSenses::new(vec![shell]), &eff, &Desk::new("Terminal"), None, none);
    assert!(r.status == "needs_human" && eff.typed().is_empty() && r.confirm.contains("rm -rf build"), "{}", r.to_dict());
}

#[test]
fn test_a_secure_field_or_a_dialog_is_never_typed_into() {
    let eff = FakeEffectors::default();
    let login = snap_with(
        vec![cand_with("1", "text field", "Password", C { secure: true, ..C::default() })],
        S { app: "Safari", bundle: "com.apple.Safari", title: "Sign in".into(), ..S::default() },
    );
    let p = plan(vec![json!({"kind": "type", "target": "the password field", "text": "hunter2"})], "type hunter2");
    assert_eq!(go(&p, &FakeSenses::new(vec![login]), &eff, &Desk::new("Safari"), None, none).status, "none");
    assert!(eff.typed().is_empty());
    let sheet = snap_with(vec![cand("1", "text field", "Name")], S { dialog_text: "Save changes?", ..S::default() });
    let p = plan(vec![json!({"kind": "type", "target": "name", "text": "x"})], "type x");
    assert_eq!(go(&p, &FakeSenses::new(vec![sheet]), &eff, &Desk::new("Calendar"), None, none).reason, "dialog_open");
}

#[test]
fn test_among_several_fields_jev_picks_or_nothing_is_typed() {
    let form = snap_with(
        vec![cand("1", "text field", "First name"), cand("2", "text field", "City")],
        S { app: "Safari", bundle: "com.apple.Safari", title: "Form".into(), ..S::default() },
    );
    let p = plan(vec![json!({"kind": "type", "target": "the city field", "text": "Lisbon"})], "put Lisbon as the city");
    let eff = FakeEffectors::default();
    let r = go(&p, &FakeSenses::new(vec![form.clone()]), &eff, &Desk::new("Safari"), Some(&asker_saying("City")), none);
    assert_eq!(r.status, "complete", "{}", r.to_dict());
    assert_eq!(eff.typed(), [("City".to_string(), "Lisbon".to_string())]);
    // a step that names no field: Jev's pick alone, under the cut-offs
    let unsure = FakeEffectors::default();
    let vague = plan(vec![json!({"kind": "type", "target": "the place field", "text": "Lisbon"})], "put Lisbon as the city");
    let low = Saying { p: 0.5, ..asker_saying("City") };
    let r = go(&vague, &FakeSenses::new(vec![form]), &unsure, &Desk::new("Safari"), Some(&low), none);
    assert!(r.status == "none" && r.reason == "ambiguous_field" && unsure.typed().is_empty(), "{}", r.to_dict());
}

#[test]
fn test_a_checkpoint_hands_back_with_the_ledger() {
    let eff = FakeEffectors::default();
    let p = plan(
        vec![json!({"kind": "click", "target": "week", "label_hint": "Week"}), json!({"kind": "checkpoint"}),
             json!({"kind": "click", "target": "today", "label_hint": "Today"})],
        "",
    );
    let senses = FakeSenses::new(vec![calendar(false), calendar(true)]).changed(&[Some(true)]);
    let r = go(&p, &senses, &eff, &Desk::new("Calendar"), None, keyword);
    assert!(r.status == "checkpoint" && r.next_index == 2 && eff.clicks() == ["Week"] && r.ledger.len() == 1, "{}", r.to_dict());
}

#[test]
fn test_an_app_that_does_not_open_stops_the_plan() {
    let desk = Desk { fail: true, ..Desk::new("Calendar") };
    let p = plan(vec![json!({"kind": "open_app", "target": "Nonesuch"}), json!({"kind": "click", "target": "x"})], "");
    let r = go(&p, &FakeSenses::new(vec![calendar(false)]), &FakeEffectors::default(), &desk, None, none);
    assert!(r.status == "none" && r.reason == "open_app_failed", "{}", r.to_dict());
    assert_eq!(r.ledger[0].note, "RuntimeError: no app called 'Nonesuch'");
}

#[test]
fn test_a_dry_run_touches_nothing() {
    let (eff, desk) = (FakeEffectors::default(), Desk::new("Finder"));
    let p = plan(
        vec![json!({"kind": "open_app", "target": "Calendar"}), json!({"kind": "click", "target": "week", "label_hint": "Week"}),
             json!({"kind": "type", "target": "search", "text": "x"})],
        "x",
    );
    let r = go(&p, &FakeSenses::new(vec![calendar(false)]), &eff, &desk, None, |o| {
        o.decide = "keyword".into();
        o.dry_run = true;
    });
    assert!(r.status == "complete" && r.reason == "dry_run", "{}", r.to_dict());
    assert!(eff.clicks().is_empty() && eff.typed().is_empty() && desk.opened().is_empty());
}

#[test]
fn test_the_app_the_plan_was_written_for_is_brought_back_before_the_first_step() {
    // the human clicked elsewhere while the planner thought
    let (eff, desk) = (FakeEffectors::default(), Desk::new("Finder"));
    let p = plan(vec![json!({"kind": "click", "target": "week", "label_hint": "Week"})], "");
    let senses = FakeSenses::new(vec![calendar(false), calendar(true)]).changed(&[Some(true)]);
    let r = go(&p, &senses, &eff, &desk, None, |o| {
        o.decide = "keyword".into();
        o.planned_for = "Calendar".into();
    });
    assert!(desk.opened() == ["Calendar"] && r.status == "complete" && eff.clicks() == ["Week"], "{}", r.to_dict());
    let still = Desk::new("Calendar");
    let senses = FakeSenses::new(vec![calendar(false), calendar(true)]).changed(&[Some(true)]);
    go(&p, &senses, &FakeEffectors::default(), &still, None, |o| {
        o.decide = "keyword".into();
        o.planned_for = "Calendar".into();
    });
    assert!(still.opened().is_empty()); // already in front: nothing is opened
}

// ==== test_plan_executor.py: a form of several fields (browser_model_eval contact_form, 2026-09-24) ===========

fn contact_form(focused: &str) -> Snapshot {
    // a page's field is clickable too (browser_lane)
    let web = || C { app: "browser", actions: Some(vec!["AXPress", "AXFocus"]), ..C::default() };
    let fields = vec![
        cand_with("1", "text field", "Full name", web()),
        cand_with("2", "text field", "Email address", web()),
        cand_with("3", "text field", "Your message", web()),
    ];
    let focus = fields.iter().find(|c| c.label == focused).cloned();
    let mut all = fields;
    all.push(cand_with("4", "button", "Send message", C { app: "browser", ..C::default() }));
    snap_with(all, S { app: "browser", bundle: "eve.browser", title: "Contact us".into(), focused: focus, ..S::default() })
}

#[test]
fn test_the_field_the_step_names_is_typed_into_when_jev_agrees_even_below_its_cut_offs() {
    // The eval's failure: three labelled fields, Jev's top pick right but under the step cut-offs -> ambiguous_field.
    let p = plan(
        vec![json!({"kind": "type", "target": "the focused Email address text field", "text": "ada@example.com"})],
        "fill in the email ada@example.com",
    );
    let eff = FakeEffectors::default();
    let asker = Saying { p: 0.4, ..asker_saying("Email address") };
    let r = go(&p, &FakeSenses::new(vec![contact_form("")]), &eff, &Desk::new("browser"), Some(&asker), none);
    assert!(r.status == "complete", "{}", r.to_dict());
    assert_eq!(eff.typed(), [("Email address".to_string(), "ada@example.com".to_string())]);
    assert_eq!(r.ledger[0].how, "code+jev");
}

#[test]
fn test_jev_still_refuses_a_named_field_it_reads_as_another() {
    let p = plan(
        vec![json!({"kind": "type", "target": "the Email address field", "text": "ada@example.com"})],
        "fill in the email ada@example.com",
    );
    let eff = FakeEffectors::default();
    let other = Saying { p: 0.4, ..asker_saying("Full name") };
    let r = go(&p, &FakeSenses::new(vec![contact_form("")]), &eff, &Desk::new("browser"), Some(&other), none);
    assert!(r.status == "none" && r.reason == "ambiguous_field" && eff.typed().is_empty(), "{}", r.to_dict());
    let risky = FakeEffectors::default();
    let says_risky = Saying { risky: 0.9, ..asker_saying("Email address") };
    let r = go(&p, &FakeSenses::new(vec![contact_form("")]), &risky, &Desk::new("browser"), Some(&says_risky), none);
    assert!(r.status == "needs_human" && risky.typed().is_empty(), "{}", r.to_dict());
}

#[test]
fn test_right_after_clicking_a_field_the_focused_field_is_the_proposal() {
    let p = plan(
        vec![json!({"kind": "click", "target": "the message box", "label_hint": "Your message"}),
             json!({"kind": "type", "target": "the text area", "text": "Please call me back"})],
        "write Please call me back in the message",
    );
    let eff = FakeEffectors::default();
    let senses = FakeSenses::new(vec![contact_form(""), contact_form("Your message"), contact_form("Your message")])
        .changed(&[Some(true)]);
    let asker = Saying { p: 0.4, ..asker_saying("Your message") };
    let r = go(&p, &senses, &eff, &Desk::new("browser"), Some(&asker), none);
    assert!(r.status == "complete" && eff.clicks() == ["Your message"], "{}", r.to_dict());
    assert_eq!(eff.typed(), [("Your message".to_string(), "Please call me back".to_string())]);
}

#[test]
fn test_the_longest_label_wins_and_a_repeated_label_names_nothing() {
    let pool = vec![cand("1", "text field", "Name"), cand("2", "text field", "Name for the booking")];
    let step = pc::parse_step(&json!({"kind": "type", "target": "the Name for the booking field", "text": "x"}), 1, "x").unwrap();
    assert_eq!(named_field(&step, &pool).unwrap().id, "2");
    let twins = vec![cand("1", "text field", "Name"), cand("2", "text field", "Name")];
    let step = pc::parse_step(&json!({"kind": "type", "target": "the name field", "text": "x"}), 1, "x").unwrap();
    assert!(named_field(&step, &twins).is_none());
}

#[test]
fn test_a_cookie_notice_is_dismissed_before_a_click_and_is_in_the_ledger() {
    let eff = FakeEffectors { consent: Some("dismissed the cookie notice with 'Reject all'".into()), ..Default::default() };
    let p = plan(vec![json!({"kind": "click", "target": "week", "label_hint": "Week"})], "");
    let senses = FakeSenses::new(vec![calendar(false), calendar(true)]).changed(&[Some(true)]);
    let r = go(&p, &senses, &eff, &Desk::new("Calendar"), None, keyword);
    assert!(r.status == "complete" && eff.cleared.get() == 1 && eff.clicks() == ["Week"], "{}", r.to_dict());
    let first = &r.ledger[0];
    assert_eq!((first.step.as_str(), first.effect.as_str(), first.how.as_str()), ("dismiss the cookie notice", "confirmed", "code"));
    // nothing to dismiss: nothing in the ledger
    let quiet = FakeEffectors { consent: Some(String::new()), ..Default::default() };
    let senses = FakeSenses::new(vec![calendar(false), calendar(true)]).changed(&[Some(true)]);
    let r = go(&p, &senses, &quiet, &Desk::new("Calendar"), None, keyword);
    assert_eq!(r.ledger.iter().map(|e| e.step.as_str()).collect::<Vec<_>>(), ["click week"]);
}

#[test]
fn test_the_humans_yes_to_a_flagged_step_reaches_the_effectors_own_gate_for_that_step_only() {
    let form = contact_form("");
    let p = plan(
        vec![json!({"kind": "click", "target": "button labelled Send message", "label_hint": "Send message",
                    "consequential": true})],
        "send the contact form",
    );
    let eff = FakeEffectors { gated: true, ..Default::default() };
    let first = go(&p, &FakeSenses::new(vec![form.clone()]), &eff, &Desk::new("browser"), Some(&asker_saying("Send message")), none);
    assert!(first.status == "needs_human" && eff.clicks().is_empty(), "{}", first.to_dict());
    let senses = FakeSenses::new(vec![form.clone(), form.clone(), form.clone(), form]);
    let confirm = first.confirm.clone();
    let r = go(&p, &senses, &eff, &Desk::new("browser"), Some(&asker_saying("Send message")), |o| {
        o.approved = HashMap::from([(0, confirm)]);
    });
    assert!(eff.clicks() == ["Send message"] && r.status != "partial", "{}", r.to_dict());
    assert_eq!(*eff.approve.borrow(), ""); // nothing carries over to a later step
}

// ==== the result's own shape ==================================================================================

#[test]
fn test_the_result_dict_keeps_pythons_keys_and_rounding() {
    let mut r = PlanResult::new("partial");
    r.ms = 12.345;
    let mut e = eve_desktop_lanes::plan_executor::Entry::new(1, "click week", "confirmed");
    e.note = "x".repeat(250);
    e.ms = 0.25;
    r.ledger.push(e);
    let d = r.to_dict();
    let keys: Vec<&str> = d.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["status", "next_index", "reason", "confirm", "sentence", "ms", "success_rejected", "ledger"]);
    assert_eq!(d["ms"], json!(12.3));
    let entry = &d["ledger"][0];
    let keys: Vec<&str> = entry.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["index", "step", "effect", "how", "note", "ms"]);
    assert_eq!(entry["note"].as_str().unwrap().len(), 200);
    assert_eq!(entry["ms"], json!(0.2)); // round(0.25, 1) == 0.2 in Python
}

#[test]
fn test_the_lanes_lines_are_read_back_as_python_reads_them() {
    use eve_desktop_lanes::plan_executor::{confirm_label, reason};
    assert_eq!(confirm_label(r#"confirm: "Delete \"x\"" needs the human's yes; app="Calendar""#, "f"), "Delete \"x\"");
    assert_eq!(confirm_label("confirm: \"\" needs the human's yes", "fallback"), "fallback");
    assert_eq!(confirm_label("escalate: reason=no_match", "fallback"), "fallback");
    assert_eq!(reason("escalate: reason=dialog_open: \"Save?\"; steps=0"), "dialog_open");
    assert_eq!(reason("escalate: reason=stalled (2 unchanged); steps=2"), "stalled");
    assert_eq!(reason("stopped: steps=1"), "");
}

// ==== test_plan_success_guard.py: a plan is DONE only when something observable says so ========================

/// test_plan_success_guard.raw.
fn raw(steps: Vec<Value>, success: Value, app: Option<&str>) -> Value {
    let full: Vec<Value> = steps.into_iter().map(full_step).collect();
    let mut r = json!({"needs_eyes": false, "why": "", "steps": full, "final_say": "All set.", "success": success});
    if let Some(app) = app {
        r["app"] = json!(app);
    }
    r
}

/// test_plan_success_guard.Page: a browser page whose URL is the last one opened.
struct Page {
    url: RefCell<String>,
    text: HashMap<String, HashSet<String>>,
    opened: RefCell<Vec<String>>,
}

impl Page {
    fn new(url: &str) -> Self {
        Page { url: RefCell::new(url.to_string()), text: HashMap::new(), opened: RefCell::new(Vec::new()) }
    }

    fn open_url(&self, url: &str) -> anyhow::Result<String> {
        self.opened.borrow_mut().push(url.to_string());
        *self.url.borrow_mut() = url.to_string();
        Ok(format!("opened {url}"))
    }
}

impl Senses for Page {
    fn snapshot(&self) -> anyhow::Result<Snapshot> {
        Ok(snap_with(vec![], S { app: "browser", title: self.url.borrow().clone(), ..S::default() }))
    }

    fn text_visible(&self, s: &str) -> anyhow::Result<bool> {
        Ok(self.text.get(self.url.borrow().as_str()).is_some_and(|t| t.contains(s)))
    }

    fn focused(&self) -> Option<Candidate> {
        None
    }

    fn frontmost_pid(&self) -> anyhow::Result<i64> {
        Ok(42)
    }

    fn screen_changed(&self) -> anyhow::Result<Option<bool>> {
        Ok(Some(true))
    }

    fn current_url(&self) -> Option<anyhow::Result<String>> {
        Some(Ok(self.url.borrow().clone()))
    }
}

/// test_plan_success_guard.go on a Page: keyword decide, no asker, frontmost "browser".
fn go_page(p: &Plan, page: &Page, eff: &FakeEffectors, kw: impl FnOnce(&mut PlanOptions<'_>)) -> PlanResult {
    let clock = FakeClock::new();
    let tick = || clock.tick();
    let mut opts = PlanOptions { clock: &tick, decide: "keyword".into(), ..PlanOptions::default() };
    kw(&mut opts);
    let desk = Desk { url_suffix: "", ..Desk::new("browser") };
    run_plan(p, page, eff, None, &|name| desk.open_app(name), &|url| page.open_url(url), &|| desk.frontmost_app(), opts)
}

fn outcome(kind: Kind, host: Option<&str>, query: Option<&str>) -> RequestOutcome {
    RequestOutcome::new(kind, host, query, None, 0.9, "laya")
}

fn browser_plan(request: &str, url: &str, success: Value, intent: pc::Intent<'_>) -> Plan {
    pc::parse_plan_with(
        &raw(vec![json!({"kind": "open_url", "target": url})], success, Some("browser")),
        request,
        ParseOptions { intent: Some(intent), ..ParseOptions::default() },
    )
    .unwrap()
}

/// test_browser_intent.pick: a Laya that always picks `kind`.
struct Pick(&'static str);

impl IntentAsker for Pick {
    fn ask(&self, _goal: &str, question: &Value, _state: &Value) -> anyhow::Result<Value> {
        let options: Vec<String> = question["criteria"].as_object().unwrap().keys().cloned().collect();
        let rest = (1.0 - 0.9) / (options.len().max(2) - 1) as f64;
        let probabilities: Map<String, Value> =
            options.iter().map(|k| (k.clone(), json!(if k == self.0 { 0.9 } else { rest }))).collect();
        Ok(json!({"type": "choice", "choice": self.0, "probabilities": probabilities, "confidence": 0.9}))
    }
}

fn laya_search(request: &str) -> anyhow::Result<Option<RequestOutcome>> {
    Ok(expected_outcome(request, Some(&Pick("search")), Some(&HashMap::new())))
}

fn ddg_cats(_: &str) -> anyhow::Result<Option<RequestOutcome>> {
    Ok(Some(outcome(Kind::Search, Some("duckduckgo.com"), Some("cats"))))
}

#[test]
fn test_a_success_check_that_holds_only_after_the_steps_is_complete() {
    let p = pc::parse_plan(
        &raw(vec![json!({"kind": "open_app", "target": "Calendar"})], json!({"kind": "text_visible", "value": "Week"}), None),
        "open calendar",
    )
    .unwrap();
    let senses = FakeSenses::new(vec![calendar(false), calendar(false)]).visible(&[&[], &["Week"]]);
    let r = go(&p, &senses, &FakeEffectors::default(), &Desk::new("Finder"), None, keyword);
    assert!(r.status == "complete" && r.sentence == "All set.", "{}", r.to_dict());
}

#[test]
fn test_a_success_check_already_true_before_step_1_is_rejected() {
    let p = browser_plan("search duck duck go for cats", "https://duckduckgo.com/?q=cats", Value::Null, &ddg_cats);
    let page = Page::new("https://duckduckgo.com/?q=Cats"); // the results were open before anything was done
    let r = go_page(&p, &page, &FakeEffectors::default(), |o| o.planned_for = "browser".into());
    assert!(r.status == "partial" && r.reason == "success_already_held", "{}", r.to_dict());
    assert!(r.success_rejected && r.to_dict()["success_rejected"] == json!(true));
    assert!(r.log_lines.iter().any(|line| line.contains("already holds before step 1")));
}

#[test]
fn test_a_resumed_plan_keeps_the_rejection_it_was_handed() {
    let p = pc::parse_plan_with(
        &raw(
            vec![json!({"kind": "open_url", "target": "https://example.com/"}),
                 json!({"kind": "open_url", "target": "https://duckduckgo.com/?q=cats"})],
            Value::Null,
            Some("browser"),
        ),
        "search duck duck go for cats",
        ParseOptions { intent: Some(&ddg_cats), ..ParseOptions::default() },
    )
    .unwrap();
    let page = Page::new("about:blank");
    let r = go_page(&p, &page, &FakeEffectors::default(), |o| {
        o.planned_for = "browser".into();
        o.start = 1;
        o.success_rejected = true;
    });
    assert!(r.status == "partial" && r.reason == "success_already_held", "{}", r.to_dict());
}

#[test]
fn test_the_pre_check_does_not_wait_for_a_check_that_is_false() {
    let p = pc::parse_plan_with(
        &raw(
            vec![json!({"kind": "open_url", "target": "https://example.com/"}),
                 json!({"kind": "open_url", "target": "https://www.google.com/search?q=dogs"})],
            Value::Null,
            Some("browser"),
        ),
        "search duck duck go for cats",
        ParseOptions { intent: Some(&ddg_cats), ..ParseOptions::default() },
    )
    .unwrap();
    let (fresh, resumed) = (FakeEffectors::default(), FakeEffectors::default());
    // start 0: reads the check before step 1
    go_page(&p, &Page::new("about:blank"), &fresh, |o| o.planned_for = "browser".into());
    // start 1: no pre-check
    go_page(&p, &Page::new("https://example.com/"), &resumed, |o| {
        o.planned_for = "browser".into();
        o.start = 1;
    });
    let polls = |e: &FakeEffectors| e.settles.borrow().iter().filter(|s| **s == 0.4).count();
    assert_eq!(polls(&fresh), polls(&resumed)); // the pre-check added no polling
}

#[test]
fn test_duck_duck_go_by_google_is_not_done() {
    let p = browser_plan(
        "search duck duck go for cats",
        "https://www.google.com/search?q=cats",
        json!({"kind": "text_visible", "value": "cats"}),
        &laya_search,
    );
    let success = p.success.clone().unwrap();
    assert!(success.kind == "url_query" && success.host == "duckduckgo.com");
    // the planner's own check would hold
    let mut page = Page::new("about:blank");
    page.text.insert("https://www.google.com/search?q=cats".into(), HashSet::from(["cats".to_string()]));
    let r = go_page(&p, &page, &FakeEffectors::default(), |o| o.planned_for = "browser".into());
    assert!(r.status == "partial" && r.reason.contains("cats"), "{}", r.to_dict());
}

#[test]
fn test_duck_duck_go_by_duck_duck_go_is_done() {
    // the planner gave no success
    let p = browser_plan("search duck duck go for cats", "https://duckduckgo.com/?q=Cats", Value::Null, &laya_search);
    let r = go_page(&p, &Page::new("about:blank"), &FakeEffectors::default(), |o| o.planned_for = "browser".into());
    assert!(r.status == "complete" && r.sentence == "All set.", "{}", r.to_dict());
}

#[test]
fn test_the_request_check_replaces_the_planners_choice() {
    // The planner checked for a page it chose; the request said github.
    let github = |_: &str| -> anyhow::Result<Option<RequestOutcome>> {
        Ok(Some(outcome(Kind::Search, Some("github.com"), Some("laya"))))
    };
    let p = browser_plan(
        "search git hub for laya",
        "https://www.google.com/search?q=laya",
        json!({"kind": "url_is", "value": "https://www.google.com/search?q=laya"}),
        &github,
    );
    let r = go_page(&p, &Page::new("about:blank"), &FakeEffectors::default(), |o| o.planned_for = "browser".into());
    assert_eq!(r.status, "partial", "{}", r.to_dict());
}

#[test]
fn test_a_request_check_already_true_before_step_1_is_rejected() {
    let p = browser_plan("search duck duck go for cats", "https://duckduckgo.com/?q=cats", Value::Null, &ddg_cats);
    let page = Page::new("https://duckduckgo.com/?q=cats"); // already on that results page
    let r = go_page(&p, &page, &FakeEffectors::default(), |o| o.planned_for = "browser".into());
    assert!(r.status == "partial" && r.reason == "success_already_held", "{}", r.to_dict());
}

#[test]
fn test_no_outcome_falls_back_to_the_planners_check() {
    let url = "https://example.com/pricing";
    let nothing = |_: &str| -> anyhow::Result<Option<RequestOutcome>> { Ok(None) };
    let site = |_: &str| -> anyhow::Result<Option<RequestOutcome>> { Ok(Some(outcome(Kind::OpenSite, Some("github.com"), None))) };
    let no_kind = |_: &str| -> anyhow::Result<Option<RequestOutcome>> { Ok(Some(outcome(Kind::None, None, None))) };
    let intents: [pc::Intent<'_>; 3] = [&nothing, &site, &no_kind];
    for intent in intents {
        let p = browser_plan("show me example's pricing", url, json!({"kind": "url_is", "value": url}), intent);
        assert!(p.success.as_ref().is_some_and(|s| s.kind == "url_is"));
        let r = go_page(&p, &Page::new("about:blank"), &FakeEffectors::default(), |o| o.planned_for = "browser".into());
        assert_eq!(r.status, "complete", "{}", r.to_dict());
    }
    // No derived check and none from the planner: as before F1's rule, it completes (test_plan_scope).
    let p = browser_plan("show me example's pricing", url, Value::Null, &nothing);
    let r = go_page(&p, &Page::new("about:blank"), &FakeEffectors::default(), |o| o.planned_for = "browser".into());
    assert_eq!(r.status, "complete", "{}", r.to_dict());
}

// ==== test_plan_url_expectation.py: the url oracle =============================================================

/// test_plan_url_expectation.Senses: current_url is the URL of the last snapshot taken.
struct UrlSenses {
    url: String,
    fail: bool,
    fresh: RefCell<String>,
    reads: Cell<usize>,
}

impl UrlSenses {
    fn new(url: &str, fail: bool) -> Self {
        UrlSenses { url: url.to_string(), fail, fresh: RefCell::new(String::new()), reads: Cell::new(0) }
    }
}

impl Senses for UrlSenses {
    fn snapshot(&self) -> anyhow::Result<Snapshot> {
        self.reads.set(self.reads.get() + 1);
        if self.fail {
            return Err(eve_desktop_lanes::LaneError::err("RuntimeError", "detached"));
        }
        *self.fresh.borrow_mut() = self.url.clone();
        Ok(Snapshot { title: "Generic destination".into(), ..Default::default() })
    }

    fn text_visible(&self, _s: &str) -> anyhow::Result<bool> {
        Ok(false)
    }

    fn focused(&self) -> Option<Candidate> {
        None
    }

    fn frontmost_pid(&self) -> anyhow::Result<i64> {
        Ok(42)
    }

    fn screen_changed(&self) -> anyhow::Result<Option<bool>> {
        Ok(None)
    }

    fn current_url(&self) -> Option<anyhow::Result<String>> {
        Some(Ok(self.fresh.borrow().clone()))
    }
}

/// Senses with a snapshot and nothing else: no current_url.
struct TitleOnly;

impl Senses for TitleOnly {
    fn snapshot(&self) -> anyhow::Result<Snapshot> {
        Ok(Snapshot { title: "Article".into(), ..Default::default() })
    }

    fn text_visible(&self, _s: &str) -> anyhow::Result<bool> {
        Ok(false)
    }

    fn focused(&self) -> Option<Candidate> {
        None
    }

    fn frontmost_pid(&self) -> anyhow::Result<i64> {
        Ok(42)
    }

    fn screen_changed(&self) -> anyhow::Result<Option<bool>> {
        Ok(None)
    }
}

fn url_holds(expect: Expect, senses: &dyn Senses) -> Option<bool> {
    let clock = FakeClock::new();
    holds(Some(&expect), senses, &FakeEffectors::default(), &|| "browser".to_string(), &|| clock.tick(), 0.0)
}

#[test]
fn test_exact_url_requires_fresh_snapshot_and_does_not_replace_title_predicate() {
    let s = UrlSenses::new("https://example.com/article", false);
    assert_eq!(url_holds(Expect::new("url_is", s.url.clone()), &s), Some(true));
    assert_eq!(s.reads.get(), 1);
    assert_eq!(url_holds(Expect::new("title_contains", "Article"), &s), Some(false));
    assert_eq!(url_holds(Expect::new("url_is", "https://example.com/other"), &s), Some(false));
    assert_eq!(url_holds(Expect::new("url_is", s.url.clone()), &UrlSenses::new(&s.url, true)), Some(false));
    assert_eq!(url_holds(Expect::new("url_is", s.url.clone()), &TitleOnly), Some(false));
}

#[test]
fn test_an_unreadable_oracle_is_unknown_except_for_a_url() {
    let failing = UrlSenses::new("https://example.com/", true);
    assert_eq!(url_holds(Expect::new("text_visible", "x"), &failing), None);
    assert_eq!(url_holds(Expect::new("title_contains", "x"), &failing), None);
    let clock = FakeClock::new();
    assert_eq!(holds(None, &failing, &FakeEffectors::default(), &|| String::new(), &|| clock.tick(), 0.0), None);
    let front = holds(Some(&Expect::new("app_frontmost", "safari")), &failing, &FakeEffectors::default(),
                      &|| "Safari".to_string(), &|| clock.tick(), 0.0);
    assert_eq!(front, Some(true));
}

// ==== test_vault_fill.py: the executor's fill step ==============================================================

/// A Senses that is never read (Python passed `senses=None`).
struct NoSenses;

impl Senses for NoSenses {
    fn snapshot(&self) -> anyhow::Result<Snapshot> {
        unreachable!("a fill plan never takes a snapshot")
    }

    fn text_visible(&self, _s: &str) -> anyhow::Result<bool> {
        unreachable!("a fill plan never reads text")
    }

    fn focused(&self) -> Option<Candidate> {
        None
    }

    fn frontmost_pid(&self) -> anyhow::Result<i64> {
        Ok(0)
    }

    fn screen_changed(&self) -> anyhow::Result<Option<bool>> {
        Ok(None)
    }
}

fn fill_run(effectors: &FakeEffectors) -> PlanResult {
    let plan = pc::parse_plan(
        &json!({"steps": [{"kind": "fill", "target": "the sign-in form", "text": "v_ab12cd34"}],
                "final_say": "Filled.", "success": null}),
        "sign in",
    )
    .unwrap();
    run_plan(
        &plan,
        &NoSenses,
        effectors,
        None,
        &|s| Ok(s.to_string()),
        &|s| Ok(s.to_string()),
        &|| "browser".to_string(),
        PlanOptions::default(),
    )
}

#[test]
fn test_the_executor_runs_a_fill_through_the_page_and_stops_on_a_refusal() {
    let ok = FakeEffectors { vault: Some("filled username, password from 'Chase'".into()), ..Default::default() };
    let result = fill_run(&ok);
    assert!(*ok.asked.borrow() == ["v_ab12cd34"] && result.status == "complete", "{}", result.to_dict());
    assert!(result.ledger[0].effect == "confirmed" && result.ledger[0].how == "code");
    let half = fill_run(&FakeEffectors {
        vault: Some("filled username from 'Chase' but could not fill the password (TimeoutError)".into()),
        ..Default::default()
    });
    assert!(half.status == "complete" && half.ledger[0].effect == "unverifiable", "{}", half.to_dict());
    let refused = fill_run(&FakeEffectors {
        vault: Some("refused: \"Chase\" is saved for chase.com, not this page".into()),
        ..Default::default()
    });
    assert!(refused.status == "none" && refused.reason == "fill_refused", "{}", refused.to_dict());
    let mac = fill_run(&FakeEffectors::default());
    assert!(mac.status == "none" && mac.ledger[0].note.contains("only in the browser"), "{}", mac.to_dict());
}

// ==== test_task_router.py: the classifier =======================================================================

const APPS: &[&str] = &[
    "Spotify", "TV", "Terminal", "Safari", "Slack", "Google Chrome", "System Settings", "Notes", "Mail", "Music",
    "Reminders", "Preview", "Calendar",
];

fn apps(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}

fn classify(goal: &str) -> tr::Plan {
    tr::classify(goal, "", &apps(APPS), None, None)
}

fn tiers(p: &tr::Plan) -> Vec<&str> {
    p.tiers.iter().map(String::as_str).collect()
}

#[test]
fn test_a_bare_launch_of_an_installed_app_is_a_complete_reflex() {
    for (goal, app) in [
        ("Open Spotify.", "Spotify"),
        ("open up Spotify on my laptop", "Spotify"),
        ("Open up Apple TV on my Mac.", "TV"),
        ("Can you fire up Slack for me?", "Slack"),
        ("Would you mind opening Preview?", "Preview"),
        ("fire up system settings", "System Settings"),
        ("Open Spotify on the Mac and bring it to the front so you can start listening.", "Spotify"),
    ] {
        let p = classify(goal);
        assert!(p.kind == "launch" && tiers(&p) == ["reflex"] && p.app == app, "{goal}: {p:?}");
        assert!(p.complete() && p.code() == format!("open_app('{app}', 4.0)") && p.sentence() == format!("Opened {app}."));
    }
}

#[test]
fn test_a_launch_with_more_to_do_hands_the_rest_to_the_planner() {
    let p = classify("Open Spotify and play \"Don Toliver\"");
    assert!(p.kind == "launch" && tiers(&p) == ["reflex", "astra"] && p.app == "Spotify" && p.rest.contains("play"), "{p:?}");
    assert!(!p.complete());
}

#[test]
fn test_what_looks_like_a_launch_and_is_not() {
    for goal in [
        "Open the downloads folder.", "Open notes from yesterday's meeting.", "Show me photos of the trip.",
        "Open a new Astra 6 session.", "Open Figma.", "Play Spotify", "Pause Spotify.",
    ] {
        let p = classify(goal);
        assert!(!p.tiers.iter().any(|t| t == "reflex"), "{goal}: {p:?}");
    }
    for goal in [
        "Force quit Spotify.", "Open Mail and send the draft to Sam.", "Is Slack open?",
        "Open Reminders and add a reminder to call the dentist.",
        "Can you open the Music app and tell me what's playing?",
        "Write this into the Terminal right now: \"hello\"",
    ] {
        // consequential, typing, or needs eyes: planner only
        assert_eq!(tiers(&classify(goal)), ["astra"], "{goal}");
    }
}

#[test]
fn test_an_explicit_web_search_is_a_reflex_with_the_query_found_by_code() {
    let p = classify("Search for the closest gym in San Francisco using Google.");
    assert!(p.kind == "search" && p.complete() && p.query == "the closest gym in San Francisco", "{p:?}");
    assert_eq!(p.code(), "open_url('https://www.google.com/search?q=the+closest+gym+in+San+Francisco')");
    assert_eq!(
        classify("Pull up an article on Google on my laptop right now about the two leaders meeting.").query,
        "an article about the two leaders meeting"
    );
    let told = classify("In Safari, search Google for the weather in San Francisco and tell me the temperature you see.");
    assert!(tiers(&told) == ["reflex", "astra"] && told.browser == "Safari" && told.query == "the weather in San Francisco", "{told:?}");
    assert!(told.code().contains("app='Safari'"));
    // a web search is harmless whatever its query says …
    assert!(classify("Google how to reset a Casio watch.").complete());
    assert_eq!(classify("search the web for how to delete a git branch").query, "how to delete a git branch");
    // … but a risky ACTION after the search is the planner's
    assert_eq!(tiers(&classify("Search for wireless earbuds and buy the cheapest pair.")), ["astra"]);
}

#[test]
fn test_what_looks_like_a_web_search_and_is_not() {
    for goal in [
        "Search Spotify for jazz.", "Search for the budget thread in Slack.", "look up Priya Raman in my contacts",
        "Pull up the budget.", "Find my keys.", "Search up some cool shit for me on Google right now.",
        "Pull up something cool on Google.",
    ] {
        assert!(!classify(goal).tiers.iter().any(|t| t == "reflex"), "{goal}");
    }
    assert!(classify("Search Google for calendar templates for 2027.").complete()); // an app's name as a query word
}

#[test]
fn test_a_model_is_asked_only_after_the_gates_and_may_only_add_a_bare_launch() {
    let asked: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let says = |app: &'static str| {
        let asked = &asked;
        move |text: &str, _apps: &[String]| -> anyhow::Result<String> {
            asked.borrow_mut().push(text.to_string());
            Ok(app.to_string())
        }
    };
    let installed = apps(APPS);
    let with_model = |goal: &str, model: tr::RouterModel<'_>| tr::classify(goal, "", &installed, Some(model), None);

    let notes = says("Notes");
    let p = with_model("Notes, please.", &notes);
    assert!(p.kind == "launch" && tiers(&p) == ["reflex"] && p.app == "Notes" && p.reasons[0].contains("model"), "{p:?}");
    let spotify = says("Spotify");
    // the code gate first
    assert_eq!(tiers(&with_model("get Spotify going and delete my playlist", &spotify)), ["astra"]);
    assert_eq!(*asked.borrow(), ["Notes, please."]);
    // rules fire: not asked
    assert_eq!(with_model("Open Spotify.", &notes).app, "Spotify");
    assert_eq!(*asked.borrow(), ["Notes, please."]);
    let figma = says("Figma");
    assert!(!with_model("Take me to Figma.", &figma).tiers.iter().any(|t| t == "reflex")); // not installed
    let nothing = says("");
    assert!(!with_model("Take me somewhere nice.", &nothing).tiers.iter().any(|t| t == "reflex")); // abstained
    let boom = |_: &str, _: &[String]| -> anyhow::Result<String> { anyhow::bail!("network down") };
    assert_eq!(tiers(&with_model("Notes, please.", &boom)), ["lane", "astra"]);
    assert_eq!(tr::find_app_mention("put on apple tv", &installed), "TV");
    assert_eq!(tr::find_app_mention("chrome please", &installed), "Google Chrome");
}

#[test]
fn test_installed_apps_lists_real_names_once() {
    let listing = |_: &str| -> std::io::Result<Vec<String>> {
        Ok(vec!["Spotify.app".into(), "TV.app".into(), "notes.txt".into(), "Spotify.app".into()])
    };
    assert_eq!(tr::installed_apps_in(&["/x"], &listing), ["Spotify", "TV"]);
    let missing = |_: &str| -> std::io::Result<Vec<String>> { Err(std::io::Error::other("no such folder")) };
    assert!(tr::installed_apps_in(&["/missing"], &missing).is_empty());
    // no folders named: the usual ones, each read once, with "~" expanded
    let seen: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let record = |d: &str| -> std::io::Result<Vec<String>> {
        seen.borrow_mut().push(d.to_string());
        Ok(vec![])
    };
    assert!(tr::installed_apps_in(&[], &record).is_empty());
    let seen = seen.into_inner();
    assert_eq!(seen.len(), 4);
    assert_eq!(&seen[..3], ["/Applications", "/System/Applications", "/System/Applications/Utilities"]);
    assert!(!seen[3].starts_with('~') && seen[3].ends_with("/Applications"));
}

#[test]
fn test_the_routing_doc_states_the_shipped_defaults_and_names_every_knob() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let text = std::fs::read_to_string(root.join("docs/stackchan/routing.md")).unwrap();
    let row = |knob: &str| -> String {
        let pattern = format!(r"(?m)^\| `{}` \| `([^`]*)` \|", regex::escape(knob));
        let caps = regex::Regex::new(&pattern).unwrap().captures(&text).unwrap_or_else(|| panic!("{knob}"));
        caps[1].to_string()
    };
    assert_eq!(row("EVE_REFLEXES"), if tr::REFLEX_DEFAULT { "1" } else { "0" });
    assert_eq!(row("EVE_ROUTER_MODEL"), tr::ROUTER_MODEL_DEFAULT);
    assert!(row("EVE_HEAD_MODEL") == "off" && text.contains("already done"));
    assert_eq!(row("EVE_LANE_FIRST"), if LANE_FIRST_DEFAULT { "1" } else { "0" });
    assert!(row("EVE_FAST_LANE_DECIDE") == DEFAULT_DECIDE && row("EVE_DECIDER") == "unset");
    for phrase in [
        "absolute noul gate", "laya ranks", "Jev judges", "holdout 3", "thin but real", "typed_ask.py",
        "tools/route_eval.py", "tools/head_eval.py", "unsafe",
    ] {
        assert!(text.contains(phrase), "{phrase}");
    }
    let voice = std::fs::read_to_string(root.join("docs/stackchan/voice.md")).unwrap();
    assert!(voice.contains("EVE_REFLEXES") && voice.contains("EVE_ROUTER_MODEL") && voice.contains("routing.md"));
    assert!(std::fs::read_to_string(root.join("README.md")).unwrap().contains("routing.md"));
}

#[test]
fn test_is_question_is_one_rule_for_both_routers() {
    assert!(is_question("what's playing?", false) && is_question("is Slack open", false) && is_question("open it?", false));
    assert!(!is_question("open it?", true)); // "can you open it?" is a request
    assert!(!is_question("open Spotify", false));
}

#[test]
fn test_app_names_said_as_one_word_and_every_mention() {
    let photo = apps(&["Photo Booth", "Photos"]);
    assert_eq!(tr::match_app("photobooth", &photo), "Photo Booth");
    assert_eq!(tr::match_app("photo booth", &photo), "Photo Booth");
    assert_eq!(tr::match_app("photos", &photo), "Photos");
    assert_eq!(tr::match_app("photobooth", &apps(&["Photo Booth", "Photob Ooth"])), ""); // two apps have those letters
    let two = apps(&["Spotify", "Google Chrome"]);
    assert_eq!(tr::app_mentions("open spotify and chrome", &two), ["Spotify", "Google Chrome"]);
    assert_eq!(tr::app_mentions("take a photo in photo booth", &photo), ["Photo Booth"]);
    assert_eq!(tr::find_app_mention("open spotify and chrome", &two), "");
    assert_eq!(tr::classify("open photobooth", "", &photo, None, None).app, "Photo Booth");
}

#[test]
fn test_folded_names_are_shown_title_cased_and_the_code_line_quotes_like_python() {
    let names = tr::display_names(&apps(&["spotify", "google  chrome", "TV", " "]));
    assert_eq!(names.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect::<Vec<_>>(),
               [("spotify", "Spotify"), ("google chrome", "Google Chrome"), ("tv", "TV")]);
    let mut p = tr::Plan::new("launch", &["reflex"]);
    p.app = "Bob's App".into();
    assert_eq!(p.code(), "open_app(\"Bob's App\", 4.0)");
    let mut s = tr::Plan::new("search", &["reflex"]);
    s.query = "café & crème".into();
    assert_eq!(s.code(), "open_url('https://www.google.com/search?q=caf%C3%A9+%26+cr%C3%A8me')");
    s.query = "x".repeat(70);
    assert_eq!(s.sentence(), format!("Here's a search for {}….", "x".repeat(59)));
    assert_eq!(tr::Plan::new("lane", &["lane", "astra"]).code(), "");
}

// ==== test_quit_reflex.py: quitting by the word "quit" ==========================================================

const QUIT_APPS: &[&str] = &["Spotify", "Safari", "Google Chrome", "System Settings", "Notes", "Warp", "TV", "Messages", "Slack"];

#[test]
fn test_rules_take_a_bare_quit() {
    for (goal, kind, app) in [
        ("Quit Spotify.", "quit", "Spotify"),
        ("can you quit spotify for me", "quit", "Spotify"),
        ("go ahead and quit out of chrome", "quit", "Google Chrome"),
        ("quit system preferences", "quit", "System Settings"),
        ("Quit Apple TV please", "quit", "TV"),
        ("please quit all", "quit_all", ""),
        ("quit everything", "quit_all", ""),
        ("quit all my open apps", "quit_all", ""),
    ] {
        let p = tr::classify(goal, "", &apps(QUIT_APPS), None, None);
        assert!(p.kind == kind && p.app == app && p.complete(), "{goal}: {p:?}");
    }
}

#[test]
fn test_everything_else_is_never_a_quit() {
    for goal in [
        "force quit Spotify", "quit spotify and open notes", "close Safari", "exit Slack", "kill Spotify",
        "should I quit Slack?", "is Spotify still running?", "quit all the Safari tabs", "I quit!", "quit Photoshop",
        "quit Notes and Slack",
    ] {
        let p = tr::classify(goal, "", &apps(QUIT_APPS), None, None);
        assert!(p.kind != "quit" && p.kind != "quit_all" && !p.complete(), "{goal}: {p:?}");
    }
}

#[test]
fn test_jev_is_asked_only_behind_the_quit_word_gate() {
    let installed = apps(QUIT_APPS);
    let asked: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let model = |goal: &str, _apps: &[String]| -> anyhow::Result<String> {
        asked.borrow_mut().push(goal.to_string());
        Ok("Spotify".to_string())
    };
    assert_eq!(tr::classify("Spotify, quit it.", "", &installed, None, Some(&model)).app, "Spotify");
    for goal in ["close Spotify", "force quit Spotify", "is Spotify quitting?", "Quit Spotify."] {
        tr::classify(goal, "", &installed, None, Some(&model));
    }
    // the rules took "Quit Spotify."; the rest never reach it
    assert_eq!(*asked.borrow(), ["Spotify, quit it."]);
    let photoshop = |_: &str, _: &[String]| -> anyhow::Result<String> { Ok("Photoshop".into()) };
    assert_eq!(tr::classify("Photoshop, quit it", "", &installed, None, Some(&photoshop)).kind, "astra");
    let every = |_: &str, _: &[String]| -> anyhow::Result<String> { Ok("*".into()) };
    assert_eq!(tr::classify("everything, quit it", "", &installed, None, Some(&every)).kind, "quit_all");
    let boom = |_: &str, _: &[String]| -> anyhow::Result<String> { anyhow::bail!("TimeoutError") };
    assert_eq!(tr::classify("Spotify, quit it.", "", &installed, None, Some(&boom)).kind, "astra");
}

// ==== test_site_names.py: the router sees joined names ==========================================================

#[test]
fn test_the_router_sees_joined_names() {
    let safari = apps(&["Safari"]);
    let p = tr::classify("search for git hub copilot pricing", "", &safari, None, None);
    assert!(p.kind == "search" && p.query == "GitHub copilot pricing", "{p:?}");
    let p = tr::classify("look up you tube premium plans", "", &safari, None, None);
    assert!(p.kind == "search" && p.query == "YouTube premium plans", "{p:?}");
}
