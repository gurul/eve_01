//! A port of `telegram.py`, lines 6611-7077: tell_owner, the chief of staff, spend, stick, apps, direct/share/gate.
//!
//! Skeleton: constants and prompts are verbatim (generated from the Python values); bodies are
//! `todo!()` until this file's unit fills them.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

impl TelegramInlet {
    /// py: telegram.py:6611 `async def tell_owner(self, text: str, about: str = "", title: str = WATCH_TITLE, buttons: Sequence[Choice] = ()) -> bool:`
    ///
    /// A watch alert (watch.Watcher.notify): a new message in the owner's chat, unasked. It goes into the
    /// chat's history and transcript as eve's, so "stop watching that" in reply has something to point at.
    /// Stealth does not hold it back: stealth is the desk's, and this is the phone. Returns whether it was sent:
    /// the watcher keeps an alert Telegram refused and sends it again (``_say`` would swallow the refusal).
    /// The history and the transcript get ``about``, a line the watcher builds from the watch alone: the alert
    /// itself can carry words from the web, and the brain reading them as its own words was a stored prompt
    /// injection (re-verification, 2026-09-25).
    ///
    /// ``buttons`` (the chief's, 2026-09-29): SAY buttons under the message, whose tap types their words, so a
    /// tap and the typed code word take one path (``_on_tap``). A keyboard Telegram refuses leaves the message
    /// plain, never unsent.
    ///
    /// While the owner is in Eve Link (an app turn's work, ``_FROM_APP``, or Telegram app-only), the buttons
    /// reach the app the way a picker's do (``_send_choices``): as the words that answer them (``reply_words``),
    /// which the owner types or taps back and which take the tap's own path (a code word, ``_chief_command``).
    /// An app turn's message goes to the app's open stream, not to Telegram, when its words are eve's own
    /// (no separate ``about``); the transcript line, which the app's chat shows, carries the reply words.
    pub async fn tell_owner(self: &Arc<Self>, text: &str, about: &str, title: &str, buttons: &[Choice]) -> bool {
        todo!("telegram.py:6611 TelegramInlet.tell_owner")
    }

    /// py: telegram.py:6669 `def _chief_word(self, text: str) -> Optional[tuple[str, str]]:`
    ///
    /// ("jobs", "" | card id) or (word, card id) for a chief code word, or None: always None while the chief is
    /// off, so the words go where they always went.
    pub(crate) fn chief_word(self: &Arc<Self>, text: &str) -> Option<(String, String)> {
        todo!("telegram.py:6669 TelegramInlet._chief_word")
    }

    /// py: telegram.py:6683 `async def _chief_command(self, inbound: Inbound, verb: str, cid: str) -> None:`
    ///
    /// /jobs, "/jobs c12" and "<word> c12", by code (chief.py's own methods), never a model call. A word that
    /// sends its own message (done, happened: the receipt) is answered only when it could not act.
    pub(crate) async fn chief_command(self: &Arc<Self>, inbound: &Inbound, verb: &str, cid: &str) {
        todo!("telegram.py:6683 TelegramInlet._chief_command")
    }

    /// py: telegram.py:6739 `async def _chief_tool(self, name: str, args: dict[str, Any]) -> dict[str, Any]:`
    ///
    /// take_on, jobs_list or close_card. take_on and close_card get this turn's own words (``_turn_said``, taken by code from the
    /// owner's message, never from the model) for its door floor, and a reference to them (the transcript day
    /// and time). Its backbrief, composed by code, goes out here with Change and Drop; the turn then ends
    /// (``_think``). A card filed on an Eve Link app turn is recorded against that turn (``app_filed``).
    pub(crate) async fn chief_tool(self: &Arc<Self>, name: &str, args: &Obj) -> Obj {
        todo!("telegram.py:6739 TelegramInlet._chief_tool")
    }

