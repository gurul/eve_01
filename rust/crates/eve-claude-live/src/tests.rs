//! Port of bridge/tests/test_claude_live.py: a Claude session whose process is gone leaves the "claude on"
//! picker, and a doubt keeps it.

use super::*;
use eve_state::State;
use parking_lot::Mutex;
use std::sync::Arc;

const PS_OUT: &str = "  1524 /Users/g/.local/bin/claude --chrome-native-host
  2643 claude --dangerously-skip-permissions
  3001 /bin/zsh -l
  4100 node /opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/cli.js
  5000 /usr/bin/python3 claude.py
";

#[derive(Default)]
struct FakeRun {
    ps: String,
    lsof: String,
    ps_code: i32,
    lsof_code: i32,
    fail: String,
    calls: Mutex<Vec<Vec<String>>>,
}

impl FakeRun {
    fn new(lsof: &str) -> Self {
        FakeRun { ps: PS_OUT.to_string(), lsof: lsof.to_string(), ..Default::default() }
    }

    fn run(&self, argv: &[String], _timeout: Duration) -> Result<RunOutput, RunError> {
        self.calls.lock().push(argv.to_vec());
        let name = basename(&argv[0]);
        if name == self.fail {
            return Err(RunError::Os(std::io::Error::new(std::io::ErrorKind::NotFound, argv[0].clone())));
        }
        if name == "ps" {
            return Ok(RunOutput { returncode: self.ps_code, stdout: self.ps.clone() });
        }
        Ok(RunOutput { returncode: self.lsof_code, stdout: self.lsof.clone() })
    }
}

fn cwds(fake: &FakeRun) -> Option<HashSet<String>> {
    live_cwds(&|a: &[String], t: Duration| fake.run(a, t))
}

fn set(items: &[&str]) -> HashSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn the_claude_processes_and_their_folders_are_read() {
    let fake = FakeRun::new("p1524\nfcwd\nn/Users/g/.claude/chrome\np2643\nfcwd\nn/Users/g/repo\n");
    assert_eq!(cwds(&fake), Some(set(&["/Users/g/.claude/chrome", "/Users/g/repo"])));
    // only Claude Code: the native binary by any path and the npm install; never a shell or a claude.py
    let calls = fake.calls.lock();
    assert_eq!(calls[1].last().unwrap(), "1524,2643,4100");
    assert_eq!(calls[0], vec!["/bin/ps", "-axww", "-o", "pid=,args="]);
    assert_eq!(
        calls[1],
        vec!["/usr/sbin/lsof", "-a", "-b", "-w", "-d", "cwd", "-Fn", "-p", "1524,2643,4100"]
    );
}

#[test]
fn a_pid_gone_since_ps_still_gives_the_rest() {
    let fake = FakeRun { lsof_code: 1, ..FakeRun::new("p2643\nfcwd\nn/Users/g/repo\n") };
    assert_eq!(cwds(&fake), Some(set(&["/Users/g/repo"])));
}

#[test]
fn a_probe_that_cannot_tell_says_so() {
    assert_eq!(cwds(&FakeRun { fail: "ps".into(), ..FakeRun::new("") }), None);
    assert_eq!(cwds(&FakeRun { fail: "lsof".into(), ..FakeRun::new("") }), None);
    assert_eq!(cwds(&FakeRun { ps_code: 1, ..FakeRun::new("") }), None);
    // no claude at all: blind
    assert_eq!(cwds(&FakeRun { ps: "  3001 /bin/zsh -l\n".into(), ..FakeRun::new("") }), None);
    assert_eq!(cwds(&FakeRun::new("")), None);

    let slow = |argv: &[String], timeout: Duration| -> Result<RunOutput, RunError> {
        Err(RunError::Timeout { argv: argv.to_vec(), timeout })
    };
    assert_eq!(live_cwds(&slow), None);
}

#[test]
fn a_session_in_a_subfolder_of_a_live_process_is_alive() {
    let tmp = tempfile::tempdir().unwrap();
    let tmp_path = tmp.path();
    let repo = tmp_path.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    let s = |p: &Path| p.to_string_lossy().into_owned();
    let live = set(&[&s(&repo)]);
    assert!(alive(&s(&repo), &live));
    assert!(alive(&s(&repo.join("src")), &live)); // the session's shell moved in
    assert!(!alive(&s(tmp_path), &live)); // a folder above is another session
    assert!(!alive(&s(&tmp_path.join("other")), &live));
}

