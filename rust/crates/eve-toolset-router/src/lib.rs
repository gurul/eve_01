//! Which of the owner's toolsets a Telegram turn starts with: Jev's judgment of the message, plus keywords (port
//! of `toolset_router.py`).
//!
//! Why (2026-10-05): a turn starts with only the toolsets its words name (apps_mcp::APP_WORDS) and an `open_app`
//! tool for the rest. A toolset the turn needs but did not start with costs a whole model round to open it, and
//! worse, it changes the tool list mid-turn, which busts the prompt cache: live 2026-10-05, a 13k-token turn whose
//! tool list changed read cached=0 on the next round. Keywords miss most of what the owner actually types ("whens
//! my thing w the advisor", "did anyone @ me"); Jev reads the job and answers in 148-266 ms (measured live
//! 2026-10-05: "my next class and a maps link" -> calendar 0.93, maps 0.90).
//!
//! The seven toolsets (the datasets' policy field is the definition):
//!
//! * calendar, maps, gmail, drive, github: the direct apps (apps_mcp), opened per turn.
//! * share: sending a file from the Mac to another person by iMessage or email (share).
//! * workspace: the owner's Slack, Notion and Linear (composio_tools).
//!
//! Jev is asked the way search_router asks it (docs.typesafe.ai: State, Noul; typed_ask's idiom):
//!
//! * The state is the message, the owner's previous message (a follow-up's context: "and send it to ana" after
//!   "find my resume in drive" needs drive AND share), and one line saying what the text is. No tool manuals in
//!   the state.
//! * One absolute Noul per toolset, each a plain judgment of the job ("must the assistant read or change the
//!   owner's own calendar ..."), written in the owner's own words for things and naming no wrong thing (the
//!   owner's rule: never write a wrong *name* into a prompt; a name in a prompt is an option the model may fill a
//!   slot with).
//! * Code owns the rule: a toolset opens when its Noul clears its fitted gate (GATES), OR its keywords match
//!   (keyword_toolsets). An error, a timeout or a missing key is the empty set from Jev: keywords and open_app
//!   still apply, exactly as before the router.
//!
//! Fitting (tools/toolset_eval.py, the house recipe of tools/route_eval.py --search): live Jev runs once over the
//! tuning set (toolset_tuning.json, 220 cases, writer A; answers cached in toolset_tuning_answers.json), and per
//! toolset the HIGHEST gate on GRID is kept such that keywords + Jev reach recall >= 0.98 on the tuning cases that
//! need it. Then live Jev runs ONCE over the blind holdout (toolset_holdout.json, 120 cases, writer B) and is
//! scored against the bar below, written before any holdout result existed. Nothing is refitted after the
//! holdout.
//!
//! PRE-REGISTERED BAR (written by the orchestrator 2026-10-05, before any holdout result existed), verbatim:
//!   (1) case-level full recall (every needed toolset opened) of keywords∪Jev >= 0.95;
//!   (2) strictly higher than keywords alone on the same metric;
//!   (3) mean unneeded toolsets opened per case <= 0.6;
//!   (4) Jev latency p95 <= 800 ms.
//! SHIPPED is True only if all four hold.
//!
//! `EVE_TOOLSET_ROUTER` = auto (the default: Jev on when SHIPPED) | on (Jev on before or despite the bar, for a
//! live trial) | off (keywords only). Jev is a third party: the owner's message and the previous one leave the
//! Mac on every Telegram turn while it is on.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use fancy_regex::Regex as FancyRegex;
use indexmap::IndexMap;
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

pub use eve_jev::{Env, ProcessEnv};

/// `Predict = Callable[[Any, dict[str, Any]], dict[str, Any]]`: `predict(state, questions)`. An `Err` is the
/// exception Python's predict raises.
pub type Predict<'a> = &'a dyn Fn(&Value, &Value) -> anyhow::Result<Value>;

/// `Callable[[str, str], Route]`: `route(text, previous)`. An `Err` is the exception the router raises.
pub type Router = Arc<dyn Fn(&str, &str) -> anyhow::Result<Route> + Send + Sync>;

pub const CALENDAR: &str = "calendar";
pub const MAPS: &str = "maps";
pub const GMAIL: &str = "gmail";
pub const DRIVE: &str = "drive";
pub const GITHUB: &str = "github";
pub const SHARE: &str = "share";
pub const WORKSPACE: &str = "workspace";
// 2026-10-05: an eighth toolset, ordering delivery, was removed with its integration (docs/toolset-router.md). The
// datasets and cached answers keep its labels and Nouls as history; the eval and the fit read only these.
pub const TOOLSETS: [&str; 7] = [CALENDAR, MAPS, GMAIL, DRIVE, GITHUB, SHARE, WORKSPACE];
// Which toolsets are apps_mcp's apps is AppsBridge.apps at run time (the list grew to contacts on 2026-10-09); the
// other two, share and workspace, are telegram's own tools.
// Opened by their words alone, never asked of Jev (2026-10-09): Google Contacts is one small read tool, so a false
// open costs a few hundred tokens, and a Noul of its own would change the call the gates above were fitted on.
pub const CONTACTS: &str = "contacts";
pub const KEYWORD_ONLY: [&str; 1] = [CONTACTS];