    /// py: telegram.py:6758 `def _chief_line(self) -> str:`
    ///
    /// The chief's line for the turn's note (open cards, what waits on the owner), or "".
    pub(crate) fn chief_line(self: &Arc<Self>) -> String {
        todo!("telegram.py:6758 TelegramInlet._chief_line")
    }

    /// py: telegram.py:6768 `def _chief_spoke(self, text: str) -> None:`
    ///
    /// A message the owner sent through the Claude relay: the chief's breakpoint clock and their own actions
    /// on its cards, without the day's brief (that rides eve's own reply, _chief_heard).
    pub(crate) fn chief_spoke(self: &Arc<Self>, text: &str) {
        todo!("telegram.py:6768 TelegramInlet._chief_spoke")
    }

    /// py: telegram.py:6778 `async def _chief_heard(self, chat_id: int, text: str) -> None:`
    ///
    /// The owner wrote: the chief hears it (their action cancels a queued nudge, 90 s later is a moment), and
    /// the day's first message gets the brief after the reply, with any problem the ledger had.
    pub(crate) async fn chief_heard(self: &Arc<Self>, chat_id: i64, text: &str) {
        todo!("telegram.py:6778 TelegramInlet._chief_heard")
    }

    /// py: telegram.py:6792 `async def _chief_result(self, card: tuple[str, int], final: str, *, ok: bool, secs: float, steps: Sequence[str]) -> None:`
    ///
    /// A chief's Mac step ended: its closing sentence and the agent's UI states go to the chief, which reads
    /// the evidence (chief_receipt) and sends the receipt. The wrappers hand ``ui_evidence`` on from whoever
    /// ran (app_reflex, chrome_lane, web_reader).
    pub(crate) async fn chief_result(self: &Arc<Self>, card: (String, i64), final_: &str, ok: bool, secs: f64, steps: &[String]) {
        todo!("telegram.py:6792 TelegramInlet._chief_result")
    }

    /// py: telegram.py:6810 `def chief_start(self, goal: str, *, card_id: str, n: int, floor: Optional[str] = None) -> dict[str, Any]:`
    ///
    /// The chief's Mac step (chief.StartTask): the same slot, the same agent and the same Stop as a texted task.
    pub fn chief_start(self: &Arc<Self>, goal: &str, card_id: &str, n: i64, floor: Option<&str>) -> Obj {
        todo!("telegram.py:6810 TelegramInlet.chief_start")
    }

    /// py: telegram.py:6816 `def chief_stop(self, card_id: str, n: int) -> None:`
    ///
    /// Stop the chief's step at its ceiling (chief.StopTask): only that step, never another task.
    pub fn chief_stop(self: &Arc<Self>, card_id: &str, n: i64) {
        todo!("telegram.py:6816 TelegramInlet.chief_stop")
    }

    /// py: telegram.py:6822 `def chief_mac_free(self) -> bool:`
    ///
    /// The Mac can take a chief's step: tasks are on, no task runs, and the desk does not have it.
    pub fn chief_mac_free(self: &Arc<Self>) -> bool {
        todo!("telegram.py:6822 TelegramInlet.chief_mac_free")
    }

    /// py: telegram.py:6831 `async def chief_ask(self, text: str) -> str:`
    ///
    /// The chief's Go (chief.Ask), with Yes and No. Never while another question waits: its typed yes would
    /// answer this one. The history gets a line naming the card, not the Go's quoted pick. Firm: only a bare yes
    /// or no or a tap answers it, typed or spoken, relay or not; other words go on as a new message.
    pub async fn chief_ask(self: &Arc<Self>, text: &str) -> String {
        todo!("telegram.py:6831 TelegramInlet.chief_ask")
    }

    /// py: telegram.py:6844 `async def chief_notify(self, text: str, about: str = "") -> bool:`
    ///
    /// The chief's messages (chief.Notify), through ``tell_owner``, with the buttons the card has now
    /// (chief.choices). The card is the first id in ``about``, a line the chief composes from the card.
    pub async fn chief_notify(self: &Arc<Self>, text: &str, about: &str) -> bool {
        todo!("telegram.py:6844 TelegramInlet.chief_notify")
    }

