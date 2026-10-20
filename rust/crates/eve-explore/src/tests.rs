//! Idle explorer — schedule, change detector, budget, notes file, note taker.
//!
//! No board, no Mac, no network: the Explorer is ticked with a hand-rolled
//! clock, thumbnails come from the pure gray path, and the note client is a fake.
//! Port of bridge/tests/test_explore.py (the `_daemon` tests belong to eve-daemon).

use super::*;
use chrono::NaiveDate;
use std::sync::Mutex as StdMutex;

const W: i64 = 160;
const H: i64 = 120;
const IDLE: f64 = 601.0; // past after_secs

fn frame(level: u8) -> Frame {
    frame_seq(level, 1)
}

fn frame_seq(level: u8, seq: i64) -> Frame {
    Frame::new(seq, W, H, "gray", vec![level; (W * H) as usize])
}

fn cfg() -> ExploreConfig {
    ExploreConfig {
        enabled: true,
        after_secs: 600.0,
        notes_per_hour: 6.0,
        model: "fake".into(),
        notes_dir: PathBuf::from("/nonexistent"),
        cycle_wait_secs: 900.0,
    }
}

fn explorer() -> Explorer {
    Explorer::new(cfg(), 0.0)
}

fn explorer_without_notes(config: ExploreConfig) -> Explorer {
    Explorer::with_options(config, 0.0, ExplorerOptions { notes_enabled: false, ..Default::default() })
}

fn mode(explore: bool, reason: &str) -> Action {
    Action::Mode(Mode::new(explore, reason))
}

fn look(wp: (i64, i64)) -> Action {
    Action::Look(Look::new(wp.0, wp.1))
}

/// The actions are exactly one Note of this frame at this gaze.
fn note_at(actions: &[Action], level: u8, yaw: i64, pitch: i64) -> bool {
    match actions {
        [Action::Note(n)] => n.frame == frame(level) && (n.yaw, n.pitch) == (yaw, pitch),
        _ => false,
    }
}

fn the_note(actions: &[Action]) -> &Note {
    match actions {
        [Action::Note(n)] => n,
        other => panic!("not one note: {other:?}"),
    }
}

struct T {
    idle: f64,
    listening: bool,
    connected: bool,
}

impl Default for T {
    fn default() -> Self {
        T { idle: IDLE, listening: false, connected: true }
    }
}

fn tick(e: &mut Explorer, now: f64) -> Vec<Action> {
    tick_with(e, now, None, T::default())
}

fn tick_idle(e: &mut Explorer, now: f64, idle: f64) -> Vec<Action> {
    tick_with(e, now, None, T { idle, ..Default::default() })
}

fn tick_frame(e: &mut Explorer, now: f64, f: &Frame) -> Vec<Action> {
    tick_with(e, now, Some(f), T::default())
}

fn tick_with(e: &mut Explorer, now: f64, f: Option<&Frame>, t: T) -> Vec<Action> {
    e.tick(now, t.idle, t.listening, f, t.connected)
}

fn blockers() -> Vec<(T, &'static str)> {
    vec![
        (T { idle: 0.0, ..Default::default() }, "activity"),
        (T { listening: true, ..Default::default() }, "listen key"),
        (T { connected: false, ..Default::default() }, "board disconnected"),
    ]
}

fn hard_blockers() -> Vec<(T, &'static str)> {
    vec![
        (T { idle: 0.0, listening: true, ..Default::default() }, "listen key"),
        (T { idle: 0.0, connected: false, ..Default::default() }, "board disconnected"),
    ]
}

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn dt(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(y, mo, d).unwrap().and_hms_opt(h, mi, 0).unwrap()
}

