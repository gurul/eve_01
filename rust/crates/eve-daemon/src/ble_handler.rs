//! What the board sends (`_handle_ble`), the transcript tailer's callbacks (`_on_tokens`, `_on_assistant_text`),
//! the turn end's side effects and the deferred running→0 flip, and the ack waiters (`expect_ack`,
//! `wait_for_ack`; [`eve_folder_push::PushTarget`] for the folder push).

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::daemon::Daemon;
use crate::board::{py_repr, py_repr_str, py_str, truthy};
use crate::seams::Obj;

impl Daemon {
    /// `_handle_ble(obj)`: one line from the board (or the Voice PE): acks, status, diag, frames, faces, poses,
    /// buttons, swipes, ptt, the listen key, the stick, music mode, explore, the snap waiter.
    pub async fn handle_ble(self: &Arc<Self>, obj: Obj) -> anyhow::Result<()> {
        if let Some(Value::Object(frame)) = obj.get("frame") {
            let frame = Value::Object(frame.clone());
            if frame.get("snap") == Some(&Value::Bool(true)) {
                // The photo we asked for: hand it to the waiting diary, never
                // to the face tracker (it is 320x240, and it is not a stream
                // frame). An unasked-for snap is dropped.
                let waiter = self.st.lock().snap_waiter.take();
                if let Some(waiter) = waiter {
                    if !waiter.is_closed() {
                        let _ = waiter.send(eve_vision::decode_frame(&frame));
                    }
                }
                return Ok(());
            }
            // Camera frame: hottest object on the link (5/s). Hand it to the
            // tracker, which parks or runs it and replies with the face cmd.
            // While exploring, also keep the newest one for the next waypoint
            // sample (the explore loop decodes it, once, when it is due).
            if self.explorer.lock().active() {
                self.st.lock().explore_raw_frame = Some(frame.clone());
            }
            // The head pose rides on every frame; the scene watcher keeps only the
            // newest frame, and only while a conversation is open.
            self.head
                .observe(frame.get("yaw").unwrap_or(&Value::Null), frame.get("pitch").unwrap_or(&Value::Null));
            self.scene.offer(frame.clone());
            self.vision.on_frame(&frame).await;
            return Ok(());
        }
        if let Some(expression @ Value::Object(_)) = obj.get("expression") {
            self.expressions.observe(expression);
            return Ok(());
        }
        if let Some(Value::Object(diag)) = obj.get("diag") {
            self.log_diag(diag);
            return Ok(());
        }
        let cmd = obj.get("cmd").and_then(Value::as_str).map(str::to_string);
        let cmd = cmd.as_deref();
        if cmd == Some("explore") {
            // Two quick taps on the screen: come back. The board only sends
            // this while it is exploring, so it always means stop.
            self.note_activity();
            tracing::info!("explore: called back by a double tap");
            self.dismiss_explore("double tap").await;
            return Ok(());
        }
        if cmd == Some("music") {
            // Spotify mode's input from the Voice PE (music_mode.py): clicks, holds and the dial.
            self.note_activity();
            let mode = self.st.lock().music_mode.clone();
            if let Some(mode) = mode {
                mode.on_input(&Value::Object(obj.clone())).await?;
            } else if let Some(ctl) = &self.controller {
                // no Spotify: a board stuck in the mode is put back
                ctl.show(&json!({"cmd": "music_mode", "on": false})).await;
            }
            return Ok(());
        }
        if cmd == Some("ptt") {
            // The board's button held to talk (Voice PE, desk_call.py): the Mini App's push-to-talk call, on
            // the desk. A wake-word conversation already has the mic, so a press during one is ignored. A
            // release always goes through: dropped, it left the mic of a press made before the conversation
            // open (verification/Eve/DeskCall.lean).
            self.note_activity();
            let on = truthy(obj.get("on"));
            self.st.lock().ptt_held = on;
            if on && self.conversation_open() {
                tracing::info!("board ptt during a conversation — ignored");
                return Ok(());
            }
            if let Some(calls) = self.desk_calls_get() {
                calls.press(on).await;
            }
            return Ok(());
        }
        if let Some(touch @ ("focus" | "key")) = cmd {
            // A touch on the board: the human is here, stop exploring — at
            // once, and even a manual explore, which ignores the idle clock.
            self.note_activity();
            self.dismiss_explore(&format!("touch ({touch})")).await;
            // ... but a touch during a conversation does nothing more. On
            // 2026-09-10 a phantom body-pad blip in the attention pose sent
            // focus and killed a live task; a terminal raise would also take
            // the frontmost app from a running computer-use task. The
            // conversation ends by voice (goodbye), not by a touch.
            if self.conversation_open() {
                tracing::info!("board touch ({}) during a conversation — ignored", touch);
                return Ok(());
            }
        }
        if cmd == Some("focus") {
            // A tap on the pet in attention state: raise the terminal of the
            // session that is waiting on the human (oldest pending permission,
            // else newest needs-input session).
            let cwd = self.state.lock().attention_cwd();
            tracing::info!(
                "focus: requested for {}",
                py_repr_str(if cwd.is_empty() { "(unknown session)" } else { &cwd })
            );
            let focus = self.seams.focus_terminal.clone();
            self.spawn("focus-terminal", focus(cwd));
            return Ok(());
        }
        if cmd == Some("key") {
            // Swipe-down on the pet → Enter on the host.
            let name = match obj.get("name") {
                v if !truthy(v) => String::new(),
                Some(v) => py_str(v),
                None => String::new(),
            };
            let mut st = self.st.lock();
            if st.keys.is_none() {
                st.keys = Some((self.seams.key_tapper)());
            }
            if let Some(keys) = st.keys.as_mut() {
                keys.tap(&name);
            }
            return Ok(());
        }
        // {"pose":{"y":tenths,"p":tenths}}: where the head actually is, ten times a
        // second, but only while a motion runs. It exists because the camera
        // frames that normally carry the pose are quarantined for the whole of a
        // movement, so this is the only way to measure a delivered swing.
        if let Some(Value::Object(pose)) = obj.get("pose") {
            if let (Some(yaw), Some(pitch)) = (py_number(pose.get("y")), py_number(pose.get("p"))) {
                let (yaw, pitch) = (yaw / 10.0, pitch / 10.0);
                self.head.observe(&json!(yaw), &json!(pitch));
                let mut st = self.st.lock();
                st.motion_trace.push((eve_state::monotonic(), yaw, pitch));
                let excess = st.motion_trace.len().saturating_sub(crate::MOTION_TRACE_MAX);
                st.motion_trace.drain(..excess);
            }
            return Ok(());
        }

        // Status acks come back from the device after we poll with {"cmd":"status"}.
        // Shape per REFERENCE.md: {"ack":"status","ok":true,"data":{"name","sec","bat":{...},"sys":{...},"stats":{...}}}.
        let ack = obj.get("ack").filter(|a| !a.is_null());
        let is_status = ack.and_then(Value::as_str) == Some("status");
        if is_status {
            // A status reply proves the write path works RIGHT NOW — but the
            // observed wedge pattern is flapping: a fresh reopen restores TX
            // for one exchange (63-183s) and then goes deaf again. Zeroing the
            // escalation on any single ack meant pulse_reset could never fire.
            // Only a streak of clean poll cycles (counted in the poller)
            // de-escalates; here we just mark the poll answered.
            //
            // Only an ack that answers an OUTSTANDING watchdog poll counts.
            // The {"cmd":"status"} in _send_resync goes out on every reconnect
            // and is answered by the same fresh link that then goes deaf; when
            // it counted, "resync ack + one poll ack" made the streak of two
            // after every reconnect and the RTS pulse never fired
            // (verification/Eve/Serial.lean, B).
            let mut st = self.st.lock();
            let answered_poll = st.status_sent_at.is_some();
            st.status_sent_at = None;
            st.status_missed = 0;
            if answered_poll {
                st.clean_polls += 1;
                if st.ack_escalation != 0 && st.clean_polls >= 2 {
                    tracing::info!(
                        "status acks stable for {} clean polls — resetting ack escalation (was {})",
                        st.clean_polls,
                        st.ack_escalation
                    );
                    st.ack_escalation = 0;
                }
            }
        }
        if is_status && truthy(obj.get("ok")) {
            let empty = Obj::new();
            let data = match obj.get("data") {
                Some(Value::Object(m)) => m,
                _ => &empty,
            };
            self.on_status_data(data);
            return Ok(());
        }

        // Route any ack (status already handled above, but others — char_begin,
        // file, chunk, file_end, char_end, name, owner, unpair, etc.) to the
        // oldest waiter that registered for that ack type. A waiter that names
        // an "n" takes only a successful ack carrying that n: a late ack from a
        // timed-out request must not answer the next one (firmware failure acks
        // carry n=0, so those still match).
        if ack.is_some() {
            self.acks.route(&Value::Object(obj));
            return Ok(());
        }

        if matches!(cmd, Some("name" | "owner" | "unpair" | "char_begin" | "char_end" | "file" | "file_end" | "chunk")) {
            // We're the central; we don't send these, but acknowledge defensively.
            return Ok(());
        }

        let shown = py_repr(&Value::Object(obj));
        tracing::debug!("ble: unhandled {}", shown);
        Ok(())
    }

