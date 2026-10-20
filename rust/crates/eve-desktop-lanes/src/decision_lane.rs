//! Opt-in bounded visual click loop. Decisions judges; existing code owns action admission. Port of
//! `decision_lane.py`.
//!
//! No generated coordinates, arbitrary scripts, typing, or blind repeats. Unknown action effects are carried to
//! Codex as partial progress even if a later sense or effector raises.

use indexmap::IndexMap;
use serde_json::{Map, Value};

use eve_decisions::{Answer, Evaluate};

use crate::ax_candidates::{Candidate, Snapshot};
use crate::fast_lane::Effectors;
use crate::lane_router::RouteResult;

/// Conservative experiment cut-offs, NOT calibrated Jev thresholds. Default stays off.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gates {
    pub target: f64,
    pub margin: f64,
    pub present: f64,
    pub covered: f64,
    pub risk_max: f64,
    pub verified: f64,
}

impl Default for Gates {
    fn default() -> Self {
        Gates { target: 0.95, margin: 0.5, present: 0.95, covered: 0.95, risk_max: 0.05, verified: 0.95 }
    }
}

impl Gates {
    /// The target id the answer clears every cut-off for, or "".
    pub fn pick(&self, answer: &Answer) -> String {
        todo!("decision_lane: Gates.pick")
    }
}

/// What the visual loop observes: AX and the screen together (the image in screen points), an image comparison,
/// and the frontmost pid. desktop_helpers::LaneAdapter implements it.
pub trait DecisionSenses {
    fn observe(&self) -> anyhow::Result<(Snapshot, image::DynamicImage)>;
    /// Any pixel change below the macOS menu bar invalidates a pending visual decision.
    fn image_matches(&self, before: &image::DynamicImage, after: &image::DynamicImage) -> bool;
    fn frontmost_pid(&self) -> anyhow::Result<i64>;
}

/// The evidence for one judgment. Rebuild the context allowlist from safe AX metadata rather than accepting
/// arbitrary context strings or editable values (including a secure field's focused label).
pub fn evidence(goal: &str, snapshot: &Snapshot) -> Map<String, Value> {
    todo!("decision_lane: evidence")
}

/// Share the existing filters: sensitive labels, disabled/secure fields, page content and settings values.
/// {candidate id: candidate} of the click options, in menu order; empty when anything relevant is withheld.
pub fn menu(goal: &str, snapshot: &Snapshot) -> IndexMap<String, Candidate> {
    todo!("decision_lane: menu")
}

/// IDs and values, not just titles: a re-render, moved element or new dialog invalidates the decision.
pub fn same_state(before: &Snapshot, after: &Snapshot) -> bool {
    todo!("decision_lane: same_state")
}

/// Python `_scores`: only bounded numeric/model status diagnostics; never echo observed/private text.
pub fn scores(answer: &Answer, selection: bool) -> String {
    todo!("decision_lane: _scores")
}

/// The visual loop over `goal`'s clauses. `apps` None scans lane_router::installed_app_names(). Never fails: an
/// error after input is partial progress, named in `reason` as `error:{kind}`.
pub fn route(
    goal: &str,
    senses: &dyn DecisionSenses,
    effectors: &dyn Effectors,
    client: &mut dyn Evaluate,
    apps: Option<&[String]>,
    gates: Gates,
    dry_run: bool,
) -> RouteResult {
    todo!("decision_lane: route")
}
