//! The idle explorer (`_explore_loop`, `_explore_step`, `_run_explore_action`, request/dismiss/stop), thoughts on
//! the robot's screen, snapshots and photos for the owner, the listen key, and the daily update check.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tracing::{info, warn};

use crate::daemon::Daemon;
use crate::lesson::{obj, py_split};
use crate::seams::Obj;
use crate::{SNAP_TIMEOUT_SECS, UPDATE_CHECK_INTERVAL_SECS};

/// `time.monotonic()`.
fn monotonic() -> f64 {
    eve_head::monotonic()
}

/// Python `int(round(x))`: halves go to the even neighbour.
fn py_round(x: f64) -> i64 {
    let r = x.round();
    if (x - x.trunc()).abs() == 0.5 && (r as i64) % 2 != 0 {
        (r - x.signum()) as i64
    } else {
        r as i64
    }
}

/// The first `n` characters (Python's `text[:n]`).
fn head(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// Until the shutdown flag is set, or `secs` pass: true when it was the shutdown
/// (`await asyncio.wait_for(self._shutdown.wait(), timeout=secs)`).
async fn shutdown_within(rx: &mut tokio::sync::watch::Receiver<bool>, secs: f64) -> bool {
    tokio::time::timeout(Duration::from_secs_f64(secs), rx.wait_for(|set| *set)).await.is_ok_and(|r| r.is_ok())
}

impl Daemon {
    /// `_note_activity()`: something a human or a session did, now (the idle clock restarts): a hook event, a
    /// board touch, the listen key.
    pub fn note_activity(&self) {
        self.st.lock().last_activity_at = monotonic();
    }

    /// `_idle_secs(now)`: seconds since the last activity. A running or waiting session is activity in itself, so
    /// the clock does not start until it is gone.
    pub fn idle_secs(&self, now: f64) -> f64 {
        let busy = {
            let state = self.state.lock();
            state.running_count() > 0 || state.waiting_count() > 0
        };
        let mut st = self.st.lock();
        if busy {
            st.last_activity_at = now;
            return 0.0;
        }
        (now - st.last_activity_at).max(0.0)
    }

    /// `_explore_loop()`: tick the explorer once a second until shutdown.
    pub async fn explore_loop(self: &Arc<Self>) {
        let mut rx = self.shutdown_rx();
        while !self.shutting_down() {
            if shutdown_within(&mut rx, 1.0).await {
                return;
            }
            self.explore_step(monotonic()).await;
        }
    }

    /// `_explore_step(now)`: one tick: gather the facts, run the schedule, execute its actions.
    pub async fn explore_step(self: &Arc<Self>, now: f64) {
        let mut frame = None;
        if self.explorer.lock().wants_frame(now) {
            let raw = self.st.lock().explore_raw_frame.take();
            if let Some(raw) = raw {
                frame = eve_vision::decode_frame(&raw).ok();
            }
        }
        let idle_secs = self.idle_secs(now);
        let listening = self.st.lock().listen_sent.unwrap_or(false);
        let connected = self.ble.connected();
        let actions = self.explorer.lock().tick(now, idle_secs, listening, frame.as_ref(), connected);
        for action in actions {
            self.run_explore_action(action).await;
        }
    }

    /// `_run_explore_action(action)`: look, mode, note.
    pub async fn run_explore_action(self: &Arc<Self>, action: eve_explore::Action) {
        match action {
            eve_explore::Action::Mode(mode) => {
                if mode.explore {
                    info!("explore: start ({})", mode.reason);
                } else {
                    info!("explore: stop ({})", mode.reason);
                    self.st.lock().explore_raw_frame = None;
                }
                if self.ble.connected() {
                    self.ble.send(&eve_explore::build_mode_cmd(mode.explore), None).await;
                }
            }
            eve_explore::Action::Look(look) => {
                info!("explore: look yaw={:+} pitch={}", look.yaw, look.pitch);
                if self.ble.connected() {
                    self.ble
                        .send(&eve_explore::build_look_cmd(look.yaw as f64, look.pitch as f64, look.hold_ms), None)
                        .await;
                }
            }
            eve_explore::Action::Note(note) => {
                let notes = self.st.lock().notes.clone();
                if let Some(notes) = notes {
                    self.spawn("diary-note", async move {
                        if let Err(e) = notes.take(&note).await {
                            warn!("diary: note failed: {:#}", e);
                        }
                    });
                }
            }
            eve_explore::Action::Rest(rest) => {
                // Nothing over the wire: the board stays in explore mode and
                // looks around on its own until the next pan cycle.
                info!("explore: rest ({})", rest.reason);
                self.st.lock().explore_raw_frame = None;
            }
        }
    }

    /// `_show_thought(thought)`: put a thought eve just had on its own screen, if it earns it.
    ///
    /// The conversation owns the screen while one is open, and the listen key outranks anything eve has to say
    /// to itself; in either case the thought is skipped rather than queued, because by the time the screen frees
    /// up eve is looking somewhere else. Everything that gets past those is then judged on its own merits by
    /// thought_screen.
    pub fn show_thought(self: &Arc<Self>, thought: &eve_diary::Thought) {
        if !self.explorer.lock().on_board() || !self.ble.connected() {
            return;
        }
        if self.st.lock().listen_down {
            return;
        }
        if self.conversation_open() {
            return;
        }
        let now = monotonic();
        let mut text = py_split(&thought.text).join(" ");
        let cool = if thought.photographed { 1.0 } else { thought.cool };
        {
            let mut screen = self.screen.lock();
            let why = screen.refusal(now, &text, &thought.tags, thought.importance, thought.novelty, cool);
            if let Some(why) = why {
                screen.refused += 1;
                info!("explore: thought kept to itself ({}) — {}", why, head(&text, 50));
                return;
            }
            screen.record(now, &text, &thought.tags); // judged just above; offer() would judge again
        }
        if thought.photographed {
            text = format!("{text} [photo]");
        }
        self.thought_pager.lock().begin_reply(now);
        self.on_expression_text("diary", &text);
        self.thought_pager.lock().update(now, &text, true);
        self.flush_thought_pager();
        info!("explore: thought on screen — {}", head(&text, 70));
    }

    /// `_flush_thought_pager()`: the pager's due events to the board.
    pub fn flush_thought_pager(self: &Arc<Self>) {
        let events = self.thought_pager.lock().poll(monotonic());
        for event in events {
            if let Value::Object(msg) = event.to_wire() {
                self.on_caption(msg);
            }
        }
    }

    /// `_thought_caption_loop()`: turn the pages of whatever thought is on screen, and take it down the moment eve
    /// stops exploring.
    pub async fn thought_caption_loop(self: &Arc<Self>) {
        let mut rx = self.shutdown_rx();
        while !self.shutting_down() {
            if shutdown_within(&mut rx, 0.1).await {
                return;
            }
            let busy = self.thought_pager.lock().busy();
            if busy && !self.explorer.lock().on_board() {
                self.clear_thought();
            } else {
                self.flush_thought_pager();
            }
        }
    }

    /// `_clear_thought()`: the screen back to normal.
    pub fn clear_thought(self: &Arc<Self>) {
        let events = self.thought_pager.lock().reset(monotonic());
        for event in events {
            if let Value::Object(msg) = event.to_wire() {
                self.on_caption(msg);
            }
        }
    }

    /// `_send_emote(e)`: the diary's appraisal of what the camera saw → the board's mood engine.
    pub async fn send_emote(self: &Arc<Self>, e: eve_diary::Emote) {
        if self.ble.connected() {
            self.ble.send(&eve_diary::build_emote_cmd(&e), None).await;
        }
    }

    /// `_take_snapshot()`: the diary wants a photo: ask the board for one full-size frame and wait for it, within
    /// SNAP_TIMEOUT_SECS. None when the board is away, busy with another snap, or silent (older firmware) — the
    /// caller then keeps the stream frame.
    pub async fn take_snapshot(self: &Arc<Self>) -> Option<eve_vision::Frame> {
        if !self.ble.connected() {
            return None;
        }
        let rx = {
            let mut st = self.st.lock();
            if st.snap_waiter.as_ref().is_some_and(|w| !w.is_closed()) {
                return None;
            }
            let (tx, rx) = tokio::sync::oneshot::channel();
            st.snap_waiter = Some(tx);
            rx
        };
        self.ble.send(&eve_vision::build_snap_cmd(), None).await;
        let got = tokio::time::timeout(Duration::from_secs_f64(SNAP_TIMEOUT_SECS), rx).await;
        self.st.lock().snap_waiter = None;
        match got {
            Ok(Ok(Ok(frame))) => Some(frame),
            Ok(Ok(Err(e))) => {
                warn!("diary: snap frame undecodable: {}", e.0);
                None
            }
            // the waiter was dropped unanswered
            Ok(Err(_)) => None,
            Err(_) => {
                info!(
                    "diary: snap not answered in {:.0} s (older firmware?) — keeping the stream frame",
                    SNAP_TIMEOUT_SECS
                );
                None
            }
        }
    }

    /// `_photo_for_owner(said)`: the voice's take_photo: one full-size frame from the board (or the newest
    /// streamed frame when it cannot snap), kept in the diary as a picture the owner asked for. The answer is what
    /// the picture shows. With no robot connected it is the Mac's own camera (`mac_photo`).
    pub async fn photo_for_owner(self: &Arc<Self>, said: &str) -> Obj {
        if !self.ble.connected() {
            return self.mac_photo(said).await;
        }
        let mut frame = self.take_snapshot().await;
        if frame.is_none() {
            if let Some(raw) = self.scene.newest_frame() {
                frame = eve_vision::decode_frame(&raw).ok();
            }
        }
        let Some(frame) = frame.filter(|f| f.fmt == "jpeg") else {
            return obj(json!({"ok": false, "reason": "the camera gave no picture just now"}));
        };
        let (yaw, pitch) = (py_round(self.head.yaw()), py_round(self.head.pitch()));
        let notes_dir = self.explore_cfg.notes_dir.clone();
        let notes = self.st.lock().notes.clone();
        let (rel, caption) = if let Some(notes) = notes {
            match notes.keep(Some(&frame), said, yaw, pitch).await {
                Ok(Some(rec)) => (rec.photo, rec.caption),
                Ok(None) => return obj(json!({"ok": false, "reason": "the picture could not be saved"})),
                Err(e) => {
                    warn!("diary: the picture could not be kept: {:#}", e);
                    return obj(json!({"ok": false, "reason": "the picture could not be saved"}));
                }
            }
        } else {
            // No diary model (no OPENROUTER_API_KEY for it): keep the file and a plain line.
            let when = chrono::Local::now().naive_local();
            let Some(rel) = eve_photos::save(&notes_dir, &when, 0, &frame.data, None) else {
                return obj(json!({"ok": false, "reason": "the picture could not be saved"}));
            };
            let caption = if said.is_empty() { "a picture you asked for".to_string() } else { said.to_string() };
            if let Err(e) = eve_explore::append_note(
                &notes_dir,
                &when,
                yaw,
                pitch,
                &format!("A picture you asked for: {caption}"),
                Some(&eve_photos::photo_line(&rel)),
            ) {
                warn!("diary: the picture's note could not be written: {}", e);
            }
            (rel, caption)
        };
        self.note_activity();
        info!("voice: photo on request -> {}", rel);
        obj(json!({"ok": true, "path": notes_dir.join(&rel).to_string_lossy(), "caption": caption,
                   "answer": format!("Kept it: {caption}")}))
    }

    /// `_mac_photo(said)`: take_photo without the robot (retired 2026-10-03): one still from the Mac's camera
    /// (mac_camera), kept on the same shelf as the robot's pictures. It is not written into the diary: the diary
    /// is the robot's notes, posed by its head. The caption is the owner's words; nothing looked at the picture.
    pub async fn mac_photo(self: &Arc<Self>, said: &str) -> Obj {
        let shot = eve_mac_camera::snap(self.environ(), eve_mac_camera::TIMEOUT_SECS).await;
        let jpeg = match shot {
            eve_mac_camera::Snap::Ok { jpeg } => jpeg,
            eve_mac_camera::Snap::Failed { reason } => {
                let reason =
                    if reason.is_empty() { "the Mac's camera gave no picture just now".to_string() } else { reason };
                return obj(json!({"ok": false, "reason": reason}));
            }
        };
        let notes_dir = self.explore_cfg.notes_dir.clone();
        let Some(rel) = eve_photos::save(&notes_dir, &chrono::Local::now().naive_local(), 0, &jpeg, None) else {
            return obj(json!({"ok": false, "reason": "the picture could not be saved"}));
        };
        let caption = if said.is_empty() { "a picture from the Mac's camera".to_string() } else { said.to_string() };
        self.note_activity();
        info!("voice: Mac camera photo on request -> {}", rel);
        obj(json!({"ok": true, "path": notes_dir.join(&rel).to_string_lossy(), "caption": caption,
                   "answer": format!("Took it with the Mac's camera: {caption}")}))
    }

    /// `_request_explore(reason)`: the owner asked (CLI or voice): start a manual explore now. Refused when the
    /// listen key or a disconnect stands in the way.
    pub async fn request_explore(self: &Arc<Self>, reason: &str) -> Result<(), eve_explore::ExploreRefused> {
        let listening = self.st.lock().listen_sent.unwrap_or(false);
        let connected = self.ble.connected();
        let actions = self.explorer.lock().request(monotonic(), reason, listening, connected)?;
        for action in actions {
            self.run_explore_action(action).await;
        }
        Ok(())
    }

    /// `_dismiss_explore(reason)`: end an explore now (a touch, a wake word, `explore stop`).
    pub async fn dismiss_explore(self: &Arc<Self>, reason: &str) {
        self.clear_thought();
        let actions = self.explorer.lock().dismiss(reason);
        for action in actions {
            self.run_explore_action(action).await;
        }
    }

    /// `_stop_explore(reason)`: shutdown path: leave explore mode on the board if we put it there.
    pub async fn stop_explore(self: &Arc<Self>, reason: &str) {
        self.dismiss_explore(reason).await;
        let notes = self.st.lock().notes.clone();
        if let Some(notes) = notes {
            notes.stop();
        }
    }

    /// `_on_listen_key(on)`: debounced edge from listen_key, delivered on the loop thread. Dedupe here,
    /// synchronously, so two fast edges cannot both read the same last-sent state and put duplicates on the wire.
    pub fn on_listen_key(self: &Arc<Self>, on: bool) {
        info!("listen key: {}", if on { "down" } else { "up" });
        self.st.lock().listen_down = on;
        self.note_activity();
        // Releasing the key while eve listens to a learner keeps the pose up.
        self.sync_listen_pose();
    }

    /// `_reset_listen()`: board (re)connected or rebooted: tell it the key is up. Its listen state lives in RAM,
    /// and a reboot mid-hold would otherwise leave the pose stuck until the next key press.
    pub async fn reset_listen(self: &Arc<Self>) {
        self.ble.send(&json!({"cmd": "listen", "on": false}), None).await;
        let listening = {
            let mut st = self.st.lock();
            st.listen_sent = Some(false);
            st.think_aloud_state == "on"
        };
        if listening {
            self.sync_listen_pose(); // eve is listening to a learner: raise the pose again
        }
    }

    /// `_update_check_loop()`: poll GitHub releases once at startup, then every 24 hours.
    ///
    /// Network calls happen on a thread so the loop isn't blocked by the sync IO. Failures are logged at DEBUG
    /// and we just retry next cycle — never raise, never crash the daemon.
    pub async fn update_check_loop(self: &Arc<Self>) {
        // First check is on startup (uses cache if fresh) so the hud has a
        // value to display immediately.
        let mut first = true;
        let mut rx = self.shutdown_rx();
        while !self.shutting_down() {
            let force = !first;
            let Ok(info) = tokio::task::spawn_blocking(move || eve_version_check::check(force)).await else {
                return;
            };
            let has_update = info.has_update();
            self.st.lock().update_available = if has_update { info.latest.clone() } else { None };
            if has_update && first {
                info!(
                    "update available: {} → {} (you're on the latest cached info; see https://github.com/SnowWarri0r/eve-bridge/releases)",
                    info.current,
                    info.latest.as_deref().unwrap_or("None")
                );
            }
            first = false;
            if shutdown_within(&mut rx, UPDATE_CHECK_INTERVAL_SECS).await {
                return;
            }
        }
    }
}
