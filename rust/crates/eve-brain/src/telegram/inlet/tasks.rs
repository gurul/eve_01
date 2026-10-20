//! A port of `telegram.py`, lines 7078-7339: computer tasks, agent events, questions to the owner.
//!
//! Locking as in `say.rs`: `self.st` is never held across an await, while settling a future, or while calling
//! another method of the inlet.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

use std::sync::OnceLock;

use super::relay::relay_util::chief_error_name;
use super::say::say_relay_util::{class_of, on_drop, one_line, py_textwrap, reason_line, truthy};

/// A chief's step's record while it runs: the step lines (for the reflection's loop check) and the errors or stops
/// the agent reported rather than raised. Shared with the agent's `on_event`, which fills them as the run goes.
type Trail = Arc<Mutex<Vec<String>>>;

impl TelegramInlet {
    /// py: telegram.py:7078 `def _start_task(self, goal: str, chat_id: int, *, card: Optional[tuple[str, int]] = None, floor: Optional[str] = None, display_goal: Optional[str] = None) -> dict[str, Any]:`
    ///
    /// A computer task. ``card`` (card id, step) makes it a chief's step (``chief_start``): its goal is the
    /// chief's own, exactly as the owner's Go showed it, so no link from the chat is added. ``floor`` selects
    /// an explicit executor ("codex" for a chief's act or OpenTable's browser search). ``display_goal`` is
    /// the owner's short request when ``goal`` includes fixed integration instructions. A chief's result
    /// goes to the chief, which sends the receipt, in place of "Task result".
    pub(crate) fn start_task(self: &Arc<Self>, goal: &str, chat_id: i64, card: Option<(String, i64)>, floor: Option<&str>, display_goal: Option<&str>) -> Obj {
        let refuse = |reason: &str| {
            let mut o = Obj::new();
            o.insert("ok".into(), Value::Bool(false));
            o.insert("reason".into(), Value::String(reason.into()));
            o
        };
        if goal.is_empty() {
            return refuse("empty goal");
        }
        let mut goal = goal.to_string();
        if card.is_none() {
            // "Use Google search to open it up" after the owner shared a link: the model writes only this
            // message's words, so the link the owner means is carried in from their recent messages
            // (app_reflex, 2026-09-24).
            let owner: Vec<String> = self.current_turns().into_iter().filter(|(who, _)| who == "user").map(|(_, t)| t).collect();
            goal = eve_app_reflex::with_referenced_links(&goal, owner);
        }
        let Some(factory) = self.agent_factory.clone().filter(|_| self.agent_enabled) else {
            return refuse("computer control is disabled (EVE_COMPUTER_CONTROL=0)");
        };
        if self.task_running() {
            return refuse("a task is already running; steer or stop it first");
        }
        if (self.busy)() {
            return refuse("someone is talking with eve at the desk right now; the Mac is theirs until that conversation ends");
        }
        let display = display_goal.filter(|d| !d.is_empty()).map(str::to_string).unwrap_or_else(|| goal.clone());
        let request = {
            let mut st = self.st.lock();
            st.stopped_from_chat = false;
            st.task_returned = false;
            st.turn_request
        };
        // One message for the task's progress, "On it" with a Stop button, edited as steps come (owner,
        // 2026-09-23). It stands for the "On it" reply a started task used to get.
        let head = match &card {
            None => ON_IT_LINE.to_string(),
            Some((id, step)) => format!("On it: {id}, step {step}."),
        };
        let cell: Arc<OnceLock<std::sync::Weak<_Progress>>> = Arc::new(OnceLock::new());
        let on_stop: OnStop = {
            let (weak, cell) = (Arc::downgrade(self), cell.clone());
            Arc::new(move |cid| {
                let Some(me) = weak.upgrade() else { return };
                let progress = cell.get().and_then(|p| p.upgrade());
                let stopping = me.clone();
                me.spawn(
                    async move {
                        match progress {
                            Some(progress) => stopping.stop_task(&progress, cid).await,
                            // a progress message that is gone is no task's any more
                            None => stopping.say(cid, NOTHING_TO_STOP_LINE, None, None, &[], 0, None).await,
                        }
                    },
                    "telegram-stop",
                );
            })
        };
        let progress = self.open_progress(chat_id, &head, on_stop, None, None, if card.is_none() { request } else { 0 });
        let _ = cell.set(Arc::downgrade(&progress));
        self.st.lock().task_progress = Some(progress.clone());
        match &card {
            None => self.event(&format!("You asked the computer agent: {}", eve_chat_log::note_text(&display, eve_chat_log::NOTE_CHARS)), "", false),
            Some((id, step)) => self.event(&format!("You started step {step} of {id} on the computer agent"), "", false),
        }
        self.note("eve", &head, "say", None, None, None, false, None);
        let steps: Trail = Arc::new(Mutex::new(Vec::new()));
        let broke: Trail = Arc::new(Mutex::new(Vec::new()));
        let on_event: OnEvent = {
            let (weak, progress) = (Arc::downgrade(self), Arc::downgrade(&progress));
            let (steps, broke, chief) = (steps.clone(), broke.clone(), card.is_some());
            Arc::new(move |ev: &eve_agent_contract::AgentEvent| {
                if chief {
                    if ev.kind() == "progress" {
                        let mut steps = steps.lock();
                        if steps.len() < MAX_CHIEF_STEPS {
                            steps.push(ev.text().chars().take(200).collect());
                        }
                    }
                    if ev.kind() == "error" || ev.kind() == "cancelled" {
                        broke.lock().push(ev.kind().to_string());
                    }
                }
                if let Some(me) = weak.upgrade() {
                    me.on_agent_event(ev, Some(chat_id), progress.upgrade().as_ref());
                }
            })
        };
        let ask: AskUser = {
            let weak = Arc::downgrade(self);
            Arc::new(move |question: String| {
                let weak = weak.clone();
                Box::pin(async move {
                    let Some(me) = weak.upgrade() else { return Err(anyhow::Error::new(CancelledError)) };
                    me.ask_user_inner(&question, chat_id, TASK_ASKS_TITLE, None, None, false, false, None, true)
                        .await
                        .map_err(anyhow::Error::new)
                })
            })
        };
        let agent = factory(on_event, ask, floor.map(str::to_string));
        let since = wall(); // a file the task writes after this is the owner's to get
        {
            let mut st = self.st.lock();
            st.agent = Some(agent);
            st.chief_task = card.clone();
            st.task_chat = Some(chat_id);
            st.task_goal = display.clone();
        }
        let me = self.clone();
        let (run_goal, display_goal) = (goal.clone(), display_goal.map(str::to_string));
        let job = self.spawn(
            async move {
                match card {
                    None => me.run_agent_with(&run_goal, chat_id, Some(&progress), None, None, None, display_goal.as_deref(), since).await,
                    Some(card) => me.run_agent_with(&run_goal, chat_id, Some(&progress), Some(card), Some(steps), Some(broke), None, since).await,
                }
            },
            "telegram-agent",
        );
        self.st.lock().agent_task = Some(job);
        let mut o = Obj::new();
        o.insert("ok".into(), Value::Bool(true));
        o.insert("goal".into(), Value::String(goal));
        o.insert("note".into(), Value::String("started, not finished; the result is texted when it is done".into()));
        o
    }

