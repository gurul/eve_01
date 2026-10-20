//! The desk lane: keyword-decided clicks on the frontmost window, in front of Codex (port of `desk_lane.py`).
//!
//! Every desktop task went to Codex's Computer Use: a model turn, then another, tens of seconds, for "switch to
//! week view" too. The fast lane that clicks such a request in about a second (fast_lane, lane_router) had been
//! wired only under the old planner, which Codex replaced. This puts lane_router::route back in the path, in
//! front of Codex, through app_reflex's ReflexFirstAgent:
//!
//! ```text
//! complete  every clause was clicked and seen to change something → eve says so; Codex never starts
//! partial   something was clicked but not all of it, or not verified → Codex runs the task, told what was
//!           clicked, so it looks before it acts and never clicks the same thing blind (an unknown effect is
//!           never repeated: computah's rule)
//! none / refused / unavailable → Codex runs the task exactly as before; the lane cost one AX snapshot
//! ```
//!
//! The router is the measured one (lane_router, fast_lane::LANE_FIRST_DEFAULT): keyword gate only, every word of
//! the request accounted for by one control's label, a question / another app / a long request refused before
//! any snapshot, every sensitive label and dialog a stop with zero input. On holdout it engaged on 23 of 82 cases
//! and was right on all 23, with no sensitive click. It never types and never presses a key.
//!
//! The lane runs in the daemon (no worker process to start), on a blocking thread, one task at a time. It needs
//! the daemon's Accessibility access; without it the snapshot is empty and every request goes to Codex, said once
//! in the log. `EVE_DESK_LANE=0` turns it off.

pub mod pyautogui;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use eve_decisions::Evaluate;
#[cfg(target_os = "macos")]
use eve_desktop_lanes::desktop_helpers::Backends;
use eve_desktop_lanes::desktop_helpers::Helpers;
use eve_desktop_lanes::{error_kind, error_message};
use parking_lot::Mutex;
use serde_json::{Map, Value};

pub const ENV: &str = "EVE_DESK_LANE";
pub const DEFAULT: bool = true;
pub const MAX_GOAL_CHARS: usize = 400;

/// `EVE_DESK_LANE`: 0/false/no/off turns the lane off, 1/true/yes/on on (trimmed, any case); anything else is
/// [`DEFAULT`]. `environ` `None` reads the process environment.
pub fn enabled(environ: Option<&HashMap<String, String>>) -> bool {
    let raw = match environ {
        Some(env) => env.get(ENV).cloned().unwrap_or_default(),
        None => std::env::var_os(ENV).map(|v| v.to_string_lossy().into_owned()).unwrap_or_default(),
    };
    match raw.trim().to_lowercase().as_str() {
        "0" | "false" | "no" | "off" => false,
        "1" | "true" | "yes" | "on" => true,
        _ => DEFAULT,
    }
}

/// `helpers.bind`'s log callable: one short sentence per call.
pub type LogFn = Box<dyn Fn(&str) + Send>;

/// What the lane needs of `desktop_helpers.Helpers` (the tests give a fake, as Python's `FakeHelpers` did).
pub trait DeskHelpers: Send {
    /// `helpers.bind(log, lambda _image: None)`: one short sentence per call to `log`, images dropped.
    fn bind(&mut self, log: LogFn);
    /// `helpers.begin()`.
    fn begin(&mut self);
    /// `helpers.lane_first(goal)`: lane_router.RouteResult as a dict.
    fn lane_first(&mut self, goal: &str) -> Value;
    /// `helpers.decisions_first(goal, client)`: the opt-in visual loop's result as a dict.
    fn decisions_first(&mut self, goal: &str, client: &mut dyn Evaluate) -> Value;
}

impl DeskHelpers for Helpers {
    fn bind(&mut self, log: LogFn) {
        Helpers::bind(self, log, Box::new(|_image| {}));
    }

    fn begin(&mut self) {
        Helpers::begin(self);
    }

    fn lane_first(&mut self, goal: &str) -> Value {
        Helpers::lane_first(self, goal, false)
    }

    fn decisions_first(&mut self, goal: &str, client: &mut dyn Evaluate) -> Value {
        Helpers::decisions_first(self, goal, client, false)
    }
}

/// Builds the lane's helpers, once, on the first attempt (Python `make_helpers`).
pub type MakeHelpers = Box<dyn Fn() -> anyhow::Result<Box<dyn DeskHelpers>> + Send + Sync>;
/// Whether the daemon has Accessibility access (Python `trusted`).
pub type Trusted = Box<dyn Fn() -> bool + Send + Sync>;
/// `decisions.enabled()`.
pub type DecisionsEnabled = Box<dyn Fn() -> bool + Send + Sync>;
/// `decisions.from_env()`.
pub type DecisionsFromEnv = Box<dyn Fn() -> Option<Box<dyn Evaluate + Send>> + Send + Sync>;

