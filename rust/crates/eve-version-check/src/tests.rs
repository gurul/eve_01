//! Port of `bridge/tests/test_version_check.py`, plus parity checks against
//! outputs recorded from the Python module.

use super::*;
use std::cell::Cell;

fn info(current: &str, latest: Option<&str>, fetched_at: f64) -> UpdateInfo {
    UpdateInfo { current: current.into(), latest: latest.map(Into::into), fetched_at }
}

fn network_was_hit() -> Option<String> {
    panic!("network was hit")
}

// ---- _is_newer ----

#[test]
fn test_is_newer() {
    let cases = [
        ("0.1.1", "0.1.0", true),
        ("0.2.0", "0.1.99", true),
        ("1.0.0", "0.99.99", true),
        ("0.1.0", "0.1.0", false),
        ("0.1.0", "0.1.1", false),
        ("v0.1.1", "v0.1.0", true), // tolerates 'v' prefix on either side
        ("0.1.1", "v0.1.0", true),
        ("v0.1.0", "0.1.1", false),
        ("0.2.0", "0.2.0-rc1", true),     // bare release > pre-release
        ("0.2.0-rc2", "0.2.0-rc1", true), // rc2 > rc1 lexically
        ("", "0.1.0", false),             // empty inputs are safe
        ("0.1.0", "", false),
    ];
    for (candidate, baseline, expected) in cases {
        assert_eq!(is_newer(candidate, baseline), expected, "{candidate} vs {baseline}");
    }
}

/// Recorded from Python's `_is_newer`, including the cases where its tuple
/// comparison raises `TypeError` (mixed int/str, or different lengths) and
/// so returns False.
#[test]
fn is_newer_matches_python_edge_cases() {
    let cases = [
        ("0.1.1", "0.1", false),
        ("0.1", "0.1.1", false),
        ("0.2", "0.1.9", true),
        ("1.a", "1.b", false),
        ("1.a", "1.0", false),
        ("1.10.0", "1.9.0", true),
        ("010", "9", true),
        ("v1.0", "vv1.0", false),
        (" v1.0.1 ", "1.0.0", true),
        ("1.0.0-beta", "1.0.0-alpha", true),
        ("1.0.0-rc1-x", "1.0.0-rc1", true),
        ("1..0", "1..1", false),
        ("2", "10", false),
        ("99999999999999999999999.1", "99999999999999999999998.9", true),
        ("1.0.0+b", "1.0.0", false),
        ("x", "y", false),
        ("1.0-", "1.0", false),
        ("1.0", "1.0-", false),
    ];
    for (candidate, baseline, expected) in cases {
        assert_eq!(is_newer(candidate, baseline), expected, "{candidate:?} vs {baseline:?}");
    }
}

#[test]
fn parts_builds_python_tuple() {
    assert_eq!(
        parts(" v0.2.0-rc1 "),
        vec![
            Part::Int("0".into()),
            Part::Int("2".into()),
            Part::Int("0".into()),
            Part::Pre(false, "rc1".into())
        ]
    );
    assert_eq!(parts("1.x"), vec![Part::Int("1".into()), Part::Str("x".into()), Part::Pre(true, String::new())]);
    assert_eq!(parts(""), vec![Part::Str(String::new()), Part::Pre(true, String::new())]);
    assert_eq!(parts("007"), vec![Part::Int("7".into()), Part::Pre(true, String::new())]);
}

// ---- UpdateInfo / has_update / is_fresh ----

#[test]
fn test_update_info_has_update_with_no_latest() {
    assert!(!info("0.1.0", None, now()).has_update());
}

#[test]
fn test_update_info_has_update_with_newer_latest() {
    assert!(info("0.1.0", Some("0.1.1"), now()).has_update());
}

#[test]
fn update_info_empty_latest_has_no_update() {
    assert!(!info("0.1.0", Some(""), now()).has_update());
}

#[test]
fn test_update_info_is_fresh_within_ttl() {
    assert!(info("0.1.0", Some("0.1.0"), now() - 100.0).is_fresh());
}