// The toolsets every turn carries, outside the routed seven (2026-10-08, owner: "expose all the menu items natively
// as well"). They are never routed: each is a few small tools whose schemas are fixed, so carrying them on every
// turn keeps the cached prefix stable, while a routed toolset would need its own Noul in Jev's call (which changes
// the call the gates above were fitted on) and a refit. Each is lent only while this Mac has it
// (telegram._menu_tools):
//
// * menu: the / menu's commands that had no tool (menu_tools), each reaching its command's own handler;
// * events: concerts, games and shows by place and date on Ticketmaster (events), while TICKETMASTER_API_KEY is
//   set. Event questions reach it on every turn, so web_search is left for the rest of the web.
pub const MENU: &str = "menu";
pub const EVENTS: &str = "events";
/// `ALWAYS`, in Python's dict order.
pub const ALWAYS: [(&str, &[&str]); 2] = [
    (
        MENU,
        &[
            "spend_report",
            "daily_rundown",
            "claude_relay",
            "codex_relay",
            "set_stealth",
            "open_apps",
            "pair_eve_link",
            "read_file",
        ],
    ),
    (EVENTS, &["events_search"]),
];

/// The always-carried toolset a tool belongs to, or "".
pub fn always_toolset(tool: &str) -> &'static str {
    ALWAYS.iter().find(|(_, tools)| tools.contains(&tool)).map(|(name, _)| *name).unwrap_or("")
}

pub const MODES: [&str; 3] = ["auto", "on", "off"];
// Jev's whole budget at a turn's start. Live 2026-10-05: 148-266 ms; past this the turn starts on keywords alone,
// which is today's behaviour, so a slow Jev costs at most this much and never a wrong tool list.
pub const TIMEOUT_S: f64 = 0.8;
/// `tuple(round(0.05 * i, 2) for i in range(1, 20))`: 0.05 .. 0.95.
pub const GRID: [f64; 19] = [
    0.05, 0.1, 0.15, 0.2, 0.25, 0.3, 0.35, 0.4, 0.45, 0.5, 0.55, 0.6, 0.65, 0.7, 0.75, 0.8, 0.85, 0.9, 0.95,
];
pub const MIN_TUNING_RECALL: f64 = 0.98;
/// `GATES_FILE`: bridge/tests/fixtures/routes/toolset_gates.json in the checkout, where tools/toolset_eval.py
/// --fit writes it. A binary away from the checkout has no file and [`load_gates`] falls back to [`GATES`], as an
/// installed Python eve does (tests/ is not in the wheel).
pub static GATES_FILE: LazyLock<PathBuf> = LazyLock::new(|| {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../bridge/tests/fixtures/routes/toolset_gates.json")
});

// ---- the question ---------------------------------------------------------------------------------------------

pub const ABOUT: &str = "A message the owner texted their personal assistant. When a previous message is given, the new one may \
be a follow-up to it: judge the job the new message asks for, reading the previous one only as context.";

// One Noul per toolset: does doing what the owner asks need this? Each is the job, in the words the owner uses.
/// `INSTRUCTIONS`, in Python's dict order (the TOOLSETS order).
pub const INSTRUCTIONS: [(&str, &str); 7] = [
    (
        CALENDAR,
        "To do what the owner asks, must the assistant read or change the owner's own calendar: their events, \
classes, lectures, exams, meetings, appointments, free or busy time, invites, what is next or what their \
day or week holds? Yes too when the request points at one of their events by its time or name (\"my 3pm\", \
\"my next class\") and needs when or where it is, or when the request asks to put something on their \
calendar. No for general questions about dates or how calendars work.",
    ),
    (
        MAPS,
        "To do what the owner asks, must the assistant look up places or getting around: a place's address or map \
link, directions, travel time, the route, traffic, places nearby to go to, or the weather or forecast \
somewhere? No for a business's opening hours, news, or general facts.",
    ),
    (
        GMAIL,
        "To do what the owner asks, must the assistant read the owner's own email: find, read, count or summarize \
messages in their inbox, what someone emailed or sent them, or a receipt, code, link or confirmation that \
came by email? No when the request only sends something, and no for whether an email service is working.",
    ),
    (
        DRIVE,
        "To do what the owner asks, must the assistant find, open, read or create a document, spreadsheet, slide \
deck or PDF stored in the owner's cloud drive, or save something there? No for a file only on the \
computer's desktop or downloads folder.",
    ),
    (
        GITHUB,
        "To do what the owner asks, must the assistant read or change the owner's code projects on GitHub: \
repositories, issues, pull requests, commits, branches, or build and CI status? No for general \
programming questions or advice.",
    ),
    (
        SHARE,
        "To do what the owner asks, must the assistant send a file (a document, deck, PDF, photo, screenshot or \
other file) to another person by text message or email? No when the file goes to the owner themself, \
and no for a plain message with no file.",
    ),
    (
        WORKSPACE,
        "To do what the owner asks, must the assistant read or write in the owner's Slack, Notion or Linear: \
messages, channels, mentions, threads, pages, tickets or sprints? No for opening one of those apps on the \
computer screen, or for comparing such products.",
    ),
];

