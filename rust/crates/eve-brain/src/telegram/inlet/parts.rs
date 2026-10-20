//! A port of `telegram.py`, lines 5571-5852: a turn's parts, tool lists, Work-session tools, lent toolsets, warm-up.
//!
//! Skeleton: constants and prompts are verbatim (generated from the Python values); bodies are
//! `todo!()` until this file's unit fills them.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

impl TelegramInlet {
    /// py: telegram.py:5571 `async def _turn_parts(self, tail: int, *, warm: bool = False) -> tuple[dict[str, Any], set[str], list[dict[str, Any]]]:`
    ///
    /// What every round of a turn sends besides its items: request()'s keyword arguments, the tool names the
    /// model may call, and the app tools (the rundown narrows them). The cache warm-up (``warm``) sends the same,
    /// but with the router on it opens no toolset: the previous turn's would be stale (2026-10-05).
    pub(crate) async fn turn_parts(self: &Arc<Self>, tail: i64, warm: bool) -> (Obj, Vec<String>, Vec<Obj>) {
        todo!("telegram.py:5571 TelegramInlet._turn_parts")
    }

    /// py: telegram.py:5632 `def _tool_lists(self, opened: set[str], lent: set[str], routed: bool) -> dict[str, Any]:`
    ///
    /// The tool lists one turn offers, and their callable names: what ``_turn_parts`` sends and what a Work
    /// session's eve MCP server offers (``session_tools``), built in one place so the two cannot drift
    /// (docs/work-sessions.md). ``allowed`` is a plain set here; ``_turn_parts`` wraps it in AllowedNames.
    pub(crate) fn tool_lists(self: &Arc<Self>, opened: &[String], lent: &[String], routed: bool) -> Obj {
        todo!("telegram.py:5632 TelegramInlet._tool_lists")
    }

    /// py: telegram.py:5682 `def session_tools(self) -> tuple[list[dict[str, Any]], set[str]]:`
    ///
    /// The tools a Work session's eve MCP server offers, and their names: the chat brain's own lists
    /// (``_tool_lists``) as an unrouted turn with every up direct app opened offers them, less SESSION_HIDDEN and
    /// any tool that is not a plain function (the hosted web search has no MCP form). Never a second copy: a
    /// tool that comes or goes for the chat comes or goes here on the next tools/list.
    pub fn session_tools(self: &Arc<Self>) -> (Vec<Obj>, Vec<String>) {
        todo!("telegram.py:5682 TelegramInlet.session_tools")
    }

    /// py: telegram.py:5702 `async def session_call(self, name: str, args: Any, *, sid: str) -> tuple[dict[str, Any], str]:`
    ///
    /// One tool call from a Work session → (its result, the outside source it read, or ""). The call takes the
    /// chat's own path: a name the session was not offered is refused; ``tool_gate.needs_confirmation`` with
    /// SESSION_READ as the read (so recording, joining and answering ask, as after any read), then ``_gate``,
    /// which asks the owner on the phone and waits (``_ask_user``, ``config.ask_timeout_secs``); then ``_tool``,
    /// where the apps' own policies ask for their consequential writes, purchases and shares as in the chat.
    /// A question nobody answers ends as OWNER_NO_ANSWER. The chat turn's ``_taint`` is never read or written
    /// here. Known side effect: a direct app the call names is opened for the chat's next turn too
    /// (``_direct_tool``'s ensure_open), as a call from the chat would.
    pub async fn session_call(self: &Arc<Self>, name: &str, args: &Value, sid: &str) -> (Obj, String) {
        todo!("telegram.py:5702 TelegramInlet.session_call")
    }

    /// py: telegram.py:5738 `def _menu_tools(self) -> list[dict[str, Any]]:`
    ///
    /// The / menu's tools (menu_tools.py), then events_search while Ticketmaster's key is set: on every turn,
    /// in a fixed order, so the cached prefix holds. None while EVE_MENU_TOOLS is off.
    pub(crate) fn menu_tools(self: &Arc<Self>) -> Vec<Obj> {
        todo!("telegram.py:5738 TelegramInlet._menu_tools")
    }

