//! A port of `telegram.py`, lines 1722-2193: the request, the response parse, the owner's files, the screen capture.
//!
//! Skeleton: constants and prompts are verbatim (generated from the Python values); bodies are
//! `todo!()` until this file's unit fills them.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

/// py: telegram.py:1724 `def turn_context(brief: str = "", *, clock: Optional[str] = None) -> str:`
///
/// The turn's developer note: the clock (system_context), and the opening brief worded for a chat.
/// Built once per turn and sent in every round of it, never kept in the history.
pub fn turn_context(brief: &str, clock: Option<&str>) -> String {
    todo!("telegram.py:1724 turn_context")
}

/// py: telegram.py:1734 `def with_turn_context(items: list[dict[str, Any]], context: str) -> list[dict[str, Any]]:`
///
/// `items` with the developer note put right before the newest user message (after everything when
/// there is none). The note changes every turn, so it sits after the history, where it costs the cache
/// nothing; the model still reads it next to what it answers.
pub fn with_turn_context(items: &[Obj], context: &str) -> Vec<Obj> {
    todo!("telegram.py:1734 with_turn_context")
}

/// py: telegram.py:1748 `def spoken_question(text: str, choices: Sequence[Choice]) -> str:`
///
/// A question with buttons, as words to hear: the question, then how to answer it out loud. Buttons that
/// mean yes and no (Yes/No, Allow/Deny) are asked as "Yes or no?".
pub fn spoken_question(text: &str, choices: &[Choice]) -> String {
    todo!("telegram.py:1748 spoken_question")
}

/// py: telegram.py:1757 `def split_for_call(text: str) -> tuple[str, str]:`
///
/// (what to say, what to text) for a reply on a phone call: the words before a line of just WRITTEN_MARK are
/// said, what follows is texted, and every link is texted too (a link read aloud is no use).
pub fn split_for_call(text: &str) -> (String, String) {
    todo!("telegram.py:1757 split_for_call")
}

/// py: telegram.py:1767 `class SentenceStream:`
///
/// A streamed reply into sentences: each finished sentence goes to ``say`` while the rest is still being
/// written. The written part (after WRITTEN_MARK) is never said.
pub struct SentenceStream {
    pub say: Arc<dyn Fn(&str) + Send + Sync>,
    pub min_chars: i64,
    pub text: String,
    pub(crate) buf: String,
    pub(crate) stopped: bool,
}

impl SentenceStream {
    /// py: telegram.py:1771 `def __init__(self, say: Callable[[str], None], min_chars: int = 12) -> None:`
    pub fn new(say: Arc<dyn Fn(&str) + Send + Sync>, min_chars: i64) -> Self {
        todo!("telegram.py:1771 SentenceStream.__init__")
    }

    /// py: telegram.py:1777 `def feed(self, delta: str) -> None:`
    pub fn feed(&self, delta: &str) {
        todo!("telegram.py:1777 SentenceStream.feed")
    }

    /// py: telegram.py:1794 `def flush(self) -> None:`
    pub fn flush(&self) {
        todo!("telegram.py:1794 SentenceStream.flush")
    }

    /// py: telegram.py:1799 `def _emit(self, piece: str) -> None:`
    pub(crate) fn emit(&self, piece: &str) {
        todo!("telegram.py:1799 SentenceStream._emit")
    }
}

/// py: telegram.py:1804 `def call_names(calls: Sequence[dict[str, Any]]) -> str:`
///
/// The tools of one round, for the log: names only, and a multi-execute's slugs (never arguments, which can
/// hold mail text). The office-hours turns of 2026-09-30 had to be read back from Composio's own log.
pub fn call_names(calls: &[Obj]) -> String {
    todo!("telegram.py:1804 call_names")
}

