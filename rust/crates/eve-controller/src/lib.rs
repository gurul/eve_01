//! The controller: a second board, next to the robot, that only takes input and shows state.
//!
//! Port of `eve_bridge/controller.py`.
//!
//! The owner asked on 2026-09-26 for the StackChan to stay the robot (face, head, camera) while a Home
//! Assistant Voice PE (firmware/eve_voice_pe, docs/voice-pe.md) becomes the control: hold its button to talk
//! (desk_call.py), tap it for Enter on a waiting prompt, turn its dial through the choices. Both boards are
//! ESP32-S3s on the same `/dev/cu.usbmodem*` glob, so the controller is named by its USB serial number,
//! `EVE_CONTROLLER_SERIAL` (an ESP32-S3's is its MAC, printed by `python -m serial.tools.list_ports -v`),
//! and the robot's glob skips it.
//!
//! The robot link stays the daemon's one link (`Daemon.ble`): every status poll, ack, watchdog and resync is
//! the robot's. The controller gets a copy of what it can show — the heartbeat, the time, the conversation
//! state and the sound setting — and its lines go back to the daemon only when they are input: `ptt`,
//! `key`, `focus`. Its acks never reach the robot's ack waiters.

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, LazyLock, Weak};
use std::time::Duration;

use eve_serial_transport::{BootHook, EveSerial, IncomingHandler, USB_SERIAL_PREFIX};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use tokio::task::JoinHandle;
use tracing::info;

#[cfg(test)]
mod tests;

/// A boxed future, as the link seam returns.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What the controller sends that the daemon acts on. Everything else from it (acks, chatter) stays here.
/// `music`: Spotify mode's clicks, holds, dial.
pub const INPUT_CMDS: [&str; 4] = ["ptt", "key", "focus", "music"];
/// Commands mirrored from the robot link. Heartbeats (no "cmd", a "total") and time sync are mirrored too.
pub const MIRRORED_CMDS: [&str; 2] = ["agent", "listen"];
/// `SILENT`: sent first on every (re)connect, so the beeps stay the robot's alone.
pub static SILENT: LazyLock<Value> = LazyLock::new(|| json!({"cmd": "sound", "on": false}));

/// `controller_serial(environ=None)`: `EVE_CONTROLLER_SERIAL`, stripped; `None` when unset or blank.
/// `None` for `environ` reads the process environment.
pub fn controller_serial(environ: Option<&HashMap<String, String>>) -> Option<String> {
    let raw = match environ {
        Some(env) => env.get("EVE_CONTROLLER_SERIAL").cloned(),
        None => std::env::var("EVE_CONTROLLER_SERIAL").ok(),
    }
    .unwrap_or_default();
    let raw = py_strip(&raw);
    (!raw.is_empty()).then(|| raw.to_string())
}

/// Python's `str.strip()` with no argument: Unicode whitespace plus the separators U+001C..U+001F.
fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

/// `obj.get("cmd")` as Python sees it: a JSON `null` is the same as no key.
fn cmd_of(obj: &Value) -> Option<&Value> {
    obj.get("cmd").filter(|v| !v.is_null())
}

fn cmd_in(cmd: Option<&Value>, set: &[&str]) -> bool {
    cmd.and_then(Value::as_str).is_some_and(|c| set.contains(&c))
}

fn has_key(obj: &Value, key: &str) -> bool {
    obj.as_object().is_some_and(|m| m.contains_key(key))
}

/// `mirrored(obj)`: whether a message for the robot is one the controller shows too.
pub fn mirrored(obj: &Value) -> bool {
    match cmd_of(obj) {
        Some(cmd) => cmd_in(Some(cmd), &MIRRORED_CMDS),
        None => has_key(obj, "total") || has_key(obj, "time"),
    }
}

