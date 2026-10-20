//! What the brain's modules share and what the daemon lends them, as types: Python's `asyncio.Future`,
//! `asyncio.Event`, tasks and context variables; the callables the daemon passes in; and the duck-typed objects
//! (a computer task, the memory, eve's apps' hubs, the camera's scene, the Live connection, the Mini App) as
//! traits, so the tests drive every module with fakes, as Python's did.
//!
//! Architect-owned: no fill unit edits this file. Every module reaches it through the prelude.

use std::any::Any;
use std::cell::RefCell;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use futures::future::BoxFuture;

use crate::prelude::*;

pub use eve_link::seams::AppOut;
pub use eve_phone_call::Reader;

// ---- asyncio.Future[str]: a question waiting on the owner ------------------------------------------------------

/// How a [`Pending`] ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    /// `future.set_result(answer)`.
    Answer(String),
    /// `future.cancel()`.
    Cancelled,
}

/// `asyncio.CancelledError` out of [`Pending::wait`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("cancelled")]
pub struct CancelledError;

struct PendingInner {
    tx: tokio::sync::watch::Sender<Option<Settled>>,
    callbacks: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
}

/// Python's `loop.create_future()` for one answer: settled once (an answer or a cancel), awaited by any number
/// of waiters, with done callbacks. Two handles are the same future when they share it (`is`): [`Pending::same`]
/// and `==`.
#[derive(Clone)]
pub struct Pending(Arc<PendingInner>);

impl Pending {
    pub fn new() -> Self {
        let (tx, _rx) = tokio::sync::watch::channel(None);
        Self(Arc::new(PendingInner { tx, callbacks: Mutex::new(Vec::new()) }))
    }

    fn settle(&self, how: Settled) -> bool {
        let mut first = false;
        self.0.tx.send_if_modified(|slot| {
            if slot.is_none() {
                *slot = Some(how);
                first = true;
                true
            } else {
                false
            }
        });
        if first {
            let callbacks = std::mem::take(&mut *self.0.callbacks.lock());
            for callback in callbacks {
                callback();
            }
        }
        first
    }

    /// `future.set_result(answer)` → false when it was already done (Python raises InvalidStateError; every
    /// caller checks `done()` first, so false is that check).
    pub fn set_result(&self, answer: impl Into<String>) -> bool {
        self.settle(Settled::Answer(answer.into()))
    }

    /// `future.cancel()` → whether it was still pending.
    pub fn cancel(&self) -> bool {
        self.settle(Settled::Cancelled)
    }

    /// `future.done()`.
    pub fn done(&self) -> bool {
        self.0.tx.borrow().is_some()
    }

    /// `future.cancelled()`.
    pub fn cancelled(&self) -> bool {
        matches!(*self.0.tx.borrow(), Some(Settled::Cancelled))
    }

    /// How it ended, or None while it waits.
    pub fn outcome(&self) -> Option<Settled> {
        self.0.tx.borrow().clone()
    }

    /// `future.add_done_callback(f)`: `f` runs once it is settled (at once when it already is).
    pub fn add_done_callback(&self, f: impl FnOnce() + Send + 'static) {
        {
            let mut callbacks = self.0.callbacks.lock();
            if !self.done() {
                callbacks.push(Box::new(f));
                return;
            }
        }
        f();
    }

    /// `await future`: the answer, or [`CancelledError`].
    pub async fn wait(&self) -> Result<String, CancelledError> {
        let mut rx = self.0.tx.subscribe();
        let settled = match rx.wait_for(|slot| slot.is_some()).await {
            Ok(slot) => slot.clone(),
            Err(_) => None,
        };
        match settled {
            Some(Settled::Answer(answer)) => Ok(answer),
            _ => Err(CancelledError),
        }
    }

    /// `a is b`.
    pub fn same(&self, other: &Pending) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Default for Pending {
    fn default() -> Self {
        Self::new()
    }
}

impl PartialEq for Pending {
    fn eq(&self, other: &Self) -> bool {
        self.same(other)
    }
}

impl std::fmt::Debug for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Pending({:p}, {:?})", Arc::as_ptr(&self.0), self.outcome())
    }
}

// ---- asyncio.Event ---------------------------------------------------------------------------------------------

/// Python's `asyncio.Event`: set once (or cleared again), awaited by any number of waiters.
pub struct Flag(tokio::sync::watch::Sender<bool>);

impl Flag {
    pub fn new() -> Self {
        let (tx, _rx) = tokio::sync::watch::channel(false);
        Self(tx)
    }

    /// `event.set()`.
    pub fn set(&self) {
        self.0.send_if_modified(|on| !std::mem::replace(on, true));
    }

    /// `event.clear()`.
    pub fn clear(&self) {
        self.0.send_if_modified(|on| std::mem::replace(on, false));
    }

    /// `event.is_set()`.
    pub fn is_set(&self) -> bool {
        *self.0.borrow()
    }

    /// `await event.wait()`.
    pub async fn wait(&self) {
        let mut rx = self.0.subscribe();
        let _ = rx.wait_for(|on| *on).await;
    }
}

impl Default for Flag {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Flag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Flag({})", self.is_set())
    }
}

// ---- contextvars: which call, which app turn, which Work session ----------------------------------------------

/// A Work session's tool call running now (telegram's `_SESSION`): `{"sid", "unanswered"}`.
#[derive(Debug, Clone, Default)]
pub struct WorkSession {
    pub sid: String,
    pub unanswered: bool,
}

