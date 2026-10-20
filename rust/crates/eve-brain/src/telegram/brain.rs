//! A port of `telegram.py`, lines 1722-2193: the request, the response parse, the owner's files, the screen capture.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

use std::sync::atomic::{AtomicBool, Ordering};

// ---- the text brain ---------------------------------------------------------------------------

/// py: telegram.py:1724 `def turn_context(brief: str = "", *, clock: Optional[str] = None) -> str:`
///
/// The turn's developer note: the clock (system_context), and the opening brief worded for a chat.
/// Built once per turn and sent in every round of it, never kept in the history.
pub fn turn_context(brief: &str, clock: Option<&str>) -> String {
    let mut text = match clock {
        Some(clock) => clock.py_trim().to_string(),
        None => eve_system_context::context(None, None).py_trim().to_string(),
    };
    let brief = wire_words(brief);
    if !brief.is_empty() {
        text.push_str(BRIEF_HEADER);
        text.push_str(&brief);
        text.push_str(BRIEF_RULES);
    }
    text
}

/// py: telegram.py:1734 `def with_turn_context(items: list[dict[str, Any]], context: str) -> list[dict[str, Any]]:`
///
/// `items` with the developer note put right before the newest user message (after everything when
/// there is none). The note changes every turn, so it sits after the history, where it costs the cache
/// nothing; the model still reads it next to what it answers.
pub fn with_turn_context(items: &[Obj], context: &str) -> Vec<Obj> {
    if context.is_empty() {
        return items.to_vec();
    }
    let note = message_item("developer", context);
    for i in (0..items.len()).rev() {
        let item = &items[i];
        if item.get("type").and_then(Value::as_str) == Some("message") && item.get("role").and_then(Value::as_str) == Some("user") {
            let mut out = items[..i].to_vec();
            out.push(note);
            out.extend_from_slice(&items[i..]);
            return out;
        }
    }
    let mut out = items.to_vec();
    out.push(note);
    out
}

/// `re.sub(r"[^\w\s'-]", " ", label)`.
static NOT_WORDISH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\w\s'-]").expect("NOT_WORDISH"));

/// py: telegram.py:1748 `def spoken_question(text: str, choices: Sequence[Choice]) -> str:`
///
/// A question with buttons, as words to hear: the question, then how to answer it out loud. Buttons that
/// mean yes and no (Yes/No, Allow/Deny) are asked as "Yes or no?".
pub fn spoken_question(text: &str, choices: &[Choice]) -> String {
    if choices.iter().all(|c| c.value == "yes" || c.value == "no") {
        return format!("{text} Yes or no?");
    }
    let labels: Vec<String> = choices
        .iter()
        .map(|c| wire_words(&NOT_WORDISH.replace_all(&c.label, " ")))
        .filter(|label| !label.is_empty())
        .collect();
    format!("{text} Say {}.", labels.join(", or "))
}

static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"https?://\S+").expect("LINK"));

/// py: telegram.py:1757 `def split_for_call(text: str) -> tuple[str, str]:`
///
/// (what to say, what to text) for a reply on a phone call: the words before a line of just WRITTEN_MARK are
/// said, what follows is texted, and every link is texted too (a link read aloud is no use).
pub fn split_for_call(text: &str) -> (String, String) {
    let (head, written) = match text.split_once(WRITTEN_MARK) {
        Some((head, written)) => (head, written),
        None => (text, ""),
    };
    let (spoken, written) = (head.py_trim().to_string(), written.py_trim().to_string());
    let links: Vec<String> =
        LINK.find_iter(&spoken).map(|m| m.as_str().trim_end_matches([')', '.', ',', ';']).to_string()).collect();
    let mut parts: Vec<String> = Vec::new();
    if !written.is_empty() {
        parts.push(written.clone());
    }
    for link in links {
        if !written.contains(&link) && !link.is_empty() {
            parts.push(link);
        }
    }
    (spoken, parts.join("\n"))
}

/// A finished sentence: a stop, a bang or a question mark and the space after it, or a line break.
static SENTENCE_END: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[.!?](?:\s)|\n").expect("SENTENCE_END"));

/// py: telegram.py:1767 `class SentenceStream:`
///
/// A streamed reply into sentences: each finished sentence goes to ``say`` while the rest is still being
/// written. The written part (after WRITTEN_MARK) is never said.
///
/// Rust: fed through `&self` from a stream's text callback, so what Python reassigns is behind locks; `text` is
/// everything fed so far (`stream.text.lock().clone()`).
pub struct SentenceStream {
    pub say: Arc<dyn Fn(&str) + Send + Sync>,
    pub min_chars: i64,
    pub text: Mutex<String>,
    pub(crate) buf: Mutex<String>,
    pub(crate) stopped: AtomicBool,
}