    /// py: telegram.py:5745 `def _events_line(self) -> str:`
    ///
    /// events_search's instruction while it is lent; it names watch_add's ticketmaster kind only when the
    /// watcher offers it (no name in a prompt that the turn does not offer).
    pub(crate) fn events_line(self: &Arc<Self>) -> String {
        todo!("telegram.py:5745 TelegramInlet._events_line")
    }

    /// py: telegram.py:5754 `def _lendable(self) -> list[str]:`
    ///
    /// The lent toolsets this Mac has (in LENT_ORDER): Share when lent, the workspace while
    /// Composio's session is up.
    pub(crate) fn lendable(self: &Arc<Self>) -> Vec<String> {
        todo!("telegram.py:5754 TelegramInlet._lendable")
    }

    /// py: telegram.py:5761 `def _lent_block(self, lent: Collection[str]) -> str:`
    ///
    /// The open lent toolsets' instructions, for request()'s lent_block; "" with none open.
    pub(crate) fn lent_block(self: &Arc<Self>, lent: &[String]) -> String {
        todo!("telegram.py:5761 TelegramInlet._lent_block")
    }

    /// py: telegram.py:5770 `def _refresh_lent(self, parts: dict[str, Any], allowed: set[str]) -> list[dict[str, Any]]:`
    ///
    /// After this turn's toolsets changed: the app tools (and, routed, the lent instructions) rebuilt into
    /// ``parts`` and ``allowed``, in place. Returns the new app tools.
    pub(crate) fn refresh_lent(self: &Arc<Self>, parts: &Obj, allowed: &[String]) -> Vec<Obj> {
        todo!("telegram.py:5770 TelegramInlet._refresh_lent")
    }

    /// py: telegram.py:5780 `def _routed_owns(self, name: str) -> bool:`
    ///
    /// AllowedNames' claim while routed: a direct app's tool (AppsBridge.owns), or Composio's while it is up.
    pub(crate) fn routed_owns(self: &Arc<Self>, name: &str) -> bool {
        todo!("telegram.py:5780 TelegramInlet._routed_owns")
    }

    /// py: telegram.py:5786 `def _lend(self, name: str) -> None:`
    ///
    /// A call to a closed lent toolset's tool opens it for the rest of the turn (as AppsBridge.ensure_open for
    /// a direct app): the call runs now, and its tools are offered from the next round. Never a failed turn.
    pub(crate) fn lend(self: &Arc<Self>, name: &str) {
        todo!("telegram.py:5786 TelegramInlet._lend")
    }

    /// py: telegram.py:5792 `def _lent_tools(self, name: str) -> list[dict[str, Any]]:`
    pub(crate) fn lent_tools(self: &Arc<Self>, name: &str) -> Vec<Obj> {
        todo!("telegram.py:5792 TelegramInlet._lent_tools")
    }

    /// py: telegram.py:5802 `def _open_toolsets(self, args: dict[str, Any]) -> dict[str, Any]:`
    ///
    /// open_app while routed: opens direct apps and lent toolsets alike, for the rest of the turn. A name that
    /// is no toolset here is not repeated back (the owner's rule): the result names the ones that are.
    pub(crate) fn open_toolsets(self: &Arc<Self>, args: &Obj) -> Obj {
        todo!("telegram.py:5802 TelegramInlet._open_toolsets")
    }

    /// py: telegram.py:5823 `def _previous_said(self) -> str:`
    ///
    /// The owner's message before this turn's (a follow-up's context for the router). _answer_turn notes the
    /// running turn's own line before _think, so that line is passed over; "" with none.
    pub(crate) fn previous_said(self: &Arc<Self>) -> String {
        todo!("telegram.py:5823 TelegramInlet._previous_said")
    }

    /// py: telegram.py:5832 `async def _warm(self) -> None:`
    ///
    /// A call opened: send the chat's own prompt once with nothing to answer, so the owner's first question
    /// finds it cached (measured 2026-09-25: a call's first turn read 12k tokens with none cached). The reply is
    /// thrown away, and nothing is recorded but its spend.
    pub(crate) async fn warm(self: &Arc<Self>) {
        todo!("telegram.py:5832 TelegramInlet._warm")
    }
}
