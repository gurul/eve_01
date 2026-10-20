//! A port of `telegram.py`, lines 3263-3714: one inbound message: code words, switches, quick answers, images.
//!
//! Skeleton: constants and prompts are verbatim (generated from the Python values); bodies are
//! `todo!()` until this file's unit fills them.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

impl TelegramInlet {
    /// py: telegram.py:3263 `def _handle(self, inbound: Inbound) -> None:`
    ///
    /// One accepted message from the owner, routed. A picker's tap comes here too, with the button's
    /// words as its text (``_on_tap``), so a tap and a typed reply do exactly the same thing. In eve lite a code
    /// word of a feature it does not have is answered first (``_lite_code_word``).
    pub(crate) fn handle(self: &Arc<Self>, inbound: &Inbound) {
        todo!("telegram.py:3263 TelegramInlet._handle")
    }

    /// py: telegram.py:3481 `def _lite_code_word(self, inbound: Inbound) -> bool:`
    ///
    /// eve lite: the code word of a feature eve lite does not have (the Claude Code relay, a new coding session,
    /// Codex, stealth, the Mini App, pairing the phone app, the screen, Meet, /america, OpenTable, the chief's
    /// jobs) is answered LITE_NOT_HERE by code, and nothing else happens. True when it was one. Stop, spend and
    /// watch words, quick answers and every other message go on as in full eve, as does a message with an
    /// attachment, which is never a code word.
    pub(crate) fn lite_code_word(self: &Arc<Self>, inbound: &Inbound) -> bool {
        todo!("telegram.py:3481 TelegramInlet._lite_code_word")
    }

    /// py: telegram.py:3508 `def _codex_switch(self, chat_id: int, action: str, selection: Optional[str]) -> bool:`
    ///
    /// "codex ..." as _handle routes it: the relay's state flips now, on the loop, and the command runs as a
    /// job. False (nothing done) while another owner chat holds the Codex relay.
    pub(crate) fn codex_switch(self: &Arc<Self>, chat_id: i64, action: &str, selection: Option<&str>) -> bool {
        todo!("telegram.py:3508 TelegramInlet._codex_switch")
    }

    /// py: telegram.py:3529 `def _claude_switch(self, inbound: Inbound, on: bool, target: str = "", *, note: bool = False, tell: bool = True, ask_to_start: bool = False) -> bool:`
    ///
    /// "claude on [name]" / "claude off" as _handle routes them. False (nothing done) when turning on while
    /// another owner chat holds the Codex relay; with ``tell`` that chat is told why (the brain's tool says it
    /// from the result instead, once). ``note``: the code word goes in the transcript (a brain tool's call is not
    /// a line the owner typed). ``ask_to_start``: a session that is not running is offered as a button to start,
    /// never launched (_claude_on_pick).
    pub(crate) fn claude_switch(self: &Arc<Self>, inbound: &Inbound, on: bool, target: &str, note: bool, tell: bool, ask_to_start: bool) -> bool {
        todo!("telegram.py:3529 TelegramInlet._claude_switch")
    }

    /// py: telegram.py:3562 `def set_muse(self, on: bool, *, chat_id: Optional[int] = None, tell: bool = True) -> bool:`
    ///
    /// "muse on" / "muse off", typed or from the app's switch (link_chat ``/api/link/muse``) → whether Muse is
    /// on now. On turns the Claude and Codex relays off: one relay at a time. Off when Muse is not set up here.
    pub fn set_muse(self: &Arc<Self>, on: bool, chat_id: Option<i64>, tell: bool) -> bool {
        todo!("telegram.py:3562 TelegramInlet.set_muse")
    }

    /// py: telegram.py:3583 `async def _muse_send(self, chat_id: int, text: str) -> None:`
    ///
    /// One message to Muse; its answer comes back titled Muse. The words and the answer are in the
    /// transcript as relay lines, never in eve's model history.
    pub(crate) async fn muse_send(self: &Arc<Self>, chat_id: i64, text: &str) {
        todo!("telegram.py:3583 TelegramInlet._muse_send")
    }

