//! A port of `voice_agent.py`, lines 2241-2365: the Live API connection (open_session) and helpers.
//!
//! Skeleton: constants and prompts are verbatim (generated from the Python values); bodies are
//! `todo!()` until this file's unit fills them.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::voice_agent::*;

/// py: voice_agent.py:2241 `def _clip_output(result: dict[str, Any], cap: int) -> str:`
///
/// A tool result as JSON of at most `cap` chars, still valid JSON when clipped.
pub(crate) fn clip_output(result: &Obj, cap: i64) -> String {
    todo!("voice_agent.py:2241 _clip_output")
}

/// py: voice_agent.py:2257 `def _attr(event: Any, name: str) -> Any:`
pub(crate) fn attr(event: &Value, name: &str) -> Value {
    todo!("voice_agent.py:2257 _attr")
}

/// py: voice_agent.py:2268 `LIVE_KEY_ENV`
pub const LIVE_KEY_ENV: &str = "OPENAI_API_KEY";

/// py: voice_agent.py:2271 `def live_available(env: Any = None) -> bool:`
///
/// Whether the desk voice can open a Live session: OPENAI_API_KEY is set (wave 1 of docs/openrouter.md).
pub fn live_available(env: Option<&Environ>) -> bool {
    todo!("voice_agent.py:2271 live_available")
}

/// py: voice_agent.py:2277 `def warm_live_import() -> None:`
///
/// Import what `AsyncOpenAI().live` imports lazily: `openai.resources`, some
/// hundreds of modules. Idempotent and cheap once cached; call it off the loop.
pub fn warm_live_import() {
    todo!("voice_agent.py:2277 warm_live_import")
}

/// py: voice_agent.py:2283 `async def open_session( mic: "asyncio.Queue[bytes]", on_state: Callable[[str], None], agent_factory: Callable[..., ComputerAgent], config: Optional[VoiceConfig] = None, agent_enabled: bool = True, api_key: Optional[str] = None, on_caption: Optional[Callable[[dict], None]] = None, on_explore: Optional[Callable[[], None]] = None, scene: Any = None, head: Any = None, intent: Optional[Callable[[str], Awaitable[Optional[str]]]] = None, on_sound: Optional[Callable[[bool], None]] = None, muted: Callable[[], bool] = lambda: False, thinker: Optional[Callable[..., Awaitable[dict[str, Any]]]] = None, on_photo: Optional[Callable[[str], Awaitable[dict[str, Any]]]] = None, brief: str = "", memory: Optional["Memory"] = None, conv: str = "", today: str = "", backend_today: str = "", on_closed: Optional[Callable[[list[tuple[str, str]]], None]] = None, learning: Optional[Callable[..., dict[str, Any]]] = None, think_aloud: Optional[dict[str, Any]] = None, on_spoken_idea: Optional[Callable[[str, str], Any]] = None, on_think_aloud: Optional[Callable[[bool, Optional[str]], None]] = None, on_open: Optional[Callable[[VoiceSession], None]] = None, lesson_wake: bool = False, head_pose: Optional[Callable[[str], Awaitable[str]]] = None, gate: Any = None, on_expression: Optional[Callable[[str, str], None]] = None, mac_busy: Callable[[], bool] = lambda: False, meeter: Any = None, **app_hubs: Any, ) -> None:`
///
/// Run one full conversation on the real Live API — captions to the robot,
/// or the real speaker in audio mode.
///
/// `think_aloud` opens it for think out loud on that lesson. `on_open` hands the
/// session to the daemon, so a toggle can switch or stop it while it runs.
/// `memory` (with `conv`, `today`, `backend_today` from the daemon) is lent when
/// memory is on; the session writes its own transcript lines and close marker.
/// Rust: the session's own keyword arguments (config ... meeter, `**app_hubs`) are `options`; the real Live
/// connection (a [`LiveConnection`] over the Live API's websocket) is this file's unit's.
pub async fn open_session(mic: eve_ears::PcmQueue, on_state: OnState, agent_factory: AgentFactory, api_key: Option<&str>, on_closed: Option<OnClosed>, on_open: Option<OnOpenSession>, options: VoiceSessionOptions) {
    todo!("voice_agent.py:2283 open_session")
}

/// `on_closed(turns)`: the conversation's (speaker, text) turns once it is over.
pub type OnClosed = Arc<dyn Fn(Vec<(String, String)>) + Send + Sync>;
/// `on_open(session)`: the session handed to the daemon, so a toggle can switch or stop it while it runs.
pub type OnOpenSession = Arc<dyn Fn(&Arc<VoiceSession>) + Send + Sync>;