impl SentenceStream {
    /// py: telegram.py:1771 `def __init__(self, say: Callable[[str], None], min_chars: int = 12) -> None:`
    pub fn new(say: Arc<dyn Fn(&str) + Send + Sync>, min_chars: i64) -> Self {
        Self { say, min_chars, text: Mutex::new(String::new()), buf: Mutex::new(String::new()), stopped: AtomicBool::new(false) }
    }

    /// py: telegram.py:1777 `def feed(self, delta: str) -> None:`
    pub fn feed(&self, delta: &str) {
        self.text.lock().push_str(delta);
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        let piece = {
            let mut buf = self.buf.lock();
            buf.push_str(delta);
            if let Some((said, _)) = buf.split_once(WRITTEN_MARK) {
                let said = said.to_string();
                buf.clear();
                self.stopped.store(true, Ordering::SeqCst);
                Some(said)
            } else {
                let mut cut = None;
                for m in SENTENCE_END.find_iter(&buf) {
                    if buf[..m.end()].chars().count() as i64 >= self.min_chars {
                        cut = Some(m.end());
                    }
                }
                match cut {
                    Some(cut) if cut > 0 => {
                        let head = buf[..cut].to_string();
                        buf.replace_range(..cut, "");
                        Some(head)
                    }
                    _ => None,
                }
            }
        };
        if let Some(piece) = piece {
            self.emit(&piece);
        }
    }

    /// py: telegram.py:1794 `def flush(self) -> None:`
    pub fn flush(&self) {
        let rest = std::mem::take(&mut *self.buf.lock());
        if !self.stopped.load(Ordering::SeqCst) {
            self.emit(&rest);
        }
    }

    /// py: telegram.py:1799 `def _emit(self, piece: str) -> None:`
    pub(crate) fn emit(&self, piece: &str) {
        let piece = piece.py_trim();
        if !piece.is_empty() {
            (self.say)(piece);
        }
    }

    /// `stream.text`: everything fed so far.
    pub fn text(&self) -> String {
        self.text.lock().clone()
    }
}

/// `composio_tools._multi_execute_tools(args)`: the mappings in a multi-execute's ``tools`` list.
fn multi_execute_tools(args: &Value) -> Vec<&Obj> {
    match args.get("tools") {
        Some(Value::Array(tools)) => tools.iter().filter_map(Value::as_object).collect(),
        _ => Vec::new(),
    }
}

/// `str(x or "?")` for a JSON value.
fn or_unknown(v: Option<&Value>) -> String {
    match v {
        Some(v) if wire_truthy(Some(v)) => wire_str(v),
        _ => "?".into(),
    }
}

