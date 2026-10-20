//! A port of `telegram.py`, lines 4118-4773: the Codex chat, new coding sessions, the Claude relay, stop.
//!
//! Locking as in `say.rs`: `self.st` is never held across an await, while settling a future, or while calling
//! another method of the inlet.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

use super::say::say_relay_util::{choice, class_of, on_drop, one_line, truthy};
use self::relay_util::{caught_codex, chief_error_name, path_name, realpath};

impl TelegramInlet {
    /// py: telegram.py:4118 `async def _codex_command(self, chat_id: int, action: str, selection: Optional[str], epoch: int) -> None:`
    pub(crate) async fn codex_command(self: &Arc<Self>, chat_id: i64, action: &str, selection: Option<&str>, epoch: i64) {
        let _held = self.codex_lock.lock().await;
        if epoch != self.st.lock().codex_epoch {
            return;
        }
        if action == "off" || action == "disconnect" {
            self.end_codex_progress(PROGRESS_CLOSED_LINE).await;
            self.codex.close().await;
            if action == "off" {
                self.say(chat_id, "Codex chat closed. Back to Eve.", None, None, &[], 0, None).await;
            }
            return;
        }
        if action == "status" {
            let said = if self.codex.connected() {
                format!("Codex chat in {}", self.st.lock().codex_title)
            } else {
                "No Codex chat is connected. Send codex <folder> to start one.".to_string()
            };
            self.say(chat_id, &said, None, None, &[], 0, None).await;
            return;
        }
        let lister = self.codex_folders.clone();
        let folders = match tokio::task::spawn_blocking(move || lister()).await {
            Ok(folders) => folders,
            Err(e) => {
                // the lent lister raised (a panic on its thread): not one of the errors the chat answers
                tracing::error!("telegram: listing the Codex folders failed ({e})");
                return;
            }
        };
        if epoch != self.st.lock().codex_epoch {
            return;
        }
        let Some(selection) = selection.filter(|s| !s.is_empty()) else {
            let menu = eve_codex_chat::folder_menu(&folders);
            if menu.is_empty() {
                self.say(chat_id, "No accessible saved folders.", None, None, &[], 0, None).await;
                return;
            }
            // Each folder a button whose tap sends "codex use <folder>", as typing it would. A name two
            // folders share is listed by its full path, and that path is the button's label too, so the
            // two buttons can be told apart; "use" keeps a folder named "status" or "off" a folder
            // (review, 2026-09-23).
            let choices: Vec<Choice> =
                menu.split('\n').map(|e| choice(&folder_label(e), &format!("codex use {e}"), "", "")).collect();
            self.offer_picker(chat_id, &menu, &choices, None, None, None, &[]).await;
            return;
        };
        let failed = match eve_codex_chat::select_folder(&folders, selection) {
            Err(e) => Some(e),
            Ok(folder) => {
                let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.st.lock().codex_title = name.clone();
                self.end_codex_progress(PROGRESS_CLOSED_LINE).await; // the last chat's turn, if one was open
                let (emit, done, picture) = self.codex_callbacks(chat_id, epoch, &name);
                match self.codex.start(&folder, emit, Some(picture), Some(done)).await {
                    Err(e) => Some(e),
                    Ok(()) => {
                        if epoch != self.st.lock().codex_epoch {
                            self.codex.close().await;
                            return;
                        }
                        let line = format!("New Codex chat in {name}. Send your message.");
                        self.say(chat_id, &line, Some(CODEX_TITLE), None, &[], 0, None).await;
                        None
                    }
                }
            }
        };
        let Some(e) = failed else { return };
        if !caught_codex(e.class_name()) {
            // Python lets it escape the job, which logs it
            tracing::error!("telegram: the Codex command failed ({})", e.class_name());
            return;
        }
        tracing::warn!("telegram: codex startup failed error={}", e.class_name());
        self.codex.close().await;
        {
            let mut st = self.st.lock();
            if epoch == st.codex_epoch {
                st.codex_chat = None;
            }
        }
        let line = if e.class_name() == "CodexUnavailable" {
            e.to_string()
        } else {
            "Could not start Codex. Eve is still available.".to_string()
        };
        self.say(chat_id, &line, None, None, &[], 0, None).await;
    }

    /// The Codex chat's `emit`, `done` and `picture` for a chat started in `chat_id` under `epoch`.
    fn codex_callbacks(self: &Arc<Self>, chat_id: i64, epoch: i64, name: &str) -> (eve_codex_chat::Emit, eve_codex_chat::Emit, eve_codex_chat::Picture) {
        let current = move |me: &Arc<TelegramInlet>| {
            let st = me.st.lock();
            st.codex_chat == Some(chat_id) && epoch == st.codex_epoch
        };
        let (weak, folder) = (Arc::downgrade(self), name.to_string());
        let emit = eve_codex_chat::emit_fn(move |text: String| {
            let (weak, folder) = (weak.clone(), folder.clone());
            async move {
                // A step of the running turn edits its progress message; with none open (a note
                // between turns), it is a message of its own, as every step used to be.
                let Some(me) = weak.upgrade() else { return };
                if !current(&me) {
                    return;
                }
                let progress = me.st.lock().codex_progress.clone();
                match progress {
                    Some(progress) if !progress.st.lock().closed => me.progress_step(Some(&progress), &text),
                    _ => me.say(chat_id, &text, Some(CODEX_TITLE), Some(&folder), &[], 0, None).await,
                }
            }
        });
        let (weak, folder) = (Arc::downgrade(self), name.to_string());
        let done = eve_codex_chat::emit_fn(move |text: String| {
            let (weak, folder) = (weak.clone(), folder.clone());
            async move {
                // The turn's result: the progress message is closed, and the result is a new message
                // (it notifies) replying to the owner's request.
                let Some(me) = weak.upgrade() else { return };
                if !current(&me) {
                    return;
                }
                let progress = me.st.lock().codex_progress.take();
                let stopped = progress.as_ref().is_some_and(|p| p.st.lock().stopped);
                me.close_progress(progress.as_ref(), if stopped { PROGRESS_STOPPED_LINE } else { PROGRESS_DONE_LINE })
                    .await;
                me.event(&format!("Codex answered: {}", eve_chat_log::note_text(&text, eve_chat_log::NOTE_CHARS)), "codex", true);
                let reply = progress.as_ref().map(|p| p.request_id).unwrap_or(0);
                me.say(chat_id, &text, Some(CODEX_TITLE), Some(&folder), &[], reply, None).await;
            }
        });
        let weak = Arc::downgrade(self);
        let picture = eve_codex_chat::picture_fn(move || {
            let weak = weak.clone();
            async move {
                let Some(me) = weak.upgrade() else { return };
                if !current(&me) {
                    return;
                }
                // the chat is this Codex chat's, so the picture is the Codex agent's (`agent=self._codex`)
                let result = me.send_screen(chat_id, "", None).await;
                if !truthy(result.get("ok")) {
                    let line = format!("I couldn't send the browser picture: {}", super::say::say_relay_util::reason_line(result.get("reason")));
                    me.say(chat_id, &line, None, None, &[], 0, None).await;
                }
            }
        });
        (emit, done, picture)
    }

    /// py: telegram.py:4193 `async def _end_codex_progress(self, line: str) -> None:`
    pub(crate) async fn end_codex_progress(self: &Arc<Self>, line: &str) {
        let progress = self.st.lock().codex_progress.take();
        self.close_progress(progress.as_ref(), line).await;
    }

