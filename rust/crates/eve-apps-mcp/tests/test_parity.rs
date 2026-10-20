//! What the live Python answers (tests/fixtures/python_parity.json, written by gen_parity.py from apps_mcp.py and
//! github_repos.py), asserted against the port: thousands of generated inputs, so parity is measured.

use std::sync::LazyLock;

use eve_apps_mcp as apps;
use eve_apps_mcp::github_repos::{self as g, Repo};
use serde_json::{Map, Value};

static FIXTURE: LazyLock<Value> = LazyLock::new(|| {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/python_parity.json"
    ))
    .unwrap();
    serde_json::from_str(&text).unwrap()
});

fn cases(key: &str) -> &'static Vec<Value> {
    FIXTURE[key].as_array().unwrap()
}

fn map(v: &Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap()
}

#[test]
fn names_app_matches_python() {
    // REPO_NAMES stays empty in this binary, as the generator set it
    let mut bad = Vec::new();
    for case in cases("names_app") {
        let text = case[0].as_str().unwrap();
        let mut got: Vec<&str> = apps::APP_WORDS
            .keys()
            .copied()
            .filter(|app| apps::names_app(app, text))
            .collect();
        got.sort();
        let want: Vec<&str> = case[1]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        if got != want {
            bad.push(format!("{text:?}: rust {got:?} python {want:?}"));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {}:\n{}",
        bad.len(),
        cases("names_app").len(),
        bad.join("\n")
    );
}

#[test]
fn tool_reads_matches_python() {
    for case in cases("tool_reads") {
        assert_eq!(
            apps::tool_reads(&case[0]),
            case[1].as_bool().unwrap(),
            "{}",
            case[0]
        );
        assert_eq!(
            apps::name_reads(case[0]["name"].as_str().unwrap()),
            case[2].as_bool().unwrap(),
            "{}",
            case[0]
        );
    }
}

#[test]
fn routine_write_matches_python() {
    for case in cases("routine") {
        let got = apps::routine_write(
            case[0].as_str().unwrap(),
            case[1].as_str().unwrap(),
            Some(&case[2]),
        );
        assert_eq!(got, case[3].as_bool().unwrap(), "{case}");
    }
}

#[test]
fn describing_an_act_matches_python() {
    for case in cases("describe") {
        let (name, args) = (case[0].as_str().unwrap(), map(&case[1]));
        assert_eq!(
            apps::describe_for_confirmation(name, &args),
            case[2].as_str().unwrap(),
            "{case}"
        );
        assert_eq!(
            apps::describe_whole(name, &args, 500).as_deref(),
            case[3].as_str(),
            "{case}"
        );
        assert_eq!(
            apps::describe_whole(name, &args, apps::WHOLE_ASK_CHARS).as_deref(),
            case[4].as_str(),
            "{case}"
        );
    }
}

#[test]
fn results_reach_the_model_as_python_hands_them() {
    for case in cases("results") {
        let got = Value::Object(apps::result_for_model(&map(&case[0])));
        assert_eq!(
            got,
            case[1],
            "{}",
            case[0].to_string().chars().take(300).collect::<String>()
        );
        // and in the same key order
        assert_eq!(
            got.as_object().unwrap().keys().collect::<Vec<_>>(),
            case[1].as_object().unwrap().keys().collect::<Vec<_>>()
        );
    }
}

#[test]
fn schemas_and_function_tools_match_python() {
    for case in cases("schemas") {
        let app = case[0].as_str().unwrap();
        let tool = &case[1];
        let slim = apps::slim_schema(&case[2]);
        assert_eq!(
            serde_json::to_string(&slim).unwrap(),
            serde_json::to_string(&case[3]).unwrap(),
            "{}",
            case[2]
        );
        let f = apps::function_tool(app, tool);
        assert_eq!(
            serde_json::to_string(&f).unwrap(),
            serde_json::to_string(&case[4]).unwrap(),
            "{tool}"
        );
    }
}

#[test]
fn open_asked_matches_python() {
    for case in cases("asked") {
        let got = apps::open_asked(&map(&case[0]));
        let want: Vec<String> = case[1]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(got, want, "{}", case[0]);
    }
}

#[test]
fn policy_and_scopes_match_python() {
    for case in cases("policies") {
        let env: apps::Environ = [(
            "EVE_APPS_POLICY".to_string(),
            case[0].as_str().unwrap().to_string(),
        )]
        .into();
        let pol = apps::policy(Some(&env));
        let got: Vec<Value> = pol.iter().map(|(k, v)| serde_json::json!([k, v])).collect();
        assert_eq!(Value::Array(got), case[1], "{}", case[0]);
        assert_eq!(
            serde_json::json!(apps::google_scopes(&pol)),
            case[2],
            "{}",
            case[0]
        );
    }
}

#[test]
fn clip_matches_python() {
    for case in cases("clips") {
        let got = apps::clip(
            case[0].as_str().unwrap(),
            case[1].as_u64().unwrap() as usize,
        );
        assert_eq!(got, case[2].as_str().unwrap(), "{case}");
    }
}

#[test]
fn sse_matches_python() {
    for case in cases("sse") {
        let got: Vec<Value> = apps::sse_messages(case[0].as_str().unwrap())
            .into_iter()
            .map(Value::Object)
            .collect();
        assert_eq!(Value::Array(got), case[1], "{}", case[0]);
    }
}

#[test]
fn configured_matches_python() {
    for case in cases("configured") {
        let env: apps::Environ = map(&case[0])
            .into_iter()
            .map(|(k, v)| (k, v.as_str().unwrap().to_string()))
            .collect();
        let c = apps::configured(Some(&env));
        assert_eq!(c.enabled, case[1].as_bool().unwrap(), "{}", case[0]);
        assert_eq!(c.timeout_secs, case[2].as_f64().unwrap(), "{}", case[0]);
        assert_eq!(
            c.config_dir.to_string_lossy(),
            case[3].as_str().unwrap(),
            "{}",
            case[0]
        );
    }
}

#[test]
fn private_files_are_written_byte_for_byte_as_python_writes_them() {
    let dir = tempfile::tempdir().unwrap();
    for case in cases("files") {
        let path = dir.path().join("t.json");
        apps::write_private_json(&path, &map(&case[0])).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            case[1].as_str().unwrap()
        );
    }
}

#[test]
fn github_repos_match_python() {
    let repos: Vec<Repo> = cases("repos")
        .iter()
        .map(|r| {
            Repo::new(
                r["name"].as_str().unwrap(),
                r["description"].as_str().unwrap(),
            )
        })
        .collect();
    let names = g::keyword_names(&repos);
    let want: Vec<String> = cases("keyword")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(names.iter().cloned().collect::<Vec<_>>(), want);
    for case in cases("mentions") {
        assert_eq!(
            g::names_repo(case[0].as_str().unwrap(), &names),
            case[1].as_bool().unwrap(),
            "{case}"
        );
    }
    let lines = cases("line");
    assert_eq!(g::line(&repos), lines[0].as_str().unwrap());
    assert_eq!(g::line(&repos[..3]), lines[1].as_str().unwrap());
    assert_eq!(g::line(&[]), lines[2].as_str().unwrap());
}
