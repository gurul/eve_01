//! What the board sends (`_handle_ble`), the transcript tailer's callbacks (`_on_tokens`, `_on_assistant_text`),
//! the turn end's side effects and the deferred running→0 flip, and the ack waiters (`expect_ack`,
//! `wait_for_ack`; [`eve_folder_push::PushTarget`] for the folder push).

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::daemon::Daemon;
use crate::seams::Obj;

impl Daemon {
    /// `_handle_ble(obj)`: one line from the board (or the Voice PE): acks, status, diag, frames, faces, poses,
    /// buttons, swipes, ptt, the listen key, the stick, music mode, explore, the snap waiter.
    pub async fn handle_ble(self: &Arc<Self>, obj: Obj) -> anyhow::Result<()> {
        todo!()
    }

    /// `_on_tokens(cumulative, today, cost_cumulative, cost_today, entries)`: the tailer's totals into the state,
    /// and a heartbeat.
    pub async fn on_tokens(
        self: &Arc<Self>,
        cumulative: i64,
        today: i64,
        cost_cumulative: f64,
        cost_today: f64,
        entries: Vec<(f64, String)>,
    ) -> anyhow::Result<()> {
        todo!()
    }

    /// `_on_assistant_text(transcript_path, text, uuid)`: Claude's reply as an entry, and relayed to the text door
    /// when the relay is on.
    pub async fn on_assistant_text(self: &Arc<Self>, transcript_path: &str, text: &str, uuid: &str) -> anyhow::Result<()> {
        todo!()
    }

    /// `_turn_end_side_effects(celebrate_secs)`: the celebration and the heartbeat after a turn ends.
    pub async fn turn_end_side_effects(self: &Arc<Self>, celebrate_secs: f64) {
        todo!()
    }

    /// `_heartbeat_after(delay)`: a heartbeat `delay` seconds from now.
    pub async fn heartbeat_after(self: &Arc<Self>, delay: f64) {
        todo!()
    }

    /// `_deferred_turn_end(session_id, delay)`: the running→0 flip after the grace window, unless a new turn
    /// began meanwhile.
    pub async fn deferred_turn_end(self: &Arc<Self>, session_id: String, delay: f64) {
        todo!()
    }

    /// `expect_ack(ack_type, n)`: register a waiter now, before the request goes out.
    pub fn expect_ack(&self, ack_type: &str, n: Option<i64>) -> eve_folder_push::AckWaiter {
        self.acks.expect_ack(ack_type, n)
    }

    /// `wait_for_ack(ack_type, timeout, waiter=...)`: the ack, or a timeout.
    pub async fn wait_for_ack(
        &self,
        ack_type: &str,
        timeout: Duration,
        waiter: Option<eve_folder_push::AckWaiter>,
    ) -> Result<Value, eve_folder_push::AckWaitError> {
        self.acks.wait_for_ack(ack_type, timeout, waiter).await
    }
}

#[async_trait::async_trait]
impl eve_folder_push::PushTarget for Daemon {
    type Waiter = eve_folder_push::AckWaiter;

    fn expect_ack(&self, ack_type: &str, n: Option<i64>) -> Self::Waiter {
        self.acks.expect_ack(ack_type, n)
    }

    async fn send(&self, payload: &Value) -> bool {
        self.ble.send(payload, None).await
    }

    async fn wait_for_ack(
        &self,
        ack_type: &str,
        timeout: Duration,
        waiter: Self::Waiter,
    ) -> Result<Value, eve_folder_push::AckWaitError> {
        Daemon::wait_for_ack(self, ack_type, timeout, Some(waiter)).await
    }
}
