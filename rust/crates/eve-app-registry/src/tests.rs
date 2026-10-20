//! Port of `bridge/tests/test_app_registry.py` (the parts that test this module), plus the contract checks'
//! failure paths. The app modules are described here as their Python modules are today (2026-10-19): the registry
//! reads them through a [`ModuleSource`], and the app crates register the real ones.

use super::*;
use serde_json::json;
use std::path::PathBuf;

fn repo() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.."))
}

const TODAY_LABELS: [&str; 4] = ["lights", "music", "tv_control", "camera_aim"];
const TODAY_PREFIXES: [(&str, &str); 4] =
    [("lights_", "lights"), ("spotify_", "music"), ("firetv_", "tv_control"), ("camera_", "camera_aim")];
const OWNERS: [&str; 5] = ["lights", "spotify", "firetv", "obsbot", "pika"];

fn newer() -> Vec<&'static App> {
    APPS.iter().filter(|a| !OWNERS.contains(&a.slot.as_str())).collect()
}

fn names(apps: &[&App]) -> Vec<String> {
    apps.iter().map(|a| a.slot.clone()).collect()
}

fn tools_json(names: &[&str]) -> Value {
    Value::Array(names.iter().map(|n| json!({"name": n, "description": "", "parameters": {}})).collect())
}

fn tools_module(name: &str, tool_names: &[&str]) -> ModuleInfo {
    ModuleInfo {
        name: name.to_string(),
        tool_names: Some(tool_names.iter().map(|n| n.to_string()).collect()),
        tools: Some(tools_json(tool_names)),
        off_reason: Some(format!("{name} is off")),
        instructions: Some(format!("{name} block")),
        ..ModuleInfo::new(name)
    }
}

fn fns(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| n.to_string()).collect()
}

/// The owner's app modules as their Python modules define them today.
fn today_modules() -> Modules {
    let lights_hub = ClassInfo::new("Lights", &["close", "match", "run", "panel", "listing", "tools", "handle"]);
    let lights = ModuleInfo {
        functions: fns(&["make_lights", "enabled", "cli"]),
        classes: vec![
            ClassInfo::new("GoveeDriver", &["close"]),
            ClassInfo::new("Command", &[]),
            lights_hub.clone(),
        ],
        returns: [("make_lights".to_string(), lights_hub)].into_iter().collect(),
        instructions: None,
        instructions_head: Some("You can control the owner's lights with lights_set".to_string()),
        ..tools_module("lights", &["lights_set", "lights_match", "lights_status"])
    };
    let spotify_hub = ClassInfo::new("Spotify", &["match", "run", "panel", "close"]);
    let spotify = ModuleInfo {
        functions: fns(&["make_spotify", "enabled", "cli", "match"]),
        classes: vec![spotify_hub.clone()],
        returns: [("make_spotify".to_string(), spotify_hub)].into_iter().collect(),
        ..tools_module("spotify", &["spotify_now_playing", "spotify_control"])
    };
    let firetv_hub = ClassInfo::new("FireTV", &["panel", "match", "run", "close"]);
    let firetv = ModuleInfo {
        functions: fns(&["make_firetv", "enabled", "cli", "match"]),
        classes: vec![ClassInfo::new("App", &[]), firetv_hub.clone()],
        returns: [("make_firetv".to_string(), firetv_hub)].into_iter().collect(),
        ..tools_module("firetv", &["firetv_status", "firetv_control", "firetv_screenshot"])
    };
    let obsbot_hub = ClassInfo::new("ObsbotPanel", &["panel", "match", "run", "stream", "close"]);
    let obsbot_panel = ModuleInfo {
        functions: fns(&["make_panel", "enabled", "match"]),
        classes: vec![obsbot_hub.clone(), ClassInfo::new("Capture", &[])],
        returns: [("make_panel".to_string(), obsbot_hub)].into_iter().collect(),
        ..tools_module("obsbot_panel", &["camera_status", "camera_control"])
    };
    let obsbot = ModuleInfo { functions: fns(&["cli"]), ..ModuleInfo::new("obsbot") };
    let pika_hub = ClassInfo::new("Pika", &["panel", "close", "stream"]);
    let pika = ModuleInfo {
        functions: fns(&["make_pika", "enabled"]),
        classes: vec![ClassInfo::new("PikaConfig", &[]), pika_hub.clone()],
        returns: [("make_pika".to_string(), pika_hub)].into_iter().collect(),
        ..ModuleInfo::new("pika")
    };
    Modules::new().with(lights).with(spotify).with(firetv).with(obsbot_panel).with(obsbot).with(pika)
}

