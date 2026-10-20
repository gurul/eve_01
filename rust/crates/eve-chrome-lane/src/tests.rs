//! Port of `bridge/tests/test_chrome_lane.py`: plan once in the owner's Chrome, Codex for whatever is left.

use super::*;
use std::sync::atomic::AtomicI64;
use tokio::sync::Notify;

type Events = Arc<Mutex<Vec<(String, String)>>>;

/// What `run_in_browser` gives back: the planner's (answer, note), or the error it raises.
#[derive(Clone)]
enum Outcome {
    Done(&'static str, &'static str),
    Fails(&'static str),
}

struct TestPlanner {
    result: Outcome,
    on_event: OnEvent,
    goals: Mutex<Vec<String>>,
    cancelled: Mutex<String>,
}

impl LaneBody for TestPlanner {
    fn cancel(&self, reason: &str) {
        *self.cancelled.lock() = reason.to_string();
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait]
impl Planner for TestPlanner {
    async fn run_in_browser(&self, goal: &str) -> anyhow::Result<(String, String)> {
        self.goals.lock().push(goal.to_string());
        (self.on_event)(AgentEvent::with_text("started", goal));
        (self.on_event)(AgentEvent::with_text("progress", "opened gmail"));
        match &self.result {
            Outcome::Fails(msg) => Err(anyhow::anyhow!(*msg)),
            Outcome::Done(a, n) => Ok((a.to_string(), n.to_string())),
        }
    }
}

#[derive(Default)]
struct Codex {
    goals: Mutex<Vec<String>>,
    shot: Mutex<Option<BrowserScreenshot>>,
    cancelled: Mutex<Option<String>>,
}

impl LaneBody for Codex {
    fn cancel(&self, reason: &str) {
        *self.cancelled.lock() = Some(reason.to_string());
    }
    fn browser_screenshot(&self) -> Option<BrowserScreenshot> {
        self.shot.lock().clone()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait]
impl Fallback for Codex {
    async fn run(&self, goal: &str) -> anyhow::Result<String> {
        self.goals.lock().push(goal.to_string());
        Ok("codex finished it".into())
    }
}

#[derive(Default)]
struct Made {
    planner: Mutex<Option<Arc<TestPlanner>>>,
    codex: Mutex<Option<Arc<Codex>>>,
    order: Mutex<Vec<&'static str>>,
}

impl Made {
    fn planner(&self) -> Arc<TestPlanner> {
        self.planner.lock().clone().expect("a planner was made")
    }
    fn codex(&self) -> Option<Arc<Codex>> {
        self.codex.lock().clone()
    }
    fn codex_goals(&self) -> Vec<String> {
        self.codex().expect("codex was made").goals.lock().clone()
    }
}

fn ask() -> AskUser {
    Arc::new(|_q: String| Box::pin(async { "yes".to_string() }))
}

fn rig_with(result: Outcome, options: ChromeLaneOptions) -> (Arc<ChromeLaneAgent>, Arc<Made>, Events) {
    let events: Events = Arc::default();
    let made = Arc::new(Made::default());
    let m1 = made.clone();
    let make_planner: MakePlanner = Box::new(move |on_event, _ask| {
        let p = Arc::new(TestPlanner {
            result: result.clone(),
            on_event,
            goals: Mutex::default(),
            cancelled: Mutex::default(),
        });
        *m1.planner.lock() = Some(p.clone());
        p
    });
    let m2 = made.clone();
    let make_fallback: MakeFallback = Box::new(move || {
        m2.order.lock().push("codex");
        let c = Arc::new(Codex::default());
        *m2.codex.lock() = Some(c.clone());
        c
    });
    let ev = events.clone();
    let on_event: OnEvent =
        Arc::new(move |e: AgentEvent| ev.lock().push((e.kind().to_string(), e.text().to_string())));
    let agent = ChromeLaneAgent::with_options(make_planner, make_fallback, on_event, ask(), options);
    (Arc::new(agent), made, events)
}

fn rig(result: Outcome) -> (Arc<ChromeLaneAgent>, Arc<Made>, Events) {
    rig_with(result, ChromeLaneOptions::default())
}

fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter().map(|(k, t)| (k.to_string(), t.to_string())).collect()
}

fn prepare_sleeping(secs: u64) -> Prepare {
    Arc::new(move |_goal: String| {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_secs(secs)).await; // the owner has not answered yet
            Ok(())
        })
    })
}

