//! profile.py's tables, byte for byte: every forced switch, default, reason, secret rule, tool name and protected
//! folder, in Python's order, against `fixtures/tables.json` (written by `fixtures/gen_tables.py` from the Python
//! module itself).

use std::collections::HashMap;

use eve_profile as profile;
use serde_json::{json, Value};

fn tables() -> Value {
    serde_json::from_str(include_str!("fixtures/tables.json")).unwrap()
}

fn pairs<'a, K: AsRef<str> + 'a, V: AsRef<str> + 'a>(items: impl IntoIterator<Item = (&'a K, &'a V)>) -> Value {
    Value::Array(items.into_iter().map(|(k, v)| json!([k.as_ref(), v.as_ref()])).collect())
}

fn list(items: &[&str]) -> Value {
    json!(items)
}

fn sorted(items: &[&str]) -> Value {
    let mut v = items.to_vec();
    v.sort();
    json!(v)
}

#[test]
fn test_the_switch_tables_are_pythons_in_its_order() {
    let t = tables();
    assert_eq!(pairs(profile::LITE_FORCED.iter()), t["LITE_FORCED"]);
    assert_eq!(pairs(profile::LITE_DEFAULTS.iter()), t["LITE_DEFAULTS"]);
    assert_eq!(pairs(profile::NOT_SECRET_NAMES.iter()), t["NOT_SECRET_NAMES"]);
    let classified = pairs(profile::LITE_CLASSIFIED.iter());
    let want = t["LITE_CLASSIFIED"].as_array().unwrap();
    let got = classified.as_array().unwrap();
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        assert_eq!(g, w, "entry {i}");
    }
    assert_eq!(got.len(), want.len());
}

#[test]
fn test_the_secret_and_tool_rules_are_pythons() {
    let t = tables();
    assert_eq!(sorted(profile::LITE_SECRET_ALLOW), t["LITE_SECRET_ALLOW"]);
    assert_eq!(list(profile::SECRET_SUFFIXES), t["SECRET_SUFFIXES"]);
    assert_eq!(sorted(profile::SECRET_NAMES), t["SECRET_NAMES"]);
    assert_eq!(sorted(profile::LITE_TOOL_NAMES), t["LITE_TOOL_NAMES"]);
    assert_eq!(list(profile::LITE_TOOL_PREFIXES), t["LITE_TOOL_PREFIXES"]);
}

#[test]
fn test_the_firewall_tables_are_pythons() {
    let t = tables();
    assert_eq!(sorted(profile::EXEC_ALLOW), t["EXEC_ALLOW"]);
    assert_eq!(list(profile::PROTECTED_HOME), t["PROTECTED_HOME"]);
    assert_eq!(list(profile::PROTECTED_ROOTS), t["PROTECTED_ROOTS"]);
    assert_eq!(list(profile::FIREWALL_MODES), t["FIREWALL_MODES"]);
    let env = HashMap::from([("HOME".to_string(), "/Users/someone".to_string())]);
    let roots: Vec<Value> = profile::protected_roots(Some(&env)).iter().map(|(p, n)| json!([p, n])).collect();
    assert_eq!(Value::Array(roots), t["protected_roots"]);
}