#[test]
fn test_update_info_is_stale_past_ttl() {
    assert!(!info("0.1.0", Some("0.1.0"), now() - CACHE_TTL_SECS - 100.0).is_fresh());
}

// ---- cache path ----

#[test]
fn cache_path_per_platform() {
    let home = Path::new("/Users/me");
    assert_eq!(
        cache_path_for("darwin", home, Some("/x")),
        PathBuf::from("/Users/me/Library/Caches/eve-bridge/update_check.json")
    );
    assert_eq!(
        cache_path_for("win32", home, None),
        PathBuf::from("/Users/me/AppData/Local/eve-bridge/update_check.json")
    );
    assert_eq!(cache_path_for("linux", home, Some("/xdg")), PathBuf::from("/xdg/eve-bridge/update_check.json"));
    assert_eq!(
        cache_path_for("linux", home, Some("")),
        PathBuf::from("/Users/me/.cache/eve-bridge/update_check.json")
    );
    assert_eq!(cache_path_for("linux", home, None), PathBuf::from("/Users/me/.cache/eve-bridge/update_check.json"));
    assert!(cache_path().ends_with("eve-bridge/update_check.json"));
}

// ---- cache load/save ----

#[test]
fn test_cache_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let cache_file = dir.path().join("update_check.json");
    let saved = info("0.1.0", Some("0.1.2"), 1700000000.0);
    save_cache_at(&saved, &cache_file);
    assert_eq!(load_cache_at(&cache_file), Some(saved));
}

#[test]
fn test_cache_missing_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(load_cache_at(&dir.path().join("nope.json")), None);
}

#[test]
fn test_cache_corrupt_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    let cache_file = dir.path().join("update_check.json");
    fs::write(&cache_file, "not json").unwrap();
    assert_eq!(load_cache_at(&cache_file), None);
}

#[test]
fn cache_file_bytes_match_python_json_dump() {
    // Recorded from Python: json.dump({...}, f) with the default settings.
    assert_eq!(
        cache_json(&info("0.1.2", Some("v0.2.0"), 1760890000.123456)),
        r#"{"current": "0.1.2", "latest": "v0.2.0", "fetched_at": 1760890000.123456}"#
    );
    assert_eq!(
        cache_json(&info("é", Some("😀"), 1e16)),
        "{\"current\": \"\\u00e9\", \"latest\": \"\\ud83d\\ude00\", \"fetched_at\": 1e+16}"
    );
    assert_eq!(
        cache_json(&info("0.1.2", None, 1700000000.0)),
        r#"{"current": "0.1.2", "latest": null, "fetched_at": 1700000000.0}"#
    );
}

#[test]
fn save_cache_creates_parent_dirs_and_writes_python_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let cache_file = dir.path().join("a").join("eve-bridge").join("update_check.json");
    save_cache_at(&info("0.1.2", Some("v0.2.0"), 1760890000.123456), &cache_file);
    assert_eq!(
        fs::read_to_string(&cache_file).unwrap(),
        r#"{"current": "0.1.2", "latest": "v0.2.0", "fetched_at": 1760890000.123456}"#
    );
}

#[test]
fn save_cache_failure_is_swallowed() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("file");
    fs::write(&blocker, "x").unwrap();
    // The parent is a regular file, so mkdir fails; Python logs and returns.
    save_cache_at(&info("0.1.2", Some("v1"), 1.0), &blocker.join("c.json"));
    assert_eq!(fs::read_to_string(&blocker).unwrap(), "x");
}

