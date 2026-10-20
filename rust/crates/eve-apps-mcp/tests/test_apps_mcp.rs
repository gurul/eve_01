//! Port of bridge/tests/test_apps_mcp.py (the apps_mcp half; the Google REST half is eve-google-rest's
//! tests/test_google_rest.rs). Every HTTP exchange goes through a scripted transport, so the real request code
//! runs and nothing leaves the Mac.

mod common;

use std::collections::HashSet;
use std::sync::Arc;

use common::*;
use eve_apps_mcp as apps;
use eve_apps_mcp::{AppsBridge, McpClient};
use indexmap::IndexMap;
use serde_json::{json, Map, Value};

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn set(items: &[&str]) -> HashSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn env(pairs: &[(&str, &str)]) -> apps::Environ {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

// ---- pure: reading or writing, schemas, decisions ------------------------------------------------------

#[test]
fn test_a_tool_reads_only_when_its_hint_and_its_name_agree() {
    let cases = [
        (
            json!({"name": "search_threads", "annotations": {"readOnlyHint": true}}),
            true,
        ),
        (
            json!({"name": "trash_thread", "annotations": {"readOnlyHint": true}}),
            false,
        ), // a hint never makes a trash a read
        (
            json!({"name": "create_event", "annotations": {"readOnlyHint": false}}),
            false,
        ),
        (
            json!({"name": "list_events", "annotations": {"readOnlyHint": false}}),
            false,
        ), // the server says it writes
        (
            json!({"name": "get_draft", "annotations": {"readOnlyHint": true}}),
            true,
        ), // draft as a noun
        (json!({"name": "draft_reply"}), false), // draft as the verb
        (
            json!({"name": "lookup_weather", "annotations": {"readOnlyHint": true}}),
            true,
        ),
        (json!({"name": "lookup_weather"}), false), // no hint is not a read
        (json!({"name": "find_and_replace"}), false), // Lean AppsPolicy P1
        (json!({"name": "push_files"}), false),
        (json!({"name": "issue_write"}), false),
        (
            json!({"name": "ui_get", "annotations": {"readOnlyHint": true}}),
            true,
        ),
        (json!({"name": "frobnicate"}), false), // no reading verb: asks
    ];
    for (tool, reads) in cases {
        assert_eq!(apps::tool_reads(&tool), reads, "{tool}");
    }
}

#[test]
fn test_the_policy_has_the_owners_defaults_and_an_env_override() {
    // Private routine writes run; consequential acts still ask. Contacts and Maps only look.
    let defaults: Vec<(String, String)> = apps::policy(Some(&env(&[]))).into_iter().collect();
    let want: Vec<(String, String)> = [
        ("gmail", "auto"),
        ("calendar", "auto"),
        ("drive", "auto"),
        ("contacts", "read"),
        ("github", "auto"),
        ("maps", "read"),
    ]
    .iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect();
    assert_eq!(defaults, want);
    let pol = apps::policy(Some(&env(&[(
        "EVE_APPS_POLICY",
        "github=write, drive=read, maps=never, =x",
    )])));
    assert!(pol["github"] == "write" && pol["drive"] == "read" && pol["maps"] == "read");
    // "never" ignored
}

#[test]
fn test_scopes_follow_the_policy() {
    let g = "https://www.googleapis.com/auth/";
    let contacts = [
        format!("{g}contacts.readonly"),
        format!("{g}contacts.other.readonly"),
    ];
    // sending is gmail.send, which cannot read, delete or label anything; never gmail.modify
    let mut want: Vec<String> = [
        "gmail.readonly",
        "gmail.send",
        "calendar",
        "drive.readonly",
        "drive.file",
    ]
    .iter()
    .map(|s| format!("{g}{s}"))
    .collect();
    want.extend(contacts.iter().cloned());
    assert_eq!(apps::google_scopes(&apps::policy(Some(&env(&[])))), want);
    let strict: apps::Policy = [("gmail", "read"), ("calendar", "read"), ("drive", "read")]
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
    let mut want: Vec<String> = ["gmail.readonly", "calendar.readonly", "drive.readonly"]
        .iter()
        .map(|s| format!("{g}{s}"))
        .collect();
    want.extend(contacts.iter().cloned());
    assert_eq!(apps::google_scopes(&strict), want);
}

#[test]
fn test_a_schema_is_slimmed_for_the_model() {
    let raw = json!({"type": "object", "title": "Req", "x-google-thing": 1, "properties": {
        "q": {"type": "string", "description": "word ".repeat(100), "x-google-enum-descriptions": ["a"]},
        "old": {"type": "string", "deprecated": true},
        "nested": {"type": "array", "items": {"type": "object", "title": "T", "properties": {"a": {"type": "integer"}}}}}});
    let slim = apps::slim_schema(&raw);
    let keys: HashSet<&str> = slim
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, HashSet::from(["type", "properties"]));
    let props: HashSet<&str> = slim["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(props, HashSet::from(["q", "nested"]));
    assert_eq!(
        slim["properties"]["q"]["description"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        apps::PROP_CLIP
    );
    assert!(slim["properties"]["q"]
        .get("x-google-enum-descriptions")
        .is_none());
    assert_eq!(
        slim["properties"]["nested"]["items"],
        json!({"type": "object", "properties": {"a": {"type": "integer"}}})
    );
    let f = apps::function_tool(
        "calendar",
        &json!({"name": "list_events", "description": "Events.", "inputSchema": raw}),
    );
    assert!(f["name"] == "calendar__list_events" && f["strict"] == json!(false));
    assert!(f["description"] == "[Google Calendar] Events." && f["parameters"]["type"] == "object");
    let bare = apps::function_tool("maps", &json!({"name": "x"}));
    assert_eq!(
        bare["parameters"],
        json!({"type": "object", "properties": {}})
    );
}

#[test]
fn test_the_confirmation_line_is_plain_and_clipped() {
    let line = apps::describe_for_confirmation(
        "github__create_issue",
        &obj(json!({"repo": "eve", "title": "t".repeat(200)})),
    );
    assert!(
        line.starts_with("GitHub: create issue with repo: eve, title: ttt") && line.ends_with('?')
    );
    assert!(line.chars().count() <= apps::LINE_CLIP);
    assert_eq!(
        apps::describe_for_confirmation("drive__copy_file", &Map::new()),
        "Google Drive: copy file?"
    );
}

#[test]
fn test_a_result_reaches_the_model_structured_or_as_text_and_clipped() {
    assert_eq!(
        Value::Object(apps::result_for_model(&obj(
            json!({"structuredContent": {"a": 1}})
        ))),
        json!({"ok": true, "result": {"a": 1}})
    );
    let text = apps::result_for_model(&obj(json!({"content": [{"type": "text", "text": "one"},
        {"type": "image", "data": "x"}, {"type": "text", "text": "two"}]})));
    assert_eq!(
        Value::Object(text),
        json!({"ok": true, "result": "one\ntwo", "not_shown": ["image"]})
    );
    let err = apps::result_for_model(&obj(
        json!({"isError": true, "content": [{"type": "text", "text": "API disabled"}]}),
    ));
    assert!(err["ok"] == json!(false) && err["reason"] == "API disabled");
    let big = apps::result_for_model(&obj(
        json!({"content": [{"type": "text", "text": "x".repeat(apps::RESULT_CLIP + 10)}]}),
    ));
    assert!(
        big["clipped"] == json!(true)
            && big["result"].as_str().unwrap().chars().count() == apps::RESULT_CLIP + 1
    );
}

// ---- the bridge over fake servers ------------------------------------------------------------------------

#[tokio::test]
async fn test_start_offers_each_apps_tools_under_its_policy() {
    let dir = tmp();
    let (b, _) = bridge(dir.path(), &[], FakeResult::Default, &[("gmail", "read")]).await;
    let names: HashSet<String> = b
        .tools()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains("gmail__search_threads") && !names.contains("gmail__trash_thread")); // read only: not offered
    assert!(
        names.contains("contacts__search_contacts") && !names.contains("contacts__delete_contact")
    ); // contacts only look
    assert!(set(&[
        "calendar__create_event",
        "drive__create_file",
        "github__create_issue"
    ])
    .is_subset(&names));
    let paths: IndexMap<String, String> = [
        ("gmail", "rest"),
        ("calendar", "rest"),
        ("drive", "rest"),
        ("contacts", "rest"),
        ("github", "mcp"),
        ("maps", "mcp"),
    ]
    .iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect();
    assert_eq!(b.paths, paths);
    let mut want: std::collections::BTreeSet<String> = names.into_iter().collect();
    want.insert(apps::OPEN_TOOL.to_string());
    assert_eq!(b.names(), want);
}

#[tokio::test]
async fn test_an_app_that_fails_is_left_out_and_the_rest_come_up() {
    let dir = tmp();
    let (_guard, lines) = capture();
    let (b, _) = bridge(dir.path(), &["github"], FakeResult::Default, &[]).await;
    assert!(!b.apps().contains(&"github".to_string()) && b.apps().contains(&"gmail".to_string()));
    assert!(
        lines.text().contains("GitHub is not available"),
        "{}",
        lines.text()
    );
}

#[tokio::test]
async fn test_decisions_follow_the_policy() {
    let dir = tmp();
    let (b, _) = bridge(dir.path(), &[], FakeResult::Default, &[]).await;
    assert_eq!(b.decide("gmail__search_threads", None).action, "run");
    assert_eq!(
        b.decide("calendar__create_event", Some(&json!({"summary": "Focus"})))
            .action,
        "run"
    );
    assert_eq!(b.decide("calendar__create_event", None).action, "ask"); // args must establish no delivery
    assert_eq!(
        b.decide(
            "drive__create_file",
            Some(&json!({"title": "Private notes"}))
        )
        .action,
        "run"
    );
    assert_eq!(b.decide("github__create_issue", None).action, "ask");
    assert_eq!(b.decide("github__list_issues", None).action, "run");
    assert_eq!(b.decide("gmail__trash_thread", None).action, "ask"); // Gmail asks by default
    assert_eq!(b.decide("contacts__delete_contact", None).action, "refuse"); // never offered, never run
    let (strict, _) = bridge(
        dir.path(),
        &[],
        FakeResult::Default,
        &[("calendar", "read"), ("gmail", "read")],
    )
    .await;
    assert!(!strict.names().contains("calendar__create_event"));
    assert_eq!(strict.decide("gmail__trash_thread", None).action, "refuse");
}

fn enum_of(tool: &Value) -> Vec<String> {
    tool["parameters"]["properties"]["apps"]["items"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn test_open_app_loads_an_app_for_the_turn() {
    let dir = tmp();
    let (b, _) = bridge(dir.path(), &[], FakeResult::Default, &[]).await;
    let mut opened: HashSet<String> = HashSet::new();
    let first = b.tools_for(&opened);
    assert_eq!(
        first
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![apps::OPEN_TOOL]
    );
    let apps_prop = &first[0]["parameters"]["properties"]["apps"];
    assert!(apps_prop["type"] == "array" && apps_prop["minItems"] == 1);
    let e: HashSet<String> = enum_of(&first[0]).into_iter().collect();
    assert_eq!(e, b.apps().into_iter().collect::<HashSet<_>>());
    assert_eq!(first[0]["parameters"]["required"], json!(["apps"]));
    let out = b.open(&obj(json!({"app": "Calendar"})), &mut opened); // the older single-app form
    assert!(
        out["ok"] == json!(true)
            && out["opened"] == json!(["calendar"])
            && opened == set(&["calendar"])
    );
    assert!(out["tools"]
        .as_array()
        .unwrap()
        .contains(&json!("calendar__create_event")));
    let names: Vec<String> = b
        .tools_for(&opened)
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"calendar__list_events".to_string()) && names[0] == apps::OPEN_TOOL);
    assert!(!enum_of(&b.tools_for(&opened)[0]).contains(&"calendar".to_string()));
    assert_eq!(
        b.open(&obj(json!({"app": "slack"})), &mut opened)["ok"],
        json!(false)
    );
    let all: HashSet<String> = b.apps().into_iter().collect();
    assert_ne!(b.tools_for(&all)[0]["name"], apps::OPEN_TOOL); // all open: no open_app
}

#[tokio::test]
async fn test_open_app_opens_several_apps_in_one_call_and_reports_the_unknown_ones() {
    let dir = tmp();
    let (b, _) = bridge(dir.path(), &[], FakeResult::Default, &[]).await;
    let mut opened: HashSet<String> = HashSet::new();
    let out = b.open(
        &obj(json!({"apps": ["calendar", "Maps", "slack", "calendar"]})),
        &mut opened,
    );
    assert!(
        out["ok"] == json!(true)
            && out["opened"] == json!(["calendar", "maps"])
            && opened == set(&["calendar", "maps"])
    );
    let tools: HashSet<String> = out["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        tools,
        set(&[
            "calendar__list_events",
            "calendar__create_event",
            "maps__compute_routes"
        ])
    );
    assert!(
        out["not_connected"] == json!(["slack"])
            && out["note"]
                .as_str()
                .unwrap()
                .contains("slack is not connected")
    );
    assert_eq!(
        b.open(&obj(json!({"apps": "github"})), &mut opened)["opened"],
        json!(["github"])
    ); // a lone string is one app
    let nothing = b.open(&obj(json!({"apps": []})), &mut opened);
    assert!(
        nothing["ok"] == json!(false) && nothing["reason"].as_str().unwrap().contains("connected:")
    );
}

#[tokio::test]
async fn test_a_message_opens_the_apps_it_names() {
    let dir = tmp();
    let (b, _) = bridge(dir.path(), &[], FakeResult::Default, &[]).await;
    let cases: &[(&str, &[&str])] = &[
        ("any mail from Sam?", &["gmail"]),
        ("what's on my calendar tomorrow", &["calendar"]),
        ("find the budget sheet in my drive", &["drive"]),
        ("open PRs on eve", &["github"]),
        ("how long to SFO by car", &["maps"]),
        ("what's the weather like", &["maps"]),
        // compound requests open every app they need at once: a miss is a whole extra round
        (
            "look for my next class on my calendar and give me a maps link",
            &["calendar", "maps"],
        ),
        ("where's my next lecture", &["calendar", "maps"]),
        (
            "where\u{2019}s my midterm and how long to get there",
            &["calendar", "maps"],
        ),
        (
            "when is my flight and what's the traffic like",
            &["calendar", "maps"],
        ),
        ("did CI pass on eve", &["github"]),
        ("did the build pass on the main branch", &["github"]),
        ("any receipts in my inbox", &["gmail"]),
        (
            "did Sam's email get a reply, and add it to the spreadsheet",
            &["gmail", "drive"],
        ),
        ("what's next", &["calendar"]),
        ("how does my day look", &["calendar"]),
        ("am I free at 3", &["calendar"]),
        ("rsvp yes to the seminar", &["calendar"]),
        ("office hours this week", &["calendar"]),
        ("coffee near me", &["maps"]),
        ("drive to the office", &["maps"]), // driving there, not Google Drive
        ("open the pdf in my drive", &["drive"]),
        // negative controls: nothing opens on a plain sentence, a day word or "build me"
        ("I finally finished", &[]),
        ("tell me a joke", &[]),
        ("what's 2+2", &[]),
        ("remind me tomorrow", &[]),
        ("what about today", &[]),
        ("build me an app for groceries", &[]),
        ("what is my email address", &["gmail"]), // an email address is not a place
        // an issue is GitHub's only when nothing else is its home (review, 2026-10-05)
        ("any open issues on eve?", &["github"]),
        ("file an issue on the eve repo", &["github"]),
        ("did my PR merge", &["github"]),
        ("did the actions run fail on main", &["github"]),
        ("add a linear issue for the login bug", &[]),
        ("what are the open issues in linear", &[]),
        ("any issues with my order on uber eats", &[]),
        ("what actions do I need to take today", &[]),
        ("merge these two notes", &[]),
    ];
    for (text, want) in cases {
        assert_eq!(b.guess(text), set(want), "{text}");
    }
}

#[test]
fn test_the_contacts_words_find_someone_elses_address() {
    // the contacts pattern's lookbehinds and lookahead, which the regex crate alone cannot run
    for (text, want) in [
        ("what's Sam's number", true),
        ("email sam that I'm late", true),
        ("can you email ana about lunch", true),
        ("email me the notes", false),
        ("what is my email address", false),
        ("his email address please", true),
        ("Please Email Bob the doc", true),
    ] {
        assert_eq!(apps::names_app("contacts", text), want, "{text}");
    }
}

#[tokio::test]
async fn test_owns_and_ensure_open_cover_every_up_apps_names() {
    let dir = tmp();
    let (b, _) = bridge(dir.path(), &["drive"], FakeResult::Default, &[]).await;
    assert!(b.owns("calendar__list_events") && b.owns("calendar__anything_at_all"));
    assert!(b.owns(apps::OPEN_TOOL));
    assert!(!b.owns("drive__search_files")); // drive is not up
    assert!(!b.owns("slack__send") && !b.owns("web_search") && !b.owns("calendar__"));
    let mut opened: HashSet<String> = HashSet::new();
    b.ensure_open("maps__compute_routes", &mut opened);
    b.ensure_open("drive__search_files", &mut opened);
    b.ensure_open("web_search", &mut opened);
    assert_eq!(opened, set(&["maps"]));
    let none_up = AppsBridge::new(cfg(dir.path(), &[], &[])).with_tokens(IndexMap::new());
    assert!(!none_up.owns(apps::OPEN_TOOL) && !none_up.owns("calendar__list_events"));
}

#[tokio::test]
async fn test_a_tool_an_app_lacks_is_refused_with_its_real_tools() {
    let dir = tmp();
    let (b, _) = bridge(dir.path(), &[], FakeResult::Default, &[("gmail", "read")]).await;
    let made_up = b.decide("calendar__find_next_class", None);
    assert_eq!(made_up.action, "refuse");
    assert!(
        made_up.why.contains("calendar__list_events")
            && made_up.why.contains("calendar__create_event")
    );
    assert!(made_up.why.contains("Google Calendar"));
    let read_only = b.decide("gmail__trash_thread", None); // a write a read policy never offered
    assert!(
        read_only.action == "refuse"
            && read_only.why.contains("read only")
            && read_only.why.contains("gmail__search_threads")
    );
    assert!(!read_only
        .why
        .split("Its tools:")
        .nth(1)
        .unwrap()
        .contains("gmail__trash_thread"));
    assert_eq!(b.decide("slack__send", None).action, "refuse"); // not an app here at all
}

#[tokio::test]
async fn test_execute_calls_the_servers_own_tool_and_never_raises() {
    let dir = tmp();
    let (b, calls) = bridge(dir.path(), &[], FakeResult::Default, &[]).await;
    assert_eq!(
        Value::Object(
            b.execute("calendar__list_events", &obj(json!({"pageSize": 3})))
                .await
        ),
        json!({"ok": true, "result": {"ok": "list_events"}})
    );
    let last = calls.lock().last().cloned().unwrap();
    assert_eq!(
        last,
        (
            "calendar".to_string(),
            "list_events".to_string(),
            obj(json!({"pageSize": 3}))
        )
    );
    assert_eq!(
        b.execute("slack__send", &Map::new()).await["ok"],
        json!(false)
    );
    let (broken, _) = bridge(
        dir.path(),
        &[],
        FakeResult::Raise(apps::CallError::new("RuntimeError", "boom with mail text")),
        &[],
    )
    .await;
    let out = broken
        .execute("gmail__search_threads", &obj(json!({"query": "from:sam"})))
        .await;
    assert_eq!(
        Value::Object(out),
        json!({"ok": false, "reason": "RuntimeError: boom with mail text"})
    );
}

#[tokio::test]
async fn test_instructions_name_the_apps_up_and_their_rules() {
    let dir = tmp();
    let (b, _) = bridge(dir.path(), &[], FakeResult::Default, &[]).await;
    let text = b.instructions();
    assert!(
        text.contains("is opened first")
            && text.contains("Google Contacts (contacts__…): read only")
    );
    assert!(text.contains("Gmail (gmail__…): routine private changes run"));
    assert!(text.contains("sees each one whole") && text.contains("never guess an address"));
    // the owner's rule: no tool name in the block, since which tools a turn offers changes turn to turn (2026-10-05)
    assert!(!text.contains("open_app") && !text.contains("__list") && !text.contains("web_search"));
    assert!(text.contains("Google Calendar (calendar__…): routine private changes run"));
    assert!(text.contains("GitHub (github__…): routine private changes run"));
    assert!(text.contains("sending, deleting, sharing, publishing and purchases are asked by code"));
    let empty = AppsBridge::new(cfg(dir.path(), &[], &[])).with_tokens(IndexMap::new());
    assert!(empty.instructions().is_empty() && empty.available().is_empty());
}

#[tokio::test]
async fn test_instructions_steer_the_brain_to_the_apps_and_to_chain_them() {
    let dir = tmp();
    let (b, _) = bridge(dir.path(), &[], FakeResult::Default, &[]).await;
    let text = b.instructions();
    assert!(text.contains("rather than a task on the Mac's screen or a web search"));
    assert!(text.contains("every app the request needs in one call"));
    assert!(text.contains("Chain apps in the same turn without asking"));
    assert!(
        text.contains("the calendar's event list from now")
            && text.contains("mapsUrl and directionsUrl")
    );
    assert!(
        text.contains("needs no maps call")
            && text.contains("travel time, nearby places and weather")
    );
    assert!(text.contains("Keep the links that results carry"));
    assert!(text.chars().count() < 2600); // it rides every turn's prompt
                                          // it writes no tool name at all, only app prefixes like gmail__…
    let rx = regex::Regex::new(r"\b[a-z]+__[a-z_]+\b").unwrap();
    let found: HashSet<&str> = rx
        .find_iter(&text)
        .map(|m| m.as_str())
        .filter(|m| *m != "app__tool")
        .collect();
    assert!(found.is_empty(), "{found:?}");
    // an app that is not up is not written about
    let (no_cal, _) = bridge(dir.path(), &["calendar", "maps"], FakeResult::Default, &[]).await;
    let other = no_cal.instructions();
    assert!(
        !other.contains("calendar__") && !other.contains("maps__") && other.contains("Chain apps")
    );
}

#[test]
fn test_an_app_without_its_sign_in_is_not_available() {
    let dir = tmp();
    let b = AppsBridge::new(cfg(dir.path(), &[("EVE_GITHUB_MCP_TOKEN", "x")], &[]));
    assert_eq!(b.available(), vec!["github".to_string()]); // no Google token, no Maps key
}

#[test]
fn test_configured_reads_the_switch() {
    let dir = tmp();
    assert!(!apps::configured(Some(&env(&[]))).enabled);
    let on = apps::configured(Some(&env(&[
        ("EVE_APPS", "1"),
        ("EVE_APPS_TIMEOUT_SECS", "-3"),
        ("EVE_APPS_DIR", dir.path().to_str().unwrap()),
    ])));
    assert!(on.enabled && on.timeout_secs == apps::DEFAULT_TIMEOUT_SECS);
    assert_eq!(on.google_token_path().parent().unwrap(), dir.path());
    assert_eq!(
        apps::configured(Some(&env(&[("EVE_APPS_TIMEOUT_SECS", "12.5")]))).timeout_secs,
        12.5
    );
    assert_eq!(
        apps::configured(Some(&env(&[("EVE_APPS_TIMEOUT_SECS", "soon")]))).timeout_secs,
        60.0
    );
    assert!(apps::configured(Some(&env(&[("EVE_APPS", " Yes ")]))).enabled);
    assert_eq!(
        apps::configured(Some(&env(&[]))).config_dir,
        std::path::PathBuf::from(apps::CONFIG_DIR)
    );
}

// ---- the MCP client over a mock transport --------------------------------------------------------------

fn tok(t: &'static str) -> impl Fn() -> Option<String> + Send + Sync + 'static {
    move || Some(t.to_string())
}

fn no_tok() -> impl Fn() -> Option<String> + Send + Sync + 'static {
    || None
}

#[tokio::test]
async fn test_the_client_initializes_keeps_the_session_and_reads_sse() {
    let http = Mock::new(|req| {
        let msg = body_json(req);
        if msg["method"] == "initialize" {
            return Ok((
                200,
                vec![
                    ("content-type".into(), "application/json".into()),
                    ("Mcp-Session-Id".into(), "s1".into()),
                ],
                serde_json::to_vec(&json!({"jsonrpc": "2.0", "id": msg["id"], "result": {}}))
                    .unwrap(),
            ));
        }
        if msg.get("id").is_none() {
            return empty_reply(202);
        }
        assert_eq!(header(req, "mcp-session-id"), Some("s1"));
        let result = if msg["params"].get("cursor").is_none() {
            json!({"tools": [{"name": "a"}], "nextCursor": "c2"})
        } else {
            json!({"tools": [{"name": "b"}]})
        };
        let body = format!(
            ": ping\n\nevent: message\ndata: {}\n\n",
            json!({"jsonrpc": "2.0", "id": msg["id"], "result": result})
        );
        Ok((
            200,
            vec![("content-type".into(), "text/event-stream".into())],
            body.into_bytes(),
        ))
    });
    let client = McpClient::with_transport(
        "https://x/mcp",
        tok("tok"),
        &[("X-MCP-Toolsets", "repos")],
        http.clone(),
        "",
    );
    let names: Vec<Value> = client
        .list_tools()
        .await
        .unwrap()
        .iter()
        .map(|t| t["name"].clone())
        .collect();
    assert_eq!(names, vec![json!("a"), json!("b")]);
    let seen = http.seen();
    let methods: Vec<Value> = seen
        .iter()
        .map(|r| body_json(r)["method"].clone())
        .collect();
    assert_eq!(
        methods,
        vec![
            json!("initialize"),
            json!("notifications/initialized"),
            json!("tools/list"),
            json!("tools/list")
        ]
    );
    assert!(
        header(&seen[0], "authorization") == Some("Bearer tok")
            && header(&seen[0], "x-mcp-toolsets") == Some("repos")
    );
    assert_eq!(
        header(&seen[0], "mcp-protocol-version"),
        Some(apps::PROTOCOL_VERSION)
    );
}

#[tokio::test]
async fn test_the_client_reopens_a_forgotten_session_once() {
    let state = Arc::new(parking_lot::Mutex::new((0, 0)));
    let st = state.clone();
    let http = Mock::new(move |req| {
        let msg = body_json(req);
        if msg["method"] == "initialize" {
            let mut s = st.lock();
            s.0 += 1;
            return Ok((
                200,
                vec![
                    ("content-type".into(), "application/json".into()),
                    ("Mcp-Session-Id".into(), format!("s{}", s.0)),
                ],
                serde_json::to_vec(&json!({"jsonrpc": "2.0", "id": msg["id"], "result": {}}))
                    .unwrap(),
            ));
        }
        if msg.get("id").is_none() {
            return empty_reply(202);
        }
        st.lock().1 += 1;
        if header(req, "mcp-session-id") == Some("s1") {
            return empty_reply(404);
        }
        json_reply(
            200,
            json!({"jsonrpc": "2.0", "id": msg["id"], "result": {"content": []}}),
        )
    });
    let client = McpClient::with_transport("https://x/mcp", no_tok(), &[], http, "");
    assert_eq!(
        Value::Object(client.call_tool("t", &Map::new()).await.unwrap()),
        json!({"content": []})
    );
    assert_eq!(*state.lock(), (2, 2));
}

#[tokio::test]
async fn test_the_client_sends_an_api_key_in_its_header_and_reports_errors() {
    let http = Mock::new(|req| {
        let msg = body_json(req);
        if msg["method"] == "initialize" {
            return json_reply(
                200,
                json!({"jsonrpc": "2.0", "id": msg["id"], "result": {}}),
            );
        }
        if msg.get("id").is_none() {
            return empty_reply(202);
        }
        assert!(
            header(req, "x-goog-api-key") == Some("key") && header(req, "authorization").is_none()
        );
        if msg["method"] == "tools/list" {
            return empty_reply(403);
        }
        json_reply(
            200,
            json!({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32602, "message": "bad"}}),
        )
    });
    let client =
        McpClient::with_transport("https://x/mcp", tok("key"), &[], http, "X-Goog-Api-Key");
    let e = client.list_tools().await.unwrap_err();
    assert!(
        e.kind == "McpError" && e.message.contains("HTTP 403"),
        "{e:?}"
    );
    let e = client.call_tool("t", &Map::new()).await.unwrap_err();
    assert!(e.kind == "McpError" && e.message.contains("bad"), "{e:?}");
}

