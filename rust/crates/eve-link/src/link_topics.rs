//! Horizontal History: eve's one conversation cut into topics the app shows as cards, and the ones the owner saved.
//! A port of `link_topics.py`.
//!
//! Owner, 2026-10-13: past topics "organized as swipeable horizontal cards. Users can bookmark important topics and
//! quickly jump back into them" — "for our eve app", and "remember occams razor". eve has one continuous
//! conversation, so a topic is simply a run of messages with no pause longer than `GAP_SECS`. Its title is its first
//! owner message. No model is asked anything.
//!
//! Bookmarks live in `~/.config/eve-bridge/chat/bookmarks.json` (0600), keyed by the topic's first message id, so a
//! saved topic stays saved while it grows.
//!
//! Titles (owner, 2026-10-13: "use a model for the titles"): a cheap model names each topic in a few words and says
//! whether it was only small talk (`Titler`). One call per topic, made in the background after a list is served,
//! cached in `~/.config/eve-bridge/chat/topic-titles.json` (0600) by topic id and message count, so a topic is titled
//! once and again only when it grows. Until then, and with `EVE_TOPIC_TITLES=0` or no OpenRouter key, the title is
//! the owner's first message that says something and small talk is judged by the rules below.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use indexmap::IndexMap;
use regex::Regex;
use serde::Serialize;
use serde_json::Value;

use eve_transcripts::Transcripts;

use crate::seams::Obj;
use crate::Env;