/// Python's context variables, carried by a task and copied into every task it spawns (asyncio copies the
/// context at `ensure_future`): telegram's `_FROM_CALL`, `_FROM_APP`, `_APP_GRADE`, `_SESSION`,
/// `_TURN_MEMORY_CALLS` and `_TURN_SOURCES`. The shared cells (`Arc<Mutex<..>>`) are the lists Python mutates in
/// place, which a copied context shares.
#[derive(Clone)]
pub struct TurnCtx {
    /// `_FROM_CALL`: the reader of the call the work began on, or None.
    pub call: Option<Reader>,
    /// `_FROM_APP`: the Eve Link app turn's id, or None.
    pub app: Option<String>,
    /// `_APP_GRADE`: the speed dial's grade ("auto" by default).
    pub app_grade: String,
    /// `_SESSION`.
    pub session: Option<Arc<Mutex<WorkSession>>>,
    /// `_TURN_MEMORY_CALLS`: this turn's memory reads, `[0]` when the turn starts.
    pub memory_calls: Option<Arc<Mutex<i64>>>,
    /// `_TURN_SOURCES`: the memory lines this turn's reads found.
    pub sources: Option<Arc<Mutex<Vec<Obj>>>>,
}

impl Default for TurnCtx {
    fn default() -> Self {
        Self { call: None, app: None, app_grade: "auto".into(), session: None, memory_calls: None, sources: None }
    }
}

tokio::task_local! {
    static CTX: RefCell<TurnCtx>;
}

/// The running task's context (a copy), or the defaults outside any.
pub fn ctx() -> TurnCtx {
    CTX.try_with(|c| c.borrow().clone()).unwrap_or_default()
}

/// `var.set(...)` in the running task's own context → false outside any (then nothing is kept, as nothing
/// would read it). Spawned tasks and [`with_ctx`] scopes always have one.
pub fn set_ctx(f: impl FnOnce(&mut TurnCtx)) -> bool {
    CTX.try_with(|c| f(&mut c.borrow_mut())).is_ok()
}

/// `mark = var.set(...); try: f() finally: var.reset(mark)`: run `f` with `ctx` as the context.
pub fn with_ctx<R>(ctx: TurnCtx, f: impl FnOnce() -> R) -> R {
    CTX.sync_scope(RefCell::new(ctx), f)
}

/// Await `fut` with `ctx` as its context.
pub async fn in_ctx<F: Future>(ctx: TurnCtx, fut: F) -> F::Output {
    CTX.scope(RefCell::new(ctx), fut).await
}

/// `asyncio.ensure_future(coro)`: spawned with a copy of the running task's context.
pub fn spawn_ctx<F>(fut: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    tokio::spawn(CTX.scope(RefCell::new(ctx()), fut))
}

// ---- asyncio.Task --------------------------------------------------------------------------------------------

struct DoneOnDrop(Arc<Flag>);

impl Drop for DoneOnDrop {
    fn drop(&mut self) {
        self.0.set();
    }
}

/// A spawned task as Python holds one: kept in sets, compared by identity (`is asyncio.current_task()`),
/// cancelled, asked `done()`, and awaited by more than one waiter. Spawned with a copy of the context
/// ([`spawn_ctx`]).
#[derive(Clone)]
pub struct Job {
    abort: tokio::task::AbortHandle,
    done: Arc<Flag>,
    cancelled: Arc<AtomicBool>,
    name: Arc<str>,
}

impl Job {
    /// `task = asyncio.ensure_future(coro); task.set_name(name)`.
    pub fn spawn<F>(name: &str, fut: F) -> Job
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let done = Arc::new(Flag::new());
        let guard = DoneOnDrop(done.clone());
        let handle = spawn_ctx(async move {
            let _guard = guard;
            fut.await;
        });
        Job { abort: handle.abort_handle(), done, cancelled: Arc::new(AtomicBool::new(false)), name: name.into() }
    }

    /// `task.get_name()`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The task's id: what `is` compares.
    pub fn id(&self) -> tokio::task::Id {
        self.abort.id()
    }

    /// `task.done()`: finished, failed or cancelled.
    pub fn done(&self) -> bool {
        self.done.is_set() || self.abort.is_finished()
    }

    /// `task.cancel()`.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.abort.abort();
    }

    /// `task.cancelled()`: cancel was asked for.
    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// `task is asyncio.current_task()`.
    pub fn is_current(&self) -> bool {
        tokio::task::try_id() == Some(self.abort.id())
    }

    /// `await asyncio.gather(task, return_exceptions=True)`: until it is over, however it ends.
    pub async fn join(&self) {
        if self.abort.is_finished() {
            return;
        }
        self.done.wait().await;
    }
}

impl PartialEq for Job {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Job({}, done={})", self.name, self.done())
    }
}

// ---- the daemon's callables ------------------------------------------------------------------------------------

