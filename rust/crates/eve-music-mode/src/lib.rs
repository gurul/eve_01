//! Spotify mode on the Voice PE: the board becomes spotKnob.
//!
//! Port of `eve_bridge/music_mode.py`.
//!
//! The owner asked on 2026-09-27: "when spotify mode is on voice pe leds should be green and the rotary board
//! should become volume", "pause on spotify is one click, skip is two clicks, back is three clicks", "change
//! device is hold", and exit on a long hold (3 s; two clicks were already skip). Turned on by saying or texting
//! "spotify mode" (the code path and the tools in spotify.py) or from the Mini App's Music card.
//!
//! The firmware (firmware/eve_voice_pe) stays thin: in music mode it breathes green, counts clicks, times holds
//! and passes the dial through, as `{"cmd":"music", ...}` lines. Everything that needs Spotify is here:
//!
//! | On the board | What happens |
//! |---|---|
//! | 1 click | play / pause |
//! | 2 clicks | next track |
//! | 3 clicks | previous track |
//! | dial | Spotify volume, [`VOLUME_PER_DETENT`]% a detent; the ring shows it in green at once and the write goes to Spotify [`VOLUME_DEBOUNCE_SECS`] after the dial stops (spotKnob's rule: one write per detent gets the account rate-limited) |
//! | hold, let go before 3 s | the device picker: the dial moves through the Spotify Connect devices (the ring takes the device's own colour, spotify.DeviceColors, with a bright dot for its place; eve says its name on the Voice PE), a click plays there, a hold or [`PICKER_IDLE_SECS`] without input backs out, nothing changed |
//! | hold 3 s | leave Spotify mode (the ring goes back to eve's) |
//!
//! A refusal (no device, not Premium) blinks the ring red with the "no" chirp. Hold to talk and
//! Enter-on-a-waiting-prompt are the button's again once the mode is off. The mode does not survive a daemon
//! restart.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use tokio::task::{AbortHandle, JoinHandle};

/// The Voice PE's dial has fewer, heavier detents than spotKnob's (2%).
pub const VOLUME_PER_DETENT: i64 = 4;
/// spotKnob's debounce.
pub const VOLUME_DEBOUNCE_SECS: f64 = 0.4;
/// spotKnob's picker times out the same.
pub const PICKER_IDLE_SECS: f64 = 10.0;
/// A name is said once the dial rests this long, not for every detent.
pub const PICKER_SAY_SECS: f64 = 0.35;

/// Python's `Send`: a message to the Voice PE only (`Controller.show`).
pub type SendFn = Arc<dyn Fn(Value) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;
/// Python's `Say`: eve's voice on the Voice PE.
pub type SayFn = Arc<dyn Fn(String) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;
/// The injectable sleep (Python's `sleep=asyncio.sleep` parameter), in seconds.
pub type SleepFn = Arc<dyn Fn(f64) -> BoxFuture<'static, ()> + Send + Sync>;

/// What MusicMode needs of `spotify.Spotify`: its `now_playing()` and `control(action, **kw)`.
///
/// Both return the same free-form dicts the Python does (`{"ok": ..., "playing": ..., "device": {...},
/// "devices": [...]}` and `{"ok": ..., "line": ...}`); `kw` carries the keyword arguments (`volume`, `device`).
#[async_trait]
pub trait SpotifyControl: Send + Sync {
    async fn now_playing(&self) -> anyhow::Result<Value>;
    async fn control(&self, action: &str, kw: Map<String, Value>) -> anyhow::Result<Value>;
}

/// The default sleep: tokio's timer.
pub fn tokio_sleep() -> SleepFn {
    Arc::new(|secs: f64| -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(Duration::from_secs_f64(secs.max(0.0))))
    })
}

// -- Python value semantics the module leans on --

/// Python truthiness of a JSON value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `isinstance(v, int) and not isinstance(v, bool)`: a JSON integer (any JSON integer, as Python's ints are
/// unbounded; `i128` holds every `i64` and `u64`).
fn py_int(v: Option<&Value>) -> Option<i128> {
    match v {
        Some(Value::Number(n)) => n.as_i64().map(i128::from).or_else(|| n.as_u64().map(i128::from)),
        _ => None,
    }
}