/// Runs `f` with a tracing subscriber that captures the log text (caplog).
fn capture_logs<R>(f: impl FnOnce() -> R) -> (R, String) {
    #[derive(Clone)]
    struct Buf(Arc<StdMutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf(Arc::new(StdMutex::new(Vec::new())));
    let writer = buf.clone();
    let sub = tracing_subscriber::fmt().with_writer(move || writer.clone()).with_ansi(false).finish();
    let r = tracing::subscriber::with_default(sub, f);
    let out = buf.0.lock().unwrap().clone();
    (r, String::from_utf8(out).unwrap())
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
}

// ---- wire builders ----------------------------------------------------------

#[test]
fn test_build_look_clamps_to_wire_ranges() {
    assert_eq!(build_look_cmd(-200.0, 0.0, 6000), json!({"cmd": "look", "yaw": YAW_RANGE.0, "pitch": 5, "hold": 6000}));
    assert_eq!(build_look_cmd(200.0, 999.0, 6000), json!({"cmd": "look", "yaw": YAW_RANGE.1, "pitch": 85, "hold": 6000}));
    assert_eq!(build_look_cmd(20.4, 100.0, -1), json!({"cmd": "look", "yaw": 20, "pitch": 85, "hold": 0}));
    // key order on the wire
    assert_eq!(build_look_cmd(1.0, 40.0, 6000).to_string(), r#"{"cmd":"look","yaw":1,"pitch":40,"hold":6000}"#);
}

#[test]
fn test_build_mode() {
    assert_eq!(build_mode_cmd(true), json!({"cmd": "mode", "explore": true}));
    assert_eq!(build_mode_cmd(false), json!({"cmd": "mode", "explore": false}));
    assert_eq!(build_mode_cmd(true).to_string(), r#"{"cmd":"mode","explore":true}"#);
}

// ---- config -----------------------------------------------------------------

#[test]
fn test_configured_defaults() {
    let c = configured(Some(&env(&[])));
    // Exploring is explicit: the idle start ships off (owner, 2026-09-21); EVE_EXPLORE=1 turns it on.
    assert!(!c.enabled && c.after_secs == 600.0 && c.notes_per_hour == 6.0);
    assert!(configured(Some(&env(&[("EVE_EXPLORE", "1")]))).enabled);
    assert_eq!(c.model, "gpt-6-luna");
    assert_eq!(c.cycle_wait_secs, 900.0);
    assert_eq!(c.notes_dir, eve_vision::expanduser("~/.config/eve-bridge/notes"));
    assert_eq!(c, ExploreConfig::default());
}

#[test]
fn test_configured_env_knobs() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_str().unwrap();
    let c = configured(Some(&env(&[
        ("EVE_EXPLORE", "0"),
        ("EVE_EXPLORE_AFTER_MIN", "2.5"),
        ("EVE_NOTES_PER_HOUR", "12"),
        ("EVE_NOTES_MODEL", "gpt-x"),
        ("EVE_NOTES_DIR", dir),
        ("EVE_EXPLORE_CYCLE_MIN", "1"),
    ])));
    assert!(!c.enabled);
    assert_eq!(
        (c.after_secs, c.notes_per_hour, c.model.as_str(), c.notes_dir.as_path(), c.cycle_wait_secs),
        (150.0, 12.0, "gpt-x", tmp.path(), 60.0)
    );
}

#[test]
fn test_configured_bad_number_falls_back() {
    let (c, logs) = capture_logs(|| configured(Some(&env(&[("EVE_NOTES_PER_HOUR", "lots")]))));
    assert_eq!(c.notes_per_hour, 6.0);
    assert!(logs.contains("explore: EVE_NOTES_PER_HOUR='lots' is not a number; using 6"), "{logs}");
    let (c, logs) = capture_logs(|| configured(Some(&env(&[("EVE_EXPLORE_AFTER_MIN", "-3")]))));
    assert_eq!(c.after_secs, 600.0);
    assert!(logs.contains("explore: EVE_EXPLORE_AFTER_MIN='-3' is negative; using 10"), "{logs}");
}

#[test]
fn test_configured_parses_like_python() {
    // float() accepts what Python accepts; EVE_EXPLORE takes the truthy words in any case.
    let c = configured(Some(&env(&[
        ("EVE_EXPLORE", " YES "),
        ("EVE_EXPLORE_AFTER_MIN", " 1_0 "),
        ("EVE_NOTES_PER_HOUR", "1e1"),
        ("EVE_EXPLORE_CYCLE_MIN", "inf"),
        ("EVE_NOTES_MODEL", "   "),
        ("EVE_NOTES_DIR", "  "),
    ])));
    assert!(c.enabled);
    assert_eq!(c.after_secs, 600.0);
    assert_eq!(c.notes_per_hour, 10.0);
    assert!(c.cycle_wait_secs.is_infinite());
    assert_eq!(c.model, DEFAULT_MODEL);
    assert_eq!(c.notes_dir, eve_vision::expanduser(DEFAULT_NOTES_DIR));
    for bad in ["1__0", "_1", "1_", "0x10", "", "e5", "1e", "--1", "+-1", "five"] {
        assert_eq!(py_float(bad), None, "{bad:?}");
    }
    for (good, v) in [("+5", 5.0), (".5", 0.5), ("5.", 5.0), ("1E3", 1000.0), ("-Infinity", f64::NEG_INFINITY)] {
        assert_eq!(py_float(good), Some(v), "{good:?}");
    }
    assert!(py_float("NaN").unwrap().is_nan());
    assert!(!configured(Some(&env(&[("EVE_EXPLORE", "2")]))).enabled);
}

#[test]
fn test_python_number_formats() {
    assert_eq!(py_g(10.0), "10");
    assert_eq!(py_g(6.0), "6");
    assert_eq!(py_g(15.0), "15");
    assert_eq!(py_g(0.5), "0.5");
    assert_eq!(py_g(1234567.0), "1.23457e+06");
    assert_eq!(py_g(0.0001), "0.0001");
    assert_eq!(py_g(0.00001), "1e-05");
    assert_eq!(fmt0(0.5), "0");
    assert_eq!(fmt0(1.5), "2");
    assert_eq!(fmt0(2.5), "2");
    assert_eq!(fmt0(10.016666), "10");
    assert_eq!(fmt0(f64::INFINITY), "inf");
    assert_eq!(fmt0(f64::NAN), "nan");
    assert_eq!(py_repr_str("lots"), "'lots'");
    assert_eq!(py_repr_str("it's"), "\"it's\"");
}

// ---- state machine ----------------------------------------------------------

#[test]
fn test_starts_only_when_idle_and_unblocked() {
    let mut e = explorer();
    assert_eq!(tick_idle(&mut e, 1.0, 599.0), vec![]);
    assert_eq!(tick_with(&mut e, 3.0, None, T { listening: true, ..Default::default() }), vec![]);
    assert_eq!(tick_with(&mut e, 4.0, None, T { connected: false, ..Default::default() }), vec![]);
    assert_eq!(e.state, Explorer::OFF);
    let actions = tick(&mut e, 5.0);
    assert_eq!(actions, vec![mode(true, "idle 10 min"), Action::Look(Look::with_hold(WAYPOINTS[0].0, WAYPOINTS[0].1, 6000))]);
    assert!(e.active());
}

#[test]
fn test_disabled_config_never_starts() {
    let mut e = Explorer::new(ExploreConfig { enabled: false, ..cfg() }, 0.0);
    assert_eq!(tick(&mut e, 5.0), vec![]);
    assert_eq!(e.state, Explorer::OFF);
}

#[test]
fn test_activity_stops_exploring() {
    for (t, reason) in blockers() {
        let mut e = explorer();
        tick(&mut e, 0.0);
        assert_eq!(tick_with(&mut e, 1.0, None, t), vec![mode(false, reason)], "{reason}");
        assert_eq!(e.state, Explorer::OFF);
        // And it does not restart until the idle clock climbs back past after_secs.
        assert_eq!(tick_idle(&mut e, 2.0, 100.0), vec![]);
    }
}

#[test]
fn test_waypoint_cadence_one_look_per_six_seconds() {
    let mut e = explorer_without_notes(cfg());
    let mut looks: Vec<(f64, Look)> = Vec::new();
    let mut now = 0.0;
    while e.state != Explorer::RESTING {
        for a in tick(&mut e, now) {
            if let Action::Look(lk) = a {
                looks.push((now, lk));
            }
        }
        now += 1.0;
    }
    assert_eq!(looks.iter().map(|(_, lk)| (lk.yaw, lk.pitch)).collect::<Vec<_>>(), *WAYPOINTS);
    assert!(looks.iter().all(|(_, lk)| lk.hold_ms == 6000));
    let times: Vec<f64> = looks.iter().map(|(t, _)| *t).collect();
    let gaps: Vec<f64> = times.windows(2).map(|w| w[1] - w[0]).collect();
    assert_eq!(gaps, vec![6.0; WAYPOINTS.len() - 1]);
    assert_eq!(e.cycles, 1);
}

/// A finished pan cycle sends nothing over the wire: the board stays in
/// explore mode and looks around on its own until the next cycle.
#[test]
fn test_cycle_end_rests_with_the_board_still_exploring() {
    let mut e = explorer_without_notes(ExploreConfig { cycle_wait_secs: 100.0, ..cfg() });
    let mut now = 0.0;
    let mut last = vec![];
    while e.state != Explorer::RESTING {
        last = tick(&mut e, now);
        now += 1.0;
    }
    assert_eq!(last, vec![Action::Rest(Rest::new("cycle complete, board looks around on its own; next pan in 2 min"))]);
    assert!(!last.iter().any(|a| matches!(a, Action::Mode(_))));
    assert!(e.on_board() && !e.active());
    let rest_started = now - 1.0;
    assert_eq!(tick(&mut e, rest_started + 50.0), vec![]);
    let actions = tick(&mut e, rest_started + 100.0);
    assert_eq!(actions[0], mode(true, "idle 10 min"));
    assert!(matches!(actions[1], Action::Look(_)));
}

/// The board was left exploring through the rest; a blocker must hand
/// the head back to the persona with an explicit mode-off.
#[test]
fn test_activity_while_resting_leaves_explore_mode() {
    for (t, reason) in blockers() {
        let mut e = explorer_without_notes(cfg());
        let mut now = 0.0;
        while e.state != Explorer::RESTING {
            tick(&mut e, now);
            now += 1.0;
        }
        assert_eq!(tick_with(&mut e, now, None, t), vec![mode(false, reason)], "{reason}");
        assert!(e.state == Explorer::OFF && !e.on_board());
    }
}

#[test]
fn test_on_board_tracks_exploring_and_resting() {
    let mut e = explorer_without_notes(ExploreConfig { cycle_wait_secs: 0.0, ..cfg() });
    assert!(!e.on_board());
    tick(&mut e, 0.0);
    assert!(e.on_board() && e.active());
    let mut now = 1.0;
    while e.state != Explorer::RESTING {
        tick(&mut e, now);
        now += 1.0;
    }
    assert!(e.on_board() && !e.active());
    e.reset();
    assert!(!e.on_board());
}

#[test]
fn test_frame_sampled_two_seconds_after_look_once_per_waypoint() {
    let mut e = explorer();
    tick(&mut e, 0.0);
    assert!(!e.wants_frame(1.9));
    assert_eq!(tick_frame(&mut e, 1.0, &frame(10)), vec![]); // too early: ignored
    assert!(e.wants_frame(2.0));
    let actions = tick_frame(&mut e, 2.0, &frame(10));
    assert!(note_at(&actions, 10, WAYPOINTS[0].0, WAYPOINTS[0].1));
    let n = the_note(&actions);
    assert!(n.thumb.is_some() && n.surprise.is_none()); // bank still cold
    assert!(!e.wants_frame(3.0));
    assert_eq!(tick_frame(&mut e, 3.0, &frame(200)), vec![]); // already sampled here
    // Next waypoint, new sample window.
    assert_eq!(tick(&mut e, 6.0), vec![Action::Look(Look::with_hold(WAYPOINTS[1].0, WAYPOINTS[1].1, 6000))]);
    assert!(note_at(&tick_frame(&mut e, 8.0, &frame(10)), 10, WAYPOINTS[1].0, WAYPOINTS[1].1));
}

#[test]
fn test_unchanged_view_spends_no_note_on_the_next_cycle() {
    let mut e = Explorer::new(ExploreConfig { cycle_wait_secs: 0.0, ..cfg() }, 0.0);
    e.bucket = TokenBucket::new(1000.0, 0.0);
    let mut notes = 0;
    let mut now = 0.0;
    let f = frame(50);
    for _ in 0..2 {
        loop {
            let actions = tick_frame(&mut e, now, &f);
            notes += actions.iter().filter(|a| matches!(a, Action::Note(_))).count();
            now += 1.0;
            if e.state == Explorer::RESTING {
                break;
            }
        }
        now += 1.0;
    }
    assert_eq!(notes, WAYPOINTS.len()); // first cycle: every waypoint is new
    assert_eq!(e.skipped_same, WAYPOINTS.len() as u64); // second cycle: nothing moved
}

#[test]
fn test_changed_view_spends_a_note() {
    let mut e = Explorer::new(ExploreConfig { cycle_wait_secs: 0.0, ..cfg() }, 0.0);
    e.bucket = TokenBucket::new(1000.0, 0.0);
    tick(&mut e, 0.0);
    assert!(note_at(&tick_frame(&mut e, 2.0, &frame(50)), 50, WAYPOINTS[0].0, WAYPOINTS[0].1));
    // Run the cycle out, rest is zero, restart at the same waypoint with a new view.
    let mut now = 3.0;
    while e.state != Explorer::RESTING {
        tick(&mut e, now);
        now += 1.0;
    }
    tick(&mut e, now); // restart at the first waypoint
    assert!(e.active() && e.waypoint() == WAYPOINTS[0]);
    assert!(note_at(&tick_frame(&mut e, now + 2.0, &frame(120)), 120, WAYPOINTS[0].0, WAYPOINTS[0].1));
}

#[test]
fn test_budget_exhausted_skips_note_but_keeps_reference_unchanged() {
    let mut e = explorer();
    e.bucket = TokenBucket::new(0.0, 0.0);
    tick(&mut e, 0.0);
    assert_eq!(tick_frame(&mut e, 2.0, &frame(50)), vec![]);
    assert_eq!(e.skipped_budget, 1);
    assert_eq!(e.detector.measure(0, &frame(50)), None); // nothing was kept
}

#[test]
fn test_notes_disabled_pans_without_notes() {
    let mut e = explorer_without_notes(cfg());
    tick(&mut e, 0.0);
    assert_eq!(tick_frame(&mut e, 2.0, &frame(50)), vec![]);
    assert_eq!(e.notes, 0);
}

// ---- change detector --------------------------------------------------------

#[test]
fn test_downscale_gray_box_average() {
    let (w, h) = (4, 2);
    let luma = [0u8, 0, 100, 100, 0, 0, 100, 100];
    assert_eq!(downscale_gray(w, h, &luma, 2, 1).unwrap(), vec![0, 100]);
    let err = downscale_gray(w, h, &luma[..7], 2, 1).unwrap_err();
    assert_eq!(err.0, "luma is 7 bytes, expected 8");
    let err = downscale_gray(w, h, &luma, 8, 1).unwrap_err();
    assert_eq!(err.0, "cannot downscale 4x2 to 8x1");
}

#[test]
fn test_mean_abs_diff() {
    assert_eq!(mean_abs_diff(&[0, 10], &[10, 30]).unwrap(), 15.0);
    assert_eq!(mean_abs_diff(&[0], &[0, 0]).unwrap_err().0, "thumbnails differ in size");
    assert!(mean_abs_diff(&[], &[]).is_err());
}

#[test]
fn test_change_detector_first_visit_then_threshold() {
    let mut d = ChangeDetector::new(12.0);
    assert_eq!(d.measure(0, &frame(50)), None);
    d.keep(0, &frame(50));
    let m = d.measure(0, &frame(55)).unwrap();
    assert!(m == 5.0 && m <= d.threshold);
    assert!(d.measure(0, &frame(70)).unwrap() > d.threshold);
    assert_eq!(d.measure(1, &frame(50)), None); // a different waypoint has its own memory
}

#[test]
fn test_change_detector_undecodable_counts_as_changed() {
    let mut d = ChangeDetector::with_thumb(Arc::new(|_: &Frame| None));
    d.keep(0, &frame(50));
    assert_eq!(d.measure(0, &frame(50)), None);
}

#[test]
fn test_frame_thumb_jpeg_without_imageio_is_none() {
    // Off macOS there is no ImageIO; on macOS this truncated JPEG does not decode. Either way: None.
    assert_eq!(frame_thumb(&Frame::new(1, 4, 2, "jpeg", vec![0xff, 0xd8])), None);
    #[cfg(not(target_os = "macos"))]
    {
        let png = encode_gray_png(W, H, &vec![100u8; (W * H) as usize]).unwrap();
        assert_eq!(quartz_thumb(&png, THUMB_W, THUMB_H), None);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn test_native_quartz_thumb_decodes_an_image_file() {
    // Any ImageIO-readable file goes through the Core Graphics path: a flat gray PNG
    // comes back as a 32x24 thumbnail of (about) the same level.
    let png = encode_gray_png(W, H, &vec![100u8; (W * H) as usize]).unwrap();
    let thumb = frame_thumb(&Frame::new(1, W, H, "jpeg", png)).expect("ImageIO decodes a PNG");
    assert_eq!(thumb.len(), THUMB_W * THUMB_H);
    assert!(thumb.iter().all(|&v| (v as i32 - 100).abs() <= 3), "{:?}", &thumb[..8]);
    assert_eq!(quartz_thumb(b"not an image", THUMB_W, THUMB_H), None);
}

// ---- manual explore (the owner asked) ---------------------------------------

#[test]
fn test_request_starts_at_once_and_ignores_idle() {
    let mut e = explorer();
    assert_eq!(tick_idle(&mut e, 1.0, 0.0), vec![]); // idle: nothing
    let actions = e.request(1.0, "requested by cli", false, true).unwrap();
    assert_eq!(actions, vec![mode(true, "requested by cli"), look(WAYPOINTS[0])]);
    assert!(e.active() && e.manual && e.started_reason == "requested by cli");
    // activity (a hook event, a running session) does not end a manual explore
    assert_eq!(tick_idle(&mut e, 2.0, 0.0), vec![]);
    assert_eq!(tick_idle(&mut e, 7.0, 0.0), vec![look(WAYPOINTS[1])]);
    assert_eq!(e.status()["manual"], json!(true));
    assert_eq!(e.status()["waypoint"], json!([WAYPOINTS[1].0, WAYPOINTS[1].1]));
}

#[test]
fn test_manual_explore_survives_rest_and_pans_again() {
    let mut e = Explorer::new(ExploreConfig { cycle_wait_secs: 30.0, ..cfg() }, 0.0);
    e.request(0.0, "requested by voice", false, true).unwrap();
    let mut now = 0.0;
    while e.active() {
        now += 6.0;
        tick_idle(&mut e, now, 0.0);
    }
    assert!(e.state == Explorer::RESTING && e.manual && e.cycles == 1);
    assert_eq!(tick_idle(&mut e, now + 10.0, 0.0), vec![]);
    assert_eq!(tick_idle(&mut e, now + 31.0, 0.0), vec![mode(true, "requested by voice"), look(WAYPOINTS[0])]);
    assert!(e.manual);
}

#[test]
fn test_hard_blockers_end_a_manual_explore_and_clear_manual() {
    for (t, reason) in hard_blockers() {
        let mut e = explorer();
        e.request(0.0, "requested by cli", false, true).unwrap();
        assert_eq!(tick_with(&mut e, 1.0, None, t), vec![mode(false, reason)], "{reason}");
        assert!(e.state == Explorer::OFF && !e.manual);
        // ... and afterwards the idle rule applies again
        assert_eq!(tick_idle(&mut e, 2.0, 0.0), vec![]);
    }
}

#[test]
fn test_request_is_refused_on_a_hard_blocker() {
    for (listening, connected, reason) in [(true, true, "listen key"), (false, false, "board disconnected")] {
        let mut e = explorer();
        let err = e.request(0.0, "requested by cli", listening, connected).unwrap_err();
        assert_eq!(err.to_string(), reason);
        assert!(e.state == Explorer::OFF && !e.manual);
    }
}

#[test]
fn test_request_while_resting_sends_only_a_look() {
    let mut e = explorer();
    let mut now = 0.0;
    tick(&mut e, now); // idle start
    while e.active() {
        now += 6.0;
        tick(&mut e, now);
    }
    assert_eq!(e.state, Explorer::RESTING);
    assert_eq!(e.request(now, "requested by cli", false, true).unwrap(), vec![look(WAYPOINTS[0])]);
    assert!(e.active() && e.manual);
}

#[test]
fn test_request_while_exploring_restarts_the_pan_without_mode() {
    let mut e = explorer();
    tick(&mut e, 0.0);
    tick(&mut e, 6.0); // at waypoint 1
    assert_eq!(e.waypoint(), WAYPOINTS[1]);
    assert_eq!(e.request(6.0, "requested by cli", false, true).unwrap(), vec![look(WAYPOINTS[0])]);
    assert!(e.waypoint() == WAYPOINTS[0] && e.manual);
}

#[test]
fn test_dismiss_sends_mode_off_only_when_on_board() {
    let mut e = explorer();
    assert_eq!(e.dismiss("touch"), vec![]); // nothing to undo
    e.request(0.0, "requested by cli", false, true).unwrap();
    assert_eq!(e.dismiss("touch (voice)"), vec![mode(false, "touch (voice)")]);
    assert!(e.state == Explorer::OFF && !e.manual);
    assert_eq!(e.dismiss("again"), vec![]);
}

#[test]
fn test_request_works_when_idle_start_is_disabled() {
    let mut e = Explorer::new(ExploreConfig { enabled: false, ..cfg() }, 0.0);
    assert_eq!(tick(&mut e, 1.0), vec![]); // never starts on its own
    assert_eq!(e.request(1.0, "requested by cli", false, true).unwrap()[0], mode(true, "requested by cli"));
    assert_eq!(tick_idle(&mut e, 2.0, 0.0), vec![]);
    assert!(e.active());
}

#[test]
fn test_status_when_off() {
    let e = explorer();
    assert_eq!(
        e.status(),
        json!({"state": "off", "manual": false, "reason": "", "waypoint": null, "cycles": 0, "notes": 0})
    );
    assert_eq!(
        e.status().to_string(),
        r#"{"state":"off","manual":false,"reason":"","waypoint":null,"cycles":0,"notes":0}"#
    );
}

// ---- token bucket -----------------------------------------------------------

#[test]
fn test_token_bucket_starts_full_then_refills_at_rate() {
    let mut b = TokenBucket::new(6.0, 0.0);
    assert!((0..6).all(|_| b.take(0.0)));
    assert!(!b.take(0.0));
    assert!(!b.take(599.0));
    assert!(b.take(600.0)); // one per ten minutes
    assert_eq!(b.available(600.0), 0.0);
    assert_eq!(b.available(3600.0 * 2.0), 6.0); // never above capacity
}

#[test]
fn test_token_bucket_zero_rate_never_grants() {
    let mut b = TokenBucket::new(0.0, 0.0);
    assert!(!b.take(0.0) && !b.take(10_000.0));
    // a fractional rate still holds one whole token
    let b = TokenBucket::new(0.5, 0.0);
    assert_eq!((b.capacity, b.tokens), (1.0, 1.0));
    let b = TokenBucket::with_capacity(6.0, 0.0, Some(2.0));
    assert_eq!((b.capacity, b.tokens), (2.0, 2.0));
}

// ---- notes file -------------------------------------------------------------

#[test]
fn test_note_line_format() {
    let when = dt(2026, 9, 5, 14, 7);
    assert_eq!(note_line(&when, 20, 40, "A lamp is on."), "- 14:07 yaw=+20 pitch=40 — A lamp is on.");
    assert!(note_line(&when, -45, 60, "x").starts_with("- 14:07 yaw=-45 pitch=60 — "));
}

#[test]
fn test_append_note_creates_private_dir_and_dated_file() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("notes");
    let when = dt(2026, 9, 5, 14, 7);
    let p = append_note(&d, &when, 20, 40, "one", None).unwrap();
    append_note(&d, &dt(2026, 9, 5, 14, 9), 0, 60, "two", None).unwrap();
    assert_eq!(p, d.join("2026-09-05.md"));
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "- 14:07 yaw=+20 pitch=40 — one\n- 14:09 yaw=+0 pitch=60 — two\n");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700);
}

#[test]
fn test_append_note_writes_extra_under_the_line() {
    let tmp = tempfile::tempdir().unwrap();
    let when = dt(2026, 9, 5, 14, 7);
    let p = append_note(tmp.path(), &when, 0, 40, "a mug", Some("![photo](x.jpg)")).unwrap();
    append_note(tmp.path(), &when, 0, 40, "b", Some("")).unwrap(); // empty extra is falsy
    assert_eq!(std::fs::read_to_string(p).unwrap(), "- 14:07 yaw=+0 pitch=40 — a mug\n![photo](x.jpg)\n- 14:07 yaw=+0 pitch=40 — b\n");
}

#[test]
fn test_read_notes_last_n_across_days() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    append_note(d, &dt(2026, 9, 4, 9, 0), 0, 40, "old", None).unwrap();
    append_note(d, &dt(2026, 9, 5, 9, 0), 0, 40, "a", None).unwrap();
    append_note(d, &dt(2026, 9, 5, 9, 1), 0, 40, "b", None).unwrap();
    assert_eq!(
        read_notes(d, 2).unwrap(),
        vec!["2026-09-05 - 09:00 yaw=+0 pitch=40 — a", "2026-09-05 - 09:01 yaw=+0 pitch=40 — b"]
    );
    assert_eq!(read_notes(d, 0).unwrap().len(), 3);
    assert!(read_notes(d, 0).unwrap()[0].starts_with("2026-09-04"));
    assert_eq!(read_notes(&d.join("absent"), 20).unwrap(), Vec::<String>::new());
}

