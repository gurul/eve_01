//! Conversation-aware eye choice: Jev picks one of eighteen eye labels (default), or local Laya does.
//!
//! Port of `eve_bridge/eye_model.py`. Two models, one contract — `predict(state) -> {"label", "probabilities"
//! (in LABELS order), "ms"}` ([`EyeModel`], [`EyeAnswer`]):
//!
//! * [`JevEyeModel`] (owner, 2026-09-24: "use jev for that too"): TypeSafe's Jev, asked ONE `choice` over the
//!   labels — its native shape (it ranks options; ask each model in the shape it is built for). Measured on this
//!   Mac 2026-09-24 with the then eleven labels (tools/jev_eyes_eval.py, the 84 hand-written cases in
//!   tests/fixtures/emotion, both pickers asked the eleven-label question and folded onto the sets' six labels the
//!   same way): Jev 62.5% on the 48 scenario tests and 72.2% on the 36 confirmation cases, against 47.9% and
//!   52.8% for untouched Laya; 200 ms p50 and about $0.00002 a call. 14 of Jev's 32 misses are the sets'
//!   "startled" read as "surprised", which is the fold, not the eye. Jev is a network call: the state is the last
//!   turn or two of conversation, and it leaves the Mac only while the owner has expressions enabled.
//! * [`LiveEyeModel`] (`EVE_EXPRESSION_BACKEND=laya`): the original local Laya weights, one binary question per
//!   label. No weights are trained here. Kept for a Mac without network or without a Jev key. laya-mlx is a
//!   Python-only MLX runtime, so the loaded agent lives in a helper Python process (`src/laya_eye_helper.py`).
//!
//! The seven labels after wink (owner, 2026-10-12: "also build more expressions") were chosen by a judge panel and
//! a critic (docs/expressions-design.json) for how often the moment comes up, how well the face reads at phone size
//! and how cleanly it separates from the labels next to it; wink's criterion lost "a joke" so laughing alone owns
//! humour. They ship only on the bar pre-registered in docs/more-expressions-gates.md before any result existed: on
//! a blind set, top-1 >= 65%, recall >= 55% for every label with five or more cases, p50 <= 400 ms and errors <= 2%;
//! and on the 84 cases above, folded as on 2026-09-24, no more than five points below that baseline on either set.
//! The design had an eighth, thinking; it failed the blind bar (recall 3/8) and was dropped (owner, 2026-10-12: "ship
//! 7, drop thinking"). The face's own thinking mood already covers eve working on an answer.
//! The order of LABELS is the wire contract with the phone's face (the original eleven first, the new seven after).

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, LazyLock};
use std::time::Instant;

use eve_jev::{Env, JevError, JevPredict, Meter, Opener, PredictFailure, PredictOptions, ProcessEnv};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

#[cfg(test)]
mod tests;

// ---- the label set ---------------------------------------------------------------------------------------

/// Each label and the criterion Jev (and each Laya question) is asked about, in wire order.
pub const CRITERIA: [(&str, &str); 18] = [
    ("calm", "neutral facts, routine questions or instructions"),
    ("happy", "pleased, content or gently celebrating"),
    ("curious", "wondering, exploring or wanting to learn"),
    ("affection", "love, gratitude, friendship or reassurance"),
    ("surprised", "shocked or amazed by something unexpected"),
    ("sad", "sadness, grief, loss or disappointment"),
    ("worried", "fear, anxiety, concern or danger"),
    ("skeptical", "doubt, disbelief or questioning a claim"),
    ("frustrated", "annoyed or fed up with a problem"),
    ("excited", "very enthusiastic, thrilled or eagerly anticipating"),
    ("wink", "playful teasing, a cheeky aside or asking for a wink"),
    ("laughing", "laughter or finding something genuinely funny"),
    ("sleepy", "sleepiness, drowsiness, bedtime or saying goodnight"),
    ("confused", "not understanding what the owner means, or asking them to repeat or clarify"),
    ("sheepish", "an apology, an admitted mistake or a small embarrassing slip"),
    ("relieved", "relief that a fear or problem turned out fine"),
    ("proud", "pride in a finished task or a personal achievement"),
    ("flattered", "being praised or complimented and feeling bashful"),
];

