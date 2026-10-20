use super::*;
use serde_json::json;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};

// ---------------------------------------------------------------------------
// bridge/tests/test_ipc_transport.py

#[test]
fn test_parse_tcp_loopback_spec() {
    let transport = parse_spec("127.0.0.1:48765").unwrap();
    assert!(matches!(transport, Transport::Tcp(_)));
    assert_eq!(transport.address(), "127.0.0.1:48765");
}

#[test]
fn test_parse_tcp_empty_host_uses_loopback() {
    let transport = parse_spec(":48765").unwrap();
    assert!(matches!(transport, Transport::Tcp(_)));
    assert_eq!(transport.address(), "127.0.0.1:48765");
}

#[test]
fn test_parse_tcp_rejects_non_loopback_host() {
    let err = parse_spec("0.0.0.0:48765").unwrap_err();
    assert!(err.to_string().contains("loopback"));
    assert_eq!(err.python_type(), "ValueError");
    assert_eq!(
        err.to_string(),
        "TCP IPC must bind to loopback only, got '0.0.0.0'. Use '127.0.0.1:48765' or ':48765'."
    );
}

#[test]
fn test_windows_default_is_tcp() {
    assert_eq!(default_spec_on(Platform::Windows), "127.0.0.1:48765");
}

#[test]
fn test_windows_rejects_path_spec() {
    let err = parse_spec_on(r"C:\tmp\eve-bridge.sock", Platform::Windows).unwrap_err();
    assert!(err.to_string().contains("Unix socket paths"));
    assert_eq!(
        err.to_string(),
        r"Unix socket paths are not supported on Windows: 'C:\\tmp\\eve-bridge.sock'. Use a TCP spec like '127.0.0.1:48765' or ':48765'."
    );
}

#[test]
fn test_make_transport_honors_env() {
    let transport = make_transport_from(None, Some(":48766"), Platform::Posix).unwrap();
    assert!(matches!(transport, Transport::Tcp(_)));
    assert_eq!(transport.address(), "127.0.0.1:48766");
}

#[test]
fn test_posix_default_is_unix_socket() {
    assert_eq!(default_spec_on(Platform::Posix), "/tmp/eve-bridge.sock");
}

/// The hook client's `post` returns None when `make_transport` raises; the
/// part this crate owns is that the spec is refused.
#[test]
fn test_hook_post_returns_none_for_invalid_transport_spec() {
    assert!(make_transport(Some("0.0.0.0:48765")).is_err());
}

// ---------------------------------------------------------------------------
// Spec parsing, beyond the Python tests

#[test]
fn test_spec_edge_cases_follow_the_python_branches() {
    assert_eq!(parse_spec_on("", Platform::Posix).unwrap_err(), SpecError::Empty);
    assert_eq!(parse_spec_on(":0", Platform::Posix).unwrap_err().to_string(), "port out of range: 0");
    assert_eq!(parse_spec_on(":65536", Platform::Posix).unwrap_err().to_string(), "port out of range: 65536");
    assert_eq!(
        parse_spec_on(":00099999999999999999999", Platform::Posix).unwrap_err().to_string(),
        "port out of range: 99999999999999999999"
    );
    assert_eq!(parse_spec_on(":080", Platform::Posix).unwrap().address(), "127.0.0.1:80");
    assert_eq!(parse_spec_on("localhost:9", Platform::Posix).unwrap().address(), "localhost:9");
    // A path, not TCP: slash in the host, a non-numeric port, a backslash anywhere, a drive letter.
    for spec in ["/tmp/a:80", "/tmp/eve-bridge.sock", "host:12ab", "C:/x/y.sock", "host:"] {
        match parse_spec_on(spec, Platform::Posix).unwrap() {
            Transport::Unix(t) => assert_eq!(t.path, spec),
            other => panic!("{spec} parsed as {other:?}"),
        }
    }
    assert!(matches!(parse_spec_on("a\\b:80", Platform::Posix).unwrap(), Transport::Unix(_)));
    assert!(matches!(parse_spec_on("[::1]:80", Platform::Posix), Err(SpecError::NotLoopback { .. })));
    assert!(matches!(parse_spec_on(":48765", Platform::Windows).unwrap(), Transport::Tcp(_)));
}

