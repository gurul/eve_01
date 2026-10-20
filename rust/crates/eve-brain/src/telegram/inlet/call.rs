//! A port of `telegram.py`, lines 6308-6610: /america, /meet, a call's reader, hearing and spoken answers.
//!
//! Also the two doors' views of the inlet: Eve Link's [`eve_link::seams::Inlet`] (with the chief as link_chat reads
//! it, [`LinkChief`]) and a call's [`eve_phone_call::Brain`].

use std::any::Any;
use std::panic::AssertUnwindSafe;

use async_trait::async_trait;

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

use super::tools::{itl_lower, itl_o, itl_py_str, itl_rstrip_chars, itl_strip, itl_truthy};

/// `answer_context is pending`: the same question (or none at all) by identity.
fn same_context(context: &eve_phone_call::AnswerContext, pending: &Option<Arc<_Question>>) -> bool {
    match (context, pending) {
        (None, None) => true,
        (Some(a), Some(q)) => std::ptr::eq(Arc::as_ptr(a) as *const (), Arc::as_ptr(q) as *const ()),
        _ => false,
    }
}

/// `_america_command`'s `finally:`, run however the job ends (a cancelled job is dropped mid-await).
struct AmericaDone {
    inlet: Arc<TelegramInlet>,
    chat: i64,
    asked: Arc<Mutex<Option<Arc<_Question>>>>,
}

impl Drop for AmericaDone {
    fn drop(&mut self) {
        let asked = self.asked.lock().clone();
        let mut st = self.inlet.st.lock();
        let held = st.america_questions.get(&self.chat).cloned();
        let same = match (&held, &asked) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        };
        if same {
            st.america_questions.remove(&self.chat);
        }
        if st.america_jobs.get(&self.chat).is_some_and(Job::is_current) {
            st.america_jobs.remove(&self.chat);
            st.america_phases.remove(&self.chat);
        }
    }
}

impl TelegramInlet {
    /// The inlet's own `Arc`, for a trait method that takes `&self`.
    fn itl_me(&self) -> Option<Arc<TelegramInlet>> {
        self.me.upgrade()
    }

    /// py: telegram.py:6308 `def _cancel_america_command(self, chat_id: int) -> bool:`
    pub(crate) fn cancel_america_command(self: &Arc<Self>, chat_id: i64) -> bool {
        let (job, asked) = {
            let mut st = self.st.lock();
            let job = st.america_jobs.remove(&chat_id);
            let asked = st.america_questions.remove(&chat_id);
            st.america_phases.remove(&chat_id);
            (job, asked)
        };
        if let Some(asked) = asked {
            if !asked.future.done() {
                asked.future.cancel();
                self.show_question();
            }
        }
        if let Some(job) = job {
            if !job.done() {
                job.cancel();
                return true;
            }
        }
        false
    }

    /// `self._spawn(self._say(chat, text), name)`.
    fn itl_spawn_say(self: &Arc<Self>, chat: i64, text: String, name: &str) {
        let me = self.clone();
        self.spawn(async move { me.say(chat, &text, None, None, &[], 0, None).await }, name);
    }

    /// py: telegram.py:6320 `def _start_america_command(self, inbound: Inbound, question: str) -> None:`
    ///
    /// A menu command owns its question, even while a relay or another prompt is active.
    pub(crate) fn start_america_command(self: &Arc<Self>, inbound: &Inbound, question: &str) {
        let chat = inbound.chat_id;
        if !self.config.owner_ids.contains(&chat) || !self.config.owner_ids.contains(&inbound.user_id) {
            self.itl_spawn_say(chat, "Only a configured owner can use /america.".into(), "telegram-america-denied");
            return;
        }
        if self.america.is_none() {
            self.itl_spawn_say(chat, format!("{}\n{}", eve_america::OFF_REASON, eve_america::AMERICA_URL), "telegram-america-off");
            return;
        }
        if inbound.image.is_some() {
            self.itl_spawn_say(
                chat,
                "Please send the /america question in text without attachments.".into(),
                "telegram-america-image",
            );
            return;
        }
        self.st.lock().chat_id = Some(chat);
        self.note("user", &inbound.text, "command", None, None, None, false, None);
        if AMERICA_CANCEL.contains(&itl_rstrip_chars(&itl_lower(question), ".! ")) {
            let cancelled = self.cancel_america_command(chat);
            let line = if cancelled { "Cancelled the /america question." } else { "No /america question is waiting." };
            self.itl_spawn_say(chat, line.into(), "telegram-america-cancel");
            return;
        }
        let (job, phase) = {
            let st = self.st.lock();
            (st.america_jobs.get(&chat).cloned(), st.america_phases.get(&chat).cloned())
        };
        if let Some(job) = job {
            if !job.done() {
                if phase.as_deref() == Some("running") {
                    self.itl_spawn_say(
                        chat,
                        "Your /america question is already running. Use /america cancel to stop waiting.".into(),
                        "telegram-america-busy",
                    );
                    return;
                }
                self.cancel_america_command(chat);
            }
        }
        self.st.lock().america_phases.insert(chat, "queued".into());
        let (me, inbound, question) = (self.clone(), inbound.clone(), question.to_string());
        let job = self.spawn(async move { me.america_command(&inbound, &question).await }, "telegram-america");
        self.st.lock().america_jobs.insert(chat, job);
    }

