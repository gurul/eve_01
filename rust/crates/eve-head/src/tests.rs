//! Port of bridge/tests/test_head.py: pose geometry, the look command, relative moves from the echoed
//! pose, and the look_around / find routines with a fake scene and a fake clock.

use super::*;

#[derive(Clone, Default)]
struct FakeClock {
    t: Arc<Mutex<f64>>,
}

impl FakeClock {
    fn clock(&self) -> Clock {
        let t = self.t.clone();
        Arc::new(move || *t.lock())
    }
    fn sleep(&self) -> Sleep {
        let t = self.t.clone();
        Arc::new(move |secs: f64| {
            *t.lock() += secs;
            Box::pin(async {})
        })
    }
}

#[derive(Clone)]
struct Wire {
    sent: Arc<Mutex<Vec<Value>>>,
    ok: bool,
}

impl Wire {
    fn new(ok: bool) -> Self {
        Wire { sent: Arc::new(Mutex::new(Vec::new())), ok }
    }
    fn send(&self) -> Sender {
        let sent = self.sent.clone();
        let ok = self.ok;
        Arc::new(move |cmd: Value| {
            sent.lock().push(cmd);
            Box::pin(async move { ok })
        })
    }
    fn last(&self) -> Value {
        self.sent.lock().last().cloned().expect("something was sent")
    }
}

fn head(wire: Option<&Wire>, clock: Option<&FakeClock>, connected: bool) -> Arc<Head> {
    let clock = clock.cloned().unwrap_or_default();
    let wire = wire.cloned().unwrap_or_else(|| Wire::new(true));
    Arc::new(Head::with(wire.send(), Arc::new(move || connected), clock.clock(), clock.sleep()))
}

fn run<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
}

#[test]
fn test_clamp_and_command() {
    assert_eq!(clamp_pose(-200.0, 100.0), (-YAW_MAX, 85, true));
    assert_eq!(clamp_pose(30.4, 44.6), (30, 45, false));
    assert_eq!(build_look_cmd(-60, 50, 99999), json!({"cmd": "look", "yaw": -60, "pitch": 50, "hold": 65535}));
}

#[test]
fn test_clamp_rounds_half_to_even_like_python() {
    assert_eq!(clamp_pose(30.5, 44.5), (30, 44, false));
    assert_eq!(clamp_pose(31.5, 45.5), (32, 46, false));
}

#[test]
fn test_direction_words_follow_the_robot_frame() {
    assert_eq!(direction_word(0.0), "straight ahead");
    assert_eq!(direction_word(-25.0), "a little to my left");
    assert_eq!(direction_word(60.0), "to my right");
    assert_eq!(direction_word(-100.0), "far to my left");
    assert_eq!(direction_word(120.0), "behind me on my right");
}

#[test]
fn test_target_pose_centres_a_point_like_the_firmware() {
    // Right edge of a frame taken straight ahead: half the 66° field of view to the right.
    assert_eq!(target_pose(0.0, 45.0, 100.0, 0.0), (33, 45, false));
    // Bottom of the frame: look down by half the vertical field (24.75°).
    assert_eq!(target_pose(0.0, 45.0, 0.0, 100.0), (0, 20, false));
    assert_eq!(target_pose(110.0, 45.0, 100.0, 0.0), (YAW_MAX, 45, true));
}

#[test]
fn test_absolute_and_relative_moves_use_the_echoed_pose() {
    run(async {
        let wire = Wire::new(true);
        let h = head(Some(&wire), None, true);
        let r = h.move_to(Some(-60.0), Some(30.0), false, 10.0).await;
        assert_eq!(r, json!({"ok": true, "yaw": -60, "pitch": 30, "facing": "to my left", "hold_secs": 10.0}));
        assert_eq!(wire.last(), json!({"cmd": "look", "yaw": -60, "pitch": 30, "hold": 10000}));
        h.observe(&json!(-55), &json!(32)); // the board says where the head really is
        let r = h.move_to(Some(-15.0), None, true, DEFAULT_HOLD_SECS).await;
        assert_eq!((r["yaw"].as_i64(), r["pitch"].as_i64()), (Some(-70), Some(32))); // from -55, not -60
        let r = h.move_to(Some(-90.0), Some(0.0), true, DEFAULT_HOLD_SECS).await;
        assert_eq!(r["yaw"].as_i64(), Some(-YAW_MAX)); // clamped: says it hit the limit
        assert!(r.get("note").is_some());
        h.observe(&json!(true), &json!("x")); // junk is ignored
        assert_eq!((h.yaw(), h.pitch()), (-120.0, 32.0));
    });
}

