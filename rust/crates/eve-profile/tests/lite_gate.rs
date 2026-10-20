//! profile::apply under EVE_PROFILE=lite: secrets are removed but the OpenRouter key, the defaults fill only what is
//! unset, the log line carries names only, and the tool filter keeps exactly lite's tools (port of
//! test_profile_lite_gate.py). The checks through each forced switch's own module reader live with those modules'
//! crates; the app switches, which this crate's upstream (eve-app-registry) reads, are checked here.

mod common;

use std::collections::HashMap;

use common::{capture_log, CANARY};
use eve_profile as profile;
use indexmap::IndexMap;
use serde_json::{json, Value};

fn map(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

const KEY: (&str, &str) = ("OPENROUTER_API_KEY", CANARY);

/// Every environment the Python test's CHECKS table uses to turn a switch on, at once: an owner-shaped eve with every
/// door, key and lane.
fn owner_env() -> IndexMap<String, String> {
    let mut env = map(&[
        ("EVE_TELEGRAM", "1"), ("EVE_TELEGRAM_TOKEN", CANARY), ("EVE_TELEGRAM_OWNER", "4242"),
        ("EVE_MINIAPP", "1"), KEY, ("TYPESAFE_API_KEY", CANARY), ("EVE_MENU_TOOLS", "on"), ("EVE_MEMORY", "0"),
        ("EVE_MEM0_EMBEDDER", "gemma"), ("EVE_SECOND_BRAIN", "1"), ("EVE_CLAUDE_MEM", "1"), ("EVE_ROSBRIDGE", "1"),
        ("EVE_APPS", "1"), ("EVE_COMPOSIO", "1"), ("COMPOSIO_API_KEY", CANARY), ("EVE_SHARE", "1"),
        ("EVE_WEB_READER", "1"), ("TINYFISH_API_KEY", CANARY), ("EVE_WATCH_BROWSER", "1"), ("EVE_WATCH_TLS", "1"),
        ("EVE_CALL_LIVE_STT", "1"), ("OPENAI_API_KEY", CANARY), ("EVE_LISTEN_KEY", "option"), ("EVE_EXPLORE", "1"),
        ("EVE_ROBOT", "1"), ("EVE_BROWSER_ATTACH", "1"), ("EVE_BROWSER_OWN", "1"), ("EVE_BROWSER_STEEL", "1"),
        ("EVE_PIKA", "1"), ("EVE_AMERICA", "1"), ("EVE_OPENTABLE", "1"), ("EVE_AGENTCARD", "1"),
        ("AGENTCARD_ACCESS_TOKEN", CANARY), ("CANVAS_BASE_URL", "https://school.instructure.example"),
        ("CANVAS_API_TOKEN", CANARY), ("EVE_CHIEF", "on"), ("EVE_COMMAND_RISK", "shadow"),
    ]);
    for app in eve_app_registry::APPS.iter() {
        env.entry(app.switch.clone()).or_insert_with(|| "1".to_string());
    }
    env
}

#[test]
fn test_each_forced_app_switch_reads_as_lite_in_the_registry() {
    for app in eve_app_registry::APPS.iter() {
        let on: HashMap<String, String> = HashMap::from([(app.switch.clone(), "1".to_string())]);
        assert!(eve_app_registry::switched_on(app, Some(&on)), "control: {} should read as on here", app.switch);
        let mut lite = on.clone();
        lite.insert(app.switch.clone(), profile::LITE_FORCED[app.switch.as_str()].clone());
        assert!(!eve_app_registry::switched_on(app, Some(&lite)), "{}", app.switch);
    }
}

#[test]
fn test_every_app_reads_off_from_the_applied_environment() {
    let mut env: HashMap<String, String> = owner_env().into_iter().collect();
    env.insert("EVE_PROFILE".into(), "lite".into());
    env.insert("HOME".into(), "/Users/someone".into());
    let owner = env.clone();
    profile::apply(&mut env).unwrap();
    for app in eve_app_registry::APPS.iter() {
        assert!(eve_app_registry::switched_on(app, Some(&owner)), "control: {}", app.switch);
        assert!(!eve_app_registry::switched_on(app, Some(&env)), "{}", app.switch);
    }
    for (name, value) in profile::LITE_FORCED.iter() {
        assert_eq!(env.get(name), Some(value), "{name}");
    }
}

#[test]
fn test_apply_reports_what_it_changed_and_runs_once() {
    let mut env = owner_env();
    env.insert("EVE_PROFILE".into(), "lite".into());
    env.insert("HOME".into(), "/Users/someone".into());
    let changed = profile::apply(&mut env).unwrap();
    for name in ["EVE_TELEGRAM", "EVE_MINIAPP", "EVE_BRIDGE_SOCK", "TINYFISH_API_KEY", "EVE_TELEGRAM_TOKEN"] {
        assert!(changed.iter().any(|c| c == name), "{name}");
    }
    let mut sorted = changed.clone();
    sorted.sort();
    assert_eq!(changed, sorted);
    assert!(!changed.iter().any(|c| c == "OPENROUTER_API_KEY"));
    let mut again = env.clone();
    assert_eq!(profile::apply(&mut again).unwrap(), Vec::<String>::new()); // already lite: nothing to do
    assert_eq!(again, env);
}

#[test]
fn test_the_kept_switches_keep_their_defaults() {
    // EVE_WATCH, EVE_THINK, EVE_SMALL_TALK, EVE_SCIENCE_SEARCH, EVE_WEB_SEARCH and EVE_CHAT_LOG are left alone
    let kept = ["EVE_WATCH", "EVE_THINK", "EVE_SMALL_TALK", "EVE_SCIENCE_SEARCH", "EVE_WEB_SEARCH", "EVE_CHAT_LOG"];
    assert!(!kept.iter().any(|k| profile::LITE_FORCED.contains_key(*k)));
    let mut env = map(&[("EVE_PROFILE", "lite"), ("HOME", "/Users/someone"), KEY]);
    profile::apply(&mut env).unwrap();
    assert!(!kept.iter().any(|k| env.contains_key(*k)));
    for k in kept {
        assert!(profile::LITE_CLASSIFIED[k].starts_with("kept: "), "{k}");
    }
}

// ---- secrets and defaults -----------------------------------------------------------------------------------------

#[test]
fn test_secrets_are_removed_but_the_openrouter_key() {
    let secrets = [
        "OPENAI_API_KEY", "ANTHROPIC_API_KEY", "COMPOSIO_API_KEY", "TINYFISH_API_KEY", "EVE_TELEGRAM_TOKEN",
        "EVE_GITHUB_MCP_TOKEN", "EVE_GOOGLE_MAPS_KEY", "SPOTIFY_CLIENT_ID", "SPOTIFY_CLIENT_SECRET",
        "SOME_SERVICE_PASSWORD", "lowercase_token", "EVE_TELEGRAM_OWNER",
    ];
    let plain = map(&[
        ("HOME", "/Users/someone"), ("PATH", "/usr/bin:/bin"), ("CANVAS_BASE_URL", "https://x.example"),
        ("EVE_WATCH_RATE", "6"), ("TOKEN_FILE", "/tmp/x"), ("EVE_KEY_METHOD", "post"),
    ]);
    let mut env = map(&[("EVE_PROFILE", "lite")]);
    env.extend(plain.clone());
    env.insert(KEY.0.into(), KEY.1.into());
    for s in secrets {
        env.insert(s.into(), CANARY.into());
    }
    env.insert("EVE_LISTEN_KEY".into(), "option".into());
    let removed: std::collections::BTreeSet<String> = profile::apply(&mut env)
        .unwrap()
        .into_iter()
        .filter(|n| !profile::LITE_FORCED.contains_key(n) && !profile::LITE_DEFAULTS.contains_key(n))
        .collect();
    assert_eq!(removed, secrets.iter().map(|s| s.to_string()).collect());
    assert!(!secrets.iter().any(|s| env.contains_key(*s)));
    assert_eq!(env["OPENROUTER_API_KEY"], CANARY); // the allowlist
    for (k, v) in &plain {
        assert_eq!(&env[k], v); // not secret-shaped: untouched
    }
    assert_eq!(env["EVE_LISTEN_KEY"], "off"); // ends in _KEY, and is forced: forced wins
}

#[test]
fn test_secret_shape() {
    for name in ["X_KEY", "X_TOKEN", "X_SECRET", "X_PASSWORD", "X_CLIENT_ID", "x_api_key", "EVE_TELEGRAM_OWNER"] {
        assert!(profile::is_secret_name(name), "{name}");
    }
    for name in ["KEYCHAIN", "TOKEN_FILE", "EVE_KEY_METHOD", "PATH", "EVE_TELEGRAM", "CLIENT_ID_FILE"] {
        assert!(!profile::is_secret_name(name), "{name}");
    }
    assert_eq!(profile::LITE_SECRET_ALLOW, &["OPENROUTER_API_KEY"]);
}

#[test]
fn test_the_ipc_socket_default_fills_only_what_is_unset() {
    let mut env = map(&[("EVE_PROFILE", "lite"), ("HOME", "/Users/someone")]);
    profile::apply(&mut env).unwrap();
    assert_eq!(env["EVE_BRIDGE_SOCK"], "/Users/someone/.eve-lite/ipc.sock");
    let mut env = map(&[("EVE_PROFILE", "lite"), ("HOME", "/Users/someone"), ("EVE_BRIDGE_SOCK", " ")]);
    profile::apply(&mut env).unwrap();
    assert_eq!(env["EVE_BRIDGE_SOCK"], "/Users/someone/.eve-lite/ipc.sock"); // blank is unset: never /tmp's socket
    let mut env = map(&[
        ("EVE_PROFILE", "lite"), ("HOME", "/Users/someone"), ("EVE_BRIDGE_SOCK", "/Users/someone/elsewhere.sock"),
    ]);
    assert!(!profile::apply(&mut env).unwrap().iter().any(|n| n == "EVE_BRIDGE_SOCK"));
    assert_eq!(env["EVE_BRIDGE_SOCK"], "/Users/someone/elsewhere.sock");
}

#[test]
fn test_the_log_line_carries_names_only() {
    // In a fresh process: tracing caches whether a call site is listened to, and a test on another thread that
    // logged first with nothing listening would leave this one's line unheard.
    let ran = common::run_child("child_log_line", &[], None);
    let lines: Vec<String> = serde_json::from_value(ran.result["lines"].clone()).unwrap();
    let lines: Vec<&String> = lines.iter().filter(|l| l.contains("profile: ")).collect();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("TINYFISH_API_KEY") && lines[0].contains("EVE_BRIDGE_SOCK"));
    assert!(!lines[0].contains(CANARY) && !lines[0].contains("/Users/someone"));
    let want = format!(
        "profile: eve lite: {} switches forced, 0 already so; set EVE_BRIDGE_SOCK; removed TINYFISH_API_KEY",
        profile::LITE_FORCED.len()
    );
    assert_eq!(lines[0].trim(), want);
}

