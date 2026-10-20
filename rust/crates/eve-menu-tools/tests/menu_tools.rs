//! Port of the parts of `bridge/tests/test_menu_tools.py` (and the `menu_tools` asserts of
//! `test_menu_off_bytes.py`) that test this module itself. The rest drive telegram's inlet and belong to its crate.

use std::collections::{BTreeSet, HashMap};

use eve_menu_tools as menu_tools;
use serde_json::{json, Value};

fn env(value: &str) -> HashMap<String, String> {
    HashMap::from([("EVE_MENU_TOOLS".to_string(), value.to_string())])
}

fn on() -> HashMap<String, String> {
    env("on")
}

fn names(tools: &[Value]) -> Vec<String> {
    tools.iter().map(|t| t["name"].as_str().unwrap().to_string()).collect()
}

/// The Python module's own output, `json.dumps(..., separators=(",", ":"), ensure_ascii=False)`, captured from
/// `bridge/src/eve_bridge/menu_tools.py`: the schemas are prompt text, so they must match byte for byte.
const PYTHON: &str = include_str!("fixtures/python_tools.json");

#[test]
fn test_tool_schemas_are_the_pythons_byte_for_byte() {
    let py: Value = serde_json::from_str(PYTHON).unwrap();
    let tools = serde_json::to_string(&*menu_tools::TOOLS).unwrap();
    assert_eq!(tools, serde_json::to_string(&py["TOOLS"]).unwrap());
    let file_tools = serde_json::to_string(&*menu_tools::FILE_TOOLS).unwrap();
    assert_eq!(file_tools, serde_json::to_string(&py["FILE_TOOLS"]).unwrap());
    // and against the raw fixture text, so key order is checked too (not only the parsed value)
    assert!(PYTHON.starts_with(&format!("{{\"TOOLS\":{tools},\"FILE_TOOLS\":{file_tools},")));
    assert_eq!(json!(menu_tools::TOOL_NAMES), py["TOOL_NAMES"]);
    let mut hands: Vec<&str> = menu_tools::HANDS_OFF.to_vec();
    hands.sort();
    assert_eq!(json!(hands), py["HANDS_OFF"]);
}

#[test]
fn test_every_menu_tool_is_lent_allowed_and_dispatched_on_every_turn() {
    let lent = names(&menu_tools::tools(Some(&on())));
    // first, fixed: the cached prefix
    assert_eq!(lent[..menu_tools::TOOL_NAMES.len()], menu_tools::TOOL_NAMES.map(String::from));
    assert_eq!(names(&menu_tools::TOOLS), menu_tools::TOOL_NAMES.map(String::from));
}

#[test]
fn test_the_switch_off_lends_nothing_and_writes_no_line() {
    assert!(menu_tools::tools(Some(&env("off"))).is_empty());
    for value in ["", "maybe"] {
        assert_eq!(menu_tools::enabled(Some(&env(value))), menu_tools::SHIPPED);
    }
    assert_eq!(menu_tools::enabled(Some(&HashMap::new())), menu_tools::SHIPPED);
}

#[test]
fn test_switch_off_jobs_list_and_opentable_are_origin_main_s_wording() {
    // test_menu_off_bytes.py: with EVE_MENU_TOOLS=off the switch reads off; on is its positive control
    assert!(!menu_tools::enabled(Some(&env("off"))));
    assert!(menu_tools::enabled(Some(&env("on"))));
}

#[test]
fn test_the_switch_reads_every_spelling() {
    for value in ["1", "true", "yes", "on", " ON ", "True", "\tyes\n", "\u{1f}on\u{1c}"] {
        assert!(menu_tools::enabled(Some(&env(value))), "{value:?}");
    }
    for value in ["0", "false", "no", "off", " Off ", "NO"] {
        assert!(!menu_tools::enabled(Some(&env(value))), "{value:?}");
    }
    assert_eq!(menu_tools::MODES, ["on", "off"]);
}

#[test]
fn test_the_always_carried_toolsets_match_the_tools_and_leave_the_routed_seven_alone() {
    // toolset_router.ALWAYS[MENU] == TOOL_NAMES + (READ_FILE,): exactly what tools() lends, in order
    let mut always: Vec<String> = menu_tools::TOOL_NAMES.map(String::from).to_vec();
    always.push(menu_tools::READ_FILE.to_string());
    assert_eq!(names(&menu_tools::tools(Some(&on()))), always);
    for t in menu_tools::TOOLS.iter().chain(menu_tools::FILE_TOOLS.iter()) {
        let params = &t["parameters"];
        assert_eq!(t["strict"], json!(true));
        let required: BTreeSet<&str> =
            params["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        let properties: BTreeSet<&str> = params["properties"].as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(required, properties, "{}", t["name"]);
    }
}

#[test]
fn test_a_failed_hand_off_goes_back_to_the_model() {
    assert!(!menu_tools::ended(&json!({"ok": false, "reason": "x"})) && !menu_tools::ended(&json!({"ok": true})));
    assert!(menu_tools::ended(&json!({"ok": true, "end_turn": true})));
    // not a dict: never ends the turn
    assert!(!menu_tools::ended(&json!([{"ok": true, "end_turn": true}])) && !menu_tools::ended(&json!(null)));
    // Python truthiness, not strict booleans
    assert!(menu_tools::ended(&json!({"ok": 1, "end_turn": "yes"})));
    assert!(!menu_tools::ended(&json!({"ok": true, "end_turn": 0})));
}

#[test]
fn test_every_hand_off_is_a_menu_tool_and_spend_is_a_reading() {
    for name in menu_tools::HANDS_OFF {
        assert!(menu_tools::TOOL_NAMES.contains(&name) && menu_tools::hands_off(name));
    }
    assert!(!menu_tools::hands_off(menu_tools::SPEND) && !menu_tools::hands_off(menu_tools::READ_FILE));
}

#[test]
fn test_read_file_is_lent_with_the_switch_and_dispatched() {
    assert!(names(&menu_tools::tools(Some(&on()))).contains(&menu_tools::READ_FILE.to_string()));
    assert!(!names(&menu_tools::tools(Some(&env("off")))).contains(&menu_tools::READ_FILE.to_string()));
}
