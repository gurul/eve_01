//! A port of `voice_agent.py`, lines 1288-1582: VoiceSession: speaker drain, captions, mic pump, watchdog, events, turns.
//!
//! Skeleton: constants and prompts are verbatim (generated from the Python values); bodies are
//! `todo!()` until this file's unit fills them.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::voice_agent::*;

impl VoiceSession {
    /// py: voice_agent.py:1288 `async def _drain_speaker(self, timeout: float = 15.0) -> None:`
    ///
    /// Let a queued reply finish playing (barge-in and errors skip this).
    pub(crate) async fn drain_speaker(self: &Arc<Self>, timeout: f64) {
        todo!("voice_agent.py:1288 VoiceSession._drain_speaker")
    }

    /// py: voice_agent.py:1295 `def _set_after_captions(self, state: str) -> None:`
    ///
    /// The phase flips when the last caption page has been read, not when the model stops.
    pub(crate) fn set_after_captions(self: &Arc<Self>, state: &str) {
        todo!("voice_agent.py:1295 VoiceSession._set_after_captions")
    }

    /// py: voice_agent.py:1302 `def _emit_events(self, events: list[Event]) -> None:`
    pub(crate) fn emit_events(self: &Arc<Self>, events: &[eve_caption_pager::Event]) {
        todo!("voice_agent.py:1302 VoiceSession._emit_events")
    }

    /// py: voice_agent.py:1311 `def _flush_pager(self) -> None:`
    pub(crate) fn flush_pager(self: &Arc<Self>) {
        todo!("voice_agent.py:1311 VoiceSession._flush_pager")
    }

    /// py: voice_agent.py:1320 `async def _tick_loop(self) -> None:`
    ///
    /// Closes a turn that has gone quiet, and pages captions when they are on.
    ///
    /// Runs in both output modes: the turn boundary is what ends a conversation
    /// after a task result, and audio mode needs it too.
    pub(crate) async fn tick_loop(self: &Arc<Self>) {
        todo!("voice_agent.py:1320 VoiceSession._tick_loop")
    }

    /// py: voice_agent.py:1332 `async def _drain_captions(self, timeout: float = 15.0) -> None:`
    ///
    /// Let the page on screen finish its hold before the session goes idle.
    pub(crate) async fn drain_captions(self: &Arc<Self>, timeout: f64) {
        todo!("voice_agent.py:1332 VoiceSession._drain_captions")
    }

    /// py: voice_agent.py:1341 `async def _pump_mic(self) -> None:`
    pub(crate) async fn pump_mic(self: &Arc<Self>) {
        todo!("voice_agent.py:1341 VoiceSession._pump_mic")
    }

    /// py: voice_agent.py:1372 `async def _watchdog(self) -> None:`
    pub(crate) async fn watchdog(self: &Arc<Self>) {
        todo!("voice_agent.py:1372 VoiceSession._watchdog")
    }

    /// py: voice_agent.py:1391 `async def _until_ended(self) -> None:`
    ///
    /// Serve Live events until the session ends — by an event, or by the watchdog.
    ///
    /// The watchdog's idle timeout and session cap only set `_ended`, and `_events`
    /// checks it only when the next Live event arrives. When the server went quiet
    /// the session never closed: all three idle closes in the log hung (+91 s,
    /// +129 s, +93 min), while the keepalive kept the frozen phase alive on the
    /// board and the wake word stayed blocked (timer review, 2026-09-10).
    pub(crate) async fn until_ended(self: &Arc<Self>) {
        todo!("voice_agent.py:1391 VoiceSession._until_ended")
    }

    /// py: voice_agent.py:1411 `async def _events(self) -> None:`
    pub(crate) async fn events(self: &Arc<Self>) {
        todo!("voice_agent.py:1411 VoiceSession._events")
    }

    /// py: voice_agent.py:1419 `async def handle_event(self, event: Any) -> None:`
    pub async fn handle_event(self: &Arc<Self>, event: &Value) {
        todo!("voice_agent.py:1419 VoiceSession.handle_event")
    }

    /// py: voice_agent.py:1449 `async def _backend_event(self, ev: Any) -> None:`
    ///
    /// A Responses streaming event, nested inside a Live `response.event`.
    pub(crate) async fn backend_event(self: &Arc<Self>, ev: &Value) {
        todo!("voice_agent.py:1449 VoiceSession._backend_event")
    }

    /// py: voice_agent.py:1491 `def _transcript_delta(self, who: str, text: str) -> None:`
    pub(crate) fn transcript_delta(self: &Arc<Self>, who: &str, text: &str) {
        todo!("voice_agent.py:1491 VoiceSession._transcript_delta")
    }

    /// py: voice_agent.py:1526 `def _close_due_turn(self) -> None:`
    pub(crate) fn close_due_turn(self: &Arc<Self>) {
        todo!("voice_agent.py:1526 VoiceSession._close_due_turn")
    }

    /// py: voice_agent.py:1531 `def _close_turn(self, who: str, text: str) -> None:`
    pub(crate) fn close_turn(self: &Arc<Self>, who: &str, text: &str) {
        todo!("voice_agent.py:1531 VoiceSession._close_turn")
    }

    /// py: voice_agent.py:1575 `def _emit_expression(self, who: str, text: str) -> None:`
    pub(crate) fn emit_expression(self: &Arc<Self>, who: &str, text: &str) {
        todo!("voice_agent.py:1575 VoiceSession._emit_expression")
    }
}
