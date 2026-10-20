//! Intent: does what the owner just said mean "go away", "be silent", or "sound back on"?
//!
//! The voice model decides for itself whether to delegate a turn, and the log shows it answering "Goodbye, pal."
//! without ever calling end_conversation: the session then sat open until the 20 s idle timer. So the session no
//! longer relies on the model to notice. Every finished user turn goes through two checks, cheapest first:
//!
//! 1. [`fast_intent`] — a phrase table for the unmistakable cases ("bye eve", "stop listening", "I want you to
//!    leave", "mute"). No network, no delay.
//! 2. A small text model (`gpt-5.4-nano` by default) for everything else, so "alright, I'm heading out" or "you can
//!    go now, little guy" also end the conversation. It answers one label with a confidence; anything under
//!    [`MIN_CONFIDENCE`] counts as no intent.
//!
//! The labels are about eve itself. "Leave the tab open", "mute Spotify" and "tell Sam goodbye" are requests about
//! the computer and must stay `none` — the table only matches when the whole turn is the phrase, and the model is
//! told the same rule.

use std::collections::HashMap;
use std::future::Future;
use std::io::Write;
use std::pin::Pin;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use regex::Regex;
use serde_json::{json, Value};

pub const LEAVE: &str = "leave";
pub const MUTE: &str = "mute";
pub const UNMUTE: &str = "unmute";
/// Turn the head, look somewhere, look around, find something with its eyes.
pub const LOOK: &str = "look";
/// Teach me, quiz me, tutor me, help me understand: open the lesson.
pub const LESSON: &str = "lesson";
/// The owner is promoting something to the permanent layer, out loud.
pub const REMEMBER: &str = "remember";
/// Start taking notes: eve records the room until told to stop.
pub const NOTES: &str = "notes";
pub const NONE: &str = "none";
pub const LABELS: [&str; 8] = [LEAVE, MUTE, UNMUTE, LOOK, LESSON, REMEMBER, NOTES, NONE];

pub const DEFAULT_MODEL: &str = "gpt-5.4-nano";
pub const MIN_CONFIDENCE: f64 = 0.6;
pub const TIMEOUT_SECS: f64 = 4.0;
/// Longer turns are requests, not dismissals; the backend owns them.
pub const MAX_WORDS: usize = 25;

/// [`TIMEOUT_SECS`] as a duration.
pub fn timeout() -> Duration {
    Duration::from_secs_f64(TIMEOUT_SECS)
}

// Words that decorate a dismissal without changing it: "okay, bye eve, thanks".
// "thank you" is folded to "thanks" first (normalize), so "you" never counts as filler:
// it is the subject of "you can go".
const LEAD: &[&str] = &[
    "ok", "okay", "alright", "right", "well", "um", "uh", "so", "hey", "yeah", "yes", "oh", "thanks", "please", "eve",
    "cool", "great", "perfect", "nice",
];
const TRAIL: &[&str] = &[
    "eve", "now", "then", "please", "thanks", "pal", "friend", "man", "dude", "mate", "little", "guy", "today", "bud",
];

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid regex")
}