/// The surface of `EveSerial` the controller uses. The seam the tests' fake link implements, as the Python
/// tests pass `link_factory`.
pub trait ControllerLink: Send + Sync + 'static {
    /// The port pattern the link was made with (`link.port`).
    fn port(&self) -> String;
    /// `link.connected`.
    fn connected(&self) -> bool;
    /// `link.wait_connected()`.
    fn wait_connected(&self) -> BoxFuture<'_, ()>;
    /// `link.send(obj)`. `Err` where Python's send raises.
    fn send<'a>(&'a self, obj: &'a Value) -> BoxFuture<'a, anyhow::Result<bool>>;
    /// `link.run()`: owned, so it can be spawned.
    fn run(&self) -> BoxFuture<'static, ()>;
    /// `link.stop()`.
    fn stop(&self) -> BoxFuture<'_, ()>;
    /// `link.on_boot = hook`.
    fn set_on_boot(&self, hook: Option<BootHook>);
}

impl ControllerLink for EveSerial {
    fn port(&self) -> String {
        self.port_pattern().to_string()
    }

    fn connected(&self) -> bool {
        EveSerial::connected(self)
    }

    fn wait_connected(&self) -> BoxFuture<'_, ()> {
        Box::pin(EveSerial::wait_connected(self))
    }

    fn send<'a>(&'a self, obj: &'a Value) -> BoxFuture<'a, anyhow::Result<bool>> {
        Box::pin(async move { Ok(EveSerial::send(self, obj, None).await?) })
    }

    fn run(&self) -> BoxFuture<'static, ()> {
        let link = self.clone();
        Box::pin(async move { link.run().await })
    }

    fn stop(&self) -> BoxFuture<'_, ()> {
        Box::pin(EveSerial::stop(self))
    }

    fn set_on_boot(&self, hook: Option<BootHook>) {
        EveSerial::set_on_boot(self, hook)
    }
}

/// `link_factory(on_message=..., port=...)`.
pub type LinkFactory = dyn Fn(IncomingHandler, &str) -> Arc<dyn ControllerLink>;

/// The default factory: `EveSerial(on_message=..., port=...)` with its defaults (115200 baud, no skips).
pub fn eve_serial_link(on_message: IncomingHandler, port: &str) -> Arc<dyn ControllerLink> {
    Arc::new(EveSerial::new(on_message, port, 115_200, BTreeSet::new()))
}

struct Shared {
    link: Arc<dyn ControllerLink>,
    /// The latest of each mirrored kind, replayed on connect (insertion order kept, as a Python dict).
    last: Mutex<Map<String, Value>>,
}

impl Shared {
    async fn send_quietly(&self, obj: &Value) {
        // contextlib.suppress(Exception): a failed send to the controller never reaches the robot's path.
        let _ = self.link.send(obj).await;
    }

    async fn replay(&self) {
        let mut objs = vec![SILENT.clone()];
        objs.extend(self.last.lock().values().cloned());
        for obj in &objs {
            self.send_quietly(obj).await;
        }
    }
}

/// `Controller`: the controller board's own serial link: input in, a mirror of the robot's state out.
pub struct Controller {
    pub serial_number: String,
    pub on_input: IncomingHandler,
    shared: Arc<Shared>,
}

