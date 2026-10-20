//! Fresh, folder-scoped Telegram chats through the public Codex app-server.
//!
//! Port of `eve_bridge/codex_chat.py`. This session owns its new thread. It never selects, resumes, or writes to
//! an existing desktop task. Saved folder metadata is read-only; transcripts stay in Codex's own thread storage,
//! not Eve's diagnostic logs.
//!
//! `CodexChat` is Python's subclass of `CodexComputerAgent`: here it wraps one (and derefs to it, for the
//! inherited `status`, `steer`, `cancel`, `running`, `browser_screenshot`...), receives its events, and answers
//! command and file-change approvals in its place (eve-codex-computer's `set_request_handler`).

use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock, OnceLock, Weak};
use std::time::Duration;

use eve_agent_contract::AgentEvent;
use eve_codex_auth::AuthMode;
use eve_codex_computer::{
    meter_codex_in, py, Ask, CodexComputerAgent, CodexError, OnEvent, Options, PyError, TurnOutcome,
    BROWSER_INSTRUCTIONS, ONCE_ANSWERS, OWNER_ACTION_INSTRUCTIONS,
};
use eve_vault::pystr;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::watch;

/// The chat thread's developer instructions: the chat rules, then the owner's action policy, then the browser
/// rules (`CHAT_INSTRUCTIONS = """...""" + OWNER_ACTION_INSTRUCTIONS + BROWSER_INSTRUCTIONS`).
pub static CHAT_INSTRUCTIONS: LazyLock<String> = LazyLock::new(|| {
    format!("{CHAT_RULES}{OWNER_ACTION_INSTRUCTIONS}{BROWSER_INSTRUCTIONS}")
});

const CHAT_RULES: &str = "The owner is chatting with you through Eve on Telegram.
This is a new conversation in the selected folder. Follow its AGENTS.md and the
owner's requests. Keep replies and progress concise. Use your normal coding tools
for repository work and the installed cua API for UI work. Ask consequential
questions through request_user_input or native permission requests; Eve relays
them to the owner. Your public answers are delivered automatically; do not also
send duplicate replies through Telegram tools. Task-requested Telegram sends are
authorized. Do not claim an action succeeded without evidence.
ChatGPT's native UI is blocked by Computer Use; do not bypass that restriction.
";

const NOT_ACCESSIBLE: &str = "That folder is no longer accessible.";
const NOT_STARTED: &str = "Could not start a fresh Codex chat. Eve is still available.";
const OTHER_FOLDER: &str = "Codex opened a different folder. The chat was closed.";
const DISCONNECTED: &str = "Codex disconnected. Send codex <folder> to start a new chat.";
const STILL_STARTING: &str = "Codex is still starting your last message. Please wait.";
const NOT_CONFIRMED: &str = "Codex did not confirm the message. It was not retried. \
                             The chat is closed; check Codex before sending the work again.";
const NOTHING_RUNNING: &str = "Nothing is running in this Codex chat.";
const NO_REPLY: &str = "Codex ended without a reply.";
const STOPPED: &str = "Codex stopped.";
const NOT_FINISHED: &str = "Codex could not finish this message.";
const STOPPED_RESPONDING: &str = "Codex stopped responding. No work was retried. \
                                  Send codex off to return to Eve, or codex <folder> for a new chat.";
const UNREVIEWABLE: &str = "Codex requested permission without enough detail to review here. \
                            It was declined; no permission was changed.";
const COMMAND_APPROVAL: &str = "item/commandExecution/requestApproval";
const FILE_APPROVAL: &str = "item/fileChange/requestApproval";

// ---- Python's path and string semantics ------------------------------------------------------------------------

/// `str.casefold()`: full Unicode case folding.
fn casefold(s: &str) -> String {
    caseless::default_case_fold_str(s)
}

/// pathlib's parse of a POSIX path: its root (`""`, `"/"`, or exactly two slashes, which POSIX keeps) and its
/// parts without empty or `.` components.
fn parse_path(s: &str) -> (&'static str, Vec<&str>) {
    let root = if s.starts_with("//") && !s.starts_with("///") {
        "//"
    } else if s.starts_with('/') {
        "/"
    } else {
        ""
    };
    (root, s.split('/').filter(|p| !p.is_empty() && *p != ".").collect())
}

