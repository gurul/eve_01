//! A port of `telegram.py`, lines 1167-1460: the prompt headers, recall lines, TelegramConfig and configured.
//!
//! Skeleton: constants and prompts are verbatim (generated from the Python values); bodies are
//! `todo!()` until this file's unit fills them.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

/// py: telegram.py:1169 `WANTS_SCREEN`
/// Python flags 34.
pub static WANTS_SCREEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\b(screen ?shots?|screen ?grab|show me|send me (a |the )?(picture|screen|image)|picture of (the|my) (screen|mac)|what does .{0,40}look like)\b"#).expect("WANTS_SCREEN"));

/// py: telegram.py:1172 `PROFILE_HEADER`
pub const PROFILE_HEADER: &str = "

What you know about your owner, from earlier conversations. Let it shape every reply, not only questions
about them: call them by name, fit their taste and how they like to be talked to, and do what they asked
for without making them say it again. Asked what you know about them, tell them plainly from this page.
Search your memory (memory_search, then memory_read) before saying you do not know something about them,
and never recite the page back unasked:";

/// py: telegram.py:1182 `MEMORY_HINT`
pub const MEMORY_HINT: &str = "

Search your memory (memory_search, then memory_read) before saying you do not know something about your
owner or about what the two of you said before.";

/// py: telegram.py:1189 `TODAY_HEADER`
pub const TODAY_HEADER: &str = "

Everything said between you and your owner earlier today, by voice and by text, oldest first. Use it to
resolve what they refer to, like \"the second one\" or \"what I said this morning\". Never quote it back as a
log. Older days are not here: memory_search finds them.
";

/// py: telegram.py:1200 `RECALL_SOURCES_LOG`
pub const RECALL_SOURCES_LOG: &str = "the history you are shown, {memory}and what recent_messages returns";

/// py: telegram.py:1201 `RECALL_SOURCES_NO_LOG`
pub const RECALL_SOURCES_NO_LOG: &str = "the history you are shown{memory}";

/// py: telegram.py:1202 `RECALL_MEMORY_LOG`
pub const RECALL_MEMORY_LOG: &str = "today's talk at the end of these instructions, what memory_search finds, ";

/// py: telegram.py:1203 `RECALL_MEMORY_NO_LOG`
pub const RECALL_MEMORY_NO_LOG: &str = ", today's talk at the end of these instructions and what memory_search finds";

/// py: telegram.py:1204 `RECALL_HEAD`
pub const RECALL_HEAD: &str = "

Earlier messages: what you know of this chat before the message you are answering is exactly {sources}.";

/// py: telegram.py:1206 `RECALL_LOG_RULES`
pub const RECALL_LOG_RULES: &str = " When the owner asks what they or you said earlier, like \"what was my last message\" or \"before that\", call recent_messages and answer word for word from what it returns. The message you are answering is not counted: their last message is owner_back 1. \"Before that\" means before the message the two of you were just talking about: when you have just quoted a message, answer with the one before it, which recent_messages gives as before_that. A note in brackets in the history, like (You asked the computer agent: ...), is work you handed on, not words the owner said; a note like (2 hours later) marks a quiet spell.";

/// py: telegram.py:1214 `RECALL_NO_LOG_RULES`
pub const RECALL_NO_LOG_RULES: &str = " Older messages are not kept.";

/// py: telegram.py:1215 `RECALL_TAIL`
pub const RECALL_TAIL: &str = " When the answer is in none of these, say plainly that you do not have it. Never give a message you already quoted as the one before it, and never make one up.";

/// py: telegram.py:1218 `_RECALL_NOTES`
pub(crate) const RECALL_NOTES: &str = "A note in brackets in the history, like (You asked the computer agent: ...), is work you handed on, not words the owner said; a note like (2 hours later) marks a quiet spell.";

/// py: telegram.py:1220 `_RECALL_NOTES_LITE`
pub(crate) const RECALL_NOTES_LITE: &str = "A note in brackets in the history, like (2 hours later), marks a quiet spell.";

// py: telegram.py:1221: a top-level `Assert`, ported by this file's unit.

/// py: telegram.py:1225 `AGENT_KIND`
pub const AGENT_KIND: &str = "agent";

/// py: telegram.py:1226 `AGENT_NOTE`
pub const AGENT_NOTE: &str = "({label}; its words are in recent_messages)";

/// py: telegram.py:1229 `OLDER_NOTE`
pub const OLDER_NOTE: &str = "(Older messages of this chat are not shown here: recent_messages reads them.)";

