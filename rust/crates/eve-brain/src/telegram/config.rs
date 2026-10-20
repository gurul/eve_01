//! A port of `telegram.py`, lines 1167-1460: the prompt headers, recall lines, TelegramConfig and configured.
//!
//! The constants and prompts are the Python values, verbatim.

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

// py: telegram.py:1221 `assert _RECALL_NOTES in RECALL_LOG_RULES`: the unit test `the_recall_notes_are_in_the_log_rules`
// below.

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
///
/// `datetime.fromisoformat` is eve-transcripts' port of it (dates, any one separator, times, offsets), and a naive
/// time is read in this computer's zone as `astimezone()` reads it (`local_aware`).
pub(crate) fn local_time(raw: &Value) -> Option<f64> {
    let text = py_str_or_empty(raw);
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let (naive, offset) = eve_transcripts::py::fromisoformat(&text.replace('Z', "+00:00"))?;
    let at = match offset {
        Some(offset) => chrono::TimeZone::from_local_datetime(&offset, &naive).single()?,
        None => local_aware(naive),
    };
    // timedelta.total_seconds(): the whole microseconds, over 10**6
    Some(at.timestamp_micros() as f64 / 1e6)
}

/// `naive.astimezone()`: a naive time read as this computer's local time. A time the clocks show twice (the hour
/// after a fall-back) is its first, as Python's fold=0 is; one they skip is eve-transcripts' reading of it (mktime's).
fn local_aware(naive: chrono::NaiveDateTime) -> chrono::DateTime<chrono::FixedOffset> {
    use chrono::TimeZone;
    match chrono::Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(at) => at.fixed_offset(),
        // chrono lists the two by offset, not by instant: the earlier instant is the one fold=0 names
        chrono::LocalResult::Ambiguous(a, b) => if a <= b { a } else { b }.fixed_offset(),
        chrono::LocalResult::None => eve_transcripts::py::local_aware(naive),
    }
}

/// `str(raw or "")`: a falsy value is "", a string is itself, anything else is Python's `str` of it.
fn py_str_or_empty(raw: &Value) -> String {
    match raw {
        Value::Null | Value::Bool(false) => String::new(),
        Value::Bool(true) => "True".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) if n.as_f64() == Some(0.0) => String::new(),
        Value::Number(n) => n.to_string(),
        Value::Array(a) if a.is_empty() => String::new(),
        Value::Object(o) if o.is_empty() => String::new(),
        // a list or a dict never reads as a time: any text that is not one will do
        other => other.to_string(),
    }
}

/// py: telegram.py:1267 `def _words(text: str) -> list[str]:`
pub(crate) fn words(text: &str) -> Vec<String> {
    static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new("[a-z0-9]+").expect("WORD"));
    let flat = text.to_lowercase().replace('’', "").replace('\'', "");
    WORD.find_iter(&flat).map(|m| m.as_str().to_string()).collect()
}

/// py: telegram.py:1271 `def _quote_run(said: str, reply_words: Sequence[str]) -> int:`
///
/// How much of an owner's message ``said`` a reply quotes: the longest run of its words found in a row in the
/// reply, when that run counts as a quote (the whole message, or 3 words or more and half of it or more), else 0.
/// A reply quotes part of a message as often as all of it: "my locker code is 4417" for "(memory test, just
/// reply ok) my locker code is 4417" (live, 2026-10-08).
pub(crate) fn quote_run(said: &str, reply_words: &[String]) -> i64 {
    let words = words(said);
    if words.is_empty() || reply_words.is_empty() {
        return 0;
    }
    let mut best = 0usize;
    let mut prev = vec![0usize; reply_words.len() + 1];
    for w in &words {
        let mut cur = vec![0usize; reply_words.len() + 1];
        for (j, r) in reply_words.iter().enumerate().map(|(i, r)| (i + 1, r)) {
            if w == r {
                cur[j] = prev[j - 1] + 1;
                best = best.max(cur[j]);
            }
        }
        prev = cur;
    }
    let whole = best == words.len() && words.concat().chars().count() >= 4;
    if whole || (best >= 3 && best * 2 >= words.len()) {
        best as i64
    } else {
        0
    }
}