fn format_path(root: &str, parts: &[&str]) -> String {
    let joined = parts.join("/");
    if root.is_empty() && joined.is_empty() {
        ".".into()
    } else {
        format!("{root}{joined}")
    }
}

/// `str(Path(s))`.
fn path_str(s: &str) -> String {
    let (root, parts) = parse_path(s);
    format_path(root, &parts)
}

/// `str(path)` for a path held as a `Path`.
fn path_text(path: &Path) -> String {
    path_str(&path.to_string_lossy())
}

/// `p.name`: the last part (`""` for the root).
fn name(path: &Path) -> String {
    let text = path.to_string_lossy();
    let (_, parts) = parse_path(&text);
    parts.last().map(|p| p.to_string()).unwrap_or_default()
}

/// `p.parts`.
fn parts(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|c| match c {
            Component::RootDir => Some("/".to_string()),
            Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
            Component::ParentDir => Some("..".to_string()),
            Component::CurDir | Component::Prefix(_) => None,
        })
        .collect()
}

/// A password-database entry's home directory: the user named `user` (`pwd.getpwnam(user).pw_dir`), or with no
/// name the current user (`pwd.getpwuid(os.getuid()).pw_dir`). `None` when there is no such entry.
fn user_home(user: Option<&str>) -> Option<String> {
    let name = match user {
        Some(u) => Some(std::ffi::CString::new(u).ok()?),
        None => None,
    };
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    loop {
        // SAFETY: an all-zero passwd is a valid out-parameter; getpwnam_r fills it and points its strings into
        // `buf`, which outlives every read below.
        let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: `name` is NUL-terminated, `buf` is writable for `buf.len()` bytes, and `pw`/`found` are valid.
        let rc = unsafe {
            match &name {
                Some(name) => libc::getpwnam_r(name.as_ptr(), &mut pw, buf.as_mut_ptr(), buf.len(), &mut found),
                None => libc::getpwuid_r(libc::getuid(), &mut pw, buf.as_mut_ptr(), buf.len(), &mut found),
            }
        };
        if rc == libc::ERANGE && buf.len() < 1 << 20 {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc != 0 || found.is_null() || pw.pw_dir.is_null() {
            return None;
        }
        // SAFETY: on success `pw.pw_dir` is a NUL-terminated string inside `buf`.
        let dir = unsafe { std::ffi::CStr::from_ptr(pw.pw_dir) };
        return Some(dir.to_string_lossy().into_owned());
    }
}

/// `os.path.expanduser(part)` for one leading part (`~` or `~user`, no slash): `$HOME` (or the password
/// database when it is unset) without trailing slashes, `/` when that leaves nothing; the part unchanged when
/// there is no such user.
fn expand_tilde(part: &str, home: Option<&str>) -> String {
    let user = &part[1..];
    let found = if user.is_empty() {
        match home {
            Some(h) => Some(h.to_string()),
            None => user_home(None),
        }
    } else {
        user_home(Some(user))
    };
    match found {
        None => part.to_string(),
        Some(h) => {
            let h = h.trim_end_matches('/');
            if h.is_empty() { "/".to_string() } else { h.to_string() }
        }
    }
}

/// `Path.home()`.
fn py_home(home: Option<&str>) -> Result<PathBuf, CodexError> {
    let h = expand_tilde("~", home);
    if h.starts_with('~') {
        return Err(CodexError::Runtime("Could not determine home directory.".into()));
    }
    Ok(PathBuf::from(h))
}

/// `str(Path(value).expanduser())`; `RuntimeError` when a leading `~` names no home directory.
fn expanduser_str(value: &str, home: Option<&str>) -> Result<String, CodexError> {
    let (root, parts) = parse_path(value);
    match parts.first() {
        Some(first) if root.is_empty() && first.starts_with('~') => {
            let expanded = expand_tilde(first, home);
            if expanded.starts_with('~') {
                return Err(CodexError::Runtime("Could not determine home directory.".into()));
            }
            let (root, mut head) = parse_path(&expanded);
            head.extend_from_slice(&parts[1..]);
            Ok(format_path(root, &head))
        }
        _ => Ok(format_path(root, &parts)),
    }
}

/// `os.access(path, os.R_OK | os.X_OK)`.
fn readable_dir(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
    // SAFETY: `c` is a valid NUL-terminated path for the duration of the call.
    unsafe { libc::access(c.as_ptr(), libc::R_OK | libc::X_OK) == 0 }
}

/// `path.is_dir() and os.access(path, os.R_OK | os.X_OK)`.
fn accessible(path: &Path) -> bool {
    path.is_dir() && readable_dir(path)
}

// ---- folders ---------------------------------------------------------------------------------------------------

/// The Codex home: `CODEX_HOME`, else `~/.codex` (Python evaluates `Path.home()` either way, so a home that
/// cannot be determined raises even when `CODEX_HOME` is set).
pub fn codex_home() -> Result<PathBuf, CodexError> {
    let home = py_home(std::env::var("HOME").ok().as_deref())?;
    Ok(match std::env::var_os("CODEX_HOME") {
        Some(h) => PathBuf::from(h),
        None => home.join(".codex"),
    })
}

/// Only existing, accessible local roots saved in Codex, never task history.
///
/// Reads `<home>/.codex-global-state.json` (`home` default [`codex_home`]) and returns the resolved, readable
/// directories its `local-projects` list, sorted by casefolded name then path. A file that cannot be read or
/// parsed, or a `local-projects` that is not an object, is `Unavailable("Could not read saved Codex folders.")`;
/// a state that is not an object is the `AttributeError` Python raises.
pub fn accessible_folders(home: Option<&Path>) -> Result<Vec<PathBuf>, CodexError> {
    let home = match home {
        Some(h) => h.to_path_buf(),
        None => codex_home()?,
    };
    let unreadable = || CodexError::Unavailable("Could not read saved Codex folders.".into());
    let bytes = std::fs::read(home.join(".codex-global-state.json")).map_err(|_| unreadable())?;
    let text = String::from_utf8(bytes).map_err(|_| unreadable())?;
    let state: Value = serde_json::from_str(&text).map_err(|_| unreadable())?;
    let empty = json!({});
    let projects = py::get_or(&state, "local-projects", &empty)?;
    let Some(projects) = projects.as_object() else { return Err(unreadable()) };
    let mut roots: Vec<PathBuf> = vec![];
    for project in projects.values() {
        let Some(project) = project.as_object() else { continue };
        let Some(Value::Array(paths)) = project.get("rootPaths") else { continue };
        for root in paths {
            let Some(root) = root.as_str() else { continue };
            if !root.starts_with('/') {
                continue;
            }
            if root.contains('\0') {
                return Err(unreadable()); // ValueError: embedded null byte
            }
            let path = eve_codex_auth::resolve(Path::new(root));
            if accessible(&path) && !roots.contains(&path) {
                roots.push(path);
            }
        }
    }
    roots.sort_by_cached_key(|p| (casefold(&name(p)), path_text(p)));
    Ok(roots)
}

/// One line per folder: its name, or its full path where another folder shares the name. Folders inside a
/// debrief folder (`debrief`, `.debrief`) are left out.
pub fn folder_menu(folders: &[PathBuf]) -> String {
    let folders: Vec<&PathBuf> = folders
        .iter()
        .filter(|p| !parts(p).iter().any(|part| casefold(part).trim_start_matches('.') == "debrief"))
        .collect();
    let names: Vec<String> = folders.iter().map(|p| casefold(&name(p))).collect();
    folders
        .iter()
        .zip(&names)
        .map(|(p, n)| if names.iter().filter(|q| *q == n).count() > 1 { path_text(p) } else { name(p) })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The folder the owner named: by name (any case) or by full path (`~` expanded, quotes and a trailing slash
/// ignored).
pub fn select_folder(folders: &[PathBuf], selection: &str) -> Result<PathBuf, CodexError> {
    select_folder_in(folders, selection, std::env::var("HOME").ok().as_deref())
}

/// [`select_folder`] with an explicit `$HOME` (`None`: unset).
pub fn select_folder_in(folders: &[PathBuf], selection: &str, home: Option<&str>) -> Result<PathBuf, CodexError> {
    let value = pystr::strip(selection).trim_matches(['"', '\'']).trim_end_matches('/');
    let value = if value.is_empty() { "/" } else { value };
    let expanded = casefold(&expanduser_str(value, home)?);
    let wanted = casefold(value);
    let matches: Vec<&PathBuf> = folders
        .iter()
        .filter(|p| casefold(&name(p)) == wanted || casefold(&path_text(p)) == expanded)
        .collect();
    match matches.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(CodexError::Unavailable("That folder is not available. Send codex on to see folders.".into())),
        many => Err(CodexError::Unavailable(format!(
            "More than one folder has that name. Use its full path:\n{}",
            many.iter().map(|p| path_text(p)).collect::<Vec<_>>().join("\n")
        ))),
    }
}

// ---- the chat --------------------------------------------------------------------------------------------------

/// A text callback (`emit`, `done`): a step or a result on its way to the owner. An error ends the delivery as
/// a raised exception does in Python.
pub type Emit = Arc<dyn Fn(String) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;
/// The `picture` callback: send the verified browser picture (read from `browser_screenshot()`).
pub type Picture = Arc<dyn Fn() -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

/// An [`Emit`] from an async function that always delivers.
pub fn emit_fn<F, Fut>(f: F) -> Emit
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    Arc::new(move |text| {
        let fut = f(text);
        Box::pin(async move {
            fut.await;
            Ok(())
        })
    })
}

/// A [`Picture`] from an async function that always delivers.
pub fn picture_fn<F, Fut>(f: F) -> Picture
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    Arc::new(move || {
        let fut = f();
        Box::pin(async move {
            fut.await;
            Ok(())
        })
    })
}