/// a pause longer than this starts a new topic
pub const GAP_SECS: f64 = 45.0 * 60.0;
/// transcript days read for the cards
pub const DAYS: usize = 30;
pub const TITLE_CHARS: usize = 60;
pub const PREVIEW_CHARS: usize = 120;
pub const DEFAULT_LIMIT: usize = 30;
pub const MAX_LIMIT: usize = 60;
pub const BOOKMARKS_FILE: &str = "~/.config/eve-bridge/chat/bookmarks.json";
pub const TITLES_FILE: &str = "~/.config/eve-bridge/chat/topic-titles.json";
/// the cheap background model (docs/openrouter.md, watches)
pub const TITLE_MODEL: &str = "deepseek/deepseek-v4.1-flash";
/// topics titled per background pass, newest first
pub const TITLES_PER_PASS: usize = 8;
/// what of a topic the model reads: its start, where it is named
pub const TOPIC_CHARS: usize = 3000;
pub const TITLE_SYSTEM: &str = "You name one conversation between a person and their assistant, eve, for a card in a list of past topics. \
Reply with one JSON object only: {\"title\": string, \"small_talk\": boolean}. The title is 2 to 6 words in \
sentence case with no end punctuation and no quotes: the subject the person cared about, the way a note's \
heading reads (for example: Lisbon trip in November; Gym membership refund; Webcam angle and tracking). \
small_talk is true when the conversation is only greetings, pleasantries and checking in, with no task, \
question or information exchanged; otherwise false.";
/// a message id: <day>:<line index>
pub static ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}:\d{1,7}\n?\z").expect("ID_RE"));
/// What makes a line read badly on a card: an OpenUI program or any fence, a component action's context. Python's
/// `$` (end, or before a final newline) is `\n?\z` here: the text is then split on whitespace, so the newline either
/// way is gone.
pub static PROGRAM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"```[\s\S]*?(```|\n?\z)|\n\n\["User clicked: [\s\S]*\z"#).expect("PROGRAM")
});
/// markdown marks
pub static MARKS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[*_`#>|~\[\]]+").expect("MARKS"));
/// Small talk: a message that only says hello, thanks or ok names nothing and makes nothing important.
pub const FRIEND: &str = r"( (there|eve|friend|buddy|bro|dude|man|mate|pal))*";
pub static GREETING: LazyLock<Regex> = LazyLock::new(|| {
    let pattern = [
        r"(?i)^((hi+|hey+|hello|yo|hiya|sup|morning|night|gm|gn|good (morning|afternoon|evening|night))",
        FRIEND,
        r"|(how are (you|u|ya)|how r u|how('?s| is) it going|how('?s| are) (you|u) doing|how have you been|",
        r"what'?s up|wassup|are (you|u) (there|awake|up|ok|okay)|(you|u) there)",
        FRIEND,
        r"|(ok(ay)?|k|kk|cool|nice|great|perfect|awesome|thanks|thank (you|u)|thx|ty|lol|lmao|haha+|yes|yeah|yep|no|",
        r"nope|sure|got it|bye|cya|test(ing)?|nm|not much|good|fine|i'?m good|all good)",
        FRIEND,
        r")[\s!.,?]*\z",
    ]
    .concat();
    Regex::new(&pattern).expect("GREETING")
});
/// A short message built around a pleasantry ("It's going amazing, how are you?") is small talk too.
pub static PLEASANTRY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)\b(how are (you|u|ya)|how('?s| is) it going|what'?s (good|up|new)|how('?s| was| is) ",
        r"(your|ur) (day|night|morning|weekend)|i'?m (good|great|fine|well|doing (well|good|great))|",
        r"(it'?s|its) going (good|great|well|amazing)|nice to (meet|see) you)\b",
    ))
    .expect("PLEASANTRY")
});
pub const SMALL_TALK_WORDS: usize = 7;
/// a title says something: shorter messages (a garbled voice line) are passed over
pub const TITLE_WORDS: usize = 3;
/// an important topic: the owner said at least this many words beyond small talk,
pub const SUBSTANCE_WORDS: usize = 6;
/// or it went back and forth at least this many messages
pub const SUBSTANCE_COUNT: usize = 4;

/// Markdown marks and programs out, whitespace collapsed.
pub fn plain(text: &str) -> String {
    let no_program = PROGRAM.replace_all(text, " ");
    eve_transcripts::py::flat(&MARKS.replace_all(&no_program, " "))
}

/// `text` at most `n` characters, cut at a word with "…".
pub fn clip(text: &str, n: usize) -> String {
    use eve_transcripts::py;
    if py::len(text) <= n {
        return text.to_string();
    }
    // `text[: n - 1]`: for n = 0 that is Python's `text[:-1]`
    let head = if n == 0 { py::head(text, py::len(text).saturating_sub(1)) } else { py::head(text, n - 1) };
    let cut = match head.rfind(' ') {
        Some(i) => &head[..i],
        None => head,
    };
    format!("{}…", cut.trim_end_matches([' ', ',', '.', ';', ':', '-']))
}

/// Only a greeting, thanks or ok, or a short message built around a pleasantry.
pub fn small_talk(text: &str) -> bool {
    GREETING.is_match(text) || (words(text) <= SMALL_TALK_WORDS && PLEASANTRY.is_match(text))
}

/// One topic, as `segment` makes it. Serialized in Python's dict order after `view` popped `important` and
/// `_lines`: id, first_id, last_id, started_at, ended_at, count, title, preview, then `bookmarked` once `view` set it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Topic {
    #[serde(skip)]
    pub important: bool,
    /// `_lines`: (role, plain text) of every message, for the titler
    #[serde(skip)]
    pub lines: Vec<(String, String)>,
    pub id: String,
    pub first_id: String,
    pub last_id: String,
    pub started_at: f64,
    pub ended_at: f64,
    pub count: usize,
    pub title: String,
    pub preview: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bookmarked: Option<bool>,
}

/// Rows as link_chat::row makes them, oldest first → topics, oldest first. A run with no owner message (eve's own
/// reminders alone) is not a topic. `important`: the owner said something beyond small talk, and either enough of it
/// or the run went back and forth (owner, 2026-10-13: "important topics", "i dont need to see hi").
pub fn segment(rows: &[Obj], gap: f64) -> Vec<Topic> {
    let t_of = |r: &Obj| r.get("t").and_then(Value::as_f64).unwrap_or(0.0);
    let str_of = |r: &Obj, key: &str| r.get(key).and_then(Value::as_str).unwrap_or("").to_string();
    let mut runs: Vec<Vec<&Obj>> = Vec::new();
    for row in rows {
        match runs.last_mut() {
            Some(run) if t_of(row) - t_of(run[run.len() - 1]) <= gap => run.push(row),
            _ => runs.push(vec![row]),
        }
    }
    let mut out = Vec::new();
    for run in runs {
        let lines: Vec<(String, String)> = run.iter().map(|r| (str_of(r, "role"), plain(&str_of(r, "text")))).collect();
        let said: Vec<&str> =
            lines.iter().filter(|(role, text)| role == "owner" && !text.is_empty()).map(|(_, t)| t.as_str()).collect();
        if said.is_empty() {
            continue;
        }
        let meant: Vec<&str> = said.iter().copied().filter(|s| !small_talk(s)).collect();
        let first_owner = meant
            .iter()
            .copied()
            .find(|s| words(s) >= TITLE_WORDS)
            .unwrap_or_else(|| meant.first().copied().unwrap_or(said[0]));
        let said_words: usize = meant.iter().map(|s| words(s)).sum();
        let important = !meant.is_empty() && (said_words >= SUBSTANCE_WORDS || run.len() >= SUBSTANCE_COUNT);
        let preview = lines
            .iter()
            .rev()
            .find(|(role, text)| role == "eve" && !text.is_empty())
            .map(|(_, t)| t.as_str())
            .unwrap_or("");
        let (first, last) = (run[0], run[run.len() - 1]);
        out.push(Topic {
            important,
            title: clip(first_owner, TITLE_CHARS),
            preview: clip(preview, PREVIEW_CHARS),
            lines,
            id: str_of(first, "id"),
            first_id: str_of(first, "id"),
            last_id: str_of(last, "id"),
            started_at: t_of(first),
            ended_at: t_of(last),
            count: run.len(),
            bookmarked: None,
        });
    }
    out
}

/// Every shown message of the last `days` transcript days, oldest first. Blocking.
pub fn read_rows(store: &Transcripts, days: usize) -> Vec<Obj> {
    let all = store.days();
    let mut rows = Vec::new();
    let from = if days == 0 { 0 } else { all.len().saturating_sub(days) }; // `[-0:]` is the whole list
    for day in &all[from..] {
        rows.extend(store.lines(day).iter().enumerate().filter_map(|(i, ln)| crate::link_chat::row(day, i, ln)));
    }
    rows
}

/// The saved topics: `{"version": 1, "items": {topic id: when saved}}`.
#[derive(Debug, Clone)]
pub struct Bookmarks {
    pub path: PathBuf,
}

impl Bookmarks {
    /// `path` with `~` expanded; `BOOKMARKS_FILE` by default.
    pub fn new(path: Option<&Path>) -> Self {
        Bookmarks { path: expand(path, BOOKMARKS_FILE) }
    }

    /// The saved ids and when each was saved; an unreadable file is none.
    pub fn load(&self) -> IndexMap<String, f64> {
        let Some(Value::Object(raw)) = read_json(&self.path) else {
            return IndexMap::new();
        };
        let Some(Value::Object(items)) = raw.get("items") else {
            return IndexMap::new();
        };
        items
            .iter()
            .filter(|(k, _)| ID_RE.is_match(k))
            .filter_map(|(k, v)| py_number(v).map(|n| (k.clone(), n)))
            .collect()
    }

    /// Save or unsave `topic_id` (`now` defaults to the wall clock), written atomically (0600, its folder 0700).
    pub fn set(&self, topic_id: &str, on: bool, now: Option<f64>) -> std::io::Result<()> {
        let mut items = self.load();
        if on {
            items.entry(topic_id.to_string()).or_insert_with(|| now.unwrap_or_else(wall_secs));
        } else {
            items.shift_remove(topic_id);
        }
        let items: serde_json::Map<String, Value> = items.into_iter().map(|(k, v)| (k, Value::from(v))).collect();
        let mut body = Obj::new();
        body.insert("version".to_string(), Value::from(1));
        body.insert("items".to_string(), Value::Object(items));
        write_private(&self.path, ".bookmarks.", &Value::Object(body))
    }
}

/// A test's model: (system, text) → `{"title", "small_talk"}`.
pub type AskFn = Arc<dyn Fn(&str, &str) -> anyhow::Result<Value> + Send + Sync>;

/// Model titles for topics, cached by (topic id, message count). `apply` puts what is known on the topics; `kick`
/// starts one background pass over those that are not titled yet (one pass at a time).
pub struct Titler {
    pub path: PathBuf,
    /// EVE_TOPIC_TITLE_MODEL, else TITLE_MODEL
    pub model: String,
    /// EVE_TOPIC_TITLES is not 0/off/false/no
    pub enabled: bool,
    env: Option<Env>,
    ask: Option<AskFn>,
    running: parking_lot::Mutex<bool>,
    /// the last background pass (tests join it)
    pub thread: parking_lot::Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Titler {
    /// `path` (`TITLES_FILE` by default), `environ` (None: the process's), `ask` (tests).
    pub fn new(path: Option<&Path>, environ: Option<Env>, ask: Option<AskFn>) -> Self {
        let get = |key: &str| crate::env_get(environ.as_ref(), key).unwrap_or_default();
        let model = {
            let raw = get("EVE_TOPIC_TITLE_MODEL");
            let raw = if raw.is_empty() { TITLE_MODEL.to_string() } else { raw };
            let model = eve_transcripts::py::strip(&raw).to_string();
            if model.is_empty() {
                TITLE_MODEL.to_string()
            } else {
                model
            }
        };
        let enabled = {
            let raw = get("EVE_TOPIC_TITLES");
            let raw = if raw.is_empty() { "1".to_string() } else { raw };
            !matches!(eve_transcripts::py::strip(&raw).to_lowercase().as_str(), "0" | "off" | "false" | "no")
        };
        Titler {
            path: expand(path, TITLES_FILE),
            model,
            enabled,
            env: environ,
            ask,
            running: parking_lot::Mutex::new(false),
            thread: parking_lot::Mutex::new(None),
        }
    }

    /// The cache: `{topic id: {"n", "title", "small_talk"}}`, entries with a string title only.
    pub fn load(&self) -> IndexMap<String, Obj> {
        let Some(Value::Object(raw)) = read_json(&self.path) else {
            return IndexMap::new();
        };
        raw.into_iter()
            .filter_map(|(k, v)| match v {
                Value::Object(o) if matches!(o.get("title"), Some(Value::String(_))) => Some((k, o)),
                _ => None,
            })
            .collect()
    }

    /// Put the known titles and small-talk calls on `topics` (only where the count still matches).
    pub fn apply(&self, topics: &mut [Topic]) {
        let known = self.load();
        for t in topics.iter_mut() {
            let Some(got) = known.get(&t.id) else {
                continue;
            };
            if !same_count(got.get("n"), t.count) {
                continue;
            }
            let title = clip(got.get("title").and_then(Value::as_str).unwrap_or(""), TITLE_CHARS);
            if !title.is_empty() {
                t.title = title;
            }
            t.important = !got.get("small_talk").is_some_and(eve_transcripts::py::truthy);
        }
    }

    /// Title up to TITLES_PER_PASS untitled topics, newest first. Blocking. Returns how many were titled.
    pub fn title_pass(&self, topics: &[Topic]) -> usize {
        let mut known = self.load();
        let todo: Vec<&Topic> = topics
            .iter()
            .rev()
            .filter(|t| !same_count(known.get(&t.id).and_then(|k| k.get("n")), t.count))
            .take(TITLES_PER_PASS)
            .collect();
        let mut done = 0;
        for t in todo {
            let text = t
                .lines
                .iter()
                .filter(|(_, line)| !line.is_empty())
                .map(|(who, line)| format!("{}: {line}", if who == "owner" { "Person" } else { "eve" }))
                .collect::<Vec<_>>()
                .join("\n");
            let got = match self.call(eve_transcripts::py::head(&text, TOPIC_CHARS)) {
                Ok(Value::Object(got)) => got,
                Ok(_) => {
                    // `got.get` on something that is not a dict
                    tracing::info!("topics: a title failed (AttributeError)");
                    break;
                }
                Err(e) => {
                    // a title that fails keeps the first-message title; tried again later
                    tracing::info!("topics: a title failed ({})", error_name(&e));
                    break;
                }
            };
            let raw = match got.get("title") {
                Some(v) if eve_transcripts::py::truthy(v) => match v {
                    Value::String(s) => s.clone(),
                    other => eve_app_registry::py_repr(other),
                },
                _ => String::new(),
            };
            let title = eve_transcripts::py::flat(&raw.replace('"', "")).trim_end_matches([' ', '.']).to_string();
            if title.is_empty() {
                continue;
            }
            let mut entry = Obj::new();
            entry.insert("n".to_string(), Value::from(t.count));
            entry.insert("title".to_string(), Value::String(eve_transcripts::py::head(&title, TITLE_CHARS).to_string()));
            entry.insert("small_talk".to_string(), Value::Bool(got.get("small_talk") == Some(&Value::Bool(true))));
            known.insert(t.id.clone(), entry);
            done += 1;
        }
        if done > 0 {
            let mut merged = self.load();
            for (k, v) in known {
                merged.insert(k, v);
            }
            let body: serde_json::Map<String, Value> = merged.into_iter().map(|(k, v)| (k, Value::Object(v))).collect();
            if let Err(e) = write_private(&self.path, ".topic-titles.", &Value::Object(body)) {
                tracing::warn!("topics: the titles could not be saved ({})", eve_transcripts::py::os_error_name(&e));
            }
            tracing::info!("topics: titled {}", done);
        }
        done
    }

    /// `_call`: the test's `ask`, else one Chat Completions call on OpenRouter, priced in the spend ledger.
    fn call(&self, text: &str) -> anyhow::Result<Value> {
        if let Some(ask) = &self.ask {
            return ask(TITLE_SYSTEM, text);
        }
        let key = eve_openrouter::key(self.env.as_ref());
        if key.is_empty() {
            return Err(NoOpenRouterKey.into());
        }
        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": 80,
            "temperature": 0,
            "messages": [{"role": "system", "content": TITLE_SYSTEM}, {"role": "user", "content": text}],
            "usage": {"include": true},
        });
        if self.model == TITLE_MODEL {
            body["reasoning"] = serde_json::json!({"enabled": false}); // a short JSON reply; DeepSeek reasons by default
        }
        let client = eve_openrouter::client(Some(&key), eve_openrouter::ClientOptions::default())?;
        let resp = client.chat_completions_create(body)?;
        eve_spend::record_chat_completion(eve_spend::MEMORY, &resp, &self.model, "openrouter");
        let message = resp
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
            .and_then(|c| c.get("message"))
            .ok_or(MalformedReply)?;
        let content = match message.get("content") {
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        };
        let raw = eve_transcripts::py::strip(&content);
        let found = JSON_OBJECT.find(raw).map(|m| m.as_str()).unwrap_or(raw);
        Ok(serde_json::from_str(found)?)
    }

    fn has_key(&self) -> bool {
        !eve_openrouter::key(self.env.as_ref()).is_empty()
    }

    /// Start one background pass when titles are on and none is running. Never blocks.
    pub fn kick(self: &Arc<Self>, topics: Vec<Topic>) -> bool {
        if !self.enabled || (self.ask.is_none() && !self.has_key()) {
            return false;
        }
        {
            let mut running = self.running.lock();
            if *running {
                return false;
            }
            *running = true;
        }
        let me = Arc::clone(self);
        let spawned = std::thread::Builder::new().name("topic-titles".to_string()).spawn(move || {
            // reset the flag even when the pass panics (Python's `finally`)
            struct Reset(Arc<Titler>);
            impl Drop for Reset {
                fn drop(&mut self) {
                    *self.0.running.lock() = false;
                }
            }
            let guard = Reset(me);
            guard.0.title_pass(&topics);
        });
        match spawned {
            Ok(handle) => {
                *self.thread.lock() = Some(handle);
                true
            }
            Err(e) => {
                *self.running.lock() = false;
                tracing::warn!("topics: the title pass could not start ({})", eve_transcripts::py::os_error_name(&e));
                false
            }
        }
    }
}

/// `GET /api/link/topics`: `{"topics": the newest `limit` important topics, newest first, "bookmarked": the saved
/// ones, newest bookmark first}`, each with `bookmarked`. A saved topic whose first message was forgotten drops out.
/// Blocking.
pub fn view(store: &Transcripts, bookmarks: &Bookmarks, limit: usize, titler: Option<&Arc<Titler>>) -> Obj {
    let mut found = segment(&read_rows(store, DAYS), GAP_SECS);
    let saved = bookmarks.load();
    if let Some(titler) = titler {
        titler.apply(&mut found); // model titles and small-talk calls already made
        titler.kick(found.clone()); // and the rest, in the background, on copies the reply cannot strip
    }
    // small talk never makes a card
    let mut found: Vec<Topic> = found.into_iter().filter(|t| t.important || saved.contains_key(&t.id)).collect();
    for t in &mut found {
        t.lines.clear();
        t.bookmarked = Some(saved.contains_key(&t.id));
    }
    let as_value = |t: &Topic| serde_json::to_value(t).unwrap_or(Value::Null);
    let mut kept: Vec<(&String, f64)> =
        saved.iter().filter(|(i, _)| found.iter().any(|t| &t.id == *i)).map(|(i, at)| (i, *at)).collect();
    kept.sort_by(|a, b| b.1.total_cmp(&a.1)); // newest bookmark first; equal ones keep their order
    let bookmarked: Vec<Value> = kept
        .iter()
        .filter_map(|(i, _)| found.iter().find(|t| &t.id == *i))
        .map(as_value)
        .collect();
    let topics: Vec<Value> = found.iter().rev().take(limit).map(as_value).collect();
    let mut out = Obj::new();
    out.insert("topics".to_string(), Value::Array(topics));
    out.insert("bookmarked".to_string(), Value::Array(bookmarked));
    out
}

/// A topic that starts at `topic_id` exists in the last DAYS days.
pub fn known(store: &Transcripts, topic_id: &str) -> bool {
    segment(&read_rows(store, DAYS), GAP_SECS).iter().any(|t| t.id == topic_id)
}

// ---- private helpers ------------------------------------------------------------------------------------------

/// `re.search(r"\{[\s\S]*\}", raw)`: the JSON object in a model's reply.
static JSON_OBJECT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{[\s\S]*\}").expect("JSON_OBJECT"));

/// Python's `RuntimeError("no OpenRouter key")`.
#[derive(Debug, thiserror::Error)]
#[error("no OpenRouter key")]
struct NoOpenRouterKey;

/// A reply without `choices[0].message` (Python's IndexError / AttributeError).
#[derive(Debug, thiserror::Error)]
#[error("the reply has no message")]
struct MalformedReply;

/// `type(e).__name__` for the log line: never the error's words.
fn error_name(e: &anyhow::Error) -> &'static str {
    if e.is::<NoOpenRouterKey>() {
        return "RuntimeError";
    }
    if e.is::<MalformedReply>() {
        return "IndexError";
    }
    if e.is::<serde_json::Error>() {
        return "JSONDecodeError";
    }
    if let Some(err) = e.downcast_ref::<eve_openrouter::Error>() {
        return match err {
            eve_openrouter::Error::NoKey(_) => "NoKey",
            eve_openrouter::Error::Status { .. } => "APIStatusError",
            eve_openrouter::Error::Json(_) => "JSONDecodeError",
            _ if err.is_timeout() => "APITimeoutError",
            eve_openrouter::Error::Http(_) => "APIConnectionError",
            _ => "APIError",
        };
    }
    if let Some(io) = e.downcast_ref::<std::io::Error>() {
        return eve_transcripts::py::os_error_name(io);
    }
    "Exception"
}

/// `len(text.split())`.
fn words(text: &str) -> usize {
    text.split(eve_transcripts::py::is_space).filter(|w| !w.is_empty()).count()
}

/// `Path(path).expanduser()`, `default` when None.
fn expand(path: Option<&Path>, default: &str) -> PathBuf {
    match path {
        Some(p) => crate::expanduser(&p.to_string_lossy()),
        None => crate::expanduser(default),
    }
}

/// `json.loads(path.read_text())`, None for a file that cannot be read or is not JSON (OSError, ValueError).
fn read_json(path: &Path) -> Option<Value> {
    let raw = std::fs::read(path).ok()?;
    serde_json::from_str(std::str::from_utf8(&raw).ok()?).ok()
}

/// `isinstance(v, (int, float))` as a float: a bool is an int in Python.
fn py_number(v: &Value) -> Option<f64> {
    match v {
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

/// Python's `n == count` for a cached entry's `n` (an int, a float or a bool compare by value).
fn same_count(n: Option<&Value>, count: usize) -> bool {
    match n {
        Some(Value::Number(n)) => match (n.as_u64(), n.as_i64()) {
            (Some(u), _) => u == count as u64,
            (None, Some(_)) => false,
            _ => n.as_f64() == Some(count as f64),
        },
        Some(Value::Bool(b)) => usize::from(*b) == count,
        _ => false,
    }
}

/// `time.time()`.
fn wall_secs() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// `json.dump` through `tempfile.mkstemp(dir=<its folder>, prefix=...)`, 0600, then `os.replace`; the folder 0700.
fn write_private(path: &Path, prefix: &str, value: &Value) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    crate::link_chat::mkdir_private(&dir)?;
    let mut tmp = tempfile::Builder::new().prefix(prefix).tempfile_in(&dir)?;
    tmp.write_all(crate::link_chat::py_json(value).as_bytes())?;
    tmp.flush()?;
    std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o600))?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}
