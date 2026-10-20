//! The process firewall: the lite daemon's, nowhere else (the second half of `profile.py`).
//!
//! Python's firewall is an audit hook: the interpreter reports every process start and file open to it. Rust has no
//! such hook, so the same guard is called where eve's Rust code starts a program or touches a file: [`audit`] with
//! the [`Event`] Python would have raised, or the helpers that read a `std::process::Command` the way Python reads a
//! `subprocess.Popen` ([`audit_command`], [`spawn`], [`output`], [`status`]) and a path the way Python reads an
//! `open` ([`audit_path`]). Once [`install_firewall`] has run, the guard stays for the life of the process, as the
//! hook does; with none installed every check lets everything through.
//!
//! - A process may start only when its program is one of [`EXEC_ALLOW`]: pmset (the battery answer), ifconfig (the
//!   watcher's address guard), scutil (the LAN certificate's name). Everything else is refused, the developer-tool
//!   shims first (toolchain: on a Mac without Xcode's tools, git, clang, make or python3 opens their install
//!   dialog), then osascript, screencapture, codex, claude, cloudflared, adb and the shell itself.
//! - A file inside the folders macOS guards with a privacy prompt ([`PROTECTED_HOME`], [`PROTECTED_ROOTS`]: Desktop,
//!   Documents, Downloads, iCloud Drive, Mail, Messages, other apps' data, external volumes) is refused to open,
//!   list, create, move or delete, as is a path that reaches one through symlinks.
//!
//! A refusal is a PermissionError (`io::ErrorKind::PermissionDenied`, errno EPERM) where the call was made. The guard
//! logs each program or folder name once, never a path, an argument or a value. Mode "audit" refuses nothing and only
//! counts and logs, to find what a new code path starts.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use indexmap::IndexMap;
use parking_lot::Mutex;
use unicode_normalization::UnicodeNormalization;

use crate::pypath;
use crate::ProfileError;

/// The programs eve lite may start.
pub const EXEC_ALLOW: &[&str] = &[
    "/usr/bin/pmset",   // the battery quick answer (quick_answers)
    "/sbin/ifconfig",   // the watcher's guard against pages that reach this Mac's own network (watch)
    "/usr/sbin/scutil", // the Mac's LocalHostName, the LAN certificate's name (lan_net)
];
/// Folders under the home that macOS guards with a privacy prompt (or Full Disk Access): the spec's list, then the
/// same kind it did not name: cloud drives' File Provider folders (~/Dropbox and its like are links into it),
/// Reminders, HomeKit, cookies, call history, iPhone backups, the privacy database itself and the Trash.
pub const PROTECTED_HOME: &[&str] = &[
    "Desktop", "Documents", "Downloads", "Pictures", "Movies", "Music",
    "Library/Mobile Documents", "Library/Mail", "Library/Messages", "Library/Calendars",
    "Library/Application Support/AddressBook", "Library/Safari", "Library/Containers", "Library/Group Containers",
    "Library/CloudStorage", "Library/Reminders", "Library/HomeKit", "Library/Cookies",
    "Library/Application Support/CallHistoryDB", "Library/Application Support/MobileSync",
    "Library/Application Support/com.apple.TCC", ".Trash",
];
/// External and network disks.
pub const PROTECTED_ROOTS: &[&str] = &["/Volumes"];
pub const FIREWALL_MODES: &[&str] = &["enforce", "audit"];

/// What os.system and shell=True run: it can run anything, so it is never allowed.
const SHELL: &str = "/bin/sh";
/// The home folders again under the data volume's own name (a firmlink, no symlink).
const DATA_VOLUME: &str = "/System/Volumes/Data";
/// Symlinks one path may pass through before it is a loop (the kernel allows 32).
const MAX_LINKS: i32 = 40;
/// Python's wrapped `_posixsubprocess.fork_exec` event (the C function raises no audit event of its own).
const FORK_EXEC: &str = "_posixsubprocess.fork_exec";

// ---- what the guard is told -----------------------------------------------------------------------------------------

/// One argument of an event, as Python would have passed it: a path (str, bytes or PathLike), a file descriptor,
/// None, or a value of some other type (which `os.fspath` refuses).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg {
    None,
    Fd(i64),
    Path(String),
    /// Anything else, by its type's name.
    Other(String),
}

impl Arg {
    /// `os.fsdecode(os.fspath(value))`: the path's text, or None where Python raises TypeError.
    fn text(&self) -> Option<&str> {
        match self {
            Arg::Path(p) => Some(p),
            _ => None,
        }
    }
}

