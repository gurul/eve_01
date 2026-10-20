//! A port of `telegram.py`, lines 3715-4117: sending, inline buttons, taps, progress messages.
//!
//! Locking: `self.st` (a `parking_lot::Mutex`) and a progress's `st` are never held across an await, never held
//! together, and never held while calling another method of the inlet (each takes `self.st` itself) or settling a
//! [`Pending`] (its done callbacks take `self.st`).

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

use self::say_relay_util::{choice, class_of_bot, on_drop, reason_line};

impl TelegramInlet {
    /// py: telegram.py:3715 `async def _say(self, chat_id: int, text: str, title: Optional[str] = None, subtitle: Optional[str] = None, buttons: Sequence[str] = (), *, reply_to: int = 0, force_reply: Optional[str] = None) -> None:`
    ///
    /// Send one message. ``title`` names the voice or the event when it is not eve's own reply
    /// (telegram_format.compose); ``subtitle`` is an identifier that helps a person, or nothing;
    /// ``buttons`` are tap-to-send replies; ``reply_to`` is the owner's message this answers; ``force_reply``
    /// opens the reply box on it. A message Telegram refuses with a reply link or a reply box goes again
    /// without them: the words always matter more than the link.
    ///
    /// Work an Eve Link app turn began goes to the app instead (``_to_app``), and is not posted here; a free
    /// question (``force_reply``) goes as an "ask", which ends the app's stream for this turn so the owner's next
    /// text from the app can answer it. When the app is not reading, it comes here as before.
    pub(crate) async fn say(self: &Arc<Self>, chat_id: i64, text: &str, title: Option<&str>, subtitle: Option<&str>, buttons: &[String], reply_to: i64, force_reply: Option<&str>) {
        let for_app = match title.filter(|t| !t.is_empty()) {
            Some(t) => format!("**{t}**\n\n{text}"),
            None => text.to_string(),
        };
        if self.to_app(Some(chat_id), &for_app, if force_reply.is_some() { "ask" } else { "say" }) {
            return;
        }
        let mut text = text.to_string();
        if ctx().app.is_some() {
            // An app turn's line nobody in the app is reading: Telegram cannot render OpenUI Lang, so a program
            // in it becomes one plain line pointing at the app (link_chat.strip_openui).
            text = eve_link::link_chat::strip_openui(&text);
        }
        let own_chat = self.st.lock().chat_id == Some(chat_id);
        if self.call_reader().is_some() && own_chat && title.is_none() && buttons.is_empty() && force_reply.is_none() {
            // on a call: said, not texted, save what the owner needs in writing
            let (spoken, written) = split_for_call(&text);
            let streamed = self.st.lock().streamed.take();
            let echo = streamed.map(|s| split_for_call(&s).0 == spoken).unwrap_or(false);
            if !spoken.is_empty() && !echo {
                self.to_call(chat_id, &spoken, None);
            }
            if written.is_empty() {
                return;
            }
            text = written;
        } else {
            self.to_call(chat_id, &text, title);
        }
        // an app message's id (negative) is no Telegram message
        let reply = if reply_to > 0 { reply_to } else { 0 };
        let extra = reply != 0 || force_reply.is_some();
        match self.api.send_message(chat_id, &text, title, subtitle, buttons, reply, force_reply).await {
            Ok(()) => return,
            Err(e) => {
                if !extra || e.code != 400 {
                    // Only Telegram refusing the link or the box (a 400) is worth a plain resend. A 429, a 5xx or a
                    // network failure may come after earlier pieces of a long message went out, and a resend
                    // would send them twice (review, 2026-09-23).
                    tracing::warn!("telegram: could not send ({e})");
                    return;
                }
                tracing::warn!("telegram: could not send with a reply link or reply box ({e}); sending it plain");
            }
        }
        Box::pin(self.say(chat_id, &text, title, subtitle, buttons, 0, None)).await;
    }

    /// py: telegram.py:3768 `async def _screen_now(self, chat_id: int) -> None:`
    pub(crate) async fn screen_now(self: &Arc<Self>, chat_id: i64) {
        let result = self.send_screen(chat_id, "", None).await;
        if !say_relay_util::truthy(result.get("ok")) {
            let line = format!("I couldn't grab the screen: {}", reason_line(result.get("reason")));
            self.say(chat_id, &line, None, None, &[], 0, None).await;
        }
    }

    /// py: telegram.py:3774 `def _register(self, board: _Keyboard) -> None:`
    ///
    /// Give each of a keyboard's choices a fresh short key. Old keys are forgotten past MAX_LIVE_BUTTONS,
    /// oldest first: a tap on one of those is "expired", as after a restart.
    pub(crate) fn register(self: &Arc<Self>, board: &Arc<_Keyboard>) {
        let mut keys = Vec::with_capacity(board.choices.len());
        let mut st = self.st.lock();
        for c in &board.choices {
            st.tap_seq += 1;
            let key = format!("{}.{}", self.tap_gen, st.tap_seq);
            keys.push(key.clone());
            st.taps.insert(key, (board.clone(), c.clone()));
        }
        // Python's dict keeps insertion order and every key is new, so the oldest is the lowest sequence number.
        while st.taps.len() > MAX_LIVE_BUTTONS {
            let oldest = st.taps.keys().min_by_key(|k| say_relay_util::key_seq(k)).cloned();
            match oldest {
                Some(k) => {
                    st.taps.remove(&k);
                }
                None => break,
            }
        }
        drop(st);
        *board.keys.lock() = keys;
    }