/// py: telegram.py:1804 `def call_names(calls: Sequence[dict[str, Any]]) -> str:`
///
/// The tools of one round, for the log: names only, and a multi-execute's slugs (never arguments, which can
/// hold mail text). The office-hours turns of 2026-09-30 had to be read back from Composio's own log.
pub fn call_names(calls: &[Obj]) -> String {
    let empty = Value::Object(Obj::new());
    let mut out = Vec::new();
    for c in calls {
        let mut name = or_unknown(c.get("name"));
        let args = match c.get("args") {
            Some(a) if wire_truthy(Some(a)) => a,
            _ => &empty,
        };
        if name == eve_composio_tools::MULTI_EXECUTE {
            let slugs: Vec<String> = multi_execute_tools(args).iter().map(|t| or_unknown(t.get("tool_slug"))).collect();
            name += &format!("[{}]", slugs.join(","));
        } else if name == eve_apps_mcp::OPEN_TOOL {
            // the apps list (2026-10-04, several apps in one call) and the older single app, both logged
            let mut apps: Vec<String> = match args.get("apps") {
                Some(Value::String(one)) => vec![one.clone()],
                Some(Value::Array(many)) => many.iter().map(wire_str).collect(),
                _ => Vec::new(),
            };
            if let Some(app) = args.get("app").filter(|a| wire_truthy(Some(a))) {
                apps.push(wire_str(app));
            }
            let joined = apps.join(",");
            name += &format!("[{}]", if joined.is_empty() { "?" } else { joined.as_str() });
        }
        out.push(name);
    }
    out.join(", ")
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
#[allow(clippy::too_many_arguments)]
pub fn request(config: &TelegramConfig, items: &[Obj], brief: &str, profile: &str, app_tools: &[Obj], vault: bool, today: &str, memory_tools: &[Obj], context: Option<&str>, watch_tools: &[Obj], watch_block: &str, direct_apps: &[String], lent_block: &str, app_block: &str, recall: &str, chat_tools: &[Obj]) -> Obj {
    request_in(None, config, items, brief, profile, app_tools, vault, today, memory_tools, context, watch_tools, watch_block, direct_apps, lent_block, app_block, recall, chat_tools)
}

/// [`request`] for the environment ``environ`` (None: the process's): EVE_PROFILE, EVE_ROBOT and EVE_MENU_TOOLS are
/// read from it, as Python reads them from os.environ (the inlet lends the environment it was built with).
#[allow(clippy::too_many_arguments)]
pub fn request_in(environ: Option<&Environ>, config: &TelegramConfig, items: &[Obj], brief: &str, profile: &str, app_tools: &[Obj], vault: bool, today: &str, memory_tools: &[Obj], context: Option<&str>, watch_tools: &[Obj], watch_block: &str, direct_apps: &[String], lent_block: &str, app_block: &str, recall: &str, chat_tools: &[Obj]) -> Obj {
    let tools = tools_for_in(environ, config, memory_tools, app_tools, vault, watch_tools, chat_tools);
    let is_lite = lite(environ);
    let mut app_block = app_block.to_string();
    let mut recall = recall.to_string();
    let mut instructions: String;
    if is_lite {
        instructions = INSTRUCTIONS_LITE.to_string();
        if !app_block.is_empty() {
            app_block = lite_app_block(&app_block);
            instructions = instructions.replace(TEXT_STYLE_LITE, APP_STYLE_LITE);
        }
        recall = recall.replace(RECALL_NOTES, RECALL_NOTES_LITE); // no agent's hand-off notes in lite
    } else {
        instructions = if robot_enabled(environ) { INSTRUCTIONS_ROBOT } else { INSTRUCTIONS }.to_string();
        if eve_menu_tools::enabled(environ) {
            instructions = menu_instructions(&instructions); // the screen and the files are their own tools' (switch on)
        }
        if !app_block.is_empty() {
            instructions = app_instructions(&instructions); // the app draws lists and tables (APP_STYLE)
        }
    }
    instructions.push_str(&config.about);
    instructions.push_str(&recall);
    if !profile.is_empty() {
        instructions.push_str(PROFILE_HEADER);
        instructions.push('\n');
        instructions.push_str(profile);
    } else if !memory_tools.is_empty() {
        instructions.push_str(MEMORY_HINT);
    }
    if vault && !is_lite {
        instructions.push_str("\n\n");
        instructions.push_str(eve_second_brain::INSTRUCTIONS_BLOCK);
    }
    if !watch_block.is_empty() {
        instructions.push_str("\n\n");
        instructions.push_str(watch_block);
    }
    if !lent_block.is_empty() {
        // the routed toolsets' instructions (2026-10-05): after every block that holds still, so a toolset opened
        // by this turn's words, or mid-turn, keeps the longest cached prefix it can
        instructions.push_str("\n\n");
        instructions.push_str(lent_block);
    }
    let tool_name = |t: &Obj| t.get("name").map(wire_str).unwrap_or_default();
    if app_tools.iter().any(|t| tool_name(t).starts_with("COMPOSIO_")) {
        if is_lite {
            // from the names the bridge lists, and web_search only when this turn offers it
            let names: Vec<String> = app_tools.iter().map(tool_name).collect();
            let web = offered_names(&tools).iter().any(|n| n == eve_web::websearch::TOOL_NAME);
            instructions.push_str(&apps_block_lite(&names, web));
        } else {
            let rest = if eve_web::websearch::tools_for(&config.search).is_empty() { APPS_BLOCK_REST_NO_WEB } else { APPS_BLOCK_REST };
            instructions.push_str(APPS_BLOCK);
            instructions.push_str(&apps_direct_line(direct_apps));
            instructions.push_str(rest);
        }
    }
    if !app_block.is_empty() {
        instructions.push_str("\n\n");
        instructions.push_str(&app_block);
    }
    if !today.is_empty() {
        instructions.push_str(TODAY_HEADER);
        instructions.push_str(today);
    }
    let note = match context {
        Some(context) => context.to_string(),
        None => turn_context(brief, None),
    };
    let input: Vec<Value> = with_turn_context(items, &note).into_iter().map(Value::Object).collect();
    let tools: Vec<Value> = tools.into_iter().map(Value::Object).collect();
    let body = json!({
        "model": config.model,
        "instructions": instructions,
        "input": input,
        "tools": tools,
        "tool_choice": "auto",
        "parallel_tool_calls": false,
        "reasoning": {"effort": config.effort},
        "include": ["reasoning.encrypted_content"],
        "max_output_tokens": MAX_OUTPUT_TOKENS,
        "truncation": "auto",
        "store": false,
        "prompt_cache_key": PROMPT_CACHE_KEY,
    });
    match body {
        Value::Object(map) => map,
        _ => Obj::new(),
    }
}

/// py: telegram.py:1912 `def message_item(role: str, text: str) -> dict[str, Any]:`
pub fn message_item(role: &str, text: &str) -> Obj {
    let kind = if role == "assistant" { "output_text" } else { "input_text" };
    match json!({"type": "message", "role": role, "content": [{"type": kind, "text": text}]}) {
        Value::Object(map) => map,
        _ => Obj::new(),
    }
}

// Empty markdown links, which the hosted web search's citations can leave in a reply (live, 2026-09-24
// 23:35:17: "([]())" reached the phone). A group in parentheses holding only links with no label goes whole,
// with the space before it; a link with no label goes; a label with no address stays as plain words.

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
    let clean = |prose: &str| -> String {
        let prose = EMPTY_LINK_GROUP.replace_all(prose, "");
        let prose = UNLABELLED_LINK.replace_all(&prose, "");
        ADDRESSLESS_LINK.replace_all(&prose, "$1").into_owned()
    };
    let mut out = String::new();
    let mut at = 0;
    for code in CODE_SPAN.find_iter(text) {
        out.push_str(&clean(&text[at..code.start()])); // prose
        out.push_str(code.as_str()); // code, as written
        at = code.end();
    }
    out.push_str(&clean(&text[at..]));
    out
}