#[test]
fn test_make_transport_order_is_spec_then_env_then_default() {
    let t = make_transport_from(Some("/tmp/x.sock"), Some(":48766"), Platform::Posix).unwrap();
    assert_eq!(t.address(), "/tmp/x.sock");
    let t = make_transport_from(Some(""), Some(":48766"), Platform::Posix).unwrap();
    assert_eq!(t.address(), "127.0.0.1:48766");
    let t = make_transport_from(None, Some(""), Platform::Posix).unwrap();
    assert_eq!(t.address(), DEFAULT_UNIX_PATH);
    let t = make_transport_from(None, None, Platform::Windows).unwrap();
    assert_eq!(t.address(), "127.0.0.1:48765");
}

#[test]
fn test_unix_transport_refused_on_windows() {
    let err = UnixTransport::new_on("/tmp/x", Platform::Windows).unwrap_err();
    assert_eq!(err.python_type(), "RuntimeError");
    assert_eq!(
        err.to_string(),
        "UnixTransport is not supported on Windows. Use a TCP spec like '127.0.0.1:48765' instead."
    );
    assert!(TcpLoopbackTransport::new("", 5).unwrap().address() == "127.0.0.1:5");
}

// ---------------------------------------------------------------------------
// Python-compatible text

#[test]
fn test_dumps_matches_python_json_dumps() {
    let v = json!({"ok": true, "a": [1, 2.5, null], "s": "héllo \"q\"\n\u{1}", "e": {}, "l": []});
    assert_eq!(dumps(&v), r#"{"ok": true, "a": [1, 2.5, null], "s": "héllo \"q\"\n\u0001", "e": {}, "l": []}"#);
    assert_eq!(dumps(&json!([1.0, 1e16, 1e-5, 123456.789, 0.0001, -2.0])), "[1.0, 1e+16, 1e-05, 123456.789, 0.0001, -2.0]");
}

#[test]
fn test_py_float_repr() {
    for (f, want) in [
        (1.0, "1.0"),
        (0.1, "0.1"),
        (1e22, "1e+22"),
        (1.5e-7, "1.5e-07"),
        (9999999999999998.0, "9999999999999998.0"),
        (123.0, "123.0"),
        (-0.0, "-0.0"),
        (1.2345e-4, "0.00012345"),
        (2.5e300, "2.5e+300"),
    ] {
        assert_eq!(py_float_repr(f), want, "{f}");
    }
}

#[test]
fn test_py_repr() {
    assert_eq!(py_repr(&json!({"evt": "nope", "n": [1, true, null]})), "{'evt': 'nope', 'n': [1, True, None]}");
    assert_eq!(py_str_repr("it's"), "\"it's\"");
    assert_eq!(py_str_repr("a\\b\t\u{7f}\u{a0}é"), "'a\\\\b\\t\\x7f\\xa0é'");
}

#[test]
fn test_utf8_errors_read_like_python() {
    let msg = |b: &[u8]| decode_request(b).unwrap_err();
    assert_eq!(msg(b"\xff\n"), "'utf-8' codec can't decode byte 0xff in position 0: invalid start byte");
    assert_eq!(msg(b"{\xe2\x41}\n"), "'utf-8' codec can't decode byte 0xe2 in position 1: invalid continuation byte");
    assert_eq!(msg(b"\xe2\x82\x41\n"), "'utf-8' codec can't decode bytes in position 0-1: invalid continuation byte");
    assert_eq!(msg(b"ab\xe2\x82"), "'utf-8' codec can't decode bytes in position 2-3: unexpected end of data");
}

// ---------------------------------------------------------------------------
// The server, driven over an in-memory pipe

fn echo() -> Handler {
    handler(|req: Value| async move { Ok(json!({"ok": true, "echo": req.get("evt").cloned().unwrap_or(Value::Null)})) })
}

async fn exchange(h: Handler, request: &[u8]) -> Vec<u8> {
    let (mut client, server) = tokio::io::duplex(1 << 20);
    let task = tokio::spawn(handle_conn(h, server));
    client.write_all(request).await.unwrap();
    client.shutdown().await.unwrap();
    let mut out = Vec::new();
    client.read_to_end(&mut out).await.unwrap();
    task.await.unwrap();
    out
}

#[tokio::test]
async fn test_one_line_one_reply() {
    let out = exchange(echo(), b"{\"evt\":\"mcp_tools\"}\n{\"evt\":\"second\"}\n").await;
    assert_eq!(out, b"{\"ok\": true, \"echo\": \"mcp_tools\"}\n");
}

#[tokio::test]
async fn test_a_line_without_newline_is_read_at_eof() {
    let out = exchange(echo(), b"{\"evt\":\"x\"}").await;
    assert_eq!(out, b"{\"ok\": true, \"echo\": \"x\"}\n");
}

#[tokio::test]
async fn test_an_empty_connection_gets_no_reply() {
    assert!(exchange(echo(), b"").await.is_empty());
}

#[tokio::test]
async fn test_bad_json_is_refused() {
    let out = exchange(echo(), b"{\"ok\": tr\n").await;
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["ok"], json!(false));
    assert!(v["error"].as_str().unwrap().starts_with("bad json: "), "{v}");
    let out = exchange(echo(), b"\xff\n").await;
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "{\"ok\": false, \"error\": \"bad json: 'utf-8' codec can't decode byte 0xff in position 0: invalid start byte\"}\n"
    );
}

