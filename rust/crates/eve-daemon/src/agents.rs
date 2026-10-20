//! The computer-use agent factory (`_make_agent`) and its bodies: the reflex in front of Codex, the Chrome lane,
//! the web reader, the desk lane (`_warn_removed_computer`, `_make_chrome_lane`, `_desk`, `_bodies`,
//! `_reader_route`, `_chrome_body`).
//!
//! Python passed one duck-typed agent object through every layer. Here each layer has its own contract (the chat's
//! and the voice's [`TaskAgent`], app_reflex's [`eve_app_reflex::Agent`], chrome_lane's planner and floor), so the
//! adapters between them live here: [`CodexTask`] (Codex as a task), [`TaskBody`] (a task as a reflex body and as
//! the Chrome lane's floor), [`ChromeAuto`] (the Chrome lane as a reflex body), [`PlannerBody`] (the browser
//! planner), and [`ReflexTask`] (the reflex as the task the doors hold). Python's `__getattr__` forwarding
//! (`browser_used`, `browser_screenshot`, `browser_tab_id`, `browser_shot` from whichever body ran) is
//! [`ReflexTask`]'s.

use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;
use serde_json::{Map, Value};

use eve_agent_contract::AgentEvent;
use eve_app_reflex::{Agent as ReflexAgent, MakeAgent, ReflexFirstAgent, ReflexOptions, RouteBody};
use eve_brain::seams::BrowserShot;
use eve_chrome_lane::{BrowserScreenshot, ChromeLaneAgent, ChromeLaneOptions, Fallback, LaneBody, Planner};
use eve_codex_computer::CodexComputerAgent;
use eve_computer_agent::{AgentConfig, ComputerAgent, CreateResponse};

use crate::daemon::Daemon;
use crate::seams::{AskUser, Environ, OnEvent, TaskAgent};

/// The floor a body falls back to (Codex): a [`TaskAgent`] that may also be closed unrun (`discard`, a warm Codex
/// agent's app-server). Python's `getattr(body, "discard", None)`: the default does nothing.
#[async_trait]
pub trait FloorAgent: TaskAgent {
    /// `await agent.discard()`.
    async fn discard(&self) {}
}

/// `make_inner()`: the agent a body wraps (Codex, warm when one is ready), made on the handoff.
pub type MakeInner = Arc<dyn Fn() -> Arc<dyn FloorAgent> + Send + Sync>;

/// `reader_route(goal)`: browser_router's Jev router as `_bodies` calls it (on a thread): "web" or "codex", or the
/// exception's class name when it raised.
pub type ReaderRoute = Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

/// What `_bodies` / `_chrome_body` give `ReflexFirstAgent` beside Codex (Python's dict of keyword arguments):
/// `make_auto` (the Chrome lane), `route_body` (the one router that picks), and `bodies` by name (the web reader).
/// Empty: every task stays Codex's.
#[derive(Clone, Default)]
pub struct Bodies {
    pub make_auto: Option<MakeAgent>,
    pub route_body: Option<RouteBody>,
    pub bodies: HashMap<String, MakeAgent>,
}

impl Bodies {
    /// Python's `{}`.
    pub fn is_empty(&self) -> bool {
        self.make_auto.is_none() && self.route_body.is_none() && self.bodies.is_empty()
    }
}

/// `_warn_removed_computer(environ)`: EVE_COMPUTER / EVE_COMPUTER_AGENT named a body that was removed (Holo, the
/// legacy worker); log once and say so. True when it warned.
///
/// EVE_COMPUTER once chose the desktop executor (Codex or Holo). Holo was removed on 2026-09-30, so Codex is the
/// only one: a leftover EVE_COMPUTER=holo logs one warning and tasks run on Codex.
pub fn warn_removed_computer(environ: Option<&Environ>) -> bool {
    let value = crate::seams::env_get(environ, "EVE_COMPUTER").unwrap_or_default();
    if value.trim().to_lowercase() != "holo" {
        return false;
    }
    tracing::warn!("agent: EVE_COMPUTER=holo is set, but Holo was removed (2026-09-30); tasks run on Codex");
    true
}

// ---- the callbacks' shapes ------------------------------------------------------------------------------------------

