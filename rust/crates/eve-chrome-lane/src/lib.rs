//! The fast path into the owner's logged-in Chrome: plan once, execute in eve's own tab, Codex as the floor.
//!
//! Port of `eve_bridge/chrome_lane.py`.
//!
//! Owner, 2026-09-23: "i want eve to be able to control logged in browser, that's the most important".
//! Codex already drives the owner's Chrome; this is the faster road to the same browser. The daemon holds ONE
//! `browser_lane.BrowserLane` in attach mode for its whole life — Chrome asks "Allow remote debugging?" per
//! connection, so one connection means one click per Chrome session, not one per task. For each task:
//!
//! 1. A fresh ComputerAgent (the planner, gpt-6-astra) runs `run_in_browser`: one plan against the page in
//!    eve's tab, executed step by step with Jev grounding each click, a sensitive step (buy, send, delete…)
//!    stopping for the owner's yes (consent.py), and a code check of each step's expected result.
//! 2. Finished: that is the answer. Stopped by the owner's no: that is the answer.
//! 3. Part done: the rest goes to Codex with a note of what was already done, so nothing is repeated.
//!    Nothing done (no plan, the lane unreachable, the owner not clicking Allow): Codex takes the whole task,
//!    exactly as before this module. "Nothing done" has a clock: when no plan is executing and no step has been
//!    reported [`NOTHING_DONE_BUDGET_SECS`] after the start, the lane is stopped and Codex takes the task then. A
//!    plan that has started runs to its end.
//!
//! [`ChromeLaneAgent`] keeps the run/steer/cancel/status contract every door already uses, so it drops in behind
//! `app_reflex.ReflexFirstAgent` as its second body (`make_auto`) with no change to the voice or Telegram doors.
//!
//! Python duck-types the planner (a `ComputerAgent`) and the fallback (Codex). Here they are the [`Planner`] and
//! [`Fallback`] traits, both built on [`LaneBody`], the part of the contract the two share.

use std::any::Any;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::future::{AbortHandle, Abortable, BoxFuture};
use parking_lot::Mutex;
use serde_json::{Map, Value};

pub use eve_agent_contract::AgentEvent;

/// How long the lane may go with nothing done (no plan executing, no step reported) before Codex takes the task.
/// Production, 2026-09-23/24: once connected, the lane reached a plan's first step (or gave up) 4-6 s later; the
/// runs that did nothing still spent 11.3 s and 25.6 s before handing on (19 s of it one profile lookup, now
/// budgeted at 3 s), and one waited 36 s on Chrome's Allow until the owner pressed Stop. 15 s is about twice
/// the slowest healthy path; a plan that has started is never cut off.
pub const NOTHING_DONE_BUDGET_SECS: f64 = 15.0;

/// The progress line the owner hears when the lane did part of the task and Codex finishes it.
pub const PART_DONE_PROGRESS: &str = "Part of it is done in your Chrome; Codex is finishing it.";

/// A captured browser picture as Codex keeps it (`browser_screenshot` with `.data` and `.suffix`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserScreenshot {
    pub data: Vec<u8>,
    pub suffix: String,
}

/// The run/steer/cancel/status contract the planner and Codex share. The defaults are what Python's
/// `getattr(..., default)` / `hasattr` reads give for an agent that lacks the attribute.
pub trait LaneBody: Send + Sync {
    /// `running`; `False` when the agent has none.
    fn running(&self) -> bool {
        false
    }
    /// `status()`; `{}` when the agent has none.
    fn status(&self) -> Map<String, Value> {
        Map::new()
    }
    /// `steer(text)`: whether the steer was taken.
    fn steer(&self, _text: &str) -> bool {
        false
    }
    /// `cancel(reason=...)`.
    fn cancel(&self, reason: &str);
    /// `browser_screenshot`: the last captured browser view, if the agent keeps one.
    fn browser_screenshot(&self) -> Option<BrowserScreenshot> {
        None
    }
    /// The concrete agent, for the attributes Python reaches through `__getattr__` (`ui_evidence` …).
    fn as_any(&self) -> &dyn Any;
}

/// The planner (`ComputerAgent` with a browser): one plan in eve's tab of the owner's Chrome.
#[async_trait]
pub trait Planner: LaneBody {
    /// `run_in_browser(goal)` → `(answer, note)`: an answer when finished (or stopped by the owner's no), else
    /// `""` and a note of what was already done (`""` when nothing was).
    async fn run_in_browser(&self, goal: &str) -> anyhow::Result<(String, String)>;
    /// `planner.browser.plans_started`: how many plans `BrowserLane.run_plan` has started executing; `0` when the
    /// planner has no browser.
    fn plans_started(&self) -> i64 {
        0
    }
}