/// One absolute Noul per toolset, the same on every Mac whichever toolsets are up (so the fitted gates hold).
pub fn questions() -> Value {
    let mut out = Map::new();
    for (name, instructions) in INSTRUCTIONS {
        out.insert(name.to_string(), json!({"type": "noul", "instructions": instructions}));
    }
    Value::Object(out)
}

pub fn state(text: &str, previous: &str) -> Value {
    json!({
        "about": ABOUT,
        "previous": clip(&collapse_ws(previous), 500),
        "request": clip(&collapse_ws(text), 500),
    })
}

// ---- small Python helpers ----------------------------------------------------------------------------------

/// An exception Python would raise out of a function (an `AttributeError` from `ask` on a malformed reply, a
/// `ValueError` from `fit`'s strict zip). `kind` is `type(e).__name__`, `Display` is `str(e)`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct Raised {
    pub kind: String,
    pub message: String,
}

impl Raised {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self { kind: kind.into(), message: message.into() }
    }
}

/// `type(e).__name__` for an error a predict or a router returned: the Python class it stands for when it says
/// so ([`eve_jev::PredictFailure`], [`eve_jev::JevError`], [`eve_decider::PredictError`], [`Raised`]), else
/// `Exception`.
pub fn exception_name(e: &anyhow::Error) -> String {
    if let Some(f) = e.downcast_ref::<eve_jev::PredictFailure>() {
        return f.kind().to_string();
    }
    if e.downcast_ref::<eve_jev::JevError>().is_some() {
        return "JevError".to_string();
    }
    if let Some(p) = e.downcast_ref::<eve_decider::PredictError>() {
        return p.kind.clone();
    }
    if let Some(r) = e.downcast_ref::<Raised>() {
        return r.kind.clone();
    }
    "Exception".to_string()
}

/// What Python's `str.split()` / `str.strip()` treat as whitespace: Unicode whitespace and the ASCII separators
/// \x1c-\x1f.
fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `" ".join(s.split())`.
fn collapse_ws(s: &str) -> String {
    s.split(is_py_space).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// `s[:n]` in characters.
fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Python's `round(x, digits)` on a float: the nearest decimal, ties to even on the exact binary value (Rust's
/// formatter rounds the same way).
fn py_round(x: f64, digits: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.digits$}").parse().unwrap_or(x)
}

/// `f"{x:.0f}"`.
fn py_f0(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    format!("{x:.0}")
}

/// `bool(v)` for a decoded JSON value.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `type(v).__name__` for a decoded JSON value.
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

fn no_get(v: &Value) -> Raised {
    Raised::new("AttributeError", format!("'{}' object has no attribute 'get'", py_type_name(v)))
}

/// `float(v)` for a decoded JSON value.
fn py_float(v: &Value) -> Result<f64, Raised> {
    match v {
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64().ok_or_else(|| Raised::new("OverflowError", "int too large to convert to float")),
        Value::String(s) => {
            // float() strips Unicode whitespace; `_` is allowed only between two digits ("1_000").
            let t = s.trim();
            let b = t.as_bytes();
            let underscores_ok = b.iter().enumerate().all(|(i, c)| {
                *c != b'_' || (i > 0 && i + 1 < b.len() && b[i - 1].is_ascii_digit() && b[i + 1].is_ascii_digit())
            });
            match t.replace('_', "").parse::<f64>() {
                Ok(f) if underscores_ok && !t.is_empty() => Ok(f),
                _ => Err(Raised::new(
                    "ValueError",
                    format!("could not convert string to float: {}", eve_decider::py_str_repr(s)),
                )),
            }
        }
        other => Err(Raised::new(
            "TypeError",
            format!("float() argument must be a string or a real number, not '{}'", py_type_name(other)),
        )),
    }
}

/// `str(v)` for a decoded JSON value.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => eve_decider::py_repr(other),
    }
}

/// `time.perf_counter()`: seconds on a monotonic clock.
pub fn perf_counter() -> f64 {
    eve_decider::perf_counter()
}

// ---- Jev's answer ----------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ToolsetAnswer {
    /// toolset -> Jev's Noul, 0.0 when absent
    pub nouls: IndexMap<String, f64>,
    pub ms: f64,
    pub error: String,
}

