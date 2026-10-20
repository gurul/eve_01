//! eve lite's daemon: the parts behind eve's web app, and nothing of full eve's Mac (docs/plans/eve-lite.md M1, WP-9).
//!
//! The owner, 2026-10-13: "create a variant of eve that can be one click onboarding … use composio and openrouter as
//! the only setup things", and "this is so that people dont need to have xcode and stuff setup". Under
//! EVE_PROFILE=lite, `eve-bridge daemon` runs [`run_daemon`] (the lite LaunchAgent pins the profile). Full eve's daemon
//! is never built or linked: its run starts BLE, the ears, vision, learning, Codex and the Mini App. [`LiteDaemon`]
//! builds:
//!
//! * the IPC server, on a 0600 Unix socket in ~/.eve-lite (profile.LITE_DEFAULTS), for `eve-bridge web` and `status`;
//! * memory, by the steps of full eve's `Daemon._build_memory` and `_start_dream`, copied here ([`build_memory`],
//!   [`start_dream`]) because full eve's daemon is not linked; tests/memory_parity.rs holds the copies to the
//!   originals;
//! * the chat log, and the watcher, whose alerts land in the chat (`watcher.notify = inlet.tell_owner`);
//! * a ConnectBridge (composio_connect) once Composio is signed in, that is once composio-connect.json exists;
//! * the brain: telegram's inlet, app-only (telegram.configured gives eve lite its own), lent memory, the chat log,
//!   the watcher, the apps and think_hard, and none of the Mac: no agent, no screen, a launcher that refuses, no
//!   recent folders, no Claude Code sessions, no Codex folders;
//! * exactly one LinkChat, whose uploads are the inlet's; PhoneCalls on the same brain, a call proving itself with
//!   its WebRTC peer's own secret (lite_rtc), never with Telegram's initData; the RtcHub over both;
//! * the Mac's page (lite_web.LocalServer), always; the LAN door once a phone was added (LocalServer opens it again
//!   when web.json says it was on, and the page or `eve-bridge web lan` turns it on and off);
//! * the providers' spend figures (spend_sync's loop) and the nightly dream.
//!
//! States. NEEDS_OPENROUTER while there is no model key: the setup page and the IPC, and nothing else. READY once
//! there is one: everything above. The state is decided once, at start; a sign-in moves it on by restarting.
//!
//! Restarts. A sign-in that writes a credential (the OpenRouter key into the env file, Composio's
//! composio-connect.json) makes lite_web call `restart`: the daemon stops gracefully (the page's open streams end,
//! the WebRTC peers close, the brain says goodbye, the memory closes its conversation and waits for the dream) and
//! exits 75 (EXIT_RESTART). launchd starts it again (the lite plist: KeepAlive {SuccessfulExit: false},
//! ThrottleInterval 5), now with the credential. A stop (SIGTERM, SIGINT) exits 0, which launchd leaves alone.
//!
//! Full eve on this Mac. While full eve's LaunchAgent (FULL_LABEL) is installed for this user, eve lite logs one line
//! and exits 0 before anything starts, so launchd leaves it down: one edition per macOS user (both keep their data
//! in ~/.config/eve-bridge).
//!
//! The firewall. [`run_daemon`] installs profile's process firewall before it builds anything (only logging is set
//! up first, so the firewall's own line is kept), and it stays for the life of the process: no developer tool, no
//! other program but pmset, ifconfig and scutil, and no private folder.
//!
//! IPC: one JSON line each way (M11), on the 0600 socket only. A TCP address is not taken ([`ipc_path`]): any
//! account on the Mac could reach a port, and `web_login` signs a browser in.
//!
//! ```text
//!     {"evt": "web_login"}                 -> {"ok": true, "code": <one-time code, 2 minutes>, "port": <the page's>}
//!     {"evt": "web_login_used", "code"}    -> {"ok": true, "used": <whether a browser signed in with it>}
//!     {"evt": "status"}                    -> {"ok": true, "state", "openrouter", "chat", "composio", "apps", "voice",
//!                                              "page", "port", "lan", "lan_port", "local_network", "restarting",
//!                                              "firewall": {<program or folder>: <times refused>}}
//!     {"evt": "devices"}                   -> {"ok": true, "devices": [{"id", "label", "host", "created", "last_seen"}]}
//!     {"evt": "revoke", "id": <id or all>} -> {"ok": true, "revoked": [<ids>]}
//!     {"evt": "lan", "on": <bool>}         -> {"ok": true, "lan": <whether the LAN door is open now>}
//! ```
//!
//! A refusal is {"ok": false, "error": <one plain line>}. No request, code, key or token reaches a log: lines carry
//! states, counts, ports and exception types (AGENTS rule 10). The model key is handed to telegram.hide_token at
//! start, and the HTTP and WebRTC stacks log at WARNING (QUIET_LOGGERS): their INFO lines name every URL and every
//! address and candidate.

pub mod apps;
pub mod logging;

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, LazyLock, Weak};
use std::time::Duration;

use futures::future::BoxFuture;
use futures::FutureExt;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::watch;

use eve_brain::seams::{ClaudeSessions, CodexFolders, Job, LaunchRecent, Launcher, ScreenFn, Sessions, StrFn};
use eve_brain::telegram::{InletOptions, TelegramConfig, TelegramInlet};
use eve_chat_log::ChatLog;
use eve_lite_rtc::{CallDoor, LinkDoor, RtcHub};
use eve_lite_tls::LocalTls;
use eve_lite_web::{
    ComposioSignIn, LocalServer, OpenRouterSignIn, Parts, RestartFn, StartError, StatusFn, TlsDoor, WebContext,
};
use eve_link::link_chat::door::LinkChatOptions;
use eve_link::link_chat::LinkChat;
use eve_mem0_memory::OwnerMemory;
use eve_memory::{MeaningIndex, Memory, MemoryOptions};
use eve_phone_call::phone_call::{PhoneCalls, Voice};
use eve_records::{DreamClient, RecallConfig};
use eve_transcripts::{TranscriptConfig, Transcripts};
use eve_web::watch::Watcher;
use eve_web_auth::{Device, Devices, MacLogin, PairingWindow};

pub use apps::{ConnectApps, ConnectClient, ConnectSession, LiteApps, FEED_POLL_SECS};
pub use logging::{level_of, log_filter, logger_name, setup_logging, PyFormat, QUIET_LOGGERS, QUIET_TARGETS};

/// An environment as the upstream crates read one (None: the process's).
pub type Environ = HashMap<String, String>;

// ---- exit codes (launchd reads them: the lite plist restarts on any but 0) ----------------------------------------

/// stopped, or full eve is installed for this user: launchd leaves eve lite down
pub const EXIT_OK: i32 = 0;
/// another daemon holds eve lite's IPC socket (as cli._run_daemon)
pub const EXIT_IN_USE: i32 = 2;
/// EX_TEMPFAIL: a sign-in saved a credential; launchd starts eve lite again into it
pub const EXIT_RESTART: i32 = 75;
/// EX_CONFIG: EVE_PROFILE is not lite (the CLI exits 78 on profile.ProfileError too)
pub const EXIT_CONFIG: i32 = 78;

pub const READY: &str = eve_lite_web::READY;
pub const NEEDS_OPENROUTER: &str = eve_lite_web::NEEDS_OPENROUTER;

// ---- where ---------------------------------------------------------------------------------------------------------

/// full eve's LaunchAgent (_service_launchd.LABEL)
pub const FULL_LABEL: &str = "com.github.eve-bridge.daemon";
/// `Path("~/Library/LaunchAgents") / f"{FULL_LABEL}.plist"`
pub const FULL_PLIST: &str = "~/Library/LaunchAgents/com.github.eve-bridge.daemon.plist";
/// the IPC address (profile.LITE_DEFAULTS)
pub const SOCK_ENV: &str = "EVE_BRIDGE_SOCK";
/// web_auth.Devices: token hashes, 0600
pub const DEVICES_FILE: &str = "~/.config/eve-bridge/web-devices.json";
/// lite_tls.LocalTls: the CA and the LAN certificate
pub const TLS_DIR: &str = "~/.config/eve-bridge/web-tls";
/// macOS keeps a socket's path in 104 bytes, NUL included
pub const SOCKET_MAX_BYTES: usize = 103;

// ---- how long, and the log -----------------------------------------------------------------------------------------

/// shutdown: the conversation the brain left open is closed within this (daemon.py)
pub const CLOSE_MARKERS_SECS: f64 = 3.0;
/// and background work (the dream) is given this before it is cut off (daemon.py)
pub const DRAIN_SECS: f64 = 12.0;
/// every port of the Mac's page taken: tried again after these, the last repeating
pub const PAGE_RETRY_SECS: [f64; 3] = [10.0, 30.0, 60.0];
/// shutdown: a page start already under way is let finish this long, then stopped
pub const PAGE_STOP_SECS: f64 = 5.0;
/// full eve's daemon's (cli._run_daemon)
pub const LOG_FORMAT: &str = "%(asctime)s %(levelname)s %(name)s: %(message)s";