    /// py: telegram.py:7137 `async def _run_agent(self, goal: str, chat_id: int, progress: Optional[_Progress] = None, *, card: Optional[tuple[str, int]] = None, steps: Sequence[str] = (), broke: Sequence[str] = (), display_goal: Optional[str] = None) -> None:`
    pub(crate) async fn run_agent(self: &Arc<Self>, goal: &str, chat_id: i64, progress: Option<&Arc<_Progress>>, card: Option<(String, i64)>, steps: &[String], broke: &[String], display_goal: Option<&str>) {
        let steps: Trail = Arc::new(Mutex::new(steps.to_vec()));
        let broke: Trail = Arc::new(Mutex::new(broke.to_vec()));
        // `getattr(self, "_task_started", time.time())`: a run not started by _start_task counts from now
        self.run_agent_with(goal, chat_id, progress, card, Some(steps), Some(broke), display_goal, wall()).await;
    }

    /// `_run_agent` with the chief's step record shared with the running agent, and `since` (`_task_started`).
    #[allow(clippy::too_many_arguments)]
    async fn run_agent_with(self: &Arc<Self>, goal: &str, chat_id: i64, progress: Option<&Arc<_Progress>>, card: Option<(String, i64)>, steps: Option<Trail>, broke: Option<Trail>, display_goal: Option<&str>, since: f64) {
        let Some(agent) = self.st.lock().agent.clone() else { return };
        // The hint goes beside the goal, never in it: the router reads only the owner's words (app_reflex.run).
        let (final_, failed) = match agent.run(goal, TASK_FILE_HINT).await {
            Ok(final_) => (final_, false),
            Err(e) => {
                if e.downcast_ref::<CancelledError>().is_some() {
                    return; // the daemon is stopping: nothing to say or settle
                }
                tracing::warn!("telegram: task failed: {}", class_of(&e));
                (self.on_state)("error");
                ("That task failed on my side.".to_string(), true)
            }
        };
        self.st.lock().task_returned = true; // before any await: a stop from here on is too late
        let mut final_ = eve_envfile::py_strip(&final_).to_string();
        if final_.is_empty() {
            final_ = "The task ended without a result.".into();
        }
        // A chief's step: the owner never sees its sentence (the receipt goes instead), and it may carry web text,
        // so the history gets a line composed by code, never the executor's words as eve's own (reviewer after
        // P4, 2026-09-29; tell_owner's ``about`` pattern).
        match &card {
            None => self.note("eve", &final_, "say", None, None, None, true, None),
            Some((id, step)) => self.note("eve", &format!("(Step {step} of {id} ended.)"), "say", None, None, None, false, None),
        }
        self.show(None, &final_, true);
        let stopped = {
            let mut st = self.st.lock();
            let ours = match (&st.task_progress, progress) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            };
            if ours {
                st.task_progress = None;
            }
            st.stopped_from_chat
        };
        self.close_progress(progress, if stopped { PROGRESS_STOPPED_LINE } else { PROGRESS_DONE_LINE }).await;
        if let Some(card) = card {
            // a chief's step: the chief reads the evidence and sends the receipt (never the agent's sentence)
            let stopped = {
                let mut st = self.st.lock();
                if st.chief_task.as_ref() == Some(&card) {
                    st.chief_task = None;
                }
                st.stopped_from_chat
            };
            let steps = steps.map(|s| s.lock().clone()).unwrap_or_default();
            let broke = broke.is_some_and(|b| !b.lock().is_empty());
            // Codex returns its failure sentence instead of raising (codex_computer.py:585-588): the error or stop
            // it reported is a failed step, never "ok" (reviewer after P4, 2026-09-29: such a run closed done)
            self.chief_result(card, &final_, !failed && !stopped && !broke, wall() - since, &steps).await;
        } else if !self.st.lock().stopped_from_chat {
            // The result arrives minutes after the request: the goal under the title says which one, and it
            // is a new message (an edit would not notify) replying to the owner's request.
            let title = if failed { TASK_FAILED_TITLE } else { TASK_DONE_TITLE };
            let subtitle = display_goal.filter(|d| !d.is_empty()).unwrap_or(goal);
            let reply = progress.map(|p| p.request_id).unwrap_or(0);
            self.say(chat_id, &final_, Some(title), Some(subtitle), &[], reply, None).await;
            let result = final_.clone();
            let files = tokio::task::spawn_blocking(move || task_files(&result, since, None)).await.unwrap_or_default();
            for path in files {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let sent = self.send_file(chat_id, &path.to_string_lossy(), &format!("From the task: {name}")).await;
                if !truthy(sent.get("ok")) {
                    let line = format!("I couldn't send {name}: {}", reason_line(sent.get("reason")));
                    self.say(chat_id, &line, None, None, &[], 0, None).await;
                }
            }
            // A picture only when the owner asked to see one (owner, 2026-10-13: "I don't need this browser shit for
            // computer use"): a task that merely used a browser used to send its tab's last view, often a blank card.
            if WANTS_SCREEN.is_match(goal) {
                let result = self.send_screen(chat_id, "the screen when the task ended", None).await;
                if !truthy(result.get("ok")) {
                    let line = format!("I couldn't send the task picture: {}", reason_line(result.get("reason")));
                    self.say(chat_id, &line, None, None, &[], 0, None).await;
                }
            }
        }
        let chief = self.chief.lock().clone();
        if let Some(chief) = chief {
            // a queued step or a held reminder may go now; the chief never costs a task its close
            if let Err(e) = chief.breakpoint("task_end", None) {
                tracing::warn!("telegram: the chief's task breakpoint failed ({})", chief_error_name(&e));
            }
        }
        let me = self.clone();
        self.spawn(async move { me.settle_board().await }, "telegram-board");
    }

    /// py: telegram.py:7194 `async def _settle_board(self) -> None:`
    pub(crate) async fn settle_board(self: &Arc<Self>) {
        (self.sleep)(DONE_HOLD_SECS).await;
        if !self.task_running() && !(self.busy)() {
            (self.on_state)("idle");
        }
    }

    /// py: telegram.py:7199 `def _show(self, state: Optional[str], text: str = "", chirp: bool = True) -> None:`
    ///
    /// The robot acts out what the chat is doing — unless it is playing asleep.
    pub(crate) fn show(self: &Arc<Self>, state: Option<&str>, text: &str, chirp: bool) {
        if self.stealth() {
            return;
        }
        if let Some(state) = state {
            (self.on_state)(state);
        }
        let Some(on_caption) = self.on_caption.as_ref().filter(|_| !text.is_empty()) else { return };
        let lines: Vec<String> = py_textwrap(&one_line(text), 17).into_iter().take(4).collect(); // 4 lines of 17
        if !lines.is_empty() {
            on_caption(json!({"cmd": "caption", "page": 0, "of": 1, "lines": lines, "hold_ms": 5000, "final": true, "chirp": chirp}));
        }
    }

    /// py: telegram.py:7211 `def _on_agent_event(self, ev: AgentEvent, chat_id: Optional[int] = None, progress: Optional[_Progress] = None) -> None:`
    ///
    /// A task's event: the robot acts it out (unless in stealth), and a step ("progress") also goes into
    /// the task's progress message in the chat, whichever provider runs it. Before 2026-09-23 only Codex's
    /// steps reached the chat, one message each; stealth never hid the chat, only the robot.
    pub(crate) fn on_agent_event(self: &Arc<Self>, ev: &eve_agent_contract::AgentEvent, chat_id: Option<i64>, progress: Option<&Arc<_Progress>>) {
        let state = match ev.kind() {
            "started" | "exec" | "commentary" | "turn" => Some("working"),
            "ask" => Some("asking"),
            "final" => Some("done"),
            "error" => Some("error"),
            "cancelled" => Some("idle"),
            _ => None,
        };
        if ev.kind() == "progress" {
            self.show(None, ev.text(), false);
            match (chat_id, progress) {
                (Some(_), Some(progress)) => self.progress_step(Some(progress), ev.text()),
                (Some(chat_id), None) => {
                    let codex = self.st.lock().agent.as_ref().is_some_and(|a| a.provider() == "codex");
                    if codex {
                        // No progress message to edit: a Codex step goes as a message of its own, as before.
                        let (me, text) = (self.clone(), ev.text().to_string());
                        self.spawn(
                            async move { me.say(chat_id, &text, Some("Codex progress"), None, &[], 0, None).await },
                            "telegram-codex-progress",
                        );
                    }
                }
                _ => {}
            }
        } else if let Some(state) = state {
            self.show(Some(state), "", true);
        }
    }

    /// py: telegram.py:7228 `def _open_question(self, future: asyncio.Future, chat_id: Optional[int], *, strict: bool, words: frozenset[str] = frozenset(), firm: bool = False) -> _Question:`
    ///
    /// A new question waiting on the owner: the newest, so the slot names it. When its future is
    /// settled (an answer, a tap, a timeout, its asker gone) the slot moves on at once, not only when the
    /// asker's ``finally`` runs (``_close_question``).
    pub(crate) fn open_question(self: &Arc<Self>, future: &Pending, chat_id: Option<i64>, strict: bool, words: &HashSet<String>, firm: bool) -> Arc<_Question> {
        let asked = Arc::new(_Question {
            future: future.clone(),
            chat_id,
            strict,
            words: words.clone(),
            text: Mutex::new(String::new()),
            firm,
        });
        self.st.lock().questions.push(asked.clone());
        let weak = Arc::downgrade(self);
        future.add_done_callback(move || {
            if let Some(me) = weak.upgrade() {
                me.show_question();
            }
        });
        self.show_question();
        asked
    }

    /// py: telegram.py:7239 `def _close_question(self, asked: _Question) -> None:`
    pub(crate) fn close_question(self: &Arc<Self>, asked: &Arc<_Question>) {
        {
            let mut st = self.st.lock();
            if let Some(i) = st.questions.iter().position(|q| Arc::ptr_eq(q, asked)) {
                st.questions.remove(i);
            }
        }
        self.show_question();
    }

    /// py: telegram.py:7244 `def _show_question(self) -> None:`
    ///
    /// The slot (``_pending_answer`` and the three fields beside it) names the newest open question whose
    /// future is not done, or nothing when none is.
    pub(crate) fn show_question(self: &Arc<Self>) {
        let mut st = self.st.lock();
        let live = st.questions.iter().rev().find(|q| !q.future.done()).cloned();
        match live {
            None => {
                st.pending_answer = None;
                st.pending_answer_chat = None;
                st.pending_strict = false;
                st.pending_words = HashSet::new();
                st.pending_firm = false;
            }
            Some(live) => {
                st.pending_answer = Some(live.future.clone());
                st.pending_answer_chat = live.chat_id;
                st.pending_strict = live.strict;
                st.pending_words = live.words.clone();
                st.pending_firm = live.firm;
            }
        }
    }

    /// py: telegram.py:7255 `async def _ask_user(self, question: str, chat_id: int, title: str = TASK_ASKS_TITLE, choices: Optional[Sequence[Choice]] = None, about: Optional[str] = None, firm: bool = False, read_all: bool = False, on_open: Optional[Callable[[_Question], None]] = None, outside: bool = False) -> str:`
    ///
    /// A question for the owner from a task, an app or Codex. A free question needs words: the owner's
    /// next message is its answer, whatever it says. A question whose answers are known (``choices``, or
    /// what ``answer_choices`` reads off its wording: a yes/no, Codex's app-access choices) also gets
    /// buttons, and a tap is that same answer. Afterwards the question is edited to say what was chosen.
    ///
    /// A question with buttons is strict from the moment it is created, like ``decide_permission``: while a
    /// relay is on, only a tap, a bare yes/no or a button's words typed answer it, and any other text goes
    /// to Claude or Codex as usual. Before this a Composio consent or a Codex "Reply yes or no." took the
    /// owner's next message for Claude as the answer, and "ok, also update the README" ran a Drive delete
    /// (owner, 2026-09-23). Only a send that falls back to plain text makes it non-strict again.
    ///
    /// A question already waiting (a Claude permission prompt, another task's question) gets the slot back
    /// once every question asked after it has ended, in whatever order they end: a typed yes then reaches
    /// it again, instead of going to Claude while the prompt waits out its timeout (review, 2026-09-23;
    /// ``_questions``).
    ///
    /// ``about``: the history's line in place of the question (the chief's Go, whose pick is quoted from the web:
    /// as tell_owner's ``about``, web words never become eve's own words in the history). ``firm``: only a bare
    /// yes or no or a button answers it, relay or not, on a call too (``_Question.firm``). ``outside``: the question's
    /// words are not eve's (a task's or Codex's question, written after reading screens and pages): its history
    /// line is marked, so a later turn that shows it starts tainted (tool_gate.py; v3 safety review).
    ///
    /// Rust: Python's `CancelledError` out of the wait (the question's future cancelled: the asker's slot was
    /// cleared) has no `str` to be; it returns "" here, and [`TelegramInlet::ask_user_inner`] gives it as an error.
    pub(crate) async fn ask_user(self: &Arc<Self>, question: &str, chat_id: i64, title: &str, choices: Option<&[Choice]>, about: Option<&str>, firm: bool, read_all: bool, on_open: Option<OnOpen>, outside: bool) -> String {
        self.ask_user_inner(question, chat_id, title, choices, about, firm, read_all, on_open, outside).await.unwrap_or_default()
    }

    /// `_ask_user` → the answer, or `CancelledError` when the question's future was cancelled while it waited.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn ask_user_inner(self: &Arc<Self>, question: &str, chat_id: i64, title: &str, choices: Option<&[Choice]>, about: Option<&str>, firm: bool, read_all: bool, on_open: Option<OnOpen>, outside: bool) -> Result<String, CancelledError> {
        let future = Pending::new();
        let session = ctx().session;
        let title = if session.is_some() {
            format!("{title} ({SESSION_TITLE})") // the owner sees the question is a Work session's, not the chat's
        } else {
            title.to_string()
        };
        let choices: Vec<Choice> = match choices {
            None => answer_choices(question),
            Some(c) => c.to_vec(),
        };
        // A free question: any next message answers it.
        let words: HashSet<String> = choices.iter().map(|c| c.value.clone()).collect();
        let asked = self.open_question(&future, Some(chat_id), !choices.is_empty(), &words, firm);
        if let Some(on_open) = &on_open {
            on_open(&asked);
        }
        *asked.text.lock() = question.to_string();
        let asked = Arc::new(Mutex::new(asked));
        match about {
            None => self.note("eve", question, "say", None, None, None, outside, None),
            Some(about) => self.note("eve", about, "say", None, None, None, false, None),
        }
        let board = (!choices.is_empty()).then(|| {
            Arc::new(_Keyboard {
                chat_id: Some(chat_id),
                kind: ANSWER.into(),
                choices: choices.clone(),
                future: Some(future.clone()),
                valid: None,
                on_stop: None,
                keys: Mutex::new(Vec::new()),
                message_id: Mutex::new(0),
            })
        });
        // Waiting on the owner is not working: no "typing…" under the question. It comes back with the answer,
        // except a reason that ended while the question waited (_stop_typing drops it from the paused set).
        // Only the outermost question in a chat owns the paused set: a question asked under another one adds
        // to it and resumes nothing, so no "typing…" shows under the outer question still waiting (review,
        // 2026-09-23).
        let owner = !self.st.lock().paused_typing.contains_key(&chat_id);
        self.st.lock().paused_typing.entry(chat_id).or_default();
        let ended = self.end_typing(chat_id);
        self.st.lock().paused_typing.entry(chat_id).or_default().extend(ended);
        // What the question is edited to say. A cut-off asker (the future dropped: a stopped task, Chrome's dialog
        // answered at the Mac, a restart) took nothing as a no, so the question must not say so (review, 2026-09-23).
        let line = Arc::new(Mutex::new(ASK_CLOSED_LINE.to_string()));
        let _finally = {
            let (me, asked, board, line, question, title) =
                (self.clone(), asked.clone(), board.clone(), line.clone(), question.to_string(), title.clone());
            on_drop(move || {
                let asked = asked.lock().clone();
                me.close_question(&asked);
                if let Some(board) = board {
                    me.retire(Some(&board), false);
                    // a restart's buttons answer "expired" anyway
                    if !me.st.lock().stopping && tokio::runtime::Handle::try_current().is_ok() {
                        let (settling, line) = (me.clone(), line.lock().clone());
                        me.spawn(
                            async move { settling.settle_prompt(&board, &question, &line, Some(&title), None).await },
                            "telegram-edit",
                        );
                    }
                }
                if owner {
                    let paused = me.st.lock().paused_typing.remove(&chat_id).unwrap_or_default();
                    for (reason, ticks) in paused {
                        me.keep_typing(Some(chat_id), &reason, ticks as f64 * TYPING_EVERY_SECS);
                    }
                }
            })
        };
        match &board {
            Some(board) => {
                let per_row = if choices.len() == 2 { 2 } else { 1 };
                let buttons = self.send_choices(Some(chat_id), question, board, Some(&title), None, &[], per_row, read_all).await;
                if !buttons && !future.done() {
                    // plain text after all: the next message answers
                    let loose = self.loosen_question(&asked.lock().clone());
                    *asked.lock() = loose;
                    self.show_question();
                }
            }
            None => {
                // A free question opens the reply box on itself (ForceReply): the next message is its answer.
                self.say(chat_id, question, Some(&title), None, &[], 0, Some(ASK_PLACEHOLDER)).await;
            }
        }
        let timeout = self.config.ask_timeout_secs;
        match tokio::time::timeout(std::time::Duration::from_secs_f64(timeout.max(0.0)), future.wait()).await {
            Ok(Ok(answer)) => {
                let said = answer.trim_matches(eve_envfile::is_py_space).to_lowercase();
                let said = said.trim_end_matches(['.', '!']);
                *line.lock() = choices
                    .iter()
                    .find(|c| c.value == said)
                    .map(|c| if c.done.is_empty() { c.label.clone() } else { c.done.clone() })
                    .unwrap_or_else(|| ANSWERED_LINE.to_string());
                Ok(answer)
            }
            Ok(Err(cancelled)) => Err(cancelled), // the line stays ASK_CLOSED_LINE, as Python's CancelledError
            Err(_) => {
                future.cancel(); // wait_for cancels what it waited on
                *line.lock() = UNANSWERED_LINE.to_string();
                self.say(chat_id, UNANSWERED_LINE, None, None, &[], 0, None).await;
                if let Some(session) = session {
                    session.lock().unanswered = true; // session_call reports OWNER_NO_ANSWER, not a plain refusal
                }
                Ok(format!("no (no answer within {} seconds)", timeout as i64))
            }
        }
    }
}

