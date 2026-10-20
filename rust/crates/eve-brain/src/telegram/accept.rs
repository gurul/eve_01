//! A port of `telegram.py`, lines 1461-1721: per-turn context (task locals), Inbound, accept, Tap, Choice, answer words.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

// ---- Python's view of a JSON value, for the pure deciders below ----------------------------------------------

/// Python's truthiness of a JSON value: null, false, 0, "", [] and {} are false.
pub(crate) fn wire_truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `str(value)` for a JSON value: a string as itself, null as "None", a bool as "True"/"False", anything else as
/// its JSON.
pub(crate) fn wire_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        other => other.to_string(),
    }
}

/// `isinstance(v, int)` (a bool counts, as in Python) → its value.
pub(crate) fn wire_int(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Number(n) if n.is_i64() || n.is_u64() => n.as_i64(),
        _ => None,
    }
}

/// `isinstance(v, int) and not isinstance(v, bool)` → its value.
pub(crate) fn wire_int_not_bool(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Number(n) if n.is_i64() || n.is_u64() => n.as_i64(),
        _ => None,
    }
}

/// `s[:n]` by code points.
pub(crate) fn wire_cut(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((at, _)) => s[..at].to_string(),
        None => s.to_string(),
    }
}

/// `" ".join(s.split())`.
pub(crate) fn wire_words(s: &str) -> String {
    s.split(eve_envfile::is_py_space).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

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
    let mut out = payload.clone();
    match grade {
        "instant" => {
            let tools: Vec<Value> = match payload.get("tools") {
                Some(Value::Array(tools)) => tools
                    .iter()
                    .filter(|t| t.get("name").and_then(Value::as_str) != Some("think_hard"))
                    .cloned()
                    .collect(),
                _ => Vec::new(),
            };
            out.insert("model".into(), json!(config.model));
            out.insert("reasoning".into(), json!({"effort": "low"}));
            out.insert("tools".into(), Value::Array(tools));
        }
        "think" => {
            out.insert("model".into(), json!(config.app_think_model));
            out.insert("reasoning".into(), json!({"effort": config.app_think_effort}));
        }
        _ => {}
    }
    out
}

/// py: telegram.py:1502 `APP_BACKGROUND_JOBS`
/// A Python set: sorted here; membership is what matters.
pub const APP_BACKGROUND_JOBS: &[&str] = &["telegram-agent", "telegram-board", "telegram-call-warm", "telegram-claude", "telegram-codex", "telegram-codex-progress", "telegram-edit", "telegram-progress", "telegram-relay"];

/// py: telegram.py:1508 `class Inbound:`
/// Equality is Python's: chat, user, text and image only (`message_id`, `tapped` and `uploads` are `compare=False`).
#[derive(Debug, Clone, Default)]
pub struct Inbound {
    pub chat_id: i64,
    pub user_id: i64,
    pub text: String,
    pub image: Option<eve_telegram_images::Attachment>,
    /// what a reaction lands on; 0 when unknown
    pub message_id: i64,
    /// A picker's tap, fed to _handle as its words: it means that picker's choice and nothing else, so it is
    /// never taken as the answer to a different question that happens to be waiting.
    pub tapped: bool,
    /// Attachments from the Eve Link app (link_uploads.Upload, link_chat.py): with any, ``image`` is set too, so
    /// the message takes the Telegram photo's path (_image), and _relay_image reads them from disk instead of
    /// downloading.
    pub uploads: Vec<eve_link_uploads::Upload>,
}

impl PartialEq for Inbound {
    fn eq(&self, other: &Self) -> bool {
        self.chat_id == other.chat_id && self.user_id == other.user_id && self.text == other.text && self.image == other.image
    }
}

impl Inbound {
    /// `Inbound(chat_id, user_id, text)`.
    pub fn new(chat_id: i64, user_id: i64, text: impl Into<String>) -> Self {
        Self { chat_id, user_id, text: text.into(), ..Self::default() }
    }
}

/// py: telegram.py:1523 `def _attached(inbound: Inbound) -> str:`
///
/// The history's note for what came with a message: "[image attached]", as Telegram's, or what the app sent.
pub(crate) fn attached(inbound: &Inbound) -> String {
    if inbound.uploads.is_empty() {
        return "[image attached]".into();
    }
    let pics = inbound.uploads.iter().filter(|u| u.kind == "image").count();
    let mut parts: Vec<String> = Vec::new();
    if pics > 0 {
        parts.push(if pics > 1 { format!("{pics} images") } else { "image".into() });
    }
    parts.extend(inbound.uploads.iter().filter(|u| u.kind != "image").map(|u| format!("file {}", u.name)));
    format!("[{} attached]", parts.join(", "))
}

