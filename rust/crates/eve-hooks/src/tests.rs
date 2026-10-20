use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::client::{self, post, read_hook_input_from, replace_lone_surrogates, DEFAULT_TIMEOUT_SECS};
use crate::pretooluse::{summarize, BLOCK_TIMEOUT_SECS};
use crate::{
    notification, permission_request, posttooluse, pretooluse, py_strip, py_truthy, session_end, session_start, stop,
    user_prompt_submit, HookError, Poster,
};

type RunFn = fn(&[u8], &mut Poster<'_>, &mut dyn Write) -> Result<i32, HookError>;

/// Run a hook with a fake `post` that records the event and timeout and answers `resp`.
fn run_hook(run: RunFn, input: &str, resp: Option<Value>) -> (Result<i32, HookError>, String, Vec<(Value, f64)>) {
    let mut sent = Vec::new();
    let mut out = Vec::new();
    let result = {
        let mut post = |event: Value, timeout: f64| {
            sent.push((event, timeout));
            resp.clone()
        };
        run(input.as_bytes(), &mut post, &mut out)
    };
    (result, String::from_utf8(out).unwrap(), sent)
}

// --- bridge/tests/test_relay_questions.py -----------------------------------

fn run_permission_hook(resp: Option<Value>) -> (String, Value) {
    let input = r#"{"tool_name": "Write", "tool_input": {"file_path": "/x/a.py"}, "session_id": "s"}"#;
    let (code, text, sent) = run_hook(permission_request::run, input, resp);
    assert_eq!(code, Ok(0));
    (text, sent.into_iter().next().unwrap().0)
}

#[test]
fn test_permission_hook_output_is_the_shape_claude_code_validates() {
    let (text, sent) = run_permission_hook(Some(json!({"ok": true, "decision": "allow"})));
    assert!(sent["evt"] == "permissionrequest" && sent["hint"] == "/x/a.py");
    let parsed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        parsed,
        json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": {"behavior": "allow"}}})
    );
    let (text, _) = run_permission_hook(Some(json!({"ok": true, "decision": "deny"})));
    let denied = serde_json::from_str::<Value>(&text).unwrap()["hookSpecificOutput"]["decision"].clone();
    assert!(denied["behavior"] == "deny" && !denied["message"].as_str().unwrap().is_empty());
    assert_eq!(run_permission_hook(Some(json!({"ok": true}))).0, "");
    assert_eq!(run_permission_hook(None).0, "");
}

// --- bridge/tests/test_telegram.py (the _summarize half) ---------------------

#[test]
fn test_a_question_for_the_owner_is_streamed_as_a_question() {
    let asked = summarize(&json!({"questions": [
        {"question": "Which database?", "header": "DB",
         "options": [{"label": "Postgres", "description": "x"}, {"label": "SQLite", "description": "y"}]},
        {"question": "Ship it now?", "options": []},
    ]}))
    .unwrap();
    assert_eq!(asked, "Which database? (1. Postgres / 2. SQLite) | Ship it now?");
    assert_eq!(summarize(&json!({"questions": "nope", "command": "ls"})).unwrap(), "ls");
}

// --- bridge/tests/test_ipc_transport.py (the hook half) ----------------------

#[test]
fn test_hook_post_returns_none_for_invalid_transport_spec() {
    assert_eq!(post(&json!({"evt": "session_start"}), Some("0.0.0.0:48765"), DEFAULT_TIMEOUT_SECS, DEFAULT_TIMEOUT_SECS), None);
}

// --- _client ----------------------------------------------------------------

#[test]
fn test_read_hook_input_empty_and_garbage_give_an_empty_dict() {
    assert_eq!(read_hook_input_from(b"").unwrap(), json!({}));
    assert_eq!(read_hook_input_from(b"not json").unwrap(), json!({}));
    assert_eq!(read_hook_input_from(b"{\"a\": 1").unwrap(), json!({}));
    // A BOM is refused by json.loads too.
    assert_eq!(read_hook_input_from("\u{feff}{\"a\": 1}".as_bytes()).unwrap(), json!({}));
    // Any JSON value comes back as is.
    assert_eq!(read_hook_input_from(b"[1, 2]").unwrap(), json!([1, 2]));
    assert_eq!(read_hook_input_from(b" {\"a\": 1}\n").unwrap(), json!({"a": 1}));
}

