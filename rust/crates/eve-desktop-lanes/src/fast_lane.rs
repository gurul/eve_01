//! The fast lane: a narrow run of clicks on labelled controls, decided without the planner. Port of
//! `fast_lane.py`; its module docstring is the contract and is kept here in substance.
//!
//! [`run_delegate`] is what the `delegate` helper calls. Per step it takes an accessibility snapshot
//! (ax_candidates), builds a bounded menu in code, picks the option that advances the objective (the keyword gate
//! in code, or a typed-decision model — see `decide`), gates the answer in code, acts through injected effectors,
//! settles, and checks `done_when`. The model only ranks: every judgement that can cost the human something —
//! sensitive labels, dialogs, System Settings values, return into a message, a hit-test miss, a focus change, a
//! repeat, a stall — is a code oracle here, and the lane can only ADD stops to the planner's ask_user contract,
//! never remove one.
//!
//! Measured cost floor of a step on this Mac (PLAN §0): AX snapshot 0.04-0.43 s, a keyword decide well under a
//! millisecond (a hosted Jev answer ~240 ms p50), hit-test ≤ 24 ms, settle 0.33-1.0 s — against a replaced planner
//! turn of 3.56 s API + 1.3 s exec. The honest promise is fewer failed-click recovery turns first.
//!
//! The decide step is two oracles in order. First the keyword gate ([`keyword_pick`]): when exactly one offered
//! click option shares the most objective tokens (label + value, via ax_candidates::objective_tokens) and that
//! count is above zero, it is the pick and the model is not asked — on the select fixtures this code-only rule is
//! 0.90 precise at 0.66 coverage, better than the model on its own. Otherwise the model ranks the menu, with each
//! click option worded by eve_decider::render_option ("Week (radio button)") and a context of the window title
//! (and the focused control). Every gate after the pick applies to a keyword pick exactly as to a model pick;
//! `Step::via` says which one decided.
//!
//! Senses ([`Senses`], the adapter lives in desktop_helpers):
//!
//! ```text
//! snapshot() -> Snapshot            the frontmost app's focused window, a fresh seq each call
//! text_visible(s) -> bool           fast OCR plus AX labels/values, right now
//! focused() -> Option<Candidate>    AXFocusedUIElement re-read now
//! frontmost_pid() -> int            0 when unknown
//! screen_changed() -> Option<bool>  the helpers' thumbnail diff since the step began; None = unknown
//! ```
//!
//! Effectors ([`Effectors`]):
//!
//! ```text
//! click_candidate(c) -> str         re-hit-tests the centre; a sentence starting "refused:" means
//!                                   the element is gone, in another app, or its title differs
//! focus_and_type(c, text) -> str    click, then paste only if the focused element is c; "refused:" otherwise
//! press(key) -> str                 one key, never a combo
//! settle(secs) -> float             wait for the screen to stop changing, up to secs; returns the wait
//! ```
//!
//! Result line (exact templates; values single-line, quotes escaped; the payload states are none | pending |
//! applied and a step applies its payload at most once):
//!
//! ```text
//! done: matched="{done_when}" via={ax|ocr|title}; steps={n}; last={action}; text={state}; key={state}
//! stopped: reason={one_step|dry_run}; steps={n}; last={action}; pick={id role "label"}; text=…; key=…
//! escalate: app="{app}"; reason={no_window|no_candidate|dialog_open|truncated|model_invalid|abstain|
//!           stalled|stalled_2|ambiguous_target|no_match|uncovered|hit_test_failed|focus_changed|
//!           step_cap|jev_error|jev_veto|jev_none}; top=[…≤3]; steps={n}; last={action}; text=…; key=…
//! confirm: "{label}" needs the human's yes; app="{app}"; action={click|type|press}; steps={n};
//!          last={action}; text=…; key=…
//! unavailable: {reason}
//! ```
//!
//! `last` is the last APPLIED input or none; `steps` counts steps that applied input; a confirm guarantees the
//! blocked action did not run; unavailable guarantees zero input.
//!
//! Who decides is `decide` (DECIDE_MODES, default "keyword"): the keyword gate alone, where a tie escalates
//! `ambiguous_target` and zero shared words escalates `no_match`, and no model is loaded or asked; "model", the gate
//! first and the decider on the rest; or "jev", the gate proposes and Jev's step asker can refuse. Keyword mode
//! clicks only: a call with `text` or `key` answers unavailable.
//!
//! [`run_script`] is the unit both callers above the lane use: an ordered list of objectives, one keyword-decided
//! click each, a fresh snapshot per step, stopping at the first step that does not act. Its line:
//!
//! ```text
//! script: {complete|partial|none}; applied=[{action}, …]; step {i}/{n} {that step's own line}
//! ```
//!
//! `complete` means every step either applied input that the Accessibility tree or the screen diff saw change
//! something, or found its control already selected. Anything less is partial (something was applied) or none
//! (nothing was), and the caller hands the rest to the planner.

use std::collections::HashSet;
use std::sync::LazyLock;

use indexmap::IndexMap;

use eve_decider::{render_option, Choice, RESERVED};
use eve_typed_ask::{StepAnswer, StepGates, JEV_STEP_GATES};

use crate::ax_candidates::{
    describe, needs_confirmation, objective_tokens, pressable, rank_candidates, Candidate, Kinds, Snapshot,
};
use crate::{casefold, error_kind, error_message, py_collapse, py_is_space, py_len, py_strip, py_take, Clock};

/// The shipped default for EVE_FAST_LANE. The holdout eval (tools/fastlane_eval.py --check-default, gate G6)
/// decides it: enabled only if gated top-1 ≥ 0.80, coverage ≥ 0.70, real ≥ keyword + 0.10 on overlap:false cases
/// and the cost-weighted score > 0. The last run (holdout, 82 cases, 2026-09-21) was of a local checkpoint since
/// removed from the lane: gated top-1 72.3%, coverage 79.3% — two of the four conditions failed, so the lane
/// ships off. The keyword gate alone decided 54.9% of holdout at 88.9%.
pub const FAST_LANE_DEFAULT: bool = false;
/// Who decides a step. "keyword": the keyword gate alone — a tie or zero shared words escalates, no model is asked
/// or loaded. "model": the gate first, the decider on the rest. "jev": the gate proposes and Jev's step asker can
/// refuse. Sliced by decision path on holdout (2026-09-21): keyword picks 88.9% right (n=45) against 29.7% for the
/// model picks of that run, so the default keeps any model out of the click path.
pub const DECIDE_MODES: [&str; 3] = ["keyword", "model", "jev"];
pub const DEFAULT_DECIDE: &str = "keyword";
/// The shipped default for EVE_LANE_FIRST (lane_router: the lane runs before the planner's first turn). The bar
/// was fixed before the first run: precision ≥ 0.90 on ≥ 10 engaged holdout cases, zero sensitive clicks.
/// Measured 2026-09-21: holdout 23 engaged of 82, 23 right (Wilson 95% 85.7–100%), select 26 of 73, 26 right; no
/// click on any abstain-expected case, no sensitive click.
pub const LANE_FIRST_DEFAULT: bool = true;
pub const MAX_SCRIPT_STEPS: usize = 6;
/// "jev" mode's menu: 25 real options + the two reserved. Jev reads 255 options, and on select the right control
/// is on a 25-menu in 60 of 69 cases against 58 on the 10-menu `model` mode offers.
pub const JEV_MAX_OPTIONS: usize = 27;
/// How `model` mode words its question: the style Jev scored best with.
pub const DEFAULT_STYLE: &str = "jev";
pub const SYSTEM_SETTINGS_BUNDLE: &str = "com.apple.systempreferences";
pub const SYSTEM_SETTINGS_NAV_ROLES: &[&str] = &["row", "cell", "tab", "link"];
/// In System Settings every `button` looks alike to the Accessibility tree: "Dark" (a value) and "Language &
/// Region" (a door to another pane) are both role button. So the doors are named here, a closed set: the sub-panes
/// of General. Opening one changes no setting; every control inside it is still a value and still confirms.
/// "Transfer or Reset" and "Device Management" are left out on purpose. This set never outranks the sensitive
/// table. Compared case-folded (Python stores them folded).
pub const SYSTEM_SETTINGS_PANE_BUTTONS: &[&str] = &[
    "About",
    "Software Update",
    "Storage",
    "AppleCare & Warranty",
    "AirDrop & Continuity",
    "AutoFill & Passwords",
    "Date & Time",
    "Language & Region",
    "Sharing",
    "Startup Disk",
    "Time Machine",
];
/// Python: `\b(search|address|url|go to)\b`, IGNORECASE.
pub static SEARCH_FIELD_WORDS: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?i)\b(search|address|url|go to)\b").expect("SEARCH_FIELD_WORDS"));
pub const RETURN_KEYS: &[&str] = &["return", "enter"];
pub const REOBSERVE_TEXT: &str = "the screen is still changing, look again";
pub const ABSTAIN_TEXT: &str = "none of these advances the objective";
pub const MAX_TYPE_OPTIONS: i64 = 3;
pub const MAX_TOP: usize = 3;
pub const MAX_LOG_CHARS: usize = 80;
pub const MAX_RECENT: usize = 3;
pub const STATUSES: [&str; 5] = ["done", "stopped", "escalate", "confirm", "unavailable"];