// What `eve-bridge web` and `status` show: one plain line each.
pub const PAGE_DOWN: &str = "eve's page is not up on this Mac yet. Try again in a minute.";
pub const STOPPING: &str = "eve is stopping on this Mac right now. Try again in a few seconds.";
pub const UNKNOWN_EVENT: &str = "eve lite does not take that request.";

// ---- small Python helpers ------------------------------------------------------------------------------------------

/// `Path(raw).expanduser()`.
pub fn expanduser(raw: &str) -> PathBuf {
    eve_brain::prelude::expanduser(raw)
}

/// Seconds as a Duration for a wait: a negative or broken figure is no wait.
fn secs(s: f64) -> Duration {
    if s.is_nan() || s <= 0.0 {
        Duration::ZERO
    } else {
        Duration::try_from_secs_f64(s).unwrap_or(Duration::MAX)
    }
}

/// `type(e).__name__` for an error a part returned: the Python class it names (eve_ipc's mapping: an I/O error by
/// its errno, a `NamedError` by its kind), `Exception` for anything else.
pub fn error_name(e: &anyhow::Error) -> String {
    eve_ipc::error_type_name(e)
}

/// `type(e).__name__` of what a worker thread or a part's task raised: a panic names itself.
fn join_error_name(e: &tokio::task::JoinError) -> &'static str {
    if e.is_cancelled() {
        "CancelledError"
    } else {
        "panic"
    }
}

/// Run `f`, a panic in it standing for Python's exception (named "panic").
fn guarded<T>(f: impl FnOnce() -> T) -> Result<T, anyhow::Error> {
    std::panic::catch_unwind(AssertUnwindSafe(f))
        .map_err(|_| anyhow::Error::new(eve_ipc::NamedError::new("panic", "a part panicked")))
}

// ---- what the brain is lent instead of the Mac ---------------------------------------------------------------------

/// The inlet's screen in eve lite: none (eve lite never asks for Screen Recording).
pub fn no_screen() -> Option<PathBuf> {
    None
}

/// No recent coding folders, no Claude Code sessions, no Codex folders: eve lite has none of them.
pub fn nothing<T>() -> Vec<T> {
    Vec::new()
}

/// LinkChat's Work sessions in eve lite: none (lite_web answers 404 for their routes before LinkChat would).
pub fn no_work() -> Option<Arc<dyn eve_link::seams::WorkDoor>> {
    None
}

/// PhoneCalls' Telegram check in eve lite: no initData belongs to anyone. A call proves itself with its WebRTC
/// peer's own secret instead (lite_rtc.RtcCallSocket's hello, PhoneCalls.serve_socket's token_check).
pub fn no_owner(_init_data: &str) -> Option<i64> {
    None
}

/// The inlet's launcher in eve lite: a coding session is not part of eve lite (no Claude Code, no Codex), so
/// nothing opens, and the line says what eve lite does instead. Its code words and tool are refused before this
/// anyway.
pub async fn refuse_launch(_folder: PathBuf, _harness: String) -> String {
    eve_brain::telegram::LITE_NOT_HERE.to_string()
}

// ---- memory: full eve's steps, copied ------------------------------------------------------------------------------

/// mem0's `OwnerMemory` as the memory facade searches, finds and forgets through it (Python duck-typed the
/// index; the Rust facade takes [`MeaningIndex`]). Full eve's daemon wraps it the same way.
pub struct Mem0Index(pub Arc<OwnerMemory>);

impl MeaningIndex for Mem0Index {
    fn search(&self, query: &str) -> anyhow::Result<Vec<Value>> {
        Ok(self.0.search(query, eve_mem0_memory::MAX_HITS))
    }

    fn find(&self, pred: &dyn Fn(&str) -> bool) -> anyhow::Result<Vec<Value>> {
        Ok(self.0.find(pred).into_iter().map(|f| json!({"id": f.id, "text": f.text})).collect())
    }

    fn forget(&self, ids: &[String]) -> anyhow::Result<usize> {
        self.0.forget(ids).map_err(anyhow::Error::new)
    }

    fn forget_matching(&self, pred: &dyn Fn(&str) -> bool) -> Option<anyhow::Result<usize>> {
        Some(self.0.forget_matching(pred).map_err(anyhow::Error::new))
    }
}

/// A built memory (weakly) and its mem0 index: the dream is handed the same object (`memory.index`) the facade
/// searches, which the facade keeps as a [`MeaningIndex`] and the dream takes as a [`eve_dream::DreamIndex`].
type Mem0Entry = (Weak<Memory>, Arc<OwnerMemory>);

static MEM0_OF: LazyLock<Mutex<Vec<Mem0Entry>>> = LazyLock::new(|| Mutex::new(Vec::new()));

fn remember_mem0(memory: &Arc<Memory>, index: Arc<OwnerMemory>) {
    let mut all = MEM0_OF.lock();
    all.retain(|(m, _)| m.strong_count() > 0);
    all.push((Arc::downgrade(memory), index));
}

/// The mem0 index `memory` was built with, as the dream takes it; None when it was built without one.
pub fn mem0_of(memory: &Arc<Memory>) -> Option<Arc<OwnerMemory>> {
    memory.index.as_ref()?;
    let all = MEM0_OF.lock();
    all.iter().find(|(m, _)| m.as_ptr() == Arc::as_ptr(memory)).map(|(_, idx)| idx.clone())
}

/// `memory.migrate(cfg, legacy_store, legacy_mem0)`.
pub type MigrateFn = Arc<dyn Fn(&RecallConfig, &Path, &Path) -> io::Result<Value> + Send + Sync>;
/// `records.ensure_repo(repo)`.
pub type EnsureRepoFn = Arc<dyn Fn(&Path) -> bool + Send + Sync>;
/// `transcripts.Transcripts(config, archive=archive)`.
pub type TranscriptsFn = Arc<dyn Fn(TranscriptConfig, Option<PathBuf>) -> Transcripts + Send + Sync>;
/// `mem0_memory.shared(cfg)`.
pub type Mem0Fn = Arc<dyn Fn(&RecallConfig, Option<&Environ>) -> Option<Arc<OwnerMemory>> + Send + Sync>;

/// What `build_memory` opens, in the order it opens them (the module functions Python's tests patch).
/// [`MemoryOpeners::default`] is the real ones.
#[derive(Clone)]
pub struct MemoryOpeners {
    pub migrate: MigrateFn,
    pub ensure_repo: EnsureRepoFn,
    pub transcripts: TranscriptsFn,
    pub mem0: Mem0Fn,
}

impl Default for MemoryOpeners {
    fn default() -> Self {
        MemoryOpeners {
            migrate: Arc::new(eve_memory::migrate),
            ensure_repo: Arc::new(eve_records::records::ensure_repo),
            transcripts: Arc::new(|config, archive| Transcripts::new(config, archive, None)),
            mem0: Arc::new(eve_mem0_memory::shared),
        }
    }
}

/// The memory stack, or None while memory is off: the steps of full eve's `Daemon._build_memory`, copied, since eve
/// lite never links full eve's daemon. tests/memory_parity.rs holds the copy to the original, statement for
/// statement and file for file. Synchronous (renames and a few git calls): run it on a worker thread. In eve lite
/// records starts no git (toolchain.git_usable is false there), so ensure_repo says in one line that the records keep
/// no history.
pub fn build_memory(cfg: &RecallConfig) -> Option<Arc<Memory>> {
    build_memory_with(cfg, None, &MemoryOpeners::default())
}

/// [`build_memory`] over an explicit environment and openers.
pub fn build_memory_with(cfg: &RecallConfig, environ: Option<&Environ>, openers: &MemoryOpeners) -> Option<Arc<Memory>> {
    if !eve_memory::enabled(environ) {
        tracing::info!("memory: off (EVE_MEMORY) — nothing said is kept, and prompts carry no memory");
        return None;
    }
    let legacy_store = eve_records::legacy_store(environ);
    let legacy_mem0 = eve_records::legacy_mem0();
    if let Err(e) = (openers.migrate)(cfg, &legacy_store, &legacy_mem0) {
        // a failed move leaves the old store where it was
        tracing::warn!(
            "memory: the move to the memory folder failed ({}) — the old store is untouched",
            eve_transcripts::py::os_error_name(&e)
        );
    }
    (openers.ensure_repo)(&cfg.records_dir());
    let transcripts = Arc::new((openers.transcripts)(eve_transcripts::configured(environ), Some(cfg.archive_dir())));
    let index = (openers.mem0)(cfg, environ);
    tracing::info!("memory: on — transcripts, records{}", if index.is_some() { ", mem0 index" } else { " (no mem0 index)" });
    if let Some(idx) = &index {
        if eve_mem0_memory::embedder_of(environ) == "gemma" {
            // the local embedder loads in ~3 s; loaded now, the first search is not the one that waits
            let idx = idx.clone();
            let _ = std::thread::Builder::new().name("mem0-warm".into()).spawn(move || idx.warm());
        }
    }
    let facade_index: Option<Arc<dyn MeaningIndex>> =
        index.clone().map(|i| Arc::new(Mem0Index(i)) as Arc<dyn MeaningIndex>);
    // only the daemon runs the index worker
    let opts = MemoryOptions { background: true, environ: environ.cloned(), ..MemoryOptions::default() };
    let memory = Arc::new(Memory::with_options(cfg.clone(), transcripts, facade_index, opts));
    if let Some(idx) = index {
        remember_mem0(&memory, idx);
    }
    Some(memory)
}

