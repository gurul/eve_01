//! test_phone_location.py's tests of phone_location itself: parsing the app's body, keeping only the newest fix
//! (atomic, 0600), the tool's answer (fresh, stale, none), the switch and when the tool is offered.
//!
//! Coordinates here are fictional: 0.0/0.0 and a city center (the repository is public).

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, TimeZone, Utc};
use eve_places::phone_location as pl;
use eve_places::Env;
use serde_json::{json, Map, Value};

fn now_fixed() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 12, 18, 0, 0).unwrap()
}

fn center() -> Map<String, Value> {
    json!({"lat": 37.7749, "lon": -122.4194, "accuracy_m": 12.5, "at": "2026-10-12T17:55:00Z",
           "place": "San Francisco, CA", "speed_mps": 1.2})
    .as_object()
    .unwrap()
    .clone()
}

fn body(over: Value) -> Value {
    let mut out = center();
    for (k, v) in over.as_object().unwrap() {
        out.insert(k.clone(), v.clone());
    }
    Value::Object(out)
}

fn fix(minutes_ago: f64) -> pl::Fix {
    pl::Fix::new(0.0, 0.0, 10.0, Utc::now() - Duration::milliseconds((minutes_ago * 60_000.0) as i64), Some("Null Island"))
}

/// The test's environment: the fix and the places history in its own folder (the conftest's autouse fixture).
fn env(dir: &Path) -> Env {
    Env::from([
        (pl::FILE_ENV.to_string(), dir.join("phone").join("phone-location.json").display().to_string()),
        ("EVE_PLACES_FILE".to_string(), dir.join("phone").join("places.db").display().to_string()),
        ("EVE_TIMEZONE".to_string(), "UTC".to_string()),
    ])
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

// ---- the body ----------------------------------------------------------------------------------------------------

#[test]
fn test_a_good_body_is_a_fix() {
    let now = Some(now_fixed());
    let f = pl::parse(&body(json!({})), now).unwrap();
    assert_eq!((f.lat, f.lon, f.accuracy_m, f.place.as_deref(), f.speed_mps), (37.7749, -122.4194, 12.5, Some("San Francisco, CA"), Some(1.2)));
    assert_eq!(f.at, Utc.with_ymd_and_hms(2026, 10, 12, 17, 55, 0).unwrap());
    // an offset zone is the same instant; ints are numbers; null place and speed; keys beyond the six are ignored
    let g = pl::parse(
        &body(json!({"at": "2026-10-12T10:55:00-07:00", "lat": 0, "lon": 0, "place": null, "speed_mps": null, "extra": 1})),
        now,
    )
    .unwrap();
    assert!(g.at == f.at && (g.lat, g.lon, g.place.clone(), g.speed_mps) == (0.0, 0.0, None, None));
    // iOS's "invalid speed" (-1) is no speed, not garbage
    assert_eq!(pl::parse(&body(json!({"speed_mps": -1})), now).unwrap().speed_mps, None);
    // the place is one clean line
    assert_eq!(
        pl::parse(&body(json!({"place": "  Market St\n\tSan Francisco  "})), now).unwrap().place.as_deref(),
        Some("Market St San Francisco")
    );
    assert_eq!(pl::parse(&body(json!({"place": "   "})), now).unwrap().place, None);
    // str.strip() takes the separators \x1c-\x1f as whitespace
    assert_eq!(pl::parse(&body(json!({"at": "\u{1f}2026-10-12T17:55:00Z"})), now).unwrap().at, f.at);
    // the edges are in range
    assert_eq!(pl::parse(&body(json!({"lat": -90, "lon": 180, "accuracy_m": 0})), now).unwrap().lat, -90.0);
}

#[test]
fn test_garbage_is_refused_with_a_reason() {
    let long = "x".repeat(pl::MAX_PLACE_CHARS + 1);
    let cases = vec![
        Value::Null,
        json!([]),
        json!("x"),
        json!({}),
        body(json!({"lat": null})),
        body(json!({"lat": "37.7"})),
        body(json!({"lat": true})),
        body(json!({"lat": 90.01})),
        body(json!({"lat": -91})),
        body(json!({"lon": 180.5})),
        body(json!({"lon": null})),
        body(json!({"lat": 1e300})),
        body(json!({"accuracy_m": -1e300})),
        body(json!({"accuracy_m": -1})),
        body(json!({"accuracy_m": pl::MAX_ACCURACY_M + 1.0})),
        body(json!({"accuracy_m": null})),
        body(json!({"accuracy_m": "5"})),
        body(json!({"speed_mps": pl::MAX_SPEED_MPS + 1.0})),
        body(json!({"speed_mps": "fast"})),
        body(json!({"speed_mps": false})),
        body(json!({"at": null})),
        body(json!({"at": 1760290500})),
        body(json!({"at": "yesterday"})),
        body(json!({"at": "2026-10-12T17:55:00"})),  // no zone
        body(json!({"at": "2026-10-12T18:30:00Z"})), // the future
        body(json!({"place": 7})),
        body(json!({"place": ["a"]})),
        body(json!({"place": long})),
    ];
    for bad in cases {
        let e = pl::parse(&bad, Some(now_fixed())).expect_err(&bad.to_string());
        assert!(!e.0.is_empty() && !e.0.contains("37.77"), "{bad}"); // the reason never repeats a value
    }
    // the reasons, word for word
    let reason = |b: Value| pl::parse(&b, Some(now_fixed())).unwrap_err().0;
    assert_eq!(reason(body(json!({"lat": 90.01}))), "lat must be from -90 to 90");
    assert_eq!(reason(body(json!({"accuracy_m": -1}))), "accuracy_m must be from 0 to 100000");
    assert_eq!(reason(body(json!({"lat": true}))), "lat must be a number");
    assert_eq!(reason(json!([])), "the body must be a JSON object");
    assert_eq!(reason(body(json!({"at": "yesterday"}))), "at must be an ISO 8601 time with a zone");
    assert_eq!(reason(body(json!({"at": "2026-10-12T18:30:00Z"}))), "at is in the future");
    assert_eq!(reason(body(json!({"place": 7}))), "place must be a string or null");
    assert_eq!(reason(body(json!({"place": "x".repeat(121)}))), "place is at most 120 characters");
}

#[test]
fn test_a_little_clock_skew_is_allowed() {
    assert!(pl::parse(&body(json!({"at": "2026-10-12T18:04:00Z"})), Some(now_fixed())).unwrap().at > now_fixed());
}

// ---- keeping it --------------------------------------------------------------------------------------------------

#[test]
fn test_only_the_newest_fix_is_kept_atomically_and_owner_only() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("state").join("phone-location.json");
    let first = fix(10.0);
    assert!(pl::store(&first, Some(&path)).unwrap());
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().unwrap()), 0o700); // a folder it made is owner-only too
    assert_eq!(pl::load(Some(&path)), Some(first.clone()));
    // an older fix, or the same one again (a retry), is ignored and nothing is written
    let before = std::fs::read(&path).unwrap();
    assert!(!pl::store(&fix(20.0), Some(&path)).unwrap() && !pl::store(&first, Some(&path)).unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    // a newer one replaces it: one fix in the file, no history, no temp file left
    let mut newer = fix(1.0);
    newer.lat = 1.5;
    newer.place = None;
    assert!(pl::store(&newer, Some(&path)).unwrap() && pl::load(Some(&path)) == Some(newer.clone()));
    let data: Map<String, Value> = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let keys: Vec<&str> = data.keys().map(String::as_str).collect();
    assert_eq!(keys, ["lat", "lon", "accuracy_m", "at", "place", "speed_mps"]);
    assert_eq!(data["lat"], json!(1.5));
    let names: Vec<String> =
        std::fs::read_dir(path.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    assert_eq!(names, ["phone-location.json"]);
    assert_eq!(mode(&path), 0o600);
}

#[test]
fn test_the_file_is_what_python_json_dump_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("phone-location.json");
    let at = Utc.with_ymd_and_hms(2026, 10, 12, 17, 55, 0).unwrap();
    let mut f = pl::Fix::new(37.7749, -122.4194, 12.5, at, Some("Zürich"));
    f.speed_mps = Some(1.0);
    pl::store(&f, Some(&path)).unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        r#"{"lat": 37.7749, "lon": -122.4194, "accuracy_m": 12.5, "at": "2026-10-12T17:55:00+00:00", "place": "Z\u00fcrich", "speed_mps": 1.0}"#
    );
    // and a file the Python daemon wrote reads back
    std::fs::write(&path, r#"{"lat": 1.0, "lon": 2.0, "accuracy_m": 5.0, "at": "2026-10-12T17:55:00.250000+00:00", "place": null, "speed_mps": null}"#).unwrap();
    let back = pl::load(Some(&path)).unwrap();
    assert_eq!((back.lat, back.at.timestamp_subsec_micros()), (1.0, 250_000));
}

