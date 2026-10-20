//! A port of `telegram.py`, lines 2884-3262: the poll loop, shutdown, the menu, typing and drafts, spawn, app hand-off.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

use futures::FutureExt;

/// Python's `try: ... finally: f()` for a task that may be cancelled: `f` runs when the future is dropped,
/// finished or not.
pub(super) struct Finally<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> Finally<F> {
    pub(super) fn new(f: F) -> Self {
        Self(Some(f))
    }

    /// The body ran to its end and did the `finally` itself: nothing more on drop.
    pub(super) fn disarm(&mut self) {
        self.0 = None;
    }
}

impl<F: FnOnce()> Drop for Finally<F> {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

/// `run`'s `finally: await self._shutdown()` when the loop is cancelled (its future dropped): the shutdown goes on
/// as its own task, as Python's finally runs inside the cancelled one. A loop that returns shuts down itself and
/// disarms this first.
struct ShutdownOnDrop(Option<Arc<TelegramInlet>>);

impl Drop for ShutdownOnDrop {
    fn drop(&mut self) {
        if let Some(inlet) = self.0.take() {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move { inlet.shutdown().await });
            }
        }
    }
}

/// Python's `type(e).__name__` for the errors these paths log (never their text, which may quote a URL).
pub(super) fn error_name(e: &anyhow::Error) -> &'static str {
    if e.downcast_ref::<BotApiError>().is_some() {
        return "BotApiError";
    }
    if let Some(set) = e.downcast_ref::<eve_link_reactions::SetError>() {
        return match set {
            eve_link_reactions::SetError::NotAnEmoji => "ValueError",
            _ => "KeyError",
        };
    }
    if e.downcast_ref::<std::io::Error>().is_some() {
        return "OSError";
    }
    if e.downcast_ref::<eve_telegram_images::ImageError>().is_some() {
        return "ImageError";
    }
    "Exception"
}

impl TelegramInlet {
    /// py: telegram.py:2884 `async def run(self) -> None:`
    pub async fn run(self: &Arc<Self>) {
        let mut finally = ShutdownOnDrop(Some(self.clone()));
        if self.config.app_only {
            // EVE_TELEGRAM=app: the brain without the door. Nothing polls Telegram; this waits to be stopped.
            tracing::info!(
                "telegram: app only (EVE_TELEGRAM=app): Telegram is not polled and nothing is sent to it; turns come \
                 from the Eve Link app, at {} with {} effort",
                self.config.model,
                self.config.effort
            );
            futures::future::pending::<()>().await;
            return;
        }
        tracing::info!(
            "telegram: listening for {} owner id(s); text turns go to {} at {} effort",
            self.config.owner_ids.len(),
            self.config.model,
            self.config.effort
        );
        let me = self.clone();
        self.spawn(async move { me.set_commands().await }, "telegram-commands");
        let mut offset: Option<i64> = None;
        let mut backoff = 1.0_f64;
        loop {
            let updates = match self.api.get_updates(offset, POLL_TIMEOUT_SECS as i64).await {
                Ok(updates) => updates,
                Err(e) => {
                    if FATAL_CODES.contains(&e.code) {
                        tracing::error!(
                            "telegram: stopped — the Bot API answered {} ({}). 401/404 is a wrong EVE_TELEGRAM_TOKEN; \
                             409 is another program polling the same bot.",
                            e.code,
                            e.description
                        );
                        break;
                    }
                    tracing::warn!("telegram: poll failed ({}); retrying in {:.0} s", e, backoff);
                    (self.sleep)(backoff).await;
                    backoff = BACKOFF_MAX_SECS.min(backoff * 2.0);
                    continue;
                }
            };
            backoff = 1.0;
            for update in updates {
                // Nothing Telegram sends may end the loop: a shape this code does not know is one more thing to
                // drop (accept() says "malformed"), never a crash.
                let uid = match update.get("update_id") {
                    Some(Value::Number(n)) => n.as_i64(),
                    Some(Value::Bool(b)) => Some(i64::from(*b)),
                    _ => None,
                };
                if let Some(uid) = uid {
                    offset = Some(uid + 1); // acknowledged on the next poll: handled once
                }
                self.dispatch(&Value::Object(update));
            }
            self.close_quiet_chat();
        }
        finally.0 = None;
        self.shutdown().await;
    }