static UNMUTE_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r"^(?:un ?mute(?: yourself)?|sound (?:back )?on|turn (?:your )?(?:sound|sounds|volume|beeps?) (?:back )?on",
        r"|you can (?:make (?:sounds?|noise)|beep|talk out loud)(?: again)?)$"
    ))
});
static MUTE_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r"^(?:mute(?: yourself)?|go (?:mute|silent|quiet)|silence|shh+|hush|sound off|be quiet|quiet",
        // "your", never "the": "turn the volume down" is about the Mac, not eve.
        r"|no (?:more )?(?:sounds?|noise|beeps?|beeping)|turn (?:your )?(?:sound|sounds|volume|beeps?) (?:off|down)",
        r"|stop (?:beeping|making (?:sounds?|noises?)))$"
    ))
});
// Dismissals that stand on their own anywhere in the turn: they only make sense said to eve.
static LEAVE_ANYWHERE: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r"\b(?:stop listening|leave me alone|(?:i|we) (?:want|need) you to (?:leave|go(?: away)?)",
        r"|get lost|end (?:the |this |our )?conversation|you'?re dismissed)\b"
    ))
});
// Dismissals that must be the whole (trimmed) turn: "go to sleep" alone, not "put the Mac to sleep".
static LEAVE_WHOLE: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r"^(?:(?:good ?)?bye(?: bye)?|see (?:you|ya)(?: later| soon| around)?|good ?night|farewell|later",
        r"|catch you later|talk (?:to you )?(?:later|soon)|ttyl|go away|just go|go to sleep|you can (?:go|leave)",
        r"|that'?s (?:all|it)(?: for(?: now)?)?|we'?re done(?: here)?|i'?m done(?: talking)?|dismissed|that will be all)$"
    ))
});
// Head moves: a look/turn verb first and a direction or room target last ("look a bit up and to your left", "turn
// around", "look at me"). Anything with a computer or media word is left to the backend: "turn the volume down",
// "look up the weather".
static LOOK_START: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:can|could|would|will) you |please |now |and )*(?:look|turn|glance|peek|face|spin)\b")
});
static LOOK_END: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r"\b(?:left|right|behind(?: you| yourself)?|around|round|backwards?|back here|ceiling|floor|desk|door|window",
        r"|me|up there|down there|over there|this way|that way|straight ahead|ahead|up|down)$"
    ))
});
static NOT_HEAD: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r"\b(?:volume|music|song|sound|it|them|brightness|screen|tab|page|app|light|lights|heat|tv|video|spotify",
        r"|safari|chrome|browser|file|email|mail|weather|off|on)\b"
    ))
});

// A lesson: the owner wants to be taught, quizzed, tutored or walked through something, or wants help understanding
// or working through a problem. The voice model tends to teach it itself instead of delegating, so the session routes
// these to the backend, which opens the lesson. Anchored at the start and always about the owner ("teach me"): "what
// is ownership in Rust" is a question, "teach the kids to say hi" is about other people, and both stay none.
static LESSON_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r"^(?:(?:can|could|would|will) you |please |now |and |so |maybe )*",
        r"(?:teach me|tutor me|quiz me|test me on|drill me on|coach me|walk me through",
        r"|help me (?:understand|learn|study|work through|practi[cs]e|figure out)",
        r"|(?:i|we)(?: want| wanna| would like|'d like| need) (?:to (?:learn|study|practi[cs]e|understand",
        r"|do a lesson|have a lesson|start a lesson)|a lesson)",
        r"|(?:let'?s|can we|could we|shall we) (?:do|have|start) a lesson|(?:let'?s|can we|shall we) (?:learn|study|practi[cs]e)",
        r"|(?:start|open|begin) (?:a |the |my )?lesson|lesson time)\b"
    ))
});
// "I want to learn more about this email" is a computer job, not a lesson.
static NOT_LESSON: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r"\b(?:email|mail|inbox|file|files|folder|tab|page|app|screen|window|messages?|text|texts|calendar",
        r"|spotify|safari|chrome|browser|youtube|video|song|playlist)\b"
    ))
});

// "remember that", "don't forget that", "keep that in mind", "note that down".
// Deliberately narrow: it must be a whole short clause about remembering, so
// "remember when you said" and "I remember that" do not promote anything.
// (Python applies it with re.match; every alternative starts with ^, so the outer group changes nothing.)
static REMEMBER_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r"^(?:please\s+)?(?:remember|memorize|memorise|note)\s+(?:that|this|it)?$",
        r"|^(?:please\s+)?(?:do\s*not|don'?t|never)\s+forget\s+(?:that|this|it)?$",
        r"|^keep\s+(?:that|this|it)\s+in\s+mind$",
        r"|^(?:write|note)\s+(?:that|this|it)\s+down$",
        r"|^(?:remember|note)\s+(?:that|this|it)\s+(?:for|forever|always).*$"
    ))
});

// "start taking notes", "take notes", "can you take notes", "note this down for me".
// Narrow on purpose: it starts recording the room, so it must not fire on someone
// saying the word "notes" in passing.
static NOTES_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r"(?i)\b(?:start|begin|take|taking)\s+(?:some\s+|the\s+)?not(?:e|es)\b",
        r"|\bcan you (?:take|start)\s+(?:some\s+)?not(?:e|es)\b",
        r"|\bnote[- ]?tak(?:e|ing)\b",
        r"|\bstart\s+(?:record(?:ing)?|transcri\w+)\b"
    ))
});

