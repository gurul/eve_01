//! The idle explorer (`_explore_loop`, `_explore_step`, `_run_explore_action`, request/dismiss/stop), thoughts on
//! the robot's screen, snapshots and photos for the owner, the listen key, and the daily update check.

use std::sync::Arc;

use crate::daemon::Daemon;
use crate::seams::Obj;

impl Daemon {
    /// `_note_activity()`: something a human or a session did, now (the idle clock restarts).
    pub fn note_activity(&self) {
        todo!()
    }

    /// `_idle_secs(now)`: seconds since the last activity.
    pub fn idle_secs(&self, now: f64) -> f64 {
        todo!()
    }

    /// `_explore_loop()`: tick the explorer once a second until shutdown.
    pub async fn explore_loop(self: &Arc<Self>) {
        todo!()
    }

    /// `_explore_step(now)`: one tick: the explorer's actions, run.
    pub async fn explore_step(self: &Arc<Self>, now: f64) {
        todo!()
    }

    /// `_run_explore_action(action)`: look, mode, note.
    pub async fn run_explore_action(self: &Arc<Self>, action: eve_explore::Action) {
        todo!()
    }

    /// `_show_thought(thought)`: a diary thought on the robot's screen, paged.
    pub fn show_thought(self: &Arc<Self>, thought: &eve_diary::Thought) {
        todo!()
    }

    /// `_flush_thought_pager()`: the pager's due events to the board.
    pub fn flush_thought_pager(self: &Arc<Self>) {
        todo!()
    }

    /// `_thought_caption_loop()`: poll the pager while exploring.
    pub async fn thought_caption_loop(self: &Arc<Self>) {
        todo!()
    }

    /// `_clear_thought()`: the screen back to normal.
    pub fn clear_thought(self: &Arc<Self>) {
        todo!()
    }

    /// `_send_emote(e)`: the diary's emote to the board.
    pub async fn send_emote(self: &Arc<Self>, e: eve_diary::Emote) {
        todo!()
    }

    /// `_take_snapshot()`: one full-size frame from the board ({"cmd":"snap"}), within SNAP_TIMEOUT_SECS.
    pub async fn take_snapshot(self: &Arc<Self>) -> Option<eve_vision::Frame> {
        todo!()
    }

    /// `_photo_for_owner(said)`: a picture for the owner from the robot's camera, kept on the shelf.
    pub async fn photo_for_owner(self: &Arc<Self>, said: &str) -> Obj {
        todo!()
    }

    /// `_mac_photo(said)`: a picture from the Mac's camera.
    pub async fn mac_photo(self: &Arc<Self>, said: &str) -> Obj {
        todo!()
    }

    /// `_request_explore(reason)`: go explore now, or why not.
    pub async fn request_explore(self: &Arc<Self>, reason: &str) -> Result<(), eve_explore::ExploreRefused> {
        todo!()
    }

    /// `_dismiss_explore(reason)`: stop exploring and stay home for a while.
    pub async fn dismiss_explore(self: &Arc<Self>, reason: &str) {
        todo!()
    }

    /// `_stop_explore(reason)`: stop exploring.
    pub async fn stop_explore(self: &Arc<Self>, reason: &str) {
        todo!()
    }

    /// `_on_listen_key(on)`: the held Option key, from the key tap's thread.
    pub fn on_listen_key(self: &Arc<Self>, on: bool) {
        todo!()
    }

    /// `_reset_listen()`: forget what the board was sent, so the next sync sends it again.
    pub async fn reset_listen(self: &Arc<Self>) {
        todo!()
    }

    /// `_update_check_loop()`: GitHub's newest release once a day; `update_available` when newer.
    pub async fn update_check_loop(self: &Arc<Self>) {
        todo!()
    }
}