impl ToolsetAnswer {
    /// `ToolsetAnswer(nouls)`.
    pub fn new<K: Into<String>>(nouls: impl IntoIterator<Item = (K, f64)>) -> Self {
        Self { nouls: nouls.into_iter().map(|(k, v)| (k.into(), v)).collect(), ms: 0.0, error: String::new() }
    }

    /// `ToolsetAnswer(nouls, ms=ms)`.
    pub fn with_ms(mut self, ms: f64) -> Self {
        self.ms = ms;
        self
    }

    /// `ToolsetAnswer(nouls, error=error)`.
    pub fn with_error(mut self, error: impl Into<String>) -> Self {
        self.error = error.into();
        self
    }

    pub fn to_json(&self) -> Value {
        let nouls: Map<String, Value> = self.nouls.iter().map(|(k, v)| (k.clone(), json!(py_round(*v, 4)))).collect();
        json!({"nouls": nouls, "ms": py_round(self.ms, 1), "error": self.error})
    }

    /// `ToolsetAnswer.from_json(raw)`: each Noul through `float()`, `float(raw.get("ms") or 0.0)`,
    /// `str(raw.get("error") or "")`. A value Python refuses is the exception it raises.
    pub fn from_json(raw: &Value) -> Result<ToolsetAnswer, Raised> {
        let Value::Object(raw) = raw else {
            return Err(no_get(raw));
        };
        let field = |k: &str| raw.get(k).filter(|v| truthy(v));
        let mut nouls = IndexMap::new();
        match field("nouls") {
            None => {}
            Some(Value::Object(m)) => {
                for (k, v) in m {
                    nouls.insert(k.clone(), py_float(v)?);
                }
            }
            Some(other) => {
                return Err(Raised::new(
                    "AttributeError",
                    format!("'{}' object has no attribute 'items'", py_type_name(other)),
                ))
            }
        }
        let ms = field("ms").map(py_float).transpose()?.unwrap_or(0.0);
        let error = field("error").map(py_str).unwrap_or_default();
        Ok(ToolsetAnswer { nouls, ms, error })
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Route {
    pub toolsets: BTreeSet<String>,
    /// Jev's time
    pub ms: f64,
    pub error: String,
}

impl Route {
    pub fn new<S: Into<String>>(toolsets: impl IntoIterator<Item = S>, ms: f64, error: impl Into<String>) -> Self {
        Self { toolsets: toolsets.into_iter().map(Into::into).collect(), ms, error: error.into() }
    }
}

/// Jev's Nouls for one message. A failure is an answer with `error` set and no Nouls: it opens nothing.
///
/// `Err` is what Python lets escape `ask`: a reply whose `answers` is a truthy non-object (`AttributeError:
/// '<type>' object has no attribute 'get'`, raised outside the `try`).
pub fn ask(predict: Predict<'_>, text: &str, previous: &str, clock: &dyn Fn() -> f64) -> Result<ToolsetAnswer, Raised> {
    let t0 = clock();
    // A model that fails abstains: keywords and open_app still apply.
    let failed = |e: String| ToolsetAnswer {
        nouls: IndexMap::new(),
        ms: (clock() - t0) * 1000.0,
        error: clip(&e, 160),
    };
    let answers = match predict(&state(text, previous), &questions()) {
        Ok(Value::Object(body)) => body.get("answers").filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!({})),
        Ok(other) => {
            let e = no_get(&other);
            return Ok(failed(format!("{}: {}", e.kind, e.message)));
        }
        Err(e) => return Ok(failed(format!("{}: {}", exception_name(&e), e))),
    };
    let Value::Object(answers) = answers else {
        return Err(no_get(&answers));
    };
    let noul = |key: &str| -> f64 {
        match answers.get(key) {
            Some(Value::Object(a)) => match a.get("noul") {
                Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
                _ => 0.0,
            },
            _ => 0.0,
        }
    };
    let nouls = TOOLSETS.iter().map(|name| (name.to_string(), noul(name))).collect();
    Ok(ToolsetAnswer { nouls, ms: (clock() - t0) * 1000.0, error: String::new() })
}

/// The toolsets whose Noul clears its gate. An error, or an answer slower than `budget_ms` (the turn would not
/// have waited for it), opens nothing. Python's `budget_ms` defaults to infinity: pass `f64::INFINITY`.
pub fn decide(a: &ToolsetAnswer, gates: &IndexMap<String, f64>, budget_ms: f64) -> BTreeSet<String> {
    if !a.error.is_empty() || a.ms > budget_ms {
        return BTreeSet::new();
    }
    TOOLSETS
        .iter()
        .filter(|name| a.nouls.get(**name).copied().unwrap_or(0.0) >= gates.get(**name).copied().unwrap_or(1.01))
        .map(|name| name.to_string())
        .collect()
}

// ---- keywords ---------------------------------------------------------------------------------------------------

// share and workspace have no apps_mcp::APP_WORDS; these are theirs, written from the tuning set's positives and
// its hard negatives ("send me the pdf", "email it to me", "open slack on my mac", "notion vs obsidian?"). They are
// a net under Jev, not the router: a miss here is Jev's to catch.
const SHARE_VERB: &str = r"(?:send|text|i-?message|e-?mail|shoot|forward)";
const SHARE_THING: &str = r"(?:deck|pdf|file|files|docs?|slides|resume|cover letter|screenshot|pic|pics|photos?|zip|sheet|report|itinerary|lease|contract|attachment|it|that|this|them)";

pub static SHARE_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)\b{SHARE_VERB}\b(?:\s+\S+){{0,4}}?\s+(?:the |my |a |that |this )?{SHARE_THING}\b"
    ))
    .expect("SHARE_WORDS")
});
pub static TO_SELF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"(?i)\b{SHARE_VERB} (?:it |that |this |them )?(?:to )?me\b|\bto me\b|\bme (?:here|too)\b"))
        .expect("TO_SELF")
});
pub static WORKSPACE_WORDS: LazyLock<FancyRegex> = LazyLock::new(|| {
    FancyRegex::new(concat!(
        r"(?i)(?:\b(?<!open )(?<!launch )(?<!quit )(?<!close )slack(?:ed|ing)?\b(?! (?:app|on my mac))|",
        r"\bnotion\b(?! vs\b| or\b)|\blinear\b(?! (?:algebra|regression|time|function))|\btickets?\b|\bchannels?\b|",
        r"#[a-z][\w-]*|@\s?me\b|\bdms?\b|\bthreads?\b|\bsprint\b|\bstandup thread\b|",
        // the owner's work Gmail and calendar live in Composio (composio_tools.OTHER_ACCOUNT_TOOLKITS, 2026-10-05)
        r"\bwork (?:e-?mails?|mail|inbox|gmail|calendar|cal)\b|\bera (?:e-?mail|mail|inbox|calendar)\b)",
    ))
    .expect("WORKSPACE_WORDS")
});

