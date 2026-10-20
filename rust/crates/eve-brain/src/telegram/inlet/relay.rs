//! A port of `telegram.py`, lines 4118-4773: the Codex chat, new coding sessions, the Claude relay, stop.
//!
//! Skeleton: constants and prompts are verbatim (generated from the Python values); bodies are
//! `todo!()` until this file's unit fills them.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

impl TelegramInlet {
    /// py: telegram.py:4118 `async def _codex_command(self, chat_id: int, action: str, selection: Optional[str], epoch: int) -> None:`
    pub(crate) async fn codex_command(self: &Arc<Self>, chat_id: i64, action: &str, selection: Option<&str>, epoch: i64) {
        todo!("telegram.py:4118 TelegramInlet._codex_command")
    }

    /// py: telegram.py:4193 `async def _end_codex_progress(self, line: str) -> None:`
    pub(crate) async fn end_codex_progress(self: &Arc<Self>, line: &str) {
        todo!("telegram.py:4193 TelegramInlet._end_codex_progress")
    }

    /// py: telegram.py:4197 `async def _codex_send(self, chat_id: int, text: str, epoch: int, *, interrupt: bool = False, message_id: int = 0) -> bool:`
    ///
    /// Send the owner's message to the Codex chat, or interrupt its turn → True when a message went to
    /// Codex. A message that starts a turn gets the turn's progress message ("Sent to Codex.", then its
    /// steps, with a Stop button); one sent while a turn runs steers it and is confirmed by a 👍 on the
    /// owner's message, or by the line when the reaction fails.
    pub(crate) async fn codex_send(self: &Arc<Self>, chat_id: i64, text: &str, epoch: i64, interrupt: bool, message_id: i64) -> bool {
        todo!("telegram.py:4197 TelegramInlet._codex_send")
    }

    /// py: telegram.py:4258 `def _new_launch(self) -> claude_launch.LaunchFlow:`
    pub(crate) fn new_launch(self: &Arc<Self>) -> eve_claude_launch::LaunchFlow {
        todo!("telegram.py:4258 TelegramInlet._new_launch")
    }

    /// py: telegram.py:4262 `def _launch_dispatch(self, inbound: Inbound, word: str) -> bool:`
    ///
    /// "new claude" starts the tree; while it is open, the owner's next texts answer it, before the relays
    /// and the model. Each step is decided here, synchronously, so two quick replies cannot interleave.
    pub(crate) fn launch_dispatch(self: &Arc<Self>, inbound: &Inbound, word: &str) -> bool {
        todo!("telegram.py:4262 TelegramInlet._launch_dispatch")
    }

    /// py: telegram.py:4286 `async def _launch_step(self, chat_id: int, step: claude_launch.Step) -> None:`
    pub(crate) async fn launch_step(self: &Arc<Self>, chat_id: i64, step: &eve_claude_launch::Step) {
        todo!("telegram.py:4286 TelegramInlet._launch_step")
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
        todo!("telegram.py:4322 TelegramInlet._claude_on")
    }

    /// py: telegram.py:4342 `async def _claude_on_listed(self, inbound: Inbound, target: str, listed: Awaitable[Any], ask_to_start: bool = False) -> None:`
    pub(crate) async fn claude_on_listed(self: &Arc<Self>, inbound: &Inbound, target: &str, listed: futures::future::BoxFuture<'static, anyhow::Result<Vec<String>>>, ask_to_start: bool) {
        todo!("telegram.py:4342 TelegramInlet._claude_on_listed")
    }

    /// py: telegram.py:4366 `def _claude_on_pick(self, inbound: Inbound, target: str, sessions: Any, ask_to_start: bool = False) -> None:`
    pub(crate) fn claude_on_pick(self: &Arc<Self>, inbound: &Inbound, target: &str, sessions: &Value, ask_to_start: bool) {
        todo!("telegram.py:4366 TelegramInlet._claude_on_pick")
    }

