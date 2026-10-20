//! A typed-decision model behind the fast lane's `model` mode, asked as ONE `choice` question.
//!
//! `Decider::new(predict, ..)` wraps any callable with the System One signature —
//! `predict(state, questions) -> {"answers": {qid: {...}}, "usage": {...}}` — and `choose()`
//! asks it exactly one thing: which of these options advances the objective now. It hands
//! back a `Choice` that the lane gates on `p_top` and `margin` (from the probabilities), never
//! on the answer's `confidence` (a normalized entropy, not comparable across option counts).
//! Every judgement that matters — done, risk, repeat, dialog — is a code oracle in the fast
//! lane; the model only ranks.
//!
//! The one backend the worker loads is TypeSafe's hosted Jev (jev's `load` builds a Decider
//! over jev's `make_predict`). The seam was written first for a local encoder with a 256-token
//! decision head, which is why the wrapper still respects a head budget BEFORE predict: every
//! option costs at most 48 tokens, and `choose` trims the last non-reserved option until the
//! head fits, counting what it dropped in `dropped_for_budget`; a state that fills the
//! sequence (usage.input_tokens == max_len) is flagged `overflow`. Jev sets both budgets past
//! any real menu, so for Jev the trim never drops an option. The reserved options "reobserve"
//! and "abstain" are never dropped — the caller adds them, the gate keys on them.
//!
//! How an option is worded decides more than the style (select fixtures, 73 cases,
//! 2026-09-21): "click radio button: Week" with the full context lines scored top-1 0.315;
//! `render_option` — "Week (radio button)", "Month (radio button, selected)" — with a
//! "title: …" context scored 0.603. The lane, the planner's outline and the evals all render
//! through `render_option`.
//!
//! A lock serializes every predict, because the worker loads on one thread and serves on
//! another.

use std::collections::HashSet;
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::LazyLock;
use std::time::Instant;

use indexmap::IndexMap;
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Map, Value};

pub const STYLES: [&str; 3] = ["jev", "compact", "hinted"];
pub const RESERVED: [&str; 2] = ["reobserve", "abstain"];
pub const QUESTION_ID: &str = "pick";
/// A marker token + at most 48 option tokens per option in the head.
pub const OPTION_TOKEN_CAP: usize = 48;
/// Head tokens kept free past the options, or every option gets cut.
pub const HEAD_RESERVE: i64 = 16;
pub const TIE_TOLERANCE: f64 = 1e-4;
pub const STOP_WORDS: [&str; 18] = [
    "the", "and", "for", "with", "into", "onto", "from", "this", "that", "please", "now", "then", "one", "all",
    "any", "its", "your", "our",
];
pub const OPTION_LABEL_CHARS: usize = 40;

/// The instruction template per style (`{goal}` is the whitespace-collapsed objective).
pub fn instruction_template(style: &str) -> Option<&'static str> {
    match style {
        "jev" => Some("Which single element should be acted on next to accomplish the goal? Goal: {goal}"),
        "compact" => Some(
            "which one action advances the goal now? reobserve if the screen is changing; abstain if none does",
        ),
        "hinted" => Some(
            "which one action advances the goal now? reobserve if the screen is changing; abstain if none does",
        ),
        _ => None,
    }
}

// The warm-up menu (jev's load): 12 options and a ~250-token state, the shape a real step takes.
pub const WARMUP_OBJECTIVE: &str = "switch to week view";

pub static WARMUP_OPTIONS: LazyLock<IndexMap<String, String>> = LazyLock::new(|| {
    let controls = [
        ("radio button", "Day"),
        ("radio button", "Week"),
        ("radio button", "Month"),
        ("radio button", "Year"),
        ("button", "Today"),
        ("button", "previous month"),
        ("button", "next month"),
        ("button", "Add Event"),
        ("search field", "Search"),
        ("button", "Inspector"),
    ];
    let mut options: IndexMap<String, String> = controls
        .iter()
        .enumerate()
        .map(|(i, (role, label))| ((i + 1).to_string(), format!("click {role}: {label}")))
        .collect();
    options.insert("reobserve".into(), "the screen is still changing, look again".into());
    options.insert("abstain".into(), "none of these advances the objective".into());
    options
});