/// The nightly dream's loop over `memory` with `client`, until `shutdown` is set; None with no memory or no
/// client. `index` is the mem0 index the dream feeds (the memory's own, [`mem0_of`]). Full eve's daemon's, copied.
pub fn dream_loop(
    memory: Option<&Arc<Memory>>,
    cfg: &RecallConfig,
    client: Option<Arc<dyn DreamClient + Send + Sync>>,
    index: Option<Arc<dyn eve_dream::DreamIndex>>,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> Option<impl Future<Output = ()> + Send + 'static> {
    let memory = memory?;
    let client = client?;
    let dreamer = eve_dream::Dreamer::new(cfg.clone(), memory.transcripts.clone(), client, index, None);
    Some(async move { dreamer.run_loop(shutdown, eve_dream::LOOP_INTERVAL_SECS).await })
}

/// Work that must outlive the call that started it (the dream): drained at shutdown, not cancelled. A job that
/// ended leaves it (`task.add_done_callback(background.discard)`).
#[derive(Clone, Default)]
pub struct Background(Arc<Mutex<Vec<Job>>>);

impl Background {
    pub fn add(&self, job: Job) {
        let mut jobs = self.0.lock();
        jobs.retain(|j| !j.done());
        jobs.push(job);
    }

    /// The jobs still running.
    pub fn jobs(&self) -> Vec<Job> {
        let mut jobs = self.0.lock();
        jobs.retain(|j| !j.done());
        jobs.clone()
    }

    /// Their names (`[t.get_name() for t in background]`).
    pub fn names(&self) -> Vec<String> {
        self.jobs().iter().map(|j| j.name().to_string()).collect()
    }
}

/// `records.make_client()`: the dream's model client, or None (it logs why).
pub type MakeDreamClient = Arc<dyn Fn() -> Option<Arc<dyn DreamClient + Send + Sync>> + Send + Sync>;

/// The real [`MakeDreamClient`]: records.make_client with the process's environment.
pub fn records_client() -> MakeDreamClient {
    Arc::new(|| {
        eve_records::make_client(None, eve_records::records::DEFAULT_MODEL)
            .map(|c| Arc::new(c) as Arc<dyn DreamClient + Send + Sync>)
    })
}

/// The nightly dream, held in `background` so shutdown drains it instead of cancelling it: full eve's
/// `Daemon._start_dream`, copied. No memory or no model client (records.make_client logs why): no dream.
pub fn start_dream(
    memory: Option<&Arc<Memory>>,
    cfg: &RecallConfig,
    shutdown: watch::Receiver<bool>,
    background: &Background,
) -> Option<Job> {
    start_dream_with(memory, cfg, shutdown, background, &records_client())
}

/// [`start_dream`] with the model client's maker given.
pub fn start_dream_with(
    memory: Option<&Arc<Memory>>,
    cfg: &RecallConfig,
    shutdown: watch::Receiver<bool>,
    background: &Background,
    make_client: &MakeDreamClient,
) -> Option<Job> {
    let memory = memory?;
    let client = make_client()?;
    let index = mem0_of(memory).map(|i| i as Arc<dyn eve_dream::DreamIndex>);
    let fut = dream_loop(Some(memory), cfg, Some(client), index, shutdown)?;
    let task = Job::spawn("memory-dream", fut);
    background.add(task.clone());
    Some(task)
}

// ---- the parts -----------------------------------------------------------------------------------------------------

/// The person's apps through Composio Connect once they signed in (`token_path`, composio-connect.json, exists:
/// onboarding writes it only once a token arrived), else None. Not started here: `start` lists the tools on a task
/// of its own, trying again until Composio answers.
pub fn make_bridge(token_path: &Path) -> Option<Arc<dyn LiteApps>> {
    make_bridge_with(token_path, &real_bridge())
}

/// `composio_connect.ConnectBridge(composio_connect.ConnectTokenSource(token_path))` (the tests lend a stand-in).
pub type MakeBridge = Arc<dyn Fn(&Path) -> Arc<dyn LiteApps> + Send + Sync>;

/// The real [`MakeBridge`]: a ConnectBridge, and the brain's view of it ([`ConnectApps`]).
pub fn real_bridge() -> MakeBridge {
    Arc::new(|token_path: &Path| Arc::new(ConnectApps::new(token_path)) as Arc<dyn LiteApps>)
}

/// [`make_bridge`] with the bridge's maker given.
pub fn make_bridge_with(token_path: &Path, make: &MakeBridge) -> Option<Arc<dyn LiteApps>> {
    // Path.is_file: false for a missing file and for one that cannot be looked at
    if !token_path.is_file() {
        tracing::info!("apps: Composio is not signed in on this Mac; the web app's Settings, Apps signs in");
        return None;
    }
    Some(make(token_path))
}

/// Whether full eve's LaunchAgent is installed for this user (its plist is in ~/Library/LaunchAgents): eve lite then
/// stays down (crate docs).
pub fn full_eve_installed() -> bool {
    expanduser(FULL_PLIST).exists()
}

/// The IPC socket's path: `spec` (the CLI's --socket), else EVE_BRIDGE_SOCK, else eve lite's default in ~/.eve-lite.
/// Only a socket file is taken: a TCP address (`host:port`) is replaced by the default, with one line, because any
/// account on the Mac could connect to a port and `web_login` signs a browser in.
pub fn ipc_path(spec: Option<&str>) -> PathBuf {
    let from_env = std::env::var(SOCK_ENV).ok();
    let default = eve_profile::LITE_DEFAULTS.get(SOCK_ENV).map(String::as_str).unwrap_or("~/.eve-lite/ipc.sock");
    ipc_path_from(spec, from_env.as_deref(), &expanduser(default))
}

/// [`ipc_path`] with EVE_BRIDGE_SOCK's value and the default given.
pub fn ipc_path_from(spec: Option<&str>, from_env: Option<&str>, default: &Path) -> PathBuf {
    // str(spec or os.environ.get(SOCK_ENV) or "").strip()
    let raw = spec.filter(|s| !s.is_empty()).or(from_env.filter(|s| !s.is_empty())).unwrap_or("");
    let chosen = eve_transcripts::py::strip(raw);
    if chosen.is_empty() {
        return default.to_path_buf();
    }
    let socket_file = matches!(eve_ipc::parse_spec(chosen), Ok(t) if t.as_unix().is_some());
    if !socket_file {
        tracing::warn!("lite: the IPC address must be a socket file in eve lite, not a TCP port; using eve lite's own");
        return default.to_path_buf();
    }
    expanduser(chosen)
}

/// The socket's folder, made 0700 when it is missing (~/.eve-lite: the installer makes it so). A folder that is
/// there is left as it is: it may be one this daemon does not own.
fn private_folder(folder: &Path) -> io::Result<()> {
    if folder.is_dir() {
        return Ok(());
    }
    // mkdir(mode=0o700, parents=True): the parents as the umask makes them, the folder itself 0700
    if let Some(parent) = folder.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(folder) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && folder.is_dir() => Ok(()),
        other => other,
    }
}

/// The byte length of a path as the OS takes it (`len(os.fsencode(str(path)))`).
fn path_bytes(path: &Path) -> usize {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().len()
    }
    #[cfg(not(unix))]
    {
        path.to_string_lossy().len()
    }
}

/// The backstop that closes the brain's open conversation `conv` in `store`: a conversation with lines and no close
/// gets one; one with nothing in it gets nothing. Whether a marker was written.
pub fn close_conversation(store: &Transcripts, conv: &str) -> bool {
    let lines = store.conv_lines(conv);
    if lines.is_empty() || lines.iter().any(|ln| ln.get("kind").and_then(Value::as_str) == Some("close")) {
        return false;
    }
    store.close(eve_brain::telegram::CHANNEL, conv, None)
}

// ---- the seams the tests replace -----------------------------------------------------------------------------------