impl From<&str> for Arg {
    fn from(v: &str) -> Self {
        Arg::Path(v.to_string())
    }
}
impl From<String> for Arg {
    fn from(v: String) -> Self {
        Arg::Path(v)
    }
}
impl From<&String> for Arg {
    fn from(v: &String) -> Self {
        Arg::Path(v.clone())
    }
}
impl From<&Path> for Arg {
    fn from(v: &Path) -> Self {
        Arg::Path(v.to_string_lossy().into_owned())
    }
}
impl From<PathBuf> for Arg {
    fn from(v: PathBuf) -> Self {
        Arg::Path(v.to_string_lossy().into_owned())
    }
}
impl From<&PathBuf> for Arg {
    fn from(v: &PathBuf) -> Self {
        Arg::Path(v.to_string_lossy().into_owned())
    }
}
impl From<i32> for Arg {
    fn from(v: i32) -> Self {
        Arg::Fd(v as i64)
    }
}
impl<T: Into<Arg>> From<Option<T>> for Arg {
    fn from(v: Option<T>) -> Self {
        v.map(Into::into).unwrap_or(Arg::None)
    }
}

/// The file events, each with the positions of its path arguments (os.symlink: only where the link is made).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileEvent {
    Open,
    Listdir,
    Scandir,
    SqliteConnect,
    Mkdir,
    Rmdir,
    Remove,
    Rename,
    Link,
    Symlink,
    Truncate,
    Chmod,
    Chown,
    Utime,
    Chflags,
}

impl FileEvent {
    pub const ALL: [FileEvent; 15] = [
        FileEvent::Open, FileEvent::Listdir, FileEvent::Scandir, FileEvent::SqliteConnect, FileEvent::Mkdir,
        FileEvent::Rmdir, FileEvent::Remove, FileEvent::Rename, FileEvent::Link, FileEvent::Symlink,
        FileEvent::Truncate, FileEvent::Chmod, FileEvent::Chown, FileEvent::Utime, FileEvent::Chflags,
    ];

    /// The Python audit event's name.
    pub fn name(self) -> &'static str {
        match self {
            FileEvent::Open => "open",
            FileEvent::Listdir => "os.listdir",
            FileEvent::Scandir => "os.scandir",
            FileEvent::SqliteConnect => "sqlite3.connect",
            FileEvent::Mkdir => "os.mkdir",
            FileEvent::Rmdir => "os.rmdir",
            FileEvent::Remove => "os.remove",
            FileEvent::Rename => "os.rename",
            FileEvent::Link => "os.link",
            FileEvent::Symlink => "os.symlink",
            FileEvent::Truncate => "os.truncate",
            FileEvent::Chmod => "os.chmod",
            FileEvent::Chown => "os.chown",
            FileEvent::Utime => "os.utime",
            FileEvent::Chflags => "os.chflags",
        }
    }

    /// The positions of the event's path arguments.
    pub fn positions(self) -> &'static [usize] {
        match self {
            FileEvent::Rename | FileEvent::Link => &[0, 1],
            FileEvent::Symlink => &[1],
            _ => &[0],
        }
    }
}

/// An event the guard judges: a process start (each the way its own call looks programs up) or a file event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// `subprocess.Popen`: (executable, args, cwd, env); executable is args[0] when unset. `env` None: inherited.
    Popen { executable: Arg, cwd: Arg, env: Option<HashMap<String, String>> },
    /// `os.posix_spawn`, for posix_spawn and posix_spawnp alike: ./name, then the PATH.
    PosixSpawn { path: Arg },
    /// `os.exec`: the program as given (an exec*p looked the PATH up already).
    Exec { path: Arg },
    /// `os.spawn`.
    Spawn { path: Arg },
    /// `os.system`: the shell.
    System,
    /// `_posixsubprocess.fork_exec`: (executable_list, cwd), the first one that runs.
    ForkExec { executables: Vec<Arg>, cwd: Arg },
    /// A file event and its arguments.
    File { event: FileEvent, args: Vec<Arg> },
}

impl Event {
    /// A file event with these arguments.
    pub fn file<I, A>(event: FileEvent, args: I) -> Event
    where
        I: IntoIterator<Item = A>,
        A: Into<Arg>,
    {
        Event::File { event, args: args.into_iter().map(Into::into).collect() }
    }

