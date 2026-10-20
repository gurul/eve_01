//! test_system_context.py, plus the system-context half of test_places.py's where-the-owner-is test.

use std::path::Path;

use chrono::{Duration, Offset, TimeZone, Utc};
use chrono_tz::America::Los_Angeles as PT;
use eve_places::phone_location;
use eve_places::places::{self, Places, Visit};
use eve_places::{py, Env};
use eve_system_context::context;
use serde_json::json;

/// The places and fix files in a temp folder, so a test never reads this Mac's real history.
fn env(tmp: &Path, pairs: &[(&str, &str)]) -> Env {
    let mut e = Env::from([
        ("EVE_PLACES_FILE".to_string(), tmp.join("places.db").display().to_string()),
        ("EVE_PHONE_LOCATION_FILE".to_string(), tmp.join("phone.json").display().to_string()),
    ]);
    for (k, v) in pairs {
        e.insert((*k).to_string(), (*v).to_string());
    }
    e
}

#[test]
fn test_clock_uses_configured_timezone_and_daylight_saving() {
    for (month, offset, hour) in [(1, "-08:00", "04"), (9, "-07:00", "05")] {
        let tmp = tempfile::tempdir().unwrap();
        let e = env(
            tmp.path(),
            &[("EVE_LOCATION", "Example Apartments, Seattle"), ("EVE_TIMEZONE", "America/Los_Angeles")],
        );
        let result = context(Some(&e), Some(Utc.with_ymd_and_hms(2026, month, 22, 12, 0, 0).unwrap()));
        assert!(result.contains(&format!("T{hour}:00:00{offset}")), "{result}");
        assert!(result.contains("Timezone: America/Los_Angeles"));
        assert!(result.contains("Owner-configured location: Example Apartments, Seattle"));
        assert!(result.contains("not live GPS") && result.contains("snapshot, not a ticking clock"));
    }
}

#[test]
fn test_missing_location_and_invalid_timezone_use_local_clock() {
    let tmp = tempfile::tempdir().unwrap();
    let result = context(Some(&env(tmp.path(), &[("EVE_TIMEZONE", "Missing/Zone")])), None);
    assert!(!result.contains("Owner-configured location"));
    assert!(!result.contains("Missing/Zone"));
    assert!(result.contains("Local clock when this context was generated:"));
}

#[test]
fn test_the_whole_block_is_python_s_text() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path(), &[("EVE_LOCATION", "  Example\n Apartments,\tSeattle "), ("EVE_TIMEZONE", " America/Los_Angeles ")]);
    let at = Utc.with_ymd_and_hms(2026, 9, 22, 12, 0, 0).unwrap() + Duration::microseconds(654_321);
    // the string system_context.context() returns for these inputs
    let expected = "\n\nSystem context:\n\
        Local clock when this context was generated: 2026-09-22T05:00:00-07:00.\n\
        Timezone: America/Los_Angeles.\n\
        Owner-configured location: Example Apartments, Seattle.\n\
        Use this location for local questions unless the owner names another place; it is a configured default, \
        not live GPS.\n\
        The timestamp is a snapshot, not a ticking clock. For the current time later in a conversation, use a fresh \
        time lookup. Do not treat an old snapshot as the current time.";
    assert_eq!(context(Some(&e), Some(at)), expected);
}

#[test]
fn test_without_a_zone_the_clock_is_this_mac_s() {
    let tmp = tempfile::tempdir().unwrap();
    let at = Utc.with_ymd_and_hms(2026, 1, 22, 12, 0, 0).unwrap();
    let result = context(Some(&env(tmp.path(), &[])), Some(at));
    let local = at.with_timezone(&chrono::Local);
    let off = local.offset().fix().local_minus_utc();
    let sign = if off < 0 { '-' } else { '+' };
    let stamp = format!("{}{sign}{:02}:{:02}", local.format("%Y-%m-%dT%H:%M:%S"), off.abs() / 3600, off.abs() % 3600 / 60);
    assert!(result.contains(&format!("generated: {stamp}.")), "{result}");
    let zone_line = result.lines().find(|l| l.starts_with("Timezone: ")).unwrap();
    assert!(zone_line.len() > "Timezone: .".len(), "{zone_line}");
}

#[test]
fn test_a_zone_in_the_wrong_case_loads_and_keeps_its_spelling() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path(), &[("EVE_TIMEZONE", "america/los_angeles")]);
    let result = context(Some(&e), Some(Utc.with_ymd_and_hms(2026, 1, 22, 12, 0, 0).unwrap()));
    assert!(result.contains("2026-01-22T04:00:00-08:00"));
    assert!(result.contains("Timezone: america/los_angeles."));
}

#[test]
fn test_an_old_offset_keeps_its_seconds_and_the_location_is_capped() {
    let tmp = tempfile::tempdir().unwrap();
    let long = "x".repeat(400);
    let e = env(tmp.path(), &[("EVE_TIMEZONE", "Asia/Kolkata"), ("EVE_LOCATION", &long)]);
    let result = context(Some(&e), Some(Utc.with_ymd_and_hms(1850, 1, 1, 0, 0, 0).unwrap()));
    // datetime(1850, 1, 1, tzinfo=timezone.utc).astimezone(ZoneInfo("Asia/Kolkata")).isoformat(timespec="seconds")
    assert!(result.contains("1850-01-01T05:53:28+05:53:28"), "{result}");
    assert!(result.contains(&format!("Owner-configured location: {}.\n", "x".repeat(300))));
}

/// test_places.py: test_where_the_owner_is_now_is_said_by_name_in_the_system_context (the system_context lines).
#[test]
fn test_where_the_owner_is_now_is_said_by_name_in_the_system_context() {
    let tmp = tempfile::tempdir().unwrap();
    let mut e = env(tmp.path(), &[("EVE_TIMEZONE", "America/Los_Angeles")]);
    let st = Places::new(None, Some(e.clone()), None);
    assert!(!context(Some(&e), None).contains("Where the owner is now"));
    let arrived = Utc::now() - Duration::minutes(40);
    let v = Visit {
        lat: 41.8700,
        lon: -87.6400,
        accuracy_m: 20.0,
        arrived: Some(arrived),
        departed: None,
        place: None,
        poi: Some("Equinox".into()),
        category: Some("fitnessCenter".into()),
    };
    st.record_visit(&v).unwrap();
    let fix = phone_location::parse(
        &json!({"lat": 41.8700, "lon": -87.6400, "accuracy_m": 10.0, "at": py::isoformat_utc(&Utc::now()),
                "place": "a street", "speed_mps": null}),
        None,
    )
    .unwrap();
    assert!(phone_location::store(&fix, Some(Path::new(&e["EVE_PHONE_LOCATION_FILE"]))).unwrap());
    let since = py::trunc_micros(arrived).with_timezone(&PT).format("%H:%M").to_string();
    let line = format!("Where the owner is now, from their phone: Equinox (fitness center), since {since}.");
    assert_eq!(places::here_line(Some(&e)), line);
    let result = context(Some(&e), Some(PT.with_ymd_and_hms(2026, 10, 22, 12, 0, 0).unwrap().with_timezone(&Utc)));
    assert!(result.contains(&line));
    // the line sits after the clock and before the snapshot warning, as Python appends it
    let tail = result.split(&line).nth(1).unwrap();
    assert!(tail.starts_with("\nThe timestamp is a snapshot"));
    e.insert("EVE_PLACES".into(), "0".into());
    assert!(!context(Some(&e), None).contains(&line));
}