/// `spend_sync.loop(shutdown)`: the providers' spend figures, until the shutdown flag is set.
pub type SpendLoop = Arc<dyn Fn(watch::Receiver<bool>) -> BoxFuture<'static, ()> + Send + Sync>;
/// `telegram.make_inlet(config, **lent)`: the brain, None when it cannot run (it logs why); an error is the call
/// raising.
pub type MakeInlet =
    Arc<dyn Fn(TelegramConfig, InletOptions) -> anyhow::Result<Option<Arc<TelegramInlet>>> + Send + Sync>;
/// `build_memory(cfg)`: an error is the build raising.
pub type BuildMemory = Arc<dyn Fn(&RecallConfig) -> anyhow::Result<Option<Arc<Memory>>> + Send + Sync>;
/// `phone_call.make_voice()`: the call's ears and voice, None without a key (it logs why); an error is the call
/// raising.
pub type MakeVoice = Arc<dyn Fn() -> anyhow::Result<Option<Arc<dyn Voice>>> + Send + Sync>;
/// `devices.list()`: an error is the read raising.
pub type ListDevices = Arc<dyn Fn(&Devices) -> anyhow::Result<Vec<Device>> + Send + Sync>;
/// WebContext's own keywords for a test (ports 0, fakes for macOS and the network): applied to the parts last.
pub type WebOptions = Arc<dyn Fn(&mut Parts) + Send + Sync>;
/// `on_ready(daemon)`, called once the daemon serves; an error is the call raising.
pub type OnReady = Arc<dyn Fn(&Arc<LiteDaemon>) -> anyhow::Result<()> + Send + Sync>;

/// The module functions Python's tests patch (`monkeypatch.setattr(spend_sync, "loop", ...)` and the like), and
/// `PAGE_RETRY_SECS`. [`Seams::default`] is the real ones.
#[derive(Clone)]
pub struct Seams {
    pub spend_loop: SpendLoop,
    pub make_bridge: MakeBridge,
    pub make_inlet: MakeInlet,
    pub build_memory: BuildMemory,
    pub make_voice: MakeVoice,
    pub make_dream_client: MakeDreamClient,
    pub list_devices: ListDevices,
    pub page_retry_secs: Vec<f64>,
}

impl Default for Seams {
    fn default() -> Self {
        Seams {
            spend_loop: Arc::new(|shutdown: watch::Receiver<bool>| {
                eve_spend_sync::run_loop(shutdown, Duration::from_secs_f64(eve_spend_sync::SYNC_SECS), None).boxed()
            }),
            make_bridge: real_bridge(),
            make_inlet: Arc::new(|config: TelegramConfig, lent: InletOptions| {
                guarded(move || eve_brain::telegram::make_inlet(config, lent))
            }),
            build_memory: Arc::new(|cfg: &RecallConfig| guarded(|| build_memory(cfg))),
            make_voice: Arc::new(|| {
                guarded(|| eve_phone_call::phone_call::make_voice(None).map(|v| Arc::new(v) as Arc<dyn Voice>))
            }),
            make_dream_client: records_client(),
            list_devices: Arc::new(|devices: &Devices| Ok(devices.list())),
            page_retry_secs: PAGE_RETRY_SECS.to_vec(),
        }
    }
}

/// `LiteDaemon(socket_path=..., web_options=..., openrouter_login=..., composio_login=..., on_ready=...)`, plus the
/// seams.
#[derive(Clone, Default)]
pub struct LiteOptions {
    /// the IPC socket's path (None: [`ipc_path`]'s choice)
    pub socket_path: Option<String>,
    pub web_options: Option<WebOptions>,
    pub openrouter_login: Option<Arc<dyn OpenRouterSignIn>>,
    pub composio_login: Option<Arc<dyn ComposioSignIn>>,
    pub on_ready: Option<OnReady>,
    pub seams: Seams,
}

// ---- the daemon ----------------------------------------------------------------------------------------------------

/// The composition (READY only; None until run builds it, and for good in NEEDS_OPENROUTER), and what serves.
#[derive(Default)]
struct Composed {
    memory: Option<Arc<Memory>>,
    chat_log: Option<Arc<ChatLog>>,
    watcher: Option<Arc<Watcher>>,
    apps: Option<Arc<dyn LiteApps>>,
    inlet: Option<Arc<TelegramInlet>>,
    link: Option<Arc<LinkChat>>,
    calls: Option<Arc<PhoneCalls>>,
    rtc: Option<Arc<RtcHub>>,
    web: Option<Arc<WebContext>>,
    local: Option<LocalServer>,
    ipc: Option<Arc<eve_ipc::IpcServer>>,
}

/// eve lite's daemon (crate docs): `run` builds it, serves until `shutdown` or `restart`, stops it in order and
/// returns the exit code. Every part is read from this process's environment and home, as full eve's daemon reads
/// them. The options are for tests and their stand-ins: the IPC socket's path, WebContext's own keywords (ports 0,
/// fakes for macOS and the network), the two sign-ins, `on_ready` (called with the daemon once it serves) and the
/// seams.
pub struct LiteDaemon {
    me: Weak<LiteDaemon>,
    pub socket_path: PathBuf,
    web_options: Option<WebOptions>,
    on_ready: Option<OnReady>,
    seams: Seams,
    state: Mutex<&'static str>,
    exit_code: AtomicI32,
    pub recall_cfg: RecallConfig,
    pub connect_path: PathBuf,
    // who may use the web app, and how a browser proves it (web_auth, lite_tls)
    pub devices: Arc<Devices>,
    pub pairing: Arc<PairingWindow>,
    pub mac_login: Arc<MacLogin>,
    pub tls: Arc<LocalTls>,
    // the two sign-ins (onboarding): the setup page's, in either state
    pub openrouter_login: Arc<dyn OpenRouterSignIn>,
    pub composio_login: Arc<dyn ComposioSignIn>,
    parts: Mutex<Composed>,
    shutdown_tx: watch::Sender<bool>,
    tasks: Mutex<Vec<Job>>,
    /// _keep_page_up, while every page port is taken
    page_task: Mutex<Option<Job>>,
    ipc_task: Mutex<Option<Job>>,
    /// work that must outlive the call that started it (the dream): drained at shutdown, not cancelled
    background: Background,
}

impl std::fmt::Debug for LiteDaemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LiteDaemon({})", self.state())
    }
}

impl LiteDaemon {
    pub fn new(options: LiteOptions) -> Arc<Self> {
        let socket_path = ipc_path(options.socket_path.as_deref());
        let connect_path = expanduser(eve_brain::onboarding::CONNECT_TOKEN_FILE);
        let openrouter_login = options
            .openrouter_login
            .unwrap_or_else(|| Arc::new(eve_brain::onboarding::OpenRouterLogin::new(None, None)));
        let composio_login = options.composio_login.unwrap_or_else(|| {
            Arc::new(eve_brain::onboarding::ComposioConnectLogin::new(Some(&connect_path), "", None, None))
        });
        Arc::new_cyclic(|me| LiteDaemon {
            me: me.clone(),
            socket_path,
            web_options: options.web_options,
            on_ready: options.on_ready,
            seams: options.seams,
            state: Mutex::new(NEEDS_OPENROUTER),
            exit_code: AtomicI32::new(EXIT_OK),
            recall_cfg: eve_records::configured(None),
            connect_path: connect_path.clone(),
            devices: Arc::new(Devices::new(expanduser(DEVICES_FILE))),
            pairing: Arc::new(PairingWindow::new()),
            mac_login: Arc::new(MacLogin::new()),
            tls: Arc::new(LocalTls::new(expanduser(TLS_DIR))),
            openrouter_login,
            composio_login,
            parts: Mutex::new(Composed::default()),
            shutdown_tx: watch::channel(false).0,
            tasks: Mutex::new(Vec::new()),
            page_task: Mutex::new(None),
            ipc_task: Mutex::new(None),
            background: Background::default(),
        })
    }

    // -- what the tests and the CLI read --