/// An int volume as the shadow holds it; past `i64` it saturates, which the 0..100 clamp makes exact.
fn sat_i64(n: i128) -> i64 {
    i64::try_from(n).unwrap_or(if n < 0 { i64::MIN } else { i64::MAX })
}

/// `isinstance(v, int)`, bools included (Python's `True` is the int 1): the volume a device reports.
fn py_int_or_bool(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Bool(b)) => Some(i64::from(*b)),
        other => py_int(other).map(sat_i64),
    }
}

/// `v == n` with Python's numeric equality (`1.0 == 1`, `True == 1`).
fn py_eq_int(v: Option<&Value>, n: i64) -> bool {
    match v {
        Some(Value::Number(x)) => match x.as_i64() {
            Some(i) => i == n,
            None => x.as_f64() == Some(n as f64),
        },
        Some(Value::Bool(b)) => i64::from(*b) == n,
        _ => false,
    }
}

/// `str(v)` for the values that reach a log line or eve's voice.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        other => other.to_string(),
    }
}

/// `(d or {}).get(key)`.
fn get<'a>(d: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    d.and_then(|d| d.get(key))
}

/// `int(s, 16)` as Python parses it: surrounding whitespace, a sign, an optional `0x`, single underscores
/// between digits.
fn py_int_base16(s: &str) -> Option<i64> {
    let s = s.trim();
    let (neg, rest) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let rest = match rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
        Some(r) => r.strip_prefix('_').unwrap_or(r),
        None => rest,
    };
    if rest.is_empty() || rest.starts_with('_') || rest.ends_with('_') || rest.contains("__") {
        return None;
    }
    let mut n: i64 = 0;
    for c in rest.chars() {
        if c == '_' {
            continue;
        }
        n = n.checked_mul(16)?.checked_add(i64::from(c.to_digit(16)?))?;
    }
    Some(if neg { -n } else { n })
}

/// A device's colour (spotify.DeviceColors, as `"#rrggbb"` on the device) as [r, g, b] for the ring.
pub fn rgb(device: &Value) -> Vec<i64> {
    let c: Vec<char> = match device.get("color") {
        Some(v) if truthy(Some(v)) => py_str(v).chars().collect(),
        _ => Vec::new(),
    };
    if c.len() == 7 && c[0] == '#' {
        let parts: Option<Vec<i64>> = [1usize, 3, 5]
            .iter()
            .map(|&i| py_int_base16(&c[i..i + 2].iter().collect::<String>()))
            .collect();
        if let Some(parts) = parts {
            return parts;
        }
    }
    vec![30, 215, 96] // no colour: Spotify green
}

/// A spawned task this mode may cancel, with its generation. The slot is stored under the state lock before the
/// task can take that lock, so a task acts only while its own slot is still the current one: one that already
/// woke when a newer one replaced it, or when `close_picker` took it (on another worker thread, past the point
/// `abort` can reach), stands down instead of acting, as a cancelled asyncio task would.
struct Slot {
    gen: u64,
    handle: AbortHandle,
}

impl Slot {
    fn cancel(&self) {
        if !self.handle.is_finished() {
            self.handle.abort();
        }
    }
}

#[derive(Default)]
struct State {
    on: bool,
    /// The shadow the dial moves; Spotify's own read when the mode opens.
    volume: Option<i64>,
    volume_task: Option<Slot>,
    /// The devices while the picker is open.
    picker: Option<Vec<Value>>,
    index: usize,
    picker_timer: Option<Slot>,
    say_task: Option<Slot>,
    tasks: Vec<JoinHandle<()>>,
    gen: u64,
}

struct Inner {
    spotify: Arc<dyn SpotifyControl>,
    send: SendFn,
    say: Option<SayFn>,
    sleep: SleepFn,
    state: Mutex<State>,
}

/// The Voice PE's Spotify mode: the board's input in, Spotify commands and ring levels out.
///
/// A cheap handle: clones share one mode.
#[derive(Clone)]
pub struct MusicMode {
    inner: Arc<Inner>,
}

impl MusicMode {
    /// `spotify` is the Spotify hub, `send` goes to the Voice PE only (`Controller.show`), `say` is eve's voice
    /// on the Voice PE or `None` (ring only).
    pub fn new(spotify: Arc<dyn SpotifyControl>, send: SendFn, say: Option<SayFn>) -> Self {
        Self::with_sleep(spotify, send, say, tokio_sleep())
    }