// ---- the registry itself --------------------------------------------------------------------------------------------

#[test]
fn test_every_app_meets_the_contract_once() {
    let modules = today_modules();
    let slots: Vec<String> = APPS.iter().map(|a| a.slot.clone()).collect();
    assert_eq!(&slots[..OWNERS.len()], OWNERS); // tool order is model bytes
    assert_eq!(slots.iter().collect::<BTreeSet<_>>().len(), APPS.len());
    let switches: HashMap<&str, &str> = [
        ("lights", "EVE_LIGHTS"),
        ("spotify", "EVE_SPOTIFY"),
        ("firetv", "EVE_FIRETV"),
        ("obsbot", "EVE_OBSBOT"),
        ("pika", "EVE_PIKA"),
    ]
    .into_iter()
    .collect();
    for app in APPS.iter() {
        assert_eq!(check_in(app, &modules), Vec::<String>::new(), "{}", app.slot);
        let want = switches.get(app.slot.as_str()).map(|s| s.to_string()).unwrap_or(format!("EVE_{}", app.slot.to_uppercase()));
        assert_eq!(app.switch, want);
    }
    for slot in CODE_ORDER.iter().chain(PHONE_ORDER) {
        assert!(get(slot).is_some(), "{slot}");
    }
    assert_eq!(&names(&by_code())[..4], ["firetv", "obsbot", "lights", "spotify"]);
    assert_eq!(&names(&for_phone())[..5], ["spotify", "lights", "firetv", "pika", "obsbot"]);
    let pika = get("pika").expect("pika");
    assert!(!pika.default_on && !pika.tools);
}

#[test]
fn test_ordered_puts_unnamed_apps_last() {
    let mut want: Vec<String> = ["pika", "lights", "spotify", "firetv", "obsbot"].iter().map(|s| s.to_string()).collect();
    want.extend(newer().iter().map(|a| a.slot.clone()));
    assert_eq!(names(&ordered(["pika"])), want);
}

#[test]
fn test_ordered_skips_unknown_slots() {
    assert_eq!(names(&ordered(["toaster", "obsbot"]))[..2], ["obsbot", "lights"]);
    assert_eq!(ordered(Vec::<String>::new()).len(), APPS.len());
}

#[test]
fn test_switched_on_reads_the_switch_both_ways() {
    let (lights, pika) = (get("lights").expect("lights"), get("pika").expect("pika"));
    let env = |k: &str, v: &str| -> HashMap<String, String> { [(k.to_string(), v.to_string())].into_iter().collect() };
    let empty = HashMap::new();
    assert!(switched_on(lights, Some(&empty)) && !switched_on(lights, Some(&env("EVE_LIGHTS", "off"))));
    assert!(!switched_on(pika, Some(&empty)) && switched_on(pika, Some(&env("EVE_PIKA", "yes"))));
    // stripped and lowercased, as Python's str(...).strip().lower()
    assert!(!switched_on(lights, Some(&env("EVE_LIGHTS", "  FALSE \n"))));
    assert!(switched_on(pika, Some(&env("EVE_PIKA", " On "))));
    // Python's strip() also trims the information separators U+001C to U+001F
    assert!(!switched_on(lights, Some(&env("EVE_LIGHTS", "\u{1f}off\u{1c}"))));
    assert!(switched_on(pika, Some(&env("EVE_PIKA", "\u{1d}1\u{a0}"))));
    // anything that is not an off word leaves a default-on app on, and an on word is needed for a default-off app
    assert!(switched_on(lights, Some(&env("EVE_LIGHTS", "maybe"))));
    assert!(!switched_on(pika, Some(&env("EVE_PIKA", "maybe"))));
}