#[tokio::test]
async fn test_a_non_object_request_still_reaches_the_handler() {
    let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let s = seen.clone();
    let h = handler(move |req: Value| {
        let s = s.clone();
        async move {
            s.lock().push(req);
            Ok(json!({"ok": true}))
        }
    });
    exchange(h, b"[1, 2]\n").await;
    assert_eq!(*seen.lock(), vec![json!([1, 2])]);
}

#[tokio::test]
async fn test_handler_faults_become_an_error_reply() {
    let h = handler(|_req: Value| async move { Err::<Value, _>(NamedError::new("KeyError", "'session_id'").into()) });
    assert_eq!(exchange(h, b"{}\n").await, b"{\"ok\": false, \"error\": \"KeyError: 'session_id'\"}\n");
    let h = handler(|_req: Value| async move { Err::<Value, _>(anyhow::anyhow!("boom")) });
    assert_eq!(exchange(h, b"{}\n").await, b"{\"ok\": false, \"error\": \"Exception: boom\"}\n");
    let h: Handler = Arc::new(|_req: Value| async move { panic!("kaput") }.boxed());
    assert_eq!(exchange(h, b"{}\n").await, b"{\"ok\": false, \"error\": \"Exception: kaput\"}\n");
}

#[tokio::test]
async fn test_a_line_over_the_reader_limit_gets_no_reply() {
    // The newline may sit at index READ_LIMIT exactly; one byte further is refused.
    let body = format!("\"{}\"", "x".repeat(READ_LIMIT - 2));
    assert_eq!(body.len(), READ_LIMIT);
    let ok = exchange(echo(), format!("{body}\n").as_bytes()).await;
    assert_eq!(ok, b"{\"ok\": true, \"echo\": null}\n");
    let long = format!("\"{}\"\n", "x".repeat(READ_LIMIT - 1));
    assert!(exchange(echo(), long.as_bytes()).await.is_empty());
    let no_newline = format!("\"{}\"", "x".repeat(READ_LIMIT));
    assert!(exchange(echo(), no_newline.as_bytes()).await.is_empty());
}

// ---------------------------------------------------------------------------
// The server over real sockets (test_eve_mcp / test_cli_lite use it this way)

fn short_dir() -> tempfile::TempDir {
    // macOS keeps a socket's path in 104 bytes; the default temp dir is longer.
    tempfile::Builder::new().prefix("evi").tempdir_in("/tmp").unwrap()
}

fn post_sync(t: &Transport, request: &[u8], timeout: Duration) -> io::Result<Vec<u8>> {
    let mut s = t.sync_connect(timeout)?;
    s.write_all(request)?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = s.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.contains(&b'\n') {
            break;
        }
    }
    Ok(buf)
}

async fn post(t: &Transport, request: &'static [u8], timeout: Duration) -> io::Result<Vec<u8>> {
    let t = t.clone();
    tokio::task::spawn_blocking(move || post_sync(&t, request, timeout)).await.unwrap()
}

fn slow_echo() -> Handler {
    handler(|req: Value| async move {
        if req.get("slow").is_some() {
            tokio::time::sleep(Duration::from_millis(600)).await;
        }
        Ok(json!({"ok": true, "echo": req.get("evt").cloned().unwrap_or(Value::Null)}))
    })
}

#[cfg(unix)]
#[tokio::test]
async fn test_post_over_a_real_socket_and_its_two_failures() {
    let dir = short_dir();
    let path = dir.path().join("s.sock").display().to_string();
    let server = IpcServer::new(slow_echo(), Some(&path)).unwrap();
    assert_eq!(server.address(), path);
    server.start().await.unwrap();
    let t = server.transport().clone();
    let got = post(&t, b"{\"evt\": \"mcp_tools\"}\n", Duration::from_secs(5)).await.unwrap();
    assert_eq!(got, b"{\"ok\": true, \"echo\": \"mcp_tools\"}\n");
    // Unanswered: the read times out.
    let err = post(&t, b"{\"evt\": \"x\", \"slow\": true}\n", Duration::from_millis(200)).await.unwrap_err();
    assert!(matches!(err.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut), "{err:?}");
    server.stop().await;
    assert!(!Path::new(&path).exists());
    // Unreachable: nothing at the path.
    let none = parse_spec(&dir.path().join("none.sock").display().to_string()).unwrap();
    let err = post(&none, b"{}\n", Duration::from_secs(1)).await.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
}