#[test]
fn test_read_notes_skips_blank_lines_and_other_files() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    std::fs::write(d.join("2026-09-05.md"), "- a\r\n\n   \n- b\r- c").unwrap();
    std::fs::write(d.join("notes.txt"), "- not a note\n").unwrap();
    assert_eq!(read_notes(d, 0).unwrap(), vec!["2026-09-05 - a", "2026-09-05 - b", "2026-09-05 - c"]);
    assert_eq!(read_notes(d, 1).unwrap(), vec!["2026-09-05 - c"]);
}

#[test]
fn test_notes_cli_prints() {
    let tmp = tempfile::tempdir().unwrap();
    append_note(tmp.path(), &dt(2026, 9, 5, 9, 0), 0, 40, "hello", None).unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let c = ExploreConfig { notes_dir: tmp.path().to_path_buf(), ..cfg() };
    assert_eq!(run_notes_cli_to(5, Some(&c), &mut out, &mut err), 0);
    assert!(String::from_utf8(out).unwrap().contains("hello"));
    let mut out = Vec::new();
    let c = ExploreConfig { notes_dir: tmp.path().join("none"), ..cfg() };
    assert_eq!(run_notes_cli_to(5, Some(&c), &mut out, &mut err), 0);
    let text = String::from_utf8(out).unwrap();
    assert_eq!(text, format!("no notes yet in {}\n", tmp.path().join("none").display()));
}

