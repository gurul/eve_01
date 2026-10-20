//! A port of `telegram.py`, lines 2448-2883: the inlet's types and state, its constructor.
//!
//! Skeleton: constants and prompts are verbatim (generated from the Python values); bodies are
//! `todo!()` until this file's unit fills them.

pub mod call;
pub mod chief;
pub mod handle;
pub mod history;
pub mod media;
pub mod parts;
pub mod relay;
pub mod run;
pub mod say;
pub mod tasks;
pub mod tools;
pub mod turn;
pub use call::*;
pub use chief::*;
pub use handle::*;
pub use history::*;
pub use media::*;
pub use parts::*;
pub use relay::*;
pub use run::*;
pub use say::*;
pub use tasks::*;
pub use tools::*;
pub use turn::*;

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

/// py: telegram.py:2450 `def _same_folder(a: str, b: str) -> bool:`
pub(crate) fn same_folder(a: &str, b: &str) -> bool {
    todo!("telegram.py:2450 _same_folder")
}

/// py: telegram.py:2457 `def _folder_label(entry: str) -> str:`
///
/// A Codex folder menu entry as a button label: a bare name as it is, a full path (two folders share
/// the name) with ~ for the home folder and, when still too long, its end kept, where the names differ.
pub(crate) fn folder_label(entry: &str) -> String {
    todo!("telegram.py:2457 _folder_label")
}

// py: telegram.py:2467 `Create` (_CallableGenericAlias): `seams::Create`.

/// py: telegram.py:2476 `ANSWER`
pub const ANSWER: &str = "answer";

/// py: telegram.py:2476 `TYPE`
pub const TYPE: &str = "type";

/// py: telegram.py:2476 `SAY`
pub const SAY: &str = "say";

/// py: telegram.py:2476 `STOP`
pub const STOP: &str = "stop";

/// py: telegram.py:2480 `class _Keyboard:`
///
/// One message's inline buttons, while they mean something. Every keyboard is one-shot: the first tap
/// retires all its keys, so a second tap on the same message is "already answered" or "expired".
///
/// Shared (`Arc<_Keyboard>`) between the taps table and the slot that shows it; what Python reassigns after
/// it is made (its keys, its message id) is behind a lock. Two handles are the same keyboard by `Arc::ptr_eq`.
pub struct _Keyboard {
    pub chat_id: Option<i64>,
    pub kind: String,
    pub choices: Vec<Choice>,
    /// ANSWER: the question it answers
    pub future: Option<Pending>,
    /// still means something (a picker's `valid`); None is always
    pub valid: Option<BoolFn>,
    /// STOP: `on_stop(message_id)`
    pub on_stop: Option<OnStop>,
    pub keys: Mutex<Vec<String>>,
    /// set once the message is sent
    pub message_id: Mutex<i64>,
}

/// `on_stop(message_id)`: a Stop button tapped (a progress message's).
pub type OnStop = Arc<dyn Fn(i64) + Send + Sync>;

/// py: telegram.py:2494 `class _Question:`
///
/// One question waiting on the owner (``_ask_user``, ``decide_permission``): its future and what answers
/// it. ``chat_id`` None is any owner chat, as a permission prompt has always been answered.
///
/// Shared as `Arc<_Question>`: the questions list, America's slots and a call's answer context hold the same
/// one, compared by `Arc::ptr_eq` (Python's `is`).
pub struct _Question {
    pub future: Pending,
    pub chat_id: Option<i64>,
    pub strict: bool,
    pub words: HashSet<String>,
    /// the question as asked: what a spoken answer is judged against
    pub text: Mutex<String>,
    pub firm: bool,
}

/// py: telegram.py:2508 `class _Progress:`
///
/// One running piece of work's progress message: a computer task started from the chat, or one Codex
/// relay turn. ``head`` is what it says before any step ("On it…", "Sent to Codex."); ``steps`` are the
/// latest few; ``unsent`` are the steps no edit has shown yet, which go as a new message if editing fails.
/// ``request_id`` is the owner's message that asked for the work: the result replies to it. Steps are
/// whole; ``text`` shows them all, and ``_progress_step`` scrolls the oldest off near the message limit.
///
/// Shared as `Arc<_Progress>`; its mutable fields are [`ProgressState`] behind `st` (never held across an
/// await), and `lock` orders the first send, the edits and the close.
pub struct _Progress {
    pub chat_id: i64,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub request_id: i64,
    pub st: Mutex<ProgressState>,
    /// the first send, edits and the close, in order
    pub lock: tokio::sync::Mutex<()>,
}