#[test]
fn load_cache_reads_python_values() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("c.json");
    // str(current) and float(fetched_at), as Python coerces them.
    fs::write(&f, r#"{"current": 12, "latest": null, "fetched_at": "1700000000"}"#).unwrap();
    assert_eq!(load_cache_at(&f), Some(info("12", None, 1700000000.0)));
    fs::write(&f, r#"{"current": null, "fetched_at": true}"#).unwrap();
    assert_eq!(load_cache_at(&f), Some(info("None", None, 1.0)));
}

#[test]
fn load_cache_rejects_unusable_shapes() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("c.json");
    for body in [
        r#"["current", "fetched_at"]"#,
        r#""text""#,
        r#"{"latest": "v1", "fetched_at": 1.0}"#,
        r#"{"current": "0.1.2", "latest": "v1"}"#,
        r#"{"current": "0.1.2", "fetched_at": null}"#,
        r#"{"current": "0.1.2", "fetched_at": "soon"}"#,
        r#"{"current": "0.1.2", "fetched_at": [1]}"#,
    ] {
        fs::write(&f, body).unwrap();
        assert_eq!(load_cache_at(&f), None, "{body}");
    }
    // A directory where the file should be: open() raises OSError.
    assert_eq!(load_cache_at(dir.path()), None);
}

// ---- fetch ----