    /// The `{"diag":{...}}` branch of `_handle_ble`: a crash/hang report from the board (see firmware diag.h).
    /// Logged at WARNING when the previous run died abnormally, because that line is the whole point: it says
    /// what the board was doing when it froze, which nothing else on this link can tell us.
    fn log_diag(&self, diag: &Obj) {
        let reset = diag.get("reset").cloned().unwrap_or_else(|| Value::from("?"));
        let abnormal = matches!(
            reset.as_str(),
            Some("PANIC" | "TASK-WATCHDOG" | "INT-WATCHDOG" | "BROWNOUT" | "other-watchdog")
        );
        self.st.lock().last_diag = Some(diag.clone());
        let field = |k: &str| py_str(diag.get(k).unwrap_or(&Value::Null));
        let line = format!(
            "board diag: boot #{} after {} (up={}s heap={} min={} psram={})",
            field("boot"),
            py_str(&reset),
            field("up"),
            field("heap"),
            field("minheap"),
            field("psram")
        );
        let log = |line: &str| {
            if abnormal {
                tracing::warn!("{}", line);
            } else {
                tracing::info!("{}", line);
            }
        };
        log(&line);
        // The single most valuable field after a watchdog reset: which
        // loop() phase never returned. The firmware has always sent it;
        // dropping it here is why a week of TASK-WATCHDOG resets never
        // named the hanging call.
        if diag.contains_key("diedIn") {
            log(&format!(
                "  died in: {} (entered {}ms, {} loops)",
                field("diedIn"),
                field("diedAtMs"),
                field("loops")
            ));
        }
        // `for ev in diag.get("last") or []`: a list's items, a string's characters, a dict's keys.
        let events: Vec<String> = match diag.get("last") {
            v if !truthy(v) => Vec::new(),
            Some(Value::Array(a)) => a.iter().map(py_str).collect(),
            Some(Value::String(s)) => s.chars().map(String::from).collect(),
            Some(Value::Object(m)) => m.keys().cloned().collect(),
            _ => Vec::new(),
        };
        for ev in events {
            log(&format!("  pre-reset event: {ev}"));
        }
    }

