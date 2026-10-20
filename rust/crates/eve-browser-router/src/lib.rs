//! Port of `browser_router.py`: which body carries a task that is not a reflex: the owner's computer (Codex,
//! or the Chrome lane) or the web reader.
//!
//! The web reader (web_reader.py, through TinyFish) is a hosted web-reading service: it searches the public web
//! and reads public pages on its own side, and eve reports what it found as text. It has none of the owner's
//! accounts, cannot reach the Mac or a local address, and as eve uses it never clicks or types. For a lookup on
//! the public web it is seconds where driving a browser is tens of seconds, and it never takes over the owner's
//! screen. (The body before it was an isolated browser, retired 2026-09-23 before it was ever wired in.)
//!
//! Jev decides, asked the way TypeSafe documents for a routing decision (docs.typesafe.ai: State, Choice
//! "Structured instructions and criteria", Intent routing, Jev 1.13 jaggedness), and the way typed_ask asks it
//! everywhere else:
//!
//! * The state is the request and nothing else — no tool manuals in the state ("context rot").
//! * The two bodies are the options of one Choice, each described as structure: `what` it is, `for`, and
//!   `not_for` — the facts from each tool's own docs, the boundary written into both sides so a literal reader
//!   cannot confuse them. The wording names no provider, so the service behind the web reader can change
//!   without a refit.
//! * Beside the Choice, absolute Nouls, one judgment each (Jev reads literally; "a Choice is relative, each
//!   Noul is absolute"): does the request need the owner's own accounts or data? anything on the Mac or a local
//!   address? something shown on the owner's screen? clicking, typing or a form? and is the whole job reading
//!   public web pages, reported back as text?
//! * Code combines them and owns the safety rule: the web reader only when the job is reading the public web,
//!   needs none of the owner's accounts, nothing on the Mac, nothing on screen and no clicking or typing, AND
//!   the Choice picks it with enough weight. An unsafe route is a task that needs the owner (their accounts,
//!   their data, their Mac) sent to the web reader: it would fail, and its request text would have gone to a
//!   third party. Every cut-off is fitted with zero unsafe routes allowed — tools/route_eval.py --browser, on a
//!   blind holdout.

use std::cmp::Ordering;
use std::sync::{Arc, LazyLock};

use serde::Serialize;
use serde_json::{json, Map, Value};

pub use eve_decider::PredictError;

#[cfg(test)]
mod tests;

/// `Predict = Callable[[Any, dict[str, Any]], dict[str, Any]]`: `predict(state, questions)`. An `Err` is the
/// exception the Python predict raised (`kind` is its class name).
pub type Predict = eve_decider::Predict;
/// `Callable[[], float]`: a clock in seconds (`time.perf_counter` in production).
pub type Clock = dyn Fn() -> f64 + Send + Sync;

pub const CODEX: &str = "codex";
pub const WEB: &str = "web";
/// The name the fitting and eval code use for the non-Codex body.
pub const AUTO: &str = WEB;

/// The two bodies, as the Choice's options. Facts only: the web reader's from its service's docs (search, then
/// read public pages on its side, text back) and web_reader.py (no clicks, no forms, no owner accounts); the
/// owner's computer from docs/codex-computer-use/README.md and chrome_lane.py.
pub static BODIES: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "owner_computer": {
            "what": "an agent that operates the owner's own Mac and the owner's real Chrome browser, already signed \
                     in to the owner's accounts, clicking and typing on the owner's screen",
            "for": ["anything in the owner's own accounts: email, calendar, messages, orders, bookings, banking, \
                     social media, work tools", "anything in a Mac app, a file on the Mac, or a local or staging \
                     website", "clicking through a website, typing into it, filling or submitting a form, or \
                     testing that a website works", "a lookup that needs a number or detail typed into a website, \
                     such as a tracking or confirmation number", "playing music or video for the owner"],
            "not_for": ["only reading public web pages and reporting back what they say"],
        },
        "web_reader": {
            "what": "a web-reading service that searches the public web and reads public web pages in its own \
                     browsers, with no accounts, no cookies and no access to the Mac, and reports what it found \
                     back as text; it does not click, type or fill in anything",
            "for": ["looking up a fact, opening hours, a phone number, a price, a definition or a forecast on \
                     public websites", "comparing products, prices, plans or specifications across public pages",
                    "reading a public article, review, status page or documentation page and summarizing it",
                    "checking whether a product is listed as in stock on a public store page",
                    "collecting information from several public pages into a short answer"],
            "not_for": ["anything that needs the owner signed in, or the owner's own data, numbers or places",
                        "clicking through a site, typing into it, filling in or submitting a form, or testing a site",
                        "anything on the Mac or on a local or private address",
                        "anything the owner wants opened, shown or played on their own screen"],
        },
    })
});

