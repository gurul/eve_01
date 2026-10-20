//! app_reflex.py: a bare app launch is `open -a`, everything else is the wrapped agent's.
//!
//! Ports bridge/tests/test_app_reflex.py, plus the parts of test_quit_reflex.py, test_desk_lane.py and
//! test_browser_router.py that drive ReflexFirstAgent or quit_all.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Weak};

use async_trait::async_trait;
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Map, Value};

use super::*;

const APPS: [&str; 4] = ["Spotify", "Notes", "Safari", "Warp"];

fn apps_of(names: &[&str]) -> Apps {
    let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
    Arc::new(move || names.clone())
}

/// The wrapped agent: records its goals, answers `reply`.
struct Inner {
    provider: String,
    reply: String,
    goals: Mutex<Vec<String>>,
    cancelled: Mutex<String>,
    /// `codex.discard = discard`: a warm agent's close, recorded here; `None` is an agent without one.
    discarded: Option<Arc<Mutex<Vec<String>>>>,
}

impl Inner {
    fn new() -> Arc<Inner> {
        Inner::named("codex", "codex did it")
    }

    fn named(provider: &str, reply: &str) -> Arc<Inner> {
        Arc::new(Inner {
            provider: provider.into(),
            reply: reply.into(),
            goals: Mutex::new(Vec::new()),
            cancelled: Mutex::new(String::new()),
            discarded: None,
        })
    }

    fn goals(&self) -> Vec<String> {
        self.goals.lock().clone()
    }
}

#[async_trait]
impl Agent for Inner {
    async fn run(&self, goal: &str) -> anyhow::Result<String> {
        self.goals.lock().push(goal.to_string());
        Ok(self.reply.clone())
    }

    fn cancel(&self, reason: &str) {
        *self.cancelled.lock() = reason.to_string();
    }

    fn steer(&self, _text: &str) -> bool {
        false
    }

    fn status(&self) -> Map<String, Value> {
        match json!({"running": false, "provider": self.provider}) {
            Value::Object(m) => m,
            _ => Map::new(),
        }
    }

    fn provider(&self) -> String {
        self.provider.clone()
    }