pub static WARMUP_CONTEXT: LazyLock<String> = LazyLock::new(|| {
    let days = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
    let text: Vec<String> = (0..6)
        .flat_map(|_| days.iter())
        .enumerate()
        .map(|(i, day)| format!("{day} {}", i + 1))
        .collect();
    format!("title: September 2026; focused: radio button Month; text: {}", text.join(" "))
});

/// A caller contract violation (Python's `ValueError`). The lane logs it as
/// `choose raised ValueError: …`, so `kind()` names the Python class.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeciderError {
    #[error("{0}")]
    Value(String),
}

impl DeciderError {
    /// The Python exception class this error stands for.
    pub fn kind(&self) -> &'static str {
        match self {
            DeciderError::Value(_) => "ValueError",
        }
    }
}

/// What a `predict` failure looks like: Python's exception class name and `str(e)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredictError {
    pub kind: String,
    pub message: String,
}

impl PredictError {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self { kind: kind.into(), message: message.into() }
    }
}

impl fmt::Display for PredictError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PredictError {}

impl From<anyhow::Error> for PredictError {
    fn from(e: anyhow::Error) -> Self {
        PredictError::new("Exception", e.to_string())
    }
}

/// One answer. `id` is a key of the options, or "" when the answer was rejected (see `error`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Choice {
    pub id: String,
    pub p_top: f64,
    pub margin: f64,
    /// The model's normalized entropy — LOGGING ONLY.
    pub confidence: f64,
    pub probabilities: IndexMap<String, f64>,
    pub ms: f64,
    pub input_tokens: i64,
    pub k: usize,
    /// The state was right-truncated (usage.input_tokens == max_len).
    pub overflow: bool,
    /// Options removed by the head-budget trim before predict.
    pub dropped_for_budget: usize,
    pub error: String,
}

impl Choice {
    /// A choice with only its id set (every other field at its default).
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into(), ..Self::default() }
    }
}

/// Python's `str.casefold()`, as far as it matters to `[a-z0-9]+`: `to_lowercase` plus the
/// full case foldings that yield ASCII letters where lowercasing does not.
fn casefold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        let folded: Option<&str> = match c {
            '\u{df}' | '\u{1e9e}' => Some("ss"),
            '\u{149}' => Some("\u{2bc}n"),
            '\u{17f}' => Some("s"),
            '\u{1f0}' => Some("j\u{30c}"),
            '\u{1e96}' => Some("h\u{331}"),
            '\u{1e97}' => Some("t\u{308}"),
            '\u{1e98}' => Some("w\u{30a}"),
            '\u{1e99}' => Some("y\u{30a}"),
            '\u{1e9a}' => Some("a\u{2be}"),
            '\u{fb00}' => Some("ff"),
            '\u{fb01}' => Some("fi"),
            '\u{fb02}' => Some("fl"),
            '\u{fb03}' => Some("ffi"),
            '\u{fb04}' => Some("ffl"),
            '\u{fb05}' | '\u{fb06}' => Some("st"),
            _ => None,
        };
        match folded {
            Some(s) => out.push_str(s),
            None => out.extend(c.to_lowercase()),
        }
    }
    out
}

/// The words of an objective worth matching: ≥ 3 chars, case-folded, stop-words removed.
pub fn objective_tokens(text: &str) -> HashSet<String> {
    casefold(text)
        .split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit()))
        .filter(|w| w.len() >= 3 && !STOP_WORDS.contains(w))
        .map(str::to_string)
        .collect()
}

/// A control as `render_option` reads it: Python's `getattr(c, "label", "") or ""` and
/// friends, so a missing or empty field is `None` or "".
pub trait OptionFields {
    fn label(&self) -> Option<&str> {
        None
    }
    fn role(&self) -> Option<&str> {
        None
    }
    fn value(&self) -> Option<&str> {
        None
    }
}