    /// The Python audit event's name.
    pub fn name(&self) -> &'static str {
        match self {
            Event::Popen { .. } => "subprocess.Popen",
            Event::PosixSpawn { .. } => "os.posix_spawn",
            Event::Exec { .. } => "os.exec",
            Event::Spawn { .. } => "os.spawn",
            Event::System => "os.system",
            Event::ForkExec { .. } => FORK_EXEC,
            Event::File { event, .. } => event.name(),
        }
    }
}

/// A refused event: the count key, what the log line names, and the error's words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub key: String,
    pub what: String,
    pub words: String,
}

/// The error a refusal raises, Python's `PermissionError(errno.EPERM, words)`: its text is "[Errno 1] <words>".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub words: String,
}

impl Refused {
    pub fn errno(&self) -> i32 {
        libc::EPERM
    }
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[Errno {}] {}", libc::EPERM, self.words)
    }
}

impl std::error::Error for Refused {}

/// The firewall's mode: enforce refuses, audit only counts and logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Enforce,
    Audit,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Enforce => "enforce",
            Mode::Audit => "audit",
        }
    }

    /// One of FIREWALL_MODES; Python's ValueError otherwise.
    pub fn parse(mode: &str) -> Result<Mode, FirewallError> {
        match mode {
            "enforce" => Ok(Mode::Enforce),
            "audit" => Ok(Mode::Audit),
            _ => Err(FirewallError::Mode(format!(
                "firewall mode is one of ('enforce', 'audit'), not {}",
                eve_app_registry::py_str_repr(mode)
            ))),
        }
    }
}

/// What install_firewall refuses: a mode that is not one of FIREWALL_MODES (Python's ValueError), or an EVE_PROFILE
/// that is neither full nor lite (ProfileError).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FirewallError {
    #[error("{0}")]
    Mode(String),
    #[error(transparent)]
    Profile(#[from] ProfileError),
}

// ---- the helpers the guard reads paths with ------------------------------------------------------------------------

/// A path as APFS compares names: case- and Unicode-normalization-insensitive.
pub(crate) fn fold(path: &str) -> String {
    if path.is_ascii() {
        path.to_ascii_lowercase()
    } else {
        caseless::default_case_fold_str(&path.nfc().collect::<String>())
    }
}

fn absolute(path: &str, base: Option<&str>) -> io::Result<String> {
    if pypath::isabs(path) {
        return Ok(path.to_string());
    }
    let base = match base {
        Some(b) if !b.is_empty() => b.to_string(),
        _ => pypath::getcwd()?,
    };
    Ok(pypath::join(&base, path))
}

/// The paths the kernel would run: regular files with the execute bit (what exec skips over, it never runs).
fn runnable(paths: Vec<String>) -> Vec<String> {
    paths.into_iter().filter(|p| pypath::runnable(p)).collect()
}

fn on_path(name: &str, dirs: Vec<String>, base: Option<&str>) -> io::Result<Vec<String>> {
    let mut candidates = Vec::new();
    for d in dirs {
        candidates.push(absolute(&pypath::join(&d, name), base)?);
    }
    Ok(runnable(candidates).into_iter().take(1).collect())
}

/// Python's TypeError or OSError while reading a start: the guard refuses it.
struct Unreadable;

impl From<io::Error> for Unreadable {
    fn from(_: io::Error) -> Self {
        Unreadable
    }
}

fn text(arg: &Arg) -> Result<&str, Unreadable> {
    arg.text().ok_or(Unreadable)
}

fn cwd_base(cwd: &Arg) -> Result<Option<String>, Unreadable> {
    match cwd {
        Arg::None => Ok(None),
        other => Ok(Some(absolute(text(other)?, None)?)),
    }
}

/// The program files a process start would run, absolute; [] when it would run nothing (a missing program fails on
/// its own). Each start is read the way its own call looks programs up.
fn programs(event: &Event) -> Result<Vec<String>, Unreadable> {
    match event {
        Event::System => Ok(vec![SHELL.to_string()]),
        Event::Popen { executable, cwd, env } => {
            let base = cwd_base(cwd)?;
            let name = text(executable)?;
            if !pypath::dirname(name).is_empty() {
                return Ok(runnable(vec![absolute(name, base.as_deref())?]));
            }
            Ok(on_path(name, pypath::get_exec_path(env.as_ref()), base.as_deref())?)
        }
        Event::PosixSpawn { path } => {
            let name = text(path)?;
            let mut here = runnable(vec![absolute(name, None)?]);
            if !pypath::dirname(name).is_empty() {
                return Ok(here);
            }
            // ./name for posix_spawn, the PATH for posix_spawnp
            here.extend(on_path(name, pypath::get_exec_path(None), None)?);
            Ok(here)
        }
        Event::Exec { path } | Event::Spawn { path } => Ok(runnable(vec![absolute(text(path)?, None)?])),
        Event::ForkExec { executables, cwd } => {
            let base = cwd_base(cwd)?;
            let mut candidates = Vec::new();
            for p in executables {
                candidates.push(absolute(text(p)?, base.as_deref())?);
            }
            Ok(runnable(candidates).into_iter().take(1).collect())
        }
        Event::File { .. } => Ok(Vec::new()),
    }
}

