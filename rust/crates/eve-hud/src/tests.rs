//! Port of `bridge/tests/test_hud.py` (pure formatting, no IPC), plus tests
//! for the IPC query and the Python edge cases.

use super::*;
use serde_json::json;

fn fl(state: Value) -> String {
    format_line(state.as_object(), false)
}

fn fla(state: Value) -> String {
    format_line(state.as_object(), true)
}

#[test]
fn test_format_none_state() {
    assert_eq!(format_line(None, false), "🐾 off");
    assert_eq!(format_line(None, true), "eve: off");
}

#[test]
fn test_format_disconnected() {
    let state = json!({"ble_connected": false});
    assert_eq!(fl(state.clone()), "🐾 ∅");
    assert_eq!(fla(state), "eve: disc");
}

#[test]
fn test_format_full_state() {
    let state = json!({
        "ble_connected": true,
        "sec": true,
        "battery_pct": 96,
        "running": 2,
        "total": 2,
    });
    let out = fl(state.clone());
    assert!(out.contains("96%"));
    assert!(out.contains(BAR_FULL)); // the battery bar rendered
    assert!(out.contains("🔒"));
    assert!(out.contains("2run"));
    let ascii_out = fla(state);
    assert!(ascii_out.contains("96%"));
    assert!(ascii_out.contains('[') && ascii_out.contains(']'));
    assert!(ascii_out.contains("lock"));
    assert!(ascii_out.contains("2run"));
}

#[test]
fn test_format_low_battery_icon() {
    let state = json!({"ble_connected": true, "sec": true, "battery_pct": 10});
    assert!(fl(state.clone()).contains("🪫"));
    // Low battery should be coloured red via ANSI.
    assert!(fl(state).contains("\x1b[31m"));
}

#[test]
fn test_bar_rendering_endpoints() {
    assert_eq!(bar(0, BAR_WIDTH), BAR_EMPTY.repeat(BAR_WIDTH));
    assert_eq!(bar(100, BAR_WIDTH), BAR_FULL.repeat(BAR_WIDTH));
}

#[test]
fn test_bar_rendering_middle() {
    // 50% should be roughly half full.
    let b = bar(50, BAR_WIDTH);
    assert_eq!(b.matches(BAR_FULL).count(), BAR_WIDTH / 2);
    assert_eq!(b.matches(BAR_EMPTY).count(), BAR_WIDTH / 2);
}

#[test]
fn test_bar_clamps_out_of_range() {
    assert_eq!(bar(-10, BAR_WIDTH), BAR_EMPTY.repeat(BAR_WIDTH));
    assert_eq!(bar(200, BAR_WIDTH), BAR_FULL.repeat(BAR_WIDTH));
}

#[test]
fn test_ascii_bar_uses_ascii_chars_only() {
    let state = json!({"ble_connected": true, "sec": true, "battery_pct": 60});
    let ascii_out = fla(state);
    assert!(!ascii_out.contains(BAR_FULL));
    assert!(!ascii_out.contains(BAR_EMPTY));
    assert!(ascii_out.contains('=') || ascii_out.contains('-'));
}

#[test]
fn test_format_unencrypted_warns() {
    let state = json!({"ble_connected": true, "sec": false, "battery_pct": 80});
    assert!(fl(state).contains("UNSEC"));
}

#[test]
fn test_format_no_running_omits_count() {
    let state = json!({"ble_connected": true, "sec": true, "battery_pct": 80, "running": 0});
    assert!(!fl(state).contains("run"));
}

#[test]
fn test_format_cost_shows_when_above_one_cent() {
    let state = json!({"ble_connected": true, "sec": true, "battery_pct": 80, "cost_today": 0.42});
    assert!(fl(state.clone()).contains("$0.42"));
    assert!(fla(state).contains("$0.42"));
}

#[test]
fn test_format_cost_hidden_below_one_cent() {
    let state = json!({"ble_connected": true, "sec": true, "battery_pct": 80, "cost_today": 0.004});
    assert!(!fl(state).contains('$'));
}

#[test]
fn test_format_cost_two_decimals_for_double_digits() {
    let state = json!({"ble_connected": true, "sec": true, "battery_pct": 80, "cost_today": 12.5});
    assert!(fl(state).contains("$12.50"));
}

#[test]
fn test_token_formatter_thresholds() {
    assert_eq!(format_tokens(0), "0");
    assert_eq!(format_tokens(999), "999");
    assert_eq!(format_tokens(1_000), "1.0K");
    assert_eq!(format_tokens(12_345), "12.3K");
    assert_eq!(format_tokens(99_999), "100.0K"); // right at the 100K boundary
    assert_eq!(format_tokens(100_000), "100K"); // past it, drop the decimal
    assert_eq!(format_tokens(850_000), "850K");
    assert_eq!(format_tokens(999_999), "999K");
    assert_eq!(format_tokens(1_000_000), "1.0M");
    assert_eq!(format_tokens(1_234_567), "1.2M");
}

