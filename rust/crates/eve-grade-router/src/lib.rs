//! How much model an app turn gets: Jev grades the owner's message instant or think.
//!
//! Port of `eve_bridge/grade_router.py`.
//!
//! Why (2026-10-08): the owner moved to texting eve in Eve Link and asked that "jev routes request to best grade so
//! that we have instantaneous when we need it and the snappy stuff we have already built in", with thinking done by
//! gpt-6.1-sol. Measured the same day on one easy message: gpt-6-luna at low effort answered in 1.0-1.4 s,
//! gpt-6.1-sol in 2.7-3.6 s with one run at 12.9 s; on a hard money question sol took 11.8 s at medium effort and
//! 16.8 s at high. Sol on every turn would make easy chat two to three times slower; luna on every turn answers hard
//! questions without thinking. So each turn is graded:
//!
//! * `instant`: today's chat turn (the brain's model, gpt-6-luna at low effort), with every fast path before it
//!   (quick answers by code, reflexes, the fast lane).
//! * `think`: the turn runs on the think model at the think effort (EVE_APP_THINK_MODEL at
//!   EVE_APP_THINK_EFFORT, as the explicit think grade does); the model spends reasoning tokens only as the turn
//!   needs them (the probe: 0 to 20 on a trivial question).
//!
//! Quick answers by code still answer first in both grades: a grade is only asked for a turn that reaches the model.
//!
//! Jev is asked the way toolset_router.py asks it: the state is the message, the owner's previous message and one
//! line about what the text is; one absolute Noul ("must the assistant think it through ...") written as the job in
//! the owner's words, naming no wrong thing (the owner's rule for prompts). Code owns the rule: think when the Noul
//! clears the fitted gate. An error, a timeout or a missing key is `instant`: today's behaviour. It is a separate
//! call from the toolset router's, run beside it and the prompt's building, so it usually adds no wall time (when
//! the toolset router is off or keywords-only, the turn can wait up to TIMEOUT_S for it) and leaves that router's
//! fitted gates as they were.
//!
//! Fitting (tools/grade_eval.py): live Jev runs once over the tuning set (grade_tuning.json, 220 cases, writer A;
//! answers cached in grade_tuning_answers.json), and the HIGHEST gate on GRID is kept whose think recall on the
//! tuning set's think-only cases is >= MIN_TUNING_RECALL. Then live Jev runs ONCE over the blind holdout
//! (grade_holdout.json, 100 cases, writer B, sha256 committed in 4b76703 before this module existed) and is scored
//! against the bar below. Nothing is refitted after the holdout.
//!
//! PRE-REGISTERED BAR (written 2026-10-08, before any Jev answer on either set existed):
//!   (1) think recall: think-only holdout cases graded think        >= 0.90
//!   (2) over-think: instant-only holdout cases graded think        <= 0.15
//!   (3) Jev latency p95                                            <= 800 ms
//!   (4) errors and timeouts                                        <= 2% of cases
//! SHIPPED is True only if all four hold. Ambiguous cases (both acceptable) count in no gate.
//!
//! `EVE_GRADE_ROUTER` = auto (the default: on when SHIPPED) | on (a live trial before or despite the bar) |
//! off (every auto turn is instant). Jev is a third party: with it on, the owner's app message and the previous one
//! leave the Mac on every auto app turn, as they already do for the toolset router.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::{Duration, Instant};

use eve_jev::Env;
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Value};

/// `Predict = Callable[[Any, dict[str, Any]], dict[str, Any]]`: `predict(state, questions)`. An `Err` is the
/// exception the Python predict raises; see [`exception_name`] for how its class is read.
pub type Predict<'a> = &'a dyn Fn(&Value, &Value) -> anyhow::Result<Value>;

/// A grader: `grader(text, previous) -> GradeAnswer`, which may raise (an `Err`).
pub type Grader = Arc<dyn Fn(&str, &str) -> anyhow::Result<GradeAnswer> + Send + Sync>;