/// py: telegram.py:1533 `def _att_of(inbound: Inbound) -> Optional[list[dict[str, str]]]:`
///
/// The transcript's ``att`` for an app message's attachments, or None.
pub(crate) fn att_of(inbound: &Inbound) -> Option<Vec<HashMap<String, String>>> {
    if inbound.uploads.is_empty() {
        return None;
    }
    Some(
        eve_link::link_chat::att_record(&inbound.uploads)
            .into_iter()
            .map(|entry| match entry {
                Value::Object(map) => map.into_iter().map(|(k, v)| (k, wire_str(&v))).collect(),
                _ => HashMap::new(),
            })
            .collect(),
    )
}

/// py: telegram.py:1544 `OK`
pub const OK: &str = "ok";

/// py: telegram.py:1544 `NOT_TEXT`
pub const NOT_TEXT: &str = "not-text";

/// py: telegram.py:1544 `FORWARDED`
pub const FORWARDED: &str = "forwarded";

const FORWARD_KEYS: [&str; 5] = ["forward_origin", "forward_from", "forward_from_chat", "forward_sender_name", "forward_date"];

/// py: telegram.py:1547 `def accept(update: Any, config: TelegramConfig, now: float) -> tuple[str, Optional[Inbound]]:`
///
/// Decide one update. Pure. ``now`` is wall-clock seconds (Telegram stamps messages in Unix time).
///
/// accept() verdicts. OK carries an Inbound; the two *_LINE verdicts are the owner, so they get one fixed
/// line back; every other verdict is a drop: no reply, no model call, the sender learns nothing.
pub fn accept(update: &Value, config: &TelegramConfig, now: f64) -> (String, Option<Inbound>) {
    let verdict = |v: &str| (v.to_string(), None);
    let Some(update) = update.as_object() else {
        return verdict("malformed");
    };
    let Some(msg) = update.get("message").and_then(Value::as_object) else {
        return verdict("not-a-new-message"); // edited_message, channel_post, callback_query, …
    };
    let (Some(sender), Some(chat)) =
        (msg.get("from").and_then(Value::as_object), msg.get("chat").and_then(Value::as_object))
    else {
        return verdict("malformed");
    };
    let (Some(user_id), Some(chat_id)) = (wire_int_not_bool(sender.get("id")), wire_int(chat.get("id"))) else {
        return verdict("malformed");
    };
    if !config.owner_ids.contains(&user_id) {
        return verdict("stranger");
    }
    if wire_truthy(sender.get("is_bot")) {
        return verdict("bot");
    }
    if chat.get("type").and_then(Value::as_str) != Some("private") || chat_id != user_id {
        return verdict("not-private"); // a group the owner is in is other people's words too
    }
    let date = match msg.get("date") {
        Some(Value::Bool(b)) => Some(f64::from(u8::from(*b))),
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    };
    match date {
        Some(date) if now - date <= config.stale_secs => {}
        _ => return verdict("stale"),
    }
    let message_id = wire_int(msg.get("message_id")).unwrap_or(0);
    let inbound = Inbound { chat_id, user_id, text: String::new(), message_id, ..Inbound::default() };
    if FORWARD_KEYS.iter().any(|key| msg.contains_key(*key)) {
        return (FORWARDED.into(), Some(inbound));
    }
    let message = Value::Object(msg.clone());
    if let Some(image) = eve_telegram_images::attachment(&message) {
        let caption = msg.get("caption").and_then(Value::as_str).map(|c| c.py_trim().to_string()).unwrap_or_default();
        return (OK.into(), Some(Inbound { chat_id, user_id, text: caption, image: Some(image), message_id, ..Inbound::default() }));
    }
    match msg.get("text").and_then(Value::as_str) {
        Some(text) if !text.py_trim().is_empty() => {
            (OK.into(), Some(Inbound { chat_id, user_id, text: text.py_trim().to_string(), message_id, ..Inbound::default() }))
        }
        _ => (NOT_TEXT.into(), Some(inbound)),
    }
}

/// py: telegram.py:1585 `class Tap:`
///
/// A tap on one of eve's inline buttons, from the owner, in their private chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tap {
    pub query_id: String,
    pub chat_id: i64,
    pub user_id: i64,
    /// the button's key; "" when the query carries none
    pub data: String,
    /// the bot's message the button sits on; 0 when unknown
    pub message_id: i64,
}