#[tokio::test]
async fn test_a_finished_plan_answers_and_codex_never_starts() {
    let (agent, made, events) = rig(Outcome::Done("You have 3 unread emails.", ""));
    assert_eq!(agent.run("how many unread emails do I have").await.unwrap(), "You have 3 unread emails.");
    assert!(made.codex().is_none() && !agent.handed_on());
    // the planner's own start is not repeated
    assert_eq!(
        *events.lock(),
        pairs(&[
            ("started", "how many unread emails do I have"),
            ("progress", "opened gmail"),
            ("final", "You have 3 unread emails."),
        ])
    );
    assert_eq!(agent.final_answer(), "You have 3 unread emails.");
    assert_eq!(agent.goal(), "how many unread emails do I have");
}

#[tokio::test]
async fn test_part_done_hands_codex_the_goal_and_what_was_done() {
    let (agent, made, events) = rig(Outcome::Done("", "\n\n[note] a plan already did: open gmail."));
    assert_eq!(agent.run("archive the newsletters").await.unwrap(), "codex finished it");
    assert_eq!(made.codex_goals(), vec!["archive the newsletters\n\n[note] a plan already did: open gmail."]);
    assert!(agent.handed_on());
    assert!(events.lock().iter().any(|(k, t)| k == "progress" && t.contains("Codex is finishing")));
    assert_eq!(agent.final_answer(), "codex finished it");
}

#[tokio::test]
async fn test_nothing_done_gives_codex_the_whole_task() {
    let (agent, made, events) = rig(Outcome::Done("", ""));
    agent.run("reply to Sam").await.unwrap();
    assert_eq!(made.codex_goals(), vec!["reply to Sam"]);
    assert!(!events.lock().iter().any(|(_, t)| t == PART_DONE_PROGRESS));
}

#[tokio::test]
async fn test_a_broken_lane_never_costs_the_task() {
    let (agent, made, _) = rig(Outcome::Fails("Chrome went away"));
    assert_eq!(agent.run("check my orders").await.unwrap(), "codex finished it");
    assert_eq!(made.codex_goals(), vec!["check my orders"]);
}

#[tokio::test]
async fn test_the_owners_no_is_the_answer_and_codex_does_not_retry_it() {
    let (agent, made, _) = rig(Outcome::Done("Okay, I stopped before that: click Place order.", ""));
    assert_eq!(agent.run("buy the batteries").await.unwrap(), "Okay, I stopped before that: click Place order.");
    assert!(made.codex().is_none());
}

#[tokio::test]
async fn test_a_cancel_stops_without_handing_on() {
    let (agent, made, events) = rig(Outcome::Done("", "\n\n[note] something"));
    agent.cancel("stopped from Telegram"); // as cancel() does while the lane runs
    assert_eq!(agent.run("anything").await.unwrap(), "Stopped.");
    assert!(made.codex().is_none());
    assert_eq!(events.lock().last().unwrap().0, "cancelled");
}

