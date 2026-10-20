//! A port of `telegram.py`, lines 6308-6610: /america, /meet, a call's reader, hearing and spoken answers.
//!
//! Skeleton: constants and prompts are verbatim (generated from the Python values); bodies are
//! `todo!()` until this file's unit fills them.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

impl TelegramInlet {
    /// py: telegram.py:6308 `def _cancel_america_command(self, chat_id: int) -> bool:`
    pub(crate) fn cancel_america_command(self: &Arc<Self>, chat_id: i64) -> bool {
        todo!("telegram.py:6308 TelegramInlet._cancel_america_command")
    }

    /// py: telegram.py:6320 `def _start_america_command(self, inbound: Inbound, question: str) -> None:`
    ///
    /// A menu command owns its question, even while a relay or another prompt is active.
    pub(crate) fn start_america_command(self: &Arc<Self>, inbound: &Inbound, question: &str) {
        todo!("telegram.py:6320 TelegramInlet._start_america_command")
    }

    /// py: telegram.py:6350 `def _answer_america_question(self, inbound: Inbound) -> bool:`
    pub(crate) fn answer_america_question(self: &Arc<Self>, inbound: &Inbound) -> bool {
        todo!("telegram.py:6350 TelegramInlet._answer_america_question")
    }

    /// py: telegram.py:6373 `async def _america_command(self, inbound: Inbound, question: str) -> None:`
    pub(crate) async fn america_command(self: &Arc<Self>, inbound: &Inbound, question: &str) {
        todo!("telegram.py:6373 TelegramInlet._america_command")
    }

    /// py: telegram.py:6404 `async def _meet_command(self, chat_id: int, rest: str) -> None:`
    ///
    /// /meet by code: bare (or "status") says where eve is, "leave" takes it out, anything else is a link or
    /// the owner's words for a meeting on their calendar.
    pub(crate) async fn meet_command(self: &Arc<Self>, chat_id: i64, rest: &str) {
        todo!("telegram.py:6404 TelegramInlet._meet_command")
    }

    /// py: telegram.py:6432 `def chat_ready(self) -> bool:`
    ///
    /// Whether a call has an owner chat to be in, asked without taking the listener (phone_call.Call.run: a
    /// call takes it at its first press). The model's cache is warmed for the first answer either way.
    pub fn chat_ready(self: &Arc<Self>) -> bool {
        todo!("telegram.py:6432 TelegramInlet.chat_ready")
    }

    /// py: telegram.py:6440 `def _call_reader(self) -> Optional[Callable[[str], None]]:`
    ///
    /// The call's reader, for work that began on this same call (``_FROM_CALL``) while it still holds the
    /// listener; None for anything else: typed work, and work begun on a call that has since ended. ``==``, not
    /// ``is``, as in ``listen``: each ``call.say`` is a new bound method.
    pub(crate) fn call_reader(self: &Arc<Self>) -> Option<Reader> {
        todo!("telegram.py:6440 TelegramInlet._call_reader")
    }

    /// py: telegram.py:6447 `def listen(self, say: Optional[Callable[[str], None]], owner: Optional[Callable[[str], None]] = None) -> bool:`
    ///
    /// A phone call starts (``say``) or ends (None). While it lasts, what eve says in the owner's chat is also
    /// read out on the call. False when there is no owner chat to be in.
    ///
    /// An ending call passes its own reader as ``owner``, and the listener is cleared only if it is still that
    /// one. The phone and the desk share this one listener, and a phone call can start while a desk call is on:
    /// the desk call's end used to clear the phone call's reader, and the rest of that call was texted, not
    /// spoken (verification/Eve/DeskCall.lean). ``==``, not ``is``: each ``call.say`` is a new bound method.
    pub fn listen(self: &Arc<Self>, say: Option<Reader>, owner: Option<&Reader>) -> bool {
        todo!("telegram.py:6447 TelegramInlet.listen")
    }

    /// py: telegram.py:6462 `def answer_context(self) -> object | None:`
    ///
    /// Opaque question identity, captured when a push-to-talk recording begins.
    pub fn answer_context(self: &Arc<Self>) -> eve_phone_call::AnswerContext {
        todo!("telegram.py:6462 TelegramInlet.answer_context")
    }