/// A plain control for callers (and tests) that have no candidate type of their own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OptionControl {
    pub label: Option<String>,
    pub role: Option<String>,
    pub value: Option<String>,
}

impl OptionFields for OptionControl {
    fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }
    fn role(&self) -> Option<&str> {
        self.role.as_deref()
    }
    fn value(&self) -> Option<&str> {
        self.value.as_deref()
    }
}

/// Python's `str.split()` with no separator: Rust's whitespace plus U+001C..U+001F.
fn py_split(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)).filter(|w| !w.is_empty())
}

/// Python's `" ".join(text.split())`.
fn py_collapse(text: &str) -> String {
    py_split(text).collect::<Vec<_>>().join(" ")
}

fn collapse(text: Option<&str>) -> String {
    py_collapse(text.unwrap_or(""))
}

/// The option text the model sees for a control: `label (role)` or `label (role, value)`,
/// e.g. "Week (radio button)", "Month (radio button, selected)". The one place the lane and
/// the eval render a candidate (the crate docs have the numbers behind the wording).
pub fn render_option<C: OptionFields + ?Sized>(c: &C) -> String {
    let label: String = collapse(c.label()).chars().take(OPTION_LABEL_CHARS).collect();
    let role = collapse(c.role());
    let value = collapse(c.value());
    let inner = if value.is_empty() { role } else { format!("{role}, {value}") };
    if inner.is_empty() {
        label
    } else {
        format!("{label} ({inner})")
    }
}

fn hint_keys(objective: &str, options: &IndexMap<String, String>) -> Vec<String> {
    let words = objective_tokens(objective);
    if words.is_empty() {
        return Vec::new();
    }
    options
        .iter()
        .filter(|(k, text)| {
            !RESERVED.contains(&k.as_str()) && !words.is_disjoint(&objective_tokens(text))
        })
        .map(|(k, _)| k.clone())
        .collect()
}

fn styles_repr() -> String {
    let inner: Vec<String> = STYLES.iter().map(|s| py_str_repr(s)).collect();
    format!("({})", inner.join(", "))
}

fn check_style(style: &str) -> Result<&'static str, DeciderError> {
    instruction_template(style).ok_or_else(|| {
        DeciderError::Value(format!("style must be one of {}, not {}", styles_repr(), py_str_repr(style)))
    })
}

/// The instruction sentence of the one `choice` question, per style.
pub fn render_instructions(
    style: &str,
    objective: &str,
    options: &IndexMap<String, String>,
) -> Result<String, DeciderError> {
    let template = check_style(style)?;
    let goal = py_collapse(objective);
    let mut text = template.replace("{goal}", &goal);
    if style == "hinted" {
        let keys = hint_keys(objective, options);
        if !keys.is_empty() {
            text.push_str(" matches: ");
            text.push_str(&keys.join(", "));
        }
    }
    Ok(text)
}

/// The question dict for `choose`, so the eval can print exactly what the model was asked.
pub fn render_question(
    style: &str,
    objective: &str,
    options: &IndexMap<String, String>,
) -> Result<Value, DeciderError> {
    let criteria: Map<String, Value> = options.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();
    Ok(json!({
        "type": "choice",
        "instructions": render_instructions(style, objective, options)?,
        "criteria": criteria,
    }))
}

/// The token estimate used when no tokenizer is injected (tests, fakes).
pub fn word_count(text: &str) -> usize {
    py_split(text).count()
}

/// A token counter: text in, token count out.
pub type Tokenize = dyn Fn(&str) -> usize + Send + Sync;

/// Tokens the head spends before the options: "choice question: <instructions>".
pub fn instruction_tokens(
    style: &str,
    objective: &str,
    options: Option<&IndexMap<String, String>>,
    tokenize: Option<&Tokenize>,
) -> Result<usize, DeciderError> {
    let empty = IndexMap::new();
    let text = format!("choice question: {}", render_instructions(style, objective, options.unwrap_or(&empty))?);
    Ok(match tokenize {
        Some(count) => count(&text),
        None => word_count(&text),
    })
}