// ---- note taker (fake client, no network) -----------------------------------

struct FakeClient {
    text: String,
    fail: Option<String>,
    delay: f64,
    calls: StdMutex<Vec<(usize, String)>>,
}

impl FakeClient {
    fn ok(text: &str) -> Arc<Self> {
        Arc::new(FakeClient { text: text.into(), fail: None, delay: 0.0, calls: StdMutex::new(vec![]) })
    }

    fn failing(msg: &str) -> Arc<Self> {
        Arc::new(FakeClient { text: String::new(), fail: Some(msg.into()), delay: 0.0, calls: StdMutex::new(vec![]) })
    }

    fn slow(delay: f64) -> Arc<Self> {
        Arc::new(FakeClient { text: "A quiet desk.".into(), fail: None, delay, calls: StdMutex::new(vec![]) })
    }
}

impl NoteClient for FakeClient {
    fn describe(&self, image: &[u8], mime: &str) -> anyhow::Result<String> {
        self.calls.lock().unwrap().push((image.len(), mime.to_string()));
        if self.delay > 0.0 {
            std::thread::sleep(Duration::from_secs_f64(self.delay));
        }
        if let Some(msg) = &self.fail {
            return Err(RuntimeError(msg.clone()).into());
        }
        Ok(self.text.clone())
    }
}