/// py: telegram.py:1937 `def offered_names(tools: Sequence[dict[str, Any]]) -> list[str]:`
///
/// The callable names in a request's tools, in order (a hosted tool without a name is left out).
pub fn offered_names(tools: &[Obj]) -> Vec<String> {
    tools.iter().filter_map(|t| t.get("name").filter(|n| wire_truthy(Some(n))).map(wire_str)).collect()
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

// A Work session's tool calls (session_tools, session_call; eve_mcp.py, docs/work-sessions.md). A session reads repos
// and the web, so every call it makes counts as coming after outside text: the gate's question names this as the read.

/// py: telegram.py:1951 `SESSION_READ`
pub const SESSION_READ: &str = "what a Claude Code session wrote";

/// py: telegram.py:1955 `SESSION_HIDDEN`
/// Tools the chat offers that a session does not get: the ones that steer eve's own chat or relays, pair the phone, or
/// start, steer or stop a computer task on the Mac's screen (a session already runs on the Mac), and the / menu's
/// hand-offs, whose result is a message to the owner's chat that ends the chat's turn.
/// A Python set: sorted here; membership is what matters.
pub const SESSION_HIDDEN: &[&str] = &["claude_relay", "codex_relay", "daily_rundown", "open_apps", "pair_eve_link", "set_stealth", "start_coding_session", "start_task", "steer_task", "stop_task"];

/// py: telegram.py:1959 `SESSION_NOT_OFFERED`
pub const SESSION_NOT_OFFERED: &str = "not run: that is not one of eve's tools for a Work session. tools/list has every tool a session may call.";

/// py: telegram.py:1961 `SESSION_NO_CHAT`
pub const SESSION_NO_CHAT: &str = "not run: eve's chat isn't running, or no owner has texted it yet, so nobody can be asked.";

/// py: telegram.py:1962 `OWNER_NO_ANSWER`
pub const OWNER_NO_ANSWER: &str = "the owner did not answer, so it did not run. Do not try it again unless the owner asks.";

// A Mac task that sends email, while Gmail is up and may send: refused before the gate asks, so the model sends it
// with Gmail's own tool, which shows the owner the whole email (apps_mcp.WHOLE_ASKS). On 2026-10-09 "Email me a test
// saying hi" went to Codex, which drafted it in the browser and asked twice. The words are sending ones only:
// "open my email" or "check my inbox" on the screen still start a task.

/// py: telegram.py:1968 `MAIL_SEND`
/// Python flags 50.
pub static MAIL_SEND: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)^\W*e-?mail\s+\S|\b(?:send|write|compose|draft|reply)\b.{0,60}\b(?:e-?mails?|mail|gmail)\b|\be-?mail\s+(?:me|him|her|them|us|my|back)\b"#).expect("MAIL_SEND"));

/// py: telegram.py:1970 `MAIL_SEND_REASON`
pub const MAIL_SEND_REASON: &str = "not started: email is sent with gmail__send_message, which shows the owner the whole email before it goes; send it with that tool this turn (open_app gmail first if it is closed).";

/// The callable a turn's AllowedNames asks about a name it does not hold (apps_mcp.AppsBridge.owns).
pub type OwnsFn = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// py: telegram.py:1974 `class AllowedNames(set):`
///
/// A turn's callable tool names: the offered ones, plus any name ``owns`` claims (apps_mcp.AppsBridge.owns: a
/// tool of a direct app that is up but not opened this turn, or one it lacks). Before 2026-10-04 a call to
/// calendar__list_events on "what's next today?" (no app word, so the calendar was not opened) failed the whole
/// turn with FAILED_LINE; now it reaches _direct_tool, which opens the app or answers with its real tools.
/// (Python bases: set.)
#[derive(Clone, Default)]
pub struct AllowedNames {
    /// the set itself
    pub(crate) names: HashSet<String>,
    pub(crate) owns: Option<OwnsFn>,
}

impl AllowedNames {
    /// py: telegram.py:1980 `def __init__(self, names: Collection[str] = (), owns: Optional[Callable[[str], bool]] = None) -> None:`
    pub fn new(names: &[String], owns: Option<Arc<dyn Fn(&str) -> bool + Send + Sync>>) -> Self {
        Self { names: names.iter().cloned().collect(), owns }
    }