    async fn discard(&self) -> anyhow::Result<()> {
        if let Some(d) = &self.discarded {
            d.lock().push("codex".into());
        }
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn make(inner: &Arc<Inner>) -> MakeAgent {
    let inner = Arc::clone(inner);
    Arc::new(move || Arc::clone(&inner) as Arc<dyn Agent>)
}

type Events = Arc<Mutex<Vec<(String, String)>>>;

fn recorder(events: &Events) -> OnEvent {
    let events = Arc::clone(events);
    Arc::new(move |ev: AgentEvent| events.lock().push((ev.kind().to_string(), ev.text().to_string())))
}

fn done_recorder(events: &Events) -> OnDone {
    let events = Arc::clone(events);
    Arc::new(move || events.lock().push(("done".into(), String::new())))
}

fn opener_recording(opened: &Arc<Mutex<Vec<String>>>, ok: bool) -> Opener {
    let opened = Arc::clone(opened);
    Arc::new(move |app: String| {
        opened.lock().push(app);
        Box::pin(async move { Ok((ok, if ok { String::new() } else { "Unable to find application".to_string() })) })
    })
}

fn ok_opener() -> Opener {
    Arc::new(|_app: String| Box::pin(async { Ok((true, String::new())) }))
}

fn ev(kind: &str, text: &str) -> (String, String) {
    (kind.to_string(), text.to_string())
}

struct Rig {
    agent: ReflexFirstAgent,
    inner: Arc<Inner>,
    opened: Arc<Mutex<Vec<String>>>,
    events: Events,
}

fn rig_with(ok: bool, asker: Option<Asker>, enabled: bool) -> Rig {
    let events: Events = Arc::default();
    let opened: Arc<Mutex<Vec<String>>> = Arc::default();
    let inner = Inner::new();
    let agent = ReflexFirstAgent::new(
        make(&inner),
        recorder(&events),
        ReflexOptions {
            apps: apps_of(&APPS),
            asker,
            opener: opener_recording(&opened, ok),
            enabled,
            on_done: done_recorder(&events),
            ..Default::default()
        },
    );
    Rig { agent, inner, opened, events }
}

fn rig() -> Rig {
    rig_with(true, None, true)
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[tokio::test]
async fn test_bare_launch_is_opened_without_the_inner_agent() {
    let r = rig();
    assert_eq!(r.agent.run("open Spotify", "").await.unwrap(), "Opened Spotify.");
    assert_eq!(*r.opened.lock(), strs(&["Spotify"]));
    assert!(r.inner.goals().is_empty());
    assert_eq!(r.agent.reflexed().as_deref(), Some("Spotify"));
    assert_eq!(*r.events.lock(), vec![ev("started", "open Spotify"), ev("final", "Opened Spotify.")]);
}

#[tokio::test]
async fn test_launch_then_more_goes_to_the_inner_agent_whole() {
    let r = rig();
    assert_eq!(r.agent.run("open Spotify and play my discover weekly", "").await.unwrap(), "codex did it");
    assert!(r.opened.lock().is_empty());
    assert_eq!(r.inner.goals(), strs(&["open Spotify and play my discover weekly"]));
}

#[tokio::test]
async fn test_consequential_and_uninstalled_are_the_inner_agents() {
    let r = rig();
    r.agent.run("force quit Spotify", "").await.unwrap();
    r.agent.run("open Photoshop", "").await.unwrap();
    assert!(r.opened.lock().is_empty());
    assert_eq!(r.inner.goals(), strs(&["force quit Spotify", "open Photoshop"]));
}

#[tokio::test]
async fn test_failed_open_falls_back_to_the_inner_agent() {
    let r = rig_with(false, None, true);
    assert_eq!(r.agent.run("open Notes", "").await.unwrap(), "codex did it");
    assert_eq!(*r.opened.lock(), strs(&["Notes"]));
    assert_eq!(r.inner.goals(), strs(&["open Notes"]));
}

#[tokio::test]
async fn test_jev_answers_wording_the_rules_do_not_know() {
    let asked: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = Arc::clone(&asked);
    let asker: Asker = Arc::new(move |goal: &str, _apps: &[String]| {
        seen.lock().push(goal.to_string());
        Ok("Notes".to_string())
    });
    let r = rig_with(true, Some(asker), true);
    assert_eq!(r.agent.run("Notes, please.", "").await.unwrap(), "Opened Notes.");
    assert!(!asked.lock().is_empty());
    assert_eq!(*r.opened.lock(), strs(&["Notes"]));
    assert!(r.inner.goals().is_empty());
}

#[tokio::test]
async fn test_jev_naming_an_uninstalled_app_is_ignored_and_errors_abstain() {
    let r = rig_with(true, Some(Arc::new(|_g: &str, _a: &[String]| Ok("Photoshop".to_string()))), true);
    r.agent.run("Notes, please.", "").await.unwrap();

    let boom: Asker = Arc::new(|_g: &str, _a: &[String]| {
        Err(anyhow::Error::new(std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out")))
    });
    let r2 = rig_with(true, Some(boom), true);
    r2.agent.run("Notes, please.", "").await.unwrap();
    assert!(r.opened.lock().is_empty() && r2.opened.lock().is_empty());
    assert_eq!(r.inner.goals(), strs(&["Notes, please."]));
    assert_eq!(r2.inner.goals(), strs(&["Notes, please."]));
}

#[tokio::test]
async fn test_disabled_passes_everything_through() {
    let r = rig_with(true, None, false);
    r.agent.run("open Spotify", "").await.unwrap();
    assert!(r.opened.lock().is_empty());
    assert_eq!(r.inner.goals(), strs(&["open Spotify"]));
}

#[tokio::test]
async fn test_the_inner_agent_is_built_only_when_the_reflex_declines() {
    let built: Arc<Mutex<Vec<i32>>> = Arc::default();
    let inner = Inner::new();
    let (b, i) = (Arc::clone(&built), Arc::clone(&inner));
    let make: MakeAgent = Arc::new(move || {
        b.lock().push(1);
        Arc::clone(&i) as Arc<dyn Agent>
    });
    let agent = ReflexFirstAgent::new(
        make,
        Arc::new(|_ev| {}),
        ReflexOptions { apps: apps_of(&APPS), opener: ok_opener(), ..Default::default() },
    );
    agent.run("open Spotify", "").await.unwrap();
    assert!(built.lock().is_empty()); // a launch never spends the warm Codex agent
    agent.run("open Spotify and play jazz", "").await.unwrap();
    assert_eq!(*built.lock(), vec![1]);
}

#[tokio::test]
async fn test_on_done_follows_a_handoff_only() {
    let r = rig();
    r.agent.run("open Spotify", "").await.unwrap();
    assert!(!r.events.lock().contains(&ev("done", "")));
    r.agent.run("play jazz on spotify", "").await.unwrap();
    assert_eq!(r.events.lock().last().cloned(), Some(ev("done", "")));
}

#[tokio::test]
async fn test_cancel_before_the_handoff_never_starts_it_and_after_is_forwarded() {
    let r = rig();
    r.agent.cancel("stop");
    assert_eq!(r.agent.run("play jazz on spotify", "").await.unwrap(), "Stopped.");
    assert!(r.inner.goals().is_empty());
    let r2 = rig();
    r2.agent.run("play jazz on spotify", "").await.unwrap();
    r2.agent.cancel("stop");
    assert_eq!(*r2.inner.cancelled.lock(), "stop");
    assert_eq!(r2.agent.provider(), "codex");
    assert!(!r2.agent.running());
    assert!(!r2.agent.status().is_empty());
}

#[test]
fn test_switches() {
    let env = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    };
    assert!(!reflexes_on(Some(&env(&[("EVE_REFLEXES", "0")]))));
    assert!(reflexes_on(Some(&env(&[]))));
    assert!(jev_asker(Some(&env(&[("EVE_ROUTER_MODEL", "off")]))).is_none());
    // no key
    assert!(jev_asker(Some(&env(&[("EVE_ROUTER_MODEL", "jev"), ("EVE_JEV_ROUTE", "openrouter")]))).is_none());
    assert!(jev_quit_asker(Some(&env(&[("EVE_ROUTER_MODEL", "off")]))).is_none());
    assert!(jev_quit_asker(Some(&env(&[("EVE_ROUTER_MODEL", "jev"), ("EVE_JEV_ROUTE", "openrouter")]))).is_none());
}

#[test]
fn test_the_switches_parse_like_python() {
    let env = |k: &str, v: &str| -> HashMap<String, String> { HashMap::from([(k.to_string(), v.to_string())]) };
    for off in ["0", "false", " NO ", "Off"] {
        assert!(!reflexes_on(Some(&env("EVE_REFLEXES", off))), "{off}");
    }
    for on in ["1", "yes", "anything", "  "] {
        assert!(reflexes_on(Some(&env("EVE_REFLEXES", on))), "{on:?}");
    }
    // with a key the asker exists, under the same switch, and Jev is only built
    let with_key: HashMap<String, String> = [
        ("EVE_ROUTER_MODEL", " JEV "),
        ("EVE_JEV_ROUTE", "openrouter"),
        ("OPENROUTER_API_KEY", "test-key"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    assert!(jev_asker(Some(&with_key)).is_some());
    assert!(jev_quit_asker(Some(&with_key)).is_some());
}

#[test]
fn test_agent_event_has_one_definition() {
    // the wrapper speaks agent_contract's AgentEvent, re-exported, not a copy of it
    let event: eve_agent_contract::AgentEvent = AgentEvent::with_text("final", "Opened Spotify.");
    assert_eq!(event.kind(), "final");
}

// ---- a request that refers back to a link from an earlier message ----------------------------------------

const MAP: &str = "https://maps.app.goo.gl/AbCdEf123";

/// browser_lane.WEB_WORDS (is_web_goal with no route kind), as the tests use it.
static WEB_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)(https?://|www\.|\.(com|org|net|io|ai|gov|edu)\b|\b(browser|chrome|safari|website|web ?page|webpage|",
        r"google|youtube|amazon|github|wikipedia|reddit|twitter|linkedin|gmail|news|hacker ?news|bbc|nytimes|",
        r"stack ?overflow|search (the )?web|look up|google it)\b)",
    ))
    .unwrap()
});

fn is_web_goal(goal: &str) -> bool {
    WEB_WORDS.is_match(goal)
}

#[test]
fn test_open_it_after_a_shared_link_carries_the_link() {
    // Production, 2026-09-24: the owner shared a Google Maps link, then "Use Google search to open it up"; the goal
    // was the owner's words for that request only, so the link was not in it.
    let here = format!("here {MAP}");
    let recent = [here.as_str(), "Use Google search to open it up"];
    let goal = with_referenced_links("Use Google search to open it up", recent);
    assert!(goal.starts_with("Use Google search to open it up") && goal.contains(MAP));
    assert!(is_web_goal(&goal));
    assert_eq!(
        goal,
        format!(
            "Use Google search to open it up\n\n[note] The owner's recent messages included this link, which the \
             request refers to: {MAP}"
        )
    );
}

#[test]
fn test_the_newest_links_come_first_and_only_the_recent_messages_count() {
    let (old, mid, new) = ("https://example.org/old", "https://example.org/mid", "https://example.org/new.");
    let recent = [old, "a", "b", mid, new]; // `old` is five messages back: out of reach
    let goal = with_referenced_links("open that link", recent);
    assert!(goal.find("example.org/new").unwrap() < goal.find("example.org/mid").unwrap() && !goal.contains(old));
    assert!(!goal.contains("example.org/new.")); // trailing punctuation is not part of a link
    assert!(goal.contains("these links, newest first"));
}

#[test]
fn test_a_goal_that_does_not_refer_back_or_has_its_own_link_is_unchanged() {
    let here = format!("here {MAP}");
    let recent = [here.as_str()];
    for goal in ["play some jazz on spotify", "open https://example.org/x", "what's the weather", ""] {
        assert_eq!(with_referenced_links(goal, recent), goal);
    }
    assert_eq!(with_referenced_links("open it", ["no links here"]), "open it");
}

#[test]
fn test_links_are_capped_and_deduplicated() {
    let recent = ["https://a.org/1 https://a.org/1", "https://a.org/2", "https://a.org/3 https://a.org/4)"];
    assert_eq!(links_in("x https://a.org/1, https://a.org/1. (https://a.org/2)"), strs(&["https://a.org/1", "https://a.org/2"]));
    let goal = with_referenced_links("show me that page", recent);
    assert!(goal.ends_with(": https://a.org/3 https://a.org/4 https://a.org/2"), "{goal}");
}

#[test]
fn test_goal_shape_names_hosts_never_words() {
    assert_eq!(goal_shape(""), "0 words, 0 link(s)");
    assert_eq!(
        goal_shape("open that link HTTPS://User@Example.ORG:8080/x and http://[::1]:80/ too"),
        "7 words, 2 link(s) (example.org, ::1), refers back"
    );
    assert_eq!(goal_shape("go to https:///nothing"), "3 words, 1 link(s) (?)");
}

/// `caplog`: run `f` with a thread-local subscriber and return the log text.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn caplog<R>(f: impl FnOnce() -> R) -> (R, String) {
    let buf = Captured::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt().with_ansi(false).with_writer(move || writer.clone()).finish();
    let out = tracing::subscriber::with_default(subscriber, f);
    let text = String::from_utf8(buf.0.lock().clone()).unwrap();
    (out, text)
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
}

#[test]
fn test_the_body_logs_the_goals_shape_never_its_words() {
    let r = rig_with(true, None, false);
    let (_, text) = caplog(|| block_on(r.agent.run(&format!("please open this secretive place {MAP}"), "")).unwrap());
    let line = text.lines().find(|l| l.contains("gets a goal of")).expect("the goal line");
    assert!(line.contains("6 words, 1 link(s) (maps.app.goo.gl)") && !line.contains("secretive"), "{line}");
}

/// telegram.TASK_FILE_HINT.
const TASK_FILE_HINT: &str = "\n\n(If you make or save a file the owner should get, such as a photo, a screenshot \
or a document, give its full path in your final answer: eve sends it to them. Do not try to attach, upload or open \
it in a browser yourself.)";

struct Routed {
    agent: ReflexFirstAgent,
    codex: Arc<Inner>,
    chrome: Arc<Inner>,
    opened: Arc<Mutex<Vec<String>>>,
}

/// The daemon's shape: Codex inside, the Chrome lane beside it, the Chrome lane's own rule choosing.
fn routed_rig() -> Routed {
    let opened: Arc<Mutex<Vec<String>>> = Arc::default();
    let (codex, chrome) = (Inner::new(), Inner::named("chrome-lane", "codex did it"));
    let route_body: RouteBody = Arc::new(|goal: String| {
        Box::pin(async move { Ok(if is_web_goal(&goal) { "chrome" } else { "codex" }.to_string()) })
    });
    let agent = ReflexFirstAgent::new(
        make(&codex),
        Arc::new(|_ev| {}),
        ReflexOptions {
            apps: apps_of(&["Spotify", "Notes", "Safari", "Warp", "Photo Booth"]),
            opener: opener_recording(&opened, true),
            make_auto: Some(make(&chrome)),
            route_body: Some(route_body),
            ..Default::default()
        },
    );
    Routed { agent, codex, chrome, opened }
}

#[tokio::test]
async fn test_the_door_note_never_steers_the_route() {
    // Production 2026-09-26: Telegram's file hint says "open it in a browser", and read with the goal it sent every
    // Telegram task to the Chrome lane and made "open Spotify" too long to be a launch. The note now goes beside the
    // goal: the reflex and the router read the owner's words, the body gets both.
    let r = routed_rig();
    assert_eq!(r.agent.run("Open up Spotify.", TASK_FILE_HINT).await.unwrap(), "Opened Spotify.");
    assert_eq!(*r.opened.lock(), strs(&["Spotify"]));
    assert!(r.codex.goals().is_empty() && r.chrome.goals().is_empty());

    let r = routed_rig();
    let goal = "Send the picture I just took on Photo Booth to me";
    r.agent.run(goal, TASK_FILE_HINT).await.unwrap();
    assert!(r.chrome.goals().is_empty() && r.opened.lock().is_empty());
    assert_eq!(r.codex.goals(), vec![format!("{goal}{TASK_FILE_HINT}")]);

    let r = routed_rig();
    r.agent.run("open youtube.com in chrome", TASK_FILE_HINT).await.unwrap();
    assert!(r.codex.goals().is_empty());
    assert_eq!(r.chrome.goals(), vec![format!("open youtube.com in chrome{TASK_FILE_HINT}")]);
    assert_eq!(r.agent.body(), "chrome");
    assert_eq!(r.agent.provider(), "chrome-lane");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_stop_while_the_body_router_thinks_never_starts_the_body() {
    // A Stop during the router's await (Jev, about 2 s) used to be recorded and then ignored: the body ran.
    let events: Events = Arc::default();
    let discarded: Arc<Mutex<Vec<String>>> = Arc::default();
    let codex = Arc::new(Inner {
        provider: "codex".into(),
        reply: "codex did it".into(),
        goals: Mutex::new(Vec::new()),
        cancelled: Mutex::new(String::new()),
        discarded: Some(Arc::clone(&discarded)), // a warm agent's close
    });
    let chrome = Inner::new();
    let asked = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (a, rel) = (Arc::clone(&asked), Arc::clone(&release));
    let route_body: RouteBody = Arc::new(move |_goal: String| {
        let (a, rel) = (Arc::clone(&a), Arc::clone(&rel));
        Box::pin(async move {
            a.notify_one();
            rel.notified().await;
            Ok("codex".to_string())
        })
    });
    let agent = Arc::new(ReflexFirstAgent::new(
        make(&codex),
        recorder(&events),
        ReflexOptions {
            apps: apps_of(&APPS),
            opener: ok_opener(),
            make_auto: Some(make(&chrome)),
            route_body: Some(route_body),
            on_done: done_recorder(&events),
            ..Default::default()
        },
    ));
    let runner = Arc::clone(&agent);
    let task = tokio::spawn(async move { runner.run("play jazz on spotify", "").await });
    asked.notified().await;
    agent.cancel("stop");
    release.notify_one();
    assert_eq!(task.await.unwrap().unwrap(), "Stopped.");
    assert!(codex.goals().is_empty() && chrome.goals().is_empty());
    assert!(events.lock().contains(&ev("cancelled", "Stopped.")));
    // the warm agent closed, the next one warmed
    assert_eq!(*discarded.lock(), strs(&["codex"]));
    assert!(events.lock().contains(&ev("done", "")));
}

// ---- test_quit_reflex.py: the wrapper and quit_all ------------------------------------------------------------

const QUIT_APPS: [&str; 9] =
    ["Spotify", "Safari", "Google Chrome", "System Settings", "Notes", "Warp", "TV", "Messages", "Slack"];

#[tokio::test]
async fn test_the_wrapper_quits_without_codex() {
    let quit: Arc<Mutex<Vec<String>>> = Arc::default();
    let inner = Inner::named("codex", "codex");
    let (q1, q2) = (Arc::clone(&quit), Arc::clone(&quit));
    let quitter: Quitter = Arc::new(move |app: String| {
        q1.lock().push(app.clone());
        Box::pin(async move { Ok(format!("Quit {app}.")) })
    });
    let everything: QuitEverything = Arc::new(move || {
        q2.lock().push("*".into());
        Box::pin(async { Ok("Quit 3 apps.".to_string()) })
    });
    let agent = ReflexFirstAgent::new(
        make(&inner),
        Arc::new(|_ev| {}),
        ReflexOptions { apps: apps_of(&QUIT_APPS), quitter, quit_everything: Some(everything), ..Default::default() },
    );
    assert_eq!(agent.run("quit slack", "").await.unwrap(), "Quit Slack.");
    assert_eq!(agent.reflexed().as_deref(), Some("Slack"));
    assert_eq!(agent.run("quit all", "").await.unwrap(), "Quit 3 apps.");
    assert_eq!(agent.reflexed().as_deref(), Some("*"));
    assert_eq!(agent.run("close slack", "").await.unwrap(), "codex");
    assert_eq!(*quit.lock(), strs(&["Slack", "*"]));
    assert_eq!(inner.goals(), strs(&["close slack"]));
}

#[tokio::test]
async fn test_a_quit_that_fails_falls_back_to_the_inner_agent() {
    let inner = Inner::new();
    let quitter: Quitter = Arc::new(|_app: String| {
        Box::pin(async { Err(anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::NotFound))) })
    });
    let agent = ReflexFirstAgent::new(
        make(&inner),
        Arc::new(|_ev| {}),
        ReflexOptions { apps: apps_of(&QUIT_APPS), quitter, ..Default::default() },
    );
    assert_eq!(agent.run("quit slack", "").await.unwrap(), "codex did it");
    assert_eq!(inner.goals(), strs(&["quit slack"]));
    assert_eq!(agent.reflexed(), None);
}

