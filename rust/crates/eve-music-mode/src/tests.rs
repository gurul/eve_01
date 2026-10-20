//! Port of bridge/tests/test_music_mode.py: Spotify mode on the Voice PE — clicks, holds, the dial as volume,
//! the device picker.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use super::*;

struct FakeState {
    playing: bool,
    volume: i64,
    devices: Vec<Value>,
    calls: Vec<(String, Map<String, Value>)>,
    refuse: HashSet<String>,
}

struct FakeSpotify {
    st: Mutex<FakeState>,
}

impl FakeSpotify {
    fn new() -> Self {
        Self {
            st: Mutex::new(FakeState {
                playing: true,
                volume: 40,
                devices: vec![
                    json!({"name": "MacBook", "type": "computer", "active": true, "volume": 40, "color": "#005aff"}),
                    json!({"name": "Kitchen Speaker", "type": "speaker", "active": false, "color": "#ff6e00"}),
                    json!({"name": "TV", "type": "tv", "active": false, "restricted": true}),
                ],
                calls: Vec::new(),
                refuse: HashSet::new(),
            }),
        }
    }

    fn actions(&self) -> Vec<String> {
        self.st.lock().calls.iter().map(|(a, _)| a.clone()).collect()
    }

    fn called(&self, action: &str, kw: Value) -> bool {
        let kw = kw.as_object().cloned().unwrap_or_default();
        self.st.lock().calls.iter().any(|(a, k)| a == action && *k == kw)
    }
}

#[async_trait]
impl SpotifyControl for FakeSpotify {
    async fn now_playing(&self) -> anyhow::Result<Value> {
        let st = self.st.lock();
        Ok(json!({"ok": true, "playing": st.playing, "track": "One More Time",
                  "device": {"name": "MacBook", "volume": st.volume}, "devices": st.devices}))
    }

    async fn control(&self, action: &str, kw: Map<String, Value>) -> anyhow::Result<Value> {
        let mut st = self.st.lock();
        st.calls.push((action.to_string(), kw.clone()));
        if st.refuse.contains(action) {
            return Ok(json!({"ok": false, "line": "Controlling playback needs Spotify Premium."}));
        }
        if action == "volume" {
            st.volume = kw["volume"].as_i64().unwrap_or_default();
        }
        if action == "play" || action == "pause" {
            st.playing = action == "play";
        }
        let on = kw.get("device").and_then(Value::as_str).map(|d| format!(" on {d}")).unwrap_or_default();
        Ok(json!({"ok": true, "line": format!("did {action}{on}")}))
    }
}

struct Rig {
    spotify: Arc<FakeSpotify>,
    sent: Arc<Mutex<Vec<Value>>>,
    said: Arc<Mutex<Vec<String>>>,
    sleeps: Arc<Mutex<Vec<f64>>>,
    /// The picker's 10 s idle timer fires only when a test says so.
    idle_passes: Arc<AtomicBool>,
    mode: MusicMode,
}