pub const INSTANT: &str = "instant";
pub const THINK: &str = "think";
pub const GRADES: [&str; 2] = [INSTANT, THINK];
pub const MODES: [&str; 3] = ["auto", "on", "off"];
/// The toolset router's budget: past it the turn is instant, today's behaviour.
pub const TIMEOUT_S: f64 = 0.8;
/// `tuple(round(0.05 * i, 2) for i in range(1, 20))`: 0.05 .. 0.95.
pub const GRID: [f64; 19] = [
    0.05, 0.1, 0.15, 0.2, 0.25, 0.3, 0.35, 0.4, 0.45, 0.5, 0.55, 0.6, 0.65, 0.7, 0.75, 0.8, 0.85, 0.9, 0.95,
];
pub const MIN_TUNING_RECALL: f64 = 0.95;
/// Used only until grade_gate.json exists.
pub const DEFAULT_GATE: f64 = 0.5;
/// Holdout 2026-10-08: recall 0.943, over-think 0.0, p95 482 ms, errors 0 (passed).
pub const SHIPPED: bool = true;

/// `GATE_FILE`: beside the Python module (bridge/src/eve_bridge/grade_gate.json), where tools/grade_eval.py
/// --tune writes it. Under tests/fixtures an installed eve had no file and fell back to DEFAULT_GATE without a
/// word, so the file also ships inside this binary: see [`GATE_JSON`] and [`load_shipped_gate`].
pub static GATE_FILE: LazyLock<PathBuf> = LazyLock::new(|| {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../bridge/src/eve_bridge/grade_gate.json")
});

/// The bytes of grade_gate.json at build time: the wheel's package-data copy, for a binary that runs away from
/// the checkout.
pub const GATE_JSON: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src/eve_bridge/grade_gate.json"));

pub const ABOUT: &str = "A message the owner texted their personal assistant from their phone. When a previous message is given, \
the new one may follow it: judge what the new message asks for, reading the previous one only as context.";

pub const NOUL: &str = "To answer this well, must the assistant think it through before replying: plan several steps, work out \
money, dates or schedules under several constraints, weigh options with trade-offs, debug or write code, \
write something careful for another person to read, or give advice where a quick wrong answer would cost \
the owner? No when a short reply right away is the right answer: small talk, thanks, a greeting, a quick \
fact, or a simple request to the assistant's own tools such as music, lights, a timer, a reminder, today's \
calendar or opening an app.";

// ---- Python's exceptions -----------------------------------------------------------------------------------

/// An exception Python would raise out of a function (a `ValueError` from `fit`, an `AttributeError` from `ask`
/// on a malformed reply). `kind` is `type(e).__name__`, `Display` is `str(e)`.
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

/// `type(e).__name__` for an error a predict or a grader returned: the Python class it stands for when it says
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

// ---- small Python helpers ----------------------------------------------------------------------------------

/// Python's `round(x, digits)` on a float: the nearest decimal, ties to even (Rust's formatter rounds the exact
/// binary value the same way).
fn py_round(x: f64, digits: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.digits$}").parse().unwrap_or(x)
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