#[test]
fn test_frame_image_formats() {
    let jpeg = Frame::new(1, 4, 2, "jpeg", vec![0xff, 0xd8, 0xff, 0xd9]);
    assert_eq!(frame_image(&jpeg).unwrap(), (vec![0xff, 0xd8, 0xff, 0xd9], "image/jpeg"));
    let (png, mime) = frame_image(&frame(7)).unwrap();
    assert!(mime == "image/png" && png.starts_with(b"\x89PNG"));
}

#[test]
fn test_data_url() {
    assert_eq!(data_url(b"abc", "image/png"), "data:image/png;base64,YWJj");
}

#[test]
fn test_note_taker_appends_and_logs() {
    let tmp = tempfile::tempdir().unwrap();
    let client = FakeClient::ok("Two mugs by the window.");
    let taker = NoteTaker::new(client.clone(), tmp.path()).with_wall(Arc::new(|| dt(2026, 9, 5, 15, 30)));
    let (path, logs) = capture_logs(|| block_on(taker.take(&Note::new(frame(9), 20, 40))));
    taker.stop();
    let path = path.unwrap().unwrap();
    assert_eq!(path, tmp.path().join("2026-09-05.md"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "- 15:30 yaw=+20 pitch=40 — Two mugs by the window.\n");
    assert_eq!(*client.calls.lock().unwrap(), vec![(frame_image(&frame(9)).unwrap().0.len(), "image/png".to_string())]);
    assert!(logs.contains(&format!("explore: note -> {}", path.display())), "{logs}");
    assert_eq!(taker.taken(), 1);
}

#[test]
fn test_note_taker_failure_skips_and_logs_once_per_ten_minutes() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = Arc::new(StdMutex::new(0.0f64));
    let c = clock.clone();
    let taker = NoteTaker::new(FakeClient::failing("boom"), tmp.path()).with_clock(Arc::new(move || *c.lock().unwrap()));
    let (_, logs) = capture_logs(|| {
        assert_eq!(block_on(taker.take(&Note::new(frame(0), 0, 40))).unwrap(), None);
        *clock.lock().unwrap() = 100.0;
        assert_eq!(block_on(taker.take(&Note::new(frame(0), 0, 40))).unwrap(), None);
        *clock.lock().unwrap() = 700.0;
        assert_eq!(block_on(taker.take(&Note::new(frame(0), 0, 40))).unwrap(), None);
    });
    taker.stop();
    assert_eq!(taker.failed(), 3);
    assert_eq!(logs.matches("explore: note failed").count(), 2, "{logs}");
    assert!(logs.contains("RuntimeError: boom"));
    assert!(logs.contains("(further failures muted for 10 min)"));
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}