/// A callback's failure as the exception it would be: a `CodexError` as itself, anything else as `Exception`.
fn raised(e: anyhow::Error) -> CodexError {
    match e.downcast::<CodexError>() {
        Ok(c) => c,
        Err(e) => CodexError::Py(PyError::new("Exception", e.to_string())),
    }
}

/// What `start` and `send` catch: `OSError`, `TimeoutError`, `KeyError` and `CodexUnavailable`.
fn caught(e: &CodexError) -> bool {
    match e {
        CodexError::Os { .. } | CodexError::Timeout | CodexError::Unavailable(_) => true,
        CodexError::Py(p) => p.class == "KeyError",
        _ => false,
    }
}

fn secs(s: f64) -> Duration {
    Duration::try_from_secs_f64(s.max(0.0)).unwrap_or(Duration::MAX)
}

/// An `asyncio.Task`: cancellable, awaitable any number of times, and remembering what it raised.
struct Job {
    abort: tokio::task::AbortHandle,
    finished: watch::Receiver<bool>,
    raised: Arc<Mutex<Option<CodexError>>>,
}

/// Marks a job finished when its future ends or is dropped (cancelled).
struct Finished(watch::Sender<bool>);

impl Drop for Finished {
    fn drop(&mut self) {
        self.0.send_replace(true);
    }
}