/// A lane result's status (STATUSES). Python's `format_line` raised ValueError for any other string; the enum makes
/// that impossible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LaneStatus {
    Done,
    Stopped,
    Escalate,
    Confirm,
    Unavailable,
}

impl LaneStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            LaneStatus::Done => "done",
            LaneStatus::Stopped => "stopped",
            LaneStatus::Escalate => "escalate",
            LaneStatus::Confirm => "confirm",
            LaneStatus::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    /// p_min / margin_min are the eval's (tools/fastlane_eval.py, 2026-09-21, select fixtures, 73 cases): the
    /// highest grid pair with coverage ≥ 0.70 on select — 0.60 / 0.15 covered 79.5% at 81.0% gated top-1. They gate
    /// model picks only; a keyword-gate pick carries p_top 1.0. The gate escalates ambiguous_target below them.
    pub p_min: f64,
    pub margin_min: f64,
    /// click + type + press + reobserve + abstain, total
    pub max_options: usize,
    /// clamped to [1, 6] by run_delegate
    pub max_steps: i64,
    pub settle_secs: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            p_min: 0.60,
            margin_min: 0.15,
            max_options: 12,
            max_steps: 4,
            settle_secs: 1.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub n: usize,
    pub snapshot_seq: i64,
    pub k: usize,
    pub dropped_cap: usize,
    pub dropped_budget: usize,
    pub chosen: String,
    pub description: String,
    pub p_top: f64,
    pub margin: f64,
    pub confidence: f64,
    /// act | reobserve | escalate | confirm | stopped
    pub verdict: String,
    pub reason: String,
    pub changed: Option<bool>,
    pub snapshot_ms: f64,
    pub node_count: i64,
    pub truncated: bool,
    pub decide_ms: f64,
    pub act_ms: f64,
    pub settle_ms: f64,
    /// keyword | model | jev | keyword+jev: which oracle picked
    pub via: String,
    /// click | type | press, "" for a reserved answer
    pub kind: String,
    /// the option texts the menu carried, in menu order
    pub offered: Vec<String>,
    /// the picked control's full label, "" for a press or a reserved answer
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DelegateResult {
    pub status: LaneStatus,
    pub line: String,
    pub steps: Vec<Step>,
    pub log_lines: Vec<String>,
}

/// One real option: what the lane would do if the model picks its key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    pub key: String,
    /// click | type | press
    pub kind: String,
    /// the option text the model sees
    pub text: String,
    pub candidate: Option<Candidate>,
    /// the text to type, or the key to press
    pub payload: String,
    /// the control's href on a page (Snapshot::href): an account link asks by it
    pub href: String,
}

// ---- the lane's protocols ------------------------------------------------------------------------

/// The senses protocol (module docs). `&self`: one adapter is lent as both senses and effectors.
pub trait Senses {
    fn snapshot(&self) -> anyhow::Result<Snapshot>;
    fn text_visible(&self, s: &str) -> anyhow::Result<bool>;
    fn focused(&self) -> Option<Candidate>;
    fn frontmost_pid(&self) -> anyhow::Result<i64>;
    fn screen_changed(&self) -> anyhow::Result<Option<bool>>;
    /// Python's optional `current_url` (browser_lane's page); None when the senses have none.
    fn current_url(&self) -> Option<anyhow::Result<String>> {
        None
    }
}

/// The effectors protocol (module docs). `&self`: one adapter is lent as both senses and effectors.
pub trait Effectors {
    fn click_candidate(&self, c: &Candidate) -> anyhow::Result<String>;
    fn focus_and_type(&self, c: &Candidate, text: &str) -> anyhow::Result<String>;
    fn press(&self, key: &str) -> anyhow::Result<String>;
    fn settle(&self, secs: f64) -> anyhow::Result<f64>;
    /// Python's optional `fill_from_vault(handle)` (browser_lane's page); None when the effectors have none.
    fn fill_from_vault(&self, _handle: &str) -> Option<anyhow::Result<String>> {
        None
    }
    /// Python's optional `clear_consent()` (browser_lane's page); None when the effectors have none.
    fn clear_consent(&self) -> Option<anyhow::Result<String>> {
        None
    }
    /// Python's `effectors.approve = …` (plan_executor._grant) on effectors that have their own sensitive-label
    /// gate; a no-op for the rest. `approved` is already normalised (collapsed, case-folded).
    fn grant(&self, _approved: &str) {}
}

/// `model` mode's decider (eve_decider::Decider over hosted Jev).
pub trait LaneDecider {
    /// Python `getattr(decider, "available", False)`.
    fn available(&self) -> bool;
    /// Python `decider.choose(objective, app=, context=, options=, recent=)`. An error is reported as
    /// "choose raised {kind}: {message}" (crate::error_kind / error_message).
    fn choose(
        &self,
        objective: &str,
        app: &str,
        context: &str,
        options: &IndexMap<String, String>,
        recent: &[String],
    ) -> anyhow::Result<Choice>;
}

impl LaneDecider for eve_decider::Decider {
    fn available(&self) -> bool {
        self.available
    }

    fn choose(
        &self,
        objective: &str,
        app: &str,
        context: &str,
        options: &IndexMap<String, String>,
        recent: &[String],
    ) -> anyhow::Result<Choice> {
        eve_decider::Decider::choose(self, objective, app, context, options, recent)
            .map_err(|e| crate::LaneError::err(e.kind(), e.to_string()))
    }
}

/// `jev` mode's step asker (eve_typed_ask::ask_jev_step bound to a predict): Python's
/// `asker(step, app=, context=, options=, recent=)` with an optional `max_options` attribute.
pub trait StepAsk {
    fn ask(
        &self,
        step: &str,
        app: &str,
        context: &str,
        options: &IndexMap<String, String>,
        recent: &[String],
    ) -> StepAnswer;
    /// A grouped asker's `max_options` (real controls it may be offered); None keeps the lane's own cap.
    fn max_options(&self) -> Option<usize> {
        None
    }
}

impl<F> StepAsk for F
where
    F: Fn(&str, &str, &str, &IndexMap<String, String>, &[String]) -> StepAnswer,
{
    fn ask(
        &self,
        step: &str,
        app: &str,
        context: &str,
        options: &IndexMap<String, String>,
        recent: &[String],
    ) -> StepAnswer {
        self(step, app, context, options, recent)
    }
}

impl<P, C> StepAsk for eve_typed_ask::StepAsker<P, C>
where
    P: Fn(&serde_json::Value, &serde_json::Value) -> anyhow::Result<serde_json::Value>,
    C: Fn() -> f64,
{
    fn ask(
        &self,
        step: &str,
        app: &str,
        context: &str,
        options: &IndexMap<String, String>,
        recent: &[String],
    ) -> StepAnswer {
        eve_typed_ask::StepAsker::ask(self, step, app, context, options, recent)
    }

    fn max_options(&self) -> Option<usize> {
        self.max_options
    }
}

// ---- formatting ----------------------------------------------------------------------------------

