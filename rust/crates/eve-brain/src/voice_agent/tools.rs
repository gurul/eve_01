//! A port of `voice_agent.py`, lines 1583-2240: VoiceSession: tool calls, the gate, tasks, look/photo/head, intents, speech.
//!
//! Rust: `st` is never held across an await or while another method that takes it runs. A tool's or a background
//! job's Python `except Exception` is a caught panic or an `Err` here. A Live send that fails is logged; Python let
//! it raise out of the task that sent it (the conversation's event task, a background send, a computer task's
//! question), and the question path still does ([`VoiceSession::ask_user_raising`]).

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::voice_agent::*;

use futures::FutureExt as _;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Weak;
use std::time::Duration;

/// py: voice_agent.py:1873 `VoiceSession.PHOTO_TIMEOUT_SECS` — a class attribute Python reads at call time (a test
/// sets it shorter), so a settable value here.
pub static PHOTO_TIMEOUT_SECS: LazyLock<Mutex<f64>> = LazyLock::new(|| Mutex::new(20.0));

/// scene.py's `LOOK_FRESH_SECS`: the default of `scene.look()` (the look tool reuses a view this young).
const SCENE_LOOK_FRESH_SECS: f64 = 4.0;

/// The tools answered from a background job (`_slow_tool`) besides the memory, Meet, app, place, science and NYT
/// tools.
const SLOW_TOOLS: &[&str] = &["look", "look_around", "find", "think_hard", "lesson", "take_photo"];

/// The head tools: their call is a head tool that ran this turn.
const HEAD_TOOLS: &[&str] = &["move_head", "look_around", "find", "look"];

/// `asyncio.sleep(secs)`.
async fn sleep_secs(secs: f64) {
    if secs.is_finite() && secs > 0.0 {
        tokio::time::sleep(Duration::from_secs_f64(secs)).await;
    } else {
        tokio::task::yield_now().await;
    }
}

/// Python truthiness of a JSON value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Python's `str(v)` of a JSON value (its `%s` in a log line).
fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `str(args.get(key, default))`.
fn arg_str(args: &Obj, key: &str, default: &str) -> String {
    args.get(key).map(py_str).unwrap_or_else(|| default.to_string())
}

/// `str(args.get(key) or "")`.
fn arg_or_empty(args: &Obj, key: &str) -> String {
    match args.get(key) {
        Some(v) if truthy(Some(v)) => py_str(v),
        _ => String::new(),
    }
}

/// `json.dumps(value)`: Python's separators, non-ASCII escaped.
fn dumps(value: &Value) -> String {
    eve_agentcard::py::dumps(value, false, true)
}

/// A refusal: `{"ok": False, "reason": reason}`.
fn refusal(reason: &str) -> Obj {
    let mut out = Obj::new();
    out.insert("ok".into(), Value::Bool(false));
    out.insert("reason".into(), Value::String(reason.to_string()));
    out
}

/// A dict the tools return, from a JSON value (`dict(value)`; anything else is an empty dict).
fn obj_of(value: Value) -> Obj {
    match value {
        Value::Object(m) => m,
        _ => Obj::new(),
    }
}

impl VoiceSession {
    /// py: voice_agent.py:1583 `async def _tool(self, name: str, call_id: str, arguments: str) -> None:`
    pub(crate) async fn tool(self: &Arc<Self>, name: &str, call_id: &str, arguments: &str) {
        let args: Obj = if arguments.is_empty() {
            Obj::new()
        } else {
            // a JSON list or string: every tool reads args with .get
            match serde_json::from_str::<Value>(arguments) {
                Ok(Value::Object(m)) => m,
                _ => Obj::new(),
            }
        };
        let now = (self.clock)();
        let taint = {
            let mut st = self.st.lock();
            st.tool_calls.push((name.to_string(), args.clone()));
            st.last_activity = now;
            if HEAD_TOOLS.contains(&name) {
                st.last_head_tool_at = now;
            }
            st.taint.clone()
        };
        let gated = if eve_tool_gate::needs_confirmation(name, &args, &taint) && !self.owner_answered(name, &args) {
            self.gate(name, &args)
        } else {
            None
        };
        let (listening, memory_tool) = {
            let st = self.st.lock();
            (st.think_aloud.is_some(), st.memory_tool_names.contains(name))
        };
        let result: Value = if let Some(gated) = gated {
            // A consequential native action requires its exact spoken approval (tool_gate.py).
            Value::Object(gated)
        } else if listening && (["start_task", "go_explore", "think_hard"].contains(&name) || memory_tool) {
            // A learner is working a problem: eve does not take over the Mac, wander off, or solve it elsewhere.
            Value::Object(refusal("not while the learner is thinking out loud; listen and give a hint"))
        } else {
            match name {
                "start_task" => Value::Object(self.start_task(arg_str(&args, "goal", "").trim())),
                "steer_task" => {
                    let agent = self.st.lock().agent.clone();
                    let ok = agent.is_some_and(|a| a.steer(&arg_str(&args, "text", "")));
                    if ok {
                        json!({"ok": true})
                    } else {
                        json!({"ok": false, "reason": "no task is running"})
                    }
                }
                "stop_task" => {
                    let agent = self.st.lock().agent.clone();
                    match agent {
                        Some(agent) if self.task_running() => {
                            agent.cancel("you asked me to stop");
                            json!({"ok": true})
                        }
                        _ => json!({"ok": false, "reason": "no task is running"}),
                    }
                }
                "task_status" => {
                    let agent = self.st.lock().agent.clone();
                    match agent {
                        Some(agent) => {
                            let status = agent.status();
                            let mut st = self.st.lock();
                            if st.taint.is_empty() {
                                // the running task's own words: outside text (tool_gate.CLEAN leaves task_status out)
                                st.taint = eve_tool_gate::TASK_RESULT.to_string();
                            }
                            status
                        }
                        None => json!({"running": false}),
                    }
                }
                "answer_question" => {
                    let ans = arg_str(&args, "answer", "").trim().to_string();
                    let pending = self.st.lock().pending_answer.clone();
                    match pending {
                        Some(p) if !p.done() => {
                            p.set_result(ans);
                            json!({"ok": true})
                        }
                        _ => json!({"ok": false, "reason": "no question is pending"}),
                    }
                }
                "end_conversation" => {
                    self.ended.set();
                    json!({"ok": true})
                }
                "go_explore" => {
                    // Same shape as end_conversation: the send-off has been said, the
                    // conversation closes, and the daemon starts the explore once
                    // the board is free of the conversation pose.
                    if self.task_running() {
                        json!({"ok": false, "reason": "a task is running; stop it first"})
                    } else {
                        self.st.lock().explore_requested = true;
                        if let Some(on_explore) = &self.on_explore {
                            match catch_unwind(AssertUnwindSafe(|| on_explore())) {
                                Ok(fut) => {
                                    crate::seams::spawn_ctx(async move {
                                        if let Err(e) = fut.await {
                                            tracing::error!("voice: on_explore failed: {e}");
                                        }
                                    });
                                }
                                Err(_) => tracing::error!("voice: on_explore failed"),
                            }
                        }
                        self.ended.set();
                        json!({"ok": true})
                    }
                }
                "move_head" => {
                    let (fast, turn_started_at) = {
                        let st = self.st.lock();
                        (st.fast_head.clone(), st.user_turn_started_at)
                    };
                    match fast {
                        // the fast path already turned the head for THIS turn: a relative move must never run twice
                        Some((at, done)) if at >= turn_started_at => Value::Object(done),
                        _ => Value::Object(self.move_head(&args).await),
                    }
                }
                "set_sound" => match args.get("on") {
                    Some(Value::Bool(on)) => {
                        self.set_sound(*on, "the backend");
                        json!({"ok": true, "sound": if *on { "on" } else { "off" }})
                    }
                    _ => json!({"ok": false, "reason": "on must be true or false"}),
                },
                _ if SLOW_TOOLS.contains(&name)
                    || name == eve_web::websearch::TOOL_NAME
                    || memory_tool
                    || MEET_TOOL_NAMES.contains(&name)
                    || eve_app_registry::owner(name, true).is_some()
                    || eve_places::phone_location::TOOL_NAMES.contains(&name)
                    || eve_science_search::TOOL_NAMES.contains(&name)
                    || eve_nyt::TOOL_NAMES.contains(&name) =>
                {
                    // Seconds (or a minute, for think_hard) of camera, head, model or disk work:
                    // answered from a background task, so Live events (the owner talking,
                    // captions) keep flowing meanwhile.
                    self.slow_tool(name, call_id, &args);
                    return;
                }
                _ => json!({"ok": false, "reason": format!("unknown tool {name}")}),
            }
        };
        let item = json!({"type": "function_call_output", "call_id": call_id, "output": dumps(&result)});
        if let Err(e) = self.conn.response_item_create(item).await {
            tracing::warn!("voice: {name} result not sent: {e}");
            return;
        }
        if !self.ended.is_set() {
            self.request_response().await;
        }
    }