/// `responses.create`: a request body in, the response body out (computer_agent.make_response_creator).
pub type Create = eve_computer_agent::CreateResponse;
/// responses.create, streamed a sentence at a time (computer_agent.make_stream_creator).
pub type StreamCreate = eve_computer_agent::StreamResponse;
/// `ask_user(question) -> answer`, lent to a computer task.
pub type AskUser = eve_computer_agent::AskUser;
/// `on_event(event)`, lent to a computer task.
pub type OnEvent = eve_computer_agent::OnEvent;
/// `agent_factory(on_event, ask_user, floor=...)`: a computer task; `floor` names an explicit executor
/// ("codex"), None for the router's own pick.
pub type AgentFactory = Arc<dyn Fn(OnEvent, AskUser, Option<String>) -> Arc<dyn TaskAgent> + Send + Sync>;
/// A `() -> bool` (busy, muted, mac_busy, a keyboard's `valid`).
pub type BoolFn = Arc<dyn Fn() -> bool + Send + Sync>;
/// A `() -> str` (the opening brief, an owner context line).
pub type StrFn = Arc<dyn Fn() -> String + Send + Sync>;
/// `on_state(phase)`: the board's phase.
pub type OnState = Arc<dyn Fn(&str) + Send + Sync>;
/// `on_caption(message)`: a page on the robot's screen.
pub type OnCaption = Arc<dyn Fn(Value) + Send + Sync>;
/// `on_sound(on)`: mute / unmute.
pub type OnSound = Arc<dyn Fn(bool) + Send + Sync>;
/// `on_explore()`: "go explore" (daemon._request_explore); an error carries the refusal's reason.
pub type OnExplore = Arc<dyn Fn() -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;
/// `on_photo(note) -> {"ok", "path", "caption"}` (daemon._photo_for_owner).
pub type OnPhoto = Arc<dyn Fn(String) -> BoxFuture<'static, Obj> + Send + Sync>;
/// think.Thinker: `(question, context) -> {"ok", "answer"}`; an error is the thinker raising.
pub type Thinker = Arc<dyn Fn(String, String) -> BoxFuture<'static, anyhow::Result<Obj>> + Send + Sync>;
/// `screen() -> Path | None`: a JPEG of the screen (capture_screen).
pub type ScreenFn = Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>;
/// `notes() -> RoomNotes`: the room note-taker (daemon._room_notes_taker).
pub type NotesFn = Arc<dyn Fn() -> Arc<dyn RoomNotes> + Send + Sync>;
/// `later(f)`: run `f` once the turn is over (the inlet's `_after_turn.append`).
pub type Later = Arc<dyn Fn(Box<dyn FnOnce() + Send>) + Send + Sync>;
/// `phone_notes(action, call_reader, later=...) -> dict | None` (phone_call.PhoneCalls.notes_action).
pub type PhoneNotes = Arc<dyn Fn(String, Option<Reader>, Later) -> BoxFuture<'static, Option<Obj>> + Send + Sync>;
/// `terminal(cwd, text) -> str`: "" when the line went in, else the line to say.
pub type Terminal = Arc<dyn Fn(String, String) -> BoxFuture<'static, anyhow::Result<String>> + Send + Sync>;
/// `launcher(folder, harness) -> str` (claude_launch.open_session).
pub type Launcher = Arc<dyn Fn(PathBuf, String) -> BoxFuture<'static, anyhow::Result<String>> + Send + Sync>;
/// `launch_recent() -> [Recent]` (claude_launch.load_recent).
pub type LaunchRecent = Arc<dyn Fn() -> Vec<eve_claude_launch::Recent> + Send + Sync>;
/// `codex_folders() -> [Path]` (codex_chat.accessible_folders).
pub type CodexFolders = Arc<dyn Fn() -> Vec<PathBuf> + Send + Sync>;
/// `small_talk.ask(text, owner_voice) -> Answer`; an error is the lane raising.
pub type SmallTalkAsk =
    Arc<dyn Fn(String, String) -> BoxFuture<'static, anyhow::Result<eve_small_talk::Answer>> + Send + Sync>;
/// `sleep(secs)`.
pub type SleepFn = crate::prelude::Sleep;