/// `_osascript` and `_running` as the quit tests patch them.
struct FakeOsa {
    calls: Mutex<Vec<String>>,
    processes: String,
    list_ok: bool,
    still_running: Vec<String>,
    quit_ok: bool,
}

impl FakeOsa {
    fn new(processes: &str, still_running: &[&str]) -> FakeOsa {
        FakeOsa {
            calls: Mutex::new(Vec::new()),
            processes: processes.into(),
            list_ok: true,
            still_running: strs(still_running),
            quit_ok: true,
        }
    }
}

#[async_trait]
impl Osascript for FakeOsa {
    async fn osascript(&self, lines: &[String]) -> anyhow::Result<(bool, String)> {
        self.calls.lock().push(lines.join(" "));
        if lines[0].contains("every application process") {
            return Ok((self.list_ok, self.processes.clone()));
        }
        if lines.iter().any(|l| l.contains(" to quit")) && !self.quit_ok {
            return Ok((false, "execution error: not allowed".into()));
        }
        Ok((true, String::new()))
    }

    async fn running(&self, name: &str) -> anyhow::Result<bool> {
        Ok(self.still_running.iter().any(|n| n == name))
    }
}

#[tokio::test]
async fn test_quit_all_keeps_eves_own_and_the_terminals() {
    let osa = FakeOsa::new("Finder, ChatGPT, stable, Claude, Spotify, Slack, StackChanNotes, Preview", &["Preview"]);
    let env = HashMap::from([("EVE_QUIT_ALL_KEEP".to_string(), "Slack".to_string())]);
    let said = quit_all_with(&osa, &quit_keep(Some(&env)), 0.0).await.unwrap();
    let calls = osa.calls.lock().clone();
    let quits: Vec<&String> = calls.iter().filter(|c| c.contains(" to quit")).collect();
    assert_eq!(quits.len(), 2);
    assert!(quits.iter().any(|c| c.contains("\"Spotify\"")) && quits.iter().any(|c| c.contains("\"Preview\"")));
    assert!(said.starts_with("Quit 1 app: Spotify.") && said.contains("Preview didn't quit"), "{said}");
    assert!(said.contains("Kept ChatGPT, Warp, Claude, Slack, StackChanNotes."), "{said}");
    assert_eq!(
        said,
        "Quit 1 app: Spotify. Preview didn't quit (asking to save, or declined); it is still open. Kept ChatGPT, \
         Warp, Claude, Slack, StackChanNotes."
    );
}

