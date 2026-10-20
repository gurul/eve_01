//! Port of `bridge/tests/test_update.py`, plus the helpers the Python tests
//! leave alone.
//!
//! The shell-out paths (git pull, pip install, service restart) are
//! intentionally not exercised against the real system — they're too
//! OS-specific and side-effectful. We test the helpers + the bail-out
//! branches, with every effect behind a fake [`Host`].

use super::*;
use std::cell::RefCell;

fn fake_info(current: &str, latest: Option<&str>) -> UpdateInfo {
    UpdateInfo { current: current.to_string(), latest: latest.map(str::to_string), fetched_at: 0.0 }
}

type RunFn = Box<dyn Fn(&[String]) -> RunResult>;

/// A host whose every effect is a field. Unset overrides fall through to the
/// trait defaults built on `run`, as unpatched module functions do in Python.
struct Fake {
    info: UpdateInfo,
    repo: Option<PathBuf>,
    clean: Option<bool>,
    branch: Option<String>,
    backend: Option<Option<String>>,
    restart: Option<(bool, String)>,
    tty: bool,
    answer: Option<String>,
    run: RunFn,
    calls: RefCell<Vec<(Vec<String>, Option<PathBuf>)>>,
}

impl Fake {
    fn new(info: UpdateInfo) -> Self {
        Self {
            info,
            repo: None,
            clean: None,
            branch: None,
            backend: None,
            restart: None,
            tty: false,
            answer: None,
            run: Box::new(|_| (0, String::new(), String::new())),
            calls: RefCell::new(Vec::new()),
        }
    }

    /// The patches every post-checkout test applies.
    fn ready(info: UpdateInfo, repo: &Path, backend: Option<&str>) -> Self {
        let mut f = Self::new(info);
        f.repo = Some(repo.to_path_buf());
        f.clean = Some(true);
        f.branch = Some("main".to_string());
        f.backend = Some(backend.map(str::to_string));
        f
    }

    fn with_run(mut self, run: impl Fn(&[String]) -> RunResult + 'static) -> Self {
        self.run = Box::new(run);
        self
    }
}

impl Host for Fake {
    fn check(&self, force: bool) -> UpdateInfo {
        assert!(force, "run_update always forces the check");
        self.info.clone()
    }
    fn package_repo_root(&self) -> Option<PathBuf> {
        self.repo.clone()
    }
    fn run(&self, cmd: &[String], cwd: Option<&Path>) -> RunResult {
        self.calls.borrow_mut().push((cmd.to_vec(), cwd.map(Path::to_path_buf)));
        (self.run)(cmd)
    }
    fn git_status_clean(&self, repo: &Path) -> bool {
        match self.clean {
            Some(c) => c,
            None => git_status_clean_with(self, repo),
        }
    }
    fn current_branch(&self, repo: &Path) -> String {
        match &self.branch {
            Some(b) => b.clone(),
            None => current_branch_with(self, repo),
        }
    }
    fn detect_service_backend(&self) -> Option<String> {
        self.backend.clone().unwrap_or(None)
    }
    fn restart_service(&self, backend: &str) -> (bool, String) {
        match &self.restart {
            Some(r) => r.clone(),
            None => restart_service_with(self, backend),
        }
    }
    fn install_target(&self) -> String {
        ".".to_string()
    }
    fn python(&self) -> String {
        "/venv/bin/python".to_string()
    }
    fn stdin_isatty(&self) -> bool {
        self.tty
    }
    fn read_line(&self) -> Option<String> {
        self.answer.clone()
    }
}

