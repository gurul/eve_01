//! Port of `bridge/tests/test_motion.py`: the host half of the board's motion
//! vocabulary.
//!
//! The board is the authority on what is safe, so these tests are about the
//! wire shape and about the host telling the truth — not about enforcing
//! limits. The limits are tested where they live, in
//! firmware/claude_pet_stackchan/host/motion_test.cpp, against the same header
//! the board runs.

use super::*;
use serde_json::json;

// What the firmware will accept (firmware/claude_pet_stackchan/src/motion.h:54,60);
// the board re-checks all of it.
const PERIOD_MIN_MS: i64 = 650;
const PERIOD_MAX_MS: i64 = 8000;
const CYCLES_MAX: i64 = 12;

fn osc(preset: &str) -> Command {
    osc_command(Some(preset), 1.0, &OscParams::default()).unwrap()
}

fn osc_speed(preset: &str, speed: f64) -> Command {
    osc_command(Some(preset), speed, &OscParams::default()).unwrap()
}

fn keys(preset: &str) -> Command {
    keys_command(Some(preset), None).unwrap()
}

fn int(cmd: &Command, key: &str) -> i64 {
    cmd[key].as_i64().unwrap_or_else(|| panic!("{key} missing from {cmd:?}"))
}

fn key_rows(cmd: &Command) -> Vec<Vec<i64>> {
    cmd["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_array().unwrap().iter().map(|n| n.as_i64().unwrap()).collect())
        .collect()
}

#[test]
fn test_every_preset_makes_one_wire_line_of_the_right_shape() {
    let mut names: Vec<&str> = osc_names();
    names.extend(keys_names());
    names.sort_unstable();
    names.dedup();
    for name in names {
        let cmd = if osc_preset(name).is_some() { osc(name) } else { keys(name) };
        assert_eq!(cmd["cmd"], "move");
        let kind = cmd["kind"].as_str().unwrap();
        assert!(kind == "osc" || kind == "keys");
        if kind == "keys" {
            let rows = key_rows(&cmd);
            assert!((1..=MAX_KEYS).contains(&rows.len()));
            assert!(rows.iter().all(|k| k.len() == 4));
        } else {
            assert!(int(&cmd, "period_ms") >= PERIOD_MIN_MS);
            assert!(int(&cmd, "yaw_amp") != 0 || int(&cmd, "pitch_amp") != 0, "{name}");
        }
    }
}

/// That phase is what makes it an ellipse rather than a diagonal line — the
/// difference between "up and down side to side" and one wobble.
#[test]
fn test_the_dance_is_two_axes_a_quarter_cycle_apart() {
    let cmd = osc("dance");
    assert!(int(&cmd, "yaw_amp") > 0 && int(&cmd, "pitch_amp") > 0);
    assert_eq!(int(&cmd, "phase_deg"), 90);
}

/// Amplitude yields on the board, the beat is kept. A request to go faster
/// must therefore only shorten the period here.
#[test]
fn test_faster_means_a_quicker_beat_not_a_refusal() {
    let base = osc("dance");
    let fast = osc_speed("dance", 1.6);
    let slow = osc_speed("dance", 0.6);
    assert!(int(&fast, "period_ms") < int(&base, "period_ms"));
    assert!(int(&base, "period_ms") < int(&slow, "period_ms"));
    assert_eq!(
        int(&fast, "yaw_amp"),
        int(&base, "yaw_amp"),
        "the host does not pre-shrink; the board does"
    );
}

#[test]
fn test_speed_is_bounded_so_a_model_cannot_ask_for_a_blur() {
    for wild in [0.01, -5.0, 99.0] {
        let cmd = osc_speed("dance", wild);
        let p = int(&cmd, "period_ms");
        assert!((PERIOD_MIN_MS / 4..=PERIOD_MAX_MS * 4).contains(&p), "{wild}: {p}");
    }
}

/// A rhythm should grow out of the pose it is in, not snap somewhere first.
#[test]
fn test_no_centre_means_centre_on_wherever_the_head_is() {
    assert!(!osc("dance").contains_key("center_yaw"));
    assert!(!osc("dance").contains_key("center_pitch"));
    let params = OscParams { center_yaw: Some(-30), center_pitch: Some(40), ..Default::default() };
    let aimed = osc_command(Some("dance"), 1.0, &params).unwrap();
    assert!(int(&aimed, "center_yaw") == -30 && int(&aimed, "center_pitch") == 40);
}

/// dance once asked for 28 degrees and the board reduced it to 25, so the host
/// predicted a swing it never delivered. Checked against the shipped header by
/// firmware/.../host/motion_test.cpp and tools' admit_check.
#[test]
fn test_the_host_table_asks_for_what_the_board_actually_runs() {
    assert_eq!(osc_field("dance", "yaw_amp"), Some(25));
}

#[test]
fn test_overrides_beat_the_preset_and_a_bad_name_is_refused() {
    let params = OscParams { pitch_amp: Some(6), cycles: Some(2), ..Default::default() };
    let cmd = osc_command(Some("nod"), 1.0, &params).unwrap();
    assert!(int(&cmd, "pitch_amp") == 6 && int(&cmd, "cycles") == 2);
    let err = osc_command(Some("moonwalk"), 1.0, &OscParams::default()).unwrap_err();
    assert_eq!(err.python_type(), "ValueError");
    let err = keys_command(Some("moonwalk"), None).unwrap_err();
    assert_eq!(err.python_type(), "ValueError");
}

#[test]
fn test_a_gesture_keeps_an_axis_alone_with_the_sentinel() {
    let cmd = keys("doubletake");
    assert!(key_rows(&cmd).iter().any(|k| k[2] == KEEP), "a doubletake is yaw only");
    assert_eq!(keys_preset("perk").unwrap()[0][1], KEEP, "perk is pitch only");
}

#[test]
fn test_raw_keys_are_accepted_capped_and_defaulted() {
    let cmd = keys_command(None, Some(&[json!([0, 127, 35]), json!([900, 127, 45, 400])])).unwrap();
    let rows = key_rows(&cmd);
    assert_eq!(rows[0], vec![0, 127, 35, 500], "a missing speed gets a default");
    assert_eq!(rows[1], vec![900, 127, 45, 400]);
    let long: Vec<Vec<i64>> = (0..20).map(|i| vec![i * 100, 0, 45, 400]).collect();
    let long = keys_command_rows(&long).unwrap();
    assert_eq!(key_rows(&long).len(), MAX_KEYS);
    let err = keys_command(None, Some(&[json!([0, 1])])).unwrap_err();
    assert_eq!(err.python_type(), "ValueError");
    let err = keys_command(None, Some(&[])).unwrap_err();
    assert_eq!(err.python_type(), "ValueError");
}

#[test]
fn test_stop_is_one_short_line() {
    assert_eq!(Value::Object(stop_command()), json!({"cmd": "move", "kind": "stop"}));
}

#[test]
fn test_predict_reports_the_speed_the_board_budgets_against() {
    let p = predict(&osc("dance")).unwrap();
    assert_eq!(p["kind"], "osc");
    assert_eq!(
        p["ms"].as_i64().unwrap(),
        osc_field("dance", "cycles").unwrap() * osc_field("dance", "period_ms").unwrap()
    );
    // 2*pi*f*A, the same form as the firmware's own velocity budget
    let peak = p["peak_yaw_dps"].as_f64().unwrap();
    assert!(150.0 < peak && peak < 240.0, "{p:?}");
    let k = predict(&keys("shrug")).unwrap();
    assert!(k["kind"] == "keys" && k["keys"] == 3 && k["ms"].as_i64().unwrap() > 700);
}

#[test]
fn test_describe_says_something_a_person_can_read() {
    let text = describe(&osc("dance")).unwrap();
    assert!(text.contains("yaw") && text.contains('°') && text.contains("ms"));
    assert!(describe(&keys("shrug")).unwrap().contains("keys"));
}

/// Not a safety check — the board does that. This catches a table edit that
/// would be silently reduced, which is how a preset stops matching its name.
#[test]
// 3.1416 is the Python test's own literal, kept so the bound is the same one.
#[allow(clippy::approx_constant)]
fn test_every_preset_stays_inside_the_periods_and_cycles_the_board_accepts() {
    for (name, _) in OSC {
        let period = osc_field(name, "period_ms").unwrap();
        assert!((PERIOD_MIN_MS..=PERIOD_MAX_MS).contains(&period), "{name}");
        if let Some(pp) = osc_field(name, "pitch_period_ms").filter(|v| *v != 0) {
            assert!((PERIOD_MIN_MS..=PERIOD_MAX_MS).contains(&pp), "{name}");
        }
        let cycles = osc_field(name, "cycles").unwrap_or(4);
        assert!((1..=CYCLES_MAX).contains(&cycles), "{name}");
        let yaw = osc_field(name, "yaw_amp").unwrap() as f64;
        assert!(yaw * 2.0 * 3.1416 * (1000.0 / period as f64) < 240.0, "{name}");
    }
}

// ---------------------------------------------------------------------------
// Byte parity with the Python module. Each expected value below was produced
// by running bridge/src/eve_bridge/motion.py and json.dumps on its output.

fn line(cmd: &Command) -> String {
    serde_json::to_string(cmd).unwrap()
}

#[test]
fn test_wire_lines_match_the_python_bytes() {
    assert_eq!(
        line(&osc("dance")),
        r#"{"cmd":"move","kind":"osc","yaw_amp":25,"pitch_amp":12,"period_ms":900,"phase_deg":90,"cycles":8,"dwell_pct":8,"jitter_pct":10}"#
    );
    // 900 / 1.6 = 562.5, and Python's round() goes to the even neighbour.
    assert_eq!(int(&osc_speed("dance", 1.6), "period_ms"), 562);
    assert_eq!(
        line(&osc_speed("fig8", 0.6)),
        r#"{"cmd":"move","kind":"osc","yaw_amp":25,"pitch_amp":10,"period_ms":2667,"pitch_period_ms":1333,"cycles":4,"dwell_pct":8,"jitter_pct":8}"#
    );
    let params = OscParams { center_yaw: Some(-30), center_pitch: Some(40), ..Default::default() };
    assert_eq!(
        line(&osc_command(Some("dance"), 1.0, &params).unwrap()),
        r#"{"cmd":"move","kind":"osc","yaw_amp":25,"pitch_amp":12,"period_ms":900,"phase_deg":90,"cycles":8,"dwell_pct":8,"jitter_pct":10,"center_yaw":-30,"center_pitch":40}"#
    );
    assert_eq!(
        line(&keys("shrug")),
        r#"{"cmd":"move","kind":"keys","keys":[[0,14,35,800],[300,14,35,200],[700,0,45,250]]}"#
    );
    assert_eq!(line(&stop_command()), r#"{"cmd":"move","kind":"stop"}"#);
    // No preset and no overrides: just the two fixed fields.
    assert_eq!(line(&osc_command(None, 1.0, &OscParams::default()).unwrap()), r#"{"cmd":"move","kind":"osc"}"#);
    // An empty name is no preset, not a bad one; 3 / 2 = 1.5 rounds to 2.
    let p = OscParams { period_ms: Some(3), ..Default::default() };
    assert_eq!(line(&osc_command(Some(""), 2.0, &p).unwrap()), r#"{"cmd":"move","kind":"osc","period_ms":2}"#);
}

#[test]
fn test_predict_and_describe_match_the_python_text() {
    let dance = osc("dance");
    assert_eq!(
        line(&predict(&dance).unwrap()),
        r#"{"kind":"osc","ms":7200,"yaw_amp":25,"pitch_amp":12,"period_ms":900,"peak_yaw_dps":174.5,"peak_pitch_dps":83.8}"#
    );
    assert_eq!(
        describe(&dance).unwrap(),
        "yaw 25° pitch 12° every 900 ms for 7200 ms (peak 174.5/83.8 °/s asked)"
    );
    assert_eq!(
        describe(&osc_speed("dance", 1.6)).unwrap(),
        "yaw 25° pitch 12° every 562 ms for 4496 ms (peak 279.5/134.2 °/s asked)"
    );
    let nod = osc_command(Some("nod"), 1.0, &OscParams { pitch_amp: Some(6), cycles: Some(2), ..Default::default() })
        .unwrap();
    assert_eq!(
        line(&predict(&nod).unwrap()),
        r#"{"kind":"osc","ms":1400,"yaw_amp":0,"pitch_amp":6,"period_ms":700,"peak_yaw_dps":0.0,"peak_pitch_dps":53.9}"#
    );
    assert_eq!(describe(&nod).unwrap(), "yaw 0° pitch 6° every 700 ms for 1400 ms (peak 0.0/53.9 °/s asked)");
    assert_eq!(line(&predict(&keys("shrug")).unwrap()), r#"{"kind":"keys","keys":3,"ms":1300}"#);
    assert_eq!(describe(&keys("shrug")).unwrap(), "3 keys over 1300 ms");
    // The defaults predict() assumes for a bare rhythm.
    let bare = osc_command(None, 1.0, &OscParams::default()).unwrap();
    assert_eq!(describe(&bare).unwrap(), "yaw 0° pitch 0° every 900 ms for 3600 ms (peak 0.0/0.0 °/s asked)");
    // A zero period means no frequency rather than a division by zero.
    let zero = osc_command(None, 1.0, &OscParams { period_ms: Some(0), yaw_amp: Some(10), ..Default::default() })
        .unwrap();
    let p = predict(&zero).unwrap();
    assert_eq!(p["peak_yaw_dps"].as_f64(), Some(0.0));
    assert_eq!(p["ms"], 0);
}

#[test]
fn test_error_messages_match_the_python_text() {
    let err = osc_command(Some("moonwalk"), 1.0, &OscParams::default()).unwrap_err();
    assert_eq!(err.to_string(), "'moonwalk' is not a rhythm; try bounce, dance, fig8, nod, shake, sway, wiggle");
    let err = keys_command(Some("moonwalk"), None).unwrap_err();
    assert_eq!(
        err.to_string(),
        "'moonwalk' is not a gesture; try doubletake, droop, home, lean_peek, perk, shrug, tilt"
    );
    let err = keys_command(None, None).unwrap_err();
    assert_eq!(
        err.to_string(),
        "None is not a gesture; try doubletake, droop, home, lean_peek, perk, shrug, tilt"
    );
    let err = keys_command(None, Some(&[json!([0, 1])])).unwrap_err();
    assert_eq!(err.to_string(), "a key is [at_ms, yaw, pitch] with an optional speed");
    assert_eq!(keys_command(None, Some(&[])).unwrap_err().to_string(), "no keys");
    let err = osc_command(Some("dance"), f64::NAN, &OscParams::default()).unwrap_err();
    assert_eq!((err.python_type(), err.to_string().as_str()), ("ValueError", "cannot convert float NaN to integer"));
    // A NaN speed with nothing to scale is harmless, as in Python.
    assert!(osc_command(None, f64::NAN, &OscParams::default()).is_ok());
    assert_eq!(py_repr_str("it's"), "\"it's\"");
}

#[test]
fn test_raw_keys_after_the_cap_are_never_read() {
    let mut raw: Vec<Value> = (0..MAX_KEYS).map(|i| json!([i * 10, 0, 45])).collect();
    raw.push(json!([1]));
    raw.push(json!("not a key"));
    let cmd = keys_command(None, Some(&raw)).unwrap();
    assert_eq!(key_rows(&cmd).len(), MAX_KEYS);
    // Raw keys win over a preset name, as in Python.
    let cmd = keys_command(Some("shrug"), Some(&[json!([5, 1, 2, 3])])).unwrap();
    assert_eq!(key_rows(&cmd), vec![vec![5, 1, 2, 3]]);
}

#[test]
fn test_request_numbers_are_read_the_way_python_int_reads_them() {
    let req = json!({
        "yaw_amp": "12", "pitch_amp": 7.9, "cycles": true, "period_ms": null,
        "center_yaw": " -3 ", "phase_deg": "1_0",
    });
    let params = OscParams::from_request(req.as_object().unwrap()).unwrap();
    assert_eq!(params.yaw_amp, Some(12));
    assert_eq!(params.pitch_amp, Some(7));
    assert_eq!(params.cycles, Some(1));
    assert_eq!(params.period_ms, None);
    assert_eq!(params.center_yaw, Some(-3));
    assert_eq!(params.phase_deg, Some(10));
    let bad = json!({"cycles": "2.5"});
    let err = OscParams::from_request(bad.as_object().unwrap()).unwrap_err();
    assert_eq!((err.python_type(), err.to_string()), ("ValueError", "invalid literal for int() with base 10: '2.5'".into()));
    let bad = json!({"cycles": [1]});
    let err = OscParams::from_request(bad.as_object().unwrap()).unwrap_err();
    assert_eq!(err.python_type(), "TypeError");
    let cmd = keys_command(None, Some(&[json!(["100", 1.5, false])])).unwrap();
    assert_eq!(key_rows(&cmd), vec![vec![100, 1, 0, 500]]);
}

#[test]
fn test_python_float_text() {
    assert_eq!(py_float_repr(0.0), "0.0");
    assert_eq!(py_float_repr(174.5), "174.5");
    assert_eq!(py_float_repr(-2.0), "-2.0");
    assert_eq!(py_float_repr(1e16), "1e+16");
    assert_eq!(py_float_repr(1.5e-5), "1.5e-05");
    assert_eq!(py_round1(0.25), 0.2);
    assert_eq!(py_round1(0.05), 0.1);
    assert_eq!(py_round1(1.45), 1.4);
}