fn state_at(now: f64) -> State {
    let mut state = State::new();
    for (sid, cwd, age) in [
        ("dead", Some("/Users/g/old"), 3600.0),
        ("live", Some("/Users/g/repo"), 600.0),
        ("new", Some("/Users/g/fresh"), 5.0),
        ("asks", Some("/Users/g/asks"), 900.0),
        ("bare", None, 900.0),
    ] {
        state.session_start(sid, None, cwd);
        state.sessions.get_mut(sid).unwrap().started_at = now - age;
    }
    state
}

fn ids(state: &State) -> HashSet<String> {
    state.sessions.keys().cloned().collect()
}

#[test]
fn the_picker_drops_dead_sessions_and_forgets_them() {
    let now = 1_800_000_000.0;
    let mut state = state_at(now);
    let got = picker_sessions(&mut state, Some(set(&["/Users/g/repo"])), None, Some(now), &[]);
    // newest first; the new one (grace) is kept whatever the probe says
    assert_eq!(got, vec!["/Users/g/fresh", "/Users/g/repo"]);
    assert!(!state.sessions.contains_key("dead"));
    assert!(!state.sessions.contains_key("asks"));
    assert!(state.sessions.contains_key("bare"));
}

#[test]
fn a_blind_probe_keeps_every_session() {
    let now = 1_800_000_000.0;
    let mut state = state_at(now);
    let blind = || None;
    let got = picker_sessions(&mut state, None, Some(&blind), Some(now), &[]);
    assert_eq!(got, vec!["/Users/g/fresh", "/Users/g/repo", "/Users/g/asks", "/Users/g/old"]);
    assert_eq!(ids(&state), set(&["dead", "live", "new", "asks", "bare"]));
}

/// The Python test drives Daemon._make_telegram; the part of it that is this module's behavior is that a
/// probe reporting only the live folder leaves only the live session, here and in the state.
#[tokio::test]
async fn the_daemon_lends_the_live_list_to_the_inlet() {
    let mut state = State::new();
    state.session_start("dead", None, Some("/Users/g/old"));
    state.session_start("live", None, Some("/Users/g/repo"));
    state.sessions.get_mut("dead").unwrap().started_at -= 3600.0;
    state.sessions.get_mut("live").unwrap().started_at -= 600.0; // past the grace: kept by the probe alone
    let got = picker_sessions_off_loop_with(&mut state, None, || {
        Some(IndexMap::from([("/Users/g/repo".to_string(), true)]))
    })
    .await;
    assert_eq!(got, vec!["/Users/g/repo"]);
    assert_eq!(ids(&state), set(&["live"]));
}

#[test]
fn a_session_run_through_the_npm_bin_shim_is_a_claude_process() {
    assert!(is_claude("node /opt/homebrew/bin/claude --resume"));
    assert!(is_claude("/usr/local/bin/bun /Users/g/.bun/bin/claude"));
    assert!(!is_claude("node /opt/homebrew/bin/claude-mem worker")); // the control
    assert!(!is_claude("node server.js claude"));
    assert!(!is_claude(""));
    assert!(!is_claude("   "));
}

#[tokio::test(flavor = "current_thread")]
async fn the_probe_runs_off_the_event_loop_and_the_pruning_on_it() {
    let mut state = State::new();
    state.session_start("dead", None, Some("/Users/g/old"));
    state.session_start("live", None, Some("/Users/g/repo"));
    state.sessions.get_mut("dead").unwrap().started_at -= 3600.0;
    state.sessions.get_mut("live").unwrap().started_at -= 600.0;
    let threads: Arc<Mutex<Vec<std::thread::ThreadId>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = threads.clone();
    let probe: BoxedProbe = Box::new(move || {
        seen.lock().push(std::thread::current().id());
        Some(set(&["/Users/g/repo"]))
    });
    let got = picker_sessions_off_loop(&mut state, Some(probe)).await;
    assert_eq!(got, vec!["/Users/g/repo"]);
    let threads = threads.lock();
    assert!(!threads.is_empty());
    assert_ne!(threads[0], std::thread::current().id());
    assert_eq!(ids(&state), set(&["live"]));
}