impl Job {
    fn spawn(fut: impl Future<Output = Option<CodexError>> + Send + 'static) -> Arc<Job> {
        let (tx, rx) = watch::channel(false);
        let flag = Finished(tx);
        let raised = Arc::new(Mutex::new(None));
        let slot = raised.clone();
        let handle = tokio::spawn(async move {
            let _flag = flag;
            *slot.lock() = fut.await;
        });
        Arc::new(Job { abort: handle.abort_handle(), finished: rx, raised })
    }

    fn done(&self) -> bool {
        *self.finished.borrow()
    }

    fn cancel(&self) {
        self.abort.abort();
    }

    /// Until it has finished or stopped (`gather(job, return_exceptions=True)`).
    async fn finish(&self) {
        let mut rx = self.finished.clone();
        let _ = rx.wait_for(|done| *done).await;
    }

    /// `await job`: what it raised, if anything.
    async fn wait(&self) -> Result<(), CodexError> {
        self.finish().await;
        match self.raised.lock().clone() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// Cancels the awaited step deliveries if the turn job is cancelled while it waits on them, as `gather` does.
struct CancelUnlessDone(Vec<Arc<Job>>);

impl Drop for CancelUnlessDone {
    fn drop(&mut self) {
        for job in &self.0 {
            if !job.done() {
                job.cancel();
            }
        }
    }
}

#[derive(Default)]
struct ChatState {
    output: Option<Emit>,
    picture: Option<Picture>,
    /// The turn's result, when the caller tells it apart from progress (Telegram edits one progress message and
    /// sends the result as a new one); without it the result goes to the output like a step.
    result: Option<Emit>,
    turn_job: Option<Arc<Job>>,
    /// Steps on their way to the caller, awaited before the result.
    outputs: Vec<Arc<Job>>,
    connected: bool,
    cwd: Option<PathBuf>,
}

struct Inner {
    agent: CodexComputerAgent,
    st: Mutex<ChatState>,
}

/// One fresh Codex thread in a chosen folder, driven one message at a time from Telegram.
///
/// A handle: clones share one chat. Derefs to its [`CodexComputerAgent`] for what the Python subclass
/// inherits.
#[derive(Clone)]
pub struct CodexChat {
    inner: Arc<Inner>,
}

impl std::ops::Deref for CodexChat {
    type Target = CodexComputerAgent;

    fn deref(&self) -> &CodexComputerAgent {
        &self.inner.agent
    }
}

impl std::fmt::Debug for CodexChat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexChat").field("connected", &self.connected()).field("agent", &self.inner.agent).finish()
    }
}