/// What `claude_sessions()` returned: the running sessions' folders now, or an awaitable of them (the daemon's
/// probes ps and lsof off the loop).
pub enum Sessions {
    Now(Vec<String>),
    Later(BoxFuture<'static, anyhow::Result<Vec<String>>>),
}

/// `claude_sessions()`: the running Claude Code sessions' folders, newest first; an error is the call raising.
pub type ClaudeSessions = Arc<dyn Fn() -> anyhow::Result<Sessions> + Send + Sync>;

/// A keyword argument whose default is read from the environment at construction (telegram's `_FROM_ENV`):
/// `FromEnv` builds it as Python's default does, `Given` is what the caller lent (None included).
#[derive(Clone)]
pub enum Lent<T> {
    FromEnv,
    Given(Option<T>),
}

impl<T> Default for Lent<T> {
    fn default() -> Self {
        Lent::FromEnv
    }
}

// ---- a computer task ---------------------------------------------------------------------------------------------

/// A picture of the task's browser page (chrome_lane): its bytes, the file suffix and the caption's label.
#[derive(Debug, Clone, PartialEq)]
pub struct BrowserShot {
    pub data: Vec<u8>,
    pub suffix: String,
    pub label: String,
}

/// A computer task as the chat and the voice drive it (computer_agent.ComputerAgent, app_reflex's router, the
/// Codex executor, or a test's FakeAgent). Every default is what Python's `getattr(agent, name, None)` meant.
#[async_trait]
pub trait TaskAgent: Send + Sync {
    /// `await agent.run(goal, note=note)`: the task's closing sentence; an error is the run raising.
    async fn run(&self, goal: &str, note: &str) -> anyhow::Result<String>;
    /// `agent.cancel(reason=reason)`.
    fn cancel(&self, reason: &str);
    /// `agent.steer(text)` → whether a running task took it.
    fn steer(&self, text: &str) -> bool;
    /// `agent.status()`: what the task is up to (the voice's task_status).
    fn status(&self) -> Value {
        Value::Object(Obj::new())
    }
    /// `agent.provider` ("codex" for the Codex executor).
    fn provider(&self) -> String {
        String::new()
    }
    /// `agent.running`.
    fn running(&self) -> bool {
        false
    }
    /// `agent.browser_used`.
    fn browser_used(&self) -> bool {
        false
    }
    /// `await agent.browser_shot()`: the Chrome lane's page; None without one (or when it failed).
    async fn browser_shot(&self) -> Option<BrowserShot> {
        None
    }
    /// `agent.browser_screenshot`: (data, suffix) of this task's last verified browser picture.
    fn browser_screenshot(&self) -> Option<(Vec<u8>, String)> {
        None
    }
    /// `agent.browser_tab_id`.
    fn browser_tab_id(&self) -> String {
        String::new()
    }
}

// ---- the memory -----------------------------------------------------------------------------------------------

/// memory.Memory as the chat and the voice use it (the tests lend a FakeMemory around real transcripts).
pub trait BrainMemory: Send + Sync {
    /// `memory.transcripts`.
    fn transcripts(&self) -> Arc<eve_transcripts::Transcripts>;
    fn profile(&self) -> String;
    fn profile_for_voice(&self) -> String;
    /// `memory.talking_section()`, None when the memory has none (a test's stand-in).
    fn talking_section(&self) -> Option<String> {
        None
    }
    /// `memory.today(max_chars, exclude_conv=..., exclude_tail=..., exclude=..., style=...)`.
    fn today(&self, max_chars: i64, options: &eve_transcripts::Today) -> String;
    /// `memory.star(text)`: the starred line, or None when it could not be written.
    fn star(&self, text: &str) -> Option<String>;
    /// `memory.tools(voice=...)`.
    fn tools(&self, voice: bool) -> Vec<Value>;
    /// `memory.handle_tool(name, args, since=..., channel=...)`.
    fn handle_tool(&self, name: &str, args: &Value, since: Option<eve_transcripts::Dt>, channel: &str) -> Value;
    /// `getattr(memory, "retry", False) is True`.
    fn retry(&self) -> bool {
        false
    }
}

impl BrainMemory for eve_memory::Memory {
    fn transcripts(&self) -> Arc<eve_transcripts::Transcripts> {
        self.transcripts.clone()
    }
    fn profile(&self) -> String {
        eve_memory::Memory::profile(self)
    }
    fn profile_for_voice(&self) -> String {
        eve_memory::Memory::profile_for_voice(self)
    }
    fn talking_section(&self) -> Option<String> {
        Some(eve_memory::Memory::talking_section(self))
    }
    fn today(&self, max_chars: i64, options: &eve_transcripts::Today) -> String {
        eve_memory::Memory::today(self, max_chars, options)
    }
    fn star(&self, text: &str) -> Option<String> {
        eve_memory::Memory::star(self, text)
    }
    fn tools(&self, voice: bool) -> Vec<Value> {
        eve_memory::Memory::tools(self, voice)
    }
    fn handle_tool(&self, name: &str, args: &Value, since: Option<eve_transcripts::Dt>, channel: &str) -> Value {
        eve_memory::Memory::handle_tool(self, name, args, since, channel)
    }
    fn retry(&self) -> bool {
        self.retry
    }
}

// ---- eve's apps (app_registry.APPS): their hubs and modules ---------------------------------------------------

/// One app's code-path command (`hub.match(text)` / `module.match(text)`), kept as the app's own value; `action`
/// and `query` are what the taint gate reads off it (`getattr(cmd, "action", None)`).
#[derive(Clone)]
pub struct HubCommand {
    pub action: Option<String>,
    pub query: Option<String>,
    pub inner: Arc<dyn Any + Send + Sync>,
}

impl std::fmt::Debug for HubCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubCommand").field("action", &self.action).field("query", &self.query).finish()
    }
}

/// One of eve's apps' hubs (lights.Lights, spotify.Spotify, firetv.FireTV, obsbot_panel.ObsbotPanel, ...) as the
/// chat and the voice use it. The daemon wraps each app crate's hub in one; the tests lend fakes.
#[async_trait]
pub trait AppHub: Send + Sync {
    /// `hub.tools()`.
    fn tools(&self) -> Vec<Value>;
    /// `hub.instructions(**kwargs)` (the voice passes the app's `voice_kwargs`; the chat none).
    fn instructions(&self, kwargs: &Obj) -> String;
    /// `await hub.handle(name, args)`.
    async fn handle(&self, name: &str, args: &Value) -> Value;
    /// `hub.match(text)`: the hub's own code-path command (the lights match their names).
    fn match_command(&self, _text: &str) -> Option<HubCommand> {
        None
    }
    /// `await hub.run(cmd)`: the command done, and the line that says what the app did.
    async fn run(&self, _cmd: &HubCommand) -> String {
        String::new()
    }
    /// `await hub.screenshot_for_model()` (the TV's): `{"ok", "path", "shows"}`, None for an app without one.
    async fn screenshot_for_model(&self) -> Option<Value> {
        None
    }
}