/// py: telegram.py:1825 `def request(config: TelegramConfig, items: list[dict[str, Any]], brief: str = "", profile: str = "", app_tools: Sequence[dict[str, Any]] = (), vault: bool = False, *, today: str = "", memory_tools: Sequence[dict[str, Any]] = (), context: Optional[str] = None, watch_tools: Sequence[dict[str, Any]] = (), watch_block: str = "", direct_apps: Sequence[str] = (), lent_block: str = "", app_block: str = "", recall: str = "", chat_tools: Sequence[dict[str, Any]] = ()) -> dict[str, Any]:`
///
/// The exact Responses body. Stateless: ``store=False`` and the turn's own items sent back each round
/// (with the model's reasoning as ``encrypted_content``), so nothing the owner texted is kept on OpenAI's
/// side and no ``previous_response_id`` is needed.
///
/// Ordered for the prompt cache, most stable first (owner, 2026-09-23): the instructions are INSTRUCTIONS,
/// what eve runs on (``config.about``, self_context.py: fixed from boot to restart, so it is part of the
/// stable prefix; 2026-09-29), ``recall`` (recall_line: what the model knows of earlier messages, fixed per
/// install), the profile (with a Memory lent), the second brain, the apps, then ``today`` (the day's transcript, which
/// only grows). What changes every turn — the clock and the opening ``brief`` — is not in them: it
/// is one developer message right before the newest user message (``turn_context``; pass ``context`` to
/// keep it identical across the rounds of one turn). Before, the clock sat in the middle of the
/// instructions and every text paid for the whole prompt again. With no Memory lent the instructions are
/// INSTRUCTIONS, ``config.about`` and ``recall`` only.
///
/// ``app_block``: a turn from the Eve Link app (link_chat.app_block: OpenUI Lang's component guide and the line
/// saying the app renders it). Before ``today``, which grows every turn, so consecutive app turns keep it in the
/// cached prefix; a Telegram turn has none, and its instructions are exactly what they were. An app turn also reads
/// ``APP_STYLE`` in place of the texting style (``app_instructions``), which forbids the lists the app draws.
///
/// eve lite (``_lite``): INSTRUCTIONS_LITE, with APP_STYLE_LITE for an app turn; the app block names eve's web app
/// (``lite_app_block``); the apps block names no app (``apps_block_lite``); the recall paragraph's notes are the
/// quiet spells alone (``_RECALL_NOTES_LITE``); the tools are eve lite's (``tools_for``), so the second brain's
/// block, whose tools eve lite does not have, is never added. Everything else is built as for full eve, in the
/// same order.
pub fn request(config: &TelegramConfig, items: &[Obj], brief: &str, profile: &str, app_tools: &[Obj], vault: bool, today: &str, memory_tools: &[Obj], context: Option<&str>, watch_tools: &[Obj], watch_block: &str, direct_apps: &[String], lent_block: &str, app_block: &str, recall: &str, chat_tools: &[Obj]) -> Obj {
    todo!("telegram.py:1825 request")
}

/// py: telegram.py:1912 `def message_item(role: str, text: str) -> dict[str, Any]:`
pub fn message_item(role: &str, text: &str) -> Obj {
    todo!("telegram.py:1912 message_item")
}

