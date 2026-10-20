//! test_phone_call_serve_socket.py: `PhoneCalls::serve_socket`, a call on a socket that is already open (`serve`
//! without the WebSocket handshake; eve lite's WebRTC call). A scripted call gives the same messages, audio and
//! effects through `serve` over a real WebSocket and through `serve_socket` over an in-memory socket.

#![allow(clippy::type_complexity)]

use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde_json::{json, Value};

use super::test_support::*;
use super::*;

const TOKEN: &str = "call-token-CANARY-NOT-A-KEY-7f3c";

fn token_check() -> Option<TokenCheck> {
    Some(Arc::new(|t: &str| t == TOKEN))
}

fn no_owner() -> CheckOwner {
    Arc::new(|_: &str| None)
}

/// What the call sent, JSON as itself and each audio message as its length.
#[derive(Debug, PartialEq)]
enum Seen {
    Json(Value),
    Audio(usize),
}

fn seen(m: Msg) -> Seen {
    match m {
        Msg::Json(v) => Seen::Json(v),
        Msg::Bin(b) => Seen::Audio(b.len()),
    }
}

fn kind(s: &Seen) -> String {
    match s {
        Seen::Json(v) => v["type"].as_str().unwrap_or("").to_string(),
        Seen::Audio(_) => "audio".into(),
    }
}