    /// Close the owner's open turn now, so its words are heard (`_on_user_turn`) before a decision reads them.
    fn close_open_owner_turn(self: &Arc<Self>) {
        let closing = {
            let mut st = self.st.lock();
            if st.turn_marks.speaker.as_deref() == Some("user") {
                st.turn_marks.flush()
            } else {
                None
            }
        };
        if let Some((who, text)) = closing {
            self.close_turn(&who, &text);
        }
    }

    /// py: voice_agent.py:1676 `def _owner_answered(self, name: str, args: dict[str, Any]) -> bool:`
    ///
    /// Pass the owner's exact answer to the question it followed, once, without asking again.
    ///
    /// Bare yes/no synonyms may be normalized; free words must match the owner's transcript exactly.
    /// A model-authored answer, a stale answer or an answer for a different question still needs confirmation.
    pub(crate) fn owner_answered(self: &Arc<Self>, name: &str, args: &Obj) -> bool {
        if name != "answer_question" || self.st.lock().pending_answer.is_none() {
            return false;
        }
        // the owner's answer may still be an open turn: close it first, so its words are heard (_on_user_turn)
        self.close_open_owner_turn();
        let mut st = self.st.lock();
        let Some((turn, said, answers)) = st.decision.clone() else { return false };
        let supplied = arg_str(args, "answer", "").trim().to_string();
        let matched = if !said.is_empty() {
            eve_consent::bare_decision(&supplied) == said
        } else {
            supplied == st.answer_words
        };
        if !(answers == "task" || answers == "task-gate")
            || turn <= st.question_turn
            || turn == st.used_turn
            || supplied.is_empty()
            || !matched
        {
            // a yes said while the gate's question was the one asked last is the gate's, never the task's
            return false;
        }
        st.used_turn = turn; // this yes or no answers this one question only
        true
    }

    /// py: voice_agent.py:1701 `def _gate(self, name: str, args: dict[str, Any]) -> Optional[dict[str, Any]]:`
    ///
    /// The taint gate out loud: None when this exact call was asked about and the owner has since said a bare
    /// yes (it runs), else the call's refusal. The question is code's, never the model's (v3 safety review: a
    /// tainted model wrote its own question, and could ask about one act and call another): tool_gate.spoken_question
    /// says the held act from its name and arguments, handed to the voice word for word, and the yes approves that
    /// exact call (same name, same arguments) only. A call that cannot be said briefly is refused unasked.
    ///
    /// The limit (round-4 review): the Live model has no say-this-verbatim path and eve has no separate TTS here,
    /// so the question goes as commentary and the model could paraphrase it. The yes counts only after the voice's
    /// own output transcript held the act whole (tool_gate.said_whole, checked in _close_turn); otherwise the held
    /// call is dropped and asked again. The transcript is the model's account of its audio: words, not sound.
    pub(crate) fn gate(self: &Arc<Self>, name: &str, args: &Obj) -> Option<Obj> {
        // json.dumps(args, sort_keys=True, default=str): the exact call, keys in order
        let key = format!("{name} {}", eve_agentcard::py::dumps(&Value::Object(args.clone()), true, true));
        if self.st.lock().held.is_some() {
            // the owner's answer may still be an open turn: close it first, so its yes is heard (_on_user_turn)
            self.close_open_owner_turn();
        }
        let taint = {
            let mut st = self.st.lock();
            if let Some((held_key, held_turn)) = st.held.clone() {
                if held_key == key && st.owner_yes > held_turn && st.owner_yes != st.used_turn {
                    st.held = None;
                    st.used_turn = st.owner_yes; // used once: never also a task's answer
                    drop(st);
                    tracing::info!("voice: gate: {name} runs on the owner's spoken yes");
                    return None;
                }
            }
            st.taint.clone()
        };
        let read = if taint.is_empty() { "your request".to_string() } else { taint };
        let asked = eve_tool_gate::spoken_question(name, args, &read);
        let act = eve_tool_gate::say_act(name, args);
        let (Some(asked), Some(_)) = (asked, act) else {
            // too long to say whole: refused, never held for a yes (tool_gate.SAY_CHARS)
            self.st.lock().held = None;
            tracing::info!("voice: gate: {name} refused unasked (too long to say)");
            return Some(obj_of(eve_tool_gate::TOO_LONG_ALOUD.clone()));
        };
        {
            let mut st = self.st.lock();
            st.held = Some((key, st.owner_turns));
            st.held_act = asked.clone();
            st.held_said = false;
            // A gate question about answering the task's question is both: its yes runs the held answer_question
            // call (round-4 verify: marking it "task" alone locked a free-words answer out), and the owner's own
            // bare yes or no still answers the task. One turn's word is used once either way (_used_turn).
            st.asked_last = if name == "answer_question" { "task-gate" } else { "gate" }.to_string();
        }
        tracing::info!("voice: gate: {name} needs confirmation; eve asks out loud first");
        let me = self.clone();
        let content = format!("{}{asked}", eve_tool_gate::SAY_EXACTLY);
        self.bg(async move { me.speak(&content).await });
        Some(obj_of(eve_tool_gate::ASKED_ALOUD.clone()))
    }