#[test]
fn each_folder_says_whether_an_owner_can_type_there() {
    let ps = format!("{PS_OUT}  6100 /Users/g/.local/bin/claude --output-format stream-json --verbose\n");
    let fake = FakeRun {
        ps,
        ..FakeRun::new(
            "p1524\nfcwd\nn/Users/g/.claude/chrome\np2643\nfcwd\nn/Users/g/repo\np6100\nfcwd\nn/Users/g/.claude-mem/observer\n",
        )
    };
    let got = live_processes(&|a: &[String], t: Duration| fake.run(a, t)).unwrap();
    let want = IndexMap::from([
        ("/Users/g/.claude/chrome".to_string(), false),
        ("/Users/g/repo".to_string(), true),
        ("/Users/g/.claude-mem/observer".to_string(), false),
    ]);
    assert_eq!(got, want);
    assert!(interactive("claude --dangerously-skip-permissions --resume"));
    assert!(!interactive("claude -p hello"));
    assert!(!interactive("claude --output-format=stream-json"));
}

#[tokio::test]
async fn after_a_restart_claude_on_still_finds_the_open_session() {
    let mut state = State::new();
    let got = picker_sessions_off_loop_with(&mut state, None, || {
        Some(IndexMap::from([
            ("/Users/g/buddy".to_string(), true),
            ("/Users/g/.claude/chrome".to_string(), false),
        ]))
    })
    .await;
    assert_eq!(got, vec!["/Users/g/buddy"]);
}

#[tokio::test]
async fn a_known_session_is_not_offered_twice() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let other = tmp.path().join("other");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    let s = |p: &Path| p.to_string_lossy().into_owned();
    let mut state = State::new();
    state.session_start("known", None, Some(&s(&repo.join("src")))); // its shell moved into a subfolder
    let (r, o) = (s(&repo), s(&other));
    let got = picker_sessions_off_loop_with(&mut state, None, move || {
        Some(IndexMap::from([(r, true), (o, true)]))
    })
    .await;
    assert_eq!(got, vec![s(&repo.join("src")), s(&other)]);
}

#[tokio::test]
async fn a_blind_probe_offers_only_what_the_hooks_told() {
    let mut state = State::new();
    state.session_start("known", None, Some("/Users/g/repo"));
    let got = picker_sessions_off_loop_with(&mut state, None, || None).await;
    assert_eq!(got, vec!["/Users/g/repo"]);
}

// ---- beyond the Python file: the pieces the Python got from the standard library ----

#[test]
fn a_blind_boxed_probe_keeps_every_session() {
    let now = wall_time();
    let mut state = state_at(now);
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let got = rt.block_on(picker_sessions_off_loop(&mut state, Some(Box::new(|| None))));
    assert_eq!(got, vec!["/Users/g/fresh", "/Users/g/repo", "/Users/g/asks", "/Users/g/old"]);
    assert_eq!(state.sessions.len(), 5);
}

#[test]
fn equal_start_times_keep_the_state_order() {
    let mut state = State::new();
    for (sid, cwd) in [("a", "/a"), ("b", "/b"), ("c", "/c")] {
        state.session_start(sid, None, Some(cwd));
        state.sessions.get_mut(sid).unwrap().started_at = 100.0;
    }
    let blind = || None;
    assert_eq!(picker_sessions(&mut state, None, Some(&blind), Some(200.0), &[]), vec!["/a", "/b", "/c"]);
}

#[test]
fn an_empty_cwd_is_never_in_the_picker() {
    let mut state = State::new();
    state.session_start("empty", None, Some(""));
    state.sessions.get_mut("empty").unwrap().started_at = 0.0;
    let got = picker_sessions(&mut state, Some(HashSet::new()), None, Some(1000.0), &[]);
    assert!(got.is_empty());
    assert!(state.sessions.contains_key("empty")); // nothing to judge by: not dropped
}

#[test]
fn unknown_folders_follow_in_sorted_order() {
    let mut state = State::new();
    let unknown = vec!["/z".to_string(), "/a".to_string(), "/a/b".to_string()];
    let got = picker_sessions(&mut state, Some(set(&["/z", "/a"])), None, Some(0.0), &unknown);
    // a kept folder counts only when it is in or under the process's folder: "/a" is above "/a/b"
    assert_eq!(got, vec!["/a", "/a/b", "/z"]);
    // a known session in a subfolder of the process's folder covers it
    let mut state = State::new();
    state.session_start("x", None, Some("/a/b"));
    state.sessions.get_mut("x").unwrap().started_at = 0.0;
    let got = picker_sessions(&mut state, Some(set(&["/a"])), None, Some(1000.0), &["/a".to_string()]);
    assert_eq!(got, vec!["/a/b"]);
}