/// `OWN_WORDS`: the routed toolsets with words of their own here.
pub fn own_words() -> [(&'static str, &'static FancyRegex); 1] {
    [(WORKSPACE, &WORKSPACE_WORDS)]
}

// A short follow-up that points back ("and send it to ana", "put that on my cal"): its keywords alone cannot say
// what "it" is. Chosen on the tuning set only (tools/toolset_eval.py --fit prints both): the previous message's
// keywords are added only for such a follow-up, so "lights blue" then "dimmer" stays empty.
pub static FOLLOW_UP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:and|also|ok|okay|now|then|wait|actually|so|cool|fine)\b|\b(?:it|that|those|them|there|the first one|same|instead|too)\b",
    )
    .expect("FOLLOW_UP")
});

/// Python's `re.IGNORECASE` also lets the Turkish dotted and dotless I (U+0130, U+0131) match an ASCII `i`,
/// which the regex crates' simple case folding does not; read them as `i` before matching (as apps_mcp does).
fn python_ignorecase(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains(['\u{130}', '\u{131}']) {
        std::borrow::Cow::Owned(text.replace(['\u{130}', '\u{131}'], "i"))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

/// `_words(text)`: the toolsets (and keyword-only sets) the text's own words name.
fn words(text: &str) -> BTreeSet<String> {
    let mut found: BTreeSet<String> = eve_apps_mcp::APP_WORDS
        .keys()
        .filter(|app| eve_apps_mcp::names_app(app, text))
        .map(|app| app.to_string())
        .collect();
    let folded = python_ignorecase(text);
    for (name, rx) in own_words() {
        if rx.is_match(folded.as_ref()).unwrap_or(false) {
            found.insert(name.to_string());
        }
    }
    if SHARE_WORDS.is_match(&folded) && !TO_SELF.is_match(&folded) {
        found.insert(SHARE.to_string());
    }
    found.retain(|n| TOOLSETS.contains(&n.as_str()) || KEYWORD_ONLY.contains(&n.as_str()));
    found
}

/// The toolsets the message's words name; for a short follow-up that points back, the previous message's too.
pub fn keyword_toolsets(text: &str, previous: &str) -> BTreeSet<String> {
    let text = collapse_ws(text);
    let mut found = words(&text);
    if !previous.is_empty()
        && text.split(is_py_space).filter(|w| !w.is_empty()).count() <= 12
        && FOLLOW_UP.is_match(&python_ignorecase(&text))
    {
        found.extend(words(&collapse_ws(previous)));
    }
    found
}

// ---- the fit ----------------------------------------------------------------------------------------------------

/// The share of the cases that need `toolset` whose `said` opened it; 1.0 when none needs it. A `needed` longer
/// than `said` is the `IndexError` Python raises when one of the extra cases needs the toolset.
pub fn recall(said: &[BTreeSet<String>], needed: &[BTreeSet<String>], toolset: &str) -> Result<f64, Raised> {
    let want: Vec<usize> = needed.iter().enumerate().filter(|(_, n)| n.contains(toolset)).map(|(i, _)| i).collect();
    if want.is_empty() {
        return Ok(1.0);
    }
    let mut hit = 0usize;
    for i in &want {
        let s = said.get(*i).ok_or_else(|| Raised::new("IndexError", "list index out of range"))?;
        if s.contains(toolset) {
            hit += 1;
        }
    }
    Ok(hit as f64 / want.len() as f64)
}

/// Per toolset, the HIGHEST gate on GRID at which keywords + Jev open it for >= MIN_TUNING_RECALL of the cases
/// that need it (the highest gate is the fewest unneeded opens). The toolsets are independent: one's gate never
/// changes another's opens. No gate reaching the recall: the lowest on GRID, and the eval says so.
///
/// Python's `budget_ms` defaults to `TIMEOUT_S * 1000.0`. `answers` and `keywords` of different lengths are the
/// `ValueError` of Python's `zip(strict=True)`.
pub fn fit(
    answers: &[ToolsetAnswer],
    keywords: &[BTreeSet<String>],
    needed: &[BTreeSet<String>],
    budget_ms: f64,
) -> Result<IndexMap<String, f64>, Raised> {
    if keywords.len() < answers.len() {
        return Err(Raised::new("ValueError", "zip() argument 2 is shorter than argument 1"));
    }
    if keywords.len() > answers.len() {
        return Err(Raised::new("ValueError", "zip() argument 2 is longer than argument 1"));
    }
    let mut gates = IndexMap::new();
    for name in TOOLSETS {
        let mut chosen = GRID[0];
        for g in GRID.iter().rev() {
            let gate: IndexMap<String, f64> = IndexMap::from([(name.to_string(), *g)]);
            let said: Vec<BTreeSet<String>> = answers
                .iter()
                .zip(keywords)
                .map(|(a, kw)| {
                    let mut s = kw.clone();
                    if decide(a, &gate, budget_ms).contains(name) {
                        s.insert(name.to_string());
                    }
                    s
                })
                .collect();
            if recall(&said, needed, name)? >= MIN_TUNING_RECALL {
                chosen = *g;
                break;
            }
        }
        gates.insert(name.to_string(), chosen);
    }
    Ok(gates)
}

// Fitted 2026-10-05 (tools/toolset_eval.py --fit, live Jev once over toolset_tuning.json, 220 cases; answers cached
// in toolset_tuning_answers.json). Mirrored in toolset_gates.json, which the eval writes and a test checks. On the
// tuning set (seen data, not evidence): keywords alone fully covered 163 of 220 cases, keywords + Jev 219 (the one
// miss: "when is cse 446 this week", calendar 0.33), mean unneeded toolsets 0.17 -> 0.32; Jev p50 177 ms, p95
// 312 ms. gmail's gate is the lowest because its Noul is shy on mail named without the word ("summarize the thread
// w ana about the trip" 0.22, "what did the ta say about my regrade" 0.25); share's is the top of GRID because its
// keywords already catch every tuning case, so Jev adds share only when it is near certain.
/// `GATES`, in Python's dict order.
pub const GATES: [(&str, f64); 7] =
    [(CALENDAR, 0.55), (MAPS, 0.55), (GMAIL, 0.2), (DRIVE, 0.6), (GITHUB, 0.8), (SHARE, 0.95), (WORKSPACE, 0.8)];

/// `GATES` as the mapping the router reads.
pub fn gates() -> IndexMap<String, f64> {
    GATES.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

// PRE-REGISTERED BAR (above, verbatim in the crate docs and docs/toolset-router.md): scored once on the blind
// toolset_holdout.json by tools/toolset_eval.py --holdout. SHIPPED is True only when all four hold.
//
// Scored 2026-10-05 (live Jev once, 120 cases, answers kept in toolset_holdout_answers.json; nothing tuned after):
// (1) full recall 118/120 = 98.3% (>= 95%) pass; (2) keywords alone 92/120 = 76.7%, strictly lower, pass; (3) mean
// unneeded toolsets 0.333 (<= 0.6) pass; (4) Jev p95 286 ms, p50 172 ms, 0 errors, 0 over budget (<= 800) pass.
// Missed toolsets 30 -> 2 (each an open_app round). The two misses, verbatim:
//   "how long a drive to the restaurant for dinner w sam tonight": needed calendar+maps, calendar 0.51 < 0.55
//   "send my mom the boarding pass on my desktop and check traffic to sea-tac": needed maps+share, share 0.94 < 0.95
//   (the share keywords have no "boarding pass") and gmail 0.35 opened unneeded.
//
// Re-scored 2026-10-05 after ordering delivery was removed (no Jev calls: the cached answers, its labels and Nouls
// dropped when read; the other gates unchanged): tuning 219/220 with keywords + Jev, mean unneeded 0.318; holdout
// (1) 118/120 = 98.3% pass; (2) keywords alone 98/120 = 81.7%, strictly lower, pass; (3) mean unneeded 0.317 pass;
// (4) p95 286 ms pass (the cached latencies). Jev was asked one more Noul in the same call when those answers were
// cached, so a fresh blind set should confirm on the next refit.
pub const SHIPPED: bool = true;

/// toolset_gates.json's gates, or GATES when the file is missing or unreadable.
pub fn load_gates(path: &Path) -> IndexMap<String, f64> {
    let read = || -> Option<IndexMap<String, f64>> {
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
        let raw = raw.as_object()?.get("gates")?.as_object()?;
        TOOLSETS.iter().map(|name| Some((name.to_string(), py_float(raw.get(*name)?).ok()?))).collect()
    };
    read().unwrap_or_else(gates)
}

// ---- the router -------------------------------------------------------------------------------------------------

/// `route(text, previous) -> Route`: Jev's Nouls under the gates (GATES when none, or an empty mapping, is
/// given). `clock` is Python's `time.perf_counter` default: pass [`perf_counter`].
pub fn router_from<P, C>(predict: P, clock: C, gates: Option<IndexMap<String, f64>>) -> Router
where
    P: Fn(&Value, &Value) -> anyhow::Result<Value> + Send + Sync + 'static,
    C: Fn() -> f64 + Send + Sync + 'static,
{
    let gates = gates.filter(|g| !g.is_empty()).unwrap_or_else(self::gates);
    Arc::new(move |text: &str, previous: &str| {
        let a = ask(&predict, text, previous, &clock)?;
        Ok(Route { toolsets: decide(&a, &gates, f64::INFINITY), ms: a.ms, error: a.error })
    })
}

/// EVE_TOOLSET_ROUTER = auto (the default: on when SHIPPED) | on (a live trial before or despite the bar) |
/// off (keywords only, as before the router). Pass [`ProcessEnv`] for `os.environ`.
pub fn mode<E: Env + ?Sized>(env: &E) -> &'static str {
    let raw = env.var("EVE_TOOLSET_ROUTER").filter(|v| !v.is_empty()).unwrap_or_else(|| "auto".to_string());
    let mut raw = raw.trim_matches(is_py_space).to_lowercase();
    if ["1", "true", "yes"].contains(&raw.as_str()) {
        raw = "on".into();
    }
    if ["0", "false", "no"].contains(&raw.as_str()) {
        raw = "off".into();
    }
    if let Some(m) = MODES.iter().find(|m| **m == raw) {
        return m;
    }
    let modes: Vec<String> = MODES.iter().map(|m| eve_decider::py_str_repr(m)).collect();
    tracing::warn!(
        "toolset router: EVE_TOOLSET_ROUTER={} is not one of ({}); auto",
        eve_decider::py_str_repr(&raw),
        modes.join(", ")
    );
    "auto"
}

/// `_JEV`: the one Jev router a process makes, keyed on its route. The key is part of the route (a rotated key
/// makes a new router), kept only as a fingerprint; `Debug` never shows a key.
#[derive(Default)]
pub struct JevCache(Mutex<Option<(JevRoute, Router)>>);

/// `(url, model, sha256(key)[:16])`: what a cached router was made for.
type JevRoute = (String, String, String);

impl JevCache {
    pub const fn new() -> Self {
        Self(Mutex::new(None))
    }

    /// `_JEV.clear()`.
    pub fn clear(&self) {
        *self.0.lock() = None;
    }
}

impl std::fmt::Debug for JevCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("JevCache").field(&self.0.lock().as_ref().map(|(route, _)| route.clone())).finish()
    }
}

