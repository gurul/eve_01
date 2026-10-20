//! Unit `apps_maker`: apps_maker.py 1130-1956 (apps_maker/maker); tests: test_apps_maker.py the maker and the real generate (328-586), the chat door (642-772), journeys and the change chat (1052-1258), review round 3 (1350-1525).
//!
//! The shared fakes are in `support` (architect-owned); this unit's own go here: the scripted Claude (`Script`),
//! the faked phone check (`FakeCheck` and its journey, walked and failing kinds), the Mini App the chat door
//! drives (`FakeMiniApp`) and the chat it sends to (`Chat`).

mod support;

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::FutureExt;
use parking_lot::Mutex;
use serde_json::{json, Value};

use eve_app_check::{CheckOptions, CheckReport};
use eve_brain::apps_maker::*;
use eve_brain::prelude::{
    HttpError, HttpRequest, HttpResponse, HttpTransport, MakerHost, SayFn, SendButtons, SendPhoto, SpawnFn,
};
use eve_jev_verify::{ExpectResult, Journey, JourneyResult, JourneyRun};

type Obj = serde_json::Map<String, Value>;

const DOC: &str = "<!doctype html><html><head><title>Habit Tracker</title><meta name=\"eve-icon\" content=\"✅\">\
<meta content=\"Check in daily and keep streaks.\" name=\"description\"></head><body>hi<script>\
eve.load().then(render); function add() { eve.save(state); }</script></body></html>";

fn usage() -> Value {
    json!({"input_tokens": 2000, "output_tokens": 10000})
}

fn doc(marker: &str) -> String {
    DOC.replace("hi", marker)
}

fn doc_titled(marker: &str, title: &str) -> String {
    DOC.replace("hi", marker).replace("Habit Tracker", title)
}

fn fenced(html: &str) -> String {
    format!("Here it is.\n```html\n{html}\n```")
}

/// miniapp.cost_usd for claude-opus-5-5 (miniapp.PRICES), the bill the tests read.
fn cost(u: &Value) -> f64 {
    let n = |k: &str| u.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let hour = u.get("cache_creation").and_then(|c| c.get("ephemeral_1h_input_tokens")).and_then(Value::as_f64).unwrap_or(0.0);
    let writes = n("cache_creation_input_tokens");
    (n("input_tokens") * 4.0 + n("output_tokens") * 20.0 + n("cache_read_input_tokens") * 0.2
        + (writes - hour).max(0.0) * 5.0
        + hour * 8.0)
        / 1_000_000.0
}

fn one() -> f64 {
    cost(&usage())
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6 * b.abs().max(1.0)
}

// ---- a fake Claude ------------------------------------------------------------------------------------------------

enum Answer {
    Text(String, &'static str),
    Fail(fn() -> anyhow::Error),
}

fn text(t: impl Into<String>) -> Answer {
    Answer::Text(t.into(), "end_turn")
}

fn stopped(t: impl Into<String>, stop: &'static str) -> Answer {
    Answer::Text(t.into(), stop)
}

fn rate_limited() -> anyhow::Error {
    eve_openrouter::Error::Status {
        status: 429,
        body: "slow down".into(),
        headers: Box::new(reqwest::header::HeaderMap::new()),
    }
    .into()
}

fn connection_error() -> anyhow::Error {
    anyhow::anyhow!("ConnectionError: down")
}

fn overloaded() -> anyhow::Error {
    anyhow::anyhow!("RuntimeError: overloaded")
}

/// A fake Claude: answers from a list, one per call (the last one again and again), and records every call's
/// messages.
struct Script {
    answers: Mutex<VecDeque<Answer>>,
    calls: Mutex<Vec<Vec<Value>>>,
}

impl Script {
    fn new(answers: Vec<Answer>) -> Arc<Self> {
        Arc::new(Self { answers: Mutex::new(answers.into()), calls: Mutex::new(Vec::new()) })
    }

    fn set(&self, answers: Vec<Answer>) {
        *self.answers.lock() = answers.into();
    }

    fn calls(&self) -> Vec<Vec<Value>> {
        self.calls.lock().clone()
    }

    /// `calls[i][j]["content"]` as text.
    fn content(&self, i: usize, j: usize) -> String {
        self.calls.lock()[i][j]["content"].as_str().unwrap_or("").to_string()
    }

    fn generate(self: &Arc<Self>) -> Generate {
        let me = self.clone();
        Arc::new(move |messages: Vec<Value>, progress: MakerProgress| {
            let n = {
                let mut calls = me.calls.lock();
                calls.push(messages);
                calls.len()
            };
            let next = {
                let mut answers = me.answers.lock();
                match answers.len() {
                    0 => None,
                    1 => answers.front().map(|a| match a {
                        Answer::Text(t, s) => Answer::Text(t.clone(), s),
                        Answer::Fail(f) => Answer::Fail(*f),
                    }),
                    _ => answers.pop_front(),
                }
            };
            async move {
                match next.expect("a model call nobody scripted") {
                    Answer::Fail(f) => Err(f()),
                    Answer::Text(text, stop) => {
                        progress("thinking", 0);
                        progress("writing", text.chars().count() as i64);
                        Ok(Generated {
                            content: json!([{"type": "text", "text": text, "n": n}]),
                            text,
                            usage: usage(),
                            stop_reason: stop.to_string(),
                        })
                    }
                }
            }
            .boxed()
        })
    }
}

// ---- a fake phone check -------------------------------------------------------------------------------------------

#[derive(Clone)]
enum Kind {
    /// the issues for a page are keyed by a marker in it
    Plain(Vec<(String, Vec<String>)>),
    /// fails journey "Add one" on a page with this marker
    Journey(String),
    /// Jev walked `passed` of `total`, or did not run (`why`)
    Walked(usize, usize, String),
    /// a journey that shows the wrong screen
    Failing,
}

struct FakeCheck {
    kind: Kind,
    seeds: Mutex<Vec<Option<Value>>>,
    given: Mutex<Vec<Option<Vec<Journey>>>>,
}

impl FakeCheck {
    fn new(kind: Kind) -> Arc<Self> {
        Arc::new(Self { kind, seeds: Mutex::new(Vec::new()), given: Mutex::new(Vec::new()) })
    }

    fn plain(issues: &[(&str, &[&str])]) -> Arc<Self> {
        Self::new(Kind::Plain(
            issues.iter().map(|(k, v)| (k.to_string(), v.iter().map(|s| s.to_string()).collect())).collect(),
        ))
    }

    fn journeys(fail: &str) -> Arc<Self> {
        Self::new(Kind::Journey(fail.to_string()))
    }

    fn walked(passed: usize, total: usize, why: &str) -> Arc<Self> {
        Self::new(Kind::Walked(passed, total, why.to_string()))
    }

    fn given_names(&self, i: usize) -> Vec<String> {
        self.given.lock()[i].as_ref().map(|js| js.iter().map(|j| j.name.clone()).collect()).unwrap_or_default()
    }

