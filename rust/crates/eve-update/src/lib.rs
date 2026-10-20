//! Self-update via `git pull && pip install -e .` + service restart.
//!
//! Port of `eve_bridge/update.py`. The Rust daemon's own update (the
//! `eve-bridge update` this binary runs) takes the native path instead:
//! `git pull`, `cargo build --release` of the binaries in `rust/`, signing,
//! a refresh of `bridge/.venv` for the Python helpers, then the restart.
//!
//! Designed for the common case: cloned from GitHub, running in an editable
//! venv, daemon managed by the platform's service backend (launchd / systemd
//! user unit / Task Scheduler). Bails clearly on:
//!
//! - Not a git checkout (e.g. installed from a wheel) — direct user to pip
//! - Uncommitted local changes — direct user to stash/commit first
//! - Already on latest — no-op
//! - Non-tty without --yes — refuse to prompt blindly
//!
//! We *do not* try to be cleverer than git here. If the user is on a feature
//! branch, mid-rebase, or has a dirty tree, we say so and step away.
//!
//! Every outside effect `run_update` has (the version check, the repo lookup,
//! subprocesses, the terminal) goes through [`Host`], so tests swap in a fake
//! where the Python tests monkeypatch module functions.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use eve_version_check::py::{str_repr, strip};
use eve_version_check::UpdateInfo;

/// `subprocess.run` result as `_run` returns it: `(returncode, stdout, stderr)`.
pub type RunResult = (i32, String, String);

/// The launchd label and the plist the installer writes for it.
pub const LAUNCHD_LABEL: &str = "com.github.eve-bridge.daemon";
/// The systemd user unit name.
pub const SYSTEMD_UNIT: &str = "eve-bridge.service";
/// The Task Scheduler task name.
pub const SCHEDULER_TASK: &str = "eve-bridge-daemon";

// ---- platform facts ----

/// `sys.platform` for the platforms the Python branches on.
pub fn sys_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(windows) {
        "win32"
    } else {
        "linux"
    }
}

/// `Path.home()`: `$HOME` when it is set (an empty one reads as `/`, as
/// `os.path.expanduser` gives), else the password database.
pub fn home() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(h) if h.is_empty() => PathBuf::from("/"),
        Some(h) => PathBuf::from(h),
        None => dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
    }
}

/// `platform.machine()`: `os.uname().machine` on Unix (so `arm64` on Apple
/// silicon, `aarch64` / `x86_64` on Linux); on Windows the
/// `PROCESSOR_ARCHITEW6432` / `PROCESSOR_ARCHITECTURE` variables, as Python
/// reads them.
pub fn platform_machine() -> String {
    #[cfg(unix)]
    {
        // SAFETY: `uname` fills a zeroed, caller-owned struct; `machine` is a
        // NUL-terminated C string inside it.
        unsafe {
            let mut u: libc::utsname = std::mem::zeroed();
            if libc::uname(&mut u) == 0 {
                let c = std::ffi::CStr::from_ptr(u.machine.as_ptr());
                return c.to_string_lossy().into_owned();
            }
        }
        String::new()
    }
    #[cfg(not(unix))]
    {
        std::env::var("PROCESSOR_ARCHITEW6432")
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| std::env::var("PROCESSOR_ARCHITECTURE").ok())
            .unwrap_or_default()
    }
}

// ---- module functions ----

/// The directory `update.py` lives in, as the Rust build saw it: this
/// crate's `src/` inside the checkout it was built from.
fn module_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Walk up from this module to find a `.git` dir. None if installed from a
/// built package (a binary moved off the checkout it was built from), which
/// means self-update isn't available — user should use pip directly.
pub fn package_repo_root() -> Option<PathBuf> {
    package_repo_root_from(&module_dir())
}

/// [`package_repo_root`] from an explicit start directory. Like
/// `Path.resolve()` (non-strict), a path that cannot be resolved is walked as
/// given; a `.git` file (a worktree) counts as well as a directory.
pub fn package_repo_root_from(start: &Path) -> Option<PathBuf> {
    let here = std::fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
    here.ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .map(Path::to_path_buf)
}

/// What `pip install -e` gets: ".[laya]" on Apple silicon (laya-mlx for the
/// eye expressions, pyproject.toml), plain "." everywhere else.
///
/// `platform` is a `sys.platform` value; `machine` defaults to
/// [`platform_machine`].
pub fn install_target(platform: &str, machine: Option<&str>) -> &'static str {
    let machine = match machine {
        Some(m) => m.to_string(),
        None => platform_machine(),
    };
    if platform == "darwin" && machine == "arm64" {
        ".[laya]"
    } else {
        "."
    }
}

