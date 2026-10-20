//! `run()` and shutdown, the memory facade and the dream, closing the open conversations, draining the background
//! work, and the memory bus with its sinks (rosbridge, claude-mem) and services (remember, recall).

use std::sync::Arc;

use crate::daemon::Daemon;
use crate::seams::{Job, Obj};

impl Daemon {
    /// `run()`: everything the daemon does until shutdown: the IPC server, the link, the tailer, the heartbeat,
    /// the poller, the ears, the explorer, the memory, the dream, the text door, the Mini App, the learning
    /// server, the bus's sinks, the update check; then the orderly close.
    pub async fn run(self: &Arc<Self>) -> anyhow::Result<()> {
        todo!()
    }

    /// `shutdown()`: set the shutdown flag; `run()` returns once its close is done.
    pub async fn shutdown(&self) {
        todo!()
    }

    /// `_close_notes()`: stop the room notes and the phone's notes, giving them NOTES_SHUTDOWN_SECS.
    pub async fn close_notes(self: &Arc<Self>) {
        todo!()
    }

    /// `_build_memory()`: the memory facade (records migrated, transcripts, mem0), None while memory is off.
    pub fn build_memory(&self) -> Option<Arc<eve_memory::Memory>> {
        todo!()
    }

    /// `_open_memory()`: build the memory off the loop and keep it as `memory`.
    pub async fn open_memory(self: &Arc<Self>) {
        todo!()
    }

    /// `_start_dream()`: the nightly dream's loop, None while memory is off.
    pub fn start_dream(self: &Arc<Self>) -> Option<Job> {
        todo!()
    }

    /// `_close_open_conversations()`: close markers on the open voice and text conversations, within
    /// CLOSE_MARKERS_SECS.
    pub async fn close_open_conversations(self: &Arc<Self>) {
        todo!()
    }

    /// `_drain_background()`: wait up to DRAIN_SECS for the background jobs, then cancel what is left.
    pub async fn drain_background(&self) {
        todo!()
    }

    /// `_voice_memory(memory)`: (brief, today, backend_today) for a voice conversation's opening: the opening
    /// brief and today's lines when they build within TODAY_BUILD_SECS.
    pub async fn voice_memory(self: &Arc<Self>, memory: Option<Arc<eve_memory::Memory>>) -> (String, String, String) {
        todo!()
    }

    /// `_start_memory_sinks()`: the rosbridge server and the claude-mem sink and mirror, when configured.
    pub async fn start_memory_sinks(self: &Arc<Self>) {
        todo!()
    }

    /// `_stop_memory_sinks()`.
    pub async fn stop_memory_sinks(self: &Arc<Self>) {
        todo!()
    }

    /// `_publish_observation(rec)`: a scene observation onto the bus.
    pub fn publish_observation(&self, rec: &eve_scene::Observation) {
        todo!()
    }

    /// `_publish_lesson(action, stage, feedback)`: a lesson event onto the bus.
    pub fn publish_lesson(&self, action: &str, stage: &str, feedback: &str) {
        todo!()
    }

    /// `_on_remember(msg)`: the bus's "remember" service: a fact into the records.
    pub fn on_remember(&self, msg: &Obj) {
        todo!()
    }

    /// `_recall_service(args)`: the bus's "recall" service: claude-mem's search.
    pub async fn recall_service(self: &Arc<Self>, args: Obj) -> Obj {
        todo!()
    }
}
