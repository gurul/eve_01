//! Ports of bridge/tests/test_state.py, the State half of
//! bridge/tests/test_agent_rows.py, and the State behavior test_protocol.py
//! relies on (celebrate pulse, entries).

use super::*;
use parking_lot::Mutex;

fn fake_clock(start: f64) -> (State, Arc<Mutex<f64>>) {
    let t = Arc::new(Mutex::new(start));
    let c = t.clone();
    (State::with_clock(Arc::new(move || *c.lock())), t)
}

// ---- test_state.py ----

#[test]
fn test_session_lifecycle() {
    let mut s = State::new();
    s.session_start("a", Some("/tmp/a.jsonl"), Some("/tmp"));
    s.session_start("b", None, None);
    assert_eq!(s.total(), 2);
    s.session_end("a");
    assert_eq!(s.total(), 1);
    assert!(s.sessions.contains_key("b"));
}

#[test]
fn test_turn_running_count() {
    let mut s = State::new();
    s.session_start("x", None, None);
    s.session_start("y", None, None);
    s.turn_begin("x");
    assert_eq!(s.running_count(), 1);
    s.turn_begin("y");
    assert_eq!(s.running_count(), 2);
    s.turn_end("x");
    assert_eq!(s.running_count(), 1);
}

#[test]
fn test_entries_newest_first_and_capped() {
    let mut s = State::new();
    for i in 0..20 {
        s.add_entry(&format!("line {i}"), None);
    }
    assert_eq!(s.entries.len(), State::MAX_ENTRIES);
    // newest first
    assert_eq!(s.entries[0].text, "line 19");
}

#[test]
fn test_tokens_setter() {
    let mut s = State::new();
    s.set_tokens(123, 45, 0.0, 0.0);
    assert_eq!(s.tokens_cumulative, 123);
    assert_eq!(s.tokens_today, 45);
    // Cost params default to 0 when omitted
    assert_eq!(s.cost_cumulative, 0.0);
    assert_eq!(s.cost_today, 0.0);
}

#[test]
fn test_tokens_setter_with_cost() {
    let mut s = State::new();
    s.set_tokens(1000, 200, 12.34, 2.50);
    assert_eq!(s.cost_cumulative, 12.34);
    assert_eq!(s.cost_today, 2.50);
}

#[test]
fn test_attention_cwd_falls_back_to_newest_needs_input() {
    let (mut s, t) = fake_clock(100.0);
    s.session_start("a", None, Some("/repos/alpha"));
    s.session_start("b", None, Some("/repos/beta"));
    s.needs_input("a");
    *t.lock() = 101.0;
    s.needs_input("b"); // later flag wins
    assert_eq!(s.attention_cwd(), "/repos/beta");
}

#[test]
fn test_attention_cwd_falls_back_to_newest_needs_input_real_clock() {
    let mut s = State::new();
    s.session_start("a", None, Some("/repos/alpha"));
    s.session_start("b", None, Some("/repos/beta"));
    s.needs_input("a");
    s.needs_input("b"); // later (or equal) flag; ties keep the first, so force order
    s.sessions["a"].needs_input_at -= 1.0;
    assert_eq!(s.attention_cwd(), "/repos/beta");
}

#[test]
fn test_attention_cwd_empty_when_nothing_waits() {
    let mut s = State::new();
    s.session_start("a", None, Some("/repos/alpha"));
    assert_eq!(s.attention_cwd(), "");
    s.needs_input("a");
    s.input_received("a");
    assert_eq!(s.attention_cwd(), "");
}

#[test]
fn test_notification_kind_permission_prompt_waits() {
    assert!(notification_waits(Some("permission_prompt")));
}

#[test]
fn test_notification_kind_elicitation_dialog_waits() {
    assert!(notification_waits(Some("elicitation_dialog")));
}

#[test]
fn test_notification_kind_idle_prompt_does_not_wait() {
    assert!(!notification_waits(Some("idle_prompt")));
}

#[test]
fn test_notification_kind_idle_does_not_wait() {
    assert!(!notification_waits(Some("idle")));
}

#[test]
fn test_notification_kind_auth_success_does_not_wait() {
    assert!(!notification_waits(Some("auth_success")));
}

#[test]
fn test_notification_kind_elicitation_response_does_not_wait() {
    assert!(!notification_waits(Some("elicitation_response")));
}

#[test]
fn test_notification_kind_missing_or_empty_waits() {
    // No kind at all: keep the old, conservative behavior.
    assert!(notification_waits(None));
    assert!(notification_waits(Some("")));
}

#[test]
fn test_notification_kind_unknown_waits() {
    // A kind Claude Code adds later may be blocking: still get attention.
    assert!(notification_waits(Some("some_future_kind")));
}

