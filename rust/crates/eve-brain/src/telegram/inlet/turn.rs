//! A port of `telegram.py`, lines 5104-5570: small talk, one text turn, the model loop (_think).

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

use std::sync::atomic::{AtomicI64, Ordering};

/// Python's `id(inbound)`: the key a turn holds in `_open_turns` while it runs.
static TURN_KEYS: AtomicI64 = AtomicI64::new(1);

/// Python truthiness of a JSON value.
pub(crate) fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `result.get("ok")` read as Python's `if result.get("ok")`.
fn ok_of(result: &Obj) -> bool {
    truthy(result.get("ok"))
}

/// A call's name as `call["name"]` reads it ("" when it is not a string).
fn name_of(call: &Obj) -> &str {
    call.get("name").and_then(Value::as_str).unwrap_or("")
}

/// A call's args (`call["args"]`), an empty object when it has none.
fn args_of(call: &Obj) -> Obj {
    call.get("args").and_then(Value::as_object).cloned().unwrap_or_default()
}

/// The memory sources (`_TURN_SOURCES`) as the transcript's `src` rows: each a `dict[str, str]`.
fn source_rows(sources: &[Obj]) -> Vec<HashMap<String, String>> {
    sources
        .iter()
        .map(|row| {
            row.iter()
                .map(|(k, v)| (k.clone(), v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())))
                .collect()
        })
        .collect()
}

/// `finally: self._stop_typing(chat_id, reason)` (and, for a turn, `self._turn_said = ""`), run on every exit,
/// a cancel included.
struct TypingGuard {
    inlet: Arc<TelegramInlet>,
    chat_id: i64,
    reason: &'static str,
    clear_said: bool,
}

impl Drop for TypingGuard {
    fn drop(&mut self) {
        self.inlet.stop_typing(Some(self.chat_id), self.reason);
        if self.clear_said {
            self.inlet.st.lock().turn_said.clear();
        }
    }
}

/// `_turn`'s `finally`: the turn's key out of `_open_turns`, then the end-of-turn steps, each on its own.
struct TurnGuard {
    inlet: Arc<TelegramInlet>,
    key: i64,
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        let after = {
            let mut st = self.inlet.st.lock();
            st.open_turns.remove(&self.key);
            std::mem::take(&mut st.after_turn)
        };
        for done in after {
            // the turn is over either way
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(done)).is_err() {
                tracing::warn!("telegram: an end-of-turn step failed (Exception)");
            }
        }
    }
}

impl TelegramInlet {
    /// py: telegram.py:5104 `def _chief_waits(self) -> tuple[bool, bool]:`
    ///
    /// (a chief card waits on the owner's answer, a chief job is running): its Go, its question or its budget;
    /// a step under way. A ledger that cannot be read is both, the cautious reading.
    pub(crate) fn chief_waits(self: &Arc<Self>) -> (bool, bool) {
        let Some(chief) = self.chief.lock().clone() else {
            return (false, false);
        };
        // the Rust ledger reads its own state and cannot fail: there is no unreadable case to be cautious about
        let cards = chief.ledger.open();
        let waiting = cards.iter().any(|c| {
            c.status == "waiting" && matches!(c.waiting_for.as_deref(), Some("go" | "question" | "budget"))
        });
        let running = cards.iter().any(|c| c.phases.iter().any(|p| p.status == "running"));
        (waiting, running)
    }