#[test]
fn child_log_line() {
    if !common::is_child("child_log_line") {
        return;
    }
    let mut env = map(&[
        ("EVE_PROFILE", "lite"), ("HOME", "/Users/someone"), ("TINYFISH_API_KEY", CANARY), ("EVE_MINIAPP", "1"), KEY,
    ]);
    let (_, lines) = capture_log(|| profile::apply(&mut env).unwrap());
    common::print_result(&json!({"lines": lines}));
}

// ---- the tools eve lite has -----------------------------------------------------------------------------------------

#[test]
fn test_every_lite_tool_name_is_a_real_tool() {
    // The tools are defined by the Python modules the lite brain offers (telegram, memory, watch, science search,
    // web search): each name is a quoted tool name in one of them.
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src/eve_bridge");
    let source: String = ["telegram.py", "memory.py", "watch.py", "science_search.py", "websearch.py"]
        .iter()
        .map(|f| std::fs::read_to_string(format!("{dir}/{f}")).unwrap())
        .collect();
    for name in profile::LITE_TOOL_NAMES {
        assert!(source.contains(&format!("\"{name}\"")), "{name} is named by no tool module");
    }
    assert_eq!(profile::LITE_TOOL_PREFIXES, &["COMPOSIO_"]);
}

#[test]
fn test_the_tool_filter() {
    for name in profile::LITE_TOOL_NAMES.iter().copied().chain(["COMPOSIO_SEARCH_TOOLS", "COMPOSIO_MANAGE_CONNECTIONS"])
    {
        assert!(profile::lite_tool_allowed(name), "{name}");
    }
    for name in [
        "screenshot", "start_task", "send_file", "list_files", "read_file", "take_photo", "start_coding_session",
        "claude_relay", "codex_relay", "COMPOSIO_", "composio_search_tools", "GMAIL_SEND_EMAIL", "",
    ] {
        assert!(!profile::lite_tool_allowed(name), "{name}");
    }
    assert!(!profile::lite_tool_allowed_value(&Value::Null) && !profile::lite_tool_allowed_value(&json!(7)));
    assert!(profile::lite_tool_allowed_value(&json!("remember")));
    let tools = vec![
        json!({"type": "function", "name": "start_task"}), json!({"type": "function", "name": "remember"}),
        json!({"type": "web_search"}), json!({"type": "function", "function": {"name": "COMPOSIO_SEARCH_TOOLS"}}),
        json!({"type": "function"}), json!({"type": "code_interpreter"}),
        json!({"type": "function", "name": "think_hard"}),
    ];
    let kept = profile::filter_tools(&tools);
    let expected = [&tools[1], &tools[2], &tools[3], &tools[6]];
    assert_eq!(kept, expected);
    assert!(kept.iter().zip(expected).all(|(a, b)| std::ptr::eq(*a, b))); // the same objects
    assert_eq!(profile::filter_tools_owned(tools.clone()), expected.into_iter().cloned().collect::<Vec<_>>());
}

#[test]
fn test_tool_name_reads_each_shape() {
    assert_eq!(profile::tool_name(&json!({"name": "a"})), "a");
    assert_eq!(profile::tool_name(&json!({"name": "", "function": {"name": "b"}})), "b");
    assert_eq!(profile::tool_name(&json!({"type": "web_search"})), "web_search");
    assert_eq!(profile::tool_name(&json!({"type": "function"})), "");
    assert_eq!(profile::tool_name(&json!({"name": 7})), "");
    assert_eq!(profile::tool_name(&json!(["name"])), "");
}

#[test]
fn test_the_lite_chat_and_the_contract_names() {
    assert_eq!(profile::LITE_CHAT_ID, 1);
    assert_eq!(profile::PROFILE_ENV, "EVE_PROFILE");
    assert_eq!(profile::LITE_DEFAULTS.keys().collect::<Vec<_>>(), vec!["EVE_BRIDGE_SOCK"]);
    for app in eve_app_registry::APPS.iter() {
        assert!(profile::LITE_FORCED.contains_key(&app.switch), "{}", app.switch);
    }
}