    /// py: telegram.py:6350 `def _answer_america_question(self, inbound: Inbound) -> bool:`
    pub(crate) fn answer_america_question(self: &Arc<Self>, inbound: &Inbound) -> bool {
        let (asked, latest) = {
            let st = self.st.lock();
            let asked = st.america_questions.get(&inbound.chat_id).cloned();
            let latest = st
                .questions
                .iter()
                .rev()
                .find(|q| q.chat_id == Some(inbound.chat_id) && !q.future.done())
                .cloned();
            (asked, latest)
        };
        let Some(asked) = asked else {
            return false;
        };
        if asked.future.done() || !self.config.owner_ids.contains(&inbound.user_id) {
            return false;
        }
        if !latest.is_some_and(|q| Arc::ptr_eq(&q, &asked)) {
            return false;
        }
        let word = itl_lower(itl_strip(&inbound.text));
        let word = itl_rstrip_chars(&word, ".! ");
        if AMERICA_CANCEL.contains(&word) || STOP_WORDS.contains(&word) {
            self.cancel_america_command(inbound.chat_id);
            self.itl_spawn_say(inbound.chat_id, "Cancelled the /america question.".into(), "telegram-america-cancel");
            return true;
        }
        if itl_strip(&inbound.text).starts_with('/') {
            return false;
        }
        if inbound.image.is_some() {
            self.itl_spawn_say(
                inbound.chat_id,
                "Please send the /america question in text without attachments.".into(),
                "telegram-america-image",
            );
            return true;
        }
        asked.future.set_result(inbound.text.clone());
        self.note("user", &inbound.text, "say", None, None, None, false, None);
        true
    }