/// The `finally` of `_finish_turn`: runs however the turn job ends, cancellation included.
struct TurnFinally(Arc<Inner>);

impl Drop for TurnFinally {
    fn drop(&mut self) {
        let agent = &self.0.agent;
        if !agent.is_cancelled() {
            agent.finish_metering();
        }
        agent.set_running(false);
        agent.set_turn_id(Value::Null);
    }
}

impl CodexChat {
    /// `CodexChat(ask_user=..., **kw)`: `options` are the agent's keyword arguments. A vault that cannot be
    /// opened is logged and the chat runs without it (see [`CodexComputerAgent::new`]).
    pub fn new(ask_user: Ask, options: Options) -> Self {
        Self::build(|on_event| Ok::<_, std::convert::Infallible>(CodexComputerAgent::new(on_event, ask_user, options)))
            .unwrap_or_else(|e| match e {})
    }

    /// [`new`](Self::new), raising as Python's constructor does when `Vault()` cannot be made.
    pub fn try_new(ask_user: Ask, options: Options) -> Result<Self, eve_vault::VaultError> {
        Self::build(|on_event| CodexComputerAgent::try_new(on_event, ask_user, options))
    }

    /// The agent with `on_event=self._event`, and this chat's approvals in place of the agent's.
    fn build<E>(make: impl FnOnce(OnEvent) -> Result<CodexComputerAgent, E>) -> Result<Self, E> {
        let cell: Arc<OnceLock<Weak<Inner>>> = Arc::new(OnceLock::new());
        let events = cell.clone();
        let on_event: OnEvent = Arc::new(move |e: AgentEvent| {
            if let Some(inner) = events.get().and_then(Weak::upgrade) {
                inner.event(&e);
            }
        });
        let agent = make(on_event)?;
        let inner = Arc::new(Inner { agent, st: Mutex::new(ChatState::default()) });
        let _ = cell.set(Arc::downgrade(&inner));
        let weak = Arc::downgrade(&inner);
        inner.agent.set_request_handler(Some(Arc::new(move |msg: &Value| {
            let inner = weak.upgrade()?;
            let own = inner.handles(msg)?;
            let msg = msg.clone();
            Some(Box::pin(async move {
                if own {
                    inner.answer_approval(msg).await;
                }
            }) as BoxFuture<'static, ()>)
        })));
        Ok(Self { inner })
    }

    /// The agent this chat is built on.
    pub fn agent(&self) -> &CodexComputerAgent {
        &self.inner.agent
    }

    /// The folder of the current (or last) chat.
    pub fn cwd(&self) -> Option<PathBuf> {
        self.inner.st.lock().cwd.clone()
    }

    /// Started, and its app-server and reader still alive.
    pub fn connected(&self) -> bool {
        self.inner.connected()
    }

    /// Close any chat, then start a new thread in `folder`: steps go to `emit`, the turn's result to `done` (or
    /// `emit` without it), and after a completed turn that used the browser, `picture` is called.
    pub async fn start(
        &self,
        folder: &Path,
        emit: Emit,
        picture: Option<Picture>,
        done: Option<Emit>,
    ) -> Result<(), CodexError> {
        self.close().await;
        let agent = &self.inner.agent;
        agent.set_cancelled(false);
        {
            let mut st = self.inner.st.lock();
            st.cwd = Some(folder.to_path_buf());
            st.output = Some(emit);
            st.picture = picture;
            st.result = done;
        }
        if !accessible(folder) {
            return Err(CodexError::Unavailable(NOT_ACCESSIBLE.into()));
        }
        match self.inner.open(folder).await {
            Ok(()) => Ok(()),
            Err(e) if caught(&e) => {
                self.close().await;
                Err(CodexError::Unavailable(NOT_STARTED.into()))
            }
            Err(e) => Err(e),
        }
    }

    /// Send the owner's message: a new turn, or a correction to the running one. A message Codex does not
    /// confirm closes the chat and is never retried.
    pub async fn send(&self, text: &str) -> Result<(), CodexError> {
        let inner = &self.inner;
        let agent = &inner.agent;
        if !inner.connected() {
            return Err(CodexError::Unavailable(DISCONNECTED.into()));
        }
        let job = inner.st.lock().turn_job.clone();
        if let Some(job) = job {
            if agent.done_finished() {
                job.wait().await?;
            }
        }
        if agent.running() {
            let turn = agent.turn_id();
            if !py::truthy(&turn) {
                return Err(CodexError::Unavailable(STILL_STARTING.into()));
            }
            let params = json!({"threadId": agent.thread_id(), "expectedTurnId": turn,
                                "input": [{"type": "text", "text": text}]});
            agent.rpc("turn/steer", params).await?;
            return Ok(());
        }
        agent.set_running(true);
        agent.reset_turn(text);
        let done = agent.new_done();
        agent.begin_metering("chat turn");
        let params = json!({"threadId": agent.thread_id(), "input": [{"type": "text", "text": text}]});
        let started = match agent.rpc("turn/start", params).await {
            Ok(result) => py::index(&result, "turn").and_then(|t| py::index(t, "id")).cloned().map_err(CodexError::from),
            Err(e) => Err(e),
        };
        match started {
            Ok(turn_id) => {
                agent.set_turn_id(turn_id.clone());
                agent.set_meter_turn_id(turn_id);
            }
            Err(e) if caught(&e) => {
                self.close().await;
                return Err(CodexError::Unavailable(NOT_CONFIRMED.into()));
            }
            Err(e) => return Err(e),
        }
        if agent.auth_mode() == AuthMode::Plan {
            meter_codex_in(agent.ledger(), "chat turn");
        }
        let runner = inner.clone();
        let job = Job::spawn(async move { runner.finish_turn(done).await });
        inner.st.lock().turn_job = Some(job);
        Ok(())
    }

    /// `await chat._turn_job`: until the current turn's result has been delivered (at once when there is none).
    pub async fn wait_turn(&self) -> Result<(), CodexError> {
        let job = self.inner.st.lock().turn_job.clone();
        match job {
            Some(job) => job.wait().await,
            None => Ok(()),
        }
    }

    /// Stop the running turn; the chat stays usable.
    pub async fn interrupt(&self) -> Result<(), CodexError> {
        self.inner.interrupt().await
    }

    /// `_answer_request(msg)`: command and file-change approvals are asked here, with exactly their scope;
    /// everything else is the agent's.
    pub async fn answer_request(&self, msg: Value) {
        match self.inner.handles(&msg) {
            Some(true) => self.inner.answer_approval(msg).await,
            Some(false) => {}
            None => self.inner.agent.answer_request(msg).await,
        }
    }

    /// End the chat: interrupt a running turn, stop its delivery, close the app-server and record its spend.
    pub async fn close(&self) {
        self.inner.close().await;
    }

    /// `_event(event)`: the agent's events; a progress step goes to `emit`.
    pub fn event(&self, event: &AgentEvent) {
        self.inner.event(event);
    }
}

impl Inner {
    fn connected(&self) -> bool {
        self.st.lock().connected && self.agent.process_alive()
    }