/// `_Progress`'s fields Python reassigns.
#[derive(Default)]
pub struct ProgressState {
    pub head: String,
    pub steps: Vec<String>,
    pub unsent: Vec<String>,
    pub message_id: i64,
    /// the text last put on the phone
    pub shown: String,
    pub edited_at: f64,
    /// edits failed: steps go as new messages
    pub broken: bool,
    pub closed: bool,
    /// a stop was asked for: it closes as "Stopped."
    pub stopped: bool,
    /// the Stop button
    pub board: Option<Arc<_Keyboard>>,
    pub flush: Option<Job>,
    /// the flush holds steps it took: never cancel it
    pub pushing: bool,
    /// the first unsent steps a send or edit in flight shows
    pub taking: i64,
}

impl _Progress {
    /// py: telegram.py:2533 `def text(self) -> str:`
    pub fn text(&self) -> String {
        todo!("telegram.py:2533 _Progress.text")
    }
}

/// `TelegramInlet(...)`'s keyword arguments. `Default` is Python's defaults: a `None` callable with a Python
/// default (`screen`, `launcher`, `launch_recent`, `codex_folders`, `busy`, `brief`, `on_state`,
/// `claude_sessions`, `clock`, `wall`, `sleep`) is that default; `Lent::FromEnv` is `_FROM_ENV`.
pub struct InletOptions {
    pub stream_create: Option<StreamCreate>,
    pub agent_factory: Option<AgentFactory>,
    pub agent_enabled: bool,
    pub busy: Option<BoolFn>,
    pub brief: Option<StrFn>,
    pub on_photo: Option<OnPhoto>,
    pub thinker: Option<Thinker>,
    pub on_state: Option<OnState>,
    pub memory: Option<Arc<dyn BrainMemory>>,
    pub apps: Option<Arc<eve_composio_tools::ComposioBridge>>,
    pub direct: Option<Arc<eve_apps_mcp::AppsBridge>>,
    pub vault: Option<Arc<eve_second_brain::VaultConfig>>,
    pub chat_log: Option<Arc<eve_chat_log::ChatLog>>,
    pub watcher: Option<Arc<eve_web::watch::Watcher>>,
    pub chief: Option<Arc<eve_chief::Chief>>,
    pub meeter: Option<Arc<eve_meet::Meeter>>,
    pub canvas: Option<Arc<eve_canvas::Canvas>>,
    pub agentcard: Option<Arc<eve_agentcard::AgentCard>>,
    pub america: Option<Arc<eve_america::America>>,
    pub opentable_enabled: bool,
    pub share: Option<Arc<crate::share::Share>>,
    pub toolset_mode: Option<String>,
    pub toolset_route: Lent<eve_toolset_router::Router>,
    pub grader: Lent<eve_grade_router::Grader>,
    pub small_talk_ask: Lent<SmallTalkAsk>,
    pub screen: Option<ScreenFn>,
    pub scene: Option<Arc<dyn SceneWatch>>,
    pub head: Option<Arc<eve_head::Head>>,
    pub on_explore: Option<OnExplore>,
    pub on_sound: Option<OnSound>,
    pub on_caption: Option<OnCaption>,
    pub notes: Option<NotesFn>,
    pub terminal: Option<Terminal>,
    pub launcher: Option<Launcher>,
    pub launch_root: Option<PathBuf>,
    pub launch_recent: Option<LaunchRecent>,
    pub claude_sessions: Option<ClaudeSessions>,
    pub codex: Option<Arc<eve_codex_chat::CodexChat>>,
    pub codex_folders: Option<CodexFolders>,
    pub permission_timeout_secs: f64,
    pub clock: Option<Clock>,
    pub wall: Option<Clock>,
    pub sleep: Option<Sleep>,
    /// `**app_hubs`
    pub app_hubs: AppHubs,
    /// The environment `__init__` reads (toolset_router.mode, small_talk.enabled, composio_tools.toolkit_policy,
    /// the routers' makers); None is the process's.
    pub environ: Option<Environ>,
}