    /// py: telegram.py:3786 `def _retire(self, board: Optional[_Keyboard], *, drop: bool = False) -> None:`
    ///
    /// Forget a keyboard's keys; with ``drop``, take its buttons off the message too (fail-soft).
    pub(crate) fn retire(self: &Arc<Self>, board: Option<&Arc<_Keyboard>>, drop: bool) {
        let Some(board) = board else { return };
        let keys = board.keys.lock().clone();
        {
            let mut st = self.st.lock();
            for key in &keys {
                st.taps.remove(key);
            }
        }
        let message_id = *board.message_id.lock();
        if drop && message_id != 0 {
            if let Some(chat_id) = board.chat_id {
                let me = self.clone();
                self.spawn(async move { me.drop_buttons(chat_id, message_id).await }, "telegram-edit");
            }
        }
    }

    /// py: telegram.py:3795 `async def _send_choices(self, chat_id: Optional[int], text: str, board: _Keyboard, *, title: Optional[str] = None, subtitle: Optional[str] = None, fallback: Sequence[str] = (), per_row: int = 1, read_all: bool = False) -> bool:`
    ///
    /// Send ``text`` with the keyboard's buttons under it → True. When Telegram (or a fake) will not take
    /// an inline keyboard, the keys are forgotten and the message goes as it did before buttons: plain,
    /// or with ``fallback`` as a reply keyboard. The message is never lost for want of its buttons.
    ///
    /// A question or a picker from work an Eve Link app turn began goes to the app as words with the replies
    /// that answer it (the app has no buttons: the owner types one), and True keeps the question strict, as a
    /// sent keyboard does. A question goes as an "ask", which ends the app's stream for the turn. A progress
    /// card (Stop) and the relay's choices stay in Telegram, where they are edited and tapped.
    pub(crate) async fn send_choices(self: &Arc<Self>, chat_id: Option<i64>, text: &str, board: &Arc<_Keyboard>, title: Option<&str>, subtitle: Option<&str>, fallback: &[String], per_row: i64, read_all: bool) -> bool {
        let Some(chat_id) = chat_id else { return false };
        if (board.kind == ANSWER || board.kind == SAY) && !board.choices.is_empty() {
            let head = title.filter(|t| !t.is_empty()).map(|t| format!("**{t}**\n\n")).unwrap_or_default();
            let line = format!("{head}{text}\n\n{}", reply_words(&board.choices));
            if self.to_app(Some(chat_id), &line, if board.kind == ANSWER { "ask" } else { "say" }) {
                return true;
            }
        }
        let own_chat = self.st.lock().chat_id == Some(chat_id);
        if self.call_reader().is_some() && own_chat && !board.choices.is_empty() {
            let words = spoken_question(text, &board.choices);
            // Purchase details must survive both the call's 1500-character message cap and its
            // 880-character sentence cap. Queue every part, including the final yes/no instruction.
            let pieces = if read_all { say_relay_util::py_textwrap(&words, 400) } else { vec![words] };
            for piece in pieces {
                self.to_call(chat_id, &piece, title);
            }
        }
        self.register(board);
        let keys = board.keys.lock().clone();
        let per_row = per_row.max(1) as usize;
        let mut rows: Vec<Vec<(String, String, String)>> = Vec::new();
        for (i, (c, key)) in board.choices.iter().zip(keys.iter()).enumerate() {
            if i % per_row == 0 {
                rows.push(Vec::new());
            }
            if let Some(row) = rows.last_mut() {
                row.push((c.label.clone(), key.clone(), c.style.clone()));
            }
        }
        // A send cut off (the asker cancelled) forgets the keys, as Python's `except CancelledError`.
        let cut = {
            let me = self.clone();
            let board = board.clone();
            on_drop(move || me.retire(Some(&board), false))
        };
        let sent = self.api.send_inline(chat_id, text, &rows, title, subtitle).await;
        cut.disarm();
        match sent {
            Ok(id) => {
                *board.message_id.lock() = id;
                true
            }
            Err(e) => {
                self.retire(Some(board), false);
                // no buttons is today's message, never no message
                tracing::warn!("telegram: inline buttons not sent ({}); sent as a plain message", class_of_bot(&e));
                self.say(chat_id, text, title, subtitle, fallback, 0, None).await;
                false
            }
        }
    }

