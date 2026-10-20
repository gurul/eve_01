//! fast_lane, lane_router and decision_lane with fake senses, effectors, deciders, askers and clients. Ports of
//! bridge/tests/test_fast_lane.py, test_lane_first.py, test_decision_lane.py, and the lane parts of
//! test_jev_step.py, test_jev_grouped.py, test_agent_lane_first.py and test_fastlane_eval.py.
//!
//! Objectives are chosen for the oracle under test: "switch to week view" uniquely matches Week, so the keyword gate
//! decides; "show the next seven days" matches nothing, so the model (the fake Decider) is asked.

use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicI64, Ordering};

use image::{DynamicImage, Rgb, RgbImage};
use indexmap::IndexMap;
use serde_json::{json, Map, Value};

use eve_decider::{render_option, Choice};
use eve_decisions::{Answer, Evaluate};
use eve_desktop_lanes::ax_candidates::{Candidate, Snapshot};
use eve_desktop_lanes::decision_lane as dl;
use eve_desktop_lanes::desktop_helpers::LaneAdapter;
use eve_desktop_lanes::fast_lane::{
    self as fl, describe_pick, format_line, gate, run_delegate, run_script, DelegateOptions, DelegateResult,
    Effectors as EffectorsTrait, GateState, LaneDecider, LaneStatus, LineFields, MenuItem, ScriptOptions,
    Senses as SensesTrait, StepAsk, Thresholds, SYSTEM_SETTINGS_BUNDLE,
};
use eve_desktop_lanes::lane_router::{self as lr, RouteOptions};
use eve_desktop_lanes::LaneError;
use eve_typed_ask::{StepAnswer, GROUPED_MAX_OPTIONS, STEP_NONE};

// ---- fakes -------------------------------------------------------------------------------------

fn cand(cid: &str, role: &str, label: &str) -> Candidate {
    candv(cid, role, label, "")
}

fn candv(cid: &str, role: &str, label: &str, value: &str) -> Candidate {
    let x = 100 + 70 * cid.parse::<i64>().expect("numeric id");
    let editable = ["text field", "search field", "text area", "combo box"].contains(&role);
    Candidate {
        id: cid.into(),
        role: role.into(),
        label: label.into(),
        value: value.into(),
        frame: (x, 100, 60, 30),
        actions: if editable { vec![] } else { vec!["AXPress".into()] },
        enabled: true,
        app: "Calendar".into(),
        in_content: false,
        in_dialog: false,
        secure: false,
        editable,
    }
}

static SEQ: AtomicI64 = AtomicI64::new(0);

struct SnapOpts {
    app: &'static str,
    bundle: &'static str,
    title: &'static str,
    pid: i64,
    dialog_text: &'static str,
    truncated: bool,
    node_count: Option<i64>,
    focused: Option<Candidate>,
}

impl Default for SnapOpts {
    fn default() -> Self {
        SnapOpts {
            app: "Calendar",
            bundle: "com.apple.iCal",
            title: "September 2026",
            pid: 42,
            dialog_text: "",
            truncated: false,
            node_count: None,
            focused: None,
        }
    }
}

fn snap_with(cands: Vec<Candidate>, o: SnapOpts) -> Snapshot {
    let seq = SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    let count = o.node_count.unwrap_or(cands.len() as i64 + 3);
    Snapshot {
        seq,
        app: o.app.into(),
        bundle: o.bundle.into(),
        title: o.title.into(),
        pid: o.pid,
        elements: cands,
        context_lines: vec![format!("title: {}", o.title)],
        focused: o.focused,
        dialog_text: o.dialog_text.into(),
        node_count: count,
        truncated: o.truncated,
        secs: 0.05,
        ..Snapshot::default()
    }
}

fn snap(cands: Vec<Candidate>) -> Snapshot {
    snap_with(cands, SnapOpts::default())
}

fn calendar(week_selected: bool) -> Snapshot {
    snap(vec![
        cand("0", "button", "Add Event"),
        cand("1", "radio button", "Day"),
        candv("2", "radio button", "Week", if week_selected { "selected" } else { "" }),
        candv(
            "3",
            "radio button",
            "Month",
            if week_selected { "" } else { "selected" },
        ),
        cand("4", "button", "Today"),
        cand("5", "button", "Delete Event"),
        cand("6", "search field", "Search"),
    ])
}

struct Senses {
    snapshots: Vec<Snapshot>,
    visible: Vec<HashSet<String>>,
    changed: Vec<Option<bool>>,
    pid: i64,
    calls: Cell<usize>,
}

impl Senses {
    fn new(snapshots: Vec<Snapshot>) -> Self {
        Senses {
            snapshots,
            visible: vec![HashSet::new()],
            changed: vec![None],
            pid: 42,
            calls: Cell::new(0),
        }
    }

    fn changed(mut self, changed: Vec<Option<bool>>) -> Self {
        self.changed = changed;
        self
    }

    fn visible(mut self, visible: Vec<&[&str]>) -> Self {
        self.visible = visible
            .into_iter()
            .map(|v| v.iter().map(|s| s.to_string()).collect())
            .collect();
        self
    }

    fn pid(mut self, pid: i64) -> Self {
        self.pid = pid;
        self
    }

    fn calls(&self) -> usize {
        self.calls.get()
    }
}

impl SensesTrait for Senses {
    fn snapshot(&self) -> anyhow::Result<Snapshot> {
        let i = self.calls.get().min(self.snapshots.len() - 1);
        self.calls.set(self.calls.get() + 1);
        Ok(self.snapshots[i].clone())
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
        Ok(self.pid)
    }

    fn screen_changed(&self) -> anyhow::Result<Option<bool>> {
        let idx = self.calls.get().saturating_sub(2).min(self.changed.len() - 1);
        Ok(self.changed[idx])
    }
}

#[derive(Default)]
struct Effectors {
    clicks: RefCell<Vec<String>>,
    typed: RefCell<Vec<(String, String)>>,
    presses: RefCell<Vec<String>>,
    settles: RefCell<Vec<f64>>,
    refuse_click: bool,
    refuse_type: bool,
    raise_after_click: bool,
}

impl Effectors {
    fn clicks(&self) -> Vec<String> {
        self.clicks.borrow().clone()
    }
}

impl EffectorsTrait for Effectors {
    fn click_candidate(&self, c: &Candidate) -> anyhow::Result<String> {
        if self.refuse_click {
            return Ok(format!("refused: nothing pressable at the centre of '{}'", c.label));
        }
        self.clicks.borrow_mut().push(c.label.clone());
        if self.raise_after_click {
            return Err(LaneError::err("RuntimeError", "after delivery"));
        }
        Ok(format!("clicked {} '{}'", c.role, c.label))
    }

    fn focus_and_type(&self, c: &Candidate, text: &str) -> anyhow::Result<String> {
        if self.refuse_type {
            return Ok("refused: the focused element is not the field".into());
        }
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
}

struct DecideCall {
    app: String,
    context: String,
    recent: Vec<String>,
}

/// Picks by option-text substring ("Week"), or a reserved key, or "error"; records every menu.
struct Decider {
    picks: RefCell<VecDeque<String>>,
    p_top: f64,
    margin: f64,
    available: bool,
    menus: RefCell<Vec<IndexMap<String, String>>>,
    calls: RefCell<Vec<DecideCall>>,
}

impl Decider {
    fn new(picks: &[&str]) -> Self {
        Decider {
            picks: RefCell::new(picks.iter().map(|s| s.to_string()).collect()),
            p_top: 0.7,
            margin: 0.4,
            available: true,
            menus: RefCell::new(Vec::new()),
            calls: RefCell::new(Vec::new()),
        }
    }

    fn odds(mut self, p_top: f64, margin: f64) -> Self {
        self.p_top = p_top;
        self.margin = margin;
        self
    }

    fn menus(&self) -> Vec<IndexMap<String, String>> {
        self.menus.borrow().clone()
    }
}

impl LaneDecider for Decider {
    fn available(&self) -> bool {
        self.available
    }

    fn choose(
        &self,
        _objective: &str,
        app: &str,
        context: &str,
        options: &IndexMap<String, String>,
        recent: &[String],
    ) -> anyhow::Result<Choice> {
        self.menus.borrow_mut().push(options.clone());
        self.calls.borrow_mut().push(DecideCall {
            app: app.into(),
            context: context.into(),
            recent: recent.to_vec(),
        });
        let pick = self.picks.borrow_mut().pop_front().unwrap_or_else(|| "abstain".into());
        let mut probs: IndexMap<String, f64> = options.keys().map(|k| (k.clone(), 0.0)).collect();
        if pick == "error" {
            return Ok(Choice {
                error: "predict raised RuntimeError: boom".into(),
                k: options.len(),
                ..Choice::default()
            });
        }
        let key = if pick == "reobserve" || pick == "abstain" {
            pick.clone()
        } else {
            let found = options
                .iter()
                .find(|(_, v)| v.to_lowercase().contains(&pick.to_lowercase()))
                .map(|(k, _)| k.clone());
            match found {
                Some(k) => k,
                None => {
                    return Err(LaneError::err(
                        "AssertionError",
                        format!("{pick:?} is not in the menu {options:?}"),
                    ))
                }
            }
        };
        probs.insert(key.clone(), self.p_top);
        Ok(Choice {
            id: key,
            p_top: self.p_top,
            margin: self.margin,
            confidence: 0.3,
            probabilities: probs,
            ms: 9.0,
            input_tokens: 200,
            k: options.len(),
            ..Choice::default()
        })
    }
}

/// A decider that fails the test the moment anyone asks it: the positive control for "the model is out of the
/// click path".
struct NeverAsked;

impl LaneDecider for NeverAsked {
    fn available(&self) -> bool {
        true
    }