#[test]
fn test_observe_records_when_the_pose_was_echoed() {
    let clock = FakeClock::default();
    let h = head(None, Some(&clock), true);
    assert_eq!(h.pose_at(), f64::NEG_INFINITY);
    *clock.t.lock() = 7.5;
    h.observe(&json!(10.5), &Value::Null);
    assert_eq!((h.yaw(), h.pitch(), h.pose_at()), (10.5, 45.0, 7.5));
}

#[test]
fn test_move_refuses_without_a_board_and_reports_a_lost_send() {
    run(async {
        let r = head(None, None, false).move_to(Some(10.0), Some(45.0), false, DEFAULT_HOLD_SECS).await;
        assert_eq!(r["ok"], json!(false));
        let lost = Wire::new(false);
        let r = head(Some(&lost), None, true).move_to(Some(10.0), Some(45.0), false, DEFAULT_HOLD_SECS).await;
        assert_eq!(r, json!({"ok": false, "reason": "the move did not reach the robot"}));
    });
}

#[test]
fn test_hold_is_capped_and_zero_hands_the_head_back() {
    run(async {
        let wire = Wire::new(true);
        let h = head(Some(&wire), None, true);
        h.move_to(Some(0.0), Some(45.0), false, 500.0).await;
        assert_eq!(wire.last()["hold"], json!(60000));
        h.move_to(Some(0.0), Some(45.0), false, 0.0).await;
        assert_eq!(wire.last()["hold"], json!(0));
    });
}

#[test]
fn test_on_move_is_told_the_hold() {
    run(async {
        let h = head(None, None, true);
        let holds = Arc::new(Mutex::new(Vec::new()));
        let hs = holds.clone();
        h.set_on_move(Some(Arc::new(move |hold| hs.lock().push(hold))));
        assert_eq!(h.move_to(Some(-60.0), Some(45.0), false, 15.0).await["ok"], json!(true));
        assert_eq!(*holds.lock(), vec![15.0]);
        // A move that did not land is not a hold.
        let lost = Wire::new(false);
        let failing = head(Some(&lost), None, true);
        let hs = holds.clone();
        failing.set_on_move(Some(Arc::new(move |hold| hs.lock().push(hold))));
        assert_eq!(failing.move_to(Some(10.0), Some(45.0), false, DEFAULT_HOLD_SECS).await["ok"], json!(false));
        assert_eq!(*holds.lock(), vec![15.0]);
    });
}

#[test]
fn test_find_collapses_whitespace_like_python_split_and_caps_the_target() {
    run(async {
        let h = head(None, None, true);
        let scene = FakeScene::new(&h, vec![], Some(50), false);
        find(&h, &scene, " my\u{1c}green\u{3000} \tmug ", SWEEP, SETTLE_SECS, DEFAULT_HOLD_SECS).await;
        assert_eq!(scene.locates.lock()[0].0, "my green mug");
        let long = "é".repeat(100);
        let scene = FakeScene::new(&h, vec![], Some(50), false);
        find(&h, &scene, &long, SWEEP, SETTLE_SECS, DEFAULT_HOLD_SECS).await;
        assert_eq!(scene.locates.lock()[0].0.chars().count(), 80);
        let blank = find(&h, &scene, "\u{1d}\u{1f}", SWEEP, SETTLE_SECS, DEFAULT_HOLD_SECS).await;
        assert_eq!(blank, json!({"ok": false, "reason": "no target given"}));
    });
}

struct OddScene;