/// `run_update_with` and the captured `(rc, stdout, stderr)`, like capsys.
fn update(host: &Fake, yes: bool) -> (i32, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let rc = run_update_with(host, yes, &mut out, &mut err);
    (rc, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
}

fn rr(rc: i32, out: &str, err: &str) -> RunResult {
    (rc, out.to_string(), err.to_string())
}

// ---- package_repo_root ----

/// The eve-bridge repo itself has a .git dir, so the function should locate
/// it from within the package.
#[test]
fn test_package_repo_root_finds_dotgit() {
    let root = package_repo_root().expect("built from a checkout");
    assert!(root.join(".git").exists());
    // And the repo is the one we're running from. In the claude-pet fork the
    // bridge lives in a bridge/ subdir of the repo root.
    assert!(root.join("pyproject.toml").exists() || root.join("bridge").join("pyproject.toml").exists());
}

#[test]
fn package_repo_root_none_outside_a_checkout() {
    let tmp = tempfile::tempdir().unwrap();
    let deep = tmp.path().join("a").join("b");
    std::fs::create_dir_all(&deep).unwrap();
    // A temp dir is not inside a git checkout on any CI or dev machine.
    let found = package_repo_root_from(&deep);
    assert!(found.is_none_or(|r| !r.starts_with(std::fs::canonicalize(tmp.path()).unwrap())));
}

#[test]
fn package_repo_root_finds_nearest_dotgit_file_or_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let repo = base.join("repo");
    let deep = repo.join("bridge").join("src").join("eve_bridge");
    std::fs::create_dir_all(&deep).unwrap();
    // A worktree's .git is a file, and Path.exists() accepts it.
    std::fs::write(repo.join(".git"), "gitdir: /elsewhere\n").unwrap();
    assert_eq!(package_repo_root_from(&deep), Some(repo.clone()));
    // The start directory itself counts.
    assert_eq!(package_repo_root_from(&repo), Some(repo.clone()));
    // The nearest one wins.
    std::fs::create_dir(repo.join("bridge").join(".git")).unwrap();
    assert_eq!(package_repo_root_from(&deep), Some(repo.join("bridge")));
}

#[test]
fn package_repo_root_walks_an_unresolvable_path_as_given() {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::create_dir(base.join(".git")).unwrap();
    // Path.resolve(strict=False) keeps a missing tail; the walk still finds base.
    let missing = base.join("gone").join("src");
    assert_eq!(package_repo_root_from(&missing), Some(base));
}

// ---- install_target ----

#[test]
fn install_target_laya_only_on_apple_silicon() {
    assert_eq!(install_target("darwin", Some("arm64")), ".[laya]");
    assert_eq!(install_target("darwin", Some("x86_64")), ".");
    assert_eq!(install_target("linux", Some("arm64")), ".");
    assert_eq!(install_target("linux", Some("aarch64")), ".");
    assert_eq!(install_target("win32", Some("ARM64")), ".");
}

#[test]
fn install_target_defaults_to_this_machine() {
    let expected = if cfg!(all(target_os = "macos", target_arch = "aarch64")) { ".[laya]" } else { "." };
    assert_eq!(default_install_target(), expected);
    assert_eq!(install_target(sys_platform(), None), expected);
}

#[test]
fn platform_machine_reads_uname() {
    let m = platform_machine();
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        assert_eq!(m, "arm64");
    } else if cfg!(target_arch = "x86_64") && cfg!(unix) {
        assert_eq!(m, "x86_64");
    } else {
        assert!(!m.is_empty());
    }
}

// ---- _run ----

#[test]
fn run_captures_rc_stdout_stderr() {
    let (rc, out, err) = run(&argv(&["sh", "-c", "printf out; printf err >&2; exit 3"]), None);
    assert_eq!((rc, out.as_str(), err.as_str()), (3, "out", "err"));
}

#[test]
fn run_uses_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let (rc, out, _) = run(&argv(&["pwd"]), Some(tmp.path()));
    assert_eq!(rc, 0);
    let got = std::fs::canonicalize(strip(&out)).unwrap();
    assert_eq!(got, std::fs::canonicalize(tmp.path()).unwrap());
}

#[test]
fn run_missing_program_is_127_with_python_text() {
    let (rc, out, err) = run(&argv(&["eve-no-such-program-xyz"]), None);
    assert_eq!(rc, 127);
    assert_eq!(out, "");
    assert_eq!(err, "[Errno 2] No such file or directory: 'eve-no-such-program-xyz'");
}

#[test]
fn run_missing_cwd_names_the_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let gone = tmp.path().join("gone");
    let (rc, _, err) = run(&argv(&["true"]), Some(&gone));
    assert_eq!(rc, 127);
    assert_eq!(err, format!("[Errno 2] No such file or directory: '{}'", gone.display()));
}

#[cfg(unix)]
#[test]
fn run_signal_is_negative() {
    let (rc, _, _) = run(&argv(&["sh", "-c", "kill -TERM $$"]), None);
    assert_eq!(rc, -15);
}