#[test]
fn test_a_hub_for_no_app_is_refused() {
    assert_eq!(check_hubs(["lights", "obsbot"]), Ok(()));
    assert!(check_hubs(["toaster"]).is_err());
    assert!(check_hubs(["pika"]).is_err()); // pika gives the models no tools
    let err = check_hubs(["zed", "lights", "toaster", "zed"]).expect_err("refused");
    assert_eq!(err.to_string(), "unexpected app hub(s): toaster, zed (not a tools app in app_registry.APPS)");
}

#[test]
fn test_owner_finds_the_app_and_out_loud_skips_voice_omit() {
    let m = today_modules();
    let slot = |a: Option<&App>| a.map(|a| a.slot.clone());
    assert_eq!(slot(owner_in("lights_set", false, &m)).as_deref(), Some("lights"));
    assert_eq!(slot(owner_in("firetv_screenshot", false, &m)).as_deref(), Some("firetv"));
    assert!(owner_in("firetv_screenshot", true, &m).is_none());
    assert_eq!(slot(owner_in("firetv_control", true, &m)).as_deref(), Some("firetv"));
    assert!(owner_in("web_search", false, &m).is_none());
}

#[test]
fn test_owner_reads_the_registered_modules() {
    for name in ["lights", "spotify", "firetv", "obsbot_panel", "obsbot", "pika"] {
        register_module((*today_modules().module(name).expect("module")).clone());
    }
    assert_eq!(owner("camera_control", false).map(|a| a.slot.as_str()), Some("obsbot"));
    assert_eq!(get("spotify").and_then(|a| a.module_info()).map(|m| m.tool_names().len()), Some(2));
    assert_eq!(get("obsbot").and_then(|a| a.cli_module_info()).map(|m| m.name.clone()).as_deref(), Some("obsbot"));
    assert_eq!(check(get("lights").expect("lights")), Vec::<String>::new());
}

#[test]
fn test_an_unregistered_module_holds_no_tools() {
    let none = Modules::new();
    assert!(owner_in("lights_set", false, &none).is_none());
    assert_eq!(check_in(get("lights").expect("lights"), &none), vec!["eve_bridge.lights is not registered"]);
    // the camera's CLI lives in another module; without it, check names that module
    let only_panel = Modules::new().with((*today_modules().module("obsbot_panel").expect("panel")).clone());
    assert_eq!(check_in(get("obsbot").expect("obsbot"), &only_panel), vec!["eve_bridge.obsbot is not registered"]);
}