    /// py: telegram.py:4197 `async def _codex_send(self, chat_id: int, text: str, epoch: int, *, interrupt: bool = False, message_id: int = 0) -> bool:`
    ///
    /// Send the owner's message to the Codex chat, or interrupt its turn → True when a message went to
    /// Codex. A message that starts a turn gets the turn's progress message ("Sent to Codex.", then its
    /// steps, with a Stop button); one sent while a turn runs steers it and is confirmed by a 👍 on the
    /// owner's message, or by the line when the reaction fails.
    pub(crate) async fn codex_send(self: &Arc<Self>, chat_id: i64, text: &str, epoch: i64, interrupt: bool, message_id: i64) -> bool {
        let _held = self.codex_lock.lock().await;
        let (current_epoch, codex_chat) = {
            let st = self.st.lock();
            (st.codex_epoch, st.codex_chat)
        };
        if epoch != current_epoch {
            return false;
        }
        if codex_chat != Some(chat_id) || !self.codex.connected() {
            if codex_chat == Some(chat_id) {
                self.st.lock().codex_chat = None;
                self.end_codex_progress(PROGRESS_CLOSED_LINE).await; // its Stop has nothing left to stop
            }
            self.say(chat_id, "Send codex <folder>, for example codex eve, to start a new chat. Eve is still available.", None, None, &[], 0, None)
                .await;
            return false;
        }
        let mut opened: Option<Arc<_Progress>> = None;
        let failed = if interrupt {
            match self.codex.interrupt().await {
                Err(e) => e,
                Ok(()) => {
                    if let Some(progress) = self.st.lock().codex_progress.clone() {
                        // The turn ends as interrupted, and its progress message says "Stopped.", not
                        // "Finished." (owner, 2026-09-23, completeness review of the Telegram batches).
                        progress.st.lock().stopped = true;
                    }
                    self.say(chat_id, "Stop requested in Codex.", None, None, &[], 0, None).await;
                    return false;
                }
            }
        } else {
            let steering = self.codex.running() && self.st.lock().codex_progress.is_some();
            if !steering {
                self.end_codex_progress(PROGRESS_CLOSED_LINE).await;
                // Opened before the send, not after: commentary Codex emits while turn/start is awaited, and
                // a turn that ends inside that await, find this turn's progress message (review, 2026-09-23).
                // Its Stop interrupts this Codex chat, under this epoch: a later chat is never reached.
                let weak = Arc::downgrade(self);
                let on_stop: OnStop = Arc::new(move |cid| {
                    if let Some(me) = weak.upgrade() {
                        me.spawn_codex_interrupt(cid, epoch);
                    }
                });
                let title = self.st.lock().codex_title.clone();
                let progress = self.open_progress(chat_id, CODEX_SENT_LINE, on_stop, Some(CODEX_TITLE), Some(&title), message_id);
                self.st.lock().codex_progress = Some(progress.clone());
                opened = Some(progress);
            }
            match self.codex.send(text).await {
                Err(e) => e,
                Ok(()) => {
                    if steering {
                        // A steer is confirmed by a 👍 on the owner's message (owner, 2026-09-23), not a line.
                        self.receipt(chat_id, message_id, DELIVERED_REACTION, CODEX_SENT_LINE).await;
                        return true;
                    }
                    if message_id != 0 {
                        // The progress message says it was sent; the 👍 says it went in, as in the Claude relay,
                        // and takes an image's 👀 off. No line when it fails: the progress message is that line.
                        self.receipt(chat_id, message_id, DELIVERED_REACTION, "").await;
                    }
                    return true;
                }
            }
        };
        if !caught_codex(failed.class_name()) {
            // Python lets it escape the job, which logs it
            tracing::error!("telegram: the Codex send failed ({})", failed.class_name());
            return false;
        }
        tracing::warn!("telegram: codex send failed error={}", failed.class_name());
        if interrupt {
            let progress = self.st.lock().codex_progress.clone();
            self.restore_stop(progress.as_ref());
        } else if let Some(opened) = opened {
            let ours = {
                let mut st = self.st.lock();
                let ours = st.codex_progress.as_ref().is_some_and(|p| Arc::ptr_eq(p, &opened));
                if ours {
                    st.codex_progress = None;
                }
                ours
            };
            if ours {
                self.close_progress(Some(&opened), CODEX_NOT_SENT_LINE).await;
            }
        }
        if !self.codex.connected() {
            self.st.lock().codex_chat = None;
            self.end_codex_progress(PROGRESS_CLOSED_LINE).await; // a disconnect: no Stop left live
        }
        let line = if failed.class_name() == "CodexUnavailable" {
            failed.to_string()
        } else {
            "Codex did not confirm the message. It was not retried.".to_string()
        };
        self.say(chat_id, &line, None, None, &[], 0, None).await;
        false
    }