    /// py: voice_agent.py:1741 `def _start_task(self, goal: str) -> dict[str, Any]:`
    pub(crate) fn start_task(self: &Arc<Self>, goal: &str) -> Obj {
        if goal.is_empty() {
            return refusal("empty goal");
        }
        if !self.agent_enabled {
            return refusal("computer control is disabled (EVE_COMPUTER_CONTROL=0)");
        }
        if self.task_running() {
            return refusal("a task is already running; steer or stop it first");
        }
        if (self.mac_busy)() {
            // Two agents on one mouse is not a race worth having: the texted task keeps the Mac.
            return refusal(
                "a task the owner texted is using the Mac right now; it has to finish or be stopped from the chat first",
            );
        }
        // The task's callbacks hold the session weakly: the session holds the task.
        let weak: Weak<VoiceSession> = Arc::downgrade(self);
        let on_event: OnEvent = {
            let weak = weak.clone();
            Arc::new(move |ev: &eve_agent_contract::AgentEvent| {
                if let Some(me) = weak.upgrade() {
                    me.on_agent_event(ev);
                }
            })
        };
        let ask_user: AskUser = Arc::new(move |question: String| {
            let weak = weak.clone();
            async move {
                match weak.upgrade() {
                    Some(me) => me.ask_user_raising(&question).await,
                    None => Err(anyhow::anyhow!("the conversation closed")),
                }
            }
            .boxed()
        });
        let agent = (self.agent_factory)(on_event, ask_user, None);
        self.st.lock().agent = Some(agent);
        let me = self.clone();
        let goal_owned = goal.to_string();
        let job = Job::spawn("voice-agent", async move {
            me.run_agent(&goal_owned).await;
        });
        self.st.lock().agent_task = Some(job);
        // Started, not done. The voice gets that as silent context, and the face goes to
        // "working" once the reply on screen has been read — it used to wait on the
        // backend's own reply and came up 8-11 s late (task review, 2026-09-10).
        let me = self.clone();
        let note = format!(
            "A computer task has started: {goal}. Nothing is done yet; its result arrives on its own when it finishes."
        );
        self.bg(async move { me.quiet(&note).await });
        self.set_after_captions("working");
        let mut out = Obj::new();
        out.insert("ok".into(), Value::Bool(true));
        out.insert("goal".into(), Value::String(goal.to_string()));
        out
    }

    /// py: voice_agent.py:1763 `def _slow_tool(self, name: str, call_id: str, args: dict[str, Any]) -> None:`
    pub(crate) fn slow_tool(self: &Arc<Self>, name: &str, call_id: &str, args: &Obj) {
        let me = self.clone();
        let (name, call_id, args) = (name.to_string(), call_id.to_string(), args.clone());
        let job = Job::spawn("voice-slow-tool", async move {
            me.slow_tool_run(&name, &call_id, &args).await;
            // done: out of the set (Python's done callback discards it)
            me.st.lock().slow_tasks.retain(|j| !j.is_current() && !j.done());
        });
        let mut st = self.st.lock();
        st.slow_tasks.retain(|j| !j.done());
        if !job.done() {
            st.slow_tasks.push(job);
        }
    }

    /// The body of `_slow_tool`'s `run()`.
    async fn slow_tool_run(self: &Arc<Self>, name: &str, call_id: &str, args: &Obj) {
        let computed = AssertUnwindSafe(self.slow_result(name, args)).catch_unwind().await;
        let result: Value = match computed {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => {
                tracing::error!("voice: {name} failed: {e:#}");
                json!({"ok": false, "reason": format!("{name} failed")})
            }
            Err(_) => {
                tracing::error!("voice: {name} failed");
                json!({"ok": false, "reason": format!("{name} failed")})
            }
        };
        if self.ended.is_set() {
            return;
        }
        let now = (self.clock)();
        let memory_tool = {
            let mut st = self.st.lock();
            st.last_activity = now;
            st.memory_tool_names.contains(name)
        };
        let output = if memory_tool {
            clip_output(&obj_of(result.clone()), MEMORY_RESULT_CHARS as i64) // _memory_tool logged it
        } else {
            // What the camera saw, or what was asked, is never logged: only whether the call worked.
            let ok = result.get("ok").map(py_str).unwrap_or_else(|| "None".into());
            let found = result.get("found").map(|f| format!(" found={}", py_str(f))).unwrap_or_default();
            tracing::info!("voice: {name} → ok={ok}{found}");
            dumps(&result)
        };
        self.capture_result(name, &result);
        let tainted = !self.st.lock().taint.is_empty();
        if !tainted && eve_tool_gate::taints(name, args) {
            // a result with words from outside eve taints the rest of the conversation (tool_gate.py)
            match eve_tool_gate::source(name) {
                Ok(source) => self.st.lock().taint = source,
                Err(e) => {
                    // Python raised here, out of the task, before the result was sent
                    tracing::error!("voice: {name}: {e}");
                    return;
                }
            }
        }
        let item = json!({"type": "function_call_output", "call_id": call_id, "output": output});
        if let Err(e) = self.conn.response_item_create(item).await {
            tracing::warn!("voice: {name} result not sent: {e}");
            return;
        }
        if !self.ended.is_set() {
            self.request_response().await;
        }
    }

