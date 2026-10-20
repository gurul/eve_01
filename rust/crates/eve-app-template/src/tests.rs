//! Ports of the app_template tests in bridge/tests/test_app_registry.py, and parity checks against outputs the
//! daemon's Python 3.13 produced (tests/fixtures).

use super::*;
use eve_app_registry::name_problem;
use serde_json::Value;
use std::collections::BTreeMap;

fn repo() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.."))
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name))
        .expect("fixture")
}

/// Every file under `root` and its bytes (Python's `_digest`, readable).
fn digest(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).expect("read_dir").flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk(&p, root, out);
            } else {
                out.insert(p.strip_prefix(root).expect("under root").to_path_buf(), std::fs::read(&p).expect("read"));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// A repository with what the scaffold reads: bridge/pyproject.toml, the registry, bridge/tests and docs.
fn tmp_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("bridge").join("src").join("eve_bridge");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::copy(repo().join("bridge/src/eve_bridge/app_registry.py"), src.join("app_registry.py")).unwrap();
    std::fs::create_dir_all(tmp.path().join("bridge").join("tests")).unwrap();
    std::fs::write(tmp.path().join("bridge").join("pyproject.toml"), "").unwrap();
    std::fs::create_dir_all(tmp.path().join("docs")).unwrap();
    tmp
}

fn refused_text(r: Result<Vec<PathBuf>, ScaffoldError>) -> String {
    match r {
        Err(ScaffoldError::Refused(m)) => m,
        other => panic!("not refused: {other:?}"),
    }
}

#[test]
fn test_a_scaffolded_app_passes_its_own_tests_and_registers() {
    let tmp = tmp_repo();
    let root = tmp.path();
    let registry_before = std::fs::read_to_string(root.join("bridge/src/eve_bridge/app_registry.py")).unwrap();

    let before = digest(root);
    let would = scaffold("fan", Some(root), true).unwrap();
    assert_eq!(digest(root), before); // --dry-run writes nothing
    let wrote = scaffold("fan", Some(root), false).unwrap();
    let (mut a, mut b) = (would.clone(), wrote.clone());
    a.sort();
    b.sort();
    assert_eq!(a, b);
    assert!(wrote.iter().all(|p| p.is_file()));
    assert_eq!(wrote, vec![
        root.join("bridge/src/eve_bridge/fan.py"),
        root.join("bridge/tests/test_fan.py"),
        root.join("docs/fan.md"),
        root.join("bridge/src/eve_bridge/app_registry.py"),
    ]);

    // byte for byte what Python's scaffold wrote for the same name
    assert_eq!(std::fs::read_to_string(&wrote[0]).unwrap(), fixture("fan.py"));
    assert_eq!(std::fs::read_to_string(&wrote[1]).unwrap(), fixture("test_fan.py"));
    assert_eq!(std::fs::read_to_string(&wrote[2]).unwrap(), fixture("fan.md"));
    let marker_at = registry_before.find(&format!("    {}", eve_app_registry::MARKER)).unwrap();
    let want = format!("{}{}{}", &registry_before[..marker_at], fixture("fan_registry_line.txt"), &registry_before[marker_at..]);
    assert_eq!(std::fs::read_to_string(&wrote[3]).unwrap(), want);

    let after = digest(root);
    assert!(matches!(scaffold("fan", Some(root), false), Err(ScaffoldError::Refused(_)))); // a second time: refused
    for bad in ["lights", "memory", "app", "class", "watch", "Fan", "f", "camera", "music", "quick", "link", "screenshot"] {
        assert!(matches!(scaffold(bad, Some(root), false), Err(ScaffoldError::Refused(_))), "{bad}");
    }
    assert_eq!(digest(root), after); // and nothing changed
}

#[test]
fn test_a_new_name_may_not_take_a_tool_prefix_a_label_or_a_wiring_attribute() {
    let mut taken = eval_labels(&repo()).unwrap();
    taken.extend(own_names());
    for n in ["screenshot", "stealth", "camera_photo", "meet"] {
        assert!(taken.contains(n), "{n}");
    }
    let prefixes = own_prefixes();
    let problem = |name: &str| name_problem(name, &prefixes, &taken);
    assert!(problem("camera").contains("obsbot")); // camera_ is the OBSBOT's prefix
    assert!(problem("music").contains("label") && problem("screenshot").contains("label")
        && problem("stealth").contains("label"));
    for name in ["quick", "today", "config", "gate", "brief", "conn", "link", "calls", "port"] {
        assert!(problem(name).contains("wiring"), "{name}"); // the inlet, the voice, the server or the daemon has it
    }
    for name in ["fan", "kettle", "blinds", "coin"] {
        assert_eq!(problem(name), "", "{name}");
    }
}