    /// py: telegram.py:1984 `def __contains__(self, name: object) -> bool:`
    pub fn contains(&self, name: &str) -> bool {
        if self.names.contains(name) {
            return true;
        }
        self.owns.as_ref().is_some_and(|owns| owns(name))
    }

    /// `set.add(name)`.
    pub fn insert(&mut self, name: impl Into<String>) -> bool {
        self.names.insert(name.into())
    }

    /// The set's own names (what iterating it gives), without what ``owns`` would claim.
    pub fn names(&self) -> &HashSet<String> {
        &self.names
    }

    /// `len(names)`.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// `not names`.
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

impl std::fmt::Debug for AllowedNames {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut names: Vec<&String> = self.names.iter().collect();
        names.sort();
        f.debug_struct("AllowedNames").field("names", &names).field("owns", &self.owns.is_some()).finish()
    }
}

/// py: telegram.py:1990 `def parse_response(response: Any, allowed: Collection[str] = (), *, keep_unknown: bool = False) -> tuple[list[dict[str, Any]], str, list[dict[str, Any]]]:`
///
/// → (function calls, text, items to send back next round). Raises on a reply the loop cannot read.
/// `allowed` names the lent tools (the apps') this turn offered beside TOOL_NAMES. A call to any other name
/// raises, unless `keep_unknown`: then it is kept with ``"unknown": True`` and the turn answers it with a
/// refusal (_think; review, 2026-10-05: one wrong name threw away a whole rundown's reads). A name that is
/// empty or not a string is unknown too, kept as "" (Lean RoutedTurn P1, 2026-10-05).
///
/// Rust: Python's RuntimeError is the `Err`; [`parse_response_with`] takes an [`AllowedNames`] (with its ``owns``).
pub fn parse_response(response: &Value, allowed: &[String], keep_unknown: bool) -> anyhow::Result<(Vec<Obj>, String, Vec<Obj>)> {
    parse_response_with(response, &AllowedNames::new(allowed, None), keep_unknown)
}