    /// As [`MusicMode::new`], with the sleep injected (Python's `sleep=` parameter).
    pub fn with_sleep(spotify: Arc<dyn SpotifyControl>, send: SendFn, say: Option<SayFn>, sleep: SleepFn) -> Self {
        Self {
            inner: Arc::new(Inner { spotify, send, say, sleep, state: Mutex::new(State::default()) }),
        }
    }

    /// Whether the mode is on (Python's `.on`).
    pub fn on(&self) -> bool {
        self.inner.state.lock().on
    }

    /// The volume shadow (Python's `.volume`).
    pub fn volume(&self) -> Option<i64> {
        self.inner.state.lock().volume
    }

    /// The devices while the picker is open (Python's `.picker`).
    pub fn picker(&self) -> Option<Vec<Value>> {
        self.inner.state.lock().picker.clone()
    }

    /// The picker's place (Python's `.index`).
    pub fn index(&self) -> usize {
        self.inner.state.lock().index
    }

    async fn send(&self, obj: Value) -> anyhow::Result<()> {
        (self.inner.send)(obj).await
    }

    // -- on and off --

    /// Turn the mode on or off; the line to tell the owner.
    pub async fn set(&self, on: bool) -> anyhow::Result<String> {
        {
            let mut st = self.inner.state.lock();
            if on == st.on {
                return Ok(format!("Spotify mode is already {}", if on { "on." } else { "off." }));
            }
            st.on = on;
        }
        self.close_picker().await;
        self.send(json!({"cmd": "music_mode", "on": on})).await?;
        tracing::info!("music mode: {}", if on { "on" } else { "off" });
        if on {
            let now = self.inner.spotify.now_playing().await?;
            let vol = py_int_or_bool(get(now.get("device"), "volume"));
            self.inner.state.lock().volume = vol;
            return Ok("Spotify mode on: the dial is the volume, one click plays or pauses, two skip, three go back, \
                       hold to pick a device, hold 3 s to leave."
                .to_string());
        }
        Ok("Spotify mode off.".to_string())
    }

    // -- the board's input --

    /// `{"cmd":"music","clicks":n}`, `{"cmd":"music","hold":"short"|"long"}`, `{"cmd":"music","dial":d}`.
    pub async fn on_input(&self, obj: &Value) -> anyhow::Result<()> {
        let (on, picker_open) = {
            let st = self.inner.state.lock();
            (st.on, st.picker.is_some())
        };
        if !on {
            // a board that still thinks it is in music mode (the daemon restarted): put it back
            self.send(json!({"cmd": "music_mode", "on": false})).await?;
            return Ok(());
        }
        let hold = obj.get("hold");
        if hold.and_then(Value::as_str) == Some("long") {
            self.set(false).await?;
            self.flash("done", None).await;
            return Ok(());
        }
        if picker_open {
            return self.picker_input(obj).await;
        }
        if hold.and_then(Value::as_str) == Some("short") {
            return self.open_picker().await;
        }
        if let Some(dial) = py_int(obj.get("dial")).filter(|d| *d != 0) {
            return self.dial_volume(sat_i64(dial)).await;
        }
        let clicks = obj.get("clicks");
        let Some(n) = (1..=3).find(|n| py_eq_int(clicks, *n)) else {
            return Ok(());
        };
        let mut action = match n {
            2 => "next",
            3 => "previous",
            _ => "",
        };
        if action.is_empty() {
            let now = self.inner.spotify.now_playing().await?;
            action = if truthy(now.get("playing")) { "pause" } else { "play" };
        }
        let out = self.inner.spotify.control(action, Map::new()).await?;
        let ok = truthy(out.get("ok"));
        tracing::info!("music mode: {} click(s) -> {} ({})", n, action, if ok { "done" } else { "refused" });
        if !ok {
            self.flash("error", None).await;
        }
        Ok(())
    }

    /// A short sweep ("done": green, or `rgb`) or red blink ("error") on the ring, with its chirp; the board
    /// times it.
    async fn flash(&self, state: &str, rgb: Option<Vec<i64>>) {
        let mut msg = json!({"cmd": "music_flash", "ok": state == "done"});
        if let Some(rgb) = rgb.filter(|c| !c.is_empty()) {
            msg["rgb"] = json!(rgb);
        }
        let _ = self.send(msg).await;
    }