/// `tuple(CRITERIA)`: the wire order the phone's face reads.
pub const LABELS: [&str; 18] = {
    let mut out = [""; 18];
    let mut i = 0;
    while i < CRITERIA.len() {
        out[i] = CRITERIA[i].0;
        i += 1;
    }
    out
};

/// `CRITERIA[label]`.
pub fn criterion(label: &str) -> Option<&'static str> {
    CRITERIA.iter().find(|(name, _)| *name == label).map(|(_, text)| *text)
}

/// `LABELS.index(label)`.
pub fn label_index(label: &str) -> Option<usize> {
    LABELS.iter().position(|name| *name == label)
}

// How long a label's face holds (docs/expressions-design.json; the original eleven keep 4 s). live_expressions
// subtracts the turn's age and clamps the result to TTL_MIN_MS..TTL_MAX_MS.
pub const DEFAULT_TTL_MS: i64 = 4000;

/// `TTL_MS`: every label's hold in milliseconds, in LABELS order.
pub const TTL_MS: [(&str, i64); 18] = [
    ("calm", DEFAULT_TTL_MS),
    ("happy", DEFAULT_TTL_MS),
    ("curious", DEFAULT_TTL_MS),
    ("affection", DEFAULT_TTL_MS),
    ("surprised", DEFAULT_TTL_MS),
    ("sad", DEFAULT_TTL_MS),
    ("worried", DEFAULT_TTL_MS),
    ("skeptical", DEFAULT_TTL_MS),
    ("frustrated", DEFAULT_TTL_MS),
    ("excited", DEFAULT_TTL_MS),
    ("wink", DEFAULT_TTL_MS),
    ("laughing", 3000),
    ("sleepy", 5000),
    ("confused", 3500),
    ("sheepish", 4000),
    ("relieved", 3500),
    ("proud", 4000),
    ("flattered", 4000),
];

/// `TTL_MS.get(label)`.
pub fn ttl_ms(label: &str) -> Option<i64> {
    TTL_MS.iter().find(|(name, _)| *name == label).map(|(_, ms)| *ms)
}

pub const TTL_MIN_MS: i64 = 500;
pub const TTL_MAX_MS: i64 = 6000;

/// `QUESTION`: Laya's shape, one binary `noul` question per label, keyed by label in LABELS order.
pub static QUESTION: LazyLock<Value> = LazyLock::new(|| {
    let mut out = Map::new();
    for (label, description) in CRITERIA {
        out.insert(
            label.to_string(),
            json!({
                "type": "noul",
                "instructions": format!("Does the last message express {description}?"),
                "criteria": {"false": "No", "true": "Yes"},
            }),
        );
    }
    Value::Object(out)
});

/// `CRITERIA` as the JSON object Jev is sent (keys in LABELS order).
fn criteria_object() -> Value {
    Value::Object(CRITERIA.iter().map(|(k, v)| (k.to_string(), Value::String(v.to_string()))).collect())
}

/// `EYE_CHOICE`: Jev's shape, one `choice` question over every label.
pub static EYE_CHOICE: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "eye": {
            "type": "choice",
            "instructions": "Which expression fits Eve's eyes for the last message?",
            "criteria": criteria_object(),
        }
    })
});

pub const WARM_STATE: &str = "The room is quiet and nothing has changed.";

// ---- the contract ----------------------------------------------------------------------------------------

/// What `predict` returns: `{"label", "probabilities" (in LABELS order), "ms"}`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EyeAnswer {
    pub label: String,
    pub probabilities: Vec<f64>,
    pub ms: f64,
}