/// Python `_q`: a value as one line inside double quotes — whitespace collapsed, ';' turned to ',' (a label may
/// not forge a field), backslashes and quotes escaped.
pub fn q(value: &str) -> String {
    let text = py_collapse(value).replace(';', ","); // ';' separates fields: a label may not forge one
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// `7 radio button "Week"` — the id, role and full label of a candidate; "none" for None.
pub fn describe_pick(c: Option<&Candidate>) -> String {
    match c {
        None => "none".to_string(),
        Some(c) => format!("{} {} \"{}\"", c.id, c.role, q(&c.label)),
    }
}

/// Python `_action_text`: the `last=` form of an applied input — never the typed text, only its length.
pub fn action_text(item: &MenuItem) -> String {
    match (item.kind.as_str(), item.candidate.as_ref()) {
        ("click", Some(c)) => format!("click {}: {}", c.role, q(&c.label)),
        ("type", Some(c)) => format!("type {} chars into {}: {}", py_len(&item.payload), c.role, q(&c.label)),
        _ => format!("press {}", item.payload),
    }
}

/// format_line's keyword arguments, with Python's defaults (`Default`: steps 0, last/text/key "none", the rest
/// empty).
#[derive(Debug, Clone, PartialEq)]
pub struct LineFields {
    pub steps: usize,
    pub last: String,
    pub text_state: String,
    pub key_state: String,
    pub app: String,
    pub reason: String,
    pub done_when: String,
    pub via: String,
    pub pick: Option<Candidate>,
    pub top: Vec<Candidate>,
    pub label: String,
    pub action: String,
}

impl Default for LineFields {
    fn default() -> Self {
        LineFields {
            steps: 0,
            last: "none".into(),
            text_state: "none".into(),
            key_state: "none".into(),
            app: String::new(),
            reason: String::new(),
            done_when: String::new(),
            via: String::new(),
            pick: None,
            top: Vec::new(),
            label: String::new(),
            action: String::new(),
        }
    }
}

/// The one result sentence, exactly per the module docs' templates.
pub fn format_line(status: LaneStatus, f: &LineFields) -> String {
    let tail = format!(
        "steps={}; last={}; text={}; key={}",
        f.steps, f.last, f.text_state, f.key_state
    );
    match status {
        LaneStatus::Unavailable => format!("unavailable: {}", f.reason),
        LaneStatus::Done => format!("done: matched=\"{}\" via={}; {tail}", q(&f.done_when), f.via),
        LaneStatus::Stopped => format!(
            "stopped: reason={}; steps={}; last={}; pick={}; text={}; key={}",
            f.reason,
            f.steps,
            f.last,
            describe_pick(f.pick.as_ref()),
            f.text_state,
            f.key_state
        ),
        LaneStatus::Escalate => {
            let tops: Vec<String> = f.top.iter().take(MAX_TOP).map(|c| describe_pick(Some(c))).collect();
            format!(
                "escalate: app=\"{}\"; reason={}; top=[{}]; {tail}",
                q(&f.app),
                f.reason,
                tops.join(", ")
            )
        }
        LaneStatus::Confirm => format!(
            "confirm: \"{}\" needs the human's yes; app=\"{}\"; action={}; {tail}",
            q(&f.label),
            q(&f.app),
            f.action
        ),
    }
}

/// Python `_clip(text, limit=MAX_LOG_CHARS)`: whitespace collapsed, capped with "…".
pub fn clip(text: &str, limit: usize) -> String {
    let text = py_collapse(text);
    if py_len(&text) <= limit {
        return text;
    }
    let head = py_take(&text, limit.saturating_sub(1));
    format!("{}…", head.trim_end_matches(py_is_space))
}

// ---- the pure gate (step 5) ----------------------------------------------------------------------

/// The run state `gate` reads besides the answer and the item.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GateState<'a> {
    pub approve: Option<&'a str>,
    pub approve_used: bool,
    pub search_typed: bool,
    pub dialog_open: bool,
    pub reobserve_count: usize,
}

/// (verdict, reason) for one model answer. Pure: every rule is a code judgement.
///
/// Verdicts: "escalate" (model_invalid | abstain | stalled | ambiguous_target), "reobserve", "confirm" (sensitive |
/// return), "act". The order is the plan's: an invalid answer first, then the reserved answers, then the
/// probability gate, then what the pick would do.
pub fn gate(
    choice: &Choice,
    item: Option<&MenuItem>,
    thresholds: &Thresholds,
    state: &GateState<'_>,
) -> (&'static str, &'static str) {
    if choice.id.is_empty() || !choice.error.is_empty() {
        return ("escalate", "model_invalid");
    }
    if choice.id == "abstain" {
        return ("escalate", "abstain");
    }
    if choice.id == "reobserve" {
        return if state.reobserve_count < 1 {
            ("reobserve", "")
        } else {
            ("escalate", "stalled")
        };
    }
    if choice.p_top < thresholds.p_min || choice.margin < thresholds.margin_min {
        return ("escalate", "ambiguous_target");
    }
    let Some(item) = item.filter(|it| it.key == choice.id) else {
        return ("escalate", "model_invalid");
    };
    if item.kind == "click" || item.kind == "type" {
        let Some(c) = item.candidate.as_ref() else {
            // Python asserts a candidate here; a click or type item without one is not a valid answer.
            return ("escalate", "model_invalid");
        };
        let approved = matches!(state.approve, Some(a) if !state.approve_used && casefold(&c.label) == casefold(a));
        if needs_confirmation(c, &item.href) && !approved {
            // sensitive, or an account change
            return ("confirm", "sensitive");
        }
        return ("act", "");
    }
    let key = casefold(&item.payload);
    // Python: `key in RETURN_KEYS and not (search_typed and not dialog_open)`
    if RETURN_KEYS.contains(&key.as_str()) && (!state.search_typed || state.dialog_open) {
        return ("confirm", "return");
    }
    ("act", "")
}

// ---- helpers -------------------------------------------------------------------------------------

pub fn is_search_field(c: &Candidate) -> bool {
    if c.role == "search field" {
        return true;
    }
    (c.role == "text field" || c.role == "combo box") && SEARCH_FIELD_WORDS.is_match(&c.label)
}

/// Python `_same_control`: same role, label and frame.
pub fn same_control(a: &Candidate, b: &Candidate) -> bool {
    a.role == b.role && a.label == b.label && a.frame == b.frame
}

/// Fields the lane may type into: never secure, never in a dialog, and page-content fields (a web form) only when
/// the caller allowed page content.
pub fn editable_pool(snapshot: &Snapshot, allow_page_links: bool) -> Vec<Candidate> {
    snapshot
        .elements
        .iter()
        .filter(|c| {
            c.editable
                && c.enabled
                && !py_strip(&c.label).is_empty()
                && !c.secure
                && !c.in_dialog
                && (allow_page_links || !c.in_content)
        })
        .cloned()
        .collect()
}

/// Python `_shares_token`.
pub fn shares_token(c: &Candidate, tokens: &[String]) -> bool {
    let hay = casefold(&format!("{} {}", c.label, c.value));
    tokens.iter().any(|t| hay.contains(t.as_str()))
}

/// The focused control's line, `focused: {role} {label}` right-stripped, when it has a label or a role.
fn focused_line(snapshot: &Snapshot) -> Option<String> {
    let f = snapshot.focused.as_ref()?;
    if f.label.is_empty() && f.role.is_empty() {
        return None;
    }
    let line = format!("focused: {} {}", f.role, py_collapse(&f.label));
    Some(line.trim_end_matches(py_is_space).to_string())
}

/// What the model is told besides the goal and the app: the window title, and the focused control when there is
/// one — nothing else (the full context lines cost accuracy). The eval passes the same string.
pub fn lane_context(snapshot: &Snapshot) -> String {
    let mut parts = vec![format!("title: {}", py_collapse(&snapshot.title))];
    if let Some(line) = focused_line(snapshot) {
        parts.push(line);
    }
    parts.join("; ")
}

/// What a HOSTED model is told besides the step, the app and the menu: the focused control, and never the window
/// title — the one string most likely to be a document's name, a message's subject or a chat's. Measured on select
/// (2026-09-21): top-1 58/69 without the title against 57/69 with it, and the gate-then-veto path identical (49
/// right, 2 wrong), so the title bought nothing.
pub fn jev_context(snapshot: &Snapshot) -> String {
    focused_line(snapshot).unwrap_or_default()
}

/// Python `_overlap`: how many objective tokens the control's label and value carry (the keyword gate's score).
pub fn overlap(c: &Candidate, tokens: &HashSet<String>) -> usize {
    let have: HashSet<String> = objective_tokens(&format!("{} {}", c.label, c.value))
        .into_iter()
        .collect();
    tokens.intersection(&have).count()
}

/// The keyword gate, with its reason: (the pick, "") or (None, "tie" | "no_match"). The pick is the one click
/// option sharing the most objective tokens with its label and value, when that count is above zero and no other
/// click option ties it.
pub fn keyword_verdict<'m>(items: &'m [MenuItem], objective: &str) -> (Option<&'m MenuItem>, &'static str) {
    let tokens: HashSet<String> = objective_tokens(objective).into_iter().collect();
    if tokens.is_empty() {
        return (None, "no_match");
    }
    let mut best: Option<&MenuItem> = None;
    let mut best_score = 0usize;
    let mut tied = false;
    for it in items {
        let Some(c) = it.candidate.as_ref().filter(|_| it.kind == "click") else {
            continue;
        };
        let score = overlap(c, &tokens);
        if score > best_score {
            best = Some(it);
            best_score = score;
            tied = false;
        } else if score == best_score && score > 0 {
            tied = true;
        }
    }
    if best_score == 0 {
        return (None, "no_match");
    }
    if tied {
        return (None, "tie");
    }
    (best, "")
}