// ---- _git_status_clean (parses subprocess output) ----

#[test]
fn test_git_status_clean_when_no_output() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Fake::new(fake_info("0.1.0", None)).with_run(|_| rr(0, "", ""));
    assert!(host.git_status_clean(tmp.path()));
    let calls = host.calls.borrow();
    assert_eq!(calls[0].0, argv(&["git", "status", "--porcelain"]));
    assert_eq!(calls[0].1.as_deref(), Some(tmp.path()));
}

#[test]
fn test_git_status_clean_false_when_dirty() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Fake::new(fake_info("0.1.0", None)).with_run(|_| rr(0, " M src/foo.py\n", ""));
    assert!(!host.git_status_clean(tmp.path()));
}

#[test]
fn test_git_status_clean_false_on_git_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Fake::new(fake_info("0.1.0", None)).with_run(|_| rr(128, "", "not a git repo"));
    assert!(!host.git_status_clean(tmp.path()));
}

#[test]
fn git_status_clean_whitespace_only_is_clean() {
    let host = Fake::new(fake_info("0.1.0", None)).with_run(|_| rr(0, "\n  \n", ""));
    assert!(host.git_status_clean(Path::new("/r")));
}

#[test]
fn git_status_clean_real_git_on_a_fresh_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let (rc, _, _) = run(&argv(&["git", "init", "-q"]), Some(tmp.path()));
    if rc != 0 {
        return; // no git on this machine
    }
    assert!(git_status_clean(tmp.path()));
    std::fs::write(tmp.path().join("new.txt"), "x").unwrap();
    assert!(!git_status_clean(tmp.path()));
    // Not a repository at all.
    let other = tempfile::tempdir().unwrap();
    let outside = run(&argv(&["git", "rev-parse", "--git-dir"]), Some(other.path())).0 != 0;
    if outside {
        assert!(!git_status_clean(other.path()));
    }
}

// ---- _current_branch ----

#[test]
fn current_branch_strips_and_blanks_on_failure() {
    let host = Fake::new(fake_info("0.1.0", None)).with_run(|_| rr(0, "main\n", ""));
    assert_eq!(host.current_branch(Path::new("/r")), "main");
    assert_eq!(host.calls.borrow()[0].0, argv(&["git", "branch", "--show-current"]));
    let host = Fake::new(fake_info("0.1.0", None)).with_run(|_| rr(128, "main\n", "fatal"));
    assert_eq!(host.current_branch(Path::new("/r")), "");
}

// ---- _detect_service_backend ----