static NOT_ALNUM: LazyLock<Regex> = LazyLock::new(|| re(r"[^a-z0-9' ]+"));
static THANK_YOU: LazyLock<Regex> = LazyLock::new(|| re(r"\bthank you\b"));

// ---- Python string helpers -------------------------------------------------------------------------------------

/// Python's `str.isspace()` for one character: Rust's `White_Space` plus the four separators \x1c-\x1f.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python's `str.split()` with no argument.
fn py_split(s: &str) -> Vec<&str> {
    s.split(py_isspace).filter(|w| !w.is_empty()).collect()
}

/// Python's `str.strip()` with no argument.
fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

/// Python's `str.lower()` (Rust's full Unicode lowercase; the same mapping for every character eve hears).
fn py_lower(s: &str) -> String {
    s.to_lowercase()
}

/// Python's `repr()` of a str, for the `%r` / `!r` in log and report lines.
pub fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if !py_printable(c) => {
                let n = c as u32;
                if n <= 0xff {
                    out.push_str(&format!("\\x{n:02x}"));
                } else if n <= 0xffff {
                    out.push_str(&format!("\\u{n:04x}"));
                } else {
                    out.push_str(&format!("\\U{n:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `str.isprintable()` for one character, for the classes a spoken turn can carry: control characters,
/// separators other than the plain space, format characters and private use are escaped by `repr()`.
fn py_printable(c: char) -> bool {
    let n = c as u32;
    if c == ' ' {
        return true;
    }
    if n < 0x20 || (0x7f..=0xa0).contains(&n) || n == 0xad {
        return false;
    }
    if py_isspace(c) {
        return false;
    }
    !matches!(n,
        0x0600..=0x0605 | 0x061c | 0x06dd | 0x070f | 0x180e | 0x200b..=0x200f | 0x202a..=0x202e | 0x2060..=0x2064
        | 0x2066..=0x206f | 0xd800..=0xf8ff | 0xfeff | 0xfff9..=0xfffb | 0x110bd | 0x1bca0..=0x1bca3
        | 0x1d173..=0x1d17a | 0xe0001 | 0xe0020..=0xe007f | 0xf0000..=0x10ffff)
}

/// Python's `float(str)`: surrounding whitespace, `_` between digits, `inf`/`infinity`/`nan` in any case.
fn py_float_str(s: &str) -> Option<f64> {
    let t = py_strip(s);
    if t.is_empty() {
        return None;
    }
    let cleaned: String = if t.contains('_') {
        let chars: Vec<char> = t.chars().collect();
        for (i, c) in chars.iter().enumerate() {
            if *c == '_' {
                let before = i.checked_sub(1).and_then(|j| chars.get(j)).is_some_and(|p| p.is_ascii_digit());
                let after = chars.get(i + 1).is_some_and(|n| n.is_ascii_digit());
                if !(before && after) {
                    return None;
                }
            }
        }
        chars.into_iter().filter(|c| *c != '_').collect()
    } else {
        t.to_string()
    };
    // Rust also reads these spellings; Python reads only the ASCII forms checked here.
    let body = cleaned.strip_prefix(['+', '-']).unwrap_or(&cleaned);
    let lower = body.to_ascii_lowercase();
    if matches!(lower.as_str(), "inf" | "infinity" | "nan") {
        let v = match lower.as_str() {
            "nan" => f64::NAN,
            _ => f64::INFINITY,
        };
        return Some(if cleaned.starts_with('-') { -v } else { v });
    }
    if !body.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-')) {
        return None;
    }
    cleaned.parse::<f64>().ok()
}

fn py_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

// ---- the phrase table ------------------------------------------------------------------------------------------

fn is_head_move(core: &str) -> bool {
    LOOK_START.is_match(core) && LOOK_END.is_match(core) && !NOT_HEAD.is_match(core)
}

pub fn normalize(text: &str) -> String {
    let t = py_lower(text).replace(['\u{2019}', '\u{2018}'], "'");
    let t = NOT_ALNUM.replace_all(&t, " ");
    let t = py_split(&t).join(" ");
    THANK_YOU.replace_all(&t, "thanks").into_owned()
}

fn core<'a>(mut words: &'a [&'a str]) -> &'a [&'a str] {
    while let Some((first, rest)) = words.split_first() {
        if !LEAD.contains(first) {
            break;
        }
        words = rest;
    }
    while let Some((last, rest)) = words.split_last() {
        if !TRAIL.contains(last) {
            break;
        }
        words = rest;
    }
    words
}

/// The phrase table. None means "not obvious" — ask the model.
pub fn fast_intent(text: &str) -> Option<&'static str> {
    let t = normalize(text);
    if t.is_empty() {
        return None;
    }
    let words = py_split(&t);
    let core_text = core(&words).join(" ");
    // A goodbye said as a separate clause: "alright, thanks, bye" → last clause "bye". The last clause with words in
    // it: "heading home, bye." splits to a final "" after the full stop.
    let lowered = py_lower(text).replace('\u{2019}', "'");
    let clauses: Vec<&str> = if text.is_empty() {
        Vec::new()
    } else {
        lowered.split([',', '.', '!', '?', ';']).filter(|c| !py_strip(c).is_empty()).collect()
    };
    let tail = match clauses.last() {
        Some(last) => {
            let ws = py_split(last);
            core(&ws).join(" ")
        }
        None => String::new(),
    };
    let tail = normalize(&tail);
    for candidate in [core_text.as_str(), tail.as_str()] {
        if candidate.is_empty() {
            continue;
        }
        if UNMUTE_RE.is_match(candidate) {
            return Some(UNMUTE);
        }
        if MUTE_RE.is_match(candidate) {
            return Some(MUTE);
        }
        if LEAVE_WHOLE.is_match(candidate) {
            return Some(LEAVE);
        }
    }
    if LEAVE_ANYWHERE.is_match(&t) {
        return Some(LEAVE);
    }
    if is_head_move(&core_text) {
        return Some(LOOK);
    }
    if LESSON_RE.is_match(&core_text) && !NOT_LESSON.is_match(&t) {
        return Some(LESSON);
    }
    if REMEMBER_RE.is_match(&core_text) || REMEMBER_RE.is_match(&tail) {
        return Some(REMEMBER);
    }
    if NOTES_RE.is_match(&t) {
        return Some(NOTES);
    }
    None
}

// ---- the model -------------------------------------------------------------------------------------------------

pub const PROMPT: &str = "You label what a person just said to their personal sidekick, called eve. The label is about \
eve itself, never about the computer or other people.

- leave: they want eve to end the conversation \u{2014} goodbye, see you, go away, leave, stop listening, go to \
sleep, \"that's all\", \"I'm heading out\", \"you can go now\".
- mute: they want eve itself to stop making sound.
- unmute: they want eve's own sound back on.
- look: they want eve to move its head or use its eyes on the room \u{2014} look somewhere, turn, look around, \
look at them or at a thing in the room, find something, \"what's behind you?\", and asking where a physical \
thing is: \"where did I leave my keys?\", \"where's my mug?\".
- lesson: they want eve to teach, tutor, quiz or coach them, or to help them understand, learn or work \
through something \u{2014} \"teach me about fractions\", \"quiz me on French verbs\", \"help me work through this \
proof\", \"I want a lesson\", \"walk me through recursion\". A plain question they want answered \u{2014} \"what is a \
mutex?\", \"what's 17 times 23?\" \u{2014} is none.
- remember: they are telling eve to keep something permanently \u{2014} \"remember that\", \"don't forget that\", \
\"keep that in mind\", \"note that down\". Only when the whole point of what they said is that eve should keep \
it: \"I remember that\" and \"remember when you said that?\" are none.
- notes: they want eve to start taking notes, recording or transcribing what is said in the room \u{2014} \
\"take notes\", \"start taking notes\", \"can you write this down as we go\". Not \"remember that\", which is \
remember, and not a request to read notes back.
- none: anything else. Requests about apps, music, windows, tabs, files or other people are none, even when \
they use the same words: \"leave the tab open\" is none, \"mute Spotify\" is none, \"tell Sam goodbye\" is none, \
\"look up the weather\" is none, \"turn the volume down\" is none.

Return the label and your confidence from 0 to 1.";

/// The strict JSON schema the model answers in.
pub static SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "intent": {"type": "string", "enum": LABELS},
            "confidence": {"type": "number"},
        },
        "required": ["intent", "confidence"],
        "additionalProperties": false,
    })
});

