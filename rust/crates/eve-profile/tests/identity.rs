//! profile with no profile: full eve, the owner's, is untouched (port of test_profile_identity.py).
//!
//! With EVE_PROFILE unset (or "full") `apply` returns [] and leaves every variable as it was, and `install_firewall`
//! installs nothing: checked in a fresh process that then runs a fake git, which prints its marker. The same process
//! under EVE_PROFILE=lite does not get to run it (the positive control: the check can see a firewall when there is
//! one).

mod common;

use std::collections::HashMap;
use std::process::Command;

use common::{is_child, print_result, run_child, CANARY};
use eve_profile as profile;
use indexmap::IndexMap;
use serde_json::json;

/// An owner-shaped environment: every door on, keys and ids set (placeholders), the switches lite would force.
fn owner_env() -> IndexMap<String, String> {
    [
        ("HOME", "/Users/someone"), ("PATH", "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"),
        ("OPENROUTER_API_KEY", CANARY), ("OPENAI_API_KEY", CANARY), ("COMPOSIO_API_KEY", CANARY),
        ("TINYFISH_API_KEY", CANARY),
        ("EVE_TELEGRAM", "1"), ("EVE_TELEGRAM_TOKEN", CANARY), ("EVE_TELEGRAM_OWNER", "4242"), ("EVE_MINIAPP", "1"),
        ("EVE_COMPOSIO", "1"), ("EVE_APPS", "1"), ("EVE_MEMORY", "1"), ("EVE_SHARE", "1"), ("EVE_LISTEN_KEY", "option"),
        ("EVE_BRIDGE_SOCK", "/tmp/eve-bridge.sock"), ("SPOTIFY_CLIENT_ID", CANARY), ("AGENTCARD_ACCESS_TOKEN", CANARY),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

fn profile_env(value: Option<&str>) -> HashMap<String, String> {
    value.map(|v| HashMap::from([("EVE_PROFILE".to_string(), v.to_string())])).unwrap_or_default()
}

#[test]
fn test_unset_or_full_is_full() {
    for value in [None, Some(""), Some("full"), Some("FULL"), Some(" full\n")] {
        let env = profile_env(value);
        assert_eq!(profile::active(Some(&env)), Ok(profile::FULL), "{value:?}");
        assert_eq!(profile::is_lite(Some(&env)), Ok(false), "{value:?}");
    }
}

#[test]
fn test_lite_is_lite() {
    for value in ["lite", "Lite", " lite\n"] {
        let env = profile_env(Some(value));
        assert_eq!(profile::active(Some(&env)), Ok(profile::LITE), "{value:?}");
        assert_eq!(profile::is_lite(Some(&env)), Ok(true), "{value:?}");
    }
}

#[test]
fn test_any_other_value_is_an_error() {
    for value in ["lit", "1", "true", "on", "eve lite", "lite,full"] {
        assert!(profile::active(Some(&profile_env(Some(value)))).is_err(), "{value:?}");
        let mut env = owner_env();
        env.insert("EVE_PROFILE".to_string(), value.to_string());
        let before = env.clone();
        assert!(profile::apply(&mut env).is_err(), "{value:?}");
        assert_eq!(env, before); // nothing changed before the refusal
    }
    let err = profile::active(Some(&profile_env(Some("lit")))).unwrap_err();
    assert_eq!(err.to_string(), "EVE_PROFILE must be 'lite' or 'full', or unset; not 'lit'");
}

#[test]
fn test_the_error_does_not_repeat_a_long_value() {
    let value = format!("{}{CANARY}", "x".repeat(30));
    let err = profile::active(Some(&profile_env(Some(&value)))).unwrap_err();
    assert!(!err.to_string().contains(CANARY));
    assert!(err.to_string().ends_with(&format!("not '{}'", "x".repeat(24))));
}

#[test]
fn test_the_process_environment_is_read_when_none_is_given() {
    for (value, lite) in [(None, false), (Some("lite"), true)] {
        let env: Vec<(&str, String)> = value.map(|v| vec![("EVE_PROFILE", v.to_string())]).unwrap_or_default();
        let ran = run_child("child_reads_the_process_environment", &env, None);
        assert_eq!(ran.result["is_lite"], json!(lite));
        assert_eq!(ran.result["active"], json!(if lite { "lite" } else { "full" }));
        assert_eq!(ran.result["mapping_given"], json!(false)); // a mapping passed in is the one read
    }
}

#[test]
fn child_reads_the_process_environment() {
    if !is_child("child_reads_the_process_environment") {
        return;
    }
    print_result(&json!({
        "is_lite": profile::is_lite(None).unwrap(),
        "active": profile::active(None).unwrap(),
        "mapping_given": profile::is_lite(Some(&HashMap::new())).unwrap(),
    }));
}

#[test]
fn test_apply_with_no_profile_is_the_identity() {
    for value in [None, Some("full")] {
        let mut env = owner_env();
        if let Some(v) = value {
            env.insert("EVE_PROFILE".to_string(), v.to_string());
        }
        let before = env.clone();
        assert_eq!(profile::apply(&mut env).unwrap(), Vec::<String>::new());
        assert_eq!(env, before);
        assert!(env.keys().eq(before.keys())); // the same names, values and order
    }
}

#[test]
fn test_apply_with_no_profile_leaves_this_process_alone() {
    let env = vec![("PATH", "/usr/bin:/bin".to_string()), ("OPENAI_API_KEY", CANARY.to_string())];
    let ran = run_child("child_applies_with_no_profile", &env, None);
    assert_eq!(ran.result, json!({"changed": [], "same": true}));
}

#[test]
fn child_applies_with_no_profile() {
    if !is_child("child_applies_with_no_profile") {
        return;
    }
    let before: Vec<(String, String)> = std::env::vars().collect();
    let changed = profile::apply_process_env().unwrap();
    let after: Vec<(String, String)> = std::env::vars().collect();
    print_result(&json!({"changed": changed, "same": before == after}));
}

#[test]
fn test_no_profile_no_firewall_and_lite_has_one() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let fake = bin.join("git");
    common::write_program(&fake, "#!/bin/sh\necho RAN-FAKE-GIT\n");
    let base = vec![
        ("PATH", format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display())),
        ("HOME", tmp.path().join("home").display().to_string()),
        ("OPENAI_API_KEY", CANARY.to_string()),
    ];
    let run = |extra: Option<&str>| {
        let mut env = base.clone();
        if let Some(v) = extra {
            env.push(("EVE_PROFILE", v.to_string()));
        }
        run_child("child_applies_installs_and_runs_git", &env, None).result
    };
    for extra in [None, Some("full")] {
        assert_eq!(run(extra), json!({"changed": [], "git": "RAN-FAKE-GIT", "counts": {}}));
    }
    let lite = run(Some("lite"));
    assert!(lite["git"].as_str().unwrap().starts_with("refused"), "{lite}");
    assert_eq!(lite["counts"], json!({"exec:git": 1}));
    let changed: Vec<&str> = lite["changed"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert!(changed.contains(&"OPENAI_API_KEY") && changed.contains(&"EVE_TELEGRAM"));
}

#[test]
fn child_applies_installs_and_runs_git() {
    if !is_child("child_applies_installs_and_runs_git") {
        return;
    }
    let changed = profile::apply_process_env().unwrap();
    profile::install_firewall("enforce").unwrap();
    let out = match profile::output(&mut Command::new("git")) {
        Ok(done) => String::from_utf8_lossy(&done.stdout).trim().to_string(),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => format!("refused: {e}"),
        Err(e) => format!("error: {e}"),
    };
    print_result(&json!({"changed": changed, "git": out, "counts": profile::firewall_counts()}));
}

#[test]
fn test_install_firewall_in_full_eve_says_so_and_installs_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let env = vec![("PATH", "/usr/bin:/bin".to_string()), ("HOME", tmp.path().display().to_string())];
    let ran = run_child("child_installs_in_full_eve", &env, None);
    assert_eq!(ran.log.matches("firewall: not installed").count(), 2, "{}", ran.log);
    assert_eq!(ran.result["counts"], json!({}));
}

#[test]
fn child_installs_in_full_eve() {
    if !is_child("child_installs_in_full_eve") {
        return;
    }
    common::log_to_stderr();
    profile::install_firewall("audit").unwrap();
    profile::install_firewall("enforce").unwrap();
    print_result(&json!({"counts": profile::firewall_counts()}));
}

#[test]
fn test_a_bad_mode_is_refused_before_anything() {
    assert!(matches!(profile::install_firewall("off"), Err(profile::FirewallError::Mode(_))));
    assert!(profile::firewall_counts().is_empty());
}