#[test]
fn the_eval_labels_are_todays() {
    // Python 3.13's app_template.eval_labels over this checkout's bridge/tools/intent_eval.py
    let want = [
        "app_builder", "ask", "calendar_read", "calendar_write", "camera_photo", "chat_history_recall", "claude_relay",
        "codex", "coding_session", "computer_task", "contacts_read", "deep_reasoning", "email_read", "email_send",
        "events", "github", "gov_legal_lookup", "jobs_and_reminders", "linear", "mac_files_read", "maps_directions",
        "meet", "memory_forget", "memory_recall", "memory_write", "none", "notes", "notion", "nyt", "nyt_archive",
        "nyt_bestsellers", "nyt_headlines", "nyt_popular", "nyt_search", "phone_location", "phone_pairing",
        "price_watch", "restaurant_booking", "room_notes", "science_search", "screenshot", "send_file_to_owner", "slack",
        "spend_report", "stealth", "stop_task", "todos_read", "web_search",
    ];
    let got: Vec<String> = eval_labels(&repo()).unwrap().into_iter().collect();
    assert_eq!(got, want);
    assert!(eval_labels(&repo().join("docs")).unwrap().is_empty()); // no file: no labels
}

#[test]
fn eval_labels_reads_what_python_s_ast_walk_reads() {
    let cases: Vec<Value> = serde_json::from_str(&fixture("labels_cases.json")).unwrap();
    assert!(cases.len() > 20);
    for case in cases {
        let src = case["src"].as_str().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("bridge/tools")).unwrap();
        std::fs::write(tmp.path().join("bridge/tools/intent_eval.py"), src).unwrap();
        let got = eval_labels(tmp.path());
        match case.get("ok") {
            Some(Value::Array(want)) => {
                let want: Vec<&str> = want.iter().map(|v| v.as_str().unwrap()).collect();
                let got: Vec<String> = got.unwrap_or_else(|e| panic!("{src:?}: {e}")).into_iter().collect();
                assert_eq!(got, want, "{src:?}");
            }
            _ => assert!(matches!(got, Err(ScaffoldError::Syntax(_))), "{src:?}: {got:?}"),
        }
    }
}

#[test]
fn eval_labels_reads_crlf_as_python_s_read_text_does() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("bridge/tools")).unwrap();
    std::fs::write(tmp.path().join("bridge/tools/intent_eval.py"), "LABELS = ('''a\r\nb''',\r\n  'c')\r\n").unwrap();
    let got: Vec<String> = eval_labels(tmp.path()).unwrap().into_iter().collect();
    assert_eq!(got, ["a\nb", "c"]);
}

#[test]
fn own_prefixes_and_names_are_todays() {
    // Python 3.13's app_template.own_prefixes() and len(own_names())
    let want = [
        "agentcard_", "america_", "answer_", "capture_", "change_", "claude_", "close_", "codex_", "daily_", "delete_",
        "edit_", "events_", "file_", "find_", "forget_", "go_", "jobs_", "join_", "leave_", "list_", "look_", "make_",
        "meet_", "memory_", "move_", "nyt_", "open_", "opentable_", "pair_", "phone_", "read_", "recent_", "rename_",
        "restore_", "science_", "search_", "send_", "set_", "spend_", "start_", "steer_", "stop_", "take_", "think_",
        "undo_", "watch_", "web_",
    ];
    let got: Vec<String> = own_prefixes().into_iter().collect();
    assert_eq!(got, want);
    assert_eq!(own_names().len(), 82);
    assert!(own_names().contains("screenshot") && own_names().contains("lights_set"));
    // an app's own tools are not eve's prefixes
    assert!(!own_prefixes().contains("lights_") && !own_prefixes().contains("spotify_"));
}