impl Rig {
    fn new(say: bool) -> Self {
        let spotify = Arc::new(FakeSpotify::new());
        let sent: Arc<Mutex<Vec<Value>>> = Arc::default();
        let said: Arc<Mutex<Vec<String>>> = Arc::default();
        let sleeps: Arc<Mutex<Vec<f64>>> = Arc::default();
        let idle_passes = Arc::new(AtomicBool::new(false));
        let send: SendFn = {
            let sent = sent.clone();
            Arc::new(move |obj: Value| -> BoxFuture<'static, anyhow::Result<()>> {
                sent.lock().push(obj);
                Box::pin(async { Ok(()) })
            })
        };
        let speak: SayFn = {
            let said = said.clone();
            Arc::new(move |text: String| -> BoxFuture<'static, anyhow::Result<()>> {
                said.lock().push(text);
                Box::pin(async { Ok(()) })
            })
        };
        // short timers fire at once; the sleeps are recorded
        let sleep: SleepFn = {
            let sleeps = sleeps.clone();
            let idle = idle_passes.clone();
            Arc::new(move |secs: f64| -> BoxFuture<'static, ()> {
                sleeps.lock().push(secs);
                let block = secs >= PICKER_IDLE_SECS && !idle.load(Ordering::SeqCst);
                Box::pin(async move {
                    if block {
                        std::future::pending::<()>().await; // cancelled by the next input, or by close()
                    }
                    tokio::task::yield_now().await;
                })
            })
        };
        let mode = MusicMode::with_sleep(spotify.clone(), send, if say { Some(speak) } else { None }, sleep);
        Self { spotify, sent, said, sleeps, idle_passes, mode }
    }

    async fn settle(&self) {
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }

    fn sent(&self) -> Vec<Value> {
        self.sent.lock().clone()
    }

    fn said(&self) -> Vec<String> {
        self.said.lock().clone()
    }

    fn levels(&self) -> Vec<Value> {
        self.sent().into_iter().filter(|o| o["cmd"] == "ring_level").collect()
    }
}

fn music(e: Value) -> Value {
    let mut obj = json!({"cmd": "music"});
    for (k, v) in e.as_object().unwrap() {
        obj[k] = v.clone();
    }
    obj
}

async fn go(rig: &Rig, events: &[Value]) {
    rig.mode.set(true).await.unwrap();
    for e in events {
        rig.mode.on_input(&music(e.clone())).await.unwrap();
        rig.settle().await;
    }
    rig.mode.close().await;
}

#[tokio::test]
async fn test_turning_it_on_tells_the_board_and_reads_the_volume() {
    let rig = Rig::new(true);
    let on = rig.mode.set(true).await.unwrap();
    assert_eq!(rig.mode.volume(), Some(40));
    let again = rig.mode.set(true).await.unwrap();
    let off = rig.mode.set(false).await.unwrap();
    assert_eq!(rig.sent(), vec![json!({"cmd": "music_mode", "on": true}), json!({"cmd": "music_mode", "on": false})]);
    assert!(on.contains("dial is the volume"));
    assert_eq!(again, "Spotify mode is already on.");
    assert_eq!(off, "Spotify mode off.");
    assert_eq!(rig.mode.set(false).await.unwrap(), "Spotify mode is already off.");
}

#[tokio::test]
async fn test_one_click_toggles_two_skip_three_go_back() {
    let rig = Rig::new(true);
    go(&rig, &[json!({"clicks": 1}), json!({"clicks": 1}), json!({"clicks": 2}), json!({"clicks": 3}), json!({"clicks": 5})])
        .await;
    assert_eq!(rig.spotify.actions(), vec!["pause", "play", "next", "previous"]); // 5 clicks: nothing
}

#[tokio::test]
async fn test_the_dial_is_the_volume_shown_at_once_written_once() {
    let rig = Rig::new(true);
    go(&rig, &[json!({"dial": 1}), json!({"dial": 2}), json!({"dial": -1})]).await;
    let levels: Vec<Value> = rig.levels().into_iter().map(|o| o["n"].clone()).collect();
    assert_eq!(levels, vec![json!(44), json!(52), json!(48)]); // 4% a detent, from Spotify's 40
    // every step starts the debounce again; each write that lands is the shadow at that moment
    let writes: Vec<Value> = rig
        .spotify
        .st
        .lock()
        .calls
        .iter()
        .filter(|(a, _)| a == "volume")
        .map(|(_, kw)| kw["volume"].clone())
        .collect();
    assert_eq!(writes.last(), Some(&json!(48)));
    assert!(rig.sleeps.lock().contains(&VOLUME_DEBOUNCE_SECS));
}

#[tokio::test]
async fn test_the_volume_is_clamped() {
    let rig = Rig::new(true);
    go(&rig, &[json!({"dial": 30})]).await;
    let levels: Vec<Value> = rig.levels().into_iter().map(|o| o["n"].clone()).collect();
    assert_eq!(levels, vec![json!(100)]);
}

