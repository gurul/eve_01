//! scene.py: timestamped views for the voice, with a fake describer, a fake
//! clock and a fake wall clock. The watcher is stepped by hand (start(false)
//! then tick()), so every staleness and camera-loss rule is deterministic.
//! Ports bridge/tests/test_scene.py plus the scene parts of
//! test_daemon_scene.py and test_spend_wiring.py.

use super::*;
use base64::Engine as _;
use chrono::NaiveDate;
use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::sync::{Condvar, Mutex as StdMutex};

fn t0() -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 9, 10).unwrap().and_hms_opt(16, 43, 0).unwrap()
}

#[derive(Clone)]
struct TestClock(Arc<StdMutex<f64>>);

impl TestClock {
    fn new() -> Self {
        TestClock(Arc::new(StdMutex::new(100.0)))
    }
    fn t(&self) -> f64 {
        *self.0.lock().unwrap()
    }
    fn add(&self, d: f64) {
        *self.0.lock().unwrap() += d;
    }
    fn wall(&self) -> NaiveDateTime {
        t0() + chrono::Duration::microseconds(((self.t() - 100.0) * 1e6).round() as i64)
    }
    fn clock_fn(&self) -> Clock {
        let c = self.clone();
        Arc::new(move || c.t())
    }
    fn wall_fn(&self) -> Wall {
        let c = self.clone();
        Arc::new(move || c.wall())
    }
}

/// Scripted answers. Records the image bytes each describe got.
struct FakeClient {
    answers: StdMutex<VecDeque<(String, String)>>,
    images: StdMutex<Vec<Vec<u8>>>,
    previous: StdMutex<Vec<Option<String>>>,
    clock: Option<TestClock>,
    takes: f64,
}

impl FakeClient {
    fn new(answers: &[(&str, &str)]) -> Arc<Self> {
        Self::timed(answers, None, 0.0)
    }
    fn timed(answers: &[(&str, &str)], clock: Option<TestClock>, takes: f64) -> Arc<Self> {
        Arc::new(FakeClient {
            answers: StdMutex::new(answers.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()),
            images: StdMutex::default(),
            previous: StdMutex::default(),
            clock,
            takes,
        })
    }
    fn images(&self) -> Vec<Vec<u8>> {
        self.images.lock().unwrap().clone()
    }
}

impl SceneClient for FakeClient {
    fn describe(&self, image: &[u8], _mime: &str, previous: Option<&str>) -> anyhow::Result<(String, String)> {
        self.images.lock().unwrap().push(image.to_vec());
        self.previous.lock().unwrap().push(previous.map(str::to_string));
        if let Some(c) = &self.clock {
            c.add(self.takes); // the answer comes back later than the frame
        }
        self.answers.lock().unwrap().pop_front().ok_or_else(|| RuntimeError("no more answers".into()).into())
    }

    fn locate(&self, _image: &[u8], _mime: &str, target: &str) -> anyhow::Result<Map<String, Value>> {
        let Value::Object(m) = json!({"visible": true, "x": 12.0, "y": -40.0, "what": target}) else {
            unreachable!()
        };
        Ok(m)
    }
}

fn jpeg_frame(seq: i64, tag: Option<&[u8]>) -> Value {
    let mut data = b"\xff\xd8".to_vec();
    match tag {
        Some(t) => data.extend_from_slice(t),
        None => data.extend_from_slice(format!("frame{seq}").as_bytes()),
    }
    json!({"seq": seq, "w": 160, "h": 120, "fmt": "jpeg",
           "b64": base64::engine::general_purpose::STANDARD.encode(data)})
}

fn test_config() -> SceneConfig {
    SceneConfig {
        interval_secs: 3.0,
        stale_secs: 15.0,
        camera_lost_secs: 3.0,
        timeout_secs: 2.0,
        refresh_secs: 20.0,
        ..SceneConfig::default()
    }
}

fn watcher_with(client: Option<Arc<dyn SceneClient>>, clock: &TestClock, config: SceneConfig) -> Arc<SceneWatcher> {
    Arc::new(SceneWatcher::new(client, Some(config)).with_clock(clock.clock_fn()).with_wall(clock.wall_fn()))
}

fn watcher(client: Arc<FakeClient>, clock: &TestClock) -> Arc<SceneWatcher> {
    watcher_with(Some(client), clock, test_config())
}

// ---- pure helpers ----------------------------------------------------------------------