/// `float(v)` for a decoded JSON value.
fn py_float(v: &Value) -> Result<f64, Raised> {
    match v {
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64().ok_or_else(|| Raised::new("OverflowError", "int too large to convert to float")),
        Value::String(s) => {
            // float() strips ASCII and Unicode whitespace but not \x1c-\x1f (str.strip() would): `str::trim`.
            let t = s.trim();
            // Python allows `_` only between two digits ("1_000", never "1__0", "_1", "1_" or "1_.5").
            let b = t.as_bytes();
            let digits_ok = b.iter().enumerate().all(|(i, c)| {
                *c != b'_' || (i > 0 && i + 1 < b.len() && b[i - 1].is_ascii_digit() && b[i + 1].is_ascii_digit())
            });
            let cleaned = t.replace('_', "");
            match cleaned.parse::<f64>() {
                Ok(f) if digits_ok && !t.is_empty() => Ok(f),
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

/// `str(v)` for a decoded JSON scalar (containers print as JSON).
fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::String(s) => s.clone(),
        Value::Number(n) => {
            if n.is_f64() {
                let f = n.as_f64().unwrap_or(0.0);
                if f.fract() == 0.0 && f.abs() < 1e16 {
                    format!("{f:.1}")
                } else {
                    f.to_string()
                }
            } else {
                n.to_string()
            }
        }
        other => other.to_string(),
    }
}

/// What Python's `str.split()` / `str.strip()` treat as whitespace: Unicode whitespace and the ASCII separators
/// \x1c-\x1f.
fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `" ".join(s.split())`.
fn collapse_ws(s: &str) -> String {
    s.split(is_py_space)
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `s[:n]` in characters.
fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `zip(a, b, strict=True)`'s length check.
fn zip_strict(a: usize, b: usize) -> Result<(), Raised> {
    if b < a {
        return Err(Raised::new("ValueError", "zip() argument 2 is shorter than argument 1"));
    }
    if b > a {
        return Err(Raised::new("ValueError", "zip() argument 2 is longer than argument 1"));
    }
    Ok(())
}

fn is_only(ok: &[impl AsRef<str>], grade: &str) -> bool {
    ok.len() == 1 && ok[0].as_ref() == grade
}

static PERF_ORIGIN: LazyLock<Instant> = LazyLock::new(Instant::now);

/// `time.perf_counter()`: seconds on a monotonic clock.
pub fn perf_counter() -> f64 {
    PERF_ORIGIN.elapsed().as_secs_f64()
}

// ---- the question and the state ----------------------------------------------------------------------------

pub fn questions() -> Value {
    json!({THINK: {"type": "noul", "instructions": NOUL}})
}

pub fn state(text: &str, previous: &str) -> Value {
    json!({
        "about": ABOUT,
        "previous": clip(&collapse_ws(previous), 500),
        "request": clip(&collapse_ws(text), 500),
    })
}

// ---- Jev's answer ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Default)]
pub struct GradeAnswer {
    /// Jev's Noul for THINK; 0.0 when absent.
    pub noul: f64,
    pub ms: f64,
    pub error: String,
}

impl GradeAnswer {
    pub fn new(noul: f64, ms: f64) -> Self {
        Self { noul, ms, error: String::new() }
    }

    pub fn failed(noul: f64, ms: f64, error: impl Into<String>) -> Self {
        Self { noul, ms, error: error.into() }
    }

    pub fn to_json(&self) -> Value {
        json!({"noul": py_round(self.noul, 4), "ms": py_round(self.ms, 1), "error": self.error})
    }

    /// `GradeAnswer.from_json(raw)`: `float(raw.get("noul") or 0.0)`, the same for `ms`, `str(raw.get("error")
    /// or "")`. A value `float()` refuses is the `ValueError`/`TypeError` Python raises.
    pub fn from_json(raw: &Value) -> Result<GradeAnswer, Raised> {
        if !raw.is_object() {
            return Err(Raised::new("AttributeError", format!("'{}' object has no attribute 'get'", py_type_name(raw))));
        }
        let field = |k: &str| raw.get(k).filter(|v| truthy(v));
        let noul = field("noul").map(py_float).transpose()?.unwrap_or(0.0);
        let ms = field("ms").map(py_float).transpose()?.unwrap_or(0.0);
        let error = field("error").map(py_str).unwrap_or_default();
        Ok(GradeAnswer { noul, ms, error })
    }
}

/// Jev's Noul for one message. A failure is an answer with `error` set: it grades instant.
///
/// `Err` is what Python lets escape `ask`: a reply whose `answers` is a truthy non-object
/// (`AttributeError: '<type>' object has no attribute 'get'`, raised outside the `try`).
pub fn ask(predict: Predict<'_>, text: &str, previous: &str, clock: &mut dyn FnMut() -> f64) -> Result<GradeAnswer, Raised> {
    let t0 = clock();
    // A model that fails abstains: the turn is instant, as before.
    let reply = predict(&state(text, previous), &questions());
    let answers = match reply {
        Ok(Value::Object(body)) => body.get("answers").filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!({})),
        Ok(other) => {
            let e = format!("AttributeError: '{}' object has no attribute 'get'", py_type_name(&other));
            return Ok(GradeAnswer::failed(0.0, (clock() - t0) * 1000.0, clip(&e, 160)));
        }
        Err(e) => {
            let msg = format!("{}: {}", exception_name(&e), e);
            return Ok(GradeAnswer::failed(0.0, (clock() - t0) * 1000.0, clip(&msg, 160)));
        }
    };
    let Value::Object(answers) = answers else {
        return Err(Raised::new("AttributeError", format!("'{}' object has no attribute 'get'", py_type_name(&answers))));
    };
    let noul = match answers.get(THINK) {
        Some(Value::Object(a)) => match a.get("noul") {
            Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
            _ => 0.0,
        },
        _ => 0.0,
    };
    Ok(GradeAnswer::new(noul, (clock() - t0) * 1000.0))
}

/// THINK when the Noul clears the gate; INSTANT on an error or an answer slower than the turn waits.
/// Python's `budget_ms` defaults to infinity: pass `f64::INFINITY`.
pub fn decide(a: &GradeAnswer, gate: f64, budget_ms: f64) -> &'static str {
    if !a.error.is_empty() || a.ms > budget_ms {
        return INSTANT;
    }
    if a.noul >= gate {
        THINK
    } else {
        INSTANT
    }
}

/// (gate, tuning think recall): the highest GRID gate whose recall on think-only cases is >= MIN_TUNING_RECALL;
/// the lowest GRID gate when none reaches it (then the recall says by how much it missed).
pub fn fit<S: AsRef<str>>(answers: &[GradeAnswer], acceptable: &[Vec<S>]) -> Result<(f64, f64), Raised> {
    zip_strict(answers.len(), acceptable.len())?;
    let think_only: Vec<&GradeAnswer> =
        answers.iter().zip(acceptable).filter(|(_, ok)| is_only(ok, THINK)).map(|(a, _)| a).collect();
    if think_only.is_empty() {
        return Err(Raised::new("ValueError", "the tuning set has no think-only case"));
    }
    let recall = |gate: f64| {
        think_only.iter().filter(|a| decide(a, gate, f64::INFINITY) == THINK).count() as f64 / think_only.len() as f64
    };
    let mut best = (GRID[0], 0.0);
    for gate in GRID {
        let rec = recall(gate);
        if rec >= MIN_TUNING_RECALL {
            best = (gate, rec);
        }
    }
    if best.1 == 0.0 {
        best = (GRID[0], recall(GRID[0]));
    }
    Ok(best)
}

/// The pre-registered bar's verdict: the dict `bar()` returns, keys in the same order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Bar {
    pub n: usize,
    pub think_only: usize,
    pub instant_only: usize,
    pub recall: f64,
    /// `"1"`: recall >= 0.90.
    #[serde(rename = "1")]
    pub recall_ok: bool,
    pub over_think: f64,
    /// `"2"`: over-think <= 0.15.
    #[serde(rename = "2")]
    pub over_think_ok: bool,
    pub p95_ms: f64,
    /// `"3"`: p95 <= 800 ms.
    #[serde(rename = "3")]
    pub p95_ok: bool,
    pub errors: f64,
    /// `"4"`: errors <= 2%.
    #[serde(rename = "4")]
    pub errors_ok: bool,
    pub shipped: bool,
}