#[test]
fn test_format_tokens_shown_when_above_1k() {
    let state = json!({"ble_connected": true, "sec": true, "battery_pct": 80, "tokens_today": 12_345});
    assert!(fl(state).contains("12.3K"));
}

#[test]
fn test_format_tokens_hidden_below_1k() {
    let state = json!({"ble_connected": true, "sec": true, "battery_pct": 80, "tokens_today": 850});
    // Don't render single-figure token counts — the line is for at-a-glance.
    assert!(!fl(state).contains("850"));
}

#[test]
fn test_format_update_segment_appears_at_end() {
    let state = json!({
        "ble_connected": true, "sec": true, "battery_pct": 80,
        "update_available": "v0.1.1",
    });
    let out = fl(state);
    assert!(out.contains("↑ v0.1.1"));
    // New segment renders at the end of the line.
    assert!(out.trim_end().ends_with("v0.1.1\x1b[0m") || out.trim_end().ends_with("v0.1.1"));
}

#[test]
fn test_format_update_segment_ascii() {
    let state = json!({
        "ble_connected": true, "sec": true, "battery_pct": 80,
        "update_available": "v0.1.1",
    });
    let out = fla(state);
    assert!(out.contains("up v0.1.1"));
    assert!(!out.contains("\x1b["));
}

#[test]
fn test_format_no_update_omits_segment() {
    let state = json!({
        "ble_connected": true, "sec": true, "battery_pct": 80,
        "update_available": null,
    });
    assert!(!fl(state).contains('↑'));
}

#[test]
fn test_format_tokens_and_cost_appear_together() {
    let state = json!({
        "ble_connected": true, "sec": true, "battery_pct": 80,
        "tokens_today": 1_234_567, "cost_today": 4.20,
    });
    let out = fl(state);
    // Tokens render before cost (input → derived spend reads naturally L→R).
    assert!(out.find("1.2M").unwrap() < out.find("$4.20").unwrap());
}

// ---- Exact lines, checked against the Python `format_line` output ----

#[test]
fn test_exact_lines_match_python() {
    let state = json!({
        "ble_connected": true, "sec": true, "battery_pct": 96, "running": 2,
        "tokens_today": 12_345, "cost_today": 0.42, "update_available": "v0.1.1",
    });
    assert_eq!(
        fl(state.clone()),
        "🐾 🔋 \x1b[32m████████\x1b[0m 96% 🔒 12.3K $0.42 2run \x1b[33m↑ v0.1.1\x1b[0m"
    );
    assert_eq!(fla(state), "eve: [========] 96% lock 12.3K $0.42 2run up v0.1.1");
    let state = json!({"ble_connected": true, "sec": false, "battery_pct": 30});
    assert_eq!(fl(state.clone()), "🐾 🔋 \x1b[33m██░░░░░░\x1b[0m 30% ⚠UNSEC");
    assert_eq!(fla(state), "eve: [==------] 30% UNSEC");
    // Connected with nothing to say.
    assert_eq!(fl(json!({"ble_connected": true})), "🐾 ok");
    assert_eq!(fla(json!({"ble_connected": true})), "eve: ok");
    // An empty dict is a state, not "off": it reads as disconnected.
    assert_eq!(fl(json!({})), "🐾 ∅");
}

#[test]
fn test_python_type_edges() {
    // bool is an int in Python: True renders as a 1% battery reading "True%".
    assert_eq!(fla(json!({"ble_connected": 1, "battery_pct": true})), "eve: [--------] True%");
    // A float battery is not an int, so no battery segment.
    assert_eq!(fla(json!({"ble_connected": true, "battery_pct": 96.0})), "eve: ok");
    // sec must be a real bool; a truthy 1 is omitted.
    assert_eq!(fla(json!({"ble_connected": true, "sec": 1})), "eve: ok");
    // cost: an int counts, and True formats as 1.00.
    assert_eq!(fla(json!({"ble_connected": true, "cost_today": 3})), "eve: $3.00");
    assert_eq!(fla(json!({"ble_connected": true, "cost_today": true})), "eve: $1.00");
    // running is printed with str(): floats keep their repr, strings are raw.
    assert_eq!(fla(json!({"ble_connected": true, "running": 2.0})), "eve: 2.0run");
    assert_eq!(fla(json!({"ble_connected": true, "running": "x"})), "eve: xrun");
    assert_eq!(fla(json!({"ble_connected": true, "running": ""})), "eve: ok");
    // A non-string update is ignored, as is an empty one.
    assert_eq!(fla(json!({"ble_connected": true, "update_available": 1})), "eve: ok");
    assert_eq!(fla(json!({"ble_connected": true, "update_available": ""})), "eve: ok");
    // Battery boundaries: red at 15, yellow at 40, green above.
    assert!(fl(json!({"ble_connected": true, "battery_pct": 15})).contains("🪫 \x1b[31m"));
    assert!(fl(json!({"ble_connected": true, "battery_pct": 16})).contains("🔋 \x1b[33m"));
    assert!(fl(json!({"ble_connected": true, "battery_pct": 41})).contains("🔋 \x1b[32m"));
    // Out of range still prints the raw number.
    assert_eq!(fla(json!({"ble_connected": true, "battery_pct": 250})), "eve: [========] 250%");
}