#[test]
fn test_parse_scene_json_plain_and_none_change() {
    assert_eq!(
        parse_scene(r#"{"scene":"A mug on a desk.","change":"mug moved left"}"#).unwrap(),
        ("A mug on a desk.".to_string(), "mug moved left".to_string())
    );
    assert_eq!(parse_scene(r#"{"scene":"A mug.","change":"None."}"#).unwrap(), ("A mug.".into(), "".into()));
    assert_eq!(parse_scene("  A plain   sentence.  ").unwrap(), ("A plain sentence.".into(), "".into()));
    assert!(parse_scene("").is_err());
    assert!(parse_scene(r#"{"scene":"","change":"x"}"#).is_err());
}

#[test]
fn test_parse_scene_follows_python_str_and_truthiness() {
    // A JSON value that is not an object is the scene as plain text.
    assert_eq!(parse_scene("[1, 2]").unwrap(), ("[1, 2]".into(), "".into()));
    assert_eq!(parse_scene("42").unwrap(), ("42".into(), "".into()));
    // Non-string fields go through str(); falsy ones are empty.
    assert_eq!(parse_scene(r#"{"scene": 5, "change": 0}"#).unwrap(), ("5".into(), "".into()));
    assert_eq!(parse_scene(r#"{"scene": true, "change": "No change"}"#).unwrap(), ("True".into(), "".into()));
    assert_eq!(parse_scene(r#"{"scene": "a\n b", "change": "N/A..."}"#).unwrap(), ("a b".into(), "".into()));
    assert_eq!(parse_scene(r#"{"scene": ["x"], "change": null}"#).unwrap(), ("['x']".into(), "".into()));
    assert_eq!(parse_scene("   ").unwrap_err().0, "empty answer");
    assert_eq!(parse_scene(r#"{"change":"x"}"#).unwrap_err().0, "answer has no scene");
}

#[test]
fn test_parse_locate_clamps_and_blanks_when_not_visible() {
    let got = parse_locate(r#"{"visible": true, "x": 250, "y": "-12.5", "what": " a  green\nmug "}"#).unwrap();
    assert_eq!(Value::Object(got), json!({"visible": true, "x": 100.0, "y": -12.5, "what": "a green mug"}));
    let got = parse_locate(r#"{"visible": false, "x": 50, "y": 50, "what": "mug"}"#).unwrap();
    assert_eq!(Value::Object(got), json!({"visible": false, "x": 0.0, "y": 0.0, "what": ""}));
    // Only the boolean true counts as visible; unreadable coordinates are 0.
    let got = parse_locate(r#"{"visible": 1, "x": 50}"#).unwrap();
    assert_eq!(got["visible"], json!(false));
    let got = parse_locate(r#"{"visible": true, "x": null, "y": "nan", "what": null}"#).unwrap();
    assert_eq!(Value::Object(got), json!({"visible": true, "x": 0.0, "y": 100.0, "what": ""}));
    let got = parse_locate(r#"{"visible": true, "x": true, "y": -1e9}"#).unwrap();
    assert_eq!((got["x"].clone(), got["y"].clone()), (json!(1.0), json!(-100.0)));
    let err = parse_locate("not json").unwrap_err();
    assert_eq!(error_name(&err), "JSONDecodeError");
    let err = parse_locate("[1]").unwrap_err();
    assert_eq!(error_name(&err), "AttributeError");
    assert_eq!(err.to_string(), "'list' object has no attribute 'get'");
}

#[test]
fn test_format_note_carries_seen_time_and_change_and_is_clipped() {
    let obs = Observation::new(1.0, t0(), "A person holds a green mug.", "they picked it up", 0.4);
    assert_eq!(format_note(&obs), "[vision 16:43:00] A person holds a green mug. Change: they picked it up");
    let long = Observation::new(1.0, t0(), &"x".repeat(900), "", 0.1);
    let note = format_note(&long);
    assert!(note.chars().count() <= NOTE_MAX_CHARS);
    assert!(note.ends_with('…'));
    assert_eq!(note.chars().count(), NOTE_MAX_CHARS);
}

#[test]
fn test_note_lines() {
    let last = Observation::new(1.0, t0(), "A desk.", "", 0.1);
    let later = t0() + chrono::Duration::seconds(5);
    assert_eq!(
        lost_note(&later, Some(&last)),
        "[vision 16:43:05] Camera lost: you cannot see anything right now. Your last view, from 16:43:00, may be out of date."
    );
    assert_eq!(lost_note(&later, None), "[vision 16:43:05] Camera lost: you cannot see anything right now.");
    assert_eq!(
        blind_note(&later, Some(&last)),
        "[vision 16:43:05] You cannot make out the view right now. Your last clear view is from 16:43:00 and may be out of date."
    );
    assert_eq!(blind_note(&later, None), "[vision 16:43:05] You cannot make out the view right now.");
    let newer = Observation::new(4.0, later, "A desk.", "", 0.1);
    assert_eq!(unchanged_note(&newer, &last), "[vision 16:43:05] No change since 16:43:00: A desk.");
}

#[test]
fn test_configured_reads_env() {
    let empty = HashMap::new();
    assert_eq!(configured(Some(&empty)).model, "gpt-6-luna");
    assert!(configured(Some(&empty)).enabled);
    assert_eq!(configured(Some(&empty)), SceneConfig::default());
    let env: HashMap<String, String> = [
        ("EVE_SCENE", "0"),
        ("EVE_SCENE_MODEL", "gpt-5-mini"),
        ("EVE_SCENE_INTERVAL_SECS", "0.1"),
        ("EVE_SCENE_STALE_SECS", "abc"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let c = configured(Some(&env));
    assert!(!c.enabled && c.model == "gpt-5-mini");
    assert_eq!(c.interval_secs, 1.0); // clamped to the floor
    assert_eq!(c.stale_secs, 15.0); // not a number: the default
}

#[test]
fn test_configured_edges() {
    let env = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    };
    assert!(!configured(Some(&env(&[("EVE_SCENE", " OFF ")]))).enabled);
    assert!(configured(Some(&env(&[("EVE_SCENE", "")]))).enabled);
    assert!(configured(Some(&env(&[("EVE_SCENE", "yes")]))).enabled);
    assert_eq!(configured(Some(&env(&[("EVE_SCENE_MODEL", "  ")]))).model, DEFAULT_MODEL);
    assert_eq!(configured(Some(&env(&[("EVE_SCENE_INTERVAL_SECS", "1_000")]))).interval_secs, 60.0);
    assert_eq!(configured(Some(&env(&[("EVE_SCENE_INTERVAL_SECS", " 7.5 ")]))).interval_secs, 7.5);
    // max(lo, nan) is lo in Python, so a NaN lands on the floor.
    assert_eq!(configured(Some(&env(&[("EVE_SCENE_STALE_SECS", "nan")]))).stale_secs, 3.0);
    assert_eq!(configured(Some(&env(&[("EVE_SCENE_STALE_SECS", "inf")]))).stale_secs, 300.0);
}

#[test]
fn test_mime_for() {
    assert_eq!(mime_for(b"\xff\xd8abc"), Some("image/jpeg"));
    assert_eq!(mime_for(b"\x89PNG\r\n\x1a\nxx"), Some("image/png"));
    assert_eq!(mime_for(b"GIF89a"), None);
    assert_eq!(mime_for(b""), None);
}

// ---- the watcher -------------------------------------------------------------------------

#[tokio::test]
async fn test_frames_are_ignored_outside_a_conversation() {
    let clock = TestClock::new();
    let w = watcher(FakeClient::new(&[("A desk.", "")]), &clock);
    w.offer(jpeg_frame(1, None));
    assert!(w.held_frame().is_none()); // not started: nothing is held
    w.start(false);
    w.offer(jpeg_frame(2, None));
    assert!(w.held_frame().is_some());
    w.stop().await;
    assert!(w.held_frame().is_none() && w.latest().is_none() && w.notes().is_empty());
    w.offer(jpeg_frame(3, None));
    assert!(w.held_frame().is_none()); // stopped again: nothing is held
}

#[tokio::test]
async fn test_disabled_watcher_never_starts() {
    let clock = TestClock::new();
    let w = watcher_with(None, &clock, test_config());
    w.start(false);
    assert!(!w.enabled() && !w.active());
    assert_eq!(w.look(None, None).await["ok"], json!(false));
    assert_eq!(
        Value::Object(w.look(None, None).await),
        json!({"ok": false, "reason": "vision is off on this computer (no API key or EVE_SCENE=0)"})
    );
    let w2 = watcher_with(Some(FakeClient::new(&[])), &clock, SceneConfig { enabled: false, ..SceneConfig::default() });
    w2.start(false);
    assert!(!w2.active());
}

#[tokio::test]
async fn test_only_the_newest_frame_is_described_and_note_uses_seen_time() {
    let clock = TestClock::new();
    let client = FakeClient::timed(&[("A person at a desk.", "")], Some(clock.clone()), 2.0);
    let w = watcher(client.clone(), &clock);
    w.start(false);
    for seq in [1, 2, 3] {
        w.offer(jpeg_frame(seq, None));
    }
    clock.add(0.5); // the frame arrived at 16:43:00, the tick is later
    w.tick().await;
    assert_eq!(client.images(), vec![b"\xff\xd8frame3".to_vec()]); // latest-only: 1 and 2 never left
    assert!(w.held_frame().is_none()); // released once handed off
    assert_eq!(w.notes(), vec!["[vision 16:43:00] A person at a desk.".to_string()]); // seen time, not answer time
    let latest = w.latest().unwrap();
    assert!((latest.latency_secs - 2.0).abs() < 1e-9);
    w.stop().await;
}

#[tokio::test]
async fn test_change_is_announced_and_sameness_is_repeated_on_refresh() {
    let clock = TestClock::new();
    let client = FakeClient::new(&[
        ("A desk with a mug.", ""),
        ("A desk with a mug.", ""),
        ("A hand lifts the mug.", "the mug was picked up"),
    ]);
    let w = watcher_with(Some(client.clone()), &clock, SceneConfig { refresh_secs: 10.0, ..test_config() });
    w.start(false);
    w.offer(jpeg_frame(1, None));
    w.tick().await; // first view: always a note
    clock.add(3.0);
    w.offer(jpeg_frame(2, None));
    w.tick().await; // no change: no note
    assert_eq!(w.notes().len(), 1);
    assert_eq!(client.previous.lock().unwrap()[1].as_deref(), Some("A desk with a mug.")); // the describer compares
    clock.add(8.0);
    w.offer(jpeg_frame(3, None)); // keep the camera alive...
    w.set_last_describe_at(clock.t()); // ...but not due for a describe yet
    w.tick().await;
    // Stamped with the view that confirmed it (16:43:03), not with the time of the tick (16:43:11).
    assert_eq!(w.notes().last().unwrap(), "[vision 16:43:03] No change since 16:43:00: A desk with a mug.");
    let n = w.notes().len();
    clock.add(25.0); // 16:43:36, a full refresh later
    w.offer(jpeg_frame(3, None));
    w.set_last_describe_at(clock.t());
    w.tick().await;
    assert_eq!(w.notes().len(), n); // nothing newer confirmed it: no repeat of a stale line
    clock.add(3.0); // 16:43:39: due again
    w.offer(jpeg_frame(4, None));
    w.tick().await;
    assert_eq!(w.notes().last().unwrap(), "[vision 16:43:39] A hand lifts the mug. Change: the mug was picked up");
    w.stop().await;
}

#[tokio::test]
async fn test_camera_loss_is_said_once_after_a_grace_and_board_loss_counts() {
    let clock = TestClock::new();
    let cam = Arc::new(AtomicBool::new(true));
    let client = FakeClient::new(&[("A desk.", "")]);
    let c2 = cam.clone();
    let w = Arc::new(
        SceneWatcher::new(Some(client), Some(SceneConfig { camera_lost_secs: 3.0, ..SceneConfig::default() }))
            .with_clock(clock.clock_fn())
            .with_wall(clock.wall_fn())
            .with_camera_ok(Arc::new(move || c2.load(Ordering::SeqCst))),
    );
    w.start(false);
    w.tick().await;
    assert!(w.notes().is_empty()); // inside the start grace: nothing said
    w.offer(jpeg_frame(1, None));
    w.tick().await;
    assert_eq!(w.notes().len(), 1);
    clock.add(3.5); // no frame for longer than camera_lost_secs
    w.tick().await;
    w.tick().await;
    let lost: Vec<String> = w.notes().into_iter().filter(|n| n.contains("Camera lost")).collect();
    assert_eq!(
        lost,
        vec!["[vision 16:43:03] Camera lost: you cannot see anything right now. \
              Your last view, from 16:43:00, may be out of date."
            .to_string()]
    );
    let look = w.look(None, None).await;
    assert_eq!(look["ok"], json!(false));
    assert!(look["reason"].as_str().unwrap().contains("not connected"));
    assert_eq!(look["view"], json!("A desk."));
    // The board goes away while frames are fresh: still lost.
    w.offer(jpeg_frame(2, None));
    cam.store(false, Ordering::SeqCst);
    assert_eq!(w.camera_state(clock.t()), "lost");
    w.stop().await;
}

#[tokio::test]
async fn test_camera_never_delivering_a_frame_is_reported_after_the_grace() {
    let clock = TestClock::new();
    let w = watcher(FakeClient::new(&[]), &clock);
    w.start(false);
    clock.add(3.1);
    w.tick().await;
    assert_eq!(w.notes(), vec!["[vision 16:43:03] Camera lost: you cannot see anything right now.".to_string()]);
    w.stop().await;
}

/// A describer that blocks until the gate opens (or 2 s pass).
struct Slow {
    gate: Arc<(StdMutex<bool>, Condvar)>,
}

impl SceneClient for Slow {
    fn describe(&self, _image: &[u8], _mime: &str, _previous: Option<&str>) -> anyhow::Result<(String, String)> {
        let (lock, cv) = &*self.gate;
        let open = lock.lock().unwrap();
        let _ = cv.wait_timeout_while(open, Duration::from_secs(2), |open| !*open).unwrap();
        Ok(("late".into(), "".into()))
    }
    fn locate(&self, _image: &[u8], _mime: &str, _target: &str) -> anyhow::Result<Map<String, Value>> {
        Ok(Map::new())
    }
}

#[tokio::test]
async fn test_slow_describe_times_out_and_blindness_is_said_once() {
    let clock = TestClock::new();
    let gate = Arc::new((StdMutex::new(false), Condvar::new()));
    let w = watcher_with(
        Some(Arc::new(Slow { gate: gate.clone() })),
        &clock,
        SceneConfig { timeout_secs: 0.05, ..SceneConfig::default() },
    );
    w.start(false);
    w.offer(jpeg_frame(1, None));
    w.tick().await;
    clock.add(3.0);
    w.offer(jpeg_frame(2, None));
    w.tick().await;
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    assert_eq!(w.failures(), 2);
    assert!(w.latest().is_none());
    let blind: Vec<String> = w.notes().into_iter().filter(|n| n.contains("cannot make out")).collect();
    assert_eq!(blind, vec!["[vision 16:43:00] You cannot make out the view right now.".to_string()]);
    w.stop().await;
}

/// Blocks every call until the gate opens.
struct Gated {
    gate: Arc<(StdMutex<bool>, Condvar)>,
}

impl Gated {
    fn wait(&self) {
        let (lock, cv) = &*self.gate;
        let open = lock.lock().unwrap();
        let _ = cv.wait_timeout_while(open, Duration::from_secs(5), |open| !*open).unwrap();
    }
}

impl SceneClient for Gated {
    fn describe(&self, _image: &[u8], _mime: &str, _previous: Option<&str>) -> anyhow::Result<(String, String)> {
        self.wait();
        Ok(("A desk.".into(), "".into()))
    }
    fn locate(&self, _image: &[u8], _mime: &str, _target: &str) -> anyhow::Result<Map<String, Value>> {
        self.wait();
        Ok(Map::new())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_describe_dropped_by_stop_is_not_a_failure() {
    // Python: stop() cancels the queued future, CancelledError leaves `_describe`
    // before any counter or note is touched.
    let clock = TestClock::new();
    let gate = Arc::new((StdMutex::new(false), Condvar::new()));
    let w = watcher_with(Some(Arc::new(Gated { gate: gate.clone() })), &clock, SceneConfig {
        timeout_secs: 5.0,
        ..test_config()
    });
    w.start(false);
    w.offer(jpeg_frame(1, None));
    let w1 = w.clone();
    let locating = tokio::spawn(async move { w1.locate("mug", None).await }); // holds the one worker
    tokio::time::sleep(Duration::from_millis(100)).await;
    let w2 = w.clone();
    let ticking = tokio::spawn(async move { w2.tick().await }); // its describe queues behind
    tokio::time::sleep(Duration::from_millis(100)).await;
    w.stop().await;
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    locating.await.unwrap();
    ticking.await.unwrap();
    assert_eq!((w.describes(), w.failures()), (0, 0));
    assert!(w.notes().is_empty() && w.latest().is_none());
}

#[tokio::test]
async fn test_look_reuses_a_fresh_view_and_refreshes_a_stale_one() {
    let clock = TestClock::new();
    let client = FakeClient::new(&[("A desk.", ""), ("A cat on the desk.", "a cat jumped up")]);
    let w = watcher_with(Some(client.clone()), &clock, SceneConfig { interval_secs: 30.0, ..test_config() });
    w.start(false);
    w.offer(jpeg_frame(1, None));
    w.tick().await;
    clock.add(1.0);
    let fresh = w.look(None, None).await;
    assert_eq!(
        Value::Object(fresh),
        json!({"ok": true, "view": "A desk.", "seen_at": "16:43:00", "age_secs": 1.0, "stale": false})
    );
    assert_eq!(client.images().len(), 1); // no second call for a 1 s old view

    clock.add(9.0); // 10 s old: look asks for a new one
    w.offer(jpeg_frame(2, None));

    let w2 = w.clone();
    let step = tokio::spawn(async move {
        for _ in 0..50 {
            tokio::task::yield_now().await;
            w2.tick().await;
        }
    });
    let refreshed = w.look(None, None).await;
    step.await.unwrap();
    assert_eq!(client.images().len(), 2); // interval was 30 s: look forced it
    assert_eq!(refreshed["ok"], json!(true));
    assert_eq!(refreshed["view"], json!("A cat on the desk."));
    assert_eq!(refreshed["seen_at"], json!("16:43:10"));
    w.stop().await;
}

#[tokio::test]
async fn test_look_outside_a_conversation_says_so() {
    let clock = TestClock::new();
    let w = watcher(FakeClient::new(&[]), &clock);
    let out = w.look(None, None).await;
    assert_eq!(Value::Object(out), json!({"ok": false, "reason": "vision only runs during a conversation"}));
}

#[tokio::test]
async fn test_look_newer_than_waits_for_a_newer_frame() {
    let clock = TestClock::new();
    let client = FakeClient::new(&[("A desk.", ""), ("A window.", "the view turned to a window")]);
    let w = watcher(client.clone(), &clock);
    w.start(false);
    w.offer(jpeg_frame(1, None));
    w.tick().await;
    // A young view that predates the turn is not good enough.
    let turned_at = clock.t();
    clock.add(0.5);
    w.offer(jpeg_frame(2, None));
    let w2 = w.clone();
    let step = tokio::spawn(async move {
        for _ in 0..50 {
            tokio::task::yield_now().await;
            w2.tick().await;
        }
    });
    let out = w.look(None, Some(turned_at)).await;
    step.await.unwrap();
    assert_eq!(out["ok"], json!(true));
    assert_eq!(out["view"], json!("A window."));
    assert_eq!(client.images().len(), 2);
    w.stop().await;
}

/// Python's test also chdirs into a temp dir and points HOME there; the watcher has no file
/// paths at all, so the Rust test keeps the behavioral assertions (tests must not mutate the
/// process environment) and checks the temp dir it would have used stays empty.
#[tokio::test]
async fn test_a_full_cycle_writes_nothing_to_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new();
    let w = watcher(FakeClient::new(&[("A desk.", ""), ("A desk.", "")]), &clock);
    w.start(false);
    for i in 0..4 {
        w.offer(jpeg_frame(i, None));
        clock.add(3.0);
        w.tick().await; // two describes answer, two fail: both paths run
    }
    assert_eq!((w.describes(), w.failures()), (2, 2));
    let out = w.look(None, None).await; // the forced describe fails too: bounded, and says so
    assert_eq!(out["ok"], json!(false));
    w.stop().await;
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn test_a_bad_frame_counts_as_a_failure() {
    let clock = TestClock::new();
    let w = watcher(FakeClient::new(&[("A desk.", "")]), &clock);
    w.start(false);
    w.offer(json!({"seq": 1, "w": 160, "h": 120, "fmt": "jpeg", "b64": "!!"}));
    w.tick().await;
    assert_eq!((w.describes(), w.failures()), (0, 1));
    assert_eq!(w.notes(), vec!["[vision 16:43:00] You cannot make out the view right now.".to_string()]);
    w.stop().await;
}

#[tokio::test]
async fn test_newest_frame_ages_out() {
    let clock = TestClock::new();
    let w = watcher(FakeClient::new(&[]), &clock);
    w.start(false);
    assert!(w.newest_frame().is_none());
    w.offer(jpeg_frame(1, None));
    assert_eq!(w.newest_frame().unwrap()["seq"], json!(1));
    clock.add(3.5);
    assert!(w.newest_frame().is_none());
    assert!(w.held_frame().is_some());
    w.stop().await;
}

#[tokio::test]
async fn test_locate_uses_the_newest_frame_and_echoes_the_pose() {
    let clock = TestClock::new();
    let w = watcher(FakeClient::new(&[]), &clock);
    w.start(false);
    let mut frame = jpeg_frame(1, None);
    frame["yaw"] = json!(-30);
    frame["pitch"] = json!(50.5);
    w.offer(frame);
    let out = w.locate("mug", None).await;
    assert_eq!(
        Value::Object(out),
        json!({"ok": true, "visible": true, "x": 12.0, "y": -40.0, "what": "mug", "yaw": -30.0, "pitch": 50.5})
    );
    assert!(w.held_frame().is_some()); // a locate does not use the frame up
    // Only a frame after `newer_than`; none comes, and the board has gone.
    let floor = clock.t();
    let w_lost = watcher_with(
        Some(FakeClient::new(&[])),
        &clock,
        SceneConfig { camera_lost_secs: 0.1, ..test_config() },
    );
    w_lost.start(false);
    w_lost.offer(jpeg_frame(2, None));
    let out = w_lost.locate("mug", Some(floor)).await;
    assert_eq!(Value::Object(out), json!({"ok": false, "reason": "no new frame came in time"}));
    clock.add(1.0);
    let out = w_lost.locate("mug", Some(floor)).await;
    assert_eq!(Value::Object(out), json!({"ok": false, "reason": NO_CAMERA}));
    w.stop().await;
    w_lost.stop().await;
    assert_eq!(
        Value::Object(w.locate("mug", None).await),
        json!({"ok": false, "reason": "vision only runs during a conversation"})
    );
}

#[tokio::test]
async fn test_locate_with_a_bad_frame_cannot_check() {
    let clock = TestClock::new();
    let w = watcher(FakeClient::new(&[]), &clock);
    w.start(false);
    w.offer(json!({"seq": 1, "w": 160, "h": 120, "fmt": "png", "b64": "AAAA"}));
    let out = w.locate("mug", None).await;
    assert_eq!(Value::Object(out), json!({"ok": false, "reason": "could not check the view"}));
    w.stop().await;
}

#[tokio::test]
async fn test_the_loop_runs_until_stop() {
    let client = FakeClient::new(&[("A desk.", "")]);
    let w = Arc::new(SceneWatcher::new(Some(client.clone()), None).with_tick_secs(0.01));
    w.start(true);
    w.offer(jpeg_frame(1, None));
    for _ in 0..200 {
        if !w.notes().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(w.notes().len(), 1);
    assert!(w.notes()[0].ends_with("] A desk."));
    w.stop().await;
    assert!(!w.active() && w.notes().is_empty());
}

// ---- test_daemon_scene.py: what the daemon relies on -------------------------------------

#[tokio::test]
async fn test_frames_reach_the_scene_only_during_a_conversation() {
    let w = Arc::new(SceneWatcher::new(Some(FakeClient::new(&[])), Some(SceneConfig::default())));
    let frame = |seq: i64| {
        let mut f = jpeg_frame(seq, Some(b"x"));
        f["yaw"] = json!(-30);
        f["pitch"] = json!(50);
        f
    };
    w.offer(frame(1));
    assert!(w.held_frame().is_none()); // no conversation: nothing held
    w.start(false);
    w.offer(frame(2));
    assert_eq!(w.held_frame().unwrap()["seq"], json!(2));
    w.stop().await;
    assert!(w.held_frame().is_none());
}

#[tokio::test]
async fn test_board_loss_is_camera_loss() {
    let connected = Arc::new(AtomicBool::new(true));
    let c2 = connected.clone();
    let w = Arc::new(
        SceneWatcher::new(Some(FakeClient::new(&[])), Some(SceneConfig::default()))
            .with_camera_ok(Arc::new(move || c2.load(Ordering::SeqCst))),
    );
    w.start(false);
    w.offer(jpeg_frame(1, None));
    assert_eq!(w.camera_state(w.clock()), "ok");
    connected.store(false, Ordering::SeqCst);
    assert_eq!(w.camera_state(w.clock()), "lost");
    w.stop().await;
}

#[test]
fn test_scene_watcher_without_a_client_is_inert() {
    let w = Arc::new(SceneWatcher::new(None, None));
    w.start(false);
    w.offer(jpeg_frame(1, None));
    assert!(!w.enabled() && !w.active() && w.held_frame().is_none());
}

// ---- the network client (no network: the SDK call is replaced) ----------------------------------

struct FakeSdk {
    reply: Value,
    calls: Arc<StdMutex<Vec<Value>>>,
}

impl Responses for FakeSdk {
    fn create(&self, body: Value) -> anyhow::Result<Value> {
        self.calls.lock().unwrap().push(body);
        Ok(self.reply.clone())
    }
}

fn reply(text: &str) -> Value {
    json!({
        "status": "completed",
        "incomplete_details": null,
        "output": [{"type": "message", "content": [{"type": "output_text", "text": text}]}],
        "usage": {"input_tokens": 1_000_000, "output_tokens": 0},
    })
}

#[test]
fn test_request_is_not_stored_and_sends_one_low_detail_image() {
    let c = OpenAISceneClient::new("gpt-5.4-nano", Some("sk-test"), DEFAULT_TIMEOUT_SECS).unwrap();
    let body = c.request(b"\xff\xd8abc", "image/jpeg", Some("A desk."));
    assert_eq!(body["store"], json!(false));
    assert_eq!(body["model"], json!("gpt-5.4-nano"));
    assert_eq!(body["reasoning"], json!({"effort": "low"}));
    assert!(body["max_output_tokens"].as_i64().unwrap() >= 1024);
    let parts = body["input"][0]["content"].as_array().unwrap();
    assert_eq!(parts.iter().map(|p| p["type"].as_str().unwrap()).collect::<Vec<_>>(), ["input_text", "input_image"]);
    assert_eq!(parts[1]["detail"], json!("low"));
    assert!(parts[1]["image_url"].as_str().unwrap().starts_with("data:image/jpeg;base64,"));
    assert!(parts[0]["text"].as_str().unwrap().contains("Previous description: A desk."));
    let fmt = &body["text"]["format"];
    assert_eq!(fmt["type"], json!("json_schema"));
    assert_eq!(fmt["strict"], json!(true));
    assert_eq!(fmt["name"], json!("scene"));
    assert_eq!(fmt["schema"], schema());
    let first = c.request(b"\xff\xd8abc", "image/jpeg", None);
    assert!(first["input"][0]["content"][0]["text"]
        .as_str()
        .unwrap()
        .ends_with("\nThere is no previous description; set change to an empty string."));
}

#[test]
fn test_locate_request_names_the_target_tidied_and_capped() {
    let c = OpenAISceneClient::new("gpt-5.4-nano", Some("sk-test"), DEFAULT_TIMEOUT_SECS).unwrap();
    let target = format!("  the green\n mug {}", "y".repeat(200));
    let body = c.locate_request(b"\xff\xd8abc", "image/jpeg", &target);
    let text = body["input"][0]["content"][0]["text"].as_str().unwrap();
    let expect: String = format!("the green mug {}", "y".repeat(200)).chars().take(80).collect();
    assert_eq!(text, format!("{LOCATE_PROMPT}{expect}"));
    assert_eq!(body["text"]["format"]["name"], json!("locate"));
    assert_eq!(body["text"]["format"]["schema"], locate_schema());
    assert_eq!(body["store"], json!(false));
}

#[test]
fn test_describe_parses_the_sdk_answer() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = Arc::new(StdMutex::new(vec![]));
    let c = OpenAISceneClient::with_responses(
        "gpt-5.4-nano",
        Box::new(FakeSdk { reply: reply(r#"{"scene":"A mug.","change":""}"#), calls: calls.clone() }),
    )
    .with_ledger(Ledger::at(tmp.path()));
    assert_eq!(c.describe(b"\xff\xd8x", "image/jpeg", None).unwrap(), ("A mug.".into(), "".into()));
    assert_eq!(calls.lock().unwrap()[0]["store"], json!(false));
}

#[test]
fn test_an_empty_answer_is_a_runtime_error() {
    let tmp = tempfile::tempdir().unwrap();
    let mut r = reply("  ");
    r["status"] = json!("incomplete");
    r["incomplete_details"] = json!({"reason": "max_output_tokens"});
    let c = OpenAISceneClient::with_responses("m", Box::new(FakeSdk { reply: r, calls: Arc::default() }))
        .with_ledger(Ledger::at(tmp.path()));
    let err = c.describe(b"x", "image/jpeg", None).unwrap_err();
    assert_eq!(error_name(&err), "RuntimeError");
    assert_eq!(
        err.to_string(),
        "empty answer (status=incomplete, incomplete=IncompleteDetails(reason='max_output_tokens'))"
    );
    let err = c.locate(b"x", "image/jpeg", "mug").unwrap_err();
    assert_eq!(error_name(&err), "RuntimeError");
}

#[test]
fn test_the_camera_describe_and_locate_are_metered() {
    let tmp = tempfile::tempdir().unwrap();
    let c = OpenAISceneClient::with_responses(
        "gpt-5.4-nano",
        Box::new(FakeSdk { reply: reply("not json"), calls: Arc::default() }),
    )
    .with_ledger(Ledger::at(tmp.path()));
    c.describe(b"x", "image/jpeg", None).unwrap();
    // not a locate answer; the call still cost
    let err = c.locate(b"x", "image/jpeg", "mug").unwrap_err();
    assert!(err.downcast_ref::<serde_json::Error>().is_some());
    let today = chrono::Local::now().date_naive().to_string();
    let rows = eve_spend::day_rows(&today, Some(tmp.path()));
    assert_eq!(rows.iter().map(|r| r["f"].clone()).collect::<Vec<_>>(), vec![json!("camera"), json!("camera")]);
    assert!((rows[0]["usd"].as_f64().unwrap() - 0.20).abs() < 1e-9, "{:?}", rows[0]);
}

#[test]
fn test_make_scene_client_needs_a_key_and_the_switch() {
    let no_key: HashMap<String, String> = HashMap::new();
    assert!(make_scene_client(&SceneConfig::default(), Some(&no_key)).is_none());
    let key: HashMap<String, String> = [("OPENROUTER_API_KEY".to_string(), "sk-test".to_string())].into();
    assert!(make_scene_client(&SceneConfig { enabled: false, ..SceneConfig::default() }, Some(&key)).is_none());
    assert!(make_scene_client(&SceneConfig::default(), Some(&key)).is_some());
}

#[test]
fn test_py_repr_of_a_locate_answer() {
    let loc = json!({"visible": true, "x": 12.0, "y": -3.5, "what": "a mug's handle"});
    assert_eq!(py_repr(&loc), r#"{'visible': True, 'x': 12.0, 'y': -3.5, 'what': "a mug's handle"}"#);
}

#[test]
fn test_os_error_text_matches_python() {
    let e = std::fs::read("/nonexistent/eve-scene-test.jpg").unwrap_err();
    assert_eq!(
        py_os_error(&e, "/nonexistent/eve-scene-test.jpg"),
        "[Errno 2] No such file or directory: '/nonexistent/eve-scene-test.jpg'"
    );
}

#[test]
#[ignore = "live: needs OPENROUTER_API_KEY and an image at EVE_SCENE_TEST_IMAGE"]
fn test_live_scene_test() {
    let path = std::env::var("EVE_SCENE_TEST_IMAGE").unwrap();
    assert_eq!(run_scene_test(&path, Some("mug")), 0);
}
