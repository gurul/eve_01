//! "hey eve": the ears and the wake word, the conversation (`_converse`), the board's hold-to-talk desk calls,
//! the speaker gate, the head-pose asker, the agent's resync and keepalive, the room notes and the phone's notes,
//! and the conversation's hands (the terminal, the Mac's busy flags).

use std::sync::Arc;

use parking_lot::Mutex;

use crate::daemon::Daemon;
use crate::seams::{HeadAsker, Inlet, Obj};

/// The speaker gate as the voice session takes it: `eve_voice_gate::SpeakerGate` behind a lock, logging its
/// summary when the conversation ends (Python logged `gate.summary()` in `_converse`'s finally).
pub struct VoiceGateHandle(pub Arc<Mutex<eve_voice_gate::SpeakerGate>>);

impl eve_brain::seams::SpeakerGate for VoiceGateHandle {
    fn set_bypass(&self, on: bool) {
        todo!()
    }
    fn set_lenient(&self, on: bool) {
        todo!()
    }
    fn process(&self, raw: &[u8]) -> Vec<Vec<u8>> {
        todo!()
    }
}

impl Daemon {
    /// `_start_ears(loop)`: the wake word's microphone, when voice is on and the model loads; stored as `ears`.
    pub fn start_ears(self: &Arc<Self>, handle: &tokio::runtime::Handle) {
        todo!()
    }

    /// `_wake_suppressed()`: a wake now would be ignored (a conversation is open, the mic is off, a lesson is
    /// listening, a desk call holds the mic).
    pub fn wake_suppressed(&self) -> bool {
        todo!()
    }

    /// `_desk_calls_get()`: the board's hold-to-talk calls, made on the first press (needs the text door).
    pub fn desk_calls_get(self: &Arc<Self>) -> Option<Arc<eve_desk_call::DeskCalls>> {
        todo!()
    }

    /// `_on_wake(keyword)`: the ears heard the wake word: open a conversation unless one is open or suppressed.
    pub fn on_wake(self: &Arc<Self>, keyword: &str) {
        todo!()
    }

    /// `_voice_gate_for(think_aloud)`: the speaker gate for this conversation, None when off or the model does
    /// not load (the embedder is loaded once, off the loop).
    pub async fn voice_gate_for(self: &Arc<Self>, think_aloud: Option<&Obj>) -> Option<Arc<VoiceGateHandle>> {
        todo!()
    }

    /// `_head_pose_asker()`: Jev's head-pose asker, made once; None when off.
    pub fn head_pose_asker(&self) -> Option<HeadAsker> {
        todo!()
    }

    /// `_resync_agent()`: the agent's phase to the board again.
    pub async fn resync_agent(self: &Arc<Self>) {
        todo!()
    }

    /// `_agent_keepalive()`: re-send the phase while a conversation is open.
    pub async fn agent_keepalive(self: &Arc<Self>) {
        todo!()
    }

    /// `_converse(think_aloud=None, lesson_wake=False)`: one voice conversation, start to close: the memory's
    /// opening, the gate, the session with everything it is lent, the transcript, the explore wish afterwards.
    pub async fn converse(self: &Arc<Self>, think_aloud: Option<Obj>, lesson_wake: bool) {
        todo!()
    }

    /// `_room_notes_taker()`: the room notes, made on the first ask.
    pub fn room_notes_taker(self: &Arc<Self>) -> Arc<eve_notes::RoomNotes> {
        todo!()
    }

    /// `_phone_notes(inlet)`: the phone's notes for the Mini App's calls, made with the text door.
    pub fn phone_notes(self: &Arc<Self>, inlet: &Arc<dyn Inlet>) -> Arc<eve_phone_call::PhoneNotes> {
        todo!()
    }

    /// `_on_voice_explore()`: "go explore" said by voice: granted once the conversation has closed.
    pub fn on_voice_explore(&self) {
        todo!()
    }

    /// `_type_into_terminal(cwd, text)`: raise the session's terminal and type `text` into it; the outcome line.
    pub async fn type_into_terminal(&self, cwd: &str, text: &str) -> String {
        todo!()
    }

    /// `_texted_task_running()`: the text door has a desktop task running.
    pub fn texted_task_running(&self) -> bool {
        todo!()
    }

    /// `_desk_has_the_mac()`: a desk call's task holds the Mac.
    pub fn desk_has_the_mac(&self) -> bool {
        todo!()
    }
}