/// The files sqlite3.connect may open: the name as a file, and for a `file:` URI its path as well.
fn sqlite_paths(database: &Arg) -> Vec<Arg> {
    let Arg::Path(name) = database else {
        return Vec::new();
    };
    if name.is_empty() || name == ":memory:" {
        return Vec::new();
    }
    let mut paths = vec![Arg::Path(name.clone())];
    if let Some(after) = name.strip_prefix("file:") {
        let mut rest = after.split('?').next().unwrap_or("").split('#').next().unwrap_or("").to_string();
        if let Some(authority) = rest.strip_prefix("//") {
            // file://host/path: the authority is not part of the path
            let path = authority.split_once('/').map(|(_, p)| p).unwrap_or("");
            rest = format!("/{path}");
        }
        let decoded = urlencoding::decode_binary(rest.as_bytes());
        paths.push(Arg::Path(String::from_utf8_lossy(&decoded).into_owned()));
    }
    paths
}

/// Where a walk stopped: at a protected folder (by its name), or at nothing (an OSError or a loop).
enum WalkEnd {
    Reached(String),
    Nothing,
}

thread_local! {
    /// The guard's own work (a log line opening its file) is not checked again.
    static BUSY: Cell<bool> = const { Cell::new(false) };
}

type LinkCheck = Box<dyn Fn(&str) -> bool + Send + Sync>;

#[derive(Default)]
struct Tally {
    counts: IndexMap<String, u64>,
    logged: HashSet<String>,
}

/// The firewall itself: one per process (install_firewall), called with every event.
pub struct Guard {
    enforcing: AtomicBool,
    /// An exec refused in a forked child exits it (`call`).
    pid: u32,
    /// Each allowed program by its path and by the file it resolves to (a system path may become a link).
    allowed: HashSet<String>,
    roots: IndexMap<String, String>,
    prefixes: Vec<String>,
    exempt: Vec<String>,
    tally: Mutex<Tally>,
    is_link: LinkCheck,
}

impl fmt::Debug for Guard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Guard").field("mode", &self.mode()).field("pid", &self.pid).finish_non_exhaustive()
    }
}

impl Guard {
    /// A guard over these protected folders ((folder, its name in a log line)) with these code folders exempt, for
    /// the process `pid`. The programs it allows are EXEC_ALLOW.
    pub fn new<R, S>(mode: Mode, roots: R, pid: u32, exempt: &[S]) -> Guard
    where
        R: IntoIterator<Item = (String, String)>,
        S: AsRef<str>,
    {
        Guard::with_exec_allow(mode, roots, pid, exempt, EXEC_ALLOW)
    }

    /// [`Guard::new`] with another set of allowed programs (the tests' seam for Python's patched EXEC_ALLOW).
    pub fn with_exec_allow<R, S, P>(mode: Mode, roots: R, pid: u32, exempt: &[S], allow: &[P]) -> Guard
    where
        R: IntoIterator<Item = (String, String)>,
        S: AsRef<str>,
        P: AsRef<str>,
    {
        let mut allowed: HashSet<String> = allow.iter().map(|p| p.as_ref().to_string()).collect();
        allowed.extend(allow.iter().map(|p| pypath::realpath(p.as_ref())));
        let mut folded: IndexMap<String, String> = IndexMap::new();
        for (path, name) in roots {
            folded.insert(fold(&path), name);
        }
        let prefixes = folded.keys().map(|root| format!("{root}/")).collect();
        let exempt = exempt.iter().map(|p| format!("{}/", fold(p.as_ref()).trim_end_matches('/'))).collect();
        Guard {
            enforcing: AtomicBool::new(mode == Mode::Enforce),
            pid,
            allowed,
            roots: folded,
            prefixes,
            exempt,
            tally: Mutex::new(Tally::default()),
            is_link: Box::new(pypath::is_link),
        }
    }