#[async_trait::async_trait]
impl Scene for OddScene {
    async fn look(&self, _fresh_secs: f64, _newer_than: Option<f64>) -> Value {
        json!({"ok": false})
    }
    async fn locate(&self, _target: &str, _newer_than: Option<f64>) -> Value {
        // A null yaw falls back to the head's pose; a bool pitch is 1 in Python's arithmetic.
        json!({"ok": true, "visible": true, "x": 0, "y": 0, "what": "", "yaw": null, "pitch": true})
    }
}

#[test]
fn test_find_reads_a_located_pose_as_python_arithmetic_does() {
    run(async {
        let h = head(None, None, true);
        h.observe(&json!(30), &Value::Null);
        let out = find(&h, &OddScene, "mug", SWEEP, SETTLE_SECS, DEFAULT_HOLD_SECS).await;
        assert_eq!(
            out,
            json!({"ok": true, "found": true, "what": "mug", "facing": "a little to my right", "yaw": 30, "pitch": 5})
        );
        let out = look_around(&h, &OddScene, &[(0, 50)], SETTLE_SECS).await;
        assert_eq!(out["views"][0], json!({"direction": "straight ahead", "yaw": 0, "view": null, "reason": "no clear view"}));
    });
}

#[test]
fn test_default_clock_is_the_system_monotonic_clock() {
    // A system clock (uptime), not seconds since this process started using it.
    let a = monotonic();
    assert!(a > 0.0);
    std::thread::sleep(std::time::Duration::from_millis(5));
    assert!(monotonic() >= a + 0.004);
    assert!(Head::new(Wire::new(true).send()).now() >= a);
}

#[test]
fn test_pose_moves_match_the_backend_numbers() {
    assert_eq!(pose_move("bit_left"), Some(PoseMove { yaw: Some(-15), pitch: None, relative: true }));
    assert_eq!(pose_move("center"), Some(PoseMove { yaw: Some(0), pitch: Some(45), relative: false }));
    assert_eq!(pose_move("ceiling").unwrap().pitch, Some(PITCH_MAX));
    assert_eq!(POSE_MOVES.len(), 13);
    assert_eq!(pose_move("none"), None);
}