#[test]
fn find_root_walks_up_to_bridge_pyproject() {
    let tmp = tmp_repo();
    let deep = tmp.path().join("bridge").join("deep").join("er");
    std::fs::create_dir_all(&deep).unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    assert_eq!(find_root(Some(&deep)).unwrap(), root);
    assert_eq!(find_root(Some(tmp.path())).unwrap(), root);
    assert_eq!(find_root(Some(&deep.join("not").join("there"))).unwrap(), root); // resolve() needs no folder
    let outside = tempfile::tempdir().unwrap();
    match find_root(Some(outside.path())) {
        Err(ScaffoldError::Refused(m)) => {
            assert_eq!(m, "run this inside eve's repository (no bridge/pyproject.toml above this folder)")
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_refusal_names_what_is_in_the_way() {
    let tmp = tmp_repo();
    let root = tmp.path();
    std::fs::write(root.join("docs").join("kettle.md"), "mine").unwrap();
    assert_eq!(refused_text(scaffold("kettle", Some(root), false)), "docs/kettle.md already exists");
    std::fs::write(root.join("bridge/tests/test_blinds.py"), "").unwrap();
    assert_eq!(refused_text(scaffold("blinds", Some(root), true)), "bridge/tests/test_blinds.py already exists");
    assert_eq!(refused_text(scaffold("Fan", Some(root), false)),
               "a name is 2 to 16 lowercase letters or digits, starting with a letter");
    assert_eq!(refused_text(scaffold("class", Some(root), false)), "class is a Python keyword");
    assert_eq!(refused_text(scaffold("music", Some(root), false)),
               "music is already an intent-eval label or one of eve's tools");

    let registry = root.join("bridge/src/eve_bridge/app_registry.py");
    let text = std::fs::read_to_string(&registry).unwrap();
    std::fs::write(&registry, text.replace("    # eve-bridge app new", "    App(slot=\"coin\")\n    # eve-bridge app new")).unwrap();
    assert_eq!(refused_text(scaffold("coin", Some(root), false)), "coin is already in the registry");
    std::fs::write(&registry, text.replace(eve_app_registry::MARKER, "# the end")).unwrap();
    assert_eq!(refused_text(scaffold("coin", Some(root), false)),
               "the marker line is missing from bridge/src/eve_bridge/app_registry.py");
    std::fs::remove_file(&registry).unwrap();
    assert_eq!(refused_text(scaffold("coin", Some(root), false)),
               "the marker line is missing from bridge/src/eve_bridge/app_registry.py");
    // the marker without its four spaces: Python's str.index raises ValueError, not Refused
    std::fs::write(&registry, format!("APPS = [\n{}\n]\n", eve_app_registry::MARKER)).unwrap();
    match scaffold("coin", Some(root), false) {
        Err(ScaffoldError::Value(m)) => assert_eq!(m, "substring not found"),
        other => panic!("{other:?}"),
    }
    assert!(!root.join("bridge/src/eve_bridge/coin.py").exists());
}

#[test]
fn the_registry_is_read_with_universal_newlines() {
    let tmp = tmp_repo();
    let registry = tmp.path().join("bridge/src/eve_bridge/app_registry.py");
    let text = std::fs::read_to_string(&registry).unwrap();
    std::fs::write(&registry, text.replace('\n', "\r\n")).unwrap();
    scaffold("fan", Some(tmp.path()), false).unwrap();
    let written = std::fs::read_to_string(&registry).unwrap();
    assert!(!written.contains('\r'));
    assert!(written.contains(&fixture("fan_registry_line.txt")));
}

#[test]
fn the_cli_parses_prints_and_exits_as_python_s_does() {
    let outside = tempfile::tempdir().unwrap();
    let env: HashMap<String, String> = HashMap::new();
    let lines = fixture("cli_cases.jsonl");
    let mut repo_root: Option<tempfile::TempDir> = None;
    let mut checked = 0;
    for line in lines.lines() {
        let case: Value = serde_json::from_str(line).unwrap();
        let argv: Vec<String> = case["argv"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        let cwd = if case["repo"].as_bool().unwrap() {
            let root = repo_root.get_or_insert_with(|| {
                let t = tmp_repo();
                std::fs::create_dir_all(t.path().join("bridge/deep/er")).unwrap();
                t
            });
            root.path().join(case["cwd"].as_str().unwrap())
        } else {
            outside.path().to_path_buf()
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = cli_in(&argv, &cwd, Some(&env), &mut out, &mut err);
        assert_eq!(code as i64, case["code"].as_i64().unwrap(), "{argv:?}");
        assert_eq!(String::from_utf8(out).unwrap(), case["out"].as_str().unwrap(), "{argv:?}");
        assert_eq!(String::from_utf8(err).unwrap(), case["err"].as_str().unwrap(), "{argv:?}");
        checked += 1;
    }
    assert!(checked > 60);
    // what `new fan` wrote is what Python wrote
    let root = repo_root.unwrap();
    assert_eq!(std::fs::read_to_string(root.path().join("bridge/src/eve_bridge/fan.py")).unwrap(), fixture("fan.py"));
}

#[test]
fn app_list_reads_the_switches_from_the_environment() {
    let env: HashMap<String, String> =
        HashMap::from([("EVE_LIGHTS".to_string(), "0".to_string()), ("EVE_PIKA".to_string(), " Yes ".to_string())]);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let here = std::env::temp_dir();
    assert_eq!(cli_in(&["list".to_string()], &here, Some(&env), &mut out, &mut err), 0);
    let out = String::from_utf8(out).unwrap();
    let rows: Vec<&str> = out.lines().collect();
    assert_eq!(rows.len(), eve_app_registry::APPS.len());
    for (row, app) in rows.iter().zip(eve_app_registry::APPS.iter()) {
        let on = if eve_app_registry::switched_on(app, Some(&env)) { "on" } else { "off" };
        assert_eq!(*row, format!("{:<10} {:<14} {:<4} {}", app.slot, app.switch, on, app.doc));
    }
    assert!(out.contains("EVE_LIGHTS     off "));
    assert!(err.is_empty());
}

/// Python's test also runs the generated app's own tests (7 pass) and its registration probe with the daemon's
/// Python; this does the same against what the Rust scaffold wrote. Needs a Python with pytest: `EVE_PYTHON`, else
/// the daemon checkout's venv.
#[test]
#[ignore = "live: runs the generated app's pytest with the daemon's Python"]
fn the_generated_app_passes_its_own_tests_with_the_daemons_python() {
    let python = std::env::var("EVE_PYTHON").unwrap_or_else(|_| {
        format!("{}/Documents/personal/eve/bridge/.venv/bin/python", std::env::var("HOME").unwrap_or_default())
    });
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let status = std::process::Command::new("cp")
        .arg("-R")
        .arg(repo().join("bridge/src/eve_bridge"))
        .arg(root.join("eve_bridge"))
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::create_dir_all(root.join("bridge/src")).unwrap();
    std::fs::rename(root.join("eve_bridge"), root.join("bridge/src/eve_bridge")).unwrap();
    std::fs::create_dir_all(root.join("bridge/tests")).unwrap();
    std::fs::copy(repo().join("bridge/tests/conftest.py"), root.join("bridge/tests/conftest.py")).unwrap();
    std::fs::write(root.join("bridge/pytest.ini"), "[pytest]\n").unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    let wrote = scaffold("demo", Some(root), false).unwrap();
    assert_eq!(wrote.len(), 4);
    let run = std::process::Command::new(&python)
        .args(["-m", "pytest", "-q", "-p", "no:cacheprovider", "-p", "no:xdist", "-c"])
        .arg(root.join("bridge/pytest.ini"))
        .arg("--rootdir")
        .arg(root.join("bridge"))
        .arg(root.join("bridge/tests/test_demo.py"))
        .current_dir(root)
        .env("PYTHONPATH", root.join("bridge/src"))
        .env_remove("EVE_DEMO")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success(), "{stdout}{}", String::from_utf8_lossy(&run.stderr));
    assert!(stdout.lines().last().unwrap_or_default().contains("7 passed"), "{stdout}");
    let probe = std::process::Command::new(&python)
        .args(["-c", "from eve_bridge import app_registry as r; print(r.APPS[-1].slot, r.check(r.APPS[-1]))"])
        .current_dir(root)
        .env("PYTHONPATH", root.join("bridge/src"))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&probe.stdout).lines().next().unwrap_or_default(), "demo []");
}