    pub fn state(&self) -> &'static str {
        *self.state.lock()
    }

    pub fn exit_code(&self) -> i32 {
        self.exit_code.load(Ordering::SeqCst)
    }

    pub fn stopping(&self) -> bool {
        *self.shutdown_tx.borrow()
    }

    pub fn memory(&self) -> Option<Arc<Memory>> {
        self.parts.lock().memory.clone()
    }

    pub fn chat_log(&self) -> Option<Arc<ChatLog>> {
        self.parts.lock().chat_log.clone()
    }

    pub fn watcher(&self) -> Option<Arc<Watcher>> {
        self.parts.lock().watcher.clone()
    }

    pub fn apps(&self) -> Option<Arc<dyn LiteApps>> {
        self.parts.lock().apps.clone()
    }

    pub fn inlet(&self) -> Option<Arc<TelegramInlet>> {
        self.parts.lock().inlet.clone()
    }

    pub fn link(&self) -> Option<Arc<LinkChat>> {
        self.parts.lock().link.clone()
    }

    pub fn calls(&self) -> Option<Arc<PhoneCalls>> {
        self.parts.lock().calls.clone()
    }

    pub fn rtc(&self) -> Option<Arc<RtcHub>> {
        self.parts.lock().rtc.clone()
    }

    pub fn web(&self) -> Option<Arc<WebContext>> {
        self.parts.lock().web.clone()
    }

    pub fn local(&self) -> Option<LocalServer> {
        self.parts.lock().local.clone()
    }

    /// The names of the parts' tasks still running (`[t.get_name() for t in d._tasks if not t.done()]`), sorted.
    pub fn running_tasks(&self) -> Vec<String> {
        let mut names: Vec<String> =
            self.tasks.lock().iter().filter(|t| !t.done()).map(|t| t.name().to_string()).collect();
        names.sort();
        names
    }

    /// Every part's task is over.
    pub fn tasks_done(&self) -> bool {
        self.tasks.lock().iter().all(Job::done)
    }

    pub fn background(&self) -> &Background {
        &self.background
    }

    /// A receiver of the shutdown flag (Python's `_shutdown` event).
    pub fn shutdown_rx(&self) -> watch::Receiver<bool> {
        self.shutdown_tx.subscribe()
    }

    // -- the lifecycle --

    /// Build (`build`), serve (`serve`) until `shutdown` or `restart`, then stop in order (`stop`), whatever
    /// happened before. The exit code; an error is a part raising while it was built or served (the stop has run).
    pub async fn run(self: &Arc<Self>) -> anyhow::Result<i32> {
        let outcome = async {
            self.build().await?;
            if !self.stopping() {
                // a stop asked for while it was building: nothing more starts
                self.serve().await?;
                self.ready();
            }
            let mut rx = self.shutdown_tx.subscribe();
            let _ = rx.wait_for(|set| *set).await;
            Ok::<(), anyhow::Error>(())
        }
        .await;
        self.stop().await;
        outcome.map(|()| self.exit_code())
    }

    /// Stop (SIGTERM, SIGINT): exit 0, unless a restart was asked for first. From any thread.
    pub fn shutdown(&self) {
        self.wake();
    }

    /// WebContext.restart: a sign-in saved its credential. Stop gracefully and exit 75, so launchd starts eve lite
    /// again into it (crate docs). Asked twice, it is one restart. From any thread.
    pub fn restart(&self) {
        if self.exit_code.swap(EXIT_RESTART, Ordering::SeqCst) != EXIT_RESTART {
            tracing::info!("lite: a sign-in was saved; eve lite restarts into it (exit {})", EXIT_RESTART);
        }
        self.wake();
    }

    /// Set the shutdown flag: a watch channel wakes its waiters from any thread.
    fn wake(&self) {
        self.shutdown_tx.send_replace(true);
    }

    // -- building --

    /// READY with a model key: everything in the crate docs. NEEDS_OPENROUTER without one: nothing here, the setup
    /// page alone (`serve`).
    async fn build(self: &Arc<Self>) -> anyhow::Result<()> {
        let key = eve_openrouter::key(None);
        if key.is_empty() {
            *self.state.lock() = NEEDS_OPENROUTER;
            tracing::info!("lite: no OpenRouter key yet, so the setup page only, until the sign-in");
            return Ok(());
        }
        eve_brain::telegram::bot_api::hide_token(&key); // the key never reaches a log line, whoever writes it
        let (build, cfg) = (self.seams.build_memory.clone(), self.recall_cfg.clone());
        let memory = match tokio::task::spawn_blocking(move || build(&cfg)).await {
            Ok(Ok(memory)) => memory,
            // memory is a nicety: eve lite runs without it, as full eve does
            Ok(Err(e)) => {
                tracing::warn!("memory: could not open ({}) — running without memory", error_name(&e));
                None
            }
            Err(e) => {
                tracing::warn!("memory: could not open ({}) — running without memory", join_error_name(&e));
                None
            }
        };
        self.parts.lock().memory = memory.clone();
        let me = self.me.clone();
        let watcher = match guarded(|| eve_web::watch::make_watcher(None)) {
            Ok(watcher) => watcher.map(|mut w| {
                // `watcher.notify = inlet.tell_owner`, read at call time: the brain is made after the watcher it is
                // lent, and the watcher runs only beside it
                w.set_notify(Some(Arc::new(move |text: String, about: String| {
                    let me = me.clone();
                    async move {
                        let inlet = me.upgrade().and_then(|d| d.inlet());
                        let Some(inlet) = inlet else { return Ok(false) };
                        Ok(inlet.tell_owner(&text, &about, eve_brain::telegram::WATCH_TITLE, &[]).await)
                    }
                    .boxed()
                })));
                Arc::new(w)
            }),
            // a watcher that cannot start costs the watching, never the daemon
            Err(e) => {
                tracing::warn!("watch: could not start ({}); watching is off this run", error_name(&e));
                None
            }
        };
        self.parts.lock().watcher = watcher.clone();
        let chat_log = eve_chat_log::open_configured(None)?.map(Arc::new);
        if let (Some(log), Some(memory)) = (&chat_log, &memory) {
            // forget reaches the chat log through the memory
            memory.set_chat_log(Some(log.clone() as Arc<dyn eve_memory::ChatLogLayer>));
        }
        self.parts.lock().chat_log = chat_log.clone();
        let apps = make_bridge_with(&self.connect_path, &self.seams.make_bridge);
        self.parts.lock().apps = apps.clone();
        // A brain or a voice that cannot be built costs the chat or the calls, never the setup page: a daemon that
        // failed here would be started again by launchd every 5 s, and nobody could reach its page.
        let lent = self.lent(memory.clone(), chat_log, watcher.clone(), apps.as_ref());
        let inlet = match (self.seams.make_inlet)(eve_brain::telegram::configured(None), lent) {
            Ok(inlet) => inlet,
            Err(e) => {
                // the type only
                tracing::warn!("lite: the chat brain could not be built ({})", error_name(&e));
                None
            }
        };
        self.parts.lock().inlet = inlet.clone();
        // the one chat door: the Mac's page and every phone reach this LinkChat, over HTTP or the WebRTC link channel
        let for_link = inlet.clone();
        let link = Arc::new(LinkChat::new(
            Arc::new(move || for_link.clone().map(|i| i as Arc<dyn eve_link::seams::Inlet>)),
            LinkChatOptions { work: Some(Arc::new(no_work)), ..LinkChatOptions::default() },
        ));
        self.parts.lock().link = Some(link.clone());
        match &inlet {
            Some(inlet) => {
                // eve's own pictures and files land in the door's store
                *inlet.link_uploads.lock() = Some(link.uploads().clone());
            }
            None => tracing::warn!("lite: the chat brain is off this run, so the chat answers that it is not running"),
        }
        let voice = match (self.seams.make_voice)() {
            Ok(voice) => voice,
            Err(e) => {
                tracing::warn!("call: the voice could not be built ({}); calls are off", error_name(&e));
                None
            }
        };
        let calls = Arc::new(PhoneCalls::new(
            Arc::new(no_owner),
            inlet.clone().map(|i| i as Arc<dyn eve_phone_call::phone_call::Brain>),
            voice,
        ));
        let rtc = Arc::new(RtcHub::new(
            Some(link.clone() as Arc<dyn LinkDoor>),
            Some(calls.clone() as Arc<dyn CallDoor>),
        ));
        {
            let mut parts = self.parts.lock();
            parts.calls = Some(calls);
            parts.rtc = Some(rtc);
        }
        *self.state.lock() = READY;
        Ok(())
    }

    /// What the brain is lent (`telegram.make_inlet(..., **lent)`): memory, the chat log, the watcher, the apps,
    /// think_hard and the opening brief, and none of the Mac.
    fn lent(
        &self,
        memory: Option<Arc<Memory>>,
        chat_log: Option<Arc<ChatLog>>,
        watcher: Option<Arc<Watcher>>,
        apps: Option<&Arc<dyn LiteApps>>,
    ) -> InletOptions {
        let cfg = self.recall_cfg.clone();
        let transcripts = memory.as_ref().map(|m| m.transcripts.clone());
        let brief: StrFn = Arc::new(move || {
            eve_records::opening_brief(&cfg, transcripts.as_deref().map(|t| t as &dyn eve_records::BriefSource), None)
        });
        let screen: ScreenFn = Arc::new(no_screen);
        let launcher: Launcher =
            Arc::new(|folder: PathBuf, harness: String| async move { Ok(refuse_launch(folder, harness).await) }.boxed());
        let launch_recent: LaunchRecent = Arc::new(nothing);
        let claude_sessions: ClaudeSessions = Arc::new(|| Ok(Sessions::Now(nothing())));
        let codex_folders: CodexFolders = Arc::new(nothing);
        InletOptions {
            memory: memory.map(|m| m as Arc<dyn eve_brain::seams::BrainMemory>),
            chat_log,
            watcher,
            apps: apps.and_then(|a| a.brain()),
            thinker: eve_brain::think::make_thinker(&eve_brain::think::configured(None, ""), None),
            brief: Some(brief),
            agent_factory: None,
            agent_enabled: false,
            screen: Some(screen),
            launcher: Some(launcher),
            launch_recent: Some(launch_recent),
            claude_sessions: Some(claude_sessions),
            codex_folders: Some(codex_folders),
            ..InletOptions::default()
        }
    }

    // -- serving --

    /// The Mac's page (and the LAN door when web.json says it was on), then the IPC socket, then the work that runs
    /// beside them: the brain, the watcher, the spend figures, the dream and Composio's start.
    async fn serve(self: &Arc<Self>) -> anyhow::Result<()> {
        let (link, rtc, apps) = {
            let parts = self.parts.lock();
            (parts.link.clone(), parts.rtc.clone(), parts.apps.clone())
        };
        let mut parts = Parts::new(
            self.devices.clone(),
            self.pairing.clone(),
            self.mac_login.clone(),
            self.tls.clone() as Arc<dyn TlsDoor>,
            eve_lite_web::WEB_FILE,
        );
        parts.link = link.map(|l| l as Arc<dyn eve_lite_web::LinkServe>);
        parts.rtc = rtc.map(|r| r as Arc<dyn eve_lite_web::RtcDoor>);
        parts.openrouter = Some(self.openrouter_login.clone());
        parts.composio_login = Some(self.composio_login.clone());
        parts.apps = apps.as_ref().and_then(|a| a.door());
        parts.status = Some(self.status_fn());
        parts.restart = Some(self.restart_fn());
        if let Some(options) = &self.web_options {
            options(&mut parts);
        }
        let web = WebContext::new(parts);
        let local = LocalServer::new(web.clone());
        {
            let mut composed = self.parts.lock();
            composed.web = Some(web);
            composed.local = Some(local.clone());
        }
        match local.start().await {
            Ok(_) => {}
            // PortsTaken: every port the page may use is taken now
            Err(e @ (StartError::PortsTaken(_) | StartError::Tls(_))) => {
                let first = self.seams.page_retry_secs.first().copied().unwrap_or(0.0);
                tracing::warn!("lite: the Mac's page did not start ({}); trying again in {:.0} s", e.name(), first);
                let me = self.clone();
                let task = self.spawn("page", async move { me.keep_page_up().await });
                *self.page_task.lock() = Some(task);
            }
            Err(e) => return Err(e.into()),
        }
        self.start_ipc().await?;
        let (inlet, watcher) = {
            let parts = self.parts.lock();
            (parts.inlet.clone(), parts.watcher.clone())
        };
        if let Some(inlet) = inlet {
            self.spawn("brain", async move { inlet.run().await });
            if let Some(watcher) = watcher {
                self.spawn("watch", async move { watcher.run().await });
            }
        }
        if self.state() == READY {
            // the providers' own spend figures, at start and hourly; Settings shows them beside eve's own meter
            self.spawn("spend-sync", (self.seams.spend_loop)(self.shutdown_rx()));
            // the nightly dream: the one automatic writer of the records and the mem0 index, drained at shutdown
            let memory = self.memory();
            start_dream_with(
                memory.as_ref(),
                &self.recall_cfg,
                self.shutdown_rx(),
                &self.background,
                &self.seams.make_dream_client,
            );
        }
        if let Some(apps) = apps {
            apps.start();
        }
        Ok(())
    }

    /// `WebContext.status`: the state and whether a model key is saved; lite_web reads nothing else from it.
    fn status_fn(&self) -> StatusFn {
        let me = self.me.clone();
        Arc::new(move || {
            let me = me.clone();
            async move {
                match me.upgrade() {
                    Some(d) => Ok(d.web_status()),
                    None => Ok(json!({"state": NEEDS_OPENROUTER, "openrouter": eve_openrouter::available(None)})),
                }
            }
            .boxed()
        })
    }

    /// `WebContext.restart`: [`LiteDaemon::restart`].
    fn restart_fn(&self) -> RestartFn {
        let me = self.me.clone();
        Arc::new(move || {
            if let Some(d) = me.upgrade() {
                d.restart();
            }
            async { Ok(()) }.boxed()
        })
    }

    /// The IPC server on its 0600 socket (eve_ipc's UnixTransport chmods it; its folder is made 0700). One that
    /// cannot open is a warning, not a stop: the page still serves the browsers already signed in.
    async fn start_ipc(self: &Arc<Self>) -> anyhow::Result<()> {
        let path = self.socket_path.clone();
        let length = path_bytes(&path);
        if length > SOCKET_MAX_BYTES {
            tracing::warn!(
                "lite: the IPC socket's path is longer than macOS takes ({length} bytes); `eve-bridge web` cannot reach eve"
            );
            return Ok(());
        }
        let me = self.me.clone();
        let handler = eve_ipc::handler(move |req: Value| {
            let me = me.clone();
            async move {
                Ok(match me.upgrade() {
                    Some(d) => d.handle_ipc(req).await,
                    None => json!({"ok": false, "error": eve_lite_web::FAILED}),
                })
            }
        });
        let server = Arc::new(eve_ipc::IpcServer::new(handler, Some(&path.to_string_lossy()))?);
        let opened = match private_folder(path.parent().unwrap_or(Path::new(""))) {
            Ok(()) => server.start().await,
            Err(e) => Err(e),
        };
        if let Err(e) = opened {
            tracing::warn!(
                "lite: the IPC socket did not open ({}); `eve-bridge web` cannot reach eve",
                eve_ipc::io_error_type_name(&e)
            );
            return Ok(());
        }
        self.parts.lock().ipc = Some(server.clone());
        let task = Job::spawn("lite-ipc", async move {
            if let Err(e) = server.serve_forever().await {
                tracing::warn!("lite: lite-ipc stopped ({})", error_name(&e));
            }
        });
        *self.ipc_task.lock() = Some(task);
        Ok(())
    }

    /// Every port the Mac's page may use was taken at start: try again after PAGE_RETRY_SECS, until one is free or
    /// eve stops.
    async fn keep_page_up(self: &Arc<Self>) {
        let Some(local) = self.local() else { return };
        let retry = self.seams.page_retry_secs.clone();
        let last = retry.last().copied().unwrap_or(0.0);
        let mut attempt = 0usize;
        loop {
            let wait = retry.get(attempt).copied().unwrap_or(last);
            let mut rx = self.shutdown_rx();
            if tokio::time::timeout(secs(wait), rx.wait_for(|set| *set)).await.is_ok() {
                return;
            }
            match local.start().await {
                Ok(_) => return,
                Err(e @ (StartError::PortsTaken(_) | StartError::Tls(_))) => {
                    if attempt > 0 {
                        tracing::info!("lite: the Mac's page did not start yet ({})", e.name());
                    } else {
                        tracing::warn!("lite: the Mac's page did not start yet ({})", e.name());
                    }
                    attempt += 1;
                }
                Err(e) => {
                    // the task stops on it, said by type as `_ended` says it
                    tracing::warn!("lite: lite-page stopped ({})", e.name());
                    return;
                }
            }
        }
    }

    fn ready(self: &Arc<Self>) {
        let local = self.local();
        let page = match &local {
            Some(local) if local.running() => format!("on port {}", local.port()),
            _ => "not up yet".to_string(),
        };
        let voice = self.rtc().is_some_and(|rtc| rtc.voice_ready());
        tracing::info!(
            "lite: {}; the Mac's page {}; memory {}, apps {}, voice {}",
            self.state(),
            page,
            if self.memory().is_some() { "on" } else { "off" },
            if self.apps().is_some() { "signed in" } else { "not signed in" },
            if voice { "ready" } else { "off" }
        );
        if let Some(on_ready) = &self.on_ready {
            let outcome = guarded(|| on_ready(self)).and_then(|r| r);
            if let Err(e) = outcome {
                // the type only
                tracing::warn!("lite: on_ready failed ({})", error_name(&e));
            }
        }
    }

    /// A part's task, named `lite-<name>`; one that stopped on its own with a panic is said by type (its own lines
    /// say why).
    fn spawn<F>(&self, name: &str, fut: F) -> Job
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let full = format!("lite-{name}");
        let said = full.clone();
        let task = Job::spawn(&full, async move {
            if AssertUnwindSafe(fut).catch_unwind().await.is_err() {
                tracing::warn!("lite: {said} stopped (panic)");
            }
        });
        self.tasks.lock().push(task.clone());
        task
    }

    // -- stopping --

    /// Everything that started, stopped in order, each step on its own, so one that fails (logged by its type)
    /// leaves the rest to run: the Mac's page first (LinkChat's open streams end with their [DONE], the LAN door
    /// closes), then the WebRTC peers and their calls, then the brain (its own goodbye: TelegramInlet's shutdown),
    /// the watcher and the spend figures, then Composio's task; then memory (the conversation the brain left open is
    /// closed, and the dream is given DRAIN_SECS); the IPC socket last.
    async fn stop(self: &Arc<Self>) {
        let page_task = self.page_task.lock().clone();
        if let Some(task) = page_task {
            if !task.done() {
                // it ends by itself once it sees the stop; a start it is in the middle of finishes first, so the
                // page's sockets are already open when the page is stopped below, and none opens after it
                let _ = tokio::time::timeout(secs(PAGE_STOP_SECS), task.join()).await;
            }
        }
        let (local, rtc, apps, inlet) = {
            let parts = self.parts.lock();
            (parts.local.clone(), parts.rtc.clone(), parts.apps.clone(), parts.inlet.clone())
        };
        if let Some(local) = local {
            step("the page", async move { local.stop().await }).await;
        }
        if let Some(rtc) = rtc {
            step("the WebRTC peers", async move { rtc.close().await }).await;
        }
        let tasks: Vec<Job> = self.tasks.lock().iter().filter(|t| !t.done()).cloned().collect();
        for task in &tasks {
            task.cancel();
        }
        futures::future::join_all(tasks.iter().map(Job::join)).await;
        if let Some(inlet) = &inlet {
            // The brain's goodbye runs once its task is cancelled (the brain's own guard starts it); its last step
            // closes the conversation. It is given CLOSE_MARKERS_SECS to get there before the backstop below.
            self.await_goodbye(inlet).await;
        }
        if let Some(apps) = apps {
            step("Composio", apps.close()).await;
        }
        let me = self.clone();
        step("the open conversation", async move { me.close_open_conversation().await }).await;
        let me = self.clone();
        step("the dream", async move { me.drain_background().await }).await;
        let ipc_task = self.ipc_task.lock().clone();
        if let Some(task) = ipc_task {
            if !task.done() {
                task.cancel();
                task.join().await;
            }
        }
        let ipc = self.parts.lock().ipc.clone();
        if let Some(ipc) = ipc {
            step("the IPC socket", async move { ipc.stop().await }).await;
        }
        tracing::info!("lite: stopped{}", if self.exit_code() == EXIT_RESTART { " for a restart" } else { "" });
    }

    /// Wait (CLOSE_MARKERS_SECS at most) until the brain's goodbye has closed its conversation, or it had none.
    async fn await_goodbye(&self, inlet: &Arc<TelegramInlet>) {
        let deadline = tokio::time::Instant::now() + secs(CLOSE_MARKERS_SECS);
        while inlet.conversation().is_some() && self.memory().is_some() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// A restart must not leave the chat's conversation open in the transcripts: the brain writes its own close
    /// marker on the way out, and this is the backstop for one that did not (full eve's
    /// `Daemon._close_open_conversations`, for the one brain eve lite has). A conversation with nothing in it gets
    /// nothing.
    async fn close_open_conversation(&self) {
        let (memory, inlet) = {
            let parts = self.parts.lock();
            (parts.memory.clone(), parts.inlet.clone())
        };
        let conv = inlet.as_ref().and_then(|i| i.conversation()).unwrap_or_default();
        let Some(memory) = memory else { return };
        if conv.is_empty() {
            return;
        }
        let store = memory.transcripts.clone();
        let worker = tokio::task::spawn_blocking(move || close_conversation(&store, &conv));
        let closed = match tokio::time::timeout(secs(CLOSE_MARKERS_SECS), worker).await {
            Ok(Ok(closed)) => closed,
            // shutting down: best effort
            Ok(Err(e)) => {
                tracing::warn!("memory: close marker not written at shutdown ({})", join_error_name(&e));
                return;
            }
            Err(_) => {
                tracing::warn!("memory: close marker not written at shutdown (TimeoutError)");
                return;
            }
        };
        if let Some(inlet) = &inlet {
            if inlet.conversation().is_some() {
                inlet.door_forget_conversation(); // closed above: a late _close_chat must not close it twice
            }
        }
        if closed {
            tracing::info!("memory: the open conversation was closed at shutdown");
        }
    }

    /// Wait up to DRAIN_SECS for background work (the nightly dream) to finish, then cancel whatever is left (full
    /// eve's `Daemon._drain_background`).
    async fn drain_background(&self) {
        let pending = self.background.jobs();
        if pending.is_empty() {
            return;
        }
        let all = futures::future::join_all(pending.iter().map(Job::join));
        let _ = tokio::time::timeout(secs(DRAIN_SECS), all).await;
        let still: Vec<&Job> = pending.iter().filter(|j| !j.done()).collect();
        for task in &still {
            task.cancel();
        }
        if !still.is_empty() {
            futures::future::join_all(still.iter().map(|j| j.join())).await;
            tracing::warn!("shutdown: {} background task(s) cut off after {:.0} s", still.len(), DRAIN_SECS);
        }
    }

    // -- what the doors and the CLI read --

    /// WebContext.status: the state and whether a model key is saved; lite_web reads nothing else from it.
    pub fn web_status(&self) -> Value {
        json!({"state": self.state(), "openrouter": eve_openrouter::available(None)})
    }

    /// Signed in to Composio, and not a sign-in only a new one can fix (as the web app's Settings says it).
    fn composio_live(&self) -> bool {
        let signed_in = self.connect_path.is_file();
        let bridge = self.apps();
        signed_in && !bridge.is_some_and(|b| b.needs_sign_in())
    }

    /// Whether the last credit read found the OpenRouter key turned down (a 401). Read from the web's cached read,
    /// which `ipc_status` warms before it answers, so `doctor` sees a fresh answer.
    fn openrouter_rejected(&self) -> bool {
        let kept = self.web().and_then(|web| web.shared.credit());
        kept.is_some_and(|(_, _, view)| eve_lite_web::key_rejected(Some(&view)))
    }

    /// `{"evt": "status"}`, for `eve-bridge status` and `doctor`: words, booleans, ports and the firewall's counts by
    /// program or folder name. Never a key, a code, a path or an address.
    pub fn status(&self) -> Value {
        let (local, web, inlet, apps, rtc) = {
            let parts = self.parts.lock();
            (parts.local.clone(), parts.web.clone(), parts.inlet.clone(), parts.apps.clone(), parts.rtc.clone())
        };
        let lan = web.as_ref().and_then(|w| w.shared.lan());
        let page = local.as_ref().is_some_and(|l| l.running());
        let lan_on = lan.as_ref().is_some_and(|l| l.running());
        json!({
            "state": self.state(), "openrouter": eve_openrouter::available(None),
            "openrouter_rejected": self.openrouter_rejected(), "chat": inlet.is_some(),
            "composio": self.composio_live(), "apps": apps.as_ref().is_some_and(|a| a.started()),
            "voice": rtc.as_ref().is_some_and(|r| r.voice_ready()),
            "page": page, "port": if page { local.as_ref().map_or(0, |l| l.port()) } else { 0 },
            "lan": lan_on, "lan_port": if lan_on { lan.as_ref().map_or(0, |l| l.port()) } else { 0 },
            "local_network": web.as_ref().map_or_else(|| "unknown".to_string(), |w| w.shared.local_network()),
            "restarting": self.exit_code() == EXIT_RESTART || web.as_ref().is_some_and(|w| w.shared.restarting()),
            "firewall": eve_profile::firewall::firewall_counts(),
        })
    }

    // -- IPC (crate docs) --

    /// One request from `eve-bridge web`, `status` or `doctor`. Never fails: eve_ipc's server would log a failed
    /// request whole, and a login code may be in it.
    pub async fn handle_ipc(self: &Arc<Self>, req: Value) -> Value {
        let evt = req.as_object().and_then(|m| m.get("evt")).and_then(Value::as_str).unwrap_or("").to_string();
        let answer = AssertUnwindSafe(self.dispatch(&evt, &req)).catch_unwind().await;
        let failed = match answer {
            Ok(Ok(Some(reply))) => return reply,
            Ok(Ok(None)) => return json!({"ok": false, "error": UNKNOWN_EVENT}),
            Ok(Err(e)) => error_name(&e),
            Err(_) => "panic".to_string(),
        };
        // the type only
        tracing::warn!("lite: an IPC request failed ({failed})");
        json!({"ok": false, "error": eve_lite_web::FAILED})
    }

    /// `_IPC.get(evt)` and its answer; None for an event eve lite does not take.
    async fn dispatch(self: &Arc<Self>, evt: &str, req: &Value) -> anyhow::Result<Option<Value>> {
        Ok(Some(match evt {
            "web_login" => self.ipc_web_login(),
            "web_login_used" => self.ipc_web_login_used(req),
            "status" => self.ipc_status().await,
            "devices" => self.ipc_devices().await?,
            "revoke" => self.ipc_revoke(req).await,
            "lan" => self.ipc_lan(req).await?,
            _ => return Ok(None),
        }))
    }

    /// A one-time code for `http://localhost:<port>/#login=<code>`: `eve-bridge web open` hands it to the browser in
    /// a 0600 file, never on the screen.
    fn ipc_web_login(&self) -> Value {
        if self.stopping() {
            return json!({"ok": false, "error": STOPPING});
        }
        match self.local() {
            Some(local) if local.running() => json!({"ok": true, "code": self.mac_login.issue(), "port": local.port()}),
            _ => json!({"ok": false, "error": PAGE_DOWN}),
        }
    }

    fn ipc_web_login_used(&self, req: &Value) -> Value {
        let code = req.get("code").and_then(Value::as_str);
        json!({"ok": true, "used": self.mac_login.used(code)})
    }

    async fn ipc_status(&self) -> Value {
        // Read the OpenRouter credit now, so `openrouter_rejected` is fresh for `doctor` even when no browser has
        // polled (it is bounded by CREDIT_WAIT_SECS and never fails). Skipped while eve restarts into a key. The
        // page's own status read is what reads it (LocalServer.status reads the credit under the same conditions).
        let local = self.local();
        let restarting = self.web().is_some_and(|w| w.shared.restarting());
        if let Some(local) = local {
            if eve_openrouter::available(None) && !restarting {
                let _ = AssertUnwindSafe(local.status()).catch_unwind().await;
            }
        }
        let mut out = serde_json::Map::new();
        out.insert("ok".into(), Value::Bool(true));
        if let Value::Object(status) = self.status() {
            out.extend(status);
        }
        Value::Object(out)
    }

    async fn ipc_devices(&self) -> anyhow::Result<Value> {
        let (devices, list) = (self.devices.clone(), self.seams.list_devices.clone());
        let rows = tokio::task::spawn_blocking(move || list(&devices))
            .await
            .map_err(|e| anyhow::Error::new(eve_ipc::NamedError::new(join_error_name(&e), "the device list")))??;
        Ok(json!({"ok": true, "devices": serde_json::to_value(rows)?}))
    }

    /// Forget one device by its id, or every device with "all"; their WebRTC peers close (LocalServer.revoke).
    async fn ipc_revoke(&self, req: &Value) -> Value {
        let target = match req.get("id") {
            Some(Value::String(raw)) => eve_transcripts::py::strip(raw).to_lowercase(),
            _ => String::new(),
        };
        if !(target == "all" || eve_web_auth::DEVICE_ID.is_match(&target)) {
            return json!({"ok": false, "error": eve_lite_web::DEVICE_ID});
        }
        let Some(local) = self.local() else {
            return json!({"ok": false, "error": PAGE_DOWN});
        };
        match local.revoke(&target).await {
            Ok(gone) => json!({"ok": true, "revoked": gone}),
            Err(e) => {
                tracing::warn!("lite: a revoke was not saved ({})", eve_ipc::io_error_type_name(&e));
                json!({"ok": false, "error": eve_lite_web::NOT_SAVED})
            }
        }
    }

    /// The LAN door on or off, remembered in web.json (LocalServer.set_lan: off also closes a pairing window and the
    /// peers that came through the door).
    async fn ipc_lan(&self, req: &Value) -> anyhow::Result<Value> {
        let Some(Value::Bool(on)) = req.get("on") else {
            return Ok(json!({"ok": false, "error": eve_lite_web::LAN_FLAG}));
        };
        let Some(local) = self.local() else {
            return Ok(json!({"ok": false, "error": PAGE_DOWN}));
        };
        match local.set_lan(*on).await {
            Ok(_) => Ok(json!({"ok": true, "lan": local.lan_on()})),
            Err(e @ StartError::PortsTaken(_)) => {
                tracing::warn!("lite: the LAN door did not open ({})", e.name());
                Ok(json!({"ok": false, "error": eve_lite_web::LAN_FAILED}))
            }
            // the certificate could not be made or loaded
            Err(e @ (StartError::Tls(_) | StartError::Value(_))) => {
                tracing::warn!("lite: the LAN certificate is not usable ({})", e.name());
                Ok(json!({"ok": false, "error": eve_lite_web::TLS_OFF}))
            }
            Err(e @ StartError::Runtime(_)) => {
                Err(anyhow::Error::new(eve_ipc::NamedError::new(e.name(), e.to_string())))
            }
        }
    }
}

