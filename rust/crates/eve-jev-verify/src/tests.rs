//! Port of bridge/tests/test_jev_verify.py.
//!
//! Python walks the journeys on a real page (app_check's sandbox in headless Chromium). Here the page is a fake
//! [`Page`]: each app below is a small Rust model of the test's HTML that answers COLLECT_JS, TEXT_JS and
//! PRESS_JS the way the page scripts would, and acts on the controls the scripts tagged. Jev is faked by a literal
//! reader that answers the questions jev_verify really sends, as in Python, so the wiring, the gates and the
//! failure paths are tested with no network and no browser. What only a browser can show (COLLECT_JS's own DOM
//! walk, app_check's sandbox) is app-check's to test.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::*;

// ---- Jev, faked ------------------------------------------------------------------------------------------

/// Answers jev_verify's two questions the way a literal reader would, and counts the calls.
fn literal_answer(state: &Value, questions: &Value) -> Value {
    if questions.get("target").is_some() {
        let instructions = questions["present"]["instructions"].as_str().unwrap_or("");
        let label = instructions.split('"').nth(1).unwrap_or("").to_lowercase();
        let hits: Vec<String> = questions["target"]["criteria"]
            .as_object()
            .unwrap()
            .iter()
            .filter(|(k, v)| {
                k.as_str() != STEP_NONE && v.as_str().unwrap_or("").split(" (").next().unwrap_or("").to_lowercase() == label
            })
            .map(|(k, _)| k.clone())
            .collect();
        let key = hits.first().cloned().unwrap_or_else(|| STEP_NONE.to_string());
        return json!({"answers": {"target": {"choice": key, "probabilities": {key.clone(): 0.99}},
                                  "present": {"noul": if hits.is_empty() { 0.03 } else { 0.97 }}},
                      "usage": {"input_tokens": 500}});
    }
    let screen: Vec<&str> = state["screen"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    let screen = screen.join(" ");
    let mut answers = serde_json::Map::new();
    for (q, v) in questions.as_object().unwrap() {
        let text = v["instructions"].as_str().unwrap().split("show: ").nth(1).unwrap().split(" ?").next().unwrap();
        answers.insert(q.clone(), json!({"noul": if screen.contains(text) { 0.96 } else { 0.02 }}));
    }
    json!({"answers": answers, "usage": {"input_tokens": 300}})
}

struct LiteralJev {
    calls: Arc<AtomicUsize>,
}

impl LiteralJev {
    fn new() -> Self {
        Self { calls: Arc::new(AtomicUsize::new(0)) }
    }

    fn predict(&self) -> Predict {
        let calls = self.calls.clone();
        Arc::new(move |state: &Value, questions: &Value| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(literal_answer(state, questions))
        })
    }
}

fn literal() -> Verifier {
    Verifier::new(LiteralJev::new().predict())
}

// ---- the page, faked -------------------------------------------------------------------------------------

/// One control as COLLECT_JS would read it; `handle` is the app's own name for it.
#[derive(Clone)]
struct Widget {
    handle: usize,
    kind: &'static str,
    label: String,
    type_: &'static str,
    placeholder: &'static str,
    value: String,
    options: Vec<String>,
}

impl Widget {
    fn new(handle: usize, kind: &'static str, label: &str) -> Self {
        Self { handle, kind, label: label.into(), type_: "", placeholder: "", value: String::new(), options: Vec::new() }
    }
}

enum Blur {
    Done,
    Hang,
}

/// An app's page as the scripts see it.
trait FakeApp: Send {
    fn widgets(&self) -> Vec<Widget>;
    fn heading(&self) -> String;
    fn body_text(&self) -> String;
    fn main_button(&self) -> Option<String> {
        None
    }
    fn fill(&mut self, _handle: usize, _value: &str) {}
    fn blur(&mut self, _handle: usize) -> Blur {
        Blur::Done
    }
    fn click(&mut self, _handle: usize) {}
    fn select(&mut self, _handle: usize, _label: &str) {}
    fn press(&mut self, _name: &str) -> bool {
        false
    }
}

struct FakePage {
    app: parking_lot::Mutex<Box<dyn FakeApp>>,
    /// data-journey-id -> handle, as the last COLLECT_JS tagged them
    tags: parking_lot::Mutex<Vec<usize>>,
    hung: AtomicBool,
    pressed: parking_lot::Mutex<Vec<String>>,
}

impl FakePage {
    fn new(app: impl FakeApp + 'static) -> Arc<Self> {
        Arc::new(Self {
            app: parking_lot::Mutex::new(Box::new(app)),
            tags: parking_lot::Mutex::new(Vec::new()),
            hung: AtomicBool::new(false),
            pressed: parking_lot::Mutex::new(Vec::new()),
        })
    }

    /// The app re-rendered its view: every tagged node was replaced.
    fn rerender(&self) {
        self.tags.lock().clear();
    }

    fn handle(&self, selector: &str) -> Option<usize> {
        let id: usize = selector.strip_prefix("[data-journey-id='")?.strip_suffix("']")?.parse().ok()?;
        self.tags.lock().get(id).copied()
    }

    fn located(&self, selector: &str) -> anyhow::Result<usize> {
        self.handle(selector)
            .ok_or_else(|| PageError::new("TimeoutError", format!("Timeout 1500ms exceeded.\nwaiting for {selector}")).into())
    }

    async fn stuck(&self) {
        if self.hung.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
    }
}

#[async_trait]
impl Page for FakePage {
    async fn evaluate(&self, js: &str, arg: Option<Value>) -> anyhow::Result<Value> {
        self.stuck().await;
        let app = &mut *self.app.lock();
        if js == COLLECT_JS {
            let widgets = app.widgets();
            *self.tags.lock() = widgets.iter().map(|w| w.handle).collect();
            let controls: Vec<Value> = widgets
                .iter()
                .enumerate()
                .map(|(i, w)| {
                    json!({"kind": w.kind, "label": w.label, "type": w.type_, "placeholder": w.placeholder,
                           "value": w.value, "options": w.options, "on": false, "id": i.to_string()})
                })
                .collect();
            return Ok(json!({"controls": controls, "heading": app.heading(), "main": app.main_button().unwrap_or_default(),
                             "back": false, "text": app.body_text()}));
        }
        if js == TEXT_JS {
            let main = app.main_button().map(|m| format!("\n{m}")).unwrap_or_default();
            return Ok(Value::String(app.body_text() + &main));
        }
        if js == PRESS_JS {
            let name = arg.and_then(|a| a.as_str().map(String::from)).unwrap_or_default();
            self.pressed.lock().push(name.clone());
            return Ok(Value::Bool(app.press(&name)));
        }
        anyhow::bail!("unexpected script")
    }

    async fn count(&self, selector: &str) -> anyhow::Result<usize> {
        self.stuck().await;
        Ok(usize::from(self.handle(selector).is_some()))
    }

    async fn fill(&self, selector: &str, value: &str, _timeout_ms: u64) -> anyhow::Result<()> {
        let h = self.located(selector)?;
        self.app.lock().fill(h, value);
        Ok(())
    }