    fn report(&self, html: &str, opts: &CheckOptions) -> CheckReport {
        let shot = Some(b"\xff\xd8jpeg".to_vec());
        match &self.kind {
            Kind::Plain(issues) => {
                let found = issues.iter().find(|(k, _)| html.contains(k.as_str())).map(|(_, v)| v.clone()).unwrap_or_default();
                CheckReport { ok: found.is_empty(), issues: found, taps: 3, fields: 1, saves: 2, screenshot: shot, ..Default::default() }
            }
            Kind::Journey(fail) => {
                let bad = html.contains(fail.as_str())
                    && opts.journeys.as_ref().and_then(|j| j.first()).is_some_and(|j| j.expect == vec!["1 habit".to_string()]);
                let issues: Vec<String> = if bad {
                    vec!["Journey \"Add one\": after all 1 steps the screen does not show \"1 habit\".".into()]
                } else {
                    Vec::new()
                };
                CheckReport { ok: issues.is_empty(), issues, taps: 3, fields: 1, saves: 2, screenshot: shot, ..Default::default() }
            }
            Kind::Walked(passed, total, why) => {
                let results = if why.is_empty() {
                    (0..*total)
                        .map(|i| {
                            let mut r = JourneyResult::new(format!("j{i}"), if i < *passed { "passed" } else { "step" });
                            r.steps = 2;
                            r
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                let run = JourneyRun { results, total: *total, not_run: why.clone(), ..Default::default() };
                let issues = run.issues();
                CheckReport { ok: issues.is_empty(), issues, taps: 3, fields: 1, saves: 2, journeys: Some(run), ..Default::default() }
            }
            Kind::Failing => {
                let mut r = JourneyResult::new("Add one", "expect");
                r.steps = 1;
                r.expects = vec![ExpectResult { text: "1 habit".into(), score: 0.1, shown: false }];
                r.evidence = format!("\"{}\"", "No habits yet ".repeat(40));
                let run = JourneyRun { results: vec![r], total: 1, ..Default::default() };
                let issues = run.issues();
                CheckReport { ok: false, issues, journeys: Some(run), screenshot: shot, ..Default::default() }
            }
        }
    }

    fn check(self: &Arc<Self>) -> Check {
        let me = self.clone();
        Arc::new(move |html: String, opts: CheckOptions| {
            me.seeds.lock().push(opts.seed.clone());
            me.given.lock().push(opts.journeys.clone());
            let report = me.report(&html, &opts);
            async move { report }.boxed()
        })
    }
}

// ---- the maker, its store and its bill ----------------------------------------------------------------------------

struct Rig {
    maker: Arc<AppMaker>,
    store: Arc<AppStore>,
    spent: Arc<Mutex<f64>>,
}

fn maker_with(store: Arc<AppStore>, generate: Generate, check: Option<Check>, max_repairs: i64, spent: Arc<Mutex<f64>>) -> Arc<AppMaker> {
    let s = spent.clone();
    AppMaker::new(
        store,
        generate,
        Arc::new(cost),
        Arc::new(move |usd: f64| {
            let mut t = s.lock();
            *t += usd;
            *t
        }),
        Some(Arc::new(|| "Today is Thursday.".to_string())),
        check,
        max_repairs,
    )
}

fn maker_for(dir: &Path, generate: Generate, check: Option<Arc<FakeCheck>>, max_repairs: i64) -> Rig {
    let store = Arc::new(AppStore::new(&dir.join("apps")));
    let check = check.unwrap_or_else(|| FakeCheck::plain(&[]));
    let spent = Arc::new(Mutex::new(0.0));
    let maker = maker_with(store.clone(), generate, Some(check.check()), max_repairs, spent.clone());
    Rig { maker, store, spent }
}

type Seen = Arc<Mutex<Vec<(String, i64)>>>;

fn stages() -> (Seen, MakerProgress) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    (seen, Arc::new(move |stage: &str, n: i64| s.lock().push((stage.to_string(), n))))
}

fn names(journeys: &Option<Vec<Journey>>) -> Vec<String> {
    journeys.as_ref().map(|js| js.iter().map(|j| j.name.clone()).collect()).unwrap_or_default()
}

// ---- the maker ----------------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_making_an_app_saves_it_bills_it_and_reports_each_stage() {
    let dir = support::tmp();
    let gen = Script::new(vec![text(fenced(DOC))]);
    let rig = maker_for(dir.path(), gen.generate(), None, 2);
    let (seen, progress) = stages();
    let made = rig.maker.make("a habit tracker with streaks", Some(progress)).await;
    let app = made.app.clone().unwrap();
    assert!(made.ok && app.slug == "habit-tracker" && rig.store.html("habit-tracker").as_deref() == Some(DOC));
    let first = gen.content(0, 0);
    assert!(first.contains("habit tracker with streaks") && first.contains("<owner_context>\nToday is Thursday."));
    assert!(approx(*rig.spent.lock(), one()) && approx(made.usd, one()));
    assert!(made.rounds == 0 && made.check.as_ref().unwrap().ok && made.issues.is_empty() && app.icon == "✅");
    let got: Vec<String> = seen.lock().iter().map(|(s, _)| s.clone()).collect();
    assert_eq!(got, ["thinking", "writing", "testing", "saving"]);
}

#[tokio::test]
async fn test_a_problem_found_on_the_phone_goes_back_for_one_repair() {
    let dir = support::tmp();
    let gen = Script::new(vec![text(fenced(&doc("v1"))), text(fenced(&doc("v2")))]);
    let check = FakeCheck::plain(&[("v1", &["Uncaught error while tapping \"Add\": x is not defined (line 12)"])]);
    let rig = maker_for(dir.path(), gen.generate(), Some(check), 2);
    let (seen, progress) = stages();
    let made = rig.maker.make("habit tracker", Some(progress)).await;
    let slug = made.app.clone().unwrap().slug;
    assert!(made.ok && made.rounds == 1 && rig.store.html(&slug).unwrap().contains("v2") && made.check.as_ref().unwrap().ok);
    let calls = gen.calls();
    let second = &calls[1];
    let roles: Vec<&str> = second.iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["user", "assistant", "user"]);
    assert_eq!(second[0], calls[0][0]); // the first turn, untouched
    assert_eq!(second[1]["content"][0]["n"], 1); // Claude's own content, unchanged
    let repair = second[2]["content"].as_str().unwrap();
    assert!(repair.contains("tapping \"Add\"") && repair.contains("whole fixed file"));
    assert!(seen.lock().contains(&("fixing".to_string(), 1)));
    assert!(approx(*rig.spent.lock(), 2.0 * one()) && approx(made.usd, 2.0 * one()));
    assert_eq!(rig.store.info(&slug).unwrap().versions, 0); // the broken draft is not a version
}

#[tokio::test]
async fn test_a_repair_answered_with_edits_changes_only_those_lines() {
    let dir = support::tmp();
    let bad = doc("v1");
    let edit = "Fixed.\n<<<<<<< FIND\nv1<script>\n=======\nv2<script>\n>>>>>>> REPLACE\n";
    let gen = Script::new(vec![text(fenced(&bad)), text(edit)]);
    let check = FakeCheck::plain(&[("v1", &["Uncaught error while tapping \"Add\""])]);
    let rig = maker_for(dir.path(), gen.generate(), Some(check), 2);
    let made = rig.maker.make("habit tracker", None).await;
    let slug = made.app.clone().unwrap().slug;
    assert!(made.ok && made.rounds == 1);
    assert_eq!(rig.store.html(&slug).unwrap(), bad.replace("v1<script>", "v2<script>"));
    assert!(gen.content(1, 2).contains("<<<<<<< FIND") && gen.content(1, 2).contains("whole fixed file"));
    assert_eq!(made.history, vec![vec!["Uncaught error while tapping \"Add\"".to_string()], vec![]]);
}

#[tokio::test]
async fn test_an_edit_that_does_not_fit_the_file_is_sent_back_with_the_reason() {
    let dir = support::tmp();
    let gen = Script::new(vec![
        text(fenced(&doc("v1"))),
        text("<<<<<<< FIND\nnot in the file\n=======\nx\n>>>>>>> REPLACE"),
        text(fenced(&doc("v3"))),
    ]);
    let rig = maker_for(dir.path(), gen.generate(), Some(FakeCheck::plain(&[("v1", &["broken"])])), 2);
    let made = rig.maker.make("habit tracker", None).await;
    assert!(made.ok && rig.store.html(&made.app.unwrap().slug).unwrap().contains("v3") && gen.calls().len() == 3);
    assert!(gen.content(2, 4).contains("Edit 1 of 1 could not be applied"));
    assert!(!gen.content(2, 4).contains("tapped every button")); // nothing new was tested
}

#[test]
fn test_edits_apply_to_the_file() {
    let base = "<!doctype html><html><body>\n  <b>a</b>\n  <i>b</i>\n</body></html>";
    for (answer, expect) in [
        ("<<<<<<< FIND\n<b>a</b>\n=======\n<b>z</b>\n>>>>>>> REPLACE", "<b>z</b>"),
        // the same lines with other indentation still fit, when they fit one place only
        ("<<<<<<< FIND\n<b>a</b>\n    <i>b</i>\n=======\n<i>y</i>\n>>>>>>> REPLACE", "<i>y</i>"),
        // an empty replacement deletes; two blocks apply in order
        (
            "<<<<<<< FIND\n<i>b</i>\n=======\n>>>>>>> REPLACE\n<<<<<<< FIND\n<b>a</b>\n=======\n<p>c</p>\n>>>>>>> REPLACE",
            "<p>c</p>",
        ),
    ] {
        let (out, why) = apply_edits(base, answer);
        assert_eq!(why, "", "{answer}");
        let out = out.unwrap();
        assert!(out.contains(expect) && out.ends_with("</html>"), "{out}");
    }
}

#[test]
fn test_edits_that_cannot_apply_say_why() {
    for (answer, why) in [
        ("<<<<<<< FIND\n<u>x</u>\n=======\ny\n>>>>>>> REPLACE", "occur 0 times"),
        ("<<<<<<< FIND\n<s>d</s>\n=======\ny\n>>>>>>> REPLACE", "occur 2 times"),
        ("<<<<<<< FIND\n</html>\n=======\n\n>>>>>>> REPLACE", "no longer one complete HTML document"),
    ] {
        let (out, reason) = apply_edits("<!doctype html><html><body><s>d</s><s>d</s>\n</html>", answer);
        assert!(out.is_none() && reason.contains(why), "{reason}");
    }
}

#[test]
fn test_an_answer_without_edits_is_not_an_edit() {
    assert_eq!(apply_edits("<html></html>", &fenced(DOC)), (None, String::new()));
}

#[tokio::test]
async fn test_repairs_that_do_not_converge_keep_the_best_version() {
    let dir = support::tmp();
    let gen = Script::new(vec![text(fenced(&doc("v1"))), text(fenced(&doc("v2"))), text(fenced(&doc("v3")))]);
    let check = FakeCheck::plain(&[("v1", &["a", "b", "c"]), ("v2", &["only b"]), ("v3", &["b", "d"])]);
    let rig = maker_for(dir.path(), gen.generate(), Some(check), 2);
    let made = rig.maker.make("habit tracker", None).await;
    assert!(made.ok && made.rounds == 2 && gen.calls().len() == 3);
    assert!(rig.store.html(&made.app.clone().unwrap().slug).unwrap().contains("v2"));
    assert_eq!(made.issues, ["only b"]);
    assert_eq!(made.check.unwrap().issues, ["only b"]);
    assert!(approx(*rig.spent.lock(), 3.0 * one()));
}

#[tokio::test]
async fn test_a_static_problem_is_repaired_even_when_the_phone_check_passes() {
    let dir = support::tmp();
    let never_saves = DOC.replace("eve.save(state)", "render()");
    let gen = Script::new(vec![text(fenced(&never_saves)), text(fenced(DOC))]);
    let rig = maker_for(dir.path(), gen.generate(), None, 2);
    let made = rig.maker.make("habit tracker", None).await;
    assert!(made.ok && made.rounds == 1 && gen.content(1, 2).contains("eve.save()"));
    assert_eq!(rig.store.html(&made.app.unwrap().slug).unwrap(), DOC);
}

#[tokio::test]
async fn test_an_answer_without_a_file_is_asked_for_again() {
    let dir = support::tmp();
    let gen = Script::new(vec![stopped("```html\n<!doctype html><html><head>", "max_tokens"), text(fenced(DOC))]);
    let rig = maker_for(dir.path(), gen.generate(), None, 2);
    let made = rig.maker.make("habit tracker", None).await;
    let repair = gen.content(1, 2);
    assert!(made.ok && made.rounds == 1 && repair.contains("cut off"));
    assert!(!repair.contains("tapped every button")); // nothing was tested yet
    assert!(!repair.contains("<<<<<<< FIND")); // no file to edit: the whole file again
}

#[tokio::test]
async fn test_a_build_that_never_produces_a_file_saves_nothing_and_says_why() {
    for (answer, why) in [
        (stopped("```html\n<html><body>", "max_tokens"), "too long"),
        (stopped("no.", "refusal"), "declined"),
        (stopped("just words", "end_turn"), "complete"),
    ] {
        let dir = support::tmp();
        let gen = Script::new(vec![answer]);
        let rig = maker_for(dir.path(), gen.generate(), None, 2);
        let made = rig.maker.make("anything", None).await;
        assert!(!made.ok && made.reason.contains(why) && rig.store.list().is_empty() && *rig.spent.lock() > 0.0, "{why}");
        assert_eq!(gen.calls().len(), if why == "declined" { 1 } else { 3 });
    }
}

#[tokio::test]
async fn test_a_failed_repair_round_keeps_the_first_version() {
    let dir = support::tmp();
    let gen = Script::new(vec![text(fenced(&doc("v1"))), Answer::Fail(overloaded)]);
    let rig = maker_for(dir.path(), gen.generate(), Some(FakeCheck::plain(&[("v1", &["small thing"])])), 2);
    let made = rig.maker.make("habit tracker", None).await;
    assert!(made.ok && rig.store.html(&made.app.clone().unwrap().slug).unwrap().contains("v1"));
    assert_eq!(made.issues, ["small thing"]);
}

#[tokio::test]
async fn test_a_failure_before_any_version_is_raised() {
    let dir = support::tmp();
    let gen = Script::new(vec![Answer::Fail(|| anyhow::anyhow!("RuntimeError: down"))]);
    let rig = maker_for(dir.path(), gen.generate(), None, 2);
    let made = rig.maker.make("x", None).await;
    let e = made.error.expect("raised");
    assert!(!made.ok && e.to_string().contains("down"));
}

#[tokio::test]
async fn test_the_check_can_be_off() {
    let dir = support::tmp();
    let gen = Script::new(vec![text(fenced(DOC))]);
    let store = Arc::new(AppStore::new(&dir.path().join("apps")));
    let maker = AppMaker::new(store, gen.generate(), Arc::new(|_: &Value| 0.0), Arc::new(|usd: f64| usd), Some(Arc::new(String::new)), None, 2);
    let made = maker.make("x", None).await;
    assert!(made.ok && made.check.is_none() && made.rounds == 0);
}

#[tokio::test]
async fn test_changing_an_app_gives_claude_the_file_the_data_and_the_history() {
    let dir = support::tmp();
    let gen = Script::new(vec![text(fenced(DOC)), text(fenced(&doc("v2")))]);
    let check = FakeCheck::plain(&[]);
    let rig = maker_for(dir.path(), gen.generate(), Some(check.clone()), 2);
    rig.maker.make("habit tracker", None).await;
    let data = json!({"v": 1, "habits": [{"name": "Stretch", "days": ["2026-09-23"]}]});
    rig.store.save_data("habit-tracker", &data).unwrap();
    let made = rig.maker.edit("the habit tracker", "add a weekly chart", None).await;
    let prompt = gen.content(1, 0);
    let app = made.app.clone().unwrap();
    assert!(made.ok && app.slug == "habit-tracker" && app.versions == 1);
    assert!(prompt.contains(DOC) && prompt.contains("add a weekly chart") && prompt.contains("\"Habit Tracker\""));
    assert!(prompt.contains("\"Stretch\"") && prompt.contains("- ") && prompt.contains(": habit tracker"));
    assert!(prompt.contains("whole updated file") && prompt.contains("<owner_context>"));
    assert_eq!(check.seeds.lock().last().cloned().flatten(), Some(data));
    let none = rig.maker.edit("zebra planner", "x", None).await;
    assert!(!none.ok && none.reason == "there is no app called 'zebra planner'");
}

#[tokio::test]
async fn test_an_app_deleted_while_it_was_being_changed_is_not_brought_back() {
    let dir = support::tmp();
    let rig = maker_for(dir.path(), Script::new(vec![text(fenced(DOC))]).generate(), None, 2);
    rig.maker.make("habit tracker", None).await;
    let store = rig.store.clone();
    let slow: Generate = Arc::new(move |_messages: Vec<Value>, _progress: MakerProgress| {
        store.delete("habit-tracker").unwrap();
        async move {
            Ok(Generated { text: fenced(&doc("v2")), usage: usage(), stop_reason: "end_turn".into(), content: json!([]) })
        }
        .boxed()
    });
    let maker = maker_with(rig.store.clone(), slow, Some(FakeCheck::plain(&[]).check()), 2, rig.spent.clone());
    let made = maker.edit("habit tracker", "x", None).await;
    assert!(!made.ok && made.reason.contains("deleted") && rig.store.list().is_empty());
}

// ---- the real generate, against a fake transport -------------------------------------------------------------------

/// The Anthropic endpoint, faked: records each request and answers with a streamed body (or a status).
struct FakeAnthropic {
    status: u16,
    body: String,
    requests: Mutex<Vec<HttpRequest>>,
}

#[async_trait]
impl HttpTransport for FakeAnthropic {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        self.requests.lock().push(request);
        Ok(HttpResponse { status: self.status, headers: vec![], body: self.body.clone().into_bytes() })
    }
}

