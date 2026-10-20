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

use eve_decider::Choice;
use eve_typed_ask::{StepAnswer, StepGates, JEV_STEP_GATES};

use crate::ax_candidates::{Candidate, Snapshot};
use crate::Clock;

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
    "About", "Software Update", "Storage", "AppleCare & Warranty", "AirDrop & Continuity", "AutoFill & Passwords",
    "Date & Time", "Language & Region", "Sharing", "Startup Disk", "Time Machine",
];
/// Python: `\b(search|address|url|go to)\b`, IGNORECASE.
pub static SEARCH_FIELD_WORDS: LazyLock<regex::Regex> = LazyLock::new(|| todo!("fast_lane: SEARCH_FIELD_WORDS"));
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
        Thresholds { p_min: 0.60, margin_min: 0.15, max_options: 12, max_steps: 4, settle_secs: 1.0 }
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
    todo!("fast_lane: _q")
}

/// `7 radio button "Week"` — the id, role and full label of a candidate; "none" for None.
pub fn describe_pick(c: Option<&Candidate>) -> String {
    todo!("fast_lane: describe_pick")
}

/// Python `_action_text`: the `last=` form of an applied input — never the typed text, only its length.
pub fn action_text(item: &MenuItem) -> String {
    todo!("fast_lane: _action_text")
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
    todo!("fast_lane: format_line")
}

/// Python `_clip(text, limit=MAX_LOG_CHARS)`: whitespace collapsed, capped with "…".
pub fn clip(text: &str, limit: usize) -> String {
    todo!("fast_lane: _clip")
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
    todo!("fast_lane: gate")
}

// ---- helpers -------------------------------------------------------------------------------------

pub fn is_search_field(c: &Candidate) -> bool {
    todo!("fast_lane: is_search_field")
}

/// Python `_same_control`: same role, label and frame.
pub fn same_control(a: &Candidate, b: &Candidate) -> bool {
    todo!("fast_lane: _same_control")
}

/// Fields the lane may type into: never secure, never in a dialog, and page-content fields (a web form) only when
/// the caller allowed page content.
pub fn editable_pool(snapshot: &Snapshot, allow_page_links: bool) -> Vec<Candidate> {
    todo!("fast_lane: editable_pool")
}

/// Python `_shares_token`.
pub fn shares_token(c: &Candidate, tokens: &[String]) -> bool {
    todo!("fast_lane: _shares_token")
}

/// What the model is told besides the goal and the app: the window title, and the focused control when there is
/// one — nothing else.
pub fn lane_context(snapshot: &Snapshot) -> String {
    todo!("fast_lane: lane_context")
}

/// What a HOSTED model is told besides the step, the app and the menu: the focused control, and never the window
/// title — the one string most likely to be a document's name, a message's subject or a chat's.
pub fn jev_context(snapshot: &Snapshot) -> String {
    todo!("fast_lane: jev_context")
}

/// Python `_overlap`: how many objective tokens the control's label and value carry (the keyword gate's score).
pub fn overlap(c: &Candidate, tokens: &HashSet<String>) -> usize {
    todo!("fast_lane: _overlap")
}

/// The keyword gate, with its reason: (the pick, "") or (None, "tie" | "no_match"). The pick is the one click
/// option sharing the most objective tokens with its label and value, when that count is above zero and no other
/// click option ties it.
pub fn keyword_verdict<'m>(items: &'m [MenuItem], objective: &str) -> (Option<&'m MenuItem>, &'static str) {
    todo!("fast_lane: keyword_verdict")
}

/// The keyword gate's pick, or None on a tie or on zero overlap (see keyword_verdict).
pub fn keyword_pick<'m>(items: &'m [MenuItem], objective: &str) -> Option<&'m MenuItem> {
    todo!("fast_lane: keyword_pick")
}

/// The objective's words that the picked control does not account for: not in its label, its role, the
/// frontmost app's name, or `extra` (the caller's generic words). The value is left out on purpose: "is week
/// selected" must not read as covered by a selected Week.
pub fn uncovered_tokens(objective: &str, c: &Candidate, app: &str, extra: &[String]) -> Vec<String> {
    todo!("fast_lane: uncovered_tokens")
}

/// Python `_ax_changed`: the chosen control's value flipped, the focus moved, the title changed, the element count
/// or truncation changed. None when nothing in the tree says either way.
pub fn ax_changed(before: &Snapshot, after: &Snapshot, chosen: Option<&Candidate>) -> Option<bool> {
    todo!("fast_lane: _ax_changed")
}

/// Python `_jev_verdict`: (via, choice, stop) for one Jev answer. `stop` is "" to go on to the code gate,
/// "confirm", or an escalation reason: jev_error, jev_veto, jev_none.
pub fn jev_verdict(
    answer: &StepAnswer,
    keyword: Option<&MenuItem>,
    gates: &StepGates,
    options: &IndexMap<String, String>,
) -> (String, Choice, String) {
    todo!("fast_lane: _jev_verdict")
}

/// Python `_valid_key`: (normalised key, error). Combos (a "+", "command", "cmd", a space) are never offered.
pub fn valid_key(key: Option<&str>) -> (Option<String>, String) {
    todo!("fast_lane: _valid_key")
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
        todo!("fast_lane: _Run.__init__")
    }

    pub(crate) fn finish(&mut self, status: LaneStatus, line: String) -> DelegateResult {
        todo!("fast_lane: _Run.finish")
    }

    pub(crate) fn escalate(&mut self, reason: &str) -> DelegateResult {
        todo!("fast_lane: _Run.escalate")
    }

    pub(crate) fn confirm(&mut self, label: &str, action: &str) -> DelegateResult {
        todo!("fast_lane: _Run.confirm")
    }

    pub(crate) fn done(&mut self, via: &str) -> DelegateResult {
        todo!("fast_lane: _Run.done")
    }

    pub(crate) fn stopped(&mut self, reason: &str, pick: Option<&Candidate>) -> DelegateResult {
        todo!("fast_lane: _Run.stopped")
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
    todo!("fast_lane: _done_check")
}

/// Python `_timed_snapshot`: (snapshot, ms).
pub(crate) fn timed_snapshot(run: &Run<'_>, senses: &dyn Senses) -> anyhow::Result<(Snapshot, f64)> {
    todo!("fast_lane: _timed_snapshot")
}

/// Python `_build_menu`: (items, dropped_by_cap, blocked). `blocked` are the objective-relevant controls the lane
/// withholds — sensitive labels without approval, and non-navigation roles in System Settings — best match first,
/// so the caller can answer confirm for the best of them when nothing offered matches. Sets `run.top`.
pub(crate) fn build_menu(
    run: &mut Run<'_>,
    snapshot: &Snapshot,
    allow_page_links: bool,
) -> (Vec<MenuItem>, usize, Vec<Candidate>) {
    todo!("fast_lane: _build_menu")
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
/// under `step_gates`. Jev's `risky` noul can only ADD a confirm, and every code gate after the pick applies to its
/// pick too.
pub fn run_delegate(
    objective: &str,
    senses: &dyn Senses,
    effectors: &dyn Effectors,
    decider: Option<&dyn LaneDecider>,
    opts: DelegateOptions<'_>,
) -> DelegateResult {
    todo!("fast_lane: run_delegate")
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
}

/// Python `_script_line`.
pub fn script_line(status: &str, applied: &[String], index: usize, total: usize, last_line: &str) -> String {
    todo!("fast_lane: _script_line")
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
    todo!("fast_lane: run_script")
}