/// One line of the chat log as the recall checks read it: chat_log's `Line`, or the same line as JSON (its
/// `text`, `who`, `seq` and `quotes` keys).
pub trait RecallLine {
    /// `ln.text`.
    fn line_text(&self) -> String;
    /// `ln.who`: "user", "eve" or "event".
    fn line_who(&self) -> String;
    /// `ln.seq`.
    fn line_seq(&self) -> i64;
    /// `ln.quotes`: the seq of the owner line an eve line quoted, 0 when none is known.
    fn line_quotes(&self) -> i64;
}

impl RecallLine for eve_chat_log::Line {
    fn line_text(&self) -> String {
        self.text.clone()
    }
    fn line_who(&self) -> String {
        self.who.clone()
    }
    fn line_seq(&self) -> i64 {
        self.seq
    }
    fn line_quotes(&self) -> i64 {
        self.quotes
    }
}

impl RecallLine for Value {
    fn line_text(&self) -> String {
        self.get("text").and_then(Value::as_str).unwrap_or("").to_string()
    }
    fn line_who(&self) -> String {
        self.get("who").and_then(Value::as_str).unwrap_or("").to_string()
    }
    fn line_seq(&self) -> i64 {
        self.get("seq").and_then(Value::as_i64).unwrap_or(0)
    }
    fn line_quotes(&self) -> i64 {
        self.get("quotes").and_then(Value::as_i64).unwrap_or(0)
    }
}

impl<T: RecallLine + ?Sized> RecallLine for &T {
    fn line_text(&self) -> String {
        (**self).line_text()
    }
    fn line_who(&self) -> String {
        (**self).line_who()
    }
    fn line_seq(&self) -> i64 {
        (**self).line_seq()
    }
    fn line_quotes(&self) -> i64 {
        (**self).line_quotes()
    }
}

/// py: telegram.py:1291 `def quoted_line(reply: str, candidates: Sequence[Any]) -> Optional[Any]:`
///
/// The owner's line a recall answer quoted, of ``candidates`` (log lines, oldest first), or None. When it
/// quotes several, the oldest of them: "before that" then steps back from the earliest one it gave.
pub fn quoted_line<L: RecallLine + Clone>(reply: &str, candidates: &[L]) -> Option<L> {
    let reply_words = words(reply);
    candidates.iter().find(|ln| quote_run(&ln.line_text(), &reply_words) != 0).cloned()
}

/// py: telegram.py:1301 `def _last_quoted(lines: Sequence[Any]) -> Optional[Any]:`
///
/// The owner's line eve's last reply quoted, or None. The replies after the owner's newest message, newest
/// first: one tagged when it was written (``quotes``, set when its turn called recent_messages) is a fact; one
/// untagged is read by its words, the newest owner line before it that it quotes.
pub(crate) fn last_quoted<L: RecallLine + Clone>(lines: &[L]) -> Option<L> {
    let last_owner = lines.iter().rposition(|ln| ln.line_who() == "user");
    let start = last_owner.map_or(0, |i| i + 1);
    let replies: Vec<usize> = (start..lines.len()).rev().filter(|&i| lines[i].line_who() == "eve").collect();
    // {ln.seq: ln}: a later line with the same seq is the one kept
    let mut by_seq: HashMap<i64, &L> = HashMap::new();
    for ln in lines.iter().filter(|ln| ln.line_who() == "user") {
        by_seq.insert(ln.line_seq(), ln);
    }
    for &i in &replies {
        let quotes = lines[i].line_quotes();
        if quotes != 0 {
            if let Some(ln) = by_seq.get(&quotes) {
                return Some((*ln).clone());
            }
        }
    }
    let first = *replies.first()?;
    let reply_words = words(&lines[first].line_text());
    lines[..first]
        .iter()
        .rev()
        .find(|ln| ln.line_who() == "user" && quote_run(&ln.line_text(), &reply_words) != 0)
        .cloned()
}