#[test]
fn test_note_taker_timeout() {
    let tmp = tempfile::tempdir().unwrap();
    let taker = NoteTaker::new(FakeClient::slow(0.2), tmp.path()).with_timeout(0.02);
    let (r, logs) = capture_logs(|| block_on(taker.take(&Note::new(frame(0), 0, 40))));
    taker.stop();
    assert_eq!(r.unwrap(), None);
    assert!(logs.contains("timed out"), "{logs}");
    assert_eq!(taker.failed(), 1);
}

#[test]
fn test_note_taker_stop_cancels_a_queued_call_and_restarts_lazily() {
    let tmp = tempfile::tempdir().unwrap();
    let client = FakeClient::slow(0.15);
    let taker = Arc::new(NoteTaker::new(client.clone(), tmp.path()).with_wall(Arc::new(|| dt(2026, 9, 5, 9, 0))));
    block_on(async {
        let a = taker.clone();
        let first = tokio::spawn(async move { a.take(&Note::new(frame(0), 0, 40)).await });
        tokio::time::sleep(Duration::from_millis(30)).await; // the first call is running
        let b = taker.clone();
        let second = tokio::spawn(async move { b.take(&Note::new(frame(0), 0, 40)).await });
        tokio::time::sleep(Duration::from_millis(30)).await; // the second is queued behind it
        taker.stop();
        // the running call finishes; the queued one is cancelled, never run
        assert!(first.await.unwrap().unwrap().is_some());
        let err = second.await.unwrap().unwrap_err();
        assert!(err.downcast_ref::<NoteCancelled>().is_some());
        assert_eq!(client.calls.lock().unwrap().len(), 1);
        // the next take starts a fresh worker
        assert!(taker.take(&Note::new(frame(0), 0, 40)).await.unwrap().is_some());
    });
    taker.stop();
    assert_eq!(taker.taken(), 2);
}

#[test]
fn test_note_taker_bad_frame_escapes_take() {
    let tmp = tempfile::tempdir().unwrap();
    let taker = NoteTaker::new(FakeClient::ok("x"), tmp.path());
    let bad = Frame::new(1, W, H, "gray", vec![0; 3]);
    let err = block_on(taker.take(&Note::new(bad, 0, 40))).unwrap_err();
    assert!(err.downcast_ref::<ValueError>().is_some());
    assert_eq!(taker.failed(), 0);
}

#[test]
fn test_make_note_client_without_key_is_none() {
    let (c, logs) = capture_logs(|| make_note_client(&cfg(), Some(&env(&[]))));
    assert!(c.is_none());
    assert!(logs.contains("OPENROUTER_API_KEY not set"), "{logs}");
}

#[test]
fn test_make_note_client_with_key_builds_inside_a_runtime() {
    // The daemon builds the client from async code; the blocking HTTP client must survive that.
    block_on(async {
        let c = make_note_client(&cfg(), Some(&env(&[("OPENROUTER_API_KEY", "sk-test")])));
        assert!(c.is_some());
        tokio::task::spawn_blocking(move || drop(c)).await.unwrap();
    });
}

// ---- the note client against a fake SDK (test_spend_wiring) -----------------

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
fn test_exploring_notes_are_metered() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = Arc::new(StdMutex::new(vec![]));
    let note = OpenAINoteClient::with_responses("gpt-5-mini", Box::new(FakeSdk { reply: reply("a  room\n"), calls: calls.clone() }))
        .with_ledger(Ledger::at(tmp.path()));
    assert_eq!(note.describe(b"x", "image/jpeg").unwrap(), "a room");
    let today = chrono::Local::now().date_naive().to_string();
    let rows = eve_spend::day_rows(&today, Some(tmp.path()));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["f"], json!("exploring"));
    assert_eq!(rows[0]["m"], json!("gpt-5-mini"));
    assert!((rows[0]["usd"].as_f64().unwrap() - 0.25).abs() < 1e-9, "{:?}", rows[0]);
    let body = &calls.lock().unwrap()[0];
    assert_eq!(
        *body,
        json!({
            "model": "gpt-5-mini",
            "input": [{
                "role": "user",
                "content": [
                    {"type": "input_text", "text": PROMPT},
                    {"type": "input_image", "image_url": "data:image/jpeg;base64,eA==", "detail": "low"},
                ],
            }],
            "max_output_tokens": 1024,
            "reasoning": {"effort": "low"},
        })
    );
}