#[test]
fn detect_service_backend_darwin_needs_the_plist() {
    let home = tempfile::tempdir().unwrap();
    let host = Fake::new(fake_info("0.1.0", None));
    assert_eq!(detect_service_backend_for(&host, "darwin", home.path()), None);
    let agents = home.path().join("Library").join("LaunchAgents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(agents.join("com.github.eve-bridge.daemon.plist"), "").unwrap();
    assert_eq!(detect_service_backend_for(&host, "darwin", home.path()).as_deref(), Some("launchd"));
    assert!(host.calls.borrow().is_empty());
}

#[test]
fn detect_service_backend_linux_needs_the_unit() {
    let home = tempfile::tempdir().unwrap();
    let host = Fake::new(fake_info("0.1.0", None));
    assert_eq!(detect_service_backend_for(&host, "linux", home.path()), None);
    let units = home.path().join(".config").join("systemd").join("user");
    std::fs::create_dir_all(&units).unwrap();
    std::fs::write(units.join("eve-bridge.service"), "").unwrap();
    assert_eq!(detect_service_backend_for(&host, "linux", home.path()).as_deref(), Some("systemd"));
}

#[test]
fn detect_service_backend_windows_asks_schtasks() {
    let home = tempfile::tempdir().unwrap();
    let host = Fake::new(fake_info("0.1.0", None)).with_run(|_| rr(0, "", ""));
    assert_eq!(detect_service_backend_for(&host, "win32", home.path()).as_deref(), Some("scheduler"));
    assert_eq!(host.calls.borrow()[0].0, argv(&["schtasks", "/query", "/tn", "eve-bridge-daemon"]));
    let host = Fake::new(fake_info("0.1.0", None)).with_run(|_| rr(1, "", "ERROR"));
    assert_eq!(detect_service_backend_for(&host, "win32", home.path()), None);
}

// ---- _restart_service ----

#[test]
fn restart_service_launchd_kickstarts_the_label() {
    let host = Fake::new(fake_info("0.1.0", None));
    let (ok, msg) = restart_service_with(&host, "launchd");
    if cfg!(unix) {
        assert!(ok);
        assert_eq!(msg, "Daemon restarted (launchctl kickstart).");
        let uid = unsafe { libc::getuid() };
        let expected = argv(&["launchctl", "kickstart", "-k", &format!("gui/{uid}/com.github.eve-bridge.daemon")]);
        assert_eq!(host.calls.borrow()[0].0, expected);
    }
    let host = Fake::new(fake_info("0.1.0", None)).with_run(|_| rr(113, "", "  Could not find service\n"));
    let (ok, msg) = restart_service_with(&host, "launchd");
    assert!(!ok);
    if cfg!(unix) {
        assert_eq!(msg, "launchctl failed: Could not find service");
    }
    let host = Fake::new(fake_info("0.1.0", None)).with_run(|_| rr(1, "", " \n"));
    if cfg!(unix) {
        assert_eq!(restart_service_with(&host, "launchd").1, "launchctl failed: unknown error");
    }
}

#[test]
fn restart_service_systemd() {
    let host = Fake::new(fake_info("0.1.0", None));
    assert_eq!(restart_service_with(&host, "systemd"), (true, "Daemon restarted (systemctl --user).".to_string()));
    assert_eq!(host.calls.borrow()[0].0, argv(&["systemctl", "--user", "restart", "eve-bridge.service"]));
    let host = Fake::new(fake_info("0.1.0", None)).with_run(|_| rr(5, "", "Unit not found.\n"));
    assert_eq!(restart_service_with(&host, "systemd"), (false, "systemctl failed: Unit not found.".to_string()));
}

#[test]
fn restart_service_scheduler_ends_then_runs() {
    let host = Fake::new(fake_info("0.1.0", None)).with_run(|cmd| {
        // /end failing (task not running) does not matter.
        if cmd[1] == "/end" { rr(1, "", "not running") } else { rr(0, "", "") }
    });
    assert_eq!(restart_service_with(&host, "scheduler"), (true, "Daemon restarted (Task Scheduler).".to_string()));
    let calls: Vec<_> = host.calls.borrow().iter().map(|c| c.0.clone()).collect();
    assert_eq!(
        calls,
        vec![
            argv(&["schtasks", "/end", "/tn", "eve-bridge-daemon"]),
            argv(&["schtasks", "/run", "/tn", "eve-bridge-daemon"]),
        ]
    );
    let host = Fake::new(fake_info("0.1.0", None)).with_run(|_| rr(1, "", ""));
    assert_eq!(restart_service_with(&host, "scheduler"), (false, "schtasks failed: unknown error".to_string()));
}

#[test]
fn restart_service_unknown_backend_uses_repr() {
    let host = Fake::new(fake_info("0.1.0", None));
    assert_eq!(restart_service_with(&host, "runit"), (false, "Unknown service backend: 'runit'".to_string()));
    assert!(host.calls.borrow().is_empty());
}

// ---- run_update early exits ----

#[test]
fn test_run_update_already_on_latest() {
    let host = Fake::new(fake_info("0.1.0", Some("0.1.0")));
    let (rc, out, _) = update(&host, true);
    assert_eq!(rc, 0);
    assert!(out.to_lowercase().contains("latest"));
    assert_eq!(out, "Already on the latest release (0.1.0).\n");
}

#[test]
fn test_run_update_network_failure() {
    let host = Fake::new(fake_info("0.1.0", None));
    let (rc, out, _) = update(&host, true);
    assert_eq!(rc, 2);
    assert!(out.to_lowercase().contains("github"));
    assert_eq!(out, "Could not reach github.com to check for updates. Try again later.\n");
}

#[test]
fn run_update_older_latest_is_already_latest() {
    let host = Fake::new(fake_info("0.2.0", Some("0.1.0")));
    let (rc, out, _) = update(&host, true);
    assert_eq!(rc, 0);
    assert_eq!(out, "Already on the latest release (0.2.0).\n");
}

#[test]
fn test_run_update_not_a_git_checkout() {
    let host = Fake::new(fake_info("0.1.0", Some("0.2.0")));
    let (rc, _, err) = update(&host, true);
    assert_eq!(rc, 2);
    assert!(err.contains("git checkout"));
    assert!(err.contains("pip install"));
    assert_eq!(
        err,
        "eve-bridge: this install isn't a git checkout — self-update isn't supported.\n\
         Upgrade with `pip install -U eve-bridge` (when on PyPI) or re-clone the repo.\n"
    );
}

#[test]
fn test_run_update_refuses_dirty_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let mut host = Fake::new(fake_info("0.1.0", Some("0.2.0")));
    host.repo = Some(tmp.path().to_path_buf());
    host.clean = Some(false);
    let (rc, _, err) = update(&host, true);
    assert_eq!(rc, 2);
    assert!(err.to_lowercase().contains("uncommitted"));
    assert_eq!(
        err,
        format!(
            "eve-bridge: uncommitted changes in {} — refusing to `git pull`.\nStash or commit them first, then retry.\n",
            tmp.path().display()
        )
    );
}

