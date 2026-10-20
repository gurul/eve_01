//! A port of `telegram.py`, lines 2884-3262: the poll loop, shutdown, the menu, typing and drafts, spawn, app hand-off.
//!
//! Skeleton: constants and prompts are verbatim (generated from the Python values); bodies are
//! `todo!()` until this file's unit fills them.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

impl TelegramInlet {
    /// py: telegram.py:2884 `async def run(self) -> None:`
    pub async fn run(self: &Arc<Self>) {
        todo!("telegram.py:2884 TelegramInlet.run")
    }

    /// py: telegram.py:2925 `async def _shutdown(self) -> None:`
    pub(crate) async fn shutdown(self: &Arc<Self>) {
        todo!("telegram.py:2925 TelegramInlet._shutdown")
    }

    /// py: telegram.py:2980 `def menu_commands(self) -> tuple[tuple[str, str], ...]:`
    ///
    /// The / menu as this inlet runs: BOT_COMMANDS, plus /jobs while the chief is on, /america and /opentable
    /// while they are on, each before /stop. Telegram's menu is set from it (_set_commands).
    pub fn menu_commands(self: &Arc<Self>) -> Vec<(String, String)> {
        todo!("telegram.py:2980 TelegramInlet.menu_commands")
    }

    /// py: telegram.py:2991 `async def _set_commands(self) -> None:`
    ///
    /// eve's code words in the / menu of each owner's private chat (a private chat's id is its user's
    /// id). Once per start, and fail-soft: a menu Telegram refuses leaves every word working when typed.
    pub(crate) async fn set_commands(self: &Arc<Self>) {
        todo!("telegram.py:2991 TelegramInlet._set_commands")
    }

    /// py: telegram.py:3011 `def _keep_typing(self, chat_id: Optional[int], reason: str, secs: float) -> None:`
    ///
    /// Show "typing…" in this chat for ``reason`` for about ``secs`` (in 4 s ticks), until
    /// ``_stop_typing`` with the same reason. A second call for a reason already shown extends it, never
    /// doubles it: one loop per chat, one sendChatAction per tick, whatever the number of reasons. Not for work
    /// an app turn began (``_FROM_APP``): the owner is in Eve Link, which shows its own, and a "typing…" in a
    /// Telegram chat nobody wrote in would only puzzle them.
    pub(crate) fn keep_typing(self: &Arc<Self>, chat_id: Option<i64>, reason: &str, secs: f64) {
        todo!("telegram.py:3011 TelegramInlet._keep_typing")
    }

    /// py: telegram.py:3026 `def _stop_typing(self, chat_id: Optional[int], reason: str) -> None:`
    ///
    /// Drop one reason; with none left, the loop ends now, so no "typing…" follows the reply. A reason
    /// paused under a question to the owner is dropped as well: it is over, and must not resume after the
    /// answer (review, 2026-09-23: the relay's "typing…" came back for minutes after Claude had finished).
    pub(crate) fn stop_typing(self: &Arc<Self>, chat_id: Option<i64>, reason: &str) {
        todo!("telegram.py:3026 TelegramInlet._stop_typing")
    }

    /// py: telegram.py:3040 `def _end_typing(self, chat_id: int) -> dict[str, int]:`
    ///
    /// Every reason at once, returned so ``_ask_user`` can put them back after the owner answers.
    pub(crate) fn end_typing(self: &Arc<Self>, chat_id: i64) -> HashMap<String, i64> {
        todo!("telegram.py:3040 TelegramInlet._end_typing")
    }

    /// py: telegram.py:3048 `async def _typing_loop(self, chat_id: int) -> None:`
    pub(crate) async fn typing_loop(self: &Arc<Self>, chat_id: i64) {
        todo!("telegram.py:3048 TelegramInlet._typing_loop")
    }

    /// py: telegram.py:3073 `def _start_thinking(self, chat_id: Optional[int]) -> None:`
    ///
    /// A line went into Claude's terminal: show the "Thinking…" draft until Claude asks, waits on the
    /// owner or ends its turn (``_stop_thinking``), for TYPING_RELAY_SECS at most. With drafts off, or once
    /// Telegram has refused one, this is the relay's "typing…" exactly as before.
    pub(crate) fn start_thinking(self: &Arc<Self>, chat_id: Option<i64>) {
        todo!("telegram.py:3073 TelegramInlet._start_thinking")
    }

    /// py: telegram.py:3088 `def _stop_thinking(self, chat_id: Optional[int]) -> None:`
    ///
    /// The turn is not working any more (it asks, waits, ended, or the relay went off): no bubble is sent
    /// again. One already on the screen goes when the next message arrives, or by itself within 30 s.
    pub(crate) fn stop_thinking(self: &Arc<Self>, chat_id: Option<i64>) {
        todo!("telegram.py:3088 TelegramInlet._stop_thinking")
    }

    /// py: telegram.py:3098 `def _thinking(self) -> bool:`
    pub(crate) fn thinking(self: &Arc<Self>) -> bool {
        todo!("telegram.py:3098 TelegramInlet._thinking")
    }

    /// py: telegram.py:3101 `async def _draft_loop(self, chat_id: int, draft_id: int) -> None:`
    pub(crate) async fn draft_loop(self: &Arc<Self>, chat_id: i64, draft_id: i64) {
        todo!("telegram.py:3101 TelegramInlet._draft_loop")
    }