#[test]
fn tag_from_body_reads_tag_name() {
    assert_eq!(tag_from_body(br#"{"tag_name": "v0.2.0", "name": "x"}"#), Some("v0.2.0".into()));
    assert_eq!(tag_from_body(br#"{"tag_name": 5}"#), None);
    assert_eq!(tag_from_body(br#"{"message": "Not Found"}"#), None);
    assert_eq!(tag_from_body(b"<html>"), None);
    assert_eq!(tag_from_body(b"[]"), None);
}

#[test]
fn user_agent_names_the_package_version() {
    assert_eq!(USER_AGENT.as_str(), format!("eve-bridge/{}", eve_meta::VERSION));
    assert_eq!(GITHUB_API, "https://api.github.com/repos/SnowWarri0r/eve-bridge/releases/latest");
    assert_eq!(HTTP_TIMEOUT_SECS, 3.0);
    assert_eq!(CACHE_TTL_SECS, 24.0 * 3600.0);
}

#[test]
#[ignore = "live: calls api.github.com"]
fn fetch_latest_live() {
    let tag = fetch_latest();
    assert!(tag.is_none_or(|t| !t.is_empty()));
}

// ---- check() ----

#[test]
fn test_check_disabled_by_env() {
    let dir = tempfile::tempdir().unwrap();
    // Should NOT hit the network even if forced.
    let got = check_with(true, true, eve_meta::VERSION, &dir.path().join("c.json"), network_was_hit);
    assert_eq!(got.latest, None);
    assert!(!got.has_update());
    assert!(!dir.path().join("c.json").exists());
}

#[test]
fn test_check_uses_fresh_cache() {
    let dir = tempfile::tempdir().unwrap();
    let cache_file = dir.path().join("c.json");
    // Seed a fresh cache that matches current version.
    save_cache_at(&info(eve_meta::VERSION, Some("0.99.0"), now()), &cache_file);
    let got = check_with(false, false, eve_meta::VERSION, &cache_file, network_was_hit);
    assert_eq!(got.latest.as_deref(), Some("0.99.0"));
}

#[test]
fn test_check_force_ignores_cache() {
    let dir = tempfile::tempdir().unwrap();
    let cache_file = dir.path().join("c.json");
    save_cache_at(&info(eve_meta::VERSION, Some("0.99.0"), now()), &cache_file);
    let got = check_with(true, false, eve_meta::VERSION, &cache_file, || Some("1.2.3".into()));
    assert_eq!(got.latest.as_deref(), Some("1.2.3"));
    assert_eq!(load_cache_at(&cache_file).unwrap().latest.as_deref(), Some("1.2.3"));
}

#[test]
fn test_check_network_failure_returns_no_latest() {
    let dir = tempfile::tempdir().unwrap();
    let cache_file = dir.path().join("c.json");
    let got = check_with(true, false, eve_meta::VERSION, &cache_file, || None);
    assert_eq!(got.latest, None);
    assert!(!got.has_update());
    // A failed fetch is not cached.
    assert!(!cache_file.exists());
}

#[test]
fn check_network_failure_keeps_old_cache() {
    let dir = tempfile::tempdir().unwrap();
    let cache_file = dir.path().join("c.json");
    let old = info(eve_meta::VERSION, Some("0.0.1"), 1700000000.0);
    save_cache_at(&old, &cache_file);
    let got = check_with(true, false, eve_meta::VERSION, &cache_file, || None);
    assert_eq!(got.latest, None);
    assert_eq!(load_cache_at(&cache_file), Some(old));
}

#[test]
fn test_check_stale_cache_triggers_refetch() {
    let dir = tempfile::tempdir().unwrap();
    let cache_file = dir.path().join("c.json");
    save_cache_at(&info(eve_meta::VERSION, Some("0.0.1"), now() - CACHE_TTL_SECS - 100.0), &cache_file);
    let got = check_with(false, false, eve_meta::VERSION, &cache_file, || Some("0.5.0".into()));
    assert_eq!(got.latest.as_deref(), Some("0.5.0"));
}

/// If user upgraded since cache was written, the cache is invalid for them.
#[test]
fn test_check_cached_version_mismatch_triggers_refetch() {
    let dir = tempfile::tempdir().unwrap();
    let cache_file = dir.path().join("c.json");
    save_cache_at(&info("0.0.1", Some("0.0.2"), now()), &cache_file);
    let fetched = Cell::new(false);
    let got = check_with(false, false, eve_meta::VERSION, &cache_file, || {
        fetched.set(true);
        Some("2.0.0".into())
    });
    assert!(fetched.get());
    assert_eq!(got.latest.as_deref(), Some("2.0.0"));
    assert_eq!(got.current, eve_meta::VERSION);
}

#[test]
fn check_reports_installed_version() {
    let dir = tempfile::tempdir().unwrap();
    let got = check_with(true, false, "0.1.2", &dir.path().join("c.json"), || Some("v0.1.3".into()));
    assert_eq!(got.current, "0.1.2");
    assert!(got.has_update());
}

// ---- render_cli ----

#[test]
fn test_render_cli_no_update() {
    let out = render_cli(&info("0.1.0", Some("0.1.0"), now()));
    assert!(out.contains("Installed:"));
    assert!(out.contains("0.1.0"));
    assert!(out.to_lowercase().contains("latest"));
    assert!(!out.contains("Update available"));
    assert_eq!(out, "Installed:   0.1.0\nLatest:      0.1.0\nYou're on the latest release.");
}

#[test]
fn test_render_cli_has_update() {
    let out = render_cli(&info("0.1.0", Some("0.2.0"), now()));
    assert!(out.contains("Update available: 0.1.0 → 0.2.0"));
    assert!(out.contains("git pull"));
    assert_eq!(
        out,
        "Installed:   0.1.0\nLatest:      0.2.0\n\nUpdate available: 0.1.0 → 0.2.0\n\
         Pull with:        git pull && pip install -e .\n\
         Then restart:     eve-bridge install --service  (or kickstart the daemon)"
    );
}

#[test]
fn test_render_cli_no_latest() {
    let out = render_cli(&info("0.1.0", None, now()));
    assert!(out.to_lowercase().contains("could not reach github"));
    assert_eq!(out, "Installed:   0.1.0\nLatest:      (could not reach github.com — try again later)");
}

// ---- test_update.py's helper ----

#[test]
fn fake_info_shape_from_test_update() {
    // test_update.py builds UpdateInfo(current, latest, fetched_at) directly.
    let i = info("0.1.0", None, 0.0);
    assert!(!i.is_fresh());
    assert!(!i.has_update());
    assert!(info("0.1.0", Some("0.1.0"), 0.0).latest.is_some());
}

#[tokio::test]
#[ignore = "live: calls api.github.com and writes the real cache file"]
async fn check_async_runs_off_the_runtime() {
    // Must not panic with reqwest's "blocking inside async" guard, whatever
    // the network says; current is always reported.
    let got = check_async(false).await;
    assert_eq!(got.current, eve_meta::VERSION);
}