    /// py: telegram.py:3837 `async def _offer_picker(self, chat_id: int, text: str, choices: Sequence[Choice], *, title: Optional[str] = None, subtitle: Optional[str] = None, valid: Callable[[], bool] = lambda: True, fallback: Sequence[str] = ()) -> None:`
    ///
    /// A picker as inline buttons whose tap sends the button's words (SAY). Only the latest picker is
    /// live: a new one retires the last, whose buttons would now answer the wrong question.
    pub(crate) async fn offer_picker(self: &Arc<Self>, chat_id: i64, text: &str, choices: &[Choice], title: Option<&str>, subtitle: Option<&str>, valid: Option<BoolFn>, fallback: &[String]) {
        let last = self.st.lock().picker_board.clone();
        self.retire(last.as_ref(), true);
        let board = Arc::new(_Keyboard {
            chat_id: Some(chat_id),
            kind: SAY.into(),
            choices: choices.iter().take(MAX_INLINE_BUTTONS).cloned().collect(),
            future: None,
            valid,
            on_stop: None,
            keys: Mutex::new(Vec::new()),
            message_id: Mutex::new(0),
        });
        self.st.lock().picker_board = Some(board.clone());
        self.send_choices(Some(chat_id), text, &board, title, subtitle, fallback, 1, false).await;
    }

    /// py: telegram.py:3847 `def _on_tap(self, tap: Tap) -> None:`
    ///
    /// One tap from the owner, decided here, synchronously, so two quick taps cannot both act. Every tap
    /// is answered (answerCallbackQuery), whatever it did. A key this run did not make, a keyboard already
    /// used, one for another chat or one whose moment has passed is answered and does nothing.
    pub(crate) fn on_tap(self: &Arc<Self>, tap: &Tap) {
        let entry = self.st.lock().taps.get(&tap.data).cloned();
        let stale = match &entry {
            None => true,
            Some((board, _)) => {
                let message_id = *board.message_id.lock();
                board.chat_id != Some(tap.chat_id) || (message_id != 0 && tap.message_id != 0 && message_id != tap.message_id)
            }
        };
        let Some((board, picked)) = entry.filter(|_| !stale) else {
            self.spawn_answer_tap(tap, TAP_EXPIRED_LINE, true);
            return;
        };
        self.retire(Some(&board), false);
        if !board.valid.as_ref().map(|valid| valid()).unwrap_or(true) {
            self.spawn_answer_tap(tap, TAP_EXPIRED_LINE, true);
            return;
        }
        tracing::info!("telegram: a {} button was tapped", board.kind); // the kind, never the words
        if board.kind == ANSWER {
            let future = match &board.future {
                Some(f) if !f.done() => f.clone(),
                _ => {
                    self.spawn_answer_tap(tap, TAP_ANSWERED_LINE, true);
                    return;
                }
            };
            // Exactly what a typed answer does (_handle): the waiting question gets these words. The prompt's
            // own code then edits the message to say what was decided.
            let permission = self.st.lock().permission_future.as_ref().is_some_and(|p| p.same(&future));
            future.set_result(picked.value.clone());
            self.note("user", &picked.value, if permission { "relay" } else { "say" }, None, None, None, false, None);
            let toast = if picked.done.is_empty() { picked.label.clone() } else { picked.done.clone() };
            self.spawn_answer_tap(tap, &toast, false);
            return;
        }
        let toast = if board.kind == TYPE { format!("Typed {}", picked.value) } else { picked.label.clone() };
        self.spawn_answer_tap(tap, &toast, true);
        if board.kind == TYPE {
            // as a typed reply is: the chat's record keeps the choice
            self.note("user", &picked.label, "relay", None, None, None, false, None);
            let me = self.clone();
            let (chat_id, value) = (tap.chat_id, picked.value.clone());
            self.spawn(async move {
                me.type_to_claude(chat_id, &value, 0, true).await;
            }, "telegram-claude");
        } else if board.kind == STOP {
            if let Some(on_stop) = &board.on_stop {
                on_stop(tap.chat_id);
            }
        } else {
            self.handle(&Inbound {
                chat_id: tap.chat_id,
                user_id: tap.user_id,
                text: picked.value.clone(),
                tapped: true,
                ..Inbound::default()
            });
        }
    }

    /// `self._spawn(self._answer_tap(tap, text, drop=drop), "telegram-tap")`.
    fn spawn_answer_tap(self: &Arc<Self>, tap: &Tap, text: &str, drop: bool) {
        let me = self.clone();
        let (tap, text) = (tap.clone(), text.to_string());
        self.spawn(async move { me.answer_tap(&tap, &text, drop).await }, "telegram-tap");
    }

    /// py: telegram.py:3885 `async def _answer_tap(self, tap: Tap, text: str, *, drop: bool = False) -> None:`
    pub(crate) async fn answer_tap(self: &Arc<Self>, tap: &Tap, text: &str, drop: bool) {
        if let Err(e) = self.api.answer_callback(&tap.query_id, text).await {
            // the spinner times out on its own; the tap already acted
            tracing::warn!("telegram: could not answer a tap ({})", class_of_bot(&e));
        }
        if drop && tap.message_id != 0 {
            self.drop_buttons(tap.chat_id, tap.message_id).await;
        }
    }

    /// py: telegram.py:3895 `async def _drop_buttons(self, chat_id: int, message_id: int) -> None:`
    pub(crate) async fn drop_buttons(self: &Arc<Self>, chat_id: i64, message_id: i64) {
        if self.api.drop_buttons(chat_id, message_id).await.is_err() {
            // buttons left on a message only answer "expired"
            tracing::debug!("telegram: could not take buttons off a message (BotApiError)");
        }
    }

