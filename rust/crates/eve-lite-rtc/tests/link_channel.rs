//! lite_rtc.LinkChannel: the web app's `link` data channel carrying the unchanged LinkChat as HTTP over messages
//! (port of bridge/tests/test_lite_link_channel.py).
//!
//! The real LinkChat, with a fake inlet and a real transcript store in a tmp folder. The channel is a fake with the
//! surface LinkChannel uses (send, readyState, bufferedAmount and its low event, close), so every message the page
//! would get is read back as the page reads it (`answers`): a head, the body's frames under the request's id, then
//! end or err, in that order and nothing after.

mod common;

use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use common::*;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::io::AsyncWrite;

use eve_lite_rtc::hub::TokenFn;
use eve_lite_rtc::{BodyReader, ChannelEvent, ChannelWriter, LinkChannel, LinkDoor, BODY_FRAME_BYTES, BUFFER_HIGH};
use eve_lite_rtc::{MAX_BODY_BYTES, MAX_REQUESTS};
use eve_link::link_chat::door::LinkChatOptions;
use eve_link::link_chat::{self, LinkChat, MESSAGES_PATH, TURN_PATH};
use eve_link::seams::{DynRead, DynWrite, Headers};

fn channel(ch: &Arc<FakeChannel>, link: &LinkChat) -> LinkChannel {
    LinkChannel::new(ch.clone(), Some(Arc::new(link.clone())), SECRET)
}

/// The page's cursor as the query carries it.
fn cursor_of(page: &Value) -> String {
    match &page["cursor"] {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn turn_id(inlet: &FakeInlet, n: usize) -> String {
    inlet.heard()[n].2.clone().expect("an app turn")
}

// ---- a turn ----------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_a_turn_streams_as_head_frames_and_end_with_the_sse_dechunked() {
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())), Some(two_lines()));
    let link = door(tmp.path(), inlet.clone(), fast());
    let ch = FakeChannel::new("link");
    let lc = channel(&ch, &link);
    ch.page(req(
        1.into(),
        "POST",
        TURN_PATH,
        Some(json!({"text": "  hello eve  ", "client_id": "rtc-turn-1"})),
        "",
        &[("Content-Type", "application/json")],
    ));
    until(|| ended(&ch, 1), 5.0).await;
    let a = answer(&ch, 1).unwrap();
    assert!(a.end.as_deref() == Some("end") && a.status() == 200);
    let head = a.h();
    assert!(head["content-type"] == "text/event-stream" && head["cache-control"] == "no-store");
    assert!(!head.contains_key("transfer-encoding") && !head.contains_key("connection")); // de-chunked here
    let (contents, done, _) = events(&a.body); // SSE events, well formed
    assert!(contents == ["First line.", "\n\nSecond line."] && done);
    assert!(a.body.ends_with(b"data: [DONE]\n\n") && !a.body.windows(2).any(|w| w == b"\r\n")); // no chunk framing
    assert_eq!(inlet.heard(), vec![("hello eve".to_string(), false, Some("rtc-turn-1".to_string()))]); // typed
    assert_eq!(inlet.delivered(), vec![true, true]); // a reader had both lines
    until(|| lc.requests().is_empty(), 2.0).await; // the slot is free again
    link.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_while_a_turn_streams_it_has_one_reader_and_a_cancel_hangs_up() {
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())), Some(forever()));
    let link = door(tmp.path(), inlet.clone(), LinkChatOptions { ping_secs: 30.0, ..fast() });
    let ch = FakeChannel::new("link");
    let lc = channel(&ch, &link);
    ch.page(req(7.into(), "POST", TURN_PATH, Some(json!({"text": "hi", "client_id": "rtc-turn-7"})), "", &[]));
    until(|| answer(&ch, 7).is_some_and(|a| String::from_utf8_lossy(&a.body).contains("Working on it.")), 5.0).await;
    let turn = turn_id(&inlet, 0);
    assert_eq!(inlet.delivered(), vec![true]); // one reader
    let running = lc.request(7).unwrap();
    assert!(!running.reader.at_eof());
    let sent = ch.sent().len();
    ch.page(json!({"t": "cancel", "id": 7}));
    assert!(lc.requests().is_empty()); // the slot is free at once
    until(|| !link.deliver(&turn, "", "say"), 5.0).await; // no reader left
    assert!(running.reader.at_eof());
    until(|| running.task_done(), 2.0).await;
    inlet.out(&turn, "Said after the cancel."); // the brain is still going
    assert_eq!(inlet.delivered(), vec![true, false]); // and nobody reads it now
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(ch.sent().len(), sent); // no end, no err, no frames
    ch.page(json!({"t": "cancel", "id": 7})); // twice: nothing happens
    ch.page(json!({"t": "cancel", "id": 99}));
    assert_eq!(ch.sent().len(), sent);
    link.close();
}