#[test]
fn test_run_update_no_tty_no_yes_bails() {
    let tmp = tempfile::tempdir().unwrap();
    let mut host = Fake::ready(fake_info("0.1.0", Some("0.2.0")), tmp.path(), Some("launchd"));
    host.tty = false;
    let (rc, _, err) = update(&host, false);
    assert_eq!(rc, 2);
    assert!(err.contains("-y"));
    assert!(host.calls.borrow().is_empty());
}

#[test]
fn test_run_update_user_declines() {
    let tmp = tempfile::tempdir().unwrap();
    let mut host = Fake::ready(fake_info("0.1.0", Some("0.2.0")), tmp.path(), None);
    host.tty = true;
    host.answer = Some("n".to_string());
    let (rc, out, _) = update(&host, false);
    assert_eq!(rc, 1);
    assert!(out.contains("Aborted"));
    assert!(out.ends_with("\nProceed? [y/N] Aborted.\n"));
    assert!(host.calls.borrow().is_empty());
}

#[test]
fn run_update_eof_at_the_prompt_aborts() {
    let tmp = tempfile::tempdir().unwrap();
    let mut host = Fake::ready(fake_info("0.1.0", Some("0.2.0")), tmp.path(), None);
    host.tty = true;
    host.answer = None;
    let (rc, out, _) = update(&host, false);
    assert_eq!(rc, 1);
    assert!(out.ends_with("Proceed? [y/N] \nAborted.\n"));
}

#[test]
fn run_update_user_accepts_with_yes_in_any_case() {
    for answer in ["y", " YES ", "Yes"] {
        let tmp = tempfile::tempdir().unwrap();
        let mut host = Fake::ready(fake_info("0.1.0", Some("0.2.0")), tmp.path(), None);
        host.tty = true;
        host.answer = Some(answer.to_string());
        let (rc, out, _) = update(&host, false);
        assert_eq!(rc, 0, "answer {answer:?}");
        assert!(out.contains("$ git pull"));
    }
}

/// End-to-end with -y, all subprocess calls stubbed to success.
#[test]
fn test_run_update_full_flow_with_yes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut host = Fake::ready(fake_info("0.1.0", Some("0.2.0")), tmp.path(), Some("launchd"));
    host.restart = Some((true, "Daemon restarted.".to_string()));
    let (rc, out, _) = update(&host, true);
    assert_eq!(rc, 0);
    assert!(out.contains("git pull"));
    assert!(out.contains("pip install"));
    assert!(out.contains("Daemon restarted"));
    let repo = tmp.path().display();
    assert_eq!(
        out,
        format!(
            "Update plan for {repo}:\n\
             \x20 Current:  0.1.0  (main)\n\
             \x20 Target:   0.2.0\n\
             \x20 Steps:    git pull → pip install -e .  →  restart via launchd\n\
             \n\
             $ git pull\n\
             $ /venv/bin/python -m pip install -e .\n\
             \n\
             Daemon restarted.\n"
        )
    );
    let calls = host.calls.borrow();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], (argv(&["git", "pull"]), Some(tmp.path().to_path_buf())));
    assert_eq!(
        calls[1],
        (argv(&["/venv/bin/python", "-m", "pip", "install", "-e", "."]), Some(tmp.path().to_path_buf()))
    );
}

