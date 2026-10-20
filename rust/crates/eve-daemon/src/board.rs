//! The board side of the daemon: heartbeats, resync on connect, the status poller (the link's watchdog), the Mac
//! microphone switch, the camera, the sound, the tracker's stats and the owner's identity.

use std::sync::Arc;

use crate::daemon::Daemon;
use crate::seams::Obj;

impl Daemon {
    /// `_heartbeat_loop()`: a heartbeat whenever the state changed, and a keepalive every HEARTBEAT_KEEPALIVE.
    pub async fn heartbeat_loop(self: &Arc<Self>) {
        todo!()
    }

    /// `_push_heartbeat(force)`: build the heartbeat from the state and send it when it changed (or `force`, or
    /// the keepalive is due); entries are encoded with the CJK codec when the firmware has one.
    pub async fn push_heartbeat(self: &Arc<Self>, force: bool) {
        todo!()
    }

    /// `_resync_board()`: everything the board forgets on a reconnect, now.
    pub async fn resync_board(self: &Arc<Self>) {
        todo!()
    }

    /// `_send_resync()`: time sync, the forced heartbeat, the camera, the sound, the listen pose, the mode.
    pub async fn send_resync(self: &Arc<Self>) {
        todo!()
    }

    /// `_on_ble_connected()`: the link came up (each time): resync and the agent's phase.
    pub async fn on_ble_connected(self: &Arc<Self>) {
        todo!()
    }

    /// `_mic_wanted()`: the Mac microphone should be listening (the switch is on and nothing suppresses it).
    pub fn mic_wanted(&self) -> bool {
        todo!()
    }

    /// `_apply_mic(why)`: start or stop the ears to match `mic_wanted`, logging `why`.
    pub fn apply_mic(&self, why: &str) {
        todo!()
    }

    /// `_mic_status()`: {"mic": on, "listening": ...} for IPC and the Mini App.
    pub fn mic_status(&self) -> Obj {
        todo!()
    }

    /// `_status_poller()`: {"cmd":"status"} every STATUS_POLL_INTERVAL_SECS; STATUS_MISSED_LIMIT unanswered in a
    /// row reconnects (serial: pulses reset, then forces a reconnect, escalating).
    pub async fn status_poller(self: &Arc<Self>) {
        todo!()
    }

    /// `_send_cam(on)`: the camera command (vision.build_cam_cmd).
    pub async fn send_cam(self: &Arc<Self>, on: bool) {
        todo!()
    }

    /// `_send_sound()`: the board's sound to match the mute switch.
    pub async fn send_sound(self: &Arc<Self>) {
        todo!()
    }

    /// `_set_sound(on)`: flip the mute switch, persist it, and send it.
    pub fn set_sound(&self, on: bool) {
        todo!()
    }

    /// `_vision_stats_loop()`: the tracker's stats line every STATS_INTERVAL_SECS while it is enabled.
    pub async fn vision_stats_loop(self: &Arc<Self>) {
        todo!()
    }

    /// `_make_identity()`: the owner's prints and the Vision describer, None when identity is off.
    pub fn make_identity(&self) -> Option<Arc<eve_identity::FaceIdentity>> {
        todo!()
    }

    /// `_handle_identity(action)`: "status", "enroll", "forget" over IPC.
    pub fn handle_identity(&self, action: &str) -> Obj {
        todo!()
    }
}