impl Default for InletOptions {
    fn default() -> Self {
        Self {
            stream_create: None,
            agent_factory: None,
            agent_enabled: true,
            busy: None,
            brief: None,
            on_photo: None,
            thinker: None,
            on_state: None,
            memory: None,
            apps: None,
            direct: None,
            vault: None,
            chat_log: None,
            watcher: None,
            chief: None,
            meeter: None,
            canvas: None,
            agentcard: None,
            america: None,
            opentable_enabled: false,
            share: None,
            toolset_mode: None,
            toolset_route: Lent::FromEnv,
            grader: Lent::FromEnv,
            small_talk_ask: Lent::FromEnv,
            screen: None,
            scene: None,
            head: None,
            on_explore: None,
            on_sound: None,
            on_caption: None,
            notes: None,
            terminal: None,
            launcher: None,
            launch_root: None,
            launch_recent: None,
            claude_sessions: None,
            codex: None,
            codex_folders: None,
            permission_timeout_secs: DEFAULT_PERMISSION_TIMEOUT_SECS,
            clock: None,
            wall: None,
            sleep: None,
            app_hubs: AppHubs::default(),
            environ: None,
        }
    }
}

/// py: telegram.py:2537 `class TelegramInlet:`
///
/// Polls, decides who may speak, runs one text turn at a time, and owns at most one computer task.
///
/// Everything it needs from the daemon is lent as a callable, as the voice session's are, so the tests
/// drive it with fakes and no network:
///
/// * ``create``        — responses.create, dict in, dict out (computer_agent.make_response_creator)
/// * ``agent_factory`` — (on_event, ask_user) -> ComputerAgent, the one the voice uses
/// * ``busy``          — True while the desk has the Mac: a spoken conversation or its task
/// * ``brief``         — eve's opening brief (recall.opening_brief), read per turn; it goes in the turn's
///                       developer note, worded for a chat
/// * ``on_photo``      — note -> {"ok", "path", "caption"} (daemon._photo_for_owner)
/// * ``thinker``       — question -> {"ok", "answer"} (think.make_thinker), or None
/// * ``on_state``      — the board's phase, for a texted task
/// * ``memory``        — memory.Memory, or None (memory off): every turn is written to its transcript as
///                       it happens, the profile and today's transcript are in the prompt, its tools are
///                       offered and "remember that" stars through it. None: no capture, no memory tools,
///                       and the prompt a memory-less chat has always had
/// * ``apps``          — composio_tools.ComposioBridge: the owner's apps by API (Gmail read only, the
///                       calendar writable, everything else asked first), or None. Since 2026-10-04 it holds
///                       Slack, Notion, Linear and the long tail; the rest is ``direct``
/// * ``direct``        — apps_mcp.AppsBridge: Gmail, Calendar, Drive (Google's APIs), GitHub and Maps (their MCP
///                       servers), opened per turn (open_app), or None
/// * ``vault``         — second_brain.VaultConfig: the owner's own notes, todos and journals as a local
///                       markdown vault (PARA+), captured from this chat and read back, or None
/// * ``watcher``       — watch.Watcher: prices, quotes and ticket releases watched on a schedule; its tools are
///                       offered, /watches lists them, and its alerts reach this chat through ``tell_owner``
/// * ``chief``         — chief.Chief, or None (EVE_CHIEF off: then nothing below changes a byte of any
///                       request, reply or tool list). On: take_on, jobs_list and close_card ride the watcher's slot, the
///                       card line rides the turn's note, /jobs and "<word> c<N>" are code words, and its Mac
///                       steps run through ``_start_task`` (``chief_start``), their result going to the chief
///                       as a receipt in place of "Task result" (design 3.2, 2026-09-29). The daemon lends it
///                       after the inlet exists, because it is built from the inlet's own sends and asks
/// * ``screen``        — () -> Path | None: a JPEG of the screen (capture_screen); tests hand in a fake
/// * ``scene``, ``head`` — the daemon's SceneWatcher and Head, for look / look_around / find / move_head
/// * ``on_explore``    — () -> None: "go explore" (daemon._request_explore)
/// * ``on_sound``      — (bool) -> None: mute / unmute (daemon._set_sound)
/// * ``on_caption``    — (dict) -> None: a page on the robot's screen (daemon._on_caption)
/// * ``notes``         — () -> RoomNotes: the room note-taker (daemon._room_notes_taker), for take_notes
/// * ``_phone_notes``  — (action, call reader) -> awaitable dict | None: notes on the phone
///                       (phone_call.PhoneCalls.notes_action), lent by the daemon with the Mini App; take_notes
///                       tries it first, and None means the room note-taker answers
/// * ``terminal``      — (cwd, text) -> str: type a line into the Claude Code terminal for that session;
///                       "" when it went in (the owner's message gets a reaction), else the line to say
/// * ``claude_sessions`` — () -> [cwd], or an awaitable of it: the running Claude Code sessions' folders,
///                       newest first, for the "claude on" picker (the daemon's leaves out sessions whose
///                       process is gone, probing ps and lsof on a worker thread: claude_live.picker_sessions_off_loop)
/// * ``launcher``      — (folder, harness) -> str: open a new coding session on the Mac
///                       (claude_launch.open_session). "new claude" (code word) and the start_coding_session
///                       tool walk claude_launch's tree: personal or work, then general or which folder;
///                       ``launch_root`` and ``launch_recent`` feed it
///
/// "claude on" / "claude off" is the terminal relay, explicit only (owner, 2026-09-21): while on, the chat
/// is the terminal. What Claude Code says (``relay_text``), asks (an AskUserQuestion call, shown as a
/// question by ``relay_tool_call``) and waits on (``relay_notification``) is forwarded here, and plain
/// text is typed into its terminal; "eve: <text>" is for eve. Only what the terminal shows in white
/// travels: no thinking, no tool calls, no result tails (owner, 2026-09-21, "the gray stuff"). The relay is bypass: the daemon allows a tool call without asking (daemon.py), and only
/// the owner's always_ask commands (rm, sudo) become a yes/no here (``decide_permission``, with Allow and
/// Deny buttons; silence defers to Claude Code's own flow, never denies).
///
/// Inline buttons (owner, 2026-09-23): yes/no prompts, a relayed question's options and the pickers ("claude
/// on", "new claude", the Codex folders) are buttons under the message. A tap arrives as a callback_query
/// (``_on_tap``), is always answered, and does exactly what typing its words would. Typing still works.
///
/// Progress (owner, 2026-09-23): a computer task started here, and each Codex relay turn, has one progress
/// message with a Stop button (``_open_progress``). Steps edit it in place, paced and never a no-op; the
/// result is a new message replying to the owner's request. Any refusal falls back to plain messages.
///
/// The robot shows what the chat is doing — the phase on its face, a caption for each task step and the
/// result — unless the owner has said "stealth mode": then it acts asleep (idle, no captions, no head)
/// until "wake up". Stealth is a code word, never a model call.
///
/// Rust: built by [`TelegramInlet::new`] as `Arc<Self>` (`Arc::new_cyclic`, so the Codex chat's `ask_user` and the
/// events' limiter reach the inlet through a `Weak`). What the daemon lends after the inlet exists (`chief`,
/// `_maker`, `_phone_notes`, `app_out`, `link_uploads`, a hub) is behind a lock; everything Python reassigns is in
/// `st`, a `parking_lot::Mutex` never held across an await.
pub struct TelegramInlet {
    pub config: TelegramConfig,
    pub api: Arc<dyn TelegramApi>,
    pub(crate) create: Create,
    pub(crate) agent_factory: Option<AgentFactory>,
    pub(crate) agent_enabled: bool,
    pub(crate) busy: BoolFn,
    pub(crate) brief: StrFn,
    pub(crate) memory: Option<Arc<dyn BrainMemory>>,
    pub(crate) on_photo: Option<OnPhoto>,
    pub(crate) thinker: Option<Thinker>,
    pub(crate) on_state: OnState,
    /// composio_tools.ComposioBridge, or None
    pub(crate) apps: Option<Arc<eve_composio_tools::ComposioBridge>>,
    /// apps_mcp.AppsBridge, or None
    pub(crate) direct: Option<Arc<eve_apps_mcp::AppsBridge>>,
    pub(crate) app_policy: eve_composio_tools::Policy,
    /// second_brain.VaultConfig (enabled), or None
    pub(crate) vault: Option<Arc<eve_second_brain::VaultConfig>>,
    /// watch.Watcher, or None (EVE_WATCH=0)
    pub(crate) watcher: Option<Arc<eve_web::watch::Watcher>>,
    /// meet.Meeter, or None (EVE_MEET=0, no Chrome attach)
    pub(crate) meeter: Option<Arc<eve_meet::Meeter>>,
    /// canvas.Canvas, or None (no CANVAS_BASE_URL/CANVAS_API_TOKEN)
    pub(crate) canvas: Option<Arc<eve_canvas::Canvas>>,
    pub(crate) agentcard: Option<Arc<eve_agentcard::AgentCard>>,
    pub(crate) purchases: Option<eve_agentcard_tools::AgentCardTools<eve_agentcard::AgentCard>>,
    /// optional U.S. legal/government sources
    pub(crate) america: Option<Arc<eve_america::America>>,
    pub(crate) opentable_enabled: bool,
    /// share.Share, or None (EVE_SHARE off)
    pub(crate) share: Option<Arc<crate::share::Share>>,
    pub(crate) toolset_mode: String,
    pub(crate) toolset_route: Option<eve_toolset_router::Router>,
    pub(crate) grader: Option<eve_grade_router::Grader>,
    pub(crate) small_talk_ask: Option<SmallTalkAsk>,
    pub(crate) events: eve_events::Events,
    /// the time, the weather, a sum: by code
    pub(crate) quick: eve_quick_answers::QuickAnswers,
    /// responses streamed a sentence at a time, on a call
    pub(crate) stream_create: Option<StreamCreate>,
    pub(crate) screen: ScreenFn,
    pub(crate) scene: Option<Arc<dyn SceneWatch>>,
    pub(crate) head: Option<Arc<eve_head::Head>>,
    pub(crate) on_explore: Option<OnExplore>,
    pub(crate) on_sound: Option<OnSound>,
    pub(crate) on_caption: Option<OnCaption>,
    pub(crate) notes: Option<NotesFn>,
    pub(crate) codex: Arc<eve_codex_chat::CodexChat>,
    pub(crate) codex_folders: CodexFolders,
    pub(crate) codex_lock: tokio::sync::Mutex<()>,
    pub(crate) terminal: Option<Terminal>,
    pub(crate) launcher: Launcher,
    pub(crate) launch_root: Option<PathBuf>,
    pub(crate) launch_recent: LaunchRecent,
    pub(crate) claude_sessions: ClaudeSessions,
    pub(crate) permission_timeout: f64,
    pub(crate) clock: Clock,
    pub(crate) wall: Clock,
    pub(crate) sleep: Sleep,
    pub(crate) turn_lock: tokio::sync::Mutex<()>,
    pub(crate) tap_gen: String,
    pub app_reactions: Arc<eve_link_reactions::Reactions>,
    /// the inlet's own `Arc` (set by `Arc::new_cyclic`), for trait methods that take `&self` and spawn
    pub(crate) me: std::sync::Weak<TelegramInlet>,
    /// the environment lent at construction (None: the process's)
    pub(crate) environ: Option<Environ>,
    /// chief.Chief, or None (EVE_CHIEF off); the daemon lends it after the inlet exists
    pub chief: Mutex<Option<Arc<eve_chief::Chief>>>,
    /// eve's apps' hubs (`self._<slot>`), read on every call (`_live_apps`)
    pub hubs: Mutex<AppHubs>,
    /// apps_maker.ChatMaker once the Mini App runs, or None
    pub maker: Mutex<Option<Arc<crate::apps_maker::ChatMaker>>>,
    /// notes on the phone (phone_call.PhoneCalls.notes_action), lent with the Mini App
    pub phone_notes: Mutex<Option<PhoneNotes>>,
    /// Eve Link's chat: where an app turn's lines go
    pub app_out: Mutex<Option<AppOut>>,
    /// the app's upload store (link_uploads.UploadStore)
    pub link_uploads: Mutex<Option<Arc<eve_link_uploads::UploadStore>>>,
    /// What Python reassigns after `__init__`, behind one lock.
    pub(crate) st: Mutex<TelegramInletState>,
}