#[test]
fn test_notification_kind_sets_are_exact_and_disjoint() {
    let waiting = ["permission_prompt", "elicitation_dialog"];
    let mut kinds = NON_WAITING_NOTIFICATION_KINDS.to_vec();
    kinds.sort();
    assert_eq!(kinds, vec!["auth_success", "elicitation_response", "idle", "idle_prompt"]);
    assert!(waiting.iter().all(|k| !NON_WAITING_NOTIFICATION_KINDS.contains(k)));
    assert!(waiting.iter().all(|k| notification_waits(Some(k))));
    assert!(!NON_WAITING_NOTIFICATION_KINDS.iter().any(|k| notification_waits(Some(k))));
}

#[test]
fn test_notification_kind_drives_waiting_count() {
    let mut s = State::new();
    s.session_start("x", None, None);
    for kind in NON_WAITING_NOTIFICATION_KINDS {
        if notification_waits(Some(kind)) {
            s.needs_input("x");
        }
        assert_eq!(s.waiting_count(), 0, "{kind}");
    }
    for kind in [Some("permission_prompt"), Some("elicitation_dialog"), None, Some("some_future_kind")] {
        s.input_received("x");
        assert_eq!(s.waiting_count(), 0);
        if notification_waits(kind) {
            s.needs_input("x");
        }
        assert_eq!(s.waiting_count(), 1, "{kind:?}");
    }
}

// ---- needs-input TTL and conjuring ----

#[test]
fn needs_input_expires_after_ttl() {
    let (mut s, t) = fake_clock(1000.0);
    s.session_start("x", None, Some("/r/x"));
    s.needs_input("x");
    assert_eq!(s.waiting_count(), 1);
    *t.lock() = 1000.0 + State::NEEDS_INPUT_TTL_SECS - 0.5;
    assert_eq!(s.waiting_count(), 1);
    assert_eq!(s.attention_cwd(), "/r/x");
    *t.lock() = 1000.0 + State::NEEDS_INPUT_TTL_SECS;
    assert_eq!(s.waiting_count(), 0);
    assert_eq!(s.attention_cwd(), "");
    assert_eq!(s.agent_rows(None)[0].s, "idle");
}

#[test]
fn needs_input_conjures_a_session_and_turn_begin_clears_it() {
    let mut s = State::new();
    s.needs_input("ghost");
    assert_eq!(s.total(), 1);
    assert_eq!(s.waiting_count(), 1);
    assert!(s.sessions["ghost"].started_at > 0.0);
    s.turn_begin("ghost");
    assert_eq!(s.waiting_count(), 0);
    assert!(s.sessions["ghost"].running);
}

#[test]
fn session_start_is_idempotent() {
    let mut s = State::new();
    s.session_start("a", None, Some("/one"));
    s.session_start("a", None, Some("/two"));
    assert_eq!(s.sessions["a"].cwd.as_deref(), Some("/one"));
}

#[test]
fn session_end_keeps_order_of_the_rest() {
    let mut s = State::new();
    for id in ["a", "b", "c"] {
        s.session_start(id, None, None);
    }
    s.session_end("a");
    s.session_end("missing");
    assert_eq!(s.sessions.keys().collect::<Vec<_>>(), ["b", "c"]);
}

#[test]
fn attention_cwd_ties_keep_the_first_and_missing_cwd_is_empty() {
    let (mut s, _t) = fake_clock(5.0);
    s.session_start("a", None, Some("/first"));
    s.session_start("b", None, Some("/second"));
    s.needs_input("a");
    s.needs_input("b");
    assert_eq!(s.attention_cwd(), "/first");
    let mut s2 = State::new();
    s2.needs_input("nocwd");
    assert_eq!(s2.attention_cwd(), "");
}

// ---- entries ----

#[test]
fn add_entry_strips_and_drops_blank() {
    let mut s = State::new();
    s.add_entry("   \n\t", None);
    s.add_entry("\u{1c}\u{1f}", None);
    assert!(s.entries.is_empty());
    s.add_entry("  hi there \n", Some(0.0));
    assert_eq!(s.entries[0], Entry { at: 0.0, text: "hi there".into() });
}

#[test]
fn add_entry_defaults_at_to_wall_clock() {
    let mut s = State::new();
    let before = wall_time();
    s.add_entry("x", None);
    assert!(s.entries[0].at >= before && s.entries[0].at <= wall_time());
}

// ---- celebrate pulse (test_protocol.py's heartbeat `completed`) ----

#[test]
fn not_celebrating_when_idle() {
    assert!(!State::new().is_celebrating());
}

#[test]
fn celebrating_during_pulse() {
    let mut s = State::new();
    s.pulse_completed(2.0);
    assert!(s.is_celebrating());
}

#[test]
fn pulse_already_expired() {
    let mut s = State::new();
    s.pulse_completed(-1.0); // already expired
    assert!(!s.is_celebrating());
}

#[test]
fn default_pulse_lasts_five_seconds() {
    let (mut s, t) = fake_clock(10.0);
    s.pulse_completed(State::DEFAULT_PULSE_SECS);
    *t.lock() = 14.9;
    assert!(s.is_celebrating());
    *t.lock() = 15.0;
    assert!(!s.is_celebrating());
}

// ---- test_agent_rows.py (State half) ----

fn row(n: &str, s: &str) -> AgentRow {
    AgentRow { n: n.into(), s: s.into(), t: None }
}