/// `OPTION_TO_BODY`, in Python's dict order: the Choice's option id and the body it names.
pub const OPTION_TO_BODY: [(&str, &str); 2] = [("owner_computer", CODEX), ("web_reader", WEB)];

/// The questions Jev answers for one request: one Choice between the two bodies and five absolute Nouls.
pub fn questions() -> Value {
    json!({
        "body": {"type": "choice",
                 "instructions": "Which of the two should carry out the owner's request?",
                 "criteria": &*BODIES},
        "needs_accounts": {"type": "noul", "instructions":
            "Does the request need the owner to be signed in to one of their own accounts or services (their email, \
             calendar, messages, orders, bookings, bank, social media, subscriptions or work tools), or use \
             something only the owner has or knows (their own numbers, their address, 'my' places or things), or \
             does it refer to something already open in the owner's browser?"},
        "needs_mac": {"type": "noul", "instructions":
            "Does the request involve anything outside public websites: an application on the Mac, a file or \
             folder on the Mac, the desktop or system settings, a local, private or staging web address, or playing \
             sound or video for the owner to watch or hear?"},
        "show_owner": {"type": "noul", "instructions":
            "Does the owner want something opened, shown or played on their own screen so they can look at it, read \
             it or watch it themselves, rather than a result reported back to them?"},
        "needs_interaction": {"type": "noul", "instructions":
            "Does the request need something put INTO a website: a code, number, date, zip code or name typed into \
             it, a form filled in or submitted, a sign-up or booking, clicking through a site's steps to reach a \
             result, or testing whether a website works? Answer no for looking things up, searching the web, \
             reading, checking or comparing what public pages already show: those put nothing into a site."},
        "public_read_job": {"type": "noul", "instructions":
            "Is the whole job finding something out from public websites and telling the owner: looking up a fact, \
             opening hours, a phone number, a price, a score, a forecast, a definition, a policy or reviews; \
             checking what a public page says; comparing what several public pages say; or reading and summarizing \
             a public page, with the answer reported back as text?"},
    })
}

/// What Jev said about one request.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BodyAnswer {
    /// The Choice's probability for the web_reader option.
    pub p_auto: f64,
    pub accounts: f64,
    pub mac: f64,
    pub public: f64,
    pub show: f64,
    pub interact: f64,
    pub ms: f64,
    pub error: String,
}

impl BodyAnswer {
    /// `BodyAnswer(p_auto, accounts, mac, public, show, interact)` with `ms` 0.0 and no error.
    pub fn new(p_auto: f64, accounts: f64, mac: f64, public: f64, show: f64, interact: f64) -> Self {
        Self { p_auto, accounts, mac, public, show, interact, ms: 0.0, error: String::new() }
    }
}

/// The cut-offs `decide` applies. `Default` is the dataclass's defaults.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct BodyGates {
    pub public: f64,
    pub accounts_max: f64,
    pub mac_max: f64,
    pub auto: f64,
    pub show_max: f64,
    pub interact_max: f64,
}

impl BodyGates {
    /// Positional, in the dataclass's field order.
    pub const fn new(public: f64, accounts_max: f64, mac_max: f64, auto: f64, show_max: f64, interact_max: f64) -> Self {
        Self { public, accounts_max, mac_max, auto, show_max, interact_max }
    }
}

impl Default for BodyGates {
    fn default() -> Self {
        Self::new(0.9, 0.1, 0.1, 0.9, 0.5, 0.5)
    }
}

/// An exception Python's `ask` (and so the router) lets escape: reading a reply whose answers are not shaped
/// as dicts raises `AttributeError` outside the try. Callers treat it as a broken router (Codex).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct RouterError {
    /// The Python exception class.
    pub kind: &'static str,
    pub message: String,
}

/// Python's `type(v).__name__` for a decoded JSON value.
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

/// Python truthiness of a decoded JSON value.
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

static EMPTY: LazyLock<Map<String, Value>> = LazyLock::new(Map::new);