    /// py: telegram.py:3112 `async def _show_draft(self, chat_id: int, draft_id: int) -> bool:`
    ///
    /// One "Thinking…" placeholder → False when Telegram refused it. A refusal turns drafts off for the
    /// process and hands the time left to the relay's "typing…", so the owner still sees work going on.
    pub(crate) async fn show_draft(self: &Arc<Self>, chat_id: i64, draft_id: i64) -> bool {
        todo!("telegram.py:3112 TelegramInlet._show_draft")
    }

    /// py: telegram.py:3129 `def _spawn(self, coro: Awaitable[None], name: str) -> asyncio.Task:`
    pub(crate) fn spawn(self: &Arc<Self>, coro: impl std::future::Future<Output = ()> + Send + 'static, name: &str) -> Job {
        todo!("telegram.py:3129 TelegramInlet._spawn")
    }

    /// py: telegram.py:3142 `def app_filed(self, turn: str) -> Optional[str]:`
    ///
    /// The id of the card Eve Link app turn ``turn`` filed through take_on (its first), or None.
    pub fn app_filed(self: &Arc<Self>, turn: &str) -> Option<String> {
        todo!("telegram.py:3142 TelegramInlet.app_filed")
    }

    /// py: telegram.py:3146 `def owner_busy(self) -> bool:`
    ///
    /// A turn the owner started is in flight: a text turn waiting for the lock or running, or the work an app
    /// turn started that answers within it (link_chat.py: the app's stream ends, and its ``busy`` clears, once
    /// this is False). Synchronous and cheap: two set sizes.
    pub fn owner_busy(self: &Arc<Self>) -> bool {
        todo!("telegram.py:3146 TelegramInlet.owner_busy")
    }

    /// py: telegram.py:3152 `def waiting_on_owner(self) -> bool:`
    ///
    /// A question is waiting on the owner's answer (a chief's Go, an approval, a task's question). The app's
    /// ``busy`` is False meanwhile (link_chat ``_messages``): waiting on the owner is not working, so no typing
    /// dots under the question. ``owner_busy`` itself is unchanged, since a stream's end is decided by it and a
    /// new request typed while a question waits must keep its stream open.
    pub fn waiting_on_owner(self: &Arc<Self>) -> bool {
        todo!("telegram.py:3152 TelegramInlet.waiting_on_owner")
    }

    /// py: telegram.py:3160 `def transcripts(self) -> Any:`
    ///
    /// The transcript store this chat writes (memory.Memory.transcripts), or None without a Memory.
    pub fn transcripts(self: &Arc<Self>) -> Value {
        todo!("telegram.py:3160 TelegramInlet.transcripts")
    }

    /// py: telegram.py:3164 `def _to_app(self, chat_id: Optional[int], text: str, kind: str = "say") -> bool:`
    ///
    /// One of eve's lines, for the Eve Link app turn this work began on (``_FROM_APP``) → whether the app
    /// has it. False (not an app turn, no link, or nobody reading) leaves it to Telegram, so nothing is lost.
    pub(crate) fn to_app(self: &Arc<Self>, chat_id: Option<i64>, text: &str, kind: &str) -> bool {
        todo!("telegram.py:3164 TelegramInlet._to_app")
    }

    /// py: telegram.py:3176 `def _dispatch(self, update: Any) -> None:`
    ///
    /// Decide one update and return at once: a turn is a job, so a slow one never stops the poll.
    pub(crate) fn dispatch(self: &Arc<Self>, update: &Value) {
        todo!("telegram.py:3176 TelegramInlet._dispatch")
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
        todo!("telegram.py:3211 TelegramInlet._answers_pending")
    }

    /// py: telegram.py:3235 `def _strict_answer(self, text: str) -> str:`
    ///
    /// What a typed answer hands the waiting question. A prompt with buttons takes a bare yes or no in
    /// any wording ("ok", "sure", "nope"): it is handed on as the buttons' own "yes" or "no", so the words
    /// the owner typed do what a tap does. A Codex command's prompt accepts only "yes" (ONCE_ANSWERS), and
    /// a typed "ok" used to decline it (review, 2026-09-23). Any other answer goes on as it was typed.
    pub(crate) fn strict_answer(self: &Arc<Self>, text: &str) -> String {
        todo!("telegram.py:3235 TelegramInlet._strict_answer")
    }

    /// py: telegram.py:3245 `def _image_waits(self) -> bool:`
    ///
    /// An image cannot answer a question, so while one waits on a typed answer the image is refused.
    /// That is every question with no relay on (the next message is its answer, buttons or not), and a
    /// question without buttons with one on. A prompt with buttons while a relay takes the chat's words
    /// lets the image through to the relay, as it lets other text through.
    pub(crate) fn image_waits(self: &Arc<Self>) -> bool {
        todo!("telegram.py:3245 TelegramInlet._image_waits")
    }

    /// py: telegram.py:3254 `def _defer_permission(self) -> None:`
    ///
    /// The relay is going off, or to another session: a permission prompt still waiting ends as "no
    /// answer here", so the dialog on the Mac decides and the prompt is edited to say so. Without this the
    /// prompt stayed pending for minutes after "claude off", and the owner's next message, meant for eve,
    /// could be taken as its answer: "ok, what's on my calendar" allowed an rm (owner, 2026-09-23).
    pub(crate) fn defer_permission(self: &Arc<Self>) {
        todo!("telegram.py:3254 TelegramInlet._defer_permission")
    }
}