/// py: telegram.py:1920 `_CODE_SPAN`
/// Python flags 48.
pub(crate) static CODE_SPAN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)(```.*?```|`[^`\n]*`)"#).expect("CODE_SPAN"));

/// py: telegram.py:1921 `_EMPTY_LINK_GROUP`
/// Python flags 32.
pub(crate) static EMPTY_LINK_GROUP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"[ \t]*\(\s*\[\s*\]\([^)]*\)(?:\s*[,;]?\s*\[\s*\]\([^)]*\))*\s*\)"#).expect("EMPTY_LINK_GROUP"));

/// py: telegram.py:1922 `_UNLABELLED_LINK`
/// Python flags 32.
pub(crate) static UNLABELLED_LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"[ \t]*\[\s*\]\([^)]*\)"#).expect("UNLABELLED_LINK"));

/// py: telegram.py:1923 `_ADDRESSLESS_LINK`
/// Python flags 32.
pub(crate) static ADDRESSLESS_LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\[([^\]\n]+)\]\(\s*\)"#).expect("ADDRESSLESS_LINK"));

/// py: telegram.py:1926 `def strip_empty_links(text: str) -> str:`
///
/// `text` without empty markdown links ("([]())", "[](url)", "[label]()" → "label"). Code spans and
/// fenced blocks are left exactly as written: there "[]()" may be code.
pub fn strip_empty_links(text: &str) -> String {
    todo!("telegram.py:1926 strip_empty_links")
}

/// py: telegram.py:1937 `def offered_names(tools: Sequence[dict[str, Any]]) -> list[str]:`
///
/// The callable names in a request's tools, in order (a hosted tool without a name is left out).
pub fn offered_names(tools: &[Obj]) -> Vec<String> {
    todo!("telegram.py:1937 offered_names")
}

/// py: telegram.py:1942 `class _QuestionWaiting(Exception):`
///
/// _ask_alone refused to ask: another question is waiting on the owner.
/// (Python bases: Exception.)
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct _QuestionWaiting {
    /// `str(error)`.
    pub message: String,
}

/// py: telegram.py:1946 `QUESTION_WAITING_REASON`
pub const QUESTION_WAITING_REASON: &str = "another question is waiting for the owner; nothing was asked and nothing was done. Tell the owner to answer that question first, then try again.";

/// py: telegram.py:1951 `SESSION_READ`
pub const SESSION_READ: &str = "what a Claude Code session wrote";

/// py: telegram.py:1955 `SESSION_HIDDEN`
/// A Python set: sorted here; membership is what matters.
pub const SESSION_HIDDEN: &[&str] = &["claude_relay", "codex_relay", "daily_rundown", "open_apps", "pair_eve_link", "set_stealth", "start_coding_session", "start_task", "steer_task", "stop_task"];

/// py: telegram.py:1959 `SESSION_NOT_OFFERED`
pub const SESSION_NOT_OFFERED: &str = "not run: that is not one of eve's tools for a Work session. tools/list has every tool a session may call.";

/// py: telegram.py:1961 `SESSION_NO_CHAT`
pub const SESSION_NO_CHAT: &str = "not run: eve's chat isn't running, or no owner has texted it yet, so nobody can be asked.";

/// py: telegram.py:1962 `OWNER_NO_ANSWER`
pub const OWNER_NO_ANSWER: &str = "the owner did not answer, so it did not run. Do not try it again unless the owner asks.";

/// py: telegram.py:1968 `MAIL_SEND`
/// Python flags 50.
pub static MAIL_SEND: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)^\W*e-?mail\s+\S|\b(?:send|write|compose|draft|reply)\b.{0,60}\b(?:e-?mails?|mail|gmail)\b|\be-?mail\s+(?:me|him|her|them|us|my|back)\b"#).expect("MAIL_SEND"));

/// py: telegram.py:1970 `MAIL_SEND_REASON`
pub const MAIL_SEND_REASON: &str = "not started: email is sent with gmail__send_message, which shows the owner the whole email before it goes; send it with that tool this turn (open_app gmail first if it is closed).";

/// py: telegram.py:1974 `class AllowedNames(set):`
///
/// A turn's callable tool names: the offered ones, plus any name ``owns`` claims (apps_mcp.AppsBridge.owns: a
/// tool of a direct app that is up but not opened this turn, or one it lacks). Before 2026-10-04 a call to
/// calendar__list_events on "what's next today?" (no app word, so the calendar was not opened) failed the whole
/// turn with FAILED_LINE; now it reaches _direct_tool, which opens the app or answers with its real tools.
/// (Python bases: set.)
pub struct AllowedNames {
    pub(crate) owns: Option<Arc<dyn Fn(&str) -> bool + Send + Sync>>,
}

impl AllowedNames {
    /// py: telegram.py:1980 `def __init__(self, names: Collection[str] = (), owns: Optional[Callable[[str], bool]] = None) -> None:`
    pub fn new(names: &[String], owns: Option<Arc<dyn Fn(&str) -> bool + Send + Sync>>) -> Self {
        todo!("telegram.py:1980 AllowedNames.__init__")
    }

    /// py: telegram.py:1984 `def __contains__(self, name: object) -> bool:`
    pub fn contains(&self, name: &str) -> bool {
        todo!("telegram.py:1984 AllowedNames.__contains__")
    }
}

/// py: telegram.py:1990 `def parse_response(response: Any, allowed: Collection[str] = (), *, keep_unknown: bool = False) -> tuple[list[dict[str, Any]], str, list[dict[str, Any]]]:`
///
/// → (function calls, text, items to send back next round). Raises on a reply the loop cannot read.
/// `allowed` names the lent tools (the apps') this turn offered beside TOOL_NAMES. A call to any other name
/// raises, unless `keep_unknown`: then it is kept with ``"unknown": True`` and the turn answers it with a
/// refusal (_think; review, 2026-10-05: one wrong name threw away a whole rundown's reads). A name that is
/// empty or not a string is unknown too, kept as "" (Lean RoutedTurn P1, 2026-10-05).
pub fn parse_response(response: &Value, allowed: &[String], keep_unknown: bool) -> (Vec<Obj>, String, Vec<Obj>) {
    todo!("telegram.py:1990 parse_response")
}

/// py: telegram.py:2031 `def resolve_owner_path(raw: str, home: Optional[Path] = None) -> tuple[Optional[Path], str]:`
///
/// A path the model named → (real path, "") or (None, why not). The rules: inside the owner's home after
/// symlinks are followed, and no hidden component anywhere on the way — ~/.ssh, ~/.config, ~/.aws and their
/// kind are where the secrets live, and a chat that can reach a Mac must never be a way to read them.
pub fn resolve_owner_path(raw: &str, home: Option<&Path>) -> (Option<PathBuf>, String) {
    todo!("telegram.py:2031 resolve_owner_path")
}

/// py: telegram.py:2064 `TASK_FILE_HINT`
pub const TASK_FILE_HINT: &str = "

(If you make or save a file the owner should get, such as a photo, a screenshot or a document, give its full path in your final answer: eve sends it to them. Do not try to attach, upload or open it in a browser yourself.)";

/// py: telegram.py:2067 `MAX_TASK_FILES`
pub const MAX_TASK_FILES: usize = 5;

/// py: telegram.py:2068 `TASK_FILE_SLACK_SECS`
pub const TASK_FILE_SLACK_SECS: f64 = 5.0;

/// py: telegram.py:2069 `_TASK_LINKED`
/// Python flags 32.
pub(crate) static TASK_LINKED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\]\(([^)\s]+)\)|`([^`\n]+)`"#).expect("TASK_LINKED"));

/// py: telegram.py:2070 `_TASK_BARE_START`
/// Python flags 32.
pub(crate) static TASK_BARE_START: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?:file://)?(~/|/Users/)"#).expect("TASK_BARE_START"));

/// py: telegram.py:2072 `_TASK_BARE_END`
/// Python flags 32.
pub(crate) static TASK_BARE_END: LazyLock<fancy_regex::Regex> =
    LazyLock::new(|| fancy_regex::Regex::new(r#"\.[A-Za-z0-9]{1,6}(?=[\s).,;:!?'\"]|(?:\n?\z))"#).expect("TASK_BARE_END"));

/// py: telegram.py:2075 `def _task_path_candidates(text: str) -> list[list[str]]:`
///
/// Each path a result names, as the spellings it could be, shortest first. A bare path may hold spaces and
/// dots ("Eve Photo 2026-09-25.jpg", "my.photo.v2.png"), so every place it could end on its line is a
/// candidate, and the caller keeps the first that is a real file.
pub(crate) fn task_path_candidates(text: &str) -> Vec<Vec<String>> {
    todo!("telegram.py:2075 _task_path_candidates")
}

/// py: telegram.py:2090 `def task_files(result: str, since: float, home: Optional[Path] = None) -> list[Path]:`
///
/// The files a task's result names that the task made: markdown links, `code` and bare ~/ or /Users/ paths
/// (URL-encoded or not), each inside the owner's home by resend's own rules (resolve_owner_path), a regular
/// file under Telegram's limit, and written at or after ``since`` — an old file a result merely mentions is
/// never sent. In order, without repeats, at most MAX_TASK_FILES.
pub fn task_files(result: &str, since: f64, home: Option<&Path>) -> Vec<PathBuf> {
    todo!("telegram.py:2090 task_files")
}

/// py: telegram.py:2120 `def list_files(raw: str, home: Optional[Path] = None) -> dict[str, Any]:`
pub fn list_files(raw: &str, home: Option<&Path>) -> Obj {
    todo!("telegram.py:2120 list_files")
}

/// py: telegram.py:2140 `READ_CHARS`
pub const READ_CHARS: usize = 12000;

/// py: telegram.py:2141 `READ_BYTES`
/// a bigger file is refused before it is read
pub const READ_BYTES: usize = 4194304;

/// py: telegram.py:2142 `TEXT_SUFFIXES`
/// A Python set: sorted here; membership is what matters.
pub const TEXT_SUFFIXES: &[&str] = &["", ".c", ".cfg", ".conf", ".cpp", ".css", ".csv", ".env.example", ".go", ".h", ".htm", ".html", ".ini", ".java", ".js", ".json", ".jsx", ".kt", ".log", ".markdown", ".md", ".py", ".rb", ".rs", ".sh", ".sql", ".srt", ".swift", ".tex", ".toml", ".ts", ".tsv", ".tsx", ".txt", ".vtt", ".xml", ".yaml", ".yml"];

/// py: telegram.py:2148 `def read_file(raw: str, home: Optional[Path] = None) -> dict[str, Any]:`
///
/// read_file (menu_tools.FILE_TOOLS): a plain-text file's text, under the same rules as list_files and send_file
/// (resolve_owner_path: the home folder, no hidden component). Anything else is refused with what to use.
pub fn read_file(raw: &str, home: Option<&Path>) -> Obj {
    todo!("telegram.py:2148 read_file")
}

/// py: telegram.py:2175 `def capture_screen() -> Optional[Path]:`
///
/// The screen as a JPEG file (macOS `screencapture`, the same call PyAutoGUI makes). None when it fails —
/// no Screen Recording permission, not macOS. The caller deletes the file.
pub fn capture_screen() -> Option<PathBuf> {
    todo!("telegram.py:2175 capture_screen")
}