/// The keyword gate's pick, or None on a tie or on zero overlap (see keyword_verdict).
pub fn keyword_pick<'m>(items: &'m [MenuItem], objective: &str) -> Option<&'m MenuItem> {
    keyword_verdict(items, objective).0
}

/// The objective's words that the picked control does not account for: not in its label, its role, the
/// frontmost app's name, or `extra` (the caller's generic words). The value is left out on purpose: "is week
/// selected" must not read as covered by a selected Week. An empty list is what lets the router act on a human's
/// own words without a planner: the request says nothing the click does not do.
pub fn uncovered_tokens(objective: &str, c: &Candidate, app: &str, extra: &[String]) -> Vec<String> {
    let mut have: HashSet<String> = objective_tokens(&format!("{} {} {}", c.label, c.role, app))
        .into_iter()
        .collect();
    have.extend(extra.iter().map(|w| casefold(w)));
    objective_tokens(objective)
        .into_iter()
        .filter(|t| !have.contains(t))
        .collect()
}

/// Python `_ax_changed`: the chosen control's value flipped, the focus moved, the title changed, the element count
/// or truncation changed. None when nothing in the tree says either way.
pub fn ax_changed(before: &Snapshot, after: &Snapshot, chosen: Option<&Candidate>) -> Option<bool> {
    if let Some(chosen) = chosen {
        if after
            .elements
            .iter()
            .any(|c| same_control(c, chosen) && c.value != chosen.value)
        {
            return Some(true);
        }
    }
    let focus_key = |s: &Snapshot| s.focused.as_ref().map(|f| (f.role.clone(), f.label.clone(), f.frame));
    if focus_key(before) != focus_key(after) {
        return Some(true);
    }
    if before.title != after.title {
        return Some(true);
    }
    if before.elements.len() != after.elements.len() || before.truncated != after.truncated {
        return Some(true);
    }
    None
}

/// A choice that puts all of `p` on `pick`: Python's `Choice(pick, p_top=, margin=, confidence=,
/// probabilities={k: (p if k == pick else 0.0) for k in options}, k=len(options))`.
fn sure_choice(pick: &str, p_top: f64, margin: f64, confidence: f64, options: &IndexMap<String, String>) -> Choice {
    Choice {
        id: pick.to_string(),
        p_top,
        margin,
        confidence,
        probabilities: options
            .keys()
            .map(|k| (k.clone(), if k == pick { p_top } else { 0.0 }))
            .collect(),
        k: options.len(),
        ..Choice::default()
    }
}

/// Python `_jev_verdict`: (via, choice, stop) for one Jev answer. `stop` is "" to go on to the code gate,
/// "confirm", or an escalation reason: jev_error (no usable answer — nothing is pressed on a guess), jev_veto (the
/// keyword gate had a pick and Jev's top control is another one, without clearing its own bar), jev_none (no pick
/// from either).
pub fn jev_verdict(
    answer: &StepAnswer,
    keyword: Option<&MenuItem>,
    gates: &StepGates,
    options: &IndexMap<String, String>,
) -> (String, Choice, String) {
    let empty = Choice {
        k: options.len(),
        ..Choice::default()
    };
    if !answer.error.is_empty() || answer.target.is_empty() {
        return ("jev".into(), empty, "jev_error".into());
    }
    // the pick is still worked out: the human is asked about IT
    let risky = answer.risky >= gates.risky_max;
    let stop = |risky: bool| if risky { "confirm".to_string() } else { String::new() };
    if let Some(keyword) = keyword.filter(|kw| answer.target == kw.key) {
        let choice = sure_choice(&keyword.key, 1.0, 1.0, answer.present, options);
        return ("keyword+jev".into(), choice, stop(risky));
    }
    let pick = StepGates {
        risky_max: 2.0,
        ..*gates
    }
    .decide(answer);
    if options.contains_key(&pick) && !RESERVED.contains(&pick.as_str()) {
        let choice = sure_choice(&pick, answer.p_target, answer.margin, answer.present, options);
        return ("jev".into(), choice, stop(risky));
    }
    if risky {
        return ("jev".into(), empty, "confirm".into());
    }
    let reason = if keyword.is_some() { "jev_veto" } else { "jev_none" };
    ("jev".into(), empty, reason.into())
}

/// Python `_valid_key`: (normalised key, error). Combos (a "+", "command", "cmd", a space) are never offered.
pub fn valid_key(key: Option<&str>) -> (Option<String>, String) {
    let Some(key) = key else {
        return (None, String::new());
    };
    let k = casefold(&py_collapse(key));
    if k.is_empty() {
        return (None, String::new());
    }
    if k.contains('+') || k.contains("command") || k.contains("cmd") || k.contains(' ') {
        return (None, "key combos are not supported".into());
    }
    (Some(k), String::new())
}

/// Python's `round(x, 2)` for the step timings.
fn round2(x: f64) -> f64 {
    crate::py_round_n(x, 2)
}

/// Python's `str(type(e).__name__)` and `str(e)` of a caught error.
fn kind_and_message(e: &anyhow::Error) -> (String, String) {
    (error_kind(e), error_message(e))
}

// ---- the loop ------------------------------------------------------------------------------------

/// Python `_Run`: one run_delegate call — the state the templates need, and the step log.
pub(crate) struct Run<'a> {
    pub(crate) objective: String,
    pub(crate) text: Option<String>,
    pub(crate) key: Option<String>,
    pub(crate) done_when: Option<String>,
    pub(crate) approve: Option<String>,
    pub(crate) approve_used: bool,
    pub(crate) thresholds: Thresholds,
    pub(crate) clock: Clock<'a>,
    pub(crate) steps: Vec<Step>,
    pub(crate) log_lines: Vec<String>,
    pub(crate) applied: usize,
    pub(crate) last: String,
    pub(crate) recent: Vec<String>,
    pub(crate) search_typed: bool,
    pub(crate) text_state: String,
    pub(crate) key_state: String,
    /// last step's chosen control, never re-offered
    pub(crate) previous: Option<Candidate>,
    pub(crate) unchanged_streak: usize,
    pub(crate) reobserves: usize,
    pub(crate) app: String,
    pub(crate) top: Vec<Candidate>,
}

impl<'a> Run<'a> {
    pub(crate) fn new(
        objective: &str,
        text: Option<String>,
        key: Option<String>,
        done_when: Option<String>,
        approve: Option<String>,
        thresholds: Thresholds,
        clock: Clock<'a>,
    ) -> Self {
        let text_state = if text.as_deref().is_some_and(|t| !t.is_empty()) {
            "pending"
        } else {
            "none"
        };
        let key_state = if key.as_deref().is_some_and(|k| !k.is_empty()) {
            "pending"
        } else {
            "none"
        };
        Run {
            objective: objective.to_string(),
            text,
            key,
            done_when,
            approve,
            approve_used: false,
            thresholds,
            clock,
            steps: Vec::new(),
            log_lines: Vec::new(),
            applied: 0,
            last: "none".into(),
            recent: Vec::new(),
            search_typed: false,
            text_state: text_state.into(),
            key_state: key_state.into(),
            previous: None,
            unchanged_streak: 0,
            reobserves: 0,
            app: String::new(),
            top: Vec::new(),
        }
    }

    pub(crate) fn finish(&mut self, status: LaneStatus, line: String) -> DelegateResult {
        self.log_lines.push(clip(&format!("delegate {line}"), MAX_LOG_CHARS));
        DelegateResult {
            status,
            line,
            steps: self.steps.clone(),
            log_lines: self.log_lines.clone(),
        }
    }

    /// The fields every template after the first snapshot shares.
    fn fields(&self) -> LineFields {
        LineFields {
            steps: self.applied,
            last: self.last.clone(),
            text_state: self.text_state.clone(),
            key_state: self.key_state.clone(),
            ..LineFields::default()
        }
    }

    pub(crate) fn escalate(&mut self, reason: &str) -> DelegateResult {
        let f = LineFields {
            app: self.app.clone(),
            reason: reason.to_string(),
            top: self.top.clone(),
            ..self.fields()
        };
        let line = format_line(LaneStatus::Escalate, &f);
        self.finish(LaneStatus::Escalate, line)
    }

    pub(crate) fn confirm(&mut self, label: &str, action: &str) -> DelegateResult {
        let f = LineFields {
            label: label.to_string(),
            app: self.app.clone(),
            action: action.to_string(),
            ..self.fields()
        };
        let line = format_line(LaneStatus::Confirm, &f);
        self.finish(LaneStatus::Confirm, line)
    }

    pub(crate) fn done(&mut self, via: &str) -> DelegateResult {
        let f = LineFields {
            done_when: self.done_when.clone().unwrap_or_default(),
            via: via.to_string(),
            ..self.fields()
        };
        let line = format_line(LaneStatus::Done, &f);
        self.finish(LaneStatus::Done, line)
    }