/// py: telegram.py:1230 `RECENT_MESSAGES_TOOL`
/// The Python value as JSON (tuples are arrays).
pub static RECENT_MESSAGES_TOOL: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(r#"{"type": "function", "name": "recent_messages", "strict": true, "description": "This chat's earlier messages, word for word, from the chat log on this Mac: the owner's, yours, and the notes of work you handed to an agent. Oldest first, newest last; the message you are answering now is left out. Each has back (1 is the newest before the message you are answering), and an owner's message also has owner_back (1 is the owner's last message before this one, 2 the one before it). When your last reply quoted an owner's message, your_last_reply_quoted is that message and before_that is the owner's message before it: the answer to \"before that\". before_owner_back: read back from the owner's message with this owner_back (the messages before it, it left out), or null to start at the newest. before: how many more lines to skip (0 skips none). count: how many to return, 1 to 20. For a stretch of time (\"what happened last night\", \"this morning\", \"yesterday\"), pass since and until instead, in the owner's local time as YYYY-MM-DDTHH:MM (until null for now): every message in it comes back at once, oldest first, so a summary of that time covers all of it. \"Last night\" is from 18:00 the day before to 04:00.", "parameters": {"type": "object", "additionalProperties": false, "required": ["count", "before", "before_owner_back", "since", "until"], "properties": {"count": {"type": ["integer", "null"]}, "before": {"type": ["integer", "null"]}, "before_owner_back": {"type": ["integer", "null"]}, "since": {"type": ["string", "null"]}, "until": {"type": ["string", "null"]}}}}"#).expect("RECENT_MESSAGES_TOOL"));

/// py: telegram.py:1254 `def _local_time(raw: Any) -> Optional[float]:`
///
/// A time the model wrote (ISO 8601, the owner's local time unless it carries an offset) as epoch seconds, or
/// None when there is none or it cannot be read.
pub(crate) fn local_time(raw: &Value) -> Option<f64> {
    todo!("telegram.py:1254 _local_time")
}

/// py: telegram.py:1267 `def _words(text: str) -> list[str]:`
pub(crate) fn words(text: &str) -> Vec<String> {
    todo!("telegram.py:1267 _words")
}

/// py: telegram.py:1271 `def _quote_run(said: str, reply_words: Sequence[str]) -> int:`
///
/// How much of an owner's message ``said`` a reply quotes: the longest run of its words found in a row in the
/// reply, when that run counts as a quote (the whole message, or 3 words or more and half of it or more), else 0.
/// A reply quotes part of a message as often as all of it: "my locker code is 4417" for "(memory test, just
/// reply ok) my locker code is 4417" (live, 2026-10-08).
pub(crate) fn quote_run(said: &str, reply_words: &[String]) -> i64 {
    todo!("telegram.py:1271 _quote_run")
}

/// py: telegram.py:1291 `def quoted_line(reply: str, candidates: Sequence[Any]) -> Optional[Any]:`
///
/// The owner's line a recall answer quoted, of ``candidates`` (log lines, oldest first), or None. When it
/// quotes several, the oldest of them: "before that" then steps back from the earliest one it gave.
pub fn quoted_line(reply: &str, candidates: &[Value]) -> Option<Value> {
    todo!("telegram.py:1291 quoted_line")
}

/// py: telegram.py:1301 `def _last_quoted(lines: Sequence[Any]) -> Optional[Any]:`
///
/// The owner's line eve's last reply quoted, or None. The replies after the owner's newest message, newest
/// first: one tagged when it was written (``quotes``, set when its turn called recent_messages) is a fact; one
/// untagged is read by its words, the newest owner line before it that it quotes.
pub(crate) fn last_quoted(lines: &[Value]) -> Option<Value> {
    todo!("telegram.py:1301 _last_quoted")
}

/// py: telegram.py:1320 `def recall_line(chat_log: bool, memory: bool) -> str:`
///
/// The earlier-messages paragraph for an install: with the chat log or without, with memory or without.
pub fn recall_line(chat_log: bool, memory: bool) -> String {
    todo!("telegram.py:1320 recall_line")
}

/// py: telegram.py:1329 `def gap_note(secs: float, restart: bool) -> str:`
///
/// A quiet spell (and a restart) between two lines, as the history shows it: "(2 hours later)".
pub fn gap_note(secs: f64, restart: bool) -> String {
    todo!("telegram.py:1329 gap_note")
}

/// py: telegram.py:1343 `BRIEF_HEADER`
pub const BRIEF_HEADER: &str = "

Since you last talked: ";

/// py: telegram.py:1344 `BRIEF_RULES`
pub const BRIEF_RULES: &str = " Mention it only if it fits what they text now, in a few words, at most once. Never read it back, never ask them to confirm it, never mention having memories.";

/// py: telegram.py:1351 `class TelegramConfig:`
///
/// `Default` is the dataclass's defaults. Equality leaves `about` out (`compare=False`); `Debug` never shows the token
/// (`__repr__`): both are this file's unit's.
#[derive(Clone)]
pub struct TelegramConfig {
    pub enabled: bool,
    pub token: String,
    pub owner_ids: Vec<i64>,
    pub model: String,
    pub effort: String,
    /// an app turn graded "think" (GRADES)
    pub app_think_model: String,
    pub app_think_effort: String,
    pub stale_secs: f64,
    pub ask_timeout_secs: f64,
    pub idle_close_secs: f64,
    pub app_only: bool,
    pub ask_permissions: bool,
    /// "Thinking…" (sendMessageDraft) while a relayed Claude turn works
    pub drafts: bool,
    pub search: eve_web::websearch::SearchConfig,
    pub about: String,
}

impl TelegramConfig {
    // py: telegram.py:1373 `__repr__`: Rust trait impl, ported by this file's unit.
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            enabled: TELEGRAM_DEFAULT,
            token: String::new(),
            owner_ids: Vec::new(),
            model: DEFAULT_MODEL.into(),
            effort: DEFAULT_EFFORT.into(),
            app_think_model: DEFAULT_APP_THINK_MODEL.into(),
            app_think_effort: DEFAULT_APP_THINK_EFFORT.into(),
            stale_secs: DEFAULT_STALE_SECS,
            ask_timeout_secs: DEFAULT_ASK_TIMEOUT_SECS,
            idle_close_secs: DEFAULT_IDLE_CLOSE_SECS,
            app_only: false,
            ask_permissions: false,
            drafts: DRAFTS_DEFAULT,
            search: eve_web::websearch::SearchConfig::default(),
            about: String::new(),
        }
    }
}