pub static JEV: JevCache = JevCache::new();

/// Jev over EVE_JEV_ROUTE (TIMEOUT_S), made once a process, or None: the mode is off (or auto while not SHIPPED),
/// or no Jev route is configured. None means keywords only. Pass [`ProcessEnv`] for `os.environ`.
pub fn make_router<E: Env + ?Sized>(env: &E) -> Option<Router> {
    make_router_in(&JEV, env, SHIPPED)
}

/// [`make_router`] with the cache and SHIPPED as seams (the Python tests clear `_JEV` and patch `SHIPPED`).
pub fn make_router_in<E: Env + ?Sized>(cache: &JevCache, env: &E, shipped: bool) -> Option<Router> {
    let m = mode(env);
    if m == "off" || (m == "auto" && !shipped) {
        return None;
    }
    let (url, key, model) = eve_jev::route_config(env, "").ok()?;
    let fingerprint = hex::encode(Sha256::digest(key.as_bytes()))[..16].to_string();
    let route = (url, model, fingerprint);
    let mut held = cache.0.lock();
    if let Some((r, router)) = held.as_ref() {
        if *r == route {
            return Some(router.clone());
        }
    }
    let predict = eve_jev::make_predict(
        &route.0,
        &key,
        &route.1,
        eve_jev::PredictOptions { timeout_s: TIMEOUT_S, ..Default::default() },
    );
    let router = router_from(predict.anyhow_fn(), perf_counter, None);
    *held = Some((route, router.clone()));
    Some(router)
}