/// py: telegram.py:1320 `def recall_line(chat_log: bool, memory: bool) -> str:`
///
/// The earlier-messages paragraph for an install: with the chat log or without, with memory or without.
pub fn recall_line(chat_log: bool, memory: bool) -> String {
    if chat_log {
        let sources = RECALL_SOURCES_LOG.replace("{memory}", if memory { RECALL_MEMORY_LOG } else { "" });
        return format!("{}{RECALL_LOG_RULES}{RECALL_TAIL}", RECALL_HEAD.replace("{sources}", &sources));
    }
    let sources = RECALL_SOURCES_NO_LOG.replace("{memory}", if memory { RECALL_MEMORY_NO_LOG } else { "" });
    format!("{}{RECALL_NO_LOG_RULES}{RECALL_TAIL}", RECALL_HEAD.replace("{sources}", &sources))
}

/// py: telegram.py:1329 `def gap_note(secs: float, restart: bool) -> str:`
///
/// A quiet spell (and a restart) between two lines, as the history shows it: "(2 hours later)".
///
/// Python's `round` rounds a half to the even number: 90 seconds is "2 minutes", 150 is "2 minutes" too.
pub fn gap_note(secs: f64, restart: bool) -> String {
    let (n, unit) = if secs < 90.0 * 60.0 {
        ((secs / 60.0).round_ties_even().max(1.0), "minute")
    } else if secs < 36.0 * 3600.0 {
        ((secs / 3600.0).round_ties_even(), "hour")
    } else {
        ((secs / 86400.0).round_ties_even(), "day")
    };
    let n = n as i64;
    let text = format!("{n} {unit}{} later", if n == 1 { "" } else { "s" });
    if restart {
        format!("({text}, after a restart)")
    } else {
        format!("({text})")
    }
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
    /// py: telegram.py:1373 `__repr__`: a config can end up in a log line or a traceback, so never the token.
    pub fn repr(&self) -> String {
        format!(
            "TelegramConfig(enabled={}, token={}, owners={}, model={}, effort={})",
            if self.enabled { "True" } else { "False" },
            if self.token.is_empty() { "unset" } else { "set" },
            owner_set(&self.owner_ids).len(),
            eve_app_registry::py_str_repr(&self.model),
            eve_app_registry::py_str_repr(&self.effort)
        )
    }

    /// `cfg.owner_ids == frozenset({...})`: the ids as a set, whatever order they were given in.
    pub fn owners(&self) -> std::collections::BTreeSet<i64> {
        owner_set(&self.owner_ids)
    }
}

fn owner_set(ids: &[i64]) -> std::collections::BTreeSet<i64> {
    ids.iter().copied().collect()
}

impl std::fmt::Debug for TelegramConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.repr())
    }
}

