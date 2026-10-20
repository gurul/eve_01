//! A port of `voice_agent.py`, lines 1583-2240: VoiceSession: tool calls, the gate, tasks, look/photo/head, intents, speech.
//!
//! Skeleton: constants and prompts are verbatim (generated from the Python values); bodies are
//! `todo!()` until this file's unit fills them.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::voice_agent::*;

impl VoiceSession {
    /// py: voice_agent.py:1583 `async def _tool(self, name: str, call_id: str, arguments: str) -> None:`
    pub(crate) async fn tool(self: &Arc<Self>, name: &str, call_id: &str, arguments: &str) {
        todo!("voice_agent.py:1583 VoiceSession._tool")
    }

    /// py: voice_agent.py:1676 `def _owner_answered(self, name: str, args: dict[str, Any]) -> bool:`
    ///
    /// Pass the owner's exact answer to the question it followed, once, without asking again.
    ///
    /// Bare yes/no synonyms may be normalized; free words must match the owner's transcript exactly.
    /// A model-authored answer, a stale answer or an answer for a different question still needs confirmation.
    pub(crate) fn owner_answered(self: &Arc<Self>, name: &str, args: &Obj) -> bool {
        todo!("voice_agent.py:1676 VoiceSession._owner_answered")
    }

    /// py: voice_agent.py:1701 `def _gate(self, name: str, args: dict[str, Any]) -> Optional[dict[str, Any]]:`
    ///
    /// The taint gate out loud: None when this exact call was asked about and the owner has since said a bare
    /// yes (it runs), else the call's refusal. The question is code's, never the model's (v3 safety review: a
    /// tainted model wrote its own question, and could ask about one act and call another): tool_gate.spoken_question
    /// says the held act from its name and arguments, handed to the voice word for word, and the yes approves that
    /// exact call (same name, same arguments) only. A call that cannot be said briefly is refused unasked.
    ///
    /// The limit (round-4 review): the Live model has no say-this-verbatim path and eve has no separate TTS here,
    /// so the question goes as commentary and the model could paraphrase it. The yes counts only after the voice's
    /// own output transcript held the act whole (tool_gate.said_whole, checked in _close_turn); otherwise the held
    /// call is dropped and asked again. The transcript is the model's account of its audio: words, not sound.
    pub(crate) fn gate(self: &Arc<Self>, name: &str, args: &Obj) -> Option<Obj> {
        todo!("voice_agent.py:1701 VoiceSession._gate")
    }

    /// py: voice_agent.py:1741 `def _start_task(self, goal: str) -> dict[str, Any]:`
    pub(crate) fn start_task(self: &Arc<Self>, goal: &str) -> Obj {
        todo!("voice_agent.py:1741 VoiceSession._start_task")
    }

    /// py: voice_agent.py:1763 `def _slow_tool(self, name: str, call_id: str, args: dict[str, Any]) -> None:`
    pub(crate) fn slow_tool(self: &Arc<Self>, name: &str, call_id: &str, args: &Obj) {
        todo!("voice_agent.py:1763 VoiceSession._slow_tool")
    }

    /// py: voice_agent.py:1859 `async def _meeting(self, name: str, args: dict[str, Any]) -> dict[str, Any]:`
    ///
    /// join_meeting / leave_meeting: the same Meeter the chat uses (a calendar read can take a second or two,
    /// so it runs as a slow tool).
    pub(crate) async fn meeting(self: &Arc<Self>, name: &str, args: &Obj) -> Obj {
        todo!("voice_agent.py:1859 VoiceSession._meeting")
    }

    /// py: voice_agent.py:1868 `async def _look(self) -> dict[str, Any]:`
    pub(crate) async fn look(self: &Arc<Self>) -> Obj {
        todo!("voice_agent.py:1868 VoiceSession._look")
    }

    /// py: voice_agent.py:1875 `async def _take_photo(self, note: str) -> dict[str, Any]:`
    ///
    /// The owner asked for a picture: the daemon snaps, keeps and captions it.
    pub(crate) async fn take_photo(self: &Arc<Self>, note: &str) -> Obj {
        todo!("voice_agent.py:1875 VoiceSession._take_photo")
    }

    /// py: voice_agent.py:1884 `async def _look_around(self) -> dict[str, Any]:`
    pub(crate) async fn look_around(self: &Arc<Self>) -> Obj {
        todo!("voice_agent.py:1884 VoiceSession._look_around")
    }

