//! Which Claude Code sessions are still alive, for the "claude on" picker.
//!
//! The daemon learns of a session from its hooks and forgets it only on SessionEnd. A terminal that is closed,
//! killed or crashes never sends that, so its session stayed in the picker for good, and joining it typed into
//! whatever window was in front (docs/stackchan/telegram-bot-api.md, relay weakness 2).
//!
//! The hooks carry no process id: SessionStart sends the session id, the transcript path and the folder, and
//! nothing else. So liveness is read from the Mac itself. `ps` lists the running `claude` processes and
//! `lsof -d cwd` gives each one's working folder, both in one call each (about 30 ms together, measured
//! 2026-09-23). A session is alive while a claude process works in its folder, or in a folder above it: the hook's
//! `cwd` can be a subfolder the session's shell moved into, while the process stays where it started.
//!
//! Every doubt keeps a session. The probe failing, or finding no claude process at all (a renamed binary would
//! look like that), answers "unknown" (`None`), and the picker then shows what it showed before. A session with a
//! permission still pending, or one that started moments ago, is never dropped either. Hiding a live session is
//! worse than showing a dead one: the first loses the owner's way in, the second costs one tap on a stale button.
//!
//! The daemon's list lives in memory, so a restart empties it until each session's next hook (live, 2026-10-10: the
//! daemon restarted at 15:31, the session in front of the owner was idle, and "claude on" at 15:33 found none and
//! opened a new one). So the picker also offers every interactive claude process whose folder no known session is
//! in. Joining one needs only its folder; its answers reach the phone once its next hook adopts it.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use indexmap::IndexMap;

pub const PS: &str = "/bin/ps";
pub const LSOF: &str = "/usr/sbin/lsof";
/// Each call; the picker is a reply to a text, not worth a longer wait.
pub const PROBE_TIMEOUT_SECS: f64 = 1.5;
/// A session this new is kept whatever the probe says.
pub const GRACE_SECS: f64 = 30.0;

/// What an npm install's `claude` shim runs under.
pub const RUNTIMES: [&str; 3] = ["node", "bun", "deno"];

/// A claude process with one of these flags is not a terminal session the owner can type into: Chrome's native
/// messaging host, a headless print run, an SDK or plugin worker speaking stream-json.
pub const HEADLESS_FLAGS: [&str; 5] = ["--chrome-native-host", "-p", "--print", "--output-format", "--input-format"];

// ---------------------------------------------------------------------------------------------------------------
// The subprocess seam (Python's `run: Run = subprocess.run`)
// ---------------------------------------------------------------------------------------------------------------

/// What `subprocess.run(..., capture_output=True, text=True)` hands back that this module reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutput {
    pub returncode: i32,
    pub stdout: String,
}

/// The exceptions `live_processes` catches: `OSError` and `subprocess.SubprocessError` (of which
/// `TimeoutExpired` is the one `run` raises).
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("{0}")]
    Os(#[from] std::io::Error),
    #[error("command {argv:?} timed out after {timeout:?}")]
    Timeout { argv: Vec<String>, timeout: Duration },
}

impl RunError {
    /// The Python exception class name, for the debug log line (`type(e).__name__`).
    pub fn type_name(&self) -> &'static str {
        match self {
            RunError::Timeout { .. } => "TimeoutExpired",
            RunError::Os(e) => match e.kind() {
                std::io::ErrorKind::NotFound => "FileNotFoundError",
                std::io::ErrorKind::PermissionDenied => "PermissionError",
                std::io::ErrorKind::Interrupted => "InterruptedError",
                _ => "OSError",
            },
        }
    }
}

/// `run(argv, capture_output=True, text=True, timeout=...)`.
pub type Run<'a> = &'a dyn Fn(&[String], Duration) -> Result<RunOutput, RunError>;

/// `subprocess.run(argv, capture_output=True, text=True, timeout=timeout)`: stdout captured and decoded, stderr
/// captured and dropped, and the child killed when it outlives the timeout.
pub fn subprocess_run(argv: &[String], timeout: Duration) -> Result<RunOutput, RunError> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| RunError::Os(std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty argv")))?;
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Drain both pipes on their own threads so a chatty child never blocks on a full pipe while we wait.
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = out.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = err.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = out_reader.join();
            let _ = err_reader.join();
            return Err(RunError::Timeout { argv: argv.to_vec(), timeout });
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    let stdout = out_reader.join().unwrap_or_default();
    let _ = err_reader.join();
    Ok(RunOutput { returncode: returncode(&status), stdout: String::from_utf8_lossy(&stdout).into_owned() })
}