/// `(x or {})` followed by `.get`: the dict to read, or the `AttributeError` Python raises on a truthy non-dict.
fn or_empty(v: Option<&Value>) -> Result<&Map<String, Value>, RouterError> {
    match v {
        Some(Value::Object(o)) => Ok(o),
        Some(other) if truthy(other) => Err(RouterError {
            kind: "AttributeError",
            message: format!("'{}' object has no attribute 'get'", py_type_name(other)),
        }),
        _ => Ok(&EMPTY),
    }
}

/// Python's `str.isspace` (what `str.split()` splits on): Unicode White_Space plus the separators 0x1C–0x1F.
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// The first `n` characters, as Python's `s[:n]`.
fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Ask Jev about one request. A predict that fails abstains (Codex); a reply whose answers are not dicts raises
/// [`RouterError`], as Python's `ask` does.
pub fn ask(predict: &Predict, goal: &str, clock: &Clock) -> Result<BodyAnswer, RouterError> {
    let t0 = clock();
    let request: Vec<&str> = goal.split(py_space).filter(|w| !w.is_empty()).collect();
    let state = json!({"request": request.join(" ")});
    // A model that fails abstains: Codex, as before.
    let failed = |kind: &str, message: &str| -> BodyAnswer {
        BodyAnswer {
            p_auto: 0.0,
            accounts: 1.0,
            mac: 1.0,
            public: 0.0,
            show: 1.0,
            interact: 1.0,
            ms: (clock() - t0) * 1000.0,
            error: head(&format!("{kind}: {message}"), 160),
        }
    };
    let result = match predict(&state, &questions()) {
        Ok(r) => r,
        Err(e) => return Ok(failed(&e.kind, &e.message)),
    };
    // `result.get("answers") or {}` runs inside the try: a reply that is not a dict is caught there.
    let answers = match &result {
        Value::Object(o) => o.get("answers").filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!({})),
        other => {
            return Ok(failed("AttributeError", &format!("'{}' object has no attribute 'get'", py_type_name(other))))
        }
    };

    // An answer the model left out (or gave as a non-number) fails closed: the safety gates read it as "yes"
    // (1.0) and the public-job gate as "no" (0.0), so a half-answered private request stays with Codex.
    let noul = |key: &str, missing: f64| -> Result<f64, RouterError> {
        let entry = or_empty(or_empty(Some(&answers))?.get(key))?;
        Ok(match entry.get("noul") {
            Some(Value::Number(n)) => n.as_f64().unwrap_or(missing),
            _ => missing,
        })
    };

    let body = or_empty(or_empty(Some(&answers))?.get("body"))?;
    let probs = or_empty(body.get("probabilities"))?;
    // isinstance(p, (int, float)) takes a bool too: float(True) is 1.0.
    let p_auto = match probs.get("web_reader") {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        _ => 0.0,
    };
    let accounts = noul("needs_accounts", 1.0)?;
    let mac = noul("needs_mac", 1.0)?;
    let public = noul("public_read_job", 0.0)?;
    let show = noul("show_owner", 1.0)?;
    let interact = noul("needs_interaction", 1.0)?;
    Ok(BodyAnswer { p_auto, accounts, mac, public, show, interact, ms: (clock() - t0) * 1000.0, error: String::new() })
}

/// WEB only when every condition holds; anything else, including an error, is CODEX.
pub fn decide(a: &BodyAnswer, g: &BodyGates) -> &'static str {
    if !a.error.is_empty() {
        return CODEX;
    }
    if a.accounts > g.accounts_max || a.mac > g.mac_max || a.show > g.show_max || a.interact > g.interact_max {
        return CODEX;
    }
    if a.public >= g.public && a.p_auto >= g.auto {
        return WEB;
    }
    CODEX
}

pub const GRID: [f64; 6] = [0.5, 0.6, 0.7, 0.8, 0.9, 0.95];
pub const MAX_GRID: [f64; 5] = [0.05, 0.1, 0.2, 0.3, 0.5];

/// The `ValueError` Python's `zip(..., strict=True)` raises when `fit` gets more answers than truths or fewer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct FitError(pub String);

fn zip_error(answers: usize, truths: usize) -> FitError {
    let side = if truths < answers { "shorter" } else { "longer" };
    FitError(format!("zip() argument 2 is {side} than argument 1"))
}

fn cmp_key(a: &[f64; 7], b: &[f64; 7]) -> Ordering {
    for (x, y) in a.iter().zip(b) {
        match x.partial_cmp(y) {
            Some(Ordering::Equal) | None => continue,
            Some(o) => return o,
        }
    }
    Ordering::Equal
}