impl EyeAnswer {
    /// The dict Python returns, keys in the same order.
    pub fn to_value(&self) -> Value {
        json!({"label": self.label, "probabilities": self.probabilities, "ms": self.ms})
    }
}

/// What `predict` (or `LiveEyeModel`'s load) raises. `Display` is `str(exc)`; [`EyeError::kind`] is
/// `type(exc).__name__`, so the worker's `f"{type(exc).__name__}: {exc}"` reads the same.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EyeError {
    /// `ValueError`: an answer the model cannot stand behind.
    #[error("{0}")]
    Value(String),
    /// Whatever Jev's predict raised (`JevError`, or an exception urllib lets through).
    #[error("{0}")]
    Jev(#[from] PredictFailure),
    /// Any other Python exception, by class name (`AttributeError`, `TypeError`, laya-mlx's own).
    #[error("{message}")]
    Raised { kind: String, message: String },
}

impl EyeError {
    pub fn kind(&self) -> &str {
        match self {
            EyeError::Value(_) => "ValueError",
            EyeError::Jev(failure) => failure.kind(),
            EyeError::Raised { kind, .. } => kind,
        }
    }

    fn raised(kind: impl Into<String>, message: impl Into<String>) -> Self {
        EyeError::Raised { kind: kind.into(), message: message.into() }
    }
}

/// `model.predict(state)`, the one method live_expressions calls on either model.
pub trait EyeModel: Send + Sync {
    fn predict(&self, state: &str) -> Result<EyeAnswer, EyeError>;
}

// ---- Python value semantics ------------------------------------------------------------------------------

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

/// Python's whitespace for `str.strip()`: Unicode White_Space plus the ASCII separators U+001C..U+001F.
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `float(text)` for a string: surrounding whitespace stripped, `inf`/`infinity`/`nan` in any case with an
/// optional sign, and digit-group underscores only between two digits. `None` is Python's ValueError.
fn py_float_str(text: &str) -> Option<f64> {
    let raw = text.trim_matches(py_space);
    if raw.is_empty() {
        return None;
    }
    let unsigned = raw.strip_prefix(['+', '-']).unwrap_or(raw);
    let negative = raw.starts_with('-');
    let lower = unsigned.to_ascii_lowercase();
    if lower == "inf" || lower == "infinity" {
        return Some(if negative { f64::NEG_INFINITY } else { f64::INFINITY });
    }
    if lower == "nan" {
        return Some(f64::NAN);
    }
    // From here on only a decimal literal: digits, one '.', an exponent. Rust's parser would also take
    // spellings Python refuses, so the shape is checked first.
    let bytes = unsigned.as_bytes();
    if !bytes.iter().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-' | b'_')) {
        return None;
    }
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'_' {
            let before = i > 0 && bytes[i - 1].is_ascii_digit();
            let after = bytes.get(i + 1).is_some_and(u8::is_ascii_digit);
            if !(before && after) {
                return None;
            }
        }
    }
    let cleaned = raw.replace('_', "");
    cleaned.parse::<f64>().ok()
}

/// `float(probs.get(label) or 0.0)`.
fn py_float_of(v: &Value) -> Result<f64, EyeError> {
    if !truthy(v) {
        return Ok(0.0);
    }
    match v {
        Value::Bool(_) => Ok(1.0),
        Value::Number(n) => Ok(n.as_f64().unwrap_or(f64::INFINITY)),
        Value::String(s) => py_float_str(s).ok_or_else(|| {
            EyeError::Value(format!("could not convert string to float: {}", eve_decider::py_str_repr(s)))
        }),
        other => Err(EyeError::raised(
            "TypeError",
            format!("float() argument must be a string or a real number, not '{}'", py_type_name(other)),
        )),
    }
}