#[test]
fn realpath_folds_dots_and_follows_symlinks() {
    let tmp = tempfile::tempdir().unwrap();
    let base = realpath(&tmp.path().to_string_lossy()).unwrap();
    let real = Path::new(&base).join("real");
    std::fs::create_dir_all(real.join("deep")).unwrap();
    let link = Path::new(&base).join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let s = |p: &Path| p.to_string_lossy().into_owned();
    assert_eq!(realpath(&s(&link.join("deep"))).unwrap(), s(&real.join("deep")));
    assert_eq!(realpath(&format!("{}/./deep/..//deep/", s(&real))).unwrap(), s(&real.join("deep")));
    // a missing part is kept as written
    assert_eq!(realpath(&s(&link.join("nope/x"))).unwrap(), s(&real.join("nope/x")));
    assert_eq!(realpath("/").unwrap(), "/");
    assert_eq!(realpath("/..").unwrap(), "/");
    // a relative symlink target resolves against the link's folder
    let rel = Path::new(&base).join("rel");
    std::os::unix::fs::symlink("real/deep", &rel).unwrap();
    assert_eq!(realpath(&s(&rel)).unwrap(), s(&real.join("deep")));
    // a symlink loop does not hang, and leaves the looping link
    let a = Path::new(&base).join("loop_a");
    let b = Path::new(&base).join("loop_b");
    std::os::unix::fs::symlink(&b, &a).unwrap();
    std::os::unix::fs::symlink(&a, &b).unwrap();
    // Outcomes recorded from Python 3.13's Path.resolve on the same layout:
    assert_eq!(realpath(&s(&a)).unwrap(), s(&a));
    assert_eq!(realpath(&s(&a.join("x"))).unwrap(), s(&a.join("x")));
    assert_eq!(realpath(&format!("{base}/real/../rel/..")).unwrap(), s(&real));
    assert_eq!(realpath(&format!("{base}/nope/../real")).unwrap(), s(&real));
    assert_eq!(realpath(&format!("{base}/rel/")).unwrap(), s(&real.join("deep")));
    assert_eq!(realpath("//a//b").unwrap(), "/a/b");
    let up = Path::new(&base).join("up");
    let base_name = Path::new(&base).file_name().unwrap().to_string_lossy().into_owned();
    std::os::unix::fs::symlink(format!("../{base_name}/real"), &up).unwrap();
    assert_eq!(realpath(&format!("{base}/up/deep/../deep")).unwrap(), s(&real.join("deep")));
    // a session reached through the link is alive for a process in the real folder
    assert!(alive(&s(&link.join("deep")), [s(&real)]));
}

#[test]
fn the_log_name_is_the_last_folder() {
    assert_eq!(folder_name("/Users/g/repo"), "repo");
    assert_eq!(folder_name("/Users/g/repo/"), "repo");
    assert_eq!(folder_name("/"), "");
    assert_eq!(folder_name("/a/.."), "..");
}

#[test]
fn the_runner_captures_stdout_and_the_exit_code() {
    let argv = vec!["/bin/sh".to_string(), "-c".to_string(), "echo hi; echo err >&2; exit 3".to_string()];
    let out = subprocess_run(&argv, Duration::from_secs(5)).unwrap();
    assert_eq!(out, RunOutput { returncode: 3, stdout: "hi\n".to_string() });
}

#[test]
fn the_runner_times_out_and_names_its_errors_as_python_does() {
    let argv = vec!["/bin/sleep".to_string(), "5".to_string()];
    let started = Instant::now();
    let err = subprocess_run(&argv, Duration::from_millis(100)).unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(err.type_name(), "TimeoutExpired");
    let err = subprocess_run(&["/nonexistent/ps".to_string()], Duration::from_secs(1)).unwrap_err();
    assert_eq!(err.type_name(), "FileNotFoundError");
}

#[test]
#[ignore = "live: probes this Mac's running claude processes with ps and lsof"]
fn the_live_probe_reads_this_mac() {
    let found = live_processes(&subprocess_run);
    eprintln!("live claude folders: {found:?}");
    if let Some(found) = found {
        assert!(found.keys().all(|k| k.starts_with('/')));
    }
}
