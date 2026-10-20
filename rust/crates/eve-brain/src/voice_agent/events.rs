//! A port of `voice_agent.py`, lines 1288-1582: VoiceSession: speaker drain, captions, mic pump, watchdog, events, turns.
//!
//! Rust: `st` (the session's mutable state) is never held across an await, nor while a callback, `_set`, or another
//! method that takes it runs. A Live send that fails, or output audio that does not decode, raises out of
//! `handle_event` as Python's did: it ends the conversation's event loop (`_events`), and the session closes
//! (Python's `_until_ended` re-raised it out of `run`'s `finally`; here it is logged there). The mic pump ends the
//! same way on a failed send.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::voice_agent::*;

use base64::Engine as _;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// `re.search(r"[.!?](?:\s|$)", fresh)`: a finished clause in a streamed reply.
static CLAUSE_END: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[.!?](?:\s|$)").expect("CLAUSE_END"));

/// `asyncio.sleep(secs)`.
async fn sleep_secs(secs: f64) {
    if secs.is_finite() && secs > 0.0 {
        tokio::time::sleep(std::time::Duration::from_secs_f64(secs)).await;
    } else {
        tokio::task::yield_now().await;
    }
}

/// `TRANSCRIPT_WHO[who]`: the transcript's speaker for a turn's ("user" → "owner", "assistant" → "eve").
fn transcript_who(who: &str) -> Option<&'static str> {
    TRANSCRIPT_WHO.iter().find(|(k, _)| *k == who).map(|(_, v)| *v)
}