    /// What a slow tool answers (`_slow_tool`'s dispatch); an error is the call raising.
    async fn slow_result(self: &Arc<Self>, name: &str, args: &Obj) -> anyhow::Result<Value> {
        if name == "lesson" {
            return self.lesson_tool(args).await;
        }
        let memory_tool = self.st.lock().memory_tool_names.contains(name);
        let result = if name == "look" {
            Value::Object(self.look().await)
        } else if name == "look_around" {
            Value::Object(self.look_around().await)
        } else if name == "think_hard" {
            Value::Object(self.think_hard(&arg_str(args, "question", "")).await)
        } else if name == eve_web::websearch::TOOL_NAME {
            // websearch.search (the engine, or routed by Jev), off the loop: a second or two of network
            let query = arg_str(args, "query", "");
            let config = self.config.search.clone();
            tokio::task::spawn_blocking(move || {
                eve_web::websearch::search(
                    eve_web::Ctx::global(),
                    &query,
                    Some(&config),
                    &eve_web::websearch::EngineOpts::default(),
                )
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?
        } else if name == "take_photo" {
            Value::Object(self.take_photo(&arg_str(args, "note", "")).await)
        } else if memory_tool {
            Value::Object(self.memory_tool(name, args).await)
        } else if MEET_TOOL_NAMES.contains(&name) {
            Value::Object(self.meeting(name, args).await)
        } else if let Some(app) = eve_app_registry::owner(name, true) {
            // eve's apps (app_registry.APPS): a Bluetooth light can take seconds to connect, Spotify is a
            // Web API round trip, an ``input`` key on the TV starts a Java VM there (~1.2 s), a camera move
            // reads the camera back until it stops: slow tools, like the camera
            let hub = self.hubs.lock().get(&app.slot);
            match hub {
                Some(hub) => hub.handle(name, &Value::Object(args.clone())).await,
                None => {
                    let reason = app
                        .module_info()
                        .and_then(|m| m.off_reason.clone())
                        .ok_or_else(|| anyhow::anyhow!("module {} has no attribute OFF_REASON", app.module))?;
                    Value::Object(refusal(&reason))
                }
            }
        } else if eve_places::phone_location::TOOL_NAMES.contains(&name) {
            // the phone's latest fix (phone_location.py): a small file read, here so the taint gate sees
            // its result as it sees every read's (the place name is the phone's geocoder's words)
            Value::Object(eve_places::phone_location::handle(name, args, None).await)
        } else if eve_science_search::TOOL_NAMES.contains(&name) {
            // papers' titles and abstracts (science_search.py): a second of network; outside words, so the
            // taint gate marks the conversation after it, as after a web search
            eve_science_search::handle(name, &Value::Object(args.clone()), None, &Default::default()).await
        } else if eve_nyt::TOOL_NAMES.contains(&name) {
            // The New York Times (nyt.py): seconds of network (an archive month, longer, once); outside
            // words, so the taint gate marks the conversation after it, as after a web search
            eve_nyt::handle(name, &Value::Object(args.clone()), None, &eve_nyt::Opts::default()).await
        } else {
            Value::Object(self.find(&arg_str(args, "target", "")).await)
        };
        Ok(result)
    }

    /// The lesson tool (`_slow_tool`'s `name == "lesson"` branch).
    async fn lesson_tool(self: &Arc<Self>, args: &Obj) -> anyhow::Result<Value> {
        let action = arg_or_empty(args, "action").trim().to_lowercase();
        if self.st.lock().think_aloud.is_some() {
            // The check, hint or step must see what the learner just said: close their open
            // turn and wait for its line to reach the lesson first.
            self.close_open_owner_turn();
            let saves: Vec<Job> = self.st.lock().idea_saves.clone();
            for save in &saves {
                save.join().await;
            }
        }
        let listening = self.st.lock().think_aloud.is_some();
        if listening && action == "stop-listening" {
            // Said out loud: eve's goodbye is spoken first, then the session closes.
            self.begin_farewell((self.clock)());
            return Ok(json!({"ok": true, "answer": "Say a two- or three-word goodbye."}));
        }
        if listening && action == "ideas" {
            // Never overwrite typed ideas: spoken thinking is already appended turn by turn.
            return Ok(json!({"ok": true, "answer": "Already saved: what the learner says is added to their ideas."}));
        }
        if self.learning.is_some() && self.task_running() {
            return Ok(json!({"ok": false, "reason": "Stop the computer task before starting a lesson."}));
        }
        // run_lesson validates the model's arguments and maps a missing workspace,
        // a user error and a stale lesson to a reason the voice can say.
        let learning = self.learning.clone();
        let request = Value::Object(args.clone());
        let result = tokio::task::spawn_blocking(move || eve_learning::run_lesson(learning.as_deref(), &request))
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let ok = truthy(result.get("ok"));
        if ok && action == "open" {
            self.st.lock().lesson_setup = true;
        } else if ok && (action == "start" || action == "end") {
            self.st.lock().lesson_setup = false;
        }
        Ok(Value::Object(result))
    }

    /// py: voice_agent.py:1859 `async def _meeting(self, name: str, args: dict[str, Any]) -> dict[str, Any]:`
    ///
    /// join_meeting / leave_meeting: the same Meeter the chat uses (a calendar read can take a second or two,
    /// so it runs as a slow tool).
    pub(crate) async fn meeting(self: &Arc<Self>, name: &str, args: &Obj) -> Obj {
        let Some(meeter) = &self.meeter else {
            return refusal("joining meetings is off on this computer");
        };
        if name == "leave_meeting" {
            return obj_of(meeter.leave());
        }
        let meeting = arg_or_empty(args, "meeting").trim().to_string();
        let meeting = if meeting.is_empty() { "now".to_string() } else { meeting };
        match meeter.join(&meeting).await {
            Ok(out) => obj_of(out),
            Err(e) => {
                // Python raised out of join; the slow tool answers that the call failed
                tracing::error!("voice: {name} failed: {e:#}");
                refusal(&format!("{name} failed"))
            }
        }
    }

    /// py: voice_agent.py:1868 `async def _look(self) -> dict[str, Any]:`
    pub(crate) async fn look(self: &Arc<Self>) -> Obj {
        let Some(scene) = &self.scene else {
            return refusal("vision is not set up on this computer");
        };
        obj_of(scene.look(SCENE_LOOK_FRESH_SECS, None).await)
    }

    /// py: voice_agent.py:1875 `async def _take_photo(self, note: str) -> dict[str, Any]:`
    ///
    /// The owner asked for a picture: the daemon snaps, keeps and captions it.
    pub(crate) async fn take_photo(self: &Arc<Self>, note: &str) -> Obj {
        let Some(on_photo) = &self.on_photo else {
            return refusal("no camera to take a photo with right now");
        };
        let timeout = *PHOTO_TIMEOUT_SECS.lock();
        let timeout = if timeout.is_finite() && timeout > 0.0 { timeout } else { 0.0 };
        match tokio::time::timeout(Duration::from_secs_f64(timeout), on_photo(note.to_string())).await {
            Ok(out) => out,
            Err(_) => refusal("the camera did not answer in time"),
        }
    }

    /// py: voice_agent.py:1884 `async def _look_around(self) -> dict[str, Any]:`
    pub(crate) async fn look_around(self: &Arc<Self>) -> Obj {
        let (Some(head), Some(scene)) = (&self.head, &self.scene) else {
            return refusal("head control or vision is not set up on this computer");
        };
        let scene: &dyn eve_head::Scene = &**scene;
        obj_of(eve_head::look_around(head, scene, eve_head::SWEEP, eve_head::SETTLE_SECS).await)
    }

    /// py: voice_agent.py:1889 `async def _find(self, target: str) -> dict[str, Any]:`
    pub(crate) async fn find(self: &Arc<Self>, target: &str) -> Obj {
        let (Some(head), Some(scene)) = (&self.head, &self.scene) else {
            return refusal("head control or vision is not set up on this computer");
        };
        let scene: &dyn eve_head::Scene = &**scene;
        obj_of(
            eve_head::find(head, scene, target, eve_head::SWEEP, eve_head::SETTLE_SECS, eve_head::DEFAULT_HOLD_SECS)
                .await,
        )
    }

    /// py: voice_agent.py:1895 `async def _think_hard(self, question: str) -> dict[str, Any]:`
    pub(crate) async fn think_hard(self: &Arc<Self>, question: &str) -> Obj {
        let question = question.trim();
        if question.is_empty() {
            return refusal("empty question");
        }
        let Some(thinker) = &self.thinker else {
            return refusal("deep reasoning is off on this computer; answer as best you can");
        };
        // The voice was told to keep the owner company; a fresh reply from the
        // backend is not needed until the answer is in.
        self.quiet(
            "A hard question is being worked on in the background. Say a few words that \
             you are on it if you have not already; do not guess the answer.",
        )
        .await;
        let context = if self.memory.is_none() {
            String::new()
        } else {
            // The slow brain answers "what should I get my sister" better knowing who the owner is and what
            // was said today (think.py bounds the context).
            let me = self.clone();
            tokio::task::spawn_blocking(move || me.think_context()).await.unwrap_or_default()
        };
        let answered = AssertUnwindSafe(thinker(question.to_string(), context)).catch_unwind().await;
        match answered {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => {
                tracing::warn!("voice: think_hard failed: {e:#}");
                refusal("the slow brain did not answer in time; answer as best you can")
            }
            Err(_) => {
                tracing::warn!("voice: think_hard failed");
                refusal("the slow brain did not answer in time; answer as best you can")
            }
        }
    }

    /// py: voice_agent.py:1915 `async def _move_head(self, args: dict[str, Any]) -> dict[str, Any]:`
    pub(crate) async fn move_head(self: &Arc<Self>, args: &Obj) -> Obj {
        let Some(head) = &self.head else {
            return refusal("head control is not set up on this computer");
        };
        // a number (never a bool); anything else is dropped, not guessed
        let num = |key: &str| match args.get(key) {
            Some(Value::Number(n)) => n.as_f64(),
            _ => None,
        };
        let hold = num("hold_secs").unwrap_or(eve_head::DEFAULT_HOLD_SECS);
        let relative = matches!(args.get("relative"), Some(Value::Bool(true)));
        obj_of(head.move_to(num("yaw"), num("pitch"), relative, hold).await)
    }

    /// py: voice_agent.py:1927 `async def _stop_scene(self) -> None:`
    pub(crate) async fn stop_scene(self: &Arc<Self>) {
        if let Some(scene) = &self.scene {
            scene.stop().await;
        }
    }

    /// py: voice_agent.py:1932 `def _on_user_turn(self, text: str) -> None:`
    pub(crate) fn on_user_turn(self: &Arc<Self>, text: &str) {
        if self.ended.is_set() || self.st.lock().farewell {
            return;
        }
        // The owner's turn ends when eve starts answering it (or on a silence gap),
        // so this instant is also where eve's reply to it begins.
        let said_at = (self.clock)();
        let bare = eve_consent::bare_decision(text);
        let (listening, lesson_setup, turn_started_at) = {
            let mut st = self.st.lock();
            st.owner_turns += 1;
            // the yes or no answers the question asked last (_owner_answered, _gate)
            st.decision = Some((st.owner_turns, bare.to_string(), st.asked_last.clone()));
            st.answer_words = text.trim().to_string();
            if st.held.is_some() {
                // the gate's question is answered only by a bare spoken yes (tool_gate.py, _gate) said while it was
                // the question asked last; anything else drops the held call, so a later yes to something else (a
                // task's question asked after it) cannot approve it
                if bare == "allow" && (st.asked_last == "gate" || st.asked_last == "task-gate") && st.held_said {
                    st.owner_yes = st.owner_turns;
                } else {
                    st.held = None;
                }
            }
            (st.think_aloud.is_some(), st.lesson_setup, st.user_turn_started_at)
        };
        if listening && eve_learning::think_aloud::said_done(&eve_intent::normalize(text)) {
            tracing::info!("voice: the learner is done thinking out loud");
            self.begin_farewell(said_at);
            return;
        }
        if self.head_pose.is_some() && self.head.is_some() && !listening && HEAD_WORDS.is_match(text) {
            // "look left" used to wait ~3.2 s for the backend to pick the numbers. A typed-decision model picks
            // from a closed set of poses in a quarter of a second; only a turn that mentions a direction or the
            // head is ever sent to it. The ordinary routing below still runs, and sees that the head has moved.
            let me = self.clone();
            let words = text.to_string();
            self.bg(async move { me.fast_head_move(&words, turn_started_at).await });
        }
        if let Some(label) = eve_intent::fast_intent(text) {
            self.apply_intent(label, text, "phrase", said_at);
        } else if lesson_setup && !listening {
            // Choosing the lesson: the answer belongs to the backend, whatever words it uses.
            let me = self.clone();
            let words = text.to_string();
            self.bg(async move { me.route_lesson(&words, turn_started_at, true).await });
        } else if self.intent.is_some() && !listening {
            // Not while listening: math talk is not a command, and each classification is one more model call
            // carrying the learner's words. The backend still ends listening when they say they are done.
            let me = self.clone();
            let words = text.to_string();
            self.bg(async move { me.classify(&words, said_at).await });
        }
    }

    /// py: voice_agent.py:1971 `async def _classify(self, text: str, said_at: float) -> None:`
    pub(crate) async fn classify(self: &Arc<Self>, text: &str, said_at: f64) {
        let Some(intent) = &self.intent else { return };
        let label = intent(text.to_string()).await;
        if let Some(label) = label {
            if !self.ended.is_set() {
                self.apply_intent(&label, text, "classifier", said_at);
            }
        }
    }

    /// py: voice_agent.py:1977 `def _apply_intent(self, label: str, text: str, how: str, said_at: float) -> None:`
    pub(crate) fn apply_intent(self: &Arc<Self>, label: &str, text: &str, how: &str, said_at: f64) {
        if self.st.lock().think_aloud.is_some() {
            if !LISTENING_INTENTS.contains(&label) {
                // "Look at the left side" is about the problem, and "remember that" must never star a learner's
                // words into eve's permanent memory.
                return;
            }
            tracing::info!("voice: a turn means {label} ({how})"); // a learner's words are never logged
        } else {
            let head: String = text.chars().take(80).collect();
            tracing::info!("voice: {} means {label} ({how})", eve_intent::py_repr(&head));
        }
        let turn_started_at = self.st.lock().user_turn_started_at;
        if label == eve_intent::LEAVE {
            self.begin_farewell(said_at);
        } else if label == eve_intent::MUTE {
            self.set_sound(false, how);
        } else if label == eve_intent::UNMUTE {
            self.set_sound(true, how);
        } else if label == eve_intent::LOOK {
            let me = self.clone();
            let words = text.to_string();
            self.bg(async move { me.route_look(&words, turn_started_at).await });
        } else if label == eve_intent::LESSON {
            let me = self.clone();
            let words = text.to_string();
            self.bg(async move { me.route_lesson(&words, turn_started_at, false).await });
        } else if label == eve_intent::REMEMBER {
            let claim = self.last_substantive_turn(text);
            self.remember(&claim);
        }
    }

    /// py: voice_agent.py:1999 `def _last_substantive_turn(self, said: str) -> str:`
    ///
    /// What "remember that" points at.
    ///
    /// It is anaphoric: the claim is in the previous turn, not in the words
    /// "remember that". Only the owner's own last substantive turn counts — it
    /// is their claim about themselves. eve's own words are never starred
    /// (owner, 2026-09-23: the old fallback to eve's last line laundered its
    /// replies into the permanent layer); with no owner turn, nothing is.
    pub(crate) fn last_substantive_turn(self: &Arc<Self>, said: &str) -> String {
        let phrase = said.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ");
        let st = self.st.lock();
        for (turn_who, text) in st.turns.iter().rev() {
            let body = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if turn_who != "user" || body.is_empty() {
                continue;
            }
            if body.to_lowercase() == phrase || body.chars().count() < 8 {
                continue;
            }
            return body;
        }
        String::new()
    }

    /// py: voice_agent.py:2018 `def _remember(self, claim: str) -> None:`
    ///
    /// Star it through memory.star, and say so in one clause. Silence would look like it failed.
    pub(crate) fn remember(self: &Arc<Self>, claim: &str) {
        if self.memory.is_none() {
            return;
        }
        let me = self.clone();
        if claim.is_empty() {
            self.bg(async move {
                me.quiet(
                    "[memory] They asked you to remember something, but they said nothing before \
                     it that you could keep. Say so in one short line.",
                )
                .await
            });
            return;
        }
        let claim = claim.to_string();
        self.bg(async move { me.star(&claim).await });
    }

    /// py: voice_agent.py:2028 `async def _star(self, claim: str) -> None:`
    pub(crate) async fn star(self: &Arc<Self>, claim: &str) {
        let Some(memory) = self.memory.clone() else { return };
        let owned = claim.to_string();
        let kept = match tokio::task::spawn_blocking(move || memory.star(&owned)).await {
            Ok(kept) => kept,
            Err(e) => {
                tracing::warn!("voice: could not star it ({e})");
                None
            }
        };
        match kept.filter(|k| !k.is_empty()) {
            Some(kept) => {
                tracing::info!("voice: starred one line ({} chars)", kept.chars().count()); // never the words
                self.quiet(
                    "[memory] You have written that down for good. Tell them you will remember \
                     it, in three or four words. Do not repeat it back to them.",
                )
                .await;
            }
            None => self.quiet("[memory] You could not write it down. Say so in one short line, plainly.").await,
        }
    }

    /// py: voice_agent.py:2044 `async def _fast_head_move(self, text: str, turn_started_at: float) -> None:`
    ///
    /// Ask the typed model for a pose and, when it is sure, turn the head now. Any failure, an abstention or
    /// "none" leaves the turn to the ordinary path exactly as before.
    pub(crate) async fn fast_head_move(self: &Arc<Self>, text: &str, turn_started_at: f64) {
        let (Some(head_pose), Some(head)) = (&self.head_pose, &self.head) else { return };
        let t0 = (self.clock)();
        let pose = match AssertUnwindSafe(head_pose(text.to_string())).catch_unwind().await {
            Ok(Ok(pose)) => pose,
            // the fast path is an optimisation, never a way to fail a turn
            Ok(Err(e)) => {
                tracing::warn!("voice: fast head pose failed ({}: {e})", eve_intent::error_name(&e));
                return;
            }
            Err(_) => {
                tracing::warn!("voice: fast head pose failed (panic)");
                return;
            }
        };
        let Some(pose_move) = eve_head::pose_move(&pose) else { return };
        if self.ended.is_set() {
            return;
        }
        {
            let mut st = self.st.lock();
            if st.last_head_tool_at >= turn_started_at {
                return; // the backend got there first: never move twice
            }
            st.last_head_tool_at = (self.clock)(); // _route_look and the backend's move_head both read this
        }
        let result = head
            .move_to(
                pose_move.yaw.map(|y| y as f64),
                pose_move.pitch.map(|p| p as f64),
                pose_move.relative,
                eve_head::DEFAULT_HOLD_SECS,
            )
            .await;
        if truthy(result.get("ok")) {
            let mut done = obj_of(result);
            done.insert("note".into(), Value::String("already done: the head turned for these words".into()));
            self.st.lock().fast_head = Some((turn_started_at, done));
            tracing::info!("voice: head → {pose} in {:.2} s (fast path)", (self.clock)() - t0);
        }
    }

    /// py: voice_agent.py:2066 `async def _route_look(self, text: str, turn_started_at: float) -> None:`
    ///
    /// Hand a head request to the backend unless the voice already delegated this
    /// turn: a relative move ("a bit more left") must never run twice.
    pub(crate) async fn route_look(self: &Arc<Self>, text: &str, turn_started_at: f64) {
        sleep_secs(LOOK_ROUTE_DELAY_SECS).await;
        if self.ended.is_set() {
            return;
        }
        let (delegated_at, last_head_tool_at) = {
            let st = self.st.lock();
            (st.delegated_at, st.last_head_tool_at)
        };
        if delegated_at >= turn_started_at || last_head_tool_at >= turn_started_at {
            tracing::info!("voice: head request already delegated by the voice — not routing it again");
            return;
        }
        tracing::info!("voice: routing a head request the voice did not delegate");
        self.backend_note(&format!("[look request] {text}")).await;
        self.request_response().await;
    }

    /// py: voice_agent.py:2079 `async def _route_lesson(self, text: str, turn_started_at: float, setup: bool = False) -> None:`
    ///
    /// Hand a "teach me" (or, in lesson setup, any answer) to the backend unless the voice already
    /// delegated this turn.
    ///
    /// 11:44 on the bench: "teach me about Rust ownership" got "Sure. What have you done in Rust so
    /// far?" and a spoken explanation, and the lesson window never opened.
    pub(crate) async fn route_lesson(self: &Arc<Self>, text: &str, turn_started_at: f64, setup: bool) {
        sleep_secs(LESSON_ROUTE_DELAY_SECS).await;
        if self.ended.is_set() || self.st.lock().think_aloud.is_some() {
            return;
        }
        let what = if setup { "answer" } else { "request" };
        if self.st.lock().delegated_at >= turn_started_at {
            tracing::info!("voice: lesson {what} already delegated by the voice — not routing it again");
            return;
        }
        tracing::info!("voice: routing a lesson {what} the voice did not delegate");
        let note = if setup { format!("[lesson setup] {text}") } else { format!("[lesson request] {text}") };
        self.backend_note(&note).await;
        self.request_response().await;
    }

    /// py: voice_agent.py:2096 `def _begin_farewell(self, said_at: float) -> None:`
    ///
    /// Stamped with when the owner finished saying it, not when a classifier
    /// answered: eve's goodbye may already be on its way by then.
    pub(crate) fn begin_farewell(self: &Arc<Self>, said_at: f64) {
        {
            let mut st = self.st.lock();
            if st.farewell {
                return;
            }
            st.farewell = true;
            st.farewell_since = said_at;
        }
        if self.task_running() {
            tracing::info!("voice: the owner said goodbye — mic off; the task finishes and its result is said first");
        } else {
            tracing::info!("voice: the owner said goodbye — closing after eve's goodbye");
        }
        let me = self.clone();
        self.bg(async move {
            me.quiet("[leaving] The owner is done talking. Say a two- or three-word goodbye, nothing else.").await
        });
    }

    /// py: voice_agent.py:2109 `def _farewell_due(self, now: float) -> bool:`
    ///
    /// The goodbye has been said back (or never will be) and nothing is still being said.
    pub(crate) fn farewell_due(self: &Arc<Self>, now: f64) -> bool {
        if self.task_running() {
            return false;
        }
        let (farewell_since, task_done_at, last_reply_at, open_reply) = {
            let st = self.st.lock();
            if st.pending_answer.is_some() {
                return false;
            }
            let open_reply = st.turn_marks.speaker.as_deref() == Some("assistant") && !st.turn_marks.text.is_empty();
            (st.farewell_since, st.task_done_at, st.last_reply_at, open_reply)
        };
        let start = farewell_since.max(task_done_at);
        if now - start >= FAREWELL_MAX_SECS {
            return true;
        }
        let said_back = last_reply_at >= start; // the reply that closed the goodbye turn counts
        said_back && !open_reply && !self.speaker.busy() && now - last_reply_at >= FAREWELL_QUIET_SECS
    }

    /// py: voice_agent.py:2121 `def _set_sound(self, on: bool, how: str) -> None:`
    pub(crate) fn set_sound(self: &Arc<Self>, on: bool, how: &str) {
        let was_muted = (self.muted)();
        if let Some(on_sound) = &self.on_sound {
            if catch_unwind(AssertUnwindSafe(|| on_sound(on))).is_err() {
                tracing::error!("voice: on_sound failed");
            }
        }
        if !on {
            self.speaker.stop(); // audio mode: drop what is queued at once
        }
        if was_muted == !on {
            return; // no change: nothing new to tell the voice
        }
        tracing::info!("voice: sound {} ({how})", if on { "on" } else { "off" });
        if !self.ended.is_set() {
            let me = self.clone();
            self.bg(async move { me.quiet(if on { UNMUTED_NOTE } else { MUTED_NOTE }).await });
        }
    }

    /// py: voice_agent.py:2136 `def _bg(self, coro: Awaitable[None]) -> None:`
    pub(crate) fn bg(self: &Arc<Self>, coro: impl std::future::Future<Output = ()> + Send + 'static) {
        // The sends log their own failures ("background send failed" in Python's done callback); a panic is
        // the task's own and is logged here.
        crate::seams::spawn_ctx(async move {
            if AssertUnwindSafe(coro).catch_unwind().await.is_err() {
                tracing::warn!("voice: background send failed: panic");
            }
        });
    }

    /// py: voice_agent.py:2141 `async def _run_agent(self, goal: str) -> str:`
    pub(crate) async fn run_agent(self: &Arc<Self>, goal: &str) -> String {
        let Some(agent) = self.st.lock().agent.clone() else { return String::new() };
        let finished = match agent.run(goal, "").await {
            Ok(finished) => finished,
            Err(e) => {
                // Python's task ended with the exception; run() gathers it
                tracing::warn!("voice: the computer task failed: {e:#}");
                return String::new();
            }
        };
        let now = (self.clock)();
        {
            let mut st = self.st.lock();
            st.last_activity = now;
            st.task_done_at = now;
            // The task's sentence was written after it read screens and pages: it taints the conversation from here
            // on, and its tool line is kept marked (tool_gate.RESULT_LINES, outside), so a later prompt showing it
            // starts tainted too (v3 safety review: a task's result reached the voice clean).
            if st.taint.is_empty() {
                st.taint = eve_tool_gate::TASK_RESULT.to_string();
            }
        }
        let line: String = finished.chars().take(TOOL_LINE_CHARS).collect();
        self.capture("eve", "tool", &line, "start_task", true);
        if !self.ended.is_set() {
            // The conversation stays open after the result: the owner ends it with a
            // goodbye, or the idle timeout does (owner request 2026-09-10, reversing
            // the 2026-09-06 close-after-task).
            tracing::info!("voice: task result to the voice: {}", finished.chars().take(160).collect::<String>());
            self.speak(&format!(
                "The computer task you started has finished. Tell the owner this result in one \
                 short line. Do not say goodbye: keep listening until they end the conversation. \
                 The result: {finished}"
            ))
            .await;
        }
        finished
    }

    /// py: voice_agent.py:2160 `def _on_agent_event(self, ev: AgentEvent) -> None:`
    pub(crate) fn on_agent_event(self: &Arc<Self>, ev: &eve_agent_contract::AgentEvent) {
        let kind = ev.kind();
        if kind == "progress" {
            // Steps are never spoken: hearing each one read out is noise (owner report 2026-09-26).
            // The voice says "on it" once and the result once; a step shows only as a caption.
            // A helper sentence from the task ("opened Safari") as a caption page
            // while the robot is working — when nothing else is on screen and at
            // least PROGRESS_MIN_GAP_SECS after the previous one.
            if !self.captions {
                return;
            }
            let shown = {
                let mut st = self.st.lock();
                if !st.pager.busy() && ["working", "done", "idle", "listening"].contains(&st.state.as_str()) {
                    let now = (self.clock)();
                    if now - st.last_progress_at >= PROGRESS_MIN_GAP_SECS {
                        st.last_progress_at = now;
                        st.pager.begin_reply(now);
                        let step: String = ev.text().chars().take(120).collect();
                        st.pager.update(now, &step, true);
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            };
            if shown {
                self.flush_pager();
            }
            return;
        }
        let state = self.st.lock().state.clone();
        match kind {
            "started" | "exec" | "commentary" | "turn" => {
                if !["speaking", "listening", "thinking", "asking"].contains(&state.as_str()) {
                    self.set("working");
                }
            }
            "ask" => self.set("asking"),
            "final" => self.set("done"),
            "error" => self.set("error"),
            "cancelled" => self.set("listening"), // a stopped task is not a finished one: no done nod
            _ => {}
        }
    }

    /// py: voice_agent.py:2187 `async def _ask_user(self, question: str) -> str:`
    ///
    /// Rust: a send that fails ends the question as Python's raise did; here the answer is then a no. The
    /// computer task itself gets the failure ([`VoiceSession::ask_user_raising`]).
    pub(crate) async fn ask_user(self: &Arc<Self>, question: &str) -> String {
        match self.ask_user_raising(question).await {
            Ok(answer) => answer,
            Err(e) => {
                tracing::warn!("voice: the task's question was not asked: {e}");
                format!("no (the question could not be asked: {e})")
            }
        }
    }

    /// `_ask_user` as the computer task calls it: a failed send raises out of it.
    async fn ask_user_raising(self: &Arc<Self>, question: &str) -> anyhow::Result<String> {
        let pending = {
            let mut st = self.st.lock();
            if st.farewell {
                return Ok("no (the owner has ended the conversation and is not listening)".into());
            }
            let pending = Pending::new();
            st.pending_answer = Some(pending.clone());
            // the task's question is its words, written after reading screens and pages: the conversation is
            // tainted from here on (v3 safety review)
            if st.taint.is_empty() {
                st.taint = eve_tool_gate::TASK_QUESTION.to_string();
            }
            st.question_turn = st.owner_turns;
            st.asked_last = "task".into();
            pending
        };
        eve_tool_gate::MARKS.add([question]);
        // The slot is released on every exit, a failed send included: a question left pending
        // blocks the idle close and the goodbye for the rest of the session (Eve/Voice.lean, B).
        let asked = async {
            // The backend gets the question as context (no response): it is the half that
            // calls answer_question when the owner replies. The voice gets it to ask.
            self.send_backend_note(&format!("[task question] {question}")).await?;
            self.conn
                .commentary_append(
                    &format!(
                        "The computer task needs an answer from the owner. Ask exactly this, then wait for \
                         their answer: {question}"
                    ),
                    None,
                )
                .await?;
            // nothing cancels the question's future; were it cancelled, it would end as the minute's no
            Ok::<String, anyhow::Error>(match tokio::time::timeout(Duration::from_secs(60), pending.wait()).await {
                Ok(Ok(answer)) => answer,
                Ok(Err(_)) | Err(_) => "no (no answer within a minute)".to_string(),
            })
        };
        let out = asked.await;
        self.st.lock().pending_answer = None;
        out
    }

    /// py: voice_agent.py:2212 `async def _speak(self, content: str) -> None:`
    ///
    /// Hand the voice something to say (`session.commentary.append`).
    ///
    /// The SDK documents commentary as "speakable context … for a result the model
    /// should communicate". Routing a result through the backend instead
    /// (response.item.create + response.create) left the voice free to say anything:
    /// on 2026-09-10 a finished task, "Spotify is playing INTERGALACTIC.", reached the
    /// owner as "Okay, waiting."
    pub(crate) async fn speak(self: &Arc<Self>, content: &str) {
        if let Err(e) = self.conn.commentary_append(content, None).await {
            tracing::warn!("voice: background send failed: {e}");
        }
    }

    /// py: voice_agent.py:2223 `async def _quiet(self, content: str) -> None:`
    ///
    /// Silent context for the voice (`session.thinking.append`): it can shape later
    /// speech but asks for none.
    pub(crate) async fn quiet(self: &Arc<Self>, content: &str) {
        if let Err(e) = self.conn.thinking_append(content, None).await {
            tracing::warn!("voice: background send failed: {e}");
        }
    }

    /// `_backend_note`'s send, raising.
    async fn send_backend_note(self: &Arc<Self>, text: &str) -> anyhow::Result<()> {
        self.conn
            .response_item_create(json!({"type": "message", "role": "user",
                                         "content": [{"type": "input_text", "text": text}]}))
            .await
    }

    /// py: voice_agent.py:2228 `async def _backend_note(self, text: str) -> None:`
    ///
    /// Context for the backend's next delegated turn; asks for no response.
    pub(crate) async fn backend_note(self: &Arc<Self>, text: &str) {
        if let Err(e) = self.send_backend_note(text).await {
            tracing::warn!("voice: background send failed: {e}");
        }
    }

    /// py: voice_agent.py:2233 `async def _request_response(self) -> None:`
    ///
    /// response.create now, or as soon as the in-flight backend response completes.
    pub(crate) async fn request_response(self: &Arc<Self>) {
        {
            let mut st = self.st.lock();
            if st.response_active {
                st.response_wanted = true;
                return;
            }
        }
        if let Err(e) = self.conn.response_create().await {
            tracing::warn!("voice: response.create failed: {e}");
        }
    }
}

// ---- what the tests read and drive (Python's tests reach into the session's private attributes) ---------------

/// A snapshot of a session's private state, as test_voice_agent.py reads it (`s._farewell`, `s.tool_calls`, ...).
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct VoiceProbe {
    pub farewell: bool,
    pub ended: bool,
    pub end_reason: String,
    pub explore_requested: bool,
    pub lesson_setup: bool,
    pub tool_calls: Vec<(String, Obj)>,
    pub transcript: Vec<String>,
    pub turns: Vec<(String, String)>,
    pub state: String,
    pub has_agent: bool,
    pub slow_tasks: usize,
    pub idea_saves: usize,
    pub taint: String,
}

impl VoiceSession {
    /// The session's private state now.
    #[doc(hidden)]
    pub fn probe_state(self: &Arc<Self>) -> VoiceProbe {
        let st = self.st.lock();
        VoiceProbe {
            farewell: st.farewell,
            ended: self.ended.is_set(),
            end_reason: st.end_reason.clone(),
            explore_requested: st.explore_requested,
            lesson_setup: st.lesson_setup,
            tool_calls: st.tool_calls.clone(),
            transcript: st.transcript.clone(),
            turns: st.turns.clone(),
            state: st.state.clone(),
            has_agent: st.agent.is_some(),
            slow_tasks: st.slow_tasks.iter().filter(|j| !j.done()).count(),
            idea_saves: st.idea_saves.iter().filter(|j| !j.done()).count(),
            taint: st.taint.clone(),
        }
    }

    /// `await s._tool(name, call_id, arguments)`.
    #[doc(hidden)]
    pub async fn probe_tool(self: &Arc<Self>, name: &str, call_id: &str, arguments: &str) {
        self.tool(name, call_id, arguments).await
    }

    /// `s._transcript_delta(who, text)`.
    #[doc(hidden)]
    pub fn probe_transcript_delta(self: &Arc<Self>, who: &str, text: &str) {
        self.transcript_delta(who, text)
    }

    /// `s._close_turn(who, text)`.
    #[doc(hidden)]
    pub fn probe_close_turn(self: &Arc<Self>, who: &str, text: &str) {
        self.close_turn(who, text)
    }

    /// `s._on_agent_event(ev)`.
    #[doc(hidden)]
    pub fn probe_agent_event(self: &Arc<Self>, ev: &eve_agent_contract::AgentEvent) {
        self.on_agent_event(ev)
    }

    /// `await asyncio.gather(*s._slow_tasks)`.
    #[doc(hidden)]
    pub async fn probe_slow_tasks(self: &Arc<Self>) {
        let jobs: Vec<Job> = self.st.lock().slow_tasks.clone();
        for job in jobs {
            job.join().await;
        }
    }

    /// `await asyncio.gather(*s._idea_saves)`.
    #[doc(hidden)]
    pub async fn probe_idea_saves(self: &Arc<Self>) {
        let jobs: Vec<Job> = self.st.lock().idea_saves.clone();
        for job in jobs {
            job.join().await;
        }
    }

    /// `await asyncio.gather(s._agent_task, return_exceptions=True)`.
    #[doc(hidden)]
    pub async fn probe_agent_task(self: &Arc<Self>) {
        let job = self.st.lock().agent_task.clone();
        if let Some(job) = job {
            job.join().await;
        }
    }

    /// `s._started.set()`.
    #[doc(hidden)]
    pub fn probe_mark_started(self: &Arc<Self>) {
        self.started.set();
    }

    /// `s._last_activity = t`.
    #[doc(hidden)]
    pub fn probe_set_last_activity(self: &Arc<Self>, t: f64) {
        self.st.lock().last_activity = t;
    }
}
