//! `watchfiles.awatch` (1.3.0) as the tailer calls it: `awatch(*roots, recursive=True, step=500)` with the
//! default `DefaultFilter`, 1600 ms debounce and the `WATCHFILES_*` environment switches.
//!
//! watchfiles is itself a thin layer over the `notify` crate (8.x, the same major version used here), so this
//! port keeps its event mapping, its batching loop (sleep a step; yield once the set stops growing or the
//! debounce runs out) and its filter, and drops only the Python plumbing.

use std::collections::HashSet;
use std::io::ErrorKind as IoErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use notify::event::{Event, EventKind, ModifyKind, RenameMode};
use notify::{Config as NotifyConfig, ErrorKind as NotifyErrorKind, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;
use regex::Regex;

/// `watchfiles.Change`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Change {
    Added = 1,
    Modified = 2,
    Deleted = 3,
}

impl Change {
    fn from_raw(raw: u8) -> Option<Change> {
        match raw {
            1 => Some(Change::Added),
            2 => Some(Change::Modified),
            3 => Some(Change::Deleted),
            _ => None,
        }
    }
}

/// `watchfiles.main.FileChange`: a change and the raw path it happened to.
pub type FileChange = (Change, String);

const CHANGE_ADDED: u8 = 1;
const CHANGE_MODIFIED: u8 = 2;
const CHANGE_DELETED: u8 = 3;

/// The exceptions `RustNotify` raises, by Python class.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WatchError {
    #[error("FileNotFoundError: {0}")]
    FileNotFound(String),
    #[error("PermissionError: {0}")]
    PermissionDenied(String),
    #[error("OSError: {0}")]
    Os(String),
    /// `WatchfilesRustInternalError`.
    #[error("WatchfilesRustInternalError: {0}")]
    Internal(String),
}

/// awatch's settings: the arguments the tailer passes plus what watchfiles reads from the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchConfig {
    pub debounce_ms: u64,
    pub step_ms: u64,
    /// `_calc_async_timeout(None)`: the watch loop returns "timeout" after this long with no changes, and awatch
    /// logs it at debug level and starts again.
    pub rust_timeout_ms: u64,
    pub debug: bool,
    pub force_polling: bool,
    pub poll_delay_ms: u64,
    pub recursive: bool,
    pub ignore_permission_denied: bool,
}

impl WatchConfig {
    /// `awatch(..., step=step_ms)` with every other argument at its default, read from the process environment.
    pub fn from_env(step_ms: u64) -> Self {
        Self::from_lookup(step_ms, |k| std::env::var_os(k).map(|v| v.to_string_lossy().into_owned()), auto_force_polling())
    }

    /// The same with an explicit environment and WSL answer (for tests, which must not mutate the process env).
    pub fn from_lookup(step_ms: u64, env: impl Fn(&str) -> Option<String>, on_wsl: bool) -> Self {
        let non_empty = |k: &str| env(k).filter(|v| !v.is_empty());
        let force_polling = match non_empty("WATCHFILES_FORCE_POLLING") {
            Some(v) => !matches!(v.to_lowercase().as_str(), "false" | "disable" | "disabled"),
            None => on_wsl,
        };
        let poll_delay_ms = non_empty("WATCHFILES_POLL_DELAY_MS")
            .filter(|v| v.chars().all(|c| crate::py::decimal_value(c).is_some()))
            .and_then(|v| crate::py::parse_int_str(&v).ok())
            .map(|i| i.value.max(0) as u64)
            .unwrap_or(300);
        WatchConfig {
            debounce_ms: 1_600,
            step_ms,
            rust_timeout_ms: if cfg!(windows) { 1_000 } else { 5_000 },
            debug: non_empty("WATCHFILES_DEBUG").is_some(),
            force_polling,
            poll_delay_ms,
            recursive: true,
            ignore_permission_denied: non_empty("WATCHFILES_IGNORE_PERMISSION_DENIED").is_some(),
        }
    }
}