#[test]
fn test_sse_parsing_joins_data_lines_and_skips_junk() {
    let body = "data: {\"id\": 1,\ndata:  \"result\": {}}\n\ndata: not json\n\nevent: x\ndata: {\"id\": 2}\n";
    let got: Vec<Value> = apps::sse_messages(body)
        .into_iter()
        .map(Value::Object)
        .collect();
    assert_eq!(got, vec![json!({"id": 1, "result": {}}), json!({"id": 2})]);
}

#[tokio::test]
async fn test_the_client_uses_the_protocol_version_the_server_agreed() {
    let http = Mock::new(|req| {
        let msg = body_json(req);
        if msg["method"] == "initialize" {
            return json_reply(
                200,
                json!({"jsonrpc": "2.0", "id": msg["id"], "result": {"protocolVersion": "2025-03-26"}}),
            );
        }
        if msg.get("id").is_none() {
            return empty_reply(202);
        }
        assert_eq!(header(req, "mcp-protocol-version"), Some("2025-03-26"));
        json_reply(
            200,
            json!({"jsonrpc": "2.0", "id": msg["id"], "result": {"tools": []}}),
        )
    });
    let client = McpClient::with_transport("https://x/mcp", no_tok(), &[], http, "");
    assert!(client.list_tools().await.unwrap().is_empty());
}

