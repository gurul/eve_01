use std::collections::{HashMap, HashSet};

use parking_lot::Mutex;

use super::*;

fn t(name: &str, bid: Option<&str>) -> ActivateTarget {
    (name.to_string(), bid.map(str::to_string))
}

#[test]
fn test_default_targets_without_env() {
    assert_eq!(activate_targets_from(""), default_activate_targets());
    assert_eq!(default_activate_targets().len(), 7);
    assert_eq!(default_activate_targets()[1], t("stable", Some("dev.warp.Warp-Stable")));
}

#[test]
fn test_env_overrides_order_and_keeps_known_bundle_ids() {
    assert_eq!(
        activate_targets_from("cmux, Warp, MyTerm"),
        vec![
            t("cmux", Some("com.cmuxterm.app")), // known → keeps its bundle id
            t("Warp", Some("dev.warp.Warp")),
            t("MyTerm", None), // unknown → name activation
        ]
    );
}

#[test]
fn test_env_matches_known_names_case_insensitively() {
    assert_eq!(
        activate_targets_from("warp,composer"),
        vec![t("Warp", Some("dev.warp.Warp")), t("Composer", None)]
    );
}

#[test]
fn test_env_blank_or_garbage_falls_back_to_defaults() {
    assert_eq!(activate_targets_from("  "), default_activate_targets());
    assert_eq!(activate_targets_from(", ,"), default_activate_targets());
}

// ---- focus_session_terminal decision flow ------------------------------------
//
// The scripts are stubbed at the two seams the module has — app_running
// (pgrep) and osascript — so each test states which apps are running and what
// System Events / the app would answer, then asserts what was raised. Scripts
// are identified by a distinctive substring.

const CWD: &str = "/Users/someone/code/claude-pet";

fn script_kind(script: &str) -> &'static str {
    if script.contains("tell application \"iTerm2\"") {
        return "iterm";
    }
    if script.contains("tell application \"Terminal\"") {
        return "terminal";
    }
    if script.contains("AXRaise") && script.contains("needle") {
        return "ax-raise";
    }
    if script.contains("AXMinimized") && script.contains("no-windows") {
        return "unminimize";
    }
    if script.contains("tell application id") {
        return "activate-id";
    }
    if script.contains("tell application (item 1 of argv)") {
        return "activate-name";
    }
    panic!("unknown script:\n{script}");
}