/// `install_target()` with its defaults: this platform and machine.
pub fn default_install_target() -> &'static str {
    install_target(sys_platform(), None)
}

/// The extras the native update installs into `bridge/.venv`: the same as
/// `install.sh` ("miniapp", plus "laya" on Apple silicon).
pub fn native_install_target(platform: &str, machine: &str) -> &'static str {
    if platform == "darwin" && machine == "arm64" {
        ".[miniapp,laya]"
    } else {
        ".[miniapp]"
    }
}

/// The identifier the native build is signed with, so macOS keeps its
/// permissions (Microphone, Accessibility, Screen Recording) across rebuilds.
pub const SIGNING_IDENTIFIER: &str = "com.github.eve-bridge";

/// The packages the native update builds: the daemon and its hooks, and every helper binary the daemon starts
/// beside itself: the desktop worker (computer use), `eve-watch --render` (the watcher's browser), the vault's
/// MCP server for Codex, and a Work session's launcher and MCP server. A helper left unbuilt is a feature that
/// fails at its first use, so they are built with the daemon.
pub const NATIVE_PACKAGES: &[&str] =
    &["eve-bridge", "eve-hooks", "eve-desktop-worker", "eve-web", "eve-vault-mcp", "eve-eve-mcp", "eve-work-sessions"];

/// `cargo build --release --locked -p <each of NATIVE_PACKAGES>`, after the cargo program.
pub fn native_build_args() -> Vec<String> {
    let mut args: Vec<String> = ["build", "--release", "--locked"].iter().map(|s| s.to_string()).collect();
    for p in NATIVE_PACKAGES {
        args.push("-p".to_string());
        args.push(p.to_string());
    }
    args
}

/// Who signs the native build: `EVE_CODESIGN_IDENTITY` when set (`-` is ad
/// hoc), else the first Apple Development identity `security` lists, else
/// nobody.
pub fn signing_identity_with<H: Host + ?Sized>(host: &H) -> Option<String> {
    if let Some(id) = host.env("EVE_CODESIGN_IDENTITY").filter(|v| !v.is_empty()) {
        return Some(id);
    }
    let (rc, out, _) = host.run(&argv(&["security", "find-identity", "-v", "-p", "codesigning"]), None);
    if rc != 0 {
        return None;
    }
    out.lines().find_map(|line| {
        let start = line.find("\"Apple Development: ")? + 1;
        let end = start + line[start..].find('"')?;
        Some(line[start..end].to_string())
    })
}

/// The last `n` lines of a tool's stderr, for a failure message.
fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = strip(text).lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Python's `str(OSError)`: `[Errno N] <strerror>: '<filename>'`.
fn os_error_text(e: &std::io::Error, filename: &str) -> String {
    let display = e.to_string();
    match e.raw_os_error() {
        Some(code) => {
            let suffix = format!(" (os error {code})");
            let msg = display.strip_suffix(&suffix).unwrap_or(&display);
            format!("[Errno {code}] {msg}: {}", str_repr(filename))
        }
        None => display,
    }
}

/// subprocess.run wrapper that returns (rc, stdout, stderr) instead of
/// raising. Streams nothing — caller decides what to print.
///
/// A program or `cwd` that does not exist is rc 127 with Python's
/// `FileNotFoundError` text. A signal-killed child is `-signum`, as
/// `returncode` reports it.
pub fn run(cmd: &[String], cwd: Option<&Path>) -> RunResult {
    let Some((program, args)) = cmd.split_first() else {
        return (127, String::new(), "empty command".to_string());
    };
    let mut c = Command::new(program);
    c.args(args);
    if let Some(dir) = cwd {
        c.current_dir(dir);
    }
    match c.output() {
        Ok(out) => {
            let rc = match out.status.code() {
                Some(code) => code,
                None => signal_code(&out.status),
            };
            (
                rc,
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )
        }
        Err(e) => {
            // CPython names the cwd when the chdir fails, the program otherwise.
            let filename = match cwd {
                Some(dir) if !dir.is_dir() => dir.to_string_lossy().into_owned(),
                _ => program.clone(),
            };
            let rc = if e.kind() == std::io::ErrorKind::NotFound { 127 } else { 126 };
            (rc, String::new(), os_error_text(&e, &filename))
        }
    }
}

#[cfg(unix)]
fn signal_code(status: &std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status.signal().map(|s| -s).unwrap_or(-1)
}