/// One turn's toolsets and where each came from, for the turn's single log line (telegram._think).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TurnToolsets {
    pub keywords: BTreeSet<String>,
    pub jev: BTreeSet<String>,
    /// Jev's time; 0 when it was not asked
    pub ms: f64,
    /// a router was there to ask
    pub asked: bool,
    /// Jev's error, or "timeout" when it did not answer within the budget
    pub error: String,
}

impl TurnToolsets {
    pub fn toolsets(&self) -> BTreeSet<String> {
        self.keywords.union(&self.jev).cloned().collect()
    }

    pub fn how(&self, name: &str) -> &'static str {
        if self.keywords.contains(name) && self.jev.contains(name) {
            "both"
        } else if self.jev.contains(name) {
            "jev"
        } else {
            "keywords"
        }
    }

    /// Names and milliseconds only, never the message: "calendar (jev), maps (both); jev 212 ms".
    pub fn log_line(&self) -> String {
        let opened: Vec<String> = self.toolsets().iter().map(|n| format!("{n} ({})", self.how(n))).collect();
        let opened = if opened.is_empty() { "none".to_string() } else { opened.join(", ") };
        if !self.asked {
            return format!("{opened}; jev off");
        }
        let mut line = format!("{opened}; jev {} ms", py_f0(self.ms));
        if !self.error.is_empty() {
            line.push_str(&format!(" ({})", self.error));
        }
        line
    }
}