    fn event(&self, event: &AgentEvent) {
        if event.kind() != "progress" {
            return;
        }
        let Some(output) = self.st.lock().output.clone() else { return };
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        // Not one of the permission jobs _finish_turn cancels: a step is delivered, never dropped.
        let fut = output(event.text().to_string());
        let job = Job::spawn(async move {
            let _ = fut.await;
            None
        });
        let mut st = self.st.lock();
        st.outputs.retain(|j| !j.done());
        st.outputs.push(job);
    }

    /// Whether the chat answers this request itself: `Some(true)` for a command or file-change approval,
    /// `Some(false)` where Python's override raises before answering (`msg['method']` on a message that is not a
    /// dict or has no method, or an unhashable method), `None` for the agent's own answer.
    fn handles(&self, msg: &Value) -> Option<bool> {
        let Some(method) = msg.as_object().and_then(|m| m.get("method")) else { return Some(false) };
        if py::hashable(method).is_err() {
            return Some(false);
        }
        matches!(method.as_str(), Some(COMMAND_APPROVAL | FILE_APPROVAL)).then_some(true)
    }

    async fn open(&self, folder: &Path) -> Result<(), CodexError> {
        let agent = &self.agent;
        agent.launch().await?;
        let params = json!({
            "cwd": path_text(folder), "ephemeral": false, "sandbox": "workspace-write",
            "approvalPolicy": "on-request", "developerInstructions": CHAT_INSTRUCTIONS.as_str(),
        });
        let result = agent.rpc("thread/start", params).await?;
        agent.thread_started(&result)?;
        let cwd = py::index(py::index(&result, "thread")?, "cwd")?;
        let Some(cwd) = cwd.as_str() else {
            return Err(PyError::new("TypeError", "expected str, bytes or os.PathLike object").into());
        };
        if cwd.contains('\0') {
            return Err(PyError::new("ValueError", "embedded null byte").into());
        }
        if eve_codex_auth::resolve(Path::new(cwd)) != eve_codex_auth::resolve(folder) {
            return Err(CodexError::Unavailable(OTHER_FOLDER.into()));
        }
        self.st.lock().connected = true;
        tracing::info!("codex chat: started thread={}", pystr::value_str(&agent.thread_id()));
        Ok(())
    }