/// `on_open(asked)`: told the question as it opens (`_ask_user`'s `on_open`).
pub type OnOpen = Arc<dyn Fn(&Arc<_Question>) + Send + Sync>;

/// What this unit's tests reach that Python's tests read or set as private attributes (`rig.inlet._relay_to(...)`,
/// `rig.inlet._chat_id = OWNER`, `await asyncio.gather(*rig.inlet._jobs)`). Not part of the inlet's API.
#[doc(hidden)]
pub struct SayRelayProbe(Arc<TelegramInlet>);

impl TelegramInlet {
    #[doc(hidden)]
    pub fn say_relay_probe(self: &Arc<Self>) -> SayRelayProbe {
        SayRelayProbe(self.clone())
    }
}

#[doc(hidden)]
impl SayRelayProbe {
    /// `inlet._dispatch(update)`.
    pub fn dispatch(&self, update: &Value) {
        self.0.dispatch(update);
    }

    /// `await asyncio.gather(*list(inlet._jobs))`.
    pub async fn jobs(&self) {
        let jobs = self.0.st.lock().jobs.clone();
        for job in jobs {
            job.join().await;
        }
    }

    /// `inlet._relay_to(cwd)`.
    pub fn relay_to(&self, cwd: &str) {
        self.0.relay_to(cwd);
    }