/// What `parse_intent` raises on, as Python does: the reply is not JSON (`JSONDecodeError`) or not an object
/// (`AttributeError` from `.get`).
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("'{0}' object has no attribute 'get'")]
    NotObject(&'static str),
}

impl ParseError {
    /// Python's exception class name.
    pub fn py_name(&self) -> &'static str {
        match self {
            ParseError::Json(_) => "JSONDecodeError",
            ParseError::NotObject(_) => "AttributeError",
        }
    }
}

/// `(label, confidence)` from the model's JSON. An unknown or missing label is none; the confidence is clamped to
/// 0..1 and anything that is not a number is 0.
pub fn parse_intent(raw: &str) -> Result<(&'static str, f64), ParseError> {
    let obj: Value = serde_json::from_str(raw)?;
    let Value::Object(map) = &obj else {
        return Err(ParseError::NotObject(py_type_name(&obj)));
    };
    // str(obj.get("intent") or NONE): only a string can name a label.
    let label = match map.get("intent") {
        Some(Value::String(s)) if !s.is_empty() => LABELS.iter().copied().find(|l| *l == s).unwrap_or(NONE),
        _ => NONE,
    };
    let raw_conf: Option<f64> = match map.get("confidence") {
        None => Some(0.0),
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::Bool(b)) => Some(if *b { 1.0 } else { 0.0 }),
        Some(Value::String(s)) => py_float_str(s),
        Some(_) => None,
    };
    let conf = match raw_conf {
        // max(0.0, min(1.0, x)), with Python's comparison order (a NaN lands on 1.0).
        Some(x) => {
            let m = if x < 1.0 { x } else { 1.0 };
            if m > 0.0 {
                m
            } else {
                0.0
            }
        }
        None => 0.0,
    };
    Ok((label, conf))
}

