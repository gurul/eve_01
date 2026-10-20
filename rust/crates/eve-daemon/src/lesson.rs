//! The robot's caption and phase while a lesson runs, lessons over IPC (`_handle_lesson`), and think out loud:
//! the listener the learning server calls, starting and stopping the listening conversation.

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use futures::future::BoxFuture;
use serde_json::{json, Map, Value};
use tracing::{info, warn};

use crate::daemon::Daemon;
use crate::seams::{Obj, VoiceSessionHandle};
use crate::{THINK_ALOUD_CALL_SECS, THINK_ALOUD_TASK};

/// [`eve_learning::Listener`] for the daemon: the learning server's thread asks the loop to start or stop think
/// out loud, waiting at most THINK_ALOUD_CALL_SECS (`_make_listener`).
///
/// start() and stop() are called on the server's worker threads (an HTTP request, or run_lesson in a blocking
/// task). They hand the change to the event loop, which owns every piece of think-aloud state, so the browser
/// toggle, `eve-bridge lesson` and the voice tool cannot race each other. status() only reads.
pub struct DaemonListener {
    pub daemon: std::sync::Weak<Daemon>,
    pub handle: tokio::runtime::Handle,
}

/// Whether this thread may block waiting on the loop: not one that is driving async code (a runtime worker, or a
/// `block_on`), which would wait on itself. A blocking task's thread and a plain thread may.
fn blocking_allowed() -> bool {
    match tokio::runtime::Handle::try_current() {
        Err(_) => true,
        // Handle::block_on refuses (panics) exactly when the thread is inside an asynchronous execution context.
        Ok(h) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h.block_on(async {}))).is_ok(),
    }
}

impl DaemonListener {
    fn call<F>(&self, make: F) -> anyhow::Result<Obj>
    where
        F: FnOnce(Arc<Daemon>) -> BoxFuture<'static, Obj> + Send + 'static,
    {
        if !blocking_allowed() {
            anyhow::bail!("the think-out-loud listener blocks; call it from a worker thread");
        }
        let Some(daemon) = self.daemon.upgrade() else {
            warn!("think aloud: request failed ({})", "ReferenceError");
            return Ok(reason_only("Something went wrong. Try again."));
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let task = self.handle.spawn(async move {
            let out = make(daemon).await;
            let _ = tx.send(out);
        });
        match rx.recv_timeout(Duration::from_secs_f64(THINK_ALOUD_CALL_SECS)) {
            Ok(out) => Ok(out),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                task.abort();
                Ok(reason_only("eve did not answer in time. Try again."))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                warn!("think aloud: request failed ({})", "CancelledError");
                Ok(reason_only("Something went wrong. Try again."))
            }
        }
    }
}

fn reason_only(reason: &str) -> Obj {
    obj(json!({"ok": false, "reason": reason}))
}

impl eve_learning::Listener for DaemonListener {
    fn start(&self, lesson: &Obj) -> anyhow::Result<Obj> {
        let lesson = lesson.clone();
        self.call(move |d| Box::pin(async move { d.think_aloud_start(lesson).await }))
    }
    fn stop(&self) -> anyhow::Result<Obj> {
        self.call(|d| Box::pin(async move { d.think_aloud_stop().await }))
    }
    fn status(&self) -> anyhow::Result<Obj> {
        let Some(d) = self.daemon.upgrade() else { anyhow::bail!("the daemon is gone") };
        let st = d.st.lock();
        let lesson_id = st.think_aloud_lesson_id.clone().map(Value::String).unwrap_or(Value::Null);
        Ok(obj(json!({"state": st.think_aloud_state, "lesson_id": lesson_id})))
    }
}

impl Daemon {
    /// `_cancel_active_task(reason)`: cancel the desktop task's agent when one is running.
    ///
    /// Stop a running desktop task with a reason the owner can read (log + caption), instead of letting a restart
    /// or a touch kill it silently (bench 2026-09-06 09:45).
    pub async fn cancel_active_task(&self, reason: &str) {
        let agent = self.st.lock().active_agent.clone();
        let Some(agent) = agent.filter(|a| a.running()) else { return };
        warn!("agent: cancelling the running task — {}", reason);
        agent.cancel(reason);
        if self.ble.connected() {
            let line: String = format!("stopped: {reason}").chars().take(17).collect();
            self.ble
                .send(
                    &json!({"cmd": "caption", "lines": [line], "page": 0, "of": 1, "hold_ms": 4000, "final": true,
                            "chirp": false}),
                    None,
                )
                .await;
        }
    }