/// Tokens one option costs in the head: a marker token plus at most 48 text tokens.
pub fn option_tokens(text: &str, tokenize: Option<&Tokenize>) -> usize {
    let spaced = format!(" {text}");
    let n = match tokenize {
        Some(count) => count(&spaced),
        None => word_count(&spaced),
    };
    1 + OPTION_TOKEN_CAP.min(n)
}

/// Drop the LAST non-reserved options until the rendered options fit `budget` tokens.
///
/// Options arrive in rank order, so the least likely go first. The reserved two are
/// never dropped, and at least one real option always stays (the caller sees the count
/// in dropped_for_budget). Returns
/// (kept options in their original order, dropped count).
pub fn trim_options(
    options: &IndexMap<String, String>,
    budget: i64,
    tokenize: Option<&Tokenize>,
) -> (IndexMap<String, String>, usize) {
    let mut kept = options.clone();
    let costs: IndexMap<&str, i64> =
        options.iter().map(|(k, v)| (k.as_str(), option_tokens(v, tokenize) as i64)).collect();
    let mut dropped = 0;
    let mut real: Vec<String> = kept.keys().filter(|k| !RESERVED.contains(&k.as_str())).cloned().collect();
    while kept.keys().map(|k| costs[k.as_str()]).sum::<i64>() > budget && real.len() > 1 {
        if let Some(gone) = real.pop() {
            kept.shift_remove(&gone);
            dropped += 1;
        }
    }
    (kept, dropped)
}

/// `value` as a float in [0, 1], or None when it is not one.
fn finite_unit(value: Option<&Value>) -> Option<f64> {
    // A JSON bool is its own variant, so Python's bool exclusion is automatic.
    let f = value?.as_f64()?;
    if !f.is_finite() || !(0.0..=1.0).contains(&f) {
        return None;
    }
    Some(f)
}

/// The predict seam: `predict(state, questions) -> result dict`.
pub type Predict = dyn Fn(&Value, &Value) -> Result<Value, PredictError> + Send + Sync;
/// A monotonic clock in seconds (Python's `time.perf_counter`).
pub type Clock = dyn Fn() -> f64 + Send + Sync;

static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

/// `time.perf_counter()`: seconds on a monotonic clock with an arbitrary origin.
pub fn perf_counter() -> f64 {
    EPOCH.elapsed().as_secs_f64()
}

/// Python's keyword arguments to `Decider(...)`, with the same defaults.
pub struct DeciderConfig {
    pub tokenize: Option<Box<Tokenize>>,
    pub max_len: i64,
    pub head_max_len: i64,
    pub clock: Box<Clock>,
    pub style: String,
}

impl Default for DeciderConfig {
    fn default() -> Self {
        Self {
            tokenize: None,
            max_len: 1024,
            head_max_len: 256,
            clock: Box::new(perf_counter),
            style: "compact".into(),
        }
    }
}

/// A typed-decision model behind an injectable `predict(state, questions) -> result dict`.
///
/// `available` turns true once a warm-up answer came back (jev's load sets it).
pub struct Decider {
    predict: Box<Predict>,
    tokenize: Option<Box<Tokenize>>,
    pub max_len: i64,
    pub head_max_len: i64,
    clock: Box<Clock>,
    pub style: String,
    /// True only after a loader's warm-up (jev's load).
    pub available: bool,
    pub load_ms: f64,
    pub warm_ms: f64,
    lock: Mutex<()>,
}

impl fmt::Debug for Decider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Decider")
            .field("max_len", &self.max_len)
            .field("head_max_len", &self.head_max_len)
            .field("style", &self.style)
            .field("available", &self.available)
            .field("load_ms", &self.load_ms)
            .field("warm_ms", &self.warm_ms)
            .finish_non_exhaustive()
    }
}