impl Bar {
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// The pre-registered bar, exactly (the module's text).
pub fn bar<G: AsRef<str>, S: AsRef<str>>(
    grades: &[G],
    acceptable: &[Vec<S>],
    answers: &[GradeAnswer],
) -> Result<Bar, Raised> {
    zip_strict(grades.len(), acceptable.len())?;
    let pairs = || grades.iter().zip(acceptable);
    let think_only: Vec<&str> = pairs().filter(|(_, ok)| is_only(ok, THINK)).map(|(g, _)| g.as_ref()).collect();
    let instant_only: Vec<&str> = pairs().filter(|(_, ok)| is_only(ok, INSTANT)).map(|(g, _)| g.as_ref()).collect();
    let share = |gs: &[&str]| gs.iter().filter(|g| **g == THINK).count() as f64 / gs.len() as f64;
    let recall = if think_only.is_empty() { 0.0 } else { share(&think_only) };
    let over = if instant_only.is_empty() { 1.0 } else { share(&instant_only) };
    let mut ms: Vec<f64> = answers.iter().filter(|a| a.error.is_empty()).map(|a| a.ms).collect();
    ms.sort_by(f64::total_cmp);
    let p95 = if ms.is_empty() {
        f64::INFINITY
    } else {
        ms[(ms.len() - 1).min((0.95 * ms.len() as f64) as usize)]
    };
    let errors = if answers.is_empty() {
        1.0
    } else {
        answers.iter().filter(|a| !a.error.is_empty()).count() as f64 / answers.len() as f64
    };
    let mut out = Bar {
        n: grades.len(),
        think_only: think_only.len(),
        instant_only: instant_only.len(),
        recall: py_round(recall, 3),
        recall_ok: recall >= 0.90,
        over_think: py_round(over, 3),
        over_think_ok: over <= 0.15,
        p95_ms: py_round(p95, 1),
        p95_ok: p95 <= 800.0,
        errors: py_round(errors, 3),
        errors_ok: errors <= 0.02,
        shipped: false,
    };
    out.shipped = out.recall_ok && out.over_think_ok && out.p95_ok && out.errors_ok;
    Ok(out)
}

// ---- the gate ----------------------------------------------------------------------------------------------

/// `float(json.loads(path.read_text())["gate"])`, or DEFAULT_GATE on any failure.
pub fn load_gate(path: &Path) -> f64 {
    std::fs::read_to_string(path).ok().and_then(|t| gate_of(&t)).unwrap_or(DEFAULT_GATE)
}

fn gate_of(text: &str) -> Option<f64> {
    let v: Value = serde_json::from_str(text).ok()?;
    let g = v.as_object()?.get("gate")?;
    py_float(g).ok()
}

/// The gate `grade_turn` uses: `load_gate(GATE_FILE)` when the file is there (a refit in the checkout is seen
/// on the next start, as the editable Python install sees it), else the copy built in ([`GATE_JSON`]), as the
/// wheel's package-data copy is read; DEFAULT_GATE only when neither parses.
pub fn load_shipped_gate() -> f64 {
    if GATE_FILE.exists() {
        return load_gate(&GATE_FILE);
    }
    gate_of(GATE_JSON).unwrap_or(DEFAULT_GATE)
}

/// `_GATE`: the shipped gate, read once a process by `grade_turn`.
#[derive(Debug, Default)]
pub struct GateCache(OnceLock<f64>);

impl GateCache {
    pub const fn new() -> Self {
        Self(OnceLock::new())
    }