/// Python's `sum()` over floats, which (3.12 and later; the bridge runs 3.13) adds with Neumaier compensation.
fn py_fsum(values: &[f64]) -> f64 {
    let mut total = 0.0f64;
    let mut compensation = 0.0f64;
    for &x in values {
        let t = total + x;
        if total.abs() >= x.abs() {
            compensation += (total - t) + x;
        } else {
            compensation += (x - t) + total;
        }
        total = t;
    }
    if compensation != 0.0 && compensation.is_finite() {
        total + compensation
    } else {
        total
    }
}

/// `max(range(len(p)), key=p.__getitem__)`: the first index of the largest value.
fn argmax(p: &[f64]) -> usize {
    let mut best = 0;
    for (i, x) in p.iter().enumerate().skip(1) {
        if *x > p[best] {
            best = i;
        }
    }
    best
}

// ---- Jev -------------------------------------------------------------------------------------------------

/// Python's keyword arguments to `JevEyeModel(env, opener=..., timeout_s=...)`, plus the meters as seams.
#[derive(Clone, Default)]
pub struct JevEyeOptions {
    /// `opener=`: jev.make_predict's socket seam for tests. `None` is the real HTTPS.
    pub opener: Option<Arc<dyn Opener>>,
    /// `timeout_s=`: 0 (the default) reads `EVE_JEV_TIMEOUT` (`jev.timeout_from_env`).
    pub timeout_s: f64,
    /// The call meter; `None` is the process-wide `jev.METER`.
    pub meter: Option<Arc<Meter>>,
    /// The spend ledger; `None` is the configured one.
    pub ledger: Option<eve_spend::Ledger>,
}

/// Jev picks the eye label: one `choice` question over CRITERIA, the conversation as the state.
///
/// `env` supplies the route and key (jev.route_config: EVE_JEV_ROUTE and its key); `options.opener` is
/// jev.make_predict's socket seam for tests. Fails with a [`JevError`] at construction when no key is set, so
/// the worker reports "no Jev key" instead of failing on every turn.
#[derive(Clone, Debug)]
pub struct JevEyeModel {
    pub model: String,
    predict: JevPredict,
}

impl JevEyeModel {
    pub fn new<E: Env + ?Sized>(env: &E, options: JevEyeOptions) -> Result<Self, JevError> {
        let (url, key, model) = eve_jev::route_config(env, "")?;
        // `timeout_s or jev.timeout_from_env(source)`: only a zero is falsy.
        let timeout_s = if options.timeout_s != 0.0 { options.timeout_s } else { eve_jev::timeout_from_env(env) };
        let predict = eve_jev::make_predict(
            &url,
            &key,
            &model,
            PredictOptions {
                timeout_s,
                opener: options.opener,
                meter: options.meter,
                ledger: options.ledger,
                ..PredictOptions::default()
            },
        );
        Ok(Self { model, predict })
    }

    /// `JevEyeModel()`: the route, key and timeout from `os.environ`.
    pub fn from_env() -> Result<Self, JevError> {
        Self::new(&ProcessEnv, JevEyeOptions::default())
    }
}

impl EyeModel for JevEyeModel {
    fn predict(&self, state: &str) -> Result<EyeAnswer, EyeError> {
        let start = Instant::now();
        let body = self.predict.predict(&Value::String(state.to_string()), &EYE_CHOICE)?;
        let answer = match body.as_object() {
            Some(body) => {
                // `(body.get("answers") or {}).get("eye")`
                match body.get("answers").filter(|a| truthy(a)) {
                    None => None,
                    Some(Value::Object(answers)) => answers.get("eye"),
                    Some(other) => {
                        return Err(EyeError::raised(
                            "AttributeError",
                            format!("'{}' object has no attribute 'get'", py_type_name(other)),
                        ))
                    }
                }
            }
            None => None,
        };
        let probs = answer.and_then(Value::as_object).and_then(|a| a.get("probabilities"));
        let Some(Value::Object(probs)) = probs else {
            return Err(EyeError::Value("jev returned no eye probabilities".into()));
        };
        let p = LABELS
            .iter()
            .map(|label| probs.get(*label).map_or(Ok(0.0), py_float_of))
            .collect::<Result<Vec<f64>, EyeError>>()?;
        let total = py_fsum(&p);
        if !total.is_finite() || total <= 0.0 || p.iter().any(|x| *x < 0.0) {
            return Err(EyeError::Value("jev returned unusable eye probabilities".into()));
        }
        // LiveExpressions.label wants a distribution over LABELS
        let p: Vec<f64> = p.iter().map(|x| x / total).collect();
        Ok(EyeAnswer {
            label: LABELS[argmax(&p)].to_string(),
            probabilities: p,
            ms: start.elapsed().as_secs_f64() * 1000.0,
        })
    }
}