#[cfg(unix)]
#[tokio::test]
async fn test_unix_socket_is_owner_only_and_replaces_a_stale_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = short_dir();
    let path = dir.path().join("ipc.sock");
    std::fs::write(&path, b"stale").unwrap();
    let server = IpcServer::new(echo(), Some(path.to_str().unwrap())).unwrap();
    server.start().await.unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert!(server.transport().is_in_use());
    server.stop().await;
    assert!(!path.exists());
    assert!(!server.transport().is_in_use());
}

#[cfg(unix)]
#[test]
fn test_is_in_use_removes_a_socket_nobody_accepts_on() {
    let dir = short_dir();
    let path = dir.path().join("dead.sock");
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
    assert!(path.exists());
    let t = UnixTransport::new(path.to_str().unwrap()).unwrap();
    assert!(!t.is_in_use());
    assert!(!path.exists());
    assert!(!t.is_in_use());
}

#[cfg(unix)]
#[tokio::test]
async fn test_stop_waits_for_a_request_in_flight() {
    let dir = short_dir();
    let path = dir.path().join("s.sock").display().to_string();
    let server = Arc::new(IpcServer::new(slow_echo(), Some(&path)).unwrap());
    server.start().await.unwrap();
    let t = server.transport().clone();
    let client = tokio::spawn(async move { post(&t, b"{\"evt\": \"late\", \"slow\": 1}\n", Duration::from_secs(5)).await });
    tokio::time::sleep(Duration::from_millis(150)).await;
    let started = std::time::Instant::now();
    server.stop().await;
    assert!(started.elapsed() >= Duration::from_millis(300), "stop returned before the handler finished");
    assert_eq!(client.await.unwrap().unwrap(), b"{\"ok\": true, \"echo\": \"late\"}\n");
    // No longer accepting.
    let err = post(server.transport(), b"{}\n", Duration::from_secs(1)).await.unwrap_err();
    assert!(matches!(err.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused), "{err:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn test_a_client_that_hangs_up_does_not_stop_the_server() {
    let dir = short_dir();
    let path = dir.path().join("s.sock").display().to_string();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let h = handler(move |_req: Value| {
        let c = c.clone();
        async move {
            c.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok(json!({"ok": true, "pad": "y".repeat(200_000)}))
        }
    });
    let server = IpcServer::new(h, Some(&path)).unwrap();
    server.start().await.unwrap();
    let t = server.transport().clone();
    let t2 = t.clone();
    tokio::task::spawn_blocking(move || {
        let mut s = t2.sync_connect(Duration::from_secs(2)).unwrap();
        s.write_all(b"{}\n").unwrap();
        drop(s);
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let got = post(&t, b"{}\n", Duration::from_secs(5)).await.unwrap();
    assert!(got.starts_with(b"{\"ok\": true"));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    server.stop().await;
}

#[tokio::test]
async fn test_tcp_loopback_round_trip() {
    let transport = Transport::Tcp(TcpLoopbackTransport::new("127.0.0.1", 0).unwrap());
    let server = IpcServer::with_transport(echo(), transport);
    server.start().await.unwrap();
    let bound = server.server().unwrap().addresses()[0].clone();
    let port: u16 = bound.rsplit(':').next().unwrap().parse().unwrap();
    let t = parse_spec(&format!(":{port}")).unwrap();
    assert!(t.is_in_use());
    let got = post(&t, b"{\"evt\": \"hi\"}\n", Duration::from_secs(5)).await.unwrap();
    assert_eq!(got, b"{\"ok\": true, \"echo\": \"hi\"}\n");
    server.stop().await;
    assert!(!t.is_in_use());
}

#[cfg(unix)]
#[tokio::test]
async fn test_serve_forever_needs_start_and_ends_on_stop() {
    let dir = short_dir();
    let path = dir.path().join("s.sock").display().to_string();
    let server = Arc::new(IpcServer::new(echo(), Some(&path)).unwrap());
    assert!(server.serve_forever().await.is_err());
    server.start().await.unwrap();
    let s = server.clone();
    let forever = tokio::spawn(async move { s.serve_forever().await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!forever.is_finished());
    server.stop().await;
    tokio::time::timeout(Duration::from_secs(2), forever).await.unwrap().unwrap().unwrap();
}