    /// py: telegram.py:4394 `def _offer_start(self, chat_id: int, target: str, cwds: list[str], names: list[str]) -> None:`
    ///
    /// claude_relay (the brain's tool) named a session that is not running, or none runs: a new session opens
    /// Claude Code with every permission granted (claude_launch), so it is the owner's tap, never the brain's
    /// call (review, 2026-10-08). The picker offers the running sessions and one button to start; a tap sends the
    /// typed command's own words, so the owner's tap runs exactly what typing them would.
    pub(crate) fn offer_start(self: &Arc<Self>, chat_id: i64, target: &str, cwds: &[String], names: &[String]) {
        todo!("telegram.py:4394 TelegramInlet._offer_start")
    }

    /// py: telegram.py:4411 `def _relay_to(self, cwd: str) -> None:`
    pub(crate) fn relay_to(self: &Arc<Self>, cwd: &str) {
        todo!("telegram.py:4411 TelegramInlet._relay_to")
    }

    /// py: telegram.py:4421 `def relay_session_started(self, cwd: str) -> None:`
    ///
    /// Claude Code's SessionStart hook: a session in ``cwd`` is up. The one eve just opened and joined can now be
    /// typed into.
    pub fn relay_session_started(self: &Arc<Self>, cwd: &str) {
        todo!("telegram.py:4421 TelegramInlet.relay_session_started")
    }

    /// py: telegram.py:4428 `async def _await_ready(self) -> None:`
    ///
    /// Wait for a just-opened session to start before typing into it (owner, 2026-10-10: the first message
    /// after a new claude was typed a second before Claude Code was listening, and was lost). A session that never
    /// says so is typed into after CLAUDE_READY_SECS anyway, as before.
    pub(crate) async fn await_ready(self: &Arc<Self>) {
        todo!("telegram.py:4428 TelegramInlet._await_ready")
    }

    /// py: telegram.py:4445 `def _join(self, chat_id: int, cwd: str) -> None:`
    pub(crate) fn join(self: &Arc<Self>, chat_id: i64, cwd: &str) {
        todo!("telegram.py:4445 TelegramInlet._join")
    }

    /// py: telegram.py:4450 `def _start_then_join(self, chat_id: int, argument: str, intro: str = "") -> None:`
    pub(crate) fn start_then_join(self: &Arc<Self>, chat_id: i64, argument: &str, intro: &str) {
        todo!("telegram.py:4450 TelegramInlet._start_then_join")
    }

    /// py: telegram.py:4464 `async def _type_to_claude(self, chat_id: int, text: str, message_id: int = 0, *, tapped: bool = False) -> bool:`
    ///
    /// Type one line into the joined session → True when it went in. ``tapped``: an option button's
    /// number, whose tap was already answered, so no reaction or "Typed." follows it.
    pub(crate) async fn type_to_claude(self: &Arc<Self>, chat_id: i64, text: &str, message_id: i64, tapped: bool) -> bool {
        todo!("telegram.py:4464 TelegramInlet._type_to_claude")
    }

    /// py: telegram.py:4506 `async def _receipt(self, chat_id: int, message_id: int, emoji: str, fallback: str = "") -> bool:`
    ///
    /// A reaction on the owner's own message in place of a one-line acknowledgement → True when it was
    /// set. When it cannot be (no message id to put it on, Telegram refused it, the network failed), the
    /// ``fallback`` line is sent instead, so the owner is never left without the receipt; with no fallback
    /// the failure is only logged (a 👀 is a status light, not something to confirm). Never raises.
    pub(crate) async fn receipt(self: &Arc<Self>, chat_id: i64, message_id: i64, emoji: &str, fallback: &str) -> bool {
        todo!("telegram.py:4506 TelegramInlet._receipt")
    }