/// An app's module as `app.mod()` is read at run time: its own `match` (one that needs no hub), its
/// `NOT_SET_UP_LINE` and `enabled()`. The registry's `ModuleInfo` (eve_app_registry) has its tool names and
/// off reason.
pub trait AppModule: Send + Sync {
    /// `getattr(mod, "match", None) is not None`.
    fn has_match(&self) -> bool {
        false
    }
    /// `mod.match(text)`.
    fn module_match(&self, _text: &str) -> Option<HubCommand> {
        None
    }
    /// `getattr(mod, "NOT_SET_UP_LINE", None)`.
    fn not_set_up_line(&self) -> Option<String> {
        None
    }
    /// `mod.enabled()`.
    fn enabled(&self) -> bool {
        true
    }
}

/// `**app_hubs`: eve's apps' hubs by slot, and their modules. Python set each on the object (`self._<slot>`);
/// a slot with no hub is an app that is off or not set up here.
#[derive(Clone, Default)]
pub struct AppHubs {
    hubs: HashMap<String, Arc<dyn AppHub>>,
    modules: HashMap<String, Arc<dyn AppModule>>,
}

impl AppHubs {
    /// `app_registry.check_hubs(app_hubs)` then the hubs kept: a slot no app has is refused.
    pub fn new(hubs: Vec<(String, Arc<dyn AppHub>)>) -> Result<Self, eve_app_registry::UnexpectedHubs> {
        eve_app_registry::check_hubs(hubs.iter().map(|(slot, _)| slot.as_str()))?;
        Ok(Self { hubs: hubs.into_iter().collect(), modules: HashMap::new() })
    }

    /// The same, with each app's module.
    pub fn with_modules(mut self, modules: Vec<(String, Arc<dyn AppModule>)>) -> Self {
        self.modules.extend(modules);
        self
    }

    /// `getattr(self, "_" + slot, None)`.
    pub fn get(&self, slot: &str) -> Option<Arc<dyn AppHub>> {
        self.hubs.get(slot).cloned()
    }

    /// `app.mod()`'s code-path side, None for a module that lent none.
    pub fn module(&self, slot: &str) -> Option<Arc<dyn AppModule>> {
        self.modules.get(slot).cloned()
    }

    /// `setattr(self, "_" + slot, hub)` (a test or the eval sets one).
    pub fn set(&mut self, slot: &str, hub: Option<Arc<dyn AppHub>>) {
        match hub {
            Some(hub) => {
                self.hubs.insert(slot.to_string(), hub);
            }
            None => {
                self.hubs.remove(slot);
            }
        }
    }

    /// The slots with a hub.
    pub fn slots(&self) -> Vec<String> {
        let mut slots: Vec<String> = self.hubs.keys().cloned().collect();
        slots.sort();
        slots
    }
}

// ---- the robot's eyes and the room's notes -------------------------------------------------------------------------

/// scene.SceneWatcher as the chat and the voice use it: `look`/`locate` (head.py's [`eve_head::Scene`]) and the
/// voice's `start` / `stop`.
#[async_trait]
pub trait SceneWatch: eve_head::Scene {
    /// `scene.start()`: watch quietly; the look tool reads it on request.
    fn start(&self) {}
    /// `await scene.stop()`.
    async fn stop(&self) {}
}

/// notes.RoomNotes as the chat's take_notes drives it.
#[async_trait]
pub trait RoomNotes: Send + Sync {
    /// `taker.start()`.
    fn start(&self) -> Value;
    /// `await taker.stop(reason)`.
    async fn stop(&self, reason: &str) -> Value;
    /// `taker.status()`.
    fn status(&self) -> Obj;
}

// ---- the voice's Live connection, speaker and gate -------------------------------------------------------------------

/// The Live API connection (`client.live.connect()`), as the session calls it. Events are the server's JSON
/// (Python's SDK objects, read with `_attr`).
#[async_trait]
pub trait LiveConnection: Send + Sync {
    /// `conn.session.start(session=cfg)`.
    async fn session_start(&self, session: Value) -> anyhow::Result<()>;
    /// `conn.session.update(session=...)`.
    async fn session_update(&self, session: Value) -> anyhow::Result<()>;
    /// `conn.session.commentary.append(content=..., delegation_id=...)`: speakable context.
    async fn commentary_append(&self, content: &str, delegation_id: Option<&str>) -> anyhow::Result<()>;
    /// `conn.session.thinking.append(content=..., delegation_id=...)`: silent context.
    async fn thinking_append(&self, content: &str, delegation_id: Option<&str>) -> anyhow::Result<()>;
    /// `conn.session.input_audio.append(audio=<base64>)`.
    async fn input_audio_append(&self, audio_b64: &str) -> anyhow::Result<()>;
    /// `conn.response.create()`.
    async fn response_create(&self) -> anyhow::Result<()>;
    /// `conn.response.item.create(item=...)`.
    async fn response_item_create(&self, item: Value) -> anyhow::Result<()>;
    /// The next event (`async for event in conn`); None once the connection is closed.
    async fn next_event(&self) -> Option<anyhow::Result<Value>>;
}

/// The voice's speaker (Speaker, NullSpeaker, or a test's fake): play/stop/busy.
pub trait VoiceSpeaker: Send + Sync {
    fn start(&self) -> bool;
    fn play(&self, pcm: &[u8]);
    fn stop(&self);
    fn busy(&self) -> bool;
    fn close(&self);
}

/// voice_gate.SpeakerGate: only the person who said the wake word reaches the model.
pub trait SpeakerGate: Send + Sync {
    /// `gate.bypass = ...`.
    fn set_bypass(&self, on: bool);
    /// `gate.lenient = ...`.
    fn set_lenient(&self, on: bool);
    /// `gate.process(raw)`: the pieces to send on.
    fn process(&self, raw: &[u8]) -> Vec<Vec<u8>>;
}