    /// `self._spawn(self._codex_send(cid, "", epoch, interrupt=True), "telegram-codex")`.
    pub(crate) fn spawn_codex_interrupt(self: &Arc<Self>, chat_id: i64, epoch: i64) {
        let me = self.clone();
        let fut: futures::future::BoxFuture<'static, ()> = Box::pin(async move {
            me.codex_send(chat_id, "", epoch, true, 0).await;
        });
        self.spawn(fut, "telegram-codex");
    }

    /// py: telegram.py:4258 `def _new_launch(self) -> claude_launch.LaunchFlow:`
    pub(crate) fn new_launch(self: &Arc<Self>) -> eve_claude_launch::LaunchFlow {
        let root = match &self.launch_root {
            Some(root) if !root.as_os_str().is_empty() => root.clone(),
            _ => eve_claude_launch::code_root(&eve_envfile::ProcessEnv),
        };
        let clock = self.clock.clone();
        eve_claude_launch::LaunchFlow::new(root)
            .with_areas(&eve_claude_launch::area_names(&eve_envfile::ProcessEnv))
            .with_recent((self.launch_recent)())
            .with_clock(move || clock())
    }

    /// py: telegram.py:4262 `def _launch_dispatch(self, inbound: Inbound, word: str) -> bool:`
    ///
    /// "new claude" starts the tree; while it is open, the owner's next texts answer it, before the relays
    /// and the model. Each step is decided here, synchronously, so two quick replies cannot interleave.
    pub(crate) fn launch_dispatch(self: &Arc<Self>, inbound: &Inbound, word: &str) -> bool {
        let open = {
            let mut st = self.st.lock();
            if st.launch.as_ref().is_some_and(|l| l.expired()) {
                st.launch = None;
            }
            st.launch.is_some()
        };
        let trigger = eve_claude_launch::trigger_argument(eve_envfile::py_strip(&inbound.text));
        if trigger.is_none() && (!open || (!inbound.tapped && self.answers_pending(Some(&inbound.text)))) {
            return false;
        }
        if trigger.is_none() && STOP_WORDS.contains(&word) {
            self.st.lock().launch = None;
            let (me, chat_id) = (self.clone(), inbound.chat_id);
            self.spawn(async move { me.say(chat_id, "Okay, no new session.", None, None, &[], 0, None).await }, "telegram-say");
            return true;
        }
        let step = match trigger {
            Some(argument) => {
                let mut flow = self.new_launch();
                let step = flow.start(&argument);
                self.st.lock().launch = Some(flow);
                step
            }
            None => {
                let mut st = self.st.lock();
                match st.launch.as_mut() {
                    Some(flow) => flow.answer(&inbound.text),
                    None => return false, // closed meanwhile (Python asserts it is open)
                }
            }
        };
        if step.done {
            self.st.lock().launch = None;
        }
        let (me, chat_id) = (self.clone(), inbound.chat_id);
        self.spawn(async move { me.launch_step(chat_id, &step).await }, "telegram-launch");
        true
    }

    /// py: telegram.py:4286 `async def _launch_step(self, chat_id: int, step: claude_launch.Step) -> None:`
    pub(crate) async fn launch_step(self: &Arc<Self>, chat_id: i64, step: &eve_claude_launch::Step) {
        tracing::info!("telegram: new session step ({})", if step.folder.is_some() { "open" } else { "ask" });
        let title = Some(step.title.as_str()); // `title=step.title`, as Python passes it, even when it is ""
        if !step.buttons.is_empty() {
            // Inline, and live only while this tree is the one open: a tap on an older step's button, or
            // after the tree closed, is "expired" rather than an answer to a question no longer asked.
            let flow = self.st.lock().launch.as_ref().map(|l| l.clock.clone());
            let weak = Arc::downgrade(self);
            let valid: BoolFn = Arc::new(move || {
                let (Some(flow), Some(me)) = (flow.as_ref(), weak.upgrade()) else { return false };
                let st = me.st.lock();
                st.launch.as_ref().is_some_and(|l| relay_util::same_clock(&l.clock, flow) && !l.expired())
            });
            let choices: Vec<Choice> = step.buttons.iter().map(|b| choice(b, b, "", "")).collect();
            self.offer_picker(chat_id, &step.text, &choices, title, None, Some(valid), &step.buttons).await;
        } else {
            self.say(chat_id, &step.text, title, None, &[], 0, None).await;
        }
        let (Some(folder), Some(harness)) = (step.folder.as_ref(), step.harness.as_ref()) else {
            if step.done {
                self.st.lock().join_after_launch = false;
            }
            return;
        };
        let (mut join, epoch) = {
            let mut st = self.st.lock();
            (std::mem::replace(&mut st.join_after_launch, false), st.claude_epoch)
        };
        // Opening the terminal takes seconds: a relay switch meanwhile ("claude off", another "claude on", a Codex
        // folder) moves the Claude epoch, and this late join is dropped rather than turning a second relay on
        // (verification/Eve/RelayJoin.lean).
        let said = match (self.launcher)(folder.clone(), harness.clone()).await {
            Ok(said) => said,
            Err(e) => {
                tracing::warn!("telegram: opening a coding session failed ({})", class_of(&e));
                join = false;
                "I couldn't open a terminal on the Mac.".to_string()
            }
        };
        let name = path_name(&folder.to_string_lossy());
        self.event(
            &format!(
                "You opened a coding session in {}: {}",
                eve_chat_log::note_text(&name, 60),
                eve_chat_log::note_text(&said, eve_chat_log::NOTE_CHARS)
            ),
            "",
            false,
        );
        if join && folder.is_dir() && epoch == self.st.lock().claude_epoch {
            self.relay_to(&realpath(&folder.to_string_lossy()));
            self.st.lock().relay_ready = Some(Arc::new(Flag::new())); // typing waits for its SessionStart
            self.say(chat_id, CLAUDE_ON_LINE, Some(CLAUDE_ON_TITLE), Some(&name), &[], 0, None).await;
            return;
        }
        self.say(chat_id, &said, None, None, &[], 0, None).await;
    }

    /// py: telegram.py:4322 `def _claude_on(self, inbound: Inbound, target: str, *, ask_to_start: bool = False) -> None:`
    ///
    /// "claude on" joins a running session: the only one straight away, a picker when there are several
    /// (each a button), the new-session tree when there are none, then joins what it opens. "claude on
    /// <name>" picks by folder name, and a name no session has starts that folder instead.
    ///
    /// The daemon's list probes the Mac (ps, then lsof: tens of milliseconds, up to 3 s at the timeouts), so it
    /// is awaited as a job, never run on the event loop that serves the hooks, BLE and this poll (review,
    /// 2026-09-23). Messages arriving meanwhile are held and routed after, in order.
    pub(crate) fn claude_on(self: &Arc<Self>, inbound: &Inbound, target: &str, ask_to_start: bool) {
        self.st.lock().join_after_launch = false;
        let listed = match (self.claude_sessions)() {
            Ok(listed) => listed,
            Err(e) => {
                // no list is only a shorter menu
                tracing::warn!("telegram: could not list Claude sessions ({})", class_of(&e));
                Sessions::Now(Vec::new())
            }
        };
        match listed {
            Sessions::Later(listed) => {
                // The job is the slot's before it runs, as in Python, where it cannot start before the assignment.
                let registered = Arc::new(Flag::new());
                let (me, inbound, target, gate) = (self.clone(), inbound.clone(), target.to_string(), registered.clone());
                let job = self.spawn(
                    async move {
                        gate.wait().await;
                        me.claude_on_listed(&inbound, &target, listed, ask_to_start).await;
                    },
                    "telegram-claude-on",
                );
                self.st.lock().claude_on_job = Some(job);
                registered.set();
            }
            Sessions::Now(sessions) => {
                let sessions = Value::Array(sessions.into_iter().map(Value::String).collect());
                self.claude_on_pick(inbound, target, &sessions, ask_to_start);
            }
        }
    }

    /// py: telegram.py:4342 `async def _claude_on_listed(self, inbound: Inbound, target: str, listed: Awaitable[Any], ask_to_start: bool = False) -> None:`
    pub(crate) async fn claude_on_listed(self: &Arc<Self>, inbound: &Inbound, target: &str, listed: futures::future::BoxFuture<'static, anyhow::Result<Vec<String>>>, ask_to_start: bool) {
        {
            // `finally`: the slot is cleared even when this job is cancelled
            let me = self.clone();
            let _slot = on_drop(move || {
                let mut st = me.st.lock();
                if st.claude_on_job.as_ref().is_some_and(|j| j.is_current()) {
                    st.claude_on_job = None;
                }
            });
            let sessions = match listed.await {
                Ok(sessions) => sessions,
                Err(e) => {
                    // no list is only a shorter menu
                    tracing::warn!("telegram: could not list Claude sessions ({})", class_of(&e));
                    Vec::new()
                }
            };
            let sessions = Value::Array(sessions.into_iter().map(Value::String).collect());
            self.claude_on_pick(inbound, target, &sessions, ask_to_start);
        }
        let held = std::mem::take(&mut self.st.lock().held);
        for (message, origin) in held {
            // its own origin, not this task's: "claude on" said on a call must not read a typed message's answer
            // aloud, and "claude on" typed must not silence words said meanwhile (CallScope.lean, current_gaps)
            let mut turn = ctx();
            turn.call = origin;
            with_ctx(turn, || self.handle(&message));
        }
    }

    /// py: telegram.py:4366 `def _claude_on_pick(self, inbound: Inbound, target: &str, sessions: Any, ask_to_start: bool = False) -> None:`
    pub(crate) fn claude_on_pick(self: &Arc<Self>, inbound: &Inbound, target: &str, sessions: &Value, ask_to_start: bool) {
        let mut cwds: Vec<String> = Vec::new();
        for c in sessions.as_array().map(Vec::as_slice).unwrap_or_default() {
            let c = match c {
                Value::String(s) => s.clone(),
                Value::Null | Value::Bool(false) => continue,
                other => other.to_string(),
            };
            if !c.is_empty() && !cwds.contains(&c) {
                cwds.push(c);
            }
        }
        let names: Vec<String> = cwds.iter().map(|c| path_name(c)).collect();
        if !target.is_empty() {
            let hits = eve_claude_launch::match_names(target, &names);
            if let Some(hit) = hits.first() {
                if let Some(i) = names.iter().position(|n| n == hit) {
                    return self.join(inbound.chat_id, &cwds[i]);
                }
            }
            if ask_to_start {
                return self.offer_start(inbound.chat_id, target, &cwds, &names);
            }
            return self.start_then_join(inbound.chat_id, target, "");
        }
        if cwds.len() == 1 {
            return self.join(inbound.chat_id, &cwds[0]);
        }
        if cwds.is_empty() {
            if ask_to_start {
                return self.offer_start(inbound.chat_id, "", &cwds, &names);
            }
            return self.start_then_join(inbound.chat_id, "", CLAUDE_NONE_LINE);
        }
        self.defer_permission();
        self.st.lock().claude = false;
        // The relay is off while the owner picks: nothing ends the last session's "Thinking…" now (its text
        // and Stop hook are no longer relayed), so it ends here (review, 2026-09-23).
        self.stop_thinking(Some(inbound.chat_id));
        self.st.lock().join_after_launch = true; // the picker's "new claude" joins what it opens
        tracing::info!("telegram: claude relay asks which of {} sessions", cwds.len());
        let mut words: Vec<String> = names.iter().map(|n| format!("claude on {n}")).collect();
        words.push(NEW_CLAUDE_BUTTON.to_string());
        let mut labels = names.clone();
        labels.push("New claude".into());
        let choices: Vec<Choice> = labels.iter().zip(&words).map(|(n, w)| choice(n, w, "", "")).collect();
        let (me, chat_id) = (self.clone(), inbound.chat_id);
        self.spawn(
            async move { me.offer_picker(chat_id, CLAUDE_PICK_LINE, &choices, Some(CLAUDE_ON_TITLE), None, None, &words).await },
            "telegram-say",
        );
    }

    /// py: telegram.py:4394 `def _offer_start(self, chat_id: int, target: str, cwds: list[str], names: list[str]) -> None:`
    ///
    /// claude_relay (the brain's tool) named a session that is not running, or none runs: a new session opens
    /// Claude Code with every permission granted (claude_launch), so it is the owner's tap, never the brain's
    /// call (review, 2026-10-08). The picker offers the running sessions and one button to start; a tap sends the
    /// typed command's own words, so the owner's tap runs exactly what typing them would.
    pub(crate) fn offer_start(self: &Arc<Self>, chat_id: i64, target: &str, cwds: &[String], names: &[String]) {
        self.defer_permission();
        self.st.lock().claude = false;
        self.stop_thinking(Some(chat_id));
        let start_label = if target.is_empty() {
            "New claude".to_string()
        } else {
            format!("Start one in {target}").chars().take(40).collect()
        };
        let start_word = if target.is_empty() { NEW_CLAUDE_BUTTON.to_string() } else { format!("claude on {target}") };
        let mut words: Vec<String> = names.iter().map(|n| format!("claude on {n}")).collect();
        words.push(start_word);
        tracing::info!("telegram: claude relay offers to start a session ({} running)", cwds.len());
        let line = if target.is_empty() {
            CLAUDE_NONE_ASK_LINE.to_string()
        } else {
            CLAUDE_START_ASK_LINE.replace("{name}", target)
        };
        let mut labels = names.to_vec();
        labels.push(start_label);
        let choices: Vec<Choice> = labels.iter().zip(&words).map(|(n, w)| choice(n, w, "", "")).collect();
        let me = self.clone();
        self.spawn(
            async move { me.offer_picker(chat_id, &line, &choices, Some(CLAUDE_ON_TITLE), None, None, &words).await },
            "telegram-say",
        );
    }

    /// py: telegram.py:4411 `def _relay_to(self, cwd: str) -> None:`
    pub(crate) fn relay_to(self: &Arc<Self>, cwd: &str) {
        self.retire_options();
        let moved = {
            let st = self.st.lock();
            !st.claude || cwd != st.relay_cwd
        };
        if moved {
            self.defer_permission(); // another session: its prompt is not this one's
            let mut st = self.st.lock();
            st.relay_prompts.clear(); // nor its turns
            st.relay_origin = None;
        }
        {
            let mut st = self.st.lock();
            st.claude = true;
            st.relay_pin = cwd.to_string();
            st.relay_cwd = cwd.to_string();
            st.relay_ready = None;
        }
        tracing::info!("telegram: claude relay on");
    }

    /// py: telegram.py:4421 `def relay_session_started(self, cwd: str) -> None:`
    ///
    /// Claude Code's SessionStart hook: a session in ``cwd`` is up. The one eve just opened and joined can now be
    /// typed into.
    pub fn relay_session_started(self: &Arc<Self>, cwd: &str) {
        let (ready, target) = {
            let st = self.st.lock();
            let target = if st.relay_pin.is_empty() { st.relay_cwd.clone() } else { st.relay_pin.clone() };
            (st.relay_ready.clone(), target)
        };
        if let Some(ready) = ready {
            if !cwd.is_empty() && realpath(cwd) == realpath(&target) {
                ready.set();
            }
        }
    }

    /// py: telegram.py:4428 `async def _await_ready(self) -> None:`
    ///
    /// Wait for a just-opened session to start before typing into it (owner, 2026-10-10: the first message
    /// after a new claude was typed a second before Claude Code was listening, and was lost). A session that never
    /// says so is typed into after CLAUDE_READY_SECS anyway, as before.
    pub(crate) async fn await_ready(self: &Arc<Self>) {
        let Some(ready) = self.st.lock().relay_ready.clone() else { return };
        if !ready.is_set() {
            tracing::info!("telegram: waiting for the new Claude session to start before typing");
            let waited = tokio::time::timeout(std::time::Duration::from_secs_f64(CLAUDE_READY_SECS), ready.wait()).await;
            if waited.is_err() {
                tracing::warn!("telegram: the new Claude session did not report it started; typing anyway");
            }
        }
        let ours = {
            let mut st = self.st.lock();
            let ours = st.relay_ready.as_ref().is_some_and(|r| Arc::ptr_eq(r, &ready));
            if ours {
                st.relay_ready = None;
            }
            ours
        };
        if ours {
            (self.sleep)(CLAUDE_SETTLE_SECS).await;
        }
    }

    /// py: telegram.py:4445 `def _join(self, chat_id: int, cwd: str) -> None:`
    pub(crate) fn join(self: &Arc<Self>, chat_id: i64, cwd: &str) {
        self.relay_to(cwd);
        let (me, name) = (self.clone(), path_name(cwd));
        self.spawn(
            async move { me.say(chat_id, CLAUDE_ON_LINE, Some(CLAUDE_ON_TITLE), Some(&name), &[], 0, None).await },
            "telegram-say",
        );
    }

    /// py: telegram.py:4450 `def _start_then_join(self, chat_id: int, argument: str, intro: str = "") -> None:`
    pub(crate) fn start_then_join(self: &Arc<Self>, chat_id: i64, argument: &str, intro: &str) {
        self.defer_permission();
        self.st.lock().claude = false;
        self.stop_thinking(Some(chat_id)); // as for the picker: no relay left to end it
        let mut flow = self.new_launch();
        let mut step = flow.start(argument);
        {
            let mut st = self.st.lock();
            st.launch = if step.done { None } else { Some(flow) };
            st.join_after_launch = true;
        }
        if !intro.is_empty() {
            step.text = format!("{intro}\n\n{}", step.text);
        }
        let me = self.clone();
        self.spawn(async move { me.launch_step(chat_id, &step).await }, "telegram-launch");
    }

    /// py: telegram.py:4464 `async def _type_to_claude(self, chat_id: int, text: str, message_id: int = 0, *, tapped: bool = False) -> bool:`
    ///
    /// Type one line into the joined session → True when it went in. ``tapped``: an option button's
    /// number, whose tap was already answered, so no reaction or "Typed." follows it.
    pub(crate) async fn type_to_claude(self: &Arc<Self>, chat_id: i64, text: &str, message_id: i64, tapped: bool) -> bool {
        if !tapped {
            self.retire_options(); // typed instead: the option buttons are spent
        }
        self.chief_spoke(text); // a relayed message is the owner speaking too
        if !self.claude() {
            self.say(chat_id, CLAUDE_NOT_ON_LINE, None, None, &[], 0, None).await;
            return false;
        }
        let Some(terminal) = self.terminal.clone().filter(|_| !text.is_empty()) else {
            self.say(chat_id, "I can't reach a terminal on this computer.", None, None, &[], 0, None).await;
            return false;
        };
        self.await_ready().await;
        if !self.claude() {
            // "claude off" while it waited
            return false;
        }
        // Queued before it is typed: Claude Code's UserPromptSubmit hook for it can reach relay_turn_began before
        // the keystrokes' osascript returns. Claude's lines for this prompt are read on the call it was said on, if
        // any (a spoken prompt to Claude is explicit; a typed one is not: owner, 2026-10-04, "dont read aloud
        // things from telegram unless its something explicit from like the action call"; CallScope.lean, the relay).
        let entry = (prompt_key(text), ctx().call);
        let cwd = {
            let mut st = self.st.lock();
            if st.relay_prompts.len() >= RELAY_PROMPTS_MAX {
                st.relay_prompts.pop_front(); // a deque with maxlen: the oldest goes
            }
            st.relay_prompts.push_back(entry.clone());
            if st.relay_pin.is_empty() { st.relay_cwd.clone() } else { st.relay_pin.clone() }
        };
        let said = match terminal(cwd, text.to_string()).await {
            Ok(said) => said,
            Err(e) => {
                tracing::warn!("telegram: typing to the terminal failed ({})", class_of(&e));
                "I couldn't type that into the terminal.".to_string()
            }
        };
        if !said.is_empty() {
            {
                let mut st = self.st.lock();
                if let Some(i) = st.relay_prompts.iter().position(|p| *p == entry) {
                    st.relay_prompts.remove(i); // it never went in
                }
            }
            self.say(chat_id, &said, None, None, &[], 0, None).await;
            return false;
        }
        // Claude is on it: "Thinking…" until it asks, waits on the owner or ends its turn (relay_tool_call,
        // relay_notification, decide_permission, relay_turn_ended), or the cap; with drafts off, "typing…" until
        // it also says something (relay_text). Started before the receipt is awaited: a quick turn's answer or
        // Stop hook landing during that await must find it on and end it, not precede it (review, 2026-09-23).
        self.start_thinking(Some(chat_id));
        if tapped {
            return true;
        }
        // It went in: a reaction on the owner's own message says so (owner, 2026-09-23), not a line.
        self.receipt(chat_id, message_id, TYPED_REACTION, "Typed.").await;
        true
    }

    /// py: telegram.py:4506 `async def _receipt(self, chat_id: int, message_id: int, emoji: str, fallback: str = "") -> bool:`
    ///
    /// A reaction on the owner's own message in place of a one-line acknowledgement → True when it was
    /// set. When it cannot be (no message id to put it on, Telegram refused it, the network failed), the
    /// ``fallback`` line is sent instead, so the owner is never left without the receipt; with no fallback
    /// the failure is only logged (a 👀 is a status light, not something to confirm). Never raises.
    pub(crate) async fn receipt(self: &Arc<Self>, chat_id: i64, message_id: i64, emoji: &str, fallback: &str) -> bool {
        // `_react`, whose failure this needs to see: an app message's reaction in the app's store, any other in
        // Telegram.
        let failed: String = if message_id == 0 {
            BotApiError::new(0, "no message id").to_string()
        } else if eve_link_reactions::Reactions::is_app(message_id) {
            match self.app_reactions.set(message_id, emoji) {
                Ok(()) => String::new(),
                Err(eve_link_reactions::SetError::NotAnEmoji) => "ValueError".into(),
                Err(eve_link_reactions::SetError::NoSuchMessage) => "KeyError".into(),
            }
        } else {
            match self.api.react(chat_id, message_id, emoji).await {
                Ok(()) => String::new(),
                Err(e) => e.to_string(),
            }
        };
        if failed.is_empty() {
            if emoji != SEEN_REACTION {
                self.st.lock().seen_marks.remove(&message_id); // the 👀 was replaced
            }
            return true;
        }
        // a receipt that fails must never cost the message or the loop
        tracing::warn!("telegram: could not react ({failed}){}", if fallback.is_empty() { "" } else { "; said it instead" });
        let seen = emoji != SEEN_REACTION && self.st.lock().seen_marks.remove(&message_id);
        if seen {
            // The 👍 did not replace the 👀: take the 👀 off, so the message does not still say "on its way".
            self.unreact(chat_id, message_id).await;
        }
        if !fallback.is_empty() {
            self.say(chat_id, fallback, None, None, &[], 0, None).await;
        }
        false
    }

    /// py: telegram.py:4531 `def relay_line(self, body: str, title: str = CLAUDE_TITLE, subtitle: str = "", options: Sequence[Choice] = ()) -> None:`
    ///
    /// One message for the phone (what Claude said, a question it asks), batched with its neighbours:
    /// a burst of short messages under the same title is one text, not ten (``_flush_relay``). ``options``
    /// become buttons under it that type their value into the terminal (a question's numbered options).
    pub fn relay_line(self: &Arc<Self>, body: &str, title: &str, subtitle: &str, options: &[Choice]) {
        let flush_now = {
            let mut st = self.st.lock();
            if !st.claude || st.chat_id.is_none() {
                return;
            }
            let body = eve_envfile::py_strip(body);
            if body.is_empty() {
                return;
            }
            // The origin is the turn's now, as the line arrives: by the flush, up to RELAY_BATCH_SECS later, Claude may
            // have begun the next prompt, and that one may have been typed, not said.
            let origin = st.relay_origin.clone();
            st.relay_lines.push((title.to_string(), subtitle.to_string(), body.to_string(), options.to_vec(), origin));
            st.relay_flush.as_ref().map(|f| f.done()).unwrap_or(true)
        };
        if flush_now {
            let me = self.clone();
            let job = self.spawn(async move { me.flush_relay().await }, "telegram-relay");
            self.st.lock().relay_flush = Some(job);
        }
    }

    /// py: telegram.py:4547 `async def _flush_relay(self) -> None:`
    pub(crate) async fn flush_relay(self: &Arc<Self>) {
        (self.sleep)(RELAY_BATCH_SECS).await;
        let entries = std::mem::take(&mut self.st.lock().relay_lines);
        // Neighbours under one title become one message, their bodies as paragraphs; a change of title
        // (Claude said, then Claude asks) starts the next, and so does a change of origin: a batch that straddles
        // two prompts is two messages, each read (or not) as its own prompt's.
        // A message with buttons takes nothing after it, so its buttons stay under the words they answer.
        let mut groups: Vec<(String, String, Vec<String>, Vec<Choice>, Option<Reader>)> = Vec::new();
        for (title, subtitle, body, options, origin) in entries {
            match groups.last_mut() {
                Some(last) if last.0 == title && last.1 == subtitle && last.3.is_empty() && last.4 == origin => {
                    last.2.push(body);
                    last.3 = options;
                }
                _ => groups.push((title, subtitle, vec![body], options, origin)),
            }
        }
        for (title, subtitle, bodies, options, origin) in groups {
            let Some(chat_id) = self.st.lock().chat_id else { continue };
            // This task was spawned from a daemon task, where no call is set: each message is read on the call its
            // prompt was said on, if any. Set in this task's own context only.
            set_ctx(|c| c.call = origin);
            let subtitle = if subtitle.is_empty() { None } else { Some(subtitle.as_str()) };
            let text = bodies.join("\n\n");
            if options.is_empty() {
                self.say(chat_id, &text, Some(&title), subtitle, &[], 0, None).await;
                let chat_now = self.st.lock().chat_id;
                if let Some(chat_now) = chat_now.filter(|_| self.thinking()) {
                    // The message just cleared the "Thinking…" draft, and Claude is still at work: show it
                    // again now, not at the next refresh.
                    let draft_id = self.st.lock().draft_id;
                    self.show_draft(chat_now, draft_id).await;
                }
                continue;
            }
            self.retire_options();
            let weak = Arc::downgrade(self);
            let board = Arc::new(_Keyboard {
                chat_id: Some(chat_id),
                kind: TYPE.into(),
                choices: options,
                future: None,
                valid: Some(Arc::new(move || weak.upgrade().is_some_and(|me| me.claude()))),
                on_stop: None,
                keys: Mutex::new(Vec::new()),
                message_id: Mutex::new(0),
            });
            self.st.lock().options_board = Some(board.clone());
            self.send_choices(Some(chat_id), &text, &board, Some(&title), subtitle, &[], 1, false).await;
        }
    }

    /// py: telegram.py:4581 `def relay_tool_call(self, tool: str, hint: str) -> None:`
    ///
    /// A tool call the daemon saw. Only a question for the owner (AskUserQuestion) reaches the phone:
    /// the terminal's gray lines, the call itself and its result tail, stay on the Mac (owner, 2026-09-21).
    pub fn relay_tool_call(self: &Arc<Self>, tool: &str, hint: &str) {
        if tool != "AskUserQuestion" {
            return;
        }
        let chat_id = self.st.lock().chat_id;
        self.stop_thinking(chat_id);
        let mut body = one_line(hint);
        if body.is_empty() {
            body = "(see the terminal)".into();
        }
        // One question's options become buttons; a tap types the option's number, as the reply does.
        let options: Vec<Choice> =
            question_options(&body).into_iter().map(|(n, label)| choice(&format!("{n}. {label}"), &n.to_string(), "", "")).collect();
        if body.contains("(1. ") {
            body.push_str("\n\nReply with the option's number.");
        }
        self.st.lock().last_ask_at = (self.clock)();
        self.relay_line(&body, CLAUDE_ASKS_TITLE, "", &options);
    }

    /// py: telegram.py:4594 `async def relay_text(self, text: str, cwd: str = "") -> None:`
    ///
    /// What Claude Code just said, when the relay is on, with its paragraphs and lists as it wrote
    /// them (telegram_format turns the markdown into Telegram's own bold, bullets and code). Never
    /// logged here either.
    pub async fn relay_text(self: &Arc<Self>, text: &str, cwd: &str) {
        let (chat_id, pin) = {
            let st = self.st.lock();
            if !st.claude {
                return;
            }
            let Some(chat_id) = st.chat_id else { return };
            (chat_id, st.relay_pin.clone())
        };
        if !cwd.is_empty() && !pin.is_empty() && !same_folder(cwd, &pin) {
            return; // another session: the owner picked this one
        }
        if !cwd.is_empty() {
            self.st.lock().relay_cwd = cwd.to_string();
        }
        let mut body = eve_envfile::py_strip(text).to_string();
        if !body.is_empty() {
            // Claude's next words: with "typing…" they are the sign it answered, so it stops. The "Thinking…"
            // draft stays for the rest of the turn (Claude says a line, then keeps working), and its cap starts
            // again: a turn is capped from Claude's last words, not from the owner's line.
            self.stop_typing(Some(chat_id), "relay");
            if self.thinking() {
                self.st.lock().draft_ticks = ((TYPING_RELAY_SECS / DRAFT_REFRESH_SECS).ceil() as i64).max(1);
            }
        }
        if body.chars().count() > MAX_RELAY_CHARS {
            // Cut on a paragraph, else a line, else a space near the cap, and say the rest is on the Mac.
            let window: String = body.chars().take(MAX_RELAY_CHARS).collect();
            let at_char = |at: usize| window[..at].chars().count();
            let cut = [window.rfind("\n\n"), window.rfind('\n'), window.rfind(' ')]
                .into_iter()
                .flatten()
                .find(|&at| at_char(at) >= MAX_RELAY_CHARS / 2)
                .unwrap_or(window.len());
            body = format!("{}…\n\n{RELAY_CUT_LINE}", window[..cut].trim_end_matches(eve_envfile::is_py_space));
        }
        if !body.is_empty() {
            self.record("claude", "relay", &body, "", None, false, None); // what Claude did is remembered
            self.event(&format!("Claude Code answered: {}", eve_chat_log::note_text(&body, eve_chat_log::NOTE_CHARS)), "claude", true);
            let relay_cwd = self.st.lock().relay_cwd.clone();
            let subtitle = if relay_cwd.is_empty() { String::new() } else { path_name(&relay_cwd) };
            self.relay_line(&body, CLAUDE_TITLE, &subtitle, &[]);
        }
    }

    /// py: telegram.py:4623 `def relay_turn_began(self, prompt: str, cwd: str = "") -> None:`
    ///
    /// Claude Code's UserPromptSubmit hook: a turn began with ``prompt``. For the joined session, the lines that
    /// follow answer that prompt, so they take its origin: the prompt eve typed with these words (the oldest such,
    /// and any typed before it that never began are dropped), or None for a prompt eve did not type (the owner at
    /// the Mac's keyboard). A turn of another session changes nothing. The boundary is the next turn's beginning,
    /// not the Stop hook, so a line the transcript tailer delivers after Stop still belongs to its own turn
    /// (verification/Eve/CallScope.lean, the relay FIFO).
    pub fn relay_turn_began(self: &Arc<Self>, prompt: &str, cwd: &str) {
        let target = {
            let st = self.st.lock();
            if st.relay_pin.is_empty() { st.relay_cwd.clone() } else { st.relay_pin.clone() }
        };
        if !target.is_empty() && !cwd.is_empty() && !same_folder(cwd, &target) {
            return;
        }
        let key = prompt_key(prompt);
        let mut st = self.st.lock();
        if let Some(i) = st.relay_prompts.iter().position(|(typed, _)| *typed == key) {
            let mut origin = None;
            for _ in 0..=i {
                origin = st.relay_prompts.pop_front().and_then(|(_, o)| o);
            }
            st.relay_origin = origin;
            return;
        }
        st.relay_origin = None;
    }

    /// py: telegram.py:4642 `def relay_turn_ended(self, cwd: str = "") -> None:`
    ///
    /// Claude Code's Stop hook: a turn ended. For the joined session, "Thinking…" (or "typing…") stops
    /// now, even when the turn ended without a word for the phone. Another session's end changes nothing, and
    /// neither does the end of a session eve does not know (the daemon passes no folder): with a session
    /// picked, a Stop from anywhere else used to end its "Thinking…" early (review, 2026-09-23).
    pub fn relay_turn_ended(self: &Arc<Self>, cwd: &str) {
        let (pin, chat_id) = {
            let st = self.st.lock();
            (st.relay_pin.clone(), st.chat_id)
        };
        if !pin.is_empty() && (cwd.is_empty() || !same_folder(cwd, &pin)) {
            return;
        }
        self.stop_thinking(chat_id);
        self.retire_options(); // the turn is over: its question was answered
        let chief = self.chief.lock().clone();
        if let Some(chief) = chief {
            // chief_desk.BREAKPOINTS names relay_end, and until 2026-10-06 nothing sent it: a held reminder
            // never went out while the owner worked through the relay.
            if let Err(e) = chief.breakpoint("relay_end", None) {
                tracing::warn!("telegram: the chief missed a relay breakpoint ({})", chief_error_name(&e));
            }
        }
    }

    /// py: telegram.py:4659 `async def relay_notification(self, kind: str, message: str, waits: bool) -> None:`
    pub async fn relay_notification(self: &Arc<Self>, _kind: &str, message: &str, waits: bool) {
        let chat_id = {
            let st = self.st.lock();
            if !st.claude || !waits {
                return;
            }
            let Some(chat_id) = st.chat_id else { return };
            chat_id
        };
        self.stop_thinking(Some(chat_id)); // waiting on the owner is not working
        let (last_ask_at, origin) = {
            let st = self.st.lock();
            (st.last_ask_at, st.relay_origin.clone())
        };
        if (self.clock)() - last_ask_at < ASKED_RECENTLY_SECS {
            return; // the question itself was just sent: no vague echo
        }
        let mut turn = ctx();
        turn.call = origin; // the daemon's task: the turn's prompt's call, as relay_line
        let line = eve_envfile::py_strip(message).to_string();
        in_ctx(turn, self.say(chat_id, &line, Some(CLAUDE_WAITS_TITLE), None, &[], 0, None)).await;
    }

    /// py: telegram.py:4671 `async def decide_permission(self, tool: str, hint: str, cwd: str = "", *, always: bool = False) -> Optional[str]:`
    ///
    /// A permission prompt as a question in the chat. "allow" | "deny" | None (no answer: Claude Code's
    /// own flow decides). Asked only with ``always`` (the daemon's always_ask class, or Jev's risky verdict)
    /// or EVE_TELEGRAM_ASK=1; otherwise the relay allows without asking and this is never reached.
    ///
    /// It comes with Allow and Deny buttons (owner, 2026-09-23). A tap answers it; so does a typed clear
    /// yes or no. Anything else the owner types meanwhile is not taken as the answer: it goes to Claude as
    /// usual and the prompt keeps waiting (``_answers_pending``). When the buttons cannot be sent, the
    /// prompt is plain text and the owner's next message is its answer, as before. Once decided, or when
    /// it times out, the message is edited to say what happened and the buttons go.
    pub async fn decide_permission(self: &Arc<Self>, tool: &str, hint: &str, cwd: &str, always: bool) -> Option<String> {
        let chat_id = {
            let st = self.st.lock();
            if !st.claude || !(always || self.config.ask_permissions) {
                return None; // off by default: Claude Code's own flow decides
            }
            st.chat_id?
        };
        if self.awaiting_answer() {
            return None; // one question at a time; this one defers
        }
        if !cwd.is_empty() {
            self.st.lock().relay_cwd = cwd.to_string();
        }
        self.stop_thinking(Some(chat_id)); // asking is not working
        // The command as code, so nothing in it is read as markup; the repo under the title.
        let hint300: String = eve_envfile::py_strip(hint).chars().take(300).collect();
        let command = format!("```\n{hint300}\n```");
        let question = format!("{command}\n\nyes / no?");
        let title = CLAUDE_PERMISSION_TITLE.replace("{tool}", tool);
        let subtitle = if cwd.is_empty() { None } else { Some(path_name(cwd)) };
        let origin = {
            let mut st = self.st.lock();
            st.last_ask_at = (self.clock)();
            st.relay_origin.clone() // the turn's prompt, as the prompt arrives
        };
        let future = Pending::new();
        self.st.lock().permission_future = Some(future.clone());
        // Strict from the start, not from when the send returns: while the prompt is on its way the owner may
        // well be typing to Claude, and a non-strict prompt would take that text as its answer and lose it.
        // Only a send that falls back to plain text makes it non-strict (owner, 2026-09-23). No words beyond
        // yes and no: bare_decision reads those.
        let asked = Arc::new(Mutex::new(self.open_question(&future, None, true, &HashSet::new(), false)));
        let board = Arc::new(_Keyboard {
            chat_id: Some(chat_id),
            kind: ANSWER.into(),
            choices: relay_util::allow_deny().to_vec(),
            future: Some(future.clone()),
            valid: None,
            on_stop: None,
            keys: Mutex::new(Vec::new()),
            message_id: Mutex::new(0),
        });
        self.note("eve", &format!("{title}: {hint300}"), "relay", None, None, None, false, None);
        // `finally`, which also runs when this is cancelled: the slot moves on, the keys go, and the prompt is
        // edited to say what happened.
        let outcome: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let _finally = {
            let (me, future, asked, board, outcome) = (self.clone(), future.clone(), asked.clone(), board.clone(), outcome.clone());
            let (command, title, subtitle) = (command.clone(), title.clone(), subtitle.clone());
            on_drop(move || {
                let asked = asked.lock().clone();
                me.close_question(&asked);
                {
                    let mut st = me.st.lock();
                    if st.permission_future.as_ref().is_some_and(|p| p.same(&future)) {
                        st.permission_future = None;
                    }
                }
                me.retire(Some(&board), false);
                let [allow, deny] = relay_util::allow_deny();
                let line = match outcome.lock().as_deref() {
                    Some("allow") => allow.done,
                    Some("deny") => deny.done,
                    _ => PERMISSION_DEFERRED_LINE.to_string(),
                };
                if !me.st.lock().stopping && tokio::runtime::Handle::try_current().is_ok() {
                    let settling = me.clone();
                    me.spawn(
                        async move { settling.settle_prompt(&board, &command, &line, Some(&title), subtitle.as_deref()).await },
                        "telegram-edit",
                    );
                }
            })
        };
        let mut turn = ctx();
        turn.call = origin; // the daemon's hook task: the prompt's call, as above
        let buttons = in_ctx(
            turn,
            self.send_choices(Some(chat_id), &question, &board, Some(&title), subtitle.as_deref(), &[], 2, false),
        )
        .await;
        if !buttons && !future.done() {
            let loose = self.loosen_question(&asked.lock().clone());
            *asked.lock() = loose;
            self.show_question();
        }
        let waited = tokio::time::timeout(std::time::Duration::from_secs_f64(self.permission_timeout.max(0.0)), future.wait()).await;
        let decided = match waited {
            Ok(Ok(answer)) => {
                // neither a clear yes nor a no: the dialog decides
                let decided = eve_consent::decision(&answer);
                if decided.is_empty() { None } else { Some(decided.to_string()) }
            }
            Ok(Err(CancelledError)) => {
                // Python's CancelledError: it leaves with the slot moved on and the prompt settled (the finally),
                // and Claude Code's own flow decides.
                None
            }
            Err(_) => {
                future.cancel(); // wait_for cancels what it waited on
                None
            }
        };
        *outcome.lock() = decided.clone();
        decided
    }

    /// py: telegram.py:4730 `async def _stop_task(self, progress: _Progress, chat_id: int) -> None:`
    ///
    /// The Stop button on a task's progress message: that task stops, exactly as "stop" typed outside a
    /// Codex chat. A Codex chat that is on is left alone; the button belongs to the task.
    pub(crate) async fn stop_task(self: &Arc<Self>, progress: &Arc<_Progress>, chat_id: i64) {
        let ours = self.st.lock().task_progress.as_ref().is_some_and(|p| Arc::ptr_eq(p, progress));
        if ours {
            self.stop(chat_id, None).await;
        } else {
            self.say(chat_id, NOTHING_TO_STOP_LINE, None, None, &[], 0, None).await;
        }
    }

    /// py: telegram.py:4738 `def _route_stop(self, chat_id: int) -> None:`
    pub(crate) fn route_stop(self: &Arc<Self>, chat_id: i64) {
        let america_stop = self.cancel_america_command(chat_id);
        let purchase_stop: Option<Obj> = self.purchases.as_ref().map(|p| match p.stop(Some(chat_id)).to_value() {
            Value::Object(o) => o,
            _ => Obj::new(),
        });
        let purchase_hit = purchase_stop.as_ref().is_some_and(|p| truthy(p.get("stopped")) || truthy(p.get("in_flight")));
        let codex_chat = self.st.lock().codex_chat;
        if america_stop {
            let me = self.clone();
            self.spawn(
                async move { me.say(chat_id, "Cancelled the /america question.", None, None, &[], 0, None).await },
                "telegram-america-cancel",
            );
            if codex_chat != Some(chat_id) && !self.task_running() && !purchase_hit {
                return;
            }
        }
        if codex_chat == Some(chat_id) {
            let epoch = self.st.lock().codex_epoch;
            self.spawn_codex_interrupt(chat_id, epoch);
            if purchase_hit {
                let line = relay_util::line_of(purchase_stop.as_ref());
                let me = self.clone();
                self.spawn(async move { me.say(chat_id, &line, None, None, &[], 0, None).await }, "telegram-purchase-stop");
            }
        } else {
            let me = self.clone();
            self.spawn(async move { me.stop(chat_id, purchase_stop.as_ref()).await }, "telegram-stop");
        }
    }

    /// py: telegram.py:4753 `async def _stop(self, chat_id: int, purchase_stop: Optional[dict[str, Any]] = None) -> None:`
    pub(crate) async fn stop(self: &Arc<Self>, chat_id: i64, purchase_stop: Option<&Obj>) {
        let made;
        let purchase_stop = match (purchase_stop, &self.purchases) {
            (None, Some(purchases)) => {
                made = match purchases.stop(Some(chat_id)).to_value() {
                    Value::Object(o) => o,
                    _ => Obj::new(),
                };
                Some(&made)
            }
            (given, _) => given,
        };
        let stopped_purchase = purchase_stop.is_some_and(|p| truthy(p.get("stopped")) || truthy(p.get("in_flight")));
        let agent = {
            let st = self.st.lock();
            st.agent.clone().filter(|_| !st.task_returned)
        };
        let agent = agent.filter(|_| self.task_running());
        if let Some(agent) = &agent {
            // Said now, by code: the agent can take seconds to unwind, and its own "I stopped" would
            // only repeat this, so that one is not sent (_run_agent).
            self.st.lock().stopped_from_chat = true;
            agent.cancel("stopped from Telegram");
        }
        if stopped_purchase {
            let line = relay_util::line_of(purchase_stop);
            self.say(chat_id, &line, None, None, &[], 0, None).await;
        }
        if agent.is_some() {
            self.say(chat_id, STOPPED_LINE, None, None, &[], 0, None).await;
        } else if !stopped_purchase {
            self.say(chat_id, NOTHING_TO_STOP_LINE, None, None, &[], 0, None).await;
        }
    }

    /// `asked.strict = False` (and, for a task's question, `asked.words = frozenset()`): the question went as plain
    /// text after all, so the owner's next message answers it. `_Question`'s fields are fixed once shared, so the
    /// loosened question takes the old one's place in every slot that holds it → the new handle.
    pub(crate) fn loosen_question(self: &Arc<Self>, asked: &Arc<_Question>) -> Arc<_Question> {
        let loose = Arc::new(_Question {
            future: asked.future.clone(),
            chat_id: asked.chat_id,
            strict: false,
            words: HashSet::new(),
            text: Mutex::new(asked.text.lock().clone()),
            firm: asked.firm,
        });
        let mut st = self.st.lock();
        for q in st.questions.iter_mut() {
            if Arc::ptr_eq(q, asked) {
                *q = loose.clone();
            }
        }
        for q in st.america_questions.values_mut() {
            if Arc::ptr_eq(q, asked) {
                *q = loose.clone();
            }
        }
        loose
    }
}