#[tokio::test]
async fn test_the_client_reports_an_http_error_with_its_body_and_a_missing_reply() {
    let http = Mock::new(|req| {
        let msg = body_json(req);
        if msg["method"] == "initialize" {
            return json_reply(
                200,
                json!({"jsonrpc": "2.0", "id": msg["id"], "result": {}}),
            );
        }
        if msg.get("id").is_none() {
            return empty_reply(202);
        }
        if msg["method"] == "tools/list" {
            return Ok((500, Vec::new(), "x".repeat(300).into_bytes()));
        }
        json_reply(200, json!({"jsonrpc": "2.0", "id": 99, "result": {}}))
    });
    let client = McpClient::with_transport("https://x/mcp", no_tok(), &[], http, "");
    let e = client.list_tools().await.unwrap_err();
    assert_eq!(e.message, format!("HTTP 500: {}", "x".repeat(200)));
    assert_eq!(
        client
            .call_tool("t", &Map::new())
            .await
            .unwrap_err()
            .message,
        "no reply to tools/call"
    );
}

#[tokio::test]
async fn test_a_reply_that_is_not_json_fails_with_pythons_decode_error() {
    // httpx's Response.json() is json.loads of the bytes: the owner reads CPython's own text
    let http = Mock::new(|req| {
        let msg = body_json(req);
        if msg["method"] == "initialize" {
            return json_reply(200, json!({"jsonrpc": "2.0", "id": msg["id"], "result": {}}));
        }
        if msg.get("id").is_none() {
            return empty_reply(202);
        }
        Ok((200, vec![("content-type".into(), "text/html".into())], b"<html>busy</html>".to_vec()))
    });
    let client = McpClient::with_transport("https://x/mcp", no_tok(), &[], http, "");
    let e = client.call_tool("t", &Map::new()).await.unwrap_err();
    assert_eq!(
        (e.kind.as_str(), e.message.as_str()),
        ("JSONDecodeError", "Expecting value: line 1 column 1 (char 0)")
    );
}