/// `_auto_force_polling`: on only under WSL (`'microsoft-standard'` in the kernel release on Linux).
fn auto_force_polling() -> bool {
    #[cfg(unix)]
    {
        let mut u: libc::utsname = unsafe { std::mem::zeroed() };
        // SAFETY: uname writes only into `u`.
        if unsafe { libc::uname(&mut u) } != 0 {
            return false;
        }
        let field = |f: &[libc::c_char]| {
            let bytes: Vec<u8> = f.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
            String::from_utf8_lossy(&bytes).to_lowercase()
        };
        field(&u.release).contains("microsoft-standard") && field(&u.sysname) == "linux"
    }
    #[cfg(not(unix))]
    {
        false
    }
}

// ---- DefaultFilter ---------------------------------------------------------------------------------------------

/// `DefaultFilter.ignore_dirs`.
pub const IGNORE_DIRS: [&str; 11] = [
    "__pycache__",
    ".git",
    ".hg",
    ".svn",
    ".tox",
    ".venv",
    ".idea",
    "node_modules",
    ".mypy_cache",
    ".pytest_cache",
    ".hypothesis",
];

/// `DefaultFilter.ignore_entity_patterns`, searched in the last path part. Python's `$` also matches before a
/// trailing newline, hence `\n?\z`.
static IGNORE_ENTITY: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [r"\.py[cod]\n?\z", r"\.___jb_...___\n?\z", r"\.sw.\n?\z", r"~\n?\z", r"\A\.\#", r"\A\.DS_Store\n?\z", r"\Aflycheck_"]
        .iter()
        .map(|p| Regex::new(p).expect("DefaultFilter pattern compiles"))
        .collect()
});

/// `DefaultFilter()(change, path)`: false for a path inside an ignored directory or naming an ignored entity.
pub fn default_filter(path: &str) -> bool {
    let sep = std::path::MAIN_SEPARATOR;
    let parts: Vec<&str> = path.trim_start_matches(sep).split(sep).collect();
    if parts.iter().any(|p| IGNORE_DIRS.contains(p)) {
        return false;
    }
    let entity = parts.last().copied().unwrap_or("");
    !IGNORE_ENTITY.iter().any(|r| r.is_match(entity))
}

// ---- RustNotify ------------------------------------------------------------------------------------------------

/// A source of change batches, the shape `async for changes in awatch(...)` has. `None` is the end of the
/// iteration; the real watcher never ends.
pub trait ChangeSource: Send {
    fn next_batch(&mut self) -> BoxFuture<'_, Option<Result<HashSet<FileChange>, WatchError>>>;
}

#[allow(dead_code)]
enum WatcherEnum {
    Poll(PollWatcher),
    Recommended(RecommendedWatcher),
}

type Changes = Arc<Mutex<HashSet<(u8, String)>>>;
type ErrorSlot = Arc<Mutex<Option<String>>>;

/// `awatch(*paths, ...)`: a running notify watcher and the batching loop over what it reports.
pub struct Awatch {
    _watcher: WatcherEnum,
    changes: Changes,
    error: ErrorSlot,
    config: WatchConfig,
}

fn map_watch_error(error: notify::Error) -> WatchError {
    let err_string = error.to_string();
    match &error.kind {
        NotifyErrorKind::PathNotFound => return WatchError::FileNotFound(err_string),
        NotifyErrorKind::Generic(err) if err.as_str() == "Input watch path is neither a file nor a directory." => {
            return WatchError::FileNotFound(err_string);
        }
        NotifyErrorKind::Io(io_error) => match io_error.kind() {
            IoErrorKind::NotFound => return WatchError::FileNotFound(err_string),
            IoErrorKind::PermissionDenied => return WatchError::PermissionDenied(err_string),
            _ => {}
        },
        _ => {}
    }
    WatchError::Os(format!("{err_string} ({error:?})"))
}