#[test]
fn test_row_names_come_from_cwd_basename() {
    let mut st = State::new();
    st.session_start("s1", None, Some("/Users/x/Documents/work/claude-pet"));
    assert_eq!(st.agent_rows(None), vec![row("claude-pet", "idle")]);
}

#[test]
fn test_trailing_slash_does_not_blank_the_name() {
    let mut st = State::new();
    st.session_start("s1", None, Some("/Users/x/work/era-hub-api/"));
    assert_eq!(st.agent_rows(None)[0].n, "era-hub-api");
}

#[test]
fn test_name_falls_back_to_session_prefix_without_cwd() {
    // A hook that carries no cwd must still produce a labelled row.
    let mut st = State::new();
    st.session_start("9f3fdfd2-aaaa", None, None);
    assert_eq!(st.agent_rows(None)[0].n, "9f3fdf");
}

#[test]
fn name_falls_back_for_root_cwd_and_to_question_mark() {
    let mut st = State::new();
    st.session_start("abc", None, Some("/"));
    st.session_start("", None, None);
    let names: Vec<_> = st.agent_rows(None).into_iter().map(|r| r.n).collect();
    assert_eq!(names, ["?", "abc"]);
}

#[test]
fn test_long_name_is_clipped_to_the_column_width() {
    let mut st = State::new();
    st.session_start("s1", None, Some("/tmp/a-really-long-repository-name"));
    assert_eq!(st.agent_rows(None)[0].n.chars().count(), State::AGENT_NAME_CHARS);
}

#[test]
fn clipping_counts_characters_not_bytes() {
    let mut st = State::new();
    st.session_start("s1", None, Some("/tmp/ééééééééééééééééé"));
    st.note_tool("s1", "ツールツールツールツールツール");
    let r = &st.agent_rows(None)[0];
    assert_eq!(r.n, "é".repeat(14));
    assert_eq!(r.t.as_deref(), Some("ツールツールツールツール"));
}

#[test]
fn test_states_and_current_tool() {
    let mut st = State::new();
    st.session_start("run", None, Some("/tmp/r"));
    st.session_start("idle", None, Some("/tmp/i"));
    st.turn_begin("run");
    st.note_tool("run", "Bash");
    let rows = st.agent_rows(None);
    let r = rows.iter().find(|r| r.n == "r").unwrap();
    let i = rows.iter().find(|r| r.n == "i").unwrap();
    assert_eq!(r.s, "run");
    assert_eq!(r.t.as_deref(), Some("Bash"));
    assert_eq!(i.s, "idle");
    // no tool yet → key omitted, not empty string
    assert_eq!(serde_json::to_value(i).unwrap(), serde_json::json!({"n": "i", "s": "idle"}));
    assert_eq!(
        serde_json::to_string(r).unwrap(),
        r#"{"n":"r","s":"run","t":"Bash"}"#
    );
}

#[test]
fn note_tool_ignores_empty_tool_name() {
    let mut st = State::new();
    st.session_start("s", None, None);
    st.note_tool("s", "Read");
    st.note_tool("s", "");
    assert_eq!(st.sessions["s"].last_tool, "Read");
}

#[test]
fn test_ordering_is_waiting_then_running_then_idle() {
    let mut st = State::new();
    st.session_start("i", None, Some("/tmp/zzz-idle"));
    st.session_start("r", None, Some("/tmp/aaa-run"));
    st.session_start("w", None, Some("/tmp/mmm-wait"));
    st.turn_begin("r");
    st.needs_input("w");
    let names: Vec<_> = st.agent_rows(None).into_iter().map(|r| r.n).collect();
    assert_eq!(names, ["mmm-wait", "aaa-run", "zzz-idle"]);
}

#[test]
fn test_ordering_is_stable_by_name_within_a_rank() {
    // Rank-then-name, never recency: a row that hops between refreshes costs
    // a full e-ink redraw and reads as flicker.
    let mut st = State::new();
    for cwd in ["/tmp/charlie", "/tmp/alpha", "/tmp/bravo"] {
        st.session_start(cwd, None, Some(cwd));
    }
    let names: Vec<_> = st.agent_rows(None).into_iter().map(|r| r.n).collect();
    assert_eq!(names, ["alpha", "bravo", "charlie"]);
}

#[test]
fn test_rows_are_capped() {
    let mut st = State::new();
    for i in 0..State::MAX_AGENT_ROWS + 4 {
        st.session_start(&format!("s{i}"), None, Some(&format!("/tmp/repo{i}")));
    }
    assert_eq!(st.agent_rows(None).len(), State::MAX_AGENT_ROWS);
    assert_eq!(st.agent_rows(Some(2)).len(), 2);
}

#[test]
fn test_note_tool_never_conjures_a_session() {
    // Unlike needs_input, a stray tool hook must not invent an agent row.
    let mut st = State::new();
    st.note_tool("ghost", "Bash");
    assert_eq!(st.agent_rows(None), vec![]);
    assert_eq!(st.total(), 0);
}