/// py: telegram.py:1594 `def accept_tap(update: Any, config: TelegramConfig) -> tuple[str, Optional[Tap]]:`
///
/// Decide one callback_query update. Pure. The same rule as ``accept``: only an owner id, never a bot,
/// and only in the owner's own private chat. A tap has no date to check for staleness (its message's date
/// is when eve sent it); a stale button is caught by its key instead (TelegramInlet._on_tap).
pub fn accept_tap(update: &Value, config: &TelegramConfig) -> (String, Option<Tap>) {
    let verdict = |v: &str| (v.to_string(), None);
    let Some(update) = update.as_object() else {
        return verdict("malformed");
    };
    let Some(query) = update.get("callback_query").and_then(Value::as_object) else {
        return verdict("not-a-tap");
    };
    let msg = query.get("message");
    let (Some(sender), Some(query_id)) =
        (query.get("from").and_then(Value::as_object), query.get("id").and_then(Value::as_str))
    else {
        return verdict("malformed");
    };
    let Some(user_id) = wire_int_not_bool(sender.get("id")) else {
        return verdict("malformed");
    };
    if !config.owner_ids.contains(&user_id) {
        return verdict("stranger");
    }
    if wire_truthy(sender.get("is_bot")) {
        return verdict("bot");
    }
    let msg = msg.and_then(Value::as_object);
    let chat = msg.and_then(|m| m.get("chat")).and_then(Value::as_object);
    // `chat.get("id") != user_id` is Python's ==: True is 1 and 42.0 is 42
    let same_id = |id: Option<&Value>| match id {
        Some(Value::Number(n)) if !(n.is_i64() || n.is_u64()) => n.as_f64() == Some(user_id as f64),
        other => wire_int(other) == Some(user_id),
    };
    let private = chat.is_some_and(|c| c.get("type").and_then(Value::as_str) == Some("private") && same_id(c.get("id")));
    if !private {
        return verdict("not-private"); // a button in a group, or one whose chat Telegram did not say
    }
    let message_id = wire_int(msg.and_then(|m| m.get("message_id"))).unwrap_or(0);
    let data = query.get("data").and_then(Value::as_str).unwrap_or("").to_string();
    (OK.into(), Some(Tap { query_id: query_id.to_string(), chat_id: user_id, user_id, data, message_id }))
}

/// py: telegram.py:1623 `class Choice:`
///
/// One inline button: its ``label``, what a tap means (``value``: the answer, the line to type, the
/// words to send), its colour, and the line the prompt is edited to once it is chosen.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Choice {
    pub label: String,
    pub value: String,
    pub style: String,
    pub done: String,
}

impl Choice {
    /// `Choice(label, value, style, done)`.
    pub fn new(label: &str, value: &str, style: &str, done: &str) -> Self {
        Self { label: label.into(), value: value.into(), style: style.into(), done: done.into() }
    }

    /// `Choice(label, value)`: no colour, no done line.
    pub fn plain(label: &str, value: &str) -> Self {
        Self::new(label, value, "", "")
    }
}

/// py: telegram.py:1632 `ALLOW_DENY`
pub static ALLOW_DENY: LazyLock<Vec<Choice>> = LazyLock::new(|| {
    vec![Choice::new("Allow", "yes", "success", "Allowed."), Choice::new("Deny", "no", "danger", "Denied.")]
});

/// py: telegram.py:1633 `YES_NO`
pub static YES_NO: LazyLock<Vec<Choice>> =
    LazyLock::new(|| vec![Choice::new("Yes", "yes", "success", "Yes."), Choice::new("No", "no", "danger", "No.")]);

/// py: telegram.py:1636 `def reply_words(choices: Sequence[Choice]) -> str:`
///
/// How the Eve Link app shows a keyboard's buttons: the words that answer it, typed by the owner (the app has
/// no buttons for eve's own messages; a typed value takes the path its tap would).
pub fn reply_words(choices: &[Choice]) -> String {
    let words: Vec<String> = choices.iter().map(|c| format!("\"{}\"", c.value)).collect();
    format!("Reply {}.", words.join(" or "))
}

// A question that ends asking for a yes or a no (Composio's "yes / no?", Chrome's "yes / no", a Codex command's
// "Reply yes or no.") gets Allow and Deny; the planner's own "Should I go ahead: …?" gets Yes and No.

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

/// py: telegram.py:1647 `CODEX_CHOICES`
/// codex_computer.py's app-access prompt, piece by piece; the value is what it accepts typed.
pub static CODEX_CHOICES: LazyLock<Vec<(&'static str, Choice)>> = LazyLock::new(|| {
    vec![
        ("yes to allow once", Choice::new("Allow once", "yes", "success", "Allowed once.")),
        ("\"allow for task\"", Choice::new("Allow for this task", "allow for task", "primary", "Allowed for this task.")),
        ("\"always allow\"", Choice::new("Always allow", "always allow", "primary", "Always allowed.")),
        ("no to deny", Choice::new("Deny", "no", "danger", "Denied.")),
    ]
});