#[tokio::test]
async fn test_hold_opens_the_picker_dial_moves_click_plays_there() {
    let rig = Rig::new(true);
    go(&rig, &[json!({"hold": "short"}), json!({"dial": 1}), json!({"clicks": 1})]).await;
    let dots: Vec<Value> = rig.levels().into_iter().filter(|o| o["dot"] == true).collect();
    let places: Vec<(Value, Value)> = dots.iter().map(|d| (d["n"].clone(), d["of"].clone())).collect();
    // the TV (restricted) is not in the picker
    assert_eq!(places, vec![(json!(1), json!(2)), (json!(2), json!(2))]);
    let colours: Vec<Value> = dots.iter().map(|d| d["rgb"].clone()).collect();
    assert_eq!(colours, vec![json!([0, 90, 255]), json!([255, 110, 0])]); // each device in its own colour
    assert!(rig.spotify.called("transfer", json!({"device": "Kitchen Speaker"})));
    // the sweep in its colour
    assert!(rig.sent().contains(&json!({"cmd": "music_flash", "ok": true, "rgb": [255, 110, 0]})));
    let said = rig.said();
    assert!(said.contains(&"Kitchen Speaker".to_string()));
    assert_eq!(said.last().unwrap(), "did transfer on Kitchen Speaker");
    assert!(rig.mode.picker().is_none() && rig.mode.on());
}

#[tokio::test]
async fn test_in_the_picker_a_click_on_the_playing_device_or_a_hold_changes_nothing() {
    let rig = Rig::new(true);
    go(
        &rig,
        &[json!({"hold": "short"}), json!({"clicks": 1}), json!({"hold": "short"}), json!({"dial": 1}), json!({"hold": "short"})],
    )
    .await;
    assert!(!rig.spotify.actions().iter().any(|a| a == "transfer"));
    assert!(rig.mode.picker().is_none());
    assert_eq!(rig.sent().last().unwrap(), &json!({"cmd": "ring_level", "n": 0, "of": 1, "ms": 0})); // the dot is cleared
}

#[tokio::test]
async fn test_the_picker_times_out() {
    let rig = Rig::new(true);
    rig.idle_passes.store(true, Ordering::SeqCst);
    rig.mode.set(true).await.unwrap();
    rig.mode.on_input(&music(json!({"hold": "short"}))).await.unwrap();
    rig.settle().await;
    assert!(rig.sleeps.lock().contains(&PICKER_IDLE_SECS));
    assert!(rig.mode.picker().is_none());
    assert_eq!(rig.sent().last().unwrap(), &json!({"cmd": "ring_level", "n": 0, "of": 1, "ms": 0}));
}

#[tokio::test]
async fn test_a_long_hold_leaves_the_mode() {
    let rig = Rig::new(true);
    go(&rig, &[json!({"hold": "long"}), json!({"clicks": 1})]).await;
    assert!(!rig.mode.on());
    let sent = rig.sent();
    assert!(sent.contains(&json!({"cmd": "music_mode", "on": false})));
    assert!(sent.contains(&json!({"cmd": "music_flash", "ok": true})));
    assert!(rig.spotify.st.lock().calls.is_empty()); // the click after it is not Spotify's any more
}

#[tokio::test]
async fn test_a_refusal_blinks_red() {
    let rig = Rig::new(true);
    rig.spotify.st.lock().refuse = HashSet::from(["next".to_string()]);
    go(&rig, &[json!({"clicks": 2})]).await;
    assert_eq!(rig.sent().last().unwrap(), &json!({"cmd": "music_flash", "ok": false}));
}

#[tokio::test]
async fn test_no_device_online_blinks_and_says_so() {
    let rig = Rig::new(true);
    rig.spotify.st.lock().devices = Vec::new();
    go(&rig, &[json!({"hold": "short"})]).await;
    assert!(rig.sent().contains(&json!({"cmd": "music_flash", "ok": false})));
    assert_eq!(rig.said(), vec!["No Spotify device is online.".to_string()]);
}