    pub(crate) fn stopped(&mut self, reason: &str, pick: Option<&Candidate>) -> DelegateResult {
        let f = LineFields {
            reason: reason.to_string(),
            pick: pick.cloned(),
            ..self.fields()
        };
        let line = format_line(LaneStatus::Stopped, &f);
        self.finish(LaneStatus::Stopped, line)
    }

    fn unavailable(&mut self, reason: &str) -> DelegateResult {
        let line = format_line(
            LaneStatus::Unavailable,
            &LineFields {
                reason: reason.to_string(),
                ..LineFields::default()
            },
        );
        self.finish(LaneStatus::Unavailable, line)
    }
}

/// Python `_done_check`: which oracle says done_when is met: "ax", "ocr", "title", or "". The ocr and title rules
/// need the marker to have been absent at step 0; the ax rule (a control with that label is selected/checked) may
/// fire at once, which is how a run from the target state returns done.
pub(crate) fn done_check(
    run: &Run<'_>,
    senses: &dyn Senses,
    snapshot: &Snapshot,
    title0: &str,
    visible0: bool,
    allow_now: bool,
) -> &'static str {
    let Some(marker) = run.done_when.as_deref().filter(|m| !m.is_empty()) else {
        return "";
    };
    let folded = casefold(marker);
    let needle = py_strip(&folded);
    for c in &snapshot.elements {
        if py_strip(&casefold(&c.label)) == needle && (c.value == "selected" || c.value == "checked") {
            return "ax";
        }
    }
    if !allow_now {
        return "";
    }
    if casefold(&snapshot.title).contains(needle) && !casefold(title0).contains(needle) {
        return "title";
    }
    if !visible0 {
        // a failed OCR read is "not visible", never a crash mid-run
        return match senses.text_visible(marker) {
            Ok(true) => "ocr",
            _ => "",
        };
    }
    ""
}

/// Python `_timed_snapshot`: (snapshot, ms).
pub(crate) fn timed_snapshot(run: &Run<'_>, senses: &dyn Senses) -> anyhow::Result<(Snapshot, f64)> {
    let t0 = (run.clock)();
    let snap = senses.snapshot()?;
    Ok((snap, ((run.clock)() - t0) * 1000.0))
}

/// Python `_build_menu`: (items, dropped_by_cap, blocked). `blocked` are the objective-relevant controls the lane
/// withholds — sensitive labels without approval, and non-navigation roles in System Settings — best match first,
/// so the caller can answer confirm for the best of them when nothing offered matches. Sets `run.top`.
pub(crate) fn build_menu(
    run: &mut Run<'_>,
    snapshot: &Snapshot,
    allow_page_links: bool,
) -> (Vec<MenuItem>, usize, Vec<Candidate>) {
    let th = run.thresholds;
    let tokens = objective_tokens(&run.objective);
    let mut type_items: Vec<MenuItem> = Vec::new();
    let mut press_items: Vec<MenuItem> = Vec::new();
    if run.text_state == "pending" {
        let (fields, _) = rank_candidates(
            snapshot,
            &run.objective,
            MAX_TYPE_OPTIONS,
            Kinds::Editable,
            allow_page_links,
            false,
        );
        for c in fields {
            if run.previous.as_ref().is_some_and(|p| same_control(&c, p)) {
                continue;
            }
            type_items.push(MenuItem {
                key: String::new(),
                kind: "type".into(),
                text: describe(&c, "type into", 40),
                candidate: Some(c),
                payload: run.text.clone().unwrap_or_default(),
                href: String::new(),
            });
        }
    }
    if run.key_state == "pending" && run.text_state != "pending" {
        let key = run.key.clone().unwrap_or_default();
        press_items.push(MenuItem {
            key: String::new(),
            kind: "press".into(),
            text: format!("press {key}"),
            candidate: None,
            payload: key,
            href: String::new(),
        });
    }
    let cap = (th.max_options as i64 - RESERVED.len() as i64 - type_items.len() as i64 - press_items.len() as i64)
        .max(1) as usize;
    let max_out = if snapshot.elements.is_empty() {
        1
    } else {
        snapshot.elements.len() as i64
    };
    let (pool, _) = rank_candidates(
        snapshot,
        &run.objective,
        max_out,
        Kinds::Pressable,
        allow_page_links,
        true,
    );
    let settings = snapshot.bundle == SYSTEM_SETTINGS_BUNDLE;
    let mut offered: Vec<Candidate> = Vec::new();
    let mut blocked: Vec<Candidate> = Vec::new();
    for c in pool {
        if run.previous.as_ref().is_some_and(|p| same_control(&c, p)) {
            continue;
        }
        let approved = run
            .approve
            .as_deref()
            .is_some_and(|a| !run.approve_used && casefold(&c.label) == casefold(a));
        if needs_confirmation(&c, &snapshot.href(&c.id)) && !approved {
            blocked.push(c);
            continue;
        }
        if settings
            && !SYSTEM_SETTINGS_NAV_ROLES.contains(&c.role.as_str())
            && casefold(&c.label) != "back"
            && !(c.role == "button" && is_pane_button(&c.label))
        {
            blocked.push(c);
            continue;
        }
        offered.push(c);
    }
    let dropped_cap = offered.len().saturating_sub(cap);
    let mut items: Vec<MenuItem> = Vec::new();
    for c in offered.into_iter().take(cap) {
        let href = snapshot.href(&c.id);
        items.push(MenuItem {
            key: String::new(),
            kind: "click".into(),
            text: render_option(&c),
            candidate: Some(c),
            payload: String::new(),
            href,
        });
    }
    items.extend(type_items);
    items.extend(press_items);
    for (i, it) in items.iter_mut().enumerate() {
        it.key = (i + 1).to_string();
    }
    run.top = items
        .iter()
        .filter(|it| it.kind == "click")
        .filter_map(|it| it.candidate.clone())
        .take(MAX_TOP)
        .collect();
    let token_set: HashSet<String> = tokens.into_iter().collect();
    let mut blocked_matching: Vec<(usize, Candidate)> = blocked
        .into_iter()
        .map(|c| (overlap(&c, &token_set), c))
        .filter(|(score, _)| *score > 0)
        .collect();
    // stable: reading order breaks ties, as Python's sorted(key=-overlap)
    blocked_matching.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    (
        items,
        dropped_cap,
        blocked_matching.into_iter().map(|(_, c)| c).collect(),
    )
}

/// One of the System Settings sub-pane doors (SYSTEM_SETTINGS_PANE_BUTTONS), compared collapsed and case-folded.
fn is_pane_button(label: &str) -> bool {
    let name = casefold(&py_collapse(label));
    SYSTEM_SETTINGS_PANE_BUTTONS.iter().any(|b| casefold(b) == name)
}

/// run_delegate's keyword arguments, with Python's defaults (`Default`).
pub struct DelegateOptions<'a> {
    pub text: Option<String>,
    pub key: Option<String>,
    pub done_when: Option<String>,
    pub approve: Option<String>,
    pub allow_page_links: bool,
    pub max_steps: i64,
    pub dry_run: bool,
    pub thresholds: Thresholds,
    pub clock: Clock<'a>,
    /// DECIDE_MODES; anything else answers unavailable
    pub decide: String,
    pub require_cover: bool,
    pub cover_extra: Vec<String>,
    pub skip_if_selected: bool,
    pub asker: Option<&'a dyn StepAsk>,
    pub step_gates: StepGates,
}

impl Default for DelegateOptions<'_> {
    fn default() -> Self {
        DelegateOptions {
            text: None,
            key: None,
            done_when: None,
            approve: None,
            allow_page_links: false,
            max_steps: 4,
            dry_run: false,
            thresholds: Thresholds::default(),
            clock: &crate::perf_counter,
            decide: DEFAULT_DECIDE.to_string(),
            require_cover: false,
            cover_extra: Vec::new(),
            skip_if_selected: false,
            asker: None,
            step_gates: JEV_STEP_GATES,
        }
    }
}

/// `" ".join(str(s).split())` when `s` has any non-space character, else None (done_when and approve).
fn collapsed_or_none(s: Option<&str>) -> Option<String> {
    s.filter(|s| !py_strip(s).is_empty()).map(py_collapse)
}

