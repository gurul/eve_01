//! The desktop lanes: the code that clicks labelled controls on the owner's Mac without a planner turn.
//!
//! A module-for-module port of `bridge/src/eve_bridge`:
//!
//! | Python             | Rust                    |
//! |--------------------|-------------------------|
//! | ax_candidates.py   | [`ax_candidates`] (+ the macOS AX/AppKit layer in `ax_ffi`, and the `eve-ax-candidates` bin) |
//! | desktop_helpers.py | [`desktop_helpers`]     |
//! | fast_lane.py       | [`fast_lane`]           |
//! | lane_router.py     | [`lane_router`]         |
//! | decision_lane.py   | [`decision_lane`]       |
//! | plan_executor.py   | [`plan_executor`]       |
//! | task_router.py     | [`task_router`]         |
//!
//! Python passes one adapter object as both `senses=` and `effectors=`; here the lane traits
//! ([`fast_lane::Senses`], [`fast_lane::Effectors`], [`decision_lane::DecisionSenses`]) take `&self`, so the same
//! adapter can be lent as both. Implementors keep their state behind interior mutability.
//!
//! Where Python catches an exception and prints `type(e).__name__`, the Rust side carries a [`LaneError`] whose
//! `kind` is that name; [`error_kind`] reads it back from any `anyhow::Error`.

pub mod ax_candidates;
#[cfg(target_os = "macos")]
pub mod ax_ffi;
pub mod decision_lane;
pub mod desktop_helpers;
pub mod fast_lane;
pub mod lane_router;
pub mod plan_executor;
pub mod task_router;

use std::sync::LazyLock;
use std::time::Instant;

/// A Python exception crossing a lane boundary: `kind` is `type(e).__name__` ("RuntimeError", "ValueError",
/// "LookupError", ...), `message` is `str(e)`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct LaneError {
    pub kind: String,
    pub message: String,
}

impl LaneError {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        LaneError { kind: kind.into(), message: message.into() }
    }

    /// `LaneError::new(kind, message)` as an `anyhow::Error`.
    pub fn err(kind: impl Into<String>, message: impl Into<String>) -> anyhow::Error {
        anyhow::Error::new(Self::new(kind, message))
    }
}

/// `type(e).__name__` for an error: the [`LaneError`] kind anywhere in its chain, else "Exception".
pub fn error_kind(e: &anyhow::Error) -> String {
    e.chain()
        .find_map(|cause| cause.downcast_ref::<LaneError>())
        .map(|le| le.kind.clone())
        .unwrap_or_else(|| "Exception".to_string())
}

/// `str(e)` for an error: the [`LaneError`] message anywhere in its chain, else the error's own text.
pub fn error_message(e: &anyhow::Error) -> String {
    e.chain()
        .find_map(|cause| cause.downcast_ref::<LaneError>())
        .map(|le| le.message.clone())
        .unwrap_or_else(|| e.to_string())
}

static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

/// `time.perf_counter()`: seconds on a monotonic clock with an arbitrary origin.
pub fn perf_counter() -> f64 {
    EPOCH.elapsed().as_secs_f64()
}

/// A clock in seconds, injectable for the tests (Python's `clock=time.perf_counter`).
pub type Clock<'a> = &'a dyn Fn() -> f64;

/// Python's `str.isspace()` for one character: Unicode whitespace plus U+001C..U+001F.
pub fn py_is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python's `str.split()` with no separator.
pub fn py_split(text: &str) -> impl Iterator<Item = &str> {
    text.split(py_is_space).filter(|w| !w.is_empty())
}

/// Python's `" ".join(text.split())`.
pub fn py_collapse(text: &str) -> String {
    py_split(text).collect::<Vec<_>>().join(" ")
}

/// Python's `str.strip()` with no argument.
pub fn py_strip(text: &str) -> &str {
    text.trim_matches(py_is_space)
}

/// Python's `str.casefold()`, as far as eve's text needs it: `to_lowercase` plus the full case foldings that
/// lowercasing does not do (ß → ss, the ﬁ ligatures, ...).
pub fn casefold(text: &str) -> String {
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
            '\u{3c2}' => Some("\u{3c3}"),
            _ => None,
        };
        match folded {
            Some(s) => out.push_str(s),
            None => out.extend(c.to_lowercase()),
        }
    }
    out
}

/// Python's `round(x, n)` on a float: correctly rounded on the exact binary value, ties to even. Rust's
/// fixed-precision formatting rounds the same way, so the decimal it prints is the value Python's round returns.
pub fn py_round_n(x: f64, n: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.n$}").parse().unwrap_or(x)
}

/// Python's `len(text)`: characters (code points), not bytes.
pub fn py_len(text: &str) -> usize {
    text.chars().count()
}

/// Python's `text[:n]` on characters.
pub fn py_take(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}