    /// py: telegram.py:3903 `async def _settle_prompt(self, board: _Keyboard, body: str, line: str, title: Optional[str], subtitle: Optional[str]) -> None:`
    ///
    /// A decided prompt, rewritten to say what was decided (``body`` then ``line``), its buttons gone
    /// (fail-soft: the decision stands whether or not the message changes). An edit holds one piece, on the
    /// message the buttons sit on, which is the last piece of a long prompt: rewriting it with the prompt's
    /// start would duplicate that and cut the outcome (review, 2026-09-23). A prompt longer than one piece
    /// loses its buttons instead, and the outcome goes as a short message of its own.
    pub(crate) async fn settle_prompt(self: &Arc<Self>, board: &Arc<_Keyboard>, body: &str, line: &str, title: Option<&str>, subtitle: Option<&str>) {
        let message_id = *board.message_id.lock();
        let Some(chat_id) = board.chat_id.filter(|_| message_id != 0) else { return };
        let text = format!("{body}\n\n{line}");
        let composed = eve_telegram_format::compose(&text, title, subtitle);
        if eve_telegram_format::split(&composed, MAX_MESSAGE_CHARS).len() > 1 {
            self.drop_buttons(chat_id, message_id).await;
            self.say(chat_id, line, None, None, &[], 0, None).await;
            return;
        }
        if self.api.edit_message(chat_id, message_id, &text, title, subtitle, None).await.is_err() {
            tracing::debug!("telegram: could not edit a prompt (BotApiError)");
            self.drop_buttons(chat_id, message_id).await;
        }
    }

    /// py: telegram.py:3925 `def _retire_options(self) -> None:`
    ///
    /// The relayed question's option buttons stop meaning anything once the owner types to Claude, the
    /// turn ends, or the relay moves: a late tap would type a number into whatever comes next.
    pub(crate) fn retire_options(self: &Arc<Self>) {
        let board = self.st.lock().options_board.take();
        self.retire(board.as_ref(), true);
    }

    /// py: telegram.py:3932 `def _open_progress(self, chat_id: int, head: str, on_stop: Callable[[int], None], *, title: Optional[str] = None, subtitle: Optional[str] = None, request_id: int = 0) -> _Progress:`
    ///
    /// Start a progress message for work that is starting now → its handle. It is sent at once (a job)
    /// with a Stop button (a STOP keyboard) whose tap calls ``on_stop``: it stops this work, as texting
    /// "stop" would (a task is cancelled, a Codex turn interrupted), and never some other work that the
    /// typed word would reach first. The button is live only while the work is: after the close, a tap is
    /// "expired".
    pub(crate) fn open_progress(self: &Arc<Self>, chat_id: i64, head: &str, on_stop: OnStop, title: Option<&str>, subtitle: Option<&str>, request_id: i64) -> Arc<_Progress> {
        let progress = Arc::new(_Progress {
            chat_id,
            title: title.map(str::to_string),
            subtitle: subtitle.map(str::to_string),
            request_id,
            st: Mutex::new(ProgressState { head: head.to_string(), edited_at: f64::NEG_INFINITY, ..ProgressState::default() }),
            lock: tokio::sync::Mutex::new(()),
        });
        let weak = Arc::downgrade(&progress);
        let board = Arc::new(_Keyboard {
            chat_id: Some(chat_id),
            kind: STOP.into(),
            choices: vec![choice(STOP_BUTTON, "stop", "danger", "")],
            future: None,
            valid: Some(Arc::new(move || weak.upgrade().is_some_and(|p| !p.st.lock().closed))),
            on_stop: Some(on_stop),
            keys: Mutex::new(Vec::new()),
            message_id: Mutex::new(0),
        });
        progress.st.lock().board = Some(board);
        let me = self.clone();
        let sending = progress.clone();
        self.spawn(async move { me.send_progress(&sending).await }, "telegram-progress");
        progress
    }

    /// py: telegram.py:3946 `async def _send_progress(self, progress: _Progress) -> None:`
    pub(crate) async fn send_progress(self: &Arc<Self>, progress: &Arc<_Progress>) {
        let _held = progress.lock.lock().await;
        let (text, board) = {
            let mut st = progress.st.lock();
            if st.closed {
                return;
            }
            st.taking = st.unsent.len() as i64; // the steps this send shows; later ones stay unsent
            (say_relay_util::progress_text(&st.head, &st.steps), st.board.clone())
        };
        let taking = {
            let progress = progress.clone();
            on_drop(move || progress.st.lock().taking = 0)
        };
        let sent = match &board {
            Some(board) => {
                self.send_choices(Some(progress.chat_id), &text, board, progress.title.as_deref(), progress.subtitle.as_deref(), &[], 1, false)
                    .await
            }
            // Python's send would raise on a missing board; there is always one (open_progress)
            None => false,
        };
        {
            let mut st = progress.st.lock();
            st.shown = text;
            st.edited_at = (self.clock)();
            let n = (st.taking.max(0) as usize).min(st.unsent.len());
            st.unsent.drain(..n);
        }
        drop(taking);
        let message_id = board.as_ref().map(|b| *b.message_id.lock()).unwrap_or(0);
        let mut st = progress.st.lock();
        if sent && message_id != 0 {
            st.message_id = message_id;
        } else {
            st.broken = true; // sent plain, or not at all: nothing to edit later
        }
    }