    fn choose(&self, _: &str, _: &str, _: &str, _: &IndexMap<String, String>, _: &[String]) -> anyhow::Result<Choice> {
        panic!("the keyword lane asked the model")
    }
}

fn fake_clock() -> impl Fn() -> f64 {
    let t = Cell::new(0.0);
    move || {
        t.set(t.get() + 0.001);
        t.get()
    }
}

/// test_fast_lane's `run`: these tests pin the lane's gates on the path where a model answers, so `decide` is
/// "model" unless `set` says otherwise.
fn run(
    objective: &str,
    senses: &Senses,
    decider: Option<&Decider>,
    eff: Effectors,
    set: impl FnOnce(&mut DelegateOptions<'_>),
) -> (DelegateResult, Effectors) {
    let clock = fake_clock();
    let mut opts = DelegateOptions {
        decide: "model".into(),
        clock: &clock,
        ..DelegateOptions::default()
    };
    set(&mut opts);
    let r = run_delegate(objective, senses, &eff, decider.map(|d| d as &dyn LaneDecider), opts);
    (r, eff)
}

fn done_when(w: &'static str) -> impl FnOnce(&mut DelegateOptions<'_>) {
    move |o| o.done_when = Some(w.into())
}

fn none(_: &mut DelegateOptions<'_>) {}

fn offered_any(step: &fl::Step, needle: &str) -> bool {
    step.offered.iter().any(|t| t.contains(needle))
}

fn menu_any(menu: &IndexMap<String, String>, needle: &str) -> bool {
    menu.values().any(|v| v.contains(needle))
}

/// run_delegate with the keyword default (test_lane_first's direct calls).
fn delegate_keyword(
    objective: &str,
    senses: &dyn SensesTrait,
    eff: &Effectors,
    decider: Option<&dyn LaneDecider>,
) -> DelegateResult {
    let clock = fake_clock();
    run_delegate(
        objective,
        senses,
        eff,
        decider,
        DelegateOptions {
            clock: &clock,
            ..DelegateOptions::default()
        },
    )
}

fn strings(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| s.to_string()).collect()
}

fn script(objectives: &[&str], senses: &Senses, eff: &Effectors) -> fl::ScriptResult {
    let clock = fake_clock();
    run_script(
        &strings(objectives),
        senses,
        eff,
        None,
        ScriptOptions {
            clock: &clock,
            ..ScriptOptions::default()
        },
    )
}

// ---- the safety gates (GATES.md G8) ------------------------------------------------------------

#[test]
fn test_sensitive_candidate_not_offered() {
    let d = Decider::new(&["Week"]);
    let (r, eff) = run(
        "show the next seven days",
        &Senses::new(vec![calendar(false), calendar(true)]),
        Some(&d),
        Effectors::default(),
        done_when("Week"),
    );
    assert_eq!(r.status, LaneStatus::Done);
    let menus = d.menus();
    assert_eq!(menus.len(), 1); // the model was asked: no keyword match
    let menu = &menus[0];
    assert!(!menu_any(menu, "Delete Event"));
    assert!(menu_any(menu, "Week") && eff.clicks() == ["Week"]);
    let offered: Vec<String> = menu
        .iter()
        .filter(|(k, _)| *k != "reobserve" && *k != "abstain")
        .map(|(_, v)| v.clone())
        .collect();
    assert!(r.steps[0].via == "model" && r.steps[0].offered == offered);
    // the keyword path builds the same menu: the sensitive control is absent there too
    let d2 = Decider::new(&[]);
    let (r2, eff2) = run(
        "switch to week view",
        &Senses::new(vec![calendar(false), calendar(true)]),
        Some(&d2),
        Effectors::default(),
        done_when("Week"),
    );
    assert!(r2.status == LaneStatus::Done && d2.menus().is_empty() && eff2.clicks() == ["Week"]);
    assert!(r2.steps[0].via == "keyword" && !offered_any(&r2.steps[0], "Delete Event"));
}

#[test]
fn test_sensitive_pick_confirms_without_clicking() {
    let compose = snap_with(
        vec![
            cand("0", "button", "Send"),
            cand("1", "button", "Attach"),
            cand("2", "button", "Emoji"),
        ],
        SnapOpts {
            app: "Mail",
            bundle: "com.apple.mail",
            title: "New Message",
            ..SnapOpts::default()
        },
    );
    let d = Decider::new(&["Send"]);
    let (r, eff) = run(
        "send the email",
        &Senses::new(vec![compose]),
        Some(&d),
        Effectors::default(),
        none,
    );
    assert_eq!(r.status, LaneStatus::Confirm);
    assert_eq!(
        r.line,
        "confirm: \"Send\" needs the human's yes; app=\"Mail\"; action=click; steps=0; last=none; text=none; key=none"
    );
    assert!(eff.clicks().is_empty() && d.menus().is_empty()); // never offered, never clicked
    assert!(r.log_lines.last().unwrap().starts_with("delegate confirm:"));
}

#[test]
fn test_sheet_button_never_offered_escalates_dialog_open() {
    let sheet = snap_with(
        vec![
            cand("0", "radio button", "Week"),
            cand("1", "button", "Today"),
            Candidate {
                in_dialog: true,
                ..cand("2", "button", "OK")
            },
            Candidate {
                in_dialog: true,
                ..cand("3", "button", "Cancel")
            },
        ],
        SnapOpts {
            dialog_text: "Delete this event?",
            ..SnapOpts::default()
        },
    );
    let d = Decider::new(&["OK"]);
    let (r, eff) = run(
        "switch to week view",
        &Senses::new(vec![sheet]),
        Some(&d),
        Effectors::default(),
        done_when("Week"),
    );
    assert!(r.status == LaneStatus::Escalate && r.line.contains("reason=dialog_open: \"Delete this event?\""));
    assert!(d.menus().is_empty() && eff.clicks().is_empty());
}

fn settings_appearance() -> Snapshot {
    snap_with(
        vec![
            cand("0", "row", "General"),
            cand("1", "row", "Appearance"),
            candv("2", "radio button", "Light", "selected"),
            cand("3", "radio button", "Dark"),
            cand("4", "button", "Back"),
        ],
        SnapOpts {
            app: "System Settings",
            bundle: SYSTEM_SETTINGS_BUNDLE,
            title: "Appearance",
            ..SnapOpts::default()
        },
    )
}

#[test]
fn test_system_settings_toggle_confirms() {
    let settings = settings_appearance();
    let d = Decider::new(&["Dark"]);
    let (r, eff) = run(
        "turn on dark mode",
        &Senses::new(vec![settings.clone()]),
        Some(&d),
        Effectors::default(),
        none,
    );
    assert!(
        r.status == LaneStatus::Confirm
            && r.line
                .starts_with("confirm: \"Dark\" needs the human's yes; app=\"System Settings\"")
    );
    assert!(eff.clicks().is_empty() && d.menus().is_empty());
    // navigation in the same pane is still offered: rows, tabs, links and Back
    let d2 = Decider::new(&["Back"]);
    let (r2, eff2) = run(
        "go back to the previous pane",
        &Senses::new(vec![settings.clone(), settings.clone()]),
        Some(&d2),
        Effectors::default(),
        none,
    );
    assert!(r2.status == LaneStatus::Stopped && eff2.clicks() == ["Back"]);
    assert!(d2.menus().is_empty() && r2.steps[0].via == "keyword"); // "back" uniquely matches: no model call
    assert!(!offered_any(&r2.steps[0], "Dark") && !offered_any(&r2.steps[0], "Light"));
    // the value change is withheld from the model path just the same
    let d3 = Decider::new(&["Appearance"]);
    let (r3, _) = run(
        "show me the look and feel pane",
        &Senses::new(vec![settings.clone(), settings]),
        Some(&d3),
        Effectors::default(),
        none,
    );
    let menus = d3.menus();
    assert_eq!(menus.len(), 1);
    assert!(!menu_any(&menus[0], "Dark") && !menu_any(&menus[0], "Light") && r3.steps[0].via == "model");
}

fn message_compose() -> Snapshot {
    snap_with(
        vec![
            cand("0", "button", "Emoji"),
            cand("1", "button", "Audio"),
            cand("2", "text area", "Message"),
        ],
        SnapOpts {
            app: "Messages",
            bundle: "com.apple.MobileSMS",
            title: "Maya Patel",
            ..SnapOpts::default()
        },
    )
}

#[test]
fn test_return_after_message_area_confirms() {
    let d = Decider::new(&["type into text area: Message", "press return"]);
    let (r, eff) = run(
        "enter the message and send it",
        &Senses::new(vec![message_compose(), message_compose()]),
        Some(&d),
        Effectors::default(),
        |o| {
            o.text = Some("I'm downstairs.".into());
            o.key = Some("return".into());
            o.done_when = Some("Delivered".into());
        },
    );
    assert_eq!(r.status, LaneStatus::Confirm);
    assert_eq!(r.line, "confirm: \"return\" needs the human's yes; app=\"Messages\"; action=press; steps=1; last=type 15 chars into text area: Message; text=applied; key=pending");
    assert_eq!(
        *eff.typed.borrow(),
        vec![("Message".to_string(), "I'm downstairs.".to_string())]
    );
    assert!(eff.presses.borrow().is_empty());
    assert!(!r.line.contains("I'm downstairs")); // the typed text never reaches a log line
}

fn notes_search() -> Snapshot {
    snap_with(
        vec![
            cand("0", "button", "New Note"),
            cand("1", "button", "Folders"),
            cand("2", "search field", "Search"),
        ],
        SnapOpts {
            app: "Notes",
            bundle: "com.apple.Notes",
            title: "Notes",
            ..SnapOpts::default()
        },
    )
}

#[test]
fn test_return_after_search_field_presses_once() {
    let d = Decider::new(&["type into search field: Search", "press return"]);
    let senses =
        Senses::new(vec![notes_search(), notes_search(), notes_search()]).visible(vec![&[], &[], &["Results"]]);
    let (r, eff) = run("search my notes", &senses, Some(&d), Effectors::default(), |o| {
        o.text = Some("blue adapter".into());
        o.key = Some("return".into());
        o.done_when = Some("Results".into());
    });
    assert_eq!(r.status, LaneStatus::Done, "{}", r.line);
    assert_eq!(
        *eff.typed.borrow(),
        vec![("Search".to_string(), "blue adapter".to_string())]
    );
    assert_eq!(*eff.presses.borrow(), vec!["return".to_string()]);
    assert_eq!(
        r.line,
        "done: matched=\"Results\" via=ocr; steps=2; last=press return; text=applied; key=applied"
    );
    // a press option is offered only once the text has been applied
    let menus = d.menus();
    assert!(!menu_any(&menus[0], "press return"));
    assert!(menu_any(&menus[1], "press return"));
}

#[test]
fn test_hit_test_miss_zero_input() {
    let d = Decider::new(&["Week"]);
    let (r, eff) = run(
        "switch to week view",
        &Senses::new(vec![calendar(false)]),
        Some(&d),
        Effectors {
            refuse_click: true,
            ..Effectors::default()
        },
        done_when("Week"),
    );
    assert!(
        r.status == LaneStatus::Escalate
            && r.line.contains("reason=hit_test_failed")
            && r.line.contains("steps=0; last=none")
    );
    assert!(eff.clicks().is_empty() && r.steps.last().unwrap().reason == "hit_test_failed");
}

#[test]
fn test_focus_mismatch_zero_paste() {
    let d = Decider::new(&["type into search field: Search"]);
    let (r, eff) = run(
        "search my notes",
        &Senses::new(vec![notes_search()]),
        Some(&d),
        Effectors {
            refuse_type: true,
            ..Effectors::default()
        },
        |o| {
            o.text = Some("blue adapter".into());
            o.done_when = Some("Results".into());
        },
    );
    assert!(r.status == LaneStatus::Escalate && r.line.contains("reason=focus_changed"));
    assert!(eff.typed.borrow().is_empty() && r.line.contains("text=pending"));
}

#[test]
fn test_no_repeat_after_unchanged() {
    let d = Decider::new(&["abstain"]);
    let senses =
        Senses::new(vec![calendar(false), calendar(false), calendar(false)]).changed(vec![Some(false), Some(false)]);
    let (r, eff) = run(
        "switch to week view",
        &senses,
        Some(&d),
        Effectors::default(),
        done_when("Never"),
    );
    assert_eq!(eff.clicks(), ["Week"]);
    assert!(offered_any(&r.steps[0], "Week") && r.steps[0].via == "keyword");
    let menus = d.menus();
    assert_eq!(menus.len(), 1); // step 2 went to the model
    assert!(!menu_any(&menus[0], "Week")); // not re-offered after an unchanged step
    assert!(!offered_any(&r.steps[1], "Week"));
    assert!(r.status == LaneStatus::Escalate && r.line.contains("reason=abstain") && r.steps[0].changed == Some(false));
}

#[test]
fn test_approve_allows_one_sensitive_click() {
    let events = snap(vec![
        cand("0", "button", "Delete Event"),
        cand("1", "button", "Discard Draft"),
        cand("2", "button", "Today"),
        cand("3", "button", "Inspector"),
    ]);
    let d = Decider::new(&["abstain"]);
    let senses =
        Senses::new(vec![events.clone(), events.clone(), events.clone()]).changed(vec![Some(true), Some(true)]);
    let (r, eff) = run(
        "delete the selected event",
        &senses,
        Some(&d),
        Effectors::default(),
        |o| {
            o.approve = Some("Delete Event".into());
            o.done_when = Some("Never".into());
            o.max_steps = 3;
        },
    );
    assert!(eff.clicks() == ["Delete Event"] && r.steps[0].via == "keyword"); // approved AND the unique match
    assert!(offered_any(&r.steps[0], "Delete Event"));
    assert!(!offered_any(&r.steps[0], "Discard")); // only the approved label, once
    let menus = d.menus();
    assert_eq!(menus.len(), 1); // step 2: nothing matches, the model is asked
    assert!(!menu_any(&menus[0], "Delete") && !menu_any(&menus[0], "Discard")); // approval consumed: neither offered
    assert!(r.status == LaneStatus::Escalate && r.line.contains("steps=1; last=click button: Delete Event"));
    // with no safe match left after the approved click, the next sensitive match is a confirm, never a click
    let only = snap(vec![
        cand("0", "button", "Delete Event"),
        cand("1", "button", "Delete All"),
        cand("2", "button", "Today"),
    ]);
    let d2 = Decider::new(&[]);
    let (r2, eff2) = run(
        "delete the selected event",
        &Senses::new(vec![only.clone(), only]).changed(vec![Some(true)]),
        Some(&d2),
        Effectors::default(),
        |o| {
            o.approve = Some("Delete Event".into());
            o.done_when = Some("Never".into());
            o.max_steps = 3;
        },
    );
    assert!(eff2.clicks() == ["Delete Event"] && d2.menus().is_empty());
    assert!(r2.status == LaneStatus::Confirm && r2.line.starts_with("confirm: \"Delete All\" needs the human's yes"));
    assert!(r2.line.contains("steps=1; last=click button: Delete Event"));
    // the model path honours approve identically: the objective shares no token, the fake picks it
    let d3 = Decider::new(&["Delete Event"]);
    let (r3, eff3) = run(
        "get rid of the highlighted appointment",
        &Senses::new(vec![events.clone(), events]).changed(vec![Some(true)]),
        Some(&d3),
        Effectors::default(),
        |o| {
            o.approve = Some("Delete Event".into());
            o.done_when = Some("Never".into());
            o.max_steps = 1;
        },
    );
    assert!(eff3.clicks() == ["Delete Event"] && r3.steps[0].via == "model");
}

#[test]
fn test_week_radio_clicked_once() {
    let d = Decider::new(&[]);
    let (r, eff) = run(
        "switch to week view",
        &Senses::new(vec![calendar(false), calendar(true)]),
        Some(&d),
        Effectors::default(),
        done_when("Week"),
    );
    assert_eq!(r.status, LaneStatus::Done);
    assert_eq!(
        r.line,
        "done: matched=\"Week\" via=ax; steps=1; last=click radio button: Week; text=none; key=none"
    );
    assert!(eff.clicks() == ["Week"] && *eff.settles.borrow() == vec![1.0]);
    assert_eq!(r.steps.len(), 1);
    let step = &r.steps[0];
    assert!(step.verdict == "act" && step.changed == Some(true) && step.n == 1 && step.k == 7); // 5 offered + 2 reserved
    assert!(step.via == "keyword" && step.kind == "click" && step.p_top == 1.0 && d.menus().is_empty());
    assert!(
        step.description == "Week (radio button)"
            && step.offered.contains(&"Month (radio button, selected)".to_string())
    );
    assert!(r.log_lines[0].starts_with("delegate step 1: Week (radio button) (keyword) act"));
    assert!(r.log_lines.last().unwrap().starts_with("delegate done:"));
    assert!(r.log_lines.iter().all(|ln| ln.chars().count() <= 80));
    // the model path, positive control: no keyword match, the fake picks Week
    let d3 = Decider::new(&["Week"]);
    let (r3, eff3) = run(
        "show the next seven days",
        &Senses::new(vec![calendar(false), calendar(true)]),
        Some(&d3),
        Effectors::default(),
        done_when("Week"),
    );
    assert!(r3.status == LaneStatus::Done && eff3.clicks() == ["Week"] && r3.steps[0].via == "model");
    assert!(r3.log_lines[0].starts_with("delegate step 1: Week (radio button) (p 0.70) act"));
    // negative control: already in week view → done with zero clicks, before any step
    let d2 = Decider::new(&["Week"]);
    let (r2, eff2) = run(
        "switch to week view",
        &Senses::new(vec![calendar(true)]),
        Some(&d2),
        Effectors::default(),
        done_when("Week"),
    );
    assert!(r2.status == LaneStatus::Done && eff2.clicks().is_empty() && d2.menus().is_empty());
    assert!(r2.line.contains("steps=0; last=none"));
}

// ---- the other statuses and reasons -------------------------------------------------------------

#[test]
fn test_one_step_without_done_when() {
    let d = Decider::new(&["Today"]);
    let (r, eff) = run(
        "jump to today",
        &Senses::new(vec![calendar(false), calendar(false)]),
        Some(&d),
        Effectors::default(),
        none,
    );
    assert!(r.status == LaneStatus::Stopped && eff.clicks() == ["Today"]);
    assert_eq!(
        r.line,
        "stopped: reason=one_step; steps=1; last=click button: Today; pick=4 button \"Today\"; text=none; key=none"
    );
}

#[test]
fn test_dry_run_reports_the_pick_and_touches_nothing() {
    let d = Decider::new(&["Week"]);
    let (r, eff) = run(
        "switch to week view",
        &Senses::new(vec![calendar(false)]),
        Some(&d),
        Effectors::default(),
        |o| {
            o.done_when = Some("Week".into());
            o.dry_run = true;
        },
    );
    assert!(r.status == LaneStatus::Stopped && eff.clicks().is_empty() && eff.settles.borrow().is_empty());
    assert_eq!(
        r.line,
        "stopped: reason=dry_run; steps=0; last=none; pick=2 radio button \"Week\"; text=none; key=none"
    );
    assert!(r.steps[0].verdict == "stopped" && r.steps[0].reason == "dry_run");
}

#[test]
fn test_reobserve_once_then_stalled() {
    let d = Decider::new(&["reobserve", "Week"]);
    let senses = Senses::new(vec![calendar(false), calendar(false), calendar(true)]);
    let (r, eff) = run(
        "show the next seven days",
        &senses,
        Some(&d),
        Effectors::default(),
        done_when("Week"),
    );
    assert!(r.status == LaneStatus::Done && eff.clicks() == ["Week"] && senses.calls() == 3);
    assert_eq!(
        r.steps.iter().map(|s| s.verdict.as_str()).collect::<Vec<_>>(),
        ["reobserve", "act"]
    );
    assert_eq!(r.steps.iter().map(|s| s.n).collect::<Vec<_>>(), [1, 1]);
    let d2 = Decider::new(&["reobserve", "reobserve"]);
    let (r2, eff2) = run(
        "show the next seven days",
        &Senses::new(vec![calendar(false)]),
        Some(&d2),
        Effectors::default(),
        done_when("Week"),
    );
    assert!(r2.status == LaneStatus::Escalate && r2.line.contains("reason=stalled;") && eff2.clicks().is_empty());
}

#[test]
fn test_two_unchanged_steps_escalate_stalled_2() {
    let d = Decider::new(&["Today"]); // step 1 is the keyword gate's
    let senses =
        Senses::new(vec![calendar(false), calendar(false), calendar(false)]).changed(vec![Some(false), Some(false)]);
    let (r, eff) = run("switch to week view", &senses, Some(&d), Effectors::default(), |o| {
        o.done_when = Some("Never".into());
        o.max_steps = 4;
    });
    assert!(r.status == LaneStatus::Escalate && r.line.contains("reason=stalled_2"));
    assert!(eff.clicks() == ["Week", "Today"] && r.line.contains("steps=2; last=click button: Today"));
}

#[test]
fn test_ambiguous_target_escalates_on_low_probability_or_margin() {
    let (r, eff) = run(
        "show the next seven days",
        &Senses::new(vec![calendar(false)]),
        Some(&Decider::new(&["Week"]).odds(0.2, 0.1)),
        Effectors::default(),
        done_when("Week"),
    );
    assert!(r.status == LaneStatus::Escalate && r.line.contains("reason=ambiguous_target") && eff.clicks().is_empty());
    let (r, eff) = run(
        "show the next seven days",
        &Senses::new(vec![calendar(false)]),
        Some(&Decider::new(&["Week"]).odds(0.5, 0.01)),
        Effectors::default(),
        done_when("Week"),
    );
    assert!(r.status == LaneStatus::Escalate && r.line.contains("reason=ambiguous_target") && eff.clicks().is_empty());
    assert!(r
        .line
        .starts_with("escalate: app=\"Calendar\"; reason=ambiguous_target; top=["));
    assert!(r.line.contains("top=[0 button \"Add Event\", ")); // no match: menu order is reading order
                                                               // a keyword pick is never ambiguous: the same low-probability fake is not even asked
    let (r, eff) = run(
        "switch to week view",
        &Senses::new(vec![calendar(false), calendar(true)]),
        Some(&Decider::new(&["Week"]).odds(0.2, 0.1)),
        Effectors::default(),
        done_when("Week"),
    );
    assert!(r.status == LaneStatus::Done && eff.clicks() == ["Week"]);
}

#[test]
fn test_model_error_and_abstain_escalate() {
    let (r, eff) = run(
        "show the next seven days",
        &Senses::new(vec![calendar(false)]),
        Some(&Decider::new(&["error"])),
        Effectors::default(),
        done_when("Week"),
    );
    assert!(r.status == LaneStatus::Escalate && r.line.contains("reason=model_invalid") && eff.clicks().is_empty());
    assert!(r.log_lines.iter().any(|ln| ln.contains("predict raised")));
    let (r, eff) = run(
        "show the next seven days",
        &Senses::new(vec![calendar(false)]),
        Some(&Decider::new(&["abstain"])),
        Effectors::default(),
        done_when("Week"),
    );
    assert!(r.status == LaneStatus::Escalate && r.line.contains("reason=abstain") && eff.clicks().is_empty());
}

#[test]
fn test_a_raising_decider_is_model_invalid_with_its_kind() {
    // Python's `choose raised {type(e).__name__}: {e}`, capped at 200 characters.
    let (r, eff) = run(
        "show the next seven days",
        &Senses::new(vec![calendar(false)]),
        Some(&Decider::new(&["Nowhere"])),
        Effectors::default(),
        done_when("Week"),
    );
    assert!(r.status == LaneStatus::Escalate && r.line.contains("reason=model_invalid") && eff.clicks().is_empty());
    assert!(r
        .log_lines
        .iter()
        .any(|ln| ln.starts_with("delegate model: choose raised AssertionError: ")));
}

#[test]
fn test_no_window_and_no_candidate_and_truncated() {
    let empty = snap_with(
        vec![],
        SnapOpts {
            node_count: Some(0),
            ..SnapOpts::default()
        },
    );
    let (r, _) = run(
        "switch to week view",
        &Senses::new(vec![empty]),
        Some(&Decider::new(&["Week"])),
        Effectors::default(),
        done_when("Week"),
    );
    assert!(r.status == LaneStatus::Escalate && r.line.contains("reason=no_window"));
    let bare = snap(vec![cand("0", "button", "Today")]); // one pressable, nothing editable
    let senses = Senses::new(vec![bare.clone(), bare]);
    let (r, _) = run(
        "switch to week view",
        &senses,
        Some(&Decider::new(&["Today"])),
        Effectors::default(),
        done_when("Week"),
    );
    assert!(r.status == LaneStatus::Escalate && r.line.contains("reason=no_candidate") && senses.calls() == 2); // one re-snapshot
    let cut = snap_with(
        vec![cand("0", "button", "Today"), cand("1", "button", "Inspector")],
        SnapOpts {
            truncated: true,
            ..SnapOpts::default()
        },
    );
    let (r, _) = run(
        "switch to week view",
        &Senses::new(vec![cut]),
        Some(&Decider::new(&["Today"])),
        Effectors::default(),
        done_when("Week"),
    );
    assert!(r.status == LaneStatus::Escalate && r.line.contains("reason=truncated"));
    let cut2 = snap_with(
        vec![cand("0", "button", "Today"), cand("1", "radio button", "Week")],
        SnapOpts {
            truncated: true,
            ..SnapOpts::default()
        },
    );
    let (r, eff) = run(
        "switch to week view",
        &Senses::new(vec![cut2, calendar(true)]),
        Some(&Decider::new(&["Week"])),
        Effectors::default(),
        done_when("Week"),
    );
    assert!(r.status == LaneStatus::Escalate && r.line.contains("reason=truncated") && eff.clicks().is_empty());
    assert!(r.steps.is_empty()); // a matching label cannot prove uniqueness
    let complete = snap(vec![cand("0", "button", "Today"), cand("1", "radio button", "Week")]);
    let (r, eff) = run(
        "switch to week view",
        &Senses::new(vec![complete, calendar(true)]),
        Some(&Decider::new(&["Week"])),
        Effectors::default(),
        done_when("Week"),
    );
    assert!(r.status == LaneStatus::Done && eff.clicks() == ["Week"]); // complete positive control still acts
}

#[test]
fn test_truncated_after_observation_retains_input_without_claiming_done() {
    let after = Snapshot {
        truncated: true,
        ..calendar(true)
    };
    let (r, eff) = run(
        "switch to week view",
        &Senses::new(vec![calendar(false), after]),
        Some(&Decider::new(&["Week"])),
        Effectors::default(),
        done_when("Week"),
    );
    assert!(r.status == LaneStatus::Escalate && r.line.contains("reason=truncated"));
    assert!(eff.clicks() == ["Week"] && r.line.contains("steps=1"));
}

#[test]
fn test_step_cap_escalates_after_rechecking_done_when() {
    let d = Decider::new(&["Today"]); // step 1: keyword; step 2: the model
    let senses =
        Senses::new(vec![calendar(false), calendar(false), calendar(false)]).changed(vec![Some(true), Some(true)]);
    let (r, eff) = run("switch to week view", &senses, Some(&d), Effectors::default(), |o| {
        o.done_when = Some("Never".into());
        o.max_steps = 2;
    });
    assert!(
        r.status == LaneStatus::Escalate && r.line.contains("reason=step_cap") && eff.clicks() == ["Week", "Today"]
    );
    assert_eq!(
        r.steps.iter().map(|s| s.via.as_str()).collect::<Vec<_>>(),
        ["keyword", "model"]
    );
    let d = Decider::new(&["Today"]);
    let senses = Senses::new(vec![calendar(false), calendar(false), calendar(false)])
        .changed(vec![Some(true), Some(true)])
        .visible(vec![&[], &[], &["Bingo"]]);
    let (r, _) = run("switch to week view", &senses, Some(&d), Effectors::default(), |o| {
        o.done_when = Some("Bingo".into());
        o.max_steps = 2;
    });
    assert!(r.status == LaneStatus::Done && r.line.contains("via=ocr")); // the last settle is checked first
}

#[test]
fn test_done_via_title_only_when_it_was_absent_at_step_0() {
    let mail = |title: &'static str| {
        snap_with(
            vec![cand("0", "row", "Inbox"), cand("1", "row", "Travel")],
            SnapOpts {
                app: "Mail",
                title,
                ..SnapOpts::default()
            },
        )
    };
    let (r, eff) = run(
        "open the travel mailbox",
        &Senses::new(vec![mail("Inbox"), mail("Travel — 3 messages")]),
        Some(&Decider::new(&["Travel"])),
        Effectors::default(),
        done_when("Travel"),
    );
    assert!(r.status == LaneStatus::Done && r.line.contains("via=title") && eff.clicks() == ["Travel"]);
    let already = mail("Travel — 3 messages");
    let (r, _) = run(
        "open the travel mailbox",
        &Senses::new(vec![already.clone(), already]),
        Some(&Decider::new(&["Travel"])),
        Effectors::default(),
        |o| {
            o.done_when = Some("Travel".into());
            o.max_steps = 1;
        },
    );
    assert!(r.status == LaneStatus::Escalate && r.line.contains("reason=step_cap"));
    // present at step 0: not evidence
}

#[test]
fn test_unavailable_decider_means_zero_input() {
    let senses = Senses::new(vec![calendar(false)]);
    let off = Decider {
        available: false,
        ..Decider::new(&["Week"])
    };
    let (r, eff) = run(
        "switch to week view",
        &senses,
        Some(&off),
        Effectors::default(),
        done_when("Week"),
    );
    assert!(r.status == LaneStatus::Unavailable && r.line == "unavailable: the decider is not loaded");
    assert!(eff.clicks().is_empty() && senses.calls() == 0);
    let (r, eff) = run(
        "switch to week view",
        &senses,
        None,
        Effectors::default(),
        done_when("Week"),
    );
    assert!(r.status == LaneStatus::Unavailable && eff.clicks().is_empty());
    let (r, _) = run("", &senses, Some(&Decider::new(&["Week"])), Effectors::default(), none);
    assert!(r.status == LaneStatus::Unavailable && r.line.contains("objective"));
    // an unknown decide mode names the modes as Python's tuple repr
    let (r, _) = run("switch to week view", &senses, None, Effectors::default(), |o| {
        o.decide = "maybe".into()
    });
    assert_eq!(r.line, "unavailable: decide must be one of ('keyword', 'model', 'jev')");
}

#[test]
fn test_key_combos_are_unavailable() {
    for key in ["command+l", "cmd+w", "ctrl+c", "shift tab"] {
        let (r, eff) = run(
            "close the tab",
            &Senses::new(vec![calendar(false)]),
            Some(&Decider::new(&["Week"])),
            Effectors::default(),
            |o| o.key = Some(key.into()),
        );
        assert!(r.status == LaneStatus::Unavailable && r.line == "unavailable: key combos are not supported");
        assert!(eff.presses.borrow().is_empty() && eff.clicks().is_empty());
    }
}

#[test]
fn test_other_keys_press_without_confirm_and_escape_after_type_is_fine() {
    let d = Decider::new(&["press escape"]);
    let (r, eff) = run(
        "dismiss the popover",
        &Senses::new(vec![notes_search(), notes_search()]),
        Some(&d),
        Effectors::default(),
        |o| o.key = Some("escape".into()),
    );
    assert!(
        r.status == LaneStatus::Stopped
            && *eff.presses.borrow() == vec!["escape".to_string()]
            && r.line.contains("key=applied")
    );
    assert!(r.line.ends_with("last=press escape; pick=none; text=none; key=applied"));
}

#[test]
fn test_type_options_are_capped_and_never_secure() {
    let form = snap_with(
        vec![
            cand("0", "button", "Continue"),
            cand("1", "text field", "Name"),
            cand("2", "text field", "Email"),
            Candidate {
                secure: true,
                ..cand("3", "text field", "Password")
            },
            cand("4", "text field", "Postcode"),
            cand("5", "text field", "City"),
        ],
        SnapOpts {
            app: "Safari",
            bundle: "com.apple.Safari",
            title: "Form",
            ..SnapOpts::default()
        },
    );
    let d = Decider::new(&["type into text field: Name"]);
    let (r, eff) = run(
        "fill in the name",
        &Senses::new(vec![form.clone(), form]),
        Some(&d),
        Effectors::default(),
        |o| o.text = Some("Maya".into()),
    );
    let menus = d.menus();
    assert_eq!(menus.len(), 1);
    let typed: Vec<&String> = menus[0].values().filter(|v| v.starts_with("type into")).collect();
    assert!(typed.len() == 3 && !typed.iter().any(|v| v.contains("Password")));
    assert_eq!(typed[0], "type into text field: Name"); // the objective's match ranks first
    assert_eq!(*eff.typed.borrow(), vec![("Name".to_string(), "Maya".to_string())]);
    assert!(r.status == LaneStatus::Stopped && r.line.contains("text=applied"));
}

#[test]
fn test_max_steps_is_clamped_and_menu_is_bounded() {
    let many = snap(
        (0..20)
            .map(|i| cand(&i.to_string(), "button", &format!("Button {i}")))
            .collect(),
    );
    let picks: Vec<&str> = ["Button 0", "Button 1"].iter().cycle().take(8).copied().collect();
    let d = Decider::new(&picks); // the previous pick is never re-offered
    let senses = Senses::new(vec![many; 8]).changed(vec![Some(true); 8]);
    let (r, eff) = run("press button 0", &senses, Some(&d), Effectors::default(), |o| {
        o.done_when = Some("Never".into());
        o.max_steps = 99;
    });
    assert!(eff.clicks().len() == 6 && r.line.contains("reason=step_cap")); // clamped to 6
    assert!(d.menus().iter().all(|m| m.len() <= 12) && r.steps[0].dropped_cap == 10);
}

#[test]
fn test_focus_change_before_acting_escalates() {
    let senses = Senses::new(vec![calendar(false)]).pid(99); // another app took the front
    let (r, eff) = run(
        "switch to week view",
        &senses,
        Some(&Decider::new(&["Week"])),
        Effectors::default(),
        done_when("Week"),
    );
    assert!(r.status == LaneStatus::Escalate && r.line.contains("reason=focus_changed") && eff.clicks().is_empty());
}

#[test]
fn test_decider_receives_context_and_recent_actions() {
    let d = Decider::new(&["Week", "Today"]);
    let senses =
        Senses::new(vec![calendar(false), calendar(false), calendar(false)]).changed(vec![Some(true), Some(true)]);
    run(
        "show the next seven days",
        &senses,
        Some(&d),
        Effectors::default(),
        |o| {
            o.done_when = Some("Never".into());
            o.max_steps = 2;
        },
    );
    {
        let calls = d.calls.borrow();
        assert!(calls[0].app == "Calendar" && calls[0].context == "title: September 2026");
        assert!(calls[0].recent.is_empty() && calls[1].recent == ["click radio button: Week"]);
    }
    let menus = d.menus();
    assert!(["reobserve", "abstain", "1"].iter().all(|k| menus[0].contains_key(*k)));
    assert!(menus[0]["reobserve"] == fl::REOBSERVE_TEXT && menus[0]["abstain"] == fl::ABSTAIN_TEXT);
    assert!(menus[0]["1"] == "Add Event (button)" && menus[0].values().any(|v| v == "Month (radio button, selected)"));
    let focused = snap_with(
        vec![cand("0", "button", "Today"), cand("1", "radio button", "Week")],
        SnapOpts {
            focused: Some(cand("9", "search field", "Search")),
            ..SnapOpts::default()
        },
    );
    let d2 = Decider::new(&["Week"]);
    run(
        "show the next seven days",
        &Senses::new(vec![focused.clone(), focused]),
        Some(&d2),
        Effectors::default(),
        none,
    );
    assert_eq!(
        d2.calls.borrow()[0].context,
        "title: September 2026; focused: search field Search"
    );
    assert_eq!(
        fl::lane_context(&snap_with(
            vec![],
            SnapOpts {
                title: "  A   B ",
                ..SnapOpts::default()
            }
        )),
        "title: A B"
    );
}

// ---- the keyword gate -----------------------------------------------------------------------------

fn items(cands: &[Candidate]) -> Vec<MenuItem> {
    cands
        .iter()
        .enumerate()
        .map(|(i, c)| MenuItem {
            key: (i + 1).to_string(),
            kind: "click".into(),
            text: render_option(c),
            candidate: Some(c.clone()),
            payload: String::new(),
            href: String::new(),
        })
        .collect()
}

fn picked(items: &[MenuItem], objective: &str) -> Option<String> {
    fl::keyword_pick(items, objective)
        .and_then(|it| it.candidate.as_ref())
        .map(|c| c.label.clone())
}

#[test]
fn test_keyword_pick_unique_maximum_only() {
    let (week, today, month) = (
        cand("1", "radio button", "Week"),
        cand("2", "button", "Today"),
        cand("3", "radio button", "Month"),
    );
    let its = items(&[week.clone(), today, month]);
    assert_eq!(
        fl::keyword_pick(&its, "switch to week view").and_then(|it| it.candidate.clone()),
        Some(week)
    );
    assert_eq!(picked(&its, "go to the next seven days"), None); // zero overlap
    assert_eq!(picked(&its, "week or month"), None); // a tie
    assert_eq!(picked(&its, ""), None);
    let two = items(&[cand("1", "row", "Week calendar"), cand("2", "radio button", "Week")]);
    assert_eq!(picked(&two, "show the week calendar").as_deref(), Some("Week calendar")); // two tokens beat one
                                                                                          // the value counts: "the selected one" matches a selected control; type and press items never do
    let sel = items(&[
        candv("1", "radio button", "Month", "selected"),
        cand("2", "radio button", "Week"),
    ]);
    assert_eq!(picked(&sel, "open the selected one").as_deref(), Some("Month"));
    let typed = vec![
        MenuItem {
            key: "1".into(),
            kind: "type".into(),
            text: "type into search field: Search".into(),
            candidate: Some(cand("5", "search field", "Search")),
            payload: "x".into(),
            href: String::new(),
        },
        MenuItem {
            key: "2".into(),
            kind: "press".into(),
            text: "press return".into(),
            candidate: None,
            payload: "return".into(),
            href: String::new(),
        },
    ];
    assert_eq!(picked(&typed, "search return"), None);
}

#[test]
fn test_keyword_pick_is_still_gated_by_every_code_oracle() {
    // hit-test refusal: the keyword pick is refused by the effector and nothing is applied
    let (r, eff) = run(
        "switch to week view",
        &Senses::new(vec![calendar(false)]),
        Some(&Decider::new(&[])),
        Effectors {
            refuse_click: true,
            ..Effectors::default()
        },
        done_when("Week"),
    );
    assert!(r.status == LaneStatus::Escalate && r.line.contains("reason=hit_test_failed") && eff.clicks().is_empty());
    assert_eq!(r.steps[0].via, "keyword");
    // a sensitive control is withheld before the gate ever sees it: "delete" cannot be keyword-picked, and the
    // look-alike "Add Event" (one shared token against Delete Event's two) is never clicked instead
    let (r, eff) = run(
        "delete the event",
        &Senses::new(vec![calendar(false)]),
        Some(&Decider::new(&[])),
        Effectors::default(),
        none,
    );
    assert!(
        r.status == LaneStatus::Confirm && r.line.starts_with("confirm: \"Delete Event\"") && eff.clicks().is_empty()
    );
    let (r, eff) = run(
        "delete the event",
        &Senses::new(vec![calendar(false)]),
        Some(&Decider::new(&["Add Event"])),
        Effectors::default(),
        none,
    );
    assert!(r.status == LaneStatus::Confirm && eff.clicks().is_empty());
    // an offered control that matches strictly better than any withheld one is picked as usual
    let (r, eff) = run(
        "add a new event",
        &Senses::new(vec![calendar(false), calendar(false)]),
        Some(&Decider::new(&[])),
        Effectors::default(),
        none,
    );
    assert!(r.status == LaneStatus::Stopped && eff.clicks() == ["Add Event"] && r.steps[0].via == "keyword");
    // System Settings: a value control is withheld; the navigation row is keyword-picked
    let settings = snap_with(
        vec![
            cand("0", "row", "General"),
            cand("1", "row", "Appearance"),
            cand("2", "button", "Dark"),
        ],
        SnapOpts {
            app: "System Settings",
            bundle: SYSTEM_SETTINGS_BUNDLE,
            title: "General",
            ..SnapOpts::default()
        },
    );
    let (r, eff) = run(
        "open the appearance pane",
        &Senses::new(vec![settings.clone(), settings.clone()]),
        Some(&Decider::new(&[])),
        Effectors::default(),
        none,
    );
    assert!(r.status == LaneStatus::Stopped && eff.clicks() == ["Appearance"] && r.steps[0].via == "keyword");
    let (r, eff) = run(
        "turn on dark mode",
        &Senses::new(vec![settings]),
        Some(&Decider::new(&[])),
        Effectors::default(),
        none,
    );
    assert!(r.status == LaneStatus::Confirm && eff.clicks().is_empty());
    // a dialog stops the run before any pick
    let sheet = snap_with(
        vec![
            cand("0", "radio button", "Week"),
            Candidate {
                in_dialog: true,
                ..cand("1", "button", "OK")
            },
        ],
        SnapOpts {
            dialog_text: "Sure?",
            ..SnapOpts::default()
        },
    );
    let (r, eff) = run(
        "switch to week view",
        &Senses::new(vec![sheet]),
        Some(&Decider::new(&[])),
        Effectors::default(),
        done_when("Week"),
    );
    assert!(r.status == LaneStatus::Escalate && r.line.contains("dialog_open") && eff.clicks().is_empty());
    // the focus check and the no-repeat rule apply too
    let (r, eff) = run(
        "switch to week view",
        &Senses::new(vec![calendar(false)]).pid(7),
        Some(&Decider::new(&[])),
        Effectors::default(),
        done_when("Week"),
    );
    assert!(r.status == LaneStatus::Escalate && r.line.contains("reason=focus_changed") && eff.clicks().is_empty());
    let d = Decider::new(&["abstain"]);
    let (r, eff) = run(
        "switch to week view",
        &Senses::new(vec![calendar(false); 3]).changed(vec![Some(true), Some(true)]),
        Some(&d),
        Effectors::default(),
        done_when("Never"),
    );
    assert!(eff.clicks() == ["Week"] && !offered_any(&r.steps[1], "Week") && r.steps[1].via == "model");
}

// ---- format_line and gate() -----------------------------------------------------------------------

#[test]
fn test_format_line_templates_and_escaping() {
    let week = cand("2", "radio button", "Say \"hi\"\nnow");
    assert_eq!(describe_pick(Some(&week)), "2 radio button \"Say \\\"hi\\\" now\"");
    assert_eq!(
        format_line(
            LaneStatus::Unavailable,
            &LineFields {
                reason: "x".into(),
                ..LineFields::default()
            }
        ),
        "unavailable: x"
    );
    assert_eq!(
        format_line(
            LaneStatus::Done,
            &LineFields {
                done_when: "We\"ek".into(),
                via: "ax".into(),
                steps: 1,
                last: "click radio button: Week".into(),
                ..LineFields::default()
            }
        ),
        "done: matched=\"We\\\"ek\" via=ax; steps=1; last=click radio button: Week; text=none; key=none"
    );
    let w = "2 radio button \"Say \\\"hi\\\" now\"";
    assert_eq!(
        format_line(
            LaneStatus::Escalate,
            &LineFields {
                app: "Cal".into(),
                reason: "abstain".into(),
                top: vec![week.clone(), week.clone(), week.clone(), week],
                steps: 0,
                ..LineFields::default()
            }
        ),
        format!("escalate: app=\"Cal\"; reason=abstain; top=[{w}, {w}, {w}]; steps=0; last=none; text=none; key=none")
    );
    assert_eq!(
        format_line(LaneStatus::Confirm, &LineFields {
            label: "Send".into(),
            app: "Mail".into(),
            action: "click".into(),
            text_state: "applied".into(),
            key_state: "pending".into(),
            ..LineFields::default()
        }),
        "confirm: \"Send\" needs the human's yes; app=\"Mail\"; action=click; steps=0; last=none; text=applied; key=pending"
    );
    // Python's ValueError for an unknown status: the enum admits only STATUSES
    assert_eq!(fl::STATUSES.len(), 5);
    assert_eq!(
        [
            LaneStatus::Done,
            LaneStatus::Stopped,
            LaneStatus::Escalate,
            LaneStatus::Confirm,
            LaneStatus::Unavailable
        ]
        .map(LaneStatus::as_str),
        fl::STATUSES
    );
}

#[test]
fn test_gate_table() {
    let th = Thresholds {
        p_min: 0.35,
        margin_min: 0.05,
        ..Thresholds::default()
    };
    let mk = |key: &str, kind: &str, text: &str, c: Option<Candidate>, payload: &str| MenuItem {
        key: key.into(),
        kind: kind.into(),
        text: text.into(),
        candidate: c,
        payload: payload.into(),
        href: String::new(),
    };
    let week = mk(
        "1",
        "click",
        "click radio button: Week",
        Some(cand("2", "radio button", "Week")),
        "",
    );
    let delete = mk(
        "2",
        "click",
        "click button: Delete Event",
        Some(cand("5", "button", "Delete Event")),
        "",
    );
    let ret = mk("3", "press", "press return", None, "return");
    let tab = mk("4", "press", "press tab", None, "tab");
    let ok = GateState {
        approve: None,
        approve_used: false,
        search_typed: false,
        dialog_open: false,
        reobserve_count: 0,
    };
    let choice = |cid: &str, p: f64, m: f64, error: &str| Choice {
        id: cid.into(),
        p_top: p,
        margin: m,
        error: error.into(),
        ..Choice::default()
    };
    let c = |cid: &str| choice(cid, 0.7, 0.4, "");

    assert_eq!(gate(&c(""), Some(&week), &th, &ok), ("escalate", "model_invalid"));
    assert_eq!(
        gate(&choice("1", 0.7, 0.4, "x"), Some(&week), &th, &ok),
        ("escalate", "model_invalid")
    );
    assert_eq!(gate(&c("abstain"), None, &th, &ok), ("escalate", "abstain"));
    assert_eq!(gate(&c("reobserve"), None, &th, &ok), ("reobserve", ""));
    assert_eq!(
        gate(
            &c("reobserve"),
            None,
            &th,
            &GateState {
                reobserve_count: 1,
                ..ok
            }
        ),
        ("escalate", "stalled")
    );
    assert_eq!(
        gate(&choice("1", 0.3, 0.4, ""), Some(&week), &th, &ok),
        ("escalate", "ambiguous_target")
    );
    assert_eq!(
        gate(&choice("1", 0.7, 0.04, ""), Some(&week), &th, &ok),
        ("escalate", "ambiguous_target")
    );
    assert_eq!(gate(&c("9"), None, &th, &ok), ("escalate", "model_invalid"));
    assert_eq!(gate(&c("1"), Some(&week), &th, &ok), ("act", ""));
    assert_eq!(gate(&c("2"), Some(&delete), &th, &ok), ("confirm", "sensitive"));
    assert_eq!(
        gate(
            &c("2"),
            Some(&delete),
            &th,
            &GateState {
                approve: Some("delete event"),
                ..ok
            }
        ),
        ("act", "")
    );
    assert_eq!(
        gate(
            &c("2"),
            Some(&delete),
            &th,
            &GateState {
                approve: Some("delete event"),
                approve_used: true,
                ..ok
            }
        ),
        ("confirm", "sensitive")
    );
    assert_eq!(gate(&c("3"), Some(&ret), &th, &ok), ("confirm", "return"));
    assert_eq!(
        gate(
            &c("3"),
            Some(&ret),
            &th,
            &GateState {
                search_typed: true,
                ..ok
            }
        ),
        ("act", "")
    );
    assert_eq!(
        gate(
            &c("3"),
            Some(&ret),
            &th,
            &GateState {
                search_typed: true,
                dialog_open: true,
                ..ok
            }
        ),
        ("confirm", "return")
    );
    assert_eq!(gate(&c("4"), Some(&tab), &th, &ok), ("act", ""));
}

// ---- review fixes (2026-09-21) ----------------------------------------------------------------------

#[test]
fn test_semicolons_in_labels_cannot_forge_result_fields() {
    assert_eq!(fl::q("Send; steps=9; last=none"), "Send, steps=9, last=none");
    assert_eq!(fl::q("say \"hi\"\nthere"), "say \\\"hi\\\" there");
}

#[test]
fn test_content_fields_are_not_offered_for_typing_unless_page_links_allowed() {
    let s = snap(vec![
        Candidate {
            in_content: true,
            ..cand("1", "text field", "Part name")
        },
        cand("2", "search field", "Search"),
    ]);
    let ids = |pool: Vec<Candidate>| pool.into_iter().map(|c| c.id).collect::<Vec<_>>();
    assert_eq!(ids(fl::editable_pool(&s, false)), ["2"]);
    assert_eq!(ids(fl::editable_pool(&s, true)), ["1", "2"]);
}

#[test]
fn test_dialog_open_beats_a_satisfied_done_when() {
    // A sheet with the target already selected: the lane reports the dialog, never 'done'.
    let s = snap_with(
        vec![
            candv("1", "radio button", "Week", "selected"),
            Candidate {
                in_dialog: true,
                ..cand("2", "button", "OK")
            },
        ],
        SnapOpts {
            dialog_text: "Delete this event?",
            ..SnapOpts::default()
        },
    );
    let (result, eff) = run(
        "switch to week view",
        &Senses::new(vec![s]),
        Some(&Decider::new(&[])),
        Effectors::default(),
        done_when("Week"),
    );
    assert!(result.status == LaneStatus::Escalate && result.line.contains("dialog_open: \"Delete this event?\""));
    assert!(eff.clicks().is_empty() && eff.presses.borrow().is_empty());
}

#[test]
fn test_a_control_named_exactly_is_not_mistaken_for_a_withheld_look_alike() {
    // browser_model_eval contact_form, 2026-09-24: the plan clicked the "Your message" field and the lane asked the
    // human about "Send message" instead (one shared word each), which a yes would then have pressed.
    let form = snap_with(
        vec![
            Candidate {
                actions: vec!["AXPress".into(), "AXFocus".into()],
                ..cand("0", "text field", "Your message")
            },
            cand("1", "button", "Send message"),
        ],
        SnapOpts {
            app: "browser",
            bundle: "eve.browser",
            title: "Contact us",
            ..SnapOpts::default()
        },
    );
    let (r, eff) = run(
        "Your message",
        &Senses::new(vec![form.clone(), form.clone()]),
        Some(&Decider::new(&[])),
        Effectors::default(),
        none,
    );
    assert!(
        r.status == LaneStatus::Stopped && eff.clicks() == ["Your message"],
        "{}",
        r.line
    );
    // a looser objective still gets the withheld control's confirm, and the withheld control is never clicked
    let (r, eff) = run(
        "the message",
        &Senses::new(vec![form]),
        Some(&Decider::new(&[])),
        Effectors::default(),
        none,
    );
    assert!(
        r.status == LaneStatus::Confirm && r.line.starts_with("confirm: \"Send message\"") && eff.clicks().is_empty()
    );
}

#[test]
fn test_helpers_valid_key_clip_and_ax_changed() {
    assert_eq!(fl::valid_key(None), (None, String::new()));
    assert_eq!(fl::valid_key(Some("  ")), (None, String::new()));
    assert_eq!(
        fl::valid_key(Some(" Return ")),
        (Some("return".to_string()), String::new())
    );
    assert_eq!(fl::valid_key(Some("cmd+w")).1, "key combos are not supported");
    assert_eq!(fl::clip("a  b\nc", 80), "a b c");
    assert_eq!(fl::clip(&"x".repeat(100), 10), format!("{}…", "x".repeat(9)));
    let before = calendar(false);
    assert_eq!(fl::ax_changed(&before, &before.clone(), None), None);
    assert_eq!(
        fl::ax_changed(&before, &calendar(true), Some(&before.elements[2])),
        Some(true)
    );
    assert!(fl::is_search_field(&cand("1", "text field", "Address and search bar")));
    assert!(!fl::is_search_field(&cand("1", "text field", "Name")));
    assert!(fl::shares_token(&cand("1", "button", "Week"), &strings(&["week"])));
    let focused = snap_with(
        vec![],
        SnapOpts {
            focused: Some(cand("9", "search field", "Search")),
            ..SnapOpts::default()
        },
    );
    assert_eq!(fl::jev_context(&focused), "focused: search field Search");
    assert_eq!(fl::jev_context(&calendar(false)), "");
}

// ---- the keyword-only lane (test_lane_first.py, G3) -------------------------------------------------

fn year_view() -> Vec<Snapshot> {
    // Calendar, month view → click Year → year view → click next year → next year.
    let base = |month: &str, year: &str| {
        vec![
            cand("1", "radio button", "Day"),
            cand("2", "radio button", "Week"),
            candv("3", "radio button", "Month", month),
            candv("4", "radio button", "Year", year),
            cand("5", "button", "Today"),
        ]
    };
    let month = snap_with(
        base("selected", ""),
        SnapOpts {
            title: "September 2026",
            ..SnapOpts::default()
        },
    );
    let with_years = || {
        let mut v = base("", "selected");
        v.push(cand("6", "button", "previous year"));
        v.push(cand("7", "button", "next year"));
        v
    };
    let year = snap_with(
        with_years(),
        SnapOpts {
            title: "2026",
            ..SnapOpts::default()
        },
    );
    let nxt = snap_with(
        with_years(),
        SnapOpts {
            title: "2027",
            ..SnapOpts::default()
        },
    );
    vec![month, year.clone(), year, nxt]
}

#[test]
fn test_keyword_mode_clicks_without_a_decider() {
    assert_eq!(fl::DEFAULT_DECIDE, "keyword");
    let eff = Effectors::default();
    let r = delegate_keyword(
        "switch to week view",
        &Senses::new(vec![calendar(false), calendar(true)]),
        &eff,
        None,
    );
    assert!(
        r.status == LaneStatus::Stopped && r.line.contains("reason=one_step"),
        "{}",
        r.line
    );
    let last = r.steps.last().unwrap();
    assert!(eff.clicks() == ["Week"] && last.via == "keyword" && last.changed == Some(true));
    assert_eq!(last.label, "Week");
}

#[test]
fn test_keyword_mode_tie_escalates_and_never_asks_the_model() {
    let two = snap(vec![
        cand("1", "button", "Done"),
        Candidate {
            frame: (400, 400, 60, 30),
            ..cand("2", "button", "Done")
        },
        cand("3", "button", "Cancel"),
    ]);
    let eff = Effectors::default();
    let r = delegate_keyword("press done", &Senses::new(vec![two]), &eff, Some(&NeverAsked));
    assert!(
        r.status == LaneStatus::Escalate && r.line.contains("reason=ambiguous_target"),
        "{}",
        r.line
    );
    assert!(eff.clicks().is_empty() && r.line.contains("steps=0"));
}

#[test]
fn test_keyword_mode_zero_overlap_escalates_no_match() {
    let eff = Effectors::default();
    let r = delegate_keyword(
        "show the next seven days",
        &Senses::new(vec![calendar(false)]),
        &eff,
        Some(&NeverAsked),
    );
    assert!(
        r.status == LaneStatus::Escalate && r.line.contains("reason=no_match") && eff.clicks().is_empty(),
        "{}",
        r.line
    );
    // and the keyword lane only clicks: text or a key is refused with zero input
    let clock = fake_clock();
    let typed = run_delegate(
        "search",
        &Senses::new(vec![calendar(false)]),
        &eff,
        None,
        DelegateOptions {
            text: Some("jazz".into()),
            clock: &clock,
            ..DelegateOptions::default()
        },
    );
    assert!(
        typed.status == LaneStatus::Unavailable && typed.line.contains("only clicks") && eff.typed.borrow().is_empty()
    );
}

#[test]
fn test_model_mode_still_asks_the_model_on_a_tie() {
    let d = Decider::new(&["Day"]);
    let (r, eff) = run(
        "show the next seven days",
        &Senses::new(vec![calendar(false), calendar(false)]),
        Some(&d),
        Effectors::default(),
        none,
    );
    assert!(d.menus().len() == 1 && eff.clicks() == ["Day"] && r.steps.last().unwrap().via == "model");
    let (none_r, _) = run(
        "x y z",
        &Senses::new(vec![calendar(false)]),
        None,
        Effectors::default(),
        none,
    );
    assert!(none_r.status == LaneStatus::Unavailable && none_r.line.contains("not loaded"));
}

#[test]
fn test_keyword_verdict_says_why_it_abstained() {
    let its = vec![
        MenuItem {
            text: "Week".into(),
            ..items(&[cand("1", "radio button", "Week")]).remove(0)
        },
        MenuItem {
            key: "2".into(),
            text: "Week numbers".into(),
            ..items(&[cand("2", "checkbox", "Week numbers")]).remove(0)
        },
    ];
    let (p, why) = fl::keyword_verdict(&its, "switch to week view");
    assert!(p.is_none() && why == "tie");
    let (p, why) = fl::keyword_verdict(&its, "print it");
    assert!(p.is_none() && why == "no_match");
    let (p, why) = fl::keyword_verdict(&its, "show week numbers");
    assert!(std::ptr::eq(p.unwrap(), &its[1]) && why.is_empty());
}

// ---- scripts (G4) ---------------------------------------------------------------------------------

#[test]
fn test_script_runs_each_step_in_order() {
    let eff = Effectors::default();
    let s = script(&["Year", "next year"], &Senses::new(year_view()), &eff);
    assert_eq!(eff.clicks(), ["Year", "next year"]);
    assert!(s.status == "complete" && s.complete() && s.labels == ["Year", "next year"] && s.already.is_empty());
    assert!(
        s.line
            .starts_with("script: complete; applied=[Year (radio button), next year (button)]; step 2/2 "),
        "{}",
        s.line
    );
    assert!(s.results.len() == 2 && s.log_lines.last().unwrap().starts_with("delegate script: complete"));
    // a step that asks for the view the window is already in costs no input and still counts
    let eff2 = Effectors::default();
    let same = script(&["Month"], &Senses::new(year_view()[..1].to_vec()), &eff2);
    assert!(
        same.status == "complete" && eff2.clicks().is_empty() && same.already == ["Month"] && same.labels.is_empty()
    );
}

#[test]
fn test_script_stops_at_the_first_step_that_does_not_act() {
    let eff = Effectors::default();
    let s = script(
        &["Year", "the fortnight view", "next year"],
        &Senses::new(year_view()),
        &eff,
    );
    assert_eq!(eff.clicks(), ["Year"]); // step 3 never ran
    assert!(s.status == "partial" && s.labels == ["Year"] && s.line.contains("step 2/3 escalate:"));
    assert!(s.line.contains("reason=no_match"));
    // nothing applied at all is "none"
    let eff0 = Effectors::default();
    let none_s = script(&["the fortnight view"], &Senses::new(year_view()), &eff0);
    assert!(none_s.status == "none" && eff0.clicks().is_empty() && none_s.labels.is_empty());
    // a click nobody saw change anything is not "complete": unknown is not proof
    let still = vec![calendar(false), calendar(false)];
    let eff1 = Effectors::default();
    let unseen = script(&["Today"], &Senses::new(still.clone()).changed(vec![None]), &eff1);
    assert!(eff1.clicks() == ["Today"] && unseen.status == "partial");
    let eff2 = Effectors::default();
    let dead = script(
        &["Today", "Week"],
        &Senses::new(still).changed(vec![Some(false)]),
        &eff2,
    );
    assert!(eff2.clicks() == ["Today"] && dead.status == "partial" && dead.line.contains("step 1/2"));
}

#[test]
fn test_script_sensitive_step_confirms_and_clicks_nothing_more() {
    let eff = Effectors::default();
    let s = script(
        &["Week", "Delete Event", "Today"],
        &Senses::new(vec![calendar(false), calendar(true), calendar(true)]),
        &eff,
    );
    assert_eq!(eff.clicks(), ["Week"]);
    assert!(
        s.status == "partial"
            && s.line
                .contains("step 2/3 confirm: \"Delete Event\" needs the human's yes")
    );
}

#[test]
fn test_script_is_capped_at_six_steps() {
    let eff = Effectors::default();
    let seven = vec!["Week"; fl::MAX_SCRIPT_STEPS + 1];
    let s = script(&seven, &Senses::new(vec![calendar(false)]), &eff);
    assert!(s.status == "none" && s.line.contains("at most 6 steps") && eff.clicks().is_empty());
    let empty = script(&["", "  "], &Senses::new(vec![calendar(false)]), &eff);
    assert!(empty.status == "none" && empty.line.contains("at least one step"));
}

// ---- the router (G5) --------------------------------------------------------------------------------

fn apps() -> Vec<String> {
    strings(&["notes", "calendar", "system settings", "safari", "mail"])
}

fn route(
    goal: &str,
    snapshots: Vec<Snapshot>,
    front: &str,
    changed: Option<Vec<Option<bool>>>,
) -> (lr::RouteResult, Effectors) {
    let eff = Effectors::default();
    let mut senses = Senses::new(snapshots);
    if let Some(c) = changed {
        senses = senses.changed(c);
    }
    let clock = fake_clock();
    let r = lr::route(
        goal,
        &senses,
        &eff,
        RouteOptions {
            frontmost_app: front.into(),
            apps: Some(apps()),
            clock: &clock,
            ..RouteOptions::default()
        },
    );
    (r, eff)
}

#[test]
fn test_router_clauses() {
    assert_eq!(
        lr::split_clauses("go to year view and then next year"),
        ["go to year view", "next year"]
    );
    assert_eq!(
        lr::split_clauses("Can you switch to week view, then today"),
        ["switch to week view", "today"]
    );
    assert_eq!(lr::split_clauses("week"), ["week"]);
    assert_eq!(
        lr::normalise("hey eve, could you please switch to week view"),
        "switch to week view"
    );
    assert_eq!(lr::refuse_reason("what is on my calendar this week"), "question");
    assert_eq!(lr::refuse_reason("is week selected?"), "question");
    assert_eq!(lr::refuse_reason("to the"), "empty");
    assert_eq!(
        lr::refuse_reason("alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo"),
        "too_long"
    );
    assert_eq!(lr::refuse_reason("switch to week view"), "");
    assert_eq!(lr::spoken_sentence(&strings(&["Week"]), &[]), "Done. I clicked Week.");
    assert_eq!(
        lr::spoken_sentence(&strings(&["Year", "next year"]), &[]),
        "Done. I clicked Year, then next year."
    );
    assert_eq!(
        lr::spoken_sentence(&[], &strings(&["Month"])),
        "Month is already selected."
    );
    let long = vec!["x".repeat(60); 6];
    assert!(lr::spoken_sentence(&long, &[]).chars().count() <= lr::MAX_SPOKEN_CHARS);
    // a polite "?" is manners, not a question (task_router's use)
    assert!(lr::is_question("can you open slack?", false) && !lr::is_question("open slack?", true));
    assert!(lr::is_question("What's on", true));
}

#[test]
fn test_router_engages_on_a_fully_covered_goal() {
    let (r, eff) = route(
        "Can you switch the calendar to week view",
        vec![calendar(false), calendar(true)],
        "Calendar",
        None,
    );
    assert_eq!(eff.clicks(), ["Week"], "{r:?}");
    assert!(r.status == "complete" && r.clicked == ["Week"] && r.sentence == "Done. I clicked Week.");
    let d = r.to_dict();
    assert!(d["status"] == "complete" && d["clicked"] == json!(["Week"]));
    assert!(!d["log"].as_array().unwrap().is_empty() && d["ms"].as_f64().unwrap() >= 0.0);
    assert_eq!(
        d.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["status", "reason", "line", "clicked", "already", "sentence", "log", "ms"]
    );
    // "the week button": the role word is accounted for, so is the generic "tab"
    assert!(fl::uncovered_tokens(
        "click the week button",
        &cand("2", "radio button", "Week"),
        "Calendar",
        &[]
    )
    .is_empty());
    let generic = strings(lr::GENERIC_WORDS);
    assert!(fl::uncovered_tokens("week tab", &cand("2", "radio button", "Week"), "Calendar", &generic).is_empty());
    // the value never counts: a selected Week does not account for "selected"
    assert_eq!(
        fl::uncovered_tokens(
            "week selected",
            &candv("2", "radio button", "Week", "selected"),
            "",
            &[]
        ),
        ["selected"]
    );
}

#[test]
fn test_router_refuses_a_goal_with_uncovered_words() {
    // "week" matches the Week control, but the request is about a review page: nothing is clicked
    let (r, eff) = route(
        "go to the week in review page for september",
        vec![calendar(false)],
        "Calendar",
        None,
    );
    assert!(
        eff.clicks().is_empty() && r.status == "none" && r.reason == "uncovered",
        "{r:?}"
    );
    assert!(r.log_lines.iter().any(|line| line.contains("unexplained")));
    // a sensitive control is never the router's to click
    let (r2, eff2) = route("delete event", vec![calendar(false)], "Calendar", None);
    assert!(
        eff2.clicks().is_empty() && r2.status == "none" && r2.reason == "confirm",
        "{r2:?}"
    );
    // a question is refused before a single snapshot
    let senses = Senses::new(vec![calendar(false)]);
    let clock = fake_clock();
    let r3 = lr::route(
        "what is on this week",
        &senses,
        &Effectors::default(),
        RouteOptions {
            frontmost_app: "Calendar".into(),
            apps: Some(apps()),
            clock: &clock,
            ..RouteOptions::default()
        },
    );
    assert!(r3.status == "refused" && r3.reason == "question" && senses.calls() == 0);
    // a dialog is never operated
    let sheet = snap_with(
        vec![Candidate {
            in_dialog: true,
            ..cand("1", "button", "Week")
        }],
        SnapOpts {
            dialog_text: "Save changes?",
            ..SnapOpts::default()
        },
    );
    let (r4, eff4) = route("week", vec![sheet], "Calendar", None);
    assert!(
        eff4.clicks().is_empty() && r4.status == "none" && r4.reason == "dialog_open",
        "{r4:?}"
    );
}

#[test]
fn test_router_refuses_a_goal_naming_another_app() {
    let finder = snap_with(
        vec![cand("1", "row", "Notes"), cand("2", "row", "Downloads")],
        SnapOpts {
            app: "Finder",
            bundle: "com.apple.finder",
            ..SnapOpts::default()
        },
    );
    let senses = Senses::new(vec![finder]);
    let eff = Effectors::default();
    let clock = fake_clock();
    let r = lr::route(
        "open notes",
        &senses,
        &eff,
        RouteOptions {
            frontmost_app: "Finder".into(),
            apps: Some(apps()),
            clock: &clock,
            ..RouteOptions::default()
        },
    );
    assert!(r.status == "refused" && r.reason == "names_app:notes" && eff.clicks().is_empty() && senses.calls() == 0);
    // the frontmost app's own name is not "another app"
    assert_eq!(
        lr::names_other_app("switch the calendar to week view", "Calendar", &apps()),
        ""
    );
    assert_eq!(
        lr::names_other_app("open system settings", "Finder", &apps()),
        "system settings"
    );
    assert_eq!(lr::names_other_app("go to week view", "Calendar", &apps()), "");
    let listed = lr::installed_app_names_in(&["/x"], &|_d: &str| {
        Ok(strings(&["Notes.app", "Safari.app", "readme.txt"]))
    });
    assert_eq!(
        listed.into_iter().collect::<HashSet<_>>(),
        ["notes", "safari"].map(String::from).into_iter().collect()
    );
    let missing = lr::installed_app_names_in(&["/missing"], &|_d: &str| Err(std::io::Error::other("gone")));
    assert!(missing.is_empty());
}

#[test]
fn test_router_two_clauses_two_clicks() {
    // the whole goal ties/uncovers on one control, so it is split: [month] + year_view()'s four
    let views = year_view();
    let mut five = vec![views[0].clone()];
    five.extend(views.iter().cloned());
    let (r, eff) = route("go to year view and then next year", five.clone(), "Calendar", None);
    assert_eq!(eff.clicks(), ["Year", "next year"], "{r:?}");
    assert!(r.status == "complete" && r.sentence == "Done. I clicked Year, then next year.");
    // "desktop and dock" is ONE row: the whole goal is tried first and never split
    let settings_opts = |title: &'static str| SnapOpts {
        app: "System Settings",
        bundle: SYSTEM_SETTINGS_BUNDLE,
        title,
        ..SnapOpts::default()
    };
    let settings = snap_with(
        vec![
            cand("1", "row", "Desktop & Dock"),
            cand("2", "row", "General"),
            cand("3", "row", "Displays"),
        ],
        settings_opts(""),
    );
    let opened = snap_with(
        vec![
            candv("1", "row", "Desktop & Dock", "selected"),
            cand("2", "row", "General"),
            cand("3", "row", "Displays"),
            cand("4", "pop up button", "Position on screen"),
        ],
        settings_opts("Desktop & Dock"),
    );
    let (r2, eff2) = route("open desktop and dock", vec![settings, opened], "System Settings", None);
    assert!(eff2.clicks() == ["Desktop & Dock"] && r2.status == "complete", "{r2:?}");
    // "security" is a sensitive word (ax_candidates SENSITIVE_LABEL): that row is the planner's, with ask_user
    let privacy = snap_with(
        vec![cand("1", "row", "Privacy & Security"), cand("2", "row", "General")],
        settings_opts(""),
    );
    let (r_p, eff_p) = route("open privacy and security", vec![privacy], "System Settings", None);
    assert!(
        eff_p.clicks().is_empty() && r_p.status == "none" && r_p.reason == "confirm",
        "{r_p:?}"
    );
    // a second clause that finds nothing leaves a partial: the planner is told what was clicked
    let (r3, eff3) = route("year view and then the fortnight", five, "Calendar", None);
    assert!(eff3.clicks() == ["Year"] && r3.status == "partial" && r3.clicked == ["Year"] && r3.sentence.is_empty());
}