    /// py: telegram.py:6857 `async def _spend_text(self) -> Optional[str]:`
    ///
    /// The spend brief (spend.py, spend_sync.py): today, yesterday, this month, the top features and OpenRouter's
    /// own figure, read off the loop; None when a ledger cannot be read. /spend and spend_report share it.
    pub(crate) async fn spend_text(self: &Arc<Self>) -> Option<String> {
        todo!("telegram.py:6857 TelegramInlet._spend_text")
    }

    /// py: telegram.py:6868 `async def _spend_now(self, chat_id: int) -> None:`
    ///
    /// /spend: the spend brief as a line. No model call.
    pub(crate) async fn spend_now(self: &Arc<Self>, chat_id: i64) {
        todo!("telegram.py:6868 TelegramInlet._spend_now")
    }

    /// py: telegram.py:6873 `async def _stick_link(self, chat_id: int, rotate: bool) -> None:`
    ///
    /// /stick: Eve Link's pairing link, a URL button (it opens a page that hands off to the app).
    pub(crate) async fn stick_link(self: &Arc<Self>, chat_id: i64, rotate: bool) {
        todo!("telegram.py:6873 TelegramInlet._stick_link")
    }

    /// py: telegram.py:6887 `async def _open_apps(self, chat_id: int) -> None:`
    ///
    /// /apps: the Mini App's Open button, at its address right now.
    pub(crate) async fn open_apps(self: &Arc<Self>, chat_id: i64) {
        todo!("telegram.py:6887 TelegramInlet._open_apps")
    }

    /// py: telegram.py:6895 `def _app_tools(self, opened: Optional[set[str]] = None, lent: Optional[set[str]] = None, routed: Optional[bool] = None) -> list[dict[str, Any]]:`
    ///
    /// The apps' tools for this turn: eve's app maker while the Mini App is live, then the direct apps'
    /// (open_app and the opened apps' tools), then Composio's once its session is up (both start on a thread).
    ///
    /// Routed (the toolset router on, 2026-10-05): the opened direct apps' tools, then the open lent toolsets'
    /// (the workspace's Composio tools, Share's), then open_app over every toolset still closed, last:
    /// what changes from turn to turn, or mid-turn, sits at the end of the list so the cached prefix is longest.
    pub(crate) fn app_tools(self: &Arc<Self>, opened: Option<&[String]>, lent: Option<&[String]>, routed: Option<bool>) -> Vec<Obj> {
        todo!("telegram.py:6895 TelegramInlet._app_tools")
    }

    /// py: telegram.py:6929 `def _read_now(self) -> str:`
    ///
    /// What the work running now has read from outside eve: a Work session's tool call counts as having read
    /// what the session wrote (SESSION_READ, ``_SESSION``); anything else is the chat turn's own ``_taint``.
    pub(crate) fn read_now(self: &Arc<Self>) -> String {
        todo!("telegram.py:6929 TelegramInlet._read_now")
    }

    /// py: telegram.py:6934 `def _direct_up(self) -> list[str]:`
    ///
    /// The direct apps that are up now (apps_mcp.AppsBridge.apps); none before the bridge has started.
    pub(crate) fn direct_up(self: &Arc<Self>) -> Vec<String> {
        todo!("telegram.py:6934 TelegramInlet._direct_up")
    }

    /// py: telegram.py:6940 `def _composio_up(self) -> bool:`
    pub(crate) fn composio_up(self: &Arc<Self>) -> bool {
        todo!("telegram.py:6940 TelegramInlet._composio_up")
    }

