//! A port of `telegram.py`, lines 1461-1721: per-turn context (task locals), Inbound, accept, Tap, Choice, answer words.
//!
//! Skeleton: constants and prompts are verbatim (generated from the Python values); bodies are
//! `todo!()` until this file's unit fills them.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

// py: telegram.py:1469 `_FROM_CALL` (ContextVar): `seams::TurnCtx::call` (`ctx()`, `set_ctx`, `in_ctx`).

// py: telegram.py:1477 `_FROM_APP` (ContextVar): `seams::TurnCtx::app`.

// py: telegram.py:1480 `_APP_GRADE` (ContextVar): `seams::TurnCtx::app_grade`.

// py: telegram.py:1485 `_SESSION` (ContextVar): `seams::TurnCtx::session`.

/// py: telegram.py:1489 `def graded(payload: dict[str, Any], config: TelegramConfig, grade: str) -> dict[str, Any]:`
///
/// One round's request as the app's speed dial grades it. "auto" (and anything else): the payload as it is.
/// "instant": the chat's model at low effort, think_hard not offered. "think": the app's think model and effort.
/// The instructions and the other tools stay, so the cached prefix is the chat's own.
pub fn graded(payload: &Obj, config: &TelegramConfig, grade: &str) -> Obj {
    todo!("telegram.py:1489 graded")
}

/// py: telegram.py:1502 `APP_BACKGROUND_JOBS`
/// A Python set: sorted here; membership is what matters.
pub const APP_BACKGROUND_JOBS: &[&str] = &["telegram-agent", "telegram-board", "telegram-call-warm", "telegram-claude", "telegram-codex", "telegram-codex-progress", "telegram-edit", "telegram-progress", "telegram-relay"];

/// py: telegram.py:1508 `class Inbound:`
/// Equality is Python's: chat, user, text and image only (`message_id`, `tapped` and `uploads` are `compare=False`);
/// the fill unit writes that `PartialEq`.
#[derive(Debug, Clone, Default)]
pub struct Inbound {
    pub chat_id: i64,
    pub user_id: i64,
    pub text: String,
    pub image: Option<eve_telegram_images::Attachment>,
    /// what a reaction lands on; 0 when unknown
    pub message_id: i64,
    pub tapped: bool,
    pub uploads: Vec<eve_link_uploads::Upload>,
}

/// py: telegram.py:1523 `def _attached(inbound: Inbound) -> str:`
///
/// The history's note for what came with a message: "[image attached]", as Telegram's, or what the app sent.
pub(crate) fn attached(inbound: &Inbound) -> String {
    todo!("telegram.py:1523 _attached")
}

/// py: telegram.py:1533 `def _att_of(inbound: Inbound) -> Optional[list[dict[str, str]]]:`
///
/// The transcript's ``att`` for an app message's attachments, or None.
pub(crate) fn att_of(inbound: &Inbound) -> Option<Vec<HashMap<String, String>>> {
    todo!("telegram.py:1533 _att_of")
}

/// py: telegram.py:1544 `OK`
pub const OK: &str = "ok";

/// py: telegram.py:1544 `NOT_TEXT`
pub const NOT_TEXT: &str = "not-text";

/// py: telegram.py:1544 `FORWARDED`
pub const FORWARDED: &str = "forwarded";

/// py: telegram.py:1547 `def accept(update: Any, config: TelegramConfig, now: float) -> tuple[str, Optional[Inbound]]:`
///
/// Decide one update. Pure. ``now`` is wall-clock seconds (Telegram stamps messages in Unix time).
pub fn accept(update: &Value, config: &TelegramConfig, now: f64) -> (String, Option<Inbound>) {
    todo!("telegram.py:1547 accept")
}

/// py: telegram.py:1585 `class Tap:`
///
/// A tap on one of eve's inline buttons, from the owner, in their private chat.
#[derive(Debug, Clone)]
pub struct Tap {
    pub query_id: String,
    pub chat_id: i64,
    pub user_id: i64,
    pub data: String,
    pub message_id: i64,
}

