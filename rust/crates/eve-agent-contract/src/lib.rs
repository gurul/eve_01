//! What a computer-use agent tells its callers, as one small leaf crate.
//!
//! Port of `eve_bridge/agent_contract.py`. The voice session, the Telegram door,
//! the reflex wrapper and every agent (Codex's, the legacy planner in
//! `computer_agent`) speak [`AgentEvent`]. It lives here so none of them has to
//! pull in the legacy planner (and, through it, the lanes and Jev) to get a
//! three-field record. `computer_agent` re-exports it, so both paths name the
//! one definition.

use serde::{Deserialize, Serialize};

/// The kinds the agents emit. `kind` stays a free string, as in Python, so an
/// unknown kind passes through untouched; these name the documented set.
pub mod kind {
    pub const STARTED: &str = "started";
    pub const TURN: &str = "turn";
    pub const COMMENTARY: &str = "commentary";
    pub const EXEC: &str = "exec";
    pub const PROGRESS: &str = "progress";
    pub const ASK: &str = "ask";
    pub const FINAL: &str = "final";
    pub const CANCELLED: &str = "cancelled";
    pub const ERROR: &str = "error";

    /// Every documented kind, in the order the Python docstring lists them.
    pub const ALL: [&str; 9] = [
        STARTED, TURN, COMMENTARY, EXEC, PROGRESS, ASK, FINAL, CANCELLED, ERROR,
    ];
}

/// What the voice session and the board get told. kind: started | turn |
/// commentary | exec | progress | ask | final | cancelled | error.
///
/// Immutable like the Python `@dataclass(frozen=True)`: fields are read
/// through accessors and there are no setters. Equality and hashing are by
/// value over all three fields, as the frozen dataclass's `__eq__`/`__hash__`.
/// Serializes as `{"kind", "text", "turn"}` (the `dataclasses.asdict` shape),
/// and a missing `text` or `turn` reads as the Python defaults `""` and `0`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentEvent {
    kind: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    turn: i64,
}

impl AgentEvent {
    /// `AgentEvent(kind)`: text `""`, turn `0`.
    pub fn new(kind: impl Into<String>) -> Self {
        Self { kind: kind.into(), text: String::new(), turn: 0 }
    }

    /// `AgentEvent(kind, text)`: turn `0`.
    pub fn with_text(kind: impl Into<String>, text: impl Into<String>) -> Self {
        Self { kind: kind.into(), text: text.into(), turn: 0 }
    }

    /// `AgentEvent(kind, text, turn)`.
    pub fn with_turn(kind: impl Into<String>, text: impl Into<String>, turn: i64) -> Self {
        Self { kind: kind.into(), text: text.into(), turn }
    }

    pub fn kind(&self) -> &str {
        &self.kind
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn turn(&self) -> i64 {
        self.turn
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn defaults_match_the_dataclass() {
        let ev = AgentEvent::new("started");
        assert_eq!(ev.kind(), "started");
        assert_eq!(ev.text(), "");
        assert_eq!(ev.turn(), 0);
        assert_eq!(AgentEvent::with_text("progress", "opened gmail").turn(), 0);
    }

    #[test]
    fn positional_construction_like_chrome_lane_tests() {
        // test_chrome_lane.py: AgentEvent("started", goal), AgentEvent("progress", "opened gmail")
        let ev = AgentEvent::with_text(kind::PROGRESS, "opened gmail");
        assert_eq!(ev.kind(), "progress");
        assert_eq!(ev.text(), "opened gmail");
        let ev = AgentEvent::with_turn(kind::TURN, "thinking", 3);
        assert_eq!(ev.turn(), 3);
    }

    #[test]
    fn equality_and_hash_are_by_value() {
        let a = AgentEvent::with_turn("final", "done", 2);
        let b = AgentEvent::with_turn("final", "done", 2);
        assert_eq!(a, b);
        assert_ne!(a, AgentEvent::with_turn("final", "done", 1));
        assert_ne!(a, AgentEvent::with_turn("error", "done", 2));
        let set: HashSet<_> = [a.clone(), b].into_iter().collect();
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn unknown_kind_passes_through() {
        assert_eq!(AgentEvent::new("whatever").kind(), "whatever");
        assert!(!kind::ALL.contains(&"whatever"));
    }

    #[test]
    fn documented_kinds_in_docstring_order() {
        assert_eq!(
            kind::ALL,
            ["started", "turn", "commentary", "exec", "progress", "ask", "final", "cancelled", "error"]
        );
    }

    #[test]
    fn serializes_as_asdict() {
        let ev = AgentEvent::with_turn("ask", "which one?", 4);
        assert_eq!(
            serde_json::to_string(&ev).unwrap(),
            r#"{"kind":"ask","text":"which one?","turn":4}"#
        );
    }

    #[test]
    fn deserializes_with_defaults() {
        let ev: AgentEvent = serde_json::from_str(r#"{"kind":"cancelled"}"#).unwrap();
        assert_eq!(ev, AgentEvent::new("cancelled"));
        assert!(serde_json::from_str::<AgentEvent>(r#"{"text":"x"}"#).is_err());
    }
}