/// The loop of the module docs. Never fails for a model or sense failure: the status says what happened, and
/// every path that could act consequentially returns confirm first.
///
/// `decide` is who picks (DECIDE_MODES): in "keyword" mode the decider may be None. `require_cover` is the
/// router's rule (lane_router): act only on a keyword pick that accounts for every word of the objective
/// (uncovered_tokens), else escalate `uncovered` with zero input. `skip_if_selected` answers done with zero input
/// when the keyword pick is already selected.
///
/// In "jev" mode `asker` is the step asker, and the decider is unused. The keyword gate PROPOSES and Jev can
/// REFUSE: a gate pick is pressed only when Jev's own top control is the same one; otherwise the step is Jev's,
/// under `step_gates`. On the read fixtures that took the click path from 40 right / 5 wrong to 43 right / 1 wrong.
/// Jev's `risky` noul can only ADD a confirm, and every code gate after the pick applies to its pick too.
///
/// A snapshot that fails AFTER step 0 (Python lets that exception out of run_delegate) answers
/// `escalate: … reason=no_window ({kind})` here, with the applied steps still in the line; [`try_run_delegate`]
/// returns the error instead.
pub fn run_delegate(
    objective: &str,
    senses: &dyn Senses,
    effectors: &dyn Effectors,
    decider: Option<&dyn LaneDecider>,
    opts: DelegateOptions<'_>,
) -> DelegateResult {
    // With propagate=false a failed snapshot is answered in the line, so the Err arm only names it.
    delegate(objective, senses, effectors, decider, opts, false).unwrap_or_else(|e| failed_snapshot(&e))
}

/// run_delegate with Python's exception path kept: a snapshot that fails after step 0 is an `Err`, as the Python
/// raised it to its caller (lane_router::route turns it into `error:{kind}`, as Helpers.lane_first did).
pub fn try_run_delegate(
    objective: &str,
    senses: &dyn Senses,
    effectors: &dyn Effectors,
    decider: Option<&dyn LaneDecider>,
    opts: DelegateOptions<'_>,
) -> anyhow::Result<DelegateResult> {
    delegate(objective, senses, effectors, decider, opts, true)
}

/// `escalate: … reason=no_window ({kind})` for a snapshot error that reached the infallible wrappers.
fn failed_snapshot(e: &anyhow::Error) -> DelegateResult {
    let fields = LineFields {
        reason: format!("no_window ({})", error_kind(e)),
        ..LineFields::default()
    };
    let line = format_line(LaneStatus::Escalate, &fields);
    DelegateResult {
        status: LaneStatus::Escalate,
        log_lines: vec![clip(&format!("delegate {line}"), MAX_LOG_CHARS)],
        line,
        steps: Vec::new(),
    }
}