    /// py: telegram.py:3964 `def _progress_step(self, progress: Optional[_Progress], line: str) -> None:`
    ///
    /// One step of the work (a task's narration, a Codex commentary), whole. It is added to the progress
    /// message by an edit, paced to one per PROGRESS_EDIT_SECS; the same step twice in a row is shown once.
    /// Nothing is cut: a step too long for the message goes as its own message, and near the limit the
    /// oldest steps scroll off, each one the phone never showed sent as its own message first.
    pub(crate) fn progress_step(self: &Arc<Self>, progress: Option<&Arc<_Progress>>, line: &str) {
        let line = eve_envfile::py_strip(line).to_string();
        let Some(progress) = progress else { return };
        let mut alone: Vec<String> = Vec::new(); // steps that go as messages of their own, in order
        let flush_now = {
            let mut st = progress.st.lock();
            if st.closed || line.is_empty() || st.steps.last() == Some(&line) {
                return;
            }
            let (title, subtitle) = (progress.title.as_deref(), progress.subtitle.as_deref());
            if st.broken {
                // edits failed: each step goes as a message anyway
                st.steps.push(line.clone());
                st.unsent.push(line.clone());
            } else if !say_relay_util::fits(&st, title, subtitle, &[line.clone()], true) {
                alone.push(line.clone());
                if st.steps.last().map(String::as_str) == Some(PROGRESS_LONG_STEP_LINE) {
                    // the pointer is already the latest line
                    drop(st);
                    self.say_each(progress, alone);
                    return;
                }
                st.steps.push(PROGRESS_LONG_STEP_LINE.into());
                st.unsent.push(PROGRESS_LONG_STEP_LINE.into());
            } else {
                st.steps.push(line.clone());
                st.unsent.push(line.clone());
            }
            while st.steps.len() > 1 && !say_relay_util::fits(&st, title, subtitle, &[], false) {
                if st.unsent.len() >= st.steps.len() {
                    // never shown: it goes on its own, not lost
                    let gone = st.unsent.remove(0);
                    st.taking = (st.taking - 1).max(0); // it left the in-flight share too
                    if gone != PROGRESS_LONG_STEP_LINE {
                        alone.push(gone);
                    }
                }
                st.steps.remove(0);
            }
            st.flush.as_ref().map(|f| f.done()).unwrap_or(true)
        };
        self.say_each(progress, alone);
        if flush_now {
            let me = self.clone();
            let flushing = progress.clone();
            let job = self.spawn(async move { me.flush_progress(&flushing).await }, "telegram-progress");
            progress.st.lock().flush = Some(job);
        }
    }

    /// Each line as a message of its own under the progress message's title (`self._spawn(self._say(...))`).
    fn say_each(self: &Arc<Self>, progress: &Arc<_Progress>, lines: Vec<String>) {
        for line in lines {
            let me = self.clone();
            let (chat_id, title, subtitle) = (progress.chat_id, progress.title.clone(), progress.subtitle.clone());
            self.spawn(
                async move { me.say(chat_id, &line, title.as_deref(), subtitle.as_deref(), &[], 0, None).await },
                "telegram-progress",
            );
        }
    }

    /// py: telegram.py:3997 `def _progress_fits(progress: _Progress, extra: Sequence[str] = (), *, alone: bool = False) -> bool:`
    ///
    /// True when the progress message, with ``extra`` steps and its longest closing line, is one piece
    /// under the limit once composed as HTML (escaping makes it longer than the words). With ``alone``,
    /// the steps already there are left out: would ``extra`` fit even after they all scroll off?
    pub(crate) fn progress_fits(progress: &Arc<_Progress>, extra: &[String], alone: bool) -> bool {
        let st = progress.st.lock();
        say_relay_util::fits(&st, progress.title.as_deref(), progress.subtitle.as_deref(), extra, alone)
    }

    /// py: telegram.py:4006 `def _restore_stop(self, progress: Optional[_Progress]) -> None:`
    ///
    /// A Stop tap retires its key before the stop runs. When the stop then fails (Codex: "still starting",
    /// "nothing is running" before the turn has an id) the work goes on, so the button comes back under a
    /// fresh key and the next edit shows it; without this nothing on the message could stop it any more
    /// (review, 2026-09-23).
    pub(crate) fn restore_stop(self: &Arc<Self>, progress: Option<&Arc<_Progress>>) {
        let Some(progress) = progress else { return };
        let board = {
            let st = progress.st.lock();
            if st.closed {
                return;
            }
            match &st.board {
                Some(board) => board.clone(),
                None => return,
            }
        };
        self.register(&board);
        let flush_now = {
            let mut st = progress.st.lock();
            st.shown = String::new(); // the same text, but its button changed: edit it
            st.flush.as_ref().map(|f| f.done()).unwrap_or(true)
        };
        if flush_now {
            let me = self.clone();
            let flushing = progress.clone();
            let job = self.spawn(async move { me.flush_progress(&flushing).await }, "telegram-progress");
            progress.st.lock().flush = Some(job);
        }
    }