/// The openai SDK's `Response.output_text`: every `output_text` part of every `message` item, joined.
pub fn output_text(resp: &Value) -> String {
    let mut texts = String::new();
    for item in resp.get("output").and_then(Value::as_array).into_iter().flatten() {
        if item.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        for part in item.get("content").and_then(Value::as_array).into_iter().flatten() {
            if part.get("type").and_then(Value::as_str) == Some("output_text") {
                if let Some(t) = part.get("text").and_then(Value::as_str) {
                    texts.push_str(t);
                }
            }
        }
    }
    texts
}

/// Python's exception class name for an error out of a classify call, for the log and the intent-test report.
pub fn error_name(e: &anyhow::Error) -> &'static str {
    if let Some(p) = e.downcast_ref::<ParseError>() {
        return p.py_name();
    }
    if e.downcast_ref::<serde_json::Error>().is_some() {
        return "JSONDecodeError";
    }
    if let Some(o) = e.downcast_ref::<eve_openrouter::Error>() {
        return match o {
            eve_openrouter::Error::NoKey(_) => "NoKey",
            eve_openrouter::Error::Status { status, .. } => match status {
                400 => "BadRequestError",
                401 => "AuthenticationError",
                403 => "PermissionDeniedError",
                404 => "NotFoundError",
                409 => "ConflictError",
                422 => "UnprocessableEntityError",
                429 => "RateLimitError",
                s if *s >= 500 => "InternalServerError",
                _ => "APIStatusError",
            },
            eve_openrouter::Error::Http(h) if h.is_timeout() => "APITimeoutError",
            eve_openrouter::Error::Http(_) => "APIConnectionError",
            eve_openrouter::Error::Json(_) => "JSONDecodeError",
            eve_openrouter::Error::Invalid(_) => "ValueError",
        };
    }
    "Exception"
}

/// `str(e)` as the SDK would print it: its connection and timeout errors carry fixed messages.
pub fn error_text(e: &anyhow::Error) -> String {
    if let Some(eve_openrouter::Error::Http(h)) = e.downcast_ref::<eve_openrouter::Error>() {
        return if h.is_timeout() { "Request timed out.".into() } else { "Connection error.".into() };
    }
    e.to_string()
}