#[tokio::test]
async fn test_input_while_off_puts_the_board_back() {
    let rig = Rig::new(true);
    rig.mode.on_input(&music(json!({"clicks": 1}))).await.unwrap();
    assert_eq!(rig.sent(), vec![json!({"cmd": "music_mode", "on": false})]);
    assert!(rig.spotify.st.lock().calls.is_empty());
}

#[tokio::test]
async fn test_without_a_voice_the_picker_is_the_ring_alone() {
    let rig = Rig::new(false);
    go(&rig, &[json!({"hold": "short"}), json!({"dial": 1}), json!({"clicks": 1})]).await;
    assert!(rig.said().is_empty());
    assert!(rig.spotify.called("transfer", json!({"device": "Kitchen Speaker"})));
}

#[test]
fn test_a_device_without_a_colour_is_green() {
    assert_eq!(rgb(&json!({"name": "x"})), vec![30, 215, 96]);
    assert_eq!(rgb(&json!({"color": "#zzzzzz"})), vec![30, 215, 96]);
    assert_eq!(rgb(&json!({"color": "#ff6e00"})), vec![255, 110, 0]);
}

// ---- beyond the Python file: the edges the module's Python semantics decide ----

#[test]
fn rgb_parses_like_python_int_base_16() {
    // int(" f", 16) == 15, int("+f", 16) == 15, int("-f", 16) == -15; "_f" is not a number
    assert_eq!(rgb(&json!({"color": "# f+f-f"})), vec![15, 15, -15]);
    assert_eq!(rgb(&json!({"color": "#_fffff"})), vec![30, 215, 96]);
    assert_eq!(rgb(&json!({"color": "#ff6e0"})), vec![30, 215, 96]); // wrong length
    assert_eq!(rgb(&json!({"color": 123})), vec![30, 215, 96]);
    assert_eq!(rgb(&json!({"color": "#FF6E00"})), vec![255, 110, 0]);
}

#[tokio::test]
async fn clicks_compare_like_python() {
    // `clicks in (1, 2, 3)`: 2.0 == 2 and True == 1 in Python
    let rig = Rig::new(true);
    go(&rig, &[json!({"clicks": 2.0}), json!({"clicks": true}), json!({"clicks": "1"}), json!({"clicks": 0})]).await;
    assert_eq!(rig.spotify.actions(), vec!["next", "pause"]);
}

#[tokio::test]
async fn a_boolean_or_zero_dial_is_not_a_turn() {
    let rig = Rig::new(true);
    go(&rig, &[json!({"dial": true}), json!({"dial": 0}), json!({"dial": 1.5})]).await;
    assert!(rig.levels().is_empty());
    assert!(rig.spotify.st.lock().calls.is_empty());
}

#[tokio::test]
async fn the_picker_dial_wraps_both_ways() {
    let rig = Rig::new(false);
    rig.mode.set(true).await.unwrap();
    rig.mode.on_input(&music(json!({"hold": "short"}))).await.unwrap();
    assert_eq!(rig.mode.index(), 0); // the active device
    rig.mode.on_input(&music(json!({"dial": -1}))).await.unwrap();
    assert_eq!(rig.mode.index(), 1);
    rig.mode.on_input(&music(json!({"dial": 5}))).await.unwrap();
    assert_eq!(rig.mode.index(), 0);
    rig.mode.close().await;
}

#[tokio::test]
async fn a_refused_volume_write_reads_the_volume_again() {
    let rig = Rig::new(true);
    rig.spotify.st.lock().refuse = HashSet::from(["volume".to_string()]);
    go(&rig, &[json!({"dial": 1})]).await;
    assert_eq!(rig.mode.volume(), None);
    assert_eq!(rig.sent().last().unwrap(), &json!({"cmd": "music_flash", "ok": false}));
}