    /// py: telegram.py:2925 `async def _shutdown(self) -> None:`
    pub(crate) async fn shutdown(self: &Arc<Self>) {
        self.st.lock().stopping = true;
        if let Some(purchases) = &self.purchases {
            purchases.shutdown();
        }
        // An open progress message would keep saying "On it" with a live-looking Stop button that, after the
        // restart, only answers "expired" (review, 2026-09-23). Closed first, best effort, briefly.
        let (task_progress, codex_progress) = {
            let st = self.st.lock();
            (st.task_progress.clone(), st.codex_progress.clone())
        };
        for (progress, line) in [(task_progress, PROGRESS_STOPPED_LINE), (codex_progress, PROGRESS_CLOSED_LINE)] {
            if let Some(progress) = progress {
                if !progress.st.lock().closed {
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(3),
                        self.close_progress(Some(&progress), line),
                    )
                    .await;
                }
            }
        }
        let agent = self.st.lock().agent.clone();
        if let Some(agent) = agent {
            if self.task_running() {
                agent.cancel("the daemon is stopping");
                // A restart used to end a texted task in silence (live, 2026-09-23: a Chrome task died with two
                // restarts from another session's firmware flash, and the owner waited). Say so, briefly.
                let (chat, goal) = {
                    let st = self.st.lock();
                    (st.task_chat, st.task_goal.clone())
                };
                if let Some(chat) = chat {
                    let goal: String = goal.chars().take(120).collect();
                    let line = RESTART_LINE.replace("{goal}", &goal);
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(3),
                        self.say(chat, &line, None, None, &[], 0, None),
                    )
                    .await;
                }
            }
        }
        // Taken as the jobs are cancelled: every turn still open now is one whose answer will never come.
        let (cut_off, jobs): (Vec<(i64, i64)>, Vec<Job>) = {
            let st = self.st.lock();
            (st.open_turns.values().copied().collect(), st.jobs.clone())
        };
        for job in &jobs {
            job.cancel();
        }
        if !cut_off.is_empty() {
            // Best effort, and briefly: one line per cut-off turn, as a reply to the owner's message, all within
            // TURN_RESTART_SECS so a slow Telegram never holds up the restart.
            let lines = cut_off
                .iter()
                .map(|&(chat, message_id)| self.say(chat, TURN_RESTART_LINE, None, None, &[], message_id, None));
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs_f64(TURN_RESTART_SECS),
                futures::future::join_all(lines),
            )
            .await;
            tracing::info!(
                "telegram: {} chat turn(s) cut off by the restart; the owner was told, best effort",
                cut_off.len()
            );
        }
        self.stop_thinking(None);
        let typing: Vec<Job> = {
            let mut st = self.st.lock();
            st.typing.clear();
            st.typing_tasks.drain().map(|(_, job)| job).collect()
        };
        for job in &typing {
            job.cancel();
        }
        let jobs: Vec<Job> = self.st.lock().jobs.clone();
        for job in jobs.iter().chain(typing.iter()) {
            job.join().await;
        }
        let close_america = {
            let mut st = self.st.lock();
            let first = !st.america_closed;
            if self.america.is_some() && first {
                st.america_closed = true;
            }
            self.america.is_some() && first
        };
        if close_america {
            if let Some(america) = &self.america {
                if let Err(exc) = america.close().await {
                    // closing a provider must not cost the inlet's shutdown
                    tracing::warn!("telegram: /america could not close ({})", error_name(&exc));
                }
            }
        }
        self.codex.close().await;
        self.close_chat();
    }

    /// py: telegram.py:2980 `def menu_commands(self) -> tuple[tuple[str, str], ...]:`
    ///
    /// The / menu as this inlet runs: BOT_COMMANDS, plus /jobs while the chief is on, /america and /opentable
    /// while they are on, each before /stop. Telegram's menu is set from it (_set_commands).
    pub fn menu_commands(self: &Arc<Self>) -> Vec<(String, String)> {
        fn before_last(commands: &mut Vec<(String, String)>, command: (&str, &str)) {
            let at = commands.len().saturating_sub(1);
            commands.insert(at, (command.0.to_string(), command.1.to_string()));
        }
        let mut commands: Vec<(String, String)> =
            BOT_COMMANDS.iter().map(|(c, d)| (c.to_string(), d.to_string())).collect();
        // /jobs only while the chief is on: the menu never offers a dead word
        if self.chief.lock().is_some() {
            before_last(&mut commands, (JOBS_MENU[0], JOBS_MENU[1]));
        }
        if self.america.is_some() {
            before_last(&mut commands, (AMERICA_MENU[0], AMERICA_MENU[1]));
        }
        if self.opentable_enabled {
            before_last(&mut commands, eve_opentable::MENU);
        }
        commands
    }

    /// py: telegram.py:2991 `async def _set_commands(self) -> None:`
    ///
    /// eve's code words in the / menu of each owner's private chat (a private chat's id is its user's
    /// id). Once per start, and fail-soft: a menu Telegram refuses leaves every word working when typed.
    pub(crate) async fn set_commands(self: &Arc<Self>) {
        let mut done = 0;
        let commands = self.menu_commands();
        let mut owners = self.config.owner_ids.clone();
        owners.sort_unstable();
        owners.dedup();
        for owner in owners {
            if let Err(e) = self.api.set_commands(&commands, owner).await {
                // One owner's refusal (a chat that never opened the bot: "chat not found") must not cost the
                // owners after it their menu (review, 2026-09-23): log it and go on.
                tracing::warn!(
                    "telegram: could not set the / menu for one owner chat ({}); the code words still work typed",
                    e
                );
                continue;
            }
            done += 1;
        }
        tracing::info!("telegram: / menu set for {} of {} owner chat(s)", done, self.config.owner_ids.len());
    }

    /// py: telegram.py:3011 `def _keep_typing(self, chat_id: Optional[int], reason: str, secs: float) -> None:`
    ///
    /// Show "typing…" in this chat for ``reason`` for about ``secs`` (in 4 s ticks), until
    /// ``_stop_typing`` with the same reason. A second call for a reason already shown extends it, never
    /// doubles it: one loop per chat, one sendChatAction per tick, whatever the number of reasons. Not for work
    /// an app turn began (``_FROM_APP``): the owner is in Eve Link, which shows its own, and a "typing…" in a
    /// Telegram chat nobody wrote in would only puzzle them.
    pub(crate) fn keep_typing(self: &Arc<Self>, chat_id: Option<i64>, reason: &str, secs: f64) {
        let Some(chat_id) = chat_id else { return };
        if ctx().app.is_some() {
            return;
        }
        let ticks = 1_i64.max((secs / TYPING_EVERY_SECS).ceil() as i64);
        let mut st = self.st.lock();
        let reasons = st.typing.entry(chat_id).or_default();
        let held = reasons.get(reason).copied().unwrap_or(0);
        reasons.insert(reason.to_string(), held.max(ticks));
        if st.typing_tasks.get(&chat_id).is_none_or(|task| task.done()) {
            // Not one of self._jobs: it is not work to wait for, only a sign that work is going on.
            let me = self.clone();
            st.typing_tasks.insert(chat_id, Job::spawn("telegram-typing", async move { me.typing_loop(chat_id).await }));
        }
    }

    /// py: telegram.py:3026 `def _stop_typing(self, chat_id: Optional[int], reason: str) -> None:`
    ///
    /// Drop one reason; with none left, the loop ends now, so no "typing…" follows the reply. A reason
    /// paused under a question to the owner is dropped as well: it is over, and must not resume after the
    /// answer (review, 2026-09-23: the relay's "typing…" came back for minutes after Claude had finished).
    pub(crate) fn stop_typing(self: &Arc<Self>, chat_id: Option<i64>, reason: &str) {
        let Some(chat_id) = chat_id else { return };
        let none_left = {
            let mut st = self.st.lock();
            if let Some(paused) = st.paused_typing.get_mut(&chat_id) {
                paused.remove(reason);
            }
            let Some(reasons) = st.typing.get_mut(&chat_id) else { return };
            reasons.remove(reason);
            reasons.is_empty()
        };
        if none_left {
            self.end_typing(chat_id);
        }
    }

    /// py: telegram.py:3040 `def _end_typing(self, chat_id: int) -> dict[str, int]:`
    ///
    /// Every reason at once, returned so ``_ask_user`` can put them back after the owner answers.
    pub(crate) fn end_typing(self: &Arc<Self>, chat_id: i64) -> HashMap<String, i64> {
        let (reasons, task) = {
            let mut st = self.st.lock();
            (st.typing.remove(&chat_id).unwrap_or_default(), st.typing_tasks.remove(&chat_id))
        };
        if let Some(task) = task {
            if !task.is_current() {
                task.cancel();
            }
        }
        reasons
    }

    /// py: telegram.py:3048 `async def _typing_loop(self, chat_id: int) -> None:`
    pub(crate) async fn typing_loop(self: &Arc<Self>, chat_id: i64) {
        let mine = tokio::task::try_id();
        let me = Arc::downgrade(self);
        let _finally = Finally::new(move || {
            let Some(inlet) = me.upgrade() else { return };
            let mut st = inlet.st.lock();
            if mine.is_some() && st.typing_tasks.get(&chat_id).map(Job::id) == mine {
                st.typing_tasks.remove(&chat_id);
            }
        });
        loop {
            {
                let mut st = self.st.lock();
                if st.typing.get(&chat_id).is_none_or(HashMap::is_empty) {
                    st.typing.remove(&chat_id);
                    return;
                }
            }
            if self.api.typing(chat_id).await.is_err() {
                // a missing "typing…" is not worth more than a debug line
                tracing::debug!("telegram: typing failed (BotApiError); not shown for this work");
                self.st.lock().typing.remove(&chat_id);
                return;
            }
            {
                let mut st = self.st.lock();
                if let Some(reasons) = st.typing.get_mut(&chat_id) {
                    for left in reasons.values_mut() {
                        *left -= 1;
                    }
                    reasons.retain(|_, left| *left > 0);
                }
            }
            (self.sleep)(TYPING_EVERY_SECS).await;
        }
    }

    /// py: telegram.py:3073 `def _start_thinking(self, chat_id: Optional[int]) -> None:`
    ///
    /// A line went into Claude's terminal: show the "Thinking…" draft until Claude asks, waits on the
    /// owner or ends its turn (``_stop_thinking``), for TYPING_RELAY_SECS at most. With drafts off, or once
    /// Telegram has refused one, this is the relay's "typing…" exactly as before.
    pub(crate) fn start_thinking(self: &Arc<Self>, chat_id: Option<i64>) {
        let Some(chat_id) = chat_id else { return };
        if !self.st.lock().drafts_ok {
            self.keep_typing(Some(chat_id), "relay", TYPING_RELAY_SECS);
            return;
        }
        let mut st = self.st.lock();
        st.draft_ticks = 1_i64.max((TYPING_RELAY_SECS / DRAFT_REFRESH_SECS).ceil() as i64);
        if st.draft_task.as_ref().is_none_or(Job::done) {
            st.draft_id += 1; // non-zero, and new for each turn
            let draft_id = st.draft_id;
            // Not one of self._jobs, as the typing loop is not: only a sign that work is going on.
            let me = self.clone();
            st.draft_task = Some(Job::spawn("telegram-draft", async move { me.draft_loop(chat_id, draft_id).await }));
        }
    }

    /// py: telegram.py:3088 `def _stop_thinking(self, chat_id: Optional[int]) -> None:`
    ///
    /// The turn is not working any more (it asks, waits, ended, or the relay went off): no bubble is sent
    /// again. One already on the screen goes when the next message arrives, or by itself within 30 s.
    pub(crate) fn stop_thinking(self: &Arc<Self>, chat_id: Option<i64>) {
        self.stop_typing(chat_id, "relay");
        let task = {
            let mut st = self.st.lock();
            st.draft_ticks = 0;
            st.draft_task.take()
        };
        if let Some(task) = task {
            if !task.is_current() {
                task.cancel();
            }
        }
    }

    /// py: telegram.py:3098 `def _thinking(self) -> bool:`
    pub(crate) fn thinking(self: &Arc<Self>) -> bool {
        let st = self.st.lock();
        st.draft_task.as_ref().is_some_and(|task| !task.done()) && st.draft_ticks > 0
    }

    /// py: telegram.py:3101 `async def _draft_loop(self, chat_id: int, draft_id: int) -> None:`
    pub(crate) async fn draft_loop(self: &Arc<Self>, chat_id: i64, draft_id: i64) {
        let mine = tokio::task::try_id();
        let me = Arc::downgrade(self);
        let _finally = Finally::new(move || {
            let Some(inlet) = me.upgrade() else { return };
            let mut st = inlet.st.lock();
            if mine.is_some() && st.draft_task.as_ref().map(Job::id) == mine {
                st.draft_task = None;
            }
        });
        loop {
            {
                let st = self.st.lock();
                if !(st.draft_ticks > 0 && st.drafts_ok) {
                    return;
                }
            }
            if !self.show_draft(chat_id, draft_id).await {
                return;
            }
            self.st.lock().draft_ticks -= 1;
            (self.sleep)(DRAFT_REFRESH_SECS).await;
        }
    }

    /// py: telegram.py:3112 `async def _show_draft(self, chat_id: int, draft_id: int) -> bool:`
    ///
    /// One "Thinking…" placeholder → False when Telegram refused it. A refusal turns drafts off for the
    /// process and hands the time left to the relay's "typing…", so the owner still sees work going on.
    pub(crate) async fn show_draft(self: &Arc<Self>, chat_id: i64, draft_id: i64) -> bool {
        match self.api.send_draft(chat_id, draft_id, "").await {
            Ok(()) => true,
            Err(e) => {
                // a missing bubble is cosmetic; "typing…" takes over
                tracing::warn!("telegram: could not show a draft ({}); \"typing…\" from now on", e);
                let left = {
                    let mut st = self.st.lock();
                    st.drafts_ok = false;
                    std::mem::replace(&mut st.draft_ticks, 0)
                };
                if left > 0 {
                    self.keep_typing(Some(chat_id), "relay", left as f64 * DRAFT_REFRESH_SECS);
                }
                false
            }
        }
    }

    /// py: telegram.py:3129 `def _spawn(self, coro: Awaitable[None], name: str) -> asyncio.Task:`
    ///
    /// A job of the chat's, kept in `jobs` until it is done (Python's done callback discards it; here a done job
    /// is pruned at the next spawn, and every reader skips one). Work an app turn began counts for `owner_busy`
    /// unless it is one of APP_BACKGROUND_JOBS.
    pub(crate) fn spawn(self: &Arc<Self>, coro: impl std::future::Future<Output = ()> + Send + 'static, name: &str) -> Job {
        let me = Arc::downgrade(self);
        let owned = async move {
            let mine = tokio::task::try_id();
            let _finally = Finally::new(move || {
                let (Some(inlet), Some(mine)) = (me.upgrade(), mine) else { return };
                let mut st = inlet.st.lock();
                st.jobs.retain(|job| job.id() != mine);
                st.app_jobs.retain(|job| job.id() != mine);
            });
            coro.await;
        };
        let task = Job::spawn(name, owned.boxed());
        let from_app = ctx().app.is_some() && !APP_BACKGROUND_JOBS.contains(&name);
        let mut st = self.st.lock();
        st.jobs.retain(|job| !job.done());
        st.app_jobs.retain(|job| !job.done());
        if !task.done() {
            st.jobs.push(task.clone());
            if from_app {
                st.app_jobs.push(task.clone());
            }
        }
        task
    }

    /// py: telegram.py:3142 `def app_filed(self, turn: str) -> Optional[str]:`
    ///
    /// The id of the card Eve Link app turn ``turn`` filed through take_on (its first), or None.
    pub fn app_filed(self: &Arc<Self>, turn: &str) -> Option<String> {
        self.st.lock().app_filed.get(turn).cloned()
    }

    /// py: telegram.py:3146 `def owner_busy(self) -> bool:`
    ///
    /// A turn the owner started is in flight: a text turn waiting for the lock or running, or the work an app
    /// turn started that answers within it (link_chat.py: the app's stream ends, and its ``busy`` clears, once
    /// this is False). Synchronous and cheap: two set sizes.
    pub fn owner_busy(self: &Arc<Self>) -> bool {
        let st = self.st.lock();
        !st.open_turns.is_empty() || st.app_jobs.iter().any(|job| !job.done())
    }

    /// py: telegram.py:3152 `def waiting_on_owner(self) -> bool:`
    ///
    /// A question is waiting on the owner's answer (a chief's Go, an approval, a task's question). The app's
    /// ``busy`` is False meanwhile (link_chat ``_messages``): waiting on the owner is not working, so no typing
    /// dots under the question. ``owner_busy`` itself is unchanged, since a stream's end is decided by it and a
    /// new request typed while a question waits must keep its stream open.
    pub fn waiting_on_owner(self: &Arc<Self>) -> bool {
        self.live_question().is_some()
    }

    /// py: telegram.py:3160 `def transcripts(self) -> Any:`
    ///
    /// The transcript store this chat writes (memory.Memory.transcripts), or None without a Memory.
    pub fn transcripts(self: &Arc<Self>) -> Option<Arc<eve_transcripts::Transcripts>> {
        self.memory.as_ref().map(|memory| memory.transcripts())
    }

    /// py: telegram.py:3164 `def _to_app(self, chat_id: Optional[int], text: str, kind: str = "say") -> bool:`
    ///
    /// One of eve's lines, for the Eve Link app turn this work began on (``_FROM_APP``) → whether the app
    /// has it. False (not an app turn, no link, or nobody reading) leaves it to Telegram, so nothing is lost.
    pub(crate) fn to_app(self: &Arc<Self>, chat_id: Option<i64>, text: &str, kind: &str) -> bool {
        let Some(turn) = ctx().app else { return false };
        let Some(out) = self.app_out.lock().clone() else { return false };
        if chat_id != self.st.lock().chat_id || text.is_empty() {
            return false;
        }
        // a broken link costs the app the line, never the owner
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| out(&turn, text, kind))) {
            Ok(delivered) => delivered,
            Err(_) => {
                tracing::warn!("telegram: a line for the app was not delivered (Exception); sent to Telegram");
                false
            }
        }
    }

    /// py: telegram.py:3176 `def _dispatch(self, update: Any) -> None:`
    ///
    /// Decide one update and return at once: a turn is a job, so a slow one never stops the poll.
    pub(crate) fn dispatch(self: &Arc<Self>, update: &Value) {
        if update.as_object().is_some_and(|u| u.contains_key("callback_query")) {
            let (verdict, tap) = accept_tap(update, &self.config);
            let Some(tap) = tap else {
                let uid = sender_id(update);
                let query_id = update.get("callback_query").and_then(|q| q.get("id")).and_then(Value::as_str);
                if let (Some(uid), Some(query_id)) = (uid, query_id) {
                    if self.config.owner_ids.contains(&uid) {
                        // The owner's own tap outside their private chat: nothing acts on it, but it is answered,
                        // empty, so their button does not spin until Telegram gives up (review, 2026-09-23). A
                        // stranger's tap stays unanswered: the sender learns nothing.
                        let tap = Tap { query_id: query_id.to_string(), chat_id: 0, user_id: uid, data: String::new(), message_id: 0 };
                        let me = self.clone();
                        self.spawn(async move { me.answer_tap(&tap, "", false).await }, "telegram-tap");
                    }
                }
                self.drop_log(uid, "tap", &verdict);
                return;
            };
            self.on_tap(&tap);
            return;
        }
        let (verdict, inbound) = accept(update, &self.config, (self.wall)());
        let Some(inbound) = inbound else {
            // Once per id: enough to find your own number in the log, not enough to be flooded.
            self.drop_log(sender_id(update), "message", &verdict);
            return;
        };
        if verdict == FORWARDED {
            let me = self.clone();
            self.spawn(async move { me.say(inbound.chat_id, FORWARDED_LINE, None, None, &[], 0, None).await }, "telegram-say");
            return;
        }
        if verdict == NOT_TEXT {
            let me = self.clone();
            self.spawn(async move { me.say(inbound.chat_id, NOT_TEXT_LINE, None, None, &[], 0, None).await }, "telegram-say");
            return;
        }
        self.handle(&inbound);
    }

    /// The one log line a dropped update gets, once per sender id (at most 256 ids).
    fn drop_log(self: &Arc<Self>, uid: Option<i64>, what: &str, verdict: &str) {
        let Some(uid) = uid else { return };
        {
            let mut st = self.st.lock();
            if st.dropped_ids.contains(&uid) || st.dropped_ids.len() >= 256 {
                return;
            }
            st.dropped_ids.insert(uid);
        }
        tracing::info!("telegram: dropped a {} ({}) from user id {}", what, verdict, uid);
    }

    /// py: telegram.py:3211 `def _answers_pending(self, text: Optional[str]) -> bool:`
    ///
    /// Is this message the answer to the question waiting on the owner? Any message is, for a task's
    /// question and for a prompt sent without buttons (the next message is the answer). A strict prompt
    /// (Allow/Deny buttons on the screen) takes only a reply that is wholly a yes or a no
    /// (``consent.bare_decision``) or one of its buttons' words typed out: "ok, also update the README" opens
    /// with a yes-word but is a message for Claude, and it must not allow an rm on the way (owner,
    /// 2026-09-23). Other text is not the answer only while a relay (Claude or Codex) is on to take it: with
    /// no relay, the next message answers even a prompt with buttons, as it always has, so a task's
    /// "yes, but the cheaper one" is still its answer. ``None`` (an image) never is. A firm question (the
    /// chief's Go) is strict like a permission prompt, relay or not, buttons or not: with no relay on, "ok, also
    /// what's the weather tomorrow?" approved a one-way act and never reached the brain (reviewer after P4,
    /// 2026-09-29).
    pub(crate) fn answers_pending(self: &Arc<Self>, text: Option<&str>) -> bool {
        if !self.awaiting_answer() {
            return false;
        }
        let st = self.st.lock();
        if !st.pending_strict && !st.pending_firm {
            return true;
        }
        let Some(text) = text else { return false };
        if !eve_consent::bare_decision(text).is_empty() || st.pending_words.contains(&typed_word(text)) {
            return true;
        }
        let permission = match (&st.permission_future, &st.pending_answer) {
            (Some(permission), Some(pending)) => permission.same(pending),
            (None, None) => true,
            _ => false,
        };
        if permission || st.pending_firm {
            return false; // a permission prompt and a Go stay strict, relay or not
        }
        !(st.claude || st.codex_chat.is_some())
    }

    /// py: telegram.py:3235 `def _strict_answer(self, text: str) -> str:`
    ///
    /// What a typed answer hands the waiting question. A prompt with buttons takes a bare yes or no in
    /// any wording ("ok", "sure", "nope"): it is handed on as the buttons' own "yes" or "no", so the words
    /// the owner typed do what a tap does. A Codex command's prompt accepts only "yes" (ONCE_ANSWERS), and
    /// a typed "ok" used to decline it (review, 2026-09-23). Any other answer goes on as it was typed.
    pub(crate) fn strict_answer(self: &Arc<Self>, text: &str) -> String {
        {
            let st = self.st.lock();
            if !(st.pending_strict || st.pending_firm) || st.pending_words.contains(&typed_word(text)) {
                return text.to_string();
            }
        }
        match eve_consent::bare_decision(text) {
            "allow" => "yes".to_string(),
            "deny" => "no".to_string(),
            _ => text.to_string(),
        }
    }

    /// py: telegram.py:3245 `def _image_waits(self) -> bool:`
    ///
    /// An image cannot answer a question, so while one waits on a typed answer the image is refused.
    /// That is every question with no relay on (the next message is its answer, buttons or not), and a
    /// question without buttons with one on. A prompt with buttons while a relay takes the chat's words
    /// lets the image through to the relay, as it lets other text through.
    pub(crate) fn image_waits(self: &Arc<Self>) -> bool {
        if !self.awaiting_answer() {
            return false;
        }
        let st = self.st.lock();
        !st.pending_strict || !(st.claude || st.codex_chat.is_some())
    }

    /// py: telegram.py:3254 `def _defer_permission(self) -> None:`
    ///
    /// The relay is going off, or to another session: a permission prompt still waiting ends as "no
    /// answer here", so the dialog on the Mac decides and the prompt is edited to say so. Without this the
    /// prompt stayed pending for minutes after "claude off", and the owner's next message, meant for eve,
    /// could be taken as its answer: "ok, what's on my calendar" allowed an rm (owner, 2026-09-23).
    pub(crate) fn defer_permission(self: &Arc<Self>) {
        let future = self.st.lock().permission_future.clone();
        if let Some(future) = future {
            if !future.done() {
                future.set_result(""); // consent.decision("") is "": the dialog decides
            }
        }
    }
}

/// `text.strip().lower().rstrip(".!")`: a typed answer as a button's word.
fn typed_word(text: &str) -> String {
    text.trim().to_lowercase().trim_end_matches(['.', '!']).to_string()
}