fn truncate_chars(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

impl Decider {
    pub fn new<P>(predict: P, config: DeciderConfig) -> Result<Self, DeciderError>
    where
        P: Fn(&Value, &Value) -> Result<Value, PredictError> + Send + Sync + 'static,
    {
        check_style(&config.style)?;
        Ok(Self {
            predict: Box::new(predict),
            tokenize: config.tokenize,
            max_len: config.max_len,
            head_max_len: config.head_max_len,
            clock: config.clock,
            style: config.style,
            available: false,
            load_ms: 0.0,
            warm_ms: 0.0,
            lock: Mutex::new(()),
        })
    }

    /// Whether a predict is running right now (the predict lock is held).
    pub fn predict_locked(&self) -> bool {
        self.lock.is_locked()
    }

    // -- the one question --

    /// Which option advances the objective now. Never fails for a model failure: see `error`.
    pub fn choose(
        &self,
        objective: &str,
        app: &str,
        context: &str,
        options: &IndexMap<String, String>,
        recent: &[String],
    ) -> Result<Choice, DeciderError> {
        if options.is_empty() {
            return Err(DeciderError::Value("options must be a non-empty {str: str} mapping".into()));
        }
        if RESERVED.iter().any(|key| !options.contains_key(*key)) {
            let reserved: Vec<String> = RESERVED.iter().map(|s| py_str_repr(s)).collect();
            return Err(DeciderError::Value(format!(
                "options must include the reserved keys ({})",
                reserved.join(", ")
            )));
        }
        if options.len() == RESERVED.len() {
            return Err(DeciderError::Value("options must include at least one real option".into()));
        }
        let tokenize = self.tokenize.as_deref();
        let head = instruction_tokens(&self.style, objective, Some(options), tokenize)? as i64;
        let budget = self.head_max_len - HEAD_RESERVE - head;
        let (kept, dropped) = trim_options(options, budget, tokenize);
        let question = render_question(&self.style, objective, &kept)?;
        let state = json!({"goal": objective, "app": app, "context": context, "recent": recent});
        let mut questions = Map::new();
        questions.insert(QUESTION_ID.into(), question);
        let questions = Value::Object(questions);
        let t0 = (self.clock)();
        let outcome = {
            let _held = self.lock.lock();
            // A panic in predict is the lane's "unavailable" too, never a crash.
            catch_unwind(AssertUnwindSafe(|| (self.predict)(&state, &questions)))
        };
        let failure = match outcome {
            Ok(Ok(result)) => {
                let ms = ((self.clock)() - t0) * 1000.0;
                return Ok(self.parse_choice(&result, &kept, ms, dropped));
            }
            Ok(Err(e)) => format!("predict raised {}: {}", e.kind, e.message),
            Err(panic) => {
                let message = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_default();
                format!("predict raised panic: {message}")
            }
        };
        Ok(Choice {
            ms: ((self.clock)() - t0) * 1000.0,
            k: kept.len(),
            dropped_for_budget: dropped,
            error: truncate_chars(&failure, 200),
            ..Choice::default()
        })
    }

    fn parse_choice(&self, result: &Value, options: &IndexMap<String, String>, ms: f64, dropped: usize) -> Choice {
        let k = options.len();
        let rejected = |why: String, input_tokens: i64, overflow: bool, probabilities: IndexMap<String, f64>| Choice {
            ms,
            k,
            dropped_for_budget: dropped,
            error: why,
            input_tokens,
            overflow,
            probabilities,
            ..Choice::default()
        };

        let Some(result) = result.as_object() else {
            return rejected("predict returned no dict".into(), 0, false, IndexMap::new());
        };
        let tokens = result.get("usage").and_then(Value::as_object).and_then(|u| u.get("input_tokens"));
        let input_tokens = match tokens {
            Some(Value::Number(n)) if n.is_i64() => n.as_i64().unwrap_or(0),
            Some(Value::Number(n)) if n.is_u64() => i64::MAX,
            _ => 0,
        };
        let overflow = input_tokens >= self.max_len;
        let answer = result
            .get("answers")
            .and_then(Value::as_object)
            .and_then(|a| a.get(QUESTION_ID))
            .and_then(Value::as_object);
        let Some(answer) = answer else {
            return rejected("predict returned no answer".into(), input_tokens, overflow, IndexMap::new());
        };
        let chosen = answer.get("choice");
        let chosen_key = match chosen {
            Some(Value::String(s)) if options.contains_key(s) => s.clone(),
            other => {
                return rejected(
                    format!("choice {} is not an option", py_repr(other.unwrap_or(&Value::Null))),
                    input_tokens,
                    overflow,
                    IndexMap::new(),
                )
            }
        };
        let probs = answer.get("probabilities").and_then(Value::as_object);
        let covers = probs.is_some_and(|p| p.len() == options.len() && p.keys().all(|key| options.contains_key(key)));
        let Some(probs) = probs.filter(|_| covers) else {
            return rejected("probabilities do not cover the options".into(), input_tokens, overflow, IndexMap::new());
        };
        let mut clean: IndexMap<String, f64> = IndexMap::new();
        for key in options.keys() {
            // option order, for a stable log line
            match finite_unit(probs.get(key)) {
                Some(value) => {
                    clean.insert(key.clone(), value);
                }
                None => {
                    return rejected(
                        format!("probability of {} is not in [0, 1]", py_str_repr(key)),
                        input_tokens,
                        overflow,
                        IndexMap::new(),
                    )
                }
            }
        }
        let mut ranked: Vec<f64> = clean.values().copied().collect();
        ranked.sort_by(|a, b| b.total_cmp(a));
        let p_top = clean[&chosen_key];
        if p_top < ranked[0] - TIE_TOLERANCE {
            return rejected("choice is not the most probable option".into(), input_tokens, overflow, clean);
        }
        let margin = p_top - ranked.get(1).copied().unwrap_or(0.0);
        let confidence = finite_unit(answer.get("confidence"));
        Choice {
            id: chosen_key,
            p_top,
            margin: margin.max(0.0),
            confidence: confidence.unwrap_or(0.0),
            probabilities: clean,
            ms,
            input_tokens,
            k,
            overflow,
            dropped_for_budget: dropped,
            error: String::new(),
        }
    }
}

/// Python's `repr()` of a str.
pub fn py_str_repr(s: &str) -> String {
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
            c if is_printable(c) => out.push(c),
            c => {
                let cp = c as u32;
                if cp < 0x100 {
                    out.push_str(&format!("\\x{cp:02x}"));
                } else if cp < 0x10000 {
                    out.push_str(&format!("\\u{cp:04x}"));
                } else {
                    out.push_str(&format!("\\U{cp:08x}"));
                }
            }
        }
    }
    out.push(quote);
    out
}