/// py: telegram.py:1378 `def _owner_ids(raw: str) -> frozenset[int]:`
pub(crate) fn owner_ids(raw: &str) -> Vec<i64> {
    todo!("telegram.py:1378 _owner_ids")
}

/// py: telegram.py:1392 `def configured(environ: Any = None) -> TelegramConfig:`
///
/// ``EVE_TELEGRAM=1`` turns it on, and only together with ``EVE_TELEGRAM_TOKEN`` and at
/// least one numeric id in ``EVE_TELEGRAM_OWNER``. A door with no allowlist never opens.
/// eve lite (EVE_PROFILE=lite in ``environ``) has a brain of its own (``_lite_configured``).
pub fn configured(environ: Option<&Environ>) -> TelegramConfig {
    todo!("telegram.py:1392 configured")
}

/// py: telegram.py:1429 `def _lite_configured(env: Any) -> TelegramConfig:`
///
/// ``configured`` in eve lite: the brain of eve's web app and nothing of Telegram's. Always on and app-only, so
/// Telegram is never polled and nothing is sent to it (MutedBotApi). No token, which hide_token and MutedBotApi take
/// as nothing to hide and nothing to reach. One owner, the web app's own chat (profile.LITE_CHAT_ID), so the app's
/// chat and the watcher's alerts have a chat to be in (TelegramInlet._chat_id). The models, the efforts and the web
/// search are read as full eve reads them, and ``about`` is eve lite's block (self_context._lite_lines).
pub(crate) fn lite_configured(env: &Environ) -> TelegramConfig {
    todo!("telegram.py:1429 _lite_configured")
}

/// py: telegram.py:1445 `def _effort(env: Any, name: str, default: str) -> str:`
///
/// The reasoning effort ``name`` sets, or ``default`` (with one warning) when it is not one of EFFORTS.
pub(crate) fn effort(env: &Environ, name: &str, default: &str) -> String {
    todo!("telegram.py:1445 _effort")
}

/// py: telegram.py:1454 `def _prompt_key(text: str) -> str:`
///
/// A prompt as Claude Code's UserPromptSubmit hook reports it (stripped), whitespace runs as one space: how
/// ``relay_turn_began`` matches a turn to the line eve typed.
pub(crate) fn prompt_key(text: &str) -> String {
    todo!("telegram.py:1454 _prompt_key")
}