    /// The cached gate, loading it with `load` the first time only.
    pub fn get_or_load(&self, load: &dyn Fn() -> f64) -> f64 {
        *self.0.get_or_init(load)
    }
}

pub static GATE: GateCache = GateCache::new();

// ---- the mode ----------------------------------------------------------------------------------------------

/// `EVE_GRADE_ROUTER`: one of MODES, `auto` when unset, empty or unknown (with a warning).
pub fn mode<E: Env + ?Sized>(env: &E) -> &'static str {
    let raw = env.var("EVE_GRADE_ROUTER").filter(|v| !v.is_empty()).unwrap_or_else(|| "auto".to_string());
    let raw = raw.trim_matches(is_py_space).to_lowercase();
    if let Some(m) = MODES.iter().find(|m| **m == raw) {
        return m;
    }
    let modes: Vec<String> = MODES.iter().map(|m| eve_decider::py_str_repr(m)).collect();
    tracing::warn!(
        "grade router: EVE_GRADE_ROUTER={} is not one of ({}); auto",
        eve_decider::py_str_repr(&raw),
        modes.join(", ")
    );
    "auto"
}

// ---- the grade of one turn ---------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Grade {
    pub grade: &'static str,
    pub noul: f64,
    pub ms: f64,
    pub asked: bool,
    pub error: String,
}

impl Default for Grade {
    fn default() -> Self {
        Self { grade: INSTANT, noul: 0.0, ms: 0.0, asked: false, error: String::new() }
    }
}

impl Grade {
    pub fn new(grade: &'static str, noul: f64, ms: f64, asked: bool, error: impl Into<String>) -> Self {
        Self { grade, noul, ms, asked, error: error.into() }
    }

    /// Numbers only, never the message.
    pub fn log_line(&self) -> String {
        if !self.asked {
            return format!("{}; jev off", self.grade);
        }
        let mut line = format!("{} (noul {:.2}); jev {:.0} ms", self.grade, self.noul, self.ms);
        if !self.error.is_empty() {
            line.push_str(&format!(" ({})", self.error));
        }
        line
    }
}