/// `TelegramInlet`'s mutable state (the attributes Python reassigns or mutates after `__init__`).
#[derive(Default)]
pub(crate) struct TelegramInletState {
    pub(crate) chat_log: Option<Arc<eve_chat_log::ChatLog>>,
    /// the log line the shown history starts at (_log_window)
    pub(crate) window_seq: i64,
    /// the log's generation it was computed for (forget moves it); -1 at first
    pub(crate) window_gen: i64,
    /// chats this run has seeded the log for (_seed_log)
    pub(crate) seeded_chats: HashSet<i64>,
    pub(crate) recall_seen: Vec<Value>,
    pub(crate) memory_tools_cache: Option<Vec<Value>>,
    /// the direct apps whose tools this turn has (open_app)
    pub(crate) direct_open: HashSet<String>,
    pub(crate) after_turn: Vec<Box<dyn FnOnce() + Send>>,
    /// the owner's words in the running turn: take_on's door
    pub(crate) turn_said: String,
    pub(crate) chief_task: Option<(String, i64)>,
    pub(crate) america_closed: bool,
    pub(crate) america_jobs: HashMap<i64, Job>,
    pub(crate) america_phases: HashMap<i64, String>,
    pub(crate) america_questions: HashMap<i64, Arc<_Question>>,
    pub(crate) opentable_agent: Option<Arc<dyn TaskAgent>>,
    /// this turn's open Share / workspace toolsets
    pub(crate) lent: HashSet<String>,
    /// the running turn is a rundown (_think)
    pub(crate) daily: bool,
    pub(crate) taint: String,
    pub(crate) routed: bool,
    /// a phone call's reader (phone_call.Call.say)
    pub(crate) call_say: Option<Reader>,
    pub(crate) streamed: Option<String>,
    pub(crate) codex_chat: Option<i64>,
    pub(crate) codex_title: String,
    pub(crate) codex_epoch: i64,
    pub(crate) claude_epoch: i64,
    pub(crate) launch: Option<eve_claude_launch::LaunchFlow>,
    /// "claude on" with nothing running: join what opens
    pub(crate) join_after_launch: bool,
    pub(crate) claude_on_job: Option<Job>,
    pub(crate) held: Vec<(Inbound, Option<Reader>)>,
    /// at most RELAY_PROMPTS_MAX, oldest dropped
    pub(crate) relay_prompts: VecDeque<(String, Option<Reader>)>,
    pub(crate) relay_origin: Option<Reader>,
    pub(crate) stealth: bool,
    /// the terminal relay
    pub(crate) claude: bool,
    pub(crate) muse: bool,
    /// a private chat's id is the user's
    pub(crate) chat_id: Option<i64>,
    /// the session Claude last spoke from
    pub(crate) relay_cwd: String,
    /// the session picked at "claude on"; "" follows the last
    pub(crate) relay_pin: String,
    pub(crate) relay_ready: Option<Arc<Flag>>,
    pub(crate) relay_lines: Vec<(String, String, String, Vec<Choice>, Option<Reader>)>,
    pub(crate) relay_flush: Option<Job>,
    /// a question or yes/no just went to the phone (-inf at first)
    pub(crate) last_ask_at: f64,
    pub(crate) turns: Vec<(String, String)>,
    pub(crate) turn_kinds: Vec<String>,
    pub(crate) conv: Option<String>,
    pub(crate) last_turn_at: Option<f64>,
    pub(crate) open_turns: HashMap<i64, (i64, i64)>,
    pub(crate) app_jobs: Vec<Job>,
    /// the card each app turn's take_on filed, by turn id, oldest first
    pub(crate) app_filed: indexmap::IndexMap<String, String>,
    pub(crate) agent: Option<Arc<dyn TaskAgent>>,
    pub(crate) agent_task: Option<Job>,
    pub(crate) task_chat: Option<i64>,
    pub(crate) task_goal: String,
    pub(crate) task_progress: Option<Arc<_Progress>>,
    pub(crate) codex_progress: Option<Arc<_Progress>>,
    pub(crate) turn_request: i64,
    pub(crate) pending_answer: Option<Pending>,
    pub(crate) pending_answer_chat: Option<i64>,
    pub(crate) pending_strict: bool,
    pub(crate) pending_words: HashSet<String>,
    pub(crate) pending_firm: bool,
    pub(crate) permission_future: Option<Pending>,
    pub(crate) questions: Vec<Arc<_Question>>,
    pub(crate) taps: HashMap<String, (Arc<_Keyboard>, Choice)>,
    pub(crate) tap_seq: i64,
    pub(crate) options_board: Option<Arc<_Keyboard>>,
    /// the latest picker (claude on, new claude, codex)
    pub(crate) picker_board: Option<Arc<_Keyboard>>,
    pub(crate) stopped_from_chat: bool,
    pub(crate) task_returned: bool,
    /// _shutdown has begun: no new edit jobs are started
    pub(crate) stopping: bool,
    pub(crate) seen_marks: HashSet<i64>,
    pub(crate) jobs: Vec<Job>,
    pub(crate) dropped_ids: HashSet<i64>,
    pub(crate) typing: HashMap<i64, HashMap<String, i64>>,
    pub(crate) typing_tasks: HashMap<i64, Job>,
    pub(crate) paused_typing: HashMap<i64, HashMap<String, i64>>,
    pub(crate) drafts_ok: bool,
    pub(crate) draft_task: Option<Job>,
    pub(crate) draft_ticks: i64,
    pub(crate) draft_id: i64,
}