// ---- conversation context --------------------------------------------------------------------------------

/// The last user and assistant turns, so the eye reads a line in its conversation.
#[derive(Debug, Clone, Default)]
pub struct ConversationContext {
    /// `self.turns`: role -> (at, text), in the order each role was first seen (a dict keeps that order).
    pub turns: Vec<(String, (f64, String))>,
}

impl ConversationContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&mut self, who: &str, text: &str, now: f64) -> String {
        // Bounded chronological context: previous turn first, current speaker last.
        let speaker = match who {
            "assistant" => "Eve",
            "diary" => "Eve's observation",
            _ => "User",
        };
        let mut parts = Vec::new();
        for (role, (at, previous)) in &self.turns {
            if now - at <= 90.0 && role != who {
                let name = if role == "user" { "User" } else { "Eve" };
                parts.push(format!("{name}: {}", last_chars(previous, 400)));
            }
        }
        parts.push(format!("{speaker}: {text}"));
        if who == "user" || who == "assistant" {
            match self.turns.iter_mut().find(|(role, _)| role == who) {
                Some((_, slot)) => *slot = (now, text.to_string()),
                None => self.turns.push((who.to_string(), (now, text.to_string()))),
            }
        }
        parts.join("\n")
    }
}

/// `text[-n:]`, by code point.
fn last_chars(text: &str, n: usize) -> &str {
    let count = text.chars().count();
    if count <= n {
        return text;
    }
    let skip = count - n;
    let (at, _) = text.char_indices().nth(skip).unwrap_or((text.len(), ' '));
    &text[at..]
}

// ---- local Laya ------------------------------------------------------------------------------------------

const HELPER: &str = include_str!("laya_eye_helper.py");
const PYTHON_ENV: &str = "EVE_EXPRESSION_PYTHON";

/// The Python that has laya-mlx: EVE_EXPRESSION_PYTHON, else the bridge checkout's venv, else `python3`.
pub fn default_python() -> PathBuf {
    if let Some(p) = std::env::var_os(PYTHON_ENV).filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    let venv = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/.venv/bin/python3"));
    if venv.exists() {
        return venv;
    }
    PathBuf::from("python3")
}

struct Helper {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Helper {
    fn read(&mut self) -> Result<Value, EyeError> {
        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).map_err(|e| EyeError::raised("OSError", e.to_string()))?;
        if n == 0 {
            let status = self.child.try_wait().ok().flatten();
            return Err(EyeError::raised(
                "RuntimeError",
                format!("laya eye helper exited{}", status.map(|s| format!(" ({s})")).unwrap_or_default()),
            ));
        }
        serde_json::from_str(line.trim_end())
            .map_err(|e| EyeError::raised("RuntimeError", format!("laya eye helper sent a bad line: {e}")))
    }