/// Python's `returncode`: the exit code, or minus the signal that killed the child.
fn returncode(status: &std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return -sig;
        }
    }
    -1
}

// ---------------------------------------------------------------------------------------------------------------
// Recognising Claude Code processes
// ---------------------------------------------------------------------------------------------------------------

/// `os.path.basename`: everything after the last `/`.
fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// A Claude Code process: the native binary (`claude`, or its full path), the npm install's
/// `node …/claude-code/cli.js`, or that install launched through its bin shim, which ps shows as
/// `node /opt/homebrew/bin/claude` (review, 2026-09-23: such a session was judged dead and dropped).
///
/// Python: `_is_claude` (work_sessions uses it too, so it is public here).
pub fn is_claude(args: &str) -> bool {
    let words: Vec<&str> = args.split_whitespace().collect();
    let Some(first) = words.first() else {
        return false;
    };
    if basename(first) == "claude" {
        return true;
    }
    if RUNTIMES.contains(&basename(first)) && words.len() > 1 && basename(words[1]) == "claude" {
        return true;
    }
    words.iter().skip(1).take(2).any(|w| w.contains("claude-code/cli"))
}

/// A claude process an owner types into: none of HEADLESS_FLAGS on its command line.
pub fn interactive(args: &str) -> bool {
    !args
        .split_whitespace()
        .map(|w| w.split_once('=').map_or(w, |(flag, _)| flag))
        .any(|flag| HEADLESS_FLAGS.contains(&flag))
}

/// The working folders of every running Claude Code process, or None when that cannot be told.
pub fn live_cwds(run: Run<'_>) -> Option<HashSet<String>> {
    live_processes(run).map(|found| found.into_keys().collect())
}