#[test]
fn test_a_broken_file_is_no_fix_and_is_replaced() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("phone-location.json");
    std::fs::write(&path, "{not json").unwrap();
    assert_eq!(pl::load(Some(&path)), None);
    assert!(pl::store(&fix(1.0), Some(&path)).unwrap() && pl::load(Some(&path)).is_some());
    std::fs::write(&path, json!({"lat": 500}).to_string()).unwrap();
    assert_eq!(pl::load(Some(&path)), None);
}

#[test]
fn test_the_file_is_where_the_environment_says() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.json");
    let e = Env::from([(pl::FILE_ENV.to_string(), a.display().to_string())]);
    assert_eq!(pl::state_path(Some(&e)), a);
    if std::env::var_os(pl::FILE_ENV).is_none() {
        let home = PathBuf::from(std::env::var("HOME").unwrap());
        assert_eq!(pl::state_path(Some(&Env::new())), home.join(".config/eve-bridge/phone-location.json"));
    }
    assert!(pl::DEFAULT_FILE.starts_with("~/.config/eve-bridge/"));
}

// ---- the tool ----------------------------------------------------------------------------------------------------

#[test]
fn test_the_tool_is_strict_with_no_arguments_and_says_when_to_use_it() {
    // places' three ride with it (places_recall, place_name, thing_place: tests/places.rs)
    assert_eq!(pl::TOOL_NAMES[0], "phone_location");
    assert_eq!(&pl::TOOL_NAMES[1..], &pl::PLACES_TOOL_NAMES);
    assert_eq!(*pl::TOOLS, vec![pl::TOOL.clone()]);
    assert_eq!(pl::TOOL["strict"], json!(true));
    assert_eq!(pl::TOOL["parameters"], json!({"type": "object", "additionalProperties": false, "required": [], "properties": {}}));
    let text = pl::TOOL["description"].as_str().unwrap();
    for word in ["where they are", "near", "how long", "weather", "stale", "age_min", "accuracy"] {
        assert!(text.contains(word), "{word}");
    }
}

