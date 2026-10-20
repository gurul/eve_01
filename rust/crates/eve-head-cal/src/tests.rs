//! Port of `bridge/tests/test_head_cal.py` (the page's API, driven with a fake daemon), plus the HTTP side.

use std::io::{Cursor, Read, Write};
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use super::*;

/// `fake(answer)`: an ipc that records every request and always gives `answer`.
fn fake(answer: Option<Value>) -> (impl FnMut(Value) -> Option<Value>, Arc<Mutex<Vec<Value>>>) {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let log = sent.clone();
    let ipc = move |req: Value| {
        log.lock().unwrap().push(req);
        answer.clone()
    };
    (ipc, sent)
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => panic!("not an object"),
    }
}

#[test]
fn test_each_extreme_holds_a_pose_inside_the_boards_limits() {
    for (name, (yaw, pitch)) in POSES {
        let keys = pose_keys(name).unwrap();
        assert!(keys.len() <= 8 && keys[keys.len() - 1][0] <= 8000); // motion.h: 8 keys inside kBoutMaxMs
        assert!(keys.iter().all(|k| k[1] == yaw && k[2] == pitch));
        assert!(yaw.abs() <= 100 && (5..=85).contains(&pitch));
    }
    let p = |n| pose(n).unwrap();
    assert!(p("down").1 < p("center").1 && p("center").1 < p("up").1);
    assert!(p("left").0 > 0 && 0 > p("right").0); // body.cpp: +yaw is the viewer's left
    assert_eq!(pose_keys("down").unwrap(), vec![[0, 0, 5, 300], [3000, 0, 5, 300], [6000, 0, 5, 300]]);
    assert_eq!(pose_keys("sideways"), None);
}