    fn ask(&mut self, state: &str) -> Result<Value, EyeError> {
        let request = json!({"state": state});
        writeln!(self.stdin, "{request}")
            .and_then(|_| self.stdin.flush())
            .map_err(|e| EyeError::raised("OSError", e.to_string()))?;
        self.read()
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The exception a helper reply reports, `{"kind", "error"}`.
fn reported(reply: &Value) -> EyeError {
    let kind = reply.get("kind").and_then(Value::as_str).unwrap_or("RuntimeError");
    let message = reply.get("error").and_then(Value::as_str).unwrap_or_default();
    if kind == "ValueError" {
        EyeError::Value(message.to_string())
    } else {
        EyeError::raised(kind, message)
    }
}

/// The original local Laya weights at `path`, one binary question per label (QUESTION), softmaxed over the
/// labels' yes-minus-no scores.
pub struct LiveEyeModel {
    pub path: PathBuf,
    python: PathBuf,
    env: Vec<(OsString, OsString)>,
    helper: Mutex<Option<Helper>>,
}

impl std::fmt::Debug for LiveEyeModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveEyeModel").field("path", &self.path).field("python", &self.python).finish_non_exhaustive()
    }
}

impl LiveEyeModel {
    /// `LiveEyeModel(path)`: loads the agent (float16 on the GPU, the scorer in float32) or fails as the load did.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, EyeError> {
        Self::with_python(path, default_python(), Vec::new())
    }

    /// The same with an explicit interpreter and extra environment for the helper (tests point PYTHONPATH at
    /// fake modules).
    pub fn with_python(
        path: impl AsRef<Path>,
        python: impl Into<PathBuf>,
        env: Vec<(OsString, OsString)>,
    ) -> Result<Self, EyeError> {
        let model = Self { path: path.as_ref().to_path_buf(), python: python.into(), env, helper: Mutex::new(None) };
        let helper = model.spawn()?;
        *model.helper.lock() = Some(helper);
        Ok(model)
    }

    fn spawn(&self) -> Result<Helper, EyeError> {
        let mut cmd = Command::new(&self.python);
        cmd.arg("-c").arg(HELPER).arg(&self.path).arg(QUESTION.to_string());
        cmd.envs(self.env.iter().map(|(k, v)| (k, v)));
        // The script's own directory is not on sys.path; a neutral cwd keeps a planted module from loading.
        cmd.current_dir("/");
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
        let mut child = cmd.spawn().map_err(|e| {
            let kind = if e.kind() == std::io::ErrorKind::NotFound { "FileNotFoundError" } else { "OSError" };
            EyeError::raised(kind, format!("{}: {e}", self.python.display()))
        })?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(EyeError::raised("OSError", "laya eye helper has no pipes"));
        };
        let mut helper = Helper { child, stdin, stdout: BufReader::new(stdout) };
        let status = helper.read()?;
        match status.get("status").and_then(Value::as_str) {
            Some("ready") => Ok(helper),
            _ => Err(reported(&status)),
        }
    }
}

impl EyeModel for LiveEyeModel {
    fn predict(&self, state: &str) -> Result<EyeAnswer, EyeError> {
        let start = Instant::now();
        let mut slot = self.helper.lock();
        if slot.is_none() {
            // The helper died on an earlier turn: load again rather than fail every turn after.
            *slot = Some(self.spawn()?);
        }
        let reply = match slot.as_mut().map(|h| h.ask(state)) {
            Some(Ok(reply)) => reply,
            Some(Err(e)) => {
                *slot = None;
                return Err(e);
            }
            None => return Err(EyeError::raised("RuntimeError", "laya eye helper is not running")),
        };
        drop(slot);
        let Some(values) = reply.get("probabilities").and_then(Value::as_array) else {
            return Err(reported(&reply));
        };
        let p = values
            .iter()
            .map(|v| v.as_f64().filter(|x| x.is_finite()))
            .collect::<Option<Vec<f64>>>()
            .filter(|p| p.len() == LABELS.len())
            .ok_or_else(|| EyeError::raised("RuntimeError", "laya eye helper sent unusable probabilities"))?;
        Ok(EyeAnswer {
            label: LABELS[argmax(&p)].to_string(),
            probabilities: p,
            ms: start.elapsed().as_secs_f64() * 1000.0,
        })
    }
}