#[test]
fn test_format_tokens_rounds_like_python() {
    // Python's f"{x:.1f}" rounds the binary value half-to-even: 12.25 → 12.2.
    assert_eq!(format_tokens(12_250), "12.2K");
    assert_eq!(format_tokens(12_350), "12.3K");
    assert_eq!(format_tokens(2_500_000_000), "2500.0M");
}

#[test]
fn test_format_state_only_formats_a_dict() {
    assert_eq!(format_state(None, true), "eve: off");
    assert_eq!(format_state(Some(&json!([1])), true), "eve: off");
    assert_eq!(format_state(Some(&json!({"ble_connected": true})), true), "eve: ok");
}

// ---- The IPC query against a real eve-ipc server ----

#[cfg(unix)]
mod ipc {
    use super::*;
    use eve_ipc::{handler, IpcServer};

    fn short_dir() -> tempfile::TempDir {
        tempfile::Builder::new().prefix("hud").tempdir_in("/tmp").unwrap()
    }

    async fn query(path: String) -> Option<Value> {
        tokio::task::spawn_blocking(move || query_state(Some(&path), QUERY_TIMEOUT)).await.unwrap()
    }

    async fn serve(reply: Value) -> (tempfile::TempDir, String, IpcServer, std::sync::Arc<parking_lot::Mutex<Vec<Value>>>) {
        let dir = short_dir();
        let path = dir.path().join("s.sock").display().to_string();
        let seen = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let s = seen.clone();
        let h = handler(move |req: Value| {
            let s = s.clone();
            let reply = reply.clone();
            async move {
                s.lock().push(req);
                Ok(reply)
            }
        });
        let server = IpcServer::new(h, Some(&path)).unwrap();
        server.start().await.unwrap();
        (dir, path, server, seen)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_query_state_returns_the_state() {
        let state = json!({"ble_connected": true, "battery_pct": 80});
        let (_dir, path, server, seen) = serve(json!({"ok": true, "state": state})).await;
        assert_eq!(query(path).await, Some(state));
        assert_eq!(seen.lock().as_slice(), &[json!({"evt": "get_state"})]);
        server.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_query_state_refusal_is_none() {
        let (_dir, path, server, _) = serve(json!({"ok": false, "error": "nope"})).await;
        assert_eq!(query(path.clone()).await, None);
        server.stop().await;
        let (_dir, path, server, _) = serve(json!({"ok": true})).await;
        assert_eq!(query(path).await, None);
        server.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_query_state_unreachable_is_none() {
        let dir = short_dir();
        let path = dir.path().join("none.sock").display().to_string();
        assert_eq!(query(path).await, None);
        // A spec the transport refuses is None too.
        let bad = tokio::task::spawn_blocking(|| query_state(Some("example.com:1"), QUERY_TIMEOUT)).await.unwrap();
        assert_eq!(bad, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_query_state_bad_reply_is_none() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for reply in [&b"not json\n"[..], b"\xff\n", b"\n", b""] {
            let dir = short_dir();
            let path = dir.path().join("raw.sock");
            let listener = tokio::net::UnixListener::bind(&path).unwrap();
            let reply = reply.to_vec();
            let task = tokio::spawn(async move {
                let (mut s, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 64];
                let _ = s.read(&mut buf).await;
                s.write_all(&reply).await.unwrap();
                s.shutdown().await.unwrap();
            });
            assert_eq!(query(path.display().to_string()).await, None);
            task.await.unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_query_state_reads_only_the_first_line() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let dir = short_dir();
        let path = dir.path().join("raw.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let task = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 64];
            let _ = s.read(&mut buf).await;
            // Held open after the newline: the client must not wait for EOF.
            s.write_all(b"{\"ok\": 1, \"state\": {\"ble_connected\": true}}\ngarbage").await.unwrap();
            tokio::time::sleep(Duration::from_secs(2)).await;
        });
        let started = std::time::Instant::now();
        assert_eq!(query(path.display().to_string()).await, Some(json!({"ble_connected": true})));
        assert!(started.elapsed() < Duration::from_secs(1));
        task.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_query_state_silent_daemon_times_out() {
        let dir = short_dir();
        let path = dir.path().join("mute.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let task = tokio::spawn(async move {
            let (_s, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let p = path.display().to_string();
        let started = std::time::Instant::now();
        let got = tokio::task::spawn_blocking(move || query_state(Some(&p), Duration::from_millis(200))).await.unwrap();
        assert_eq!(got, None);
        assert!(started.elapsed() < Duration::from_secs(2));
        task.abort();
    }
}