    async fn interrupt(&self) -> Result<(), CodexError> {
        let agent = &self.agent;
        let turn = agent.turn_id();
        if !agent.running() || !py::truthy(&turn) {
            return Err(CodexError::Unavailable(NOTHING_RUNNING.into()));
        }
        agent.rpc("turn/interrupt", json!({"threadId": agent.thread_id(), "turnId": turn})).await?;
        Ok(())
    }

    /// `_finish_turn`: deliver the turn's result once it ends. Returns what it raised, if anything.
    async fn finish_turn(self: Arc<Self>, mut done: watch::Receiver<Option<TurnOutcome>>) -> Option<CodexError> {
        let _finally = TurnFinally(self.clone());
        match self.deliver(&mut done).await {
            Ok(()) => None,
            Err(CodexError::Timeout | CodexError::Unavailable(_)) => {
                self.st.lock().connected = false;
                match tokio::time::timeout(Duration::from_secs(3), self.interrupt()).await {
                    Err(_) | Ok(Ok(())) | Ok(Err(CodexError::Unavailable(_) | CodexError::Timeout)) => {}
                    Ok(Err(other)) => return Some(other),
                }
                let out = {
                    let st = self.st.lock();
                    st.result.clone().or_else(|| st.output.clone())
                };
                match out {
                    Some(out) => out(STOPPED_RESPONDING.to_string()).await.err().map(raised),
                    None => None,
                }
            }
            Err(other) => Some(other),
        }
    }