/// py: telegram.py:1653 `ASK_OPTIONS`
/// Python flags 48.
pub static ASK_OPTIONS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)^(?P<question>.+?) \((?P<options>1\. .+)\)(?:\n?\z)"#).expect("ASK_OPTIONS"));

/// `re.split(r" / (?=\d+\. )", options)`: the text cut at every " / " that a numbered option follows. The lookahead
/// consumes nothing, so " / 2. / 3. b" is cut twice (Rust's regex has no lookahead: each " / " is tested in turn).
fn option_pieces(options: &str) -> Vec<&str> {
    static NUMBERED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d+\. ").expect("NUMBERED"));
    let mut pieces = Vec::new();
    let mut start = 0;
    let mut from = 0;
    while let Some(at) = options[from..].find(" / ").map(|i| from + i) {
        let after = at + " / ".len();
        if NUMBERED.is_match(&options[after..]) {
            pieces.push(&options[start..at]);
            start = after;
        }
        from = at + 1;
    }
    pieces.push(&options[start..]);
    pieces
}

/// py: telegram.py:1656 `def answer_choices(question: str) -> tuple[Choice, ...]:`
///
/// The buttons a task's question gets, or none. Only a question whose answers are known is given
/// buttons: a free question ("which account?") is answered in words, as before.
pub fn answer_choices(question: &str) -> Vec<Choice> {
    let text = question.py_trim();
    if let Some(codex) = CODEX_REPLY.captures(text) {
        let mut chosen = Vec::new();
        for piece in codex["choices"].split(';').map(|p| p.py_trim()) {
            match CODEX_CHOICES.iter().find(|(start, _)| piece.starts_with(start)) {
                Some((_, choice)) => chosen.push(choice.clone()),
                None => return Vec::new(), // a choice this code does not know: words only, never a guess
            }
        }
        return chosen;
    }
    if YES_NO_END.is_match(text) {
        return ALLOW_DENY.clone();
    }
    if GO_AHEAD.is_match(text) {
        return YES_NO.clone();
    }
    Vec::new()
}

/// py: telegram.py:1676 `def question_options(hint: str) -> list[tuple[int, str]]:`
///
/// An AskUserQuestion hint (hooks/pretooluse._question) → its numbered options, when it is one question.
/// Several questions ("… | …") are answered one after another in the terminal, so they get no buttons.
pub fn question_options(hint: &str) -> Vec<(i64, String)> {
    let body = wire_words(hint);
    let Some(m) = ASK_OPTIONS.captures(&body) else {
        return Vec::new();
    };
    if body.contains(" | ") {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (n, piece) in option_pieces(&m["options"]).into_iter().enumerate() {
        let n = n as i64 + 1;
        let head = format!("{n}. ");
        let Some(rest) = piece.strip_prefix(&head) else {
            return Vec::new();
        };
        let label = rest.py_trim();
        if label.is_empty() {
            return Vec::new();
        }
        out.push((n, label.to_string()));
    }
    out
}

/// py: telegram.py:1691 `def sender_id(update: Any) -> Optional[int]:`
///
/// The numeric id behind an update, for the one log line a drop gets. Never the name, never the words.
pub fn sender_id(update: &Value) -> Option<i64> {
    let update = update.as_object()?;
    for key in ["message", "edited_message", "channel_post", "callback_query"] {
        if let Some(sender) = update.get(key).and_then(Value::as_object).and_then(|m| m.get("from")).and_then(Value::as_object) {
            return wire_int(sender.get("id"));
        }
    }
    None
}

/// py: telegram.py:1703 `def receipt_line(name: str, result: dict[str, Any]) -> str:`
///
/// The short line a receipt reaction stands for, sent only when the reaction cannot be set: where a
/// capture went (second_brain's own "Saved to …" line), or that a fact was starred.
pub fn receipt_line(name: &str, result: &Obj) -> String {
    if name == "remember" {
        return STARRED_LINE.into();
    }
    if name == "take_notes" {
        return if wire_truthy(result.get("already")) { "Already taking notes.".into() } else { NOTES_LINE.into() };
    }
    match result.get("line") {
        Some(line) if wire_truthy(Some(line)) => wire_str(line),
        _ => "Saved.".into(),
    }
}

/// py: telegram.py:1713 `def _tool_line(result: Any) -> str:`
///
/// A tool result as one transcript line: its JSON, at most TOOL_LINE_CHARS.
pub(crate) fn tool_line(result: &Value) -> String {
    let text = eve_computer_agent::py::dumps_unicode(result);
    if text.chars().count() <= TOOL_LINE_CHARS {
        text
    } else {
        wire_cut(&text, TOOL_LINE_CHARS - 1) + "…"
    }
}