    /// py: telegram.py:6373 `async def _america_command(self, inbound: Inbound, question: str) -> None:`
    pub(crate) async fn america_command(self: &Arc<Self>, inbound: &Inbound, question: &str) {
        let chat = inbound.chat_id;
        let asked: Arc<Mutex<Option<Arc<_Question>>>> = Arc::new(Mutex::new(None));
        let _done = AmericaDone { inlet: self.clone(), chat, asked: asked.clone() };
        let mut question = question.to_string();
        if question.is_empty() {
            self.st.lock().america_phases.insert(chat, "asking".into());
            let (me, slot) = (Arc::downgrade(self), asked.clone());
            let opened: OnOpen = Arc::new(move |prompt: &Arc<_Question>| {
                *slot.lock() = Some(prompt.clone());
                if let Some(me) = me.upgrade() {
                    me.st.lock().america_questions.insert(chat, prompt.clone());
                }
            });
            question = self
                .ask_user(
                    "What U.S. legal or government question should I research? Include the state/city or say U.S. federal. Reply cancel to stop.",
                    chat,
                    "U.S. government sources",
                    Some(&[]),
                    None,
                    false,
                    false,
                    Some(opened),
                    false,
                )
                .await;
            if asked.lock().as_ref().is_some_and(|q| q.future.cancelled()) {
                // _ask_user's timeout text is never submitted as a legal question
                return;
            }
        }
        {
            let held = asked.lock().clone();
            let mut st = self.st.lock();
            let same = match (st.america_questions.get(&chat), &held) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            };
            if same {
                st.america_questions.remove(&chat);
            }
        }
        let said = itl_strip(&question);
        if said.is_empty() || AMERICA_CANCEL.contains(&itl_rstrip_chars(&itl_lower(said), ".! ")) {
            self.say(chat, "Cancelled the /america question.", None, None, &[], 0, None).await;
            return;
        }
        self.st.lock().america_phases.insert(chat, "running".into());
        let mut turn = inbound.clone();
        turn.text = format!("Search official U.S. sources via america_ask: {said}");
        let me = self.clone();
        // boxed: the turn's tools come back to the inlet, and a boxed future keeps the types finite
        let run: futures::future::BoxFuture<'static, ()> = Box::pin(async move { me.turn(&turn, None, &[], false, false).await });
        run.await;
    }

    /// py: telegram.py:6404 `async def _meet_command(self, chat_id: int, rest: str) -> None:`
    ///
    /// /meet by code: bare (or "status") says where eve is, "leave" takes it out, anything else is a link or
    /// the owner's words for a meeting on their calendar.
    pub(crate) async fn meet_command(self: &Arc<Self>, chat_id: i64, rest: &str) {
        let Some(m) = self.meeter.clone() else {
            self.say(chat_id, MEET_OFF_LINE, Some(MEET_TITLE), None, &[], 0, None).await;
            return;
        };
        if rest.is_empty() || itl_lower(rest) == "status" {
            self.say(chat_id, &m.status_line(), Some(MEET_TITLE), None, &[], 0, None).await;
            return;
        }
        if MEET_LEAVE_WORDS.contains(&itl_rstrip_chars(&itl_lower(rest), ".!")) {
            let r = m.leave();
            let r = r.as_object().cloned().unwrap_or_default();
            let line = if itl_truthy(&r, "ok") {
                format!("Leaving {}; the notes follow.", itl_py_str(r.get("leaving")))
            } else {
                "I'm not in a meeting.".to_string()
            };
            self.say(chat_id, &line, Some(MEET_TITLE), None, &[], 0, None).await;
            return;
        }
        let r = match m.join(rest).await {
            Ok(r) => r.as_object().cloned().unwrap_or_default(),
            Err(e) => {
                tracing::error!("telegram: /meet could not join ({e:#})");
                return;
            }
        };
        let ok = itl_truthy(&r, "ok");
        let text = if ok {
            format!(
                "Joining {} with the microphone and camera off. I'll text you when I'm in, and send the notes when it ends.",
                itl_py_str(r.get("joining"))
            )
        } else {
            format!("I can't join that: {}.", itl_py_str(r.get("reason")))
        };
        // the label (and a reason naming it) can be an invite's title: the history keeps code's own line
        let about = if ok { eve_meet::ABOUT_JOINING } else { eve_meet::ABOUT_NOT_JOINING };
        self.note("eve", about, "say", None, None, None, false, None);
        self.say(chat_id, &text, Some(MEET_TITLE), None, &[], 0, None).await;
    }

    /// py: telegram.py:6432 `def chat_ready(self) -> bool:`
    ///
    /// Whether a call has an owner chat to be in, asked without taking the listener (phone_call.Call.run: a
    /// call takes it at its first press). The model's cache is warmed for the first answer either way.
    pub fn chat_ready(self: &Arc<Self>) -> bool {
        if self.st.lock().chat_id.is_none() {
            return false;
        }
        let me = self.clone();
        self.spawn(async move { me.warm().await }, "telegram-call-warm");
        true
    }

    /// py: telegram.py:6440 `def _call_reader(self) -> Optional[Callable[[str], None]]:`
    ///
    /// The call's reader, for work that began on this same call (``_FROM_CALL``) while it still holds the
    /// listener; None for anything else: typed work, and work begun on a call that has since ended. ``==``, not
    /// ``is``, as in ``listen``: each ``call.say`` is a new bound method.
    pub(crate) fn call_reader(self: &Arc<Self>) -> Option<Reader> {
        let began_on = ctx().call;
        let say = self.st.lock().call_say.clone();
        match (began_on, say) {
            (Some(began_on), Some(say)) if say == began_on => Some(say),
            _ => None,
        }
    }

    /// py: telegram.py:6447 `def listen(self, say: Optional[Callable[[str], None]], owner: Optional[Callable[[str], None]] = None) -> bool:`
    ///
    /// A phone call starts (``say``) or ends (None). While it lasts, what eve says in the owner's chat is also
    /// read out on the call. False when there is no owner chat to be in.
    ///
    /// An ending call passes its own reader as ``owner``, and the listener is cleared only if it is still that
    /// one. The phone and the desk share this one listener, and a phone call can start while a desk call is on:
    /// the desk call's end used to clear the phone call's reader, and the rest of that call was texted, not
    /// spoken (verification/Eve/DeskCall.lean). ``==``, not ``is``: each ``call.say`` is a new bound method.
    pub fn listen(self: &Arc<Self>, say: Option<Reader>, owner: Option<&Reader>) -> bool {
        let (warm, ready) = {
            let mut st = self.st.lock();
            if say.is_none() {
                if let Some(owner) = owner {
                    if st.call_say.as_ref() != Some(owner) {
                        return st.chat_id.is_some();
                    }
                }
            }
            let warm = say.is_some() && st.chat_id.is_some();
            st.call_say = say;
            (warm, st.chat_id.is_some())
        };
        if warm {
            // the first answer should not pay a cold cache
            let me = self.clone();
            self.spawn(async move { me.warm().await }, "telegram-call-warm");
        }
        ready
    }

    /// py: telegram.py:6462 `def answer_context(self) -> object | None:`
    ///
    /// Opaque question identity, captured when a push-to-talk recording begins.
    pub fn answer_context(self: &Arc<Self>) -> eve_phone_call::AnswerContext {
        self.live_question().map(|q| q as Arc<dyn Any + Send + Sync>)
    }

    /// py: telegram.py:6466 `def hear(self, text: str, *, answer_context: object = _UNCAPTURED_ANSWER, on_call: bool = True, call: Optional[Callable[[str], None]] = None, app: Optional[str] = None, grade: str = "auto", attachments: Sequence[Any] = ()) -> None:`
    ///
    /// The owner's words from a call, handled exactly as a text from them, so the call can do everything the
    /// chat can. Not echoed into the chat (owner: no text unless it is needed); the transcript keeps them.
    /// What they start is marked as this call's (``_FROM_CALL``), so its answer is read out on this call.
    /// ``call``: the reader of the call that heard the words (phone_call.Call.heard passes its own ``say``). The
    /// desk and the phone share one listener, and a call can hold the button while the other holds the listener:
    /// the words are marked with the call that heard them, not the one holding the listener, and that call takes
    /// the listener back, so its answer is read where it was asked (CallScope.lean, the hearing call). None: the
    /// listener's (a caller that does not say which call it is).
    /// ``on_call=False``: words typed in the Mac chat window (daemon ``_ipc_chat``), which take this path for
    /// its stop and answer handling but are typed, so nothing they start is read aloud (CallScope.lean).
    /// ``app``: the Eve Link app turn these typed words are (link_chat.py, with ``on_call=False``): what they
    /// start is marked as that turn's (``_FROM_APP``), so its lines go to the app and not to Telegram.
    /// ``grade``: the app's speed dial for that turn (GRADES, ``graded``); anything else is "auto".
    /// ``attachments``: the app turn's photos and files (link_uploads.Upload): the words and they go as one
    /// message with an image does in Telegram (``_image``), never as an answer to a waiting question.
    #[allow(clippy::too_many_arguments)]
    pub fn hear(self: &Arc<Self>, text: &str, answer_context: eve_phone_call::Captured, on_call: bool, call: Option<&Reader>, app: Option<&str>, grade: &str, attachments: &[eve_link_uploads::Upload]) {
        let mut reader = None;
        if on_call {
            let mut st = self.st.lock();
            if let Some(call) = call {
                if st.call_say.as_ref() != Some(call) {
                    // the hearing call takes the listener back (no re-warm)
                    st.call_say = Some(call.clone());
                }
            }
            reader = st.call_say.clone();
        }
        let mut marks = ctx();
        marks.call = reader;
        marks.app = app.map(str::to_string);
        marks.app_grade = if GRADES.contains(&grade) { grade.to_string() } else { "auto".to_string() };
        with_ctx(marks, || {
            if !attachments.is_empty() {
                self.hear_attached(text, attachments);
            } else {
                self.do_hear(text, answer_context);
            }
        });
    }

    /// py: telegram.py:6502 `def _hear(self, text: str, answer_context: object) -> None:`
    pub(crate) fn do_hear(self: &Arc<Self>, text: &str, answer_context: eve_phone_call::Captured) {
        let chat = self.st.lock().chat_id;
        let Some(chat) = chat else {
            return;
        };
        let said = itl_strip(text);
        if said.is_empty() {
            return;
        }
        if STOP_WORDS.contains(&itl_rstrip_chars(&itl_lower(said), ".!?")) {
            // Stop withdraws all queued approvals too, rather than just answering
            // the top question with a no. This path also serves the Mac chat IPC.
            let inbound =
                Inbound { chat_id: chat, user_id: chat, text: "stop".into(), message_id: self.app_message_id(), ..Inbound::default() };
            self.handle(&inbound);
            return;
        }
        let pending = self.live_question();
        if let eve_phone_call::Captured::Context(context) = &answer_context {
            if !same_context(context, &pending) {
                self.to_call(chat, "The question changed while you were speaking. Please repeat your answer.", None);
                return;
            }
        }
        // Spoken words take the spoken-answer rules. Text typed in the Eve Link app (``_FROM_APP``) takes the
        // typed rules (``_handle``: _answers_pending, _strict_answer), as Telegram text does: on the spoken path a
        // new request typed while a question waits was dropped, unheard, since an app turn has no call to read
        // "which one?" to (review, 2026-10-08).
        if let Some(pending) = pending {
            if !pending.words.is_empty() && ctx().app.is_none() {
                let (me, said) = (self.clone(), said.to_string());
                self.spawn(async move { me.hear_answer(chat, &said, &pending).await }, "telegram-call-answer");
                return;
            }
        }
        let inbound =
            Inbound { chat_id: chat, user_id: chat, text: said.to_string(), message_id: self.app_message_id(), ..Inbound::default() };
        self.handle(&inbound);
    }

    /// py: telegram.py:6524 `def _app_message_id(self) -> int:`
    ///
    /// The id a reaction lands on for the Eve Link app message being heard (link_reactions.Reactions.mint:
    /// negative, never a Telegram message's); 0 for words that are not an app turn's, as before.
    pub(crate) fn app_message_id(self: &Arc<Self>) -> i64 {
        let Some(turn) = ctx().app else {
            return 0;
        };
        self.app_reactions.mint(&turn).unwrap_or(0)
    }

    /// py: telegram.py:6535 `def _hear_attached(self, text: str, uploads: tuple[Any, ...]) -> None:`
    ///
    /// Words with attachments from the Eve Link app: routed as a Telegram photo with its caption is.
    ///
    /// Rust: the uploads are link_uploads.Upload values, as `hear` takes them (the skeleton had them as JSON).
    pub(crate) fn hear_attached(self: &Arc<Self>, text: &str, uploads: &[eve_link_uploads::Upload]) {
        let chat = self.st.lock().chat_id;
        let Some(chat) = chat else {
            return;
        };
        let size: u64 = uploads.iter().map(|u| u.size).sum();
        let inbound = Inbound {
            chat_id: chat,
            user_id: chat,
            text: itl_strip(text).to_string(),
            image: Some(eve_telegram_images::Attachment::with_size("app", size)),
            message_id: self.app_message_id(),
            tapped: false,
            uploads: uploads.to_vec(),
        };
        self.handle(&inbound);
    }

    /// py: telegram.py:6545 `def _live_question(self) -> Optional[_Question]:`
    pub(crate) fn live_question(self: &Arc<Self>) -> Option<Arc<_Question>> {
        self.st.lock().questions.iter().rev().find(|q| !q.future.done()).cloned()
    }

    /// `asked.future.done() or self._live_question() is not asked or (asked.chat_id is not None and asked.chat_id != chat)`.
    fn itl_answer_gone(self: &Arc<Self>, chat: i64, asked: &Arc<_Question>) -> bool {
        asked.future.done()
            || !self.live_question().is_some_and(|q| Arc::ptr_eq(&q, asked))
            || asked.chat_id.is_some_and(|c| c != chat)
    }

    /// py: telegram.py:6548 `async def _hear_answer(self, chat: int, text: str, asked: _Question) -> None:`
    ///
    /// A spoken answer to a question with choices. The exact choice or a plain yes/no counts at once; a
    /// Claude permission prompt takes nothing else. Other words are read against the question by the model
    /// ("Can you search, bro?" to "press Return?" is a yes); when it cannot tell, eve asks again, and the
    /// words never start a new request while the question waits. A firm question (the chief's Go) is read as a
    /// permission prompt is: a model's reading of the owner's words never approves a one-way act (design
    /// principle 3; reviewer after P4, 2026-09-29: "hmm, I'll think about it after lunch" was judged a yes).
    pub(crate) async fn hear_answer(self: &Arc<Self>, chat: i64, text: &str, asked: &Arc<_Question>) {
        if self.itl_answer_gone(chat, asked) {
            return;
        }
        let mut choices: Vec<String> = asked.words.iter().cloned().collect();
        choices.sort();
        let lowered = itl_lower(text);
        let said = itl_rstrip_chars(itl_strip(&lowered), ".!?");
        let yes_no = asked.words.contains("yes") && asked.words.contains("no");
        let mut pick = if asked.words.contains(said) { Some(said.to_string()) } else { None };
        if pick.is_none() && yes_no {
            pick = match eve_consent::bare_decision(text) {
                "allow" => Some("yes".to_string()),
                "deny" => Some("no".to_string()),
                _ => None,
            };
        }
        let permission = self.st.lock().permission_future.as_ref().is_some_and(|p| p.same(&asked.future));
        let strict = permission || asked.firm;
        if pick.is_none() && !strict {
            let question = asked.text.lock().clone();
            pick = self.judge_answer(&question, &choices, text).await;
        }
        if self.itl_answer_gone(chat, asked) {
            // answered meanwhile (a tap, a timeout)
            return;
        }
        let Some(pick) = pick.filter(|p| asked.words.contains(p)) else {
            let again = if yes_no { CALL_ASK_AGAIN.to_string() } else { format!("Sorry, which one: {}?", choices.join(", ")) };
            self.to_call(chat, &again, None);
            return;
        };
        self.handle(&Inbound { chat_id: chat, user_id: chat, text: pick, ..Inbound::default() });
    }

    /// py: telegram.py:6575 `async def _judge_answer(self, question: str, choices: list[str], said: str) -> Optional[str]:`
    ///
    /// Which choice a spoken answer means, or None when it is not clearly one of them.
    pub(crate) async fn judge_answer(self: &Arc<Self>, question: &str, choices: &[String], said: &str) -> Option<String> {
        let mut options: Vec<Value> = choices.iter().map(|c| json!(c)).collect();
        options.push(json!("unclear"));
        let payload = json!({
            "model": self.config.model, "store": false, "reasoning": {"effort": "low"}, "max_output_tokens": 400,
            "instructions": "The owner was asked a question on a voice call and answered out loud; the answer was transcribed automatically. Say which choice they meant. Pick a choice only when they clearly mean it; for anything unsure, sarcastic, or about something else, pick unclear.",
            "input": format!("Question: {question}\nChoices: {}\nThey said: {said}", choices.join(", ")),
            "text": {"format": {"type": "json_schema", "name": ANSWER_SCHEMA_NAME, "strict": true, "schema": {
                "type": "object", "additionalProperties": false, "required": ["choice"],
                "properties": {"choice": {"type": "string", "enum": options}}}}},
        });
        let judged: anyhow::Result<Option<String>> = async {
            let response = (self.create)(payload).await?;
            eve_spend::record_response(eve_spend::CALLS, &response, &self.config.model, "openrouter", None);
            let parsed = std::panic::catch_unwind(AssertUnwindSafe(|| parse_response(&response, &[], false)))
                .map_err(|_| anyhow::anyhow!("the reply could not be read"))?;
            let out = if parsed.1.is_empty() { "{}".to_string() } else { parsed.1 };
            let data: Value = serde_json::from_str(&out)?;
            Ok(data.get("choice").and_then(Value::as_str).map(str::to_string))
        }
        .await;
        match judged {
            Ok(choice) => choice.filter(|c| choices.contains(c)),
            Err(e) => {
                // not understood is asked again, never taken as an answer
                tracing::info!("telegram: a spoken answer could not be judged ({e})");
                None
            }
        }
    }

    /// py: telegram.py:6598 `def _to_call(self, chat_id: int, text: str, title: Optional[str]) -> None:`
    ///
    /// Read a message out on the phone call, when one is on: the owner's chat only, and not the call's own
    /// echo of what they said or a task's step-by-step progress.
    pub(crate) fn to_call(self: &Arc<Self>, chat_id: i64, text: &str, title: Option<&str>) {
        let Some(say) = self.call_reader() else {
            return;
        };
        if self.st.lock().chat_id != Some(chat_id) || text.is_empty() {
            return;
        }
        if title.is_some_and(|t| itl_lower(t).contains("progress")) {
            return;
        }
        if std::panic::catch_unwind(AssertUnwindSafe(|| say.say(text))).is_err() {
            // a call that cannot read costs the reading, never the message
            tracing::error!("telegram: could not read a message out on the call");
        }
    }
}