#[test]
fn test_run_update_git_pull_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Fake::ready(fake_info("0.1.0", Some("0.2.0")), tmp.path(), None).with_run(|cmd| {
        if cmd[..2] == argv(&["git", "pull"])[..] { rr(1, "", "fatal: unable to access") } else { rr(0, "", "") }
    });
    let (rc, _, err) = update(&host, true);
    assert_eq!(rc, 2);
    // pip install should NOT have been attempted after git pull failed.
    assert!(host.calls.borrow().iter().all(|c| !c.0.join(" ").contains("pip")));
    assert_eq!(err, "git pull failed: fatal: unable to access\n");
}

#[test]
fn run_update_pip_failure_says_the_repo_is_updated() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Fake::ready(fake_info("0.1.0", Some("0.2.0")), tmp.path(), Some("launchd"))
        .with_run(|cmd| if cmd.iter().any(|a| a == "pip") { rr(1, "", "") } else { rr(0, "", "") });
    let (rc, _, err) = update(&host, true);
    assert_eq!(rc, 2);
    assert_eq!(
        err,
        "pip install failed: unknown\nRepo is updated; only the install step failed. Re-run pip install manually.\n"
    );
    // No restart after a failed install.
    assert_eq!(host.calls.borrow().len(), 2);
}

#[test]
fn run_update_without_backend_asks_for_a_manual_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let mut host = Fake::ready(fake_info("0.1.0", Some("0.2.0")), tmp.path(), None);
    host.branch = Some(String::new());
    let (rc, out, _) = update(&host, true);
    assert_eq!(rc, 0);
    assert!(out.contains("  Current:  0.1.0  ((detached))\n"));
    assert!(out.contains("pip install -e .  →  (no service backend detected)\n"));
    assert!(out.ends_with("\n\nDone. Restart your daemon manually to pick up the new code.\n"));
}

#[test]
fn run_update_failed_restart_is_exit_2() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Fake::ready(fake_info("0.1.0", Some("0.2.0")), tmp.path(), Some("systemd"))
        .with_run(|cmd| if cmd[0] == "systemctl" { rr(1, "", "boom\n") } else { rr(0, "", "") });
    let (rc, out, _) = update(&host, true);
    assert_eq!(rc, 2);
    assert!(out.ends_with("\nsystemctl failed: boom\n"));
}

#[test]
fn system_host_defaults_to_python3() {
    assert_eq!(System::default().python(), "python3");
    assert_eq!(System { python: "/v/bin/python".into(), native: false }.python(), "/v/bin/python");
}

// ---- the native (Rust) update ----

/// A fake for the Rust daemon's own update: `native` on, a signing identity
/// from the environment, and a venv that exists.
struct Native {
    inner: Fake,
    identity: Option<String>,
    venv: bool,
}

impl Host for Native {
    fn check(&self, force: bool) -> UpdateInfo {
        self.inner.check(force)
    }
    fn package_repo_root(&self) -> Option<PathBuf> {
        self.inner.package_repo_root()
    }
    fn run(&self, cmd: &[String], cwd: Option<&Path>) -> RunResult {
        self.inner.run(cmd, cwd)
    }
    fn git_status_clean(&self, repo: &Path) -> bool {
        self.inner.git_status_clean(repo)
    }
    fn current_branch(&self, repo: &Path) -> String {
        self.inner.current_branch(repo)
    }
    fn detect_service_backend(&self) -> Option<String> {
        self.inner.detect_service_backend()
    }
    fn restart_service(&self, backend: &str) -> (bool, String) {
        self.inner.restart_service(backend)
    }
    fn python(&self) -> String {
        self.inner.python()
    }
    fn native(&self) -> bool {
        true
    }
    fn env(&self, key: &str) -> Option<String> {
        (key == "EVE_CODESIGN_IDENTITY").then(|| self.identity.clone()).flatten()
    }
    fn exists(&self, path: &Path) -> bool {
        self.venv && path.ends_with("bridge/.venv/bin/python")
    }
}