/// Cancels the link's task when `run()` ends or is dropped (Python's `finally`).
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Controller {
    /// `Controller(serial_number, on_input)` on a real `EveSerial` link.
    pub fn new(serial_number: &str, on_input: IncomingHandler) -> Controller {
        Self::with_link_factory(serial_number, on_input, &eve_serial_link)
    }

    /// `Controller(serial_number, on_input, link_factory=...)`.
    pub fn with_link_factory(serial_number: &str, on_input: IncomingHandler, link_factory: &LinkFactory) -> Controller {
        let handler = on_input.clone();
        let on_message: IncomingHandler = Arc::new(move |obj: Map<String, Value>| {
            let handler = handler.clone();
            Box::pin(async move {
                if cmd_in(obj.get("cmd").filter(|v| !v.is_null()), &INPUT_CMDS) {
                    handler(obj).await;
                }
            })
        });
        let port = format!("{USB_SERIAL_PREFIX}{serial_number}");
        let shared = Arc::new_cyclic(|weak: &Weak<Shared>| {
            let link = link_factory(on_message, &port);
            let weak = weak.clone();
            let hook: BootHook = Arc::new(move || {
                let weak = weak.clone();
                Box::pin(async move {
                    if let Some(shared) = weak.upgrade() {
                        shared.replay().await;
                    }
                })
            });
            link.set_on_boot(Some(hook));
            Shared { link, last: Mutex::new(Map::new()) }
        });
        Controller { serial_number: serial_number.to_string(), on_input, shared }
    }

    /// The board's link (`self.link`).
    pub fn link(&self) -> &Arc<dyn ControllerLink> {
        &self.shared.link
    }

    /// A copy of the kept state (`self.last`).
    pub fn last(&self) -> Map<String, Value> {
        self.shared.last.lock().clone()
    }

    /// `connected`.
    pub fn connected(&self) -> bool {
        self.shared.link.connected()
    }

    /// `mirror(obj)`: a message the robot was sent: keep it for the next connect, and send it now if it is
    /// shown.
    pub async fn mirror(&self, obj: &Value) {
        if !mirrored(obj) {
            return;
        }
        // mirrored() let it through, so a present cmd is one of MIRRORED_CMDS (a string).
        let kind = match cmd_of(obj).and_then(Value::as_str).filter(|c| !c.is_empty()) {
            Some(cmd) => cmd.to_string(),
            None if has_key(obj, "time") => "time".to_string(),
            None => "heartbeat".to_string(),
        };
        self.shared.last.lock().insert(kind, obj.clone());
        if self.connected() {
            self.shared.send_quietly(obj).await;
        }
    }

    /// `show(obj)`: a message for this board only (Spotify mode, music_mode.py). `music_mode` is kept and
    /// replayed on the next connect, so a board that reboots comes back green; ring levels and flashes are
    /// only sent.
    pub async fn show(&self, obj: &Value) {
        if cmd_of(obj).and_then(Value::as_str) == Some("music_mode") {
            self.shared.last.lock().insert("music_mode".to_string(), obj.clone());
        }
        if self.connected() {
            self.shared.send_quietly(obj).await;
        }
    }

    /// `send_audio(obj)`: eve's voice for the controller's speaker (desk_call.BoardSpeaker): sent only to
    /// this board. A send error reaches the caller.
    pub async fn send_audio(&self, obj: &Value) -> anyhow::Result<()> {
        if self.connected() {
            self.shared.link.send(obj).await?;
        }
        Ok(())
    }

    /// `_replay()`: SILENT, then the latest of each kept kind.
    pub async fn replay(&self) {
        self.shared.replay().await;
    }

    /// `run()`: the link's own reconnect loop, plus a replay of the current state each time it
    /// (re)connects.
    pub async fn run(&self) {
        info!("controller: looking for the board with USB serial {}", self.serial_number);
        let link = &self.shared.link;
        let mut link_task = AbortOnDrop(tokio::spawn(link.run()));
        let mut joined = false;
        while !link_task.0.is_finished() {
            tokio::select! {
                _ = link.wait_connected() => {}
                // The link's run() only returns when it was stopped; don't wait for a connect that can't come.
                _ = &mut link_task.0 => {
                    joined = true;
                    break;
                }
            }
            info!("controller: connected");
            self.replay().await;
            while self.connected() && !link_task.0.is_finished() {
                tokio::time::sleep(Duration::from_secs_f64(1.0)).await;
            }
            if !link_task.0.is_finished() {
                info!("controller: disconnected");
                tokio::time::sleep(Duration::from_secs_f64(0.5)).await; // the link clears its connected flag as it closes
            }
        }
        // finally: cancel the link's task and wait for it (errors swallowed, as gather(return_exceptions)).
        if !joined {
            link_task.0.abort();
            let _ = (&mut link_task.0).await;
        }
    }

    /// `stop()`.
    pub async fn stop(&self) {
        self.shared.link.stop().await;
    }
}