    // -- volume --

    async fn dial_volume(&self, detents: i64) -> anyhow::Result<()> {
        if self.inner.state.lock().volume.is_none() {
            let now = self.inner.spotify.now_playing().await?;
            match py_int_or_bool(get(now.get("device"), "volume")) {
                Some(vol) => self.inner.state.lock().volume = Some(vol),
                None => {
                    self.flash("error", None).await; // nothing playing anywhere: no volume to move
                    return Ok(());
                }
            }
        }
        let level = {
            let mut st = self.inner.state.lock();
            let v = st.volume.unwrap_or(0).saturating_add(detents.saturating_mul(VOLUME_PER_DETENT));
            let v = v.clamp(0, 100);
            st.volume = Some(v);
            v
        };
        self.send(json!({"cmd": "ring_level", "n": level, "of": 100})).await?;
        let me = self.clone();
        self.spawn_slot(|st| &mut st.volume_task, move |gen| async move { me.write_volume(gen).await });
        Ok(())
    }

    async fn write_volume(&self, gen: u64) {
        (self.inner.sleep)(VOLUME_DEBOUNCE_SECS).await;
        let volume = {
            let st = self.inner.state.lock();
            if st.volume_task.as_ref().is_none_or(|s| s.gen != gen) {
                return; // a newer turn of the dial took over
            }
            st.volume
        };
        let mut kw = Map::new();
        kw.insert("volume".into(), json!(volume));
        let ok = match self.inner.spotify.control("volume", kw).await {
            Ok(out) => truthy(out.get("ok")),
            Err(e) => {
                // Python: the exception ends the task (unretrieved); the shadow stays
                tracing::warn!("music mode: volume write failed: {e:#}");
                return;
            }
        };
        if !ok {
            self.inner.state.lock().volume = None; // read it again next time
            self.flash("error", None).await;
        }
    }

    // -- the device picker --