#[test]
fn test_the_tool_gate_sets_are_todays() {
    // the apps' part of tool_gate.ACTING, CLEAN, _SOURCES, _SAY_HEADS and _OWN_WORDS_ACTIONS (the tool-gate crate
    // holds the rest)
    let owners_only: Vec<&App> = APPS.iter().filter(|a| OWNERS.contains(&a.slot.as_str())).collect();
    let newer_set = |f: fn(&App) -> &BTreeSet<String>| -> BTreeSet<String> {
        newer().iter().flat_map(|a| f(a).iter().cloned()).collect()
    };
    let mut want_acting = fns(&["camera_control", "firetv_control", "lights_match", "lights_set", "spotify_control"]);
    want_acting.extend(newer_set(|a| &a.acting));
    assert_eq!(acting(), want_acting);
    let mut want_clean = fns(&["camera_status", "firetv_status", "lights_status"]);
    want_clean.extend(newer_set(|a| &a.clean));
    assert_eq!(clean(), want_clean);
    let today_sources: IndexMap<String, String> =
        map(&[("spotify_control", "Spotify"), ("spotify_now_playing", "Spotify")]);
    let today_say: IndexMap<String, String> = map(&[
        ("lights_set", "change the lights"),
        ("lights_match", "change the lights"),
        ("spotify_control", "change the music"),
        ("firetv_control", "use the TV"),
        ("camera_control", "move the camera"),
    ]);
    let filtered = |m: IndexMap<String, String>, keep: &IndexMap<String, String>| -> IndexMap<String, String> {
        m.into_iter().filter(|(k, _)| keep.contains_key(k)).collect()
    };
    assert_eq!(filtered(sources(), &today_sources), today_sources);
    assert_eq!(filtered(say(), &today_say), today_say);
    let mut want_own: IndexMap<String, BTreeSet<String>> =
        [("spotify_control".to_string(), fns(&["pause", "volume", "transfer", "mode_on", "mode_off", "mode"]))]
            .into_iter()
            .collect();
    for a in newer() {
        want_own.extend(a.own_words.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    assert_eq!(own_words(), want_own);
    let owners_sources: IndexMap<String, String> =
        owners_only.iter().flat_map(|a| a.sources.iter().map(|(k, v)| (k.clone(), v.clone()))).collect();
    assert_eq!(owners_sources, today_sources);
    let owners_say: IndexMap<String, String> =
        owners_only.iter().flat_map(|a| a.say.iter().map(|(k, v)| (k.clone(), v.clone()))).collect();
    // the dict comparison in Python ignores order; here the order is also the registry's
    assert_eq!(owners_say, today_say);
    assert_eq!(owners_say.keys().collect::<Vec<_>>(), today_say.keys().collect::<Vec<_>>());
}

#[test]
fn test_the_eval_labels_and_prefixes_are_todays() {
    assert_eq!(&labels()[..4], TODAY_LABELS);
    assert_eq!(&prefixes()[..4], TODAY_PREFIXES);
    // no app's label is taken twice
    assert_eq!(labels().iter().collect::<BTreeSet<_>>().len(), labels().len());
}

// ---- the contract's failures ----------------------------------------------------------------------------------------

#[test]
fn test_check_names_every_broken_part() {
    let app = App {
        slot: "Fan".into(),
        module: "fan".into(),
        make: "make_fan".into(),
        switch: "FAN".into(),
        tool_prefix: "fan_".into(),
        acting: fs(&["fan_set", "fan_spin"]),
        code: true,
        code_taint_tool: "fan_spin".into(),
        stream: true,
        cli: vec!["fan".into()],
        ..App::default()
    };
    let module = ModuleInfo {
        tool_names: Some(vec!["fan_set".into(), "blow".into()]),
        tools: Some(tools_json(&["fan_set"])),
        ..ModuleInfo::new("fan")
    };
    let problems = check_in(&app, &Modules::new().with(module));
    assert_eq!(
        problems,
        vec![
            "slot 'Fan' is not a lowercase name outside ['app', 'memory', 'notes']",
            "switch 'FAN' does not start with EVE_",
            "fan.make_fan is missing",
            "fan.OFF_REASON is missing",
            "fan.TOOLS is missing, or its names are not TOOL_NAMES in order",
            "fan.INSTRUCTIONS is missing (the intent eval lends it)",
            "fan_spin is not one of fan.TOOL_NAMES",
            "blow does not start with fan_",
            "code_taint_tool fan_spin is not one of its tools",
            "a code app needs a match(text)",
            "a code app's hub needs run(cmd)",
            "a controls app's hub needs panel(body)",
            "a stream app's hub needs stream(writer)",
            "fan.cli is missing",
        ]
    );
}

#[test]
fn test_check_wants_tool_names_and_a_prefix() {
    let app = App { slot: "memory".into(), module: "kettle".into(), make: "make".into(), switch: "EVE_K".into(),
                    controls: false, ..App::default() };
    let module = ModuleInfo { functions: fns(&["make"]), off_reason: Some(String::new()),
                              instructions: Some(String::new()), tools: Some(json!([])), ..ModuleInfo::new("kettle") };
    assert_eq!(
        check_in(&app, &Modules::new().with(module)),
        vec![
            "slot 'memory' is not a lowercase name outside ['app', 'memory', 'notes']",
            "kettle.TOOL_NAMES is missing",
            "a tools app needs a tool_prefix",
        ]
    );
}

#[test]
fn test_hub_class_is_the_factorys_return_else_the_one_panel_class() {
    let hub = ClassInfo::new("Hub", &["panel", "close", "run"]);
    let other = ClassInfo::new("Other", &["panel", "close"]);
    let mut m = ModuleInfo { functions: fns(&["make"]), classes: vec![hub.clone()], ..ModuleInfo::new("m") };
    assert_eq!(hub_class(&m, "make"), Some(hub.clone())); // the scan: one class with panel and close
    m.classes.push(other.clone());
    assert_eq!(hub_class(&m, "make"), None); // two: no hub
    m.returns.insert("make".into(), other.clone());
    assert_eq!(hub_class(&m, "make"), Some(other.clone()));
    // a return annotation is read only when the factory exists
    m.functions.clear();
    assert_eq!(hub_class(&m, "make"), None);
}

// ---- the phone's contract -------------------------------------------------------------------------------------------

#[test]
fn test_the_panel_contract_example_passes_and_bad_controls_do_not() {
    let text = std::fs::read_to_string(repo().join("bridge/tests/fixtures/app_panel_example.json")).expect("fixture");
    let example: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(check_panel(&example), Vec::<String>::new());
    let built = panel(
        "On · speed 3",
        Some(vec![
            toggle("power", "Power", true, "power"),
            slider("speed", "Speed", 3, 1, 5, "speed", Some(json!(1))),
            button("boost", "Boost", "boost", "bolt", false),
            button("off", "Turn off", "off", "", true),
        ]),
        "The fan is on.",
        true,
    );
    assert_eq!(built, example);
    let with_controls = |controls: Value| -> Value {
        let mut e = example.clone();
        e["controls"] = controls;
        e
    };
    assert!(!check_panel(&with_controls(
        json!([{"type": "slider", "id": "s", "label": "S", "value": 1, "min": 5, "max": 5, "action": "s"}])
    ))
    .is_empty());
    assert!(!check_panel(&with_controls(json!([{"type": "dial", "id": "d"}]))).is_empty());
    assert!(!check_panel(&with_controls(Value::Array(vec![example["controls"][0].clone(); 25]))).is_empty());
    assert_eq!(check_panel(&json!({"ok": false, "line": "No."})), Vec::<String>::new());
}

#[test]
fn test_panel_writes_only_what_it_was_given() {
    assert_eq!(serde_json::to_string(&panel("", None, "", true)).expect("json"), r#"{"ok":true,"ui":1}"#);
    assert_eq!(
        serde_json::to_string(&panel("s", Some(vec![]), "l", false)).expect("json"),
        r#"{"ok":false,"ui":1,"line":"l","status":"s","controls":[]}"#
    );
    assert_eq!(
        serde_json::to_string(&slider("v", "Vol", 0.5, 0, 1, "v", None)).expect("json"),
        r#"{"type":"slider","id":"v","label":"Vol","value":0.5,"min":0,"max":1,"action":"v"}"#
    );
}

#[test]
fn test_check_panel_messages() {
    assert_eq!(check_panel(&json!([1])), vec!["the answer is not a JSON object"]);
    assert_eq!(
        check_panel(&json!({"ok": "yes", "line": 3, "status": null, "ui": 2, "controls": {}})),
        vec!["ok must be true or false", "line must be a string", "status must be a string", "ui must be 1",
             "controls must be a list"]
    );
    // ui equal to 1 as Python compares it: 1.0 and true pass
    assert_eq!(check_panel(&json!({"ok": true, "ui": 1.0})), Vec::<String>::new());
    assert_eq!(check_panel(&json!({"ok": true, "ui": true, "controls": null})), Vec::<String>::new());
    assert_eq!(check_panel(&json!({"ok": false, "line": 1})), vec!["line must be a string"]);
    let controls = json!([
        "x",
        {"id": "a"},
        {"type": 5},
        {"type": "button", "id": "b", "label": "", "action": "go", "role": "primary"},
        {"type": "toggle", "id": "b", "label": "T", "action": "t", "on": 1},
        {"type": "slider", "id": "s", "label": "S", "action": "s", "value": true, "min": 0, "max": 1},
        {"type": "slider", "id": "s2", "label": "S", "action": "s", "value": 0, "min": 0, "max": 1, "step": 0},
        {"type": "slider", "id": "s3", "label": "S", "action": "s", "value": 0, "min": 0.5, "max": 1, "step": "1"},
        {"type": "button", "label": "L", "action": "x"},
        {"type": "button", "id": "None", "label": "L", "action": "x"},
        {"type": "button", "id": 5, "label": "L", "action": "x"}
    ]);
    assert_eq!(
        check_panel(&json!({"ok": true, "ui": 1, "controls": controls})),
        vec![
            "controls[0] is not an object",
            "controls[1]: type None is not one of button, toggle, slider",
            "controls[2]: type 5 is not one of button, toggle, slider",
            "controls[3]: label must be a non-empty string",
            "controls[3]: role is destructive or absent",
            "controls[4]: id 'b' is used twice",
            "controls[4]: a toggle needs on",
            "controls[5]: a slider needs numeric value, min and max",
            "controls[6]: step must be a positive number",
            "controls[7]: step must be a positive number",
            "controls[8]: id must be a non-empty string",
            // str(None) was added for controls[8], so the string "None" is a second use
            "controls[9]: id 'None' is used twice",
            "controls[10]: id must be a non-empty string",
        ]
    );
}

#[test]
fn test_py_repr_matches_python() {
    assert_eq!(py_repr(&json!(null)), "None");
    assert_eq!(py_repr(&json!(true)), "True");
    assert_eq!(py_repr(&json!(-3)), "-3");
    assert_eq!(py_repr(&json!(1.5)), "1.5");
    assert_eq!(py_repr(&json!(2.0)), "2.0");
    assert_eq!(py_repr(&json!(1e16)), "1e+16");
    assert_eq!(py_repr(&json!(1.5e-5)), "1.5e-05");
    assert_eq!(py_repr(&json!(0.0001)), "0.0001");
    assert_eq!(py_repr(&json!(123456789012345.6)), "123456789012345.6");
    assert_eq!(py_repr(&json!("it's")), "\"it's\"");
    assert_eq!(py_repr(&json!("a'\"b\n")), "'a\\'\"b\\n'");
    assert_eq!(py_repr(&json!(["a", {"k": 1}])), "['a', {'k': 1}]");
    // what str.isprintable() rejects is escaped; the rest prints as itself (values from Python 3.13's repr)
    for (s, want) in [
        ("a\u{a0}b", "'a\\xa0b'"),
        ("\u{200b}", "'\\u200b'"),
        ("\u{e000}", "'\\ue000'"),
        ("é", "'é'"),
        ("\u{7f}", "'\\x7f'"),
        ("\u{e0001}", "'\\U000e0001'"),
        ("\u{3000}", "'\\u3000'"),
        ("\u{fffe}", "'\\ufffe'"),
        ("\u{85}", "'\\x85'"),
        ("日本", "'日本'"),
    ] {
        assert_eq!(py_str_repr(s), want, "{s:?}");
    }
    // a duplicate id with a no-break space is named as Python names it
    let dup = json!({"type": "button", "id": "a\u{a0}b", "label": "x", "action": "y"});
    assert_eq!(
        check_panel(&json!({"ok": true, "ui": 1, "controls": [dup.clone(), dup]})),
        vec!["controls[1]: id 'a\\xa0b' is used twice"]
    );
}

// ---- the docs -------------------------------------------------------------------------------------------------------

#[test]
fn test_agents_md_names_every_switch_and_the_walkthrough_exists() {
    let agents = std::fs::read_to_string(repo().join("AGENTS.md")).expect("AGENTS.md");
    for app in APPS.iter() {
        assert!(repo().join(&app.doc).is_file(), "{}", app.doc);
        if OWNERS.contains(&app.slot.as_str()) {
            // the shipped apps' table; an app of your own need not be in it
            assert!(agents.contains(&app.switch), "{}", app.switch);
        }
    }
    assert!(repo().join("docs/adding-an-app.md").is_file());
    assert!(agents.contains("docs/adding-an-app.md"));
}

// ---- names a new app may take ---------------------------------------------------------------------------------------

#[test]
fn test_a_new_name_may_not_take_a_tool_prefix_a_label_or_a_wiring_attribute() {
    // the scaffold's own_prefixes and labels come from the app-template crate; these are the members the Python
    // test asserts on
    let taken = ["screenshot", "stealth", "camera_photo", "meet"];
    let modules = today_modules();
    let problem = |name: &str| name_problem_in(name, Vec::<String>::new(), taken, &modules, &site_dir());
    assert!(problem("camera").contains("obsbot")); // camera_ is the OBSBOT's prefix
    assert!(problem("music").contains("label") && problem("screenshot").contains("label"));
    assert!(problem("stealth").contains("label"));
    for name in ["quick", "today", "config", "gate", "brief", "conn", "link", "calls", "port"] {
        assert!(problem(name).contains("wiring"), "{name}"); // the inlet, the voice, the server or the daemon has it
    }
    for name in ["fan", "kettle", "blinds", "coin"] {
        assert_eq!(problem(name), "", "{name}");
    }
}

#[test]
fn test_name_problem_messages() {
    let m = today_modules();
    let dir = tempfile::tempdir().expect("tempdir");
    let problem = |name: &str, own: &[&str]| name_problem_in(name, own.iter().copied(), ["meet"], &m, dir.path());
    let shape = "a name is 2 to 16 lowercase letters or digits, starting with a letter";
    for bad in ["Fan", "f", "2fan", "fan_x", "abcdefghijklmnopq", ""] {
        assert_eq!(problem(bad, &[]), shape, "{bad}");
    }
    assert_eq!(problem("abcdefghijklmnop", &[]), "");
    assert_eq!(problem("class", &[]), "class is a Python keyword");
    assert_eq!(problem("match", &[]), "match is a Python keyword");
    assert_eq!(problem("type", &[]), "type is a Python keyword");
    assert_eq!(problem("lights", &[]), "lights is taken");
    assert_eq!(problem("memory", &[]), "memory is taken");
    assert_eq!(problem("app", &[]), "app is taken");
    assert_eq!(problem("watch", &["watch_"]), "watch_ would collide with eve's own tools (watch_…)");
    assert_eq!(problem("go", &["go_explore"]), "go_ would collide with eve's own tools (go_explore…)");
    assert_eq!(problem("lights2", &[]), "");
    assert_eq!(problem("camera", &[]), "camera_ would collide with the obsbot app's tools (camera_…)");
    assert_eq!(problem("meet", &[]), "meet is already an intent-eval label or one of eve's tools");
    assert_eq!(problem("music", &[]), "music is already an intent-eval label or one of eve's tools");
    // a tool name of an app that does not share its prefix
    let odd = ModuleInfo { tool_names: Some(vec!["lights_set".into(), "kettle_on".into()]),
                           ..(*m.module("lights").expect("lights")).clone() };
    let m2 = today_modules().with(odd);
    assert_eq!(
        name_problem_in("kettle", Vec::<String>::new(), Vec::<String>::new(), &m2, dir.path()),
        "kettle_ would collide with the lights app's tool kettle_on"
    );
}

#[test]
fn test_site_names_reads_attributes_defs_and_fields() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("telegram.py"),
        "class Inlet:\n    quick: int = 1\n    _fan = None\n    def go(self):\n        self.kettle = server.blinds\n\nasync def brew():\n    pass\n",
    )
    .expect("write");
    std::fs::write(dir.path().join("other.py"), "self.ignored = 1\n").expect("write"); // not a site
    let got = site_names_in(dir.path());
    assert_eq!(got, fns(&["_fan", "blinds", "brew", "go", "kettle", "quick"]));
    let m = today_modules();
    assert_eq!(
        name_problem_in("fan", Vec::<String>::new(), Vec::<String>::new(), &m, dir.path()),
        "fan would clash with _fan, a name eve's wiring already uses"
    );
    assert_eq!(
        name_problem_in("kettle", Vec::<String>::new(), Vec::<String>::new(), &m, dir.path()),
        "kettle would clash with kettle, a name eve's wiring already uses"
    );
    // the real sites are found
    assert!(site_names().contains("_quick") || site_names().contains("quick"));
}
