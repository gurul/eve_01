//! In-memory aggregated state across all live Claude Code sessions.
//!
//! Port of `eve_bridge/state.py`.

use std::fmt;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use indexmap::IndexMap;
use serde::Serialize;

// Claude Code Notification kinds (the hook's `notification_type`):
// permission_prompt and elicitation_dialog mean a session is blocked on the
// human, and any kind not in NON_WAITING_NOTIFICATION_KINDS counts as waiting
// (notification_waits). Daemon log counts: idle_prompt 566,
// permission_prompt 155, auth_success 19, idle 2, elicitation_response 1. The
// owner's rule (firmware main.cpp derive()): Claude idle -> the pet sleeps; a
// session blocked on a permission -> attention. So an idle reminder must not
// light the attention pose over a live voice conversation.
/// Kinds that are news, not a request: they do not mark the session waiting.
pub const NON_WAITING_NOTIFICATION_KINDS: [&str; 4] =
    ["idle_prompt", "idle", "auth_success", "elicitation_response"];

/// True when a Notification of this kind means Claude waits on the human.
///
/// A missing or unknown kind counts as waiting — the old behavior, kept so a
/// new blocking kind from Claude Code still gets the human's attention.
pub fn notification_waits(kind: Option<&str>) -> bool {
    match kind {
        None | Some("") => true,
        Some(k) => !NON_WAITING_NOTIFICATION_KINDS.contains(&k),
    }
}

/// A clock in seconds, `time.monotonic` by default.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

/// `time.monotonic()`: seconds on the system's monotonic clock (on macOS the
/// one `mach_absolute_time` reads, as Python's). Never 0.0 in practice, which
/// matters because 0.0 is the "not waiting" sentinel of `needs_input_at`.
pub fn monotonic() -> f64 {
    #[cfg(target_vendor = "apple")]
    let clock = libc::CLOCK_UPTIME_RAW;
    #[cfg(not(target_vendor = "apple"))]
    let clock = libc::CLOCK_MONOTONIC;
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a valid out-parameter for the call.
    unsafe { libc::clock_gettime(clock, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9
}

/// `time.time()`: wall-clock epoch seconds as a float.
pub fn wall_time() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub session_id: String,
    pub started_at: f64,
    pub transcript_path: Option<String>,
    pub cwd: Option<String>,
    pub running: bool,
    /// Set by the Notification hook when Claude is blocked on the user
    /// (permission prompt in the terminal, idle waiting for input). Drives
    /// the firmware's attention animation via the heartbeat's `waiting`
    /// count. 0.0 = not waiting. Cleared by turn_begin/posttooluse.
    /// Monotonic seconds.
    pub needs_input_at: f64,
    /// Most recent tool this session touched, for the monitor's per-agent row.
    /// Set on pre/post tool use; purely cosmetic, so a miss just blanks the
    /// column rather than mis-stating the session's state.
    pub last_tool: String,
}

impl Session {
    /// `Session(session_id=..., started_at=...)` with the dataclass defaults.
    pub fn new(session_id: impl Into<String>, started_at: f64) -> Self {
        Session {
            session_id: session_id.into(),
            started_at,
            transcript_path: None,
            cwd: None,
            running: false,
            needs_input_at: 0.0,
            last_tool: String::new(),
        }
    }
}

/// One transcript-style line surfaced on the stick.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// wall-clock epoch seconds (for formatting HH:MM)
    pub at: f64,
    pub text: String,
}

/// One compact monitor row: `{"n": name, "s": status, "t"?: tool}`.
///
/// Keys are single letters because this rides in every heartbeat; `t` is
/// omitted (not empty) when the session has touched no tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentRow {
    pub n: String,
    pub s: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub t: Option<String>,
}