/// run_delegate and try_run_delegate: `propagate` says what a mid-run snapshot failure becomes.
fn delegate(
    objective: &str,
    senses: &dyn Senses,
    effectors: &dyn Effectors,
    decider: Option<&dyn LaneDecider>,
    opts: DelegateOptions<'_>,
    propagate: bool,
) -> anyhow::Result<DelegateResult> {
    let objective = py_collapse(objective);
    let text = opts.text.filter(|t| !t.is_empty());
    let done_when = collapsed_or_none(opts.done_when.as_deref());
    let approve = collapsed_or_none(opts.approve.as_deref());
    let (key, key_error) = valid_key(opts.key.as_deref());
    let mut thresholds = opts.thresholds;
    let mut run = Run::new(
        &objective,
        text.clone(),
        key.clone(),
        done_when.clone(),
        approve.clone(),
        thresholds,
        opts.clock,
    );
    if !key_error.is_empty() {
        return Ok(run.unavailable(&key_error));
    }
    if objective.is_empty() {
        return Ok(run.unavailable("an objective is required"));
    }
    let decide = opts.decide.as_str();
    if !DECIDE_MODES.contains(&decide) {
        let modes: Vec<String> = DECIDE_MODES.iter().map(|m| format!("'{m}'")).collect();
        return Ok(run.unavailable(&format!("decide must be one of ({})", modes.join(", "))));
    }
    let decider = decider.filter(|d| d.available());
    if decide == "model" && decider.is_none() {
        return Ok(run.unavailable("the decider is not loaded"));
    }
    if decide == "jev" && opts.asker.is_none() {
        return Ok(run.unavailable("jev is not configured"));
    }
    if (decide == "keyword" || decide == "jev") && (text.is_some() || key.is_some()) {
        return Ok(run.unavailable("the keyword lane only clicks; type with type_text and press keys yourself"));
    }
    if decide == "jev" {
        // A grouped asker (EVE_JEV_GROUPS) reads past one 25-menu: its `max_options` real controls, plus the two
        // reserved. Any other asker keeps the 25.
        let menu = match opts.asker.and_then(|a| a.max_options()) {
            Some(n) if n > 0 => n + RESERVED.len(),
            _ => JEV_MAX_OPTIONS,
        };
        if thresholds.max_options < menu {
            thresholds.max_options = menu;
            run.thresholds = thresholds;
        }
    }
    let max_steps = opts.max_steps.clamp(1, 6) as usize;

    // step 0: the baseline the ocr and title oracles compare against
    let (mut snapshot, mut snapshot_ms) = match timed_snapshot(&run, senses) {
        Ok(s) => s,
        Err(e) => {
            // no snapshot means no lane, not a traceback in exec_py
            run.app = String::new();
            return Ok(run.escalate(&format!("no_window ({})", error_kind(&e))));
        }
    };
    run.app = snapshot.app.clone();
    let title0 = snapshot.title.clone();
    let mut visible0 = false;
    if let Some(dw) = done_when.as_deref() {
        // unknown baseline: the ocr oracle then needs a later sighting
        visible0 = senses.text_visible(dw).unwrap_or(true);
    }
    if !snapshot.dialog_text.is_empty() {
        // a dialog is never operated, not even to say "done"
        return Ok(run.escalate(&format!("dialog_open: \"{}\"", q(&snapshot.dialog_text))));
    }
    if snapshot.truncated {
        // A visible label cannot establish uniqueness when other controls were omitted.
        return Ok(run.escalate("truncated"));
    }
    let via = done_check(&run, senses, &snapshot, &title0, visible0, false);
    if !via.is_empty() {
        return Ok(run.done(via));
    }

    // A snapshot after step 0 that fails: Python raised it out of run_delegate.
    macro_rules! resnapshot {
        () => {
            match timed_snapshot(&run, senses) {
                Ok(s) => s,
                Err(e) if propagate => return Err(e),
                Err(e) => return Ok(run.escalate(&format!("no_window ({})", error_kind(&e)))),
            }
        };
    }

    let mut n = 0usize;
    while n < max_steps {
        n += 1;
        if !snapshot.app.is_empty() {
            run.app = snapshot.app.clone();
        }
        if snapshot.elements.is_empty() && snapshot.node_count == 0 {
            return Ok(run.escalate("no_window"));
        }
        if !snapshot.dialog_text.is_empty() {
            return Ok(run.escalate(&format!("dialog_open: \"{}\"", q(&snapshot.dialog_text))));
        }
        if pressable(&snapshot).len() < 2 && editable_pool(&snapshot, false).is_empty() {
            (snapshot, snapshot_ms) = resnapshot!(); // one re-snapshot
            if snapshot.elements.is_empty() && snapshot.node_count == 0 {
                return Ok(run.escalate("no_window"));
            }
            if !snapshot.dialog_text.is_empty() {
                return Ok(run.escalate(&format!("dialog_open: \"{}\"", q(&snapshot.dialog_text))));
            }
            if pressable(&snapshot).len() < 2 && editable_pool(&snapshot, false).is_empty() {
                return Ok(run.escalate("no_candidate"));
            }
        }
        let (items, dropped_cap, blocked) = build_menu(&mut run, &snapshot, opts.allow_page_links);
        let tokens = objective_tokens(&objective);
        if snapshot.truncated {
            return Ok(run.escalate("truncated"));
        }
        // A withheld control (sensitive, or a System Settings value) that matches the objective at least as well
        // as anything offered is what the human meant: confirm it, never a look-alike ("delete the event" must not
        // click "Add Event"). Only an offered option that matches strictly better goes on to the pick.
        // An offered control whose label IS the objective (the plan copied it from the outline) is what was meant,
        // however many words a withheld one shares: "Your message" is the field, not "Send message"
        // (browser_model_eval contact_form, 2026-09-24). The withheld control stays withheld either way.
        let token_set: HashSet<String> = tokens.into_iter().collect();
        let offered_best = items
            .iter()
            .filter_map(|it| it.candidate.as_ref())
            .map(|c| overlap(c, &token_set))
            .max()
            .unwrap_or(0);
        let objective_folded = casefold(&objective);
        let named = items
            .iter()
            .filter_map(|it| it.candidate.as_ref())
            .any(|c| casefold(&py_collapse(&c.label)) == objective_folded);
        if let Some(first) = blocked.first() {
            if !named && overlap(first, &token_set) >= offered_best {
                let label = first.label.clone();
                return Ok(run.confirm(&label, "click"));
            }
        }
        if items.is_empty() {
            return Ok(run.escalate("no_candidate"));
        }
        let mut options: IndexMap<String, String> = items.iter().map(|it| (it.key.clone(), it.text.clone())).collect();
        options.insert("reobserve".into(), REOBSERVE_TEXT.into());
        options.insert("abstain".into(), ABSTAIN_TEXT.into());
        let t0 = (run.clock)();
        let (keyword, why_not) = keyword_verdict(&items, &objective);
        if let Some(kc) = keyword.and_then(|k| k.candidate.as_ref()) {
            if opts.require_cover {
                let missing = uncovered_tokens(&objective, kc, &snapshot.app, &opts.cover_extra);
                if !missing.is_empty() {
                    run.log_lines.push(clip(
                        &format!(
                            "delegate route: {} leaves {} unexplained",
                            describe_pick(Some(kc)),
                            missing.join(", ")
                        ),
                        MAX_LOG_CHARS,
                    ));
                    return Ok(run.escalate("uncovered"));
                }
            }
            if opts.skip_if_selected && kc.value == "selected" {
                run.done_when = Some(kc.label.clone());
                return Ok(run.done("ax"));
            }
        }
        let recent: Vec<String> = run.recent[run.recent.len().saturating_sub(MAX_RECENT)..].to_vec();
        let via: String;
        let choice: Choice;
        if decide == "jev" && (keyword.is_some() || !opts.require_cover) {
            let Some(asker) = opts.asker else {
                return Ok(run.unavailable("jev is not configured"));
            };
            let real: IndexMap<String, String> = items
                .iter()
                .filter(|it| it.kind == "click")
                .map(|it| (it.key.clone(), it.text.clone()))
                .collect();
            let answer = asker.ask(&objective, &snapshot.app, &jev_context(&snapshot), &real, &recent);
            let (v, c, stop) = jev_verdict(&answer, keyword, &opts.step_gates, &options);
            let target = if answer.target.is_empty() {
                "-"
            } else {
                answer.target.as_str()
            };
            let error = if answer.error.is_empty() {
                String::new()
            } else {
                format!(" {}", answer.error)
            };
            run.log_lines.push(clip(
                &format!(
                    "delegate jev: target={target} p={:.2} present={:.2} risky={:.2} {:.0}ms{error}",
                    answer.p_target, answer.present, answer.risky, answer.ms
                ),
                MAX_LOG_CHARS,
            ));
            if stop == "confirm" {
                // Jev read the step as consequential. The human is asked about the control it would press, and
                // their yes (`approve`, once) is the only thing that lets that control through.
                let wanted = if c.id.is_empty() {
                    answer.target.as_str()
                } else {
                    c.id.as_str()
                };
                let label = items
                    .iter()
                    .find(|it| it.key == wanted && it.candidate.is_some())
                    .and_then(|it| it.candidate.as_ref())
                    .map(|cand| cand.label.clone())
                    .unwrap_or_else(|| objective.clone());
                let approved = !c.id.is_empty()
                    && approve
                        .as_deref()
                        .is_some_and(|a| !run.approve_used && casefold(&label) == casefold(a));
                if !approved {
                    return Ok(run.confirm(&label, "click"));
                }
            } else if !stop.is_empty() {
                return Ok(run.escalate(&stop));
            }
            via = v;
            choice = c;
        } else if let Some(kw) = keyword {
            via = "keyword".into();
            choice = sure_choice(&kw.key, 1.0, 1.0, 1.0, &options);
        } else if decide == "keyword" || decide == "jev" || opts.require_cover {
            // The gate abstained and nobody else is asked: a tie is ambiguous, zero shared words is no match.
            // Zero input either way; the planner takes it from here.
            return Ok(run.escalate(if why_not == "tie" {
                "ambiguous_target"
            } else {
                "no_match"
            }));
        } else {
            via = "model".into();
            choice = match decider {
                // a contract error in the lane is still "the model gave nothing"
                Some(d) => match d.choose(&objective, &snapshot.app, &lane_context(&snapshot), &options, &recent) {
                    Ok(c) => c,
                    Err(e) => {
                        let (kind, message) = kind_and_message(&e);
                        Choice {
                            error: py_take(&format!("choose raised {kind}: {message}"), 200),
                            ..Choice::default()
                        }
                    }
                },
                None => return Ok(run.unavailable("the decider is not loaded")),
            };
        }
        let decide_ms = ((run.clock)() - t0) * 1000.0;
        let item = items.iter().find(|it| it.key == choice.id);
        let (verdict, reason) = gate(
            &choice,
            item,
            &thresholds,
            &GateState {
                approve: approve.as_deref(),
                approve_used: run.approve_used,
                search_typed: run.search_typed,
                dialog_open: !snapshot.dialog_text.is_empty(),
                reobserve_count: run.reobserves,
            },
        );
        let step = Step {
            n,
            snapshot_seq: snapshot.seq,
            k: if choice.k != 0 { choice.k } else { options.len() },
            dropped_cap,
            dropped_budget: choice.dropped_for_budget,
            chosen: choice.id.clone(),
            description: item.map(|it| it.text.clone()).unwrap_or_else(|| choice.id.clone()),
            p_top: choice.p_top,
            margin: choice.margin,
            confidence: choice.confidence,
            verdict: verdict.into(),
            reason: reason.into(),
            changed: None,
            snapshot_ms: round2(snapshot_ms),
            node_count: snapshot.node_count,
            truncated: snapshot.truncated,
            decide_ms: round2(decide_ms),
            act_ms: 0.0,
            settle_ms: 0.0,
            via: via.clone(),
            kind: item.map(|it| it.kind.clone()).unwrap_or_default(),
            offered: items.iter().map(|it| it.text.clone()).collect(),
            label: item
                .and_then(|it| it.candidate.as_ref())
                .map(|c| c.label.clone())
                .unwrap_or_default(),
        };
        let how = if via == "keyword" {
            "keyword".to_string()
        } else {
            format!("p {:.2}", choice.p_top)
        };
        let suffix = if reason.is_empty() {
            String::new()
        } else {
            format!(" {reason}")
        };
        run.log_lines.push(clip(
            &format!("delegate step {n}: {} ({how}) {verdict}{suffix}", step.description),
            MAX_LOG_CHARS,
        ));
        run.steps.push(step);
        let si = run.steps.len() - 1;
        if verdict == "escalate" {
            if reason == "model_invalid" && !choice.error.is_empty() {
                run.log_lines
                    .push(clip(&format!("delegate model: {}", choice.error), MAX_LOG_CHARS));
            }
            return Ok(run.escalate(reason));
        }
        if verdict == "reobserve" {
            run.reobserves += 1;
            n -= 1; // a look is not a step
            (snapshot, snapshot_ms) = resnapshot!();
            continue;
        }
        let Some(item) = item.cloned() else {
            // gate() answers act/confirm only for an item it was handed
            return Ok(run.escalate("model_invalid"));
        };
        if verdict == "confirm" {
            let label = item
                .candidate
                .as_ref()
                .map(|c| c.label.clone())
                .unwrap_or_else(|| item.payload.clone());
            return Ok(run.confirm(&label, &item.kind));
        }
        if opts.dry_run {
            run.steps[si].verdict = "stopped".into();
            run.steps[si].reason = "dry_run".into();
            return Ok(run.stopped("dry_run", item.candidate.as_ref()));
        }

        // execute
        // an unreadable pid is unknown, not a mismatch
        let front = senses.frontmost_pid().unwrap_or(0);
        if front != 0 && snapshot.pid != 0 && front != snapshot.pid {
            run.steps[si].verdict = "escalate".into();
            run.steps[si].reason = "focus_changed".into();
            return Ok(run.escalate("focus_changed"));
        }
        let t0 = (run.clock)();
        let said = match (item.kind.as_str(), item.candidate.as_ref()) {
            ("click", Some(c)) => effectors.click_candidate(c),
            ("type", Some(c)) => effectors.focus_and_type(c, &item.payload),
            _ => effectors.press(&item.payload),
        };
        // an effector that raised applied nothing we can vouch for
        let said = said.unwrap_or_else(|e| {
            let (kind, message) = kind_and_message(&e);
            format!("refused: {kind}: {message}")
        });
        run.steps[si].act_ms = round2(((run.clock)() - t0) * 1000.0);
        if said.starts_with("refused:") {
            let reason = if item.kind != "type" {
                "hit_test_failed"
            } else {
                "focus_changed"
            };
            run.steps[si].verdict = "escalate".into();
            run.steps[si].reason = reason.into();
            run.log_lines.push(clip(&format!("delegate {said}"), MAX_LOG_CHARS));
            return Ok(run.escalate(reason));
        }
        run.applied += 1;
        run.last = action_text(&item);
        run.recent.push(run.last.clone());
        if item.kind == "click" {
            if let (Some(a), Some(c)) = (approve.as_deref(), item.candidate.as_ref()) {
                if casefold(&c.label) == casefold(a) {
                    run.approve_used = true;
                }
            }
        } else if item.kind == "type" {
            run.text_state = "applied".into();
            if item.candidate.as_ref().is_some_and(is_search_field) {
                run.search_typed = true;
            }
        } else {
            run.key_state = "applied".into();
        }
        run.previous = item.candidate.clone();

        // settle, re-snapshot, diff, oracles
        let t0 = (run.clock)();
        // a failed settle is a shorter wait, nothing more
        let _ = effectors.settle(thresholds.settle_secs);
        run.steps[si].settle_ms = round2(((run.clock)() - t0) * 1000.0);
        let (after, after_ms) = resnapshot!();
        snapshot_ms = after_ms;
        let mut changed = ax_changed(&snapshot, &after, item.candidate.as_ref());
        if changed.is_none() {
            // unknown is not unchanged
            changed = senses.screen_changed().unwrap_or(None);
        }
        run.steps[si].changed = changed;
        run.unchanged_streak = if changed == Some(false) {
            run.unchanged_streak + 1
        } else {
            0
        };
        snapshot = after;
        if !snapshot.app.is_empty() {
            run.app = snapshot.app.clone();
        }
        if snapshot.truncated {
            // Preserve the applied input, but incomplete post-action evidence cannot prove success.
            run.steps[si].reason = "truncated".into();
            return Ok(run.escalate("truncated"));
        }
        let via = done_check(&run, senses, &snapshot, &title0, visible0, true);
        if !via.is_empty() {
            return Ok(run.done(via));
        }
        if done_when.is_none() {
            return Ok(run.stopped("one_step", item.candidate.as_ref()));
        }
        if run.unchanged_streak >= 2 {
            return Ok(run.escalate("stalled_2"));
        }
    }
    Ok(run.escalate("step_cap"))
}