/// `intent(text) -> label | None` (intent.make_classifier).
pub type IntentFn = eve_intent::Classifier;
/// `head_pose(utterance) -> label or "none"` (typed_ask).
pub type HeadPoseFn = Arc<dyn Fn(String) -> BoxFuture<'static, anyhow::Result<String>> + Send + Sync>;
/// `learning(request) -> dict` (learning.run_lesson's voice).
pub type LearningFn =
    Arc<dyn Fn(&eve_learning::LessonRequest) -> Result<Obj, eve_learning::LearningError> + Send + Sync>;
/// `on_spoken_idea(lesson_id, text)` (LearningApp.append_spoken, blocking).
pub type SpokenIdeaFn = Arc<dyn Fn(&str, &str) -> anyhow::Result<()> + Send + Sync>;
/// `on_think_aloud(listening, lesson_id)`.
pub type ThinkAloudFn = Arc<dyn Fn(bool, Option<&str>) + Send + Sync>;
/// `on_expression(who, text)`.
pub type ExpressionFn = Arc<dyn Fn(&str, &str) + Send + Sync>;

// ---- the app builder's hosts and sends ------------------------------------------------------------------------------

/// `send(chat_id, text, [(label, url)])`: a message with web_app buttons (BotApi.send_web_apps).
pub type SendButtons =
    Arc<dyn Fn(i64, String, Vec<(String, String)>) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;
/// `send_photo(chat_id, jpeg, caption, [(label, url)])`.
pub type SendPhoto =
    Arc<dyn Fn(i64, Vec<u8>, String, Vec<(String, String)>) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;
/// `say(chat_id, text)`.
pub type SayFn = Arc<dyn Fn(i64, String) -> BoxFuture<'static, ()> + Send + Sync>;
/// `spawn(coro, name)`: a background job the chat keeps (TelegramInlet._spawn).
pub type SpawnFn = Arc<dyn Fn(BoxFuture<'static, ()>, &str) + Send + Sync>;

/// The live Mini App (miniapp.MiniApp) as the chat's app tools use it (apps_maker.ChatMaker): `.url`, `.maker`,
/// `.store`, `.app_url(slug)`, `.chat_url(slug)`, `.stick_link(rotate)`. The tests lend a FakeMiniApp.
#[async_trait]
pub trait MakerHost: Send + Sync {
    /// `app.url`: the tunnel's address, "" while it is down.
    fn url(&self) -> String;
    fn maker(&self) -> Arc<crate::apps_maker::AppMaker>;
    fn store(&self) -> Arc<crate::apps_maker::AppStore>;
    fn app_url(&self, slug: &str) -> String;
    fn chat_url(&self, slug: &str) -> String;
    async fn stick_link(&self, rotate: bool) -> String;
}

/// One of the Mini App server's cards beyond the panel (pika, the OBSBOT camera): `await card.close(...)` when
/// the server stops.
#[async_trait]
pub trait PanelCard: eve_link::seams::Hub {
    async fn close(&self) {}
}

// ---- HTTP: httpx with a MockTransport in the tests ---------------------------------------------------------------

/// One HTTP request, as httpx builds it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    /// in order, names as given
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// the request's timeout in seconds (None: the client's)
    pub timeout: Option<f64>,
}

impl HttpRequest {
    /// The first header named `name`, case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// One HTTP response.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// `response.json()`.
    pub fn json(&self) -> Result<Value, serde_json::Error> {
        serde_json::from_slice(&self.body)
    }

    /// `response.text`.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Why a request got no response (httpx.TransportError: a timeout, a refused connection, ...). `kind` is the
/// Python exception's class name where a log line or a test reads it ("ConnectTimeout", "ReadTimeout",
/// "ConnectError").
#[derive(Debug, Clone, thiserror::Error)]
#[error("{kind}: {message}")]
pub struct HttpError {
    pub kind: String,
    pub message: String,
}

/// An httpx client's transport: the real one is [`ReqwestTransport`]; a test lends a fake (httpx.MockTransport).
#[async_trait]
pub trait HttpTransport: Send + Sync {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError>;
}

/// The real transport, over reqwest.
#[derive(Clone, Default)]
pub struct ReqwestTransport {
    pub client: reqwest::Client,
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|e| HttpError { kind: "UnsupportedProtocol".into(), message: e.to_string() })?;
        let mut builder = self.client.request(method, &request.url);
        for (k, v) in &request.headers {
            builder = builder.header(k, v);
        }
        if let Some(secs) = request.timeout {
            if secs.is_finite() && secs > 0.0 {
                builder = builder.timeout(std::time::Duration::from_secs_f64(secs));
            }
        }
        let response = builder.body(request.body).send().await.map_err(|e| {
            let kind = if e.is_timeout() {
                if e.is_connect() { "ConnectTimeout" } else { "ReadTimeout" }
            } else if e.is_connect() {
                "ConnectError"
            } else {
                "TransportError"
            };
            HttpError { kind: kind.into(), message: e.to_string() }
        })?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
            .collect();
        let body = response
            .bytes()
            .await
            .map_err(|e| HttpError { kind: "ReadError".into(), message: e.to_string() })?
            .to_vec();
        Ok(HttpResponse { status, headers, body })
    }
}

/// A shared transport.
pub type Http = Arc<dyn HttpTransport>;