struct JevCache {
    url: String,
    model: String,
    grader: Grader,
}

static JEV: LazyLock<Mutex<Option<JevCache>>> = LazyLock::new(|| Mutex::new(None));

/// Jev over EVE_JEV_ROUTE, made once a process, or None (off, auto while not SHIPPED, or no Jev route).
/// Pass `eve_jev::ProcessEnv` for `os.environ`.
pub fn make_grader<E: Env + ?Sized>(env: &E) -> Option<Grader> {
    let m = mode(env);
    if m == "off" || (m == "auto" && !SHIPPED) {
        return None;
    }
    let (url, key, model) = eve_jev::route_config(env, "").ok()?;
    let mut cache = JEV.lock();
    if let Some(c) = cache.as_ref() {
        if c.url == url && c.model == model {
            return Some(c.grader.clone());
        }
    }
    let predict = eve_jev::make_predict(
        &url,
        &key,
        &model,
        eve_jev::PredictOptions { timeout_s: TIMEOUT_S, ..Default::default() },
    );
    let call = predict.anyhow_fn();
    let grader: Grader = Arc::new(move |text: &str, previous: &str| {
        ask(&call, text, previous, &mut perf_counter).map_err(anyhow::Error::from)
    });
    *cache = Some(JevCache { url, model, grader: grader.clone() });
    Some(grader)
}

/// The grade of one auto turn. Blocking (call it with `tokio::task::spawn_blocking`). Jev runs in a worker waited
/// on for `timeout_s` at most; a grader that is None, raises, errs or is slower grades instant, today's
/// behaviour. Python's defaults: `previous` "", `gate` None (the shipped gate, read once), `timeout_s` TIMEOUT_S.
pub fn grade_turn(text: &str, previous: &str, grader: Option<&Grader>, gate: Option<f64>, timeout_s: f64) -> Grade {
    grade_turn_in(&GATE, &load_shipped_gate, text, previous, grader, gate, timeout_s)
}

/// [`grade_turn`] with the gate cache and its loader as seams (the Python tests patch `_GATE` and `load_gate`).
pub fn grade_turn_in(
    cache: &GateCache,
    load: &dyn Fn() -> f64,
    text: &str,
    previous: &str,
    grader: Option<&Grader>,
    gate: Option<f64>,
    timeout_s: f64,
) -> Grade {
    let Some(grader) = grader else {
        return Grade::default();
    };
    let (tx, rx) = mpsc::channel::<GradeAnswer>();
    let t0 = Instant::now();
    let elapsed_ms = || t0.elapsed().as_secs_f64() * 1000.0;
    let (g, text_owned, previous_owned) = (grader.clone(), text.to_string(), previous.to_string());
    // A panicking grader drops the sender without an answer: that reads as "not an answer" below.
    let spawned = std::thread::Builder::new().name("grade-router".into()).spawn(move || {
        let a = match g(&text_owned, &previous_owned) {
            Ok(a) => a,
            Err(e) => GradeAnswer::failed(0.0, 0.0, exception_name(&e)),
        };
        let _ = tx.send(a);
    });
    if spawned.is_err() {
        return Grade::new(INSTANT, 0.0, elapsed_ms(), true, "not an answer");
    }
    let got = match Duration::try_from_secs_f64(timeout_s.max(0.0)) {
        Ok(d) => rx.recv_timeout(d),
        Err(_) => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
    };
    let a = match got {
        Ok(a) => a,
        Err(RecvTimeoutError::Timeout) => return Grade::new(INSTANT, 0.0, elapsed_ms(), true, "timeout"),
        Err(RecvTimeoutError::Disconnected) => return Grade::new(INSTANT, 0.0, elapsed_ms(), true, "not an answer"),
    };
    // Once: the file only changes when the gate is refitted.
    let gate = gate.unwrap_or_else(|| cache.get_or_load(load));
    let g = decide(&a, gate, f64::INFINITY);
    Grade::new(g, a.noul, a.ms, true, a.error)
}

#[cfg(test)]
mod tests;