    /// py: telegram.py:4018 `def _stop_rows(self, progress: _Progress) -> Optional[list[list[tuple[str, str, str]]]]:`
    ///
    /// The Stop button's row while its key is live, else None (a tapped or retired button stays gone).
    pub(crate) fn stop_rows(self: &Arc<Self>, progress: &Arc<_Progress>) -> Option<Vec<Vec<(String, String, String)>>> {
        let board = progress.st.lock().board.clone()?;
        let keys = board.keys.lock().clone();
        if keys.is_empty() {
            return None;
        }
        {
            let st = self.st.lock();
            if !keys.iter().all(|k| st.taps.contains_key(k)) {
                return None;
            }
        }
        Some(vec![board.choices.iter().zip(keys).map(|(c, k)| (c.label.clone(), k, c.style.clone())).collect()])
    }

    /// py: telegram.py:4025 `async def _flush_progress(self, progress: _Progress) -> None:`
    ///
    /// Put the latest steps on the phone, no sooner than PROGRESS_EDIT_SECS after the last change, and
    /// again while steps keep coming. An edit that would change nothing is skipped.
    pub(crate) async fn flush_progress(self: &Arc<Self>, progress: &Arc<_Progress>) {
        loop {
            let seen = {
                let st = progress.st.lock();
                if st.closed {
                    return;
                }
                st.edited_at
            };
            let wait = seen + PROGRESS_EDIT_SECS - (self.clock)();
            if wait > 0.0 {
                (self.sleep)(wait).await;
            }
            let _held = progress.lock.lock().await;
            let text = {
                let mut st = progress.st.lock();
                if st.closed {
                    return;
                }
                if st.edited_at != seen {
                    // The first send (or another edit) landed while this waited for the lock: its second is
                    // waited out again, not skipped (review, 2026-09-23: an edit followed the send at once).
                    continue;
                }
                let text = say_relay_util::progress_text(&st.head, &st.steps);
                if text == st.shown && st.unsent.is_empty() {
                    return;
                }
                st.pushing = true; // from here the steps are ours: the close waits
                st.taking = st.unsent.len() as i64; // the steps this edit shows; later ones stay unsent
                text
            };
            let pushed = {
                let progress = progress.clone();
                on_drop(move || {
                    let mut st = progress.st.lock();
                    st.pushing = false;
                    st.taking = 0;
                })
            };
            self.push_progress(progress, &text).await;
            drop(pushed);
        }
    }

    /// py: telegram.py:4050 `async def _push_progress(self, progress: _Progress, text: str) -> None:`
    ///
    /// Edit ``text`` into the progress message. On success only the steps taken with ``text`` count as
    /// shown: a step that arrives while the edit is in flight stays unsent, so a later failed edit or a
    /// failed close still sends it as a plain message rather than losing it (owner, 2026-09-23).
    pub(crate) async fn push_progress(self: &Arc<Self>, progress: &Arc<_Progress>, text: &str) {
        let (broken, message_id) = {
            let mut st = progress.st.lock();
            st.edited_at = (self.clock)();
            (st.broken, st.message_id)
        };
        if !broken && message_id != 0 {
            let rows = self.stop_rows(progress);
            let (title, subtitle) = (progress.title.as_deref(), progress.subtitle.as_deref());
            let edited = match &rows {
                Some(rows) if !rows.is_empty() => {
                    self.api.edit_message(progress.chat_id, message_id, text, title, subtitle, Some(rows)).await
                }
                _ => self.api.edit_message(progress.chat_id, message_id, text, title, subtitle, None).await,
            };
            let shown = match edited {
                Ok(()) => true,
                Err(e) if e.description.to_lowercase().contains("not modified") => true,
                Err(e) => {
                    // a failed edit is today's behaviour: one message per step
                    tracing::warn!("telegram: could not edit a progress message ({e}); steps go as new messages");
                    progress.st.lock().broken = true;
                    false
                }
            };
            if shown {
                let mut st = progress.st.lock();
                st.shown = text.to_string();
                let n = (st.taking.max(0) as usize).min(st.unsent.len());
                st.unsent.drain(..n);
                return;
            }
        }
        progress.st.lock().shown = text.to_string();
        self.say_unsent(progress).await;
    }

    /// py: telegram.py:4080 `async def _say_unsent(self, progress: _Progress) -> None:`
    ///
    /// The steps no edit has shown, as one plain message: today's behaviour when editing does not work.
    /// The long-step pointer is left out, since the step it points to already went on its own.
    pub(crate) async fn say_unsent(self: &Arc<Self>, progress: &Arc<_Progress>) {
        let lines: Vec<String> = {
            let mut st = progress.st.lock();
            std::mem::take(&mut st.unsent).into_iter().filter(|s| s != PROGRESS_LONG_STEP_LINE).collect()
        };
        if !lines.is_empty() {
            self.say(progress.chat_id, &lines.join("\n"), progress.title.as_deref(), progress.subtitle.as_deref(), &[], 0, None)
                .await;
        }
    }