/// The blocking `classify(text) -> (label, conf)` the async wrapper runs on a worker thread.
pub trait IntentClient: Send + Sync {
    fn classify(&self, text: &str) -> anyhow::Result<(String, f64)>;
}

/// `client.responses.create(**body)`: the seam the tests replace (Python swapped `_client`).
pub trait Responses: Send + Sync {
    fn create(&self, body: Value) -> anyhow::Result<Value>;
}

impl Responses for eve_openrouter::Client {
    /// Blocking: runs the request on the current runtime from a worker thread (where `classify` runs), or on a
    /// private one when there is none (the intent-test command). Never call it on an async worker thread.
    fn create(&self, body: Value) -> anyhow::Result<Value> {
        let fut = self.responses_create(body);
        let out = match tokio::runtime::Handle::try_current() {
            Ok(handle) => handle.block_on(fut),
            Err(_) => tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(fut),
        };
        Ok(out?)
    }
}

/// Responses API, text only, strict JSON, `store=False`.
pub struct OpenAIIntentClient {
    pub model: String,
    client: Box<dyn Responses>,
    ledger: eve_spend::Ledger,
}

impl OpenAIIntentClient {
    /// The real client: OpenRouter, `timeout` per request, no retries (a retry would outlive the turn). Raises NoKey.
    pub fn new(model: &str, api_key: Option<&str>, timeout: Duration) -> anyhow::Result<Self> {
        let opts = eve_openrouter::ClientOptions::default().timeout(timeout).max_retries(0);
        let client = eve_openrouter::async_client(api_key, opts)?;
        Ok(Self::with_client(model, Box::new(client)))
    }

    /// Over any `responses.create` (tests).
    pub fn with_client(model: &str, client: Box<dyn Responses>) -> Self {
        Self { model: model.to_string(), client, ledger: eve_spend::Ledger::default() }
    }

    /// Meter into this ledger instead of the configured one (tests).
    pub fn with_ledger(mut self, ledger: eve_spend::Ledger) -> Self {
        self.ledger = ledger;
        self
    }

    pub fn request(&self, text: &str) -> Value {
        json!({
            "model": self.model,
            "instructions": PROMPT,
            "input": [{"role": "user", "content": [{"type": "input_text", "text": text}]}],
            "text": {"format": {"type": "json_schema", "name": "intent", "strict": true, "schema": &*SCHEMA}},
            "max_output_tokens": 40,
            "store": false,
        })
    }

    pub fn classify(&self, text: &str) -> anyhow::Result<(&'static str, f64)> {
        let resp = self.client.create(self.request(text))?;
        self.ledger.record_response(eve_spend::VOICE, &resp, &self.model, eve_openrouter::PROVIDER, None);
        let out = output_text(&resp);
        Ok(parse_intent(if out.is_empty() { "{}" } else { &out })?)
    }
}

impl IntentClient for OpenAIIntentClient {
    fn classify(&self, text: &str) -> anyhow::Result<(String, f64)> {
        OpenAIIntentClient::classify(self, text).map(|(l, c)| (l.to_string(), c))
    }
}

/// An async `text -> label | None`.
pub type Classifier = Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> + Send + Sync>;

/// Wrap a blocking `classify(text) -> (label, conf)` as an async `text -> label | None`. A failure or a timeout is
/// no intent. Python's defaults are `timeout=TIMEOUT_SECS` ([`timeout()`]) and `min_confidence=MIN_CONFIDENCE`.
pub fn make_classifier(client: Arc<dyn IntentClient>, timeout: Duration, min_confidence: f64) -> Classifier {
    Arc::new(move |text: String| {
        let client = Arc::clone(&client);
        Box::pin(async move {
            if py_split(&text).len() > MAX_WORDS {
                return None;
            }
            let job_text = text.clone();
            let work = tokio::task::spawn_blocking(eve_spend::carry(move || client.classify(&job_text)));
            let result = match tokio::time::timeout(timeout, work).await {
                Ok(Ok(Ok(answer))) => Ok(answer),
                Ok(Ok(Err(e))) => Err((error_name(&e).to_string(), error_text(&e))),
                Ok(Err(join)) => Err(("Exception".to_string(), join.to_string())),
                Err(_) => Err(("TimeoutError".to_string(), String::new())),
            };
            let (label, conf) = match result {
                Ok(answer) => answer,
                Err((name, msg)) => {
                    tracing::info!("intent: classifier skipped ({}: {})", name, msg);
                    return None;
                }
            };
            if label == NONE || conf < min_confidence {
                return None;
            }
            let head: String = text.chars().take(80).collect();
            tracing::info!("intent: {} \u{2192} {} ({:.2})", py_repr(&head), label, conf);
            Some(label)
        })
    })
}