    /// The tests' seam for Python's patched `os.lstat`: how the walk asks whether a path is a symlink.
    pub fn with_link_check(mut self, is_link: impl Fn(&str) -> bool + Send + Sync + 'static) -> Guard {
        self.is_link = Box::new(is_link);
        self
    }

    pub fn mode(&self) -> Mode {
        if self.enforcing.load(Ordering::SeqCst) {
            Mode::Enforce
        } else {
            Mode::Audit
        }
    }

    fn enforce(&self) {
        self.enforcing.store(true, Ordering::SeqCst);
    }

    /// Python's `__call__`: judge an event, count and log a refusal, and in enforce mode refuse it.
    pub fn call(&self, event: &Event) -> io::Result<()> {
        if BUSY.with(Cell::get) {
            return Ok(());
        }
        let enforcing = self.mode() == Mode::Enforce;
        // a forked child about to exit 126 (below) logs nothing: the line would be lost with its buffers
        let child = matches!(event, Event::Exec { .. }) && std::process::id() != self.pid;
        BUSY.with(|b| b.set(true));
        let refusal = catch_unwind(AssertUnwindSafe(|| {
            let refusal = self.check(event);
            if let Some(r) = &refusal {
                if !(child && enforcing) {
                    self.note(&r.key, &r.what);
                }
            }
            refusal
        }));
        BUSY.with(|b| b.set(false));
        let refusal = match refusal {
            Ok(r) => r,
            Err(panic) => std::panic::resume_unwind(panic),
        };
        match refusal {
            Some(r) if enforcing => {
                if child {
                    // a forked child whose exec is refused must not run on as a copy of the daemon
                    // SAFETY: _exit ends this process at once; nothing after it runs.
                    unsafe { libc::_exit(126) };
                }
                Err(io::Error::new(io::ErrorKind::PermissionDenied, Refused { words: r.words }))
            }
            _ => Ok(()),
        }
    }

    /// The refusal for an event (count key, what the log line names, the error's words); None when it may go.
    pub fn check(&self, event: &Event) -> Option<Refusal> {
        if let Event::File { event: kind, args } = event {
            let checked = catch_unwind(AssertUnwindSafe(|| {
                for &i in kind.positions() {
                    let Some(arg) = args.get(i) else { continue };
                    let values =
                        if *kind == FileEvent::SqliteConnect { sqlite_paths(arg) } else { vec![arg.clone()] };
                    for value in values {
                        if let Some(name) = self.reaches(&value) {
                            return Some(Refusal {
                                key: format!("path:{name}"),
                                what: format!("a file in {name}"),
                                words: format!("eve lite stays out of {name}"),
                            });
                        }
                    }
                }
                None
            }));
            // a fault in this check must not stop every file eve opens
            return checked.unwrap_or_else(|_| {
                self.fault("panic");
                None
            });
        }
        let Ok(programs) = programs(event) else {
            // a start this guard cannot read is refused, never let through
            return Some(Refusal {
                key: "exec:?".to_string(),
                what: "a program it could not read".to_string(),
                words: "eve lite does not run this program".to_string(),
            });
        };
        for program in programs {
            let real = pypath::realpath(&program);
            if !self.allowed.contains(&real) {
                let mut name = pypath::basename(&real);
                if name.is_empty() {
                    name = "?".to_string();
                }
                return Some(Refusal {
                    key: format!("exec:{name}"),
                    what: format!("the program {name}"),
                    words: format!("eve lite does not run {name}"),
                });
            }
        }
        None
    }

    /// Count a refusal, and log it the first time its key is seen.
    pub fn note(&self, key: &str, what: &str) {
        let first = {
            let mut tally = self.tally.lock();
            *tally.counts.entry(key.to_string()).or_insert(0) += 1;
            tally.logged.insert(key.to_string())
        };
        if first {
            let verb = if self.mode() == Mode::Audit { "would refuse" } else { "refused" };
            tracing::warn!("firewall: {verb} {what} (said once; firewall_counts has the count)");
        }
    }

    /// A path this guard failed to check (a fault of its own, let through): said once per kind of error.
    pub fn fault(&self, kind: &str) {
        let first = self.tally.lock().logged.insert(format!("fault:{kind}"));
        if first {
            tracing::warn!("firewall: could not check a path ({kind}); it was let through");
        }
    }

    /// What this guard refused (or, auditing, would have), by "exec:<program>" and "path:<folder>": a copy.
    pub fn counts(&self) -> IndexMap<String, u64> {
        self.tally.lock().counts.clone()
    }