#[test]
fn test_a_tap_moves_the_head_and_a_bad_pose_is_refused() {
    let (mut ipc, sent) = fake(Some(json!({"ok": true})));
    assert_eq!(
        handle("/api/move", &obj(json!({"pose": "down"})), &mut ipc),
        Ok((200, json!({"ok": true, "error": null, "pose": "down"})))
    );
    {
        let sent = sent.lock().unwrap();
        let last = sent.last().unwrap();
        assert!(last["evt"] == "move" && last["kind"] == "keys" && last["keys"][0][2] == 5);
        assert_eq!(dumps_ascii(last), r#"{"evt": "move", "kind": "keys", "keys": [[0, 0, 5, 300], [3000, 0, 5, 300], [6000, 0, 5, 300]]}"#);
    }
    let (code, out) = handle("/api/move", &obj(json!({"pose": "sideways"})), &mut ipc).unwrap();
    assert!(code == 400 && out["ok"] == false && sent.lock().unwrap().len() == 1);
    assert_eq!(out["error"], "unknown pose 'sideways'");
}

#[test]
fn test_a_flip_goes_to_the_board_and_a_read_sends_nothing_to_set() {
    let (mut ipc, sent) = fake(Some(json!({"ok": true, "pitch_rev": false, "yaw_rev": false})));
    assert_eq!(handle("/api/axis", &obj(json!({"pitch_rev": false})), &mut ipc).unwrap().1["pitch_rev"], false);
    assert_eq!(sent.lock().unwrap().last().unwrap(), &json!({"evt": "axis", "pitch_rev": false}));
    handle("/api/axis", &Map::new(), &mut ipc).unwrap();
    assert_eq!(sent.lock().unwrap().last().unwrap(), &json!({"evt": "axis"}));
}

#[test]
fn test_no_daemon_is_said_plainly() {
    let (mut ipc, _) = fake(None);
    let (code, out) = handle("/api/move", &obj(json!({"pose": "up"})), &mut ipc).unwrap();
    assert!(code == 502 && out["error"].as_str().unwrap().contains("not reachable"));
    let (code, out) = handle("/api/axis", &Map::new(), &mut ipc).unwrap();
    assert!(code == 502 && out["error"].as_str().unwrap().contains("not reachable"));
}

/// Review A9 (2026-10-18): any web page could POST /api/axis (text/plain, no preflight) and flip the robot's
/// axes. learning/server.py's guard: the Host is this server's, and an Origin, when sent, is too.
#[test]
fn test_only_the_page_itself_may_call_the_api() {
    let ok = |pairs: &[(&str, &str)], port| allowed(&Headers::from_pairs(pairs.iter().copied()), port);
    assert!(ok(&[("Host", "127.0.0.1:8766")], 8766)); // curl, or the page's own GET
    assert!(ok(&[("Host", "127.0.0.1:8766"), ("Origin", "http://127.0.0.1:8766")], 8766));
    assert!(!ok(&[("Host", "127.0.0.1:8766"), ("Origin", "https://evil.example")], 8766));
    assert!(!ok(&[("Host", "evil.example:8766")], 8766)); // DNS rebinding
    assert!(!ok(&[], 8766));
    assert!(ok(&[("host", "127.0.0.1:8766")], 8766)); // header names ignore case
}

#[test]
fn page_is_the_python_page_verbatim() {
    let src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src/eve_bridge/head_cal.py"));
    let page = src.split_once("PAGE = \"\"\"").unwrap().1.split_once("\"\"\"").unwrap().0;
    assert_eq!(PAGE, page);
}

#[test]
fn move_body_values_follow_python_truthiness_and_str() {
    let (mut ipc, sent) = fake(Some(json!({"ok": 1, "error": "busy"})));
    let refused = |v: Value, ipc: &mut dyn FnMut(Value) -> Option<Value>| {
        handle("/api/move", &obj(json!({"pose": v})), ipc).unwrap().1["error"].clone()
    };
    assert_eq!(refused(json!(null), &mut ipc), "unknown pose ''");
    assert_eq!(refused(json!(0), &mut ipc), "unknown pose ''");
    assert_eq!(refused(json!(5), &mut ipc), "unknown pose '5'");
    assert_eq!(refused(json!(["up"]), &mut ipc), "unknown pose \"['up']\"");
    assert_eq!(refused(json!(true), &mut ipc), "unknown pose 'True'");
    assert_eq!(handle("/api/move", &Map::new(), &mut ipc).unwrap().1["error"], "unknown pose ''");
    assert!(sent.lock().unwrap().is_empty());
    // The daemon's ok is read by truthiness and its error passed through.
    assert_eq!(
        handle("/api/move", &obj(json!({"pose": "left"})), &mut ipc).unwrap(),
        (200, json!({"ok": true, "error": "busy", "pose": "left"}))
    );
    // Flip values are bool() of what was sent.
    handle("/api/axis", &obj(json!({"yaw_rev": "yes", "pitch_rev": 0, "other": 1})), &mut ipc).unwrap();
    assert_eq!(sent.lock().unwrap().last().unwrap(), &json!({"evt": "axis", "pitch_rev": false, "yaw_rev": true}));
    assert_eq!(handle("/api/nope", &Map::new(), &mut ipc).unwrap(), (404, json!({"ok": false, "error": "not found"})));
}

#[test]
fn a_move_reply_that_is_not_an_object_is_pythons_attribute_error() {
    let (mut ipc, _) = fake(Some(json!([1])));
    assert_eq!(handle("/api/move", &obj(json!({"pose": "up"})), &mut ipc), Err(HandleError::ReplyNotADict("list")));
    // /api/axis passes any reply through.
    assert_eq!(handle("/api/axis", &Map::new(), &mut ipc), Ok((200, json!([1]))));
}

#[test]
fn json_replies_are_ascii_like_python_json_dumps() {
    let v = json!({"error": "caf\u{e9} \u{2615} \u{1F600}\u{7f}", "n": 1.0});
    assert_eq!(dumps_ascii(&v), "{\"error\": \"caf\\u00e9 \\u2615 \\ud83d\\ude00\\u007f\", \"n\": 1.0}");
}

// ---------------------------------------------------------------------------
// HTTP

/// One request through `handle_connection`; returns what was written.
fn exchange(request: &[u8], ipc: &mut dyn FnMut(Value) -> Option<Value>) -> (Result<(), Crash>, Vec<u8>) {
    let mut reader = Cursor::new(request.to_vec());
    let mut out = Vec::new();
    let r = handle_connection(&mut reader, &mut out, 8766, ipc);
    (r, out)
}

fn split(resp: &[u8]) -> (String, String) {
    let text = String::from_utf8(resp.to_vec()).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or_else(|| panic!("no head in {text:?}"));
    (head.to_string(), body.to_string())
}

#[test]
fn get_serves_the_page_to_this_host_only() {
    let (mut ipc, sent) = fake(None);
    let (r, out) = exchange(b"GET / HTTP/1.1\r\nHost: 127.0.0.1:8766\r\n\r\n", &mut ipc);
    assert!(r.is_ok());
    let (head, body) = split(&out);
    let lines: Vec<&str> = head.split("\r\n").collect();
    assert_eq!(lines[0], "HTTP/1.0 200 OK");
    assert!(lines[1].starts_with("Server: BaseHTTP/0.6 "));
    assert!(lines[2].starts_with("Date: ") && lines[2].ends_with(" GMT"));
    assert_eq!(&lines[3..], ["Content-Type: text/html; charset=utf-8", "Cache-Control: no-store"]);
    assert_eq!(body, PAGE);

    let (_, out) = exchange(b"GET / HTTP/1.1\r\nHost: evil.example:8766\r\n\r\n", &mut ipc);
    let (head, body) = split(&out);
    assert!(head.starts_with("HTTP/1.0 403 Forbidden\r\n") && head.contains("Content-Type: application/json"));
    assert_eq!(body, r#"{"ok": false, "error": "local access only"}"#);
    assert!(sent.lock().unwrap().is_empty());
}

#[test]
fn post_drives_the_api_and_another_origin_is_refused() {
    let (mut ipc, sent) = fake(Some(json!({"ok": true})));
    let body = r#"{"pose": "up"}"#;
    let req = format!("POST /api/move HTTP/1.1\r\nHost: 127.0.0.1:8766\r\nOrigin: http://127.0.0.1:8766\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len());
    let (r, out) = exchange(req.as_bytes(), &mut ipc);
    assert!(r.is_ok());
    let (head, out) = split(&out);
    assert!(head.starts_with("HTTP/1.0 200 OK\r\n") && !head.contains("Content-Length"));
    assert_eq!(out, r#"{"ok": true, "error": null, "pose": "up"}"#);
    assert_eq!(sent.lock().unwrap().len(), 1);

    let req = "POST /api/axis HTTP/1.1\r\nHost: 127.0.0.1:8766\r\nOrigin: https://evil.example\r\nContent-Length: 18\r\n\r\n{\"pitch_rev\": true}";
    let (_, out) = exchange(req.as_bytes(), &mut ipc);
    assert!(split(&out).0.starts_with("HTTP/1.0 403 Forbidden"));
    assert_eq!(sent.lock().unwrap().len(), 1);

    // "//" at the start of the path reduces to one "/" (gh-87389), as in Python.
    let req = "POST //api/axis HTTP/1.1\r\nHost: 127.0.0.1:8766\r\n\r\n";
    let (_, out) = exchange(req.as_bytes(), &mut ipc);
    assert_eq!(split(&out).1, r#"{"ok": true}"#);
    assert_eq!(sent.lock().unwrap().last().unwrap(), &json!({"evt": "axis"}));
}

#[test]
fn a_bad_body_is_an_empty_one_and_a_bad_length_is_a_crash() {
    let (mut ipc, sent) = fake(Some(json!({"ok": true})));
    for body in ["not json", "[1, 2]", ""] {
        let req = format!("POST /api/move HTTP/1.1\r\nHost: 127.0.0.1:8766\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        let (_, out) = exchange(req.as_bytes(), &mut ipc);
        let (head, out) = split(&out);
        assert!(head.starts_with("HTTP/1.0 400 Bad Request\r\n"), "{head}");
        assert_eq!(out, r#"{"ok": false, "error": "unknown pose ''"}"#);
    }
    // UTF-16 with a BOM decodes, as json.loads(bytes) does.
    let mut body = vec![0xff, 0xfe];
    for u in r#"{"pose": "up"}"#.encode_utf16() {
        body.extend(u.to_le_bytes());
    }
    let mut req = format!("POST /api/move HTTP/1.1\r\nHost: 127.0.0.1:8766\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
    req.extend(&body);
    let (_, out) = exchange(&req, &mut ipc);
    assert_eq!(split(&out).1, r#"{"ok": true, "error": null, "pose": "up"}"#);
    // int("x") raises in do_POST: no reply at all.
    let (r, out) = exchange(b"POST /api/move HTTP/1.1\r\nHost: 127.0.0.1:8766\r\nContent-Length: x\r\n\r\n", &mut ipc);
    assert!(r.unwrap_err().0.starts_with("ValueError: invalid literal for int()"));
    assert!(out.is_empty());
    assert_eq!(sent.lock().unwrap().len(), 1);
}

#[test]
fn a_move_reply_that_is_not_an_object_closes_without_a_reply() {
    let (mut ipc, _) = fake(Some(json!("yes")));
    let body = r#"{"pose": "up"}"#;
    let req = format!("POST /api/move HTTP/1.0\r\nHost: 127.0.0.1:8766\r\nContent-Length: {}\r\n\r\n{body}", body.len());
    let (r, out) = exchange(req.as_bytes(), &mut ipc);
    assert_eq!(r.unwrap_err().0, "AttributeError: 'str' object has no attribute 'get'");
    assert!(out.is_empty());
}

#[test]
fn other_methods_and_bad_requests_get_pythons_error_pages() {
    let (mut ipc, _) = fake(None);
    let (_, out) = exchange(b"PUT / HTTP/1.1\r\nHost: 127.0.0.1:8766\r\n\r\n", &mut ipc);
    let (head, body) = split(&out);
    assert!(head.starts_with("HTTP/1.0 501 Unsupported method ('PUT')\r\n"), "{head}");
    assert!(head.contains("\r\nConnection: close\r\nContent-Type: text/html;charset=utf-8\r\nContent-Length: "));
    assert!(body.contains("<p>Message: Unsupported method (&#x27;PUT&#x27;).</p>") || body.contains("<p>Message: Unsupported method ('PUT').</p>"));
    assert!(body.contains("<p>Error code explanation: 501 - Server does not support this operation.</p>"));
    let len: usize = head.split("Content-Length: ").nth(1).unwrap().trim().parse().unwrap();
    assert_eq!(len, body.len());

    // HEAD has no handler either, and gets no body.
    let (_, out) = exchange(b"HEAD / HTTP/1.1\r\n\r\n", &mut ipc);
    assert!(split(&out).1.is_empty());

    // 505 is sent before the version is taken: body only, as Python does.
    let (_, out) = exchange(b"GET / HTTP/2.0\r\n\r\n", &mut ipc);
    let out = String::from_utf8(out).unwrap();
    assert!(out.starts_with("<!DOCTYPE HTML>") && out.contains("<p>Message: Invalid HTTP version (2.0).</p>"));

    // An HTTP/0.9 request gets the body alone; it has no Host, so it is refused.
    let (_, out) = exchange(b"GET /\r\n", &mut ipc);
    assert_eq!(out, br#"{"ok": false, "error": "local access only"}"#);
    // A bad version is answered before the version is known: body only, as Python does.
    let (_, out) = exchange(b"GET / FTP/1.0\r\n\r\n", &mut ipc);
    assert!(String::from_utf8(out).unwrap().starts_with("<!DOCTYPE HTML>"));

    let (r, out) = exchange(b"", &mut ipc);
    assert!(r.is_ok() && out.is_empty());

    let mut long = b"GET /".to_vec();
    long.extend(std::iter::repeat_n(b'a', 70000));
    let (_, out) = exchange(&long, &mut ipc);
    assert!(split(&out).0.starts_with("HTTP/1.0 414 URI Too Long\r\n"));

    let mut many = b"GET / HTTP/1.1\r\n".to_vec();
    for i in 0..100 {
        many.extend(format!("X-{i}: y\r\n").bytes());
    }
    many.extend(b"\r\n");
    let (_, out) = exchange(&many, &mut ipc);
    let (head, body) = split(&out);
    assert!(head.starts_with("HTTP/1.0 431 Too many headers\r\n"));
    assert!(body.contains("got more than 100 headers"));
}

#[test]
fn header_parsing_follows_the_email_parser() {
    let h = parse_header_lines(&[
        b"Host:   127.0.0.1:8766\r\n".to_vec(),
        b"Origin: http://a\r\n".to_vec(),
        b"Origin: http://b\r\n".to_vec(),
        b"\r\n".to_vec(),
    ]);
    assert_eq!(h.get("host"), Some("127.0.0.1:8766"));
    assert_eq!(h.get("Origin"), Some("http://a"));
    // A line that is not a header ends the block.
    let h = parse_header_lines(&[b"Bad Line\r\n".to_vec(), b"Host: x\r\n".to_vec(), b"\r\n".to_vec()]);
    assert_eq!(h.get("Host"), None);
    // A continuation line is kept in the value, so it no longer matches.
    let h = parse_header_lines(&[b"Host: 127.0.0.1:8766\r\n".to_vec(), b" more\r\n".to_vec(), b"\r\n".to_vec()]);
    assert_eq!(h.get("Host"), Some("127.0.0.1:8766\r\n more"));
    // Trailing spaces stay, as in compat32.
    let h = parse_header_lines(&[b"Host: 127.0.0.1:8766 \r\n".to_vec(), b"\r\n".to_vec()]);
    assert!(!allowed(&h, 8766));
}

#[test]
fn content_length_is_read_like_python_int() {
    assert_eq!(py_int(" 12 "), Some(12));
    assert_eq!(py_int("+1_0"), Some(10));
    assert_eq!(py_int("-1"), Some(-1));
    assert_eq!(py_int("1__0"), None);
    assert_eq!(py_int("_1"), None);
    assert_eq!(py_int("1.0"), None);
    assert_eq!(py_int(""), None);
}

#[test]
fn the_server_answers_over_tcp_until_shut_down() {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let sent = Arc::new(Mutex::new(Vec::new()));
    let log = sent.clone();
    let ipc: SharedIpc = Arc::new(move |req: Value| {
        log.lock().unwrap().push(req);
        Some(json!({"ok": true, "pitch_rev": true, "yaw_rev": false}))
    });
    let listener = rt.block_on(tokio::net::TcpListener::bind(("127.0.0.1", 0))).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = rt.spawn(run(listener, port, ipc, async {
        let _ = stopped.await;
    }));
    let body = r#"{"pitch_rev": true}"#;
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(s, "POST /api/axis HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap(); // the server closes after one request
    assert!(out.starts_with("HTTP/1.0 200 OK\r\n"));
    assert!(out.ends_with("\r\n\r\n{\"ok\": true, \"pitch_rev\": true, \"yaw_rev\": false}"));
    assert_eq!(sent.lock().unwrap().as_slice(), &[json!({"evt": "axis", "pitch_rev": true})]);
    stop.send(()).unwrap();
    rt.block_on(server).unwrap();
    rt.shutdown_background();
}

#[cfg(unix)]
#[test]
fn the_daemon_is_reached_over_its_socket() {
    use std::io::BufRead as _;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("eve.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let daemon = std::thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        let mut line = String::new();
        std::io::BufReader::new(&s).read_line(&mut line).unwrap();
        (&s).write_all(b"{\"ok\": true, \"pitch_rev\": false, \"yaw_rev\": true}\n").unwrap();
        line
    });
    let ipc = daemon_ipc(Some(path.to_str().unwrap()));
    assert_eq!(ipc(json!({"evt": "axis"})), Some(json!({"ok": true, "pitch_rev": false, "yaw_rev": true})));
    assert_eq!(daemon.join().unwrap(), "{\"evt\": \"axis\"}\n");
    // Nobody listening: None, which the page reports as 502.
    let gone = daemon_ipc(Some(dir.path().join("none.sock").to_str().unwrap()));
    assert_eq!(gone(json!({"evt": "axis"})), None);
}