    /// py: telegram.py:6466 `def hear(self, text: str, *, answer_context: object = _UNCAPTURED_ANSWER, on_call: bool = True, call: Optional[Callable[[str], None]] = None, app: Optional[str] = None, grade: str = "auto", attachments: Sequence[Any] = ()) -> None:`
    ///
    /// The owner's words from a call, handled exactly as a text from them, so the call can do everything the
    /// chat can. Not echoed into the chat (owner: no text unless it is needed); the transcript keeps them.
    /// What they start is marked as this call's (``_FROM_CALL``), so its answer is read out on this call.
    /// ``call``: the reader of the call that heard the words (phone_call.Call.heard passes its own ``say``). The
    /// desk and the phone share one listener, and a call can hold the button while the other holds the listener:
    /// the words are marked with the call that heard them, not the one holding the listener, and that call takes
    /// the listener back, so its answer is read where it was asked (CallScope.lean, the hearing call). None: the
    /// listener's (a caller that does not say which call it is).
    /// ``on_call=False``: words typed in the Mac chat window (daemon ``_ipc_chat``), which take this path for
    /// its stop and answer handling but are typed, so nothing they start is read aloud (CallScope.lean).
    /// ``app``: the Eve Link app turn these typed words are (link_chat.py, with ``on_call=False``): what they
    /// start is marked as that turn's (``_FROM_APP``), so its lines go to the app and not to Telegram.
    /// ``grade``: the app's speed dial for that turn (GRADES, ``graded``); anything else is "auto".
    /// ``attachments``: the app turn's photos and files (link_uploads.Upload): the words and they go as one
    /// message with an image does in Telegram (``_image``), never as an answer to a waiting question.
    pub fn hear(self: &Arc<Self>, text: &str, answer_context: eve_phone_call::Captured, on_call: bool, call: Option<&Reader>, app: Option<&str>, grade: &str, attachments: &[eve_link_uploads::Upload]) {
        todo!("telegram.py:6466 TelegramInlet.hear")
    }

    /// py: telegram.py:6502 `def _hear(self, text: str, answer_context: object) -> None:`
    pub(crate) fn do_hear(self: &Arc<Self>, text: &str, answer_context: eve_phone_call::Captured) {
        todo!("telegram.py:6502 TelegramInlet._hear")
    }

    /// py: telegram.py:6524 `def _app_message_id(self) -> int:`
    ///
    /// The id a reaction lands on for the Eve Link app message being heard (link_reactions.Reactions.mint:
    /// negative, never a Telegram message's); 0 for words that are not an app turn's, as before.
    pub(crate) fn app_message_id(self: &Arc<Self>) -> i64 {
        todo!("telegram.py:6524 TelegramInlet._app_message_id")
    }

    /// py: telegram.py:6535 `def _hear_attached(self, text: str, uploads: tuple[Any, ...]) -> None:`
    ///
    /// Words with attachments from the Eve Link app: routed as a Telegram photo with its caption is.
    pub(crate) fn hear_attached(self: &Arc<Self>, text: &str, uploads: &[Value]) {
        todo!("telegram.py:6535 TelegramInlet._hear_attached")
    }

    /// py: telegram.py:6545 `def _live_question(self) -> Optional[_Question]:`
    pub(crate) fn live_question(self: &Arc<Self>) -> Option<Arc<_Question>> {
        todo!("telegram.py:6545 TelegramInlet._live_question")
    }

    /// py: telegram.py:6548 `async def _hear_answer(self, chat: int, text: str, asked: _Question) -> None:`
    ///
    /// A spoken answer to a question with choices. The exact choice or a plain yes/no counts at once; a
    /// Claude permission prompt takes nothing else. Other words are read against the question by the model
    /// ("Can you search, bro?" to "press Return?" is a yes); when it cannot tell, eve asks again, and the
    /// words never start a new request while the question waits. A firm question (the chief's Go) is read as a
    /// permission prompt is: a model's reading of the owner's words never approves a one-way act (design
    /// principle 3; reviewer after P4, 2026-09-29: "hmm, I'll think about it after lunch" was judged a yes).
    pub(crate) async fn hear_answer(self: &Arc<Self>, chat: i64, text: &str, asked: &Arc<_Question>) {
        todo!("telegram.py:6548 TelegramInlet._hear_answer")
    }