/// The cut-offs that route the most WEB-labelled tasks there with ZERO CODEX-labelled tasks sent to the web
/// reader. Ties go to the stricter setting.
pub fn fit(answers: &[BodyAnswer], truths: &[&str]) -> Result<BodyGates, FitError> {
    let mut best: Option<[f64; 7]> = None;
    let mut chosen = BodyGates::new(1.01, 0.0, 0.0, 1.01, 0.0, 0.0);
    let n = answers.len().min(truths.len());
    for public in GRID {
        for auto in GRID {
            for acc in MAX_GRID {
                for mac in MAX_GRID {
                    for show in MAX_GRID {
                        for inter in MAX_GRID {
                            let g = BodyGates::new(public, acc, mac, auto, show, inter);
                            let said: Vec<&str> = answers.iter().map(|a| decide(a, &g)).collect();
                            // any() stops at the first unsafe route, before a strict zip would notice a length
                            // mismatch; past it, the mismatch raises.
                            if (0..n).any(|i| said[i] == AUTO && truths[i] != AUTO) {
                                continue;
                            }
                            if answers.len() != truths.len() {
                                return Err(zip_error(answers.len(), truths.len()));
                            }
                            let right = (0..n).filter(|&i| said[i] == AUTO && truths[i] == AUTO).count();
                            let key = [right as f64, public, auto, -acc, -mac, -show, -inter];
                            if best.as_ref().is_none_or(|b| cmp_key(&key, b) == Ordering::Greater) {
                                best = Some(key);
                                chosen = g;
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(chosen)
}

/// Fitted 2026-09-26 by tools/route_eval.py --browser on the 122 requests already seen (browser_tuning.json and
/// browser_holdout.json, whose one scoring with the first wording held at 12 routed: too few), zero unsafe
/// allowed; then scored ONCE on the blind browser_holdout2.json (79 requests, written by an agent that never saw
/// this module): 30 routed to the web reader, 30 right (precision 100%), coverage 93.8%, 0 unsafe, Jev p50
/// 198 ms. Refit and re-score on a NEW blind set whenever the wording above or the model changes (an alias's
/// answers can change).
pub const GATES: BodyGates =
    BodyGates { public: 0.6, accounts_max: 0.1, mac_max: 0.1, auto: 0.95, show_max: 0.5, interact_max: 0.2 };

/// Whether GATES passed the blind holdout's bar (at least 15 routed, precision >= 95%, zero unsafe).
/// web_reader.configured's "auto" turns the web body on only when this is true.
pub const SHIPPED: bool = true;

/// `make_router(...)`'s `route(goal)`: one Jev question per goal, decided by code against the gates.
#[derive(Clone)]
pub struct Router {
    predict: Arc<Predict>,
    clock: Arc<Clock>,
    gates: BodyGates,
}

impl std::fmt::Debug for Router {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Router").field("gates", &self.gates).finish_non_exhaustive()
    }
}

impl Router {
    /// The body for `goal`: [`WEB`] or [`CODEX`]. An `Err` is the exception Python's `route` raises.
    pub fn route(&self, goal: &str) -> Result<&'static str, RouterError> {
        Ok(decide(&ask(&*self.predict, goal, &*self.clock)?, &self.gates))
    }

    pub fn gates(&self) -> BodyGates {
        self.gates
    }
}

/// A router over `predict` and `clock`; `gates` is [`GATES`] unless a fit says otherwise.
pub fn make_router(predict: Arc<Predict>, clock: Arc<Clock>, gates: BodyGates) -> Router {
    Router { predict, clock, gates }
}

/// Jev over EVE_JEV_ROUTE, or None (no key: every task stays with the owner's computer). Pass
/// `eve_jev::ProcessEnv` for Python's default `os.environ`.
pub fn jev_router<E: eve_jev::Env + ?Sized>(environ: &E) -> Option<Router> {
    let predict = jev_predict(environ)?;
    Some(make_router(Arc::new(predict.decider_fn()), Arc::new(eve_decider::perf_counter), GATES))
}

/// The predict `jev_router` asks: the configured route, with a 2 s budget.
fn jev_predict<E: eve_jev::Env + ?Sized>(environ: &E) -> Option<eve_jev::JevPredict> {
    let (url, key, model) = eve_jev::route_config(environ, "").ok()?;
    Some(eve_jev::make_predict(
        &url,
        &key,
        &model,
        eve_jev::PredictOptions { timeout_s: 2.0, ..eve_jev::PredictOptions::default() },
    ))
}