    /// `_learning_notice(text, stage)`: a lesson caption on the robot, a bus event, and "back to idle" later.
    ///
    /// A lesson moved on (from the web page, `eve-bridge lesson`, or an error): the robot shows it. The existing
    /// voice session speaks tool results, so nothing is drawn while it is open.
    pub fn learning_notice(self: &Arc<Self>, text: &str, stage: &str) {
        self.note_activity();
        let action = {
            let mut st = self.st.lock();
            st.learning_notices += 1;
            st.lesson_action.clone()
        };
        self.publish_lesson(if action.is_empty() { "notice" } else { &action }, stage, text);
        if self.conversation_open() {
            return; // Live owns captions while the owner is talking.
        }
        let state = match stage {
            "error" => "error",
            "complete" => "done",
            _ => "speaking",
        };
        self.on_agent_state(state);
        // The robot screen holds 4 lines of 17 columns (caption_pager.PagerConfig).
        let mut lines: Vec<String> = wrap(&py_split(text).join(" "), 17).into_iter().take(4).collect();
        if lines.is_empty() {
            lines.push("...".to_string());
        }
        self.on_caption(obj(json!({"cmd": "caption", "page": 0, "of": 1, "lines": lines,
                                   "hold_ms": 6000, "final": true, "chirp": state != "error"})));
        // Only the latest notice may send the robot back to idle.
        if let Some(handle) = self.st.lock().learning_idle.take() {
            handle.abort();
        }
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                let weak = Arc::downgrade(self);
                let timer = rt.spawn(async move {
                    tokio::time::sleep(Duration::from_secs(6)).await;
                    if let Some(d) = weak.upgrade() {
                        d.learning_idle();
                    }
                });
                self.st.lock().learning_idle = Some(timer.abort_handle());
            }
            Err(_) => warn!("lesson: no event loop for the back-to-idle timer"),
        }
    }

    /// `_learning_idle()`: the robot back to idle after a lesson caption.
    pub fn learning_idle(self: &Arc<Self>) {
        if !self.conversation_open() {
            self.on_agent_state("idle");
        }
    }

    /// `_make_listener(loop)`.
    pub fn make_listener(self: &Arc<Self>, handle: &tokio::runtime::Handle) -> Arc<DaemonListener> {
        Arc::new(DaemonListener { daemon: Arc::downgrade(self), handle: handle.clone() })
    }

    /// `_think_aloud_start(lesson)`: listen to the owner think out loud for this lesson. Idempotent; never opens a
    /// second conversation.
    pub async fn think_aloud_start(self: &Arc<Self>, lesson: Obj) -> Obj {
        let ears = self.st.lock().ears.clone();
        let Some(ears) = ears else {
            return reason_only(
                "eve's microphone is off (EVE_VOICE=0, or no microphone was found), so eve cannot listen.",
            );
        };
        if !ears.listening() {
            return reason_only(
                "eve's microphone is closed because the robot is not connected. Plug eve in, or set EVE_MIC_ALWAYS=1.",
            );
        }
        if !eve_brain::voice_agent::live_available(self.environ()) {
            return reason_only(
                "eve cannot listen yet: its desk voice still needs OPENAI_API_KEY in ~/.config/eve-bridge/env (until it moves to OpenRouter).",
            );
        }
        self.note_activity();
        let session = self.st.lock().voice_session.clone();
        let talking = self.conversation_open();
        let mut entered = false;
        if let Some(session) = session.as_ref().filter(|s| talking && !s.ended()) {
            // A conversation is open (the wake word, or listening already): it switches, no second session.
            let result = session.enter_think_aloud(&lesson).await;
            entered = truthy(result.get("ok"));
            if !entered && !session.ended() {
                info!("think aloud: start -> ok=False");
                let reason = str_or(result.get("reason"), "eve cannot listen right now.");
                return reason_only(&reason);
            }
        }
        if !entered && talking {
            // Still connecting (on_open applies it) or closing (_converse opens a new one after it).
            let mut st = self.st.lock();
            if st.think_aloud_state == "off" {
                st.think_aloud_state = "starting".into();
                st.think_aloud_lesson_id = Some(py_str(lesson.get("id")));
            }
            st.think_aloud_wish = Some(lesson);
        } else if !entered {
            self.start_think_aloud_conversation(lesson);
        }
        let state = self.st.lock().think_aloud_state.clone();
        info!("think aloud: start -> ok=True ({})", state);
        obj(json!({"ok": true, "answer": "I'm listening. Think out loud whenever you're ready."}))
    }

    /// `_think_aloud_stop()`: stop listening now: the toggle, the command, or the voice tool outside a listening
    /// session.
    pub async fn think_aloud_stop(self: &Arc<Self>) -> Obj {
        let (was, session, conversation) = {
            let mut st = self.st.lock();
            st.think_aloud_wish = None;
            (st.think_aloud_state.clone(), st.voice_session.clone(), st.conversation.clone())
        };
        if let Some(session) = session.as_ref().filter(|s| s.think_aloud_lesson_id().is_some()) {
            session.stop_think_aloud("stopped");
        } else if let Some(conversation) = conversation.as_ref() {
            if !conversation.done() && session.is_none() && conversation.name() == THINK_ALOUD_TASK {
                conversation.cancel(); // still connecting for listening: nothing to say goodbye to
            }
        }
        if was != "off" {
            self.on_think_aloud(false, None);
        }
        info!("think aloud: stop -> ok=True (was {})", was);
        let answer = if was != "off" { "Okay, I stopped listening." } else { "eve was not listening." };
        obj(json!({"ok": true, "answer": answer}))
    }

    /// `_start_think_aloud_conversation(lesson)`: a conversation opened for listening (THINK_ALOUD_TASK), without
    /// the wake word.
    pub fn start_think_aloud_conversation(self: &Arc<Self>, lesson: Obj) {
        {
            let mut st = self.st.lock();
            st.think_aloud_state = "starting".into();
            st.think_aloud_lesson_id = Some(py_str(lesson.get("id")));
        }
        let me = self.clone();
        self.spawn("dismiss-explore", async move { me.dismiss_explore("think out loud").await });
        let me = self.clone();
        let job =
            crate::seams::Job::spawn(THINK_ALOUD_TASK, async move { me.converse(Some(lesson), false).await });
        self.st.lock().conversation = Some(job);
    }

    /// `_on_voice_session(session)`: open_session connected: remember the session, and hand it a listen asked for
    /// while connecting.
    pub fn on_voice_session(self: &Arc<Self>, session: Arc<dyn VoiceSessionHandle>) {
        let wish = {
            let mut st = self.st.lock();
            st.voice_session = Some(session.clone());
            st.think_aloud_wish.take()
        };
        if let Some(wish) = wish {
            session.prepare_think_aloud(&wish);
        }
    }

    /// `_on_think_aloud(on, lesson_id)`: the session started or stopped listening. The web page polls this state.
    pub fn on_think_aloud(self: &Arc<Self>, on: bool, lesson_id: Option<&str>) {
        {
            let mut st = self.st.lock();
            st.think_aloud_state = if on { "on" } else { "off" }.into();
            st.think_aloud_lesson_id = if on { lesson_id.map(str::to_string) } else { None };
        }
        info!("think aloud: {}", if on { "listening" } else { "off" });
        self.sync_listen_pose();
    }

    /// `_on_expression_text(who, text)`: a line the live expressions read.
    pub fn on_expression_text(&self, who: &str, text: &str) {
        self.expressions.offer(who, text);
    }

    /// `_on_caption(msg)`: one page of eve's reply (or a clear) onto the robot's screen; the pager owns the
    /// timing. Muted, the page goes without its talk chirp (older firmware has no sound command).
    pub fn on_caption(self: &Arc<Self>, msg: Obj) {
        if self.ble.connected() {
            let muted = self.sound.lock().muted();
            let wire = Value::Object(eve_sound::quiet_caption(&msg, muted).into_owned());
            let link = self.ble.clone();
            self.spawn("caption", async move {
                link.send(&wire, None).await;
            });
        }
    }

    /// `_on_agent_state(state)`: mirror the conversation/task phase on the board: the board's, the follower's and
    /// the listen pose.
    pub fn on_agent_state(self: &Arc<Self>, state: &str) {
        {
            let mut st = self.st.lock();
            if st.agent_state == state {
                return;
            }
            st.agent_state = state.to_string();
        }
        self.note_activity();
        info!("agent: {}", state);
        self.bus.publish("/eve/state", obj(json!({"state": state, "time": eve_brain::prelude::wall()})));
        if self.ble.connected() {
            let link = self.ble.clone();
            let msg = json!({"cmd": "agent", "state": state});
            self.spawn("agent-state", async move {
                link.send(&msg, None).await;
            });
        }
        let follower = self.follower.clone();
        let phase = state.to_string();
        self.spawn("follow-phase", async move { follower.on_phase(&phase).await });
        self.sync_listen_pose();
    }

    /// `_sync_listen_pose()`: the board's listening pose (face the learner, solid blue "mic is live" LEDs).
    ///
    /// Raised while the dictation key is held, or while eve listens to a learner thinking out loud. The pose pins
    /// the head and overrides the phase LEDs, so it is lowered while eve thinks or speaks: the robot acts those
    /// out like any conversation, then shows listening again.
    pub fn sync_listen_pose(self: &Arc<Self>) {
        let want = {
            let mut st = self.st.lock();
            let listening = st.think_aloud_state == "on"
                && !["thinking", "speaking", "working", "error", "done"].contains(&st.agent_state.as_str());
            let want = st.listen_down || listening;
            if !self.ble.connected() || st.listen_sent == Some(want) {
                return;
            }
            st.listen_sent = Some(want);
            want
        };
        let link = self.ble.clone();
        self.spawn("listen-pose", async move {
            link.send(&json!({"cmd": "listen", "on": want}), None).await;
        });
    }

    /// `_handle_lesson(req)`: one lesson action over IPC, through the same run_lesson helper the voice tool uses.
    ///
    /// The robot acts it out: "thinking" while the tutor works, then one caption with the answer. While a
    /// conversation is open, Live owns the screen and nothing is drawn.
    pub async fn handle_lesson(self: &Arc<Self>, req: Obj) -> Obj {
        let request = match eve_learning::lesson_request(&Value::Object(req)) {
            Ok(r) => r,
            Err(e) => return obj(json!({"ok": false, "error": e})),
        };
        let action = request.action.clone();
        let server = self.st.lock().learning_server.clone();
        let talking = self.conversation_open();
        if action != "status" {
            // The board cannot pan the room and teach at once (the wake word does the same).
            self.dismiss_explore("lesson").await;
            if server.is_some() && !talking && eve_learning::TUTOR_ACTIONS.contains(&action.as_str()) {
                self.on_agent_state("thinking");
            }
        }
        let before = {
            let mut st = self.st.lock();
            st.lesson_action = action.clone(); // names the action in the lesson event the notice publishes
            st.learning_notices
        };
        let args = Value::Object(request.to_map());
        let ran = tokio::task::spawn_blocking(move || {
            let voice = server.map(|s| move |r: &eve_learning::LessonRequest| s.voice(r));
            eve_learning::run_lesson(voice.as_ref().map(|f| f as eve_learning::VoiceFn<'_>), &args)
        })
        .await;
        let result = match ran {
            Ok(Ok(result)) => result,
            // run_lesson lets unexpected errors (a disk error, a tutor bug) through. Treat one as a refusal, so
            // the robot never stays on "thinking". Log only the type: the message may carry the learner's text.
            Ok(Err(e)) => {
                warn!("lesson: {} failed ({})", action, learning_error_name(&e));
                reason_only("Something went wrong with the lesson. Try again.")
            }
            Err(e) => {
                if e.is_cancelled() {
                    warn!("lesson: {} failed ({})", action, "CancelledError");
                } else {
                    warn!("lesson: {} failed ({})", action, "PanicException");
                }
                reason_only("Something went wrong with the lesson. Try again.")
            }
        };
        // A notice the worker handed to the loop is not certain to have run when the blocking call returns
        // (3 in 40 under load, 2026-10-09: the answer's caption then showed twice). One pass of the loop runs it.
        tokio::task::yield_now().await;
        let has_voice = self.st.lock().learning_server.is_some();
        // Only the action and the outcome are logged, never the learner's text, topic or answer.
        let ok = py_repr(result.get("ok").unwrap_or(&Value::Null));
        info!("lesson: {} -> ok={}", action, ok);
        if !truthy(result.get("ok")) {
            let reason = str_or(result.get("reason"), "That did not work.");
            if has_voice {
                self.learning_notice(&reason, "error");
            }
            self.st.lock().lesson_action = String::new();
            let mut out = result;
            out.insert("ok".into(), Value::Bool(false));
            out.insert("action".into(), Value::String(action));
            out.insert("reason".into(), Value::String(reason.clone()));
            out.insert("error".into(), Value::String(reason));
            return out;
        }
        // The notify callback is handed to the loop from the worker thread before the blocking call returns, so
        // it has run by now. open, ideas and help-mode start never notify: show those here. listen and
        // stop-listening show the listening pose instead of a caption over the new session.
        let notices = self.st.lock().learning_notices;
        if notices == before && action != "status" && !eve_learning::LISTEN_ACTIONS.contains(&action.as_str()) {
            let lesson = result.get("lesson").filter(|v| truthy(Some(v)));
            let stage = str_or(lesson.and_then(|l| l.get("stage")), "");
            let answer = str_or(result.get("answer"), "");
            self.learning_notice(&answer, &stage);
        }
        self.st.lock().lesson_action = String::new();
        let mut out = result;
        out.insert("ok".into(), Value::Bool(true));
        out.insert("action".into(), Value::String(action));
        out
    }
}

// ---- small Python semantics the daemon's files share ------------------------------------------------------------

/// A JSON object from a `json!({...})` literal.
pub fn obj(v: Value) -> Obj {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// Python truthiness of a JSON value (`None` for a missing key).
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Python `str(v)`: a string as it is, None as "None", anything else as its repr.
pub fn py_str(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        None => "None".into(),
        Some(other) => py_repr(other),
    }
}

/// Python `str(v or default)`.
pub fn str_or(v: Option<&Value>, default: &str) -> String {
    if truthy(v) {
        py_str(v)
    } else {
        default.to_string()
    }
}

/// Python `repr()` of a JSON value, for log lines.
pub fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => eve_focus_terminal::py_repr(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!("{}: {}", eve_focus_terminal::py_repr(k), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// The Python exception a [`eve_learning::LearningError`] stands for, for a log line that names only the type.
pub fn learning_error_name(e: &eve_learning::LearningError) -> &'static str {
    match e {
        eve_learning::LearningError::Value(_) => "ValueError",
        eve_learning::LearningError::NotFound(_) => "KeyError",
        eve_learning::LearningError::Type(_) => "TypeError",
        eve_learning::LearningError::Other(_) => "Exception",
    }
}

/// Python's `str.isspace()` for one character.
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python `str.split()`: runs of whitespace separate, none at the ends.
pub fn py_split(s: &str) -> Vec<&str> {
    s.split(py_space).filter(|w| !w.is_empty()).collect()
}

/// textwrap's `wordsep_re` (break_on_hyphens): whitespace runs, em-dashes between words, and words split after
/// their hyphens. `_whitespace` is '\t\n\x0b\x0c\r '.
static WORDSEP: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    let ws = r"[\t\n\x0b\x0c\r ]";
    let nws = r"[^\t\n\x0b\x0c\r ]";
    let wp = r#"[\w!"'&.,?]"#;
    let lt = r"[^\d\W]";
    let pattern = format!(
        "({ws}+|(?<={wp})-{{2,}}(?=\\w)|{nws}+?(?:-(?:(?<={lt}{{2}}-)|(?<={lt}-{lt}-))(?={lt}-?{lt})|(?={ws}|\\z)|(?<={wp})(?=-{{2,}}\\w)))"
    );
    fancy_regex::Regex::new(&pattern).expect("textwrap's wordsep pattern")
});