    /// The protected folder a path argument is inside of, or reaches through symlinks; None for any other path, and
    /// for a file descriptor (its path was checked when it was opened).
    pub fn reaches(&self, value: &Arg) -> Option<String> {
        let path = match value {
            Arg::None => ".",               // os.listdir() and os.scandir() with no argument
            Arg::Fd(_) | Arg::Other(_) => return None,
            Arg::Path(p) => p.as_str(),
        };
        if path.is_empty() || path.contains('\0') {
            return None; // the call refuses these itself
        }
        // no working folder: a relative path reaches nothing
        let path = absolute(path, None).ok()?;
        let normal = pypath::normpath(&path);
        if let Some(name) = self.lexical(&normal) {
            if !self.in_code(&normal) {
                return Some(name);
            }
        }
        // this process's own code may live in a protected folder (a checkout under ~/Documents): its words pass,
        // but its links are still followed, so one from the checkout into ~/Desktop is refused (review A6)
        self.walk(&path)
    }

    /// Whether a path is at or inside one of this process's own code folders (`exempt`).
    pub fn in_code(&self, path: &str) -> bool {
        if self.exempt.is_empty() {
            return false;
        }
        let folded = format!("{}/", fold(path));
        self.exempt.iter().any(|e| folded.starts_with(e.as_str()))
    }

    /// Whether a path is a code folder, inside one, or a folder on the way to one: the walk may pass these.
    pub fn toward_code(&self, path: &str) -> bool {
        let folded = format!("{}/", fold(path));
        self.exempt.iter().any(|root| folded.starts_with(root.as_str()) || root.starts_with(folded.as_str()))
    }

    /// The protected folder a normalized absolute path is at or inside of, by its words alone.
    pub fn lexical(&self, path: &str) -> Option<String> {
        let path = if path.starts_with("//") {
            // normpath keeps a leading "//", which is "/" here
            format!("/{}", path.trim_start_matches('/'))
        } else {
            path.to_string()
        };
        let folded = fold(&path);
        if let Some(name) = self.roots.get(&folded) {
            return Some(name.clone());
        }
        if self.prefixes.iter().any(|p| folded.starts_with(p.as_str())) {
            return self
                .roots
                .iter()
                .find(|(root, _)| folded.starts_with(&format!("{root}/")))
                .map(|(_, n)| n.clone());
        }
        None
    }

    /// Follow `path`'s symlinks the way the kernel does (posixpath.realpath's walk), stopping short of a protected
    /// folder: nothing at or inside one is looked at, since an lstat there is itself an access macOS may ask about.
    /// The one exception is the way to this process's own code folders (`toward_code`), which it already reads.
    pub fn walk(&self, path: &str) -> Option<String> {
        let mut seen: HashMap<String, Option<String>> = HashMap::new();
        let mut links = MAX_LINKS;
        match self.follow("/", path, &mut seen, &mut links) {
            Ok(end) => {
                if self.in_code(&end) {
                    None
                } else {
                    self.lexical(&end)
                }
            }
            Err(WalkEnd::Reached(name)) => Some(name),
            Err(WalkEnd::Nothing) => None,
        }
    }

    fn follow(
        &self,
        base: &str,
        rest: &str,
        seen: &mut HashMap<String, Option<String>>,
        links: &mut i32,
    ) -> Result<String, WalkEnd> {
        let (mut base, rest) = match rest.strip_prefix('/') {
            Some(r) => ("/".to_string(), r),
            None => (base.to_string(), rest),
        };
        for part in rest.split('/') {
            if part.is_empty() || part == "." {
                continue;
            }
            if part == ".." {
                base = pypath::dirname(&base);
                continue;
            }
            let new = pypath::join(&base, part);
            if let Some(name) = self.lexical(&new) {
                if !self.toward_code(&new) {
                    return Err(WalkEnd::Reached(name));
                }
            }
            if !(self.is_link)(&new) {
                base = new;
                continue;
            }
            if let Some(resolved) = seen.get(&new) {
                match resolved {
                    None => return Err(WalkEnd::Nothing), // the kernel's ELOOP: the path reaches nothing
                    Some(resolved) => {
                        base = resolved.clone();
                        continue;
                    }
                }
            }
            *links -= 1;
            if *links < 0 {
                return Err(WalkEnd::Nothing); // too many symlinks
            }
            seen.insert(new.clone(), None);
            let target = std::fs::read_link(&new).map_err(|_| WalkEnd::Nothing)?;
            // a relative target is relative to the link's folder
            base = self.follow(&base, &target.to_string_lossy(), seen, links)?;
            seen.insert(new, Some(base.clone()));
        }
        Ok(base)
    }
}