    async fn evaluate_on(&self, selector: &str, js: &str, _timeout_ms: u64) -> anyhow::Result<Value> {
        assert_eq!(js, BLUR_JS);
        let h = self.located(selector)?;
        let blur = self.app.lock().blur(h);
        if let Blur::Hang = blur {
            self.hung.store(true, Ordering::SeqCst);
            std::future::pending::<()>().await;
        }
        Ok(Value::Null)
    }

    async fn select_option(&self, selector: &str, label: Option<&str>, _timeout_ms: u64) -> anyhow::Result<()> {
        let h = self.located(selector)?;
        if let Some(label) = label {
            self.app.lock().select(h, label);
        }
        Ok(())
    }

    async fn scroll_into_view_if_needed(&self, selector: &str, _timeout_ms: u64) -> anyhow::Result<()> {
        self.located(selector).map(|_| ())
    }

    async fn click(&self, selector: &str, _timeout_ms: u64) -> anyhow::Result<()> {
        let h = self.located(selector)?;
        self.app.lock().click(h);
        Ok(())
    }

    async fn wait_for_timeout(&self, _ms: u64) {}
}

// ---- the apps --------------------------------------------------------------------------------------------

/// A small expense splitter: add a person, add an expense, see who owes whom (SPLITTER in the Python test).
/// `broken` is the negative control: Add expense does nothing.
#[derive(Default)]
struct Splitter {
    broken: bool,
    name: String,
    amount: String,
    payer: String,
    people: Vec<String>,
    exp: Vec<(f64, String)>,
}

impl Splitter {
    fn render(&mut self) {
        // the select is rebuilt: its first option is selected again
        self.payer = self.people.first().cloned().unwrap_or_default();
    }

