//! Port of bridge/tests/test_github_repos.py: the owner's repositories fetched through the GitHub app's tools,
//! kept a day, shown to the brain in one line, and their unusual names opening GitHub. Every name here is made
//! up. (Its own binary: the one test here that starts a bridge sets the process-wide REPO_NAMES.)

mod common;

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use common::*;
use eve_apps_mcp as apps;
use eve_apps_mcp::github_repos::{self as g, Repo};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

fn repos() -> Vec<Repo> {
    vec![
        Repo::new(
            "zorbcar",
            "autonomous car from a kit, step by step to self-driving and more words",
        ),
        Repo::new("remix", "a music thing"),
        Repo::new("qbt", ""),
        Repo::new("plantpal-x", "plant watering"),
    ]
}

fn repos_json() -> Value {
    Value::Array(
        repos()
            .iter()
            .map(|r| json!({"name": r.name, "description": r.description}))
            .collect(),
    )
}

type Seen = Arc<Mutex<Vec<(String, Map<String, Value>)>>>;

/// `caller(repos, login, ok)`: GitHub's two tools in AppsBridge::execute's shape, recording each call.
fn caller(
    repos: Value,
    login: &str,
    ok: bool,
) -> (
    impl Fn(&str, &Map<String, Value>) -> Value + Send + Sync,
    Seen,
) {
    let calls: Seen = Arc::new(Mutex::new(Vec::new()));
    let seen = calls.clone();
    let login = login.to_string();
    let call = move |name: &str, args: &Map<String, Value>| {
        seen.lock().push((name.to_string(), args.clone()));
        if name == "github__get_me" {
            return json!({"ok": ok, "result": json!({"login": login}).to_string()});
        }
        let n = repos.as_array().map(Vec::len).unwrap_or(0);
        json!({"ok": ok, "result": json!({"total_count": n, "items": repos}).to_string()})
    };
    (call, calls)
}

#[tokio::test]
async fn test_fetch_reads_the_owners_login_then_their_repositories() {
    let (call, calls) = caller(
        json!([{"full_name": "someone/zorbcar", "description": "car  kit\n"}, {"name": "qbt"}, "junk"]),
        "someone",
        true,
    );
    assert_eq!(
        g::fetch(&call).await.unwrap(),
        Some(vec![Repo::new("zorbcar", "car kit"), Repo::new("qbt", "")])
    );
    let second = calls.lock()[1].clone();
    assert_eq!(second.0, "github__search_repositories");
    assert_eq!(
        Value::Object(second.1),
        json!({"query": "user:someone fork:true sort:updated", "perPage": g::FETCH_MAX})
    );
    // a login that is not one is not searched
    assert_eq!(
        g::fetch(&caller(json!([]), "bad login; rm", true).0)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        g::fetch(&caller(json!([]), "someone", false).0)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn test_fetch_reads_a_structured_result_and_the_repositories_key() {
    let call = |name: &str, _args: &Map<String, Value>| {
        if name == "github__get_me" {
            return json!({"ok": true, "result": {"login": "someone"}});
        }
        json!({"ok": true, "result": {"repositories": [{"name": " zorbcar ", "description": null}]}})
    };
    assert_eq!(
        g::fetch(&call).await.unwrap(),
        Some(vec![Repo::new("zorbcar", "")])
    );
}

#[tokio::test]
async fn test_refresh_keeps_a_day_and_keeps_the_old_list_when_github_fails() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(g::FILE);
    let (call, calls) = caller(repos_json(), "someone", true);
    assert_eq!(g::refresh(&path, &call, &|| 1000.0).await.unwrap(), repos());
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let n = calls.lock().len();
    g::refresh(&path, &call, &|| 1000.0 + g::REFRESH_SECS - 1.0)
        .await
        .unwrap(); // fresh: no request
    assert_eq!(calls.lock().len(), n);
    let (broken, _) = caller(json!([]), "someone", false);
    let kept = g::refresh(&path, &broken, &|| 1000.0 + g::REFRESH_SECS + 1.0)
        .await
        .unwrap();
    assert_eq!(
        kept.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        vec!["zorbcar", "remix", "qbt", "plantpal-x"]
    );
    // what Python's json.dump(indent=2) writes
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.starts_with(
            "{\n  \"fetched\": 1000.0,\n  \"repos\": [\n    {\n      \"name\": \"zorbcar\",\n"
        ),
        "{text}"
    );
    assert!(text.ends_with("\n  ]\n}\n"));
}