fn matches(s: &Seen, k: &str, state: Option<&str>) -> bool {
    match s {
        Seen::Json(v) => v["type"] == k && state.is_none_or(|st| v["state"] == st),
        Seen::Audio(_) => false,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_a_call_on_an_open_socket_runs_as_serve_runs_it_without_the_handshake() {
    // through serve: a real WebSocket, its handshake and frames
    let (brain, voice) = (FakeBrain::default_reply(), FakeVoice::calendar());
    let calls = Arc::new(PhoneCalls::new(no_owner(), Some(brain.clone()), Some(voice.clone())));
    let base = serve(calls.clone()).await;
    // the loopback server's Eve Link door checks its own token; serve_socket below checks this file's
    let mut over_ws: Vec<Seen> = Vec::new();
    let ws = Phone::connect(&format!("{base}/api/stick")).await;
    ws.send_json(json!({"token": LINK, "can": []})).await;
    over_ws.extend(ws.until(is_state("connected")).await.into_iter().map(seen));
    ws.send_json(json!({"type": "talk"})).await;
    for _ in 0..10 {
        ws.send_bytes(vec![0u8; 4800]).await;
    }
    ws.send_json(json!({"type": "done"})).await;
    over_ws.extend(ws.until(is_state("listening")).await.into_iter().map(seen));
    ws.send_json(json!({"type": "end"})).await;
    over_ws.extend(ws.until(is_kind("ended")).await.into_iter().map(seen));
    ws.close().await;
    until("the call to end", || calls.active().is_none()).await;
    let ws_effects = (brain.heard(), voice.transcribed(), voice.spoken(), brain.say().is_none(), calls.active().is_none());

    // through serve_socket: the same call on a socket that is already open
    let (brain, voice) = (FakeBrain::default_reply(), FakeVoice::calendar());
    let calls = Arc::new(PhoneCalls::new(no_owner(), Some(brain.clone()), Some(voice.clone())));
    let sock = MemorySocket::new();
    let c = calls.clone();
    let s: Arc<dyn CallSocket> = sock.clone();
    let task = tokio::spawn(async move { c.serve_socket(s, token_check()).await });
    let mut over_socket: Vec<Seen> = Vec::new();
    sock.page_json(json!({"token": TOKEN, "can": []}));
    over_socket.extend(sock.until(|m| m.state() == Some("connected")).await.into_iter().map(seen));
    sock.command("talk");
    for _ in 0..10 {
        sock.page_bytes(vec![0u8; 4800]);
    }
    sock.command("done");
    over_socket.extend(sock.until(|m| m.state() == Some("listening")).await.into_iter().map(seen));
    sock.command("end");
    over_socket.extend(sock.until(|m| m.is("ended")).await.into_iter().map(seen));
    tokio::time::timeout(std::time::Duration::from_secs(5), task).await.unwrap().unwrap().unwrap();
    assert!(sock.closed() && sock.closes.load(Ordering::SeqCst) >= 1); // closed when the call ends
    let socket_effects =
        (brain.heard(), voice.transcribed(), voice.spoken(), brain.say().is_none(), calls.active().is_none());

    assert_eq!(over_socket, over_ws);
    let kinds: Vec<String> = over_socket.iter().map(kind).collect();
    assert_eq!(kinds[..4], ["state", "flush", "state", "heard"]);
    assert_eq!(kinds.last().unwrap(), "ended");
    assert!(kinds.iter().any(|k| k == "audio"));
    assert_eq!(socket_effects, ws_effects);
    assert_eq!(socket_effects.0, vec!["what's on my calendar"]);
    assert_eq!(socket_effects.1, vec![48000]);
    assert!(socket_effects.3 && socket_effects.4); // the chat let go; no call left
    assert!(over_socket.iter().any(|s| *s == Seen::Json(json!({"type": "ended", "reason": HUNG_UP}))));
    assert!(over_socket.iter().any(|s| matches(s, "state", Some("speaking"))));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_serve_socket_checks_the_hello_and_says_why_it_ends() {
    async fn run(hello: Option<Msg>, brain: bool) -> Arc<MemorySocket> {
        let (b, v): (Option<Arc<dyn Brain>>, Option<Arc<dyn Voice>>) = if brain {
            (Some(FakeBrain::default_reply()), Some(FakeVoice::calendar()))
        } else {
            (None, None)
        };
        let calls = PhoneCalls::new(no_owner(), b, v);
        let sock = MemorySocket::new();
        match hello {
            Some(Msg::Json(v)) if v.is_string() => sock.page_text(v.as_str().unwrap()),
            Some(Msg::Json(v)) => sock.page_json(v),
            Some(Msg::Bin(b)) => sock.page_bytes(b),
            None => {}
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), calls.serve_socket(sock.clone(), token_check()))
            .await
            .unwrap()
            .unwrap();
        sock
    }

    for hello in [
        Msg::Json(json!({"token": "wrong"})),
        Msg::Json(json!({"token": ""})),
        Msg::Json(json!({"initData": "x"})),
        Msg::Json(json!("not json")),
        Msg::Json(json!([TOKEN])),
        Msg::Bin(vec![0, 1]),
    ] {
        let sock = run(Some(hello.clone()), true).await;
        assert_eq!(sock.sent_json(), vec![json!({"type": "ended", "reason": "Only eve's owner can call."})], "{hello:?}");
        assert!(sock.closed());
    }
    let sock = run(Some(Msg::Json(json!({"token": TOKEN, "can": []}))), false).await; // no chat or voice here
    assert_eq!(sock.sent_json(), vec![json!({"type": "ended", "reason": "Calls aren't set up on this computer."})]);
    // a socket that closes before saying anything: nothing is sent, and it is closed once more, harmlessly
    let calls = PhoneCalls::new(no_owner(), Some(FakeBrain::default_reply()), Some(FakeVoice::calendar()));
    let sock = MemorySocket::new();
    sock.page_close();
    tokio::time::timeout(std::time::Duration::from_secs(5), calls.serve_socket(sock.clone(), token_check()))
        .await
        .unwrap()
        .unwrap();
    assert!(sock.closed() && calls.active().is_none() && sock.sent().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_a_second_socket_replaces_the_call_still_open() {
    let brain = FakeBrain::default_reply();
    let calls = Arc::new(PhoneCalls::new(no_owner(), Some(brain), Some(FakeVoice::calendar())));
    let (first, second) = (MemorySocket::new(), MemorySocket::new());
    first.page_json(json!({"token": TOKEN, "can": []}));
    let (c, s) = (calls.clone(), first.clone());
    let one = tokio::spawn(async move { c.serve_socket(s, token_check()).await });
    first.next_out().await;
    let active = calls.active().unwrap();
    assert!(std::ptr::eq(Arc::as_ptr(&active.ws) as *const (), Arc::as_ptr(&first) as *const ()));
    second.page_json(json!({"token": TOKEN, "can": []}));
    let (c, s) = (calls.clone(), second.clone());
    let two = tokio::spawn(async move { c.serve_socket(s, token_check()).await });
    assert_eq!(second.next_out().await, Msg::Json(json!({"type": "state", "state": "connected"})));
    let active = calls.active().unwrap();
    assert!(first.closed() && std::ptr::eq(Arc::as_ptr(&active.ws) as *const (), Arc::as_ptr(&second) as *const ()));
    tokio::time::timeout(std::time::Duration::from_secs(5), one).await.unwrap().unwrap().unwrap();
    second.command("end");
    tokio::time::timeout(std::time::Duration::from_secs(5), two).await.unwrap().unwrap().unwrap();
    assert!(calls.active().is_none());
    assert_eq!(second.sent_json().last().unwrap(), &json!({"type": "ended", "reason": HUNG_UP}));
}