#[cfg(not(unix))]
fn signal_code(_status: &std::process::ExitStatus) -> i32 {
    -1
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// `_git_status_clean` over a host's `run`.
pub fn git_status_clean_with<H: Host + ?Sized>(host: &H, repo: &Path) -> bool {
    let (rc, out, _) = host.run(&argv(&["git", "status", "--porcelain"]), Some(repo));
    rc == 0 && strip(&out).is_empty()
}

/// `_git_status_clean` with real subprocesses.
pub fn git_status_clean(repo: &Path) -> bool {
    git_status_clean_with(&System::default(), repo)
}

/// `_current_branch` over a host's `run`.
pub fn current_branch_with<H: Host + ?Sized>(host: &H, repo: &Path) -> String {
    let (rc, out, _) = host.run(&argv(&["git", "branch", "--show-current"]), Some(repo));
    if rc == 0 {
        strip(&out).to_string()
    } else {
        String::new()
    }
}

/// `_current_branch` with real subprocesses.
pub fn current_branch(repo: &Path) -> String {
    current_branch_with(&System::default(), repo)
}

/// `_detect_service_backend` with its inputs explicit: `platform` is a
/// `sys.platform` value, `home` is `Path.home()`, and the Windows check runs
/// through the host.
///
/// Best-effort guess at what to restart after the install.
///
/// We only restart when the service is plausibly installed; otherwise we tell
/// the user to bounce the daemon themselves.
pub fn detect_service_backend_for<H: Host + ?Sized>(host: &H, platform: &str, home: &Path) -> Option<String> {
    if platform == "darwin" {
        let plist = home
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{LAUNCHD_LABEL}.plist"));
        return plist.exists().then(|| "launchd".to_string());
    }
    if platform == "win32" {
        // schtasks query is the most reliable check; defer to subprocess.
        let (rc, _, _) = host.run(&argv(&["schtasks", "/query", "/tn", SCHEDULER_TASK]), None);
        return (rc == 0).then(|| "scheduler".to_string());
    }
    // Linux / other Unix — assume systemd user unit if the file is there.
    let unit = home.join(".config").join("systemd").join("user").join(SYSTEMD_UNIT);
    unit.exists().then(|| "systemd".to_string())
}

/// `_detect_service_backend` for this machine.
pub fn detect_service_backend() -> Option<String> {
    detect_service_backend_for(&System::default(), sys_platform(), &home())
}

/// `os.getuid()`.
#[cfg(unix)]
fn getuid() -> Option<u32> {
    // SAFETY: getuid has no preconditions and cannot fail.
    Some(unsafe { libc::getuid() })
}

#[cfg(not(unix))]
fn getuid() -> Option<u32> {
    None
}

fn failure_text(err: &str, fallback: &str) -> String {
    let e = strip(err);
    if e.is_empty() { fallback.to_string() } else { e.to_string() }
}

/// `_restart_service` over a host's `run`.
/// Restart the daemon. Returns (ok, human-readable message).
pub fn restart_service_with<H: Host + ?Sized>(host: &H, backend: &str) -> (bool, String) {
    if backend == "launchd" {
        let Some(uid) = getuid() else {
            // Python's os.getuid does not exist off Unix; launchd never does either.
            return (false, "launchctl failed: os.getuid is unavailable on this platform".to_string());
        };
        let (rc, _, err) = host.run(
            &argv(&["launchctl", "kickstart", "-k", &format!("gui/{uid}/{LAUNCHD_LABEL}")]),
            None,
        );
        if rc == 0 {
            return (true, "Daemon restarted (launchctl kickstart).".to_string());
        }
        return (false, format!("launchctl failed: {}", failure_text(&err, "unknown error")));
    }
    if backend == "systemd" {
        let (rc, _, err) = host.run(&argv(&["systemctl", "--user", "restart", SYSTEMD_UNIT]), None);
        if rc == 0 {
            return (true, "Daemon restarted (systemctl --user).".to_string());
        }
        return (false, format!("systemctl failed: {}", failure_text(&err, "unknown error")));
    }
    if backend == "scheduler" {
        // Stop + start to force a re-run with the new code.
        host.run(&argv(&["schtasks", "/end", "/tn", SCHEDULER_TASK]), None);
        let (rc, _, err) = host.run(&argv(&["schtasks", "/run", "/tn", SCHEDULER_TASK]), None);
        if rc == 0 {
            return (true, "Daemon restarted (Task Scheduler).".to_string());
        }
        return (false, format!("schtasks failed: {}", failure_text(&err, "unknown error")));
    }
    (false, format!("Unknown service backend: {}", str_repr(backend)))
}

/// `_restart_service` with real subprocesses.
pub fn restart_service(backend: &str) -> (bool, String) {
    restart_service_with(&System::default(), backend)
}

// ---- the seam ----

/// Everything `run_update` touches outside itself. The defaults are the real
/// thing; the Python tests' monkeypatches become overrides.
pub trait Host {
    /// `version_check.check(force=...)`.
    fn check(&self, force: bool) -> UpdateInfo {
        eve_version_check::check(force)
    }

    /// `package_repo_root()`.
    fn package_repo_root(&self) -> Option<PathBuf> {
        package_repo_root()
    }

    /// `_run`.
    fn run(&self, cmd: &[String], cwd: Option<&Path>) -> RunResult {
        run(cmd, cwd)
    }

    /// `_git_status_clean`.
    fn git_status_clean(&self, repo: &Path) -> bool {
        git_status_clean_with(self, repo)
    }

    /// `_current_branch`.
    fn current_branch(&self, repo: &Path) -> String {
        current_branch_with(self, repo)
    }

    /// `_detect_service_backend`.
    fn detect_service_backend(&self) -> Option<String> {
        detect_service_backend_for(self, sys_platform(), &home())
    }

    /// `_restart_service`.
    fn restart_service(&self, backend: &str) -> (bool, String) {
        restart_service_with(self, backend)
    }

    /// `install_target()` with its defaults.
    fn install_target(&self) -> String {
        default_install_target().to_string()
    }

    /// `sys.executable`: the interpreter that runs `-m pip install -e`.
    fn python(&self) -> String;

    /// Whether this is the Rust daemon's own update (the native path).
    fn native(&self) -> bool {
        false
    }

    /// `os.environ.get(key)`.
    fn env(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }

    /// `Path.exists()`.
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    /// `sys.stdin.isatty()`.
    fn stdin_isatty(&self) -> bool {
        std::io::stdin().is_terminal()
    }

    /// One line from stdin, as `input()` reads it after printing its prompt.
    /// `None` is end of input (`EOFError`).
    fn read_line(&self) -> Option<String> {
        let mut line = String::new();
        match std::io::stdin().lock().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => {
                if line.ends_with('\n') {
                    line.pop();
                    if line.ends_with('\r') {
                        line.pop();
                    }
                }
                Some(line)
            }
        }
    }
}