fn sse(events: &[Value]) -> String {
    events
        .iter()
        .map(|e| format!("event: {}\ndata: {}\n\n", e["type"].as_str().unwrap(), e))
        .collect::<Vec<_>>()
        .concat()
}

#[tokio::test]
async fn test_claude_generate_streams_stages_and_caches_the_prefix() {
    let body = sse(&[
        json!({"type": "message_start", "message": {"id": "m", "type": "message", "role": "assistant", "content": [],
               "stop_reason": null, "usage": {"input_tokens": 1, "output_tokens": 0}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "a"}}),
        json!({"type": "ping"}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "b"}}),
        json!({"type": "content_block_stop", "index": 1}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": null}, "usage": {"output_tokens": 2}}),
        json!({"type": "message_stop"}),
    ]);
    let fake = Arc::new(FakeAnthropic { status: 200, body, requests: Mutex::new(Vec::new()) });
    let gen = claude_generate("claude-opus-5-5", "high", Some(fake.clone()));
    let (seen, progress) = stages();
    let out = gen(vec![json!({"role": "user", "content": "x"})], progress).await.unwrap();
    assert_eq!((out.text.as_str(), out.stop_reason.as_str()), ("ab", "end_turn"));
    assert_eq!(out.content, json!([{"type": "thinking", "thinking": "", "signature": "sig"}, {"type": "text", "text": "ab"}]));
    assert_eq!(out.usage, json!({"input_tokens": 1, "output_tokens": 2}));
    let request = fake.requests.lock()[0].clone();
    assert!(request.method == "POST" && request.url.ends_with("/v1/messages"));
    assert_eq!(request.header("anthropic-version"), Some("2023-06-01"));
    let kw: Value = serde_json::from_slice(&request.body).unwrap();
    // the client sends OpenRouter's id for the model the maker names (openrouter.anthropic_client)
    assert_eq!(kw["model"], "anthropic/claude-opus-5.5");
    assert_eq!(kw["output_config"], json!({"effort": "high"}));
    assert_eq!(kw["thinking"], json!({"type": "adaptive"}));
    assert_eq!(kw["cache_control"], json!({"type": "ephemeral"}));
    assert_eq!(kw["system"][0]["text"], MAKER_PROMPT);
    assert_eq!(kw["system"][0]["cache_control"], json!({"type": "ephemeral", "ttl": "1h"})); // outlives a long first round
    assert_eq!(kw["max_tokens"], 96000);
    assert_eq!(kw["stream"], true);
    let got: Vec<(String, i64)> = seen.lock().clone();
    let want: Vec<(String, i64)> =
        vec![("thinking".into(), 0), ("thinking".into(), 0), ("writing".into(), 1), ("writing".into(), 2)];
    assert_eq!(got, want);
}

