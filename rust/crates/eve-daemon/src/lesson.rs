//! The robot's caption and phase while a lesson runs, lessons over IPC (`_handle_lesson`), and think out loud:
//! the listener the learning server calls, starting and stopping the listening conversation.

use std::sync::Arc;

use crate::daemon::Daemon;
use crate::seams::{Obj, VoiceSessionHandle};

/// [`eve_learning::Listener`] for the daemon: the learning server's thread asks the loop to start or stop think
/// out loud, waiting at most THINK_ALOUD_CALL_SECS (`_make_listener`).
pub struct DaemonListener {
    pub daemon: std::sync::Weak<Daemon>,
    pub handle: tokio::runtime::Handle,
}

impl eve_learning::Listener for DaemonListener {
    fn start(&self, lesson: &Obj) -> anyhow::Result<Obj> {
        todo!()
    }
    fn stop(&self) -> anyhow::Result<Obj> {
        todo!()
    }
    fn status(&self) -> anyhow::Result<Obj> {
        todo!()
    }
}

impl Daemon {
    /// `_cancel_active_task(reason)`: cancel the desktop task's agent when one is running.
    pub async fn cancel_active_task(&self, reason: &str) {
        todo!()
    }

    /// `_learning_notice(text, stage)`: a lesson caption on the robot, a bus event, and "back to idle" later.
    pub fn learning_notice(self: &Arc<Self>, text: &str, stage: &str) {
        todo!()
    }

    /// `_learning_idle()`: the robot back to idle after a lesson caption.
    pub fn learning_idle(self: &Arc<Self>) {
        todo!()
    }

    /// `_make_listener(loop)`.
    pub fn make_listener(self: &Arc<Self>, handle: &tokio::runtime::Handle) -> Arc<DaemonListener> {
        todo!()
    }

    /// `_think_aloud_start(lesson)`: listen to the owner think out loud for this lesson.
    pub async fn think_aloud_start(self: &Arc<Self>, lesson: Obj) -> Obj {
        todo!()
    }

    /// `_think_aloud_stop()`.
    pub async fn think_aloud_stop(self: &Arc<Self>) -> Obj {
        todo!()
    }

    /// `_start_think_aloud_conversation(lesson)`: a conversation opened for listening (THINK_ALOUD_TASK).
    pub fn start_think_aloud_conversation(self: &Arc<Self>, lesson: Obj) {
        todo!()
    }

    /// `_on_voice_session(session)`: the conversation's session connected; a waiting wish is entered.
    pub fn on_voice_session(self: &Arc<Self>, session: Arc<dyn VoiceSessionHandle>) {
        todo!()
    }

    /// `_on_think_aloud(on, lesson_id)`: the session entered or left listening.
    pub fn on_think_aloud(self: &Arc<Self>, on: bool, lesson_id: Option<&str>) {
        todo!()
    }

    /// `_on_expression_text(who, text)`: a line the live expressions read.
    pub fn on_expression_text(&self, who: &str, text: &str) {
        todo!()
    }

    /// `_on_caption(msg)`: the voice's caption to the board.
    pub fn on_caption(self: &Arc<Self>, msg: Obj) {
        todo!()
    }

    /// `_on_agent_state(state)`: the voice's phase: the board's, the follower's and the listen pose.
    pub fn on_agent_state(self: &Arc<Self>, state: &str) {
        todo!()
    }

    /// `_sync_listen_pose()`: {"cmd":"listen"} to match the key, the button and the phase, when it changed.
    pub fn sync_listen_pose(self: &Arc<Self>) {
        todo!()
    }

    /// `_handle_lesson(req)`: a lesson over IPC (tutor and listen actions).
    pub async fn handle_lesson(self: &Arc<Self>, req: Obj) -> Obj {
        todo!()
    }
}