/// The chat as Eve Link's door drives it (link.py reads these attributes off the inlet). `self.me` is the inlet's own
/// `Arc`, for the methods that spawn.
impl eve_link::seams::Inlet for TelegramInlet {
    fn chat_ready(&self) -> bool {
        self.st.lock().chat_id.is_some()
    }
    fn hear(&self, text: &str, opts: eve_link::seams::HearOptions) -> anyhow::Result<()> {
        let me = self.itl_me().ok_or_else(|| anyhow::anyhow!("the chat has shut down"))?;
        TelegramInlet::hear(
            &me,
            text,
            eve_phone_call::Captured::Uncaptured,
            opts.on_call,
            None,
            opts.app.as_deref(),
            &opts.grade,
            &opts.attachments,
        );
        Ok(())
    }
    fn owner_busy(&self) -> bool {
        self.itl_me().is_some_and(|me| TelegramInlet::owner_busy(&me))
    }
    fn waiting_on_owner(&self) -> bool {
        self.itl_me().is_some_and(|me| TelegramInlet::waiting_on_owner(&me))
    }
    fn app_filed(&self, turn_id: &str) -> Option<String> {
        self.itl_me().and_then(|me| TelegramInlet::app_filed(&me, turn_id))
    }
    fn set_app_out(&self, out: AppOut) {
        *self.app_out.lock() = Some(out);
    }
    fn set_link_uploads(&self, uploads: Arc<eve_link_uploads::UploadStore>) {
        *self.link_uploads.lock() = Some(uploads);
    }
    fn transcripts(&self) -> Option<Arc<eve_transcripts::Transcripts>> {
        // getattr(self._memory, "transcripts", None) if self._memory is not None else None
        self.memory.as_ref().map(|m| m.transcripts())
    }
    fn app_reactions(&self) -> Option<Arc<eve_link_reactions::Reactions>> {
        Some(self.app_reactions.clone())
    }
    fn chief(&self) -> Option<Arc<dyn eve_link::seams::Chief>> {
        let chief = self.chief.lock().clone()?;
        Some(Arc::new(LinkChief(chief)))
    }
    fn watches(&self) -> Option<Vec<eve_web::watch::Watch>> {
        self.watcher.as_ref().map(|w| w.snapshot())
    }
    fn direct_apps(&self) -> Option<Arc<dyn eve_link::seams::DirectApps>> {
        self.direct.clone().map(|d| d as Arc<dyn eve_link::seams::DirectApps>)
    }
    fn vault_root(&self) -> Option<PathBuf> {
        self.vault.as_ref().map(|v| v.root.clone())
    }
    fn composio(&self) -> Option<Arc<dyn eve_link::seams::ComposioRead>> {
        self.apps.clone().map(|a| a as Arc<dyn eve_link::seams::ComposioRead>)
    }
    fn muse(&self) -> bool {
        self.st.lock().muse
    }
    fn set_muse(&self, on: bool, tell: bool) {
        if let Some(me) = self.itl_me() {
            TelegramInlet::set_muse(&me, on, None, tell);
        }
    }
}