#[tokio::test]
async fn test_claude_generate_raises_the_apis_refusal() {
    let fake = Arc::new(FakeAnthropic { status: 429, body: "slow down".into(), requests: Mutex::new(Vec::new()) });
    let gen = claude_generate("claude-opus-5-5", "high", Some(fake));
    let e = match gen(vec![json!({"role": "user", "content": "x"})], Arc::new(|_: &str, _: i64| {})).await {
        Err(e) => e,
        Ok(_) => panic!("a 429 is an error"),
    };
    assert_eq!(e.downcast_ref::<eve_openrouter::Error>().and_then(|e| e.status()), Some(429));
    assert_eq!(stopped_reason(&e), "Claude is rate-limited right now. Try again in a minute");
}

// ---- the chat door ------------------------------------------------------------------------------------------------

const URL: &str = "https://quiet-owl.trycloudflare.com";

struct FakeMiniApp {
    maker: Mutex<Arc<AppMaker>>,
    store: Arc<AppStore>,
    url: String,
}

#[async_trait]
impl MakerHost for FakeMiniApp {
    fn url(&self) -> String {
        self.url.clone()
    }
    fn maker(&self) -> Arc<AppMaker> {
        self.maker.lock().clone()
    }
    fn store(&self) -> Arc<AppStore> {
        self.store.clone()
    }
    fn app_url(&self, slug: &str) -> String {
        if self.url.is_empty() { String::new() } else { format!("{}/apps/{slug}/", self.url) }
    }
    fn chat_url(&self, slug: &str) -> String {
        if self.url.is_empty() { String::new() } else { format!("{}/?chat={slug}", self.url) }
    }
    async fn stick_link(&self, _rotate: bool) -> String {
        format!("{}/stick", self.url)
    }
}

type Buttons = Vec<(String, String)>;
type Photo = (i64, Vec<u8>, String, Buttons);

#[derive(Default)]
struct Chat {
    sent: Mutex<Vec<(i64, String, Buttons)>>,
    photos: Mutex<Vec<Photo>>,
    said: Mutex<Vec<String>>,
    photo_fails: bool,
}

impl Chat {
    fn new(photo_fails: bool) -> Arc<Self> {
        Arc::new(Self { photo_fails, ..Self::default() })
    }
    fn send(self: &Arc<Self>) -> SendButtons {
        let me = self.clone();
        Arc::new(move |chat: i64, text: String, buttons: Buttons| {
            me.sent.lock().push((chat, text, buttons));
            async { Ok(()) }.boxed()
        })
    }
    fn say(self: &Arc<Self>) -> SayFn {
        let me = self.clone();
        Arc::new(move |_chat: i64, text: String| {
            me.said.lock().push(text);
            async {}.boxed()
        })
    }
    fn send_photo(self: &Arc<Self>) -> SendPhoto {
        let me = self.clone();
        Arc::new(move |chat: i64, jpeg: Vec<u8>, caption: String, buttons: Buttons| {
            if me.photo_fails {
                return async { Err(anyhow::anyhow!("telegram said no")) }.boxed();
            }
            me.photos.lock().push((chat, jpeg, caption, buttons));
            async { Ok(()) }.boxed()
        })
    }
}

struct Door {
    door: Arc<ChatMaker>,
    host: Arc<FakeMiniApp>,
    store: Arc<AppStore>,
    spawned: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

impl Door {
    /// `await asyncio.gather(*spawned)`.
    async fn gather(&self) {
        loop {
            let jobs: Vec<_> = self.spawned.lock().drain(..).collect();
            if jobs.is_empty() {
                break;
            }
            for job in jobs {
                job.await.unwrap();
            }
        }
        support::settle(20).await; // the progress lines the builds spawned
    }