#[tokio::test]
async fn test_the_answer_fresh_stale_and_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let path = pl::state_path(Some(&e));
    let no_args = Map::new();
    assert_eq!(pl::handle("phone_location", &no_args, Some(&e)).await, json!({"error": pl::NO_FIX}).as_object().unwrap().clone());
    let taken = pl::Fix::new(37.774912345, -122.419412345, 12.4, Utc::now() - Duration::minutes(5), Some("Null Island")).at;
    let f = pl::Fix::new(37.774912345, -122.419412345, 12.4, taken, Some("Null Island"));
    pl::store(&f, Some(&path)).unwrap();
    assert_eq!(
        Value::Object(pl::reading(Some(&e), Some(taken + Duration::minutes(5) + Duration::seconds(30)))),
        json!({"place": "Null Island", "lat": 37.77491, "lon": -122.41941, "accuracy_m": 12, "age_min": 5, "stale": false})
    );
    let out = pl::handle("phone_location", &no_args, Some(&e)).await;
    let keys: std::collections::BTreeSet<&str> = out.keys().map(String::as_str).collect();
    assert_eq!(keys, ["accuracy_m", "age_min", "lat", "lon", "place", "stale"].into_iter().collect());
    assert_eq!(out["stale"], json!(false));
    pl::store(&fix(61.0), Some(&path)).unwrap(); // older than the one kept: ignored
    assert_eq!(pl::handle("phone_location", &no_args, Some(&e)).await["stale"], json!(false));
    std::fs::remove_file(&path).unwrap();
    pl::store(&fix(61.0), Some(&path)).unwrap();
    let old = pl::handle("phone_location", &no_args, Some(&e)).await;
    assert!(old["stale"] == json!(true) && old["age_min"].as_i64().unwrap() >= 60);
    // the boundary: exactly an hour is not stale, past it is
    let at = pl::Fix::new(0.0, 0.0, 5.0, Utc::now(), None).at;
    std::fs::remove_file(&path).unwrap();
    pl::store(&pl::Fix::new(0.0, 0.0, 5.0, at - Duration::minutes(60), None), Some(&path)).unwrap();
    assert_eq!(pl::reading(Some(&e), Some(at))["stale"], json!(false));
    assert_eq!(pl::reading(Some(&e), Some(at + Duration::seconds(1)))["stale"], json!(true));
}