#[tokio::test]
async fn test_quit_all_sentences() {
    let keep = quit_keep(Some(&HashMap::new()));
    let osa = FakeOsa::new("Finder, Claude", &[]);
    assert_eq!(quit_all_with(&osa, &keep, 0.0).await.unwrap(), "Nothing to quit. Kept Claude.");
    let osa = FakeOsa::new("Finder", &[]);
    assert_eq!(quit_all_with(&osa, &keep, 0.0).await.unwrap(), "Nothing to quit.");
    let osa = FakeOsa::new("Spotify, Slack", &["Spotify", "Slack"]);
    assert_eq!(
        quit_all_with(&osa, &keep, 0.0).await.unwrap(),
        "Quit 0 apps. Spotify, Slack didn't quit (asking to save, or declined); they are still open."
    );
    let osa = FakeOsa::new("Spotify, Slack, , ", &[]);
    assert_eq!(quit_all_with(&osa, &keep, 0.0).await.unwrap(), "Quit 2 apps: Spotify, Slack.");
    let mut osa = FakeOsa::new(&"x".repeat(300), &[]);
    osa.list_ok = false;
    assert_eq!(
        quit_all_with(&osa, &keep, 0.0).await.unwrap(),
        format!("I couldn't list the open apps: {}", "x".repeat(120))
    );
}