    async fn handle(&self, name: &str, args: Value, chat: &Arc<Chat>, photo: bool) -> Obj {
        let args = args.as_object().cloned().unwrap_or_default();
        let send_photo = if photo { Some(chat.send_photo()) } else { None };
        self.door.handle(name, &args, 7, chat.send(), chat.say(), send_photo).await
    }
}

fn door_with(dir: &Path, generate: Generate, check: Option<Arc<FakeCheck>>, max_repairs: i64, url: &str) -> Door {
    let rig = maker_for(dir, generate, check, max_repairs);
    let host = Arc::new(FakeMiniApp { maker: Mutex::new(rig.maker.clone()), store: rig.store.clone(), url: url.into() });
    let spawned: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
    let s = spawned.clone();
    let spawn: SpawnFn = Arc::new(move |job: BoxFuture<'static, ()>, _name: &str| s.lock().push(tokio::spawn(job)));
    let door = ChatMaker::new(host.clone(), spawn);
    Door { door, host, store: rig.store, spawned }
}

fn door_for(dir: &Path, generate: Option<Generate>) -> Door {
    door_with(dir, generate.unwrap_or_else(|| Script::new(vec![text(fenced(DOC))]).generate()), None, 2, URL)
}

fn open() -> (String, String) {
    ("Open Habit Tracker".into(), format!("{URL}/apps/habit-tracker/"))
}

fn change() -> (String, String) {
    ("Change".into(), format!("{URL}/?chat=habit-tracker"))
}

#[tokio::test]
async fn test_a_build_from_the_chat_arrives_as_the_apps_picture_with_an_open_button() {
    let dir = support::tmp();
    let chat = Chat::new(false);
    let d = door_for(dir.path(), None);
    let first = d.handle("make_app", json!({"request": "habit tracker"}), &chat, true).await;
    d.gather().await;
    assert_eq!(first["ok"], true);
    assert!(chat.sent.lock().is_empty());
    assert_eq!(*chat.said.lock(), ["Written. Testing it on a phone…"]);
    assert_eq!(
        *chat.photos.lock(),
        vec![(
            7,
            b"\xff\xd8jpeg".to_vec(),
            "✅ Habit Tracker is ready.\nCheck in daily and keep streaks.\n(Picture: a test run with sample entries.)".to_string(),
            vec![open()]
        )]
    );
}

#[tokio::test]
async fn test_without_a_picture_the_button_still_arrives() {
    for photo in ["none", "fails"] {
        let dir = support::tmp();
        let chat = Chat::new(photo == "fails");
        let d = door_for(dir.path(), None);
        d.handle("make_app", json!({"request": "habit tracker"}), &chat, photo == "fails").await;
        d.gather().await;
        assert_eq!(
            *chat.sent.lock(),
            vec![(7, "✅ Habit Tracker is ready.\nCheck in daily and keep streaks.".to_string(), vec![open()])],
            "{photo}"
        );
    }
}

#[tokio::test]
async fn test_the_chat_door_changes_undoes_renames_deletes_and_lists() {
    let dir = support::tmp();
    let chat = Chat::new(false);
    let d = door_for(dir.path(), Some(Script::new(vec![text(fenced(DOC)), text(fenced(&doc("v2")))]).generate()));
    let make = d.handle("make_app", json!({"request": "habit tracker"}), &chat, false).await;
    d.gather().await;
    let change_out = d.handle("change_app", json!({"app": "habit tracker", "change": "add a chart"}), &chat, false).await;
    d.gather().await;
    let v2 = d.store.html("habit-tracker").unwrap();
    let undo = d.handle("undo_app", json!({"app": "habit tracker"}), &chat, false).await;
    let undo_again = d.handle("undo_app", json!({"app": "habit tracker"}), &chat, false).await;
    let rename = d.handle("rename_app", json!({"app": "habit tracker", "title": "Daily Habits"}), &chat, false).await;
    let list = d.handle("list_apps", json!({}), &chat, false).await;
    let unknown = d.handle("delete_app", json!({"app": "zebra"}), &chat, false).await;
    let delete = d.handle("delete_app", json!({"app": "daily habits"}), &chat, false).await;
    let after = d.handle("list_apps", json!({}), &chat, false).await;
    assert!(make["ok"] == true && change_out["ok"] == true && v2.contains("v2"));
    let sent = chat.sent.lock().clone();
    assert_eq!(sent[1], (7, "✅ Habit Tracker is updated.\nCheck in daily and keep streaks.".to_string(), vec![open(), change()]));
    assert!(undo["ok"] == true && undo["earlier_versions_left"] == 0);
    assert_eq!(sent[2].1, "Habit Tracker is back to how it was before its last change.");
    assert!(undo_again["ok"] == false && undo_again["reason"].as_str().unwrap().contains("no earlier version"));
    assert_eq!(rename["renamed"], json!({"from": "Habit Tracker", "to": "Daily Habits"}));
    assert_eq!(list["apps"], json!(["Daily Habits"]));
    assert_eq!(sent.last().unwrap().2[0].0, "✅ Daily Habits");
    assert!(unknown["ok"] == false && unknown["reason"].as_str().unwrap().contains("Daily Habits"));
    assert!(delete["ok"] == true && delete["deleted"] == "Daily Habits" && delete["note"].as_str().unwrap().contains(".trash"));
    assert!(after["apps"] == json!([]) && d.store.list().is_empty());
    assert_eq!(*chat.said.lock(), ["Written. Testing it on a phone…"; 2]); // one line per build, then the result
}

#[tokio::test]
async fn test_an_app_being_changed_cannot_be_deleted_under_it() {
    let dir = support::tmp();
    let chat = Chat::new(false);
    let d = door_for(dir.path(), None);
    d.store.save(DOC, "x", "", None, None).unwrap();
    d.door.building.lock().insert("habit-tracker".into());
    let out = d.handle("delete_app", json!({"app": "habit tracker"}), &chat, false).await;
    assert!(out["ok"] == false && out["reason"].as_str().unwrap().contains("being changed"));
    assert!(d.store.info("habit-tracker").is_some());
}

#[tokio::test]
async fn test_the_chat_door_offers_nothing_while_the_tunnel_is_down() {
    let dir = support::tmp();
    let down = door_with(dir.path(), Script::new(vec![text(fenced(DOC))]).generate(), None, 2, "");
    let up = door_for(dir.path(), None);
    assert!(down.door.tools().is_empty());
    let mut got: Vec<String> = up.door.tools().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    got.sort();
    assert_eq!(got, MAKER_TOOL_NAMES);
    let mut want = vec!["make_app", "change_app", "list_apps", "delete_app", "rename_app", "undo_app", "restore_app"];
    want.sort();
    assert_eq!(MAKER_TOOL_NAMES, want.as_slice());
    for t in up.door.tools() {
        // strict tools: every property required
        let mut required: Vec<String> =
            t["parameters"]["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        let mut props: Vec<String> = t["parameters"]["properties"].as_object().unwrap().keys().cloned().collect();
        required.sort();
        props.sort();
        assert_eq!(required, props);
    }
    // the home screen and the pairing link only while the tunnel is up
    assert_eq!(up.door.home_url(), format!("{URL}/"));
    assert_eq!(down.door.home_url(), "");
    assert_eq!(up.door.stick_link(false).await, format!("{URL}/stick"));
    assert_eq!(down.door.stick_link(false).await, "");
    let chat = Chat::new(false);
    let off = down.handle("list_apps", json!({}), &chat, false).await;
    assert_eq!(off["reason"], "the Mini App is not running right now");
}

#[tokio::test]
async fn test_the_chat_door_restores_a_deleted_app_and_asks_when_unsure() {
    let dir = support::tmp();
    let chat = Chat::new(false);
    let d = door_for(dir.path(), None);
    d.store.save(DOC, "x", "", None, None).unwrap();
    d.handle("delete_app", json!({"app": "habit tracker"}), &chat, false).await;
    let none = d.handle("restore_app", json!({"app": "zebra"}), &chat, false).await;
    assert_eq!(none["reason"], "no deleted app called 'zebra'; the deleted apps are: Habit Tracker");
    let back = d.handle("restore_app", json!({"app": "habit tracker"}), &chat, false).await;
    assert!(back["ok"] == true && back["restored"] == "Habit Tracker");
    assert_eq!(chat.sent.lock().last().unwrap().1, "✅ Habit Tracker is back, with its data.");
    // a new app took the address: the deleted copy cannot come back under it
    d.handle("delete_app", json!({"app": "habit tracker"}), &chat, false).await;
    d.store.save(DOC, "again", "", None, None).unwrap();
    let taken = d.handle("restore_app", json!({"app": "habit tracker"}), &chat, false).await;
    assert!(taken["ok"] == false && taken["reason"].as_str().unwrap().contains("a new app now has Habit Tracker's address"));
    // two apps fit the name: nothing is picked
    let two = door_for(&dir.path().join("two"), None);
    two.store.save(&doc_titled("a", "Water Log"), "a", "", None, None).unwrap();
    two.store.save(&doc_titled("b", "Water Plants"), "b", "", None, None).unwrap();
    let unsure = two.handle("delete_app", json!({"app": "water"}), &chat, false).await;
    assert!(unsure["ok"] == false && unsure["reason"].as_str().unwrap().contains("could mean more than one app"));
    let mut candidates: Vec<String> =
        unsure["candidates"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    candidates.sort();
    assert_eq!(candidates, ["Water Log", "Water Plants"]);
    assert_eq!(two.store.list().len(), 2);
}

// ---- journeys: the maker writes them, the store versions them, the check walks them ----------------------------

fn journey_one() -> Value {
    json!([{"name": "Add one", "steps": [{"do": "tap", "target": "Add"}], "expect": ["1 habit"]}])
}

fn two() -> Value {
    json!([{"name": "Add one", "steps": [{"do": "tap", "target": "Add"}], "expect": ["1 habit"]},
           {"name": "Add two", "steps": [{"do": "tap", "target": "Add"}, {"do": "tap", "target": "Add"}],
            "expect": ["2 habits"]}])
}

fn with_journeys(page: &str, journeys: &Value) -> String {
    format!("{}\n```journeys\n{}\n```\n", fenced(page), journeys)
}

#[test]
fn test_the_prompt_asks_for_journeys_and_the_skeletons_parse() {
    for must in ["```journeys", "\"do\": \"tap\"", "literal", "fresh app with no saved data", "aria-label"] {
        assert!(MAKER_PROMPT.contains(must), "{must}");
    }
    let (found, problems) = eve_jev_verify::parse_journeys(&serde_json::from_str(SKELETON_JOURNEYS).unwrap());
    assert!(problems.is_empty() && found.len() == 3 && MAKER_PROMPT.contains(SKELETON_JOURNEYS));
}

#[tokio::test]
async fn test_a_build_keeps_its_journeys_and_undo_restores_the_pair() {
    let dir = support::tmp();
    let check = FakeCheck::journeys("\0");
    let mut first = journey_one();
    first[0]["name"] = json!("First");
    let gen = Script::new(vec![text(with_journeys(&doc("v1"), &first)), text(with_journeys(&doc("v2"), &journey_one()))]);
    let rig = maker_for(dir.path(), gen.generate(), Some(check.clone()), 2);
    let made = rig.maker.make("habit tracker", None).await;
    let slug = made.app.unwrap().slug;
    assert_eq!(names(&rig.store.journeys(&slug)), ["First"]);
    assert_eq!(check.given_names(0), ["First"]);
    let made2 = rig.maker.edit(&slug, "add a thing", None).await;
    assert!(made2.ok);
    assert_eq!(names(&rig.store.journeys(&slug)), ["Add one"]);
    let edit_prompt_text = gen.content(1, 0);
    assert!(edit_prompt_text.contains("Its journeys now:") && edit_prompt_text.contains("\"First\""));
    assert!(dir.path().join("apps").join(&slug).join("versions").join("1.journeys.json").is_file());
    rig.store.revert(&slug).unwrap();
    assert!(rig.store.html(&slug).unwrap().contains("v1"));
    assert_eq!(names(&rig.store.journeys(&slug)), ["First"]);
}

#[tokio::test]
async fn test_an_app_without_journeys_still_builds_and_is_checked_by_the_script_alone() {
    let dir = support::tmp();
    let check = FakeCheck::journeys("\0");
    let rig = maker_for(dir.path(), Script::new(vec![text(fenced(&doc("v1")))]).generate(), Some(check.clone()), 2);
    let made = rig.maker.make("habit tracker", None).await;
    assert!(made.ok && rig.store.journeys(&made.app.unwrap().slug).is_none());
    assert_eq!(*check.given.lock(), vec![None]);
}

#[tokio::test]
async fn test_a_failed_journey_goes_to_the_repair_round_and_a_journey_only_fix_is_rechecked() {
    let dir = support::tmp();
    let check = FakeCheck::journeys("v1");
    let gen = Script::new(vec![text(with_journeys(&doc("v1"), &journey_one())), text(with_journeys(&doc("v2"), &journey_one()))]);
    let rig = maker_for(dir.path(), gen.generate(), Some(check), 2);
    let made = rig.maker.make("habit tracker", None).await;
    let repair = gen.content(1, 2);
    assert!(repair.contains("Journey \"Add one\"") && repair.contains("Never change a journey to get around"));
    assert!(made.ok && made.rounds == 1 && rig.store.html(&made.app.unwrap().slug).unwrap().contains("v2"));
    // a repair that only rewrites the journeys keeps the last file and checks it again with them
    let check2 = FakeCheck::journeys("v1");
    let mut fixed = journey_one();
    fixed[0]["expect"] = json!(["other"]);
    let gen2 = Script::new(vec![text(with_journeys(&doc("v1"), &journey_one())), text(format!("```journeys\n{fixed}\n```"))]);
    let rig2 = maker_for(&dir.path().join("b"), gen2.generate(), Some(check2.clone()), 2);
    let made2 = rig2.maker.make("habit tracker", None).await;
    let given = check2.given.lock().clone();
    assert_eq!(given.len(), 2);
    assert_eq!(given[1].as_ref().unwrap()[0].expect, ["other"]);
    assert_eq!(rig2.store.journeys(&made2.app.unwrap().slug).unwrap()[0].expect, ["other"]);
}

#[tokio::test]
async fn test_a_malformed_journeys_block_is_a_problem_the_repair_sees() {
    let dir = support::tmp();
    let gen = Script::new(vec![
        text(format!("{}\n```journeys\n[not json\n```", fenced(&doc("v1")))),
        text(with_journeys(&doc("v1"), &journey_one())),
    ]);
    let rig = maker_for(dir.path(), gen.generate(), Some(FakeCheck::journeys("\0")), 2);
    let made = rig.maker.make("habit tracker", None).await;
    assert!(gen.content(1, 2).contains("Journeys block: The journeys block is not valid JSON"));
    assert!(made.ok && rig.store.journeys(&made.app.unwrap().slug).is_some());
}

#[test]
fn test_an_old_versions_journeys_go_with_it_when_it_is_pruned() {
    let dir = support::tmp();
    let store = AppStore::new(&dir.path().join("apps"));
    let (found, _) = eve_jev_verify::parse_journeys(&journey_one());
    let info = store.save(&doc("v0"), "make it", "", Some(&found), None).unwrap();
    for n in 1..(MAX_VERSIONS + 3) {
        let js = if n % 2 == 1 { Some(found.as_slice()) } else { None };
        store.save(&doc(&format!("v{n}")), &format!("change {n}"), &info.slug, js, None).unwrap();
    }
    let vdir = dir.path().join("apps").join(&info.slug).join("versions");
    let files: Vec<String> =
        std::fs::read_dir(&vdir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    let pages: std::collections::HashSet<String> =
        files.iter().filter(|f| f.ends_with(".html")).map(|f| f.trim_end_matches(".html").to_string()).collect();
    let kept: std::collections::HashSet<String> = files
        .iter()
        .filter(|f| f.ends_with(".journeys.json"))
        .map(|f| f.split('.').next().unwrap().to_string())
        .collect();
    assert!(pages.len() == MAX_VERSIONS && kept.is_subset(&pages)); // no journeys outlive their page
    assert!(store.journeys(&info.slug).is_none()); // the last save had none
}

// ---- the app's change chat: what each build did, kept in meta.json ----------------------------------------------

const RESULT_KEYS: [&str; 11] =
    ["ok", "version", "check", "tested", "journeys", "problems", "issues", "usd", "secs", "rounds", "reason"];

fn thread_result(store: &AppStore, slug: &str, i: usize) -> Obj {
    store.thread(slug).unwrap()[i]["result"].as_object().cloned().unwrap()
}

#[tokio::test]
async fn test_each_build_records_its_version_check_journeys_and_cost_for_the_change_chat() {
    let dir = support::tmp();
    let gen = Script::new(vec![text(with_journeys(DOC, &journey_one())), text(with_journeys(&doc("v2"), &journey_one()))]);
    let rig = maker_for(dir.path(), gen.generate(), Some(FakeCheck::walked(3, 3, "")), 2);
    rig.maker.make("a habit tracker", None).await;
    rig.maker.edit("habit-tracker", "make the buttons bigger", None).await;
    let thread = rig.store.thread("habit-tracker").unwrap();
    let kinds: Vec<(String, String)> =
        thread.iter().map(|i| (i["kind"].as_str().unwrap().into(), i["text"].as_str().unwrap().into())).collect();
    assert_eq!(
        kinds,
        vec![("request".to_string(), "a habit tracker".to_string()), ("request".into(), "make the buttons bigger".into())]
    );
    let (first, second) = (thread_result(&rig.store, "habit-tracker", 0), thread_result(&rig.store, "habit-tracker", 1));
    assert!(first["ok"] == true && first["version"] == 1 && second["version"] == 2);
    assert!(first["check"] == "passed: 1 fields filled, 3 taps, 2 saves, journeys 3/3" && first["tested"] == true);
    assert_eq!(first["journeys"], json!({"passed": 3, "total": 3, "not_run": ""}));
    assert!((first["usd"].as_f64().unwrap() - one()).abs() <= 1e-4 && first["problems"] == 0 && first["rounds"] == 0);
    let mut keys: Vec<&str> = first.keys().map(String::as_str).collect();
    keys.sort();
    let mut want = RESULT_KEYS.to_vec();
    want.sort();
    assert_eq!(keys, want);
    let meta: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("apps/habit-tracker/meta.json")).unwrap()).unwrap();
    assert!(meta["version"] == 2 && meta["builds"].as_array().unwrap().len() == 2);
    let asked: Vec<String> = rig.store.requests("habit-tracker").iter().map(|r| r["text"].clone()).collect();
    assert_eq!(asked, ["a habit tracker", "make the buttons bigger"]);
}

#[tokio::test]
async fn test_a_journey_that_did_not_run_or_failed_is_in_the_record() {
    let dir = support::tmp();
    let rig = maker_for(
        dir.path(),
        Script::new(vec![text(with_journeys(DOC, &journey_one()))]).generate(),
        Some(FakeCheck::walked(0, 3, "Jev did not answer")),
        2,
    );
    rig.maker.make("a habit tracker", None).await;
    // the same store, another check, no repairs
    let gen = Script::new(vec![text(with_journeys(DOC, &journey_one()))]);
    let maker = maker_with(rig.store.clone(), gen.generate(), Some(FakeCheck::walked(1, 2, "").check()), 0, rig.spent.clone());
    maker.edit("habit-tracker", "add categories", None).await;
    let (first, second) = (thread_result(&rig.store, "habit-tracker", 0), thread_result(&rig.store, "habit-tracker", 1));
    assert_eq!(first["journeys"], json!({"passed": 0, "total": 3, "not_run": "Jev did not answer"}));
    assert!(second["journeys"] == json!({"passed": 1, "total": 2, "not_run": ""}) && second["problems"] == 1);
    assert_eq!(second["issues"][0], "\"j1\" did not go through."); // the owner's line, not the repair's
}

#[tokio::test]
async fn test_a_change_that_saved_nothing_is_recorded_but_is_not_a_request() {
    let dir = support::tmp();
    let gen = Script::new(vec![text(fenced(DOC)), text("Sorry, I can't."), Answer::Fail(connection_error)]);
    let rig = maker_for(dir.path(), gen.generate(), None, 0);
    rig.maker.make("a habit tracker", None).await;
    gen.set(vec![text("Sorry, I can't.")]);
    let made = rig.maker.edit("habit-tracker", "add a chart", None).await;
    gen.set(vec![Answer::Fail(connection_error)]);
    let purple_made = rig.maker.edit("habit-tracker", "make it purple", None).await;
    assert!(purple_made.error.is_some()); // Python raised the ConnectionError
    let thread = rig.store.thread("habit-tracker").unwrap();
    let texts: Vec<&str> = thread.iter().map(|i| i["text"].as_str().unwrap()).collect();
    assert!(!made.ok);
    assert_eq!(texts, ["a habit tracker", "add a chart", "make it purple"]);
    let (chart, purple) = (thread[1]["result"].clone(), thread[2]["result"].clone());
    assert!(chart["ok"] == false && chart["reason"] == "Claude did not return a complete app" && chart["version"].is_null());
    assert!((chart["usd"].as_f64().unwrap() - one()).abs() <= 1e-4); // it cost money and did nothing
    assert!(purple["ok"] == false && purple["reason"] == "the build stopped before it finished" && purple["usd"] == 0.0);
    // the next build's prompt hears only what the app was changed to do
    let asked: Vec<String> = rig.store.requests("habit-tracker").iter().map(|r| r["text"].clone()).collect();
    assert_eq!(asked, ["a habit tracker"]);
    assert!(rig.store.info("habit-tracker").unwrap().versions == 0 && rig.store.html("habit-tracker").unwrap().contains("hi"));
}

#[test]
fn test_the_thread_keeps_undos_and_requests_from_before_results_and_stays_bounded() {
    let dir = support::tmp();
    let store = AppStore::new(&dir.path().join("apps"));
    let d = dir.path().join("apps").join("habit-tracker");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("index.html"), DOC).unwrap();
    // an app built before results were recorded
    let meta = json!({"title": "Habit Tracker", "created": "2026-09-20T10:00:00", "updated": "2026-09-21T10:00:00",
                      "requests": [{"at": "2026-09-20T10:00:00", "text": "a habit tracker"},
                                   {"at": "2026-09-21T10:00:00", "text": "add streaks"},
                                   {"at": "2026-09-21T11:00:00", "text": "(undo)"}]});
    std::fs::write(d.join("meta.json"), meta.to_string()).unwrap();
    let build = json!({"ok": true, "check": "passed"}).as_object().cloned().unwrap();
    store.save(&doc("v3"), "add a chart", "habit-tracker", None, Some(&build)).unwrap();
    let thread = store.thread("habit-tracker").unwrap();
    let got: Vec<(String, String, bool)> = thread
        .iter()
        .map(|i| (i["kind"].as_str().unwrap().into(), i["text"].as_str().unwrap().into(), !i["result"].is_null()))
        .collect();
    assert_eq!(
        got,
        vec![
            ("request".to_string(), "a habit tracker".to_string(), false),
            ("request".into(), "add streaks".into(), false),
            ("undo".into(), "(undo)".into(), false),
            ("request".into(), "add a chart".into(), true),
        ]
    );
    assert_eq!(thread.last().unwrap()["result"]["version"], 3); // two builds before it (the undo is not one)
    store.revert("habit-tracker").unwrap();
    assert_eq!(store.thread("habit-tracker").unwrap().last().unwrap()["kind"], "undo");
    for n in 0..(MAX_BUILDS + 5) {
        let failed = json!({"ok": false, "reason": "no"}).as_object().cloned().unwrap();
        store.record_build("habit-tracker", &format!("try {n}"), &failed).unwrap();
    }
    let meta: Value = serde_json::from_str(&std::fs::read_to_string(d.join("meta.json")).unwrap()).unwrap();
    let builds = meta["builds"].as_array().unwrap();
    assert!(builds.len() == MAX_BUILDS && builds.last().unwrap()["request"] == format!("try {}", MAX_BUILDS + 4));
    assert!(store.thread("no-such-app").is_err());
    assert!(store.record_build("no-such-app", "x", &json!({"ok": false}).as_object().cloned().unwrap()).is_err());
}

// ---- review round 3: each of these failed on the code before its fix ----------------------------------------

#[tokio::test]
async fn test_an_empty_journeys_block_never_drops_the_journeys_in_hand() {
    let dir = support::tmp();
    let check = FakeCheck::journeys("\0");
    let gen = Script::new(vec![
        text(with_journeys(DOC, &two())),
        text(format!("{}\n```journeys\n[]\n```", fenced(&doc("v2")))),
        text(fenced(&doc("v2"))),
    ]);
    let rig = maker_for(dir.path(), gen.generate(), Some(check.clone()), 1);
    rig.maker.make("a habit tracker", None).await;
    let made = rig.maker.edit("habit-tracker", "bigger buttons", None).await;
    assert_eq!(check.given_names(1), ["Add one", "Add two"]); // walked with the ones in hand
    assert!(gen.content(2, 2).contains("Journeys block: The journeys block holds no journey"));
    assert!(made.ok);
    assert_eq!(names(&rig.store.journeys("habit-tracker")), ["Add one", "Add two"]);
}

#[tokio::test]
async fn test_a_broken_journeys_block_stays_a_problem_until_a_good_one_comes() {
    let dir = support::tmp();
    let gen = Script::new(vec![
        text(format!("{}\n```journeys\n[not json\n```", fenced(&doc("v1")))),
        text(format!("{EDIT_START}\nv1\n{EDIT_MID}\nv1b\n{EDIT_END}")),
    ]);
    let rig = maker_for(dir.path(), gen.generate(), Some(FakeCheck::journeys("\0")), 1);
    let made = rig.maker.make("habit tracker", None).await;
    assert!(
        made.issues.iter().any(|i| i.starts_with("Journeys block: The journeys block is not valid JSON")),
        "{:?}",
        made.issues
    );
}

#[tokio::test]
async fn test_a_repair_that_leaves_a_journey_out_still_walks_it() {
    let dir = support::tmp();
    let check = FakeCheck::journeys("v1");
    let rest = json!([two()[1].clone()]);
    let gen = Script::new(vec![text(with_journeys(&doc("v1"), &two())), text(with_journeys(&doc("v2"), &rest))]);
    let rig = maker_for(dir.path(), gen.generate(), Some(check.clone()), 1);
    let made = rig.maker.make("habit tracker", None).await;
    assert_eq!(check.given_names(1), ["Add two", "Add one"]); // never weakened by a repair
    assert!(made.ok);
    let mut kept = names(&rig.store.journeys(&made.app.unwrap().slug));
    kept.sort();
    assert_eq!(kept, ["Add one", "Add two"]);
}

#[tokio::test]
async fn test_an_edits_journeys_never_carry_the_owners_saved_data_to_jev() {
    let dir = support::tmp();
    let check = FakeCheck::journeys("\0");
    let leaky = json!([
        {"name": "Split", "steps": [{"do": "type", "target": "Name", "value": "Priya"}, {"do": "tap", "target": "Add"}],
         "expect": ["Priya owes $4.20"]},
        {"name": "Clean", "steps": [{"do": "type", "target": "Name", "value": "Alex"}, {"do": "tap", "target": "Add"}],
         "expect": ["Alex owes $3.00"]},
        {"name": "Default", "steps": [{"do": "tap", "target": "Groceries"}], "expect": ["Groceries"]}]);
    // the app's own default: not the owner's
    let page = doc("v2").replace("</body>", "<p>Groceries</p></body>");
    let tail = Value::Array(leaky.as_array().unwrap()[1..].to_vec());
    let gen = Script::new(vec![text(with_journeys(DOC, &two())), text(with_journeys(&page, &leaky)), text(with_journeys(&page, &tail))]);
    let rig = maker_for(dir.path(), gen.generate(), Some(check.clone()), 1);
    rig.maker.make("a splitter", None).await;
    rig.store
        .save_data("habit-tracker", &json!({"v": 1, "people": ["Priya", "Sam"], "cats": ["Groceries"],
                                             "exp": [{"what": "Dinner at the corner place", "amt": 420}]}))
        .unwrap();
    let made = rig.maker.edit("habit-tracker", "add categories", None).await;
    assert_eq!(check.given_names(1), ["Clean", "Default"]); // "Split" never reaches the verifier
    let repair = gen.content(2, 2);
    assert!(repair.contains("Journeys block: journey \"Split\" uses the owner's saved data") && !repair.contains("Priya"));
    assert!(made.ok);
    assert_eq!(names(&rig.store.journeys("habit-tracker")), ["Clean", "Default"]);
}

#[test]
fn test_owner_values_leave_out_the_apps_own_words_dates_and_short_strings() {
    let page = "<p>Groceries</p>";
    let data = json!({"v": 1, "people": ["Priya", "Al"], "cats": ["groceries"], "on": "2026-09-24", "n": [1, 2],
                      "note": "  Dinner   at\nthe place "});
    let mut got = owner_values(&data, page);
    got.sort();
    assert_eq!(got, ["Dinner at the place", "Priya", "cats", "note", "people"]);
    // a value inside a longer word is not that value
    let (kept, problems) = without_owner_data(
        &eve_jev_verify::parse_journeys(&json!([{"name": "x", "steps": [{"do": "tap", "target": "Priyanka"}],
                                                  "expect": ["ok"]}])).0,
        &json!(["Priya"]),
        "",
    );
    assert!(kept.len() == 1 && problems.is_empty());
}

#[test]
fn test_the_prompt_asks_for_result_text_invented_values_a_free_corner_and_option_picks() {
    let journeys_part = &MAKER_PROMPT[MAKER_PROMPT.find("<journeys>").unwrap()..MAKER_PROMPT.find("</journeys>").unwrap()];
    assert!(journeys_part.contains("toast") && journeys_part.contains("the value the journey entered"));
    assert!(journeys_part.contains("made-up example values") && journeys_part.contains("never from the saved data"));
    assert!(journeys_part.contains("a group of option buttons"));
    assert!(MAKER_PROMPT.contains("top-right 56x56px") && MAKER_PROMPT.contains("padding-right: 56px"));
}

#[tokio::test]
async fn test_a_change_that_stops_on_an_api_error_records_why_and_what_it_already_cost() {
    let dir = support::tmp();
    let cut = "```html\n<!doctype html><html><body>cut";
    let gen = Script::new(vec![text(fenced(DOC)), stopped(cut, "max_tokens"), Answer::Fail(rate_limited)]);
    let rig = maker_for(dir.path(), gen.generate(), None, 1);
    rig.maker.make("a habit tracker", None).await;
    gen.set(vec![stopped(cut, "max_tokens"), Answer::Fail(rate_limited)]);
    let made = rig.maker.edit("habit-tracker", "add a chart", None).await;
    assert!(made.error.as_ref().is_some_and(|e| e.to_string().contains("slow down")));
    let thread = rig.store.thread("habit-tracker").unwrap();
    let res = &thread.last().unwrap()["result"];
    assert_eq!(res["reason"], "Claude is rate-limited right now. Try again in a minute");
    assert!((res["usd"].as_f64().unwrap() - one()).abs() <= 1e-4 && res["rounds"] == 1);
}

#[test]
fn test_the_owner_reads_journey_problems_in_plain_words_and_long_lines_end_in_an_ellipsis() {
    let mut failed = JourneyResult::new("Split a dinner", "step");
    failed.steps = 6;
    failed.step = 3;
    failed.step_words = "tap \"Add expense\"".into();
    failed.reason = format!(
        "tap \"Add expense\": could not find \"Add expense\" to tap (the controls on screen: {}).",
        (0..14).map(|n| format!("\"Control {n}\"")).collect::<Vec<_>>().join(", ")
    );
    failed.evidence = format!("\"{}\"", "Expense Splitter | People ".repeat(30));
    let run = JourneyRun { results: vec![failed], total: 1, ..Default::default() };
    let long_one = format!("Uncaught error while tapping: {}", "x".repeat(400));
    let mut issues = run.issues();
    issues.push(long_one);
    let report = CheckReport { ok: false, issues: issues.clone(), journeys: Some(run), ..Default::default() };
    let rec = build_record(true, Some(&report), &report.issues, 0.1, 5.0, 1, "");
    assert_eq!(rec["issues"][0], "Couldn't tap \"Add expense\" in \"Split a dinner\".");
    let second = rec["issues"][1].as_str().unwrap();
    assert!(second.chars().count() == 300 && second.ends_with('…'));
    assert_eq!(owner_issues(Some(&report), &report.issues, 300)[0], rec["issues"][0]);
}

#[tokio::test]
async fn test_a_chat_build_caption_names_a_journey_problem_in_plain_words() {
    let dir = support::tmp();
    let chat = Chat::new(false);
    let d = door_with(
        dir.path(),
        Script::new(vec![text(with_journeys(DOC, &journey_one()))]).generate(),
        Some(FakeCheck::new(Kind::Failing)),
        0,
        URL,
    );
    d.handle("make_app", json!({"request": "habit tracker"}), &chat, true).await;
    d.gather().await;
    let photos = chat.photos.lock();
    assert!(
        photos[0].2.contains("It may still have a problem: After \"Add one\", the screen didn't show \"1 habit\"."),
        "{}",
        photos[0].2
    );
}

#[tokio::test]
async fn test_a_chat_change_that_fails_still_offers_its_change_chat() {
    let dir = support::tmp();
    let chat = Chat::new(false);
    let d = door_with(dir.path(), Script::new(vec![text("Sorry, I can't.")]).generate(), None, 0, URL);
    d.store.save(DOC, "x", "", None, None).unwrap();
    d.handle("change_app", json!({"app": "habit tracker", "change": "add a chart"}), &chat, false).await;
    let again = d.handle("change_app", json!({"app": "habit tracker", "change": "add a chart"}), &chat, false).await;
    assert_eq!(again["reason"], "that one is already being built");
    d.gather().await;
    let failing = maker_with(
        d.store.clone(),
        Script::new(vec![Answer::Fail(rate_limited)]).generate(),
        Some(FakeCheck::plain(&[]).check()),
        0,
        Arc::new(Mutex::new(0.0)),
    );
    *d.host.maker.lock() = failing;
    d.handle("change_app", json!({"app": "habit tracker", "change": "add a chart"}), &chat, false).await;
    d.gather().await;
    let lines: Vec<(String, Buttons)> = chat.sent.lock().iter().map(|(_, t, b)| (t.clone(), b.clone())).collect();
    assert_eq!(
        lines[0],
        ("I couldn't change Habit Tracker; it is unchanged: Claude did not return a complete app.".to_string(), vec![change()])
    );
    assert_eq!(
        *lines.last().unwrap(),
        (
            "I couldn't change Habit Tracker; it is unchanged: Claude is rate-limited right now. Try again in a minute."
                .to_string(),
            vec![change()]
        )
    );
}

// ---- the pieces of this file that the tests above reach only in part ----------------------------------------------

#[test]
fn test_the_prompts_say_what_to_send_back() {
    let issues = vec!["one".to_string(), "two".to_string()];
    let edit = repair_prompt(&issues, true, true);
    assert!(edit.starts_with("I opened that file on a phone-sized screen") && edit.contains("1. one\n2. two"));
    assert!(edit.contains("<<<<<<< FIND\n(lines copied exactly") && edit.ends_with(JOURNEY_REPAIR));
    let whole = repair_prompt(&issues, false, false);
    assert!(whole.starts_with("These problems came up:\n1. one") && whole.ends_with("then its ```journeys block."));
    assert_eq!(build_prompt("  a log  ", "ctx"), "<owner_context>\nctx\n</owner_context>\n\nBuild this app: a log");
}

#[test]
fn test_served_apps_get_telegram_and_window_eve_first() {
    let out = serve_app_html(DOC);
    assert!(out.find("src=\"/eve.js\"").unwrap() < out.find("<title>").unwrap() && out.contains("telegram-web-app.js"));
    let own = serve_app_html("<HTML><HEAD lang=x><script src=\"https://telegram.org/js/telegram-web-app.js\"></script></HEAD></HTML>");
    assert_eq!(own.matches("telegram-web-app.js").count(), 1);
    assert!(own.starts_with("<HTML><HEAD lang=x><script src=\"/eve.js\"></script>"));
    assert_eq!(serve_app_html("<p>x</p>"), "<script src=\"https://telegram.org/js/telegram-web-app.js\"></script><script src=\"/eve.js\"></script><p>x</p>");
    assert!(EVE_JS.contains("window.eve") && EVE_JS.contains("/save"));
}