/// What the relay's code shares beyond `say_relay_util`: path names as Python's `pathlib` gives them, and the
/// errors' class names.
pub(super) mod relay_util {
    use crate::prelude::*;
    use crate::telegram::*;

    use super::super::say::say_relay_util::choice;

    /// `ALLOW_DENY`.
    pub(crate) fn allow_deny() -> [Choice; 2] {
        [choice("Allow", "yes", "success", "Allowed."), choice("Deny", "no", "danger", "Denied.")]
    }

    /// `purchase_stop["line"]` as `str` would give it.
    pub(crate) fn line_of(stop: Option<&Obj>) -> String {
        match stop.and_then(|s| s.get("line")) {
            Some(Value::String(s)) => s.clone(),
            other => super::super::say::say_relay_util::reason_line(other),
        }
    }

    /// `except (codex_chat.CodexUnavailable, OSError, TimeoutError)`: the classes the chat answers. TimeoutError is
    /// an OSError in Python, and so are its other subclasses.
    pub(crate) fn caught_codex(class: &str) -> bool {
        matches!(
            class,
            "CodexUnavailable"
                | "OSError"
                | "TimeoutError"
                | "FileNotFoundError"
                | "PermissionError"
                | "FileExistsError"
                | "IsADirectoryError"
                | "NotADirectoryError"
                | "ConnectionError"
                | "ConnectionRefusedError"
                | "ConnectionResetError"
                | "ConnectionAbortedError"
                | "BrokenPipeError"
                | "BlockingIOError"
                | "ChildProcessError"
                | "InterruptedError"
                | "ProcessLookupError"
        )
    }

