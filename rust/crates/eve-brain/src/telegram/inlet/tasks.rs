//! A port of `telegram.py`, lines 7078-7339: computer tasks, agent events, questions to the owner.
//!
//! Skeleton: constants and prompts are verbatim (generated from the Python values); bodies are
//! `todo!()` until this file's unit fills them.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

impl TelegramInlet {
    /// py: telegram.py:7078 `def _start_task(self, goal: str, chat_id: int, *, card: Optional[tuple[str, int]] = None, floor: Optional[str] = None, display_goal: Optional[str] = None) -> dict[str, Any]:`
    ///
    /// A computer task. ``card`` (card id, step) makes it a chief's step (``chief_start``): its goal is the
    /// chief's own, exactly as the owner's Go showed it, so no link from the chat is added. ``floor`` selects
    /// an explicit executor ("codex" for a chief's act or OpenTable's browser search). ``display_goal`` is
    /// the owner's short request when ``goal`` includes fixed integration instructions. A chief's result
    /// goes to the chief, which sends the receipt, in place of "Task result".
    pub(crate) fn start_task(self: &Arc<Self>, goal: &str, chat_id: i64, card: Option<(String, i64)>, floor: Option<&str>, display_goal: Option<&str>) -> Obj {
        todo!("telegram.py:7078 TelegramInlet._start_task")
    }

    /// py: telegram.py:7137 `async def _run_agent(self, goal: str, chat_id: int, progress: Optional[_Progress] = None, *, card: Optional[tuple[str, int]] = None, steps: Sequence[str] = (), broke: Sequence[str] = (), display_goal: Optional[str] = None) -> None:`
    pub(crate) async fn run_agent(self: &Arc<Self>, goal: &str, chat_id: i64, progress: Option<&Arc<_Progress>>, card: Option<(String, i64)>, steps: &[String], broke: &[String], display_goal: Option<&str>) {
        todo!("telegram.py:7137 TelegramInlet._run_agent")
    }

    /// py: telegram.py:7194 `async def _settle_board(self) -> None:`
    pub(crate) async fn settle_board(self: &Arc<Self>) {
        todo!("telegram.py:7194 TelegramInlet._settle_board")
    }

    /// py: telegram.py:7199 `def _show(self, state: Optional[str], text: str = "", chirp: bool = True) -> None:`
    ///
    /// The robot acts out what the chat is doing — unless it is playing asleep.
    pub(crate) fn show(self: &Arc<Self>, state: Option<&str>, text: &str, chirp: bool) {
        todo!("telegram.py:7199 TelegramInlet._show")
    }

    /// py: telegram.py:7211 `def _on_agent_event(self, ev: AgentEvent, chat_id: Optional[int] = None, progress: Optional[_Progress] = None) -> None:`
    ///
    /// A task's event: the robot acts it out (unless in stealth), and a step ("progress") also goes into
    /// the task's progress message in the chat, whichever provider runs it. Before 2026-09-23 only Codex's
    /// steps reached the chat, one message each; stealth never hid the chat, only the robot.
    pub(crate) fn on_agent_event(self: &Arc<Self>, ev: &eve_agent_contract::AgentEvent, chat_id: Option<i64>, progress: Option<&Arc<_Progress>>) {
        todo!("telegram.py:7211 TelegramInlet._on_agent_event")
    }

    /// py: telegram.py:7228 `def _open_question(self, future: asyncio.Future, chat_id: Optional[int], *, strict: bool, words: frozenset[str] = frozenset(), firm: bool = False) -> _Question:`
    ///
    /// A new question waiting on the owner: the newest, so the slot names it. When its future is
    /// settled (an answer, a tap, a timeout, its asker gone) the slot moves on at once, not only when the
    /// asker's ``finally`` runs (``_close_question``).
    pub(crate) fn open_question(self: &Arc<Self>, future: &Pending, chat_id: Option<i64>, strict: bool, words: &HashSet<String>, firm: bool) -> Arc<_Question> {
        todo!("telegram.py:7228 TelegramInlet._open_question")
    }

    /// py: telegram.py:7239 `def _close_question(self, asked: _Question) -> None:`
    pub(crate) fn close_question(self: &Arc<Self>, asked: &Arc<_Question>) {
        todo!("telegram.py:7239 TelegramInlet._close_question")
    }

    /// py: telegram.py:7244 `def _show_question(self) -> None:`
    ///
    /// The slot (``_pending_answer`` and the three fields beside it) names the newest open question whose
    /// future is not done, or nothing when none is.
    pub(crate) fn show_question(self: &Arc<Self>) {
        todo!("telegram.py:7244 TelegramInlet._show_question")
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
    pub(crate) async fn ask_user(self: &Arc<Self>, question: &str, chat_id: i64, title: &str, choices: Option<&[Choice]>, about: Option<&str>, firm: bool, read_all: bool, on_open: Option<OnOpen>, outside: bool) -> String {
        todo!("telegram.py:7255 TelegramInlet._ask_user")
    }
}

/// `on_open(asked)`: told the question as it opens (`_ask_user`'s `on_open`).
pub type OnOpen = Arc<dyn Fn(&Arc<_Question>) + Send + Sync>;