#[tokio::test]
async fn test_quit_app_sentences() {
    let osa = FakeOsa::new("", &[]);
    assert_eq!(quit_app_with(&osa, "Slack", 0.0).await.unwrap(), "Slack isn't open.");
    let osa = FakeOsa::new("", &["Slack"]);
    assert_eq!(
        quit_app_with(&osa, "Slack", 0.0).await.unwrap(),
        "Slack didn't quit. It may be asking to save, or it declined; it's still open."
    );
    let mut osa = FakeOsa::new("", &["Slack"]);
    osa.quit_ok = false;
    assert_eq!(quit_app_with(&osa, "Slack", 0.0).await.unwrap(), "I couldn't quit Slack: execution error: not allowed");

    /// Running until it is told to quit.
    struct Quits(Mutex<bool>);
    #[async_trait]
    impl Osascript for Quits {
        async fn osascript(&self, lines: &[String]) -> anyhow::Result<(bool, String)> {
            if lines.iter().any(|l| l.contains(" to quit")) {
                *self.0.lock() = true;
                return Ok((true, String::new()));
            }
            Ok((true, if *self.0.lock() { "false" } else { "true" }.to_string()))
        }
    }
    let osa = Quits(Mutex::new(false));
    assert_eq!(quit_app_with(&osa, "Slack", 2.0).await.unwrap(), "Quit Slack.");
}