    /// py: voice_agent.py:1889 `async def _find(self, target: str) -> dict[str, Any]:`
    pub(crate) async fn find(self: &Arc<Self>, target: &str) -> Obj {
        todo!("voice_agent.py:1889 VoiceSession._find")
    }

    /// py: voice_agent.py:1895 `async def _think_hard(self, question: str) -> dict[str, Any]:`
    pub(crate) async fn think_hard(self: &Arc<Self>, question: &str) -> Obj {
        todo!("voice_agent.py:1895 VoiceSession._think_hard")
    }

    /// py: voice_agent.py:1915 `async def _move_head(self, args: dict[str, Any]) -> dict[str, Any]:`
    pub(crate) async fn move_head(self: &Arc<Self>, args: &Obj) -> Obj {
        todo!("voice_agent.py:1915 VoiceSession._move_head")
    }

    /// py: voice_agent.py:1927 `async def _stop_scene(self) -> None:`
    pub(crate) async fn stop_scene(self: &Arc<Self>) {
        todo!("voice_agent.py:1927 VoiceSession._stop_scene")
    }

    /// py: voice_agent.py:1932 `def _on_user_turn(self, text: str) -> None:`
    pub(crate) fn on_user_turn(self: &Arc<Self>, text: &str) {
        todo!("voice_agent.py:1932 VoiceSession._on_user_turn")
    }

    /// py: voice_agent.py:1971 `async def _classify(self, text: str, said_at: float) -> None:`
    pub(crate) async fn classify(self: &Arc<Self>, text: &str, said_at: f64) {
        todo!("voice_agent.py:1971 VoiceSession._classify")
    }

    /// py: voice_agent.py:1977 `def _apply_intent(self, label: str, text: str, how: str, said_at: float) -> None:`
    pub(crate) fn apply_intent(self: &Arc<Self>, label: &str, text: &str, how: &str, said_at: f64) {
        todo!("voice_agent.py:1977 VoiceSession._apply_intent")
    }

    /// py: voice_agent.py:1999 `def _last_substantive_turn(self, said: str) -> str:`
    ///
    /// What "remember that" points at.
    ///
    /// It is anaphoric: the claim is in the previous turn, not in the words
    /// "remember that". Only the owner's own last substantive turn counts — it
    /// is their claim about themselves. eve's own words are never starred
    /// (owner, 2026-09-23: the old fallback to eve's last line laundered its
    /// replies into the permanent layer); with no owner turn, nothing is.
    pub(crate) fn last_substantive_turn(self: &Arc<Self>, said: &str) -> String {
        todo!("voice_agent.py:1999 VoiceSession._last_substantive_turn")
    }

    /// py: voice_agent.py:2018 `def _remember(self, claim: str) -> None:`
    ///
    /// Star it through memory.star, and say so in one clause. Silence would look like it failed.
    pub(crate) fn remember(self: &Arc<Self>, claim: &str) {
        todo!("voice_agent.py:2018 VoiceSession._remember")
    }

    /// py: voice_agent.py:2028 `async def _star(self, claim: str) -> None:`
    pub(crate) async fn star(self: &Arc<Self>, claim: &str) {
        todo!("voice_agent.py:2028 VoiceSession._star")
    }

    /// py: voice_agent.py:2044 `async def _fast_head_move(self, text: str, turn_started_at: float) -> None:`
    ///
    /// Ask the typed model for a pose and, when it is sure, turn the head now. Any failure, an abstention or
    /// "none" leaves the turn to the ordinary path exactly as before.
    pub(crate) async fn fast_head_move(self: &Arc<Self>, text: &str, turn_started_at: f64) {
        todo!("voice_agent.py:2044 VoiceSession._fast_head_move")
    }

    /// py: voice_agent.py:2066 `async def _route_look(self, text: str, turn_started_at: float) -> None:`
    ///
    /// Hand a head request to the backend unless the voice already delegated this
    /// turn: a relative move ("a bit more left") must never run twice.
    pub(crate) async fn route_look(self: &Arc<Self>, text: &str, turn_started_at: f64) {
        todo!("voice_agent.py:2066 VoiceSession._route_look")
    }