#[tokio::test]
async fn a_picked_device_says_the_refusal() {
    let rig = Rig::new(true);
    rig.spotify.st.lock().refuse = HashSet::from(["transfer".to_string()]);
    go(&rig, &[json!({"hold": "short"}), json!({"dial": 1}), json!({"clicks": 1})]).await;
    assert!(rig.sent().contains(&json!({"cmd": "music_flash", "ok": false})));
    assert_eq!(rig.said().last().unwrap(), "Controlling playback needs Spotify Premium.");
    assert_eq!(rig.mode.volume(), Some(40)); // not cleared: the device did not change
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_real_timer_debounces_one_write() {
    let spotify = Arc::new(FakeSpotify::new());
    let send: SendFn = Arc::new(|_obj: Value| -> BoxFuture<'static, anyhow::Result<()>> { Box::pin(async { Ok(()) }) });
    let mode = MusicMode::new(spotify.clone(), send, None);
    mode.set(true).await.unwrap();
    for _ in 0..3 {
        mode.on_input(&json!({"cmd": "music", "dial": 1})).await.unwrap();
    }
    tokio::time::sleep(std::time::Duration::from_secs_f64(VOLUME_DEBOUNCE_SECS + 0.3)).await;
    let writes: Vec<_> = spotify.st.lock().calls.iter().filter(|(a, _)| a == "volume").cloned().collect();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].1["volume"], json!(52));
    mode.close().await;
}

#[tokio::test]
async fn a_boolean_volume_is_python_s_int() {
    // `isinstance(True, int)` holds in Python, so a device reporting `"volume": true` moves from 1
    struct BoolVolume;
    #[async_trait]
    impl SpotifyControl for BoolVolume {
        async fn now_playing(&self) -> anyhow::Result<Value> {
            Ok(json!({"ok": true, "playing": true, "device": {"volume": true}, "devices": []}))
        }
        async fn control(&self, _action: &str, _kw: Map<String, Value>) -> anyhow::Result<Value> {
            Ok(json!({"ok": true}))
        }
    }
    let sent: Arc<Mutex<Vec<Value>>> = Arc::default();
    let send: SendFn = {
        let sent = sent.clone();
        Arc::new(move |obj: Value| -> BoxFuture<'static, anyhow::Result<()>> {
            sent.lock().push(obj);
            Box::pin(async { Ok(()) })
        })
    };
    let mode = MusicMode::new(Arc::new(BoolVolume), send, None);
    mode.set(true).await.unwrap();
    assert_eq!(mode.volume(), Some(1));
    mode.on_input(&json!({"cmd": "music", "dial": 1})).await.unwrap();
    assert_eq!(sent.lock().last().unwrap(), &json!({"cmd": "ring_level", "n": 5, "of": 100}));
    mode.close().await;
}

#[tokio::test]
async fn a_dial_past_i64_is_still_a_turn() {
    // Python's ints are unbounded: a dial of 2**63 is an int, so the volume moves (and clamps); in the picker the
    // index wraps exactly
    let rig = Rig::new(false);
    go(&rig, &[json!({"dial": 9_223_372_036_854_775_808u64})]).await;
    let levels: Vec<Value> = rig.levels().into_iter().map(|o| o["n"].clone()).collect();
    assert_eq!(levels, vec![json!(100)]);

    let rig = Rig::new(false);
    rig.mode.set(true).await.unwrap();
    rig.mode.on_input(&music(json!({"hold": "short"}))).await.unwrap();
    // (0 + 2**63) % 2 == 0, and (0 + 2**64 - 1) % 2 == 1
    rig.mode.on_input(&music(json!({"dial": 9_223_372_036_854_775_808u64}))).await.unwrap();
    assert_eq!(rig.mode.index(), 0);
    rig.mode.on_input(&music(json!({"dial": u64::MAX}))).await.unwrap();
    assert_eq!(rig.mode.index(), 1);
    rig.mode.close().await;
}