#[test]
fn test_quote_and_the_keep_list() {
    assert_eq!(quote(r#"My "App"\x"#), r#""My \"App\"\\x""#);
    let keep = quit_keep(Some(&HashMap::from([("EVE_QUIT_ALL_KEEP".to_string(), " Slack ,,Mail".to_string())])));
    assert!(keep.contains("slack") && keep.contains("mail") && keep.contains("finder"));
    assert_eq!(keep.len(), QUIT_ALL_KEEP.len() + 2);
}

// ---- test_desk_lane.py: the lane in front of Codex --------------------------------------------------------------

struct DeskRig {
    agent: Arc<ReflexFirstAgent>,
    inner: Arc<Inner>,
    asked: Arc<Mutex<Vec<String>>>,
    events: Events,
    made: Arc<Mutex<Vec<i32>>>,
}

fn desk_rig(lane_result: Option<Value>, raises: bool, cancel_during: bool) -> DeskRig {
    let events: Events = Arc::default();
    let asked: Arc<Mutex<Vec<String>>> = Arc::default();
    let made: Arc<Mutex<Vec<i32>>> = Arc::default();
    let inner = Inner::new();
    let holder: Arc<Mutex<Weak<ReflexFirstAgent>>> = Arc::new(Mutex::new(Weak::new()));
    let (asked2, holder2) = (Arc::clone(&asked), Arc::clone(&holder));
    let lane: Option<Map<String, Value>> = lane_result.map(|v| match v {
        Value::Object(m) => m,
        _ => Map::new(),
    });
    let desk: Desk = Arc::new(move |goal: String| {
        asked2.lock().push(goal);
        if cancel_during {
            if let Some(agent) = holder2.lock().upgrade() {
                agent.cancel("owner said stop");
            }
        }
        let lane = lane.clone();
        Box::pin(async move {
            if raises {
                return Err(anyhow::anyhow!("ax down"));
            }
            Ok(lane)
        })
    });
    let (made2, i) = (Arc::clone(&made), Arc::clone(&inner));
    let make: MakeAgent = Arc::new(move || {
        made2.lock().push(1);
        Arc::clone(&i) as Arc<dyn Agent>
    });
    let agent = Arc::new(ReflexFirstAgent::new(
        make,
        recorder(&events),
        ReflexOptions { apps: apps_of(&APPS), opener: ok_opener(), desk: Some(desk), ..Default::default() },
    ));
    *holder.lock() = Arc::downgrade(&agent);
    DeskRig { agent, inner, asked, events, made }
}

fn complete() -> Value {
    json!({"status": "complete", "reason": "", "clicked": ["Week"], "already": [], "sentence": "Done. I clicked Week."})
}

#[tokio::test]
async fn test_a_complete_lane_answers_and_codex_never_starts() {
    let r = desk_rig(Some(complete()), false, false);
    assert_eq!(r.agent.run("switch to week view", "").await.unwrap(), "Done. I clicked Week.");
    assert_eq!(*r.asked.lock(), strs(&["switch to week view"]));
    assert!(r.made.lock().is_empty() && r.inner.goals().is_empty());
    assert_eq!(*r.events.lock(), vec![ev("started", "switch to week view"), ev("final", "Done. I clicked Week.")]);
    assert_eq!(r.agent.body(), "desk");
    assert_eq!(r.agent.provider(), "desk lane");
    assert_eq!(r.agent.reflexed().as_deref(), Some("desk"));
    assert!(!r.agent.running());
}

#[tokio::test]
async fn test_a_partial_lane_hands_codex_the_task_and_what_was_clicked() {
    let lane = json!({"status": "partial", "reason": "unverified", "clicked": ["Year"], "already": [], "sentence": ""});
    let r = desk_rig(Some(lane), false, false);
    assert_eq!(r.agent.run("year view and then next year", "\n\n(file hint)").await.unwrap(), "codex did it");
    let goals = r.inner.goals();
    assert_eq!(goals.len(), 1);
    let goal = &goals[0];
    assert!(goal.starts_with(
        "year view and then next year\n\nBefore you started, eve's fast lane already clicked 'Year'"
    ));
    assert!(goal.contains("do not click those again") && goal.ends_with("(file hint)"));
}

#[tokio::test]
async fn test_a_lane_that_did_nothing_leaves_the_task_to_codex_unchanged() {
    for lane in [
        None,
        Some(json!({"status": "none", "reason": "no_match", "clicked": []})),
        Some(json!({"status": "refused", "reason": "question"})),
        Some(json!({"status": "unavailable", "reason": "off"})),
    ] {
        let r = desk_rig(lane, false, false);
        assert_eq!(r.agent.run("find the email from Sam", "").await.unwrap(), "codex did it");
        assert_eq!(r.inner.goals(), strs(&["find the email from Sam"]));
    }
}

#[tokio::test]
async fn test_a_lane_that_raises_never_costs_the_task() {
    let r = desk_rig(None, true, false);
    assert_eq!(r.agent.run("switch to week view", "").await.unwrap(), "codex did it");
    assert_eq!(r.inner.goals(), strs(&["switch to week view"]));
}

#[tokio::test]
async fn test_a_stop_while_the_lane_runs_stops_before_codex() {
    let lane = json!({"status": "none", "reason": "no_match", "clicked": []});
    let r = desk_rig(Some(lane), false, true);
    assert_eq!(r.agent.run("switch to week view", "").await.unwrap(), "Stopped.");
    assert!(r.made.lock().is_empty());
}

#[tokio::test]
async fn test_a_bare_launch_is_still_the_launch_reflex_and_never_the_lane() {
    let r = desk_rig(Some(complete()), false, false);
    assert_eq!(r.agent.run("open Spotify", "").await.unwrap(), "Opened Spotify.");
    assert!(r.asked.lock().is_empty());
}

#[tokio::test]
async fn test_a_complete_lane_without_a_sentence_is_not_an_answer() {
    let mut lane = complete();
    lane["sentence"] = json!("");
    let r = desk_rig(Some(lane), false, false);
    assert_eq!(r.agent.run("switch to week view", "").await.unwrap(), "codex did it");
}

// ---- test_browser_router.py: the wiring -------------------------------------------------------------------------

#[tokio::test]
async fn test_the_wrapper_sends_a_routed_task_to_its_named_body_and_keeps_reflexes_first() {
    let (codex, chrome, reader) =
        (Inner::named("codex", "codex"), Inner::named("chrome-lane", "chrome-lane"), Inner::named("web", "web"));
    let routed: Arc<Mutex<Vec<String>>> = Arc::default();
    let agent = || {
        let routed = Arc::clone(&routed);
        let route_body: RouteBody = Arc::new(move |goal: String| {
            routed.lock().push(goal.clone());
            Box::pin(async move {
                Ok(if goal.contains("compare") {
                    "web"
                } else if goal.contains("gmail") {
                    "chrome"
                } else {
                    "codex"
                }
                .to_string())
            })
        });
        ReflexFirstAgent::new(
            make(&codex),
            Arc::new(|_ev| {}),
            ReflexOptions {
                apps: apps_of(&["Spotify"]),
                opener: ok_opener(),
                make_auto: Some(make(&chrome)),
                route_body: Some(route_body),
                bodies: HashMap::from([("web".to_string(), make(&reader))]),
                ..Default::default()
            },
        )
    };
    // the reflex never asks the router
    assert_eq!(agent().run("open Spotify", "").await.unwrap(), "Opened Spotify.");
    let a = agent();
    assert_eq!(a.run("compare three laptops", "").await.unwrap(), "web");
    assert_eq!(a.provider(), "web");
    // any other name: make_auto
    assert_eq!(agent().run("check my gmail", "").await.unwrap(), "chrome-lane");
    assert_eq!(agent().run("rename a file", "").await.unwrap(), "codex");
    assert_eq!(*routed.lock(), strs(&["compare three laptops", "check my gmail", "rename a file"]));
}

#[tokio::test]
async fn test_named_bodies_alone_are_enough_and_a_broken_router_keeps_codex() {
    let (codex, reader) = (Inner::named("codex", "codex"), Inner::named("web", "web"));
    let with_router = |route_body: RouteBody| {
        ReflexFirstAgent::new(
            make(&codex),
            Arc::new(|_ev| {}),
            ReflexOptions {
                apps: apps_of(&[]),
                route_body: Some(route_body),
                bodies: HashMap::from([("web".to_string(), make(&reader))]),
                ..Default::default()
            },
        )
    };
    let only = with_router(Arc::new(|_g: String| Box::pin(async { Ok("web".to_string()) })));
    assert_eq!(only.run("weather in denver", "").await.unwrap(), "web");

    // an unknown name without make_auto
    let no_auto = with_router(Arc::new(|_g: String| Box::pin(async { Ok("somewhere-else".to_string()) })));
    assert_eq!(no_auto.run("x", "").await.unwrap(), "codex");

    let broken = with_router(Arc::new(|_g: String| Box::pin(async { Err(anyhow::anyhow!("router down")) })));
    assert_eq!(broken.run("compare laptops", "").await.unwrap(), "codex");
}

#[tokio::test]
async fn test_a_router_without_a_body_to_choose_is_never_asked() {
    let codex = Inner::named("codex", "codex");
    let asked: Arc<Mutex<Vec<String>>> = Arc::default();
    let a2 = Arc::clone(&asked);
    let route_body: RouteBody = Arc::new(move |goal: String| {
        a2.lock().push(goal);
        Box::pin(async { Ok("web".to_string()) })
    });
    let agent = ReflexFirstAgent::new(
        make(&codex),
        Arc::new(|_ev| {}),
        ReflexOptions { apps: apps_of(&[]), route_body: Some(route_body), ..Default::default() },
    );
    assert_eq!(agent.run("compare laptops", "").await.unwrap(), "codex");
    assert!(asked.lock().is_empty());
    assert_eq!(agent.body(), "codex");
}

#[tokio::test]
async fn test_status_before_any_handoff_and_steer_without_a_body() {
    let r = rig();
    assert!(!r.agent.steer("faster"));
    assert_eq!(
        Value::Object(r.agent.status()),
        json!({"running": false, "provider": "codex", "goal": "", "reflex": null})
    );
    r.agent.run("open Spotify", "").await.unwrap();
    assert_eq!(
        Value::Object(r.agent.status()),
        json!({"running": false, "provider": "codex", "goal": "", "reflex": "Spotify"})
    );
    r.agent.run("play jazz on spotify", "").await.unwrap();
    assert_eq!(Value::Object(r.agent.status()), json!({"running": false, "provider": "codex"}));
    assert!(r.agent.inner().unwrap().as_any().downcast_ref::<Inner>().is_some());
}