/// The chat as a phone or desk call drives it (phone_call.PhoneCalls lends it as the call's brain).
impl eve_phone_call::Brain for TelegramInlet {
    fn hear(&self, text: &str, call: Option<&Reader>, answer_context: eve_phone_call::Captured) {
        if let Some(me) = self.itl_me() {
            TelegramInlet::hear(&me, text, answer_context, true, call, None, "auto", &[]);
        }
    }
    fn listen(&self, say: Option<Reader>, owner: Option<&Reader>) -> bool {
        match self.itl_me() {
            Some(me) => TelegramInlet::listen(&me, say, owner),
            None => false,
        }
    }
    fn chat_ready(&self) -> Option<bool> {
        Some(self.itl_me().is_some_and(|me| TelegramInlet::chat_ready(&me)))
    }
    fn answer_context(&self) -> Option<eve_phone_call::AnswerContext> {
        Some(self.itl_me().and_then(|me| TelegramInlet::answer_context(&me)))
    }
    fn takes_call(&self) -> bool {
        true
    }
}

// ---- the chief as link_chat reads it (chief.Chief: its ledger and its own methods) ----------------------------------

/// `inlet.chief` for Eve Link's Jobs view and its code words: the chief's ledger as [`eve_link::seams::JobCard`]s,
/// with what the chief says of each (`_as_data`, `_doubtful`), and its own methods.
pub struct LinkChief(pub Arc<eve_chief::Chief>);