/// Senses whose snapshot fails after the first: Python raised it out of route, and Helpers.lane_first answered
/// RouteResult("none", reason="error:{kind}").
struct FailingSenses {
    first: Snapshot,
    calls: Cell<usize>,
}

impl SensesTrait for FailingSenses {
    fn snapshot(&self) -> anyhow::Result<Snapshot> {
        self.calls.set(self.calls.get() + 1);
        if self.calls.get() > 1 {
            return Err(LaneError::err("RuntimeError", "screen unavailable"));
        }
        Ok(self.first.clone())
    }
    fn text_visible(&self, _: &str) -> anyhow::Result<bool> {
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

#[test]
fn test_a_snapshot_failure_after_input_is_named_not_hidden() {
    let senses = FailingSenses {
        first: calendar(false),
        calls: Cell::new(0),
    };
    let eff = Effectors::default();
    let clock = fake_clock();
    let r = lr::route(
        "switch to week view",
        &senses,
        &eff,
        RouteOptions {
            frontmost_app: "Calendar".into(),
            apps: Some(apps()),
            clock: &clock,
            ..RouteOptions::default()
        },
    );
    assert!(
        r.status == "none" && r.reason == "error:RuntimeError" && eff.clicks() == ["Week"],
        "{r:?}"
    );
    // the infallible run_delegate keeps the applied click in its line
    let senses = FailingSenses {
        first: calendar(false),
        calls: Cell::new(0),
    };
    let eff = Effectors::default();
    let r = delegate_keyword("switch to week view", &senses, &eff, None);
    assert!(
        r.status == LaneStatus::Escalate && r.line.contains("reason=no_window (RuntimeError)"),
        "{}",
        r.line
    );
    assert!(r.line.contains("steps=1; last=click radio button: Week"));
    // and the first snapshot failing is no_window with zero input, as in Python
    struct Dead;
    impl SensesTrait for Dead {
        fn snapshot(&self) -> anyhow::Result<Snapshot> {
            Err(LaneError::err("LookupError", "no window"))
        }
        fn text_visible(&self, _: &str) -> anyhow::Result<bool> {
            Ok(false)
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
    let r = delegate_keyword("switch to week view", &Dead, &Effectors::default(), None);
    assert_eq!(
        r.line,
        "escalate: app=\"\"; reason=no_window (LookupError); top=[]; steps=0; last=none; text=none; key=none"
    );
}

// ---- System Settings doors (G8) ---------------------------------------------------------------------

fn general_pane() -> Vec<Snapshot> {
    let o = |title: &'static str| SnapOpts {
        app: "System Settings",
        bundle: SYSTEM_SETTINGS_BUNDLE,
        title,
        ..SnapOpts::default()
    };
    let before = snap_with(
        vec![
            candv("1", "row", "General", "selected"),
            cand("2", "row", "Appearance"),
            cand("3", "button", "About"),
            cand("4", "button", "Language & Region"),
            cand("5", "button", "Transfer or Reset"),
        ],
        o("General"),
    );
    let after = snap_with(
        vec![
            candv("1", "row", "General", "selected"),
            cand("2", "button", "Back"),
            cand("3", "pop up button", "Region"),
        ],
        o("Language & Region"),
    );
    vec![before, after]
}

#[test]
fn test_settings_general_navigation_button_is_clicked() {
    let eff = Effectors::default();
    let r = delegate_keyword("Language & Region", &Senses::new(general_pane()), &eff, None);
    assert!(
        eff.clicks() == ["Language & Region"] && r.status == LaneStatus::Stopped,
        "{}",
        r.line
    );
    let (routed, eff2) = route("open language and region", general_pane(), "System Settings", None);
    assert!(
        eff2.clicks() == ["Language & Region"] && routed.status == "complete",
        "{routed:?}"
    );
}

#[test]
fn test_settings_value_button_still_confirms() {
    let appearance = snap_with(
        vec![
            candv("1", "row", "Appearance", "selected"),
            cand("2", "button", "Light"),
            cand("3", "button", "Dark"),
        ],
        SnapOpts {
            app: "System Settings",
            bundle: SYSTEM_SETTINGS_BUNDLE,
            title: "Appearance",
            ..SnapOpts::default()
        },
    );
    let eff = Effectors::default();
    let r = delegate_keyword("Dark", &Senses::new(vec![appearance.clone()]), &eff, None);
    assert!(
        r.status == LaneStatus::Confirm && r.line.contains("\"Dark\" needs the human's yes") && eff.clicks().is_empty()
    );
    // a door that is NOT in the closed set is a value like any other button
    let eff2 = Effectors::default();
    let r2 = delegate_keyword("Transfer or Reset", &Senses::new(general_pane()), &eff2, None);
    assert!(r2.status == LaneStatus::Confirm && eff2.clicks().is_empty());
    let (routed, eff3) = route("switch to dark", vec![appearance], "System Settings", None);
    assert!(eff3.clicks().is_empty() && routed.status == "none" && routed.reason == "confirm");
}

// ---- jev mode (test_jev_step.py, test_jev_grouped.py) -----------------------------------------------

/// The envelope jev.make_predict returns.
fn envelope(target: &str, options: &[String], p: f64, present: f64, already_done: f64, risky: f64) -> Value {
    let mut keys: Vec<String> = options.to_vec();
    keys.push(STEP_NONE.into());
    let rest = (1.0 - p) / (keys.len().saturating_sub(1).max(1)) as f64;
    let probs: Map<String, Value> = keys
        .iter()
        .map(|k| (k.clone(), json!(if k == target { p } else { rest })))
        .collect();
    json!({"answers": {
        "target": {"type": "choice", "choice": target, "probabilities": probs},
        "present": {"type": "noul", "noul": present},
        "already_done": {"type": "noul", "noul": already_done},
        "risky": {"type": "noul", "noul": risky}}})
}

/// An asker that picks the option whose text starts with `label` ("none" picks none), and records what it was
/// sent, over the real eve_typed_ask::ask_jev_step.
struct AskerSaying {
    label: String,
    p: f64,
    risky: f64,
    seen: RefCell<Vec<Value>>,
}

fn asker_saying(label: &str) -> AskerSaying {
    AskerSaying {
        label: label.into(),
        p: 0.95,
        risky: 0.05,
        seen: RefCell::new(Vec::new()),
    }
}

impl StepAsk for AskerSaying {
    fn ask(
        &self,
        step: &str,
        app: &str,
        context: &str,
        options: &IndexMap<String, String>,
        recent: &[String],
    ) -> StepAnswer {
        let predict = |_state: &Value, questions: &Value| -> anyhow::Result<Value> {
            self.seen.borrow_mut().push(questions.clone());
            let criteria = questions["target"]["criteria"].as_object().cloned().unwrap_or_default();
            let opts: Vec<(String, String)> = criteria
                .iter()
                .filter(|(k, _)| *k != STEP_NONE)
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                .collect();
            let target = opts
                .iter()
                .find(|(_, v)| v.starts_with(&self.label))
                .map_or(STEP_NONE.to_string(), |(k, _)| k.clone());
            let keys: Vec<String> = opts.into_iter().map(|(k, _)| k).collect();
            Ok(envelope(&target, &keys, self.p, 0.95, 0.05, self.risky))
        };
        let clock = fake_clock();
        eve_typed_ask::ask_jev_step(&predict, step, app, context, options, recent, &clock)
    }
}

fn jev(objective: &str, senses: &Senses, eff: &Effectors, asker: Option<&dyn StepAsk>) -> DelegateResult {
    let clock = fake_clock();
    run_delegate(
        objective,
        senses,
        eff,
        None,
        DelegateOptions {
            decide: "jev".into(),
            asker,
            clock: &clock,
            ..DelegateOptions::default()
        },
    )
}

#[test]
fn test_jev_mode_presses_the_gate_pick_when_jev_agrees() {
    let eff = Effectors::default();
    let asker = asker_saying("Week");
    let r = jev(
        "switch to week view",
        &Senses::new(vec![calendar(false), calendar(true)]),
        &eff,
        Some(&asker),
    );
    assert!(
        eff.clicks() == ["Week"] && r.steps.last().unwrap().via == "keyword+jev",
        "{}",
        r.line
    );
    assert_eq!(asker.seen.borrow().len(), 1);
}

#[test]
fn test_jev_mode_refuses_a_gate_pick_jev_does_not_share() {
    let eff = Effectors::default();
    let r = jev(
        "switch to week view",
        &Senses::new(vec![calendar(false)]),
        &eff,
        Some(&asker_saying("none")),
    );
    assert!(
        r.status == LaneStatus::Escalate && r.line.contains("reason=jev_veto") && eff.clicks().is_empty(),
        "{}",
        r.line
    );
}

#[test]
fn test_jev_mode_decides_a_step_the_gate_cannot() {
    // "show the next seven days" shares no word with "Week": the keyword gate says no_match, Jev says Week.
    let eff = Effectors::default();
    let r = jev(
        "show the next seven days",
        &Senses::new(vec![calendar(false), calendar(true)]),
        &eff,
        Some(&asker_saying("Week")),
    );
    let last = r.steps.last().unwrap();
    assert!(
        eff.clicks() == ["Week"] && last.via == "jev" && last.p_top >= 0.9,
        "{}",
        r.line
    );
}

#[test]
fn test_jev_mode_presses_nothing_on_a_weak_or_failed_answer() {
    let eff = Effectors::default();
    let weak_asker = AskerSaying {
        p: 0.6,
        ..asker_saying("Week")
    };
    let weak = jev(
        "show the next seven days",
        &Senses::new(vec![calendar(false)]),
        &eff,
        Some(&weak_asker),
    );
    assert!(
        weak.status == LaneStatus::Escalate && weak.line.contains("reason=jev_none") && eff.clicks().is_empty(),
        "{}",
        weak.line
    );
    let dead = |_: &str, _: &str, _: &str, _: &IndexMap<String, String>, _: &[String]| StepAnswer {
        error: "HTTP 503".into(),
        ..StepAnswer::new("", 0.0, 0.0, 0.0, 0.0, 1.0)
    };
    let failed = jev(
        "switch to week view",
        &Senses::new(vec![calendar(false)]),
        &eff,
        Some(&dead),
    );
    assert!(
        failed.status == LaneStatus::Escalate && failed.line.contains("reason=jev_error") && eff.clicks().is_empty(),
        "{}",
        failed.line
    );
    assert!(failed
        .log_lines
        .iter()
        .any(|l| l.starts_with("delegate jev: target=- p=0.00") && l.ends_with("HTTP 503")));
    let missing = jev("switch to week view", &Senses::new(vec![calendar(false)]), &eff, None);
    assert!(missing.status == LaneStatus::Unavailable && missing.line.contains("jev is not configured"));
}

#[test]
fn test_jev_mode_a_risky_step_confirms_and_clicks_nothing() {
    let eff = Effectors::default();
    let risky = AskerSaying {
        risky: 0.8,
        ..asker_saying("Week")
    };
    let r = jev(
        "switch to week view",
        &Senses::new(vec![calendar(false)]),
        &eff,
        Some(&risky),
    );
    assert!(
        r.status == LaneStatus::Confirm && r.line.contains("\"Week\"") && eff.clicks().is_empty(),
        "{}",
        r.line
    );
    // the human's yes for that control lets the risky pick through, once
    let eff2 = Effectors::default();
    let clock = fake_clock();
    let r2 = run_delegate(
        "switch to week view",
        &Senses::new(vec![calendar(false), calendar(true)]),
        &eff2,
        None,
        DelegateOptions {
            decide: "jev".into(),
            asker: Some(&risky),
            approve: Some("week".into()),
            clock: &clock,
            ..DelegateOptions::default()
        },
    );
    assert!(
        r2.status == LaneStatus::Stopped && eff2.clicks() == ["Week"],
        "{}",
        r2.line
    );
}

#[test]
fn test_jev_mode_never_offers_a_sensitive_control_and_the_code_gate_still_confirms() {
    // The sensitive control is withheld from the menu, so Jev cannot pick it; the lane confirms it in code.
    let window = snap(vec![
        cand("1", "button", "Delete Event"),
        cand("2", "button", "Add Event"),
        cand("3", "button", "Today"),
    ]);
    let eff = Effectors::default();
    let asker = asker_saying("Add");
    let r = jev("delete the event", &Senses::new(vec![window]), &eff, Some(&asker));
    assert!(r.status == LaneStatus::Confirm && r.line.contains("Delete Event") && eff.clicks().is_empty());
    assert!(asker.seen.borrow().is_empty(), "{}", r.line);
}

#[test]
fn test_jev_mode_offers_the_wider_menu() {
    let many = snap(
        (1..=40)
            .map(|i| Candidate {
                frame: (10, 10 + 30 * i, 80, 24),
                ..cand(&i.to_string(), "button", &format!("Item {i}"))
            })
            .collect(),
    );
    let asker = asker_saying("Item 7 ");
    jev(
        "the seventh thing",
        &Senses::new(vec![many.clone(), many]),
        &Effectors::default(),
        Some(&asker),
    );
    let seen = asker.seen.borrow();
    let offered = seen[0]["target"]["criteria"]
        .as_object()
        .unwrap()
        .keys()
        .filter(|k| *k != STEP_NONE)
        .count();
    assert_eq!(offered, fl::JEV_MAX_OPTIONS - 2);
}

struct CountingAsker {
    max: Option<usize>,
    seen: RefCell<Vec<usize>>,
}

impl StepAsk for CountingAsker {
    fn ask(&self, _: &str, _: &str, _: &str, options: &IndexMap<String, String>, _: &[String]) -> StepAnswer {
        self.seen.borrow_mut().push(options.len());
        StepAnswer::new(STEP_NONE, 0.0, 0.0, 0.0, 0.0, 0.0)
    }
    fn max_options(&self) -> Option<usize> {
        self.max
    }
}

#[test]
fn test_run_delegate_offers_a_grouped_asker_up_to_its_max_and_a_plain_one_25() {
    let things = || {
        let els: Vec<Candidate> = (0..90)
            .map(|i| Candidate {
                id: i.to_string(),
                role: "button".into(),
                label: format!("Thing {i}"),
                frame: (10, 10 + 30 * i, 80, 20),
                actions: vec!["AXPress".into()],
                enabled: true,
                app: "App".into(),
                ..Candidate::default()
            })
            .collect();
        Snapshot {
            seq: SEQ.fetch_add(1, Ordering::SeqCst) + 1,
            app: "App".into(),
            bundle: "x.app".into(),
            title: "w".into(),
            pid: 1,
            elements: els,
            ..Snapshot::default()
        }
    };
    for (max, expect) in [(None, 25), (Some(GROUPED_MAX_OPTIONS), 90)] {
        let asker = CountingAsker {
            max,
            seen: RefCell::new(Vec::new()),
        };
        let eff = Effectors::default();
        let clock = fake_clock();
        let mut senses = Senses::new(vec![things()]);
        senses.pid = 1;
        run_delegate(
            "press thing",
            &senses,
            &eff,
            None,
            DelegateOptions {
                decide: "jev".into(),
                asker: Some(&asker),
                max_steps: 1,
                clock: &clock,
                ..DelegateOptions::default()
            },
        );
        assert_eq!(*asker.seen.borrow().last().unwrap(), expect);
        assert!(eff.clicks().is_empty()); // nothing should be pressed
    }
}

// ---- the shipped switches (test_fastlane_eval.py, test_agent_lane_first.py) ------------------------

#[test]
fn test_shipped_defaults_follow_the_evals() {
    // The holdout eval failed two of FAST_LANE's four conditions; the router cleared its bar.
    const { assert!(!fl::FAST_LANE_DEFAULT) };
    const { assert!(fl::LANE_FIRST_DEFAULT) };
    assert_eq!(fl::DECIDE_MODES, ["keyword", "model", "jev"]);
    assert_eq!(fl::DEFAULT_DECIDE, "keyword");
    assert_eq!(fl::JEV_MAX_OPTIONS, 27);
    assert_eq!(lr::MAX_CLAUSES, 4);
    assert_eq!(lr::STATUSES, ["complete", "partial", "none", "refused"]);
}

// ---- the decisions loop (test_decision_lane.py) -----------------------------------------------------

fn solid(w: u32, h: u32, rgb: [u8; 3]) -> DynamicImage {
    DynamicImage::ImageRgb8(RgbImage::from_pixel(w, h, Rgb(rgb)))
}

/// desktop_helpers._LaneAdapter.image_matches: any pixel change below the 32-point menu bar is a mismatch.
fn image_matches(before: &DynamicImage, after: &DynamicImage) -> bool {
    LaneAdapter::image_matches(before, after)
}

struct DSenses {
    states: Vec<Snapshot>,
    calls: Cell<usize>,
    stale_image: bool,
    pid: i64,
    fail_on: Option<usize>,
}

fn dsenses(states: Vec<Snapshot>) -> DSenses {
    DSenses {
        states,
        calls: Cell::new(0),
        stale_image: false,
        pid: 42,
        fail_on: None,
    }
}

impl dl::DecisionSenses for DSenses {
    fn observe(&self) -> anyhow::Result<(Snapshot, DynamicImage)> {
        if self.fail_on == Some(self.calls.get()) {
            return Err(LaneError::err("RuntimeError", "screen unavailable"));
        }
        let s = self.states[self.calls.get().min(self.states.len() - 1)].clone();
        self.calls.set(self.calls.get() + 1);
        let red = self.stale_image && self.calls.get() == 2;
        Ok((s, solid(800, 500, if red { [255, 0, 0] } else { [255, 255, 255] })))
    }

    fn image_matches(&self, before: &DynamicImage, after: &DynamicImage) -> bool {
        image_matches(before, after)
    }

    fn frontmost_pid(&self) -> anyhow::Result<i64> {
        Ok(self.pid)
    }
}

struct Client {
    target: String,
    verified: f64,
    error: String,
    risky: f64,
    covered: f64,
    present: f64,
    p: f64,
    /// step → target (the Ordered client of the multi-step test)
    by_step: Vec<(&'static str, &'static str)>,
    calls: Vec<(Map<String, Value>, usize, usize)>,
}

fn client() -> Client {
    Client {
        target: "2".into(),
        verified: 0.99,
        error: String::new(),
        risky: 0.01,
        covered: 0.99,
        present: 0.99,
        p: 0.99,
        by_step: Vec::new(),
        calls: Vec::new(),
    }
}

impl Evaluate for Client {
    fn evaluate(&mut self, evidence: &Map<String, Value>, images: &[DynamicImage], questions: &[Value]) -> Answer {
        self.calls.push((evidence.clone(), images.len(), questions.len()));
        if !self.by_step.is_empty() {
            let step = evidence.get("step").and_then(Value::as_str).unwrap_or("");
            self.target = self
                .by_step
                .iter()
                .find(|(s, _)| *s == step)
                .map_or("2", |(_, t)| *t)
                .into();
        }
        let values = if questions.len() == 1 {
            json!({"verified": self.verified})
        } else {
            json!({"target": {"id": self.target, "p": self.p, "margin": 0.98}, "present": self.present,
                   "covered": self.covered, "risky": self.risky})
        };
        Answer::with_error(values.as_object().unwrap().clone(), self.error.clone())
    }
}

fn droute(goal: &str, senses: &DSenses, eff: &Effectors, cli: &mut Client, dry_run: bool) -> lr::RouteResult {
    dl::route(goal, senses, eff, cli, Some(&[]), dl::Gates::default(), dry_run)
}

fn drun(
    states: Option<Vec<Snapshot>>,
    mut cli: Client,
    eff: Effectors,
    dry_run: bool,
) -> (lr::RouteResult, Effectors, Client) {
    let senses = dsenses(states.unwrap_or_else(|| vec![calendar(false), calendar(false), calendar(true)]));
    let r = droute("show the next seven days", &senses, &eff, &mut cli, dry_run);
    (r, eff, cli)
}

#[test]
fn test_observe_select_revalidate_act_verify_completes_without_planner() {
    let (r, eff, cli) = drun(None, client(), Effectors::default(), false);
    assert!(
        r.status == "complete" && r.clicked == ["Week"] && eff.clicks() == ["Week"],
        "{r:?}"
    );
    assert!(cli.calls.len() == 2 && cli.calls[0].1 == 1 && cli.calls[1].1 == 2);
    assert_ne!(
        cli.calls[1].0["before"]["snapshot_seq"],
        cli.calls[1].0["after"]["snapshot_seq"]
    );
    assert!(r.sentence.contains("Week"));
    assert!(
        r.log_lines[0].starts_with("decisions select: ")
            && r.log_lines[0].contains("present=0.990 covered=0.990 risky=0.010 p=0.990 margin=0.980")
    );
    assert!(r.log_lines[1].starts_with("decisions verify: ") && r.log_lines[1].ends_with("verified=0.990"));
}

#[test]
fn test_model_abstention_and_risk_stop_before_input() {
    let cases: Vec<Client> = vec![
        Client { risky: 0.9, ..client() },
        Client {
            covered: 0.5,
            ..client()
        },
        Client {
            present: 0.3,
            ..client()
        },
        Client { p: 0.2, ..client() },
        Client {
            error: "refused".into(),
            ..client()
        },
        Client {
            target: "unoffered".into(),
            ..client()
        },
        Client {
            target: "none".into(),
            ..client()
        },
    ];
    for c in cases {
        let (r, eff, _) = drun(None, c, Effectors::default(), false);
        assert!(r.status == "none" && eff.clicks().is_empty(), "{r:?}");
    }
}

#[test]
fn test_stale_target_or_state_prevents_click() {
    for mutation in ["moved", "disabled", "dialog", "app", "truncated", "value"] {
        let before = calendar(false);
        let mut after = before.clone();
        match mutation {
            "moved" => after.elements[2].frame = (5, 5, 60, 30),
            "disabled" => after.elements[2].enabled = false,
            "value" => after.elements[2].value = "selected".into(),
            "dialog" => after.dialog_text = "Confirm".into(),
            "app" => after.pid = 99,
            _ => after.truncated = true,
        }
        let (r, eff, _) = drun(Some(vec![before, after]), client(), Effectors::default(), false);
        assert!(
            r.reason == "stale_observation" && eff.clicks().is_empty(),
            "{mutation}: {r:?}"
        );
    }
}

#[test]
fn test_pixels_can_invalidate_even_an_identical_ax_tree() {
    let eff = Effectors::default();
    let senses = DSenses {
        stale_image: true,
        ..dsenses(vec![calendar(false)])
    };
    let r = droute("show the next seven days", &senses, &eff, &mut client(), false);
    assert!(r.reason == "stale_observation" && eff.clicks().is_empty());
    assert!(r.log_lines.contains(&"decisions admission: pixels changed".to_string()));
}

#[test]
fn test_unknown_effect_is_partial_and_named_in_codex_handoff() {
    let (r, eff, cli) = drun(
        None,
        Client {
            verified: 0.2,
            ..client()
        },
        Effectors::default(),
        false,
    );
    assert!(r.status == "partial" && r.reason == "unverified" && eff.clicks() == ["Week"]);
    // desk_lane.codex_note (another crate) reads `clicked` off to_dict
    assert!(r.to_dict()["clicked"] == json!(["Week"]) && cli.calls.len() == 2);
}

#[test]
fn test_exception_after_delivering_input_preserves_progress() {
    let (r, eff, _) = drun(
        None,
        client(),
        Effectors {
            raise_after_click: true,
            ..Effectors::default()
        },
        false,
    );
    assert!(
        r.status == "partial" && r.clicked == eff.clicks() && eff.clicks() == ["Week"],
        "{r:?}"
    );
    assert!(r.reason == "error:RuntimeError" && r.log_lines.last().unwrap() == "decisions input: RuntimeError");
}

#[test]
fn test_post_action_capture_failure_preserves_progress() {
    let senses = DSenses {
        fail_on: Some(2),
        ..dsenses(vec![calendar(false)])
    };
    let eff = Effectors::default();
    let r = droute("show the next seven days", &senses, &eff, &mut client(), false);
    assert!(r.status == "partial" && r.clicked == ["Week"], "{r:?}");
    assert_eq!(
        r.log_lines.last().unwrap(),
        "decisions verification observation: RuntimeError"
    );
}

#[test]
fn test_refused_hit_test_is_zero_input_and_dry_run_does_not_act() {
    let (r, eff, _) = drun(
        None,
        client(),
        Effectors {
            refuse_click: true,
            ..Effectors::default()
        },
        false,
    );
    assert!(r.status == "none" && r.clicked.is_empty() && eff.clicks().is_empty());
    let (r, eff, cli) = drun(None, client(), Effectors::default(), true);
    assert!(r.reason == "dry_run" && eff.clicks().is_empty() && cli.calls.len() == 1);
}

#[test]
fn test_request_gates_refuse_before_observation() {
    for goal in [
        "delete the event",
        "send it",
        "type hello",
        "what is on my screen",
        "turn off wifi",
    ] {
        let senses = dsenses(vec![calendar(false)]);
        let eff = Effectors::default();
        let mut cli = client();
        let r = droute(goal, &senses, &eff, &mut cli, false);
        assert!(
            r.status == "none" && senses.calls.get() == 0 && eff.clicks().is_empty() && cli.calls.is_empty(),
            "{goal}: {r:?}"
        );
    }
}

#[test]
fn test_withheld_and_system_settings_controls_are_never_offered() {
    let before = snap(vec![cand("1", "button", "Delete Event"), cand("2", "button", "Today")]);
    assert!(dl::menu("delete the event", &before).is_empty());
    let settings = Snapshot {
        bundle: SYSTEM_SETTINGS_BUNDLE.into(),
        ..calendar(false)
    };
    let (r, eff, cli) = drun(Some(vec![settings]), client(), Effectors::default(), false);
    assert!(r.reason == "unsafe_observation" && eff.clicks().is_empty() && cli.calls.is_empty());
}

#[test]
fn test_selected_target_is_satisfied_without_input() {
    let (r, eff, cli) = drun(Some(vec![calendar(true)]), client(), Effectors::default(), false);
    assert!(
        r.status == "complete" && r.already == ["Week"] && eff.clicks().is_empty() && cli.calls.len() == 1,
        "{r:?}"
    );
}

#[test]
fn test_focus_unknown_never_authorizes_input() {
    let eff = Effectors::default();
    let senses = DSenses {
        pid: 0,
        ..dsenses(vec![calendar(false)])
    };
    let r = droute("show the next seven days", &senses, &eff, &mut client(), false);
    assert!(r.reason == "focus_changed" && eff.clicks().is_empty());
}

#[test]
fn test_multi_step_stops_on_unverified_first_click() {
    let eff = Effectors::default();
    let mut cli = Client {
        verified: 0.1,
        ..client()
    };
    let r = droute(
        "week view then month view",
        &dsenses(vec![calendar(false)]),
        &eff,
        &mut cli,
        false,
    );
    assert!(r.status == "partial" && eff.clicks() == ["Week"] && cli.calls.len() == 2);
}

#[test]
fn test_same_control_cannot_be_repeated_after_a_verified_action() {
    let eff = Effectors::default();
    let mut cli = client();
    let r = droute(
        "week view then week view",
        &dsenses(vec![calendar(false)]),
        &eff,
        &mut cli,
        false,
    );
    assert!(
        r.status == "partial" && r.reason == "repeat" && eff.clicks() == ["Week"],
        "{r:?}"
    );
}

#[test]
fn test_successful_multiple_steps_have_separate_observations_and_verifications() {
    let eff = Effectors::default();
    let mut cli = Client {
        by_step: vec![("month view", "3")],
        ..client()
    };
    let states = vec![
        calendar(false),
        calendar(false),
        calendar(true),
        calendar(true),
        calendar(true),
        calendar(false),
    ];
    let r = droute("week view then month view", &dsenses(states), &eff, &mut cli, false);
    assert!(
        r.status == "complete" && eff.clicks() == ["Week", "Month"] && cli.calls.len() == 4,
        "{r:?}"
    );
}

#[test]
fn test_pixel_checker_has_positive_and_negative_controls() {
    let (white, red) = (solid(100, 100, [255, 255, 255]), solid(100, 100, [255, 0, 0]));
    assert!(image_matches(&white, &white.clone()));
    assert!(!image_matches(&white, &red));
}

#[test]
fn test_gates_pick_scores_and_evidence() {
    let g = dl::Gates::default();
    let ok = |v: Value| Answer::new(v.as_object().unwrap().clone());
    assert_eq!(
        g.pick(&ok(
            json!({"target": {"id": "2", "p": 0.99, "margin": 0.9}, "present": 0.99, "covered": 0.99, "risky": 0.0})
        )),
        "2"
    );
    assert_eq!(
        g.pick(&ok(
            json!({"target": {"id": "2", "p": 0.99, "margin": 0.9}, "present": 0.99, "covered": 0.99})
        )),
        ""
    );
    assert_eq!(
        g.pick(&ok(
            json!({"target": {"id": "none", "p": 0.99, "margin": 0.9}, "present": 0.99, "covered": 0.99, "risky": 0.0})
        )),
        ""
    );
    assert_eq!(dl::scores(&Answer::with_error(Map::new(), "x"), true), "model_error");
    assert_eq!(dl::scores(&ok(json!({"verified": true})), false), "");
    assert_eq!(dl::scores(&ok(json!({"verified": 1})), false), "verified=1.000");
    // evidence: secure fields and editable values never leave; context lines only from safe metadata
    let mut s = snap_with(
        vec![
            cand("0", "static text", "Total"),
            Candidate {
                secure: true,
                value: "hunter2".into(),
                ..cand("1", "text field", "Password")
            },
            candv("2", "radio button", "Week", "selected"),
            candv("3", "text field", "Note", "private words"),
        ],
        SnapOpts {
            focused: Some(Candidate {
                secure: true,
                ..cand("1", "text field", "Password")
            }),
            ..SnapOpts::default()
        },
    );
    s.context_lines = strings(&[
        "title: September 2026",
        "focused: text field Password",
        "Total",
        "something else",
    ]);
    s.readouts = vec![("0".into(), "$12.00".into()), ("3".into(), "private words".into())];
    let e = dl::evidence("show totals", &s);
    assert_eq!(
        e.keys().collect::<Vec<_>>(),
        ["step", "app", "snapshot_seq", "context", "readouts", "controls"]
    );
    assert_eq!(e["context"], json!(["title: September 2026", "Total"]));
    assert_eq!(e["readouts"], json!([{"id": "0", "label": "Total", "text": "$12.00"}]));
    let controls = e["controls"].as_array().unwrap();
    assert_eq!(controls.len(), 3);
    assert_eq!(
        controls[1],
        json!({"id": "2", "role": "radio button", "label": "Week", "value": "selected",
                                   "rect_points": [240, 100, 60, 30], "enabled": true})
    );
    assert_eq!(controls[2]["value"], "");
    assert!(dl::same_state(&s, &s.clone()));
    assert!(!dl::same_state(
        &Snapshot { pid: 0, ..s.clone() },
        &Snapshot { pid: 0, ..s.clone() }
    ));
}

#[test]
fn test_route_result_ms_rounds_as_pythons_round() {
    // Python: round(0.25, 1) == 0.2 (tie to even), round(0.35, 1) == 0.3 (0.35 is below the tie in binary),
    // round(2.675, 2) == 2.67, round(0.125, 2) == 0.12.
    let ms = |v: f64| lr::RouteResult { ms: v, ..lr::RouteResult::new("none") }.to_dict()["ms"].as_f64().unwrap();
    assert_eq!(ms(0.25), 0.2);
    assert_eq!(ms(0.35), 0.3);
    assert_eq!(ms(0.45), 0.5);
    assert_eq!(ms(12.0), 12.0);
    assert_eq!(eve_desktop_lanes::py_round_n(2.675, 2), 2.67);
    assert_eq!(eve_desktop_lanes::py_round_n(0.125, 2), 0.12);
    assert_eq!(eve_desktop_lanes::py_round_n(0.375, 2), 0.38);
}