#[tokio::test]
async fn test_a_result_whose_content_cannot_be_iterated_is_a_type_error_not_a_reply() {
    // `for c in 3` raises inside execute's try in Python: the model reads the TypeError
    let dir = tmp();
    let (b, _) = bridge(dir.path(), &[], FakeResult::Value(json!({"content": 3})), &[]).await;
    assert_eq!(
        Value::Object(b.execute("calendar__list_events", &Map::new()).await),
        json!({"ok": false, "reason": "TypeError: 'int' object is not iterable"})
    );
    let (b, _) = bridge(dir.path(), &[], FakeResult::Value(json!({"content": 0})), &[]).await;
    assert_eq!(
        Value::Object(b.execute("calendar__list_events", &Map::new()).await),
        json!({"ok": true, "result": ""})
    );
    assert!(apps::try_result_for_model(&obj(json!({"content": true}))).is_err());
    assert_eq!(
        Value::Object(apps::result_for_model(&obj(json!({"content": {"type": "text"}})))),
        json!({"ok": true, "result": ""})
    );
}

// ---- tokens --------------------------------------------------------------------------------------------

fn google_files(dir: &std::path::Path, expires_at: f64) {
    std::fs::write(
        dir.join("google-oauth-client.json"),
        json!({"installed": {"client_id": "cid", "client_secret": "sec"}}).to_string(),
    )
    .unwrap();
    apps::write_private_json(
        &dir.join("google-token.json"),
        &obj(json!({"refresh_token": "r1", "access_token": "a1", "expires_at": expires_at})),
    )
    .unwrap();
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

#[tokio::test]
async fn test_a_google_token_is_refreshed_only_near_expiry_and_kept_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmp();
    let http = Mock::new(|_| json_reply(200, json!({"access_token": "a2", "expires_in": 3600})));
    google_files(dir.path(), now() + 3000.0);
    let src = apps::TokenSource::with_transport("google", cfg(dir.path(), &[], &[]), http.clone());
    assert!(src.available() && src.get().await.as_deref() == Some("a1") && http.seen().is_empty());
    google_files(dir.path(), now() + 10.0);
    assert_eq!(src.get().await.as_deref(), Some("a2"));
    let posts: Vec<Vec<(String, String)>> = http
        .seen()
        .iter()
        .map(|r| {
            url::form_urlencoded::parse(r.body.as_deref().unwrap())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .collect();
    let want: Vec<(String, String)> = [
        ("grant_type", "refresh_token"),
        ("refresh_token", "r1"),
        ("client_id", "cid"),
        ("client_secret", "sec"),
    ]
    .iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect();
    assert_eq!(posts, vec![want]);
    assert_eq!(http.seen()[0].url, apps::GOOGLE_TOKEN_URL);
    let saved: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("google-token.json")).unwrap(),
    )
    .unwrap();
    assert!(saved["access_token"] == "a2" && saved["refresh_token"] == "r1");
    let mode = std::fs::metadata(dir.path().join("google-token.json"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    assert!(!format!("{src:?}").contains("a2"));
}

#[tokio::test]
async fn test_a_refused_refresh_is_none_and_a_log_line_without_the_token() {
    let dir = tmp();
    let http = Mock::new(|_| json_reply(400, json!({"error": "invalid_grant"})));
    google_files(dir.path(), 0.0);
    let (_guard, lines) = capture();
    assert_eq!(
        apps::TokenSource::with_transport("google", cfg(dir.path(), &[], &[]), http)
            .get()
            .await,
        None
    );
    let text = lines.text();
    assert!(
        text.contains("invalid_grant") && !text.contains("r1"),
        "{text}"
    );
}

#[tokio::test]
async fn test_a_token_endpoint_that_answers_junk_is_none_and_names_the_type_only() {
    let dir = tmp();
    let http = Mock::new(|_| Ok((200, Vec::new(), b"<html>r1</html>".to_vec())));
    google_files(dir.path(), 0.0);
    let (_guard, lines) = capture();
    assert_eq!(
        apps::TokenSource::with_transport("google", cfg(dir.path(), &[], &[]), http)
            .get()
            .await,
        None
    );
    let text = lines.text();
    assert!(
        text.contains("apps: google token refresh failed (JSONDecodeError)")
            && !text.contains("r1"),
        "{text}"
    );
}

#[tokio::test]
async fn test_env_tokens_are_read_as_they_are() {
    let dir = tmp();
    let c = cfg(
        dir.path(),
        &[
            ("EVE_GITHUB_MCP_TOKEN", " ghp \n"),
            ("EVE_GOOGLE_MAPS_KEY", ""),
        ],
        &[],
    );
    assert_eq!(
        apps::TokenSource::new("github", c.clone())
            .get()
            .await
            .as_deref(),
        Some("ghp")
    );
    assert_eq!(apps::TokenSource::new("maps", c.clone()).get().await, None);
    assert!(!apps::TokenSource::new("maps", c).available());
}

#[test]
fn test_app_urls_stay_out_of_the_log() {
    assert!(!apps::quiet_app_urls(
        "HTTP Request: GET https://gmail.googleapis.com/gmail/v1/users/me/threads?q=from%3Asam \"HTTP/1.1 200 OK\""
    ));
    assert!(apps::quiet_app_urls(
        "HTTP Request: POST https://api.openai.com/v1/responses \"HTTP/1.1 200 OK\""
    ));
}

#[test]
fn test_a_private_json_file_is_written_as_python_writes_it() {
    let dir = tmp();
    let path = dir.path().join("nested").join("t.json");
    apps::write_private_json(
        &path,
        &obj(json!({"a": "é", "n": 1.5, "l": [1, 2], "e": {}})),
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{\n  \"a\": \"\\u00e9\",\n  \"n\": 1.5,\n  \"l\": [\n    1,\n    2\n  ],\n  \"e\": {}\n}\n"
    );
    assert!(!dir.path().join("nested").join("t.json.tmp").exists());
    assert_eq!(apps::read_json(&path)["a"], "é");
    std::fs::write(dir.path().join("list.json"), "[1]").unwrap();
    assert!(apps::read_json(&dir.path().join("list.json")).is_empty());
    assert!(apps::read_json(&dir.path().join("missing.json")).is_empty());
}

// ---- Google's plain APIs (the apps_mcp assertions; the rest are eve-google-rest's) -----------------------

#[test]
fn test_contacts_is_a_read_only_toolset() {
    let contacts = &eve_google_rest::TOOLS["contacts"];
    assert_eq!(
        contacts
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["search_contacts"]
    );
    assert!(contacts.iter().all(apps::tool_reads));
    let send = eve_google_rest::TOOLS["gmail"]
        .iter()
        .find(|t| t["name"] == "send_message")
        .unwrap();
    assert!(!apps::tool_reads(send)); // a send is never a read
}

#[test]
fn test_an_email_question_is_the_whole_email_with_hidden_characters_shown() {
    let body = format!("Line one\n\nLine two {}", "word ".repeat(300));
    let q = apps::describe_whole(
        "gmail__send_message",
        &obj(json!({"to": ["ana@example.com"], "subject": "Hi\u{202e}", "body": body})),
        apps::WHOLE_ASK_CHARS,
    )
    .unwrap();
    assert!(
        q.starts_with("Send this email?\nTo: ana@example.com\nSubject: Hi[U+202E]\n\n")
            && q.ends_with(&body)
    );
    assert!(apps::asks_whole("gmail__send_message") && !apps::asks_whole("drive__create_file"));
    let too_long = apps::describe_whole(
        "gmail__send_message",
        &obj(json!({"to": ["a@b.co"], "body": "x".repeat(4000)})),
        apps::WHOLE_ASK_CHARS,
    );
    assert!(too_long.is_none()); // refused unasked, never clipped
}

#[test]
fn test_a_reply_question_says_it_goes_into_the_thread() {
    let q = apps::describe_whole(
        "gmail__send_message",
        &obj(json!({"to": "sam@example.com", "cc": ["a@x", "b@x"], "replyToMessageId": "m1", "subject": "  ", "body": "ok"})),
        500,
    )
    .unwrap();
    assert_eq!(
        q,
        "Send this email?\nTo: sam@example.com\nCc: a@x, b@x\nIn reply to the message eve read (same thread)\n\
         Subject: (Re: the original's)\n\nok"
    );
    let plain =
        apps::describe_whole("gmail__send_message", &obj(json!({"to": ["a@x"]})), 500).unwrap();
    assert_eq!(plain, "Send this email?\nTo: a@x\nSubject: (none)\n\n");
    let other = apps::describe_whole(
        "github__create_issue",
        &obj(json!({"repo": "eve", "labels": ["a", "b"]})),
        500,
    )
    .unwrap();
    assert_eq!(
        other,
        "GitHub: create issue with repo: eve, labels: [\"a\", \"b\"]?"
    );
}

#[test]
fn test_the_rest_tools_carry_the_mcp_servers_names() {
    let cal: Vec<&str> = eve_google_rest::TOOLS["calendar"]
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(&cal[..2], &["list_calendars", "list_events"]);
    for (app, tools) in eve_google_rest::TOOLS.iter() {
        for t in tools {
            assert_eq!(
                apps::tool_reads(t),
                t["annotations"]["readOnlyHint"] == json!(true),
                "{app} {}",
                t["name"]
            );
        }
    }
}

#[test]
fn test_github_asks_for_ci_beside_its_toolsets() {
    let headers: IndexMap<&str, &str> = apps::SERVERS["github"].headers.iter().cloned().collect();
    assert_eq!(
        headers["X-MCP-Toolsets"],
        "context,repos,issues,pull_requests,users"
    );
    assert_eq!(
        headers["X-MCP-Tools"],
        "actions_list,actions_get,get_job_logs"
    ); // notifications: classic token only
    assert!(!headers.contains_key("X-MCP-Readonly")); // it would override the ask policy's writes
}

#[tokio::test]
async fn test_maps_replies_keep_their_links() {
    let dir = tmp();
    let (b, _) = bridge(dir.path(), &[], FakeResult::Default, &[]).await;
    assert!(b.instructions().contains("Google Maps link"));
}

// ---- the open_app helpers and the names --------------------------------------------------------------

#[test]
fn test_open_tool_spec_and_open_asked() {
    assert!(apps::open_tool_spec(&[]).is_none());
    let spec = apps::open_tool_spec(&[("share", "Share"), ("maps", "Google Maps")]).unwrap();
    assert_eq!(
        spec["description"],
        "Load the tools of the owner's apps for the rest of this turn, then call them. Name every app the request \
         needs in one call. Apps: share = Share; maps = Google Maps."
    );
    assert_eq!(
        spec["parameters"]["properties"]["apps"]["items"]["enum"],
        json!(["share", "maps"])
    );
    assert_eq!(
        apps::open_asked(&obj(
            json!({"apps": [" Maps ", "", null, "maps", 3], "app": "Gmail"})
        )),
        vec!["maps", "3", "gmail"]
    );
    assert_eq!(
        apps::split_name("calendar__list__x"),
        ("calendar", "list__x")
    );
    assert_eq!(apps::split_name("slack__send"), ("", ""));
}

#[test]
fn test_lean_replay_an_unhinted_tool_named_like_a_read_does_not_run_under_a_read_policy() {
    // test_lean_appspolicy_replay.py: Gmail read only, a server listing find_and_replace with no annotations
    fn tools(app: &str) -> Vec<Value> {
        if app == "gmail" {
            vec![json!({"name": "find_and_replace"})]
        } else {
            Vec::new()
        }
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let dir = tmp();
        let (b, calls) = bridge_with(
            dir.path(),
            &[],
            FakeResult::Default,
            &[("gmail", "read")],
            tools,
        )
        .await;
        let name = "gmail__find_and_replace";
        assert!(!b.names().contains(name)); // not offered: Gmail is read only
        assert_eq!(b.decide(name, None).action, "refuse");
        if b.decide(name, None).action == "run" {
            b.execute(name, &Map::new()).await;
        }
        assert!(calls.lock().is_empty());
    });
}

#[tokio::test]
async fn test_lean_replay_control_a_hinted_read_runs_and_a_hinted_trash_does_not() {
    fn tools(app: &str) -> Vec<Value> {
        if app == "gmail" {
            vec![
                json!({"name": "search_threads", "annotations": {"readOnlyHint": true}}),
                json!({"name": "trash_thread", "annotations": {"readOnlyHint": true}}),
            ]
        } else {
            Vec::new()
        }
    }
    let dir = tmp();
    let (b, _) = bridge_with(
        dir.path(),
        &[],
        FakeResult::Default,
        &[("gmail", "read")],
        tools,
    )
    .await;
    assert_eq!(b.decide("gmail__search_threads", None).action, "run");
    assert_eq!(b.decide("gmail__trash_thread", None).action, "refuse");
}

#[test]
fn test_the_policies_are_the_four_self_context_words_for() {
    // test_self_context.py: self_context's rules cover exactly apps_mcp.POLICIES
    assert_eq!(apps::POLICIES, ["read", "auto", "write", "ask"]);
}

// ---- the one-time sign-in ------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_google_login_signs_in_on_a_loopback_port_and_says_what_was_not_granted() {
    use std::io::{Read, Write};
    let dir = tmp();
    std::fs::write(
        dir.path().join("google-oauth-client.json"),
        json!({"installed": {"client_id": "cid", "client_secret": "sec"}}).to_string(),
    )
    .unwrap();
    let http = Mock::new(|_| {
        json_reply(
            200,
            json!({"refresh_token": "r9", "access_token": "a9", "expires_in": 3600,
                              "scope": "https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/calendar"}),
        )
    });
    let said: Arc<parking_lot::Mutex<Vec<String>>> = Arc::default();
    let pages: Arc<parking_lot::Mutex<Vec<String>>> = Arc::default();
    let (s, p) = (said.clone(), pages.clone());
    let say = move |text: &str| s.lock().push(text.to_string());
    let open_url = move |url: &str| {
        let u = url::Url::parse(url).unwrap();
        let q: IndexMap<String, String> = u
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(q["client_id"], "cid");
        assert_eq!(q["code_challenge_method"], "S256");
        assert!(q["scope"].contains("gmail.send") && q["access_type"] == "offline");
        let redirect = url::Url::parse(&q["redirect_uri"]).unwrap();
        let addr = format!("127.0.0.1:{}", redirect.port().unwrap());
        let state = q["state"].clone();
        let pages = p.clone();
        std::thread::spawn(move || {
            for path in [
                "/favicon.ico".to_string(),
                "/?state=wrong&code=evil".to_string(),
                format!("/?code=c1&state={state}"),
            ] {
                let mut conn = std::net::TcpStream::connect(&addr).unwrap();
                write!(conn, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
                let mut page = String::new();
                conn.read_to_string(&mut page).unwrap();
                pages.lock().push(page);
            }
        });
    };
    let scope =
        apps::google_login_with(&cfg(dir.path(), &[], &[]), &open_url, 0, &say, http.clone())
            .await
            .unwrap();
    assert!(scope.contains("gmail.readonly"));
    let said = said.lock().clone();
    assert!(said[0].starts_with("Opening Google sign-in in your browser. If it does not open, visit:\nhttps://accounts.google.com/"));
    assert!(said[1].starts_with(
        "Not granted (those tools will fail until you sign in again and allow them): "
    ));
    assert!(
        said[1].contains("gmail.send")
            && !said[1].contains("auth/calendar,")
            && said[1].contains("drive.file")
    );
    let form: IndexMap<String, String> =
        url::form_urlencoded::parse(http.seen()[0].body.as_deref().unwrap())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["code"], "c1"); // the wrong state's code is never used
    assert!(
        form["code_verifier"].len() == 86 && form["redirect_uri"].starts_with("http://127.0.0.1:")
    );
    let saved: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("google-token.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["refresh_token"], "r9");
    assert_eq!(
        saved
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec![
            "refresh_token",
            "access_token",
            "expires_at",
            "scope",
            "signed_in_at"
        ]
    );
    // the browser's pages: the third (or a later one) says it is signed in
    for _ in 0..50 {
        if pages.lock().len() == 3 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let pages = pages.lock().clone();
    assert!(pages[0].ends_with("eve: no sign-in code here."));
    assert!(
        pages[2].starts_with("HTTP/1.0 200 OK")
            && pages[2].ends_with("eve: signed in. You can close this tab.")
    );
}

#[test]
fn test_pkce_is_s256_over_the_verifier() {
    use base64::Engine as _;
    use sha2::Digest as _;
    let (verifier, challenge) = apps::pkce();
    assert_eq!(verifier.len(), 86); // token_urlsafe(64)
    let want = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(verifier.as_bytes()));
    assert_eq!(challenge, want);
    assert_ne!(apps::pkce().0, verifier);
}