/// The doors' `on_event` (by reference) as the bodies take it (by value).
fn owned_events(on_event: &OnEvent) -> Arc<dyn Fn(AgentEvent) + Send + Sync> {
    let on_event = on_event.clone();
    Arc::new(move |e: AgentEvent| on_event(&e))
}

/// A body's `on_event` (by value) as the planner takes it (by reference).
fn borrowed_events(on_event: Arc<dyn Fn(AgentEvent) + Send + Sync>) -> OnEvent {
    Arc::new(move |e: &AgentEvent| on_event(e.clone()))
}

/// The doors' `ask_user` (an error is "no answer") as the bodies that always answer take it: no answer is "".
fn answering(ask_user: &AskUser) -> Arc<dyn Fn(String) -> BoxFuture<'static, String> + Send + Sync> {
    let ask_user = ask_user.clone();
    Arc::new(move |q: String| {
        let fut = ask_user(q);
        Box::pin(async move { fut.await.unwrap_or_default() })
    })
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

// ---- Codex as a task ----------------------------------------------------------------------------------------------

/// Codex computer use (codex_computer.py) as the task the doors and the bodies hold.
pub struct CodexTask(pub CodexComputerAgent);

#[async_trait]
impl TaskAgent for CodexTask {
    /// The reflex hands over `goal + note` (app_reflex.py: `self._inner.run(goal + note)`).
    async fn run(&self, goal: &str, note: &str) -> anyhow::Result<String> {
        Ok(self.0.run(&format!("{goal}{note}")).await?)
    }
    fn cancel(&self, reason: &str) {
        self.0.cancel(reason)
    }
    fn steer(&self, text: &str) -> bool {
        self.0.steer(text)
    }
    fn status(&self) -> Value {
        self.0.status()
    }
    fn provider(&self) -> String {
        self.0.provider().to_string()
    }
    fn running(&self) -> bool {
        self.0.running()
    }
    fn browser_used(&self) -> bool {
        self.0.browser_used()
    }
    fn browser_screenshot(&self) -> Option<(Vec<u8>, String)> {
        self.0.browser_screenshot().map(|shot| (shot.data, shot.suffix))
    }
    /// Python formats `agent.browser_tab_id` into the caption: None reads "None".
    fn browser_tab_id(&self) -> String {
        self.0.browser_tab_id().unwrap_or_else(|| "None".to_string())
    }
}

#[async_trait]
impl FloorAgent for CodexTask {
    async fn discard(&self) {
        self.0.discard().await
    }
}

// ---- a task as a body ---------------------------------------------------------------------------------------------

/// The floor ([`FloorAgent`], Codex in the daemon) as app_reflex's body and as chrome_lane's fallback.
pub struct TaskBody(pub Arc<dyn FloorAgent>);