/// The real transport, as a shared one.
pub fn real_http() -> Http {
    Arc::new(ReqwestTransport::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pending_settles_once_and_runs_its_callbacks() {
        let p = Pending::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        p.add_done_callback(move || s.lock().push("first"));
        assert!(!p.done());
        let waiter = {
            let p = p.clone();
            tokio::spawn(async move { p.wait().await })
        };
        assert!(p.set_result("yes"));
        assert!(!p.set_result("no"));
        assert!(!p.cancel());
        assert_eq!(waiter.await.unwrap(), Ok("yes".to_string()));
        let s = seen.clone();
        p.add_done_callback(move || s.lock().push("late"));
        assert_eq!(*seen.lock(), vec!["first", "late"]);
        assert!(p.same(&p.clone()) && !p.same(&Pending::new()));
    }

    #[tokio::test]
    async fn a_cancelled_pending_raises_cancelled() {
        let p = Pending::new();
        assert!(p.cancel());
        assert!(p.cancelled());
        assert_eq!(p.wait().await, Err(CancelledError));
    }

    #[tokio::test]
    async fn the_context_is_copied_into_spawned_jobs() {
        let mut c = TurnCtx::default();
        c.app = Some("turn-1".into());
        let seen = in_ctx(c, async {
            let job_seen = Arc::new(Mutex::new(None));
            let js = job_seen.clone();
            let job = Job::spawn("probe", async move {
                *js.lock() = ctx().app;
                set_ctx(|c| c.app = Some("changed".into()));
            });
            job.join().await;
            assert!(job.done());
            let outer = ctx().app;
            let inner = job_seen.lock().clone();
            (inner, outer)
        })
        .await;
        assert_eq!(seen, (Some("turn-1".to_string()), Some("turn-1".to_string())));
        assert_eq!(ctx().app, None);
        assert_eq!(ctx().app_grade, "auto");
    }

    #[tokio::test]
    async fn a_cancelled_job_is_done() {
        let job = Job::spawn("forever", futures::future::pending::<()>());
        assert!(!job.done());
        job.cancel();
        job.join().await;
        assert!(job.done() && job.cancelled());
    }

    #[tokio::test]
    async fn a_flag_wakes_its_waiters() {
        let f = Arc::new(Flag::new());
        let g = f.clone();
        let waiter = tokio::spawn(async move { g.wait().await });
        f.set();
        waiter.await.unwrap();
        assert!(f.is_set());
        f.clear();
        assert!(!f.is_set());
    }
}

// ---- app_registry: eve's apps' modules, as `app.mod()` imports them -----------------------------------------------

/// Make eve's app modules known to the app registry (`app_registry.App.mod()`: Python imports
/// `eve_bridge.<module>` on first use; Rust has no runtime import, so the brain registers the modules it is built
/// with). Each one's TOOLS, TOOL_NAMES, INSTRUCTIONS (the lights' INSTRUCTIONS_HEAD), OFF_REASON and module-level
/// callables, read from its own crate. A module already registered (the daemon's fuller description) is kept.
/// Runs once per process; the inlet and the voice session call it as they are made.
pub fn register_app_modules() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        use eve_app_registry::{ModuleInfo, ModuleSource, Registered};
        let names = |n: &[&str]| -> Option<Vec<String>> { Some(n.iter().map(|s| s.to_string()).collect()) };
        let funcs = |n: &[&str]| -> std::collections::BTreeSet<String> { n.iter().map(|s| s.to_string()).collect() };
        let modules = [
            ModuleInfo {
                functions: funcs(&["enabled", "make_lights"]),
                tool_names: names(&eve_lights::lights::TOOL_NAMES),
                tools: Some(eve_lights::lights::TOOLS.clone()),
                off_reason: Some(eve_lights::lights::OFF_REASON.to_string()),
                instructions_head: Some(eve_lights::lights::INSTRUCTIONS_HEAD.to_string()),
                ..ModuleInfo::new("lights")
            },
            ModuleInfo {
                functions: funcs(&["enabled", "make_spotify", "match"]),
                tool_names: names(&eve_spotify::TOOL_NAMES),
                tools: Some(Value::Array(eve_spotify::TOOLS.clone())),
                off_reason: Some(eve_spotify::OFF_REASON.to_string()),
                instructions: Some(eve_spotify::INSTRUCTIONS.to_string()),
                ..ModuleInfo::new("spotify")
            },
            ModuleInfo {
                functions: funcs(&["enabled", "make_firetv", "match"]),
                tool_names: names(&eve_firetv::TOOL_NAMES),
                tools: Some(Value::Array(eve_firetv::TOOLS.clone())),
                off_reason: Some(eve_firetv::OFF_REASON.to_string()),
                instructions: Some(eve_firetv::INSTRUCTIONS.to_string()),
                ..ModuleInfo::new("firetv")
            },
            ModuleInfo {
                functions: funcs(&["enabled", "make_panel", "match"]),
                tool_names: names(&eve_obsbot_panel::TOOL_NAMES),
                tools: Some(Value::Array(eve_obsbot_panel::TOOLS.clone())),
                off_reason: Some(eve_obsbot_panel::OFF_REASON.to_string()),
                instructions: Some(eve_obsbot_panel::INSTRUCTIONS.to_string()),
                ..ModuleInfo::new("obsbot_panel")
            },
        ];
        for info in modules {
            if Registered.module(&info.name).is_none() {
                eve_app_registry::register_module(info);
            }
        }
    });
}

// ---- codex_chat.CodexChat, as the Telegram inlet drives it -------------------------------------------------------