#[tokio::test]
async fn test_a_failing_call_keeps_the_old_list_and_logs_the_type_only() {
    struct Raises;
    #[async_trait::async_trait]
    impl g::Call for Raises {
        async fn call(
            &self,
            _name: &str,
            _args: &Map<String, Value>,
        ) -> Result<Value, apps::CallError> {
            Err(apps::CallError::new("ConnectError", "secret words"))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(g::FILE);
    let (_guard, lines) = capture();
    assert!(g::refresh(&path, &Raises, &|| 1.0)
        .await
        .unwrap()
        .is_empty());
    let text = lines.text();
    assert!(
        text.contains("github repos: not refreshed (ConnectError)")
            && !text.contains("secret words"),
        "{text}"
    );
    assert!(text.contains("github repos: kept 0 (refresh failed)"));
    assert!(!path.exists());
}

#[test]
fn test_load_reads_nothing_from_a_broken_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(g::FILE);
    assert_eq!(g::load(&path), (Vec::new(), 0.0));
    std::fs::write(
        &path,
        r#"{"fetched": "soon", "repos": [{"name": "zorbcar"}]}"#,
    )
    .unwrap();
    assert_eq!(g::load(&path), (Vec::new(), 0.0));
    std::fs::write(
        &path,
        r#"{"fetched": 5, "repos": [{"name": "zorbcar"}, {"name": ""}, 3]}"#,
    )
    .unwrap();
    assert_eq!(g::load(&path), (vec![Repo::new("zorbcar", "")], 5.0));
}

#[test]
fn test_the_line_names_each_repository_with_what_it_is_clipped() {
    let line = g::line(&repos());
    let first = &repos()[0].description;
    let desc = format!(
        "{}…",
        first
            .chars()
            .take(g::DESC_CLIP - 1)
            .collect::<String>()
            .trim_end()
    );
    assert!(line.contains(&format!("zorbcar ({desc})")) && desc.chars().count() == g::DESC_CLIP);
    assert!(
        line.contains("; qbt;")
            && line.contains("\"my car repo\"")
            && line.ends_with("plantpal-x (plant watering).")
    );
    assert_eq!(g::line(&[]), "");
    let many: Vec<Repo> = (0..g::SHOWN + 5)
        .map(|i| Repo::new(&format!("r{i}"), ""))
        .collect();
    let text = g::line(&many);
    assert!(
        text.contains(&format!("r{}", g::SHOWN - 1)) && !text.contains(&format!("r{}.", g::SHOWN))
    );
}

#[test]
fn test_only_unusual_names_open_github_as_words() {
    let names = g::keyword_names(&repos());
    assert!(names.contains("zorbcar") && names.contains("plantpal-x"));
    assert!(!names.contains("remix") && !names.contains("qbt")); // a word, and too short
    assert!(g::names_repo("any new commits on zorbcar?", &names));
    assert!(g::names_repo("is plantpal-x green", &names));
    assert!(g::names_repo("Check the last commit on zorbcar.", &names)); // the full stop is not the name
    assert!(g::names_repo("look at plantpal-x...", &names));
    assert!(
        !g::names_repo("can you remix this song", &names) && !g::names_repo("zorbcars", &names)
    );
}

#[test]
fn test_a_dictionary_word_is_never_a_keyword() {
    let dictionary = ["leap".to_string()].into_iter().collect();
    let names = g::keyword_names_in(
        &[
            Repo::new("leap", ""),
            Repo::new("Zorbcar", ""),
            Repo::new("eve", ""),
        ],
        &dictionary,
    );
    assert_eq!(
        names.into_iter().collect::<Vec<_>>(),
        vec!["zorbcar".to_string()]
    );
}

#[tokio::test]
async fn test_the_bridge_loads_them_and_the_brain_and_the_router_see_them() {
    let dir = tempfile::tempdir().unwrap();
    // kept and fresh
    std::fs::write(
        dir.path().join(g::FILE),
        json!({"fetched": 9e12, "repos": repos_json()}).to_string(),
    )
    .unwrap();
    apps::set_repo_names(Default::default());
    let (b, _) = bridge(dir.path(), &[], FakeResult::Default, &[]).await;
    assert_eq!(
        b.repos.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        vec!["zorbcar", "remix", "qbt", "plantpal-x"]
    );
    assert!(b.instructions().contains("zorbcar (autonomous car"));
    assert!(apps::names_app("github", "what's failing on zorbcar"));
    assert!(!apps::names_app("github", "can you remix this"));
    // GitHub down: no line, no names
    apps::set_repo_names(Default::default());
    let (down, _) = bridge(dir.path(), &["github"], FakeResult::Default, &[]).await;
    assert!(
        down.repos.is_empty()
            && !down.instructions().contains("zorbcar")
            && apps::repo_names().is_empty()
    );
}