#[async_trait]
impl ReflexAgent for TaskBody {
    async fn run(&self, goal: &str) -> anyhow::Result<String> {
        self.0.run(goal, "").await
    }
    fn cancel(&self, reason: &str) {
        self.0.cancel(reason)
    }
    fn steer(&self, text: &str) -> bool {
        self.0.steer(text)
    }
    fn status(&self) -> Map<String, Value> {
        obj(self.0.status())
    }
    fn running(&self) -> bool {
        self.0.running()
    }
    fn provider(&self) -> String {
        self.0.provider()
    }
    async fn discard(&self) -> anyhow::Result<()> {
        self.0.discard().await;
        Ok(())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl LaneBody for TaskBody {
    fn running(&self) -> bool {
        self.0.running()
    }
    fn status(&self) -> Map<String, Value> {
        obj(self.0.status())
    }
    fn steer(&self, text: &str) -> bool {
        self.0.steer(text)
    }
    fn cancel(&self, reason: &str) {
        self.0.cancel(reason)
    }
    fn browser_screenshot(&self) -> Option<BrowserScreenshot> {
        self.0.browser_screenshot().map(|(data, suffix)| BrowserScreenshot { data, suffix })
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait]
impl Fallback for TaskBody {
    async fn run(&self, goal: &str) -> anyhow::Result<String> {
        self.0.run(goal, "").await
    }
}

// ---- the Chrome lane's planner ------------------------------------------------------------------------------------

/// `ComputerAgent(create, config=browser_config(cfg), on_event=ev, ask_user=ask, browser=lane)`: one plan in eve's
/// tab of the owner's Chrome.
pub struct PlannerBody {
    pub agent: Arc<ComputerAgent>,
    pub lane: Arc<eve_browser_lane::BrowserLane>,
}

impl LaneBody for PlannerBody {
    fn running(&self) -> bool {
        self.agent.running()
    }
    fn status(&self) -> Map<String, Value> {
        obj(self.agent.status())
    }
    fn steer(&self, text: &str) -> bool {
        self.agent.steer(text)
    }
    fn cancel(&self, reason: &str) {
        self.agent.cancel(reason)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait]
impl Planner for PlannerBody {
    async fn run_in_browser(&self, goal: &str) -> anyhow::Result<(String, String)> {
        self.agent.run_in_browser(goal).await
    }
    fn plans_started(&self) -> i64 {
        i64::try_from(self.lane.plans_started.load(Ordering::SeqCst)).unwrap_or(i64::MAX)
    }
}

// ---- the Chrome lane as a body ------------------------------------------------------------------------------------

/// chrome_lane.ChromeLaneAgent as app_reflex's `make_auto` body.
pub struct ChromeAuto(pub Arc<ChromeLaneAgent>);

#[async_trait]
impl ReflexAgent for ChromeAuto {
    async fn run(&self, goal: &str) -> anyhow::Result<String> {
        self.0.run(goal).await
    }
    fn cancel(&self, reason: &str) {
        self.0.cancel(reason)
    }
    fn steer(&self, text: &str) -> bool {
        self.0.steer(text)
    }
    fn status(&self) -> Map<String, Value> {
        self.0.status()
    }
    fn running(&self) -> bool {
        self.0.running()
    }
    fn provider(&self) -> String {
        self.0.provider().to_string()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---- the reflex as the doors' task ----------------------------------------------------------------------------------

/// app_reflex.ReflexFirstAgent as the task the chat and the voice hold. What Python's `__getattr__` reached on the
/// body that took the task (`browser_used`, `browser_screenshot`, `browser_tab_id`, `browser_shot`) is read off
/// that body here; before the handoff there is none.
pub struct ReflexTask(pub Arc<ReflexFirstAgent>);

/// The floor inside a body, when the body is (or handed on to) one: Codex's captured tab lives there.
fn floor_of(body: &dyn ReflexAgent) -> Option<Arc<dyn FloorAgent>> {
    let any = body.as_any();
    if let Some(task) = any.downcast_ref::<TaskBody>() {
        return Some(task.0.clone());
    }
    if let Some(chrome) = any.downcast_ref::<ChromeAuto>() {
        return match chrome.0.current()? {
            eve_chrome_lane::Body::Fallback(f) => f.as_any().downcast_ref::<TaskBody>().map(|t| t.0.clone()),
            eve_chrome_lane::Body::Planner(_) => None,
        };
    }
    if let Some(reader) = any.downcast_ref::<eve_web::web_reader::WebReaderAgent>() {
        return floor_of(reader.current()?.as_ref());
    }
    None
}

#[async_trait]
impl TaskAgent for ReflexTask {
    async fn run(&self, goal: &str, note: &str) -> anyhow::Result<String> {
        self.0.run(goal, note).await
    }
    fn cancel(&self, reason: &str) {
        self.0.cancel(reason)
    }
    fn steer(&self, text: &str) -> bool {
        self.0.steer(text)
    }
    fn status(&self) -> Value {
        Value::Object(self.0.status())
    }
    fn provider(&self) -> String {
        self.0.provider()
    }
    fn running(&self) -> bool {
        self.0.running()
    }
    fn browser_used(&self) -> bool {
        let Some(inner) = self.0.inner() else { return false };
        let any = inner.as_any();
        if let Some(task) = any.downcast_ref::<TaskBody>() {
            return task.0.browser_used();
        }
        if let Some(chrome) = any.downcast_ref::<ChromeAuto>() {
            return chrome.0.browser_used();
        }
        if let Some(reader) = any.downcast_ref::<eve_web::web_reader::WebReaderAgent>() {
            return reader.browser_used();
        }
        false
    }
    async fn browser_shot(&self) -> Option<BrowserShot> {
        let inner = self.0.inner()?;
        let any = inner.as_any();
        let (data, suffix, label) = if let Some(chrome) = any.downcast_ref::<ChromeAuto>() {
            chrome.0.browser_shot().await?
        } else if let Some(reader) = any.downcast_ref::<eve_web::web_reader::WebReaderAgent>() {
            reader.browser_shot().await?
        } else {
            return None;
        };
        Some(BrowserShot { data, suffix, label })
    }
    fn browser_screenshot(&self) -> Option<(Vec<u8>, String)> {
        let inner = self.0.inner()?;
        if let Some(chrome) = inner.as_any().downcast_ref::<ChromeAuto>() {
            return chrome.0.browser_screenshot().map(|s| (s.data, s.suffix));
        }
        floor_of(inner.as_ref())?.browser_screenshot()
    }
    fn browser_tab_id(&self) -> String {
        self.0.inner().and_then(|inner| floor_of(inner.as_ref())).map(|f| f.browser_tab_id()).unwrap_or_default()
    }
}

// ---- the routers ----------------------------------------------------------------------------------------------------

/// The Chrome lane's rule (browser_lane.is_browser_only_goal): "chrome" for a goal only a browser serves (a URL, a
/// site, the browser), else "codex".
pub fn chrome_route() -> RouteBody {
    Arc::new(|goal: String| {
        Box::pin(async move {
            let apps = eve_desktop_lanes::task_router::installed_apps();
            Ok(if eve_browser_lane::is_browser_only_goal(&goal, &apps) { "chrome" } else { "codex" }.to_string())
        })
    })
}

/// `_bodies`' one router and its bodies: Jev first (browser_router.py: the web reader for a public-web reading
/// task), then the Chrome lane's rule for a web goal, else Codex. Without a Jev route the Chrome lane's wiring is
/// the whole answer.
pub fn route_bodies(chrome: Bodies, reader_route: Option<ReaderRoute>, make_web: MakeAgent) -> Bodies {
    let Some(reader_route) = reader_route else {
        return chrome;
    };
    let chrome_route = chrome.route_body.clone();
    let route_body: RouteBody = Arc::new(move |goal: String| {
        let reader_route = reader_route.clone();
        let chrome_route = chrome_route.clone();
        Box::pin(async move {
            let g = goal.clone();
            let body = match tokio::task::spawn_blocking(move || reader_route(&g)).await {
                Ok(Ok(body)) => body,
                Ok(Err(kind)) => {
                    // a router that fails keeps the task on the owner's computer
                    tracing::warn!("web reader: the Jev router failed ({kind})");
                    "codex".to_string()
                }
                Err(_) => {
                    tracing::warn!("web reader: the Jev router failed (Exception)");
                    "codex".to_string()
                }
            };
            if body == "web" {
                return Ok(body);
            }
            match chrome_route {
                Some(route) => route(goal).await,
                None => Ok("codex".to_string()),
            }
        })
    });
    let mut bodies = HashMap::new();
    bodies.insert("web".to_string(), make_web);
    Bodies { make_auto: chrome.make_auto, route_body: Some(route_body), bodies }
}

/// The reflex's `make_inner` and the bodies' floor from one [`MakeInner`].
fn reflex_inner(make_inner: &MakeInner) -> MakeAgent {
    let make_inner = make_inner.clone();
    Arc::new(move || Arc::new(TaskBody(make_inner())) as Arc<dyn ReflexAgent>)
}

impl Daemon {
    /// The environment as a map, for what takes one by value (the daemon's, else the process's).
    fn environ_map(&self) -> Environ {
        match self.environ() {
            Some(env) => env.clone(),
            None => std::env::vars().collect(),
        }
    }

    /// `make_inner`: the warm Codex agent when one is ready (codex_warm.py), else a new one.
    fn codex_maker(&self, on_event: &OnEvent, ask_user: &AskUser) -> MakeInner {
        let warm = self.st.lock().codex_warm.clone();
        let events = owned_events(on_event);
        let ask = ask_user.clone();
        Arc::new(move || {
            let agent = match &warm {
                Some(warm) => {
                    let agent = warm.take(events.clone(), answering(&ask));
                    // the doors' own ask, so a failed ask reaches Codex as the failure it is
                    agent.set_callbacks(events.clone(), ask.clone());
                    agent
                }
                None => CodexComputerAgent::new(events.clone(), ask.clone(), eve_codex_computer::Options::default()),
            };
            Arc::new(CodexTask(agent)) as Arc<dyn FloorAgent>
        })
    }

    /// `_make_agent(on_event, ask_user, floor=None)`: the agent for one desktop task. Codex behind the reflex
    /// (app_reflex) with the Chrome lane, the web reader and the desk lane as bodies; remembered as
    /// `active_agent`.
    ///
    /// Codex computer use, behind the launch reflex (app_reflex.py): "open Spotify" is `open -a`, with Jev for
    /// wording the rules do not know; everything else is Codex's.
    ///
    /// ``floor="codex"`` (the chief's one-way act, 2026-09-29): Codex and nothing else: no reflex and no other
    /// body (the web reader, the Chrome lane), so the act the owner approved goes whole to the agent that asks
    /// before a consequential step.
    pub fn make_agent(&self, on_event: OnEvent, ask_user: AskUser, floor: Option<String>) -> Arc<dyn TaskAgent> {
        let make_inner = self.codex_maker(&on_event, &ask_user);
        self.make_agent_with(make_inner, on_event, ask_user, floor)
    }

    /// [`Daemon::make_agent`] around a given floor (the tests' stand-in for Codex).
    pub fn make_agent_with(
        &self,
        make_inner: MakeInner,
        on_event: OnEvent,
        ask_user: AskUser,
        floor: Option<String>,
    ) -> Arc<dyn TaskAgent> {
        let warm = self.st.lock().codex_warm.clone();
        let on_done: eve_app_reflex::OnDone = match warm {
            Some(warm) => Arc::new(move || warm.kick()),
            None => Arc::new(|| {}),
        };
        let events = owned_events(&on_event);
        let options = if floor.as_deref() == Some("codex") {
            ReflexOptions { enabled: false, on_done, ..ReflexOptions::default() }
        } else {
            let environ = self.environ();
            let bodies = self.bodies(make_inner.clone(), on_event.clone(), ask_user.clone());
            ReflexOptions {
                asker: eve_app_reflex::jev_asker(environ),
                quit_asker: eve_app_reflex::jev_quit_asker(environ),
                enabled: eve_app_reflex::reflexes_on(environ),
                on_done,
                desk: self.desk().map(|lane| eve_app_reflex::desk_from_lane((*lane).clone())),
                make_auto: bodies.make_auto,
                route_body: bodies.route_body,
                bodies: bodies.bodies,
                ..ReflexOptions::default()
            }
        };
        let reflex = Arc::new(ReflexFirstAgent::new(reflex_inner(&make_inner), events, options));
        let agent: Arc<dyn TaskAgent> = Arc::new(ReflexTask(reflex));
        self.st.lock().active_agent = Some(agent.clone());
        agent
    }

    /// The daemon's [`eve_brain::seams::AgentFactory`]: `make_agent` behind an `Arc`. It holds the daemon weakly
    /// (the doors that keep it are the daemon's); once the daemon is gone it makes Codex alone.
    pub fn agent_factory(&self) -> eve_brain::seams::AgentFactory {
        let me = self.me.clone();
        Arc::new(move |on_event: OnEvent, ask_user: AskUser, floor: Option<String>| match me.upgrade() {
            Some(daemon) => daemon.make_agent(on_event, ask_user, floor),
            None => {
                let events = owned_events(&on_event);
                let agent = CodexComputerAgent::new(events, ask_user, eve_codex_computer::Options::default());
                Arc::new(CodexTask(agent)) as Arc<dyn TaskAgent>
            }
        })
    }

    /// `_make_chrome_lane()`: the browser lane for the daemon's life (EVE_BROWSER_ATTACH=1), made once; None when
    /// off.
    ///
    /// EVE_BROWSER_ATTACH=1: the browser lane attached to the owner's own Chrome, kept for the daemon's life
    /// (connected on the first web task, not now). EVE_BROWSER_OWN=1: the same lane on eve's own Chrome instead
    /// (browser_lane.py, EVE'S OWN CHROME). None otherwise, or when the lane cannot start.
    pub fn make_chrome_lane(&self) -> Option<Arc<eve_browser_lane::BrowserLane>> {
        if let Some(lane) = self.st.lock().chrome_lane.clone() {
            return Some(lane);
        }
        let cfg = eve_browser_lane::configured(self.environ());
        if !cfg.attach {
            return None;
        }
        // Jev grounds each click unless the owner turned it off (EVE_JEV_STEP=0).
        let mut env = self.environ_map();
        let step = env.get("EVE_JEV_STEP").cloned().unwrap_or_else(|| "1".to_string());
        env.insert("EVE_JEV_STEP".into(), step);
        let step_asker = eve_browser_lane::make_step_asker(Some(&env))
            .map(|a| a as Box<dyn eve_desktop_lanes::fast_lane::StepAsk + Send>);
        let options = eve_browser_lane::LaneOptions { step_asker, ..Default::default() };
        let lane = match eve_browser_lane::BrowserLane::new(cfg.clone(), options) {
            Ok(lane) => Arc::new(lane),
            Err(e) => {
                tracing::warn!("chrome lane: EVE_BROWSER_ATTACH is on but the browser lane could not start ({e}); off");
                return None;
            }
        };
        if cfg.steel {
            tracing::info!(
                "chrome lane: on — web tasks try eve's Steel browser first (local Steel, no key, signed in with \
                 `eve-bridge steel-profile`); Codex is the floor; no Allow to press"
            );
        } else if cfg.own {
            tracing::info!(
                "chrome lane: on — web tasks try eve's own Chrome first ({}, signed in with `eve-bridge \
                 chrome-profile`); Codex is the floor; no Allow to press",
                cfg.own_dir.display()
            );
        } else {
            let profile = if cfg.chrome_profile.is_empty() { "the first open" } else { cfg.chrome_profile.as_str() };
            let allow = if self.chrome_access() == "allow" {
                "pressed by eve (EVE_CHROME_ACCESS=allow)"
            } else {
                "asked on the phone"
            };
            tracing::info!(
                "chrome lane: on — web tasks try the owner's logged-in Chrome first (profile: {profile}); Codex is the \
                 floor; Chrome's Allow: {allow}"
            );
        }
        self.st.lock().chrome_lane = Some(lane.clone());
        Some(lane)
    }

    /// `chrome_consent.access_preference()` from the daemon's environment.
    fn chrome_access(&self) -> &'static str {
        match self.environ() {
            Some(env) => eve_chrome_consent::access_preference_in(env),
            None => eve_chrome_consent::access_preference(),
        }
    }

    /// `_desk()`: the desk lane, made once (EVE_DESK_LANE on), None when off.
    pub fn desk(&self) -> Option<Arc<eve_desk_lane::DeskLane>> {
        if !eve_desk_lane::enabled(self.environ()) {
            return None;
        }
        let mut st = self.st.lock();
        if st.desk_lane.is_none() {
            st.desk_lane = Some(Arc::new(eve_desk_lane::DeskLane::new()));
            tracing::info!("desk lane: on — keyword-decided clicks on the frontmost window before Codex");
        }
        st.desk_lane.clone()
    }

    /// `_bodies(make_inner, on_event, ask_user)`: the bodies the reflex may hand a goal to.
    ///
    /// ReflexFirstAgent's bodies beside Codex, and the one router that picks between them: Jev first
    /// (browser_router.py: the web reader for a public-web reading task, web_reader.py), then the Chrome lane's rule
    /// for a web goal, else Codex. Empty when neither extra body is on: every task stays Codex's.
    pub fn bodies(&self, make_inner: MakeInner, on_event: OnEvent, ask_user: AskUser) -> Bodies {
        let chrome = self.chrome_body(make_inner.clone(), on_event.clone(), ask_user.clone());
        let ctx = self.web_ctx();
        let reader_on = eve_web::web_reader::configured(&ctx).enabled;
        let reader_route: Option<ReaderRoute> = if reader_on {
            self.reader_route().map(|router| {
                Arc::new(move |goal: &str| router.route(goal).map(str::to_string).map_err(|e| e.kind.to_string()))
                    as ReaderRoute
            })
        } else {
            None
        };
        let fallback = reflex_inner(&make_inner);
        let events = owned_events(&on_event);
        let ask = answering(&ask_user);
        let make_web: MakeAgent = Arc::new(move || {
            let options =
                eve_web::web_reader::ReaderOptions { ctx: Some(ctx.clone()), browser_screenshot: Some(codex_shot()), ..Default::default() };
            Arc::new(eve_web::web_reader::WebReaderAgent::new(fallback.clone(), events.clone(), ask.clone(), options))
                as Arc<dyn ReflexAgent>
        });
        route_bodies(chrome, reader_route, make_web)
    }

    /// The web's process services with the daemon's environment.
    fn web_ctx(&self) -> eve_web::Ctx {
        let mut ctx = eve_web::Ctx::global().clone();
        if let Some(env) = self.environ() {
            ctx.env = Some(Arc::new(env.clone()));
        }
        ctx
    }

    /// `_reader_route()`: browser_router's Jev router for the web reader, made once; None when off.
    ///
    /// None without a Jev route: every task stays where it was.
    pub fn reader_route(&self) -> Option<Arc<eve_browser_router::Router>> {
        if let Some(made) = self.st.lock().reader_router.clone() {
            return made;
        }
        let router = match self.environ() {
            Some(env) => eve_browser_router::jev_router(env),
            None => eve_browser_router::jev_router(&eve_jev::ProcessEnv),
        }
        .map(Arc::new);
        if router.is_none() {
            tracing::info!("web reader: no Jev route (EVE_JEV_ROUTE); the web reader takes no tasks");
        }
        let mut st = self.st.lock();
        st.reader_router.get_or_insert(router).clone()
    }

    /// `_chrome_body(make_inner, on_event, ask_user)`: the Chrome lane as a body (browser-only goals), with its
    /// planner; empty when the lane is off.
    ///
    /// The Chrome lane as ReflexFirstAgent's second body, for web goals (browser_lane.is_web_goal: a URL, a site,
    /// the browser). Empty when the lane is off: every task stays Codex's.
    pub fn chrome_body(&self, make_inner: MakeInner, on_event: OnEvent, ask_user: AskUser) -> Bodies {
        let lane = self.st.lock().chrome_lane.clone();
        let Some(lane) = lane else {
            return Bodies::default();
        };
        let have_planner = self.st.lock().planner_create.is_some();
        if !have_planner && !eve_openrouter::available(self.environ()) {
            tracing::warn!("chrome lane: OPENROUTER_API_KEY is not set; web tasks go to Codex");
            return Bodies::default();
        }
        let telegram_on = self.st.lock().telegram.is_some();
        let ask_owner: Option<eve_chrome_consent::AskOwner> = telegram_on.then(|| {
            let me = self.me.clone();
            Arc::new(move |q: String| {
                let me = me.clone();
                Box::pin(async move {
                    // Python's `_ask_owner_on_phone` raises when there is no way to ask: the broker reads that as
                    // "unanswered", where an empty reply would read as "declined".
                    match me.upgrade() {
                        Some(daemon) => daemon.ask_owner_checked(&q).await,
                        None => Err(anyhow::anyhow!("no Telegram chat to ask the owner in")),
                    }
                }) as BoxFuture<'static, anyhow::Result<String>>
            }) as eve_chrome_consent::AskOwner
        });
        let tell_owner: Option<eve_chrome_consent::TellOwner> = telegram_on.then(|| {
            let me = self.me.clone();
            Arc::new(move |text: String| {
                let me = me.clone();
                Box::pin(async move {
                    if let Some(daemon) = me.upgrade() {
                        daemon.tell_owner_on_phone(&text).await;
                    }
                    Ok(())
                }) as BoxFuture<'static, anyhow::Result<()>>
            }) as eve_chrome_consent::TellOwner
        });
        let broker = Arc::new(eve_chrome_consent::ConsentBroker::new(
            ask_owner,
            eve_chrome_consent::BrokerOptions {
                tell_owner,
                auto_allow: self.chrome_access() == "allow", // EVE_CHROME_ACCESS=allow
                ..Default::default()
            },
        ));
        let create = {
            let mut st = self.st.lock();
            match st.planner_create.clone() {
                Some(create) => create,
                None => {
                    let options = eve_computer_agent::CreatorOptions {
                        api_key: self.environ().map(|env| eve_openrouter::key(Some(env))),
                        ..Default::default()
                    };
                    match eve_computer_agent::make_response_creator(options) {
                        Ok(create) => {
                            st.planner_create = Some(create.clone());
                            create
                        }
                        Err(e) => {
                            tracing::warn!("chrome lane: the planner's model could not be reached ({e}); web tasks go to Codex");
                            return Bodies::default();
                        }
                    }
                }
            }
        };
        let cfg = eve_computer_agent::browser_config(&self.agent_cfg, self.environ());
        let make_auto = chrome_make_auto(ChromeWiring { lane, broker, create, cfg, make_inner, on_event, ask_user });
        Bodies { make_auto: Some(make_auto), route_body: Some(chrome_route()), bodies: HashMap::new() }
    }
}

/// What the Chrome lane body is built from each time the reflex asks for it.
struct ChromeWiring {
    lane: Arc<eve_browser_lane::BrowserLane>,
    broker: Arc<eve_chrome_consent::ConsentBroker>,
    create: CreateResponse,
    cfg: AgentConfig,
    make_inner: MakeInner,
    on_event: OnEvent,
    ask_user: AskUser,
}

/// `make_auto`: a ChromeLaneAgent planning in the lane (prepare: connect, answering Chrome's Allow, then pick the
/// profile), Codex for the rest; before Codex takes over, the owner's Chrome is made sure of for its extension.
fn chrome_make_auto(w: ChromeWiring) -> MakeAgent {
    let w = Arc::new(w);
    Arc::new(move || {
        let w = w.clone();
        let prepare: eve_chrome_lane::Prepare = {
            let (lane, broker) = (w.lane.clone(), w.broker.clone());
            Arc::new(move |goal: String| {
                let (lane, broker) = (lane.clone(), broker.clone());
                Box::pin(async move {
                    lane.connect_answering(move || async move { broker.answer_own_connection().await }).await?;
                    lane.use_profile(&goal).await?;
                    Ok(())
                })
            })
        };
        let lane_screenshot: eve_chrome_lane::LaneScreenshot = {
            let lane = w.lane.clone();
            Arc::new(move || {
                let lane = lane.clone();
                Box::pin(async move { lane.screenshot().await.ok().flatten() })
            })
        };
        let before_fallback: eve_chrome_lane::BeforeFallback = Arc::new(|| {
            Box::pin(async move {
                tokio::task::spawn_blocking(eve_browser_lane::ensure_owner_chrome).await??;
                Ok(())
            })
        });
        let make_planner: eve_chrome_lane::MakePlanner = {
            let w = w.clone();
            Box::new(move |ev: Arc<dyn Fn(AgentEvent) + Send + Sync>, _ask| {
                let options = eve_computer_agent::agent::AgentOptions {
                    config: Some(w.cfg.clone()),
                    on_event: Some(borrowed_events(ev)),
                    // the lane hands the planner the ask it was given: the doors' own
                    ask_user: Some(w.ask_user.clone()),
                    browser: Some(Arc::new(eve_computer_agent::BrowserLaneWorker(w.lane.clone()))),
                    ..Default::default()
                };
                let agent = Arc::new(ComputerAgent::with_options(w.create.clone(), options));
                Arc::new(PlannerBody { agent, lane: w.lane.clone() }) as Arc<dyn Planner>
            })
        };
        let make_fallback: eve_chrome_lane::MakeFallback = {
            let make_inner = w.make_inner.clone();
            Box::new(move || Arc::new(TaskBody(make_inner())) as Arc<dyn Fallback>)
        };
        let agent = ChromeLaneAgent::with_options(
            make_planner,
            make_fallback,
            owned_events(&w.on_event),
            answering(&w.ask_user),
            ChromeLaneOptions {
                prepare: Some(prepare),
                lane_screenshot: Some(lane_screenshot),
                idle_budget_secs: None,
                before_fallback: Some(before_fallback),
            },
        );
        Arc::new(ChromeAuto(Arc::new(agent))) as Arc<dyn ReflexAgent>
    })
}

/// The web reader's `getattr(self._current, "browser_screenshot")`: Codex's captured tab once it took over.
fn codex_shot() -> eve_web::web_reader::ScreenshotFn {
    Arc::new(|agent: &dyn ReflexAgent| floor_of(agent)?.browser_screenshot())
}