#[test]
fn test_empty_answer_is_a_runtime_error() {
    let tmp = tempfile::tempdir().unwrap();
    let mut r = reply("  ");
    r["status"] = json!("incomplete");
    r["incomplete_details"] = json!({"reason": "max_output_tokens"});
    let note = OpenAINoteClient::with_responses("m", Box::new(FakeSdk { reply: r, calls: Arc::default() }))
        .with_ledger(Ledger::at(tmp.path()));
    let err = note.describe(b"x", "image/png").unwrap_err();
    assert_eq!(error_name(&err), "RuntimeError");
    assert_eq!(
        err.to_string(),
        "empty answer (status=incomplete, incomplete=IncompleteDetails(reason='max_output_tokens'))"
    );
    let note = OpenAINoteClient::with_responses("m", Box::new(FakeSdk { reply: json!({}), calls: Arc::default() }))
        .with_ledger(Ledger::at(tmp.path()));
    assert_eq!(note.describe(b"x", "image/png").unwrap_err().to_string(), "empty answer (status=None, incomplete=None)");
}

#[test]
fn test_the_note_request_goes_to_openrouter_with_the_prefixed_model() {
    use axum::routing::post;
    let seen: Arc<StdMutex<Vec<(String, Value)>>> = Arc::default();
    let tmp = tempfile::tempdir().unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let base = rt.block_on(async {
        let s = seen.clone();
        let app = axum::Router::new().route(
            "/responses",
            post(move |uri: axum::http::Uri, body: axum::Json<Value>| {
                let s = s.clone();
                async move {
                    s.lock().unwrap().push((uri.path().to_string(), body.0));
                    axum::Json(reply("A lamp."))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    });
    let opts = eve_openrouter::ClientOptions::default().base_url(base).max_retries(0);
    let client = OpenAINoteClient::with_options(DEFAULT_MODEL, Some("sk-test"), opts)
        .unwrap()
        .with_ledger(Ledger::at(tmp.path()));
    assert_eq!(client.describe(b"img", "image/png").unwrap(), "A lamp.");
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].0, "/responses");
    assert_eq!(seen[0].1["model"], json!(eve_openrouter::model(DEFAULT_MODEL)));
    assert_eq!(seen[0].1["reasoning"], json!({"effort": "low"}));
    let today = chrono::Local::now().date_naive().to_string();
    assert_eq!(eve_spend::day_rows(&today, Some(tmp.path()))[0]["m"], json!(DEFAULT_MODEL));
    drop(rt);
}

#[test]
fn test_notes_test_cli() {
    let tmp = tempfile::tempdir().unwrap();
    let img = tmp.path().join("shot.PNG");
    std::fs::write(&img, b"png-bytes").unwrap();
    let client = FakeClient::ok("A chair.");
    let c = client.clone();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_notes_test_with(
        img.to_str().unwrap(),
        Some(&cfg()),
        move |_| Some(c as Arc<dyn NoteClient>),
        &mut out,
        &mut err,
    );
    assert_eq!(code, 0);
    let text = String::from_utf8(out).unwrap();
    assert!(text.starts_with("fake (") && text.ends_with(" ms): A chair.\n"), "{text}");
    assert_eq!(*client.calls.lock().unwrap(), vec![(9, "image/png".to_string())]);

    let missing = tmp.path().join("absent.jpg");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(run_notes_test_with(missing.to_str().unwrap(), Some(&cfg()), |_| None, &mut out, &mut err), 2);
    assert_eq!(
        String::from_utf8(err).unwrap(),
        format!(
            "notes-test: cannot read {0}: [Errno 2] No such file or directory: '{0}'\n",
            missing.display()
        )
    );

    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(run_notes_test_with(img.to_str().unwrap(), Some(&cfg()), |_| None, &mut out, &mut err), 2);
    assert_eq!(
        String::from_utf8(err).unwrap(),
        "notes-test: no note client (OPENROUTER_API_KEY missing or openai not installed)\n"
    );

    let (mut out, mut err) = (Vec::new(), Vec::new());
    let failing = FakeClient::failing("boom");
    assert_eq!(
        run_notes_test_with(img.to_str().unwrap(), Some(&cfg()), move |_| Some(failing as Arc<dyn NoteClient>), &mut out, &mut err),
        1
    );
    assert_eq!(String::from_utf8(err).unwrap(), "notes-test: RuntimeError: boom\n");
}

// ---- surprise: what this waypoint usually looks like -------------------------

/// A frame of a plausible room: a bright half, a dark half, a bright band
/// across the middle, plus a couple of luma units of sensor wobble that move
/// with `seed`. `level` shifts the whole frame (the lights changing);
/// `block` paints (x0, y0, x1, y1, value) over it (something moved).
fn room(seed: i64, level: i64, block: Option<(i64, i64, i64, i64, u8)>) -> Frame {
    let mut data = vec![0u8; (W * H) as usize];
    for y in 0..H {
        for x in 0..W {
            let mut base = if x < W / 2 { 70 } else { 150 };
            if (50..70).contains(&y) {
                base = 200;
            }
            data[(y * W + x) as usize] = (base + level + (x + y + seed * 3).rem_euclid(3)).clamp(0, 255) as u8;
        }
    }
    if let Some((x0, y0, x1, y1, value)) = block {
        for y in y0..y1 {
            for x in x0..x1 {
                data[(y * W + x) as usize] = value;
            }
        }
    }
    Frame::new(seed, W, H, "gray", data)
}

/// Kept for the callers that only need "a frame that differs a bit".
fn noise(level: i64, seed: i64) -> Frame {
    room(seed, level - 100, None)
}

fn thumb(f: &Frame) -> Option<Vec<u8>> {
    frame_thumb(f)
}

#[test]
fn test_surprise_is_none_until_the_bank_has_five_frames() {
    let mut d = ChangeDetector::default();
    for i in 0..5 {
        assert_eq!(d.surprise(0, thumb(&noise(100, i)).as_deref()), None);
    }
    assert_eq!(d.banked(0), 5);
    assert!(d.surprise(0, thumb(&noise(100, 6)).as_deref()).is_some());
}

#[test]
fn test_surprise_stays_low_for_the_usual_view_and_jumps_for_a_change() {
    let mut d = ChangeDetector::default();
    for i in 0..12 {
        d.surprise(0, thumb(&noise(100, i)).as_deref());
    }
    let usual = d.surprise(0, thumb(&room(99, 0, None)).as_deref()).unwrap();
    // Something the size of a mug appears low on the left, where the wall has
    // never been anything but flat.
    let odd = d.surprise(0, thumb(&room(100, 0, Some((10, 80, 50, 115, 240)))).as_deref()).unwrap();
    assert!(usual < 1.5 && 1.5 < odd, "{usual} {odd}");
}

/// The noisy-TV case: a region that changes wildly every look raises its
/// own spread, so it cannot keep buying attention.
#[test]
fn test_a_flickering_cell_stops_counting_as_surprise() {
    fn flicker(seed: i64) -> Frame {
        // A screen in the corner: black one look, white the next.
        room(seed, 0, Some((10, 80, 50, 115, if seed % 2 != 0 { 0 } else { 255 })))
    }
    let mut d = ChangeDetector::default();
    for i in 0..20 {
        d.surprise(0, thumb(&flicker(i)).as_deref());
    }
    let flickering = d.surprise(0, thumb(&flicker(21)).as_deref()).unwrap();
    // The same-sized change where the wall has always been still scores far higher.
    let still = d.surprise(0, thumb(&room(22, 0, Some((100, 80, 140, 115, 240)))).as_deref()).unwrap();
    assert!(flickering < 1.5 && 1.5 < still, "{flickering} {still}");
}

#[test]
fn test_surprise_ignores_the_lights_going_down() {
    let mut d = ChangeDetector::default();
    for i in 0..12 {
        d.surprise(0, thumb(&room(i, 0, None)).as_deref());
    }
    let dimmer = room(99, -45, None); // same room, much darker
    assert!(d.surprise(0, thumb(&dimmer).as_deref()).unwrap() < 1.5);
}

#[test]
fn test_banks_are_per_waypoint() {
    let mut d = ChangeDetector::default();
    for i in 0..12 {
        d.surprise(0, thumb(&room(i, 0, None)).as_deref());
    }
    assert!(d.banked(0) == 12 && d.banked(1) == 0);
    assert_eq!(d.surprise(1, thumb(&room(3, 0, None)).as_deref()), None);
}

#[test]
fn test_bank_keeps_only_the_newest_bank_size() {
    let mut d = ChangeDetector::default();
    for i in 0..(BANK_SIZE as i64 + 7) {
        d.surprise(0, thumb(&room(i, 0, None)).as_deref());
    }
    assert_eq!(d.banked(0), BANK_SIZE);
    assert_eq!(d.surprise(0, None), None);
    assert_eq!(d.banked(0), BANK_SIZE);
}

#[test]
fn test_normalise_centres_on_mid_grey() {
    assert!(normalise(&[10u8; 16]).iter().all(|&v| v == 128));
    assert!(normalise(&[200u8; 16]).iter().all(|&v| v == 128));
    // int() truncates toward zero before the clamp
    assert_eq!(normalise(&[0, 255]), vec![0, 255]);
    assert_eq!(normalise(&[0, 1]), vec![127, 128]);
}

#[test]
fn test_every_considered_frame_is_banked_even_when_no_note_is_spent() {
    let mut e = explorer();
    e.bucket = TokenBucket::new(0.0, 0.0); // no budget: nothing is ever spent
    tick(&mut e, 0.0);
    for i in 0..3 {
        tick_frame(&mut e, 2.0 + i as f64 * 6.0, &noise(100, i));
        tick(&mut e, 6.0 + i as f64 * 6.0);
    }
    assert_eq!(e.notes, 0);
    assert!((0..WAYPOINTS.len()).map(|w| e.detector.banked(w)).sum::<usize>() >= 3);
}

// ---- the pan plan against the neck it has to run on -----------------------------------------

/// The BSP configures the yaw servo with angleLimit +-128 degrees, and the
/// firmware clamps to +-120 (body.cpp YAW_MAX). Every waypoint and everything
/// build_look_cmd can emit has to fit inside that, or the head is asking for a
/// place it cannot go.
#[test]
fn test_the_pan_plan_stays_inside_the_servos_own_limit() {
    assert_eq!(YAW_RANGE, (-120, 120));
    for &(yaw, pitch) in WAYPOINTS.iter() {
        assert!(YAW_RANGE.0 <= yaw && yaw <= YAW_RANGE.1, "{yaw} {pitch}");
        assert!((5..=85).contains(&pitch), "{yaw} {pitch}");
    }
    for yaw in [-1000.0, -121.0, 0.0, 121.0, 1000.0] {
        let y = build_look_cmd(yaw, 40.0, LOOK_HOLD_MS)["yaw"].as_i64().unwrap();
        assert!(YAW_RANGE.0 <= y && y <= YAW_RANGE.1);
    }
}

/// Both extremes are visited at both heights, and no two consecutive
/// waypoints are far apart — the head never swings the full span inside one
/// hold, which it could not finish in six seconds.
#[test]
fn test_the_pan_sweeps_the_whole_range_in_small_steps() {
    let yaws: Vec<i64> = WAYPOINTS.iter().map(|w| w.0).collect();
    assert!(*yaws.iter().min().unwrap() == YAW_RANGE.0 && *yaws.iter().max().unwrap() == YAW_RANGE.1);
    for pitch in PAN_PITCHES {
        let row: Vec<i64> = WAYPOINTS.iter().filter(|w| w.1 == pitch).map(|w| w.0).collect();
        assert!(*row.iter().min().unwrap() == YAW_RANGE.0 && *row.iter().max().unwrap() == YAW_RANGE.1);
        assert_eq!(row.len(), PAN_YAWS.len());
    }
    // Inside a cycle every move is small; the one long swing back to the first
    // waypoint happens after the rest, with minutes to make it.
    let steps: Vec<i64> = WAYPOINTS.windows(2).map(|w| (w[1].0 - w[0].0).abs()).collect();
    assert!(*steps.iter().max().unwrap() <= 30);
}

#[test]
fn test_a_cycle_is_still_a_sane_length() {
    let secs = WAYPOINTS.len() as f64 * LOOK_INTERVAL_SECS;
    assert!((60.0..=240.0).contains(&secs));
}

/// PITCH_LEVEL in the firmware is 45 and its own comment calls that a
/// guess — it depends on how the head was zeroed. So the plan must not assume
/// it: one row has to sit below any plausible level and one above.
#[test]
fn test_the_rows_straddle_wherever_level_turns_out_to_be() {
    assert!(PAN_PITCHES.len() >= 3);
    assert!(*PAN_PITCHES.iter().min().unwrap() <= 25, "no row looks down at the desk");
    assert!(*PAN_PITCHES.iter().max().unwrap() >= 65, "no row looks up at the room");
    assert!(*PAN_PITCHES.iter().min().unwrap() >= 5 && *PAN_PITCHES.iter().max().unwrap() <= 85);
    for guess_at_level in [35, 45, 55] {
        assert!(PAN_PITCHES.iter().any(|&p| p < guess_at_level), "{guess_at_level}");
        assert!(PAN_PITCHES.iter().any(|&p| p > guess_at_level), "{guess_at_level}");
    }
}

#[test]
fn test_waypoints_are_the_serpentine() {
    assert_eq!(WAYPOINTS.len(), 36);
    assert_eq!(WAYPOINTS[0], (-120, 5));
    assert_eq!(WAYPOINTS[8], (120, 5));
    assert_eq!(WAYPOINTS[9], (120, 25));
    assert_eq!(WAYPOINTS[35], (-120, 65));
}
