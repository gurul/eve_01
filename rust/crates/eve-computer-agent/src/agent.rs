//! `ComputerAgent`: one task on the desktop. See the crate docs for the contract.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use futures::future::BoxFuture;
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use tokio::sync::watch;

use eve_desktop_lanes::{lane_router, task_router};
use eve_jev::{Meter, MeterSnapshot};
use eve_plan_contract as pc;

use crate::py::{self, num, round, str_or_empty, truthy, value_str};
use crate::worker::{Worker, WorkerClient};
use crate::{
    classify_response, closing_say, describe_failure, error_name, instructions, is_a, name_and_text,
    nothing_left, progress_lines, same_page, tools, user, verdict, AgentConfig, AgentEvent, Cancelled,
    FailSafe, GOAL_URL, MAX_BROWSER_REPLANS, MAX_PLAN_CONFIRMS, RETRYABLE_API_ERRORS, VERIFY_INSTRUCTIONS,
    VERIFY_SCHEMA,
};

/// `create_response(request) -> response`: responses.create as a dict-in/dict-out coroutine (the seam tests
/// fake).
pub type CreateResponse = Arc<dyn Fn(Value) -> BoxFuture<'static, anyhow::Result<Value>> + Send + Sync>;
/// `ask_user(question) -> spoken answer`.
pub type AskUser = Arc<dyn Fn(String) -> BoxFuture<'static, anyhow::Result<String>> + Send + Sync>;
/// `on_event(event)`.
pub type OnEvent = Arc<dyn Fn(&AgentEvent) + Send + Sync>;
/// `sleep(seconds)`.
pub type SleepFn = Arc<dyn Fn(f64) -> BoxFuture<'static, ()> + Send + Sync>;
/// `clock() -> seconds` (monotonic).
pub type ClockFn = Arc<dyn Fn() -> f64 + Send + Sync>;
/// `worker_factory() -> worker`.
pub type WorkerFactory = Arc<dyn Fn() -> Arc<dyn Worker> + Send + Sync>;
/// `task_router.installed_apps()` as a seam.
pub type AppsFn = Arc<dyn Fn() -> Vec<String> + Send + Sync>;
/// The hosted router classifier: `asker(goal, apps) -> app or ""`.
pub type RouterAsker = Arc<dyn Fn(&str, &[String]) -> String + Send + Sync>;

/// `time.monotonic()`.
pub fn monotonic() -> f64 {
    static EPOCH: std::sync::LazyLock<std::time::Instant> = std::sync::LazyLock::new(std::time::Instant::now);
    // an arbitrary origin, as Python's
    EPOCH.elapsed().as_secs_f64()
}

fn tokio_sleep() -> SleepFn {
    Arc::new(|s: f64| {
        Box::pin(async move {
            if let Ok(d) = std::time::Duration::try_from_secs_f64(s.max(0.0)) {
                tokio::time::sleep(d).await;
            }
        })
    })
}

#[derive(Clone, Copy, Default)]
struct BillTokens {
    input: i64,
    cached: i64,
    out: i64,
}

impl BillTokens {
    fn to_value(self) -> Value {
        json!({"in": self.input, "cached": self.cached, "out": self.out})
    }
}

enum AskerSlot {
    Unbuilt,
    Failed,
    Built(RouterAsker),
}

struct State {
    steer: Vec<String>,
    cancel_reason: String,
    t0: f64,
    last_code: Option<String>,
    /// any step this run clicked, typed or pressed keys
    acted: bool,
    /// the latest [after …] line
    last_after: String,
    verify_retried: bool,
    retry_note: String,
    running: bool,
    turn: i64,
    goal: String,
    last_commentary: String,
    final_: Option<String>,
    run_log: Option<PathBuf>,
    bill_tokens: BillTokens,
    bill_calls: i64,
    jev_before: MeterSnapshot,
    asker: AskerSlot,
}

/// One task on the desktop. Shared as `Arc<ComputerAgent>`: `steer`, `cancel` and `status` are called from
/// other tasks while `run` is in flight.
pub struct ComputerAgent {
    pub create_response: CreateResponse,
    pub worker_factory: Mutex<Option<WorkerFactory>>,
    /// the browser lane, when EVE_BROWSER_ATTACH is on
    pub browser: Option<Arc<dyn Worker>>,
    pub config: AgentConfig,
    on_event: Mutex<Option<OnEvent>>,
    ask_user: Mutex<Option<AskUser>>,
    clock: ClockFn,
    sleep: SleepFn,
    /// task_router.installed_apps(), as a seam
    apps: AppsFn,
    /// the daily spend ledger (eve-spend); `None` is the configured one
    ledger: Option<eve_spend::Ledger>,
    /// the Jev call meter; `None` is the process-wide `eve_jev::METER`
    meter: Option<Arc<Meter>>,
    cancel_tx: watch::Sender<bool>,
    state: Mutex<State>,
}

/// The agent's optional seams and handles (Python's keyword arguments).
#[derive(Default)]
pub struct AgentOptions {
    pub worker_factory: Option<WorkerFactory>,
    pub config: Option<AgentConfig>,
    pub on_event: Option<OnEvent>,
    pub ask_user: Option<AskUser>,
    pub clock: Option<ClockFn>,
    pub sleep: Option<SleepFn>,
    pub browser: Option<Arc<dyn Worker>>,
    pub apps: Option<AppsFn>,
    pub ledger: Option<eve_spend::Ledger>,
    pub meter: Option<Arc<Meter>>,
}

type Res<T> = anyhow::Result<T>;

fn cancelled() -> anyhow::Error {
    anyhow::Error::new(Cancelled)
}

fn is_stop(e: &anyhow::Error) -> bool {
    is_a::<Cancelled>(e) || is_a::<FailSafe>(e)
}

fn text_items(out: &[Value]) -> Vec<String> {
    out.iter()
        .filter(|o| o.get("type").and_then(Value::as_str) == Some("input_text"))
        .map(|o| o.get("text").map(value_str).unwrap_or_default())
        .collect()
}