fn env_get(env: Option<&HashMap<String, String>>, name: &str) -> Option<String> {
    match env {
        Some(map) => map.get(name).cloned(),
        None => std::env::var_os(name).map(|v| v.to_string_lossy().into_owned()),
    }
}

fn model_from(env: Option<&HashMap<String, String>>) -> String {
    let raw = env_get(env, "EVE_INTENT_MODEL").unwrap_or_default();
    let m = py_strip(&raw);
    if m.is_empty() {
        DEFAULT_MODEL.to_string()
    } else {
        m.to_string()
    }
}

/// The real classifier, or None when there is no key (the phrase table still works). `None` reads the process
/// environment.
pub fn make_intent_classifier(environ: Option<&HashMap<String, String>>) -> Option<Classifier> {
    let gate = env_get(environ, "EVE_INTENT").filter(|v| !v.is_empty()).unwrap_or_else(|| "1".to_string());
    if matches!(py_lower(py_strip(&gate)).as_str(), "0" | "false" | "no" | "off") {
        return None;
    }
    let key = eve_openrouter::key(environ);
    if key.is_empty() {
        return None;
    }
    let model = model_from(environ);
    match OpenAIIntentClient::new(&model, Some(&key), timeout()) {
        Ok(client) => Some(make_classifier(Arc::new(client), timeout(), MIN_CONFIDENCE)),
        Err(e) => {
            tracing::warn!("intent: openai SDK not importable ({}) \u{2014} phrase table only", e);
            None
        }
    }
}

/// `eve-bridge intent-test --expect leave "..." ...`: the real classifier on each phrase (the phrase table is
/// bypassed, so this measures the model). Prints `intent-test ok`. Pairs are `(want, text)`; `stay` means none.
pub fn run_intent_test(pairs: &[(String, String)]) -> i32 {
    run_intent_test_with(pairs, None, None, &mut std::io::stdout(), &mut std::io::stderr())
}

/// [`run_intent_test`] over an explicit environment, client and output streams (tests). `client` None builds the
/// real one from the environment.
pub fn run_intent_test_with(
    pairs: &[(String, String)],
    env: Option<&HashMap<String, String>>,
    client: Option<&dyn IntentClient>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let key = eve_openrouter::key(env);
    if key.is_empty() {
        let _ = writeln!(err, "intent-test: OPENROUTER_API_KEY not set");
        return 2;
    }
    let model = model_from(env);
    let built;
    let client: &dyn IntentClient = match client {
        Some(c) => c,
        None => match OpenAIIntentClient::new(&model, Some(&key), timeout()) {
            Ok(c) => {
                built = c;
                &built
            }
            Err(e) => {
                let _ = writeln!(err, "intent-test: {e}");
                return 2;
            }
        },
    };
    let mut bad = 0usize;
    for (want, text) in pairs {
        let want_label = if want == "stay" { NONE } else { want.as_str() };
        let (label, conf) = match client.classify(text) {
            Ok(answer) => answer,
            Err(e) => {
                let _ = writeln!(out, "  ERROR {}: {}: {}", py_repr(text), error_name(&e), error_text(&e));
                bad += 1;
                continue;
            }
        };
        let got = if conf >= MIN_CONFIDENCE { label.as_str() } else { NONE };
        let ok = got == want_label;
        if !ok {
            bad += 1;
        }
        let _ = writeln!(
            out,
            "  {} {}: {} ({:.2}), wanted {}",
            if ok { "ok " } else { "BAD" },
            py_repr(text),
            label,
            conf,
            want_label
        );
    }
    if bad > 0 {
        let _ = writeln!(err, "intent-test: {bad} wrong ({model})");
        return 1;
    }
    let _ = writeln!(out, "intent-test ok ({model}, {} phrase(s))", pairs.len());
    0
}

#[cfg(test)]
mod tests;