#[tokio::test]
async fn test_stop_while_waiting_for_chromes_allow_is_immediate() {
    let (agent, made, events) = rig_with(
        Outcome::Done("never", ""),
        ChromeLaneOptions { prepare: Some(prepare_sleeping(30)), ..Default::default() },
    );
    let t = Instant::now();
    let a = agent.clone();
    let task = tokio::spawn(async move { a.run("check my gmail").await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    agent.cancel("stopped from Telegram");
    let said = task.await.unwrap().unwrap();
    assert_eq!(said, "Stopped.");
    assert!(t.elapsed() < Duration::from_secs(1));
    assert!(made.codex().is_none());
    assert_eq!(events.lock().last().unwrap().0, "cancelled");
    assert!(made.planner().goals.lock().is_empty()); // the plan never started
    assert_eq!(*made.planner().cancelled.lock(), "stopped from Telegram");
}

// ---- the browser picture ---------------------------------------------------------------------------------

#[tokio::test]
async fn test_a_lane_task_pictures_eves_tab_never_the_desktop() {
    let tab: LaneScreenshot = Arc::new(|| Box::pin(async { Some(b"JPEGBYTES".to_vec()) }));
    let (agent, _, _) =
        rig_with(Outcome::Done("Done.", ""), ChromeLaneOptions { lane_screenshot: Some(tab), ..Default::default() });
    agent.run("open my github").await.unwrap();
    assert_eq!(
        agent.browser_shot().await,
        Some((b"JPEGBYTES".to_vec(), ".jpg".to_string(), "Your Chrome: eve's tab".to_string()))
    );
}

#[tokio::test]
async fn test_an_empty_lane_picture_is_no_picture() {
    let tab: LaneScreenshot = Arc::new(|| Box::pin(async { Some(Vec::new()) }));
    let (agent, _, _) =
        rig_with(Outcome::Done("Done.", ""), ChromeLaneOptions { lane_screenshot: Some(tab), ..Default::default() });
    agent.run("open my github").await.unwrap();
    assert_eq!(agent.browser_shot().await, None);
    let (bare, _, _) = rig(Outcome::Done("Done.", ""));
    assert_eq!(bare.browser_shot().await, None);
}

#[tokio::test]
async fn test_after_codex_took_over_the_picture_is_codexs_tab() {
    let (agent, made, _) = rig(Outcome::Done("", ""));
    agent.run("reply to Sam").await.unwrap();
    let codex = made.codex().unwrap();
    *codex.shot.lock() = Some(BrowserScreenshot { data: b"CODEXTAB".to_vec(), suffix: ".png".into() });
    let (data, suffix, caption) = agent.browser_shot().await.unwrap();
    assert_eq!((data.as_slice(), suffix.as_str()), (&b"CODEXTAB"[..], ".png"));
    assert_eq!(caption, "The browser tab Codex worked in (last captured view)");
    *codex.shot.lock() = None;
    assert_eq!(agent.browser_shot().await, None);
}

// ---- the "nothing done yet" budget ----------------------------------------------------------------------

/// A planner that never gets a plan going (a slow plan call, a lane that hangs) until it is cancelled.
struct StuckPlanner {
    plans_started: AtomicI64,
    cancelled: Mutex<String>,
    stop: Notify,
    working: bool,
}

impl LaneBody for StuckPlanner {
    fn cancel(&self, reason: &str) {
        *self.cancelled.lock() = reason.to_string();
        self.stop.notify_one();
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait]
impl Planner for StuckPlanner {
    async fn run_in_browser(&self, _goal: &str) -> anyhow::Result<(String, String)> {
        if self.working {
            // A plan starts executing at once and takes longer than the budget to finish.
            tokio::time::sleep(Duration::from_millis(20)).await;
            self.plans_started.fetch_add(1, Ordering::SeqCst); // BrowserLane.run_plan: a plan is executing
            tokio::time::sleep(Duration::from_millis(400)).await;
            return Ok(("You have 3 unread emails.".into(), String::new()));
        }
        self.stop.notified().await;
        Ok((format!("Stopped: {}.", self.cancelled.lock()), String::new()))
    }
    fn plans_started(&self) -> i64 {
        self.plans_started.load(Ordering::SeqCst)
    }
}

struct BudgetMade {
    planner: Mutex<Option<Arc<StuckPlanner>>>,
    codex: Mutex<Option<Arc<Codex>>>,
}

fn budget_rig(working: bool, prepare: Option<Prepare>) -> (Arc<ChromeLaneAgent>, Arc<BudgetMade>) {
    let made = Arc::new(BudgetMade { planner: Mutex::new(None), codex: Mutex::new(None) });
    let m1 = made.clone();
    let make_planner: MakePlanner = Box::new(move |_on_event, _ask| {
        let p = Arc::new(StuckPlanner {
            plans_started: AtomicI64::new(0),
            cancelled: Mutex::default(),
            stop: Notify::new(),
            working,
        });
        *m1.planner.lock() = Some(p.clone());
        p
    });
    let m2 = made.clone();
    let make_fallback: MakeFallback = Box::new(move || {
        let c = Arc::new(Codex::default());
        *m2.codex.lock() = Some(c.clone());
        c
    });
    let options = ChromeLaneOptions { prepare, idle_budget_secs: Some(0.1), ..Default::default() };
    let agent = ChromeLaneAgent::with_options(make_planner, make_fallback, Arc::new(|_| {}), ask(), options);
    (Arc::new(agent), made)
}

async fn timed(agent: &ChromeLaneAgent, goal: &str) -> (String, Duration) {
    let t = Instant::now();
    let said = agent.run(goal).await.unwrap();
    (said, t.elapsed())
}

/// Production, 2026-09-23/24: runs that did nothing still spent 11.3 s and 25.6 s before Codex started.
#[tokio::test]
async fn test_a_lane_with_nothing_done_hands_codex_the_task_within_the_budget() {
    let (agent, made) = budget_rig(false, None);
    let (said, secs) = timed(&agent, "open that map link").await;
    assert_eq!(said, "codex finished it");
    assert_eq!(*made.codex.lock().as_ref().unwrap().goals.lock(), vec!["open that map link"]);
    assert!(secs < Duration::from_secs(1));
    let planner = made.planner.lock().clone().unwrap();
    assert!(planner.cancelled.lock().starts_with("no progress"));
    assert_eq!(*planner.cancelled.lock(), "no progress in 0 s");
    assert!(agent.handed_on());
}

/// Production, 2026-09-23: the lane waited 36 s on Chrome's Allow until the owner pressed Stop.
#[tokio::test]
async fn test_waiting_on_chromes_allow_past_the_budget_hands_codex_the_task() {
    let (agent, made) = budget_rig(false, Some(prepare_sleeping(30)));
    let (said, secs) = timed(&agent, "check my gmail").await;
    assert_eq!(said, "codex finished it");
    assert_eq!(*made.codex.lock().as_ref().unwrap().goals.lock(), vec!["check my gmail"]);
    assert!(secs < Duration::from_secs(1));
}

#[tokio::test]
async fn test_a_plan_under_way_is_never_cut_off_by_the_budget() {
    let (agent, made) = budget_rig(true, None);
    let (said, secs) = timed(&agent, "how many unread emails do I have").await;
    assert_eq!(said, "You have 3 unread emails.");
    assert!(made.codex.lock().is_none());
    assert_eq!(*made.planner.lock().as_ref().unwrap().cancelled.lock(), "");
    assert!(secs >= Duration::from_millis(400));
}

#[tokio::test]
async fn test_codex_gets_the_owners_chrome_ready_before_it_takes_over() {
    let made_slot: Arc<Mutex<Option<Arc<Made>>>> = Arc::default();
    let slot = made_slot.clone();
    let ready: BeforeFallback = Arc::new(move || {
        let slot = slot.clone();
        Box::pin(async move {
            slot.lock().as_ref().unwrap().order.lock().push("ready");
            Ok(())
        })
    });
    let (agent, made, _) = rig_with(
        Outcome::Done("", " Already done: opened the login page."),
        ChromeLaneOptions { before_fallback: Some(ready), ..Default::default() },
    );
    *made_slot.lock() = Some(made.clone());
    assert_eq!(agent.run("log in to turnitin").await.unwrap(), "codex finished it");
    assert_eq!(*made.order.lock(), vec!["ready", "codex"]);
}

#[tokio::test]
async fn test_a_failed_chrome_start_still_hands_codex_the_task() {
    let broken: BeforeFallback = Arc::new(|| {
        Box::pin(async { Err(std::io::Error::other("open failed").into()) })
    });
    let (agent, _, _) =
        rig_with(Outcome::Done("", ""), ChromeLaneOptions { before_fallback: Some(broken), ..Default::default() });
    assert_eq!(agent.run("log in to turnitin").await.unwrap(), "codex finished it");
}

#[tokio::test]
async fn test_a_finished_plan_never_starts_the_owners_chrome() {
    let called: Arc<Mutex<Vec<&'static str>>> = Arc::default();
    let c = called.clone();
    let ready: BeforeFallback = Arc::new(move || {
        let c = c.clone();
        Box::pin(async move {
            c.lock().push("ready");
            Ok(())
        })
    });
    let (agent, _, _) =
        rig_with(Outcome::Done("done", ""), ChromeLaneOptions { before_fallback: Some(ready), ..Default::default() });
    assert_eq!(agent.run("x").await.unwrap(), "done");
    assert!(called.lock().is_empty());
}

/// A cancel() during _before_fallback only reached the finished planner; Codex then ran the whole task.
#[tokio::test]
async fn test_a_stop_while_the_owners_chrome_gets_ready_never_starts_codex() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (e, r) = (entered.clone(), release.clone());
    let ready: BeforeFallback = Arc::new(move || {
        let (e, r) = (e.clone(), r.clone());
        Box::pin(async move {
            e.notify_one();
            r.notified().await;
            Ok(())
        })
    });
    let (agent, made, events) =
        rig_with(Outcome::Done("", ""), ChromeLaneOptions { before_fallback: Some(ready), ..Default::default() });
    let a = agent.clone();
    let task = tokio::spawn(async move { a.run("log in to turnitin").await });
    entered.notified().await;
    agent.cancel("stopped from Telegram");
    release.notify_one();
    assert_eq!(task.await.unwrap().unwrap(), "Stopped.");
    assert!(made.codex().is_none());
    assert_eq!(events.lock().last().unwrap(), &("cancelled".to_string(), "Stopped.".to_string()));
}

// ---- the contract every door uses -------------------------------------------------------------------------

struct Steerable {
    steered: Mutex<Vec<String>>,
}

impl LaneBody for Steerable {
    fn running(&self) -> bool {
        true
    }
    fn status(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("provider".into(), Value::String("inner".into()));
        m.insert("step".into(), Value::from(3));
        m
    }
    fn steer(&self, text: &str) -> bool {
        self.steered.lock().push(text.to_string());
        true
    }
    fn cancel(&self, _reason: &str) {}
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait]
impl Planner for Steerable {
    async fn run_in_browser(&self, _goal: &str) -> anyhow::Result<(String, String)> {
        Ok(("ok".into(), String::new()))
    }
}

#[tokio::test]
async fn test_status_steer_and_running_reach_whoever_runs() {
    let (agent, _, _) = rig(Outcome::Done("x", ""));
    // before any run: nothing to steer, not running, the agent's own keys only
    assert!(!agent.steer("left"));
    assert!(!agent.running());
    assert_eq!(serde_json::to_string(&agent.status()).unwrap(), r#"{"provider":"chrome-lane","handed_on":false}"#);
    assert_eq!(agent.provider(), "chrome-lane");
    assert!(agent.browser_used());
    assert!(agent.current().is_none());

    let make_planner: MakePlanner = Box::new(|_, _| Arc::new(Steerable { steered: Mutex::default() }));
    let make_fallback: MakeFallback = Box::new(|| Arc::new(Codex::default()));
    let agent = ChromeLaneAgent::new(make_planner, make_fallback, Arc::new(|_| {}), ask());
    agent.run("y").await.unwrap();
    assert!(agent.running() && agent.steer("go left"));
    // `{**inner, "provider": ..., "handed_on": ...}`: the inner key keeps its place, the value is ours
    assert_eq!(
        serde_json::to_string(&agent.status()).unwrap(),
        r#"{"provider":"chrome-lane","step":3,"handed_on":false}"#
    );
    let Some(Body::Planner(p)) = agent.current() else { panic!("the planner ran last") };
    let inner = p.as_any().downcast_ref::<Steerable>().unwrap();
    assert_eq!(*inner.steered.lock(), vec!["go left"]);
}

#[tokio::test]
async fn test_a_stop_reaches_codex_once_it_took_over() {
    let (agent, made, _) = rig(Outcome::Done("", ""));
    agent.run("reply to Sam").await.unwrap();
    assert!(agent.handed_on());
    assert!(matches!(agent.current(), Some(Body::Fallback(_))));
    agent.cancel("stopped from the app");
    assert_eq!(made.codex().unwrap().cancelled.lock().as_deref(), Some("stopped from the app"));
    assert_eq!(
        serde_json::to_string(&agent.status()).unwrap(),
        r#"{"provider":"chrome-lane","handed_on":true}"#
    );
}

#[tokio::test]
async fn test_a_failed_connection_hands_codex_the_task() {
    let refused: Prepare =
        Arc::new(|_goal: String| Box::pin(async { Err(anyhow::anyhow!("connection refused\nat port 9222")) }));
    let (agent, made, _) =
        rig_with(Outcome::Done("never", ""), ChromeLaneOptions { prepare: Some(refused), ..Default::default() });
    assert_eq!(agent.run("check my gmail").await.unwrap(), "codex finished it");
    assert_eq!(made.codex_goals(), vec!["check my gmail"]);
    assert!(made.planner().goals.lock().is_empty());
}

#[test]
fn test_helpers_match_python() {
    assert_eq!(first_line("one\ntwo"), "one");
    assert_eq!(first_line("one\r\ntwo"), "one");
    assert_eq!(first_line("a\u{2028}b"), "a");
    assert_eq!(first_line("\n"), "");
    assert_eq!(first_line("plain"), "plain");
    assert_eq!(budget_duration(15.0), Some(Duration::from_secs(15)));
    assert_eq!(budget_duration(-1.0), Some(Duration::ZERO));
    assert_eq!(budget_duration(f64::INFINITY), None);
    assert_eq!(error_kind(&std::io::Error::other("x").into()), "OSError");
    assert_eq!(error_kind(&anyhow::anyhow!("x")), "Error");
    assert_eq!(NOTHING_DONE_BUDGET_SECS, 15.0);
}