type Calls = Vec<(&'static str, Vec<String>)>;

/// running: process names pgrep would find. answers: kind -> reply.
struct Harness {
    running: HashSet<String>,
    answers: HashMap<&'static str, Option<String>>,
    /// When set, the ax-raise answer is "raised" only for this process.
    raise_only: Option<String>,
    calls: Mutex<Calls>,
}

impl Harness {
    fn new(running: &[&str], answers: &[(&'static str, Option<&str>)]) -> Self {
        Harness {
            running: running.iter().map(|s| s.to_string()).collect(),
            answers: answers.iter().map(|(k, v)| (*k, v.map(str::to_string))).collect(),
            raise_only: None,
            calls: Mutex::new(Vec::new()),
        }
    }

    fn kinds(&self) -> Vec<&'static str> {
        self.calls.lock().iter().map(|(k, _)| *k).collect()
    }

    fn args(&self, i: usize) -> Vec<String> {
        self.calls.lock()[i].1.clone()
    }
}

#[async_trait]
impl FocusBackend for Harness {
    async fn app_running(&self, name: &str) -> anyhow::Result<bool> {
        Ok(self.running.contains(name))
    }

    async fn osascript(&self, script: &str, args: &[&str]) -> anyhow::Result<Option<String>> {
        let kind = script_kind(script);
        self.calls.lock().push((kind, args.iter().map(|s| s.to_string()).collect()));
        if kind == "ax-raise" {
            if let Some(only) = &self.raise_only {
                return Ok(Some(if args[0] == only { "raised" } else { "no-match" }.to_string()));
            }
        }
        Ok(self.answers.get(kind).cloned().flatten())
    }
}

fn run(h: &Harness, cwd: &str) {
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    rt.block_on(focus_session_terminal_with(h, cwd, &default_activate_targets()));
}

fn run_case(running: &[&str], answers: &[(&'static str, Option<&str>)]) -> Harness {
    let h = Harness::new(running, answers);
    run(&h, CWD);
    h
}

#[test]
fn test_nothing_running_makes_no_osascript_calls() {
    let h = run_case(&[], &[]);
    assert!(h.calls.lock().is_empty());
}

#[test]
fn test_iterm_match_wins_and_stops() {
    let h = run_case(&["iTerm2", "stable"], &[("iterm", Some("matched"))]);
    assert_eq!(h.kinds(), vec!["iterm"]);
    assert_eq!(h.args(0), vec!["claude-pet"]);
}

#[test]
fn test_window_title_match_raises_that_window() {
    let h = run_case(&["stable"], &[("ax-raise", Some("raised"))]);
    assert_eq!(h.kinds(), vec!["ax-raise"]);
    assert_eq!(h.args(0), vec!["stable", "claude-pet"]);
}

#[test]
fn test_frontmost_with_visible_window_stands_down() {
    let h = run_case(
        &["stable"],
        &[("ax-raise", Some("frontmost")), ("unminimize", Some("visible"))],
    );
    assert_eq!(h.kinds(), vec!["ax-raise", "unminimize"]);
}

#[test]
fn test_frontmost_but_minimized_restores_the_window() {
    // The 2026-09-05 bug: Warp active, its only window in the Dock, every tap
    // logged "already frontmost — assuming it's on screen" and did nothing.
    let h = run_case(
        &["stable"],
        &[("ax-raise", Some("frontmost")), ("unminimize", Some("unminimized"))],
    );
    assert_eq!(h.kinds(), vec!["ax-raise", "unminimize"]);
    assert_eq!(h.args(1), vec!["stable"]);
}

#[test]
fn test_frontmost_with_no_window_on_this_space_activates_by_bundle_id() {
    let h = run_case(
        &["stable"],
        &[
            ("ax-raise", Some("frontmost")),
            ("unminimize", Some("no-windows")),
            ("activate-id", Some("activated")),
        ],
    );
    assert_eq!(h.kinds(), vec!["ax-raise", "unminimize", "activate-id"]);
    assert_eq!(h.args(2), vec!["dev.warp.Warp-Stable"]);
}

#[test]
fn test_frontmost_ax_error_still_activates() {
    let h = run_case(
        &["stable"],
        &[
            ("ax-raise", Some("frontmost")),
            ("unminimize", None),
            ("activate-id", Some("activated")),
        ],
    );
    assert_eq!(h.kinds(), vec!["ax-raise", "unminimize", "activate-id"]);
}

#[test]
fn test_frontmost_name_only_app_activates_by_name() {
    let h = run_case(
        &["Composer"],
        &[
            ("ax-raise", Some("frontmost")),
            ("unminimize", Some("no-windows")),
            ("activate-name", Some("activated")),
        ],
    );
    assert_eq!(h.kinds(), vec!["ax-raise", "unminimize", "activate-name"]);
    assert_eq!(h.args(2), vec!["Composer"]);
}

#[test]
fn test_not_frontmost_no_match_activates_first_running_and_unminimizes() {
    let h = run_case(
        &["stable", "cmux"],
        &[
            ("ax-raise", Some("no-match")),
            ("activate-id", Some("activated")),
            ("unminimize", Some("visible")),
        ],
    );
    // one AX pass per running app, then activate the first, then the Dock check
    assert_eq!(h.kinds(), vec!["ax-raise", "ax-raise", "activate-id", "unminimize"]);
    assert_eq!(h.args(2), vec!["dev.warp.Warp-Stable"]);
}

#[test]
fn test_second_app_title_match_beats_first_app_activate() {
    let mut h = Harness::new(&["stable", "cmux"], &[]);
    h.raise_only = Some("cmux".to_string());
    run(&h, CWD);
    assert_eq!(h.kinds(), vec!["ax-raise", "ax-raise"]);
}

#[test]
fn test_empty_cwd_skips_title_matching() {
    let h = Harness::new(
        &["stable"],
        &[("activate-id", Some("activated")), ("unminimize", Some("visible"))],
    );
    run(&h, "");
    assert_eq!(h.kinds(), vec!["activate-id", "unminimize"]);
}

// ---- extra coverage of the Python behavior ----------------------------------

#[test]
fn iterm_error_falls_through_to_terminal() {
    // _osascript returned None for iTerm2 → try Terminal next.
    let h = run_case(&["iTerm2", "Terminal"], &[("iterm", None), ("terminal", Some("matched"))]);
    assert_eq!(h.kinds(), vec!["iterm", "terminal"]);
    assert_eq!(h.args(1), vec!["claude-pet"]);
}

#[test]
fn empty_osascript_output_is_falsy() {
    // Python `if result:` treats "" as a miss, so iTerm2 answering "" moves on.
    let h = run_case(&["iTerm2", "stable"], &[("iterm", Some("")), ("ax-raise", Some("raised"))]);
    assert_eq!(h.kinds(), vec!["iterm", "ax-raise"]);
}

#[test]
fn empty_cwd_sends_tilde_to_iterm() {
    let h = Harness::new(&["iTerm2"], &[("iterm", Some("activated"))]);
    run(&h, "");
    assert_eq!(h.args(0), vec!["~"]);
}

#[test]
fn failed_activate_tries_the_next_running_app() {
    let h = Harness::new(&["stable", "Composer"], &[("activate-name", Some("activated"))]);
    run(&h, "");
    assert_eq!(h.kinds(), vec!["activate-id", "activate-name", "unminimize"]);
    assert_eq!(h.args(1), vec!["Composer"]);
}

#[test]
fn first_frontmost_app_is_the_one_kept() {
    let h = run_case(
        &["stable", "cmux"],
        &[("ax-raise", Some("frontmost")), ("unminimize", Some("visible"))],
    );
    assert_eq!(h.kinds(), vec!["ax-raise", "ax-raise", "unminimize"]);
    assert_eq!(h.args(2), vec!["stable"]);
}

struct Failing;

#[async_trait]
impl FocusBackend for Failing {
    async fn app_running(&self, _name: &str) -> anyhow::Result<bool> {
        anyhow::bail!("No such file or directory: 'pgrep'")
    }
    async fn osascript(&self, _s: &str, _a: &[&str]) -> anyhow::Result<Option<String>> {
        anyhow::bail!("unreachable")
    }
}

#[test]
fn backend_errors_are_non_fatal() {
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    rt.block_on(focus_session_terminal_with(&Failing, CWD, &default_activate_targets()));
}

#[test]
fn needle_is_the_cwd_basename() {
    assert_eq!(needle_for(CWD), "claude-pet");
    assert_eq!(needle_for("/a/b/repo/"), "repo");
    assert_eq!(needle_for("/a/b/repo///"), "repo");
    assert_eq!(needle_for("/"), "");
    assert_eq!(needle_for(""), "");
    assert_eq!(needle_for("repo"), "repo");
}

#[test]
fn py_repr_matches_python() {
    assert_eq!(py_repr("claude-pet"), "'claude-pet'");
    assert_eq!(py_repr(""), "''");
    assert_eq!(py_repr("it's"), "\"it's\"");
    assert_eq!(py_repr("a'b\"c"), "'a\\'b\"c'");
    assert_eq!(py_repr("a\\b\n"), "'a\\\\b\\n'");
}

#[test]
fn env_duplicates_and_unknown_case_are_kept() {
    assert_eq!(
        activate_targets_from("myterm,STABLE,stable"),
        vec![
            t("myterm", None),
            t("stable", Some("dev.warp.Warp-Stable")),
            t("stable", Some("dev.warp.Warp-Stable")),
        ]
    );
}

#[cfg(target_os = "macos")]
#[test]
fn real_pgrep_reports_a_missing_process() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let running = rt.block_on(app_running("eve-no-such-process-xyz")).unwrap();
    assert!(!running);
}