    /// `type(e).__name__` for what `chief.breakpoint` raised.
    pub(crate) fn chief_error_name(e: &anyhow::Error) -> String {
        match e.downcast_ref::<eve_chief::Raised>() {
            Some(raised) => raised.name.clone(),
            None => super::super::say::say_relay_util::class_of(e).to_string(),
        }
    }

    /// `Path(p).name`: the last part, "" for a root or an empty path.
    pub(crate) fn path_name(p: &str) -> String {
        p.split('/').filter(|part| !part.is_empty() && *part != ".").next_back().unwrap_or_default().to_string()
    }

    /// Two launch flows' clocks are the same flow's (Python's `self._launch is flow`): each flow gets its own.
    pub(crate) fn same_clock(a: &eve_claude_launch::Clock, b: &eve_claude_launch::Clock) -> bool {
        std::ptr::addr_eq(Arc::as_ptr(a), Arc::as_ptr(b))
    }

    /// `os.path.realpath(p)` (not strict): links resolved as far as the path exists, the rest normalised.
    pub(crate) fn realpath(p: &str) -> String {
        let base = if p.starts_with('/') {
            PathBuf::from("/")
        } else {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
        };
        let mut out = base;
        let mut resolved = true;
        for part in p.split('/') {
            match part {
                "" | "." => continue,
                ".." => {
                    out.pop();
                }
                name => {
                    out.push(name);
                    if resolved {
                        match std::fs::canonicalize(&out) {
                            Ok(real) => out = real,
                            Err(_) => resolved = false,
                        }
                    }
                }
            }
        }
        out.to_string_lossy().into_owned()
    }
}