    /// The `"data"` of an answered `{"ack":"status","ok":true}`: link security, sound, battery, the filesystem.
    fn on_status_data(self: &Arc<Self>, data: &Obj) {
        // "sec" is the board's BLE encryption; over a USB cable there is nothing to sniff,
        // so leave it unknown (no warning, no HUD "UNSEC").
        let sec = if self.ble.wired() { None } else { data.get("sec").filter(|v| !v.is_null()) };
        if let Some(sec) = sec {
            let last = self.st.lock().last_stick_sec;
            // Python's `sec != self._last_stick_sec`: a bool or a number against True/False/None.
            let same = match (sec, last) {
                (Value::Bool(b), Some(l)) => *b == l,
                (Value::Number(n), Some(l)) => n.as_f64() == Some(if l { 1.0 } else { 0.0 }),
                _ => false,
            };
            if !same {
                tracing::info!(
                    "stick link: {} (was {})",
                    if truthy(Some(sec)) { "ENCRYPTED" } else { "UNENCRYPTED — transcript sniffable!" },
                    match last {
                        None => "None",
                        Some(true) => "True",
                        Some(false) => "False",
                    }
                );
                self.st.lock().last_stick_sec = Some(truthy(Some(sec)));
            }
        }
        // firmware with {"cmd":"sound"}; older boards omit it
        if let Some(Value::Bool(snd)) = data.get("snd") {
            let on = self.sound.lock().on;
            if *snd != on {
                tracing::info!(
                    "sound: board has sound {} but the owner chose {} — re-sending",
                    if *snd { "on" } else { "off" },
                    if on { "on" } else { "off" }
                );
                let me = self.clone();
                self.spawn("send-sound", async move { me.send_sound().await });
            }
        }
        if let Some(Value::Object(bat)) = data.get("bat") {
            if !bat.is_empty() {
                let pct = bat.get("pct").and_then(py_int);
                let ma = bat.get("mA").and_then(py_int);
                if let Some(pct) = pct {
                    if self.st.lock().last_stick_battery_pct != Some(pct) {
                        let charging = if ma.is_some_and(|m| m < 0) { "+" } else { " " };
                        tracing::info!("stick battery: {}% {}", pct, charging);
                        self.st.lock().last_stick_battery_pct = Some(pct);
                    }
                }
            }
        }
        if let Some(Value::Object(sys_info)) = data.get("sys") {
            let free = sys_info.get("fsFree").and_then(py_int);
            let total = sys_info.get("fsTotal").and_then(py_int);
            if let (Some(free), Some(total)) = (free, total) {
                if total == 0 {
                    // LittleFS isn't mounted. Firmware calls begin(false),
                    // so an un-formatted partition reports 0/0. push-character
                    // will fail with "have 0K" until the user factory-resets
                    // the stick (hold A → settings → reset → factory reset),
                    // which runs LittleFS.format().
                    tracing::error!(
                        "stick LittleFS appears unformatted (fsTotal=0). Run factory reset on the stick to format it; \
                         push-character will reject until then."
                    );
                } else {
                    tracing::info!(
                        "stick fs: {}/{} bytes free ({:.0}%)",
                        free,
                        total,
                        100.0 * free as f64 / total as f64
                    );
                }
            }
        }
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
        let _ = entries;
        self.state.lock().set_tokens(cumulative, today, cost_cumulative, cost_today);
        self.push_heartbeat(false).await;
        Ok(())
    }