    /// py: voice_agent.py:2079 `async def _route_lesson(self, text: str, turn_started_at: float, setup: bool = False) -> None:`
    ///
    /// Hand a "teach me" (or, in lesson setup, any answer) to the backend unless the voice already
    /// delegated this turn.
    ///
    /// 11:44 on the bench: "teach me about Rust ownership" got "Sure. What have you done in Rust so
    /// far?" and a spoken explanation, and the lesson window never opened.
    pub(crate) async fn route_lesson(self: &Arc<Self>, text: &str, turn_started_at: f64, setup: bool) {
        todo!("voice_agent.py:2079 VoiceSession._route_lesson")
    }

    /// py: voice_agent.py:2096 `def _begin_farewell(self, said_at: float) -> None:`
    ///
    /// Stamped with when the owner finished saying it, not when a classifier
    /// answered: eve's goodbye may already be on its way by then.
    pub(crate) fn begin_farewell(self: &Arc<Self>, said_at: f64) {
        todo!("voice_agent.py:2096 VoiceSession._begin_farewell")
    }

    /// py: voice_agent.py:2109 `def _farewell_due(self, now: float) -> bool:`
    ///
    /// The goodbye has been said back (or never will be) and nothing is still being said.
    pub(crate) fn farewell_due(self: &Arc<Self>, now: f64) -> bool {
        todo!("voice_agent.py:2109 VoiceSession._farewell_due")
    }

    /// py: voice_agent.py:2121 `def _set_sound(self, on: bool, how: str) -> None:`
    pub(crate) fn set_sound(self: &Arc<Self>, on: bool, how: &str) {
        todo!("voice_agent.py:2121 VoiceSession._set_sound")
    }

    /// py: voice_agent.py:2136 `def _bg(self, coro: Awaitable[None]) -> None:`
    pub(crate) fn bg(self: &Arc<Self>, coro: impl std::future::Future<Output = ()> + Send + 'static) {
        todo!("voice_agent.py:2136 VoiceSession._bg")
    }

    /// py: voice_agent.py:2141 `async def _run_agent(self, goal: str) -> str:`
    pub(crate) async fn run_agent(self: &Arc<Self>, goal: &str) -> String {
        todo!("voice_agent.py:2141 VoiceSession._run_agent")
    }

    /// py: voice_agent.py:2160 `def _on_agent_event(self, ev: AgentEvent) -> None:`
    pub(crate) fn on_agent_event(self: &Arc<Self>, ev: &eve_agent_contract::AgentEvent) {
        todo!("voice_agent.py:2160 VoiceSession._on_agent_event")
    }

    /// py: voice_agent.py:2187 `async def _ask_user(self, question: str) -> str:`
    pub(crate) async fn ask_user(self: &Arc<Self>, question: &str) -> String {
        todo!("voice_agent.py:2187 VoiceSession._ask_user")
    }

    /// py: voice_agent.py:2212 `async def _speak(self, content: str) -> None:`
    ///
    /// Hand the voice something to say (`session.commentary.append`).
    ///
    /// The SDK documents commentary as "speakable context … for a result the model
    /// should communicate". Routing a result through the backend instead
    /// (response.item.create + response.create) left the voice free to say anything:
    /// on 2026-09-10 a finished task, "Spotify is playing INTERGALACTIC.", reached the
    /// owner as "Okay, waiting."
    pub(crate) async fn speak(self: &Arc<Self>, content: &str) {
        todo!("voice_agent.py:2212 VoiceSession._speak")
    }

    /// py: voice_agent.py:2223 `async def _quiet(self, content: str) -> None:`
    ///
    /// Silent context for the voice (`session.thinking.append`): it can shape later
    /// speech but asks for none.
    pub(crate) async fn quiet(self: &Arc<Self>, content: &str) {
        todo!("voice_agent.py:2223 VoiceSession._quiet")
    }

    /// py: voice_agent.py:2228 `async def _backend_note(self, text: str) -> None:`
    ///
    /// Context for the backend's next delegated turn; asks for no response.
    pub(crate) async fn backend_note(self: &Arc<Self>, text: &str) {
        todo!("voice_agent.py:2228 VoiceSession._backend_note")
    }

    /// py: voice_agent.py:2233 `async def _request_response(self) -> None:`
    ///
    /// response.create now, or as soon as the in-flight backend response completes.
    pub(crate) async fn request_response(self: &Arc<Self>) {
        todo!("voice_agent.py:2233 VoiceSession._request_response")
    }
}