/// The real host. `python` stands in for `sys.executable`: a Rust process has
/// no interpreter of its own, so the caller names the one whose environment
/// holds eve (the daemon's venv `python`). The default is `python3` from
/// `PATH`.
///
/// `native` is the Rust daemon's own update, which rebuilds `rust/` rather
/// than reinstalling the Python package.
#[derive(Debug, Clone)]
pub struct System {
    pub python: String,
    pub native: bool,
}

impl Default for System {
    fn default() -> Self {
        Self { python: "python3".to_string(), native: false }
    }
}

impl Host for System {
    fn python(&self) -> String {
        self.python.clone()
    }
    fn native(&self) -> bool {
        self.native
    }
}

// ---- entry point ----

/// Entry point for `eve-bridge update`. Returns process exit code.
pub fn run_update(yes: bool) -> i32 {
    run_update_using(&System { native: true, ..System::default() }, yes)
}

/// [`run_update`] on a given host, printing to the real stdout and stderr.
pub fn run_update_using<H: Host + ?Sized>(host: &H, yes: bool) -> i32 {
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();
    run_update_with(host, yes, &mut out, &mut err)
}

/// `run_update` with every effect explicit: the host, and the two streams
/// Python's `print` writes to.
pub fn run_update_with<H: Host + ?Sized>(host: &H, yes: bool, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    // print() failures (a closed pipe) are not worth aborting an update over.
    macro_rules! say {
        ($w:expr, $($arg:tt)*) => {{
            let _ = writeln!($w, $($arg)*);
            let _ = $w.flush();
        }};
    }

    let info = host.check(true);
    if !info.has_update() {
        if info.latest.is_none() {
            say!(out, "Could not reach github.com to check for updates. Try again later.");
            return 2;
        }
        say!(out, "Already on the latest release ({}).", info.current);
        return 0;
    }

    let Some(repo) = host.package_repo_root() else {
        say!(
            err,
            "eve-bridge: this install isn't a git checkout — self-update isn't supported.\n\
             Upgrade with `pip install -U eve-bridge` (when on PyPI) or re-clone the repo."
        );
        return 2;
    };
    let repo_s = repo.to_string_lossy();

    if !host.git_status_clean(&repo) {
        say!(
            err,
            "eve-bridge: uncommitted changes in {repo_s} — refusing to `git pull`.\n\
             Stash or commit them first, then retry."
        );
        return 2;
    }

    let branch = {
        let b = host.current_branch(&repo);
        if b.is_empty() { "(detached)".to_string() } else { b }
    };
    let backend = host.detect_service_backend();

    say!(out, "Update plan for {repo_s}:");
    say!(out, "  Current:  {}  ({branch})", info.current);
    say!(out, "  Target:   {}", info.latest.as_deref().unwrap_or("None"));
    if host.native() {
        let target = native_install_target(sys_platform(), &platform_machine());
        let _ = write!(out, "  Steps:    git pull → cargo build --release → sign → pip install -e {target} (helpers)");
    } else {
        let _ = write!(out, "  Steps:    git pull → pip install -e {}", host.install_target());
    }
    match &backend {
        Some(b) => say!(out, "  →  restart via {b}"),
        None => say!(out, "  →  (no service backend detected)"),
    }

    if !yes {
        if !host.stdin_isatty() {
            say!(err, "eve-bridge: stdin is not a terminal — re-run with `-y` to skip the confirmation prompt.");
            return 2;
        }
        let _ = write!(out, "\nProceed? [y/N] ");
        let _ = out.flush();
        let Some(line) = host.read_line() else {
            say!(out, "\nAborted.");
            return 1;
        };
        let confirm = strip(&line).to_lowercase();
        if confirm != "y" && confirm != "yes" {
            say!(out, "Aborted.");
            return 1;
        }
    }

    say!(out, "");
    say!(out, "$ git pull");
    let (rc, _, e) = host.run(&argv(&["git", "pull"]), Some(&repo));
    if rc != 0 {
        say!(err, "git pull failed: {}", failure_text(&e, "unknown"));
        return 2;
    }

    if host.native() {
        let rust_dir = repo.join("rust");
        let home_cargo = home().join(".cargo").join("bin").join("cargo");
        let cargo = if host.exists(&home_cargo) { home_cargo.to_string_lossy().into_owned() } else { "cargo".to_string() };
        let build = native_build_args();
        say!(out, "$ cargo {}", build.join(" "));
        let mut cmd = vec![cargo.clone()];
        cmd.extend(build);
        let (rc, _, e) = host.run(&cmd, Some(&rust_dir));
        if rc != 0 {
            say!(err, "cargo build failed: {}", failure_text(&tail_lines(&e, 20), "unknown"));
            say!(err, "Repo is updated; only the build step failed. Re-run the build in rust/ manually.");
            return 2;
        }

        let bin = rust_dir.join("target").join("release").join("eve-bridge");
        let bin_s = bin.to_string_lossy().into_owned();
        match signing_identity_with(host) {
            Some(id) => {
                say!(out, "$ codesign --force --sign {} --identifier {SIGNING_IDENTIFIER} {bin_s}", str_repr(&id));
                let (rc, _, e) = host.run(
                    &argv(&["codesign", "--force", "--sign", &id, "--identifier", SIGNING_IDENTIFIER, &bin_s]),
                    None,
                );
                if rc != 0 {
                    say!(err, "codesign failed: {} (macOS may ask for its permissions again)", failure_text(&e, "unknown"));
                }
            }
            None => say!(out, "(no Apple Development identity: macOS may ask for its permissions again)"),
        }

        let bridge = repo.join("bridge");
        let venv_python = bridge.join(".venv").join("bin").join("python");
        if host.exists(&venv_python) {
            let py = venv_python.to_string_lossy().into_owned();
            let target = native_install_target(sys_platform(), &platform_machine());
            say!(out, "$ {py} -m pip install --quiet -e {target}");
            let (rc, _, e) = host.run(&argv(&[&py, "-m", "pip", "install", "--quiet", "-e", target]), Some(&bridge));
            if rc != 0 {
                say!(
                    err,
                    "pip install failed: {} (the Python helpers keep their old libraries)",
                    failure_text(&tail_lines(&e, 5), "unknown")
                );
            }
        }
    } else {
        let target = host.install_target();
        let python = host.python();
        say!(out, "$ {python} -m pip install -e {target}");
        let (rc, _, e) = host.run(&argv(&[&python, "-m", "pip", "install", "-e", &target]), Some(&repo));
        if rc != 0 {
            say!(err, "pip install failed: {}", failure_text(&e, "unknown"));
            say!(err, "Repo is updated; only the install step failed. Re-run pip install manually.");
            return 2;
        }
    }

    let Some(backend) = backend else {
        say!(out, "\nDone. Restart your daemon manually to pick up the new code.");
        return 0;
    };

    let (ok, msg) = host.restart_service(&backend);
    say!(out, "\n{msg}");
    if ok { 0 } else { 2 }
}

#[cfg(test)]
mod tests;