    /// py: telegram.py:6943 `async def _direct_tool(self, name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:`
    ///
    /// One direct app call under the owner's policy (apps_mcp.AppsBridge.decide): open_app loads an app for
    /// the turn; reads run, explicit read policies refuse writes, and args-aware policy permits routine
    /// private edits while consequential or unknown writes require a whole-action approval.
    pub(crate) async fn direct_tool(self: &Arc<Self>, name: &str, args: &Obj, chat_id: i64) -> Obj {
        todo!("telegram.py:6943 TelegramInlet._direct_tool")
    }

    /// py: telegram.py:6976 `def _write_question(self, describer: Any, name: str, args: dict[str, Any], *, mail: bool = False) -> Optional[str]:`
    ///
    /// Show every argument for a consequential app approval, or refuse it unasked.
    ///
    /// Gmail keeps its larger whole-email allowance: a describer's own (``asks_whole``), or ``mail`` when the
    /// caller knows the call sends email (eve lite's Gmail through Composio). Clean turns use the same full preview
    /// as tainted turns: an approval must never hide an argument behind a clipped label.
    pub(crate) fn write_question(self: &Arc<Self>, describer: &Value, name: &str, args: &Obj, mail: bool) -> Option<String> {
        todo!("telegram.py:6976 TelegramInlet._write_question")
    }

    /// py: telegram.py:6989 `async def _share_tool(self, name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:`
    ///
    /// share_file / find_contact (share.py): owner only. share_file sends nothing until the owner taps Allow on
    /// the exact file and recipient; the approval token never reaches the model (Share.ask_and_share).
    pub(crate) async fn share_tool(self: &Arc<Self>, name: &str, args: &Obj, chat_id: i64) -> Obj {
        todo!("telegram.py:6989 TelegramInlet._share_tool")
    }

    /// py: telegram.py:7008 `async def _gate(self, name: str, args: dict[str, Any], chat_id: int, read: Optional[str] = None) -> Optional[dict[str, Any]]:`
    ///
    /// The taint gate's question (tool_gate.py): None when the owner said yes and the call may run, else the
    /// call's result. Firm (only a bare yes or a tap answers it, relay or not), asked only while no other question
    /// waits (as _ask_alone: a yes meant for another question must not approve this one). The history gets a
    /// neutral line in place of the question, so no words from the read become eve's own. ``read``: what was read
    /// before the call (a Work session passes SESSION_READ); by default the turn's own taint (``_read_now``).
    pub(crate) async fn gate(self: &Arc<Self>, name: &str, args: &Obj, chat_id: i64, read: Option<&str>) -> Option<Obj> {
        todo!("telegram.py:7008 TelegramInlet._gate")
    }

    /// py: telegram.py:7031 `async def _ask_alone(self, question: str, chat_id: int, *, title: str, about: Optional[str] = None, read_all: bool = False) -> str:`
    ///
    /// A firm Allow/Deny for an outward send, asked only while no other question waits (as chief_ask).
    /// Review, 2026-10-05: a task's "Reply yes or no." was open when an Allow question opened; the
    /// newest question takes a typed yes, so the owner's yes for the task approved the other one. Checked again
    /// here, after the preview's awaits, as the caller's check may be stale by now. Any live question counts, not
    /// only the slot ``_awaiting_answer`` reads, which lags a loop turn behind a settled question (Lean ShareGate
    /// P3, 2026-10-05).
    pub(crate) async fn ask_alone(self: &Arc<Self>, question: &str, chat_id: i64, title: &str, about: Option<&str>, read_all: bool) -> String {
        todo!("telegram.py:7031 TelegramInlet._ask_alone")
    }

    /// py: telegram.py:7044 `async def _app_tool(self, name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:`
    ///
    /// One Composio call under args-aware policy: reads and permitted routine edits run; explicit
    /// read policy refuses writes, and consequential or unknown writes need the owner's whole-action yes.
    pub(crate) async fn app_tool(self: &Arc<Self>, name: &str, args: &Obj, chat_id: i64) -> Obj {
        todo!("telegram.py:7044 TelegramInlet._app_tool")
    }
}