    /// py: telegram.py:3591 `def _set_stealth(self, chat_id: int, on: bool) -> None:`
    ///
    /// Stealth on ("act asleep at the desk") or off ("wake up"), and the line that says so.
    pub(crate) fn set_stealth(self: &Arc<Self>, chat_id: i64, on: bool) {
        todo!("telegram.py:3591 TelegramInlet._set_stealth")
    }

    /// py: telegram.py:3599 `async def _app_by_code(self, app: app_registry.App, hub: Any, inbound: Inbound, cmd: Any) -> None:`
    ///
    /// A message that is only an app's command, done by code; the line says what the app did. An app with a
    /// ``code_taint_tool`` (Spotify: a play by name answers with the catalog's words) keeps its line as outside
    /// words when that tool's call would taint, so the next turn that shows it starts tainted (tool_gate).
    pub(crate) async fn app_by_code(self: &Arc<Self>, app: &eve_app_registry::App, hub: Option<Arc<dyn AppHub>>, inbound: &Inbound, cmd: &HubCommand) {
        todo!("telegram.py:3599 TelegramInlet._app_by_code")
    }

    /// py: telegram.py:3612 `def _live_apps(self) -> list[tuple[app_registry.App, Any]]:`
    ///
    /// The tools apps whose hub is here, in tool order (app_registry.APPS): the order the model reads them in.
    pub(crate) fn live_apps(self: &Arc<Self>) -> Vec<(&'static eve_app_registry::App, Option<Arc<dyn AppHub>>)> {
        todo!("telegram.py:3612 TelegramInlet._live_apps")
    }

    /// py: telegram.py:3617 `async def _quick_answer(self, inbound: Inbound, ask: quick_answers.Ask) -> None:`
    ///
    /// A standard question, answered by code. When the source fails the model answers, as it always did.
    pub(crate) async fn quick_answer(self: &Arc<Self>, inbound: &Inbound, ask: &eve_quick_answers::Ask) {
        todo!("telegram.py:3617 TelegramInlet._quick_answer")
    }

    /// py: telegram.py:3628 `async def _image(self, inbound: Inbound, target: str, epoch: int, *, claude_epoch: Optional[int] = None) -> None:`
    ///
    /// One image from the owner, downloaded and handed to its recipient. For the Claude or Codex relay a
    /// 👀 on the owner's message says it arrived (owner, 2026-09-23), and the 👍 that replaces it says it was
    /// delivered; when it is not delivered, the 👀 comes off again and the line saying why stays.
    pub(crate) async fn image(self: &Arc<Self>, inbound: &Inbound, target: &str, epoch: i64, claude_epoch: Option<i64>) {
        todo!("telegram.py:3628 TelegramInlet._image")
    }

    /// py: telegram.py:3647 `async def _unreact(self, chat_id: int, message_id: int) -> None:`
    ///
    /// Take the bot's reaction off a message (a 👀 whose image never arrived). Best effort, never raises.
    pub(crate) async fn unreact(self: &Arc<Self>, chat_id: i64, message_id: i64) {
        todo!("telegram.py:3647 TelegramInlet._unreact")
    }

    /// py: telegram.py:3656 `async def _react(self, chat_id: int, message_id: int, emoji: str) -> None:`
    ///
    /// Set eve's one reaction on an owner message ("" takes it off): an Eve Link app message's
    /// (link_reactions.Reactions, a negative id) in the app's store, which the app reads with its messages; any
    /// other in Telegram (setMessageReaction). Raises when it cannot be set.
    pub(crate) async fn react(self: &Arc<Self>, chat_id: i64, message_id: i64, emoji: &str) {
        todo!("telegram.py:3656 TelegramInlet._react")
    }

    /// py: telegram.py:3665 `async def _relay_image(self, inbound: Inbound, target: str, epoch: int, claude_epoch: Optional[int] = None) -> bool:`
    ///
    /// The download and the hand-over → True when the image reached the Claude terminal or Codex.
    pub(crate) async fn relay_image(self: &Arc<Self>, inbound: &Inbound, target: &str, epoch: i64, claude_epoch: Option<i64>) -> bool {
        todo!("telegram.py:3665 TelegramInlet._relay_image")
    }
}
