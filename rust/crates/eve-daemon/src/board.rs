//! The board side of the daemon: heartbeats, resync on connect, the status poller (the link's watchdog), the Mac
//! microphone switch, the camera, the sound, the tracker's stats and the owner's identity.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::daemon::Daemon;
use crate::seams::Obj;
use crate::{STATUS_MISSED_LIMIT, STATUS_POLL_INTERVAL_SECS};

/// Python's `%r` of a string: single quotes unless the text holds one and no double quote.
pub(crate) fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `%r` of a JSON value.
pub(crate) fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => py_repr_str(s),
        other => py_str(other),
    }
}

/// Python's `%s` (`str()`) of a JSON value.
pub(crate) fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() => {
                if f.fract() == 0.0 && f.is_finite() && f.abs() < 1e16 {
                    format!("{f:.1}")
                } else {
                    f.to_string()
                }
            }
            _ => n.to_string(),
        },
        Value::String(s) => s.clone(),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter().map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Python's truthiness of a JSON value (`bool(x)`; None when absent is falsy).
pub(crate) fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(m)) => !m.is_empty(),
    }
}

impl Daemon {
    /// `await asyncio.wait_for(self._shutdown.wait(), timeout=secs)`: true when shutdown was set (at once if it
    /// already is), false on the timeout.
    pub(crate) async fn wait_shutdown(&self, secs: f64) -> bool {
        let mut rx = self.shutdown_rx();
        let res = tokio::time::timeout(Duration::from_secs_f64(secs.max(0.0)), rx.wait_for(|set| *set)).await;
        matches!(res, Ok(Ok(_)))
    }

    /// `_heartbeat_loop()`: a heartbeat whenever the state changed, and a keepalive every HEARTBEAT_KEEPALIVE.
    pub async fn heartbeat_loop(self: &Arc<Self>) {
        while !self.shutting_down() {
            self.push_heartbeat(false).await;
            // Set or timed out, the loop's condition decides.
            self.wait_shutdown(eve_protocol::HEARTBEAT_KEEPALIVE).await;
        }
    }