    /// py: telegram.py:4088 `async def _close_progress(self, progress: Optional[_Progress], line: str) -> None:`
    ///
    /// The work is over: the progress message says ``line`` under its last steps and loses its Stop
    /// button. That closing edit shows every step, so none waits for the paced flush. When there is no
    /// message to edit (the buttons were refused) or the edit fails, the steps not yet shown go as one plain
    /// message before the result: a step is never lost, least of all a Codex commentary (owner, 2026-09-23).
    /// A flush waiting out its second is cancelled; one already sending steps is waited for, not cut off.
    /// Fail-soft throughout.
    pub(crate) async fn close_progress(self: &Arc<Self>, progress: Option<&Arc<_Progress>>, line: &str) {
        let Some(progress) = progress else { return };
        let (board, flush, pushing) = {
            let mut st = progress.st.lock();
            if st.closed {
                return;
            }
            st.closed = true;
            (st.board.clone(), st.flush.clone(), st.pushing)
        };
        self.retire(board.as_ref(), false);
        if let Some(flush) = flush {
            if !flush.done() && !flush.is_current() && !pushing {
                flush.cancel();
            }
        }
        let _held = progress.lock.lock().await;
        let (message_id, text) = {
            let st = progress.st.lock();
            (st.message_id, say_relay_util::progress_text(&st.head, &st.steps))
        };
        if message_id != 0 {
            let text = format!("{text}\n\n{line}");
            let (title, subtitle) = (progress.title.as_deref(), progress.subtitle.as_deref());
            match self.api.edit_message(progress.chat_id, message_id, &text, title, subtitle, None).await {
                Ok(()) => {
                    progress.st.lock().unsent.clear(); // every step is on the phone now
                    return;
                }
                Err(_) => {
                    // the result still goes; a stale Stop only answers "expired"
                    tracing::debug!("telegram: could not close a progress message (BotApiError)");
                    self.drop_buttons(progress.chat_id, message_id).await;
                }
            }
        }
        self.say_unsent(progress).await;
    }
}

/// What this unit's three files share: small Python stand-ins (`textwrap.wrap`, `str(x)`, `" ".join(s.split())`),
/// a `finally` that also runs when the future is dropped (a cancelled task), and the progress message's text.
pub(super) mod say_relay_util {
    use crate::prelude::*;
    use crate::telegram::*;

    /// `Choice(label, value, style, done)`.
    pub(crate) fn choice(label: &str, value: &str, style: &str, done: &str) -> Choice {
        Choice { label: label.into(), value: value.into(), style: style.into(), done: done.into() }
    }

    /// `_Progress.text()` over its fields: the head, then the steps as a list.
    pub(crate) fn progress_text(head: &str, steps: &[String]) -> String {
        if steps.is_empty() {
            return head.to_string();
        }
        let list: Vec<String> = steps.iter().map(|s| format!("- {s}")).collect();
        format!("{head}\n\n{}", list.join("\n"))
    }

    /// `_progress_fits` over a progress message's state.
    pub(crate) fn fits(st: &ProgressState, title: Option<&str>, subtitle: Option<&str>, extra: &[String], alone: bool) -> bool {
        let mut steps: Vec<String> = if alone { Vec::new() } else { st.steps.clone() };
        steps.extend(extra.iter().cloned());
        let text = progress_text(&st.head, &steps);
        let closing = [PROGRESS_DONE_LINE, PROGRESS_STOPPED_LINE, PROGRESS_CLOSED_LINE]
            .into_iter()
            .fold("", |best, c| if c.chars().count() > best.chars().count() { c } else { best });
        let composed = eve_telegram_format::compose(&format!("{text}\n\n{closing}"), title, subtitle);
        composed.chars().count() <= MAX_PROGRESS_CHARS
    }

    /// A tap key's sequence number (`<generation>.<n>`).
    pub(crate) fn key_seq(key: &str) -> i64 {
        key.rsplit('.').next().and_then(|n| n.parse().ok()).unwrap_or(i64::MIN)
    }

    /// Python's truthiness of a JSON value (`result.get("ok")`).
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

    /// `str(result.get("reason"))`: a string as it is, None as "None", anything else as its JSON.
    pub(crate) fn reason_line(v: Option<&Value>) -> String {
        match v {
            None | Some(Value::Null) => "None".into(),
            Some(Value::String(s)) => s.clone(),
            Some(Value::Bool(true)) => "True".into(),
            Some(Value::Bool(false)) => "False".into(),
            Some(other) => other.to_string(),
        }
    }

    /// `e if isinstance(e, BotApiError) else type(e).__name__`: a Bot API error as its own words.
    pub(crate) fn class_of_bot(e: &BotApiError) -> String {
        e.to_string()
    }