fn late() -> Script {
    script(|inlet: Arc<FakeInlet>, app: String| async move {
        tokio::time::sleep(Duration::from_millis(300)).await; // long after the body was read
        inlet.out(&app, "Late line.");
        inlet.busy.store(false, Ordering::SeqCst);
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_no_eof_reaches_linkchat_before_a_cancel() {
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())), Some(late()));
    let link = door(tmp.path(), inlet.clone(), fast());
    let ch = FakeChannel::new("link");
    let lc = channel(&ch, &link);
    ch.page(req(1.into(), "POST", TURN_PATH, Some(json!({"text": "hi", "client_id": "rtc-late-1"})), "", &[]));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!lc.request(1).unwrap().reader.at_eof()); // read whole, never ended
    until(|| ended(&ch, 1), 5.0).await;
    assert_eq!(inlet.delivered(), vec![true]); // a reader for the late line
    let (contents, done, _) = events(&answer(&ch, 1).unwrap().body);
    assert!(contents == ["Late line."] && done);

    // Positive control: the same turn read from a reader that ends with its body, as a plain stream would be.
    // LinkChat takes that EOF as the page hanging up, and the late line has no reader.
    let tmp2 = tmp.path().join("eof");
    let inlet2 = FakeInlet::new(Some(store_at(&tmp2)), Some(late()));
    let link2 = door(&tmp2, inlet2.clone(), fast());
    let lc2 = LinkChannel::new(FakeChannel::new("link"), Some(Arc::new(link2.clone())), SECRET);
    let body = eve_lite_rtc::py::dumps(&json!({"text": "hi", "client_id": "rtc-late-2"}), false).into_bytes();
    let mut reader = BodyReader::new();
    reader.feed_data(&body);
    reader.feed_eof();
    let headers: Headers = [
        ("authorization".to_string(), format!("Bearer {SECRET}")),
        ("content-length".to_string(), body.len().to_string()),
    ]
    .into_iter()
    .collect();
    let mut writer = ChannelWriter::new(&lc2, 9);
    let check = lc2.clone();
    let check = move |t: &str| check.check(t);
    let _ = link2.serve("POST", TURN_PATH, "", &headers, &mut reader, &mut writer, Some(&check), None).await;
    assert_eq!(inlet2.delivered(), vec![false]);
    link.close();
    link2.close();
}

// ---- the conversation, and what never reaches LinkChat ---------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_a_long_messages_page_is_split_into_frames_and_reassembled() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_at(tmp.path());
    for i in 0..link_chat::HISTORY_LIMIT {
        assert!(store.append("telegram", "t-1", "eve", "say", &format!("{i:03} {}", "words ".repeat(180))));
    }
    let link = door(tmp.path(), FakeInlet::new(Some(store), None), fast());
    let ch = FakeChannel::new("link");
    let _lc = channel(&ch, &link);
    ch.page(get(3, MESSAGES_PATH));
    until(|| ended(&ch, 3), 5.0).await;
    let a = answer(&ch, 3).unwrap();
    assert!(a.end.as_deref() == Some("end") && a.status() == 200);
    assert_eq!(a.h()["content-type"], "application/json");
    let size = a.body.len();
    assert!(size > 100_000 && a.h()["content-length"].as_str().unwrap().parse::<usize>().unwrap() == size);
    assert!(a.frames == size.div_ceil(BODY_FRAME_BYTES) && a.frames >= 7); // full frames, then the rest
    let page = a.json();
    let got: Vec<String> = page["messages"].as_array().unwrap().iter().map(|m| m["text"].as_str().unwrap()[..3].to_string()).collect();
    let want: Vec<String> = (0..link_chat::HISTORY_LIMIT).map(|i| format!("{i:03}")).collect();
    assert_eq!(got, want);
    // the query passes as it is: the cursor from that page gives nothing newer
    let q = format!("?after={}&wait=0", cursor_of(&page));
    ch.page(req(4.into(), "GET", MESSAGES_PATH, None, &q, &[]));
    until(|| ended(&ch, 4), 5.0).await;
    assert_eq!(answer(&ch, 4).unwrap().json()["messages"], json!([]));
    link.close();
}