fn native_update(host: &Native) -> (i32, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let rc = run_update_with(host, true, &mut out, &mut err);
    (rc, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
}

#[test]
fn native_update_builds_signs_refreshes_the_venv_and_restarts() {
    let repo = PathBuf::from("/repo");
    let mut inner = Fake::ready(fake_info("0.1.0", Some("0.2.0")), &repo, Some("launchd"));
    inner.restart = Some((true, "Daemon restarted (launchctl kickstart).".to_string()));
    let host = Native { inner, identity: Some("Apple Development: Test (TEAM)".to_string()), venv: true };
    let (rc, out, err) = native_update(&host);
    assert_eq!(rc, 0, "{err}");
    assert!(out.contains("git pull → cargo build --release → sign → pip install"));
    let calls = host.inner.calls.borrow();
    let names: Vec<String> = calls.iter().map(|(c, _)| c[..2.min(c.len())].join(" ")).collect();
    assert_eq!(names[0], "git pull");
    assert!(calls[1].0.ends_with(&argv(&["build", "--release", "--locked", "-p", "eve-bridge", "-p", "eve-hooks"])));
    assert_eq!(calls[1].1.as_deref(), Some(Path::new("/repo/rust")));
    assert_eq!(
        calls[2].0,
        argv(&[
            "codesign", "--force", "--sign", "Apple Development: Test (TEAM)", "--identifier",
            SIGNING_IDENTIFIER, "/repo/rust/target/release/eve-bridge",
        ])
    );
    assert_eq!(calls[3].0[0], "/repo/bridge/.venv/bin/python");
    assert_eq!(calls[3].1.as_deref(), Some(Path::new("/repo/bridge")));
    assert!(out.contains("Daemon restarted"));
}

#[test]
fn native_update_stops_when_the_build_fails_and_never_restarts() {
    let repo = PathBuf::from("/repo");
    let inner = Fake::ready(fake_info("0.1.0", Some("0.2.0")), &repo, Some("launchd")).with_run(|cmd| {
        if cmd.iter().any(|a| a == "build") { rr(101, "", "error[E0425]: cannot find value") } else { rr(0, "", "") }
    });
    let host = Native { inner, identity: None, venv: true };
    let (rc, _, err) = native_update(&host);
    assert_eq!(rc, 2);
    assert!(err.contains("cargo build failed: error[E0425]"));
    assert!(err.contains("only the build step failed"));
    let calls = host.inner.calls.borrow();
    assert!(!calls.iter().any(|(c, _)| c[0] == "codesign" || c[0] == "launchctl"));
}

#[test]
fn native_update_without_an_identity_or_venv_still_restarts() {
    let repo = PathBuf::from("/repo");
    let mut inner = Fake::ready(fake_info("0.1.0", Some("0.2.0")), &repo, Some("launchd")).with_run(|cmd| {
        if cmd[0] == "security" { rr(0, "     0 valid identities found\n", "") } else { rr(0, "", "") }
    });
    inner.restart = Some((true, "Daemon restarted (launchctl kickstart).".to_string()));
    let host = Native { inner, identity: None, venv: false };
    let (rc, out, _) = native_update(&host);
    assert_eq!(rc, 0);
    assert!(out.contains("no Apple Development identity"));
    let calls = host.inner.calls.borrow();
    assert!(!calls.iter().any(|(c, _)| c[0] == "codesign" || c[0].ends_with("python")));
}

#[test]
fn signing_identity_reads_the_first_apple_development_line() {
    let inner = Fake::new(fake_info("0.1.0", None)).with_run(|_| {
        rr(
            0,
            "  1) ABCDEF \"Developer ID Application: X (T1)\"\n  2) 012345 \"Apple Development: Y (T2)\"\n     2 valid identities found\n",
            "",
        )
    });
    let host = Native { inner, identity: None, venv: false };
    assert_eq!(signing_identity_with(&host).as_deref(), Some("Apple Development: Y (T2)"));
    let host = Native { identity: Some("-".to_string()), ..host };
    assert_eq!(signing_identity_with(&host).as_deref(), Some("-"));
}

#[test]
fn native_install_target_matches_install_sh() {
    assert_eq!(native_install_target("darwin", "arm64"), ".[miniapp,laya]");
    assert_eq!(native_install_target("darwin", "x86_64"), ".[miniapp]");
    assert_eq!(native_install_target("linux", "aarch64"), ".[miniapp]");
}