/// textwrap's chunks: `wordsep_re.split(text)` without the empty strings.
fn wrap_chunks(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut at = 0;
    let mut start = 0;
    while start <= text.len() {
        let Ok(Some(m)) = WORDSEP.find_from_pos(text, start) else { break };
        if m.start() > at {
            chunks.push(text[at..m.start()].to_string());
        }
        if !m.as_str().is_empty() {
            chunks.push(m.as_str().to_string());
        }
        at = m.end();
        start = if m.end() > m.start() {
            m.end()
        } else {
            match text[m.end()..].chars().next() {
                Some(c) => m.end() + c.len_utf8(),
                None => break,
            }
        };
    }
    if at < text.len() {
        chunks.push(text[at..].to_string());
    }
    chunks
}

/// Python `textwrap.wrap(text, width)` with its defaults (break_long_words, break_on_hyphens, drop_whitespace),
/// for text already free of tabs and newlines.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let len = |s: &str| s.chars().count();
    let is_blank = |s: &str| s.trim_matches(py_space).is_empty();
    let mut chunks = wrap_chunks(text);
    chunks.reverse();
    let mut lines: Vec<String> = Vec::new();
    while !chunks.is_empty() {
        let mut cur_line: Vec<String> = Vec::new();
        let mut cur_len = 0;
        // First chunk on line is whitespace -- drop it, unless this is the very beginning of the text.
        if chunks.last().is_some_and(|c| is_blank(c)) && !lines.is_empty() {
            chunks.pop();
        }
        while let Some(next) = chunks.last() {
            let l = len(next);
            if cur_len + l <= width {
                cur_len += l;
                cur_line.push(chunks.pop().unwrap_or_default());
            } else {
                break;
            }
        }
        // The current line is full, and the next chunk is too big to fit on any line.
        if chunks.last().is_some_and(|c| len(c) > width) {
            let space_left = if width < 1 { 1 } else { width - cur_len };
            if let Some(last) = chunks.last_mut() {
                let head: String = last.chars().take(space_left).collect();
                let rest: String = last.chars().skip(space_left).collect();
                cur_line.push(head);
                *last = rest;
            }
            cur_len = cur_line.iter().map(|c| len(c)).sum();
        }
        // If the last chunk on this line is all whitespace, drop it.
        if cur_line.last().is_some_and(|c| is_blank(c)) {
            cur_len -= cur_line.last().map(|c| len(c)).unwrap_or(0);
            cur_line.pop();
        }
        let _ = cur_len;
        if !cur_line.is_empty() {
            lines.push(cur_line.concat());
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str) -> Vec<String> {
        wrap(&py_split(text).join(" "), 17)
    }

    #[test]
    fn wrap_matches_python_textwrap() {
        // Expected values from CPython's textwrap.wrap(" ".join(text.split()), width=17).
        assert_eq!(
            w("Subtract three from both sides to get the x part alone."),
            ["Subtract three", "from both sides", "to get the x part", "alone."]
        );
        assert_eq!(
            w(&"  supercalifragilisticexpialidocious\n and   more words here ".repeat(3)),
            [
                "supercalifragilis",
                "ticexpialidocious",
                "and more words",
                "here supercalifra",
                "gilisticexpialido",
                "cious and more",
                "words here superc",
                "alifragilisticexp",
                "ialidocious and",
                "more words here"
            ]
        );
        assert_eq!(w("a well-known goof-ball -- use it"), ["a well-known", "goof-ball -- use", "it"]);
        assert_eq!(w("Look, goof-ball -- use the -b option!"), ["Look, goof-ball", "-- use the -b", "option!"]);
        assert_eq!(w(&"x".repeat(40)), ["xxxxxxxxxxxxxxxxx", "xxxxxxxxxxxxxxxxx", "xxxxxx"]);
        assert_eq!(
            w("one two three-four-five-six-seven-eight"),
            ["one two three-", "four-five-six-", "seven-eight"]
        );
        assert_eq!(w("éèêëēėę-ąįų many words here ok"), ["éèêëēėę-ąįų many", "words here ok"]);
        assert!(w("").is_empty());
    }
}