/// Helpers over the real desktop, configured as desktop_worker.main configures them (`pyautogui.FAILSAFE = True`,
/// `pyautogui.PAUSE = 0.05`, every other backend the real Mac).
#[cfg(target_os = "macos")]
pub fn real_helpers() -> anyhow::Result<Box<dyn DeskHelpers>> {
    let gui = pyautogui::PyAutoGui::new(pyautogui::RealQuartz);
    Ok(Box::new(Helpers::new(Box::new(gui), Backends::default())))
}

/// Off the Mac there is no Quartz to post through (Python: pyautogui's macOS backend fails to import).
#[cfg(not(target_os = "macos"))]
pub fn real_helpers() -> anyhow::Result<Box<dyn DeskHelpers>> {
    Err(eve_desktop_lanes::LaneError::err("ModuleNotFoundError", "No module named 'Quartz'"))
}

/// `AXIsProcessTrusted()`; false off the Mac (no ApplicationServices, no lane).
pub fn trusted() -> bool {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: no arguments; reads this process's trust state.
        unsafe { objc2_application_services::AXIsProcessTrusted() }
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// The first `n` characters of `s` (Python `s[:n]`).
fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Python truthiness of a JSON value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Python `str(v)` of a JSON value (`None` for a missing key, as `dict.get` gives).
fn py_str(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".to_string(),
        Some(Value::Bool(true)) => "True".to_string(),
        Some(Value::Bool(false)) => "False".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// Python `len(v)` of a truthy JSON value.
fn py_len(v: &Value) -> usize {
    match v {
        Value::Array(a) => a.len(),
        Value::Object(o) => o.len(),
        Value::String(s) => s.chars().count(),
        _ => 0,
    }
}

/// Python's `str.isprintable()` for one character, without the Unicode tables: control, format, separator and
/// private-use characters are escaped.
fn py_printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    if c.is_control() {
        return false;
    }
    let u = c as u32;
    let separator = matches!(u, 0xA0 | 0x1680 | 0x2000..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000);
    let format = matches!(
        u,
        0xAD | 0x600..=0x605
            | 0x61C
            | 0x6DD
            | 0x70F
            | 0x890..=0x891
            | 0x8E2
            | 0x180E
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    );
    let private = matches!(u, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD);
    !(separator || format || private)
}

/// Python's `repr(str)`: single quotes unless the text holds a single quote and no double quote.
fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if py_printable(c) => out.push(c),
            c if (c as u32) < 0x100 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) < 0x10000 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push(quote);
    out
}

struct State {
    helpers: Option<Box<dyn DeskHelpers>>,
    /// why the lane is off for this daemon's life, once known
    broken: String,
    decisions: Option<Box<dyn Evaluate + Send>>,
}

struct Inner {
    make_helpers: MakeHelpers,
    trusted: Trusted,
    decisions_enabled: DecisionsEnabled,
    decisions_from_env: DecisionsFromEnv,
    state: Mutex<State>,
}

/// A failure inside one attempt, by its Python exception name (`dict(...)` of a result that is not a dict, or a
/// panic in the helpers).
struct AttemptError(String);

impl Inner {
    fn attempt_blocking(&self, goal: &str) -> Result<Option<Map<String, Value>>, AttemptError> {
        let mut guard = self.state.lock();
        let state = &mut *guard;
        if !state.broken.is_empty() {
            return Ok(None);
        }
        if state.helpers.is_none() {
            if !(self.trusted)() {
                state.broken = "no Accessibility access for the daemon".to_string();
                tracing::warn!("desk lane: off — {}; desktop tasks go to Codex", state.broken);
                return Ok(None);
            }
            match (self.make_helpers)() {
                Ok(h) => state.helpers = Some(h),
                Err(e) => {
                    // a lane that cannot start leaves every task to Codex
                    state.broken = take_chars(&format!("{}: {}", error_kind(&e), error_message(&e)), 120);
                    tracing::warn!("desk lane: off — {}; desktop tasks go to Codex", state.broken);
                    return Ok(None);
                }
            }
        }
        let Some(helpers) = state.helpers.as_mut() else {
            return Ok(None);
        };
        let lines: Arc<Mutex<Vec<String>>> = Arc::default();
        let sink = Arc::clone(&lines);
        helpers.bind(Box::new(move |line: &str| sink.lock().push(line.to_string())));
        helpers.begin();
        let goal = take_chars(goal, MAX_GOAL_CHARS);
        let mut result = as_dict(helpers.lane_first(&goal))?;
        // Keep measured keyword successes cheap. The visual loop only sees a zero-input mismatch;
        // partial actions, dialogs and confirmation requests go directly to Codex.
        let status = result.get("status").and_then(Value::as_str);
        let reason = result.get("reason").and_then(Value::as_str);
        if status == Some("none")
            && matches!(reason, Some("no_match" | "ambiguous_target" | "uncovered"))
            && (self.decisions_enabled)()
        {
            if state.decisions.is_none() {
                state.decisions = (self.decisions_from_env)();
            }
            if let (Some(client), Some(helpers)) = (state.decisions.as_mut(), state.helpers.as_mut()) {
                result = as_dict(helpers.decisions_first(&goal, client.as_mut()))?;
            }
        }
        if !result.contains_key("log") {
            let lines = lines.lock().iter().cloned().map(Value::String).collect();
            result.insert("log".to_string(), Value::Array(lines));
        }
        Ok(Some(result))
    }
}