    fn balance(&self) -> String {
        if self.exp.is_empty() {
            return "No expenses yet".into();
        }
        let total: f64 = self.exp.iter().map(|e| e.0).sum();
        let share = if self.people.is_empty() { 0.0 } else { total / self.people.len() as f64 };
        self.people
            .iter()
            .map(|p| {
                let paid: f64 = self.exp.iter().filter(|e| &e.1 == p).map(|e| e.0).sum();
                format!("{p} balance {:.2}", paid - share)
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl FakeApp for Splitter {
    fn widgets(&self) -> Vec<Widget> {
        vec![
            Widget { placeholder: "e.g. Alex", value: self.name.clone(), ..Widget::new(0, "field", "Name") },
            Widget::new(1, "button", "Add person"),
            Widget { type_: "number", placeholder: "e.g. 30", value: self.amount.clone(), ..Widget::new(2, "field", "Amount") },
            Widget { options: self.people.clone(), value: self.payer.clone(), ..Widget::new(3, "select", "Paid by") },
            Widget::new(4, "button", "Add expense"),
        ]
    }

    fn heading(&self) -> String {
        "Splitter".into()
    }

    fn body_text(&self) -> String {
        let mut lines = vec!["Splitter".to_string(), "Name".into(), "Add person".into(), "Amount".into(), "Paid by".into()];
        lines.extend(self.people.iter().cloned());
        lines.push("Add expense".into());
        lines.extend(self.people.iter().cloned());
        lines.push(self.balance());
        lines.join("\n")
    }

    fn fill(&mut self, handle: usize, value: &str) {
        match handle {
            0 => self.name = value.into(),
            2 => self.amount = value.into(),
            _ => {}
        }
    }

    fn click(&mut self, handle: usize) {
        match handle {
            1 => {
                let n = self.name.trim().to_string();
                if n.is_empty() {
                    return;
                }
                self.people.push(n);
                self.name.clear();
                self.render();
            }
            4 if !self.broken => {
                self.exp.push((self.amount.parse().unwrap_or(0.0), self.payer.clone()));
                self.render();
            }
            _ => {}
        }
    }

    fn select(&mut self, handle: usize, label: &str) {
        if handle == 3 && self.people.iter().any(|p| p == label) {
            self.payer = label.into();
        }
    }
}

fn splitter() -> Arc<FakePage> {
    FakePage::new(Splitter::default())
}

fn broken() -> Arc<FakePage> {
    FakePage::new(Splitter { broken: true, ..Splitter::default() })
}

/// A custom dropdown (a button that opens a list) and a radio group under a legend: both are "choose" steps.
#[derive(Default)]
struct Chooser {
    menu_open: bool,
    cat: String,
    by: String,
}

impl FakeApp for Chooser {
    fn widgets(&self) -> Vec<Widget> {
        let mut out = vec![Widget::new(0, "button", "Category")];
        if self.menu_open {
            out.push(Widget::new(1, "button", "Food"));
            out.push(Widget::new(2, "button", "Travel"));
        }
        out.push(Widget::new(3, "radio", "Alex"));
        out.push(Widget::new(4, "radio", "Sam"));
        out
    }

    fn heading(&self) -> String {
        String::new()
    }

    fn body_text(&self) -> String {
        let out = if self.cat.is_empty() && self.by.is_empty() {
            "Nothing picked".to_string()
        } else {
            format!(
                "Category: {} · Paid by {}",
                if self.cat.is_empty() { "none" } else { &self.cat },
                if self.by.is_empty() { "nobody" } else { &self.by }
            )
        };
        let menu = if self.menu_open { "Food\nTravel\n" } else { "" };
        format!("Category\n{menu}Paid by\nAlex\nSam\n{out}")
    }

    fn click(&mut self, handle: usize) {
        match handle {
            0 => self.menu_open = !self.menu_open,
            1 | 2 if self.menu_open => {
                self.cat = if handle == 1 { "Food" } else { "Travel" }.into();
                self.menu_open = false;
            }
            3 => self.by = "Alex".into(),
            4 => self.by = "Sam".into(),
            _ => {}
        }
    }
}

fn split_dinner() -> Value {
    json!([{"name": "Split a dinner", "steps": [
        {"do": "type", "target": "Name", "value": "Alex"}, {"do": "tap", "target": "Add person"},
        {"do": "type", "target": "Name", "value": "Sam"}, {"do": "tap", "target": "Add person"},
        {"do": "type", "target": "Amount", "value": "30"}, {"do": "choose", "target": "Paid by", "value": "Sam"},
        {"do": "tap", "target": "Add expense"}],
     "expect": ["Alex balance -15.00, Sam balance 15.00"]}])
}

fn journeys() -> Vec<Journey> {
    let (found, problems) = parse_journeys(&split_dinner());
    assert!(problems.is_empty(), "{problems:?}");
    found
}

fn one(raw: Value) -> Journey {
    let (found, problems) = parse_journeys(&json!([raw]));
    assert!(problems.is_empty(), "{problems:?}");
    found.into_iter().next().unwrap()
}

async fn walk_on(page: &Arc<FakePage>, journey: &Journey, verifier: &Verifier) -> anyhow::Result<JourneyResult> {
    walk(page.as_ref(), journey, verifier, WalkOptions::default()).await
}

fn run_of(results: Vec<JourneyResult>, total: usize, verifier: &Verifier) -> JourneyRun {
    JourneyRun { results, total, jev: verifier.calls.as_dict(), ..JourneyRun::default() }
}

// ---- walking the journeys --------------------------------------------------------------------------------

#[tokio::test]
async fn test_a_working_app_passes_its_journey_and_the_summary_says_so() {
    let fake = LiteralJev::new();
    let v = Verifier::new(fake.predict());
    let res = walk_on(&splitter(), &journeys()[0], &v).await.unwrap();
    assert!(res.passed(), "{}", res.as_dict());
    let run = run_of(vec![res], 1, &v);
    assert_eq!(run.passed(), 1);
    assert!(run.summary().contains("journeys 1/1"));
    assert!(!run.issues().iter().any(|i| i.starts_with("Journey")));
    assert_eq!(run.jev["calls"], json!(8));
    assert_eq!(fake.calls.load(Ordering::SeqCst), 8);
    assert_eq!(run.jev["input_tokens"], json!(7 * 500 + 300));
}

#[tokio::test]
async fn test_the_broken_add_is_caught_as_an_expectation_not_shown_with_evidence() {
    let v = literal();
    let res = walk_on(&broken(), &journeys()[0], &v).await.unwrap();
    let run = run_of(vec![res], 1, &v);
    assert_eq!(run.passed(), 0);
    let res = &run.results[0];
    assert!(res.status == "expect" && res.evidence.contains("No expenses yet"), "{}", res.as_dict());
    let issues = run.issues();
    assert!(issues[0].starts_with("Journey \"Split a dinner\"") && issues[0].contains("Alex balance -15.00"));
    assert!(run.summary().contains("journeys 0/1"));
}

#[tokio::test]
async fn test_a_step_whose_control_is_not_there_fails_at_that_step_and_says_what_is() {
    let mut raw = split_dinner()[0].clone();
    raw["steps"] = json!([{"do": "tap", "target": "Add participant"}]);
    let res = walk_on(&splitter(), &one(raw), &literal()).await.unwrap();
    assert!(res.status == "step" && res.step == 1);
    assert!(res.reason.contains("could not find \"Add participant\"") && res.reason.contains("\"Add person\""), "{}", res.reason);
}

#[tokio::test]
async fn test_jev_down_never_fails_the_app_and_the_journeys_are_not_run() {
    // the walk stops with JevDown (app_check makes it "not run"); both asks were made and metered as errors
    let down: Predict =
        Arc::new(|_: &Value, _: &Value| Err(eve_typed_ask::PredictError::new("TimeoutError", "timed out").into()));
    let v = Verifier::new(down);
    let err = walk_on(&broken(), &journeys()[0], &v).await.unwrap_err();
    let down = err.downcast_ref::<JevDown>().expect("JevDown");
    assert_eq!(down.to_string(), "timed out");
    assert_eq!(v.calls.as_dict()["errors"], json!(2));
    let run = JourneyRun { total: 1, not_run: format!("Jev did not answer: {down}"), ..JourneyRun::default() };
    assert!(run.issues().is_empty() && run.summary() == "journeys not run (Jev did not answer: timed out)");
    // an empty message reads as the class name
    let quiet: Predict = Arc::new(|_: &Value, _: &Value| Err(eve_typed_ask::PredictError::new("TimeoutError", "").into()));
    let err = walk_on(&broken(), &journeys()[0], &Verifier::new(quiet)).await.unwrap_err();
    assert_eq!(err.downcast_ref::<JevDown>().unwrap().0, "TimeoutError");
}

#[tokio::test]
async fn test_a_wrong_envelope_is_jev_down() {
    let odd: Predict = Arc::new(|_: &Value, _: &Value| Ok(json!({"answers": {"target": {}}})));
    let err = walk_on(&splitter(), &journeys()[0], &Verifier::new(odd)).await.unwrap_err();
    assert_eq!(err.downcast_ref::<JevDown>().unwrap().0, "Jev answered without the questions asked");
}

#[tokio::test]
async fn test_the_main_buttons_text_is_on_the_screen_jev_reads() {
    #[derive(Default)]
    struct Picker {
        main: Option<String>,
        cleared: bool,
    }
    impl FakeApp for Picker {
        fn widgets(&self) -> Vec<Widget> {
            vec![Widget::new(0, "button", "Pick")]
        }
        fn heading(&self) -> String {
            String::new()
        }
        fn body_text(&self) -> String {
            if self.cleared { "Pick\nCleared".into() } else { "Pick".into() }
        }
        fn main_button(&self) -> Option<String> {
            self.main.clone()
        }
        fn click(&mut self, _handle: usize) {
            self.main = Some("Clear 1 checked".into());
        }
        fn press(&mut self, name: &str) -> bool {
            if name == "MainButton" && self.main.is_some() {
                self.cleared = true;
                return true;
            }
            false
        }
    }
    let page = FakePage::new(Picker::default());
    let j = one(json!({"name": "Pick", "steps": [{"do": "tap", "target": "Pick"}], "expect": ["Clear 1 checked"]}));
    let res = walk_on(&page, &j, &literal()).await.unwrap();
    assert!(res.passed(), "{}", res.as_dict());
    // and the MainButton is a control a tap can press
    let j = one(json!({"name": "Clear", "steps": [{"do": "tap", "target": "Pick"}, {"do": "tap", "target": "Clear 1 checked"}],
                      "expect": ["Cleared"]}));
    let page = FakePage::new(Picker::default());
    let res = walk_on(&page, &j, &literal()).await.unwrap();
    assert!(res.passed(), "{}", res.as_dict());
    assert_eq!(*page.pressed.lock(), vec!["MainButton".to_string()]);
}

#[tokio::test]
async fn test_a_control_rerendered_while_jev_answers_is_found_again_not_blamed_on_the_app() {
    // a view re-rendered by a timer (a clock, a running timer) replaces the tagged node while Jev answers
    #[derive(Default)]
    struct Counter {
        n: usize,
    }
    impl FakeApp for Counter {
        fn widgets(&self) -> Vec<Widget> {
            vec![Widget::new(0, "button", "Add one")]
        }
        fn heading(&self) -> String {
            String::new()
        }
        fn body_text(&self) -> String {
            format!("Count {}\n1760000000000\nAdd one", self.n)
        }
        fn click(&mut self, _handle: usize) {
            self.n += 1;
        }
    }
    let page = FakePage::new(Counter::default());
    let fake = LiteralJev::new().predict();
    let seen = page.clone();
    let rerendering: Predict = Arc::new(move |state: &Value, questions: &Value| {
        seen.rerender();
        fake(state, questions)
    });
    let j = one(json!({"name": "Count", "steps": [{"do": "tap", "target": "Add one"}, {"do": "tap", "target": "Add one"},
                                                  {"do": "tap", "target": "Add one"}], "expect": ["Count 3"]}));
    let res = walk_on(&page, &j, &Verifier::new(rerendering)).await.unwrap();
    assert!(res.passed(), "{}", res.as_dict());
}

#[tokio::test]
async fn test_a_control_gone_for_good_is_the_apps_problem() {
    #[derive(Default)]
    struct Vanishing {
        gone: bool,
    }
    impl FakeApp for Vanishing {
        fn widgets(&self) -> Vec<Widget> {
            if self.gone { vec![] } else { vec![Widget::new(0, "button", "Go")] }
        }
        fn heading(&self) -> String {
            String::new()
        }
        fn body_text(&self) -> String {
            "Go".into()
        }
    }
    let page = FakePage::new(Vanishing::default());
    let j = one(json!({"name": "Go", "steps": [{"do": "tap", "target": "Go"}], "expect": ["Went"]}));
    let screen = read_screen(page.as_ref()).await.unwrap();
    let control = screen.controls[0].clone();
    *page.app.lock() = Box::new(Vanishing { gone: true });
    page.rerender();
    let why = act(page.as_ref(), &j.steps[0], &control, Some(&screen)).await.unwrap();
    assert_eq!(why, "\"Go\" went away when the screen changed, before it could be used.");
    // without the screen it was chosen on, the locator's own failure is the reason
    let why = act(page.as_ref(), &j.steps[0], &control, None).await.unwrap();
    assert_eq!(why, "\"Go\" could not be used (LookupError: it is no longer on the page).");
}

#[tokio::test]
async fn test_choose_on_a_custom_dropdown_picks_the_value_not_just_the_opener() {
    let j = one(json!({"name": "Pick", "steps": [{"do": "choose", "target": "Category", "value": "Travel"}],
                      "expect": ["Category: Travel"]}));
    let res = walk_on(&FakePage::new(Chooser::default()), &j, &literal()).await.unwrap();
    assert!(res.passed(), "{}", res.as_dict());
}

#[tokio::test]
async fn test_choose_in_a_radio_group_named_by_its_legend_picks_the_option() {
    let j = one(json!({"name": "Payer", "steps": [{"do": "choose", "target": "Paid by", "value": "Sam"}],
                      "expect": ["Paid by Sam"]}));
    let res = walk_on(&FakePage::new(Chooser::default()), &j, &literal()).await.unwrap();
    assert!(res.passed(), "{}", res.as_dict());
}

#[tokio::test]
async fn test_choose_fails_when_the_value_is_nowhere_after_opening_the_target() {
    let j = one(json!({"name": "Pick", "steps": [{"do": "choose", "target": "Category", "value": "Rent"}],
                      "expect": ["Category: Rent"]}));
    let res = walk_on(&FakePage::new(Chooser::default()), &j, &literal()).await.unwrap();
    assert!(res.status == "step" && res.reason.contains("could not find \"Rent\""), "{}", res.as_dict());
    assert!(res.reason.starts_with("choose \"Rent\" in \"Category\": opened \"Category\" but could not find \"Rent\" to pick"));
}

#[tokio::test]
async fn test_choose_on_a_select_without_the_option_says_what_it_has() {
    let mut raw = split_dinner()[0].clone();
    raw["steps"] = json!([{"do": "type", "target": "Name", "value": "Alex"}, {"do": "tap", "target": "Add person"},
                          {"do": "choose", "target": "Paid by", "value": "Kim"}]);
    let res = walk_on(&splitter(), &one(raw), &literal()).await.unwrap();
    assert_eq!(res.status, "step");
    assert_eq!(res.step, 3);
    assert_eq!(res.reason, "choose \"Kim\" in \"Paid by\": \"Paid by\" has no option \"Kim\" (it has: Alex).");
    assert_eq!(res.owner_line(), "Couldn't choose \"Kim\" in \"Paid by\" in \"Split a dinner\".");
}

#[tokio::test]
async fn test_one_failed_jev_request_is_asked_again_before_giving_up() {
    let fake = LiteralJev::new().predict();
    let failed = Arc::new(AtomicBool::new(false));
    let flaky: Predict = Arc::new(move |state: &Value, questions: &Value| {
        if !failed.swap(true, Ordering::SeqCst) {
            return Err(eve_typed_ask::PredictError::new("TimeoutError", "read timed out").into());
        }
        fake(state, questions)
    });
    let v = Verifier::new(flaky);
    let res = walk_on(&splitter(), &journeys()[0], &v).await.unwrap();
    assert!(res.passed(), "{}", res.as_dict());
    let jev = v.calls.as_dict();
    assert_eq!((jev["calls"].clone(), jev["errors"].clone()), (json!(9), json!(1)));
}

#[tokio::test]
async fn test_an_app_that_hangs_in_a_journey_fails_that_journey_in_seconds() {
    #[derive(Default)]
    struct Hanger {
        name: String,
    }
    impl FakeApp for Hanger {
        fn widgets(&self) -> Vec<Widget> {
            vec![Widget { value: self.name.clone(), ..Widget::new(0, "field", "Name") }]
        }
        fn heading(&self) -> String {
            String::new()
        }
        fn body_text(&self) -> String {
            "Name\nHi".into()
        }
        fn fill(&mut self, _handle: usize, value: &str) {
            self.name = value.into();
        }
        fn blur(&mut self, _handle: usize) -> Blur {
            if self.name == "spin-forever" { Blur::Hang } else { Blur::Done }
        }
    }
    let j = one(json!({"name": "Hang", "steps": [{"do": "type", "target": "Name", "value": "spin-forever"},
                                                {"do": "type", "target": "Name", "value": "b"}], "expect": ["Hi b"]}));
    let page = FakePage::new(Hanger::default());
    let t0 = Instant::now();
    let res = walk(page.as_ref(), &j, &literal(), WalkOptions { eval_timeout_s: 0.2, ..WalkOptions::default() })
        .await
        .unwrap();
    assert!(t0.elapsed().as_secs_f64() < 30.0);
    assert!(res.status == "step" && res.reason.contains("stopped responding"), "{}", res.as_dict());
    assert_eq!(res.step, 1);
    assert_eq!(
        res.reason,
        "type \"spin-forever\" into \"Name\": the app stopped responding at step 1 (the page did not answer for 3 s: \
         an endless loop, or work that never yields)."
    );
    assert_eq!(res.evidence, "(the page stopped answering)");
}

#[tokio::test]
async fn test_errors_and_steps_are_reported_to_the_caller() {
    let errors = Mutex::new(vec![
        "Uncaught error: x is not defined".to_string(),
        "The page is 40px wider".into(),
        "Console error: boom".into(),
        "Uncaught error: 2".into(),
        "Uncaught error: 3".into(),
    ]);
    let steps = Mutex::new(Vec::<String>::new());
    let on_step = |text: &str| steps.lock().push(text.to_string());
    let opts = WalkOptions { errors: Some(&errors), on_step: Some(&on_step), ..WalkOptions::default() };
    let res = walk(broken().as_ref(), &journeys()[0], &literal(), opts).await.unwrap();
    assert_eq!(res.errors, vec!["Uncaught error: x is not defined", "Console error: boom", "Uncaught error: 2"]);
    assert_eq!(steps.lock().len(), 7);
    assert_eq!(steps.lock()[0], "the journey \"Split a dinner\", step 1 (type \"Alex\" into \"Name\")");
    assert!(res.issue().contains(" While it ran: Uncaught error: x is not defined Console error: boom"));
}

#[tokio::test]
async fn test_the_verifier_records_every_request_when_asked() {
    let v = Verifier::with(LiteralJev::new().predict(), JOURNEY_STEP_GATES, EXPECT_SHOWN, true);
    let res = walk_on(&splitter(), &journeys()[0], &v).await.unwrap();
    assert!(res.passed());
    let record = v.record().unwrap();
    assert_eq!(record.len(), 8);
    assert_eq!(record[0]["state"]["step"], json!("type \"Alex\" into \"Name\""));
    assert_eq!(record[0]["state"]["controls"], json!(["Name (text field, e.g. Alex)", "Amount (number field, e.g. 30)"]));
    assert!(record[7]["questions"].get("e1").is_some());
    assert!(literal().record().is_none());
}

// ---- pure parts ------------------------------------------------------------------------------------------

#[test]
fn test_journeys_parse_strictly_and_drop_what_cannot_be_walked() {
    let raw = json!([{"name": "ok", "steps": [{"do": "tap", "target": "Add"}], "expect": ["1 item"]},
                     {"name": "no value", "steps": [{"do": "type", "target": "Name"}], "expect": ["x"]},
                     {"name": "no expect", "steps": [{"do": "tap", "target": "Add"}], "expect": []},
                     {"name": "bad kind", "steps": [{"do": "swipe", "target": "x"}], "expect": ["x"]}]);
    let (found, problems) = parse_journeys(&raw);
    assert_eq!(found.iter().map(|j| j.name.as_str()).collect::<Vec<_>>(), vec!["ok"]);
    assert_eq!(problems.len(), 3);
    assert_eq!(problems[0], "Journey \"no value\" step 1 is not a tap, type or choose with a target and a value.");
    assert_eq!(problems[1], "Journey \"no expect\" expects nothing on screen afterwards.");
    assert_eq!(problems[2], "Journey \"bad kind\" step 1 is not a tap, type or choose with a target.");
    let text = format!(
        "Here.\n```html\n<html></html>\n```\n```journeys\n{}\n```",
        serde_json::to_string(&json!([raw[0]])).unwrap()
    );
    let (got, _) = extract_journeys(&text);
    let got = got.unwrap();
    assert_eq!(got[0].steps[0], Step::tap("Add"));
    assert_eq!(extract_journeys("```html\nx\n```"), (None, vec![]));
    assert_eq!(extract_journeys("```journeys\n[oops\n```").0, Some(vec![]));
}

#[test]
fn test_a_bad_journeys_block_quotes_pythons_json_error() {
    let (got, problems) = extract_journeys("```journeys\n[not json\n```");
    assert_eq!(got, Some(vec![]));
    assert_eq!(problems, vec!["The journeys block is not valid JSON (Expecting value: line 1 column 2 (char 1))."]);
    // the last block wins, and the fence's word is read in any case
    let two = "```journeys\n[]\n```\n```JOURNEYS  \n[{\"name\": \"b\", \"steps\": [{\"do\": \"TAP\", \"target\": \"Go\"}], \"expect\": [\"Gone\"]}]```";
    let (got, problems) = extract_journeys(two);
    assert!(problems.is_empty());
    assert_eq!(got.unwrap()[0].steps[0], Step::tap("Go"));
    assert_eq!(parse_journeys(&json!({"a": 1})).1, vec!["The journeys block is not a JSON list."]);
}

#[test]
fn test_journeys_are_capped_and_their_text_is_one_short_line() {
    let j = |n: usize| json!({"name": format!("J{n}"), "steps": [{"do": "tap", "target": "A"}], "expect": ["x"]});
    let (found, problems) = parse_journeys(&json!([j(1), j(2), "nope", j(3), j(4), j(5), j(6)]));
    assert_eq!(found.len(), MAX_JOURNEYS);
    assert_eq!(problems, vec!["Journey 3 is not an object.", "Only 4 journeys are run; journey 6 and later were left out."]);
    let raw = json!([{"name": true, "steps": [{"do": "type", "target": "  Amount\n field ", "value": 30.0},
                                              {"do": "choose", "target": "Qty", "value": 2},
                                              {"do": "tap", "target": "Add", "value": "ignored"}],
                      "expect": ["a", "", 1, "b", "c", "d", "e", "f", false]}]);
    let (found, problems) = parse_journeys(&raw);
    assert!(problems.is_empty(), "{problems:?}");
    let j = &found[0];
    assert_eq!(j.name, "Journey 1");
    assert_eq!(j.steps[0], Step::new("type", "Amount field", "30.0"));
    assert_eq!(j.steps[1], Step::new("choose", "Qty", "2"));
    assert_eq!(j.steps[2], Step::tap("Add"));
    assert_eq!(j.expect, vec!["a", "1", "b", "c", "d"]);
    assert_eq!(
        journeys_json(&found)[0],
        json!({"name": "Journey 1", "steps": [{"do": "type", "target": "Amount field", "value": "30.0"},
                                              {"do": "choose", "target": "Qty", "value": "2"},
                                              {"do": "tap", "target": "Add"}],
               "expect": ["a", "1", "b", "c", "d"]})
    );
    let many: Vec<Value> = (0..20).map(|i| json!({"do": "tap", "target": format!("B{i}")})).collect();
    let (found, _) = parse_journeys(&json!([{"name": "long", "steps": many, "expect": ["x"]}]));
    assert_eq!(found[0].steps.len(), MAX_STEPS);
}

#[test]
fn test_the_fitted_gates_act_only_when_the_noul_and_the_choice_both_clear() {
    let g = JOURNEY_STEP_GATES;
    let sure = StepAnswer::new("c1", 0.99, 0.98, 0.95, 0.0, 0.0);
    assert_eq!(g.decide(&sure), "c1");
    assert_eq!(g.decide(&StepAnswer::new("c1", 0.99, 0.98, g.present - 0.01, 0.0, 0.0)), STEP_NONE); // choice alone
    assert_eq!(g.decide(&StepAnswer::new("none", 0.99, 0.98, 0.95, 0.0, 0.0)), STEP_NONE);
    const { assert!(0.0 < EXPECT_SHOWN && EXPECT_SHOWN < 1.0) };
}

#[test]
fn test_the_step_question_names_the_label_and_the_state_holds_the_controls() {
    let step = Step::new("type", "Amount", "30");
    let screen = Screen {
        controls: vec![
            Control { type_: "number".into(), placeholder: "e.g. 30".into(), ..Control::new("0", "field", "Amount") },
            Control::new("1", "button", "Add expense"),
            Control::new("main", "main", "Save"),
        ],
        heading: "H".into(),
        lines: vec!["H".into()],
    };
    let by_id = offered(&step, &screen);
    assert_eq!(by_id.values().map(|c| c.label.as_str()).collect::<Vec<_>>(), vec!["Amount"]); // a type step is offered fields only
    let options: IndexMap<String, String> = by_id.iter().map(|(k, c)| (k.clone(), c.render())).collect();
    let q = step_questions(&step, &options);
    assert!(q["target"]["instructions"].as_str().unwrap().contains("\"Amount\"") && q["present"]["type"] == "noul");
    let keys: HashSet<&String> = q["target"]["criteria"].as_object().unwrap().keys().collect();
    assert_eq!(keys, HashSet::from([&"c1".to_string(), &STEP_NONE.to_string()]));
    assert!(Control::new("main", "main", "Save").render().starts_with("Save (Telegram's main button"));
    // the wire text, byte for byte
    assert_eq!(
        q,
        json!({"target": {"type": "choice", "instructions": "Which control is labelled \"Amount\"? A leading icon, emoji or \"+\", letter case, and details after the name (a count, a date, an amount or a status, as a list row shows) do not matter. A control that only shares a word with it, or does something related, is not it.",
                          "criteria": {"c1": "Amount (number field, e.g. 30)", "none": "no control here is labelled \"Amount\""}},
               "present": {"type": "noul", "instructions": "Is a control labelled \"Amount\" in the list of controls? A leading icon, emoji or \"+\", letter case, and details after the name (a count, a date, an amount or a status, as a list row shows) do not matter. Answer no when no control carries this label, even when a similar or related control is there."}})
    );
    assert_eq!(
        step_state(&step, &screen, &options),
        json!({"about": STEP_ABOUT, "screen": "H | H", "step": "type \"30\" into \"Amount\"",
               "controls": ["Amount (number field, e.g. 30)"]})
    );
    assert_eq!(
        expect_questions(&["Total 3"]),
        json!({"e1": {"type": "noul", "instructions": "Does the screen text show: Total 3 ? Answer yes when this text is on the screen, allowing only a different letter case, spacing, line break or number format of the same amount. Answer no when a name, number or amount differs, or when it is not there."}})
    );
}

#[test]
fn test_controls_render_as_jev_reads_them() {
    let c = |kind: &str| Control::new("0", kind, "Go");
    assert_eq!(c("back").render(), "Back (Telegram's back arrow)");
    assert_eq!(c("field").render(), "Go (text field)");
    assert_eq!(Control { placeholder: "Go".into(), ..c("field") }.render(), "Go (text field)");
    assert_eq!(Control { type_: "textarea".into(), placeholder: "e.g.  a note ".into(), ..c("field") }.render(), "Go (text box, e.g. a note)");
    let opts: Vec<String> = (1..=9).map(|i| format!("o{i}")).collect();
    assert_eq!(Control { options: opts, ..c("select") }.render(), "Go (dropdown: o1, o2, o3, o4, o5, o6, o7, o8…)");
    assert_eq!(c("item").render(), "Go (list item)");
    assert_eq!(Control { on: true, ..c("radio") }.render(), "Go (option, selected)");
    assert_eq!(c("tab").render(), "Go (tab)");
    assert_eq!(c("checkbox").render(), "Go (checkbox)");
    assert_eq!(c("link").render(), "Go (link)");
    assert_eq!(c("whatever").render(), "Go (button)");
}

#[test]
fn test_a_busy_screen_offers_at_most_max_controls_with_telegrams_buttons_kept() {
    let mut controls: Vec<Control> = (0..60).map(|i| Control::new(i.to_string(), "button", format!("B{i}"))).collect();
    controls.push(Control::new("0", "field", "F"));
    controls.push(Control::new("main", "main", "Save"));
    controls.push(Control::new("back", "back", "Back"));
    let screen = Screen { controls, ..Screen::default() };
    let by_id = offered(&Step::tap("x"), &screen);
    assert_eq!(by_id.len(), MAX_CONTROLS);
    assert_eq!(by_id["c46"].label, "B45");
    assert_eq!(by_id["c47"].kind, "main");
    assert_eq!(by_id["c48"].kind, "back");
    assert!(offered(&Step::new("swipe", "x", ""), &screen).is_empty());
}

#[test]
fn test_a_step_answer_is_read_as_the_click_paths_step_answer() {
    let options: IndexMap<String, String> = [("c1".to_string(), "A".to_string()), ("c2".into(), "B".into())].into();
    let answers = json!({"target": {"choice": "c1", "probabilities": {"c1": 0.7, "c2": 0.2, "none": 0.1}},
                         "present": {"noul": 0.9}});
    let a = read_step(answers.as_object().unwrap(), &options, 12.0);
    assert_eq!((a.target.as_str(), a.k, a.ms, a.present), ("c1", 3, 12.0, 0.9));
    assert!((a.p_target - 0.7).abs() < 1e-12 && (a.margin - 0.5).abs() < 1e-12);
    let odd = json!({"target": {"choice": "c9", "probabilities": {"c9": 1.0}}, "present": {"noul": 0.9}});
    let a = read_step(odd.as_object().unwrap(), &options, 5.0);
    assert_eq!((a.target.as_str(), a.error.as_str(), a.k), ("", "target 'c9' is not an option", 3));
    assert_eq!(JOURNEY_STEP_GATES.decide(&a), STEP_NONE);
}

#[test]
fn test_the_screen_text_is_short_distinct_lines() {
    let text = format!("A\n\n  A  \nB   c\n{}", (0..400).map(|i| format!("n{i}")).collect::<Vec<_>>().join("\n"));
    let lines = screen_lines(&text);
    assert_eq!(&lines[..2], &["A", "B c"]);
    assert!(lines.len() <= MAX_LINES);
    // the characters read off the page are bounded too
    let long = (0..100).map(|i| format!("{i}{}", "x".repeat(200))).collect::<Vec<_>>().join("\n");
    let lines = screen_lines(&long);
    assert_eq!(lines.len(), 42);
    assert!(lines.iter().all(|l| l.chars().count() == MAX_LINE_CHARS));
}

#[test]
fn test_a_screen_is_read_from_what_the_script_returns() {
    let raw = json!({"controls": [{"id": "0", "kind": "", "label": "Go", "options": "ab", "on": 1},
                                  {"kind": "select", "label": "S", "options": ["x", 2], "value": null}, "junk"],
                     "heading": "Title", "main": "Save", "back": true, "text": "Title\nline"});
    let s = screen_from(&raw);
    assert_eq!(s.controls.len(), 4);
    assert_eq!(s.controls[0], Control { options: vec!["a".into(), "b".into()], on: true, ..Control::new("0", "button", "Go") });
    assert_eq!(s.controls[1], Control { options: vec!["x".into(), "2".into()], ..Control::new("None", "select", "S") });
    assert_eq!(s.controls[2], Control::new("main", "main", "Save"));
    assert_eq!(s.controls[3], Control::new("back", "back", "Back"));
    assert_eq!(s.summary(), "Title | Title | line");
    assert_eq!(screen_from(&json!({"text": null})), Screen::default());
    let long = Screen { heading: "h".repeat(400), ..Screen::default() };
    assert_eq!(long.summary().chars().count(), SUMMARY_CHARS);
}

#[test]
fn test_from_env_needs_a_key_and_can_be_turned_off() {
    let env = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    };
    let (v, why) = from_env(&env(&[("EVE_JEV_ROUTE", "openrouter")]));
    assert!(v.is_none() && why.starts_with("Jev is not configured: "), "{why}");
    assert!(from_env(&env(&[("EVE_APP_JOURNEYS", "0"), ("OPENROUTER_API_KEY", "k")])).1.contains("turned off"));
    assert!(from_env(&env(&[("EVE_APP_JOURNEYS", " Off "), ("EVE_JEV_ROUTE", "openrouter"), ("OPENROUTER_API_KEY", "k")])).0.is_none());
    let (v, why) = from_env(&env(&[("EVE_JEV_ROUTE", "openrouter"), ("OPENROUTER_API_KEY", "k")]));
    assert!(v.is_some() && why.is_empty());
    let (v, _) = from_env(&env(&[("EVE_APP_JOURNEYS", ""), ("EVE_JEV_ROUTE", "openrouter"), ("OPENROUTER_API_KEY", "k"),
                                 ("EVE_JEV_TIMEOUT", "2")]));
    assert!(v.is_some());
}

#[test]
fn test_jev_reads_a_shortlist_of_the_screen_with_the_near_misses_kept() {
    // a month calendar is dozens of lines of single numbers: on the whole screen Jev missed text that was there
    let mut lines: Vec<String> = vec!["Mood Journal".into()];
    lines.extend((1..31).map(|d| d.to_string()));
    lines.extend(["average mood · Good", "Alex owes Sam", "$15.00", "Settings"].map(String::from));
    let got = relevant_lines(&lines, &["Alex owes Sam $25.00", "average mood · Good"], MAX_EXPECT_LINES);
    let has = |s: &str| got.iter().any(|l| l == s);
    assert!(has("average mood · Good") && has("$15.00") && has("Alex owes Sam")); // the near miss stays in
    assert!(got.len() <= MAX_EXPECT_LINES && has("Settings")); // a neighbour line too
    assert_eq!(relevant_lines(&["a", "b"], &["zzz"], MAX_EXPECT_LINES), vec!["a", "b"]); // nothing shared: the start
    assert_eq!(
        expect_state(&["x", "Total 3"], &["Total 3"]),
        json!({"about": EXPECT_ABOUT, "screen": ["x", "Total 3"]})
    );
}

#[test]
fn test_a_number_jev_did_not_really_see_vetoes_shown() {
    assert!(numbers_shown("Sam owes $30", &["Sam owes", "$30.00"])); // the same amount, two ways
    assert!(numbers_shown("Total $1,234.50", &["Total $1234.5"]));
    assert!(!numbers_shown("1 book", &["Add book", "Braiding Sweetgrass"])); // Jev read 0.90 here
    assert!(!numbers_shown("Sam owes $25.00", &["Sam owes $15.00"]));
    assert!(numbers_shown("Saved", &["Saved"]));
    assert!(numbers_shown("09 items, 0.0 left", &["9 items", "0 left"]));
}

#[test]
fn test_an_empty_or_unusable_journeys_block_is_a_problem_not_silence() {
    assert_eq!(parse_journeys(&json!([])).1, vec!["The journeys block holds no journey that can be walked; write 2 to 4."]);
    let (got, problems) = extract_journeys("```journeys\n[]\n```");
    assert!(got == Some(vec![]) && !problems.is_empty());
    let only_bad = json!([{"name": "x", "steps": [], "expect": ["y"]}]);
    assert_eq!(parse_journeys(&only_bad).1.len(), 2);
}

#[test]
fn test_the_owner_reads_a_short_line_and_the_repair_round_the_details() {
    let step = JourneyResult {
        steps: 6,
        step: 3,
        reason: "tap \"Add expense\": could not find \"Add expense\" to tap (the controls on screen: \"Add person\", \"Name\").".into(),
        evidence: "\"Expense Splitter | People\"".into(),
        ..JourneyResult::new("Split a dinner", "step")
    };
    assert_eq!(step.owner_line(), "Couldn't tap \"Add expense\" in \"Split a dinner\".");
    assert!(step.issue().contains("the controls on screen"));
    assert_eq!(
        step.issue(),
        "Journey \"Split a dinner\" failed at step 3 of 6: tap \"Add expense\": could not find \"Add expense\" to tap \
         (the controls on screen: \"Add person\", \"Name\"). On screen: \"Expense Splitter | People\" Fix the app if it \
         lacks this or it does not work; fix the journey only if the app is right and the journey names it wrongly."
    );
    let exp = JourneyResult {
        steps: 6,
        expects: vec![ExpectResult { text: "Alex owes Sam $15.00".into(), score: 0.1, shown: false }],
        ..JourneyResult::new("Split a dinner", "expect")
    };
    assert_eq!(exp.owner_line(), "After \"Split a dinner\", the screen didn't show \"Alex owes Sam $15.00\".");
    assert_eq!(JourneyResult::new("X", "step").owner_line(), "\"X\" did not go through.");
    let run = JourneyRun { results: vec![step.clone(), exp.clone()], total: 3, not_run: "Jev did not answer: x".into(), ..Default::default() };
    assert_eq!(run.summary(), "journeys 0/3 (1 not run: Jev did not answer: x)");
    assert_eq!(run.owner_lines().get(&exp.issue()).unwrap(), &exp.owner_line());
    assert_eq!(JourneyRun::default().summary(), "");
    assert_eq!(
        run.as_dict(),
        json!({"total": 3, "passed": 0, "ran": 2, "not_run": "Jev did not answer: x", "secs": 0.0, "jev": {},
               "results": [step.as_dict(), exp.as_dict()]})
    );
    assert_eq!(exp.as_dict()["expects"], json!([{"text": "Alex owes Sam $15.00", "score": 0.1, "shown": false}]));
}

#[test]
fn test_words_there_but_not_in_the_expectations_order_veto_shown() {
    // a live build: Jev read 0.56 to 0.62 for "1-day streak" over this line, and the cut was 0.4
    assert!(!words_shown("1-day streak", &["Drink water", "Streak ended · best 1 day"]));
    assert!(!words_shown("Alex owes Sam $15.00", &["Sam owes Alex $15.00"]));
    assert!(words_shown("1-day streak", &["Drink water", "🔥 1-DAY streak"])); // case, emoji, hyphen
    assert!(words_shown("Alex owes Sam $15.00", &["Alex owes Sam", "$15"])); // a line break, a format
    assert!(words_shown("Total $1,234.50", &["Total: $1234.5"]));
    assert!(words_shown("✓", &["anything"])); // no words to hold to
    assert!(!words_shown("a b c", &["a b"]));
    assert!(words_shown("STRASSE", &["Straße"]));
}

#[test]
fn test_refind_keeps_the_place_among_controls_that_share_a_label() {
    let row = |id: &str| Control::new(id, "button", "Delete");
    let before = Screen { controls: vec![row("0"), Control::new("1", "button", "Add"), row("2")], ..Screen::default() };
    let now = Screen { controls: vec![row("5"), row("6")], ..Screen::default() };
    assert_eq!(refind(&row("2"), &before, &now), Some(row("6")));
    let one_left = Screen { controls: vec![row("9")], ..Screen::default() };
    assert_eq!(refind(&row("2"), &before, &one_left), Some(row("9")));
    assert_eq!(refind(&row("2"), &before, &Screen::default()), None);
}

#[test]
fn test_a_choose_value_finds_its_option() {
    let c = Control { options: vec!["Alex".into(), "Sam Smith".into(), "Samantha".into(), "$5.00 fee".into()], ..Control::new("0", "select", "S") };
    assert_eq!(option_for(&c, " alex "), Some("Alex".into()));
    assert_eq!(option_for(&c, "smith"), Some("Sam Smith".into()));
    assert_eq!(option_for(&c, "Sam"), None); // two options hold it
    assert_eq!(option_for(&c, "$5.00!"), Some("$5.00 fee".into()));
    assert_eq!(option_for(&c, "!!"), None);
}

#[test]
fn test_the_bill_has_p50_and_p90() {
    let calls = JevCalls::default();
    assert_eq!(
        Value::Object(calls.as_dict()),
        json!({"calls": 0, "input_tokens": 0, "ms": 0, "errors": 0, "usd": 0.0, "p50_ms": 0, "p90_ms": 0})
    );
    for (i, ms) in [100.0, 300.0, 200.0, 400.0].into_iter().enumerate() {
        calls.add(&json!({"usage": {"input_tokens": 1000}}), ms, i == 3);
    }
    let d = calls.as_dict();
    assert_eq!((d["calls"].clone(), d["errors"].clone(), d["ms"].clone()), (json!(4), json!(1), json!(1000)));
    assert_eq!((d["p50_ms"].clone(), d["p90_ms"].clone()), (json!(250), json!(400)));
    assert_eq!(d["usd"], json!(py::round_to(eve_pricing::estimate_jev_cost(4000), 6)));
}

#[test]
fn test_the_page_scripts_are_pythons() {
    let py = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src/eve_bridge/jev_verify.py"));
    let script = |name: &str| -> String {
        let start = py.find(&format!("{name} = r\"\"\"")).unwrap() + name.len() + 7;
        let end = start + py[start..].find("\"\"\"").unwrap();
        py[start..end].to_string()
    };
    assert_eq!(script("COLLECT_JS"), COLLECT_JS);
    assert_eq!(script("TEXT_JS"), TEXT_JS);
    assert_eq!(script("PRESS_JS"), PRESS_JS);
}

// ---- the Python module's own answers ---------------------------------------------------------------------

/// tests/fixtures/python_cases.json: random text (letters, digits of other scripts, superscripts, ß, combining
/// marks, emoji, every Python line break) and random journeys blocks, with what jev_verify.py answered for each
/// under the daemon's Python 3.13 (tests/fixtures/gen_cases.py writes it).
#[test]
fn test_every_text_rule_answers_as_the_python_module_does() {
    let raw = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/python_cases.json")).unwrap();
    let cases: Value = serde_json::from_str(&raw).unwrap();
    let strs = |v: &Value| -> Vec<String> { v.as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect() };
    let sorted = |set: HashSet<String>| {
        let mut v: Vec<String> = set.into_iter().collect();
        v.sort();
        v
    };
    for c in cases["tokens"].as_array().unwrap() {
        let t = c["text"].as_str().unwrap();
        assert_eq!(tokens(t), strs(&c["tokens"]), "tokens of {t:?}");
        // Python sorts by code point; so does Rust's String order
        assert_eq!(sorted(numbers_of(t)), strs(&c["numbers"]), "numbers of {t:?}");
        assert_eq!(sorted(words_of(t)), strs(&c["words"]), "words of {t:?}");
    }
    for c in cases["shown"].as_array().unwrap() {
        let (e, lines) = (c["expect"].as_str().unwrap(), strs(&c["lines"]));
        assert_eq!(numbers_shown(e, &lines), c["numbers"].as_bool().unwrap(), "numbers_shown {e:?} {lines:?}");
        assert_eq!(words_shown(e, &lines), c["words"].as_bool().unwrap(), "words_shown {e:?} {lines:?}");
    }
    for c in cases["relevant"].as_array().unwrap() {
        let (lines, expects) = (strs(&c["lines"]), strs(&c["expects"]));
        let limit = c["limit"].as_u64().unwrap() as usize;
        assert_eq!(relevant_lines(&lines, &expects, limit), strs(&c["got"]), "relevant_lines {lines:?} {expects:?}");
    }
    for c in cases["lines"].as_array().unwrap() {
        assert_eq!(screen_lines(c["text"].as_str().unwrap()), strs(&c["lines"]));
    }
    for c in cases["option"].as_array().unwrap() {
        let control = Control { options: strs(&c["options"]), ..Control::new("0", "select", "S") };
        let v = c["value"].as_str().unwrap();
        assert_eq!(option_for(&control, v), c["got"].as_str().map(String::from), "option_for {v:?} in {:?}", control.options);
    }
    for c in cases["parse"].as_array().unwrap() {
        let (found, problems) = parse_journeys(&c["raw"]);
        assert_eq!(journeys_json(&found), c["found"], "parse_journeys {}", c["raw"]);
        assert_eq!(problems, strs(&c["problems"]), "parse_journeys {}", c["raw"]);
    }
    for c in cases["extract"].as_array().unwrap() {
        let (found, problems) = extract_journeys(c["text"].as_str().unwrap());
        assert_eq!(found.map(|f| journeys_json(&f)).unwrap_or(Value::Null), c["found"], "extract {}", c["text"]);
        assert_eq!(problems, strs(&c["problems"]), "extract {}", c["text"]);
    }
    for c in cases["render"].as_array().unwrap() {
        let control = Control {
            type_: c["type"].as_str().unwrap().into(),
            placeholder: c["placeholder"].as_str().unwrap().into(),
            options: strs(&c["options"]),
            on: c["on"].as_bool().unwrap(),
            ..Control::new("1", c["kind"].as_str().unwrap(), c["label"].as_str().unwrap())
        };
        assert_eq!(control.render(), c["got"].as_str().unwrap());
    }
}

#[test]
fn test_a_walk_can_run_on_its_own_task() {
    // app_check walks every journey side by side, each on a spawned task
    fn send<T: Send>(_: &T) {}
    let page = splitter();
    let v = literal();
    let j = journeys().remove(0);
    let errors = Mutex::new(Vec::new());
    let fut = walk(page.as_ref(), &j, &v, WalkOptions { errors: Some(&errors), ..WalkOptions::default() });
    send(&fut);
}