fn event_handler(changes: Changes, error: ErrorSlot, debug: bool) -> impl Fn(notify::Result<Event>) + Clone + Send + 'static {
    move |res: notify::Result<Event>| match res {
        Ok(event) => {
            let Some(path_buf) = event.paths.first() else {
                if debug {
                    eprintln!("raw-event={event:?} no paths found");
                }
                return;
            };
            let path = match path_buf.to_str() {
                Some(s) => s.to_string(),
                None => {
                    *error.lock() = Some(format!("Unable to decode path {path_buf:?} to string"));
                    return;
                }
            };
            let change = match event.kind {
                EventKind::Create(_) => CHANGE_ADDED,
                EventKind::Modify(ModifyKind::Metadata(_))
                | EventKind::Modify(ModifyKind::Data(_))
                | EventKind::Modify(ModifyKind::Other)
                | EventKind::Modify(ModifyKind::Any) => {
                    // These events sometimes happen when creating files and deleting them, hence these checks.
                    let set = changes.lock();
                    if set.contains(&(CHANGE_DELETED, path.clone())) || set.contains(&(CHANGE_ADDED, path.clone())) {
                        // The file was already deleted or added in this batch.
                        return;
                    }
                    CHANGE_MODIFIED
                }
                EventKind::Modify(ModifyKind::Name(RenameMode::From)) => CHANGE_DELETED,
                EventKind::Modify(ModifyKind::Name(RenameMode::To)) => CHANGE_ADDED,
                // RenameMode::Both duplicates RenameMode::From and RenameMode::To.
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => return,
                EventKind::Modify(ModifyKind::Name(_)) => {
                    // On macOS a rename gives no hint which side this path is, so check whether it exists.
                    if Path::new(&path).exists() {
                        CHANGE_ADDED
                    } else {
                        CHANGE_DELETED
                    }
                }
                EventKind::Remove(_) => CHANGE_DELETED,
                event_kind => {
                    if debug {
                        eprintln!("raw-event={event_kind:?} event.kind={event_kind:?} no change detected");
                    }
                    return;
                }
            };
            if debug {
                eprintln!("raw-event={event:?} change={change:?}");
            }
            changes.lock().insert((change, path));
        }
        Err(e) => {
            if debug {
                eprintln!("raw-error={e:?} error.kind={:?} error.paths={:?}", e.kind, e.paths);
            }
            // An IO error for a file that is gone reports the file as deleted rather than failing the watch.
            if let NotifyErrorKind::Io(io_error) = &e.kind {
                if io_error.kind() == IoErrorKind::NotFound {
                    changes.lock().extend(e.paths.iter().map(|p| (CHANGE_DELETED, p.to_string_lossy().into_owned())));
                    return;
                }
            }
            *error.lock() = Some(format!("error in underlying watcher: {e}"));
        }
    }
}

fn watch_paths<W: Watcher>(watcher: &mut W, paths: &[PathBuf], config: &WatchConfig) -> Result<(), WatchError> {
    let mode = if config.recursive { RecursiveMode::Recursive } else { RecursiveMode::NonRecursive };
    for p in paths {
        if let Err(err) = watcher.watch(p, mode) {
            let err = map_watch_error(err);
            if !config.ignore_permission_denied {
                return Err(err);
            }
        }
    }
    Ok(())
}

impl Awatch {
    /// `RustNotify(paths, debug, force_polling, poll_delay_ms, recursive, ignore_permission_denied)`.
    pub fn new(paths: &[PathBuf], config: WatchConfig) -> Result<Self, WatchError> {
        let changes: Changes = Arc::new(Mutex::new(HashSet::new()));
        let error: ErrorSlot = Arc::new(Mutex::new(None));
        let handler = event_handler(changes.clone(), error.clone(), config.debug);
        let poll = |handler| -> Result<WatcherEnum, WatchError> {
            if paths.iter().any(|p| !p.exists()) {
                return Err(WatchError::FileNotFound("No such file or directory".into()));
            }
            let cfg = NotifyConfig::default().with_poll_interval(Duration::from_millis(config.poll_delay_ms));
            let mut watcher =
                PollWatcher::new(handler, cfg).map_err(|e| WatchError::Internal(format!("Error creating poll watcher: {e}")))?;
            watch_paths(&mut watcher, paths, &config)?;
            if config.debug {
                eprintln!("watcher: {watcher:?}");
            }
            Ok(WatcherEnum::Poll(watcher))
        };
        let watcher = if config.force_polling {
            poll(handler)?
        } else {
            match RecommendedWatcher::new(handler.clone(), NotifyConfig::default()) {
                Ok(mut watcher) => {
                    watch_paths(&mut watcher, paths, &config)?;
                    if config.debug {
                        eprintln!("watcher: {watcher:?}");
                    }
                    WatcherEnum::Recommended(watcher)
                }
                // ENOSYS: fall back to polling.
                Err(e) if matches!(&e.kind, NotifyErrorKind::Io(io) if io.raw_os_error() == Some(38)) => {
                    if config.debug {
                        eprintln!("IO error using recommend watcher: {e:?}, falling back to PollWatcher");
                    }
                    poll(handler).map_err(|e| match e {
                        WatchError::Internal(m) => {
                            WatchError::Internal(m.replace("Error creating poll watcher", "Error creating fallback poll watcher"))
                        }
                        other => other,
                    })?
                }
                Err(e) => return Err(WatchError::Internal(format!("Error creating recommended watcher: {e}"))),
            }
        };
        Ok(Awatch { _watcher: watcher, changes, error, config })
    }