/// An approximation of Python's `str.isprintable()` for one char: controls, separators other
/// than the ASCII space, surrogates and unassigned format marks are escaped.
fn is_printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    if c.is_control() || c.is_whitespace() {
        return false;
    }
    !matches!(c as u32,
        0xad | 0x600..=0x605 | 0x61c | 0x6dd | 0x70f | 0x8e2 | 0x180e | 0x200b..=0x200f | 0x202a..=0x202e
        | 0x2060..=0x2064 | 0x2066..=0x206f | 0xfeff | 0xfff9..=0xfffb | 0xe000..=0xf8ff
        | 0x110bd | 0x110cd | 0x1bca0..=0x1bca3 | 0x1d173..=0x1d17a | 0xe0001 | 0xe0020..=0xe007f
        | 0xf0000..=0x10ffff)
}

/// Python's `repr()` of a float.
fn py_float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let sci = format!("{f:e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if (-4..16).contains(&exp) {
        let fixed = format!("{f}");
        if fixed.contains('.') {
            fixed
        } else {
            format!("{fixed}.0")
        }
    } else {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{:02}", exp.abs())
    }
}

/// Python's `repr()` of the object a JSON value decodes to (`json.loads`).
pub fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => i.to_string(),
            (None, Some(u)) => u.to_string(),
            _ => py_float_repr(n.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(s) => py_str_repr(s),
        Value::Array(items) => format!("[{}]", items.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter().map(|(k, v)| format!("{}: {}", py_str_repr(k), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

#[cfg(test)]
mod tests;