/// The floor (Codex): takes the whole task, or the rest of it with a note of what was done.
#[async_trait]
pub trait Fallback: LaneBody {
    async fn run(&self, goal: &str) -> anyhow::Result<String>;
}

/// Whoever is running the task now: the planner, then (maybe) Codex.
#[derive(Clone)]
pub enum Body {
    Planner(Arc<dyn Planner>),
    Fallback(Arc<dyn Fallback>),
}

impl Body {
    pub fn as_lane_body(&self) -> &dyn LaneBody {
        match self {
            Body::Planner(p) => p.as_ref(),
            Body::Fallback(f) => f.as_ref(),
        }
    }
}

/// `on_event(AgentEvent)`.
pub type OnEvent = Arc<dyn Fn(AgentEvent) + Send + Sync>;
/// `ask_user(question) -> answer`.
pub type AskUser = Arc<dyn Fn(String) -> BoxFuture<'static, String> + Send + Sync>;
/// `make_planner(on_event, ask_user)`.
pub type MakePlanner = Box<dyn Fn(OnEvent, AskUser) -> Arc<dyn Planner> + Send + Sync>;
/// `make_fallback()`.
pub type MakeFallback = Box<dyn Fn() -> Arc<dyn Fallback> + Send + Sync>;
/// `prepare(goal)`: connect (the owner's Allow) and pick the Chrome profile. An error means Chrome cannot be
/// reached, and Codex takes the task.
pub type Prepare = Arc<dyn Fn(String) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;
/// `lane_screenshot()`: eve's tab in the owner's Chrome, as JPEG bytes.
pub type LaneScreenshot = Arc<dyn Fn() -> BoxFuture<'static, Option<Vec<u8>>> + Send + Sync>;
/// `before_fallback()`: make sure the owner's Chrome is up for Codex's extension.
pub type BeforeFallback = Arc<dyn Fn() -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

/// The keyword arguments of `ChromeLaneAgent.__init__` after `ask_user`.
#[derive(Default, Clone)]
pub struct ChromeLaneOptions {
    pub prepare: Option<Prepare>,
    pub lane_screenshot: Option<LaneScreenshot>,
    /// `None` is [`NOTHING_DONE_BUDGET_SECS`].
    pub idle_budget_secs: Option<f64>,
    pub before_fallback: Option<BeforeFallback>,
}

#[derive(Default)]
struct State {
    current: Option<Body>,
    cancel_reason: Option<String>,
    /// Connecting / waiting on the owner's Allow: cancellable.
    preparing: Option<AbortHandle>,
    goal: String,
    final_: String,
    /// Codex finished what the lane started (or did it all).
    handed_on: bool,
}

/// Plan once in the owner's Chrome (`make_planner().run_in_browser`), Codex (`make_fallback`) for the rest.
///
/// Every method takes `&self`, so a door can hold the agent in an `Arc`, run it on one task and steer or
/// cancel it from another, as the Python doors do on one event loop.
pub struct ChromeLaneAgent {
    make_planner: MakePlanner,
    make_fallback: MakeFallback,
    idle_budget: f64,
    progressed: Arc<AtomicBool>,
    timed_out: AtomicBool,
    prepare: Option<Prepare>,
    lane_screenshot: Option<LaneScreenshot>,
    before_fallback: Option<BeforeFallback>,
    pub on_event: OnEvent,
    pub ask_user: AskUser,
    state: Mutex<State>,
}

impl ChromeLaneAgent {
    pub const PROVIDER: &'static str = "chrome-lane";

    /// `ChromeLaneAgent(make_planner, make_fallback, on_event, ask_user)` with every keyword at its default.
    pub fn new(make_planner: MakePlanner, make_fallback: MakeFallback, on_event: OnEvent, ask_user: AskUser) -> Self {
        Self::with_options(make_planner, make_fallback, on_event, ask_user, ChromeLaneOptions::default())
    }

    pub fn with_options(
        make_planner: MakePlanner,
        make_fallback: MakeFallback,
        on_event: OnEvent,
        ask_user: AskUser,
        options: ChromeLaneOptions,
    ) -> Self {
        Self {
            make_planner,
            make_fallback,
            idle_budget: options.idle_budget_secs.unwrap_or(NOTHING_DONE_BUDGET_SECS),
            progressed: Arc::new(AtomicBool::new(false)),
            timed_out: AtomicBool::new(false),
            prepare: options.prepare,
            lane_screenshot: options.lane_screenshot,
            before_fallback: options.before_fallback,
            on_event,
            ask_user,
            state: Mutex::new(State::default()),
        }
    }