    /// `_push_heartbeat(force)`: build the heartbeat from the state and send it when it changed (or `force`, or
    /// the keepalive is due); entries are encoded with the CJK codec when the firmware has one.
    pub async fn push_heartbeat(self: &Arc<Self>, force: bool) {
        let codec = self.cjk_codec.as_deref();
        let snap = match eve_protocol::build_heartbeat(&self.state.lock(), None, codec) {
            Ok(snap) => snap,
            Err(e) => {
                tracing::error!("heartbeat: cannot build the snapshot: {e}");
                return;
            }
        };
        let snap = Value::Object(snap);
        // Dedup key is the Python-side serialized snapshot, not the wire bytes
        // — same dict means same content regardless of wire codec.
        let serialized = eve_agentcard::py::dumps(&snap, true, false);
        let now = eve_state::monotonic();
        let (changed, stale) = {
            let st = self.st.lock();
            (
                st.last_hb_serialized.as_deref() != Some(serialized.as_str()),
                (now - st.last_hb_sent_at) >= eve_protocol::HEARTBEAT_KEEPALIVE,
            )
        };
        if !(force || changed || stale) {
            return;
        }
        if !self.ble.connected() {
            return;
        }
        // Monitor build: what actually went on the wire for the agent
        // table. Keep this — "the board shows 0 sessions" has to be
        // separable from "the daemon sent 0 sessions", and reading that
        // off a photo of an e-ink panel is not a diagnosis.
        if tracing::enabled!(tracing::Level::DEBUG) {
            let agents: Vec<String> = snap
                .get("agents")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|r| {
                            format!(
                                "{}:{}",
                                py_str(r.get("n").unwrap_or(&Value::Null)),
                                py_str(r.get("s").unwrap_or(&Value::Null))
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            tracing::debug!(
                "hb->board: total={} running={} waiting={} agents=[{}]",
                snap["total"],
                snap["running"],
                snap["waiting"],
                agents.iter().map(|a| py_repr_str(a)).collect::<Vec<_>>().join(", ")
            );
            let entries = snap.get("entries").and_then(Value::as_array).cloned().unwrap_or_default();
            tracing::debug!(
                "heartbeat: {} bytes, entries={} (last={}), force={}, changed={}",
                serialized.chars().count(),
                entries.len(),
                entries.last().map(py_repr).unwrap_or_else(|| "None".into()),
                if force { "True" } else { "False" },
                if changed { "True" } else { "False" },
            );
        }
        let ok = self.ble.send(&snap, codec).await;
        if ok {
            let mut st = self.st.lock();
            st.last_hb_serialized = Some(serialized);
            st.last_hb_sent_at = now;
        } else {
            tracing::warn!("heartbeat: ble.send returned failure");
        }
    }

    /// `_resync_board()`: everything the board forgets on a reconnect, now.
    ///
    /// Re-run the on-connect resync after a board reboot on a live link:
    /// time sync, forced heartbeat, status poll. The board buffers serial RX
    /// during its boot-time panel clear, so no settling delay is needed.
    pub async fn resync_board(self: &Arc<Self>) {
        tracing::info!("board rebooted under a live link — resyncing time + state");
        self.send_resync().await;
    }

    /// `_send_resync()`: time sync, the forced heartbeat, the camera, the sound, the listen pose, the mode.
    ///
    /// What a board needs after a reboot or a (re)connect, in this order (test_vision pins it): the time,
    /// a forced heartbeat, a status poll, then listening, the agent's face, the camera and the sound. One
    /// copy for both paths, so a step added for one is never missing from the other.
    pub async fn send_resync(self: &Arc<Self>) {
        self.ble.send(&Value::Object(eve_protocol::build_time_sync()), None).await;
        self.push_heartbeat(true).await;
        self.ble.send(&json!({"cmd": "status"}), None).await;
        self.reset_listen().await;
        self.resync_agent().await;
        self.send_cam(true).await;
        self.send_sound().await;
    }

    /// `_on_ble_connected()`: the link came up (each time): resync and the agent's phase.
    ///
    /// On every (re)connect, emit time sync + force a heartbeat + kick
    /// a status poll so we learn the link's encryption state right away.
    pub async fn on_ble_connected(self: &Arc<Self>) {
        while !self.shutting_down() {
            self.ble.wait_connected().await;
            // Guard against a stale connected-event: if we woke but the link is
            // already gone (the event was set but cleared a beat later, or a
            // teardown race), don't fire sends into a dead link and spin. Sleep
            // briefly and re-wait instead of looping with no delay.
            if !self.ble.connected() {
                tokio::time::sleep(Duration::from_secs_f64(0.5)).await;
                continue;
            }
            self.send_resync().await;
            self.apply_mic("the robot connected");
            // Wait for the connection to drop before waiting again.
            while self.ble.connected() && !self.shutting_down() {
                tokio::time::sleep(Duration::from_secs_f64(1.0)).await;
            }
            self.apply_mic("the robot is not connected");
        }
    }

    /// `_mic_wanted()`: the Mac microphone should be listening (the switch is on and nothing suppresses it).
    ///
    /// The mic policy: the owner's switch is on, and the robot is connected
    /// (or EVE_MIC_ALWAYS=1 says to listen from boot).
    pub fn mic_wanted(&self) -> bool {
        self.mic.lock().on && (self.ears_cfg.always || self.ble.connected())
    }

    /// `_apply_mic(why)`: start or stop the ears to match `mic_wanted`, logging `why`.
    ///
    /// Open or close the Mac microphone to match _mic_wanted. Called at boot,
    /// on every robot connect and drop, and when the owner flips the switch.
    pub fn apply_mic(&self, why: &str) {
        let Some(ears) = self.st.lock().ears.clone() else {
            return;
        };
        let want = self.mic_wanted();
        if want && !ears.listening() {
            if !ears.start() {
                tracing::warn!("ears: the microphone did not open ({}); the wake word is off until it does", why);
            }
        } else if !want && ears.listening() {
            ears.stop();
            tracing::info!("ears: microphone closed — {}", why);
        }
    }

    /// `_mic_status()`: {"mic": on, "listening": ...} for IPC and the Mini App.
    pub fn mic_status(&self) -> Obj {
        let ears = self.st.lock().ears.clone();
        let listening = ears.as_ref().is_some_and(|e| e.listening());
        let on = self.mic.lock().on;
        let mut out = Obj::new();
        out.insert("ok".into(), Value::Bool(true));
        out.insert("mic".into(), Value::from(if on { "on" } else { "off" }));
        out.insert("listening".into(), Value::Bool(listening));
        out.insert("connected".into(), Value::Bool(self.ble.connected()));
        out.insert("always".into(), Value::Bool(self.ears_cfg.always));
        out.insert("available".into(), Value::Bool(ears.is_some()));
        out
    }

    /// `_status_poller()`: {"cmd":"status"} every STATUS_POLL_INTERVAL_SECS; STATUS_MISSED_LIMIT unanswered in a
    /// row reconnects (serial: pulses reset, then forces a reconnect, escalating).
    ///
    /// Poll the stick for status, and use the replies as a TX health check.
    ///
    /// The status ack is the only thing that proves host→board writes are
    /// landing. A half-dead link — reads fine, writes silently go nowhere —
    /// keeps RX_SILENCE_SECS happy (the board is still talking) while the board
    /// shows "No Claude connected" because no heartbeat ever reaches it.
    /// Missing acks are the only signal, so treat them as one.
    pub async fn status_poller(self: &Arc<Self>) {
        while !self.shutting_down() {
            if self.wait_shutdown(STATUS_POLL_INTERVAL_SECS).await {
                return;
            }
            self.status_poll_once().await;
        }
    }

    /// One pass of `_status_poller`'s loop after its wait: count a missed poll and escalate, or send the next.
    pub async fn status_poll_once(self: &Arc<Self>) {
        if !self.ble.connected() {
            return;
        }
        let escalate = {
            let mut st = self.st.lock();
            if st.status_sent_at.is_some() {
                st.status_missed += 1;
                // A missed poll ends the clean streak: de-escalation needs
                // CONSECUTIVE answered polls, or a link that answers every
                // other poll would keep resetting the escalation.
                st.clean_polls = 0;
                if st.status_missed >= STATUS_MISSED_LIMIT {
                    st.status_missed = 0;
                    st.status_sent_at = None;
                    // Escalate: first a plain reconnect (which now holds the
                    // port closed for a while — the only host-side action that
                    // ever revived this wedge); if acks are STILL missing a
                    // cycle later, the wedge is board-side (its RX died while
                    // TX kept going) and we pulse RTS to reboot it. The
                    // escalation counter survives a lone post-reopen ack —
                    // the field pattern is a flap, not a clean recovery — and
                    // only a streak of clean polls (see the ack handler)
                    // resets it.
                    st.ack_escalation += 1;
                    st.clean_polls = 0;
                    tracing::info!("ack escalation → {}", st.ack_escalation);
                    Some(st.ack_escalation)
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some(level) = escalate {
            let why = format!(
                "no status ack for {} polls (~{}s) — writes are not reaching the board",
                STATUS_MISSED_LIMIT,
                (f64::from(STATUS_MISSED_LIMIT) * STATUS_POLL_INTERVAL_SECS) as i64
            );
            if level >= 2 {
                let reason = format!("reconnect did not restore acks; {why}");
                if self.ble.pulse_reset(&reason) {
                    let mut st = self.st.lock();
                    st.ack_escalation = 0;
                    st.clean_polls = 0;
                    return;
                }
            }
            self.ble.force_reconnect(&why);
            return;
        }
        self.st.lock().status_sent_at = Some(eve_state::monotonic());
        // Re-sync time with every poll. The board's RTC-valid flag is
        // RAM-only, so any watchdog reset blanks the mini clock; a reboot
        // that slips past silence detection (fast reboot, half-dead link)
        // never refires the on-connect time sync, leaving the clock gone
        // until the next reconnect. Periodic sync makes it self-heal
        // within one poll and corrects RTC drift for free.
        self.ble.send(&Value::Object(eve_protocol::build_time_sync()), None).await;
        self.ble.send(&json!({"cmd": "status"}), None).await;
    }

    /// `_send_cam(on)`: the camera command (vision.build_cam_cmd).
    ///
    /// Ask the board to start/stop streaming frames. Only when a detector
    /// exists: a board streaming at a host that cannot look is wasted link
    /// time, and the board keeps its own tracking while no host asks.
    pub async fn send_cam(self: &Arc<Self>, on: bool) {
        if !self.vision.enabled() || !self.ble.connected() {
            return;
        }
        let cmd = eve_vision::build_cam_cmd(on);
        if tokio::time::timeout(Duration::from_secs_f64(2.0), self.ble.send(&cmd, None)).await.is_err() {
            tracing::warn!("vision: cam {} command timed out", if on { "on" } else { "off" });
        }
    }

    /// `_send_sound()`: the board's sound to match the mute switch.
    ///
    /// Tell the board the owner's choice. Its firmware gates every chirp on it.
    pub async fn send_sound(self: &Arc<Self>) {
        if self.ble.connected() {
            let on = self.sound.lock().on;
            self.ble.send(&eve_sound::build_sound_cmd(on), None).await;
        }
    }

    /// `_set_sound(on)`: flip the mute switch, persist it, and send it.
    ///
    /// The owner muted or unmuted eve (by voice or `eve-bridge sound`).
    /// Persisted, and the board is told at once. Motion and LEDs are untouched.
    pub fn set_sound(&self, on: bool) {
        if self.sound.lock().set(on) {
            tracing::info!("sound: {} (the head and lights keep moving)", if on { "on" } else { "muted" });
        }
        if self.ble.connected() {
            let me = self.me();
            self.spawn("send-sound", async move { me.send_sound().await });
        }
    }

    /// `_vision_stats_loop()`: the tracker's stats line every STATS_INTERVAL_SECS while it is enabled.
    ///
    /// One summary line per window, and only when frames arrived. Never per-frame.
    pub async fn vision_stats_loop(self: &Arc<Self>) {
        while !self.shutting_down() {
            tokio::time::sleep(Duration::from_secs_f64(eve_vision::STATS_INTERVAL_SECS)).await;
            if let Some(line) = self.vision.stats_line() {
                tracing::info!("{}", line);
            }
        }
    }

    /// `identity_default_path()` read from the daemon's environment.
    fn identity_path(&self) -> std::path::PathBuf {
        eve_identity::default_path_from(self.env("EVE_OWNER_PRINTS").as_deref(), self.env("XDG_CONFIG_HOME").as_deref())
    }

    /// `configured_threshold()` read from the daemon's environment.
    fn identity_threshold(&self) -> f64 {
        eve_identity::configured_threshold_from(self.env("EVE_OWNER_THRESHOLD").as_deref())
    }

    /// `_make_identity()`: the owner's prints and the Vision describer, None when identity is off.
    ///
    /// Load the owner prints and wire the Vision describer. None when
    /// this host cannot produce feature prints — faces stay 'unknown'.
    pub fn make_identity(&self) -> Option<Arc<eve_identity::FaceIdentity>> {
        if !self.vision.enabled() {
            return None;
        }
        let describe = eve_identity::make_describer()?;
        let core = eve_identity::OwnerIdentity::new()
            .with_path(self.identity_path())
            .with_threshold(self.identity_threshold());
        let n = core.load(None);
        tracing::info!(
            "identity: {} owner print(s) loaded from {} (threshold {:.2})",
            n,
            core.path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "None".into()),
            core.threshold
        );
        let me = self.me.clone();
        let listen_down: eve_identity::ListenDown =
            Arc::new(move || me.upgrade().is_some_and(|d| d.st.lock().listen_down));
        Some(Arc::new(eve_identity::FaceIdentity::new(Arc::new(core), describe).with_listen_down(listen_down)))
    }

    /// `_handle_identity(action)`: "status", "enroll", "forget" over IPC.
    ///
    /// Loop thread. The core is lock-guarded, so this is safe against a
    /// classify/enrol in flight on the vision executor.
    pub fn handle_identity(&self, action: &str) -> Obj {
        let identity = self.st.lock().identity.clone();
        let mut out = Obj::new();
        if action == "reset" {
            let removed = match &identity {
                None => {
                    let core = eve_identity::OwnerIdentity::new().with_path(self.identity_path());
                    core.load(None);
                    core.reset()
                }
                Some(id) => {
                    let removed = id.core.reset();
                    id.set_last_who(None);
                    removed
                }
            };
            let removed = match removed {
                Ok(n) => n,
                // Python lets the OSError out of the handler: the IPC reply names it.
                Err(e) => {
                    out.insert("ok".into(), Value::Bool(false));
                    out.insert("error".into(), Value::from(format!("OSError: {e}")));
                    return out;
                }
            };
            tracing::info!("identity: reset — {} owner print(s) removed", removed);
            out.insert("ok".into(), Value::Bool(true));
            out.insert("removed".into(), Value::from(removed));
            return out;
        }
        let status = match &identity {
            None => {
                let core = eve_identity::OwnerIdentity::new()
                    .with_path(self.identity_path())
                    .with_threshold(self.identity_threshold());
                core.load(None);
                let none: eve_identity::Describer = Arc::new(|_f, _r| Ok(None));
                eve_identity::FaceIdentity::new(Arc::new(core), none).status()
            }
            Some(id) => id.status(),
        };
        out.insert("ok".into(), Value::Bool(true));
        out.insert("enabled".into(), Value::Bool(identity.is_some()));
        out.insert("identity".into(), status);
        out
    }
}