/// The Codex chat as the inlet uses it (`self._codex`): Python duck-types it, and its tests lend a FakeCodex. The
/// real one is [`eve_codex_chat::CodexChat`].
#[async_trait]
pub trait CodexDoor: Send + Sync {
    /// `codex.connected`.
    fn connected(&self) -> bool;
    /// `codex.running`: a turn is going, so a message sent now steers it.
    fn running(&self) -> bool;
    /// `await codex.start(folder, emit, picture=..., done=...)`.
    async fn start(
        &self,
        folder: &std::path::Path,
        emit: eve_codex_chat::Emit,
        picture: Option<eve_codex_chat::Picture>,
        done: Option<eve_codex_chat::Emit>,
    ) -> Result<(), eve_codex_computer::CodexError>;
    /// `await codex.send(text)`.
    async fn send(&self, text: &str) -> Result<(), eve_codex_computer::CodexError>;
    /// `await codex.interrupt()`.
    async fn interrupt(&self) -> Result<(), eve_codex_computer::CodexError>;
    /// `await codex.close()`.
    async fn close(&self);
}

#[async_trait]
impl CodexDoor for eve_codex_chat::CodexChat {
    fn connected(&self) -> bool {
        eve_codex_chat::CodexChat::connected(self)
    }
    fn running(&self) -> bool {
        (**self).running()
    }
    async fn start(
        &self,
        folder: &std::path::Path,
        emit: eve_codex_chat::Emit,
        picture: Option<eve_codex_chat::Picture>,
        done: Option<eve_codex_chat::Emit>,
    ) -> Result<(), eve_codex_computer::CodexError> {
        eve_codex_chat::CodexChat::start(self, folder, emit, picture, done).await
    }
    async fn send(&self, text: &str) -> Result<(), eve_codex_computer::CodexError> {
        eve_codex_chat::CodexChat::send(self, text).await
    }
    async fn interrupt(&self) -> Result<(), eve_codex_computer::CodexError> {
        eve_codex_chat::CodexChat::interrupt(self).await
    }
    async fn close(&self) {
        eve_codex_chat::CodexChat::close(self).await
    }
}

// ---- agentcard: the purchase card and its approvals (agentcard_tools.AgentCardTools) ------------------------------

/// The owner question a purchase confirmation asks: `(question, about) -> answer`.
pub type PurchaseAsk = Box<
    dyn FnOnce(String, String) -> BoxFuture<'static, Result<String, eve_agentcard_tools::AskError>> + Send,
>;

/// The `agentcard` lent to the inlet (`agentcard.AgentCard`, duck-typed: Python's tests lend a Service of their
/// own) as the inlet reads it (`tools()`, `instructions()`), with the approvals the inlet wraps around it
/// (`self._purchases = agentcard_tools.AgentCardTools(agentcard)`).
#[async_trait]
pub trait Purchases: Send + Sync {
    /// `agentcard.tools()`.
    fn tools(&self) -> Vec<Value>;
    /// `agentcard.instructions()`.
    fn instructions(&self) -> String;
    /// `purchases.active(chat_id)`.
    fn active(&self, chat_id: Option<i64>) -> bool;
    /// `purchases.stop(chat_id)`.
    fn stop(&self, chat_id: Option<i64>) -> eve_agentcard_tools::StopReport;
    /// `purchases.shutdown()`.
    fn shutdown(&self) -> eve_agentcard_tools::StopReport;
    /// `await purchases.handle(name, args, chat_id=..., ask_user=...)`.
    async fn handle(&self, name: &str, args: &Value, chat_id: Option<i64>, ask: PurchaseAsk) -> Value;
}

/// A purchase card's own surface beside the client calls (`tools()`, `instructions()`).
pub trait CardService: eve_agentcard_tools::Service {
    fn tools(&self) -> Vec<Value>;
    fn instructions(&self) -> String;
}

impl CardService for eve_agentcard::AgentCard {
    fn tools(&self) -> Vec<Value> {
        eve_agentcard::AgentCard::tools(self)
    }
    fn instructions(&self) -> String {
        eve_agentcard::AgentCard::instructions().to_string()
    }
}

/// [`Purchases`] over a card: `AgentCardTools(agentcard)`.
pub struct CardPurchases<S: CardService> {
    tools: eve_agentcard_tools::AgentCardTools<S>,
}

impl<S: CardService> CardPurchases<S> {
    pub fn new(service: Arc<S>) -> Arc<Self> {
        Arc::new(Self { tools: eve_agentcard_tools::AgentCardTools::new(service) })
    }

    /// The approvals themselves (`inlet._purchases`).
    pub fn approvals(&self) -> &eve_agentcard_tools::AgentCardTools<S> {
        &self.tools
    }
}

#[async_trait]
impl<S: CardService> Purchases for CardPurchases<S> {
    fn tools(&self) -> Vec<Value> {
        self.tools.service().tools()
    }
    fn instructions(&self) -> String {
        self.tools.service().instructions()
    }
    fn active(&self, chat_id: Option<i64>) -> bool {
        self.tools.active(chat_id)
    }
    fn stop(&self, chat_id: Option<i64>) -> eve_agentcard_tools::StopReport {
        self.tools.stop(chat_id)
    }
    fn shutdown(&self) -> eve_agentcard_tools::StopReport {
        self.tools.shutdown()
    }
    async fn handle(&self, name: &str, args: &Value, chat_id: Option<i64>, ask: PurchaseAsk) -> Value {
        self.tools.handle(name, args, chat_id, ask).await
    }
}