// ---- the one guard of this process ----------------------------------------------------------------------------------

static GUARD: OnceLock<Guard> = OnceLock::new();
static GUARD_LOCK: Mutex<()> = Mutex::new(());

/// The account's own home folder (`pwd.getpwuid(os.getuid()).pw_dir`); "" when there is none.
pub(crate) fn account_home() -> String {
    // SAFETY: getpwuid_r writes into the buffers given, sized as sysconf says; the result is read only on success.
    unsafe {
        let mut size = libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX);
        if size <= 0 {
            size = 16384;
        }
        let mut buf = vec![0 as libc::c_char; size as usize];
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc = libc::getpwuid_r(libc::getuid(), &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result);
        if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
            return String::new();
        }
        std::ffi::CStr::from_ptr(pwd.pw_dir).to_string_lossy().into_owned()
    }
}

/// (folder, its name in a log line) for every protected folder: each of PROTECTED_HOME under $HOME, as given and
/// resolved, and again under the data volume's name; then PROTECTED_ROOTS.
pub fn protected_roots(environ: Option<&HashMap<String, String>>) -> Vec<(String, String)> {
    let mut home = match environ {
        Some(env) => env.get("HOME").cloned().unwrap_or_default(),
        None => std::env::var_os("HOME").map(|h| h.to_string_lossy().into_owned()).unwrap_or_default(),
    };
    if !pypath::isabs(&home) {
        home = account_home(); // HOME unset or relative: the account's own
    }
    let mut homes: Vec<String> = Vec::new();
    if pypath::isabs(&home) {
        for h in [pypath::normpath(&home), pypath::realpath(&home)] {
            if !homes.contains(&h) {
                homes.push(h);
            }
        }
    }
    let mut roots: Vec<(String, String)> = Vec::new();
    let mut push = |root: (String, String)| {
        if !roots.contains(&root) {
            roots.push(root);
        }
    };
    for base in &homes {
        for sub in PROTECTED_HOME {
            let folder = pypath::join(base, sub);
            push((folder.clone(), format!("~/{sub}")));
            push((format!("{DATA_VOLUME}{folder}"), format!("~/{sub}")));
        }
    }
    for root in PROTECTED_ROOTS {
        push((root.to_string(), root.to_string()));
    }
    roots
}