/// [`parse_response`] with the turn's [`AllowedNames`], as `_turn_parts` passes it.
pub fn parse_response_with(response: &Value, allowed: &AllowedNames, keep_unknown: bool) -> anyhow::Result<(Vec<Obj>, String, Vec<Obj>)> {
    let Some(output) = response.as_object().and_then(|r| r.get("output")).and_then(Value::as_array) else {
        anyhow::bail!("malformed Responses API reply");
    };
    let mut calls: Vec<Obj> = Vec::new();
    let mut texts: Vec<String> = Vec::new();
    let mut carry: Vec<Obj> = Vec::new();
    for item in output {
        let Some(item) = item.as_object() else {
            continue;
        };
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                let raw_name = item.get("name");
                // a blank or non-string name is unknown too
                let name = raw_name.and_then(Value::as_str).unwrap_or("").to_string();
                let unknown = name.is_empty() || (!TOOL_NAMES.contains(&name.as_str()) && !allowed.contains(&name));
                if unknown && !keep_unknown {
                    let shown = raw_name.map(eve_canvas::py::py_repr).unwrap_or_else(|| "None".into());
                    anyhow::bail!("unexpected function call {shown}");
                }
                let args: Value = match item.get("arguments") {
                    Some(raw) if wire_truthy(Some(raw)) => match raw.as_str() {
                        Some(text) => serde_json::from_str(text).map_err(|_| anyhow::anyhow!("function call arguments are not JSON"))?,
                        None => anyhow::bail!("function call arguments are not JSON"),
                    },
                    _ => Value::Object(Obj::new()),
                };
                let Value::Object(args) = args else {
                    anyhow::bail!("function call arguments must be an object");
                };
                let Some(call_id) = item.get("call_id") else {
                    anyhow::bail!("'call_id'");
                };
                let mut call = Obj::new();
                call.insert("name".into(), Value::String(name));
                call.insert("call_id".into(), call_id.clone());
                call.insert("args".into(), Value::Object(args));
                if unknown {
                    call.insert("unknown".into(), Value::Bool(true));
                }
                calls.push(call);
                carry.push(item.clone());
            }
            Some("reasoning") => carry.push(item.clone()), // its encrypted_content is what makes the next round stateless
            Some("message") => {
                let parts = match item.get("content") {
                    Some(Value::Array(parts)) => parts.as_slice(),
                    Some(other) if wire_truthy(Some(other)) => anyhow::bail!("malformed Responses API reply"),
                    _ => &[],
                };
                for part in parts {
                    let Some(part) = part.as_object() else {
                        anyhow::bail!("malformed Responses API reply");
                    };
                    match part.get("type").and_then(Value::as_str) {
                        Some("output_text") if wire_truthy(part.get("text")) => match part.get("text").and_then(Value::as_str) {
                            Some(text) => texts.push(strip_empty_links(text)),
                            None => anyhow::bail!("malformed Responses API reply"),
                        },
                        Some("refusal") => {
                            let refusal = part.get("refusal").and_then(Value::as_str).filter(|r| !r.is_empty());
                            texts.push(refusal.unwrap_or("I can't do that.").to_string());
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    let text = texts.iter().map(|t| t.py_trim()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n");
    Ok((calls, text, carry))
}

/// py: telegram.py:2031 `def resolve_owner_path(raw: str, home: Optional[Path] = None) -> tuple[Optional[Path], str]:`
///
/// A path the model named → (real path, "") or (None, why not). The rules: inside the owner's home after
/// symlinks are followed, and no hidden component anywhere on the way — ~/.ssh, ~/.config, ~/.aws and their
/// kind are where the secrets live, and a chat that can reach a Mac must never be a way to read them.
pub fn resolve_owner_path(raw: &str, home: Option<&Path>) -> (Option<PathBuf>, String) {
    let home_given = home.map(Path::to_path_buf).unwrap_or_else(crate::prelude::home);
    let home = eve_transcripts::py::resolve(&home_given).unwrap_or(home_given);
    let text = raw.py_trim();
    if text.is_empty() {
        return (None, "no path given".into());
    }
    if text.contains('\0') {
        return (None, "that path cannot be read".into());
    }
    let mut path = if text == "~" || text.starts_with("~/") {
        home.join(text.get(2..).unwrap_or("")) // ~ is the same home the guard below uses
    } else {
        PathBuf::from(text)
    };
    if !path.is_absolute() {
        path = home.join(path);
    }
    let Ok(real) = eve_transcripts::py::resolve(&path) else {
        return (None, "that path cannot be read".into());
    };
    let Ok(rel) = real.strip_prefix(&home) else {
        return (None, "only files in the owner's home folder can be sent".into());
    };
    if rel.components().any(|part| part.as_os_str().to_string_lossy().starts_with('.')) {
        return (None, "hidden folders and files are off limits".into());
    }
    if !real.exists() {
        return (None, "no such file".into());
    }
    (Some(real), String::new())
}

// A task that makes a file for the owner hands it over through eve (owner, 2026-09-25: "route sending files
// through the sending call so that it always works"; a Photo Booth task saved the photo, then tried to attach it
// through a browser, was blocked, and sent only its path). The task is told to name the file's full path, and
// every file the task's result names that was made or changed while it ran is sent, as send_file sends one.

/// py: telegram.py:2064 `TASK_FILE_HINT`
pub const TASK_FILE_HINT: &str = "

(If you make or save a file the owner should get, such as a photo, a screenshot or a document, give its full path in your final answer: eve sends it to them. Do not try to attach, upload or open it in a browser yourself.)";

/// py: telegram.py:2067 `MAX_TASK_FILES`
pub const MAX_TASK_FILES: usize = 5;

/// py: telegram.py:2068 `TASK_FILE_SLACK_SECS`
/// a file saved just before the task's clock started still counts
pub const TASK_FILE_SLACK_SECS: f64 = 5.0;

/// py: telegram.py:2069 `_TASK_LINKED`
/// a Markdown link's target, or `code`
/// Python flags 32.
pub(crate) static TASK_LINKED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\]\(([^)\s]+)\)|`([^`\n]+)`"#).expect("TASK_LINKED"));

/// py: telegram.py:2070 `_TASK_BARE_START`
/// Python flags 32.
pub(crate) static TASK_BARE_START: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?:file://)?(~/|/Users/)"#).expect("TASK_BARE_START"));

/// py: telegram.py:2072 `_TASK_BARE_END`
/// where a bare path may end: after an extension, at a space, punctuation or the end
/// Python flags 32.
pub(crate) static TASK_BARE_END: LazyLock<fancy_regex::Regex> =
    LazyLock::new(|| fancy_regex::Regex::new(r#"\.[A-Za-z0-9]{1,6}(?=[\s).,;:!?'\"]|(?:\n?\z))"#).expect("TASK_BARE_END"));

/// py: telegram.py:2075 `def _task_path_candidates(text: str) -> list[list[str]]:`
///
/// Each path a result names, as the spellings it could be, shortest first. A bare path may hold spaces and
/// dots ("Eve Photo 2026-09-25.jpg", "my.photo.v2.png"), so every place it could end on its line is a
/// candidate, and the caller keeps the first that is a real file.
pub(crate) fn task_path_candidates(text: &str) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = TASK_LINKED
        .captures_iter(text)
        .filter_map(|c| c.get(1).or_else(|| c.get(2)).map(|g| vec![g.as_str().to_string()]))
        .collect();
    for m in TASK_BARE_START.captures_iter(text) {
        let Some(group) = m.get(1) else {
            continue;
        };
        let start = group.start();
        let line_end = text[start..].find('\n').map(|i| start + i).unwrap_or(text.len());
        let mut rest = &text[start..line_end];
        if let Some(next) = TASK_BARE_START.find_at(rest, 2) {
            rest = &rest[..next.start()];
        }
        let rest = wire_cut(rest, 400);
        let ends: Vec<String> = TASK_BARE_END
            .find_iter(&rest)
            .filter_map(Result::ok)
            .map(|e| rest[..e.end()].py_trim().to_string())
            .collect();
        out.push(ends);
    }
    out
}

/// `os.stat(path).st_mtime`.
fn mtime_of(meta: &std::fs::Metadata) -> f64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// py: telegram.py:2090 `def task_files(result: str, since: float, home: Optional[Path] = None) -> list[Path]:`
///
/// The files a task's result names that the task made: markdown links, `code` and bare ~/ or /Users/ paths
/// (URL-encoded or not), each inside the owner's home by resend's own rules (resolve_owner_path), a regular
/// file under Telegram's limit, and written at or after ``since`` — an old file a result merely mentions is
/// never sent. In order, without repeats, at most MAX_TASK_FILES.
pub fn task_files(result: &str, since: f64, home: Option<&Path>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for spellings in task_path_candidates(result) {
        for spelling in spellings {
            let decoded = urlencoding::decode_binary(spelling.as_bytes());
            let mut raw = String::from_utf8_lossy(&decoded).py_trim().to_string();
            if let Some(rest) = raw.strip_prefix("file://") {
                raw = rest.to_string();
            }
            if !(raw.starts_with('/') || raw.starts_with('~')) {
                continue;
            }
            let (real, _) = resolve_owner_path(&raw, home);
            let Some(real) = real.filter(|r| r.is_file()) else {
                continue;
            };
            let Ok(meta) = std::fs::metadata(&real) else {
                break;
            };
            if !out.contains(&real) && meta.len() as usize <= MAX_DOCUMENT_BYTES && mtime_of(&meta) >= since - TASK_FILE_SLACK_SECS {
                out.push(real);
            }
            break; // this path's file is found: its longer spellings are not
        }
        if out.len() >= MAX_TASK_FILES {
            break;
        }
    }
    out
}

/// py: telegram.py:2120 `def list_files(raw: str, home: Optional[Path] = None) -> dict[str, Any]:`
///
/// A folder that cannot be read lists as empty here; [`try_list_files`] returns that error as Python raises it.
pub fn list_files(raw: &str, home: Option<&Path>) -> Obj {
    try_list_files(raw, home).unwrap_or_else(|_| {
        let real = resolve_owner_path(raw, home).0.unwrap_or_default();
        match json!({"ok": true, "folder": real.display().to_string(), "files": [], "more": 0}) {
            Value::Object(map) => map,
            _ => Obj::new(),
        }
    })
}

/// `list_files` with the `OSError` Python's `real.iterdir()` raises: a folder (or one of its entries) that cannot
/// be read is an `Err`, which `_tool` answers as `list_files failed`.
pub fn try_list_files(raw: &str, home: Option<&Path>) -> std::io::Result<Obj> {
    let refuse = |why: &str| match json!({"ok": false, "reason": why}) {
        Value::Object(map) => map,
        _ => Obj::new(),
    };
    let (real, why) = resolve_owner_path(raw, home);
    let Some(real) = real else {
        return Ok(refuse(&why));
    };
    if !real.is_dir() {
        return Ok(refuse("that is a file, not a folder"));
    }
    let mut entries: Vec<(f64, String, u64)> = Vec::new();
    for entry in std::fs::read_dir(&real)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let p = entry.path();
        let Ok(meta) = std::fs::metadata(&p) else {
            continue;
        };
        let shown = if p.is_dir() { format!("{name}/") } else { name };
        entries.push((mtime_of(&meta), shown, meta.len()));
    }
    entries.sort_by(|a, b| {
        b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal).then_with(|| b.1.cmp(&a.1)).then_with(|| b.2.cmp(&a.2))
    });
    let files: Vec<Value> = entries.iter().take(MAX_LISTING).map(|(_, n, s)| json!({"name": n, "bytes": s})).collect();
    let more = entries.len().saturating_sub(MAX_LISTING);
    Ok(match json!({"ok": true, "folder": real.display().to_string(), "files": files, "more": more}) {
        Value::Object(map) => map,
        _ => Obj::new(),
    })
}

/// py: telegram.py:2140 `READ_CHARS`
/// read_file: one file's text, at most
pub const READ_CHARS: usize = 12000;

/// py: telegram.py:2141 `READ_BYTES`
/// a bigger file is refused before it is read
pub const READ_BYTES: usize = 4194304;

/// py: telegram.py:2142 `TEXT_SUFFIXES`
/// A Python set: sorted here; membership is what matters.
pub const TEXT_SUFFIXES: &[&str] = &["", ".c", ".cfg", ".conf", ".cpp", ".css", ".csv", ".env.example", ".go", ".h", ".htm", ".html", ".ini", ".java", ".js", ".json", ".jsx", ".kt", ".log", ".markdown", ".md", ".py", ".rb", ".rs", ".sh", ".sql", ".srt", ".swift", ".tex", ".toml", ".ts", ".tsv", ".tsx", ".txt", ".vtt", ".xml", ".yaml", ".yml"];

/// `PurePath.suffix`: the last dot and what follows it, "" for a name with none, that starts with its only dot, or
/// that ends in a dot.
fn py_suffix(path: &Path) -> String {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match name.rfind('.') {
        Some(i) if i > 0 && i < name.len() - 1 => name[i..].to_string(),
        _ => String::new(),
    }
}

/// py: telegram.py:2148 `def read_file(raw: str, home: Optional[Path] = None) -> dict[str, Any]:`
///
/// read_file (menu_tools.FILE_TOOLS): a plain-text file's text, under the same rules as list_files and send_file
/// (resolve_owner_path: the home folder, no hidden component). Anything else is refused with what to use.
pub fn read_file(raw: &str, home: Option<&Path>) -> Obj {
    let refuse = |why: &str| match json!({"ok": false, "reason": why}) {
        Value::Object(map) => map,
        _ => Obj::new(),
    };
    let (real, why) = resolve_owner_path(raw, home);
    let Some(real) = real else {
        return refuse(&why);
    };
    if real.is_dir() {
        return refuse("that is a folder; list_files shows what it holds");
    }
    if !real.is_file() {
        return refuse("no such file");
    }
    let suffix = py_suffix(&real);
    if !TEXT_SUFFIXES.contains(&suffix.to_lowercase().as_str()) {
        let shown = if suffix.is_empty() { "that" } else { suffix.as_str() };
        return refuse(&format!("{shown} is not a plain-text file; send_file sends it here"));
    }
    let read = || -> std::io::Result<(u64, Vec<u8>)> {
        use std::io::Read;
        let size = std::fs::metadata(&real)?.len();
        if size > READ_BYTES as u64 {
            return Ok((size, Vec::new()));
        }
        let mut data = Vec::new();
        std::fs::File::open(&real)?.take((READ_CHARS * 4) as u64).read_to_end(&mut data)?;
        Ok((size, data))
    };
    let (size, data) = match read() {
        Ok((size, _)) if size > READ_BYTES as u64 => return refuse("that file is too big to read here; send_file sends it"),
        Ok(read) => read,
        Err(e) => return refuse(&format!("could not read it ({})", eve_transcripts::py::os_error_name(&e))),
    };
    if data[..data.len().min(4096)].contains(&0) {
        return refuse("that file is not text; send_file sends it here");
    }
    let text = String::from_utf8_lossy(&data).into_owned();
    let clipped = text.chars().count() > READ_CHARS || size > data.len() as u64;
    match json!({"ok": true, "path": real.display().to_string(), "text": wire_cut(&text, READ_CHARS), "clipped": clipped}) {
        Value::Object(map) => map,
        _ => Obj::new(),
    }
}

/// py: telegram.py:2175 `def capture_screen() -> Optional[Path]:`
///
/// The screen as a JPEG file (macOS `screencapture`, the same call PyAutoGUI makes). None when it fails —
/// no Screen Recording permission, not macOS. The caller deletes the file.
pub fn capture_screen() -> Option<PathBuf> {
    let tool = which::which("screencapture").ok()?;
    let file = tempfile::Builder::new().prefix("eve-screen-").suffix(".jpg").tempfile().ok()?;
    let path = match file.keep() {
        Ok((_, path)) => path,
        Err(_) => return None,
    };
    let run = || -> Result<Option<PathBuf>, &'static str> {
        use std::io::Read;
        let mut child = std::process::Command::new(tool)
            .args(["-x", "-t", "jpg"])
            .arg(&path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| eve_transcripts::py::os_error_name(&e))?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if std::time::Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("TimeoutExpired");
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(25)),
                Err(e) => return Err(eve_transcripts::py::os_error_name(&e)),
            }
        };
        let mut stderr = Vec::new();
        if let Some(mut pipe) = child.stderr.take() {
            let _ = pipe.read_to_end(&mut stderr);
        }
        let size = std::fs::metadata(&path).map_err(|e| eve_transcripts::py::os_error_name(&e))?.len();
        if status.success() && size > 0 {
            return Ok(Some(path.clone()));
        }
        let said = String::from_utf8_lossy(&stderr).py_trim().to_string();
        tracing::warn!("telegram: screencapture failed ({})", wire_cut(&said, 120));
        Ok(None)
    };
    match run() {
        Ok(Some(path)) => return Some(path),
        Ok(None) => {}
        Err(kind) => tracing::warn!("telegram: screencapture failed ({kind})"),
    }
    let _ = std::fs::remove_file(&path);
    None
}