/// One step of the stop: a panic in it is logged by its type and leaves the rest to run.
async fn step<F: Future<Output = ()>>(what: &str, fut: F) {
    if AssertUnwindSafe(fut).catch_unwind().await.is_err() {
        // shutting down: best effort, the type only
        tracing::warn!("lite: {what} did not stop cleanly (panic)");
    }
}

// ---- the process ---------------------------------------------------------------------------------------------------

/// The CLI's parsed command line as `run_daemon` reads it: `socket` (--socket) and `log_level` (--log-level).
#[derive(Debug, Clone, Default)]
pub struct DaemonArgs {
    pub socket: Option<String>,
    pub log_level: Option<String>,
}

/// `eve-bridge daemon` in eve lite (the CLI calls it under EVE_PROFILE=lite): logging, then the process firewall
/// (profile.install_firewall, for the life of the process), then LiteDaemon until it stops. The exit code: EXIT_OK
/// when it was stopped or full eve is installed for this user, EXIT_RESTART after a sign-in, EXIT_CONFIG outside eve
/// lite, EXIT_IN_USE when another daemon has the socket. `options` go to LiteDaemon (tests); their `socket_path` is
/// replaced by the one chosen here.
pub fn run_daemon(args: Option<&DaemonArgs>, options: LiteOptions) -> i32 {
    let level = args.and_then(|a| a.log_level.clone()).filter(|l| !l.is_empty()).unwrap_or_else(|| "INFO".into());
    setup_logging(&level);
    // first: before any part of eve lite is built
    let lite = match eve_profile::firewall::install_firewall("enforce").map_err(|e| e.to_string()).and_then(|()| {
        eve_profile::is_lite(None).map_err(|e| e.to_string())
    }) {
        Ok(lite) => lite,
        Err(e) => {
            tracing::error!("lite: {e}");
            return EXIT_CONFIG;
        }
    };
    if !lite {
        tracing::error!("lite: EVE_PROFILE is not lite here, so eve lite's daemon does not start");
        return EXIT_CONFIG;
    }
    if full_eve_installed() {
        tracing::warn!(
            "lite: full eve's LaunchAgent ({FULL_LABEL}) is installed for this user, so eve lite stays down: one \
             edition per Mac account. Uninstall full eve's service to run eve lite."
        );
        return EXIT_OK;
    }
    // the CLI applied the profile right after the env file; applied again here, so the daemon runs on eve lite's
    // switches however it was started (idempotent: a second pass changes nothing)
    if let Err(e) = eve_profile::apply_process_env() {
        tracing::error!("lite: {e}");
        return EXIT_CONFIG;
    }
    let path = ipc_path(args.and_then(|a| a.socket.as_deref()));
    let in_use = eve_ipc::UnixTransport::new(path.to_string_lossy().into_owned()).is_ok_and(|t| t.is_in_use());
    if in_use {
        tracing::error!("lite: another eve daemon is already listening on eve lite's socket; this one stops");
        return EXIT_IN_USE;
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(e) => {
            tracing::error!("lite: no event loop could start ({})", eve_ipc::io_error_type_name(&e));
            return 1;
        }
    };
    let daemon = {
        let _entered = runtime.enter();
        LiteDaemon::new(LiteOptions { socket_path: Some(path.to_string_lossy().into_owned()), ..options })
    };
    // a stalled loop cannot log why it stalled; this thread does it from outside, and SIGUSR1 dumps every thread
    let watchdog = eve_loop_watchdog::LoopWatchdog::new(runtime.handle().clone()).start().ok();
    let _ = eve_loop_watchdog::register_dump_signal(libc::SIGUSR1);
    let code = runtime.block_on(async {
        stop_on_signals(&daemon);
        match daemon.run().await {
            Ok(code) => code,
            Err(e) => {
                tracing::error!("lite: eve lite's daemon stopped on an error ({})", error_name(&e));
                1
            }
        }
    });
    if let Some(watchdog) = watchdog {
        watchdog.stop();
    }
    runtime.shutdown_timeout(Duration::from_secs(1));
    code
}

/// `loop.add_signal_handler(sig, daemon.shutdown)` for SIGINT and SIGTERM.
fn stop_on_signals(daemon: &Arc<LiteDaemon>) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        for kind in [SignalKind::interrupt(), SignalKind::terminate()] {
            let Ok(mut stream) = signal(kind) else { continue };
            let me = Arc::downgrade(daemon);
            tokio::spawn(async move {
                while stream.recv().await.is_some() {
                    match me.upgrade() {
                        Some(d) => d.shutdown(),
                        None => return,
                    }
                }
            });
        }
    }
    #[cfg(not(unix))]
    {
        let me = Arc::downgrade(daemon);
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                if let Some(d) = me.upgrade() {
                    d.shutdown();
                }
            }
        });
    }
}

#[cfg(test)]
mod tests;