    /// `inlet._chat_id = chat_id`.
    pub fn set_chat(&self, chat_id: Option<i64>) {
        self.0.st.lock().chat_id = chat_id;
    }

    /// `inlet.claude = on`.
    pub fn set_claude(&self, on: bool) {
        self.0.st.lock().claude = on;
    }

    /// `await inlet._shutdown()`.
    pub async fn shutdown(&self) {
        self.0.shutdown().await;
    }

    /// `inlet._taps == {}`.
    pub fn taps_empty(&self) -> bool {
        self.0.st.lock().taps.is_empty()
    }

    /// `inlet._relay_pin`.
    pub fn relay_pin(&self) -> String {
        self.0.st.lock().relay_pin.clone()
    }

    /// `inlet._launch is not None`.
    pub fn launch_open(&self) -> bool {
        self.0.st.lock().launch.is_some()
    }

    /// `inlet._awaiting_answer`.
    pub fn awaiting_answer(&self) -> bool {
        self.0.awaiting_answer()
    }

    /// `inlet._permission_future is not None`.
    pub fn permission_pending(&self) -> bool {
        self.0.st.lock().permission_future.is_some()
    }

    /// `inlet._pending_strict`, `inlet._pending_words`.
    pub fn pending(&self) -> (bool, HashSet<String>) {
        let st = self.0.st.lock();
        (st.pending_strict, st.pending_words.clone())
    }