/// `e.get("e") == kind`.
fn is_event(e: &Obj, kind: &str) -> bool {
    e.get("e").and_then(Value::as_str) == Some(kind)
}

/// `e.get(key) == n` for an int n.
fn num_is(v: Option<&Value>, n: i64) -> bool {
    match v {
        Some(Value::Number(x)) => x.as_i64().map(|i| i == n).unwrap_or_else(|| x.as_f64() == Some(n as f64)),
        Some(Value::Bool(b)) => i64::from(*b) == n,
        _ => false,
    }
}

impl LinkChief {
    /// chief._router_word: the router's word for a phase's latest dispatch, "" with none.
    fn router_word(events: &[Obj], n: usize) -> String {
        let mut word = String::new();
        for e in events {
            if is_event(e, "dispatched") && num_is(e.get("phase"), n as i64) {
                word = super::tools::itl_str_of(e.get("router"));
            }
        }
        word
    }

    /// chief._in_doubt: a phase dispatched with no result since.
    fn in_doubt(events: &[Obj], n: usize) -> bool {
        let mut state = false;
        for e in events {
            if !num_is(e.get("phase"), n as i64) {
                continue;
            }
            if is_event(e, "dispatched") {
                state = true;
            } else if is_event(e, "result") {
                state = false;
            }
        }
        state
    }