#[test]
fn test_read_hook_input_replaces_bad_utf8_and_lone_surrogates() {
    let raw = b"{\"prompt\": \"caf\xff\"}";
    assert_eq!(read_hook_input_from(raw).unwrap(), json!({"prompt": "caf\u{fffd}"}));
    assert_eq!(read_hook_input_from(br#"{"p": "a\ud800b"}"#).unwrap(), json!({"p": "a?b"}));
    assert_eq!(read_hook_input_from(br#"{"p": "\udc00"}"#).unwrap(), json!({"p": "?"}));
    assert_eq!(read_hook_input_from(br#"{"p": "\ud800\ud800x"}"#).unwrap(), json!({"p": "??x"}));
    // A real pair is joined, other escapes stay escapes.
    assert_eq!(read_hook_input_from(br#"{"p": "\ud83d\ude00"}"#).unwrap(), json!({"p": "\u{1F600}"}));
    assert_eq!(read_hook_input_from(br#"{"p": "\\ud800 \"q\" \u00e9"}"#).unwrap(), json!({"p": "\\ud800 \"q\" \u{e9}"}));
    // Outside a string nothing is touched.
    assert_eq!(replace_lone_surrogates(r#"{"k": 1}"#), r#"{"k": 1}"#);
}

#[test]
fn test_py_helpers_match_python() {
    assert_eq!(py_strip("\u{1c} hi \u{a0}\t\n"), "hi");
    assert_eq!(py_strip("\u{200b}x"), "\u{200b}x");
    for falsy in [json!(null), json!(false), json!(0), json!(0.0), json!(""), json!([]), json!({})] {
        assert!(!py_truthy(&falsy), "{falsy}");
    }
    for truthy in [json!(true), json!(1), json!(-0.5), json!(" "), json!([0]), json!({"a": null})] {
        assert!(py_truthy(&truthy), "{truthy}");
    }
}

/// A one-shot daemon on a Unix socket: reads one line, answers `reply` (raw bytes), closes.
fn one_shot_server(dir: &tempfile::TempDir, reply: &'static [u8], delay: Duration) -> (String, mpsc::Receiver<String>) {
    let path = dir.path().join("eve.sock").to_string_lossy().into_owned();
    let listener = UnixListener::bind(&path).unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        tx.send(line).unwrap();
        thread::sleep(delay);
        let mut stream = stream;
        let _ = stream.write_all(reply);
    });
    (path, rx)
}

#[test]
fn test_post_sends_one_python_json_line_and_reads_one_reply() {
    let dir = tempfile::tempdir().unwrap();
    let (path, rx) = one_shot_server(&dir, b"{\"ok\": true, \"decision\": \"allow\"}\n{\"ignored\": 1}\n", Duration::ZERO);
    let resp = post(&json!({"evt": "turn_begin", "prompt": "café 日本"}), Some(&path), 3.0, 3.0);
    assert_eq!(resp, Some(json!({"ok": true, "decision": "allow"})));
    // json.dumps(event, ensure_ascii=False) + "\n"
    assert_eq!(rx.recv().unwrap(), "{\"evt\": \"turn_begin\", \"prompt\": \"café 日本\"}\n");
}

#[test]
fn test_post_reads_a_reply_ended_by_eof() {
    let dir = tempfile::tempdir().unwrap();
    let (path, _rx) = one_shot_server(&dir, b"{\"ok\": false}", Duration::ZERO);
    assert_eq!(post(&json!({"evt": "x"}), Some(&path), 3.0, 3.0), Some(json!({"ok": false})));
}

#[test]
fn test_post_returns_none_for_empty_bad_or_slow_replies() {
    let dir = tempfile::tempdir().unwrap();
    let (path, _rx) = one_shot_server(&dir, b"", Duration::ZERO);
    assert_eq!(post(&json!({"evt": "x"}), Some(&path), 3.0, 3.0), None);

    let dir = tempfile::tempdir().unwrap();
    let (path, _rx) = one_shot_server(&dir, b"\n{\"ok\": true}\n", Duration::ZERO);
    assert_eq!(post(&json!({"evt": "x"}), Some(&path), 3.0, 3.0), None);

    let dir = tempfile::tempdir().unwrap();
    let (path, _rx) = one_shot_server(&dir, b"nope\n", Duration::ZERO);
    assert_eq!(post(&json!({"evt": "x"}), Some(&path), 3.0, 3.0), None);

    let dir = tempfile::tempdir().unwrap();
    let (path, _rx) = one_shot_server(&dir, b"\"\xff\"\n", Duration::ZERO);
    assert_eq!(post(&json!({"evt": "x"}), Some(&path), 3.0, 3.0), None);

    // The read timeout applies once connected.
    let dir = tempfile::tempdir().unwrap();
    let (path, _rx) = one_shot_server(&dir, b"{\"ok\": true}\n", Duration::from_millis(1500));
    let started = Instant::now();
    assert_eq!(post(&json!({"evt": "x"}), Some(&path), 0.3, 3.0), None);
    assert!(started.elapsed() < Duration::from_millis(1400));
}

#[test]
fn test_post_returns_none_when_nobody_listens() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("none.sock").to_string_lossy().into_owned();
    assert_eq!(post(&json!({"evt": "x"}), Some(&missing), 3.0, 3.0), None);
    assert_eq!(post(&json!({"evt": "x"}), Some(&missing), -1.0, 3.0), None);
}

#[test]
fn test_post_over_tcp_loopback() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let mut stream = stream;
        stream.write_all(b"{\"ok\": true}\n").unwrap();
        line
    });
    let spec = format!("127.0.0.1:{port}");
    assert_eq!(post(&json!({"evt": "session_end", "session_id": "s"}), Some(&spec), 3.0, 3.0), Some(json!({"ok": true})));
    assert_eq!(server.join().unwrap(), "{\"evt\": \"session_end\", \"session_id\": \"s\"}\n");
}

// --- pretooluse ---------------------------------------------------------------

const PRE_INPUT: &str = r#"{"session_id": "s1", "tool_use_id": "t1", "tool_name": "Bash",
    "tool_input": {"command": "rm -rf build", "description": "clean"}, "cwd": "/r", "permission_mode": "default"}"#;

#[test]
fn test_pretooluse_event_and_blocking_timeout() {
    let (code, text, sent) = run_hook(pretooluse::run, PRE_INPUT, None);
    assert_eq!(code, Ok(0));
    assert_eq!(text, "");
    let (event, timeout) = &sent[0];
    assert_eq!(*timeout, BLOCK_TIMEOUT_SECS);
    assert_eq!(
        eve_ipc::dumps(event),
        r#"{"evt": "pretooluse", "session_id": "s1", "tool_use_id": "t1", "tool_name": "Bash", "hint": "rm -rf build", "cwd": "/r", "permission_mode": "default"}"#
    );
}

#[test]
fn test_pretooluse_writes_each_decision_with_its_reason() {
    for (decision, reason) in [
        ("allow", "User approved on Hardware Eve device."),
        ("deny", "User denied on Hardware Eve device. Do not retry with alternative phrasings; the human has rejected this action."),
        ("ask", "Hardware Eve deferred to Claude Code's normal approval flow."),
    ] {
        let (code, text, _) = run_hook(pretooluse::run, PRE_INPUT, Some(json!({"ok": true, "decision": decision})));
        assert_eq!(code, Ok(0));
        assert_eq!(
            text,
            format!(
                "{{\"hookSpecificOutput\": {{\"hookEventName\": \"PreToolUse\", \"permissionDecision\": \"{decision}\", \"permissionDecisionReason\": \"{}\"}}}}\n",
                reason
            )
        );
    }
}

#[test]
fn test_pretooluse_defers_without_a_usable_decision() {
    for resp in [
        None,
        Some(json!(null)),
        Some(json!({"ok": false, "decision": "allow"})),
        Some(json!({"ok": true})),
        Some(json!({"ok": true, "decision": "maybe"})),
        Some(json!({"ok": 1, "decision": 1})),
    ] {
        let (code, text, _) = run_hook(pretooluse::run, PRE_INPUT, resp.clone());
        assert_eq!((code, text.as_str()), (Ok(0), ""), "{resp:?}");
    }
    // A truthy non-bool "ok" still counts.
    let (_, text, _) = run_hook(pretooluse::run, PRE_INPUT, Some(json!({"ok": 1, "decision": "allow"})));
    assert!(text.contains("\"allow\""));
}

#[test]
fn test_a_reply_that_is_not_a_dict_fails_like_python() {
    let (code, text, _) = run_hook(pretooluse::run, PRE_INPUT, Some(json!([1])));
    assert_eq!(code, Err(HookError::new("AttributeError", "'list' object has no attribute 'get'")));
    assert_eq!(text, "");
    let (code, _, _) = run_hook(permission_request::run, PRE_INPUT, Some(json!(false)));
    assert_eq!(code, Err(HookError::new("AttributeError", "'bool' object has no attribute 'get'")));
}

#[test]
fn test_summarize_picks_the_hint_like_python() {
    assert_eq!(summarize(&json!({"description": "d", "file_path": "/f", "command": ""})).unwrap(), "/f");
    assert_eq!(summarize(&json!({"path": "/p", "url": "u"})).unwrap(), "/p");
    assert_eq!(summarize(&json!({"url": "https://x"})).unwrap(), "https://x");
    assert_eq!(summarize(&json!({"n": 3, "a": "", "b": "first"})).unwrap(), "first");
    assert_eq!(summarize(&json!({"command": 5})).unwrap(), "");
    assert_eq!(summarize(&json!("raw text")).unwrap(), "raw text");
    assert_eq!(summarize(&json!("")).unwrap(), "");
    assert_eq!(summarize(&json!(null)).unwrap(), "");
    assert_eq!(summarize(&json!(["x"])).unwrap(), "");
}

#[test]
fn test_question_follows_pythons_str_and_or() {
    // str() of non-strings, strip(), skipped empties and non-dicts.
    let asked = summarize(&json!({"questions": [
        "not a dict",
        {"question": 42, "options": [{"label": 1.0}, {"label": true}, {"label": "  "}, "x", {"label": null}]},
        {"question": "  Pad  ", "options": {"label": "a dict iterates its keys"}},
        {"question": "Str", "options": "abc"},
        {"question": "", "options": [{"label": "lost"}]},
        {"question": ["a", 1]},
    ]}))
    .unwrap();
    assert_eq!(asked, "42 (1. 1.0 / 2. True) | Pad | Str | ['a', 1]");
    // Not that shape, or nothing to ask: fall through.
    assert_eq!(summarize(&json!({"questions": [], "command": "ls"})).unwrap(), "ls");
    assert_eq!(summarize(&json!({"questions": [{"options": [{"label": "a"}]}], "file_path": "/f"})).unwrap(), "/f");
    // Truthy options that cannot be iterated raise, as in Python.
    assert_eq!(
        summarize(&json!({"questions": [{"question": "q", "options": 3}]})),
        Err(HookError::new("TypeError", "'int' object is not iterable"))
    );
    assert_eq!(summarize(&json!({"questions": [{"question": "q", "options": 0}]})).unwrap(), "q");
}

// --- permission_request ---------------------------------------------------------

#[test]
fn test_permission_request_event_and_deny_text() {
    let (_, text, sent) = run_hook(permission_request::run, PRE_INPUT, Some(json!({"ok": true, "decision": "deny"})));
    let (event, timeout) = &sent[0];
    assert_eq!(*timeout, BLOCK_TIMEOUT_SECS);
    assert_eq!(
        eve_ipc::dumps(event),
        r#"{"evt": "permissionrequest", "session_id": "s1", "tool_use_id": "t1", "tool_name": "Bash", "hint": "rm -rf build", "cwd": "/r", "permission_mode": "default"}"#
    );
    assert_eq!(
        text,
        "{\"hookSpecificOutput\": {\"hookEventName\": \"PermissionRequest\", \"decision\": {\"behavior\": \"deny\", \"message\": \"The owner denied this from their phone. Do not retry it with alternative phrasings; the human has rejected this action.\"}}}\n"
    );
    // "ask" is not a PermissionRequest decision.
    let (_, text, _) = run_hook(permission_request::run, PRE_INPUT, Some(json!({"ok": true, "decision": "ask"})));
    assert_eq!(text, "");
}

// --- the fire-and-forget hooks ----------------------------------------------------

fn sent_line(run: RunFn, input: &str) -> String {
    let (code, text, sent) = run_hook(run, input, Some(json!({"ok": true})));
    assert_eq!(code, Ok(0));
    assert_eq!(text, "");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1, DEFAULT_TIMEOUT_SECS);
    eve_ipc::dumps(&sent[0].0)
}

#[test]
fn test_each_notice_hook_sends_its_event() {
    let full = r#"{"session_id": "s", "tool_use_id": "t", "tool_name": "Edit", "transcript_path": "/t.jsonl",
        "cwd": "/c", "prompt": "  hi there \n", "message": "needs you", "type": "permission_prompt", "tool_response": "big"}"#;
    assert_eq!(sent_line(posttooluse::run, full), r#"{"evt": "posttooluse", "session_id": "s", "tool_use_id": "t", "tool_name": "Edit"}"#);
    assert_eq!(sent_line(session_end::run, full), r#"{"evt": "session_end", "session_id": "s"}"#);
    assert_eq!(
        sent_line(session_start::run, full),
        r#"{"evt": "session_start", "session_id": "s", "transcript_path": "/t.jsonl", "cwd": "/c"}"#
    );
    assert_eq!(sent_line(stop::run, full), r#"{"evt": "turn_end", "session_id": "s", "transcript_path": "/t.jsonl"}"#);
    assert_eq!(sent_line(user_prompt_submit::run, full), r#"{"evt": "turn_begin", "session_id": "s", "prompt": "hi there"}"#);
    assert_eq!(
        sent_line(notification::run, full),
        r#"{"evt": "notification", "session_id": "s", "message": "needs you", "notification_type": "permission_prompt"}"#
    );
}

#[test]
fn test_missing_fields_default_to_empty_and_present_values_pass_through() {
    assert_eq!(sent_line(session_start::run, ""), r#"{"evt": "session_start", "session_id": "", "transcript_path": "", "cwd": ""}"#);
    assert_eq!(sent_line(user_prompt_submit::run, r#"{"prompt": null}"#), r#"{"evt": "turn_begin", "session_id": "", "prompt": ""}"#);
    // payload.get(key, "") hands a present null or number through unchanged.
    assert_eq!(sent_line(session_end::run, r#"{"session_id": null}"#), r#"{"evt": "session_end", "session_id": null}"#);
    assert_eq!(sent_line(stop::run, r#"{"session_id": 7}"#), r#"{"evt": "turn_end", "session_id": 7, "transcript_path": ""}"#);
    // type, then notification_type, then "".
    assert_eq!(
        sent_line(notification::run, r#"{"type": "", "notification_type": "idle_prompt"}"#),
        r#"{"evt": "notification", "session_id": "", "message": "", "notification_type": "idle_prompt"}"#
    );
    assert_eq!(
        sent_line(notification::run, r#"{"type": 0}"#),
        r#"{"evt": "notification", "session_id": "", "message": "", "notification_type": ""}"#
    );
}

#[test]
fn test_a_payload_that_is_not_a_dict_fails_like_python() {
    let (code, _, sent) = run_hook(session_start::run, "[1]", None);
    assert_eq!(code, Err(HookError::new("AttributeError", "'list' object has no attribute 'get'")));
    assert!(sent.is_empty());
    let (code, _, _) = run_hook(pretooluse::run, "null", None);
    assert_eq!(code, Err(HookError::new("AttributeError", "'NoneType' object has no attribute 'get'")));
    let (code, _, sent) = run_hook(user_prompt_submit::run, r#"{"prompt": 5}"#, None);
    assert_eq!(code, Err(HookError::new("AttributeError", "'int' object has no attribute 'strip'")));
    assert!(sent.is_empty());
}

#[test]
fn test_client_module_constants() {
    assert_eq!(client::DEFAULT_TIMEOUT_SECS, 3.0);
    assert_eq!(BLOCK_TIMEOUT_SECS, 320.0);
}

#[test]
fn test_read_hook_input_parses_like_cpython_json_loads() {
    // Deeper than serde_json's default limit of 128, well inside CPython's: still parsed.
    let deep = format!("{{\"session_id\": \"s\", \"tool_input\": {}1{}}}", "[".repeat(300), "]".repeat(300));
    let payload = read_hook_input_from(deep.as_bytes()).unwrap();
    assert_eq!(payload["session_id"], "s");
    assert_eq!(sent_line(session_end::run, &deep), r#"{"evt": "session_end", "session_id": "s"}"#);
    // Past the recursion limit json.loads raises RecursionError, which `except ValueError` lets through.
    let too_deep = format!("{}{}", "[".repeat(eve_ipc::pyjson::MAX_DEPTH + 1), "]".repeat(eve_ipc::pyjson::MAX_DEPTH + 1));
    assert_eq!(read_hook_input_from(too_deep.as_bytes()).unwrap_err().kind, "RecursionError");
    let (code, _, sent) = run_hook(session_start::run, &too_deep, None);
    assert_eq!(code.unwrap_err().kind, "RecursionError");
    assert!(sent.is_empty());
    // CPython's ValueErrors: the int-digit limit, trailing data.
    let huge = format!("{{\"n\": {}}}", "9".repeat(4301));
    assert_eq!(read_hook_input_from(huge.as_bytes()).unwrap(), json!({}));
    assert_eq!(read_hook_input_from(b"{} {}").unwrap(), json!({}));
}

#[test]
fn test_post_with_a_zero_timeout_does_not_block() {
    // settimeout(0.0) is non-blocking: the first recv finds nothing and post gives up.
    let dir = tempfile::tempdir().unwrap();
    let (path, _rx) = one_shot_server(&dir, b"{\"ok\": true}\n", Duration::from_millis(1500));
    let started = Instant::now();
    assert_eq!(post(&json!({"evt": "x"}), Some(&path), 0.0, 3.0), None);
    assert!(started.elapsed() < Duration::from_millis(1400));
}