/// Python `dict(result)`: a result that is not a dict is a TypeError, which `attempt` turns into no lane.
fn as_dict(v: Value) -> Result<Map<String, Value>, AttemptError> {
    match v {
        Value::Object(m) => Ok(m),
        _ => Err(AttemptError("TypeError".to_string())),
    }
}

/// `lane.attempt(goal).await` → lane_router.RouteResult as a dict, or None when the lane did not try. Cloning
/// shares the one lane (its helpers, its lock), as Python's bound `attempt` method did.
#[derive(Clone)]
pub struct DeskLane {
    inner: Arc<Inner>,
}

impl Default for DeskLane {
    fn default() -> Self {
        Self::new()
    }
}

impl DeskLane {
    /// `DeskLane()`: the real desktop and the process's Accessibility trust, decisions from the environment.
    pub fn new() -> Self {
        Self::with(Box::new(real_helpers), Box::new(trusted))
    }

    /// `DeskLane(make_helpers=…, trusted=…)`.
    pub fn with(make_helpers: MakeHelpers, trusted: Trusted) -> Self {
        Self::with_decisions(
            make_helpers,
            trusted,
            Box::new(|| eve_decisions::enabled(None)),
            Box::new(|| eve_decisions::from_env(None).map(|c| Box::new(c) as Box<dyn Evaluate + Send>)),
        )
    }

    /// [`DeskLane::with`] plus the `decisions.enabled` / `decisions.from_env` seams (Python's tests monkeypatched
    /// them).
    pub fn with_decisions(
        make_helpers: MakeHelpers,
        trusted: Trusted,
        decisions_enabled: DecisionsEnabled,
        decisions_from_env: DecisionsFromEnv,
    ) -> Self {
        DeskLane {
            inner: Arc::new(Inner {
                make_helpers,
                trusted,
                decisions_enabled,
                decisions_from_env,
                state: Mutex::new(State { helpers: None, broken: String::new(), decisions: None }),
            }),
        }
    }

    /// Why the lane is off for this daemon's life ("" while it is not known to be off).
    pub fn broken(&self) -> String {
        self.inner.state.lock().broken.clone()
    }

    /// Try the lane on `goal` on a blocking thread. Never fails: an error costs the lane, not the task.
    pub async fn attempt(&self, goal: &str) -> Option<Map<String, Value>> {
        let t0 = Instant::now();
        let inner = Arc::clone(&self.inner);
        let goal = goal.to_string();
        let outcome = tokio::task::spawn_blocking(move || inner.attempt_blocking(&goal)).await;
        let result = match outcome {
            Ok(Ok(result)) => result,
            Ok(Err(AttemptError(kind))) => {
                // never costs the task
                tracing::warn!("desk lane: failed ({kind}); Codex takes it");
                return None;
            }
            Err(join) => {
                let kind = if join.is_panic() { "PanicException" } else { "CancelledError" };
                tracing::warn!("desk lane: failed ({kind}); Codex takes it");
                return None;
            }
        };
        if let Some(result) = &result {
            // The outcome and the labels clicked, never the request's words.
            let reason = if truthy(result.get("reason")) {
                format!(" ({})", py_str(result.get("reason")))
            } else {
                String::new()
            };
            let clicked = match result.get("clicked") {
                Some(c) if truthy(Some(c)) => format!("; clicked {}", py_len(c)),
                _ => String::new(),
            };
            tracing::info!(
                "desk lane: {}{} in {:.2} s{}",
                py_str(result.get("status")),
                reason,
                t0.elapsed().as_secs_f64(),
                clicked
            );
        }
        result
    }
}

/// What Codex is told after a partial run, so it starts from the screen as the lane left it.
pub fn codex_note(result: &Map<String, Value>) -> String {
    let clicked: Vec<String> = match result.get("clicked") {
        Some(Value::Array(items)) => items.iter().map(|c| py_str(Some(c))).collect(),
        // `for c in "Week"` walks the characters
        Some(Value::String(s)) => s.chars().map(String::from).collect(),
        Some(Value::Object(o)) => o.keys().cloned().collect(),
        _ => Vec::new(),
    };
    if clicked.is_empty() {
        return String::new();
    }
    let named: Vec<String> = clicked.iter().map(|c| py_repr(c)).collect();
    format!(
        "\n\nBefore you started, eve's fast lane already clicked {} in the frontmost window, and could not confirm \
         the request is finished. Look at the screen first; do not click those again unless the screen shows they \
         did nothing.",
        named.join(", then ")
    )
}

#[cfg(test)]
mod tests;