/// The dataclass's `__eq__`: every field but `about` (`compare=False`); `owner_ids` is a frozenset, so as a set.
impl PartialEq for TelegramConfig {
    fn eq(&self, other: &Self) -> bool {
        self.enabled == other.enabled
            && self.token == other.token
            && owner_set(&self.owner_ids) == owner_set(&other.owner_ids)
            && self.model == other.model
            && self.effort == other.effort
            && self.app_think_model == other.app_think_model
            && self.app_think_effort == other.app_think_effort
            && self.stale_secs == other.stale_secs
            && self.ask_timeout_secs == other.ask_timeout_secs
            && self.idle_close_secs == other.idle_close_secs
            && self.app_only == other.app_only
            && self.ask_permissions == other.ask_permissions
            && self.drafts == other.drafts
            && self.search == other.search
    }
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
///
/// A frozenset in Python: here the ids sorted, each once.
pub(crate) fn owner_ids(raw: &str) -> Vec<i64> {
    let mut ids = std::collections::BTreeSet::new();
    for piece in raw.replace(';', ",").split(',') {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        match py_positive_int(piece) {
            Some(id) => {
                ids.insert(id);
            }
            None => {
                // A @username is not an identity: it can be changed and re-registered. Numbers only.
                tracing::warn!("telegram: EVE_TELEGRAM_OWNER has an entry that is not a numeric user id; ignored");
            }
        }
    }
    ids.into_iter().collect()
}

/// `piece.isdigit() and int(piece) > 0`, as the id: ASCII decimal digits, above 0, within i64 (every Telegram id is).
fn py_positive_int(piece: &str) -> Option<i64> {
    if piece.is_empty() || !piece.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    piece.parse::<i64>().ok().filter(|&n| n > 0)
}

/// `(env.get(name) or default).strip()`.
fn env_str(environ: Option<&Environ>, name: &str, default: &str) -> String {
    match env_get(environ, name) {
        Some(v) if !v.is_empty() => v.trim().to_string(),
        _ => default.trim().to_string(),
    }
}

/// `(env.get(name) or default).strip().lower() in ("1", "true", "yes", "on")`.
fn env_flag(environ: Option<&Environ>, name: &str, default: &str) -> bool {
    matches!(env_str(environ, name, default).to_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

/// `websearch.configured(env)`: the process's context, reading ``environ`` when one is lent.
fn search_configured(environ: Option<&Environ>) -> eve_web::websearch::SearchConfig {
    match environ {
        Some(env) => eve_web::websearch::configured(&eve_web::Ctx::global().clone().with_env(env.clone())),
        None => eve_web::websearch::configured(eve_web::Ctx::global()),
    }
}

/// py: telegram.py:1392 `def configured(environ: Any = None) -> TelegramConfig:`
///
/// ``EVE_TELEGRAM=1`` turns it on, and only together with ``EVE_TELEGRAM_TOKEN`` and at
/// least one numeric id in ``EVE_TELEGRAM_OWNER``. A door with no allowlist never opens.
/// eve lite (EVE_PROFILE=lite in ``environ``) has a brain of its own (``_lite_configured``).
pub fn configured(environ: Option<&Environ>) -> TelegramConfig {
    if lite(environ) {
        return match environ {
            Some(env) => lite_configured(env),
            None => lite_configured(&std::env::vars().collect()),
        };
    }
    let switch = env_str(environ, "EVE_TELEGRAM", if TELEGRAM_DEFAULT { "1" } else { "0" }).to_lowercase();
    let app_only = switch == "app";
    let wanted = app_only || matches!(switch.as_str(), "1" | "true" | "yes" | "on");
    let token = env_str(environ, "EVE_TELEGRAM_TOKEN", "");
    let owners = owner_ids(&env_get(environ, "EVE_TELEGRAM_OWNER").unwrap_or_default());
    let model = env_str(environ, "EVE_TELEGRAM_MODEL", DEFAULT_MODEL);
    let mut effort = env_str(environ, "EVE_TELEGRAM_EFFORT", DEFAULT_EFFORT).to_lowercase();
    if !EFFORTS.contains(&effort.as_str()) {
        tracing::warn!(
            "telegram: EVE_TELEGRAM_EFFORT={} is not a reasoning effort; using {}",
            eve_app_registry::py_str_repr(&effort),
            DEFAULT_EFFORT
        );
        effort = DEFAULT_EFFORT.to_string();
    }
    let think_model = env_str(environ, "EVE_APP_THINK_MODEL", DEFAULT_APP_THINK_MODEL);
    let mut think_effort = env_str(environ, "EVE_APP_THINK_EFFORT", DEFAULT_APP_THINK_EFFORT).to_lowercase();
    if !EFFORTS.contains(&think_effort.as_str()) {
        tracing::warn!(
            "telegram: EVE_APP_THINK_EFFORT={} is not a reasoning effort; using {}",
            eve_app_registry::py_str_repr(&think_effort),
            DEFAULT_APP_THINK_EFFORT
        );
        think_effort = DEFAULT_APP_THINK_EFFORT.to_string();
    }
    let ask = env_flag(environ, "EVE_TELEGRAM_ASK", "0");
    let drafts = env_flag(environ, "EVE_TELEGRAM_DRAFTS", if DRAFTS_DEFAULT { "1" } else { "0" });
    let enabled = wanted && !token.is_empty() && !owners.is_empty();
    if wanted && !enabled {
        let missing: Vec<&str> = [("EVE_TELEGRAM_TOKEN", !token.is_empty()), ("EVE_TELEGRAM_OWNER", !owners.is_empty())]
            .into_iter()
            .filter(|(_, have)| !have)
            .map(|(name, _)| name)
            .collect();
        tracing::warn!("telegram: asked for (EVE_TELEGRAM=1) but off: {} not set", missing.join(" and "));
    }
    TelegramConfig {
        enabled,
        app_only,
        token,
        owner_ids: owners,
        model,
        effort,
        app_think_model: think_model,
        app_think_effort: think_effort,
        ask_permissions: ask,
        drafts,
        search: search_configured(environ),
        about: crate::self_context::block(environ, None, None),
        ..TelegramConfig::default()
    }
}

/// py: telegram.py:1429 `def _lite_configured(env: Any) -> TelegramConfig:`
///
/// ``configured`` in eve lite: the brain of eve's web app and nothing of Telegram's. Always on and app-only, so
/// Telegram is never polled and nothing is sent to it (MutedBotApi). No token, which hide_token and MutedBotApi take
/// as nothing to hide and nothing to reach. One owner, the web app's own chat (profile.LITE_CHAT_ID), so the app's
/// chat and the watcher's alerts have a chat to be in (TelegramInlet._chat_id). The models, the efforts and the web
/// search are read as full eve reads them, and ``about`` is eve lite's block (self_context._lite_lines).
pub(crate) fn lite_configured(env: &Environ) -> TelegramConfig {
    TelegramConfig {
        enabled: true,
        app_only: true,
        token: String::new(),
        owner_ids: vec![eve_profile::LITE_CHAT_ID],
        model: env_str(Some(env), "EVE_TELEGRAM_MODEL", DEFAULT_MODEL),
        effort: effort(env, "EVE_TELEGRAM_EFFORT", DEFAULT_EFFORT),
        app_think_model: env_str(Some(env), "EVE_APP_THINK_MODEL", DEFAULT_APP_THINK_MODEL),
        app_think_effort: effort(env, "EVE_APP_THINK_EFFORT", DEFAULT_APP_THINK_EFFORT),
        search: search_configured(Some(env)),
        about: crate::self_context::block(Some(env), None, None),
        ..TelegramConfig::default()
    }
}

/// py: telegram.py:1445 `def _effort(env: Any, name: str, default: str) -> str:`
///
/// The reasoning effort ``name`` sets, or ``default`` (with one warning) when it is not one of EFFORTS.
pub(crate) fn effort(env: &Environ, name: &str, default: &str) -> String {
    let effort = env_str(Some(env), name, default).to_lowercase();
    if !EFFORTS.contains(&effort.as_str()) {
        tracing::warn!(
            "telegram: {name}={} is not a reasoning effort; using {default}",
            eve_app_registry::py_str_repr(&effort)
        );
        return default.to_string();
    }
    effort
}

/// py: telegram.py:1454 `def _prompt_key(text: str) -> str:`
///
/// A prompt as Claude Code's UserPromptSubmit hook reports it (stripped), whitespace runs as one space: how
/// ``relay_turn_began`` matches a turn to the line eve typed.
pub(crate) fn prompt_key(text: &str) -> String {
    // str.split(): Python's whitespace, which also counts the four separators \x1c-\x1f
    text.split(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Environ {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn line(seq: i64, who: &str, text: &str, quotes: i64) -> Value {
        json!({"seq": seq, "who": who, "text": text, "quotes": quotes})
    }

    #[test]
    fn the_recall_notes_are_in_the_log_rules() {
        // py: telegram.py:1221
        assert!(RECALL_LOG_RULES.contains(RECALL_NOTES));
        assert!(!RECALL_NOTES_LITE.contains("computer agent"));
    }

    #[test]
    fn words_are_lowercase_letters_and_digits_with_apostrophes_dropped() {
        assert_eq!(words("Don’t STOP, it's 4417!"), ["dont", "stop", "its", "4417"]);
        assert_eq!(words("Héllo wörld"), ["h", "llo", "w", "rld"]); // [a-z0-9] only, as Python's pattern
        assert!(words("").is_empty());
    }

    #[test]
    fn a_quote_is_the_whole_message_or_most_of_a_long_one() {
        let reply = words("You said: my locker code is 4417.");
        assert_eq!(quote_run("my locker code is 4417", &reply), 5);
        assert_eq!(quote_run("(memory test, just reply ok) my locker code is 4417", &reply), 5);
        assert_eq!(quote_run("my bike code is 1234", &reply), 0); // a run of one word is no quote
        assert_eq!(quote_run("ok", &words("ok then")), 0); // whole, but under four letters
        assert_eq!(quote_run("thanks", &words("you said thanks")), 1);
        assert_eq!(quote_run("", &reply), 0);
        assert_eq!(quote_run("anything", &[]), 0);
        // three words in a row, but under half of a long message
        assert_eq!(quote_run("one two three four five six seven eight", &words("one two three")), 0);
        assert_eq!(quote_run("one two three four five six", &words("one two three")), 3);
    }

    #[test]
    fn the_last_quoted_line_is_the_tag_when_there_is_one_else_the_words() {
        // untagged: read by its words, the newest owner line before the reply that it quotes
        let lines = vec![line(1, "user", "my locker code is 4417", 0), line(2, "eve", "ok", 0),
                         line(3, "user", "what was my last message", 0),
                         line(4, "eve", "You said: my locker code is 4417.", 0)];
        assert_eq!(last_quoted(&lines).map(|l| l["seq"].clone()), Some(json!(1)));
        // tagged: a fact, even when the words say otherwise
        let tagged = vec![line(1, "user", "alpha beta gamma", 0), line(2, "user", "delta", 0),
                          line(3, "eve", "alpha beta gamma", 2)];
        assert_eq!(last_quoted(&tagged).map(|l| l["seq"].clone()), Some(json!(2)));
        // a tag that names no owner line is passed over for the words
        let stale = vec![line(1, "user", "alpha beta gamma", 0), line(2, "eve", "alpha beta gamma", 9)];
        assert_eq!(last_quoted(&stale).map(|l| l["seq"].clone()), Some(json!(1)));
        // no reply after the owner's newest message: nothing was quoted
        assert!(last_quoted(&[line(1, "eve", "alpha", 0), line(2, "user", "hi there friend", 0)]).is_none());
        assert!(last_quoted::<Value>(&[]).is_none());
        // the replies before the owner's newest message do not count
        let old = vec![line(1, "user", "alpha beta gamma", 0), line(2, "eve", "alpha beta gamma", 1),
                       line(3, "user", "next one", 0)];
        assert!(last_quoted(&old).is_none());
    }

    #[test]
    fn a_time_the_model_wrote_is_epoch_seconds() {
        let utc = local_time(&json!("2026-10-09T18:00Z")).unwrap();
        assert_eq!(utc, 1_791_568_800.0);
        assert_eq!(local_time(&json!("2026-10-09T20:00+02:00")), Some(utc));
        assert_eq!(local_time(&json!("2026-10-09T18:00:00.5Z")), Some(utc + 0.5));
        // a naive time is this computer's local time
        let naive = chrono::NaiveDate::from_ymd_opt(2026, 10, 9).unwrap().and_hms_opt(18, 0, 0).unwrap();
        let local = chrono::TimeZone::from_local_datetime(&chrono::Local, &naive).earliest().unwrap();
        assert_eq!(local_time(&json!("2026-10-09T18:00")), Some(local.timestamp() as f64));
        for none in [json!(null), json!(""), json!("  "), json!("nonsense"), json!("2026-13-01T00:00"), json!(0),
                     json!(false)] {
            assert_eq!(local_time(&none), None, "{none}");
        }
    }

    #[test]
    fn the_owner_ids_are_numbers_above_zero() {
        assert_eq!(owner_ids("4242, 7"), [7, 4242]);
        assert_eq!(owner_ids("1;2;,3"), [1, 2, 3]);
        assert_eq!(owner_ids(" 12 ,12"), [12]);
        assert_eq!(owner_ids("007"), [7]);
        for bad in ["", "  ", "@guru", "-5", "0", "abc", "+5", "1.5"] {
            assert!(owner_ids(bad).is_empty(), "{bad:?}");
        }
    }

    #[test]
    fn an_effort_that_is_not_one_falls_back() {
        assert_eq!(effort(&env(&[("E", " HIGH ")]), "E", "low"), "high");
        assert_eq!(effort(&env(&[("E", "sideways")]), "E", "low"), "low");
        assert_eq!(effort(&env(&[("E", "")]), "E", "medium"), "medium");
        assert_eq!(effort(&env(&[]), "E", "medium"), "medium");
    }

    #[test]
    fn lite_is_configured_without_telegram() {
        let cfg = lite_configured(&env(&[("EVE_TELEGRAM_TOKEN", "t"), ("EVE_TELEGRAM_OWNER", "42"),
                                         ("EVE_TELEGRAM_EFFORT", "max")]));
        assert!(cfg.enabled && cfg.app_only && cfg.token.is_empty());
        assert_eq!(cfg.owner_ids, vec![eve_profile::LITE_CHAT_ID]);
        assert_eq!(cfg.effort, "max");
        assert!(cfg.drafts && !cfg.ask_permissions);
    }

    #[test]
    fn a_prompt_key_is_its_words_one_space_apart() {
        assert_eq!(prompt_key("  a   b\n c\t"), "a b c");
        assert_eq!(prompt_key(""), "");
        assert_eq!(prompt_key("x\x1cy"), "x y");
        assert_eq!(prompt_key("a\u{a0}b"), "a b");
        assert_eq!(prompt_key("a\u{200b}b"), "a\u{200b}b");
    }

    struct Module(Option<&'static str>);
    impl AppModule for Module {
        fn has_match(&self) -> bool {
            self.0.is_some()
        }
        fn module_match(&self, text: &str) -> Option<HubCommand> {
            (text == "go").then(|| HubCommand { action: self.0.map(str::to_string), query: None, inner: Arc::new(()) })
        }
    }

    struct Hub;
    #[async_trait::async_trait]
    impl AppHub for Hub {
        fn tools(&self) -> Vec<Value> {
            Vec::new()
        }
        fn instructions(&self, _kwargs: &Obj) -> String {
            String::new()
        }
        async fn handle(&self, _name: &str, _args: &Value) -> Value {
            Value::Null
        }
        fn match_command(&self, text: &str) -> Option<HubCommand> {
            (text == "go").then(|| HubCommand { action: Some("hub".into()), query: None, inner: Arc::new(()) })
        }
    }

    #[test]
    fn an_apps_own_match_comes_before_its_hubs() {
        let own = Module(Some("module"));
        let none = Module(None);
        let action = |c: Option<HubCommand>| c.and_then(|c| c.action);
        assert_eq!(action(code_match(Some(&own), Some(&Hub), "go")).as_deref(), Some("module"));
        // the module's match is the answer even when it finds nothing, and with no hub at all
        assert!(code_match(Some(&own), Some(&Hub), "stay").is_none());
        assert_eq!(action(code_match(Some(&own), None, "go")).as_deref(), Some("module"));
        // a module with no match of its own: the hub's
        assert_eq!(action(code_match(Some(&none), Some(&Hub), "go")).as_deref(), Some("hub"));
        assert_eq!(action(code_match(None, Some(&Hub), "go")).as_deref(), Some("hub"));
        assert!(code_match(Some(&none), None, "go").is_none());
    }

    #[test]
    fn a_flat_replace_reads_through_line_breaks_once() {
        assert_eq!(replace_flat("a b\nc d, a b c", "a b c", "X"), "X d, a b c");
        assert_eq!(replace_flat("a  b", "a b", "$1"), "$1"); // the new text is taken as it is
        assert_eq!(replace_flat("abc", "a b", "X"), "abc");
        assert_eq!(replace_flat("1+1 = 2", "1+1 =", "two is"), "two is 2");
    }
}