    /// `_on_assistant_text(transcript_path, text, uuid)`: Claude's reply as an entry, and relayed to the text door
    /// when the relay is on.
    ///
    /// Fired by the JSONL tailer the moment a new assistant text record
    /// lands on disk (typically <500 ms after Claude Code finishes the
    /// message). Emitting here beats the Stop hook, so the stick receives
    /// the '@ ...' entry while the user is still looking at the terminal —
    /// before auto-off kicks in.
    ///
    /// Only a session eve's hooks know: the tailer reads every transcript
    /// under the roots, and a background Claude (claude-mem's observer,
    /// whose hooks never reach eve) once had its "Prompt is too long" error
    /// relayed to the phone as Claude's answer (2026-10-12). Claude Code
    /// names a transcript <session_id>.jsonl.
    pub async fn on_assistant_text(self: &Arc<Self>, transcript_path: &str, text: &str, uuid: &str) -> anyhow::Result<()> {
        let _ = uuid;
        let (cwd, count) = {
            let mut state = self.state.lock();
            let by_path = state.sessions.values().find(|s| s.transcript_path.as_deref() == Some(transcript_path));
            let session = by_path.or_else(|| state.sessions.get(session_id_of(transcript_path)));
            let Some(session) = session else {
                tracing::debug!("tailer: assistant text from an unknown session, not relayed");
                return Ok(());
            };
            let cwd = session.cwd.clone().unwrap_or_default();
            let entry =
                format!("@ {}", eve_protocol::truncate_utf8_bytes(text, crate::ENTRY_PAYLOAD_MAX_BYTES));
            state.add_entry(&entry, None);
            (cwd, state.entries.len())
        };
        tracing::info!("tailer: new assistant text → entry added (state.entries={})", count);
        self.push_heartbeat(true).await;
        let inlet = self.st.lock().telegram.clone();
        if let Some(inlet) = inlet {
            if inlet.claude() {
                let text = text.to_string();
                self.spawn("relay-text", async move { inlet.relay_text(&text, &cwd).await });
            }
        }
        Ok(())
    }