    /// py: telegram.py:4531 `def relay_line(self, body: str, title: str = CLAUDE_TITLE, subtitle: str = "", options: Sequence[Choice] = ()) -> None:`
    ///
    /// One message for the phone (what Claude said, a question it asks), batched with its neighbours:
    /// a burst of short messages under the same title is one text, not ten (``_flush_relay``). ``options``
    /// become buttons under it that type their value into the terminal (a question's numbered options).
    pub fn relay_line(self: &Arc<Self>, body: &str, title: &str, subtitle: &str, options: &[Choice]) {
        todo!("telegram.py:4531 TelegramInlet.relay_line")
    }

    /// py: telegram.py:4547 `async def _flush_relay(self) -> None:`
    pub(crate) async fn flush_relay(self: &Arc<Self>) {
        todo!("telegram.py:4547 TelegramInlet._flush_relay")
    }

    /// py: telegram.py:4581 `def relay_tool_call(self, tool: str, hint: str) -> None:`
    ///
    /// A tool call the daemon saw. Only a question for the owner (AskUserQuestion) reaches the phone:
    /// the terminal's gray lines, the call itself and its result tail, stay on the Mac (owner, 2026-09-21).
    pub fn relay_tool_call(self: &Arc<Self>, tool: &str, hint: &str) {
        todo!("telegram.py:4581 TelegramInlet.relay_tool_call")
    }

    /// py: telegram.py:4594 `async def relay_text(self, text: str, cwd: str = "") -> None:`
    ///
    /// What Claude Code just said, when the relay is on, with its paragraphs and lists as it wrote
    /// them (telegram_format turns the markdown into Telegram's own bold, bullets and code). Never
    /// logged here either.
    pub async fn relay_text(self: &Arc<Self>, text: &str, cwd: &str) {
        todo!("telegram.py:4594 TelegramInlet.relay_text")
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
        todo!("telegram.py:4623 TelegramInlet.relay_turn_began")
    }

    /// py: telegram.py:4642 `def relay_turn_ended(self, cwd: str = "") -> None:`
    ///
    /// Claude Code's Stop hook: a turn ended. For the joined session, "Thinking…" (or "typing…") stops
    /// now, even when the turn ended without a word for the phone. Another session's end changes nothing, and
    /// neither does the end of a session eve does not know (the daemon passes no folder): with a session
    /// picked, a Stop from anywhere else used to end its "Thinking…" early (review, 2026-09-23).
    pub fn relay_turn_ended(self: &Arc<Self>, cwd: &str) {
        todo!("telegram.py:4642 TelegramInlet.relay_turn_ended")
    }

    /// py: telegram.py:4659 `async def relay_notification(self, kind: str, message: str, waits: bool) -> None:`
    pub async fn relay_notification(self: &Arc<Self>, kind: &str, message: &str, waits: bool) {
        todo!("telegram.py:4659 TelegramInlet.relay_notification")
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
        todo!("telegram.py:4671 TelegramInlet.decide_permission")
    }

    /// py: telegram.py:4730 `async def _stop_task(self, progress: _Progress, chat_id: int) -> None:`
    ///
    /// The Stop button on a task's progress message: that task stops, exactly as "stop" typed outside a
    /// Codex chat. A Codex chat that is on is left alone; the button belongs to the task.
    pub(crate) async fn stop_task(self: &Arc<Self>, progress: &Arc<_Progress>, chat_id: i64) {
        todo!("telegram.py:4730 TelegramInlet._stop_task")
    }

    /// py: telegram.py:4738 `def _route_stop(self, chat_id: int) -> None:`
    pub(crate) fn route_stop(self: &Arc<Self>, chat_id: i64) {
        todo!("telegram.py:4738 TelegramInlet._route_stop")
    }

    /// py: telegram.py:4753 `async def _stop(self, chat_id: int, purchase_stop: Optional[dict[str, Any]] = None) -> None:`
    pub(crate) async fn stop(self: &Arc<Self>, chat_id: i64, purchase_stop: Option<&Obj>) {
        todo!("telegram.py:4753 TelegramInlet._stop")
    }
}