#[tokio::test]
async fn test_the_switch_turns_off_the_tool_and_its_answer() {
    let tmp = tempfile::tempdir().unwrap();
    let mut e = env(tmp.path());
    pl::store(&fix(1.0), Some(&pl::state_path(Some(&e)))).unwrap();
    assert!(pl::enabled(Some(&e)) && pl::offered(Some(&e)) && pl::tools(Some(&e))[..1] == [pl::TOOL.clone()]);
    for off in ["0", "false", "no", "off", " OFF "] {
        e.insert(pl::SWITCH.into(), off.into());
        assert!(!pl::enabled(Some(&e)) && !pl::offered(Some(&e)) && pl::tools(Some(&e)).is_empty());
        assert_eq!(pl::fresh(Some(&e)), None);
        let out = pl::handle("phone_location", &Map::new(), Some(&e)).await;
        assert_eq!(Value::Object(out.clone()), json!({"ok": false, "reason": pl::OFF_REASON}));
        assert!(!out.contains_key("lat"));
    }
    e.insert(pl::SWITCH.into(), "1".into());
    assert!(pl::enabled(Some(&e)) && pl::offered(Some(&e)));
}

#[test]
fn test_the_tool_is_offered_only_once_the_phone_shared_a_fix() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    assert!(!pl::offered(Some(&e)) && pl::tools(Some(&e)).is_empty()); // never shared: every prompt as before
    pl::store(&fix(500.0), Some(&pl::state_path(Some(&e)))).unwrap(); // any age: the tool says it is stale
    assert!(pl::offered(Some(&e)) && pl::tools(Some(&e))[..1] == [pl::TOOL.clone()]);
    assert_eq!(pl::fresh(Some(&e)), None); // but it is not "here" for quick answers
}

#[test]
fn test_the_log_words_carry_age_and_accuracy_only() {
    let mut f = fix(2.0);
    f.lat = 37.7749;
    f.lon = -122.4194;
    f.accuracy_m = 8.4;
    f.place = Some("San Francisco, CA".into());
    let words = pl::describe(&f, None);
    let re = regex::Regex::new(r"^age 1[12]\d s, accuracy 8 m$").unwrap();
    assert!(re.is_match(&words), "{words}");
}

#[tokio::test]
async fn test_another_tool_name_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let out = pl::handle("camera_status", &Map::new(), Some(&e)).await;
    assert_eq!(Value::Object(out), json!({"ok": false, "reason": "camera_status is not the phone's location tool"}));
}