/// py: telegram.py:1594 `def accept_tap(update: Any, config: TelegramConfig) -> tuple[str, Optional[Tap]]:`
///
/// Decide one callback_query update. Pure. The same rule as ``accept``: only an owner id, never a bot,
/// and only in the owner's own private chat. A tap has no date to check for staleness (its message's date
/// is when eve sent it); a stale button is caught by its key instead (TelegramInlet._on_tap).
pub fn accept_tap(update: &Value, config: &TelegramConfig) -> (String, Option<Tap>) {
    todo!("telegram.py:1594 accept_tap")
}

/// py: telegram.py:1623 `class Choice:`
///
/// One inline button: its ``label``, what a tap means (``value``: the answer, the line to type, the
/// words to send), its colour, and the line the prompt is edited to once it is chosen.
#[derive(Debug, Clone)]
pub struct Choice {
    pub label: String,
    pub value: String,
    pub style: String,
    pub done: String,
}

// py: telegram.py:1632 `ALLOW_DENY` (tuple): ported by this file's unit.

// py: telegram.py:1633 `YES_NO` (tuple): ported by this file's unit.

/// py: telegram.py:1636 `def reply_words(choices: Sequence[Choice]) -> str:`
///
/// How the Eve Link app shows a keyboard's buttons: the words that answer it, typed by the owner (the app has
/// no buttons for eve's own messages; a typed value takes the path its tap would).
pub fn reply_words(choices: &[Choice]) -> String {
    todo!("telegram.py:1636 reply_words")
}

/// py: telegram.py:1644 `YES_NO_END`
/// Python flags 34.
pub static YES_NO_END: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)(\byes\s*/\s*no\??|\breply yes or no\.?)\s*(?:\n?\z)"#).expect("YES_NO_END"));

/// py: telegram.py:1645 `GO_AHEAD`
/// Python flags 34.
pub static GO_AHEAD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)^should i go ahead\b"#).expect("GO_AHEAD"));

/// py: telegram.py:1646 `CODEX_REPLY`
/// Python flags 32.
pub static CODEX_REPLY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\nReply: (?P<choices>[^\n]+?)\.?\s*(?:\n?\z)"#).expect("CODEX_REPLY"));

// py: telegram.py:1647 `CODEX_CHOICES` (tuple): ported by this file's unit.

/// py: telegram.py:1653 `ASK_OPTIONS`
/// Python flags 48.
pub static ASK_OPTIONS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)^(?P<question>.+?) \((?P<options>1\. .+)\)(?:\n?\z)"#).expect("ASK_OPTIONS"));

/// py: telegram.py:1656 `def answer_choices(question: str) -> tuple[Choice, ...]:`
///
/// The buttons a task's question gets, or none. Only a question whose answers are known is given
/// buttons: a free question ("which account?") is answered in words, as before.
pub fn answer_choices(question: &str) -> Vec<Choice> {
    todo!("telegram.py:1656 answer_choices")
}

/// py: telegram.py:1676 `def question_options(hint: str) -> list[tuple[int, str]]:`
///
/// An AskUserQuestion hint (hooks/pretooluse._question) → its numbered options, when it is one question.
/// Several questions ("… | …") are answered one after another in the terminal, so they get no buttons.
pub fn question_options(hint: &str) -> Vec<(i64, String)> {
    todo!("telegram.py:1676 question_options")
}

/// py: telegram.py:1691 `def sender_id(update: Any) -> Optional[int]:`
///
/// The numeric id behind an update, for the one log line a drop gets. Never the name, never the words.
pub fn sender_id(update: &Value) -> Option<i64> {
    todo!("telegram.py:1691 sender_id")
}

/// py: telegram.py:1703 `def receipt_line(name: str, result: dict[str, Any]) -> str:`
///
/// The short line a receipt reaction stands for, sent only when the reaction cannot be set: where a
/// capture went (second_brain's own "Saved to …" line), or that a fact was starred.
pub fn receipt_line(name: &str, result: &Obj) -> String {
    todo!("telegram.py:1703 receipt_line")
}

/// py: telegram.py:1713 `def _tool_line(result: Any) -> str:`
///
/// A tool result as one transcript line: its JSON, at most TOOL_LINE_CHARS.
pub(crate) fn tool_line(result: &Value) -> String {
    todo!("telegram.py:1713 _tool_line")
}