fn obj(v: &Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

impl ComputerAgent {
    /// `ComputerAgent(create_response)` with every other argument at its default.
    pub fn new(create_response: CreateResponse) -> Self {
        Self::with_options(create_response, AgentOptions::default())
    }

    pub fn with_options(create_response: CreateResponse, o: AgentOptions) -> Self {
        let (cancel_tx, _) = watch::channel(false);
        ComputerAgent {
            create_response,
            worker_factory: Mutex::new(o.worker_factory),
            browser: o.browser,
            config: o.config.unwrap_or_default(),
            on_event: Mutex::new(o.on_event),
            ask_user: Mutex::new(o.ask_user),
            clock: o.clock.unwrap_or_else(|| Arc::new(monotonic)),
            sleep: o.sleep.unwrap_or_else(tokio_sleep),
            apps: o.apps.unwrap_or_else(|| Arc::new(task_router::installed_apps)),
            ledger: o.ledger,
            meter: o.meter,
            cancel_tx,
            state: Mutex::new(State {
                steer: Vec::new(),
                cancel_reason: String::new(),
                t0: 0.0,
                last_code: None,
                acted: false,
                last_after: String::new(),
                verify_retried: false,
                retry_note: String::new(),
                running: false,
                turn: 0,
                goal: String::new(),
                last_commentary: String::new(),
                final_: None,
                run_log: None,
                bill_tokens: BillTokens::default(),
                bill_calls: 0,
                jev_before: MeterSnapshot::default(),
                asker: AskerSlot::Unbuilt,
            }),
        }
    }

    pub fn set_on_event(&self, f: Option<OnEvent>) {
        *self.on_event.lock() = f;
    }

    pub fn set_ask_user(&self, f: Option<AskUser>) {
        *self.ask_user.lock() = f;
    }

    pub fn set_worker_factory(&self, f: Option<WorkerFactory>) {
        *self.worker_factory.lock() = f;
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    fn meter(&self) -> &Meter {
        self.meter.as_deref().unwrap_or(&eve_jev::METER)
    }

    pub fn running(&self) -> bool {
        self.state.lock().running
    }

    pub fn turn(&self) -> i64 {
        self.state.lock().turn
    }

    pub fn goal(&self) -> String {
        self.state.lock().goal.clone()
    }

    pub fn last_commentary(&self) -> String {
        self.state.lock().last_commentary.clone()
    }

    /// The last run's answer (`final` in Python).
    pub fn final_answer(&self) -> Option<String> {
        self.state.lock().final_.clone()
    }

    /// The last run's action log.
    pub fn run_log(&self) -> Option<PathBuf> {
        self.state.lock().run_log.clone()
    }

    /// Whether any step of this run clicked, typed or pressed keys.
    pub fn acted(&self) -> bool {
        self.state.lock().acted
    }

    // -- the human's handles --

    pub fn steer(&self, text: &str) -> bool {
        {
            let mut s = self.state.lock();
            if !s.running {
                return false;
            }
            s.steer.push(py::py_strip(text).to_string());
        }
        self.log(json!({"steer": text}));
        true
    }

    pub fn cancel(&self, reason: &str) {
        {
            let mut s = self.state.lock();
            if s.cancel_reason.is_empty() {
                s.cancel_reason = reason.to_string();
            }
        }
        self.cancel_tx.send_replace(true);
    }

    pub fn status(&self) -> Value {
        let s = self.state.lock();
        json!({"running": s.running, "turn": s.turn, "max_turns": self.config.max_turns,
               "goal": s.goal, "last": s.last_commentary, "final": s.final_})
    }

    fn begin(&self, goal: &str) {
        let before = self.meter().snapshot();
        let t0 = self.now();
        {
            let mut s = self.state.lock();
            s.t0 = t0;
            s.goal = goal.to_string();
            s.running = true;
            s.turn = 0;
            s.final_ = None;
            s.cancel_reason.clear();
            s.last_code = None;
            s.acted = false;
            s.last_after.clear();
            s.verify_retried = false;
            s.bill_tokens = BillTokens::default();
            s.bill_calls = 0;
            s.jev_before = before;
        }
        self.cancel_tx.send_replace(false);
    }

    fn stopped_sentence(&self) -> (String, String) {
        let reason = self.state.lock().cancel_reason.clone();
        let said = if reason.is_empty() { "Stopped.".to_string() } else { format!("Stopped: {reason}.") };
        (said, reason)
    }

    // -- the loop --

    /// Only the browser lane's turn (`browser_first`): plan once against the lane's page and execute it, asking
    /// the human before any sensitive step. No desktop worker, no screenshot loop. Returns (answer, "") when the
    /// plan finished or the human said no, ("", note) when it did part of the job — the caller hands the rest,
    /// with the note, to another agent (chrome_lane: Codex). Never fails for a lane failure; a cancel stops it
    /// like `run`. A fail-safe is the one error that comes back.
    pub async fn run_in_browser(&self, goal: &str) -> Res<(String, String)> {
        if self.browser.is_none() {
            return Ok((String::new(), String::new()));
        }
        let t0 = self.now();
        let before = self.meter().snapshot();
        {
            let mut s = self.state.lock();
            s.t0 = t0;
            s.goal = goal.to_string();
            s.running = true;
            s.turn = 0;
            s.final_ = None;
            s.cancel_reason.clear();
            s.acted = false;
            s.bill_tokens = BillTokens::default();
            s.bill_calls = 0;
            s.jev_before = before;
        }
        self.cancel_tx.send_replace(false);
        self.open_log(goal);
        let result = self.browser_first(goal, None).await;
        let (answer, note) = match result {
            Ok(pair) => pair,
            Err(e) if is_a::<Cancelled>(&e) => {
                let (said, reason) = self.stopped_sentence();
                self.log(json!({"cancelled": reason}));
                (said, String::new())
            }
            Err(e) => {
                self.state.lock().running = false;
                return Err(e);
            }
        };
        {
            let mut s = self.state.lock();
            s.running = false;
            s.final_ = if answer.is_empty() { None } else { Some(answer.clone()) };
        }
        self.log(json!({"final": answer, "browser_only": true, "handed_on": answer.is_empty() && !note.is_empty()}));
        self.log(json!({"bill": self.bill()}));
        Ok((answer, note))
    }

    fn default_worker(&self) -> Arc<dyn Worker> {
        Arc::new(WorkerClient::new(self.config.exec_timeout_secs))
    }

    pub async fn run(&self, goal: &str) -> String {
        self.begin(goal);
        self.open_log(goal);
        self.emit("started", goal, 0);
        let factory = self.worker_factory.lock().clone();
        let worker: Arc<dyn Worker> = match factory {
            Some(f) => f(),
            None => self.default_worker(),
        };
        let outcome = async {
            let ready = worker.start().await?;
            if let Value::Object(m) = &ready {
                let info: Map<String, Value> = m.iter().filter(|(k, _)| k.as_str() != "ready").map(|(k, v)| (k.clone(), v.clone())).collect();
                let fast = info.get("fast_lane").map(value_str).unwrap_or_else(|| "off (not reported)".into());
                self.log(json!({"worker": Value::Object(info)}));
                tracing::info!("agent: worker ready — fast lane {}", fast);
            }
            self.run_loop(goal, worker.as_ref()).await
        }
        .await;
        let result = match outcome {
            Ok(r) => r,
            Err(e) if is_a::<Cancelled>(&e) => {
                let (said, reason) = self.stopped_sentence();
                self.log(json!({"cancelled": reason}));
                self.emit("cancelled", &said, 0);
                said
            }
            Err(e) if is_a::<FailSafe>(&e) => {
                self.emit("cancelled", &e.to_string(), 0);
                "Stopped: the mouse hit a screen corner.".to_string()
            }
            Err(e) => {
                tracing::error!("agent: run failed: {:#}", e);
                let said = describe_failure(&e);
                self.emit("error", &name_and_text(&e), 0);
                said
            }
        };
        worker.close().await;
        {
            let mut s = self.state.lock();
            s.running = false;
            s.final_ = Some(result.clone());
        }
        self.log(json!({"final": result}));
        self.log(json!({"bill": self.bill()}));
        result
    }

    // -- the bill per task (the Jev Engineering article: "track the bill per completed task") --

    /// Count one Responses call's usage towards this run's bill. Every model call goes through here.
    fn meter_response(&self, response: &Value) {
        let mut s = self.state.lock();
        s.bill_calls += 1;
        if let Some(usage @ Value::Object(_)) = response.get("usage") {
            let t = eve_pricing::responses_tokens(usage);
            s.bill_tokens.input += t.input;
            s.bill_tokens.cached += t.cached;
            s.bill_tokens.out += t.out;
        }
    }

    /// Model tokens and USD at the grounded rates (eve-pricing), Jev calls, tokens and USD since the run began,
    /// and the wall seconds. `usd` is null for a model the table does not know: never a guess.
    pub fn bill(&self) -> Value {
        let (tokens, calls, before, t0) = {
            let s = self.state.lock();
            (s.bill_tokens, s.bill_calls, s.jev_before, s.t0)
        };
        let usd = eve_pricing::estimate_openai_cost(
            &self.config.model,
            &json!({"input_tokens": tokens.input, "output_tokens": tokens.out,
                    "input_tokens_details": {"cached_tokens": tokens.cached}}),
        );
        let jev_d = Meter::delta(&before, &self.meter().snapshot());
        let jev_usd = eve_pricing::estimate_jev_cost(jev_d.input_tokens);
        let total = usd.map(|u| num(round(u + jev_usd, 6)));
        json!({"model": self.config.model, "calls": calls, "tokens": tokens.to_value(),
               "usd": usd.map(|u| num(round(u, 6))),
               "jev": {"calls": jev_d.calls, "input_tokens": jev_d.input_tokens, "ms": jev_d.ms,
                       "errors": jev_d.errors, "usd": num(round(jev_usd, 6))},
               "total_usd": total, "secs": num(round(self.now() - t0, 2))})
    }

    /// Await `fut`, but let cancel() win at once (the SDK's own timeout is long).
    async fn interruptible<T, F: Future<Output = Res<T>>>(&self, fut: F) -> Res<T> {
        // As asyncio.wait(FIRST_COMPLETED) over the call and the cancel event: an awaitable that is already done
        // wins even when cancel() came first (both are done, and Python returns the result); one still pending
        // loses to a cancel at once.
        let mut rx = self.cancel_tx.subscribe();
        tokio::select! {
            biased;
            r = fut => r,
            _ = rx.wait_for(|c| *c) => Err(cancelled()),
        }
    }

    /// responses.create with one retry after 1 s on a transient API error. Every model call of a run goes
    /// through here (the loop, the plan, the checks), so this is where the daily spend meter reads it.
    pub async fn create(&self, req: &Value, turn: i64) -> Res<Value> {
        let response = match (self.create_response)(req.clone()).await {
            Ok(r) => r,
            Err(e) => {
                let name = error_name(&e);
                if !RETRYABLE_API_ERRORS.contains(&name.as_str()) {
                    return Err(e);
                }
                tracing::warn!("agent: {} on turn {}; retrying once", name, turn);
                self.log(json!({"turn": turn, "retry": name}));
                (self.sleep)(1.0).await;
                (self.create_response)(req.clone()).await?
            }
        };
        let model = match req.get("model") {
            Some(m) if truthy(m) => value_str(m),
            _ => self.config.model.clone(),
        };
        let ledger = self.ledger.clone().unwrap_or_default();
        ledger.record_response(eve_spend::TASKS, &response, &model, eve_openrouter::PROVIDER, None);
        Ok(response)
    }

    async fn run_loop(&self, goal: &str, worker: &dyn Worker) -> Res<String> {
        let cfg = &self.config;
        // every item sent and received so far (no previous_response_id)
        let mut history: Vec<Value> = Vec::new();
        let mut lane_note = String::new();
        let mut lane_clicked = false;
        // the browser lane took this goal: the Mac tiers stand down
        let mut web = false;
        let mut plan: Option<task_router::Plan> = None;
        if cfg.reflexes {
            // In a blocking thread: the rules are microseconds, but a hosted model is a network call, and the
            // daemon's loop is never the place to wait on one (loop_watchdog exists because of that).
            let asker = self.router_asker();
            let apps = (self.apps)();
            let g = goal.to_string();
            let classified = self
                .interruptible(async move {
                    tokio::task::spawn_blocking(move || {
                        let model_fn = asker.map(|a| move |text: &str, installed: &[String]| Ok(a(text, installed)));
                        match &model_fn {
                            Some(m) => task_router::classify(&g, "", &apps, Some(m), None),
                            None => task_router::classify(&g, "", &apps, None, None),
                        }
                    })
                    .await
                    .map_err(|e| crate::runtime(format!("the router thread failed: {e}")))
                })
                .await?;
            self.log(json!({"turn": 0, "route": {"kind": classified.kind, "tiers": classified.tiers,
                            "app": classified.app, "query": classified.query, "reasons": classified.reasons}}));
            plan = Some(classified);
        }
        let route_kind = plan.as_ref().map(|p| p.kind.clone()).unwrap_or_default();
        if self.browser.is_some() && eve_browser_lane::is_web_goal(goal, &route_kind) {
            // The browser lane: eve's own Chromium, the page as the snapshot, the same plan executor.
            // A web reflex (search, open a URL) is one navigation there; the rest is planned once.
            let (answer, note) = self.browser_first(goal, plan.as_ref()).await?;
            lane_note = note;
            if !answer.is_empty() {
                self.emit("final", &answer, 0);
                return Ok(answer);
            }
            // the Mac reflexes must not also open the URL in Safari
            plan = None;
            web = true;
        }
        if let Some(p) = plan.as_ref().filter(|p| p.tiers.iter().any(|t| t == "reflex")) {
            let (done, note) = self.reflex(p, worker).await?;
            lane_note = note;
            if !done.is_empty() {
                self.emit("final", &done, 0);
                return Ok(done);
            }
        }
        let lane_tier = plan.as_ref().is_none_or(|p| p.tiers.iter().any(|t| t == "lane"));
        if cfg.lane_first && !web && lane_tier {
            let routed = self.lane_first(goal, worker).await?;
            if routed.get("status").and_then(Value::as_str) == Some("complete") && routed.get("sentence").is_some_and(truthy) {
                let answer = value_str(&routed["sentence"]);
                self.emit("final", &answer, 0);
                return Ok(answer);
            }
            let clicked: Vec<String> = match routed.get("clicked") {
                Some(Value::Array(a)) => a.iter().map(value_str).collect(),
                _ => Vec::new(),
            };
            if !clicked.is_empty() {
                lane_clicked = true;
                // the lane clicked: the planner's final answer gets checked
                self.state.lock().acted = true;
                let why = match routed.get("reason") {
                    Some(v) if truthy(v) => value_str(v),
                    _ => "unverified".to_string(),
                };
                let names: Vec<String> = clicked.iter().map(|c| format!("\"{c}\"")).collect();
                lane_note = format!(
                    "\n\n[note] Before you started, the fast lane already clicked, in order: {}. It stopped there \
                     ({why}). Start from the screenshot; do not repeat those clicks.",
                    names.join(", ")
                );
            }
        }
        // A launch reflex before this point is what the plan wants: the outline is then the right app's
        // window. Lane clicks are not — the screen has moved under a request that is half done.
        if cfg.plan_exec && !web && !lane_clicked && !task_router::TELL_ME.is_match(goal) {
            let (answer, plan_note) = self.plan_once(goal, worker, "").await?;
            if !answer.is_empty() {
                self.emit("final", &answer, 0);
                return Ok(answer);
            }
            if !plan_note.is_empty() {
                lane_note = plan_note;
            }
        }
        let items = self.interruptible(worker.observe()).await?;
        let texts = text_items(&items);
        // the context line; the worker's [after] line is noise here
        let context = texts.first().cloned().unwrap_or_default();
        let images: Vec<Value> =
            items.iter().filter(|o| o.get("type").and_then(Value::as_str) == Some("input_image")).cloned().collect();
        self.log(json!({"turn": 0, "context": context}));
        let mut content = vec![json!({"type": "input_text",
                                      "text": format!("Goal: {goal}\n\nMac: {context}. Screenshot attached.{lane_note}")})];
        content.extend(images);
        let mut next_input: Vec<Value> = vec![json!({"type": "message", "role": "user", "content": content})];
        let mut effort = cfg.plan_reasoning_effort.clone();
        let mut continued = false;
        let instructions_text = instructions(cfg.fast_lane, &cfg.lane_decide);
        let tool_list = tools(cfg.fast_lane);
        for turn in 1..=cfg.max_turns.max(0) {
            self.check_cancel()?;
            let t0 = self.state.lock().t0;
            if self.now() - t0 > cfg.max_secs {
                let msg = format!("I ran out of time ({} s) before finishing.", cfg.max_secs.trunc() as i64);
                let t = self.turn();
                self.emit("final", &msg, t);
                return Ok(msg);
            }
            self.state.lock().turn = turn;
            let mut input = history.clone();
            input.extend(next_input.iter().cloned());
            let req = json!({
                "model": cfg.model,
                "instructions": instructions_text,
                "input": input,
                "tools": tool_list,
                "parallel_tool_calls": false,
                "reasoning": {"effort": effort},
                "truncation": "auto",
                "store": false,
                "include": ["reasoning.encrypted_content"],
                "timeout": cfg.api_timeout_secs,
            });
            let t_req = self.now();
            let response = self.interruptible(self.create(&req, turn)).await?;
            self.meter_response(&response);
            let mut entry = json!({"turn": turn, "effort": effort, "api_secs": num(round(self.now() - t_req, 2))});
            if let Some(Value::Object(usage)) = response.get("usage") {
                let reasoning = usage
                    .get("output_tokens_details")
                    .and_then(Value::as_object)
                    .and_then(|d| d.get("reasoning_tokens"))
                    .cloned()
                    .unwrap_or(Value::Null);
                entry["tokens"] = json!({"in": usage.get("input_tokens").cloned().unwrap_or(Value::Null),
                                         "out": usage.get("output_tokens").cloned().unwrap_or(Value::Null),
                                         "reasoning": reasoning});
            }
            self.log(entry);
            self.check_cancel()?;
            history = input;
            if let Some(Value::Array(out)) = response.get("output") {
                history.extend(out.iter().filter(|i| i.is_object()).cloned());
            }
            let incomplete_reason = response
                .get("incomplete_details")
                .and_then(Value::as_object)
                .and_then(|d| d.get("reason"))
                .and_then(Value::as_str);
            if response.get("status").and_then(Value::as_str) == Some("incomplete")
                && incomplete_reason == Some("max_output_tokens")
                && !continued
            {
                next_input = vec![user("[note] Your reply was cut off; continue.")];
                continued = true;
                continue;
            }
            let c = classify_response(&response)?;
            effort = cfg.reasoning_effort.clone();
            let mut escalate = false;
            self.emit("turn", "", turn);
            if !c.text.is_empty() {
                self.state.lock().last_commentary = c.text.clone();
                self.log(json!({"turn": turn, "commentary": c.text, "phase": c.phases}));
                if c.kind == "final" {
                    match self.checked_final(goal, &c.text, turn, worker).await? {
                        None => {
                            // sent back once to look again
                            let note = self.state.lock().retry_note.clone();
                            next_input = vec![user(&note)];
                            effort = cfg.plan_reasoning_effort.clone();
                            continue;
                        }
                        Some(answer) => {
                            self.emit("final", &answer, turn);
                            return Ok(answer);
                        }
                    }
                }
                self.emit("commentary", &c.text, turn);
            }
            let mut outputs: Vec<Value> = Vec::new();
            let mut notes: Vec<Value> = Vec::new();
            for call in &c.calls {
                self.check_cancel()?;
                let out: Vec<Value> = if call.name == "exec_py" {
                    let code = call.args.get("code").map(value_str).unwrap_or_default();
                    self.emit("exec", &code, turn);
                    self.log(json!({"turn": turn, "exec": code}));
                    let out = self.interruptible(worker.execute(&code)).await?;
                    let texts = text_items(&out);
                    let images = out.iter().filter(|o| o.get("type").and_then(Value::as_str) == Some("input_image")).count();
                    let mut entry = json!({"turn": turn, "result": texts, "images": images});
                    if let Some(timing @ Value::Object(_)) = worker.last_timing() {
                        entry["timing"] = timing;
                    }
                    self.log(entry);
                    if texts.iter().any(|t| t.contains("Traceback") || t.contains("exec_py error") || t.contains("was restarted")) {
                        escalate = true;
                    }
                    // the lane handing back (a confirm, an escalate, a script that stopped) is a recovery turn too
                    const HANDBACK: [&str; 4] =
                        ["delegate escalate", "delegate confirm", "delegate script: partial", "delegate script: none"];
                    if texts.iter().any(|t| HANDBACK.iter().any(|p| t.starts_with(p))) {
                        escalate = true;
                    }
                    let after = texts.iter().rev().find(|t| t.starts_with("[after")).cloned().unwrap_or_default();
                    let no_effect = after.starts_with("[after your input]") && after.contains("screen: unchanged");
                    let repeat = {
                        let mut s = self.state.lock();
                        if !after.is_empty() {
                            s.last_after = after.clone();
                        }
                        if after.starts_with("[after your input]") {
                            s.acted = true;
                        }
                        let repeat = s.last_code.as_deref() == Some(code.as_str()) && no_effect;
                        s.last_code = Some(code.clone());
                        repeat
                    };
                    // Only a repeat whose input changed nothing earns the note: the note says the screen did not
                    // change, so it must have checked (B3).
                    if repeat {
                        notes.push(user(
                            "[note] That is the same code as the previous step and the screen did not change. \
                             Look (zoom / screen_text / observe) and try a different way.",
                        ));
                        self.log(json!({"turn": turn, "repeat": true}));
                        escalate = true;
                    }
                    let lines = progress_lines(&out);
                    if let Some(last) = lines.last() {
                        self.state.lock().last_commentary = last.clone();
                    }
                    let tail = &lines[lines.len().saturating_sub(2)..];
                    for line in tail {
                        self.emit("progress", line, turn);
                    }
                    out
                } else {
                    let asked = call.args.get("question").map(value_str).unwrap_or_default();
                    let q = {
                        let s = py::py_strip(&asked);
                        if s.is_empty() { "May I continue?".to_string() } else { s.to_string() }
                    };
                    self.emit("ask", &q, turn);
                    self.log(json!({"turn": turn, "ask": q}));
                    let answer = self.ask(&q).await;
                    self.log(json!({"turn": turn, "answer": answer}));
                    if answer.to_lowercase().starts_with("no") {
                        escalate = true;
                    }
                    vec![json!({"type": "input_text", "text": answer})]
                };
                outputs.push(json!({"type": "function_call_output", "call_id": call.call_id, "output": out}));
            }
            let steers = self.steer_items();
            if !steers.is_empty() {
                escalate = true;
            }
            if escalate {
                effort = cfg.plan_reasoning_effort.clone();
            }
            next_input = outputs;
            next_input.extend(notes);
            next_input.extend(steers);
        }
        let msg = format!("I ran out of steps ({}) before finishing.", cfg.max_turns);
        let t = self.turn();
        self.emit("final", &msg, t);
        Ok(msg)
    }

    /// The hosted classifier for wording the rules do not know, or None (EVE_ROUTER_MODEL=off, no key, anything
    /// wrong): built once per agent, and a failure to build it is simply "off".
    fn router_asker(&self) -> Option<RouterAsker> {
        if self.config.router_model != "jev" {
            return None;
        }
        let mut s = self.state.lock();
        if let AskerSlot::Unbuilt = s.asker {
            s.asker = match eve_jev::route_config(&eve_jev::ProcessEnv, "") {
                Ok((url, key, model)) => {
                    let predict = eve_jev::make_predict(
                        &url,
                        &key,
                        &model,
                        eve_jev::PredictOptions { timeout_s: 2.0, ..Default::default() },
                    );
                    let asker = eve_typed_ask::make_jev_request_asker(predict.anyhow_fn(), eve_typed_ask::perf_counter);
                    AskerSlot::Built(Arc::new(asker))
                }
                Err(e) => {
                    tracing::warn!("agent: the router model is off (JevError: {})", e);
                    AskerSlot::Failed
                }
            };
        }
        match &s.asker {
            AskerSlot::Built(a) => Some(a.clone()),
            _ => None,
        }
    }

    /// Run a reflex (task_router::Plan::code(): one open_app or open_url line) in the worker.
    ///
    /// Returns (what to say, "") when the reflex fully answered the request and the helper's own sentence
    /// confirms it; ("", a [note] for the planner) when it did its part and the planner continues; ("", "") when
    /// it failed — the planner then runs as if nothing had been tried.
    async fn reflex(&self, plan: &task_router::Plan, worker: &dyn Worker) -> Res<(String, String)> {
        let code = plan.code();
        if code.is_empty() {
            return Ok((String::new(), String::new()));
        }
        let t0 = self.now();
        let out = match self.interruptible(worker.execute(&code)).await {
            Ok(o) => o,
            Err(e) if is_stop(&e) => return Err(e),
            Err(e) => {
                // a reflex is an optimisation, never a way to fail a task
                tracing::warn!("agent: reflex failed ({}); the planner takes the task", name_and_text(&e));
                self.log(json!({"turn": 0, "reflex": {"kind": plan.kind, "error": error_name(&e)}}));
                return Ok((String::new(), String::new()));
            }
        };
        let texts = text_items(&out);
        let said = texts.iter().find(|t| t.starts_with("opened ")).cloned().unwrap_or_default();
        let ok = !said.is_empty()
            && !said.contains(" but ")
            && !texts.iter().any(|t| t.contains("Traceback") || t.contains("exec_py error"));
        let secs = self.now() - t0;
        self.log(json!({"turn": 0, "reflex": {"kind": plan.kind, "app": plan.app, "query": plan.query, "ok": ok,
                        "reasons": plan.reasons}, "exec": code, "secs": num(round(secs, 2))}));
        tracing::info!("agent: reflex {} {} in {:.2} s", plan.kind, if ok { "ok" } else { "did not confirm" }, secs);
        if !ok {
            return Ok((String::new(), String::new()));
        }
        self.emit("progress", &said, 0);
        if plan.complete() {
            return Ok((plan.sentence(), String::new()));
        }
        let did = if plan.kind == "launch" {
            format!("opened {}", plan.app)
        } else {
            format!("opened a web search for {}", py::py_repr(&plan.query))
        };
        let rest = if plan.rest.is_empty() { "what remains" } else { plan.rest.as_str() };
        Ok((
            String::new(),
            format!("\n\n[note] Before you started, a reflex already {did}. Start from the screenshot and do the rest of the request: {rest}."),
        ))
    }

    /// Ask the worker's router to finish the goal before any planner call. Whatever goes wrong here is logged
    /// and answered as "unavailable": the planner then runs exactly as before.
    async fn lane_first(&self, goal: &str, worker: &dyn Worker) -> Res<Value> {
        let t0 = self.now();
        let attempt = self
            .interruptible(async { Ok(worker.lane_first(goal).await) })
            .await?;
        let Some(result) = attempt else {
            return Ok(json!({"status": "unavailable", "reason": "this worker has no router"}));
        };
        let routed = match result {
            Ok(v @ Value::Object(_)) => v,
            Ok(_) => json!({"status": "unavailable", "reason": "bad reply"}),
            Err(e) if is_a::<Cancelled>(&e) => return Err(e),
            Err(e) => {
                // the router is an optimisation, never a way to fail a task
                tracing::warn!("agent: lane-first failed ({}); the planner takes the task", name_and_text(&e));
                json!({"status": "unavailable", "reason": error_name(&e)})
            }
        };
        let secs = round(self.now() - t0, 2);
        let mut shown = Map::new();
        for k in ["status", "reason", "clicked", "already", "line", "ms"] {
            shown.insert(k.into(), routed.get(k).cloned().unwrap_or(Value::Null));
        }
        self.log(json!({"turn": 0, "lane_first": shown, "secs": num(secs)}));
        let reason = match routed.get("reason") {
            Some(v) if truthy(v) => format!(" ({})", value_str(v)),
            _ => String::new(),
        };
        let status = routed.get("status").map(value_str).unwrap_or_else(|| "None".into());
        tracing::info!("agent: lane-first {}{} in {:.2} s", status, reason, secs);
        if let Some(Value::Array(lines)) = routed.get("log") {
            for line in &lines[lines.len().saturating_sub(2)..] {
                self.emit("progress", &value_str(line), 0);
            }
        }
        Ok(routed)
    }

    /// The browser lane's turn: a search or URL reflex is one navigation; then one plan, executed against the
    /// page. Returns (answer, "") when done, ("", note) for the turn-by-turn loop — the floor, as always: the
    /// planner then works the Chromium window from screenshots.
    async fn browser_first(&self, goal: &str, route: Option<&task_router::Plan>) -> Res<(String, String)> {
        let Some(browser) = self.browser.clone() else {
            return Ok((String::new(), String::new()));
        };
        let t0 = self.now();
        let mut note = String::new();
        let attempt: Res<(String, String)> = async {
            if let Some(r) = route.filter(|r| r.kind == "search" && !r.query.is_empty()) {
                let url = format!("{}{}", task_router::SEARCH_URL, py::quote_plus(&r.query));
                let said = self.interruptible(browser.open_url(&url)).await?;
                self.state.lock().acted = true;
                self.emit("progress", &said, 0);
                if r.rest.is_empty() {
                    self.log(json!({"turn": 0, "browser": {"reflex": "search", "secs": num(round(self.now() - t0, 2))}}));
                    return Ok((r.sentence(), String::new()));
                }
                note = format!("\n\n[note] The browser already opened {url}. Start from the page; do not open it again.");
            }
            self.plan_once(goal, browser.as_ref(), "browser").await
        }
        .await;
        match attempt {
            Ok((answer, plan_note)) => {
                let n = if plan_note.is_empty() { note } else { plan_note };
                Ok((answer, n))
            }
            Err(e) if is_stop(&e) => Err(e),
            Err(e) => {
                // a browser that fails is "today's loop", never a failed task
                tracing::warn!("agent: browser lane failed ({}); the planner takes over", name_and_text(&e));
                self.log(json!({"turn": 0, "browser": {"error": py::take(&name_and_text(&e), 200)}}));
                Ok((String::new(), note))
            }
        }
    }

    /// Plan once, execute without a planner turn between steps or at the end.
    ///
    /// Returns (what to say, "") when the plan ran to a code-checked finish or the human said no, and ("", a
    /// [note] for the turn-by-turn loop) otherwise — that loop is the floor: a plan that cannot be made, parsed
    /// or finished costs one short call and then today's behaviour, from the screen as the executor left it. A
    /// consequential step asks the human directly (`ask`), with no planner turn; nothing in a plan can pre-approve
    /// one.
    ///
    /// In the browser (lane="browser") three things differ, all measured on tools/browser_model_eval.py
    /// (2026-09-24): a blank page with a URL in the request opens that URL before planning, so the planner sees
    /// the site instead of declining an empty outline; a checkpoint, or a question whose page did not hold the
    /// answer, asks the planner again with the new page and the steps already done (MAX_BROWSER_REPLANS); and a
    /// question is answered only from the page's text, never with the executor's "Done: open url …" sentence.
    async fn plan_once(&self, goal: &str, worker: &dyn Worker, lane: &str) -> Res<(String, String)> {
        let t0 = self.now();
        let browser = lane == "browser";
        let mut outline = self.interruptible(worker.outline()).await?;
        let named = if browser { String::new() } else { task_router::find_app_mention(goal, &(self.apps)()) };
        let front = str_or_empty(outline.get("app"));
        if !named.is_empty() && eve_desktop_lanes::casefold(&named) != eve_desktop_lanes::casefold(&front) {
            // The request names an app that is not in front: the planner would plan blind (live, 2026-09-21: a
            // checkpoint instead of a click). Open it first, in the worker, and read ITS window.
            let opened: Res<Value> = async {
                self.interruptible(worker.execute(&format!("open_app({})", py::py_repr(&named)))).await?;
                self.state.lock().acted = true;
                self.interruptible(worker.outline()).await
            }
            .await;
            match opened {
                Ok(o) => outline = o,
                Err(e) if is_stop(&e) => return Err(e),
                // the plan's own open_app step is the fallback
                Err(e) => tracing::info!("agent: could not open {} before planning ({})", named, error_name(&e)),
            }
        }
        // every step applied, across plans, in order
        let mut done: Vec<String> = Vec::new();
        if browser && !outline.get("lines").is_some_and(truthy) {
            outline = self.open_named_page(goal, worker, outline, &mut done).await?;
        }
        if browser && outline.get("blocked").is_some_and(truthy) {
            // a bot check (browser_lane::browser_blocker): say so, no hand-off
            return Ok((value_str(&outline["blocked"]), String::new()));
        }
        // In the browser a question is navigation plus a read of the page's text (browser_lane page_text), not
        // "needs eyes": plan only the way to the page, then answer from what it says.
        let reads = browser && (lane_router::is_question(goal, false) || task_router::TELL_ME.is_match(goal));
        let mut asked = 0usize;
        let mut again = String::new();
        // what the last plan would have said, had its checkpoint been its end
        let mut closing = String::new();
        let mut result = json!({});
        let attempts = 1 + if browser { MAX_BROWSER_REPLANS } else { 0 };
        let mut exhausted = true;
        for attempt in 0..attempts {
            if attempt > 0 {
                outline = self.interruptible(worker.outline()).await?;
                if browser && outline.get("blocked").is_some_and(truthy) {
                    return Ok((value_str(&outline["blocked"]), String::new()));
                }
            }
            let lines: Vec<String> = match outline.get("lines") {
                Some(Value::Array(a)) => a.iter().map(value_str).collect(),
                _ => Vec::new(),
            };
            let url = str_or_empty(outline.get("url"));
            let site = if browser {
                let s = str_or_empty(outline.get("site"));
                if s.is_empty() { py::urlsplit(&url).hostname() } else { s }
            } else {
                String::new()
            };
            let vault: Vec<String> = if browser {
                match outline.get("vault") {
                    Some(Value::Array(a)) => a.iter().map(value_str).collect(),
                    _ => Vec::new(),
                }
            } else {
                Vec::new()
            };
            let input = pc::PlanInput {
                app: str_or_empty(outline.get("app")),
                outline: lines.clone(),
                apps: if browser { Vec::new() } else { (self.apps)() },
                site,
                done: done.clone(),
                again: again.clone(),
                current_url: if browser { url.clone() } else { String::new() },
                vault,
            };
            let req = pc::plan_request(
                &self.config.model,
                goal,
                &input,
                &self.config.plan_exec_effort,
                self.config.api_timeout_secs,
                reads,
                browser,
            );
            let planned: Res<Option<pc::Plan>> = async {
                let response = self.interruptible(self.create(&req, 0)).await?;
                self.meter_response(&response);
                let raw: Value = serde_json::from_str(&classify_response(&response)?.text)?;
                if browser && (reads || attempt > 0 || !done.is_empty()) && nothing_left(&raw) {
                    return Ok(None);
                }
                pc::parse_plan(&raw, goal).map(Some).map_err(|e| crate::PyError::err("PlanError", e.0))
            }
            .await;
            let plan = match planned {
                Ok(p) => p,
                Err(e) if is_stop(&e) => return Err(e),
                Err(e) => {
                    // no plan is "the planner does it turn by turn", never a failed task
                    self.log(json!({"turn": 0, "plan": {"error": py::take(&name_and_text(&e), 200), "attempt": attempt,
                                    "secs": num(round(self.now() - t0, 2))}}));
                    let why = if again.is_empty() { "no plan".to_string() } else { again.clone() };
                    return Ok((String::new(), handoff_note(&done, &why)));
                }
            };
            let Some(plan) = plan else {
                // Browser: the planner looked at the page (a question's, or one steps were already applied to) and
                // saw nothing left to do.
                self.log(json!({"turn": 0, "plan": {"steps": [], "nothing_left": true, "attempt": attempt,
                                "secs": num(round(self.now() - t0, 2))}}));
                if reads {
                    let said = self.read_answer(goal, worker).await?;
                    return Ok(if said.is_empty() {
                        (String::new(), handoff_note(&done, "the page now open did not show the answer"))
                    } else {
                        (said, String::new())
                    });
                }
                if !closing.is_empty() {
                    return Ok((closing, String::new()));
                }
                return Ok((String::new(), handoff_note(&done, "the planner saw nothing left to do")));
            };
            let mut plan_dict = obj(&pc::plan_to_dict(&plan));
            plan_dict.insert("app".into(), json!(str_or_empty(outline.get("app"))));
            if browser {
                // the page it was planned on: a search there counts (plan_contract::url_query_matches)
                plan_dict.insert("here".into(), json!(pc::bare_host_value(outline.get("url"))));
            }
            let plan_value = Value::Object(plan_dict.clone());
            let mut logged = plan_dict.clone();
            logged.insert("secs".into(), num(round(self.now() - t0, 2)));
            logged.insert("attempt".into(), json!(attempt));
            logged.insert("outline_lines".into(), json!(lines.len()));
            self.log(json!({"turn": 0, "plan": logged}));
            if plan.needs_eyes {
                let why = if again.is_empty() { "the planner needs to see the page".to_string() } else { again.clone() };
                return Ok((String::new(), handoff_note(&done, &why)));
            }
            let mut approved: IndexMap<String, String> = IndexMap::new();
            let mut start: i64 = 0;
            // this plan's ledger, across the human's yeses
            let mut ran: Vec<Value> = Vec::new();
            // its success check held before step 1 (plan_executor)
            let mut rejected = false;
            for _ in 0..=MAX_PLAN_CONFIRMS {
                result = self
                    .interruptible(worker.run_plan(&plan_value, goal, start, &approved, rejected))
                    .await?;
                rejected = rejected || result.get("success_rejected").is_some_and(truthy);
                self.log(json!({"turn": 0, "ledger": result}));
                let ledger: Vec<Value> = match result.get("ledger") {
                    Some(Value::Array(a)) => a.clone(),
                    _ => Vec::new(),
                };
                ran.extend(ledger.iter().cloned());
                for entry in &ledger {
                    if entry.get("effect").and_then(Value::as_str) != Some("refused") {
                        self.state.lock().acted = true;
                        let step = str_or_empty(entry.get("step"));
                        done.push(step.clone());
                        self.emit("progress", &step, 0);
                    }
                }
                if result.get("status").and_then(Value::as_str) != Some("needs_human") {
                    break;
                }
                start = py::int_or(result.get("next_index"), 0);
                let what = {
                    let w = str_or_empty(result.get("confirm"));
                    if w.is_empty() { "this step".to_string() } else { w }
                };
                if asked >= MAX_PLAN_CONFIRMS {
                    break;
                }
                let question = format!("Should I go ahead: {what}?");
                self.emit("ask", &question, 0);
                let said = self.ask(&question).await;
                asked += 1;
                self.log(json!({"turn": 0, "ask": question, "answer": said}));
                // fail-closed: "yeah no" is not a yes
                if !eve_consent::approves(&said) {
                    return Ok((format!("Okay, I stopped before that: {what}."), String::new()));
                }
                approved.insert(start.to_string(), what);
            }
            let status = str_or_empty(result.get("status"));
            if status == "blocked" {
                // a step landed on a bot check: never clicked, never handed off
                return Ok((str_or_empty(result.get("sentence")), String::new()));
            }
            let ran_all = py::int_or(result.get("next_index"), 0) >= plan.steps.len() as i64;
            // The planner leaves final_say empty exactly when the answer must be read off the page (READ
            // instructions); a plan with its own sentence ("Here is the top headline.") keeps it. A plan whose
            // every step ran is read even when its own `success` guess was not on the page: the read decides.
            if reads && plan.final_say.is_empty() {
                if status == "complete" || status == "checkpoint" || (status == "partial" && ran_all) {
                    let said = self.read_answer(goal, worker).await?;
                    if !said.is_empty() {
                        tracing::info!("agent: plan and page read in {:.2} s", self.now() - t0);
                        return Ok((said, String::new()));
                    }
                    again = "the page now open did not show the answer".to_string();
                    continue;
                }
                // a question is never answered with "Done: open url …"
                exhausted = false;
                break;
            }
            if status == "complete" && result.get("sentence").is_some_and(truthy) {
                tracing::info!(
                    "agent: plan ran in {:.2} s with {} planner call{}",
                    self.now() - t0,
                    attempt + 1,
                    if attempt == 0 { "" } else { "s" }
                );
                return Ok((value_str(&result["sentence"]), String::new()));
            }
            if browser && status == "checkpoint" {
                again = "the plan stopped at a checkpoint to look at the new page".to_string();
                closing = if ran_all { closing_say(&plan, &ran) } else { String::new() };
                continue;
            }
            exhausted = false;
            break;
        }
        if exhausted {
            // the re-plan budget ran out
            return Ok((String::new(), handoff_note(&done, &again)));
        }
        let why = {
            let r = str_or_empty(result.get("reason"));
            if r.is_empty() { str_or_empty(result.get("status")) } else { r }
        };
        Ok((String::new(), handoff_note(&done, &why)))
    }

    /// A blank page and a URL in the request: open it, then plan against the site. Measured on
    /// tools/browser_model_eval.py (2026-09-24, search_then_link and help_read_after_nav from a blank tab): with
    /// "none readable" as the outline gpt-6-astra and gpt-6-luna declined as needs_eyes, and the others opened
    /// the site and stopped at a checkpoint to look. Anything else keeps the blank outline.
    async fn open_named_page(&self, goal: &str, worker: &dyn Worker, outline: Value, done: &mut Vec<String>) -> Res<Value> {
        let Some(m) = GOAL_URL.find(goal) else {
            return Ok(outline);
        };
        let url = m.as_str().trim_end_matches(['.', '!', '?', ':']).to_string();
        if same_page(&url, &str_or_empty(outline.get("url"))) {
            // already open: a page with no controls (a table) stays as it is
            return Ok(outline);
        }
        let opened: Res<(String, Value)> = async {
            let said = self.interruptible(worker.open_url(&url)).await?;
            let again = self.interruptible(worker.outline()).await?;
            Ok((said, again))
        }
        .await;
        let (said, again) = match opened {
            Ok(pair) => pair,
            Err(e) if is_stop(&e) => return Err(e),
            Err(e) => {
                // the plan's own open_url step is the fallback
                tracing::info!("agent: could not open {} before planning ({})", url, error_name(&e));
                return Ok(outline);
            }
        };
        self.state.lock().acted = true;
        done.push(format!("open url {url}"));
        self.emit("progress", &said, 0);
        self.log(json!({"turn": 0, "browser": {"opened_before_planning": url}}));
        Ok(again)
    }

    /// Answer a question from the page the plan left on screen: its visible text, one short text-only call. ""
    /// on any failure (the caller then does what it would have done without the read).
    async fn read_answer(&self, goal: &str, worker: &dyn Worker) -> Res<String> {
        let attempt: Res<(Value, Option<String>)> = async {
            let page = self.interruptible(worker.page_text()).await?;
            if page.get("blocked").is_some_and(truthy) {
                self.log(json!({"turn": 0, "read": {"url": page.get("url").cloned().unwrap_or(Value::Null), "blocked": true}}));
                let said = value_str(&page["blocked"]);
                return Ok((page, Some(said)));
            }
            if page.get("signed_out").is_some_and(truthy) {
                let host = {
                    let h = py::urlsplit(&str_or_empty(page.get("url"))).hostname();
                    if h.is_empty() { "that site".to_string() } else { h }
                };
                let shown = host.strip_prefix("www.").unwrap_or(&host).to_string();
                let said = format!("You're not signed in to {shown} in Chrome, so I couldn't check.");
                self.log(json!({"turn": 0, "read": {"url": page.get("url").cloned().unwrap_or(Value::Null), "signed_out": true}}));
                return Ok((page, Some(said)));
            }
            let req = pc::read_request(
                &self.config.model,
                goal,
                &obj(&page),
                &self.config.plan_exec_effort,
                self.config.api_timeout_secs,
            );
            let response = self.interruptible(self.create(&req, 0)).await?;
            self.meter_response(&response);
            let text = classify_response(&response)?.text;
            Ok((page, Some(py::py_collapse(&text))))
        }
        .await;
        let (page, said) = match attempt {
            Ok((page, Some(said))) if page.get("blocked").is_some_and(truthy) || page.get("signed_out").is_some_and(truthy) => {
                return Ok(said);
            }
            Ok((page, said)) => (page, said.unwrap_or_default()),
            Err(e) if is_stop(&e) => return Err(e),
            Err(e) => {
                self.log(json!({"turn": 0, "read": {"error": py::take(&name_and_text(&e), 200)}}));
                return Ok(String::new());
            }
        };
        self.log(json!({"turn": 0, "read": {"url": page.get("url").cloned().unwrap_or(Value::Null),
                        "chars": str_or_empty(page.get("text")).chars().count(), "answer": py::take(&said, 200)}}));
        if said.trim_end_matches('.').to_lowercase() == pc::READ_NOT_FOUND.trim_end_matches('.').to_lowercase() {
            // not on the page: not an answer — the caller hands it on
            return Ok(String::new());
        }
        Ok(said)
    }

    /// The answer to speak, or None to send the agent back once to look again.
    ///
    /// Runs that only looked (no click, key or typing) are not checked: nothing they did could have failed
    /// silently. A failed or unreadable check lets the claim through — the check guards the answer, it must
    /// never block it.
    async fn checked_final(&self, goal: &str, claim: &str, turn: i64, worker: &dyn Worker) -> Res<Option<String>> {
        if !(self.config.verify && self.state.lock().acted) {
            return Ok(Some(claim.to_string()));
        }
        let Some((valid, guidance)) = self.verify(goal, claim, turn, worker).await? else {
            return Ok(Some(claim.to_string()));
        };
        if valid {
            return Ok(Some(claim.to_string()));
        }
        let mut s = self.state.lock();
        if !s.verify_retried {
            s.verify_retried = true;
            let seen = if guidance.is_empty() { "the screen does not show that".to_string() } else { guidance };
            s.retry_note = format!(
                "[note] A fresh screenshot does not confirm your answer: {seen} Look again, fix it if you can, and \
                 report only what the screen shows."
            );
            return Ok(None);
        }
        Ok(Some(format!("I couldn't confirm that on screen. {claim}")))
    }

    async fn verify(&self, goal: &str, claim: &str, turn: i64, worker: &dyn Worker) -> Res<Option<(bool, String)>> {
        let t0 = self.now();
        let attempt: Res<(bool, String)> = async {
            let items = self.interruptible(worker.observe()).await?;
            let images: Vec<Value> = items
                .iter()
                .filter(|o| o.get("type").and_then(Value::as_str) == Some("input_image"))
                .take(1)
                .cloned()
                .collect();
            let last_after = {
                let a = self.state.lock().last_after.clone();
                if a.is_empty() { "(none)".to_string() } else { a }
            };
            let mut content = vec![json!({"type": "input_text", "text": format!(
                "Goal: {goal}\nAgent's final message: {claim}\nAgent's last status line: {last_after}")})];
            content.extend(images);
            let req = json!({
                "model": self.config.model,
                "instructions": VERIFY_INSTRUCTIONS,
                "input": [{"type": "message", "role": "user", "content": content}],
                "reasoning": {"effort": self.config.verify_reasoning_effort},
                "text": {"format": {"type": "json_schema", "name": "verdict", "schema": VERIFY_SCHEMA.clone(),
                                    "strict": true}},
                "store": false,
                "timeout": self.config.api_timeout_secs,
            });
            let response = self.interruptible(self.create(&req, turn)).await?;
            verdict(&response)
        }
        .await;
        match attempt {
            Ok((valid, guidance)) => {
                self.log(json!({"turn": turn, "verify": {"valid": valid, "guidance": guidance,
                                "secs": num(round(self.now() - t0, 2))}}));
                Ok(Some((valid, guidance)))
            }
            Err(e) if is_a::<Cancelled>(&e) => Err(e),
            Err(e) => {
                // a broken check must not eat the answer
                tracing::warn!("agent: final-answer check failed ({}); letting the answer through", e);
                self.log(json!({"turn": turn, "verify": {"error": error_name(&e)}}));
                Ok(None)
            }
        }
    }

    fn steer_items(&self) -> Vec<Value> {
        let queued: Vec<String> = std::mem::take(&mut self.state.lock().steer);
        queued.into_iter().filter(|t| !t.is_empty()).map(|t| user(&format!("[steer] {t}"))).collect()
    }

    async fn ask(&self, question: &str) -> String {
        let asker = self.ask_user.lock().clone();
        let Some(asker) = asker else {
            return "no (nobody is listening right now — do not proceed with that action)".to_string();
        };
        match asker(question.to_string()).await {
            Ok(a) => {
                let a = py::py_strip(&a).to_string();
                if a.is_empty() { "no (no answer — do not proceed with that action)".to_string() } else { a }
            }
            Err(e) => {
                tracing::warn!("agent: ask_user failed: {}", e);
                "no (could not reach the human — do not proceed with that action)".to_string()
            }
        }
    }

    fn check_cancel(&self) -> Res<()> {
        if *self.cancel_tx.borrow() {
            return Err(cancelled());
        }
        Ok(())
    }

    fn emit(&self, kind: &str, text: &str, turn: i64) {
        let f = self.on_event.lock().clone();
        if let Some(f) = f {
            let ev = AgentEvent::with_turn(kind, text, turn);
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&ev))).is_err() {
                tracing::error!("agent: on_event failed");
            }
        }
    }

    // -- action log --

    fn open_log(&self, goal: &str) {
        let dir = self.config.runs_dir.clone();
        let made = (|| -> std::io::Result<PathBuf> {
            make_private_dir(&dir)?;
            let stamp = chrono::Local::now().format("%Y-%m-%d-%H%M%S").to_string();
            Ok(dir.join(format!("{stamp}.jsonl")))
        })();
        match made {
            Ok(path) => {
                self.state.lock().run_log = Some(path);
                self.log(json!({"goal": goal, "model": self.config.model}));
            }
            Err(e) => {
                tracing::warn!("agent: cannot write the action log ({})", e);
                self.state.lock().run_log = None;
            }
        }
    }

    fn log(&self, entry: Value) {
        let Some(path) = self.state.lock().run_log.clone() else {
            return;
        };
        let mut line = Map::new();
        line.insert("t".into(), num(round(self.now(), 3)));
        if let Value::Object(m) = entry {
            for (k, v) in m {
                line.insert(k, v);
            }
        }
        let text = py::dumps_unicode(&Value::Object(line)) + "\n";
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = f.write_all(text.as_bytes());
        }
    }
}

/// `mkdir(parents=True, exist_ok=True, mode=0o700)`: the parents with the default mode, the folder itself 0700.
fn make_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut b = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    match b.create(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
        Err(e) => Err(e),
    }
}

/// The [note] the turn-by-turn loop (or Codex) starts from, "" when nothing was applied at all.
pub fn handoff_note(done: &[String], why: &str) -> String {
    if done.is_empty() {
        return String::new();
    }
    let why = if why.is_empty() { "unfinished" } else { why };
    format!(
        "\n\n[note] Before you started, a plan already did, in order: {}. It stopped there ({why}). Start from the \
         screenshot; do not repeat those steps.",
        done.join("; ")
    )
}