impl TelegramInlet {
    /// py: telegram.py:2610 `def __init__(self, config: TelegramConfig, api: Any, create: Create, *, stream_create: Optional[Callable[[dict[str, Any], Callable[[str], None]], Awaitable[dict[str, Any]]]] = None, agent_factory: Optional[Callable[..., Any]] = None, agent_enabled: bool = True, busy: Callable[[], bool] = lambda: False, brief: Callable[[], str] = lambda: "", on_photo: Optional[Callable[[str], Awaitable[dict[str, Any]]]] = None, thinker: Optional[Callable[..., Awaitable[dict[str, Any]]]] = None, on_state: Callable[[str], None] = lambda state: None, memory: Optional[Memory] = None, apps: Any = None, direct: Any = None, vault: Any = None, chat_log: Optional[chat_log.ChatLog] = None, watcher: Optional[watch.Watcher] = None, chief: Optional[chief_mod.Chief] = None, meeter: Optional[meet.Meeter] = None, canvas: Optional[canvas.Canvas] = None, agentcard: Any = None, america: Any = None, opentable_enabled: bool = False, share: Any = None, toolset_mode: Optional[str] = None, toolset_route: Any = _FROM_ENV, grader: Any = _FROM_ENV, small_talk_ask: Any = _FROM_ENV, screen: Callable[[], Optional[Path]] = capture_screen, scene: Any = None, head: Any = None, on_explore: Optional[Callable[[], Any]] = None, on_sound: Optional[Callable[[bool], None]] = None, on_caption: Optional[Callable[[dict[str, Any]], None]] = None, notes: Optional[Callable[[], Any]] = None, terminal: Optional[Callable[[str, str], Awaitable[str]]] = None, launcher: Callable[[Path, str], Awaitable[str]] = claude_launch.open_session, launch_root: Optional[Path] = None, launch_recent: Callable[[], list[claude_launch.Recent]] = claude_launch.load_recent, claude_sessions: Callable[[], Any] = lambda: [], codex: Optional[codex_chat.CodexChat] = None, codex_folders: Callable[[], list[Path]] = codex_chat.accessible_folders, permission_timeout_secs: float = DEFAULT_PERMISSION_TIMEOUT_SECS, clock: Callable[[], float] = time.monotonic, wall: Callable[[], float] = time.time, sleep: Callable[[float], Awaitable[None]] = asyncio.sleep, **app_hubs: Any) -> None:`
    pub fn new(config: TelegramConfig, api: Arc<dyn TelegramApi>, create: Create, options: InletOptions) -> Arc<Self> {
        todo!("telegram.py:2610 TelegramInlet.__init__")
    }

    /// `self.stealth`.
    pub fn stealth(&self) -> bool {
        self.st.lock().stealth
    }

    /// `self.claude`: the terminal relay is on.
    pub fn claude(&self) -> bool {
        self.st.lock().claude
    }

    /// `self.muse`.
    pub fn muse(&self) -> bool {
        self.st.lock().muse
    }

    /// `self.turns`, copied.
    pub fn turns(&self) -> Vec<(String, String)> {
        self.st.lock().turns.clone()
    }

    /// py: telegram.py:2875 `def task_running(self) -> bool:`
    pub fn task_running(self: &Arc<Self>) -> bool {
        todo!("telegram.py:2875 TelegramInlet.task_running")
    }

    /// py: telegram.py:2879 `def _awaiting_answer(self) -> bool:`
    ///
    /// A question (a task's, or a permission yes/no) is waiting. Whether a given text answers it is
    /// ``_answers_pending``: any text for a task's question, only a clear yes/no for a prompt with buttons.
    pub(crate) fn awaiting_answer(self: &Arc<Self>) -> bool {
        todo!("telegram.py:2879 TelegramInlet._awaiting_answer")
    }
}