    /// `inlet._tap_gen`.
    pub fn tap_gen(&self) -> String {
        self.0.tap_gen.clone()
    }

    /// `board = _Keyboard(OWNER, ANSWER, list(ALLOW_DENY)); inlet._register(board)` → `board.keys`.
    pub fn register_allow_deny(&self, chat_id: i64) -> Vec<String> {
        let board = Arc::new(_Keyboard {
            chat_id: Some(chat_id),
            kind: ANSWER.into(),
            choices: super::relay::relay_util::allow_deny().to_vec(),
            future: None,
            valid: None,
            on_stop: None,
            keys: Mutex::new(Vec::new()),
            message_id: Mutex::new(0),
        });
        self.0.register(&board);
        let keys = board.keys.lock().clone();
        keys
    }

    /// `await inlet._ask_user(question, chat_id, title=...)`.
    pub async fn ask_user(&self, question: &str, chat_id: i64, title: Option<&str>) -> String {
        self.0.ask_user(question, chat_id, title.unwrap_or(TASK_ASKS_TITLE), None, None, false, false, None, false).await
    }

    /// `inlet._codex_chat`.
    pub fn codex_chat(&self) -> Option<i64> {
        self.0.st.lock().codex_chat
    }

    /// `inlet._codex_chat = chat_id`.
    pub fn set_codex_chat(&self, chat_id: Option<i64>) {
        self.0.st.lock().codex_chat = chat_id;
    }