// ---- scripts: an ordered list of objectives ------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct ScriptResult {
    /// complete | partial | none (see the module docs)
    pub status: String,
    pub line: String,
    /// the `last=` text of every applied input, in order
    pub applied: Vec<String>,
    /// the label of every control clicked, in order
    pub labels: Vec<String>,
    /// the label of every control found already selected
    pub already: Vec<String>,
    pub results: Vec<DelegateResult>,
    pub log_lines: Vec<String>,
}

impl ScriptResult {
    pub fn complete(&self) -> bool {
        self.status == "complete"
    }

    fn new(status: &str, line: String) -> Self {
        ScriptResult {
            status: status.into(),
            line,
            applied: Vec::new(),
            labels: Vec::new(),
            already: Vec::new(),
            results: Vec::new(),
            log_lines: Vec::new(),
        }
    }
}

/// Python `_script_line`.
pub fn script_line(status: &str, applied: &[String], index: usize, total: usize, last_line: &str) -> String {
    format!(
        "script: {status}; applied=[{}]; step {index}/{total} {last_line}",
        applied.join(", ")
    )
}

/// run_script's keyword arguments, with Python's defaults (`Default`).
pub struct ScriptOptions<'a> {
    pub decide: String,
    pub require_cover: bool,
    pub cover_extra: Vec<String>,
    pub approve: Option<String>,
    pub dry_run: bool,
    pub thresholds: Thresholds,
    pub clock: Clock<'a>,
}

impl Default for ScriptOptions<'_> {
    fn default() -> Self {
        ScriptOptions {
            decide: DEFAULT_DECIDE.to_string(),
            require_cover: false,
            cover_extra: Vec::new(),
            approve: None,
            dry_run: false,
            thresholds: Thresholds::default(),
            clock: &crate::perf_counter,
        }
    }
}

/// Run `objectives` in order, one step each (the module docs have the contract).
///
/// Each step is its own run_delegate call with max_steps=1, so it gets a fresh snapshot and every gate; a step
/// whose control is already selected counts as satisfied with zero input. The script stops at the first step that
/// does not act or whose click changed nothing anyone could see.
pub fn run_script(
    objectives: &[String],
    senses: &dyn Senses,
    effectors: &dyn Effectors,
    decider: Option<&dyn LaneDecider>,
    opts: ScriptOptions<'_>,
) -> ScriptResult {
    // With propagate=false every step answers in its own line, so the Err arm only names the failure.
    script(objectives, senses, effectors, decider, opts, false).unwrap_or_else(|e| {
        let failed = failed_snapshot(&e);
        let line = script_line("none", &[], 0, objectives.len(), &failed.line);
        let mut out = ScriptResult::new("none", line.clone());
        out.log_lines = vec![clip(&format!("delegate {line}"), 160)];
        out.results = vec![failed];
        out
    })
}

/// run_script over [`try_run_delegate`]: a mid-run snapshot failure is an `Err`, as Python raised it.
pub fn try_run_script(
    objectives: &[String],
    senses: &dyn Senses,
    effectors: &dyn Effectors,
    decider: Option<&dyn LaneDecider>,
    opts: ScriptOptions<'_>,
) -> anyhow::Result<ScriptResult> {
    script(objectives, senses, effectors, decider, opts, true)
}

fn script(
    objectives: &[String],
    senses: &dyn Senses,
    effectors: &dyn Effectors,
    decider: Option<&dyn LaneDecider>,
    opts: ScriptOptions<'_>,
    propagate: bool,
) -> anyhow::Result<ScriptResult> {
    let steps: Vec<String> = objectives
        .iter()
        .map(|o| py_collapse(o))
        .filter(|s| !s.is_empty())
        .collect();
    let total = steps.len();
    let refuse = |reason: &str, total: usize| {
        let unavailable = format_line(
            LaneStatus::Unavailable,
            &LineFields {
                reason: reason.into(),
                ..LineFields::default()
            },
        );
        let line = script_line("none", &[], 0, total, &unavailable);
        let mut out = ScriptResult::new("none", line.clone());
        out.log_lines = vec![clip(&format!("delegate {line}"), MAX_LOG_CHARS)];
        out
    };
    if total == 0 {
        return Ok(refuse("a script needs at least one step", 0));
    }
    if total > MAX_SCRIPT_STEPS {
        return Ok(refuse(&format!("a script is at most {MAX_SCRIPT_STEPS} steps"), total));
    }
    let mut out = ScriptResult::new("none", String::new());
    let mut verified = true;
    for (i, objective) in steps.iter().enumerate() {
        let i = i + 1;
        let step_opts = DelegateOptions {
            decide: opts.decide.clone(),
            require_cover: opts.require_cover,
            cover_extra: opts.cover_extra.clone(),
            approve: opts.approve.clone(),
            max_steps: 1,
            dry_run: opts.dry_run,
            thresholds: opts.thresholds,
            clock: opts.clock,
            skip_if_selected: true,
            ..DelegateOptions::default()
        };
        let result = delegate(objective, senses, effectors, decider, step_opts, propagate)?;
        out.log_lines.extend(result.log_lines.iter().cloned());
        let last_step = result.steps.last();
        let acted = result.status == LaneStatus::Stopped && last_step.is_some_and(|s| s.verdict == "act");
        let satisfied = result.status == LaneStatus::Done;
        if acted {
            if let Some(step) = last_step {
                out.applied.push(step.description.clone());
                out.labels.push(if step.label.is_empty() {
                    step.description.clone()
                } else {
                    step.label.clone()
                });
                if step.changed != Some(true) {
                    // None is unknown, not proof: the planner looks
                    verified = false;
                }
            }
        } else if satisfied {
            let after = result
                .line
                .split_once("matched=\"")
                .map_or(result.line.as_str(), |(_, rest)| rest);
            out.already
                .push(after.split_once('"').map_or(after, |(head, _)| head).to_string());
        }
        let dead = acted && last_step.is_some_and(|s| s.changed == Some(false));
        let line = result.line.clone();
        out.results.push(result);
        if !(acted || satisfied) || dead {
            let status = if out.applied.is_empty() { "none" } else { "partial" };
            out.status = status.into();
            out.line = script_line(status, &out.applied, i, total, &line);
            out.log_lines.push(clip(&format!("delegate {}", out.line), 160));
            return Ok(out);
        }
    }
    let mut status = if verified { "complete" } else { "partial" };
    if out.applied.is_empty() && status == "partial" {
        status = "none";
    }
    out.status = status.into();
    let last_line = out.results.last().map(|r| r.line.clone()).unwrap_or_default();
    out.line = script_line(status, &out.applied, total, total, &last_line);
    out.log_lines.push(clip(&format!("delegate {}", out.line), 160));
    Ok(out)
}