    /// chief._as_data(card), in its key order.
    fn as_data(c: &eve_chief_card::Card, events: &[Obj]) -> Obj {
        let phases: Vec<Value> = c
            .phases
            .iter()
            .map(|p| {
                json!({"n": p.n, "do": p.r#do, "goal": p.goal, "door": p.door, "status": p.status,
                       "executor": p.executor, "why": Self::router_word(events, p.n)})
            })
            .collect();
        itl_o(json!({"id": c.id, "kind": c.kind, "title": c.title, "status": c.status, "waiting_for": c.waiting_for,
               "end_state": c.end_state, "deadline": c.deadline,
               "cue": c.cue.as_ref().map(|q| json!({"at": q.at, "next_action": q.next_action})),
               "phases": phases,
               "checks": c.done_checks.iter().map(eve_chief_card::Check::describe).collect::<Vec<_>>(),
               "spent": {"usd": super::tools::itl_round(c.spent.usd, 4),
                         "minutes": super::tools::itl_round(c.spent.wall_s / 60.0, 1)},
               "budget": {"usd": c.budget.usd, "minutes": c.budget.wall_min}}))
    }

    fn view(&self, c: &eve_chief_card::Card) -> eve_link::seams::JobCard {
        let events = self.0.ledger.events(Some(&c.id));
        let doubtful = !c.terminal()
            && c.phases.iter().any(|p| p.r#do == "act" && p.status == "queued" && Self::in_doubt(&events, p.n));
        eve_link::seams::JobCard {
            id: c.id.clone(),
            kind: c.kind.clone(),
            status: c.status.clone(),
            waiting_for: c.waiting_for.clone(),
            terminal: c.terminal(),
            rev: c.rev,
            updated: c.updated,
            phases: c
                .phases
                .iter()
                .map(|p| eve_link::seams::JobPhase {
                    n: p.n as i64,
                    status: p.status.clone(),
                    door: p.door.clone(),
                    approval: p
                        .approval
                        .as_ref()
                        .map(|a| json!({"token": a.token, "rev": a.rev, "at": a.at, "what": a.what})),
                    do_: p.r#do.clone(),
                })
                .collect(),
            cue_at: c.cue.as_ref().and_then(|q| q.at.clone()),
            doubtful,
            data: Self::as_data(c, &events),
        }
    }
}

fn as_obj(v: Value) -> Obj {
    match v {
        Value::Object(m) => m,
        other => {
            let mut m = Obj::new();
            m.insert("result".into(), other);
            m
        }
    }
}

#[async_trait]
impl eve_link::seams::Chief for LinkChief {
    fn cards(&self) -> Vec<eve_link::seams::JobCard> {
        self.0.ledger.all().iter().map(|c| self.view(c)).collect()
    }
    fn card(&self, cid: &str) -> Option<eve_link::seams::JobCard> {
        self.0.ledger.get(cid).map(|c| self.view(&c))
    }
    async fn approve(&self, cid: &str, via: &str, rev: Option<i64>) -> anyhow::Result<Obj> {
        self.0.approve(cid, via, rev, None, None).map(as_obj)
    }
    async fn done(&self, cid: &str) -> anyhow::Result<Obj> {
        self.0.done(cid).await.map(as_obj)
    }
    async fn drop_job(&self, cid: &str) -> anyhow::Result<Obj> {
        eve_chief::Chief::drop(&self.0, cid).map(as_obj)
    }
    async fn happened(&self, cid: &str) -> anyhow::Result<Obj> {
        self.0.happened(cid).await.map(as_obj)
    }
    async fn didnt(&self, cid: &str) -> anyhow::Result<Obj> {
        self.0.didnt(cid).map(as_obj)
    }
    async fn raise_budget(&self, cid: &str) -> anyhow::Result<Obj> {
        self.0.raise_budget(cid).map(as_obj)
    }
}

#[cfg(test)]
mod itl_call_tests {
    use super::*;