    /// `RustNotify.watch(debounce, step, timeout, stop_event)`: `None` on "timeout".
    async fn watch_once(&mut self) -> Result<Option<HashSet<(u8, String)>>, WatchError> {
        let step = Duration::from_millis(self.config.step_ms);
        let mut max_debounce_time: Option<Instant> = None;
        let mut last_size = 0usize;
        let max_timeout_time =
            if self.config.rust_timeout_ms == 0 { None } else { Some(Instant::now() + Duration::from_millis(self.config.rust_timeout_ms)) };
        loop {
            tokio::time::sleep(step).await;
            if let Some(error) = self.error.lock().clone() {
                self.changes.lock().clear();
                return Err(WatchError::Internal(error));
            }
            let size = self.changes.lock().len();
            if size > 0 {
                if size == last_size {
                    break;
                }
                last_size = size;
                let now = Instant::now();
                match max_debounce_time {
                    Some(max_time) if now > max_time => break,
                    Some(_) => {}
                    None => max_debounce_time = Some(now + Duration::from_millis(self.config.debounce_ms)),
                }
            } else if let Some(max_time) = max_timeout_time {
                if Instant::now() > max_time {
                    self.changes.lock().clear();
                    return Ok(None);
                }
            }
        }
        let mut set = self.changes.lock();
        Ok(Some(std::mem::take(&mut *set)))
    }

    /// The next non-empty, filtered batch (one iteration of `async for changes in awatch(...)`).
    pub async fn next(&mut self) -> Result<HashSet<FileChange>, WatchError> {
        loop {
            let Some(raw) = self.watch_once().await? else {
                tracing::debug!(target: "watchfiles.main", "rust notify timeout, continuing");
                continue;
            };
            let changes: HashSet<FileChange> = raw
                .iter()
                .filter_map(|(c, p)| Change::from_raw(*c).map(|c| (c, p.clone())))
                .filter(|(_, p)| default_filter(p))
                .collect();
            if changes.is_empty() {
                tracing::debug!(target: "watchfiles.main", "all changes filtered out, raw_changes={raw:?}");
                continue;
            }
            log_changes(&changes);
            return Ok(changes);
        }
    }
}

/// `_log_changes`.
fn log_changes(changes: &HashSet<FileChange>) {
    let count = changes.len();
    let plural = if count == 1 { "" } else { "s" };
    if tracing::enabled!(target: "watchfiles.main", tracing::Level::DEBUG) {
        tracing::debug!(target: "watchfiles.main", "{count} change{plural} detected: {changes:?}");
    } else {
        tracing::info!(target: "watchfiles.main", "{count} change{plural} detected");
    }
}

/// The watcher the tailer runs by default; created on the first `next_batch`, as the Python generator creates
/// its `RustNotify` on the first iteration (so a failure lands inside the tailer's "crashed" handler).
pub struct LazyAwatch {
    paths: Vec<PathBuf>,
    config: WatchConfig,
    inner: Option<Awatch>,
}

impl LazyAwatch {
    pub fn new(paths: Vec<PathBuf>, config: WatchConfig) -> Self {
        LazyAwatch { paths, config, inner: None }
    }
}