    /// `provider`.
    pub fn provider(&self) -> &'static str {
        Self::PROVIDER
    }

    /// `goal`: the task of the latest `run`.
    pub fn goal(&self) -> String {
        self.state.lock().goal.clone()
    }

    /// `final`: what the latest `run` said.
    pub fn final_answer(&self) -> String {
        self.state.lock().final_.clone()
    }

    /// `handed_on`: Codex finished what the lane started (or did it all).
    pub fn handed_on(&self) -> bool {
        self.state.lock().handed_on
    }

    /// `browser_used`: a Chrome-lane task always used the browser.
    pub fn browser_used(&self) -> bool {
        true
    }

    /// The planner, then (maybe) Codex: who `__getattr__` forwards to. `None` before the first `run`.
    pub fn current(&self) -> Option<Body> {
        self.state.lock().current.clone()
    }

    /// `browser_screenshot`, reached through `__getattr__` on whoever ran last.
    pub fn browser_screenshot(&self) -> Option<BrowserScreenshot> {
        self.current().and_then(|b| b.as_lane_body().browser_screenshot())
    }

    pub fn running(&self) -> bool {
        self.current().is_some_and(|b| b.as_lane_body().running())
    }

    pub fn status(&self) -> Map<String, Value> {
        let current = self.current();
        let mut out = current.map(|b| b.as_lane_body().status()).unwrap_or_default();
        // `{**inner, "provider": ..., "handed_on": ...}`: an existing key keeps its place, as in a dict.
        out.insert("provider".into(), Value::String(Self::PROVIDER.into()));
        out.insert("handed_on".into(), Value::Bool(self.handed_on()));
        out
    }

    pub fn steer(&self, text: &str) -> bool {
        self.current().is_some_and(|b| b.as_lane_body().steer(text))
    }

    pub fn cancel(&self, reason: &str) {
        let (preparing, current) = {
            let mut st = self.state.lock();
            st.cancel_reason = Some(reason.to_string());
            (st.preparing.clone(), st.current.clone())
        };
        if let Some(handle) = preparing {
            handle.abort(); // "stop" while waiting for Chrome's Allow is immediate
        }
        if let Some(body) = current {
            body.as_lane_body().cancel(reason);
        }
    }

    /// (image bytes, suffix, caption) of the browser this task used — never the desktop: eve's tab in the
    /// owner's Chrome (the page itself, full size), or Codex's captured tab once Codex took over. None when
    /// there is none (the caller then says so or falls back).
    pub async fn browser_shot(&self) -> Option<(Vec<u8>, String, String)> {
        if self.handed_on() {
            return self.browser_screenshot().map(|shot| {
                (shot.data, shot.suffix, "The browser tab Codex worked in (last captured view)".to_string())
            });
        }
        if let Some(shoot) = &self.lane_screenshot {
            if let Some(data) = shoot().await {
                if !data.is_empty() {
                    return Some((data, ".jpg".to_string(), "Your Chrome: eve's tab".to_string()));
                }
            }
        }
        None
    }

    fn emit(&self, kind: &str, text: &str) {
        (self.on_event)(AgentEvent::with_text(kind, text));
    }

    /// Run the task. An error is Codex's own, passed through as Python lets its exception propagate; the lane
    /// itself never fails the task.
    pub async fn run(&self, goal: &str) -> anyhow::Result<String> {
        self.state.lock().goal = goal.to_string();
        self.emit("started", goal);
        let t0 = Instant::now();
        let planner = (self.make_planner)(self.forwarder(), self.ask_user.clone());
        self.state.lock().current = Some(Body::Planner(planner.clone()));
        self.progressed.store(false, Ordering::SeqCst);
        self.timed_out.store(false, Ordering::SeqCst);
        let plans_before = planner.plans_started();
        let attempt = self.attempt(planner.clone(), goal);
        tokio::pin!(attempt);
        let early = match budget_duration(self.idle_budget) {
            Some(budget) => tokio::time::timeout(budget, &mut attempt).await.ok(),
            None => Some((&mut attempt).await),
        };
        let (mut answer, mut note) = match early {
            Some(done) => done,
            None => {
                if !self.progressed.load(Ordering::SeqCst) && planner.plans_started() == plans_before {
                    // Nothing done yet (no plan executing, not a step reported): the lane is stuck connecting,
                    // waiting on Chrome's Allow, or planning slowly. Codex takes it now rather than later.
                    self.timed_out.store(true, Ordering::SeqCst);
                    tracing::info!("chrome-lane: nothing done within {:.0} s; Codex takes it", self.idle_budget);
                    let preparing = self.state.lock().preparing.clone();
                    if let Some(handle) = preparing {
                        handle.abort();
                    }
                    planner.cancel(&format!("no progress in {:.0} s", self.idle_budget));
                }
                attempt.await
            }
        };
        if self.timed_out.load(Ordering::SeqCst) {
            // "Stopped: no progress…" is ours, not the owner's answer
            answer.clear();
            note.clear();
        }
        if self.cancel_requested() {
            return Ok(self.finish_cancelled(if answer.is_empty() { "Stopped.".to_string() } else { answer }));
        }
        if !answer.is_empty() {
            tracing::info!("chrome-lane: done in the owner's Chrome in {:.1} s", t0.elapsed().as_secs_f64());
            self.state.lock().final_ = answer.clone();
            self.emit("final", &answer);
            return Ok(answer);
        }
        self.state.lock().handed_on = true;
        tracing::info!(
            "chrome-lane: {} after {:.1} s; Codex takes it",
            if note.is_empty() { "nothing done" } else { "part done" },
            t0.elapsed().as_secs_f64()
        );
        if !note.is_empty() {
            self.emit("progress", PART_DONE_PROGRESS);
        }
        if let Some(ready) = &self.before_fallback {
            // Codex still runs and says what it cannot reach
            if let Err(e) = ready().await {
                tracing::warn!("chrome-lane: getting the owner's Chrome ready failed ({})", e);
            }
        }
        if self.cancel_requested() {
            // A Stop while the owner's Chrome was getting ready reached only the finished planner: Codex
            // must not start the whole task after it.
            return Ok(self.finish_cancelled("Stopped.".to_string()));
        }
        let codex = (self.make_fallback)();
        self.state.lock().current = Some(Body::Fallback(codex.clone()));
        let said = codex.run(&format!("{goal}{note}")).await?;
        self.state.lock().final_ = said.clone();
        Ok(said)
    }

    fn cancel_requested(&self) -> bool {
        self.state.lock().cancel_reason.is_some()
    }

    fn finish_cancelled(&self, said: String) -> String {
        self.state.lock().final_ = said.clone();
        self.emit("cancelled", &said);
        said
    }

    /// Connect, pick the profile, plan and execute: (answer, note), or ("", "") when the lane cannot.
    async fn attempt(&self, planner: Arc<dyn Planner>, goal: &str) -> (String, String) {
        match self.attempt_inner(planner, goal).await {
            Ok(done) => done,
            Err(e) => {
                // the lane never costs the task: Codex takes it
                let text = e.to_string();
                tracing::warn!(
                    "chrome-lane: the lane failed ({}: {}); Codex takes the task",
                    error_kind(&e),
                    first_line(&text).chars().take(300).collect::<String>()
                );
                (String::new(), String::new())
            }
        }
    }

    async fn attempt_inner(&self, planner: Arc<dyn Planner>, goal: &str) -> anyhow::Result<(String, String)> {
        if let Some(prepare) = &self.prepare {
            let (handle, registration) = AbortHandle::new_pair();
            self.state.lock().preparing = Some(handle);
            match Abortable::new(prepare(goal.to_string()), registration).await {
                Ok(connected) => connected?, // an error: Chrome cannot be reached, Codex takes it
                Err(_aborted) => {
                    if self.timed_out.load(Ordering::SeqCst) {
                        return Ok((String::new(), String::new())); // our own budget ran out while connecting
                    }
                    // Only cancel() (which sets the reason first) and our budget abort the connection; a
                    // cancel from outside drops this whole future instead.
                    return Ok(("Stopped.".to_string(), String::new()));
                }
            }
        }
        if self.timed_out.load(Ordering::SeqCst) {
            return Ok((String::new(), String::new())); // the budget ran out as the connection came up
        }
        planner.run_in_browser(goal).await
    }

    /// The planner's own started/final are this agent's to say; its progress and questions pass through.
    fn forwarder(&self) -> OnEvent {
        let progressed = self.progressed.clone();
        let on_event = self.on_event.clone();
        Arc::new(move |ev: AgentEvent| {
            if ev.kind() != "started" && ev.kind() != "final" {
                progressed.store(true, Ordering::SeqCst); // a step done, or a question: the plan is under way
                on_event(ev);
            }
        })
    }
}

/// The idle budget as a timeout: `None` waits without one (an infinite budget), a negative or NaN budget is 0.
fn budget_duration(secs: f64) -> Option<Duration> {
    if secs.is_nan() {
        return Some(Duration::ZERO);
    }
    if secs.is_infinite() && secs > 0.0 {
        return None;
    }
    Duration::try_from_secs_f64(secs.max(0.0)).ok()
}

/// The error's kind for the log line, as `type(e).__name__` names it: the innermost cause's type when it is a
/// standard I/O error, else `Error`.
fn error_kind(e: &anyhow::Error) -> &'static str {
    match e.root_cause().downcast_ref::<std::io::Error>() {
        Some(_) => "OSError",
        None => "Error",
    }
}

/// `text.splitlines()[0]` for a non-empty text: up to the first line break Python's `splitlines` knows.
fn first_line(text: &str) -> &str {
    let end = text
        .find(['\n', '\r', '\u{0b}', '\u{0c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}', '\u{2029}'])
        .unwrap_or(text.len());
    &text[..end]
}

#[cfg(test)]
mod tests;