    async fn deliver(&self, done: &mut watch::Receiver<Option<TurnOutcome>>) -> Result<(), CodexError> {
        let agent = &self.agent;
        let outcome = match tokio::time::timeout(secs(agent.max_secs()), done.wait_for(Option::is_some)).await {
            Err(_) => return Err(CodexError::Timeout),
            Ok(Err(_)) => return Err(CodexError::Cancelled),
            Ok(Ok(v)) => v.clone().unwrap_or(Err(CodexError::Cancelled)),
        };
        let turn = outcome?;
        // A step emitted just before the turn completed is delivered first, in order: cancelled with the jobs
        // below it was lost, and left running it arrived after the result (review, 2026-09-23).
        let outputs: Vec<Arc<Job>> = self.st.lock().outputs.iter().filter(|j| !j.done()).cloned().collect();
        if !outputs.is_empty() {
            let pending = CancelUnlessDone(outputs);
            for job in &pending.0 {
                job.finish().await;
            }
        }
        // An interrupted/failed turn can leave a permission question waiting.
        // Cancel it before the next turn so its answer cannot approve stale work.
        agent.cancel_jobs().await;
        let status = py::get(&turn, "status")?.clone();
        let result = match status.as_str() {
            Some("completed") => {
                let text = agent.final_text();
                if text.is_empty() { NO_REPLY.to_string() } else { text }
            }
            Some("interrupted") => STOPPED.to_string(),
            _ => NOT_FINISHED.to_string(),
        };
        let (out, picture) = {
            let st = self.st.lock();
            (st.result.clone().or_else(|| st.output.clone()), st.picture.clone())
        };
        if let Some(out) = out {
            out(result).await.map_err(raised)?;
        }
        if status.as_str() == Some("completed") && agent.browser_used() {
            if let Some(picture) = picture {
                picture().await.map_err(raised)?;
            }
        }
        Ok(())
    }

    /// The command and file-change approvals: only a command shown in full is asked, and only `yes`/`allow`/
    /// `allow once`/`approve` accepts it, for this one call. Anything else is declined.
    async fn answer_approval(&self, msg: Value) {
        let agent = &self.agent;
        let Some(obj) = msg.as_object() else { return };
        let method = obj.get("method").and_then(Value::as_str).unwrap_or_default();
        let params = obj.get("params").cloned().unwrap_or_else(|| json!({}));
        let mut decision = "decline";
        let _held = agent.lock_approvals().await;
        // `params.get(...)` on a params that is not a dict raises: nothing is sent.
        let Ok(thread) = py::get(&params, "threadId") else { return };
        if py::eq(thread, &agent.thread_id()) && !agent.is_cancelled() {
            let question = if method == COMMAND_APPROVAL {
                let detail = py::get(&params, "command").unwrap_or(&Value::Null);
                if py::truthy(detail) {
                    let cwd = match params.get("cwd") {
                        Some(v) => pystr::value_str(v),
                        None => self.st.lock().cwd.as_deref().map(path_text).unwrap_or_else(|| "None".into()),
                    };
                    format!("Codex asks to run this command in {cwd}:\n{}", pystr::value_str(detail))
                } else {
                    String::new()
                }
            } else {
                // This protocol form has no patch content to review here.
                String::new()
            };
            if !question.is_empty() {
                // `question + '\n' + reason` raises on a reason that is not a string: a question that could not be
                // asked is a decline, sent now (unanswered, Codex would wait on the approval until max_secs).
                let reason = match params.get("reason") {
                    Some(r) if py::truthy(r) => r.as_str(),
                    _ => Some(""),
                };
                if let Some(reason) = reason {
                    let ask = agent.ask_user();
                    if let Ok(answer) = ask(format!("{question}\n{reason}\nReply yes or no.")).await {
                        let answer = pystr::strip(&answer).to_lowercase();
                        if ONCE_ANSWERS.contains(&answer.trim_end_matches(['.', '!'])) {
                            decision = "accept";
                        }
                    }
                }
            } else {
                agent.emit("progress", UNREVIEWABLE);
            }
        }
        if !agent.is_cancelled() {
            // `msg['id']` raises on a message without one: nothing is sent.
            let Some(id) = obj.get("id").cloned() else { return };
            let _ = agent.send_message(&json!({"id": id, "result": {"decision": decision}})).await;
        }
    }

    async fn close(&self) {
        let agent = &self.agent;
        self.st.lock().connected = false;
        agent.set_cancelled(true);
        if agent.running() && py::truthy(&agent.turn_id()) && agent.done_pending() {
            let _ = tokio::time::timeout(Duration::from_secs(3), self.interrupt()).await;
        }
        let job = self.st.lock().turn_job.take();
        if let Some(job) = job {
            job.cancel();
            job.finish().await;
        }
        agent.close_process().await;
        agent.finish_metering();
        agent.detach().await;
        agent.set_thread_id(Value::Null);
        agent.set_turn_id(Value::Null);
        agent.set_running(false);
        let mut st = self.st.lock();
        st.output = None;
        st.picture = None;
        st.result = None;
    }
}

#[cfg(test)]
mod tests;