/// toolsets_for with where each toolset came from, and without its log line (the caller logs one per turn).
/// Blocking (call it with `tokio::task::spawn_blocking`). Jev runs in a worker waited on for `timeout_s` at most;
/// a router that is None, raises, errs or is slower than that adds nothing, and the keywords stand alone.
/// Python's defaults: `previous` "", `router` None, `timeout_s` TIMEOUT_S.
pub fn route_turn(text: &str, previous: &str, router: Option<&Router>, timeout_s: f64) -> TurnToolsets {
    let found = keyword_toolsets(text, previous);
    let Some(router) = router else {
        return TurnToolsets { keywords: found, ..Default::default() };
    };
    let t0 = Instant::now();
    let elapsed_ms = || t0.elapsed().as_secs_f64() * 1000.0;
    let (tx, rx) = mpsc::channel::<Route>();
    let (r, text_owned, previous_owned) = (router.clone(), text.to_string(), previous.to_string());
    // A router that fails adds nothing. One that panics drops the sender without boxing a Route (Python's
    // BaseException the worker does not catch): that reads as "not a route" below.
    let spawned = std::thread::Builder::new().name("toolset-router".into()).spawn(move || {
        let route = match r(&text_owned, &previous_owned) {
            Ok(route) => route,
            Err(e) => Route { error: exception_name(&e), ..Default::default() },
        };
        let _ = tx.send(route);
    });
    let without = |error: &str| TurnToolsets {
        keywords: found.clone(),
        jev: BTreeSet::new(),
        ms: elapsed_ms(),
        asked: true,
        error: error.to_string(),
    };
    if spawned.is_err() {
        return without("not a route");
    }
    let got = match Duration::try_from_secs_f64(timeout_s.max(0.0)) {
        Ok(d) => rx.recv_timeout(d),
        Err(_) => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
    };
    // A router that answers with anything else adds nothing (review, 2026-10-05); so does one that ended without
    // boxing a Route: the keywords still stand, and log_line can format it (Lean RoutedTurn P4, 2026-10-05).
    let r = match got {
        Ok(r) => r,
        Err(RecvTimeoutError::Timeout) => return without("timeout"),
        Err(RecvTimeoutError::Disconnected) => return without("not a route"),
    };
    let jev = r.toolsets.into_iter().filter(|n| TOOLSETS.contains(&n.as_str())).collect();
    TurnToolsets { keywords: found, jev, ms: r.ms, asked: true, error: r.error }
}

/// Keywords ∪ Jev: the toolsets a turn starts with. Blocking (call it with `tokio::task::spawn_blocking`). Jev
/// runs in a worker waited on for `timeout_s` at most; a router that is None, raises, errs or is slower than that
/// adds nothing, and the keywords stand alone. Logs the toolsets and Jev's milliseconds, never the message.
pub fn toolsets_for(text: &str, previous: &str, router: Option<&Router>, timeout_s: f64) -> BTreeSet<String> {
    let r = route_turn(text, previous, router, timeout_s);
    if r.asked {
        tracing::info!("toolset router: {}", r.log_line());
    }
    r.toolsets()
}

#[cfg(test)]
mod tests;