/// Aggregator. Hook handlers mutate this via the public methods below.
pub struct State {
    /// Insertion-ordered, like the Python dict.
    pub sessions: IndexMap<String, Session>,
    /// Newest first.
    pub entries: Vec<Entry>,
    pub tokens_cumulative: i64,
    pub tokens_today: i64,
    /// USD cost estimates derived from the same usage records that feed
    /// tokens_*. See pricing.py. Displayed in the statusline.
    pub cost_cumulative: f64,
    pub cost_today: f64,
    /// Monotonic timestamp until which heartbeats should carry
    /// `completed: true` — the firmware's celebrate-animation trigger.
    /// Pulsed by turn_end, observed by build_heartbeat.
    pub completed_until: f64,
    clock: Clock,
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("State")
            .field("sessions", &self.sessions)
            .field("entries", &self.entries)
            .field("tokens_cumulative", &self.tokens_cumulative)
            .field("tokens_today", &self.tokens_today)
            .field("cost_cumulative", &self.cost_cumulative)
            .field("cost_today", &self.cost_today)
            .field("completed_until", &self.completed_until)
            .finish()
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

/// Python's `str.strip()` with no argument: Unicode whitespace plus the
/// information separators U+001C..U+001F, which `str.isspace` also counts.
fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

/// `s[:n]` on a Python str: the first `n` code points.
fn py_prefix(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// `os.path.basename(p)`: everything after the last `/`.
fn basename(p: &str) -> &str {
    match p.rfind('/') {
        Some(i) => &p[i + 1..],
        None => p,
    }
}

impl State {
    /// stick's display can only hold a few
    pub const MAX_ENTRIES: usize = 8;

    /// Auto-expire so a notification for an abandoned session doesn't leave
    /// the pet impatient forever.
    pub const NEEDS_INPUT_TTL_SECS: f64 = (15 * 60) as f64;

    /// How many rows the landscape monitor can show. The firmware clips to the
    /// same number; sending more would only burn heartbeat bytes.
    pub const MAX_AGENT_ROWS: usize = 6;

    /// Display width of the name column on the panel, in characters.
    pub const AGENT_NAME_CHARS: usize = 14;

    /// `pulse_completed()`'s default duration.
    pub const DEFAULT_PULSE_SECS: f64 = 5.0;

    pub fn new() -> Self {
        Self::with_clock(Arc::new(monotonic))
    }

    /// A State whose monotonic reads (needs-input TTL, celebrate pulse) come
    /// from `clock` — the seam tests use where Python patched `time.monotonic`.
    pub fn with_clock(clock: Clock) -> Self {
        State {
            sessions: IndexMap::new(),
            entries: Vec::new(),
            tokens_cumulative: 0,
            tokens_today: 0,
            cost_cumulative: 0.0,
            cost_today: 0.0,
            completed_until: 0.0,
            clock,
        }
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    // ---- session lifecycle ----

    pub fn session_start(&mut self, session_id: &str, transcript_path: Option<&str>, cwd: Option<&str>) {
        if !self.sessions.contains_key(session_id) {
            let mut s = Session::new(session_id, wall_time());
            s.transcript_path = transcript_path.map(str::to_owned);
            s.cwd = cwd.map(str::to_owned);
            self.sessions.insert(session_id.to_owned(), s);
        }
    }

    pub fn session_end(&mut self, session_id: &str) {
        // `dict.pop` keeps the order of the remaining sessions.
        self.sessions.shift_remove(session_id);
    }

    // ---- turn lifecycle ----

    pub fn turn_begin(&mut self, session_id: &str) {
        if let Some(s) = self.sessions.get_mut(session_id) {
            s.running = true;
            s.needs_input_at = 0.0; // user responded — attention satisfied
        }
    }

    pub fn turn_end(&mut self, session_id: &str) {
        if let Some(s) = self.sessions.get_mut(session_id) {
            s.running = false;
        }
    }

    // ---- needs-input (Notification hook) ----

    pub fn needs_input(&mut self, session_id: &str) {
        let now = self.now();
        let s = self
            .sessions
            .entry(session_id.to_owned())
            .or_insert_with(|| Session::new(session_id, wall_time()));
        s.needs_input_at = now;
    }

    /// Record the tool a session most recently touched (monitor display).
    ///
    /// Silently ignores unknown sessions — unlike needs_input this must not
    /// conjure a Session, or a stray hook would invent an agent row for a
    /// session that never started.
    pub fn note_tool(&mut self, session_id: &str, tool_name: &str) {
        if let Some(s) = self.sessions.get_mut(session_id) {
            if !tool_name.is_empty() {
                s.last_tool = tool_name.to_owned();
            }
        }
    }

    pub fn input_received(&mut self, session_id: &str) {
        if let Some(s) = self.sessions.get_mut(session_id) {
            s.needs_input_at = 0.0;
        }
    }

    fn needs_input_live_at(s: &Session, now: f64) -> bool {
        s.needs_input_at > 0.0 && (now - s.needs_input_at) < Self::NEEDS_INPUT_TTL_SECS
    }

    /// `_needs_input_live`: flagged, and the flag is younger than the TTL.
    pub fn needs_input_live(&self, s: &Session) -> bool {
        Self::needs_input_live_at(s, self.now())
    }

    /// The cwd behind the pet's attention state — for tap-the-pet focus.
    ///
    /// The most recently flagged needs-input session (Claude idle, waiting
    /// on a reply in the terminal). Empty when nothing is waiting — focus
    /// then degrades to raising whichever terminal app is running.
    pub fn attention_cwd(&self) -> String {
        let mut newest: Option<&Session> = None;
        for s in self.sessions.values().filter(|s| self.needs_input_live(s)) {
            // `max` keeps the first of equal keys.
            if newest.is_none_or(|n| s.needs_input_at > n.needs_input_at) {
                newest = Some(s);
            }
        }
        newest.and_then(|s| s.cwd.clone()).unwrap_or_default()
    }

    // ---- celebrate pulse ----

    /// Make subsequent heartbeats include `completed: true` for a few
    /// seconds — firmware reads that field as the celebrate-animation
    /// trigger (data.h:_applyJson → main.cpp:derive). Python's default
    /// duration is [`State::DEFAULT_PULSE_SECS`].
    pub fn pulse_completed(&mut self, duration_secs: f64) {
        self.completed_until = self.now() + duration_secs;
    }

    pub fn is_celebrating(&self) -> bool {
        self.now() < self.completed_until
    }

    // ---- entries ----

    /// Prepend a stripped, non-empty line; `at` defaults to `time.time()`.
    pub fn add_entry(&mut self, text: &str, at: Option<f64>) {
        let text = py_strip(text);
        if text.is_empty() {
            return;
        }
        self.entries.insert(
            0,
            Entry { at: at.unwrap_or_else(wall_time), text: text.to_owned() },
        );
        self.entries.truncate(Self::MAX_ENTRIES);
    }

    // ---- tokens ----

    /// Called by the JSONL tailer after recomputing from transcript files.
    /// Python's cost parameters default to 0.0.
    pub fn set_tokens(&mut self, cumulative: i64, today: i64, cost_cumulative: f64, cost_today: f64) {
        self.tokens_cumulative = cumulative;
        self.tokens_today = today;
        self.cost_cumulative = cost_cumulative;
        self.cost_today = cost_today;
    }

    // ---- aggregates ----

    pub fn total(&self) -> usize {
        self.sessions.len()
    }

    pub fn running_count(&self) -> usize {
        self.sessions.values().filter(|s| s.running).count()
    }

    pub fn waiting_count(&self) -> usize {
        let now = self.now();
        self.sessions.values().filter(|s| Self::needs_input_live_at(s, now)).count()
    }

    // ---- per-agent rows (monitor display) ----

    /// One compact row per live session for the monitor's agent list.
    ///
    /// Keys are single letters because this rides in every heartbeat and the
    /// firmware's line buffer is 2KB shared with a 720-byte prompt detail.
    ///
    /// Ordering is (rank, name) — deterministic rather than recency-based, so
    /// a row doesn't hop between refreshes. On e-ink a reordered list costs a
    /// full redraw and reads as flicker, and "which agent is where" is worth
    /// more than "which moved last".
    pub fn agent_rows(&self, limit: Option<usize>) -> Vec<AgentRow> {
        let cap = limit.unwrap_or(Self::MAX_AGENT_ROWS);
        let now = self.now();
        let mut rows: Vec<(u8, String, AgentRow)> = Vec::with_capacity(self.sessions.len());
        for s in self.sessions.values() {
            let cwd = s.cwd.as_deref().unwrap_or("").trim_end_matches('/');
            let mut name = basename(cwd);
            if name.is_empty() {
                name = py_prefix(&s.session_id, 6);
            }
            if name.is_empty() {
                name = "?";
            }
            let name = py_prefix(name, Self::AGENT_NAME_CHARS).to_owned();
            let (rank, status) = if Self::needs_input_live_at(s, now) {
                (0, "wait")
            } else if s.running {
                (1, "run")
            } else {
                (2, "idle")
            };
            let tool = &s.last_tool;
            let row = AgentRow {
                n: name.clone(),
                s: status.to_owned(),
                t: (!tool.is_empty()).then(|| py_prefix(tool, 12).to_owned()),
            };
            rows.push((rank, name, row));
        }
        // Python compares str by code point, as Rust's `str` Ord does; the
        // sort is stable in both.
        rows.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        rows.into_iter().take(cap).map(|r| r.2).collect()
    }
}

#[cfg(test)]
mod tests;