/// look/locate answers keyed by the yaw the head was sent to last.
struct FakeScene {
    h: Arc<Head>,
    views: Vec<(i64, &'static str)>,
    found_at: Option<i64>,
    lost: bool,
    looks: Mutex<Vec<(f64, Option<f64>)>>,
    locates: Mutex<Vec<(String, Option<f64>)>>,
}

impl FakeScene {
    fn new(h: &Arc<Head>, views: Vec<(i64, &'static str)>, found_at: Option<i64>, lost: bool) -> Self {
        FakeScene {
            h: h.clone(),
            views,
            found_at,
            lost,
            looks: Mutex::new(Vec::new()),
            locates: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl Scene for FakeScene {
    async fn look(&self, fresh_secs: f64, newer_than: Option<f64>) -> Value {
        self.looks.lock().push((fresh_secs, newer_than));
        if self.lost {
            return json!({"ok": false, "reason": "the camera is not connected, so nothing can be seen right now"});
        }
        let y = self.h.yaw() as i64;
        let view = self.views.iter().find(|(k, _)| *k == y).map(|(_, v)| *v).unwrap_or("a wall");
        json!({"ok": true, "view": view})
    }

    async fn locate(&self, target: &str, newer_than: Option<f64>) -> Value {
        self.locates.lock().push((target.to_string(), newer_than));
        if Some(self.h.yaw() as i64) == self.found_at {
            return json!({"ok": true, "visible": true, "x": 50.0, "y": -40.0, "what": "a green mug",
                          "yaw": self.h.yaw(), "pitch": self.h.pitch()});
        }
        json!({"ok": true, "visible": false, "x": 0.0, "y": 0.0, "what": "", "yaw": self.h.yaw(),
               "pitch": self.h.pitch()})
    }
}

#[test]
fn test_look_around_describes_each_stop_after_it_settles_then_faces_the_owner() {
    run(async {
        let (clock, wire) = (FakeClock::default(), Wire::new(true));
        let h = head(Some(&wire), Some(&clock), true);
        let scene =
            FakeScene::new(&h, vec![(-100, "a door"), (0, "the owner at the desk"), (100, "a window")], None, false);
        let out = look_around(&h, &scene, SWEEP, SETTLE_SECS).await;
        assert_eq!(out["ok"], json!(true));
        let dirs: Vec<&str> = out["views"].as_array().unwrap().iter().map(|v| v["direction"].as_str().unwrap()).collect();
        assert_eq!(dirs, ["far to my left", "to my left", "straight ahead", "to my right", "far to my right"]);
        assert_eq!(out["views"][0]["view"], json!("a door"));
        assert_eq!(out["views"][4]["view"], json!("a window"));
        // Every view was asked for from a frame newer than the moment the head stopped moving.
        let nts: Vec<Option<f64>> = scene.looks.lock().iter().map(|(_, nt)| *nt).collect();
        assert_eq!(nts, [Some(0.0), Some(1.0), Some(2.0), Some(3.0), Some(4.0)]);
        assert!(scene.looks.lock().iter().all(|(fs, _)| *fs == 0.0));
        assert_eq!(wire.last(), json!({"cmd": "look", "yaw": 0, "pitch": PITCH_LEVEL, "hold": 3000}));
    });
}

#[test]
fn test_look_around_stops_when_the_camera_is_gone() {
    run(async {
        let h = head(None, None, true);
        let out = look_around(&h, &FakeScene::new(&h, vec![], None, true), SWEEP, SETTLE_SECS).await;
        assert_eq!(out["ok"], json!(false));
        assert!(out["reason"].as_str().unwrap().contains("not connected"));
    });
}

#[test]
fn test_look_around_without_a_board_returns_the_move_failure_with_views() {
    run(async {
        let h = head(None, None, false);
        let out = look_around(&h, &FakeScene::new(&h, vec![], None, false), SWEEP, SETTLE_SECS).await;
        assert_eq!(
            out,
            json!({"ok": false, "reason": "the robot is not connected, so it cannot move", "views": []})
        );
    });
}

#[test]
fn test_find_checks_the_current_view_first_then_sweeps_and_centres() {
    run(async {
        let (clock, wire) = (FakeClock::default(), Wire::new(true));
        let h = head(Some(&wire), Some(&clock), true);
        let scene = FakeScene::new(&h, vec![], Some(50), false);
        let out = find(&h, &scene, "my green mug", SWEEP, SETTLE_SECS, DEFAULT_HOLD_SECS).await;
        assert_eq!(out["found"], json!(true));
        assert_eq!(out["what"], json!("a green mug"));
        // Current view first (no newer_than), then the sweep until the sighting at yaw 50.
        let locates = scene.locates.lock().clone();
        assert_eq!(locates[0], ("my green mug".to_string(), None));
        let nones: Vec<bool> = locates.iter().map(|(_, nt)| nt.is_none()).collect();
        assert_eq!(nones, [true, false, false, false, false]);
        // x=50 → +16.5° from yaw 50; y=-40 (above centre) → +9.9° pitch from 50.
        assert_eq!((out["yaw"].as_i64(), out["pitch"].as_i64()), (Some(66), Some(60)));
        assert_eq!(wire.last()["hold"], json!(15000));
    });
}

#[test]
fn test_find_reports_not_found_and_faces_the_owner() {
    run(async {
        let wire = Wire::new(true);
        let h = head(Some(&wire), None, true);
        let out = find(&h, &FakeScene::new(&h, vec![], None, false), "a unicorn", SWEEP, SETTLE_SECS, DEFAULT_HOLD_SECS)
            .await;
        assert_eq!(out["found"], json!(false));
        assert_eq!(out["searched_stops"], json!(6));
        assert_eq!(out["reason"], json!("could not see a unicorn anywhere the head can turn"));
        assert_eq!(wire.last()["yaw"], json!(0));
        let blank = find(&h, &FakeScene::new(&h, vec![], None, false), "  ", SWEEP, SETTLE_SECS, DEFAULT_HOLD_SECS).await;
        assert_eq!(blank["ok"], json!(false));
    });
}

#[test]
fn test_behind_is_the_neck_limit() {
    for yaw in [-120i64, 120] {
        assert_eq!(clamp_pose(yaw as f64 * 1.5, 45.0).0, yaw);
    }
}