    /// py: telegram.py:5117 `def _small_talk_state(self, inbound: Inbound) -> small_talk.State:`
    ///
    /// What waits on the owner as this message arrives, read from the inlet's own state, for small_talk.gate.
    /// Anything it cannot read counts as waiting.
    pub(crate) fn small_talk_state(self: &Arc<Self>, inbound: &Inbound) -> eve_small_talk::State {
        let live = self.live_question();
        let (permission, relay, claude_on) = {
            let st = self.st.lock();
            let permission = st.permission_future.as_ref().is_some_and(|f| !f.done());
            // a relay on (Claude Code or Codex) is a conversation with an agent: its questions arrive as chat lines
            let relay = st.claude || st.codex_chat.is_some();
            let claude_on = st.claude_on_job.as_ref().is_some_and(|j| !j.done());
            (permission, relay, claude_on)
        };
        let confirming = permission
            || live.as_ref().is_some_and(|q| q.firm || q.strict || (q.words.contains("yes") && q.words.contains("no")));
        let (chief_waiting, chief_running) = self.chief_waits();
        let purchase = self.purchases.as_ref().is_some_and(|p| p.active(Some(inbound.chat_id)));
        let desk = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.busy)())).unwrap_or(true);
        eve_small_talk::State {
            attachment: inbound.image.is_some() || !inbound.uploads.is_empty(),
            awaiting_confirmation: confirming || purchase,
            awaiting_answer: self.awaiting_answer() || live.is_some() || chief_waiting || relay,
            running_task: self.task_running() || chief_running || desk || claude_on,
            tapped: inbound.tapped,
        }
    }

    /// py: telegram.py:5139 `def _small_talk_voice(self) -> str:`
    ///
    /// The owner's "how they like to talk" from the profile, when memory is on; "" otherwise. Blocking. Read
    /// from the whole page (Memory.talking_section), never from the clipped voice profile, whose cut drops this
    /// section first. A memory without it (a test's stand-in) lends its voice profile.
    pub(crate) fn small_talk_voice(self: &Arc<Self>) -> String {
        let Some(memory) = self.memory.as_ref() else {
            return String::new();
        };
        let page = memory.talking_section().unwrap_or_else(|| memory.profile_for_voice());
        eve_small_talk::voice_from_profile(&page)
    }

    /// py: telegram.py:5152 `async def _small_talk_lane(self, inbound: Inbound) -> bool:`
    ///
    /// The small-talk lane, inside the turn's lock: True when it answered (the turn is over); False when the
    /// brain answers, with nothing written or said (off, not a small-talk phrase, eve waiting on the owner, a
    /// call, the think grade, or Haiku failed).
    pub(crate) async fn small_talk_lane(self: &Arc<Self>, inbound: &Inbound) -> bool {
        let Some(ask) = self.small_talk_ask.clone() else {
            return false;
        };
        if !eve_small_talk::matches(&inbound.text) {
            return false;
        }
        let c = ctx();
        if c.call.is_some() || (c.app.is_some() && c.app_grade == "think") {
            return false; // a call reads its answer aloud as the brain streams it; the thinking model
        }
        if !eve_small_talk::gate(&inbound.text, &self.small_talk_state(inbound)).is_empty() {
            return false;
        }
        let (t0, arrived) = ((self.clock)(), (self.wall)());
        let voice = self.small_talk_voice();
        self.keep_typing(Some(inbound.chat_id), "turn", TYPING_TURN_SECS);
        let answer = {
            let _typing = TypingGuard { inlet: self.clone(), chat_id: inbound.chat_id, reason: "turn", clear_said: false };
            match ask(inbound.text.clone(), voice).await {
                Ok(answer) => answer,
                // a lane that breaks costs nothing: the brain answers
                Err(e) => eve_small_talk::Answer {
                    reason: "error".into(),
                    error: crate::telegram::inlet::turn::error_type(&e),
                    ..Default::default()
                },
            }
        };
        if !answer.answered() {
            // reasons and seconds only, never the message or the reply
            let error = if answer.error.is_empty() { String::new() } else { format!(" {}", answer.error) };
            tracing::info!("telegram: small talk fell through ({}{}) after {:.1} s", answer.reason, error, answer.secs);
            return false;
        }
        if self.needs_seed() {
            self.seed_log();
        }
        self.note("user", &inbound.text, "say", None, Some(arrived), None, false, None);
        self.note("eve", &answer.reply, "say", None, None, None, false, None);
        self.say(inbound.chat_id, &answer.reply, None, None, &[], 0, None).await;
        self.chief_heard(inbound.chat_id, &inbound.text).await;
        tracing::info!("telegram: turn answered in {:.1} s (small talk, {})", (self.clock)() - t0, answer.model);
        true
    }

    /// `self._chat_log is not None and self._memory is not None and self._log_chat() not in self._seeded_chats`.
    fn needs_seed(self: &Arc<Self>) -> bool {
        let chat = self.log_chat();
        let st = self.st.lock();
        st.chat_log.is_some() && self.memory.is_some() && !st.seeded_chats.contains(&chat)
    }

    /// py: telegram.py:5189 `async def _turn(self, inbound: Inbound, *, image: Optional[telegram_images.ReceivedImage] = None, images: Sequence[telegram_images.ReceivedImage] = (), handoff: bool = False, lane: bool = False) -> None:`
    ///
    /// One message's turn, one at a time. ``lane``: the message came straight from _handle's last step, past
    /// every code path, so the small-talk lane may answer it (_small_talk_lane); never while another of the owner's
    /// turns was in flight when it arrived (the owner may be adding to that one).
    pub async fn turn(self: &Arc<Self>, inbound: &Inbound, image: Option<&eve_telegram_images::ReceivedImage>, images: &[eve_telegram_images::ReceivedImage], handoff: bool, lane: bool) {
        let key = TURN_KEYS.fetch_add(1, Ordering::Relaxed);
        let queued = {
            let mut st = self.st.lock();
            let queued = !st.open_turns.is_empty();
            st.open_turns.insert(key, (inbound.chat_id, inbound.message_id));
            queued
        };
        let _guard = TurnGuard { inlet: self.clone(), key };
        // the turn's context variables are its own (asyncio runs each task in a copy of its spawner's context)
        in_ctx(ctx(), self.answer_turn(inbound, image, images, handoff, lane && !queued)).await;
    }

    /// py: telegram.py:5210 `async def _answer_turn(self, inbound: Inbound, *, image: Optional[telegram_images.ReceivedImage] = None, images: Sequence[telegram_images.ReceivedImage] = (), handoff: bool = False, lane: bool = False) -> None:`
    ///
    /// One message's turn. ``handoff``: daily_rundown handed this message on (_rundown_handoff): it is answered
    /// as a rundown, and its words are already in the transcript and the chief's ear from the turn that handed it
    /// on, so neither hears them twice. ``lane``: the small-talk lane may answer it first (_small_talk_lane); when
    /// it does, nothing below runs; when it does not, nothing was written or said and the turn goes on as ever.
    pub(crate) async fn answer_turn(self: &Arc<Self>, inbound: &Inbound, image: Option<&eve_telegram_images::ReceivedImage>, images: &[eve_telegram_images::ReceivedImage], handoff: bool, lane: bool) {
        let _lock = self.turn_lock.lock().await;
        if lane
            && !handoff
            && image.is_none()
            && images.is_empty()
            && inbound.uploads.is_empty()
            && self.small_talk_lane(inbound).await
        {
            return;
        }
        let chat_id = inbound.chat_id;
        {
            let mut st = self.st.lock();
            st.turn_request = inbound.message_id; // a task this turn starts replies to it
            st.streamed = None;
        }
        let t0 = (self.clock)();
        let pictures: Vec<&eve_telegram_images::ReceivedImage> =
            if images.is_empty() { image.into_iter().collect() } else { images.iter().collect() };
        let files: Vec<&eve_link_uploads::Upload> = inbound.uploads.iter().filter(|u| u.kind != "image").collect();
        let first = if !inbound.text.is_empty() {
            inbound.text.as_str()
        } else if !pictures.is_empty() || files.is_empty() {
            "Please describe this image."
        } else {
            "Please look at this file."
        };
        let mut user_item = message_item("user", first);
        let mut content: Vec<Value> = user_item.get("content").and_then(Value::as_array).cloned().unwrap_or_default();
        for picture in &pictures {
            content.push(json!({"type": "input_image", "image_url": picture.data_url(), "detail": "auto"}));
        }
        for upload in &files {
            let upload = (*upload).clone();
            let name = upload.name.clone();
            match tokio::task::spawn_blocking(move || eve_link_uploads::brain_parts(&upload)).await {
                Ok(Ok(parts)) => content.extend(parts),
                _ => content.push(json!({"type": "input_text", "text": format!("(The attached file \"{name}\" could not be read.)")})),
            }
        }
        user_item.insert("content".into(), Value::Array(content));
        let attached = !pictures.is_empty() || !files.is_empty();
        let arrived = (self.wall)();
        self.st.lock().recall_seen.clear();
        // this turn's memory sources, its own (memory_sources.py)
        set_ctx(|c| c.sources = Some(Arc::new(Mutex::new(Vec::new()))));
        if self.needs_seed() {
            self.seed_log();
        }
        // with the chat log, a quiet spell or a restart before this message is a gap note right before it
        let mut items = self.history();
        items.extend(self.gap_before_now(arrived));
        items.push(user_item);
        let mut shown: Vec<String> = {
            let st = self.st.lock();
            let from = st.turn_kinds.len().saturating_sub(HISTORY_TURNS);
            st.turn_kinds[from..].to_vec() // the history's lines, as the transcript has them
        };
        // eve lite has no rundown (its skill reads a vault and apps lite does not have): its words are an ordinary
        // turn, with every tool lite has, Composio's connection manager and its sign-in links included
        let lite = crate::telegram::prompts::lite(self.environ.as_ref());
        let daily = handoff || (!attached && !lite && eve_rundown::matches(&inbound.text));
        let mut carried_in = String::new();
        if daily {
            let school = self.canvas_rundown().await.map(Value::Object);
            let root = self.vault.as_ref().map(|v| v.root.clone());
            let now = chrono::DateTime::from_timestamp((self.wall)().floor() as i64, (((self.wall)().fract()) * 1e9) as u32)
                .unwrap_or_default()
                .with_timezone(&chrono::Local)
                .fixed_offset();
            let direct_apps = self.direct_up();
            let composio = self.composio_up();
            let (skill, fed) = tokio::task::spawn_blocking(move || {
                let apps: Vec<&str> = direct_apps.iter().map(String::as_str).collect();
                eve_rundown::context_and_outside(root.as_deref(), &now, school.as_ref(), &apps, composio)
            })
            .await
            .unwrap_or_default();
            // vault todos or Canvas titles fed it: the turn starts tainted, so its reply is kept as outside
            // words even when no read tool ran (round-4 review)
            carried_in = if fed { eve_tool_gate::source("daily_rundown").unwrap_or_default() } else { String::new() };
            items = vec![message_item("user", &inbound.text), message_item("developer", &skill)];
            shown = Vec::new();
        }
        if !handoff {
            let text = if attached {
                format!("{} {}", inbound.text, attached_line(inbound)).trim().to_string()
            } else {
                inbound.text.clone()
            };
            let att = att_of(inbound);
            self.note(
                "user",
                &text,
                if pictures.is_empty() { "say" } else { "image" },
                if attached { Some(&inbound.text) } else { None },
                Some(arrived),
                att.as_deref(),
                false,
                None,
            );
        }
        let tail = {
            let st = self.st.lock();
            match st.turn_kinds.last() {
                Some(last) => {
                    let mut kinds = shown.clone();
                    kinds.push(last.clone());
                    Self::shown(&kinds)
                }
                None => 0,
            }
        };
        // "typing…" for the whole turn, not only its first 5 s (owner, 2026-09-23), gone before the reply.
        self.keep_typing(Some(chat_id), "turn", TYPING_TURN_SECS);
        let mut usage: HashMap<String, i64> = ["in", "cached", "out", "instr"].iter().map(|k| (k.to_string(), 0)).collect();
        self.st.lock().turn_said = inbound.text.clone(); // take_on's door floor reads it; gone with the turn
        let outcome = {
            let _typing = TypingGuard { inlet: self.clone(), chat_id, reason: "turn", clear_said: true };
            // an attached file's words, or what a picture shows, are not the owner's: the turn starts
            // tainted (tool_gate.py)
            let taint = if !files.is_empty() {
                eve_tool_gate::ATTACHED.to_string()
            } else if !pictures.is_empty() {
                eve_tool_gate::PICTURE.to_string()
            } else {
                carried_in
            };
            self.think(&items, chat_id, daily, tail, Some(&mut usage), &taint).await
        };
        let (reply, rounds) = match outcome {
            Ok(done) => done,
            Err(e) => {
                // the type only: a message can quote what was asked
                tracing::warn!("telegram: turn failed: {}", error_type(&e));
                let mut failed = if eve_computer_agent::is_connection_drop(&e) { DROPPED_LINE } else { FAILED_LINE };
                if lite && no_credit(&e) {
                    failed = LITE_NO_CREDIT_LINE; // OpenRouter's 402: the line says what to do about it
                } else if lite && key_rejected(&e) {
                    failed = LITE_KEY_REJECTED_LINE; // OpenRouter's 401: the OAuth key is gone; sign in again
                }
                // the chat log keeps the line, so the next turn sees this message was not answered
                self.log_line("eve", failed, "", Some(""), "", None, 0, false);
                self.say(chat_id, failed, None, None, &[], 0, None).await;
                return;
            }
        };
        if !reply.is_empty() {
            // Kept whole, OpenUI Lang programs included: a later app turn's history reads them as written, and a
            // Telegram, voice or call turn's reads each as one plain line (link_chat.history_text).
            // the memory lines the reply leaned on ride on its transcript line only (Sources in the app)
            let sources = ctx().sources.map(|s| s.lock().clone()).unwrap_or_default();
            let rows = source_rows(&sources);
            let outside = !self.st.lock().taint.is_empty();
            self.note("eve", &reply, "say", None, None, None, outside, if rows.is_empty() { None } else { Some(&rows) });
            self.st.lock().recall_seen.clear();
            set_ctx(|c| c.sources = None);
            self.say(chat_id, &reply, None, None, &[], 0, None).await;
        }
        if !handoff {
            self.chief_heard(chat_id, &inbound.text).await;
        }
        // Counts and seconds only: never what was asked, never what was answered. The token counts say
        // whether the cache-ordered prompt pays off (cached of in), and instr how big the prefix is.
        tracing::info!(
            "telegram: turn answered in {:.1} s ({} model call{}; in={} cached={} out={} instr={} chars)",
            (self.clock)() - t0,
            rounds,
            if rounds == 1 { "" } else { "s" },
            usage["in"],
            usage["cached"],
            usage["out"],
            usage["instr"]
        );
    }

    /// py: telegram.py:5317 `def _count_usage(response: Any, usage: dict[str, int]) -> None:`
    ///
    /// Add one response's token counts to the turn's. A reply without usage adds nothing.
    ///
    /// Rust: `usage` is `&mut`, the dict Python adds into in place.
    pub fn count_usage(response: &Value, usage: &mut HashMap<String, i64>) {
        let Some(u) = response.get("usage").and_then(Value::as_object) else {
            return;
        };
        // an int, never a bool (JSON keeps them apart) nor a float
        let num = |v: Option<&Value>| v.and_then(|v| if v.is_i64() || v.is_u64() { v.as_i64() } else { None }).unwrap_or(0);
        let details = u.get("input_tokens_details").and_then(Value::as_object);
        *usage.entry("in".into()).or_insert(0) += num(u.get("input_tokens"));
        *usage.entry("out".into()).or_insert(0) += num(u.get("output_tokens"));
        *usage.entry("cached".into()).or_insert(0) += details.map(|d| num(d.get("cached_tokens"))).unwrap_or(0);
    }

    /// The names a response calls that ``AllowedNames.owns`` claims although the turn did not offer them: a
    /// direct app's tool not opened this turn, or (routed) Composio's while it is up. Python's AllowedNames answers
    /// `in` with them; parse_response here takes a plain list, so they are added to it before the parse.
    fn owned_names(self: &Arc<Self>, response: &Value, allowed: &[String]) -> Vec<String> {
        let routed = self.st.lock().routed;
        let mut out = allowed.to_vec();
        let Some(output) = response.get("output").and_then(Value::as_array) else {
            return out;
        };
        for item in output {
            if item.get("type").and_then(Value::as_str) != Some("function_call") {
                continue;
            }
            let Some(name) = item.get("name").and_then(Value::as_str) else {
                continue;
            };
            if name.is_empty() || out.iter().any(|n| n == name) {
                continue;
            }
            let owned = if routed {
                self.routed_owns(name)
            } else {
                self.direct.as_ref().is_some_and(|d| d.owns(name))
            };
            if owned {
                out.push(name.to_string());
            }
        }
        out
    }

    /// `request(self.config, items, context=note, app_block=app_block, **parts)`.
    pub(crate) fn request_with(self: &Arc<Self>, items: &[Obj], context: &str, app_block: &str, parts: &Obj) -> Obj {
        let s = |k: &str| parts.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let objs = |k: &str| -> Vec<Obj> {
            parts.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(|t| t.as_object().cloned()).collect()).unwrap_or_default()
        };
        let strs = |k: &str| -> Vec<String> {
            parts.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect()).unwrap_or_default()
        };
        request_in(
            self.environ.as_ref(),
            &self.config,
            items,
            "",
            &s("profile"),
            &objs("app_tools"),
            parts.get("vault").and_then(Value::as_bool).unwrap_or(false),
            &s("today"),
            &objs("memory_tools"),
            Some(context),
            &objs("watch_tools"),
            &s("watch_block"),
            &strs("direct_apps"),
            &s("lent_block"),
            app_block,
            &s("recall"),
            &objs("chat_tools"),
        )
    }

    /// One response's spend (spend.record_response), off the loop's hot path but never lost.
    fn record_spend(feature: &str, response: &Value, payload: &Obj) {
        let model = match payload.get("model") {
            Some(Value::String(m)) => m.clone(),
            None | Some(Value::Null) => String::new(),
            Some(other) => other.to_string(),
        };
        eve_spend::record_response(feature, response, &model, "openrouter", None);
    }

    /// py: telegram.py:5331 `async def _think(self, items: list[dict[str, Any]], chat_id: int, *, daily: bool = False, tail: int = 0, usage: Optional[dict[str, int]] = None, taint: str = "") -> tuple[str, int]:`
    ///
    /// Rust: an `Err` is the model call raising (Python's exception out of `_think`); `usage` is `&mut`.
    pub(crate) async fn think(self: &Arc<Self>, items: &[Obj], chat_id: i64, daily: bool, tail: i64, usage: Option<&mut HashMap<String, i64>>, taint: &str) -> anyhow::Result<(String, i64)> {
        let mut own_usage: HashMap<String, i64> = HashMap::new();
        let usage = match usage {
            Some(u) => u,
            None => {
                for k in ["in", "cached", "out", "instr"] {
                    own_usage.insert(k.into(), 0);
                }
                &mut own_usage
            }
        };
        let mut items: Vec<Obj> = items.to_vec();
        // The taint gate (tool_gate.py): clean until a call returns outside text, or tainted from the start by an
        // attached file (``taint``); from then on an acting call is the owner's yes first (_gate).
        self.st.lock().taint = taint.to_string();
        set_ctx(|c| c.memory_calls = Some(Arc::new(Mutex::new(0)))); // this turn's memory reads, its own
        // One developer note for the whole turn: the clock and the brief, identical in every round of it; on a
        // phone call, also how to answer out loud (it rides the turn's note, so the cached prefix is untouched).
        let mut note = turn_context(&(self.brief)(), None);
        let line = self.chief_line();
        if !line.is_empty() {
            // the chief's open cards: after the history, cache-free
            note += &format!("\n\n{line}");
        }
        let lite = crate::telegram::prompts::lite(self.environ.as_ref()); // eve lite (EVE_PROFILE=lite): fixed for the turn
        let call_say = self.call_reader(); // fixed for the turn: it began on the call, or it did not
        if call_say.is_some() {
            note += &format!("\n\n{}", if lite { CALL_NOTE_LITE } else { CALL_NOTE }); // eve lite's calls are its web app's
        }
        // A turn from the Eve Link app: the app renders OpenUI Lang, so the brain is told how (link_chat.py).
        let c = ctx();
        let from_app = c.app.is_some();
        let mut grade = if from_app { c.app_grade.clone() } else { "auto".to_string() }; // the speed dial's (GRADES)
        let app_block = if from_app { eve_link::link_chat::app_block(None) } else { String::new() };
        // An "auto" app turn is graded by Jev beside the toolset router and the prompt's building (a thread each,
        // awaited once both are under way), so it usually adds no wall time. When the toolset router is off or
        // keywords-only, the turn can wait up to 0.8 s for the grade. Telegram turns are never graded.
        let grading = if grade == "auto" && from_app && !daily && self.grader.is_some() {
            let said = self.st.lock().turn_said.clone();
            let previous = self.previous_said();
            let grader = self.grader.clone();
            Some(tokio::task::spawn_blocking(move || {
                eve_grade_router::grade_turn(&said, &previous, grader.as_ref(), None, eve_grade_router::TIMEOUT_S)
            }))
        } else {
            None
        };
        // The direct apps this turn starts with: the ones its words name, or the rundown's (apps_mcp.APP_WORDS).
        let up: HashSet<String> = match &self.direct {
            Some(d) if d.started() => d.apps().into_iter().collect(),
            _ => HashSet::new(),
        };
        let routed = !daily && self.toolset_mode != "off";
        {
            let mut st = self.st.lock();
            st.lent.clear();
            st.daily = daily; // a rundown offers only its read tools, so only their blocks ride
            st.routed = routed;
        }
        let (mut parts, mut allowed, mut app_tools);
        if !routed {
            let open: HashSet<String> = if daily {
                eve_rundown::DIRECT_APPS.iter().map(|a| a.to_string()).filter(|a| up.contains(a)).collect()
            } else if !up.is_empty() {
                let said = self.st.lock().turn_said.clone();
                self.direct.as_ref().map(|d| d.guess(&said)).unwrap_or_default()
            } else {
                HashSet::new()
            };
            self.st.lock().direct_open = open;
            (parts, allowed, app_tools) = self.turn_parts(tail, false).await;
        } else {
            // The toolset router (2026-10-05): keywords ∪ Jev pick every toolset the turn starts with, direct apps
            // and Share and the workspace alike. Jev's 150-300 ms runs beside the prompt's building (the
            // memory reads), so it is hidden; then the tools are rebuilt so round 1 already has what it needs, and
            // no open_app round (nor the cache it busts) is spent on it.
            self.st.lock().direct_open.clear();
            let said = self.st.lock().turn_said.clone();
            let previous = self.previous_said();
            let route;
            if self.toolset_route.is_none() {
                // keywords only (no Jev route): a few regexes, run here; a thread hop would buy nothing
                route = eve_toolset_router::route_turn(&said, &previous, None, eve_toolset_router::TIMEOUT_S);
                (parts, allowed, _) = self.turn_parts(tail, false).await;
            } else {
                let router = self.toolset_route.clone();
                let routing = tokio::task::spawn_blocking(move || {
                    eve_toolset_router::route_turn(&said, &previous, router.as_ref(), eve_toolset_router::TIMEOUT_S)
                });
                let (routed_to, built) = tokio::join!(routing, self.turn_parts(tail, false));
                route = routed_to.unwrap_or_default();
                (parts, allowed, _) = built;
            }
            let toolsets = route.toolsets();
            let lendable: HashSet<String> = self.lendable().into_iter().collect();
            {
                let mut st = self.st.lock();
                st.direct_open = toolsets.iter().filter(|t| up.contains(*t)).cloned().collect();
                st.lent = toolsets.iter().filter(|t| lendable.contains(*t)).cloned().collect();
            }
            tracing::info!("telegram: toolsets: {}", route.log_line()); // names and ms, never the message
            app_tools = self.refresh_lent(&mut parts, &mut allowed);
        }
        if let Some(grading) = grading {
            if let Ok(g) = grading.await {
                tracing::info!("telegram: grade: {}", g.log_line()); // numbers only, never the message
                if g.grade == eve_grade_router::THINK {
                    grade = "think".into(); // as the explicit think grade; instant keeps auto, today's turn
                }
            }
        }
        let mut text = String::new();
        let mut said_on_call = false;
        for round_no in 1..=MAX_TOOL_ROUNDS as i64 {
            let mut payload = self.request_with(&items, &note, &app_block, &parts);
            if round_no == 1 {
                let instructions = payload.get("instructions").cloned().unwrap_or(Value::Null);
                usage.insert("instr".into(), instructions.as_str().map(|s| s.chars().count() as i64).unwrap_or(0));
                if self.st.lock().taint.is_empty() {
                    // outside words already in this prompt (a read's line in today's block, a tainted turn's reply
                    // in the history) taint the turn from its start: they cross turns as eve's own (tool_gate.py)
                    let input = payload.get("input").cloned().unwrap_or(Value::Null);
                    let carried = eve_tool_gate::carried([&instructions, &input], None);
                    self.st.lock().taint = carried.to_string();
                }
            }
            if daily {
                payload.insert("tools".into(), Value::Array(self.rundown_tools(&app_tools).into_iter().map(Value::Object).collect()));
            }
            let payload = graded(&payload, &self.config, &grade);
            let response = match (&call_say, &self.stream_create) {
                (Some(_), Some(stream_create)) => {
                    // on a call: each sentence is read out as soon as the model has written it
                    // each sentence through _to_call, so a call that ends mid-turn (or another that takes the listener)
                    // is not read the rest of it (CallScope.lean, cross-call)
                    let me = self.clone();
                    let reader = Arc::new(SentenceStream::new(Arc::new(move |sentence: &str| me.to_call(chat_id, sentence, None)), 12));
                    let feeder = reader.clone();
                    let response = stream_create(Value::Object(payload.clone()), Arc::new(move |delta: &str| feeder.feed(delta))).await?;
                    reader.flush();
                    let streamed = reader.text();
                    self.st.lock().streamed = if streamed.is_empty() { None } else { Some(streamed) };
                    response
                }
                _ => (self.create)(Value::Object(payload.clone())).await?,
            };
            Self::count_usage(&response, usage);
            Self::record_spend(eve_spend::CHAT, &response, &payload);
            let claimable = self.owned_names(&response, &allowed);
            let (mut calls, round_text, carry) = parse_response(&response, &claimable, true)?;
            text = round_text;
            if grade == "instant" {
                for c in calls.iter_mut() {
                    // not offered on instant: refused like any name not offered
                    if name_of(c) == "think_hard" {
                        c.insert("unknown".into(), Value::Bool(true));
                    }
                }
            }
            if lite {
                for c in calls.iter_mut() {
                    // eve lite never offers it: refused the same way, before the taint gate could ask the owner
                    // about it (_tool refuses too)
                    if !lite_has(name_of(c)) {
                        c.insert("unknown".into(), Value::Bool(true));
                    }
                }
            }
            if calls.is_empty() {
                return Ok((text, round_no));
            }
            tracing::info!("telegram: round {}: {}", round_no, call_names(&calls));
            if call_say.is_some()
                && text.trim().is_empty()
                && !said_on_call
                && calls.iter().any(|c| matches!(name_of(c), "start_task" | "opentable_search"))
            {
                // a long job gets a short "On it."; a lookup says nothing until it has the answer (owner,
                // 2026-10-04, of "One sec, checking.": "i dont need one sec checking", "its annoying")
                said_on_call = true;
                self.to_call(chat_id, CALL_ON_IT, None);
            } else if !text.trim().is_empty() {
                said_on_call = true;
            }
            items.extend(carry);
            let mut results: Vec<Obj> = Vec::new();
            let opened_before = {
                let st = self.st.lock();
                (st.direct_open.clone(), st.lent.clone())
            };
            let offered: Vec<Obj> = payload
                .get("tools")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|t| t.as_object().cloned()).collect())
                .unwrap_or_default();
            for call in &calls {
                let name = name_of(call);
                let args = args_of(call);
                let (chat_log, taint_now) = {
                    let st = self.st.lock();
                    (st.chat_log.is_some(), st.taint.clone())
                };
                let result = if truthy(call.get("unknown")) {
                    // A name this turn never offered: refused as the call's result, naming the tools it has, so
                    // the next round repairs it (review, 2026-10-05). Before, it raised and the turn was lost. The
                    // reason never repeats the wrong name (owner's rule): it gives the set that is right.
                    tracing::info!("telegram: refused a call to a tool this turn does not offer");
                    let mut r = Obj::new();
                    r.insert("ok".into(), Value::Bool(false));
                    r.insert(
                        "reason".into(),
                        Value::String(format!("that name is not a tool here; the tools this turn has: {}", offered_names(&offered).join(", "))),
                    );
                    r
                } else if daily
                    && !eve_rundown::allows(name, &args, self.direct.as_deref().map(|d| d as &dyn eve_rundown::Direct))
                    && !(Some(name) == RECENT_MESSAGES_TOOL.get("name").and_then(Value::as_str) && chat_log)
                {
                    let mut r = Obj::new();
                    r.insert("ok".into(), Value::Bool(false));
                    r.insert("reason".into(), json!("rundown only reads email, calendar, Slack and the supplied Obsidian todos"));
                    r
                } else if name == "start_task" && self.mail_task(&args) {
                    tracing::info!("telegram: start_task refused: email goes through Gmail's tool");
                    let mut r = Obj::new();
                    r.insert("ok".into(), Value::Bool(false));
                    r.insert("reason".into(), json!(MAIL_SEND_REASON));
                    r
                } else if eve_tool_gate::needs_confirmation(name, &args, &taint_now) {
                    // Only consequential native calls ask; delegated tools own their final action's approval.
                    match self.gate(name, &args, chat_id, None).await {
                        Some(r) if !r.is_empty() => r,
                        _ => self.tool(name, &args, chat_id).await,
                    }
                } else {
                    self.tool(name, &args, chat_id).await
                };
                if TRANSCRIBED_TOOLS.contains(&name) && result.get("ok") != Some(&Value::Bool(false)) {
                    self.record("eve", "tool", &tool_line(&Value::Object(result.clone())), name, None, false, None);
                }
                let call_id = call.get("call_id").cloned().unwrap_or(Value::Null);
                let mut out = Obj::new();
                out.insert("type".into(), json!("function_call_output"));
                out.insert("call_id".into(), call_id);
                out.insert("output".into(), Value::String(eve_computer_agent::py::dumps(&Value::Object(result.clone()))));
                items.push(out);
                results.push(result);
            }
            if self.st.lock().taint.is_empty() {
                // a result with words from outside eve taints the rest of the turn: this round's calls were
                // written before it existed, the next round's are written after reading it (tool_gate.py)
                let source = calls
                    .iter()
                    .filter(|c| !truthy(c.get("unknown")) && eve_tool_gate::taints(name_of(c), &args_of(c)))
                    .map(|c| eve_tool_gate::source(name_of(c)).unwrap_or_default())
                    .next()
                    .unwrap_or_default();
                self.st.lock().taint = source;
            }
            let opened_now = {
                let st = self.st.lock();
                (st.direct_open.clone(), st.lent.clone())
            };
            if opened_now != opened_before {
                // open_app (or a call to a closed toolset's tool) opened a toolset: its tools are offered from the
                // next round of this turn
                app_tools = self.refresh_lent(&mut parts, &mut allowed);
            }
            if calls.iter().all(|c| matches!(name_of(c), "start_task" | "opentable_search")) && results.iter().all(ok_of) {
                // A started task needs no second model call to say so (measured live 2026-09-21: the
                // round that only produced "On it!" cost 1.9 s). The result is texted when it exists.
                // Its progress message already says "On it" (_start_task), so only the model's own words,
                // if it wrote any, go as a reply as well.
                return Ok((text, round_no));
            }
            if calls.iter().all(|c| name_of(c) == "take_on") && results.iter().all(|r| ok_of(r) && truthy(r.get("end_turn"))) {
                // take_on ends the turn as start_task does: the backbrief, composed by code, went out from the
                // tool (_chief_tool) with its buttons; one Telegram refused goes as the reply instead.
                let unsent: Vec<String> = results
                    .iter()
                    .filter(|r| !truthy(r.get("sent")))
                    .map(|r| match r.get("backbrief") {
                        Some(Value::String(s)) => s.clone(),
                        Some(v) if truthy(Some(v)) => v.to_string(),
                        _ => String::new(),
                    })
                    .collect();
                let joined: Vec<&str> = std::iter::once(text.as_str()).chain(unsent.iter().map(String::as_str)).filter(|x| !x.is_empty()).collect();
                return Ok((joined.join("\n"), round_no));
            }
            if calls.iter().all(|c| eve_menu_tools::HANDS_OFF.contains(&name_of(c)))
                && results.iter().all(|r| eve_menu_tools::ended(&Value::Object(r.clone())))
            {
                // a / menu command's handler sent its own message (its button, picker or line), as the typed
                // command does: no second model call to say it again (menu_tools.py)
                return Ok((text, round_no));
            }
            let receipts: Vec<Option<String>> =
                calls.iter().zip(results.iter()).map(|(c, r)| Self::receipt_of(c, Some(&Value::Object(r.clone())))).collect();
            if receipts.iter().all(|r| r.as_deref().is_some_and(|e| !e.is_empty())) && results.iter().all(ok_of) {
                // Kept in the vault or starred: a ✍ or 🏆 on the owner's message is the receipt (owner,
                // 2026-09-23), with no second model call to write "Saved." (the same saving as start_task).
                // The model's own words, if it wrote any, still go; when the reaction fails, the line it
                // stood for goes in their place.
                let line = calls.iter().zip(results.iter()).map(|(c, r)| receipt_line(name_of(c), r)).collect::<Vec<_>>().join("\n");
                let emoji = if receipts.iter().any(|e| e.as_deref() == Some(STAR_REACTION)) { STAR_REACTION } else { VAULT_REACTION };
                let request_id = self.st.lock().turn_request;
                let reacted = self.receipt(chat_id, request_id, emoji, "").await;
                if reacted && text.is_empty() {
                    // The history still says what was kept. The transcript has it already, as the tool's
                    // own line, so this one is the history's only.
                    self.history_only("eve", &format!("({line})"));
                }
                if !reacted {
                    return Ok((if text.is_empty() { line } else { text }, round_no));
                }
                // One reaction per message: in a round that starred a fact and kept a note, the 🏆 wins, and
                // where the note went (inbox, todo list, journal) is said in words, not lost (review, 2026-09-23).
                let others: Vec<String> = calls
                    .iter()
                    .zip(results.iter())
                    .zip(receipts.iter())
                    .filter(|(_, e)| e.as_deref() != Some(emoji))
                    .map(|((c, r), _)| receipt_line(name_of(c), r))
                    .collect();
                let joined: Vec<&str> = std::iter::once(text.as_str()).chain(others.iter().map(String::as_str)).filter(|x| !x.is_empty()).collect();
                return Ok((joined.join("\n"), round_no));
            }
        }
        // Every round used and the work not finished: one more call, with tools off, says what was found.
        tracing::info!("telegram: turn used all {} rounds; asking for what was found so far", MAX_TOOL_ROUNDS);
        let mut payload = self.request_with(&items, &format!("{note}\n\n{OUT_OF_ROUNDS_NOTE}"), &app_block, &parts);
        if daily {
            payload.insert("tools".into(), Value::Array(self.rundown_tools(&app_tools).into_iter().map(Value::Object).collect()));
        }
        let mut payload = graded(&payload, &self.config, &grade);
        payload.insert("tool_choice".into(), json!("none"));
        let response = match (self.create)(Value::Object(payload.clone())).await {
            Ok(r) => r,
            Err(e) => {
                // the summary is a courtesy; the turn's own failure line still goes
                tracing::warn!("telegram: the out-of-rounds summary failed ({})", error_type(&e));
                let out = if text.is_empty() { TANGLED_LINE.to_string() } else { text };
                return Ok((out, MAX_TOOL_ROUNDS as i64));
            }
        };
        Self::count_usage(&response, usage);
        Self::record_spend(eve_spend::CHAT, &response, &payload);
        // a call made despite tool_choice "none" is not run, and never fails the turn (Lean RoutedTurn P1)
        let claimable = self.owned_names(&response, &allowed);
        let (_calls, summary, _carry) = parse_response(&response, &claimable, true)?;
        let out = if !summary.is_empty() {
            summary
        } else if !text.is_empty() {
            text
        } else {
            TANGLED_LINE.to_string()
        };
        Ok((out, MAX_TOOL_ROUNDS as i64 + 1))
    }

    /// py: telegram.py:5535 `def _receipt_of(call: dict[str, Any], result: Any = None) -> Optional[str]:`
    ///
    /// The reaction a tool call's success stands for, or None when it is news the model should word. Telegram:
    /// RECEIPT_TOOLS. An Eve Link app turn (``_FROM_APP``): APP_RECEIPT_TOOLS, where take_notes counts only
    /// when it starts the note taker (a stop has a file to tell about, a status is an answer) and only when it was
    /// not already running: "already taking notes" is news, so the model says it, as it does in Telegram
    /// (review, 2026-10-09).
    pub fn receipt_of(call: &Obj, result: Option<&Value>) -> Option<String> {
        let name = call.get("name").and_then(Value::as_str);
        let lookup = |table: &[(&str, &str)]| -> Option<String> {
            let name = name?;
            table.iter().find(|(n, _)| *n == name).map(|(_, e)| e.to_string())
        };
        if ctx().app.is_none() {
            return lookup(RECEIPT_TOOLS);
        }
        if name == Some("take_notes") {
            if let Some(Value::Object(r)) = result {
                if truthy(r.get("already")) {
                    return None;
                }
            }
            let start = call.get("args").and_then(Value::as_object).and_then(|a| a.get("action")) == Some(&json!("start"));
            return if start { Some(VAULT_REACTION.to_string()) } else { None };
        }
        lookup(APP_RECEIPT_TOOLS)
    }

    /// py: telegram.py:5551 `def _rundown_tools(self, app_tools: list[dict[str, Any]]) -> list[dict[str, Any]]:`
    ///
    /// A rundown's tools: its read tools (rundown.tools), and recent_messages while the chat log is kept. The
    /// instructions name recent_messages on every turn (recall_line, in the cached prefix), so a rundown offers it
    /// too: no name in a prompt that the turn does not offer (owner's rule; review, 2026-10-08).
    pub(crate) fn rundown_tools(self: &Arc<Self>, app_tools: &[Obj]) -> Vec<Obj> {
        let values: Vec<Value> = app_tools.iter().cloned().map(Value::Object).collect();
        let direct = self.direct.as_deref().map(|d| d as &dyn eve_rundown::Direct);
        let mut tools: Vec<Obj> = eve_rundown::tools(&values, direct).into_iter().filter_map(|t| t.as_object().cloned()).collect();
        if self.st.lock().chat_log.is_some() {
            if let Some(t) = RECENT_MESSAGES_TOOL.as_object() {
                tools.push(t.clone());
            }
        }
        tools
    }

    /// py: telegram.py:5558 `async def _canvas_rundown(self) -> Optional[dict[str, Any]]:`
    ///
    /// The rundown's Canvas part (canvas.Canvas.rundown: the next three days' unsubmitted deadlines), read by code
    /// before the turn so the rundown's tools stay what they were; None without Canvas. Never raises.
    pub(crate) async fn canvas_rundown(self: &Arc<Self>) -> Option<Obj> {
        let canvas = self.canvas.clone()?;
        let secs = std::time::Duration::from_secs_f64(CANVAS_RUNDOWN_SECS);
        match tokio::time::timeout(secs, canvas.rundown_default()).await {
            Ok(Value::Object(school)) => Some(school),
            Ok(_) => None,
            Err(_) => {
                // a slow Canvas costs the rundown its deadlines, nothing more
                tracing::info!("telegram: canvas for the rundown failed (TimeoutError)");
                Some(canvas_unavailable())
            }
        }
    }
}

/// The rundown's Canvas part when Canvas did not answer.
fn canvas_unavailable() -> Obj {
    let mut m = Obj::new();
    m.insert("available".into(), Value::Bool(false));
    m.insert("reason".into(), json!("Canvas did not answer in time"));
    m
}

/// `_attached(inbound)`, named apart from this file's `attached` flag.
fn attached_line(inbound: &Inbound) -> String {
    crate::telegram::accept::attached(inbound)
}

/// `type(e).__name__` as far as Rust can say it: the root cause's type when it is a known one, else "Exception".
pub(crate) fn error_type(e: &anyhow::Error) -> String {
    let root = e.root_cause();
    if root.is::<std::io::Error>() {
        return "OSError".into();
    }
    if root.is::<serde_json::Error>() {
        return "JSONDecodeError".into();
    }
    if root.is::<tokio::time::error::Elapsed>() {
        return "TimeoutError".into();
    }
    if root.is::<BotApiError>() {
        return "BotApiError".into();
    }
    "Exception".into()
}