/// LinkChat behind a spy: what reached it, and the token check it was given, tried on three tokens.
struct Spy {
    link: LinkChat,
    reached: Mutex<Vec<String>>,
    seen: Mutex<Vec<(Headers, [bool; 3])>>,
}

#[async_trait]
impl LinkDoor for Spy {
    async fn serve(
        &self,
        method: &str,
        path: &str,
        query: &str,
        headers: &Headers,
        reader: &mut DynRead,
        writer: &mut DynWrite,
        token_check: &TokenFn,
    ) -> io::Result<()> {
        self.reached.lock().push(path.to_string());
        self.seen.lock().push((headers.clone(), [token_check(SECRET), token_check("stolen"), token_check("")]));
        self.link.serve(method, path, query, headers, reader, writer, Some(token_check), None).await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_a_path_outside_turn_and_messages_is_a_404_here() {
    let file = format!("/api/link/file/{}", "a".repeat(32));
    let paths = [
        "/api/link/jobs",
        "/api/link/upload",
        file.as_str(),
        "/api/link/controls",
        "/api/link/today",
        "/api/web/link/turn",
        "/api/link/turn/",
        "/api/link/turn/../jobs",
        "/",
        "",
    ];
    for path in paths {
        let tmp = tempfile::tempdir().unwrap();
        let inlet = FakeInlet::new(Some(store_at(tmp.path())), None);
        let link = door(tmp.path(), inlet.clone(), fast());
        let spy = Arc::new(Spy { link: link.clone(), reached: Mutex::new(vec![]), seen: Mutex::new(vec![]) });
        let ch = FakeChannel::new("link");
        let _lc = LinkChannel::new(ch.clone(), Some(spy.clone()), SECRET);
        ch.page(get(5, path));
        ch.page(req(6.into(), "POST", path, Some(json!({"text": "hi"})), "", &[]));
        for rid in [5, 6] {
            let a = answer(&ch, rid).unwrap(); // answered at once, by the door
            assert!(a.status() == 404 && a.end.as_deref() == Some("end"), "{path}");
            assert_eq!(a.json(), json!({"ok": false, "error": "not found"}));
        }
        assert!(spy.reached.lock().is_empty() && inlet.heard().is_empty());
        ch.page(get(8, MESSAGES_PATH)); // positive control
        until(|| ended(&ch, 8), 5.0).await;
        assert_eq!(*spy.reached.lock(), vec![MESSAGES_PATH.to_string()]);
        assert_eq!(answer(&ch, 8).unwrap().status(), 200);
        link.close();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_the_pages_origin_never_reaches_linkchat_and_the_peers_secret_does() {
    let tmp = tempfile::tempdir().unwrap();
    let link = door(tmp.path(), FakeInlet::new(Some(store_at(tmp.path())), None), fast());
    let spy = Arc::new(Spy { link: link.clone(), reached: Mutex::new(vec![]), seen: Mutex::new(vec![]) });
    let ch = FakeChannel::new("link");
    let _lc = LinkChannel::new(ch.clone(), Some(spy.clone()), SECRET);
    ch.page(req(
        1.into(),
        "GET",
        MESSAGES_PATH,
        None,
        "",
        &[
            ("Origin", "https://evil.example"),
            ("Authorization", "Bearer stolen"),
            ("Cookie", "eve_dev=CANARY-NOT-A-KEY"),
            ("Content-Length", "999"),
            ("Accept", "application/json"),
        ],
    ));
    until(|| ended(&ch, 1), 5.0).await;
    assert_eq!(answer(&ch, 1).unwrap().status(), 200); // LinkChat let it in
    let (headers, checks) = spy.seen.lock()[0].clone();
    let want: Headers = [
        ("accept".to_string(), "application/json".to_string()),
        ("authorization".to_string(), format!("Bearer {SECRET}")),
    ]
    .into_iter()
    .collect();
    assert_eq!(headers, want);
    assert_eq!(checks, [true, false, false]);
    // Positive control: LinkChat itself refuses any Origin, so the drop above is what let the page in.
    let mut writer = ChannelWriter::new(&LinkChannel::new(FakeChannel::new("link"), None, SECRET), 2);
    let headers: Headers = [
        ("origin".to_string(), "https://evil.example".to_string()),
        ("authorization".to_string(), format!("Bearer {SECRET}")),
    ]
    .into_iter()
    .collect();
    let check = |t: &str| t == SECRET;
    let _ = link.serve("GET", MESSAGES_PATH, "", &headers, &mut BodyReader::new(), &mut writer, Some(&check), None).await;
    assert_eq!(writer.status(), 403);
    link.close();
}

// ---- the limits ------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_the_limits_on_one_request() {
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())), None);
    let link = door(tmp.path(), inlet.clone(), fast());
    let ch = FakeChannel::new("link");
    let _lc = channel(&ch, &link);
    ch.page(req(1.into(), "POST", TURN_PATH, Some(Value::String("x".repeat(MAX_BODY_BYTES + 1))), "", &[]));
    ch.page(req(2.into(), "POST", TURN_PATH, Some(Value::String("é".repeat(MAX_BODY_BYTES / 2 + 1))), "", &[])); // UTF-8
    ch.page(req(3.into(), "PUT", TURN_PATH, Some(json!({"text": "hi"})), "", &[]));
    ch.page(req(4.into(), "POST", TURN_PATH, Some(json!({"text": "hi"})), "", &[("x-bad", "a\r\nb")]));
    let many: Vec<(String, String)> = (0..40).map(|i| (format!("x-{i}"), "1".to_string())).collect();
    let many: Vec<(&str, &str)> = many.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    ch.page(req(5.into(), "POST", TURN_PATH, Some(json!({"text": "hi"})), "", &many));
    let mut six = get(6, MESSAGES_PATH);
    six["q"] = json!(5);
    ch.page(six);
    let mut seven = req(7.into(), "POST", TURN_PATH, None, "", &[]);
    seven["b"] = json!({"text": "hi"});
    ch.page(seven);
    let got = answers(&ch.sent());
    let statuses: Vec<(u32, i64)> = (1..8).map(|rid| (rid, got[&rid].status())).collect();
    assert_eq!(statuses, vec![(1, 413), (2, 413), (3, 405), (4, 400), (5, 400), (6, 400), (7, 400)]);
    assert!((1..8).all(|rid| got[&rid].end.as_deref() == Some("end")));
    // a body of exactly the limit reaches LinkChat, which answers it as its own (the text is too long there)
    let mut exact = eve_lite_rtc::py::dumps(&json!({"text": "x".repeat(MAX_BODY_BYTES - 50)}), false);
    while exact.len() < MAX_BODY_BYTES {
        exact.push(' ');
    }
    ch.page(req(8.into(), "POST", TURN_PATH, Some(Value::String(exact)), "", &[]));
    until(|| ended(&ch, 8), 5.0).await;
    let a = answer(&ch, 8).unwrap();
    assert!(a.status() == 400 && a.json()["error"].as_str().unwrap().contains("4000 characters"));
    // what cannot be answered (no id, not JSON, not text) is dropped, and the channel goes on
    let before = ch.sent().len();
    for bad in [Value::Null, json!(-1), json!(1u64 << 32), json!(true)] {
        ch.page(req(bad, "GET", MESSAGES_PATH, None, "", &[]));
    }
    ch.page(Value::String("not json".into()));
    ch.page(Value::String("[1, 2]".into()));
    ch.page_bytes(b"\x00\x01binary");
    ch.page(json!({"t": "nope", "id": 9}));
    assert!(ch.sent().len() == before && inlet.heard().is_empty());
    ch.page(get(9, MESSAGES_PATH));
    until(|| ended(&ch, 9), 5.0).await;
    assert_eq!(answer(&ch, 9).unwrap().status(), 200);
    link.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_eight_requests_run_at_once_and_a_ninth_waits_its_turn_elsewhere() {
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())), None);
    let link = door(tmp.path(), inlet, LinkChatOptions { poll_secs: 0.05, ..fast() });
    let ch = FakeChannel::new("link");
    let lc = channel(&ch, &link);
    ch.page(get(0, MESSAGES_PATH));
    until(|| ended(&ch, 0), 5.0).await;
    let cursor = cursor_of(&answer(&ch, 0).unwrap().json());
    let long = format!("after={cursor}&wait=20");
    for rid in 1..9u32 {
        ch.page(req(rid.into(), "GET", MESSAGES_PATH, None, &long, &[])); // long polls: each holds a slot
    }
    assert_eq!(lc.requests().len(), MAX_REQUESTS);
    ch.page(req(9.into(), "GET", MESSAGES_PATH, None, &long, &[]));
    let a = answer(&ch, 9).unwrap();
    assert!(a.status() == 429 && a.end.as_deref() == Some("end"));
    assert!(a.json()["error"].as_str().unwrap().contains("at most 8"));
    ch.page(req(1.into(), "GET", MESSAGES_PATH, None, &long, &[])); // an id still running: dropped
    assert!(answer(&ch, 1).is_none());
    ch.page(json!({"t": "cancel", "id": 1}));
    ch.page(req(10.into(), "GET", MESSAGES_PATH, None, &format!("after={cursor}&wait=0"), &[])); // its slot
    until(|| ended(&ch, 10), 5.0).await;
    assert!(answer(&ch, 10).unwrap().status() == 200 && answer(&ch, 1).is_none());
    let running: Vec<_> = lc.requests().into_iter().filter_map(|rid| lc.request(rid)).collect();
    ch.close(); // every long poll is cancelled
    assert!(lc.closed() && lc.requests().is_empty());
    until(|| running.iter().all(|r| r.task_done()), 2.0).await;
    assert!((1..9).all(|rid| answer(&ch, rid).is_none())); // nothing went out after it
    link.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_a_closed_channel_ends_its_turns_streams() {
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())), Some(forever()));
    let link = door(tmp.path(), inlet.clone(), LinkChatOptions { ping_secs: 30.0, ..fast() });
    let ch = FakeChannel::new("link");
    let lc = channel(&ch, &link);
    ch.page(req(1.into(), "POST", TURN_PATH, Some(json!({"text": "hi", "client_id": "rtc-close-1"})), "", &[]));
    until(|| answer(&ch, 1).is_some_and(|a| String::from_utf8_lossy(&a.body).contains("Working")), 5.0).await;
    let turn = turn_id(&inlet, 0);
    assert_eq!(inlet.delivered(), vec![true]); // one reader
    ch.close();
    assert!(lc.closed());
    until(|| !link.deliver(&turn, "", "say"), 5.0).await; // its reader is gone
    ch.page(get(2, MESSAGES_PATH)); // a closed channel takes nothing
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(answer(&ch, 2).is_none());
    link.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_a_writer_drains_while_the_channel_is_behind() {
    let ch = FakeChannel::new("link");
    let lc = LinkChannel::new(ch.clone(), None, SECRET);
    assert_eq!(ch.low_threshold.load(Ordering::SeqCst), BUFFER_HIGH);
    let writer = ChannelWriter::new(&lc, 1);
    ch.buffered.store(BUFFER_HIGH + 1, Ordering::SeqCst);
    let w = writer.clone();
    let waiting = tokio::spawn(async move { w.drain().await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!waiting.is_finished()); // the page is behind: wait
    ch.buffered.store(BUFFER_HIGH, Ordering::SeqCst);
    ch.emit(ChannelEvent::BufferedAmountLow);
    tokio::time::timeout(Duration::from_secs(1), waiting).await.unwrap().unwrap().unwrap();
    ch.buffered.store(BUFFER_HIGH + 1, Ordering::SeqCst);
    let w = writer.clone();
    let waiting = tokio::spawn(async move { w.drain().await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!waiting.is_finished());
    ch.close(); // a closed channel lets go
    let _ = tokio::time::timeout(Duration::from_secs(1), waiting).await.unwrap().unwrap();
    let e = writer.write(b"HTTP/1.1 200 OK\r\n\r\n").unwrap_err(); // and the next write fails
    assert_eq!(e.kind(), io::ErrorKind::ConnectionReset);
}

// ---- the writer's framing --------------------------------------------------------------------------------------

fn writer_on(ch: &Arc<FakeChannel>, rid: u32) -> ChannelWriter {
    ChannelWriter::new(&LinkChannel::new(ch.clone(), None, SECRET), rid)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_the_writer_dechunks_a_stream_written_a_byte_at_a_time() {
    let ch = FakeChannel::new("link");
    let w = writer_on(&ch, 70000);
    let pieces = [link_chat::sse("one"), link_chat::sse(&"two ".repeat(5000)), link_chat::DONE.to_vec()];
    let mut raw = link_chat::STREAM_HEAD.to_vec();
    for p in &pieces {
        raw.extend_from_slice(format!("{:x}\r\n", p.len()).as_bytes());
        raw.extend_from_slice(p);
        raw.extend_from_slice(b"\r\n");
    }
    raw.extend_from_slice(b"0\r\n\r\n");
    for i in 0..raw.len() {
        w.write(&raw[i..i + 1]).unwrap();
        w.drain().await.unwrap();
    }
    let a = answer(&ch, 70000).unwrap();
    assert!(a.status() == 200 && a.end.as_deref() == Some("end") && a.body == pieces.concat());
    assert!(!a.h().contains_key("transfer-encoding") && a.h()["x-accel-buffering"] == "no");
    w.finish(); // already ended: nothing more
    w.write(b"after the end").unwrap();
    assert_eq!(answer(&ch, 70000).unwrap().end.as_deref(), Some("end"));
}

#[tokio::test]
async fn test_the_writer_frames_a_content_length_body_and_a_body_to_the_close() {
    let ch = FakeChannel::new("link");
    let w = writer_on(&ch, 1);
    let body = vec![b'x'; BODY_FRAME_BYTES * 2 + 5];
    let mut first = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    first.extend_from_slice(&body[..100]);
    w.write(&first).unwrap();
    assert!(answer(&ch, 1).unwrap().end.is_none()); // not all of it yet
    w.write(&body[100..]).unwrap();
    let a = answer(&ch, 1).unwrap();
    assert!(a.end.as_deref() == Some("end") && a.body == body && a.frames == 3);
    assert_eq!(Value::Object(a.h().clone()), json!({"content-type": "application/json", "content-length": body.len().to_string()}));
    let w2 = writer_on(&ch, 2);
    w2.write(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nuntil the close").unwrap();
    assert!(answer(&ch, 2).unwrap().end.is_none());
    w2.finish();
    let a = answer(&ch, 2).unwrap();
    assert!(a.end.as_deref() == Some("end") && a.body == b"until the close");
}

#[tokio::test]
async fn test_an_answer_that_cannot_be_framed_ends_in_err() {
    let raws: [&[u8]; 6] = [
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabcXY",
        b"HTTP/1.1 OK\r\n\r\n",
        b"not http at all\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nno colon here\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length: many\r\n\r\n",
    ];
    for raw in raws {
        let ch = FakeChannel::new("link");
        let w = writer_on(&ch, 1);
        assert_eq!(w.write(raw).unwrap_err().kind(), io::ErrorKind::ConnectionReset);
        assert_eq!(answer(&ch, 1).unwrap().end.as_deref(), Some("err"), "{}", String::from_utf8_lossy(raw));
        assert_eq!(w.write(b"more").unwrap_err().kind(), io::ErrorKind::ConnectionReset); // the stream stops here
    }
}

#[tokio::test]
async fn test_an_answer_cut_short_ends_in_err_and_a_cancelled_one_in_nothing() {
    let ch = FakeChannel::new("link");
    let w = writer_on(&ch, 1);
    w.write(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc").unwrap();
    w.finish();
    let a = answer(&ch, 1).unwrap();
    assert!(a.end.as_deref() == Some("err") && a.body == b"abc");
    let w2 = writer_on(&ch, 2);
    w2.write(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap();
    w2.fail();
    assert_eq!(answer(&ch, 2).unwrap().end.as_deref(), Some("err"));
    let w3 = writer_on(&ch, 3);
    w3.set_dead(); // cancelled by the page
    w3.finish();
    w3.fail();
    assert!(answer(&ch, 3).is_none());
    assert_eq!(w3.write(b"HTTP/1.1 200 OK\r\n\r\n").unwrap_err().kind(), io::ErrorKind::ConnectionReset);
}

#[tokio::test]
async fn test_closing_the_writer_ends_the_answer_as_a_closed_connection_would() {
    let ch = FakeChannel::new("link");
    let w = writer_on(&ch, 1);
    w.write(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc").unwrap();
    w.close(); // mid-body: the page must know
    assert!(answer(&ch, 1).unwrap().end.as_deref() == Some("err") && w.is_closing());
    let w2 = writer_on(&ch, 2);
    w2.write(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nto the close").unwrap();
    w2.close(); // the close is this body's end
    let a = answer(&ch, 2).unwrap();
    assert!(a.end.as_deref() == Some("end") && a.body == b"to the close");
    let w3 = writer_on(&ch, 3);
    w3.write(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").unwrap();
    w3.close(); // already whole: nothing more
    assert_eq!(answer(&ch, 3).unwrap().end.as_deref(), Some("end"));
    assert_eq!(w3.write(b"more").unwrap_err().kind(), io::ErrorKind::ConnectionReset);
}

/// A writer with nothing but `write` and `flush` (the drain): what LinkChat may use (ChannelWriter's contract). A
/// shutdown is noted, and the test fails on it.
struct OnlyWriteAndDrain {
    inner: ChannelWriter,
    shut: Arc<AtomicBool>,
}

impl AsyncWrite for OnlyWriteAndDrain {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.shut.store(true, Ordering::SeqCst);
        Poll::Ready(Ok(()))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_linkchat_uses_only_write_and_drain_on_the_paths_the_channel_forwards() {
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())), Some(two_lines()));
    let link = door(tmp.path(), inlet, fast());
    let ch = FakeChannel::new("link");
    let lc = channel(&ch, &link);
    let body = eve_lite_rtc::py::dumps(&json!({"text": "hi", "client_id": "rtc-strict-1"}), false).into_bytes();
    let cases: Vec<(&str, &str, Vec<u8>)> = vec![
        ("POST", TURN_PATH, body),
        ("GET", MESSAGES_PATH, vec![]),
        ("GET", TURN_PATH, vec![]),
        ("POST", TURN_PATH, b"{}".to_vec()),
        ("GET", MESSAGES_PATH, vec![]),
    ];
    let shut = Arc::new(AtomicBool::new(false));
    for (i, (method, path, data)) in cases.into_iter().enumerate() {
        let rid = i as u32 + 1;
        let reader = BodyReader::new();
        reader.feed_data(&data);
        let mut headers: Headers = [("authorization".to_string(), format!("Bearer {SECRET}"))].into_iter().collect();
        if !data.is_empty() {
            headers.insert("content-length".into(), data.len().to_string());
        }
        let writer = ChannelWriter::new(&lc, rid);
        let query = if rid == 5 { "after=nope" } else { "" };
        let (link2, lc2, w2, mut r2, shut2) = (link.clone(), lc.clone(), writer.clone(), reader.clone(), shut.clone());
        let (method, path, query) = (method.to_string(), path.to_string(), query.to_string());
        let task = tokio::spawn(async move {
            let mut only = OnlyWriteAndDrain { inner: w2, shut: shut2 };
            let check = move |t: &str| lc2.check(t);
            let _ = link2.serve(&method, &path, &query, &headers, &mut r2, &mut only, Some(&check), None).await;
        });
        until(|| writer.done(), 5.0).await;
        reader.feed_eof();
        tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap();
    }
    let got = answers(&ch.sent());
    let statuses: Vec<i64> = (1..6).map(|rid| got[&rid].status()).collect();
    assert_eq!(statuses, vec![200, 200, 405, 400, 400]);
    assert!((1..6).all(|rid| got[&rid].end.as_deref() == Some("end")));
    assert!(!shut.load(Ordering::SeqCst));
    link.close();
}

/// A LinkChat that fails as Python's raised: a panic mid-answer.
struct Panics;

#[async_trait]
impl LinkDoor for Panics {
    async fn serve(
        &self,
        _method: &str,
        _path: &str,
        _query: &str,
        _headers: &Headers,
        _reader: &mut DynRead,
        writer: &mut DynWrite,
        _token_check: &TokenFn,
    ) -> io::Result<()> {
        use tokio::io::AsyncWriteExt;
        writer.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc").await?;
        panic!("LinkChat broke mid-answer");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_request_whose_linkchat_fails_ends_in_err_and_frees_its_slot() {
    let ch = FakeChannel::new("link");
    let lc = LinkChannel::with(ch.clone(), Some(Arc::new(Panics)), SECRET, None, 1);
    ch.page(get(1, MESSAGES_PATH));
    until(|| ended(&ch, 1), 5.0).await;
    let a = answer(&ch, 1).unwrap();
    assert!(a.status() == 200 && a.body == b"abc" && a.end.as_deref() == Some("err")); // cut short: err
    until(|| lc.requests().is_empty(), 2.0).await; // the slot is free again
    ch.page(get(2, MESSAGES_PATH)); // and taken: not a 429
    until(|| ended(&ch, 2), 5.0).await;
    assert_eq!(answer(&ch, 2).unwrap().end.as_deref(), Some("err"));
    assert_eq!(answer(&ch, 2).unwrap().status(), 200);
}

#[tokio::test]
async fn test_a_chunk_size_is_read_as_python_reads_int_of_bytes() {
    // int(b"...", 16): a 0x prefix, single underscores and ASCII spaces around pass
    for (size, ok) in [(&b"5"[..], true), (b"0x5", true), (b" 5 ", true), (b"0_5", true), (b"5;ext=1", true)] {
        let ch = FakeChannel::new("link");
        let w = writer_on(&ch, 1);
        let mut raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        raw.extend_from_slice(size);
        raw.extend_from_slice(b"\r\nhello\r\n0\r\n\r\n");
        assert_eq!(w.write(&raw).is_ok(), ok, "{}", String::from_utf8_lossy(size));
        let a = answer(&ch, 1).unwrap();
        assert!(a.end.as_deref() == Some("end") && a.body == b"hello", "{}", String::from_utf8_lossy(size));
    }
    // and what Python's int() of bytes refuses: a non-ASCII space, an information separator, a double underscore
    for size in [&b"\xc2\xa05"[..], b"\x1c5", b"0__5", b"-5"] {
        let ch = FakeChannel::new("link");
        let w = writer_on(&ch, 1);
        let mut raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        raw.extend_from_slice(size);
        raw.extend_from_slice(b"\r\nhello\r\n");
        assert_eq!(w.write(&raw).unwrap_err().kind(), io::ErrorKind::ConnectionReset, "{:?}", size);
        assert_eq!(answer(&ch, 1).unwrap().end.as_deref(), Some("err"));
    }
}