    /// `type(e).__name__` for an error a lent callable returned: the class it would be in Python, as far as the
    /// error says (an OS error, a timeout), else `Exception`. Never the message: it may carry the owner's words.
    pub(crate) fn class_of(e: &anyhow::Error) -> &'static str {
        if e.downcast_ref::<std::io::Error>().is_some() {
            "OSError"
        } else if e.downcast_ref::<tokio::time::error::Elapsed>().is_some() {
            "TimeoutError"
        } else {
            "Exception"
        }
    }

    /// `" ".join(text.split())`: whitespace runs as one space, none at the ends.
    pub(crate) fn one_line(text: &str) -> String {
        text.split(eve_envfile::is_py_space).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
    }

    /// A `finally` that also runs when the future holding it is dropped (Python's `CancelledError` at an await).
    pub(crate) struct OnDrop<F: FnOnce()>(Option<F>);

    impl<F: FnOnce()> OnDrop<F> {
        /// The work went on to its own end: nothing to undo.
        pub(crate) fn disarm(mut self) {
            self.0 = None;
        }
    }

    impl<F: FnOnce()> Drop for OnDrop<F> {
        fn drop(&mut self) {
            if let Some(f) = self.0.take() {
                f();
            }
        }
    }

    pub(crate) fn on_drop<F: FnOnce()>(f: F) -> OnDrop<F> {
        OnDrop(Some(f))
    }

    /// Python's `textwrap.wrap(text, width)` with its defaults: tabs expanded, whitespace made spaces, words
    /// split after hyphens, long words broken, whitespace dropped at the line ends. Lengths in characters.
    pub(crate) fn py_textwrap(text: &str, width: usize) -> Vec<String> {
        let munged: String = expand_tabs(text)
            .chars()
            .map(|c| if matches!(c, '\t' | '\n' | '\x0b' | '\x0c' | '\r') { ' ' } else { c })
            .collect();
        let mut chunks: Vec<Vec<char>> = split_chunks(&munged).into_iter().map(|c| c.chars().collect()).collect();
        chunks.reverse();
        let is_blank = |c: &[char]| c.iter().all(|ch| eve_envfile::is_py_space(*ch));
        let mut lines: Vec<String> = Vec::new();
        while !chunks.is_empty() {
            let mut cur: Vec<Vec<char>> = Vec::new();
            let mut cur_len = 0usize;
            if chunks.last().is_some_and(|c| is_blank(c)) && !lines.is_empty() {
                chunks.pop();
            }
            while let Some(last) = chunks.last() {
                if cur_len + last.len() <= width {
                    cur_len += last.len();
                    cur.push(chunks.pop().unwrap_or_default());
                } else {
                    break;
                }
            }
            if chunks.last().is_some_and(|c| c.len() > width) {
                // _handle_long_word, break_long_words and break_on_hyphens
                let space_left = if width < 1 { 1 } else { width - cur_len };
                if let Some(chunk) = chunks.last_mut() {
                    let mut end = space_left.min(chunk.len());
                    if chunk.len() > space_left {
                        let hyphen = chunk[..space_left.min(chunk.len())].iter().rposition(|c| *c == '-');
                        if let Some(h) = hyphen {
                            if h > 0 && chunk[..h].iter().any(|c| *c != '-') {
                                end = h + 1;
                            }
                        }
                    }
                    cur.push(chunk[..end].to_vec());
                    chunk.drain(..end);
                }
            }
            if cur.last().is_some_and(|c| is_blank(c)) {
                cur.pop();
            }
            if !cur.is_empty() {
                lines.push(cur.into_iter().flatten().collect());
            }
        }
        lines
    }

    /// `str.expandtabs(8)`.
    fn expand_tabs(text: &str) -> String {
        let mut out = String::new();
        let mut col = 0usize;
        for c in text.chars() {
            match c {
                '\t' => {
                    let n = 8 - col % 8;
                    out.extend(std::iter::repeat_n(' ', n));
                    col += n;
                }
                '\n' | '\r' => {
                    out.push(c);
                    col = 0;
                }
                _ => {
                    out.push(c);
                    col += 1;
                }
            }
        }
        out
    }

    /// `TextWrapper.wordsep_re` (break_on_hyphens): whitespace runs, em-dashes between words, and words split
    /// after their hyphens.
    static WORDSEP: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
        let ws = r"[\t\n\x0b\x0c\r ]";
        let nws = r"[^\t\n\x0b\x0c\r ]";
        let wp = r#"[\w!"'&.,?]"#;
        let lt = r"[^\d\W]";
        let pattern = format!(
            r"({ws}+|(?<={wp})-{{2,}}(?=\w)|{nws}+?(?:-(?:(?<={lt}{{2}}-)|(?<={lt}-{lt}-))(?={lt}-?{lt})|(?={ws}|\z)|(?<={wp})(?=-{{2,}}\w)))"
        );
        fancy_regex::Regex::new(&pattern).expect("textwrap wordsep")
    });

    /// `[c for c in wordsep_re.split(text) if c]`.
    fn split_chunks(text: &str) -> Vec<String> {
        let mut chunks = Vec::new();
        let mut last = 0usize;
        for m in WORDSEP.find_iter(text) {
            let Ok(m) = m else { break };
            if m.start() > last {
                chunks.push(text[last..m.start()].to_string());
            }
            if !m.as_str().is_empty() {
                chunks.push(m.as_str().to_string());
            }
            last = m.end();
        }
        if last < text.len() {
            chunks.push(text[last..].to_string());
        }
        chunks
    }
}