impl ChangeSource for LazyAwatch {
    fn next_batch(&mut self) -> BoxFuture<'_, Option<Result<HashSet<FileChange>, WatchError>>> {
        Box::pin(async move {
            if self.inner.is_none() {
                match Awatch::new(&self.paths, self.config.clone()) {
                    Ok(w) => self.inner = Some(w),
                    Err(e) => return Some(Err(e)),
                }
            }
            let watcher = self.inner.as_mut()?;
            Some(watcher.next().await)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_filter_matches_watchfiles() {
        assert!(default_filter("/Users/x/.claude/projects/p/s.jsonl"));
        assert!(!default_filter("/Users/x/.claude/projects/.git/s.jsonl"));
        assert!(!default_filter("/a/node_modules/b/s.jsonl"));
        assert!(!default_filter("/a/b/.#s.jsonl"));
        assert!(!default_filter("/a/b/flycheck_s.jsonl"));
        assert!(!default_filter("/a/b/s.jsonl~"));
        assert!(!default_filter("/a/b/x.pyc"));
        assert!(!default_filter("/a/b/.s.jsonl.swp"));
        assert!(!default_filter("/a/b/.DS_Store"));
        assert!(default_filter("/a/b/.DS_Store.jsonl"));
        assert!(default_filter("/a/.gitx/s.jsonl"));
    }

    #[test]
    fn config_reads_watchfiles_env() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
        };
        let c = WatchConfig::from_lookup(500, env(&[]), false);
        assert_eq!((c.debounce_ms, c.step_ms, c.poll_delay_ms, c.force_polling, c.debug), (1600, 500, 300, false, false));
        assert!(c.recursive && !c.ignore_permission_denied);
        assert!(WatchConfig::from_lookup(500, env(&[]), true).force_polling);
        assert!(WatchConfig::from_lookup(500, env(&[("WATCHFILES_FORCE_POLLING", "yes")]), false).force_polling);
        assert!(!WatchConfig::from_lookup(500, env(&[("WATCHFILES_FORCE_POLLING", "Disabled")]), true).force_polling);
        assert!(WatchConfig::from_lookup(500, env(&[("WATCHFILES_FORCE_POLLING", "")]), true).force_polling);
        assert_eq!(WatchConfig::from_lookup(500, env(&[("WATCHFILES_POLL_DELAY_MS", "50")]), false).poll_delay_ms, 50);
        assert_eq!(WatchConfig::from_lookup(500, env(&[("WATCHFILES_POLL_DELAY_MS", "٥٠")]), false).poll_delay_ms, 50);
        assert_eq!(WatchConfig::from_lookup(500, env(&[("WATCHFILES_POLL_DELAY_MS", " 50")]), false).poll_delay_ms, 300);
        assert_eq!(WatchConfig::from_lookup(500, env(&[("WATCHFILES_POLL_DELAY_MS", "-5")]), false).poll_delay_ms, 300);
        assert!(WatchConfig::from_lookup(500, env(&[("WATCHFILES_DEBUG", "1")]), false).debug);
        assert!(WatchConfig::from_lookup(500, env(&[("WATCHFILES_IGNORE_PERMISSION_DENIED", "0")]), false).ignore_permission_denied);
    }

    #[test]
    fn missing_path_is_file_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("nope");
        let mut cfg = WatchConfig::from_lookup(50, |_| None, false);
        assert!(matches!(Awatch::new(std::slice::from_ref(&gone), cfg.clone()), Err(WatchError::FileNotFound(_))));
        cfg.force_polling = true;
        assert!(matches!(Awatch::new(&[gone], cfg), Err(WatchError::FileNotFound(_))));
    }

    #[tokio::test]
    async fn awatch_reports_a_new_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let mut cfg = WatchConfig::from_lookup(50, |_| None, false);
        cfg.debounce_ms = 300;
        let mut w = Awatch::new(std::slice::from_ref(&root), cfg).unwrap();
        // FSEvents can report events from just before the watch started; give it a moment, then write.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let file = root.join("s.jsonl");
        std::fs::write(&file, "{}\n").unwrap();
        let want = file.to_str().unwrap().to_string();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let batch = tokio::time::timeout(Duration::from_secs(20), w.next()).await.expect("a batch").unwrap();
            if batch.iter().any(|(c, p)| *p == want && matches!(c, Change::Added | Change::Modified)) {
                break;
            }
            assert!(Instant::now() < deadline, "no event for {want}: {batch:?}");
        }
    }
}