/// Each running Claude Code process's working folder → whether any process there is interactive
/// (`interactive`), or None when that cannot be told.
pub fn live_processes(run: Run<'_>) -> Option<IndexMap<String, bool>> {
    let timeout = Duration::from_secs_f64(PROBE_TIMEOUT_SECS);
    let argv: Vec<String> = [PS, "-axww", "-o", "pid=,args="].iter().map(|s| s.to_string()).collect();
    let ps = match run(&argv, timeout) {
        Ok(out) => out,
        Err(e) => {
            tracing::debug!("claude_live: ps failed ({})", e.type_name());
            return None;
        }
    };
    if ps.returncode != 0 {
        return None;
    }
    let mut pids: IndexMap<String, bool> = IndexMap::new();
    for line in ps.stdout.lines() {
        let line = line.trim();
        let (pid, args) = line.split_once(' ').unwrap_or((line, ""));
        if !pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit()) && is_claude(args.trim()) {
            pids.insert(pid.to_string(), interactive(args));
        }
    }
    if pids.is_empty() {
        return None; // no claude at all is more likely a blind probe
    }
    // -b: never block on a stalled mount; -w: no warnings. A pid gone since ps makes lsof exit 1 and
    // still print the rest, so its output is read whatever the exit code.
    let joined = pids.keys().cloned().collect::<Vec<_>>().join(",");
    let argv: Vec<String> = [LSOF, "-a", "-b", "-w", "-d", "cwd", "-Fn", "-p", joined.as_str()]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let lsof = match run(&argv, timeout) {
        Ok(out) => out,
        Err(e) => {
            tracing::debug!("claude_live: lsof failed ({})", e.type_name());
            return None;
        }
    };
    let mut cwds: IndexMap<String, bool> = IndexMap::new();
    let mut pid = String::new();
    for line in lsof.stdout.lines() {
        if let Some(rest) = line.strip_prefix('p') {
            pid = rest.to_string();
        } else if line.starts_with("n/") {
            let folder = &line[1..];
            let typed = pids.get(&pid).copied().unwrap_or(false);
            let entry = cwds.entry(folder.to_string()).or_insert(false);
            *entry = *entry || typed;
        }
    }
    if cwds.is_empty() {
        None
    } else {
        Some(cwds)
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Folder matching
// ---------------------------------------------------------------------------------------------------------------

/// `os.path.realpath(path)` (Python 3.13, `strict=False`): absolute, `.` and `..` folded, every symlink on the
/// way resolved; a missing part is kept as written, and a symlink loop leaves the looping link unresolved.
fn realpath(filename: &str) -> std::io::Result<String> {
    const SEP: char = '/';
    // The stack of unresolved parts; `None` marks a symlink whose target has just been resolved, and the
    // symlink path itself is the entry under it.
    let mut rest: Vec<Option<String>> = filename.split(SEP).rev().map(|s| Some(s.to_string())).collect();
    let mut part_count = rest.len();
    let mut path: String = if filename.starts_with(SEP) {
        "/".to_string()
    } else {
        std::env::current_dir()?.to_string_lossy().into_owned()
    };
    let mut seen: std::collections::HashMap<String, Option<String>> = std::collections::HashMap::new();

    while part_count > 0 {
        let Some(name) = rest.pop() else { break };
        let Some(name) = name else {
            // resolved symlink target
            if let Some(Some(link)) = rest.pop() {
                seen.insert(link, Some(path.clone()));
            }
            continue;
        };
        part_count -= 1;
        if name.is_empty() || name == "." {
            continue;
        }
        if name == ".." {
            let cut = path.rfind(SEP).unwrap_or(0);
            path.truncate(cut);
            if path.is_empty() {
                path.push(SEP);
            }
            continue;
        }
        let newpath = if path == "/" { format!("/{name}") } else { format!("{path}/{name}") };
        let meta = match std::fs::symlink_metadata(&newpath) {
            Ok(m) => m,
            Err(_) => {
                path = newpath;
                continue;
            }
        };
        if !meta.file_type().is_symlink() {
            path = newpath;
            continue;
        }
        if let Some(cached) = seen.get(&newpath) {
            match cached {
                Some(resolved) => {
                    path = resolved.clone();
                }
                None => {
                    // The symlink is not resolved, so we must have a symlink loop.
                    path = newpath;
                }
            }
            continue;
        }
        let target = match std::fs::read_link(&newpath) {
            Ok(t) => t.to_string_lossy().into_owned(),
            Err(_) => {
                path = newpath;
                continue;
            }
        };
        if target.starts_with(SEP) {
            path = "/".to_string();
        }
        seen.insert(newpath.clone(), None);
        rest.push(Some(newpath));
        rest.push(None);
        let parts: Vec<Option<String>> = target.split(SEP).rev().map(|s| Some(s.to_string())).collect();
        part_count += parts.len();
        rest.extend(parts);
    }
    Ok(path)
}

/// `Path(path).resolve()`, falling back to `Path(path)` when that fails.
fn resolved(path: &str) -> PathBuf {
    match realpath(path) {
        Ok(p) => PathBuf::from(p),
        Err(_) => PathBuf::from(path),
    }
}

/// A claude process works in this folder or in one above it.
pub fn alive<I, S>(cwd: &str, live: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let here = resolved(cwd);
    for folder in live {
        let there = resolved(folder.as_ref());
        if here == there || here.ancestors().skip(1).any(|a| a == there) {
            return true;
        }
    }
    false
}

/// `Path(cwd).name`, for the log line.
fn folder_name(cwd: &str) -> String {
    match Path::new(cwd).components().next_back() {
        Some(Component::Normal(n)) => n.to_string_lossy().into_owned(),
        Some(Component::ParentDir) => "..".to_string(),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The picker
// ---------------------------------------------------------------------------------------------------------------

/// One known session, as the picker reads it from the daemon's state.
#[derive(Debug, Clone, PartialEq)]
pub struct KnownSession {
    pub session_id: String,
    pub cwd: Option<String>,
    /// `time.time()` epoch seconds.
    pub started_at: f64,
}

/// What the picker needs of the daemon's state (`state.sessions` and `state.session_end`).
pub trait SessionBook {
    /// Every known session, in the state's own order.
    fn known_sessions(&self) -> Vec<KnownSession>;
    /// Forget a session, as a SessionEnd hook would.
    fn session_end(&mut self, session_id: &str);
}

impl SessionBook for eve_state::State {
    fn known_sessions(&self) -> Vec<KnownSession> {
        self.sessions
            .values()
            .map(|s| KnownSession { session_id: s.session_id.clone(), cwd: s.cwd.clone(), started_at: s.started_at })
            .collect()
    }

    fn session_end(&mut self, session_id: &str) {
        eve_state::State::session_end(self, session_id);
    }
}

fn wall_time() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// A liveness probe: the live folders, or None when that cannot be told.
pub type Probe<'a> = &'a dyn Fn() -> Option<HashSet<String>>;

/// The "claude on" picker's folders, newest first, without the sessions whose process is gone. Those
/// are dropped from `state` too, as a SessionEnd would have: a hook from one that was alive after all
/// brings it straight back (the tool hooks adopt an unknown session). `unknown`: the interactive claude
/// processes' folders; each that no kept session is in follows them (after a restart, all of them).
///
/// `live` None and `probe` None probe the Mac with `live_cwds(subprocess_run)`.
pub fn picker_sessions<S: SessionBook + ?Sized>(
    state: &mut S,
    live: Option<HashSet<String>>,
    probe: Option<Probe<'_>>,
    now: Option<f64>,
    unknown: &[String],
) -> Vec<String> {
    let mut sessions = state.known_sessions();
    // `sorted(..., reverse=True)` is stable: equal times keep the state's order.
    sessions.sort_by(|a, b| b.started_at.partial_cmp(&a.started_at).unwrap_or(std::cmp::Ordering::Equal));
    let live = match live {
        Some(l) => Some(l),
        None => match probe {
            Some(p) => p(),
            None => live_cwds(&subprocess_run),
        },
    };
    let Some(live) = live else {
        return sessions.into_iter().filter_map(|s| s.cwd.filter(|c| !c.is_empty())).collect();
    };
    let at = now.unwrap_or_else(wall_time);
    let mut keep: Vec<String> = Vec::new();
    for s in sessions {
        let Some(cwd) = s.cwd.filter(|c| !c.is_empty()) else {
            continue; // no folder: never in the picker, and nothing to judge by
        };
        if alive(&cwd, &live) || at - s.started_at < GRACE_SECS {
            keep.push(cwd);
            continue;
        }
        state.session_end(&s.session_id);
        tracing::info!("claude_live: dropped a session whose Claude process is gone ({})", folder_name(&cwd));
    }
    let mut unknown: Vec<&String> = unknown.iter().collect();
    unknown.sort();
    for folder in unknown {
        if !keep.contains(folder) && !keep.iter().any(|k| alive(k, [folder])) {
            keep.push(folder.clone());
        }
    }
    keep
}

/// A probe that can be moved onto a worker thread.
pub type BoxedProbe = Box<dyn FnOnce() -> Option<HashSet<String>> + Send + 'static>;

/// `picker_sessions` for code on the event loop: the probe (two subprocesses, up to 1.5 s each) runs on
/// a worker thread, and only the pruning of `state`, which the loop owns, runs on the loop.
pub async fn picker_sessions_off_loop<S: SessionBook + ?Sized>(state: &mut S, probe: Option<BoxedProbe>) -> Vec<String> {
    picker_sessions_off_loop_with(state, probe, || live_processes(&subprocess_run)).await
}

/// `picker_sessions_off_loop` with the process probe (`live_processes`) as a parameter, the seam the Python
/// tests reach by patching the module function.
pub async fn picker_sessions_off_loop_with<S, F>(state: &mut S, probe: Option<BoxedProbe>, processes: F) -> Vec<String>
where
    S: SessionBook + ?Sized,
    F: FnOnce() -> Option<IndexMap<String, bool>> + Send + 'static,
{
    if let Some(probe) = probe {
        let live = to_thread(probe).await;
        let blind = || None;
        return match live {
            Some(l) => picker_sessions(state, Some(l), None, None, &[]),
            None => picker_sessions(state, None, Some(&blind), None, &[]),
        };
    }
    let Some(found) = to_thread(processes).await else {
        let blind = || None;
        return picker_sessions(state, None, Some(&blind), None, &[]);
    };
    let unknown: Vec<String> = found.iter().filter(|(_, typed)| **typed).map(|(c, _)| c.clone()).collect();
    let live: HashSet<String> = found.into_keys().collect();
    picker_sessions(state, Some(live), None, None, &unknown)
}

/// `asyncio.to_thread`: run on the blocking pool; a panic there surfaces here, as the exception would.
async fn to_thread<T, F>(f: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(v) => v,
        Err(e) => match e.try_into_panic() {
            Ok(payload) => std::panic::resume_unwind(payload),
            Err(e) => panic!("claude_live: probe task cancelled: {e}"),
        },
    }
}

#[cfg(test)]
mod tests;