/// Python's `str(v)` of a JSON value (its `%s` in a log line): a dict or list as Python shows it.
fn py_shown(v: &Value) -> String {
    eve_canvas::py::py_str(v)
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

impl VoiceSession {
    /// A slow tool's or a spoken idea's job is in flight (`bool(self._slow_tasks)` / `bool(self._idea_saves)`:
    /// a job leaves the set when it is done).
    pub(crate) fn slow_tasks_pending(&self) -> bool {
        self.st.lock().slow_tasks.iter().any(|j| !j.done())
    }

    pub(crate) fn idea_saves_pending(&self) -> bool {
        self.st.lock().idea_saves.iter().any(|j| !j.done())
    }

    /// py: voice_agent.py:1288 `async def _drain_speaker(self, timeout: float = 15.0) -> None:`
    ///
    /// Let a queued reply finish playing (barge-in and errors skip this).
    pub(crate) async fn drain_speaker(self: &Arc<Self>, timeout: f64) {
        let deadline = (self.clock)() + timeout;
        while self.speaker.busy() && (self.clock)() < deadline {
            sleep_secs(0.05).await;
        }
    }

    /// py: voice_agent.py:1295 `def _set_after_captions(self, state: str) -> None:`
    ///
    /// The phase flips when the last caption page has been read, not when the model stops.
    pub(crate) fn set_after_captions(self: &Arc<Self>, state: &str) {
        {
            let mut st = self.st.lock();
            if st.pager.busy() {
                st.state_after_captions = Some(state.to_string());
                return;
            }
        }
        self.set(state);
    }

    /// py: voice_agent.py:1302 `def _emit_events(self, events: list[Event]) -> None:`
    pub(crate) fn emit_events(self: &Arc<Self>, events: &[eve_caption_pager::Event]) {
        let Some(on_caption) = &self.on_caption else { return };
        for e in events {
            let wire = e.to_wire();
            if catch_unwind(AssertUnwindSafe(|| on_caption(wire))).is_err() {
                tracing::error!("voice: on_caption failed");
            }
        }
    }

    /// py: voice_agent.py:1311 `def _flush_pager(self) -> None:`
    pub(crate) fn flush_pager(self: &Arc<Self>) {
        if !self.captions {
            return;
        }
        let now = (self.clock)();
        let events = self.st.lock().pager.poll(now);
        self.emit_events(&events);
        let after = {
            let mut st = self.st.lock();
            if !st.pager.busy() && st.state_after_captions.is_some() {
                let state = st.state_after_captions.take();
                if st.response_active {
                    None
                } else {
                    state
                }
            } else {
                None
            }
        };
        if let Some(state) = after {
            self.set(&state);
        }
    }

    /// py: voice_agent.py:1320 `async def _tick_loop(self) -> None:`
    ///
    /// Closes a turn that has gone quiet, and pages captions when they are on.
    ///
    /// Runs in both output modes: the turn boundary is what ends a conversation
    /// after a task result, and audio mode needs it too.
    pub(crate) async fn tick_loop(self: &Arc<Self>) {
        while !self.ended.is_set() {
            sleep_secs(self.caption_tick).await;
            self.close_due_turn();
            if self.captions {
                self.flush_pager();
            }
        }
    }

    /// py: voice_agent.py:1332 `async def _drain_captions(self, timeout: float = 15.0) -> None:`
    ///
    /// Let the page on screen finish its hold before the session goes idle.
    pub(crate) async fn drain_captions(self: &Arc<Self>, timeout: f64) {
        let deadline = (self.clock)() + timeout;
        while self.st.lock().pager.busy() && (self.clock)() < deadline {
            sleep_secs(self.caption_tick).await;
            self.flush_pager();
        }
        let now = (self.clock)();
        let events = {
            let mut st = self.st.lock();
            if st.pager.busy() {
                st.pager.reset(now)
            } else {
                Vec::new()
            }
        };
        if !events.is_empty() {
            self.emit_events(&events);
        }
    }

    /// py: voice_agent.py:1341 `async def _pump_mic(self) -> None:`
    pub(crate) async fn pump_mic(self: &Arc<Self>) {
        // Captions mode plays nothing on the Mac, so there is no echo to dodge and
        // the mic runs full duplex — gpt-live-1 listens while it speaks and takes
        // the interruption itself.
        //
        // Audio mode still has no echo cancellation: the Mac mic hears the Mac
        // speaker, and eve would answer its own greeting in a loop (bench
        // 2026-09-06 09:01). While a reply is playing, plus a short tail, the mic
        // is not forwarded. Cost: no barge-in mid-sentence; say it after.
        let gate = self.config.output != "captions";
        let b64 = base64::engine::general_purpose::STANDARD;
        while !self.ended.is_set() {
            let raw = self.mic.get().await;
            if self.st.lock().farewell {
                continue; // "go away": eve has stopped listening
            }
            if gate {
                if self.speaker.busy() {
                    let until = (self.clock)() + SPEAKER_TAIL_SECS;
                    self.st.lock().speaking_until = until;
                    continue;
                }
                if (self.clock)() < self.st.lock().speaking_until {
                    continue;
                }
            }
            let Some(voice_gate) = &self.gate else {
                if let Err(e) = self.conn.input_audio_append(&b64.encode(&raw)).await {
                    tracing::warn!("voice: mic audio not sent: {e}");
                    return;
                }
                continue;
            };
            // The voice gate (voice_gate.py): only the person who said the wake word reaches the model. It is
            // for QUERIES: in a think-aloud lesson eve takes notes on whoever is speaking, so nothing is
            // judged there; and while a yes/no is awaited or a task runs, a short "stop" is never held back.
            let (listening, pending) = {
                let st = self.st.lock();
                (st.think_aloud.is_some(), st.pending_answer.is_some())
            };
            voice_gate.set_bypass(listening);
            voice_gate.set_lenient(self.task_running() || pending);
            for piece in voice_gate.process(&raw) {
                if let Err(e) = self.conn.input_audio_append(&b64.encode(&piece)).await {
                    tracing::warn!("voice: mic audio not sent: {e}");
                    return;
                }
            }
        }
    }

    /// py: voice_agent.py:1372 `async def _watchdog(self) -> None:`
    pub(crate) async fn watchdog(self: &Arc<Self>) {
        while !self.ended.is_set() {
            sleep_secs(self.watchdog_secs).await;
            let now = (self.clock)();
            let (listening, started_at, farewell, pending, pager_busy, last_activity) = {
                let st = self.st.lock();
                (
                    st.think_aloud.is_some(),
                    st.started_at,
                    st.farewell,
                    st.pending_answer.is_some(),
                    st.pager.busy(),
                    st.last_activity,
                )
            };
            // Think out loud waits through thinking pauses: a minute of quiet, not the idle timeout.
            let quiet_secs =
                if listening { eve_learning::think_aloud::SILENCE_SECS } else { self.config.idle_timeout_secs };
            if now - started_at > self.config.max_session_secs {
                tracing::info!("voice: session hit its {:.0} s cap", self.config.max_session_secs);
                self.finish("cap");
            } else if farewell && self.farewell_due(now) {
                tracing::info!("voice: goodbye said — closing");
                self.finish("goodbye");
            } else if !self.task_running()
                && !pending
                && !self.speaker.busy()
                && !pager_busy
                && !self.slow_tasks_pending()
                && !self.idea_saves_pending()
                && now - last_activity > quiet_secs
            {
                tracing::info!("voice: nothing said for {quiet_secs:.0} s — closing");
                self.finish("silence");
            }
        }
    }

    /// py: voice_agent.py:1391 `async def _until_ended(self) -> None:`
    ///
    /// Serve Live events until the session ends — by an event, or by the watchdog.
    ///
    /// The watchdog's idle timeout and session cap only set `_ended`, and `_events`
    /// checks it only when the next Live event arrives. When the server went quiet
    /// the session never closed: all three idle closes in the log hung (+91 s,
    /// +129 s, +93 min), while the keepalive kept the frozen phase alive on the
    /// board and the wake word stayed blocked (timer review, 2026-09-10).
    pub(crate) async fn until_ended(self: &Arc<Self>) {
        // The first to finish wins; the other is dropped (cancelled) with it.
        tokio::select! {
            _ = self.events() => {}
            _ = self.ended.wait() => {}
        }
    }

    /// py: voice_agent.py:1411 `async def _events(self) -> None:`
    pub(crate) async fn events(self: &Arc<Self>) {
        loop {
            let event = match self.conn.next_event().await {
                None => return,
                Some(Err(e)) => {
                    tracing::warn!("voice: live connection failed: {e}");
                    return;
                }
                Some(Ok(event)) => event,
            };
            if self.ended.is_set() {
                return;
            }
            if let Err(e) = self.handle_event_raising(&event).await {
                // Python's exception ended the events task, and `_until_ended` re-raised it
                tracing::warn!("voice: the conversation's events stopped: {e:#}");
                return;
            }
            if self.ended.is_set() {
                return;
            }
        }
    }

    /// py: voice_agent.py:1419 `async def handle_event(self, event: Any) -> None:`
    ///
    /// Rust: what Python let raise out of it is logged here; the event loop takes it from
    /// [`VoiceSession::handle_event_raising`] and stops on it.
    pub async fn handle_event(self: &Arc<Self>, event: &Value) {
        if let Err(e) = self.handle_event_raising(event).await {
            tracing::warn!("voice: a live event failed: {e:#}");
        }
    }

    /// `handle_event` as Python ran it: a failed send or undecodable audio raises (an `Err`).
    pub(crate) async fn handle_event_raising(self: &Arc<Self>, event: &Value) -> anyhow::Result<()> {
        let t = attr(event, "type");
        let t = t.as_str().unwrap_or("");
        let delta_text = || attr(event, "delta").as_str().map(str::to_string).unwrap_or_default();
        match t {
            "session.input_transcript.delta" => self.transcript_delta("user", &delta_text()),
            "session.output_transcript.delta" => self.transcript_delta("assistant", &delta_text()),
            "session.output_audio.delta" => {
                let delta = attr(event, "delta");
                let delta = delta.as_str().unwrap_or("");
                if !delta.is_empty() && !self.captions && !(self.muted)() {
                    // base64.b64decode (validate=False): characters outside the alphabet are discarded first
                    let kept: String =
                        delta.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=')).collect();
                    // an undecodable chunk raises (binascii.Error), as Python's did
                    let pcm = base64::engine::general_purpose::STANDARD
                        .decode(kept)
                        .map_err(|e| anyhow::anyhow!("output audio not decoded: {e}"))?;
                    self.speaker.play(&pcm);
                    self.set("speaking");
                }
            }
            "session.delegation.created" => {
                // The Live model handed the turn to the backend: eve is thinking.
                let now = (self.clock)();
                let thinking = {
                    let mut st = self.st.lock();
                    st.response_active = true;
                    st.delegated_at = now;
                    if self.captions && !st.pager.busy() {
                        st.pager.begin_reply(now);
                        st.reply_open = true;
                    }
                    !st.pager.busy()
                };
                if thinking {
                    self.set("thinking");
                }
            }
            "response.event" => {
                let ev = attr(event, "event");
                let ev = if truthy(Some(&ev)) { ev } else { Value::Object(Obj::new()) };
                self.backend_event(&ev).await?;
            }
            "session.usage.updated" => self.note_live_usage(event),
            "session.closed" => {
                self.note_live_usage(event);
                self.ended.set();
            }
            "error" => tracing::warn!("voice: live error: {}", py_shown(&attr(event, "error"))),
            _ => {}
        }
        Ok(())
    }

    /// py: voice_agent.py:1449 `async def _backend_event(self, ev: Any) -> None:`
    ///
    /// A Responses streaming event, nested inside a Live `response.event`.
    pub(crate) async fn backend_event(self: &Arc<Self>, ev: &Value) -> anyhow::Result<()> {
        let et = attr(ev, "type");
        let et = et.as_str().unwrap_or("");
        match et {
            "response.created" => self.st.lock().response_active = true,
            "response.output_item.done" => {
                let item = attr(ev, "item");
                let item = if truthy(Some(&item)) { item } else { Value::Object(Obj::new()) };
                let kind = attr(&item, "type");
                if kind.as_str() == Some("web_search_call") {
                    self.st.lock().backend_searches += 1;
                }
                if kind.as_str() == Some("function_call") {
                    let text = |name: &str, default: &str| {
                        let v = attr(&item, name);
                        match v.as_str() {
                            Some(s) if !s.is_empty() => s.to_string(),
                            _ => default.to_string(),
                        }
                    };
                    self.tool_raising(&text("name", ""), &text("call_id", ""), &text("arguments", "{}")).await?;
                }
            }
            "response.completed" | "response.failed" | "response.incomplete" | "error" => {
                // A stream `error` is terminal too: no completed/failed follows it, and left active every later
                // response.create waited forever and the face held "thinking" (Eve/Voice.lean, sub-target A).
                if et == "error" {
                    let error = attr(ev, "error");
                    let shown = if truthy(Some(&error)) { error } else { ev.clone() };
                    tracing::warn!("voice: backend error: {}", py_shown(&shown));
                } else {
                    self.bill_backend(&attr(ev, "response"));
                }
                let now = (self.clock)();
                let wanted = {
                    let mut st = self.st.lock();
                    st.response_active = false;
                    st.last_activity = now;
                    if st.response_wanted && !self.ended.is_set() {
                        st.response_wanted = false;
                        true
                    } else {
                        false
                    }
                };
                if wanted {
                    self.conn.response_create().await?;
                } else if self.st.lock().turn_marks.speaker.as_deref() != Some("assistant") {
                    // a tool-only response: nothing will be spoken, so the phase has to
                    // move on without waiting for a caption page that never comes
                    let events = {
                        let mut st = self.st.lock();
                        if self.captions && st.reply_open {
                            st.reply_open = false;
                            Some(st.pager.reset(now))
                        } else {
                            None
                        }
                    };
                    if let Some(events) = events {
                        self.emit_events(&events);
                    }
                    self.set_after_captions(if self.task_running() { "working" } else { "listening" });
                }
            }
            "response.output_text.done" => {
                // What the backend handed the voice. Logged because the voice speaks its own
                // words: without this line a wrong reply cannot be traced to either half.
                let text = attr(ev, "text");
                let text = if truthy(Some(&text)) { py_shown(&text) } else { String::new() };
                let listening = self.st.lock().think_aloud.is_some();
                if !text.is_empty() && listening {
                    tracing::info!(
                        "voice: backend replied ({} chars; words not logged while listening)",
                        text.chars().count()
                    );
                } else if !text.is_empty() {
                    tracing::info!("voice: backend said: {}", text.chars().take(160).collect::<String>());
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// py: voice_agent.py:1491 `def _transcript_delta(self, who: str, text: str) -> None:`
    pub(crate) fn transcript_delta(self: &Arc<Self>, who: &str, text: &str) {
        if text.is_empty() {
            return;
        }
        let now = (self.clock)();
        let closed = self.st.lock().turn_marks.delta(who, text, now);
        let was_closed = closed.is_some();
        if let Some((w, t)) = closed {
            self.close_turn(&w, &t);
        }
        let fresh_turn = {
            let mut st = self.st.lock();
            let fresh = was_closed || st.turn_marks.text == text;
            if fresh {
                st.expression_chars = 0;
            }
            fresh
        };
        if who == "user" {
            {
                let mut st = self.st.lock();
                if fresh_turn {
                    st.user_turn_started_at = now; // the first words of a new turn of the owner's
                }
                st.last_activity = now;
            }
            self.speaker.stop(); // barge-in: the human talks over eve
            let reset = {
                let mut st = self.st.lock();
                if self.captions && (st.reply_open || st.pager.busy()) {
                    st.reply_open = false;
                    st.state_after_captions = None;
                    Some(st.pager.reset(now)) // ... and over the page on screen
                } else {
                    None
                }
            };
            if let Some(events) = reset {
                self.emit_events(&events);
            }
            self.set("listening");
            return;
        }
        self.st.lock().last_reply_at = now; // eve's last spoken word (a goodbye waits on it)
        self.set("speaking");
        // Stable clauses during a streamed reply; the local worker coalesces at 1.2s.
        let (current, emit) = {
            let st = self.st.lock();
            let current = st.turn_marks.text.clone();
            let fresh: String = current.chars().skip(st.expression_chars.max(0) as usize).collect();
            let n = fresh.chars().count();
            (current, n >= 24 && (CLAUSE_END.is_match(&fresh) || n >= 100))
        };
        if emit {
            self.emit_expression("assistant", &current);
            self.st.lock().expression_chars = current.chars().count() as i64;
        }
        if self.captions {
            {
                let mut st = self.st.lock();
                if !st.reply_open {
                    st.pager.begin_reply(now);
                    st.reply_open = true;
                }
                let text = st.turn_marks.text.clone();
                st.pager.update(now, &text, false);
            }
            self.flush_pager();
        }
    }

    /// py: voice_agent.py:1526 `def _close_due_turn(self) -> None:`
    pub(crate) fn close_due_turn(self: &Arc<Self>) {
        let now = (self.clock)();
        let closed = self.st.lock().turn_marks.due(now);
        if let Some((who, text)) = closed {
            self.close_turn(&who, &text);
        }
    }

    /// py: voice_agent.py:1531 `def _close_turn(self, who: str, text: str) -> None:`
    pub(crate) fn close_turn(self: &Arc<Self>, who: &str, text: &str) {
        if text.is_empty() {
            return;
        }
        if who == "user" || who == "assistant" {
            self.emit_expression(who, text);
        }
        // While listening, neither side is kept for chat memory: the learner's words go to their lesson only.
        let (listening, tainted) = {
            let mut st = self.st.lock();
            if who == "assistant" && st.held.is_some() {
                // The gate's question goes to the voice as commentary (no verbatim path exists on a Live speech
                // model): it counts as asked only once the voice's own transcript says the act whole
                // (tool_gate.said_whole). Every later turn of the voice's is checked again: the question must be the
                // last thing heard before the yes, so a reframe said after it ("...or should I not?") drops the yes
                // (review A8)
                st.held_said = eve_tool_gate::said_whole(&st.held_act, text);
            }
            (st.think_aloud.is_some(), !st.taint.is_empty())
        };
        if (who == "user" || who == "assistant") && !listening {
            let outside = who == "assistant" && tainted;
            if outside {
                // said after outside words: a later turn showing it starts tainted
                eve_tool_gate::MARKS.add([text]);
            }
            self.st.lock().turns.push((who.to_string(), text.to_string()));
            // only here: think out loud never reaches it
            if let Some(speaker) = transcript_who(who) {
                self.capture(speaker, "say", text, "", outside);
            }
        }
        if who == "user" {
            if listening {
                self.save_spoken_idea(text);
            }
            self.on_user_turn(text);
            return;
        }
        if who != "assistant" {
            return;
        }
        let now = (self.clock)();
        {
            let mut st = self.st.lock();
            st.transcript.push(text.to_string());
            st.last_activity = now;
        }
        if listening {
            tracing::info!("eve: replied ({} chars; words not logged while listening)", text.chars().count());
        } else {
            tracing::info!("eve: {text}");
        }
        if self.captions {
            {
                let mut st = self.st.lock();
                if !st.reply_open {
                    st.pager.begin_reply(now);
                    st.reply_open = true;
                }
                st.pager.update(now, text, true);
                st.pager.end_reply(now);
                st.reply_open = false;
            }
            self.flush_pager();
        }
        self.set_after_captions(if self.task_running() { "working" } else { "listening" });
    }

    /// py: voice_agent.py:1575 `def _emit_expression(self, who: str, text: str) -> None:`
    pub(crate) fn emit_expression(self: &Arc<Self>, who: &str, text: &str) {
        if let Some(on_expression) = &self.on_expression {
            if catch_unwind(AssertUnwindSafe(|| on_expression(who, text))).is_err() {
                tracing::error!("voice: expression callback failed");
            }
        }
    }
}