    /// `_turn_end_side_effects(celebrate_secs)`: the celebration and the heartbeat after a turn ends.
    ///
    /// Post-response work for a turn_end event, run as a background task.
    ///
    /// Runs *after* the IPC ``{"ok": True}`` reply has been sent, so the Stop
    /// hook's spawnSync call never waits on a BLE write.
    ///
    /// 1. Force-push the heartbeat carrying ``completed=true`` so the stick's
    ///    celebrate animation fires within ~50 ms of the turn ending.
    /// 2. Schedule a follow-up push at pulse end so ``completed`` flips back
    ///    to false on time rather than waiting for the next keepalive.
    pub async fn turn_end_side_effects(self: &Arc<Self>, celebrate_secs: f64) {
        self.push_heartbeat(true).await;
        let me = self.clone();
        self.spawn("heartbeat-after", async move { me.heartbeat_after(celebrate_secs + 0.1).await });
    }

    /// `_heartbeat_after(delay)`: a heartbeat `delay` seconds from now.
    ///
    /// Used by the celebrate-pulse logic to flush the completed=false transition right
    /// when the pulse expires, instead of waiting for the next keepalive.
    pub async fn heartbeat_after(self: &Arc<Self>, delay: f64) {
        tokio::time::sleep(Duration::from_secs_f64(delay.max(0.0))).await;
        self.push_heartbeat(true).await;
    }

    /// `_deferred_turn_end(session_id, delay)`: the running→0 flip after the grace window, unless a new turn
    /// began meanwhile.
    ///
    /// Delay the state.turn_end so that running>0 keeps the firmware out of
    /// clock mode long enough to render the @-entry the tailer just pushed.
    pub async fn deferred_turn_end(self: &Arc<Self>, session_id: String, delay: f64) {
        // Python's `finally`, which runs on a cancel too: self-cleanup. If a new turn_begin already replaced
        // the task, the entry is that task instead — ignore the mismatch.
        struct Cleanup(std::sync::Weak<Daemon>, String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                if let Some(d) = self.0.upgrade() {
                    let mut st = d.st.lock();
                    if st.pending_turn_ends.get(&self.1).is_some_and(|j| j.done()) {
                        st.pending_turn_ends.remove(&self.1);
                    }
                }
            }
        }
        let _cleanup = Cleanup(Arc::downgrade(self), session_id.clone());
        tokio::time::sleep(Duration::from_secs_f64(delay.max(0.0))).await;
        self.state.lock().turn_end(&session_id);
        self.push_heartbeat(false).await;
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

/// `isinstance(x, (int, float))` (a bool is an int) as a float.
fn py_number(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

/// `isinstance(x, int)` (a bool is an int).
fn py_int(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Number(n) => n.as_i64(),
        _ => None,
    }
}

/// `os.path.splitext(os.path.basename(path))[0]`: what Claude Code names a transcript, its session id.
fn session_id_of(path: &str) -> &str {
    let base = path.rsplit('/').next().unwrap_or(path);
    // splitext ignores leading dots: ".jsonl" has no extension.
    let lead = base.len() - base.trim_start_matches('.').len();
    match base[lead..].rfind('.') {
        Some(i) => &base[..lead + i],
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::session_id_of;

    #[test]
    fn session_id_is_the_transcripts_stem() {
        assert_eq!(session_id_of("/a/b/abc-123.jsonl"), "abc-123");
        assert_eq!(session_id_of("abc.x.jsonl"), "abc.x");
        assert_eq!(session_id_of("/a/.jsonl"), ".jsonl");
        assert_eq!(session_id_of("/a/noext"), "noext");
        assert_eq!(session_id_of("/a/b/"), "");
    }
}