    fn question() -> Arc<_Question> {
        Arc::new(_Question {
            future: Pending::new(),
            chat_id: None,
            strict: true,
            words: Default::default(),
            text: Mutex::new(String::new()),
            firm: false,
        })
    }

    #[test]
    fn the_answer_context_is_the_same_question_by_identity() {
        let q = question();
        let other = question();
        let captured: eve_phone_call::AnswerContext = Some(q.clone() as Arc<dyn Any + Send + Sync>);
        assert!(same_context(&captured, &Some(q.clone())));
        assert!(!same_context(&captured, &Some(other))); // an equal-looking question is another one
        assert!(!same_context(&captured, &None));
        assert!(same_context(&None, &None)); // none then, none now
        assert!(!same_context(&None, &Some(q)));
    }

    fn ev(v: Value) -> Obj {
        v.as_object().cloned().unwrap()
    }

    #[test]
    fn the_router_word_is_the_latest_dispatch_and_a_phase_is_in_doubt_until_its_result() {
        let events = vec![
            ev(json!({"e": "dispatched", "phase": 1, "router": "web"})),
            ev(json!({"e": "result", "phase": 1})),
            ev(json!({"e": "dispatched", "phase": 1, "router": "mac"})),
            ev(json!({"e": "dispatched", "phase": 2, "router": null})),
        ];
        assert_eq!(LinkChief::router_word(&events, 1), "mac");
        assert_eq!(LinkChief::router_word(&events, 2), ""); // str(None or "")
        assert_eq!(LinkChief::router_word(&events, 3), "");
        assert!(LinkChief::in_doubt(&events, 1)); // dispatched again after its result
        assert!(LinkChief::in_doubt(&events, 2));
        assert!(!LinkChief::in_doubt(&events[..2], 1));
        assert!(!LinkChief::in_doubt(&events, 3));
        // e.get("phase") == n: 1.0 is 1 to Python, "1" is not
        assert!(LinkChief::in_doubt(&[ev(json!({"e": "dispatched", "phase": 1.0}))], 1));
        assert!(!LinkChief::in_doubt(&[ev(json!({"e": "dispatched", "phase": "1"}))], 1));
    }
}