    /// py: telegram.py:6575 `async def _judge_answer(self, question: str, choices: list[str], said: str) -> Optional[str]:`
    ///
    /// Which choice a spoken answer means, or None when it is not clearly one of them.
    pub(crate) async fn judge_answer(self: &Arc<Self>, question: &str, choices: &[String], said: &str) -> Option<String> {
        todo!("telegram.py:6575 TelegramInlet._judge_answer")
    }

    /// py: telegram.py:6598 `def _to_call(self, chat_id: int, text: str, title: Optional[str]) -> None:`
    ///
    /// Read a message out on the phone call, when one is on: the owner's chat only, and not the call's own
    /// echo of what they said or a task's step-by-step progress.
    pub(crate) fn to_call(self: &Arc<Self>, chat_id: i64, text: &str, title: Option<&str>) {
        todo!("telegram.py:6598 TelegramInlet._to_call")
    }
}

/// The chat as Eve Link's door drives it (link.py reads these attributes off the inlet). `self.me` is the inlet's own
/// `Arc`, for the methods that spawn.
impl eve_link::seams::Inlet for TelegramInlet {
    fn chat_ready(&self) -> bool {
        todo!("telegram.py: inlet._chat_id is not None")
    }
    fn hear(&self, text: &str, opts: eve_link::seams::HearOptions) -> anyhow::Result<()> {
        todo!("telegram.py:6466 TelegramInlet.hear (on_call, app, grade, attachments)")
    }
    fn owner_busy(&self) -> bool {
        todo!("telegram.py owner_busy")
    }
    fn waiting_on_owner(&self) -> bool {
        todo!("telegram.py waiting_on_owner")
    }
    fn app_filed(&self, turn_id: &str) -> Option<String> {
        todo!("telegram.py app_filed")
    }
    fn set_app_out(&self, out: AppOut) {
        todo!("telegram.py: inlet.app_out = out")
    }
    fn set_link_uploads(&self, uploads: Arc<eve_link_uploads::UploadStore>) {
        todo!("telegram.py: inlet.link_uploads = uploads")
    }
    fn transcripts(&self) -> Option<Arc<eve_transcripts::Transcripts>> {
        todo!("telegram.py transcripts")
    }
    fn app_reactions(&self) -> Option<Arc<eve_link_reactions::Reactions>> {
        todo!("telegram.py: inlet.app_reactions")
    }
    fn chief(&self) -> Option<Arc<dyn eve_link::seams::Chief>> {
        todo!("telegram.py: inlet.chief")
    }
    fn watches(&self) -> Option<Vec<eve_web::watch::Watch>> {
        todo!("telegram.py: inlet._watcher.watches")
    }
    fn direct_apps(&self) -> Option<Arc<dyn eve_link::seams::DirectApps>> {
        todo!("telegram.py: inlet._direct")
    }
    fn vault_root(&self) -> Option<PathBuf> {
        todo!("telegram.py: Path(inlet._vault.root)")
    }
    fn composio(&self) -> Option<Arc<dyn eve_link::seams::ComposioRead>> {
        todo!("telegram.py: inlet._apps")
    }
    fn muse(&self) -> bool {
        todo!("telegram.py: inlet.muse")
    }
    fn set_muse(&self, on: bool, tell: bool) {
        todo!("telegram.py set_muse")
    }
}

/// The chat as a phone or desk call drives it (phone_call.PhoneCalls lends it as the call's brain).
impl eve_phone_call::Brain for TelegramInlet {
    fn hear(&self, text: &str, call: Option<&Reader>, answer_context: eve_phone_call::Captured) {
        todo!("telegram.py:6466 TelegramInlet.hear from a call")
    }
    fn listen(&self, say: Option<Reader>, owner: Option<&Reader>) -> bool {
        todo!("telegram.py:6447 TelegramInlet.listen")
    }
    fn chat_ready(&self) -> Option<bool> {
        todo!("telegram.py: inlet._chat_id is not None")
    }
    fn answer_context(&self) -> Option<eve_phone_call::AnswerContext> {
        todo!("telegram.py:6462 TelegramInlet.answer_context")
    }
    fn takes_call(&self) -> bool {
        true
    }
}