    async fn open_picker(&self) -> anyhow::Result<()> {
        let now = self.inner.spotify.now_playing().await?;
        let devices: Vec<Value> = if truthy(now.get("ok")) {
            match now.get("devices") {
                Some(Value::Array(ds)) => ds.iter().filter(|d| !truthy(d.get("restricted"))).cloned().collect(),
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };
        if devices.is_empty() {
            self.flash("error", None).await;
            if self.inner.say.is_some() {
                self.say_later("No Spotify device is online.".to_string(), 0.0);
            }
            return Ok(());
        }
        let count = devices.len();
        {
            let mut st = self.inner.state.lock();
            st.index = devices.iter().position(|d| truthy(d.get("active"))).unwrap_or(0);
            st.picker = Some(devices);
        }
        tracing::info!("music mode: picker open, {} devices", count);
        self.show_pick().await?;
        self.arm_picker_timer();
        Ok(())
    }

    async fn picker_input(&self, obj: &Value) -> anyhow::Result<()> {
        if let Some(dial) = py_int(obj.get("dial")).filter(|d| *d != 0) {
            {
                let mut st = self.inner.state.lock();
                let Some(len) = st.picker.as_ref().map(Vec::len) else {
                    return Ok(());
                };
                let len = len as i128;
                st.index = (st.index as i128 + dial).rem_euclid(len) as usize;
            }
            self.show_pick().await?;
            self.arm_picker_timer();
            return Ok(());
        }
        if obj.get("hold").and_then(Value::as_str) == Some("short") {
            self.close_picker().await; // back out, nothing changed
            return Ok(());
        }
        if py_eq_int(obj.get("clicks"), 1) {
            let chosen = {
                let st = self.inner.state.lock();
                match st.picker.as_ref().and_then(|p| p.get(st.index)) {
                    Some(d) => d.clone(),
                    None => return Ok(()),
                }
            };
            self.close_picker().await;
            if truthy(chosen.get("active")) {
                return Ok(()); // a click with no turn changes nothing, as on the knob
            }
            let name = chosen.get("name").cloned().ok_or_else(|| anyhow::anyhow!("KeyError: 'name'"))?;
            let mut kw = Map::new();
            kw.insert("device".into(), name.clone());
            let out = self.inner.spotify.control("transfer", kw).await?;
            let ok = truthy(out.get("ok"));
            tracing::info!("music mode: picked {} ({})", py_str(&name), if ok { "done" } else { "refused" });
            if ok {
                self.inner.state.lock().volume = None; // the new device has its own volume
                self.flash("done", Some(rgb(&chosen))).await; // the sweep in the new device's colour
            } else {
                self.flash("error", None).await;
            }
            if self.inner.say.is_some() {
                let line = match out.get("line") {
                    Some(v) if truthy(Some(v)) => py_str(v),
                    _ => String::new(),
                };
                self.say_later(line, 0.0);
            }
        }
        Ok(())
    }

    async fn show_pick(&self) -> anyhow::Result<()> {
        let (msg, name) = {
            let st = self.inner.state.lock();
            let Some(picker) = st.picker.as_ref() else {
                return Ok(());
            };
            let device = &picker[st.index];
            let msg = json!({"cmd": "ring_level", "n": st.index + 1, "of": picker.len(), "dot": true,
                             "ms": (PICKER_IDLE_SECS * 1000.0) as i64, "rgb": rgb(device)});
            (msg, device.get("name").cloned())
        };
        self.send(msg).await?;
        if self.inner.say.is_some() {
            let name = name.ok_or_else(|| anyhow::anyhow!("KeyError: 'name'"))?;
            self.say_later(py_str(&name), PICKER_SAY_SECS);
        }
        Ok(())
    }

    fn say_later(&self, text: String, after: f64) {
        let me = self.clone();
        // a newer name replaces one not yet said
        self.spawn_slot(|st| &mut st.say_task, move |gen| async move {
            (me.inner.sleep)(after).await;
            if me.inner.state.lock().say_task.as_ref().is_none_or(|s| s.gen != gen) {
                return; // a newer name replaced this one
            }
            if let Some(say) = me.inner.say.as_ref() {
                let _ = say(text).await;
            }
        });
    }

    fn arm_picker_timer(&self) {
        let me = self.clone();
        self.spawn_slot(|st| &mut st.picker_timer, move |gen| async move {
            (me.inner.sleep)(PICKER_IDLE_SECS).await;
            let was_open = {
                let mut st = me.inner.state.lock();
                if st.picker_timer.as_ref().is_none_or(|s| s.gen != gen) {
                    return; // re-armed by newer input, or the picker closed
                }
                tracing::info!("music mode: picker timed out");
                st.picker_timer = None; // this task is ending: close_picker must not cancel it
                Self::take_picker(&mut st)
            };
            me.clear_dot(was_open).await;
        });
    }

    async fn close_picker(&self) {
        let was_open = Self::take_picker(&mut self.inner.state.lock());
        self.clear_dot(was_open).await;
    }

    /// `_close_picker`'s state half: cancel the idle timer, drop the devices; whether the picker was open.
    fn take_picker(st: &mut State) -> bool {
        if let Some(timer) = st.picker_timer.take() {
            timer.cancel();
        }
        st.picker.take().is_some()
    }

    /// `_close_picker`'s board half.
    async fn clear_dot(&self, was_open: bool) {
        if was_open {
            let _ = self.send(json!({"cmd": "ring_level", "n": 0, "of": 1, "ms": 0})).await; // the dot goes
        }
    }

    /// Spawn the task `make(gen)` into the slot `slot` picks, cancelling the one it replaces. The slot is stored
    /// before the state lock is released, so the task (which takes the lock before acting) always finds it.
    fn spawn_slot<F>(&self, slot: fn(&mut State) -> &mut Option<Slot>, make: impl FnOnce(u64) -> F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut st = self.inner.state.lock();
        st.gen += 1;
        let gen = st.gen;
        let task = tokio::spawn(make(gen));
        let handle = task.abort_handle();
        st.tasks.retain(|t| !t.is_finished());
        st.tasks.push(task);
        if let Some(old) = slot(&mut st).replace(Slot { gen, handle }) {
            old.cancel();
        }
    }

    /// Cancel every task the mode started (debounced writes, the picker timer, a name not yet said) and wait
    /// for them to end.
    pub async fn close(&self) {
        let tasks = std::mem::take(&mut self.inner.state.lock().tasks);
        for t in &tasks {
            t.abort();
        }
        futures::future::join_all(tasks).await;
    }
}

#[cfg(test)]
mod tests;