/// The folders this process runs its code from: the executable's own folder, and the Python package's source
/// folder (bridge/src) of the checkout it was built from, when it is there (its assets are read from it). Never
/// refused, even inside a protected folder: macOS has already been asked about a folder a program started from, and
/// a developer's checkout under ~/Documents must still read what it runs (eve lite's own install lives in
/// ~/.eve-lite, outside every protected folder).
pub fn code_roots() -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        found.push(pypath::dirname(&exe.to_string_lossy()));
    }
    let package_parent = pypath::normpath(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src"));
    if Path::new(&package_parent).is_dir() {
        found.push(package_parent);
    }
    let mut out: Vec<String> = Vec::new();
    for root in found.iter().filter(|r| !r.is_empty() && pypath::isabs(r)) {
        for p in [pypath::normpath(root), pypath::realpath(root)] {
            if p != "/" && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// Turn the process firewall on, for good: nothing removes it. Only in eve lite (EVE_PROFILE=lite): anywhere else
/// this logs one line and installs nothing. "audit" lets everything through and only counts and logs; a later
/// "enforce" tightens it, and nothing loosens it. A test that runs code which calls this does it in a subprocess:
/// installed in a test runner, the guard would stay for every test after it.
pub fn install_firewall(mode: &str) -> Result<(), FirewallError> {
    let mode = Mode::parse(mode)?;
    if !crate::is_lite(None)? {
        tracing::warn!("firewall: not installed: it is eve lite's (EVE_PROFILE=lite), and this is full eve");
        return Ok(());
    }
    {
        let _held = GUARD_LOCK.lock();
        if let Some(guard) = GUARD.get() {
            if mode == Mode::Enforce && guard.mode() != Mode::Enforce {
                guard.enforce();
                tracing::info!("firewall: enforcing now (was auditing)");
            }
            return Ok(());
        }
        let guard = Guard::new(mode, protected_roots(None), std::process::id(), &code_roots());
        let _ = GUARD.set(guard);
    }
    tracing::info!(
        "firewall: on ({}): {} programs may start; {} folders are refused",
        mode.as_str(),
        EXEC_ALLOW.len(),
        PROTECTED_HOME.len() + PROTECTED_ROOTS.len()
    );
    Ok(())
}

/// What the firewall refused (or, auditing, would have), by "exec:<program>" and "path:<folder>". {} when it is not
/// on or refused nothing.
pub fn firewall_counts() -> IndexMap<String, u64> {
    GUARD.get().map(Guard::counts).unwrap_or_default()
}

/// Judge one event: Ok when no firewall is on, or it lets the event go; a PermissionError when it refuses.
pub fn audit(event: &Event) -> io::Result<()> {
    match GUARD.get() {
        Some(guard) => guard.call(event),
        None => Ok(()),
    }
}

/// Judge a file event on these paths (an open, a listing, a rename, ...).
pub fn audit_path<I, A>(event: FileEvent, args: I) -> io::Result<()>
where
    I: IntoIterator<Item = A>,
    A: Into<Arg>,
{
    if GUARD.get().is_none() {
        return Ok(());
    }
    audit(&Event::file(event, args))
}

/// The `subprocess.Popen` event a `Command` would raise: its program, its working folder, and the PATH it looks the
/// program up on (its own PATH when it sets one, else this process's).
pub fn popen_event(cmd: &Command) -> Event {
    let executable = Arg::from(Path::new(cmd.get_program()));
    let cwd = cmd.get_current_dir().map(Arg::from).unwrap_or(Arg::None);
    let env = cmd.get_envs().find(|(k, _)| *k == "PATH").map(|(_, v)| {
        let mut env = HashMap::new();
        if let Some(v) = v {
            env.insert("PATH".to_string(), v.to_string_lossy().into_owned());
        }
        env
    });
    Event::Popen { executable, cwd, env }
}

/// Judge a process start before it is made. A `tokio::process::Command` is judged through its `as_std()`.
pub fn audit_command(cmd: &Command) -> io::Result<()> {
    if GUARD.get().is_none() {
        return Ok(());
    }
    audit(&popen_event(cmd))
}

/// `cmd.spawn()`, judged first.
pub fn spawn(cmd: &mut Command) -> io::Result<Child> {
    audit_command(cmd)?;
    cmd.spawn()
}

/// `cmd.output()`, judged first.
pub fn output(cmd: &mut Command) -> io::Result<Output> {
    audit_command(cmd)?;
    cmd.output()
}

/// `cmd.status()`, judged first.
pub fn status(cmd: &mut Command) -> io::Result<ExitStatus> {
    audit_command(cmd)?;
    cmd.status()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_is_apfs_comparison() {
        assert_eq!(fold("/Users/A/Documents"), "/users/a/documents");
        assert_eq!(fold("Stra\u{df}e"), "strasse");
        assert_eq!(fold("e\u{301}"), fold("\u{e9}"));
    }

    #[test]
    fn sqlite_paths_reads_uris() {
        assert_eq!(sqlite_paths(&Arg::from(":memory:")), vec![]);
        assert_eq!(sqlite_paths(&Arg::from("")), vec![]);
        assert_eq!(sqlite_paths(&Arg::Fd(3)), vec![]);
        assert_eq!(sqlite_paths(&Arg::from("/a/b.db")), vec![Arg::from("/a/b.db")]);
        assert_eq!(
            sqlite_paths(&Arg::from("file:/a/my%20b.db?mode=ro#x")),
            vec![Arg::from("file:/a/my%20b.db?mode=ro#x"), Arg::from("/a/my b.db")]
        );
        assert_eq!(
            sqlite_paths(&Arg::from("file://host/a/b.db")),
            vec![Arg::from("file://host/a/b.db"), Arg::from("/a/b.db")]
        );
    }

    #[test]
    fn event_names_are_pythons() {
        assert_eq!(Event::System.name(), "os.system");
        assert_eq!(Event::ForkExec { executables: vec![], cwd: Arg::None }.name(), FORK_EXEC);
        assert_eq!(FileEvent::SqliteConnect.name(), "sqlite3.connect");
        assert_eq!(FileEvent::Symlink.positions(), &[1]);
        assert_eq!(FileEvent::Rename.positions(), &[0, 1]);
    }

    #[test]
    fn a_bad_mode_is_a_value_error() {
        assert_eq!(
            Mode::parse("off").unwrap_err().to_string(),
            "firewall mode is one of ('enforce', 'audit'), not 'off'"
        );
    }
}