    /// `inlet._codex_progress.shown`, None while no Codex progress message is open.
    pub fn codex_progress_shown(&self) -> Option<String> {
        let progress = self.0.st.lock().codex_progress.clone();
        progress.map(|p| p.st.lock().shown.clone())
    }

    /// `inlet._launch_step(chat_id, step)`.
    pub async fn launch_step(&self, chat_id: i64, step: &eve_claude_launch::Step) {
        self.0.launch_step(chat_id, step).await;
    }

    /// `await inlet._type_to_claude(chat_id, text, message_id)`.
    pub async fn type_to_claude(&self, chat_id: i64, text: &str, message_id: i64) -> bool {
        self.0.type_to_claude(chat_id, text, message_id, false).await
    }

    /// `await inlet._send_file(chat_id, raw, caption)`.
    pub async fn send_file(&self, chat_id: i64, raw: &str, caption: &str) -> Obj {
        self.0.send_file(chat_id, raw, caption).await
    }

    /// `inlet._join_after_launch = on`.
    pub fn set_join_after_launch(&self, on: bool) {
        self.0.st.lock().join_after_launch = on;
    }

    /// `inlet._progress_step(inlet._task_progress, line)`.
    pub fn task_step(&self, line: &str) {
        let progress = self.0.st.lock().task_progress.clone();
        self.0.progress_step(progress.as_ref(), line);
    }
}
